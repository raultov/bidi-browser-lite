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
pub(crate) const RDP_GET_WATCHER: &str = include_str!("fixtures/rdp/get_watcher.json");
pub(crate) const RDP_TARGET_AVAILABLE: &str = include_str!("fixtures/rdp/target_available.json");
pub(crate) const RDP_TARGET_DESTROYED: &str = include_str!("fixtures/rdp/target_destroyed.json");
pub(crate) const RDP_RESOURCES_SOURCES: &str = include_str!("fixtures/rdp/resources_sources.json");
pub(crate) const RDP_BREAKPOINT_POSITIONS: &str =
    include_str!("fixtures/rdp/breakpoint_positions.json");
pub(crate) const RDP_THREAD_STATE_PAUSED: &str =
    include_str!("fixtures/rdp/thread_state_paused.json");
pub(crate) const RDP_THREAD_STATE_RESUMED: &str =
    include_str!("fixtures/rdp/thread_state_resumed.json");
pub(crate) const RDP_RESOURCES_THREAD_STATE_PAUSED: &str =
    include_str!("fixtures/rdp/resources_thread_state_paused.json");
pub(crate) const RDP_RESOURCES_THREAD_STATE_RESUMED: &str =
    include_str!("fixtures/rdp/resources_thread_state_resumed.json");
pub(crate) const RDP_FRAMES: &str = include_str!("fixtures/rdp/frames.json");
pub(crate) const RDP_EVALUATE_REPLY: &str = include_str!("fixtures/rdp/evaluate_reply.json");
pub(crate) const RDP_EVALUATION_RESULT: &str = include_str!("fixtures/rdp/evaluation_result.json");
pub(crate) const RDP_SOURCE_LONG_STRING: &str =
    include_str!("fixtures/rdp/source_long_string.json");
pub(crate) const RDP_GET_PROCESS: &str = include_str!("fixtures/rdp/get_process.json");
pub(crate) const RDP_PARENT_PROCESS_TARGET: &str =
    include_str!("fixtures/rdp/parent_process_target.json");
pub(crate) const RDP_BROWSER_ID_LOOKUP: &str = include_str!("fixtures/rdp/browser_id_lookup.json");
pub(crate) const RDP_BROWSER_ID_LOOKUP_NULL: &str =
    include_str!("fixtures/rdp/browser_id_lookup_null.json");

/// Captured `listTabs` reply with three tabs, the last two on the same URL
/// (`browserId` 2/11/12, `browsingContextID` 3/12/13), for
/// [`MockRdp::set_tabs`](super::MockRdp::set_tabs).
pub const RDP_LIST_TABS_TWO_TABS: &str = include_str!("fixtures/rdp/list_tabs_two_tabs.json");

/// Captured `getBreakpointPositionsCompressed` reply of the first inline
/// `<script>` of a page for a line it does not cover (`positions: {}`).
pub const RDP_BREAKPOINT_POSITIONS_INLINE_NONE: &str =
    include_str!("fixtures/rdp/breakpoint_positions_inline_none.json");

/// Captured `getBreakpointPositionsCompressed` reply of the second inline
/// `<script>` of the same page for line 9 (`positions: {"9": [2]}`).
pub const RDP_BREAKPOINT_POSITIONS_INLINE_SECOND: &str =
    include_str!("fixtures/rdp/breakpoint_positions_inline_second.json");

/// Captured `forwardingCancelled` notification sent by `root` after a
/// debugger detached and its tab closed.
pub const RDP_FORWARDING_CANCELLED: &str = include_str!("fixtures/rdp/forwarding_cancelled.json");

/// Captured `websocket` resources from Firefox 157 (open, frame sent/recv, close).
pub const RDP_RESOURCES_WEBSOCKET: &str = include_str!("fixtures/rdp/resources_websocket.json");

/// Captured `error-message` resources from Firefox 157 (JS errors and cookie rejections).
pub const RDP_RESOURCES_ERROR_MESSAGE: &str =
    include_str!("fixtures/rdp/resources_error_message.json");

/// Captured `css-message` resources from Firefox 157 (CSS parser warnings).
pub const RDP_RESOURCES_CSS_MESSAGE: &str = include_str!("fixtures/rdp/resources_css_message.json");

/// Synthetic `platform-message` resource array.
pub const RDP_RESOURCES_PLATFORM_MESSAGE: &str =
    include_str!("fixtures/rdp/resources_platform_message.handmade.json");

/// Every embedded fixture, for the parse test.
#[cfg(test)]
pub(crate) const ALL: [&str; 43] = [
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
    RDP_GET_WATCHER,
    RDP_TARGET_AVAILABLE,
    RDP_TARGET_DESTROYED,
    RDP_RESOURCES_SOURCES,
    RDP_BREAKPOINT_POSITIONS,
    RDP_THREAD_STATE_PAUSED,
    RDP_THREAD_STATE_RESUMED,
    RDP_RESOURCES_THREAD_STATE_PAUSED,
    RDP_RESOURCES_THREAD_STATE_RESUMED,
    RDP_FRAMES,
    RDP_EVALUATE_REPLY,
    RDP_EVALUATION_RESULT,
    RDP_SOURCE_LONG_STRING,
    RDP_GET_PROCESS,
    RDP_PARENT_PROCESS_TARGET,
    RDP_BROWSER_ID_LOOKUP,
    RDP_BROWSER_ID_LOOKUP_NULL,
    RDP_LIST_TABS_TWO_TABS,
    RDP_BREAKPOINT_POSITIONS_INLINE_NONE,
    RDP_BREAKPOINT_POSITIONS_INLINE_SECOND,
    RDP_FORWARDING_CANCELLED,
    RDP_RESOURCES_WEBSOCKET,
    RDP_RESOURCES_ERROR_MESSAGE,
    RDP_RESOURCES_CSS_MESSAGE,
    RDP_RESOURCES_PLATFORM_MESSAGE,
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
