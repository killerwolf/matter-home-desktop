//! Tests des descriptions : un faux serveur local à la place de l'API (jamais la vraie), un hub
//! sans caméra réelle (aucune connexion), et les commandes vers le moteur lues au passage.

use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use protocol::events::Command;
use protocol::wire::{CameraPlacement, DetectedClass};

use super::mock::{jpeg, Mock, Reply};
use super::settings::{CameraFilter, Model, Settings};
use super::*;
use crate::cameras::CameraDraft;

const KEY: &str = "sk-ant-test-0123456789abcdef";

struct Fixture {
    hub: Arc<Hub>,
    rx: UnboundedReceiver<Command>,
    camera: u64,
    dir: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let dir = crate::hub::tests::temp(&format!("descr-{tag}"));
    let (hub, rx) = crate::hub::tests::hub_with(&dir, 64);
    let hub = Arc::new(hub);
    let camera = hub
        .add_camera(&CameraDraft {
            name: "Allée".into(),
            url: "rtsp://127.0.0.1:9/live/ch00_0".into(),
            ..CameraDraft::default()
        })
        .unwrap();
    Fixture {
        hub,
        rx,
        camera,
        dir,
    }
}

impl Fixture {
    fn descriptions(&self) -> &Descriptions {
        self.hub.detection().descriptions()
    }

    /// Descriptions actives, vers ce faux serveur.
    fn enable(&self, mock: &Mock, change: impl FnOnce(&mut Settings)) {
        let d = self.descriptions();
        d.set_endpoint(Endpoint {
            base_url: mock.url(),
            retry_default: Duration::from_millis(20),
            ..Endpoint::default()
        });
        d.set_api_key(Some(KEY)).unwrap();
        let mut settings = Settings {
            enabled: true,
            ..Settings::default()
        };
        change(&mut settings);
        d.update(settings).unwrap();
    }

    fn event(&self, n: u64, in_progress: bool) -> DetectionEventView {
        self.event_on(self.camera, n, in_progress)
    }

    fn event_on(&self, camera: u64, n: u64, in_progress: bool) -> DetectionEventView {
        DetectionEventView {
            id: format!("{}-{camera}", 1_000_000 + n),
            camera_id: camera,
            camera_name: "Allée".into(),
            classes: vec![DetectedClass::Personne, DetectedClass::Chien],
            score: 0.8,
            start: 1000.0 + n as f64,
            end: 1005.0 + n as f64,
            has_clip: false,
            clip_s: 0.0,
            in_progress,
            description: None,
            niveau: None,
        }
    }

    fn save(&self, event: &DetectionEventView) -> DetectionEventView {
        self.hub
            .detection()
            .store()
            .save(event.clone(), Some(&jpeg(64, 48)), None)
            .unwrap()
    }

    /// Les commandes reçues par le moteur jusqu'ici.
    fn commands(&mut self) -> Vec<Command> {
        let mut out = Vec::new();
        while let Ok(c) = self.rx.try_recv() {
            out.push(c);
        }
        out
    }

