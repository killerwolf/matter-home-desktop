//! Notifications web push de l'app web (ticket n° 17) : l'iPhone ou l'iPad reçoit une alerte même
//! quand l'app web est fermée (PWA ajoutée à l'écran d'accueil, iOS 16.4 ou plus). Voir
//! `docs/notifications.md`.
//!
//! # La chaîne
//!
//! ```text
//! règle « Notifier » (Event::Notification) ─┐
//! passage détecté, clos ou décrit (watcher) ─┼─▶ texte nettoyé (content.rs) ─▶ file bornée (64)
//! pile faible, appareil injoignable (Hub) ──┘
//!                    fil « notifications » : pour chaque abonnement qui veut la catégorie,
//!                    chiffrement RFC 8291 (crypto.rs) + jeton VAPID ─▶ curl ─▶ service de push
//!                    (Apple : web.push.apple.com) ─▶ appareil (service worker de l'app web)
//! ```
//!
//! - **Rien ne bloque** : le fil des événements, l'analyse des caméras et les règles ne font que
//!   déposer un message dans la file ; un seul fil envoie.
//! - **Ce qui sort du Mac** : le message chiffré de bout en bout (seul l'appareil le déchiffre),
//!   l'adresse d'abonnement et le jeton VAPID, vers le service de push de l'appareil seulement.
//! - **Abonnement mort** (404, 410) : oublié aussitôt. 429 et 5xx : un nouvel essai.
//! - **Démonstration** : rien n'est écrit ni envoyé (les abonnements restent en mémoire).

pub mod content;
pub mod crypto;
pub mod store;
pub mod transport;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use protocol::wire::{
    DetectionEventView, PushCategories, PushPayload, PushStatus, PushSubscribeRequest,
    PushSubscriptionView, PushUpdateRequest, PUSH_LABEL_MAX, PUSH_MAX_SUBSCRIPTIONS,
};

use crate::events::{now_unix, Event, MeasureKind, NodeStatus, StateKind};
use crypto::VapidKey;
use store::{Files, Stored, Subscription};
use transport::{PushRequest, Transport};

/// Messages en attente au plus ; au-delà, le plus ancien part (une ligne au journal).
pub const QUEUE_MAX: usize = 64;
/// Pile faible : en dessous (%), un message ; il n'y en a pas d'autre avant de repasser au-dessus
/// de [`BATTERY_REARM`].
pub const BATTERY_LOW: f64 = 15.0;
pub const BATTERY_REARM: f64 = 20.0;
/// Une mesure plus ancienne (historique rejoué au démarrage) ne déclenche rien.
const RECENT_S: f64 = 600.0;
/// Un appareil injoignable depuis plus longtemps est signalé (une fois par panne).
pub const UNREACHABLE_AFTER: Duration = Duration::from_secs(10 * 60);
/// Le fil vérifie les pannes à ce rythme.
const TICK: Duration = Duration::from_secs(30);
/// Passages déjà notifiés retenus (un seul message par passage).
const SENT_KEPT: usize = 256;

/// Ce qu'annonce un message (et ce qu'un appareil choisit de recevoir).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Rules,
    Detections,
    Battery,
    Unreachable,
    /// Une demande d'un agent d'IA à valider (envoyée à une seule personne).
    AgentRequests,
}

impl Category {
    fn wanted_by(self, c: &PushCategories) -> bool {
        match self {
            Category::Rules => c.rules,
            Category::Detections => c.detections,
            Category::Battery => c.battery,
            Category::Unreachable => c.unreachable,
            Category::AgentRequests => c.agent_requests,
        }
    }

    /// Urgence (`Urgency`) et durée de vie (`TTL`, s) chez le service de push : une alarme part
    /// tout de suite et ne sert plus à rien après une heure ; une pile faible peut attendre.
    fn delivery(self) -> (&'static str, u64) {
        match self {
            Category::Rules | Category::Detections => ("high", 3600),
            Category::Battery => ("normal", 24 * 3600),
            Category::Unreachable => ("normal", 6 * 3600),
            // Une demande expire au bout de 5 minutes : plus aucun intérêt après.
            Category::AgentRequests => ("high", 300),
        }
    }
}

/// Un refus d'une demande d'un client (texte court, renvoyé comme détail d'erreur).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    Invalid(&'static str),
    TooMany,
    NotFound,
    Disabled,
    Storage,
}

