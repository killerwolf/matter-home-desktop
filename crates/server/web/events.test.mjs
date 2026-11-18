// node --test crates/server/web/events.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";

import { classLabel, classesText, descriptionText, niveauLabel, whenText } from "./events.js";

test("les classes en clair, la plus sûre d'abord", () => {
  assert.equal(classLabel("vehicule"), "Véhicule");
  assert.equal(classLabel("girafe"), "Objet");
  assert.equal(classesText({ classes: ["personne", "chien"] }), "Personne, Chien");
  assert.equal(classesText({ classes: [] }), "Objet");
});

test("l'heure dit aujourd'hui, hier, ou la date", () => {
  const now = new Date(2026, 10, 18, 15, 0).getTime();
  const at = (d, h, m) => new Date(2026, 10, d, h, m).getTime() / 1000;
  assert.match(whenText(at(18, 14, 5), now), /^aujourd'hui 14:05$/);
  assert.match(whenText(at(17, 9, 12), now), /^hier 09:12$/);
  assert.match(whenText(at(3, 22, 40), now), /^3 nov\.? 22:40$/);
});

test("le niveau de menace et la description en clair", () => {
  assert.equal(niveauLabel(0), "normal");
  assert.equal(niveauLabel(1), "suspect");
  assert.equal(niveauLabel(2), "critique");
  assert.equal(niveauLabel(3), null);
  assert.equal(niveauLabel(undefined), null);
  assert.equal(descriptionText({ description: "Un livreur dépose un colis." }), "Un livreur dépose un colis.");
  assert.equal(descriptionText({}), "");
});
