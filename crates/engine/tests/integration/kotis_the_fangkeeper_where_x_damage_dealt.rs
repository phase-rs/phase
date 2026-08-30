//! Regression for issue #5923 and for the resolution-scoped cast window.
//!
//! Kotis, the Fangkeeper's combat-damage trigger must exile the top X cards of
//! the damaged player's library (X = damage dealt) and let Kotis's controller
//! cast, from among just that exiled batch, only the cards with mana value X or
//! less — **while the trigger is resolving**.
//!
//! Two distinct defects are covered here:
//!
//! 1. Issue #5923: before the `oracle_nom/quantity.rs` fix, the "where X is the
//!    amount of damage dealt" binding was left unresolved, so the totality guard
//!    in `oracle_effect/lower.rs` collapsed both the `ExileTop` step and the
//!    `CastFromZone` sub-ability to `Effect::Unimplemented` and neither the
//!    exile nor the free-cast offer ever happened.
//!    <https://github.com/phase-rs/phase/issues/5923>
//!
//! 2. The cast grant was then lowered to an INDEFINITE lingering
//!    `CastingPermission` ("stay castable until they leave exile"). That is
//!    wrong for this Oracle grammar. CR 608.2g: a resolving object "continues to
//!    resolve, which may include casting other spells this way", and "no other
//!    spells can normally be cast … during resolution" — there is no later
//!    window in which the permission could be used. WotC's own ruling for Kotis
//!    is explicit: "You cast the spells from among the exiled cards while
//!    Kotis's last ability is resolving and still on the stack. You can't wait
//!    to cast them later in the turn."

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KOTIS_ORACLE: &str = "Indestructible\nWhenever Kotis deals combat damage to a player, exile the top X cards of their library, where X is the amount of damage dealt. You may cast any number of spells with mana value X or less from among them without paying their mana costs.";

struct KotisFixture {
    runner: GameRunner,
    cheap: ObjectId,
    expensive: ObjectId,
    filler: ObjectId,
    controller_top: ObjectId,
}

/// CR 120.2a: Kotis deals 2 combat damage (each attacking creature deals combat
/// damage equal to its power), so X = 2. The damaged player's top two library
/// cards are exiled (one within budget, MV 1; one over budget, MV 5) and a third
/// card stays in the library, proving the exile is bounded to exactly X cards
/// from the DAMAGED player's library, not Kotis's controller's.
fn kotis_combat_damage_fixture() -> KotisFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (Kotis's controller) has its own library card that must NOT be
    // touched by Kotis's trigger.
    let controller_top = scenario.add_card_to_library_top(P0, "Controller Top");

    // P1 (the damaged player) library, top-to-bottom after seeding:
    // Cheap Card (MV 1, within budget) -> Expensive Card (MV 5, over budget)
    // -> Filler Card (must remain in library; outside the top-X window).
    let filler = scenario.add_card_to_library_top(P1, "Filler Card");
    let expensive = scenario.add_card_to_library_top(P1, "Expensive Card");
    let cheap = scenario.add_card_to_library_top(P1, "Cheap Card");

    let kotis = scenario
        .add_creature(P0, "Kotis, the Fangkeeper", 2, 1)
        .from_oracle_text_with_keywords(&["Indestructible"], KOTIS_ORACLE)
        .id();

    let mut runner = scenario.build();
    {
        let card = runner.state_mut().objects.get_mut(&cheap).unwrap();
        card.card_types.core_types.push(CoreType::Instant);
        card.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 1,
        };
    }
    {
        let card = runner.state_mut().objects.get_mut(&expensive).unwrap();
        card.card_types.core_types.push(CoreType::Instant);
        card.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 5,
        };
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(kotis, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Kotis attacking P1");

    KotisFixture {
        runner,
        cheap,
        expensive,
        filler,
        controller_top,
    }
}

/// Drive combat until Kotis's trigger has opened its resolution-scoped cast
/// window: declare no blockers, order the single trigger, accept the "you may"
/// offer, and pass priority until the window is parked.
fn drain_until_kotis_cast_window(runner: &mut GameRunner) -> Vec<ObjectId> {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order Kotis's trigger");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            // CR 603.5 + CR 608.2d: the "you may cast ... from among them"
            // sub-ability is a "may" effect — accept it so the cast window opens.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional cast offer");
            }
            // PRIMARY: the trigger pauses mid-resolution on the free-cast window.
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::FreeCastWindow { candidates, .. },
            } => {
                assert_eq!(player, P0, "the window belongs to Kotis's controller");
                return candidates;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while draining Kotis's trigger");
            }
            other => panic!(
                "unexpected waiting state while draining Kotis's trigger: {other:?} \
                 (phase={:?})",
                runner.state().phase
            ),
        }
    }
    panic!("Kotis's trigger never opened its resolution-scoped cast window");
}

