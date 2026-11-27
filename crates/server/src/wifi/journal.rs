//! La mémoire du journal de la présence par le Wi-Fi : de quoi n'écrire une ligne que quand quelque
//! chose **change** (un relevé toutes les 2 minutes ne doit pas inonder `serveur.log`).
//!
//! Rien ici n'écrit : chaque méthode rend la ligne à journaliser, ou `None` quand c'est déjà dit.
//! Jamais d'adresse MAC, d'IP ni de secret : seulement des noms de comptes et des nombres.

use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct Journal {
    settings: Option<String>,
    /// `Some(None)` : les conditions sont réunies ; `Some(Some(raison))` : la règle est ignorée.
    condition: Option<Option<String>>,
    /// Dernier état connu de chaque téléphone (clé : compte) : vu ou non.
    seen: HashMap<String, bool>,
    /// Comptes dont le téléphone n'est dans aucun relevé alors que la box répond.
    unlisted: HashSet<String>,
}

impl Journal {
    /// L'état de la fonction : une ligne au premier relevé, puis à chaque changement de réglage.
    pub fn settings(&mut self, auto_mode: bool, linked: bool, phones: usize) -> Option<String> {
        let plural = if phones > 1 { "s" } else { "" };
        let line = format!(
            "présence Wi-Fi : mode automatique {}, box {}, {phones} téléphone{plural} associé{plural} à un compte actif",
            if auto_mode { "allumé" } else { "éteint" },
            if linked { "reliée" } else { "non reliée" },
        );
        (self.settings.as_deref() != Some(&line)).then(|| {
            self.settings = Some(line.clone());
            line
        })
    }

    /// Pourquoi la règle de mode est ignorée (`Some(raison)`), ou `None` : rien ne l'empêche.
    /// Une ligne seulement quand la raison change.
    pub fn condition(&mut self, reason: Option<&str>) -> Option<String> {
        let new = reason.map(str::to_string);
        if self.condition.as_ref() == Some(&new) {
            return None;
        }
        let line = match &new {
            Some(r) => format!("présence Wi-Fi : règle de mode ignorée ({r})"),
            None => "présence Wi-Fi : conditions réunies, la règle de mode s'applique".to_string(),
        };
        self.condition = Some(new);
        Some(line)
    }

    /// Le téléphone de ce compte vient-il de changer d'état (vu / plus vu) ? Le premier relevé
    /// compte comme un changement.
    pub fn phone_seen(&mut self, user_id: &str, seen: bool) -> bool {
        self.seen.insert(user_id.to_string(), seen) != Some(seen)
    }

    /// Les comptes dont le téléphone vient de disparaître des relevés (pas ceux déjà signalés).
    /// Un compte qui réapparaît pourra être signalé de nouveau.
    pub fn newly_unlisted(&mut self, now_unlisted: &[String]) -> Vec<String> {
        let new = now_unlisted
            .iter()
            .filter(|u| !self.unlisted.contains(*u))
            .cloned()
            .collect();
        self.unlisted = now_unlisted.iter().cloned().collect();
        new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_once_until_they_change() {
        let mut j = Journal::default();
        let first = j.settings(false, true, 2).unwrap();
        assert!(
            first.contains("éteint") && first.contains("reliée") && first.contains("2 téléphones")
        );
        assert!(j.settings(false, true, 2).is_none());
        assert!(j.settings(false, true, 2).is_none());
        let on = j.settings(true, true, 2).unwrap();
        assert!(on.contains("allumé"));
        assert!(j.settings(true, true, 2).is_none());
    }

    #[test]
    fn condition_once_until_reason_changes() {
        let mut j = Journal::default();
        assert!(j.condition(Some("interrupteur éteint")).is_some());
        for _ in 0..5 {
            assert!(j.condition(Some("interrupteur éteint")).is_none());
        }
        let l = j.condition(Some("pas de box reliée")).unwrap();
        assert!(l.contains("pas de box reliée"));
        assert!(j.condition(Some("pas de box reliée")).is_none());
        assert!(j.condition(None).unwrap().contains("s'applique"));
        assert!(j.condition(None).is_none());
        assert!(j.condition(Some("pas de box reliée")).is_some());
    }

    #[test]
    fn phone_state_changes_only() {
        let mut j = Journal::default();
        assert!(j.phone_seen("a", true));
        assert!(!j.phone_seen("a", true));
        assert!(j.phone_seen("b", true));
        assert!(j.phone_seen("a", false));
        assert!(!j.phone_seen("a", false));
        assert!(j.phone_seen("a", true));
    }

    #[test]
    fn unlisted_reported_once_per_absence() {
        let mut j = Journal::default();
        let a = vec!["a".to_string()];
        assert_eq!(j.newly_unlisted(&a), a);
        assert!(j.newly_unlisted(&a).is_empty());
        let ab = vec!["a".to_string(), "b".to_string()];
        assert_eq!(j.newly_unlisted(&ab), vec!["b".to_string()]);
        assert!(j.newly_unlisted(&[]).is_empty());
        assert_eq!(j.newly_unlisted(&a), a);
    }
}
