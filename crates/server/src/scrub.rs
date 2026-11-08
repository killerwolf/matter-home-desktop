//! Nettoyage des textes libres venus de la couche Matter avant de les envoyer aux clients :
//! ni chemin local (dossier de données, dossier personnel), ni nom d'utilisateur du Mac.
//! Le texte complet reste dans le journal du serveur.

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    /// Préfixes remplacés, du plus long au plus court : (préfixe, remplacement).
    prefixes: Vec<(String, &'static str)>,
}

impl Scrubber {
    pub fn new(data_dir: &Path) -> Self {
        let mut prefixes = vec![(data_dir.display().to_string(), "<données>")];
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy().into_owned();
            if home.len() > 1 {
                prefixes.push((home, "~"));
            }
        }
        prefixes.retain(|(p, _)| !p.is_empty());
        prefixes.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        Self { prefixes }
    }

    pub fn clean(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (prefix, replacement) in &self.prefixes {
            out = out.replace(prefix.as_str(), replacement);
        }
        mask_paths(&out)
    }
}

/// Remplace tout mot qui ressemble à un chemin absolu (`/a/b…`) par `<chemin>`.
fn mask_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first = true;
    for word in text.split(' ') {
        if !first {
            out.push(' ');
        }
        first = false;
        out.push_str(&mask_word(word));
    }
    out
}

fn mask_word(word: &str) -> String {
    let is_edge = |c: char| matches!(c, '(' | '[' | '"' | '\'' | '«' | '`');
    let start = word.len() - word.trim_start_matches(is_edge).len();
    let body = &word[start..];
    if !body.starts_with('/') || body.matches('/').count() < 2 {
        return word.to_string();
    }
    let trail_len = body.len()
        - body
            .trim_end_matches(|c: char| {
                matches!(
                    c,
                    ')' | ']' | '"' | '\'' | '»' | '`' | ',' | ';' | ':' | '.'
                )
            })
            .len();
    format!(
        "{}<chemin>{}",
        &word[..start],
        &body[body.len() - trail_len..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_paths_never_reach_clients() {
        let s = Scrubber::new(Path::new("/Users/hamdi/Library/Application Support/mh"));
        let t = s.clean(
            "Données : /Users/hamdi/Library/Application Support/mh/controller.json — fabric 0x1",
        );
        assert!(!t.contains("/Users"), "{t}");
        assert!(t.contains("fabric 0x1"));
        let t = s.clean("lecture de /var/folders/xx/yy.json impossible (/tmp/a/b), réessai");
        assert!(!t.contains("/var") && !t.contains("/tmp"), "{t}");
        assert!(t.contains("impossible (<chemin>), réessai"), "{t}");
        // Les identifiants Matter (1/0x0402/0x0000) ne sont pas des chemins.
        assert_eq!(
            s.clean("pas de réponse (lecture 1/0x0402/0x0000)"),
            "pas de réponse (lecture 1/0x0402/0x0000)"
        );
    }
}
