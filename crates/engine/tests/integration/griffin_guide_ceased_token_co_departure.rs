//! Griffin Guide — a co-departing member that ceased to exist must not be
//! dropped from the departure group (CR 704.5d).
//!
//! Griffin Guide (verbatim Oracle text, below) is an Aura whose
//! leaves-the-battlefield trigger observes its own host. When the host is a
//! TOKEN, the host and the Aura leave the battlefield in the same state-based
//! action iteration (CR 704.5f/g kills the host, CR 704.5m sweeps the now
//! hostless Aura), and then CR 704.5d removes the token from `state.objects`
//! entirely — *before* the end-of-iteration co-departure stamp runs. The stamp's
//! old predicate read "absent from `state.objects`" as "never departed", so the
//! two-member group collapsed to one member, fell below
//! `mark_simultaneous_departures`' `len() < 2` floor, and was never stamped at
//! all. The Aura's trigger then found no co-departed host and silently did
//! nothing. CR 400.7f is the rule that entitles it to find the host: the Aura is
//! findable because it went to the graveyard *at the same time* the enchanted
//! permanent left the battlefield.
//!
//! The fix is `zones::battlefield_residency`, the shared authority that reads
//! absence as `DepartedCeased` (a departure) rather than as survival, plus the
//! matching polarity fix on the CR 603.10a co-departed observer guard.
//!
//! **No card database, no fixture.** Every card here is built from verbatim
//! Oracle text through the real parser, so there is nothing to skip on: no test
//! in this file may `return` early, and none contains a `shared_card_db` /
//! `add_real_card` guard. `add_real_card` is specifically FORBIDDEN for this
//! Aura — with zero legal hosts it leaves the object in `Zone::Library`, with
//! exactly one it silently auto-attaches, and with two or more it panics in
//! `scenario_db.rs`.
//!
//! **Harness invariants that were measured, not guessed:**
//! * `with_subtypes(vec!["Aura"])` is mandatory and is the only correct spelling
//!   — it syncs `base_card_types`. A post-build push to `card_types.subtypes` is
//!   discarded at the next continuous-effect recomputation, the CR 704.5m sweep
//!   never sees an Aura, and BOTH arms then produce zero Griffins, which reads
//!   as "the bug reproduces everywhere". Every test asserts the subtype.
//! * `attach::attach_to` returns the PRIOR host, so `None` is the normal
//!   first-attach result and is not a failure signal.
//! * A run in which `griffin_guide_token_host_death_creates_griffin` AND
//!   `griffin_guide_nontoken_host_death_creates_griffin` both fail means the
//!   HARNESS is broken, not the engine — the nontoken arm passes on the
//!   unfixed base.
//! * `state.zone_changes_this_turn` records carry NO `co_departed` on the
//!   ordinary priority path: `mark_simultaneous_departures` stamps the EVENTS,
//!   and the `zone_changes_this_turn` mirror is written only by
//!   `mark_simultaneous_departure_records`, which the priority path never calls.
//!   Any group-membership assertion therefore uses the event-inspecting driver
//!   (`sba::check_state_based_actions` + `triggers::process_triggers`), never
//!   `pass_both_players()`.
//! * Hosts are killed with marked damage (CR 704.5g), never combat: Griffin
//!   Guide grants flying, so a combat kill would drag CR 702.9b in as a
//!   confound.
//!
//! **Deliberately absent tests, with their reasons:**
//! * CR 704.5n Equipment — measured unreachable. `sba::check_unattached_equipment`
//!   performs no zone move, so an unattached Equipment emits no
//!   battlefield-origin `ZoneChanged`, and `battlefield_residency` is never
//!   called with its id. The `Remained` arm is covered by
//!   `regenerated_creature_excluded_from_co_departure_in_sba_pass` instead.
//! * CR 704.5e ceased *copy* — no test is claimed. `check_token_cease_to_exist`
//!   removes tokens and copies of cards through the same loop, so a ceased copy
//!   takes the identical `DepartedCeased` arm by construction.

