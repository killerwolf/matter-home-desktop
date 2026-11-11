// Page Températures : tous les capteurs sur un seul graphique (une courbe par capteur, couleur
// stable), une légende avec case à cocher (valeur actuelle, minimum, maximum), une durée
// (1 h, 24 h, 1 semaine) et un curseur au survol ou au toucher. Le choix des courbes et de la
// durée est gardé dans le localStorage de l'app. Mêmes règles que le client de bureau.
import { cursorLabel, gapThreshold, lineSegments, periodRange, tempChart, valueAt } from "./charts.js";
import { formatTemperature } from "./tiles.js";

const WINDOWS = [
  { key: "1h", label: "1 h", seconds: 3600 },
  { key: "24h", label: "24 h", seconds: 86400 },
  { key: "7d", label: "1 semaine", seconds: 7 * 86400 },
];
const KEY = "matter-home.temps";
const REFRESH_MS = 60_000;
const NONE = "—";

function loadPrefs() {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || "null");
    return {
      hidden: new Set(Array.isArray(v?.hidden) ? v.hidden.map(String) : []),
      window: WINDOWS.some((w) => w.key === v?.window) ? v.window : "24h",
    };
  } catch {
    return { hidden: new Set(), window: "24h" };
  }
}

function savePrefs(prefs) {
  try { localStorage.setItem(KEY, JSON.stringify({ hidden: [...prefs.hidden], window: prefs.window })); } catch { /* stockage refusé : le choix ne sera pas gardé */ }
}

const make = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

// Les identifiants sont des nombres écrits en texte : tri par longueur puis par ordre alphabétique.
const byId = (a, b) => a.id.length - b.id.length || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

/**
 * `request(method, path)` lit l'API ; `getSensors()` rend les capteurs actuels :
 * [{ id, label, room, stale, current, mode }].
 */
