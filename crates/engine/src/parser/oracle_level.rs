use nom::bytes::tag;
use nom::Parser;

use crate::types::ability::{
    ContinuousModification, StaticCondition, StaticDefinition, TargetFilter,
};

use super::oracle::find_activated_colon;
use super::oracle_keyword::parse_keyword_from_oracle;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_special::normalize_self_refs_for_static;
use super::oracle_static::{parse_static_line, parse_static_line_multi};

/// CR 711: Parse LEVEL block lines from a leveler creature's Oracle text.
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
/// Returns:
/// - `Vec<StaticDefinition>`: Parsed static abilities (P/T, keywords) gated on level counter count.
/// - `Vec<usize>`: Indices of consumed Oracle text lines.
/// - `Vec<(String, StaticCondition)>`: Ability text lines found within level blocks,
///   paired with the level condition they should be gated by. These need re-parsing
///   by the main Oracle dispatcher as triggers, activated abilities, or statics.
pub(crate) fn parse_level_blocks(
    lines: &[&str],
    card_name: &str,
) -> (
    Vec<StaticDefinition>,
    Vec<usize>,
    Vec<(String, StaticCondition)>,
) {
    let mut statics = Vec::new();
    let mut consumed_indices = Vec::new();
    let mut ability_lines = Vec::new();

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

                // CR 711.2a + CR 711.2b: Static abilities within LEVEL blocks get a
                // HasCounters condition. Parse them directly here so they get the level
                // condition attached without a redundant re-parse round-trip through oracle.rs.
                let static_text = normalize_self_refs_for_static(next, card_name);
                let multi = parse_static_line_multi(&static_text);
                if !multi.is_empty() {
                    consumed_indices.push(i);
                    for mut sd in multi {
                        sd.condition = Some(condition.clone());
                        statics.push(sd);
                    }
                    i += 1;
                    continue;
                }
                if let Some(mut sd) = parse_static_line(&static_text) {
                    consumed_indices.push(i);
                    sd.condition = Some(condition.clone());
                    statics.push(sd);
                    i += 1;
                    continue;
                }

                // Activated abilities: structural colon with cost-like prefix (mana symbols,
                // "sacrifice", "tap", etc.). Uses find_activated_colon from oracle.rs.
                // Triggered abilities: nom-based prefix detection for "when"/"whenever"/"at the beginning".
                let is_activated = find_activated_colon(next).is_some();
                let is_trigger = tag::<&str, &str, nom::error::Error<&str>>("when")
                    .parse(next_lower.as_str())
                    .is_ok()
                    || tag::<&str, &str, nom::error::Error<&str>>("whenever")
                        .parse(next_lower.as_str())
                        .is_ok()
                    || tag::<&str, &str, nom::error::Error<&str>>("at the beginning")
                        .parse(next_lower.as_str())
                        .is_ok();
                if is_activated || is_trigger {
                    consumed_indices.push(i);
                    ability_lines.push((next.to_string(), condition.clone()));
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

    (statics, consumed_indices, ability_lines)
}

enum LevelRange {
    Bounded { min: u32, max: u32 },
    Unbounded { min: u32 },
}

/// Parse "level N-M" or "level N+" from lowercase text.
///
/// Uses `nom_primitives::parse_number` for number recognition within level headers.
fn parse_level_header(lower: &str) -> Option<LevelRange> {
    let rest = lower.strip_prefix("level ")?;
    let rest = rest.trim();

    if let Some(plus_rest) = rest.strip_suffix('+') {
        let (_, min) = nom_primitives::parse_number(plus_rest.trim()).ok()?;
        Some(LevelRange::Unbounded { min })
    } else if rest.contains('-') {
        let mut parts = rest.splitn(2, '-');
        let (_, min) = nom_primitives::parse_number(parts.next()?.trim()).ok()?;
        let (_, max) = nom_primitives::parse_number(parts.next()?.trim()).ok()?;
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
        let (statics, consumed, _ability_lines) = parse_level_blocks(&lines, "Test Card");

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

    #[test]
    fn parse_level_block_with_trigger() {
        let lines = vec![
            "LEVEL 6+",
            "6/6",
            "Whenever this creature attacks, it deals 6 damage to each creature defending player controls.",
        ];
        let (statics, consumed, ability_lines) = parse_level_blocks(&lines, "Test Card");

        // P/T modifications parsed as static
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0].modifications.len(), 2);

        // Trigger line captured with level condition
        assert_eq!(ability_lines.len(), 1);
        assert_eq!(
            ability_lines[0].0,
            "Whenever this creature attacks, it deals 6 damage to each creature defending player controls."
        );
        assert!(matches!(
            ability_lines[0].1,
            StaticCondition::HasCounters {
                minimum: 6,
                maximum: None,
                ..
            }
        ));

        // All 3 lines consumed
        assert_eq!(consumed.len(), 3);
    }

    #[test]
    fn parse_level_block_with_activated_ability() {
        let lines = vec![
            "LEVEL 1-2",
            "2/3",
            "{T}: This creature deals 1 damage to any target.",
            "LEVEL 3+",
            "2/4",
            "{T}: This creature deals 3 damage to any target.",
        ];
        let (statics, consumed, ability_lines) = parse_level_blocks(&lines, "Test Card");

        // Two P/T statics
        assert_eq!(statics.len(), 2);

        // Two activated ability lines captured with level conditions
        assert_eq!(ability_lines.len(), 2);

        // First: bounded range 1-2
        assert_eq!(
            ability_lines[0].0,
            "{T}: This creature deals 1 damage to any target."
        );
        assert!(matches!(
            ability_lines[0].1,
            StaticCondition::HasCounters {
                minimum: 1,
                maximum: Some(2),
                ..
            }
        ));

        // Second: unbounded 3+
        assert_eq!(
            ability_lines[1].0,
            "{T}: This creature deals 3 damage to any target."
        );
        assert!(matches!(
            ability_lines[1].1,
            StaticCondition::HasCounters {
                minimum: 3,
                maximum: None,
                ..
            }
        ));

        // All 6 lines consumed
        assert_eq!(consumed.len(), 6);
    }

    #[test]
    fn parse_level_block_with_static_lord_ability() {
        // Coralhelm Commander pattern: LEVEL 4+ block contains a lord static
        let lines = vec![
            "LEVEL 4+",
            "4/4",
            "Flying",
            "Other Merfolk creatures you control get +1/+1.",
        ];
        let (statics, consumed, ability_lines) = parse_level_blocks(&lines, "Coralhelm Commander");

        // Lord pump parsed directly as statics[0] (encountered first in inner loop),
        // P/T + keyword block pushed as statics[1] (after inner loop completes).
        assert_eq!(statics.len(), 2);
        assert_eq!(statics[0].modifications.len(), 2); // AddPower, AddToughness (lord pump)
        assert_eq!(statics[1].modifications.len(), 3); // SetPower, SetToughness, AddKeyword

        // Lord static parsed directly with level condition (not routed to ability_lines)
        assert_eq!(ability_lines.len(), 0);
        assert!(matches!(
            statics[0].condition,
            Some(StaticCondition::HasCounters {
                minimum: 4,
                maximum: None,
                ..
            })
        ));

        // All 4 lines consumed
        assert_eq!(consumed.len(), 4);
    }
}
