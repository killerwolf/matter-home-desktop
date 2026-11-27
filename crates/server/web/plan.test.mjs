// Essai à la main (non embarqué dans le serveur) : node crates/server/web/plan.test.mjs
// La géométrie de la page Plan : ajustement, bornes de zoom, pincement, étage à montrer d'abord.
import assert from "node:assert/strict";
import { boundsOf, clampView, firstFilledFloor, fitView, initialView, limitsFor, pinchView, planIsEmpty, shutterState, stageHeightFor, zoomAt } from "./plan.js";

const floor = {
  id: 1,
  rooms: [{ id: 2, name: "Salon", x: 0, y: 0, w: 6, h: 4.5 }, { id: 3, name: "Cuisine", x: 6, y: 0, w: 4, h: 3 }],
  items: [{ id: 10, kind: { type: "Device", node_id: "1" }, x: 9, y: 4.25 }],
};
const near = (a, b, eps = 1e-9) => assert.ok(Math.abs(a - b) < eps, `${a} != ${b}`);

// Bornes : pièces et objets, avec la marge ; un étage vide n'en a pas.
{
  const b = boundsOf(floor);
  assert.deepEqual(b, { x0: -0.5, y0: -0.5, x1: 10.5, y1: 5 });
  assert.equal(boundsOf({ rooms: [], items: [] }), null);
  assert.equal(boundsOf(undefined), null);
  // Un objet hors des pièces agrandit les bornes.
  assert.equal(boundsOf({ rooms: floor.rooms, items: [{ x: 14, y: 1 }] }).x1, 14.5);
}

// Ajustement : tout le plan tient (iPhone 375 x 640, iPad 820 x 1000), centré.
for (const [w, h] of [[375, 640], [820, 1000], [1280, 700]]) {
  const b = boundsOf(floor);
  const v = fitView(b, w, h);
  const left = v.tx + b.x0 * v.s;
  const right = v.tx + b.x1 * v.s;
  const top = v.ty + b.y0 * v.s;
  const bottom = v.ty + b.y1 * v.s;
  assert.ok(left >= 0 && right <= w && top >= 0 && bottom <= h, `tient dans ${w}x${h}`);
  near(left, w - right, 1e-6); // centré en largeur
  near(top, h - bottom, 1e-6); // et en hauteur
  assert.ok(v.s <= 90);
}

// Hauteur du cadre : celle du plan ajusté (marges comprises), le CSS la borne ensuite.
{
  const b = boundsOf(floor);
  const h = stageHeightFor(b, 390);
  const v = fitView(b, 390, h - 30);
  assert.ok(v.s <= 90);
  const top = v.ty + b.y0 * v.s;
  assert.ok(top >= 0 && top <= 17.5, "marge haute d'environ 16 px : pas de vide autour du plan");
}

// Ouverture : toujours le plan entier (l'ajustement complet), en portrait comme en paysage.
{
  const b = boundsOf(floor);
  for (const [w, h] of [[375, 640], [820, 1000], [1280, 700]]) assert.deepEqual(initialView(b, w, h), fitView(b, w, h));
  const tall = { x0: 0, y0: 0, x1: 4, y1: 10 };
  assert.deepEqual(initialView(tall, 375, 640), fitView(tall, 375, 640));
}

// Zoom autour d'un point : le point du plan sous le doigt ne bouge pas.
{
  const v = { s: 30, tx: 20, ty: 40 };
  const z = zoomAt(v, 2, 150, 200);
  near(z.s, 60);
  near((150 - z.tx) / z.s, (150 - v.tx) / v.s);
  near((200 - z.ty) / z.s, (200 - v.ty) / v.s);
}

// Pincement : les deux doigts s'écartent de 1 à 2 fois, le milieu suit.
{
  const start = { s: 30, tx: 20, ty: 40 };
  const p = pinchView(start, 100, { x: 150, y: 200 }, 200, { x: 170, y: 210 });
  near(p.s, 60);
  near((170 - p.tx) / p.s, (150 - start.tx) / start.s); // même point du plan sous le milieu
  near((210 - p.ty) / p.s, (200 - start.ty) / start.s);
  assert.equal(pinchView(start, 0, { x: 0, y: 0 }, 50, { x: 0, y: 0 }).s, 30, "écart nul : pas de division");
}

// Limites : échelle bornée, un bout du plan reste à l'écran.
{
  const b = boundsOf(floor);
  const fit = fitView(b, 375, 640);
  const lim = limitsFor(fit);
  assert.equal(clampView({ ...fit, s: 1e6 }, b, 375, 640, lim).s, lim.max);
  assert.equal(clampView({ ...fit, s: 0.001 }, b, 375, 640, lim).s, lim.min);
  const far = clampView({ s: fit.s, tx: 1e6, ty: -1e6 }, b, 375, 640, lim);
  assert.ok(far.tx + b.x0 * far.s < 375, "le bord gauche reste dans l'écran");
  assert.ok(far.ty + b.y1 * far.s > 0, "le bord bas reste dans l'écran");
  assert.deepEqual(clampView(fit, b, 375, 640, lim), fit, "la vue ajustée est déjà dans les limites");
}

// Étage montré d'abord, plan vide.
{
  const plan = { floors: [{ id: 1, rooms: [], items: [] }, floor] };
  assert.equal(firstFilledFloor(plan), 1);
  assert.equal(firstFilledFloor({ floors: [] }), 0);
  assert.equal(firstFilledFloor(null), 0);
  assert.equal(planIsEmpty(plan), false);
  assert.equal(planIsEmpty({ floors: [{ id: 1, rooms: [], items: [] }] }), true);
  assert.equal(planIsEmpty(null), true);
}

console.log("plan.test.mjs : ok");
