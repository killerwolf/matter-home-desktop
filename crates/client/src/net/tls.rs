//! Connexion TLS vers le serveur, avec son certificat auto-signé épinglé par empreinte.
//!
//! Trois modes, jamais mélangés :
//! - [`Trust::Probe`] : première connexion. On fait la poignée de main TLS pour lire l'empreinte
//!   du certificat présenté, puis on ferme **sans rien envoyer**. L'utilisateur compare cette
//!   empreinte (affichée par le serveur à son lancement) avant de l'approuver ;
//! - [`Trust::Pinned`] : toutes les connexions suivantes, y compris celle qui envoie le mot de
//!   passe. Le certificat doit avoir exactement l'empreinte épinglée, la signature de la
//!   poignée de main est vérifiée pour de bon (rejouer le certificat public ne suffit pas).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    StreamOwned,
};
use sha2::{Digest, Sha256};

use super::addr::ServerAddr;
use super::error::NetError;

pub type TlsStream = StreamOwned<ClientConnection, DeadlineStream>;

/// Délai pour établir la connexion TCP puis la poignée de main TLS.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Test d'annulation consulté avant chaque lecture ou écriture : `true` coupe la connexion.
pub type Cancel = Arc<dyn Fn() -> bool + Send + Sync>;

/// Un `TcpStream` dont **chaque** lecture et écriture respecte une échéance absolue.
///
/// Un délai de lecture posé une fois sur la socket se réarme à chaque octet reçu : un serveur
/// qui goutte un octet toutes les quelques secondes tient la poignée de main, la réponse HTTP
/// ou le WebSocket ouverts aussi longtemps qu'il veut. Ici, le temps restant avant l'échéance
/// est recalculé avant chaque appel système, et l'échéance dépassée donne
/// [`std::io::ErrorKind::TimedOut`] quoi que le serveur envoie.
///
/// `tick`, s'il est posé, borne en plus chaque lecture : elle rend alors `WouldBlock` (sans que
/// l'échéance soit atteinte), ce que rustls et tungstenite savent reprendre en cours de
/// message. Le WebSocket s'en sert pour revenir régulièrement dans sa boucle.
pub struct DeadlineStream {
    sock: TcpStream,
    deadline: Instant,
    tick: Option<Duration>,
    cancel: Option<Cancel>,
}

impl DeadlineStream {
    pub fn new(sock: TcpStream, deadline: Instant) -> Self {
        Self {
            sock,
            deadline,
            tick: None,
            cancel: None,
        }
    }

    pub fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = deadline;
    }

    pub fn set_tick(&mut self, tick: Option<Duration>) {
        self.tick = tick;
    }

    pub fn set_cancel(&mut self, cancel: Option<Cancel>) {
        self.cancel = cancel;
    }

    /// Temps restant avant l'échéance, ou l'erreur à rendre si elle est passée.
    fn left(&self) -> std::io::Result<Duration> {
        if self.cancel.as_ref().is_some_and(|c| c()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "connexion abandonnée",
            ));
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(deadline_error());
        }
        Ok(left)
    }

    /// Une socket qui a rendu `WouldBlock` alors que l'échéance est passée : c'est elle.
    fn after_wait<T>(&self, result: std::io::Result<T>) -> std::io::Result<T> {
        match result {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && Instant::now() >= self.deadline =>
            {
                Err(deadline_error())
            }
            other => other,
        }
    }
}

fn deadline_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "délai dépassé")
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.left()?;
        let wait = self.tick.map_or(left, |t| t.min(left));
        self.sock.set_read_timeout(Some(wait))?;
        let result = self.sock.read(buf);
        self.after_wait(result)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let left = self.left()?;
        self.sock.set_write_timeout(Some(left))?;
        let result = self.sock.write(buf);
        self.after_wait(result)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.sock.flush()
    }
}

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

/// Comment le certificat est jugé (voir l'en-tête du module).
enum Verify {
    Probe,
    Pinned(String),
    /// Chaîne de confiance publique, avec les autorités racines données.
    Public(Arc<WebPkiServerVerifier>),
}

