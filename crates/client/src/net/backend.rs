//! Le moteur réseau : une petite file de requêtes HTTPS traitées par quelques fils, et la
//! session (WebSocket) dans son propre fil.
//!
//! Le parallélisme HTTP est borné à [`HTTP_WORKERS`] : le serveur plafonne à 16 connexions par
//! adresse (docs/protocole.md), et une requête = une connexion.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use protocol::wire::{ErrorCode, LoginRequest, LoginResponse, VersionInfo, PROTOCOL_VERSION};

use super::addr::ServerAddr;
use super::error::NetError;
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

    fn creds(&self) -> Option<Creds> {
        self.creds.lock().ok().and_then(|c| c.clone())
    }

    fn set_creds(&self, creds: Option<Creds>) {
        if let Ok(mut c) = self.creds.lock() {
            *c = creds;
        }
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

    /// Déconnexion : le jeton est révoqué auprès du serveur puis tout est supprimé ici.
    pub fn logout(&self) {
        let creds = self.shared.creds();
        self.shared.stop_session();
        if let Err(e) = self.shared.store.delete() {
            self.shared.emit(Event::Notice(e.message()));
        }
        self.shared.link(Link::LoggedOut {
            url: None,
            pinned: false,
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
        let job = match jobs.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(job) = job else { return };
        match job {
            Job::Probe { url } => probe(&shared, &url),
            Job::Login {
                url,
                fingerprint,
                username,
                password,
            } => login(&shared, &url, &fingerprint, &username, &password),
            Job::LoginSaved { username, password } => match shared.store.load() {
                Ok(Some(saved)) => login(
                    &shared,
                    &saved.url,
                    &saved.fingerprint,
                    &username,
                    &password,
                ),
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
            Job::Revoke { creds } => {
                let request =
                    Request::new("POST", "/api/logout").with_timeout(Duration::from_secs(8));
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
