//! La présence **déclarée** de chaque personne : « à la maison » ou « partie », telle que son
//! raccourci « Je rentre » / « Je pars » (ou, plus tard, son téléphone vu sur le Wi-Fi) l'a dite.
//!
//! Rangée dans `serveur/presence.json` (0600, jamais exportée) : la dernière déclaration de chacun,
//! avec la date du dernier changement. Pour l'instant, **rien n'en est déduit** : le mode de la
//! maison ne change pas tout seul (c'est l'étape « présence et mode automatique »). Le serveur
//! garde seulement ce que chacun a dit, pour que cette étape n'ait rien à migrer.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::security::write_secret;

pub const FILE: &str = "presence.json";

/// Où est la personne.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Whereabouts {
    Home,
    Away,
}

/// Comment on l'a su.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Un raccourci Apple (jeton de mode de la personne).
    Shortcut,
    /// La personne, depuis une session de l'app.
    Session,
}

/// La dernière déclaration d'une personne.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Declared {
    pub user_id: String,
    pub state: Whereabouts,
    /// Secondes Unix du dernier **changement** (redire la même chose ne le change pas).
    pub since: f64,
    pub source: Source,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    people: Vec<Declared>,
}

pub struct Presence {
    path: Option<PathBuf>,
    people: Mutex<Vec<Declared>>,
}

impl Presence {
    /// Ouvre `dir/presence.json` (absent : personne n'a rien déclaré). Un fichier illisible est mis
    /// de côté : on repart de zéro.
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let people = match crate::persistence::load_json::<File>(&path) {
            Ok(file) => file.map(|f| f.people).unwrap_or_default(),
            Err(e) => {
                let aside = path.with_extension("json.illisible");
                log::error!(
                    "présence illisible, personne n'a rien déclaré ({e:#}) ; fichier conservé sous {}",
                    aside.display()
                );
                let _ = std::fs::rename(&path, aside);
                Vec::new()
            }
        };
        Self {
            path: Some(path),
            people: Mutex::new(people),
        }
    }

    /// Sans fichier (démonstration, dossier inutilisable).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            people: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Declared>> {
        self.people.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Enregistre ce que la personne vient de dire. `true` : cela change son état (rien n'est
    /// écrit si elle redit la même chose).
    pub fn declare(&self, user_id: &str, state: Whereabouts, source: Source) -> Result<bool> {
        let mut people = self.lock();
        if people
            .iter()
            .any(|d| d.user_id == user_id && d.state == state)
        {
            return Ok(false);
        }
        let mut next = people.clone();
        next.retain(|d| d.user_id != user_id);
        next.push(Declared {
            user_id: user_id.to_string(),
            state,
            since: crate::security::now(),
            source,
        });
        if let Some(path) = &self.path {
            let file = File {
                version: 1,
                people: next.clone(),
            };
            write_secret(path, serde_json::to_string_pretty(&file)?.as_bytes())?;
        }
        *people = next;
        Ok(true)
    }

    /// Ce que cette personne a déclaré en dernier, s'il y a quelque chose.
    pub fn get(&self, user_id: &str) -> Option<Declared> {
        self.lock().iter().find(|d| d.user_id == user_id).cloned()
    }

    /// Les dernières déclarations de tout le monde.
    pub fn all(&self) -> Vec<Declared> {
        self.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_word_of_each_person_is_kept_and_a_repeat_changes_nothing() {
        let presence = Presence::in_memory();
        assert!(presence.get("lea").is_none());
        assert!(presence
            .declare("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap());
        let first = presence.get("lea").unwrap();
        assert!(!presence
            .declare("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap());
        assert_eq!(presence.get("lea").unwrap().since, first.since);
        assert!(presence
            .declare("lea", Whereabouts::Home, Source::Shortcut)
            .unwrap());
        assert!(presence
            .declare("adam", Whereabouts::Away, Source::Session)
            .unwrap());
        assert_eq!(presence.get("lea").unwrap().state, Whereabouts::Home);
        assert_eq!(presence.get("adam").unwrap().state, Whereabouts::Away);
        assert_eq!(presence.all().len(), 2);
    }

    #[test]
    fn declarations_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("ostal-presence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Presence::open(&dir)
            .declare("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap();
        let again = Presence::open(&dir);
        assert_eq!(again.get("lea").unwrap().state, Whereabouts::Away);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
