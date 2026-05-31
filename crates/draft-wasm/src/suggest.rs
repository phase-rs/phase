use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use draft_core::types::{DeckAddableCardPolicy, DeckAddableCards, DraftCardInstance};
use engine::database::CardDatabase;
use engine::types::mana::ManaType;
use phase_ai::config::AiDifficulty;
use phase_ai::{draft_eval, mana_colors};

/// A suggested Limited deck: spell names + land distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedDeck {
    pub main_deck: Vec<String>,
    pub lands: HashMap<String, u8>,
}

/// A standard Limited deck is 40 cards: ~23 spells + ~17 lands.
const DEFAULT_DECK_SIZE: usize = 40;
const DEFAULT_SPELLS: usize = 23;

/// Auto-build a playable 40-card Limited deck from a pool.
///
/// Per D-12: selects ~23 best spells + ~17 lands with curve awareness, always
/// totalling exactly `TARGET_DECK_SIZE` cards.
/// Algorithm:
/// 1. Identify the 2 strongest colors by card count + quality
/// 2. Score every card; pick ~23 on-color spells respecting the mana curve
/// 3. If the on-color pool is too thin to field 23, top up with the best
///    remaining cards regardless of color so the deck still reaches 40
/// 4. Fill the remaining slots with lands distributed by color frequency
pub fn suggest_deck(
    pool: &[DraftCardInstance],
    _difficulty: AiDifficulty,
    card_db: Option<&CardDatabase>,
    min_deck_size: usize,
    addable_cards: &DeckAddableCards,
) -> SuggestedDeck {
    // `_difficulty` is intentionally unused: deck suggestion always builds the
    // strongest legal deck. Difficulty governs the *opponents*, not the player's
    // own deck.
    if pool.is_empty() {
        return SuggestedDeck {
            main_deck: Vec::new(),
            lands: HashMap::new(),
        };
    }

    let best_colors = find_best_colors(pool, card_db);

    // Spell candidates: every pool card that isn't a land. Lands are added
    // separately as basics in step 4 (a drafted nonbasic land counted here
    // would inflate the deck past 40 once basics are layered on top).
    let mut scored: Vec<(&DraftCardInstance, f64)> = pool
        .iter()
        .filter(|c| !is_land(c))
        .map(|c| (c, score_card(c, card_db)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // On-color (or colorless) cards, preserving the global score order.
    let on_color: Vec<(&DraftCardInstance, f64)> = scored
        .iter()
        .filter(|(c, _)| {
            c.colors.is_empty()
                || c.colors
                    .iter()
                    .any(|col| best_colors.contains(&col.as_str()))
        })
        .copied()
        .collect();

    let target_spells = target_spell_count(min_deck_size);
    let mut spells = select_spells_with_curve(&on_color, target_spells);

    // If we couldn't field 23 on-color playables, top up with the best
    // remaining cards from anywhere in the pool so the deck still hits 40.
    if spells.len() < target_spells {
        let chosen: HashSet<&str> = spells.iter().map(|c| c.instance_id.as_str()).collect();
        for entry in &scored {
            if spells.len() >= target_spells {
                break;
            }
            let card = entry.0;
            if !chosen.contains(card.instance_id.as_str()) {
                spells.push(card);
            }
        }
    }

    // `main_deck` holds the non-land spells only; `lands` carries the land
    // distribution separately. Consumers (the deckbuilder store, `get_bot_deck`)
    // concatenate the two — appending lands here as well would double-count them
    // (e.g. 23 spells + 17 lands in `main_deck`, then +17 lands again = 57).
    let spell_names: Vec<String> = spells.iter().map(|c| c.name.clone()).collect();
    let land_total = min_deck_size.saturating_sub(spell_names.len()) as u8;

    // Admit on-color drafted nonbasic fixing lands into the manabase — only under
    // the standard basic-land fill (a custom addable-card policy supplies its own
    // lands, so injecting nonbasics there would be wrong). Each admitted nonbasic
    // replaces exactly one basic, so the deck total is unchanged.
    let nonbasic_lands = if matches!(
        addable_cards.policy,
        DeckAddableCardPolicy::StandardBasics | DeckAddableCardPolicy::StandardBasicsPlusCustom
    ) {
        select_fixing_lands(pool, &best_colors, card_db, land_total)
    } else {
        HashMap::new()
    };
    let nonbasic_count: u8 = nonbasic_lands.values().copied().sum();
    let basics_total = land_total.saturating_sub(nonbasic_count);
    let mut lands = suggest_addable_cards(&spell_names, pool, basics_total, addable_cards);
    for (name, count) in nonbasic_lands {
        *lands.entry(name).or_insert(0) += count;
    }

    SuggestedDeck {
        main_deck: spell_names,
        lands,
    }
}

/// On-color drafted nonbasic fixing lands as a `name -> copy-count` map, capped at
/// `cap` lands total. A fixing land is a drafted nonbasic that taps for 2+ colors
/// (via basic land subtypes and/or `Effect::Mana` abilities — shared with draft
/// pick value through [`mana_colors::land_produced_color_types`]) including at
/// least one of the deck's colors. Each admitted copy is a real drafted entry, so
/// copy counts never exceed what the drafter owns. Empty without a card database
/// (produced colors can't be read from the printed type line alone).
fn select_fixing_lands(
    pool: &[DraftCardInstance],
    best_colors: &[&str],
    card_db: Option<&CardDatabase>,
    cap: u8,
) -> HashMap<String, u8> {
    let Some(db) = card_db else {
        return HashMap::new();
    };
    let mut result: HashMap<String, u8> = HashMap::new();
    let mut admitted: u8 = 0;
    for card in pool {
        if admitted >= cap {
            break;
        }
        if !is_land(card) {
            continue;
        }
        let Some(face) = db.get_face_by_name(&card.name) else {
            continue;
        };
        let colors =
            mana_colors::land_produced_color_types(&face.card_type.subtypes, &face.abilities);
        if colors.len() < 2 {
            continue;
        }
        let on_color = colors
            .iter()
            .filter_map(|&t| mana_type_to_color_str(t))
            .any(|s| best_colors.contains(&s));
        if !on_color {
            continue;
        }
        *result.entry(card.name.clone()).or_insert(0) += 1;
        admitted += 1;
    }
    result
}

/// Map a produced `ManaType` to the "W/U/B/R/G" key the color logic uses;
/// colorless has no color key.
fn mana_type_to_color_str(t: ManaType) -> Option<&'static str> {
    match t {
        ManaType::White => Some("W"),
        ManaType::Blue => Some("U"),
        ManaType::Black => Some("B"),
        ManaType::Red => Some("R"),
        ManaType::Green => Some("G"),
        ManaType::Colorless => None,
    }
}

/// Whether a drafted card is a land (so it isn't counted as a spell).
///
/// The engine-truth check is `CardFace.card_type` containing `CoreType::Land`,
/// but this filter runs over the raw `DraftCardInstance` pool before any
/// `CardDatabase` lookup, so the printed type line is the right tool here.
fn is_land(card: &DraftCardInstance) -> bool {
    card.type_line.to_ascii_lowercase().contains("land")
}

/// Find the 2 strongest colors in the pool by card count weighted by quality.
fn find_best_colors<'a>(
    pool: &[DraftCardInstance],
    card_db: Option<&CardDatabase>,
) -> Vec<&'a str> {
    let mut color_scores: HashMap<&str, f64> = HashMap::new();

    for card in pool {
        let card_score = score_card(card, card_db);
        for color in &card.colors {
            let key = match color.as_str() {
                "W" => "W",
                "U" => "U",
                "B" => "B",
                "R" => "R",
                "G" => "G",
                _ => continue,
            };
            *color_scores.entry(key).or_insert(0.0) += card_score;
        }
    }

    let mut sorted: Vec<(&&str, &f64)> = color_scores.iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));

    sorted.iter().take(2).map(|(color, _)| **color).collect()
}

