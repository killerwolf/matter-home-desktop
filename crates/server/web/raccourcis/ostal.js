// Variables used by Scriptable.
// These must be at the very top of the file. Do not edit.
// icon-color: deep-green; icon-glyph: home;

// ─────────────────────────────────────────────────────────────
//  Ostal — widget Scriptable (petit et moyen)
//  Montre : le mode de la maison, une phrase d'état, les lumières
//  allumées, et deux boutons (Absent, Présent). Rien à modifier :
//  l'adresse du serveur et
//  le jeton de lecture sont écrits ci-dessous par l'app web
//  (« Widgets et raccourcis » › « Installer le widget »).
//  Le jeton de lecture ne permet que de lire l'état : ni appareils,
//  ni caméras, ni changement de mode. Pour en changer, recopie le
//  widget depuis l'app web (l'ancien jeton cesse de marcher).
// ─────────────────────────────────────────────────────────────

const SERVEUR = "__SERVEUR__";
const TOKEN = "__JETON_DE_LECTURE__";

// Noms exacts des raccourcis installés par l'app web (sans accent : iOS
// enregistre les accents d'un nom de fichier d'une façon que le lien
// shortcuts:// ne retrouve pas). Le widget n'a que deux boutons : Nuit et
// Vacances sont des raccourcis à lancer depuis l'app Raccourcis ou l'app web.
const RACCOURCI_ABSENT = "Mode absent";
const RACCOURCI_PRESENT = "Mode present";

const NOM_SCRIPT = Script.name(); // « Ostal »
const CACHE = "ostal-cache.json";

// Présent est en Lueur ; les autres modes en accent (charte « Nuit et Lueur »).
const MODES = {
  present: { label: "Présent", symbol: "house.fill" },
  absent: { label: "Absent", symbol: "lock.fill" },
  nuit: { label: "Nuit", symbol: "moon.fill" },
  vacances: { label: "Vacances", symbol: "airplane" },
};

const dyn = (clair, sombre) => Color.dynamic(new Color(clair), new Color(sombre));
const C = {
  bg: dyn("#ffffff", "#12202a"),
  text: dyn("#10232d", "#e8eef1"),
  dim: dyn("#50626b", "#9aacb5"),
  line: dyn("#d3dce0", "#2a3c47"),
  btn: dyn("#e9eef0", "#192a35"),
  warn: dyn("#b4500e", "#f08a3c"),
  bad: dyn("#b42318", "#f27066"),
  ok: dyn("#1f7a46", "#4cc07e"),
  lueur: dyn("#f5b33f", "#ffc35a"),
  lueurTexte: dyn("#8a5a00", "#ffc35a"),
  surLueur: new Color("#10232d"),
  accent: dyn("#0b6a94", "#5fb6de"),
  accentSoft: dyn("#e2f0f6", "#12303d"),
};

// ───────── Réglages ─────────

// Un script recopié à la main depuis le dépôt garde ses emplacements vides.
const rempli = (v) => typeof v === "string" && v.trim() !== "" && !v.startsWith("__");
const serveur = () => (rempli(SERVEUR) ? SERVEUR.trim().replace(/\/+$/, "") : "");
const jeton = () => (rempli(TOKEN) ? TOKEN.trim() : "");

// ───────── Réseau ─────────

class HttpError extends Error {
  constructor(status, code) {
    super(code || "http_" + status);
    this.status = status;
  }
}

// Lecture seule : le jeton de lecture n'ouvre que GET /api/state et GET /api/events.
async function api(path) {
  const base = serveur();
  if (!base || !jeton()) throw new HttpError(0, "pas_installe");
  const req = new Request(base + path);
  req.method = "GET";
  req.timeoutInterval = 10;
  // Scriptable : `req.headers` doit être affecté d'un bloc. Modifier une clé après coup
  // (une clé de req.headers changée ensuite) est perdu en silence, et l'en-tête Authorization ne
  // partirait pas.
  const headers = { Accept: "application/json", Authorization: "Bearer " + jeton() };
  req.headers = headers;
  const text = await req.loadString(); // lève une erreur si le réseau ne répond pas
  const status = req.response ? req.response.statusCode : 0;
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch (e) { json = null; }
  if (status < 200 || status >= 300) {
    throw new HttpError(status, json && json.error ? json.error.code : null);
  }
  return json;
}

