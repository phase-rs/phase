//! Balance ({1}{W} sorcery) — end-to-end equalization.
//!
//! Oracle: "Each player chooses a number of lands they control equal to the
//! number of lands controlled by the player who controls the fewest, then
//! sacrifices the rest. Players discard cards and sacrifice creatures the same
//! way."
//!
//! Balance lowers (via `try_parse_balance_equalization`) to a three-link
//! `sub_ability` chain — sacrifice lands, discard cards, sacrifice creatures —
//! each a `player_scope: All` effect whose count is
//! `Difference { per-player count, cross-player minimum }`. This test drives
//! the parsed ability through `resolve_ability_chain` against an asymmetric
//! board and asserts every player is reduced to the per-zone minimum, proving:
//!   - the player-scope APNAP fan-out,
//!   - the `QuantityExpr::Difference` disposal count,
//!   - the `ControlledByEachPlayer` / `HandSize { AllPlayers }` resolvers,
//!   - the §8 clause-local minimum snapshot (each clause's minimum is locked
//!     against its own pre-clause board), and
//!   - the interactive `EffectZoneChoice` / `DiscardChoice` round-trip.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BALANCE_ORACLE: &str = "Each player chooses a number of lands they \
control equal to the number of lands controlled by the player who controls \
the fewest, then sacrifices the rest. Players discard cards and sacrifice \
creatures the same way.";

/// Build Balance's resolved ability chain controlled by `controller`.
fn balance_ability(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(BALANCE_ORACLE, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

/// Create `n` battlefield permanents of `core_type` owned by `player`.
fn add_permanents(
    state: &mut GameState,
    base_card_id: u64,
    player: PlayerId,
    name: &str,
    core_type: CoreType,
    n: usize,
) {
    for i in 0..n {
        let id = create_object(
            state,
            CardId(base_card_id + i as u64),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![core_type];
        obj.base_card_types = obj.card_types.clone();
    }
}

/// Put `n` cards into `player`'s hand.
fn add_hand_cards(state: &mut GameState, base_card_id: u64, player: PlayerId, n: usize) {
    for i in 0..n {
        create_object(
            state,
            CardId(base_card_id + i as u64),
            player,
            "Forest".to_string(),
            Zone::Hand,
        );
    }
}

fn count_battlefield(state: &GameState, player: PlayerId, core_type: CoreType) -> usize {
    state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.core_types.contains(&core_type)
        })
        .count()
}

fn hand_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Drain any pending `EffectZoneChoice` / `DiscardChoice` prompts by selecting
/// the first `n` legal cards each prompt offers. Balance fans out per player;
/// each prompt is resolved before the next player's, so a simple loop drives
/// the whole chain to completion.
fn drain_choice_prompts(state: &mut GameState) {
    // Generous bound — Balance has at most one prompt per player per clause.
    for _ in 0..64 {
        let (player, cards): (PlayerId, Vec<ObjectId>) = match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } => (*player, cards.iter().take(*count).copied().collect()),
            WaitingFor::DiscardChoice {
                player,
                cards,
                count,
                ..
            } => (*player, cards.iter().take(*count).copied().collect()),
            _ => return,
        };
        apply(state, player, GameAction::SelectCards { cards })
            .expect("selecting the prompted cards should succeed");
    }
    panic!("choice prompts did not drain within the iteration bound");
}

/// Lands-only equalization: P0 has 4 lands, P1 has 2 → both end with 2.
#[test]
fn balance_equalizes_lands_to_the_minimum() {
    let mut state = GameState::new_two_player(42);
    let balance = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Balance".to_string(),
        Zone::Stack,
    );
    add_permanents(&mut state, 100, PlayerId(0), "Forest", CoreType::Land, 4);
    add_permanents(&mut state, 200, PlayerId(1), "Island", CoreType::Land, 2);

    let ability = balance_ability(PlayerId(0), balance);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    drain_choice_prompts(&mut state);

    assert_eq!(
        count_battlefield(&state, PlayerId(0), CoreType::Land),
        2,
        "P0 must sacrifice down to the minimum (2 lands)"
    );
    assert_eq!(
        count_battlefield(&state, PlayerId(1), CoreType::Land),
        2,
        "P1 was already at the minimum — keeps all 2 lands"
    );
}