struct Job {
    /// Un seul abonnement (notification d'essai) ; sinon tous ceux qui veulent la catégorie.
    target: Option<String>,
    /// Les abonnements de cette personne seulement (jamais ceux sans propriétaire).
    owner: Option<String>,
    category: Option<Category>,
    payload: PushPayload,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

/// Un passage n'est notifié qu'une fois : à sa fin, ou à l'arrivée de sa description si elle est
/// attendue (voir [`Push::detection_closed`]).
#[derive(Default)]
struct Detections {
    pending: HashSet<String>,
    sent: VecDeque<String>,
}

/// Une panne en cours.
struct Outage {
    since: Instant,
    name: Option<String>,
    alerted: bool,
}

/// Le résultat d'un envoi à un appareil.
#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    Sent,
    /// 404 ou 410 : l'abonnement n'existe plus (app supprimée, autorisation retirée).
    Gone,
    Failed(String),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Push {
    files: Files,
    vapid: VapidKey,
    stored: Mutex<Stored>,
    transport: Mutex<Arc<dyn Transport>>,
    shared: Arc<Shared>,
    worker_started: Mutex<bool>,
    detections: Mutex<Detections>,
    outages: Mutex<BTreeMap<u64, Outage>>,
    /// Attente avant le nouvel essai quand la réponse ne dit pas `retry-after`.
    retry_default: Mutex<Duration>,
    /// Le compte de cette personne est-il actif ? Les appareils d'une personne désactivée ne
    /// reçoivent plus rien (sans filtre : tout le monde reçoit).
    owner_active: Mutex<Option<OwnerFilter>>,
}

/// Voir `Push::set_owner_filter`.
pub type OwnerFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

impl Push {
    /// Lit `serveur/push-vapid.json` et `serveur/push.json` (créés au besoin). En démonstration,
    /// rien n'est lu ni écrit, et rien ne part.
    pub fn new(data_dir: &Path, demo: bool) -> Push {
        let server_dir = if demo {
            None
        } else {
            match crate::security::server_dir(data_dir) {
                Ok(dir) => Some(dir),
                Err(e) => {
                    log::error!("notifications : dossier du serveur inutilisable : {e:#}");
                    None
                }
            }
        };
        if !demo {
            crate::descriptions::clean_leftover_bodies(transport::BODY_PREFIX);
        }
        let files = Files::new(server_dir.as_deref());
        let (vapid, created) = files.load_or_create_vapid().unwrap_or_else(|e| {
            log::error!("notifications : clé VAPID non enregistrée, en mémoire seulement : {e:#}");
            (VapidKey::generate().expect("clé VAPID"), true)
        });
        let mut stored = files.load();
        if created && !stored.subscriptions.is_empty() {
            log::warn!(
                "notifications : nouvelle clé VAPID, {} abonnement(s) oublié(s) : réabonnez les appareils",
                stored.subscriptions.len()
            );
            stored.subscriptions.clear();
            if let Err(e) = files.save(&stored) {
                log::warn!("notifications : {e:#}");
            }
        }
        // Les abonnements d'avant les comptes nominatifs reviennent au premier utilisateur.
        if let Some(dir) = &server_dir {
            if stored.subscriptions.iter().any(|s| s.user_id.is_none()) {
                if let Some(first) = crate::security::account::first_user_id(dir) {
                    for sub in stored
                        .subscriptions
                        .iter_mut()
                        .filter(|s| s.user_id.is_none())
                    {
                        sub.user_id = Some(first.clone());
                    }
                    match files.save(&stored) {
                        Ok(()) => log::info!(
                            "notifications : abonnements existants rattachés au premier utilisateur"
                        ),
                        Err(e) => log::warn!("notifications : {e:#}"),
                    }
                }
            }
        }
        let transport: Arc<dyn Transport> = if demo || cfg!(test) {
            Arc::new(transport::NullTransport)
        } else {
            Arc::new(transport::CurlTransport::default())
        };
        Push {
            files,
            vapid,
            stored: Mutex::new(stored),
            transport: Mutex::new(transport),
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue::default()),
                wake: Condvar::new(),
            }),
            worker_started: Mutex::new(false),
            detections: Mutex::new(Detections::default()),
            outages: Mutex::new(BTreeMap::new()),
            retry_default: Mutex::new(Duration::from_secs(2)),
            owner_active: Mutex::new(None),
        }
    }

    /// Rattache à `user_id` les abonnements d'avant les comptes nominatifs (sans propriétaire).
    /// Idempotent : sans abonnement orphelin, rien n'est écrit. Les abonnements, leurs clés et
    /// leurs catégories restent tels quels.
    pub fn adopt_unowned(&self, user_id: &str) -> Result<usize, PushError> {
        self.update_stored(|s| {
            let mut n = 0;
            for sub in s.subscriptions.iter_mut().filter(|x| x.user_id.is_none()) {
                sub.user_id = Some(user_id.to_string());
                n += 1;
            }
            n
        })
    }

    /// Dit si le compte d'une personne est actif : les appareils des personnes désactivées ne
    /// reçoivent plus de notifications.
    pub fn set_owner_filter(&self, filter: OwnerFilter) {
        *lock(&self.owner_active) = Some(filter);
    }

    /// Remplace l'envoi (tests d'intégration : un faux service local, jamais le réseau).
    pub fn set_transport(&self, transport: Arc<dyn Transport>) {
        *lock(&self.transport) = transport;
    }

    #[cfg(test)]
    pub(crate) fn set_retry_default(&self, wait: Duration) {
        *lock(&self.retry_default) = wait;
    }

    /// Tests : pas de fil, les messages sont envoyés par [`Push::run_pending`].
    #[cfg(test)]
    pub(crate) fn manual(&self) {
        *lock(&self.worker_started) = true;
    }

    #[cfg(test)]
    pub(crate) fn run_pending(&self) {
        loop {
            let job = lock(&self.shared.queue).jobs.pop_front();
            match job {
                Some(job) => self.process(&job),
                None => return,
            }
        }
    }

    pub fn public_key(&self) -> String {
        self.vapid.public_key_b64()
    }

    /// Tous les abonnements (fenêtre du serveur).
    pub fn status(&self) -> PushStatus {
        self.status_of(None)
    }

    /// Les abonnements de cette personne seulement, jamais ceux des autres (app web).
    pub fn status_for(&self, owner: &str) -> PushStatus {
        self.status_of(Some(owner))
    }

    fn status_of(&self, owner: Option<&str>) -> PushStatus {
        let stored = lock(&self.stored);
        PushStatus {
            enabled: stored.enabled,
            public_key: self.public_key(),
            subscriptions: stored
                .subscriptions
                .iter()
                .filter(|s| owner.is_none_or(|o| s.user_id.as_deref() == Some(o)))
                .map(view)
                .collect(),
        }
    }

    pub fn enabled(&self) -> bool {
        lock(&self.stored).enabled
    }

    fn update_stored<R>(&self, change: impl FnOnce(&mut Stored) -> R) -> Result<R, PushError> {
        let mut stored = lock(&self.stored);
        let mut next = stored.clone();
        let result = change(&mut next);
        if next != *stored {
            self.files.save(&next).map_err(|e| {
                log::error!("notifications : {e:#}");
                PushError::Storage
            })?;
            *stored = next;
        }
        Ok(result)
    }

    /// Le réglage général (fenêtre du serveur, app web).
    pub fn set_enabled(&self, enabled: bool) -> Result<(), PushError> {
        self.update_stored(|s| s.enabled = enabled)?;
        log::info!(
            "notifications : {}",
            if enabled { "activées" } else { "coupées" }
        );
        Ok(())
    }

    /// Enregistre un abonnement sans propriétaire (ou met à jour celui de la même adresse).
    pub fn subscribe(
        &self,
        request: PushSubscribeRequest,
        user_agent: &str,
    ) -> Result<PushSubscriptionView, PushError> {
        self.subscribe_inner(None, request, user_agent)
    }

    /// Enregistre l'abonnement d'un appareil au nom de cette personne (ou met à jour celui de la
    /// même adresse, qui lui revient : le téléphone a changé de compte).
    pub fn subscribe_for(
        &self,
        owner: &str,
        request: PushSubscribeRequest,
        user_agent: &str,
    ) -> Result<PushSubscriptionView, PushError> {
        self.subscribe_inner(Some(owner), request, user_agent)
    }

    fn subscribe_inner(
        &self,
        owner: Option<&str>,
        request: PushSubscribeRequest,
        user_agent: &str,
    ) -> Result<PushSubscriptionView, PushError> {
        let refused = PushError::Invalid("adresse d'abonnement refusée (service de push inconnu)");
        let raw = request.endpoint.trim();
        if transport::service_of(raw).is_none() {
            return Err(refused);
        }
        // L'adresse normalisée (hôte en minuscules…) : c'est elle qui est gardée, comparée et
        // envoyée, et elle est vérifiée de nouveau.
        let endpoint = url::Url::parse(raw)
            .map(|u| u.as_str().to_string())
            .map_err(|_| refused.clone())?;
        if transport::service_of(&endpoint).is_none() {
            return Err(refused);
        }
        let p256dh = crypto::b64url_decode(&request.keys.p256dh)
            .filter(|k| k.len() == crypto::PUBLIC_KEY_LEN && k[0] == 0x04)
            .ok_or(PushError::Invalid("clé p256dh invalide"))?;
        // Un point qui n'est pas sur la courbe serait refusé à chaque envoi : refusé tout de suite.
        if crypto::encrypt(&p256dh, &[0u8; crypto::AUTH_LEN], b"").is_err() {
            return Err(PushError::Invalid("clé p256dh invalide"));
        }
        let auth = crypto::b64url_decode(&request.keys.auth)
            .filter(|k| k.len() == crypto::AUTH_LEN)
            .ok_or(PushError::Invalid("secret auth invalide"))?;
        let label = clean_label(&request.label);
        let user_agent: String = user_agent
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        self.update_stored(|s| {
            if let Some(existing) = s.subscriptions.iter_mut().find(|x| x.endpoint == endpoint) {
                existing.p256dh = crypto::b64url(&p256dh);
                existing.auth = crypto::b64url(&auth);
                // Le nom n'est donné qu'à la création : l'app web renvoie son abonnement à chaque
                // démarrage avec un nom générique, qui ne doit pas écraser un nom choisi
                // (`PUT …/{id}` le change).
                existing.user_agent = user_agent.clone();
                existing.last_error = None;
                if let Some(c) = request.categories {
                    existing.categories = c;
                }
                // Le téléphone passe d'un compte à un autre : il reçoit désormais pour le nouveau,
                // avec ses catégories à lui (celles de l'ancienne personne ne le suivent pas).
                if let Some(owner) = owner {
                    if existing.user_id.as_deref().is_some_and(|o| o != owner) {
                        existing.categories = request.categories.unwrap_or_default();
                    }
                    existing.user_id = Some(owner.to_string());
                }
                return Ok(view(existing));
            }
            if s.subscriptions.len() >= PUSH_MAX_SUBSCRIPTIONS {
                return Err(PushError::TooMany);
            }
            let sub = Subscription {
                id: new_id(&s.subscriptions),
                endpoint: endpoint.clone(),
                p256dh: crypto::b64url(&p256dh),
                auth: crypto::b64url(&auth),
                label: label.clone(),
                user_agent: user_agent.clone(),
                created_at: now_unix(),
                last_sent_at: None,
                last_error: None,
                categories: request.categories.unwrap_or_default(),
                user_id: owner.map(str::to_string),
            };
            let v = view(&sub);
            s.subscriptions.push(sub);
            log::info!("notifications : nouvel abonnement « {} »", v.label);
            Ok(v)
        })?
    }

    pub fn update(
        &self,
        id: &str,
        request: PushUpdateRequest,
    ) -> Result<PushSubscriptionView, PushError> {
        self.update_inner(None, id, request)
    }

    /// Comme [`Push::update`], pour un appareil de cette personne seulement (`NotFound` sinon).
    pub fn update_for(
        &self,
        owner: &str,
        id: &str,
        request: PushUpdateRequest,
    ) -> Result<PushSubscriptionView, PushError> {
        self.update_inner(Some(owner), id, request)
    }

    fn update_inner(
        &self,
        owner: Option<&str>,
        id: &str,
        request: PushUpdateRequest,
    ) -> Result<PushSubscriptionView, PushError> {
        self.update_stored(|s| {
            let sub = s
                .subscriptions
                .iter_mut()
                .find(|x| x.id == id && owns(owner, x))
                .ok_or(PushError::NotFound)?;
            if let Some(c) = request.categories {
                sub.categories = c;
            }
            if let Some(label) = &request.label {
                sub.label = clean_label(label);
            }
            Ok(view(sub))
        })?
    }

    /// Révoque un abonnement (l'appareil ne reçoit plus rien).
    pub fn remove(&self, id: &str) -> Result<(), PushError> {
        self.remove_inner(None, id)
    }

    /// Comme [`Push::remove`], pour un appareil de cette personne seulement (`NotFound` sinon).
    pub fn remove_for(&self, owner: &str, id: &str) -> Result<(), PushError> {
        self.remove_inner(Some(owner), id)
    }

    fn remove_inner(&self, owner: Option<&str>, id: &str) -> Result<(), PushError> {
        self.update_stored(|s| {
            let before = s.subscriptions.len();
            s.subscriptions.retain(|x| !(x.id == id && owns(owner, x)));
            if s.subscriptions.len() == before {
                Err(PushError::NotFound)
            } else {
                Ok(())
            }
        })??;
        log::info!("notifications : abonnement révoqué");
        Ok(())
    }

    /// Une notification d'essai vers un seul appareil.
    pub fn send_test(self: &Arc<Self>, id: &str) -> Result<(), PushError> {
        self.send_test_inner(None, id)
    }

    /// Comme [`Push::send_test`], vers un appareil de cette personne seulement.
    pub fn send_test_for(self: &Arc<Self>, owner: &str, id: &str) -> Result<(), PushError> {
        self.send_test_inner(Some(owner), id)
    }

    fn send_test_inner(self: &Arc<Self>, owner: Option<&str>, id: &str) -> Result<(), PushError> {
        if !self.enabled() {
            return Err(PushError::Disabled);
        }
        if !lock(&self.stored)
            .subscriptions
            .iter()
            .any(|x| x.id == id && owns(owner, x))
        {
            return Err(PushError::NotFound);
        }
        self.enqueue(Job {
            target: Some(id.to_string()),
            owner: None,
            category: None,
            payload: content::test_message(),
        });
        Ok(())
    }

    /// Un message pour les appareils qui veulent la catégorie (sans effet si les notifications
    /// sont coupées ou si aucun appareil ne la veut).
    pub fn notify(self: &Arc<Self>, category: Category, payload: PushPayload) {
        {
            let stored = lock(&self.stored);
            if !stored.enabled
                || !stored
                    .subscriptions
                    .iter()
                    .any(|s| category.wanted_by(&s.categories))
            {
                return;
            }
        }
        self.enqueue(Job {
            target: None,
            owner: None,
            category: Some(category),
            payload,
        });
    }

    /// Comme [`Push::notify`], mais **aux appareils de cette personne seulement** (ceux qui veulent
    /// la catégorie) : la demande d'un agent ne part jamais vers le téléphone d'un autre. Un
    /// appareil sans propriétaire (abonné avant les comptes) n'est pas à elle : il ne reçoit rien.
    pub fn notify_user(self: &Arc<Self>, user_id: &str, category: Category, payload: PushPayload) {
        {
            let stored = lock(&self.stored);
            if !stored.enabled
                || !stored.subscriptions.iter().any(|s| {
                    s.user_id.as_deref() == Some(user_id) && category.wanted_by(&s.categories)
                })
            {
                return;
            }
        }
        self.enqueue(Job {
            target: None,
            owner: Some(user_id.to_string()),
            category: Some(category),
            payload,
        });
    }

    fn enqueue(self: &Arc<Self>, job: Job) {
        self.ensure_worker();
        let dropped = {
            let mut queue = lock(&self.shared.queue);
            if queue.closed {
                return;
            }
            let dropped = (queue.jobs.len() >= QUEUE_MAX)
                .then(|| queue.jobs.pop_front())
                .flatten();
            queue.jobs.push_back(job);
            dropped
        };
        self.shared.wake.notify_one();
        if dropped.is_some() {
            log::warn!("notifications : file pleine, le plus ancien message est abandonné");
        }
    }

    // -----------------------------------------------------------------------
    // Détection : un message par passage
    // -----------------------------------------------------------------------

    /// Un passage vient de se terminer. Si sa description est attendue (`awaiting`), le message
    /// part avec elle ([`Push::detection_described`]) ; sinon tout de suite.
    ///
    /// `awaiting` est évalué sous le verrou des passages : la description ne peut pas arriver
    /// entre la question et l'inscription (elle prend le même verrou après avoir quitté
    /// l'attente), il y a toujours exactement un message.
    pub fn detection_closed(
        self: &Arc<Self>,
        event: &DetectionEventView,
        awaiting: impl FnOnce() -> bool,
        clean: &dyn Fn(&str) -> String,
    ) {
        let payload = {
            let mut det = lock(&self.detections);
            if det.sent.contains(&event.id) || det.pending.contains(&event.id) {
                return;
            }
            if awaiting() {
                det.pending.insert(event.id.clone());
                // Borné : une description attendue arrive toujours (ou son échéance).
                if det.pending.len() > SENT_KEPT {
                    log::warn!(
                        "notifications : {} passages attendaient leur description, oubliés",
                        det.pending.len()
                    );
                    det.pending.clear();
                }
                return;
            }
            remember(&mut det, &event.id);
            content::detection(clean, event)
        };
        self.notify(Category::Detections, payload);
    }

    /// La description d'un passage est arrivée (ou ne viendra pas) : le message du passage part
    /// s'il l'attendait.
    pub fn detection_described(
        self: &Arc<Self>,
        event: &DetectionEventView,
        clean: &dyn Fn(&str) -> String,
    ) {
        let payload = {
            let mut det = lock(&self.detections);
            if !det.pending.remove(&event.id) {
                return;
            }
            remember(&mut det, &event.id);
            content::detection(clean, event)
        };
        self.notify(Category::Detections, payload);
    }

    // -----------------------------------------------------------------------
    // Pile faible et appareil injoignable
    // -----------------------------------------------------------------------

    /// Un événement du backend (déjà retenu par le hub). `name` : le nom de l'appareil, déjà
    /// nettoyé (`None` : inconnu).
    pub fn observe(self: &Arc<Self>, event: &Event, name: Option<&str>, retained: bool) {
        let recent = |at: f64| now_unix() - at <= RECENT_S;
        match event {
            Event::Measurement {
                node_id,
                kind: MeasureKind::Battery,
                value,
                at,
            } if retained && recent(*at) => {
                // Le pourcentage fait foi dès qu'il a été vu une fois (retenu sur disque) : le
                // drapeau « pile faible » de l'appareil, qui peut le contredire, n'est plus lu.
                if !lock(&self.stored).battery_percent.contains(node_id) {
                    let _ = self.update_stored(|s| s.battery_percent.insert(*node_id));
                }
                if *value < BATTERY_LOW {
                    self.battery_low(*node_id, name, Some(*value));
                } else if *value >= BATTERY_REARM {
                    self.battery_ok(*node_id);
                }
            }
            Event::State {
                node_id,
                kind: StateKind::BatteryLow,
                value,
                at,
            } if retained && recent(*at) => {
                if lock(&self.stored).battery_percent.contains(node_id) {
                    return;
                }
                if *value {
                    self.battery_low(*node_id, name, None);
                } else {
                    self.battery_ok(*node_id);
                }
            }
            Event::Status { node_id, status } => match status {
                NodeStatus::Unreachable { .. } => {
                    lock(&self.outages)
                        .entry(*node_id)
                        .or_insert_with(|| Outage {
                            since: Instant::now(),
                            name: name.map(str::to_string),
                            alerted: false,
                        });
                }
                NodeStatus::Online => {
                    lock(&self.outages).remove(node_id);
                }
                // « Connexion… » entre deux essais : la panne continue.
                _ => {}
            },
            Event::NodeRemoved(id) => {
                lock(&self.outages).remove(id);
                let _ = self.update_stored(|s| {
                    s.battery_alerted.remove(id);
                    s.battery_percent.remove(id);
                });
            }
            _ => {}
        }
    }

    fn battery_low(self: &Arc<Self>, node_id: u64, name: Option<&str>, percent: Option<f64>) {
        // Personne pour la recevoir (coupé, ou aucun appareil ne veut les piles) : rien n'est
        // retenu, l'alerte partira à la prochaine mesure quand quelqu'un la voudra.
        {
            let stored = lock(&self.stored);
            if !stored.enabled
                || stored.battery_alerted.contains(&node_id)
                || !stored
                    .subscriptions
                    .iter()
                    .any(|s| Category::Battery.wanted_by(&s.categories))
            {
                return;
            }
        }
        match self.update_stored(|s| s.battery_alerted.insert(node_id)) {
            Ok(true) => {}
            Ok(false) => return,
            // Sans écriture possible, on préfère ne rien envoyer qu'envoyer à chaque mesure.
            Err(_) => return,
        }
        self.notify(
            Category::Battery,
            content::battery(&|s| s.to_string(), name, percent),
        );
    }

    fn battery_ok(&self, node_id: u64) {
        // Pas de copie ni d'écriture quand rien ne change (cas courant : à chaque mesure).
        if !lock(&self.stored).battery_alerted.contains(&node_id) {
            return;
        }
        let _ = self.update_stored(|s| s.battery_alerted.remove(&node_id));
    }

    /// Les pannes plus longues que [`UNREACHABLE_AFTER`] (une fois chacune).
    pub(crate) fn check_outages(self: &Arc<Self>, now: Instant, after: Duration) {
        let late: Vec<(Option<String>, u64)> = lock(&self.outages)
            .values_mut()
            .filter(|o| !o.alerted && now.duration_since(o.since) >= after)
            .map(|o| {
                o.alerted = true;
                (o.name.clone(), now.duration_since(o.since).as_secs() / 60)
            })
            .collect();
        for (name, minutes) in late {
            self.notify(
                Category::Unreachable,
                content::unreachable(&|s| s.to_string(), name.as_deref(), minutes),
            );
        }
    }

    // -----------------------------------------------------------------------
    // Le fil d'envoi
    // -----------------------------------------------------------------------

    fn ensure_worker(self: &Arc<Self>) {
        let mut started = lock(&self.worker_started);
        if *started {
            return;
        }
        let (shared, weak) = (self.shared.clone(), Arc::downgrade(self));
        match std::thread::Builder::new()
            .name("notifications".into())
            .spawn(move || worker(&shared, &weak))
        {
            Ok(_) => *started = true,
            Err(e) => log::error!("fil des notifications impossible : {e}"),
        }
    }

    /// Démarre le fil (pour la surveillance des pannes, même sans message à envoyer).
    pub fn start(self: &Arc<Self>) {
        self.ensure_worker();
    }

    /// Arrêt du serveur : les messages en attente sont abandonnés.
    pub fn shutdown(&self) {
        let pending = {
            let mut queue = lock(&self.shared.queue);
            queue.closed = true;
            let n = queue.jobs.len();
            queue.jobs.clear();
            n
        };
        if pending > 0 {
            log::info!("notifications : {pending} message(s) abandonné(s) à l'arrêt");
        }
        self.shared.wake.notify_all();
    }

    fn process(&self, job: &Job) {
        let active = lock(&self.owner_active).clone();
        let targets: Vec<Subscription> = {
            let stored = lock(&self.stored);
            if !stored.enabled {
                return;
            }
            stored
                .subscriptions
                .iter()
                // Une personne désactivée ne reçoit plus rien ; un appareil sans propriétaire
                // (aucun compte à l'époque) reçoit comme avant.
                .filter(|s| match (&active, &s.user_id) {
                    (Some(active), Some(owner)) => active(owner),
                    _ => true,
                })
                .filter(|s| {
                    job.owner
                        .as_deref()
                        .is_none_or(|owner| s.user_id.as_deref() == Some(owner))
                })
                .filter(|s| match (&job.target, job.category) {
                    (Some(id), _) => &s.id == id,
                    (None, Some(c)) => c.wanted_by(&s.categories),
                    (None, None) => false,
                })
                .cloned()
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        let (urgency, ttl) = job.category.map_or(("normal", 600), Category::delivery);
        let plaintext = match serde_json::to_vec(&job.payload) {
            Ok(p) => p,
            Err(_) => return,
        };
        let results: Vec<(String, Delivery)> = targets
            .iter()
            .map(|s| (s.id.clone(), self.deliver(s, &plaintext, urgency, ttl)))
            .collect();
        let now = now_unix();
        let _ = self.update_stored(|stored| {
            for (id, result) in &results {
                match result {
                    Delivery::Gone => {
                        log::info!("notifications : abonnement expiré, oublié");
                        stored.subscriptions.retain(|s| &s.id != id);
                    }
                    Delivery::Sent => {
                        if let Some(s) = stored.subscriptions.iter_mut().find(|s| &s.id == id) {
                            s.last_sent_at = Some(now);
                            s.last_error = None;
                        }
                    }
                    Delivery::Failed(why) => {
                        log::warn!("notifications : envoi impossible : {why}");
                        if let Some(s) = stored.subscriptions.iter_mut().find(|s| &s.id == id) {
                            s.last_error = Some(why.clone());
                        }
                    }
                }
            }
        });
    }

    /// Un envoi à un appareil, avec au plus un nouvel essai (429, 5xx, réseau).
    fn deliver(
        &self,
        sub: &Subscription,
        plaintext: &[u8],
        urgency: &'static str,
        ttl: u64,
    ) -> Delivery {
        if transport::service_of(&sub.endpoint).is_none() {
            return Delivery::Failed("adresse de service de push refusée".into());
        }
        let (Some(p256dh), Some(auth)) = (
            crypto::b64url_decode(&sub.p256dh),
            crypto::b64url_decode(&sub.auth),
        ) else {
            return Delivery::Failed("clés de l'abonnement illisibles".into());
        };
        let body = match crypto::encrypt(&p256dh, &auth, plaintext) {
            Ok(b) => b,
            Err(why) => return Delivery::Failed(why),
        };
        let Some(audience) = transport::origin_of(&sub.endpoint) else {
            return Delivery::Failed("adresse d'abonnement illisible".into());
        };
        let authorization = match self.vapid.authorization(&audience, now_unix() as u64) {
            Ok(a) => a,
            Err(why) => return Delivery::Failed(why),
        };
        let request = PushRequest {
            endpoint: sub.endpoint.clone(),
            authorization,
            ttl_s: ttl,
            urgency,
            body,
        };
        let transport = lock(&self.transport).clone();
        let retry_default = *lock(&self.retry_default);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let (why, wait) = match transport.post(&request) {
                Ok(r) if (200..300).contains(&r.status) => return Delivery::Sent,
                Ok(r) if r.status == 404 || r.status == 410 => return Delivery::Gone,
                Ok(r) if r.status == 429 || r.status >= 500 => (
                    format!("HTTP {}", r.status),
                    r.retry_after.unwrap_or(retry_default),
                ),
                Ok(r) => return Delivery::Failed(format!("HTTP {}", r.status)),
                Err(e) if e.retryable => (e.why, retry_default),
                Err(e) => return Delivery::Failed(e.why),
            };
            if attempt >= 2 {
                return Delivery::Failed(why);
            }
            std::thread::sleep(wait.min(Duration::from_secs(30)));
        }
    }
}

