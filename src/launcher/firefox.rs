//! Firefox command-line flags and `user.js` prefs, as pure functions.
//!
//! Every choice here was verified against Firefox 157 in the spike: BiDi on
//! `--remote-debugging-port` (0 = Firefox picks and writes its port file),
//! RDP on `--start-debugger-server ws:<port>` (never 0: Firefox reads it as
//! 6000), window size through `--width`/`--height` (`--window-size` is
//! ignored), and proxy and user agent through prefs (Firefox has no flags for
//! them).

use std::ffi::OsString;
use std::path::Path;

use crate::config::{BrowserConfig, PrefValue, ProxySpec};

/// Everything `build_args` needs for one launch.
#[derive(Debug)]
pub(crate) struct LaunchPlan<'a> {
    /// BiDi port; 0 lets Firefox pick one.
    pub(crate) bidi_port: u16,
    /// RDP port when RDP is enabled (always a real port).
    pub(crate) rdp_port: Option<u16>,
    /// Profile directory.
    pub(crate) profile_dir: &'a Path,
    /// The browser configuration.
    pub(crate) config: &'a BrowserConfig,
    /// Windows' `firefox.exe` starts a short-lived launcher process unless
    /// told to wait for the browser (Puppeteer passes this flag there too).
    pub(crate) wait_for_browser: bool,
}

/// The Firefox command line, in a stable order; the user's extra arguments
/// come last so they can override the library's.
pub(crate) fn build_args(plan: &LaunchPlan<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> =
        vec![format!("--remote-debugging-port={}", plan.bidi_port).into()];
    if let Some(rdp) = plan.rdp_port {
        args.push("--start-debugger-server".into());
        args.push(format!("ws:{rdp}").into());
    }
    // Isolation comes from --profile; --no-remote is defence in depth.
    args.push("--no-remote".into());
    args.push("--profile".into());
    args.push(plan.profile_dir.as_os_str().to_owned());
    if plan.wait_for_browser {
        args.push("--wait-for-browser".into());
    }
    if plan.config.headless {
        args.push("--headless".into());
    }
    if let Some((width, height)) = plan.config.window_size {
        args.extend(["--width".into(), width.to_string().into()]);
        args.extend(["--height".into(), height.to_string().into()]);
    }
    args.extend(plan.config.extra_args.iter().map(OsString::from));
    args
}

/// Prefs that make startup deterministic. Firefox's remote agent applies its
/// own recommended prefs once it starts; these duplicate the early-startup
/// ones so even the first launch of a profile behaves.
fn base_prefs() -> Vec<(&'static str, PrefValue)> {
    vec![
        // No "make Firefox your default browser" prompt.
        ("browser.shell.checkDefaultBrowser", false.into()),
        // No update download or restart prompt in the middle of a run.
        ("app.update.disabledForTesting", true.into()),
        // No "restore previous session" after a kill.
        ("browser.sessionstore.resume_from_crash", false.into()),
        ("toolkit.startup.max_resumed_crashes", (-1).into()),
        // No "what's new" page after an upgrade, no welcome page.
        ("browser.startup.homepage_override.mstone", "ignore".into()),
        ("startup.homepage_welcome_url", "about:blank".into()),
        // No data-reporting notification bar.
        ("datareporting.policy.dataSubmissionEnabled", false.into()),
        (
            "datareporting.policy.dataSubmissionPolicyBypassNotification",
            true.into(),
        ),
        // No "close all tabs?" dialog on browser.close.
        ("browser.warnOnQuit", false.into()),
    ]
}

/// Prefs the debugger server needs. With them, any local process reaching
/// the RDP port can drive the browser with chrome privileges, so they are
/// written only when RDP is enabled.
fn rdp_prefs() -> Vec<(&'static str, PrefValue)> {
    vec![
        ("devtools.debugger.remote-enabled", true.into()),
        ("devtools.chrome.enabled", true.into()),
        ("devtools.debugger.prompt-connection", false.into()),
    ]
}

