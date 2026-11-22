// App web Ostal : appareils en tuiles, températures, caméras. Sans framework, sans build.
import * as api from "./api.js";
import { ICONS, eventIcon, modeIcon, mountIcons } from "./icons.js";
import {
  CLICK_GUARD_MS, EXPECT_TTL_MS, batteryLow, clockText, formatTemperature, groups, isOn, isStale, position,
  statusSummary, tempClass, temperature, tileOf, unreachableSince, watts,
} from "./tiles.js";
import { lineChart, rangeOf } from "./charts.js";
import { mountTemps } from "./temps.js";
import { mountPlan } from "./plan.js";
import { createPlayer, createSnapshot, webCodecsSupported } from "./cameras.js";
import { classesText, createThumbCache, deleteEvent, descriptionText, loadEvents, niveauLabel, playClip, whenText } from "./events.js";
import { DEFAULT_MODE, MODES, cameraOff, effect, isMode, modeChangeNotice, modeLabel, offText } from "./mode.js";
import * as push from "./push.js";
import { kindLabel, lastSeenLabel, sortConnections, viaLabel } from "./connections.js";
import * as presence from "./presence.js";
import { ANDROID_NOTE, SHORTCUT_NAMES, copyLater, isAndroid, myTokens, originInfo, prepareShortcuts, prepareWidget, sinceText, tokenHere } from "./widgets.js";

const $ = (sel) => document.querySelector(sel);
const view = $("#view");

// ---------------------------------------------------------------------------
// État de la maison (le serveur est la source de vérité ; ici, une copie vivante)
// ---------------------------------------------------------------------------
const state = {
  devices: new Map(), // id (texte) -> { id, info, status, last_seen, mode, latest, open_since, cover_motion }
  plan: null,
  cameras: [],
  mode: DEFAULT_MODE, // le mode global de la maison, tenu par le serveur
  hasMode: false, // le serveur a annoncé un mode : sinon la pastille reste cachée
  connection: "connecting", // « connecting », « connected » ou « lost » (voir api.connectEvents)
  loaded: false,
  updatedAt: null, // l'heure du dernier état ou événement reçu (« À jour à 18:42 »)
  lostAt: null, // la liaison perdue après avoir été établie : le bandeau « La maison ne répond pas »
  notice: "", // un changement de mode venu d'ailleurs, à la place de la carte d'état
  presence: null, // GET /api/presence/people : { people, auto_mode, box_link, updated_at } ; null : rien à montrer (pas de box, serveur plus ancien)
  phones: null, // GET /api/wifi/phones : { linked, box_link, mine, free } (noms seulement)
  tokens: null, // GET /api/tokens : mes jetons { mode: { exists, created }, read: { … }, items: [{ id, kind, label, created }], label } (carte « Installer »)
};
const expects = new Map(); // id -> { on, motion, at, acked, req, timer }
let tab = "devices";
let events = null;

function deviceFrom(view) {
  const latest = {};
  for (const r of view.latest ?? []) latest[r.metric] = { value: r.value, at: r.at };
  return {
    id: view.device.node_id, info: view.device, status: view.status, last_seen: view.last_seen ?? null,
    mode: view.mode ?? null, latest, open_since: view.open_since ?? null, cover_motion: view.cover_motion ?? null,
  };
}

function applySnapshot(s) {
  state.devices = new Map(s.devices.map((v) => { const d = deviceFrom(v); return [d.id, d]; }));
  state.plan = s.plan;
  state.cameras = s.cameras ?? [];
  state.hasMode = isMode(s.mode);
  state.mode = state.hasMode ? s.mode : DEFAULT_MODE;
  state.loaded = true;
  state.updatedAt = Date.now();
  scheduleRender(true);
}

function applyEvent(e) {
  state.updatedAt = Date.now();
  const d = (id) => state.devices.get(id);
  switch (e.type) {
    case "device_updated": {
      const old = d(e.device.node_id);
      if (old) old.info = e.device;
      else state.devices.set(e.device.node_id, deviceFrom({ device: e.device, status: { state: "connecting" } }));
      scheduleRender(true);
      return;
    }
    case "device_removed": state.devices.delete(e.node_id); scheduleRender(true); return;
    case "status": if (d(e.node_id)) d(e.node_id).status = e.status; break;
    case "seen": if (d(e.node_id)) d(e.node_id).last_seen = e.at; break;
    case "mode": if (d(e.node_id)) d(e.node_id).mode = e.mode; break;
    case "reading": {
      const dev = d(e.node_id);
      if (!dev) return;
      if (e.metric === "open" && dev.latest.open?.value !== e.value) dev.open_since = e.at;
      dev.latest[e.metric] = { value: e.value, at: e.at };
      break;
    }
    case "cover_motion": if (d(e.node_id)) d(e.node_id).cover_motion = e.motion; break;
    case "plan_changed": state.plan = e.plan; scheduleRender(true); return;
    case "cameras_changed": state.cameras = e.cameras; scheduleRender(true); return;
    case "mode_changed": {
      // Un changement venu d'ailleurs (raccourci, automatisation, autre app) : une notification
      // brève dit d'où il vient. Pas pour celui que cette app vient de demander.
      const notice = e.mode !== state.mode ? modeChangeNotice(e.mode, e.source, modeRequested) : null;
      if (e.mode === modeRequested) modeRequested = null;
      if (isMode(e.mode)) state.mode = e.mode;
      if (notice) showModeNotice(notice);
      scheduleRender(true);
      loadPresence();
      return;
    }
    case "detection": {
      // Un passage commence (« en cours »), se met à jour, se termine, ou reçoit sa description
      // (même `id` à chaque fois) : en tête de liste, sans relancer les vidéos de l'onglet. La
      // capture change à chaque mise à jour d'un passage en cours et à sa fin (pas quand seule
      // la description arrive) : on l'oublie, sinon la vignette resterait périmée.
      const before = detections.list.find((x) => x.id === e.event.id);
      // Même comparaison que le client de bureau : l'état « en cours » ou la fin du passage ont
      // changé.
      if (!before || before.in_progress !== e.event.in_progress || before.end !== e.event.end) thumbs.forget(e.event.id);
      detections.list = [e.event, ...detections.list.filter((x) => x.id !== e.event.id)].slice(0, 60);
      if (tab === "cameras") renderDetections();
      return;
    }
    default: return;
  }
  settleExpect(e.node_id);
  scheduleRender(false);
}

// ---------------------------------------------------------------------------
// Commandes : retour visuel immédiat, jamais de double envoi
// ---------------------------------------------------------------------------
let requestCounter = 0;

function expectSettled(dev, e) {
  return (e.on == null || isOn(dev) === e.on) && (e.motion == null || dev.cover_motion === e.motion);
}

function settleExpect(id) {
  const e = expects.get(id);
  const dev = state.devices.get(id);
  if (e && dev && e.acked && expectSettled(dev, e)) clearExpect(id);
}

function clearExpect(id) {
  const e = expects.get(id);
  if (!e) return;
  clearTimeout(e.timer);
  expects.delete(id);
  scheduleRender(false);
}

function toast(text, role = "alert") {
  const node = document.createElement("div");
  node.className = role === "status" ? "banner info toast" : "banner toast";
  node.setAttribute("role", role);
  node.textContent = text;
  document.body.appendChild(node);
  setTimeout(() => node.remove(), 4000);
}

/** La confirmation d'un geste qu'on ne défait pas : un dialogue [Annuler][action] dont les deux
 *  boutons se partagent la largeur (le dialogue natif ne peut pas nommer ses boutons). Rend une
 *  promesse : `true` si l'action est choisie. Annuler a le focus ; Échap et le fond annulent aussi. */
let confirmDone = null; // ferme le dialogue ouvert avec une réponse
function askConfirm({ title, text, action, plain = false }) {
  if (confirmDone) return Promise.resolve(false);
  return new Promise((resolve) => {
    const box = $("#confirm");
    const opener = document.activeElement;
    $("#confirm-title").textContent = title;
    $("#confirm-text").textContent = text;
    const ok = $("#confirm-ok");
    ok.textContent = action;
    ok.classList.toggle("plain", plain);
    const finish = (answer) => {
      confirmDone = null;
      box.hidden = true;
      $("#sheet").inert = false;
      box.removeEventListener("click", onBackdrop);
      $("#confirm-cancel").removeEventListener("click", onCancel);
      ok.removeEventListener("click", onOk);
      if (opener && document.contains(opener) && !opener.closest("[hidden]")) opener.focus();
      resolve(answer);
    };
    const onCancel = () => finish(false);
    const onOk = () => finish(true);
    const onBackdrop = (e) => { if (e.target === box) finish(false); };
    box.addEventListener("click", onBackdrop);
    $("#confirm-cancel").addEventListener("click", onCancel);
    ok.addEventListener("click", onOk);
    confirmDone = finish;
    $("#sheet").inert = true; // la feuille derrière devient inerte (ni focus ni lecteur d'écran)
    box.hidden = false;
    $("#confirm-cancel").focus();
  });
}

/** Un changement de mode venu d'ailleurs : « Mode Absent · raccourci iPhone ». Sur Appareils, il
 *  prend quelques secondes la place de la carte d'état ; ailleurs, une notification brève. */
let noticeTimer = null;
function showModeNotice(text) {
  if (tab !== "devices") { toast(text, "status"); return; }
  state.notice = text;
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => { state.notice = ""; scheduleRender(false); }, 6000);
}

/** Envoie une commande. Une tuile avec une commande en cours ignore un nouveau tap (ni double
 *  envoi, ni annulation par un double-tap) ; la tuile montre aussitôt l'effet attendu. */
async function sendCommand(dev, kind, command, expectation) {
  const id = dev.id;
  const existing = expects.get(id);
  if (existing && (!existing.acked || Date.now() - existing.at < CLICK_GUARD_MS)) return;
  const e = { ...expectation, at: Date.now(), acked: false, req: ++requestCounter };
  e.timer = setTimeout(() => clearExpect(id), EXPECT_TTL_MS);
  clearTimeout(existing?.timer);
  expects.set(id, e);
  scheduleRender(false);
  try {
    await api.request("POST", `/api/devices/${id}/${kind}`, { command });
    if (expects.get(id)?.req === e.req) { e.acked = true; settleExpect(id); }
  } catch (error) {
    if (expects.get(id)?.req === e.req) clearExpect(id);
    if (error.code !== "unauthorized") toast("La commande n'a pas pu être envoyée.");
  }
}

function actOn(dev, action) {
  const e = expects.get(dev.id);
  const on = e?.on ?? isOn(dev);
  switch (action) {
    case "toggle": return sendCommand(dev, "on-off", on ? "Off" : "On", { on: !on });
    case "cover-open": return sendCommand(dev, "cover", "Open", { motion: "opening" });
    case "cover-close": return sendCommand(dev, "cover", "Close", { motion: "closing" });
    case "cover-stop": return sendCommand(dev, "cover", "Stop", { motion: "stopped" });
    default: return openDetail(dev.id);
  }
}

// ---------------------------------------------------------------------------
// Rendu : appareils
// ---------------------------------------------------------------------------
let renderQueued = false;
let structureDirty = true;
function scheduleRender(structure) {
  structureDirty ||= structure;
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(() => { renderQueued = false; render(); });
}

const make = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
/** Un pictogramme de ICONS (chaîne fixe du code, jamais une donnée de la maison). */
const icon = (name, className = "ico") => {
  const node = make("span", className);
  node.innerHTML = ICONS[name] ?? "";
  return node;
};

function tileElement(id) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "tile";
  b.dataset.id = id;
  b.innerHTML = `<span class="icon"></span><span class="name"></span><span class="value"><span class="ico">${ICONS.blocked}</span><span class="vtext"></span></span><span class="small"></span>`;
  return b;
}

