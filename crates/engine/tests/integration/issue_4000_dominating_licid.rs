//! Issue #4000 — Dominating Licid's licid activation must not present a yes/no
//! optional prompt. The trailing "you may pay {U} to end this effect" is a
//! separate termination permission, not an optional resolution choice on the
//! activation itself.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::AbilityDefinition;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

const DOMINATING_LICID_ORACLE: &str = "{1}{U}{U}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {U} to end this effect.\n\
You control enchanted creature.";

fn ability_tree_has_optional(def: &AbilityDefinition) -> bool {
    if def.optional {
        return true;
    }
    def.sub_ability
        .as_ref()
        .is_some_and(|sub| ability_tree_has_optional(sub))
        || def
            .else_ability
            .as_ref()
            .is_some_and(|sub| ability_tree_has_optional(sub))
}

#[test]
fn dominating_licid_parsed_activation_is_not_optional() {
    let parsed = parse_oracle_text(
        DOMINATING_LICID_ORACLE,
        "Dominating Licid",
        &[],
        &[],
        &["Licid".to_string()],
    );
    assert_eq!(parsed.abilities.len(), 1, "expected one activated ability");
    let ability = &parsed.abilities[0];
    assert!(
        !ability.optional,
        "licid activation must not be optional at parse time"
    );
    assert!(
        !ability_tree_has_optional(ability),
        "licid activation chain must not carry optional=true anywhere"
    );
}

#[test]
fn dominating_licid_activation_does_not_open_optional_effect_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut mana = floating_mana(2, ManaType::Blue);
    mana.extend(floating_mana(1, ManaType::Colorless));
    scenario.with_mana_pool(P0, mana);

    let licid = scenario
        .add_creature(P0, "Dominating Licid", 1, 1)
        .from_oracle_text(DOMINATING_LICID_ORACLE)
        .id();
    let target = scenario.add_creature(P1, "Elite Vanguard", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .activate(licid, 0)
        .target_object(target)
        .decline_optional()
        .resolve();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "licid activation must not end on OptionalEffectChoice; got {:?}",
        runner.state().waiting_for
    );
}
