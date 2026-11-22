// Page Plan (lecture seule) : les pièces dessinées en SVG, les appareils placés dessus avec leur
// état en direct, zoom au pincement (ou à la molette) et déplacement au doigt (pointer events).
// Tout est construit par le DOM (createElementNS, textContent, setAttribute) : aucune donnée de la
// maison n'est jamais passée à innerHTML (seuls les pictogrammes, des chaînes fixes, le sont).
// Les pièces sont dessinées dans un groupe à l'échelle du plan (mètres) ; les objets et les noms
// restent à taille d'écran constante, posés au bon endroit : ils restent lisibles et touchables.
import { ICONS, shutterIcon } from "./icons.js";
import { formatTemperature, roomAverages, symbolOf, tempClass, tileOf } from "./tiles.js";

const NS = "http://www.w3.org/2000/svg";
export const EMPTY_TEXT = "Le plan se dessine dans l'application de bureau.";

const PAD = 16; // marge autour du plan à l'ajustement (px) : le plan remplit le cadre
const MARGIN_M = 0.5; // marge autour des pièces et objets (m)
const MAX_FIT = 90; // pas plus de 90 px par mètre à l'ajustement
const ZOOM_OUT = 0.6; // jusqu'à 0,6 fois l'ajustement
const ZOOM_IN = 14; // jusqu'à 14 fois l'ajustement
const KEEP_PX = 56; // une part du plan reste toujours à l'écran
const TAP_SLOP = 8; // en dessous, un doigt qui bouge reste un tap
const MARKER_MIN = 0.65; // taille relative des pictogrammes à l'ajustement

// ---------------------------------------------------------------------------
// Géométrie (pure, essayée par plan.test.mjs)
// ---------------------------------------------------------------------------

/** Rectangle { x0, y0, x1, y1 } (m) qui contient les pièces et les objets de l'étage, ou null. */
export function boundsOf(floor) {
  let b = null;
  const add = (x0, y0, x1, y1) => {
    if (![x0, y0, x1, y1].every(Number.isFinite)) return;
    b = b ? { x0: Math.min(b.x0, x0), y0: Math.min(b.y0, y0), x1: Math.max(b.x1, x1), y1: Math.max(b.y1, y1) } : { x0, y0, x1, y1 };
  };
  for (const r of floor?.rooms ?? []) add(r.x, r.y, r.x + r.w, r.y + r.h);
  for (const i of floor?.items ?? []) add(i.x, i.y, i.x, i.y);
  if (!b) return null;
  return { x0: b.x0 - MARGIN_M, y0: b.y0 - MARGIN_M, x1: b.x1 + MARGIN_M, y1: b.y1 + MARGIN_M };
}

/** Vue qui montre tout `bounds` dans `w` x `h` px : { s (px/m), tx, ty (px) }. */
export function fitView(bounds, w, h) {
  const bw = Math.max(bounds.x1 - bounds.x0, 1);
  const bh = Math.max(bounds.y1 - bounds.y0, 1);
  const s = Math.max(1, Math.min(MAX_FIT, (w - 2 * PAD) / bw, (h - 2 * PAD) / bh));
  return { s, tx: (w - bw * s) / 2 - bounds.x0 * s, ty: (h - bh * s) / 2 - bounds.y0 * s };
}

/**
 * Vue d'ouverture : le plan entier, centré (l'ajustement complet), comme la maquette Web-Plan.
 * Le zoom et le glissement viennent ensuite, au doigt ou aux boutons.
 */
export function initialView(bounds, w, h) {
  return fitView(bounds, w, h);
}

export const limitsFor = (fit) => ({ min: fit.s * ZOOM_OUT, max: fit.s * ZOOM_IN });

/** Ramène la vue dans les limites : échelle bornée, et un bout du plan toujours visible. */
export function clampView(view, bounds, w, h, limits) {
  const s = Math.max(limits.min, Math.min(limits.max, view.s));
  const keepX = Math.min(KEEP_PX, w / 2);
  const keepY = Math.min(KEEP_PX, h / 2);
  const left = bounds.x0 * s;
  const right = bounds.x1 * s;
  const top = bounds.y0 * s;
  const bottom = bounds.y1 * s;
  // Le bord droit du plan ne passe pas à gauche de keepX, le gauche pas à droite de w - keepX.
  const tx = Math.max(keepX - right, Math.min(w - keepX - left, view.tx));
  const ty = Math.max(keepY - bottom, Math.min(h - keepY - top, view.ty));
  return { s, tx, ty };
}

