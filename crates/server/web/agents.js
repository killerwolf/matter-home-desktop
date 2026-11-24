// « Demandes à valider » : ce qu'un agent d'IA (Claude Code, Cursor…) demande par le serveur MCP et
// qui attend l'accord d'une personne (docs/agents-ia.md). Que de la logique pure et des phrases
// françaises, sans DOM, pour l'essayer à la main : node --test crates/server/web/agents.test.mjs
//
// Le serveur ne rend que du texte qu'il a fabriqué lui-même (phrase de l'action, motifs, noms
// d'automatisations nettoyés) et le libellé de l'agent. Rien de tout cela n'est une consigne : l'app
// l'affiche en texte brut (`textContent`), jamais en HTML.

/** Une personne ne voit que ses propres demandes ; un serveur plus ancien n'a pas la route. */
export const isMissingRoute = (error) => error?.status === 404 || error?.status === 405;

/** Au plus ce qu'on montre : un texte venu du serveur ne déborde jamais de la carte. */
const TEXT_MAX = 300;
const LIST_MAX = 8;

const text = (value) => (typeof value === "string" ? value.slice(0, TEXT_MAX) : "");
const list = (value) => (Array.isArray(value) ? value.filter((x) => typeof x === "string").slice(0, LIST_MAX).map(text) : []);

/** Le temps qui reste, en toutes lettres : « 4 min 12 s », « 45 s », « expirée ». */
export function remainingText(expires, now = Date.now() / 1000) {
  const left = Math.ceil(Number(expires) - now);
  if (!Number.isFinite(left) || left <= 0) return "expirée";
  if (left < 60) return `${left} s`;
  const minutes = Math.floor(left / 60);
  const seconds = left % 60;
  return seconds ? `${minutes} min ${seconds} s` : `${minutes} min`;
}

/** Les cartes à montrer : seulement celles qui ont un identifiant et une échéance, pas encore
 *  expirées, les plus anciennes d'abord. Les champs sont ramenés à du texte court. */
export function requestCards(response, now = Date.now() / 1000) {
  const items = Array.isArray(response?.requests) ? response.requests : [];
  return items
    .filter((r) => r && typeof r.id === "string" && /^[0-9a-f]{32}$/.test(r.id) && Number.isFinite(Number(r.expires)) && Number(r.expires) > now)
    .map((r) => ({
      id: r.id,
      agent: text(r.agent) || "Un agent d'IA",
      text: text(r.text),
      details: list(r.details),
      rules: list(r.rules),
      created: Number(r.created) || 0,
      expires: Number(r.expires),
    }))
    .sort((a, b) => a.created - b.created);
}

/** Le titre de la carte : « Claude Code demande ». */
export const titleOf = (card) => `${card.agent} demande`;

/** « Elle fera partir 2 automatisations : A, B. » (rien quand il n'y en a pas). */
export function rulesText(rules) {
  if (!rules.length) return "";
  const names = rules.map((n) => `« ${n} »`).join(", ");
  return rules.length === 1 ? `Elle ferait partir l'automatisation ${names}.` : `Elle ferait partir ${rules.length} automatisations : ${names}.`;
}

/** Ce que dit l'app quand « Autoriser » ou « Refuser » échoue. */
export function decisionError(error) {
  if (error?.status === 404) return "Cette demande n'existe plus (expirée, déjà traitée ou agent révoqué).";
  if (error?.status === 400) return "Cette demande n'est plus à valider.";
  if (error?.status === 403) return "Ce compte n'a pas le droit d'autoriser cette demande.";
  if (error?.status === 0) return "La maison ne répond pas : réessayez dans un instant.";
  return "Impossible de répondre pour l'instant.";
}

/** Dans combien de millisecondes revoir la liste : vite quand une demande attend, doucement sinon. */
export const refreshDelay = (cards) => (cards.length ? 5_000 : 20_000);

// ---------------------------------------------------------------------------------------------------
// « Mes agents » : créer, voir et révoquer ses agents d'IA (GET/POST /api/agents, DELETE
// /api/agents/{id}). Toujours de la logique pure : l'app dessine, ici on décide et on écrit les phrases.
// Le jeton n'est jamais dans ce fichier : seule l'app le tient, le temps de l'afficher une fois.
// ---------------------------------------------------------------------------------------------------

