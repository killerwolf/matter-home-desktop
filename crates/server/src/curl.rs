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
