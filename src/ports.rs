//! Port allocation without collisions: [`PortAllocator`],
//! [`PortReservation`] and BiDi + RDP pairs.
//!
//! A reservation says "this process is about to start a browser on this
//! port": it is bindable when reserved and no other reservation from the same
//! allocator can get it until it is dropped. Other processes may still race
//! for it, which is why launches are retried by the pool. The reserved set is
//! guarded by a `std::sync::Mutex` that is never held across an `.await`, so
//! dropping a reservation releases its port immediately.

use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::error::BrowserError;

/// How many times the kernel is asked for a free port before giving up.
const KERNEL_PORT_ATTEMPTS: usize = 32;

/// Hands out ports so that concurrent launches in this process never pick the
/// same one. Each allocator keeps its own set: a pool owns one, and
/// `Browser::ensure` uses a process-wide
/// default.
#[derive(Debug, Default)]
pub struct PortAllocator {
    reserved: Mutex<HashSet<u16>>,
}

/// Where [`PortAllocator::reserve_near`] looks: `tries` ports from `base`
/// upwards on `host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSearch {
    /// Host the ports are bound on to check they are free.
    pub host: String,
    /// First port tried.
    pub base: u16,
    /// How many consecutive ports are tried.
    pub tries: u16,
}

impl PortSearch {
    /// A search of `tries` ports from `base` on `host`.
    pub fn new(host: impl Into<String>, base: u16, tries: u16) -> Self {
        Self {
            host: host.into(),
            base,
            tries,
        }
    }
}

/// The ports one browser needs. `0` means "allocate one" for RDP and "let
/// Firefox pick" for BiDi (Firefox writes the chosen port to its port file;
/// it treats an RDP port 0 as 6000, so RDP is always allocated here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRequest {
    /// Host the ports are checked on.
    pub host: String,
    /// BiDi port; `0` = not reserved, Firefox picks it.
    pub bidi: u16,
    /// RDP port, if RDP is enabled; `Some(0)` = allocate.
    pub rdp: Option<u16>,
}

/// The reservations for one browser, released on drop.
#[derive(Debug)]
pub struct PortPair {
    /// The fixed BiDi port, when one was requested.
    pub bidi: Option<PortReservation>,
    /// The RDP port, when RDP is enabled.
    pub rdp: Option<PortReservation>,
}

/// A reserved port, released when dropped.
#[derive(Debug)]
pub struct PortReservation {
    port: u16,
    owner: Weak<PortAllocator>,
}

impl PortReservation {
    /// The reserved port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for PortReservation {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.lock().remove(&self.port);
        }
    }
}

impl PortAllocator {
    /// A new allocator with an empty reserved set.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether `port` is currently reserved by this allocator.
    pub fn is_reserved(&self, port: u16) -> bool {
        self.lock().contains(&port)
    }

    /// Reserves the first port from `search.base` upwards that is not
    /// reserved, passes `accept` and can be bound on `search.host`. Never
    /// wraps around to port 0.
    pub fn reserve_near(
        self: &Arc<Self>,
        search: &PortSearch,
        accept: impl Fn(u16) -> bool,
    ) -> Result<PortReservation, BrowserError> {
        let mut reserved = self.lock();
        let found = (0..search.tries)
            .map_while(|i| search.base.checked_add(i))
            .filter(|port| *port != 0 && !reserved.contains(port))
            .find(|port| accept(*port) && is_bindable(&search.host, *port))
            .ok_or(BrowserError::PortConflict { port: search.base })?;
        reserved.insert(found);
        Ok(self.reservation(found))
    }

    /// Reserves the ports of one browser under a single lock, so two
    /// concurrent launches never share an RDP port. Fixed ports are reserved
    /// as given (the caller decides what a busy fixed port means); an RDP
    /// port 0 is allocated from the kernel.
    pub fn reserve_pair(self: &Arc<Self>, request: &PairRequest) -> Result<PortPair, BrowserError> {
        let mut reserved = self.lock();
        let bidi = match request.bidi {
            0 => None,
            port => Some(take_fixed(&mut reserved, port)?),
        };
        let rdp = match request.rdp {
            None => Ok(None),
            Some(0) => take_free(&mut reserved, &request.host).map(Some),
            Some(port) => take_fixed(&mut reserved, port).map(Some),
        };
        let rdp = match rdp {
            Ok(rdp) => rdp,
            Err(err) => {
                if let Some(port) = bidi {
                    reserved.remove(&port);
                }
                return Err(err);
            }
        };
        drop(reserved);
        Ok(PortPair {
            bidi: bidi.map(|port| self.reservation(port)),
            rdp: rdp.map(|port| self.reservation(port)),
        })
    }

