//! #10213 — CR 201.2a: one shared "same name" authority for the object-to-object
//! name filters.
//!
//! CR 201.2a: "Two or more objects have the same name if they have at least one
//! name in common, even if one or more of those objects have additional names.
//! An object with no name doesn't have the same name as any other object,
//! including another object with no name." A face-down permanent has no name
//! (CR 708.2a), so it has the same name as nothing.
//!
//! Before the fix the comparators were `eq_ignore_ascii_case` against a
//! reference name, so two nameless objects matched each other (`""` equals
//! `""`) and every mass same-name arm swept the whole face-down population.
//!
//! Driving cards: **Bile Blight** ("Target creature and all other creatures
//! with the same name as that creature get -3/-3 until end of turn.") exercises
//! the live-parent rung — the pump never moves the target, so the mass leg reads
//! its live `""`. **Maelstrom Pulse** and **Deputy of Detention** exercise the
//! snapshot rung — the chain walker stamps the parent instruction's zone-change
//! record (which carries the pre-exit, nameless name) onto the sub-ability.
//!
//! Every face-down fixture builds the production shape: the real face is
//! snapshotted into `back_face` before the CR 708.2a characteristics are
//! applied (`effects/turn_face_down.rs`, `morph::manifest_card`). Without the
//! stash a fixture would carry a state production never has.

use engine::game::morph::apply_face_down_creature_characteristics;
use engine::game::printed_cards::snapshot_object_base_face;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::FaceDownProfile;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BILE_BLIGHT: &str = "Target creature and all other creatures with the same name as that \
creature get -3/-3 until end of turn.";

const MAELSTROM_PULSE: &str =
    "Destroy target nonland permanent and all other permanents with the same name as that \
permanent.";

const DEPUTY_OF_DETENTION: &str = "When this creature enters, exile target nonland permanent an \
opponent controls and all other nonland permanents that player controls with the same name as \
that permanent until this creature leaves the battlefield.";

const MURDER: &str = "Destroy target creature.";

fn free() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![],
    }
}

/// Turn an existing battlefield creature face down the way production does:
/// snapshot the real face into `back_face`, apply the CR 708.2a vanilla 2/2
/// characteristics, then stash the snapshot.
fn make_face_down_nameless(runner: &mut GameRunner, id: ObjectId) {
    let state = runner.state_mut();
    let stash = snapshot_object_base_face(&state.objects[&id]);
    apply_face_down_creature_characteristics(
        state.objects.get_mut(&id).unwrap(),
        &FaceDownProfile::vanilla_2_2(),
    );
    state.objects.get_mut(&id).unwrap().back_face = Some(stash);
}

fn on_battlefield(state: &GameState, id: ObjectId) -> bool {
    state.battlefield.contains(&id)
}

fn battlefield_count_named(state: &GameState, player: PlayerId, name: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == player && obj.name == name)
        .count()
}

/// CR 201.2a + CR 708.2a: Bile Blight's mass leg — "all OTHER creatures with the
/// same name as that creature" — must exclude a nameless sibling when the
/// announced target is a nameless face-down creature.
///
/// The target takes the single `Pump` (-3/-3 on a 2/2) and dies, which is also
/// the reach guard that the spell resolved at all. Before the fix the mass leg
/// matched `""` against `""` and shrank the sibling too.
#[test]
fn bile_blight_spares_a_nameless_sibling_of_a_nameless_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario.add_creature(P1, "Hidden A", 2, 2).id();
    let sibling = scenario.add_creature(P1, "Hidden B", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bile Blight", true, BILE_BLIGHT)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    make_face_down_nameless(&mut runner, target);
    make_face_down_nameless(&mut runner, sibling);
    assert_eq!(
        runner.state().objects[&target].name,
        "",
        "reach guard: the announced target is nameless"
    );
    assert_eq!(
        runner.state().objects[&sibling].name,
        "",
        "reach guard: the sibling is nameless"
    );

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    outcome.assert_zone(&[target], Zone::Graveyard);

    let state = runner.state();
    assert!(
        on_battlefield(state, sibling),
        "CR 201.2a: a nameless target names no sibling, so the mass leg spares it"
    );
    assert_eq!(
        state.objects[&sibling].toughness,
        Some(2),
        "the spared sibling keeps its uneffected 2/2 body"
    );
}

