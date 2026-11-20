// « Widgets et raccourcis » : trois raccourcis Apple signés et le script du widget Scriptable,
// téléchargés depuis le serveur (`/app/raccourcis/…`, sans jeton, aucun secret dedans).
//
// Ici, seulement la logique sans navigateur (essai : `node --test crates/server/web/widgets.test.mjs`).
// L'affichage est dans app.js ; le presse-papiers et `fetch` sont passés en paramètre.

/** Les trois raccourcis. Le nom importé vient du nom du fichier envoyé par le serveur : le widget
 *  Scriptable les appelle par « Mode absent », « Mode présent » et « Tout éteindre ». */
export const SHORTCUTS = [
  { id: "absent", label: "Mode absent", href: "raccourcis/mode-absent.shortcut" },
  { id: "present", label: "Mode présent", href: "raccourcis/mode-present.shortcut" },
  { id: "all-off", label: "Tout éteindre", href: "raccourcis/tout-eteindre.shortcut", sessionToken: true },
];

export const SCRIPT_PATH = "raccourcis/matter-home.js";

/** L'adresse à coller dans les raccourcis : `location.origin`. Sur le port 8443 (adresse directe
 *  du serveur), Raccourcis refuse le certificat : on conseille l'adresse en .ts.net. */
export function originInfo(loc) {
  const origin = String(loc?.origin ?? "");
  const direct = String(loc?.port ?? "") === "8443";
  return {
    origin,
    usable: /^https?:\/\/[^/]+$/.test(origin),
    direct,
    warning: direct
      ? "Ouvre plutôt l'app par l'adresse en .ts.net : Raccourcis refuse le certificat de l'adresse directe."
      : "",
  };
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

/** Lit le script Scriptable. Erreur claire si le serveur ne répond pas ou renvoie autre chose. */
export async function fetchScript(fetchFn, path = SCRIPT_PATH) {
  const response = await fetchFn(path, { cache: "no-store" });
  if (!response || !response.ok) throw new Error("Script indisponible : mettez le serveur à jour.");
  const text = await response.text();
  if (!text.includes("Scriptable")) throw new Error("Script illisible : réessayez.");
  return text;
}
