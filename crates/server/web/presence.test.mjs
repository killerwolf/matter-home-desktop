// À lancer à la main : node --test crates/server/web/presence.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import {
  joinNames, presenceCard, modeChipLabel, shouldAskPhone, readLater, writeLater, clearLater, LATER_MS,
  chooserTexts, listWithMine, chooseError, isMissingRoute, initial,
} from "./presence.js";

const at = (h, m) => new Date(2026, 10, 22, h, m).getTime() / 1000;
const PEOPLE = {
  auto_mode: true,
  box_link: "ok",
  people: [
    { name: "Léa", state: "away", since: at(17, 20), me: false },
    { name: "Hamdi", state: "home", since: at(8, 2), me: true },
    { name: "Sam", state: "home", since: at(9, 0), me: false },
  ],
};

test("les prénoms se joignent à la française", () => {
  assert.equal(joinNames([]), "");
  assert.equal(joinNames(["Sam"]), "Sam");
  assert.equal(joinNames(["Sam", "Léa"]), "Sam et Léa");
  assert.equal(joinNames(["Sam", "Léa", "Adam"]), "Sam, Léa et Adam");
});

test("la carte dit qui est là, qui n'y est pas, et le mode automatique (maquette)", () => {
  const card = presenceCard(PEOPLE, "present");
  assert.equal(card.lead, "À la maison : vous et Sam");
  assert.equal(card.away, "Léa n'est pas là depuis 17:20");
  assert.equal(card.mode, "Mode Présent, automatique");
  assert.equal(card.note, "");
  assert.deepEqual(card.people.map((p) => [p.name, p.initial, p.home]), [
    ["Vous", "H", true], ["Sam", "S", true], ["Léa", "L", false],
  ]);
  assert.equal(card.people[0].aria, "Vous (Hamdi), à la maison");
  assert.equal(card.people[2].aria, "Léa, pas là depuis 17:20");
});

test("aucune phrase ne suppose un genre", () => {
  const card = presenceCard(PEOPLE, "present");
  const all = [card.lead, card.away, card.mode, ...card.people.map((p) => p.aria)].join(" ");
  assert.doesNotMatch(all, /\bpartie?s?\b|\bsortie?s?\b|\babsente?s?\b/);
});

test("plusieurs absents, soi-même absent, personne à la maison", () => {
  const away = (name, me = false) => ({ name, state: "away", since: at(10, 0), me });
  assert.equal(presenceCard({ box_link: "ok", people: [away("Léa"), away("Adam")] }, "absent").away, "Léa et Adam ne sont pas là");
  assert.equal(presenceCard({ box_link: "ok", people: [away("Hamdi", true)] }, "absent").away, "Vous n'êtes pas là depuis 10:00");
  const none = presenceCard({ box_link: "ok", people: [away("Léa")] }, "absent");
  assert.equal(none.lead, "Personne n'est à la maison");
  assert.equal(none.mode, "Mode Absent", "sans le mot automatique quand l'interrupteur est éteint");
});

test("sans box ni personne, aucune carte ; box muette, une note", () => {
  assert.equal(presenceCard(null, "present"), null);
  assert.equal(presenceCard({ people: [], auto_mode: false, box_link: "none" }, "present"), null);
  const down = presenceCard({ ...PEOPLE, box_link: "unreachable" }, "present");
  assert.match(down.note, /La box ne répond pas/);
  assert.match(presenceCard({ people: [], box_link: "refused" }, "present").note, /La box ne répond pas/);
  // Un état inconnu d'un serveur plus récent est ignoré.
  assert.equal(presenceCard({ box_link: "ok", people: [{ name: "X", state: "dormant" }] }, "present"), null);
});

test("la pastille du mode dit « automatique » à voix haute", () => {
  assert.equal(modeChipLabel("present", false), "Mode : Présent");
  assert.equal(modeChipLabel("nuit", true), "Mode : Nuit, automatique");
});

function memory(initial = {}) {
  const data = { ...initial };
  return { getItem: (k) => data[k] ?? null, setItem: (k, v) => { data[k] = String(v); }, removeItem: (k) => { delete data[k]; }, data };
}

test("on demande le téléphone une fois la box reliée, jusqu'à ce qu'il soit choisi ou remis à plus tard", () => {
  const now = 10 * LATER_MS;
  assert.equal(shouldAskPhone(null, 0, now), false);
  assert.equal(shouldAskPhone({ linked: false, mine: null }, 0, now), false);
  assert.equal(shouldAskPhone({ linked: true, mine: "iPhone-de-Sam" }, 0, now), false);
  assert.equal(shouldAskPhone({ linked: true, mine: null }, 0, now), true);
  assert.equal(shouldAskPhone({ linked: true, mine: null }, now - 1000, now), false, "plus tard : pas tout de suite");
  assert.equal(shouldAskPhone({ linked: true, mine: null }, now - LATER_MS - 1, now), true, "au bout d'une semaine");
});

test("« plus tard » se garde sans jamais échouer", () => {
  const storage = memory();
  assert.equal(readLater(storage), 0);
  writeLater(12345, storage);
  assert.equal(readLater(storage), 12345);
  clearLater(storage);
  assert.equal(readLater(storage), 0);
  const throwing = { getItem() { throw new Error("refusé"); }, setItem() { throw new Error("refusé"); }, removeItem() { throw new Error("refusé"); } };
  assert.equal(readLater(throwing), 0);
  writeLater(1, throwing);
  clearLater(throwing);
  assert.equal(readLater(memory({ "ostal.phone.later": "pas un nombre" })), 0);
});

test("les textes de l'écran suivent la maquette", () => {
  const t = chooserTexts({ linked: true, mine: null, free: ["Galaxy-de-Lea", "iPhone-7"] }, "Léa");
  assert.equal(t.title, "Lequel est votre téléphone ?");
  assert.equal(t.intro, "Bonjour Léa. Ostal reconnaît les téléphones vus sur le Wi-Fi de la maison. Choisissez le vôtre.");
  assert.equal(t.hasFree, true);
  assert.equal(t.later, "Plus tard : je n'ai pas mon téléphone ici");
  assert.match(t.privacy, /jamais son adresse/);
  assert.equal(chooserTexts({ free: [] }).hasFree, false);
  assert.match(chooserTexts({ free: [] }).intro, /^Bonjour\. /);
});

test("la liste garde le téléphone déjà choisi, sans doublon", () => {
  assert.deepEqual(listWithMine({ mine: "Galaxy-de-Lea", free: ["iPhone-7"] }), ["Galaxy-de-Lea", "iPhone-7"]);
  assert.deepEqual(listWithMine({ mine: "galaxy-de-lea", free: ["Galaxy-de-Lea"] }), ["Galaxy-de-Lea"]);
  assert.deepEqual(listWithMine({ mine: null, free: ["a"] }), ["a"]);
  assert.deepEqual(listWithMine(null), []);
});

test("les refus du serveur ont leur phrase, et une vieille route ne fait pas de bruit", () => {
  assert.match(chooseError({ status: 409 }), /déjà un propriétaire/);
  assert.match(chooseError({ status: 404 }), /plus vu sur le Wi-Fi/);
  assert.match(chooseError({ status: 503 }), /plus relié à la box/);
  assert.match(chooseError(new Error("x")), /Réessayez/);
  assert.equal(isMissingRoute({ status: 404 }), true);
  assert.equal(isMissingRoute({ status: 500 }), false);
  assert.equal(initial("léa"), "L");
});
