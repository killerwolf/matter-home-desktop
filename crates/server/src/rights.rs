//! Les droits d'un compte : la fondation, sans les activer.
//!
//! Chaque route qui modifie la maison demande `can(personne, capacité)` avant d'agir. En V1,
//! tous les comptes ont les mêmes droits : la réponse est toujours oui. Le jour où l'on voudra
//! restreindre un compte (un invité qui ne modifie pas les automatisations), il suffira de changer
//! cette fonction : ni migration de fichier, ni changement de protocole (le code d'erreur
//! `forbidden` existe déjà, voir `ErrorCode::Forbidden`).
//!
//! Les routes de lecture ne passent pas par ici, ni ce qui ne concerne que la personne elle-même
//! (ses connexions, ses jetons, ses abonnements aux notifications).

use std::sync::atomic::{AtomicU16, Ordering};

/// Ce qu'un compte peut faire dans la maison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Voir la maison (état, plan, caméras).
    View,
    /// Commander les équipements (allumer, éteindre, volets) et gérer leur historique et les
    /// événements des caméras.
    ControlDevices,
    /// Changer le mode de la maison (et déclarer sa présence, qui peut le changer).
    ChangeMode,
    /// Modifier le plan et la personnalisation des équipements.
    EditPlan,
    /// Modifier les automatisations (et le réglage commun des notifications).
    EditAutomations,
    /// Appairer ou retirer un équipement.
    Pair,
    /// Exporter ou importer une sauvegarde.
    ExportImport,
    /// Gérer les comptes (créer, désactiver, changer un mot de passe).
    ManageAccounts,
    /// Lire le journal d'audit.
    ReadJournal,
    /// Activer, désactiver et régler les extensions (météo, Somfy…).
    ManageExtensions,
}

impl Capability {
    /// Toutes les capacités.
    pub const ALL: [Capability; 10] = [
        Capability::View,
        Capability::ControlDevices,
        Capability::ChangeMode,
        Capability::EditPlan,
        Capability::EditAutomations,
        Capability::Pair,
        Capability::ExportImport,
        Capability::ManageAccounts,
        Capability::ReadJournal,
        Capability::ManageExtensions,
    ];

    fn bit(self) -> u16 {
        1 << Capability::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }
}

/// Les droits de la maison. Vide en V1 : `can` répond oui à tout. Les tests peuvent retirer une
/// capacité à tout le monde (`deny`) pour prouver qu'une route la demande ; rien en production
/// ne s'en sert.
#[derive(Debug, Default)]
pub struct Rights {
    denied: AtomicU16,
}

impl Rights {
    pub fn new() -> Rights {
        Rights::default()
    }

    /// `true` si la personne (son identifiant de compte) peut faire cela. V1 : toujours oui.
    pub fn can(&self, _user_id: &str, capability: Capability) -> bool {
        self.denied.load(Ordering::Relaxed) & capability.bit() == 0
    }

    /// Pour les tests : refuse cette capacité à tout le monde.
    #[doc(hidden)]
    pub fn deny(&self, capability: Capability) {
        self.denied.fetch_or(capability.bit(), Ordering::Relaxed);
    }

    /// Pour les tests : refuse tout.
    #[doc(hidden)]
    pub fn deny_all(&self) {
        for capability in Capability::ALL {
            self.deny(capability);
        }
    }

    /// Pour les tests : rend tous les droits.
    #[doc(hidden)]
    pub fn allow_all(&self) {
        self.denied.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_says_yes_to_everything() {
        let rights = Rights::new();
        for capability in Capability::ALL {
            assert!(rights.can("n'importe qui", capability));
        }
    }

    #[test]
    fn each_capability_has_its_own_bit() {
        let rights = Rights::new();
        rights.deny(Capability::EditPlan);
        for capability in Capability::ALL {
            assert_eq!(
                rights.can("x", capability),
                capability != Capability::EditPlan
            );
        }
        rights.allow_all();
        assert!(rights.can("x", Capability::EditPlan));
    }
}
