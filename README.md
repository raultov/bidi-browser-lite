# bidi-browser-lite

Full lifecycle control of WebDriver BiDi browsers (Firefox) on top of
[`bidi-lite`](https://crates.io/crates/bidi-lite): the BiDi twin of
`cdp-browser-lite`. It finds Firefox, launches it (or attaches to a running
one), allocates its ports, prepares its profile, owns its single BiDi session
and stops it cleanly.

**Work in progress.** Not published yet.

```rust,no_run
use bidi_browser_lite::{Browser, BrowserConfig, LaunchMode, ReadinessState};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let browser = Browser::ensure(BrowserConfig::builder().mode(LaunchMode::LaunchNew).build()).await?;
let tab = browser.new_tab("about:blank").await?;
tab.navigate("https://www.rust-lang.org", ReadinessState::Complete).await?;
browser.stop().await?;
# Ok(())
# }
```

## License

MIT, see [LICENSE](LICENSE).
