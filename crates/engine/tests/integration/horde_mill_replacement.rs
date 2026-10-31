//! R3-#2 — the Horde damage→mill redirect resolves through the replacement
//! pipeline as a real `ProposedEvent::Mill` (not a bare library dump), so
//! event-level mill replacements apply and either pause channel propagates.
//!
//! Before the fix, `mill_from_loss` never proposed a Mill event (Bruvac-class
//! doublings silently did nothing) and discarded both pause signals (`let _ =`
//! on the basic path's `apply_mill_after_replacement` and on the advanced
//! path's batch), so a parked CR 616.1 prompt was stranded while the caller
//! proceeded as if the redirect settled.

use engine::game::effects::life::{apply_damage_life_loss, ReplacementDeferred};
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, ReplacementDefinition,
    ReplacementPlayerScope, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::{ChallengeDeck, FormatConfig, HordeLegendaryDeath};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const HORDE: PlayerId = PlayerId(0);
const SURVIVOR: PlayerId = PlayerId(1);

/// A minimal Horde game (seat 0 = Horde, seat 1 = survivor) under the basic
/// legendary rule.
fn horde_state() -> GameState {
    GameState::new(
        FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
        2,
        42,
    )
}

/// A minimal Horde game under the opt-in ADVANCED legendary rule (milled
/// legendary permanents deploy + phase out instead of being buried).
fn advanced_horde_state() -> GameState {
    let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
    ruleset.legendary_death = HordeLegendaryDeath::EtbThenPhaseOut;
    GameState::new(FormatConfig::horde(ruleset), 2, 42)
}

/// Put `n` distinct plain cards on top of `owner`'s library.
fn stock_library(state: &mut GameState, owner: PlayerId, n: usize) {
    for i in 0..n {
        let card_id = CardId(state.next_object_id);
        create_object(state, card_id, owner, format!("Lib {i}"), Zone::Library);
    }
}

/// Bruvac the Grandiloquent's replacement, hand-built to the parser's exact
/// shape (`parses_opponent_mill_replacement_with_multiplier`): opponent-scoped,
/// folding `2 × EventContextAmount` into the substituted Mill event.
fn bruvac_replacement() -> ReplacementDefinition {
    let mut def =
        ReplacementDefinition::new(ReplacementEvent::Mill).execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                count: QuantityExpr::Multiply {
                    factor: 2,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }),
                },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        ));
    def.valid_player = Some(ReplacementPlayerScope::Opponent);
    def.description("Bruvac the Grandiloquent doubling".to_string())
}

/// A graveyard→exile `Moved` redirect (Rest in Peace / Leyline of the Void
/// class), mirroring `effects::mill`'s `graveyard_exile_redirect` unit-test
/// fixture. Two of these are simultaneously applicable to each milled card, so
/// the CR 616.1 materiality classifier prompts for ordering per card.
fn graveyard_exile_redirect(description: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description(description.to_string())
}

/// Attach `def` to a fresh battlefield permanent under `controller`'s control,
/// writing BOTH the live and base definition lists (a live-only write would be
/// dropped by a layers reset).
fn attach_replacement(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    def: ReplacementDefinition,
) -> ObjectId {
    let source = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&source).unwrap();
    obj.replacement_definitions = vec![def.clone()].into();
    obj.base_replacement_definitions = vec![def].into();
    source
}

