//! `Auto` + `PersistentPerPort`: the profile survives `stop()`, the user's own
//! `user.js` lines are preserved, and the next run reuses the directory.
//!
//! Profiles live under `$BIDI_BROWSER_LITE_E2E_HOME`, else
//! `~/snap/firefox/common/bidi-browser-lite` when snap Firefox is installed
//! (the snap cannot see the host temp dir), else the system temp dir.

use std::path::PathBuf;

use bidi_browser_lite::{Browser, BrowserConfig, LaunchMode, ProfileMode};
use tracing_subscriber::EnvFilter;

const PORT: u16 = 9330;
const USER_LINE: &str = "// kept by the user\n";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let root = e2e_home().join("persistent-example");
    let first = launch_and_stop(&root).await?;
    // A line the user adds by hand must survive the next launch.
    let user_js = first.join("user.js");
    let text = std::fs::read_to_string(&user_js)?;
    std::fs::write(&user_js, format!("{USER_LINE}{text}"))?;
    let second = launch_and_stop(&root).await?;
    let kept = std::fs::read_to_string(second.join("user.js"))?.starts_with(USER_LINE);
    if first != second || !kept {
        return Err(
            format!("profile not reused: {first:?} vs {second:?}, user line kept: {kept}").into(),
        );
    }
    println!("Profile reused: {}", second.display());
    Ok(())
}

fn e2e_home() -> PathBuf {
    if let Some(home) = std::env::var_os("BIDI_BROWSER_LITE_E2E_HOME") {
        return PathBuf::from(home);
    }
    let snap = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("snap/firefox/common"));
    match snap.filter(|dir| dir.is_dir()) {
        Some(dir) => dir.join("bidi-browser-lite"),
        None => std::env::temp_dir(),
    }
}

async fn launch_and_stop(root: &std::path::Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let headless = std::env::var_os("BIDI_BROWSER_LITE_HEADLESS").is_some();
    let config = BrowserConfig::builder()
        .mode(LaunchMode::Auto)
        .port(PORT)
        .headless(headless)
        .profile(ProfileMode::PersistentPerPort {
            root: root.to_path_buf(),
            prefix: "ff-".into(),
        })
        .build();
    let browser = Browser::ensure(config).await?;
    let dir = browser
        .profile_dir()
        .await
        .ok_or("a managed browser has a profile")?;
    browser.new_tab("https://www.rust-lang.org").await?;
    browser.stop().await?;
    Ok(dir)
}