function patchTile(b, dev, bridgeCount) {
  const expect = expects.get(dev.id);
  const live = expect && Date.now() - expect.at < EXPECT_TTL_MS ? expect : null;
  const t = bridgeCount != null
    ? { icon: ICONS.bridge, iconWarn: false, value: `${bridgeCount} appareil${bridgeCount > 1 ? "s" : ""}`, valueWarn: false, small: isStale(dev) ? { kind: "err", text: "injoignable" } : null, lit: false, dim: isStale(dev), action: "detail" }
    : tileOf(dev, live);
  // Injoignable : le mot prend la place de la valeur (orange, avec son icône), pas de seconde ligne.
  const unreachable = t.small?.kind === "err";
  const valueText = unreachable ? t.small.text : (t.value ?? "");
  // Injoignable : « depuis 18:02 » dessous (d'après last_seen), rien si le serveur ne sait pas.
  const since = unreachable && dev.status?.state === "unreachable" ? unreachableSince(dev) : null;
  const small = unreachable ? (since ? { kind: "err", text: since } : null) : t.small;
  b.classList.toggle("lit", t.lit);
  b.classList.toggle("unreach", unreachable);
  b.classList.toggle("pending", Boolean(live) && !live.acked);
  b.dataset.action = t.action;
  const ico = b.querySelector(".icon");
  if (ico._html !== t.icon) { ico.innerHTML = t.icon; ico._html = t.icon; }
  ico.classList.toggle("warn", t.iconWarn);
  const nameEl = b.querySelector(".name");
  nameEl.textContent = dev.info.name;
  b.title = dev.info.name; // le nom complet, quand la ligne le coupe
  b.querySelector(".vtext").textContent = valueText;
  const value = b.querySelector(".value");
  value.classList.toggle("warn", t.valueWarn);
  value.classList.toggle("off", valueText === "Éteinte");
  const note = b.querySelector(".small");
  note.textContent = small?.text ?? "";
  note.className = `small ${small?.kind ?? ""}`;
  b.setAttribute("aria-label", [dev.info.name, valueText, small?.text].filter(Boolean).join(", "));
}

/** La carte d'état : la phrase de statusSummary (jumelle de protocol::status), puis « À jour à ».
 *  Un changement de mode venu d'ailleurs la remplace quelques secondes (« Mode Absent · raccourci iPhone »). */
function patchStatus() {
  const slot = $("#status-slot");
  if (!slot) return;
  slot.hidden = !state.loaded || Boolean(state.lostAt);
  if (slot.hidden) return;
  if (state.notice) {
    if (slot._key !== `notice:${state.notice}`) {
      slot._key = `notice:${state.notice}`;
      const p = make("p", "notice");
      p.setAttribute("role", "status");
      p.append(icon("info"), make("span", "", state.notice));
      slot.replaceChildren(p);
    }
    return;
  }
  const s = statusSummary(state.devices, state.cameras);
  const key = JSON.stringify([s.title, s.detail, s.ok]);
  if (slot._key !== key) {
    slot._key = key;
    const card = make("section", "card");
    card.setAttribute("aria-label", "État de la maison");
    const row = make("div", `state ${s.ok ? "ok" : "warn"}`);
    row.setAttribute("role", "status");
    const text = make("div");
    text.append(make("div", "state-title", s.title), make("div", "state-detail", s.detail));
    row.append(icon(s.ok ? "okCircle" : "alert"), text);
    const time = make("div", "state-time");
    time.id = "state-time";
    text.appendChild(time);
    card.appendChild(row);
    slot.replaceChildren(card);
  }
  const time = $("#state-time");
  if (time && state.updatedAt) time.textContent = `À jour à ${clockText(state.updatedAt)}`;
}

/** La carte « Présence » : qui est à la maison, qui n'y est pas, le mode (« automatique » quand
 *  l'interrupteur du serveur est allumé). Absente sans box reliée ni personne de connue. */
function patchPresence() {
  const slot = $("#presence-slot");
  if (!slot) return;
  const card = state.loaded && !state.lostAt ? presence.presenceCard(state.presence, state.mode) : null;
  slot.hidden = !card;
  if (!card) { slot._key = ""; return; }
  const key = JSON.stringify(card);
  if (slot._key === key) return;
  slot._key = key;
  const section = make("section", "card presence");
  section.setAttribute("aria-label", "Présence à la maison");
  const text = make("div", "presence-text");
  text.appendChild(make("div", "presence-lead", card.lead));
  if (card.away) text.appendChild(make("div", "presence-away", card.away));
  text.appendChild(make("div", "presence-mode", card.mode));
  if (card.note) text.appendChild(make("div", "presence-note", card.note));
  const faces = make("div", "faces");
  for (const p of card.people) {
    const face = make("div", "face");
    const dot = make("span", `avatar${p.home ? "" : " away"}`, p.initial);
    dot.setAttribute("role", "img");
    dot.setAttribute("aria-label", p.aria);
    face.append(dot, make("span", "face-name", p.name));
    faces.appendChild(face);
  }
  section.append(text, faces);
  slot.replaceChildren(section);
}

let presenceTimer = null;
/** Qui est à la maison : à l'ouverture, au retour de l'app, à chaque changement de mode et toutes les
 *  minutes (la box est relevée toutes les deux minutes). Un serveur plus ancien n'a pas la route :
 *  on ne montre rien, sans erreur. */
async function loadPresence() {
  if (!api.hasToken()) return;
  try {
    state.presence = await api.request("GET", "/api/presence/people");
  } catch (error) {
    if (presence.isMissingRoute(error)) state.presence = null;
    // Autre échec (réseau coupé) : on garde ce qu'on savait.
  }
  scheduleRender(false);
}

function startPresence() {
  clearInterval(presenceTimer);
  loadPresence();
  presenceTimer = setInterval(() => { if (!document.hidden) loadPresence(); }, 60_000);
}

function stopPresence() {
  clearInterval(presenceTimer);
  presenceTimer = null;
  state.presence = null;
  state.phones = null;
  closePhonePick();
}

// ---- « Lequel est votre téléphone ? » (première connexion, ou « Mon téléphone ») ----------------------

const pick = { open: false, first: false, choice: "", phones: null, busy: false, error: "", asked: false };

async function loadPhones(refresh = false) {
  try {
    state.phones = await api.request(refresh ? "POST" : "GET", refresh ? "/api/wifi/refresh" : "/api/wifi/phones");
    pick.error = "";
  } catch (error) {
    if (presence.isMissingRoute(error)) state.phones = null;
    else if (refresh) pick.error = "La liste n'a pas pu être actualisée.";
  }
  pick.phones = state.phones;
  scheduleRender(true);
  if (pick.open) renderPhonePick();
  return state.phones;
}

/** Après la connexion : si une box est reliée et que la personne n'a pas encore son téléphone, on
 *  demande une fois par ouverture (et pas avant une semaine si elle a dit « plus tard »). */
async function maybeAskPhone() {
  const phones = await loadPhones();
  if (pick.asked || !presence.shouldAskPhone(phones, presence.readLater())) return;
  pick.asked = true;
  openPhonePick(true);
}

function openPhonePick(first) {
  pick.open = true;
  pick.first = first;
  pick.phones = state.phones;
  pick.choice = state.phones?.mine ? state.phones.mine.toLowerCase() : "";
  pick.error = "";
  closeSheet();
  $("#phone-pick").hidden = false;
  document.documentElement.classList.add("noscroll");
  renderPhonePick();
  $("#pick-body").querySelector("input[type=radio]:checked, input[type=radio]")?.focus();
}

function closePhonePick() {
  pick.open = false;
  const el = $("#phone-pick");
  if (el) el.hidden = true;
  document.documentElement.classList.remove("noscroll");
}

function renderPhonePick() {
  const t = presence.chooserTexts(pick.phones, pick.phones?.name ?? "");
  const list = presence.listWithMine(pick.phones);
  const body = $("#pick-body");
  const keep = body.scrollTop;
  $("#pick-step").hidden = !pick.first;
  $("#pick-cancel").hidden = pick.first;
  body.replaceChildren();
  const head = make("div");
  head.append(make("h1", "pick-title", t.title), make("p", "pick-intro", t.intro));
  const group = make("div", "pick-group");
  group.setAttribute("role", "radiogroup");
  group.setAttribute("aria-labelledby", "pick-list-title");
  const choiceRow = (value, label, selected, className = "") => {
    const row = make("label", `pick-row${selected ? " chosen" : ""} ${className}`.trim());
    const input = make("input");
    input.type = "radio";
    input.name = "tel";
    input.value = value;
    input.checked = selected;
    input.disabled = pick.busy;
    input.addEventListener("change", () => { pick.choice = value; pick.error = ""; renderPhonePick(); });
    row.appendChild(input);
    return [row, input];
  };
  if (list.length) {
    const section = make("section", "pick-list");
    const title = make("h2", "", t.listTitle);
    title.id = "pick-list-title";
    section.appendChild(title);
    for (const name of list) {
      const selected = pick.choice === name.toLowerCase();
      const [row] = choiceRow(name.toLowerCase(), name, selected);
      row.append(icon("phone", "ico pick-ico"), make("span", "pick-name", name));
      if (selected) row.appendChild(make("span", "pick-ok", "✓ choisi"));
      section.appendChild(row);
    }
    group.appendChild(section);
  } else {
    const empty = make("p", "pick-empty", t.empty);
    empty.setAttribute("role", "status");
    empty.id = "pick-list-title";
    group.appendChild(empty);
  }
  const refresh = make("button", "ghost pick-refresh", t.refresh);
  refresh.type = "button";
  refresh.disabled = pick.busy;
  refresh.addEventListener("click", async () => { pick.busy = true; renderPhonePick(); await loadPhones(true); pick.busy = false; renderPhonePick(); });
  (group.querySelector(".pick-list") ?? group).appendChild(refresh);
  if (pick.first) {
    const [laterRow] = choiceRow("later", t.later, pick.choice === "later", "pick-later");
    laterRow.appendChild(make("span", "pick-name", t.later));
    group.appendChild(laterRow);
  }
  const why = make("section", "pick-why");
  why.append(make("h2", "", t.whyTitle), make("p", "", t.why), make("p", "pick-privacy", t.privacy));
  body.append(head, group);
  if (pick.error) { const e = make("p", "error", pick.error); e.setAttribute("role", "alert"); body.appendChild(e); }
  body.appendChild(why);
  body.scrollTop = keep;
  const confirm = $("#pick-confirm");
  const later = pick.choice === "later";
  confirm.textContent = later ? "Continuer sans téléphone" : t.confirm;
  confirm.disabled = pick.busy || !pick.choice || (pick.choice !== "later" && !list.some((n) => n.toLowerCase() === pick.choice));
}

async function confirmPhonePick() {
  if (pick.busy || !pick.choice) return;
  if (pick.choice === "later") {
    presence.writeLater();
    closePhonePick();
    return;
  }
  const name = (presence.listWithMine(pick.phones).find((n) => n.toLowerCase() === pick.choice)) ?? pick.choice;
  if (pick.phones?.mine && pick.phones.mine.toLowerCase() === name.toLowerCase()) { closePhonePick(); return; }
  pick.busy = true;
  pick.error = "";
  renderPhonePick();
  try {
    state.phones = await api.request("PUT", "/api/wifi/phone", { phone: name });
    presence.clearLater();
    pick.busy = false;
    closePhonePick();
    scheduleRender(true);
    loadPresence();
  } catch (error) {
    pick.busy = false;
    pick.error = presence.chooseError(error);
    await loadPhones();
    renderPhonePick();
  }
}

/** Les trois étapes de « Installer sur l'iPhone » : l'état vient des jetons (GET /api/tokens) et de
 *  l'abonnement de cet appareil aux notifications. `null` tant que les jetons ne sont pas connus. */
function installSteps() {
  if (!state.tokens) return null;
  const support = push.pushSupport(pushEnv());
  return [
    { done: Boolean(tokenHere(state.tokens, "mode")), parts: ["Ajouter vos raccourcis : ", ["Je pars", "Je rentre", "Nuit", "Vacances"], "."], open: openWidgets },
    { done: Boolean(tokenHere(state.tokens, "read")), parts: ["Ajouter le ", ["widget"], " à l'écran d'accueil."], open: openWidgets },
    // Un navigateur qui ne sait pas recevoir de notifications (ordinateur, Android sans push) n'a rien à y faire.
    { done: Boolean(pushUi.mine) || support.reason === "unsupported", parts: ["Activer les ", ["notifications"], " sur cet iPhone."], open: openNotifications },
  ];
}

