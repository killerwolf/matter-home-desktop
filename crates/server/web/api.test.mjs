import assert from "node:assert/strict";
import { HOUSE_HINT, HOUSE_HINT_LOCAL, houseHint, rememberPublicOrigin } from "./api.js";

// « La maison ne répond pas » : deux pistes selon l'adresse par laquelle on joint le Mac.
assert.equal(houseHint("192.168.1.20"), HOUSE_HINT_LOCAL);
assert.equal(houseHint("mac-mini.local"), HOUSE_HINT_LOCAL);
assert.equal(houseHint("localhost"), HOUSE_HINT_LOCAL);
assert.equal(houseHint("mac-mini.exemple.ts.net"), HOUSE_HINT);
assert.equal(houseHint("100.101.102.103"), HOUSE_HINT);
assert.equal(houseHint("100.200.1.1"), HOUSE_HINT_LOCAL, "hors de 100.64.0.0/10");
assert.match(HOUSE_HINT_LOCAL, /n'est plus sur le Wi-Fi de la maison/);
assert.match(HOUSE_HINT, /Tailscale/);

// L'adresse publique réglée sur le serveur est une adresse de loin : la piste est Tailscale.
assert.equal(houseHint("maison.exemple.fr"), HOUSE_HINT_LOCAL);
rememberPublicOrigin("https://maison.exemple.fr");
assert.equal(houseHint("maison.exemple.fr"), HOUSE_HINT);
assert.equal(houseHint("evilmaison.exemple.fr"), HOUSE_HINT_LOCAL);
assert.equal(houseHint("192.168.1.20"), HOUSE_HINT_LOCAL);
rememberPublicOrigin(undefined);
assert.equal(houseHint("maison.exemple.fr"), HOUSE_HINT_LOCAL);