use engine::game::effects::attach;
use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::{sba, triggers};
use engine::types::ability::{ReplacementDefinition, TargetFilter};
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor, ZoneChangeRecord};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Griffin Guide, {2}{W} Enchantment — Aura. Verbatim Oracle text
/// (oracle_id c5323a43-82de-4340-8578-b3ffcc66f8fa), confirmed against Scryfall
/// and against the shipped card export.
const GRIFFIN_GUIDE_ORACLE: &str = "Enchant creature\n\
Enchanted creature gets +2/+2 and has flying.\n\
When enchanted creature dies, create a 2/2 white Griffin creature token with flying.";

/// Class-B leaves-the-battlefield observer (the Blood Artist / Zulaport shape),
/// Aura-free. Verbatim shape of the printed clause.
const LTB_OBSERVER_ORACLE: &str = "Whenever another creature you control dies, you gain 1 life.";

/// A plain token maker, so a test can use a REAL engine-created token rather
/// than a hand-set `is_token` flag.
const MAKE_BEAR_TOKEN: &str = "Create a 2/2 green Bear creature token.";

// ---------------------------------------------------------------------------
// Observables
// ---------------------------------------------------------------------------

/// Griffin tokens on the battlefield, keyed on the SUBTYPE the Oracle text
/// names. The Aura itself is subtyped `Aura`, so it can never be miscounted
/// here even though its name contains "Griffin".
fn griffins(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|obj| is_griffin_token(obj))
        .count()
}

fn griffins_controlled_by(state: &GameState, player: PlayerId) -> usize {
    state
        .objects
        .values()
        .filter(|obj| is_griffin_token(obj) && obj.controller == player)
        .count()
}

fn is_griffin_token(obj: &GameObject) -> bool {
    obj.zone == Zone::Battlefield
        && obj
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Griffin")
}

/// Every battlefield-origin `ZoneChanged` record in an event slice — the
/// producer's own output, and the only place `co_departed` is observable.
fn battlefield_departures(events: &[GameEvent]) -> Vec<&ZoneChangeRecord> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                record,
                ..
            } => Some(record.as_ref()),
            _ => None,
        })
        .collect()
}

fn departure_record<'a>(
    records: &'a [&'a ZoneChangeRecord],
    id: ObjectId,
) -> Option<&'a ZoneChangeRecord> {
    records.iter().copied().find(|rec| rec.object_id == id)
}

fn life(state: &GameState, player: PlayerId) -> i32 {
    state.players[player.0 as usize].life
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Assert the Aura really is an Aura in the layer-visible `base_card_types`.
/// Without this, a mis-built fixture makes every arm produce zero Griffins and
/// the suite reads as "the bug reproduces everywhere".
fn assert_is_aura(state: &GameState, aura: ObjectId) {
    assert!(
        state.objects[&aura]
            .base_card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Aura"),
        "harness guard: the Aura's BASE card types must carry the Aura subtype, or the \
         CR 704.5m unattached-Aura sweep never sees it and this test is vacuous"
    );
}

/// Attach through the engine's own authority and assert both sides of the link.
/// `attach_to` returns the PRIOR host, so `None` is the expected first-attach
/// result — it is not a failure signal.
fn attach_and_assert(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    attach::attach_to(runner.state_mut(), aura, host);
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "the Aura must be attached to its host through the real attach authority"
    );
}

/// CR 613: the Aura's +2/+2 must have reached the layer input before anything
/// dies. A 2/2 host reading 4/4 proves the static ability is live; a broken
/// setup otherwise reads as a broken engine.
fn assert_enchanted_host_is_four_four(runner: &mut GameRunner, host: ObjectId) {
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&host];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(4), Some(4)),
        "reach guard: the Aura's +2/+2 must have reached the layer input"
    );
}

fn mark_lethal(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("object must exist to be marked")
        .damage_marked = 99;
}

fn set_token(runner: &mut GameRunner, id: ObjectId, is_token: bool) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("object must exist")
        .is_token = is_token;
}

