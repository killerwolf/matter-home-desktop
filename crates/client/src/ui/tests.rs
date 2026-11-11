//! L'interface sans fenêtre : événements du serveur (fabriqués à la main) → état affiché.
//! Le pendant côté réseau, contre le vrai serveur, est dans `tests/connection.rs`.

use super::*;
use matter_home_client::net::Store;
use protocol::events::{BridgeLink, Capabilities, CoverMotion};
use protocol::wire::{
    DeviceView, ErrorCode, HistoryPoint, LatestReading, Metric, RuleRunView, WireBridge, WireNode,
};

fn node(node_id: u64, name: &str, caps: Capabilities) -> WireNode {
    WireNode {
        node_id,
        name: name.into(),
        device_name: name.into(),
        custom_name: None,
        icon: None,
        vendor: None,
        product: None,
        device_types: vec![],
        caps,
        bridge: None,
    }
}

fn view(node: WireNode, latest: Vec<LatestReading>) -> DeviceView {
    DeviceView {
        device: node,
        status: WireStatus::Online,
        last_seen: Some(10.0),
        mode: None,
        latest,
        open_since: None,
        cover_motion: None,
    }
}

fn reading(metric: Metric, value: f64, at: f64) -> LatestReading {
    LatestReading { metric, value, at }
}

fn snapshot(seq: u64, devices: Vec<DeviceView>) -> Snapshot {
    Snapshot {
        seq,
        server_time: 100.0,
        devices,
        plan: TextIds(HomePlan::default()),
        rules: TextIds(protocol::automation::RuleSet::default()),
        rule_runs: vec![],
        pairing_busy: false,
        cameras: vec![],
    }
}

struct Fixture {
    app: MatterApp,
    tx: mpsc::Sender<Event>,
}

impl Fixture {
    fn new() -> Fixture {
        // Le moteur réseau n'a aucune session (fichier de connexion inexistant, dossier
        // temporaire) : il ne contacte personne. Les événements viennent de `tx`.
        let path = std::env::temp_dir()
            .join(format!(
                "mhd-ui-test-{}-{}",
                std::process::id(),
                now_unix() as u64
            ))
            .join("connexion.json");
        let (backend, _backend_events) =
            Backend::start(Store::at(path), std::sync::Arc::new(|| {}));
        let (tx, events) = mpsc::channel();
        let mut app = MatterApp::new(backend, events);
        // Jamais de vraie notification du système pendant les tests.
        app.notifier = Box::new(|_, _| {});
        Fixture { app, tx }
    }

    fn send(&self, event: Event) {
        self.tx.send(event).unwrap();
    }

    fn live(&mut self, event: WireEvent) {
        self.send(Event::Live(Box::new(event)));
        self.app.pump_network();
    }

    fn connect(&mut self, snapshot: Snapshot) {
        self.send(Event::Link(Link::Connected {
            server_version: "0.1.0".into(),
            server: "https://127.0.0.1:8443#AA".into(),
        }));
        self.send(Event::Snapshot(Box::new(snapshot)));
        self.app.pump_network();
    }

    fn pending_ids(&self) -> Vec<u64> {
        self.app.pending.keys().copied().collect()
    }
}

/// Sans serveur : l'interface est « déconnectée » et n'envoie aucune commande.
#[test]
fn starts_disconnected_and_refuses_commands() {
    let mut f = Fixture::new();
    assert!(!f.app.connected());
    f.app.retry_device(1);
    f.app.pair("12345678901".into());
    f.app.test_rule(1);
    assert!(f.app.pending.is_empty(), "aucune requête en vol");
    assert!(matches!(f.app.pairing, Pairing::Idle));
    assert!(f.app.log.back().is_some_and(|l| l.contains("Déconnecté")));
    assert!(f.app.model.devices.is_empty());
}

/// État complet puis événements → liste, résumés, appairage ; le tout vient du protocole.
#[test]
fn snapshot_then_events_drive_the_interface_state() {
    let mut f = Fixture::new();
    let bridge_id = 5;
    let shutter_id = u64::MAX - 0xFF;
    let plug_id = 4;
    let plug_caps = Capabilities {
        power: true,
        energy: true,
        on_off: true,
        ..Default::default()
    };
    let cover = Capabilities {
        cover: true,
        cover_position: true,
        ..Default::default()
    };
    let mut shutter = node(shutter_id, "Volet", cover);
    shutter.bridge = Some(WireBridge {
        node_id: bridge_id,
        endpoint: 2,
    });
    f.connect(snapshot(
        10,
        vec![
            view(
                node(
                    1,
                    "Salon",
                    Capabilities {
                        temperature: true,
                        humidity: true,
                        battery: true,
                        ..Default::default()
                    },
                ),
                vec![
                    reading(Metric::Temperature, 21.0, 90.0),
                    reading(Metric::Battery, 87.0, 90.0),
                    reading(Metric::BatteryLow, 1.0, 90.0),
                ],
            ),
            view(
                node(
                    3,
                    "Porte",
                    Capabilities {
                        contact: true,
                        ..Default::default()
                    },
                ),
                vec![reading(Metric::Open, 0.0, 90.0)],
            ),
            view(
                node(plug_id, "GRILLPLATS Plug", plug_caps),
                vec![
                    reading(Metric::Power, 85.0, 90.0),
                    reading(Metric::OnOff, 1.0, 90.0),
                ],
            ),
            view(node(bridge_id, "TaHoma", Capabilities::default()), vec![]),
            view(shutter, vec![reading(Metric::Position, 100.0, 90.0)]),
        ],
    ));
    assert!(f.app.connected());
    assert_eq!(f.app.model.devices.len(), 5);
    assert_eq!(
        f.app.selected, None,
        "le panneau de détail ne s'ouvre pas tout seul"
    );
    assert!(f.app.model.devices[&1]
        .summary()
        .contains("pile 87 % (faible)"));
    assert_eq!(f.app.model.devices[&shutter_id].summary(), "Ouvert");
    assert!(f.app.model.devices[&plug_id].summary().contains(" W"));

    // Mesures en direct : retenues si récentes, ignorées si plus anciennes que la valeur connue.
    f.live(WireEvent::Reading {
        node_id: 1,
        metric: Metric::Temperature,
        value: 22.5,
        at: 95.0,
    });
    f.live(WireEvent::Reading {
        node_id: 1,
        metric: Metric::Temperature,
        value: 10.0,
        at: 50.0,
    });
    assert_eq!(f.app.model.devices[&1].temperature().unwrap().value, 22.5);

    // Un volet se ferme.
    f.live(WireEvent::CoverMotion {
        node_id: shutter_id,
        motion: CoverMotion::Closing,
    });
    f.live(WireEvent::Reading {
        node_id: shutter_id,
        metric: Metric::Position,
        value: 0.0,
        at: 96.0,
    });
    f.live(WireEvent::CoverMotion {
        node_id: shutter_id,
        motion: CoverMotion::Stopped,
    });
    assert_eq!(f.app.model.devices[&shutter_id].summary(), "Fermé");

    // Appairage : progression, échec traduit, réussite, puis oubli.
    f.live(WireEvent::PairingProgress {
        message: "Recherche…".into(),
    });
    assert!(matches!(f.app.pairing, Pairing::InProgress(_)));
    f.live(WireEvent::PairingFailed {
        code: ErrorCode::PairingFailed,
        detail: Some("code invalide".into()),
    });
    match &f.app.pairing {
        Pairing::Failed(m) => assert_eq!(m, "L'appairage a échoué (code invalide)"),
        _ => panic!("échec attendu"),
    }
    f.live(WireEvent::PairingDone {
        node_id: 9,
        name: "Nouveau".into(),
    });
    f.live(WireEvent::DeviceUpdated {
        device: node(9, "Nouveau", Capabilities::default()),
    });
    assert!(matches!(f.app.pairing, Pairing::Success(_)));
    assert_eq!(f.app.selected, None, "l'appairage n'ouvre pas le détail");
    assert_eq!(f.app.toasts.len(), 2, "échec puis réussite, annoncés");
    assert_eq!(f.app.model.devices.len(), 6);
    f.live(WireEvent::DeviceRemoved { node_id: 9 });
    assert_eq!(f.app.model.devices.len(), 5);
    assert_eq!(f.app.selected, None);

    // Nom et icône choisis : le serveur renvoie la fiche, les dernières valeurs restent.
    let mut renamed = node(plug_id, "GRILLPLATS Plug", plug_caps);
    renamed.name = "Lampe du salon".into();
    renamed.custom_name = Some("Lampe du salon".into());
    renamed.icon = Some(Symbol::Light);
    f.live(WireEvent::DeviceUpdated { device: renamed });
    let p = &f.app.model.devices[&plug_id];
    assert_eq!(p.info.name, "Lampe du salon");
    assert_eq!(p.info.device_name, "GRILLPLATS Plug");
    assert_eq!(plan::icon_of(p), Some(Symbol::Light));
    assert_eq!(p.power().unwrap().value, 85.0);

    // Journal des règles : l'exécution arrive en événement.
    f.live(WireEvent::RuleRun(RuleRunView {
        rule_id: 1,
        name: "Porte ouverte".into(),
        at: 97.0,
        ok: true,
        detail: "ok".into(),
    }));
    assert_eq!(f.app.model.rule_runs.len(), 1);
    assert!(f
        .app
        .log
        .back()
        .is_some_and(|l| l.contains("Porte ouverte") && l.contains("exécutée")));
}

