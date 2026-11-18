//! Le côté hub des caméras : ajout, modification, suppression, état, spectateurs, tests.
//!
//! Seule la fenêtre du serveur les modifie (aucune route d'écriture), et le serveur est le seul à
//! connaître leurs adresses et mots de passe. Voir `cameras/mod.rs`.
//!
//! # Une transaction par modification
//!
//! Chaque modification prend `write_lock` **une seule fois**, change la mémoire, enregistre
//! `cameras.json`, diffuse, et si le plan est concerné l'enregistre et le diffuse dans la même
//! prise (`Hub::delete_camera`). Ne jamais appeler d'ici une méthode qui reprend `write_lock`
//! (`set_plan`, `replace_document`, `update_document`) : `std::sync::Mutex` n'est pas réentrant, ce
//! serait un blocage. `persist_and_publish` convient : il suppose le verrou déjà pris.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use protocol::wire::{
    CameraBehavior, CameraStatus, CameraView, DetectionEventView, DetectionSettings, StreamInfo,
    WireEvent,
};

use crate::cameras::{
    self, CameraAdmin, CameraBook, CameraDraft, CameraError, CameraTest, ProbeError, StreamKind,
    StreamTest, Target, ViewerLease, Wake,
};
use crate::persistence;

use super::{Document, Hub, HubState};

impl Hub {
    /// Les caméras vues par les clients : sans adresse ni mot de passe.
    pub fn cameras(&self) -> Vec<CameraView> {
        let st = self.lock();
        st.cameras.views_in(Some(st.mode))
    }

    /// Les caméras pour la fenêtre du serveur (adresses et identifiant, jamais le mot de passe).
    pub fn camera_admin_list(&self) -> Vec<CameraAdmin> {
        let st = self.lock();
        st.cameras.admin_list_in(Some(st.mode))
    }

    pub fn camera_ids(&self) -> Vec<u64> {
        self.lock().cameras.ids()
    }

    /// Les identifiants d'appareils (nœuds et appareils pontés) que la nouvelle caméra ne doit pas
    /// reprendre : ceux que le hub connaît, et ceux de `controller.json` (au démarrage, avant que
    /// les appareils soient annoncés). Des appareils pontés tirés avant les caméras peuvent
    /// se trouver dans la plage des caméras.
    fn device_ids_taken(&self, st: &HubState) -> HashSet<u64> {
        let mut taken: HashSet<u64> = st.devices.keys().copied().collect();
        if !self.demo {
            if let Ok(Some(state)) = persistence::load(&self.data_dir.join(persistence::FILE_NAME))
            {
                taken.extend(state.device_ids());
            }
        }
        taken
    }

    pub fn add_camera(&self, draft: &CameraDraft) -> Result<u64, CameraError> {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        let backup = st.cameras.clone();
        let taken = self.device_ids_taken(&st);
        let id = st.cameras.add_avoiding(draft, &taken)?;
        self.commit_cameras(&mut st, backup)?;
        drop(st);
        self.wake_cameras(Wake::Camera(id));
        self.detection().wake();
        log::info!("caméra ajoutée : {id:#x}");
        Ok(id)
    }

    pub fn update_camera(&self, id: u64, draft: &CameraDraft) -> Result<(), CameraError> {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        let backup = st.cameras.clone();
        st.cameras.update(id, draft)?;
        self.commit_cameras(&mut st, backup)?;
        drop(st);
        self.wake_cameras(Wake::Camera(id));
        self.detection().wake();
        log::info!("caméra modifiée : {id:#x}");
        Ok(())
    }

    /// Supprime une caméra **et la retire du plan**, en une seule transaction (un seul passage
    /// par `write_lock`) : le plan ne garde jamais l'objet d'une caméra qui n'existe plus, et les
    /// clients reçoivent `cameras_changed` puis, si le plan a changé, `plan_changed`.
    ///
    /// Ticket #6 : l'emplacement du plan est `st.plan.remove_camera(id)` ci-dessous. Y ajouter ce
    /// qui doit suivre une suppression (autres références à la caméra) **sans** reprendre
    /// `write_lock`.
    pub fn delete_camera(&self, id: u64) -> Result<(), CameraError> {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        let backup = st.cameras.clone();
        st.cameras.remove(id)?;
        self.commit_cameras(&mut st, backup)?;
        // --- Emplacement du plan (#6) ---
        if st.plan.remove_camera(id) {
            // Diffusé même si l'écriture échoue : la mémoire fait foi, comme pour tout le plan.
            let _ = self.persist_and_publish(&mut st, Document::Plan);
        }
        drop(st);
        self.detection().wake();
        log::info!("caméra supprimée : {id:#x}");
        Ok(())
    }

