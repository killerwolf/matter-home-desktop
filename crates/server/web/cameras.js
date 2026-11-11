// Caméras en direct : le serveur relaie du H.264 (AVCC) en WebSocket binaire, le navigateur le
// décode avec WebCodecs (VideoDecoder, Safari depuis iOS 17) et dessine dans un canvas.
// Messages binaires : 0x01 Config (codec u8, largeur u16, hauteur u16, rapport de pixel n u16 /
// d u16, puis l'avcC) ; 0x02 Frame (drapeaux u8, bit 0 = image clé, horodatage u32 en ms,
// données AVCC). Messages texte : l'état du flux.
import { openSocket } from "./api.js";

export const webCodecsSupported = () => typeof VideoDecoder === "function" && typeof EncodedVideoChunk === "function";

const hex2 = (n) => n.toString(16).padStart(2, "0");

/** Un lecteur : `play(canvas, onState)` ; `stop()` ferme tout (WebSocket, décodeur). */
export function createPlayer(cameraId, kind, canvas, onState) {
  let socket = null;
  let decoder = null;
  let stopped = false;
  let retry = null;
  let delay = 1000;
  let needKey = true;

  const ctx = canvas.getContext("2d");
  const closeDecoder = () => {
    try { decoder?.close(); } catch { /* déjà fermé */ }
    decoder = null;
  };

  const configure = (bytes) => {
    const view = new DataView(bytes);
    const width = view.getUint16(2);
    const height = view.getUint16(4);
    const avcc = new Uint8Array(bytes, 10);
    if (avcc.length < 4) return;
    closeDecoder();
    canvas.width = width;
    canvas.height = height;
    decoder = new VideoDecoder({
      output: (frame) => { ctx.drawImage(frame, 0, 0, canvas.width, canvas.height); frame.close(); },
      error: () => { onState({ kind: "retry", text: "Erreur de décodage, reprise…" }); socket?.close(); },
    });
    decoder.configure({
      codec: `avc1.${hex2(avcc[1])}${hex2(avcc[2])}${hex2(avcc[3])}`,
      codedWidth: width,
      codedHeight: height,
      description: avcc,
      optimizeForLatency: true,
    });
    needKey = true;
  };

  const frame = (bytes) => {
    if (!decoder || decoder.state !== "configured") return;
    const view = new DataView(bytes);
    const key = (view.getUint8(1) & 1) === 1;
    if (needKey && !key) return;
    needKey = false;
    // Un décodeur qui prend du retard : on saute jusqu'à la prochaine image clé.
    if (decoder.decodeQueueSize > 8 && !key) { needKey = true; return; }
    decoder.decode(new EncodedVideoChunk({
      type: key ? "key" : "delta",
      timestamp: view.getUint32(2) * 1000,
      data: new Uint8Array(bytes, 6),
    }));
  };

  const open = () => {
    if (stopped) return;
    onState({ kind: "connecting", text: "Connexion…" });
    socket = openSocket(`/api/cameras/${cameraId}/stream?kind=${kind}`);
    socket.onmessage = (message) => {
      if (typeof message.data === "string") {
        let status;
        try { status = JSON.parse(message.data); } catch { return; }
        const state = status.state ?? status.type;
        if (state === "live") onState({ kind: "live", text: "" });
        else if (state === "connecting") onState({ kind: "connecting", text: "Connexion…" });
        else if (state === "retrying") onState({ kind: "retry", text: "Flux interrompu, reprise…" });
        else if (state === "unsupported") onState({ kind: "error", text: `Codec ${status.codec ?? ""} non pris en charge` });
        return;
      }
      delay = 1000;
      const tag = new Uint8Array(message.data)[0];
      if (tag === 1) configure(message.data);
      else if (tag === 2) { frame(message.data); }
    };
    socket.onclose = (event) => {
      socket = null;
      closeDecoder();
      if (stopped) return;
      if (event.code === 1000) { onState({ kind: "error", text: "Codec non pris en charge" }); return; }
      onState({ kind: "retry", text: "Reconnexion…" });
      retry = setTimeout(open, delay);
      delay = Math.min(delay * 2, 15000);
    };
    socket.onerror = () => {};
  };
  open();
  return {
    stop() {
      stopped = true;
      clearTimeout(retry);
      socket?.close();
      closeDecoder();
    },
  };
}