/// D2 reach guard for D1: the mass leg is still a real mass leg. Two 5/5
/// "Grizzly Bears" both shrink; a differently named creature does not.
#[test]
fn bile_blight_still_sweeps_named_same_name_siblings() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario.add_creature(P1, "Grizzly Bears", 5, 5).id();
    let twin = scenario.add_creature(P1, "Grizzly Bears", 5, 5).id();
    let other = scenario.add_creature(P1, "Runeclaw Bear", 5, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bile Blight", true, BILE_BLIGHT)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Bile Blight must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );

    let state = runner.state();
    assert_eq!(
        state.objects[&target].toughness,
        Some(2),
        "the target shrinks"
    );
    assert_eq!(state.objects[&twin].toughness, Some(2), "the twin shrinks");
    assert_eq!(
        state.objects[&other].toughness,
        Some(5),
        "a differently named creature is untouched"
    );
}

/// CR 201.2a + CR 708.2a + CR 608.2h: Maelstrom Pulse's "all other permanents
/// with the same name as that permanent" resolves the reference from the
/// parent instruction's move-time snapshot. A nameless face-down target names
/// no other permanent, so no nameless sibling is destroyed.
#[test]
fn maelstrom_pulse_spares_a_nameless_sibling_of_a_nameless_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario.add_creature(P1, "Hidden A", 2, 2).id();
    let sibling = scenario.add_creature(P1, "Hidden B", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Maelstrom Pulse", false, MAELSTROM_PULSE)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    make_face_down_nameless(&mut runner, target);
    make_face_down_nameless(&mut runner, sibling);
    assert_eq!(runner.state().objects[&target].name, "");

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    outcome.assert_zone(&[target], Zone::Graveyard);

    assert!(
        on_battlefield(runner.state(), sibling),
        "CR 201.2a: a nameless target shares its name with no other permanent"
    );
}

/// CR 201.2a + CR 708.2a + CR 610.3: Deputy of Detention's "all other nonland
/// permanents that player controls with the same name as that permanent" must
/// not exile a nameless sibling. The "until" axis also returns the exiled
/// target when Deputy leaves.
#[test]
fn deputy_of_detention_spares_a_nameless_sibling_of_a_nameless_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario.add_creature(P1, "Hidden A", 2, 2).id();
    let sibling = scenario.add_creature(P1, "Hidden B", 2, 2).id();

    let deputy = scenario
        .add_creature_to_hand_from_oracle(P0, "Deputy of Detention", 1, 3, DEPUTY_OF_DETENTION)
        .with_mana_cost(free())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    // Capture the printed name before the face-down profile blanks it.
    let original_name = runner.state().objects[&target].base_name.clone();
    assert!(
        !original_name.is_empty(),
        "reach guard: the target has a real printed name"
    );
    make_face_down_nameless(&mut runner, target);
    make_face_down_nameless(&mut runner, sibling);

    let outcome = runner.cast(deputy).target_objects(&[target]).resolve();
    outcome.assert_zone(&[target], Zone::Exile);

    assert!(
        on_battlefield(runner.state(), sibling),
        "CR 201.2a: a nameless target names no other permanent, so the sibling stays"
    );

    // CR 610.3: the "until" exile ends when Deputy leaves.
    runner.cast(removal).target_objects(&[deputy]).resolve();
    assert!(
        !on_battlefield(runner.state(), deputy),
        "Deputy must have left the battlefield"
    );
    assert_eq!(
        battlefield_count_named(runner.state(), P1, &original_name),
        1,
        "the exiled target returns under its printed name"
    );
}
