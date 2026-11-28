//! How a process hears it is asked to stop, by Ctrl-C or by its service
//! manager's terminate signal, and what a node's stop waits for before
//! the node ends: the lends it is answering.

use std::sync::Arc;
use std::time::Duration;

use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::lend::lender::Lender;

/// The stops a process is asked, heard one after another, so a second
/// can end what the first began.
pub struct Stops {
    /// The service manager's terminate signal, where it can be heard.
    terminate: Option<Signal>,
}

impl Stops {
    /// Hears each stop asked from now.
    #[must_use]
    pub fn heard() -> Self {
        Self {
            terminate: signal(SignalKind::terminate()).ok(),
        }
    }

    /// Resolves at the next stop asked.
    pub async fn next(&mut self) {
        let Some(terminate) = self.terminate.as_mut() else {
            // With no terminate signal to hear, Ctrl-C alone stops it.
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
}

/// Resolves when the process is asked to stop.
pub async fn stopped() {
    Stops::heard().next().await;
}

/// Resolves when the node is asked to stop and the lends its `lender`
/// is answering have ended, waiting at most `within_secs` seconds for
/// them, or until a second stop; at once where it lends nothing. Until
/// then the node runs on as ever, but for taking no new lend, so what it
/// carries ends after.
pub async fn lends_drained(lender: Option<Arc<Lender>>, within_secs: u64) {
    let mut stops = Stops::heard();
    stops.next().await;
    if let Some(lender) = lender {
        let within = Duration::from_secs(within_secs);
        lender.drain(within, stops.next()).await;
    }
}
