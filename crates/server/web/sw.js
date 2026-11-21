// Service worker minimal : garde la coquille (HTML, CSS, JS, icônes) pour que l'app s'ouvre
// instantanément et hors ligne. Il ne touche jamais à l'API : /api/ passe toujours par le réseau
// (jeton, état, flux), rien de la maison n'est mis en cache.
const CACHE = "ostal-shell-v10";
const SHELL = [
  "./", "index.html", "app.css", "tokens.css", "app.js", "api.js", "tiles.js", "icons.js",
  "charts.js", "temps.js", "cameras.js", "events.js", "plan.js", "mode.js", "push.js", "widgets.js", "manifest.webmanifest", "icon-192.png", "icon-512.png", "apple-touch-icon.png", "favicon.png",
];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== "GET" || url.origin !== self.location.origin) return;
  if (!url.pathname.startsWith("/app/")) return; // l'API n'est jamais interceptée
  // Les raccourcis préparés pour l'iPhone (ils portent un jeton) ne sont jamais gardés, et un
  // téléchargement ne passe pas par le service worker.
  if (url.pathname.startsWith("/app/raccourcis/")) return;
  // Réseau d'abord (une nouvelle version du serveur se voit tout de suite), cache en secours.
  event.respondWith(
    fetch(event.request)
      .then((response) => {
        if (response.ok) {
          const copy = response.clone();
          caches.open(CACHE).then((c) => c.put(event.request, copy));
        }
        return response;
      })
      .catch(() => caches.match(event.request).then((hit) => hit || (event.request.mode === "navigate" ? caches.match("index.html") : Response.error()))),
  );
});

// ---------------------------------------------------------------------------
// Notifications web push (ticket n° 17). Le message arrive chiffré de bout en bout (RFC 8291) ;
// le navigateur le déchiffre avant de le donner ici : { title, body, url, tag }.
// ---------------------------------------------------------------------------

/** Même règle que `clickTarget` (push.js) : même origine, sous /app/, sinon l'accueil. */
function clickTarget(url) {
  try {
    const target = new URL(url || "/app/", self.location.origin);
    if (target.origin === self.location.origin && target.pathname.startsWith("/app/")) return target.href;
  } catch { /* adresse illisible */ }
  return new URL("/app/", self.location.origin).href;
}

self.addEventListener("push", (event) => {
  let data = {};
  try { data = (event.data ? event.data.json() : {}) || {}; } catch { data = {}; }
  // Toujours une notification visible : Safari retire l'abonnement après des messages muets.
  const title = typeof data.title === "string" && data.title ? data.title : "Ostal";
  const options = {
    body: typeof data.body === "string" ? data.body : "",
    icon: "icon-192.png",
    badge: "icon-192.png",
    lang: "fr",
    data: { url: clickTarget(data.url) },
  };
  if (typeof data.tag === "string" && data.tag) options.tag = data.tag;
  event.waitUntil(self.registration.showNotification(title, options));
});

// Le service de push a renouvelé l'abonnement : on se réabonne avec la même clé du serveur. Le
// service worker n'a pas le jeton de session ; l'app renvoie le nouvel abonnement au serveur à son
// prochain démarrage (`syncOnStart`, push.js).
self.addEventListener("pushsubscriptionchange", (event) => {
  const key = event.oldSubscription && event.oldSubscription.options && event.oldSubscription.options.applicationServerKey;
  if (!key) return;
  event.waitUntil(
    self.registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key }).catch(() => {}),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = clickTarget(event.notification.data && event.notification.data.url);
  event.waitUntil((async () => {
    const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    const open = windows.find((c) => new URL(c.url).pathname.startsWith("/app/"));
    if (open) {
      // L'app ouverte affiche la page demandée (fiche d'un événement) sans se recharger.
      open.postMessage({ type: "open", url });
      return open.focus();
    }
    return self.clients.openWindow(url);
  })());
});
