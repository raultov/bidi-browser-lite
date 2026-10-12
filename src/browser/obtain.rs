//! How a [`Browser`](super::Browser) gets its Firefox: the pure launch
//! decision, attaching, launching, and opening the single BiDi session.

use std::path::PathBuf;
use std::sync::Arc;

use bidi_lite::browser::BrowserClient;
use bidi_lite::client::{BidiClient, default_capabilities, session_url};
use bidi_lite::error::BidiError;
use tracing::{debug, info};

use super::state::BrowserState;
use crate::config::{BrowserConfig, LaunchMode, ProfileMode};
use crate::discovery;
use crate::error::BrowserError;
use crate::launcher::firefox::{LaunchPlan, build_args, managed_prefs};
use crate::ports::{PairRequest, PortAllocator, PortReservation, PortSearch};
use crate::probe;
use crate::process::{self, Readiness, SpawnSpec};
use crate::profile::{self, LockState, Profile, ProfileSpec};

/// How many ports `Auto` tries after a taken one.
const NEAR_TRIES: u16 = 100;

/// What answers on the configured port (never by creating a session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortProbe {
    /// Nothing listens.
    Closed,
    /// Something listens that is not Firefox BiDi.
    NotBidi,
    /// Firefox BiDi with a free session.
    BidiFree,
    /// Firefox BiDi whose only session is taken.
    BidiBusy,
}

/// Where a launch puts its BiDi port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortChoice {
    /// Exactly this port (0 = Firefox picks one).
    Exact(u16),
    /// The first suitable port from this one upwards.
    Near(u16),
}

/// The outcome of [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Attach,
    Launch(PortChoice),
    RemoteUnavailable,
    SessionBusy,
    PortConflict,
}

/// The launch decision of `ensure` as a pure function of the mode, the port,
/// what answers there (`None` when nothing was probed) and whether another
/// managed instance owns that port's per-port profile.
pub(crate) fn decide(mode: LaunchMode, port: u16, probe: Option<PortProbe>, owned: bool) -> Action {
    use PortProbe::{BidiBusy, BidiFree, Closed, NotBidi};
    let next = port.saturating_add(1);
    match (mode, probe) {
        (LaunchMode::LaunchNew, None | Some(Closed)) => Action::Launch(PortChoice::Exact(port)),
        (LaunchMode::LaunchNew, Some(_)) => Action::PortConflict,
        (LaunchMode::AttachOnly, Some(BidiFree)) => Action::Attach,
        (LaunchMode::AttachOnly, Some(BidiBusy)) => Action::SessionBusy,
        (LaunchMode::AttachOnly, _) => Action::RemoteUnavailable,
        (LaunchMode::Auto, None | Some(Closed)) => Action::Launch(PortChoice::Exact(port)),
        (LaunchMode::Auto, Some(NotBidi)) => Action::Launch(PortChoice::Near(next)),
        (LaunchMode::Auto, Some(_)) if owned => Action::Launch(PortChoice::Near(next)),
        (LaunchMode::Auto, Some(BidiFree)) => Action::Attach,
        (LaunchMode::Auto, Some(BidiBusy)) => Action::SessionBusy,
    }
}

/// Obtains a ready browser state for `config` (already validated).
pub(crate) async fn obtain(
    config: &BrowserConfig,
    allocator: &Arc<PortAllocator>,
) -> Result<BrowserState, BrowserError> {
    let (host, port) = (config.host.as_str(), config.port);
    let skip_probe = config.mode == LaunchMode::LaunchNew && port == 0;
    let probe = match skip_probe {
        true => None,
        false => Some(probe_port(config).await),
    };
    let action = decide(
        config.mode,
        port,
        probe,
        per_port_owned(&config.profile, port),
    );
    debug!(?action, port, "launch decision");
    match action {
        Action::Attach => attach(config).await,
        Action::Launch(choice) => launch(config, allocator, choice).await,
        Action::RemoteUnavailable => Err(BrowserError::RemoteUnavailable {
            host: host.to_owned(),
            port,
        }),
        Action::SessionBusy => Err(BrowserError::SessionBusy {
            host: host.to_owned(),
            port,
        }),
        Action::PortConflict => Err(BrowserError::PortConflict { port }),
    }
}

