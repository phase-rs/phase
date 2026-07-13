//! Cluster-113 — The Eighth Doctor: play/cast-from-graveyard permission that
//! GRANTS a per-object leave-the-battlefield-exile replacement to the permanent
//! played this way.
//!
//! Oracle text (verbatim, Scryfall):
//!   "When The Eighth Doctor enters, mill three cards.
//!    Once during each of your turns, you may play a historic land or cast a
//!    historic permanent spell from your graveyard. If you do, it gains \"If this
//!    permanent would leave the battlefield, exile it instead of putting it
//!    anywhere else.\""
//!
//! Runtime proofs (the parse-level assertions live in
//! `parser/oracle_static/tests.rs`):
//!   T1 — cast a historic permanent from the graveyard via the permission: it
//!        enters the battlefield AND the granted `Moved`/`SelfRef` → Exile redirect
//!        is installed on its `base_replacement_definitions`; a subsequent
//!        battlefield exit (Destroy) lands it in EXILE, not the graveyard. This
//!        proves (i) the `SelfRef Moved` install machinery fires for a def installed
//!        directly onto a battlefield object, and (ii) `SelfRef` binds to the
//!        ENTRANT, not the granting Doctor.
//!   T2 — CR 611.2c persistence: after the grant, the Doctor leaves the
//!        battlefield; the granted permanent's exit STILL redirects to exile (the
//!        rider is independent of its source).
//!   T3 — net-new land path: a historic land played from the graveyard via the
//!        permission carries the rider on its `base_replacement_definitions`, and
//!        its exit redirects to exile.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const EIGHTH_DOCTOR_PERMISSION: &str = "Once during each of your turns, you may play a historic land or cast a historic permanent spell from your graveyard. If you do, it gains \"If this permanent would leave the battlefield, exile it instead of putting it anywhere else.\"";
const DESTROY_CREATURE: &str = "Destroy target creature.";
const DESTROY_LAND: &str = "Destroy target land.";

/// True when `obj`'s `base_replacement_definitions` carries the granted
/// leave-the-battlefield → exile redirect (`Moved` / `SelfRef` / `ChangeZone`
/// Battlefield→Exile). The rider MUST live on base so it survives every layer
/// reset (the runtime redirect is rebuilt from base each pass).
fn has_granted_leave_battlefield_exile(runner: &GameRunner, obj: ObjectId) -> bool {
    runner.state().objects[&obj]
        .base_replacement_definitions
        .iter()
        .any(|def| {
            def.event == ReplacementEvent::Moved
                && def.valid_card == Some(TargetFilter::SelfRef)
                && def.execute.as_ref().is_some_and(|ability| {
                    matches!(
                        ability.effect.as_ref(),
                        Effect::ChangeZone {
                            origin: Some(Zone::Battlefield),
                            destination: Zone::Exile,
                            target: TargetFilter::SelfRef,
                            ..
                        }
                    )
                })
        })
}

/// Build a scenario with The Eighth Doctor's permission static on the battlefield
/// (P0), a zero-cost historic (legendary) permanent spell in P0's graveyard, and
/// a "Destroy target creature." spell in hand. Returns the runner plus the Doctor
/// and graveyard-creature ids.
fn doctor_with_graveyard_historic() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let doctor = scenario
        .add_creature(P0, "The Eighth Doctor", 4, 5)
        .from_oracle_text(EIGHTH_DOCTOR_PERMISSION)
        .id();
    let historic = scenario
        .add_creature_to_graveyard(P0, "Legendary Reveler", 2, 2)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let runner = scenario.build();
    (runner, doctor, historic, murder)
}

/// T1 — the graveyard-cast permanent enters carrying the granted redirect on its
/// base store, and a battlefield exit redirects to EXILE (bound to the entrant,
/// not the Doctor).
#[test]
fn granted_rider_installs_and_redirects_battlefield_exit_to_exile() {
    let (mut runner, _doctor, historic, murder) = doctor_with_graveyard_historic();

    // reach-guard: the historic permanent is castable from the graveyard.
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&historic),
        "The Eighth Doctor must surface the historic graveyard permanent as castable"
    );

    // Cast it via the permission; it resolves onto the battlefield.
    let outcome = runner.cast(historic).resolve();
    assert_eq!(
        outcome.zone_of(historic),
        Zone::Battlefield,
        "the historic permanent cast via the permission must resolve onto the battlefield"
    );

    // DISCRIMINATING: the granted redirect is installed on the entrant's base.
    assert!(
        has_granted_leave_battlefield_exile(&runner, historic),
        "the permanent played via the permission must carry the granted leave-battlefield→exile redirect on its base store"
    );

    // Drive a battlefield exit; the redirect sends it to EXILE, not the graveyard.
    runner.cast(murder).target_object(historic).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&historic].zone,
        Zone::Exile,
        "the granted rider must redirect the destroyed permanent to exile (SelfRef binds to the entrant)"
    );
}

/// T2 — CR 611.2c: the granted rider persists after the granting Doctor leaves.
#[test]
fn granted_rider_persists_after_doctor_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let doctor = scenario
        .add_creature(P0, "The Eighth Doctor", 4, 5)
        .from_oracle_text(EIGHTH_DOCTOR_PERMISSION)
        .id();
    let historic = scenario
        .add_creature_to_graveyard(P0, "Legendary Reveler", 2, 2)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();
    // Two destroy spells: one for the Doctor, one for the granted permanent.
    let kill_doctor = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, DESTROY_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let kill_grantee = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    // Cast the historic permanent, then remove the Doctor from the battlefield.
    runner.cast(historic).resolve();
    assert!(
        has_granted_leave_battlefield_exile(&runner, historic),
        "reach-guard: the rider must be installed before the Doctor leaves"
    );
    runner.cast(kill_doctor).target_object(doctor).resolve();
    runner.advance_until_stack_empty();
    assert_ne!(
        runner.state().objects[&doctor].zone,
        Zone::Battlefield,
        "reach-guard: the Doctor must have left the battlefield"
    );

    // The granted rider still redirects the permanent's exit to EXILE.
    runner.cast(kill_grantee).target_object(historic).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&historic].zone,
        Zone::Exile,
        "the granted rider persists independent of its source (CR 611.2c): the exit still redirects to exile"
    );
}

/// T3 — net-new land path: a historic land played from the graveyard via the
/// permission carries the rider on its base store; its exit redirects to exile.
#[test]
fn granted_rider_installs_on_land_played_from_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "The Eighth Doctor", 4, 5)
        .from_oracle_text(EIGHTH_DOCTOR_PERMISSION);
    // A historic (legendary) land in the graveyard.
    let land = scenario
        .add_creature_to_graveyard(P0, "Legendary Wastes", 0, 0)
        .as_land()
        .as_legendary()
        .id();
    let raze = scenario
        .add_spell_to_hand_from_oracle(P0, "Stone Rain", true, DESTROY_LAND)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    // Play the land from the graveyard via the permission (land special action).
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("historic land must be playable from the graveyard via the permission");
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "playing the historic land must move it to the battlefield"
    );

    // DISCRIMINATING (net-new land branch): the rider is installed on the land's
    // base store — no counter rider populates this queue for a land today.
    assert!(
        has_granted_leave_battlefield_exile(&runner, land),
        "the land played via the permission must carry the granted leave-battlefield→exile redirect on its base store"
    );

    // Its exit redirects to EXILE.
    runner.cast(raze).target_object(land).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Exile,
        "the granted rider must redirect the destroyed land to exile"
    );
}