/// Vérificateur de certificat : note l'empreinte présentée et, selon le mode, la juge.
struct Verifier {
    verify: Verify,
    seen: Arc<Mutex<Option<String>>>,
    /// Raison du refus en mode chaîne publique, pour la dire à l'utilisateur.
    refused: Arc<Mutex<Option<String>>>,
    provider: Arc<CryptoProvider>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Verifier")
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let found = fingerprint_of(end_entity);
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(found.clone());
        }
        match &self.verify {
            Verify::Probe => Ok(ServerCertVerified::assertion()),
            // Le nom n'est volontairement pas regardé : seule l'empreinte compte.
            Verify::Pinned(expected) if *expected == found => Ok(ServerCertVerified::assertion()),
            Verify::Pinned(_) => Err(rustls::Error::General("empreinte différente".into())),
            Verify::Public(inner) => inner
                .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
                .inspect_err(|e| {
                    if let Ok(mut refused) = self.refused.lock() {
                        *refused = Some(e.to_string());
                    }
                }),
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
    connect_until(addr, trust, Instant::now() + CONNECT_TIMEOUT)
}

/// Comme [`connect`], avec une échéance absolue : la connexion TCP et la poignée de main TLS
/// n'en dépassent jamais, même si le serveur répond au compte-gouttes. Le flux rendu garde
/// cette échéance : l'appelant la remplace par la sienne (`stream.sock.set_deadline`).
pub fn connect_until(
    addr: &ServerAddr,
    trust: &Trust,
    deadline: Instant,
) -> Result<Connected, NetError> {
    let provider = Arc::new(ring::default_provider());
    let verify = match trust {
        Trust::Probe => Verify::Probe,
        // `tailscale serve` : certificat public de Tailscale, jugé par la chaîne de confiance.
        Trust::Pinned(_) if addr.is_tailscale_serve() => {
            Verify::Public(public_verifier(public_roots(), &provider)?)
        }
        Trust::Pinned(fp) => Verify::Pinned(fp.clone()),
    };
    connect_with(addr, trust, verify, provider, None, deadline)
}

/// Autorités racines publiques (Mozilla, embarquées : ne dépendent pas du système).
fn public_roots() -> RootCertStore {
    RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

fn public_verifier(
    roots: RootCertStore,
    provider: &Arc<CryptoProvider>,
) -> Result<Arc<WebPkiServerVerifier>, NetError> {
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|e| NetError::Protocol(e.to_string()))
}

