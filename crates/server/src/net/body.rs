//! Corps de requête avec délai d'inactivité : un client qui n'envoie plus rien pendant
//! `idle` ne retient pas la connexion (et la mémoire déjà reçue) jusqu'à l'échéance de la
//! requête entière.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use tokio::time::{Instant, Sleep};

/// Aucun octet du corps reçu pendant le délai.
#[derive(Debug)]
pub struct BodyIdleTimeout;

impl fmt::Display for BodyIdleTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("corps de requête interrompu")
    }
}

impl Error for BodyIdleTimeout {}

/// Le corps d'origine, avec un minuteur remis à zéro à chaque morceau reçu.
pub struct IdleBody {
    inner: Body,
    idle: Duration,
    sleep: Pin<Box<Sleep>>,
}

impl IdleBody {
    pub fn new(inner: Body, idle: Duration) -> Self {
        Self {
            inner,
            idle,
            sleep: Box::pin(tokio::time::sleep(idle)),
        }
    }
}

impl HttpBody for IdleBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => {
                let next = Instant::now() + this.idle;
                this.sleep.as_mut().reset(next);
                Poll::Ready(frame)
            }
            Poll::Pending => match this.sleep.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Some(Err(axum::Error::new(BodyIdleTimeout)))),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// `true` si `error` (ou l'une de ses causes) est un délai d'inactivité du corps.
pub fn is_idle_timeout(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(e) = current {
        if e.is::<BodyIdleTimeout>() {
            return true;
        }
        current = e.source();
    }
    false
}
