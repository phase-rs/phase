//! Issue #4824: library-search put steps that attach Auras must honor the
//! parsed attach host (SelfRef, ParentTarget, or typed target creature/player)
//! instead of opening an Aura host-choice prompt.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter, TypedFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LIGHT_PAWS_ORACLE: &str =
    "Whenever an Aura you control enters, if you cast it, you may search your library for an Aura card with mana value less than or equal to that Aura and with a different name than each Aura you control, put that card onto the battlefield attached to Light-Paws, then shuffle.";

const TYPED_CREATURE_TUTOR_ORACLE: &str =
    "{T}: Search your library for an Aura card, put it onto the battlefield attached to target creature, then shuffle.";

const TYPED_PLAYER_TUTOR_ORACLE: &str =
    "{T}: Search your library for a Curse card, put it onto the battlefield attached to target player, then shuffle.";

fn search_attach_host_from_ability(
    ability: &engine::types::ability::AbilityDefinition,
) -> &TargetFilter {
    let change_zone = ability.sub_ability.as_ref().expect("search put-step sub");
    let attach = change_zone
        .sub_ability
        .as_ref()
        .expect("attach sub")
        .effect
        .as_ref();
    match attach {
        Effect::Attach { target, .. } => target,
        other => panic!("expected Attach sub, got {other:?}"),
    }
}

#[test]
fn typed_creature_tutor_oracle_search_attach_host_parses_as_creature() {
    let parsed = parse_oracle_text(
        TYPED_CREATURE_TUTOR_ORACLE,
        "Aura Tutor",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("activated ability");
    assert_eq!(
        search_attach_host_from_ability(ability),
        &TargetFilter::Typed(TypedFilter::creature()),
        "typed creature attach host must parse from search continuation"
    );
}

#[test]
fn typed_player_tutor_oracle_search_attach_host_parses_as_player() {
    let parsed = parse_oracle_text(
        "{T}: Search your library for a Curse card, put it onto the battlefield attached to target player, then shuffle.",
        "Curse Tutor",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("activated ability");
    assert_eq!(
        search_attach_host_from_ability(ability),
        &TargetFilter::Player,
        "typed player attach host must parse from search continuation"
    );
}

#[test]
fn light_paws_oracle_search_attach_host_parses_as_self_ref() {
    let parsed = parse_oracle_text(
        LIGHT_PAWS_ORACLE,
        "Light-Paws, Emperor's Voice",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed.triggers.first().expect("trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    let sub = execute.sub_ability.as_ref().expect("change zone sub");
    let attach = sub
        .sub_ability
        .as_ref()
        .expect("attach sub")
        .effect
        .as_ref();
    match attach {
        Effect::Attach { target, .. } => {
            assert_eq!(
                target,
                &TargetFilter::SelfRef,
                "search put-step must attach to the ability source (~)"
            );
        }
        other => panic!("expected Attach sub, got {other:?}"),
    }
}

#[test]
fn light_paws_searched_aura_enters_attached_without_host_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let light_paws = scenario
        .add_creature_from_oracle(P0, "Light-Paws, Emperor's Voice", 1, 2, LIGHT_PAWS_ORACLE)
        .id();

    let searched_aura = scenario
        .add_spell_to_library_top(P0, "Search Aura Two", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let cast_aura = scenario
        .add_spell_to_hand(P0, "Cast Aura One", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(9_999), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    runner
        .cast(cast_aura)
        .target_object(light_paws)
        .accept_optional()
        .search_first_legal()
        .resolve();

    assert_eq!(
        runner.state().objects[&searched_aura].zone,
        Zone::Battlefield,
        "searched Aura must enter the battlefield from the library search"
    );
    assert_eq!(
        runner.state().objects[&searched_aura].attached_to,
        Some(AttachTarget::Object(light_paws)),
        "searched Aura must attach to Light-Paws without a host-choice prompt"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. } | WaitingFor::TargetSelection { .. }
        ),
        "search put must not surface an Aura host prompt, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn searched_aura_attaches_to_chosen_creature_without_host_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tutor = scenario
        .add_creature_from_oracle(P0, "Aura Tutor", 1, 1, TYPED_CREATURE_TUTOR_ORACLE)
        .id();
    let chosen_target = scenario.add_vanilla(P0, 2, 2);
    let _decoy_host = scenario.add_vanilla(P0, 3, 3);

    let searched_aura = scenario
        .add_spell_to_library_top(P0, "Searched Aura", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();

    runner
        .activate(tutor, 0)
        .target_object(chosen_target)
        .search_first_legal()
        .resolve();

    assert_eq!(
        runner.state().objects[&searched_aura].zone,
        Zone::Battlefield,
        "searched Aura must enter the battlefield from the library search"
    );
    assert_eq!(
        runner.state().objects[&searched_aura].attached_to,
        Some(AttachTarget::Object(chosen_target)),
        "searched Aura must attach to the ability's chosen target creature"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. } | WaitingFor::TargetSelection { .. }
        ),
        "typed creature attach host must not surface an Aura host prompt, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn searched_curse_attaches_to_chosen_player_without_host_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tutor = scenario
        .add_creature_from_oracle(P0, "Curse Tutor", 1, 1, TYPED_PLAYER_TUTOR_ORACLE)
        .id();

    let searched_curse = scenario
        .add_spell_to_library_top(P0, "Searched Curse", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura", "Curse"])
        .with_keyword(Keyword::Enchant(TargetFilter::Player))
        .id();

    let mut runner = scenario.build();

    runner
        .activate(tutor, 0)
        .target_player(P1)
        .search_first_legal()
        .resolve();

    assert_eq!(
        runner.state().objects[&searched_curse].zone,
        Zone::Battlefield,
        "searched Curse must enter the battlefield from the library search"
    );
    assert_eq!(
        runner.state().objects[&searched_curse].attached_to,
        Some(AttachTarget::Player(P1)),
        "searched Curse must attach to the ability's chosen target player"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. } | WaitingFor::TargetSelection { .. }
        ),
        "typed player attach host must not surface an Aura host prompt when P0 is also legal, got {:?}",
        runner.state().waiting_for
    );
}