/// Score a card for deck inclusion: the shared engine-data evaluator
/// ([`draft_eval::evaluate_draft_card`]) plus a small rarity prior, falling back
/// to just the rarity prior when no `CardDatabase` is loaded.
fn score_card(card: &DraftCardInstance, card_db: Option<&CardDatabase>) -> f64 {
    let quality = card_db
        .and_then(|db| db.get_face_by_name(&card.name))
        .map(draft_eval::evaluate_draft_card_default)
        .unwrap_or(0.0);
    quality + draft_eval::rarity_prior(&card.rarity)
}

/// Select spells respecting a good mana curve for Limited.
///
/// Target distribution for ~23 spells:
/// - CMC 1: 1-2
/// - CMC 2: 5-6
/// - CMC 3: 5-6
/// - CMC 4: 3-4
/// - CMC 5: 2-3
/// - CMC 6+: 1-2
fn select_spells_with_curve<'a>(
    scored: &[(&'a DraftCardInstance, f64)],
    target: usize,
) -> Vec<&'a DraftCardInstance> {
    // Curve slot targets
    let curve_targets: [(u8, u8, usize); 6] = [
        (0, 1, 2),   // CMC 0-1: up to 2
        (2, 2, 6),   // CMC 2: up to 6
        (3, 3, 6),   // CMC 3: up to 6
        (4, 4, 4),   // CMC 4: up to 4
        (5, 5, 3),   // CMC 5: up to 3
        (6, 255, 2), // CMC 6+: up to 2
    ];

    let mut selected: Vec<&DraftCardInstance> = Vec::new();
    let mut used: Vec<bool> = vec![false; scored.len()];

    // First pass: fill curve slots from highest-scored cards
    for (cmc_low, cmc_high, max_count) in &curve_targets {
        let mut count = 0;
        for (i, (card, _)) in scored.iter().enumerate() {
            if used[i] {
                continue;
            }
            if card.cmc >= *cmc_low && card.cmc <= *cmc_high && count < *max_count {
                selected.push(card);
                used[i] = true;
                count += 1;
            }
        }
    }

    // Second pass: fill remaining slots with best remaining cards
    if selected.len() < target {
        for (i, (card, _)) in scored.iter().enumerate() {
            if selected.len() >= target {
                break;
            }
            if !used[i] {
                selected.push(card);
                used[i] = true;
            }
        }
    }

    // Truncate to target if we overshot
    selected.truncate(target);
    selected
}

