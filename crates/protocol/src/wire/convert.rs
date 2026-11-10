//! Passage entre les types internes (`crate::events`) et les types du fil.

use super::error::ErrorCode;
use super::message::{Metric, RuleRunView, WireBridge, WireEvent, WireMode, WireNode, WireStatus};
use crate::events::{
    BridgeLink, Event, MeasureKind, NodeInfo, NodeStatus, ReportingMode, StateKind,
};

impl From<MeasureKind> for Metric {
    fn from(kind: MeasureKind) -> Self {
        match kind {
            MeasureKind::Temperature => Metric::Temperature,
            MeasureKind::Humidity => Metric::Humidity,
            MeasureKind::Power => Metric::Power,
            MeasureKind::Energy => Metric::Energy,
            MeasureKind::Battery => Metric::Battery,
            MeasureKind::Position => Metric::Position,
        }
    }
}

impl From<StateKind> for Metric {
    fn from(kind: StateKind) -> Self {
        match kind {
            StateKind::Open => Metric::Open,
            StateKind::OnOff => Metric::OnOff,
            StateKind::BatteryLow => Metric::BatteryLow,
        }
    }
}

impl From<&NodeInfo> for WireNode {
    fn from(n: &NodeInfo) -> Self {
        WireNode {
            node_id: n.node_id,
            name: n.name.clone(),
            device_name: n.device_name.clone(),
            custom_name: n.custom_name.clone(),
            icon: n.icon,
            vendor: n.vendor.clone(),
            product: n.product.clone(),
            device_types: n.device_types.clone(),
            caps: n.caps,
            bridge: n.bridge.map(|b| WireBridge {
                node_id: b.node_id,
                endpoint: b.endpoint,
            }),
        }
    }
}

impl From<WireNode> for NodeInfo {
    fn from(n: WireNode) -> Self {
        NodeInfo {
            node_id: n.node_id,
            name: n.name,
            device_name: n.device_name,
            custom_name: n.custom_name,
            icon: n.icon,
            vendor: n.vendor,
            product: n.product,
            device_types: n.device_types,
            caps: n.caps,
            bridge: n.bridge.map(|b| BridgeLink {
                node_id: b.node_id,
                endpoint: b.endpoint,
            }),
        }
    }
}

impl WireStatus {
    /// `clean` nettoie le texte venu de la couche Matter (chemins locaux, etc.).
    pub fn from_status(status: &NodeStatus, clean: &dyn Fn(&str) -> String) -> Self {
        match status {
            NodeStatus::Connecting => WireStatus::Connecting,
            NodeStatus::Online => WireStatus::Online,
            NodeStatus::Unreachable { error, retry_in_s } => WireStatus::Unreachable {
                code: ErrorCode::DeviceUnreachable,
                detail: Some(clean(error)).filter(|s| !s.is_empty()),
                retry_in_s: *retry_in_s,
            },
        }
    }
}

impl From<WireStatus> for NodeStatus {
    fn from(status: WireStatus) -> Self {
        match status {
            WireStatus::Connecting => NodeStatus::Connecting,
            WireStatus::Online => NodeStatus::Online,
            WireStatus::Unreachable {
                detail, retry_in_s, ..
            } => NodeStatus::Unreachable {
                error: detail.unwrap_or_default(),
                retry_in_s,
            },
        }
    }
}

impl From<&ReportingMode> for WireMode {
    fn from(mode: &ReportingMode) -> Self {
        match mode {
            ReportingMode::Subscription { max_interval_s } => WireMode::Subscription {
                max_interval_s: *max_interval_s,
            },
            ReportingMode::Polling { every_s } => WireMode::Polling { every_s: *every_s },
        }
    }
}

impl From<WireMode> for ReportingMode {
    fn from(mode: WireMode) -> Self {
        match mode {
            WireMode::Subscription { max_interval_s } => {
                ReportingMode::Subscription { max_interval_s }
            }
            WireMode::Polling { every_s } => ReportingMode::Polling { every_s },
        }
    }
}

