// Accès au serveur : même origine (pas de CORS), mêmes routes que le client de bureau.
// Le jeton (jamais le mot de passe) est gardé dans le stockage local de l'app, comme le client
// natif garde le sien dans connexion.json : il est révocable (POST /api/logout) et expire côté
// serveur. Il ne part jamais dans une URL : en-tête Authorization pour le REST, sous-protocole
// WebSocket pour le temps réel (un navigateur ne peut pas poser d'en-tête sur un WebSocket).
import { describeInstallation } from "./connections.js";

// 4 : le login dit quelle connexion se présente (« Mes connexions »). Un serveur d'avant la
// version 4 refuse alors de se connecter : l'app web et le serveur sortent ensemble.
export const PROTOCOL_VERSION = 4;
const KEY = "ostal.session";
const SUBPROTOCOL = "ostal.v1";

// Avant le renommage en Ostal, les clés du stockage local commençaient par « matter-home. » :
// la valeur est déplacée (pas copiée) sous la nouvelle clé, pour qu'une déconnexion ne laisse
// pas l'ancien jeton derrière elle.
export function adoptLegacyKey(key, legacyKey) {
  try {
    const old = localStorage.getItem(legacyKey);
    if (old === null) return;
    if (localStorage.getItem(key) === null) localStorage.setItem(key, old);
    localStorage.removeItem(legacyKey);
  } catch { /* stockage refusé : rien à reprendre */ }
}
adoptLegacyKey(KEY, "matter-home.session");

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

/** Quand le serveur ne répond pas : le titre, puis la piste (le Mac dort le plus souvent). Le même
 *  texte sert à l'erreur réseau, à l'écran de connexion et au bandeau « La maison ne répond pas ». */
export const HOUSE_SILENT = "La maison ne répond pas.";
export const HOUSE_HINT = "Le Mac est peut-être en veille : vérifiez qu'il est allumé et que Tailscale est actif.";

export class ApiError extends Error {
  constructor(status, code, detail, retryAfter, hint) {
    super(hint ? `${detail} ${hint}` : detail || code);
    this.hint = hint;
    this.status = status;
    this.code = code;
    this.detail = detail;
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
    throw new ApiError(0, "network", HOUSE_SILENT, undefined, HOUSE_HINT);
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

/**
 * Une ressource binaire (l'image fixe d'une caméra) : même en-tête `Authorization` que le reste,
 * jamais le jeton dans l'URL. Rend un `Blob`.
 */
export async function requestBlob(path) {
  const headers = {};
  if (token) headers.Authorization = `Bearer ${token}`;
  let response;
  try {
    response = await fetch(path, { method: "GET", headers, cache: "no-store", credentials: "omit" });
  } catch {
    throw new ApiError(0, "network", HOUSE_SILENT, undefined, HOUSE_HINT);
  }
  if (response.ok) return response.blob();
  const data = await response.json().catch(() => ({}));
  const error = data?.error ?? {};
  if (response.status === 401) {
    save(null);
    unauthorizedListeners.forEach((fn) => fn());
  }
  throw new ApiError(response.status, error.code ?? "unknown", error.detail, error.retry_after_s);
}

export async function login(username, password) {
  const result = await request("POST", "/api/login", {
    username, password, protocol_version: PROTOCOL_VERSION, installation: describeInstallation(),
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
 * Flux d'événements : `hello`, `snapshot` puis `event`. L'état annoncé à `onStatus` est
 * « connecting », « connected » ou « lost » (la liaison est perdue ; elle reprend toute seule). Se reconnecte tout seul (attente
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
        onStatus("connected");
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
      onStatus("lost");
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