function patchInstall() {
  const slot = $("#install-slot");
  if (!slot) return;
  const steps = state.loaded && !state.lostAt ? installSteps() : null;
  const current = steps?.findIndex((x) => !x.done) ?? -1;
  slot.hidden = current < 0; // tout est fait : la carte disparaît
  if (slot.hidden) { slot._key = null; return; }
  const key = JSON.stringify(steps.map((x) => x.done));
  if (slot._key === key) return;
  slot._key = key;
  const done = steps.filter((x) => x.done).length;
  const card = make("section", "install");
  card.setAttribute("aria-labelledby", "install-title");
  const head = make("div", "install-head");
  const h2 = make("h2", "", "Installer sur l'iPhone");
  h2.id = "install-title";
  head.append(h2, make("span", "install-step", `Étape ${done + 1} sur ${steps.length}`));
  const bar = make("div", "progress");
  bar.setAttribute("aria-hidden", "true");
  steps.forEach((_, i) => bar.appendChild(make("span", i <= done ? "done" : "")));
  const text = make("p");
  for (const part of steps[current].parts) {
    if (Array.isArray(part)) part.forEach((word, i) => { if (i > 0) text.append(i === part.length - 1 ? " et " : ", "); text.appendChild(make("strong", "", word)); });
    else text.append(part);
  }
  const go = make("button", "link-button", "Continuer l'installation");
  go.type = "button";
  go.addEventListener("click", () => steps[current].open());
  card.append(head, bar, text, go);
  slot.replaceChildren(card);
}

function roomElement(key, title, note) {
  const section = make("article", "room");
  section.dataset.room = key;
  const head = make("div", "room-head");
  head.appendChild(make("h3", "", title));
  // La température moyenne de la pièce, juste à côté de son nom ; l'étage vient après.
  const pill = make("span", "pill");
  pill.hidden = true;
  head.appendChild(pill);
  if (note) head.appendChild(make("span", "note", note));
  section.appendChild(head);
  return section;
}

function renderDevices() {
  const layout = groups(state.devices, state.plan);
  const signature = JSON.stringify([layout.groups.map((g) => [g.key, g.title, g.ids]), layout.bridges, state.devices.size === 0]);
  if (structureDirty || view._signature !== signature || view._tab !== "devices") {
    view._signature = signature;
    view._tab = "devices";
    view.replaceChildren();
    const status = make("div");
    status.id = "status-slot";
    const presenceSlot = make("div");
    presenceSlot.id = "presence-slot";
    presenceSlot.hidden = true;
    const install = make("div");
    install.id = "install-slot";
    install.hidden = true;
    const note = make("p", "view-note");
    note.id = "rooms-note";
    note.hidden = true;
    view.append(status, presenceSlot, install, make("h2", "view-title", "Pièces"), note);
    if (state.devices.size === 0) view.appendChild(make("p", "empty", state.loaded ? "Aucun appareil sur ce serveur." : "Chargement…"));
    const rooms = make("div", "rooms");
    rooms.id = "rooms";
    view.appendChild(rooms);
    for (const g of layout.groups) {
      const section = roomElement(g.key, g.title, g.note);
      for (const id of g.ids) section.appendChild(tileElement(id));
      rooms.appendChild(section);
    }
    if (layout.bridges.length) {
      // Les ponts forment un dernier bloc, comme une pièce.
      const section = roomElement("bridges", "Ponts", null);
      delete section.dataset.room;
      section.classList.add("bridges");
      for (const id of layout.bridges) { const t = tileElement(id); t.dataset.bridge = "1"; section.appendChild(t); }
      rooms.appendChild(section);
    }
    if (state.loaded) {
      const out = make("div", "section-title");
      const notif = make("button", "ghost", "Notifications");
      notif.id = "notifications";
      const connections = make("button", "ghost", "Mes connexions");
      connections.id = "connections";
      const myPhone = make("button", "ghost", "Mon téléphone");
      myPhone.id = "my-phone";
      myPhone.hidden = !state.phones?.linked;
      const widgets = make("button", "ghost", "Widgets et raccourcis");
      widgets.id = "widgets";
      const logout = make("button", "ghost", "Se déconnecter");
      logout.id = "logout";
      out.append(notif, connections, myPhone, widgets, logout);
      view.appendChild(out);
    }
    structureDirty = false;
  }
  for (const section of view.querySelectorAll(".room[data-room]")) {
    const avg = layout.averages.get(section.dataset.room);
    const pill = section.querySelector(".pill");
    pill.hidden = avg == null;
    if (avg != null) { pill.textContent = formatTemperature(avg); pill.className = `pill ${tempClass(avg)}`; }
  }
  for (const b of view.querySelectorAll(".tile")) {
    const dev = state.devices.get(b.dataset.id);
    if (!dev) continue;
    const count = b.dataset.bridge
      ? [...state.devices.values()].filter((o) => o.info.bridge?.node_id === dev.id).length
      : null;
    patchTile(b, dev, count);
  }
  // La pièce prend la teinte Lueur s'il y a une lampe allumée, la bordure orange s'il y a un appareil injoignable.
  for (const section of view.querySelectorAll(".room")) {
    section.classList.toggle("lit", Boolean(section.querySelector(".tile.lit")));
    section.classList.toggle("alert", Boolean(section.querySelector(".tile.unreach")));
  }
  // La maison ne répond pas : les cartes restent, atténuées, avec l'heure de la dernière lecture.
  const lost = Boolean(state.lostAt);
  $("#rooms")?.classList.toggle("stale", lost);
  const note = $("#rooms-note");
  if (note) {
    note.hidden = !lost;
    if (lost) note.textContent = `Dernières valeurs connues. Lu à ${clockText(state.updatedAt ?? state.lostAt)}.`;
  }
  patchStatus();
  patchPresence();
  patchInstall();
  if (sheetId) renderSheet();
}

// ---------------------------------------------------------------------------
// Rendu : températures (tous les capteurs sur un seul graphique, voir temps.js)
// ---------------------------------------------------------------------------
const historyCache = new Map(); // id -> { at, points } (courbe 24 h de la fiche d'un appareil)
const HISTORY_TTL_MS = 60_000;

async function loadHistory(id) {
  const cached = historyCache.get(id);
  if (cached && Date.now() - cached.at < HISTORY_TTL_MS) return cached.points;
  const until = Math.floor(Date.now() / 1000);
  const response = await api.request("GET", `/api/devices/${id}/history?since=${until - 86400}&until=${until}&metrics=temperature&buckets=144`);
  const points = (response?.points ?? []).filter((p) => p.metric === "temperature");
  historyCache.set(id, { at: Date.now(), points });
  return points;
}

let temps = null;

function stopTemps() {
  temps?.destroy();
  temps = null;
}

/** Les thermomètres (hors ponts), avec leur pièce et leur état actuel. */
function tempSensors() {
  const out = [];
  for (const g of groups(state.devices, state.plan).groups) {
    for (const id of g.ids) {
      const d = state.devices.get(id);
      if (!d?.info.caps.temperature) continue;
      out.push({
        id, label: d.info.name, room: g.key === "loose" ? null : g.title,
        stale: isStale(d), current: temperature(d), mode: d.mode,
      });
    }
  }
  return out;
}

function renderTemps() {
  const sensors = tempSensors();
  const signature = JSON.stringify(sensors.map((s) => [s.id, s.label, s.room]));
  if (view._tab === "temps" && temps && view._tempSignature === signature) { temps.refreshLive(); return; }
  stopTemps();
  view._tab = "temps";
  view._tempSignature = signature;
  view.replaceChildren();
  if (!sensors.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent = state.loaded ? "Aucun thermomètre sur ce serveur." : "Chargement…";
    view.appendChild(p);
    return;
  }
  temps = mountTemps(view, { request: api.request, getSensors: tempSensors });
}

// ---------------------------------------------------------------------------
// Rendu : plan de la maison (lecture seule ; un tap sur un appareil ouvre la même feuille de
// détail que la page Appareils, voir plan.js)
// ---------------------------------------------------------------------------
let plan = null;

function stopPlan() {
  plan?.destroy();
  plan = null;
}

function renderPlan() {
  if (!plan || view._tab !== "plan") {
    stopPlan();
    view.replaceChildren();
    view._tab = "plan";
    plan = mountPlan(view, {
      getPlan: () => state.plan,
      getDevices: () => state.devices,
      getCameras: () => state.cameras,
      getExpect: (id) => expects.get(id) ?? null,
      get loaded() { return state.loaded; },
      onOpenDevice: (id) => openDetail(id, "half"),
      onOpenCameras: () => selectTab("cameras"),
    });
  } else plan.update();
  if (sheetId) renderSheet();
}

// ---------------------------------------------------------------------------
// Rendu : caméras (vignettes en flux réduit ; un clic passe une vignette en plein écran,
// en flux principal)
// ---------------------------------------------------------------------------
let players = [];
let camCards = []; // { el, cam, canvas, msg, fs, playable, thumb, player, snap, visible }
let thumbObserver = null;
const camSignature = () => JSON.stringify(state.cameras.map((c) => [c.id, c.name, c.status?.state, c.has_sub_stream, cameraOff(c, state.mode)]));
let camsDirty = false; // la grille est à refaire à la sortie du plein écran
let fullCam = null; // { id, card, player, mode: "api" | "css", pushed }
const MAX_THUMBNAILS = 4; // au plus 4 vignettes lues en même temps (décodeurs, WebSockets, batterie)

function stopPlayers() {
  thumbObserver?.disconnect();
  thumbObserver = null;
  players.forEach((p) => p.stop());
  players = [];
  camCards = [];
}

/** Ajuste les vignettes lues : celles qui sont à l'écran, quatre au plus, et aucune pendant un
 *  plein écran (le flux principal prend la place). */
function reconcileCams() {
  const visible = fullCam ? [] : camCards.filter((c) => c.visible);
  const live = new Set(visible.filter((c) => c.thumb).slice(0, MAX_THUMBNAILS));
  // Les vignettes à l'écran qui ne lisent pas de flux (pas de flux réduit, ou au-delà du plafond)
  // montrent l'image fixe du serveur, rafraîchie toutes les 20 s.
  const still = new Set(visible.filter((c) => !live.has(c)));
  // Flux actif : le libellé « En direct » (ou « Image fixe · HH:MM ») sous le nom ; sinon le message au centre.
  const showMsg = (c, st, stillImage) => {
    c.msg.textContent = st.text;
    c.msg.hidden = st.kind === "live";
    c.state.textContent = st.kind !== "live" ? "" : stillImage ? `Image fixe · ${clockText(Date.now())} · Touchez pour ouvrir` : "En direct";
  };
  for (const c of camCards) {
    if (live.has(c) && !c.player) {
      c.snap?.stop();
      players = players.filter((p) => p !== c.snap);
      c.snap = null;
      c.player = createPlayer(c.cam.id, "sub", c.canvas, (st) => showMsg(c, st, false));
      players.push(c.player);
    } else if (!live.has(c) && c.player) {
      c.player.stop();
      players = players.filter((p) => p !== c.player);
      c.player = null;
    }
    if (still.has(c) && !c.snap && !c.player) {
      c.snap = createSnapshot(c.cam.id, c.canvas, (st) => showMsg(c, st, true));
      players.push(c.snap);
    } else if (!still.has(c) && c.snap) {
      c.snap.stop();
      players = players.filter((p) => p !== c.snap);
      c.snap = null;
    }
    if (!c.player && !c.snap) {
      c.state.textContent = "";
      c.msg.hidden = false;
      c.msg.textContent = c.thumb ? "En attente…" : "Touchez pour ouvrir";
    }
  }
}

// ---------------------------------------------------------------------------
// Événements de détection (ticket n° 18), sous les caméras
// ---------------------------------------------------------------------------
const detections = { list: [], status: null, loaded: false, error: "", box: null, at: 0 };
const thumbs = createThumbCache();

/** La liste n'est relue qu'à l'entrée dans l'onglet (au plus une fois par minute) ; ensuite, les
 *  événements `detection` du WebSocket la complètent. */
function maybeRefreshDetections() {
  if (!detections.loaded || Date.now() - detections.at > 60_000) refreshDetections();
}

async function refreshDetections() {
  detections.at = Date.now();
  try {
    const r = await loadEvents();
    if (r === null) { detections.status = null; detections.error = "absent"; }
    else { detections.list = r.events ?? []; detections.status = r.detection ?? null; detections.error = ""; }
  } catch (error) {
    detections.error = error?.detail || "Événements indisponibles pour l'instant";
  }
  detections.loaded = true;
  if (tab === "cameras") renderDetections();
}

