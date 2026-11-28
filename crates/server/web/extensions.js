// Extensions (réglages › Extensions) et météo extérieure : logique pure et phrases françaises, sans
// DOM, pour l'essayer à la main : node crates/server/web/extensions.test.mjs
//
// Routes : GET /api/extensions, PUT /api/extensions/{id}, GET /api/extensions/weather/search?q=…,
// GET /api/weather. Le serveur ne renvoie jamais un secret : `secrets` dit seulement « défini » ou non.

const pad = (n) => String(n).padStart(2, "0");
const clock = (seconds) => {
  const d = new Date(seconds * 1000);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
const fr = (value, digits) => value.toLocaleString("fr-FR", { minimumFractionDigits: digits, maximumFractionDigits: digits });

export const isMissingRoute = (error) => error?.status === 404 || error?.status === 405;

/** Les trois grandeurs d'un relevé, dans l'ordre d'affichage : « 7,4 °C », « 82 % », « 1 014 hPa ». */
export function readingParts(reading) {
  if (!reading) return [];
  const parts = [];
  if (Number.isFinite(reading.temperature)) parts.push(`${fr(reading.temperature, 1)} °C`);
  if (Number.isFinite(reading.humidity)) parts.push(`${fr(reading.humidity, 0)} %`);
  if (Number.isFinite(reading.pressure)) parts.push(`${fr(reading.pressure, 0)} hPa`);
  return parts;
}

/** Ce que montrent le plan et l'accueil, ou `null` quand il n'y a rien à dire (extension éteinte,
 *  pas de lieu, pas encore de relevé). Hors ligne, la dernière valeur reste, avec son heure.
 *  - `line` : « 7,4 °C · 82 % · 1 014 hPa » ;
 *  - `note` : « Relevé à 14:30 » ou « Dernier relevé à 14:30 : Open-Meteo ne répond pas » ;
 *  - `stale` : la valeur est celle d'avant l'erreur. */
export function weatherCard(weather) {
  if (!weather?.enabled || !weather.reading) return null;
  const parts = readingParts(weather.reading);
  if (!parts.length) return null;
  const at = clock(weather.reading.at);
  return {
    place: weather.place ?? "",
    line: parts.join(" · "),
    note: weather.failing ? `Dernier relevé à ${at} : Open-Meteo ne répond pas` : `Relevé à ${at}`,
    stale: Boolean(weather.failing),
    aria: `Dehors : ${parts.join(", ")}`,
  };
}

/** Ce que dit la page de configuration de la météo sous le lieu choisi. */
export function weatherStatusText(view) {
  const status = view?.status ?? {};
  if (!view?.enabled) return "Désactivée : Ostal n'appelle pas Open-Meteo.";
  if (!status.place) return "Choisissez un lieu : Ostal ne relève rien tant qu'il n'y en a pas.";
  if (!status.reading) return status.failing ? "Open-Meteo ne répond pas pour l'instant : nouvel essai dans quelques minutes." : "Premier relevé en cours…";
  const at = clock(status.reading.at);
  return status.failing ? `Dernier relevé à ${at} (Open-Meteo ne répond pas pour l'instant).` : `Dernier relevé à ${at}.`;
}

/** La phrase d'une erreur de l'API des extensions. */
export function errorText(error) {
  if (error?.detail) return error.detail;
  if (isMissingRoute(error)) return "Ce serveur ne gère pas encore les extensions : mettez-le à jour.";
  return "Échec, réessayez.";
}

/** Le lieu choisi, tel que le serveur le garde : `{ name, latitude, longitude }`. */
export const placeConfig = (place) => ({ place: place ? { name: place.name, latitude: place.latitude, longitude: place.longitude } : null });

/** Le texte cherché est-il assez long pour partir chez Open-Meteo ? (2 caractères au moins.) */
export const searchable = (text) => [...String(text ?? "").trim()].length >= 2;

// ---------------------------------------------------------------------------
// Volets Somfy (extension « somfy ») : GET /api/extensions/somfy/devices, POST
// /api/extensions/somfy/refresh. Le serveur ne montre à tous que l'état (`status`) ; l'identifiant
// (`config.email`) et le choix des appareils (`config.exposed`) ne vont qu'aux comptes qui règlent
// les extensions.
// ---------------------------------------------------------------------------

/** La phrase d'état de la page Somfy, d'après `view.status` (jamais d'identifiant ni d'adresse). */
export function somfyStatusText(view) {
  const status = view?.status ?? {};
  const n = status.devices ?? 0;
  const shutters = `${n} volet${n > 1 ? "s" : ""}`;
  switch (status.state) {
    case "off": return "Désactivée : Ostal n'appelle pas le cloud de Somfy.";
    case "unconfigured": return "Saisissez l'identifiant et le mot de passe du compte TaHoma : Ostal n'appelle pas Somfy tant qu'ils manquent.";
    case "waiting": return "Connexion à Somfy en cours…";
    case "offline": return n ? `Somfy ne répond pas pour l'instant : ${shutters} dans Ostal, d'après la dernière liste connue.` : "Somfy ne répond pas pour l'instant : nouvel essai dans quelques minutes.";
    case "rejected": return "Somfy refuse l'identifiant ou le mot de passe. Ostal n'essaiera plus tant qu'ils ne changent pas (réessayer trop souvent risque de bloquer le compte).";
    case "ok": return n ? `Connecté à Somfy : ${shutters} dans Ostal.` : "Connecté à Somfy, mais aucun volet n'est coché : cochez-en dans la liste ci-dessous.";
    default: return "";
  }
}

/** La dernière erreur d'ordre, ou `null`. */
export const somfyLastError = (view) => view?.status?.last_error ?? null;

const KIND_LABELS = { RollerShutter: "volet roulant", Shutter: "volet", Light: "lumière", Pod: "Kit", ExteriorScreen: "store extérieur", Awning: "store banne", GarageDoor: "porte de garage", Window: "fenêtre" };
/** Le type d'un appareil du compte, en français (la classe Overkiz sinon). */
export const somfyKindLabel = (kind) => KIND_LABELS[kind] ?? (kind ? kind : "appareil");

/** La liste des appareils du compte, dans l'ordre d'affichage : ceux qui s'ouvrent et se ferment d'abord. */
export function somfyChoices(choices) {
  const devices = [...(choices?.devices ?? [])];
  return devices.sort((a, b) => Number(b.commandable) - Number(a.commandable) || a.name.localeCompare(b.name, "fr"));
}

/** Les clés cochées après un clic sur la case de `key` : la nouvelle liste à envoyer (`config.exposed`). */
export function somfyExposedAfter(choices, key, checked) {
  const selected = (choices?.devices ?? []).filter((d) => d.selected && d.commandable).map((d) => d.key);
  const rest = selected.filter((k) => k !== key);
  return checked ? [...rest, key] : rest;
}

/** La configuration à envoyer (PUT remplace toute la configuration) : l'identifiant et le choix. */
export const somfyConfig = (email, exposed) => ({ email: String(email ?? "").trim(), exposed: exposed ?? null });

/** « Liste lue chez Somfy à 14:30. », ou `null` (liste jamais lue). */
export function somfySyncedText(choices) {
  return Number.isFinite(choices?.synced_at) ? `Liste lue chez Somfy à ${clock(choices.synced_at)}.` : null;
}

// ---------------------------------------------------------------------------
// Bbox (extension « bbox ») : la box de la maison, source de la présence Wi-Fi. Le serveur ne montre
// à tous que l'état (`status`). L'adresse, le mot de passe et l'interrupteur de la box se règlent
// seulement dans Ostal Server, sur le Mac : le serveur refuse tout changement par l'API.
// ---------------------------------------------------------------------------

/** La phrase d'état de la page Bbox, d'après `view.status` (jamais d'adresse ni de mot de passe). */
export const BOX_ON_THE_MAC = "La box se règle dans Ostal Server, sur le Mac";

export function bboxStatusText(view) {
  const status = view?.status ?? {};
  const n = status.devices ?? 0;
  const seen = n === 0 ? "aucun appareil actif" : `${n} appareil${n > 1 ? "s" : ""} actif${n > 1 ? "s" : ""}`;
  switch (status.state) {
    case "off": return "Désactivée : Ostal n'interroge pas la box, la présence ne suit que les raccourcis « Je pars » et « Je rentre ».";
    case "legacy": return "Ancien réglage de la box encore lu tel quel : la reprise dans l'extension a échoué, voir Ostal Server sur le Mac.";
    case "unconfigured": return "Le mot de passe de la box manque : Ostal n'interroge pas la box tant qu'il n'est pas saisi dans Ostal Server.";
    case "waiting": return "Premier relevé en cours…";
    case "unreachable": return "La box ne répond pas pour l'instant : la présence est figée, le mode de la maison ne change pas.";
    case "refused": return "La box refuse le mot de passe. Ostal n'essaiera plus tant qu'il ne change pas (la box pourrait bloquer l'accès).";
    case "ok": return Number.isFinite(status.last_ok) ? `Box reliée : ${seen} sur le Wi-Fi (relevé à ${clock(status.last_ok)}).` : `Box reliée : ${seen} sur le Wi-Fi.`;
    default: return "";
  }
}
