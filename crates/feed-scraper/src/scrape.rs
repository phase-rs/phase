use std::thread;
use std::time::Duration;

use reqwest::blocking::Client;
use scraper::{Html, Selector};

use crate::feed::{DeckEntry, FeedDeck};

pub struct ScrapeConfig {
    pub format: String,
    pub top_n: usize,
    pub delay_ms: u64,
}

pub fn scrape_metagame(client: &Client, config: &ScrapeConfig) -> Vec<FeedDeck> {
    let url = format!("https://www.mtggoldfish.com/metagame/{}", config.format);

    eprintln!("Fetching metagame page: {url}");
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to fetch metagame page: {e}");
            return Vec::new();
        }
    };

    let body = match resp.text() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Failed to read response: {e}");
            return Vec::new();
        }
    };

    let document = Html::parse_document(&body);

    // MTGGoldfish puts deck names in .archetype-tile-title > span > a.
    // We select the title divs, then find the first link inside each one.
    let title_sel = Selector::parse(".archetype-tile-title").expect("valid CSS selector");
    let link_sel = Selector::parse("a").expect("valid CSS selector");

    let mut deck_urls: Vec<(String, String)> = Vec::new();
    for title_el in document.select(&title_sel) {
        for link in title_el.select(&link_sel) {
            if let Some(href) = link.value().attr("href") {
                if !href.contains("/archetype/") {
                    continue;
                }
                let name = link.text().collect::<String>().trim().to_string();
                if name.is_empty() {
                    continue;
                }
                // Strip fragment (#online, #paper) from the URL
                let base_url = href.split('#').next().unwrap_or(href);
                let full_url = if base_url.starts_with("http") {
                    base_url.to_string()
                } else {
                    format!("https://www.mtggoldfish.com{base_url}")
                };
                // Deduplicate by URL
                if !deck_urls.iter().any(|(u, _)| u == &full_url) {
                    deck_urls.push((full_url, name));
                }
            }
        }
        if deck_urls.len() >= config.top_n {
            break;
        }
    }

    eprintln!("Found {} deck links", deck_urls.len());

    let mut decks = Vec::new();
    for (i, (url, archetype_name)) in deck_urls.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(config.delay_ms));
        }

        eprintln!("[{}/{}] Fetching: {archetype_name}", i + 1, deck_urls.len());

        match scrape_deck(client, url, archetype_name) {
            Some(deck) => decks.push(deck),
            None => eprintln!("  Failed to parse deck page"),
        }
    }

    decks
}

fn scrape_deck(client: &Client, url: &str, archetype_name: &str) -> Option<FeedDeck> {
    let resp = client.get(url).send().ok()?;
    let body = resp.text().ok()?;
    let document = Html::parse_document(&body);

    let mut main = Vec::new();
    let mut sideboard = Vec::new();
    let mut commander = Vec::new();
    let mut companion: Option<String> = None;

    // Primary: extract deck list from hidden input#deck_input_deck (most reliable)
    let input_sel = Selector::parse("input#deck_input_deck").ok()?;
    if let Some(el) = document.select(&input_sel).next() {
        if let Some(deck_text) = el.value().attr("value") {
            // HTML entities are decoded by the parser, but apostrophes may be &#39;
            let deck_text = deck_text.replace("&#39;", "'");
            parse_text_deck_list(
                &deck_text,
                &mut main,
                &mut sideboard,
                &mut commander,
                &mut companion,
            );
        }
    }

    // Fallback: try the deck-table rows
    if main.is_empty() {
        let row_sel = Selector::parse(".deck-table tr").ok()?;
        let mut in_sideboard = false;
        let mut in_commander = false;
        let mut in_companion = false;

        for row in document.select(&row_sel) {
            let text = row.text().collect::<String>();
            let trimmed = text.trim();

            if trimmed.eq_ignore_ascii_case("sideboard") {
                in_sideboard = true;
                in_commander = false;
                in_companion = false;
                continue;
            }
            if trimmed.eq_ignore_ascii_case("commander") {
                in_commander = true;
                in_sideboard = false;
                in_companion = false;
                continue;
            }
            if trimmed.eq_ignore_ascii_case("companion") {
                in_companion = true;
                in_sideboard = false;
                in_commander = false;
                continue;
            }

            if let Some(entry) = parse_deck_row(trimmed) {
                if in_commander {
                    commander.push(entry.name);
                } else if in_companion {
                    // CR 702.139a: Record companion name only — sideboard section
                    // will include the card separately.
                    companion = Some(entry.name);
                } else if in_sideboard {
                    sideboard.push(entry);
                } else {
                    main.push(entry);
                }
            }
        }
    }

    if main.is_empty() {
        return None;
    }

    let colors = infer_colors(&main);
    let commander_field = if commander.is_empty() {
        None
    } else {
        Some(commander)
    };

    Some(FeedDeck {
        name: archetype_name.to_string(),
        author: "MTGGoldfish".to_string(),
        colors,
        tags: vec![config_format_tag(&document)],
        main,
        sideboard,
        commander: commander_field,
        companion,
    })
}

