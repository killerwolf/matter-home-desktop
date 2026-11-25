//! Les réglages réseau du serveur, rangés dans `serveur/reseau.json` (0600, jamais exportés) :
//!
//! - les **proxys de confiance** : des réseaux (CIDR) dont le serveur croit l'en-tête
//!   `X-Forwarded-For` pour savoir d'où vient vraiment une requête (voir [`client_ip`]) ;
//! - l'**adresse publique** : le nom en https (`https://ostal.exemple.fr`) sous lequel la maison se
//!   joint de loin, derrière un relais (Caddy…) qui présente un certificat officiel. C'est celle que
//!   les Raccourcis et le widget de l'iPhone, qui refusent un certificat auto-signé, savent joindre.
//!
//! # Proxys de confiance : le défaut
//!
//! Par défaut : `192.168.5.0/24`, `127.0.0.1/32` et `::1/128`.
//!
//! - `192.168.5.0/24` est le réseau privé de la machine virtuelle Colima/Lima : un relais qui tourne
//!   dans Docker sur ce Mac (Caddy) joint le serveur par la passerelle de cette machine virtuelle,
//!   donc le serveur voit `192.168.5.x` et non l'adresse réelle du client. Ce réseau n'existe que
//!   dans le Mac : aucun autre appareil ne peut en émettre une connexion.
//! - la boucle locale (`127.0.0.1`, `::1`) est celle d'un relais lancé directement sur le Mac
//!   (`tailscale serve`). Un programme du Mac qui y écrit un `X-Forwarded-For` ne gagne rien qu'il
//!   ne puisse déjà faire : il est sur la machine.
//!
//! **Attention** : si le réseau de la maison est lui-même en `192.168.5.x`, n'importe quel appareil
//! de la maison pourrait écrire un faux `X-Forwarded-For` et se faire passer pour une autre
//! adresse (échapper au limiteur d'essais de connexion, brouiller le journal). Il faut alors
//! changer ce réglage (`ostal-server network trusted-proxies …`) pour y mettre la vraie adresse du
//! relais.
//!
//! # Lecture et écriture
//!
//! Le serveur relit le fichier dès qu'il change (date, taille, inode) : le changement fait par la
//! ligne de commande ou par la fenêtre d'Ostal Server prend effet à la requête suivante. Un fichier
//! absent vaut les réglages par défaut ; un fichier **illisible** vaut « aucun proxy de confiance,
//! aucune adresse publique » (jamais l'inverse), et n'est jamais écrasé.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use axum::http::{header, HeaderMap};
use serde::{Deserialize, Serialize};

use super::write_secret;

/// Le fichier des réglages réseau.
pub const FILE: &str = "reseau.json";

/// Les proxys de confiance quand rien n'est réglé (voir l'en-tête du module).
pub const DEFAULT_TRUSTED_PROXIES: [&str; 3] = ["192.168.5.0/24", "127.0.0.1/32", "::1/128"];

/// Au plus ce nombre de réseaux de confiance.
const MAX_PROXIES: usize = 32;

// ---------------------------------------------------------------------------
// CIDR
// ---------------------------------------------------------------------------

/// Un réseau IP : une adresse et la longueur de son préfixe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// `192.168.5.0/24`, `::1/128`, ou une adresse seule (`10.0.0.1`, comme `/32`). Les bits
    /// d'hôte sont mis à zéro. Un préfixe trop long ou une adresse illisible est refusé.
    pub fn parse(text: &str) -> Result<Cidr> {
        let text = text.trim();
        let (addr, prefix) = match text.split_once('/') {
            Some((a, p)) => (a.trim(), Some(p.trim())),
            None => (text, None),
        };
        let ip: IpAddr = addr.parse().map_err(|_| {
            anyhow::anyhow!("« {text} » n'est pas un réseau (exemple : 192.168.5.0/24)")
        })?;
        // Une IPv4 écrite en IPv6 (`::ffff:a.b.c.d`) est la même machine : on la range en IPv4.
        let ip = ip.to_canonical();
        let max = if ip.is_ipv4() { 32 } else { 128 };
        let prefix: u8 = match prefix {
            None => max,
            Some(p) => p.parse().ok().filter(|p| *p <= max).ok_or_else(|| {
                anyhow::anyhow!("« {text} » : le préfixe doit aller de 0 à {max}")
            })?,
        };
        Ok(Cidr {
            network: mask(ip, prefix),
            prefix,
        })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.network.is_ipv4() && mask(ip, self.prefix) == self.network
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let keep = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            IpAddr::V4((bits & keep).into())
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let keep = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            IpAddr::V6((bits & keep).into())
        }
    }
}

