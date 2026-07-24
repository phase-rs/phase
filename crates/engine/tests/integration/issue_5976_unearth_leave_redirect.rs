//! GitHub issue #5976 — an unearthed creature's "if it would leave the
//! battlefield, exile it instead of putting it anywhere else" replacement
//! (CR 702.84a) is lost after a layer reseed, so sacrificing / destroying /
//! bouncing the creature sends it to the wrong zone instead of exile.
//!
//! CR 702.84a installs a `Moved` replacement on the returned permanent. It is
//! pushed onto the object's LIVE `replacement_definitions`, but every layer
//! pass reseeds live defs from `base_replacement_definitions`
//! (`seed_live_characteristics_from_base` in layers.rs — CR 613.1). Because the
//! rider is not written to base, the first full layer evaluation after the
//! return silently drops it, and the redirect (looked up via the host object's
//! live `replacement_definitions` in replacement.rs `moved_applier`) never
//! fires. The fix stamps the rider with
//! `RestrictionExpiry::UntilHostLeavesPlay` and installs it to base (surviving
//! the reseed, CR 613.1), non-copiable (CR 707.2), and pruned on host exit
//! (CR 400.7).
//!
//! Every row drives the REAL pipeline: activate Unearth, resolve, run a genuine
//! layer pass (`evaluate_layers`) between install and the leave event — the
//! exact reseed that used to wipe the rider — then trigger the leave through a
//! real sacrifice / destroy / bounce effect and assert the object lands in
//! exile. Reverting the base-install (and expiry stamp) flips R1/R3/R4/R8/R9
//! back to graveyard/hand.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim keyword line (reminder text is stripped by MTGJSON before the parser
// sees it, exactly as the runtime card presents). Rotting Rats is a 2/2.
const ROTTING_RATS: &str =
    "When this creature enters, each player discards a card.\nUnearth {1}{B}";

// Verbatim Oracle text of the real cards used as leave-event outlets.
const SACRIFICE: &str = "As an additional cost to cast this spell, sacrifice a creature.\n\
    Add an amount of {B} equal to the sacrificed creature's mana value.";
const GRUESOME_ENCORE: &str = "Put target creature card from an opponent's graveyard onto the \
    battlefield under your control. It gains haste. Exile it at the beginning of the next end step. \
    If that creature would leave the battlefield, exile it instead of putting it anywhere else.";
const DESTROY_TARGET: &str = "Destroy target creature.";
const BOUNCE_TARGET: &str = "Return target creature to its owner's hand.";
// Real card Goblin Bombardment — a proven activated sacrifice-cost outlet
// (see ajani_nacatl_pariah_sacrifice_outlet_6018). Its "any target" damage
// gives a clean reach guard that the ability actually resolved.
const GOBLIN_BOMBARDMENT: &str =
    "Sacrifice a creature: This enchantment deals 1 damage to any target.";

// ---------------------------------------------------------------------------
// Shared harness helpers
// ---------------------------------------------------------------------------

/// Stage a Rotting-Rats-shape unearth creature in P0's graveyard and fund the
/// {1}{B} activation. Both hands stay empty so the verbatim ETB "each player
/// discards a card" resolves as a harmless no-op at return time (nothing to
/// discard); the leave-event outlet is introduced only AFTER the return.
fn stage_rats(scenario: &mut GameScenario) -> ObjectId {
    scenario.at_phase(Phase::PreCombatMain);
    let rats = scenario
        .add_creature_to_graveyard(P0, "Rotting Rats", 2, 2)
        .from_oracle_text(ROTTING_RATS)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, rats, false, vec![]),
            ManaUnit::new(ManaType::Black, rats, false, vec![]),
        ],
    );
    rats
}