function renderDetections() {
  const box = detections.box;
  if (!box || !document.contains(box)) return;
  box.replaceChildren();
  if (detections.error === "absent") return; // serveur sans détection : rien à montrer
  const h = document.createElement("h2");
  h.className = "ev-title";
  h.textContent = "Événements";
  box.appendChild(h);
  const st = detections.status;
  if (st && !st.available) {
    const p = document.createElement("p");
    p.className = "ev-note";
    p.textContent = `Détection ${st.engine || "désactivée"}.`;
    box.appendChild(p);
  } else if (st?.paused) {
    const p = document.createElement("p");
    p.className = "ev-note";
    p.textContent = "Détection suspendue : le serveur est surchargé.";
    box.appendChild(p);
  }
  if (detections.error) {
    const p = document.createElement("p");
    p.className = "ev-note";
    p.textContent = detections.error;
    box.appendChild(p);
  }
  if (!detections.list.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent = detections.loaded ? "Aucun événement ces 7 derniers jours." : "Chargement…";
    box.appendChild(p);
    return;
  }
  thumbs.newRender();
  const ul = document.createElement("ul");
  ul.className = "ev-list";
  for (const ev of detections.list) {
    const li = document.createElement("li");
    const b = document.createElement("button");
    b.type = "button";
    b.className = "ev";
    b.dataset.event = ev.id;
    // La capture, ou à défaut le pictogramme de la classe, sur un fond sombre.
    const thumb = make("span", "thumb");
    thumb.setAttribute("aria-hidden", "true");
    const pic = make("span", "ico");
    pic.innerHTML = eventIcon(ev.classes);
    thumb.appendChild(pic);
    const canvas = document.createElement("canvas");
    canvas.width = 160; canvas.height = 90;
    canvas.hidden = true;
    thumb.appendChild(canvas);
    const text = make("span", "ev-text");
    text.appendChild(make("strong", "", `${classesText(ev)} · ${ev.camera_name}${ev.in_progress ? " · en cours" : ""}`));
    text.appendChild(make("span", "", whenText(ev.start)));
    if (ev.description) text.appendChild(make("span", "ev-desc", descriptionText(ev)));
    const level = niveauLabel(ev.niveau);
    if (level) {
      // Le niveau a toujours son mot et son icône : « normal », « suspect » (orange), « critique » (rouge).
      const row = make("span", `ev-level niv-${ev.niveau}`);
      row.append(icon(ev.niveau === 0 ? "okCircle" : ev.niveau === 1 ? "alert" : "errorX"), level);
      text.appendChild(row);
    }
    b.append(thumb, text);
    b.addEventListener("click", () => openEvent(ev));
    li.appendChild(b);
    ul.appendChild(li);
    thumbs.draw(ev.id, canvas).then((ok) => { if (ok) canvas.hidden = false; });
  }
  box.appendChild(ul);
}

let releaseClip = null;

function openEvent(ev) {
  closeSheet();
  $("#sheet-title").textContent = `${classesText(ev)} · ${ev.camera_name}`;
  const body = $("#sheet-body");
  body.replaceChildren();
  const when = document.createElement("p");
  when.className = "ev-note";
  when.textContent = `${whenText(ev.start)} · score ${Math.round((ev.score ?? 0) * 100)} %`
    + (ev.in_progress ? " · en cours : le clip sera prêt à la fin du passage" : "")
    + (ev.has_clip ? ` · clip ${Math.round(ev.clip_s ?? 0)} s` : "");
  const shot = document.createElement("canvas");
  shot.className = "ev-shot";
  body.append(when, shot);
  if (ev.description) {
    const desc = document.createElement("p");
    desc.className = `ev-description${niveauLabel(ev.niveau) ? ` niv-${ev.niveau}` : ""}`;
    const niveau = niveauLabel(ev.niveau);
    desc.textContent = niveau ? `${descriptionText(ev)} (menace : ${niveau})` : descriptionText(ev);
    body.append(desc);
  }
  thumbs.draw(ev.id, shot).then((ok) => { if (!ok) shot.hidden = true; });
  if (ev.has_clip) {
    const video = document.createElement("video");
    video.className = "ev-clip";
    video.controls = true;
    video.playsInline = true;
    video.muted = true;
    video.hidden = true;
    const load = document.createElement("button");
    load.type = "button";
    load.textContent = "Lire le clip";
    load.addEventListener("click", async () => {
      load.disabled = true;
      load.textContent = "Chargement du clip…";
      try {
        releaseClip?.();
        releaseClip = await playClip(ev.id, video);
        shot.hidden = true;
        video.hidden = false;
        load.remove();
        video.play().catch(() => {});
      } catch (error) {
        load.disabled = false;
        load.textContent = error?.detail ? `Clip indisponible : ${error.detail}` : "Clip indisponible, réessayer";
      }
    });
    body.append(video, load);
  }
  const del = document.createElement("button");
  del.type = "button";
  del.className = "btn danger-btn";
  del.textContent = "Supprimer l'événement";
  del.addEventListener("click", async () => {
    if (!await askConfirm({ title: "Supprimer cet événement ?", text: "Sa capture et son clip seront supprimés aussi.", action: "Supprimer" })) return;
    try {
      await deleteEvent(ev.id);
      detections.list = detections.list.filter((x) => x.id !== ev.id);
      thumbs.forget(ev.id);
      closeSheet();
      renderDetections();
    } catch (error) {
      del.textContent = error?.detail ? `Suppression impossible : ${error.detail}` : "Suppression impossible";
    }
  });
  body.appendChild(del);
  showSheet();
}

function renderCameras() {
  view._tab = "cameras";
  stopPlayers();
  view.replaceChildren();
  if (!webCodecsSupported()) {
    const b = document.createElement("p");
    b.className = "banner";
    b.textContent = "Ce navigateur ne sait pas décoder la vidéo (WebCodecs). Sur iPhone et iPad, il faut iOS 17 ou plus récent.";
    view.appendChild(b);
  }
  detections.box = document.createElement("section");
  detections.box.className = "events";
  detections.box.setAttribute("aria-label", "Événements de détection");
  maybeRefreshDetections();
  if (!state.cameras.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent = state.loaded ? "Aucune caméra sur ce serveur." : "Chargement…";
    view.appendChild(p);
    view.appendChild(detections.box);
    return;
  }
  const grid = document.createElement("div");
  grid.className = "cams";
  for (const cam of state.cameras) {
    const el = document.createElement("div");
    el.className = "cam";
    el.dataset.camera = cam.id;
    const canvas = document.createElement("canvas");
    const msg = document.createElement("div");
    msg.className = "msg";
    const cap = document.createElement("div");
    cap.className = "cap";
    const capName = document.createElement("strong");
    capName.textContent = cam.name;
    const capState = document.createElement("span");
    capState.className = "cap-state";
    cap.append(capName, capState);
    el.append(canvas, msg, cap);
    grid.appendChild(el);
    // Éteinte dans le mode actuel : ni flux ni image fixe (le serveur les refuserait).
    if (cameraOff(cam, state.mode)) { msg.textContent = offText(state.mode); continue; }
    const online = cam.status?.state === "online";
    if (!online) { msg.textContent = cam.status?.state === "unreachable" ? "Caméra injoignable" : "Vérification…"; continue; }
    if (!webCodecsSupported()) { msg.textContent = "Vidéo non prise en charge"; continue; }
    // Lisible : une icône « plein écran » en surimpression, et toute la vignette est cliquable.
    const fs = document.createElement("button");
    fs.type = "button";
    fs.className = "cam-fs";
    fs.setAttribute("aria-label", `${cam.name} : plein écran`);
    fs.innerHTML = ICONS.fullscreenEnter;
    el.appendChild(fs);
    el.classList.add("ok");
    const thumb = Boolean(cam.has_sub_stream);
    msg.textContent = thumb ? "En attente…" : "Touchez pour ouvrir";
    camCards.push({ el, cam, canvas, msg, state: capState, fs, playable: true, thumb, player: null, snap: null, visible: false });
  }
  view.appendChild(grid);
  view.appendChild(detections.box);
  renderDetections();
  if (!camCards.length || !api.hasToken()) return;

  // Seules les vignettes à l'écran sont lues.
  thumbObserver = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      const c = camCards.find((x) => x.el === entry.target);
      if (c) c.visible = entry.isIntersecting;
    }
    reconcileCams();
  }, { rootMargin: "100px" });
  camCards.forEach((c) => thumbObserver.observe(c.el));
}

// ---- Plein écran -----------------------------------------------------------
// L'API Fullscreen sur la vignette (le conteneur, pour que les icônes en surimpression
// restent visibles). Sur iPhone, Safari ne l'offre pas pour un élément qui n'est pas une vidéo :
// repli en plein écran CSS (l'élément devient fixe et couvre tout l'écran). Le bouton retour du
// navigateur et Échap quittent dans les deux cas.
const fsSupported = () => Boolean(document.fullscreenEnabled || document.webkitFullscreenEnabled);
const fsElement = () => document.fullscreenElement || document.webkitFullscreenElement || null;

function requestFs(el) {
  const fn = el.requestFullscreen || el.webkitRequestFullscreen;
  if (!fn) return Promise.reject(new Error("plein écran indisponible"));
  try { return Promise.resolve(fn.call(el)); } catch (error) { return Promise.reject(error); }
}

function leaveFs() {
  const fn = document.exitFullscreen || document.webkitExitFullscreen;
  try { Promise.resolve(fn?.call(document)).catch(() => {}); } catch { /* déjà sorti */ }
}

/** Plein écran CSS : tout ce qui est derrière devient inerte (ni focus, ni lecteur d'écran). */
function setBackgroundInert(card, on) {
  for (let node = card; node && node.id !== "app"; node = node.parentElement) {
    for (const sibling of node.parentElement?.children ?? []) if (sibling !== node) sibling.inert = on;
  }
  for (const sibling of document.body.children) if (sibling.id !== "app" && !sibling.contains(card)) sibling.inert = on;
}

function useCssFullscreen(full) {
  full.mode = "css";
  setBackgroundInert(full.card.el, true);
  full.card.el.classList.add("cssfull");
  document.documentElement.classList.add("noscroll");
}

function enterFull(id) {
  if (fullCam) return;
  const card = camCards.find((c) => c.cam.id === id && c.playable);
  if (!card) return;
  // La demande part tout de suite : le navigateur ne l'accepte que dans le geste de l'utilisateur.
  const pending = fsSupported() ? requestFs(card.el) : null;
  const full = { id, card, player: null, mode: pending ? "api" : "css", pushed: false };
  fullCam = full;
  reconcileCams(); // les vignettes s'arrêtent, le flux principal prend la place
  card.el.classList.add("full");
  card.fs.innerHTML = ICONS.fullscreenExit;
  card.fs.setAttribute("aria-label", `${card.cam.name} : quitter le plein écran`);
  card.msg.hidden = false;
  card.msg.textContent = "Connexion…";
  full.player = createPlayer(card.cam.id, "main", card.canvas, (s) => { card.msg.textContent = s.text; card.msg.hidden = s.kind === "live"; });
  if (pending) pending.catch(() => { if (fullCam === full) useCssFullscreen(full); });
  else useCssFullscreen(full);
  try { history.pushState({ fullCamera: id }, ""); full.pushed = true; } catch { /* sans historique : Échap et le bouton restent */ }
  card.fs.focus({ preventScroll: true });
}

function exitFull() {
  const full = fullCam;
  if (!full) return;
  fullCam = null;
  full.player?.stop();
  if (fsElement()) leaveFs();
  const { card } = full;
  card.el.classList.remove("full", "cssfull");
  setBackgroundInert(card.el, false);
  document.documentElement.classList.remove("noscroll");
  card.fs.innerHTML = ICONS.fullscreenEnter;
  card.fs.setAttribute("aria-label", `${card.cam.name} : plein écran`);
  // Retour au flux réduit (ou au message d'attente pour une caméra sans flux réduit).
  card.msg.hidden = false;
  card.msg.textContent = card.thumb ? "En attente…" : "Touchez pour ouvrir";
  if (full.pushed && history.state?.fullCamera !== undefined) history.back();
  if (camsDirty && tab === "cameras") {
    camsDirty = false;
    renderCameras();
    view._camSignature = camSignature();
  } else if (document.contains(card.el)) { reconcileCams(); card.fs.focus({ preventScroll: true }); }
}

function onFullscreenChange() {
  // Sortie par Échap ou par le geste du système : on revient au flux réduit.
  if (fullCam?.mode === "api" && !fsElement()) exitFull();
}
document.addEventListener("fullscreenchange", onFullscreenChange);
document.addEventListener("webkitfullscreenchange", onFullscreenChange);
window.addEventListener("popstate", () => {
  if (!fullCam) return;
  fullCam.pushed = false; // l'entrée d'historique est déjà retirée
  exitFull();
});

// ---------------------------------------------------------------------------
// Feuille de détail
// ---------------------------------------------------------------------------
let sheetId = null;
let sheetOpener = null;