    /// Attend que `n` événements aient leur résolution (`DetectionDescribed`).
    fn wait_described(&mut self, n: usize) -> Vec<DetectionEventView> {
        let mut found = Vec::new();
        wait("DetectionDescribed", || {
            for c in self.commands() {
                if let Command::DetectionDescribed { event } = c {
                    found.push(*event);
                }
            }
            found.len() >= n
        });
        found
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.descriptions().shutdown(&self.hub);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("délai dépassé : {what}");
}

#[test]
fn a_passage_is_described_once_at_its_end_and_the_waiting_rules_then_start() {
    let mut f = fixture("close");
    let mock = Mock::start(vec![Reply::described(
        "Une personne et un chien dans l'allée.",
        1,
    )]);
    f.enable(&mock, |_| {});
    let image = jpeg(64, 48);
    let mut wire = f.hub.subscribe();

    // L'ouverture : des règles vont devoir attendre ; rien n'est encore envoyé.
    let opened = f.save(&f.event(1, true));
    assert!(f.descriptions().on_open(&f.hub, &opened, &image));
    f.descriptions().describe_opening(&f.hub, &opened, &image);
    assert_eq!(mock.count(), 0, "une seule description, à la fin");

    // La fin du passage : la description part, arrive, est diffusée, puis les règles partent.
    let closed = f.save(&f.event(1, false));
    f.descriptions().on_close(&f.hub, &closed, &image);
    let resolved = f.wait_described(1);
    assert_eq!(mock.count(), 1);
    let event = &resolved[0];
    assert_eq!(event.id, closed.id);
    assert_eq!(
        event.description.as_deref(),
        Some("Une personne et un chien dans l'allée.")
    );
    assert_eq!(event.niveau, Some(1));
    let stored = f.hub.detection().store().get(&closed.id).unwrap();
    assert_eq!(stored.description, event.description);
    assert!(!stored.in_progress);

    // Diffusée aux clients : l'événement porte la description (même identifiant).
    let mut published = Vec::new();
    while let Ok(text) = wire.rx.try_recv() {
        if text.contains("\"detection\"") {
            published.push(text);
        }
    }
    assert_eq!(published.len(), 1, "{published:?}");
    assert!(published[0].contains("\"description\"") && published[0].contains("\"niveau\":1"));

    // Ce qui est parti : la capture et la consigne, rien d'autre (ni adresse, ni identifiant).
    let body = mock.request(0).json();
    let content = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    let sent = body.to_string();
    for secret in ["rtsp", "127.0.0.1", KEY, &closed.id, &f.camera.to_string()] {
        assert!(!sent.contains(secret), "{secret} ne doit pas partir");
    }
    assert!(sent.contains("Allée") && sent.contains("personne, chien"));
    assert_eq!(body["model"], "claude-opus-5-5", "Opus par défaut");
}

#[test]
fn describing_at_the_opening_is_a_setting_and_rules_start_after_the_first_description() {
    let mut f = fixture("open");
    let mock = Mock::start(vec![
        Reply::described("Quelqu'un entre dans l'allée.", 0),
        Reply::described("Un livreur repart avec un colis.", 0),
    ]);
    f.enable(&mock, |s| {
        s.on_open = true;
        s.model = Model::Haiku;
    });
    let image = jpeg(64, 48);
    let opened = f.save(&f.event(2, true));
    assert!(f.descriptions().on_open(&f.hub, &opened, &image));
    f.descriptions().describe_opening(&f.hub, &opened, &image);
    // Les règles partent dès la première description, avec elle.
    let first = f.wait_described(1);
    assert_eq!(
        first[0].description.as_deref(),
        Some("Quelqu'un entre dans l'allée.")
    );
    assert!(first[0].in_progress);
    assert_eq!(mock.request(0).json()["model"], "claude-haiku-5-5");
    // La fin du passage la remplace, sans relancer les règles.
    let closed = f.save(&f.event(2, false));
    assert_eq!(
        closed.description.as_deref(),
        Some("Quelqu'un entre dans l'allée."),
        "la fiche de fin garde celle d'avant"
    );
    f.descriptions().on_close(&f.hub, &closed, &image);
    wait("la seconde description", || {
        f.hub
            .detection()
            .store()
            .get(&closed.id)
            .is_some_and(|e| e.description.as_deref() == Some("Un livreur repart avec un colis."))
    });
    assert_eq!(mock.count(), 2);
    assert!(f.commands().is_empty(), "les règles ne repartent pas");
}

#[test]
fn filters_keep_events_out_without_sending_anything() {
    let mut f = fixture("filters");
    let mock = Mock::start(vec![Reply::described("x", 0)]);
    let image = jpeg(64, 48);
    let outside = f
        .hub
        .add_camera(&CameraDraft {
            name: "Salon".into(),
            url: "rtsp://127.0.0.1:9/live/ch01_0".into(),
            emplacement: CameraPlacement::Interieure,
            ..CameraDraft::default()
        })
        .unwrap();
    let ask = |f: &Fixture, camera: u64, classes: Vec<DetectedClass>| {
        let mut e = f.event_on(camera, 5, true);
        e.classes = classes;
        f.descriptions().on_open(&f.hub, &e, &image)
    };

    // Désactivé, ou sans clé : rien.
    assert!(!ask(&f, f.camera, vec![DetectedClass::Personne]));
    f.descriptions().set_api_key(Some(KEY)).unwrap();
    assert!(
        !ask(&f, f.camera, vec![DetectedClass::Personne]),
        "désactivé"
    );
    f.enable(&mock, |s| s.cameras = CameraFilter::Exterior);
    assert!(ask(&f, f.camera, vec![DetectedClass::Personne]));
    assert!(
        !ask(&f, outside, vec![DetectedClass::Personne]),
        "intérieure"
    );
    f.enable(&mock, |s| s.cameras = CameraFilter::List(vec![outside]));
    assert!(!ask(&f, f.camera, vec![DetectedClass::Personne]));
    assert!(ask(&f, outside, vec![DetectedClass::Personne]));
    f.enable(&mock, |s| s.classes = vec![DetectedClass::Vehicule]);
    assert!(!ask(
        &f,
        f.camera,
        vec![DetectedClass::Personne, DetectedClass::Chat]
    ));
    assert!(ask(&f, f.camera, vec![DetectedClass::Vehicule]));
    // Sans capture : rien à envoyer.
    let e = f.event(6, true);
    assert!(!f.descriptions().on_open(&f.hub, &e, &[]));
    // Un passage dont les réglages ont changé avant la fin : les règles en attente partent quand
    // même, sans description.
    f.descriptions()
        .update(Settings {
            enabled: false,
            ..Settings::default()
        })
        .unwrap();
    let saved = f.save(&f.event_on(outside, 5, false));
    f.descriptions().on_close(&f.hub, &saved, &image);
    let resolved = f.wait_described(1);
    assert!(resolved[0].description.is_none());
    assert_eq!(mock.count(), 0, "rien n'est parti");
}

#[test]
fn the_daily_cap_skips_the_rest_and_the_waiting_rules_still_start() {
    let mut f = fixture("cap");
    let mock = Mock::start(vec![Reply::described("Rien de spécial.", 0)]);
    f.enable(&mock, |s| s.daily_cap = 2);
    let image = jpeg(64, 48);
    for n in 1..=3 {
        let opened = f.save(&f.event(n, true));
        assert!(f.descriptions().on_open(&f.hub, &opened, &image));
        let closed = f.save(&f.event(n, false));
        f.descriptions().on_close(&f.hub, &closed, &image);
        f.wait_described(1);
    }
    assert_eq!(
        mock.count(),
        2,
        "plafond de 2 : la troisième reste sans description"
    );
    let store = f.hub.detection().store();
    let described = (1..=3)
        .filter(|n| {
            store
                .get(&format!("{}-{}", 1_000_000 + n, f.camera))
                .is_some_and(|e| e.description.is_some())
        })
        .count();
    assert_eq!(described, 2);
    // Un seul avertissement par jour.
    assert!(f.descriptions().day.lock().unwrap().warned);
}

#[test]
fn a_refusal_stores_unavailable_and_an_error_stores_nothing() {
    let mut f = fixture("refusal");
    let mock = Mock::start(vec![
        Reply::message("refusal", ""),
        Reply {
            status: 401,
            delay: Duration::ZERO,
            headers: vec![],
            body: r#"{"type":"error","error":{"type":"authentication_error","message":"non"}}"#
                .into(),
        },
    ]);
    f.enable(&mock, |_| {});
    let image = jpeg(64, 48);
    for n in 1..=2 {
        let opened = f.save(&f.event(n, true));
        assert!(f.descriptions().on_open(&f.hub, &opened, &image));
        let closed = f.save(&f.event(n, false));
        f.descriptions().on_close(&f.hub, &closed, &image);
        f.wait_described(1);
    }
    assert_eq!(mock.count(), 2, "ni le refus ni la 401 ne sont réessayés");
    let store = f.hub.detection().store();
    let refused = store.get(&format!("1000001-{}", f.camera)).unwrap();
    assert_eq!(
        refused.description.as_deref(),
        Some("description indisponible")
    );
    assert_eq!(refused.niveau, None);
    let failed = store.get(&format!("1000002-{}", f.camera)).unwrap();
    assert!(failed.description.is_none() && failed.niveau.is_none());
}

#[test]
fn a_full_queue_drops_the_oldest_and_never_blocks_the_caller() {
    let mut f = fixture("queue");
    let mock = Mock::start(vec![
        Reply::described("Lente.", 0).with_delay(Duration::from_millis(1500)),
        Reply::described("Vite.", 0),
    ]);
    f.enable(&mock, |_| {});
    let image = jpeg(64, 48);
    let started = std::time::Instant::now();
    let total = QUEUE_MAX as u64 + 3;
    for n in 1..=total {
        f.save(&f.event(n, true));
        let closed = f.save(&f.event(n, false));
        // Le premier est pris par le fil (bloqué sur le serveur lent) avant les suivants.
        if n == 2 {
            wait("le premier est pris", || {
                lock(&f.descriptions().shared.queue).jobs.is_empty()
            });
        }
        f.descriptions().on_close(&f.hub, &closed, &image);
    }
    assert!(
        started.elapsed() < Duration::from_millis(1200),
        "les événements ne bloquent pas sur le fil des descriptions"
    );
    // Le premier est en cours d'appel, puis la file garde 8 tâches (2 à 9) : à l'arrivée de la 10e et
    // de la 11e, les deux plus anciennes en attente (2 et 3) sont abandonnées. Aucun n'est « attendu » ici
    // (pas d'ouverture) : rien à résoudre, mais la file reste bornée.
    wait("la file se vide", || {
        mock.count() as u64 >= total - 2 && lock(&f.descriptions().shared.queue).jobs.is_empty()
    });
    wait_for_count(&mock, total as usize - 2);
    let store = f.hub.detection().store();
    let has = |n: u64| {
        store
            .get(&format!("{}-{}", 1_000_000 + n, f.camera))
            .is_some_and(|e| e.description.is_some())
    };
    assert!(has(1));
    assert!(
        !has(2) && !has(3),
        "les plus anciens de la file sont partis"
    );
    assert!((4..=total).all(has));
    let _ = f.commands();
}

fn wait_for_count(mock: &Mock, n: usize) {
    wait("les appels", || mock.count() >= n);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(mock.count(), n);
}

#[test]
fn settings_and_key_are_stored_privately_and_the_key_is_never_shown() {
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::hub::tests::temp("descr-file");
    let d = Descriptions::new(&dir, false);
    assert!(!d.has_key() && !d.settings().enabled);
    // Une clé invalide est refusée, sans la répéter.
    let err = d.set_api_key(Some("a b")).unwrap_err().to_string();
    assert!(!err.contains("a b"));
    d.set_api_key(Some(&format!("  {KEY}  "))).unwrap();
    d.update(Settings {
        enabled: true,
        model: Model::Sonnet,
        daily_cap: 50,
        cameras: CameraFilter::Exterior,
        ..Settings::default()
    })
    .unwrap();
    let file = dir.join("serveur").join("descriptions.json");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&dir.join("serveur")), 0o700);
    // Relu au démarrage suivant ; les réglages changent sans toucher la clé, et inversement.
    let again = Descriptions::new(&dir, false);
    assert!(again.has_key());
    assert_eq!(again.settings().model, Model::Sonnet);
    assert_eq!(again.settings().daily_cap, 50);
    assert_eq!(again.settings().cameras, CameraFilter::Exterior);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(KEY) && !text.contains("  sk-"),
        "clé nettoyée"
    );
    again.update(Settings::default()).unwrap();
    assert!(again.has_key());
    again.set_api_key(None).unwrap();
    assert!(!Descriptions::new(&dir, false).has_key());
    // Un fichier illisible : désactivé, rien d'envoyé.
    std::fs::write(&file, b"{pas du json").unwrap();
    let broken = Descriptions::new(&dir, false);
    assert!(!broken.has_key() && !broken.settings().enabled);
    // En démonstration : ni lecture ni écriture.
    let demo_dir = crate::hub::tests::temp("descr-demo");
    let demo = Descriptions::new(&demo_dir, true);
    demo.set_api_key(Some(KEY)).unwrap();
    assert!(!demo_dir.join("serveur").exists());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(demo_dir);
}

