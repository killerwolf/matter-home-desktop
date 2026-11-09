//! État affiché, tel que le protocole le décrit : l'état complet (`Snapshot`) puis les
//! événements numérotés (`WireEvent`). Le client ne garde aucune autre vérité : tout ce qui
//! est ici vient du serveur et disparaît à la déconnexion.

use std::collections::{BTreeMap, VecDeque};

use protocol::automation::RuleSet;
use protocol::events::{Capabilities, CoverMotion};
use protocol::home::HomePlan;
use protocol::wire::{
    DeviceView, ErrorCode, Metric, RuleRunView, Snapshot, WireEvent, WireMode, WireNode,
    WireStatus, RULE_RUNS_KEPT,
};

/// Dernière valeur connue d'une grandeur, et l'instant de la mesure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub value: f64,
    pub at: f64,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub info: WireNode,
    pub status: WireStatus,
    pub last_seen: Option<f64>,
    pub mode: Option<WireMode>,
    latest: BTreeMap<Metric, Reading>,
    /// Instant du dernier changement d'ouverture observé.
    pub open_since: Option<f64>,
    pub cover_motion: Option<CoverMotion>,
    /// Position choisie sur le curseur de la fiche, pas encore envoyée (brouillon local).
    pub cover_slider: Option<u8>,
}

impl Device {
    pub fn new(info: WireNode) -> Self {
        Self {
            info,
            status: WireStatus::Connecting,
            last_seen: None,
            mode: None,
            latest: BTreeMap::new(),
            open_since: None,
            cover_motion: None,
            cover_slider: None,
        }
    }

    pub fn from_view(view: DeviceView) -> Self {
        Self {
            info: view.device,
            status: view.status,
            last_seen: view.last_seen,
            mode: view.mode,
            latest: view
                .latest
                .into_iter()
                .map(|l| {
                    (
                        l.metric,
                        Reading {
                            value: l.value,
                            at: l.at,
                        },
                    )
                })
                .collect(),
            open_since: view.open_since,
            cover_motion: view.cover_motion,
            cover_slider: None,
        }
    }

    pub fn caps(&self) -> Capabilities {
        self.info.caps
    }

    pub fn latest(&self, metric: Metric) -> Option<Reading> {
        self.latest.get(&metric).copied()
    }

    pub fn temperature(&self) -> Option<Reading> {
        self.latest(Metric::Temperature)
    }
    pub fn humidity(&self) -> Option<Reading> {
        self.latest(Metric::Humidity)
    }
    pub fn power(&self) -> Option<Reading> {
        self.latest(Metric::Power)
    }
    /// Énergie cumulée (kWh).
    pub fn energy(&self) -> Option<Reading> {
        self.latest(Metric::Energy)
    }
    pub fn position(&self) -> Option<Reading> {
        self.latest(Metric::Position)
    }
    pub fn battery(&self) -> Option<f64> {
        self.latest(Metric::Battery).map(|r| r.value)
    }
    pub fn battery_low(&self) -> bool {
        self.latest(Metric::BatteryLow)
            .is_some_and(|r| r.value != 0.0)
    }
    /// `Some(true)` = ouvert.
    pub fn open(&self) -> Option<bool> {
        self.latest(Metric::Open).map(|r| r.value != 0.0)
    }
    pub fn on_off(&self) -> Option<bool> {
        self.latest(Metric::OnOff).map(|r| r.value != 0.0)
    }

    /// Retient une mesure, sauf si elle est plus ancienne que celle déjà connue (historique
    /// rejoué, message en retard). Rend `true` si elle a été retenue.
    pub fn apply_reading(&mut self, metric: Metric, value: f64, at: f64) -> bool {
        if self.latest.get(&metric).is_some_and(|known| at < known.at) {
            return false;
        }
        if metric == Metric::Open && self.open() != Some(value != 0.0) {
            self.open_since = Some(at);
        }
        self.latest.insert(metric, Reading { value, at });
        true
    }

