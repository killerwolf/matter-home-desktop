//! Le moteur réseau : une petite file de requêtes HTTPS traitées par quelques fils, et la
//! session (WebSocket) dans son propre fil.
//!
//! Le parallélisme HTTP est borné à [`HTTP_WORKERS`] : le serveur plafonne à 16 connexions par
//! adresse (docs/protocole.md), et une requête = une connexion.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use protocol::wire::{ErrorCode, LoginRequest, LoginResponse, VersionInfo, PROTOCOL_VERSION};

use super::addr::ServerAddr;
use super::error::NetError;
use super::files;
use super::http::{self, Request};
use super::store::{Saved, Store};
use super::tls::{self, normalize_fingerprint, Trust};
use super::{ws, Event, Link, Notifier};

/// Requêtes HTTPS simultanées au plus (plus la connexion WebSocket : bien sous les 16 du serveur).
const HTTP_WORKERS: usize = 4;

/// Ce qu'il faut pour parler au serveur : adresse, certificat approuvé, jeton.
#[derive(Clone)]
pub(super) struct Creds {
    /// Adresse **utilisée** en ce moment (une des candidates).
    pub addr: ServerAddr,
    /// Adresse principale, celle du fichier de connexion (`Saved::url`) : la « maison ».
    pub primary: ServerAddr,
    /// Autres adresses du même serveur (à distance), essayées après la principale.
    pub alts: Vec<ServerAddr>,
    pub fingerprint: String,
    pub token: String,
}

impl Creds {
    /// Session sur l'adresse principale ; `alt_urls` (celles du fichier) qui ne se lisent pas ou
    /// qui doublonnent sont écartées.
    pub(super) fn new(
        primary: ServerAddr,
        alt_urls: &[String],
        fingerprint: String,
        token: String,
    ) -> Creds {
        Creds {
            addr: primary.clone(),
            alts: parse_alts(&primary, alt_urls),
            primary,
            fingerprint,
            token,
        }
    }

    /// Les adresses à essayer, dans l'ordre : la principale, puis les autres.
    pub(super) fn candidates(&self) -> Vec<ServerAddr> {
        std::iter::once(self.primary.clone())
            .chain(self.alts.iter().cloned())
            .collect()
    }

    /// Quel serveur : adresse **principale** et empreinte (jamais le jeton). Stable quand la
    /// session bascule d'une adresse à l'autre : ce n'est pas « un autre serveur » (les
    /// brouillons en cours restent valables).
    pub(super) fn identity(&self) -> String {
        format!("{}#{}", self.primary.url(), self.fingerprint)
    }

    /// Joint « à distance » : une autre adresse que la principale, ou une adresse Tailscale.
    pub(super) fn is_remote(&self) -> bool {
        self.addr != self.primary || self.addr.is_tailscale()
    }
}

