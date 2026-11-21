// Variables used by Scriptable.
// These must be at the very top of the file. Do not edit.
// icon-color: deep-green; icon-glyph: home;

// ─────────────────────────────────────────────────────────────
//  Ostal — widget Scriptable (petit et moyen)
//  Montre : le mode de la maison, les lumières allumées,
//  la dernière alerte. Rien à modifier : l'adresse du serveur et
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
// shortcuts:// ne retrouve pas).
const RACCOURCI_ABSENT = "Mode absent";
const RACCOURCI_PRESENT = "Mode present";

const NOM_SCRIPT = Script.name(); // « Ostal »
const CACHE = "ostal-cache.json";

const MODES = {
  present: { label: "Présent", symbol: "house.fill", light: "#1f8a4c", dark: "#4ccf7f" },
  absent: { label: "Absent", symbol: "lock.fill", light: "#c2410c", dark: "#fb923c" },
  nuit: { label: "Nuit", symbol: "moon.fill", light: "#4338ca", dark: "#a5b4fc" },
  vacances: { label: "Vacances", symbol: "airplane", light: "#0369a1", dark: "#7dd3fc" },
};
const CLASSES = { personne: "Personne", chat: "Chat", chien: "Chien", vehicule: "Véhicule" };

const C = {
  bg: Color.dynamic(new Color("#ffffff"), new Color("#1c1c1e")),
  text: Color.dynamic(new Color("#111111"), new Color("#f2f2f7")),
  dim: Color.dynamic(new Color("#6b6b70"), new Color("#a1a1a8")),
  warn: Color.dynamic(new Color("#b45309"), new Color("#fbbf24")),
  bad: Color.dynamic(new Color("#b91c1c"), new Color("#f87171")),
  btn: Color.dynamic(new Color("#f1f1f4"), new Color("#2c2c2e")),
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
  let events = [];
  try {
    const ev = await api("/api/events?limit=1");
    events = (ev && ev.events) || [];
  } catch (e) { /* détection absente ou indisponible : on s'en passe */ }

  const devices = state.devices || [];
  const lumieres = devices.filter(estLumiere);
  const allumees = lumieres.filter((d) => latest(d, "on_off") === 1 && (d.status || {}).state !== "unreachable");
  const injoignables = devices.filter((d) => (d.status || {}).state === "unreachable").length;
  const piles = devices.filter((d) => {
    const b = latest(d, "battery");
    return latest(d, "battery_low") === 1 || (b !== null && b < 15);
  }).length;

  // Une « alerte » = une règle qui notifie (Notify), ou n'importe quelle règle en échec.
  // Les règles ordinaires (« à 23:00, passer en Nuit ») ne comptent pas.
  const notifiantes = new Set(
    ((state.rules && state.rules.rules) || [])
      .filter((r) => (r.actions || []).some((a) => a.type === "Notify"))
      .map((r) => String(r.id))
  );
  let alerte = null;
  for (const r of state.rule_runs || []) {
    if (r.ok && !notifiantes.has(String(r.rule_id))) continue;
    if (!alerte || r.at > alerte.at) alerte = { at: r.at, texte: r.name, ok: r.ok };
  }
  for (const e of events) {
    const at = e.end || e.start;
    if (!alerte || at > alerte.at) {
      const quoi = CLASSES[(e.classes || [])[0]] || "Détection";
      alerte = { at, texte: quoi + " · " + e.camera_name, ok: true };
    }
  }
  return {
    mode: state.mode || null,
    lumieres: lumieres.length,
    allumees: allumees.length,
    injoignables,
    piles,
    alerte,
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
  const m = MODES[mode] || { label: mode ? mode : "—", symbol: "house", light: "#555555", dark: "#bbbbbb" };
  return { ...m, color: Color.dynamic(new Color(m.light), new Color(m.dark)) };
}

// Colonne « état » (sert au petit et au moyen widget).
function colonneEtat(parent, s, note) {
  const col = parent.addStack();
  col.layoutVertically();

  const top = col.addStack();
  top.centerAlignContent();
  const m = modeInfo(s.mode);
  symbole(top, m.symbol, 14, m.color);
  top.addSpacer(5);
  texte(top, "Maison", 13, C.dim, { bold: true });
  col.addSpacer(4);

  texte(col, s.mode ? m.label : "Mode inconnu", 24, m.color, { bold: true });
  col.addSpacer(4);

  const l = col.addStack();
  l.centerAlignContent();
  symbole(l, s.allumees > 0 ? "lightbulb.fill" : "lightbulb", 12, s.allumees > 0 ? C.warn : C.dim);
  l.addSpacer(4);
  const lt = s.lumieres === 0 ? "Aucune lumière"
    : s.allumees === 0 ? "Tout est éteint"
    : plural(s.allumees, "lumière allumée", "lumières allumées");
  texte(l, lt, 12, C.text);

  if (s.injoignables || s.piles) {
    col.addSpacer(2);
    const parts = [];
    if (s.injoignables) parts.push(plural(s.injoignables, "injoignable", "injoignables"));
    if (s.piles) parts.push(plural(s.piles, "pile faible", "piles faibles"));
    texte(col, "⚠︎ " + parts.join(" · "), 11, C.warn);
  }

  col.addSpacer(4);
  if (s.alerte) {
    texte(col, s.alerte.texte, 11, s.alerte.ok ? C.text : C.bad, { lines: 2 });
    texte(col, heure(s.alerte.at * 1000), 10, C.dim);
  } else {
    texte(col, "Aucune alerte récente", 11, C.dim);
  }
  col.addSpacer();
  texte(col, note, 9, note.startsWith("Hors") ? C.bad : C.dim);
  return col;
}

function bouton(parent, label, symbol, color, url) {
  const b = parent.addStack();
  b.backgroundColor = C.btn;
  b.cornerRadius = 10;
  b.setPadding(6, 8, 6, 8);
  b.centerAlignContent();
  b.url = url;
  symbole(b, symbol, 13, color);
  b.addSpacer(6);
  texte(b, label, 13, C.text, { bold: true });
  b.addSpacer();
  return b;
}

function widgetMessage(titre, detail, url) {
  const w = new ListWidget();
  w.backgroundColor = C.bg;
  w.setPadding(14, 14, 14, 14);
  const top = w.addStack();
  top.centerAlignContent();
  symbole(top, "house", 14, C.dim);
  top.addSpacer(5);
  texte(top, "Maison", 13, C.dim, { bold: true });
  w.addSpacer(8);
  texte(w, titre, 15, C.text, { bold: true, lines: 2 });
  w.addSpacer(4);
  texte(w, detail, 11, C.dim, { lines: 4 });
  w.addSpacer();
  w.url = url;
  return w;
}

function construireWidget(family, s, note) {
  const w = new ListWidget();
  w.backgroundColor = C.bg;
  w.refreshAfterDate = new Date(Date.now() + 5 * 60 * 1000);
  if (family === "medium" || family === "large") {
    w.setPadding(12, 14, 12, 12);
    const row = w.addStack();
    colonneEtat(row, s, note);
    row.addSpacer(10);
    const btns = row.addStack();
    btns.layoutVertically();
    btns.size = new Size(130, 0);
    bouton(btns, "Absent", MODES.absent.symbol, modeInfo("absent").color, shortcutUrl(RACCOURCI_ABSENT));
    btns.addSpacer(6);
    bouton(btns, "Présent", MODES.present.symbol, modeInfo("present").color, shortcutUrl(RACCOURCI_PRESENT));
    btns.addSpacer();
  } else {
    w.setPadding(12, 14, 12, 12);
    colonneEtat(w, s, note);
  }
  // Toucher le fond du widget ouvre l'app web Ostal.
  const base = serveur();
  w.url = base ? base + "/app/" : scriptUrl();
  return w;
}

// ───────── Messages ─────────

function messageErreur(e) {
  if (e instanceof HttpError) {
    if (e.status === 0 && e.message === "pas_installe") return "Widget pas installé : copie-le depuis l'app web (Widgets et raccourcis › Installer le widget).";
    if (e.status === 401) return "Jeton révoqué : recopie le widget depuis l'app web.";
    if (e.status === 426) return "Version du serveur différente : mets à jour Ostal.";
    return "Le serveur a répondu une erreur (" + e.status + (e.message ? ", " + e.message : "") + ").";
  }
  return "Serveur injoignable. Vérifie que Tailscale est activé sur l'iPhone et que le Mac est allumé.";
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
    return { s, note: "À jour à " + heure(s.lu) };
  } catch (e) {
    const cache = lireCache();
    if (e instanceof HttpError && (e.status === 401 || e.status === 0)) return { erreur: e };
    if (cache) return { s: cache, note: "Hors ligne · lu à " + heure(cache.lu) };
    return { erreur: e };
  }
}

async function rendreWidget(family) {
  const r = await etatPourWidget();
  if (r.erreur) {
    const auth = r.erreur instanceof HttpError && (r.erreur.status === 401 || r.erreur.status === 0);
    const base = serveur();
    return widgetMessage(auth ? "Widget à recopier" : "Serveur injoignable", messageErreur(r.erreur), base ? base + "/app/" : scriptUrl());
  }
  return construireWidget(family, r.s, r.note);
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
