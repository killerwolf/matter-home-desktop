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
    }
}
