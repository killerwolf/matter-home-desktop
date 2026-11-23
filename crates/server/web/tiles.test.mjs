// Essai à la main (non embarqué dans le serveur) : node crates/server/web/tiles.test.mjs
// La phrase d'état : jumelle de protocol::status (crates/protocol/src/status.rs), mêmes cas, mêmes
// textes, mot pour mot. L'unité de compte est la catégorie, jamais l'appareil.
import assert from "node:assert/strict";
import { droppedTogether, allReachable, clockText, headline, unreachableSince, statusSummary, summarize, unreachablePhrase } from "./tiles.js";

const names = (...list) => list;
const devices = (...p) => ({ kind: "devices", names: p });
const cameras = (...p) => ({ kind: "cameras", names: p });
const other = (text) => ({ kind: "other", text });

// Aucun point : la maison va bien.
{
  assert.equal(headline(0), "La maison va bien.");
  assert.equal(summarize([], 10).count, 0);
  assert.equal(summarize([], 10).detail, "10 appareils, tous joignables.");
  assert.equal(allReachable(10), "10 appareils, tous joignables.");
  assert.equal(allReachable(1), "1 appareil, joignable.");
  assert.equal(allReachable(0), "Aucun appareil.");
}

// Un point, un appareil.
{
  const s = summarize([devices("Lampe d'Adam")], 10);
  assert.equal(s.title, "Une chose à regarder");
  assert.equal(s.detail, "« Lampe d'Adam » est injoignable.");
}

// Un point, deux appareils.
{
  const s = summarize([devices("Lampe d'Adam", "Prise TV")], 10);
  assert.equal(s.title, "Une chose à regarder");
  assert.equal(s.detail, "« Lampe d'Adam » et « Prise TV » sont injoignables.");
}

// Un point, trois appareils ou plus : « et 1 autre », « et 2 autres ».
{
  assert.equal(summarize([devices("Lampe d'Adam", "Prise TV", "Frigo")], 10).detail, "« Lampe d'Adam », « Prise TV » et 1 autre sont injoignables.");
  const four = summarize([devices("A", "B", "C", "D")], 10);
  assert.equal(four.title, "Une chose à regarder");
  assert.equal(four.detail, "« A », « B » et 2 autres sont injoignables.");
}

// Un point, des caméras.
{
  const one = summarize([cameras("Jardin")], 10);
  assert.equal(one.title, "Une chose à regarder");
  assert.equal(one.detail, "La caméra « Jardin » est injoignable.");
  assert.equal(summarize([cameras("Entrée", "Jardin")], 10).detail, "Les caméras « Entrée » et « Jardin » sont injoignables.");
  assert.equal(summarize([cameras("A", "B", "C")], 10).detail, "Les caméras « A », « B » et 1 autre sont injoignables.");
}

// Deux points (appareils et caméras) : « Deux choses à regarder », et le détail commence par les
// appareils même quand les caméras sont données avant.
{
  const s = summarize([cameras("Jardin"), devices("Lampe d'Adam", "Prise TV")], 10);
  assert.equal(s.title, "Deux choses à regarder");
  assert.equal(s.detail, "« Lampe d'Adam » et « Prise TV » sont injoignables.");
}

// Trois points ou plus : le nombre en chiffres, la phrase du premier point (les caméras avant les autres).
{
  const s = summarize([other("L'accès à distance est coupé."), cameras("Jardin"), other("La détection est suspendue par la chaleur.")], 10);
  assert.equal(s.title, "3 choses à regarder");
  assert.equal(s.detail, "La caméra « Jardin » est injoignable.");
  assert.equal(headline(4), "4 choses à regarder");
  assert.equal(summarize([other("A."), other("B."), other("C.")], 10).detail, "A.");
}

// L'unité de compte est la catégorie : cinq appareils injoignables font un seul point.
{
  const s = summarize([devices("L1", "L2", "P1", "P2", "P3")], 10);
  assert.equal(s.count, 1);
  assert.equal(s.title, "Une chose à regarder");
}

// Un point vide ne compte pas.
{
  const s = summarize([devices(), cameras(), other("  ")], 10);
  assert.equal(s.count, 0);
  assert.equal(s.title, "La maison va bien.");
  assert.equal(s.detail, "10 appareils, tous joignables.");
}
assert.equal(unreachablePhrase([]), "");

