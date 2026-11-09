//! Étape 5 : migration d'un `.matterhome` de la v0.2.1 vers le serveur, de bout en bout, avec le
//! vrai binaire et sans aucun réseau.
//!
//! Sécurité : chaque test a son dossier de données et son HOME factices, dans le dossier
//! temporaire. Seules les sous-commandes `create-account`, `import` et `fingerprint` sont
//! lancées : aucune ne démarre le serveur (donc ni Matter, ni mDNS, ni socket). Le serveur
//! « normal » n'est jamais lancé sur un dossier importé.
//!
//! La sauvegarde est fabriquée **à la main**, au format qu'écrit la v0.2.1 (`git show
//! main:src/backup.rs`) : champs dans l'ordre de ses structures, JSON indenté pour les fichiers,
//! JSON compact pour l'enveloppe. Seules les clés et certificats sont de vrais P-256 et de
//! vrais certificats Matter, tirés avec le code de génération d'identité du crate.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::{create_account, temp_dir, PASSWORD, USER};
use matter_home_server::matter::certs::{generate_identity, JsonCertManager};
use matter_home_server::{backup, persistence};
use protocol::automation::RuleSet;

const BIN: &str = env!("CARGO_BIN_EXE_matter-home-server");

/// Identifiants au-delà de 2^53 : un arrondi (JavaScript, `f64`) les corromprait.
const BIG_NODE: u64 = 0x1234_5678_9ABC_DEF0;
const BRIDGE_NODE: u64 = 0x0000_0000_0000_004D;
const BRIDGED_A: u64 = 0xFFFF_FFF0_0000_0001;
const BRIDGED_B: u64 = 0xFFFF_FFF0_0000_0002;

struct Fixture {
    /// Le fichier `.matterhome` tel que la v0.2.1 l'écrit.
    bundle: String,
    controller: String,
    plan: String,
    rules: String,
    history: BTreeMap<String, String>,
    /// Noms des règles à webhook incomplet que la v0.2.1 a enregistrées.
    drafts: Vec<&'static str>,
}