export function mountTemps(root, { request, getSensors }) {
  const prefs = loadPrefs();
  let destroyed = false;
  let token = 0;
  let end = Math.floor(Date.now() / 1000);
  let buckets = 400;
  let chart = null;
  let drawnWidth = 0;
  let cursorT = null;

  // Un capteur garde sa couleur : elle vient de son rang parmi les identifiants.
  const sensors = getSensors().sort(byId).map((s, i) => ({
    ...s, colorIndex: (i % 8) + 1, dashed: i >= 8, raw: [], segments: [], mean: [], range: null, gap: 0, error: false,
  }));

  // ---- Structure ----
  const wrap = make("div", "temps");
  const bar = make("div", "temps-bar");
  const seg = make("div", "seg");
  seg.setAttribute("role", "group");
  seg.setAttribute("aria-label", "Période");
  const segButtons = WINDOWS.map((w) => {
    const b = make("button", "", w.label);
    b.type = "button";
    b.dataset.window = w.key;
    b.addEventListener("click", () => { if (prefs.window !== w.key) { prefs.window = w.key; savePrefs(prefs); syncSeg(); load(); } });
    seg.appendChild(b);
    return b;
  });
  const note = make("span", "temps-note muted");
  note.setAttribute("role", "status");
  bar.append(seg, note);
  const plot = make("div", "chart-wrap");
  const host = make("div", "chart-host");
  const readout = make("div", "readout");
  readout.hidden = true;
  readout.setAttribute("aria-hidden", "true");
  plot.append(host, readout);
  const table = make("table", "legend");
  const caption = make("caption", "sr-only", "Capteurs de température");
  const head = make("thead");
  const headRow = make("tr");
  for (const [text, cls] of [["Capteur", ""], ["Actuel", ""], ["Min", ""], ["Max", ""]]) { const th = make("th", cls, text); th.scope = "col"; headRow.appendChild(th); }
  head.appendChild(headRow);
  const body = make("tbody");
  table.append(caption, head, body);
  wrap.append(bar, plot, table);
  root.appendChild(wrap);

  for (const s of sensors) {
    const tr = make("tr");
    const first = make("td");
    const label = make("label");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = !prefs.hidden.has(s.id);
    box.addEventListener("change", () => {
      if (box.checked) prefs.hidden.delete(s.id); else prefs.hidden.add(s.id);
      savePrefs(prefs);
      hideCursor();
      syncLegend();
      draw();
    });
    const sw = make("span", `sw s${s.colorIndex}${s.dashed ? " dash" : ""}`);
    sw.setAttribute("aria-hidden", "true");
    const name = make("span", "lname");
    label.append(box, sw, name);
    first.appendChild(label);
    s.cells = { tr, box, name, cur: make("td", "num"), min: make("td", "num"), max: make("td", "num") };
    tr.append(first, s.cells.cur, s.cells.min, s.cells.max);
    body.appendChild(tr);
  }

  function syncSeg() {
    segButtons.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.window === prefs.window)));
  }

  /** Légende : nom, valeur actuelle (ou « injoignable »), minimum et maximum de la période. */
  function syncLegend() {
    for (const s of sensors) {
      const c = s.cells;
      const off = prefs.hidden.has(s.id);
      c.tr.classList.toggle("off", off);
      c.tr.classList.toggle("stale", s.stale);
      c.name.textContent = s.room && !s.label.includes(s.room) ? `${s.label} (${s.room})` : s.label;
      if (s.stale) {
        c.cur.textContent = "injoignable";
        c.cur.className = "num err";
      } else {
        c.cur.textContent = s.current == null ? NONE : formatTemperature(s.current);
        c.cur.className = "num";
      }
      c.min.textContent = s.range ? formatTemperature(s.range.lo) : NONE;
      c.max.textContent = s.range ? formatTemperature(s.range.hi) : NONE;
    }
  }

  // ---- Données ----
  const windowOf = () => WINDOWS.find((w) => w.key === prefs.window);

  function process() {
    const w = windowOf();
    const start = end - w.seconds;
    for (const s of sensors) {
      s.gap = gapThreshold(s.mode, w.seconds, buckets);
      s.segments = lineSegments(s.raw, start, end, buckets, s.gap);
      s.mean = s.segments.flat();
      s.range = periodRange(s.raw.filter((p) => p.at >= start));
    }
  }

  async function load() {
    const mine = ++token;
    end = Math.floor(Date.now() / 1000);
    const w = windowOf();
    const width = Math.max(plot.clientWidth, 320);
    buckets = Math.min(1600, Math.max(400, Math.ceil(width / 200) * 200));
    syncSeg();
    note.textContent = "Chargement…";
    await Promise.all(sensors.map(async (s) => {
      try {
        const r = await request("GET", `/api/devices/${s.id}/history?since=${end - w.seconds}&until=${end}&metrics=temperature&limit=20000&buckets=${buckets}`);
        s.raw = (r?.points ?? []).filter((p) => p.metric === "temperature" && Number.isFinite(p.value)).map((p) => ({ at: p.at, value: p.value })).sort((a, b) => a.at - b.at);
        s.error = false;
      } catch {
        s.raw = [];
        s.error = true;
      }
    }));
    if (destroyed || mine !== token) return;
    const failed = sensors.filter((s) => s.error).length;
    note.textContent = failed ? (failed === sensors.length ? "Historique indisponible." : `Historique indisponible pour ${failed} capteur${failed > 1 ? "s" : ""}.`) : "";
    process();
    syncLegend();
    draw(true);
  }

  // ---- Graphique et curseur ----
  function draw(force) {
    const width = Math.max(280, Math.floor(plot.clientWidth));
    if (!force && width === drawnWidth && chart) return;
    drawnWidth = width;
    const height = width < 520 ? 220 : width < 1000 ? 280 : 340;
    const w = windowOf();
    const visible = sensors.filter((s) => !prefs.hidden.has(s.id));
    chart = tempChart(visible.map((s) => ({ colorIndex: s.colorIndex, dashed: s.dashed, segments: s.segments })), end - w.seconds, end, { width, height });
    host.replaceChildren(chart.svg);
    chart.svg.setAttribute("tabindex", "0");
    chart.svg.addEventListener("pointerdown", onPointer);
    chart.svg.addEventListener("pointermove", onPointer);
    chart.svg.addEventListener("pointerleave", (e) => { if (e.pointerType === "mouse") hideCursor(); });
    chart.svg.addEventListener("keydown", onKey);
    if (cursorT !== null) showCursor(cursorT);
  }

  function showCursor(t) {
    if (!chart?.hasData) return;
    const w = windowOf();
    cursorT = Math.min(Math.max(t, end - w.seconds), end);
    const items = sensors
      .filter((s) => !prefs.hidden.has(s.id))
      .map((s) => ({ s, colorIndex: s.colorIndex, value: valueAt(s.mean, cursorT, s.gap) }));
    chart.cursor.show(cursorT, items);
    readout.replaceChildren(make("div", "rtime", cursorLabel(cursorT, w.seconds)));
    for (const { s, value } of items) {
      const row = make("div", "rrow");
      const sw = make("span", `sw s${s.colorIndex}${s.dashed ? " dash" : ""}`);
      row.append(sw, make("span", "rname", s.label), make("span", "rval", value === null ? NONE : formatTemperature(value)));
      readout.appendChild(row);
    }
    readout.hidden = false;
    // À droite du curseur, ou à gauche s'il n'y a pas la place.
    const box = chart.svg.getBoundingClientRect();
    const x = (chart.xOf(cursorT) / drawnWidth) * box.width;
    const room = readout.offsetWidth;
    const left = x + 12 + room > box.width ? Math.max(0, x - 12 - room) : x + 12;
    readout.style.left = `${Math.round(left)}px`;
  }

  function hideCursor() {
    cursorT = null;
    chart?.cursor.hide();
    readout.hidden = true;
  }

  function onPointer(e) {
    if (!chart) return;
    const box = chart.svg.getBoundingClientRect();
    showCursor(chart.tAt(((e.clientX - box.left) / box.width) * drawnWidth));
  }

  function onKey(e) {
    const w = windowOf();
    const stepS = w.seconds / 100;
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      showCursor((cursorT ?? end) + (e.key === "ArrowLeft" ? -stepS : stepS));
    } else if (e.key === "Escape") hideCursor();
  }

  // Un doigt qui touche ailleurs que sur le graphique ferme le curseur (il reste après le toucher).
  const onOutside = (e) => { if (!plot.contains(e.target)) hideCursor(); };
  document.addEventListener("pointerdown", onOutside);

  const resizer = typeof ResizeObserver === "function" ? new ResizeObserver(() => { if (!destroyed && Math.floor(plot.clientWidth) !== drawnWidth && plot.clientWidth > 0) draw(); }) : null;
  resizer?.observe(plot);
  const timer = setInterval(() => { if (!document.hidden) load(); }, REFRESH_MS);

  syncSeg();
  syncLegend();
  load();

  return {
    /** Valeurs en direct (actuelle, injoignable) : la légende seule, sans refaire les courbes. */
    refreshLive() {
      const live = new Map(getSensors().map((s) => [s.id, s]));
      for (const s of sensors) {
        const l = live.get(s.id);
        if (l) { s.stale = l.stale; s.current = l.current; s.mode = l.mode ?? s.mode; }
      }
      syncLegend();
    },
    destroy() {
      destroyed = true;
      clearInterval(timer);
      resizer?.disconnect();
      document.removeEventListener("pointerdown", onOutside);
      wrap.remove();
    },
  };
}