/// CR 614: the redirect proposes a real Mill event, so an event-level mill
/// replacement (Bruvac, controlled by a survivor — the Horde is its opponent)
/// doubles the redirected mill.
///
/// Revert guard: routing around `replace_event` (the old direct
/// `apply_mill_after_replacement` call on a pre-replacement event) mills 3,
/// not 6 — the graveyard assertion flips.
#[test]
fn horde_redirect_mill_doubles_under_bruvac() {
    let mut state = horde_state();
    stock_library(&mut state, HORDE, 10);
    let bruvac = attach_replacement(&mut state, SURVIVOR, "Bruvac", bruvac_replacement());

    // PRECONDITIONS: the fixture built what this test believes it built.
    // Without (i), a shape mismatch yields a non-doubling replacement and the
    // count assertion below would pass for the wrong reason (3 == 3).
    let repl: Vec<_> = state
        .objects
        .get(&bruvac)
        .expect("Bruvac on battlefield")
        .replacement_definitions
        .iter_unchecked()
        .filter(|r| r.event == ReplacementEvent::Mill)
        .collect();
    assert_eq!(
        repl.len(),
        1,
        "Bruvac must carry exactly one Mill replacement"
    );
    assert_eq!(
        repl[0].valid_player,
        Some(ReplacementPlayerScope::Opponent),
        "the replacement must be opponent-scoped, or it never applies to the Horde's mill"
    );
    let library_before = state.players[HORDE.0 as usize].library.len();
    assert!(
        library_before >= 6,
        "the library must hold the doubled mill, or the CR 701.17b clamp masks the delta"
    );

    let mut events = Vec::new();
    let lost = apply_damage_life_loss(&mut state, HORDE, 3, &mut events)
        .expect("no competing replacement — the redirect must settle");

    // The settle still reports the loss amount as milled (the standing
    // redirect fiction); the MILL count is what doubled.
    assert_eq!(lost, 3, "the redirect must report the loss amount");
    assert_eq!(
        state.players[HORDE.0 as usize].library.len(),
        library_before - 6,
        "Bruvac must double the redirected 3-mill into 6"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].graveyard.len(),
        6,
        "the doubled mill lands in the Horde's graveyard"
    );
}

/// CR 616.1: a per-card ordering pause inside the redirected basic mill
/// propagates to the caller as `Deferred` — the prompt stays parked and
/// nothing is delivered — instead of being discarded while resolution
/// proceeds.
///
/// Revert guard: the old `let _ =` returned `Ok(3)` — the `is_err`
/// assertion flips.
#[test]
fn horde_redirect_basic_mill_pause_propagates_as_deferred() {
    let mut state = horde_state();
    stock_library(&mut state, HORDE, 10);
    for (description, name) in [
        ("Rest in Peace redirect", "Rest in Peace"),
        ("Leyline of the Void redirect", "Leyline of the Void"),
    ] {
        attach_replacement(
            &mut state,
            SURVIVOR,
            name,
            graveyard_exile_redirect(description),
        );
    }

    // PRECONDITIONS: both redirects attached (without two co-applicable
    // redirects there is no ordering choice and the pause below cannot fire).
    let redirect_count: usize = state
        .objects
        .values()
        .flat_map(|o| o.replacement_definitions.iter_unchecked())
        .filter(|r| r.event == ReplacementEvent::Moved)
        .count();
    assert_eq!(
        redirect_count, 2,
        "exactly two graveyard-exile redirects must be attached"
    );
    let library_before = state.players[HORDE.0 as usize].library.len();

    let mut events = Vec::new();
    let result = apply_damage_life_loss(&mut state, HORDE, 3, &mut events);

    // DISCRIMINATOR: the pause must reach the caller as a deferral.
    assert_eq!(
        result,
        Err(ReplacementDeferred::ReplacementChoice),
        "a per-card CR 616.1 pause must surface as Deferred, not settle as Ok"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the per-card ordering prompt must be parked in waiting_for"
    );
    assert!(
        state.active_batch_delivery().is_some(),
        "the undelivered tail must be stashed for the resume path"
    );
    // A parked tail has not been delivered: nothing has left the library and
    // no destination zone has received anything. The assertions above prove
    // the seam ran, so these zeros are not the instrument failing to fire.
    assert_eq!(
        state.players[HORDE.0 as usize].library.len(),
        library_before,
        "no card has been delivered while the first card's choice is parked"
    );
    assert!(
        state.players[HORDE.0 as usize].graveyard.is_empty(),
        "nothing has reached the graveyard while the batch is parked"
    );
    assert!(
        state.exile.is_empty(),
        "nothing has reached exile while the batch is parked"
    );
    assert!(
        events.is_empty(),
        "a parked batch emits no delivery events, got {events:?}"
    );
}

