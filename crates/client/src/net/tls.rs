//! Connexion TLS vers le serveur, avec son certificat auto-signé épinglé par empreinte.
//!
//! Deux modes, jamais mélangés :
//! - [`Trust::Probe`] : première connexion. On fait la poignée de main TLS pour lire l'empreinte
//!   du certificat présenté, puis on ferme **sans rien envoyer**. L'utilisateur compare cette
//!   empreinte (affichée par le serveur à son lancement) avant de l'approuver ;
//! - [`Trust::Pinned`] : toutes les connexions suivantes, y compris celle qui envoie le mot de
//!   passe. Le certificat doit avoir exactement l'empreinte épinglée, la signature de la
//!   poignée de main est vérifiée pour de bon (rejouer le certificat public ne suffit pas).

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use sha2::{Digest, Sha256};

use super::addr::ServerAddr;
use super::error::NetError;

pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// Délai pour établir la connexion TCP puis la poignée de main TLS.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Empreinte SHA-256 d'un certificat, `AA:BB:…` (la forme que le serveur affiche).
pub fn fingerprint_of(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Met une empreinte saisie ou lue dans la forme canonique (majuscules, séparées par `:`).
/// `None` si ce n'est pas un SHA-256 (32 octets en hexadécimal).
pub fn normalize_fingerprint(text: &str) -> Option<String> {
    let hex: String = text
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hex = hex.to_ascii_uppercase();
    Some(
        hex.as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap_or("00"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

#[derive(Debug, Clone)]
pub enum Trust {
    /// Lit l'empreinte présentée, accepte la poignée de main, n'envoie rien.
    Probe,
    /// N'accepte que ce certificat.
    Pinned(String),
}

/// Vérificateur de certificat : note l'empreinte présentée et, selon le mode, la compare.
#[derive(Debug)]
struct Verifier {
    trust: Trust,
    seen: Arc<Mutex<Option<String>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let found = fingerprint_of(end_entity);
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(found.clone());
        }
        match &self.trust {
            Trust::Probe => Ok(ServerCertVerified::assertion()),
            Trust::Pinned(expected) if *expected == found => Ok(ServerCertVerified::assertion()),
            Trust::Pinned(_) => Err(rustls::Error::General("empreinte différente".into())),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Connexion TLS établie : le flux, et l'empreinte du certificat présenté.
pub struct Connected {
    pub stream: TlsStream,
    pub fingerprint: String,
}

fn resolve(addr: &ServerAddr) -> Result<Vec<SocketAddr>, NetError> {
    let found: Vec<SocketAddr> = (addr.host.as_str(), addr.port)
        .to_socket_addrs()
        .map_err(|e| NetError::Unreachable(format!("adresse introuvable ({})", io_reason(&e))))?
        .collect();
    if found.is_empty() {
        return Err(NetError::Unreachable("adresse introuvable".into()));
    }
    Ok(found)
}

/// Raison d'une erreur d'entrée-sortie, en français, sans détail technique inutile.
pub fn io_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused => "connexion refusée".into(),
        TimedOut | WouldBlock => "délai dépassé".into(),
        ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof => {
            "connexion coupée".into()
        }
        NotConnected => "non connecté".into(),
        AddrNotAvailable | HostUnreachable | NetworkUnreachable | NetworkDown => {
            "réseau inaccessible".into()
        }
        _ => e.to_string(),
    }
}

/// Ouvre la connexion TCP et termine la poignée de main TLS, dans un délai borné.
pub fn connect(addr: &ServerAddr, trust: &Trust) -> Result<Connected, NetError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last = NetError::Unreachable("aucune adresse".into());
    let mut sock = None;
    for target in resolve(addr)? {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(NetError::Timeout);
        }
        match TcpStream::connect_timeout(&target, left.min(Duration::from_secs(4))) {
            Ok(s) => {
                sock = Some(s);
                break;
            }
            Err(e) => last = NetError::Unreachable(io_reason(&e)),
        }
    }
    let mut sock = sock.ok_or(last)?;
    let _ = sock.set_nodelay(true);
    let io_err = |e: std::io::Error| NetError::Unreachable(io_reason(&e));
    sock.set_read_timeout(Some(CONNECT_TIMEOUT))
        .map_err(io_err)?;
    sock.set_write_timeout(Some(CONNECT_TIMEOUT))
        .map_err(io_err)?;

    let provider = Arc::new(ring::default_provider());
    let seen = Arc::new(Mutex::new(None));
    let verifier = Arc::new(Verifier {
        trust: trust.clone(),
        seen: seen.clone(),
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| NetError::Protocol(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    // Le nom n'est pas vérifié (le certificat est épinglé), mais TLS en exige un.
    let name = ServerName::try_from(addr.host.clone())
        .or_else(|_| ServerName::try_from("matter-home.invalid".to_string()))
        .map_err(|_| NetError::BadAddress("nom de serveur invalide".into()))?;
    let mut conn = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| NetError::Protocol(e.to_string()))?;
    // Poignée de main terminée ici, avec le délai ci-dessus : ensuite seulement le flux sert.
    while conn.is_handshaking() {
        if Instant::now() >= deadline {
            return Err(NetError::Timeout);
        }
        if let Err(e) = conn.complete_io(&mut sock) {
            return Err(handshake_error(e, trust, &seen));
        }
    }
    let fingerprint = seen
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .ok_or_else(|| NetError::Protocol("le serveur n'a présenté aucun certificat".into()))?;
    Ok(Connected {
        stream: StreamOwned::new(conn, sock),
        fingerprint,
    })
}

/// Un refus d'empreinte n'est pas une panne réseau : il est signalé comme tel.
fn handshake_error(
    e: std::io::Error,
    trust: &Trust,
    seen: &Arc<Mutex<Option<String>>>,
) -> NetError {
    if let Trust::Pinned(expected) = trust {
        let found = seen.lock().ok().and_then(|s| s.clone());
        if let Some(found) = found.filter(|f| f != expected) {
            return NetError::CertificateChanged {
                expected: expected.clone(),
                found,
            };
        }
    }
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => NetError::Timeout,
        _ => NetError::Unreachable(format!(
            "connexion sécurisée impossible ({})",
            io_reason(&e)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format_matches_the_server() {
        let f = fingerprint_of(b"certificat");
        assert_eq!(f.len(), 32 * 3 - 1);
        assert!(f.split(':').all(|b| b.len() == 2
            && b.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())));
        assert_eq!(normalize_fingerprint(&f.to_lowercase()), Some(f.clone()));
        assert_eq!(normalize_fingerprint(&f.replace(':', " ")), Some(f));
        assert_eq!(normalize_fingerprint("abcd"), None);
        assert_eq!(normalize_fingerprint(&"zz".repeat(32)), None);
    }
}
