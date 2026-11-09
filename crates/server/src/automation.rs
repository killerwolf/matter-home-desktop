//! Automatisations « Quand… Si… Alors… ».
//!
//! - Les règles sont sauvegardées dans `regles.json` par le hub du serveur, seul à l'écrire
//!   (rien en mode démo). Les clients les éditent par l'API ; le hub les envoie au moteur
//!   (`Command::SetRules`).
//! - Le moteur tourne dans le runtime tokio du backend : il reçoit une copie de chaque
//!   événement (`EventSink::with_tap`), tient l'état courant des appareils, évalue les
//!   déclencheurs et lance les actions dans des tâches séparées (un délai ne bloque rien).
//! - Les règles s'exécutent tant que le serveur tourne.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use tokio::sync::mpsc;

use crate::events::{now_unix, Event, EventSink, MeasureKind, NodeStatus, StateKind};

pub const FILE_NAME: &str = "regles.json";
pub const DEMO_FILE_NAME: &str = "regles-demo.json";
/// Un événement plus vieux que ça (relecture de l'historique) met à jour l'état sans déclencher.
const STALE_AFTER_S: f64 = 30.0;
const TICK: Duration = Duration::from_secs(5);
/// Une action « Attendre » est bornée à une heure.
const MAX_DELAY_S: u64 = 3600;

pub fn path(demo: bool) -> PathBuf {
    crate::persistence::data_dir().join(if demo { DEMO_FILE_NAME } else { FILE_NAME })
}

// ---------------------------------------------------------------------------
// Modèle (partagé avec le client : crate `protocol`)
// ---------------------------------------------------------------------------

pub use protocol::automation::*;

/// Règles sauvegardées, ou `default` si le fichier n'existe pas encore.
pub fn load(demo: bool, default: impl FnOnce() -> RuleSet) -> (RuleSet, Option<String>) {
    match crate::persistence::load_json::<RuleSet>(&path(demo)) {
        Ok(Some(set)) => (set, None),
        Ok(None) => (default(), None),
        Err(e) => (
            default(),
            Some(format!(
                "Règles illisibles, règles par défaut utilisées : {e:#}"
            )),
        ),
    }
}

// ---------------------------------------------------------------------------
// Moteur (logique pure, testable avec une horloge simulée)
// ---------------------------------------------------------------------------

/// Instant courant : secondes Unix et heure locale.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub now: f64,
    /// Lundi = 0.
    pub weekday: u32,
    pub minute_of_day: u32,
}

impl Clock {
    pub fn local() -> Self {
        use chrono::{Datelike, Timelike};
        let now = chrono::Local::now();
        Self {
            now: now_unix(),
            weekday: now.weekday().num_days_from_monday(),
            minute_of_day: now.hour() * 60 + now.minute(),
        }
    }

    fn hhmm(&self) -> String {
        format!(
            "{:02}:{:02}",
            self.minute_of_day / 60,
            self.minute_of_day % 60
        )
    }
}

#[derive(Default)]
struct NodeState {
    name: String,
    values: HashMap<MeasureKind, f64>,
    open: Option<bool>,
    on: Option<bool>,
    unreachable_since: Option<f64>,
}

#[derive(Default)]
struct RuleState {
    /// Depuis quand la condition du déclencheur (seuil) est vraie.
    true_since: Option<f64>,
    /// Déjà exécutée pour la période en cours (seuil, injoignable).
    done: bool,
    last_run: Option<f64>,
    /// Minute (secondes Unix / 60) de la dernière exécution d'un déclencheur horaire.
    last_minute: Option<i64>,
}

/// Une règle à exécuter, avec le contexte des variables de texte.
#[derive(Debug, Clone)]
pub struct Firing {
    pub rule: Rule,
    pub device: String,
    pub value: String,
    pub time: String,
}

#[derive(Default)]
pub struct Engine {
    rules: Vec<Rule>,
    nodes: HashMap<u64, NodeState>,
    states: HashMap<u64, RuleState>,
}

fn compare(v: f64, above: bool, threshold: f64) -> bool {
    if above {
        v > threshold
    } else {
        v < threshold
    }
}

fn format_measure(kind: MeasureKind, v: f64) -> String {
    use protocol::format::{format_energy, format_humidity, format_power, format_temperature};
    match kind {
        MeasureKind::Temperature => format_temperature(v),
        MeasureKind::Humidity => format_humidity(v),
        MeasureKind::Power => format_power(v),
        MeasureKind::Energy => format_energy(v),
        MeasureKind::Battery => format!("{v:.0} %"),
        MeasureKind::Position => format!("{v:.0} %"),
    }
}

