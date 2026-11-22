// Cette app web est une connexion : un identifiant tiré au hasard à la première ouverture (gardé
// dans le stockage local) et un nom déduit du navigateur (« iPhone », « Android », « Ordinateur »),
// que le serveur reçoit au login. « Mes connexions » liste celles du compte (GET
// /api/me/installations) et en coupe une (DELETE). Logique pure ici, sans DOM, pour l'essayer à
// la main : node --test crates/server/web/connections.test.mjs

const ID_KEY = "ostal.installation";

/** Sorte d'appareil d'après le navigateur : « iphone », « ipad », « android » ou « computer ».
 *  Un iPad récent se dit « Macintosh » : seuls ses points de contact le distinguent d'un Mac. */
export function deviceClass(userAgent = "", maxTouchPoints = 0) {
  const ua = String(userAgent);
  if (/iPhone|iPod/.test(ua)) return "iphone";
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && maxTouchPoints > 1)) return "ipad";
  if (/Android/.test(ua)) return "android";
  return "computer";
}

/** Le nom proposé, que le serveur affichera : « iPhone », « iPad », « Android », « Ordinateur ». */
export function defaultName(device) {
  return { iphone: "iPhone", ipad: "iPad", android: "Android", computer: "Ordinateur" }[device] ?? "Navigateur";
}

/** Un identifiant neuf : 32 caractères hexadécimaux du générateur du navigateur. */
export function newId(cryptoApi = globalThis.crypto) {
  const bytes = new Uint8Array(16);
  if (cryptoApi?.getRandomValues) cryptoApi.getRandomValues(bytes);
  else for (let i = 0; i < bytes.length; i++) bytes[i] = Math.floor(Math.random() * 256);
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** L'identifiant de cette app : lu, ou créé et gardé. Sans stockage (fenêtre privée), un
 *  identifiant pour cette ouverture seulement. */
export function installationId(storage = globalThis.localStorage, cryptoApi = globalThis.crypto) {
  try {
    const saved = storage?.getItem(ID_KEY);
    if (saved && /^[0-9a-zA-Z-]{8,64}$/.test(saved)) return saved;
  } catch { /* stockage refusé */ }
  const id = newId(cryptoApi);
  try { storage?.setItem(ID_KEY, id); } catch { /* pas gardé : tant pis */ }
  return id;
}

/** Ce que le login envoie de cette app. */
export function describeInstallation(env = {}) {
  const nav = env.navigator ?? globalThis.navigator ?? {};
  const device = deviceClass(nav.userAgent, nav.maxTouchPoints ?? 0);
  return {
    id: installationId(env.storage, env.crypto),
    kind: "web",
    device,
    name: defaultName(device),
  };
}

/** « App web · iPhone », « App Mac · ordinateur », « App pas encore à jour ». */
export function kindLabel(c) {
  if (c.unknown) return "App pas encore à jour";
  const app = { web: "App web", mac: "App Mac" }[c.kind] ?? "App";
  const device = { iphone: "iPhone", ipad: "iPad", android: "Android", computer: "ordinateur" }[c.device];
  return device ? `${app} · ${device}` : app;
}

/** « Sur place » ou « À distance (Tailscale) ». */
export const viaLabel = (via) => (via === "tailscale" ? "À distance (Tailscale)" : "Sur place");

/** Le dernier accès : « maintenant », « aujourd'hui, 18:31 », « hier, 22:10 », « 12/11, 21:04 ». */
export function lastSeenLabel(lastSeenS, now = new Date()) {
  const at = new Date(lastSeenS * 1000);
  if (now - at < 120_000) return "maintenant";
  const time = `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
  const day = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((day(now) - day(at)) / 86_400_000);
  if (days === 0) return `aujourd'hui, ${time}`;
  if (days === 1) return `hier, ${time}`;
  return `${String(at.getDate()).padStart(2, "0")}/${String(at.getMonth() + 1).padStart(2, "0")}, ${time}`;
}

/** Les connexions de la feuille : la plus récente d'abord, celle de cet appareil en tête. */
export function sortConnections(list) {
  return [...list].sort((a, b) => (b.current - a.current) || (b.last_seen - a.last_seen));
}