#[test]
fn losing_the_link_wipes_every_piece_of_data_and_blocks_commands() {
    let mut f = Fixture::new();
    f.connect(snapshot(
        1,
        vec![view(
            node(
                1,
                "Salon",
                Capabilities {
                    temperature: true,
                    ..Default::default()
                },
            ),
            vec![reading(Metric::Temperature, 21.0, 90.0)],
        )],
    ));
    // Une série est en mémoire, une requête en vol.
    f.app.series.entry(1).or_default().load(0.0, false, &[]);
    f.app.maintain_series();
    f.app.confirm_clear = Some(1);
    assert!(f.app.series.contains_key(&1));

    f.send(Event::Link(Link::Reconnecting {
        attempt: 1,
        retry_in_s: 1,
        reason: "connexion coupée".into(),
    }));
    f.app.pump_network();
    assert!(!f.app.connected());
    assert!(f.app.model.devices.is_empty(), "plus aucune donnée périmée");
    assert!(f.app.series.is_empty(), "les séries sont vidées");
    assert!(f.app.pending.is_empty());
    assert!(f.app.confirm_clear.is_none());
    f.app.retry_device(1);
    f.app.set_cover(1, CoverCommand::Open);
    assert!(
        f.app.pending.is_empty(),
        "aucune commande sur des données périmées"
    );

    // Une réponse tardive à une requête d'avant la coupure est ignorée sans dégât.
    f.send(Event::Reply {
        id: 1,
        result: Ok(Response {
            status: 202,
            headers: vec![],
            body: vec![],
        }),
    });
    f.app.pump_network();
    assert!(f.app.model.devices.is_empty());

    // Retour : l'état complet est celui du serveur, rien d'ancien ne subsiste.
    f.connect(snapshot(
        7,
        vec![view(node(2, "Cuisine", Capabilities::default()), vec![])],
    ));
    assert_eq!(
        f.app.model.devices.keys().copied().collect::<Vec<_>>(),
        vec![2]
    );
}

#[test]
fn a_resync_keeps_the_display_but_asks_for_fresh_series() {
    let mut f = Fixture::new();
    let caps = Capabilities {
        temperature: true,
        ..Default::default()
    };
    f.connect(snapshot(1, vec![view(node(1, "Salon", caps), vec![])]));
    f.app.series.entry(1).or_default().load(5.0, false, &[]);
    f.send(Event::Link(Link::Resyncing));
    f.app.pump_network();
    assert!(
        f.app.connected(),
        "pas de « déconnecté » pour une simple resynchronisation"
    );
    assert_eq!(f.app.model.devices.len(), 1);
    f.send(Event::Snapshot(Box::new(snapshot(
        9,
        vec![view(node(1, "Salon", caps), vec![])],
    ))));
    f.app.pump_network();
    assert_eq!(f.app.series[&1].fetched_since, None, "à relire");
}

#[test]
fn history_is_read_on_demand_filled_live_and_errors_are_shown() {
    let mut f = Fixture::new();
    let caps = Capabilities {
        temperature: true,
        humidity: true,
        ..Default::default()
    };
    f.connect(snapshot(1, vec![view(node(1, "Salon", caps), vec![])]));
    // Le plan est l'onglet d'ouverture : l'historique se lit quand l'onglet Appareils est ouvert.
    f.app.view = View::Devices;
    f.app.selected = Some(1); // un clic sur la ligne
                              // Aucune lecture avant qu'une fiche ne la demande.
    assert!(f.app.series.is_empty());
    f.app.maintain_series();
    assert_eq!(f.pending_ids().len(), 1, "une lecture, pas plus");
    f.app.maintain_series();
    assert_eq!(
        f.pending_ids().len(),
        1,
        "pas de seconde lecture en même temps"
    );
    let id = f.pending_ids()[0];
    assert_eq!(f.app.series[&1].loading, Some(id));

    // Une mesure en direct pendant la lecture n'est pas ajoutée (la lecture la contiendra).
    f.live(WireEvent::Reading {
        node_id: 1,
        metric: Metric::Temperature,
        value: 20.0,
        at: 100.0,
    });
    assert_eq!(f.app.series[&1].len(Metric::Temperature), 0);

    let body = serde_json::to_vec(&HistoryResponse {
        node_id: 1,
        since: 10.0,
        until: 120.0,
        truncated: true,
        points: vec![
            HistoryPoint {
                at: 50.0,
                metric: Metric::Temperature,
                value: 19.0,
            },
            HistoryPoint {
                at: 100.0,
                metric: Metric::Temperature,
                value: 20.0,
            },
        ],
    })
    .unwrap();
    f.send(Event::Reply {
        id,
        result: Ok(Response {
            status: 200,
            headers: vec![],
            body,
        }),
    });
    f.app.pump_network();
    let s = &f.app.series[&1];
    assert_eq!(
        (s.len(Metric::Temperature), s.truncated, s.loading),
        (2, true, None)
    );

    // Ensuite les mesures en direct complètent la série ; une plus ancienne est ignorée.
    for (value, at) in [(21.0, 110.0), (5.0, 60.0)] {
        f.live(WireEvent::Reading {
            node_id: 1,
            metric: Metric::Temperature,
            value,
            at,
        });
    }
    assert_eq!(f.app.series[&1].len(Metric::Temperature), 3);

    // Échec de lecture : message en français, pas de relecture en boucle.
    f.app.series.get_mut(&1).unwrap().fetched_since = None;
    f.app.maintain_series();
    let id = f.pending_ids()[0];
    f.send(Event::Reply {
        id,
        result: Err(NetError::Api {
            status: 404,
            code: Some(ErrorCode::NotFound),
            detail: None,
            retry_after_s: None,
        }),
    });
    f.app.pump_network();
    assert!(f.app.series[&1]
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with("Introuvable")));
    f.app.maintain_series();
    assert!(
        f.pending_ids().is_empty(),
        "pas de relecture automatique après un échec"
    );
}

#[test]
fn command_failures_are_translated_and_shown() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.pair("00000000000".into());
    assert!(matches!(f.app.pairing, Pairing::InProgress(_)));
    let id = f.pending_ids()[0];
    f.send(Event::Reply {
        id,
        result: Err(NetError::Api {
            status: 409,
            code: Some(ErrorCode::PairingBusy),
            detail: None,
            retry_after_s: None,
        }),
    });
    f.app.pump_network();
    match &f.app.pairing {
        Pairing::Failed(m) => assert_eq!(m, "Un appairage est déjà en cours."),
        _ => panic!(),
    }

    f.app.retry_device(42);
    let id = f.pending_ids()[0];
    f.send(Event::Reply {
        id,
        result: Err(NetError::Timeout),
    });
    f.app.pump_network();
    let banner = f.app.error_banner.as_ref().unwrap();
    assert!(banner.1.contains("Reconnexion de l'appareil") && banner.1.contains("pas répondu"));
}

#[test]
fn other_clients_changes_arrive_without_overwriting_an_edit() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    let renamed = |name: &str| {
        let mut p = HomePlan::default();
        p.floors[0].name = name.into();
        p
    };
    // Rien en cours d'édition : la version d'un autre client est adoptée.
    f.live(WireEvent::PlanChanged {
        plan: TextIds(renamed("Vu d'ailleurs")),
    });
    assert_eq!(f.app.plan.show_name(), "Vu d'ailleurs");

    // Édition locale en cours : la version d'ailleurs est retenue à part, rien n'est écrasé.
    f.app.plan.edit_name("Mon édition");
    f.live(WireEvent::PlanChanged {
        plan: TextIds(renamed("Encore ailleurs")),
    });
    assert_eq!(f.app.plan.show_name(), "Mon édition");
    assert!(f.app.plan.has_remote_banner());

    // Tant que l'utilisateur n'a pas choisi, son édition ne part pas écraser l'autre version.
    let later = Instant::now() + Duration::from_secs(5);
    assert!(f.app.plan.poll_save(later, false).is_none());
    f.app.plan.keep_mine();
    // Les modifications sont envoyées une à la fois, après un délai de calme.
    assert!(f.app.plan.poll_save(later, false).is_some());
    f.app.save_documents(later, false);
    assert!(f
        .app
        .pending
        .values()
        .any(|p| matches!(p, Pending::PlanSave)));
    assert!(
        f.app.plan.poll_save(later, false).is_none(),
        "un seul envoi en vol"
    );
}