// ---------------------------------------------------------------------------
// Proxys de confiance et adresse du client
// ---------------------------------------------------------------------------

/// Les réseaux dont l'`X-Forwarded-For` est cru.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedProxies(Vec<Cidr>);

impl TrustedProxies {
    pub fn none() -> Self {
        TrustedProxies(Vec::new())
    }

    pub fn parse_list<S: AsRef<str>>(items: &[S]) -> Result<Self> {
        if items.len() > MAX_PROXIES {
            bail!("au plus {MAX_PROXIES} réseaux de confiance");
        }
        items
            .iter()
            .map(|s| Cidr::parse(s.as_ref()))
            .collect::<Result<Vec<_>>>()
            .map(TrustedProxies)
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|c| c.contains(ip))
    }

    pub fn list(&self) -> Vec<String> {
        self.0.iter().map(Cidr::to_string).collect()
    }
}

impl Default for TrustedProxies {
    fn default() -> Self {
        Self::parse_list(&DEFAULT_TRUSTED_PROXIES).expect("défaut valide")
    }
}

/// Une adresse d'un `X-Forwarded-For` : `1.2.3.4`, `[::1]`, `1.2.3.4:5678`, `[::1]:5678`.
fn parse_forwarded(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Some(ip.to_canonical());
    }
    if let Ok(sock) = entry.parse::<std::net::SocketAddr>() {
        return Some(sock.ip().to_canonical());
    }
    entry
        .strip_prefix('[')
        .and_then(|e| e.strip_suffix(']'))
        .and_then(|e| e.parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
}

/// L'adresse du client d'une requête. **La seule fonction qui la décide**, utilisée partout où elle
/// sert (classement « sur place / à distance », limiteur d'essais, journal d'audit, MCP).
///
/// - Le pair TCP n'est pas un proxy de confiance : c'est lui, et l'`X-Forwarded-For` est ignoré
///   (il serait forgé).
/// - Le pair est un proxy de confiance et il y a un `X-Forwarded-For` : on lit **toutes** ses lignes,
///   de droite à gauche. Chaque proxy de confiance ajoute à droite l'adresse de qui lui a parlé ;
///   la première adresse qui n'est pas elle-même un proxy de confiance est donc le vrai client, et
///   ce qui est plus à gauche vient du client (forgeable) et n'est jamais lu. Une valeur illisible
///   arrête la lecture : on garde le pair (jamais une valeur plus à gauche).
/// - Sinon : le pair.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &TrustedProxies) -> IpAddr {
    let peer = peer.to_canonical();
    if !trusted.contains(peer) {
        return peer;
    }
    let forwarded: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .collect();
    for entry in forwarded.iter().rev() {
        if entry.trim().is_empty() {
            // Une virgule en trop : rien à lire ici, on regarde la valeur d'avant.
            continue;
        }
        match parse_forwarded(entry) {
            Some(ip) if trusted.contains(ip) => continue,
            Some(ip) => return ip,
            None => return peer,
        }
    }
    peer
}

/// L'adresse du client d'une requête, posée une fois par la boucle d'acceptation (`net::accept_loop`)
/// et lue par les gestionnaires : aucun ne regarde le pair TCP directement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<ClientIp>()
            .copied()
            .unwrap_or(ClientIp(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))))
    }
}

// ---------------------------------------------------------------------------
// Adresse publique
// ---------------------------------------------------------------------------

/// L'adresse publique réglée : `https://nom` (sans port, sans chemin).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicAddress {
    origin: String,
    host: String,
}

