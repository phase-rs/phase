//! Issue #8115 — Temporal Extortion must grant its caster an extra turn when no
//! player pays to counter it.
//!
//! > When you cast this spell, any player may pay half their life, rounded up.
//! > If a player does, counter Temporal Extortion.
//! > Take an extra turn after this one.
//!
//! Reported as "doesn't grant an extra turn even when uncountered", in both AI
//! and PvP games. The card has two independent halves, so the two rows below
//! separate them and each is the other's reach-guard:
//!
//! - nobody pays  → the spell resolves and its caster takes an extra turn
//! - a player pays → that player loses half their life and the spell is
//!   countered, so NO extra turn is created
//!
//! Without the paying row, the declining row would pass just as well on an
//! engine where the cast trigger never fires at all.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall, 2026-11-05).
const TEMPORAL_EXTORTION: &str = "When you cast this spell, any player may pay half their life, rounded up. If a player does, counter Temporal Extortion.\nTake an extra turn after this one.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// Cast Temporal Extortion from P0, answering every "any player may pay" offer
/// with `accepts`. Returns the players actually prompted, so a decline-path
/// assertion cannot pass on an engine that never offered the choice.
fn cast_extortion(accepts: impl Fn(PlayerId) -> bool) -> (GameRunner, Vec<PlayerId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Temporal Extortion", false, TEMPORAL_EXTORTION)
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Black));
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Temporal Extortion must be castable with {B}{B}{B}{B} available");

    let mut prompted = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                prompted.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: accepts(player),
                    })
                    .expect("the pay-half-life offer must accept a decision");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
            }
            other => panic!("unexpected prompt while settling the cast: {other:?}"),
        }
    }
    (runner, prompted)
}

/// CR 118.5 + CR 500.7: nobody pays, so the spell is not countered and its
/// controller takes an extra turn after this one.
#[test]
fn temporal_extortion_grants_an_extra_turn_when_no_one_pays() {
    let (runner, prompted) = cast_extortion(|_| false);

    // Reach-guard: the offer must actually have been made, so the assertions
    // below cannot pass on an engine whose cast trigger never fired.
    assert!(
        prompted.contains(&P0) || prompted.contains(&P1),
        "at least one player must be offered the pay-half-life choice; prompted={prompted:?}"
    );
    assert_eq!(life(&runner, P0), 20, "nobody paid, so no life was lost");
    assert_eq!(life(&runner, P1), 20, "nobody paid, so no life was lost");

    assert!(
        runner.state().extra_turns.iter().any(|et| et.player == P0),
        "an uncountered Temporal Extortion must give its caster an extra turn, got {:?}",
        runner.state().extra_turns
    );
    assert!(
        !runner.state().extra_turns.iter().any(|et| et.player == P1),
        "the extra turn belongs to the caster, not the opponent, got {:?}",
        runner.state().extra_turns
    );
}

/// Paired reach-guard (CR 118.5 + CR 701.5a): a player pays half their life
/// rounded up, so the spell is countered and no extra turn is created. This is
/// what proves the row above is not passing because the offer never happens.
#[test]
fn temporal_extortion_is_countered_when_a_player_pays_half_their_life() {
    let (runner, prompted) = cast_extortion(|player| player == P1);

    assert!(
        prompted.contains(&P1),
        "P1 must be offered the choice in order to accept it; prompted={prompted:?}"
    );
    // CR 119.4: half of 20 rounded up is 10.
    assert_eq!(
        life(&runner, P1),
        10,
        "the paying player loses half their life, rounded up"
    );
    assert!(
        runner.state().extra_turns.is_empty(),
        "a countered Temporal Extortion grants no extra turn, got {:?}",
        runner.state().extra_turns
    );
}

// ---------------------------------------------------------------------------
// Class sibling — Dash Hopes.
//
// Dash Hopes is the only other card printed with this shape, and it is what
// makes the fix a building block rather than a one-card patch: it parses to the
// identical structure (`PayCost { payer: Controller }` under
// `optional_for: AnyPlayer`, with a `Counter { SelfRef }` rider gated on
// `OptionalEffectPerformed`). Its amount is a literal 5 rather than a "half
// their life" anaphor, so it isolates the *payer* binding from the dangling
// `PlayerScope::Target` anaphor that Temporal Extortion also depends on — each
// card covers one half of the defect, and neither alone would prove both.
// ---------------------------------------------------------------------------

/// CR 118.12 + CR 119.4: "any player may pay 5 life. If a player does, counter
/// Dash Hopes." The accepting player pays, not the caster.
#[test]
fn dash_hopes_is_paid_by_the_accepting_player_not_the_caster() {
    const DASH_HOPES: &str = "When you cast this spell, any player may pay 5 life. If a player does, counter Dash Hopes.\nCounter target spell.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dash = scenario
        .add_spell_to_hand_from_oracle(P0, "Dash Hopes", true, DASH_HOPES)
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Black));
    let mut runner = scenario.build();

    // A spell for Dash Hopes to target, so the cast is legal and its
    // cast-trigger actually fires. Same shape as `fold_into_aether_counter_put`.
    let opposing = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        P1,
        "Opposing Spell".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&opposing)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];
    runner.state_mut().stack.push_back(StackEntry {
        id: opposing,
        source_id: opposing,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let card_id = runner.state().objects[&dash].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: dash,
            card_id,
            targets: vec![opposing],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Dash Hopes must be castable with {B}{B} available and a spell to target");

    // P1 accepts the pay-5-life offer; P0 (the caster) declines.
    let mut prompted = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                prompted.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: player == P1,
                    })
                    .expect("the pay-5-life offer must accept a decision");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
            }
            _ => break,
        }
    }

    // Reach-guard: P1 must actually have been offered the choice, so the life
    // assertions below cannot pass on an engine that never fired the trigger.
    assert!(
        prompted.contains(&P1),
        "P1 must be offered the pay-5-life choice in order to accept it; prompted={prompted:?}"
    );
    // CR 118.12: the accepting player pays the cost.
    assert_eq!(
        life(&runner, P1),
        15,
        "the accepting player pays the 5 life"
    );
    // The caster is not the payer — this is the assertion that fails when the
    // `payer` rebind is reverted, because the parsed `payer` is `Controller`.
    assert_eq!(
        life(&runner, P0),
        20,
        "the caster pays nothing; the parsed `payer: Controller` names the offer, not the payer"
    );
}