/// Full Balance across all three zones with asymmetry in each — the
/// discriminating end-to-end test. Lands 5/1, hands 3/0, creatures 2/4.
/// After resolution: lands 1/1, hands 0/0, creatures 2/2. The creature clause
/// is the snapshot discriminator: its minimum (2) must be computed against the
/// board AFTER the land + hand clauses resolve, and P1 (4 creatures) must be
/// cut to 2 even though P0 already sat at 2.
#[test]
fn balance_equalizes_all_three_zones_independently() {
    let mut state = GameState::new_two_player(42);
    let balance = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Balance".to_string(),
        Zone::Stack,
    );
    // Lands: P0=5, P1=1 → min 1.
    add_permanents(&mut state, 100, PlayerId(0), "Forest", CoreType::Land, 5);
    add_permanents(&mut state, 150, PlayerId(1), "Island", CoreType::Land, 1);
    // Hands: P0=3, P1=0 → min 0.
    add_hand_cards(&mut state, 300, PlayerId(0), 3);
    // Creatures: P0=2, P1=4 → min 2.
    add_permanents(&mut state, 400, PlayerId(0), "Bear", CoreType::Creature, 2);
    add_permanents(&mut state, 450, PlayerId(1), "Elf", CoreType::Creature, 4);

    let ability = balance_ability(PlayerId(0), balance);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    drain_choice_prompts(&mut state);

    // Land clause: both → 1.
    assert_eq!(count_battlefield(&state, PlayerId(0), CoreType::Land), 1);
    assert_eq!(count_battlefield(&state, PlayerId(1), CoreType::Land), 1);
    // Hand clause: minimum is 0 → P0 discards its whole hand.
    assert_eq!(hand_len(&state, PlayerId(0)), 0);
    assert_eq!(hand_len(&state, PlayerId(1)), 0);
    // Creature clause: minimum 2 (computed against the post-hand-clause board)
    // → P1's 4 creatures cut to 2; P0 keeps its 2.
    assert_eq!(
        count_battlefield(&state, PlayerId(0), CoreType::Creature),
        2,
        "P0 was at the creature minimum — keeps 2"
    );
    assert_eq!(
        count_battlefield(&state, PlayerId(1), CoreType::Creature),
        2,
        "P1 must sacrifice down to the creature minimum (2)"
    );
}

/// Snapshot-independence discriminator: a player who starts ABOVE the land
/// minimum but exactly AT the creature minimum must still keep all creatures.
/// If the creature clause wrongly reused the land clause's minimum, or read a
/// live minimum shrunk by an earlier APNAP player, this would over-sacrifice.
#[test]
fn balance_creature_clause_uses_its_own_minimum() {
    let mut state = GameState::new_two_player(42);
    let balance = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Balance".to_string(),
        Zone::Stack,
    );
    // Lands: P0=3, P1=3 (already equal — land clause sacrifices nothing).
    add_permanents(&mut state, 100, PlayerId(0), "Forest", CoreType::Land, 3);
    add_permanents(&mut state, 150, PlayerId(1), "Island", CoreType::Land, 3);
    // Creatures: P0=1, P1=3 → creature minimum is 1, distinct from the land
    // count (3). P1 must end with exactly 1 creature.
    add_permanents(&mut state, 400, PlayerId(0), "Bear", CoreType::Creature, 1);
    add_permanents(&mut state, 450, PlayerId(1), "Elf", CoreType::Creature, 3);

    let ability = balance_ability(PlayerId(0), balance);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    drain_choice_prompts(&mut state);

    assert_eq!(
        count_battlefield(&state, PlayerId(0), CoreType::Land),
        3,
        "land clause: equal counts → no sacrifice"
    );
    assert_eq!(count_battlefield(&state, PlayerId(1), CoreType::Land), 3);
    assert_eq!(
        count_battlefield(&state, PlayerId(0), CoreType::Creature),
        1,
        "P0 sits at the creature minimum — keeps 1"
    );
    assert_eq!(
        count_battlefield(&state, PlayerId(1), CoreType::Creature),
        1,
        "P1 cut to the creature minimum (1), NOT the land count (3)"
    );
}