/// CR 701.19a: the engine's own one-shot regeneration replacement, installed
/// exactly as `effects::regenerate::resolve` builds it.
fn install_regeneration_shield(runner: &mut GameRunner, target: ObjectId) {
    let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
        .valid_card(TargetFilter::SelfRef)
        .description("Regenerate".to_string())
        .regeneration_shield();
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .expect("regeneration target must exist")
        .replacement_definitions
        .push(shield);
}

/// The event-inspecting driver (the only one that can see `co_departed`):
/// `sba::check_state_based_actions` is `sba.rs`'s own production entry, and
/// `triggers::process_triggers` is the production trigger collector. The stack
/// is then drained through the ordinary runner so triggered abilities actually
/// resolve.
fn run_sba_and_triggers(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);
    triggers::process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();
    events
}

// ---------------------------------------------------------------------------
// T1 / T2 — the discriminating pair (priority driver, CR 704.3)
// ---------------------------------------------------------------------------

/// Shared body for T1/T2: one knob, `is_token` on the host. Everything else is
/// byte-identical between the two arms.
fn griffin_guide_single_host(host_is_token: bool) -> (usize, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Enchanted Host", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Griffin Guide", GRIFFIN_GUIDE_ORACLE)
        .with_subtypes(vec!["Aura"])
        .id();

    let mut runner = scenario.build();
    set_token(&mut runner, host, host_is_token);
    assert_is_aura(runner.state(), aura);
    attach_and_assert(&mut runner, aura, host);
    assert_enchanted_host_is_four_four(&mut runner, host);

    // CR 704.5g: lethal marked damage, not combat — Griffin Guide grants flying,
    // so a combat kill would pull CR 702.9b in as a confound.
    mark_lethal(&mut runner, host);

    // CR 704.3: the ordinary priority path is the production entry.
    runner.pass_both_players();
    runner.advance_until_stack_empty();

    let host_present = runner.state().objects.contains_key(&host);
    (griffins(runner.state()), host_present)
}

/// **T1 — the discriminating test.** Revert the `zones.rs` residency predicate
/// and `griffin_tokens == 1` becomes `0`. Measured on the unfixed base: 0.
#[test]
fn griffin_guide_token_host_death_creates_griffin() {
    let (griffin_tokens, host_present) = griffin_guide_single_host(true);

    assert!(
        !host_present,
        "reach guard: CR 704.5d must have removed the token host from state.objects — if it \
         is still present this test never reaches the DepartedCeased arm and proves nothing"
    );
    assert_eq!(
        griffin_tokens, 1,
        "CR 400.7f + CR 603.10a: the Aura co-departed with its token host, so its \
         leaves-the-battlefield trigger must find that host and make exactly one Griffin"
    );
}

/// **T2 — the positive control.** Green before AND after the fix. If T1 and T2
/// both fail, the harness is broken, not the engine.
#[test]
fn griffin_guide_nontoken_host_death_creates_griffin() {
    let (griffin_tokens, host_present) = griffin_guide_single_host(false);

    assert!(
        host_present,
        "control guard: a NONTOKEN host survives in state.objects (in the graveyard); if it \
         is absent, the two arms are not differing by exactly one knob"
    );
    assert_eq!(
        griffin_tokens, 1,
        "control: the nontoken path already worked and must keep working"
    );
}

// ---------------------------------------------------------------------------
// T3 — the same behaviour with a REAL engine-created token host
// ---------------------------------------------------------------------------