/// CR 608.2g + CR 608.2h: the batch is exiled, the window opens DURING the
/// trigger's resolution, and the frozen X (= 2 damage dealt) admits only the
/// mana-value-1 card from that same batch.
///
/// Revert guard: on the old `LingeringPermission` lowering this test fails at
/// `drain_until_kotis_cast_window`, because the trigger finished resolving and
/// handed back priority instead of ever parking a `FreeCastWindow`.
#[test]
fn kotis_opens_a_resolution_scoped_window_bounded_by_x() {
    let KotisFixture {
        mut runner,
        cheap,
        expensive,
        filler,
        controller_top,
    } = kotis_combat_damage_fixture();

    let candidates = drain_until_kotis_cast_window(&mut runner);

    // Exactly the top two P1 library cards were exiled; the third stays put,
    // and P0's own library is untouched. "Their library" is an Oracle-text
    // grammar interpretation — the pronoun binds to the nearest preceding
    // player noun, the damaged player from "deals combat damage to a
    // player," not Kotis's controller — not a claim covered by a specific CR
    // number (CR 608.2c governs the ORDER effects apply their instructions,
    // not pronoun antecedents).
    let state = runner.state();
    assert_eq!(state.objects[&cheap].zone, Zone::Exile);
    assert_eq!(state.objects[&expensive].zone, Zone::Exile);
    assert_eq!(
        state.objects[&filler].zone,
        Zone::Library,
        "only the top X (2) cards may be exiled, not the whole library"
    );
    assert_eq!(
        state.objects[&controller_top].zone,
        Zone::Library,
        "Kotis must exile from the DAMAGED player's library, not its controller's"
    );

    // CR 608.2h: X was determined once, as the trigger resolved (2 damage), and
    // bounds the offer. Both cards are in the same exiled batch; only the one
    // within the ceiling may be offered.
    assert!(
        candidates.contains(&cheap),
        "a mana value 1 card (<= X=2) exiled by Kotis must be offered"
    );
    assert!(
        !candidates.contains(&expensive),
        "a mana value 5 card (> X=2) must NOT be offered even though it was exiled in the same batch"
    );
}

/// CR 608.2g: accepting casts the spell AS the trigger resolves — the card goes
/// straight to the stack from exile, without the controller ever regaining
/// priority in between.
#[test]
fn kotis_free_casts_the_chosen_spell_during_the_trigger_resolution() {
    let KotisFixture {
        mut runner, cheap, ..
    } = kotis_combat_damage_fixture();

    drain_until_kotis_cast_window(&mut runner);
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(cheap),
        })
        .expect("free-casting the exiled card must succeed");

    assert_eq!(
        runner.state().objects[&cheap].zone,
        Zone::Stack,
        "the chosen card must be cast onto the stack during the trigger's resolution"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the cast is free (CR 118.9) and must consume no mana"
    );

    // Drain the stack so the free-cast spell resolves.
    for _ in 0..24 {
        if runner.state().stack.is_empty() {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert_ne!(
        runner.state().objects[&cheap].zone,
        Zone::Exile,
        "the cast card must have left exile"
    );
}

/// CR 608.2g: THE defect this change fixes. Declining the window ends the
/// controller's opportunity — the exiled batch stays in exile with NO standing
/// casting permission, because "you can't wait to cast them later in the turn".
///
/// Revert guard: under the old `LingeringPermission` lowering the declined
/// `cheap` card remained in `spell_objects_available_to_cast` for the rest of
/// the game, so both assertions below flip.
#[test]
fn kotis_declining_leaves_no_lingering_cast_permission() {
    let KotisFixture {
        mut runner,
        cheap,
        expensive,
        ..
    } = kotis_combat_damage_fixture();

    drain_until_kotis_cast_window(&mut runner);
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the resolution-scoped window must succeed");

    // Return to a priority window — the point at which a lingering permission
    // would have become exercisable.
    //
    // REACH GUARD: the loop below has two exits — the intended empty-stack
    // `WaitingFor::Priority`, and a `PassPriority` rejection. Only the first one
    // proves the decline actually carried the resolution chain to completion, so
    // record it and require it BEFORE reading `spell_objects_available_to_cast`.
    // Without this, a continuation that stalls after
    // `FreeCastWindowChoice { selection: None }` (leaving the engine parked on
    // some non-priority `WaitingFor`) would fall out of the loop on the error
    // arm and still satisfy the two "no permission" assertions vacuously — the
    // permission scan is trivially empty in a state that never reached a
    // priority window at all.
    let mut reached_empty_stack_priority = false;
    for _ in 0..24 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            reached_empty_stack_priority = true;
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        reached_empty_stack_priority,
        "declining the window must let the rest of the resolution chain finish and hand \
         priority back with an empty stack; parked at {:?} with stack {:?}",
        runner.state().waiting_for,
        runner.state().stack.len(),
    );

    let state = runner.state();
    assert_eq!(
        state.objects[&cheap].zone,
        Zone::Exile,
        "a declined card stays in exile"
    );
    let available = spell_objects_available_to_cast(state, P0);
    assert!(
        !available.contains(&cheap),
        "declining the resolution window must leave NO casting permission behind — \
         CR 608.2g gives no later opportunity (Kotis ruling: \"You can't wait to cast \
         them later in the turn\")"
    );
    assert!(
        !available.contains(&expensive),
        "the over-budget card was never castable and must stay uncastable"
    );
}
