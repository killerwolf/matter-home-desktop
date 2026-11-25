// Notifications web push (ticket n° 17) : s'abonner depuis l'app web installée sur l'écran
// d'accueil, se désabonner, et ce qu'il faut dire à l'utilisateur quand ce n'est pas possible.
//
// Tout ce qui touche au navigateur (Notification, pushManager, requêtes) est passé en paramètre :
// les tests tournent sous node (`node --test crates/server/web/push.test.mjs`).
//
// Règles :
// - `Notification.requestPermission()` n'est appelée qu'au toucher d'un bouton, et c'est la
//   **première** attente du gestionnaire (Safari refuse sinon : il faut un geste de l'utilisateur) ;
//   la clé publique du serveur est donc chargée avant (à l'ouverture des réglages) ;
// - un abonnement fait avec une autre clé (clé du serveur recréée) est remplacé ;
// - le serveur garde l'abonnement par appareil ; le même abonnement renvoyé garde le même `id`.

export const SUBSCRIPTIONS_PATH = "/api/push/subscriptions";

/** base64url (avec ou sans `=`) → octets. */
export function urlBase64ToUint8Array(text) {
  const padded = text.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((text.length + 3) % 4);
  const raw = atob(padded);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
  return out;
}

/** octets (ArrayBuffer ou vue) → base64url sans `=`. */
export function bytesToUrlBase64(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  let raw = "";
  for (const b of bytes) raw += String.fromCharCode(b);
  return btoa(raw).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** La version d'iOS ou d'iPadOS d'après le User-Agent (`[16, 4]`), `null` si ce n'en est pas un.
 *  Un iPad récent se présente comme un Mac : `touch` le trahit. */
export function iosVersion(ua, touch = false) {
  const m = /(?:iPhone|iPad|iPod).*? OS (\d+)_(\d+)/.exec(ua);
  if (m) return [Number(m[1]), Number(m[2])];
  if (/Macintosh/.test(ua) && touch) {
    const v = /Version\/(\d+)\.(\d+)/.exec(ua);
    return v ? [Number(v[1]), Number(v[2])] : [0, 0];
  }
  return null;
}

const INSTALL_LATER_KEY = "ostal.install.later";
const NOTIFICATIONS_DISMISSED_KEY = "ostal.install.notifications-dismissed";
/** « Plus tard » sur la carte d'installation : elle revient au bout d'une semaine. */
export const INSTALL_LATER_MS = 7 * 24 * 3600 * 1000;

/** La carte « Installer sur l'iPhone » n'a de sens que sur un iPhone ou un iPad (raccourcis et widget
 *  n'existent pas ailleurs) : pas sur Android, pas sur un ordinateur. */
export const isApplePhone = (ua, touch = false) => iosVersion(ua, touch) !== null;

/** Faut-il montrer la carte ? Sur iPhone ou iPad, et pas si « Plus tard » date de moins d'une semaine. */
export function showInstallCard(ua, touch, laterAt, now = Date.now()) {
  if (!isApplePhone(ua, touch)) return false;
  return !(laterAt && now - laterAt < INSTALL_LATER_MS);
}

export function readInstallLater(storage = globalThis.localStorage) {
  try {
    const n = Number(storage?.getItem(INSTALL_LATER_KEY));
    return Number.isFinite(n) && n > 0 ? n : 0;
  } catch { return 0; }
}

export function writeInstallLater(now = Date.now(), storage = globalThis.localStorage) {
  try { storage?.setItem(INSTALL_LATER_KEY, String(now)); } catch { /* pas gardé : la carte reviendra */ }
}

/** L'étape facultative des notifications a été écartée avec « Plus tard » : définitif, la carte ne
 *  revient pas chaque semaine pour une seule étape facultative. */
export function readNotificationsDismissed(storage = globalThis.localStorage) {
  try { return storage?.getItem(NOTIFICATIONS_DISMISSED_KEY) === "1"; } catch { return false; }
}

export function writeNotificationsDismissed(storage = globalThis.localStorage) {
  try { storage?.setItem(NOTIFICATIONS_DISMISSED_KEY, "1"); } catch { /* pas gardé : la carte reviendra */ }
}

/** Un nom pour l'appareil (modifiable sur le serveur). */
export function deviceLabel(ua, touch = false) {
  if (/iPhone/.test(ua)) return "iPhone";
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && touch)) return "iPad";
  if (/Android/.test(ua)) return "Android";
  if (/Macintosh/.test(ua)) return "Mac";
  if (/Windows/.test(ua)) return "PC Windows";
  return "Appareil";
}

/**
 * Peut-on s'abonner ici ? `{ ok: true }` ou `{ ok: false, reason, text }` avec le conseil à afficher.
 * `env` : { ua, touch, standalone, hasNotification, hasPushManager, hasServiceWorker, permission }.
 */