#[test]
fn server_notifications_are_shown_in_the_interface() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.live(WireEvent::Notification {
        title: "Porte".into(),
        message: "La porte est ouverte".into(),
        at: 1.0,
    });
    assert_eq!(f.app.toasts.len(), 1);
    assert!(f
        .app
        .log
        .back()
        .is_some_and(|l| l.contains("La porte est ouverte")));
    // Elles disparaissent d'elles-mêmes.
    f.app.toasts[0].until = Instant::now() - Duration::from_secs(1);
    let ctx = egui::Context::default();
    f.app.toasts_ui(&ctx);
    assert!(f.app.toasts.is_empty());
}

#[test]
fn backup_summary_reads_naturally() {
    let s = ImportSummary {
        created: "2026-11-09T10:00:00+01:00".into(),
        devices: 12,
        rooms: 7,
        rules: 1,
        cameras: 0,
        history_files: 12,
        has_plan: true,
        has_rules: true,
        disabled_rules: Vec::new(),
    };
    let text = describe_backup(&s);
    assert!(
        text.starts_with("12 appareils, 7 pièces, 1 règle, sauvegarde du "),
        "{text}"
    );
    assert!(!text.contains("désactivée"), "{text}");
    // Les caméras sont comptées quand il y en a.
    let with_cameras = ImportSummary {
        cameras: 2,
        ..s.clone()
    };
    assert!(
        describe_backup(&with_cameras)
            .starts_with("12 appareils, 7 pièces, 1 règle, 2 caméras, sauvegarde du "),
        "{}",
        describe_backup(&with_cameras)
    );
    // Les brouillons désactivés par l'import sont nommés.
    let with_drafts = ImportSummary {
        disabled_rules: vec!["Alerte porte".into(), "Test".into()],
        ..s
    };
    let text = describe_backup(&with_drafts);
    assert!(
        text.contains("2 règles désactivée(s)") && text.contains("« Alerte porte », « Test »"),
        "{text}"
    );
}

#[test]
fn files_from_the_server_are_written_privately_when_they_hold_keys() {
    let dir = std::env::temp_dir().join(format!("mhd-ui-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sauvegarde.matterhome");
    // Un fichier existant aux droits larges est resserré.
    std::fs::write(&path, "ancien").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    matter_home_client::net::files::write_file(&path, b"{}", true).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ago_formatting() {
    assert_eq!(ago(5.4), "il y a 5 s");
    assert_eq!(ago(125.0), "il y a 2 min");
    assert_eq!(ago(7300.0), "il y a 2 h");
    assert_eq!(ago(-3.0), "il y a 0 s");
}

#[test]
fn bridge_link_type_is_the_wire_one() {
    // Le client n'a plus de double état : le pont vient tel quel du protocole.
    let link = WireBridge {
        node_id: 1,
        endpoint: 2,
    };
    let _: Option<WireBridge> = Some(link);
    let _ = BridgeLink {
        node_id: 1,
        endpoint: 2,
    };
}

// ---------------------------------------------------------------------------
// Brouillons, coupures, essais : plan et règles
// ---------------------------------------------------------------------------

fn api_error(status: u16, code: ErrorCode) -> NetError {
    NetError::Api {
        status,
        code: Some(code),
        detail: None,
        retry_after_s: None,
    }
}

fn ok_reply(id: u64, body: Vec<u8>) -> Event {
    Event::Reply {
        id,
        result: Ok(Response {
            status: 200,
            headers: vec![],
            body,
        }),
    }
}

fn err_reply(id: u64, error: NetError) -> Event {
    Event::Reply {
        id,
        result: Err(error),
    }
}

fn pending_of(f: &Fixture, wanted: impl Fn(&Pending) -> bool) -> Option<u64> {
    f.app
        .pending
        .iter()
        .find(|(_, p)| wanted(p))
        .map(|(id, _)| *id)
}

fn disconnect(f: &mut Fixture) {
    f.send(Event::Link(Link::Reconnecting {
        attempt: 1,
        retry_in_s: 1,
        reason: "connexion coupée".into(),
    }));
    f.app.pump_network();
}

/// Un webhook sans adresse n'est pas envoyé : le serveur répondrait 400 et l'action
/// disparaîtrait avec toute l'édition. Le brouillon reste visible, puis part une fois complet.
#[test]
fn a_rule_with_an_empty_webhook_stays_a_visible_draft_until_it_is_complete() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("");
    let later = Instant::now() + Duration::from_secs(5);
    assert!(f.app.rules.poll_save(later, false).is_none());
    f.app.save_documents(later, false);
    assert!(pending_of(&f, |p| matches!(p, Pending::RulesSave)).is_none());
    let notice = f.app.rules.notice().expect("brouillon visible");
    assert!(
        notice.contains("Brouillon non envoyé") && notice.contains("adresse"),
        "{notice}"
    );
    assert_eq!(f.app.rules.rule_count(), 1);
    // À moitié saisie, ce n'est toujours pas une adresse.
    f.app.rules.set_webhook_url("htt");
    assert!(f.app.rules.poll_save(later, false).is_none());
    // Complète, elle part.
    f.app.rules.set_webhook_url("https://exemple.fr/hook");
    assert!(f.app.rules.notice().is_none());
    f.app.save_documents(later, false);
    assert!(pending_of(&f, |p| matches!(p, Pending::RulesSave)).is_some());
}

/// Un 400 garde le brouillon et dit pourquoi : ni retour en arrière, ni renvoi en boucle.
#[test]
fn a_400_keeps_the_draft_shows_the_error_and_is_not_resent_in_a_loop() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("https://exemple.fr/a");
    let later = Instant::now() + Duration::from_secs(5);
    f.app.save_documents(later, false);
    let id = pending_of(&f, |p| matches!(p, Pending::RulesSave)).unwrap();
    f.send(err_reply(id, api_error(400, ErrorCode::BadRequest)));
    f.app.pump_network();
    assert_eq!(f.app.rules.rule_count(), 1, "le brouillon n'est pas écrasé");
    assert!(f.app.rules.editing());
    assert!(f.app.rules.notice().unwrap().contains("refusé"));
    let banner = f.app.error_banner.as_ref().expect("erreur affichée");
    assert!(banner.1.contains("brouillon gardé"), "{}", banner.1);
    // Pas de boucle de 400.
    f.app.save_documents(later + Duration::from_secs(60), false);
    assert!(pending_of(&f, |p| matches!(p, Pending::RulesSave)).is_none());
    // Une nouvelle modification relance l'envoi.
    f.app.rules.set_webhook_url("https://exemple.fr/b");
    f.app
        .save_documents(later + Duration::from_secs(120), false);
    assert!(pending_of(&f, |p| matches!(p, Pending::RulesSave)).is_some());
}

/// Délai dépassé ou 503 : le document reste à envoyer ; un refus définitif (4xx) revient en arrière.
#[test]
fn only_a_4xx_rolls_back_a_timeout_or_503_keeps_the_document_dirty() {
    let renamed = |f: &mut Fixture| {
        f.app.plan.edit_name("Mon édition");
        let later = Instant::now() + Duration::from_secs(5);
        f.app.save_documents(later, false);
        (
            pending_of(f, |p| matches!(p, Pending::PlanSave)).unwrap(),
            later,
        )
    };
    for transient in [
        NetError::Timeout,
        api_error(503, ErrorCode::Unavailable),
        NetError::Unreachable("coupée".into()),
    ] {
        let mut f = Fixture::new();
        f.connect(snapshot(1, vec![]));
        let (id, later) = renamed(&mut f);
        f.send(err_reply(id, transient));
        f.app.pump_network();
        assert_eq!(
            f.app.plan.show_name(),
            "Mon édition",
            "pas de retour en arrière"
        );
        assert!(f.app.plan.editing());
        let soon = Instant::now() + Duration::from_secs(1);
        assert!(
            f.app.plan.poll_save(soon, false).is_none(),
            "pas tout de suite"
        );
        assert!(
            f.app
                .plan
                .poll_save(later + Duration::from_secs(10), false)
                .is_some(),
            "nouvel essai plus tard"
        );
    }
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    let (id, _) = renamed(&mut f);
    f.send(err_reply(id, api_error(404, ErrorCode::NotFound)));
    f.app.pump_network();
    assert_eq!(
        f.app.plan.show_name(),
        "Rez-de-chaussée",
        "refus définitif : retour au serveur"
    );
    assert!(!f.app.plan.editing());
}