/// Cœur de la connexion. `name` remplace le nom présenté en SNI et contrôlé par la chaîne
/// publique (les essais joignent 127.0.0.1 en se faisant passer pour un nom `.ts.net`).
fn connect_with(
    addr: &ServerAddr,
    trust: &Trust,
    verify: Verify,
    provider: Arc<CryptoProvider>,
    name: Option<&str>,
    deadline: Instant,
) -> Result<Connected, NetError> {
    let public = matches!(verify, Verify::Public(_));
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
    let sock = sock.ok_or(last)?;
    let _ = sock.set_nodelay(true);
    let mut sock = DeadlineStream::new(sock, deadline);

    let seen = Arc::new(Mutex::new(None));
    let refused = Arc::new(Mutex::new(None));
    let verifier = Arc::new(Verifier {
        verify,
        seen: seen.clone(),
        refused: refused.clone(),
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| NetError::Protocol(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    // Épinglé : le nom n'est pas vérifié, mais TLS en exige un. Chaîne publique : c'est ce nom
    // que le certificat doit porter.
    let host = name.unwrap_or(&addr.host);
    let name = ServerName::try_from(host.to_string())
        .or_else(|_| ServerName::try_from("matter-home.invalid".to_string()))
        .map_err(|_| NetError::BadAddress("nom de serveur invalide".into()))?;
    let mut conn = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| NetError::Protocol(e.to_string()))?;
    // Poignée de main terminée ici, avec l'échéance ci-dessus : ensuite seulement le flux sert.
    while conn.is_handshaking() {
        if Instant::now() >= deadline {
            return Err(NetError::Timeout);
        }
        if let Err(e) = conn.complete_io(&mut sock) {
            if public {
                if let Some(why) = refused.lock().ok().and_then(|r| r.clone()) {
                    return Err(NetError::PublicCertificateRejected(why));
                }
            }
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serveur de test : sur 127.0.0.1 uniquement, il écrit `first`, puis un octet toutes les
    /// `pause` pendant `for_how_long`. C'est le goutte-à-goutte : chaque octet réarme un délai
    /// de lecture ordinaire, jamais une échéance absolue.
    fn dripping_server(
        first: &'static [u8],
        pause: Duration,
        for_how_long: Duration,
    ) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let _ = sock.write_all(first);
            let end = Instant::now() + for_how_long;
            while Instant::now() < end {
                std::thread::sleep(pause);
                if sock.write_all(&[0]).is_err() {
                    return;
                }
            }
        });
        addr
    }

    #[test]
    fn a_drip_never_extends_the_handshake_deadline() {
        // En-tête d'un enregistrement TLS de 16 Kio, dont le reste arrive octet par octet.
        let addr = dripping_server(
            &[0x16, 0x03, 0x03, 0x40, 0x00],
            Duration::from_millis(100),
            Duration::from_secs(8),
        );
        let server = ServerAddr::parse(&format!("https://{addr}")).unwrap();
        let started = Instant::now();
        let result = connect_until(&server, &Trust::Probe, started + Duration::from_millis(700));
        let took = started.elapsed();
        assert!(
            matches!(result, Err(NetError::Timeout)),
            "{:?}",
            result.err()
        );
        assert!(
            took < Duration::from_millis(1500),
            "la poignée de main a duré {took:?} pour une échéance de 700 ms"
        );
    }

    #[test]
    fn a_stream_with_a_tick_comes_back_but_the_deadline_still_ends_it() {
        let addr = dripping_server(b"x", Duration::from_millis(80), Duration::from_secs(8));
        let sock = TcpStream::connect(addr).unwrap();
        let started = Instant::now();
        let mut stream = DeadlineStream::new(sock, started + Duration::from_millis(600));
        stream.set_tick(Some(Duration::from_millis(40)));
        let mut byte = [0u8; 1];
        let (mut ticks, mut bytes) = (0, 0);
        let end = loop {
            match stream.read(&mut byte) {
                Ok(0) => panic!("fermé"),
                Ok(_) => bytes += 1,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => ticks += 1,
                Err(e) => break e,
            }
        };
        assert_eq!(end.kind(), std::io::ErrorKind::TimedOut);
        assert!(bytes > 3 && ticks > 0, "{bytes} octets, {ticks} pas");
        assert!(started.elapsed() < Duration::from_millis(1200));
    }

    #[test]
    fn a_cancelled_stream_stops_at_once() {
        let addr = dripping_server(b"x", Duration::from_millis(50), Duration::from_secs(5));
        let sock = TcpStream::connect(addr).unwrap();
        let mut stream = DeadlineStream::new(sock, Instant::now() + Duration::from_secs(30));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        stream.set_cancel(Some(Arc::new(move || {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        })));
        let mut byte = [0u8; 1];
        assert!(stream.read(&mut byte).is_ok());
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(stream.read(&mut byte).is_err());
    }

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

    // --- Certificats : épinglage direct et chaîne publique (accès à distance, ticket 14) ---

    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
    use rustls::pki_types::PrivateKeyDer;
    use rustls::{ServerConfig, ServerConnection};

    const TS_NAME: &str = "mon-mac.tailnet-xxxx.ts.net";

    struct Cert {
        der: CertificateDer<'static>,
        key: KeyPair,
    }

    /// Certificat auto-signé pour `names`, comme celui du serveur.
    fn self_signed(names: &[&str]) -> Cert {
        let key = KeyPair::generate().unwrap();
        let params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .unwrap();
        let cert = params.self_signed(&key).unwrap();
        Cert {
            der: cert.der().clone(),
            key,
        }
    }

    /// Autorité de test, et un certificat final signé par elle pour `names`.
    fn ca_and_leaf(names: &[&str]) -> (Cert, Cert) {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let leaf_params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .unwrap();
        let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).unwrap();
        (
            Cert {
                der: ca.der().clone(),
                key: ca_key,
            },
            Cert {
                der: leaf.der().clone(),
                key: leaf_key,
            },
        )
    }

    /// Serveur TLS de test sur 127.0.0.1 : fait la poignée de main avec `cert`, puis ferme.
    fn tls_server(cert: &Cert) -> SocketAddr {
        let key = PrivateKeyDer::try_from(cert.key.serialize_der()).unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der.clone()], key)
            .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { return };
                let mut conn = ServerConnection::new(config.clone()).unwrap();
                while conn.is_handshaking() {
                    if conn.complete_io(&mut sock).is_err() {
                        break;
                    }
                }
            }
        });
        addr
    }

    fn local(addr: SocketAddr) -> ServerAddr {
        ServerAddr::parse(&format!("https://{addr}")).unwrap()
    }

    fn pinned_connect(addr: SocketAddr, fp: &str, name: &str) -> Result<Connected, NetError> {
        connect_with(
            &local(addr),
            &Trust::Pinned(fp.to_string()),
            Verify::Pinned(fp.to_string()),
            Arc::new(ring::default_provider()),
            Some(name),
            Instant::now() + Duration::from_secs(5),
        )
    }

    fn public_connect(addr: SocketAddr, roots: &Cert, name: &str) -> Result<Connected, NetError> {
        let provider = Arc::new(ring::default_provider());
        let mut store = RootCertStore::empty();
        store.add(roots.der.clone()).unwrap();
        connect_with(
            &local(addr),
            &Trust::Pinned("AA".into()),
            Verify::Public(public_verifier(store, &provider).unwrap()),
            provider,
            Some(name),
            Instant::now() + Duration::from_secs(5),
        )
    }

    #[test]
    fn direct_access_pins_the_fingerprint_and_ignores_the_name() {
        // Le certificat du serveur ne porte ni le nom MagicDNS ni l'IP Tailscale.
        let cert = self_signed(&["localhost"]);
        let fp = fingerprint_of(&cert.der);
        let addr = tls_server(&cert);
        // Même empreinte, nom quelconque (celui du tailnet) : accepté.
        let c = pinned_connect(addr, &fp, TS_NAME).expect("empreinte épinglée, nom ignoré");
        assert_eq!(c.fingerprint, fp);
        // Une autre empreinte est refusée, sans contournement.
        let other = fingerprint_of(b"un autre certificat");
        match pinned_connect(addr, &other, TS_NAME) {
            Err(NetError::CertificateChanged { expected, found }) => {
                assert_eq!(expected, other);
                assert_eq!(found, fp);
            }
            other => panic!("attendu CertificateChanged, reçu {:?}", other.err()),
        }
    }

    #[test]
    fn the_public_chain_is_accepted_without_pinning_for_the_right_name() {
        let (ca, leaf) = ca_and_leaf(&[TS_NAME]);
        let addr = tls_server(&leaf);
        let c = public_connect(addr, &ca, TS_NAME).expect("chaîne valide, nom correct");
        // Empreinte différente de celle épinglée (« AA »), et pourtant acceptée : pas d'épinglage.
        assert_eq!(c.fingerprint, fingerprint_of(&leaf.der));
    }

    #[test]
    fn the_public_chain_rejects_a_wrong_name_an_unknown_authority_and_a_self_signed_one() {
        let (ca, leaf) = ca_and_leaf(&[TS_NAME]);
        let addr = tls_server(&leaf);
        // Nom différent de celui du certificat.
        let wrong = public_connect(addr, &ca, "autre.tailnet-xxxx.ts.net");
        assert!(
            matches!(wrong, Err(NetError::PublicCertificateRejected(_))),
            "{:?}",
            wrong.err()
        );
        // Autorité inconnue du client.
        let (other_ca, _) = ca_and_leaf(&[TS_NAME]);
        let unknown = public_connect(addr, &other_ca, TS_NAME);
        assert!(
            matches!(unknown, Err(NetError::PublicCertificateRejected(_))),
            "{:?}",
            unknown.err()
        );
        // Certificat auto-signé (celui du serveur) présenté sur une adresse en chaîne publique.
        let own = self_signed(&[TS_NAME]);
        let own_addr = tls_server(&own);
        let selfsigned = public_connect(own_addr, &ca, TS_NAME);
        assert!(
            matches!(selfsigned, Err(NetError::PublicCertificateRejected(_))),
            "{:?}",
            selfsigned.err()
        );
    }

    #[test]
    fn only_a_tailscale_name_on_443_uses_the_public_chain() {
        let serve = |t: &str| ServerAddr::parse(t).unwrap().is_tailscale_serve();
        assert!(serve("https://mon-mac.tailnet-xxxx.ts.net/"));
        assert!(serve("mon-mac.tailnet-xxxx.ts.net:443"));
        assert!(serve("MON-MAC.TAILNET-XXXX.TS.NET:443"));
        // Accès direct au serveur : empreinte épinglée.
        assert!(!serve("mon-mac.tailnet-xxxx.ts.net:8443"));
        assert!(!serve("mon-mac.tailnet-xxxx.ts.net"));
        // Ni une IP, ni un faux nom, ni un autre domaine.
        assert!(!serve("100.64.0.1:443"));
        assert!(!serve("evil-ts.net:443"));
        assert!(!serve("ts.net.example.com:443"));
        assert!(!serve("192.168.1.20:443"));
    }
}
