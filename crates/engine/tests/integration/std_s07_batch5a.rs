//! S07 Batch 5a — final Condition_If tranche, increment A (2 cards).
//!
//! Card 1 — Sonic Shrieker:
//!   "Flying\nWhen this creature enters, it deals 2 damage to any target and you
//!    gain 2 life. If a player is dealt damage this way, they discard a card."
//!   The trigger already parsed + resolved correctly (DealDamage → GainLife →
//!   Discard{ParentTarget}); the only defect was a spurious `Condition_If`
//!   swallow warning. The parse test below is the revert-discriminator for the
//!   new `swallow_check.rs` exemption branch; the runtime ETB tests prove the
//!   coverage flip is not hollow (player target → discard; creature target → no-op).
//!
//! Card 2 — Slumbering Trudge:
//!   "This creature enters with a number of stun counters on it equal to three
//!    minus X. If X is 2 or less, it enters tapped. (…)"
//!   The tap replacement previously carried `condition: null` (tapped
//!   UNCONDITIONALLY). This increment attaches
//!   `OnlyIfQuantity{CostXPaid, LE, 2}`. The X=3 runtime test is the
//!   revert-discriminator: reverting the condition taps it regardless and the
//!   untapped assertion fails.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    Comparator, Effect, QuantityExpr, QuantityRef, ReplacementCondition, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const SONIC_SHRIEKER_ORACLE: &str = "Flying\nWhen this creature enters, it deals 2 damage to any \
     target and you gain 2 life. If a player is dealt damage this way, they discard a card.";

const SLUMBERING_TRUDGE_ORACLE: &str = "This creature enters with a number of stun counters on it \
     equal to three minus X. If X is 2 or less, it enters tapped. (If a permanent with a stun \
     counter would become untapped, remove one from it instead.)";

fn has_condition_if_swallow(parsed: &ParsedAbilities) -> bool {
    parsed.parse_warnings.iter().any(|w| {
        matches!(w, OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If")
    })
}

fn first_hand_object(state: &GameState, player: PlayerId) -> ObjectId {
    *state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .front()
        .expect("player has a card to discard")
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

fn life_of(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// Drive the cast + ETB-trigger chain manually. The fluent `SpellCast::resolve`
/// driver stops at `DiscardChoice`, so a Sonic Shrieker player-target hit (which
/// forces a discard) must be driven here. Answers the single trigger target slot
/// with `target` and satisfies any `DiscardChoice` by discarding the affected
/// player's first hand card.
fn drive_etb(runner: &mut GameRunner, target: TargetRef) {
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target.clone()),
                    })
                    .expect("ChooseTarget (ETB trigger) must be accepted");
            }
            WaitingFor::DiscardChoice { player, .. } => {
                let pick = first_hand_object(runner.state(), player);
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("SelectCards (discard) must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

fn find_battlefield(state: &GameState, name: &str) -> ObjectId {
    state
        .objects
        .values()
        .find(|o| o.name == name && o.zone == Zone::Battlefield)
        .unwrap_or_else(|| panic!("{name} must be on the battlefield after resolution"))
        .id
}

// ── Card 1 — Sonic Shrieker ─────────────────────────────────────────────

/// DISCRIMINATING for the detector fix: the verbatim Oracle text must NOT emit a
/// `Condition_If` swallow. Reverting the `swallow_check.rs` exemption branch
/// re-raises the warning and fails this assertion.
#[test]
fn sonic_shrieker_no_condition_if_swallow() {
    // The "Flying" keyword MUST be supplied: otherwise the leading "Flying" line
    // parses to `Effect::unimplemented`, which suppresses ALL swallow detectors
    // (check_swallowed_clauses early-returns on any Unimplemented) and makes this
    // assertion vacuous. With Flying supplied the parse is clean and the
    // Condition_If detector actually runs — reverting the swallow_check.rs branch
    // then re-raises the warning and fails this assertion.
    let parsed = parse_oracle_text(
        SONIC_SHRIEKER_ORACLE,
        "Sonic Shrieker",
        &["Flying".to_string()],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        !has_condition_if_swallow(&parsed),
        "Sonic Shrieker's ParentTarget discard rider is structurally represented — \
         no Condition_If swallow expected. warnings: {:?}",
        parsed.parse_warnings
    );
}

/// Non-hollow guard (player target): ETB 2 damage to the opponent forces them to
/// discard a card; controller gains 2 life; opponent loses 2 life.
#[test]
fn sonic_shrieker_etb_damages_player_forces_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shrieker = scenario
        .add_creature_to_hand_from_oracle(P0, "Sonic Shrieker", 1, 3, SONIC_SHRIEKER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    scenario.add_card_to_hand(P1, "Filler A");
    scenario.add_card_to_hand(P1, "Filler B");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9_100),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&shrieker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: shrieker,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Sonic Shrieker must be accepted");
    drive_etb(&mut runner, TargetRef::Player(P1));

    let state = runner.state();
    assert_eq!(
        hand_len(state, P1),
        1,
        "opponent dealt damage this way must discard one of two cards"
    );
    assert_eq!(life_of(state, P0), 22, "controller gains 2 life");
    assert_eq!(life_of(state, P1), 18, "opponent takes 2 damage");
}

/// Non-hollow reach-guard (creature target): ETB 2 damage to a creature does NOT
/// force any discard (the ParentTarget discard resolves to a non-player object
/// and no-ops). Pairs with the player-target test so the negative isn't vacuous.
#[test]
fn sonic_shrieker_creature_target_no_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shrieker = scenario
        .add_creature_to_hand_from_oracle(P0, "Sonic Shrieker", 1, 3, SONIC_SHRIEKER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    let wall = scenario.add_creature(P1, "Stone Wall", 0, 5).id();
    scenario.add_card_to_hand(P1, "Filler A");
    scenario.add_card_to_hand(P1, "Filler B");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9_100),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&shrieker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: shrieker,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Sonic Shrieker must be accepted");
    drive_etb(&mut runner, TargetRef::Object(wall));

    let state = runner.state();
    assert_eq!(
        hand_len(state, P1),
        2,
        "a creature target is not a player — no discard, hand unchanged"
    );
    assert_eq!(
        state.objects[&wall].damage_marked, 2,
        "the targeted creature is marked with 2 damage"
    );
    assert_eq!(life_of(state, P0), 22, "controller still gains 2 life");
}

