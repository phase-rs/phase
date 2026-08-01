//! Throne of Eldraine's source-chosen-color mana riders exercise the complete
//! production path: Oracle parsing, mana production, cast payment, activated
//! ability payment, and the manual pool-pin/resume interface.

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{ChosenAttribute, Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, PendingCast, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaPipId, ManaType, ManaUnit};
use engine::types::phase::Phase;

const THRONE_OF_ELDRAINE: &str = "As Throne of Eldraine enters, choose a color.\n\
{T}: Add four mana of the chosen color. Spend this mana only to cast monocolored spells of that color.\n\
{3}, {T}: Draw two cards. Spend only mana of the chosen color to activate this ability.";

fn add_throne(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Throne of Eldraine", 0, 1, THRONE_OF_ELDRAINE)
        .as_artifact()
        .id()
}

fn choose_red(state: &mut GameState, throne: ObjectId) {
    state
        .objects
        .get_mut(&throne)
        .expect("Throne must be on the battlefield")
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));
}

fn throne_ability_indices(state: &GameState, throne: ObjectId) -> (usize, usize) {
    let abilities = activated_ability_definitions(state, throne);
    let mana = abilities
        .iter()
        .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::Mana { .. }))
        .map(|(index, _)| *index)
        .expect("Throne must have its mana ability");
    let draw = abilities
        .iter()
        .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::Draw { .. }))
        .map(|(index, _)| *index)
        .expect("Throne must have its draw ability");
    (mana, draw)
}

/// The produced units carry the chosen color of the actual producing Throne,
/// and that restriction is consulted by the normal cast action. Each spell has
/// a generic cost so colored-cost matching cannot hide a spend-restriction bug.
#[test]
fn throne_mana_casts_only_monocolored_spells_of_its_chosen_color() {
    for (label, colors, allowed) in [
        ("red", vec![ManaColor::Red], true),
        ("blue", vec![ManaColor::Blue], false),
        ("multicolored", vec![ManaColor::Red, ManaColor::Blue], false),
        ("colorless", vec![], false),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let throne = add_throne(&mut scenario);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, &format!("{label} test spell"), true, "")
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let mut runner = scenario.build();
        choose_red(runner.state_mut(), throne);
        {
            let obj = runner
                .state_mut()
                .objects
                .get_mut(&spell)
                .expect("test spell must be in hand");
            obj.color = colors.clone();
            obj.base_color = colors;
        }

        let (mana, _) = throne_ability_indices(runner.state(), throne);
        let waiting = runner
            .act(GameAction::ActivateAbility {
                source_id: throne,
                ability_index: mana,
            })
            .expect("Throne mana ability must resolve")
            .waiting_for;
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
        let pool = &runner.state().players[P0.0 as usize].mana_pool.mana;
        assert_eq!(pool.len(), 4, "Throne must produce four mana");
        assert!(
            pool.iter()
                .all(|unit| unit.source_id == throne && unit.color == ManaType::Red),
            "every unit must retain the producing Throne and its chosen red color: {pool:?}"
        );

        let card_id = runner.state().objects[&spell].card_id;
        let result = runner.act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        assert_eq!(
            result.is_ok(),
            allowed,
            "a {label} spell must {} be cast with mana from the red-chosen Throne: {result:?}",
            if allowed { "" } else { "not" }
        );
    }
}

fn draw_payment_state(mana: &[ManaType]) -> (GameState, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let throne = add_throne(&mut scenario);
    scenario.add_card_to_library_top(P0, "Throne draw test card one");
    scenario.add_card_to_library_top(P0, "Throne draw test card two");
    let mut state = scenario.build().state().clone();
    choose_red(&mut state, throne);
    let (_, draw) = throne_ability_indices(&state, throne);
    for (index, color) in mana.iter().copied().enumerate() {
        state.add_mana_to_pool(
            P0,
            ManaUnit::new(color, ObjectId(10_000 + index as u64), false, Vec::new()),
        );
    }
    (state, throne, draw)
}

