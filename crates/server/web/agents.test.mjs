// node --test crates/server/web/agents.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";

import { DURATIONS, SCOPE_CHOICES, agentError, agentState, connectionSnippets, createBody, decisionError, deviceChoices, emptyForm, expiryText, formError, isMissingRoute, refreshDelay, remainingText, requestCards, rulesText, scopeWords, titleOf, usageText } from "./agents.js";

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

// ---- « Mes agents » ----

const AID = "f".repeat(32);
const listing = (extra = {}) => ({
  enabled: true, default_days: 90, max_days: 365, max_devices: 20, never: ["5"], sensitive: ["7", "pas-un-id"],
  can: { control: true, mode: false },
  agents: [
    { id: AID, label: "Claude Code", scopes: ["read", "control", "teleportation"], devices: ["7", "0x1", "8"], created: 100, expires: 1000, expired: false, last_used: null, calls_today: 0 },
    { id: "pas-un-id", label: "Louche", scopes: ["read"], devices: [], created: 300, expires: 900 },
    { id: "a".repeat(32), label: "x".repeat(500), scopes: ["read"], devices: [], created: 200, expires: 10, expired: true, last_used: 150, calls_today: 12 },
  ],
  ...extra,
});

test("la liste des agents : seulement des agents bien formés, les plus récents d'abord, rien d'inconnu", () => {
  const s = agentState(listing());
  assert.deepEqual(s.agents.map((a) => a.label.length > 50 ? "long" : a.label), ["long", "Claude Code"]);
  assert.equal(s.agents[0].label.length, 300);
  assert.deepEqual(s.agents[1].devices, ["7", "8"]); // « 0x1 » n'est pas un identifiant décimal
  assert.deepEqual(s.sensitive, ["7"]);
  assert.deepEqual(s.never, ["5"]);
  assert.equal(s.canControl, true);
  assert.equal(s.canMode, false);
  assert.equal(s.enabled, true);
  assert.equal(s.agents[0].expired, true);
  assert.deepEqual(agentState(null).agents, []);
  assert.deepEqual(agentState({ agents: "n'importe quoi" }).agents, []);
  assert.equal(agentState({}).enabled, false);
  assert.equal(agentState({}).canControl, false);
});

test("les droits d'un agent en mots, sans rien d'inconnu", () => {
  assert.equal(scopeWords(["read", "occupation", "control", "mode"]), "Lecture, Présence et mode, Commandes, Changer le mode");
  assert.equal(scopeWords(["read", "teleportation", 3, null]), "Lecture");
  assert.equal(scopeWords(undefined), "");
});

test("expiration et usage : phrases honnêtes", () => {
  assert.equal(expiryText(1000 + 29 * 86_400 - 5, 1000 - 5), "Expire dans 29 jours");
  assert.equal(expiryText(1000 + 3600, 1000), "Expire aujourd'hui ou demain");
  assert.equal(expiryText(1000, 1000), "Expiré");
  assert.equal(expiryText(undefined, 0), "Expiré");
  const base = { lastUsed: null, callsToday: 0 };
  assert.equal(usageText(base), "Pas encore utilisé depuis le démarrage du serveur");
  assert.equal(usageText({ lastUsed: 990, callsToday: 1 }, 1000), "Dernier appel à l'instant, 1 appel aujourd'hui");
  assert.equal(usageText({ lastUsed: 1000 - 600, callsToday: 12 }, 1000), "Dernier appel il y a 10 min, 12 appels aujourd'hui");
  assert.equal(usageText({ lastUsed: 1000 - 7200, callsToday: 2 }, 1000), "Dernier appel il y a 2 h, 2 appels aujourd'hui");
  assert.equal(usageText({ lastUsed: 0, callsToday: 2 }, 3 * 86_400), "Dernier appel il y a 3 j, 2 appels aujourd'hui");
});

const dev = (id, name, caps) => ({ id, info: { name, caps } });

test("les appareils proposés : prises, lampes et volets, sans la liste « jamais », sensibles marqués", () => {
  const all = [
    dev("1", "Prise du salon", { on_off: true }),
    dev("2", "Volet du salon", { cover: true, on_off: true }),
    dev("3", "Capteur de porte", { contact: true }),
    dev("5", "Congélateur", { on_off: true }),
    dev("7", "Porte de garage", { cover: true }),
    dev("9", "Ampoule", { on_off: true }),
  ];
  const { choices, hidden } = deviceChoices(all, ["5"], ["7"]);
  assert.deepEqual(choices.map((c) => [c.id, c.kind, c.sensitive]), [["9", "prise", false], ["7", "volet", true], ["1", "prise", false], ["2", "volet", false]]);
  assert.equal(hidden, 1);
  assert.deepEqual(deviceChoices([null, {}, dev("1", "", { on_off: true })], [], []).choices.map((c) => c.name), ["Appareil 1"]);
});

