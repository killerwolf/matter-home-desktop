//! Les anciens noms du produit (« Matter Home », renommé **Ostal**) et la migration de leurs
//! données. C'est, avec `crates/client/src/net/store.rs` et l'app web (`api.js`, `temps.js`),
//! le seul endroit du code où ces noms restent : on les lit encore, on ne les écrit plus.
//!
//! Migration du dossier de données, au premier lancement d'Ostal (voir [`migrate_dir`]) :
//! l'ancien dossier est **copié** (jamais déplacé) vers le nouveau, la copie est vérifiée
//! fichier par fichier, puis mise en place d'un seul renommage. L'ancien reste en place, comme
//! sauvegarde, à un détail près : son `controller.json` est renommé en
//! [`NEUTRALIZED_IDENTITY`]. L'ancienne application rouverte y trouve alors « identité absente »
//! et refuse de créer une nouvelle fabric (plan et historique présents) au lieu de repartir
//! en silence avec la même fabric qu'Ostal. Pour revenir en arrière, il suffit de le renommer.
//! Pendant toute la vie du processus, le serveur tient aussi l'ancien verrou d'instance
//! (`persistence::acquire_data_lock`) : l'ancienne application ne peut pas démarrer à côté.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::persistence::FILE_NAME;

/// Dossier de données de « Matter Home » (v0.2.1 à v1.4) :
/// `~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop`.
pub const DATA_DIR: (&str, &str) = ("matter-home-desktop", "MatterHomeDesktop");
/// Dossier de données des toutes premières versions, nommées « matter-demo ».
pub const DEMO_DATA_DIR: (&str, &str) = ("matter-demo", "MatterDemo");
/// Extension des sauvegardes de « Matter Home » (toujours acceptée à l'import).
pub const BACKUP_EXTENSION: &str = "matterhome";
/// Marqueur de format des sauvegardes de « Matter Home » (toujours accepté à l'import).
pub const BACKUP_FORMAT: &str = "matter-home-desktop-sauvegarde";
/// Nom de l'import mis en attente par « Matter Home », dans le dossier de données.
pub const PENDING_IMPORT: &str = "import-en-attente.matterhome";
/// Sous-protocole WebSocket de « Matter Home » : un onglet ou un client encore à l'ancienne
/// version continue de s'authentifier.
pub const WS_SUBPROTOCOL: &str = "matter-home.v1";
/// Préfixe des dossiers temporaires des raccourcis signés, avant le renommage.
pub const SHORTCUT_TEMP_PREFIX: &str = "matter-home-raccourci-";

/// LaunchAgent de « Matter Home Server » (`~/Library/LaunchAgents/<label>.plist`).
pub const LAUNCH_AGENT_LABEL: &str = "dev.matter-home-desktop.server-app";
/// Service installé au Terminal par « Matter Home » (ancienne méthode).
pub const LAUNCH_DAEMON_PLIST: &str = "/Library/LaunchDaemons/dev.matter-home-desktop.server.plist";
/// Petits réglages de « Matter Home Server » (`~/Library/Application Support/<nom>`).
pub const MARKER_DIR: &str = "Matter Home Server";
/// Exécutables de « Matter Home » (pour reconnaître qui tient l'ancien verrou).
pub const APP_BINARY: &str = "matter-home-server-app";
pub const CLI_BINARY: &str = "matter-home-server";
/// Exécutable de l'application de bureau de « Matter Home », v0.2 comprise (elle ne prend
/// aucun verrou : on la reconnaît à son nom de processus).
pub const DESKTOP_BINARY: &str = "matter-home-desktop";

/// Suffixe de la copie en cours, à côté du nouveau dossier. Distinct de ceux de l'import
/// (`.import-partiel`, `.import-nouveau`, `.avant-import-…`) : une reprise d'import n'y touche pas.
pub const STAGING_SUFFIX: &str = ".migration-en-cours";

/// Nom que prend `controller.json` dans l'ancien dossier une fois la copie faite et vérifiée.
pub const NEUTRALIZED_IDENTITY: &str = "controller.json.copie-vers-ostal";

/// Marge gardée en plus de la taille de l'ancien dossier, pour la vérification d'espace libre.
const SPACE_MARGIN: u64 = 16 * 1024 * 1024;