async fn probe_port(config: &BrowserConfig) -> PortProbe {
    let (host, port, limit) = (&config.host, config.port, config.timeouts.connect);
    if !probe::is_port_open(host, port, limit).await {
        return PortProbe::Closed;
    }
    match probe::bidi_session_available(host, port, limit).await {
        Ok(true) => PortProbe::BidiFree,
        Ok(false) => PortProbe::BidiBusy,
        Err(_) => PortProbe::NotBidi,
    }
}

/// Whether a running Firefox owns the per-port profile of `port`.
fn per_port_owned(mode: &ProfileMode, port: u16) -> bool {
    mode.dir_for_port(port)
        .is_some_and(|dir| matches!(profile::lock_state(&dir), LockState::Live(_)))
}

/// Attaches to the Firefox at the configured address: checks the RDP port
/// if one is configured, then opens the session.
pub(crate) async fn attach(config: &BrowserConfig) -> Result<BrowserState, BrowserError> {
    let host = config.host.as_str();
    if let Some(rdp) = config.rdp
        && !probe::is_firefox_rdp(host, rdp, config.timeouts.connect).await
    {
        return Err(BrowserError::RemoteUnavailable {
            host: host.to_owned(),
            port: rdp,
        });
    }
    let session = open_session(config, config.port).await?;
    info!(port = config.port, "attached to Firefox");
    Ok(BrowserState::attached(config, session))
}

/// Opens the browser's single BiDi session: a session-less connection
/// bounded by `connect_timeout`, then `session.new` bounded by
/// `command_timeout` (bidi-lite ends a timed-out `session.new` itself).
pub(crate) async fn open_session(
    config: &BrowserConfig,
    port: u16,
) -> Result<BrowserClient, BrowserError> {
    let host = config.host.as_str();
    let unavailable = || BrowserError::RemoteUnavailable {
        host: host.to_owned(),
        port,
    };
    let url = session_url(&probe::host_port(host, port));
    let connect = BidiClient::connect(&url, config.timeouts.command);
    let client = tokio::time::timeout(config.timeouts.connect, connect)
        .await
        .map_err(|_| unavailable())??;
    match start_session_detached(client).await {
        Ok(client) => Ok(BrowserClient::from_client(client)),
        Err(BidiError::SessionNotCreated(_)) => Err(BrowserError::SessionBusy {
            host: host.to_owned(),
            port,
        }),
        Err(err) => Err(err.into()),
    }
}

/// `session.new` in its own task: if the caller is cancelled while the
/// command is in flight, the task still gets the session and ends it, so a
/// cancelled attach never leaves an orphan in a Firefox we do not own.
async fn start_session_detached(client: BidiClient) -> Result<BidiClient, BidiError> {
    let (reply, result) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let started = client
            .start_session(default_capabilities())
            .await
            .map(|_| client);
        if let Err(Ok(orphan)) = reply.send(started) {
            debug!("ensure was cancelled during session.new; ending the session");
            let _ = orphan.end_session().await;
        }
    });
    result.await.unwrap_or(Err(BidiError::Disconnected))
}

/// Launches a managed Firefox, waits for its ports and opens its session.
/// Every intermediate resource is a guard (reservations, ephemeral profile,
/// child process), so a failure or a cancelled future leaves nothing behind.
pub(crate) async fn launch(
    config: &BrowserConfig,
    allocator: &Arc<PortAllocator>,
    choice: PortChoice,
) -> Result<BrowserState, BrowserError> {
    let (near, bidi) = match choice {
        PortChoice::Exact(port) => (None, port),
        PortChoice::Near(base) => {
            let reservation = reserve_near(config, allocator, base)?;
            let port = reservation.port();
            (Some(reservation), port)
        }
    };
    let request = PairRequest {
        host: config.host.clone(),
        bidi: if near.is_some() { 0 } else { bidi },
        rdp: config.rdp,
    };
    let pair = allocator.reserve_pair(&request)?;
    let rdp = pair.rdp.as_ref().map(PortReservation::port);
    let path = firefox_path(config)?;
    let profile = prepare_profile(config, &path, bidi)?;
    // Declared after `profile`: if this future is cancelled, the process is
    // dropped (killed, exit awaited) before its profile is removed.
    let mut process = start(config, &path, &profile, (bidi, rdp))?;
    let ready = Readiness {
        host: &config.host,
        bidi_port: bidi,
        rdp_port: rdp,
        profile_dir: profile.dir(),
        timeout: config.timeouts.startup,
    };
    let port = process::wait_ready(&mut process, &ready).await?;
    info!(port, rdp = ?rdp, pid = ?process.id(), "launched Firefox");
    let mut state = BrowserState::managed(process, profile, (port, rdp));
    match open_session(config, port).await {
        Ok(session) => {
            state.set_session(session, config.keep_alive_on_drop);
            Ok(state)
        }
        Err(err) => {
            state.shutdown(config).await;
            Err(err)
        }
    }
}

