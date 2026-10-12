//! Launch configuration: [`LaunchMode`], [`ProfileMode`], [`BrowserConfig`]
//! and its builder.
//!
//! The names and defaults mirror `cdp-browser-lite` so one mental model
//! serves both crates. Firefox-specific differences: `firefox_path` instead
//! of `chrome_path`, an opt-in RDP port, extra `user.js` prefs, the proxy and
//! user agent translated into prefs, and no `enable_automation`/`no_sandbox`
//! (Firefox has no equivalent switch).

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::ConfigError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How [`Browser::ensure`](crate::browser::Browser::ensure) obtains a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaunchMode {
    /// Attach to a Firefox already answering BiDi on the port, otherwise
    /// launch one there (or nearby, if the port is taken by something else).
    #[default]
    Auto,
    /// Always launch a new managed Firefox.
    LaunchNew,
    /// Only attach to a running Firefox; never launch.
    AttachOnly,
}

/// Which profile directory a launched Firefox uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ProfileMode {
    /// A fresh temporary profile, deleted when the browser stops.
    #[default]
    Ephemeral,
    /// A fixed directory that survives restarts.
    Persistent(PathBuf),
    /// One persistent directory per BiDi port: `root/<prefix><port>`, so
    /// several `Auto` instances can coexist and each reattaches to its own.
    PersistentPerPort {
        /// Parent directory.
        root: PathBuf,
        /// Directory name prefix.
        prefix: String,
    },
    /// The user's own default profile. Only valid with
    /// [`LaunchMode::AttachOnly`]: launching would write remote-control prefs
    /// into the user's real profile.
    UserDefault,
}

impl ProfileMode {
    /// The directory a `PersistentPerPort` profile uses for `port`; `None`
    /// for every other mode.
    pub fn dir_for_port(&self, port: u16) -> Option<PathBuf> {
        match self {
            Self::PersistentPerPort { root, prefix } => Some(root.join(format!("{prefix}{port}"))),
            _ => None,
        }
    }
}

/// A Firefox preference value as written to `user.js`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefValue {
    /// `true` / `false`.
    Bool(bool),
    /// An integer pref.
    Int(i64),
    /// A string pref.
    String(String),
}

impl From<bool> for PrefValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for PrefValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<i32> for PrefValue {
    fn from(value: i32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<u16> for PrefValue {
    fn from(value: u16) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<&str> for PrefValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for PrefValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

/// A parsed proxy, translated into `network.proxy.*` prefs at launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxySpec {
    /// An HTTP proxy, used for both HTTP and HTTPS (`CONNECT`).
    Http { host: String, port: u16 },
    /// A SOCKS5 proxy with remote DNS.
    Socks5 { host: String, port: u16 },
}

/// The three timeouts every operation is bounded by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timeouts {
    pub(crate) connect: Duration,
    pub(crate) startup: Duration,
    pub(crate) command: Duration,
}

/// Validated settings for one browser. Build it with
/// [`BrowserConfig::builder`]; [`BrowserConfig::validate`] runs inside
/// [`Browser::ensure`](crate::browser::Browser::ensure).
#[derive(Clone)]
pub struct BrowserConfig {
    pub(crate) mode: LaunchMode,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) rdp: Option<u16>,
    pub(crate) headless: bool,
    pub(crate) profile: ProfileMode,
    pub(crate) proxy: Option<String>,
    pub(crate) firefox_path: Option<PathBuf>,
    pub(crate) extra_args: Vec<String>,
    pub(crate) extra_prefs: Vec<(String, PrefValue)>,
    pub(crate) window_size: Option<(u32, u32)>,
    pub(crate) user_agent: Option<String>,
    pub(crate) timeouts: Timeouts,
    pub(crate) keep_alive_on_drop: bool,
    pub(crate) auto_relaunch: bool,
    pub(crate) env_extra: Vec<(OsString, OsString)>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            mode: LaunchMode::Auto,
            host: "127.0.0.1".to_owned(),
            port: 0,
            rdp: None,
            headless: false,
            profile: ProfileMode::Ephemeral,
            proxy: None,
            firefox_path: None,
            extra_args: Vec::new(),
            extra_prefs: Vec::new(),
            window_size: None,
            user_agent: None,
            timeouts: Timeouts {
                connect: DEFAULT_TIMEOUT,
                startup: DEFAULT_TIMEOUT,
                command: DEFAULT_TIMEOUT,
            },
            keep_alive_on_drop: false,
            auto_relaunch: false,
            env_extra: Vec::new(),
        }
    }
}

