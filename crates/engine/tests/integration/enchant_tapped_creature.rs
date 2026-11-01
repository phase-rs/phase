//! CR 702.5a: "Enchant tapped creature" (Entangling Vines) restricts the
//! Aura spell's legal targets to creatures that are tapped.
//!
//! Oracle text is verbatim from Scryfall.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{FilterProp, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const ENTANGLING_VINES: &str =
    "Enchant tapped creature\nEnchanted creature doesn't untap during its controller's untap step.";

#[test]
fn entangling_vines_enchant_keyword_requires_a_tapped_creature() {
    let kw: Keyword = "Enchant:tapped creature".parse().expect("keyword parses");
    let Keyword::Enchant(TargetFilter::Typed(typed)) = kw else {
        panic!("expected a typed Enchant filter, got {kw:?}");
    };
    assert!(typed.properties.contains(&FilterProp::Tapped));
}

#[test]
fn entangling_vines_offers_only_tapped_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Untapped creature is inserted last so the tapped ones are not favored by order.
    let tapped_opp = scenario.add_creature(P1, "Tapped Bear", 2, 2).id();
    let tapped_own = scenario.add_creature(P0, "Tapped Cub", 1, 1).id();
    let untapped = scenario.add_creature(P1, "Untapped Bear", 2, 2).id();
    let vines = scenario
        .add_spell_to_hand(P0, "Entangling Vines", false)
        .from_oracle_text_with_keywords(&["Enchant"], ENTANGLING_VINES)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    for id in [tapped_opp, tapped_own] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    // Reach guard: the card carries a parsed Enchant restriction.
    assert!(
        runner.state().objects[&vines]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Enchant(_))),
        "Entangling Vines must carry an Enchant keyword"
    );

    let card_id = runner.state().objects[&vines].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: vines,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Entangling Vines");

    let legal: Vec<ObjectId> = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots
            .iter()
            .flat_map(|s| s.legal_targets.iter())
            .filter_map(|t| match t {
                TargetRef::Object(o) => Some(*o),
                _ => None,
            })
            .collect(),
        other => panic!("expected Aura target selection, got {other:?}"),
    };
    assert!(
        legal.contains(&tapped_opp),
        "tapped creature is legal: {legal:?}"
    );
    assert!(
        legal.contains(&tapped_own),
        "tapped creature is legal: {legal:?}"
    );
    assert!(
        !legal.contains(&untapped),
        "untapped creature is illegal: {legal:?}"
    );
}