/// Un clic d'onglet pendant le délai de calme ne doit pas laisser « souris enfoncée » en mémoire.
#[test]
fn holding_is_read_at_each_pass_not_remembered_from_the_last_drawing() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Mon édition");
    let later = Instant::now() + Duration::from_secs(5);
    let ctx = egui::Context::default();
    let pass = |f: &mut Fixture, pressed: Option<bool>| {
        let mut input = egui::RawInput::default();
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerButton {
                pos: egui::pos2(5.0, 5.0),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        ctx.begin_pass(input);
        f.app.tick(&ctx, later);
        ctx.end_pass().textures_delta.clear();
    };
    pass(&mut f, Some(true));
    assert!(
        pending_of(&f, |p| matches!(p, Pending::PlanSave)).is_none(),
        "glisser en cours : rien n'est envoyé"
    );
    pass(&mut f, Some(false));
    assert!(
        pending_of(&f, |p| matches!(p, Pending::PlanSave)).is_some(),
        "relâché : l'envoi part, sans autre dessin du plan"
    );
}

/// Une coupure ne perd pas l'édition : elle repart à la reprise si le serveur n'a pas changé.
#[test]
fn an_edit_survives_a_reconnection_and_is_sent_on_resume() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Mon édition");
    disconnect(&mut f);
    assert_eq!(f.app.plan.show_name(), "Mon édition", "brouillon gardé");
    assert!(f.app.model.devices.is_empty());
    let later = Instant::now() + Duration::from_secs(5);
    assert!(
        f.app.plan.poll_save(later, false).is_none(),
        "rien avant la reprise"
    );
    f.connect(snapshot(9, vec![]));
    assert_eq!(f.app.plan.show_name(), "Mon édition");
    assert!(!f.app.plan.has_remote_banner());
    f.app.save_documents(later, false);
    assert!(pending_of(&f, |p| matches!(p, Pending::PlanSave)).is_some());
}

#[test]
fn a_server_that_changed_during_the_outage_warns_instead_of_being_overwritten() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Mon édition");
    disconnect(&mut f);
    let mut other = snapshot(9, vec![]);
    other.plan.0.floors[0].name = "Modifié ailleurs".into();
    f.connect(other);
    assert_eq!(f.app.plan.show_name(), "Mon édition");
    assert!(f.app.plan.has_remote_banner(), "l'utilisateur est prévenu");
    f.app
        .save_documents(Instant::now() + Duration::from_secs(5), false);
    assert!(pending_of(&f, |p| matches!(p, Pending::PlanSave)).is_none());
}

/// Déconnexion volontaire, certificat changé, autre serveur : jamais de brouillon vers un autre serveur.
#[test]
fn a_draft_is_dropped_when_the_session_ends_or_the_server_is_not_the_same() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Mon édition");
    f.send(Event::Link(Link::LoggedOut {
        url: None,
        pinned: false,
        reason: None,
    }));
    f.app.pump_network();
    assert!(!f.app.plan.editing());

    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("https://exemple.fr/a");
    disconnect(&mut f);
    assert!(f.app.rules.editing());
    // La liaison revient, mais vers un autre serveur.
    f.send(Event::Link(Link::Connected {
        server_version: "0.1.0".into(),
        server: "https://127.0.0.1:9999#BB".into(),
    }));
    f.send(Event::Snapshot(Box::new(snapshot(1, vec![]))));
    f.app.pump_network();
    assert!(
        !f.app.rules.editing(),
        "le brouillon de l'autre serveur est abandonné"
    );
    assert_eq!(f.app.rules.rule_count(), 0);
}

#[test]
fn closing_the_window_sends_what_is_pending_and_never_blocks() {
    // Rien ne répond : l'attente est bornée.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Dernier mot");
    let started = Instant::now();
    f.app.flush_on_exit(Duration::from_millis(150));
    assert!(started.elapsed() >= Duration::from_millis(140));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        pending_of(&f, |p| matches!(p, Pending::PlanSave)).is_some(),
        "l'envoi est parti sans attendre le délai de calme"
    );

    // Le serveur répond vite : on repart aussitôt, l'édition est enregistrée.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Dernier mot");
    let mut stored = HomePlan::default();
    stored.floors[0].name = "Dernier mot".into();
    let body = serde_json::to_vec(&TextIds(stored)).unwrap();
    let tx = f.tx.clone();
    // Le moteur numérote ses requêtes à partir de 1 : c'est la première.
    let answer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        tx.send(ok_reply(1, body)).unwrap();
    });
    let started = Instant::now();
    f.app.flush_on_exit(Duration::from_secs(5));
    answer.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "reparti dès la réponse"
    );
    assert!(!f.app.plan.editing());
}

/// Un brouillon incomplet, un autre client qui a modifié : rien n'est envoyé à la fermeture.
#[test]
fn closing_the_window_never_sends_an_incomplete_draft() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("");
    f.app.flush_on_exit(Duration::from_millis(50));
    assert!(f.app.pending.is_empty());
}

/// « Tester » juste après une modification attend que la dernière version soit enregistrée.
#[test]
fn testing_a_rule_waits_for_the_latest_version_to_be_saved() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("https://exemple.fr/a");
    f.app.test_rule(1);
    let save = pending_of(&f, |p| matches!(p, Pending::RulesSave)).expect("envoi immédiat");
    assert!(
        pending_of(&f, |p| matches!(p, Pending::Command { .. })).is_none(),
        "le test n'a pas encore le droit de partir"
    );
    // Le serveur a gardé la nouvelle version : le test part.
    let stored = f.app.rules.poll_save(Instant::now(), false);
    assert!(stored.is_none());
    f.send(ok_reply(save, b"{}".to_vec()));
    f.app.pump_network();
    let test = pending_of(
        &f,
        |p| matches!(p, Pending::Command { what, .. } if *what == "Test de la règle"),
    );
    assert!(test.is_some(), "le test part une fois la règle enregistrée");
}

#[test]
fn testing_is_refused_while_the_edit_cannot_be_saved() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("");
    f.app.test_rule(1);
    assert!(
        f.app.pending.is_empty(),
        "ni envoi ni test d'une version périmée"
    );
    assert!(f
        .app
        .error_banner
        .as_ref()
        .unwrap()
        .1
        .contains("Test impossible"));
    // Sans modification en attente, le test part tout de suite.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.test_rule(1);
    assert!(pending_of(&f, |p| matches!(p, Pending::Command { .. })).is_some());
}

// ---------------------------------------------------------------------------
// Plan sans étage, appairage, fenêtre Données, mémoire à la déconnexion, notifications
// ---------------------------------------------------------------------------

/// Un plan sans étage (jamais envoyé par ce serveur, mais l'interface ne doit pas en dépendre)
/// est remplacé par le plan par défaut ; il ne fait pas paniquer l'affichage.
#[test]
fn a_plan_without_floors_never_reaches_the_drawing_code() {
    let mut f = Fixture::new();
    let mut empty = snapshot(1, vec![]);
    empty.plan.0.floors.clear();
    f.connect(empty);
    assert_eq!(f.app.plan.show_name(), "Rez-de-chaussée");
    let mut none = HomePlan::default();
    none.floors.clear();
    f.live(WireEvent::PlanChanged {
        plan: TextIds(none),
    });
    assert_eq!(f.app.plan.show_name(), "Rez-de-chaussée");
    // Dessin réel du plan, sans fenêtre : ne panique pas.
    let ctx = egui::Context::default();
    ctx.run_ui(egui::RawInput::default(), |ui| {
        let _ = f.app.plan.show(ui, &f.app.model);
    })
    .textures_delta
    .clear();
    // Et même si un plan vide atteignait l'affichage (garde obligatoire, au lieu d'une panique).
    f.app.plan.force_no_floor();
    ctx.run_ui(egui::RawInput::default(), |ui| {
        assert!(matches!(
            f.app.plan.show(ui, &f.app.model),
            plan::PlanAction::None
        ));
    })
    .textures_delta
    .clear();
}

#[test]
fn a_pairing_stuck_in_progress_is_released_by_a_snapshot_that_says_it_is_over() {
    let mut f = Fixture::new();
    let mut busy = snapshot(1, vec![]);
    busy.pairing_busy = true;
    f.connect(busy);
    assert!(matches!(f.app.pairing, Pairing::InProgress(_)));
    // Resynchronisation : la fin de l'appairage a été manquée.
    f.send(Event::Snapshot(Box::new(snapshot(5, vec![]))));
    f.app.pump_network();
    assert!(matches!(f.app.pairing, Pairing::Idle));

    // Mais une demande d'appairage encore en vol n'est pas interrompue par un état complet.
    f.app.pair("12345678901".into());
    assert!(matches!(f.app.pairing, Pairing::InProgress(_)));
    f.send(Event::Snapshot(Box::new(snapshot(6, vec![]))));
    f.app.pump_network();
    assert!(matches!(f.app.pairing, Pairing::InProgress(_)));
}

