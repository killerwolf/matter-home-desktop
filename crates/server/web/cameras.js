// Caméras en direct : le serveur relaie du H.264 (AVCC) en WebSocket binaire, le navigateur le
// décode avec WebCodecs (VideoDecoder, Safari depuis iOS 17) et dessine dans un canvas.
// Messages binaires : 0x01 Config (codec u8, largeur u16, hauteur u16, rapport de pixel n u16 /
// d u16, puis l'avcC) ; 0x02 Frame (drapeaux u8, bit 0 = image clé, horodatage u32 en ms,
// données AVCC). Messages texte : l'état du flux (`unsupported` : codec que le serveur ne relaie
// pas, définitif).
import { ApiError, checkSession, hasToken, openSocket, requestBlob } from "./api.js";

export const webCodecsSupported = () => typeof VideoDecoder === "function" && typeof EncodedVideoChunk === "function";

const hex2 = (n) => n.toString(16).padStart(2, "0");

/** Un lecteur : `stop()` ferme tout (WebSocket, décodeur). Rien ne démarre sans jeton. */
export function createPlayer(cameraId, kind, canvas, onState) {
  let socket = null;
  let decoder = null;
  let stopped = false;
  let retry = null;
  let delay = 1000;
  let needKey = true;
  let fatal = false; // codec non pris en charge, ou décodage impossible : inutile de réessayer
  let configToken = 0;

  const ctx = canvas.getContext("2d");
  const closeDecoder = () => {
    configToken++;
    try { decoder?.close(); } catch { /* déjà fermé */ }
    decoder = null;
  };
  const fail = (text) => {
    fatal = true;
    onState({ kind: "error", text });
    closeDecoder();
    try { socket?.close(); } catch { /* déjà fermé */ }
  };

  const configure = async (bytes) => {
    const view = new DataView(bytes);
    const width = view.getUint16(2);
    const height = view.getUint16(4);
    const parN = view.getUint16(6) || 1;
    const parD = view.getUint16(8) || 1;
    const avcc = new Uint8Array(bytes.slice(10));
    if (avcc.length < 4) return;
    closeDecoder();
    const mine = configToken;
    // Rapport d'aspect des pixels : la largeur affichée est largeur × n/d (l'image est
    // étirée en conséquence en la dessinant).
    canvas.width = Math.max(1, Math.round((width * parN) / parD));
    canvas.height = height;
    const config = {
      codec: `avc1.${hex2(avcc[1])}${hex2(avcc[2])}${hex2(avcc[3])}`,
      codedWidth: width,
      codedHeight: height,
      description: avcc,
      optimizeForLatency: true,
    };
    try {
      const support = await VideoDecoder.isConfigSupported(config);
      if (mine !== configToken || stopped) return;
      if (!support.supported) { fail("Ce navigateur ne décode pas ce flux vidéo."); return; }
      decoder = new VideoDecoder({
        output: (frame) => { ctx.drawImage(frame, 0, 0, canvas.width, canvas.height); frame.close(); },
        error: () => { fail("Erreur de décodage de la vidéo."); },
      });
      decoder.configure(config);
      needKey = true;
    } catch {
      fail("Ce navigateur ne décode pas ce flux vidéo.");
    }
  };

  const frame = (bytes) => {
    if (!decoder || decoder.state !== "configured") return;
    const view = new DataView(bytes);
    const key = (view.getUint8(1) & 1) === 1;
    if (needKey && !key) return;
    needKey = false;
    // Un décodeur qui prend du retard : on saute jusqu'à la prochaine image clé.
    if (decoder.decodeQueueSize > 8 && !key) { needKey = true; return; }
    try {
      decoder.decode(new EncodedVideoChunk({
        type: key ? "key" : "delta",
        timestamp: view.getUint32(2) * 1000,
        data: new Uint8Array(bytes, 6),
      }));
    } catch {
      fail("Erreur de décodage de la vidéo.");
    }
  };

  const open = () => {
    if (stopped || fatal || !hasToken()) return;
    onState({ kind: "connecting", text: "Connexion…" });
    let received = false;
    const current = openSocket(`/api/cameras/${cameraId}/stream?kind=${kind}`);
    socket = current;
    current.onmessage = (message) => {
      received = true;
      if (typeof message.data === "string") {
        let status;
        try { status = JSON.parse(message.data); } catch { return; }
        const state = status.state;
        if (state === "live") onState({ kind: "live", text: "" });
        else if (state === "connecting") onState({ kind: "connecting", text: "Connexion…" });
        else if (state === "retrying") onState({ kind: "retry", text: "Flux interrompu, reprise…" });
        else if (state === "unsupported") fail(`Codec ${String(status.codec ?? "").toUpperCase()} non pris en charge`);
        return;
      }
      delay = 1000;
      const tag = new Uint8Array(message.data)[0];
      if (tag === 1) configure(message.data);
      else if (tag === 2) frame(message.data);
    };
    current.onclose = async () => {
      if (socket === current) socket = null;
      closeDecoder();
      if (stopped || fatal) return;
      onState({ kind: "retry", text: "Reconnexion…" });
      // Refusé avant le moindre message : jeton expiré ou révoqué ? On le vérifie, et sans jeton
      // on s'arrête (l'app revient à l'écran de connexion).
      if (!received && !(await checkSession())) { onState({ kind: "error", text: "Session terminée" }); return; }
      if (stopped || fatal) return;
      retry = setTimeout(open, delay);
      delay = Math.min(delay * 2, 15000);
    };
    current.onerror = () => {};
  };
  open();
  return {
    stop() {
      stopped = true;
      clearTimeout(retry);
      try { socket?.close(); } catch { /* déjà fermé */ }
      closeDecoder();
    },
  };
}

