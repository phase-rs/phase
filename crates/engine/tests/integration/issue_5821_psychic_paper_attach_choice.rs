//! Issue #5821 — Psychic Paper: "As this Equipment becomes attached to a
//! creature, choose a creature card name and a creature type." must actually
//! prompt the two choices when Equip resolves, not just parse the downstream
//! name/type-setting static (that half was already covered).
//!
//! https://github.com/phase-rs/phase/issues/5821

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const PSYCHIC_PAPER_ORACLE: &str = "As this Equipment becomes attached to a creature, choose a creature card name and a creature type.\nEquipped creature has ward {1}, it can't be blocked, and its name and creature type are the last chosen name and creature type.\nEquip {2}";

#[test]
fn psychic_paper_equip_prompts_name_then_type_and_binds_both_on_attach() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let paper = scenario
        .add_creature(P0, "Psychic Paper", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(PSYCHIC_PAPER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().all_card_names = vec!["Llanowar Elves".to_string()].into();

    let equip_idx = runner.state().objects[&paper]
        .abilities
        .iter()
        .position(|a| {
            a.description
                .as_deref()
                .is_some_and(|d| d.contains("Equip"))
        })
        .expect("Psychic Paper must carry an Equip activated ability");

    // Drive the real activation → targeting → mana payment → stack resolution
    // path. The driver stops at the first `NamedChoice` it doesn't have a
    // declared policy for — which must be the CardName choice fired by the
    // new attach-time replacement (revert the parser gate or the
    // `Effect::Attach` replacement hook and this never fires; the equip just
    // attaches silently and `waiting_for` goes back to `Priority`).
    runner
        .activate(paper, equip_idx)
        .target_object(bear)
        .resolve();

    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            choice_type: ChoiceType::CardName,
            source: Some(source),
            ..
        } => assert_eq!(
            source.prompt.identity.reference.object_id, paper,
            "the card-name choice must bind to Psychic Paper"
        ),
        other => panic!("expected NamedChoice(CardName) right after attaching, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseOption {
            choice: "Llanowar Elves".to_string(),
        })
        .expect("card name choice must be accepted");

    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            choice_type: ChoiceType::CreatureType { .. },
            source: Some(source),
            ..
        } => assert_eq!(
            source.prompt.identity.reference.object_id, paper,
            "the creature-type choice must bind to Psychic Paper"
        ),
        other => panic!("expected NamedChoice(CreatureType) after the card name, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseOption {
            choice: "Zombie".to_string(),
        })
        .expect("creature type choice must be accepted");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().objects[&paper].attached_to,
            Some(AttachTarget::Object(id)) if id == bear
        ),
        "Psychic Paper must attach to Grizzly Bears once both choices are made"
    );

    let equipped = &runner.state().objects[&bear];
    assert_eq!(
        equipped.name, "Llanowar Elves",
        "equipped creature's name must become the chosen card name (CR 612.8)"
    );
    assert_eq!(
        equipped.card_types.subtypes,
        vec!["Zombie".to_string()],
        "equipped creature's subtypes must become exactly the chosen creature type (CR 205.1a)"
    );
    assert!(
        equipped
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Ward(_))),
        "equipped creature must gain ward {{1}} (CR 702.21)"
    );
    assert!(
        equipped
            .static_definitions
            .as_slice()
            .iter()
            .any(|def| matches!(def.mode, engine::types::statics::StaticMode::CantBeBlocked)),
        "equipped creature must gain \"can't be blocked\""
    );
}

// CR 301.5b: Equipment enters the battlefield like other artifacts — it does
// NOT enter attached to a creature. Casting bare Psychic Paper must NOT
// prompt the "as it becomes attached, choose …" replacement, since no
// attachment has occurred yet. The driver silently halts (does not panic) at
// any `NamedChoice` it has no declared policy for, so the regression check
// is on `final_waiting_for()` settling back at `Priority` rather than
// stalling on a stray name/type prompt.
#[test]
fn psychic_paper_casting_unattached_does_not_prompt_attach_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let paper = scenario
        .add_creature_to_hand(P0, "Psychic Paper", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(PSYCHIC_PAPER_ORACLE)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(paper).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "casting unattached Psychic Paper must settle back at Priority, not stall on \
         an errant attach-time choice prompt: {:?}",
        outcome.final_waiting_for()
    );

    let state = outcome.state();
    assert_eq!(
        state.objects[&paper].zone,
        engine::types::zones::Zone::Battlefield,
        "Psychic Paper must resolve onto the battlefield"
    );
    assert_eq!(
        state.objects[&paper].attached_to, None,
        "Psychic Paper must enter unattached — Equipment doesn't enter attached (CR 301.5b)"
    );
}
