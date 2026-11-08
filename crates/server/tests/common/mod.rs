//! Outils des tests d'intégration : lancement du vrai binaire en mode démo, client TLS qui
//! épingle l'empreinte du certificat (comme le fera le client), requêtes HTTPS et WebSocket.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

pub const USER: &str = "hamdi";
pub const PASSWORD: &str = "un-mot-de-passe-solide-pour-les-tests";

const BIN: &str = env!("CARGO_BIN_EXE_matter-home-server");

pub fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mhd-it-{tag}-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rand_suffix() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

/// Crée le compte avec le vrai binaire (mot de passe par l'entrée standard).
pub fn create_account(
    dir: &Path,
    username: &str,
    password: &str,
    extra: &[&str],
) -> (ExitStatus, String) {
    let mut child = Command::new(BIN)
        .args([
            "create-account",
            "--username",
            username,
            "--password-stdin",
            "--data-dir",
        ])
        .arg(dir)
        .args(extra)
        .env("HOME", dir.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{password}").unwrap();
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status, text)
}

/// Un serveur `--demo` en cours d'exécution, dans un sous-processus.
pub struct Server {
    child: Option<Child>,
    pub dir: PathBuf,
    pub addr: SocketAddr,
    pub fingerprint: String,
    pub log_path: PathBuf,
}

impl Server {
    /// Lance le serveur sur `dir` (compte `USER`/`PASSWORD` créé s'il n'existe pas).
    pub fn start(dir: &Path) -> Server {
        Server::start_with(dir, &[])
    }

    pub fn start_with(dir: &Path, env: &[(&str, &str)]) -> Server {
        if !dir.join("serveur").join("compte.json").exists() {
            let (status, text) = create_account(dir, USER, PASSWORD, &[]);
            assert!(status.success(), "{text}");
        }
        let log_path = dir.join(format!("serveur-{}.log", rand_suffix()));
        let log = std::fs::File::create(&log_path).unwrap();
        let mut command = Command::new(BIN);
        command
            .args(["--demo", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(dir)
            .env("HOME", dir.join("home"))
            .env("RUST_LOG", "debug")
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        for (k, v) in env {
            command.env(k, v);
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let (mut addr, mut fingerprint) = (None, None);
        let deadline = Instant::now() + Duration::from_secs(60);
        while addr.is_none() || fingerprint.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = rx.recv_timeout(left).unwrap_or_else(|_| {
                panic!(
                    "le serveur n'a pas démarré. Journal :\n{}",
                    std::fs::read_to_string(&log_path).unwrap_or_default()
                )
            });
            if let Some(url) = line.strip_prefix("Écoute : https://") {
                addr = Some(url.trim().parse().unwrap());
            } else if let Some(f) = line.strip_prefix("Empreinte du certificat (SHA-256) : ") {
                fingerprint = Some(f.trim().to_string());
            }
        }
        Server {
            child: Some(child),
            dir: dir.to_path_buf(),
            addr: addr.unwrap(),
            fingerprint: fingerprint.unwrap(),
            log_path,
        }
    }

    pub fn client(&self) -> Client {
        Client::new(self.addr, &self.fingerprint)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// Arrêt demandé comme le fait launchd (SIGTERM) ; retourne le code de sortie.
    pub fn terminate(&mut self) -> ExitStatus {
        let child = self.child.as_mut().unwrap();
        let status = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        self.wait_exit(Duration::from_secs(20))
            .expect("le serveur ne s'est pas arrêté")
    }

    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let child = self.child.as_mut()?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                self.child = None;
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Client TLS qui épingle l'empreinte
// ---------------------------------------------------------------------------

pub fn fingerprint_of(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint_of(end_entity) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("empreinte différente".into()))
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

pub fn tls_config(fingerprint: &str) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint: fingerprint.to_string(),
            provider,
        }))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

#[derive(Debug)]
pub struct Resp {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "JSON attendu ({e}) : {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Code d'erreur du protocole (`error.code`).
    pub fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }
}

#[derive(Clone)]
pub struct Client {
    pub addr: SocketAddr,
    pub fingerprint: String,
    config: Arc<rustls::ClientConfig>,
}

pub type Tls = TlsStream<TcpStream>;

impl Client {
    pub fn new(addr: SocketAddr, fingerprint: &str) -> Client {
        Client {
            addr,
            fingerprint: fingerprint.to_string(),
            config: tls_config(fingerprint),
        }
    }

    pub async fn connect(&self) -> Tls {
        let tcp = TcpStream::connect(self.addr).await.unwrap();
        self.handshake(tcp).await
    }

    async fn handshake(&self, tcp: TcpStream) -> Tls {
        let connector = tokio_rustls::TlsConnector::from(self.config.clone());
        connector
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap()
    }

