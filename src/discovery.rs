//! Finding the Firefox executable on Linux (including snap), macOS and
//! Windows.
//!
//! `FIREFOX_PATH` wins over everything; a wrong `FIREFOX_PATH` is an error,
//! not a silent fallback. The search itself is a pure function over an
//! injected environment and file-system probe, so the tables of all three
//! operating systems are unit-tested on every CI runner.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::BrowserError;

/// Environment variable that overrides discovery.
pub const FIREFOX_PATH_ENV: &str = "FIREFOX_PATH";

/// Finds the Firefox executable for this OS. `FIREFOX_PATH` wins.
///
/// On Linux the snap binary `/snap/bin/firefox` is preferred over `firefox`
/// in `PATH`: on Ubuntu the latter is a shell wrapper that rewrites desktop
/// settings before starting the snap.
pub fn discover_default() -> Result<PathBuf, BrowserError> {
    discover(Os::current(), &RealEnv, &RealFs)
}

/// Whether `path` is a snap-packaged Firefox, whose sandbox only sees
/// profiles under `$HOME`.
pub fn is_snap_firefox(path: &Path) -> bool {
    is_snap(path, std::fs::canonicalize(path).ok().as_deref())
}

/// Read access to environment variables (injectable for tests).
pub(crate) trait EnvProvider {
    /// The value of `key`, if set.
    fn var(&self, key: &str) -> Option<OsString>;
}

/// File-system checks (injectable for tests).
pub(crate) trait FsProbe {
    /// Whether `path` is a file this process may execute.
    fn is_executable_file(&self, path: &Path) -> bool;
}

/// The operating system whose candidate table is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    fn path_separator(self) -> char {
        match self {
            Self::Windows => ';',
            Self::Linux | Self::Mac => ':',
        }
    }
}

/// One place to look for Firefox.
enum Candidate {
    /// A fixed path.
    Path(PathBuf),
    /// A program name resolved through `PATH`.
    InPath(&'static str),
}

/// Searches `FIREFOX_PATH`, then the OS candidates in order.
pub(crate) fn discover(
    os: Os,
    env: &dyn EnvProvider,
    fs: &dyn FsProbe,
) -> Result<PathBuf, BrowserError> {
    if let Some(custom) = env.var(FIREFOX_PATH_ENV).filter(|v| !v.is_empty()) {
        let custom = PathBuf::from(custom);
        return if fs.is_executable_file(&custom) {
            Ok(custom)
        } else {
            Err(BrowserError::ExecutableNotFound {
                searched: vec![custom],
            })
        };
    }
    let mut searched = Vec::new();
    for candidate in candidates(os, env) {
        let found = match candidate {
            Candidate::Path(path) => {
                let ok = fs.is_executable_file(&path);
                searched.push(path.clone());
                ok.then_some(path)
            }
            Candidate::InPath(name) => {
                searched.push(PathBuf::from(name));
                which(name, os, env, fs)
            }
        };
        if let Some(path) = found {
            return Ok(path);
        }
    }
    Err(BrowserError::ExecutableNotFound { searched })
}

fn candidates(os: Os, env: &dyn EnvProvider) -> Vec<Candidate> {
    match os {
        Os::Linux => linux_candidates(),
        Os::Mac => macos_candidates(env),
        Os::Windows => windows_candidates(env),
    }
}

fn linux_candidates() -> Vec<Candidate> {
    vec![
        Candidate::Path(PathBuf::from("/snap/bin/firefox")),
        Candidate::InPath("firefox"),
        Candidate::Path(PathBuf::from("/usr/lib/firefox/firefox")),
        Candidate::Path(PathBuf::from("/opt/firefox/firefox")),
        Candidate::Path(PathBuf::from("/usr/bin/firefox")),
        Candidate::InPath("firefox-esr"),
    ]
}

const MACOS_BUNDLES: [&str; 3] = [
    "Firefox.app",
    "Firefox Developer Edition.app",
    "Firefox Nightly.app",
];

fn macos_candidates(env: &dyn EnvProvider) -> Vec<Candidate> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = env.var("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
        .iter()
        .flat_map(|root| {
            MACOS_BUNDLES
                .iter()
                .map(move |bundle| root.join(bundle).join("Contents/MacOS/firefox"))
        })
        .map(Candidate::Path)
        .collect()
}

fn windows_candidates(env: &dyn EnvProvider) -> Vec<Candidate> {
    let mut list: Vec<Candidate> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(|key| env.var(key))
        .map(|base| {
            Candidate::Path(
                PathBuf::from(base)
                    .join("Mozilla Firefox")
                    .join("firefox.exe"),
            )
        })
        .collect();
    list.push(Candidate::InPath("firefox"));
    list
}

/// Resolves `name` through `PATH` (and `PATHEXT` on Windows) without
/// spawning a shell.
fn which(name: &str, os: Os, env: &dyn EnvProvider, fs: &dyn FsProbe) -> Option<PathBuf> {
    let path_var = env.var("PATH")?;
    let path_var = path_var.to_string_lossy();
    let names = executable_names(name, os, env);
    path_var
        .split(os.path_separator())
        .filter(|dir| !dir.is_empty())
        .flat_map(|dir| names.iter().map(move |n| Path::new(dir).join(n)))
        .find(|candidate| fs.is_executable_file(candidate))
}

fn executable_names(name: &str, os: Os, env: &dyn EnvProvider) -> Vec<String> {
    let mut names = vec![name.to_owned()];
    if os == Os::Windows {
        let exts = env
            .var("PATHEXT")
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".EXE".to_owned());
        names.extend(
            exts.split(';')
                .filter(|e| !e.is_empty())
                .map(|e| format!("{name}{}", e.to_ascii_lowercase())),
        );
    }
    names
}

