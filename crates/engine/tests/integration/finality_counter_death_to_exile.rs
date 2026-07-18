//! CR 122.1h — a permanent with a finality counter that would be put into a
//! graveyard from the battlefield is exiled instead. One replacement effect,
//! keyed on the counter, catches every death path because all of them converge
//! on the same `ZoneChange{from:Battlefield, to:Graveyard}` event:
//!
//!   * destroy            (Effect::Destroy → apply_destroy_after_replacement)
//!   * sacrifice          (sacrifice_permanent → apply_sacrifice_after_replacement)
//!   * SBA lethal damage  (CR 704.5g check_lethal_damage → replace_event)
//!   * SBA zero toughness (CR 704.5f check_zero_toughness → move_object consult)
//!
//! Discrimination: each positive test asserts the creature ends in `Zone::Exile`;
//! each is paired with a NEGATIVE TWIN (identical scenario, no finality counter)
//! that asserts `Zone::Graveyard`. The twin is the reach-guard — it proves the
//! death driver actually kills, so "Exile" in the positive is a real redirect,
//! not a vacuous no-death. Reverting the replacement.rs virtual-candidate arm
//! flips every positive Exile→Graveyard, so all four positives fail.
//!
//! Dies-trigger differential (CR 700.4: "dies" = put into a graveyard FROM the
//! battlefield). A finality permanent that is exiled instead never "dies", so a
//! "when this dies" observer must NOT fire. This is covered on the two delivery
//! classes — cast-driven (T1) and SBA-driven (T3) — rather than all four,
//! because every death path delivers the identical post-redirect event
//! `ZoneChanged{to:Exile}` and every `WhenDies` classifier keys on `to:Graveyard`
//! only; covering one representative of each delivery class covers all four.
//!
//! Non-consumption (CR 122.1h has no "remove a counter" clause, unlike shield
//! CR 122.1c): the applier receives only an immutable game-state borrow, so it
//! structurally CANNOT decrement a counter — the redirect only rewrites the
//! event's destination and records its replacement identity. A runtime "counter still
//! present" assertion is not meaningful here: once the permanent is exiled its
//! counters cease to exist (CR 400.7 — a new object with no counters), so an
//! exiled object shows no finality counter regardless of consumption. The
//! structural guarantee (no mutable state access in the applier) is the real proof.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DIES_DRAW: &str = "When this creature dies, you draw a card.";
const MURDER: &str = "Destroy target creature.";
const EDICT: &str = "Target player sacrifices a creature.";
const BURN: &str = "Zap deals 3 damage to target creature.";

fn library_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .expect("player exists")
}

/// T1 — destroy (cast-driven). "Destroy target creature." resolves into
/// `apply_destroy_after_replacement`, which proposes the inner
/// `ZoneChange{BF→GY}` and consults the replacement pipeline (CR 614.6). With a
/// finality counter that inner move is redirected to exile.
///
/// Also carries the cast-delivery-class dies-trigger differential: a "when this
/// dies, you draw a card" observer draws (library 1→0) in the graveyard twin but
/// NOT under finality (exile is not death, CR 700.4).
#[test]
fn t1_destroy_finality_creature_is_exiled() {
    // Positive: finality counter present → redirected to exile, no dies trigger.
    let (state, bear, lib_before) = run_destroy(true);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Exile,
        "a destroyed finality creature must be exiled instead of hitting the graveyard (CR 122.1h)"
    );
    assert_eq!(
        library_len(&state, P0),
        lib_before,
        "an exiled finality creature never dies, so its 'when this dies, draw' trigger must not fire"
    );

    // Negative twin: no finality counter → normal death, dies trigger draws.
    let (state, bear, lib_before) = run_destroy(false);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Graveyard,
        "without a finality counter, 'Destroy target creature' sends it to the graveyard"
    );
    assert_eq!(
        library_len(&state, P0),
        lib_before - 1,
        "a creature that actually dies fires its 'when this dies, draw' trigger (reach-guard)"
    );
}

/// Build a P0 finality (or plain) 2/2 with a dies-draw trigger and one library
/// card, then cast "Destroy target creature." at it. Returns the final state,
/// the creature id, and P0's library size captured before the cast.
fn run_destroy(with_finality: bool) -> (GameState, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let bear = scenario
        .add_creature(P0, "Finality Bear", 2, 2)
        .from_oracle_text(DIES_DRAW)
        .id();
    if with_finality {
        scenario.with_counter(bear, CounterType::Finality, 1);
    }
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let lib_before = library_len(runner.state(), P0);
    runner.cast(murder).target_object(bear).resolve();
    runner.advance_until_stack_empty();
    (runner.state().clone(), bear, lib_before)
}