    /// Enregistre `cameras.json` et diffuse la liste. Si l'enregistrement échoue, la modification
    /// est annulée (`backup`) : la fenêtre du serveur ne doit pas montrer une caméra qui
    /// n'existera plus au prochain lancement. Appelé avec `write_lock` pris.
    fn commit_cameras(&self, st: &mut HubState, backup: CameraBook) -> Result<(), CameraError> {
        if !self.demo {
            if let Err(e) = cameras::save(&self.data_dir, st.cameras.cameras()) {
                log::error!("enregistrement des caméras : {e:#}");
                st.cameras = backup;
                return Err(CameraError(
                    "Les caméras n'ont pas pu être enregistrées (droits ou espace disque) : rien n'a changé."
                        .into(),
                ));
            }
        }
        self.publish_cameras(st);
        Ok(())
    }

    pub(super) fn publish_cameras(&self, st: &mut HubState) {
        let cameras = st.cameras.views_in(Some(st.mode));
        self.publish(st, WireEvent::CamerasChanged { cameras });
    }

    /// Adresse et identifiants d'un flux, pour le serveur seulement (lecture, #5). Ne sort
    /// jamais vers un client.
    ///
    /// `None` aussi quand la caméra est **éteinte** dans le mode actuel (`camera_is_off`) : c'est
    /// ce qui ferme, en moins de `target_check` (2 s), une lecture ouverte quand le mode change
    /// (la session voit sa cible disparaître, comme pour une caméra supprimée).
    pub fn camera_target(&self, id: u64, kind: StreamKind) -> Option<Target> {
        let st = self.lock();
        let camera = st.cameras.get(id)?;
        if camera.behavior(st.mode) == CameraBehavior::Eteinte {
            return None;
        }
        camera.target(kind)
    }

    /// La caméra est-elle éteinte dans le mode actuel (`false` pour une caméra inconnue) ? Le
    /// serveur refuse alors le direct et les images fixes (`Unavailable`, avec le mode en clair).
    ///
    /// `Surveillance` se regarde comme `Diffusion` ; en plus, la détection (ticket n° 18) analyse
    /// la caméra (voir [`Hub::cameras_to_watch`]).
    pub fn camera_is_off(&self, id: u64) -> bool {
        let st = self.lock();
        st.cameras.behavior(id, st.mode) == Some(CameraBehavior::Eteinte)
    }

    /// Les caméras à analyser maintenant (ticket n° 18) : en **Surveillance** dans le mode actuel,
    /// avec leur nom et le flux surveillé (le réduit s'il existe).
    pub fn cameras_to_watch(&self) -> Vec<(u64, String, StreamKind)> {
        let st = self.lock();
        st.cameras
            .cameras()
            .iter()
            .filter(|c| c.behavior(st.mode) == CameraBehavior::Surveillance)
            // Aucune classe suivie : rien à détecter, pas de connexion pour rien.
            .filter(|c| !c.detection.classes.is_empty())
            .map(|c| (c.id, c.name.clone(), c.preferred_kind()))
            .collect()
    }

    /// Les réglages de détection d'une caméra et son nom (`None` : caméra inconnue).
    pub fn camera_detection(&self, id: u64) -> Option<(String, DetectionSettings)> {
        let st = self.lock();
        st.cameras
            .get(id)
            .map(|c| (c.name.clone(), c.detection.clone()))
    }

    /// Un passage détecté (déjà enregistré) : diffusé aux clients (`WireEvent::Detection`, en cours
    /// à l'ouverture puis complet à la fin, même `id`). `fire_rules` : transmis au moteur
    /// d'automatisations (à l'ouverture seulement : une règle part une fois par passage).
    pub fn publish_detection(&self, event: &DetectionEventView, fire_rules: bool) {
        {
            let mut st = self.lock();
            self.publish(
                &mut st,
                WireEvent::Detection {
                    event: event.clone(),
                },
            );
        }
        if fire_rules {
            let _ = self.send(protocol::events::Command::Detection(Box::new(
                event.clone(),
            )));
        }
    }

    /// La phrase du refus d'une caméra éteinte, avec le mode en cours.
    pub fn camera_off_message(&self) -> String {
        format!("caméra éteinte en mode {}", self.mode().label())
    }

    // -----------------------------------------------------------------------
    // Spectateurs et verrous de connexion (pour #5 et la surveillance)
    // -----------------------------------------------------------------------

    /// Mode démo : pas de vraie caméra, les vignettes sont synthétiques.
    pub fn is_demo(&self) -> bool {
        self.demo
    }

    /// Nombre de spectateurs de la caméra (0 tant que #5 n'est pas là).
    pub fn camera_viewers(&self, id: u64) -> usize {
        self.viewers.count(id)
    }

    /// Un spectateur de plus pour cette caméra (`None` si elle n'existe pas). Le lâcher le retire.
    /// Tant qu'il y en a un, la surveillance saute la caméra et « Tester » lui est refusé.
    /// `None` aussi pour une caméra éteinte dans le mode actuel (garde-fou : les appelants testent
    /// `camera_is_off` d'abord, pour donner le bon refus).
    pub fn acquire_camera_viewer(self: &Arc<Self>, id: u64) -> Option<ViewerLease> {
        {
            let st = self.lock();
            if st.cameras.behavior(id, st.mode)? == CameraBehavior::Eteinte {
                return None;
            }
        }
        Some(self.viewers.acquire(id))
    }

