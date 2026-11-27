//! Le « connecteur de box » : ce que le cœur de la présence Wi-Fi demande à une box internet, quelle
//! que soit sa marque. La Bbox (`extensions/bbox.rs`) est le premier connecteur ; une SFR Box ou une
//! Freebox s'ajouteront en implémentant [`BoxConnector`] dans leur extension (et en s'inscrivant
//! dans `Extensions::box_connectors`), sans toucher au cœur (`wifi/mod.rs`, `hub/wifi.rs`).
//!
//! Le cœur ne connaît que des **noms** d'appareils (« iPhone-de-Sam ») et leur état : jamais
//! d'adresse MAC ni d'IP, même si une box les annonce.

/// Un appareil vu par la box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// Le nom DHCP (« iPhone-de-Sam »), tel que la box l'écrit.
    pub name: String,
    /// Connecté en ce moment.
    pub active: bool,
}

/// Pourquoi la box n'a pas répondu comme il faut. Dans tous les cas, **rien n'est conclu** : le
/// mode de la maison reste figé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxError {
    /// Pas de réponse, réponse illisible ou erreur de la box.
    Unreachable,
    /// La box a refusé le mot de passe.
    Refused,
}

/// Une box internet reliée à Ostal, pour la présence Wi-Fi. Fournie par une extension.
pub trait BoxConnector: Send + Sync {
    /// L'identifiant de l'extension qui la porte (« bbox »).
    fn id(&self) -> &'static str;
    /// Le nom de la marque, pour les phrases (« Bbox »).
    fn name(&self) -> &'static str;
    /// L'extension est activée et complète (adresse, mot de passe…) : un relevé peut partir.
    fn ready(&self) -> bool;
    /// Change à chaque modification des réglages de l'extension (interrupteur, adresse, mot de
    /// passe) : le cœur oublie alors ce qu'il avait relevé de l'ancienne box.
    fn generation(&self) -> u64;
    /// La liste des appareils connus de la box. Bloquant (quelques secondes au plus). Un mot de passe
    /// refusé n'est **pas rejoué** tant que les réglages ne changent pas (la box pourrait bloquer
    /// l'accès) : l'appel rend alors `Refused` sans rien envoyer.
    fn hosts(&self) -> Result<Vec<Host>, BoxError>;
}

/// Un nom d'appareil propre : sans caractères de contrôle, 64 caractères au plus.
pub fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(64)
        .collect()
}

/// Ce nom ressemble-t-il à un téléphone (iPhone ou Android) ? La box ne dit pas le type d'un
/// appareil : on s'en tient au nom. Un membre de la maison ne voit que ces noms-là ; l'administrateur
/// peut attribuer n'importe quel appareil vu.
pub fn looks_like_phone(name: &str) -> bool {
    const WORDS: &[&str] = &[
        "iphone",
        "android",
        "galaxy",
        "pixel",
        "redmi",
        "xiaomi",
        "poco",
        "oneplus",
        "oppo",
        "realme",
        "huawei",
        "honor",
        "samsung",
        "motorola",
        "moto-",
        "fairphone",
        "xperia",
        "nokia",
        "vivo",
        "phone",
        "telephone",
        "tel-",
        "smartphone",
    ];
    let lower = name.to_lowercase();
    WORDS.iter().any(|w| lower.contains(w)) || lower.starts_with("sm-")
}

/// Un faux connecteur pour les essais du cœur (aucun réseau, aucune extension).
#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    type Answer = Option<Result<Vec<Host>, BoxError>>;

    #[derive(Clone)]
    pub struct Fake {
        answer: Arc<Mutex<Answer>>,
        ready: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
        calls: Arc<AtomicUsize>,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                answer: Arc::default(),
                ready: Arc::new(AtomicBool::new(true)),
                generation: Arc::new(AtomicU64::new(1)),
                calls: Arc::default(),
            }
        }
    }

    impl Fake {
        pub fn set(&self, hosts: &[(&str, bool)]) {
            *self.answer.lock().unwrap() = Some(Ok(hosts
                .iter()
                .map(|(n, a)| Host {
                    name: n.to_string(),
                    active: *a,
                })
                .collect()));
        }

        pub fn fail(&self, error: BoxError) {
            *self.answer.lock().unwrap() = Some(Err(error));
        }

        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        /// L'extension est activée et complète, ou non (les réglages changent : nouvelle génération).
        pub fn set_ready(&self, ready: bool) {
            self.ready.store(ready, Ordering::SeqCst);
            self.bump();
        }

        /// Les réglages changent (autre mot de passe, par exemple).
        pub fn bump(&self) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl BoxConnector for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn name(&self) -> &'static str {
            "Fausse box"
        }
        fn ready(&self) -> bool {
            self.ready.load(Ordering::SeqCst)
        }
        fn generation(&self) -> u64 {
            self.generation.load(Ordering::SeqCst)
        }
        fn hosts(&self) -> Result<Vec<Host>, BoxError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(Err(BoxError::Unreachable))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_are_recognised_by_name() {
        for yes in [
            "iPhone-de-Sam",
            "Galaxy-de-Lea",
            "Redmi-de-Adam",
            "android-3f2a",
            "SM-G991B",
            "Pixel-7",
        ] {
            assert!(looks_like_phone(yes), "{yes}");
        }
        for no in ["MacBook-Pro", "LG-webOS-TV", "ESP_3F2A10", "Bbox-4K", "PS5"] {
            assert!(!looks_like_phone(no), "{no}");
        }
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Mac\u{7}mini \n"), "Macmini");
        assert_eq!(clean_name(&"a".repeat(100)).len(), 64);
    }
}
