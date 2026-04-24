use std::collections::HashMap;

use engine::game::DeckEntry;
use engine::types::ability::{AbilityCost, Effect};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Pre-computed synergy scores for each card in a deck.
///
/// Built once per game from `DeckEntry` data. Each card gets a score
/// representing how well it synergizes with the rest of the deck.
#[derive(Debug, Clone)]
pub struct SynergyGraph {
    scores: HashMap<String, f64>,
}

impl SynergyGraph {
    /// Analyze a deck's synergy relationships across all axes.
    pub fn build(deck: &[DeckEntry]) -> Self {
        if deck.is_empty() {
            return Self::empty();
        }

        let mut scores: HashMap<String, f64> = HashMap::new();

        // Detect each axis independently, accumulate into per-card scores
        detect_tribal(deck, &mut scores);
        detect_sacrifice(deck, &mut scores);
        detect_graveyard(deck, &mut scores);
        detect_spellcast(deck, &mut scores);

        Self { scores }
    }

    /// Empty synergy graph — all scores are zero.
    pub fn empty() -> Self {
        Self {
            scores: HashMap::new(),
        }
    }

    /// Look up the pre-computed synergy score for a card by name.
    /// Returns 0.0 if the card has no detected synergy.
    pub fn card_score(&self, name: &str) -> f64 {
        self.scores.get(name).copied().unwrap_or(0.0)
    }

    /// Sum synergy scores for cards currently on the player's battlefield,
    /// weighted by how many synergy partners are co-present.
    pub fn board_synergy_bonus(&self, state: &GameState, player: PlayerId) -> f64 {
        let battlefield_names: Vec<&str> = state
            .battlefield
            .iter()
            .filter_map(|&id| state.objects.get(&id))
            .filter(|obj| obj.controller == player && obj.zone == Zone::Battlefield)
            .map(|obj| obj.name.as_str())
            .collect();

        if battlefield_names.is_empty() {
            return 0.0;
        }

        // Each card's synergy score contributes proportionally to how many
        // partners are present (diminishing returns via sqrt scaling).
        let partner_factor = (battlefield_names.len() as f64).sqrt();
        battlefield_names
            .iter()
            .map(|&name| self.scores.get(name).copied().unwrap_or(0.0))
            .sum::<f64>()
            * partner_factor
            / battlefield_names.len() as f64
    }
}

impl Default for SynergyGraph {
    fn default() -> Self {
        Self::empty()
    }
}

// ─── Axis detection ──────────────────────────────────────────────────

/// Tribal: count creature subtypes. Types with ≥3 cards get synergy points.
fn detect_tribal(deck: &[DeckEntry], scores: &mut HashMap<String, f64>) {
    let mut type_counts: HashMap<&str, u32> = HashMap::new();
    let mut card_types: HashMap<&str, Vec<&str>> = HashMap::new();

    for entry in deck {
        if !entry
            .card
            .card_type
            .core_types
            .contains(&CoreType::Creature)
        {
            continue;
        }
        // Changeling counts for all types — we'll handle it below
        let is_changeling = entry.card.keywords.contains(&Keyword::Changeling);
        for subtype in &entry.card.card_type.subtypes {
            *type_counts.entry(subtype.as_str()).or_insert(0) += entry.count;
            card_types
                .entry(entry.card.name.as_str())
                .or_default()
                .push(subtype.as_str());
        }
        if is_changeling {
            card_types
                .entry(entry.card.name.as_str())
                .or_default()
                .push("__changeling__");
        }
    }

    // Find types with ≥3 cards (tribal threshold)
    let tribal_types: Vec<&str> = type_counts
        .iter()
        .filter(|(_, &count)| count >= 3)
        .map(|(&t, _)| t)
        .collect();

    if tribal_types.is_empty() {
        return;
    }

    for entry in deck {
        let name = entry.card.name.as_str();
        let card_subtypes = card_types.get(name);
        let has_tribal = card_subtypes.is_some_and(|types| {
            types.contains(&"__changeling__") || types.iter().any(|t| tribal_types.contains(t))
        });
        if has_tribal {
            *scores.entry(name.to_string()).or_insert(0.0) += 0.3;
        }
    }
}

/// Sacrifice: cards with sacrifice costs synergize with token producers.
fn detect_sacrifice(deck: &[DeckEntry], scores: &mut HashMap<String, f64>) {
    let mut sac_outlets = Vec::new();
    let mut token_producers = Vec::new();

    for entry in deck {
        if has_sacrifice_cost(&entry.card) {
            sac_outlets.push(entry.card.name.as_str());
        }
        if produces_tokens(&entry.card) {
            token_producers.push(entry.card.name.as_str());
        }
    }

    if sac_outlets.is_empty() || token_producers.is_empty() {
        return;
    }

    let bonus = 0.25;
    for name in sac_outlets {
        *scores.entry(name.to_string()).or_insert(0.0) += bonus;
    }
    for name in token_producers {
        *scores.entry(name.to_string()).or_insert(0.0) += bonus;
    }
}

