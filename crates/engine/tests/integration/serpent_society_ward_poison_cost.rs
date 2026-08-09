//! Regression for issue #6640: The Serpent Society's Ward—Get five poison
//! counters never gave the targeting opponent poison counters, because the
//! Oracle parser had no `WardCost` variant for "give yourself N counters" and
//! silently fell back to `WardCost::Mana(generic: 0)` — a free, always-paid
//! Ward that does nothing.
//!
//! https://github.com/phase-rs/phase/issues/6640
//!
//! CR references:
//!   - CR 702.21a: Ward — counter the targeting spell/ability unless the
//!     targeting player pays the stated cost.
//!   - CR 122.1 + CR 104.3d: giving a player poison counters; a player with
//!     ten or more poison counters loses the game (a separate SBA, not
//!     exercised by this test).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    QuantityModification, ReplacementDefinition, ReplacementMode, ReplacementPlayerScope,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::player::PlayerCounterKind;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;
use std::sync::Arc;

const SERPENT_SOCIETY: &str = "Deathtouch\n\
Ward—Get five poison counters. (A player with ten or more poison counters loses the game.)\n\
Whenever another creature you control with deathtouch dies, each opponent sacrifices a nontoken creature of their choice.";

#[test]
fn serpent_society_ward_prompts_the_targeting_opponent_for_poison_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    let WaitingFor::UnlessPayment { player, cost, .. } = &runner.state().waiting_for else {
        panic!(
            "Ward must prompt the targeting opponent to pay the poison-counter cost, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert!(matches!(
        cost,
        engine::types::ability::AbilityCost::GetPlayerCounters {
            counter_kind: PlayerCounterKind::Poison,
            count: 5,
        }
    ));
}

#[test]
fn serpent_society_ward_declined_counters_the_spell_and_gives_no_poison() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining Ward must be a legal action");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "declining Ward's cost must not give the opponent any poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == engine::types::zones::Zone::Battlefield),
        "declining Ward's cost must counter the targeting spell, leaving Serpent Society alive"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack"
    );
}

#[test]
fn serpent_society_ward_paid_gives_five_poison_and_the_spell_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the opponent pays Ward's poison-counter cost");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        5,
        "paying Ward's cost must give the targeting opponent five poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_none_or(|obj| obj.zone != engine::types::zones::Zone::Battlefield),
        "paying Ward's cost must let the targeted destroy spell resolve, removing Serpent Society from the battlefield"
    );
}

/// CR 104.3d + CR 704.5c: a payment that pushes the payer to ten or more
/// poison counters must trigger the loss state-based action immediately —
/// before the targeted destroy spell gets a chance to continue resolving.
/// Mirrors `crates/engine/src/game/sba.rs`'s own `sba_poison_10_player_loses`
/// unit test's expected shape.
#[test]
fn serpent_society_ward_payment_that_reaches_ten_poison_loses_the_game() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        state.players[P1.0 as usize].poison_counters = 5;
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the opponent pays Ward's poison-counter cost");

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        10,
        "5 existing + 5 from Ward's cost must reach the ten-poison threshold"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(p) } if p == P0
        ),
        "reaching ten poison must trigger the CR 104.3d loss SBA immediately, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == engine::types::zones::Zone::Battlefield),
        "the game must end (P1 loses) before the destroy spell gets a chance to resolve, so Serpent Society must still be on the battlefield"
    );
}

/// CR 122.1 + CR 614.17 + CR 702.21a: Solemnity's "Players can't get
/// counters" replacement must make Ward's poison-counter cost a FAILED
/// payment, not a free bypass. Before this fix, `add_player_counter_with_
/// replacement` reported `Prevented` as if it were a paid cost, so the
/// targeting opponent's spell would incorrectly continue resolving even
/// though no poison was actually given — nullifying Ward's entire deterrent
/// for free. Solemnity's real Oracle text is "Players can't get counters.
/// Prevent all damage that would be dealt to permanents by sources with
/// counters on them." — only the first (relevant) sentence is used here.
#[test]
fn serpent_society_ward_payment_prevented_by_solemnity_counters_the_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Solemnity", 0, 0, "Players can't get counters.")
        .as_enchantment();
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting to pay Ward's poison-counter cost must be a legal action even when Solemnity prevents the actual counter gain");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "Solemnity must prevent the poison counters from actually being given"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == engine::types::zones::Zone::Battlefield),
        "a prevented player-counter payment must be treated as a FAILED cost, countering the targeting spell exactly like a declined payment — Serpent Society must survive"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack"
    );
}

