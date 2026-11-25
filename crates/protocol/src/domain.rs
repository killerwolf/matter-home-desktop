//! Ce qui sépare un nom de domaine public d'un nom du réseau local : une seule définition, pour
//! l'app (qui juge la chaîne de certificats d'une adresse saisie) et pour le serveur (qui refuse une
//! adresse publique qui n'en serait pas une).

/// Fins de nom qui ne désignent jamais un domaine public (réseau local, usages réservés).
const LOCAL_SUFFIXES: &[&str] = &[
    "local",
    "localhost",
    "lan",
    "internal",
    "intranet",
    "corp",
    "home",
    "private",
    "test",
    "invalid",
    "arpa",
];

/// Vrai pour un nom de domaine qui peut avoir un certificat public : pas une IP, au moins deux
/// étiquettes valables, une fin de nom qui n'est ni numérique ni réservée au réseau local
/// (`.local`, `.lan`, `.internal`…). Un nom sans point (`mac-mini`) n'en est pas un.
pub fn is_public_name(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let valid = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let tld = labels[labels.len() - 1];
    labels.iter().all(valid)
        && !tld.chars().all(|c| c.is_ascii_digit())
        && !LOCAL_SUFFIXES.contains(&tld)
}