#[test]
fn waiting_rules_never_wait_forever_and_start_without_a_description() {
    // Pas de fin de passage (fil d'analyse mort) : l'échéance depuis l'ouverture.
    let mut f = fixture("deadline-open");
    let mock = Mock::start(vec![Reply::described("Jamais lue.", 0)]);
    f.enable(&mock, |_| {});
    f.descriptions()
        .set_limits(Duration::from_secs(30), Duration::from_millis(600));
    let opened = f.save(&f.event(1, true));
    assert!(f.descriptions().on_open(&f.hub, &opened, &jpeg(64, 48)));
    let resolved = f.wait_described(1);
    assert!(resolved[0].description.is_none());
    assert_eq!(mock.count(), 0);

    // L'API est lente : échéance après la fin du passage, la description arrive plus tard.
    let mock = Mock::start(vec![
        Reply::described("Arrivée en retard.", 0).with_delay(Duration::from_millis(2500))
    ]);
    f.enable(&mock, |_| {});
    f.descriptions()
        .set_limits(Duration::from_millis(500), Duration::from_secs(30));
    let opened = f.save(&f.event(2, true));
    assert!(f.descriptions().on_open(&f.hub, &opened, &jpeg(64, 48)));
    let closed = f.save(&f.event(2, false));
    let started = std::time::Instant::now();
    f.descriptions().on_close(&f.hub, &closed, &jpeg(64, 48));
    let resolved = f.wait_described(1);
    assert!(
        started.elapsed() < Duration::from_millis(2200),
        "avant la réponse de l'API"
    );
    assert!(resolved[0].description.is_none());
    wait("la description tardive est enregistrée", || {
        f.hub
            .detection()
            .store()
            .get(&closed.id)
            .is_some_and(|e| e.description.is_some())
    });
    assert!(f.commands().is_empty(), "la règle n'est pas relancée");
}