impl Engine {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            ..Default::default()
        }
    }

    pub fn set_rules(&mut self, rules: Vec<Rule>) {
        self.states
            .retain(|id, _| rules.iter().any(|r| r.id == *id));
        // Un déclencheur modifié repart de zéro.
        for r in &rules {
            if self
                .rules
                .iter()
                .find(|o| o.id == r.id)
                .is_some_and(|o| o.trigger != r.trigger)
            {
                self.states.remove(&r.id);
            }
        }
        self.rules = rules;
    }

    pub fn rule(&self, id: u64) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    pub fn device_name(&self, node_id: u64) -> String {
        self.nodes
            .get(&node_id)
            .map(|n| n.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("Nœud {node_id:016X}"))
    }

    fn conditions_hold(&self, rule: &Rule, clock: &Clock) -> bool {
        rule.conditions.iter().all(|c| match c {
            Condition::TimeBetween { from, to } => {
                let f = from.0 as u32 * 60 + from.1 as u32;
                let t = to.0 as u32 * 60 + to.1 as u32;
                let m = clock.minute_of_day;
                if f <= t {
                    m >= f && m < t
                } else {
                    m >= f || m < t
                }
            }
            Condition::Contact { node_id, open } => {
                self.nodes.get(node_id).and_then(|n| n.open) == Some(*open)
            }
            Condition::Switch { node_id, on } => {
                self.nodes.get(node_id).and_then(|n| n.on) == Some(*on)
            }
            Condition::Threshold {
                node_id,
                measure,
                above,
                value,
            } => self
                .nodes
                .get(node_id)
                .and_then(|n| n.values.get(measure))
                .is_some_and(|v| compare(*v, *above, *value)),
        })
    }

    /// Vérifie conditions et anti-rafale, puis prépare l'exécution.
    fn try_fire(
        &mut self,
        rule: &Rule,
        clock: &Clock,
        device: String,
        value: String,
    ) -> Option<Firing> {
        if !rule.enabled || !self.conditions_hold(rule, clock) {
            return None;
        }
        let st = self.states.entry(rule.id).or_default();
        if st
            .last_run
            .is_some_and(|t| clock.now - t < rule.cooldown_s as f64)
        {
            return None;
        }
        st.last_run = Some(clock.now);
        Some(Firing {
            rule: rule.clone(),
            device,
            value,
            time: clock.hhmm(),
        })
    }

    /// Traite un événement du backend ; retourne les règles à exécuter.
    pub fn on_event(&mut self, event: &Event, clock: &Clock) -> Vec<Firing> {
        let mut out = Vec::new();
        match event {
            Event::Node(info) => {
                self.nodes.entry(info.node_id).or_default().name = info.name.clone();
            }
            Event::NodeRemoved(id) => {
                self.nodes.remove(id);
            }
            Event::Status { node_id, status } => {
                let n = self.nodes.entry(*node_id).or_default();
                match status {
                    NodeStatus::Unreachable { .. } => {
                        n.unreachable_since.get_or_insert(clock.now);
                    }
                    NodeStatus::Online => {
                        n.unreachable_since = None;
                        for r in &self.rules {
                            if matches!(r.trigger, Trigger::Unreachable { node_id: id, .. } if id == *node_id)
                            {
                                self.states.entry(r.id).or_default().done = false;
                            }
                        }
                    }
                    NodeStatus::Connecting => {}
                }
            }
            Event::State {
                node_id,
                kind,
                value,
                at,
            } => {
                let fresh = clock.now - at <= STALE_AFTER_S;
                let n = self.nodes.entry(*node_id).or_default();
                let slot = match kind {
                    StateKind::Open => &mut n.open,
                    StateKind::OnOff => &mut n.on,
                    StateKind::BatteryLow => return out,
                };
                let previous = slot.replace(*value);
                // Front uniquement : la première valeur connue n'est pas un changement.
                if !fresh || previous.is_none() || previous == Some(*value) {
                    return out;
                }
                let device = self.device_name(*node_id);
                let rules: Vec<Rule> = self.rules.clone();
                for r in &rules {
                    let (hit, text) = match (&r.trigger, kind) {
                        (Trigger::Contact { node_id: id, open }, StateKind::Open)
                            if id == node_id && open == value =>
                        {
                            (true, if *value { "ouverte" } else { "fermée" })
                        }
                        (Trigger::Switch { node_id: id, on }, StateKind::OnOff)
                            if id == node_id && on == value =>
                        {
                            (true, if *value { "allumée" } else { "éteinte" })
                        }
                        _ => (false, ""),
                    };
                    if hit {
                        out.extend(self.try_fire(r, clock, device.clone(), text.into()));
                    }
                }
            }
            Event::Measurement {
                node_id,
                kind,
                value,
                at,
            } => {
                self.nodes
                    .entry(*node_id)
                    .or_default()
                    .values
                    .insert(*kind, *value);
                if clock.now - at > STALE_AFTER_S {
                    return out; // relecture de l'historique : pas de déclenchement
                }
                let device = self.device_name(*node_id);
                let rules: Vec<Rule> = self.rules.clone();
                for r in &rules {
                    let Trigger::Threshold {
                        node_id: id,
                        measure,
                        above,
                        value: threshold,
                        for_s,
                    } = r.trigger
                    else {
                        continue;
                    };
                    if id != *node_id || measure != *kind {
                        continue;
                    }
                    let st = self.states.entry(r.id).or_default();
                    let pending = if compare(*value, above, threshold) {
                        let since = *st.true_since.get_or_insert(*at);
                        !st.done && clock.now - since >= for_s as f64
                    } else {
                        st.true_since = None;
                        st.done = false;
                        false
                    };
                    if pending {
                        if let Some(f) =
                            self.try_fire(r, clock, device.clone(), format_measure(*kind, *value))
                        {
                            self.states.entry(r.id).or_default().done = true;
                            out.push(f);
                        }
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Appelé toutes les quelques secondes : horaires, durées de seuil, injoignabilité.
    pub fn on_tick(&mut self, clock: &Clock) -> Vec<Firing> {
        let mut out = Vec::new();
        let minute_index = (clock.now / 60.0).floor() as i64;
        let rules: Vec<Rule> = self.rules.clone();
        for r in &rules {
            match r.trigger {
                Trigger::Time { hour, minute, days } => {
                    let due = days[clock.weekday as usize % 7]
                        && clock.minute_of_day == hour as u32 * 60 + minute as u32;
                    let st = self.states.entry(r.id).or_default();
                    if due && st.last_minute != Some(minute_index) {
                        st.last_minute = Some(minute_index);
                        out.extend(self.try_fire(r, clock, String::new(), String::new()));
                    }
                }
                Trigger::Threshold {
                    node_id,
                    measure,
                    for_s,
                    ..
                } if for_s > 0 => {
                    let st = self.states.entry(r.id).or_default();
                    let (since, done) = (st.true_since, st.done);
                    if let (Some(since), false) = (since, done) {
                        if clock.now - since >= for_s as f64 {
                            let value = self
                                .nodes
                                .get(&node_id)
                                .and_then(|n| n.values.get(&measure))
                                .map(|v| format_measure(measure, *v))
                                .unwrap_or_default();
                            if let Some(f) =
                                self.try_fire(r, clock, self.device_name(node_id), value)
                            {
                                self.states.entry(r.id).or_default().done = true;
                                out.push(f);
                            }
                        }
                    }
                }
                Trigger::Unreachable { node_id, for_s } => {
                    let since = self.nodes.get(&node_id).and_then(|n| n.unreachable_since);
                    let done = self.states.get(&r.id).is_some_and(|s| s.done);
                    if let (Some(since), false) = (since, done) {
                        if clock.now - since >= for_s as f64 {
                            if let Some(f) = self.try_fire(
                                r,
                                clock,
                                self.device_name(node_id),
                                "injoignable".into(),
                            ) {
                                self.states.entry(r.id).or_default().done = true;
                                out.push(f);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Exécution des actions
// ---------------------------------------------------------------------------

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Ce qui pilote réellement les appareils : Matter, ou la simulation du mode démo.
pub trait Actuator: Send + Sync + 'static {
    fn set_on_off(&self, node_id: u64, command: OnOffCommand) -> BoxFuture<'_, Result<()>>;
    fn set_cover(&self, node_id: u64, command: CoverCommand) -> BoxFuture<'_, Result<()>>;
}

pub fn render(template: &str, f: &Firing) -> String {
    template
        .replace("{appareil}", &f.device)
        .replace("{valeur}", &f.value)
        .replace("{heure}", &f.time)
        .replace("{regle}", &f.rule.name)
}

// Même contrôle que celui du client, qui ne doit pas envoyer une adresse que le serveur refuse.
pub use protocol::automation::is_webhook_url;

/// Arguments de `curl` pour un webhook. Rien ici n'est interprété par curl comme une option ou
/// un fichier :
/// - `-q` en tout premier ignore `~/.curlrc` (il ne serait pas lu s'il venait après) ;
/// - `--data-raw` envoie le corps tel quel (`--data-binary` lirait un fichier du Mac si le
///   corps commençait par `@`) ;
/// - l'adresse suit `--url` (jamais lue comme une option) et `-g` désactive les `{}` et `[]` ;
/// - seuls http et https, y compris après une redirection (qui n'est de toute façon pas suivie).
fn curl_args(url: &str, body: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "-q",
        "-sS",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "-m",
        "10",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "-g",
    ]
    .map(String::from)
    .to_vec();
    if !body.is_empty() {
        let content_type = if body.trim_start().starts_with('{') {
            "application/json"
        } else {
            "text/plain"
        };
        args.extend([
            "-X".to_string(),
            "POST".to_string(),
            "-H".to_string(),
            format!("Content-Type: {content_type}"),
            "--data-raw".to_string(),
            body.to_string(),
        ]);
    }
    args.extend(["--url".to_string(), url.to_string()]);
    args
}

/// Appel HTTP via `curl` (présent sur macOS), arguments passés sans shell.
async fn webhook(url: String, body: String) -> Result<String> {
    if !is_webhook_url(&url) {
        bail!("adresse invalide (http:// ou https:// attendu, sans espace)");
    }
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("curl")
            .args(curl_args(&url, &body))
            .output()
    })
    .await??;
    let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    if !code.starts_with('2') {
        bail!("réponse HTTP {code}");
    }
    Ok(code)
}

/// Exécute les actions d'une règle dans l'ordre et rend compte à l'interface.
async fn run(
    firing: Firing,
    actuator: Arc<dyn Actuator>,
    ui: EventSink,
    device_names: HashMap<u64, String>,
) {
    let mut lines = Vec::new();
    let mut ok = true;
    for action in &firing.rule.actions {
        let result: Result<String> = match action {
            // Pas de notification du système : le serveur n'a pas d'écran. Elle part vers les
            // clients connectés (un client fermé ne la reçoit pas).
            Action::Notify { title, message } => {
                ui.send(Event::Notification {
                    title: render(title, &firing),
                    message: render(message, &firing),
                    at: now_unix(),
                });
                Ok("notification transmise aux clients connectés".to_string())
            }
            Action::SetOnOff { node_id, command } => {
                let name = device_names
                    .get(node_id)
                    .cloned()
                    .unwrap_or_else(|| format!("{node_id:016X}"));
                actuator
                    .set_on_off(*node_id, *command)
                    .await
                    .map(|_| format!("{} : {}", name, command.label().to_lowercase()))
                    .with_context(|| format!("{} : {}", name, command.label().to_lowercase()))
            }
            Action::SetCover { node_id, command } => {
                let name = device_names
                    .get(node_id)
                    .cloned()
                    .unwrap_or_else(|| format!("{node_id:016X}"));
                let what = format!("{} : {}", name, command.label().to_lowercase());
                actuator
                    .set_cover(*node_id, *command)
                    .await
                    .map(|_| what.clone())
                    .with_context(|| what)
            }
            Action::Webhook { url, body } => webhook(render(url, &firing), render(body, &firing))
                .await
                .map(|code| format!("webhook : HTTP {code}"))
                .context("webhook"),
            Action::Delay { seconds } => {
                let s = (*seconds).min(MAX_DELAY_S);
                tokio::time::sleep(Duration::from_secs(s)).await;
                Ok(format!("attente {s} s"))
            }
        };
        match result {
            Ok(line) => lines.push(line),
            Err(e) => {
                ok = false;
                lines.push(format!("échec : {e:#}"));
            }
        }
    }
    if firing.rule.actions.is_empty() {
        lines.push("aucune action".into());
    }
    let detail = lines.join(" ; ");
    log::info!("règle « {} » : {detail}", firing.rule.name);
    ui.send(Event::RuleRun {
        rule_id: firing.rule.id,
        name: firing.rule.name.clone(),
        at: now_unix(),
        ok,
        detail,
    });
}

pub enum EngineCommand {
    SetRules(Vec<Rule>),
    Test(u64),
}

/// Lance le moteur dans le runtime courant.
/// `events` : copie des événements du backend ; `ui` : envoi vers l'interface, sans copie.
pub fn spawn(
    rules: Vec<Rule>,
    mut events: mpsc::UnboundedReceiver<Event>,
    mut commands: mpsc::UnboundedReceiver<EngineCommand>,
    ui: EventSink,
    actuator: Arc<dyn Actuator>,
) {
    tokio::spawn(async move {
        let mut engine = Engine::new(rules);
        let mut tick = tokio::time::interval(TICK);
        loop {
            let firings = tokio::select! {
                ev = events.recv() => match ev {
                    Some(ev) => engine.on_event(&ev, &Clock::local()),
                    None => break,
                },
                cmd = commands.recv() => match cmd {
                    Some(EngineCommand::SetRules(rules)) => { engine.set_rules(rules); Vec::new() }
                    Some(EngineCommand::Test(id)) => engine
                        .rule(id)
                        .cloned()
                        .map(|rule| Firing { rule, device: "test".into(), value: "test".into(), time: Clock::local().hhmm() })
                        .into_iter()
                        .collect(),
                    None => break,
                },
                _ = tick.tick() => engine.on_tick(&Clock::local()),
            };
            for f in firings {
                let names = engine
                    .nodes
                    .iter()
                    .map(|(id, n)| (*id, n.name.clone()))
                    .collect();
                tokio::spawn(run(f, actuator.clone(), ui.clone(), names));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NodeInfo;

    #[test]
    fn curl_ignores_curlrc_and_never_reads_files() {
        let args = curl_args("http://127.0.0.1/", "@/etc/hosts");
        assert_eq!(args[0], "-q", "-q doit être le premier argument");
        assert!(args.contains(&"--data-raw".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--data-binary")
            || a == "-K"
            || a == "--config"
            || a == "-d"
            || a == "--data"));
        assert!(args.windows(2).any(|w| w == ["--proto", "=http,https"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--proto-redir", "=http,https"]));
        // L'adresse suit toujours `--url`, même si elle ressemble à une option.
        let args = curl_args("http://a/ -K /etc/passwd", "");
        assert_eq!(args[args.len() - 2], "--url");
        assert!(!is_webhook_url("http://a/ -K /etc/passwd"));
        assert!(!is_webhook_url("-K http://a/"));
        assert!(!is_webhook_url("file:///etc/passwd"));
        assert!(!is_webhook_url("http://a/\nb"));
        assert!(!is_webhook_url("http://a/\u{0}b"));
        assert!(is_webhook_url("https://exemple.fr/a?b=1"));
    }

    /// Un corps qui commence par `@` part tel quel : curl ne lit pas le fichier désigné.
    #[test]
    fn webhook_body_starting_with_at_is_sent_literally() {
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("mhd-webhook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = dir.join("secret.txt");
        std::fs::write(&secret, "CONTENU-SECRET-DU-MAC").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&received).contains("@/") && received.ends_with(b"txt") {
                    break;
                }
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            String::from_utf8_lossy(&received).into_owned()
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let body = format!("@{}", secret.display());
        let code = runtime
            .block_on(webhook(format!("http://127.0.0.1:{port}/"), body.clone()))
            .unwrap();
        assert_eq!(code, "200");
        let request = server.join().unwrap();
        assert!(request.contains(&body), "{request}");
        assert!(!request.contains("CONTENU-SECRET-DU-MAC"), "{request}");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn clock(now: f64, minute_of_day: u32) -> Clock {
        Clock {
            now,
            weekday: 2,
            minute_of_day,
        }
    }

    fn rule(id: u64, trigger: Trigger) -> Rule {
        Rule {
            cooldown_s: 0,
            ..Rule {
                trigger,
                ..Rule::new(id)
            }
        }
    }

    fn open(node_id: u64, value: bool, at: f64) -> Event {
        Event::State {
            node_id,
            kind: StateKind::Open,
            value,
            at,
        }
    }

    fn temp(node_id: u64, value: f64, at: f64) -> Event {
        Event::Measurement {
            node_id,
            kind: MeasureKind::Temperature,
            value,
            at,
        }
    }

    #[test]
    fn contact_fires_on_edges_only() {
        let mut e = Engine::new(vec![rule(
            1,
            Trigger::Contact {
                node_id: 7,
                open: true,
            },
        )]);
        e.on_event(
            &Event::Node(NodeInfo {
                node_id: 7,
                name: "Porte".into(),
                ..Default::default()
            }),
            &clock(0.0, 0),
        );
        // Première valeur : initialisation, pas de front.
        assert!(e
            .on_event(&open(7, true, 100.0), &clock(100.0, 0))
            .is_empty());
        assert!(e
            .on_event(&open(7, false, 110.0), &clock(110.0, 0))
            .is_empty());
        let fired = e.on_event(&open(7, true, 120.0), &clock(120.0, 0));
        assert_eq!(fired.len(), 1);
        assert_eq!(
            (fired[0].device.as_str(), fired[0].value.as_str()),
            ("Porte", "ouverte")
        );
        // Même valeur répétée (rapport de souscription) : rien.
        assert!(e
            .on_event(&open(7, true, 130.0), &clock(130.0, 0))
            .is_empty());
    }

    #[test]
    fn stale_events_do_not_fire() {
        let mut e = Engine::new(vec![rule(
            1,
            Trigger::Contact {
                node_id: 7,
                open: true,
            },
        )]);
        e.on_event(&open(7, false, 0.0), &clock(1000.0, 0));
        assert!(
            e.on_event(&open(7, true, 10.0), &clock(1000.0, 0))
                .is_empty(),
            "relecture de l'historique"
        );
        // L'état a quand même été mis à jour.
        assert_eq!(e.nodes[&7].open, Some(true));
    }

    #[test]
    fn threshold_with_duration_fires_once_per_period() {
        let trig = Trigger::Threshold {
            node_id: 3,
            measure: MeasureKind::Temperature,
            above: true,
            value: 26.0,
            for_s: 60,
        };
        let mut e = Engine::new(vec![rule(1, trig)]);
        assert!(
            e.on_event(&temp(3, 27.0, 1000.0), &clock(1000.0, 0))
                .is_empty(),
            "pas encore 60 s"
        );
        assert!(e.on_tick(&clock(1030.0, 0)).is_empty());
        assert_eq!(
            e.on_tick(&clock(1061.0, 0)).len(),
            1,
            "60 s au-dessus : déclenche"
        );
        assert!(
            e.on_tick(&clock(1100.0, 0)).is_empty(),
            "une seule fois par période"
        );
        assert!(e
            .on_event(&temp(3, 25.0, 1200.0), &clock(1200.0, 0))
            .is_empty());
        e.on_event(&temp(3, 26.5, 1300.0), &clock(1300.0, 0));
        assert_eq!(e.on_tick(&clock(1361.0, 0)).len(), 1, "nouvelle période");
    }

    #[test]
    fn threshold_without_duration_and_below() {
        let trig = Trigger::Threshold {
            node_id: 3,
            measure: MeasureKind::Battery,
            above: false,
            value: 20.0,
            for_s: 0,
        };
        let mut e = Engine::new(vec![rule(1, trig)]);
        let ev = |v: f64, t: f64| Event::Measurement {
            node_id: 3,
            kind: MeasureKind::Battery,
            value: v,
            at: t,
        };
        assert!(e.on_event(&ev(50.0, 10.0), &clock(10.0, 0)).is_empty());
        let fired = e.on_event(&ev(15.0, 20.0), &clock(20.0, 0));
        assert_eq!(fired[0].value, "15 %");
    }

    #[test]
    fn conditions_and_cooldown() {
        let mut r = rule(
            1,
            Trigger::Contact {
                node_id: 7,
                open: true,
            },
        );
        r.conditions = vec![Condition::TimeBetween {
            from: (22, 0),
            to: (7, 0),
        }];
        r.cooldown_s = 300;
        let mut e = Engine::new(vec![r]);
        e.on_event(&open(7, false, 0.0), &clock(0.0, 0));
        assert!(
            e.on_event(&open(7, true, 10.0), &clock(10.0, 12 * 60))
                .is_empty(),
            "midi : hors plage"
        );
        e.on_event(&open(7, false, 20.0), &clock(20.0, 23 * 60));
        assert_eq!(
            e.on_event(&open(7, true, 30.0), &clock(30.0, 23 * 60))
                .len(),
            1,
            "23 h : dans la plage"
        );
        e.on_event(&open(7, false, 40.0), &clock(40.0, 23 * 60));
        assert!(
            e.on_event(&open(7, true, 50.0), &clock(50.0, 23 * 60))
                .is_empty(),
            "anti-rafale"
        );
        e.on_event(&open(7, false, 400.0), &clock(400.0, 2 * 60));
        assert_eq!(
            e.on_event(&open(7, true, 410.0), &clock(410.0, 2 * 60))
                .len(),
            1,
            "2 h du matin, après 300 s"
        );
    }

    #[test]
    fn time_trigger_once_per_minute_and_days() {
        let mut days = [false; 7];
        days[2] = true; // mercredi
        let mut e = Engine::new(vec![rule(
            1,
            Trigger::Time {
                hour: 7,
                minute: 30,
                days,
            },
        )]);
        assert!(e.on_tick(&clock(60.0 * 1000.0, 7 * 60 + 29)).is_empty());
        assert_eq!(e.on_tick(&clock(60.0 * 1001.0, 7 * 60 + 30)).len(), 1);
        assert!(
            e.on_tick(&clock(60.0 * 1001.0 + 5.0, 7 * 60 + 30))
                .is_empty(),
            "même minute"
        );
        let thursday = Clock {
            now: 60.0 * 3000.0,
            weekday: 3,
            minute_of_day: 7 * 60 + 30,
        };
        assert!(e.on_tick(&thursday).is_empty(), "jour non coché");
    }

    #[test]
    fn unreachable_for_duration() {
        let mut e = Engine::new(vec![rule(
            1,
            Trigger::Unreachable {
                node_id: 5,
                for_s: 600,
            },
        )]);
        let st = |s: NodeStatus| Event::Status {
            node_id: 5,
            status: s,
        };
        e.on_event(
            &st(NodeStatus::Unreachable {
                error: "x".into(),
                retry_in_s: 5,
            }),
            &clock(0.0, 0),
        );
        assert!(e.on_tick(&clock(300.0, 0)).is_empty());
        assert_eq!(e.on_tick(&clock(601.0, 0)).len(), 1);
        assert!(e.on_tick(&clock(1200.0, 0)).is_empty());
        e.on_event(&st(NodeStatus::Online), &clock(1300.0, 0));
        e.on_event(
            &st(NodeStatus::Unreachable {
                error: "x".into(),
                retry_in_s: 5,
            }),
            &clock(1400.0, 0),
        );
        assert_eq!(e.on_tick(&clock(2001.0, 0)).len(), 1);
    }

    #[test]
    fn disabled_rule_never_fires() {
        let mut r = rule(
            1,
            Trigger::Contact {
                node_id: 7,
                open: true,
            },
        );
        r.enabled = false;
        let mut e = Engine::new(vec![r]);
        e.on_event(&open(7, false, 0.0), &clock(0.0, 0));
        assert!(e.on_event(&open(7, true, 1.0), &clock(1.0, 0)).is_empty());
    }

    #[test]
    fn template_rendering() {
        let f = Firing {
            rule: Rule::new(1),
            device: "Porte d'entrée".into(),
            value: "ouverte".into(),
            time: "22:14".into(),
        };
        assert_eq!(
            render("{appareil} {valeur} à {heure} ({regle})", &f),
            "Porte d'entrée ouverte à 22:14 (Nouvelle règle)"
        );
    }

    #[test]
    fn json_round_trip() {
        let mut set = RuleSet::default();
        let mut r = Rule::new(set.next_id());
        r.trigger = Trigger::Threshold {
            node_id: 1,
            measure: MeasureKind::Power,
            above: true,
            value: 50.0,
            for_s: 600,
        };
        r.conditions.push(Condition::Switch {
            node_id: 2,
            on: true,
        });
        r.actions.push(Action::SetOnOff {
            node_id: 2,
            command: OnOffCommand::Off,
        });
        r.actions.push(Action::Webhook {
            url: "https://example.org/x".into(),
            body: String::new(),
        });
        r.actions.push(Action::SetCover {
            node_id: 3,
            command: CoverCommand::Position(40),
        });
        set.rules.push(r);
        let text = serde_json::to_string(&set).unwrap();
        assert!(text.contains("\"type\":\"Threshold\""));
        assert!(text.contains("\"Position\":40"), "{text}");
        assert_eq!(serde_json::from_str::<RuleSet>(&text).unwrap(), set);
        assert_eq!(set.next_id(), 2);
    }
}
