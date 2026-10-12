//! Frames captured from Firefox 157.0.1 (Linux) during the spike, embedded
//! so downstream crates do not need the files. Home paths were sanitised.

use serde_json::Value;

pub(crate) const SESSION_STATUS_READY: &str =
    include_str!("fixtures/bidi/session_status_ready.json");
pub(crate) const SESSION_STATUS_BUSY: &str = include_str!("fixtures/bidi/session_status_busy.json");
pub(crate) const SESSION_NEW: &str = include_str!("fixtures/bidi/session_new.json");
pub(crate) const SESSION_NEW_BUSY: &str = include_str!("fixtures/bidi/session_new_busy.json");
pub(crate) const SESSION_END: &str = include_str!("fixtures/bidi/session_end.json");
pub(crate) const INVALID_SESSION_ID: &str = include_str!("fixtures/bidi/invalid_session_id.json");
pub(crate) const GET_TREE: &str = include_str!("fixtures/bidi/get_tree.json");
pub(crate) const CREATE_TAB: &str = include_str!("fixtures/bidi/create_tab.json");
pub(crate) const NAVIGATE: &str = include_str!("fixtures/bidi/navigate.json");
pub(crate) const NO_SUCH_FRAME: &str = include_str!("fixtures/bidi/no_such_frame.json");
pub(crate) const UNKNOWN_COMMAND: &str = include_str!("fixtures/bidi/unknown_command.json");
pub(crate) const SUBSCRIBE: &str = include_str!("fixtures/bidi/subscribe.json");
pub(crate) const BROWSER_CLOSE: &str = include_str!("fixtures/bidi/browser_close.json");
/// Firefox's `WebDriverBiDiServer.json` port file.
#[cfg(test)]
pub(crate) const PORT_FILE: &str = include_str!("fixtures/bidi/webdriver_bidi_server.json");

pub(crate) const RDP_GREETING: &str = include_str!("fixtures/rdp/greeting.json");
pub(crate) const RDP_LIST_TABS: &str = include_str!("fixtures/rdp/list_tabs.json");
pub(crate) const RDP_GET_ROOT: &str = include_str!("fixtures/rdp/get_root.json");
pub(crate) const RDP_UNRECOGNIZED: &str =
    include_str!("fixtures/rdp/unrecognized_packet_type.json");

/// Every embedded fixture, for the parse test.
#[cfg(test)]
pub(crate) const ALL: [&str; 18] = [
    SESSION_STATUS_READY,
    SESSION_STATUS_BUSY,
    SESSION_NEW,
    SESSION_NEW_BUSY,
    SESSION_END,
    INVALID_SESSION_ID,
    GET_TREE,
    CREATE_TAB,
    NAVIGATE,
    NO_SUCH_FRAME,
    UNKNOWN_COMMAND,
    SUBSCRIBE,
    BROWSER_CLOSE,
    PORT_FILE,
    RDP_GREETING,
    RDP_LIST_TABS,
    RDP_GET_ROOT,
    RDP_UNRECOGNIZED,
];

/// Parses a fixture. They are compile-time constants checked by a unit test,
/// so a parse failure can only be a broken fixture; it degrades to `null`.
pub(crate) fn load(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_default()
}

/// A BiDi reply fixture answering command `id`.
pub(crate) fn reply(text: &str, id: u64) -> Value {
    let mut value = load(text);
    value["id"] = Value::from(id);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MCK-11: Every embedded fixture is valid JSON.
    #[test]
    fn every_fixture_parses() {
        for text in ALL {
            assert!(serde_json::from_str::<Value>(text).is_ok(), "{text}");
        }
    }
}