/// **T3 — fidelity variant of T1.** The host is a token the engine actually
/// created by resolving a token spell, not a hand-set `is_token` flag. The Aura
/// is parked on a scaffold creature across the cast, because an unattached Aura
/// would be swept to the graveyard by CR 704.5m during the intervening
/// resolution. Measured on the unfixed base: 0.
#[test]
fn griffin_guide_engine_created_token_host_creates_griffin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let scaffold = scenario.add_creature(P0, "Scaffold Bear", 1, 1).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Griffin Guide", GRIFFIN_GUIDE_ORACLE)
        .with_subtypes(vec!["Aura"])
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bear Summons", false, MAKE_BEAR_TOKEN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    assert_is_aura(runner.state(), aura);
    // CR 704.5m: park the Aura on a scaffold across the cast, or the sweep eats
    // it during the intervening resolution.
    attach_and_assert(&mut runner, aura, scaffold);

    runner.cast(token_spell).resolve();

    let token = runner
        .state()
        .objects
        .values()
        .find(|obj| {
            obj.is_token
                && obj.zone == Zone::Battlefield
                && obj.card_types.subtypes.iter().any(|s| s == "Bear")
        })
        .map(|obj| obj.id)
        .expect("reach guard: the token spell must have created a Bear token");

    assert!(
        runner.state().objects[&token].is_token,
        "reach guard: the host must be a REAL engine-created token"
    );
    assert_eq!(
        runner.state().objects[&scaffold].zone,
        Zone::Battlefield,
        "reach guard: the scaffold must survive the cast, or the Aura was swept mid-test"
    );

    attach_and_assert(&mut runner, aura, token);
    assert_enchanted_host_is_four_four(&mut runner, token);

    mark_lethal(&mut runner, token);
    runner.pass_both_players();
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects.contains_key(&token),
        "reach guard: CR 704.5d must have removed the engine-created token host"
    );
    assert_eq!(
        griffins(runner.state()),
        1,
        "CR 400.7f + CR 603.10a: an engine-created token host must behave exactly as the \
         hand-set one does"
    );
}

// ---------------------------------------------------------------------------
// T4 — Class B: a ceased token inside a 3-member departure batch
// ---------------------------------------------------------------------------

/// **T4 — the Aura-free class.** Three creatures die in one SBA pass, one of
/// them a token. The old predicate dropped the token from the recomputed group
/// and — because `mark_simultaneous_departures` ASSIGNS `co_departed` rather
/// than merging — overwrote the two survivors' correct groups too, breaking the
/// mutual-record relation the CR 603.10a observer arm requires. Measured on the
/// unfixed base: the observer gained 1 life instead of 2.
#[test]
fn co_dying_token_still_observed_by_ltb_observer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let observer = scenario
        .add_creature_from_oracle(P0, "Life Watcher", 2, 2, LTB_OBSERVER_ORACLE)
        .id();
    let nontoken = scenario.add_creature(P0, "Plain Creature", 2, 2).id();
    let token = scenario.add_creature(P0, "Token Creature", 2, 2).id();

    let mut runner = scenario.build();
    set_token(&mut runner, token, true);

    let life_before = life(runner.state(), P0);
    for id in [observer, nontoken, token] {
        mark_lethal(&mut runner, id);
    }

    let events = run_sba_and_triggers(&mut runner);
    let departures = battlefield_departures(&events);

    assert_eq!(
        departures.len(),
        3,
        "reach guard: all three creatures must depart the battlefield in one SBA pass — \
         anything less and the batch under test never forms"
    );
    assert!(
        !runner.state().objects.contains_key(&token),
        "reach guard: CR 704.5d must have removed the token, or this test never reaches the \
         DepartedCeased arm"
    );

    let life_delta = life(runner.state(), P0) - life_before;
    assert_eq!(
        life_delta, 2,
        "CR 603.10a: the observer looks back and sees BOTH other creatures die — the ceased \
         token must not be dropped from the group"
    );
}

// ---------------------------------------------------------------------------
// T5 — Class C: the observer is itself a ceased token
// ---------------------------------------------------------------------------