/** Change l'échelle en gardant fixe le point d'écran (px, py). */
export function zoomAt(view, factor, px, py) {
  const s = view.s * factor;
  const wx = (px - view.tx) / view.s;
  const wy = (py - view.ty) / view.s;
  return { s, tx: px - wx * s, ty: py - wy * s };
}

/** Pincement : le point du plan qui était sous le milieu de départ suit le milieu actuel. */
export function pinchView(start, d0, mid0, d1, mid1) {
  const s = start.s * (d0 > 0 ? d1 / d0 : 1);
  const wx = (mid0.x - start.tx) / start.s;
  const wy = (mid0.y - start.ty) / start.s;
  return { s, tx: mid1.x - wx * s, ty: mid1.y - wy * s };
}

/** Étage à montrer d'abord : le premier qui a des pièces ou des objets. */
export function firstFilledFloor(plan) {
  const floors = plan?.floors ?? [];
  const i = floors.findIndex((f) => (f.rooms?.length ?? 0) + (f.items?.length ?? 0) > 0);
  return i < 0 ? 0 : i;
}

export const planIsEmpty = (plan) => !(plan?.floors ?? []).some((f) => (f.rooms?.length ?? 0) + (f.items?.length ?? 0) > 0);

// ---------------------------------------------------------------------------
// Dessin
// ---------------------------------------------------------------------------

const el = (tag, attrs, className) => {
  const n = document.createElementNS(NS, tag);
  if (className) n.setAttribute("class", className);
  for (const [k, v] of Object.entries(attrs ?? {})) n.setAttribute(k, String(v));
  return n;
};
const html = (tag, className, text) => {
  const n = document.createElement(tag);
  if (className) n.className = className;
  if (text !== undefined) n.textContent = text;
  return n;
};
const textW = (text, px) => text.length * px * 0.55; // largeur estimée d'un texte (px)
const hit = (a, b) => a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1;
const short = (text, n = 12) => (text.length > n ? `${text.slice(0, n - 1)}…` : text);

const DECOR_ICONS = {
  Door: ICONS.doorClosed, Window: ICONS.windowClosed, Light: ICONS.lightOff, Plug: ICONS.plugOff,
  Thermometer: ICONS.thermometer, Button: ICONS.button,
};
const decorIcon = (symbol) => (symbol === "Shutter" ? shutterIcon(null) : DECOR_ICONS[symbol] ?? ICONS.generic);

/** Pose un pictogramme (chaîne SVG fixe) dans le groupe, centré sur l'origine. */
function setIcon(group, icon) {
  if (group._icon === icon) return;
  group._icon = icon;
  group.innerHTML = icon;
  const svg = group.firstElementChild;
  if (svg) for (const [k, v] of [["x", -11], ["y", -11], ["width", 22], ["height", 22]]) svg.setAttribute(k, String(v));
}

/** Cône ou rectangle du champ d'une caméra, en mètres, orienté vers le haut (0 degré). */
function fovPath(kind) {
  const range = Math.max(0.5, Math.min(50, Number(kind.range) || 3));
  const fov = Math.max(10, Math.min(180, Number(kind.fov) || 90));
  const half = (fov / 2) * (Math.PI / 180);
  const sx = range * Math.sin(half);
  const sy = -range * Math.cos(half);
  if (kind.shape === "Rectangle") return `M${-sx} 0H${sx}V${-range}H${-sx}Z`;
  return `M0 0L${-sx} ${sy}A${range} ${range} 0 0 1 ${sx} ${sy}Z`;
}

/**
 * Monte le plan dans `root`. `ctx` : { getPlan, getDevices (Map id -> appareil), getCameras,
 * getExpect(id) (commande en cours), loaded (booléen), onOpenDevice(id), onOpenCameras() }.
 * Rend { update(), destroy() } ; update() se rappelle à chaque changement d'état.
 */
