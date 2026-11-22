// node --test crates/server/web/widgets.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { ANDROID_NOTE, SHORTCUT_NAMES, copyLater, copyText, isAndroid, myTokens, originInfo, prepareShortcuts, prepareWidget, sinceText, tokenHere } from "./widgets.js";

test("seule l'adresse en .ts.net convient ; sinon, un conseil", () => {
  const ts = originInfo({ origin: "https://mac.tailnet-1234.ts.net" });
  assert.equal(ts.usable, true);
  assert.equal(ts.warning, "");
  for (const origin of ["https://192.168.1.20:8443", "https://mac.tailnet-1234.ts.net:8443", "http://mac.ts.net", "null"]) {
    const info = originInfo({ origin });
    assert.equal(info.usable, false, origin);
    assert.match(info.warning, /\.ts\.net/);
  }
  assert.equal(originInfo(undefined).origin, "");
});

test("les noms des raccourcis sont sans accent et les mêmes dans le widget", () => {
  assert.deepEqual(SHORTCUT_NAMES, ["Je pars", "Je rentre", "Mode nuit", "Mode vacances"]);
  for (const name of SHORTCUT_NAMES) assert.match(name, /^[\x20-\x7e]+$/);
  const widget = readFileSync(new URL("./raccourcis/ostal.js", import.meta.url), "utf8");
  assert.match(widget, /const RACCOURCI_PART = "Je pars";/);
  assert.match(widget, /const RACCOURCI_RENTRE = "Je rentre";/);
  // Le widget n'a que deux boutons : pas de raccourci Nuit ni Vacances dans son script.
  assert.doesNotMatch(widget, /Mode nuit|Mode vacances/);
});

test("préparation des raccourcis : la page envoie son adresse et vérifie la réponse", async () => {
  const calls = [];
  const good = async (method, path, body) => {
    calls.push([method, path, body]);
    return {
      expires_in_s: 300,
      shortcuts: [
        { name: "Je pars", url: "/app/raccourcis/perso/abc/Je%20pars.shortcut" },
        { name: "Je rentre", url: "/app/raccourcis/perso/abc/Je%20rentre.shortcut" },
        { name: "Mode nuit", url: "/app/raccourcis/perso/abc/Mode%20nuit.shortcut" },
        { name: "Mode vacances", url: "/app/raccourcis/perso/abc/Mode%20vacances.shortcut" },
      ],
    };
  };
  const r = await prepareShortcuts(good, "https://mac.ts.net");
  assert.deepEqual(calls, [["POST", "/api/tokens/mode/shortcuts", { origin: "https://mac.ts.net" }]]);
  assert.equal(r.shortcuts.length, 4);
  assert.equal(r.expiresInS, 300);
  // Un serveur d'avant (Mode absent, Mode present…) : refus, il faut le mettre à jour.
  await assert.rejects(
    prepareShortcuts(async () => ({ shortcuts: ["Mode absent", "Mode present", "Mode nuit", "Mode vacances"].map((name) => ({ name, url: "/app/raccourcis/perso/a/b" })) }), "x"),
    /mettez-le à jour/,
  );
  // Le nom du téléphone, s'il est donné, accompagne l'adresse.
  calls.length = 0;
  await prepareShortcuts(good, "https://mac.ts.net", "iPhone de Léa");
  assert.deepEqual(calls, [["POST", "/api/tokens/mode/shortcuts", { origin: "https://mac.ts.net", label: "iPhone de Léa" }]]);
  // Une réponse d'un autre serveur, ou un lien ailleurs : refus.
  await assert.rejects(prepareShortcuts(async () => ({}), "x"), /mettez-le à jour/);
  await assert.rejects(
    prepareShortcuts(async () => ({ shortcuts: [{ name: "Je pars", url: "https://ailleurs/x" }, { name: "Je rentre", url: "/app/raccourcis/perso/a/b" }, { name: "Mode nuit", url: "/app/raccourcis/perso/a/c" }, { name: "Mode vacances", url: "/app/raccourcis/perso/a/d" }] }), "x"),
    /mettez-le à jour/,
  );
  // Un serveur d'avant (deux raccourcis seulement) : refus, il faut le mettre à jour.
  await assert.rejects(
    prepareShortcuts(async () => ({ shortcuts: [{ name: "Je pars", url: "/app/raccourcis/perso/a/b" }, { name: "Je rentre", url: "/app/raccourcis/perso/a/c" }] }), "x"),
    /mettez-le à jour/,
  );
  // L'erreur de l'API (signature impossible…) remonte telle quelle.
  await assert.rejects(prepareShortcuts(async () => { throw new Error("signature impossible"); }, "x"), /signature impossible/);
});

