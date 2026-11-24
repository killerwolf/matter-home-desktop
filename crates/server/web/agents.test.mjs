// node --test crates/server/web/agents.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";

import { decisionError, isMissingRoute, refreshDelay, remainingText, requestCards, rulesText, titleOf } from "./agents.js";

const ID = "0123456789abcdef0123456789abcdef";
const card = (extra = {}) => ({ id: ID, agent: "Claude Code", text: "Mettre la maison en mode Vacances.", details: ["Le mode Vacances dit que la maison est vide."], rules: [], created: 100, expires: 400, ...extra });

test("le temps qui reste, en toutes lettres", () => {
  assert.equal(remainingText(400, 100), "5 min");
  assert.equal(remainingText(400, 148), "4 min 12 s");
  assert.equal(remainingText(400, 355), "45 s");
  assert.equal(remainingText(400, 399.2), "1 s");
  assert.equal(remainingText(400, 400), "expirée");
  assert.equal(remainingText(400, 500), "expirée");
  assert.equal(remainingText(undefined, 0), "expirée");
  assert.equal(remainingText("n'importe quoi", 0), "expirée");
});

test("les cartes : seulement des demandes bien formées et vivantes, les plus anciennes d'abord", () => {
  const response = {
    requests: [
      card({ id: "b".repeat(32), created: 300, expires: 700 }),
      card({ created: 200 }),
      card({ id: "pas-un-id" }),
      card({ id: "A".repeat(32) }),
      card({ id: "c".repeat(32), expires: 50 }),
      card({ id: "d".repeat(32), expires: "bientôt" }),
      null,
      "texte",
    ],
  };
  assert.deepEqual(requestCards(response, 100).map((c) => c.id), [ID, "b".repeat(32)]);
  assert.deepEqual(requestCards({}, 0), []);
  assert.deepEqual(requestCards(null, 0), []);
  assert.deepEqual(requestCards({ requests: "x" }, 0), []);
});

test("le texte venu du serveur est ramené à du texte court, jamais interprété", () => {
  const [c] = requestCards({ requests: [card({ agent: "<img src=x onerror=alert(1)>", text: "x".repeat(1000), details: ["ok", 3, null, "<b>"], rules: new Array(20).fill("r") })] }, 0);
  assert.equal(c.agent, "<img src=x onerror=alert(1)>", "gardé tel quel : l'app l'affiche avec textContent");
  assert.equal(c.text.length, 300);
  assert.deepEqual(c.details, ["ok", "<b>"]);
  assert.equal(c.rules.length, 8);
  const [anonymous] = requestCards({ requests: [card({ agent: 5 })] }, 0);
  assert.equal(anonymous.agent, "Un agent d'IA");
  assert.equal(titleOf(anonymous), "Un agent d'IA demande");
  assert.equal(titleOf({ agent: "Claude Code" }), "Claude Code demande");
});

test("les automatisations qui partiraient", () => {
  assert.equal(rulesText([]), "");
  assert.equal(rulesText(["Nuit"]), "Elle ferait partir l'automatisation « Nuit ».");
  assert.equal(rulesText(["A", "B"]), "Elle ferait partir 2 automatisations : « A », « B ».");
});

test("les erreurs de réponse", () => {
  assert.match(decisionError({ status: 404 }), /n'existe plus/);
  assert.match(decisionError({ status: 400 }), /n'est plus à valider/);
  assert.match(decisionError({ status: 403 }), /pas le droit/);
  assert.match(decisionError({ status: 0 }), /ne répond pas/);
  assert.match(decisionError({ status: 500 }), /Impossible/);
  assert.match(decisionError(undefined), /Impossible/);
});

test("un serveur plus ancien : pas de route, rien à montrer", () => {
  assert.equal(isMissingRoute({ status: 404 }), true);
  assert.equal(isMissingRoute({ status: 405 }), true);
  assert.equal(isMissingRoute({ status: 500 }), false);
  assert.equal(isMissingRoute(null), false);
});

test("le rythme de relecture", () => {
  assert.equal(refreshDelay([]), 20_000);
  assert.equal(refreshDelay([card()]), 5_000);
});