/// Activate the synthesized Unearth ability (the object's only activated
/// ability, index 0), resolve it, and then run a full layer pass — the CR 613.1
/// reseed that used to wipe the live-only rider. Returns with the rider on the
/// battlefield, ready for a leave event.
fn unearth(runner: &mut GameRunner, rats: ObjectId) {
    let outcome = runner.activate(rats, 0).resolve();
    assert_eq!(
        outcome.zone_of(rats),
        Zone::Battlefield,
        "precondition: Unearth returns Rotting Rats to the battlefield (same ObjectId)"
    );
    // The genuine layer pass between install and leave. Before the fix this
    // reseed drops the live-only rider (CR 613.1); after the fix the base copy
    // repopulates live so the redirect survives.
    evaluate_layers(runner.state_mut());
}

/// Fund `player`'s mana pool post-build (the unearth activation drained the
/// staged pool). Provenance ObjectId is cosmetic for pool accounting.
fn fund(runner: &mut GameRunner, player: PlayerId, colors: &[ManaType], provenance: ObjectId) {
    for &c in colors {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(c, provenance, false, vec![]));
    }
}

/// Move a spell staged in the library into `player`'s hand after the return, so
/// the ETB discard never sees it.
fn to_hand(runner: &mut GameRunner, spell: ObjectId) {
    move_to_zone(runner.state_mut(), spell, Zone::Hand, &mut Vec::new());
}

fn sacrificed(events: &[GameEvent], id: ObjectId) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == id))
}

// ---------------------------------------------------------------------------
// R1 — the reported bug: sacrifice as an additional cost of the real
// "Sacrifice" card. rats must land in EXILE, not the graveyard.
// ---------------------------------------------------------------------------

#[test]
fn r1_sacrifice_additional_cost_redirects_unearthed_rats_to_exile() {
    let mut scenario = GameScenario::new();
    let rats = stage_rats(&mut scenario);
    // Staged in the library, moved to hand after the return (keeps the ETB
    // discard a no-op). The real "Sacrifice" card carries the
    // additional-cost-sacrifice clause verbatim.
    let sac = scenario
        .add_spell_to_library_top(P0, "Sacrifice", true)
        .from_oracle_text(SACRIFICE)
        .id();
    let mut runner = scenario.build();

    unearth(&mut runner, rats);
    to_hand(&mut runner, sac);
    fund(&mut runner, P0, &[ManaType::Black], rats);

    let outcome = runner.cast(sac).sacrifice_with(&[rats]).resolve();

    // Reach guard: the sacrifice actually happened (not short-circuited on a
    // cost failure) — CR 701.21a.
    assert!(
        sacrificed(outcome.events(), rats),
        "reach guard: Rotting Rats must be sacrificed as the additional cost \
         (CR 701.21a); events: {:?}",
        outcome.events()
    );
    // The load-bearing assertion (flips to Graveyard when the base-install is
    // reverted): CR 702.84a redirect to exile.
    assert_eq!(
        outcome.zone_of(rats),
        Zone::Exile,
        "issue #5976: an unearthed creature sacrificed to a cost must be EXILED \
         (CR 702.84a), not put into the graveyard"
    );
}

// ---------------------------------------------------------------------------
// R2 — ability-cost sacrifice outlet ("Sacrifice a creature: You gain 1 life.").
// ---------------------------------------------------------------------------