fn parse_deck_row(text: &str) -> Option<DeckEntry> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    // Pattern: "4 Lightning Bolt" or "4x Lightning Bolt"
    let mut parts = text.splitn(2, |c: char| c.is_whitespace());
    let count_str = parts.next()?.trim_end_matches('x');
    let count: u32 = count_str.parse().ok()?;
    let name = parts.next()?.trim().to_string();

    if name.is_empty() || count == 0 {
        return None;
    }

    Some(DeckEntry { count, name })
}

fn parse_text_deck_list(
    text: &str,
    main: &mut Vec<DeckEntry>,
    sideboard: &mut Vec<DeckEntry>,
    commander: &mut Vec<String>,
    companion: &mut Option<String>,
) {
    let mut section = "main";
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("sideboard") {
            section = "sideboard";
            continue;
        }
        if trimmed.eq_ignore_ascii_case("commander") {
            section = "commander";
            continue;
        }
        if trimmed.eq_ignore_ascii_case("companion") {
            section = "companion";
            continue;
        }
        if let Some(entry) = parse_deck_row(trimmed) {
            match section {
                "commander" => commander.push(entry.name),
                "sideboard" => sideboard.push(entry),
                // CR 702.139a: Companion lives outside the game (sideboard).
                // Only record the name — the card will appear in the "Sideboard"
                // section naturally on MTGGoldfish. If a source omits it,
                // loadActiveDeck (storage.ts:98) ensures companion is in sideboard.
                "companion" => {
                    *companion = Some(entry.name);
                }
                _ => main.push(entry),
            }
        }
    }
}

fn infer_colors(cards: &[DeckEntry]) -> Vec<String> {
    let mut colors = Vec::new();
    let mut add = |c: &str| {
        let s = c.to_string();
        if !colors.contains(&s) {
            colors.push(s);
        }
    };

    for entry in cards {
        let inferred: &[&str] = match entry.name.as_str() {
            // Basic lands
            "Plains" | "Snow-Covered Plains" => &["W"],
            "Island" | "Snow-Covered Island" => &["U"],
            "Swamp" | "Snow-Covered Swamp" => &["B"],
            "Mountain" | "Snow-Covered Mountain" => &["R"],
            "Forest" | "Snow-Covered Forest" => &["G"],
            // Shock lands
            "Hallowed Fountain" => &["W", "U"],
            "Watery Grave" => &["U", "B"],
            "Blood Crypt" => &["B", "R"],
            "Sacred Foundry" => &["R", "W"],
            "Godless Shrine" => &["W", "B"],
            "Steam Vents" => &["U", "R"],
            "Stomping Ground" => &["R", "G"],
            "Breeding Pool" => &["U", "G"],
            "Temple Garden" => &["W", "G"],
            "Overgrown Tomb" => &["B", "G"],
            _ => &[],
        };
        for color in inferred {
            add(color);
        }
    }
    colors
}

