//! GitHub issue #3297 — Rite of the Raging Storm grants haste/trample/sacrifice
//! to the enchantment instead of the Lightning Rager token it creates.
//!
//! Oracle text (upkeep clause):
//!   "At the beginning of each player's upkeep, that player creates a 5/1 red
//!   Elemental creature token named Lightning Rager. It has trample, haste, and
//!   \"At the beginning of the end step, sacrifice this token.\""

use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const RITE_ORACLE: &str = "Creatures named Lightning Rager can't attack you or planeswalkers you control.\n\
At the beginning of each player's upkeep, that player creates a 5/1 red Elemental creature token named Lightning Rager. \
It has trample, haste, and \"At the beginning of the end step, sacrifice this token.\"";

#[test]
fn rite_of_the_raging_storm_parsed_token_grant_targets_last_created() {
    let parsed = parse_oracle_text(
        RITE_ORACLE,
        "Rite of the Raging Storm",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let upkeep = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("upkeep trigger");
    let sub = upkeep
        .execute
        .as_ref()
        .and_then(|exec| exec.sub_ability.as_ref())
        .expect("token grant sub_ability");
    match sub.effect.as_ref() {
        Effect::GenericEffect {
            target,
            static_abilities,
            ..
        } => {
            assert_eq!(target, &Some(TargetFilter::LastCreated));
            assert_eq!(
                static_abilities[0].affected,
                Some(TargetFilter::LastCreated)
            );
        }
        other => panic!("expected GenericEffect token grant, got {other:?}"),
    }
}

#[test]
fn rite_of_the_raging_storm_grants_abilities_to_created_token_not_enchantment() {
    let mut scenario = GameScenario::new();
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);

    let rite = scenario
        .add_creature(P0, "Rite of the Raging Storm", 0, 0)
        .as_enchantment()
        .from_oracle_text(RITE_ORACLE)
        .id();

    let mut runner = scenario.build();
    // The real card database stores `keywords: []` for Rite — scenario oracle
    // inference falsely treats "haste" in the token clause as a card keyword.
    {
        let rite_obj = runner.state_mut().objects.get_mut(&rite).unwrap();
        rite_obj.keywords.clear();
        rite_obj.base_keywords.clear();
    }

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    let state = runner.state();
    let rager_tokens: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.is_token && obj.name == "Lightning Rager")
        .collect();

    assert_eq!(
        rager_tokens.len(),
        1,
        "upkeep trigger must create exactly one Lightning Rager token"
    );
    let rager = rager_tokens[0].id;

    assert!(
        object_has_effective_keyword_kind(state, rager, KeywordKind::Haste),
        "Lightning Rager token must have haste"
    );
    assert!(
        object_has_effective_keyword_kind(state, rager, KeywordKind::Trample),
        "Lightning Rager token must have trample"
    );

    let rite_obj = state.objects.get(&rite).expect("Rite still on battlefield");
    assert_eq!(rite_obj.zone, Zone::Battlefield);
    assert!(
        !object_has_effective_keyword_kind(state, rite, KeywordKind::Haste),
        "Rite of the Raging Storm must not gain haste from its own token clause"
    );
    assert!(
        !object_has_effective_keyword_kind(state, rite, KeywordKind::Trample),
        "Rite of the Raging Storm must not gain trample from its own token clause"
    );
}
