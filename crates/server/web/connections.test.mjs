// À lancer à la main : node --test crates/server/web/connections.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import {
  deviceClass, defaultName, installationId, describeInstallation, kindLabel, viaLabel,
  lastSeenLabel, sortConnections, newId,
} from "./connections.js";

const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148 Safari/604.1";
const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/17.5 Safari/605.1.15";
const ANDROID = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Chrome/126.0 Mobile Safari/537.36";

test("l'appareil se lit dans le navigateur, un iPad en mode Mac aussi", () => {
  assert.equal(deviceClass(IPHONE, 5), "iphone");
  assert.equal(deviceClass(ANDROID, 5), "android");
  assert.equal(deviceClass(MAC, 0), "computer");
  assert.equal(deviceClass(MAC, 5), "ipad", "iPadOS se dit Macintosh mais a des points de contact");
  assert.equal(deviceClass("", 0), "computer");
  assert.equal(defaultName("ipad"), "iPad");
  assert.equal(defaultName("computer"), "Ordinateur");
});

function memoryStorage(initial = {}) {
  const data = { ...initial };
  return { getItem: (k) => data[k] ?? null, setItem: (k, v) => { data[k] = String(v); }, data };
}

test("l'identifiant est créé une fois, gardé, et refait si le stockage est abîmé", () => {
  const storage = memoryStorage();
  const first = installationId(storage);
  assert.match(first, /^[0-9a-f]{32}$/);
  assert.equal(installationId(storage), first);
  const bad = memoryStorage({ "ostal.installation": "x" });
  assert.notEqual(installationId(bad), "x");
  // Stockage qui lève (fenêtre privée) : un identifiant quand même.
  const throwing = { getItem() { throw new Error("refusé"); }, setItem() { throw new Error("refusé"); } };
  assert.match(installationId(throwing), /^[0-9a-f]{32}$/);
  assert.equal(newId(null).length, 32);
});

test("le login décrit cette app", () => {
  const out = describeInstallation({ navigator: { userAgent: IPHONE, maxTouchPoints: 5 }, storage: memoryStorage() });
  assert.deepEqual({ ...out, id: "x" }, { id: "x", kind: "web", device: "iphone", name: "iPhone" });
});

test("les mots des connexions suivent les maquettes", () => {
  assert.equal(kindLabel({ kind: "web", device: "iphone" }), "App web · iPhone");
  assert.equal(kindLabel({ kind: "mac", device: "computer" }), "App Mac · ordinateur");
  assert.equal(kindLabel({ unknown: true, kind: "other", device: "other" }), "App pas encore à jour");
  assert.equal(viaLabel("local"), "Sur place");
  assert.equal(viaLabel("tailscale"), "À distance (Tailscale)");
});

test("le dernier accès : maintenant, aujourd'hui, hier, sinon la date", () => {
  const now = new Date(2026, 10, 22, 18, 41);
  const at = (d, h, m) => new Date(2026, 10, d, h, m).getTime() / 1000;
  assert.equal(lastSeenLabel(now.getTime() / 1000 - 30, now), "maintenant");
  assert.equal(lastSeenLabel(at(22, 18, 31), now), "aujourd'hui, 18:31");
  assert.equal(lastSeenLabel(at(21, 22, 10), now), "hier, 22:10");
  assert.equal(lastSeenLabel(at(12, 21, 4), now), "12/11, 21:04");
});

test("cet appareil d'abord, puis le plus récent", () => {
  const out = sortConnections([
    { id: "a", current: false, last_seen: 5 },
    { id: "b", current: false, last_seen: 9 },
    { id: "c", current: true, last_seen: 1 },
  ]);
  assert.deepEqual(out.map((c) => c.id), ["c", "b", "a"]);
});
