// Ce qu'on affiche pour un appareil : les mêmes règles que le client de bureau
// (crates/client/src/ui/devices.rs) : une valeur par appareil, « injoignable » et « pile faible »
// seulement quand c'est anormal, « Ouverte » / « Fermée » au féminin (porte, fenêtre).
import { ICONS, shutterIcon } from "./icons.js";

export const NO_VALUE = "—";
export const UNREACHABLE = "injoignable";
export const EXPECT_TTL_MS = 5000;
export const CLICK_GUARD_MS = 400;

const metric = (d, name) => d.latest[name];
export const isOn = (d) => (metric(d, "on_off") ? metric(d, "on_off").value > 0.5 : null);
export const isOpen = (d) => (metric(d, "open") ? metric(d, "open").value > 0.5 : null);
export const position = (d) => metric(d, "position")?.value ?? null;
export const temperature = (d) => metric(d, "temperature")?.value ?? null;
export const batteryLow = (d) => (metric(d, "battery_low")?.value ?? 0) > 0.5;

export function isStale(d) {
  const state = d.status?.state;
  return state === "unreachable" || (state === "connecting" && d.last_seen == null);
}

const oneDecimal = (c) => `${c.toFixed(1).replace(".", ",")} °C`;

export function watts(w) {
  if (Math.abs(w) >= 1000) return `${(w / 1000).toFixed(2).replace(".", ",")} kW`;
  if (Math.abs(w) >= 10) return `${Math.round(w)} W`;
  return `${w.toFixed(1).replace(".", ",")} W`;
}

export const openLabel = (open) => (open ? "Ouverte" : "Fermée");

/** Pictogramme choisi par l'utilisateur, sinon déduit de ce que l'appareil fait. */
export function symbolOf(d) {
  if (d.info.icon) return d.info.icon;
  const caps = d.info.caps;
  const types = (d.info.device_types ?? []).join(" ");
  if (caps.contact) return "Door";
  if (caps.cover) return "Shutter";
  if (caps.temperature) return "Thermometer";
  if (caps.power || (caps.on_off && types.includes("Prise"))) return "Plug";
  if (types.includes("Lampe")) return "Light";
  if (types.includes("Interrupteur")) return "Button";
  return null;
}

const coverIsOpen = (d) => {
  const p = position(d);
  return p != null ? p > 0 : d.cover_motion === "opening";
};
const coverIsMoving = (d, expect) =>
  (expect && expect.motion) || d.cover_motion === "opening" || d.cover_motion === "closing";

function coverValue(d, expectedMotion) {
  const motion = expectedMotion ?? d.cover_motion;
  const arrow = motion === "opening" ? " ▲" : motion === "closing" ? " ▼" : "";
  const p = position(d);
  let base;
  if (p != null && p <= 0) base = "Fermé";
  else if (p != null && p >= 100) base = "Ouvert";
  else if (p != null) base = `${Math.round(p)} %`;
  else if (d.info.caps.cover_position) base = NO_VALUE;
  else base = arrow ? "" : "▲ ▼";
  return `${base}${arrow}`.trim();
}

/**
 * La tuile d'un appareil. `expect` : une commande qui vient de partir (retour visuel immédiat).
 * action : "toggle" | "cover-open" | "cover-close" | "cover-stop" | "detail".
 */
export function tileOf(d, expect) {
  const caps = d.info.caps;
  const stale = isStale(d);
  const on = expect?.on ?? isOn(d);
  const symbol = symbolOf(d);
  let icon;
  let iconWarn = false;
  switch (symbol) {
    case "Light": icon = on ? ICONS.lightOn : ICONS.lightOff; break;
    case "Plug": icon = on ? ICONS.plugOn : ICONS.plugOff; break;
    case "Door":
    case "Window": {
      const open = stale ? null : isOpen(d);
      const window_ = symbol === "Window";
      icon = open ? (window_ ? ICONS.windowOpen : ICONS.doorOpen) : (window_ ? ICONS.windowClosed : ICONS.doorClosed);
      iconWarn = open === true;
      break;
    }
    case "Shutter": icon = shutterIcon(stale ? null : position(d)); break;
    case "Thermometer": icon = ICONS.thermometer; break;
    case "Button": icon = ICONS.button; break;
    default: icon = caps.temperature ? ICONS.thermometer : ICONS.generic;
  }
  let value = null;
  let valueWarn = false;
  let small = null; // { kind: "" | "warn" | "err", text }
  if (stale) {
    small = d.status?.state === "connecting" ? { kind: "", text: "connexion…" } : { kind: "err", text: UNREACHABLE };
  } else if (caps.temperature) {
    const t = temperature(d);
    value = t == null ? null : oneDecimal(t);
    const h = metric(d, "humidity");
    if (h) small = { kind: "", text: `${Math.round(h.value)} %` };
  } else if (caps.contact) {
    const open = isOpen(d);
    if (open != null) { value = openLabel(open); valueWarn = open; }
  } else if (caps.cover) {
    value = coverValue(d, expect?.motion);
  } else if (caps.on_off) {
    if (on === true) value = metric(d, "power") ? watts(metric(d, "power").value) : "Allumée";
    else value = on === false ? "Éteinte" : NO_VALUE;
  } else if (caps.power) {
    value = metric(d, "power") ? watts(metric(d, "power").value) : null;
  } else if (symbol === "Button") {
    value = "Bouton";
  }
  const low = batteryLow(d) && !stale;
  if (low) small = { kind: "warn", text: "pile faible" }; // le mot remplace l'humidité

  let action = "detail";
  if (!stale) {
    if (caps.cover) {
      action = coverIsMoving(d, expect) ? "cover-stop" : coverIsOpen(d) ? "cover-close" : "cover-open";
    } else if (symbol === "Light" && caps.on_off) {
      action = "toggle"; // seules les lumières se basculent d'un tap : une prise peut alimenter un frigo
    }
  }
  return { icon, iconWarn, value, valueWarn, small, lit: symbol === "Light" && on === true, dim: stale, lowBattery: low, action };
}