// ───────── Lecture de l'état ─────────

function latest(d, metric) {
  const l = (d.latest || []).find((x) => x.metric === metric);
  return l ? l.value : null;
}
// Même règle que l'app web (tiles.js, symbolOf) : une « lumière » est un appareil
// au pictogramme Light, ou sans pictogramme choisi et dont un type contient « Lampe ».
function estLumiere(d) {
  const info = d.device || {};
  const caps = info.caps || {};
  if (!caps.on_off) return false;
  if (info.icon) return info.icon === "Light";
  const types = (info.device_types || []).join(" ");
  if (caps.power || types.includes("Prise")) return false;
  return types.includes("Lampe");
}

async function lireEtat() {
  const state = await api("/api/state");
  const devices = state.devices || [];
  const lumieres = devices.filter(estLumiere);
  const allumees = lumieres.filter((d) => latest(d, "on_off") === 1 && (d.status || {}).state !== "unreachable");
  const injoignables = devices.filter((d) => (d.status || {}).state === "unreachable").length;
  const piles = devices.filter((d) => {
    const b = latest(d, "battery");
    return latest(d, "battery_low") === 1 || (b !== null && b < 15);
  }).length;
  return {
    mode: state.mode || null,
    lumieres: lumieres.length,
    allumees: allumees.length,
    injoignables,
    piles,
    lu: Date.now(),
  };
}

// ───────── Cache (pour l'absence de réseau) ─────────

const fm = FileManager.local();
const cachePath = fm.joinPath(fm.documentsDirectory(), CACHE);
function lireCache() {
  try { return fm.fileExists(cachePath) ? JSON.parse(fm.readString(cachePath)) : null; } catch (e) { return null; }
}
function ecrireCache(s) {
  try { fm.writeString(cachePath, JSON.stringify(s)); } catch (e) { /* rien */ }
}

// ───────── Mise en forme ─────────

function heure(ms) {
  const d = new Date(ms);
  const now = new Date();
  const hm = String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
  const jour = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const ecart = Math.round((jour(now) - jour(d)) / 86400000);
  if (ecart === 0) return hm;
  if (ecart === 1) return "hier " + hm;
  return String(d.getDate()).padStart(2, "0") + "/" + String(d.getMonth() + 1).padStart(2, "0") + " " + hm;
}
const plural = (n, un, plusieurs) => n + " " + (n > 1 ? plusieurs : un);

function shortcutUrl(name) {
  return "shortcuts://run-shortcut?name=" + encodeURIComponent(name);
}
function scriptUrl(params) {
  const q = params ? "?" + params : "";
  return "scriptable:///run/" + encodeURIComponent(NOM_SCRIPT) + q;
}

function texte(stack, s, size, color, { bold = false, lines = 1 } = {}) {
  const t = stack.addText(s);
  t.font = bold ? Font.boldSystemFont(size) : Font.systemFont(size);
  t.textColor = color;
  t.lineLimit = lines;
  t.minimumScaleFactor = 0.7;
  return t;
}
function symbole(stack, name, size, color) {
  const sym = SFSymbol.named(name);
  if (!sym) return null;
  sym.applyFont(Font.systemFont(size));
  const img = stack.addImage(sym.image);
  img.imageSize = new Size(size, size);
  img.tintColor = color;
  return img;
}
function modeInfo(mode) {
  const m = MODES[mode] || { label: mode ? mode : "—", symbol: "house" };
  return { ...m, color: mode === "present" ? C.lueurTexte : C.accent };
}