/** Affiche la feuille : le fond devient inerte (ni focus ni lecteur d'écran), le focus entre
 *  dans la feuille et reviendra à l'élément qui l'a ouverte à la fermeture. */
function showSheet(size = "") {
  if ($("#sheet").hidden) sheetOpener = document.activeElement;
  $("#sheet").classList.toggle("half", size === "half"); // la feuille à demi ouverte (appareil touché sur le plan)
  $("#sheet").classList.toggle("tall", size === "tall"); // Widgets et raccourcis, Notifications : presque tout l'écran
  $("#sheet").scrollTop = 0;
  $("#sheet").hidden = false;
  $("#sheet-backdrop").hidden = false;
  $("#app").inert = true;
  $("#sheet").setAttribute("tabindex", "-1");
  $("#sheet-close").focus();
}
function closeSheet() {
  releaseClip?.();
  releaseClip = null;
  const wasOpen = !$("#sheet").hidden;
  $("#sheet").hidden = true;
  $("#sheet-backdrop").hidden = true;
  $("#app").inert = false;
  sheetId = null;
  pushUi.open = false;
  modeUi.open = false;
  connUi.open = false;
  if (wasOpen) {
    const target = sheetOpener;
    sheetOpener = null;
    if (target && document.contains(target) && !target.closest("[hidden]")) target.focus();
  }
}

function openDetail(id, size = "") {
  if (!state.devices.has(id)) return;
  sheetId = id;
  renderSheet(true);
  showSheet(size);
}

function button(label, onClick, active) {
  const b = document.createElement("button");
  b.type = "button";
  b.textContent = label;
  if (active) b.classList.add("active");
  b.addEventListener("click", onClick);
  return b;
}

/** Le texte de la fiche : la grande valeur, et sous elle la pièce et ce qui se passe. */
function sheetTexts(dev, t) {
  const err = t.small?.kind === "err";
  const parts = [groupTitle(dev.id)];
  if (dev.cover_motion === "opening") parts.push("Ouverture en cours");
  else if (dev.cover_motion === "closing") parts.push("Fermeture en cours");
  if (t.small && !err) parts.push(dev.info.caps.temperature && !t.lowBattery ? `Humidité ${t.small.text}` : t.small.text);
  return {
    value: err ? t.small.text : (t.value ?? ""),
    warn: err || t.valueWarn,
    lit: t.lit,
    sub: parts.filter(Boolean).join(" · "),
  };
}

function renderSheet(rebuild) {
  const dev = state.devices.get(sheetId);
  if (!dev) { closeSheet(); return; }
  const t = tileOf(dev, null);
  const texts = sheetTexts(dev, t);
  $("#sheet-title").textContent = dev.info.name;
  const body = $("#sheet-body");
  if (!rebuild && body.dataset.id === dev.id) {
    // Mise à jour en place : la valeur et l'état des boutons, sans refaire la feuille.
    const v = body.querySelector(".sheet-value");
    v.textContent = texts.value;
    v.hidden = texts.value === "";
    v.classList.toggle("warn", texts.warn);
    v.classList.toggle("lit", texts.lit);
    body.querySelector(".sheet-sub").textContent = texts.sub;
    for (const b of body.querySelectorAll("[data-power]")) b.setAttribute("aria-pressed", String((t.lit || isOn(dev) === true) === (b.dataset.power === "on")));
    return;
  }
  body.dataset.id = dev.id;
  const frag = document.createDocumentFragment();
  const value = document.createElement("div");
  value.className = `sheet-value${texts.warn ? " warn" : ""}${texts.lit ? " lit" : ""}`;
  value.textContent = texts.value;
  value.hidden = texts.value === "";
  const sub = document.createElement("div");
  sub.className = "muted sheet-sub";
  sub.textContent = texts.sub;
  frag.append(value, sub);

  const caps = dev.info.caps;
  const controls = document.createElement("div");
  controls.className = "controls";
  controls.setAttribute("role", "group");
  controls.setAttribute("aria-label", `Commandes de ${dev.info.name}`);
  const stale = isStale(dev);
  if (caps.on_off && !stale) {
    const on = button("Allumer", () => sendCommand(dev, "on-off", "On", { on: true }));
    const off = button("Éteindre", () => sendCommand(dev, "on-off", "Off", { on: false }));
    on.dataset.power = "on";
    off.dataset.power = "off";
    for (const b of [on, off]) b.setAttribute("aria-pressed", String((isOn(dev) === true) === (b.dataset.power === "on")));
    controls.append(on, off);
  }
  if (caps.cover && !stale) {
    controls.append(
      button("Ouvrir", () => sendCommand(dev, "cover", "Open", { motion: "opening" })),
      button("Arrêter", () => sendCommand(dev, "cover", "Stop", { motion: "stopped" })),
      button("Fermer", () => sendCommand(dev, "cover", "Close", { motion: "closing" })),
    );
  }
  if (controls.children.length) frag.appendChild(controls);

  if (caps.temperature) {
    const plot = document.createElement("div");
    frag.appendChild(plot);
    const end = Math.floor(Date.now() / 1000);
    const id = dev.id;
    loadHistory(id).then((points) => {
      if (sheetId !== id) return;
      const series = [{ colorIndex: 1, points: points.map((p) => ({ at: p.at, value: p.value })) }];
      const range = rangeOf(series);
      if (range) {
        const r = document.createElement("div");
        r.className = "range muted";
        r.textContent = `min ${formatTemperature(range.lo)} · max ${formatTemperature(range.hi)} · 24 h`;
        plot.appendChild(r);
      }
      plot.appendChild(lineChart(series, end - 86400, end));
    }).catch(() => { plot.className = "w-note"; plot.textContent = "Historique indisponible."; });
  }

  const details = document.createElement("details");
  const summary = document.createElement("summary");
  summary.textContent = "Informations techniques";
  const dl = document.createElement("dl");
  dl.className = "tech";
  const rows = [
    ["Pièce", groupTitle(dev.id)],
    ["Type", (dev.info.device_types ?? []).join(", ") || null],
    ["Fabricant", dev.info.vendor], ["Produit", dev.info.product],
    ["Nœud", dev.id],
    ["Liaison", dev.status?.state === "online" ? "joignable" : dev.status?.state === "connecting" ? "connexion…" : "injoignable"],
    ["Dernière communication", dev.last_seen ? new Date(dev.last_seen * 1000).toLocaleString("fr-FR") : null],
    ["Pile", dev.latest.battery ? `${Math.round(dev.latest.battery.value)} %${batteryLow(dev) ? " (faible)" : ""}` : batteryLow(dev) ? "faible" : null],
    ["Puissance", dev.latest.power ? watts(dev.latest.power.value) : null],
    ["Position", position(dev) != null ? `${Math.round(position(dev))} %` : null],
  ];
  for (const [k, v] of rows) {
    if (v == null || v === "") continue;
    const dt = document.createElement("dt"); dt.textContent = k;
    const dd = document.createElement("dd"); dd.textContent = v;
    dl.append(dt, dd);
  }
  details.append(summary, dl);
  frag.appendChild(details);
  const via = dev.info.bridge && state.devices.get(dev.info.bridge.node_id);
  if (via) frag.appendChild(make("p", "w-note", `Exposé par ${via.info.name}.`));
  body.replaceChildren(frag);
}

function groupTitle(id) {
  return groups(state.devices, state.plan).groups.find((g) => g.ids.includes(id))?.title ?? null;
}

// ---------------------------------------------------------------------------
// Notifications web push (ticket n° 17) : réglages dans une feuille
// ---------------------------------------------------------------------------
const pushUi = { open: false, registration: null, status: null, mine: null, message: "", busy: false, error: "" };

function pushEnv() {
  return {
    ua: navigator.userAgent,
    touch: navigator.maxTouchPoints > 1,
    standalone: window.matchMedia?.("(display-mode: standalone)").matches || navigator.standalone === true,
    hasNotification: "Notification" in window,
    hasPushManager: "PushManager" in window,
    hasServiceWorker: "serviceWorker" in navigator,
    permission: "Notification" in window ? Notification.permission : "default",
  };
}

/** Sans attente : la demande de permission doit rester la première attente du toucher
 *  (Safari). L'inscription du service worker est chargée à l'ouverture de la feuille. */
function pushDeps() {
  if (!pushUi.registration) throw new Error("Service worker indisponible : rechargez l'app et réessayez.");
  return {
    Notification: window.Notification,
    registration: pushUi.registration,
    request: api.request,
    publicKey: pushUi.status?.public_key ?? "",
    label: push.deviceLabel(navigator.userAgent, navigator.maxTouchPoints > 1),
  };
}

/** L'inscription du service worker, gardée. Au plus 5 s : sans service worker actif, `ready` ne
 *  se résout jamais. */
async function loadRegistration() {
  if (!pushUi.registration) {
    pushUi.registration = await Promise.race([
      navigator.serviceWorker.ready,
      new Promise((resolve) => { setTimeout(() => resolve(null), 5000); }),
    ]);
  }
  return pushUi.registration;
}

/** Au démarrage : l'abonnement de cet appareil, s'il a été renouvelé, repart au serveur. */
async function syncPush() {
  try {
    const permission = "Notification" in window ? Notification.permission : "default";
    const view = await push.syncOnStart(permission, async () => {
      if (!push.pushSupport(pushEnv()).ok) return null;
      pushUi.status = await api.request("GET", "/api/push");
      return (await loadRegistration()) ? pushDeps() : null;
    });
    if (view) pushUi.mine = view;
  } catch { /* serveur ancien ou injoignable : réessayé au prochain démarrage */ }
}

/** Ouvre la feuille : la clé du serveur et l'abonnement de cet appareil sont chargés d'abord
 *  (la demande de permission doit être la première attente du toucher « Activer »). */
async function openNotifications() {
  closeSheet();
  pushUi.open = true;
  $("#sheet-title").textContent = "Notifications";
  pushUi.message = "";
  pushUi.error = "";
  renderPush();
  showSheet("tall");
  try {
    pushUi.status = await api.request("GET", "/api/push");
    const support = push.pushSupport(pushEnv());
    if (support.ok) await loadRegistration();
    pushUi.mine = support.ok && pushUi.registration && Notification.permission === "granted"
      ? await push.currentSubscription(pushDeps()).catch(() => null)
      : null;
    if (pushUi.mine) pushUi.status = await api.request("GET", "/api/push");
  } catch (error) {
    pushUi.error = error?.status === 404 ? "Ce serveur ne gère pas encore les notifications : mettez-le à jour." : (error?.detail || "Réglages indisponibles pour l'instant.");
  }
  if (pushUi.open) renderPush();
}

// ---------------------------------------------------------------------------
// Mes connexions : les téléphones et ordinateurs où le compte est ouvert (GET
// /api/me/installations), avec « Couper ». La connexion de cet appareil n'a pas de bouton.
// ---------------------------------------------------------------------------
const connUi = { open: false, list: null, error: "", busy: false };

async function openConnections() {
  closeSheet();
  connUi.open = true;
  connUi.list = null;
  connUi.error = "";
  $("#sheet-title").textContent = "Mes connexions";
  renderConnections();
  showSheet("tall");
  await loadConnections();
}

async function loadConnections() {
  try {
    connUi.list = (await api.request("GET", "/api/me/installations")).connections;
    connUi.error = "";
  } catch (error) {
    connUi.error = error?.status === 404
      ? "Ce serveur ne gère pas encore les connexions : mettez-le à jour."
      : (error?.detail || "Liste indisponible pour l'instant.");
  }
  if (connUi.open) renderConnections();
}

const THIS_DEVICE = { iphone: "Cet iPhone", ipad: "Cet iPad", android: "Ce téléphone", computer: "Cet ordinateur" };

async function cutConnection(c) {
  const text = "Elle se ferme tout de suite, et les raccourcis et le widget installés depuis elle cessent de marcher. Ce téléphone ou cet ordinateur devra se reconnecter avec son identifiant et son mot de passe.";
  if (!await askConfirm({ title: "Couper cette connexion ?", text, action: "Couper" })) return;
  connUi.busy = true;
  connUi.error = "";
  renderConnections();
  try {
    await api.request("DELETE", `/api/me/installations/${encodeURIComponent(c.id)}`);
  } catch (error) {
    // Déjà coupée (404) : la liste relue le montre. Le reste est dit.
    if (error?.status !== 404) connUi.error = error?.detail || "Échec, réessayez.";
  } finally {
    connUi.busy = false;
  }
  await loadConnections();
}

