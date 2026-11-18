// node --test crates/server/web/push.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  bytesToUrlBase64, clickTarget, currentSubscription, deviceLabel, eventFromHash, iosVersion, pushSupport,
  sameKey, subscribe, subscriptionBody, syncOnStart, unsubscribe, urlBase64ToUint8Array,
} from "./push.js";

const IPHONE_17 = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const IPHONE_16_3 = "Mozilla/5.0 (iPhone; CPU iPhone OS 16_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.3 Mobile/15E148 Safari/604.1";
const IPAD_AS_MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15";
const KEY = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";

test("base64url dans les deux sens, avec ou sans =", () => {
  const bytes = urlBase64ToUint8Array(KEY);
  assert.equal(bytes.length, 65);
  assert.equal(bytes[0], 4);
  assert.equal(bytesToUrlBase64(bytes), KEY);
  assert.deepEqual([...urlBase64ToUint8Array("BTBZMqHH6r4Tts7J_aSIgg==")], [...urlBase64ToUint8Array("BTBZMqHH6r4Tts7J_aSIgg")]);
});

test("iOS : version, iPad déguisé en Mac, nom de l'appareil", () => {
  assert.deepEqual(iosVersion(IPHONE_17), [17, 5]);
  assert.deepEqual(iosVersion(IPAD_AS_MAC, true), [17, 2]);
  assert.equal(iosVersion(IPAD_AS_MAC, false), null);
  assert.equal(deviceLabel(IPHONE_17), "iPhone");
  assert.equal(deviceLabel(IPAD_AS_MAC, true), "iPad");
  assert.equal(deviceLabel(IPAD_AS_MAC, false), "Mac");
});