/// Suggest a color-proportional land distribution for a set of spells, sized so
/// that `spells + lands` reaches a standard 40-card deck (clamped to a sane
/// 16–18 land count for hand-built decks). Per D-11.
pub fn suggest_lands(
    spell_names: &[String],
    pool: &[DraftCardInstance],
    min_deck_size: usize,
) -> HashMap<String, u8> {
    let total_lands = min_deck_size
        .saturating_sub(spell_names.len())
        .clamp(16, 18) as u8;
    distribute_lands(spell_names, pool, total_lands)
}

fn target_spell_count(min_deck_size: usize) -> usize {
    ((min_deck_size * DEFAULT_SPELLS) / DEFAULT_DECK_SIZE).max(1)
}

fn suggest_addable_cards(
    spell_names: &[String],
    pool: &[DraftCardInstance],
    total: u8,
    addable_cards: &DeckAddableCards,
) -> HashMap<String, u8> {
    if total == 0 {
        return HashMap::new();
    }
    if matches!(
        addable_cards.policy,
        DeckAddableCardPolicy::StandardBasics | DeckAddableCardPolicy::StandardBasicsPlusCustom
    ) {
        return distribute_lands(spell_names, pool, total);
    }
    let mut result = HashMap::new();
    if let Some(card) = addable_cards.custom.first() {
        result.insert(card.clone(), total);
    }
    result
}

