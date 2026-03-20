use crate::types::ability::{
    ContinuousModification, StaticCondition, StaticDefinition, TargetFilter,
};

use super::oracle_keyword::parse_keyword_from_oracle;

/// CR 710: Parse LEVEL block lines from a leveler creature's Oracle text.
///
/// Level-up creature Oracle text contains blocks like:
/// ```text
/// LEVEL 4-7
/// 4/4
/// Flying
/// LEVEL 8+
/// 8/8
/// Flying, trample
/// ```
///
/// Each LEVEL block defines static abilities gated on level counter count.
/// P/T lines use SetPower/SetToughness (Layer 7b), keyword lines use AddKeyword (Layer 6).
/// Both are conditioned on `StaticCondition::HasCounters` with min/max.
///
/// Returns the parsed static definitions and the set of Oracle line indices consumed.
pub(crate) fn parse_level_blocks(lines: &[&str]) -> (Vec<StaticDefinition>, Vec<usize>) {
    let mut statics = Vec::new();
    let mut consumed_indices = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        let lower = line.to_lowercase();

        // Detect "LEVEL N-M" or "LEVEL N+"
        if let Some(range) = parse_level_header(&lower) {
            consumed_indices.push(i);
            i += 1;

            // Build condition from level range
            let condition = match range {
                LevelRange::Bounded { min, max } => StaticCondition::HasCounters {
                    counter_type: "level".to_string(),
                    minimum: min,
                    maximum: Some(max),
                },
                LevelRange::Unbounded { min } => StaticCondition::HasCounters {
                    counter_type: "level".to_string(),
                    minimum: min,
                    maximum: None,
                },
            };

            // Consume subsequent lines: P/T line and keyword lines until next LEVEL or end
            let mut modifications = Vec::new();
            let mut description_parts = vec![line.to_string()];

            while i < lines.len() {
                let next = lines[i].trim();
                if next.is_empty() {
                    i += 1;
                    continue;
                }
                let next_lower = next.to_lowercase();

                // Stop if we hit another LEVEL header or a non-level line
                if parse_level_header(&next_lower).is_some() {
                    break;
                }

                // Try to parse as P/T (e.g., "4/4")
                if let Some((p, t)) = parse_pt_line(next) {
                    consumed_indices.push(i);
                    description_parts.push(next.to_string());
                    modifications.push(ContinuousModification::SetPower { value: p });
                    modifications.push(ContinuousModification::SetToughness { value: t });
                    i += 1;
                    continue;
                }

                // Try to parse as keyword line (e.g., "Flying" or "Flying, trample")
                let keywords: Vec<&str> = next.split(',').map(|s| s.trim()).collect();
                let mut any_keyword = false;
                for kw_text in &keywords {
                    if let Some(kw) = parse_keyword_from_oracle(&kw_text.to_lowercase()) {
                        if !matches!(kw, crate::types::keywords::Keyword::Unknown(_)) {
                            modifications.push(ContinuousModification::AddKeyword { keyword: kw });
                            any_keyword = true;
                        }
                    }
                }

                if any_keyword {
                    consumed_indices.push(i);
                    description_parts.push(next.to_string());
                    i += 1;
                    continue;
                }

                // Not a recognized level block line — stop consuming
                break;
            }

            if !modifications.is_empty() {
                statics.push(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .condition(condition)
                        .modifications(modifications)
                        .description(description_parts.join(" / ")),
                );
            }
        } else {
            i += 1;
        }
    }

    (statics, consumed_indices)
}

enum LevelRange {
    Bounded { min: u32, max: u32 },
    Unbounded { min: u32 },
}

/// Parse "level N-M" or "level N+" from lowercase text.
fn parse_level_header(lower: &str) -> Option<LevelRange> {
    let rest = lower.strip_prefix("level ")?;
    let rest = rest.trim();

    if let Some(plus_rest) = rest.strip_suffix('+') {
        let min: u32 = plus_rest.trim().parse().ok()?;
        Some(LevelRange::Unbounded { min })
    } else if rest.contains('-') {
        let mut parts = rest.splitn(2, '-');
        let min: u32 = parts.next()?.trim().parse().ok()?;
        let max: u32 = parts.next()?.trim().parse().ok()?;
        Some(LevelRange::Bounded { min, max })
    } else {
        None
    }
}

/// Parse a P/T line like "4/4" or "3/5".
fn parse_pt_line(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    let slash = text.find('/')?;
    let power: i32 = text[..slash].trim().parse().ok()?;
    let toughness: i32 = text[slash + 1..].trim().parse().ok()?;
    Some((power, toughness))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_bounded_header() {
        assert!(matches!(
            parse_level_header("level 4-7"),
            Some(LevelRange::Bounded { min: 4, max: 7 })
        ));
    }

    #[test]
    fn parse_level_unbounded_header() {
        assert!(matches!(
            parse_level_header("level 8+"),
            Some(LevelRange::Unbounded { min: 8 })
        ));
    }

    #[test]
    fn parse_full_level_blocks() {
        let lines = vec![
            "Level up {R}",
            "LEVEL 4-7",
            "4/4",
            "Flying",
            "LEVEL 8+",
            "8/8",
        ];
        let (statics, consumed) = parse_level_blocks(&lines);

        // Should consume indices 1-5 (not index 0 which is "Level up {R}")
        assert!(!consumed.contains(&0));
        assert_eq!(statics.len(), 2);

        // First block: LEVEL 4-7 → SetPower, SetToughness, AddKeyword(Flying)
        assert_eq!(statics[0].modifications.len(), 3);
        assert!(matches!(
            statics[0].condition,
            Some(StaticCondition::HasCounters {
                minimum: 4,
                maximum: Some(7),
                ..
            })
        ));

        // Second block: LEVEL 8+ → SetPower, SetToughness
        assert_eq!(statics[1].modifications.len(), 2);
        assert!(matches!(
            statics[1].condition,
            Some(StaticCondition::HasCounters {
                minimum: 8,
                maximum: None,
                ..
            })
        ));
    }
}