test("préparation du widget", async () => {
  const script = "// Variables used by Scriptable.\nconst TOKEN = \"abc\";";
  let seen = null;
  assert.equal(await prepareWidget(async (m, p, b) => { seen = [m, p, b]; return { script }; }, "https://mac.ts.net"), script);
  assert.deepEqual(seen, ["POST", "/api/tokens/read/widget", { origin: "https://mac.ts.net" }]);
  await prepareWidget(async (m, p, b) => { seen = [m, p, b]; return { script }; }, "https://mac.ts.net", "iPad");
  assert.deepEqual(seen[2], { origin: "https://mac.ts.net", label: "iPad" });
  await assert.rejects(prepareWidget(async () => ({ script: "<html>" }), "x"), /illisible/);
  await assert.rejects(prepareWidget(async () => ({ script: "Scriptable __JETON_DE_LECTURE__" }), "x"), /illisible/);
});

test("mes jetons : seulement les miens, et celui de ce téléphone", () => {
  const tokens = {
    label: "iPhone de Léa",
    items: [
      { id: "a", kind: "mode", label: "iPhone de Léa", created: 1 },
      { id: "b", kind: "mode", label: "iPad", created: 2 },
      { id: "c", kind: "read", label: "iPad", created: 3 },
    ],
  };
  assert.deepEqual(myTokens(tokens, "mode").map((t) => t.id), ["a", "b"]);
  assert.deepEqual(myTokens(tokens, "read").map((t) => t.id), ["c"]);
  assert.equal(tokenHere(tokens, "mode").id, "a");
  assert.equal(tokenHere(tokens, "read"), null);
  assert.equal(tokenHere({ ...tokens, label: " IPHONE de léa " }, "mode").id, "a");
  // Un serveur d'avant, sans liste : rien, sans planter.
  assert.deepEqual(myTokens({ mode: { exists: true } }, "mode"), []);
  assert.equal(tokenHere(null, "mode"), null);
});

test("sur Android, une note remplace les raccourcis et le widget", () => {
  assert.equal(isAndroid("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Chrome/120 Mobile Safari/537.36"), true);
  assert.equal(isAndroid("Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 Safari/604.1"), false);
  assert.equal(isAndroid(undefined), false);
  assert.equal(ANDROID_NOTE, "Pas de raccourcis : la présence par le Wi-Fi suffit.");
});

test("date d'un jeton", () => {
  assert.equal(sinceText(null), "");
  assert.equal(sinceText(Number.NaN), "");
  assert.match(sinceText(1_763_600_000), /^depuis le \d{2}\/\d{2}\/20\d{2}$/);
});

test("copie d'un texte à venir : ClipboardItem pendant le geste, sinon copie ordinaire", async () => {
  class Item { constructor(parts) { this.parts = parts; } }
  let written = null;
  const write = async (items) => { written = await items[0].parts["text/plain"]; };
  const r = await copyLater(Promise.resolve("script"), { write, ClipboardItem: Item });
  assert.deepEqual(r, { result: "copied", text: "script" });
  assert.equal(await written.text(), "script");
  // Pas de ClipboardItem : copie ordinaire une fois le texte reçu.
  const seen = [];
  const plain = await copyLater(Promise.resolve("s2"), { writeText: async (t) => { seen.push(t); } });
  assert.deepEqual([plain, seen], [{ result: "copied", text: "s2" }, ["s2"]]);
  // Tout est refusé : à la main, avec le texte.
  const refused = await copyLater(Promise.resolve("s3"), { write: async () => { throw new Error("non"); }, ClipboardItem: Item });
  assert.deepEqual(refused, { result: "manual", text: "s3" });
  // L'erreur du serveur remonte.
  await assert.rejects(copyLater(Promise.reject(new Error("jeton")), { write, ClipboardItem: Item }), /jeton/);
});

test("copie : presse-papiers, puis repli, puis à la main", async () => {
  const seen = [];
  assert.equal(await copyText("x", { writeText: async (t) => { seen.push(t); } }), "copied");
  assert.deepEqual(seen, ["x"]);
  // Le presse-papiers refuse : le repli prend le relais.
  const refuse = async () => { throw new Error("NotAllowedError"); };
  assert.equal(await copyText("y", { writeText: refuse, execCopy: () => true }), "copied");
  // Le repli échoue ou lève : à la main.
  assert.equal(await copyText("y", { writeText: refuse, execCopy: () => false }), "manual");
  assert.equal(await copyText("y", { execCopy: () => { throw new Error("non"); } }), "manual");
  assert.equal(await copyText("y"), "manual");
});
