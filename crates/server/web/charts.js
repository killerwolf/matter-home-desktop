// Courbes de température en SVG dessiné à la main, sans bibliothèque. Les couleurs viennent des
// classes CSS (.s1 à .s8, variables --color-series-*), donc elles suivent le thème.
// `lineChart` sert la fiche d'un appareil ; `tempChart` (toutes les courbes sur un graphique, avec
// curseur) sert la page Températures. Les règles de `tempChart` sont celles du client de bureau
// (crates/client/src/temperatures.rs) : heures rondes en heure locale, courbe coupée sur les
// vrais trous, virgule décimale.
const NS = "http://www.w3.org/2000/svg";
const el = (name, attrs = {}, text) => {
  const node = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
  if (text !== undefined) node.textContent = text;
  return node;
};

/** Un pas « rond » (1, 2, 5 × 10^n) pour quelques repères sur l'axe vertical. */
export function niceStep(range, ticks = 4) {
  const raw = Math.max(range, 0.5) / ticks;
  const pow = 10 ** Math.floor(Math.log10(raw));
  const f = raw / pow;
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * pow;
}

export const hourLabel = (seconds) => new Date(seconds * 1000).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });

/** Minimum et maximum d'un ensemble de séries ; null s'il n'y a aucun point. */
export function rangeOf(series) {
  let lo = Infinity;
  let hi = -Infinity;
  for (const s of series) for (const p of s.points) { lo = Math.min(lo, p.value); hi = Math.max(hi, p.value); }
  return lo === Infinity ? null : { lo, hi };
}

/**
 * series : [{ colorIndex (1 à 8), points: [{at, value}] }] ; start/end en secondes Unix.
 * Rend un <svg> adaptatif (viewBox), avec des repères horaires toutes les 6 h.
 */
export function lineChart(series, start, end, { width = 340, height = 150 } = {}) {
  const pad = { l: 34, r: 8, t: 8, b: 20 };
  const svg = el("svg", { viewBox: `0 0 ${width} ${height}`, class: "chart", role: "img", "aria-label": "Courbe de température sur 24 heures" });
  const range = rangeOf(series);
  if (!range) {
    svg.appendChild(el("text", { x: width / 2, y: height / 2, "text-anchor": "middle" }, "Aucune mesure sur cette période"));
    return svg;
  }
  const step = niceStep(range.hi - range.lo);
  const yMin = Math.floor((range.lo - 0.01) / step) * step;
  const yMax = Math.max(Math.ceil((range.hi + 0.01) / step) * step, yMin + step);
  const x = (t) => pad.l + ((t - start) / (end - start)) * (width - pad.l - pad.r);
  const y = (v) => pad.t + (1 - (v - yMin) / (yMax - yMin)) * (height - pad.t - pad.b);
  for (let v = yMin; v <= yMax + 1e-9; v += step) {
    svg.appendChild(el("line", { class: "grid", x1: pad.l, x2: width - pad.r, y1: y(v), y2: y(v) }));
    svg.appendChild(el("text", { x: pad.l - 4, y: y(v) + 3, "text-anchor": "end" }, `${Math.round(v * 10) / 10}`.replace(".", ",")));
  }
  // Repères horaires : toutes les 6 h, sur des heures rondes locales.
  const first = new Date(start * 1000);
  first.setMinutes(0, 0, 0);
  for (let t = first.getTime() / 1000; t <= end; t += 3600) {
    if (t < start) continue;
    if (new Date(t * 1000).getHours() % 6 !== 0) continue;
    svg.appendChild(el("line", { class: "grid", x1: x(t), x2: x(t), y1: pad.t, y2: height - pad.b }));
    svg.appendChild(el("text", { x: x(t), y: height - 5, "text-anchor": "middle" }, `${new Date(t * 1000).getHours()} h`));
  }
  for (const s of series) {
    const pts = s.points.filter((p) => p.at >= start && p.at <= end);
    if (!pts.length) continue;
    // Un trou de données reste un trou : on coupe la ligne au-delà de 90 min sans mesure.
    let d = "";
    let prev = null;
    for (const p of pts) {
      d += prev === null || p.at - prev > 5400 ? `M${x(p.at).toFixed(1)} ${y(p.value).toFixed(1)}` : `L${x(p.at).toFixed(1)} ${y(p.value).toFixed(1)}`;
      prev = p.at;
    }
    svg.appendChild(el("path", { class: `line s${s.colorIndex}`, d }));
    if (pts.length === 1) {
      // Une seule mesure : un point bien visible (la couleur vient de la classe de la série).
      svg.appendChild(el("circle", { class: `dot s${s.colorIndex}`, cx: x(pts[0].at).toFixed(1), cy: y(pts[0].value).toFixed(1), r: 4 }));
    }
  }
  return svg;
}

// ---------------------------------------------------------------------------
// Page Températures : toutes les courbes sur un graphique
// ---------------------------------------------------------------------------

/** Plus petit trou de données (secondes) qui coupe une courbe. */
export const MIN_GAP_S = 20 * 60;

