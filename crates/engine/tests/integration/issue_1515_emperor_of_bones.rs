//! Issue #1515 — Emperor of Bones must grant haste to, and later sacrifice,
//! the creature returned from its linked exile set.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ChoiceType, ChosenAttribute, ContinuousModification, DelayedTriggerCondition,
    Effect, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const EMPEROR_COUNTER_TRIGGER_EFFECT: &str = "put a creature card exiled with this creature onto \
the battlefield under your control with a finality counter on it. it gains haste. sacrifice it at \
the beginning of the next end step.";

const ANOINTED_PEACEKEEPER: &str = "Vigilance\n\
As this creature enters, look at an opponent's hand, then choose any card name.\n\
Spells your opponents cast with the chosen name cost {2} more to cast.\n\
Activated abilities of sources with the chosen name cost {2} more to activate unless they're mana abilities.";

const P1: PlayerId = PlayerId(1);
const NAMED_CARD: &str = "Llanowar Elves";

fn creature_has_haste_from_transient_effects(
    state: &engine::types::game_state::GameState,
    creature: ObjectId,
) -> bool {
    state.transient_continuous_effects.iter().any(|effect| {
        effect.affected == TargetFilter::SpecificObject { id: creature }
            && effect.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste
                    }
                )
            })
    })
}

#[test]
fn issue_1515_emperor_of_bones_binds_haste_and_delayed_sacrifice_to_returned_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let returned = scenario
        .add_creature_to_exile(P0, "Linked Gravebeast", 3, 3)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: returned,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    let def = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones counter-trigger effect must resolve");

    let state = runner.state();
    assert_eq!(
        state.objects[&returned].zone,
        Zone::Battlefield,
        "linked creature card must be returned to the battlefield"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "Emperor must remain on the battlefield after returning the linked creature"
    );
    assert_eq!(
        state.objects[&returned]
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "returned creature must enter with a finality counter"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, returned),
        "haste grant must bind to the returned creature, not Emperor"
    );
    assert!(
        !creature_has_haste_from_transient_effects(state, emperor),
        "Emperor itself must not receive the returned creature's haste grant"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "resolution must install exactly one delayed sacrifice trigger"
    );
    assert!(matches!(
        state.delayed_triggers[0].condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    ));
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(returned)],
        "delayed sacrifice trigger must snapshot the returned creature"
    );
    assert!(
        matches!(
            &state.delayed_triggers[0].ability.effect,
            Effect::Sacrifice {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "delayed trigger effect must sacrifice the snapshotted returned creature"
    );

    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "delayed sacrifice trigger never fired; phase = {:?}, waiting_for = {:?}, \
             delayed_triggers = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => runner
                .act(engine::types::actions::GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers while advancing to end step"),
            WaitingFor::DeclareBlockers { .. } => runner
                .act(engine::types::actions::GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declare no blockers while advancing to end step"),
            _ => runner
                .act(engine::types::actions::GameAction::PassPriority)
                .expect("priority pass while waiting for delayed sacrifice"),
        };
    }

    assert_eq!(
        runner.state().objects[&returned].zone,
        Zone::Exile,
        "returned creature must be sacrificed at the beginning of the next end step; \
         its finality counter sends it to exile"
    );
    assert_eq!(
        runner.state().objects[&emperor].zone,
        Zone::Battlefield,
        "the delayed sacrifice must not sacrifice Emperor"
    );
}

/// CR 614.12a + CR 400.7j: An as-enters choice on the returned permanent must
/// complete without losing later instructions that refer to that permanent.
#[test]
fn emperor_of_bones_resumes_riders_after_anointed_peacekeepers_as_enters_choices() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let _opponent_card = scenario.add_card_to_hand(P1, "Opponent Secret");
    let peacekeeper = {
        let mut builder = scenario.add_creature_to_exile(P0, "Anointed Peacekeeper", 3, 3);
        builder.from_oracle_text(ANOINTED_PEACEKEEPER);
        builder.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().all_card_names = std::sync::Arc::from([NAMED_CARD.to_string()]);
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: peacekeeper,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    let definition = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones return must reach Peacekeeper's as-enters choice");

    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Peacekeeper must ask which opponent to look at, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(choice_type, ChoiceType::Opponent { .. }));
    assert_eq!(options, vec![P1.0.to_string()]);
    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose the opponent whose hand Peacekeeper looks at");

    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Peacekeeper must ask for a card name after looking, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(choice_type, ChoiceType::CardName));
    runner
        .act(GameAction::ChooseOption {
            choice: NAMED_CARD.to_string(),
        })
        .expect("choose the card name for Peacekeeper");

    let state = runner.state();
    let returned = &state.objects[&peacekeeper];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(returned.chosen_attributes.iter().any(
        |attribute| matches!(attribute, ChosenAttribute::CardName(name) if name == NAMED_CARD)
    ));
    assert_eq!(
        returned
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "Peacekeeper must retain Emperor's finality entry modifier"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, peacekeeper),
        "Emperor's forwarded haste rider must resume after both as-enters choices"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "Emperor's delayed sacrifice rider must resume after both as-enters choices"
    );
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(peacekeeper)]
    );
}
