//! CR 701.60c: the suspected designation grants menace + "This creature can't
//! block" *for as long as it's suspected* — a continuous effect derived from the
//! `is_suspected` designation, NOT a mutation of the permanent's printed
//! (copiable) abilities (CR 701.60b). Verified end-to-end through the real
//! Suspect / Unsuspect effects + the layer system.
//!
//! Primary discriminator (the maintainer-requested [MED]): a creature with
//! PRINTED menace that becomes suspected and is then made no-longer-suspected
//! must STILL have its printed menace and be able to block again. The prior
//! base-mutating architecture wrote menace into `base_keywords` on suspect and
//! retained OUT every `Keyword::Menace` on unsuspect — which deleted the printed
//! menace permanently. Deriving the grant from `is_suspected` during layer
//! evaluation fixes this: unsuspect only clears the flag and the derived grant
//! lapses, leaving the printed menace untouched.
//!
//! Reverting `layers::derive_suspected_abilities` (or reverting the resolvers to
//! mutate `base_*`) flips the "printed menace survives the round-trip" assertion.

use engine::game::combat::can_block_pair;
use engine::game::effects::resolve_effect;
use engine::game::scenario::GameScenario;
use engine::types::ability::{Effect, EffectScope, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);

type Runner = engine::game::scenario::GameRunner;

fn has_menace(runner: &Runner, id: ObjectId) -> bool {
    engine::game::keywords::has_keyword(&runner.state().objects[&id], &Keyword::Menace)
}

fn has_cant_block(runner: &Runner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .static_definitions
        .iter_unchecked()
        .any(|s| s.mode == StaticMode::CantBlock)
}

fn is_suspected(runner: &Runner, id: ObjectId) -> bool {
    runner.state().objects[&id].is_suspected
}

/// Suspect a battlefield creature through the real `Effect::Suspect`, then run a
/// full layer pass so the CR 701.60c derivation is applied exactly as production.
fn suspect(runner: &mut Runner, id: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::Suspect {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
        },
        vec![TargetRef::Object(id)],
        id,
        P0,
    );
    let mut events = Vec::new();
    resolve_effect(runner.state_mut(), &ability, &mut events).expect("suspect resolves");
    let state = runner.state_mut();
    state.layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(state);
}

/// Un-suspect a battlefield creature through the real `Effect::Unsuspect`, then
/// run a full layer pass so the derived grant lapses.
fn unsuspect(runner: &mut Runner, id: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::Unsuspect {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
        },
        vec![TargetRef::Object(id)],
        id,
        P0,
    );
    let mut events = Vec::new();
    resolve_effect(runner.state_mut(), &ability, &mut events).expect("unsuspect resolves");
    let state = runner.state_mut();
    state.layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(state);
}

/// CR 701.60b + CR 701.60c: a creature with PRINTED menace keeps it across the
/// full suspect → no-longer-suspected round-trip. This is the maintainer's [MED]
/// regression: reverting the layer-derived grant (so suspect mutates base and
/// unsuspect retains-out every menace) flips the final `has_menace` assertion to
/// false because the printed menace was deleted.
#[test]
fn printed_menace_survives_suspect_round_trip() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    // Printed menace via parsed Oracle text — this lands in base_keywords.
    let bear = scenario
        .add_creature_from_oracle(P0, "Menace Bear", 2, 2, "Menace")
        .id();
    let mut runner = scenario.build();

    // Baseline: printed menace present, not suspected, can block (no CantBlock).
    assert!(has_menace(&runner, bear), "printed menace at baseline");
    assert!(!is_suspected(&runner, bear));
    assert!(!has_cant_block(&runner, bear), "no can't-block at baseline");

    // Suspect it. It already has menace (printed); while suspected it also gets
    // the derived "can't block."
    suspect(&mut runner, bear);
    assert!(is_suspected(&runner, bear), "now suspected");
    assert!(
        has_menace(&runner, bear),
        "still has menace while suspected"
    );
    assert!(
        has_cant_block(&runner, bear),
        "suspected creature can't block (CR 701.60c)"
    );

    // No longer suspected. The derived can't-block lapses, but the PRINTED
    // menace must survive — this is the bug the [MED] reported.
    unsuspect(&mut runner, bear);
    assert!(!is_suspected(&runner, bear), "designation cleared");
    assert!(
        has_menace(&runner, bear),
        "PRINTED menace must survive the suspect round-trip (CR 701.60b: the \
         designation is not part of copiable values, so clearing it must not \
         delete the printed menace)"
    );
    assert!(
        !has_cant_block(&runner, bear),
        "derived can't-block lapses when no longer suspected"
    );
}

/// CR 701.60c: a VANILLA creature (no printed menace) gains menace + can't-block
/// purely from the designation while suspected, and loses BOTH when no longer
/// suspected — proving the grant is derived and present, not a no-op. This is the
/// negative-controlcase paired with the printed-menace test: it would still pass
/// under the old architecture, so it is the present-while-suspected proof.
#[test]
fn vanilla_creature_gains_then_loses_derived_abilities() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    let bear = scenario.add_creature(P0, "Vanilla Bear", 2, 2).id();
    let mut runner = scenario.build();

    assert!(!has_menace(&runner, bear), "vanilla: no menace at baseline");
    assert!(!has_cant_block(&runner, bear));

    suspect(&mut runner, bear);
    assert!(
        has_menace(&runner, bear),
        "derived menace present while suspected (CR 701.60c)"
    );
    assert!(
        has_cant_block(&runner, bear),
        "derived can't-block present while suspected (CR 701.60c)"
    );

    unsuspect(&mut runner, bear);
    assert!(
        !has_menace(&runner, bear),
        "derived menace gone when no longer suspected"
    );
    assert!(
        !has_cant_block(&runner, bear),
        "derived can't-block gone when no longer suspected"
    );
}

/// CR 701.60c: while suspected, a creature with printed menace also can't block —
/// asserted through the real `can_block_pair` legality predicate (production's
/// block-declaration gate), and the legality returns to blockable after the
/// designation is cleared. Drives the actual combat legality seam, not just the
/// static-definition list.
#[test]
fn suspected_printed_menace_block_legality_round_trip() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    let blocker = scenario
        .add_creature_from_oracle(P0, "Menace Blocker", 2, 2, "Menace")
        .id();
    // Attacker controlled by the other player so the blocker could legally block
    // it absent the can't-block restriction.
    let attacker = scenario.add_creature(PlayerId(1), "Attacker", 3, 3).id();
    let mut runner = scenario.build();

    // Baseline: the printed-menace blocker can legally block the attacker.
    assert!(
        can_block_pair(runner.state(), blocker, attacker),
        "printed-menace creature can block at baseline"
    );

    suspect(&mut runner, blocker);
    assert!(
        !can_block_pair(runner.state(), blocker, attacker),
        "suspected creature can't block (CR 701.60c) — via real legality predicate"
    );

    unsuspect(&mut runner, blocker);
    assert!(
        can_block_pair(runner.state(), blocker, attacker),
        "block legality restored after no-longer-suspected; printed menace intact"
    );
    assert!(
        has_menace(&runner, blocker),
        "printed menace survives the round-trip"
    );
}
