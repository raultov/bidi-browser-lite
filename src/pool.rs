//! [`BrowserPool`]: several browsers with stable ids.
//!
//! Each pool owns a [`PortAllocator`], so its launches never collide with
//! each other. Listing reads [`BrowserEntry`] snapshots taken at open time,
//! so it never waits on a browser that is busy relaunching. One mutex guards
//! the pool's maps and is never held across a launch or a stop.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::browser::Browser;
use crate::config::BrowserConfig;
use crate::error::{BrowserError, EarlyExitReason};
use crate::ports::PortAllocator;

/// How many times an open with a Firefox-picked port is retried after losing
/// a port race (cdp-browser-lite's bound).
const PORT_RACE_RETRIES: usize = 5;

/// Stable identifier of a browser in a [`BrowserPool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BrowserId(u64);

impl BrowserId {
    /// The numeric id.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// Builds an id, for tests.
    #[doc(hidden)]
    pub fn from_u64_for_test(id: u64) -> Self {
        Self(id)
    }
}

impl fmt::Display for BrowserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "browser-{}", self.0)
    }
}

/// A snapshot of one pooled browser, taken when it was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEntry {
    /// The pool id.
    pub id: BrowserId,
    /// BiDi host.
    pub host: String,
    /// BiDi port (the real one for a Firefox-picked port).
    pub port: u16,
    /// RDP port, when RDP is enabled.
    pub rdp_port: Option<u16>,
    /// Profile directory of a managed browser.
    pub profile_dir: Option<PathBuf>,
    /// Whether the pool launched it.
    pub managed: bool,
}

#[derive(Default)]
struct PoolState {
    browsers: BTreeMap<BrowserId, Arc<Browser>>,
    entries: BTreeMap<BrowserId, BrowserEntry>,
}

struct PoolInner {
    allocator: Arc<PortAllocator>,
    next_id: AtomicU64,
    state: Mutex<PoolState>,
}

/// Several browsers with stable ids. Cloning shares the pool. Dropping the
/// last handle drops every browser nobody else holds, which kills the
/// managed ones (see [`Browser`]'s drop behaviour).
#[derive(Clone)]
pub struct BrowserPool {
    inner: Arc<PoolInner>,
}

impl Default for BrowserPool {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for BrowserPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserPool").finish_non_exhaustive()
    }
}

impl BrowserPool {
    /// An empty pool with its own port allocator.
    pub fn new() -> Self {
        Self::with_allocator(PortAllocator::new())
    }