    /// Résumé d'une ligne pour la liste : mesure principale et pile.
    pub fn summary(&self) -> String {
        use protocol::format::{format_position, format_power, format_temperature};
        let mut parts = Vec::new();
        if let Some(s) = self.temperature() {
            parts.push(format_temperature(s.value));
        }
        if let Some(open) = self.open() {
            parts.push(if open {
                "Ouvert".into()
            } else {
                "Fermé".into()
            });
        }
        if let Some(s) = self.power() {
            parts.push(format_power(s.value));
        }
        if let Some(on) = self.on_off() {
            parts.push(if on {
                "Allumé".into()
            } else {
                "Éteint".into()
            });
        }
        if self.info.caps.cover {
            let mut text = match self.position() {
                Some(s) => format_position(s.value),
                None if self.info.caps.cover_position => "Position inconnue".into(),
                None => String::new(),
            };
            match self.cover_motion {
                Some(CoverMotion::Opening) => text.push_str(" ▲ ouverture"),
                Some(CoverMotion::Closing) => text.push_str(" ▼ fermeture"),
                _ => {}
            }
            let text = text.trim().to_string();
            if !text.is_empty() {
                parts.push(text);
            }
        }
        if let Some(b) = self.battery() {
            parts.push(format!(
                "pile {b:.0} %{}",
                if self.battery_low() { " (faible)" } else { "" }
            ));
        }
        parts.join(" · ")
    }
}

/// Ce qu'un événement a changé et que l'interface doit savoir (journal, plan, règles,
/// appairage, notification). Les changements de simples valeurs n'ont pas besoin d'être
/// rapportés : l'interface relit le modèle à chaque image.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Nothing,
    /// Appareil nouveau ou modifié.
    DeviceUpdated(u64),
    DeviceRemoved(u64),
    /// Une mesure retenue (pour compléter une série déjà chargée).
    Reading {
        node_id: u64,
        metric: Metric,
        value: f64,
        at: f64,
    },
    /// Un appareil devient injoignable ou revient en ligne : ligne de journal.
    Journal(String),
    PairingProgress(String),
    PairingDone {
        node_id: u64,
        name: String,
    },
    PairingFailed {
        code: ErrorCode,
        detail: Option<String>,
    },
    Notification {
        title: String,
        message: String,
    },
    RuleRun(RuleRunView),
    PlanChanged(Box<HomePlan>),
    RulesChanged(Box<RuleSet>),
}

#[derive(Debug, Default)]
pub struct Model {
    pub devices: BTreeMap<u64, Device>,
    /// Les plus récentes en dernier.
    pub rule_runs: VecDeque<RuleRunView>,
    pub pairing_busy: bool,
    /// Numéro du dernier événement pris en compte.
    pub seq: u64,
}

impl Model {
    /// Remplace tout par l'état complet. Les brouillons locaux des fiches (curseur de volet)
    /// survivent à une resynchronisation.
    pub fn apply_snapshot(&mut self, snapshot: Snapshot) -> (HomePlan, RuleSet) {
        let sliders: BTreeMap<u64, u8> = self
            .devices
            .iter()
            .filter_map(|(id, d)| d.cover_slider.map(|s| (*id, s)))
            .collect();
        self.devices = snapshot
            .devices
            .into_iter()
            .map(Device::from_view)
            .map(|mut d| {
                d.cover_slider = sliders.get(&d.info.node_id).copied();
                (d.info.node_id, d)
            })
            .collect();
        self.rule_runs = snapshot.rule_runs.into();
        self.rule_runs.truncate_front(RULE_RUNS_KEPT);
        self.pairing_busy = snapshot.pairing_busy;
        self.seq = snapshot.seq;
        (snapshot.plan.0, snapshot.rules.0)
    }

    /// Oublie tout (déconnexion).
    pub fn clear(&mut self) {
        *self = Model::default();
    }

