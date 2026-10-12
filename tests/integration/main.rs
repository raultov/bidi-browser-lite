//! Integration tests that spawn the `fake_firefox_helper` binary. One test
//! binary with a module per area, so the shared helpers in `support` are
//! compiled once. `cfg(test)` marks every module as test code for clippy.

#[cfg(test)]
mod browser_lifecycle;
#[cfg(test)]
mod fake_helper;
#[cfg(test)]
mod process;
#[cfg(test)]
mod secrets;
#[cfg(test)]
mod support;
