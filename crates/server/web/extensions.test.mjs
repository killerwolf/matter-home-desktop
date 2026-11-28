// Essai à la main (non embarqué dans le serveur) : node crates/server/web/extensions.test.mjs
// Les phrases de la météo extérieure et des extensions.
import assert from "node:assert/strict";
import {
  BOX_ON_THE_MAC, bboxStatusText, errorText, placeConfig, readingParts, searchable, somfyChoices, somfyConfig, somfyExposedAfter, somfyKindLabel,
  somfyLastError, somfyStatusText, somfySyncedText, weatherCard, weatherStatusText,
} from "./extensions.js";

const at = Date.UTC(2026, 10, 27, 13, 30) / 1000;
const reading = { temperature: 7.4, humidity: 82, pressure: 1013.6, at };

// Les grandeurs, en français ; une grandeur absente est sautée.
{
  const parts = readingParts(reading);
  assert.equal(parts.length, 3);
  assert.match(parts[0], /^7,4\s°C$/);
  assert.match(parts[1], /^82\s%$/);
  assert.match(parts[2], /^1\s?014\shPa$/);
  assert.deepEqual(readingParts({ temperature: null, humidity: 55, pressure: null, at }).length, 1);
  assert.deepEqual(readingParts(null), []);
}

// La carte : rien tant qu'éteinte ou sans relevé ; la dernière valeur reste, marquée, hors ligne.
{
  assert.equal(weatherCard(null), null);
  assert.equal(weatherCard({ enabled: false, reading }), null);
  assert.equal(weatherCard({ enabled: true, place: "Lyon", reading: null }), null);
  const ok = weatherCard({ enabled: true, place: "Lyon", reading, failing: false });
  assert.equal(ok.stale, false);
  assert.match(ok.note, /^Relevé à \d\d:\d\d$/);
  assert.match(ok.aria, /^Dehors : 7,4/);
  const stale = weatherCard({ enabled: true, place: "Lyon", reading, failing: true });
  assert.equal(stale.stale, true);
  assert.match(stale.note, /^Dernier relevé à \d\d:\d\d : Open-Meteo ne répond pas$/);
  assert.equal(stale.line, ok.line);
}

// La page de configuration.
{
  assert.match(weatherStatusText({ enabled: false }), /n'appelle pas Open-Meteo/);
  assert.match(weatherStatusText({ enabled: true, status: { place: null } }), /Choisissez un lieu/);
  assert.match(weatherStatusText({ enabled: true, status: { place: "Lyon", reading: null } }), /Premier relevé/);
  assert.match(weatherStatusText({ enabled: true, status: { place: "Lyon", reading: null, failing: true } }), /ne répond pas/);
  assert.match(weatherStatusText({ enabled: true, status: { place: "Lyon", reading, failing: true } }), /Dernier relevé à \d\d:\d\d \(Open-Meteo/);
}

// Le lieu envoyé, les erreurs, la recherche.
{
  assert.deepEqual(placeConfig({ name: "Lyon", latitude: 45.7, longitude: 4.8, extra: 1 }), { place: { name: "Lyon", latitude: 45.7, longitude: 4.8 } });
  assert.deepEqual(placeConfig(null), { place: null });
  assert.equal(errorText({ detail: "Lieu invalide." }), "Lieu invalide.");
  assert.match(errorText({ status: 404 }), /mettez-le à jour/);
  assert.equal(errorText(new Error("x")), "Échec, réessayez.");
  assert.equal(searchable("L"), false);
  assert.equal(searchable(" 69 "), true);
  assert.equal(searchable(undefined), false);
}

console.log("extensions.test.mjs : ok");

// Somfy : les phrases d'état, le choix des appareils, la configuration envoyée.
{
  const view = (state, devices = 2) => ({ status: { state, devices } });
  assert.match(somfyStatusText(view("off")), /^Désactivée/);
  assert.match(somfyStatusText(view("unconfigured")), /identifiant et le mot de passe/);
  assert.match(somfyStatusText(view("ok", 2)), /^Connecté à Somfy : 2 volets dans Ostal\.$/);
  assert.match(somfyStatusText(view("ok", 1)), /1 volet dans Ostal/);
  assert.match(somfyStatusText(view("ok", 0)), /aucun volet n'est coché/);
  assert.match(somfyStatusText(view("offline", 2)), /dernière liste connue/);
  assert.match(somfyStatusText(view("rejected")), /n'essaiera plus/);
  assert.equal(somfyStatusText(null), "");
  assert.equal(somfyLastError({ status: { last_error: "Somfy ne répond pas." } }), "Somfy ne répond pas.");

  assert.equal(somfyKindLabel("RollerShutter"), "volet roulant");
  assert.equal(somfyKindLabel("Truc"), "Truc");
  assert.equal(somfyKindLabel(""), "appareil");

  const choices = {
    devices: [
      { key: "led", name: "Voyant du Kit", kind: "Light", commandable: false, selected: false },
      { key: "b", name: "Garage", kind: "RollerShutter", commandable: true, selected: true },
      { key: "a", name: "Porte", kind: "RollerShutter", commandable: true, selected: false },
    ],
    synced_at: at,
  };
  assert.deepEqual(somfyChoices(choices).map((d) => d.key), ["b", "a", "led"], "les commandables d'abord");
  assert.deepEqual(somfyExposedAfter(choices, "a", true), ["b", "a"]);
  assert.deepEqual(somfyExposedAfter(choices, "b", false), []);
  assert.deepEqual(somfyExposedAfter(choices, "b", true), ["b"], "pas de doublon");
  assert.deepEqual(somfyConfig("  sam@example.org ", ["a"]), { email: "sam@example.org", exposed: ["a"] });
  assert.deepEqual(somfyConfig(undefined, undefined), { email: "", exposed: null });
  assert.match(somfySyncedText(choices), /^Liste lue chez Somfy à \d\d:\d\d\.$/);
  assert.equal(somfySyncedText({ devices: [] }), null);
}

// Bbox : l'état de la box en une phrase, sans adresse ni mot de passe.
{
  const view = (state, extra = {}) => ({ status: { state, ...extra } });
  assert.match(bboxStatusText(view("off")), /^Désactivée/);
  assert.match(bboxStatusText(view("unconfigured")), /mot de passe de la box manque/);
  assert.match(bboxStatusText(view("waiting")), /Premier relevé/);
  assert.match(bboxStatusText(view("unreachable")), /figée/);
  assert.match(bboxStatusText(view("refused")), /refuse le mot de passe/);
  assert.match(bboxStatusText(view("legacy")), /Ancien réglage/);
  assert.match(bboxStatusText(view("ok", { devices: 0 })), /aucun appareil actif/);
  assert.match(bboxStatusText(view("ok", { devices: 1 })), /1 appareil actif sur/);
  assert.match(bboxStatusText(view("ok", { devices: 6, last_ok: at })), /6 appareils actifs sur le Wi-Fi \(relevé à \d\d:\d\d\)/);
  assert.equal(bboxStatusText(null), "");
  assert.match(BOX_ON_THE_MAC, /Ostal Server, sur le Mac$/);
}