// Le titre de la phrase d'état : la même règle que partout (protocol::status::headline,
// tiles.js). Un « point » est une catégorie à regarder, pas un appareil : deux lampes
// injoignables font « Une chose à regarder ».
function titreEtat(points) {
  if (points === 0) return "La maison va bien.";
  if (points === 1) return "Une chose à regarder";
  if (points === 2) return "Deux choses à regarder";
  return points + " choses à regarder";
}
// Le jeton de lecture ne donne pas les caméras : le widget ne compte que les appareils.
const pointsEtat = (s) => (s.injoignables > 0 ? 1 : 0);

// En-tête : « Ostal » à gauche, pastille du mode à droite.
function entete(parent, s) {
  const top = parent.addStack();
  top.centerAlignContent();
  texte(top, "Ostal", 11, C.dim, { bold: true });
  top.addSpacer();
  const m = modeInfo(s.mode);
  if (s.mode === "present") symbole(top, "circle.fill", 10, C.lueur);
  else symbole(top, m.symbol, 12, m.color);
  top.addSpacer(6);
  texte(top, s.mode ? m.label : "Mode inconnu", 13, m.color, { bold: true });
}

// Phrase d'état puis « 1 lumière allumée · 1 injoignable » (sert au petit et au moyen widget).
function corpsEtat(parent, s) {
  const points = pointsEtat(s);
  const t = parent.addStack();
  t.centerAlignContent();
  symbole(t, points === 0 ? "checkmark.circle" : "exclamationmark.triangle", 16, points === 0 ? C.ok : C.warn);
  t.addSpacer(6);
  texte(t, titreEtat(points), 15, points === 0 ? C.text : C.warn, { bold: true, lines: 2 });
  parent.addSpacer(6);

  const l = parent.addStack();
  l.centerAlignContent();
  symbole(l, s.allumees > 0 ? "lightbulb.fill" : "lightbulb", 14, s.allumees > 0 ? C.lueurTexte : C.dim);
  l.addSpacer(5);
  const lt = s.lumieres === 0 ? "Aucune lumière"
    : s.allumees === 0 ? "Tout est éteint"
    : plural(s.allumees, "lumière allumée", "lumières allumées");
  texte(l, lt, 13, C.text);
  const alertes = [];
  if (s.injoignables) alertes.push(s.injoignables + " injoignable" + (s.injoignables > 1 ? "s" : ""));
  if (s.piles) alertes.push(plural(s.piles, "pile faible", "piles faibles"));
  if (alertes.length) {
    l.addSpacer(10);
    symbole(l, "nosign", 14, C.warn);
    l.addSpacer(4);
    texte(l, alertes.join(" · "), 13, C.warn, { bold: true });
  }
}

// Bouton de mode : celui du mode actuel est plein (Présent en Lueur, Absent en accent doux).
function bouton(parent, label, mode, actuel, url) {
  const choisi = actuel === mode;
  const b = parent.addStack();
  b.size = new Size(150, 44);
  b.cornerRadius = 10;
  b.borderWidth = 1;
  if (choisi && mode === "present") {
    b.backgroundColor = C.lueur;
    b.borderColor = C.lueur;
  } else if (choisi) {
    b.backgroundColor = C.accentSoft;
    b.borderColor = C.accent;
  } else {
    b.backgroundColor = C.btn;
    b.borderColor = C.line;
  }
  b.centerAlignContent();
  b.url = url;
  b.addSpacer();
  texte(b, label, 15, choisi && mode === "present" ? C.surLueur : C.text, { bold: true });
  b.addSpacer();
  return b;
}

function widgetMessage(titre, detail, url) {
  const w = new ListWidget();
  w.backgroundColor = C.bg;
  w.setPadding(14, 14, 14, 14);
  texte(w, "Ostal", 11, C.dim, { bold: true });
  w.addSpacer(8);
  texte(w, titre, 15, C.warn, { bold: true, lines: 2 });
  w.addSpacer(4);
  texte(w, detail, 12, C.dim, { lines: 4 });
  w.addSpacer();
  w.url = url;
  return w;
}