// ── Card 2 — Slumbering Trudge ──────────────────────────────────────────

/// Parse-shape gate: the "If X is 2 or less, it enters tapped" sentence must
/// attach `OnlyIfQuantity{CostXPaid, LE, Fixed(2)}` to the tap replacement.
/// Guards the silent-drop regression at the parser layer.
#[test]
fn slumbering_trudge_tap_replacement_carries_x_condition() {
    let parsed = parse_oracle_text(
        SLUMBERING_TRUDGE_ORACLE,
        "Slumbering Trudge",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let tap_repl = parsed
        .replacements
        .iter()
        .find(|r| {
            r.execute
                .as_deref()
                .is_some_and(|a| matches!(&*a.effect, Effect::SetTapState { .. }))
        })
        .expect("a SetTapState replacement must parse");
    match &tap_repl.condition {
        Some(ReplacementCondition::OnlyIfQuantity {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 2 },
            ..
        }) => {}
        other => panic!("expected OnlyIfQuantity(CostXPaid LE 2) on the tap, got {other:?}"),
    }
    // The tap replacement's `valid_card` must be self-scoped so the enters-tapped
    // gate only affects this permanent.
    assert_eq!(tap_repl.valid_card, Some(TargetFilter::SelfRef));
}

/// No-swallow gate: with the condition attached, the `Condition_If` swallow
/// clears. Reverting the dispatch/condition drops it and re-raises the warning.
#[test]
fn slumbering_trudge_no_condition_if_swallow() {
    let parsed = parse_oracle_text(
        SLUMBERING_TRUDGE_ORACLE,
        "Slumbering Trudge",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        !has_condition_if_swallow(&parsed),
        "the X-comparison enters-tapped gate is captured — no Condition_If swallow. \
         warnings: {:?}",
        parsed.parse_warnings
    );
}

/// Runtime X=1: 3 − 1 = 2 stun counters AND (X ≤ 2) enters tapped.
#[test]
fn slumbering_trudge_x1_enters_tapped_two_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trudge = scenario
        .add_creature_to_hand_from_oracle(P0, "Slumbering Trudge", 4, 4, SLUMBERING_TRUDGE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_000), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_001), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(trudge).x(1).resolve();

    let state = runner.state();
    let permanent = find_battlefield(state, "Slumbering Trudge");
    let obj = &state.objects[&permanent];
    assert_eq!(
        obj.counters.get(&CounterType::Stun).copied().unwrap_or(0),
        2,
        "3 − X(1) = 2 stun counters"
    );
    assert!(obj.tapped, "X=1 ≤ 2 → enters tapped");
}

/// Runtime X=3 (DISCRIMINATING): 3 − 3 = 0 stun counters AND (X > 2) enters
/// UNTAPPED. Reverting the tap condition taps it unconditionally and this
/// untapped assertion fails.
#[test]
fn slumbering_trudge_x3_enters_untapped_zero_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trudge = scenario
        .add_creature_to_hand_from_oracle(P0, "Slumbering Trudge", 4, 4, SLUMBERING_TRUDGE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_000), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_001), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_002), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_003), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(trudge).x(3).resolve();

    let state = runner.state();
    let permanent = find_battlefield(state, "Slumbering Trudge");
    let obj = &state.objects[&permanent];
    assert_eq!(
        obj.counters.get(&CounterType::Stun).copied().unwrap_or(0),
        0,
        "3 − X(3) = 0 stun counters"
    );
    assert!(
        !obj.tapped,
        "X=3 > 2 → must enter UNTAPPED (condition gate)"
    );
}
