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
        {{ "type": "Webhook", "url": "https://exemple.invalid/api/webhook/matin", "body": "{{\"regle\":\"{{regle}}\"}}" }}
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
    assert!(
        refused.text.contains("y compris celui-ci"),
        "{}",
        refused.text
    );
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
        t.contains("TOUS vos Mac, y compris celui-ci") && t.contains("données les plus récentes"),
        "rappel : v0.2.1 arrêtée partout\n{t}"
    );
    assert!(t.contains("Identité de la sauvegarde : fabric"), "{t}");
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

    // `fingerprint` ne ment pas : il dit qu'un import attend d'être repris.
    let fp = run(&data, &["fingerprint"], Stdio::null());
    assert_eq!(fp.code, Some(1), "{}", fp.text);
    assert!(
        fp.text.contains("import interrompu") && !fp.text.contains("jamais été lancé"),
        "{}",
        fp.text
    );
    assert!(!data.exists(), "fingerprint ne touche à rien");

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

/// Lance le binaire avec des arguments bruts, dans `cwd`, avec un HOME factice.
fn run_raw(cwd: &Path, args: &[&str]) -> Run {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", cwd.join("home"))
        .stdin(Stdio::null())
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

/// `--data-dir donnees` ou `donnees/` (chemin relatif d'un seul élément) : le parent est « . »,
/// que le système ne sait pas synchroniser. Le chemin est rendu absolu.
#[test]
fn a_relative_data_dir_works_with_or_without_a_trailing_slash() {
    let fx = fixture();
    for (tag, arg) in [("rel", "donnees"), ("rel-slash", "donnees/")] {
        let (base, data) = workspace(tag);
        let file = base.join("sauvegarde.matterhome");
        std::fs::write(&file, &fx.bundle).unwrap();
        // Un compte d'abord, pour qu'il y ait un dossier existant à mettre de côté.
        let r = run_raw(&base, &["fingerprint", "--data-dir", arg]);
        assert_eq!(r.code, Some(1), "{}", r.text);
        let done = run_raw(
            &base,
            &["import", file.to_str().unwrap(), "--yes", "--data-dir", arg],
        );
        assert_eq!(done.code, Some(0), "{arg} : {}", done.text);
        assert!(done.text.contains(base.to_str().unwrap()), "{}", done.text);
        assert_eq!(tree(&data)["controller.json"], fx.controller.as_bytes());
        // Deuxième import, cette fois sur un dossier existant : mis de côté, sans erreur de
        // synchronisation d'un chemin vide.
        let again = run_raw(
            &base,
            &["import", file.to_str().unwrap(), "--yes", "--data-dir", arg],
        );
        assert_eq!(again.code, Some(0), "{arg} : {}", again.text);
        assert!(
            siblings(&data).iter().any(|n| n.contains("avant-import")),
            "{:?}",
            siblings(&data)
        );
        assert_eq!(mode(&data), 0o700);
        std::fs::remove_dir_all(base).unwrap();
    }
}

/// `--yes` seul ne remplace pas une identité différente : les deux fabrics et les nœuds sont
/// affichés, il faut `--replace-identity`, et rien n'est écrasé sans lui.
#[test]
fn a_different_identity_needs_replace_identity_even_with_yes() {
    let first = fixture();
    let second = fixture();
    let (base, data) = workspace("migration-identity");
    let (f1, f2) = (base.join("un.matterhome"), base.join("deux.matterhome"));
    std::fs::write(&f1, &first.bundle).unwrap();
    std::fs::write(&f2, &second.bundle).unwrap();
    let r = run(
        &data,
        &["import", f1.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(r.code, Some(0), "{}", r.text);
    let before = tree(&data);

    let refused = run(
        &data,
        &["import", f2.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(refused.code, Some(1), "{}", refused.text);
    let t = &refused.text;
    assert!(t.contains("--replace-identity"), "{t}");
    assert!(t.contains("Identité de la sauvegarde : fabric 0x"), "{t}");
    assert!(
        t.contains("Identité actuelle du dossier : fabric 0x"),
        "{t}"
    );
    assert!(
        t.contains("Capteur d'ouverture") || t.contains("Porte d'entrée"),
        "{t}"
    );
    let fabric_of = |fx: &Fixture| {
        let state: persistence::StoredState = serde_json::from_str(&fx.controller).unwrap();
        format!("{:#x}", state.identity.fabric_id)
    };
    assert!(
        t.contains(&fabric_of(&first)) && t.contains(&fabric_of(&second)),
        "{t}"
    );
    assert_eq!(tree(&data), before, "rien n'est écrasé sans l'option");
    assert_eq!(
        siblings(&data)
            .iter()
            .filter(|n| n.contains("avant-import"))
            .count(),
        1
    );

    // La même identité se réimporte avec --yes seul.
    let same = run(
        &data,
        &["import", f1.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(same.code, Some(0), "{}", same.text);
    assert!(same.text.contains("Même fabric"), "{}", same.text);

    // Avec l'option dédiée, l'identité est remplacée (l'ancien dossier est conservé).
    let done = run(
        &data,
        &[
            "import",
            f2.to_str().unwrap(),
            "--yes",
            "--replace-identity",
        ],
        Stdio::null(),
    );
    assert_eq!(done.code, Some(0), "{}", done.text);
    assert_eq!(tree(&data)["controller.json"], second.controller.as_bytes());
    std::fs::remove_dir_all(base).unwrap();
}

/// Relancer `import` reprend un import interrompu de ce même fichier ; une autre sauvegarde en
/// attente est refusée ; `import --abandon` y renonce et met de côté le dossier préparé.
#[test]
fn import_resumes_the_same_file_and_can_abandon_another() {
    let fx = fixture();
    let (base, data) = workspace("migration-resume");
    let file = base.join("sauvegarde.matterhome");
    std::fs::write(&file, &fx.bundle).unwrap();
    let r = run(
        &data,
        &["import", file.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(r.code, Some(0), "{}", r.text);

    // Coupure juste après la mise de côté de la sauvegarde : elle est restée en attente.
    backup::stage_import_text(&data, &fx.bundle).unwrap();
    let resumed = run(
        &data,
        &["import", file.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(resumed.code, Some(0), "{}", resumed.text);
    assert!(resumed.text.contains("repris"), "{}", resumed.text);
    assert!(!data.join("import-en-attente.matterhome").exists());

    // Une autre sauvegarde (celle de l'API, par exemple) n'est pas la nôtre : refus clair.
    let other = fixture();
    backup::stage_import_text(&data, &other.bundle).unwrap();
    let refused = run(
        &data,
        &["import", file.to_str().unwrap(), "--yes"],
        Stdio::null(),
    );
    assert_eq!(refused.code, Some(1), "{}", refused.text);
    assert!(refused.text.contains("--abandon"), "{}", refused.text);
    assert!(
        refused.text.contains("import-abandonne"),
        "{}",
        refused.text
    );
    assert!(data.join("import-en-attente.matterhome").exists());

    // Dossier complet déjà préparé, comme après une coupure : renoncer le met de côté.
    let complete = data.with_file_name("donnees.import-nouveau");
    std::fs::create_dir_all(&complete).unwrap();
    std::fs::write(complete.join("controller.json"), &other.controller).unwrap();
    let before = tree(&data);
    let r = run(&data, &["import", "--abandon"], Stdio::null());
    assert_eq!(r.code, Some(0), "{}", r.text);
    assert!(
        r.text.contains("retirée") && r.text.contains("import-abandonne"),
        "{}",
        r.text
    );
    assert!(!complete.exists());
    let aside: Vec<String> = siblings(&data)
        .into_iter()
        .filter(|n| n.contains("import-abandonne"))
        .collect();
    assert_eq!(aside.len(), 1, "{:?}", siblings(&data));
    let kept = data.with_file_name(&aside[0]).join("controller.json");
    assert_eq!(std::fs::read(kept).unwrap(), other.controller.as_bytes());
    let mut expected = before;
    expected.remove("import-en-attente.matterhome");
    assert_eq!(
        tree(&data),
        expected,
        "les données actuelles ne bougent pas"
    );
    // Rien à abandonner ensuite ; un fichier avec --abandon est une erreur d'usage.
    let r = run(&data, &["import", "--abandon"], Stdio::null());
    assert_eq!(r.code, Some(0));
    assert!(r.text.contains("rien à abandonner"), "{}", r.text);
    let r = run(
        &data,
        &["import", "--abandon", file.to_str().unwrap()],
        Stdio::null(),
    );
    assert_eq!(r.code, Some(2), "{}", r.text);
    std::fs::remove_dir_all(base).unwrap();
}

/// Installation neuve : `init` crée l'identité (jamais par accident), et refuse quand une
/// identité existe, quand des données existent sans identité, ou quand un import en a laissé une
/// à côté. `--require-identity` refuse de démarrer sans identité (code 78), journal à droits 0600.
#[test]
fn init_and_require_identity_never_create_a_fabric_by_accident() {
    let fx = fixture();
    let (base, data) = workspace("migration-init");

    // Pas d'identité : le service refuse de démarrer (avant tout réseau), code 78, et le dit
    // dans son journal.
    let log = base.join("logs").join("server.log");
    let r = run_raw(
        &base,
        &[
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            data.to_str().unwrap(),
            "--log-file",
            log.to_str().unwrap(),
        ],
    );
    assert_eq!(r.code, Some(78), "{}", r.text);
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains("aucune identité Matter") && logged.contains("init"),
        "{logged}"
    );
    assert_eq!(mode(&log), 0o600);
    assert_eq!(mode(log.parent().unwrap()), 0o700);
    assert!(!data.join("controller.json").exists(), "rien n'a été créé");

    // Le même refus sans option (le comportement par défaut), et avec l'ancienne option, qui
    // reste acceptée sans effet.
    for extra in [&[][..], &["--require-identity"][..]] {
        let mut args = vec![
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            data.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let r = run_raw(&base, &args);
        assert_eq!(r.code, Some(78), "{extra:?} : {}", r.text);
    }
    assert!(!data.exists(), "rien n'a été créé");

    // Un import en attente interdit `init`.
    std::fs::create_dir_all(&data).unwrap();
    backup::stage_import_text(&data, &fx.bundle).unwrap();
    let r = run(&data, &["init"], Stdio::null());
    assert_eq!(r.code, Some(1), "{}", r.text);
    assert!(r.text.contains("import est en attente"), "{}", r.text);
    assert!(!data.join("controller.json").exists());
    std::fs::remove_dir_all(&data).unwrap();

    // Un voisin « avant-import » avec une identité interdit `init`.
    let neighbour = data.with_file_name("donnees.avant-import-20261101-120000");
    std::fs::create_dir_all(&neighbour).unwrap();
    std::fs::write(neighbour.join("controller.json"), &fx.controller).unwrap();
    let r = run(&data, &["init"], Stdio::null());
    assert_eq!(r.code, Some(1), "{}", r.text);
    assert!(
        r.text.contains("nouvelle fabric") && r.text.contains("avant-import"),
        "{}",
        r.text
    );
    assert!(!data.join("controller.json").exists());
    std::fs::remove_dir_all(&neighbour).unwrap();

    // Des données sans identité l'interdisent aussi.
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("maison.json"), "{}").unwrap();
    let r = run(&data, &["init"], Stdio::null());
    assert_eq!(r.code, Some(1), "{}", r.text);
    assert!(r.text.contains("maison.json"), "{}", r.text);
    std::fs::remove_file(data.join("maison.json")).unwrap();

    // Une installation neuve : identité créée, dossier en 0700, fichier en 0600.
    let r = run(&data, &["init"], Stdio::null());
    assert_eq!(r.code, Some(0), "{}", r.text);
    assert!(r.text.contains("create-account"), "{}", r.text);
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&data.join("controller.json")), 0o600);
    let state = persistence::load(&data.join("controller.json"))
        .unwrap()
        .unwrap();
    JsonCertManager::from_identity(&state.identity).unwrap();
    // Une seconde fois : rien n'est refait.
    let before = tree(&data);
    let r = run(&data, &["init"], Stdio::null());
    assert_eq!(r.code, Some(1), "{}", r.text);
    assert!(r.text.contains("existe déjà"), "{}", r.text);
    assert_eq!(tree(&data), before);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn the_server_announces_its_version() {
    let r = run_raw(&std::env::temp_dir(), &["--version"]);
    assert_eq!(r.code, Some(0));
    assert_eq!(
        r.text.trim(),
        format!("matter-home-server {}", env!("CARGO_PKG_VERSION"))
    );
}

/// Jamais en root : `import`, `init`, `create-account` et le serveur refusent (root simulé par
/// la variable d'essai), sans toucher à rien ; `fingerprint` et `--version` restent possibles.
#[test]
fn write_commands_and_the_server_refuse_to_run_as_root() {
    let fx = fixture();
    let (base, data) = workspace("migration-root");
    let file = base.join("sauvegarde.matterhome");
    std::fs::write(&file, &fx.bundle).unwrap();
    let as_root = |args: &[&str]| {
        let out = Command::new(BIN)
            .args(args)
            .arg("--data-dir")
            .arg(&data)
            .env("HOME", base.join("home"))
            .env("MATTER_HOME_TEST_FAKE_ROOT", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    for args in [
        vec!["import", file.to_str().unwrap(), "--yes"],
        vec!["import", "--abandon"],
        vec!["init"],
        vec!["create-account", "--username", "x", "--password-stdin"],
        vec!["--listen", "127.0.0.1:0"],
    ] {
        let (code, text) = as_root(&args);
        assert_eq!(code, Some(1), "{args:?} : {text}");
        assert!(
            text.contains("root") && text.contains("sans sudo"),
            "{args:?} : {text}"
        );
    }
    assert!(
        !data.exists() && siblings(&data).is_empty(),
        "rien n'a été créé"
    );
    // La démo n'écrit pas dans le dossier de l'utilisateur : pas concernée (non lancée ici).
    std::fs::remove_dir_all(base).unwrap();
}