test("le conseil donné quand on ne peut pas s'abonner", () => {
  const full = { hasNotification: true, hasPushManager: true, hasServiceWorker: true, permission: "default" };
  assert.equal(pushSupport({ ...full, ua: IPHONE_16_3, standalone: true }).reason, "ios-too-old");
  const notInstalled = pushSupport({ ...full, ua: IPHONE_17, standalone: false });
  assert.equal(notInstalled.reason, "ios-not-installed");
  assert.match(notInstalled.text, /écran d'accueil/);
  assert.equal(pushSupport({ ...full, ua: IPHONE_17, standalone: true }).ok, true);
  assert.equal(pushSupport({ ...full, ua: IPHONE_17, standalone: true, hasPushManager: false }).reason, "unsupported");
  assert.equal(pushSupport({ ...full, ua: IPHONE_17, standalone: true, permission: "denied" }).reason, "denied");
});

/** Un faux navigateur : permission, pushManager et serveur enregistrent ce qu'on leur demande. */
function fakeBrowser({ permission = "granted", existing = null } = {}) {
  const calls = [];
  let current = existing;
  const make = (key) => ({
    options: { applicationServerKey: key },
    toJSON: () => ({ endpoint: "https://web.push.apple.com/abc", expirationTime: null, keys: { p256dh: KEY, auth: "BTBZMqHH6r4Tts7J_aSIgg" } }),
    unsubscribe: async () => { calls.push("unsubscribe"); current = null; return true; },
  });
  const deps = {
    Notification: { requestPermission: async () => { calls.push("permission"); return permission; } },
    registration: {
      pushManager: {
        getSubscription: async () => current,
        subscribe: async (options) => {
          calls.push(["subscribe", options.userVisibleOnly, options.applicationServerKey.length]);
          current = make(options.applicationServerKey.buffer);
          return current;
        },
      },
    },
    request: async (method, path, body) => { calls.push([method, path, body]); return { id: "0123456789abcdef", label: body?.label }; },
    publicKey: KEY,
    label: "iPhone",
  };
  return { deps, calls, make };
}

test("s'abonner : la permission d'abord, puis l'abonnement, puis le serveur", async () => {
  const { deps, calls } = fakeBrowser();
  const result = await subscribe(deps);
  assert.equal(result.status, "subscribed");
  assert.equal(result.view.id, "0123456789abcdef");
  assert.equal(calls[0], "permission");
  assert.deepEqual(calls[1], ["subscribe", true, 65]);
  const [method, path, body] = calls[2];
  assert.equal(method, "POST");
  assert.equal(path, "/api/push/subscriptions");
  assert.deepEqual(body, { endpoint: "https://web.push.apple.com/abc", keys: { p256dh: KEY, auth: "BTBZMqHH6r4Tts7J_aSIgg" }, label: "iPhone" });
});

test("permission refusée : rien n'est abonné ni envoyé", async () => {
  const { deps, calls } = fakeBrowser({ permission: "denied" });
  assert.deepEqual(await subscribe(deps), { status: "denied" });
  assert.deepEqual(calls, ["permission"]);
});

test("un abonnement fait avec une ancienne clé est remplacé, le bon est réutilisé", async () => {
  const old = fakeBrowser();
  old.deps.registration.pushManager.getSubscription = (() => {
    let sub = old.make(new Uint8Array(65).fill(9).buffer);
    const original = sub.unsubscribe;
    sub.unsubscribe = async () => { await original(); sub = null; return true; };
    return async () => sub;
  })();
  await subscribe(old.deps);
  assert.ok(old.calls.includes("unsubscribe"));
  assert.ok(old.calls.some((c) => Array.isArray(c) && c[0] === "subscribe"));

  const same = fakeBrowser();
  same.deps.registration.pushManager.getSubscription = async () => same.make(urlBase64ToUint8Array(KEY).buffer);
  await subscribe(same.deps);
  assert.ok(!same.calls.some((c) => Array.isArray(c) && c[0] === "subscribe"), "pas de nouvel abonnement");
  assert.ok(sameKey(same.make(urlBase64ToUint8Array(KEY).buffer), KEY));
});

test("l'abonnement actuel est retrouvé sans rien demander ; le désabonnement passe par le serveur", async () => {
  const none = fakeBrowser();
  assert.equal(await currentSubscription(none.deps), null);
  assert.deepEqual(none.calls, []);
  const { deps, calls, make } = fakeBrowser();
  const sub = make(urlBase64ToUint8Array(KEY).buffer);
  deps.registration.pushManager.getSubscription = async () => sub;
  const view = await currentSubscription(deps);
  assert.equal(view.id, "0123456789abcdef");
  assert.ok(!calls.includes("permission"), "aucune demande de permission");
  await unsubscribe(deps, view.id);
  assert.deepEqual(calls.slice(-2), [["DELETE", "/api/push/subscriptions/0123456789abcdef", undefined], "unsubscribe"]);
});

test("au démarrage, un abonnement renouvelé est renvoyé au serveur, seulement si c'est autorisé", async () => {
  let loaded = 0;
  const load = async () => { loaded += 1; return null; };
  assert.equal(await syncOnStart("default", load), null);
  assert.equal(await syncOnStart("denied", load), null);
  assert.equal(loaded, 0, "rien n'est chargé sans autorisation");
  assert.equal(await syncOnStart("granted", load), null);
  assert.equal(loaded, 1);
  // Le service worker s'est réabonné (nouvelle adresse) : elle part au serveur, sans permission.
  const { deps, calls, make } = fakeBrowser();
  const renewed = make(urlBase64ToUint8Array(KEY).buffer);
  deps.registration.pushManager.getSubscription = async () => renewed;
  const view = await syncOnStart("granted", async () => deps);
  assert.equal(view.id, "0123456789abcdef");
  assert.deepEqual(calls.map((c) => (Array.isArray(c) ? c.slice(0, 2) : c)), [["POST", "/api/push/subscriptions"]]);
});

test("le corps de l'abonnement et les catégories", () => {
  const sub = { toJSON: () => ({ endpoint: "e", expirationTime: null, keys: { p256dh: "p", auth: "a" } }) };
  assert.deepEqual(subscriptionBody(sub, "iPad", { battery: false }), { endpoint: "e", keys: { p256dh: "p", auth: "a" }, label: "iPad", categories: { battery: false } });
});

test("le toucher d'une notification n'ouvre que l'app", () => {
  const origin = "https://maison.exemple.ts.net";
  assert.equal(clickTarget("/app/#evenement=17-ab", origin), `${origin}/app/#evenement=17-ab`);
  assert.equal(clickTarget("https://ailleurs.example/app/", origin), `${origin}/app/`);
  assert.equal(clickTarget("/api/state", origin), `${origin}/app/`);
  assert.equal(clickTarget(undefined, origin), `${origin}/app/`);
  assert.equal(eventFromHash("#evenement=1800000000-ab12"), "1800000000-ab12");
  assert.equal(eventFromHash("#evenement=../x"), null);
  assert.equal(eventFromHash(""), null);
});