/// **T5 — the observer-guard arm (CR 603.10a).** A token copy of a Class-B
/// observer dies alongside another creature and ceases to exist in the same SBA
/// pass. Its own object is gone, but its `ZoneChangeRecord` still owns its
/// identity and trigger entries (CR 608.2h), exactly as the CR 603.10f
/// player-loss arm already relies on. Measured on the unfixed base: 0 life.
///
/// The nontoken arm is run first, in the same test, as the paired positive
/// control: it gains 1 today and must keep gaining 1.
#[test]
fn ceased_token_observer_fires_for_co_dying_creature() {
    let (control_delta, _) = observer_dies_with_creature(false);
    assert_eq!(
        control_delta, 1,
        "positive control: a NONTOKEN co-dying observer already gains 1 life today — if this \
         fails, the fixture never reaches the observer arm and the token case below is vacuous"
    );

    let (life_delta, ceased) = observer_dies_with_creature(true);
    assert!(
        ceased,
        "reach guard: CR 704.5d must have removed the token observer from state.objects"
    );
    assert_eq!(
        life_delta, 1,
        "CR 603.10a + CR 704.5d: a co-departed observer that ceased to exist is still an \
         observer — its departure record carries its trigger entries"
    );
}

/// Runs the T5 fixture with one knob (`is_token` on the observer) and asserts
/// the producer-side reach guards that make a `0` unambiguous: if the group is
/// present and the record carries the observer's trigger entries, then a zero
/// life delta can only be the observer guard.
fn observer_dies_with_creature(observer_is_token: bool) -> (i32, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let observer = scenario
        .add_creature_from_oracle(P0, "Life Watcher", 2, 2, LTB_OBSERVER_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, "Plain Creature", 2, 2).id();

    let mut runner = scenario.build();
    set_token(&mut runner, observer, observer_is_token);

    let life_before = life(runner.state(), P0);
    mark_lethal(&mut runner, observer);
    mark_lethal(&mut runner, victim);

    let events = run_sba_and_triggers(&mut runner);
    let departures = battlefield_departures(&events);

    assert_eq!(
        departures.len(),
        2,
        "reach guard: both creatures must depart in one SBA pass"
    );

    // The producer already grouped them, in BOTH arms — so a zero life delta
    // cannot be blamed on a missing group.
    let victim_record =
        departure_record(&departures, victim).expect("the victim must have a departure record");
    assert!(
        victim_record.co_departed.contains(&observer),
        "reach guard: the victim's record must name the observer as co-departed, or this test \
         is measuring the producer rather than the observer guard"
    );

    let observer_record = departure_record(&departures, observer)
        .expect("the observer must have a departure record of its own");
    assert!(
        observer_record.trigger_source_context.is_some(),
        "reach guard: the observer's record must carry its source context (CR 608.2h)"
    );
    assert_eq!(
        observer_record.trigger_definitions.len(),
        1,
        "reach guard: the observer's record must carry its one leaves-the-battlefield trigger"
    );

    let ceased = !runner.state().objects.contains_key(&observer);
    (life(runner.state(), P0) - life_before, ceased)
}

// ---------------------------------------------------------------------------
// T6 — the `Remained` arm (precision guard; green before and after)
// ---------------------------------------------------------------------------