/// L'identité de cet ancien dossier a déjà été copiée vers Ostal (puis neutralisée ici).
pub fn identity_copied(old: &Path) -> bool {
    old.join(NEUTRALIZED_IDENTITY).is_file()
}

fn sibling(dir: &Path, suffix: &str) -> Result<PathBuf> {
    let name = dir.file_name().context("dossier de données sans nom")?;
    Ok(dir.with_file_name(format!("{}{suffix}", name.to_string_lossy())))
}

fn sync_dir(dir: &Path) {
    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
}

/// Ce que [`migrate_dir`] doit faire, sans rien toucher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Rien : l'ancien dossier n'a pas d'identité (ou elle a déjà été copiée et neutralisée),
    /// ou le nouveau en a déjà une.
    Nothing,
    /// Copier l'ancien vers le nouveau (absent ou vide).
    Copy,
    /// Le nouveau existe sans identité mais n'est pas vide (un compte créé avant la migration,
    /// par exemple) : il est mis de côté, jamais supprimé, puis l'ancien est copié.
    SetAsideThenCopy,
}

pub fn plan(old: &Path, new: &Path) -> Plan {
    if !old.join(FILE_NAME).is_file() || new.join(FILE_NAME).exists() {
        return Plan::Nothing;
    }
    match std::fs::read_dir(new).map(|mut entries| entries.next().is_none()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Plan::Copy,
        Ok(true) => Plan::Copy,
        _ => Plan::SetAsideThenCopy,
    }
}

