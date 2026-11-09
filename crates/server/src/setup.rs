//! Les gardes de la mise en place d'un dossier de données : reprise d'un import interrompu,
//! identité Matter (jamais remplacée ni créée sans choix explicite), compte. Elles sont
//! partagées par la ligne de commande (`import`, `init`, `create-account`) et par
//! l'application de la barre de menus : les deux font exactement les mêmes vérifications.
//!
//! Hors [`prologue`], chaque fonction reçoit le dossier de données en argument.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::backup::{self, IdentityBrief, PendingKind};
use crate::security::account;
use crate::{matter, persistence};

/// Sortie après un import de sauvegarde par l'API (EX_TEMPFAIL) : non nul, pour qu'un
/// superviseur configuré pour ne relancer que les échecs relance aussi le serveur.
pub const EXIT_IMPORT_STAGED: u8 = 75;

/// Aucune identité Matter dans le dossier de données (EX_CONFIG).
pub const EXIT_NO_IDENTITY: u8 = 78;

/// Rappel affiché avant d'appliquer un import, et repris dans `docs/migration.md`.
pub const OTHER_INSTANCES_REMINDER: &str =
    "Rappel : arrêtez la v0.2.1 sur TOUS vos Mac, y compris \
celui-ci (elle ne prend pas le verrou d'instance : rien ne l'empêche de tourner en même temps). \
Deux instances avec la même identité Matter se chassent (abonnements, règles exécutées deux \
fois). Partez du Mac qui détient les données les plus récentes.";

/// Ce que toute commande qui touche au dossier de données fait d'abord, verrou d'instance pris :
/// reprendre un import interrompu entre ses deux renommages (le dossier de données est alors
/// absent, ou recréé sans identité) avant la migration de l'ancien dossier, qui pourrait
/// sinon s'installer à sa place, et avant tout chargement ou toute création de compte qui
/// créerait une identité. `apply` : applique aussi l'import mis en attente par l'API (le
/// serveur seulement ; la sous-commande `import` refuse un import en attente).
/// Rend le dossier de données à utiliser. Travaille sur le dossier de `persistence`.
pub fn prologue(apply: bool) -> Result<PathBuf> {
    let mut notes: Vec<String> = Vec::new();
    notes.extend(backup::recover(&persistence::current_data_dir())?);
    // Ancien nom « matter-demo » : son dossier de données est déplacé au premier lancement.
    notes.extend(persistence::migrate_legacy_data_dir());
    if apply {
        notes.extend(backup::apply_pending(&persistence::data_dir())?);
    }
    for m in &notes {
        log::warn!("{m}");
    }
    Ok(persistence::data_dir())
}

/// L'identité Matter d'un dossier de données.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityState {
    Present(IdentityBrief),
    Missing,
    /// `controller.json` existe mais n'est pas lisible : on n'y touche pas.
    Unreadable(String),
}

pub fn identity_state(data_dir: &Path) -> IdentityState {
    match backup::identity_in_dir(data_dir) {
        Ok(Some(identity)) => IdentityState::Present(identity),
        Ok(None) => IdentityState::Missing,
        Err(e) => IdentityState::Unreadable(format!("{e:#}")),
    }
}

/// Le compte unique existe-t-il ? (Lecture seule : ne crée aucun dossier.)
pub fn account_exists(data_dir: &Path) -> bool {
    data_dir
        .join(persistence::SERVER_DIR)
        .join(account::FILE_NAME)
        .is_file()
}

/// Un import est-il en attente, ou interrompu ?
pub fn import_in_progress(data_dir: &Path) -> bool {
    backup::pending_path(data_dir).exists() || backup::recovery_needed(data_dir)
}

/// Comparaison de l'identité d'une sauvegarde avec celle du dossier de données.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityComparison {
    pub incoming: IdentityBrief,
    pub current: IdentityState,
    /// Importer remplacerait une identité différente (ou illisible) : il faut un accord explicite.
    pub differs: bool,
}

impl IdentityComparison {
    pub fn from_parts(incoming: IdentityBrief, current: IdentityState) -> Self {
        let differs = match &current {
            IdentityState::Missing => false,
            IdentityState::Present(c) => c.fabric_id != incoming.fabric_id,
            IdentityState::Unreadable(_) => true,
        };
        Self {
            incoming,
            current,
            differs,
        }
    }
}

pub fn compare_identities(data_dir: &Path, file: &Path) -> Result<IdentityComparison> {
    let incoming = backup::identity_in_file(file)?;
    Ok(IdentityComparison::from_parts(
        incoming,
        identity_state(data_dir),
    ))
}

/// Une fabric différente (ou un `controller.json` illisible) n'est remplacée qu'avec un accord
/// explicite (`--replace-identity` en ligne de commande) : « oui » seul ne suffit jamais.
pub fn check_replace_allowed(
    comparison: &IdentityComparison,
    replace_identity: bool,
) -> Result<(), String> {
    if comparison.differs && !replace_identity {
        return Err(
            "Refusé : la fabric de la sauvegarde est différente de celle de ce dossier. Importer \
             remplacerait l'identité Matter : les appareils appairés avec l'identité actuelle ne \
             seraient plus reconnus (il faudrait les réappairer). Rien n'a été modifié. Si c'est \
             bien voulu (l'ancien dossier est conservé dans « …avant-import-… »), relancez avec \
             --replace-identity (--yes ne suffit pas)."
                .into(),
        );
    }
    Ok(())
}

