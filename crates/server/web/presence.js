// Présence par le Wi-Fi de la box : qui est à la maison (GET /api/presence/people), « Lequel est ton
// téléphone ? » (GET /api/wifi/phones, PUT /api/wifi/phone). Que de la logique pure et des phrases
// françaises (maquettes Web-Accueil-Presence et Web-Choisir-Telephone), sans DOM, pour l'essayer à
// la main : node --test crates/server/web/presence.test.mjs
//
// Le serveur ne dit que des prénoms et des états, et, pour son propre compte, le nom du téléphone :
// jamais une adresse. Aucun genre n'est retenu pour une personne : les phrases n'en supposent pas.

import { modeLabel } from "./mode.js";

const LATER_KEY = "ostal.phone.later";
/** « Plus tard » : on redemande au bout d'une semaine, pas à chaque ouverture. */
export const LATER_MS = 7 * 24 * 3600 * 1000;

const pad = (n) => String(n).padStart(2, "0");
const clock = (seconds) => {
  const d = new Date(seconds * 1000);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
};

/** « Sam », « Sam et Léa », « Sam, Léa et Adam ». */
export function joinNames(names) {
  if (names.length <= 1) return names.join("");
  return `${names.slice(0, -1).join(", ")} et ${names[names.length - 1]}`;
}

/** Une personne dont l'état est connu : « vous » pour soi. Les autres gardent leur prénom. */
const label = (p) => (p.me ? "vous" : p.name);
const capital = (text) => text.charAt(0).toUpperCase() + text.slice(1);

/** L'initiale d'une pastille. */
export const initial = (name) => ([...String(name).trim()][0] ?? "?").toUpperCase();

/** La carte « Présence » de l'accueil, ou `null` quand il n'y a rien à dire (pas de box, personne
 *  de connu) : l'app web se comporte alors comme avant.
 *  - `lead` : « À la maison : vous et Sam » ;
 *  - `away` : « Léa n'est pas là depuis 17:20 » (neutre : aucun genre n'est supposé) ;
 *  - `mode` : « Mode Présent, automatique » ;
 *  - `note` : la box ne répond pas, la présence affichée peut être dépassée ;
 *  - `people` : les pastilles, ceux qui sont là d'abord. */
export function presenceCard(view, mode) {
  if (!view) return null;
  const people = Array.isArray(view.people) ? view.people.filter((p) => p.state === "home" || p.state === "away") : [];
  const boxDown = view.box_link === "unreachable" || view.box_link === "refused";
  if (!people.length && !view.auto_mode && !boxDown) return null;
  const home = people.filter((p) => p.state === "home").sort((a, b) => Number(b.me) - Number(a.me));
  const away = people.filter((p) => p.state === "away").sort((a, b) => Number(b.me) - Number(a.me));
  const lead = home.length ? `À la maison : ${joinNames(home.map(label))}` : "Personne n'est à la maison";
  let awayText = "";
  if (away.length) {
    const names = joinNames(away.map(label));
    const be = away.some((p) => p.me) ? "n'êtes" : away.length > 1 ? "ne sont" : "n'est";
    const since = away.length === 1 && away[0].since ? ` depuis ${clock(away[0].since)}` : "";
    awayText = `${capital(names)} ${be} pas là${since}`;
  }
  return {
    lead,
    away: awayText,
    mode: `Mode ${modeLabel(mode)}${view.auto_mode ? ", automatique" : ""}`,
    note: boxDown ? "La box ne répond pas : la présence peut ne pas être à jour." : "",
    people: [...home, ...away].map((p) => ({
      name: p.me ? "Vous" : p.name,
      initial: initial(p.name),
      home: p.state === "home",
      aria: p.state === "home"
        ? `${p.me ? `Vous (${p.name})` : p.name}, à la maison`
        : `${p.me ? `Vous (${p.name})` : p.name}, pas là${p.since ? ` depuis ${clock(p.since)}` : ""}`,
    })),
  };
}