/** Les droits qu'on peut cocher, dans l'ordre. « Lecture » est toujours donné. */
export const SCOPE_CHOICES = [
  { key: "occupation", label: "Voir qui est à la maison et le mode", note: "Le mode « Vacances » dit que la maison est vide : à ne cocher que si l'agent en a besoin." },
  { key: "control", label: "Commander des prises, des lampes et des volets", note: "Seulement ceux que vous cochez ensuite, et jamais ceux qu'Ostal Server interdit." },
  { key: "mode", label: "Changer le mode de la maison", note: "Chaque changement qui désarme les caméras ou part en Vacances attend votre accord ici." },
];

export const DURATIONS = [
  { days: 7, label: "7 jours" },
  { days: 30, label: "30 jours" },
  { days: 90, label: "90 jours" },
  { days: 180, label: "180 jours" },
  { days: 365, label: "1 an" },
];

const SCOPE_WORDS = { read: "Lecture", occupation: "Présence et mode", control: "Commandes", mode: "Changer le mode" };

/** « Lecture, Commandes » : les droits d'un agent en quelques mots (un mot inconnu n'est pas montré). */
export const scopeWords = (scopes) =>
  (Array.isArray(scopes) ? scopes : []).filter((s) => typeof s === "string" && SCOPE_WORDS[s]).map((s) => SCOPE_WORDS[s]).join(", ");

const ID = /^[0-9]{1,20}$/;
const AGENT_ID = /^[0-9a-f]{32}$/;
const idList = (value) => (Array.isArray(value) ? value.filter((x) => typeof x === "string" && ID.test(x)) : []);

/** La réponse de `GET /api/agents`, ramenée à ce que l'app sait montrer (le reste est écarté). */
export function agentState(response) {
  const agents = (Array.isArray(response?.agents) ? response.agents : [])
    .filter((a) => a && typeof a.id === "string" && AGENT_ID.test(a.id))
    .map((a) => ({
      id: a.id,
      label: text(a.label) || "Agent",
      scopes: Array.isArray(a.scopes) ? a.scopes.filter((s) => typeof s === "string") : [],
      devices: idList(a.devices),
      created: Number(a.created) || 0,
      expires: Number(a.expires) || 0,
      expired: a.expired === true,
      lastUsed: Number.isFinite(Number(a.last_used)) && a.last_used != null ? Number(a.last_used) : null,
      callsToday: Number(a.calls_today) || 0,
    }))
    .sort((x, y) => y.created - x.created);
  return {
    enabled: response?.enabled === true,
    defaultDays: Number(response?.default_days) || 90,
    maxDays: Number(response?.max_days) || 365,
    maxDevices: Number(response?.max_devices) || 20,
    never: idList(response?.never),
    sensitive: idList(response?.sensitive),
    canControl: response?.can?.control === true,
    canMode: response?.can?.mode === true,
    agents,
  };
}

/** « Expire dans 29 jours », « Expire demain », « Expiré » : sans heure, la précision du jour suffit. */
export function expiryText(expires, now = Date.now() / 1000) {
  const left = Number(expires) - now;
  if (!Number.isFinite(left) || left <= 0) return "Expiré";
  const days = Math.ceil(left / 86_400);
  if (days <= 1) return "Expire aujourd'hui ou demain";
  return `Expire dans ${days} jours`;
}

/** Ce que le serveur sait de l'usage, **depuis son démarrage** : il le dit, pour ne pas faire croire
 *  qu'un agent muet n'a jamais servi. */
export function usageText(agent, now = Date.now() / 1000) {
  if (agent.lastUsed == null) return "Pas encore utilisé depuis le démarrage du serveur";
  const ago = Math.max(0, Math.round(now - agent.lastUsed));
  const when = ago < 90 ? "à l'instant" : ago < 3600 ? `il y a ${Math.round(ago / 60)} min` : ago < 86_400 ? `il y a ${Math.round(ago / 3600)} h` : `il y a ${Math.round(ago / 86_400)} j`;
  const calls = agent.callsToday === 1 ? "1 appel aujourd'hui" : `${agent.callsToday} appels aujourd'hui`;
  return `Dernier appel ${when}, ${calls}`;
}

