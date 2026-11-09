//! Certificat TLS auto-signé du serveur : généré au premier lancement, puis relu à
//! l'identique (même empreinte) à chaque démarrage. Le client mémorise l'empreinte SHA-256
//! à la première connexion.
//!
//! Générateur : `rcgen`. `matc::cert_x509` ne convient pas : il produit des certificats
//! Matter (OID propres à Matter, pas de nom alternatif), que TLS n'accepte pas.

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

use super::write_secret;

pub const CERT_FILE: &str = "tls-cert.pem";
pub const KEY_FILE: &str = "tls-key.pem";
/// Posé avant la création du premier certificat, retiré une fois clé et certificat écrits.
/// Sa présence avec la seule clé veut dire « première création interrompue » : aucun client
/// n'a pu mémoriser une empreinte (le serveur n'écoutait pas encore), on peut recommencer.
/// Sans lui, une clé seule est un certificat perdu après coup : on ne régénère pas.
pub const CREATING_MARKER: &str = "tls-creation-en-cours";

/// Validité du certificat. Le client épingle l'empreinte et ne regarde pas la date, mais un
/// certificat qui expire ferait changer l'empreinte : on le fait durer.
const VALID_YEARS: i32 = 20;

pub struct TlsIdentity {
    pub config: Arc<rustls::ServerConfig>,
    /// Empreinte SHA-256 du certificat (DER), `AA:BB:…`.
    pub fingerprint: String,
    /// `true` si le certificat vient d'être créé.
    pub created: bool,
}