impl BrowserConfig {
    /// Starts a builder with the defaults: `Auto`, `127.0.0.1`, port 0, RDP
    /// off, visible window, ephemeral profile, 10 s timeouts.
    pub fn builder() -> BrowserConfigBuilder {
        BrowserConfigBuilder::default()
    }

    /// The launch mode.
    pub fn mode(&self) -> LaunchMode {
        self.mode
    }

    /// The host of the BiDi endpoint.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The BiDi port (`0` = let Firefox pick one, `LaunchNew` only).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The RDP port: `None` when RDP is off, `Some(0)` to allocate one.
    pub fn rdp_port(&self) -> Option<u16> {
        self.rdp
    }

    /// The profile mode.
    pub fn profile(&self) -> &ProfileMode {
        &self.profile
    }

    /// Whether Firefox runs headless.
    pub fn headless(&self) -> bool {
        self.headless
    }

    /// Checks every rule a configuration must satisfy before anything is
    /// probed or launched.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.check_timeouts()?;
        self.check_port()?;
        self.check_host()?;
        self.check_profile()?;
        self.check_rdp()?;
        self.check_window()?;
        self.proxy_spec().map(drop)
    }

    /// The parsed proxy, if one is configured.
    pub(crate) fn proxy_spec(&self) -> Result<Option<ProxySpec>, ConfigError> {
        self.proxy.as_deref().map(parse_proxy).transpose()
    }

    fn launches(&self) -> bool {
        self.mode != LaunchMode::AttachOnly
    }

    fn check_timeouts(&self) -> Result<(), ConfigError> {
        let t = self.timeouts;
        let named = [
            ("connect_timeout", t.connect),
            ("startup_timeout", t.startup),
            ("command_timeout", t.command),
        ];
        match named.into_iter().find(|(_, value)| value.is_zero()) {
            Some((which, _)) => Err(ConfigError::ZeroTimeout { which }),
            None => Ok(()),
        }
    }

    fn check_port(&self) -> Result<(), ConfigError> {
        if self.port == 0 && self.mode != LaunchMode::LaunchNew {
            return Err(ConfigError::PortRequired { mode: self.mode });
        }
        Ok(())
    }

    fn check_host(&self) -> Result<(), ConfigError> {
        if self.launches() && !is_local_host(&self.host) {
            return Err(ConfigError::RemoteHostRequiresAttachOnly {
                host: self.host.clone(),
            });
        }
        Ok(())
    }

    fn check_profile(&self) -> Result<(), ConfigError> {
        if self.launches() && self.profile == ProfileMode::UserDefault {
            return Err(ConfigError::UserDefaultProfileUnsupported);
        }
        Ok(())
    }

    fn check_rdp(&self) -> Result<(), ConfigError> {
        match self.rdp {
            Some(0) if !self.launches() => Err(ConfigError::RdpPortRequired),
            Some(port) if port != 0 && port == self.port => Err(ConfigError::PortClash { port }),
            _ => Ok(()),
        }
    }

    fn check_window(&self) -> Result<(), ConfigError> {
        match self.window_size {
            Some((w, h)) if w == 0 || h == 0 => Err(ConfigError::InvalidWindowSize),
            _ => Ok(()),
        }
    }
}

/// Never shows environment variable values, pref values or the proxy URL:
/// they may carry tokens or credentials.
impl fmt::Debug for BrowserConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env_names: Vec<&OsString> = self.env_extra.iter().map(|(k, _)| k).collect();
        let pref_names: Vec<&String> = self.extra_prefs.iter().map(|(k, _)| k).collect();
        f.debug_struct("BrowserConfig")
            .field("mode", &self.mode)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("rdp", &self.rdp)
            .field("headless", &self.headless)
            .field("profile", &self.profile)
            .field("proxy", &self.proxy.as_ref().map(|_| "<redacted>"))
            .field("firefox_path", &self.firefox_path)
            .field("extra_args", &self.extra_args.len())
            .field("extra_prefs", &pref_names)
            .field("window_size", &self.window_size)
            .field("user_agent", &self.user_agent)
            .field("timeouts", &self.timeouts)
            .field("keep_alive_on_drop", &self.keep_alive_on_drop)
            .field("auto_relaunch", &self.auto_relaunch)
            .field("env_vars", &env_names)
            .finish()
    }
}