/// Copie le dossier de données `old` (ancien nom) vers `new`, si `old` porte une identité
/// Matter et `new` non. `Ok(Some(message))` si la copie a eu lieu. L'appelant tient les deux
/// verrous d'instance : personne n'écrit dans `old` pendant la copie.
///
/// 1. Un import interrompu dans `old` (voir `backup::recovery_needed`) : refus. Le terminer
///    ici modifierait l'ancien dossier ; l'ancienne application le reprend à son lancement.
/// 2. Une copie précédente inachevée (`new` + [`STAGING_SUFFIX`]) est supprimée : `old` est
///    intact, on recommence.
/// 3. Espace libre : au moins la taille de `old` (plus une marge) sur le disque de `new`.
/// 4. Copie (dossiers, fichiers avec leurs droits et dates, liens symboliques tels quels ;
///    `old` lui-même peut être un lien vers un dossier), vérification (même arborescence,
///    mêmes octets), puis un seul renommage vers `new`. En cas d'échec, `new` n'existe pas et
///    `old` n'a pas bougé.
/// 5. Seulement alors, `old/controller.json` est renommé en [`NEUTRALIZED_IDENTITY`].
pub fn migrate_dir(old: &Path, new: &Path) -> Result<Option<String>> {
    if crate::backup::recovery_needed(old) {
        bail!(
            "un import de sauvegarde interrompu attend d'être terminé dans l'ancien dossier {} : \
             rien n'est copié. Ouvrez une fois l'ancienne application (Matter Home Server) pour \
             qu'elle le termine, quittez-la, puis rouvrez Ostal Server.",
            old.display()
        );
    }
    let what = plan(old, new);
    if what == Plan::Nothing {
        return Ok(None);
    }
    // Un ancien dossier qui est un lien symbolique : on copie le dossier qu'il désigne.
    let source = old
        .canonicalize()
        .with_context(|| format!("lecture de {}", old.display()))?;
    let parent = new.parent().context("dossier de données sans parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("création de {}", parent.display()))?;
    check_space(&source, parent)?;
    let mut aside_note = String::new();
    match what {
        Plan::SetAsideThenCopy => {
            let aside = sibling(
                new,
                &format!(
                    ".sans-identite-{}",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
            )?;
            std::fs::rename(new, &aside).with_context(|| {
                format!(
                    "impossible de mettre de côté {} (sans identité) avant la copie de {}",
                    new.display(),
                    old.display()
                )
            })?;
            aside_note = format!(
                " Le dossier {} qui existait déjà, sans identité, est conservé dans {}.",
                new.display(),
                aside.display()
            );
        }
        _ => {
            // Vide : rien à garder.
            let _ = std::fs::remove_dir(new);
        }
    }
    let staging = sibling(new, STAGING_SUFFIX)?;
    remove_tree(&staging)
        .with_context(|| format!("nettoyage de la copie inachevée {}", staging.display()))?;
    let result = copy_tree(&source, &staging)
        .and_then(|files| verify_tree(&source, &staging).map(|()| files))
        .and_then(|files| {
            rename_legacy_pending(&staging)?;
            #[cfg(unix)]
            std::fs::set_permissions(
                &staging,
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )
            .with_context(|| format!("droits de {}", staging.display()))?;
            sync_dir(&staging);
            std::fs::rename(&staging, new).with_context(|| {
                format!(
                    "mise en place de {} sous le nom {}",
                    staging.display(),
                    new.display()
                )
            })?;
            sync_dir(parent);
            Ok(files)
        });
    match result {
        Ok(files) => {
            let neutralized = match neutralize(old) {
                Ok(name) => format!(
                    " Son identité Matter y est renommée en {name} : l'ancienne application ne \
                     peut plus démarrer sur ces données (renommez ce fichier en {FILE_NAME} pour \
                     revenir en arrière)."
                ),
                Err(e) => format!(
                    " Attention : l'identité de l'ancien dossier n'a pas pu être neutralisée \
                     ({e:#}) ; ne rouvrez plus l'ancienne application."
                ),
            };
            Ok(Some(format!(
                "Données de Matter Home copiées de {} vers {} ({files} fichiers, vérifiés). \
                 L'ancien dossier est gardé comme sauvegarde.{neutralized}{aside_note}",
                old.display(),
                new.display()
            )))
        }
        Err(e) => {
            let _ = remove_tree(&staging);
            Err(e.context(format!(
                "copie des données de {} vers {} impossible (l'ancien dossier n'a pas été \
                 modifié).{aside_note}",
                old.display(),
                new.display()
            )))
        }
    }
}

/// Renomme `old/controller.json` en [`NEUTRALIZED_IDENTITY`] (ou, si ce nom est déjà pris, en
/// une variante datée : rien n'est écrasé). Rend le nom choisi.
fn neutralize(old: &Path) -> Result<String> {
    let mut name = NEUTRALIZED_IDENTITY.to_string();
    if old.join(&name).exists() {
        name = format!(
            "{NEUTRALIZED_IDENTITY}-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
    }
    let (from, to) = (old.join(FILE_NAME), old.join(&name));
    std::fs::rename(&from, &to).with_context(|| format!("renommage de {}", from.display()))?;
    sync_dir(old);
    Ok(name)
}

/// Taille cumulée des fichiers de `dir` (les liens comptent pour rien).
fn tree_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).with_context(|| format!("lecture de {}", dir.display()))? {
        let entry = entry.with_context(|| format!("lecture de {}", dir.display()))?;
        let meta = std::fs::symlink_metadata(entry.path())
            .with_context(|| format!("lecture de {}", entry.path().display()))?;
        if meta.is_dir() {
            total += tree_size(&entry.path())?;
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

/// Refuse la copie si le disque de `dest` n'a pas la place de `source` (plus une marge).
fn check_space(source: &Path, dest: &Path) -> Result<()> {
    let needed = tree_size(source)?;
    if let Some((free, _)) = crate::detection::store::disk_space(dest) {
        space_ok(needed, free).map_err(|m| anyhow::anyhow!("{m} ({})", dest.display()))?;
    }
    Ok(())
}

fn space_ok(needed: u64, free: u64) -> std::result::Result<(), String> {
    if free >= needed.saturating_add(SPACE_MARGIN) {
        return Ok(());
    }
    let mo = |b: u64| b.div_ceil(1024 * 1024);
    Err(format!(
        "pas assez d'espace libre pour copier les données de Matter Home : il faut environ \
         {} Mo, il en reste {} Mo. Libérez de la place sur ce disque, puis relancez Ostal Server",
        mo(needed.saturating_add(SPACE_MARGIN)),
        mo(free)
    ))
}

fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) => Err(e),
    }
}

/// L'import mis en attente par l'ancienne version porte l'ancien nom : il prend le nouveau,
/// pour que `backup::apply_pending` le trouve.
fn rename_legacy_pending(dir: &Path) -> Result<()> {
    let old = dir.join(PENDING_IMPORT);
    if old.is_file() {
        let new = crate::backup::pending_path(dir);
        std::fs::rename(&old, &new).with_context(|| format!("renommage de {}", old.display()))?;
    }
    Ok(())
}

/// Copie récursive de `from` vers `to` (qui ne doit pas exister). Rend le nombre de fichiers.
fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    let meta = std::fs::symlink_metadata(from)
        .with_context(|| format!("lecture de {}", from.display()))?;
    if !meta.is_dir() {
        bail!("{} n'est pas un dossier", from.display());
    }
    std::fs::create_dir(to).with_context(|| format!("création de {}", to.display()))?;
    let mut files = 0;
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .with_context(|| format!("lecture de {}", from.display()))?
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("lecture de {}", from.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = std::fs::symlink_metadata(&src)
            .with_context(|| format!("lecture de {}", src.display()))?;
        let ft = kind.file_type();
        if ft.is_dir() {
            files += copy_tree(&src, &dst)?;
        } else if ft.is_file() {
            copy_file(&src, &dst, &kind)?;
            files += 1;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&src)
                .with_context(|| format!("lecture de {}", src.display()))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dst)
                .with_context(|| format!("création de {}", dst.display()))?;
        } else {
            // Socket, tube nommé… : rien de tel dans un dossier de données ; ne pas deviner.
            bail!("{} n'est ni un fichier ni un dossier", src.display());
        }
    }
    std::fs::set_permissions(to, meta.permissions())
        .with_context(|| format!("droits de {}", to.display()))?;
    sync_dir(to);
    Ok(files)
}

