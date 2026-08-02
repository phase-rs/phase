use engine::game::combat;
use engine::game::mana_abilities;
use engine::game::players;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use crate::deck_profile::DeckArchetype;

/// Zone-quality weights tuned per archetype.
///
/// `pub(crate)` so `eval`'s mana-development floor test can recompute the true
/// land-drop cost from the live weight table rather than from literals.
pub(crate) struct ZoneWeights {
    pub(crate) hand_card_base: f64,
    pub(crate) castable_bonus: f64,
    graveyard_base: f64,
    recursion_bonus: f64,
}

impl ZoneWeights {
    pub(crate) fn for_archetype(archetype: DeckArchetype) -> Self {
        match archetype {
            DeckArchetype::Aggro => Self {
                hand_card_base: 0.3,
                castable_bonus: 0.4,
                graveyard_base: 0.05,
                recursion_bonus: 0.1,
            },
            DeckArchetype::Midrange => Self {
                hand_card_base: 0.4,
                castable_bonus: 0.3,
                graveyard_base: 0.1,
                recursion_bonus: 0.2,
            },
            DeckArchetype::Control => Self {
                hand_card_base: 0.6,
                castable_bonus: 0.2,
                graveyard_base: 0.15,
                recursion_bonus: 0.3,
            },
            DeckArchetype::Combo => Self {
                hand_card_base: 0.5,
                castable_bonus: 0.3,
                graveyard_base: 0.2,
                recursion_bonus: 0.4,
            },
            DeckArchetype::Ramp => Self {
                hand_card_base: 0.4,
                castable_bonus: 0.3,
                graveyard_base: 0.1,
                recursion_bonus: 0.2,
            },
        }
    }
}

/// Compute zone-quality differential between `player` and their strongest opponent.
///
/// Returns the player's (hand quality + graveyard value) minus the best opponent's
/// equivalent, using archetype-tuned weights. Positive means the player has better
/// zone quality.
pub fn zone_bonus(state: &GameState, player: PlayerId, archetype: DeckArchetype) -> f64 {
    let weights = ZoneWeights::for_archetype(archetype);
    let my_score = player_zone_score(state, player, &weights);
    let opponents = players::opponents(state, player);
    if opponents.is_empty() {
        return my_score;
    }
    let max_opp = opponents
        .iter()
        .map(|&opp| player_zone_score(state, opp, &weights))
        .fold(f64::NEG_INFINITY, f64::max);
    my_score - max_opp
}

/// Raw zone score for a single player (hand quality + graveyard value).
fn player_zone_score(state: &GameState, player: PlayerId, weights: &ZoneWeights) -> f64 {
    let available = available_mana(state, player);
    hand_quality(state, player, available, weights) + graveyard_value(state, player, weights)
}

/// Evaluate hand quality: each card gets a base value plus a bonus if castable this turn.
fn hand_quality(
    state: &GameState,
    player: PlayerId,
    available_mana: u32,
    weights: &ZoneWeights,
) -> f64 {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter_map(|&oid| state.objects.get(&oid))
        .map(|obj| {
            let base = weights.hand_card_base;
            let castable = if obj.mana_cost.mana_value() <= available_mana {
                weights.castable_bonus
            } else {
                0.0
            };
            base + castable
        })
        .sum()
}

/// Evaluate graveyard value: each card gets a base value, recursion-capable cards get a bonus.
fn graveyard_value(state: &GameState, player: PlayerId, weights: &ZoneWeights) -> f64 {
    state.players[player.0 as usize]
        .graveyard
        .iter()
        .filter_map(|&oid| state.objects.get(&oid))
        .map(|obj| {
            let base = weights.graveyard_base;
            let recursion = if has_recursion_keyword(obj) {
                weights.recursion_bonus
            } else {
                0.0
            };
            base + recursion
        })
        .sum()
}

/// Check if a game object has any graveyard-recursion keyword.
pub(crate) fn has_recursion_keyword(obj: &engine::game::game_object::GameObject) -> bool {
    obj.keywords.iter().any(|kw| {
        matches!(
            kw,
            Keyword::Flashback(..) | Keyword::Escape(_) | Keyword::Unearth(..)
        )
    })
}

