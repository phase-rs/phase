//! Integration tests for Braids, Arisen Nightmare's end-step decline tail
//! (GitHub issues #491 / #4246 — "directs life loss to the wrong player" /
//! "declining the sacrifice still applies decline consequences").
//!
//! Oracle text:
//!   At the beginning of your end step, you may sacrifice an artifact,
//!   creature, enchantment, land, or planeswalker. If you do, each opponent
//!   may sacrifice a permanent of their choice that shares a card type with
//!   it. For each opponent who doesn't, that player loses 2 life and you
//!   draw a card.
//!
//! Rules-correct behavior (CR 608.2c / CR 109.5):
//!   - "that player loses 2 life" → the *opponent who declined* loses 2 life.
//!   - "you draw a card"          → Braids' controller draws — once per
//!     declining opponent.
//!
//! These tests drive the real resolution pipeline: the trigger's `execute`
//! chain is parsed from the real Oracle text, built into a `ResolvedAbility`,
//! and resolved via `resolve_ability_chain`; the per-opponent optional
//! sacrifice decisions are submitted as `GameAction`s through `apply`.
//!
//! Tests A/B exercise the *decline-consequence routing*. The third test,
//! `braids_opponent_accepts_no_decline_consequence`, exercises the
//! *`ParentTarget` LKI resolution that gates whether an opponent can accept
//! at all*: the parent `Sacrifice` is an untargeted effect (CR 608.2d), so the
//! sacrificed permanent never lands in `ability.targets` and the sub-ability's
//! `SharesQuality { reference: ParentTarget }` filter must resolve via the
//! effect-context LKI snapshot. With that fallback reverted, P1 is wrongly
//! treated as unable to sacrifice and the decline branch fires.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::ResolvedAbility;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BRAIDS_ORACLE: &str = "At the beginning of your end step, you may \
sacrifice an artifact, creature, enchantment, land, or planeswalker. If you \
do, each opponent may sacrifice a permanent of their choice that shares a \
card type with it. For each opponent who doesn't, that player loses 2 life \
and you draw a card.";

/// Build the Braids end-step trigger's `execute` chain as a `ResolvedAbility`
/// controlled by `controller`, with `source_id` as the source permanent.
fn braids_execute(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let parsed = parse_oracle_text(
        BRAIDS_ORACLE,
        "Braids, Arisen Nightmare",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Nightmare".to_string()],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Braids has an end-step trigger");
    let execute = trigger
        .execute
        .as_deref()
        .expect("Braids' trigger has an execute chain");
    build_resolved_from_def(execute, source_id, controller)
}

/// Create a battlefield permanent of the given core type owned by `player`.
fn add_permanent(
    state: &mut GameState,
    card_id: u64,
    player: PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![core_type];
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Seed `player`'s library with one stand-in card so a `Draw` has something
/// to draw. Returns the seeded card's `ObjectId`.
fn seed_library(state: &mut GameState, card_id: u64, player: PlayerId) -> ObjectId {
    let card = create_object(
        state,
        CardId(card_id),
        player,
        "Forest".to_string(),
        Zone::Library,
    );
    state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists")
        .library
        .push_back(card);
    card
}

fn life(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
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

/// Decide the next pending `OptionalEffectChoice` for the player it lands on.
fn decide_optional(state: &mut GameState, accept: bool) {
    let player = match &state.waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => *player,
        other => panic!("expected OptionalEffectChoice, got {other:?}"),
    };
    apply(state, player, GameAction::DecideOptionalEffect { accept })
        .expect("optional-effect decision should succeed");
}

/// Test D — controller declines the optional sacrifice. The entire "If you do"
/// branch must be skipped: no opponent prompts, no life loss, no draws.
#[test]
fn braids_controller_declines_no_consequences() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let braids = add_permanent(&mut state, 10, PlayerId(0), "Braids", CoreType::Creature);
    add_permanent(
        &mut state,
        11,
        PlayerId(0),
        "Grizzly Bears",
        CoreType::Creature,
    );
    add_permanent(&mut state, 20, PlayerId(1), "Forest", CoreType::Land);
    seed_library(&mut state, 30, PlayerId(0));

    let p0_life_before = life(&state, PlayerId(0));
    let p1_life_before = life(&state, PlayerId(1));
    let p0_hand_before = hand_len(&state, PlayerId(0));

    let ability = braids_execute(PlayerId(0), braids);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // P0 declines the optional sacrifice — the "If you do" branch must not run.
    decide_optional(&mut state, false);

    assert_eq!(
        life(&state, PlayerId(1)),
        p1_life_before,
        "opponent must not lose life when the controller declined to sacrifice"
    );
    assert_eq!(
        life(&state, PlayerId(0)),
        p0_life_before,
        "controller must not lose life"
    );
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before,
        "controller must not draw when they declined to sacrifice"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "no further optional prompts after declining the root sacrifice"
    );
}

