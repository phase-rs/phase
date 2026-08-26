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

use engine::game::effects::player_counter::preview_player_counter_addition;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, EffectKind, QuantityModification, ReplacementDefinition, ReplacementMode,
    ReplacementPlayerScope,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
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
/// counters" is a CR 614.17 can't-effect, not a CR 614.1 replacement, so
/// Ward's poison-counter cost includes an event that can't happen.
///
/// CR 614.17b: "If an event can't happen, a player can't choose to pay a cost
/// that includes that event." The pay branch is therefore never offered — the
/// engine suppresses the prompt rather than accepting the choice and failing
/// the payment afterwards.
///
/// CR 614.17c is why no replacement prompt intervenes: an event that can't
/// happen "can only be replaced by a self-replacement effect … Other
/// replacement and/or prevention effects can't modify or replace it", so
/// `replacement::pipeline_loop` short-circuits a MANDATORY prohibition ahead of
/// any CR 616.1 ordering prompt and nothing parks.
///
/// CR 702.21a: the board outcome is unchanged by the seam move — an unpaid Ward
/// still counters the targeting spell, and Serpent Society survives.
///
/// Revert probe: restoring the pay branch re-emits
/// `WaitingFor::UnlessPayment { player: P1, cost: GetPlayerCounters }`, which
/// fails the `Priority { player: P1 }` assertion.
///
/// Solemnity's real Oracle text is "Players can't get counters." /
/// "Counters can't be put on artifacts, creatures, enchantments, or lands." —
/// only the first (relevant) sentence is used in this fixture.
#[test]
fn serpent_society_ward_solemnity_makes_the_payment_unchoosable_and_counters_the_spell() {
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

    // CR 614.17b: no unless-payment prompt is ever emitted for a payer whose
    // payment would include an impossible event.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "the prohibited pay branch must leave no unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        !legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "paying Ward's poison-counter cost must not be legal under Solemnity, got {legal:?}"
    );
    // Control on the same vector: a dead `legal_actions` would satisfy the
    // `!contains(pay: true)` assertion for the wrong reason.
    assert!(
        legal.contains(&GameAction::PassPriority),
        "the action vector must still be live, got {legal:?}"
    );
    assert!(
        runner.act(GameAction::PayUnlessCost { pay: true }).is_err(),
        "submitting the refused choice directly must also be rejected"
    );

    // CR 614.17c: a mandatory "players can't get counters" effect is
    // short-circuited ahead of any CR 616.1 ordering prompt, so nothing parks.
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "a mandatory can't-effect must not park a cost-move resume, got {:?}",
        runner.state().pending_cost_move_resume
    );

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
        "CR 614.17b: a payment whose own event can't happen is never chosen, so an unpaid Ward counters the targeting spell and Serpent Society survives"
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
///
/// Why synthetic, stated as a predicate rather than a card list: no printed card
/// produces an OPTIONAL `AddCounter` replacement. Every definition matching
/// `event == "AddCounter"` in `client/public/card-data.json` is `Mandatory`
/// (33 of 33 at time of writing; regenerate with
/// `jq '[.[] | (.replacements // [])[] | select(.event=="AddCounter") | .mode.type] | group_by(.) | map({m:.[0],n:length})' client/public/card-data.json`).
/// Combined with CR 614.17c — which short-circuits every MANDATORY prohibition
/// ahead of the CR 616.1 prompt — this synthetic definition is the only route to
/// `CostMoveDrainBoundary::ReplacementPrevented` at the counter-addition resume
/// root, which is real-card-dead today.
///
/// The field set is load-bearing, and this is the single site that owns the
/// reason. `object_replacement_candidate_applies` (`game/replacement.rs`)
/// consults `repl_def.valid_card` for EVERY event kind, including player
/// placements, whenever it is `Some`; `replacement_valid_card_matches` resolves
/// an `AddCounter` event through `ProposedEvent::affected_object_id`, which is
/// `CounterPlacement::object_id` — `None` for a player placement — and then
/// `.unwrap_or(false)`. So a definition carrying a `valid_card` filter is
/// EXCLUDED from a player counter placement, and the candidate predicate for
/// this fixture's event is `valid_player.is_some() && valid_card.is_none()`
/// (plus the counter-type matcher, the condition, and the mode). `valid_card` is
/// therefore left `None` here deliberately, not by omission.
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
/// Accept branch, and the discriminating `ReplacementPrevented` case: the payer
/// ACCEPTS the optional prevention, so the counter placement is completely
/// replaced (CR 614.6 — "if an event is replaced, it never happens") and zero
/// poison counters are given.
///
/// CR 118.12 is why the Ward cost is nevertheless PAID: the "if they don't"
/// clause "checks whether the player chose to pay an optional cost … regardless
/// of what events actually occurred", and that choice was latched at
/// `PayUnlessCost { pay: true }` — before the replacement pipeline was ever
/// consulted. CR 118.11 corroborates: a cost whose payment actions were modified
/// is still paid. So the targeting spell RESOLVES and Serpent Society dies.
///
/// This fixture — not the Solemnity row above — is the only route to
/// `CostMoveDrainBoundary::ReplacementPrevented` at the counter-addition resume
/// root. Solemnity's MANDATORY can't-effect is short-circuited by CR 614.17c
/// before any CR 616.1 prompt exists, so it is settled synchronously in
/// `costs.rs` (CR 614.17b) and never parks. Only an OPTIONAL prevention that the
/// payer accepts can carry a prevented placement into this resume.
#[test]
fn serpent_society_ward_optional_counter_prevention_accepted_still_pays_the_ward_cost_and_resolves_the_spell(
) {
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

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the optional prevention must be a legal replacement choice");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "CR 614.6: the accepted prevention must stop the poison counters from being given"
    );
    // The maintainer's required discriminator: the payer chose to pay, so under
    // CR 118.12 the Ward cost is PAID even though the placement was replaced away.
    // Before the fix this arm mapped `ReplacementPrevented` to a failed payment
    // and countered the spell instead.
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_none_or(|obj| obj.zone != Zone::Battlefield),
        "CR 118.12: a prevented placement on a chosen-to-pay cost is still a PAID cost — the targeting spell must resolve and remove Serpent Society from the battlefield"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the targeting spell must leave the stack by RESOLVING, not be left stranded"
    );

    // CR 118.12: the resume settles through the PAID epilogue, so Ward's guarded
    // ability finishes resolving and the whole reducer step's event buffer
    // survives instead of being discarded by the decline tail. Asserted on the
    // events captured from the `ChooseReplacement` act above.
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Counter,
                ..
            }
        )),
        "a paid Ward cost must emit the guarded ability's EffectResolved, got {:?}",
        result.events
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

    let result = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the optional prevention must be a legal replacement choice");

    // CR 118.12: the deferred paid settle must emit exactly what the immediate
    // paid leg emits — the counters the payer actually took, and the guarded
    // ability's completion. Before the fix this reducer step returned NO events
    // at all: the resume routed through the decline tail, whose `ActionResult` is
    // discarded while `action_result` has already drained the event buffer.
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerCounterChanged {
                player,
                counter_kind: PlayerCounterKind::Poison,
                delta: 5,
            } if *player == P1
        )),
        "the poison counters the payer actually took must reach the event log, got {:?}",
        result.events
    );
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Counter,
                ..
            }
        )),
        "a paid Ward cost must emit the guarded ability's EffectResolved, got {:?}",
        result.events
    );

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