/// T2 — sacrifice (cast-driven edict). "Target player sacrifices a creature."
/// routes through `sacrifice_permanent` → `apply_sacrifice_after_replacement`,
/// which proposes the same `ZoneChange{BF→GY}` and consults the pipeline. A
/// single-eligible sacrifice is auto-chosen. The finality redirect sends it to
/// exile.
#[test]
fn t2_sacrifice_finality_creature_is_exiled() {
    // Positive: P0's only creature carries a finality counter.
    let (state, bear) = run_sacrifice(true);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Exile,
        "a sacrificed finality creature must be exiled instead of hitting the graveyard (CR 122.1h)"
    );

    // Negative twin: no finality counter → sacrifice sends it to the graveyard.
    let (state, bear) = run_sacrifice(false);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Graveyard,
        "without a finality counter, an edict sacrifice sends it to the graveyard (reach-guard)"
    );
}

fn run_sacrifice(with_finality: bool) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's ONLY creature — so the edict's single eligible target auto-resolves.
    let bear = scenario.add_creature(P0, "Finality Bear", 2, 2).id();
    if with_finality {
        scenario.with_counter(bear, CounterType::Finality, 1);
    }
    let edict = scenario
        .add_spell_to_hand_from_oracle(P0, "Diabolic Edict", true, EDICT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(edict).target_player(P0).resolve();
    runner.advance_until_stack_empty();
    (runner.state().clone(), bear)
}

/// T3 — SBA lethal damage. A burn spell marks lethal damage; the CR 704.5g
/// state-based action (`check_lethal_damage`) proposes `ZoneChange{BF→GY}` via
/// an explicit `replace_event`. Distinct code from T4. Finality redirects to
/// exile.
///
/// Carries the SBA-delivery-class dies-trigger differential (library 1→0 in the
/// graveyard twin, unchanged under finality).
#[test]
fn t3_sba_lethal_damage_finality_creature_is_exiled() {
    // Positive: lethal damage + finality → exiled, dies trigger does not fire.
    let (state, bear, lib_before) = run_lethal_damage(true);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Exile,
        "a finality creature dealt lethal damage is exiled by SBA instead of dying (CR 122.1h)"
    );
    assert_eq!(
        library_len(&state, P0),
        lib_before,
        "exiled-not-dead: the 'when this dies, draw' observer must not fire under finality"
    );

    // Negative twin: lethal damage, no finality → dies, draws.
    let (state, bear, lib_before) = run_lethal_damage(false);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Graveyard,
        "without a finality counter, lethal damage sends it to the graveyard (reach-guard)"
    );
    assert_eq!(
        library_len(&state, P0),
        lib_before - 1,
        "a creature killed by lethal-damage SBA fires its dies trigger and draws (reach-guard)"
    );
}

fn run_lethal_damage(with_finality: bool) -> (GameState, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let bear = scenario
        .add_creature(P0, "Finality Bear", 2, 2)
        .from_oracle_text(DIES_DRAW)
        .id();
    if with_finality {
        scenario.with_counter(bear, CounterType::Finality, 1);
    }
    let burn = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, BURN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let lib_before = library_len(runner.state(), P0);
    runner.cast(burn).target_object(bear).resolve();
    runner.advance_until_stack_empty();
    (runner.state().clone(), bear, lib_before)
}

/// T4 — SBA zero toughness. A 0-toughness creature is put into its graveyard by
/// the CR 704.5f state-based action, which routes through `move_object`'s
/// internal replacement consult — a DISTINCT code path from T3's explicit
/// `replace_event`. Finality redirects that move to exile.
#[test]
fn t4_sba_zero_toughness_finality_creature_is_exiled() {
    // Positive: 0-toughness + finality → exiled by SBA.
    let (state, bear) = run_zero_toughness(true);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Exile,
        "a 0-toughness finality creature is exiled by SBA instead of hitting the graveyard (CR 122.1h)"
    );

    // Negative twin: 0-toughness, no finality → graveyard.
    let (state, bear) = run_zero_toughness(false);
    assert_eq!(
        state.objects[&bear].zone,
        Zone::Graveyard,
        "without a finality counter, a 0-toughness creature is put into the graveyard (reach-guard)"
    );
}

fn run_zero_toughness(with_finality: bool) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P0, "Finality Bear", 1, 1).id();
    if with_finality {
        scenario.with_counter(bear, CounterType::Finality, 1);
    }

    let mut runner = scenario.build();
    // Force toughness to 0 (CR 704.5f). Setting both base and computed toughness
    // keeps it 0 whether or not a layer flush runs during the SBA check.
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&bear)
            .expect("creature exists");
        obj.toughness = Some(0);
        obj.base_toughness = Some(0);
    }

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    (runner.state().clone(), bear)
}
