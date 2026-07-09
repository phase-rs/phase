//! Parker Luck (B9) — symmetric cross-target "revealed by the other player" life
//! loss, plus the anaphor-precondition gate that keeps Keen Duelist honest-red.
//!
//! Card (Parker Luck):
//!   "At the beginning of your end step, two target players each reveal the top
//!    card of their library. They each lose life equal to the mana value of the
//!    card revealed by the other player. Then they each put the card they
//!    revealed into their hand."
//!
//! Sibling in the same class (Keen Duelist):
//!   "At the beginning of your upkeep, you and target opponent each reveal the
//!    top card of your library. You each lose life equal to the mana value of the
//!    card revealed by the other player. You each put the card you revealed into
//!    your hand."
//!
//! Two defects are fixed:
//!   A (parse gap) — the `lose` node parsed to `Effect::Unimplemented`; the new
//!     `parse_object_mana_value_ref` combinator + `ObjectScope::OtherRevealedCard`
//!     lower it to `LoseLife { ObjectManaValue { OtherRevealedCard } }`.
//!   B (latent runtime distribution) — a two-target reveal did NOT fan out; the
//!     `resolve_chain_body` reveal-all-then-fan-out branch (owner-keyed binding) +
//!     `reveal_top` reveal-all make both players reveal and cross-lose correctly.
//!
//! The CROSS is the discriminator: P0 loses P1's card's MV and vice-versa. A
//! same-player / Demonstrative bug yields the SWAPPED answer; a revert of Defect A
//! yields no life loss at all; a revert of Defect B reveals only one library.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityDefinition, Effect, ObjectScope, QuantityExpr, QuantityRef, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PARKER_LUCK_ORACLE: &str = "At the beginning of your end step, two target players each reveal the top card of their library. They each lose life equal to the mana value of the card revealed by the other player. Then they each put the card they revealed into their hand.";

const KEEN_DUELIST_ORACLE: &str = "At the beginning of your upkeep, you and target opponent each reveal the top card of your library. You each lose life equal to the mana value of the card revealed by the other player. You each put the card you revealed into your hand.";

// ---------------------------------------------------------------------------
// Parser-shape tests (§6.2, §6.5) — deterministic, drive the real parser.
// ---------------------------------------------------------------------------

fn parse_parker_luck() -> ParsedAbilities {
    parse_oracle_text(
        PARKER_LUCK_ORACLE,
        "Parker Luck",
        &[],
        &["Enchantment".to_string()],
        &[],
    )
}

fn parse_keen_duelist() -> ParsedAbilities {
    parse_oracle_text(
        KEEN_DUELIST_ORACLE,
        "Keen Duelist",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    )
}

/// The `lose` node is the reveal trigger's first sub-ability.
fn lose_node(parsed: &ParsedAbilities) -> &AbilityDefinition {
    let root = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("reveal trigger has an execute chain");
    assert!(
        matches!(&*root.effect, Effect::RevealTop { .. }),
        "reach-guard: the trigger root must parse as RevealTop, got {:?}",
        root.effect
    );
    root.sub_ability
        .as_ref()
        .expect("reach-guard: the reveal must carry a sub-ability chain (the lose clause)")
}

/// §6.2 SHAPE: with a multi-player reveal present, Parker Luck's lose node lowers
/// to `LoseLife { ObjectManaValue { OtherRevealedCard } }`. Revert-red: without the
/// §2.5 combinator the node stays `Effect::Unimplemented`. Reach-guards: the root
/// is a multi_target RevealTop and the lose carries the `put` (`ChangeZone`) sub,
/// so the assertion cannot pass on a whole-parse failure.
#[test]
fn parker_luck_lose_parses_to_other_revealed_card_mana_value() {
    let parsed = parse_parker_luck();
    let root = parsed.triggers[0].execute.as_ref().unwrap();
    assert!(
        root.multi_target.is_some(),
        "reach-guard: Parker Luck's reveal must carry a multi_target spec"
    );
    let lose = lose_node(&parsed);
    match &*lose.effect {
        Effect::LoseLife { amount, .. } => assert_eq!(
            *amount,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::OtherRevealedCard,
                },
            },
            "the lose amount must read the OTHER revealer's card mana value"
        ),
        other => panic!("Parker Luck lose must be LoseLife{{OtherRevealedCard}}, got {other:?}"),
    }
    assert!(
        matches!(
            &*lose.sub_ability.as_ref().unwrap().effect,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
        ),
        "reach-guard: the lose must chain the 'put into hand' ChangeZone"
    );
}

