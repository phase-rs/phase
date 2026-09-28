//! CR 608.2h — `chain_root_targets` must survive an interactive suspension in
//! every carrier that can park one.
//!
//! **What makes these rows different from the `chain_root_target_*` rows in
//! `game/quantity.rs`.** Those construct the `Pending*` frame directly with
//! `chain_root_targets` already filled in, then call the resume entry point.
//! That pins the resume CONSUMER, but it hands the test exactly the value the
//! production path is supposed to preserve — so deleting the creator-side copy
//! (`effects/roll_die.rs`'s `PendingDieRollInstruction`, `effects/flip_coin.rs`'s
//! suspended `PendingCoinFlip`, `effects/vote.rs`'s `PendingVoteBallotIteration`)
//! leaves every one of them green.
//!
//! Each row below instead starts from a real `ResolvedAbility` carrying the
//! `finalize_cast` chain-root stamp, drives the REAL resolver until a genuine
//! replacement/choice suspension parks the frame, answers the choice through the
//! production `GameAction` path, and only then asserts the counter-gated branch
//! observed the chain-root target's counter total. The creator, the frame, and
//! the resume consumer are therefore all on the measured path, and removing any
//! single creator-side copy fails the row named for it.
//!
//! The gate quantity is `QuantityRef::CountersOn { scope: ChainRootTarget }` in
//! its already-rebound form. The `EventContextAmount` → gate rebind is the
//! PARSER's half of this contract and is covered in
//! `parser::oracle_effect::tests`; these rows own the RESOLVER's half, so they
//! start from the rebound shape deliberately.
//!
//! Reach-guard discipline (as in `barbarian_class_die_roll_replacement.rs`): the
//! failure value on this path is 0 counters, which would vacuously satisfy a
//! "not the wrong number" assertion. Every row therefore asserts the branch had
//! NOT yet run while the choice was open, and asserts the exact total after.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, DieResultBranch, Effect, ObjectScope,
    QuantityExpr, QuantityRef, ResolvedAbility, TargetChoiceTiming, TargetFilter, TargetRef,
    TypeFilter, TypedFilter, VoteSubject, VoteTally, VoteVisibility, VoterScope,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BARBARIAN_CLASS_L1: &str = "If you would roll one or more dice, instead roll that many \
dice plus one and ignore the lowest roll.";

const KRARK: &str = "If you would flip a coin, instead flip two coins and ignore one.";

/// The chain-root target's counter total: 2 `+1/+1` plus 1 `oil`. Kind-agnostic,
/// matching Dismantle's own ruling that only the TOTAL matters.
const ROOT_COUNTER_TOTAL: u32 = 3;

/// A counter-gated `PutCounter` in its post-rebind shape: "put that many
/// counters on an artifact you control", where "that many" reads the chain-root
/// target. Left unpropagated, `ChainRootTarget` resolves against an empty
/// context and places ZERO — silently, with no error and no gap.
fn counter_gated_branch() -> AbilityDefinition {
    let mut def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Ref {
                qty: QuantityRef::CountersOn {
                    scope: ObjectScope::ChainRootTarget,
                    counter_type: None,
                },
            },
            // The recipient is DESCRIBED, not targeted (CR 115.10a) — exactly
            // Dismantle's shape. The fixture gives the controller exactly one
            // artifact so the resolution-time choice auto-resolves (CR 608.2d).
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }),
        },
    );
    // Without this the recipient is never selected at resolution and the branch
    // silently places nothing — the same zero the propagation bug produces, for
    // an unrelated reason. Stamped explicitly so a green row means what it says.
    def.target_choice_timing = TargetChoiceTiming::Resolution;
    def
}

/// The chain-root target (an opponent's counter-laden artifact) and the
/// controller's sole artifact, which is the branch's recipient.
///
/// The root is OPPONENT-controlled so it cannot itself satisfy the branch's
/// `controller: You` recipient filter — otherwise a run that read the wrong
/// object could still look right.
fn add_root_and_recipient(scenario: &mut GameScenario) -> (ObjectId, ObjectId) {
    let root = scenario
        .add_artifact_from_oracle(P1, "Counter-Laden Artifact", "")
        .id();
    scenario.with_counter(root, CounterType::Plus1Plus1, 2);
    scenario.with_counter(root, CounterType::Generic("oil".into()), 1);
    let recipient = scenario
        .add_artifact_from_oracle(P0, "Recipient Artifact", "")
        .id();
    (root, recipient)
}

