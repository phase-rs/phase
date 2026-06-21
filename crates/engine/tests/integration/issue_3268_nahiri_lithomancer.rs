//! Issue #3268 — Nahiri, the Lithomancer loyalty abilities:
//! +2 optional attach must not force equipment selection at activation time, and
//! −10 Stoneforged Blade must enter with a working Equip {0} activated ability.

use std::sync::Arc;

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::planeswalker;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::EffectKind;
use engine::types::ability::{AbilityCost, ContinuousModification, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const NAHIRI_ORACLE: &str = "[+2]: Create a 1/1 white Kor Soldier creature token. You may attach an Equipment you control to it.\n\
[−2]: You may put an Equipment card from your hand or graveyard onto the battlefield.\n\
[−10]: Create a colorless Equipment artifact token named Stoneforged Blade. It has indestructible, \"Equipped creature gets +5/+5 and has double strike,\" and equip {0}.\n\
Nahiri, the Lithomancer can be your commander.";

fn parsed_nahiri() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        NAHIRI_ORACLE,
        "Nahiri, the Lithomancer",
        &[],
        &["Legendary".to_string()],
        &["Nahiri".to_string()],
    )
}

fn wire_nahiri_abilities(
    state: &mut engine::types::game_state::GameState,
    nahiri: engine::types::identifiers::ObjectId,
    loyalty: u32,
    abilities: Vec<engine::types::ability::AbilityDefinition>,
) {
    let obj = state.objects.get_mut(&nahiri).expect("nahiri");
    obj.card_types.core_types = vec![CoreType::Planeswalker];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
    obj.abilities = Arc::new(abilities);
    obj.base_abilities = Arc::new(obj.abilities.iter().cloned().collect());
}

fn setup_nahiri(
    loyalty: u32,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let parsed = parsed_nahiri();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nahiri = scenario
        .add_creature(P0, "Nahiri, the Lithomancer", 0, 0)
        .id();
    let mut runner = scenario.build();
    wire_nahiri_abilities(runner.state_mut(), nahiri, loyalty, parsed.abilities);
    (runner, nahiri)
}

#[test]
fn nahiri_plus_two_attach_sub_is_optional_and_targets_last_created() {
    let face = parsed_nahiri();
    let plus_two = &face.abilities[0];
    assert!(matches!(
        plus_two.cost,
        Some(AbilityCost::Loyalty { amount: 2 })
    ));
    let attach = plus_two
        .sub_ability
        .as_ref()
        .expect("+2 must chain optional attach");
    assert!(attach.optional, "attach must be optional (you may attach)");
    let Effect::Attach { target, .. } = attach.effect.as_ref() else {
        panic!("expected Attach sub, got {:?}", attach.effect);
    };
    assert_eq!(
        *target,
        TargetFilter::LastCreated,
        "attach host must be the created Kor Soldier token"
    );
}

#[test]
fn nahiri_plus_two_does_not_collect_attach_targets_at_activation() {
    let parsed = parsed_nahiri();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nahiri = scenario
        .add_creature(P0, "Nahiri, the Lithomancer", 0, 0)
        .id();
    let mut runner = scenario.build();
    let _equipment = create_object(
        runner.state_mut(),
        CardId(99),
        P0,
        "Bonesplitter".to_string(),
        Zone::Battlefield,
    );
    wire_nahiri_abilities(runner.state_mut(), nahiri, 5, parsed.abilities.clone());
    {
        let state = runner.state();
        let resolved = build_resolved_from_def(&parsed.abilities[0], nahiri, P0);
        let slots = build_target_slots(state, &resolved).expect("slot build");
        assert!(
            slots.is_empty(),
            "+2 must not surface attach equipment targets at activation, got {slots:?}"
        );
    }
    let mut events = Vec::new();
    let waiting =
        planeswalker::handle_activate_loyalty(runner.state_mut(), P0, nahiri, 0, &mut events)
            .expect("activate +2");
    assert!(
        !matches!(waiting, WaitingFor::TargetSelection { .. }),
        "+2 must not force target selection at activation when attach is optional"
    );
}

fn add_equipment(
    runner: &mut engine::game::scenario::GameRunner,
    name: &str,
    card_id: u64,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        runner.state_mut(),
        CardId(card_id),
        P0,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&id).expect("equipment");
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Equipment".to_string());
    id
}

#[test]
fn nahiri_plus_two_prompts_equipment_choice_after_accepting_optional_attach() {
    let (mut runner, nahiri) = setup_nahiri(5);
    let first_equipment = add_equipment(&mut runner, "Bonesplitter", 99);
    let chosen_equipment = add_equipment(&mut runner, "Skullclamp", 100);

    runner.activate(nahiri, 0).accept_optional().resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Attach,
                ..
            }
        ),
        "accepting optional attach must prompt for Equipment when multiple are legal"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen_equipment],
        })
        .expect("equipment selection");

    let state = runner.state();
    let kor = state
        .last_created_token_ids
        .first()
        .copied()
        .expect("Kor Soldier token");
    assert_eq!(
        state
            .objects
            .get(&chosen_equipment)
            .and_then(|o| o.attached_to),
        Some(engine::game::game_object::AttachTarget::Object(kor)),
        "chosen Equipment must attach to the created Kor Soldier"
    );
    assert!(
        state
            .objects
            .get(&first_equipment)
            .unwrap()
            .attached_to
            .is_none(),
        "unselected Equipment must stay unattached"
    );
}

#[test]
fn nahiri_minus_ten_stoneforged_blade_carries_equip_on_token_statics() {
    let face = parsed_nahiri();
    let minus_ten = &face.abilities[2];
    assert!(matches!(
        minus_ten.cost,
        Some(AbilityCost::Loyalty { amount: -10 })
    ));
    let Effect::Token {
        name,
        static_abilities,
        ..
    } = minus_ten.effect.as_ref()
    else {
        panic!("−10 root must be Token, got {:?}", minus_ten.effect);
    };
    assert_eq!(name, "Stoneforged Blade");
    assert!(
        !static_abilities.is_empty(),
        "Stoneforged Blade grants must live on Token.static_abilities, not a GenericEffect sibling"
    );
    assert!(
        static_abilities.iter().any(|static_def| {
            static_def.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::GrantAbility { definition }
                        if matches!(*definition.effect, Effect::Attach { .. })
                )
            })
        }),
        "Stoneforged Blade must grant an Equip activated ability, got {static_abilities:?}"
    );
    assert!(
        minus_ten.sub_ability.is_none()
            || !matches!(
                minus_ten.sub_ability.as_ref().map(|s| s.effect.as_ref()),
                Some(Effect::GenericEffect { .. })
            ),
        "It has … grants must be folded into the token, not left as GenericEffect"
    );
}

#[test]
fn nahiri_minus_ten_stoneforged_blade_enters_with_equip_ability() {
    let (mut runner, nahiri) = setup_nahiri(15);
    runner.activate(nahiri, 2).resolve();

    let state = runner.state();
    let blade = state
        .last_created_token_ids
        .first()
        .copied()
        .expect("Stoneforged Blade token");
    let blade_obj = &state.objects[&blade];
    assert!(
        blade_obj
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::Attach { .. })),
        "Stoneforged Blade must have an equip activated ability on the battlefield"
    );
}