function renderConnections() {
  const body = $("#sheet-body");
  body.dataset.id = "connections";
  body.replaceChildren();
  body.appendChild(make("p", "ev-note", "Les téléphones et ordinateurs où votre compte est ouvert. Couper une connexion la ferme tout de suite, et les raccourcis et le widget installés depuis elle cessent de marcher : ce téléphone ou cet ordinateur devra se reconnecter avec son identifiant et son mot de passe."));
  if (connUi.error) body.appendChild(make("p", "error", connUi.error));
  if (!connUi.list) { if (!connUi.error) body.appendChild(make("p", "w-note", "Chargement…")); return; }
  body.appendChild(make("h3", "", "Ouvertes sur votre compte"));
  if (!connUi.list.length) body.appendChild(make("p", "w-note", "Aucune connexion ouverte pour l'instant."));
  const ul = make("ul", "conn-list");
  for (const c of sortConnections(connUi.list)) {
    const li = make("li", "conn");
    const who = make("div", "who");
    who.appendChild(make("strong", "", c.name));
    who.appendChild(make("span", "", `${kindLabel(c)} · Dernier accès : ${lastSeenLabel(c.last_seen)}`));
    who.appendChild(make("span", "", `Où : ${viaLabel(c.via)}`));
    li.appendChild(who);
    if (c.current) {
      li.appendChild(make("span", "conn-here", `✓ ${THIS_DEVICE[c.device] ?? "Cet appareil"}`));
    } else {
      const cut = make("button", "btn", "Couper");
      cut.type = "button";
      cut.setAttribute("aria-label", `Couper ${c.name}`);
      cut.disabled = connUi.busy;
      cut.addEventListener("click", () => cutConnection(c));
      li.appendChild(cut);
    }
    ul.appendChild(li);
  }
  body.appendChild(ul);
}

function checkbox(label, checked, onChange) {
  const row = document.createElement("label");
  row.className = "push-check";
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = checked;
  input.addEventListener("change", () => onChange(input.checked, input));
  row.append(input, document.createTextNode(label));
  return row;
}

async function pushAction(work) {
  if (pushUi.busy) return;
  pushUi.busy = true;
  pushUi.error = "";
  pushUi.message = "";
  renderPush();
  try {
    await work();
    pushUi.status = await api.request("GET", "/api/push");
  } catch (error) {
    pushUi.error = error?.detail || error?.message || "Échec, réessayez.";
  } finally {
    pushUi.busy = false;
    if (pushUi.open) renderPush();
  }
}

function renderPush() {
  const body = $("#sheet-body");
  body.dataset.id = "notifications";
  body.replaceChildren();
  const note = (text, cls = "w-note") => { const p = make("p", cls, text); body.appendChild(p); return p; };
  note("Recevez les alertes de la maison sur cet appareil même quand l'app est fermée. Le message part chiffré par le service de notifications d'Apple (ou de votre navigateur) : lui ne peut pas le lire.", "ev-note");
  if (pushUi.error) note(pushUi.error, "error");
  if (pushUi.message) note(pushUi.message);
  const st = pushUi.status;
  if (!st) { if (!pushUi.error) note("Chargement…"); return; }

  // L'interrupteur du serveur : coupé, rien n'est envoyé, à aucun appareil.
  const serverRow = make("div", "switch-row");
  const label = make("span", "", "Notifications activées sur le serveur");
  label.id = "notif-serveur";
  const toggle = make("button", "switch");
  toggle.type = "button";
  toggle.setAttribute("role", "switch");
  toggle.setAttribute("aria-checked", String(st.enabled));
  toggle.setAttribute("aria-labelledby", "notif-serveur");
  toggle.disabled = pushUi.busy;
  toggle.addEventListener("click", () => pushAction(() => api.request("PUT", "/api/push", { enabled: !st.enabled })));
  serverRow.append(label, toggle);
  body.appendChild(serverRow);
  if (pushUi.busy) note("Un instant : une action est en cours.");
  if (!st.enabled) note("Coupées : rien n'est envoyé, à aucun appareil.");

  body.appendChild(make("h3", "", "Cet appareil"));
  const support = push.pushSupport(pushEnv());
  const mine = pushUi.mine && st.subscriptions.find((s) => s.id === pushUi.mine.id);
  if (!support.ok && !mine) {
    if (support.steps) {
      // Dans Safari : d'abord ajouter Ostal à l'écran d'accueil.
      const callout = make("div", "callout");
      const steps = make("ol", "steps");
      support.steps.forEach((text, i) => {
        const li = make("li");
        const text_ = make("div", "step-body");
        text_.appendChild(make("p", "", text));
        li.append(make("span", "step-n now", String(i + 1)), text_);
        steps.appendChild(li);
      });
      const inner = make("div", "step-body");
      inner.append(make("strong", "", "Ajouter Ostal à l'écran d'accueil"), make("p", "", support.text), steps);
      callout.appendChild(inner);
      body.appendChild(callout);
      // Les conditions au-dessus du bouton, grisé tant qu'Ostal n'est pas ouvert depuis l'écran d'accueil.
      note("iOS 16.4 ou plus, et Ostal ajouté à l'écran d'accueil.");
      // La raison du grisé à côté du bouton ; à 390 px, la ligne est trop étroite pour les deux : elle passe dessous.
      const inactive = make("button", "primary", "Activer les notifications sur cet appareil");
      inactive.type = "button";
      inactive.disabled = true;
      const why = make("p", "w-note", "Inactif dans Safari : ouvrez Ostal depuis l'icône de l'écran d'accueil pour l'activer.");
      why.id = "notif-inactif";
      inactive.setAttribute("aria-describedby", why.id);
      const line = make("div", "btn-why");
      line.append(inactive, why);
      body.appendChild(line);
    } else note(support.text, support.reason === "ios-too-old" ? "error" : "w-note");
  } else if (!mine) {
    note("iOS 16.4 ou plus, et Ostal ajouté à l'écran d'accueil.");
    const b = make("button", "primary", "Activer les notifications sur cet appareil");
    b.type = "button";
    b.disabled = pushUi.busy || !st.public_key;
    // La permission est la première chose demandée au toucher (exigence de Safari).
    b.addEventListener("click", () => pushAction(async () => {
      const result = await push.subscribe(pushDeps());
      if (result.status === "denied") throw new Error("Notifications refusées : réautorisez-les dans Réglages › Notifications › Ostal.");
      pushUi.mine = result.view;
      pushUi.message = "Cet appareil est abonné.";
    }));
    body.appendChild(b);
    // Un bouton grisé dit toujours pourquoi.
    if (b.disabled) note(st.public_key ? "Un instant : une action est en cours." : "Le serveur n'a pas encore de clé de notification : redémarrez-le et rouvrez cette feuille.");
  } else {
    for (const c of push.CATEGORIES) {
      body.appendChild(checkbox(c.label, mine.categories[c.id] !== false, (on) => pushAction(() => api.request("PUT", `/api/push/subscriptions/${mine.id}`, { categories: { ...mine.categories, [c.id]: on } }))));
    }
    const row = make("div", "push-actions");
    const test = make("button", "btn", "Envoyer un essai");
    test.type = "button";
    test.disabled = pushUi.busy || !st.enabled;
    test.addEventListener("click", () => pushAction(async () => {
      await api.request("POST", `/api/push/subscriptions/${mine.id}/test`);
      pushUi.message = "Essai envoyé : il arrive dans quelques secondes.";
    }));
    const off = make("button", "btn danger-btn", "Désactiver sur cet appareil");
    off.type = "button";
    off.disabled = pushUi.busy;
    off.addEventListener("click", () => pushAction(async () => {
      await push.unsubscribe(pushDeps(), mine.id);
      pushUi.mine = null;
      pushUi.message = "Cet appareil ne recevra plus rien.";
    }));
    row.append(test, off);
    body.appendChild(row);
    if (test.disabled) note(st.enabled ? "Un instant : une action est en cours." : "Rallumez les notifications du serveur pour envoyer un essai.");
  }

  body.appendChild(make("h3", "", "Appareils abonnés"));
  if (!st.subscriptions.length) note("Aucun.");
  const ul = make("ul", "push-list");
  for (const s of st.subscriptions) {
    const li = make("li");
    const who = make("div", "who");
    who.appendChild(make("strong", "", s.label + (pushUi.mine?.id === s.id ? " (cet appareil)" : "")));
    const parts = [s.service, `depuis le ${push.dateText(s.created_at)}`].filter(Boolean);
    if (s.last_error) parts.push(`dernier envoi : ${s.last_error}`);
    who.appendChild(make("span", "", parts.join(" · ")));
    const revoke = make("button", "btn danger-btn", "Révoquer");
    revoke.type = "button";
    revoke.setAttribute("aria-label", `Révoquer ${s.label}`);
    revoke.disabled = pushUi.busy;
    revoke.addEventListener("click", async () => {
      if (!await askConfirm({ title: `Révoquer « ${s.label} » ?`, text: "Il ne recevra plus de notifications.", action: "Révoquer" })) return;
      pushAction(async () => {
        await api.request("DELETE", `/api/push/subscriptions/${s.id}`);
        if (pushUi.mine?.id === s.id) pushUi.mine = null;
      });
    });
    li.append(who, revoke);
    ul.appendChild(li);
  }
  body.appendChild(ul);
}

// ---------------------------------------------------------------------------
// Widgets et raccourcis : les raccourcis de la personne (« Je pars », « Je rentre », Nuit, Vacances)
// et son widget Scriptable, préparés par le serveur avec l'adresse et un jeton à elle (rien à taper
// sur l'iPhone). Chacun ne voit, n'installe et ne révoque que les siens.
// ---------------------------------------------------------------------------
const widgetsUi = { tokens: null, links: null, busy: "", message: "", error: "" };

async function openWidgets() {
  closeSheet();
  Object.assign(widgetsUi, { tokens: null, links: null, busy: "", message: "", error: "" });
  $("#sheet-title").textContent = "Widgets et raccourcis";
  renderWidgets();
  showSheet("tall");
  await refreshTokens();
}

async function refreshTokens() {
  try {
    widgetsUi.tokens = await api.request("GET", "/api/tokens");
    state.tokens = widgetsUi.tokens; // la carte « Installer sur l'iPhone » suit les jetons
    scheduleRender(false);
  } catch (error) {
    widgetsUi.tokens = null;
    if (error?.status !== 401) widgetsUi.error = error?.message || "État des jetons indisponible.";
  }
  if ($("#sheet-body").dataset.id === "widgets") renderWidgets();
}

/** Repli du presse-papiers (`navigator.clipboard` absent ou refusé) : copie d'une zone cachée. */
function execCopy(text) {
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.className = "offscreen";
  document.body.appendChild(area);
  area.select();
  try { return document.execCommand("copy"); } finally { area.remove(); }
}

const clipboardDeps = () => ({
  write: navigator.clipboard?.write?.bind(navigator.clipboard),
  ClipboardItem: globalThis.ClipboardItem,
  writeText: navigator.clipboard?.writeText?.bind(navigator.clipboard),
  execCopy,
});

async function installShortcuts() {
  if (tokenHere(widgetsUi.tokens, "mode") && !await askConfirm({
    title: "Réinstaller vos raccourcis ?",
    text: "Vos raccourcis déjà installés sur ce téléphone cesseront de marcher : il faut ajouter les nouveaux. Ceux des autres personnes ne changent pas.",
    action: "Continuer",
    plain: true,
  })) return;
  widgetsUi.busy = "shortcuts";
  widgetsUi.error = "";
  widgetsUi.message = "Signature des raccourcis sur le Mac…";
  renderWidgets();
  try {
    widgetsUi.links = await prepareShortcuts(api.request, location.origin);
    widgetsUi.message = "";
  } catch (error) {
    widgetsUi.message = "";
    widgetsUi.error = error?.message || "Raccourcis indisponibles.";
  }
  widgetsUi.busy = "";
  await refreshTokens();
}

async function installWidget() {
  widgetsUi.busy = "widget";
  widgetsUi.error = "";
  widgetsUi.message = "";
  // La copie est confiée au presse-papiers pendant le geste, avant la réponse du serveur.
  const pending = prepareWidget(api.request, location.origin);
  renderWidgets();
  let manual = "";
  try {
    const { result, text } = await copyLater(pending, clipboardDeps());
    if (result === "copied") widgetsUi.message = "Widget copié : collez-le dans Scriptable.";
    else {
      widgetsUi.message = "Copie impossible ici : copiez le texte ci-dessous à la main.";
      manual = text;
    }
  } catch (error) {
    widgetsUi.error = error?.message || "Widget indisponible.";
  }
  widgetsUi.busy = "";
  await refreshTokens();
  if (manual) renderWidgets(manual);
}