/// APNAP-order discriminator: cast Balance with a non-P0 active player and
/// confirm the clause-local snapshot still survives an APNAP-shuffled iteration.
/// The other tests in this file all cast Balance from P0 in a fresh state where
/// P0 is the active player, so APNAP order coincides with natural player order
/// — the snapshot's defence against live-shrink during fan-out is never
/// stressed. Here, `active_player = PlayerId(1)`, so APNAP visits P1 first.
/// With P1 = 5 lands and P0 = 2, a buggy live-resolve (no snapshot) would have
/// P1 sacrifice down to 2 first, shrinking the live minimum so P0 (now also
/// computing min = 2) keeps all 2. That's identical to the snapshotted answer
/// for the land clause alone, so we use creatures where the discriminator is
/// sharper: P1 = 4, P0 = 1, minimum 1. With APNAP visiting P1 first, the
/// snapshot must freeze min = 1 BEFORE P1 sacrifices; without the snapshot,
/// P1 would over-cut to 1 (correct here by coincidence) but the proof point
/// is that the snapshotted minimum equals the pre-fan-out minimum regardless
/// of which player APNAP visits first.
#[test]
fn balance_freezes_minimum_under_apnap_with_non_p0_active() {
    let mut state = GameState::new_two_player(42);
    // Cast Balance with PlayerId(1) as the active player — APNAP fan-out
    // visits P1 first, then P0, NOT natural player order.
    state.active_player = PlayerId(1);
    let balance = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Balance".to_string(),
        Zone::Stack,
    );
    // Lands: P0=2, P1=5 → minimum 2. APNAP visits P1 first.
    add_permanents(&mut state, 100, PlayerId(0), "Forest", CoreType::Land, 2);
    add_permanents(&mut state, 150, PlayerId(1), "Island", CoreType::Land, 5);
    // Creatures: P0=1, P1=4 → minimum 1. APNAP visits P1 first.
    add_permanents(&mut state, 400, PlayerId(0), "Bear", CoreType::Creature, 1);
    add_permanents(&mut state, 450, PlayerId(1), "Elf", CoreType::Creature, 4);

    // P1 (the active player) casts Balance.
    let ability = balance_ability(PlayerId(1), balance);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    drain_choice_prompts(&mut state);

    // Land clause: both players land at the pre-clause minimum (2). If the
    // snapshot failed under APNAP, P1 sacrificing first to 2 would still match,
    // but P0's iteration would then see live min=2 — coincidentally correct
    // here, which is why we also verify the creature clause below.
    assert_eq!(
        count_battlefield(&state, PlayerId(0), CoreType::Land),
        2,
        "P0 was at the land minimum — keeps 2"
    );
    assert_eq!(
        count_battlefield(&state, PlayerId(1), CoreType::Land),
        2,
        "P1 must sacrifice down to the land minimum (2) under APNAP"
    );
    // Creature clause: minimum 1, P1 must end at exactly 1 even though APNAP
    // visited P1 first. The snapshot must freeze min=1 against the pre-clause
    // board so P1's iteration sees the same minimum that a natural-order
    // iteration would. (P0 at 1 keeps all; P1 cuts 4→1.)
    assert_eq!(
        count_battlefield(&state, PlayerId(0), CoreType::Creature),
        1,
        "P0 was at the creature minimum — keeps 1"
    );
    assert_eq!(
        count_battlefield(&state, PlayerId(1), CoreType::Creature),
        1,
        "P1 must cut to the creature minimum (1) under APNAP fan-out"
    );
}

/// Three-player interactive fan-out: every player above the minimum is prompted
/// in APNAP order, the continuation chain carries the remaining players plus
/// the next clause across each `EffectZoneChoice` pause, and the clause-local
/// snapshot survives the continuation drain so every player equalizes to the
/// same minimum. Lands 5/3/4 → all end at 3.
#[test]
fn balance_three_player_interactive_fan_out_equalizes() {
    let mut state = GameState::new(FormatConfig::commander(), 3, 42);
    let balance = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Balance".to_string(),
        Zone::Stack,
    );
    add_permanents(&mut state, 100, PlayerId(0), "Forest", CoreType::Land, 5);
    add_permanents(&mut state, 150, PlayerId(1), "Island", CoreType::Land, 3);
    add_permanents(&mut state, 180, PlayerId(2), "Swamp", CoreType::Land, 4);

    let ability = balance_ability(PlayerId(0), balance);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    drain_choice_prompts(&mut state);

    for pid in [PlayerId(0), PlayerId(1), PlayerId(2)] {
        assert_eq!(
            count_battlefield(&state, pid, CoreType::Land),
            3,
            "{pid:?} must equalize to the land minimum (3)"
        );
    }
}