fn reserve_near(
    config: &BrowserConfig,
    allocator: &Arc<PortAllocator>,
    base: u16,
) -> Result<PortReservation, BrowserError> {
    let search = PortSearch::new(config.host.clone(), base, NEAR_TRIES);
    let accept = |port| Some(port) != config.rdp && !per_port_owned(&config.profile, port);
    allocator.reserve_near(&search, accept)
}

fn firefox_path(config: &BrowserConfig) -> Result<PathBuf, BrowserError> {
    match &config.firefox_path {
        Some(path) => Ok(path.clone()),
        None => discovery::discover_default(),
    }
}

fn prepare_profile(
    config: &BrowserConfig,
    firefox: &std::path::Path,
    bidi: u16,
) -> Result<Profile, BrowserError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let root = profile::ephemeral_root(discovery::is_snap_firefox(firefox), home.as_deref())?;
    let proxy = config.proxy_spec()?;
    let prefs = managed_prefs(config, proxy.as_ref());
    let spec = ProfileSpec {
        mode: &config.profile,
        bidi_port: bidi,
        ephemeral_root: &root,
        prefs: &prefs,
    };
    Ok(Profile::prepare(&spec)?)
}

fn start(
    config: &BrowserConfig,
    path: &std::path::Path,
    profile: &Profile,
    (bidi, rdp): (u16, Option<u16>),
) -> Result<process::FirefoxProcess, BrowserError> {
    let profile_dir = profile
        .dir()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let plan = LaunchPlan {
        bidi_port: bidi,
        rdp_port: rdp,
        profile_dir: &profile_dir,
        config,
        wait_for_browser: cfg!(windows),
    };
    process::start(SpawnSpec {
        path,
        args: build_args(&plan),
        env: &config.env_extra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::{Attach, Launch, PortConflict, RemoteUnavailable, SessionBusy};
    use LaunchMode::{AttachOnly, Auto, LaunchNew};
    use PortChoice::{Exact, Near};
    use PortProbe::{BidiBusy, BidiFree, Closed, NotBidi};

    /// BRW-09: decide() covers every row of the decision table.
    #[test]
    fn decision_table() {
        let rows = [
            (AttachOnly, Some(Closed), false, RemoteUnavailable),
            (AttachOnly, Some(NotBidi), false, RemoteUnavailable),
            (AttachOnly, Some(BidiFree), false, Attach),
            (AttachOnly, Some(BidiBusy), false, SessionBusy),
            (AttachOnly, Some(BidiFree), true, Attach),
            (LaunchNew, None, false, Launch(Exact(9300))),
            (LaunchNew, Some(Closed), false, Launch(Exact(9300))),
            (LaunchNew, Some(NotBidi), false, PortConflict),
            (LaunchNew, Some(BidiFree), false, PortConflict),
            (Auto, Some(Closed), false, Launch(Exact(9300))),
            (Auto, Some(NotBidi), false, Launch(Near(9301))),
            (Auto, Some(BidiFree), true, Launch(Near(9301))),
            (Auto, Some(BidiBusy), true, Launch(Near(9301))),
            (Auto, Some(BidiFree), false, Attach),
            (Auto, Some(BidiBusy), false, SessionBusy),
        ];
        for (mode, probe, owned, expected) in rows {
            assert_eq!(
                decide(mode, 9300, probe, owned),
                expected,
                "{mode:?} {probe:?} owned={owned}"
            );
        }
        assert_eq!(decide(LaunchNew, 0, None, false), Launch(Exact(0)));
        assert_eq!(
            decide(Auto, u16::MAX, Some(NotBidi), false),
            Launch(Near(u16::MAX))
        );
    }
}
