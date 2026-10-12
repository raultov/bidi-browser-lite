//! The state behind a [`Browser`](super::Browser): process, profile, ports,
//! the single BiDi session and the RDP client, plus the stop path and the
//! drop safety net.

use std::time::Duration;

use bidi_lite::browser::BrowserClient;
use bidi_lite::error::BidiError;
use bidi_lite::rdp::client::RdpClient;
use tracing::warn;

use crate::config::BrowserConfig;
use crate::process::FirefoxProcess;
use crate::profile::Profile;

/// How long `stop` waits for Firefox to exit after `browser.close` (it took
/// about 0.5 s in the spike).
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// SIGTERM grace before the kill when Firefox did not exit by itself.
const TERM_GRACE: Duration = Duration::from_secs(2);

/// What [`BrowserState::check_session`] found.
pub(crate) enum SessionCheck {
    /// The session answers.
    Alive(BrowserClient),
    /// No session, or its connection is gone.
    Dead,
    /// Connected, but `session.status` did not answer in time.
    Unresponsive,
}

/// Everything one browser owns. `Drop` lives here, not on `Browser`, so the
/// safety net never needs the `Browser`'s mutex.
#[doc(hidden)]
pub struct BrowserState {
    // Declared before `profile` so the process dies before its profile is
    // removed.
    pub(crate) process: Option<FirefoxProcess>,
    pub(crate) profile: Option<Profile>,
    pub(crate) bidi_port: u16,
    pub(crate) rdp_port: Option<u16>,
    pub(crate) managed: bool,
    pub(crate) stopped: bool,
    pub(crate) keep_alive_on_drop: bool,
    pub(crate) session: Option<BrowserClient>,
    pub(crate) rdp: Option<RdpClient>,
}

impl std::fmt::Debug for BrowserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserState")
            .field("pid", &self.process.as_ref().and_then(FirefoxProcess::id))
            .field("bidi_port", &self.bidi_port)
            .field("rdp_port", &self.rdp_port)
            .field("managed", &self.managed)
            .field("stopped", &self.stopped)
            .field("session", &self.session.is_some())
            .finish_non_exhaustive()
    }
}

impl BrowserState {
    /// An unmanaged state for `Browser::test_from_state`: nothing to kill, no
    /// session yet (it is opened on first use).
    #[doc(hidden)]
    pub fn unmanaged(bidi_port: u16, rdp_port: Option<u16>) -> Self {
        Self {
            process: None,
            profile: None,
            bidi_port,
            rdp_port,
            managed: false,
            stopped: false,
            keep_alive_on_drop: false,
            session: None,
            rdp: None,
        }
    }

    /// An attached browser and its session.
    pub(crate) fn attached(config: &BrowserConfig, session: BrowserClient) -> Self {
        let mut state = Self::unmanaged(config.port, config.rdp);
        state.session = Some(session);
        state
    }

    /// A launched browser, before its session is opened. It is killed on
    /// drop until [`BrowserState::set_session`] applies `keep_alive_on_drop`,
    /// so a launch cancelled half-way never leaves Firefox running.
    pub(crate) fn managed(
        process: FirefoxProcess,
        profile: Profile,
        (bidi_port, rdp_port): (u16, Option<u16>),
    ) -> Self {
        let mut state = Self::unmanaged(bidi_port, rdp_port);
        state.process = Some(process);
        state.profile = Some(profile);
        state.managed = true;
        state
    }

    /// Stores the session and arms `keep_alive_on_drop`.
    pub(crate) fn set_session(&mut self, session: BrowserClient, keep_alive_on_drop: bool) {
        self.session = Some(session);
        self.keep_alive_on_drop = keep_alive_on_drop;
    }

    /// Whether the session still works. `is_connected` alone is not enough:
    /// a connection whose tasks died without a close frame (a dropped
    /// runtime) still reports connected, so a side-effect free
    /// `session.status` is sent, bounded by `limit`. Only a disconnect makes
    /// a session dead; a slow reply does not (Firefox may just be busy, and
    /// dropping our own live session would orphan it).
    pub(crate) async fn check_session(&self, limit: Duration) -> SessionCheck {
        let Some(session) = self.session.clone().filter(|s| s.client().is_connected()) else {
            return SessionCheck::Dead;
        };
        match tokio::time::timeout(limit, session.client().status()).await {
            Ok(Err(BidiError::Disconnected)) => SessionCheck::Dead,
            Ok(_) => SessionCheck::Alive(session),
            Err(_) => SessionCheck::Unresponsive,
        }
    }

    /// Whether a managed process is still running.
    pub(crate) fn process_running(&mut self) -> bool {
        self.process
            .as_mut()
            .is_some_and(FirefoxProcess::is_running)
    }

    /// Stops everything this state owns. Managed: `browser.close` on our
    /// session (it ends the session; Firefox refuses `browser.close` without
    /// one), wait for the exit, terminate if needed, remove an ephemeral
    /// profile. Attached: `session.end` only; the process is never signalled.
    /// Idempotent.
    pub(crate) async fn shutdown(&mut self, config: &BrowserConfig) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.rdp = None;
        let session = self.session.take().filter(|s| s.client().is_connected());
        let limit = config.timeouts.command;
        if !self.managed {
            if let Some(session) = session {
                let _ = tokio::time::timeout(limit, session.end()).await;
            }
            return;
        }
        let closed = match session {
            Some(session) => matches!(
                tokio::time::timeout(limit, session.close_browser()).await,
                Ok(Ok(()))
            ),
            None => false,
        };
        if let Some(process) = self.process.as_mut()
            && !(closed && process.wait_exit(CLOSE_WAIT).await)
        {
            process.terminate(TERM_GRACE).await;
        }
        self.process = None;
        if let Some(profile) = self.profile.as_mut() {
            profile.cleanup();
        }
        self.profile = None;
    }
}

impl Drop for BrowserState {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if self.managed && !self.keep_alive_on_drop {
            // The guards do it: the process (declared first) is killed and
            // its exit awaited, which frees its session, then an ephemeral
            // profile is removed.
            return;
        }
        if let Some(process) = self.process.as_mut() {
            process.keep_alive();
        }
        if let Some(profile) = self.profile.as_mut() {
            profile.keep_on_drop();
        }
        if let Some(session) = self.session.take() {
            warn!(
                port = self.bidi_port,
                "Browser dropped without stop(); ending its BiDi session in the background"
            );
            end_in_background(session);
        }
    }
}

/// Best-effort `session.end` from a synchronous context: a Firefox that keeps
/// running would otherwise keep our session as an orphan forever.
fn end_in_background(session: BrowserClient) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = session.end().await;
        });
    }
}