/** Révoque un de mes jetons (`/api/tokens/{id}`) : les autres, les miens comme ceux des autres
 *  personnes, ne bougent pas. */
async function revokeToken(token) {
  const shortcuts = token.kind === "mode";
  const ok = await askConfirm(shortcuts
    ? { title: "Révoquer ces raccourcis ?", text: "Les raccourcis et les automatisations de ce téléphone qui l'utilisent ne marcheront plus.", action: "Révoquer" }
    : { title: "Révoquer ce widget ?", text: "Le widget affichera « Jeton révoqué » jusqu'à ce qu'il soit recopié.", action: "Révoquer" });
  if (!ok) return;
  try {
    await api.request("DELETE", `/api/tokens/${encodeURIComponent(token.id)}`);
    if (shortcuts) widgetsUi.links = null;
    widgetsUi.error = "";
    widgetsUi.message = shortcuts ? "Raccourcis révoqués." : "Widget révoqué.";
  } catch (error) {
    widgetsUi.error = error?.message || "Révocation impossible.";
  }
  await refreshTokens();
}

/** Où l'on trouve Scriptable (le lien est écrit ici et non dans index.html : la coquille ne porte aucune adresse externe). */
const SCRIPTABLE_URL = "https://apps.apple.com/app/scriptable/id1405459188";

function renderWidgets(manualText = "") {
  const body = $("#sheet-body");
  body.dataset.id = "widgets";
  body.replaceChildren();
  const note = (text, cls = "w-note") => { const p = make("p", cls, text); body.appendChild(p); return p; };
  const act = (label, cls, onClick, disabled = false) => {
    const b = make("button", cls, label);
    b.type = "button";
    b.disabled = disabled;
    b.addEventListener("click", onClick);
    return b;
  };
  // Une étape numérotée : `state` « now » (à faire), « done » (faite) ou rien (grisée).
  const step = (n, state_, content) => {
    const li = make("li");
    const badge = make("span", `step-n${state_ ? ` ${state_}` : ""}`, state_ === "done" ? "✓" : String(n));
    badge.setAttribute("aria-hidden", "true");
    const inner = make("div", "step-body");
    inner.append(...content);
    li.append(badge, inner);
    return li;
  };
  const status = (parent, text) => {
    const p = make("p", "w-status");
    p.append(icon("okCircle"), make("span", "", text));
    parent.appendChild(p);
  };

  if (widgetsUi.error) note(widgetsUi.error, "error");
  if (widgetsUi.message) note(widgetsUi.message, "ev-note");
  // Android : ni Raccourcis ni Scriptable ; la présence par le Wi-Fi les remplace.
  if (isAndroid(navigator.userAgent)) {
    note(ANDROID_NOTE, "ev-note");
    return;
  }
  const info = originInfo(location);
  const busy = widgetsUi.busy !== "";

  // Les révocations : un seul jeton, un seul bouton ; plusieurs téléphones, une ligne chacun.
  const revokes = (parent, kind, one) => {
    const mine = myTokens(widgetsUi.tokens, kind);
    if (mine.length === 1) {
      parent.appendChild(act(one, "btn danger-btn", () => revokeToken(mine[0]), busy));
    } else if (mine.length > 1) {
      const list = make("ul", "w-tokens");
      for (const token of mine) {
        const here = token === tokenHere(widgetsUi.tokens, kind);
        const li = make("li", "w-token");
        li.appendChild(make("span", "", `${token.label}${here ? " (ce téléphone)" : ""} ${sinceText(token.created)}`.trim()));
        const b = act("Révoquer", "btn danger-btn", () => revokeToken(token), busy);
        b.setAttribute("aria-label", `Révoquer ${token.label}`);
        li.appendChild(b);
        list.appendChild(li);
      }
      parent.appendChild(list);
    }
  };

  // 1. Vos raccourcis : « Je pars » et « Je rentre » disent où vous êtes ; Nuit et Vacances, à la main.
  const modes = make("section", "w-section");
  modes.setAttribute("aria-labelledby", "titre-raccourcis");
  const h3 = make("h3", "", "Vos raccourcis");
  h3.id = "titre-raccourcis";
  modes.appendChild(h3);
  modes.appendChild(make("p", "w-note", "Installés sur cet iPhone, pour vous seul."));
  modes.appendChild(make("p", "w-note", "« Je pars » et « Je rentre » disent où vous êtes et changent le mode de la maison : Absent au départ, Présent au retour (Nuit et Vacances sont respectés). Si l'administrateur a allumé « Changer le mode de la maison selon la présence », la maison ne passe en Absent que quand le dernier est parti."));
  const here = tokenHere(widgetsUi.tokens, "mode");
  if (here) status(modes, `Installés ${sinceText(here.created)}`.trim() + ".");
  modes.appendChild(act(widgetsUi.busy === "shortcuts" ? "Préparation…" : "Installer mes raccourcis", "primary", installShortcuts, busy || !info.usable));
  // Hors adresse .ts.net : le bouton est grisé et la raison est dite juste à côté.
  if (!info.usable) modes.appendChild(make("p", "w-note", info.warning));
  modes.appendChild(make("p", "w-note", "Réinstaller ne touche que vos raccourcis, jamais ceux des autres."));
  const list = make("ol", "steps");
  const link = (name) => {
    const found = widgetsUi.links?.shortcuts.find((x) => x.name === name);
    if (!found) return null;
    const a = make("a", "widgets-link", name);
    a.href = found.url;
    return a;
  };
  const [leaving, arriving, ...manual] = SHORTCUT_NAMES;
  list.appendChild(step(1, widgetsUi.links ? "now" : "", [make("p", "", `Touchez « ${leaving} », puis « Ajouter le raccourci ».`), link(leaving)].filter(Boolean)));
  list.appendChild(step(2, widgetsUi.links ? "now" : "", [make("p", "", `Même chose pour « ${arriving} ».`), link(arriving)].filter(Boolean)));
  list.appendChild(step(3, widgetsUi.links ? "now" : "", [
    make("p", "", "Pour la maison entière, à la main :"),
    ...manual.map(link).filter(Boolean),
    make("p", "w-note", "Ces deux-là changent le mode de toute la maison."),
  ]));
  modes.appendChild(list);
  modes.appendChild(make("p", "w-note", "« Mode absent » et « Mode présent » marchent encore. Vous pouvez les retirer."));
  if (widgetsUi.links) {
    modes.appendChild(make("p", "w-note", `Liens valables ${Math.round(widgetsUi.links.expiresInS / 60)} minutes. Si rien ne se passe, ouvrez cette page dans Safari : le téléchargement peut échouer dans l'app installée sur l'écran d'accueil.`));
  }
  revokes(modes, "mode", "Révoquer mes raccourcis");
  body.appendChild(modes);
  body.appendChild(document.createElement("hr"));

  // 2. Votre widget Scriptable.
  const widget = make("section", "w-section");
  widget.setAttribute("aria-labelledby", "titre-widget");
  const h3w = make("h3", "", "Votre widget");
  h3w.id = "titre-widget";
  widget.appendChild(h3w);
  widget.appendChild(make("p", "w-note", "Relié à votre compte."));
  const store = make("a", "link-button", "Installer Scriptable (App Store, gratuit)");
  store.href = SCRIPTABLE_URL;
  store.target = "_blank";
  store.rel = "noopener noreferrer";
  widget.appendChild(store);
  widget.appendChild(act(widgetsUi.busy === "widget" ? "Préparation…" : "Installer le widget", "primary", installWidget, busy || !info.usable));
  if (!info.usable) widget.appendChild(make("p", "w-note", info.warning));
  const wlist = make("ol", "steps");
  ["Dans Scriptable, touchez « + ».", "Collez le script copié.", "Nommez le script « Ostal ».", "Ajoutez le widget Scriptable à l'écran d'accueil et choisissez le script Ostal."]
    .forEach((text, i) => wlist.appendChild(step(i + 1, i === 0 ? "now" : "", [make("p", "", text)])));
  widget.appendChild(wlist);
  widget.appendChild(make("p", "w-note", "Le widget montre le mode de la maison. Absent et Présent changent tout seuls."));
  revokes(widget, "read", "Révoquer le widget");
  body.appendChild(widget);
  if (manualText) {
    const area = make("textarea", "widgets-script");
    area.value = manualText;
    area.readOnly = true;
    area.setAttribute("aria-label", "Script Ostal à copier");
    body.appendChild(area);
    area.focus();
    area.select();
  }
}

/** Le toucher d'une notification :`#evenement=<id>` ouvre la fiche de l'événement. */
async function openFromUrl(url) {
  const id = push.eventFromHash(new URL(url, location.href).hash);
  if (!id) return;
  history.replaceState(null, "", location.pathname);
  selectTab("cameras");
  try {
    const ev = await api.request("GET", `/api/events/${encodeURIComponent(id)}`);
    if (ev) openEvent(ev);
  } catch {
    toast("Cet événement n'existe plus.");
  }
}

// ---------------------------------------------------------------------------
// Navigation, connexion, démarrage
// ---------------------------------------------------------------------------
const TITLES = { devices: "Appareils", plan: "Plan", temps: "Températures", cameras: "Caméras" };

// ---------------------------------------------------------------------------
// Le mode de la maison : la pastille en haut et l'entrée « Mode » de la barre du bas ouvrent la
// même feuille, avec les quatre choix et ce que chacun fait des caméras
// ---------------------------------------------------------------------------
const modeChip = $("#mode-chip");
const modeTab = $("#tab-mode");
/** Le mode que cette app vient de demander (pas de notification pour lui). */
let modeRequested = null;
const modeUi = { open: false, pending: null };

function renderModeChip() {
  const shown = state.loaded && state.hasMode;
  modeChip.hidden = !shown;
  modeTab.hidden = !shown;
  const label = modeLabel(state.mode);
  const lost = Boolean(state.lostAt);
  const auto = Boolean(state.presence?.auto_mode);
  const key = JSON.stringify([shown, state.mode, lost, auto]);
  if (modeChip._key === key) return;
  modeChip._key = key;
  const present = state.mode === "present";
  const named = presence.modeChipLabel(state.mode, auto);
  const aria = lost ? `${named}. Modifiable quand la maison répond.` : `${named}. Changer de mode`;
  // La pastille : « Mode : Présent » ; les autres modes ont en plus leur pictogramme.
  const pill = make("span", "pill");
  if (!present) { const pic = make("span", "ico"); pic.innerHTML = modeIcon(state.mode); pill.appendChild(pic); }
  pill.append(`Mode : ${label}`);
  modeChip.replaceChildren(pill);
  modeChip.className = `mode-chip${present ? " present" : ""}${lost ? " lost" : ""}`;
  modeChip.setAttribute("aria-label", aria);
  modeChip.setAttribute("aria-disabled", String(lost));
  // L'entrée de la barre du bas : le mode actuel, avec l'icône de ce mode.
  $("#tab-mode-icon").innerHTML = modeIcon(state.mode);
  $("#tab-mode-label").textContent = label;
  modeTab.className = `tab-mode${present ? " present" : ""}${lost ? " lost" : ""}`;
  modeTab.setAttribute("aria-label", aria);
  modeTab.setAttribute("aria-disabled", String(lost));
}

function openModeSheet() {
  if (!state.loaded || !state.hasMode || state.lostAt) return;
  closeSheet();
  modeUi.open = true;
  modeUi.pending = null;
  $("#sheet-title").textContent = "Mode de la maison";
  renderModeSheet();
  showSheet();
}

function renderModeSheet() {
  const body = $("#sheet-body");
  body.dataset.id = "mode";
  const list = make("div", "modes");
  list.setAttribute("role", "group");
  list.setAttribute("aria-label", "Mode de la maison");
  for (const m of MODES) {
    const active = m.id === state.mode;
    const row = make("button", `mode-row ${m.id === "present" ? "present" : "away"}`);
    row.type = "button";
    row.dataset.mode = m.id;
    row.setAttribute("aria-pressed", String(active));
    row.disabled = modeUi.pending !== null;
    const pic = make("span", "ico");
    pic.innerHTML = modeIcon(m.id);
    const text = make("span", "mode-text");
    text.append(make("span", "mode-name", m.label), make("span", "mode-effect", effect(m.id)));
    row.append(pic, text);
    // Le choix ne s'affiche qu'une fois le serveur confirmé : en attendant, « Envoi… » ; ensuite « Actif ».
    if (modeUi.pending === m.id) row.appendChild(make("span", "mode-state", "Envoi…"));
    else if (active) {
      const mark = make("span", "mode-state");
      if (m.id !== "present") mark.appendChild(icon("check"));
      mark.append("Actif");
      row.appendChild(mark);
    }
    row.addEventListener("click", () => chooseMode(m.id));
    list.appendChild(row);
  }
  body.replaceChildren(list);
}

