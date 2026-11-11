// Service worker minimal : garde la coquille (HTML, CSS, JS, icônes) pour que l'app s'ouvre
// instantanément et hors ligne. Il ne touche jamais à l'API : /api/ passe toujours par le réseau
// (jeton, état, flux), rien de la maison n'est mis en cache.
const CACHE = "matter-home-shell-v2";
const SHELL = [
  "./", "index.html", "app.css", "tokens.css", "app.js", "api.js", "tiles.js", "icons.js",
  "charts.js", "temps.js", "cameras.js", "manifest.webmanifest", "icon-192.png", "icon-512.png",
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