/** Trou au-delà duquel deux mesures ne sont plus reliées : le plus grand de 20 minutes, de
 *  2,5 fois l'intervalle de rapport de l'appareil et de 3 tranches du graphique. */
export function gapThreshold(mode, windowS, buckets) {
  const report = mode?.mode === "subscription" ? mode.max_interval_s : mode?.mode === "polling" ? mode.every_s : 0;
  return Math.max(MIN_GAP_S, 2.5 * (report || 0), 3 * (windowS / buckets));
}

/**
 * La ligne d'une courbe : un point par tranche d'environ trois pixels (moyenne des mesures de la
 * tranche), coupée seulement sur une plage de tranches vides plus longue que `gapS`.
 * Rend des segments `[[{at, value}, …], …]`.
 */
export function lineSegments(points, start, end, buckets, gapS) {
  const n = Math.max(1, Math.floor(buckets / 3));
  const span = end - start;
  if (!(span > 0)) return [];
  const width = span / n;
  const cells = Array.from({ length: n }, () => ({ t: 0, v: 0, count: 0 }));
  for (const p of points) {
    if (!Number.isFinite(p.value) || p.at < start) continue;
    const i = Math.min(n - 1, Math.max(0, Math.floor(((p.at - start) / span) * n)));
    const c = cells[i];
    c.t += p.at;
    c.v += p.value;
    c.count += 1;
  }
  const segments = [];
  let emptyRun = 0;
  for (const c of cells) {
    if (c.count === 0) { emptyRun += 1; continue; }
    if (!segments.length || emptyRun * width > gapS) segments.push([]);
    emptyRun = 0;
    segments[segments.length - 1].push({ at: c.t / c.count, value: c.v / c.count });
  }
  return segments;
}

/** Valeur de la courbe à l'instant `t` : la dernière mesure avant `t`, tant qu'elle date de
 *  moins de `gapS` secondes ; `null` avant la première mesure ou dans un trou. */
export function valueAt(points, t, gapS) {
  let lo = 0;
  let hi = points.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (points[mid].at <= t) lo = mid + 1;
    else hi = mid;
  }
  if (lo === 0) return null;
  const p = points[lo - 1];
  return t - p.at <= gapS ? p.value : null;
}

/** Plus bas et plus haut d'une liste de points (valeurs finies) ; `null` sans point. */
export function periodRange(points) {
  let lo = Infinity;
  let hi = -Infinity;
  for (const p of points) if (Number.isFinite(p.value)) { lo = Math.min(lo, p.value); hi = Math.max(hi, p.value); }
  return lo === Infinity ? null : { lo, hi };
}

/** Écart (s) entre les repères de l'axe du temps : des heures rondes, et peu de repères. */
export function axisStep(spanS) {
  if (spanS <= 2 * 3600) return 15 * 60;
  if (spanS <= 12 * 3600) return 3600;
  if (spanS <= 36 * 3600) return 6 * 3600;
  return 86400;
}

/** Écart du fuseau local à UTC (secondes) à l'instant `t`. */
export const offsetAt = (t) => -new Date(t * 1000).getTimezoneOffset() * 60;

/** Repères de l'axe entre `start` et `end` : multiples du pas à l'heure locale. `offset(t)`
 *  est relu pour chaque repère, de sorte qu'un changement d'heure tombe juste. */
export function roundMarks(start, end, offset = offsetAt) {
  const step = axisStep(end - start);
  const firstOffset = offset(start);
  let k = Math.ceil((start + firstOffset) / step);
  const marks = [];
  for (let guard = 0; guard < 400; guard++, k++) {
    const local = k * step;
    const first = local - offset(local - firstOffset);
    const second = local - offset(first);
    const found = [second, first].find((t) => t + offset(t) === local);
    if ((found ?? first) > end) break;
    if (found !== undefined && found >= start) marks.push(found);
  }
  return { marks, step };
}

const DAYS = ["dim.", "lun.", "mar.", "mer.", "jeu.", "ven.", "sam."];
const pad2 = (n) => String(n).padStart(2, "0");