/// Whether `host` names this machine (`127.0.0.1`, `::1`, `[::1]`,
/// `localhost`; case-insensitive, trimmed).
pub(crate) fn is_local_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    matches!(host.as_str(), "127.0.0.1" | "::1" | "[::1]" | "localhost")
}

/// Parses `http://host:port` or `socks5://host:port` (a trailing `/` is
/// allowed). The URL is never echoed back in errors.
fn parse_proxy(url: &str) -> Result<ProxySpec, ConfigError> {
    let invalid = |reason| ConfigError::InvalidProxy { reason };
    let (scheme, rest) = url
        .trim()
        .split_once("://")
        .ok_or(invalid("expected scheme://host:port"))?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains('@') {
        return Err(ConfigError::ProxyCredentialsUnsupported);
    }
    let (host, port) = split_host_port(authority)?;
    match scheme.to_ascii_lowercase().as_str() {
        "http" => Ok(ProxySpec::Http { host, port }),
        "socks5" => Ok(ProxySpec::Socks5 { host, port }),
        _ => Err(invalid("unsupported scheme; use http:// or socks5://")),
    }
}

fn split_host_port(authority: &str) -> Result<(String, u16), ConfigError> {
    let invalid = |reason| ConfigError::InvalidProxy { reason };
    let (host, port) = authority.rsplit_once(':').ok_or(invalid("missing port"))?;
    if host.is_empty() || host.contains('/') {
        return Err(invalid("missing host"));
    }
    match port.parse::<u16>() {
        Ok(port) if port != 0 => Ok((host.to_owned(), port)),
        _ => Err(invalid("port must be 1-65535")),
    }
}

/// Builder for [`BrowserConfig`]. Every setter takes and returns `self`.
#[derive(Debug, Clone, Default)]
pub struct BrowserConfigBuilder {
    config: BrowserConfig,
}

impl BrowserConfigBuilder {
    /// How the browser is obtained. Default: [`LaunchMode::Auto`].
    pub fn mode(mut self, mode: LaunchMode) -> Self {
        self.config.mode = mode;
        self
    }

