//! Adresse du serveur saisie par l'utilisateur.

use super::error::NetError;

/// Port du serveur par défaut.
pub const DEFAULT_PORT: u16 = 8443;

/// Hôte (nom, IPv4 ou IPv6) et port du serveur. Le client ne parle que HTTPS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddr {
    pub host: String,
    pub port: u16,
}

impl ServerAddr {
    /// Accepte `192.168.1.20`, `mac-mini.local:8443`, `https://mac-mini.local:8443/`,
    /// `[fe80::1]:8443`. Le schéma `http://` est refusé : la connexion est toujours chiffrée.
    pub fn parse(text: &str) -> Result<ServerAddr, NetError> {
        let bad = |why: &str| NetError::BadAddress(why.to_string());
        let mut rest = text.trim();
        if let Some(r) = rest.strip_prefix("https://") {
            rest = r;
        } else if rest.contains("://") {
            return Err(bad(
                "seules les adresses https:// sont acceptées (la connexion est toujours chiffrée)",
            ));
        }
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) {
            return Err(bad(
                "saisissez seulement l'adresse du serveur, par exemple 192.168.1.20:8443",
            ));
        }
        let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
            let (host, after) = inner
                .split_once(']')
                .ok_or_else(|| bad("adresse IPv6 mal fermée"))?;
            match after {
                "" => (host, None),
                p => (
                    host,
                    Some(p.strip_prefix(':').ok_or_else(|| bad("port mal écrit"))?),
                ),
            }
        } else if rest.matches(':').count() > 1 {
            // IPv6 sans crochets : pas de port possible.
            (rest, None)
        } else {
            match rest.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (rest, None),
            }
        };
        if host.is_empty() {
            return Err(bad("nom ou adresse du serveur manquant"));
        }
        let port = match port {
            None => DEFAULT_PORT,
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| bad("le port doit être un nombre entre 1 et 65535"))?,
        };
        Ok(ServerAddr {
            host: host.to_string(),
            port,
        })
    }

    /// Plusieurs adresses séparées par des espaces, virgules ou points-virgules (champ « à
    /// distance » de l'écran de connexion). Les doublons sont écartés ; une adresse fausse
    /// refuse toute la liste.
    pub fn parse_list(text: &str) -> Result<Vec<ServerAddr>, NetError> {
        let mut out: Vec<ServerAddr> = Vec::new();
        for part in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
            if part.is_empty() {
                continue;
            }
            let addr = ServerAddr::parse(part)?;
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
        Ok(out)
    }

    /// Adresse du tailnet Tailscale : nom MagicDNS `….ts.net`, IPv4 `100.64.0.0/10` ou IPv6
    /// `fd7a:115c:a1e0::/48`. Sert à savoir qu'on joint le serveur « à distance » (flux caméra
    /// réduit), sans rien demander à Tailscale.
    pub fn is_tailscale(&self) -> bool {
        let host = self.host.to_ascii_lowercase();
        if host.ends_with(".ts.net") {
            return true;
        }
        match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(v4)) => {
                let o = v4.octets();
                o[0] == 100 && (o[1] & 0xc0) == 64
            }
            Ok(std::net::IpAddr::V6(v6)) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
            Err(_) => false,
        }
    }

    /// Forme canonique, celle qui est conservée : `https://hôte:port`.
    pub fn url(&self) -> String {
        if self.host.contains(':') {
            format!("https://[{}]:{}", self.host, self.port)
        } else {
            format!("https://{}:{}", self.host, self.port)
        }
    }

    /// Valeur de l'en-tête `Host` et de l'autorité d'une URL.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> (String, u16) {
        let a = ServerAddr::parse(text).unwrap();
        (a.host, a.port)
    }

    #[test]
    fn accepts_the_usual_spellings() {
        assert_eq!(ok("192.168.1.20"), ("192.168.1.20".into(), 8443));
        assert_eq!(ok(" mac-mini.local:9000 "), ("mac-mini.local".into(), 9000));
        assert_eq!(ok("https://127.0.0.1:8443/"), ("127.0.0.1".into(), 8443));
        assert_eq!(ok("[fe80::1]:9443"), ("fe80::1".into(), 9443));
        assert_eq!(ok("[::1]"), ("::1".into(), 8443));
        assert_eq!(ok("fd00::5"), ("fd00::5".into(), 8443));
    }

    #[test]
    fn refuses_clear_text_and_junk() {
        for bad in [
            "",
            "http://192.168.1.20",
            "ftp://x",
            "host:0",
            "host:99999",
            "host:abc",
            "a/b",
            "user@host",
            "[::1",
            ":8443",
        ] {
            assert!(ServerAddr::parse(bad).is_err(), "{bad} devrait être refusé");
        }
    }

    #[test]
    fn canonical_url_round_trips() {
        for text in ["https://192.168.1.20:8443", "https://[fe80::1]:9443"] {
            assert_eq!(ServerAddr::parse(text).unwrap().url(), text);
        }
        assert_eq!(
            ServerAddr::parse("[::1]").unwrap().authority(),
            "[::1]:8443"
        );
    }

    #[test]
    fn a_list_of_addresses_and_the_tailscale_ones() {
        let l = ServerAddr::parse_list("a.local, 100.64.0.1:9000;a.local\n[fd7a:115c:a1e0::1]").unwrap();
        assert_eq!(l.len(), 3);
        assert!(ServerAddr::parse_list("").unwrap().is_empty());
        assert!(ServerAddr::parse_list("ok.local http://x").is_err());
        let ts = |t: &str| ServerAddr::parse(t).unwrap().is_tailscale();
        assert!(ts("mac.tail1234.ts.net"));
        assert!(ts("MAC.Tail1234.TS.NET:443"));
        assert!(ts("100.100.1.2"));
        assert!(ts("100.127.255.255"));
        assert!(ts("fd7a:115c:a1e0::1"));
        assert!(!ts("100.128.0.1"));
        assert!(!ts("100.63.0.1"));
        assert!(!ts("192.168.1.20"));
        assert!(!ts("mac-mini.local"));
        assert!(!ts("evil-ts.net"));
    }
}