export function mountPlan(root, ctx) {
  const wrap = html("div", "plan");
  const bar = html("div", "plan-bar");
  const floorsBox = html("div", "seg");
  floorsBox.setAttribute("role", "group");
  floorsBox.setAttribute("aria-label", "Étage");
  const tools = html("div", "tools");
  tools.setAttribute("role", "group");
  tools.setAttribute("aria-label", "Zoom et centrage");
  // Un bouton de 44 px avec son pictogramme (tracé en DOM : jamais de HTML construit ici).
  const mkTool = (label, paths, onClick, ring = false) => {
    const b = html("button", "");
    b.type = "button";
    b.setAttribute("aria-label", label);
    const icon = el("svg", { viewBox: "0 0 24 24", "aria-hidden": "true" });
    for (const d of paths) icon.appendChild(el("path", { d }));
    if (ring) icon.appendChild(el("circle", { cx: 12, cy: 12, r: 7 }));
    b.appendChild(icon);
    b.addEventListener("click", onClick);
    return b;
  };
  const stage = html("div", "plan-stage");
  const svg = el("svg", { role: "group", "aria-label": "Plan de la maison", tabindex: 0 }, "plan-svg");
  const world = el("g");
  const layer = el("g");
  svg.append(world, layer);
  const message = html("p", "empty plan-empty");
  message.hidden = true;
  stage.append(svg, message);
  bar.append(floorsBox, tools);
  // La légende : ce que veulent dire les pastilles et les teintes.
  const legend = html("div", "plan-legend");
  for (const [cls, text] of [["on", "Allumé"], ["", "Injoignable"], ["tint", "Teinte : température"]]) {
    const item = html("span");
    item.append(html("i", cls), text);
    legend.appendChild(item);
  }
  wrap.append(bar, stage, legend);
  root.appendChild(wrap);

  let floorIndex = firstFilledFloor(ctx.getPlan());
  let userChoseFloor = false;
  let signature = null;
  let bounds = null;
  let fit = null;
  let limits = null;
  let view = null;
  let fitted = true;
  let size = { w: 0, h: 0 };
  let items = []; // { node, x, y, update(), ... }
  let labels = []; // { node, x, y, room }
  let rooms = []; // { node, room, key }
  let destroyed = false;
  let raf = 0;

  // ---- étiquettes qui ne se chevauchent pas ----
  // Les pictogrammes restent toujours visibles. Les noms de pièce cherchent un coin libre ; le nom
  // court d'un appareil (puis sa valeur) disparaît quand il en toucherait un autre : les appareils
  // allumés ou injoignables passent d'abord, les objets décoratifs en dernier. En zoomant, tout revient.
  const declutter = (k) => {
    const placed = [];
    const free = (b) => !placed.some((o) => hit(o, b));
    const at = items.map((i) => ({ i, px: view.tx + view.s * i.x, py: view.ty + view.s * i.y }));
    for (const { i, px, py } of at) {
      const r = i.r * k + 2;
      placed.push({ x0: px - r, y0: py - r, x1: px + r, y1: py + r });
    }
    for (const l of labels) {
      const left = view.tx + view.s * l.x;
      const top = view.ty + view.s * l.y;
      const shown = view.s * l.w >= 70;
      l.node.style.visibility = shown ? "visible" : "hidden";
      if (!shown) continue;
      const w = textW(l.node.textContent, 13);
      const right = left + view.s * l.w;
      const bottom = top + view.s * l.h;
      // En haut à gauche, puis en bas à gauche, en haut à droite, en bas à droite.
      const spots = [
        { anchor: "start", x: left + 6, y: top + 15, x0: left + 6 },
        { anchor: "start", x: left + 6, y: bottom - 7, x0: left + 6 },
        { anchor: "end", x: right - 6, y: top + 15, x0: right - 6 - w },
        { anchor: "end", x: right - 6, y: bottom - 7, x0: right - 6 - w },
      ];
      const fits = (c) => c.x0 + w <= right && c.y - 12 >= top - 2 && c.y + 4 <= bottom + 2;
      const spot = spots.find((c) => fits(c) && free({ x0: c.x0, y0: c.y - 12, x1: c.x0 + w, y1: c.y + 4 })) ?? spots[0];
      l.node.setAttribute("text-anchor", spot.anchor);
      l.node.setAttribute("x", String(spot.x));
      l.node.setAttribute("y", String(spot.y));
      placed.push({ x0: spot.x0, y0: spot.y - 12, x1: spot.x0 + w, y1: spot.y + 4 });
    }
    const rank = ({ i }) => (i.decor ? 2 : /\b(lit|warn|err)\b/.test(i.node.getAttribute("class") ?? "") ? 0 : 1);
    for (const e of at.map((a, n) => ({ ...a, n })).sort((a, b) => rank(a) - rank(b) || a.n - b.n)) {
      const [name, value] = e.i.texts.map(([node]) => node);
      const base = e.py + e.i.r * k;
      const box = (node, off, px) => {
        const w = textW(node.textContent, px);
        return { x0: e.px - w / 2, y0: base + off - 11, x1: e.px + w / 2, y1: base + off + 3 };
      };
      const hasName = name.textContent !== "";
      const nameBox = hasName ? box(name, e.i.texts[0][1], 12) : null;
      const nameOk = hasName && free(nameBox);
      name.style.visibility = nameOk ? "visible" : "hidden";
      if (nameOk) placed.push(nameBox);
      if (value) {
        const valueBox = value.textContent !== "" ? box(value, e.i.texts[1][1], 11) : null;
        const valueOk = nameOk && valueBox != null && free(valueBox);
        value.style.visibility = valueOk ? "visible" : "hidden";
        if (valueOk) placed.push(valueBox);
      }
    }
  };

  // ---- vue ----
  const apply = () => {
    raf = 0;
    if (destroyed || !view) return;
    world.setAttribute("transform", `translate(${view.tx} ${view.ty}) scale(${view.s})`);
    // Les pictogrammes grossissent un peu avec le zoom (0,65 à l'ajustement, 1 dès 1,6 fois) ;
    // les textes gardent leur taille et se décalent avec eux.
    const k = fit ? Math.max(MARKER_MIN, Math.min(1, MARKER_MIN + ((1 - MARKER_MIN) * (view.s / fit.s - 1)) / 0.6)) : 1;
    for (const i of items) {
      i.node.setAttribute("transform", `translate(${view.tx + view.s * i.x} ${view.ty + view.s * i.y})`);
      i.body.setAttribute("transform", `scale(${k})`);
      for (const [node, off] of i.texts) node.setAttribute("y", String(i.r * k + off));
    }
    declutter(k);
  };
  const schedule = () => { if (!raf) raf = requestAnimationFrame(apply); };
  const setView = (next) => {
    view = bounds ? clampView(next, bounds, size.w, size.h, limits) : next;
    schedule();
  };
  const recenter = () => {
    if (!bounds || !size.w) return;
    fit = fitView(bounds, size.w, size.h);
    limits = limitsFor(fit);
    fitted = true;
    view = clampView(initialView(bounds, size.w, size.h), bounds, size.w, size.h, limits);
    schedule();
  };
  const zoomBy = (factor) => {
    if (!view) return;
    fitted = false;
    setView(zoomAt(view, factor, size.w / 2, size.h / 2));
  };
  tools.append(
    mkTool("Zoom avant", ["M12 5v14M5 12h14"], () => zoomBy(1.5)),
    mkTool("Zoom arrière", ["M5 12h14"], () => zoomBy(1 / 1.5)),
    mkTool("Recentrer le plan", ["M12 2v4M12 18v4M2 12h4M18 12h4"], recenter, true),
  );

  const measure = () => {
    const r = svg.getBoundingClientRect();
    const w = Math.round(r.width);
    const h = Math.round(r.height);
    if (!w || !h || (w === size.w && h === size.h)) return;
    size = { w, h };
    svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
    if (!bounds) return;
    const keep = view;
    fit = fitView(bounds, w, h);
    limits = limitsFor(fit);
    if (fitted || !keep) recenter();
    else setView(keep);
  };
  const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
  observer?.observe(svg);

  // ---- construction ----
  const devices = () => ctx.getDevices();

  function deviceItem(item) {
    const k = item.kind;
    const id = String(k.node_id);
    const group = el("g", { tabindex: 0, role: "button" }, "pi");
    group.dataset.id = id;
    const disc = el("circle", { r: 17 }, "pbg");
    const icon = el("g", {}, "pico");
    const value = el("text", { "text-anchor": "middle" }, "pval");
    const name = el("text", { "text-anchor": "middle" }, "pname");
    const body = el("g", {}, "pbody");
    body.append(disc, icon);
    group.append(el("circle", { r: 24 }, "phit"), body, name, value);
    const entry = {
      node: group, x: item.x, y: item.y, body, r: 17, texts: [[name, 15], [value, 29]],
      update() {
        const dev = devices().get(id);
        if (!dev) {
          group.setAttribute("class", "pi dim");
          setIcon(icon, ICONS.generic);
          value.textContent = "";
          name.textContent = "";
          group.setAttribute("aria-label", "Appareil inconnu");
          return;
        }
        // Le pictogramme choisi sur le plan remplace celui de l'appareil ; une fenêtre se dit
        // par `opening` pour un capteur d'ouverture.
        const symbol = k.symbol ?? (dev.info.caps.contact && k.opening === "Window" ? "Window" : dev.info.icon);
        const shown = symbol === dev.info.icon ? dev : { ...dev, info: { ...dev.info, icon: symbol } };
        const e = ctx.getExpect(id);
        const t = tileOf(shown, e && Date.now() - e.at < 5000 ? e : null);
        group.setAttribute("class", `pi${t.lit ? " lit" : ""}${t.iconWarn || t.valueWarn || t.lowBattery ? " warn" : ""}${t.small?.kind === "err" ? " err" : ""}`);
        setIcon(icon, t.icon);
        const text = t.value ?? t.small?.text ?? "";
        value.textContent = text;
        name.textContent = short(dev.info.name);
        group.setAttribute("aria-label", [dev.info.name, t.value, t.small?.text].filter(Boolean).join(", "));
      },
    };
    entry.update();
    return entry;
  }

  function decorItem(item) {
    const k = item.kind;
    const group = el("g", { role: "img", "aria-label": k.label || "Objet" }, "pi decor");
    const icon = el("g", {}, "pico");
    setIcon(icon, decorIcon(k.symbol));
    const name = el("text", { "text-anchor": "middle" }, "pname");
    name.textContent = short(k.label ?? "");
    const body = el("g", {}, "pbody");
    body.append(el("circle", { r: 14 }, "pbg"), icon);
    group.append(body, name);
    return { node: group, x: item.x, y: item.y, body, r: 14, decor: true, texts: [[name, 15]], update() {} };
  }

  function cameraItem(item) {
    const k = item.kind;
    const id = String(k.camera_id);
    const group = el("g", { tabindex: 0, role: "button" }, "pi cam-pi");
    const icon = el("g", {}, "pico");
    setIcon(icon, ICONS.camera);
    const value = el("text", { "text-anchor": "middle" }, "pval");
    const name = el("text", { "text-anchor": "middle" }, "pname");
    const body = el("g", {}, "pbody");
    body.append(el("circle", { r: 17 }, "pbg"), icon);
    group.append(el("circle", { r: 24 }, "phit"), body, name, value);
    return {
      node: group, x: item.x, y: item.y, body, r: 17, texts: [[name, 15], [value, 29]],
      update() {
        const cam = ctx.getCameras().find((c) => String(c.id) === id);
        const online = cam?.status?.state === "online";
        group.setAttribute("class", `pi cam-pi${online ? "" : " err"}`);
        value.textContent = cam ? (online ? "" : "injoignable") : "inconnue";
        name.textContent = short(cam?.name ?? "Caméra");
        group.setAttribute("aria-label", `${cam?.name ?? "Caméra"}${online ? "" : ", injoignable"}`);
        group.dataset.camera = id;
      },
    };
  }

  function build(plan) {
    world.replaceChildren();
    layer.replaceChildren();
    items = [];
    labels = [];
    rooms = [];
    const floor = plan?.floors?.[floorIndex];
    bounds = boundsOf(floor);
    // Étage vide : le message, sans dessin.
    message.hidden = bounds != null;
    message.textContent = EMPTY_TEXT;
    svg.toggleAttribute("hidden", bounds == null);
    if (!floor || !bounds) return;
    for (const r of floor.rooms ?? []) {
      const node = el("rect", { x: r.x, y: r.y, width: r.w, height: r.h }, "proom");
      world.appendChild(node);
      rooms.push({ node, key: `${floor.id}/${r.id}` });
      const label = el("text", {}, "plabel");
      label.textContent = r.name;
      layer.appendChild(label);
      labels.push({ node: label, x: r.x, y: r.y, w: r.w, h: r.h });
    }
    // Les champs des caméras sous les objets, les objets par-dessus.
    for (const i of floor.items ?? []) {
      if (i.kind?.type !== "Camera") continue;
      const fov = el("path", { d: fovPath(i.kind), transform: `translate(${i.x} ${i.y}) rotate(${Number(i.rotation) || 0})` }, "pfov");
      world.appendChild(fov);
    }
    for (const i of floor.items ?? []) {
      const kind = i.kind?.type;
      const made = kind === "Device" ? deviceItem(i) : kind === "Decor" ? decorItem(i) : kind === "Camera" ? cameraItem(i) : null;
      if (!made) continue;
      layer.appendChild(made.node);
      items.push(made);
    }
    if (size.w) {
      fit = fitView(bounds, size.w, size.h);
      limits = limitsFor(fit);
    }
  }

  function patch(plan) {
    const floor = plan?.floors?.[floorIndex];
    const avg = roomAverages(devices(), plan);
    for (const r of rooms) {
      const t = avg.get(r.key);
      r.node.setAttribute("class", `proom${t != null ? ` ${tempClass(t)}` : ""}`);
    }
    // Le nom de la pièce porte sa température moyenne.
    const names = floor?.rooms ?? [];
    labels.forEach((l, idx) => {
      const t = avg.get(rooms[idx]?.key);
      l.node.textContent = t != null ? `${names[idx].name} · ${formatTemperature(t)}` : names[idx].name;
    });
    for (const i of items) i.update();
  }

  function buildFloorTabs(plan) {
    floorsBox.replaceChildren();
    const floors = plan?.floors ?? [];
    floorsBox.hidden = floors.length < 2;
    floors.forEach((f, i) => {
      const b = html("button", "", f.name);
      b.type = "button";
      b.setAttribute("aria-pressed", String(i === floorIndex));
      b.addEventListener("click", () => { floorIndex = i; userChoseFloor = true; signature = null; fitted = true; view = null; update(); });
      floorsBox.appendChild(b);
    });
  }

  function update() {
    if (destroyed) return;
    const plan = ctx.getPlan();
    if (!plan) {
      // Pas encore de plan : « Chargement… », puis le message du plan vide.
      svg.toggleAttribute("hidden", true);
      bar.hidden = true;
      message.hidden = false;
      message.textContent = ctx.loaded ? EMPTY_TEXT : "Chargement…";
      return;
    }
    bar.hidden = planIsEmpty(plan);
    if (!userChoseFloor) floorIndex = firstFilledFloor(plan);
    else if (floorIndex >= plan.floors.length) floorIndex = 0;
    const next = JSON.stringify([floorIndex, plan.floors.map((f) => [f.id, f.name, f.rooms, f.items])]);
    if (next !== signature) {
      const hadView = view != null;
      signature = next;
      buildFloorTabs(plan);
      build(plan);
      measure();
      if (bounds && size.w) {
        if (!hadView || fitted) recenter();
        else setView(view);
      }
    }
    patch(plan);
    schedule();
  }

  // ---- gestes : un doigt déplace, deux doigts pincent, un tap ouvre ----
  const pointers = new Map();
  let gesture = null; // { kind: "pan" | "pinch", ... }

  const local = (e) => {
    const r = svg.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const startPan = (p, target, moved) => { gesture = { kind: "pan", sx: p.x, sy: p.y, start: view, moved, target }; };
  const startPinch = () => {
    const [a, b] = [...pointers.values()];
    gesture = { kind: "pinch", d0: Math.hypot(a.x - b.x, a.y - b.y), mid0: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }, start: view };
  };

  svg.addEventListener("pointerdown", (e) => {
    if (!view || (e.pointerType === "mouse" && e.button !== 0)) return;
    const p = local(e);
    pointers.set(e.pointerId, p);
    try { svg.setPointerCapture(e.pointerId); } catch { /* pointeur déjà perdu */ }
    if (pointers.size === 1) startPan(p, e.target, false);
    else if (pointers.size === 2) startPinch();
  });
  svg.addEventListener("pointermove", (e) => {
    if (!pointers.has(e.pointerId) || !gesture) return;
    const p = local(e);
    pointers.set(e.pointerId, p);
    if (gesture.kind === "pinch" && pointers.size >= 2) {
      const [a, b] = [...pointers.values()];
      fitted = false;
      setView(pinchView(gesture.start, gesture.d0, gesture.mid0, Math.hypot(a.x - b.x, a.y - b.y), { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }));
    } else if (gesture.kind === "pan") {
      const dx = p.x - gesture.sx;
      const dy = p.y - gesture.sy;
      if (!gesture.moved && Math.hypot(dx, dy) < TAP_SLOP) return;
      gesture.moved = true;
      fitted = false;
      setView({ s: gesture.start.s, tx: gesture.start.tx + dx, ty: gesture.start.ty + dy });
    }
  });
  const end = (e, cancelled) => {
    if (!pointers.has(e.pointerId)) return;
    pointers.delete(e.pointerId);
    try { svg.releasePointerCapture(e.pointerId); } catch { /* déjà relâché */ }
    const g = gesture;
    if (pointers.size === 0) {
      gesture = null;
      if (!cancelled && g?.kind === "pan" && !g.moved) activate(g.target);
    } else if (pointers.size === 1) {
      // Un doigt reste après un pincement : il continue à déplacer, sans jamais compter pour un tap.
      const [rest] = pointers.values();
      startPan(rest, null, true);
    } else startPinch();
  };
  svg.addEventListener("pointerup", (e) => end(e, false));
  svg.addEventListener("pointercancel", (e) => end(e, true));
  svg.addEventListener("lostpointercapture", (e) => { if (pointers.has(e.pointerId)) end(e, true); });
  // Safari iOS : un pincement qui commence sur la barre du plan zoomerait la page entière.
  for (const type of ["gesturestart", "gesturechange"]) wrap.addEventListener(type, (e) => e.preventDefault(), { passive: false });
  svg.addEventListener("wheel", (e) => {
    if (!view) return;
    e.preventDefault();
    const p = local(e);
    fitted = false;
    setView(zoomAt(view, Math.exp(-e.deltaY * (e.deltaMode === 1 ? 0.05 : 0.0015)), p.x, p.y));
  }, { passive: false });

  // Après un tap, le navigateur envoie encore un `click` à l'endroit touché, où la feuille de détail
  // vient de s'ouvrir : il tomberait sur son fond et la fermerait aussitôt. On l'avale (touchend
  // annulé, et garde en capture sur le document pour les navigateurs qui l'envoient quand même).
  let lastActivate = 0;
  const swallowGhostClick = (e) => {
    if (Date.now() - lastActivate < 600) { e.stopPropagation(); e.preventDefault(); }
  };
  document.addEventListener("click", swallowGhostClick, true);
  svg.addEventListener("touchend", (e) => {
    if (Date.now() - lastActivate < 600 && e.cancelable) e.preventDefault();
  }, { passive: false });

  function activate(target) {
    const pi = target?.closest?.(".pi");
    if (!pi || !(pi.dataset.id || pi.dataset.camera)) return;
    lastActivate = Date.now();
    if (pi.dataset.id) ctx.onOpenDevice(pi.dataset.id);
    // Le changement d'onglet détruit le plan et sa garde : on attend que le `click` fantôme (avalé
    // ci-dessus pendant 600 ms) soit passé, sinon il tomberait sur une vignette de l'onglet Caméras.
    else setTimeout(() => { if (!destroyed) ctx.onOpenCameras(); }, 400);
  }
  svg.addEventListener("keydown", (e) => {
    const pi = e.target.closest?.(".pi");
    if (pi && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); activate(pi); return; }
    if (e.target !== svg) return;
    if (e.key === "+" || e.key === "=") zoomBy(1.5);
    else if (e.key === "-") zoomBy(1 / 1.5);
    else if (e.key === "0") recenter();
    else if (e.key.startsWith("Arrow") && view) {
      e.preventDefault();
      fitted = false;
      const d = 60;
      setView({ s: view.s, tx: view.tx + (e.key === "ArrowLeft" ? d : e.key === "ArrowRight" ? -d : 0), ty: view.ty + (e.key === "ArrowUp" ? d : e.key === "ArrowDown" ? -d : 0) });
    }
  });

  update();
  return {
    update,
    destroy() {
      destroyed = true;
      observer?.disconnect();
      document.removeEventListener("click", swallowGhostClick, true);
      if (raf) cancelAnimationFrame(raf);
      wrap.remove();
    },
  };
}
