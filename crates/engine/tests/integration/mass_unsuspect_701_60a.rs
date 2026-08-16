//! CR 701.60a: mass "all suspected creatures are no longer suspected" resolves
//! over the whole matched set — end-to-end through the real cast/ETB pipeline.
//!
//! Absolving Lammasu's enters trigger reads "all suspected creatures are no
//! longer suspected." That is a non-targeting *population* effect (no announced
//! target), so the resolver must enumerate every battlefield permanent matching
//! the parsed `TargetFilter` and remove the suspected designation from each
//! (CR 701.60a), rather than reading `ability.targets` (which is empty for a
//! mass clause).
//!
//! Discriminator: the prior resolver resolved every Unsuspect filter through
//! `ability.targets`. A mass clause announces no target, so it was a no-op —
//! both suspected creatures would stay suspected. With mass scope enumerated
//! over the battlefield, both flip to un-suspected while a never-suspected
//! creature is left untouched. Reverting the `EffectScope::All` enumeration in
//! `game/effects/suspect.rs::resolve_object_targets` flips the two `assert!`s
//! that the suspected creatures are cleared back to `true`.

use engine::game::effects::resolve_effect;
use engine::game::scenario::GameScenario;
use engine::types::ability::{Effect, EffectScope, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

// Absolving Lammasu's real Oracle text (the enters trigger is the clause under
// test; the dies trigger is included so the parse matches the printed card).
const ABSOLVING_LAMMASU: &str = "Flying\n\
When this creature enters, all suspected creatures are no longer suspected.\n\
When this creature dies, you gain 3 life and suspect up to one target creature an opponent controls.";

fn is_suspected(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.is_suspected)
        .unwrap_or(false)
}

fn has_menace(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| engine::game::keywords::has_keyword(o, &Keyword::Menace))
        .unwrap_or(false)
}

fn has_cant_block(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| {
            o.static_definitions
                .iter_unchecked()
                .any(|s| s.mode == StaticMode::CantBlock)
        })
        .unwrap_or(false)
}

/// Suspect a creature already on the battlefield via the real Suspect effect so
/// the designation + CR 701.60c menace / "can't block" abilities are present
/// exactly as production would set them.
fn suspect(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::Suspect {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
        },
        vec![TargetRef::Object(id)],
        ObjectId(9_001),
        P0,
    );
    let mut events = Vec::new();
    resolve_effect(runner.state_mut(), &ability, &mut events).expect("suspect resolves");
    let state = runner.state_mut();
    state.layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(state);
    assert!(
        state.objects[&id].is_suspected,
        "fixture creature must be suspected before the mass clear"
    );
}

/// CR 701.60a: Absolving Lammasu's enters trigger clears the suspected
/// designation from EVERY suspected creature on the battlefield (mass scope),
/// while leaving a never-suspected creature untouched.
#[test]
fn mass_no_longer_suspected_clears_all_matching_creatures() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Two suspected creatures (different controllers — the filter is "all
    // suspected creatures", uncontrolled) and one never-suspected control.
    let suspected_a = scenario.add_creature(P1, "Suspect Bear A", 2, 2).id();
    let suspected_b = scenario.add_creature(P0, "Suspect Bear B", 3, 3).id();
    let clean = scenario.add_creature(P1, "Honest Bear", 1, 1).id();

    // Absolving Lammasu in P0's hand, parsed from its real Oracle text.
    let lammasu = scenario
        .add_creature_to_hand_from_oracle(P0, "Absolving Lammasu", 3, 5, ABSOLVING_LAMMASU)
        .id();

    let mut runner = scenario.build();

    // Designate the two fixtures as suspected through the real effect.
    suspect(&mut runner, suspected_a);
    suspect(&mut runner, suspected_b);
    assert!(is_suspected(&runner, suspected_a));
    assert!(is_suspected(&runner, suspected_b));
    assert!(has_menace(&runner, suspected_a));
    assert!(has_cant_block(&runner, suspected_b));
    assert!(!is_suspected(&runner, clean));

    // Cast Absolving Lammasu (free cost); its no-target enters trigger resolves
    // the mass "all suspected creatures are no longer suspected" clause.
    let outcome = runner.cast(lammasu).resolve();
    let state = outcome.state();

    // Both suspected creatures are now un-suspected (mass enumeration).
    assert!(
        !state.objects[&suspected_a].is_suspected,
        "mass clause must clear suspected_a (reverting the All-scope enumeration \
         leaves it suspected because the trigger announces no target)"
    );
    assert!(
        !state.objects[&suspected_b].is_suspected,
        "mass clause must clear suspected_b too — the whole matched set, not just \
         one announced target"
    );

    // The CR 701.60c menace / "can't block" designation abilities are gone for
    // the cleared creatures (after the resolver's layer recalc).
    assert!(
        !engine::game::keywords::has_keyword(&state.objects[&suspected_a], &Keyword::Menace),
        "menace must be removed when no longer suspected"
    );
    assert!(
        !state.objects[&suspected_b]
            .static_definitions
            .iter_unchecked()
            .any(|s| s.mode == StaticMode::CantBlock),
        "can't-block must be removed when no longer suspected"
    );

    // The never-suspected creature is untouched.
    assert!(
        !state.objects[&clean].is_suspected,
        "a never-suspected creature stays un-suspected and is otherwise unaffected"
    );
}
