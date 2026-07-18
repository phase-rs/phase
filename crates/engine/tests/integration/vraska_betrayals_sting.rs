//! Vraska, Betrayal's Sting — the two previously-`Unimplemented` loyalty
//! abilities.
//!
//! Oracle (verbatim, Scryfall):
//!   0: You draw a card and lose 1 life. Proliferate.
//!   −2: Target creature becomes a Treasure artifact with "{T}, Sacrifice this
//!       artifact: Add one mana of any color" and loses all other card types and
//!       abilities.
//!   −9: If target player has fewer than nine poison counters, they get a number
//!       of poison counters equal to the difference.
//!
//! The [−9] rider (CR 122.1 + CR 122.1f + CR 107.1b) tops the chosen target
//! player up to nine poison via `GivePlayerCounter` with a `max(0, 9 − current)`
//! count. The [−2] animation (CR 205.1a + CR 613.1d + CR 613.1f + CR 613.8a)
//! replaces the target creature's card-type set with Artifact, makes it a
//! Treasure, wipes all abilities, and grants the sacrifice-for-mana ability.
//!
//! These tests drive the REAL production path — `handle_activate_loyalty` via the
//! `GameRunner` fluent activation — then read the post-resolution / post-layer
//! state. Not AST-shape tests.

use std::sync::Arc;

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCost, AbilityKind, Effect, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};

const VRASKA_ORACLE: &str = "0: You draw a card and lose 1 life. Proliferate.\n\
\u{2212}2: Target creature becomes a Treasure artifact with \"{T}, Sacrifice this artifact: Add one mana of any color\" and loses all other card types and abilities.\n\
\u{2212}9: If target player has fewer than nine poison counters, they get a number of poison counters equal to the difference.";

/// Verbatim [−9] effect body (loyalty prefix stripped) for the parser-shape reach guards.
const MINUS_NINE_EFFECT: &str =
    "If target player has fewer than nine poison counters, they get a number of poison counters equal to the difference.";

fn parsed_vraska() -> ParsedAbilities {
    parse_oracle_text(
        VRASKA_ORACLE,
        "Vraska, Betrayal's Sting",
        &[],
        &["Legendary".to_string()],
        &["Vraska".to_string()],
    )
}

/// Locate a loyalty ability by its exact loyalty cost.
fn loyalty_index(parsed: &ParsedAbilities, amount: i32) -> usize {
    parsed
        .abilities
        .iter()
        .position(|a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: amt }) if amt == amount))
        .unwrap_or_else(|| panic!("Vraska must parse a [{amount}] loyalty ability"))
}

fn wire_vraska(
    state: &mut engine::types::game_state::GameState,
    vraska: ObjectId,
    loyalty: u32,
    parsed: &ParsedAbilities,
) {
    let obj = state.objects.get_mut(&vraska).expect("vraska");
    obj.card_types.core_types = vec![CoreType::Planeswalker];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
    obj.abilities = Arc::new(parsed.abilities.clone());
    obj.base_abilities = Arc::new(parsed.abilities.clone());
}

fn poison_of(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> u32 {
    runner.state().players[player.0 as usize].poison_counters
}

// ---------------------------------------------------------------------------
// [−9] threshold poison top-up — runtime via handle_activate_loyalty.
// ---------------------------------------------------------------------------

/// CR 122.1 + CR 107.1b: a target with 4 poison gains max(0, 9−4)=5 → 9.
#[test]
fn vraska_minus9_tops_target_to_nine_poison() {
    let parsed = parsed_vraska();
    let index = loyalty_index(&parsed, -9);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vraska = scenario
        .add_creature(P0, "Vraska, Betrayal's Sting", 0, 0)
        .id();
    let mut runner = scenario.build();
    wire_vraska(runner.state_mut(), vraska, 9, &parsed);
    runner.state_mut().players[P1.0 as usize].poison_counters = 4;

    runner.activate(vraska, index).target_player(P1).resolve();

    // Reverting the rider (Unimplemented) or hardcoding N drops this to 4 or a
    // wrong value.
    assert_eq!(
        poison_of(&runner, P1),
        9,
        "4 + max(0, 9 − 4) = 5 must top the target to nine poison"
    );
}

/// CR 107.1b: a target already at the nine-poison threshold → ClampMin{0} makes
/// the count zero, and the resolver no-ops at amount 0. Nine (not ten) is used so
/// the fixture stays below the CR 704.5c ten-poison loss SBA while still
/// exercising the exact `9 − 9 = 0` clamp boundary.
#[test]
fn vraska_minus9_no_op_when_target_already_poisoned() {
    let parsed = parsed_vraska();
    let index = loyalty_index(&parsed, -9);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vraska = scenario
        .add_creature(P0, "Vraska, Betrayal's Sting", 0, 0)
        .id();
    let mut runner = scenario.build();
    wire_vraska(runner.state_mut(), vraska, 9, &parsed);
    runner.state_mut().players[P1.0 as usize].poison_counters = 9;

    runner.activate(vraska, index).target_player(P1).resolve();

    // Reach guard (non-vacuous): the ability actually resolved — all 9 loyalty
    // counters were paid (CR 606.4). The loyalty counter is the CR 306.5b
    // authoritative store; it is removed once it hits 0. A failed activation would
    // leave nine loyalty counters intact.
    let loyalty_counters = runner
        .state()
        .objects
        .get(&vraska)
        .unwrap()
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        loyalty_counters, 0,
        "[−9] must pay all 9 loyalty (proves the ability resolved, so the no-op is genuine)"
    );
    // CR 107.1b: 9 − 9 = 0 → no counters added.
    assert_eq!(
        poison_of(&runner, P1),
        9,
        "a target already at the nine-poison threshold must be unchanged"
    );
}