impl WireEvent {
    /// Événement du fil correspondant à un événement interne. `None` pour ce qui ne part pas
    /// vers les clients (`Fatal` : le serveur s'arrête). `clean` nettoie les textes libres.
    pub fn from_event(event: &Event, clean: &dyn Fn(&str) -> String) -> Option<WireEvent> {
        Some(match event {
            Event::Node(info) => WireEvent::DeviceUpdated {
                device: info.into(),
            },
            Event::NodeRemoved(id) => WireEvent::DeviceRemoved { node_id: *id },
            Event::Status { node_id, status } => WireEvent::Status {
                node_id: *node_id,
                status: WireStatus::from_status(status, clean),
            },
            Event::Seen { node_id, at } => WireEvent::Seen {
                node_id: *node_id,
                at: *at,
            },
            Event::Mode { node_id, mode } => WireEvent::Mode {
                node_id: *node_id,
                mode: mode.into(),
            },
            Event::Measurement {
                node_id,
                kind,
                value,
                at,
            } => WireEvent::Reading {
                node_id: *node_id,
                metric: (*kind).into(),
                value: *value,
                at: *at,
            },
            Event::State {
                node_id,
                kind,
                value,
                at,
            } => WireEvent::Reading {
                node_id: *node_id,
                metric: (*kind).into(),
                value: f64::from(u8::from(*value)),
                at: *at,
            },
            Event::CoverMotion { node_id, motion } => WireEvent::CoverMotion {
                node_id: *node_id,
                motion: *motion,
            },
            Event::RuleRun {
                rule_id,
                name,
                at,
                ok,
                detail,
            } => WireEvent::RuleRun(RuleRunView {
                rule_id: *rule_id,
                name: name.clone(),
                at: *at,
                ok: *ok,
                detail: clean(detail),
            }),
            Event::Notification { title, message, at } => WireEvent::Notification {
                title: title.clone(),
                message: message.clone(),
                at: *at,
            },
            Event::PairingProgress(message) => WireEvent::PairingProgress {
                message: clean(message),
            },
            Event::PairingDone { node_id, name } => WireEvent::PairingDone {
                node_id: *node_id,
                name: name.clone(),
            },
            Event::PairingFailed(message) => WireEvent::PairingFailed {
                code: ErrorCode::PairingFailed,
                detail: Some(clean(message)).filter(|s| !s.is_empty()),
            },
            Event::Info(text) => WireEvent::Notice { text: clean(text) },
            Event::Fatal(_) => return None,
        })
    }

    /// Événement interne correspondant, pour un client qui réutilise son modèle actuel.
    /// `None` pour `PlanChanged`, `RulesChanged` et `CamerasChanged`, que le client traite à part.
    pub fn into_event(self) -> Option<Event> {
        Some(match self {
            WireEvent::DeviceUpdated { device } => Event::Node(device.into()),
            WireEvent::DeviceRemoved { node_id } => Event::NodeRemoved(node_id),
            WireEvent::Status { node_id, status } => Event::Status {
                node_id,
                status: status.into(),
            },
            WireEvent::Seen { node_id, at } => Event::Seen { node_id, at },
            WireEvent::Mode { node_id, mode } => Event::Mode {
                node_id,
                mode: mode.into(),
            },
            WireEvent::Reading {
                node_id,
                metric,
                value,
                at,
            } => match metric {
                Metric::Temperature => measurement(node_id, MeasureKind::Temperature, value, at),
                Metric::Humidity => measurement(node_id, MeasureKind::Humidity, value, at),
                Metric::Power => measurement(node_id, MeasureKind::Power, value, at),
                Metric::Energy => measurement(node_id, MeasureKind::Energy, value, at),
                Metric::Battery => measurement(node_id, MeasureKind::Battery, value, at),
                Metric::Position => measurement(node_id, MeasureKind::Position, value, at),
                Metric::Open => state(node_id, StateKind::Open, value, at),
                Metric::OnOff => state(node_id, StateKind::OnOff, value, at),
                Metric::BatteryLow => state(node_id, StateKind::BatteryLow, value, at),
            },
            WireEvent::CoverMotion { node_id, motion } => Event::CoverMotion { node_id, motion },
            WireEvent::RuleRun(r) => Event::RuleRun {
                rule_id: r.rule_id,
                name: r.name,
                at: r.at,
                ok: r.ok,
                detail: r.detail,
            },
            WireEvent::PairingProgress { message } => Event::PairingProgress(message),
            WireEvent::PairingDone { node_id, name } => Event::PairingDone { node_id, name },
            WireEvent::PairingFailed { detail, .. } => {
                Event::PairingFailed(detail.unwrap_or_default())
            }
            WireEvent::Notice { text } => Event::Info(text),
            WireEvent::Notification { title, message, at } => {
                Event::Notification { title, message, at }
            }
            WireEvent::PlanChanged { .. }
            | WireEvent::RulesChanged { .. }
            | WireEvent::CamerasChanged { .. }
            | WireEvent::Unknown => return None,
        })
    }
}

fn measurement(node_id: u64, kind: MeasureKind, value: f64, at: f64) -> Event {
    Event::Measurement {
        node_id,
        kind,
        value,
        at,
    }
}

fn state(node_id: u64, kind: StateKind, value: f64, at: f64) -> Event {
    Event::State {
        node_id,
        kind,
        value: value != 0.0,
        at,
    }
}
