//! [`Browser`]: one managed or attached Firefox and its single BiDi session.
//!
//! Firefox allows one BiDi session and keeps it after its WebSocket dies, so
//! a `Browser` owns exactly one session and hands out clones of it:
//! [`Browser::client`], [`Browser::browser_client`] and every tab share one
//! connection. When that connection dies, the next call opens a new session
//! if Firefox freed the old one; a managed Firefox stuck with an orphan
//! session is restarted (it is ours), while an attached one yields
//! [`BrowserError::SessionBusy`] and is never signalled.
//!
//! One `tokio` mutex guards the state. It is held while a session is
//! (re)opened or a managed Firefox relaunched, which is what guarantees a
//! single `session.new` under concurrent callers; every await inside is
//! bounded by a configured timeout. It is never held while user commands run:
//! those use the cloned clients.

mod obtain;
mod state;

pub use state::BrowserState;
use state::SessionCheck;

use std::path::PathBuf;
use std::sync::Arc;

use bidi_lite::browser::{BrowserClient, ContextInfo};
use bidi_lite::client::BidiClient;
use bidi_lite::error::BidiError;
use bidi_lite::rdp::client::RdpClient;
use bidi_lite::tab::Tab;
use tokio::sync::{Mutex, MutexGuard};
use tracing::warn;

use crate::config::BrowserConfig;
use crate::error::BrowserError;
use crate::ports::{PortAllocator, default_allocator};
use crate::probe;
use obtain::{PortChoice, attach, launch, obtain, open_session};

/// A Firefox this library launched (managed) or attached to (unmanaged).
pub struct Browser {
    config: BrowserConfig,
    allocator: Arc<PortAllocator>,
    state: Mutex<BrowserState>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Browser {
    /// Attaches to or launches a Firefox according to `config`'s
    /// [`LaunchMode`](crate::config::LaunchMode), with the process-wide port
    /// allocator. Returns once the BiDi session is open.
    pub async fn ensure(config: BrowserConfig) -> Result<Self, BrowserError> {
        Self::ensure_with_allocator(config, default_allocator()).await
    }

    /// Like [`Browser::ensure`] with an explicit allocator (a pool passes its
    /// own so its instances never collide).
    pub async fn ensure_with_allocator(
        config: BrowserConfig,
        allocator: Arc<PortAllocator>,
    ) -> Result<Self, BrowserError> {
        config.validate()?;
        let state = obtain(&config, &allocator).await?;
        Ok(Self {
            config,
            allocator,
            state: Mutex::new(state),
        })
    }

    /// Wraps an existing state, for tests: e.g. an unmanaged state pointing
    /// at a mock, whose session is opened on first use.
    #[doc(hidden)]
    pub fn test_from_state(config: BrowserConfig, state: BrowserState) -> Self {
        Self {
            config,
            allocator: default_allocator(),
            state: Mutex::new(state),
        }
    }

    /// The low-level session client (a clone sharing the one connection).
    pub async fn client(&self) -> Result<BidiClient, BrowserError> {
        Ok(self.browser_client().await?.client().clone())
    }

    /// The browser-level client over the same session as [`Browser::client`].
    pub async fn browser_client(&self) -> Result<BrowserClient, BrowserError> {
        let mut state = self.live_state().await?;
        let limit = self.config.timeouts.connect;
        match state.check_session(limit).await {
            SessionCheck::Alive(session) => Ok(session),
            SessionCheck::Unresponsive => Err(BrowserError::Bidi(BidiError::Timeout {
                method: "session.status".to_owned(),
                timeout: limit,
            })),
            SessionCheck::Dead => self.reopen_session(&mut state).await,
        }
    }

    /// The Firefox debugger (RDP) client, cached and reconnected when its
    /// socket died. Fails with [`BrowserError::RdpDisabled`] unless RDP was
    /// enabled. Attaching a `Debugger` to a tab is left to the caller:
    /// `Debugger::attach_to_context(&rdp, tab.context_id())` finds the tab
    /// showing a BiDi context without activating it.
    pub async fn rdp_client(&self) -> Result<RdpClient, BrowserError> {
        let mut state = self.live_state().await?;
        let port = state.rdp_port.ok_or(BrowserError::RdpDisabled)?;
        if let Some(client) = state.rdp.clone().filter(RdpClient::is_connected) {
            return Ok(client);
        }
        let address = probe::host_port(&self.config.host, port);
        let connect = RdpClient::connect(&address, self.config.timeouts.command);
        let client = tokio::time::timeout(self.config.timeouts.connect, connect)
            .await
            .map_err(|_| BrowserError::RemoteUnavailable {
                host: self.config.host.clone(),
                port,
            })??;
        state.rdp = Some(client.clone());
        Ok(client)
    }

    /// Opens a new tab on `url`.
    pub async fn new_tab(&self, url: &str) -> Result<Tab, BrowserError> {
        Ok(self.browser_client().await?.new_tab(url).await?)
    }

    /// A handle to the existing tab `context_id`.
    pub async fn attach_tab(&self, context_id: &str) -> Result<Tab, BrowserError> {
        Ok(self.browser_client().await?.attach(context_id).await?)
    }

    /// Handles to every open tab.
    pub async fn attach_to_all_tabs(&self) -> Result<Vec<Tab>, BrowserError> {
        Ok(self.browser_client().await?.attach_to_all_tabs().await?)
    }

    /// The open tabs.
    pub async fn list_tabs(&self) -> Result<Vec<ContextInfo>, BrowserError> {
        Ok(self.browser_client().await?.list_tabs().await?)
    }

    /// Closes the tab `context_id`.
    pub async fn close_tab(&self, context_id: &str) -> Result<(), BrowserError> {
        Ok(self.browser_client().await?.close_tab(context_id).await?)
    }

    /// Stops the browser. Managed: `browser.close`, wait, terminate if
    /// needed, remove an ephemeral profile. Attached: end the session only.
    /// Idempotent; later client calls fail with [`BrowserError::Stopped`].
    pub async fn stop(&self) -> Result<(), BrowserError> {
        self.state.lock().await.shutdown(&self.config).await;
        Ok(())
    }

    /// Stops and starts again with the same configuration: a managed
    /// Firefox is relaunched (same BiDi port when it was fixed), an attached
    /// one gets a new session.
    pub async fn restart(&self) -> Result<(), BrowserError> {
        let mut state = self.state.lock().await;
        state.shutdown(&self.config).await;
        let fresh = match state.managed {
            true => self.relaunch_state(&state).await?,
            false => attach(&self.config).await?,
        };
        *state = fresh;
        Ok(())
    }

    /// Whether this library launched the browser.
    pub async fn is_managed(&self) -> bool {
        self.state.lock().await.managed
    }

    /// Whether the browser is usable: not stopped and, if managed, its
    /// process running; if attached, its BiDi port answering.
    pub async fn is_alive(&self) -> bool {
        let mut state = self.state.lock().await;
        match (state.stopped, state.managed) {
            (true, _) => false,
            (false, true) => state.process_running(),
            (false, false) => {
                let (host, port) = (&self.config.host, state.bidi_port);
                probe::is_firefox_bidi(host, port, self.config.timeouts.connect).await
            }
        }
    }

    /// The BiDi address `(host, port)`, with the real port of a port-0 launch.
    pub async fn debug_address(&self) -> (String, u16) {
        (self.config.host.clone(), self.state.lock().await.bidi_port)
    }

    /// The RDP address, when RDP is enabled.
    pub async fn rdp_address(&self) -> Option<(String, u16)> {
        let port = self.state.lock().await.rdp_port?;
        Some((self.config.host.clone(), port))
    }

    /// The profile directory of a managed browser.
    pub async fn profile_dir(&self) -> Option<PathBuf> {
        let state = self.state.lock().await;
        state
            .profile
            .as_ref()
            .and_then(|p| p.dir())
            .map(PathBuf::from)
    }

    /// The process id of a managed browser.
    #[doc(hidden)]
    pub async fn pid(&self) -> Option<u32> {
        self.state
            .lock()
            .await
            .process
            .as_ref()
            .and_then(|p| p.id())
    }

    /// The state, checked: not stopped, and a dead managed process relaunched
    /// when `auto_relaunch` is on.
    async fn live_state(&self) -> Result<MutexGuard<'_, BrowserState>, BrowserError> {
        let mut state = self.state.lock().await;
        if state.stopped {
            return Err(BrowserError::Stopped);
        }
        if state.managed && !state.process_running() {
            if !self.config.auto_relaunch {
                return Err(BrowserError::Stopped);
            }
            warn!(port = state.bidi_port, "managed Firefox died; relaunching");
            self.relaunch(&mut state).await?;
        }
        Ok(state)
    }

