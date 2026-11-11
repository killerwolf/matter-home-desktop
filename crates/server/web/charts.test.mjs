// Essai à la main (non embarqué dans le serveur) : node crates/server/web/charts.test.mjs
// Les repères d'axe au changement d'heure (offset injecté, comme Europe/Paris).
import assert from "node:assert/strict";
import { roundMarks, valueOnCurve } from "./charts.js";

const sec = (iso) => Date.parse(iso) / 1000;
const fall = sec("2026-10-25T01:00:00Z"); // 3 h d'été -> 2 h d'hiver
const spring = sec("2026-03-29T01:00:00Z"); // 2 h d'hiver -> 3 h d'été
const paris = (change, before, after) => (t) => (t < change ? before : after);

// Heure d'hiver : l'heure locale 2 h existe deux fois, les deux instants sont des repères.
{
  const offset = paris(fall, 7200, 3600);
  const { marks, step } = roundMarks(sec("2026-10-24T22:00:00Z"), sec("2026-10-25T08:00:00Z"), offset);
  assert.equal(step, 3600);
  assert.equal(new Set(marks).size, marks.length, "instants distincts");
  assert.deepEqual([...marks].sort((a, b) => a - b), marks, "dans l'ordre");
  const twoAm = marks.filter((t) => (t + offset(t)) % 86400 === 7200);
  assert.equal(twoAm.length, 2, "2 h locale en double");
  assert.equal(twoAm[1] - twoAm[0], 3600);
  assert.equal(marks.length, 11); // 00 h à 09 h locales, 2 h comptée deux fois
}

// Heure d'été : l'heure locale 2 h n'existe pas, pas de repère pour elle.
{
  const offset = paris(spring, 3600, 7200);
  const { marks } = roundMarks(sec("2026-03-28T22:00:00Z"), sec("2026-03-29T06:00:00Z"), offset);
  assert.equal(marks.filter((t) => (t + offset(t)) % 86400 === 7200).length, 0);
  assert.equal(new Set(marks).size, marks.length);
}

// Courbe : interpolation dans un segment, rien dans un trou.
{
  const segments = [[{ at: 0, value: 10 }, { at: 100, value: 20 }], [{ at: 500, value: 30 }]];
  assert.equal(valueOnCurve(segments, 50), 15);
  assert.equal(valueOnCurve(segments, 300), null);
  assert.equal(valueOnCurve(segments, 500, 1), 30);
}
console.log("ok");