#[test]
fn a_deleted_event_still_releases_its_waiting_rule_and_shutdown_releases_all() {
    let mut f = fixture("deleted");
    let mock = Mock::start(vec![Reply::described("Trop tard.", 0)]);
    f.enable(&mock, |_| {});
    let opened = f.save(&f.event(1, true));
    assert!(f.descriptions().on_open(&f.hub, &opened, &jpeg(64, 48)));
    let closed = f.save(&f.event(1, false));
    assert!(f.hub.detection().store().delete(&closed.id));
    f.descriptions().on_close(&f.hub, &closed, &jpeg(64, 48));
    let resolved = f.wait_described(1);
    assert_eq!(resolved[0].id, closed.id, "la dernière fiche connue");
    assert!(resolved[0].description.is_none());
    // L'arrêt du serveur : les règles qui attendent partent.
    let opened = f.save(&f.event(2, true));
    assert!(f.descriptions().on_open(&f.hub, &opened, &jpeg(64, 48)));
    f.descriptions().shutdown(&f.hub);
    assert_eq!(f.wait_described(1)[0].id, opened.id);
}

#[test]
fn a_class_that_makes_the_passage_eligible_makes_rules_wait() {
    let f = fixture("update");
    let mock = Mock::start(vec![Reply::described("x", 0)]);
    f.enable(&mock, |s| s.classes = vec![DetectedClass::Personne]);
    let image = jpeg(64, 48);
    let mut dog = f.event(1, true);
    dog.classes = vec![DetectedClass::Chien];
    assert!(
        !f.descriptions().on_open(&f.hub, &dog, &image),
        "un chien : rien"
    );
    assert!(!f.descriptions().on_update(&f.hub, &dog, &image));
    dog.classes = vec![DetectedClass::Chien, DetectedClass::Personne];
    assert!(
        f.descriptions().on_update(&f.hub, &dog, &image),
        "une personne apparaît"
    );
    assert!(f.descriptions().is_awaiting_for_test(&dog.id));
}

