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
