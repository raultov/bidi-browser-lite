//! Three managed Firefox instances side by side in a `BrowserPool`.

use bidi_browser_lite::{BrowserConfig, BrowserPool, LaunchMode};
use tracing_subscriber::EnvFilter;

const INSTANCES: usize = 3;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let headless = std::env::var_os("BIDI_BROWSER_LITE_HEADLESS").is_some();
    let pool = BrowserPool::new();
    let result = run(&pool, headless).await;
    pool.close_all().await?;
    result?;
    println!("Pool closed: {INSTANCES} browsers");
    Ok(())
}

async fn run(pool: &BrowserPool, headless: bool) -> Result<(), Box<dyn std::error::Error>> {
    let opens = (0..INSTANCES).map(|_| {
        let config = BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .headless(headless)
            .build();
        pool.open(config)
    });
    for id in futures_util::future::join_all(opens).await {
        let id = id?;
        if let Some(browser) = pool.get(id).await {
            browser.new_tab("https://www.rust-lang.org").await?;
        }
    }
    for entry in pool.entries().await {
        println!(
            "{}: BiDi port {}, profile {:?}",
            entry.id, entry.port, entry.profile_dir
        );
    }
    Ok(())
}
