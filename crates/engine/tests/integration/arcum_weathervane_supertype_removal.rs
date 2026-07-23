//! Surface parser near-miss: a one-shot supertype REMOVAL on a targeted
//! permanent ("target <filter> is no longer / isn't <supertype>") was not
//! recognized.
//!
//! The subject-predicate dispatch (`oracle_effect/subject.rs`) already lowered
//! the ADDITION direction — "target <filter> becomes <supertype>" — to a
//! targeted `Effect::GenericEffect` continuous grant emitting
//! `ContinuousModification::AddSupertype`, but had no arm for the
//! copula-negation REMOVAL direction. So the removal clause fell through to the
//! imperative fallback and lowered to `Effect::Unimplemented`: at resolution
//! nothing happened and the permanent kept its supertype.
//!
//! Two real cards each pair a supported "becomes <supertype>" ability with the
//! unsupported removal on the SAME card:
//!   * Arcum's Weathervane — "{2}, {T}: Target snow land is no longer snow." /
//!     "{2}, {T}: Target nonsnow basic land becomes snow." (the latter already
//!     supported).
//!   * Thermal Flux — modal "Target snow permanent isn't snow until end of turn."
//!     / "Target nonsnow permanent becomes snow until end of turn."
//!
//! The one-arm fix routes the removal clause through the SAME targeted
//! `Effect::GenericEffect` runtime as its AddSupertype sibling, emitting
//! `ContinuousModification::RemoveSupertype` instead of `AddSupertype`;
//! `game/layers.rs` already applies both directions. No new effect variant and
//! no resolver path.
//!
//! Each test drives the REAL cast -> target -> resolve pipeline against a snow
//! land and FAILS on `main`, where the clause lowers to `Unimplemented` and the
//! land keeps its Snow supertype. The clause parse is context-independent, so a
//! standalone one-shot spell exercises exactly the same subject-predicate arm
//! that Arcum's activated ability and Thermal Flux's modal chapter reach.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

// Arcum's Weathervane's removal clause, as a standalone permanent-duration
// one-shot (CR 611.2a: a "becomes"/type-change with no explicit duration is
// permanent — the same default the AddSupertype sibling receives).
const IS_NO_LONGER_SNOW: &str = "Target snow land is no longer snow.";
// Thermal Flux's removal mode, as a standalone until-end-of-turn one-shot.
const ISNT_SNOW_EOT: &str = "Target snow permanent isn't snow until end of turn.";
// The expanded copula spelling is the third parser axis, distinct from Thermal
// Flux's contraction while sharing its until-end-of-turn runtime behavior.
const IS_NOT_SNOW_EOT: &str = "Target snow permanent is not snow until end of turn.";

/// Put a snow land (Land + Snow supertype + Island subtype) onto `player`'s
/// battlefield and return its id. Mirrors the direct-`state_mut` typed object
/// construction the integration suite uses for battlefield fixtures
/// (e.g. `claim_jumper_repeat`'s basic-land helper).
fn add_snow_land(runner: &mut GameRunner, player: PlayerId) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        player,
        "Snow-Covered Island".to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.supertypes.push(Supertype::Snow);
    obj.card_types.subtypes.push("Island".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Cast a one-shot supertype-removal spell at an opponent's snow land and assert
/// the Snow supertype is stripped end-to-end. On `main` the clause lowers to
/// `Effect::Unimplemented`, so the land is unchanged and this fails.
fn snow_removal_case(oracle: &str, persists_until_next_turn: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Snow Thaw Test", true, oracle)
        .id();
    let mut runner = scenario.build();

    let land = add_snow_land(&mut runner, P1);

    assert!(
        runner.state().objects[&land]
            .card_types
            .supertypes
            .contains(&Supertype::Snow),
        "precondition: the targeted land starts with the Snow supertype"
    );

    let outcome = runner.cast(spell).target_objects(&[land]).resolve();

    // End-to-end runtime delta: the layered characteristics of the targeted land
    // no longer carry the Snow supertype (CR 205.4b layer-4 removal, applied by
    // `game/layers.rs`). On `main` the clause never parsed to a RemoveSupertype
    // grant, so the land is still snow.
    assert!(
        !outcome.state().objects[&land]
            .card_types
            .supertypes
            .contains(&Supertype::Snow),
        "the \"{oracle}\" clause must strip the Snow supertype from the targeted \
         land; still snow means the clause lowered to Unimplemented (main). \
         supertypes={:?} waiting_for={:?}",
        outcome.state().objects[&land].card_types.supertypes,
        outcome.state().waiting_for,
    );

    drop(outcome);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().objects[&land]
            .card_types
            .supertypes
            .contains(&Supertype::Snow),
        !persists_until_next_turn,
        "the \"{oracle}\" duration must determine whether Snow returns after \
         end-of-turn cleanup; supertypes={:?}",
        runner.state().objects[&land].card_types.supertypes,
    );
}

/// Arcum's Weathervane: "Target snow land is no longer snow." (permanent).
#[test]
fn arcum_weathervane_is_no_longer_snow_strips_snow_supertype() {
    snow_removal_case(IS_NO_LONGER_SNOW, true);
}

/// Thermal Flux: "Target snow permanent isn't snow until end of turn."
#[test]
fn thermal_flux_isnt_snow_strips_snow_supertype_until_end_of_turn() {
    snow_removal_case(ISNT_SNOW_EOT, false);
}

#[test]
fn is_not_snow_strips_snow_supertype_until_end_of_turn() {
    snow_removal_case(IS_NOT_SNOW_EOT, false);
}