/// Test A — single declining opponent. P0 controls Braids + a creature to
/// sacrifice; P1 controls only a Land (no shared card type → forced decline).
///
/// Discriminates Step 1a (Edge 1 stays scoped), Step 3 (`Not{IfYouDo}` gate),
/// and Step 4 (`None`→`ScopedPlayer` recipient). Does NOT discriminate Step 1b.
#[test]
fn braids_single_opponent_declines_loses_two_and_controller_draws() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let braids = add_permanent(&mut state, 10, PlayerId(0), "Braids", CoreType::Creature);
    let p0_creature = add_permanent(
        &mut state,
        11,
        PlayerId(0),
        "Grizzly Bears",
        CoreType::Creature,
    );
    // P1 controls only a Land — nothing shares a card type with a Creature.
    add_permanent(&mut state, 20, PlayerId(1), "Forest", CoreType::Land);
    seed_library(&mut state, 30, PlayerId(0));

    let p0_life_before = life(&state, PlayerId(0));
    let p1_life_before = life(&state, PlayerId(1));
    let p0_hand_before = hand_len(&state, PlayerId(0));

    let ability = braids_execute(PlayerId(0), braids);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // P0 accepts the optional sacrifice and sacrifices the creature.
    decide_optional(&mut state, true);
    apply(
        &mut state,
        PlayerId(0),
        GameAction::SelectCards {
            cards: vec![p0_creature],
        },
    )
    .unwrap();

    // P1 cannot sacrifice a matching permanent — decline.
    if matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }) {
        decide_optional(&mut state, false);
    }

    assert_eq!(
        life(&state, PlayerId(1)),
        p1_life_before - 2,
        "the declining opponent (P1) must lose 2 life"
    );
    assert_eq!(
        life(&state, PlayerId(0)),
        p0_life_before,
        "Braids' controller (P0) must NOT lose life"
    );
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before + 1,
        "Braids' controller draws exactly one card for the single declining opponent"
    );
}

/// Test B — multi-opponent fan-out (MANDATORY: the Edge-2 / Step-1b
/// discriminator). 3-player game; P1 and P2 are both forced to decline.
///
/// With Step 1b reverted (no `LoseLife` arm in
/// `effect_has_iteration_bound_recipient`), the `LoseLife→Draw` edge detaches:
/// the draw runs once after the `player_scope` loop instead of once per
/// declining opponent → P0's hand grows by 1, not 2. Test A cannot catch this.
#[test]
fn braids_two_opponents_decline_controller_draws_two() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let braids = add_permanent(&mut state, 10, PlayerId(0), "Braids", CoreType::Creature);
    let p0_creature = add_permanent(
        &mut state,
        11,
        PlayerId(0),
        "Grizzly Bears",
        CoreType::Creature,
    );
    // Both opponents control only a Land — forced decline.
    add_permanent(&mut state, 20, PlayerId(1), "Forest", CoreType::Land);
    add_permanent(&mut state, 21, PlayerId(2), "Island", CoreType::Land);
    // P0 draws once per declining opponent — seed two cards.
    seed_library(&mut state, 30, PlayerId(0));
    seed_library(&mut state, 31, PlayerId(0));

    let p0_life_before = life(&state, PlayerId(0));
    let p1_life_before = life(&state, PlayerId(1));
    let p2_life_before = life(&state, PlayerId(2));
    let p0_hand_before = hand_len(&state, PlayerId(0));

    let ability = braids_execute(PlayerId(0), braids);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    decide_optional(&mut state, true);
    apply(
        &mut state,
        PlayerId(0),
        GameAction::SelectCards {
            cards: vec![p0_creature],
        },
    )
    .unwrap();

    // Both opponents decline in turn.
    for _ in 0..2 {
        if matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }) {
            decide_optional(&mut state, false);
        }
    }

    assert_eq!(
        life(&state, PlayerId(1)),
        p1_life_before - 2,
        "declining opponent P1 loses 2 life"
    );
    assert_eq!(
        life(&state, PlayerId(2)),
        p2_life_before - 2,
        "declining opponent P2 loses 2 life"
    );
    assert_eq!(
        life(&state, PlayerId(0)),
        p0_life_before,
        "Braids' controller P0 must NOT lose life"
    );
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before + 2,
        "Braids' controller draws ONCE PER declining opponent — exactly 2 cards \
         (Step 1b: the LoseLife→Draw edge stays inside the per-opponent scope)"
    );
}