// statusSummary : à partir des appareils de l'app (Map) et des caméras du serveur.
{
  const device = (id, name, state, extra = {}) => [id, { id, info: { name, caps: {}, device_types: [], ...extra }, status: { state }, latest: {} }];
  const home = new Map([
    device("1", "Lampe canapé", "online"),
    device("2", "Lampe d'Adam", "unreachable"),
    device("3", "Prise TV", "online"),
    // Un appareil qui se connecte encore n'est pas à regarder.
    device("4", "Sonde salon", "connecting"),
    // Un pont n'entre ni dans le compte ni dans les noms, même injoignable.
    device("9", "Passerelle TaHoma", "unreachable", { device_types: ["Passerelle"] }),
  ]);
  const s = statusSummary(home, []);
  assert.equal(s.title, "Une chose à regarder");
  assert.equal(s.detail, "« Lampe d'Adam » est injoignable.");
  assert.equal(s.ok, false);
  assert.equal(s.count, 1);

  const fine = new Map([device("1", "Lampe canapé", "online"), device("2", "Prise TV", "online"), device("9", "Passerelle TaHoma", "online", { device_types: ["Passerelle"] })]);
  const ok = statusSummary(fine, [{ name: "Entrée", status: { state: "online" } }]);
  assert.equal(ok.title, "La maison va bien.");
  assert.equal(ok.detail, "2 appareils, tous joignables.", "le pont n'est pas compté");
  assert.equal(ok.ok, true);

  const two = statusSummary(home, [{ name: "Jardin", status: { state: "unreachable" } }, { name: "Entrée", status: { state: "online" } }]);
  assert.equal(two.title, "Deux choses à regarder");
  assert.equal(two.detail, "« Lampe d'Adam » est injoignable.");
  assert.equal(statusSummary(new Map(), [{ name: "Jardin", status: { state: "unreachable" } }]).detail, "La caméra « Jardin » est injoignable.");
  assert.equal(statusSummary(new Map(), undefined).detail, "Aucun appareil.");
}

// Panne d'ensemble : tous (ou ≥ 80 % et au moins 3) injoignables en même temps.
{
  const device = (id, name, state, extra = {}) => [id, { id, info: { name, caps: {}, device_types: [], ...extra }, status: { state }, latest: {} }];
  const all = statusSummary(new Map([device("1", "A", "unreachable"), device("2", "B", "unreachable"), device("3", "C", "unreachable")]), []);
  assert.equal(all.title, "Une chose à regarder");
  assert.equal(all.detail, "Tous les appareils ont décroché en même temps : le Mac n'est sans doute plus sur le réseau de la maison (Wi-Fi, IPv6). Vérifiez le Wi-Fi du Mac.");
  assert.equal(all.count, 1);
  const most = new Map(["1", "2", "3", "4"].map((i) => device(i, `L${i}`, "unreachable")));
  most.set("5", device("5", "L5", "online")[1]);
  assert.match(statusSummary(most, []).detail, /^La plupart des appareils ont décroché en même temps/);
  // Deux appareils sur deux : trop peu pour parler du réseau du Mac.
  assert.equal(statusSummary(new Map([device("1", "A", "unreachable"), device("2", "B", "unreachable")]), []).detail, "« A » et « B » sont injoignables.");
  // 3 sur 5 : sous le seuil, les noms.
  const some = new Map([device("1", "A", "unreachable"), device("2", "B", "unreachable"), device("3", "C", "unreachable"), device("4", "D", "online"), device("5", "E", "online")]);
  assert.equal(statusSummary(some, []).detail, "« A », « B » et 1 autre sont injoignables.");
  assert.equal(droppedTogether(4, 5), true);
  assert.equal(droppedTogether(3, 5), false);
  assert.equal(droppedTogether(2, 2), false);
  assert.equal(summarize([{ kind: "together", all: true }, cameras("Jardin")], 3).count, 2);
}

// L'heure, sur deux chiffres.
assert.equal(clockText(new Date(2026, 10, 22, 8, 5).getTime()), "08:05");
assert.equal(clockText(new Date(2026, 10, 22, 18, 42).getTime()), "18:42");

console.log("tiles.test.mjs : ok");

// « depuis HH:MM » : d'après last_seen (secondes) ; rien quand le serveur ne sait pas.
{
  const at = new Date(2026, 10, 22, 18, 2).getTime() / 1000;
  assert.equal(unreachableSince({ last_seen: at }), "depuis 18:02");
  assert.equal(unreachableSince({ last_seen: null }), null);
  assert.equal(unreachableSince({}), null);
}
