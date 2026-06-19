//! Regression for GitHub issue #3325 — Umbral Mantle + Brigid starter deck.
//!
//! Oracle:
//!   Equipped creature has "{3}, {Q}: This creature gets +2/+2 until end of turn."
//!   Equip {0}

use engine::ai_support::legal_actions_full;
use engine::game::casting::can_activate_ability_now;
use engine::game::derived::derive_display_state;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityKind, ContinuousModification, Effect, FilterProp, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const UMBRAL_MANTLE: &str =
    "Equipped creature has \"{3}, {Q}: This creature gets +2/+2 until end of turn.\" ({Q} is the untap symbol.)\nEquip {0}";

const BRIGID: &str =
    "{T}: Add X {G} or X {W}, where X is the number of other creatures you control.";

fn add_three_generic(
    runner: &mut engine::game::scenario::GameRunner,
    source: engine::types::identifiers::ObjectId,
) {
    for _ in 0..3 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            source,
            false,
            vec![],
        ));
    }
}

fn granted_untap_pump_index(obj: &engine::game::game_object::GameObject) -> Option<usize> {
    obj.abilities.iter().position(|a| {
        a.kind == AbilityKind::Activated
            && matches!(
                a.cost.as_ref(),
                Some(AbilityCost::Composite { costs })
                    if costs.iter().any(|c| matches!(c, AbilityCost::Untap))
            )
    })
}

#[test]
fn umbral_mantle_static_parses_granted_untap_pump() {
    use engine::parser::oracle_static::parse_static_line_multi;

    let defs = parse_static_line_multi(
        "Equipped creature has \"{3}, {Q}: This creature gets +2/+2 until end of turn.\"",
    );
    assert_eq!(
        defs.len(),
        1,
        "expected one continuous static, got {defs:?}"
    );
    let def = &defs[0];
    assert_eq!(
        def.affected,
        Some(engine::types::ability::TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ))
    );
    let grant = def
        .modifications
        .iter()
        .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }));
    let Some(ContinuousModification::GrantAbility { definition }) = grant else {
        panic!("expected GrantAbility, got {:?}", def.modifications);
    };
    assert_eq!(definition.kind, AbilityKind::Activated);
    assert!(matches!(&*definition.effect, Effect::Pump { .. }));
    assert!(matches!(
        definition.cost.as_ref(),
        Some(AbilityCost::Composite { costs })
            if costs.iter().any(|c| matches!(c, AbilityCost::Untap))
    ));
}

#[test]
fn umbral_mantle_static_with_reminder_text_parses_granted_untap_pump() {
    use engine::parser::oracle_static::parse_static_line_multi;

    let defs = parse_static_line_multi(
        "Equipped creature has \"{3}, {Q}: This creature gets +2/+2 until end of turn.\" ({Q} is the untap symbol.)",
    );
    assert_eq!(
        defs.len(),
        1,
        "expected one continuous static, got {defs:?}"
    );
    assert!(
        defs[0]
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::GrantAbility { .. })),
        "reminder text must not break GrantAbility parse: {:?}",
        defs[0].modifications
    );
}

#[test]
fn umbral_mantle_grants_untap_pump_ability_to_equipped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID)
        .id();
    let mantle = scenario
        .add_creature(P0, "Umbral Mantle", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(UMBRAL_MANTLE)
        .id();

    let mut runner = scenario.build();
    add_three_generic(&mut runner, brigid);
    {
        let brigid_obj = runner.state_mut().objects.get_mut(&brigid).unwrap();
        brigid_obj.summoning_sick = false;
        brigid_obj.entered_battlefield_turn = None;
        brigid_obj.tapped = true;
    }
    attach_to(runner.state_mut(), mantle, brigid);
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());

    let brigid_obj = runner.state().objects.get(&brigid).unwrap();
    let granted_idx = granted_untap_pump_index(brigid_obj)
        .expect("equipped creature must gain the {3},{Q} pump ability from Umbral Mantle");
    let granted = &brigid_obj.abilities[granted_idx];
    assert!(
        matches!(&*granted.effect, Effect::Pump { .. }),
        "granted ability should pump, got {:?}",
        granted.effect
    );
    assert!(
        can_activate_ability_now(runner.state(), P0, brigid, granted_idx),
        "granted {{3}},{{Q}} pump must be activatable on equipped Brigid"
    );

    let (_, _, grouped) = legal_actions_full(runner.state());
    assert!(
        grouped.get(&brigid).is_some_and(|actions| {
            actions.iter().any(|a| {
                matches!(
                    a,
                    GameAction::ActivateAbility {
                        source_id,
                        ability_index,
                    } if *source_id == brigid && *ability_index == granted_idx
                )
            })
        }),
        "legal_actions_by_object must surface the granted Umbral Mantle ability"
    );
}

#[test]
fn umbral_mantle_equip_zero_via_activated_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID)
        .id();
    let mantle = scenario
        .add_creature(P0, "Umbral Mantle", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(UMBRAL_MANTLE)
        .id();

    let mut runner = scenario.build();
    let equip_idx = runner.state().objects[&mantle]
        .abilities
        .iter()
        .position(|a| {
            a.description
                .as_deref()
                .is_some_and(|d| d.contains("Equip"))
        })
        .expect("Umbral Mantle equip ability");

    runner
        .activate(mantle, equip_idx)
        .target_object(brigid)
        .resolve();

    let mantle_obj = runner.state().objects.get(&mantle).unwrap();
    assert!(
        matches!(
            mantle_obj.attached_to,
            Some(AttachTarget::Object(id)) if id == brigid
        ),
        "Equip {{0}} must attach Umbral Mantle to Brigid, got {:?}",
        mantle_obj.attached_to
    );
    assert!(
        runner
            .state()
            .objects
            .get(&brigid)
            .unwrap()
            .attachments
            .contains(&mantle),
        "Brigid must list Umbral Mantle as an attachment"
    );
}

fn power_toughness(
    runner: &engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
) -> (i32, i32) {
    let obj = runner.state().objects.get(&id).expect("object present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

#[test]
fn umbral_mantle_granted_pump_resolves_on_equipped_brigid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID)
        .id();
    let mantle = scenario
        .add_creature(P0, "Umbral Mantle", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(UMBRAL_MANTLE)
        .id();

    let mut runner = scenario.build();
    add_three_generic(&mut runner, brigid);
    {
        let brigid_obj = runner.state_mut().objects.get_mut(&brigid).unwrap();
        brigid_obj.summoning_sick = false;
        brigid_obj.entered_battlefield_turn = None;
        brigid_obj.tapped = true;
    }
    attach_to(runner.state_mut(), mantle, brigid);
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());

    let granted_idx = granted_untap_pump_index(
        runner
            .state()
            .objects
            .get(&brigid)
            .expect("Brigid still on battlefield"),
    )
    .expect("equipped Brigid must have the granted {3},{Q} pump");

    assert_eq!(
        power_toughness(&runner, brigid),
        (2, 3),
        "baseline P/T before activation"
    );

    runner.activate(brigid, granted_idx).resolve();

    assert_eq!(
        power_toughness(&runner, brigid),
        (4, 5),
        "Umbral Mantle pump must apply +2/+2 to equipped Brigid (2/3 -> 4/5)"
    );
    assert!(
        !runner.state().objects.get(&brigid).unwrap().tapped,
        "untap-symbol cost must untap the equipped creature when it was tapped"
    );
}