#[test]
fn the_pairing_code_is_erased_as_soon_as_pairing_ends() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.code = "12345678901".into();
    f.live(WireEvent::PairingFailed {
        code: ErrorCode::PairingFailed,
        detail: None,
    });
    assert!(f.app.code.is_empty());
    f.app.code = "12345678901".into();
    f.app.pair("12345678901".into());
    let id = pending_of(&f, |p| matches!(p, Pending::Pair)).unwrap();
    f.send(err_reply(id, api_error(409, ErrorCode::PairingBusy)));
    f.app.pump_network();
    assert!(f.app.code.is_empty());
}

#[test]
fn disconnecting_forgets_the_journal_the_toasts_the_code_and_the_dialogs_in_progress() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.code = "12345678901".into();
    f.live(WireEvent::Notification {
        title: "Porte".into(),
        message: "ouverte".into(),
        at: 1.0,
    });
    f.app.show_error("une erreur".into());
    assert!(!f.app.log.is_empty() && !f.app.toasts.is_empty());
    f.app.data_dialog = Some(DataDialog::Busy("Téléchargement…".into()));
    disconnect(&mut f);
    assert!(f.app.toasts.is_empty());
    assert!(f.app.code.is_empty());
    assert!(f.app.error_banner.is_none());
    assert!(
        f.app.data_dialog.is_none(),
        "plus de téléchargement attendu"
    );
    assert_eq!(f.app.log.len(), 1, "seule la raison de la coupure reste");
    assert!(f.app.log[0].contains("Liaison perdue"));

    // « Import accepté » reste affiché : le serveur redémarre, la coupure est attendue.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.data_dialog = Some(DataDialog::ImportStarted);
    disconnect(&mut f);
    assert!(matches!(f.app.data_dialog, Some(DataDialog::ImportStarted)));
    // Déconnexion volontaire : seules les confirmations d'import disparaissent aussi.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.data_dialog = Some(DataDialog::ConfirmImport {
        path: PathBuf::from("/tmp/x.matterhome"),
        summary: "1 appareil".into(),
        bytes: Arc::new(vec![1, 2, 3]),
        server: f.app.server.clone(),
        identity_warning: None,
        replace_armed: false,
    });
    f.send(Event::Link(Link::LoggedOut {
        url: None,
        pinned: false,
        reason: None,
    }));
    f.app.pump_network();
    assert!(f.app.data_dialog.is_none());
}

/// Une sauvegarde ne part jamais vers un autre serveur que celui qui l'a validée.
#[test]
fn a_backup_is_only_sent_to_the_server_that_checked_it() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    let bytes = Arc::new(vec![b'{', b'}']);
    let confirm = |server: Option<String>| DataDialog::ConfirmImport {
        path: PathBuf::from("/tmp/x.matterhome"),
        summary: "résumé".into(),
        bytes: bytes.clone(),
        server,
        identity_warning: None,
        replace_armed: false,
    };
    // Validée par un autre serveur : refus, rien n'est envoyé.
    f.app.data_dialog = Some(confirm(Some("https://autre:8443#CC".into())));
    f.app.confirm_import(false);
    assert!(pending_of(&f, |p| matches!(p, Pending::ImportApply)).is_none());
    assert!(matches!(
        f.app.data_dialog,
        Some(DataDialog::Message { ok: false, .. })
    ));
    // Validée par ce serveur : elle part, sans copie du contenu.
    f.app.data_dialog = Some(confirm(f.app.server.clone()));
    f.app.confirm_import(false);
    assert!(pending_of(&f, |p| matches!(p, Pending::ImportApply)).is_some());
}

/// La sauvegarde lue sert à la vérification puis à l'import réel sans être copiée.
#[test]
fn the_backup_read_by_a_network_thread_is_shared_never_copied() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    let bytes = Arc::new(vec![0u8; 1024]);
    f.app.pending.insert(
        77,
        Pending::ReadBackup {
            path: PathBuf::from("/tmp/x.matterhome"),
        },
    );
    f.send(Event::FileRead {
        id: 77,
        result: Ok(bytes.clone()),
    });
    f.app.pump_network();
    let Some(Pending::ImportCheck { bytes: held, .. }) = f
        .app
        .pending
        .values()
        .find(|p| matches!(p, Pending::ImportCheck { .. }))
    else {
        panic!("la vérification n'est pas partie")
    };
    assert!(Arc::ptr_eq(held, &bytes), "même contenu, pas une copie");
    // Fichier trop gros ou illisible : message, rien d'envoyé.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.pending.insert(
        3,
        Pending::ReadBackup {
            path: PathBuf::from("/tmp/x.matterhome"),
        },
    );
    f.send(Event::FileRead {
        id: 3,
        result: Err(NetError::Local("fichier trop volumineux".into())),
    });
    f.app.pump_network();
    assert!(matches!(
        f.app.data_dialog,
        Some(DataDialog::Message { ok: false, .. })
    ));
    assert!(f.app.pending.is_empty());
}

#[test]
fn a_rule_notification_also_raises_a_local_notification() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    f.app.notifier = Box::new(move |title, message| {
        log.borrow_mut()
            .push((title.to_string(), message.to_string()));
    });
    f.live(WireEvent::Notification {
        title: "Porte d'entrée".into(),
        message: "ouverte à 14:02".into(),
        at: 1.0,
    });
    assert_eq!(
        seen.borrow().as_slice(),
        [("Porte d'entrée".to_string(), "ouverte à 14:02".to_string())]
    );
    assert_eq!(f.app.toasts.len(), 1, "le toast reste");
}

#[test]
fn forgetting_the_certificate_is_not_offered_while_logging_in() {
    let mut screen = connect::ConnectScreen::new();
    assert!(screen.reset_allowed());
    screen.start_logging_in();
    assert!(!screen.reset_allowed());
}

/// Un nouvel essai de connexion n'efface pas le journal : l'avis du moteur et la raison de la
/// coupure restent lisibles.
#[test]
fn a_new_connection_attempt_does_not_erase_the_journal() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    disconnect(&mut f);
    f.send(Event::Notice("La session reste ouverte, mais…".into()));
    f.send(Event::Link(Link::Connecting));
    f.app.pump_network();
    let lines = f
        .app
        .log
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        lines.contains("Liaison perdue") && lines.contains("La session reste ouverte"),
        "{lines}"
    );
    assert!(
        f.app.model.devices.is_empty(),
        "les données, elles, sont bien effacées"
    );
}

// ---------------------------------------------------------------------------
// Fermeture de la fenêtre, brouillons perdus, indicateur global, retour au serveur
// ---------------------------------------------------------------------------

/// Sans brouillon, ou avec un brouillon complet (il part à la fermeture) : on ferme sans question.
#[test]
fn closing_asks_nothing_when_nothing_would_be_lost() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    assert_eq!(f.app.on_close_requested(), CloseDecision::Allow);
    f.app.rules.add_webhook_rule("https://exemple.fr/ok");
    assert_eq!(f.app.on_close_requested(), CloseDecision::Allow);
    assert!(f.app.close_prompt.is_none());
}

/// Ce qui ne peut pas partir (adresse incomplète, liaison coupée, choix « modifié ailleurs »
/// en attente) est signalé avant de fermer ; la seconde demande passe, « Rester » annule.
#[test]
fn closing_asks_before_losing_a_draft_that_cannot_be_sent() {
    // Webhook incomplet.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("");
    let CloseDecision::Ask(text) = f.app.on_close_requested() else {
        panic!("la question devait être posée");
    };
    assert!(
        text.contains("les règles") && text.contains("adresse"),
        "{text}"
    );
    assert_eq!(f.app.close_prompt.as_deref(), Some(text.as_str()));
    // Il insiste : la fenêtre ne reste jamais bloquée (Cmd+Q ne passe de toute façon pas ici).
    assert_eq!(f.app.on_close_requested(), CloseDecision::Allow);
    // « Rester » : la question disparaît, la prochaine demande la repose.
    f.app.close_prompt = None;
    assert!(matches!(f.app.on_close_requested(), CloseDecision::Ask(_)));
    // Dès qu'il n'y a plus rien à perdre, la question disparaît d'elle-même.
    f.app.rules.set_webhook_url("https://exemple.fr/ok");
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        f.app.close_prompt_window(&ui.ctx().clone());
    });
    output.textures_delta.clear();
    assert!(f.app.close_prompt.is_none());

    // Liaison coupée avec un brouillon gardé.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Étage A");
    disconnect(&mut f);
    let CloseDecision::Ask(text) = f.app.on_close_requested() else {
        panic!("liaison coupée : la question devait être posée");
    };
    assert!(
        text.contains("le plan") && text.contains("liaison"),
        "{text}"
    );

    // Un autre client a modifié pendant l'édition : rien ne part tant qu'on n'a pas choisi.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.plan.edit_name("Mon étage");
    let mut other = HomePlan::default();
    other.floors[0].name = "Étage d'un autre".into();
    f.live(WireEvent::PlanChanged {
        plan: TextIds(other),
    });
    assert!(f.app.plan.conflict_pending());
    let CloseDecision::Ask(text) = f.app.on_close_requested() else {
        panic!("conflit : la question devait être posée");
    };
    assert!(text.contains("le plan"), "{text}");
}

