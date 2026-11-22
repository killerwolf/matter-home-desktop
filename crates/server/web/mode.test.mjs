// Essai à la main (non embarqué dans le serveur) : node crates/server/web/mode.test.mjs
// Le mode de la maison : valeurs, libellés, et caméras éteintes selon le mode.
import assert from "node:assert/strict";
import { DEFAULT_MODE, MODES, cameraBehavior, cameraOff, isMode, modeChangeNotice, modeLabel, offText, sourceLabel } from "./mode.js";

// Les quatre modes du serveur, dans l'ordre, avec des libellés français.
assert.deepEqual(MODES.map((m) => m.id), ["present", "absent", "nuit", "vacances"]);
assert.deepEqual(MODES.map((m) => m.label), ["Présent", "Absent", "Nuit", "Vacances"]);
assert.equal(DEFAULT_MODE, "present");
assert.ok(MODES.every((m) => isMode(m.id)));
assert.equal(isMode("dehors"), false);
assert.equal(isMode(undefined), false);

// Un mode inconnu (serveur plus récent) se lit comme « Présent » plutôt que de casser l'écran.
assert.equal(modeLabel("nuit"), "Nuit");
assert.equal(modeLabel("inconnu"), "Présent");
assert.equal(modeLabel(undefined), "Présent");

// Caméras : le tableau du serveur décide ; sans tableau (serveur plus ancien), elle diffuse.
const salon = { id: "1", modes: { present: "eteinte", absent: "surveillance", nuit: "eteinte", vacances: "surveillance" } };
assert.equal(cameraBehavior(salon, "present"), "eteinte");
assert.equal(cameraOff(salon, "present"), true);
assert.equal(cameraOff(salon, "absent"), false);
assert.equal(cameraOff(salon, "nuit"), true);
assert.equal(cameraOff({ id: "2" }, "present"), false);
assert.equal(cameraOff(undefined, "present"), false);
assert.equal(cameraBehavior({ modes: {} }, "present"), "diffusion");
assert.equal(offText("nuit"), "Éteinte en mode Nuit");

// D'où vient un changement de mode : chaque source a ses mots ; un ancien serveur n'en dit rien.
assert.equal(sourceLabel({ kind: "shortcut" }), "raccourci iPhone");
assert.equal(sourceLabel({ kind: "web" }), "app web");
assert.equal(sourceLabel({ kind: "desktop" }), "app de bureau");
assert.equal(sourceLabel({ kind: "server_app" }), "fenêtre du serveur");
assert.equal(sourceLabel({ kind: "rule", name: "Nuit à 23 h" }), "automatisation « Nuit à 23 h »");
assert.equal(sourceLabel({ kind: "rule" }), "automatisation");
assert.equal(sourceLabel({ kind: "widget" }), "widget iPhone");
assert.equal(sourceLabel({ kind: "other" }), "autre");
assert.equal(sourceLabel({ kind: "montre" }), "");
assert.equal(sourceLabel(undefined), "");
// La notification : brève, et pas pour le changement que cette app vient de demander.
assert.equal(modeChangeNotice("absent", { kind: "shortcut" }, null), "Mode Absent · raccourci iPhone");
assert.equal(modeChangeNotice("nuit", { kind: "rule", name: "Nuit à 23 h" }, null), "Mode Nuit · automatisation « Nuit à 23 h »");
assert.equal(modeChangeNotice("nuit", undefined, null), "Mode Nuit");
assert.equal(modeChangeNotice("nuit", { kind: "web" }, "nuit"), null);
assert.equal(modeChangeNotice("nuit", undefined, "nuit"), null);
assert.equal(modeChangeNotice("nuit", { kind: "web" }, null), "Mode Nuit · app web", "une autre app web");
assert.equal(modeChangeNotice("nuit", { kind: "desktop" }, "nuit"), "Mode Nuit · app de bureau");
assert.equal(modeChangeNotice("dehors", { kind: "web" }, null), null);

console.log("mode.test.mjs : ok");