const SNAPSHOT_EVERY = 20000; // une image fixe toutes les 20 s (le serveur la partage et la garde 15 s)
const SNAPSHOT_RETRY = 30000; // après un échec passager

/**
 * Image fixe d'une caméra (vignette sans flux lu : pas de flux réduit, ou au-delà du plafond) :
 * `GET /api/cameras/{id}/snapshot` par `fetch` avec l'en-tête `Authorization`, puis blob, puis
 * `createImageBitmap` et dessin dans le canvas. Aucune URL `blob:` n'est jamais chargée par la page :
 * la politique de sécurité (`img-src 'self'`) n'a pas à changer. `stop()` arrête tout.
 */
export function createSnapshot(cameraId, canvas, onState) {
  let stopped = false;
  let timer = null;
  const ctx = canvas.getContext("2d");
  const next = (ms) => { if (!stopped) timer = setTimeout(load, ms); };
  const load = async () => {
    if (stopped || !hasToken()) return;
    if (document.hidden) { next(2000); return; } // onglet caché : on ne dérange pas la caméra
    try {
      const blob = await requestBlob(`/api/cameras/${cameraId}/snapshot`);
      const bitmap = await createImageBitmap(blob);
      if (stopped) { bitmap.close?.(); return; }
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close?.();
      onState({ kind: "live", text: "" });
      next(SNAPSHOT_EVERY);
    } catch (error) {
      if (stopped) return;
      if (error instanceof ApiError && error.status === 404 && !error.detail) {
        onState({ kind: "error", text: "Serveur trop ancien pour les images fixes" }); // définitif
        return;
      }
      if (error instanceof ApiError && error.status === 401) return; // session finie : l'app revient à la connexion
      if (error instanceof ApiError && error.status === 404) { onState({ kind: "error", text: "Caméra introuvable" }); return; } // supprimée entre-temps
      onState({ kind: "retry", text: error instanceof ApiError && error.detail ? error.detail : "Pas d'image pour l'instant" });
      next(SNAPSHOT_RETRY);
    }
  };
  onState({ kind: "connecting", text: "Image en cours de chargement…" });
  load();
  return { stop() { stopped = true; clearTimeout(timer); } };
}
