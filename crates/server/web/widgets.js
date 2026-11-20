// « Widgets et raccourcis » : installe sur l'iPhone, sans rien taper, les deux raccourcis de mode
// et le widget Scriptable. Le serveur les prépare avec son adresse et un jeton à part :
// - `POST /api/tokens/mode/shortcuts` : nouveau jeton de mode, deux raccourcis signés à toucher ;
// - `POST /api/tokens/read/widget` : nouveau jeton de lecture, le script du widget prérempli.
//
// Ici, seulement la logique sans navigateur (essai : `node --test crates/server/web/widgets.test.mjs`).
// L'affichage est dans app.js ; le presse-papiers et l'appel à l'API sont passés en paramètre.

/** Noms des deux raccourcis, sans accent : iOS enregistre un nom de fichier accentué en Unicode
 *  décomposé, et `shortcuts://run-shortcut?name=…` (composé) ne le retrouve pas. Mêmes noms dans
 *  le serveur (shortcuts.rs) et dans le widget (matter-home.js). */
export const SHORTCUT_NAMES = ["Mode absent", "Mode present"];

/** Raccourcis et widget ne savent joindre que l'adresse en .ts.net : sur une autre (le port 8443,
 *  au certificat auto-signé), on le dit avant d'essayer. */
export function originInfo(loc) {
  const origin = String(loc?.origin ?? "");
  const usable = /^https:\/\/[a-z0-9.-]+\.ts\.net$/i.test(origin);
  return {
    origin,
    usable,
    warning: usable
      ? ""
      : "Ouvre l'app par son adresse en .ts.net (Tailscale) : les raccourcis et le widget ne savent pas joindre celle-ci.",
  };
}

/** Prépare les deux raccourcis (nouveau jeton de mode). `request` est `api.request`. */
export async function prepareShortcuts(request, origin) {
  const result = await request("POST", "/api/tokens/mode/shortcuts", { origin });
  const shortcuts = Array.isArray(result?.shortcuts) ? result.shortcuts : [];
  const ok = shortcuts.length === SHORTCUT_NAMES.length
    && shortcuts.every((s) => SHORTCUT_NAMES.includes(s?.name) && typeof s?.url === "string" && s.url.startsWith("/app/raccourcis/perso/"));
  if (!ok) throw new Error("Réponse inattendue du serveur : mettez-le à jour.");
  return { shortcuts, expiresInS: Number(result.expires_in_s) || 300 };
}

/** Prépare le script du widget (nouveau jeton de lecture). */
export async function prepareWidget(request, origin) {
  const result = await request("POST", "/api/tokens/read/widget", { origin });
  const script = typeof result?.script === "string" ? result.script : "";
  if (!script.includes("Scriptable") || script.includes("__JETON_DE_LECTURE__")) {
    throw new Error("Script illisible : mettez le serveur à jour.");
  }
  return script;
}

/** « depuis le 20/11/2026 », ou rien. `created` en secondes Unix. */
export function sinceText(created) {
  if (typeof created !== "number" || !Number.isFinite(created)) return "";
  const d = new Date(created * 1000);
  const two = (n) => String(n).padStart(2, "0");
  return `depuis le ${two(d.getDate())}/${two(d.getMonth() + 1)}/${d.getFullYear()}`;
}

/** Copie un texte qui n'est pas encore là (le script, demandé au serveur). Safari n'accepte une
 *  copie que pendant le geste : on lui confie tout de suite un `ClipboardItem` dont le contenu
 *  arrive plus tard (`deps.write` = `navigator.clipboard.write`, `deps.ClipboardItem`). Sinon,
 *  copie ordinaire une fois le texte reçu. Rend `{ result, text }` ; une erreur de `textPromise`
 *  remonte telle quelle. */
export async function copyLater(textPromise, deps = {}) {
  const pending = Promise.resolve(textPromise);
  if (typeof deps.write === "function" && typeof deps.ClipboardItem === "function") {
    try {
      const blob = pending.then((t) => new Blob([t], { type: "text/plain" }));
      await deps.write([new deps.ClipboardItem({ "text/plain": blob })]);
      return { result: "copied", text: await pending };
    } catch { /* refusé, ou texte en erreur : voir plus bas */ }
  }
  const text = await pending;
  return { result: await copyText(text, deps), text };
}

/** Copie `text`. Renvoie `"copied"`, ou `"manual"` quand le navigateur refuse : la page affiche
 *  alors le texte dans une zone sélectionnée à copier à la main. `deps.writeText` est
 *  `navigator.clipboard.writeText` (absent hors HTTPS), `deps.execCopy` un repli facultatif. */
export async function copyText(text, deps = {}) {
  if (typeof deps.writeText === "function") {
    try {
      await deps.writeText(text);
      return "copied";
    } catch { /* refusé (pas de geste, pas de permission) : repli */ }
  }
  if (typeof deps.execCopy === "function") {
    try {
      if (deps.execCopy(text)) return "copied";
    } catch { /* repli suivant */ }
  }
  return "manual";
}