#[test]
fn r2_ability_cost_sacrifice_outlet_redirects_to_exile() {
    let mut scenario = GameScenario::new();
    let rats = stage_rats(&mut scenario);
    // A durable (2-toughness) outlet enchantment already on the battlefield.
    let outlet = scenario
        .add_creature(P0, "Goblin Bombardment", 0, 0)
        .as_enchantment()
        .from_oracle_text(GOBLIN_BOMBARDMENT)
        .id();
    let mut runner = scenario.build();

    unearth(&mut runner, rats);
    let p1_life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;

    // Activate the outlet; drive the sacrifice cost and the damage target by
    // hand so we observe the PayCost::Sacrifice window directly (CR 602.1b).
    runner
        .act(GameAction::ActivateAbility {
            source_id: outlet,
            ability_index: 0,
        })
        .expect("activating Goblin Bombardment must succeed");

    let mut sacrificed_rats = false;
    let mut targeted = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                ..
            } => {
                assert!(
                    choices.contains(&rats),
                    "the unearthed rats must be a legal sacrifice, choices: {choices:?}"
                );
                runner
                    .act(GameAction::SelectCards { cards: vec![rats] })
                    .expect("sacrificing rats to the ability cost must succeed");
                sacrificed_rats = true;
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("targeting the opponent must succeed");
                targeted = true;
            }
            _ => {
                runner.advance_until_stack_empty();
                if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    && runner.state().stack.is_empty()
                {
                    break;
                }
            }
        }
    }

    // Reach guards: the sacrifice cost was actually paid with rats, and the
    // ability resolved (1 damage to P1) — CR 601.2b / CR 602.1b.
    assert!(sacrificed_rats, "the sacrifice cost must be paid with rats");
    assert!(targeted, "the outlet's damage must select a target");
    let p1_life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;
    assert_eq!(
        p1_life_after,
        p1_life_before - 1,
        "reach guard: the outlet must resolve and deal 1 damage to P1"
    );

    assert_eq!(
        runner.state().objects[&rats].zone,
        Zone::Exile,
        "issue #5976: rats sacrificed to an ability cost must be EXILED (CR 702.84a)"
    );
}

// ---------------------------------------------------------------------------
// R3 — destruction (a different leave event, CR 702.84a "ANY" exit).
// ---------------------------------------------------------------------------

#[test]
fn r3_destroy_redirects_unearthed_rats_to_exile() {
    let mut scenario = GameScenario::new();
    let rats = stage_rats(&mut scenario);
    let destroy = scenario
        .add_spell_to_library_top(P0, "Doom Blade", false)
        .from_oracle_text(DESTROY_TARGET)
        .id();
    let mut runner = scenario.build();

    unearth(&mut runner, rats);
    to_hand(&mut runner, destroy);
    fund(&mut runner, P0, &[ManaType::Black], rats);

    let outcome = runner.cast(destroy).target_objects(&[rats]).resolve();

    assert_eq!(
        outcome.zone_of(rats),
        Zone::Exile,
        "issue #5976: a destroyed unearthed creature must be EXILED (CR 702.84a), \
         not put into the graveyard"
    );
}

// ---------------------------------------------------------------------------
// R4 — bounce ("Return to its owner's hand" is a battlefield exit too;
// CR 702.84a redirects ANY leave to exile). Hand must stay unchanged.
// ---------------------------------------------------------------------------

#[test]
fn r4_bounce_redirects_unearthed_rats_to_exile_hand_unchanged() {
    let mut scenario = GameScenario::new();
    let rats = stage_rats(&mut scenario);
    let bounce = scenario
        .add_spell_to_library_top(P0, "Unsummon", true)
        .from_oracle_text(BOUNCE_TARGET)
        .id();
    let mut runner = scenario.build();

    unearth(&mut runner, rats);
    to_hand(&mut runner, bounce);
    fund(&mut runner, P0, &[ManaType::Blue], rats);

    let outcome = runner.cast(bounce).target_objects(&[rats]).resolve();

    // Reach guard: the bounce spell resolved cleanly (CR 608.2m) — the pipeline
    // is back at a Priority window, not stuck on a target/cost prompt.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "reach guard: the bounce must resolve cleanly, halted at {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.zone_of(rats),
        Zone::Exile,
        "issue #5976: an unearthed creature returned to hand must be EXILED instead \
         (CR 702.84a — ANY leave event)"
    );
    // CR 702.84a: it is exiled INSTEAD of going to hand — the owner's hand must
    // not gain the card.
    let owner_hand = &outcome
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .hand;
    assert!(
        !owner_hand.contains(&rats),
        "the card must not reach its owner's hand — it is exiled instead"
    );
}

// ---------------------------------------------------------------------------
// R5 — sibling: a Rotting Rats that was NEVER unearthed carries no rider, so a
// sacrifice sends it to the GRAVEYARD. Proves the rider is installed by the
// Unearth ability resolving, not intrinsic to the card shape.
// ---------------------------------------------------------------------------