/// Prefs routing every request through `proxy`.
pub(crate) fn proxy_prefs(proxy: &ProxySpec) -> Vec<(&'static str, PrefValue)> {
    let mut prefs = vec![("network.proxy.type", PrefValue::Int(1))];
    match proxy {
        ProxySpec::Http { host, port } => prefs.extend([
            ("network.proxy.http", host.as_str().into()),
            ("network.proxy.http_port", (*port).into()),
            ("network.proxy.ssl", host.as_str().into()),
            ("network.proxy.ssl_port", (*port).into()),
        ]),
        ProxySpec::Socks5 { host, port } => prefs.extend([
            ("network.proxy.socks", host.as_str().into()),
            ("network.proxy.socks_port", (*port).into()),
            ("network.proxy.socks_version", 5.into()),
            ("network.proxy.socks_remote_dns", true.into()),
        ]),
    }
    prefs
}

/// The managed `user.js` block: base prefs, RDP prefs (if enabled), proxy,
/// user agent, then the user's own prefs, which therefore win.
pub(crate) fn managed_prefs(
    config: &BrowserConfig,
    proxy: Option<&ProxySpec>,
) -> Vec<(String, PrefValue)> {
    let mut prefs = base_prefs();
    if config.rdp.is_some() {
        prefs.extend(rdp_prefs());
    }
    if let Some(proxy) = proxy {
        prefs.extend(proxy_prefs(proxy));
    }
    if let Some(agent) = &config.user_agent {
        prefs.push(("general.useragent.override", agent.as_str().into()));
    }
    let mut prefs: Vec<(String, PrefValue)> = prefs
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    prefs.extend(config.extra_prefs.iter().cloned());
    prefs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BrowserConfigBuilder, LaunchMode};

    fn args_for(builder: BrowserConfigBuilder, bidi: u16, rdp: Option<u16>) -> Vec<String> {
        let config = builder.build();
        let plan = LaunchPlan {
            bidi_port: bidi,
            rdp_port: rdp,
            profile_dir: Path::new("/p"),
            config: &config,
            wait_for_browser: false,
        };
        build_args(&plan)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn launch() -> BrowserConfigBuilder {
        BrowserConfig::builder().mode(LaunchMode::LaunchNew)
    }

    fn pref<'a>(prefs: &'a [(String, PrefValue)], name: &str) -> Option<&'a PrefValue> {
        prefs.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    /// LCH-01: build_args starts with the BiDi port flag.
    #[test]
    fn starts_with_bidi_port() {
        assert_eq!(args_for(launch(), 0, None)[0], "--remote-debugging-port=0");
        assert_eq!(
            args_for(launch(), 9300, None)[0],
            "--remote-debugging-port=9300"
        );
    }

    /// LCH-02: build_args isolates the instance from a running Firefox.
    #[test]
    fn isolates_instance() {
        assert!(args_for(launch(), 0, None).contains(&"--no-remote".to_owned()));
    }

    /// LCH-03: build_args passes the profile directory.
    #[test]
    fn passes_profile_dir() {
        let args = args_for(launch(), 0, None);
        let i = args.iter().position(|a| a == "--profile").unwrap();
        assert_eq!(args[i + 1], "/p");
    }

    /// LCH-04: RDP flag present only when RDP is enabled, with ws:<allocated port>, never ws:0.
    #[test]
    fn rdp_flag_only_when_enabled() {
        let with = args_for(launch(), 0, Some(6123));
        let i = with
            .iter()
            .position(|a| a == "--start-debugger-server")
            .unwrap();
        assert_eq!(with[i + 1], "ws:6123");
        assert!(
            !args_for(launch(), 0, None)
                .iter()
                .any(|a| a.contains("debugger"))
        );
    }

    /// LCH-05: headless adds --headless; visible does not.
    #[test]
    fn headless_flag() {
        assert!(args_for(launch().headless(true), 0, None).contains(&"--headless".to_owned()));
        assert!(!args_for(launch(), 0, None).contains(&"--headless".to_owned()));
    }

    /// LCH-06: managed_prefs contains every base pref and no RDP pref when RDP is off.
    #[test]
    fn base_prefs_without_rdp() {
        let prefs = managed_prefs(&launch().build(), None);
        for (name, value) in base_prefs() {
            assert_eq!(pref(&prefs, name), Some(&value), "{name}");
        }
        assert!(!prefs.iter().any(|(n, _)| n.starts_with("devtools.")));
    }

    /// LCH-07: window_size maps to --width w --height h.
    #[test]
    fn window_size_flags() {
        let args = args_for(launch().window_size(800, 600), 0, None);
        let i = args.iter().position(|a| a == "--width").unwrap();
        assert_eq!(args[i..i + 4], ["--width", "800", "--height", "600"]);
    }

    /// LCH-08: window_size absent adds no size flag.
    #[test]
    fn no_window_size_no_flags() {
        let args = args_for(launch(), 0, None);
        assert!(!args.iter().any(|a| a == "--width" || a == "--height"));
    }

    /// LCH-09: proxy_prefs for http (HTTP and HTTPS) and socks5.
    #[test]
    fn proxy_prefs_for_http_and_socks5() {
        let http = ProxySpec::Http {
            host: "127.0.0.1".into(),
            port: 3128,
        };
        let prefs: Vec<(String, PrefValue)> = managed_prefs(&launch().build(), Some(&http));
        assert_eq!(pref(&prefs, "network.proxy.type"), Some(&PrefValue::Int(1)));
        assert_eq!(pref(&prefs, "network.proxy.ssl"), Some(&"127.0.0.1".into()));
        assert_eq!(
            pref(&prefs, "network.proxy.ssl_port"),
            Some(&PrefValue::Int(3128))
        );
        let socks = ProxySpec::Socks5 {
            host: "s".into(),
            port: 1080,
        };
        let prefs = managed_prefs(&launch().build(), Some(&socks));
        assert_eq!(
            pref(&prefs, "network.proxy.socks_version"),
            Some(&PrefValue::Int(5))
        );
        assert_eq!(
            pref(&prefs, "network.proxy.socks_remote_dns"),
            Some(&PrefValue::Bool(true))
        );
        assert!(pref(&prefs, "network.proxy.http").is_none());
    }

    /// LCH-10: user_agent becomes general.useragent.override.
    #[test]
    fn user_agent_pref() {
        let prefs = managed_prefs(&launch().user_agent("UA/1").build(), None);
        assert_eq!(
            pref(&prefs, "general.useragent.override"),
            Some(&"UA/1".into())
        );
    }

    /// LCH-11: extra_args come last, in insertion order.
    #[test]
    fn extra_args_last() {
        let args = args_for(launch().headless(true).args(["-x", "-y"]), 0, Some(1));
        assert_eq!(args[args.len() - 2..], ["-x", "-y"]);
    }

    /// LCH-12: user prefs come after managed prefs so they win.
    #[test]
    fn user_prefs_win() {
        let config = launch().rdp(true).pref("browser.warnOnQuit", true).build();
        let prefs = managed_prefs(&config, None);
        assert_eq!(
            prefs.last(),
            Some(&("browser.warnOnQuit".to_owned(), PrefValue::Bool(true)))
        );
        assert_eq!(
            pref(&prefs, "browser.warnOnQuit"),
            Some(&PrefValue::Bool(true))
        );
    }

    /// LCH-13: The full argument vector for a reference config matches a golden list.
    #[test]
    fn golden_argument_vector() {
        let builder = launch().headless(true).window_size(1, 2).arg("--x");
        assert_eq!(
            args_for(builder, 9300, Some(6100)),
            [
                "--remote-debugging-port=9300",
                "--start-debugger-server",
                "ws:6100",
                "--no-remote",
                "--profile",
                "/p",
                "--headless",
                "--width",
                "1",
                "--height",
                "2",
                "--x",
            ]
        );
    }

    /// LCH-15: --wait-for-browser is passed when asked (Windows' launcher process).
    #[test]
    fn wait_for_browser_flag() {
        let config = launch().build();
        let plan = |wait| LaunchPlan {
            bidi_port: 0,
            rdp_port: None,
            profile_dir: Path::new("/p"),
            config: &config,
            wait_for_browser: wait,
        };
        assert!(build_args(&plan(true)).contains(&"--wait-for-browser".into()));
        assert!(!build_args(&plan(false)).contains(&"--wait-for-browser".into()));
    }

    /// LCH-14: Enabling RDP adds remote-enabled, chrome.enabled and prompt-connection=false.
    #[test]
    fn rdp_prefs_when_enabled() {
        let prefs = managed_prefs(&launch().rdp(true).build(), None);
        assert_eq!(
            pref(&prefs, "devtools.debugger.remote-enabled"),
            Some(&PrefValue::Bool(true))
        );
        assert_eq!(
            pref(&prefs, "devtools.chrome.enabled"),
            Some(&PrefValue::Bool(true))
        );
        assert_eq!(
            pref(&prefs, "devtools.debugger.prompt-connection"),
            Some(&PrefValue::Bool(false))
        );
    }
}