/// Installs a synthetic OPTIONAL "you may prevent a player from getting
/// counters" replacement on a fresh P0 permanent. No real card has exactly
/// this wording, so — mirroring this file's own Solemnity test (which uses a
/// real, if partial, MANDATORY prevention) and the engine's established
/// pattern for exercising an optional replacement choice with no real-card
/// precedent — the definition is installed directly, after `scenario.build()`,
/// so the real Ward -> `GetPlayerCounters` -> `add_player_counter_with_
/// replacement` -> `replace_event` path discovers it naturally (a production
/// setup, not a hand-constructed `WaitingFor`).
fn install_optional_player_counter_prevention(state: &mut engine::types::game_state::GameState) {
    let source = create_object(
        state,
        CardId(9101),
        P0,
        "Optional Poison Warden".to_string(),
        Zone::Battlefield,
    );
    let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter);
    def.mode = ReplacementMode::Optional { decline: None };
    def.quantity_modification = Some(QuantityModification::Prevent);
    def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
    let reps = vec![def];
    let obj = state.objects.get_mut(&source).unwrap();
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
}

/// Regression for reviewer matthewevans's finding on PR #6662: a Ward
/// player-counter cost whose `AddCounter` event needs a CR 616.1 replacement
/// choice (as opposed to Solemnity's unconditional, mandatory prevention
/// above) must not orphan the unless-payment continuation. Before this fix,
/// `add_player_counter_with_replacement`'s `NeedsChoice` arm replaced
/// `waiting_for` with the bare `ReplacementChoice` prompt and nothing
/// preserved `pending_effect`/`trigger_event` — once the player answered the
/// prompt, `handle_replacement_choice` applied (or failed to apply) the
/// counters and reset straight to `WaitingFor::Priority`, leaving Ward's
/// guarded "counter the spell" outcome permanently undetermined: the
/// targeting spell was neither countered nor allowed to resolve.
///
/// Accept branch: the optional replacement prevents the counter placement
/// (`PlayerCounterAdditionOutcome::Prevented`) — a FAILED Ward payment, so the
/// targeting spell must be countered, exactly like the Solemnity test above.
#[test]
fn serpent_society_ward_optional_counter_prevention_accepted_counters_the_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        install_optional_player_counter_prevention(state);
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting to pay Ward's poison-counter cost must be legal even when an optional replacement can prevent it");

    // Reaching a REPLACEMENT CHOICE (not an orphaned bare Priority) is the
    // regression's core assertion.
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        ..
    } = runner.state().waiting_for
    else {
        panic!(
            "optional player-counter prevention must surface a real replacement choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P1,
        "the payer (Ward's targeting opponent) makes the replacement choice"
    );
    assert_eq!(
        candidate_count, 2,
        "an Optional replacement offers accept (0) and decline (1)"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the optional prevention must be a legal replacement choice");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "the accepted prevention must stop the poison counters from being given"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == Zone::Battlefield),
        "a prevented player-counter payment must be treated as a FAILED cost, countering the targeting spell — Serpent Society must survive"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack, not left stranded"
    );
}

/// Decline branch: the optional replacement does not apply, so the original
/// `AddCounter` proceeds unmodified (`PlayerCounterAdditionOutcome::Applied`)
/// — a PAID Ward payment, so the targeting spell must resolve normally.
#[test]
fn serpent_society_ward_optional_counter_prevention_declined_pays_the_cost_and_resolves_the_spell()
{
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        install_optional_player_counter_prevention(state);
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting to pay must be legal");
    let WaitingFor::ReplacementChoice { .. } = runner.state().waiting_for else {
        panic!(
            "expected a replacement choice, got {:?}",
            runner.state().waiting_for
        );
    };

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the optional prevention must be a legal replacement choice");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        5,
        "declining the optional prevention must let Ward's cost actually give five poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_none_or(|obj| obj.zone != Zone::Battlefield),
        "a successfully paid Ward cost must let the targeted destroy spell resolve"
    );
}