/// **T6 — regeneration precision at the SBA entry.** A regeneration-shielded
/// creature and two plain creatures all take lethal damage in one pass
/// (`check_creature_deaths` → `perform_creature_deaths` → `departed_subset`).
/// The shielded creature never departs, so it must appear in NO record's
/// `co_departed`. Passes before and after — it is the precision half of the
/// relaxation, and it is non-vacuous because of its departure-count reach guard.
#[test]
fn regenerated_creature_excluded_from_co_departure_in_sba_pass() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let shielded = scenario.add_creature(P0, "Shielded Creature", 2, 2).id();
    let doomed = scenario.add_creature(P0, "Doomed Creature", 2, 2).id();
    let third = scenario.add_creature(P0, "Third Creature", 2, 2).id();

    let mut runner = scenario.build();
    install_regeneration_shield(&mut runner, shielded);
    for id in [shielded, doomed, third] {
        mark_lethal(&mut runner, id);
    }

    let mut events = Vec::new();
    sba::check_state_based_actions(runner.state_mut(), &mut events);
    let departures = battlefield_departures(&events);

    assert_eq!(
        departures.len(),
        2,
        "reach guard: exactly the two unshielded creatures depart — the instrument fires"
    );
    let doomed_record =
        departure_record(&departures, doomed).expect("the doomed creature must have a record");
    let third_record =
        departure_record(&departures, third).expect("the third creature must have a record");
    assert!(
        doomed_record.co_departed.contains(&third),
        "reach guard: the two real departures must name each other, or the group was never \
         stamped and the absence assertion below is vacuous"
    );
    assert!(
        third_record.co_departed.contains(&doomed),
        "reach guard: the co-departure relation must be mutual"
    );

    // CR 701.19a: the shield replaced the destruction, so the creature never left.
    assert_eq!(
        runner.state().objects[&shielded].zone,
        Zone::Battlefield,
        "CR 701.19a: a regenerated creature stays on the battlefield"
    );
    assert!(
        !departures
            .iter()
            .any(|record| record.co_departed.contains(&shielded)),
        "CR 603.10a: a creature that never departed must not appear in ANY co_departed group"
    );
}

// ---------------------------------------------------------------------------
// T7 — multi-authority hostile fixture (the bug report verbatim)
// ---------------------------------------------------------------------------

/// **T7 — two Griffin Guides, two controllers, one token host between them.**
/// The bug report verbatim. On the unfixed base P0 gets 0 Griffins while P1
/// gets 1. Post-fix each controller gets EXACTLY one, and the `== 1` (rather
/// than `>= 1`) on both sides is the cross-observation guard: all four
/// permanents are
/// in one co-departure group, and each Aura's `AttachedTo` filter, restored
/// against its own record's `attached_to` LKI, must still reject the other
/// player's host.
///
/// Priority driver (CR 704.3), so NO group-membership assertion appears here:
/// `state.zone_changes_this_turn` carries no `co_departed` on this path and
/// would read vacuously empty.
#[test]
fn two_griffin_guides_one_token_host_each_controller_gets_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host0 = scenario.add_creature(P0, "P0 Host", 2, 2).id();
    let aura0 = scenario
        .add_enchantment_from_oracle(P0, "Griffin Guide", GRIFFIN_GUIDE_ORACLE)
        .with_subtypes(vec!["Aura"])
        .id();
    let host1 = scenario.add_creature(P1, "P1 Host", 2, 2).id();
    let aura1 = scenario
        .add_enchantment_from_oracle(P1, "Griffin Guide", GRIFFIN_GUIDE_ORACLE)
        .with_subtypes(vec!["Aura"])
        .id();

    let mut runner = scenario.build();
    // The single knob that separates the two controllers.
    set_token(&mut runner, host0, true);

    assert_is_aura(runner.state(), aura0);
    assert_is_aura(runner.state(), aura1);
    attach_and_assert(&mut runner, aura0, host0);
    attach_and_assert(&mut runner, aura1, host1);
    assert_enchanted_host_is_four_four(&mut runner, host0);
    assert_enchanted_host_is_four_four(&mut runner, host1);

    mark_lethal(&mut runner, host0);
    mark_lethal(&mut runner, host1);

    runner.pass_both_players();
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects.contains_key(&host0),
        "reach guard: P0's TOKEN host must have ceased to exist (CR 704.5d)"
    );
    assert!(
        runner.state().objects.contains_key(&host1),
        "reach guard: P1's NONTOKEN host must survive in the graveyard — the two sides must \
         differ by exactly the token knob"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "APNAP guard: both triggers resolve and the game returns to a priority window with no \
         unexpected pause, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        griffins_controlled_by(runner.state(), P0),
        1,
        "CR 400.7f: P0's Aura co-departed with its token host and must make exactly one Griffin"
    );
    assert_eq!(
        griffins_controlled_by(runner.state(), P1),
        1,
        "cross-observation guard: P1's Aura must fire exactly ONCE — the four-member group \
         must not let it observe P0's host as well"
    );
}
