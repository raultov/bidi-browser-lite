//! POL: a pool of managed Firefox instances (the fake helper).

use std::collections::HashSet;

use bidi_browser_lite::{BrowserPool, probe};

use crate::support::helper_config;

const HOST: &str = "127.0.0.1";
const PROBE: std::time::Duration = std::time::Duration::from_millis(500);

/// POL-01: Two opens with port 0 yield distinct ids, BiDi ports, RDP ports and profile dirs.
#[tokio::test]
async fn two_opens_are_distinct() {
    let pool = BrowserPool::new();
    let (a, _sa) = helper_config("serve");
    let (b, _sb) = helper_config("serve");
    let first = pool.open(a.rdp(true).build()).await.unwrap();
    let second = pool.open(b.rdp(true).build()).await.unwrap();
    let (x, y) = (
        pool.entry(first).await.unwrap(),
        pool.entry(second).await.unwrap(),
    );
    assert_ne!(first, second);
    assert_ne!(x.port, y.port);
    assert_ne!(x.rdp_port, y.rdp_port);
    assert_ne!(x.profile_dir, y.profile_dir);
    assert!(x.managed && y.managed);
    pool.close_all().await.unwrap();
}

/// POL-02: Eight concurrent opens all succeed with distinct ports.
#[tokio::test]
async fn eight_concurrent_opens() {
    let pool = BrowserPool::new();
    let opens: Vec<_> = (0..8)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move {
                let (builder, scratch) = helper_config("serve");
                let id = pool.open(builder.rdp(true).build()).await;
                (id, scratch)
            })
        })
        .collect();
    let mut keep = Vec::new();
    for open in opens {
        let (id, scratch) = open.await.unwrap();
        id.unwrap();
        keep.push(scratch);
    }
    let entries = pool.entries().await;
    let ports: HashSet<u16> = entries
        .iter()
        .flat_map(|e| [Some(e.port), e.rdp_port])
        .flatten()
        .collect();
    assert_eq!(entries.len(), 8);
    assert_eq!(ports.len(), 16, "every BiDi and RDP port is distinct");
    pool.close_all().await.unwrap();
}

/// POL-05: close stops the browser and removes the entry.
#[tokio::test]
async fn close_stops_and_removes() {
    let pool = BrowserPool::new();
    let (builder, _scratch) = helper_config("serve");
    let id = pool.open(builder.build()).await.unwrap();
    let port = pool.entry(id).await.unwrap().port;
    pool.close(id).await.unwrap();
    assert!(pool.entry(id).await.is_none());
    assert!(
        !probe::is_port_open(HOST, port, PROBE).await,
        "the Firefox is gone"
    );
}

/// POL-09: The entry reports the resolved BiDi port when opened with port 0.
#[tokio::test]
async fn entry_reports_resolved_port() {
    let pool = BrowserPool::new();
    let (builder, _scratch) = helper_config("serve");
    let id = pool.open(builder.build()).await.unwrap();
    let port = pool.entry(id).await.unwrap().port;
    assert!(port != 0 && probe::is_firefox_bidi(HOST, port, PROBE).await);
    pool.close_all().await.unwrap();
}

/// POL-11: Dropping the pool kills managed browsers that nobody else holds.
#[cfg(unix)]
#[tokio::test]
async fn dropping_pool_kills_managed_browsers() {
    let pool = BrowserPool::new();
    let (builder, scratch) = helper_config("serve");
    pool.open(builder.build()).await.unwrap();
    drop(pool);
    assert!(crate::support::wait_gone(crate::support::helper_pid(&scratch)).await);
}

/// POL-12: open retries with a fresh launch after a port race, at most 5 times.
#[tokio::test]
async fn open_retries_after_a_lost_port_race() {
    let pool = BrowserPool::new();
    let (builder, scratch) = helper_config("serve");
    let marker = scratch.path().join("lost-once");
    let id = pool
        .open(builder.env_var("FAKE_FIREFOX_FAIL_FIRST", &marker).build())
        .await;
    assert!(id.is_ok(), "{id:?}");
    assert!(marker.exists(), "the first launch lost the race");
    pool.close_all().await.unwrap();
}