/// CR 702.21a: the single-variable partner of
/// `serpent_society_ward_solemnity_makes_the_payment_unchoosable_and_counters_the_spell`
/// — the identical board with Solemnity absent.
///
/// Reach guard, not a discriminator: it passes before and after the CR 614.17b
/// change. Its job is to make that row's "no prompt / no `pay: true`"
/// assertions non-vacuous, by showing this fixture does reach the prompt and
/// does offer both branches when nothing forbids the counter event.
#[test]
fn serpent_society_ward_without_solemnity_offers_the_pay_branch() {
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

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment { player, cost, .. }
                if *player == P1
                    && matches!(
                        cost,
                        AbilityCost::GetPlayerCounters {
                            counter_kind: PlayerCounterKind::Poison,
                            count: 5,
                        }
                    )
        ),
        "with nothing prohibiting the counter event the Ward prompt must exist, got {:?}",
        runner.state().waiting_for
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "the pay branch must be offered when the counter event can happen, got {legal:?}"
    );
    assert!(
        legal.contains(&GameAction::PayUnlessCost { pay: false }),
        "the decline branch must be offered too, got {legal:?}"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the opponent pays Ward's poison-counter cost");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        5,
        "paying must give the targeting opponent five poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_none_or(|obj| obj.zone != Zone::Battlefield),
        "paying must let the targeted destroy spell resolve"
    );
}