/// The draw rider constrains the actual mana units used for activation. This
/// covers automatic payment and the direct manual-pool fixture. `ActivateAbility`
/// currently has no manual-payment mode, so the latter exercises the internal
/// manual-payment branch without claiming an unreachable production route.
#[test]
fn throne_draw_activation_uses_only_its_chosen_color_in_auto_and_manual_payment() {
    let (red_state, red_throne, draw) =
        draw_payment_state(&[ManaType::Red, ManaType::Red, ManaType::Red]);
    let mut red_runner = GameRunner::from_state(red_state);
    let red_waiting = red_runner
        .act(GameAction::ActivateAbility {
            source_id: red_throne,
            ability_index: draw,
        })
        .expect("three red mana must pay the red-chosen Throne draw ability")
        .waiting_for;
    assert!(matches!(red_waiting, WaitingFor::Priority { .. }));
    assert!(red_runner.state().objects[&red_throne].tapped);
    assert_eq!(
        red_runner.state().players[P0.0 as usize].mana_pool.total(),
        0
    );

    for (label, mana) in [
        ("blue", vec![ManaType::Blue, ManaType::Blue, ManaType::Blue]),
        ("mixed", vec![ManaType::Red, ManaType::Red, ManaType::Blue]),
    ] {
        let (state, throne, draw) = draw_payment_state(&mana);
        let mut runner = GameRunner::from_state(state);
        assert!(
            runner
                .act(GameAction::ActivateAbility {
                    source_id: throne,
                    ability_index: draw,
                })
                .is_err(),
            "a {label} pool must not pay a red-chosen Throne draw activation"
        );
        assert!(!runner.state().objects[&throne].tapped);
    }

    let (mut state, throne, draw) =
        draw_payment_state(&[ManaType::Red, ManaType::Red, ManaType::Red, ManaType::Blue]);
    let blue_pip = state.players[P0.0 as usize]
        .mana_pool
        .mana
        .iter()
        .find(|unit| unit.color == ManaType::Blue)
        .expect("manual pool has a blue unit")
        .pip_id;
    let red_pip = state.players[P0.0 as usize]
        .mana_pool
        .mana
        .iter()
        .find(|unit| unit.color == ManaType::Red)
        .expect("manual pool has a red unit")
        .pip_id;
    assert_ne!(
        blue_pip,
        ManaPipId(0),
        "seeded pool units must have stable ids"
    );

    let draw_ability = activated_ability_definitions(&state, throne)
        .into_iter()
        .find(|(index, _)| *index == draw)
        .map(|(_, ability)| ability)
        .expect("Throne draw ability definition");
    let mut pending = PendingCast::new(
        throne,
        CardId(0xED),
        ResolvedAbility::new((*draw_ability.effect).clone(), Vec::new(), throne, P0),
        ManaCost::generic(3),
    );
    pending.activation_ability_index = Some(draw);
    state.pending_cast = Some(Box::new(pending));
    state.objects.get_mut(&throne).unwrap().tapped = true;
    state.waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: None,
    };
    assert!(matches!(state.waiting_for, WaitingFor::ManaPayment { .. }));
    let mut runner = GameRunner::from_state(state);
    assert!(
        runner
            .act(GameAction::SpendPoolMana { pip_id: blue_pip })
            .is_err(),
        "manual payment must reject pinning blue mana for the red-chosen activation"
    );
    runner
        .act(GameAction::SpendPoolMana { pip_id: red_pip })
        .expect("manual payment must accept a red mana pin");
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let library_before = runner.state().players[P0.0 as usize].library.len();
    runner
        .act(GameAction::PassPriority)
        .expect("pending activation must finalize from the eligible red mana");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("activator passes priority to resolve the paid activation");
    runner
        .act(GameAction::PassPriority)
        .expect("opponent passes priority to resolve the paid activation");
    let player = &runner.state().players[P0.0 as usize];
    assert_eq!(
        player.hand.len(),
        hand_before + 2,
        "the restricted-mana activation must resolve and draw two cards"
    );
    assert_eq!(
        player.library.len(),
        library_before - 2,
        "the draw must consume exactly two library cards"
    );
    assert_eq!(
        player.mana_pool.mana.len(),
        1,
        "only the ineligible blue mana may remain after paying {{3}} with red mana"
    );
    assert_eq!(
        player.mana_pool.mana[0].pip_id, blue_pip,
        "the red pinned mana must be consumed rather than silently abandoned"
    );
}