/// §6.5 HONEST-RED PIN (Daretti-INVERSE): Keen Duelist's reveal is single-subject
/// (`RevealTop { player: Any }`, no `multi_target`), so the B3 anchor-precondition
/// gate rewrites its `OtherRevealedCard`-bearing lose back to `Effect::Unimplemented`
/// (coverage `supported == false`) EVEN after the §2.5 combinator lands. Revert-red:
/// remove the §2.6 gate → the combinator makes this lose parse to `LoseLife` → KD
/// flips supported → this test reds. Reach-guards: the root is a RevealTop with NO
/// multi_target and the lose still chains the `put`, so the Unimplemented assertion
/// is not vacuous on a parse failure.
#[test]
fn keen_duelist_lose_stays_unimplemented_without_multiplayer_reveal() {
    let parsed = parse_keen_duelist();
    let root = parsed.triggers[0].execute.as_ref().unwrap();
    assert!(
        root.multi_target.is_none(),
        "reach-guard: Keen Duelist's single-subject reveal carries no multi_target"
    );
    let lose = lose_node(&parsed);
    assert!(
        matches!(&*lose.effect, Effect::Unimplemented { name, .. } if name == "lose"),
        "the gate must keep KD's lose an honest Unimplemented gap, got {:?}",
        lose.effect
    );
    assert!(
        matches!(
            &*lose.sub_ability.as_ref().unwrap().effect,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
        ),
        "reach-guard: the chain still parsed the 'put into hand' sub (only the lose is gapped)"
    );
}

/// §6.2 ISOLATION: the new combinator does not shadow the bare possessive path —
/// "that card's mana value" still lowers to `ObjectManaValue { Demonstrative }`
/// (Duskmantle Seer's own-card reading, unchanged).
#[test]
fn that_cards_mana_value_still_demonstrative() {
    let parsed = parse_oracle_text(
        "At the beginning of your upkeep, reveal the top card of your library and put that card into your hand. You lose life equal to that card's mana value.",
        "Duskmantle Seer Probe",
        &[],
        &["Creature".to_string()],
        &[],
    );
    // Find any LoseLife with an ObjectManaValue quantity in the parsed chain.
    fn find_lose_scope(def: &AbilityDefinition) -> Option<ObjectScope> {
        if let Effect::LoseLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue { scope },
                },
            ..
        } = &*def.effect
        {
            return Some(*scope);
        }
        def.sub_ability
            .as_deref()
            .and_then(find_lose_scope)
            .or_else(|| def.else_ability.as_deref().and_then(find_lose_scope))
    }
    let scope = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(find_lose_scope)
        .expect("the demonstrative probe must produce a LoseLife{ObjectManaValue}");
    assert_eq!(
        scope,
        ObjectScope::Demonstrative,
        "\"that card's mana value\" must stay Demonstrative, not the new OtherRevealedCard scope"
    );
}

// ---------------------------------------------------------------------------
// Runtime cast-pipeline tests — drive the real trigger→resolution pipeline.
// ---------------------------------------------------------------------------

/// Parker Luck on P0's battlefield; P0/P1 library tops carry the given mana values.
fn setup(p0_mv: u32, p1_mv: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    // Start post-combat so advancing to the end step does not halt at combat.
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Parker Luck", 1, 1, PARKER_LUCK_ORACLE);
    let p0_top = scenario
        .add_spell_to_library_top(P0, "P0 Top", false)
        .with_mana_cost(ManaCost::generic(p0_mv))
        .id();
    let p1_top = scenario
        .add_spell_to_library_top(P1, "P1 Top", false)
        .with_mana_cost(ManaCost::generic(p1_mv))
        .id();
    (scenario.build(), p0_top, p1_top)
}

/// Advance to P0's end step and choose the two DISTINCT player targets in order
/// (CR 601.2c: "two target players" are two distinct players), then resolve the
/// trigger to stack-empty.
fn fire_and_resolve(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the reveal trigger must prompt for its two player targets, got {:?}",
        runner.state().waiting_for
    );
    for pid in [P0, P1] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(pid)),
            })
            .unwrap_or_else(|e| panic!("choose player target {pid:?}: {e:?}"));
    }
    runner.advance_until_stack_empty();
}

/// §6.1 PRIMARY (the cross discriminator): P0's top MV 3, P1's top MV 5. P0 loses
/// P1's card's MV (5); P1 loses P0's card's MV (3). Both revealed cards end in
/// their owners' hands. A same-player/Demonstrative bug yields the SWAPPED −3/−5;
/// reverting Defect A yields −0/−0; reverting Defect B leaves one library revealed.
#[test]
fn parker_luck_cross_loss_is_symmetric() {
    let (mut runner, p0_top, p1_top) = setup(3, 5);
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    fire_and_resolve(&mut runner);

    // CR 119.3 + CR 108.3: each loses life equal to the OTHER revealer's card MV.
    assert_eq!(
        runner.life(P0) - p0_before,
        -5,
        "P0 must lose P1's card mana value (5), not its own (3)"
    );
    assert_eq!(
        runner.life(P1) - p1_before,
        -3,
        "P1 must lose P0's card mana value (3), not its own (5)"
    );
    // CR 608.2c: each puts the card THEY revealed into their own hand.
    assert_eq!(runner.state().objects[&p0_top].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&p1_top].zone, Zone::Hand);
    assert!(runner.state().players[0].hand.contains(&p0_top));
    assert!(runner.state().players[1].hand.contains(&p1_top));
}

