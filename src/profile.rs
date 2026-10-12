//! Profile directories, the managed `user.js` block, Firefox's port file and
//! profile locks.
//!
//! A profile is prepared only once the ports are final, so an `Auto` decision
//! that ends in attaching never creates a directory. Facts this module relies
//! on (Firefox 157, Linux): the BiDi port chosen for port 0 is written to
//! `WebDriverBiDiServer.json`; a running Firefox holds a `lock` symlink to
//! `IP:+PID`, left behind (stale) after `browser.close` or a kill and ignored
//! by the next Firefox; a second Firefox on a live profile silently hands the
//! launch to the running one. Snap Firefox only sees profiles under `$HOME`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::debug;

use crate::config::{PrefValue, ProfileMode};
use crate::error::ProfileError;

/// Prefix of every ephemeral profile directory, used by the orphan sweep.
pub const EPHEMERAL_PREFIX: &str = "bidi-browser-lite-";

/// Ephemeral profiles older than this whose Firefox is gone are swept on the
/// next ephemeral launch (one hour, as in cdp-browser-lite's code).
pub const EPHEMERAL_SWEEP_AGE: Duration = Duration::from_secs(60 * 60);

/// Firefox's BiDi port file, written when launched with port 0.
pub const PORT_FILE: &str = "WebDriverBiDiServer.json";

const USER_JS: &str = "user.js";
const BLOCK_BEGIN: &str = "// BEGIN bidi-browser-lite (managed block, rewritten on every launch)";
const BLOCK_END: &str = "// END bidi-browser-lite";

/// A prepared profile directory. Dropping it removes an ephemeral directory
/// unless it was told to keep it (a browser kept alive on drop still uses it).
#[derive(Debug)]
pub struct Profile {
    mode: ProfileMode,
    dir: Option<PathBuf>,
    keep: bool,
}

/// What [`Profile::prepare`] needs to know.
#[derive(Debug)]
pub(crate) struct ProfileSpec<'a> {
    /// The configured mode.
    pub(crate) mode: &'a ProfileMode,
    /// The final BiDi port (used by `PersistentPerPort`).
    pub(crate) bidi_port: u16,
    /// Where ephemeral profiles are created (see [`ephemeral_root`]).
    pub(crate) ephemeral_root: &'a Path,
    /// The prefs of the managed `user.js` block, in order.
    pub(crate) prefs: &'a [(String, PrefValue)],
}

/// Who holds a profile, according to its lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockState {
    /// No lock.
    Free,
    /// Locked by a running process.
    Live(u32),
    /// Locked by a process that is gone.
    Stale(u32),
    /// Locked, but liveness cannot be checked (Windows, unreadable lock).
    Unknown,
}

impl Profile {
    /// Creates or patches the profile directory: a new ephemeral directory or
    /// the persistent one, the managed `user.js` block, and no stale port
    /// file. Fails with [`ProfileError::Locked`] when a running Firefox owns
    /// a persistent profile.
    pub(crate) fn prepare(spec: &ProfileSpec<'_>) -> Result<Self, ProfileError> {
        let dir = match spec.mode {
            ProfileMode::Ephemeral => create_ephemeral(spec.ephemeral_root)?,
            ProfileMode::Persistent(dir) => open_persistent(dir)?,
            ProfileMode::PersistentPerPort { root, prefix } => {
                open_persistent(&root.join(format!("{prefix}{}", spec.bidi_port)))?
            }
            _ => {
                return Ok(Self {
                    mode: spec.mode.clone(),
                    dir: None,
                    keep: true,
                });
            }
        };
        let profile = Self {
            mode: spec.mode.clone(),
            dir: Some(dir.clone()),
            keep: false,
        };
        write_user_js(&dir, spec.prefs)?;
        remove_port_file(&dir);
        Ok(profile)
    }

    /// The profile directory (`None` for `UserDefault`).
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The mode the profile was prepared for.
    pub fn mode(&self) -> &ProfileMode {
        &self.mode
    }

    /// Keeps an ephemeral directory on drop, for a browser kept alive.
    pub(crate) fn keep_on_drop(&mut self) {
        self.keep = true;
    }

    /// The BiDi port Firefox wrote for a port-0 launch, if it did yet.
    pub(crate) fn read_port_file(&self) -> Result<Option<u16>, ProfileError> {
        match &self.dir {
            Some(dir) => read_port_file(dir),
            None => Ok(None),
        }
    }