/// Installs a synthetic MANDATORY "players can't get counters" replacement on a
/// fresh P0 permanent, scoped by `ReplacementPlayerScope`.
///
/// The mandatory sibling of `install_optional_player_counter_prevention`: every
/// parameter is preserved verbatim except `mode` (`ReplacementMode::Mandatory`)
/// and `valid_player` (`Some(scope)`). `valid_card` stays `None` for the reason
/// that helper's doc records.
///
/// CR 614.17c: a MANDATORY prohibition is short-circuited by
/// `replacement::pipeline_loop` ahead of any CR 616.1 ordering prompt, which is
/// why these rows never reach a replacement choice.
///
/// `scope` is the typed axis rather than one helper per scope, so the `You` and
/// `AnyPlayer` fixtures share one definition site.
fn install_mandatory_player_counter_prevention(
    state: &mut engine::types::game_state::GameState,
    scope: ReplacementPlayerScope,
) {
    let source = create_object(
        state,
        CardId(9102),
        P0,
        "Mandatory Poison Warden".to_string(),
        Zone::Battlefield,
    );
    let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter);
    def.mode = ReplacementMode::Mandatory;
    def.quantity_modification = Some(QuantityModification::Prevent);
    def.valid_player = Some(scope);
    let reps = vec![def];
    let obj = state.objects.get_mut(&source).unwrap();
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
}

/// Reaches the Ward prompt with nothing prohibiting the counter event, then
/// installs a mandatory prohibition while the prompt is live.
///
/// CR 614.17a: a can't-effect must exist when the event occurs, so the answer
/// is re-checked on the LIVE board rather than latched when the prompt was
/// built. The prompt survives; only the pay branch is refused, and the decline
/// branch stays legal (CR 118.12a).
///
/// Revert probe: without the reducer-side re-check, `PayUnlessCost { pay: true }`
/// stays legal after the install and `act(pay: true)` returns `Ok`, failing both
/// the `!legal.contains(pay: true)` and the `is_err()` assertions.
#[test]
fn serpent_society_ward_prohibition_arriving_mid_window_removes_the_pay_branch() {
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

    // Positive reach guard: the prompt exists before the prohibition arrives.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the fixture must reach the Ward prompt before the prohibition is installed, got {:?}",
        runner.state().waiting_for
    );
    let legal_before = engine::ai_support::legal_actions(runner.state());
    assert!(
        legal_before.contains(&GameAction::PayUnlessCost { pay: true }),
        "the pay branch must be offered before the prohibition arrives, got {legal_before:?}"
    );

    install_mandatory_player_counter_prevention(
        runner.state_mut(),
        ReplacementPlayerScope::AnyPlayer,
    );

    // CR 614.17a: the prompt itself survives — only the CHOICE is refused.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the prompt must survive a mid-window prohibition, got {:?}",
        runner.state().waiting_for
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        !legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "the pay branch must be refused on the live board, got {legal:?}"
    );
    // Control on the same vector: a member the mechanism must NOT remove.
    assert!(
        legal.contains(&GameAction::PayUnlessCost { pay: false }),
        "the decline branch must stay legal (CR 118.12a), got {legal:?}"
    );
    assert!(
        runner.act(GameAction::PayUnlessCost { pay: true }).is_err(),
        "the reducer must refuse a pay choice that includes an impossible event"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the refused action must not consume the prompt, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining stays legal after the pay branch is refused");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "declining must give no poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == Zone::Battlefield),
        "an unpaid Ward must counter the targeting spell, so Serpent Society survives"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack"
    );
}

