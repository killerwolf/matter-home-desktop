// App web Matter Home : appareils en tuiles, températures, caméras. Sans framework, sans build.
import * as api from "./api.js";
import { ICONS, mountIcons } from "./icons.js";
import {
  CLICK_GUARD_MS, EXPECT_TTL_MS, batteryLow, formatTemperature, groups, isOn, isStale, position,
  tempClass, temperature, tileOf, watts,
} from "./tiles.js";
import { lineChart, rangeOf } from "./charts.js";
import { createPlayer, webCodecsSupported } from "./cameras.js";

const $ = (sel) => document.querySelector(sel);
const view = $("#view");

// ---------------------------------------------------------------------------
// État de la maison (le serveur est la source de vérité ; ici, une copie vivante)
// ---------------------------------------------------------------------------
const state = {
  devices: new Map(), // id (texte) -> { id, info, status, last_seen, mode, latest, open_since, cover_motion }
  plan: null,
  cameras: [],
  connection: "connecting",
  loaded: false,
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
  state.loaded = true;
  scheduleRender(true);
}

function applyEvent(e) {
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
    case "cameras_changed": state.cameras = e.cameras; if (tab === "cameras") scheduleRender(true); return;
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

function toast(text) {
  const node = document.createElement("div");
  node.className = "banner toast";
  node.setAttribute("role", "alert");
  node.textContent = text;
  document.body.appendChild(node);
  setTimeout(() => node.remove(), 4000);
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

function tileElement(id) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "tile";
  b.dataset.id = id;
  b.innerHTML = '<span class="icon"></span><span class="name"></span><span class="bottom"><span class="value"></span><span class="small"></span></span>';
  return b;
}

function patchTile(b, dev, bridgeCount) {
  const expect = expects.get(dev.id);
  const live = expect && Date.now() - expect.at < EXPECT_TTL_MS ? expect : null;
  const t = bridgeCount != null
    ? { icon: ICONS.bridge, iconWarn: false, value: `${bridgeCount} appareil${bridgeCount > 1 ? "s" : ""}`, valueWarn: false, small: isStale(dev) ? { kind: "err", text: "injoignable" } : null, lit: false, dim: isStale(dev), action: "detail" }
    : tileOf(dev, live);
  b.classList.toggle("lit", t.lit);
  b.classList.toggle("dim", t.dim);
  b.classList.toggle("pending", Boolean(live) && !live.acked);
  b.dataset.action = t.action;
  const icon = b.querySelector(".icon");
  if (icon._html !== t.icon) { icon.innerHTML = t.icon; icon._html = t.icon; }
  icon.classList.toggle("warn", t.iconWarn);
  b.querySelector(".name").textContent = dev.info.name;
  const value = b.querySelector(".value");
  value.textContent = t.value ?? "";
  value.classList.toggle("warn", t.valueWarn);
  const small = b.querySelector(".small");
  small.textContent = t.small?.text ?? "";
  small.className = `small ${t.small?.kind ?? ""}`;
  b.setAttribute("aria-label", [dev.info.name, t.value, t.small?.text].filter(Boolean).join(", "));
}

function renderDevices() {
  const layout = groups(state.devices, state.plan);
  const signature = JSON.stringify([layout.groups.map((g) => [g.key, g.title, g.ids]), layout.bridges, state.devices.size === 0]);
  if (structureDirty || view._signature !== signature || view._tab !== "devices") {
    view._signature = signature;
    view._tab = "devices";
    view.replaceChildren();
    if (state.devices.size === 0) {
      const empty = document.createElement("p");
      empty.className = "empty";
      empty.textContent = state.loaded ? "Aucun appareil sur ce serveur." : "Chargement…";
      view.appendChild(empty);
    }
    for (const g of layout.groups) {
      const section = document.createElement("section");
      section.className = "room";
      section.dataset.room = g.key;
      const head = document.createElement("div");
      head.className = "room-head";
      const h2 = document.createElement("h2");
      h2.textContent = g.title;
      head.appendChild(h2);
      if (g.note) { const n = document.createElement("span"); n.className = "note"; n.textContent = g.note; head.appendChild(n); }
      const pill = document.createElement("span");
      pill.className = "pill";
      pill.hidden = true;
      head.appendChild(pill);
      section.appendChild(head);
      const grid = document.createElement("div");
      grid.className = "tiles";
      for (const id of g.ids) grid.appendChild(tileElement(id));
      section.appendChild(grid);
      view.appendChild(section);
    }
    if (layout.bridges.length) {
      const h = document.createElement("h2");
      h.className = "section-title";
      h.textContent = "Ponts";
      const grid = document.createElement("div");
      grid.className = "tiles bridges";
      for (const id of layout.bridges) { const t = tileElement(id); t.dataset.bridge = "1"; grid.appendChild(t); }
      view.append(h, grid);
    }
    if (state.loaded) {
      const out = document.createElement("div");
      out.className = "section-title";
      const button = document.createElement("button");
      button.className = "ghost";
      button.id = "logout";
      button.textContent = "Se déconnecter";
      out.appendChild(button);
      view.appendChild(out);
    }
    structureDirty = false;
  }
  for (const section of view.querySelectorAll(".room")) {
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
  if (sheetId) renderSheet();
}

// ---------------------------------------------------------------------------
// Rendu : températures (une courbe par pièce sur 24 h)
// ---------------------------------------------------------------------------
const history = new Map(); // id -> { at, points }
const HISTORY_TTL_MS = 60_000;

async function loadHistory(id) {
  const cached = history.get(id);
  if (cached && Date.now() - cached.at < HISTORY_TTL_MS) return cached.points;
  const until = Math.floor(Date.now() / 1000);
  const response = await api.request("GET", `/api/devices/${id}/history?since=${until - 86400}&until=${until}&metrics=temperature&buckets=144`);
  const points = (response?.points ?? []).filter((p) => p.metric === "temperature");
  history.set(id, { at: Date.now(), points });
  return points;
}

function temperatureCards() {
  const layout = groups(state.devices, state.plan);
  const cards = [];
  for (const g of layout.groups) {
    const sensors = g.ids.map((id) => state.devices.get(id)).filter((d) => d.info.caps.temperature);
    if (!sensors.length) continue;
    cards.push({ key: g.key, title: g.title, average: layout.averages.get(g.key) ?? null, sensors });
  }
  return cards;
}

async function renderTemps() {
  view._tab = "temps";
  const cards = temperatureCards();
  view.replaceChildren();
  if (!cards.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent = state.loaded ? "Aucun thermomètre sur ce serveur." : "Chargement…";
    view.appendChild(p);
    return;
  }
  const token = ++tempRenderToken;
  const slots = cards.map((c) => {
    const card = document.createElement("section");
    card.className = "card";
    card.innerHTML = '<div class="head"><h2></h2><span class="big"></span></div><div class="range muted"></div><div class="plot"></div>';
    card.querySelector("h2").textContent = c.title;
    view.appendChild(card);
    return card;
  });
  const end = Math.floor(Date.now() / 1000);
  await Promise.all(cards.map(async (c, i) => {
    let series = [];
    try {
      series = await Promise.all(c.sensors.map(async (d, k) => ({ id: d.id, name: d.info.name, colorIndex: (k % 8) + 1, points: (await loadHistory(d.id)).map((p) => ({ at: p.at, value: p.value })) })));
    } catch { series = null; }
    if (token !== tempRenderToken) return;
    const card = slots[i];
    const big = card.querySelector(".big");
    const current = c.average ?? (c.sensors[0] && !isStale(c.sensors[0]) ? temperature(c.sensors[0]) : null);
    big.textContent = current == null ? "—" : formatTemperature(current);
    const plot = card.querySelector(".plot");
    if (!series) { plot.textContent = "Historique indisponible."; return; }
    const range = rangeOf(series);
    card.querySelector(".range").textContent = range
      ? `min ${formatTemperature(range.lo)} · max ${formatTemperature(range.hi)} · 24 h`
      : "Aucune mesure sur 24 h";
    plot.replaceChildren(lineChart(series, end - 86400, end));
    if (c.sensors.length > 1) {
      const legend = document.createElement("div");
      legend.className = "range";
      legend.textContent = series.map((s) => s.name).join(" · ");
      plot.appendChild(legend);
    }
  }));
}
let tempRenderToken = 0;

// ---------------------------------------------------------------------------
// Rendu : caméras
// ---------------------------------------------------------------------------
let players = [];
function stopPlayers() { players.forEach((p) => p.stop()); players = []; }

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
  if (!state.cameras.length) {
    const p = document.createElement("p");
    p.className = "empty";
    p.textContent = state.loaded ? "Aucune caméra sur ce serveur." : "Chargement…";
    view.appendChild(p);
    return;
  }
  const grid = document.createElement("div");
  grid.className = "cams";
  for (const cam of state.cameras) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "cam";
    b.dataset.camera = cam.id;
    const canvas = document.createElement("canvas");
    const msg = document.createElement("div");
    msg.className = "msg";
    const cap = document.createElement("div");
    cap.className = "cap";
    cap.textContent = cam.name;
    b.append(canvas, msg, cap);
    grid.appendChild(b);
    const online = cam.status?.state === "online";
    if (!online) { msg.textContent = cam.status?.state === "unreachable" ? "Caméra injoignable" : "Vérification…"; continue; }
    if (!webCodecsSupported()) { msg.textContent = "Vidéo non prise en charge"; continue; }
    if (!cam.has_sub_stream) { msg.textContent = "Touchez pour ouvrir"; continue; }
    players.push(createPlayer(cam.id, "sub", canvas, (s) => { msg.textContent = s.text; msg.hidden = s.kind === "live"; }));
  }
  view.appendChild(grid);
}