/** Un pont expose d'autres appareils, ou se déclare « Passerelle ». */
function bridgeIds(devices) {
  const ids = new Set();
  for (const d of devices.values()) {
    const b = d.info.bridge;
    if (b && devices.has(b.node_id)) ids.add(b.node_id);
    if ((d.info.device_types ?? []).includes("Passerelle")) ids.add(d.id);
  }
  return ids;
}

/** Pièce d'un appareil : la première pose qui tombe dans une pièce (HomePlan::room_of). */
export function roomOf(plan, id) {
  for (const floor of plan?.floors ?? []) {
    for (const item of floor.items ?? []) {
      if (item.kind?.type !== "Device" || item.kind.node_id !== id) continue;
      const rooms = floor.rooms ?? [];
      for (let i = rooms.length - 1; i >= 0; i--) {
        const r = rooms[i];
        if (item.x >= r.x && item.x <= r.x + r.w && item.y >= r.y && item.y <= r.y + r.h) {
          return { floor, room: r };
        }
      }
    }
  }
  return null;
}

/** Les pièces (dans l'ordre du plan), « Sans pièce », et à part les ponts. */
export function groups(devices, plan) {
  const bridges = bridgeIds(devices);
  const byName = (a, b) => devices.get(a).info.name.toLowerCase().localeCompare(devices.get(b).info.name.toLowerCase(), "fr") || (a < b ? -1 : 1);
  const perRoom = new Map();
  const loose = [];
  for (const id of devices.keys()) {
    if (bridges.has(id)) continue;
    const hit = roomOf(plan, id);
    if (!hit) { loose.push(id); continue; }
    const key = `${hit.floor.id}/${hit.room.id}`;
    if (!perRoom.has(key)) perRoom.set(key, []);
    perRoom.get(key).push(id);
  }
  const result = [];
  const floors = plan?.floors ?? [];
  for (const floor of floors) {
    for (const room of floor.rooms ?? []) {
      const key = `${floor.id}/${room.id}`;
      const ids = perRoom.get(key);
      if (ids) {
        result.push({ key, title: room.name, note: floors.length > 1 ? floor.name : null, ids: ids.sort(byName) });
      }
    }
  }
  if (loose.length) result.push({ key: "loose", title: "Sans pièce", note: null, ids: loose.sort(byName) });
  return { groups: result, bridges: [...bridges].sort(), averages: roomAverages(devices, plan) };
}

/** Température moyenne de chaque pièce (capteurs joignables), arrondie à la décimale affichée. */
export function roomAverages(devices, plan) {
  const sums = new Map();
  for (const d of devices.values()) {
    const t = temperature(d);
    if (t == null || isStale(d)) continue;
    const hit = roomOf(plan, d.id);
    if (!hit) continue;
    const key = `${hit.floor.id}/${hit.room.id}`;
    const e = sums.get(key) ?? [0, 0];
    sums.set(key, [e[0] + t, e[1] + 1]);
  }
  const out = new Map();
  for (const [key, [sum, n]] of sums) out.set(key, Math.round((sum / n) * 10) / 10);
  return out;
}

/** Palier de température le plus proche (16 ou moins : bleu ; 24 ou plus : rouge). */
export function tempClass(celsius) {
  const step = Math.max(16, Math.min(24, Math.round(celsius / 2) * 2));
  return `t${step}`;
}

export const formatTemperature = oneDecimal;
