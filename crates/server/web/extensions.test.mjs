// Essai à la main (non embarqué dans le serveur) : node crates/server/web/extensions.test.mjs
// Les phrases de la météo extérieure et des extensions.
import assert from "node:assert/strict";
import { errorText, placeConfig, readingParts, searchable, weatherCard, weatherStatusText } from "./extensions.js";

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
