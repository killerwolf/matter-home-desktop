//! Réglages › Relais de confiance : la saisie et l'enregistrement des proxys de confiance
//! (`serveur/reseau.json`). Même analyse et même écriture que `ostal-server network
//! trusted-proxies` (`security::network`), même ligne au journal d'audit ; ici seulement la
//! lecture de ce qui est tapé et les messages.

use std::path::Path;

use anyhow::Result;
use ostal_server::audit::Who;
use ostal_server::security::network::{self, TrustedProxies};

/// Ce qui est tapé : des réseaux séparés par des espaces, des virgules, des points-virgules ou
/// des retours à la ligne. Vide : aucun relais. Chaque réseau passe par le parseur de la ligne de
/// commande (`TrustedProxies::parse_list`) ; un refus dit lequel.
pub fn parse_input(text: &str) -> Result<Vec<String>> {
    let items: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|s| !s.is_empty())
        .collect();
    Ok(TrustedProxies::parse_list(&items)?.list())
}

/// La liste réglée sur le serveur, telle que le serveur la lit.
pub fn current(server_dir: &Path) -> Vec<String> {
    network::read(server_dir).trusted.list()
}

/// Enregistre ce qui est tapé (vide : aucun relais). Rend la liste retenue.
pub fn save(server_dir: &Path, text: &str) -> Result<Vec<String>> {
    let list = parse_input(text)?;
    network::set_trusted_proxies(server_dir, Some(&list), &Who::server_app())
}

/// Le texte de confirmation montré sous la case.
pub fn saved_message(list: &[String]) -> String {
    if list.is_empty() {
        "Aucun relais de confiance : l'adresse réelle des appareils n'est pas lue.".to_string()
    } else {
        format!("Relais de confiance enregistrés : {}", list.join(", "))
    }
}

/// Ce qui remplit la case pour une liste réglée.
pub fn to_input(list: &[String]) -> String {
    list.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ostal_server::audit::{Action, AuditLog, Filter};

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ostal-relays-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn input_accepts_several_separators_and_bare_addresses() {
        assert!(parse_input("").unwrap().is_empty());
        assert!(parse_input("  \n ").unwrap().is_empty());
        let list = parse_input("10.0.0.5, 192.168.1.0/24;::1\n10.0.0.5").unwrap();
        assert_eq!(
            list,
            vec!["10.0.0.5/32", "192.168.1.0/24", "::1/128", "10.0.0.5/32"]
        );
    }

    #[test]
    fn input_refusals_name_the_culprit() {
        let e = parse_input("10.0.0.5 maison").unwrap_err().to_string();
        assert!(e.contains("maison") && e.contains("pas un réseau"), "{e}");
        let e = parse_input("10.0.0.0/33").unwrap_err().to_string();
        assert!(e.contains("préfixe"), "{e}");
    }

    #[test]
    fn save_writes_the_same_file_as_the_command_line_and_audits_without_addresses() {
        let dir = temp("save");
        assert!(current(&dir).is_empty());
        let list = save(&dir, "10.0.0.5").unwrap();
        assert_eq!(list, vec!["10.0.0.5/32"]);
        assert_eq!(current(&dir), vec!["10.0.0.5/32"]);
        // Le même réglage une seconde fois ne change rien : pas de nouvelle ligne.
        save(&dir, "10.0.0.5").unwrap();
        let entries = AuditLog::reader(&dir).read(&Filter::default());
        assert_eq!(entries.len(), 1, "{entries:?}");
        let e = &entries[0];
        assert_eq!(e.action, Action::NetworkSettings);
        assert_eq!(e.detail.as_deref(), Some("trusted:1"));
        assert!(!serde_json::to_string(e).unwrap().contains("10.0.0.5"));
        // « Aucun relais ».
        assert!(save(&dir, "").unwrap().is_empty());
        assert!(current(&dir).is_empty());
        assert_eq!(AuditLog::reader(&dir).read(&Filter::default()).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_input_changes_nothing() {
        let dir = temp("refused");
        save(&dir, "10.0.0.5").unwrap();
        assert!(save(&dir, "10.0.0.5, nimporte quoi").is_err());
        assert_eq!(current(&dir), vec!["10.0.0.5/32"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn messages_and_input_round_trip() {
        assert!(saved_message(&[]).starts_with("Aucun relais"));
        let l = vec!["10.0.0.5/32".to_string(), "::1/128".to_string()];
        assert!(saved_message(&l).contains("10.0.0.5/32, ::1/128"));
        assert_eq!(parse_input(&to_input(&l)).unwrap(), l);
    }
}
