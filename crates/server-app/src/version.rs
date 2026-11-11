//! La version affichée dans le menu. `CARGO_PKG_VERSION` ne porte pas le suffixe d'une
//! pré-version (1.0.0 pour `v1.0.0-rc.7`) : la CI donne le nom complet au build par la variable
//! d'environnement `MATTER_HOME_VERSION`, lue ici à la compilation. Sans elle (build local),
//! « 1.0.0 (dev) ».

/// Le nom complet de la version, tel qu'il s'affiche.
pub fn display_version() -> String {
    format_version(
        option_env!("MATTER_HOME_VERSION"),
        env!("CARGO_PKG_VERSION"),
    )
}

pub fn format_version(full: Option<&str>, package: &str) -> String {
    match full.map(|v| v.trim().trim_start_matches('v')) {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => format!("{package} (dev)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ci_version_is_shown_without_the_leading_v() {
        assert_eq!(format_version(Some("v1.0.0-rc.7"), "1.0.0"), "1.0.0-rc.7");
        assert_eq!(format_version(Some("1.0.0"), "1.0.0"), "1.0.0");
    }

    #[test]
    fn without_it_the_build_is_marked_dev() {
        assert_eq!(format_version(None, "1.0.0"), "1.0.0 (dev)");
        assert_eq!(format_version(Some(" "), "1.0.0"), "1.0.0 (dev)");
    }
}