    /// Opens a new session after the old connection died. A managed Firefox
    /// whose session is orphaned is restarted; an attached one is busy.
    async fn reopen_session(
        &self,
        state: &mut BrowserState,
    ) -> Result<BrowserClient, BrowserError> {
        state.session = None;
        let (host, port) = (self.config.host.as_str(), state.bidi_port);
        let free = probe::bidi_session_available(host, port, self.config.timeouts.connect).await?;
        match (free, state.managed) {
            (true, _) => {
                let session = open_session(&self.config, port).await?;
                state.session = Some(session.clone());
                Ok(session)
            }
            (false, true) => {
                warn!(
                    port,
                    "managed Firefox holds an orphaned BiDi session; restarting it"
                );
                self.relaunch(state).await?;
                state.session.clone().ok_or(BrowserError::Stopped)
            }
            (false, false) => Err(BrowserError::SessionBusy {
                host: host.to_owned(),
                port,
            }),
        }
    }

    async fn relaunch(&self, state: &mut BrowserState) -> Result<(), BrowserError> {
        state.shutdown(&self.config).await;
        *state = self.relaunch_state(state).await?;
        Ok(())
    }

    /// A fresh managed state on the same BiDi port when it was fixed (on a
    /// new Firefox-picked port otherwise).
    async fn relaunch_state(&self, old: &BrowserState) -> Result<BrowserState, BrowserError> {
        let port = match self.config.port {
            0 => 0,
            _ => old.bidi_port,
        };
        launch(&self.config, &self.allocator, PortChoice::Exact(port)).await
    }
}

#[cfg(test)]
mod tests;
