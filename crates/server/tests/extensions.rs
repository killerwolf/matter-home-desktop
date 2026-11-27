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

/// Les volets Somfy de bout en bout en démonstration (cloud fictif : **aucun appel réseau**) :
/// éteinte par défaut, le compte, les volets dans Ostal, un ordre, le plan, puis tout disparaît.
#[test]
fn somfy_shutters_follow_the_extension_in_demo() {
    let dir = temp_dir("extensions-somfy");
    let server = Server::start(&dir);
    let client = server.client();
    common::runtime().block_on(async {
        let token = client.login().await;
        let somfy_devices = |state: &serde_json::Value| -> Vec<serde_json::Value> {
            state["devices"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["device"]["vendor"] == "Somfy")
                .cloned()
                .collect()
        };

        // Éteinte par défaut, avec son mot de passe déclaré, et aucun volet.
        let list = client.get("/api/extensions", &token).await.json();
        let somfy = list["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == "somfy")
            .unwrap()
            .clone();
        assert_eq!(somfy["enabled"], false);
        assert_eq!(somfy["secrets"][0]["key"], "password");
        assert_eq!(somfy["secrets"][0]["set"], false);
        assert!(somfy["secrets"][0]["help"]
            .as_str()
            .unwrap()
            .contains("TaHoma"));
        assert!(somfy["disclosure"].as_str().unwrap().contains("Somfy"));
        assert!(somfy_devices(&client.get("/api/state", &token).await.json()).is_empty());

        // Allumée sans compte : rien ne part, rien n'apparaît.
        let r = client
            .send_json(
                "PUT",
                "/api/extensions/somfy",
                &token,
                &json!({"enabled": true}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.json()["status"]["state"], "unconfigured");

        // Le compte (mot de passe avec des guillemets) : la liste est lue, les volets arrivent.
        let r = client
            .send_json(
                "PUT",
                "/api/extensions/somfy",
                &token,
                &json!({"config": {"email": "sam@example.org", "exposed": null},
                        "secrets": {"password": "a\"b\\c"}}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert!(
            !r.text().contains("a\\\"b"),
            "le mot de passe ne revient jamais"
        );
        assert_eq!(r.json()["status"]["state"], "ok");
        assert_eq!(r.json()["status"]["devices"], 2);
        let state = client.get("/api/state", &token).await.json();
        common::assert_no_big_numbers(&state);
        let shutters = somfy_devices(&state);
        let names: Vec<_> = shutters
            .iter()
            .map(|d| d["device"]["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["Volet de la véranda", "Volet du garage"]);
        let first = &shutters[0];
        assert_eq!(first["device"]["caps"]["cover"], true);
        assert_eq!(first["device"]["caps"]["cover_position"], false);
        assert_eq!(first["status"]["state"], "online");
        assert!(
            first["latest"].as_array().unwrap().is_empty(),
            "jamais commandé"
        );
        let id = first["device"]["node_id"].as_str().unwrap().to_string();
        let id_num: u64 = id.parse().unwrap();
        assert!(protocol::events::is_extension_id(id_num));

        // Le choix des appareils (droit « extensions ») : trois appareils, le voyant non coché.
        let choices = client
            .get("/api/extensions/somfy/devices", &token)
            .await
            .json();
        assert_eq!(choices["devices"].as_array().unwrap().len(), 3);
        assert_eq!(choices["devices"][2]["commandable"], false);
        assert_eq!(choices["devices"][2]["selected"], false);
        // Le statut visible de tous ne contient ni l'identifiant ni une adresse.
        let shown = client.get("/api/extensions/somfy", &token).await.text();
        assert!(!shown.contains("rts://"), "{shown}");

        // Un ordre : fermer ; l'état montré est ce dernier ordre. Ni arrêt ni pourcentage.
        let r = client
            .send_json(
                "POST",
                &format!("/api/devices/{id}/cover"),
                &token,
                &json!({"command": "Close"}),
            )
            .await;
        assert_eq!(r.status, 202, "{}", r.text());
        for bad in [json!("Stop"), json!({"Position": 40})] {
            let r = client
                .send_json(
                    "POST",
                    &format!("/api/devices/{id}/cover"),
                    &token,
                    &json!({"command": bad}),
                )
                .await;
            assert_eq!(r.status, 400, "{}", r.text());
        }
        let state = client.get("/api/state", &token).await.json();
        let after = &somfy_devices(&state)[0];
        assert_eq!(after["latest"][0]["metric"], "position");
        assert_eq!(after["latest"][0]["value"], 0.0);
        // Un volet d'extension ne s'oublie pas : il se règle dans l'extension.
        let r = client
            .send_json("DELETE", &format!("/api/devices/{id}"), &token, &json!({}))
            .await;
        assert_eq!(r.status, 400, "{}", r.text());

        // Le plan : une fenêtre équipée du volet.
        let mut plan = client.get("/api/plan", &token).await.json();
        plan["floors"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": "900", "kind": {"type": "Decor", "symbol": "Window", "label": "Fenêtre"},
                "x": 1.0, "y": 1.0, "rotation": 0.0
            }));
        plan["shutters"] = json!([{"id": "900", "node_id": id}]);
        let r = client.send_json("PUT", "/api/plan", &token, &plan).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let plan = client.get("/api/plan", &token).await.json();
        assert_eq!(plan["shutters"], json!([{"id": "900", "node_id": id}]));
        // Un lien vers un volet hors plage est refusé.
        let mut bad = plan.clone();
        bad["shutters"] = json!([{"id": "900", "node_id": "5"}]);
        let r = client.send_json("PUT", "/api/plan", &token, &bad).await;
        assert_eq!(r.status, 400, "{}", r.text());

        // Mot de passe effacé : mêmes effets que l'extinction, les volets quittent Ostal.
        let r = client
            .send_json(
                "PUT",
                "/api/extensions/somfy",
                &token,
                &json!({"secrets": {"password": null}}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.json()["status"]["state"], "unconfigured");
        assert!(somfy_devices(&client.get("/api/state", &token).await.json()).is_empty());
        let r = client
            .send_json(
                "POST",
                &format!("/api/devices/{id}/cover"),
                &token,
                &json!({"command": "Open"}),
            )
            .await;
        assert_eq!(r.status, 404, "{}", r.text());
        // Le plan, lui, garde le lien : rallumée, tout revient.
        let plan = client.get("/api/plan", &token).await.json();
        assert_eq!(plan["shutters"], json!([{"id": "900", "node_id": id}]));

        // Mot de passe remis puis extension éteinte : les volets reviennent, puis repartent.
        client
            .send_json(
                "PUT",
                "/api/extensions/somfy",
                &token,
                &json!({"secrets": {"password": "autre"}}),
            )
            .await;
        let state = client.get("/api/state", &token).await.json();
        let again = somfy_devices(&state);
        assert_eq!(again.len(), 2);
        assert_eq!(
            again[0]["device"]["node_id"],
            id.as_str(),
            "même identifiant"
        );
        let r = client
            .send_json(
                "PUT",
                "/api/extensions/somfy",
                &token,
                &json!({"enabled": false}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.json()["status"]["state"], "off");
        assert!(somfy_devices(&client.get("/api/state", &token).await.json()).is_empty());
    });

    // Fichiers privés ; le journal d'audit ne dit aucune valeur.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("serveur/somfy.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let audit = audit_text(&dir);
    assert!(audit.contains("extension_setting"));
    for value in ["sam@example.org", "a\\\"b", "autre", "rts://"] {
        assert!(!audit.contains(value), "{value} au journal :\n{audit}");
    }
    assert!(!server.log().contains("sam@example.org"));
    assert!(!server.log().contains("rts://"));
}