/// L'événement intéresse-t-il les notifications (pile, joignabilité, appareil oublié) ? Les autres
/// (mesures ordinaires, très nombreuses) ne coûtent rien.
pub fn concerns(event: &Event) -> bool {
    matches!(
        event,
        Event::Measurement {
            kind: MeasureKind::Battery,
            ..
        } | Event::State {
            kind: StateKind::BatteryLow,
            ..
        } | Event::Status { .. }
            | Event::NodeRemoved(_)
    )
}

fn remember(det: &mut Detections, id: &str) {
    det.sent.push_back(id.to_string());
    while det.sent.len() > SENT_KEPT {
        det.sent.pop_front();
    }
}

/// `owner` : la personne qui demande (`None` : la fenêtre du serveur, qui voit tout).
fn owns(owner: Option<&str>, sub: &Subscription) -> bool {
    owner.is_none_or(|o| sub.user_id.as_deref() == Some(o))
}

fn view(s: &Subscription) -> PushSubscriptionView {
    PushSubscriptionView {
        id: s.id.clone(),
        label: s.label.clone(),
        user_agent: s.user_agent.clone(),
        service: transport::service_of(&s.endpoint)
            .unwrap_or("inconnu")
            .to_string(),
        created_at: s.created_at,
        last_sent_at: s.last_sent_at,
        last_error: s.last_error.clone(),
        categories: s.categories,
    }
}

fn clean_label(label: &str) -> String {
    let label: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(PUSH_LABEL_MAX)
        .collect();
    if label.is_empty() {
        "Appareil".into()
    } else {
        label
    }
}

fn new_id(existing: &[Subscription]) -> String {
    loop {
        let id = format!("{:016x}", rand::random::<u64>());
        if !existing.iter().any(|s| s.id == id) {
            return id;
        }
    }
}

/// Le fil : un message à la fois, et la vérification des pannes toutes les 30 s.
fn worker(shared: &Shared, push: &Weak<Push>) {
    let mut last_tick = Instant::now();
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break Some(job);
                }
                let (q, timeout) = shared
                    .wake
                    .wait_timeout(queue, TICK)
                    .unwrap_or_else(|e| e.into_inner());
                queue = q;
                if timeout.timed_out() {
                    break None;
                }
            }
        };
        let Some(push) = push.upgrade() else { return };
        if let Some(job) = job {
            push.process(&job);
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            push.check_outages(last_tick, UNREACHABLE_AFTER);
        }
    }
}
