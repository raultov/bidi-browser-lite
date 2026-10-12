//! Integration tests that spawn the `fake_firefox_helper` binary. One test
//! binary with a module per area, so the shared helpers in `support` are
//! compiled once. `cfg(test)` marks every module as test code for clippy.

#[cfg(test)]
mod browser_lifecycle;
// Two attributes, not cfg(all(..)): clippy only treats a plain cfg(test)
// module as test code.
#[cfg(test)]
#[cfg(unix)]
mod browser_lifecycle_unix;
#[cfg(test)]
mod fake_helper;
#[cfg(test)]
mod pool;
#[cfg(test)]
mod process;
#[cfg(test)]
mod secrets;
#[cfg(test)]
mod support;
