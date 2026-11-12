// App web Matter Home : appareils en tuiles, températures, caméras. Sans framework, sans build.
import * as api from "./api.js";
import { ICONS, mountIcons } from "./icons.js";
import {
  CLICK_GUARD_MS, EXPECT_TTL_MS, batteryLow, formatTemperature, groups, isOn, isStale, position,
  tempClass, temperature, tileOf, watts,
} from "./tiles.js";
import { lineChart, rangeOf } from "./charts.js";
import { mountTemps } from "./temps.js";
import { createPlayer, createSnapshot, webCodecsSupported } from "./cameras.js";

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
    case "cameras_changed": state.cameras = e.cameras; scheduleRender(true); return;
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
    const rooms = document.createElement("div");
    rooms.className = "rooms";
    view.appendChild(rooms);
    for (const g of layout.groups) {
      const section = document.createElement("section");
      section.className = "room";
      section.dataset.room = g.key;
      const head = document.createElement("div");
      head.className = "room-head";
      const h2 = document.createElement("h2");
      h2.textContent = g.title;
      head.appendChild(h2);
      // La pastille de température, juste à côté du nom de la pièce ; l'étage vient après.
      const pill = document.createElement("span");
      pill.className = "pill";
      pill.hidden = true;
      head.appendChild(pill);
      if (g.note) { const n = document.createElement("span"); n.className = "note"; n.textContent = g.note; head.appendChild(n); }
      section.appendChild(head);
      const grid = document.createElement("div");
      grid.className = "tiles";
      for (const id of g.ids) grid.appendChild(tileElement(id));
      section.appendChild(grid);
      rooms.appendChild(section);
    }
    if (layout.bridges.length) {
      // Les ponts forment un dernier bloc, comme une pièce.
      const section = document.createElement("section");
      section.className = "room bridges";
      const head = document.createElement("div");
      head.className = "room-head";
      const h2 = document.createElement("h2");
      h2.textContent = "Ponts";
      head.appendChild(h2);
      const grid = document.createElement("div");
      grid.className = "tiles";
      for (const id of layout.bridges) { const t = tileElement(id); t.dataset.bridge = "1"; grid.appendChild(t); }
      section.append(head, grid);
      rooms.appendChild(section);
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
// Rendu : caméras (vignettes en flux réduit ; un clic passe une vignette en plein écran,
// en flux principal)
// ---------------------------------------------------------------------------
let players = [];
let camCards = []; // { el, cam, canvas, msg, fs, playable, thumb, player, snap, visible }
let thumbObserver = null;
const camSignature = () => JSON.stringify(state.cameras.map((c) => [c.id, c.name, c.status?.state, c.has_sub_stream]));
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
  const showMsg = (c, st) => { c.msg.textContent = st.text; c.msg.hidden = st.kind === "live"; };
  for (const c of camCards) {
    if (live.has(c) && !c.player) {
      c.snap?.stop();
      players = players.filter((p) => p !== c.snap);
      c.snap = null;
      c.player = createPlayer(c.cam.id, "sub", c.canvas, (st) => showMsg(c, st));
      players.push(c.player);
    } else if (!live.has(c) && c.player) {
      c.player.stop();
      players = players.filter((p) => p !== c.player);
      c.player = null;
    }
    if (still.has(c) && !c.snap && !c.player) {
      c.snap = createSnapshot(c.cam.id, c.canvas, (st) => showMsg(c, st));
      players.push(c.snap);
    } else if (!still.has(c) && c.snap) {
      c.snap.stop();
      players = players.filter((p) => p !== c.snap);
      c.snap = null;
    }
    if (!c.player && !c.snap) {
      c.msg.hidden = false;
      c.msg.textContent = c.thumb ? "En attente…" : "Touchez pour ouvrir";
    }
  }
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
    const el = document.createElement("div");
    el.className = "cam";
    el.dataset.camera = cam.id;
    const canvas = document.createElement("canvas");
    const msg = document.createElement("div");
    msg.className = "msg";
    const cap = document.createElement("div");
    cap.className = "cap";
    cap.textContent = cam.name;
    el.append(canvas, msg, cap);
    grid.appendChild(el);
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
    camCards.push({ el, cam, canvas, msg, fs, playable: true, thumb, player: null, snap: null, visible: false });
  }
  view.appendChild(grid);
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
function showSheet() {
  if ($("#sheet").hidden) sheetOpener = document.activeElement;
  $("#sheet").hidden = false;
  $("#sheet-backdrop").hidden = false;
  $("#app").inert = true;
  $("#sheet").setAttribute("tabindex", "-1");
  $("#sheet-close").focus();
}
function closeSheet() {
  const wasOpen = !$("#sheet").hidden;
  $("#sheet").hidden = true;
  $("#sheet-backdrop").hidden = true;
  $("#app").inert = false;
  sheetId = null;
  if (wasOpen) {
    const target = sheetOpener;
    sheetOpener = null;
    if (target && document.contains(target) && !target.closest("[hidden]")) target.focus();
  }
}

function openDetail(id) {
  if (!state.devices.has(id)) return;
  sheetId = id;
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
    body.querySelector(".sheet-value").textContent = t.value ?? "";
    body.querySelector(".sheet-value").hidden = t.value == null;
    body.querySelector(".sheet-value").classList.toggle("warn", t.valueWarn);
    body.querySelector(".sheet-sub").textContent = t.small?.text ?? "";
    return;
  }
  body.dataset.id = dev.id;
  const frag = document.createDocumentFragment();
  const value = document.createElement("div");
  value.className = `sheet-value${t.valueWarn ? " warn" : ""}`;
  value.textContent = t.value ?? "";
  value.hidden = t.value == null;
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
  else if (tab === "temps") {
    structureDirty = false;
    renderTemps();
  } else if (tab === "cameras") {
    const signature = camSignature();
    if (fullCam && structureDirty && view._camSignature !== signature) {
      // En plein écran, le changement d'une autre caméra attend la sortie ; seule la disparition
      // ou la panne de la caméra affichée fait sortir tout de suite.
      structureDirty = false;
      const shown = state.cameras.find((c) => c.id === fullCam.id);
      if (!shown || shown.status?.state !== "online") { exitFull(); renderCameras(); view._camSignature = signature; camsDirty = false; }
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
  $("#title").textContent = TITLES[name];
  document.querySelectorAll("#tabs [role=tab]").forEach((b) => {
    b.setAttribute("aria-selected", String(b.dataset.tab === name));
    b.tabIndex = b.dataset.tab === name ? 0 : -1;
  });
  view.setAttribute("aria-labelledby", `tab-${name}`);
  view._tab = null;
  structureDirty = true;
  window.scrollTo(0, 0);
  render();
}

function showLogin(message) {
  events?.close();
  events = null;
  closeSheet();
  exitFull();
  stopPlayers();
  stopTemps();
  $("#login").hidden = false;
  $("#app").hidden = true;
  $("#topbar").hidden = true;
  $("#tabs").hidden = true;
  const err = $("#login-error");
  err.hidden = !message;
  err.textContent = message ?? "";
  state.devices.clear();
  state.loaded = false;
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
document.addEventListener("keydown", (e) => {
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
setInterval(() => { if (tab === "devices" && !document.hidden) scheduleRender(false); }, 30_000);

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("sw.js", { scope: "/app/" }).catch(() => {});
}

if (api.hasToken()) start();
else showLogin();