    /// Removes an ephemeral directory; never touches a persistent one.
    /// Idempotent.
    pub(crate) fn cleanup(&mut self) {
        if self.mode != ProfileMode::Ephemeral {
            return;
        }
        if let Some(dir) = self.dir.take()
            && let Err(err) = std::fs::remove_dir_all(&dir)
        {
            debug!(
                "could not remove ephemeral profile {}: {err}",
                dir.display()
            );
        }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        if !self.keep {
            self.cleanup();
        }
    }
}

/// Where ephemeral profiles live: under `$HOME/snap/firefox/common/` for a
/// snap Firefox (its sandbox cannot see the host temp dir), the system temp
/// dir otherwise.
pub(crate) fn ephemeral_root(snap: bool, home: Option<&Path>) -> Result<PathBuf, ProfileError> {
    if !snap {
        return Ok(std::env::temp_dir());
    }
    let home = home.ok_or(ProfileError::SnapHomeUnavailable)?;
    Ok(home.join("snap/firefox/common/bidi-browser-lite/profiles"))
}

fn create_ephemeral(root: &Path) -> Result<PathBuf, ProfileError> {
    let create = |source| ProfileError::CreateDir {
        path: root.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(root).map_err(create)?;
    sweep_orphans(root, EPHEMERAL_SWEEP_AGE);
    let dir = tempfile::Builder::new()
        .prefix(EPHEMERAL_PREFIX)
        .tempdir_in(root)
        .map_err(create)?;
    Ok(dir.keep())
}

fn open_persistent(dir: &Path) -> Result<PathBuf, ProfileError> {
    std::fs::create_dir_all(dir).map_err(|source| ProfileError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })?;
    match lock_state(dir) {
        LockState::Live(_) => Err(ProfileError::Locked {
            path: dir.to_path_buf(),
        }),
        _ => Ok(dir.to_path_buf()),
    }
}

fn write_user_js(dir: &Path, prefs: &[(String, PrefValue)]) -> Result<(), ProfileError> {
    let path = dir.join(USER_JS);
    let failed = |source| ProfileError::UserJs {
        path: path.clone(),
        source,
    };
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(failed(err)),
    };
    let names: Vec<&str> = prefs.iter().map(|(name, _)| name.as_str()).collect();
    debug!(
        "writing {} prefs to {}: {names:?}",
        prefs.len(),
        path.display()
    );
    std::fs::write(&path, merge_user_js(&existing, &render_block(prefs))).map_err(failed)
}

/// The managed block for `prefs`.
fn render_block(prefs: &[(String, PrefValue)]) -> String {
    let mut block = format!("{BLOCK_BEGIN}\n");
    for (name, value) in prefs {
        block.push_str(&format!(
            "user_pref({}, {});\n",
            js_string(name),
            js_value(value)
        ));
    }
    block.push_str(BLOCK_END);
    block.push('\n');
    block
}

/// Replaces the managed block in `existing`, or appends it; text outside the
/// block is preserved byte for byte.
fn merge_user_js(existing: &str, block: &str) -> String {
    let span = existing.find(BLOCK_BEGIN).and_then(|start| {
        let end = existing[start..].find(BLOCK_END)? + start + BLOCK_END.len();
        let end = existing[end..].strip_prefix('\n').map_or(end, |_| end + 1);
        Some((start, end))
    });
    match span {
        Some((start, end)) => format!("{}{block}{}", &existing[..start], &existing[end..]),
        None if existing.is_empty() || existing.ends_with('\n') => format!("{existing}{block}"),
        None => format!("{existing}\n{block}"),
    }
}

fn js_value(value: &PrefValue) -> String {
    match value {
        PrefValue::Bool(b) => b.to_string(),
        PrefValue::Int(i) => i.to_string(),
        PrefValue::String(s) => js_string(s),
    }
}

/// A JavaScript string literal for `user.js`.
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn remove_port_file(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(PORT_FILE));
}

fn read_port_file(dir: &Path) -> Result<Option<u16>, ProfileError> {
    let path = dir.join(PORT_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    parse_port_file(&text)
        .map(Some)
        .ok_or(ProfileError::PortFile {
            path,
            reason: "expected {\"ws_port\": <1-65535>}",
        })
}

fn parse_port_file(text: &str) -> Option<u16> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let port = u16::try_from(json.get("ws_port")?.as_u64()?).ok()?;
    (port != 0).then_some(port)
}