fn counters_on(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Stamp the chain-root context the way `finalize_cast` does for a real spell.
fn with_chain_root(mut ability: ResolvedAbility, root: ObjectId) -> ResolvedAbility {
    ability.context.chain_root_targets = vec![TargetRef::Object(root)];
    ability
}

/// CR 706.6 + CR 608.2h — die: creator → `DieKeepChoice` suspension → resume.
///
/// Barbarian Class raises the d1 roll to two dice, which necessarily tie, so
/// CR 706.6 opens a real ignore choice and `execute_roll` parks a
/// `PendingDieRoll`. The results-table branch runs only after the roller
/// submits the ignore through `GameAction::SelectDieRolls`.
///
/// Discriminating: drop `chain_root_targets` from the `PendingDieRollInstruction`
/// built in `roll_die::resolve` (or from the `PendingDieRoll` `execute_roll`
/// derives from it) and the surviving die's branch places 0 instead of 3.
#[test]
fn chain_root_targets_survive_a_real_die_keep_choice_suspension() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let (root, recipient) = add_root_and_recipient(&mut scenario);
    let mut runner = scenario.build();

    let ability = with_chain_root(
        ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 1,
                results: vec![DieResultBranch {
                    min: 1,
                    max: 1,
                    effect: Box::new(counter_gated_branch()),
                }],
                modifier: None,
            },
            vec![],
            source,
            P0,
        ),
        root,
    );

    let mut events: Vec<GameEvent> = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the die roll resolves into a keep choice");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "reach-guard: the tied d1s must open a real CR 706.6 ignore choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        counters_on(&runner, recipient),
        0,
        "reach-guard: the results branch must not have run while the choice is open"
    );

    runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the roller submits the ignore choice");

    assert_eq!(
        counters_on(&runner, recipient),
        ROOT_COUNTER_TOTAL,
        "CR 608.2h: the surviving die's branch must read the chain-root target's \
         counter total across the suspension, not an empty context"
    );
}

/// CR 705.1 + CR 608.2h — coin: creator → `CoinFlipKeepChoice` suspension →
/// resume.
///
/// Krark's Thumb replaces the single flip with two-flip-keep-one, which parks a
/// `PendingCoinFlip` in the `CoinFlipOutcome::Suspended` arm of
/// `flip_coin::resolve`. Both branches carry the same counter-gated body, so the
/// assertion holds for either kept face and the row needs no RNG pinning.
///
/// Discriminating: drop `chain_root_targets` from that suspended `PendingCoinFlip`
/// and the kept flip's branch places 0 instead of 3.
#[test]
fn chain_root_targets_survive_a_real_coin_flip_keep_choice_suspension() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Krark's Thumb", 0, 1, KRARK)
        .id();
    let (root, recipient) = add_root_and_recipient(&mut scenario);
    let mut runner = scenario.build();

    let ability = with_chain_root(
        ResolvedAbility::new(
            Effect::FlipCoin {
                win_effect: Some(Box::new(counter_gated_branch())),
                lose_effect: Some(Box::new(counter_gated_branch())),
                flipper: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        ),
        root,
    );

    let mut events: Vec<GameEvent> = Vec::new();
    engine::game::effects::flip_coin::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the flip resolves into a keep choice");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CoinFlipKeepChoice { .. }
        ),
        "reach-guard: Krark's Thumb must open a real CR 705.1 keep choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        counters_on(&runner, recipient),
        0,
        "reach-guard: neither branch may run while the keep choice is open"
    );

    runner
        .act(GameAction::SelectCoinFlips {
            keep_indices: vec![0],
        })
        .expect("the flipper keeps one flip");

    assert_eq!(
        counters_on(&runner, recipient),
        ROOT_COUNTER_TOTAL,
        "CR 608.2h: the kept flip's branch must read the chain-root target's \
         counter total across the suspension, not an empty context"
    );
}