let bigPlayer = null;
function openCamera(id) {
  const cam = state.cameras.find((c) => c.id === id);
  if (!cam) return;
  stopPlayers();
  const box = document.createElement("div");
  box.className = "bigcam";
  const canvas = document.createElement("canvas");
  const msg = document.createElement("div");
  msg.className = "msg";
  box.append(canvas, msg);
  if (!webCodecsSupported()) msg.textContent = "Ce navigateur ne sait pas décoder la vidéo (WebCodecs).";
  else if (cam.status?.state !== "online") msg.textContent = "Caméra injoignable";
  else bigPlayer = createPlayer(cam.id, "main", canvas, (s) => { msg.textContent = s.text; msg.hidden = s.kind === "live"; });
  openSheet(cam.name, box, () => { bigPlayer?.stop(); bigPlayer = null; if (tab === "cameras") renderCameras(); });
}

// ---------------------------------------------------------------------------
// Feuille de détail
// ---------------------------------------------------------------------------
let sheetId = null;
let sheetOnClose = null;

function openSheet(title, content, onClose) {
  sheetId = null;
  sheetOnClose = onClose ?? null;
  $("#sheet-title").textContent = title;
  $("#sheet-body").replaceChildren(content);
  showSheet();
}
function showSheet() { $("#sheet").hidden = false; $("#sheet-backdrop").hidden = false; }
function closeSheet() {
  $("#sheet").hidden = true;
  $("#sheet-backdrop").hidden = true;
  sheetId = null;
  const cb = sheetOnClose;
  sheetOnClose = null;
  cb?.();
}

