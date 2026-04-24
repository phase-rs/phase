mod feed;
mod scrape;

use std::path::PathBuf;

use clap::Parser;
use reqwest::blocking::Client;

use crate::feed::Feed;
use crate::scrape::{scrape_metagame, ScrapeConfig};

#[derive(Parser)]
#[command(name = "feed-scraper")]
#[command(about = "Scrape MTGGoldfish metagame decks into feed JSON")]
struct Cli {
    /// Comma-separated formats to scrape (e.g., "standard,modern")
    #[arg(short, long, default_value = "standard")]
    format: String,

    /// Output file path (used when scraping a single format)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Output directory (used when scraping multiple formats)
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Number of top decks to scrape per format
    #[arg(long, default_value_t = 10)]
    top: usize,

    /// Delay between requests in milliseconds
    #[arg(long, default_value_t = 1000)]
    delay: u64,

    /// Minimum scraped-deck count required to overwrite the existing feed
    /// file. A partial scrape (e.g., rate-limited after a few decks) that
    /// returns non-zero but < this threshold is treated the same as empty:
    /// the existing file is preserved and the process exits non-zero.
    /// Default of 5 is a safe floor for top-10 Standard/Modern/etc. runs.
    #[arg(long, default_value_t = 5)]
    min_decks: usize,
}

fn main() {
    let cli = Cli::parse();

    let client = Client::builder()
        .user_agent("Mozilla/5.0 (compatible; phase-rs-feed-scraper/0.1)")
        .build()
        .expect("failed to build HTTP client");

    let formats: Vec<&str> = cli.format.split(',').map(|s| s.trim()).collect();

    let mut had_failure = false;

    for format in &formats {
        let config = ScrapeConfig {
            format: (*format).to_string(),
            top_n: cli.top,
            delay_ms: cli.delay,
        };

        eprintln!("Scraping {format} metagame (top {})...", cli.top);
        let decks = scrape_metagame(&client, &config);
        eprintln!("Scraped {} decks for {format}", decks.len());

        if decks.len() < cli.min_decks {
            eprintln!(
                "ERROR: scrape returned {} decks for {format} (minimum {}); refusing to overwrite feed file",
                decks.len(),
                cli.min_decks,
            );
            had_failure = true;
            continue;
        }

        let now = chrono_lite_now();
        let feed = Feed {
            id: format!("mtggoldfish-{format}"),
            name: format!("MTGGoldfish {}", capitalize(format)),
            description: format!("Top metagame decks from MTGGoldfish ({format})"),
            icon: "G".to_string(),
            format: (*format).to_string(),
            version: 1,
            updated: now,
            source: format!("https://www.mtggoldfish.com/metagame/{format}"),
            decks,
        };

        let json = serde_json::to_string_pretty(&feed).expect("failed to serialize feed");

        let output_path = if formats.len() == 1 {
            cli.output
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("mtggoldfish-{format}.json")))
        } else {
            let dir = cli.output_dir.clone().unwrap_or_else(|| PathBuf::from("."));
            dir.join(format!("mtggoldfish-{format}.json"))
        };

        std::fs::write(&output_path, &json).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {e}", output_path.display());
            had_failure = true;
        });
        eprintln!("Wrote {}", output_path.display());
    }

    if had_failure {
        std::process::exit(1);
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

/// Simple ISO 8601 timestamp without pulling in chrono
fn chrono_lite_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards");
    let secs = duration.as_secs();
    // Approximate: good enough for a feed timestamp
    let days = secs / 86400;
    let years = 1970 + days / 365;
    let remaining_days = days % 365;
    let months = remaining_days / 30 + 1;
    let day = remaining_days % 30 + 1;
    format!("{years:04}-{months:02}-{day:02}T00:00:00Z")
}