/// Distribute exactly `total_lands` basics proportional to the colored-mana
/// pips of the selected spells.
fn distribute_lands(
    spell_names: &[String],
    pool: &[DraftCardInstance],
    total_lands: u8,
) -> HashMap<String, u8> {
    // Build name -> card lookup from pool
    let card_by_name: HashMap<&str, &DraftCardInstance> =
        pool.iter().map(|c| (c.name.as_str(), c)).collect();

    // Count color pip occurrences from the selected spells
    let mut color_counts: HashMap<&str, u32> = HashMap::new();
    for name in spell_names {
        if let Some(card) = card_by_name.get(name.as_str()) {
            for color in &card.colors {
                let key = match color.as_str() {
                    "W" => "W",
                    "U" => "U",
                    "B" => "B",
                    "R" => "R",
                    "G" => "G",
                    _ => continue,
                };
                *color_counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    let mut lands: HashMap<String, u8> = HashMap::new();

    if color_counts.is_empty() {
        // No color info — split evenly across the five basics.
        let base = total_lands / 5;
        let extra = total_lands % 5;
        for (i, land) in ["Plains", "Island", "Swamp", "Mountain", "Forest"]
            .into_iter()
            .enumerate()
        {
            lands.insert(land.to_string(), base + u8::from((i as u8) < extra));
        }
        return lands;
    }

    let total_pips: u32 = color_counts.values().sum();
    let mut assigned: u8 = 0;

    // Sort colors by count descending for stable assignment
    let mut sorted_colors: Vec<(&&str, &u32)> = color_counts.iter().collect();
    sorted_colors.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    for (i, (color, count)) in sorted_colors.iter().enumerate() {
        let land_name = color_to_land(color);
        let share = if i == sorted_colors.len() - 1 {
            // Last color gets the remainder so the basics sum to total_lands.
            total_lands - assigned
        } else {
            let remaining_colors = sorted_colors.len() - i - 1;
            let raw = ((**count as f64 / total_pips as f64) * total_lands as f64).round() as u8;
            // Minimum 1 land of any represented color, max leaves room for remaining
            raw.max(1)
                .min(total_lands - assigned - remaining_colors as u8)
        };
        lands.insert(land_name.to_string(), share);
        assigned += share;
    }

    lands
}

fn color_to_land(color: &str) -> &'static str {
    match color {
        "W" => "Plains",
        "U" => "Island",
        "B" => "Swamp",
        "R" => "Mountain",
        "G" => "Forest",
        _ => "Wastes",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(name: &str, colors: &[&str], cmc: u8, type_line: &str) -> DraftCardInstance {
        DraftCardInstance {
            instance_id: format!("id-{name}"),
            name: name.to_string(),
            set_code: "TST".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: colors.iter().map(|s| s.to_string()).collect(),
            cmc,
            type_line: type_line.to_string(),
        }
    }

    /// Card DB with four W/U creatures plus an on-color (Plains/Island) and an
    /// off-color (Swamp/Mountain) typed dual. The duals carry no `Effect::Mana`;
    /// their produced colors come from the basic land subtypes (true-dual shape).
    fn fixture_db() -> CardDatabase {
        let creature = |name: &str| {
            format!(
                r#""{name}": {{ "name": "{name}", "mana_cost": {{ "type": "NoCost" }},
                "card_type": {{ "supertypes": [], "core_types": ["Creature"], "subtypes": [] }},
                "power": "2", "toughness": "2", "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [] }}"#
            )
        };
        let dual = |name: &str, a: &str, b: &str| {
            format!(
                r#""{name}": {{ "name": "{name}", "mana_cost": {{ "type": "NoCost" }},
                "card_type": {{ "supertypes": [], "core_types": ["Land"], "subtypes": ["{a}", "{b}"] }},
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [] }}"#
            )
        };
        let json = format!(
            "{{ {}, {}, {}, {}, {}, {} }}",
            creature("White Bear"),
            creature("White Knight"),
            creature("Blue Bird"),
            creature("Blue Wizard"),
            dual("On Color Dual", "Plains", "Island"),
            dual("Off Color Dual", "Swamp", "Mountain"),
        );
        CardDatabase::from_json_str(&json).unwrap()
    }

    fn wu_pool() -> Vec<DraftCardInstance> {
        vec![
            instance("White Bear", &["W"], 2, "Creature — Bear"),
            instance("White Knight", &["W"], 2, "Creature — Knight"),
            instance("Blue Bird", &["U"], 1, "Creature — Bird"),
            instance("Blue Wizard", &["U"], 3, "Creature — Wizard"),
            instance("On Color Dual", &[], 0, "Land — Plains Island"),
            instance("Off Color Dual", &[], 0, "Land — Swamp Mountain"),
        ]
    }

    #[test]
    fn admits_on_color_fixing_land_and_rejects_off_color() {
        let db = fixture_db();
        let deck = suggest_deck(
            &wu_pool(),
            AiDifficulty::Medium,
            Some(&db),
            8,
            &DeckAddableCards::standard_basics(),
        );
        assert!(
            deck.lands.contains_key("On Color Dual"),
            "on-color (W/U) fixing land should be admitted to the manabase, got {:?}",
            deck.lands
        );
        assert!(
            !deck.lands.contains_key("Off Color Dual"),
            "off-color (B/R) fixing land must not be admitted, got {:?}",
            deck.lands
        );
    }

    #[test]
    fn admitting_nonbasics_keeps_deck_total_exact() {
        let db = fixture_db();
        let deck = suggest_deck(
            &wu_pool(),
            AiDifficulty::Medium,
            Some(&db),
            8,
            &DeckAddableCards::standard_basics(),
        );
        let land_count: u32 = deck.lands.values().map(|&c| c as u32).sum();
        // Each admitted nonbasic replaces one basic — the total never drifts.
        assert_eq!(
            deck.main_deck.len() as u32 + land_count,
            8,
            "spells + lands must equal min_deck_size; lands = {:?}",
            deck.lands
        );
    }

    #[test]
    fn no_card_db_admits_no_nonbasics() {
        // Without a card DB the produced colors are unknown, so no nonbasic is
        // admitted (the manabase falls back to basics only).
        let deck = suggest_deck(
            &wu_pool(),
            AiDifficulty::Medium,
            None,
            8,
            &DeckAddableCards::standard_basics(),
        );
        assert!(!deck.lands.contains_key("On Color Dual"));
    }
}