fn fixture() -> Fixture {
    let id = generate_identity().unwrap();
    let controller = format!(
        r#"{{
  "version": 1,
  "identity": {{
    "fabric_id": {fabric},
    "controller_node_id": {ctrl},
    "ca_private_key": "{ca_key}",
    "ca_cert_der": "{ca_cert}",
    "controller_private_key": "{ctrl_key}",
    "controller_cert_der": "{ctrl_cert}"
  }},
  "nodes": [
    {{
      "node_id": {BIG_NODE},
      "name": "Capteur d'ouverture",
      "address": "[fd12:3456:789a::1]:5540",
      "vendor": "Aqara",
      "product": "Door and Window Sensor",
      "device_types": ["Contact Sensor"],
      "temperature_endpoint": null,
      "humidity_endpoint": null,
      "contact_endpoint": 1,
      "battery_endpoint": 0,
      "power_endpoint": null,
      "energy_endpoint": null,
      "on_off_endpoint": null,
      "cover_endpoint": null,
      "cover_position": false,
      "interviewed": true,
      "custom_name": "Porte d'entrée",
      "icon": "Door",
      "interview_version": 3,
      "bridged": []
    }},
    {{
      "node_id": {BRIDGE_NODE},
      "name": "Pont volets",
      "address": "[fd12:3456:789a::4d]:5540",
      "vendor": "Eve",
      "product": "Bridge",
      "device_types": ["Aggregator"],
      "temperature_endpoint": null,
      "humidity_endpoint": null,
      "contact_endpoint": null,
      "battery_endpoint": null,
      "power_endpoint": null,
      "energy_endpoint": null,
      "on_off_endpoint": null,
      "cover_endpoint": null,
      "cover_position": false,
      "interviewed": true,
      "custom_name": null,
      "icon": null,
      "interview_version": 3,
      "bridged": [
        {{
          "device_id": {BRIDGED_A},
          "endpoint": 3,
          "name": "Volet chambre",
          "vendor": "Eve",
          "product": "Shutter",
          "device_types": ["Window Covering"],
          "temperature_endpoint": null,
          "humidity_endpoint": null,
          "contact_endpoint": null,
          "battery_endpoint": null,
          "power_endpoint": null,
          "energy_endpoint": null,
          "on_off_endpoint": null,
          "cover_endpoint": 3,
          "cover_position": true,
          "custom_name": "Volet de la chambre",
          "icon": "Shutter"
        }},
        {{
          "device_id": {BRIDGED_B},
          "endpoint": 4,
          "name": "Volet salon",
          "vendor": null,
          "product": null,
          "device_types": [],
          "temperature_endpoint": null,
          "humidity_endpoint": null,
          "contact_endpoint": null,
          "battery_endpoint": null,
          "power_endpoint": null,
          "energy_endpoint": null,
          "on_off_endpoint": null,
          "cover_endpoint": 4,
          "cover_position": false,
          "custom_name": null,
          "icon": null
        }}
      ]
    }}
  ]
}}"#,
        fabric = id.fabric_id,
        ctrl = id.controller_node_id,
        ca_key = id.ca_private_key,
        ca_cert = id.ca_cert_der,
        ctrl_key = id.controller_private_key,
        ctrl_cert = id.controller_cert_der,
    );

    let plan = format!(
        r#"{{
  "version": 1,
  "floors": [
    {{
      "id": 1,
      "name": "Rez-de-chaussée",
      "rooms": [
        {{ "id": 2, "name": "Salon", "x": 0.0, "y": 0.0, "w": 5.5, "h": 4.0 }},
        {{ "id": 3, "name": "Chambre", "x": 5.5, "y": 0.0, "w": 3.5, "h": 4.0 }}
      ],
      "items": [
        {{ "id": 4, "kind": {{ "type": "Device", "node_id": {BIG_NODE}, "symbol": null }}, "x": 1.25, "y": 3.75 }},
        {{ "id": 5, "kind": {{ "type": "Device", "node_id": {BRIDGED_A}, "symbol": "Shutter" }}, "x": 7.0, "y": 0.25 }},
        {{ "id": 6, "kind": {{ "type": "Decor", "symbol": "Plug", "label": "Télévision" }}, "x": 2.5, "y": 0.5 }}
      ]
    }}
  ]
}}"#
    );

    let drafts = vec![
        "Alerte sans adresse",
        "Prévenir sans http",
        "Prévenir avec espace",
    ];
    let rules = format!(
        r#"{{
  "version": 1,
  "rules": [
    {{
      "id": 1,
      "name": "Porte ouverte la nuit",
      "enabled": true,
      "trigger": {{ "type": "Contact", "node_id": {BIG_NODE}, "open": true }},
      "conditions": [
        {{ "type": "TimeBetween", "from": [22, 0], "to": [7, 0] }}
      ],
      "actions": [
        {{ "type": "Notify", "title": "Porte", "message": "{{appareil}} s'est ouvert à {{heure}}" }}
      ],
      "cooldown_s": 60
    }},
    {{
      "id": 2,
      "name": "Volet le matin",
      "enabled": true,
      "trigger": {{ "type": "Time", "hour": 7, "minute": 30, "days": [true, true, true, true, true, false, false] }},
      "conditions": [],
      "actions": [
        {{ "type": "SetCover", "node_id": {BRIDGED_A}, "command": {{ "Position": 80 }} }},
        {{ "type": "Delay", "seconds": 30 }},
        {{ "type": "Webhook", "url": "http://192.168.1.50:8123/api/webhook/matin", "body": "{{\"regle\":\"{{regle}}\"}}" }}
      ],
      "cooldown_s": 60
    }},
    {{
      "id": 3,
      "name": "{d0}",
      "enabled": true,
      "trigger": {{ "type": "Contact", "node_id": {BIG_NODE}, "open": true }},
      "conditions": [],
      "actions": [
        {{ "type": "Webhook", "url": "", "body": "" }}
      ],
      "cooldown_s": 60
    }},
    {{
      "id": 4,
      "name": "{d1}",
      "enabled": true,
      "trigger": {{ "type": "Threshold", "node_id": {BIG_NODE}, "measure": "Temperature", "above": true, "value": 28.5, "for_s": 300 }},
      "conditions": [],
      "actions": [
        {{ "type": "Notify", "title": "Chaud", "message": "{{valeur}} °C" }},
        {{ "type": "Webhook", "url": "hooks.exemple.fr/chaud", "body": "{{}}" }}
      ],
      "cooldown_s": 600
    }},
    {{
      "id": 5,
      "name": "{d2}",
      "enabled": true,
      "trigger": {{ "type": "Unreachable", "node_id": {BRIDGE_NODE}, "for_s": 3600 }},
      "conditions": [],
      "actions": [
        {{ "type": "Webhook", "url": "http://exemple.fr/pont hors ligne", "body": "" }}
      ],
      "cooldown_s": 60
    }},
    {{
      "id": 6,
      "name": "Éteinte à la main",
      "enabled": false,
      "trigger": {{ "type": "Switch", "node_id": {BIG_NODE}, "on": true }},
      "conditions": [],
      "actions": [
        {{ "type": "SetOnOff", "node_id": {BIG_NODE}, "command": "Toggle" }}
      ],
      "cooldown_s": 60
    }}
  ]
}}"#,
        d0 = drafts[0],
        d1 = drafts[1],
        d2 = drafts[2],
    );

    let mut history = BTreeMap::new();
    for (node, kind, base) in [
        (BIG_NODE, "open", 0.0),
        (BRIDGED_A, "position", 80.0),
        (BRIDGE_NODE, "battery", 90.0),
    ] {
        let mut text = String::new();
        for i in 0..40u32 {
            text.push_str(&format!(
                "{{\"t\":{:.3},\"k\":\"{kind}\",\"v\":{}}}\n",
                1_762_700_000.0 + f64::from(i) * 60.5,
                base + f64::from(i % 3)
            ));
        }
        history.insert(format!("history/{node:016X}.jsonl"), text);
    }

    // L'enveloppe de la v0.2.1 : JSON compact, `files` triés par chemin.
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    files.insert("controller.json".into(), controller.clone());
    files.insert("maison.json".into(), plan.clone());
    files.insert("regles.json".into(), rules.clone());
    files.extend(history.clone());
    let bundle = serde_json::to_string(&serde_json::json!({
        "format": "matter-home-desktop-sauvegarde",
        "version": 1,
        "created": "2026-11-09T09:15:00.123456+01:00",
        "files": files,
    }))
    .unwrap();

    Fixture {
        bundle,
        controller,
        plan,
        rules,
        history,
        drafts,
    }
}