fn parse_alts(primary: &ServerAddr, alt_urls: &[String]) -> Vec<ServerAddr> {
    let mut out: Vec<ServerAddr> = Vec::new();
    for url in alt_urls {
        if let Ok(a) = ServerAddr::parse(url) {
            if a != *primary && !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// Délai d'essai d'une adresse quand il y en a plusieurs (le TCP en demande jusqu'à 4 s).
const PICK_TIMEOUT: Duration = Duration::from_secs(3);

/// La première adresse qui répond **avec le certificat épinglé**, dans l'ordre. Une adresse qui
/// répond avec un autre certificat (autre appareil, autre réseau local) est ignorée. `None` si
/// aucune ne répond : l'appelant garde alors sa tentative habituelle et son message d'erreur.
pub(super) fn pick_address(candidates: &[ServerAddr], fingerprint: &str) -> Option<ServerAddr> {
    if candidates.len() < 2 {
        return candidates.first().cloned();
    }
    let trust = Trust::Pinned(fingerprint.to_string());
    candidates.iter().find_map(|addr| {
        // Connexion fermée aussitôt : rien n'est envoyé.
        tls::connect_until(addr, &trust, Instant::now() + PICK_TIMEOUT)
            .ok()
            .map(|_| addr.clone())
    })
}

pub(super) struct Shared {
    events: Sender<Event>,
    notify: Notifier,
    store: Store,
    /// Change à chaque début, fin ou remplacement de session : un fil de session dont le
    /// numéro n'est plus le bon s'arrête.
    generation: AtomicU64,
    creds: Mutex<Option<Creds>>,
    /// Autres adresses du serveur (fichier de connexion), gardées pour toute réécriture.
    alt_urls: Mutex<Vec<String>>,
    /// Version du protocole annoncée au serveur (celle de ce client, sauf dans les tests).
    protocol: u32,
}

impl Shared {
    pub(super) fn protocol(&self) -> u32 {
        self.protocol
    }

    pub(super) fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        (self.notify)();
    }

    pub(super) fn link(&self, link: Link) {
        self.emit(Event::Link(link));
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    // Un fil de travail qui panique (rattrapé plus bas) ne doit pas faire passer la session
    // pour « sans identifiants » : on reprend le verrou même empoisonné.
    fn creds(&self) -> Option<Creds> {
        self.creds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn alts(&self) -> Vec<String> {
        self.alt_urls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// La session bascule sur une autre adresse du même serveur (sans effet si elle a changé).
    pub(super) fn adopt_address(&self, generation: u64, creds: &Creds) {
        if self.generation() == generation {
            self.set_creds(Some(creds.clone()));
        }
    }

    fn set_creds(&self, creds: Option<Creds>) {
        // Le lecteur vidéo (`stream.rs`) n'a pas d'autre moyen de connaître la session en cours.
        super::stream::publish(self.generation(), creds.as_ref());
        *self.creds.lock().unwrap_or_else(PoisonError::into_inner) = creds;
    }

    /// Démarre une session (fil dédié) avec ces identifiants, et arrête la précédente.
    fn start_session(self: &Arc<Self>, creds: Creds) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.set_creds(Some(creds.clone()));
        self.link(Link::Connecting);
        let shared = self.clone();
        let spawned = std::thread::Builder::new()
            .name("matter-ws".into())
            .spawn(move || ws::run(shared, generation, creds));
        if spawned.is_err() {
            self.link(Link::LoggedOut {
                url: None,
                pinned: false,
                reason: Some("Impossible de démarrer la connexion (ressources du système).".into()),
            });
        }
    }

    /// Arrête la session en cours sans rien d'autre.
    fn stop_session(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.set_creds(None);
    }

    /// Efface le jeton du fichier de connexion en gardant l'adresse et l'empreinte. Rend
    /// l'adresse conservée et, si le fichier n'a pas pu être mis à jour, **ce qui a échoué** :
    /// l'appelant le met dans la raison de la déconnexion (un `Notice` émis juste avant
    /// `LoggedOut` serait effacé avec le journal). Si le fichier ne peut pas être réécrit, il est
    /// supprimé : un jeton révoqué ne doit jamais rester sur le disque pour repartir au lancement
    /// suivant.
    fn forget_token(&self, creds: Option<&Creds>) -> (Option<String>, Option<String>) {
        let mut problems: Vec<String> = Vec::new();
        let kept = match creds {
            Some(c) => Some(Saved {
                url: c.primary.url(),
                fingerprint: c.fingerprint.clone(),
                token: None,
                alt_urls: self.alts(),
            }),
            None => match self.store.load() {
                Ok(Some(saved)) => Some(Saved {
                    token: None,
                    ..saved
                }),
                _ => None,
            },
        };
        let Some(kept) = kept else {
            if let Err(e) = self.store.delete() {
                problems.push(e.message());
            }
            return (None, join_problems(problems));
        };
        if let Err(e) = self.store.save(&kept) {
            problems.push(e.message());
            if let Err(e) = self.store.delete() {
                problems.push(e.message());
            }
            return (None, join_problems(problems));
        }
        (Some(kept.url), None)
    }

    /// Le serveur ne reconnaît plus le jeton : on l'oublie (l'adresse et l'empreinte restent)
    /// et on revient à l'écran de connexion. Sans effet si la session a déjà changé.
    pub(super) fn token_rejected(&self, generation: u64) {
        if self
            .generation
            .compare_exchange(
                generation,
                generation + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return;
        }
        let creds = self.creds();
        self.set_creds(None);
        let url = creds.as_ref().map(|c| c.primary.url());
        let mut reason = "Votre session a expiré ou a été révoquée : reconnectez-vous.".to_string();
        if let Some(c) = creds {
            let kept = Saved {
                url: c.primary.url(),
                fingerprint: c.fingerprint,
                token: None,
                alt_urls: self.alts(),
            };
            if let Err(e) = self.store.save(&kept) {
                reason.push(' ');
                reason.push_str(&e.message());
                reason.push_str(" L'ancien jeton, déjà refusé, y est peut-être resté.");
            }
        }
        log::info!("jeton refusé par le serveur : retour à l'écran de connexion");
        self.link(Link::LoggedOut {
            url,
            pinned: true,
            reason: Some(reason),
        });
    }
}

/// Les échecs d'écriture du fichier de connexion, en un texte pour l'écran de connexion.
fn join_problems(problems: Vec<String>) -> Option<String> {
    (!problems.is_empty()).then(|| {
        format!(
            "{} Le jeton de la session précédente est peut-être resté dans ce fichier.",
            problems.join(" ")
        )
    })
}

enum Job {
    Probe {
        url: String,
    },
    Login {
        url: String,
        fingerprint: String,
        username: String,
        password: String,
    },
    /// Connexion avec l'adresse et l'empreinte déjà approuvées (fichier de connexion).
    LoginSaved {
        username: String,
        password: String,
    },
    Call {
        id: u64,
        generation: u64,
        creds: Creds,
        request: Request,
    },
    /// Requête dont le corps de la réponse est écrit dans un fichier, hors du fil de l'interface.
    Download {
        id: u64,
        generation: u64,
        creds: Creds,
        request: Request,
        path: PathBuf,
        /// Fichier en 0600 (la sauvegarde contient des clés privées).
        private: bool,
    },
    /// Lecture d'un fichier choisi par l'utilisateur (sauvegarde à importer).
    ReadFile {
        id: u64,
        path: PathBuf,
        max: u64,
    },
    Revoke {
        creds: Creds,
    },
}

pub struct Backend {
    shared: Arc<Shared>,
    jobs: Sender<Job>,
    next_id: AtomicU64,
}

impl Backend {
    /// Démarre le moteur. Si le fichier de connexion contient un jeton, la session s'ouvre
    /// aussitôt ; sinon l'interface reçoit `Link::LoggedOut`.
    pub fn start(store: Store, notify: Notifier) -> (Backend, Receiver<Event>) {
        Backend::start_with_protocol(store, notify, PROTOCOL_VERSION)
    }

    /// Comme [`Backend::start`], en annonçant une autre version du protocole : sert aux tests
    /// de la réaction à une version incompatible.
    #[doc(hidden)]
    pub fn start_with_protocol(
        store: Store,
        notify: Notifier,
        protocol: u32,
    ) -> (Backend, Receiver<Event>) {
        let (events, receiver) = mpsc::channel();
        let store_alts = store.load().ok().flatten().map(|s| s.alt_urls);
        let shared = Arc::new(Shared {
            events,
            notify,
            store,
            generation: AtomicU64::new(0),
            creds: Mutex::new(None),
            alt_urls: Mutex::new(store_alts.unwrap_or_default()),
            protocol,
        });
        let (jobs, jobs_rx) = mpsc::channel();
        let jobs_rx = Arc::new(Mutex::new(jobs_rx));
        for n in 0..HTTP_WORKERS {
            let (shared, rx) = (shared.clone(), jobs_rx.clone());
            let _ = std::thread::Builder::new()
                .name(format!("matter-http-{n}"))
                .spawn(move || worker(shared, rx));
        }
        let backend = Backend {
            shared: shared.clone(),
            jobs,
            next_id: AtomicU64::new(1),
        };
        match shared.store.load() {
            Ok(Some(Saved {
                url,
                fingerprint,
                token: Some(token),
                alt_urls,
            })) => match (ServerAddr::parse(&url), normalize_fingerprint(&fingerprint)) {
                (Ok(addr), Some(fingerprint)) => {
                    shared.start_session(Creds::new(addr, &alt_urls, fingerprint, token))
                }
                _ => shared.link(Link::LoggedOut {
                    url: None,
                    pinned: false,
                    reason: Some(
                        "Le fichier de connexion est incomplet : reconnectez-vous au serveur."
                            .into(),
                    ),
                }),
            },
            Ok(Some(saved)) => shared.link(Link::LoggedOut {
                url: Some(saved.url),
                pinned: true,
                reason: None,
            }),
            Ok(None) => shared.link(Link::LoggedOut {
                url: None,
                pinned: false,
                reason: None,
            }),
            Err(e) => shared.link(Link::LoggedOut {
                url: None,
                pinned: false,
                reason: Some(e.message()),
            }),
        }
        (backend, receiver)
    }

    fn submit(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    /// Première connexion : lit l'empreinte du certificat présenté (rien n'est envoyé). Réponse :
    /// `Event::Probed` ou `Event::ProbeFailed`.
    pub fn probe(&self, url: &str) {
        self.submit(Job::Probe {
            url: url.to_string(),
        });
    }

    /// Ouvre une session. `fingerprint` est celle que l'utilisateur a approuvée (ou celle
    /// déjà épinglée). Le mot de passe n'est ni conservé ni journalisé. Réponse :
    /// `Event::LoginFailed`, ou `Link::Connecting` puis la suite de la session.
    pub fn login(&self, url: &str, fingerprint: &str, username: &str, password: &str) {
        self.submit(Job::Login {
            url: url.to_string(),
            fingerprint: fingerprint.to_string(),
            username: username.to_string(),
            password: password.to_string(),
        });
    }

    /// Comme [`Backend::login`], avec l'adresse et l'empreinte déjà approuvées, conservées dans
    /// le fichier de connexion (jeton expiré ou révoqué).
    pub fn login_saved(&self, username: &str, password: &str) {
        self.submit(Job::LoginSaved {
            username: username.to_string(),
            password: password.to_string(),
        });
    }

    /// Autres adresses du serveur gardées dans le fichier de connexion (à distance).
    pub fn alt_urls(&self) -> Vec<String> {
        self.shared.alts()
    }

    /// Fixe les autres adresses du serveur (nom Tailscale, IP 100.x…). Prises en compte tout de
    /// suite par la session ouverte (à la prochaine reconnexion) et écrites dans le fichier de
    /// connexion s'il existe ; sinon à la prochaine connexion réussie.
    pub fn set_alt_urls(&self, urls: Vec<String>) {
        *self
            .shared
            .alt_urls
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = urls.clone();
        if let Ok(Some(saved)) = self.shared.store.load() {
            if saved.alt_urls != urls {
                if let Err(e) = self.shared.store.save(&Saved {
                    alt_urls: urls.clone(),
                    ..saved
                }) {
                    self.shared.emit(Event::Notice(e.message()));
                }
            }
        }
        if let Some(mut creds) = self.shared.creds() {
            creds.alts = parse_alts(&creds.primary, &urls);
            if creds.addr != creds.primary && !creds.alts.contains(&creds.addr) {
                creds.addr = creds.primary.clone();
            }
            self.shared.set_creds(Some(creds));
        }
    }

    /// Envoie une requête authentifiée. Le résultat arrive en `Event::Reply` avec le numéro
    /// rendu ici. Sans session, la réponse est une erreur immédiate.
    pub fn call(&self, request: Request) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        match self.shared.creds() {
            Some(creds) => self.submit(Job::Call {
                id,
                generation: self.shared.generation(),
                creds,
                request,
            }),
            None => self.shared.emit(Event::Reply {
                id,
                result: Err(NetError::Unreachable("pas de connexion au serveur".into())),
            }),
        }
        id
    }

    /// Comme [`Backend::call`], mais le corps de la réponse (export, CSV : plusieurs Mo) est
    /// écrit dans `path` par le fil réseau. `Event::Reply` rend alors un corps vide, ou
    /// l'erreur d'écriture.
    pub fn download(&self, request: Request, path: PathBuf, private: bool) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        match self.shared.creds() {
            Some(creds) => self.submit(Job::Download {
                id,
                generation: self.shared.generation(),
                creds,
                request,
                path,
                private,
            }),
            None => self.shared.emit(Event::Reply {
                id,
                result: Err(NetError::Unreachable("pas de connexion au serveur".into())),
            }),
        }
        id
    }

    /// Lit un fichier (au plus `max` octets) dans un fil réseau. Réponse : `Event::FileRead`,
    /// avec le contenu partagé (`Arc`) pour qu'il ne soit jamais copié.
    pub fn read_file(&self, path: PathBuf, max: u64) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.submit(Job::ReadFile { id, path, max });
        id
    }

    /// Déconnexion : le jeton est révoqué auprès du serveur puis effacé d'ici. L'adresse et
    /// l'empreinte du certificat sont **conservées** : il suffit de ressaisir le mot de passe.
    /// Oublier le certificat est un autre geste ([`Backend::reset_pinned_certificate`]).
    pub fn logout(&self) {
        let creds = self.shared.creds();
        self.shared.stop_session();
        let (url, problem) = self.shared.forget_token(creds.as_ref());
        self.shared.link(Link::LoggedOut {
            pinned: url.is_some(),
            url,
            reason: problem,
        });
        if let Some(creds) = creds {
            self.submit(Job::Revoke { creds });
        }
    }

    /// Oublie l'empreinte épinglée (et le jeton) : la prochaine connexion repasse par la
    /// confirmation de l'empreinte. Geste explicite, jamais automatique.
    pub fn reset_pinned_certificate(&self, url: Option<String>) {
        self.shared.stop_session();
        self.shared
            .alt_urls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        let reason = self.shared.store.delete().err().map(|e| e.message());
        self.shared.link(Link::LoggedOut {
            url,
            pinned: false,
            reason,
        });
    }

    /// Relance la connexion avec les identifiants du fichier (après une version incompatible
    /// corrigée, par exemple).
    pub fn reconnect(&self) {
        match self.shared.store.load() {
            Ok(Some(Saved {
                url,
                fingerprint,
                token: Some(token),
                alt_urls,
            })) => {
                if let (Ok(addr), Some(fingerprint)) =
                    (ServerAddr::parse(&url), normalize_fingerprint(&fingerprint))
                {
                    self.shared
                        .start_session(Creds::new(addr, &alt_urls, fingerprint, token));
                }
            }
            Ok(Some(saved)) => self.shared.link(Link::LoggedOut {
                url: Some(saved.url),
                pinned: true,
                reason: None,
            }),
            _ => self.shared.link(Link::LoggedOut {
                url: None,
                pinned: false,
                reason: None,
            }),
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Arrête la session ; les fils HTTP s'arrêtent quand la file se ferme.
        self.shared.stop_session();
    }
}

fn worker(shared: Arc<Shared>, jobs: Arc<Mutex<Receiver<Job>>>) {
    loop {
        let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok(job) = job else { return };
        // Un fil HTTP ne meurt jamais sans répondre : une panique dans une tâche devient une
        // erreur visible pour son demandeur (sinon l'écran de connexion resterait sur
        // « connexion en cours » et la requête sans réponse), et le fil continue.
        guarded(&shared, job, run);
    }
}

/// Exécute `work` en rattrapant une panique : le demandeur reçoit une erreur, le fil survit.
fn guarded(shared: &Arc<Shared>, job: Job, work: impl FnOnce(&Arc<Shared>, Job)) {
    let failure = job.failure();
    if catch_unwind(AssertUnwindSafe(|| work(shared, job))).is_err() {
        log::error!("erreur interne dans une tâche réseau");
        shared.emit(failure);
    }
}

impl Job {
    /// Ce que l'interface reçoit si la tâche plante.
    fn failure(&self) -> Event {
        let internal = || NetError::Local("Erreur interne du client (voir le journal).".into());
        match self {
            Job::Probe { .. } => Event::ProbeFailed(internal()),
            Job::Login { .. } | Job::LoginSaved { .. } => Event::LoginFailed(internal()),
            Job::Call { id, .. } | Job::Download { id, .. } => Event::Reply {
                id: *id,
                result: Err(internal()),
            },
            Job::ReadFile { id, .. } => Event::FileRead {
                id: *id,
                result: Err(internal()),
            },
            Job::Revoke { .. } => Event::Notice(
                "Déconnecté. Le serveur n'a pas pu être prévenu (erreur interne) : le jeton a \
                 été supprimé de ce Mac."
                    .into(),
            ),
        }
    }
}

fn run(shared: &Arc<Shared>, job: Job) {
    match job {
        Job::Probe { url } => probe(shared, &url),
        Job::Login {
            url,
            fingerprint,
            username,
            password,
        } => login(shared, &url, &fingerprint, &username, &password),
        Job::LoginSaved { username, password } => match shared.store.load() {
            Ok(Some(saved)) => login(shared, &saved.url, &saved.fingerprint, &username, &password),
            Ok(None) => shared.emit(Event::LoginFailed(NetError::Local(
                "Aucun serveur approuvé : saisissez son adresse.".into(),
            ))),
            Err(e) => shared.emit(Event::LoginFailed(e)),
        },
        Job::Call {
            id,
            generation,
            creds,
            request,
        } => {
            // Déconnecté (ou session remplacée) entre la demande et son tour dans la file : le
            // jeton de cette session ne part plus nulle part.
            if shared.generation() != generation {
                shared.emit(Event::Reply {
                    id,
                    result: Err(NetError::Unreachable("session fermée".into())),
                });
                return;
            }
            let result = http::exec(
                &creds.addr,
                &Trust::Pinned(creds.fingerprint.clone()),
                Some(&creds.token),
                &request,
            )
            .and_then(http::Response::into_result);
            if result.as_ref().is_err_and(NetError::is_unauthorized) {
                shared.token_rejected(generation);
            }
            shared.emit(Event::Reply { id, result });
        }
        Job::Download {
            id,
            generation,
            creds,
            request,
            path,
            private,
        } => {
            if shared.generation() != generation {
                shared.emit(Event::Reply {
                    id,
                    result: Err(NetError::Unreachable("session fermée".into())),
                });
                return;
            }
            let result = http::exec(
                &creds.addr,
                &Trust::Pinned(creds.fingerprint.clone()),
                Some(&creds.token),
                &request,
            )
            .and_then(http::Response::into_result);
            if result.as_ref().is_err_and(NetError::is_unauthorized) {
                shared.token_rejected(generation);
            }
            // Le corps part dans le fichier, sans copie ; la réponse n'en garde que l'état.
            let result = result.and_then(|mut response| {
                let body = std::mem::take(&mut response.body);
                files::write_file(&path, &body, private).map_err(|e| {
                    NetError::Local(format!(
                        "écriture impossible dans {} ({})",
                        path.display(),
                        e.kind()
                    ))
                })?;
                Ok(response)
            });
            shared.emit(Event::Reply { id, result });
        }
        Job::ReadFile { id, path, max } => shared.emit(Event::FileRead {
            id,
            result: files::read_limited(&path, max)
                .map(Arc::new)
                .map_err(NetError::Local),
        }),
        Job::Revoke { creds } => {
            let request = Request::new("POST", "/api/logout").with_timeout(Duration::from_secs(8));
            let revoked = http::exec(
                &creds.addr,
                &Trust::Pinned(creds.fingerprint),
                Some(&creds.token),
                &request,
            )
            .and_then(http::Response::into_result);
            shared.emit(Event::Notice(match revoked {
                Ok(_) => "Déconnecté : le jeton a été révoqué sur le serveur.".into(),
                Err(e) if e.is_unauthorized() => {
                    "Déconnecté (le jeton n'était plus valable).".into()
                }
                Err(_) => "Déconnecté. Le serveur n'a pas pu être prévenu : le jeton a été \
                           supprimé de ce Mac et expirera de lui-même sur le serveur."
                    .into(),
            }));
        }
    }
}

fn probe(shared: &Shared, url: &str) {
    let result = ServerAddr::parse(url)
        .and_then(|addr| tls::connect(&addr, &Trust::Probe).map(|c| (addr, c.fingerprint)));
    match result {
        // La connexion est fermée ici, sans qu'un seul octet n'ait été envoyé.
        Ok((addr, fingerprint)) => shared.emit(Event::Probed {
            url: addr.url(),
            fingerprint,
        }),
        Err(e) => shared.emit(Event::ProbeFailed(e)),
    }
}

fn login(shared: &Arc<Shared>, url: &str, fingerprint: &str, username: &str, password: &str) {
    match try_login(shared, url, fingerprint, username, password) {
        Ok(creds) => {
            let saved = Saved {
                url: creds.primary.url(),
                fingerprint: creds.fingerprint.clone(),
                token: Some(creds.token.clone()),
                alt_urls: shared.alts(),
            };
            if let Err(e) = shared.store.save(&saved) {
                shared.emit(Event::Notice(format!(
                    "{} La session reste ouverte, mais il faudra se reconnecter au prochain lancement.",
                    e.message()
                )));
            }
            log::info!("connexion acceptée par {}", creds.addr.url());
            shared.start_session(creds);
        }
        Err(e) => shared.emit(Event::LoginFailed(e)),
    }
}

fn try_login(
    shared: &Shared,
    url: &str,
    fingerprint: &str,
    username: &str,
    password: &str,
) -> Result<Creds, NetError> {
    let primary = ServerAddr::parse(url)?;
    let fingerprint = normalize_fingerprint(fingerprint)
        .ok_or_else(|| NetError::Protocol("empreinte du certificat invalide".into()))?;
    let mut creds = Creds::new(primary, &shared.alts(), fingerprint.clone(), String::new());
    // Plusieurs adresses : la première qui répond avec le bon certificat (maison, sinon à
    // distance). À défaut, la principale, dont l'erreur sera celle que l'utilisateur lit.
    if let Some(found) = pick_address(&creds.candidates(), &fingerprint) {
        creds.addr = found;
    }
    let addr = creds.addr.clone();
    let trust = Trust::Pinned(fingerprint.clone());
    // Version annoncée d'abord : un serveur incompatible est signalé clairement, avant de lui
    // confier un mot de passe.
    let version: VersionInfo = http::exec(&addr, &trust, None, &Request::get("/api/version"))?
        .into_result()?
        .json()?;
    if version.protocol_version != shared.protocol() {
        return Err(NetError::Api {
            status: 426,
            code: Some(ErrorCode::VersionIncompatible),
            detail: None,
            retry_after_s: None,
        });
    }
    let body = LoginRequest {
        username: username.to_string(),
        password: password.to_string(),
        protocol_version: shared.protocol(),
    };
    let response: LoginResponse = http::exec(
        &addr,
        &trust,
        None,
        &Request::json("POST", "/api/login", &body),
    )?
    .into_result()?
    .json()?;
    creds.token = response.token;
    Ok(creds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("mhd-backend-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::at(dir.join("connexion.json"))
    }

    fn wait_link(rx: &Receiver<Event>, pick: impl Fn(&Link) -> bool) -> Link {
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Event::Link(l)) if pick(&l) => return l,
                Ok(_) => {}
                Err(_) => panic!("liaison attendue non reçue"),
            }
        }
    }

    #[test]
    fn a_panicking_task_still_answers_its_requester() {
        let (events, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            events,
            notify: Arc::new(|| {}),
            store: temp_store("panic"),
            generation: AtomicU64::new(0),
            creds: Mutex::new(None),
            alt_urls: Mutex::new(Vec::new()),
            protocol: PROTOCOL_VERSION,
        });
        let creds = Creds::new(
            ServerAddr::parse("https://127.0.0.1:1").unwrap(),
            &[],
            "AA".into(),
            "t".into(),
        );
        let call = Job::Call {
            id: 7,
            generation: 0,
            creds,
            request: Request::get("/api/state"),
        };
        guarded(&shared, call, |_, _| panic!("panique voulue par le test"));
        assert!(matches!(
            rx.try_recv(),
            Ok(Event::Reply {
                id: 7,
                result: Err(NetError::Local(_))
            })
        ));
        for (job, check) in [
            (
                Job::Probe { url: "x".into() },
                (|e: &Event| matches!(e, Event::ProbeFailed(_))) as fn(&Event) -> bool,
            ),
            (
                Job::LoginSaved {
                    username: "u".into(),
                    password: "p".into(),
                },
                |e| matches!(e, Event::LoginFailed(_)),
            ),
            (
                Job::Revoke {
                    creds: Creds::new(
                        ServerAddr::parse("https://127.0.0.1:1").unwrap(),
                        &[],
                        "AA".into(),
                        "t".into(),
                    ),
                },
                |e| matches!(e, Event::Notice(_)),
            ),
        ] {
            guarded(&shared, job, |_, _| panic!("panique voulue par le test"));
            assert!(check(&rx.try_recv().unwrap()));
        }
    }

    /// Un fichier de connexion qu'on ne peut pas mettre à jour n'est pas signalé par une
    /// `Notice` (effacée avec le journal par `LoggedOut`) : la raison de la déconnexion le dit,
    /// et l'écran de connexion l'affiche.
    #[test]
    fn a_connection_file_that_cannot_be_updated_is_named_in_the_logout_reason() {
        use std::os::unix::fs::PermissionsExt;
        let store = temp_store("logout-fail");
        let fingerprint = tls::fingerprint_of(b"un certificat");
        store
            .save(&Saved {
                url: "https://127.0.0.1:1".into(),
                fingerprint,
                token: Some("jeton-secret".into()),
                alt_urls: Vec::new(),
            })
            .unwrap();
        let dir = store.path().parent().unwrap().to_path_buf();
        // Dossier en lecture seule : ni réécriture, ni suppression. (Sans effet pour root.)
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let restore = || std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        if std::fs::write(dir.join("sonde"), b"").is_ok() {
            restore().unwrap();
            return;
        }
        let (backend, rx) = Backend::start(store.clone(), Arc::new(|| {}));
        backend.logout();
        let mut notices = 0;
        let mut reason = None;
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(10)) {
            match event {
                Event::Notice(_) => notices += 1,
                Event::Link(Link::LoggedOut { reason: r, .. }) => {
                    reason = r;
                    break;
                }
                _ => {}
            }
        }
        backend.reset_pinned_certificate(None);
        let second = wait_link(&rx, |l| matches!(l, Link::LoggedOut { .. }));
        restore().unwrap();
        let reason = reason.expect("la raison doit dire ce qui a échoué");
        assert!(
            reason.contains("Impossible") && reason.contains("jeton"),
            "{reason}"
        );
        assert_eq!(notices, 0, "pas de Notice juste avant LoggedOut");
        match second {
            Link::LoggedOut {
                reason: Some(r), ..
            } => assert!(r.contains("Impossible de supprimer"), "{r}"),
            other => panic!("{other:?}"),
        }
    }

    /// Se déconnecter n'oublie que le jeton : adresse et empreinte restent, le jeton révoqué
    /// ne revient jamais (ni au fichier, ni dans une requête déjà en file).
    #[test]
    fn logout_keeps_the_address_and_the_fingerprint_but_never_the_token() {
        let store = temp_store("logout");
        let fingerprint = tls::fingerprint_of(b"un certificat");
        store
            .save(&Saved {
                url: "https://127.0.0.1:1".into(),
                fingerprint: fingerprint.clone(),
                token: Some("jeton-secret".into()),
                alt_urls: Vec::new(),
            })
            .unwrap();
        // Boucle locale, port fermé : la session essaie de se connecter et échoue aussitôt.
        let (backend, rx) = Backend::start(store.clone(), Arc::new(|| {}));
        let queued = backend.call(Request::get("/api/state"));
        backend.logout();
        let link = wait_link(&rx, |l| matches!(l, Link::LoggedOut { .. }));
        assert_eq!(
            link,
            Link::LoggedOut {
                url: Some("https://127.0.0.1:1".into()),
                pinned: true,
                reason: None
            }
        );
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.url, "https://127.0.0.1:1");
        assert_eq!(saved.fingerprint, fingerprint);
        assert_eq!(saved.token, None);
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert!(!raw.contains("jeton-secret"));
        // Sans session, plus aucune requête n'est possible, et « reconnecter » ne retrouve
        // aucun jeton.
        let id = backend.call(Request::get("/api/state"));
        backend.reconnect();
        let mut answered = Vec::new();
        let mut reconnect_link = None;
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(2)) {
            match event {
                Event::Reply { id, result } => answered.push((id, result.is_err())),
                Event::Link(l @ Link::LoggedOut { .. }) => reconnect_link = Some(l),
                Event::Link(Link::Connecting) => panic!("la session ne doit pas repartir"),
                _ => {}
            }
        }
        assert!(answered.contains(&(id, true)));
        assert!(
            answered.iter().all(|(_, failed)| *failed),
            "la requête déjà en file ne part pas avec le jeton révoqué : {answered:?} ({queued})"
        );
        assert!(matches!(
            reconnect_link,
            Some(Link::LoggedOut {
                url: Some(_),
                pinned: true,
                ..
            })
        ));
    }
}
