// node --test crates/server/web/widgets.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { SHORTCUT_NAMES, copyLater, copyText, originInfo, prepareShortcuts, prepareWidget, sinceText } from "./widgets.js";

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
  assert.deepEqual(SHORTCUT_NAMES, ["Mode absent", "Mode present"]);
  for (const name of SHORTCUT_NAMES) assert.match(name, /^[\x20-\x7e]+$/);
  const widget = readFileSync(new URL("./raccourcis/ostal.js", import.meta.url), "utf8");
  assert.match(widget, /const RACCOURCI_ABSENT = "Mode absent";/);
  assert.match(widget, /const RACCOURCI_PRESENT = "Mode present";/);
});

test("préparation des raccourcis : la page envoie son adresse et vérifie la réponse", async () => {
  const calls = [];
  const good = async (method, path, body) => {
    calls.push([method, path, body]);
    return {
      expires_in_s: 300,
      shortcuts: [
        { name: "Mode absent", url: "/app/raccourcis/perso/abc/Mode%20absent.shortcut" },
        { name: "Mode present", url: "/app/raccourcis/perso/abc/Mode%20present.shortcut" },
      ],
    };
  };
  const r = await prepareShortcuts(good, "https://mac.ts.net");
  assert.deepEqual(calls, [["POST", "/api/tokens/mode/shortcuts", { origin: "https://mac.ts.net" }]]);
  assert.equal(r.shortcuts.length, 2);
  assert.equal(r.expiresInS, 300);
  // Une réponse d'un autre serveur, ou un lien ailleurs : refus.
  await assert.rejects(prepareShortcuts(async () => ({}), "x"), /mettez-le à jour/);
  await assert.rejects(
    prepareShortcuts(async () => ({ shortcuts: [{ name: "Mode absent", url: "https://ailleurs/x" }, { name: "Mode present", url: "/app/raccourcis/perso/a/b" }] }), "x"),
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
  await assert.rejects(prepareWidget(async () => ({ script: "<html>" }), "x"), /illisible/);
  await assert.rejects(prepareWidget(async () => ({ script: "Scriptable __JETON_DE_LECTURE__" }), "x"), /illisible/);
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