#[test]
fn r5_non_unearthed_rats_sacrifice_goes_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Same-name, same Oracle text, but placed directly on the battlefield (no
    // Unearth activation), so no leaves-battlefield rider is installed.
    let rats = scenario
        .add_creature_from_oracle(P0, "Rotting Rats", 2, 2, ROTTING_RATS)
        .id();
    let sac = scenario
        .add_spell_to_hand_from_oracle(P0, "Sacrifice", true, SACRIFICE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, rats, false, vec![])],
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(sac).sacrifice_with(&[rats]).resolve();

    assert!(
        sacrificed(outcome.events(), rats),
        "reach guard: the sibling rats was sacrificed; events: {:?}",
        outcome.events()
    );
    assert_eq!(
        outcome.zone_of(rats),
        Zone::Graveyard,
        "a non-unearthed Rotting Rats has no exile rider — sacrifice goes to the graveyard"
    );
}

// ---------------------------------------------------------------------------
// R7 — host-exit prune + no revival on same-ObjectId re-entry (CR 400.7).
// A first destroy exiles rats via the rider; the prune then removes the rider
// from base AND live; after a harness re-entry (same ObjectId), a second
// destroy must go to the GRAVEYARD (no revived rider).
// ---------------------------------------------------------------------------

#[test]
fn r7_host_exit_prunes_rider_no_revival_on_reentry() {
    let mut scenario = GameScenario::new();
    let rats = stage_rats(&mut scenario);
    let destroy1 = scenario
        .add_spell_to_library_top(P0, "Doom Blade", false)
        .from_oracle_text(DESTROY_TARGET)
        .id();
    let destroy2 = scenario
        .add_spell_to_library_top(P0, "Terror", false)
        .from_oracle_text(DESTROY_TARGET)
        .id();
    let mut runner = scenario.build();

    unearth(&mut runner, rats);
    to_hand(&mut runner, destroy1);
    fund(&mut runner, P0, &[ManaType::Black], rats);

    // First leave event: redirected to exile by the rider.
    let outcome = runner.cast(destroy1).target_objects(&[rats]).resolve();
    assert_eq!(
        outcome.zone_of(rats),
        Zone::Exile,
        "first destroy is redirected to exile by the rider"
    );

    // Prune reach-guard (CR 400.7): the rider must be gone from BOTH base and
    // live after the host left the battlefield — otherwise it could revive.
    {
        let obj = &runner.state().objects[&rats];
        assert!(
            obj.base_replacement_definitions.is_empty(),
            "CR 400.7: the host-lifetime rider must be pruned from base on exit, got {:?}",
            obj.base_replacement_definitions
        );
        assert_eq!(
            obj.replacement_definitions.len(),
            0,
            "CR 400.7: the rider must be pruned from live defs on exit"
        );
    }

    // Re-enter the SAME ObjectId via a legitimate harness zone move (the hazard
    // under test is ObjectId persistence across a CR 400.7 boundary, not the
    // re-entry route). A fresh object must NOT inherit the pruned rider.
    move_to_zone(runner.state_mut(), rats, Zone::Battlefield, &mut Vec::new());
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&rats].zone,
        Zone::Battlefield,
        "precondition: rats re-entered the battlefield"
    );

    to_hand(&mut runner, destroy2);
    fund(&mut runner, P0, &[ManaType::Black], rats);
    let outcome = runner.cast(destroy2).target_objects(&[rats]).resolve();
    assert_eq!(
        outcome.zone_of(rats),
        Zone::Graveyard,
        "CR 400.7: the re-entered object is new — the pruned rider must NOT revive, \
         so the second destroy goes to the graveyard"
    );
}

// ---------------------------------------------------------------------------
// R8 — control theft: the rider is OBJECT-bound (valid_card: SelfRef), not
// controller-bound. After control passes to P1, P1 sacrificing rats still
// exiles it (CR 400.7 — the redirect follows the object).
// ---------------------------------------------------------------------------