    pub fn apply_event(&mut self, event: WireEvent) -> Change {
        match event {
            WireEvent::DeviceUpdated { device } => {
                let id = device.node_id;
                match self.devices.get_mut(&id) {
                    Some(d) => d.info = device,
                    None => {
                        self.devices.insert(id, Device::new(device));
                    }
                }
                Change::DeviceUpdated(id)
            }
            WireEvent::DeviceRemoved { node_id } => {
                self.devices.remove(&node_id);
                Change::DeviceRemoved(node_id)
            }
            WireEvent::Status { node_id, status } => {
                let Some(d) = self.devices.get_mut(&node_id) else {
                    return Change::Nothing;
                };
                let was_unreachable = matches!(d.status, WireStatus::Unreachable { .. });
                let line = match &status {
                    WireStatus::Unreachable { detail, .. } if !was_unreachable => Some(format!(
                        "{} injoignable{}",
                        d.info.name,
                        detail
                            .as_deref()
                            .map(|e| format!(" : {e}"))
                            .unwrap_or_default()
                    )),
                    WireStatus::Online if was_unreachable => {
                        Some(format!("{} de nouveau en ligne", d.info.name))
                    }
                    _ => None,
                };
                d.status = status;
                line.map_or(Change::Nothing, Change::Journal)
            }
            WireEvent::Seen { node_id, at } => {
                if let Some(d) = self.devices.get_mut(&node_id) {
                    d.last_seen = Some(d.last_seen.map_or(at, |s| s.max(at)));
                }
                Change::Nothing
            }
            WireEvent::Mode { node_id, mode } => {
                if let Some(d) = self.devices.get_mut(&node_id) {
                    d.mode = Some(mode);
                }
                Change::Nothing
            }
            WireEvent::Reading {
                node_id,
                metric,
                value,
                at,
            } => {
                let retained = self
                    .devices
                    .get_mut(&node_id)
                    .is_some_and(|d| d.apply_reading(metric, value, at));
                if retained {
                    Change::Reading {
                        node_id,
                        metric,
                        value,
                        at,
                    }
                } else {
                    Change::Nothing
                }
            }
            WireEvent::CoverMotion { node_id, motion } => {
                if let Some(d) = self.devices.get_mut(&node_id) {
                    d.cover_motion = Some(motion);
                }
                Change::Nothing
            }
            WireEvent::RuleRun(run) => {
                self.rule_runs.push_back(run.clone());
                while self.rule_runs.len() > RULE_RUNS_KEPT {
                    self.rule_runs.pop_front();
                }
                Change::RuleRun(run)
            }
            WireEvent::PairingProgress { message } => {
                self.pairing_busy = true;
                Change::PairingProgress(message)
            }
            WireEvent::PairingDone { node_id, name } => {
                self.pairing_busy = false;
                Change::PairingDone { node_id, name }
            }
            WireEvent::PairingFailed { code, detail } => {
                self.pairing_busy = false;
                Change::PairingFailed { code, detail }
            }
            WireEvent::Notice { text } => Change::Journal(text),
            WireEvent::Notification { title, message, .. } => {
                Change::Notification { title, message }
            }
            WireEvent::PlanChanged { plan } => Change::PlanChanged(Box::new(plan.0)),
            WireEvent::RulesChanged { rules } => Change::RulesChanged(Box::new(rules.0)),
        }
    }
}

trait TruncateFront {
    fn truncate_front(&mut self, keep: usize);
}