/// Empreinte SHA-256, en hexadécimal majuscule séparé par des deux-points.
pub fn fingerprint_of(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Noms et adresses que le certificat annonce : la machine, ses adresses, la boucle locale.
fn subject_names() -> Vec<String> {
    let mut names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Ok(out) = std::process::Command::new("hostname").output() {
        let host = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let valid = !host.is_empty()
            && host.len() < 100
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        if valid {
            if !host.ends_with(".local") && !host.contains('.') {
                names.push(format!("{host}.local"));
            }
            names.push(host);
        }
    }
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            let ip = iface.ip();
            let usable = match ip {
                IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local(),
                // Pas d'adresse lien-local fe80 : elle ne s'écrit pas dans une URL sans zone.
                IpAddr::V6(v6) => !v6.is_loopback() && (v6.segments()[0] & 0xffc0) != 0xfe80,
            };
            if usable {
                names.push(ip.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Clé et certificat (PEM) d'un nouveau certificat auto-signé.
fn generate_pems() -> Result<(String, String)> {
    let key = rcgen::KeyPair::generate().context("génération de la clé TLS")?;
    let mut params =
        rcgen::CertificateParams::new(subject_names()).context("noms du certificat TLS")?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Matter Home Server");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2024 + VALID_YEARS, 1, 1);
    let cert = params
        .self_signed(&key)
        .context("signature du certificat TLS")?;
    Ok((key.serialize_pem(), cert.pem()))
}

/// Écrit un premier certificat. Une coupure à n'importe quel point laisse un état que
/// `load_or_create` sait reprendre : le marqueur est posé d'abord et retiré en dernier.
fn create_first(dir: &Path) -> Result<()> {
    let (key_pem, cert_pem) = generate_pems()?;
    store_first(dir, &key_pem, &cert_pem, usize::MAX)
}

/// `stop_after` : nombre d'étapes à faire avant de s'arrêter net (simule une coupure ; les
/// tests seuls s'en servent).
fn store_first(dir: &Path, key_pem: &str, cert_pem: &str, stop_after: usize) -> Result<()> {
    let marker = dir.join(CREATING_MARKER);
    let steps: [&dyn Fn() -> Result<()>; 4] = [
        &|| write_secret(&marker, b""),
        &|| write_secret(&dir.join(KEY_FILE), key_pem.as_bytes()),
        &|| write_secret(&dir.join(CERT_FILE), cert_pem.as_bytes()),
        &|| {
            std::fs::remove_file(&marker)
                .with_context(|| format!("retrait de {}", marker.display()))?;
            sync_dir(dir)
        },
    ];
    for step in steps.iter().take(stop_after) {
        step()?;
    }
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("synchronisation de {}", dir.display()))
}

/// Charge le certificat de `dir` (créé au premier lancement) et prépare la configuration TLS.
pub fn load_or_create(dir: &Path) -> Result<TlsIdentity> {
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);
    let marker = dir.join(CREATING_MARKER);
    let created = match (cert_path.exists(), key_path.exists()) {
        (false, false) => {
            create_first(dir)?;
            true
        }
        (true, true) => {
            // Coupure juste avant le retrait du marqueur : clé et certificat sont complets
            // (écritures atomiques).
            let _ = std::fs::remove_file(&marker);
            false
        }
        // Première création interrompue entre la clé et le certificat : rien n'a été publié,
        // on recommence (le marqueur prouve qu'aucun certificat n'a jamais existé ici).
        (false, true) if marker.exists() => {
            log::warn!("création du certificat TLS interrompue au premier lancement : reprise");
            create_first(dir)?;
            true
        }
        // Régénérer changerait l'empreinte que les clients ont mémorisée : on s'arrête
        // plutôt que de le faire en silence.
        _ => bail!(
            "le certificat TLS est incomplet dans {} ({CERT_FILE} et {KEY_FILE} doivent exister \
             ensemble). Remettez le fichier manquant, ou supprimez l'autre pour recréer un \
             certificat (les clients devront alors mémoriser la nouvelle empreinte).",
            dir.display()
        ),
    };
    let certs = CertificateDer::pem_file_iter(&cert_path)
        .with_context(|| format!("lecture de {}", cert_path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("certificat illisible dans {}", cert_path.display()))?;
    let first = certs
        .first()
        .context("aucun certificat TLS dans le fichier")?;
    let fingerprint = fingerprint_of(first);
    let key = PrivateKeyDer::from_pem_file(&key_path)
        .with_context(|| format!("clé TLS illisible dans {}", key_path.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("versions TLS")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("certificat et clé TLS incompatibles")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsIdentity {
        config: Arc::new(config),
        fingerprint,
        created,
    })
}

/// Empreinte du certificat déjà créé dans `dir`, sans rien générer.
pub fn read_fingerprint(dir: &Path) -> Result<String> {
    let cert_path = dir.join(CERT_FILE);
    let certs = CertificateDer::pem_file_iter(&cert_path)
        .with_context(|| {
            format!(
                "lecture de {} (le serveur n'a jamais été lancé ?)",
                cert_path.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("certificat illisible")?;
    let first = certs
        .first()
        .context("aucun certificat TLS dans le fichier")?;
    Ok(fingerprint_of(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-tls-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn certificate_is_stable_and_private() {
        let dir = temp("stable");
        let first = load_or_create(&dir).unwrap();
        assert!(first.created);
        assert_eq!(first.fingerprint.len(), 32 * 3 - 1);
        let second = load_or_create(&dir).unwrap();
        assert!(!second.created);
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(read_fingerprint(&dir).unwrap(), first.fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [CERT_FILE, KEY_FILE] {
                let mode = std::fs::metadata(dir.join(f)).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "{f}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn interrupted_first_creation_is_resumed() {
        let (key_pem, cert_pem) = generate_pems().unwrap();
        // Une coupure après chacune des étapes : marqueur, clé, certificat.
        for stop_after in 1..=3 {
            let dir = temp(&format!("cut{stop_after}"));
            store_first(&dir, &key_pem, &cert_pem, stop_after).unwrap();
            let loaded = load_or_create(&dir)
                .unwrap_or_else(|e| panic!("coupure après l'étape {stop_after} : {e:#}"));
            assert!(!dir.join(CREATING_MARKER).exists());
            // Redémarrage suivant : même empreinte, rien de recréé.
            let again = load_or_create(&dir).unwrap();
            assert!(!again.created);
            assert_eq!(again.fingerprint, loaded.fingerprint);
            assert_eq!(read_fingerprint(&dir).unwrap(), loaded.fingerprint);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn half_a_certificate_is_refused_not_regenerated() {
        let dir = temp("half");
        load_or_create(&dir).unwrap();
        std::fs::remove_file(dir.join(KEY_FILE)).unwrap();
        let err = load_or_create(&dir).err().unwrap().to_string();
        assert!(err.contains("incomplet"), "{err}");
        assert!(!dir.join(KEY_FILE).exists());
        std::fs::remove_dir_all(dir).unwrap();

        // Le certificat seul, ou la clé seule sans marqueur (certificat perdu après coup,
        // peut-être déjà mémorisé par un client) : refus, jamais de régénération silencieuse.
        let dir = temp("half-cert-only");
        load_or_create(&dir).unwrap();
        std::fs::remove_file(dir.join(CERT_FILE)).unwrap();
        let key_before = std::fs::read(dir.join(KEY_FILE)).unwrap();
        let err = load_or_create(&dir).err().unwrap().to_string();
        assert!(err.contains("incomplet"), "{err}");
        assert_eq!(std::fs::read(dir.join(KEY_FILE)).unwrap(), key_before);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