struct Run {
    code: Option<i32>,
    text: String,
}

fn run(data: &Path, args: &[&str], stdin: Stdio) -> Run {
    let out = Command::new(BIN)
        .args(args)
        .arg("--data-dir")
        .arg(data)
        // HOME factice : rien ne doit jamais retomber sur le vrai dossier de données.
        .env("HOME", data.with_file_name("home"))
        .stdin(stdin)
        .output()
        .unwrap();
    Run {
        code: out.status.code(),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// Chemin relatif → octets, pour tous les fichiers sous `dir`.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn siblings(data: &Path) -> Vec<String> {
    let name = data.file_name().unwrap().to_string_lossy().into_owned();
    let mut names: Vec<String> = std::fs::read_dir(data.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&name) && *n != name)
        .collect();
    names.sort();
    names
}

fn lock_path(data: &Path) -> PathBuf {
    data.with_file_name(format!(
        "{}.lock",
        data.file_name().unwrap().to_string_lossy()
    ))
}

/// Une base de travail : `<tmp>/donnees` (dossier de données) et `<tmp>/home`.
fn workspace(tag: &str) -> (PathBuf, PathBuf) {
    let base = temp_dir(tag);
    std::fs::create_dir_all(base.join("home")).unwrap();
    (base.clone(), base.join("donnees"))
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Le cas principal : premier déploiement sur l'ancien Mac. Compte créé, certificat présent,
/// aucun appareil ; la sauvegarde de la v0.2.1 arrive par la sous-commande.
#[test]
fn a_v021_backup_is_imported_on_the_server_with_the_same_identity() {
    let fx = fixture();
    let (base, data) = workspace("migration");
    let file = base.join("matter-home-2026-11-09.matterhome");
    std::fs::write(&file, &fx.bundle).unwrap();
    let file_arg = file.to_str().unwrap();

    // Compte et certificat, comme après « create-account » puis un premier démarrage.
    let (status, text) = create_account(&data, USER, PASSWORD, &[]);
    assert!(status.success(), "{text}");
    matter_home_server::security::tls::load_or_create(&data.join("serveur")).unwrap();
    let fingerprint_before = run(&data, &["fingerprint"], Stdio::null());
    assert_eq!(
        fingerprint_before.code,
        Some(0),
        "{}",
        fingerprint_before.text
    );
    let serveur_before = tree(&data.join("serveur"));
    assert!(serveur_before.contains_key("compte.json"));
    assert!(serveur_before.contains_key("tls-key.pem"));

    // Sans terminal ni --yes : refus, rien n'a bougé.
    let refused = run(&data, &["import", file_arg], Stdio::null());
    assert_eq!(refused.code, Some(1), "{}", refused.text);
    assert!(refused.text.contains("--yes"), "{}", refused.text);
    assert!(refused.text.contains("ARRÊTÉE"), "{}", refused.text);
    assert!(!data.join("controller.json").exists());
    assert!(
        siblings(&data).iter().all(|n| n == "donnees.lock"),
        "{:?}",
        siblings(&data)
    );

    // Un serveur qui tourne tient le verrou : l'import refuse, clairement, sans rien toucher.
    {
        let holder = std::fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path(&data))
            .unwrap();
        holder.try_lock().unwrap();
        let busy = run(&data, &["import", file_arg, "--yes"], Stdio::null());
        assert_eq!(busy.code, Some(1), "{}", busy.text);
        assert!(busy.text.contains("déjà"), "{}", busy.text);
        assert!(busy.text.contains("Arrêtez-le"), "{}", busy.text);
        assert!(!data.join("controller.json").exists());
    }

    // Le vrai import.
    let done = run(&data, &["import", file_arg, "--yes"], Stdio::null());
    assert_eq!(done.code, Some(0), "{}", done.text);
    let t = &done.text;
    assert!(t.contains("4 appareil(s)"), "{t}");
    assert!(t.contains("2 pièce(s)"), "{t}");
    assert!(t.contains("6 automatisation(s)"), "{t}");
    assert!(t.contains("sauvegarde du 2026-11-09"), "{t}");
    assert!(
        t.contains("ARRÊTÉE"),
        "rappel : application de bureau arrêtée\n{t}"
    );
    assert!(t.contains("3 règles désactivée(s)"), "{t}");
    for name in &fx.drafts {
        assert!(t.contains(&format!("« {name} »")), "{name} dans {t}");
    }
    assert!(!t.contains("Écoute"), "aucun serveur démarré\n{t}");

    // Fichiers : l'identité et l'historique sont identiques octet pour octet.
    let after = tree(&data);
    assert_eq!(
        after["controller.json"],
        fx.controller.as_bytes(),
        "controller.json doit rester identique octet pour octet"
    );
    assert_eq!(after["maison.json"], fx.plan.as_bytes());
    for (name, text) in &fx.history {
        assert_eq!(after[name], text.as_bytes(), "{name}");
    }
    assert_eq!(
        after.keys().filter(|k| k.starts_with("history/")).count(),
        fx.history.len()
    );
    // Le compte et le certificat n'ont pas changé : même empreinte, aucun réappairage.
    for (name, bytes) in &serveur_before {
        assert_eq!(&after[&format!("serveur/{name}")], bytes, "serveur/{name}");
    }
    let fingerprint_after = run(&data, &["fingerprint"], Stdio::null());
    assert_eq!(fingerprint_after.text, fingerprint_before.text);

    // Les règles : seul `enabled` des trois brouillons a changé, leur contenu est intact.
    let original: RuleSet = serde_json::from_str(&fx.rules).unwrap();
    let stored: RuleSet = serde_json::from_slice(&after["regles.json"]).unwrap();
    assert_eq!(stored.rules.len(), original.rules.len());
    for (before, now) in original.rules.iter().zip(&stored.rules) {
        let mut expected = before.clone();
        if fx.drafts.contains(&before.name.as_str()) {
            assert!(before.enabled && !now.enabled, "{}", before.name);
            expected.enabled = false;
        }
        assert_eq!(*now, expected, "règle « {} »", before.name);
    }
    // Ce que le serveur validera au PUT suivant : tout passe, brouillons désactivés compris.
    matter_home_server::validate::rules(&stored).unwrap();

    // Droits : fichiers 0600, dossier de données comme celui de départ.
    for rel in ["controller.json", "maison.json", "regles.json"] {
        assert_eq!(mode(&data.join(rel)), 0o600, "{rel}");
    }
    assert_eq!(mode(&data.join("serveur")), 0o700);
    // Rien ne traîne : ni import en attente, ni dossier de travail ; les anciennes données
    // (le dossier avec son compte seul) sont conservées à côté.
    assert!(!data.join("import-en-attente.matterhome").exists());
    let leftovers = siblings(&data);
    assert!(
        leftovers
            .iter()
            .all(|n| n == "donnees.lock" || n.starts_with("donnees.avant-import-")),
        "{leftovers:?}"
    );
    assert_eq!(
        leftovers
            .iter()
            .filter(|n| n.contains("avant-import"))
            .count(),
        1
    );

    // Ce que le serveur chargera : même identité utilisable, mêmes identifiants (> 2^53).
    let state = persistence::load(&data.join("controller.json"))
        .unwrap()
        .expect("controller.json lisible");
    JsonCertManager::from_identity(&state.identity).unwrap();
    let ids = state.device_ids();
    for expected in [BIG_NODE, BRIDGE_NODE, BRIDGED_A, BRIDGED_B] {
        assert!(ids.contains(&expected), "{expected:#x} dans {ids:x?}");
    }
    const { assert!(BIG_NODE > (1u64 << 53) && BRIDGED_A > (1u64 << 53)) };
    // Un export du dossier importé redonne la même sauvegarde (aux règles désactivées près).
    let (exported, summary) = backup::export_to_string(&data).unwrap();
    assert_eq!(summary.devices, 4);
    assert!(backup::inspect_text(&exported)
        .unwrap()
        .disabled_rules
        .is_empty());

    std::fs::remove_dir_all(base).unwrap();
}

