//! Se trouver sur le réseau de la maison : Ostal Server s'annonce en Bonjour (mDNS, service
//! `_ostal._tcp`), l'app Mac le cherche à sa première ouverture. Ce module ne contient que ce que
//! les deux côtés partagent, sans réseau : le nom du service, ce que dit l'annonce (TXT), le choix
//! des adresses à annoncer et le nom de l'instance.
//!
//! L'annonce ne porte aucun secret et rien ne s'y fie : l'app se connecte à l'adresse trouvée comme
//! à une adresse saisie à la main, et le certificat se confirme par son empreinte à la première
//! connexion (ou doit être celui déjà épinglé). Une annonce falsifiée ne peut donc qu'amener à une
//! empreinte inconnue, que l'utilisateur voit.

use std::net::Ipv4Addr;

/// Le type du service Bonjour, tel que le demande et l'annonce `matc` (nom DNS complet).
pub const SERVICE: &str = "_ostal._tcp.local";

/// Clés du TXT.
const KEY_VERSION: &str = "v";
const KEY_FINGERPRINT: &str = "fp";

/// Longueur maximale d'une étiquette DNS (le nom de l'instance en est une).
const MAX_LABEL: usize = 63;

/// Ce que l'annonce dit du serveur, en dehors de son nom, son adresse et son port.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Txt {
    /// Version du protocole la plus récente que parle le serveur.
    pub protocol: Option<u32>,
    /// Empreinte SHA-256 de son certificat (`AA:BB:…`) : publique de toute façon, la poignée de
    /// main TLS la montre. Elle sert à reconnaître « le serveur déjà approuvé ».
    pub fingerprint: Option<String>,
}

/// Le TXT à annoncer.
pub fn txt_records(protocol: u32, fingerprint: &str) -> Vec<(String, String)> {
    vec![
        (KEY_VERSION.to_string(), protocol.to_string()),
        (KEY_FINGERPRINT.to_string(), fingerprint.to_string()),
    ]
}

/// Découpe le contenu brut d'un enregistrement TXT (suite de chaînes précédées de leur longueur,
/// RFC 6763) en entrées `clé=valeur`. Une longueur qui dépasse la fin arrête la lecture.
pub fn txt_entries(rdata: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = rdata;
    while let Some((&len, tail)) = rest.split_first() {
        let len = usize::from(len);
        if len == 0 {
            // Le TXT vide d'un service sans texte : un seul octet nul.
            rest = tail;
            continue;
        }
        if len > tail.len() {
            break;
        }
        if let Ok(text) = std::str::from_utf8(&tail[..len]) {
            out.push(text.to_string());
        }
        rest = &tail[len..];
    }
    out
}

/// Lit un TXT reçu (`clé=valeur`, dans n'importe quel ordre ; le reste est ignoré).
pub fn parse_txt(entries: &[String]) -> Txt {
    let mut out = Txt::default();
    for entry in entries {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            KEY_VERSION => out.protocol = value.trim().parse().ok(),
            KEY_FINGERPRINT => {
                let v = value.trim();
                // 32 octets en hexadécimal, séparés par « : » : 95 caractères.
                let ok = v.len() == 95
                    && v.split(':').count() == 32
                    && v.split(':')
                        .all(|b| b.len() == 2 && b.chars().all(|c| c.is_ascii_hexdigit()));
                out.fingerprint = ok.then(|| v.to_ascii_uppercase());
            }
            _ => {}
        }
    }
    out
}

/// Une adresse du réseau de la maison : privée (10/8, 172.16/12, 192.168/16). Ni la boucle locale,
/// ni le lien local, ni le tailnet (100.64/10) : ce sont d'autres chemins, annoncés à part.
pub fn is_home_lan(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

/// Le nom de l'instance, tiré du nom du Mac : « Mac de la maison ». Un point couperait le nom DNS
/// en deux, les caractères de contrôle sont retirés, la longueur bornée ; vide : « Ostal Server ».
pub fn instance_name(computer_name: &str) -> String {
    let cleaned: String = computer_name
        .chars()
        .map(|c| if c == '.' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let mut name: String = cleaned.trim().to_string();
    // Les 63 octets de l'étiquette, pas 63 caractères : on coupe sur une frontière de caractère.
    while name.len() > MAX_LABEL {
        name.pop();
    }
    let name = name.trim().to_string();
    if name.is_empty() {
        "Ostal Server".to_string()
    } else {
        name
    }
}

/// Le nom d'hôte annoncé (propre à Ostal, jamais celui du Mac, que Bonjour tient déjà) :
/// `ostal-` et les 8 premiers chiffres de l'empreinte, suivi de `.local`.
pub fn announced_host(fingerprint: &str) -> String {
    let id: String = fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if id.is_empty() {
        "ostal.local".to_string()
    } else {
        format!("ostal-{id}.local")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99";

    #[test]
    fn the_txt_round_trips_and_ignores_what_it_does_not_know() {
        let entries: Vec<String> = txt_records(4, FP)
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let txt = parse_txt(&entries);
        assert_eq!(txt.protocol, Some(4));
        assert_eq!(txt.fingerprint.as_deref(), Some(FP));
        let odd = parse_txt(&[
            "autre=1".into(),
            "pas de signe égal".into(),
            "v=pas un nombre".into(),
            "fp=trop court".into(),
        ]);
        assert_eq!(odd, Txt::default());
    }

    #[test]
    fn raw_txt_is_read_by_its_length_prefixes() {
        let mut raw = Vec::new();
        for entry in ["v=4", "fp=AB"] {
            raw.push(entry.len() as u8);
            raw.extend_from_slice(entry.as_bytes());
        }
        assert_eq!(txt_entries(&raw), ["v=4", "fp=AB"]);
        assert!(txt_entries(&[0]).is_empty());
        assert!(txt_entries(&[]).is_empty());
        // Longueur annoncée plus grande que ce qui reste : on s'arrête sans paniquer.
        assert_eq!(txt_entries(&[3, b'a', b'=', b'1', 9, b'x']), ["a=1"]);
    }

    #[test]
    fn only_home_network_addresses_are_announced() {
        let ok = |a: &str| is_home_lan(a.parse().unwrap());
        assert!(ok("192.168.1.20") && ok("10.0.0.5") && ok("172.16.4.1"));
        // Tailnet, boucle locale, lien local, adresse publique : jamais.
        assert!(
            !ok("100.101.102.103") && !ok("127.0.0.1") && !ok("169.254.3.4") && !ok("203.0.113.9")
        );
        assert!(!ok("172.32.0.1"));
    }

    #[test]
    fn the_instance_name_is_a_valid_dns_label() {
        assert_eq!(instance_name("Mac de la maison"), "Mac de la maison");
        assert_eq!(instance_name("Mac.mini.local"), "Mac mini local");
        assert_eq!(instance_name("  \u{7}  "), "Ostal Server");
        let long = "é".repeat(100);
        let name = instance_name(&long);
        assert!(name.len() <= 63 && name.chars().all(|c| c == 'é'), "{name}");
        assert_eq!(instance_name("Mac de Hamdi's"), "Mac de Hamdi's");
    }

    #[test]
    fn the_announced_host_is_ostals_own() {
        assert_eq!(announced_host(FP), "ostal-aabbccdd.local");
        assert_eq!(announced_host(""), "ostal.local");
    }
}
