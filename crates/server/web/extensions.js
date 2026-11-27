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
