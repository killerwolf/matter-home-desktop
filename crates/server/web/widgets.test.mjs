// node --test crates/server/web/widgets.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";

import { SCRIPT_PATH, SHORTCUTS, copyText, fetchScript, originInfo } from "./widgets.js";

test("l'adresse affichée est location.origin ; sur le port 8443, un conseil", () => {
  const ts = originInfo({ origin: "https://mac.tailnet-1234.ts.net", port: "" });
  assert.equal(ts.origin, "https://mac.tailnet-1234.ts.net");
  assert.equal(ts.usable, true);
  assert.equal(ts.direct, false);
  assert.equal(ts.warning, "");
  const direct = originInfo({ origin: "https://192.168.1.20:8443", port: "8443" });
  assert.equal(direct.direct, true);
  assert.match(direct.warning, /\.ts\.net/);
  assert.match(direct.warning, /certificat/);
  // Une page ouverte depuis un fichier : pas d'adresse à copier.
  assert.equal(originInfo({ origin: "null", port: "" }).usable, false);
  assert.equal(originInfo(undefined).origin, "");
});

test("les raccourcis gardent leur nom exact et un chemin relatif sans accent", () => {
  assert.deepEqual(SHORTCUTS.map((s) => s.label), ["Mode absent", "Mode présent"]);
  for (const s of SHORTCUTS) {
    assert.match(s.href, /^raccourcis\/[a-z-]+\.shortcut$/);
  }
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

test("lecture du script", async () => {
  const ok = async (path, options) => {
    assert.equal(path, SCRIPT_PATH);
    assert.equal(options.cache, "no-store");
    return { ok: true, text: async () => "// Variables used by Scriptable.\nconst X = 1;" };
  };
  assert.match(await fetchScript(ok), /Scriptable/);
  await assert.rejects(fetchScript(async () => ({ ok: false })), /mettez le serveur à jour/);
  await assert.rejects(fetchScript(async () => ({ ok: true, text: async () => "<html>" })), /illisible/);
});