/// CR 616.1 + the advanced legendary rule: a per-card pause inside the
/// advanced branch propagates as `Deferred`, and NOTHING phases out while the
/// batch is parked — the deployment completion runs once, after the resumed
/// tail delivers. Answering the prompt resumes the drain, which delivers the
/// tail and phases out the entered legendary.
///
/// Setup: the library top is a legendary permanent (battlefield-bound, so no
/// graveyard redirect applies to it and it enters cleanly), followed by plain
/// cards whose first delivery collides the two graveyard→exile redirects.
///
/// Revert guard: the old `let _ =` on the batch returned `Ok(3)` — the
/// `is_err` assertion flips; dropping the completion from the parked tail
/// leaves the entered legendary phased in after the resume — the phase-out
/// assertion flips.
#[test]
fn horde_redirect_advanced_mill_pause_defers_then_phases_out_on_resume() {
    let mut state = advanced_horde_state();

    // Library top first: `create_object` pushes to the back of the deque and
    // the mill takes from the front, so the legendary is milled first.
    let legendary_card = CardId(state.next_object_id);
    let legendary = create_object(
        &mut state,
        legendary_card,
        HORDE,
        "Milled Boss".to_string(),
        Zone::Library,
    );
    {
        let obj = state.objects.get_mut(&legendary).unwrap();
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    stock_library(&mut state, HORDE, 9);
    for (description, name) in [
        ("Rest in Peace redirect", "Rest in Peace"),
        ("Leyline of the Void redirect", "Leyline of the Void"),
    ] {
        attach_replacement(
            &mut state,
            SURVIVOR,
            name,
            graveyard_exile_redirect(description),
        );
    }

    // PRECONDITIONS: the advanced ruleset is active, the legendary is the mill
    // head, and it starts with no triggers (so no trigger below is ours).
    assert_eq!(
        state
            .format_config
            .horde_ruleset
            .as_ref()
            .expect("horde ruleset")
            .legendary_death,
        HordeLegendaryDeath::EtbThenPhaseOut,
        "the advanced legendary rule must be active"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].library.front(),
        Some(&legendary),
        "the legendary must be the first card milled"
    );
    assert!(
        state
            .objects
            .get(&legendary)
            .unwrap()
            .trigger_definitions
            .is_empty(),
        "the legendary starts with no triggers"
    );

    let mut runner = GameRunner::from_state(state);
    let mut events = Vec::new();
    let result = apply_damage_life_loss(runner.state_mut(), HORDE, 3, &mut events);

    // The pause propagates through the advanced branch too.
    assert_eq!(
        result,
        Err(ReplacementDeferred::ReplacementChoice),
        "an advanced-branch per-card pause must surface as Deferred"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the per-card ordering prompt must be parked in waiting_for"
    );
    // The legendary entered BEFORE the pause (battlefield-bound, no redirect
    // applied) — phased in, with no trigger grafted: the completion runs
    // once, after the resumed tail delivers.
    assert!(
        runner.state().battlefield.contains(&legendary),
        "the battlefield-bound legendary must have entered before the pause"
    );
    assert!(
        runner.state().objects[&legendary].is_phased_in(),
        "nothing phases out while the batch is parked"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&legendary)
            .unwrap()
            .trigger_definitions
            .is_empty(),
        "no trigger may be grafted onto the entered legendary"
    );
    // Only the legendary left the library: the pausing plain card and its tail
    // are still parked.
    assert_eq!(
        runner.state().players[HORDE.0 as usize].library.len(),
        9,
        "only the pre-pause legendary has left the library"
    );
    assert!(
        runner.state().players[HORDE.0 as usize]
            .graveyard
            .is_empty(),
        "nothing has reached the graveyard while the batch is parked"
    );

    // Answer every prompt (bounded): the resume drain delivers the tail and
    // runs the stashed completion exactly once.
    for _ in 0..10 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the CR 616.1 ordering prompt");
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "all replacement prompts must be answerable"
    );
    assert!(
        runner.state().objects[&legendary].is_phased_out(),
        "the entered legendary must phase out once the resumed tail delivers"
    );
}