/** « 18 h » ; « 14:15 » pour un pas de quinze minutes ; « lun. 04/11 » pour un pas d'un jour. */
export function axisLabel(t, step) {
  const d = new Date(t * 1000);
  if (step >= 86400) return `${DAYS[d.getDay()]} ${pad2(d.getDate())}/${pad2(d.getMonth() + 1)}`;
  if (step >= 3600) return `${d.getHours()} h`;
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

/** Heure du curseur : « 14:35 », précédée du jour quand la période dépasse une journée et demie. */
export function cursorLabel(t, spanS) {
  const d = new Date(t * 1000);
  const hm = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  return spanS > 36 * 3600 ? `${DAYS[d.getDay()]} ${pad2(d.getDate())}/${pad2(d.getMonth() + 1)} ${hm}` : hm;
}

/** Repères de l'axe vertical : un tous les 0,5, 1, 2, 5, 10 ou 20 degrés, sept au plus. */
export function yMarks(lo, hi) {
  const span = Math.abs(hi - lo);
  if (!Number.isFinite(span) || span === 0) return { marks: [], step: 1 };
  const step = [0.5, 1, 2, 5, 10, 20].find((s) => span / s <= 7) ?? 50;
  const marks = [];
  for (let v = Math.ceil(Math.min(lo, hi) / step) * step; v <= Math.max(lo, hi) + 1e-9; v += step) marks.push(Math.round(v * 100) / 100);
  return { marks, step };
}

/** « 21 °C », ou « 20,5 °C » quand le pas des repères est inférieur à un degré. */
export const yLabel = (v, step) => (step < 1 ? `${v.toFixed(1).replace(".", ",")} °C` : `${v.toFixed(0)} °C`);

/**
 * series : [{ colorIndex (1 à 8), dashed, segments: [[{at, value}, …], …] }] ; start/end en
 * secondes Unix. Rend { svg, tAt(px), xOf(t), hasData, cursor: { show(t, items), hide() } } ;
 * `items` : [{ colorIndex, value }] (les points marqués sur le curseur).
 */
export function tempChart(series, start, end, { width = 340, height = 220 } = {}) {
  const pad = { l: 46, r: 10, t: 8, b: 20 };
  const svg = el("svg", { viewBox: `0 0 ${width} ${height}`, class: "chart tchart", role: "img", "aria-label": "Courbes de température" });
  const plotW = width - pad.l - pad.r;
  const x = (t) => pad.l + ((t - start) / (end - start)) * plotW;
  const range = periodRange(series.flatMap((s) => s.segments.flat()));
  const api = {
    svg,
    xOf: x,
    tAt: (px) => start + ((Math.min(Math.max(px, pad.l), width - pad.r) - pad.l) / plotW) * (end - start),
    cursor: { show() {}, hide() {} },
    hasData: range !== null,
  };
  if (!range) {
    svg.appendChild(el("text", { x: width / 2, y: height / 2, "text-anchor": "middle" }, series.length ? "Aucune mesure sur cette période" : "Aucune courbe affichée"));
    return api;
  }
  const margin = Math.max((range.hi - range.lo) * 0.1, 0.25);
  const yMin = range.lo - margin;
  const yMax = range.hi + margin;
  const { marks, step: ystep } = yMarks(yMin, yMax);
  const y = (v) => pad.t + (1 - (v - yMin) / (yMax - yMin)) * (height - pad.t - pad.b);
  for (const v of marks) {
    svg.appendChild(el("line", { class: "grid", x1: pad.l, x2: width - pad.r, y1: y(v).toFixed(1), y2: y(v).toFixed(1) }));
    svg.appendChild(el("text", { x: pad.l - 5, y: (y(v) + 3).toFixed(1), "text-anchor": "end" }, yLabel(v, ystep)));
  }
  const { marks: xm, step } = roundMarks(start, end);
  // Pas assez de place pour tous les repères : un sur deux, un sur trois…
  const labelW = step >= 86400 ? 62 : step >= 3600 ? 34 : 40;
  const every = Math.max(1, Math.ceil((labelW + 6) / Math.max(1, (plotW * step) / (end - start))));
  xm.forEach((t, i) => {
    svg.appendChild(el("line", { class: "grid", x1: x(t).toFixed(1), x2: x(t).toFixed(1), y1: pad.t, y2: height - pad.b }));
    if (i % every === 0) svg.appendChild(el("text", { x: x(t).toFixed(1), y: height - 6, "text-anchor": "middle" }, axisLabel(t, step)));
  });
  for (const s of series) {
    let d = "";
    for (const seg of s.segments) {
      if (seg.length === 1) {
        svg.appendChild(el("circle", { class: `dot s${s.colorIndex}`, cx: x(seg[0].at).toFixed(1), cy: y(seg[0].value).toFixed(1), r: 2.5 }));
        continue;
      }
      d += seg.map((p, i) => `${i ? "L" : "M"}${x(p.at).toFixed(1)} ${y(p.value).toFixed(1)}`).join("");
    }
    if (d) svg.appendChild(el("path", { class: `line s${s.colorIndex}${s.dashed ? " dash" : ""}`, d }));
  }
  const line = el("line", { class: "cursor", y1: pad.t, y2: height - pad.b, x1: 0, x2: 0, visibility: "hidden" });
  const dots = el("g", {});
  svg.append(line, dots);
  api.cursor = {
    show(t, items) {
      const cx = x(t).toFixed(1);
      line.setAttribute("x1", cx);
      line.setAttribute("x2", cx);
      line.setAttribute("visibility", "visible");
      dots.replaceChildren(...items.filter((i) => i.value !== null).map((i) => el("circle", { class: `dot s${i.colorIndex}`, cx, cy: y(i.value).toFixed(1), r: 3.5 })));
    },
    hide() {
      line.setAttribute("visibility", "hidden");
      dots.replaceChildren();
    },
  };
  return api;
}