/// Dossier de données totalement absent (même pas de compte) : l'import le crée, en 0700, et
/// dit de créer le compte.
#[test]
fn import_works_on_a_missing_data_dir_and_asks_for_the_account() {
    let fx = fixture();
    let (base, data) = workspace("migration-empty");
    let file = base.join("sauvegarde.matterhome");
    std::fs::write(&file, &fx.bundle).unwrap();

    assert!(!data.exists());
    let done = run(
        &data,
        &["import", file.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(done.code, Some(0), "{}", done.text);
    assert!(done.text.contains("create-account"), "{}", done.text);
    assert_eq!(tree(&data)["controller.json"], fx.controller.as_bytes());
    assert_eq!(mode(&data), 0o700);
    assert!(!data.join("serveur").join("compte.json").exists());

    // Le compte se crée ensuite, à côté de l'identité importée, sans la toucher.
    let (status, text) = create_account(&data, USER, PASSWORD, &[]);
    assert!(status.success(), "{text}");
    assert_eq!(tree(&data)["controller.json"], fx.controller.as_bytes());
    std::fs::remove_dir_all(base).unwrap();
}

/// Un fichier qui n'est pas une sauvegarde, ou une option inconnue : refus net, rien ne bouge.
#[test]
fn import_refuses_bad_input_without_touching_anything() {
    let fx = fixture();
    let (base, data) = workspace("migration-bad");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("controller.json"), &fx.controller).unwrap();
    let before = tree(&data);

    let bad = base.join("autre.json");
    std::fs::write(
        &bad,
        r#"{"format":"autre","version":1,"created":"","files":{}}"#,
    )
    .unwrap();
    let r = run(
        &data,
        &["import", bad.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(r.code, Some(1), "{}", r.text);
    assert!(r.text.contains("pas une sauvegarde"), "{}", r.text);

    let r = run(&data, &["import", "--yes"], Stdio::null());
    assert_eq!(r.code, Some(2), "{}", r.text);
    let r = run(&data, &["import", "a", "b", "--yes"], Stdio::null());
    assert_eq!(r.code, Some(2), "{}", r.text);
    let r = run(&data, &["import", "--bidule"], Stdio::null());
    assert_eq!(r.code, Some(2), "{}", r.text);
    let r = run(
        &data,
        &["import", base.join("absent").to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(r.code, Some(1), "{}", r.text);

    assert_eq!(tree(&data), before);
    assert!(
        siblings(&data).iter().all(|n| n.ends_with(".lock")),
        "{:?}",
        siblings(&data)
    );
    std::fs::remove_dir_all(base).unwrap();
}

/// Import coupé entre ses deux renommages, puis `create-account` avant la relance : la reprise
/// se fait d'abord, l'identité reste celle de la sauvegarde, aucun dossier sans identité ne
/// prend la place.
#[test]
fn create_account_after_an_interrupted_import_resumes_it_first() {
    let fx = fixture();
    let (base, data) = workspace("migration-crash");
    let file = base.join("sauvegarde.matterhome");
    std::fs::write(&file, &fx.bundle).unwrap();
    let (status, text) = create_account(&data, USER, PASSWORD, &[]);
    assert!(status.success(), "{text}");
    let done = run(
        &data,
        &["import", file.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(done.code, Some(0), "{}", done.text);

    // État d'une coupure entre les renommages : le nouveau dossier est « complet »
    // (`.import-nouveau`), le dossier de données est absent.
    let complete = data.with_file_name("donnees.import-nouveau");
    std::fs::rename(&data, &complete).unwrap();
    assert!(!data.exists());

    let (status, text) = create_account(&data, "autre", "un-autre-mot-de-passe", &["--replace"]);
    assert!(status.success(), "{text}");
    assert!(!complete.exists(), "la reprise doit avoir eu lieu");
    let after = tree(&data);
    assert_eq!(after["controller.json"], fx.controller.as_bytes());
    for (name, text) in &fx.history {
        assert_eq!(after[name], text.as_bytes(), "{name}");
    }
    assert!(
        siblings(&data).iter().all(|n| !n.contains("sans-identite")),
        "{:?}",
        siblings(&data)
    );
    std::fs::remove_dir_all(base).unwrap();
}