impl PublicAddress {
    /// `https://ostal.exemple.fr` (un `/` final est toléré). Un nom de domaine seulement : ni
    /// adresse IP, ni port, ni chemin, ni identifiant, ni `http://`.
    pub fn parse(text: &str) -> Result<PublicAddress> {
        let bad = |why: &str| anyhow::anyhow!("adresse publique refusée : {why}");
        let rest = text
            .trim()
            .strip_prefix("https://")
            .ok_or_else(|| bad("elle doit commencer par https://"))?;
        let host = rest.trim_end_matches('/').to_ascii_lowercase();
        if host.contains(['/', '?', '#', '@', ':', ' ']) {
            return Err(bad(
                "écrivez seulement le nom, sans chemin ni port (par exemple https://ostal.exemple.fr)",
            ));
        }
        let well_formed = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            && host.split('.').all(|l| {
                !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-')
            });
        if !well_formed || !host.contains('.') {
            return Err(bad("ce n'est pas un nom de domaine"));
        }
        if host.parse::<IpAddr>().is_ok() || host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return Err(bad("une adresse IP n'a pas de certificat officiel"));
        }
        Ok(PublicAddress {
            origin: format!("https://{host}"),
            host,
        })
    }

    /// `https://nom`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// Cet en-tête `Host` est-il celui de l'adresse publique, ou un nom qui se termine comme elle
    /// (`ostal.exemple.fr`, `x.ostal.exemple.fr`, avec ou sans port) ? La frontière est un point :
    /// `evilostal.exemple.fr` n'en est pas.
    pub fn matches_host(&self, host_header: &str) -> bool {
        let host = host_name(host_header);
        host == self.host || host.ends_with(&format!(".{}", self.host))
    }
}

/// Le nom d'un en-tête `Host`, en minuscules, sans le port numérique.
pub fn host_name(host_header: &str) -> String {
    let h = host_header.trim();
    let name = match h.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => h,
    };
    name.to_ascii_lowercase()
}

/// L'en-tête `Host` d'une requête, sans le port.
pub fn request_host(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(host_name)
}

// ---------------------------------------------------------------------------
// Le fichier
// ---------------------------------------------------------------------------

/// Les réglages lus, prêts à servir.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub trusted: TrustedProxies,
    pub public: Option<PublicAddress>,
    /// Le fichier existe mais ne se lit pas : aucun proxy, aucune adresse (et il n'est pas écrasé).
    pub unreadable: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    /// Absent : le défaut. Présent (même vide) : exactement cette liste.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trusted_proxies: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_address: Option<String>,
}

fn parse_file(text: &str) -> Result<Settings> {
    let file: File = serde_json::from_str(text).context("fichier illisible")?;
    let trusted = match &file.trusted_proxies {
        None => TrustedProxies::default(),
        Some(list) => TrustedProxies::parse_list(list)?,
    };
    let public = match file
        .public_address
        .as_deref()
        .filter(|a| !a.trim().is_empty())
    {
        None => None,
        Some(a) => Some(PublicAddress::parse(a)?),
    };
    Ok(Settings {
        trusted,
        public,
        unreadable: false,
    })
}

fn path_of(server_dir: &Path) -> PathBuf {
    server_dir.join(FILE)
}

/// Lit les réglages d'un dossier `serveur/` (sans rien écrire).
pub fn read(server_dir: &Path) -> Settings {
    match std::fs::read_to_string(path_of(server_dir)) {
        Ok(text) => parse_file(&text).unwrap_or_else(|e| {
            log::warn!(
                "{FILE} illisible ({e:#}) : aucun proxy de confiance, aucune adresse publique"
            );
            Settings {
                trusted: TrustedProxies::none(),
                public: None,
                unreadable: true,
            }
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            log::warn!("{FILE} : {e} : aucun proxy de confiance, aucune adresse publique");
            Settings {
                trusted: TrustedProxies::none(),
                public: None,
                unreadable: true,
            }
        }
    }
}

/// Le fichier tel qu'il est, pour le modifier : jamais un fichier illisible (on ne l'écrase pas).
fn load_for_write(server_dir: &Path) -> Result<File> {
    match std::fs::read_to_string(path_of(server_dir)) {
        Ok(text) => {
            let file: File = serde_json::from_str(&text).with_context(|| {
                format!("{FILE} est illisible : corrigez-le à la main, il n'est pas écrasé")
            })?;
            // Une liste invalide aussi bloque l'écriture : on ne la remplace pas sans le dire.
            parse_file(&text).with_context(|| {
                format!("{FILE} contient une valeur refusée : corrigez-le à la main")
            })?;
            Ok(file)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(e).with_context(|| format!("lecture de {FILE}")),
    }
}

fn save(server_dir: &Path, mut file: File) -> Result<()> {
    file.version = 1;
    let mut text = serde_json::to_string_pretty(&file)?;
    text.push('\n');
    write_secret(&path_of(server_dir), text.as_bytes())
}

/// Règle l'adresse publique (`None` : l'efface). Rend l'adresse retenue, sous sa forme normale.
pub fn set_public_address(server_dir: &Path, address: Option<&str>) -> Result<Option<String>> {
    let parsed = match address.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => Some(PublicAddress::parse(a)?),
        None => None,
    };
    let mut file = load_for_write(server_dir)?;
    file.public_address = parsed.as_ref().map(|p| p.origin().to_string());
    save(server_dir, file)?;
    Ok(parsed.map(|p| p.origin().to_string()))
}

/// Règle les proxys de confiance (`None` : retour au défaut ; liste vide : aucun).
pub fn set_trusted_proxies(server_dir: &Path, list: Option<&[String]>) -> Result<Vec<String>> {
    let parsed = match list {
        Some(list) => TrustedProxies::parse_list(list)?,
        None => TrustedProxies::default(),
    };
    let mut file = load_for_write(server_dir)?;
    file.trusted_proxies = parsed_list(list, &parsed);
    save(server_dir, file)?;
    Ok(parsed.list())
}

fn parsed_list(asked: Option<&[String]>, parsed: &TrustedProxies) -> Option<Vec<String>> {
    asked.map(|_| parsed.list())
}

/// Les réglages en mémoire, relus quand le fichier change.
pub struct NetworkStore {
    dir: PathBuf,
    cache: Mutex<(Option<Stamp>, Arc<Settings>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    #[cfg(unix)]
    inode: u64,
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: meta.modified().ok(),
        len: meta.len(),
        #[cfg(unix)]
        inode: std::os::unix::fs::MetadataExt::ino(&meta),
    })
}