/** Demande le changement au serveur. La feuille suit sa réponse (et l'événement `mode_changed`
 *  qu'il diffuse à tous) : rien n'est affiché avant sa confirmation. */
async function chooseMode(id) {
  if (modeUi.pending || id === state.mode) return;
  modeRequested = id;
  modeUi.pending = id;
  renderModeSheet();
  try {
    const result = await api.request("PUT", "/api/mode", { mode: id, source: { kind: "web" } });
    if (isMode(result?.mode)) { state.mode = result.mode; scheduleRender(true); }
  } catch (error) {
    modeRequested = null;
    toast(error.detail || "Le mode n'a pas pu être changé.");
  }
  modeUi.pending = null;
  if (modeUi.open) renderModeSheet();
}

modeChip.addEventListener("click", openModeSheet);
modeTab.addEventListener("click", openModeSheet);

/** « La maison ne répond pas » : le bandeau, sous l'en-tête, sur tous les onglets. Les dernières
 *  valeurs restent à l'écran (atténuées sur Appareils). */
function renderLost() {
  const lost = state.loaded && Boolean(state.lostAt);
  $("#lost").hidden = !lost;
  const retrying = lost && state.connection === "connecting";
  const retry = $("#retry");
  retry.textContent = retrying ? "Nouvelle tentative…" : "Réessayer";
  retry.disabled = retrying;
}

function render() {
  renderModeChip();
  renderLost();
  if (modeUi.open && !modeUi.pending) renderModeSheet();
  if (tab === "devices") renderDevices();
  else if (tab === "plan") {
    structureDirty = false;
    renderPlan();
  } else if (tab === "temps") {
    structureDirty = false;
    renderTemps();
  } else if (tab === "cameras") {
    const signature = camSignature();
    if (fullCam && structureDirty && view._camSignature !== signature) {
      // En plein écran, le changement d'une autre caméra attend la sortie ; seule la disparition
      // ou la panne de la caméra affichée fait sortir tout de suite.
      structureDirty = false;
      const shown = state.cameras.find((c) => c.id === fullCam.id);
      if (!shown || shown.status?.state !== "online" || cameraOff(shown, state.mode)) { exitFull(); renderCameras(); view._camSignature = signature; camsDirty = false; }
      else camsDirty = true;
    } else if (view._tab !== "cameras" || (structureDirty && view._camSignature !== signature)) { structureDirty = false; exitFull(); renderCameras(); view._camSignature = signature; camsDirty = false; }
    else structureDirty = false;
  }
}

function selectTab(name) {
  if (tab === name && view._tab === name) return;
  tab = name;
  if (name !== "cameras") { exitFull(); stopPlayers(); }
  if (name !== "temps") stopTemps();
  if (name !== "plan") stopPlan();
  $("#title").textContent = TITLES[name];
  document.querySelectorAll("#tabs [data-tab]").forEach((b) => {
    if (b.dataset.tab === name) b.setAttribute("aria-current", "page"); else b.removeAttribute("aria-current");
  });
  view._tab = null;
  structureDirty = true;
  window.scrollTo(0, 0);
  render();
}

/** Le message d'erreur sous le mot de passe (avec son icône), ou rien. */
function setLoginError(text) {
  $("#login-error").hidden = !text;
  $("#login-error-text").textContent = text ?? "";
  $("#login-password").setAttribute("aria-invalid", text ? "true" : "false");
}

/** `info` : une information qui prend la place de la phrase d'accueil (« Session expirée »). */
function showLogin(message, info = false) {
  events?.close();
  events = null;
  confirmDone?.(false);
  closeSheet();
  exitFull();
  stopPlayers();
  stopTemps();
  stopPlan();
  $("#login").hidden = false;
  $("#app").hidden = true;
  $("#topbar").hidden = true;
  $("#tabs").hidden = true;
  $("#login-info").hidden = !(info && message);
  $("#login-info-text").textContent = info ? (message ?? "") : "";
  $("#login-intro").hidden = Boolean(info && message);
  setLoginError(info ? "" : message);
  state.devices.clear();
  state.loaded = false;
  state.updatedAt = null;
  state.lostAt = null;
  state.tokens = null;
  state.notice = "";
  stopPresence();
  pick.asked = false;
  pushUi.mine = null;
  $("#lost").hidden = true;
  historyCache.clear();
}

function start() {
  $("#login").hidden = true;
  $("#app").hidden = false;
  $("#topbar").hidden = false;
  $("#tabs").hidden = false;
  mountIcons();
  selectTab(tab);
  events = api.connectEvents({
    onSnapshot: applySnapshot,
    onEvent: applyEvent,
    onStatus: (s) => {
      state.connection = s;
      // La liaison perdue après avoir été établie : le bandeau reste, sans clignoter à chaque nouvel essai.
      if (s === "connected") state.lostAt = null;
      else if (s === "lost" && state.loaded && !state.lostAt) state.lostAt = Date.now();
      scheduleRender(false);
    },
  });
  loadTokens();
  startPresence();
  maybeAskPhone();
  // Ouverte depuis une notification (`/app/#evenement=…`) : la fiche de l'événement.
  if (location.hash) openFromUrl(location.href);
  syncPush();
}

/** Les jetons (mode et lecture) : l'état de la carte « Installer sur l'iPhone ». Un serveur plus
 *  ancien ou injoignable : pas de carte. */
async function loadTokens() {
  try {
    state.tokens = await api.request("GET", "/api/tokens");
  } catch { state.tokens = null; }
  scheduleRender(false);
}

function loginMessage(error) {
  switch (error.code) {
    case "invalid_credentials": return "Identifiant ou mot de passe incorrect.";
    case "too_many_attempts": return `Trop d'essais. Réessayez dans ${error.retryAfter ?? 30} s.`;
    case "version_incompatible": return "Le serveur et l'app ne parlent pas la même version. Mettez le serveur à jour.";
    case "network": return `${api.HOUSE_SILENT} ${api.HOUSE_HINT}`;
    default: return "Connexion impossible.";
  }
}

$("#login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const formEl = event.currentTarget;
  const form = new FormData(formEl);
  const submit = formEl.querySelector("button[type=submit]");
  if (submit.disabled) return; // pas de double envoi
  submit.disabled = true;
  // Dès la première tentative, « Session expirée » s'efface : la phrase d'accueil revient, et c'est
  // le résultat de cette tentative (ou rien) qui s'affiche, jamais les deux messages ensemble.
  $("#login-info").hidden = true;
  $("#login-intro").hidden = false;
  setLoginError("");
  try {
    await api.login(String(form.get("username")), String(form.get("password")));
    formEl.reset();
    setLoginError("");
    submit.disabled = false;
    start();
  } catch (error) {
    console.error(error);
    setLoginError(loginMessage(error));
    if (error.code === "invalid_credentials") { formEl.elements.password.value = ""; formEl.elements.password.focus(); }
  } finally {
    submit.disabled = false;
  }
});

api.onUnauthorized(() => showLogin("Session expirée : reconnectez-vous.", true));

$("#tabs").addEventListener("click", (event) => {
  const b = event.target.closest("[data-tab]");
  if (b) selectTab(b.dataset.tab);
});
$("#lost-title-text").textContent = api.HOUSE_SILENT;
$("#lost-hint").textContent = api.HOUSE_HINT;
$("#retry").addEventListener("click", () => { state.connection = "connecting"; renderLost(); events?.restart(); });
$("#sheet-close").addEventListener("click", closeSheet);
$("#pick-confirm").addEventListener("click", confirmPhonePick);
$("#pick-cancel").addEventListener("click", closePhonePick);
$("#sheet-backdrop").addEventListener("click", closeSheet);
document.addEventListener("keydown", (e) => {
  if (confirmDone) {
    // La confirmation est au premier plan : Échap l'annule, Tab reste entre ses deux boutons.
    if (e.key === "Escape") { e.preventDefault(); confirmDone(false); }
    else if (e.key === "Tab") {
      const first = $("#confirm-cancel");
      const last = $("#confirm-ok");
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
    return;
  }
  if (e.key === "Escape" && fullCam?.mode === "css") { exitFull(); return; }
  if ($("#sheet").hidden) return;
  if (e.key === "Escape") { closeSheet(); return; }
  if (e.key !== "Tab") return;
  const focusable = [...$("#sheet").querySelectorAll("button, summary, [href], input, [tabindex]:not([tabindex='-1'])")].filter((n) => !n.disabled && n.offsetParent !== null);
  if (!focusable.length) return;
  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  if (e.shiftKey && (document.activeElement === first || document.activeElement === $("#sheet"))) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
});

// Tap : commande directe (lumière, volet) ou détail ; appui long : toujours le détail.
let pressTimer = null;
let longPressed = false;
view.addEventListener("pointerdown", (e) => {
  const tile = e.target.closest(".tile");
  if (!tile) return;
  longPressed = false;
  clearTimeout(pressTimer);
  pressTimer = setTimeout(() => { longPressed = true; navigator.vibrate?.(10); openDetail(tile.dataset.id); }, 500);
});
for (const type of ["pointerup", "pointercancel", "pointerleave", "pointermove"]) {
  view.addEventListener(type, (e) => { if (type !== "pointermove" || Math.abs(e.movementX) + Math.abs(e.movementY) > 6) clearTimeout(pressTimer); });
}
view.addEventListener("contextmenu", (e) => {
  const tile = e.target.closest(".tile");
  if (!tile) return;
  e.preventDefault();
  clearTimeout(pressTimer);
  openDetail(tile.dataset.id);
});
view.addEventListener("click", (e) => {
  if (e.target.closest("#logout")) { api.logout().then(() => showLogin()); return; }
  if (e.target.closest("#notifications")) { openNotifications(); return; }
  if (e.target.closest("#connections")) { openConnections(); return; }
  if (e.target.closest("#my-phone")) { loadPhones().then(() => openPhonePick(false)); return; }
  if (e.target.closest("#widgets")) { openWidgets(); return; }
  const cam = e.target.closest(".cam");
  if (cam) {
    if (fullCam) { if (e.target.closest(".cam-fs")) exitFull(); } else if (cam.classList.contains("ok")) enterFull(cam.dataset.camera);
    return;
  }
  const tile = e.target.closest(".tile");
  if (!tile) return;
  if (longPressed) { longPressed = false; return; }
  const dev = state.devices.get(tile.dataset.id);
  if (dev) actOn(dev, tile.dataset.action);
});

// Réveil de l'appareil : après plus de 15 s cachée, la connexion est probablement morte sans
// que le navigateur le sache ; on repart d'une connexion neuve. Les caméras s'arrêtent tant que
// la page est cachée et reprennent au retour.
let hiddenAt = null;
function wake() {
  if (!api.hasToken()) return;
  events?.restart();
  loadPresence();
  if (tab === "cameras") renderCameras();
}
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    hiddenAt = Date.now();
    exitFull();
    stopPlayers();
  } else if (hiddenAt !== null) {
    const away = Date.now() - hiddenAt;
    hiddenAt = null;
    if (away > 15_000) wake();
    else {
      events?.reconnectNow();
      if (tab === "cameras") renderCameras();
    }
  }
});
window.addEventListener("pageshow", (e) => { if (e.persisted) wake(); });
window.addEventListener("online", () => { if (!document.hidden) wake(); });
setInterval(() => { if ((tab === "devices" || tab === "plan") && !document.hidden) scheduleRender(false); }, 30_000);

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("sw.js", { scope: "/app/" }).catch(() => {});
  // Le toucher d'une notification alors que l'app est déjà ouverte (voir sw.js).
  navigator.serviceWorker.addEventListener("message", (e) => {
    if (e.data?.type === "open" && typeof e.data.url === "string" && api.hasToken()) openFromUrl(e.data.url);
  });
}

mountIcons(); // les pictogrammes de l'écran de connexion (l'erreur, l'information)
if (api.hasToken()) start();
else showLogin();
