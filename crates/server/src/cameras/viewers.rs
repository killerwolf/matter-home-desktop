//! Spectateurs d'une caméra : un compteur par caméra, tenu par le hub.
//!
//! Le ticket n° 5 prend un [`ViewerLease`] (`Hub::acquire_camera_viewer`) pour chaque client qui
//! regarde et le lâche quand il part (ou se déconnecte : `Drop`). Tant qu'il y en a au moins un :
//!
//! - la surveillance saute la caméra (elle ne lui ouvre pas une session de plus) ;
//! - le bouton « Tester » est refusé pour elle ;
//! - quand le dernier part, #5 coupe la connexion RTSP du serveur : **sans spectateur, aucune
//!   connexion ouverte vers une caméra**.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct ViewerRegistry {
    counts: Mutex<HashMap<u64, usize>>,
}

impl ViewerRegistry {
    /// Nombre de spectateurs de la caméra.
    pub fn count(&self, id: u64) -> usize {
        let counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        counts.get(&id).copied().unwrap_or(0)
    }

    pub fn acquire(self: &Arc<Self>, id: u64) -> ViewerLease {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(id).or_insert(0) += 1;
        ViewerLease {
            id,
            registry: self.clone(),
        }
    }
}

/// Un spectateur. Le lâcher le retire du compte.
pub struct ViewerLease {
    id: u64,
    registry: Arc<ViewerRegistry>,
}

impl ViewerLease {
    pub fn camera_id(&self) -> u64 {
        self.id
    }
}

impl fmt::Debug for ViewerLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ViewerLease({:#x})", self.id)
    }
}

impl Drop for ViewerLease {
    fn drop(&mut self) {
        let mut counts = self
            .registry
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(n) = counts.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                counts.remove(&self.id);
            }
        }
    }
}
