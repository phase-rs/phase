//! CR 708.2 + the Duskmourn rulings of 2024-09-20: a face-down permanent records
//! WHICH keyword action put it face down (#7532).
//!
//! > You must ensure that your face-down spells and permanents can be easily
//! > differentiated from each other. … The order in which they entered should
//! > remain clear, as well as what ability caused them to be face down. (This
//! > includes manifest, disguise, cloak, morph, and a few older effects that
//! > turn cards face down.)
//!
//! CR 708.2a gives every face-down permanent identical characteristics, so the
//! object itself cannot answer that question and the display layer had nothing
//! to show but a generic card back. No game rule reads the new field; it exists
//! so the client can show the marker token paper play uses.
//!
//! The cause rides `FaceDownProfile`, which is what survives a CR 616.1 entry
//! pause, and is stamped by the single face-down entry helper
//! (`zone_pipeline::apply_face_down_entry_profile`).

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::FaceDownCause;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Resolve `oracle` as a 0-cost sorcery with `library` cards available, then
/// report the cause recorded on the one face-down permanent it produced.
fn cause_after(
    oracle: &str,
    library: usize,
    answer_manifest_choice: bool,
) -> Option<FaceDownCause> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..library {
        scenario.add_card_to_library_top(P0, &format!("Library {i}"));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Face-Down Maker", false)
        .from_oracle_text(oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    if answer_manifest_choice {
        if let WaitingFor::ManifestDreadChoice { cards, .. } = runner.state().waiting_for.clone() {
            runner
                .act(engine::types::actions::GameAction::SelectCards {
                    cards: vec![cards[0]],
                })
                .expect("choose the card to manifest");
            runner.advance_until_stack_empty();
        }
    }

    let face_down: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.face_down)
        .collect();
    assert_eq!(
        face_down.len(),
        1,
        "reach guard: exactly one face-down permanent, got {face_down:?}"
    );
    face_down[0].face_down_cause
}

/// CR 701.62a: manifest dread records the manifest cause.
#[test]
fn manifest_dread_records_the_manifest_cause() {
    assert_eq!(
        cause_after("Manifest dread.", 2, true),
        Some(FaceDownCause::Manifest)
    );
}

/// CR 701.40a: plain manifest records the same cause — same keyword action,
/// different card-selection step, one marker token in paper.
#[test]
fn plain_manifest_records_the_manifest_cause() {
    assert_eq!(
        cause_after("Manifest the top card of your library.", 1, false),
        Some(FaceDownCause::Manifest)
    );
}

/// CR 701.58a: cloak is its own keyword action and gets its own marker, even
/// though its characteristics are manifest's plus ward {2}. Keying the display
/// on the ward instead would be classifying by shape rather than asking the
/// rules.
#[test]
fn cloak_records_the_cloak_cause() {
    assert_eq!(
        cause_after("Cloak the top card of your library.", 1, false),
        Some(FaceDownCause::Cloak)
    );
}

/// The counter-direction that keeps the field honest: a face-UP permanent
/// carries no cause, so a display layer that forgets to gate on `face_down`
/// still has nothing to show.
#[test]
fn a_face_up_permanent_records_no_cause() {
    let mut scenario = GameScenario::new();
    let creature = scenario.add_creature(P0, "Plain Creature", 2, 2).id();
    let runner = scenario.build();
    assert!(!runner.state().objects[&creature].face_down);
    assert_eq!(runner.state().objects[&creature].face_down_cause, None);
}

/// CR 702.37a: a face-down CAST is morph, not manifest, even though it reuses
/// the same vanilla 2/2 characteristics. Keying the marker on those
/// characteristics would show the wrong token.
#[test]
fn a_morph_cast_records_the_morph_cause() {
    assert_eq!(
        cause_after_face_down_cast(engine::types::keywords::Keyword::Morph(
            engine::types::mana::ManaCost::generic(5)
        )),
        Some(FaceDownCause::Morph)
    );
}

/// CR 702.168a: disguise reuses cloak's warded characteristics and is still its
/// own action — the ward is shared, the marker source is not.
#[test]
fn a_disguise_cast_records_the_disguise_cause() {
    assert_eq!(
        cause_after_face_down_cast(engine::types::keywords::Keyword::Disguise(
            engine::types::keywords::DisguiseCost::Mana(engine::types::mana::ManaCost::generic(5))
        )),
        Some(FaceDownCause::Disguise)
    );
}

/// Cast a creature carrying `keyword` face down for its fixed {3} and report the
/// cause recorded on the resulting permanent.
fn cause_after_face_down_cast(keyword: engine::types::keywords::Keyword) -> Option<FaceDownCause> {
    use engine::types::mana::{ManaType, ManaUnit};
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_creature_to_hand(P0, "Face-Down Cast Probe", 4, 4)
        .with_mana_cost(ManaCost::NoCost)
        .with_keyword(keyword)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, card, false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(card).commit().resolve();
    runner.advance_until_stack_empty();

    let object = &runner.state().objects[&card];
    assert!(
        object.face_down && object.zone == Zone::Battlefield,
        "reach guard: the card must have entered the battlefield face down, got {object:?}"
    );
    object.face_down_cause
}

/// CR 708.2: an effect that turns a permanent already on the battlefield face
/// down is no keyword action at all — Wizards prints no marker for it, so the
/// cause must say so instead of inheriting the manifest default the vanilla
/// profile carries.
#[test]
fn a_generic_turn_face_down_records_its_own_cause() {
    assert_eq!(
        cause_after_turn_face_down("Turn target creature face down."),
        Some(FaceDownCause::TurnedFaceDown)
    );
}

/// The PROFILED variant (Cyber Conversion) takes the same path with an authored
/// body, and must not pick up a different cause because its profile is authored
/// rather than defaulted.
#[test]
fn a_profiled_turn_face_down_records_its_own_cause() {
    assert_eq!(
        cause_after_turn_face_down(
            "Turn target creature face down. It's a 2/2 Cyberman artifact creature."
        ),
        Some(FaceDownCause::TurnedFaceDown)
    );
}

fn cause_after_turn_face_down(oracle: &str) -> Option<FaceDownCause> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P0, "Victim", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Turn-Down Probe", false)
        .from_oracle_text(oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();

    runner.cast(spell).target_object(victim).resolve();
    runner.advance_until_stack_empty();

    let object = &runner.state().objects[&victim];
    assert!(
        object.face_down,
        "reach guard: the creature must have been turned face down, got {object:?}"
    );
    object.face_down_cause
}