/// Un brouillon incomplet ou un choix en attente se voit depuis toutes les vues, et l'on peut
/// sortir d'un brouillon de règles en revenant à la version du serveur.
#[test]
fn a_pending_draft_shows_in_the_top_bar_and_can_be_dropped() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    assert!(f.app.sync_alerts().is_empty());
    f.app.view = View::Devices;
    f.app.rules.add_webhook_rule("");
    let alerts = f.app.sync_alerts();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert_eq!(alerts[0].view, View::Rules);
    assert_eq!(alerts[0].label, "⚠ Règles non enregistrées");
    assert!(alerts[0].detail.contains("adresse"), "{alerts:?}");

    // Revenir à la version du serveur : le brouillon disparaît, l'indicateur aussi.
    assert!(f.app.rules.revert_to_server());
    assert_eq!(f.app.rules.rule_count(), 0);
    assert!(!f.app.rules.editing());
    assert!(f.app.sync_alerts().is_empty());
    // Et rien ne repart tout seul.
    f.app
        .save_documents(Instant::now() + Duration::from_secs(5), false);
    assert!(f.app.pending.is_empty());

    // Plan modifié ailleurs pendant l'édition.
    f.app.plan.edit_name("Mon étage");
    let mut other = HomePlan::default();
    other.floors[0].name = "Étage d'un autre".into();
    f.live(WireEvent::PlanChanged {
        plan: TextIds(other),
    });
    let alerts = f.app.sync_alerts();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert_eq!(
        (alerts[0].view, alerts[0].label.as_str()),
        (View::Plan, "⚠ Plan modifié ailleurs")
    );
    assert!(f.app.plan.revert_to_server());
    assert_eq!(f.app.plan.show_name(), "Étage d'un autre");
    assert!(f.app.sync_alerts().is_empty());

    // Hors ligne avec un brouillon gardé : indiqué aussi.
    f.app.plan.edit_name("Hors ligne");
    disconnect(&mut f);
    let alerts = f.app.sync_alerts();
    assert!(
        alerts.iter().any(|a| a.view == View::Plan
            && a.label == "⚠ Plan non enregistré"
            && a.detail.contains("coupée")),
        "{alerts:?}"
    );
    // Sans version du serveur connue, on ne peut pas revenir en arrière.
    assert!(!f.app.plan.revert_to_server());
    assert_eq!(f.app.plan.show_name(), "Hors ligne");
}

/// Pas de retour en arrière pendant un envoi : sa réponse réécrirait le document.
#[test]
fn dropping_a_draft_is_refused_while_an_upload_is_in_flight() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("https://exemple.fr/ok");
    f.app
        .save_documents(Instant::now() + Duration::from_secs(5), false);
    assert!(pending_of(&f, |p| matches!(p, Pending::RulesSave)).is_some());
    assert!(!f.app.rules.revert_to_server());
    assert_eq!(f.app.rules.rule_count(), 1);
}

/// Un retour à l'écran de connexion qui abandonne un brouillon le dit, sur chaque écran.
#[test]
fn leaving_for_the_login_screen_says_what_was_dropped() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("");
    f.app.plan.edit_name("Autre nom");
    f.send(Event::Link(Link::LoggedOut {
        url: Some("https://127.0.0.1:8443".into()),
        pinned: true,
        reason: Some("Votre session a expiré.".into()),
    }));
    f.app.pump_network();
    let message = f.app.connect.message_text().expect("un message");
    assert!(
        message.contains("Votre session a expiré.")
            && message.contains("du plan et des règles")
            && message.contains("abandonnées"),
        "{message}"
    );
    assert!(!f.app.rules.editing() && !f.app.plan.editing());

    // Certificat changé : même dire.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("");
    f.send(Event::Link(Link::CertificateChanged {
        url: "https://127.0.0.1:8443".into(),
        expected: "AA".into(),
        found: "BB".into(),
    }));
    f.app.pump_network();
    let message = f.app.connect.message_text().expect("un message");
    assert!(
        message.contains("des règles") && message.contains("abandonnées"),
        "{message}"
    );

    // Rien à perdre : pas de message inventé.
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.send(Event::Link(Link::CertificateChanged {
        url: "https://127.0.0.1:8443".into(),
        expected: "AA".into(),
        found: "BB".into(),
    }));
    f.app.pump_network();
    assert_eq!(f.app.connect.message_text(), None);
}

/// Dessin réel (sans fenêtre) de la barre du haut, des bandeaux et de la question de fermeture
/// avec des brouillons en attente : ne panique pas, et le bouton de retour est bien là.
#[test]
fn the_draft_widgets_draw_without_a_window() {
    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    f.app.rules.add_webhook_rule("");
    f.app.plan.edit_name("Brouillon");
    let CloseDecision::Ask(_) = f.app.on_close_requested() else {
        panic!("question attendue");
    };
    let ctx = egui::Context::default();
    for view in [
        View::Devices,
        View::Plan,
        View::Rules,
        View::Temperatures,
        View::Cameras,
    ] {
        f.app.view = view;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            f.app.top_bar(ui);
            match view {
                View::Plan => {
                    let _ = f.app.plan.show(ui, &f.app.model);
                }
                View::Rules => {
                    let _ = f
                        .app
                        .rules
                        .show(ui, &f.app.model.devices, &f.app.model.rule_runs);
                }
                View::Temperatures => f.app.temperatures_page(ui),
                View::Cameras => cameras::show(ui, &f.app.model, f.app.plan.plan()),
                View::Devices => {}
            }
            f.app.close_prompt_window(&ui.ctx().clone());
        });
        output.textures_delta.clear();
    }
    assert!(f.app.close_prompt.is_some());
}

fn identity(differs: bool) -> ImportIdentity {
    ImportIdentity {
        incoming: IdentityInfo {
            fabric_id: "0xff".into(),
            devices: 3,
        },
        current: Some(IdentityInfo {
            fabric_id: "0x1".into(),
            devices: 1,
        }),
        differs,
    }
}

/// Une sauvegarde d'une autre identité Matter prévient clairement, ne part jamais sans une
/// confirmation séparée, et n'envoie `replace_identity=1` qu'alors.
#[test]
fn a_different_identity_is_only_sent_after_a_separate_confirmation() {
    let warning = describe_identity_warning(&identity(true)).expect("avertissement");
    assert!(
        warning.contains("0xff") && warning.contains("0x1") && warning.contains("3 appareils"),
        "{warning}"
    );
    assert!(warning.contains("réappairer"), "{warning}");
    assert!(describe_identity_warning(&identity(false)).is_none());

    let mut f = Fixture::new();
    f.connect(snapshot(1, vec![]));
    let confirm = |armed: bool, server: Option<String>| DataDialog::ConfirmImport {
        path: PathBuf::from("/tmp/x.matterhome"),
        summary: "résumé".into(),
        bytes: Arc::new(vec![b'{', b'}']),
        server,
        identity_warning: Some(warning.clone()),
        replace_armed: armed,
    };
    let sent = |f: &Fixture| pending_of(f, |p| matches!(p, Pending::ImportApply)).is_some();
    // Le bouton ordinaire (sans confirmation) ne fait rien partir.
    f.app.data_dialog = Some(confirm(false, f.app.server.clone()));
    f.app.confirm_import(false);
    assert!(!sent(&f));
    assert!(matches!(
        f.app.data_dialog,
        Some(DataDialog::Message { ok: false, .. })
    ));
    // Même `replace_identity` sans que la confirmation séparée ait été affichée : rien.
    f.app.data_dialog = Some(confirm(false, f.app.server.clone()));
    f.app.confirm_import(true);
    assert!(!sent(&f));
    // Après la confirmation séparée : l'import part avec replace_identity=1.
    f.app.data_dialog = Some(confirm(true, f.app.server.clone()));
    f.app.confirm_import(true);
    assert!(sent(&f));
    // Le chemin demandé : l'option n'existe que pour ce cas.
    assert_eq!(import_path(true), "/api/import?replace_identity=1");
    assert_eq!(import_path(false), "/api/import");
}

