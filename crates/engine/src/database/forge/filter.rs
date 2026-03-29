use crate::parser::oracle_util::canonicalize_subtype_name;
use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter};
use crate::types::keywords::Keyword;
use crate::types::Zone;

use super::types::ForgeTranslateError;

/// Translate a Forge filter string into a `TargetFilter`.
///
/// Forge filter strings use dot-separated predicates within a single filter,
/// commas for OR (disjunction between filters).
///
/// Examples:
/// - `"Creature.YouCtrl"` → creature you control
/// - `"Card.Self"` / `"CARDNAME"` → self-reference
/// - `"Creature,Planeswalker"` → creature or planeswalker
/// - `"Creature.powerGE4.YouCtrl"` → creature you control with power >= 4
/// - `"Any"` → any target
pub(crate) fn translate_filter(filter_str: &str) -> Result<TargetFilter, ForgeTranslateError> {
    let filter_str = filter_str.trim();

    // Special refs
    match filter_str {
        "Any" | "any" => return Ok(TargetFilter::Any),
        "Card.Self" | "CARDNAME" | "Self" => return Ok(TargetFilter::SelfRef),
        "You" | "Player.You" => return Ok(TargetFilter::Controller),
        "Opponent" | "Player.Opp" => {
            return Ok(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ))
        }
        "Player" => return Ok(TargetFilter::Player),
        _ => {}
    }

    // Check for OR (comma-separated)
    if filter_str.contains(',') {
        let parts: Vec<&str> = filter_str.split(',').collect();
        let filters: Result<Vec<TargetFilter>, _> =
            parts.iter().map(|p| translate_filter(p.trim())).collect();
        return Ok(TargetFilter::Or { filters: filters? });
    }

    // Dot-separated predicates
    let segments: Vec<&str> = filter_str.split('.').collect();
    translate_dotted_filter(&segments)
}

fn translate_dotted_filter(segments: &[&str]) -> Result<TargetFilter, ForgeTranslateError> {
    let mut type_filter: Option<TypeFilter> = None;
    let mut controller: Option<ControllerRef> = None;
    let mut properties: Vec<FilterProp> = Vec::new();
    let mut is_card_prefix = false;
    let mut extra_type_filters: Vec<TypeFilter> = Vec::new();

    for &seg in segments {
        match seg {
            // Base type: skip "Card" prefix
            "Card" => {
                is_card_prefix = true;
                continue;
            }

            // Type predicates
            "Creature" => type_filter = Some(TypeFilter::Creature),
            "Artifact" => type_filter = Some(TypeFilter::Artifact),
            "Enchantment" => type_filter = Some(TypeFilter::Enchantment),
            "Land" => type_filter = Some(TypeFilter::Land),
            "Planeswalker" => type_filter = Some(TypeFilter::Planeswalker),
            "Instant" => type_filter = Some(TypeFilter::Instant),
            "Sorcery" => type_filter = Some(TypeFilter::Sorcery),
            "Permanent" => type_filter = Some(TypeFilter::Permanent),
            "nonCreature" => type_filter = Some(TypeFilter::Non(Box::new(TypeFilter::Creature))),
            "nonLand" => type_filter = Some(TypeFilter::Non(Box::new(TypeFilter::Land))),
            "nonArtifact" => type_filter = Some(TypeFilter::Non(Box::new(TypeFilter::Artifact))),
            "nonToken" => {
                // Filter out tokens — use Not(Token) property
                properties.push(FilterProp::Other {
                    value: "nonToken".to_string(),
                });
            }

            // Controller predicates
            "YouCtrl" => controller = Some(ControllerRef::You),
            "OppCtrl" => controller = Some(ControllerRef::Opponent),
            "YouOwn" => controller = Some(ControllerRef::You),

            // Self references
            "Self" => return Ok(TargetFilter::SelfRef),

            // Property predicates
            "tapped" => properties.push(FilterProp::Tapped),
            "untapped" => properties.push(FilterProp::Untapped),
            "attacking" => properties.push(FilterProp::Attacking),
            "token" => properties.push(FilterProp::Token),

            // Zone predicates
            "inZoneBattlefield" | "inRealZoneBattlefield" => {
                properties.push(FilterProp::InZone {
                    zone: Zone::Battlefield,
                });
            }
            "inZoneGraveyard" => {
                properties.push(FilterProp::InZone {
                    zone: Zone::Graveyard,
                });
            }
            "inZoneHand" => {
                properties.push(FilterProp::InZone { zone: Zone::Hand });
            }
            "inZoneExile" => {
                properties.push(FilterProp::InZone { zone: Zone::Exile });
            }

            // Power/toughness comparisons
            seg if seg.starts_with("power") => {
                if let Some(prop) = parse_pt_predicate(seg) {
                    properties.push(prop);
                }
            }

            // Keyword predicates: "withFlying", "withHaste", etc.
            seg if seg.starts_with("with") => {
                if let Some(kw_name) = seg.strip_prefix("with") {
                    let kw: Keyword = kw_name.parse().unwrap();
                    if !matches!(kw, Keyword::Unknown(_)) {
                        properties.push(FilterProp::WithKeyword { value: kw });
                    }
                }
            }

            // Subtype predicates (e.g., "Goblin", "Elf", "Zombie")
            // Starts with uppercase and isn't recognized as a type → subtype.
            seg if seg.starts_with(|c: char| c.is_uppercase()) && !is_card_prefix => {
                let canonical = canonicalize_subtype_name(seg);
                extra_type_filters.push(TypeFilter::Subtype(canonical));
            }

            _ => {
                // Unrecognized segment — skip silently for graceful degradation
            }
        }
    }

    // Build TypedFilter via builder API
    let mut typed = if let Some(tf) = type_filter {
        TypedFilter::new(tf)
    } else if is_card_prefix || !properties.is_empty() || controller.is_some() {
        // "Card.YouCtrl" without a type = any permanent you control
        TypedFilter::permanent()
    } else if !extra_type_filters.is_empty() {
        // Bare subtype like "Goblin" → creature with subtype
        TypedFilter::creature()
    } else {
        return Ok(TargetFilter::Any);
    };

    for tf in extra_type_filters {
        typed = typed.with_type(tf);
    }

    if let Some(ctrl) = controller {
        typed = typed.controller(ctrl);
    }

    if !properties.is_empty() {
        typed = typed.properties(properties);
    }

    Ok(TargetFilter::Typed(typed))
}

