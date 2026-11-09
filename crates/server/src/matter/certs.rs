//! Identité du contrôleur : génération de la racine (RCAC) et du certificat
//! opérationnel, et implémentation du trait `CertManager` de matc à partir du JSON.

use anyhow::{Context, Result};
use matc::certmanager::CertManager;
use rand::Rng;

use crate::persistence::ControllerIdentity;

/// Identifiant de la racine ; doit valoir 1, comme `CA_ID` dans `matc::controller`.
const CA_NODE_ID: u64 = 1;
/// Identifiant opérationnel du contrôleur sur sa fabric.
const CONTROLLER_NODE_ID: u64 = 112233;

/// Crée une nouvelle fabric : clés et certificats tirés au hasard.
pub fn generate_identity() -> Result<ControllerIdentity> {
    let mut rng = rand::thread_rng();
    let fabric_id: u64 = rng.gen_range(1..=u32::MAX as u64);

    let ca_key = p256::SecretKey::random(&mut rng);
    let ca_cert = matc::cert_x509::encode_x509(
        &ca_key.public_key().to_sec1_bytes(),
        CA_NODE_ID,
        fabric_id,
        CA_NODE_ID,
        &ca_key,
        true,
    )
    .context("génération du certificat racine")?;

    let ctrl_key = p256::SecretKey::random(&mut rng);
    let ctrl_cert = matc::cert_x509::encode_x509(
        &ctrl_key.public_key().to_sec1_bytes(),
        CONTROLLER_NODE_ID,
        fabric_id,
        CA_NODE_ID,
        &ca_key,
        false,
    )
    .context("génération du certificat du contrôleur")?;

    Ok(ControllerIdentity {
        fabric_id,
        controller_node_id: CONTROLLER_NODE_ID,
        ca_private_key: hex::encode(ca_key.to_bytes()),
        ca_cert_der: hex::encode(ca_cert),
        controller_private_key: hex::encode(ctrl_key.to_bytes()),
        controller_cert_der: hex::encode(ctrl_cert),
    })
}

/// Identité d'essai dont les deux clés sont `key_hex` (une clé P-256 valide) et dont les
/// certificats sont de vrais certificats DER signés par cette clé.
#[cfg(test)]
pub(crate) fn test_identity(key_hex: &str) -> ControllerIdentity {
    // Mémorisée : les certificats portent une date et une signature aléatoire, et les tests
    // comparent des fichiers octet pour octet.
    static CACHE: std::sync::Mutex<Vec<(String, ControllerIdentity)>> =
        std::sync::Mutex::new(Vec::new());
    let mut cache = CACHE.lock().unwrap();
    if let Some((_, id)) = cache.iter().find(|(k, _)| k == key_hex) {
        return id.clone();
    }
    let id = build_test_identity(key_hex);
    cache.push((key_hex.to_string(), id.clone()));
    id
}

#[cfg(test)]
fn build_test_identity(key_hex: &str) -> ControllerIdentity {
    let key = decode_key(key_hex).unwrap();
    let cert = |node_id: u64, ca: bool| {
        matc::cert_x509::encode_x509(
            &key.public_key().to_sec1_bytes(),
            node_id,
            1,
            CA_NODE_ID,
            &key,
            ca,
        )
        .unwrap()
    };
    ControllerIdentity {
        fabric_id: 1,
        controller_node_id: 2,
        ca_private_key: key_hex.into(),
        ca_cert_der: hex::encode(cert(CA_NODE_ID, true)),
        controller_private_key: key_hex.into(),
        controller_cert_der: hex::encode(cert(2, false)),
    }
}

/// `CertManager` servi depuis l'identité chargée du JSON.
pub struct JsonCertManager {
    fabric_id: u64,
    controller_node_id: u64,
    ca_key: p256::SecretKey,
    ca_cert: Vec<u8>,
    ctrl_key: p256::SecretKey,
    ctrl_cert: Vec<u8>,
}

fn decode_key(hex_str: &str) -> Result<p256::SecretKey> {
    let bytes = hex::decode(hex_str).context("clé hexadécimale invalide")?;
    p256::SecretKey::from_slice(&bytes).context("clé P-256 invalide")
}

impl JsonCertManager {
    pub fn from_identity(id: &ControllerIdentity) -> Result<Self> {
        Ok(Self {
            fabric_id: id.fabric_id,
            controller_node_id: id.controller_node_id,
            ca_key: decode_key(&id.ca_private_key)?,
            ca_cert: hex::decode(&id.ca_cert_der).context("certificat racine invalide")?,
            ctrl_key: decode_key(&id.controller_private_key)?,
            ctrl_cert: hex::decode(&id.controller_cert_der)
                .context("certificat du contrôleur invalide")?,
        })
    }

    /// Les deux certificats DER se décodent (comme le fera le contrôleur) : une sauvegarde dont
    /// le contenu hexadécimal est valide mais n'est pas un certificat est refusée à l'import,
    /// pas à la première connexion à un appareil.
    pub fn check_certificates(&self) -> Result<()> {
        let ca_public = self.ca_key.public_key().to_sec1_bytes();
        matc::cert_matter::convert_x509_bytes_to_matter(&self.ca_cert, &ca_public)
            .context("certificat racine illisible")?;
        matc::cert_matter::convert_x509_bytes_to_matter(&self.ctrl_cert, &ca_public)
            .context("certificat du contrôleur illisible")?;
        Ok(())
    }

    fn check_user(&self, id: u64) -> Result<()> {
        anyhow::ensure!(
            id == self.controller_node_id,
            "identité {id} inconnue (contrôleur = {})",
            self.controller_node_id
        );
        Ok(())
    }
}

impl CertManager for JsonCertManager {
    fn get_ca_cert(&self) -> Result<Vec<u8>> {
        Ok(self.ca_cert.clone())
    }
    fn get_ca_key(&self) -> Result<p256::SecretKey> {
        Ok(self.ca_key.clone())
    }
    fn get_ca_public_key(&self) -> Result<Vec<u8>> {
        Ok(self.ca_key.public_key().to_sec1_bytes().to_vec())
    }
    fn get_user_cert(&self, id: u64) -> Result<Vec<u8>> {
        self.check_user(id)?;
        Ok(self.ctrl_cert.clone())
    }
    fn get_user_key(&self, id: u64) -> Result<p256::SecretKey> {
        self.check_user(id)?;
        Ok(self.ctrl_key.clone())
    }
    fn get_fabric_id(&self) -> u64 {
        self.fabric_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_round_trip() {
        let id = generate_identity().unwrap();
        let cm = JsonCertManager::from_identity(&id).unwrap();
        assert_eq!(cm.get_fabric_id(), id.fabric_id);
        assert_eq!(cm.get_ca_public_key().unwrap().len(), 65);
        assert!(cm.get_user_cert(id.controller_node_id).is_ok());
        assert!(cm.get_user_key(999).is_err());
        cm.check_certificates().unwrap();
    }

    #[test]
    fn garbage_certificates_are_refused() {
        for field in ["ca", "controller"] {
            let mut id = generate_identity().unwrap();
            // Du texte hexadécimal valide, mais pas un certificat DER.
            if field == "ca" {
                id.ca_cert_der = "deadbeefcafe".into();
            } else {
                id.controller_cert_der = "00".repeat(40);
            }
            let cm = JsonCertManager::from_identity(&id).unwrap();
            assert!(cm.check_certificates().is_err(), "{field}");
        }
    }
}