#[test]
fn the_daily_count_survives_a_restart_and_old_bodies_are_cleaned() {
    let dir = crate::hub::tests::temp("descr-usage");
    let d = Descriptions::new(&dir, false);
    assert!(d.take_slot(5) && d.take_slot(5) && d.take_slot(5));
    let usage = dir.join("serveur").join("descriptions-usage.json");
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&usage).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let again = Descriptions::new(&dir, false);
    assert!(again.take_slot(4), "4e");
    assert!(
        !again.take_slot(4),
        "plafond de 4 atteint après redémarrage"
    );
    // Le compte d'un autre jour est ignoré.
    std::fs::write(&usage, r#"{"date":"2000-01-01","count":99}"#).unwrap();
    assert!(Descriptions::new(&dir, false).take_slot(1));
    // Corps de requête oubliés : le vieux part, le récent reste.
    let old = std::env::temp_dir().join(format!("mh-description-test-old-{}", std::process::id()));
    let fresh =
        std::env::temp_dir().join(format!("mh-description-test-new-{}", std::process::id()));
    std::fs::write(&old, b"x").unwrap();
    std::fs::write(&fresh, b"x").unwrap();
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    let _ = Descriptions::new(&dir, false);
    assert!(!old.exists() && fresh.exists());
    let _ = std::fs::remove_file(fresh);
    let _ = std::fs::remove_dir_all(dir);
}