function openDetail(id) {
  if (!state.devices.has(id)) return;
  sheetId = id;
  sheetOnClose = null;
  renderSheet(true);
  showSheet();
}

function button(label, onClick, active) {
  const b = document.createElement("button");
  b.type = "button";
  b.textContent = label;
  if (active) b.classList.add("active");
  b.addEventListener("click", onClick);
  return b;
}

function renderSheet(rebuild) {
  const dev = state.devices.get(sheetId);
  if (!dev) { closeSheet(); return; }
  const t = tileOf(dev, null);
  $("#sheet-title").textContent = dev.info.name;
  const body = $("#sheet-body");
  if (!rebuild && body.dataset.id === dev.id) {
    // Mise à jour en place : la valeur et l'état des boutons, sans refaire la feuille.
    body.querySelector(".sheet-value").textContent = t.value ?? "—";
    body.querySelector(".sheet-value").classList.toggle("warn", t.valueWarn);
    body.querySelector(".sheet-sub").textContent = t.small?.text ?? "";
    return;
  }
  body.dataset.id = dev.id;
  const frag = document.createDocumentFragment();
  const value = document.createElement("div");
  value.className = `sheet-value${t.valueWarn ? " warn" : ""}`;
  value.textContent = t.value ?? "—";
  const sub = document.createElement("div");
  sub.className = "muted sheet-sub";
  sub.textContent = t.small?.text ?? "";
  frag.append(value, sub);

  const caps = dev.info.caps;
  const controls = document.createElement("div");
  controls.className = "controls";
  const stale = isStale(dev);
  if (caps.on_off && !stale) {
    controls.append(
      button("Allumer", () => sendCommand(dev, "on-off", "On", { on: true })),
      button("Éteindre", () => sendCommand(dev, "on-off", "Off", { on: false })),
    );
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
    }).catch(() => { plot.textContent = "Historique indisponible."; });
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
    ["Liaison", dev.status?.state === "online" ? "en ligne" : dev.status?.state === "connecting" ? "connexion…" : "injoignable"],
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
  body.replaceChildren(frag);
}