/** Les appareils qu'on peut donner à un agent : prises, lampes et volets, sans ceux d'Ostal Server
 *  (« jamais »). Chaque appareil dit s'il est « sensible » (chaque commande attend votre accord).
 *  `devices` : les valeurs de `state.devices` (`{ id, info: { name, caps } }`). */
export function deviceChoices(devices, never, sensitive) {
  const banned = new Set(never);
  const careful = new Set(sensitive);
  const out = [];
  let hidden = 0;
  for (const d of devices) {
    const caps = d?.info?.caps;
    if (!caps || !(caps.on_off || caps.cover)) continue;
    const id = String(d.id);
    if (banned.has(id)) { hidden += 1; continue; }
    out.push({ id, name: text(d.info.name) || `Appareil ${id}`, kind: caps.cover ? "volet" : "prise", sensitive: careful.has(id) });
  }
  out.sort((a, b) => a.name.localeCompare(b.name, "fr"));
  return { choices: out, hidden };
}

/** Le formulaire vide. */
export const emptyForm = (days = 90) => ({ label: "", days, occupation: false, control: false, mode: false, devices: new Set() });

/** Ce qui manque au formulaire, en une phrase ; « » quand tout va. */
export function formError(form, state) {
  const label = form.label.trim();
  if (!label) return "Donnez un nom à cet agent, par exemple « Claude Code sur le portable ».";
  if (label.length > 48) return "Le nom fait 48 caractères au plus.";
  if (form.control && !state.canControl) return "Votre compte n'a pas le droit de commander la maison.";
  if (form.mode && !state.canMode) return "Votre compte n'a pas le droit de changer le mode.";
  if (form.control && form.devices.size === 0) return "Cochez au moins un appareil que cet agent peut commander.";
  if (form.control && form.devices.size > state.maxDevices) return `${state.maxDevices} appareils au plus par agent.`;
  return "";
}

/** Le corps de `POST /api/agents` : « lecture » toujours, « mode » suppose « occupation ». */
export function createBody(form) {
  const scopes = ["read"];
  if (form.occupation || form.mode) scopes.push("occupation");
  if (form.control) scopes.push("control");
  if (form.mode) scopes.push("mode");
  return {
    label: form.label.trim(),
    days: form.days,
    scopes,
    devices: form.control ? [...form.devices].sort((a, b) => Number(a) - Number(b)) : [],
  };
}

/** Les phrases quand la création ou la révocation échoue. Le serveur écrit déjà en français les
 *  refus qui se disent (`detail`) ; une panne de réseau ou un vieux serveur ont la leur. */
export function agentError(error) {
  if (error?.status === 0) return "La maison ne répond pas : réessayez dans un instant.";
  if (error?.status === 404) return "Ce serveur ne gère pas encore les agents d'IA : mettez-le à jour.";
  if (error?.status === 403) return error.detail || "Ce compte n'a pas ce droit.";
  if (error?.status === 400 && typeof error.detail === "string" && error.detail) return error.detail.charAt(0).toUpperCase() + error.detail.slice(1) + (/[.!?]$/.test(error.detail) ? "" : ".");
  return "Impossible pour l'instant, réessayez.";
}

/** Les lignes à copier pour brancher un client. **Aucune ne contient le jeton** : il se range dans
 *  le Trousseau du Mac (la commande le demande au clavier, il ne passe ni par l'historique du shell
 *  ni par la liste des processus) et les clients le lisent dans une variable d'environnement. */
export function connectionSnippets(origin) {
  const url = `${String(origin).replace(/\/+$/, "")}/mcp`;
  return {
    keychain: 'security add-generic-password -U -s ostal-agent -a "$USER" -w',
    env: 'export OSTAL_AGENT_TOKEN="$(security find-generic-password -s ostal-agent -w)"',
    claudeCode: JSON.stringify({ mcpServers: { ostal: { type: "http", url, headers: { Authorization: "Bearer ${OSTAL_AGENT_TOKEN}" } } } }, null, 2),
    cursor: JSON.stringify({ mcpServers: { ostal: { url, headers: { Authorization: "Bearer ${env:OSTAL_AGENT_TOKEN}" } } } }, null, 2),
  };
}
