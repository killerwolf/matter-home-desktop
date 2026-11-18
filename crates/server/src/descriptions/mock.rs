//! Le faux serveur de l'API Anthropic des tests : un serveur HTTP local (127.0.0.1, port éphémère),
//! qui note les requêtes et répond dans l'ordre. Les tests n'appellent jamais la vraie API.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Une requête reçue.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// Ligne de requête et en-têtes, en minuscules.
    pub head: String,
    pub body: Vec<u8>,
}

impl Recorded {
    /// La valeur d'un en-tête (nom en minuscules).
    pub fn header(&self, name: &str) -> Option<String> {
        self.head
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
            .map(str::to_string)
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("corps JSON")
    }
}

/// Une réponse programmée.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
    /// Attente avant de répondre (un serveur lent).
    pub delay: Duration,
}

impl Reply {
    pub fn status(status: u16) -> Reply {
        Reply {
            status,
            delay: Duration::ZERO,
            headers: Vec::new(),
            body: r#"{"type":"error","error":{"type":"overloaded_error","message":"occupé"}}"#
                .into(),
        }
    }

    pub fn with_delay(mut self, delay: Duration) -> Reply {
        self.delay = delay;
        self
    }

    pub fn with_header(mut self, name: &'static str, value: &str) -> Reply {
        self.headers.push((name, value.to_string()));
        self
    }

    /// Une réponse 200 de l'API : le texte du bloc est le JSON de la sortie structurée.
    pub fn described(description: &str, niveau: u8) -> Reply {
        let text = serde_json::json!({ "description": description, "niveau": niveau }).to_string();
        Reply::message("end_turn", &text)
    }

    pub fn message(stop_reason: &str, text: &str) -> Reply {
        Reply {
            status: 200,
            delay: Duration::ZERO,
            headers: Vec::new(),
            body: serde_json::json!({
                "id": "msg_test",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5-5",
                "content": [{ "type": "text", "text": text }],
                "stop_reason": stop_reason,
                "usage": { "input_tokens": 1500, "output_tokens": 60 }
            })
            .to_string(),
        }
    }
}

pub struct Mock {
    pub port: u16,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
}

impl Mock {
    /// Sert `replies` dans l'ordre ; la dernière est répétée.
    pub fn start(replies: Vec<Reply>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").expect("port local");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, halt) = (requests.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut served = 0usize;
            while !halt.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let reply = replies
                            .get(served)
                            .or(replies.last())
                            .cloned()
                            .unwrap_or_else(|| Reply::status(500));
                        served += 1;
                        if let Some(rec) = serve(stream, &reply) {
                            seen.lock().unwrap().push(rec);
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Mock {
            port,
            requests,
            stop,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn request(&self, n: usize) -> Recorded {
        self.requests.lock().unwrap()[n].clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(mut stream: TcpStream, reply: &Reply) -> Option<Recorded> {
    stream.set_nonblocking(false).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    let end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..end]).to_lowercase();
    let length = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length: "))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = data[end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    std::thread::sleep(reply.delay);
    let mut out = format!("HTTP/1.1 {} X\r\n", reply.status);
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!(
        "content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        reply.body.len(),
        reply.body
    ));
    let _ = stream.write_all(out.as_bytes());
    Some(Recorded { head, body })
}

/// Un petit JPEG valide (gris), pour les tests.
pub fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let pixels = vec![128u8; usize::from(width) * usize::from(height) * 3];
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 70)
        .encode(&pixels, width, height, jpeg_encoder::ColorType::Rgb)
        .expect("encodage");
    out
}