function groupTitle(id) {
  return groups(state.devices, state.plan).groups.find((g) => g.ids.includes(id))?.title ?? null;
}

// ---------------------------------------------------------------------------
// Navigation, connexion, démarrage
// ---------------------------------------------------------------------------
const TITLES = { devices: "Appareils", temps: "Températures", cameras: "Caméras" };

function render() {
  const conn = $("#conn");
  conn.hidden = state.connection === "online";
  conn.textContent = state.connection === "connecting" ? "connexion…" : "hors ligne";
  if (tab === "devices") renderDevices();
  else if (tab === "temps" && (view._tab !== "temps" || structureDirty)) { structureDirty = false; renderTemps(); }
  else if (tab === "cameras" && (view._tab !== "cameras")) renderCameras();
}

function selectTab(name) {
  if (tab === name && view._tab === name) return;
  tab = name;
  if (name !== "cameras") stopPlayers();
  $("#title").textContent = TITLES[name];
  document.querySelectorAll("#tabs [role=tab]").forEach((b) => b.setAttribute("aria-selected", String(b.dataset.tab === name)));
  view._tab = null;
  structureDirty = true;
  window.scrollTo(0, 0);
  render();
}

function showLogin(message) {
  events?.close();
  events = null;
  stopPlayers();
  closeSheet();
  $("#login").hidden = false;
  $("#app").hidden = true;
  $("#topbar").hidden = true;
  $("#tabs").hidden = true;
  const err = $("#login-error");
  err.hidden = !message;
  err.textContent = message ?? "";
  state.devices.clear();
  state.loaded = false;
  history.clear();
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
    onStatus: (s) => { state.connection = s; scheduleRender(false); },
  });
}

function loginMessage(error) {
  switch (error.code) {
    case "invalid_credentials": return "Identifiant ou mot de passe incorrect.";
    case "too_many_attempts": return `Trop d'essais. Réessayez dans ${error.retryAfter ?? 30} s.`;
    case "version_incompatible": return "Le serveur et l'app ne parlent pas la même version. Mettez le serveur à jour.";
    case "network": return "Serveur injoignable.";
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
  try {
    await api.login(String(form.get("username")), String(form.get("password")));
    formEl.reset();
    submit.disabled = false;
    start();
  } catch (error) {
    console.error(error);
    $("#login-error").hidden = false;
    $("#login-error").textContent = loginMessage(error);
  } finally {
    submit.disabled = false;
  }
});

api.onUnauthorized(() => showLogin("Session expirée : reconnectez-vous."));

$("#tabs").addEventListener("click", (event) => {
  const b = event.target.closest("[role=tab]");
  if (b) selectTab(b.dataset.tab);
});
$("#sheet-close").addEventListener("click", closeSheet);
$("#sheet-backdrop").addEventListener("click", closeSheet);
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !$("#sheet").hidden) closeSheet(); });

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
view.addEventListener("contextmenu", (e) => { if (e.target.closest(".tile")) e.preventDefault(); });
view.addEventListener("click", (e) => {
  if (e.target.closest("#logout")) { api.logout().then(() => showLogin()); return; }
  const cam = e.target.closest(".cam");
  if (cam) { openCamera(cam.dataset.camera); return; }
  const tile = e.target.closest(".tile");
  if (!tile) return;
  if (longPressed) { longPressed = false; return; }
  const dev = state.devices.get(tile.dataset.id);
  if (dev) actOn(dev, tile.dataset.action);
});

document.addEventListener("visibilitychange", () => { if (!document.hidden) events?.reconnectNow(); });
setInterval(() => { if (tab === "devices" && !document.hidden) scheduleRender(false); }, 30_000);

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("sw.js", { scope: "/app/" }).catch(() => {});
}

if (api.hasToken()) start();
else showLogin();