#[test]
fn temperature_page_reads_every_thermometer_by_window_and_follows_live_readings() {
    let mut f = Fixture::new();
    let thermo = Capabilities {
        temperature: true,
        ..Default::default()
    };
    let plug = Capabilities {
        power: true,
        ..Default::default()
    };
    f.connect(snapshot(
        1,
        vec![
            view(node(1, "Salon", thermo), vec![]),
            view(node(2, "Chambre", thermo), vec![]),
            view(node(3, "Prise", plug), vec![]),
        ],
    ));
    // Hors de la page : aucune lecture.
    f.app.maintain_temperatures();
    assert!(f.pending_ids().is_empty());
    f.app.show_temperatures(None);
    // Tant que la largeur du graphique est inconnue (avant la première image), pas de lecture.
    f.app.maintain_temperatures();
    assert!(f.pending_ids().is_empty());
    f.app.temp_buckets = 800;
    f.app.maintain_temperatures();
    assert_eq!(
        f.pending_ids().len(),
        2,
        "un thermomètre, une lecture ; pas la prise"
    );
    f.app.maintain_temperatures();
    assert_eq!(f.pending_ids().len(), 2, "pas de lecture en double");
    assert!(f.app.temp_series.contains_key(&1) && !f.app.temp_series.contains_key(&3));

    // Réponses : chaque lecture est rangée sous son appareil.
    let reply = |id: u64, node_id: u64, values: &[(f64, f64)]| {
        let body = serde_json::to_vec(&HistoryResponse {
            node_id,
            since: 0.0,
            until: 1000.0,
            truncated: false,
            points: values
                .iter()
                .map(|&(at, value)| HistoryPoint {
                    at,
                    metric: Metric::Temperature,
                    value,
                })
                .collect(),
        })
        .unwrap();
        Event::Reply {
            id,
            result: Ok(Response {
                status: 200,
                headers: vec![],
                body,
            }),
        }
    };
    let ids = f.pending_ids();
    let (mut id_of_1, mut id_of_2) = (0, 0);
    for id in ids {
        match f.app.pending.get(&id) {
            Some(Pending::TempHistory { node_id: 1, .. }) => id_of_1 = id,
            Some(Pending::TempHistory { node_id: 2, .. }) => id_of_2 = id,
            _ => panic!("requête inattendue"),
        }
    }
    f.send(reply(id_of_1, 1, &[(100.0, 20.0), (200.0, 21.0)]));
    f.send(reply(id_of_2, 2, &[(100.0, 18.0)]));
    f.app.pump_network();
    assert_eq!(f.app.temp_series[&1].data.len(Metric::Temperature), 2);
    assert_eq!(f.app.temp_series[&2].data.len(Metric::Temperature), 1);
    assert_eq!(f.app.temp_series[&1].window, Some(f.app.temp_window));

    // Les nouvelles mesures s'ajoutent en direct ; celles d'une autre grandeur sont ignorées.
    f.live(WireEvent::Reading {
        node_id: 1,
        metric: Metric::Temperature,
        value: 22.0,
        at: 300.0,
    });
    f.live(WireEvent::Reading {
        node_id: 1,
        metric: Metric::Humidity,
        value: 50.0,
        at: 301.0,
    });
    assert_eq!(f.app.temp_series[&1].data.len(Metric::Temperature), 3);
    assert_eq!(f.app.temp_series[&1].data.len(Metric::Humidity), 0);

    // Changer de période relit tout, à la bonne fenêtre.
    f.app.maintain_temperatures();
    assert!(f.pending_ids().is_empty());
    f.app.temp_window = matter_home_client::series::TimeWindow::All;
    f.app.maintain_temperatures();
    assert_eq!(f.pending_ids().len(), 2);

    // Resynchronisation pendant une lecture en vol : la réponse tardive ne rétablit pas des
    // points périmés, et la lecture repart d'elle-même.
    let in_flight = f.pending_ids();
    assert_eq!(in_flight.len(), 2);
    for t in f.app.temp_series.values_mut() {
        t.invalidate();
    }
    for id in &in_flight {
        let node_id = match f.app.pending.get(id) {
            Some(Pending::TempHistory { node_id, .. }) => *node_id,
            _ => panic!("requête inattendue"),
        };
        f.send(reply(*id, node_id, &[(1.0, 1.0)]));
    }
    f.app.pump_network();
    assert_eq!(
        f.app.temp_series[&1].data.fetched_since, None,
        "invalidation conservée"
    );
    assert_eq!(f.app.temp_series[&1].data.len(Metric::Temperature), 3);
    f.app.maintain_temperatures();
    assert_eq!(
        f.pending_ids().len(),
        2,
        "relecture après la resynchronisation"
    );

    // Appareil retiré : sa lecture est oubliée.
    f.live(WireEvent::DeviceRemoved { node_id: 2 });
    assert!(!f.app.temp_series.contains_key(&2));
}

#[test]
fn the_temperature_chart_draws_curves_with_gaps_and_hidden_lines_without_a_window() {
    use matter_home_client::series::TimeWindow;
    let ctx = egui::Context::default();
    let now = now_unix();
    let curve = |node_id: u64, points: Vec<(f64, f64)>| temperatures::Curve {
        node_id,
        label: format!("Capteur {node_id}"),
        color_index: node_id as usize,
        line: vec![matter_home_client::temperatures::LineSegment {
            mean: points.clone(),
            band: vec![],
        }],
        points,
        gap_s: 1200.0,
        current: Some(20.5),
        stale: false,
    };
    let curves = vec![
        // Un trou au milieu, un point isolé à la fin ; une courbe vide ; une courbe masquée.
        curve(
            1,
            vec![
                (now - 3000.0, 20.0),
                (now - 2900.0, 20.5),
                (now - 900.0, 21.0),
                (now - 10.0, 21.5),
            ],
        ),
        curve(2, vec![]),
        curve(3, vec![(now - 100.0, 18.0), (now - 50.0, 18.5)]),
    ];
    let mut hidden = std::collections::HashSet::from([3_u64]);
    let status = temperatures::Status {
        loading: 1,
        errors: vec!["Introuvable".into()],
        truncated: true,
    };
    for window in TimeWindow::OVERVIEW {
        let mut window = window;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let outcome = temperatures::show(ui, &curves, &mut hidden, &mut window, &status, now);
            assert!(!outcome.retry);
        });
        output.textures_delta.clear();
    }
    // Sans thermomètre : un message, pas de graphique.
    let mut window = TimeWindow::Hour24;
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        temperatures::show(ui, &[], &mut hidden, &mut window, &Default::default(), now);
    });
    output.textures_delta.clear();
}

#[test]
fn the_detail_panel_opens_only_on_a_click_and_closes_with_its_device() {
    let mut f = Fixture::new();
    let caps = Capabilities {
        temperature: true,
        ..Default::default()
    };
    f.connect(snapshot(
        1,
        vec![
            view(node(1, "Salon", caps), vec![]),
            view(node(2, "Chambre", caps), vec![]),
        ],
    ));
    assert_eq!(f.app.selected, None);
    f.live(WireEvent::DeviceUpdated {
        device: node(1, "Salon", caps),
    });
    assert_eq!(f.app.selected, None, "une mise à jour n'ouvre rien");

    // Un clic ouvre ; une édition de nom en cours est abandonnée à la fermeture.
    f.app.selected = Some(2);
    f.app.device_edit = Some(customize::DeviceEdit::from_info(
        &f.app.model.devices[&2].info,
    ));
    f.app.confirm_clear = Some(2);
    f.app.close_detail();
    assert_eq!(f.app.selected, None);
    assert!(f.app.device_edit.is_none());
    assert!(f.app.confirm_clear.is_none());

    // L'appareil ouvert disparaît : le panneau se ferme, sans en ouvrir un autre.
    f.app.selected = Some(2);
    f.live(WireEvent::DeviceRemoved { node_id: 2 });
    assert_eq!(f.app.selected, None);
    assert_eq!(f.app.selected_device(), None);

    // Resynchronisation avec un appareil qui n'existe plus : fermé aussi.
    f.app.selected = Some(1);
    f.send(Event::Snapshot(Box::new(snapshot(9, vec![]))));
    f.app.pump_network();
    assert_eq!(f.app.selected, None);
}

#[test]
fn a_lost_connection_closes_the_detail_and_the_add_window() {
    let mut f = Fixture::new();
    let caps = Capabilities {
        temperature: true,
        ..Default::default()
    };
    f.connect(snapshot(1, vec![view(node(1, "Salon", caps), vec![])]));
    f.app.selected = Some(1);
    f.app.add_open = true;
    // Une simple resynchronisation ne ferme pas la fenêtre.
    f.send(Event::Link(Link::Resyncing));
    f.app.pump_network();
    assert!(f.app.add_open);
    f.send(Event::Link(Link::Connecting));
    f.app.pump_network();
    assert!(!f.app.add_open);
    assert_eq!(
        f.app.selected, None,
        "pas de panneau fantôme à la reconnexion"
    );
}

