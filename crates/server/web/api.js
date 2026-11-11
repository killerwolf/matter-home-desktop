// Accès au serveur : même origine (pas de CORS), mêmes routes que le client de bureau.
// Le jeton (jamais le mot de passe) est gardé dans le stockage local de l'app, comme le client
// natif garde le sien dans connexion.json : il est révocable (POST /api/logout) et expire côté
// serveur. Il ne part jamais dans une URL : en-tête Authorization pour le REST, sous-protocole
// WebSocket pour le temps réel (un navigateur ne peut pas poser d'en-tête sur un WebSocket).
export const PROTOCOL_VERSION = 2;
const KEY = "matter-home.session";
const SUBPROTOCOL = "matter-home.v1";

let token = null;
try { token = JSON.parse(localStorage.getItem(KEY) || "null")?.token ?? null; } catch { token = null; }

export const hasToken = () => token !== null;

function save(value) {
  token = value;
  try {
    if (value) localStorage.setItem(KEY, JSON.stringify({ token: value }));
    else localStorage.removeItem(KEY);
  } catch { /* stockage refusé : la session ne survivra pas à la fermeture */ }
}

export class ApiError extends Error {
  constructor(status, code, detail, retryAfter) {
    super(detail || code);
    this.status = status;
    this.code = code;
    this.retryAfter = retryAfter;
  }
}

const unauthorizedListeners = new Set();
export const onUnauthorized = (fn) => unauthorizedListeners.add(fn);

export async function request(method, path, body) {
  const headers = {};
  if (token) headers.Authorization = `Bearer ${token}`;
  let payload;
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    payload = JSON.stringify(body);
  }
  let response;
  try {
    response = await fetch(path, { method, headers, body: payload, cache: "no-store", credentials: "omit" });
  } catch {
    throw new ApiError(0, "network", "Serveur injoignable.");
  }
  if (response.ok) {
    return response.status === 204 ? null : response.json().catch(() => null);
  }
  const data = await response.json().catch(() => ({}));
  const error = data?.error ?? {};
  if (response.status === 401 && path !== "/api/login") {
    save(null);
    unauthorizedListeners.forEach((fn) => fn());
  }
  throw new ApiError(response.status, error.code ?? "unknown", error.detail, error.retry_after_s);
}

export async function login(username, password) {
  const result = await request("POST", "/api/login", {
    username, password, protocol_version: PROTOCOL_VERSION,
  });
  save(result.token);
}

export async function logout() {
  try { await request("POST", "/api/logout"); } catch { /* déjà révoqué ou injoignable */ }
  save(null);
}

/**
 * Un WebSocket qui se ferme avant d'avoir rien reçu peut avoir été refusé (jeton expiré ou
 * révoqué : le navigateur ne donne pas le statut HTTP). Une petite route qui exige le jeton le
 * dit : un 401 efface le jeton et renvoie à l'écran de connexion (`onUnauthorized`).
 */
export async function checkSession() {
  try { await request("GET", "/api/cameras"); } catch { /* réseau : on réessaiera */ }
  return hasToken();
}

const wsUrl = (path) => `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}${path}`;

/** WebSocket authentifié par sous-protocole (jamais d'URL avec le jeton). */
export function openSocket(path) {
  if (!token) throw new ApiError(401, "unauthorized", "Pas de session.");
  const socket = new WebSocket(wsUrl(path), [SUBPROTOCOL, `bearer.${token}`]);
  socket.binaryType = "arraybuffer";
  return socket;
}

/**
 * Flux d'événements : `hello`, `snapshot` puis `event`. Se reconnecte tout seul (attente
 * croissante) ; un trou de `seq` rouvre la connexion, comme le client de bureau.
 */
export function connectEvents({ onSnapshot, onEvent, onStatus }) {
  let socket = null;
  let seq = 0;
  let delay = 1000;
  let closed = false;
  let timer = null;

  const open = () => {
    if (closed || !token) return;
    onStatus("connecting");
    let gotSnapshot = false;
    const current = openSocket(`/api/ws?protocol=${PROTOCOL_VERSION}`);
    socket = current;
    current.onmessage = (message) => {
      if (typeof message.data !== "string") return;
      let data;
      try { data = JSON.parse(message.data); } catch { return; }
      if (data.type === "snapshot") {
        gotSnapshot = true;
        seq = data.seq;
        delay = 1000;
        onStatus("online");
        onSnapshot(data);
      } else if (data.type === "event") {
        if (data.seq !== seq + 1) {
          if (data.seq > seq + 1) { current.close(); return; } // trou : on repart d'un état complet
          return; // déjà vu
        }
        seq = data.seq;
        onEvent(data.event);
      }
    };
    current.onclose = async () => {
      if (socket === current) socket = null;
      if (closed || current._replaced) return;
      onStatus("offline");
      // Fermé avant le premier état : jeton refusé ? Un 401 renvoie à la connexion ; sans
      // jeton on ne reprend pas en boucle.
      if (!gotSnapshot && !(await checkSession())) return;
      if (closed || socket) return;
      timer = setTimeout(open, delay);
      delay = Math.min(delay * 2, 15000);
    };
    current.onerror = () => {};
  };
  open();
  return {
    /** Réveil de l'appareil, retour du réseau : on repart d'une connexion neuve. */
    restart() {
      clearTimeout(timer);
      delay = 1000;
      if (socket) { socket._replaced = true; socket.close(); socket = null; }
      open();
    },
    reconnectNow() {
      if (socket && socket.readyState <= 1) return;
      clearTimeout(timer);
      delay = 1000;
      open();
    },
    close() {
      closed = true;
      clearTimeout(timer);
      socket?.close();
    },
  };
}