/// CR 106.1 + CR 605.1a + CR 701.21: a permanent that is part of `player`'s
/// standing manabase — a land, or any permanent carrying at least one
/// *renewable* mana ability (mana rocks, mana creatures).
///
/// **Intrinsic**: deliberately ignores tapped state, summoning sickness, and
/// activation gating. This is a *development* predicate and must not collapse
/// when the player taps out — its delta on a tap-out event is exactly zero, which
/// is why it does not compete with [`available_mana`].
///
/// **Renewable**: one-shot self-sacrificing sources (Treasure, Gold, Lotus Petal)
/// are excluded via [`mana_abilities::is_renewable_mana_ability`]. Counting them
/// would price cracking a Treasure at a full source's worth, forever.
/// [`available_mana`] counts them and is correct to — availability and
/// development are different questions and must not share a predicate.
///
/// **Naming collision — do NOT compose this with
/// `search::object_has_no_development_source`.** Despite the shared phrase the two
/// are near-opposites, not complements. That helper asks "is this permanent inert
/// for the large-board pass shortcut?" and answers **true** for a permanent whose
/// abilities are *all* mana abilities — so a Sol Ring "has no development source"
/// there while being an intrinsic mana source here. Treating one as `!` of the
/// other silently inverts the pass shortcut.
pub(crate) fn is_intrinsic_mana_source(obj: &engine::game::game_object::GameObject) -> bool {
    obj.card_types.core_types.contains(&CoreType::Land)
        || obj
            .abilities
            .iter()
            .any(mana_abilities::is_renewable_mana_ability)
}