fn config_format_tag(document: &Html) -> String {
    let title_sel = Selector::parse("title").expect("valid CSS selector");
    if let Some(title) = document.select(&title_sel).next() {
        let text = title.text().collect::<String>().to_lowercase();
        for format in [
            "standard",
            "modern",
            "pioneer",
            "commander",
            "legacy",
            "vintage",
            "pauper",
        ] {
            if text.contains(format) {
                return format.to_string();
            }
        }
    }
    "metagame".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shock_lands_infer_both_of_their_colors() {
        // Each Ravnica shock land is two-colored; inference must yield both.
        // Watery Grave (UB), Blood Crypt (BR), and Sacred Foundry (RW)
        // previously inferred only their primary color.
        let cases: [(&str, [&str; 2]); 10] = [
            ("Hallowed Fountain", ["W", "U"]),
            ("Watery Grave", ["U", "B"]),
            ("Blood Crypt", ["B", "R"]),
            ("Sacred Foundry", ["R", "W"]),
            ("Godless Shrine", ["W", "B"]),
            ("Steam Vents", ["U", "R"]),
            ("Stomping Ground", ["R", "G"]),
            ("Breeding Pool", ["U", "G"]),
            ("Temple Garden", ["W", "G"]),
            ("Overgrown Tomb", ["B", "G"]),
        ];
        for (name, expected) in cases {
            let cards = [DeckEntry {
                count: 4,
                name: name.to_string(),
            }];
            let colors = infer_colors(&cards);
            assert_eq!(
                colors.len(),
                2,
                "{name} should infer two colors, got {colors:?}"
            );
            for color in expected {
                assert!(
                    colors.iter().any(|c| c.as_str() == color),
                    "{name} should infer {color}, got {colors:?}"
                );
            }
        }
    }

    #[test]
    fn companion_section_sets_name_and_stays_out_of_main() {
        let text = "4 Lightning Bolt\n20 Mountain\nCompanion\n1 Lurrus of the Dream-Den\nSideboard\n1 Lurrus of the Dream-Den\n2 Wear // Tear";
        let mut main = Vec::new();
        let mut sideboard = Vec::new();
        let mut commander = Vec::new();
        let mut companion = None;
        parse_text_deck_list(
            text,
            &mut main,
            &mut sideboard,
            &mut commander,
            &mut companion,
        );

        assert_eq!(companion, Some("Lurrus of the Dream-Den".to_string()));
        // Lurrus should NOT be in main
        assert!(!main.iter().any(|e| e.name == "Lurrus of the Dream-Den"));
        // Lurrus should be in sideboard exactly once (from the Sideboard section)
        let lurrus_count: u32 = sideboard
            .iter()
            .filter(|e| e.name == "Lurrus of the Dream-Den")
            .map(|e| e.count)
            .sum();
        assert_eq!(lurrus_count, 1);
        // Other sideboard cards parsed normally
        assert!(sideboard.iter().any(|e| e.name == "Wear // Tear"));
        // Main deck parsed normally
        assert_eq!(main.len(), 2);
    }

    #[test]
    fn companion_only_no_sideboard_section() {
        let text = "4 Lightning Bolt\nCompanion\n1 Lurrus of the Dream-Den";
        let mut main = Vec::new();
        let mut sideboard = Vec::new();
        let mut commander = Vec::new();
        let mut companion = None;
        parse_text_deck_list(
            text,
            &mut main,
            &mut sideboard,
            &mut commander,
            &mut companion,
        );

        assert_eq!(companion, Some("Lurrus of the Dream-Den".to_string()));
        // No sideboard section → sideboard is empty (loadActiveDeck safety net handles this)
        assert!(sideboard.is_empty());
        assert!(!main.iter().any(|e| e.name == "Lurrus of the Dream-Den"));
    }

    #[test]
    fn no_companion_section_regression() {
        let text = "4 Lightning Bolt\n20 Mountain\nSideboard\n2 Wear // Tear";
        let mut main = Vec::new();
        let mut sideboard = Vec::new();
        let mut commander = Vec::new();
        let mut companion = None;
        parse_text_deck_list(
            text,
            &mut main,
            &mut sideboard,
            &mut commander,
            &mut companion,
        );

        assert!(companion.is_none());
        assert_eq!(main.len(), 2);
        assert_eq!(sideboard.len(), 1);
    }
}
