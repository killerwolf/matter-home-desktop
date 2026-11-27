//! Le `curl` du système, en sous-processus : le chemin réseau commun de la box (`wifi/bbox.rs`) et
//! des extensions (`extensions/`). Un environnement **vide** (ni proxy hérité, ni `~/.curlrc`),
//! l'adresse et les secrets par l'entrée standard (`-K -`, jamais en argument : visibles dans `ps`),
//! des schémas permis explicites (aussi après redirection), un délai et une taille bornés.

use std::io::Write as _;
use std::process::Stdio;
use std::time::Duration;

use crate::descriptions::api::curl_command;

/// Pas de réponse exploitable : `curl` introuvable, échec, délai, réponse trop grosse ou illisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CurlError;

/// Une réponse : statut, cookies posés (`nom=valeur`) et corps.
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) cookies: Vec<String>,
    pub(crate) body: Vec<u8>,
}

/// Lance `curl` avec cette configuration et rend la réponse. `proto` : les schémas permis
/// (`=http,https` pour la box, `=https` pour Internet).
pub(crate) fn run(
    config: &str,
    timeout: Duration,
    proto: &str,
    max_bytes: usize,
) -> Result<Reply, CurlError> {
    let mut child = curl_command()
        .args([
            "-q",
            "-sS",
            "-i",
            "-m",
            &timeout.as_secs().max(1).to_string(),
            "--connect-timeout",
            "5",
            "--proto",
            proto,
            "--proto-redir",
            proto,
            "-g",
            "--noproxy",
            "*",
            "--max-filesize",
            &max_bytes.to_string(),
            "-K",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CurlError)?;
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(config.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CurlError);
        }
    }
    let output = child.wait_with_output().map_err(|_| CurlError)?;
    if !output.status.success() {
        return Err(CurlError);
    }
    parse_reply(&output.stdout).ok_or(CurlError)
}

/// Une valeur entre guillemets pour un fichier de configuration de `curl`, **avec** échappement
/// de `"` et de `\` (ce que `descriptions::api::quoted` refuse) : pour un corps JSON ou un
/// formulaire. `None` : caractère de contrôle (hors tabulation, saut de ligne et retour chariot,
/// qui s'écrivent `\t`, `\n`, `\r`), jamais envoyé à `curl`.
pub(crate) fn quote(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => return None,
            c => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// Un en-tête ou un corps ne doit pas pouvoir ajouter une ligne à la configuration : `quote`
/// écrit tout sur une seule ligne, mais un nom d'en-tête ou une méthode viennent de ce code.
pub(crate) struct Call<'a> {
    pub(crate) method: &'a str,
    pub(crate) url: &'a str,
    pub(crate) headers: &'a [String],
    /// Le corps, envoyé tel quel (`data-raw` : jamais lu comme un nom de fichier).
    pub(crate) body: Option<&'a str>,
}

impl Call<'_> {
    /// La configuration `-K` de l'appel. `None` : une valeur refusée par [`quote`].
    pub(crate) fn config(&self) -> Option<String> {
        let mut text = format!("url = {}\n", quote(self.url)?);
        text.push_str(&format!("request = {}\n", quote(self.method)?));
        for header in self.headers {
            text.push_str(&format!("header = {}\n", quote(header)?));
        }
        text.push_str("header = \"Expect:\"\n");
        if let Some(body) = self.body {
            text.push_str(&format!("data-raw = {}\n", quote(body)?));
        }
        Some(text)
    }
}

/// Sépare la sortie de `curl -i` : statut, cookies et corps (les réponses 1xx sont passées).
pub(crate) fn parse_reply(output: &[u8]) -> Option<Reply> {
    let mut rest = output;
    loop {
        let end = rest.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&rest[..end]).into_owned();
        let body = &rest[end + 4..];
        let mut lines = head.lines();
        let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
        if (100..200).contains(&status) {
            rest = body;
            continue;
        }
        let cookies = lines
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case("set-cookie"))
            .filter_map(|(_, v)| {
                let pair = v.split(';').next()?.trim();
                (pair.contains('=') && !pair.starts_with('=')).then(|| pair.to_string())
            })
            .collect();
        return Some(Reply {
            status,
            cookies,
            body: body.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_gives_status_cookies_and_body() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nSet-Cookie: BBOX_ID=abc123; Path=/; HttpOnly\r\nset-cookie: other=1\r\n\r\n{\"x\":1}";
        let reply = parse_reply(raw).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.cookies, vec!["BBOX_ID=abc123", "other=1"]);
        assert_eq!(reply.body, b"{\"x\":1}");
        assert!(parse_reply(b"n'importe quoi").is_none());
    }
}