/// Même garde, formulée pour la fenêtre de l'application (pas d'option de ligne de commande).
pub fn replace_refusal_for_app() -> &'static str {
    "La sauvegarde porte une autre identité Matter que celle de ce Mac. La reprendre remplacerait \
     l'identité actuelle : les appareils appairés avec elle ne seraient plus reconnus et il \
     faudrait les appairer de nouveau. Rien n'a été modifié."
}

/// Import demandé à la main : `Err` si une autre sauvegarde est déjà en attente.
pub fn pending_for_import(data_dir: &Path, file: &Path) -> Result<PendingKind> {
    backup::pending_kind(data_dir, file)
}

/// Pourquoi [`init_identity`] a refusé, ou échoué.
#[derive(Debug)]
pub enum InitError {
    /// Un import est en attente ou interrompu dans ce dossier.
    ImportPending(PathBuf),
    /// Une identité existe déjà.
    AlreadyExists(PathBuf),
    Other(anyhow::Error),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::ImportPending(dir) => write!(
                f,
                "Un import est en attente ou interrompu dans {} : « init » ne crée pas de nouvelle \
                 identité par-dessus. Relancez « matter-home-server import FICHIER » (ou démarrez le \
                 serveur) pour le terminer, ou renoncez-y avec « matter-home-server import \
                 --abandon ».",
                dir.display()
            ),
            InitError::AlreadyExists(dir) => write!(
                f,
                "Une identité Matter existe déjà dans {} : « init » ne fait rien. (Pour reprendre \
                 d'autres données, utilisez « import ».)",
                dir.display()
            ),
            InitError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for InitError {}

/// Crée l'identité Matter d'une installation neuve (une nouvelle fabric : aucun appareil n'est
/// encore appairé). Refuse si un import est en attente, si une identité existe, ou si d'autres
/// données existent sans identité. Rend le numéro de fabric.
pub fn init_identity(data_dir: &Path) -> Result<u64, InitError> {
    let state_path = data_dir.join(persistence::FILE_NAME);
    if import_in_progress(data_dir) {
        return Err(InitError::ImportPending(data_dir.to_path_buf()));
    }
    if state_path.exists() {
        return Err(InitError::AlreadyExists(data_dir.to_path_buf()));
    }
    persistence::check_fresh_start(data_dir)
        .and_then(|()| persistence::ensure_private_dir(data_dir))
        .and_then(|()| {
            let state = persistence::StoredState {
                version: 1,
                identity: matter::certs::generate_identity()?,
                nodes: Vec::new(),
            };
            persistence::save(&state_path, &state)?;
            Ok(state.identity.fabric_id)
        })
        .map_err(InitError::Other)
}

/// Vérifie un nom et un mot de passe avant de créer le compte (mêmes règles que `create-account`).
pub fn check_credentials(username: &str, password: &str) -> Result<(), String> {
    account::check_username(username).map_err(|e| format!("{e:#}"))?;
    if password.chars().count() < account::MIN_PASSWORD_LEN {
        return Err(format!(
            "Le mot de passe doit faire au moins {} caractères.",
            account::MIN_PASSWORD_LEN
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief(fabric: u64) -> IdentityBrief {
        IdentityBrief {
            fabric_id: fabric,
            devices: 0,
            nodes: vec![],
        }
    }

    #[test]
    fn same_fabric_is_not_a_replacement() {
        let c = IdentityComparison::from_parts(brief(1), IdentityState::Present(brief(1)));
        assert!(!c.differs);
        assert!(check_replace_allowed(&c, false).is_ok());
    }

    #[test]
    fn other_fabric_needs_explicit_consent() {
        let c = IdentityComparison::from_parts(brief(1), IdentityState::Present(brief(2)));
        assert!(c.differs);
        assert!(check_replace_allowed(&c, false).is_err());
        assert!(check_replace_allowed(&c, true).is_ok());
    }

    #[test]
    fn unreadable_identity_needs_explicit_consent() {
        let c = IdentityComparison::from_parts(brief(1), IdentityState::Unreadable("x".into()));
        assert!(c.differs);
        assert!(check_replace_allowed(&c, false).is_err());
    }

    #[test]
    fn missing_identity_replaces_nothing() {
        let c = IdentityComparison::from_parts(brief(1), IdentityState::Missing);
        assert!(!c.differs);
    }

    #[test]
    fn init_creates_once_then_refuses() {
        let dir = std::env::temp_dir().join(format!("mh-setup-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fabric = init_identity(&dir).unwrap();
        assert!(matches!(identity_state(&dir), IdentityState::Present(i) if i.fabric_id == fabric));
        assert!(matches!(
            init_identity(&dir),
            Err(InitError::AlreadyExists(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_refuses_when_other_data_exists_without_identity() {
        let dir = std::env::temp_dir().join(format!("mh-setup-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("regles.json"), "{}").unwrap();
        assert!(matches!(init_identity(&dir), Err(InitError::Other(_))));
        assert!(!dir.join(persistence::FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn credentials_rules() {
        assert!(check_credentials("hamdi", "123456789").is_err());
        assert!(check_credentials("hamdi", "1234567890").is_ok());
        assert!(check_credentials("", "1234567890").is_err());
    }
}