/// CR 115.1 + CR 608.2c: the count reads the chosen TARGET player's poison via
/// `parent_target_controller`, NOT the caster's. Hostile multi-authority fixture:
/// caster at 9 poison, target at 2.
#[test]
fn vraska_minus9_reads_target_not_caster() {
    let parsed = parsed_vraska();
    let index = loyalty_index(&parsed, -9);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vraska = scenario
        .add_creature(P0, "Vraska, Betrayal's Sting", 0, 0)
        .id();
    let mut runner = scenario.build();
    wire_vraska(runner.state_mut(), vraska, 9, &parsed);
    // Distinct authorities: caster (P0) 9 poison, target (P1) 2 poison.
    runner.state_mut().players[P0.0 as usize].poison_counters = 9;
    runner.state_mut().players[P1.0 as usize].poison_counters = 2;

    runner.activate(vraska, index).target_player(P1).resolve();

    // Target read (2): 2 + max(0, 9 − 2) = 9. If the count read the caster (9),
    // the delta would be 0 and P1 would stay at 2.
    assert_eq!(
        poison_of(&runner, P1),
        9,
        "count must read the target's 2 poison → +7 → 9"
    );
    // The caster's own poison is untouched by the effect.
    assert_eq!(
        poison_of(&runner, P0),
        9,
        "caster poison must be unchanged (count binds to the target, not the caster)"
    );
}

/// Parser reach guard: the whole [−9] sentence is captured by the rider BEFORE
/// `split_leading_conditional` as one `GivePlayerCounter` with the
/// `max(0, 9 − current)` count tree and `TargetFilter::Player`.
#[test]
fn vraska_minus9_rider_fires_before_conditional_split() {
    let def = parse_effect_chain(MINUS_NINE_EFFECT, AbilityKind::Spell);

    let Effect::GivePlayerCounter {
        counter_kind,
        count,
        target,
    } = &*def.effect
    else {
        panic!("expected GivePlayerCounter, got {:?}", def.effect);
    };
    assert_eq!(*counter_kind, PlayerCounterKind::Poison);
    assert_eq!(*target, TargetFilter::Player);

    // count = ClampMin{ Offset{ Multiply{ -1, Ref{TargetControllerCounter{Poison}} }, 9 }, 0 }
    let QuantityExpr::ClampMin { inner, minimum } = count else {
        panic!("expected ClampMin count, got {count:?}");
    };
    assert_eq!(*minimum, 0);
    let QuantityExpr::Offset { inner, offset } = &**inner else {
        panic!("expected Offset inside ClampMin, got {inner:?}");
    };
    assert_eq!(*offset, 9);
    let QuantityExpr::Multiply { factor, inner } = &**inner else {
        panic!("expected Multiply inside Offset, got {inner:?}");
    };
    assert_eq!(*factor, -1);
    assert!(
        matches!(
            &**inner,
            QuantityExpr::Ref {
                qty: QuantityRef::TargetControllerCounter {
                    kind: PlayerCounterKind::Poison
                }
            }
        ),
        "leaf must be TargetControllerCounter{{Poison}}, got {inner:?}"
    );

    // Reach guard: the clause is no longer swallowed to Unimplemented.
    assert!(
        !matches!(&*def.effect, Effect::Unimplemented { .. }),
        "Vraska [−9] must not fall through to Effect::Unimplemented"
    );
}

