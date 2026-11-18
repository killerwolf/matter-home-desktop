// Événements de détection (ticket n° 18) : liste sous l'onglet Caméras, capture et clip.
//
// Tout passe par `fetch` avec l'en-tête `Authorization` (jamais le jeton dans l'URL) :
// - la capture est dessinée dans un canvas (`createImageBitmap`), comme les images fixes ;
// - le clip MP4 est lu par un `<video>` depuis une URL `blob:` locale (seule exception de la
//   politique de sécurité : `media-src blob:`), libérée à la fermeture.

import { ApiError, request, requestBlob } from "./api.js";

const CLASS_LABELS = { personne: "Personne", chat: "Chat", chien: "Chien", vehicule: "Véhicule" };

export const classLabel = (c) => CLASS_LABELS[c] ?? "Objet";

/** « Personne, chien » : les classes d'un événement, la plus sûre d'abord. */
export const classesText = (event) => (event.classes?.length ? event.classes.map(classLabel).join(", ") : "Objet");

/** « aujourd'hui 14:05 », « hier 09:12 », « 3 nov. 22:40 ». */
export function whenText(at, now = Date.now()) {
  const d = new Date(at * 1000);
  const hm = d.toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
  const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(new Date(now)) - day(d)) / 86_400_000);
  if (diff === 0) return `aujourd'hui ${hm}`;
  if (diff === 1) return `hier ${hm}`;
  return `${d.toLocaleDateString("fr-FR", { day: "numeric", month: "short" })} ${hm}`;
}

/** Les derniers événements, et l'état de la détection. `null` : serveur sans détection. */
export async function loadEvents(limit = 60) {
  try {
    return await request("GET", `/api/events?limit=${limit}`);
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null; // serveur plus ancien
    throw error;
  }
}

export const deleteEvent = (id) => request("DELETE", `/api/events/${encodeURIComponent(id)}`);

/** Dessine la capture d'un événement dans `canvas`. */
export async function drawSnapshot(id, canvas) {
  const blob = await requestBlob(`/api/events/${encodeURIComponent(id)}/snapshot`);
  const bitmap = await createImageBitmap(blob);
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  canvas.getContext("2d").drawImage(bitmap, 0, 0);
  bitmap.close?.();
}

/** Charge le clip et le donne au `<video>`. Rend la fonction qui libère l'URL. */
export async function playClip(id, video) {
  const blob = await requestBlob(`/api/events/${encodeURIComponent(id)}/clip`);
  const url = URL.createObjectURL(blob.type ? blob : new Blob([blob], { type: "video/mp4" }));
  video.src = url;
  return () => { video.removeAttribute("src"); video.load(); URL.revokeObjectURL(url); };
}
