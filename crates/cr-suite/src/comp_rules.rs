//! CompRules.txt parser for cr-suite fixture generation.
//!
//! Independently re-implements the rules-audit CompRules walk so this crate
//! does not depend on the `audit` binary feature. Keep the inclusion filter
//! aligned with [`crate::catalog::is_included_section`].

use std::collections::BTreeSet;
use std::path::Path;

use crate::catalog::is_included_section;

/// A single Comprehensive Rules entry extracted from CompRules.txt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompRule {
    /// Rule number, e.g. `"704.5a"`.
    pub number: String,
    /// First-line rule text (examples omitted).
    pub text: String,
    /// Three-digit section, e.g. `704`.
    pub section: u32,
}

/// Parse `MagicCompRules.txt` into structured rules for included sections.
pub fn parse_comp_rules(path: &Path) -> Result<Vec<CompRule>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    Ok(parse_comp_rules_text(&content))
}

/// Parse CompRules content already loaded in memory.
pub fn parse_comp_rules_text(content: &str) -> Vec<CompRule> {
    let mut rules = Vec::new();
    let mut seen_numbers: BTreeSet<String> = BTreeSet::new();
    let mut past_toc = false;
    let mut toc_header_count = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Detect "100. General" — appears once in TOC and once in content.
        if trimmed == "100. General" {
            toc_header_count += 1;
            if toc_header_count >= 2 {
                past_toc = true;
            }
            continue;
        }
        if !past_toc {
            continue;
        }

        let Some(number) = extract_rule_number(trimmed) else {
            continue;
        };

        if is_section_header(&number) {
            continue;
        }

        let text = trimmed[number.len()..]
            .trim()
            .trim_start_matches('.')
            .trim()
            .to_string();

        let section = parse_section(&number);
        if !is_included_section(section) {
            continue;
        }

        if seen_numbers.insert(number.clone()) {
            rules.push(CompRule {
                number,
                text,
                section,
            });
        }
    }

    rules
}

/// Extract a rule number from the start of a line (`704.5a`, `119.3`, …).
pub fn extract_rule_number(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    if bytes.len() < 4 {
        return None;
    }

    if !bytes[0].is_ascii_digit() || !bytes[1].is_ascii_digit() || !bytes[2].is_ascii_digit() {
        return None;
    }
    if bytes[3] != b'.' {
        return None;
    }

    let mut end = 4;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }

    if end < bytes.len() && bytes[end].is_ascii_lowercase() {
        if end + 1 >= bytes.len() || !bytes[end + 1].is_ascii_alphanumeric() {
            end += 1;
        }
    }

    if end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        return None;
    }

    Some(line[..end].to_string())
}

fn is_section_header(number: &str) -> bool {
    number.split('.').nth(1).unwrap_or("").is_empty()
}

fn parse_section(number: &str) -> u32 {
    number[..3].parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_common_rule_shapes() {
        assert_eq!(
            extract_rule_number("704.5a A player with 0 or less life loses the game."),
            Some("704.5a".into())
        );
        assert_eq!(
            extract_rule_number("119.3 If an effect causes a player to gain life"),
            Some("119.3".into())
        );
        assert_eq!(extract_rule_number("Not a rule"), None);
    }

    #[test]
    fn skips_toc_and_filters_sections() {
        let sample = r#"
Table of Contents
100. General
101. The Magic Golden Rules

100. General
100.1. These Magic rules apply…
101.1. The Magic Golden Rules…
999.1. Out of scope section rule.
"#;
        let rules = parse_comp_rules_text(sample);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].number, "100.1");
        assert_eq!(rules[1].number, "101.1");
    }
}