    /// Host of the BiDi endpoint. Default `127.0.0.1`; only `AttachOnly` may
    /// use a non-local host.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.config.host = host.into();
        self
    }

    /// BiDi port. `0` (default) lets Firefox pick a free one (`LaunchNew`).
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Enables (`true`, on an allocated port) or disables the Firefox
    /// debugger server (RDP). Off by default: with RDP on, any local process
    /// reaching the port can drive the browser with chrome privileges.
    pub fn rdp(mut self, enabled: bool) -> Self {
        self.config.rdp = enabled.then_some(0);
        self
    }

    /// Enables RDP on a fixed port (`0` = allocate one).
    pub fn rdp_port(mut self, port: u16) -> Self {
        self.config.rdp = Some(port);
        self
    }

    /// Runs Firefox without a window. Default: visible.
    pub fn headless(mut self, headless: bool) -> Self {
        self.config.headless = headless;
        self
    }

    /// Profile mode. Default: [`ProfileMode::Ephemeral`].
    pub fn profile(mut self, profile: ProfileMode) -> Self {
        self.config.profile = profile;
        self
    }

    /// Proxy as `http://host:port` (HTTP and HTTPS) or `socks5://host:port`.
    /// Credentials are refused: answer proxy auth through BiDi
    /// `network.continueWithAuth`.
    pub fn proxy(mut self, url: impl Into<String>) -> Self {
        self.config.proxy = Some(url.into());
        self
    }

    /// Firefox executable; wins over `FIREFOX_PATH` and discovery.
    pub fn firefox_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.firefox_path = Some(path.into());
        self
    }

    /// Appends one command-line argument. Extra arguments go last so they can
    /// override the library's flags.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.config.extra_args.push(arg.into());
        self
    }

    /// Appends several command-line arguments, in order.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config
            .extra_args
            .extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets a Firefox pref in the profile's `user.js` (after the library's
    /// own prefs, so it wins). The same name twice keeps the last value.
    pub fn pref(mut self, name: impl Into<String>, value: impl Into<PrefValue>) -> Self {
        let name = name.into();
        self.config
            .extra_prefs
            .retain(|(existing, _)| *existing != name);
        self.config.extra_prefs.push((name, value.into()));
        self
    }

    /// Sets several prefs, in order.
    pub fn prefs<I, K, V>(self, prefs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<PrefValue>,
    {
        prefs
            .into_iter()
            .fold(self, |builder, (name, value)| builder.pref(name, value))
    }

    /// Window size in CSS pixels (`--width`/`--height`).
    pub fn window_size(mut self, width: u32, height: u32) -> Self {
        self.config.window_size = Some((width, height));
        self
    }

    /// Overrides `navigator.userAgent` and the `User-Agent` header.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.config.user_agent = Some(user_agent.into());
        self
    }

    /// Bound for connecting to the BiDi/RDP endpoints. Default 10 s.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.config.timeouts.connect = timeout;
        self
    }

    /// Bound for a launched Firefox to get its ports ready. Default 10 s.
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.config.timeouts.startup = timeout;
        self
    }

    /// Default timeout of every BiDi/RDP command. Default 10 s.
    pub fn command_timeout(mut self, timeout: Duration) -> Self {
        self.config.timeouts.command = timeout;
        self
    }

    /// Leaves a managed Firefox running when the `Browser` is dropped
    /// without `stop()`. Default `false`.
    pub fn keep_alive_on_drop(mut self, keep: bool) -> Self {
        self.config.keep_alive_on_drop = keep;
        self
    }

    /// Relaunches a managed Firefox whose process died, on the next client
    /// request. Default `false`.
    pub fn auto_relaunch(mut self, relaunch: bool) -> Self {
        self.config.auto_relaunch = relaunch;
        self
    }

    /// Adds an environment variable for the Firefox process. Its value is
    /// never logged. The same key twice keeps the last value.
    pub fn env_var(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        let key = key.into();
        self.config
            .env_extra
            .retain(|(existing, _)| *existing != key);
        self.config.env_extra.push((key, value.into()));
        self
    }

    /// Adds several environment variables, in order.
    pub fn env_vars<I, K, V>(self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        vars.into_iter()
            .fold(self, |builder, (key, value)| builder.env_var(key, value))
    }

    /// Finishes the configuration. Infallible: rules are checked by
    /// [`BrowserConfig::validate`].
    pub fn build(self) -> BrowserConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch_new() -> BrowserConfigBuilder {
        BrowserConfig::builder().mode(LaunchMode::LaunchNew)
    }

    fn proxy_err(url: &str) -> Option<ConfigError> {
        launch_new().proxy(url).build().validate().err()
    }

    /// CFG-01: Builder defaults match cdp-browser-lite's, with RDP off.
    #[test]
    fn builder_defaults_match_cdp_browser_lite() {
        // When the default config is built
        let c = BrowserConfig::builder().build();
        // Then every default holds
        assert_eq!(c.mode(), LaunchMode::Auto);
        assert_eq!((c.host(), c.port(), c.rdp_port()), ("127.0.0.1", 0, None));
        assert!(!c.headless() && !c.keep_alive_on_drop && !c.auto_relaunch);
        assert_eq!(c.profile(), &ProfileMode::Ephemeral);
        let ten = Duration::from_secs(10);
        assert_eq!(
            c.timeouts,
            Timeouts {
                connect: ten,
                startup: ten,
                command: ten
            }
        );
        assert!(c.extra_args.is_empty() && c.extra_prefs.is_empty() && c.env_extra.is_empty());
        assert!(c.proxy.is_none() && c.firefox_path.is_none() && c.window_size.is_none());
    }

    /// CFG-02: AttachOnly requires an explicit BiDi port.
    #[test]
    fn attach_only_requires_port() {
        let c = BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .build();
        assert_eq!(
            c.validate(),
            Err(ConfigError::PortRequired {
                mode: LaunchMode::AttachOnly
            })
        );
    }

    /// CFG-03: Auto requires an explicit BiDi port.
    #[test]
    fn auto_requires_port() {
        let c = BrowserConfig::builder().build();
        assert_eq!(
            c.validate(),
            Err(ConfigError::PortRequired {
                mode: LaunchMode::Auto
            })
        );
    }

    /// CFG-04: Launching modes refuse a non-local host.
    #[test]
    fn launching_modes_refuse_remote_host() {
        for mode in [LaunchMode::Auto, LaunchMode::LaunchNew] {
            // Given a launching mode and a remote host
            let c = BrowserConfig::builder()
                .mode(mode)
                .host("10.0.0.5")
                .port(9300)
                .build();
            // Then validate refuses it
            assert!(
                matches!(c.validate(), Err(ConfigError::RemoteHostRequiresAttachOnly { ref host }) if host == "10.0.0.5"),
                "{mode:?}"
            );
        }
    }

    /// CFG-05: Local hosts are recognised case-insensitively and trimmed.
    #[test]
    fn local_hosts_are_recognised() {
        for host in ["127.0.0.1", "::1", "[::1]", "localhost", " LocalHost "] {
            assert!(is_local_host(host), "{host}");
        }
        assert!(!is_local_host("10.0.0.5"));
        assert!(!is_local_host("localhost.evil"));
    }

    /// CFG-06: AttachOnly may target a remote host.
    #[test]
    fn attach_only_may_target_remote_host() {
        let c = BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .host("10.0.0.5")
            .port(9222)
            .build();
        assert_eq!(c.validate(), Ok(()));
    }

    /// CFG-07: UserDefault is refused for launching modes and accepted for AttachOnly.
    #[test]
    fn user_default_only_with_attach_only() {
        let with = |mode| {
            BrowserConfig::builder()
                .mode(mode)
                .port(9300)
                .profile(ProfileMode::UserDefault)
                .build()
                .validate()
        };
        let refused = Err(ConfigError::UserDefaultProfileUnsupported);
        assert_eq!(with(LaunchMode::Auto), refused);
        assert_eq!(with(LaunchMode::LaunchNew), refused);
        assert_eq!(with(LaunchMode::AttachOnly), Ok(()));
    }

    /// CFG-08: RDP and BiDi cannot share a fixed port.
    #[test]
    fn rdp_and_bidi_cannot_share_port() {
        let c = launch_new().port(9300).rdp_port(9300).build();
        assert_eq!(c.validate(), Err(ConfigError::PortClash { port: 9300 }));
    }

    /// CFG-09: An http proxy URL is parsed into scheme, host and port.
    #[test]
    fn http_proxy_is_parsed() {
        let c = launch_new().proxy("http://127.0.0.1:3128").build();
        assert_eq!(
            c.proxy_spec(),
            Ok(Some(ProxySpec::Http {
                host: "127.0.0.1".into(),
                port: 3128
            }))
        );
    }

    /// CFG-10: A socks5 proxy URL is parsed.
    #[test]
    fn socks5_proxy_is_parsed() {
        let c = launch_new().proxy("SOCKS5://proxy.local:1080/").build();
        assert_eq!(
            c.proxy_spec(),
            Ok(Some(ProxySpec::Socks5 {
                host: "proxy.local".into(),
                port: 1080
            }))
        );
    }

    /// CFG-11: A proxy URL with credentials is refused without echoing them.
    #[test]
    fn proxy_credentials_are_refused_without_echo() {
        // Given a proxy URL with credentials
        let err = proxy_err("http://user:s3cret@127.0.0.1:3128");
        // Then it is refused
        assert_eq!(err, Some(ConfigError::ProxyCredentialsUnsupported));
        // And neither Display nor Debug echoes them
        let shown = format!(
            "{} {:?}",
            err.clone().map(|e| e.to_string()).unwrap_or_default(),
            err
        );
        assert!(
            !shown.contains("s3cret") && !shown.contains("user:"),
            "{shown}"
        );
    }

    /// CFG-12: Malformed proxies are refused with a reason.
    #[test]
    fn malformed_proxies_are_refused() {
        for url in [
            "ftp://h:1",
            "http://h",
            "http://h:99999",
            "",
            "http://:80",
            "http://h:0",
        ] {
            assert!(
                matches!(proxy_err(url), Some(ConfigError::InvalidProxy { .. })),
                "{url:?}"
            );
        }
    }

    /// CFG-13: arg/args keep insertion order.
    #[test]
    fn args_keep_insertion_order() {
        let c = BrowserConfig::builder()
            .arg("-a")
            .args(["-b", "-c"])
            .arg("-d")
            .build();
        assert_eq!(c.extra_args, ["-a", "-b", "-c", "-d"]);
    }

    /// CFG-14: pref with the same name twice keeps the last value.
    #[test]
    fn duplicate_pref_keeps_last_value() {
        let c = BrowserConfig::builder()
            .pref("a.b", true)
            .prefs([("c.d", 1)])
            .pref("a.b", "x")
            .build();
        assert_eq!(
            c.extra_prefs,
            [
                ("c.d".to_owned(), PrefValue::Int(1)),
                ("a.b".to_owned(), PrefValue::from("x"))
            ]
        );
    }

    /// CFG-15: env_var with the same key twice keeps the last value.
    #[test]
    fn duplicate_env_var_keeps_last_value() {
        let c = BrowserConfig::builder()
            .env_var("K", "1")
            .env_vars([("J", "2"), ("K", "3")])
            .build();
        let expected: Vec<(OsString, OsString)> =
            vec![("J".into(), "2".into()), ("K".into(), "3".into())];
        assert_eq!(c.env_extra, expected);
    }

    /// CFG-16: A zero window dimension is refused with InvalidWindowSize.
    #[test]
    fn zero_window_dimension_is_refused() {
        for (w, h) in [(0, 600), (800, 0)] {
            let c = launch_new().window_size(w, h).build();
            assert_eq!(c.validate(), Err(ConfigError::InvalidWindowSize));
        }
        assert_eq!(launch_new().window_size(1, 1).build().validate(), Ok(()));
    }

    /// CFG-17: A zero timeout is refused with ZeroTimeout naming which one.
    #[test]
    fn zero_timeout_is_refused_by_name() {
        let zero = Duration::ZERO;
        let cases = [
            (launch_new().connect_timeout(zero), "connect_timeout"),
            (launch_new().startup_timeout(zero), "startup_timeout"),
            (launch_new().command_timeout(zero), "command_timeout"),
        ];
        for (builder, which) in cases {
            assert_eq!(
                builder.build().validate(),
                Err(ConfigError::ZeroTimeout { which })
            );
        }
    }

    /// CFG-18: rdp(true) allocates, rdp_port(p) fixes, rdp(false) disables.
    #[test]
    fn rdp_setters() {
        assert_eq!(launch_new().rdp(true).build().rdp_port(), Some(0));
        assert_eq!(launch_new().rdp_port(6100).build().rdp_port(), Some(6100));
        assert_eq!(launch_new().rdp(true).rdp(false).build().rdp_port(), None);
    }

    /// CFG-19: Debug of BrowserConfig never shows env values, pref values or the proxy.
    #[test]
    fn debug_redacts_secrets() {
        // Given secrets in env, prefs and proxy
        let c = launch_new()
            .env_var("TOKEN", "abc123")
            .pref("x.secret", "pw")
            .proxy("http://127.0.0.1:1")
            .build();
        // When debug-formatted
        let shown = format!("{c:?}");
        // Then names appear, values do not
        assert!(
            shown.contains("TOKEN") && shown.contains("x.secret"),
            "{shown}"
        );
        for secret in ["abc123", "\"pw\"", "127.0.0.1:1"] {
            assert!(!shown.contains(secret), "{secret} leaked: {shown}");
        }
    }

    /// CFG-20: PersistentPerPort derives root/<prefix><port>.
    #[test]
    fn persistent_per_port_dir() {
        let mode = ProfileMode::PersistentPerPort {
            root: PathBuf::from("/r"),
            prefix: "ff-".into(),
        };
        assert_eq!(
            mode.dir_for_port(9300),
            Some(PathBuf::from("/r").join("ff-9300"))
        );
        assert_eq!(ProfileMode::Ephemeral.dir_for_port(9300), None);
        assert_eq!(ProfileMode::Persistent("/p".into()).dir_for_port(1), None);
    }

    /// CFG-21: AttachOnly with RDP needs an explicit RDP port.
    #[test]
    fn attach_only_with_rdp_needs_rdp_port() {
        let base = BrowserConfig::builder()
            .mode(LaunchMode::AttachOnly)
            .port(9222);
        assert_eq!(
            base.clone().rdp(true).build().validate(),
            Err(ConfigError::RdpPortRequired)
        );
        assert_eq!(base.rdp_port(6000).build().validate(), Ok(()));
    }
}