/// Negative: a generic leading conditional must NOT be captured by the rider —
/// it routes through the normal conditional path.
#[test]
fn generic_leading_conditional_not_captured_by_rider() {
    let def = parse_effect_chain("If you control a Swamp, draw a card.", AbilityKind::Spell);
    assert!(
        !matches!(&*def.effect, Effect::GivePlayerCounter { .. }),
        "a generic 'if X, draw a card' must not be captured by the poison-difference rider"
    );
}

/// Consolidation guard: after routing the kind mapping through the shared
/// `parse_player_counter_kind` authority, "get two rad counters" still yields a
/// `GivePlayerCounter{Rad, 2}`.
#[test]
fn player_counter_kind_refactor_preserves_rad() {
    let def = parse_effect_chain("You get two rad counters.", AbilityKind::Spell);
    let Effect::GivePlayerCounter {
        counter_kind,
        count,
        ..
    } = &*def.effect
    else {
        panic!("expected GivePlayerCounter, got {:?}", def.effect);
    };
    assert_eq!(*counter_kind, PlayerCounterKind::Rad);
    assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
}

/// Consolidation guard (negative): an object counter (`+1/+1`) is not a player
/// counter and must be rejected by the shared authority.
#[test]
fn object_counter_is_not_a_player_counter() {
    let def = parse_effect_chain("You get a +1/+1 counter.", AbilityKind::Spell);
    assert!(
        !matches!(&*def.effect, Effect::GivePlayerCounter { .. }),
        "a +1/+1 (object) counter must not parse as a player counter"
    );
}

// ---------------------------------------------------------------------------
// [−2] become-Treasure — runtime via handle_activate_loyalty + layers.
// ---------------------------------------------------------------------------

/// CR 205.1a + CR 613.1d + CR 613.1f + CR 613.8a: the target creature becomes an
/// Artifact Treasure, loses its creature type and printed abilities, and gains
/// the granted sacrifice-for-mana ability.
#[test]
fn vraska_minus2_target_becomes_treasure() {
    let parsed = parsed_vraska();
    let index = loyalty_index(&parsed, -2);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vraska = scenario
        .add_creature(P0, "Vraska, Betrayal's Sting", 0, 0)
        .id();
    // Target: a Goblin with an activated ability, so we can observe both the
    // type replacement and the ability wipe + regrant.
    let goblin = {
        let mut b = scenario.add_creature_from_oracle(P1, "Test Goblin", 2, 2, "{T}: Draw a card.");
        b.with_subtypes(vec!["Goblin"]);
        b.id()
    };
    let mut runner = scenario.build();
    wire_vraska(runner.state_mut(), vraska, 6, &parsed);
    runner.state_mut().all_creature_types = vec!["Goblin".to_string()];

    runner
        .activate(vraska, index)
        .target_object(goblin)
        .resolve();

    // Read the EFFECTIVE post-layer characteristics.
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&goblin];

    // CR 205.1a + CR 613.1d: card-type set replaced with Artifact.
    assert!(
        obj.card_types.core_types.contains(&CoreType::Artifact),
        "target must become an Artifact, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Creature),
        "target must lose the Creature card type, got {:?}",
        obj.card_types.core_types
    );

    // Revert-failing for the modification ORDER fix: if AddSubtype(Treasure) were
    // wiped by RemoveAllSubtypes(Artifact) applied afterward, Treasure would be
    // absent.
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Treasure"),
        "target must be a Treasure (AddSubtype must survive the subtype removals), got {:?}",
        obj.card_types.subtypes
    );
    assert!(
        !obj.card_types.subtypes.iter().any(|s| s == "Goblin"),
        "target must lose its creature subtype, got {:?}",
        obj.card_types.subtypes
    );

    // CR 613.1f + CR 613.8a: all abilities lost, then the sac-for-mana ability
    // granted (survives because RemoveAllAbilities is ordered before the grant).
    assert_eq!(
        obj.abilities.len(),
        1,
        "printed ability must be removed and only the granted ability remain, got {:?}",
        obj.abilities
    );
    assert!(
        matches!(*obj.abilities[0].effect, Effect::Mana { .. }),
        "the granted Treasure ability must produce mana, got {:?}",
        obj.abilities[0].effect
    );
}