/// Snap detection on the path as given and, when known, as resolved: a path
/// under `/snap/`, or a symlink to the `snap` launcher.
fn is_snap(path: &Path, resolved: Option<&Path>) -> bool {
    path.starts_with("/snap/")
        || resolved.is_some_and(|r| r.starts_with("/snap/") || r == Path::new("/usr/bin/snap"))
}

/// The real process environment.
struct RealEnv;

impl EnvProvider for RealEnv {
    fn var(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
}

/// The real file system.
pub(crate) struct RealFs;

impl FsProbe for RealFs {
    fn is_executable_file(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && is_executable(&meta))
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    meta.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct FakeEnv(HashMap<&'static str, OsString>);
    impl FakeEnv {
        fn with(mut self, key: &'static str, value: &str) -> Self {
            self.0.insert(key, value.into());
            self
        }
    }
    impl EnvProvider for FakeEnv {
        fn var(&self, key: &str) -> Option<OsString> {
            self.0.get(key).cloned()
        }
    }

    #[derive(Default)]
    struct FakeFs(HashSet<PathBuf>);
    impl FakeFs {
        fn with(mut self, path: impl Into<PathBuf>) -> Self {
            self.0.insert(path.into());
            self
        }
    }
    impl FsProbe for FakeFs {
        fn is_executable_file(&self, path: &Path) -> bool {
            self.0.contains(path)
        }
    }

    fn found(os: Os, env: &FakeEnv, fs: &FakeFs) -> PathBuf {
        discover(os, env, fs).expect("a candidate exists")
    }

    /// DSC-01: FIREFOX_PATH wins when it points to an executable.
    #[test]
    fn firefox_path_wins() {
        // Given FIREFOX_PATH and a snap install
        let env = FakeEnv::default().with(FIREFOX_PATH_ENV, "/custom/firefox");
        let fs = FakeFs::default()
            .with("/custom/firefox")
            .with("/snap/bin/firefox");
        // Then the override wins
        assert_eq!(
            found(Os::Linux, &env, &fs),
            PathBuf::from("/custom/firefox")
        );
    }

    /// DSC-02: A FIREFOX_PATH that is not executable is an error, not a fallback.
    #[test]
    fn bad_firefox_path_is_an_error() {
        let env = FakeEnv::default().with(FIREFOX_PATH_ENV, "/missing/firefox");
        let fs = FakeFs::default().with("/snap/bin/firefox");
        let result = discover(Os::Linux, &env, &fs);
        assert!(
            matches!(&result, Err(BrowserError::ExecutableNotFound { searched }) if searched == &[PathBuf::from("/missing/firefox")]),
            "{result:?}"
        );
    }

    /// DSC-03: Linux prefers the snap binary over the PATH wrapper.
    #[test]
    fn linux_prefers_snap_over_path_wrapper() {
        let env = FakeEnv::default().with("PATH", "/usr/local/bin:/usr/bin");
        let fs = FakeFs::default()
            .with("/usr/bin/firefox")
            .with("/snap/bin/firefox");
        assert_eq!(
            found(Os::Linux, &env, &fs),
            PathBuf::from("/snap/bin/firefox")
        );
    }

    /// DSC-04: Linux without snap uses firefox from PATH.
    #[test]
    fn linux_without_snap_uses_path() {
        let env = FakeEnv::default().with("PATH", "/usr/local/bin:/usr/bin");
        let fs = FakeFs::default()
            .with("/usr/local/bin/firefox")
            .with("/usr/bin/firefox");
        assert_eq!(
            found(Os::Linux, &env, &fs),
            PathBuf::from("/usr/local/bin/firefox")
        );
    }

    /// DSC-05: Linux falls back to fixed install paths in order.
    #[test]
    fn linux_falls_back_to_fixed_paths() {
        let env = FakeEnv::default().with("PATH", "/nowhere");
        let fs = FakeFs::default()
            .with("/opt/firefox/firefox")
            .with("/usr/bin/firefox");
        assert_eq!(
            found(Os::Linux, &env, &fs),
            PathBuf::from("/opt/firefox/firefox")
        );
    }

    /// DSC-06: macOS finds the release bundle before Developer Edition and Nightly.
    #[test]
    fn macos_prefers_release_bundle() {
        let dev = "/Applications/Firefox Developer Edition.app/Contents/MacOS/firefox";
        let release = "/Applications/Firefox.app/Contents/MacOS/firefox";
        let fs = FakeFs::default().with(dev).with(release);
        assert_eq!(
            found(Os::Mac, &FakeEnv::default(), &fs),
            PathBuf::from(release)
        );
        let only_dev = FakeFs::default().with(dev);
        assert_eq!(
            found(Os::Mac, &FakeEnv::default(), &only_dev),
            PathBuf::from(dev)
        );
    }

    /// DSC-07: macOS finds a bundle under $HOME/Applications.
    #[test]
    fn macos_finds_home_bundle() {
        let env = FakeEnv::default().with("HOME", "/Users/u");
        let path = "/Users/u/Applications/Firefox Nightly.app/Contents/MacOS/firefox";
        let fs = FakeFs::default().with(path);
        assert_eq!(found(Os::Mac, &env, &fs), PathBuf::from(path));
    }

    /// DSC-08: Windows checks ProgramFiles, ProgramFiles(x86), LOCALAPPDATA, then PATH (with PATHEXT).
    #[test]
    fn windows_search_order() {
        // Given the three install roots and a PATH entry
        let env = FakeEnv::default()
            .with("ProgramFiles", r"C:\PF")
            .with("ProgramFiles(x86)", r"C:\PF86")
            .with("LOCALAPPDATA", r"C:\LA")
            .with("PATH", r"C:\bin;D:\tools")
            .with("PATHEXT", ".COM;.EXE");
        let install = |base: &str| {
            PathBuf::from(base)
                .join("Mozilla Firefox")
                .join("firefox.exe")
        };
        // Then ProgramFiles(x86) is used when ProgramFiles has none
        let fs = FakeFs::default()
            .with(install(r"C:\PF86"))
            .with(install(r"C:\LA"));
        assert_eq!(found(Os::Windows, &env, &fs), install(r"C:\PF86"));
        // And PATH with PATHEXT is the last resort
        let in_path = Path::new(r"D:\tools").join("firefox.exe");
        let fs = FakeFs::default().with(in_path.clone());
        assert_eq!(found(Os::Windows, &env, &fs), in_path);
    }

    /// DSC-09: Nothing found lists every searched path and names FIREFOX_PATH.
    #[test]
    fn nothing_found_lists_every_candidate() {
        let result = discover(Os::Linux, &FakeEnv::default(), &FakeFs::default());
        let Err(BrowserError::ExecutableNotFound { searched }) = &result else {
            panic!("expected ExecutableNotFound, got {result:?}");
        };
        let expected: Vec<PathBuf> = [
            "/snap/bin/firefox",
            "firefox",
            "/usr/lib/firefox/firefox",
            "/opt/firefox/firefox",
            "/usr/bin/firefox",
            "firefox-esr",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(searched, &expected);
        assert!(
            result
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
                .contains("FIREFOX_PATH")
        );
    }

    /// DSC-10: Snap detection by path.
    #[test]
    fn snap_detection() {
        assert!(is_snap(Path::new("/snap/bin/firefox"), None));
        assert!(is_snap(
            Path::new("/usr/local/bin/ff"),
            Some(Path::new("/usr/bin/snap"))
        ));
        assert!(!is_snap(Path::new("/usr/lib/firefox/firefox"), None));
        assert!(!is_snap(
            Path::new("/usr/bin/firefox"),
            Some(Path::new("/usr/bin/firefox"))
        ));
    }

    /// DSC-11: The real FsProbe reports a non-executable file as not executable.
    #[cfg(unix)]
    #[test]
    fn real_fs_rejects_non_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("firefox");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!RealFs.is_executable_file(&file));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RealFs.is_executable_file(&file));
    }

    /// DSC-12: The real which finds an executable in a temp PATH dir and ignores directories.
    #[test]
    fn real_which_finds_file_and_ignores_directories() {
        // Given a PATH whose first dir has a *directory* named like the program
        let os = Os::current();
        let shadow = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let file_name = if os == Os::Windows {
            "firefox.exe"
        } else {
            "firefox"
        };
        std::fs::create_dir(shadow.path().join(file_name)).unwrap();
        let real = bin.path().join(file_name);
        std::fs::write(&real, "x").unwrap();
        make_executable(&real);
        let sep = os.path_separator();
        let path_var = format!("{}{sep}{}", shadow.path().display(), bin.path().display());
        let env = FakeEnv::default().with("PATH", &path_var);
        // When resolved
        let found = which("firefox", os, &env, &RealFs);
        // Then the file is found, not the directory
        assert_eq!(found, Some(real));
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}
}