/// CR 701.38d + CR 608.2h — vote: creator → per-ballot suspension → drain.
///
/// Two voters both vote the single choice, so `resolve_tally` runs the
/// per-choice body twice. The body is optional, so the FIRST ballot parks a
/// `WaitingFor::OptionalEffectChoice` — which is precisely the condition under
/// which `vote::resolve_tally` stashes the remaining voters in a
/// `PendingVoteBallotIteration`. `drain_active_vote_ballot` then runs the second
/// ballot from that frame alone.
///
/// Discriminating in a way the other two rows are not: the first ballot reads
/// `chain_root_targets` from the live parameter, so it places 3 either way. Only
/// the RESUMED ballot reads it from the parked frame. Dropping the frame's copy
/// therefore yields 3 rather than 6 — the row fails on the resumed half
/// specifically, which is the half that crosses the suspension.
#[test]
fn chain_root_targets_survive_a_real_vote_ballot_suspension() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (root, recipient) = add_root_and_recipient(&mut scenario);
    // The park trigger. With TWO legal recipients the branch's resolution-time
    // recipient choice cannot auto-resolve (CR 608.2d), so it parks a
    // `ChooseFromZoneChoice` — which is exactly the interactive ballot park
    // `resolve_tally` names as its example, and the condition under which it
    // stashes the remaining voters in a `PendingVoteBallotIteration`.
    let decoy = scenario
        .add_artifact_from_oracle(P0, "Decoy Artifact", "")
        .id();
    let spell = scenario.add_spell_to_hand(P0, "Vote Spell", true).id();
    let mut runner = scenario.build();

    let body = counter_gated_branch();

    let ability = with_chain_root(
        ResolvedAbility::new(
            Effect::Vote {
                choices: vec!["alpha".to_string()],
                per_choice_effect: vec![Box::new(body)],
                starting_with: ControllerRef::You,
                voter_scope: VoterScope::AllPlayers,
                tally_mode: VoteTally::PerVote,
                subject: VoteSubject::Named,
                visibility: VoteVisibility::Open,
            },
            vec![],
            spell,
            P0,
        ),
        root,
    );

    // Unlike the die and coin rows, this one must resolve through the REAL stack
    // rather than calling `vote::resolve` directly. `resolve_tally`'s per-ballot
    // park captures a child boundary on the resolution stack, so a bare
    // `resolve` call parks an `OptionalEffect` frame with no enclosing
    // resolution frame to return to and trips
    // `debug_assert_runtime_resolution_invariants` on the next public action.
    let card_id = runner.state().objects[&spell].card_id;
    runner.state_mut().objects.get_mut(&spell).unwrap().zone = Zone::Stack;
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id,
            ability: Some(Box::new(ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // Both players pass, resolving the vote spell off the top of the stack.
    runner
        .act(GameAction::PassPriority)
        .expect("active player passes");
    runner
        .act(GameAction::PassPriority)
        .expect("non-active player passes, resolving the vote");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }),
        "reach-guard: the vote must open a real CR 701.38a choice, got {:?}",
        runner.state().waiting_for
    );

    // Both players vote the single choice.
    for _ in 0..2 {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }),
            "both players must be polled before the tally runs"
        );
        runner
            .act(GameAction::ChooseOption {
                choice: "alpha".to_string(),
            })
            .expect("a player casts their vote");
    }

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "reach-guard: the first ballot must park its recipient choice, which is what \
         stashes the remaining voters in a PendingVoteBallotIteration; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        counters_on(&runner, recipient),
        0,
        "reach-guard: no counters may land while the first ballot's choice is open"
    );

    // Pick the recipient for the first ballot.
    runner
        .act(GameAction::SelectCards {
            cards: vec![recipient],
        })
        .expect("the first ballot picks its recipient");
    assert_eq!(
        counters_on(&runner, recipient),
        ROOT_COUNTER_TOTAL,
        "reach-guard: the FIRST ballot reads chain_root_targets from the live \
         parameter, so it places the gate's total either way — a final total of \
         {ROOT_COUNTER_TOTAL} would be this ballot's alone"
    );

    // The drained second ballot parks its own recipient choice, proving the
    // resume ran at all before its counter total is compared.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "reach-guard: the RESUMED ballot must park its own recipient choice; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![recipient],
        })
        .expect("the resumed ballot picks its recipient");

    assert_eq!(
        counters_on(&runner, recipient),
        ROOT_COUNTER_TOTAL * 2,
        "CR 608.2h: the RESUMED ballot must read the chain-root target's counter \
         total from the parked frame. A total of {ROOT_COUNTER_TOTAL} here means the \
         resumed ballot placed zero and only the first ballot counted"
    );
    assert_eq!(
        counters_on(&runner, decoy),
        0,
        "the decoy exists only to make the recipient choice ambiguous; it must \
         never receive counters"
    );
}