fn copy_file(src: &Path, dst: &Path, meta: &std::fs::Metadata) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    // Créé d'emblée en 0600 : il peut contenir des clés privées ; les droits d'origine sont
    // remis après la copie.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options
        .open(dst)
        .with_context(|| format!("création de {}", dst.display()))?;
    let mut input =
        std::fs::File::open(src).with_context(|| format!("lecture de {}", src.display()))?;
    std::io::copy(&mut input, &mut out).with_context(|| format!("copie de {}", src.display()))?;
    out.set_permissions(meta.permissions())
        .with_context(|| format!("droits de {}", dst.display()))?;
    if let Ok(modified) = meta.modified() {
        let _ = out.set_modified(modified);
    }
    out.sync_all()
        .with_context(|| format!("écriture de {}", dst.display()))?;
    Ok(())
}

/// Les deux arborescences sont identiques : mêmes noms, mêmes types, mêmes octets, mêmes
/// cibles de liens.
fn verify_tree(a: &Path, b: &Path) -> Result<()> {
    let names = |dir: &Path| -> Result<Vec<std::ffi::OsString>> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("lecture de {}", dir.display()))?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<_>>()?;
        v.sort();
        Ok(v)
    };
    let (left, right) = (names(a)?, names(b)?);
    if left != right {
        bail!(
            "vérification : {} et {} n'ont pas les mêmes fichiers",
            a.display(),
            b.display()
        );
    }
    for name in left {
        let (x, y) = (a.join(&name), b.join(&name));
        let (mx, my) = (
            std::fs::symlink_metadata(&x)?,
            std::fs::symlink_metadata(&y)?,
        );
        if mx.file_type() != my.file_type() {
            bail!("vérification : {} a changé de nature", y.display());
        }
        if mx.is_dir() {
            verify_tree(&x, &y)?;
        } else if mx.file_type().is_symlink() {
            if std::fs::read_link(&x)? != std::fs::read_link(&y)? {
                bail!("vérification : le lien {} diffère", y.display());
            }
        } else if mx.len() != my.len() || !same_bytes(&x, &y)? {
            bail!("vérification : {} diffère de l'original", y.display());
        }
    }
    Ok(())
}