/// Test C — accept-path: the opponent *can* and *does* sacrifice a matching
/// permanent, so no decline consequence fires.
///
/// Discriminates the `ParentTarget` LKI fix: the parent `Sacrifice` is an
/// untargeted effect (CR 608.2d), so the sacrificed Creature is never written
/// into `ability.targets`. The sub-ability's
/// `SharesQuality { quality: CardType, reference: ParentTarget }` filter must
/// fall back to the resolving ability's `effect_context_object` LKI snapshot.
///
/// With the snapshot rung reverted, `parent_target_shared_quality_values`
/// returns `None` (no object target, no `lki_cache` hit on a non-existent
/// target id) → the `SharesQuality` filter matches nothing → P1's eligible
/// pool is empty → P1's accept finds no legal sacrifice → the `Not{IfYouDo}`
/// decline branch fires → P1 loses 2 life and P0 draws. The `life(P1)` and
/// `hand_len(P0)` assertions both fail without the fix.
///
/// P1 is also given a non-matching Enchantment — it must be absent from the
/// `EffectZoneChoice.cards` pool, so the test discriminates the filter itself
/// (not merely "P1 controls some permanent").
#[test]
fn braids_opponent_accepts_no_decline_consequence() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let braids = add_permanent(&mut state, 10, PlayerId(0), "Braids", CoreType::Creature);
    let p0_creature = add_permanent(
        &mut state,
        11,
        PlayerId(0),
        "Grizzly Bears",
        CoreType::Creature,
    );
    // P1 controls two Creatures (each shares the `Creature` card type with the
    // sacrificed permanent) plus a non-matching Enchantment. Two matching
    // candidates force the engine to raise an explicit `EffectZoneChoice` for
    // P1 — a single matching candidate would auto-sacrifice with no prompt.
    let p1_creature = add_permanent(&mut state, 20, PlayerId(1), "Bear Cub", CoreType::Creature);
    let p1_creature_b = add_permanent(
        &mut state,
        22,
        PlayerId(1),
        "Scryb Sprites",
        CoreType::Creature,
    );
    let p1_enchantment = add_permanent(
        &mut state,
        21,
        PlayerId(1),
        "Pacifism",
        CoreType::Enchantment,
    );
    seed_library(&mut state, 30, PlayerId(0));

    let p0_life_before = life(&state, PlayerId(0));
    let p1_life_before = life(&state, PlayerId(1));
    let p0_hand_before = hand_len(&state, PlayerId(0));

    let ability = braids_execute(PlayerId(0), braids);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // P0 accepts the optional sacrifice and sacrifices the creature.
    decide_optional(&mut state, true);
    apply(
        &mut state,
        PlayerId(0),
        GameAction::SelectCards {
            cards: vec![p0_creature],
        },
    )
    .unwrap();

    // P1's per-opponent optional choice — P1 accepts.
    decide_optional(&mut state, true);

    // P1's sub-ability raises an `EffectZoneChoice` for the permanent to
    // sacrifice. The pool must be filtered through the now-fixed
    // `SharesQuality { reference: ParentTarget }` resolution: it must contain
    // both matching Creatures and exclude the non-matching Enchantment.
    match &state.waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, PlayerId(1), "the sacrifice choice belongs to P1");
            assert!(
                cards.contains(&p1_creature) && cards.contains(&p1_creature_b),
                "both of P1's Creatures must be legal sacrifices (each shares a \
                 card type with the sacrificed permanent)"
            );
            assert!(
                !cards.contains(&p1_enchantment),
                "P1's Enchantment shares no card type with the sacrificed \
                 Creature — it must be excluded from the candidate pool"
            );
        }
        other => panic!("expected EffectZoneChoice for P1, got {other:?}"),
    }

    apply(
        &mut state,
        PlayerId(1),
        GameAction::SelectCards {
            cards: vec![p1_creature],
        },
    )
    .unwrap();

    assert_eq!(
        life(&state, PlayerId(1)),
        p1_life_before,
        "P1 accepted and sacrificed a matching permanent — P1 loses NO life"
    );
    assert_eq!(
        life(&state, PlayerId(0)),
        p0_life_before,
        "P0 loses no life"
    );
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before,
        "no opponent declined — P0 draws no card"
    );
    assert_eq!(
        state.objects.get(&p1_creature).map(|o| o.zone),
        Some(Zone::Graveyard),
        "P1's matching Creature is in P1's graveyard — the sacrifice resolved"
    );
}
