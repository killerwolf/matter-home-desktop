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
}