fn same_bytes(x: &Path, y: &Path) -> Result<bool> {
    use std::io::Read;
    let mut fx = std::fs::File::open(x).with_context(|| format!("lecture de {}", x.display()))?;
    let mut fy = std::fs::File::open(y).with_context(|| format!("lecture de {}", y.display()))?;
    let (mut bx, mut by) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = read_full(&mut fx, &mut bx)?;
        let m = read_full(&mut fy, &mut by)?;
        if n != m || bx[..n] != by[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
    fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..])? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ostal-legacy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Un faux dossier de données de « Matter Home » : identité, sous-dossiers, fichier secret.
    fn old_dir(base: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let old = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        std::fs::create_dir_all(old.join("serveur")).unwrap();
        std::fs::create_dir_all(old.join("history")).unwrap();
        std::fs::create_dir_all(old.join("events/2026-11-20")).unwrap();
        std::fs::write(old.join(FILE_NAME), r#"{"identity":"factice"}"#).unwrap();
        std::fs::set_permissions(old.join(FILE_NAME), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::fs::write(old.join("serveur/compte.json"), "faux compte").unwrap();
        std::fs::write(old.join("maison.json"), "{}").unwrap();
        std::fs::write(old.join("history/1.jsonl"), "x\n".repeat(50_000)).unwrap();
        std::fs::write(old.join("events/2026-11-20/a.bin"), [0u8, 1, 2, 255]).unwrap();
        std::os::unix::fs::symlink("maison.json", old.join("lien")).unwrap();
        old
    }

    /// Instantané d'une arborescence : chemin relatif → (droits, octets ou cible du lien).
    fn snapshot(dir: &Path) -> Vec<(String, u32, Vec<u8>)> {
        use std::os::unix::fs::PermissionsExt;
        let mut out = Vec::new();
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u32, Vec<u8>)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                let m = std::fs::symlink_metadata(&p).unwrap();
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                let mode = m.permissions().mode() & 0o777;
                if m.is_dir() {
                    out.push((rel, mode, vec![]));
                    walk(root, &p, out);
                } else if m.file_type().is_symlink() {
                    let t = std::fs::read_link(&p).unwrap();
                    out.push((rel, 0, t.display().to_string().into_bytes()));
                } else {
                    out.push((rel, mode, std::fs::read(&p).unwrap()));
                }
            }
        }
        walk(dir, dir, &mut out);
        out
    }

    #[test]
    fn copies_verifies_and_keeps_the_old_folder_intact() {
        use std::os::unix::fs::PermissionsExt;
        let base = base("copy");
        let old = old_dir(&base);
        let new = base.join("dev.ostal.OstalServer");
        let before = snapshot(&old);
        let old_mtime = std::fs::metadata(old.join("maison.json"))
            .unwrap()
            .modified()
            .unwrap();

        assert_eq!(plan(&old, &new), Plan::Copy);
        let note = migrate_dir(&old, &new).unwrap().unwrap();
        assert!(note.contains("5 fichiers"), "{note}");
        assert!(note.contains(NEUTRALIZED_IDENTITY), "{note}");
        // Le nouveau est la copie exacte de l'ancien.
        assert_eq!(snapshot(&new), before);
        // L'ancien n'a pas changé, sauf son identité, renommée (même contenu, mêmes droits).
        let renamed: Vec<_> = before
            .iter()
            .cloned()
            .map(|(name, mode, bytes)| {
                let name = if name == FILE_NAME {
                    NEUTRALIZED_IDENTITY.to_string()
                } else {
                    name
                };
                (name, mode, bytes)
            })
            .collect();
        let mut renamed = renamed;
        renamed.sort_by(|a, b| a.0.cmp(&b.0));
        let mut after = snapshot(&old);
        after.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(after, renamed);
        assert!(!old.join(FILE_NAME).exists() && identity_copied(&old));
        assert_eq!(plan(&old, &new), Plan::Nothing);
        assert_eq!(
            std::fs::metadata(&new).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(new.join("maison.json"))
                .unwrap()
                .modified()
                .unwrap(),
            old_mtime
        );
        assert!(!sibling(&new, STAGING_SUFFIX).unwrap().exists());
        // Deuxième lancement : rien à faire, même si l'ancien a changé entre-temps.
        std::fs::write(old.join("maison.json"), "{\"modifie\":1}").unwrap();
        assert_eq!(migrate_dir(&old, &new).unwrap(), None);
        assert_eq!(std::fs::read(new.join("maison.json")).unwrap(), b"{}");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn nothing_to_do_without_an_old_identity() {
        let base = base("nothing");
        let old = base.join("ancien");
        let new = base.join("nouveau");
        assert_eq!(migrate_dir(&old, &new).unwrap(), None);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("maison.json"), "{}").unwrap();
        assert_eq!(migrate_dir(&old, &new).unwrap(), None);
        assert!(!new.exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn an_empty_new_folder_is_replaced_a_non_empty_one_is_set_aside() {
        let base = base("aside");
        let old = old_dir(&base);
        let new = base.join("dev.ostal.OstalServer");
        // Vide (créé par un framework, par exemple) : remplacé.
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(plan(&old, &new), Plan::Copy);
        migrate_dir(&old, &new).unwrap().unwrap();
        assert!(new.join(FILE_NAME).is_file());
        // (On remet l'identité de l'ancien pour recommencer.)
        std::fs::rename(old.join(NEUTRALIZED_IDENTITY), old.join(FILE_NAME)).unwrap();

        // Un compte créé dans le nouveau dossier avant la migration : mis de côté, jamais perdu.
        std::fs::remove_dir_all(&new).unwrap();
        std::fs::create_dir_all(new.join("serveur")).unwrap();
        std::fs::write(new.join("serveur/compte.json"), "nouveau compte").unwrap();
        assert_eq!(plan(&old, &new), Plan::SetAsideThenCopy);
        let note = migrate_dir(&old, &new).unwrap().unwrap();
        assert!(note.contains("sans-identite"), "{note}");
        assert_eq!(
            std::fs::read(new.join("serveur/compte.json")).unwrap(),
            b"faux compte"
        );
        let aside: Vec<_> = std::fs::read_dir(&base)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("dev.ostal.OstalServer.sans-identite-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            std::fs::read(aside[0].path().join("serveur/compte.json")).unwrap(),
            b"nouveau compte"
        );
        // Un nouveau dossier qui a déjà son identité n'est jamais touché.
        assert_eq!(migrate_dir(&old, &new).unwrap(), None);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_leftover_staging_copy_is_redone() {
        let base = base("staging");
        let old = old_dir(&base);
        let new = base.join("dev.ostal.OstalServer");
        let staging = sibling(&new, STAGING_SUFFIX).unwrap();
        std::fs::create_dir_all(staging.join("history")).unwrap();
        std::fs::write(staging.join(FILE_NAME), "à moitié").unwrap();
        let before = snapshot(&old);
        migrate_dir(&old, &new).unwrap().unwrap();
        assert_eq!(snapshot(&new), before);
        assert!(!staging.exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_failed_copy_leaves_no_new_folder_and_the_old_one_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let base = base("fail");
        let old = old_dir(&base);
        let new = base.join("dev.ostal.OstalServer");
        // Un fichier illisible : la copie échoue (sauf pour root, qui lit tout).
        let locked = old.join("history/1.jsonl");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::File::open(&locked).is_ok();
        let before = snapshot_without(&old, &locked);
        let result = migrate_dir(&old, &new);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        if !readable {
            let err = format!("{:#}", result.unwrap_err());
            assert!(err.contains("n'a pas été modifié"), "{err}");
            assert!(!new.exists());
            assert!(!sibling(&new, STAGING_SUFFIX).unwrap().exists());
            assert_eq!(snapshot_without(&old, &locked), before);
            // L'identité de l'ancien n'est neutralisée qu'après une copie réussie.
            assert!(old.join(FILE_NAME).is_file() && !identity_copied(&old));
        }
        std::fs::remove_dir_all(base).unwrap();

        fn snapshot_without(dir: &Path, skip: &Path) -> Vec<String> {
            let mut v = Vec::new();
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                if e.path() != skip {
                    v.push(e.file_name().to_string_lossy().into_owned());
                }
            }
            v.sort();
            v
        }
    }

    #[test]
    fn verification_catches_a_difference() {
        let base = base("verify");
        let a = base.join("a");
        let b = base.join("b");
        for d in [&a, &b] {
            std::fs::create_dir_all(d.join("sous")).unwrap();
            std::fs::write(d.join("sous/f"), "identique").unwrap();
        }
        verify_tree(&a, &b).unwrap();
        std::fs::write(b.join("sous/f"), "identiquE").unwrap();
        assert!(verify_tree(&a, &b).is_err());
        std::fs::write(b.join("sous/f"), "identique").unwrap();
        std::fs::write(b.join("en-trop"), "").unwrap();
        assert!(verify_tree(&a, &b).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_pending_import_keeps_working_under_its_new_name() {
        let base = base("pending");
        let old = old_dir(&base);
        std::fs::write(old.join(PENDING_IMPORT), "sauvegarde en attente").unwrap();
        let new = base.join("dev.ostal.OstalServer");
        migrate_dir(&old, &new).unwrap().unwrap();
        assert!(!new.join(PENDING_IMPORT).exists());
        assert_eq!(
            std::fs::read(crate::backup::pending_path(&new)).unwrap(),
            b"sauvegarde en attente"
        );
        // L'ancien garde le sien.
        assert!(old.join(PENDING_IMPORT).is_file());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn an_interrupted_import_in_the_old_folder_blocks_the_copy() {
        let base = base("recovery");
        let old = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        // Coupure entre les deux renommages de l'import : l'ancien dossier a disparu, le
        // dossier complet attend à côté.
        let complete = base.join("dev.matter-home-desktop.MatterHomeDesktop.import-nouveau");
        std::fs::create_dir_all(&complete).unwrap();
        std::fs::write(complete.join(FILE_NAME), "{}").unwrap();
        let new = base.join("dev.ostal.OstalServer");
        let err = format!("{:#}", migrate_dir(&old, &new).unwrap_err());
        assert!(err.contains("interrompu"), "{err}");
        assert!(!new.exists() && complete.join(FILE_NAME).exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_failed_set_aside_copy_names_the_set_aside_folder() {
        use std::os::unix::fs::PermissionsExt;
        let base = base("aside-fail");
        let old = old_dir(&base);
        let new = base.join("dev.ostal.OstalServer");
        std::fs::create_dir_all(new.join("serveur")).unwrap();
        std::fs::write(new.join("serveur/compte.json"), "nouveau compte").unwrap();
        let locked = old.join("history/1.jsonl");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::File::open(&locked).is_ok();
        let result = migrate_dir(&old, &new);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        if !readable {
            let err = format!("{:#}", result.unwrap_err());
            assert!(
                err.contains("dev.ostal.OstalServer.sans-identite-"),
                "{err}"
            );
            assert!(old.join(FILE_NAME).is_file());
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn an_old_folder_that_is_a_symlink_is_copied() {
        let base = base("symlink");
        let real = old_dir(&base);
        let real_moved = base.join("ailleurs");
        std::fs::rename(&real, &real_moved).unwrap();
        let old = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        std::os::unix::fs::symlink(&real_moved, &old).unwrap();
        let new = base.join("dev.ostal.OstalServer");
        let before = snapshot(&real_moved);
        migrate_dir(&old, &new).unwrap().unwrap();
        assert!(std::fs::symlink_metadata(&new).unwrap().is_dir());
        assert_eq!(snapshot(&new), before);
        // Le lien est toujours là ; l'identité du dossier qu'il désigne est neutralisée.
        assert!(std::fs::symlink_metadata(&old)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(identity_copied(&real_moved));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn free_space_is_checked_against_the_old_folder_size() {
        let base = base("space");
        let old = old_dir(&base);
        // 50 000 × 2 octets d'historique, plus le reste.
        let size = tree_size(&old).unwrap();
        assert!(size >= 100_000 && size < 101_000, "{size}");
        assert!(space_ok(size, size + SPACE_MARGIN).is_ok());
        let err = space_ok(size, size).unwrap_err();
        assert!(err.contains("pas assez d'espace libre"), "{err}");
        assert!(err.contains("Mo"), "{err}");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn an_already_neutralized_name_is_never_overwritten() {
        let base = base("neutral");
        let old = old_dir(&base);
        std::fs::write(old.join(NEUTRALIZED_IDENTITY), "copie précédente").unwrap();
        let new = base.join("dev.ostal.OstalServer");
        migrate_dir(&old, &new).unwrap().unwrap();
        assert_eq!(
            std::fs::read(old.join(NEUTRALIZED_IDENTITY)).unwrap(),
            b"copie pr\xc3\xa9c\xc3\xa9dente"
        );
        assert!(!old.join(FILE_NAME).exists());
        let dated = std::fs::read_dir(&old)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{NEUTRALIZED_IDENTITY}-"))
            })
            .count();
        assert_eq!(dated, 1);
        std::fs::remove_dir_all(base).unwrap();
    }
}
