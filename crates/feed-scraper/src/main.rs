mod feed;
mod scrape;
mod topdeck;

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::{Args, Parser, Subcommand};
use reqwest::blocking::Client;

use crate::feed::Feed;
use crate::scrape::{scrape_metagame, ScrapeConfig};
use crate::topdeck::{fetch_top_decks, TopdeckConfig};

#[derive(Parser)]
#[command(name = "feed-scraper")]
#[command(about = "Build metagame deck feeds as JSON")]
struct Cli {
    #[command(subcommand)]
    source: Source,

    /// Minimum deck count required to overwrite the existing feed file. A
    /// partial scrape (e.g., rate-limited after a few decks) that returns
    /// non-zero but < this threshold is treated the same as empty: the
    /// existing file is preserved and the process exits non-zero.
    #[arg(long, global = true, default_value_t = 5)]
    min_decks: usize,
}

#[derive(Subcommand)]
enum Source {
    /// Scrape MTGGoldfish metagame pages
    Mtggoldfish(MtggoldfishArgs),
    /// Fetch the top cEDH tournament finishes from the TopDeck.gg API
    Topdeck(TopdeckArgs),
}

#[derive(Args)]
struct MtggoldfishArgs {
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
}

#[derive(Args)]
struct TopdeckArgs {
    /// TopDeck.gg API key
    #[arg(long, env = "TOPDECK_API_KEY", hide_env_values = true)]
    api_key: String,

    /// Output file path
    #[arg(short, long, default_value = "topdeck-cedh.json")]
    output: PathBuf,

    /// Number of decks to keep
    #[arg(long, default_value_t = 16)]
    top: usize,

    /// Days back from today to search for completed events
    #[arg(long, default_value_t = 30)]
    days: u32,

    /// Minimum event size
    #[arg(long, default_value_t = 64)]
    min_players: u32,
}

fn main() {
    let cli = Cli::parse();

    let client = Client::builder()
        .user_agent("Mozilla/5.0 (compatible; phase-rs-feed-scraper/0.1)")
        .build()
        .expect("failed to build HTTP client");

    let ok = match &cli.source {
        Source::Mtggoldfish(args) => run_mtggoldfish(&client, args, cli.min_decks),
        Source::Topdeck(args) => run_topdeck(&client, args, cli.min_decks),
    };

    if !ok {
        std::process::exit(1);
    }
}

fn run_mtggoldfish(client: &Client, args: &MtggoldfishArgs, min_decks: usize) -> bool {
    let formats: Vec<&str> = args.format.split(',').map(|s| s.trim()).collect();

    let mut ok = true;
    for format in &formats {
        let config = ScrapeConfig {
            format: (*format).to_string(),
            top_n: args.top,
            delay_ms: args.delay,
        };

        eprintln!("Scraping {format} metagame (top {})...", args.top);
        let decks = scrape_metagame(client, &config);
        eprintln!("Scraped {} decks for {format}", decks.len());

        let feed = Feed {
            id: format!("mtggoldfish-{format}"),
            name: format!("MTGGoldfish {}", capitalize(format)),
            description: format!("Top metagame decks from MTGGoldfish ({format})"),
            icon: "G".to_string(),
            format: (*format).to_string(),
            version: 1,
            updated: chrono_lite_now(),
            source: format!("https://www.mtggoldfish.com/metagame/{format}"),
            decks,
        };

        let output_path = if formats.len() == 1 {
            args.output
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("mtggoldfish-{format}.json")))
        } else {
            let dir = args
                .output_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            dir.join(format!("mtggoldfish-{format}.json"))
        };

        ok &= write_feed(&feed, &output_path, min_decks);
    }
    ok
}

fn run_topdeck(client: &Client, args: &TopdeckArgs, min_decks: usize) -> bool {
    let config = TopdeckConfig {
        api_key: args.api_key.clone(),
        days: args.days,
        min_players: args.min_players,
        top_n: args.top,
    };

    eprintln!(
        "Fetching top {} cEDH finishes from TopDeck.gg (last {} days, {}+ players)...",
        config.top_n, config.days, config.min_players,
    );
    let decks = match fetch_top_decks(client, &config) {
        Ok(decks) => decks,
        Err(e) => {
            eprintln!("ERROR: TopDeck.gg request failed: {e}");
            return false;
        }
    };

    let feed = Feed {
        id: "topdeck-cedh".to_string(),
        name: "TopDeck.gg cEDH".to_string(),
        description: format!(
            "Top finishes from cEDH tournaments with {}+ players in the last {} days",
            config.min_players, config.days,
        ),
        icon: "T".to_string(),
        format: "commander".to_string(),
        version: 1,
        updated: chrono_lite_now(),
        source: "https://topdeck.gg".to_string(),
        decks,
    };

    write_feed(&feed, &args.output, min_decks)
}

/// Writes `feed` to `path` unless it holds fewer than `min_decks` decks, so a
/// failed or partial fetch never replaces a good feed. Returns whether the
/// file was written.
fn write_feed(feed: &Feed, path: &Path, min_decks: usize) -> bool {
    if feed.decks.len() < min_decks {
        eprintln!(
            "ERROR: {} has {} decks (minimum {min_decks}); refusing to overwrite {}",
            feed.id,
            feed.decks.len(),
            path.display(),
        );
        return false;
    }

    let json = serde_json::to_string_pretty(feed).expect("failed to serialize feed");
    match std::fs::write(path, json) {
        Ok(()) => {
            eprintln!("Wrote {}", path.display());
            true
        }
        Err(e) => {
            eprintln!("Failed to write {}: {e}", path.display());
            false
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

/// Simple ISO 8601 timestamp (UTC date) without pulling in chrono.
fn chrono_lite_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}T00:00:00Z")
}

/// Convert days since 1970-01-01 to a `(year, month, day)` Gregorian date.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm, which is leap-year
/// correct. The previous `days / 30` approximation drifted every year and
/// produced invalid months (e.g. `2025-13-..`) for the last days of a 365-day
/// cycle.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let year = yoe as i64 + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn converts_known_days_to_dates() {
        // days since 1970-01-01
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29)); // leap day
        assert_eq!(civil_from_days(11_016), (2000, 2, 29)); // century leap year
        assert_eq!(civil_from_days(20_605), (2026, 6, 1));
    }

    #[test]
    fn never_produces_invalid_month_or_day() {
        // The previous `days / 30` formula produced months > 12 near year end.
        for days in 0i64..(365 * 80) {
            let (_y, m, d) = civil_from_days(days);
            assert!((1..=12).contains(&m), "invalid month {m} for day {days}");
            assert!((1..=31).contains(&d), "invalid day {d} for day {days}");
        }
    }
}
