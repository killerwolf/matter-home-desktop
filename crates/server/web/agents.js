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