    /// Requête HTTPS complète sur une connexion neuve (`Connection: close`).
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&[u8]>,
    ) -> Resp {
        let mut stream = self.connect().await;
        let mut head =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if let Some(t) = token {
            head.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        if let Some(b) = body {
            head.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                b.len()
            ));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await.unwrap();
        if let Some(b) = body {
            stream.write_all(b).await.unwrap();
        }
        let mut raw = Vec::new();
        // Le serveur ferme après la réponse ; une coupure TLS brutale en fin de flux est normale.
        let _ = stream.read_to_end(&mut raw).await;
        parse_response(&raw)
    }

    pub async fn get(&self, path: &str, token: &str) -> Resp {
        self.request("GET", path, Some(token), None).await
    }

    pub async fn send_json(&self, method: &str, path: &str, token: &str, body: &Value) -> Resp {
        self.request(method, path, Some(token), Some(body.to_string().as_bytes()))
            .await
    }

    pub async fn login_as(&self, user: &str, password: &str, protocol: u32) -> Resp {
        let body = serde_json::json!({"username": user, "password": password, "protocol_version": protocol});
        self.request(
            "POST",
            "/api/login",
            None,
            Some(body.to_string().as_bytes()),
        )
        .await
    }

    /// Ouvre une session avec le compte de test et rend le jeton.
    pub async fn login(&self) -> String {
        let resp = self.login_as(USER, PASSWORD, 1).await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        resp.json()["token"].as_str().unwrap().to_string()
    }

    /// Ouvre un WebSocket (`protocol` = version annoncée dans l'URL).
    pub async fn ws(&self, token: &str, protocol: u32) -> Result<WebSocketStream<Tls>, Resp> {
        let tcp = TcpStream::connect(self.addr).await.unwrap();
        self.ws_over(tcp, token, protocol).await
    }

    /// Comme `ws`, avec un tampon de réception minuscule : le client « avale » très peu.
    pub async fn ws_slow(&self, token: &str, protocol: u32) -> Result<WebSocketStream<Tls>, Resp> {
        let socket = TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(2048).unwrap();
        let tcp = socket.connect(self.addr).await.unwrap();
        self.ws_over(tcp, token, protocol).await
    }

    async fn ws_over(
        &self,
        tcp: TcpStream,
        token: &str,
        protocol: u32,
    ) -> Result<WebSocketStream<Tls>, Resp> {
        let tls = self.handshake(tcp).await;
        let mut request = format!(
            "wss://localhost:{}/api/ws?protocol={protocol}",
            self.addr.port()
        )
        .into_client_request()
        .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tokio_tungstenite::client_async(request, tls).await {
            Ok((ws, _)) => Ok(ws),
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                let body = response.body().clone().unwrap_or_default();
                Err(Resp {
                    status: response.status().as_u16(),
                    headers: HashMap::new(),
                    body,
                })
            }
            Err(e) => panic!("WebSocket : {e}"),
        }
    }
}

fn parse_response(raw: &[u8]) -> Resp {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("réponse HTTP incomplète : {}", String::from_utf8_lossy(raw)));
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.contains("chunked"))
    {
        body = dechunk(&body);
    }
    Resp {
        status,
        headers,
        body,
    }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(end) = data.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(std::str::from_utf8(&data[..end]).unwrap().trim(), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        out.extend_from_slice(&data[end + 2..end + 2 + size]);
        data = &data[end + 2 + size + 2..];
    }
    out
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

/// Prochain message JSON du WebSocket (les ping/pong sont ignorés). `None` : flux fermé.
pub async fn next_json(ws: &mut WebSocketStream<Tls>, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, ws.next()).await {
            Err(_) => panic!("délai dépassé en attendant un message WebSocket"),
            Ok(None) | Ok(Some(Err(_))) => return None,
            Ok(Some(Ok(Message::Text(text)))) => {
                return Some(serde_json::from_str(text.as_str()).unwrap())
            }
            Ok(Some(Ok(Message::Close(_)))) => return None,
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// Lit jusqu'à un événement dont `event.type` vaut `kind` et qui satisfait `accept`.
pub async fn wait_event(
    ws: &mut WebSocketStream<Tls>,
    kind: &str,
    timeout: Duration,
    accept: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "événement « {kind} » non reçu à temps");
        let message = next_json(ws, left).await.expect("WebSocket fermé");
        if message["type"] == "event"
            && message["event"]["type"] == kind
            && accept(&message["event"])
        {
            return message;
        }
    }
}

pub async fn send_text(ws: &mut WebSocketStream<Tls>, text: &str) {
    let _ = ws.send(Message::text(text.to_string())).await;
}

/// Aucun entier supérieur à 2^53 dans le document : un client JavaScript ne perdrait rien.
pub fn assert_no_big_numbers(value: &Value) {
    match value {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                assert!(u <= 1 << 53, "entier trop grand dans le JSON : {u}");
            }
        }
        Value::Array(a) => a.iter().for_each(assert_no_big_numbers),
        Value::Object(o) => o.values().for_each(assert_no_big_numbers),
        _ => {}
    }
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