test("le formulaire : ce qui manque, en une phrase", () => {
  const state = { canControl: true, canMode: true, maxDevices: 20 };
  const form = (extra) => ({ ...emptyForm(), label: "Claude Code", ...extra });
  assert.equal(formError(form({}), state), "");
  assert.match(formError(form({ label: "   " }), state), /Donnez un nom/);
  assert.match(formError(form({ label: "x".repeat(49) }), state), /48 caractères/);
  assert.match(formError(form({ control: true }), state), /au moins un appareil/);
  assert.equal(formError(form({ control: true, devices: new Set(["1"]) }), state), "");
  assert.match(formError(form({ control: true, devices: new Set(["1"]) }), { ...state, canControl: false }), /pas le droit de commander/);
  assert.match(formError(form({ mode: true }), { ...state, canMode: false }), /pas le droit de changer le mode/);
  const many = new Set(Array.from({ length: 21 }, (_, i) => String(i + 1)));
  assert.match(formError(form({ control: true, devices: many }), state), /20 appareils au plus/);
});

test("le corps de la création : lecture toujours, « mode » suppose « occupation », appareils seulement avec les commandes", () => {
  const f = (extra) => ({ ...emptyForm(30), label: "  Cursor  ", ...extra });
  assert.deepEqual(createBody(f({})), { label: "Cursor", days: 30, scopes: ["read"], devices: [] });
  assert.deepEqual(createBody(f({ mode: true })).scopes, ["read", "occupation", "mode"]);
  assert.deepEqual(createBody(f({ occupation: true, control: true, devices: new Set(["13", "2"]) })), {
    label: "Cursor", days: 30, scopes: ["read", "occupation", "control"], devices: ["2", "13"],
  });
  // Des appareils cochés puis « commandes » décoché : ils ne partent pas.
  assert.deepEqual(createBody(f({ control: false, devices: new Set(["1"]) })).devices, []);
  assert.deepEqual(SCOPE_CHOICES.map((c) => c.key), ["occupation", "control", "mode"]);
  assert.deepEqual(DURATIONS.map((d) => d.days), [7, 30, 90, 180, 365]);
});

test("les erreurs de création et de révocation", () => {
  assert.match(agentError({ status: 0 }), /ne répond pas/);
  assert.match(agentError({ status: 404 }), /met(tez)?-?le à jour/);
  assert.equal(agentError({ status: 400, detail: "cette personne a déjà un agent « A » : choisissez un autre libellé" }), "Cette personne a déjà un agent « A » : choisissez un autre libellé.");
  assert.equal(agentError({ status: 403, detail: "ce compte n'a pas le droit de commander la maison" }), "ce compte n'a pas le droit de commander la maison");
  assert.equal(agentError({ status: 500 }), "Impossible pour l'instant, réessayez.");
  assert.equal(agentError(undefined), "Impossible pour l'instant, réessayez.");
});

test("les lignes à copier ne contiennent jamais un jeton, ni l'option --header", () => {
  const s = connectionSnippets("https://maison.exemple.ts.net/");
  const all = Object.values(s).join("\n");
  assert.ok(!all.includes("ostal_agent_"));
  assert.ok(!all.includes("--header"));
  assert.match(s.keychain, /^security add-generic-password .* -w$/); // -w en dernier : la commande demande le jeton au clavier
  assert.ok(!/-w\s+\S/.test(s.keychain));
  assert.equal(s.env, 'export OSTAL_AGENT_TOKEN="$(security find-generic-password -s ostal-agent -w)"');
  const claude = JSON.parse(s.claudeCode).mcpServers.ostal;
  assert.equal(claude.url, "https://maison.exemple.ts.net/mcp");
  assert.equal(claude.type, "http");
  assert.equal(claude.headers.Authorization, "Bearer ${OSTAL_AGENT_TOKEN}");
  const cursor = JSON.parse(s.cursor).mcpServers.ostal;
  assert.equal(cursor.url, "https://maison.exemple.ts.net/mcp");
  assert.equal(cursor.headers.Authorization, "Bearer ${env:OSTAL_AGENT_TOKEN}");
});