/// CR 614.17c: two MANDATORY prohibitions on the same `AddCounter` event.
///
/// An event that can't happen "can only be replaced by a self-replacement
/// effect", so `replacement::pipeline_loop` short-circuits ahead of the CR 616.1
/// ordering step: two sources produce one refusal, never an ordering prompt.
/// The single-source form of the same board is
/// `serpent_society_ward_solemnity_makes_the_payment_unchoosable_and_counters_the_spell`,
/// which is this row's single-variable control — if this row passed with two
/// sources while that one failed with one, the gate would be counting sources
/// rather than short-circuiting.
#[test]
fn serpent_society_ward_two_mandatory_prohibitions_still_refuse_once_without_ordering() {
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
    install_mandatory_player_counter_prevention(
        runner.state_mut(),
        ReplacementPlayerScope::AnyPlayer,
    );
    install_mandatory_player_counter_prevention(
        runner.state_mut(),
        ReplacementPlayerScope::AnyPlayer,
    );

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "two mandatory prohibitions must leave the pay branch unchoosable, got {:?}",
        runner.state().waiting_for
    );
    // CR 614.17c: no ordering prompt over an impossible event.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "a mandatory prohibition must short-circuit ahead of any CR 616.1 ordering, got {:?}",
        runner.state().waiting_for
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        !legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "the pay branch must not be offered, got {legal:?}"
    );
    assert!(
        legal.contains(&GameAction::PassPriority),
        "the action vector must still be live, got {legal:?}"
    );

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "no poison counters may be given"
    );
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "nothing may park, got {:?}",
        runner.state().pending_cost_move_resume
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == Zone::Battlefield),
        "an unpaid Ward must counter the targeting spell"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack"
    );
}

/// CR 614.17c: one MANDATORY and one OPTIONAL prohibition on the same
/// `AddCounter` event — the mandatory one wins ahead of any choice.
///
/// Control: `serpent_society_ward_optional_counter_prevention_accepted_…` and
/// `…_declined_…` show the same optional source ALONE parks and settles PAID
/// under CR 118.12. Same board, same optional source, one variable — the
/// mandatory sibling. So "no park" here is caused by the mandatory prohibition,
/// not by the fixture failing to install anything.
#[test]
fn serpent_society_ward_mandatory_prohibition_wins_over_an_optional_replacement() {
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
    install_mandatory_player_counter_prevention(
        runner.state_mut(),
        ReplacementPlayerScope::AnyPlayer,
    );
    install_optional_player_counter_prevention(runner.state_mut());

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "the mandatory prohibition must make the pay branch unchoosable, got {:?}",
        runner.state().waiting_for
    );
    // The optional park is never entered.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the optional replacement must not be offered, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "the optional park must never be entered, got {:?}",
        runner.state().pending_cost_move_resume
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        !legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "the pay branch must not be offered, got {legal:?}"
    );
    assert!(
        legal.contains(&GameAction::PassPriority),
        "the action vector must still be live, got {legal:?}"
    );

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "no poison counters may be given"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack"
    );
}

/// CR 614.17b + the replacement player-scope gate: a prohibition scoped
/// `ReplacementPlayerScope::You` on a P0-controlled source bites P0, not the
/// P1 payer, so the Ward prompt is unaffected.
///
/// Passes before and after the change by construction. It exists to fail if the
/// partition is drawn wrong — an implementer reading the replacement definition
/// directly instead of going through the previews, and thereby dropping the
/// player-scope check.
#[test]
fn serpent_society_ward_prohibition_scoped_to_the_source_controller_leaves_the_payer_alone() {
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
    install_mandatory_player_counter_prevention(runner.state_mut(), ReplacementPlayerScope::You);

    // Paired reach guard: the prohibition really was installed AND really does
    // bite somebody — its partner is the same call for the payer.
    assert!(
        preview_player_counter_addition(runner.state(), P0, P0, PlayerCounterKind::Poison, 5)
            .is_prohibited(),
        "a `You`-scoped prohibition on a P0 source must bite P0"
    );
    assert!(
        !preview_player_counter_addition(runner.state(), P1, P1, PlayerCounterKind::Poison, 5)
            .is_prohibited(),
        "a `You`-scoped prohibition on a P0 source must not bite P1"
    );

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment { player, cost, .. }
                if *player == P1
                    && matches!(
                        cost,
                        AbilityCost::GetPlayerCounters {
                            counter_kind: PlayerCounterKind::Poison,
                            count: 5,
                        }
                    )
        ),
        "P1 must still be prompted, got {:?}",
        runner.state().waiting_for
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "the pay branch must still be offered to the unaffected payer, got {legal:?}"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the unaffected payer may pay Ward's cost");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        5,
        "paying must give the unaffected payer five poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_none_or(|obj| obj.zone != Zone::Battlefield),
        "paying must let the targeted destroy spell resolve"
    );
}