    /// An empty pool sharing `allocator` (e.g. with other pools).
    pub fn with_allocator(allocator: Arc<PortAllocator>) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                allocator,
                next_id: AtomicU64::new(1),
                state: Mutex::new(PoolState::default()),
            }),
        }
    }

    /// Attaches to or launches a browser (see [`Browser::ensure`]) and adds
    /// it to the pool. A fixed port already used by an entry is a
    /// [`BrowserError::PortConflict`] without touching the network.
    pub async fn open(&self, config: BrowserConfig) -> Result<BrowserId, BrowserError> {
        if config.port() != 0 && self.port_in_use(config.port()).await {
            return Err(BrowserError::PortConflict {
                port: config.port(),
            });
        }
        let browser = self.ensure(config).await?;
        let id = BrowserId(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let entry = snapshot(id, &browser).await;
        debug!(%id, port = entry.port, managed = entry.managed, "browser opened");
        let mut state = self.inner.state.lock().await;
        state.entries.insert(id, entry);
        state.browsers.insert(id, Arc::new(browser));
        Ok(id)
    }

    /// The browser `id`, if it is in the pool.
    pub async fn get(&self, id: BrowserId) -> Option<Arc<Browser>> {
        self.inner.state.lock().await.browsers.get(&id).cloned()
    }

    /// The snapshot of browser `id`.
    pub async fn entry(&self, id: BrowserId) -> Option<BrowserEntry> {
        self.inner.state.lock().await.entries.get(&id).cloned()
    }

    /// Every snapshot, ordered by id.
    pub async fn entries(&self) -> Vec<BrowserEntry> {
        self.inner
            .state
            .lock()
            .await
            .entries
            .values()
            .cloned()
            .collect()
    }

    /// How many browsers are in the pool.
    pub async fn len(&self) -> usize {
        self.inner.state.lock().await.browsers.len()
    }

    /// Whether the pool is empty.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Removes browser `id` and stops it (attached browsers only lose their
    /// session). An unknown id is not an error.
    pub async fn close(&self, id: BrowserId) -> Result<(), BrowserError> {
        let removed = {
            let mut state = self.inner.state.lock().await;
            state.entries.remove(&id);
            state.browsers.remove(&id)
        };
        match removed {
            Some(browser) => browser.stop().await,
            None => Ok(()),
        }
    }

    /// Stops every browser and empties the pool. Every browser is attempted;
    /// the first error is returned.
    pub async fn close_all(&self) -> Result<(), BrowserError> {
        let drained = {
            let mut state = self.inner.state.lock().await;
            state.entries.clear();
            std::mem::take(&mut state.browsers)
        };
        let mut first_error = None;
        for (id, browser) in drained {
            if let Err(err) = browser.stop().await {
                warn!(%id, "stopping a pooled browser failed: {err}");
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn port_in_use(&self, port: u16) -> bool {
        let state = self.inner.state.lock().await;
        state
            .entries
            .values()
            .any(|e| e.port == port || e.rdp_port == Some(port))
    }

    /// `Browser::ensure` with this pool's allocator, retrying a
    /// Firefox-picked-port launch that lost a port race.
    async fn ensure(&self, config: BrowserConfig) -> Result<Browser, BrowserError> {
        let retries = if config.port() == 0 {
            PORT_RACE_RETRIES
        } else {
            0
        };
        let mut attempt = 0;
        loop {
            let allocator = Arc::clone(&self.inner.allocator);
            match Browser::ensure_with_allocator(config.clone(), allocator).await {
                Err(err) if attempt < retries && lost_port_race(&err) => {
                    attempt += 1;
                    debug!(attempt, "port race lost; retrying the launch");
                }
                result => return result,
            }
        }
    }
}

fn lost_port_race(err: &BrowserError) -> bool {
    matches!(
        err,
        BrowserError::PortConflict { .. }
            | BrowserError::EarlyExit {
                reason: EarlyExitReason::PortInUse,
                ..
            }
    )
}

async fn snapshot(id: BrowserId, browser: &Browser) -> BrowserEntry {
    let (host, port) = browser.debug_address().await;
    BrowserEntry {
        id,
        host,
        port,
        rdp_port: browser.rdp_address().await.map(|(_, port)| port),
        profile_dir: browser.profile_dir().await,
        managed: browser.is_managed().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LaunchMode;
    use crate::test_support::{MockBidi, MockBidiBehavior};

    fn attach_only(port: u16) -> BrowserConfig {
        BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .port(port)
            .firefox_path("/nonexistent/firefox")
            .build()
    }

    async fn mock() -> MockBidi {
        MockBidi::start(MockBidiBehavior::Ready).await.unwrap()
    }

    /// POL-03: open with a fixed port already in the pool fails with PortConflict without probing.
    #[tokio::test]
    async fn fixed_port_already_pooled_conflicts() {
        let mock = mock().await;
        let pool = BrowserPool::new();
        pool.open(attach_only(mock.port())).await.unwrap();
        let connections = mock.connection_count();
        let again = pool.open(attach_only(mock.port())).await;
        assert!(matches!(again, Err(BrowserError::PortConflict { port }) if port == mock.port()));
        assert_eq!(mock.connection_count(), connections, "no probe was sent");
    }

    /// POL-04: entry/entries are snapshots taken at open, ordered by id.
    #[tokio::test]
    async fn entries_are_ordered_snapshots() {
        let (a, b) = (mock().await, mock().await);
        let pool = BrowserPool::new();
        let first = pool.open(attach_only(a.port())).await.unwrap();
        let second = pool.open(attach_only(b.port())).await.unwrap();
        let entries = pool.entries().await;
        assert_eq!(
            entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            [first, second]
        );
        let entry = pool.entry(first).await.unwrap();
        assert_eq!(
            (entry.port, entry.managed, entry.rdp_port),
            (a.port(), false, None)
        );
        assert_eq!(entry.host, "127.0.0.1");
        assert!(first < second && first.to_string().starts_with("browser-"));
    }

    /// POL-06: close of an unknown id is Ok and changes nothing.
    #[tokio::test]
    async fn close_unknown_id_is_ok() {
        let pool = BrowserPool::new();
        assert!(pool.close(BrowserId::from_u64_for_test(42)).await.is_ok());
        assert!(pool.is_empty().await);
    }

    /// POL-07: close_all stops every browser and empties the pool.
    #[tokio::test]
    async fn close_all_stops_everything() {
        let (a, b) = (mock().await, mock().await);
        let pool = BrowserPool::new();
        pool.open(attach_only(a.port())).await.unwrap();
        pool.open(attach_only(b.port())).await.unwrap();
        pool.close_all().await.unwrap();
        assert!(pool.is_empty().await && pool.entries().await.is_empty());
        assert_eq!((a.count("session.end"), b.count("session.end")), (1, 1));
    }

    /// POL-08: An attached browser in the pool survives close (only its session ends).
    #[tokio::test]
    async fn attached_browser_survives_close() {
        let mock = mock().await;
        let pool = BrowserPool::new();
        let id = pool.open(attach_only(mock.port())).await.unwrap();
        pool.close(id).await.unwrap();
        assert_eq!(mock.count("session.end"), 1);
        assert!(!mock.browser_closed());
        assert!(pool.get(id).await.is_none());
    }

    /// POL-10: A Browser whose Arc is still held answers Stopped after close.
    #[tokio::test]
    async fn held_browser_is_stopped_after_close() {
        let mock = mock().await;
        let pool = BrowserPool::new();
        let id = pool.open(attach_only(mock.port())).await.unwrap();
        let held = pool.get(id).await.unwrap();
        pool.close(id).await.unwrap();
        assert!(matches!(held.client().await, Err(BrowserError::Stopped)));
    }

    /// POL-13: len/is_empty track opens and closes.
    #[tokio::test]
    async fn len_tracks_opens_and_closes() {
        let mock = mock().await;
        let pool = BrowserPool::default();
        assert!(pool.is_empty().await);
        let id = pool.open(attach_only(mock.port())).await.unwrap();
        assert_eq!(pool.len().await, 1);
        pool.close(id).await.unwrap();
        assert_eq!(pool.len().await, 0);
    }

    /// POL-14: Only port races are retried.
    #[test]
    fn only_port_races_are_retried() {
        assert!(lost_port_race(&BrowserError::PortConflict { port: 1 }));
        let in_use = BrowserError::EarlyExit {
            code: Some(69),
            reason: EarlyExitReason::PortInUse,
        };
        assert!(lost_port_race(&in_use));
        assert!(!lost_port_race(&BrowserError::Stopped));
        let handoff = BrowserError::EarlyExit {
            code: Some(0),
            reason: EarlyExitReason::ProfileInUse,
        };
        assert!(!lost_port_race(&handoff));
    }
}