/// Parse power predicate segments like "powerGE4", "powerLE2".
fn parse_pt_predicate(seg: &str) -> Option<FilterProp> {
    let rest = seg.strip_prefix("power")?;

    if let Some(val_str) = rest.strip_prefix("GE") {
        let val: i32 = val_str.parse().ok()?;
        Some(FilterProp::PowerGE { value: val })
    } else if let Some(val_str) = rest.strip_prefix("LE") {
        let val: i32 = val_str.parse().ok()?;
        Some(FilterProp::PowerLE { value: val })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_any() {
        assert_eq!(translate_filter("Any").unwrap(), TargetFilter::Any);
    }

    #[test]
    fn test_self_ref() {
        assert_eq!(
            translate_filter("Card.Self").unwrap(),
            TargetFilter::SelfRef
        );
        assert_eq!(translate_filter("CARDNAME").unwrap(), TargetFilter::SelfRef);
    }

    #[test]
    fn test_creature_you_ctrl() {
        let result = translate_filter("Creature.YouCtrl").unwrap();
        match result {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("expected Typed, got {other:?}"),
        }
    }

    #[test]
    fn test_or_filter() {
        let result = translate_filter("Creature,Planeswalker").unwrap();
        match result {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
            }
            other => panic!("expected Or, got {other:?}"),
        }
    }

    #[test]
    fn test_power_ge_predicate() {
        let result = translate_filter("Creature.powerGE4").unwrap();
        match result {
            TargetFilter::Typed(tf) => {
                assert!(tf.properties.contains(&FilterProp::PowerGE { value: 4 }));
            }
            other => panic!("expected Typed, got {other:?}"),
        }
    }

    #[test]
    fn test_non_creature() {
        let result = translate_filter("nonCreature").unwrap();
        match result {
            TargetFilter::Typed(tf) => {
                assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::Non(Box::new(TypeFilter::Creature))]
                );
            }
            other => panic!("expected Typed, got {other:?}"),
        }
    }
}
