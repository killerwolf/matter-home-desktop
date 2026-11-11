// Courbes de température en SVG dessiné à la main, sans bibliothèque. Les couleurs viennent des
// classes CSS (.s1 à .s8, variables --color-series-*), donc elles suivent le thème.
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
      // Une seule mesure : un point bien visible (la couleur vient de la variable du thème).
      const dot = el("circle", { cx: x(pts[0].at).toFixed(1), cy: y(pts[0].value).toFixed(1), r: 4 });
      dot.style.fill = `var(--color-series-${s.colorIndex})`;
      svg.appendChild(dot);
    }
  }
  return svg;
}