/// §6.1 HOSTILE FIXTURE (tie): equal mana values (3/3) must STILL apply −3/−3 and
/// put both cards to hand — guards against an implementation that no-ops when the
/// two cards' mana values coincide.
#[test]
fn parker_luck_equal_mana_values_still_resolve() {
    let (mut runner, p0_top, p1_top) = setup(3, 3);
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    fire_and_resolve(&mut runner);

    assert_eq!(runner.life(P0) - p0_before, -3);
    assert_eq!(runner.life(P1) - p1_before, -3);
    assert_eq!(runner.state().objects[&p0_top].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&p1_top].zone, Zone::Hand);
}

/// §6.1b NEG (empty library → fail-closed): P1's library is empty; P0's top MV 4.
/// P0 loses 0 (P1 revealed nothing → the by-exclusion read finds no "other" card),
/// P1 loses 4 (P0's card), P0's card is put to hand, P1 puts nothing, no panic/OOB.
/// Revert-red under the R1 positional binding: `last_revealed_ids[1]` would be
/// out of bounds because the empty library contributes no entry (N2 skip).
#[test]
fn parker_luck_empty_library_fails_closed() {
    // P0 top MV 4; P1 has an empty library (no `add_spell_to_library_top` for P1).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Parker Luck", 1, 1, PARKER_LUCK_ORACLE);
    let p0_top = scenario
        .add_spell_to_library_top(P0, "P0 Top", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // Clear P1's starting library so it reveals nothing.
    let mut runner = scenario.build();
    runner.state_mut().players[1].library.clear();

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    fire_and_resolve(&mut runner);

    assert_eq!(
        runner.life(P0) - p0_before,
        0,
        "P0 loses 0 — P1 revealed nothing, so there is no 'other' card (CR 608.2b fail-closed)"
    );
    assert_eq!(
        runner.life(P1) - p1_before,
        -4,
        "P1 (empty library) still loses P0's card mana value (4)"
    );
    assert_eq!(
        runner.state().objects[&p0_top].zone,
        Zone::Hand,
        "P0 still puts its own revealed card into hand"
    );
    assert!(
        runner.state().players[1].hand.is_empty(),
        "P1 revealed and put nothing (empty library)"
    );
}

/// §6.1b NEG (illegal target on resolution → single-survivor fizzle, CR 608.2b):
/// one of the two targeted players is eliminated before the trigger resolves.
/// Upstream CR 608.2b target re-validation prunes the illegal target, so only the
/// survivor reveals — the by-exclusion "card revealed by the other player" is
/// genuinely absent, and the survivor loses 0 (fail-closed) while still putting
/// its own revealed card into hand. Uses a 3-player game so eliminating P1 does
/// not end the game. DISCRIMINATION: the survivor's own card is MV 6; if the B1
/// by-exclusion arm resolved to the OWN card (i.e. `find(|id| id != own)` relaxed
/// to return the own entry, the Demonstrative reading), the survivor would lose 6
/// — so "loses 0" reverts to red on that mis-binding. The "card in hand" reach-
/// guard proves the reveal + put actually ran (non-vacuous negative).
#[test]
fn parker_luck_eliminated_target_fails_closed() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Parker Luck", 1, 1, PARKER_LUCK_ORACLE);
    let p0_top = scenario
        .add_spell_to_library_top(P0, "P0 Top", false)
        .with_mana_cost(ManaCost::generic(6))
        .id();
    let mut runner = scenario.build();

    let p0_before = runner.life(P0);

    // Announce the trigger and choose targets P0 and P1 explicitly.
    runner.advance_to_end_step();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the reveal trigger must prompt for two player targets, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("choose P0 as the first target");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose P1 as the second target");

    // CR 800.4a: P1 leaves the game before the trigger resolves; CR 608.2b target
    // re-validation then prunes it, leaving P0 as the sole revealer.
    runner.state_mut().players[1].is_eliminated = true;

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0) - p0_before,
        0,
        "the survivor loses 0 — the pruned target never revealed a card, so the \
         by-exclusion other card is absent (CR 608.2b)"
    );
    assert_eq!(
        runner.state().objects[&p0_top].zone,
        Zone::Hand,
        "the survivor still puts its own revealed card into hand"
    );
}
