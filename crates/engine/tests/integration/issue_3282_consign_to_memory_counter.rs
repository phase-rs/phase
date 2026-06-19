//! Issue #3282 — Consign to Memory counter target.
//!
//! Oracle: "Counter target triggered ability or colorless spell."
//! Before the fix, the parser produced bare `StackAbility` (any ability) and
//! dropped the colorless-spell disjunct, so activated abilities were wrongly
//! legal and colorless spells were not.

use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect;
use engine::types::ability::{Effect, ResolvedAbility, StackAbilityKind, TargetFilter, TargetRef};
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::zones::Zone;
use engine::types::PlayerId;

fn consign_counter_target() -> TargetFilter {
    match parse_effect("Counter target triggered ability or colorless spell") {
        Effect::Counter { target, .. } => target,
        other => panic!("expected Counter effect, got {other:?}"),
    }
}

#[test]
fn consign_to_memory_parses_triggered_or_colorless_disjunction() {
    let target = consign_counter_target();

    let TargetFilter::Or { filters } = &target else {
        panic!("expected Or disjunction, got {target:?}");
    };
    assert_eq!(filters.len(), 2, "must have triggered-ability + spell legs");

    assert!(
        filters.iter().any(|f| matches!(
            f,
            TargetFilter::StackAbility {
                kind: Some(StackAbilityKind::Triggered),
                ..
            }
        )),
        "ability leg must narrow to triggered only: {target:?}"
    );

    assert!(
        filters.iter().any(|f| matches!(f, TargetFilter::Typed(_))),
        "spell leg must be a stack-pinned typed filter: {target:?}"
    );
}

fn stack_with_counter_targets() -> (GameState, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut state = GameState::new_two_player(42);

    let perm = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Ability Source".to_string(),
        Zone::Battlefield,
    );

    let activated = ObjectId(901);
    let triggered = ObjectId(902);
    state.stack.push_back(StackEntry {
        id: activated,
        source_id: perm,
        controller: PlayerId(1),
        kind: StackEntryKind::ActivatedAbility {
            source_id: perm,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "Act".to_string(),
                    description: None,
                },
                vec![],
                perm,
                PlayerId(1),
            ),
        },
    });
    state.stack.push_back(StackEntry {
        id: triggered,
        source_id: perm,
        controller: PlayerId(1),
        kind: StackEntryKind::TriggeredAbility {
            source_id: perm,
            ability: Box::new(ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "Trig".to_string(),
                    description: None,
                },
                vec![],
                perm,
                PlayerId(1),
            )),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    });

    // Colorless spell on the stack.
    let colorless_spell = create_object(
        &mut state,
        CardId(10),
        PlayerId(1),
        "Colorless Spell".to_string(),
        Zone::Stack,
    );
    {
        let artifact = CardType {
            core_types: vec![CoreType::Artifact],
            ..Default::default()
        };
        let obj = state.objects.get_mut(&colorless_spell).unwrap();
        obj.card_types = artifact.clone();
        obj.base_card_types = artifact;
        obj.color.clear();
    }

    // Colored spell on the stack.
    let colored_spell = create_object(
        &mut state,
        CardId(11),
        PlayerId(1),
        "Colored Spell".to_string(),
        Zone::Stack,
    );
    {
        let instant = CardType {
            core_types: vec![CoreType::Instant],
            ..Default::default()
        };
        let obj = state.objects.get_mut(&colored_spell).unwrap();
        obj.card_types = instant.clone();
        obj.base_card_types = instant;
        obj.color = vec![engine::types::mana::ManaColor::Red];
    }

    for (id, card_id) in [(colorless_spell, CardId(10)), (colored_spell, CardId(11))] {
        state.stack.push_back(StackEntry {
            id,
            source_id: id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id,
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
    }

    (state, activated, triggered, colorless_spell, colored_spell)
}

#[test]
fn consign_to_memory_counter_target_legality() {
    let filter = consign_counter_target();
    let (state, activated, triggered, colorless_spell, colored_spell) =
        stack_with_counter_targets();

    let source = ObjectId(1000);
    let legal = find_legal_targets(&state, &filter, PlayerId(0), source);
    let is_legal = |id: ObjectId| legal.contains(&TargetRef::Object(id));

    assert!(
        !is_legal(activated),
        "activated abilities must NOT be legal targets"
    );
    assert!(
        is_legal(triggered),
        "triggered abilities must be legal targets"
    );
    assert!(
        is_legal(colorless_spell),
        "colorless spells must be legal targets"
    );
    assert!(
        !is_legal(colored_spell),
        "colored spells must NOT be legal targets"
    );
}
