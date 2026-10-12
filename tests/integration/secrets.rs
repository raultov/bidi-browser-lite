//! SEC: secrets never reach the logs, across a whole lifecycle captured at
//! TRACE level (this crate and bidi-lite). The capture is the process-wide
//! subscriber, so it also sees other tests' logs; none of them uses these
//! secrets.

use std::io::Write;
use std::sync::{Arc, Mutex};

use bidi_browser_lite::ReadinessState;
use tracing_subscriber::fmt::MakeWriter;

use crate::support::helper_config;

const ENV_SECRET: &str = "env-SECRET-7f3a";
const PREF_SECRET: &str = "pref-SECRET-91c2";
const PROXY_HOST: &str = "proxy-secret.invalid";
const PAYLOAD_SECRET: &str = "payload-SECRET-5d0e";

/// Collects every log line in memory.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Launches, uses and stops a browser configured with secrets; returns the logs.
async fn lifecycle_logs() -> String {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    // Global, not scoped: callsites first hit by tests on other threads would
    // otherwise cache "no interest". Only this test installs a subscriber.
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let (builder, _scratch) = helper_config("serve");
    let config = builder
        .env_var("API_TOKEN", ENV_SECRET)
        .pref("extensions.secret", PREF_SECRET)
        .proxy(format!("http://{PROXY_HOST}:3128"))
        .build();
    let browser = crate::support::ensure(config).await.unwrap();
    let tab = browser.new_tab("about:blank").await.unwrap();
    let url = format!("https://example.com/?q={PAYLOAD_SECRET}");
    tab.navigate(&url, ReadinessState::None).await.unwrap();
    browser.stop().await.unwrap();
    String::from_utf8(capture.0.lock().unwrap().clone()).unwrap()
}

/// SEC-01..04: env values, pref values, the proxy and BiDi payloads never appear in logs.
#[tokio::test]
async fn secrets_never_reach_the_logs() {
    let logs = lifecycle_logs().await;
    // The capture works: names and methods are there
    assert!(
        logs.contains("API_TOKEN"),
        "env var names are logged: {logs}"
    );
    assert!(logs.contains("extensions.secret"), "pref names are logged");
    // SEC-01, SEC-02, SEC-03, SEC-04: no value is
    for secret in [ENV_SECRET, PREF_SECRET, PROXY_HOST, PAYLOAD_SECRET] {
        assert!(
            !logs.contains(secret),
            "{secret} leaked into the logs:\n{logs}"
        );
    }
}