    /// Le verrou qui sérialise les connexions de **vérification** à une caméra : la surveillance
    /// l'essaie (et saute la caméra s'il est pris), le test le prend. Une entrée par caméra.
    pub fn camera_probe_lock(&self, id: u64) -> Arc<Mutex<()>> {
        let mut locks = self.camera_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.entry(id).or_default().clone()
    }

    // -----------------------------------------------------------------------
    // Surveillance
    // -----------------------------------------------------------------------

    /// Le flux à vérifier d'une caméra : le réduit s'il existe, sinon le principal. `None` aussi
    /// pour une caméra **éteinte** dans le mode actuel : éteinte veut dire aucune connexion, pas
    /// même la vérification d'une minute (les clients voient alors l'état `Off`).
    pub(crate) fn camera_probe_target(&self, id: u64) -> Option<(StreamKind, Target)> {
        let st = self.lock();
        let camera = st.cameras.get(id)?;
        if camera.behavior(st.mode) == CameraBehavior::Eteinte {
            return None;
        }
        let kind = camera.preferred_kind();
        Some((kind, camera.target(kind)?))
    }

    /// Enregistre le résultat d'une vérification, et le diffuse s'il change quelque chose. Un
    /// résultat périmé (caméra supprimée ou modifiée pendant la vérification) est écarté. `true` :
    /// il faut revérifier tout de suite (premier échec d'une caméra joignable, voir
    /// `CameraBook::record_probe`).
    pub(crate) fn record_camera_probe(
        &self,
        id: u64,
        kind: StreamKind,
        probed: &Target,
        outcome: Result<StreamInfo, ProbeError>,
    ) -> bool {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        let Some(camera) = st.cameras.get(id) else {
            return false;
        };
        if camera.target(kind).as_ref() != Some(probed) {
            return false;
        }
        let name = camera.name.clone();
        let recorded = st
            .cameras
            .record_probe(id, outcome.map_err(|e| (e.problem, e.message)));
        if recorded.changed {
            match st.cameras.status(id) {
                Some(CameraStatus::Online) => log::info!("caméra « {name} » joignable"),
                Some(CameraStatus::Unreachable { detail, .. }) => {
                    log::warn!(
                        "caméra « {name} » injoignable : {}",
                        detail.as_deref().unwrap_or("")
                    )
                }
                _ => {}
            }
            self.publish_cameras(&mut st);
        }
        recorded.recheck
    }

    pub(crate) fn set_camera_wake(&self, wake: Option<mpsc::UnboundedSender<Wake>>) {
        *self.camera_wake.lock().unwrap_or_else(|e| e.into_inner()) = wake;
    }

    fn wake_cameras(&self, wake: Wake) {
        if let Some(tx) = &*self.camera_wake.lock().unwrap_or_else(|e| e.into_inner()) {
            let _ = tx.send(wake);
        }
    }

    // -----------------------------------------------------------------------
    // Le bouton « Tester »
    // -----------------------------------------------------------------------

    /// Lance le test d'une saisie du formulaire (flux principal puis flux réduit), sans rien
    /// enregistrer, dans un fil annulable (`CameraTest::cancel`). `editing` : la caméra modifiée,
    /// dont le mot de passe enregistré sert si le champ est laissé vide.
    ///
    /// Refusé tant que la caméra modifiée est regardée (`camera_viewers`) : une session de plus
    /// ferait échouer la lecture. Pour une caméra enregistrée, le test prend le verrou de la
    /// caméra, comme la surveillance. En démo, aucun trafic : la réponse est fabriquée.
    pub fn start_camera_test(
        &self,
        draft: &CameraDraft,
        editing: Option<u64>,
    ) -> Result<CameraTest, CameraError> {
        let camera = {
            let st = self.lock();
            let existing = editing.and_then(|id| st.cameras.get(id));
            draft.build(editing.unwrap_or(protocol::wire::CAMERA_ID_MIN), existing)?
        };
        if let Some(id) = editing {
            if self.camera_viewers(id) > 0 {
                return Err(CameraError(
                    "Cette caméra est en cours de lecture (direct ou détection) : fermez le direct, \
                     ou testez-la dans un mode où elle n'est pas en Surveillance."
                        .into(),
                ));
            }
        }
        if self.demo {
            let results: Vec<StreamTest> = [StreamKind::Main, StreamKind::Sub]
                .into_iter()
                .filter(|kind| camera.target(*kind).is_some())
                .map(|kind| StreamTest {
                    kind,
                    outcome: Ok(StreamInfo {
                        codec: "h264".into(),
                        width: Some(if kind == StreamKind::Main { 2304 } else { 1280 }),
                        height: Some(if kind == StreamKind::Main { 1296 } else { 720 }),
                    }),
                })
                .collect();
            return Ok(CameraTest::ready(Ok(results)));
        }
        let lock = editing.map(|id| self.camera_probe_lock(id));
        Ok(CameraTest::spawn(camera, lock))
    }
}

#[cfg(test)]
mod tests;
