//! Les extensions, de bout en bout : le vrai binaire en mode `--demo` (dont la météo est fictive :
//! **aucun appel réseau**), dossier jetable, 127.0.0.1. Éteintes par défaut, activation, lieu,
//! relevé, désactivation, droits, journal d'audit, fichier privé.

mod common;

use serde_json::json;

use common::{temp_dir, Server};

fn audit_text(dir: &std::path::Path) -> String {
    let mut out = String::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("serveur/audit")) {
        for file in entries.flatten() {
            out.push_str(&std::fs::read_to_string(file.path()).unwrap_or_default());
        }
    }
    out
}

#[test]
fn weather_is_off_by_default_and_follows_the_switch() {
    let dir = temp_dir("extensions-weather");
    let server = Server::start(&dir);
    let client = server.client();
    common::runtime().block_on(async {
        let token = client.login().await;

        // Tout est éteint, la liste dit ce que sort l'extension.
        let list = client.get("/api/extensions", &token).await;
        assert_eq!(list.status, 200, "{}", list.text());
        let weather = &list.json()["extensions"][0];
        assert_eq!(weather["id"], "weather");
        assert_eq!(weather["enabled"], false);
        assert!(weather["disclosure"]
            .as_str()
            .unwrap()
            .contains("Open-Meteo"));
        let off = client.get("/api/weather", &token).await.json();
        assert_eq!(off["enabled"], false);
        assert!(off["reading"].is_null());

        // Éteinte : pas de recherche de lieu (donc aucun appel).
        let r = client
            .get("/api/extensions/weather/search?q=Lyon", &token)
            .await;
        assert_eq!(r.status, 400, "{}", r.text());

        // Une extension inconnue, un corps de travers, un secret qui n'existe pas.
        for (body, status) in [(json!({"enabled": true}), 404), (json!({"nope": 1}), 400)] {
            let path = if status == 404 {
                "/api/extensions/inconnue"
            } else {
                "/api/extensions/weather"
            };
            let r = client.send_json("PUT", path, &token, &body).await;
            assert_eq!(r.status, status, "{path} {}", r.text());
        }
        let r = client
            .send_json(
                "PUT",
                "/api/extensions/weather",
                &token,
                &json!({"secrets": {"mot-de-passe": "x"}}),
            )
            .await;
        assert_eq!(r.status, 400, "{}", r.text());
        let r = client
            .send_json(
                "PUT",
                "/api/extensions/weather",
                &token,
                &json!({"config": {"place": {"name": "X", "latitude": 99, "longitude": 0}}}),
            )
            .await;
        assert_eq!(r.status, 400, "{}", r.text());

        // Activation, recherche, choix du lieu : le premier relevé part tout de suite.
        let on = client
            .send_json(
                "PUT",
                "/api/extensions/weather",
                &token,
                &json!({"enabled": true}),
            )
            .await;
        assert_eq!(on.status, 200, "{}", on.text());
        assert_eq!(on.json()["enabled"], true);
        let found = client
            .get("/api/extensions/weather/search?q=Lyon", &token)
            .await;
        assert_eq!(found.status, 200, "{}", found.text());
        let place = found.json()["places"][0].clone();
        assert!(place["latitude"].is_number());
        let chosen = client
            .send_json(
                "PUT",
                "/api/extensions/weather",
                &token,
                &json!({"config": {"place": place}}),
            )
            .await;
        assert_eq!(chosen.status, 200, "{}", chosen.text());
        let now = client.get("/api/weather", &token).await.json();
        assert_eq!(now["enabled"], true);
        assert_eq!(now["place"], place["name"]);
        assert!(now["reading"]["temperature"].is_number(), "{now}");
        assert!(now["reading"]["humidity"].is_number());
        assert!(now["reading"]["pressure"].is_number());
        assert_eq!(now["failing"], false);

        // Désactivation : plus de relevé affiché, la recherche est de nouveau refusée.
        let off = client
            .send_json(
                "PUT",
                "/api/extensions/weather",
                &token,
                &json!({"enabled": false}),
            )
            .await;
        assert_eq!(off.status, 200, "{}", off.text());
        let after = client.get("/api/weather", &token).await.json();
        assert_eq!(after["enabled"], false);
        assert!(after["reading"].is_null() && after["place"].is_null());
        let r = client
            .get("/api/extensions/weather/search?q=Lyon", &token)
            .await;
        assert_eq!(r.status, 400);

        // Sans jeton : refusé.
        let r = client.request("GET", "/api/extensions", None, None).await;
        assert_eq!(r.status, 401);
    });

    // Fichier privé, journal d'audit sans la moindre valeur.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("serveur/extensions.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let audit = audit_text(&dir);
    for detail in ["on", "config", "off"] {
        assert!(
            audit.contains("extension_setting") && audit.contains(&format!("\"{detail}\"")),
            "ligne {detail} absente du journal :\n{audit}"
        );
    }
    assert!(!audit.contains("latitude"), "aucune valeur au journal");
}
