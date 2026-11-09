//! Client HTTP/1.1 minimal par-dessus la connexion TLS épinglée : une requête, une connexion,
//! `Connection: close`. Pas de réutilisation de connexion : le serveur plafonne à 16 connexions
//! par adresse, la file de requêtes (`session.rs`) en garde bien moins en même temps.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::addr::ServerAddr;
use super::error::NetError;
use super::tls::{self, io_reason, Trust};
use protocol::wire::ErrorBody;

/// Taille maximale acceptée pour une réponse (un export complet peut peser plusieurs Mo).
const MAX_BODY: usize = 256 * 1024 * 1024;
const MAX_HEAD: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub enum Body {
    None,
    Json(Vec<u8>),
    /// Contenu brut (sauvegarde `.matterhome`, qui est du JSON).
    Bytes(Vec<u8>),
}

/// Une requête à l'API. Le jeton est ajouté par l'envoyeur, jamais écrit ici.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: &'static str,
    /// Chemin et paramètres, par exemple `/api/devices/18/history?since=12`.
    pub path: String,
    pub body: Body,
    /// Durée maximale de toute la requête.
    pub timeout: Duration,
}

impl Request {
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn new(method: &'static str, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            body: Body::None,
            timeout: Self::DEFAULT_TIMEOUT,
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new("GET", path)
    }