/// The activation rider filters source enumeration before auto-tap. This is a
/// real `ActivateAbility` path (rather than a pre-filled pool): an Island is
/// deliberately registered before the three Mountains, so selecting any
/// off-color source for the generic cost would leave the activation unpaid.
#[test]
fn throne_draw_auto_tap_uses_only_chosen_color_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let throne = add_throne(&mut scenario);
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    let mountains: Vec<_> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    scenario.with_library_top(P0, &["Throne draw one", "Throne draw two"]);
    let mut runner = scenario.build();
    choose_red(runner.state_mut(), throne);
    let (_, draw) = throne_ability_indices(runner.state(), throne);
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: throne,
            ability_index: draw,
        })
        .expect("three red sources must pay the red-chosen activation");
    runner
        .act(GameAction::PassPriority)
        .expect("activator passes priority to resolve the paid activation");
    runner
        .act(GameAction::PassPriority)
        .expect("opponent passes priority to resolve the paid activation");

    assert!(
        !runner.state().objects[&island].tapped,
        "off-color Island must not be selected for a red-only activation payment"
    );
    assert!(
        mountains
            .iter()
            .all(|mountain| runner.state().objects[mountain].tapped),
        "all three eligible Mountains must be used for the {{3}} cost"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 2,
        "the activation must resolve after auto-tapping only eligible sources"
    );

    // A source may produce an off-color byproduct: only the red unit is spent,
    // while its blue companion remains in the pool after the activation.
    let mut mixed_output = GameScenario::new();
    mixed_output.at_phase(Phase::PreCombatMain);
    let mixed_throne = add_throne(&mut mixed_output);
    let signet = mixed_output
        .add_land_from_oracle(P0, "Izzet Test Signet", "{T}: Add {U}{R}.")
        .id();
    let mixed_mountains: Vec<_> = (0..2)
        .map(|_| mixed_output.add_basic_land(P0, ManaColor::Red))
        .collect();
    mixed_output.with_library_top(P0, &["Mixed draw one", "Mixed draw two"]);
    let mut mixed_runner = mixed_output.build();
    choose_red(mixed_runner.state_mut(), mixed_throne);
    let (_, mixed_draw) = throne_ability_indices(mixed_runner.state(), mixed_throne);
    mixed_runner
        .act(GameAction::ActivateAbility {
            source_id: mixed_throne,
            ability_index: mixed_draw,
        })
        .expect("a red unit from a blue-red source plus two Mountains must pay the activation");
    mixed_runner
        .act(GameAction::PassPriority)
        .expect("activator passes priority");
    mixed_runner
        .act(GameAction::PassPriority)
        .expect("opponent passes priority");
    assert!(mixed_runner.state().objects[&signet].tapped);
    assert!(mixed_mountains
        .iter()
        .all(|mountain| mixed_runner.state().objects[mountain].tapped));
    assert!(
        mixed_runner.state().players[P0.0 as usize]
            .mana_pool
            .mana
            .iter()
            .any(|unit| unit.color == ManaType::Blue),
        "the unspent blue byproduct must remain in the pool"
    );

    let mut blue_only = GameScenario::new();
    blue_only.at_phase(Phase::PreCombatMain);
    let blue_throne = add_throne(&mut blue_only);
    let blue_sources: Vec<_> = (0..3)
        .map(|_| blue_only.add_basic_land(P0, ManaColor::Blue))
        .collect();
    let mut blue_runner = blue_only.build();
    choose_red(blue_runner.state_mut(), blue_throne);
    let (_, blue_draw) = throne_ability_indices(blue_runner.state(), blue_throne);
    assert!(
        blue_runner
            .act(GameAction::ActivateAbility {
                source_id: blue_throne,
                ability_index: blue_draw,
            })
            .is_err(),
        "off-color sources must not make the red-chosen activation payable"
    );
    assert!(
        blue_sources
            .iter()
            .all(|source| !blue_runner.state().objects[source].tapped),
        "a rejected activation must not tap ineligible mana sources"
    );
}
