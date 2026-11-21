// Mode global de la maison (ticket n° 8) : les quatre modes, leurs libellés, et ce que fait une
// caméra dans un mode. Le serveur est la source de vérité (`mode` dans le snapshot, événement
// `mode_changed`, `PUT /api/mode`) ; ici, seulement des fonctions pures.

/** Les quatre modes, dans l'ordre d'affichage ; `id` est la valeur du JSON. */
export const MODES = [
  { id: "present", label: "Présent" },
  { id: "absent", label: "Absent" },
  { id: "nuit", label: "Nuit" },
  { id: "vacances", label: "Vacances" },
];

export const DEFAULT_MODE = "present";

export const isMode = (id) => MODES.some((m) => m.id === id);

/** Libellé d'un mode ; un mode inconnu (serveur plus récent) s'affiche comme « Présent ». */
export const modeLabel = (id) => (MODES.find((m) => m.id === id) ?? MODES[0]).label;

/** Ce que fait une caméra dans un mode : « eteinte », « diffusion » ou « surveillance ». Un serveur
 *  plus ancien n'envoie pas le tableau : la caméra diffuse, comme avant. */
export const cameraBehavior = (cam, mode) => cam?.modes?.[mode] ?? "diffusion";

/** Éteinte : le serveur refuse le direct et l'image fixe, le client ne les demande pas. */
export const cameraOff = (cam, mode) => cameraBehavior(cam, mode) === "eteinte";

export const offText = (mode) => `Éteinte en mode ${modeLabel(mode)}`;

/** D'où vient un changement de mode (`source` de `mode_changed`, absent d'un ancien serveur) :
 *  « raccourci iPhone », « automatisation « Nuit à 23 h » »… ; `""` quand on ne sait pas. */
export function sourceLabel(source) {
  switch (source?.kind) {
    case "shortcut": return "raccourci iPhone";
    case "web": return "app web";
    case "desktop": return "app de bureau";
    case "server_app": return "fenêtre du serveur";
    case "rule": return source.name ? `automatisation « ${source.name} »` : "automatisation";
    case "widget": return "widget iPhone";
    case "other": return "autre";
    default: return "";
  }
}

/** La notification brève d'un changement de mode venu d'ailleurs : « Mode Absent · raccourci
 *  iPhone ». `null` pour le changement que cette app vient de demander elle-même (`requested`). */
export function modeChangeNotice(mode, source, requested) {
  if (!isMode(mode)) return null;
  if (requested === mode && (source === undefined || source?.kind === "web")) return null;
  const from = sourceLabel(source);
  return from ? `Mode ${modeLabel(mode)} · ${from}` : `Mode ${modeLabel(mode)}`;
}