impl NetworkStore {
    pub fn open(server_dir: &Path) -> NetworkStore {
        let dir = server_dir.to_path_buf();
        let stamp = stamp_of(&path_of(&dir));
        let settings = Arc::new(read(&dir));
        NetworkStore {
            dir,
            cache: Mutex::new((stamp, settings)),
        }
    }

    /// Des réglages fixes, sans fichier (essais).
    pub fn fixed(settings: Settings) -> NetworkStore {
        NetworkStore {
            dir: PathBuf::new(),
            cache: Mutex::new((None, Arc::new(settings))),
        }
    }

    /// Les réglages du moment : le fichier est relu s'il a changé (ou apparu, ou disparu).
    pub fn current(&self) -> Arc<Settings> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if self.dir.as_os_str().is_empty() {
            return cache.1.clone();
        }
        let stamp = stamp_of(&path_of(&self.dir));
        if stamp != cache.0 {
            cache.1 = Arc::new(read(&self.dir));
            cache.0 = stamp;
        }
        cache.1.clone()
    }

    /// L'adresse du client d'une requête (voir [`client_ip`]).
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        client_ip(peer, headers, &self.current().trusted)
    }

    pub fn is_trusted_proxy(&self, peer: IpAddr) -> bool {
        self.current().trusted.contains(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn headers(lines: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for l in lines {
            h.append("x-forwarded-for", HeaderValue::from_str(l).unwrap());
        }
        h
    }

    #[test]
    fn cidr_parses_masks_and_matches() {
        let c = Cidr::parse("192.168.5.77/24").unwrap();
        assert_eq!(c.to_string(), "192.168.5.0/24");
        assert!(c.contains(ip("192.168.5.2")));
        assert!(!c.contains(ip("192.168.6.2")));
        // IPv4 vue sur une socket IPv6.
        assert!(c.contains(ip("::ffff:192.168.5.2")));
        assert!(Cidr::parse("10.0.0.1").unwrap().contains(ip("10.0.0.1")));
        assert!(!Cidr::parse("10.0.0.1").unwrap().contains(ip("10.0.0.2")));
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        assert!(Cidr::parse("::1/128").unwrap().contains(ip("::1")));
        assert!(!Cidr::parse("::1/128").unwrap().contains(ip("127.0.0.1")));
        assert!(Cidr::parse("fd7a:115c:a1e0::/48")
            .unwrap()
            .contains(ip("fd7a:115c:a1e0::5")));
        for bad in [
            "",
            "abc",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/-1",
            "10.0.0.0/x",
            "1.2.3/24",
        ] {
            assert!(Cidr::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_default_trusts_the_colima_gateway_and_loopback_only() {
        let t = TrustedProxies::default();
        assert_eq!(t.list(), ["192.168.5.0/24", "127.0.0.1/32", "::1/128"]);
        assert!(t.contains(ip("192.168.5.2")));
        assert!(t.contains(ip("127.0.0.1")));
        assert!(t.contains(ip("::1")));
        assert!(t.contains(ip("::ffff:127.0.0.1")));
        assert!(!t.contains(ip("192.168.1.20")));
        assert!(!t.contains(ip("100.64.0.1")));
    }

    #[test]
    fn a_trusted_peer_gives_the_forwarded_client() {
        let t = TrustedProxies::default();
        assert_eq!(
            client_ip(ip("192.168.5.2"), &headers(&["100.101.102.103"]), &t),
            ip("100.101.102.103")
        );
        assert_eq!(
            client_ip(ip("127.0.0.1"), &headers(&["100.101.102.103"]), &t),
            ip("100.101.102.103")
        );
        assert_eq!(
            client_ip(
                ip("::ffff:192.168.5.2"),
                &headers(&["[fd7a:115c:a1e0::1]"]),
                &t
            ),
            ip("fd7a:115c:a1e0::1")
        );
        assert_eq!(
            client_ip(ip("192.168.5.2"), &headers(&["100.64.0.9:51000"]), &t),
            ip("100.64.0.9")
        );
        // Pas d'en-tête : le pair.
        assert_eq!(
            client_ip(ip("192.168.5.2"), &HeaderMap::new(), &t),
            ip("192.168.5.2")
        );
    }

    #[test]
    fn an_untrusted_peer_cannot_forge_a_client() {
        let t = TrustedProxies::default();
        // Un appareil de la maison qui prétend venir d'ailleurs : ignoré.
        assert_eq!(
            client_ip(ip("192.168.1.20"), &headers(&["100.64.0.1"]), &t),
            ip("192.168.1.20")
        );
        assert_eq!(
            client_ip(
                ip("203.0.113.9"),
                &headers(&["127.0.0.1", "100.64.0.1"]),
                &t
            ),
            ip("203.0.113.9")
        );
        // Sans aucun proxy de confiance, même la boucle locale est crue sur parole : elle ne l'est pas.
        let none = TrustedProxies::none();
        assert_eq!(
            client_ip(ip("127.0.0.1"), &headers(&["100.64.0.1"]), &none),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn several_values_are_read_from_the_right_and_the_left_is_never_trusted() {
        let t = TrustedProxies::default();
        // Le client a ajouté « 1.1.1.1 » ; le relais a ajouté la vraie adresse à droite.
        assert_eq!(
            client_ip(ip("192.168.5.2"), &headers(&["1.1.1.1, 100.64.0.7"]), &t),
            ip("100.64.0.7")
        );
        // Deux lignes d'en-tête : la dernière est celle du relais.
        assert_eq!(
            client_ip(ip("192.168.5.2"), &headers(&["1.1.1.1", "100.64.0.7"]), &t),
            ip("100.64.0.7")
        );
        // Un autre proxy de confiance à droite est sauté.
        assert_eq!(
            client_ip(
                ip("192.168.5.2"),
                &headers(&["1.1.1.1, 100.64.0.7, 192.168.5.9"]),
                &t
            ),
            ip("100.64.0.7")
        );
        // Que des proxys : le pair.
        assert_eq!(
            client_ip(ip("192.168.5.2"), &headers(&["192.168.5.9, 127.0.0.1"]), &t),
            ip("192.168.5.2")
        );
    }

    #[test]
    fn an_unreadable_value_stops_the_walk() {
        let t = TrustedProxies::default();
        // La valeur de droite est illisible : on ne remonte pas vers la gauche (forgeable).
        assert_eq!(
            client_ip(
                ip("192.168.5.2"),
                &headers(&["100.64.0.1, n'importe quoi"]),
                &t
            ),
            ip("192.168.5.2")
        );
        assert_eq!(
            client_ip(ip("192.168.5.2"), &headers(&["unknown"]), &t),
            ip("192.168.5.2")
        );
        // Une virgule en trop n'est pas une valeur.
        assert_eq!(
            client_ip(ip("192.168.5.2"), &headers(&["100.64.0.1,"]), &t),
            ip("100.64.0.1")
        );
        // Un en-tête qui n'est pas du texte est ignoré.
        let mut h = HeaderMap::new();
        h.append(
            "x-forwarded-for",
            HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        assert_eq!(client_ip(ip("192.168.5.2"), &h, &t), ip("192.168.5.2"));
    }

    #[test]
    fn public_address_is_a_bare_https_name() {
        let a = PublicAddress::parse("  https://Ostal.Klbr.H4md1.fr/ ").unwrap();
        assert_eq!(a.origin(), "https://ostal.klbr.h4md1.fr");
        for bad in [
            "",
            "ostal.klbr.h4md1.fr",
            "http://ostal.klbr.h4md1.fr",
            "https://",
            "https://ostal.klbr.h4md1.fr/app/",
            "https://ostal.klbr.h4md1.fr:8443",
            "https://user@ostal.klbr.h4md1.fr",
            "https://ostal.klbr.h4md1.fr?x=1",
            "https://localhost",
            "https://100.101.102.103",
            "https://192.168.1.2",
            "https://-a.b.fr",
            "https://a..fr",
            "https://exemple.fr\"<",
        ] {
            assert!(PublicAddress::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn host_matching_stops_at_a_label_boundary() {
        let a = PublicAddress::parse("https://ostal.klbr.h4md1.fr").unwrap();
        assert!(a.matches_host("ostal.klbr.h4md1.fr"));
        assert!(a.matches_host("OSTAL.klbr.h4md1.fr:443"));
        assert!(a.matches_host("x.ostal.klbr.h4md1.fr"));
        assert!(!a.matches_host("evilostal.klbr.h4md1.fr"));
        assert!(!a.matches_host("ostal.klbr.h4md1.fr.evil.com"));
        assert!(!a.matches_host("klbr.h4md1.fr"));
        assert!(!a.matches_host("mac.tailnet-1.ts.net"));
        assert!(!a.matches_host(""));
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ostal-net-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_file_is_the_default_and_a_broken_one_trusts_nothing() {
        let dir = temp("file");
        let s = read(&dir);
        assert_eq!(s.trusted, TrustedProxies::default());
        assert!(s.public.is_none() && !s.unreadable);
        std::fs::write(dir.join(FILE), "{ pas du json").unwrap();
        let s = read(&dir);
        assert!(s.unreadable && s.public.is_none());
        assert!(!s.trusted.contains(ip("127.0.0.1")));
        // Jamais écrasé.
        assert!(set_public_address(&dir, Some("https://ostal.exemple.fr")).is_err());
        assert!(set_trusted_proxies(&dir, None).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).unwrap(),
            "{ pas du json"
        );
        // Une valeur refusée dans le fichier vaut « rien de confiance » aussi.
        std::fs::write(dir.join(FILE), r#"{"public_address":"http://x.fr"}"#).unwrap();
        assert!(read(&dir).unreadable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_are_written_reloaded_and_the_file_is_private() {
        let dir = temp("write");
        let store = NetworkStore::open(&dir);
        assert!(store.current().public.is_none());
        let got = set_public_address(&dir, Some("https://Ostal.exemple.fr/")).unwrap();
        assert_eq!(got.as_deref(), Some("https://ostal.exemple.fr"));
        assert_eq!(
            store
                .current()
                .public
                .as_ref()
                .map(|p| p.origin().to_string())
                .as_deref(),
            Some("https://ostal.exemple.fr")
        );
        // Le défaut est gardé tant qu'on n'a rien dit des proxys.
        assert!(store.is_trusted_proxy(ip("192.168.5.2")));
        let list = vec!["10.1.2.0/24".to_string()];
        assert_eq!(
            set_trusted_proxies(&dir, Some(&list)).unwrap(),
            ["10.1.2.0/24"]
        );
        assert!(!store.is_trusted_proxy(ip("192.168.5.2")));
        assert!(store.is_trusted_proxy(ip("10.1.2.3")));
        // L'adresse publique a survécu à ce changement ; liste vide : aucun proxy.
        assert!(store.current().public.is_some());
        set_trusted_proxies(&dir, Some(&[])).unwrap();
        assert!(!store.is_trusted_proxy(ip("127.0.0.1")));
        set_trusted_proxies(&dir, None).unwrap();
        assert!(store.is_trusted_proxy(ip("127.0.0.1")));
        set_public_address(&dir, None).unwrap();
        assert!(store.current().public.is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(set_public_address(&dir, Some("n'importe quoi")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