/// The PID in a Firefox lock symlink target such as `127.0.1.1:+166537`.
fn parse_lock_target(target: &str) -> Option<u32> {
    let pid: u32 = target.rsplit_once(":+")?.1.parse().ok()?;
    (pid != 0).then_some(pid)
}

/// Who holds `dir`, from its `lock` symlink (Unix).
#[cfg(unix)]
pub(crate) fn lock_state(dir: &Path) -> LockState {
    let Ok(target) = std::fs::read_link(dir.join("lock")) else {
        return LockState::Free;
    };
    match parse_lock_target(&target.to_string_lossy()) {
        Some(pid) if pid_alive(pid) => LockState::Live(pid),
        Some(pid) => LockState::Stale(pid),
        None => LockState::Unknown,
    }
}

/// Who holds `dir`: on Windows liveness is not checked, so a `parent.lock`
/// means "unknown", which is never deleted.
#[cfg(not(unix))]
pub(crate) fn lock_state(dir: &Path) -> LockState {
    match dir.join("parent.lock").exists() {
        true => LockState::Unknown,
        false => LockState::Free,
    }
}

/// Whether `pid` names a running process (signal 0).
#[cfg(unix)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    matches!(kill(Pid::from_raw(raw), None), Ok(()) | Err(Errno::EPERM))
}

/// Whether an ephemeral directory may be swept: never while its Firefox may
/// still run.
fn sweepable(name: &str, age: Duration, max_age: Duration, lock: LockState) -> bool {
    name.starts_with(EPHEMERAL_PREFIX)
        && age >= max_age
        && matches!(lock, LockState::Free | LockState::Stale(_))
}