export function pushSupport(env) {
  const ios = iosVersion(env.ua ?? "", env.touch);
  if (ios && (ios[0] < 16 || (ios[0] === 16 && ios[1] < 4))) {
    return { ok: false, reason: "ios-too-old", text: "Les notifications demandent iOS 16.4 ou plus récent. Mettez l'iPhone ou l'iPad à jour." };
  }
  if (ios && !env.standalone) {
    return {
      ok: false, reason: "ios-not-installed",
      text: "Ajoutez d'abord Ostal à l'écran d'accueil : sur iPhone et iPad, les notifications ne marchent que depuis l'app installée.",
      steps: ["Touchez Partager.", "Touchez « Sur l'écran d'accueil ».", "Touchez « Ajouter », puis ouvrez Ostal depuis l'écran d'accueil."],
    };
  }
  if (!env.hasServiceWorker || !env.hasPushManager || !env.hasNotification) {
    return { ok: false, reason: "unsupported", text: "Ce navigateur ne sait pas recevoir de notifications (ou la page n'est pas servie en HTTPS avec un certificat reconnu : utilisez l'adresse .ts.net ou l'adresse publique)." };
  }
  if (env.permission === "denied") {
    return { ok: false, reason: "denied", text: "Les notifications sont refusées pour cette app. Réautorisez-les dans Réglages › Notifications › Ostal, puis revenez ici." };
  }
  return { ok: true };
}

/** L'abonnement a-t-il été fait avec cette clé du serveur ? */
export function sameKey(subscription, publicKey) {
  const key = subscription?.options?.applicationServerKey;
  if (!key) return true; // inconnu (ancien navigateur) : on le garde
  return bytesToUrlBase64(key) === publicKey.replace(/=+$/, "");
}

/** Le corps de `POST /api/push/subscriptions`. */
export function subscriptionBody(subscription, label, categories) {
  const json = subscription.toJSON();
  const body = { endpoint: json.endpoint, keys: { p256dh: json.keys.p256dh, auth: json.keys.auth }, label };
  if (categories) body.categories = categories;
  return body;
}

/**
 * Abonne cet appareil. À appeler **directement** depuis le gestionnaire du toucher.
 * deps : { Notification, registration, request(method, path, body), publicKey, label }.
 * Rend `{ status: "subscribed", view }` ou `{ status: "denied" }`.
 */
export async function subscribe(deps) {
  const permission = await deps.Notification.requestPermission();
  if (permission !== "granted") return { status: "denied" };
  const manager = deps.registration.pushManager;
  let subscription = await manager.getSubscription();
  if (subscription && !sameKey(subscription, deps.publicKey)) {
    await subscription.unsubscribe();
    subscription = null;
  }
  if (!subscription) {
    subscription = await manager.subscribe({ userVisibleOnly: true, applicationServerKey: urlBase64ToUint8Array(deps.publicKey) });
  }
  const view = await deps.request("POST", SUBSCRIPTIONS_PATH, subscriptionBody(subscription, deps.label));
  return { status: "subscribed", view };
}

/**
 * L'abonnement de cet appareil tel que le serveur le connaît, sans rien demander à l'utilisateur :
 * renvoyé au serveur s'il existe (il le retrouve, ou le reprend s'il l'avait perdu). `null` sinon.
 */
export async function currentSubscription(deps) {
  const subscription = await deps.registration.pushManager.getSubscription();
  if (!subscription || !sameKey(subscription, deps.publicKey)) return null;
  return deps.request("POST", SUBSCRIPTIONS_PATH, subscriptionBody(subscription, deps.label));
}

/**
 * Au démarrage de l'app : si les notifications sont autorisées, l'abonnement du navigateur est
 * renvoyé au serveur (il a pu changer : le service de push le renouvelle, et le service worker se
 * réabonne seul, `pushsubscriptionchange`, sans pouvoir prévenir le serveur faute de jeton).
 * `loadDeps` n'est appelé que dans ce cas ; `null` : rien à faire. Jamais de demande à l'utilisateur.
 */
export async function syncOnStart(permission, loadDeps) {
  if (permission !== "granted") return null;
  const deps = await loadDeps();
  if (!deps) return null;
  return currentSubscription(deps);
}

/** Désabonne cet appareil : sur le serveur d'abord, puis dans le navigateur. */
export async function unsubscribe(deps, id) {
  if (id) await deps.request("DELETE", `${SUBSCRIPTIONS_PATH}/${encodeURIComponent(id)}`);
  const subscription = await deps.registration.pushManager.getSubscription();
  if (subscription) await subscription.unsubscribe();
}

/** La page à ouvrir au toucher d'une notification : même origine, sous `/app/`, sinon l'accueil.
 *  (Même règle dans sw.js, qui ne peut pas importer ce module.) */
export function clickTarget(url, origin) {
  try {
    const target = new URL(url ?? "/app/", origin);
    if (target.origin === origin && target.pathname.startsWith("/app/")) return target.href;
  } catch { /* adresse illisible */ }
  return new URL("/app/", origin).href;
}

/** `#evenement=<id>` → l'identifiant de l'événement à ouvrir, sinon `null`. */
export function eventFromHash(hash) {
  const m = /^#evenement=([0-9a-z-]{1,64})$/.exec(hash ?? "");
  return m ? m[1] : null;
}

/** Une date courte : « 18/11/2026 ». */
export const dateText = (at) => (at ? new Date(at * 1000).toLocaleDateString("fr-FR") : "");

/** Les catégories, dans l'ordre d'affichage. */
export const CATEGORIES = [
  { id: "rules", label: "Règles « Notifier »" },
  { id: "detections", label: "Détections caméra (Surveillance)" },
  { id: "battery", label: "Pile faible (moins de 15 %)" },
  { id: "unreachable", label: "Appareil injoignable (plus de 10 min)" },
  { id: "agent_requests", label: "Demandes d'un agent d'IA à valider" },
];