    fn reservation(self: &Arc<Self>, port: u16) -> PortReservation {
        PortReservation {
            port,
            owner: Arc::downgrade(self),
        }
    }

    /// The reserved set. A panic while it was held cannot leave it
    /// inconsistent (every critical section is a single insert or remove),
    /// so a poisoned lock is simply taken over.
    fn lock(&self) -> MutexGuard<'_, HashSet<u16>> {
        self.reserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn take_fixed(reserved: &mut HashSet<u16>, port: u16) -> Result<u16, BrowserError> {
    if reserved.insert(port) {
        Ok(port)
    } else {
        Err(BrowserError::PortConflict { port })
    }
}

fn take_free(reserved: &mut HashSet<u16>, host: &str) -> Result<u16, BrowserError> {
    for _ in 0..KERNEL_PORT_ATTEMPTS {
        let port = TcpListener::bind((host, 0))?.local_addr()?.port();
        if reserved.insert(port) {
            return Ok(port);
        }
    }
    Err(BrowserError::PortConflict { port: 0 })
}

fn is_bindable(host: &str, port: u16) -> bool {
    TcpListener::bind((host, port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "127.0.0.1";

    /// A base port from the kernel, so tests never hard-code port numbers.
    fn free_base() -> u16 {
        TcpListener::bind((HOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn search(base: u16, tries: u16) -> PortSearch {
        PortSearch::new(HOST, base, tries)
    }

    /// PRT-01: reserve_near returns a port that was bindable and marks it reserved.
    #[test]
    fn reserve_near_marks_a_bindable_port() {
        let alloc = PortAllocator::new();
        let r = alloc
            .reserve_near(&search(free_base(), 50), |_| true)
            .unwrap();
        assert!(alloc.is_reserved(r.port()));
        assert!(
            is_bindable(HOST, r.port()),
            "the port must still be free to bind"
        );
    }

    /// PRT-02: Two live reservations from one allocator never share a port.
    #[test]
    fn live_reservations_never_share_a_port() {
        let alloc = PortAllocator::new();
        let base = free_base();
        let a = alloc.reserve_near(&search(base, 50), |_| true).unwrap();
        let b = alloc.reserve_near(&search(base, 50), |_| true).unwrap();
        assert_ne!(a.port(), b.port());
    }

    /// PRT-03: Dropping a reservation releases the port synchronously.
    #[test]
    fn drop_releases_synchronously() {
        let alloc = PortAllocator::new();
        let r = alloc
            .reserve_near(&search(free_base(), 50), |_| true)
            .unwrap();
        let port = r.port();
        drop(r);
        assert!(!alloc.is_reserved(port), "released on drop, not later");
    }

    /// PRT-04: The accept predicate can reject candidates.
    #[test]
    fn accept_predicate_rejects_candidates() {
        let alloc = PortAllocator::new();
        let base = free_base();
        let r = alloc
            .reserve_near(&search(base, 50), |p| p != base)
            .unwrap();
        assert_ne!(r.port(), base);
    }

    /// PRT-05: Exhausting the search window fails with PortConflict { port: base }.
    #[test]
    fn exhausted_window_is_a_conflict() {
        let alloc = PortAllocator::new();
        let base = free_base();
        let result = alloc.reserve_near(&search(base, 5), |_| false);
        assert!(matches!(result, Err(BrowserError::PortConflict { port }) if port == base));
    }

    /// PRT-06: reserve_pair returns two distinct reserved ports, or one when RDP is off.
    #[test]
    fn reserve_pair_reserves_both_ports() {
        // Given a fixed BiDi port and an allocated RDP port
        let alloc = PortAllocator::new();
        let bidi = free_base();
        let request = PairRequest {
            host: HOST.into(),
            bidi,
            rdp: Some(0),
        };
        // When the pair is reserved
        let pair = alloc.reserve_pair(&request).unwrap();
        // Then both are reserved and distinct
        let (b, r) = (
            pair.bidi.as_ref().unwrap().port(),
            pair.rdp.as_ref().unwrap().port(),
        );
        assert_eq!(b, bidi);
        assert_ne!(b, r);
        assert!(alloc.is_reserved(b) && alloc.is_reserved(r));
        // And without RDP only BiDi; with BiDi 0 nothing for BiDi
        let only = PairRequest {
            host: HOST.into(),
            bidi: 0,
            rdp: None,
        };
        let pair = alloc.reserve_pair(&only).unwrap();
        assert!(pair.bidi.is_none() && pair.rdp.is_none());
    }

    /// PRT-07: Sixteen concurrent reserve_pair calls yield 32 distinct ports.
    #[test]
    fn concurrent_pairs_are_distinct() {
        // Given sixteen threads, each with a distinct fixed BiDi port request
        let alloc = PortAllocator::new();
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let alloc = Arc::clone(&alloc);
                std::thread::spawn(move || {
                    let bidi = free_base();
                    let req = PairRequest {
                        host: HOST.into(),
                        bidi,
                        rdp: Some(0),
                    };
                    alloc.reserve_pair(&req)
                })
            })
            .collect();
        // When they all reserve
        let pairs: Vec<PortPair> = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap().ok())
            .collect();
        // Then every port handed out is unique
        let ports: Vec<u16> = pairs
            .iter()
            .flat_map(|p| [p.bidi.as_ref(), p.rdp.as_ref()])
            .flatten()
            .map(PortReservation::port)
            .collect();
        let unique: HashSet<u16> = ports.iter().copied().collect();
        assert_eq!(unique.len(), ports.len(), "{ports:?}");
        assert!(
            pairs.len() >= 15,
            "the kernel may hand two threads the same base"
        );
    }

    /// PRT-08: A base near u16::MAX never wraps to port 0 and never panics.
    #[test]
    fn base_near_max_never_wraps() {
        let alloc = PortAllocator::new();
        let result = alloc.reserve_near(&search(u16::MAX - 1, 10), |p| p < 100);
        assert!(matches!(result, Err(BrowserError::PortConflict { .. })));
    }

    /// PRT-09: Two allocators keep independent reserved sets.
    #[test]
    fn allocators_are_independent() {
        let (a, b) = (PortAllocator::new(), PortAllocator::new());
        let port = free_base();
        let req = PairRequest {
            host: HOST.into(),
            bidi: port,
            rdp: None,
        };
        let _ra = a.reserve_pair(&req).unwrap();
        assert!(!b.is_reserved(port));
        assert!(b.reserve_pair(&req).is_ok());
        assert!(matches!(
            a.reserve_pair(&req),
            Err(BrowserError::PortConflict { .. })
        ));
    }

    /// PRT-11: A poisoned allocator mutex still serves reservations.
    #[test]
    fn poisoned_mutex_still_serves() {
        // Given a thread that panicked while holding the lock
        let alloc = PortAllocator::new();
        let poisoner = Arc::clone(&alloc);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.reserved.lock();
            panic!("poison the lock");
        })
        .join();
        assert!(alloc.reserved.is_poisoned());
        // Then reserving still works
        assert!(
            alloc
                .reserve_near(&search(free_base(), 50), |_| true)
                .is_ok()
        );
    }

    /// PRT-12: A failed RDP reservation releases the BiDi port it took.
    #[test]
    fn failed_pair_rolls_back_bidi() {
        let alloc = PortAllocator::new();
        let rdp = free_base();
        let bidi = std::iter::repeat_with(free_base)
            .find(|p| *p != rdp)
            .unwrap();
        let _held = alloc.reserve_pair(&PairRequest {
            host: HOST.into(),
            bidi: rdp,
            rdp: None,
        });
        let req = PairRequest {
            host: HOST.into(),
            bidi,
            rdp: Some(rdp),
        };
        assert!(alloc.reserve_pair(&req).is_err());
        assert!(
            !alloc.is_reserved(bidi),
            "the BiDi port must be rolled back"
        );
    }
}