/// Deletes orphaned ephemeral profiles under `root` (Unix only: liveness of
/// the owning Firefox cannot be checked portably). Errors are ignored.
#[cfg(unix)]
fn sweep_orphans(root: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let age = entry
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok());
        let name = entry.file_name();
        if let Some(age) = age
            && sweepable(&name.to_string_lossy(), age, max_age, lock_state(&path))
        {
            debug!("sweeping orphaned profile {}", path.display());
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(not(unix))]
fn sweep_orphans(_root: &Path, _max_age: Duration) {}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK_PREF: &str = "browser.shell.checkDefaultBrowser";

    fn prefs() -> Vec<(String, PrefValue)> {
        vec![(BLOCK_PREF.to_owned(), PrefValue::Bool(false))]
    }

    fn prepare(mode: &ProfileMode, root: &Path) -> Profile {
        let prefs = prefs();
        let spec = ProfileSpec {
            mode,
            bidi_port: 9300,
            ephemeral_root: root,
            prefs: &prefs,
        };
        Profile::prepare(&spec).unwrap()
    }

    fn user_js(profile: &Profile) -> String {
        std::fs::read_to_string(profile.dir().unwrap().join(USER_JS)).unwrap()
    }

    /// PRF-01: Ephemeral profiles live in the temp dir for non-snap Firefox.
    #[test]
    fn non_snap_root_is_temp_dir() {
        assert_eq!(
            ephemeral_root(false, Some(Path::new("/h"))).unwrap(),
            std::env::temp_dir()
        );
    }

    /// PRF-02: Ephemeral profiles live under $HOME/snap/firefox/common/bidi-browser-lite/profiles for snap Firefox.
    #[test]
    fn snap_root_is_under_home() {
        let root = ephemeral_root(true, Some(Path::new("/home/u"))).unwrap();
        assert_eq!(
            root,
            Path::new("/home/u/snap/firefox/common/bidi-browser-lite/profiles")
        );
    }

    /// PRF-03: Snap root without HOME fails with ProfileError::SnapHomeUnavailable.
    #[test]
    fn snap_root_without_home_fails() {
        assert!(matches!(
            ephemeral_root(true, None),
            Err(ProfileError::SnapHomeUnavailable)
        ));
    }

    /// PRF-04: An ephemeral profile's user.js contains only the managed block.
    #[test]
    fn ephemeral_user_js_is_only_the_block() {
        let root = tempfile::tempdir().unwrap();
        let profile = prepare(&ProfileMode::Ephemeral, root.path());
        let name = profile
            .dir()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(EPHEMERAL_PREFIX), "{name}");
        assert_eq!(user_js(&profile), render_block(&prefs()));
        assert!(user_js(&profile).contains(&format!("user_pref(\"{BLOCK_PREF}\", false);")));
    }

    /// PRF-07: A persistent user.js keeps user text outside the block byte for byte and replaces the block.
    #[test]
    fn persistent_user_js_preserves_user_text() {
        // Given a persistent profile with user prefs around an old block
        let dir = tempfile::tempdir().unwrap();
        let before = "user_pref(\"a\", 1);\r\n// note\n";
        let after = "user_pref(\"z\", \"keep\");";
        let old = format!("{before}{BLOCK_BEGIN}\nuser_pref(\"old\", true);\n{BLOCK_END}\n{after}");
        std::fs::write(dir.path().join(USER_JS), old).unwrap();
        // When it is prepared twice
        let mode = ProfileMode::Persistent(dir.path().to_path_buf());
        drop(prepare(&mode, dir.path()));
        let profile = prepare(&mode, dir.path());
        // Then the user text is intact and only the new block is there
        assert_eq!(
            user_js(&profile),
            format!("{before}{}{after}", render_block(&prefs()))
        );
        assert!(dir.path().exists(), "persistent dirs survive drop");
    }

    /// PRF-08: A stale port file is deleted before launch.
    #[test]
    fn stale_port_file_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PORT_FILE), r#"{"ws_port": 1}"#).unwrap();
        let profile = prepare(
            &ProfileMode::Persistent(dir.path().to_path_buf()),
            dir.path(),
        );
        assert_eq!(profile.read_port_file().unwrap(), None);
    }

    /// PRF-09: The port file is parsed into the BiDi port; malformed content is a PortFile error.
    #[test]
    fn port_file_is_parsed() {
        // Given the captured Firefox port file
        assert_eq!(
            parse_port_file(crate::test_support::fixtures_port_file()),
            Some(40375)
        );
        // And malformed ones
        for bad in [
            "",
            "{",
            r#"{"ws_port": 0}"#,
            r#"{"ws_port": 70000}"#,
            r#"{"port": 1}"#,
        ] {
            assert_eq!(parse_port_file(bad), None, "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PORT_FILE), "{").unwrap();
        assert!(matches!(
            read_port_file(dir.path()),
            Err(ProfileError::PortFile { .. })
        ));
    }

    /// PRF-10: A lock symlink pointing at a live PID is reported Live.
    #[cfg(unix)]
    #[test]
    fn live_lock_is_reported_live() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        std::os::unix::fs::symlink(format!("127.0.1.1:+{pid}"), dir.path().join("lock")).unwrap();
        assert_eq!(lock_state(dir.path()), LockState::Live(pid));
    }

    /// PRF-11: A dead-PID lock is reported Stale and does not block prepare.
    #[cfg(unix)]
    #[test]
    fn stale_lock_does_not_block_prepare() {
        let dir = tempfile::tempdir().unwrap();
        let dead = dead_pid();
        std::os::unix::fs::symlink(format!("127.0.1.1:+{dead}"), dir.path().join("lock")).unwrap();
        assert_eq!(lock_state(dir.path()), LockState::Stale(dead));
        let profile = prepare(
            &ProfileMode::Persistent(dir.path().to_path_buf()),
            dir.path(),
        );
        assert!(profile.dir().is_some());
        assert!(
            dir.path().join("lock").symlink_metadata().is_ok(),
            "stale locks are left to Firefox"
        );
    }

    /// PRF-12: prepare on a persistent profile locked by a live process fails with ProfileError::Locked.
    #[cfg(unix)]
    #[test]
    fn live_locked_profile_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        std::os::unix::fs::symlink(format!("127.0.1.1:+{pid}"), dir.path().join("lock")).unwrap();
        let prefs = prefs();
        let mode = ProfileMode::Persistent(dir.path().to_path_buf());
        let spec = ProfileSpec {
            mode: &mode,
            bidi_port: 1,
            ephemeral_root: dir.path(),
            prefs: &prefs,
        };
        assert!(matches!(
            Profile::prepare(&spec),
            Err(ProfileError::Locked { .. })
        ));
    }

    /// PRF-13: A lock whose liveness is Unknown is never swept.
    #[test]
    fn unknown_lock_is_never_swept() {
        let old = Duration::from_secs(10 * 3600);
        let name = format!("{EPHEMERAL_PREFIX}x");
        assert!(!sweepable(
            &name,
            old,
            EPHEMERAL_SWEEP_AGE,
            LockState::Unknown
        ));
        assert!(!sweepable(
            &name,
            old,
            EPHEMERAL_SWEEP_AGE,
            LockState::Live(1)
        ));
        assert!(sweepable(
            &name,
            old,
            EPHEMERAL_SWEEP_AGE,
            LockState::Stale(1)
        ));
        assert!(sweepable(&name, old, EPHEMERAL_SWEEP_AGE, LockState::Free));
    }

    /// PRF-14: The sweep deletes only old, prefixed, unlocked directories.
    #[cfg(unix)]
    #[test]
    fn sweep_deletes_only_orphans() {
        // Given a root with an orphan, a live one, a foreign dir and a young one
        let root = tempfile::tempdir().unwrap();
        let mk = |name: &str| {
            let p = root.path().join(name);
            std::fs::create_dir(&p).unwrap();
            p
        };
        let orphan = mk(&format!("{EPHEMERAL_PREFIX}orphan"));
        let live = mk(&format!("{EPHEMERAL_PREFIX}live"));
        let foreign = mk("someone-else");
        std::os::unix::fs::symlink(
            format!("1.1.1.1:+{}", std::process::id()),
            live.join("lock"),
        )
        .unwrap();
        // When swept with age 0 (everything is old), then with a huge age
        sweep_orphans(root.path(), Duration::from_secs(1_000_000));
        assert!(orphan.exists(), "young dirs are kept");
        sweep_orphans(root.path(), Duration::ZERO);
        // Then only the orphan is gone
        assert!(!orphan.exists() && live.exists() && foreign.exists());
    }

    /// PRF-15: cleanup removes an ephemeral dir and is idempotent.
    #[test]
    fn cleanup_removes_ephemeral_dir_once() {
        let root = tempfile::tempdir().unwrap();
        let mut profile = prepare(&ProfileMode::Ephemeral, root.path());
        let dir = profile.dir().unwrap().to_path_buf();
        profile.cleanup();
        profile.cleanup();
        assert!(!dir.exists());
        assert!(profile.dir().is_none());
    }

    /// PRF-16: cleanup never deletes a persistent profile directory.
    #[test]
    fn cleanup_keeps_persistent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut profile = prepare(
            &ProfileMode::Persistent(dir.path().to_path_buf()),
            dir.path(),
        );
        profile.cleanup();
        drop(profile);
        assert!(dir.path().join(USER_JS).exists());
    }

    /// PRF-17: Dropping a prepared ephemeral profile removes its directory unless kept.
    #[test]
    fn drop_removes_ephemeral_dir_unless_kept() {
        let root = tempfile::tempdir().unwrap();
        let profile = prepare(&ProfileMode::Ephemeral, root.path());
        let dir = profile.dir().unwrap().to_path_buf();
        drop(profile);
        assert!(!dir.exists());
        let mut kept = prepare(&ProfileMode::Ephemeral, root.path());
        kept.keep_on_drop();
        let dir = kept.dir().unwrap().to_path_buf();
        drop(kept);
        assert!(dir.exists());
    }

    /// PRF-18: String pref values are escaped as JS string literals.
    #[test]
    fn string_prefs_are_escaped() {
        assert_eq!(js_string("a\"b\\c\nd\r\u{2028}"), r#""a\"b\\c\nd\r\u2028""#);
        assert_eq!(js_value(&PrefValue::Int(-3)), "-3");
        assert_eq!(js_value(&PrefValue::Bool(true)), "true");
    }

    /// PRF-20: PersistentPerPort prepares root/<prefix><port>.
    #[test]
    fn per_port_profile_uses_port_dir() {
        let root = tempfile::tempdir().unwrap();
        let mode = ProfileMode::PersistentPerPort {
            root: root.path().to_path_buf(),
            prefix: "ff-".into(),
        };
        let profile = prepare(&mode, root.path());
        assert_eq!(profile.dir(), Some(root.path().join("ff-9300").as_path()));
    }

    /// PRF-21: Lock targets are parsed after the last ":+".
    #[test]
    fn lock_targets_are_parsed() {
        assert_eq!(parse_lock_target("127.0.1.1:+166537"), Some(166_537));
        assert_eq!(parse_lock_target("fe80::1:+42"), Some(42));
        assert_eq!(parse_lock_target("garbage"), None);
        assert_eq!(parse_lock_target("1.1.1.1:+0"), None);
    }

    /// A PID that is not running: a child that already exited.
    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }
}
