// Pictogrammes en SVG (traits fins, couleur = currentColor), dessinés à la main comme ceux du
// client : jamais un glyphe de police.
const svg = (inner) =>
  `<svg viewBox="0 0 28 28" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${inner}</svg>`;

export const ICONS = {
  lightOn: svg('<path d="M14 3.5a7 7 0 0 0-4 12.7c.8.7 1.2 1.5 1.2 2.5v.8h5.6v-.8c0-1 .4-1.8 1.2-2.5A7 7 0 0 0 14 3.5z" fill="currentColor" fill-opacity=".25"/><path d="M11.5 23h5M12 20.5h4"/>'),
  lightOff: svg('<path d="M14 3.5a7 7 0 0 0-4 12.7c.8.7 1.2 1.5 1.2 2.5v.8h5.6v-.8c0-1 .4-1.8 1.2-2.5A7 7 0 0 0 14 3.5z"/><path d="M11.5 23h5M12 20.5h4"/>'),
  plugOn: svg('<path d="M10 3.5v5M18 3.5v5M7.5 8.5h13v4a6.5 6.5 0 0 1-13 0z" fill="currentColor" fill-opacity=".25"/><path d="M14 19v5"/>'),
  plugOff: svg('<path d="M10 3.5v5M18 3.5v5M7.5 8.5h13v4a6.5 6.5 0 0 1-13 0z"/><path d="M14 19v5"/>'),
  doorClosed: svg('<rect x="7" y="3.5" width="14" height="21" rx="1.5"/><circle cx="17.5" cy="14.5" r="1" fill="currentColor"/>'),
  doorOpen: svg('<path d="M7 24.5V4.5l10 2.5v17z"/><path d="M17 8.5h4v16"/><circle cx="14.2" cy="15" r=".9" fill="currentColor"/>'),
  windowClosed: svg('<rect x="5" y="4.5" width="18" height="19" rx="1.5"/><path d="M14 4.5v19M5 14h18"/>'),
  windowOpen: svg('<path d="M5 4.5h18v19H5z"/><path d="M14 4.5l7 3v13l-7 3.5"/>'),
  thermometer: svg('<path d="M12 16.2V5.5a2 2 0 0 1 4 0v10.7a4.5 4.5 0 1 1-4 0z"/><path d="M14 10v8"/>'),
  button: svg('<circle cx="14" cy="14" r="9"/><circle cx="14" cy="14" r="3.5" fill="currentColor" fill-opacity=".25"/>'),
  generic: svg('<rect x="5.5" y="5.5" width="17" height="17" rx="4"/><circle cx="14" cy="14" r="2" fill="currentColor"/>'),
  bridge: svg('<rect x="4" y="9" width="20" height="10" rx="2"/><path d="M8 14h.01M12 14h.01M16 14h4"/>'),
  camera: svg('<rect x="3.5" y="8" width="15" height="12" rx="2"/><path d="M18.5 12.5l6-3v9l-6-3"/>'),
  home: svg('<path d="M4 13.5L14 4.5l10 9"/><path d="M6.5 12v11.5h15V12"/><path d="M11.5 23.5v-6h5v6"/>'),
  // Plein écran (⤢) et sortie du plein écran (⤡) : deux flèches qui s'écartent ou se rapprochent.
  fullscreenEnter: svg('<path d="M16.5 4.5h7v7M23.5 4.5l-8 8M11.5 23.5h-7v-7M4.5 23.5l8-8"/>'),
  fullscreenExit: svg('<path d="M23.5 12.5h-7v-7M16.5 12.5l8-8M4.5 15.5h7v7M11.5 15.5l-8 8"/>'),
  thermo: svg('<path d="M12 16.2V5.5a2 2 0 0 1 4 0v10.7a4.5 4.5 0 1 1-4 0z"/>'),
};

/** Volet : le rideau descend d'autant que le volet est fermé (`open` en pour cent, ou inconnu). */
export function shutterIcon(open) {
  const closed = open == null ? 0.5 : (100 - Math.max(0, Math.min(100, open))) / 100;
  const h = Math.round(15 * closed);
  const slats = [];
  for (let y = 11; y < 9.5 + h; y += 3) slats.push(`<path d="M7 ${y.toFixed(1)}h14"/>`);
  // Le caisson en haut est toujours plein ; un volet ouvert montre une flèche vers le haut.
  const arrow = open != null && open >= 99 ? '<path d="M10.5 18.5l3.5-3.5 3.5 3.5"/>' : "";
  return svg(`<rect x="5" y="4.5" width="18" height="20" rx="1.5"/><rect x="5" y="4.5" width="18" height="5" rx="1.5" fill="currentColor" fill-opacity=".35"/>${slats.join("")}${arrow}${open == null ? '<path d="M14 17v3" />' : ""}`);
}

export function mountIcons(root = document) {
  root.querySelectorAll("[data-ico]").forEach((el) => { el.innerHTML = ICONS[el.dataset.ico] ?? ""; });
}
