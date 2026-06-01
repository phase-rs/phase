mod feed;
mod scrape;

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

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