/** La pastille de mode de l'en-tête : « Mode : Présent » (automatique dans le libellé vocal). */
export function modeChipLabel(mode, auto) {
  return auto ? `Mode : ${modeLabel(mode)}, automatique` : `Mode : ${modeLabel(mode)}`;
}

// ---- « Lequel est votre téléphone ? » ----------------------------------------------------------------

/** Faut-il poser la question ? Une box est reliée, la personne n'a pas encore choisi, et elle n'a pas
 *  dit « plus tard » depuis moins d'une semaine. */
export function shouldAskPhone(phones, laterAt, now = Date.now()) {
  if (!phones?.linked || phones.mine) return false;
  if (laterAt && now - laterAt < LATER_MS) return false;
  return true;
}

/** « Plus tard » : lu et écrit dans le stockage local, sans jamais échouer. */
export function readLater(storage = globalThis.localStorage) {
  try {
    const n = Number(storage?.getItem(LATER_KEY));
    return Number.isFinite(n) && n > 0 ? n : 0;
  } catch { return 0; }
}

export function writeLater(now = Date.now(), storage = globalThis.localStorage) {
  try { storage?.setItem(LATER_KEY, String(now)); } catch { /* pas gardé : on redemandera */ }
}

export function clearLater(storage = globalThis.localStorage) {
  try { storage?.removeItem(LATER_KEY); } catch { /* rien à effacer */ }
}

/** Les textes de l'écran. `phones` : la réponse de GET /api/wifi/phones ; `name` : le prénom de la
 *  personne (vide : l'écran dit « Bonjour. »). */
export function chooserTexts(phones, name = "") {
  const hello = name ? `Bonjour ${name}. ` : "Bonjour. ";
  const free = phones?.free ?? [];
  return {
    title: "Lequel est votre téléphone ?",
    intro: `${hello}Ostal reconnaît les téléphones vus sur le Wi-Fi de la maison. Choisissez le vôtre.`,
    listTitle: "Vus sur le Wi-Fi, pas encore attribués",
    empty: "Aucun téléphone libre vu sur le Wi-Fi pour l'instant. Connectez votre téléphone au Wi-Fi de la maison, puis touchez « Actualiser la liste ».",
    refresh: "Actualiser la liste",
    later: "Plus tard : je n'ai pas mon téléphone ici",
    confirm: "Choisir ce téléphone",
    whyTitle: "À quoi ça sert ?",
    why: "Ostal voit quand votre téléphone rentre et quand il part. Quand le dernier part, la maison passe en Absent ; dès qu'une personne rentre, elle repasse en Présent.",
    privacy: "Ostal ne garde que le nom du téléphone, jamais son adresse. Le journal ne se lit que dans Ostal Server, sur le Mac de la maison.",
    hasFree: free.length > 0,
    free,
  };
}

/** Le choix proposé au départ : le téléphone déjà choisi s'il est encore dans la liste, sinon rien. */
export function listWithMine(phones) {
  const free = [...(phones?.free ?? [])];
  if (phones?.mine && !free.some((n) => n.toLowerCase() === phones.mine.toLowerCase())) free.unshift(phones.mine);
  return free;
}

/** La phrase quand le serveur refuse un choix. */
export function chooseError(error) {
  if (error?.status === 409) return "Ce téléphone a déjà un propriétaire. Actualisez la liste et choisissez-en un autre.";
  if (error?.status === 404) return "Ce téléphone n'est plus vu sur le Wi-Fi. Actualisez la liste.";
  if (error?.status === 503) return "Ostal Server n'est plus relié à la box.";
  return "Le téléphone n'a pas pu être enregistré. Réessayez.";
}

/** Les routes d'un serveur plus ancien n'existent pas (404 « not_found » sur toute la route) : on ne
 *  montre rien, sans bandeau d'erreur. */
export const isMissingRoute = (error) => error?.status === 404 || error?.status === 405;