/// Graveyard: mill/discard/surveil synergize with recursion keywords.
fn detect_graveyard(deck: &[DeckEntry], scores: &mut HashMap<String, f64>) {
    let mut fillers = Vec::new();
    let mut recursion = Vec::new();

    for entry in deck {
        if fills_graveyard(&entry.card) {
            fillers.push(entry.card.name.as_str());
        }
        if has_recursion(&entry.card) {
            recursion.push(entry.card.name.as_str());
        }
    }

    if fillers.is_empty() || recursion.is_empty() {
        return;
    }

    let bonus = 0.3;
    for name in fillers {
        *scores.entry(name.to_string()).or_insert(0.0) += bonus;
    }
    for name in recursion {
        *scores.entry(name.to_string()).or_insert(0.0) += bonus;
    }
}

/// Spellcast: instant/sorcery density synergizes with spellcast triggers.
fn detect_spellcast(deck: &[DeckEntry], scores: &mut HashMap<String, f64>) {
    let mut spell_count = 0u32;
    let mut trigger_cards = Vec::new();

    for entry in deck {
        if entry.card.card_type.core_types.contains(&CoreType::Instant)
            || entry.card.card_type.core_types.contains(&CoreType::Sorcery)
        {
            spell_count += entry.count;
        }
        if has_spellcast_trigger(&entry.card) {
            trigger_cards.push(entry.card.name.as_str());
        }
    }

    // Need meaningful spell density (≥8) and at least one trigger to matter
    if spell_count < 8 || trigger_cards.is_empty() {
        return;
    }

    let density_bonus = (spell_count as f64 / 20.0).min(1.0) * 0.4;
    for name in trigger_cards {
        *scores.entry(name.to_string()).or_insert(0.0) += density_bonus;
    }
}

// ─── Card property detectors ─────────────────────────────────────────

fn has_sacrifice_cost(card: &CardFace) -> bool {
    card.abilities.iter().any(|ability| {
        ability
            .cost
            .as_ref()
            .is_some_and(|c| matches!(c, AbilityCost::Sacrifice { .. }))
    })
}

fn produces_tokens(card: &CardFace) -> bool {
    card.abilities
        .iter()
        .any(|ability| matches!(&*ability.effect, Effect::Token { .. }))
}

fn fills_graveyard(card: &CardFace) -> bool {
    card.abilities.iter().any(|ability| {
        matches!(
            &*ability.effect,
            Effect::Mill { .. } | Effect::DiscardCard { .. } | Effect::Surveil { .. }
        )
    })
}

fn has_recursion(card: &CardFace) -> bool {
    card.keywords.iter().any(|kw| {
        matches!(
            kw,
            Keyword::Flashback(..) | Keyword::Escape { .. } | Keyword::Unearth(..)
        )
    })
}

