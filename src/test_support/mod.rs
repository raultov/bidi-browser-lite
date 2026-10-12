//! In-process BiDi and RDP mocks for tests (feature `test-support`).
//!
//! They let this crate and downstream crates (such as an MCP server) test
//! attach, session ownership, probes and stop paths without a real Firefox.
//! Every reply is shaped after frames captured from Firefox 157 (see
//! `fixtures/`), and the mocks reproduce the behaviours that matter for the
//! lifecycle: one BiDi session per browser, a session that outlives its
//! WebSocket, `session.end` closing the socket, and the RDP root greeting.
//!
//! All servers bind `127.0.0.1` (port 0 unless asked otherwise) and shut
//! down when dropped.

mod bidi;
mod fixtures;
mod rdp;
mod server;

pub use bidi::{MockBidi, MockBidiBehavior, RecordedCommand};
pub use rdp::MockRdp;

use std::io;

/// The captured `WebDriverBiDiServer.json`, for the profile tests.
#[cfg(test)]
pub(crate) fn fixtures_port_file() -> &'static str {
    fixtures::PORT_FILE
}

/// A mock Firefox: a [`MockBidi`] plus an optional [`MockRdp`], the shape a
/// downstream test attaches to with `LaunchMode::AttachOnly`.
pub struct MockFirefox {
    bidi: MockBidi,
    rdp: Option<MockRdp>,
}

impl MockFirefox {
    /// Starts a free BiDi endpoint, plus an RDP endpoint when `with_rdp`.
    pub async fn start(with_rdp: bool) -> io::Result<Self> {
        let bidi = MockBidi::start(MockBidiBehavior::Ready).await?;
        let rdp = match with_rdp {
            true => Some(MockRdp::start().await?),
            false => None,
        };
        Ok(Self { bidi, rdp })
    }

    /// The BiDi port.
    pub fn bidi_port(&self) -> u16 {
        self.bidi.port()
    }

    /// The RDP port, when started with RDP.
    pub fn rdp_port(&self) -> Option<u16> {
        self.rdp.as_ref().map(MockRdp::port)
    }

    /// The BiDi mock, to inspect traffic or change its state.
    pub fn bidi(&self) -> &MockBidi {
        &self.bidi
    }

    /// The RDP mock, when started with RDP.
    pub fn rdp(&self) -> Option<&MockRdp> {
        self.rdp.as_ref()
    }
}