#[test]
fn r8_control_change_keeps_rider_object_bound() {
    let mut scenario = GameScenario::new();
    let rats = stage_rats(&mut scenario);
    // P1's sacrifice outlet targets a creature P1 controls once control passes.
    // Staged in P1's library and moved to hand AFTER the return, so the unearth
    // ETB ("each player discards a card") stays a no-op — P1's hand is empty at
    // return time, exactly like R1/R3/R4 (see `stage_rats`).
    let sac = scenario
        .add_spell_to_library_top(P1, "Sacrifice", true)
        .from_oracle_text(SACRIFICE)
        .id();
    let mut runner = scenario.build();

    unearth(&mut runner, rats);
    to_hand(&mut runner, sac);

    // Hand P1 control of rats. Setting `base_controller` mirrors a genuine
    // control assignment through the Layer-2 baseline
    // (`obj.base_controller.unwrap_or(obj.owner)`), so a subsequent layer pass
    // keeps P1 in control rather than reverting to the owner. A control change
    // leaves the rider — an object-hosted replacement — untouched; only a
    // battlefield exit prunes it (CR 400.7).
    {
        let obj = runner.state_mut().objects.get_mut(&rats).unwrap();
        obj.controller = P1;
        obj.base_controller = Some(P1);
    }
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&rats].controller,
        P1,
        "precondition: P1 now controls rats"
    );

    // Hand P1 priority so P1 — the new controller — is the caster. The harness
    // casts as the player holding priority, and the additional sacrifice cost
    // requires the caster to control the sacrificed creature (CR 601.2h); only
    // P1 can pay it now. Sacrifice is an instant, so P1 may cast it at this
    // priority window (CR 601.3a).
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    fund(&mut runner, P1, &[ManaType::Black], rats);
    let outcome = runner.cast(sac).sacrifice_with(&[rats]).resolve();

    assert!(
        sacrificed(outcome.events(), rats),
        "reach guard: P1 sacrificed rats; events: {:?}",
        outcome.events()
    );
    assert_eq!(
        outcome.zone_of(rats),
        Zone::Exile,
        "CR 400.7: the exile rider is object-bound — a new controller sacrificing rats \
         still exiles it"
    );
}

// ---------------------------------------------------------------------------
// R9 — parsed-rider class via the REAL card Gruesome Encore. The rider is built
// by the parser path (try_parse_leave_battlefield_exile_replacement), driven
// through the full cast pipeline; a bounce of the returned creature must exile
// it instead.
// ---------------------------------------------------------------------------

#[test]
fn r9_gruesome_encore_parsed_rider_redirects_bounce_to_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A vanilla creature card in an OPPONENT's graveyard (Gruesome Encore
    // targets "an opponent's graveyard").
    let bear = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let encore = scenario
        .add_spell_to_hand_from_oracle(P0, "Gruesome Encore", false, GRUESOME_ENCORE)
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, BOUNCE_TARGET)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, bear, false, vec![]),
            ManaUnit::new(ManaType::Colorless, bear, false, vec![]),
            ManaUnit::new(ManaType::Black, bear, false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    // Cast Gruesome Encore: reanimate the bear under P0's control, installing
    // the parsed leaves-battlefield rider.
    let outcome = runner.cast(encore).target_objects(&[bear]).resolve();
    assert_eq!(
        outcome.zone_of(bear),
        Zone::Battlefield,
        "precondition: Gruesome Encore reanimates the bear"
    );
    assert_eq!(
        outcome.state().objects[&bear].controller,
        P0,
        "precondition: the bear is under P0's control"
    );

    // The genuine layer pass between install and the leave event.
    evaluate_layers(runner.state_mut());
    fund(&mut runner, P0, &[ManaType::Blue], bear);

    let outcome = runner.cast(bounce).target_objects(&[bear]).resolve();
    assert_eq!(
        outcome.zone_of(bear),
        Zone::Exile,
        "issue #5976 (parsed-rider class): the reanimated creature bounced must be \
         EXILED instead (CR 702.84a / CR 614.1a)"
    );
    // Owner (P1) hand must not gain the card — exiled instead of bounced.
    let p1_hand = &outcome
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .hand;
    assert!(
        !p1_hand.contains(&bear),
        "the reanimated card must not reach its owner's hand — it is exiled instead"
    );
}