fn has_spellcast_trigger(card: &CardFace) -> bool {
    card.triggers.iter().any(|trigger| {
        matches!(
            trigger.mode,
            TriggerMode::SpellCast | TriggerMode::SpellCastOrCopy
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, PtValue, QuantityExpr, TargetFilter, TriggerDefinition,
    };
    use engine::types::card_type::CardType;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;

    fn creature_entry(name: &str, subtypes: Vec<&str>) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                card_type: CardType {
                    core_types: vec![CoreType::Creature],
                    subtypes: subtypes.into_iter().map(String::from).collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
            count: 4,
        }
    }

    fn sac_outlet_entry(name: &str) -> DeckEntry {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        ability.cost = Some(AbilityCost::Sacrifice {
            target: TargetFilter::Any,
            count: 1,
        });
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                card_type: CardType {
                    core_types: vec![CoreType::Creature],
                    ..Default::default()
                },
                abilities: vec![ability],
                ..Default::default()
            },
            count: 4,
        }
    }

    fn token_producer_entry(name: &str) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                card_type: CardType {
                    core_types: vec![CoreType::Sorcery],
                    ..Default::default()
                },
                abilities: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Token {
                        name: "Goblin".to_string(),
                        count: QuantityExpr::Fixed { value: 2 },
                        power: PtValue::Fixed(1),
                        toughness: PtValue::Fixed(1),
                        colors: Vec::new(),
                        types: Vec::new(),
                        keywords: Vec::new(),
                        tapped: false,
                        owner: TargetFilter::Controller,
                        attach_to: None,
                        enters_attacking: false,
                        supertypes: vec![],
                        static_abilities: vec![],
                        enter_with_counters: vec![],
                    },
                )],
                ..Default::default()
            },
            count: 4,
        }
    }

    fn spell_entry(name: &str) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                card_type: CardType {
                    core_types: vec![CoreType::Instant],
                    ..Default::default()
                },
                mana_cost: ManaCost::generic(1),
                ..Default::default()
            },
            count: 4,
        }
    }

    fn spellcast_trigger_entry(name: &str) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                card_type: CardType {
                    core_types: vec![CoreType::Creature],
                    ..Default::default()
                },
                triggers: vec![TriggerDefinition::new(TriggerMode::SpellCast)],
                ..Default::default()
            },
            count: 2,
        }
    }

    #[test]
    fn tribal_synergy_detected() {
        // 8+ Elves should produce tribal synergy
        let deck = vec![
            creature_entry("Llanowar Elves", vec!["Elf"]),
            creature_entry("Elvish Mystic", vec!["Elf"]),
            creature_entry("Elvish Archdruid", vec!["Elf"]),
        ];
        let graph = SynergyGraph::build(&deck);
        assert!(
            graph.scores.values().any(|&s| s > 0.0),
            "Tribal deck should have synergy scores"
        );
    }

    #[test]
    fn sacrifice_synergy_detected() {
        let deck = vec![
            sac_outlet_entry("Viscera Seer"),
            token_producer_entry("Dragon Fodder"),
        ];
        let graph = SynergyGraph::build(&deck);
        let seer_score = graph.scores.get("Viscera Seer").copied().unwrap_or(0.0);
        let fodder_score = graph.scores.get("Dragon Fodder").copied().unwrap_or(0.0);
        assert!(
            seer_score > 0.0 && fodder_score > 0.0,
            "Sac outlet + token producer should have positive synergy: seer={seer_score}, fodder={fodder_score}"
        );
    }

    #[test]
    fn spellcast_synergy_detected() {
        // 10+ instants/sorceries + spellcast trigger creature
        let deck = vec![
            spell_entry("Lightning Bolt"),
            spell_entry("Opt"),
            spell_entry("Counterspell"),
            spellcast_trigger_entry("Young Pyromancer"),
        ];
        let graph = SynergyGraph::build(&deck);
        let pyro_score = graph.scores.get("Young Pyromancer").copied().unwrap_or(0.0);
        assert!(
            pyro_score > 0.0,
            "Spellcast trigger in spell-heavy deck should have synergy: {pyro_score}"
        );
    }

    #[test]
    fn no_synergy_in_random_deck() {
        // Each creature type has only 1 copy (count=1) — below tribal threshold.
        let deck = vec![
            DeckEntry {
                card: CardFace {
                    name: "Grizzly Bears".to_string(),
                    card_type: CardType {
                        core_types: vec![CoreType::Creature],
                        subtypes: vec!["Bear".to_string()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                count: 1,
            },
            DeckEntry {
                card: CardFace {
                    name: "Hill Giant".to_string(),
                    card_type: CardType {
                        core_types: vec![CoreType::Creature],
                        subtypes: vec!["Giant".to_string()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                count: 1,
            },
            DeckEntry {
                card: CardFace {
                    name: "Shock".to_string(),
                    card_type: CardType {
                        core_types: vec![CoreType::Instant],
                        ..Default::default()
                    },
                    mana_cost: ManaCost::generic(1),
                    ..Default::default()
                },
                count: 2,
            },
        ];
        let graph = SynergyGraph::build(&deck);
        let total: f64 = graph.scores.values().sum();
        assert!(
            total < 0.01,
            "Random deck should have near-zero synergy: {total}"
        );
    }

    #[test]
    fn board_bonus_scales_with_partners() {
        let deck = vec![
            creature_entry("Llanowar Elves", vec!["Elf"]),
            creature_entry("Elvish Mystic", vec!["Elf"]),
            creature_entry("Elvish Archdruid", vec!["Elf"]),
        ];
        let graph = SynergyGraph::build(&deck);

        // State with 1 elf on battlefield
        let mut state = GameState::new_two_player(42);
        let id1 = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id1).unwrap().controller = PlayerId(0);
        let bonus_1 = graph.board_synergy_bonus(&state, PlayerId(0));

        // Add a second elf
        let id2 = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Elvish Mystic".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id2).unwrap().controller = PlayerId(0);
        let bonus_2 = graph.board_synergy_bonus(&state, PlayerId(0));

        assert!(
            bonus_2 > bonus_1,
            "More synergy partners should increase bonus: {bonus_2} > {bonus_1}"
        );
    }
}
