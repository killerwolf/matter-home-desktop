//! La présence des personnes de la maison, telle que les clients la lisent (voir
//! `docs/comptes-presence.md`). Jamais une adresse MAC ni une IP : seulement des prénoms, un état et,
//! pour son propre compte, le nom de son téléphone.

use serde::{Deserialize, Serialize};

/// `GET` : qui est à la maison.
pub const PEOPLE_PATH: &str = "/api/presence/people";
/// `GET` : les téléphones encore libres (noms seulement) et le sien.
pub const PHONES_PATH: &str = "/api/wifi/phones";
/// `POST` : relève la box maintenant (« Actualiser la liste »).
pub const PHONES_REFRESH_PATH: &str = "/api/wifi/refresh";
/// `PUT` `{"phone": "nom" | null}` : choisit (ou retire) son propre téléphone.
pub const PHONE_PATH: &str = "/api/wifi/phone";

/// Où est une personne.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Where {
    Home,
    Away,
    #[serde(other)]
    Unknown,
}

/// L'état de la liaison du serveur avec la box de la maison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoxLink {
    /// Pas de box reliée.
    #[default]
    None,
    Ok,
    Unreachable,
    Refused,
    #[serde(other)]
    Other,
}

/// Une personne et ce qu'on sait d'elle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonPresence {
    /// Son prénom (nom affiché du compte).
    pub name: String,
    pub state: Where,
    /// Depuis quand (secondes Unix) : le dernier changement.
    pub since: f64,
    /// C'est la personne qui demande.
    #[serde(default)]
    pub me: bool,
}

/// Réponse de `GET /api/presence/people`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeopleView {
    /// Les personnes dont l'état est connu (les autres ne sont pas listées).
    #[serde(default)]
    pub people: Vec<PersonPresence>,
    /// L'interrupteur « Changer le mode de la maison selon la présence » est activé.
    #[serde(default)]
    pub auto_mode: bool,
    #[serde(default)]
    pub box_link: BoxLink,
    /// Dernier relevé réussi de la box (secondes Unix).
    #[serde(default)]
    pub updated_at: Option<f64>,
}

/// Réponse de `GET /api/wifi/phones` et `POST /api/wifi/refresh`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhonesView {
    /// Une box est reliée : sans elle, l'app ne demande rien.
    #[serde(default)]
    pub linked: bool,
    #[serde(default)]
    pub box_link: BoxLink,
    /// Le prénom (nom affiché) de la personne qui demande : « Bonjour Léa. »
    #[serde(default)]
    pub name: String,
    /// Le nom du téléphone de la personne qui demande, s'il est choisi.
    #[serde(default)]
    pub mine: Option<String>,
    /// Les noms des téléphones vus sur le Wi-Fi et pas encore attribués. Jamais d'adresse.
    #[serde(default)]
    pub free: Vec<String>,
}

/// Corps de `PUT /api/wifi/phone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneChoice {
    /// Obligatoire (même `null`) : un corps sans ce champ ne retire jamais le téléphone.
    #[serde(deserialize_with = "required")]
    pub phone: Option<String>,
}

fn required<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}

/// Ce que l'app de bureau écrit dans sa barre du haut (maquette Bureau-Maison-Presence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// « À la maison : Hamdi, Sam » (ou « Personne n'est à la maison »).
    pub lead: String,
    /// « Léa n'est pas là depuis 17:20 » ; vide quand tout le monde est là. Aucun genre supposé.
    pub away: String,
    /// Le mode suit la présence : « Mode : Présent · automatique ».
    pub auto: bool,
    /// La box ne répond pas : la présence peut ne pas être à jour.
    pub stale: bool,
}

fn join(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} et {last}", init.join(", ")),
    }
}

/// Le résumé d'une réponse de `GET /api/presence/people`, ou `None` quand il n'y a rien à dire (pas de
/// box, personne de connu). `clock` écrit une heure locale « HH:MM » (secondes Unix).
pub fn summary(view: &PeopleView, clock: impl Fn(f64) -> String) -> Option<Summary> {
    let stale = matches!(view.box_link, BoxLink::Unreachable | BoxLink::Refused);
    let known: Vec<&PersonPresence> = view
        .people
        .iter()
        .filter(|p| matches!(p.state, Where::Home | Where::Away))
        .collect();
    if known.is_empty() && !view.auto_mode && !stale {
        return None;
    }
    let names = |state| -> Vec<&str> {
        known
            .iter()
            .filter(|p| p.state == state)
            .map(|p| p.name.as_str())
            .collect()
    };
    let (home, away) = (names(Where::Home), names(Where::Away));
    let lead = if home.is_empty() {
        "Personne n'est à la maison".to_string()
    } else {
        format!("À la maison : {}", home.join(", "))
    };
    let away_text = match away.as_slice() {
        [] => String::new(),
        [one] => {
            let since = known
                .iter()
                .find(|p| p.state == Where::Away && p.name == *one)
                .filter(|p| p.since > 0.0)
                .map(|p| format!(" depuis {}", clock(p.since)))
                .unwrap_or_default();
            format!("{one} n'est pas là{since}")
        }
        many => format!("{} ne sont pas là", join(many)),
    };
    Some(Summary {
        lead,
        away: away_text,
        auto: view.auto_mode,
        stale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, state: Where, since: f64) -> PersonPresence {
        PersonPresence {
            name: name.into(),
            state,
            since,
            me: false,
        }
    }

    #[test]
    fn the_summary_says_who_is_home_and_who_is_not_without_a_gender() {
        let view = PeopleView {
            people: vec![
                person("Hamdi", Where::Home, 1.0),
                person("Sam", Where::Home, 2.0),
                person("Léa", Where::Away, 100.0),
            ],
            auto_mode: true,
            box_link: BoxLink::Ok,
            updated_at: None,
        };
        let s = summary(&view, |_| "17:20".into()).unwrap();
        assert_eq!(s.lead, "À la maison : Hamdi, Sam");
        assert_eq!(s.away, "Léa n'est pas là depuis 17:20");
        assert!(s.auto && !s.stale);
        let several = PeopleView {
            people: vec![
                person("Léa", Where::Away, 1.0),
                person("Adam", Where::Away, 2.0),
            ],
            ..PeopleView::default()
        };
        let s = summary(&several, |_| "x".into()).unwrap();
        assert_eq!(s.lead, "Personne n'est à la maison");
        assert_eq!(s.away, "Léa et Adam ne sont pas là");
        assert!(!s.auto);
    }

    #[test]
    fn nothing_to_say_without_a_box_or_anyone_known() {
        assert_eq!(summary(&PeopleView::default(), |_| String::new()), None);
        let muted = PeopleView {
            box_link: BoxLink::Unreachable,
            ..PeopleView::default()
        };
        assert!(summary(&muted, |_| String::new()).unwrap().stale);
    }

    #[test]
    fn an_old_server_without_a_field_or_a_newer_state_still_reads() {
        let view: PeopleView = serde_json::from_str(
            r#"{"people":[{"name":"X","state":"dormant","since":1.0}],"box_link":"nouveau"}"#,
        )
        .unwrap();
        assert_eq!(view.people[0].state, Where::Unknown);
        assert_eq!(view.box_link, BoxLink::Other);
        assert!(serde_json::from_str::<PhoneChoice>("{}").is_err());
        assert!(serde_json::from_str::<PhoneChoice>(r#"{"phone":null}"#).is_ok());
    }
}
