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
use std::time::Duration;

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
    pub addr: ServerAddr,
    pub fingerprint: String,
    pub token: String,
}

impl Creds {
    /// Quel serveur : adresse et empreinte (jamais le jeton).
    pub(super) fn identity(&self) -> String {
        format!("{}#{}", self.addr.url(), self.fingerprint)
    }
}

pub(super) struct Shared {
    events: Sender<Event>,
    notify: Notifier,
    store: Store,
    /// Change à chaque début, fin ou remplacement de session : un fil de session dont le
    /// numéro n'est plus le bon s'arrête.
    generation: AtomicU64,
    creds: Mutex<Option<Creds>>,
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

    fn set_creds(&self, creds: Option<Creds>) {
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
    /// l'adresse conservée. Si le fichier ne peut pas être réécrit, il est supprimé : un jeton
    /// révoqué ne doit jamais rester sur le disque pour repartir au lancement suivant.
    fn forget_token(&self, creds: Option<&Creds>) -> Option<String> {
        let kept = match creds {
            Some(c) => Some(Saved {
                url: c.addr.url(),
                fingerprint: c.fingerprint.clone(),
                token: None,
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
                self.emit(Event::Notice(e.message()));
            }
            return None;
        };
        if let Err(e) = self.store.save(&kept) {
            self.emit(Event::Notice(e.message()));
            if let Err(e) = self.store.delete() {
                self.emit(Event::Notice(e.message()));
            }
            return None;
        }
        Some(kept.url)
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
        let url = creds.as_ref().map(|c| c.addr.url());
        if let Some(c) = creds {
            let kept = Saved {
                url: c.addr.url(),
                fingerprint: c.fingerprint,
                token: None,
            };
            if let Err(e) = self.store.save(&kept) {
                self.emit(Event::Notice(e.message()));
            }
        }
        log::info!("jeton refusé par le serveur : retour à l'écran de connexion");
        self.link(Link::LoggedOut {
            url,
            pinned: true,
            reason: Some(
                "Votre session a expiré ou a été révoquée : reconnectez-vous.".to_string(),
            ),
        });
    }
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
        let shared = Arc::new(Shared {
            events,
            notify,
            store,
            generation: AtomicU64::new(0),
            creds: Mutex::new(None),
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
            })) => match (ServerAddr::parse(&url), normalize_fingerprint(&fingerprint)) {
                (Ok(addr), Some(fingerprint)) => shared.start_session(Creds {
                    addr,
                    fingerprint,
                    token,
                }),
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

    /// Quel serveur est joint (adresse et empreinte), s'il y a une session.
    pub fn server_identity(&self) -> Option<String> {
        self.shared.creds().map(|c| c.identity())
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
        let url = self.shared.forget_token(creds.as_ref());
        self.shared.link(Link::LoggedOut {
            pinned: url.is_some(),
            url,
            reason: None,
        });
        if let Some(creds) = creds {
            self.submit(Job::Revoke { creds });
        }
    }

    /// Oublie l'empreinte épinglée (et le jeton) : la prochaine connexion repasse par la
    /// confirmation de l'empreinte. Geste explicite, jamais automatique.
    pub fn reset_pinned_certificate(&self, url: Option<String>) {
        self.shared.stop_session();
        if let Err(e) = self.shared.store.delete() {
            self.shared.emit(Event::Notice(e.message()));
        }
        self.shared.link(Link::LoggedOut {
            url,
            pinned: false,
            reason: None,
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
            })) => {
                if let (Ok(addr), Some(fingerprint)) =
                    (ServerAddr::parse(&url), normalize_fingerprint(&fingerprint))
                {
                    self.shared.start_session(Creds {
                        addr,
                        fingerprint,
                        token,
                    });
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
                url: creds.addr.url(),
                fingerprint: creds.fingerprint.clone(),
                token: Some(creds.token.clone()),
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
    let addr = ServerAddr::parse(url)?;
    let fingerprint = normalize_fingerprint(fingerprint)
        .ok_or_else(|| NetError::Protocol("empreinte du certificat invalide".into()))?;
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
    Ok(Creds {
        addr,
        fingerprint,
        token: response.token,
    })
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
            protocol: PROTOCOL_VERSION,
        });
        let creds = Creds {
            addr: ServerAddr::parse("https://127.0.0.1:1").unwrap(),
            fingerprint: "AA".into(),
            token: "t".into(),
        };
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
                    creds: Creds {
                        addr: ServerAddr::parse("https://127.0.0.1:1").unwrap(),
                        fingerprint: "AA".into(),
                        token: "t".into(),
                    },
                },
                |e| matches!(e, Event::Notice(_)),
            ),
        ] {
            guarded(&shared, job, |_, _| panic!("panique voulue par le test"));
            assert!(check(&rx.try_recv().unwrap()));
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