function construireWidget(family, s, note, perime) {
  const w = new ListWidget();
  w.backgroundColor = C.bg;
  w.refreshAfterDate = new Date(Date.now() + 5 * 60 * 1000);
  w.setPadding(12, 14, 12, 14);
  entete(w, s);
  w.addSpacer(8);
  corpsEtat(w, s);
  if (family === "medium" || family === "large") {
    // Deux boutons seulement : Absent et Présent (Nuit et Vacances : raccourcis).
    w.addSpacer(8);
    const row = w.addStack();
    bouton(row, "Absent", "absent", s.mode, shortcutUrl(RACCOURCI_ABSENT));
    row.addSpacer();
    bouton(row, "Présent", "present", s.mode, shortcutUrl(RACCOURCI_PRESENT));
  }
  w.addSpacer();
  // 11 pt : jamais sous 10.
  texte(w, note, 11, perime ? C.warn : C.dim);
  // Toucher le fond du widget ouvre l'app web Ostal.
  const base = serveur();
  w.url = base ? base + "/app/" : scriptUrl();
  return w;
}

// ───────── Messages ─────────

const MAISON_NE_REPOND_PAS = "La maison ne répond pas.";
const VERIFIER_LE_MAC = "Vérifie que le Mac est allumé et que Tailscale est activé.";

function messageErreur(e) {
  if (e instanceof HttpError) {
    if (e.status === 0 && e.message === "pas_installe") return "Widget pas installé : copie-le depuis l'app web (Widgets et raccourcis › Installer le widget).";
    if (e.status === 401) return "Jeton révoqué : recopie le widget depuis l'app web.";
    if (e.status === 426) return "Version du serveur différente : mets à jour Ostal.";
    return "Le serveur a répondu une erreur (" + e.status + (e.message ? ", " + e.message : "") + ").";
  }
  return MAISON_NE_REPOND_PAS + " " + VERIFIER_LE_MAC;
}

async function info(msg) {
  const a = new Alert();
  a.title = "Ostal";
  a.message = msg;
  a.addAction("OK");
  await a.presentAlert();
}

async function etatPourWidget() {
  try {
    const s = await lireEtat();
    ecrireCache(s);
    return { s, note: "À jour à " + heure(s.lu), perime: false };
  } catch (e) {
    const cache = lireCache();
    if (e instanceof HttpError && (e.status === 401 || e.status === 0)) return { erreur: e };
    if (cache) return { s: cache, note: "injoignable · lu à " + heure(cache.lu), perime: true };
    return { erreur: e };
  }
}

async function rendreWidget(family) {
  const r = await etatPourWidget();
  if (r.erreur) {
    const auth = r.erreur instanceof HttpError && (r.erreur.status === 401 || r.erreur.status === 0);
    const base = serveur();
    const reseau = !(r.erreur instanceof HttpError);
    const titre = auth ? "Widget à recopier" : reseau ? MAISON_NE_REPOND_PAS : "Le serveur a un souci";
    const detail = reseau ? VERIFIER_LE_MAC : messageErreur(r.erreur);
    return widgetMessage(titre, detail, base ? base + "/app/" : scriptUrl());
  }
  return construireWidget(family, r.s, r.note, r.perime);
}

// ───────── Point d'entrée ─────────

async function main() {
  if (config.runsInWidget) {
    Script.setWidget(await rendreWidget(config.widgetFamily || "small"));
    return;
  }
  if (!jeton() || !serveur()) {
    await info(messageErreur(new HttpError(0, "pas_installe")));
    return;
  }
  const menu = new Alert();
  menu.title = "Ostal";
  menu.addAction("Voir le widget moyen");
  menu.addAction("Voir le petit widget");
  menu.addCancelAction("Fermer");
  const choix = await menu.presentSheet();
  if (choix === 0) (await rendreWidget("medium")).presentMedium();
  else if (choix === 1) (await rendreWidget("small")).presentSmall();
}

await main();
Script.complete();
