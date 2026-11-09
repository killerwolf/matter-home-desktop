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
        Fixture {
            app: MatterApp::new(backend, events),
            tx,
        }
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
        f.app.selected,
        Some(1),
        "le premier appareil est sélectionné"
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
    assert_eq!(f.app.selected, Some(9));
    assert_eq!(f.app.model.devices.len(), 6);
    f.live(WireEvent::DeviceRemoved { node_id: 9 });
    assert_eq!(f.app.model.devices.len(), 5);
    assert_ne!(f.app.selected, Some(9));

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

    // Les modifications sont envoyées une à la fois, après un délai de calme.
    let later = Instant::now() + Duration::from_secs(5);
    assert!(f.app.plan.poll_save(later).is_some());
    f.app.save_documents(later);
    assert!(f
        .app
        .pending
        .values()
        .any(|p| matches!(p, Pending::PlanSave)));
    assert!(
        f.app.plan.poll_save(later).is_none(),
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
        history_files: 12,
        has_plan: true,
        has_rules: true,
    };
    let text = describe_backup(&s);
    assert!(
        text.starts_with("12 appareils, 7 pièces, 1 règle, sauvegarde du "),
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
    write_file(&path, b"{}", true).unwrap();
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