/// Count untapped mana sources controlled by the player plus mana already in their pool.
/// Includes lands and non-land permanents with mana abilities (dorks, mana rocks).
/// CR 302.6: Creatures with summoning sickness cannot activate tap abilities,
/// so sick non-land mana dorks are excluded.
pub(crate) fn available_mana(state: &GameState, player: PlayerId) -> u32 {
    let untapped_mana_sources = state
        .battlefield
        .iter()
        .filter(|&&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player
                    && !obj.tapped
                    && (obj.card_types.core_types.contains(&CoreType::Land)
                        || (!combat::has_summoning_sickness(obj)
                            && obj.abilities.iter().any(mana_abilities::is_mana_ability)))
            })
        })
        .count();
    let pool_mana = state.players[player.0 as usize].mana_pool.total();
    (untapped_mana_sources + pool_mana) as u32
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn castable_cards_worth_more() {
        let mut state = make_state();
        // Add 2 untapped lands → 2 available mana
        for i in 0..2 {
            let id = create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.controller = PlayerId(0);
        }

        // Hand with a 2-drop (castable)
        let cheap_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&cheap_id).unwrap().mana_cost = ManaCost::generic(2);

        let score_cheap = zone_bonus(&state, PlayerId(0), DeckArchetype::Midrange);

        // Replace with a 5-drop (not castable)
        state.objects.get_mut(&cheap_id).unwrap().mana_cost = ManaCost::generic(5);
        let score_expensive = zone_bonus(&state, PlayerId(0), DeckArchetype::Midrange);

        assert!(
            score_cheap > score_expensive,
            "Castable card should score higher: {score_cheap} > {score_expensive}"
        );
    }

    #[test]
    fn graveyard_flashback_bonus() {
        let mut state = make_state();

        // Card in graveyard without recursion
        let plain_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Bolt".to_string(),
            Zone::Graveyard,
        );
        let score_plain = zone_bonus(&state, PlayerId(0), DeckArchetype::Midrange);

        // Add flashback keyword
        state
            .objects
            .get_mut(&plain_id)
            .unwrap()
            .keywords
            .push(Keyword::Flashback(
                engine::types::keywords::FlashbackCost::Mana(ManaCost::generic(3)),
            ));
        let score_flashback = zone_bonus(&state, PlayerId(0), DeckArchetype::Midrange);

        assert!(
            score_flashback > score_plain,
            "Flashback card should score higher: {score_flashback} > {score_plain}"
        );
    }

    #[test]
    fn available_mana_counts_creature_mana_dorks() {
        use engine::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        };
        use engine::types::mana::ManaColor;

        let mut state = make_state();
        state.turn_number = 2; // advance past turn 0 so creatures can lose sickness

        // Add 1 untapped land
        let land_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let land_obj = state.objects.get_mut(&land_id).unwrap();
        land_obj.card_types.core_types.push(CoreType::Land);

        // Add 1 untapped creature with a mana ability (Llanowar Elves)
        let dork_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let dork_obj = state.objects.get_mut(&dork_id).unwrap();
        dork_obj.card_types.core_types.push(CoreType::Creature);
        dork_obj.power = Some(1);
        dork_obj.toughness = Some(1);
        // Played on a previous turn — no summoning sickness.
        dork_obj.entered_battlefield_turn = Some(0);
        let mut mana_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        mana_ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut dork_obj.abilities).push(mana_ability);

        // Should count both: 1 land + 1 mana dork = 2
        let mana = available_mana(&state, PlayerId(0));
        assert_eq!(mana, 2, "Should count both land and creature mana dork");
    }

    #[test]
    fn control_weights_value_hand_more() {
        let mut state = make_state();
        let id = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(5);

        let control_score = zone_bonus(&state, PlayerId(0), DeckArchetype::Control);
        let aggro_score = zone_bonus(&state, PlayerId(0), DeckArchetype::Aggro);

        assert!(
            control_score > aggro_score,
            "Control should value hand cards more: {control_score} > {aggro_score}"
        );
    }

    /// Row 7's discriminator, and the executable form of the availability /
    /// development split: an untapped **Treasure** raises `available_mana` by 1
    /// (it genuinely is one mana right now) while contributing **0** to
    /// development (cracking it must not read as losing a mana source).
    ///
    /// A shared predicate — the round-2 design — makes these two numbers move
    /// together and is what this test exists to forbid.
    #[test]
    fn treasure_is_available_mana_but_not_a_development_source() {
        use engine::game::effects::token::predefined_token_abilities;

        let mut state = make_state();
        let id = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).extend(predefined_token_abilities("Treasure"));

        assert_eq!(
            available_mana(&state, PlayerId(0)),
            1,
            "an untapped Treasure IS one mana available right now"
        );
        assert!(
            !is_intrinsic_mana_source(&state.objects[&id]),
            "…but it is a one-shot conversion, not standing manabase development"
        );
    }

    /// Row 5's negatives, free from CR 605.1a via `is_mana_ability`'s own guards:
    /// a targeted mana ability and a loyalty-cost mana ability are not mana
    /// abilities at all, so neither is a development source.
    #[test]
    fn targeted_and_loyalty_mana_abilities_are_not_development_sources() {
        use engine::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, ManaTargetRole,
            QuantityExpr, TargetFilter,
        };

        let mana_effect = || Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };

        let mut state = make_state();

        // Loyalty-cost mana ability (Chandra-shaped): CR 605.1a excludes it.
        let loyalty_id = create_object(
            &mut state,
            CardId(800),
            PlayerId(0),
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&loyalty_id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        let mut loyalty = AbilityDefinition::new(AbilityKind::Activated, mana_effect());
        loyalty.cost = Some(AbilityCost::Loyalty { amount: 1 });
        Arc::make_mut(&mut obj.abilities).push(loyalty);
        assert!(!is_intrinsic_mana_source(&state.objects[&loyalty_id]));

        // Targeted mana ability (Jeska's-Will-shaped): CR 605.1a excludes it.
        let targeted_id = create_object(
            &mut state,
            CardId(801),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&targeted_id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut targeted = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: Some(ManaTargetRole::Recipient {
                    recipient: TargetFilter::Player,
                }),
            },
        );
        targeted.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(targeted);
        assert!(!is_intrinsic_mana_source(&state.objects[&targeted_id]));

        // Positive control: the same shape WITHOUT the target is a source, so the
        // two negatives above are not passing because the fixture is inert.
        let ok_id = create_object(
            &mut state,
            CardId(802),
            PlayerId(0),
            "Signet".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ok_id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut plain = AbilityDefinition::new(AbilityKind::Activated, mana_effect());
        plain.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(plain);
        assert!(is_intrinsic_mana_source(&state.objects[&ok_id]));
    }
}