#[test]
fn entering_the_plan_edit_mode_closes_the_detail() {
    let mut f = Fixture::new();
    let caps = Capabilities {
        temperature: true,
        ..Default::default()
    };
    f.connect(snapshot(1, vec![view(node(1, "Salon", caps), vec![])]));
    f.app.view = View::Plan;
    f.app.selected = Some(1);
    f.app.device_edit = Some(customize::DeviceEdit::from_info(
        &f.app.model.devices[&1].info,
    ));
    f.app.confirm_clear = Some(1);
    // Rien ne change tant que le mode Modifier n'est pas entré.
    f.app.sync_edit_mode(false);
    assert_eq!(f.app.selected, Some(1));
    f.app.plan.start_editing();
    f.app.sync_edit_mode(false);
    assert_eq!(f.app.selected, None);
    assert!(f.app.device_edit.is_none() && f.app.confirm_clear.is_none());
    // Déjà en mode Modifier : une sélection de la palette n'est pas fermée à chaque image.
    f.app.selected = Some(1);
    f.app.sync_edit_mode(true);
    assert_eq!(f.app.selected, Some(1));
}

/// Une lumière allumée, et un volet, comme le serveur les décrit.
fn lamp_and_shutter_fixture() -> Fixture {
    let mut f = Fixture::new();
    let lamp = Capabilities {
        on_off: true,
        ..Default::default()
    };
    let mut lamp_node = node(1, "Lampe du salon", lamp);
    lamp_node.device_types = vec!["Lampe".into()];
    let shutter = Capabilities {
        cover: true,
        cover_position: true,
        ..Default::default()
    };
    f.connect(snapshot(
        1,
        vec![
            view(lamp_node, vec![reading(Metric::OnOff, 1.0, 90.0)]),
            view(
                node(2, "Volet", shutter),
                vec![reading(Metric::Position, 100.0, 90.0)],
            ),
        ],
    ));
    f
}

#[test]
fn clicking_a_light_tile_toggles_it_at_once_and_a_refusal_puts_the_display_back() {
    let mut f = lamp_and_shutter_fixture();
    // Le clic bascule : allumée, donc la commande éteint, et l'état attendu est montré aussitôt.
    f.app.apply_tiles(devices::Outcome {
        toggle: Some(1),
        ..Default::default()
    });
    let id = pending_of(&f, |p| matches!(p, Pending::Command { node: Some(1), .. }))
        .expect("la commande est partie");
    let shown = devices::tile_of(&f.app.model.devices[&1], f.app.expect.get(&1));
    assert_eq!(shown.value.as_deref(), Some("Éteinte"), "retour immédiat");
    // Le serveur refuse : l'attente cesse, le message passe par le mécanisme habituel.
    f.send(Event::Reply {
        id,
        result: Err(NetError::Api {
            status: 502,
            code: Some(ErrorCode::DeviceUnreachable),
            detail: None,
            retry_after_s: None,
        }),
    });
    f.app.pump_network();
    assert!(!f.app.expect.contains_key(&1));
    let shown = devices::tile_of(&f.app.model.devices[&1], f.app.expect.get(&1));
    assert_eq!(
        shown.value.as_deref(),
        Some("Allumée"),
        "l'affichage revient"
    );
    let banner = f.app.error_banner.as_ref().expect("erreur affichée");
    assert!(banner.1.contains("Commande de l'appareil"), "{}", banner.1);
}

#[test]
fn clicking_a_shutter_tile_opens_or_closes_it_and_shows_the_movement() {
    let mut f = lamp_and_shutter_fixture();
    let tile = devices::tile_of(&f.app.model.devices[&2], None);
    // Ouvert : le clic ferme.
    assert_eq!(tile.action, devices::TileAction::Cover(CoverCommand::Close));
    f.app.apply_tiles(devices::Outcome {
        cover: Some((2, CoverCommand::Close)),
        ..Default::default()
    });
    assert!(pending_of(&f, |p| matches!(p, Pending::Command { node: Some(2), .. })).is_some());
    let shown = devices::tile_of(&f.app.model.devices[&2], f.app.expect.get(&2));
    assert_eq!(shown.value.as_deref(), Some("Ouvert ▼"));
}

#[test]
fn a_tile_detail_request_opens_the_window_and_the_usual_rules_close_it() {
    let mut f = lamp_and_shutter_fixture();
    f.app.view = View::Devices;
    f.app.apply_tiles(devices::Outcome {
        detail: Some(1),
        ..Default::default()
    });
    assert_eq!(f.app.selected_device(), Some(1));
    // L'appareil disparaît : la fenêtre se ferme.
    f.live(WireEvent::DeviceRemoved { node_id: 1 });
    assert_eq!(f.app.selected_device(), None);
    // La déconnexion aussi.
    f.app.selected = Some(2);
    disconnect(&mut f);
    assert_eq!(f.app.selected, None);
}

/// La fenêtre de détail se dessine sans fenêtre système, pour chaque sorte d'appareil : une
/// lumière sans courbe, un volet, un capteur.
#[test]
fn the_detail_window_draws_for_every_kind_of_device() {
    let mut f = lamp_and_shutter_fixture();
    let sensor = Capabilities {
        temperature: true,
        battery: true,
        ..Default::default()
    };
    f.live(WireEvent::DeviceUpdated {
        device: node(3, "Salon", sensor),
    });
    let ctx = egui::Context::default();
    for id in [1, 2, 3] {
        f.app.selected = Some(id);
        let layout = devices::groups(&f.app.model.devices, f.app.plan.plan());
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            f.app.detail_window(&ui.ctx().clone(), &layout);
        });
        out.textures_delta.clear();
        assert_eq!(f.app.selected, Some(id), "la fenêtre reste ouverte");
    }
}

#[test]
fn an_old_refusal_does_not_clear_the_wait_of_a_newer_command() {
    let mut f = lamp_and_shutter_fixture();
    // Première commande : éteindre. Seconde, aussitôt : rallumer. L'attente est celle de la seconde.
    f.app.set_on(1, false);
    let first = pending_of(&f, |p| matches!(p, Pending::Command { node: Some(1), .. })).unwrap();
    f.app.set_on(1, true);
    let second = f
        .app
        .pending
        .iter()
        .find(|(id, p)| **id != first && matches!(p, Pending::Command { node: Some(1), .. }))
        .map(|(id, _)| *id)
        .unwrap();
    assert_eq!(f.app.expect[&1].request, Some(second));
    let refused = |id| Event::Reply {
        id,
        result: Err(NetError::Api {
            status: 502,
            code: Some(ErrorCode::DeviceUnreachable),
            detail: None,
            retry_after_s: None,
        }),
    };
    // Le vieux refus arrive : l'attente de la seconde commande reste.
    f.send(refused(first));
    f.app.pump_network();
    assert_eq!(f.app.expect[&1].on, Some(true));
    // Le refus de la seconde l'efface.
    f.send(refused(second));
    f.app.pump_network();
    assert!(!f.app.expect.contains_key(&1));
    // Un accord marque l'attente comme acceptée (un clic de plus n'est plus ignoré après le délai).
    f.app.set_on(1, false);
    let third = f
        .app
        .pending
        .iter()
        .find(|(_, p)| matches!(p, Pending::Command { node: Some(1), .. }))
        .map(|(id, _)| *id)
        .unwrap();
    assert!(!f.app.expect[&1].acked);
    f.send(Event::Reply {
        id: third,
        result: Ok(Response {
            status: 202,
            headers: vec![],
            body: vec![],
        }),
    });
    f.app.pump_network();
    assert!(f.app.expect[&1].acked);
}

#[test]
fn the_waits_are_purged_when_done_or_stale() {
    let mut f = lamp_and_shutter_fixture();
    f.app.set_on(1, false);
    let now = Instant::now();
    // En cours : gardée.
    f.app.purge_expect(now);
    assert!(f.app.expect.contains_key(&1));
    // Périmée : retirée.
    f.app
        .purge_expect(now + devices::EXPECT_TTL + Duration::from_millis(1));
    assert!(f.app.expect.is_empty());
    // Réalisée (l'appareil a l'état demandé) : retirée.
    f.app.set_on(1, true);
    f.app.purge_expect(Instant::now());
    assert!(f.app.expect.is_empty(), "la lampe est déjà allumée");
}

fn escape_frame(f: &mut Fixture, ctx: &egui::Context) {
    let layout = devices::groups(&f.app.model.devices, f.app.plan.plan());
    let input = egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        f.app.detail_window(&ui.ctx().clone(), &layout);
    });
    out.textures_delta.clear();
}

#[test]
fn escape_cancels_the_name_edit_without_closing_the_window_then_closes_it() {
    let mut f = lamp_and_shutter_fixture();
    let ctx = egui::Context::default();
    f.app.selected = Some(1);
    f.app.device_edit = Some(customize::DeviceEdit::from_info(
        &f.app.model.devices[&1].info,
    ));
    escape_frame(&mut f, &ctx);
    assert!(f.app.device_edit.is_none(), "l'édition est annulée");
    assert_eq!(f.app.selected, Some(1), "la fenêtre reste ouverte");
    // Sans édition en cours, Échap ferme la fenêtre.
    escape_frame(&mut f, &ctx);
    assert_eq!(f.app.selected, None);
}