    pub fn json(method: &'static str, path: impl Into<String>, body: &impl Serialize) -> Self {
        Self {
            body: Body::Json(serde_json::to_vec(body).unwrap_or_default()),
            ..Self::new(method, path)
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    /// Noms en minuscules.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, NetError> {
        serde_json::from_slice(&self.body)
            .map_err(|e| NetError::Protocol(format!("JSON attendu ({e})")))
    }

    /// Transforme une réponse d'erreur (statut 4xx/5xx) en [`NetError::Api`].
    pub fn into_result(self) -> Result<Response, NetError> {
        if self.status < 400 {
            return Ok(self);
        }
        let retry_header = self
            .header("retry-after")
            .and_then(|v| v.trim().parse().ok());
        let (code, detail, retry) = match serde_json::from_slice::<ErrorBody>(&self.body) {
            Ok(b) => (
                Some(b.error.code),
                b.error.detail,
                b.error.retry_after_s.or(retry_header),
            ),
            Err(_) => (None, None, retry_header),
        };
        Err(NetError::Api {
            status: self.status,
            code,
            detail,
            retry_after_s: retry,
        })
    }
}

/// Exécute la requête et rend la réponse **quel que soit son statut** (voir
/// [`Response::into_result`]).
pub fn exec(
    addr: &ServerAddr,
    trust: &Trust,
    token: Option<&str>,
    request: &Request,
) -> Result<Response, NetError> {
    let deadline = Instant::now() + request.timeout;
    let tls::Connected { mut stream, .. } = tls::connect(addr, trust)?;
    stream
        .sock
        .set_read_timeout(Some(request.timeout.min(Duration::from_secs(30))))
        .ok();
    stream
        .sock
        .set_write_timeout(Some(Duration::from_secs(30)))
        .ok();

    let body: &[u8] = match &request.body {
        Body::None => &[],
        Body::Json(b) | Body::Bytes(b) => b,
    };
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: matter-home-desktop\r\nAccept: */*\r\nConnection: close\r\n",
        request.method,
        request.path,
        addr.authority()
    );
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if !matches!(request.body, Body::None) || matches!(request.method, "POST" | "PUT") {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    let write = |stream: &mut tls::TlsStream| -> std::io::Result<()> {
        stream.write_all(head.as_bytes())?;
        stream.write_all(body)?;
        // Indispensable : rustls garde la fin des données dans un tampon qui ne part que sur
        // `flush` ou à la lecture suivante ; sans lui le serveur attendrait la suite du corps.
        stream.flush()
    };
    write(&mut stream).map_err(|e| io_error(&e))?;
    read_response(&mut stream, deadline)
}

fn io_error(e: &std::io::Error) -> NetError {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => NetError::Timeout,
        _ => NetError::Unreachable(io_reason(e)),
    }
}

fn read_some(
    stream: &mut impl Read,
    buf: &mut Vec<u8>,
    deadline: Instant,
) -> Result<usize, NetError> {
    if Instant::now() >= deadline {
        return Err(NetError::Timeout);
    }
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                return Ok(n);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // Fermeture sans `close_notify` : le serveur a fini de parler.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(0),
            Err(e) => return Err(io_error(&e)),
        }
    }
}

fn read_response(stream: &mut impl Read, deadline: Instant) -> Result<Response, NetError> {
    let proto = |why: &str| NetError::Protocol(why.to_string());
    let mut buf = Vec::new();
    let (status, headers, head_len) = loop {
        let mut raw = [httparse::EMPTY_HEADER; 48];
        let mut parsed = httparse::Response::new(&mut raw);
        match parsed.parse(&buf) {
            Ok(httparse::Status::Complete(n)) => {
                let status = parsed.code.ok_or_else(|| proto("statut manquant"))?;
                let headers: Vec<(String, String)> = parsed
                    .headers
                    .iter()
                    .map(|h| {
                        (
                            h.name.to_ascii_lowercase(),
                            String::from_utf8_lossy(h.value).trim().to_string(),
                        )
                    })
                    .collect();
                break (status, headers, n);
            }
            Ok(httparse::Status::Partial) => {
                if buf.len() > MAX_HEAD {
                    return Err(proto("en-têtes trop longs"));
                }
                if read_some(stream, &mut buf, deadline)? == 0 {
                    return Err(NetError::Unreachable("connexion coupée".into()));
                }
            }
            Err(_) => return Err(proto("réponse HTTP malformée")),
        }
    };
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let mut rest = buf.split_off(head_len);
    let no_body = status == 204 || status == 304 || (100..200).contains(&status);
    let body = if no_body {
        Vec::new()
    } else if header("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        read_chunked(stream, rest, deadline)?
    } else if let Some(len) = header("content-length") {
        let len: usize = len.parse().map_err(|_| proto("Content-Length invalide"))?;
        if len > MAX_BODY {
            return Err(proto("réponse trop volumineuse"));
        }
        while rest.len() < len {
            if read_some(stream, &mut rest, deadline)? == 0 {
                return Err(NetError::Unreachable("connexion coupée".into()));
            }
        }
        rest.truncate(len);
        rest
    } else {
        // Corps délimité par la fermeture de la connexion.
        while read_some(stream, &mut rest, deadline)? > 0 {
            if rest.len() > MAX_BODY {
                return Err(proto("réponse trop volumineuse"));
            }
        }
        rest
    };
    Ok(Response {
        status,
        headers,
        body,
    })
}

fn read_chunked(
    stream: &mut impl Read,
    mut buf: Vec<u8>,
    deadline: Instant,
) -> Result<Vec<u8>, NetError> {
    let proto = |why: &str| NetError::Protocol(why.to_string());
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        // Ligne de taille.
        let line_end = loop {
            if let Some(i) = buf[pos..].windows(2).position(|w| w == b"\r\n") {
                break pos + i;
            }
            if read_some(stream, &mut buf, deadline)? == 0 {
                return Err(proto("corps tronqué"));
            }
        };
        let size_text = std::str::from_utf8(&buf[pos..line_end])
            .map_err(|_| proto("taille de morceau illisible"))?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| proto("taille de morceau invalide"))?;
        pos = line_end + 2;
        if size == 0 {
            return Ok(out);
        }
        if out.len() + size > MAX_BODY {
            return Err(proto("réponse trop volumineuse"));
        }
        while buf.len() < pos + size + 2 {
            if read_some(stream, &mut buf, deadline)? == 0 {
                return Err(proto("corps tronqué"));
            }
        }
        out.extend_from_slice(&buf[pos..pos + size]);
        pos += size + 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &[u8]) -> Result<Response, NetError> {
        read_response(&mut &raw[..], Instant::now() + Duration::from_secs(5))
    }

    #[test]
    fn reads_content_length_and_chunked_and_close_delimited() {
        let r = parse(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: b\r\n\r\nhello").unwrap();
        assert_eq!(
            (r.status, r.body.as_slice(), r.header("x-a")),
            (200, &b"hello"[..], Some("b"))
        );
        let r = parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!(r.body, b"abcde");
        let r = parse(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nfin de flux").unwrap();
        assert_eq!(r.body, b"fin de flux");
        let r = parse(b"HTTP/1.1 202 Accepted\r\n\r\n").unwrap();
        assert_eq!((r.status, r.body.len()), (202, 0));
        let r = parse(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert!(r.body.is_empty());
    }

    #[test]
    fn truncated_or_garbled_responses_are_errors() {
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort").is_err());
        assert!(parse(b"n'importe quoi").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999\r\n\r\n").is_err());
        assert!(parse(b"").is_err());
    }

    #[test]
    fn error_bodies_become_api_errors_with_the_code() {
        let body = br#"{"error":{"code":"too_many_attempts","retry_after_s":30}}"#;
        let raw = [
            format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
            body,
        ]
        .concat();
        match parse(&raw).unwrap().into_result() {
            Err(NetError::Api {
                status: 429,
                code: Some(protocol::wire::ErrorCode::TooManyAttempts),
                retry_after_s: Some(30),
                ..
            }) => {}
            other => panic!("{other:?}"),
        }
        let r = parse(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 2\r\n\r\nhi").unwrap();
        assert!(matches!(
            r.into_result(),
            Err(NetError::Api {
                status: 502,
                code: None,
                ..
            })
        ));
    }
}