impl<T> TruncateFront for VecDeque<T> {
    fn truncate_front(&mut self, keep: usize) {
        while self.len() > keep {
            self.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::{LatestReading, TextIds};

    fn node(id: u64, name: &str, caps: Capabilities) -> WireNode {
        WireNode {
            node_id: id,
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

    fn view(id: u64, name: &str, caps: Capabilities, latest: Vec<LatestReading>) -> DeviceView {
        DeviceView {
            device: node(id, name, caps),
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
            rules: TextIds(RuleSet::default()),
            rule_runs: vec![],
            pairing_busy: false,
        }
    }

    fn event(m: &mut Model, e: WireEvent) -> Change {
        m.apply_event(e)
    }

    #[test]
    fn snapshot_replaces_everything() {
        let mut m = Model::default();
        let temp = Capabilities {
            temperature: true,
            battery: true,
            ..Default::default()
        };
        m.apply_snapshot(snapshot(
            5,
            vec![
                view(
                    1,
                    "Salon",
                    temp,
                    vec![reading(Metric::Temperature, 21.0, 50.0)],
                ),
                view(
                    2,
                    "Porte",
                    Capabilities {
                        contact: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ],
        ));
        assert_eq!((m.devices.len(), m.seq), (2, 5));
        assert_eq!(m.devices[&1].temperature().map(|r| r.value), Some(21.0));
        // Un second état complet remplace, il n'ajoute pas.
        m.apply_snapshot(snapshot(
            9,
            vec![view(2, "Porte", Capabilities::default(), vec![])],
        ));
        assert_eq!((m.devices.len(), m.seq), (1, 9));
        assert!(!m.devices.contains_key(&1));
        m.clear();
        assert!(m.devices.is_empty());
    }

    #[test]
    fn older_readings_are_ignored_and_open_since_follows_changes() {
        let mut m = Model::default();
        let door = Capabilities {
            contact: true,
            ..Default::default()
        };
        m.apply_snapshot(snapshot(
            1,
            vec![view(
                3,
                "Porte",
                door,
                vec![reading(Metric::Open, 0.0, 100.0)],
            )],
        ));
        let r = |value, at| WireEvent::Reading {
            node_id: 3,
            metric: Metric::Open,
            value,
            at,
        };
        // Plus ancien que la valeur connue : ni retenu ni rapporté.
        assert_eq!(event(&mut m, r(1.0, 90.0)), Change::Nothing);
        assert_eq!(m.devices[&3].open(), Some(false));
        // Même instant : retenu (le serveur ne renumérote pas ce qu'il rejoue).
        assert!(matches!(
            event(&mut m, r(1.0, 120.0)),
            Change::Reading { .. }
        ));
        assert_eq!(
            (m.devices[&3].open(), m.devices[&3].open_since),
            (Some(true), Some(120.0))
        );
        // Même état répété : l'instant du changement ne bouge pas.
        event(&mut m, r(1.0, 130.0));
        assert_eq!(m.devices[&3].open_since, Some(120.0));
        event(&mut m, r(0.0, 140.0));
        assert_eq!(
            (m.devices[&3].open(), m.devices[&3].open_since),
            (Some(false), Some(140.0))
        );
        // Appareil inconnu : ignoré sans plantage.
        assert_eq!(
            event(
                &mut m,
                WireEvent::Reading {
                    node_id: 99,
                    metric: Metric::Power,
                    value: 1.0,
                    at: 1.0
                }
            ),
            Change::Nothing
        );
    }

    #[test]
    fn status_changes_make_journal_lines_only_on_transitions() {
        let mut m = Model::default();
        m.apply_snapshot(snapshot(
            1,
            vec![view(1, "Salon", Capabilities::default(), vec![])],
        ));
        let down = |n| WireEvent::Status {
            node_id: 1,
            status: WireStatus::Unreachable {
                code: ErrorCode::DeviceUnreachable,
                detail: Some("muet".into()),
                retry_in_s: n,
            },
        };
        assert_eq!(
            event(&mut m, down(5)),
            Change::Journal("Salon injoignable : muet".into())
        );
        assert_eq!(event(&mut m, down(10)), Change::Nothing);
        assert_eq!(
            event(
                &mut m,
                WireEvent::Status {
                    node_id: 1,
                    status: WireStatus::Online
                }
            ),
            Change::Journal("Salon de nouveau en ligne".into())
        );
    }

    #[test]
    fn device_events_pairing_and_runs() {
        let mut m = Model::default();
        let caps = Capabilities {
            power: true,
            on_off: true,
            ..Default::default()
        };
        assert_eq!(
            event(
                &mut m,
                WireEvent::DeviceUpdated {
                    device: node(4, "Prise", caps)
                }
            ),
            Change::DeviceUpdated(4)
        );
        // Une mise à jour (nom) garde les dernières valeurs.
        m.devices
            .get_mut(&4)
            .unwrap()
            .apply_reading(Metric::Power, 85.0, 5.0);
        event(
            &mut m,
            WireEvent::DeviceUpdated {
                device: node(4, "Lampe", caps),
            },
        );
        assert_eq!(m.devices[&4].info.name, "Lampe");
        assert_eq!(m.devices[&4].power().map(|r| r.value), Some(85.0));
        assert!(m.devices[&4].summary().contains("85"));

        event(
            &mut m,
            WireEvent::PairingProgress {
                message: "…".into(),
            },
        );
        assert!(m.pairing_busy);
        event(
            &mut m,
            WireEvent::PairingDone {
                node_id: 4,
                name: "Prise".into(),
            },
        );
        assert!(!m.pairing_busy);
        event(
            &mut m,
            WireEvent::PairingProgress {
                message: "…".into(),
            },
        );
        assert!(matches!(
            event(
                &mut m,
                WireEvent::PairingFailed {
                    code: ErrorCode::PairingFailed,
                    detail: None
                }
            ),
            Change::PairingFailed { .. }
        ));
        assert!(!m.pairing_busy);

        for i in 0..(RULE_RUNS_KEPT + 5) {
            event(
                &mut m,
                WireEvent::RuleRun(RuleRunView {
                    rule_id: 1,
                    name: "r".into(),
                    at: i as f64,
                    ok: true,
                    detail: String::new(),
                }),
            );
        }
        assert_eq!(m.rule_runs.len(), RULE_RUNS_KEPT);
        assert_eq!(m.rule_runs.back().unwrap().at, (RULE_RUNS_KEPT + 4) as f64);
        assert_eq!(
            event(&mut m, WireEvent::DeviceRemoved { node_id: 4 }),
            Change::DeviceRemoved(4)
        );
        assert!(m.devices.is_empty());
    }

    #[test]
    fn slider_draft_survives_a_resync() {
        let mut m = Model::default();
        let cover = Capabilities {
            cover: true,
            cover_position: true,
            ..Default::default()
        };
        m.apply_snapshot(snapshot(1, vec![view(7, "Volet", cover, vec![])]));
        m.devices.get_mut(&7).unwrap().cover_slider = Some(40);
        m.apply_snapshot(snapshot(
            2,
            vec![view(
                7,
                "Volet",
                cover,
                vec![reading(Metric::Position, 10.0, 1.0)],
            )],
        ));
        assert_eq!(m.devices[&7].cover_slider, Some(40));
    }

    #[test]
    fn summaries() {
        let mut d = Device::new(node(
            1,
            "Volet",
            Capabilities {
                cover: true,
                cover_position: true,
                ..Default::default()
            },
        ));
        assert_eq!(d.summary(), "Position inconnue");
        d.apply_reading(Metric::Position, 0.0, 1.0);
        assert_eq!(d.summary(), "Fermé");
        d.cover_motion = Some(CoverMotion::Opening);
        d.apply_reading(Metric::Position, 65.0, 2.0);
        assert!(d.summary().contains("▲ ouverture"));
        d.apply_reading(Metric::Battery, 87.0, 2.0);
        d.apply_reading(Metric::BatteryLow, 1.0, 2.0);
        assert!(d.summary().contains("pile 87 % (faible)"));
    }
}
