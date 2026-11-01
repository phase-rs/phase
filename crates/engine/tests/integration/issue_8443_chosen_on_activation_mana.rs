//! Issue #8443: a mana ability that chooses its color during activation
//! ("Choose a color. Add ... of that color") must count as mana when deciding
//! whether a spell can be cast, whether auto-pass skips the priority window,
//! and how auto-tap pays.
//!
//! CR 106.1 + CR 608.2d + CR 607.2d: "Choose a color. Add ... of that color"
//! (Astral Cornucopia, Nykthos, Nyx Lotus, Three Tree City) chooses as the
//! ability resolves, so it is mana of any one color and that provenance travels
//! with the ability. "Add one mana of the chosen color" (Sunken Citadel,
//! Heraldic Banner) refers to a linked choice; unset or undefined, it adds no
//! mana (CR 106.5, CR 607.5a).

use engine::ai_support::{auto_pass_recommended, flat_priority_actions, legal_actions};
use engine::database::synthesis::synthesize_planechase;
use engine::game::planechase::PlanarDieFace;
use engine::game::printed_cards::apply_card_face_to_object;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ChoiceValue,
    ChooseOneOfKind, ChosenAttribute, ContinuousModification, ControllerRef, Duration, Effect,
    ManaProduction, PlayerFilter, QuantityExpr, ReplacementDefinition, StaticDefinition,
    TargetFilter, TargetSelectionMode,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;
use std::sync::Arc;

const ASTRAL: &str = "This artifact enters with X charge counters on it.\n{T}: Choose a color. Add one mana of that color for each charge counter on this artifact.";
const NYKTHOS: &str = "{T}: Add {C}.\n{2}, {T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color.";
const NYX_LOTUS: &str = "This artifact enters tapped.\n{T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color.";
const KCI: &str = "Sacrifice an artifact: Add {C}{C}.";
const SUNKEN_CITADEL: &str = "This land enters tapped. As it enters, choose a color.\n{T}: Add one mana of the chosen color.\n{T}: Add two mana of the chosen color. Spend this mana only to activate abilities of land sources.";
const UNKNOWN_EVENT_SHORES: &str = "When building your deck, choose white, blue, black, red, or green (Circle it to indicate your selection.)\n{T}: Add one mana of the chosen color.\n{1}, {T}: Add 1 mana of any color.";
const CHALICE: &str = "Multikicker {2}\nThis artifact enters with a charge counter on it for each time it was kicked.\n{T}: Add {C} for each charge counter on this artifact.";
const SOL_RING: &str = "{T}: Add {C}{C}.";
const METEOR_CRATER: &str =
    "{T}: Choose a color of a permanent you control. Add one mana of that color.";
const CORRUPTED_GRAFSTONE: &str = "This artifact enters tapped.\n{T}: Choose a color of a card in your graveyard. Add one mana of that color.";

fn charge() -> CounterType {
    CounterType::Generic("charge".to_string())
}

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn add_instant(scenario: &mut GameScenario, mana_cost: ManaCost) -> ObjectId {
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C"]);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Test Instant", true, "Draw a card.")
        .with_mana_cost(mana_cost)
        .id()
}

/// Priority facts for `spell`: (CastSpell offered, auto-pass recommended).
fn priority_facts(runner: &GameRunner, spell: ObjectId) -> (bool, bool) {
    let state = runner.state();
    let offered = legal_actions(state).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
    );
    (
        offered,
        auto_pass_recommended(state, &flat_priority_actions(state)),
    )
}

/// The same facts in a priority window on the opponent's turn, which is where
/// the report hit it (holding up an instant). Restores P0 as active player.
fn opponent_turn_priority_facts(runner: &mut GameRunner, spell: ObjectId) -> (bool, bool) {
    set_priority(runner, P1);
    let facts = priority_facts(runner, spell);
    set_priority(runner, P0);
    facts
}

fn set_priority(runner: &mut GameRunner, active: engine::types::player::PlayerId) {
    let state = runner.state_mut();
    state.active_player = active;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

fn produced_of(runner: &GameRunner, source: ObjectId, index: usize) -> ManaProduction {
    match &*runner.state().objects[&source].abilities[index].effect {
        Effect::Mana { produced, .. } => produced.clone(),
        other => panic!("ability {index} must be a mana ability, got {other:?}"),
    }
}

fn spell_zone(runner: &GameRunner, spell: ObjectId) -> Zone {
    runner.state().objects[&spell].zone
}

/// Reach guard shared by every Astral leg: the printed "Choose a color. Add
/// ..." ability carries its activation-time choice as mana of any one of the
/// five colors, and no color is stored on the object.
fn assert_astral_reach(runner: &GameRunner, astral: ObjectId) {
    assert!(
        matches!(
            produced_of(runner, astral, 0),
            ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
        ),
        "Astral Cornucopia's choice is made on activation (any one color)"
    );
    assert_eq!(
        runner.state().objects[&astral].chosen_color(),
        None,
        "no color chosen before activation"
    );
}

/// CR 106.1 + CR 607.2d + CR 117.1d: Astral Cornucopia with N charge counters
/// pays for spells up to N mana of one color: offered on both turns, auto-pass
/// holds, and the auto-paid cast resolves with the excess left in the pool in
/// the color the cost needed.
#[test]
fn astral_cornucopia_pays_for_spells_at_each_counter_count() {
    let legs: [(u32, ManaCost, ManaType); 3] = [
        (1, cost(vec![ManaCostShard::Red], 0), ManaType::Red),
        (3, cost(vec![ManaCostShard::Red], 2), ManaType::Red),
        // The reported shape: Fuel for the Cause is {2}{U}{U}.
        (
            6,
            cost(vec![ManaCostShard::Blue, ManaCostShard::Blue], 2),
            ManaType::Blue,
        ),
    ];
    for (counters, mana_cost, color) in legs {
        let total = mana_cost.mana_value();
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let astral = scenario
            .add_artifact_from_oracle(P0, "Astral Cornucopia", ASTRAL)
            .id();
        scenario.with_counter(astral, charge(), counters);
        let spell = add_instant(&mut scenario, mana_cost);
        let mut runner = scenario.build();
        assert_astral_reach(&runner, astral);

        assert_eq!(
            priority_facts(&runner, spell),
            (true, false),
            "{counters} counters: own turn offers the cast and holds priority"
        );
        assert_eq!(
            opponent_turn_priority_facts(&mut runner, spell),
            (true, false),
            "{counters} counters: opponent's turn offers the cast and holds priority"
        );

        runner.cast(spell).resolve();
        assert_eq!(
            spell_zone(&runner, spell),
            Zone::Graveyard,
            "{counters} counters: the auto-paid cast resolves"
        );
        assert!(
            runner.state().objects[&astral].tapped,
            "Astral paid the cost"
        );
        let pool = &runner.state().players[0].mana_pool;
        assert_eq!(
            pool.count_color(color),
            (counters - total) as usize,
            "{counters} counters: the excess floats in the color the cost needed"
        );
        assert_eq!(
            pool.total(),
            (counters - total) as usize,
            "one activation, one color"
        );
    }
}

/// CR 106.1 + CR 107.3 + CR 601.2f: the largest X offered counts Astral
/// Cornucopia's full output. Four counters pay {X}{R} with X up to 3.
#[test]
fn astral_cornucopia_counts_its_full_output_toward_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let astral = scenario
        .add_artifact_from_oracle(P0, "Astral Cornucopia", ASTRAL)
        .id();
    scenario.with_counter(astral, charge(), 4);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "X Draw", true, "Draw X cards.")
        .with_mana_cost(cost(vec![ManaCostShard::X, ManaCostShard::Red], 0))
        .id();
    let mut runner = scenario.build();
    assert_astral_reach(&runner, astral);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the X spell can be cast");
    match &runner.state().waiting_for {
        WaitingFor::ChooseXValue { max, .. } => assert_eq!(*max, 3),
        other => panic!("expected the X choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseX { value: 3 })
        .expect("the offered maximum X is payable");
}

/// CR 106.1: Astral Cornucopia with fewer counters than the cost stays
/// uncastable and auto-pass still passes (the fix does not over-count).
#[test]
fn astral_cornucopia_does_not_cover_more_than_its_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let astral = scenario
        .add_artifact_from_oracle(P0, "Astral Cornucopia", ASTRAL)
        .id();
    scenario.with_counter(astral, charge(), 3);
    let spell = add_instant(&mut scenario, cost(vec![ManaCostShard::Red], 3));
    let mut runner = scenario.build();
    assert_astral_reach(&runner, astral);

    assert_eq!(priority_facts(&runner, spell), (false, true));
    assert_eq!(
        opponent_turn_priority_facts(&mut runner, spell),
        (false, true)
    );
}

/// CR 106.1 + CR 700.5 + CR 607.2d: Nykthos's amount depends on the color
/// chosen during activation, so each color is planned with its own devotion.
/// Devotion 3 to green pays {G}{G}{G}; devotion 1 to blue cannot pay {U}{U}.
#[test]
fn nykthos_plans_each_color_with_its_own_devotion() {
    let build = |mana_cost: ManaCost| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let nykthos = scenario
            .add_land_from_oracle(P0, "Nykthos, Shrine to Nyx", NYKTHOS)
            .id();
        scenario.add_basic_land(P0, ManaColor::White);
        scenario.add_basic_land(P0, ManaColor::White);
        scenario
            .add_creature(P0, "Green Devotee", 1, 1)
            .with_mana_cost(cost(
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                ],
                0,
            ));
        scenario
            .add_creature(P0, "Blue Devotee", 1, 1)
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
        let spell = add_instant(&mut scenario, mana_cost);
        (scenario.build(), nykthos, spell)
    };

    let (mut runner, nykthos, spell) = build(cost(
        vec![
            ManaCostShard::Green,
            ManaCostShard::Green,
            ManaCostShard::Green,
        ],
        0,
    ));
    assert!(
        matches!(
            produced_of(&runner, nykthos, 1),
            ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
        ),
        "reach: Nykthos's second ability is a pure chosen-color production"
    );
    assert_eq!(priority_facts(&runner, spell), (true, false));
    assert_eq!(
        opponent_turn_priority_facts(&mut runner, spell),
        (true, false)
    );
    runner.cast(spell).resolve();
    assert_eq!(spell_zone(&runner, spell), Zone::Graveyard);
    assert!(runner.state().objects[&nykthos].tapped);

    let (mut runner, _, spell) = build(cost(vec![ManaCostShard::Blue, ManaCostShard::Blue], 0));
    assert_eq!(
        priority_facts(&runner, spell),
        (false, true),
        "devotion 1 to blue cannot pay {{U}}{{U}}"
    );
    assert_eq!(
        opponent_turn_priority_facts(&mut runner, spell),
        (false, true)
    );
}

/// CR 106.1 + CR 700.5 + CR 607.2d + CR 601.2g: Nyx Lotus has no mana in its
/// activation cost. Alone it is planned through auto-tap; next to a manual
/// sacrifice mana ability (Krark-Clan Ironworks) castability is planned from
/// activatable mana profiles. Each color carries its own devotion either way.
#[test]
fn nyx_lotus_plans_each_color_with_its_own_devotion() {
    let build = |mana_cost: ManaCost, with_ironworks: bool| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let lotus = scenario
            .add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS)
            .id();
        if with_ironworks {
            scenario.add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KCI);
            scenario.add_artifact_from_oracle(P0, "Spare Artifact", "");
        }
        scenario
            .add_creature(P0, "Green Devotee", 1, 1)
            .with_mana_cost(cost(
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                ],
                0,
            ));
        scenario
            .add_creature(P0, "Blue Devotee", 1, 1)
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
        let spell = add_instant(&mut scenario, mana_cost);
        (scenario.build(), lotus, spell)
    };
    let green3_plus_2 = || {
        cost(
            vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            2,
        )
    };
    let blue2_plus_2 = || cost(vec![ManaCostShard::Blue, ManaCostShard::Blue], 2);

    // Auto-tap path.
    let (mut runner, lotus, spell) = build(cost(vec![ManaCostShard::Green], 2), false);
    assert!(matches!(
        produced_of(&runner, lotus, 0),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    assert_eq!(priority_facts(&runner, spell), (true, false));
    assert_eq!(
        opponent_turn_priority_facts(&mut runner, spell),
        (true, false)
    );
    runner.cast(spell).resolve();
    assert_eq!(spell_zone(&runner, spell), Zone::Graveyard);
    assert!(runner.state().objects[&lotus].tapped);

    let (runner, _, spell) = build(cost(vec![ManaCostShard::Blue], 2), false);
    assert_eq!(
        priority_facts(&runner, spell),
        (false, true),
        "devotion 1 to blue cannot pay {{2}}{{U}}"
    );

    // Reach guard for the profile path: without Ironworks, five mana is out
    // of reach, so the castable leg below needs the profile combination.
    let (runner, _, spell) = build(green3_plus_2(), false);
    assert!(!priority_facts(&runner, spell).0);

    // Profile path (Ironworks' sacrifice is a manual mana choice).
    let (mut runner, _, spell) = build(green3_plus_2(), true);
    assert!(
        priority_facts(&runner, spell).0,
        "GGG from Nyx Lotus plus CC from Ironworks pays {{2}}{{G}}{{G}}{{G}}"
    );
    assert!(opponent_turn_priority_facts(&mut runner, spell).0);

    let (runner, _, spell) = build(blue2_plus_2(), true);
    assert!(
        !priority_facts(&runner, spell).0,
        "devotion 1 to blue cannot pay {{U}}{{U}} even with Ironworks"
    );
}

/// Astral Cornucopia with four charge counters, Leatherback Baloth (devotion
/// to green 3) and Harmonize {2}{G}{G} ("Draw three cards.") in P0's hand.
fn astral_harmonize_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Library A",
            "Library B",
            "Library C",
            "Library D",
            "Library E",
        ],
    );
    let astral = scenario
        .add_artifact_from_oracle(P0, "Astral Cornucopia", ASTRAL)
        .id();
    scenario.with_counter(astral, charge(), 4);
    let baloth = scenario
        .add_creature(P0, "Leatherback Baloth", 4, 5)
        .with_mana_cost(cost(
            vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ))
        .id();
    let harmonize = scenario
        .add_spell_to_hand_from_oracle(P0, "Harmonize", false, "Draw three cards.")
        .with_mana_cost(cost(vec![ManaCostShard::Green, ManaCostShard::Green], 2))
        .id();
    (scenario.build(), astral, baloth, harmonize)
}

fn persisted_color_choice() -> Effect {
    Effect::Choose {
        chooser: ControllerRef::You,
        choice_type: ChoiceType::color(),
        persist: true,
        selection: TargetSelectionMode::Chosen,
    }
}

/// Engine-shape fixtures: a persisted color chooser placed elsewhere on Astral
/// Cornucopia, unrelated to its printed mana ability.
#[derive(Clone, Copy, Debug)]
enum UnrelatedChooser {
    /// Inside a `ChooseOneOf` branch of an as-enters replacement.
    ChooseOneOfBranch,
    /// "{0}: You may draw a card. When you do, choose a color." (CR 603.12).
    Reflexive,
    /// An own "{0}: Choose a color" whose ability array a copy payload
    /// installed for the Baloth shares (CR 707.2).
    SharedWithCopyPayload,
}

fn install_unrelated_chooser(
    runner: &mut GameRunner,
    astral: ObjectId,
    baloth: ObjectId,
    shape: UnrelatedChooser,
) {
    let free = || AbilityCost::Mana {
        cost: ManaCost::generic(0),
    };
    // As-enters replacements: they apply only to Cornucopia entering the
    // battlefield, so they never fire in these boards.
    let push_replacement = |runner: &mut GameRunner, execute: AbilityDefinition| {
        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .execute(execute);
        let obj = runner.state_mut().objects.get_mut(&astral).unwrap();
        Arc::make_mut(&mut obj.base_replacement_definitions).push(replacement.clone());
        obj.replacement_definitions.push(replacement);
    };
    match shape {
        UnrelatedChooser::ChooseOneOfBranch => {
            let branch = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![AbilityDefinition::new(
                        AbilityKind::Spell,
                        persisted_color_choice(),
                    )],
                    kind: ChooseOneOfKind::Ordinary,
                },
            );
            push_replacement(runner, branch);
        }
        UnrelatedChooser::Reflexive => {
            let mut reflexive =
                AbilityDefinition::new(AbilityKind::Spell, persisted_color_choice());
            reflexive.condition = Some(AbilityCondition::WhenYouDo);
            let mut draw = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(free());
            draw.optional = true;
            let draw = draw.sub_ability(reflexive);
            let obj = runner.state_mut().objects.get_mut(&astral).unwrap();
            Arc::make_mut(&mut obj.base_abilities).push(draw.clone());
            Arc::make_mut(&mut obj.abilities).push(draw);
        }
        UnrelatedChooser::SharedWithCopyPayload => {
            let chooser = AbilityDefinition::new(AbilityKind::Activated, persisted_color_choice())
                .cost(free());
            {
                let obj = runner.state_mut().objects.get_mut(&astral).unwrap();
                Arc::make_mut(&mut obj.base_abilities).push(chooser);
                obj.abilities = Arc::clone(&obj.base_abilities);
            }
            let values = engine::game::printed_cards::intrinsic_copiable_values(
                &runner.state().objects[&astral],
            );
            let copy_onto_baloth = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GenericEffect {
                    static_abilities: vec![StaticDefinition::continuous()
                        .affected(TargetFilter::SpecificObject { id: baloth })
                        .modifications(vec![ContinuousModification::CopyValues {
                            values: Box::new(values),
                            display: Default::default(),
                        }])],
                    duration: Some(Duration::UntilEndOfTurn),
                    target: None,
                    end_cost: None,
                },
            );
            push_replacement(runner, copy_onto_baloth);
        }
    }
}

/// CR 106.1 + CR 608.2d + CR 607.2d (corrects the r1-r4 fixtures): Astral
/// Cornucopia's printed choice is made as its own ability resolves, so a
/// persisted color chooser elsewhere on the object does not turn it into a
/// reference to that choice. Every shape keeps Harmonize payable, and a blue
/// stored by that chooser does not make the printed ability blue only.
#[test]
fn an_unrelated_color_chooser_does_not_change_an_activation_choice() {
    for shape in [
        UnrelatedChooser::ChooseOneOfBranch,
        UnrelatedChooser::Reflexive,
        UnrelatedChooser::SharedWithCopyPayload,
    ] {
        let (mut runner, astral, baloth, harmonize) = astral_harmonize_board();
        let replacements_before = runner.state().objects[&astral]
            .replacement_definitions
            .iter_unchecked()
            .count();
        install_unrelated_chooser(&mut runner, astral, baloth, shape);
        let obj = &runner.state().objects[&astral];
        let added_abilities = obj.abilities.len() - 1;
        let added_replacements =
            obj.replacement_definitions.iter_unchecked().count() - replacements_before;
        assert_eq!(
            (added_abilities, added_replacements),
            match shape {
                UnrelatedChooser::ChooseOneOfBranch => (0, 1),
                UnrelatedChooser::Reflexive => (1, 0),
                UnrelatedChooser::SharedWithCopyPayload => (1, 1),
            },
            "{shape:?}: reach: the chooser is installed"
        );
        assert_astral_reach(&runner, astral);
        assert_eq!(
            priority_facts(&runner, harmonize),
            (true, false),
            "{shape:?}: Harmonize is offered and auto-pass holds"
        );
        let hand = runner.state().players[0].hand.len();
        runner.cast(harmonize).resolve();
        assert_eq!(spell_zone(&runner, harmonize), Zone::Graveyard, "{shape:?}");
        assert!(runner.state().objects[&astral].tapped, "{shape:?}");
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand - 1 + 3,
            "{shape:?}: Harmonize drew three"
        );
    }

    // The reflexive chooser stores blue on Cornucopia through real actions;
    // the printed ability still chooses green for Harmonize.
    let (mut runner, astral, baloth, harmonize) = astral_harmonize_board();
    install_unrelated_chooser(&mut runner, astral, baloth, UnrelatedChooser::Reflexive);
    runner
        .activate(astral, 1)
        .accept_optional()
        .choose_option("Blue")
        .resolve();
    assert_eq!(
        runner.state().objects[&astral].chosen_color(),
        Some(ManaColor::Blue),
        "reach: the unrelated choice persisted blue on Cornucopia"
    );
    assert_eq!(priority_facts(&runner, harmonize), (true, false));
    runner.cast(harmonize).resolve();
    assert_eq!(spell_zone(&runner, harmonize), Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&astral].chosen_color(),
        Some(ManaColor::Blue),
        "paying with green leaves the stored blue alone"
    );
}

/// Disclosed limitation, not a migration: an object serialized before this
/// change carries the old collapsed form ("Choose a color. Add ..." as a
/// chosen-color production with no linked chooser). It behaves as before the
/// fix: the unset chosen color is undefined, so nothing is planned from it
/// (CR 106.5).
#[test]
fn legacy_collapsed_chosen_color_form_plans_no_mana() {
    let (mut runner, astral, _, harmonize) = astral_harmonize_board();
    assert!(
        priority_facts(&runner, harmonize).0,
        "reach: printed form pays"
    );
    {
        let obj = runner.state_mut().objects.get_mut(&astral).unwrap();
        for abilities in [&mut obj.abilities, &mut obj.base_abilities] {
            let ability = &mut Arc::make_mut(abilities)[0];
            let Effect::Mana { produced, .. } = &mut *ability.effect else {
                panic!("Astral's first ability is its mana ability");
            };
            let ManaProduction::AnyOneColor {
                count,
                contribution,
                ..
            } = produced.clone()
            else {
                panic!("printed form is any one color");
            };
            *produced = ManaProduction::ChosenColor {
                count,
                contribution,
                fixed_alternative: None,
            };
        }
    }
    assert!(matches!(
        produced_of(&runner, astral, 0),
        ManaProduction::ChosenColor {
            fixed_alternative: None,
            ..
        }
    ));
    assert_eq!(priority_facts(&runner, harmonize), (false, true));
}

/// Nyx Lotus (untapped), Leatherback Baloth and Merfolk of the Pearl Trident:
/// devotion G3 / U1, empty pool, precombat main. `stored` puts an unrelated
/// persisted color on the Lotus.
fn nyx_lotus_devotion_board(stored: Option<ManaColor>) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lotus = scenario
        .add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS)
        .id();
    scenario
        .add_creature(P0, "Leatherback Baloth", 4, 5)
        .with_mana_cost(cost(
            vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ));
    scenario
        .add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
    let mut runner = scenario.build();
    if let Some(color) = stored {
        runner
            .state_mut()
            .objects
            .get_mut(&lotus)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(color));
    }
    let state = runner.state();
    assert!(!state.objects[&lotus].tapped);
    assert_eq!(state.players[0].mana_pool.total(), 0);
    assert_eq!(state.objects[&lotus].chosen_color(), stored);
    for (color, devotion) in [(ManaColor::Green, 3), (ManaColor::Blue, 1)] {
        assert_eq!(
            engine::game::devotion::count_devotion(state, P0, &[color]),
            devotion
        );
    }
    assert!(matches!(
        produced_of(&runner, lotus, 0),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    (runner, lotus)
}

/// Activates Nyx Lotus by hand and answers its color prompt with `color`.
fn activate_lotus_choosing(runner: &mut GameRunner, lotus: ObjectId, color: ManaType) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: lotus,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Nyx Lotus can be activated");
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => assert_eq!(options.len(), 5, "any of the five colors"),
        other => panic!("expected the color prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(color),
            count: 1,
        })
        .expect("the chosen color is legal");
}

/// Reviewer probes A and B (#8443 m1, M1-01 / M1-02): activating Nyx Lotus by
/// hand asks for a color even though its amount depends on that color, and
/// green adds GGG (CR 608.2d, CR 700.5). The color chosen for the activation
/// is what its devotion reads: an unrelated blue stored on the Lotus is
/// neither read nor changed, and the same holds for planning.
#[test]
fn nyx_lotus_manual_activation_counts_the_color_chosen_for_it() {
    for stored in [None, Some(ManaColor::Blue)] {
        let (mut runner, lotus) = nyx_lotus_devotion_board(stored);
        activate_lotus_choosing(&mut runner, lotus, ManaType::Green);
        let pool = &runner.state().players[0].mana_pool;
        assert_eq!(pool.count_color(ManaType::Green), 3, "{stored:?}");
        assert_eq!(pool.total(), 3, "{stored:?}");
        assert!(runner.state().objects[&lotus].tapped);
        assert_eq!(
            runner.state().objects[&lotus].chosen_color(),
            stored,
            "the stored color is unchanged"
        );

        let (mut runner, _) = nyx_lotus_devotion_board(stored);
        let spell = green_spell(&mut runner, 3);
        assert_eq!(
            priority_facts(&runner, spell),
            (true, false),
            "{stored:?}: planning reads green devotion for green"
        );
    }

    // Blue adds one: the amount follows the color chosen.
    let (mut runner, lotus) = nyx_lotus_devotion_board(None);
    activate_lotus_choosing(&mut runner, lotus, ManaType::Blue);
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Blue),
        1
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
}

/// CR 608.2d + CR 700.5: The no-override resolver counts the color it produces,
/// regardless of an unrelated stored or global color choice.
#[test]
fn nyx_lotus_default_resolution_counts_its_first_color_option() {
    for stored in [None, Some(ManaColor::Blue)] {
        for (options, expected_color, expected_amount) in [
            (ManaColor::ALL.to_vec(), ManaType::White, 0),
            (vec![ManaColor::Green, ManaColor::Blue], ManaType::Green, 3),
            (vec![], ManaType::White, 0),
        ] {
            let (mut runner, lotus) = nyx_lotus_devotion_board(stored);
            runner.state_mut().last_named_choice = Some(ChoiceValue::Color(ManaColor::Blue));
            assert_eq!(
                engine::game::devotion::count_devotion(runner.state(), P0, &[ManaColor::White]),
                0,
                "white and blue devotion differ; blue devotion is one"
            );
            let mut ability = runner.state().objects[&lotus].abilities[0].clone();
            let Effect::Mana { produced, .. } = ability.effect.as_mut() else {
                panic!("Nyx Lotus must have a mana production");
            };
            let ManaProduction::AnyOneColor { color_options, .. } = produced else {
                panic!("Nyx Lotus must choose its color during resolution");
            };
            *color_options = options;
            let mut events = Vec::new();
            engine::game::mana_abilities::resolve_mana_ability(
                runner.state_mut(),
                lotus,
                P0,
                &ability,
                &mut events,
                None,
            )
            .expect("the direct no-override mana resolver completes");
            let state = runner.state();
            assert!(
                state.objects[&lotus].tapped,
                "the real activation cost was paid"
            );
            assert_eq!(
                state.players[0].mana_pool.count_color(expected_color),
                expected_amount
            );
            assert_eq!(state.players[0].mana_pool.total(), expected_amount);
            assert_eq!(state.objects[&lotus].chosen_color(), stored);
            assert_eq!(
                state.last_named_choice,
                Some(ChoiceValue::Color(ManaColor::Blue))
            );
        }
    }
}

const NYX_PLANE: &str = "Nontoken creatures are enchantments in addition to their other types.\nConstellation — Whenever an enchantment you control enters, you gain 1 life.\nWhenever chaos ensues, choose a color. Add an amount of mana of that color equal to your devotion to that color.";

/// Reviewer probe A, Nyx leg (#8443 m1, M1-01): with Nyx as P0's active plane,
/// rolling chaos on the planar die (CR 901.9b) triggers "choose a color. Add
/// an amount of mana of that color equal to your devotion to that color".
/// The trigger uses the stack (it is not a mana ability, CR 605.1b), asks for
/// a color, and green adds GGG (devotion G3 / U1).
#[test]
fn nyx_chaos_trigger_prompts_and_adds_devotion_to_the_chosen_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Leatherback Baloth", 4, 5)
        .with_mana_cost(cost(
            vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ));
    scenario
        .add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
    let mut runner = scenario.build();
    let nyx = {
        let state = runner.state_mut();
        state.format_config.format = engine::types::format::GameFormat::Planechase;
        let nyx = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(8_444),
            P0,
            "Nyx".to_string(),
            Zone::Command,
        );
        let mut card_types = state.objects[&nyx].card_types.clone();
        card_types.core_types.push(CoreType::Plane);
        let parsed = engine::parser::oracle::parse_oracle_text(
            NYX_PLANE,
            "Nyx",
            &[],
            &["Plane".to_string()],
            &["Theros".to_string()],
        );
        let mut face = CardFace {
            name: "Nyx".to_string(),
            card_type: card_types,
            triggers: parsed.triggers,
            static_abilities: parsed.statics,
            ..Default::default()
        };
        synthesize_planechase(&mut face);
        apply_card_face_to_object(state.objects.get_mut(&nyx).unwrap(), &face);
        state.command_zone.push_back(nyx);
        state.planar_controller = Some(P0);
        nyx
    };
    let chaos_mana = runner.state().objects[&nyx]
        .trigger_definitions
        .iter_unchecked()
        .find_map(
            |entry| match entry.definition.execute.as_deref().map(|d| &*d.effect) {
                Some(Effect::Mana { produced, .. }) => Some(produced.clone()),
                _ => None,
            },
        )
        .expect("reach: Nyx's chaos trigger adds mana");
    assert!(
        matches!(&chaos_mana, ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5),
        "reach: the chaos trigger's color is chosen as it resolves, got {chaos_mana:?}"
    );

    // The planar die is random (CR 901.3a); find a seed that rolls chaos.
    let chaos_seed = (0..512_u64)
        .find(|seed| {
            let mut probe = GameRunner::from_state(runner.state().clone());
            probe.state_mut().rng = engine::types::game_state::GameRng::seed_from_u64(*seed);
            probe.act(GameAction::RollPlanarDie).is_ok_and(|result| {
                result.events.iter().any(|event| {
                    matches!(
                        event,
                        GameEvent::PlanarDieRolled {
                            face: PlanarDieFace::Chaos,
                            ..
                        }
                    )
                })
            })
        })
        .expect("some seed rolls chaos");
    runner.state_mut().rng = engine::types::game_state::GameRng::seed_from_u64(chaos_seed);
    runner
        .act(GameAction::RollPlanarDie)
        .expect("P0 rolls the planar die");
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == nyx),
        "reach: Nyx's chaos trigger is on the stack"
    );
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes resolve the trigger");
    }
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => assert_eq!(options.len(), 5),
        other => panic!("expected Nyx's color prompt, got {other:?}"),
    }
    let green_before = runner.state().players[0]
        .mana_pool
        .count_color(ManaType::Green);
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("green is a legal choice");
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green)
            - green_before,
        3,
        "devotion to green is 3"
    );
}

const MYR_WELDER: &str = "Imprint — {T}: Exile target artifact card from a graveyard.\nThis creature has all activated abilities of all cards exiled with it.";
const HERALDIC_BANNER: &str = "As this artifact enters, choose a color.\nCreatures you control of the chosen color get +1/+0.\n{T}: Add one mana of the chosen color.";

/// Myr Welder, having imprinted `artifact` from P0's graveyard through its
/// real ability, then untapped. With `baloth`, Leatherback Baloth (devotion to
/// green 3, no mana ability) is beside it. Returns the runner, the Welder and
/// the imprinted card.
fn welder_with_imprinted(
    name: &str,
    oracle: &str,
    baloth: bool,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let welder = scenario
        .add_creature_from_oracle(P0, "Myr Welder", 1, 4, MYR_WELDER)
        .id();
    if baloth {
        scenario
            .add_creature(P0, "Leatherback Baloth", 4, 5)
            .with_mana_cost(cost(
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                ],
                0,
            ));
    }
    let artifact = scenario.add_artifact_from_oracle(P0, name, oracle).id();
    let mut runner = scenario.build();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        artifact,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    runner.activate(welder, 0).target_object(artifact).resolve();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&welder).unwrap().tapped = false;
        engine::game::layers::evaluate_layers(state);
    }
    let state = runner.state();
    assert_eq!(
        state.objects[&artifact].zone,
        Zone::Exile,
        "reach: {name} was exiled"
    );
    assert!(
        state
            .exile_links
            .iter()
            .any(|link| link.exiled_id == artifact && link.source_id == welder),
        "reach: {name} was exiled with the Welder"
    );
    let welder_obj = &state.objects[&welder];
    assert!(
        !welder_obj.tapped && !welder_obj.summoning_sick,
        "reach: the Welder can tap"
    );
    assert_eq!(welder_obj.chosen_color(), None, "reach: no stored color");
    assert_eq!(state.players[0].mana_pool.total(), 0, "reach: empty pool");
    let mana_sources: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects[id]
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::Mana { .. }))
        })
        .collect();
    assert_eq!(
        mana_sources,
        vec![welder],
        "reach: the Welder is the only mana source"
    );
    (runner, welder, artifact)
}

/// A {G}... instant with `green` green pips and no other effect.
fn green_spell(runner: &mut GameRunner, green: usize) -> ObjectId {
    let state = runner.state_mut();
    let id = engine::game::zones::create_object(
        state,
        engine::types::identifiers::CardId(8_445),
        P0,
        "Green Spell".to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types
        .core_types
        .push(engine::types::card_type::CoreType::Instant);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = cost(vec![ManaCostShard::Green; green], 0);
    let draw = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 0 },
            target: TargetFilter::Controller,
        },
    );
    Arc::make_mut(&mut obj.abilities).push(draw.clone());
    Arc::make_mut(&mut obj.base_abilities).push(draw);
    id
}

/// CR 607.5a + CR 106.5: Myr Welder gains Heraldic Banner's "{T}: Add one
/// mana of the chosen color" without the linked "As this artifact enters,
/// choose a color", so the choice is undefined and the acquired ability makes
/// no mana: a {G} spell, with the Welder as the only mana source, is not
/// offered. The paired positive is Nyx Lotus's "{T}: Choose a color. Add
/// ...", whose choice is made as the acquired ability resolves: beside
/// Leatherback Baloth the Welder pays the same {G}.
#[test]
fn myr_welder_acquired_linked_choice_is_undefined() {
    // Negative: Heraldic Banner.
    let (mut runner, welder, _) = welder_with_imprinted("Heraldic Banner", HERALDIC_BANNER, false);
    assert!(
        runner.state().objects[&welder]
            .abilities
            .iter()
            .any(|ability| matches!(
                &*ability.effect,
                Effect::Mana {
                    produced: ManaProduction::ChosenColor {
                        fixed_alternative: None,
                        ..
                    },
                    ..
                }
            )),
        "reach: the Welder acquired Banner's chosen-color mana ability"
    );
    let spell = green_spell(&mut runner, 1);
    assert_eq!(
        priority_facts(&runner, spell),
        (false, true),
        "the undefined choice adds no mana (CR 607.5a)"
    );

    // Positive: Nyx Lotus, beside the Baloth.
    let (mut runner, welder, _) = welder_with_imprinted("Nyx Lotus", NYX_LOTUS, true);
    assert!(
        runner.state().objects[&welder]
            .abilities
            .iter()
            .any(|ability| matches!(
                &*ability.effect,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor { color_options, .. },
                    ..
                } if color_options.len() == 5
            )),
        "reach: the Welder acquired Nyx Lotus's activation-choice mana ability"
    );
    let spell = green_spell(&mut runner, 1);
    assert_eq!(priority_facts(&runner, spell), (true, false));
    runner.cast(spell).resolve();
    assert_eq!(spell_zone(&runner, spell), Zone::Graveyard);
    assert!(runner.state().objects[&welder].tapped, "the Welder paid");
}

const THREE_TREE_CITY: &str = "As Three Tree City enters, choose a creature type.\n{T}: Add {C}.\n{2}, {T}: Choose a color. Add an amount of mana of that color equal to the number of creatures you control of the chosen type.";

/// Three Tree City played from hand (choosing `chosen` as it enters, when
/// given) or placed directly with no type chosen, beside `elves` Elves (no
/// mana abilities), one Goblin, two Plains to pay {2}, and a {G}{G}{G} spell
/// only City can pay.
fn three_tree_city_board(chosen: Option<&str>, elves: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..elves {
        scenario
            .add_creature(P0, &format!("Elf {index}"), 1, 1)
            .with_subtypes(vec!["Elf"]);
    }
    scenario
        .add_creature(P0, "Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"]);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::White);
    let city = match chosen {
        Some(_) => scenario
            .add_land_to_hand(P0, "Three Tree City")
            .from_oracle_text(THREE_TREE_CITY)
            .id(),
        None => scenario
            .add_land_from_oracle(P0, "Three Tree City", THREE_TREE_CITY)
            .id(),
    };
    let mut runner = scenario.build();
    if let Some(creature_type) = chosen {
        let card_id = runner.state().objects[&city].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: city,
                card_id,
            })
            .expect("P0 plays Three Tree City");
        runner
            .act(GameAction::ChooseOption {
                choice: creature_type.to_string(),
            })
            .expect("the creature type is chosen as City enters");
    }
    let spell = green_spell(&mut runner, 3);
    let state = runner.state();
    assert_eq!(state.objects[&city].zone, Zone::Battlefield);
    assert!(!state.objects[&city].tapped);
    assert_eq!(
        state.objects[&city].chosen_creature_type(),
        chosen,
        "reach: the chosen type"
    );
    assert!(matches!(
        produced_of(&runner, city, 1),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    let creatures = state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects[id]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature)
        })
        .count();
    assert_eq!(creatures, elves + 1, "reach: the Elves and the Goblin");
    let untapped_plains = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name == "Plains" && !state.objects[id].tapped)
        .count();
    assert_eq!(untapped_plains, 2, "reach: the fuel for {{2}}");
    (runner, city, spell)
}

/// CR 106.1 + CR 608.2d + CR 607.2d: Three Tree City played from hand with Elf
/// chosen, beside three Elves and a Goblin: {2}, {T} adds three mana of one
/// color, so a {G}{G}{G} spell only City can pay is offered, cast and paid.
/// With no type chosen, or with two Elves, it is not offered.
#[test]
fn three_tree_city_pays_with_the_chosen_type_count() {
    let (mut runner, city, spell) = three_tree_city_board(Some("Elf"), 3);
    assert_eq!(priority_facts(&runner, spell), (true, false));
    runner.cast(spell).resolve();
    assert_eq!(spell_zone(&runner, spell), Zone::Graveyard);
    let state = runner.state();
    assert!(state.objects[&city].tapped, "City paid");
    assert!(
        state
            .battlefield
            .iter()
            .filter(|id| state.objects[id].name == "Plains")
            .all(|id| state.objects[id].tapped),
        "both Plains paid City's {{2}}"
    );

    let (runner, _, spell) = three_tree_city_board(None, 3);
    assert_eq!(
        priority_facts(&runner, spell),
        (false, true),
        "no type chosen: no creatures of the chosen type"
    );

    let (runner, _, spell) = three_tree_city_board(Some("Elf"), 2);
    assert_eq!(
        priority_facts(&runner, spell),
        (false, true),
        "two Elves add two mana"
    );
}

/// Reviewer probe board (#8443 r1, R1): Nyx Lotus's per-color amounts must
/// still let surplus mana of the chosen color pay generic. Devotion G3 / U1;
/// Krark-Clan Ironworks makes the payment a manual mana choice, so the
/// activatable-profile path decides. 3G + CC pays {2}{G}{G} with one left
/// over (CR 700.5, CR 106.4, CR 601.2g).
#[test]
fn nyx_lotus_surplus_color_pays_generic_alongside_ironworks() {
    const HARMONIZE: &str = "Draw three cards.";
    let build = |with_ironworks: bool| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
        let lotus = scenario
            .add_artifact_from_oracle(
                P0,
                "Nyx Lotus",
                "Nyx Lotus enters tapped.\n{T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color.",
            )
            .id();
        scenario
            .add_creature(P0, "Leatherback Baloth", 4, 5)
            .with_mana_cost(cost(
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                ],
                0,
            ));
        scenario
            .add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
        let mut ironworks = None;
        let mut ornithopter = None;
        if with_ironworks {
            ironworks = Some(
                scenario
                    .add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KCI)
                    .id(),
            );
            ornithopter = Some(
                scenario
                    .add_creature(P0, "Ornithopter", 0, 2)
                    .as_artifact_creature()
                    .from_oracle_text("Flying")
                    .with_mana_cost(cost(vec![], 0))
                    .id(),
            );
        }
        let harmonize = scenario
            .add_spell_to_hand_from_oracle(P0, "Harmonize", false, HARMONIZE)
            .with_mana_cost(cost(vec![ManaCostShard::Green, ManaCostShard::Green], 2))
            .id();
        (scenario.build(), lotus, ironworks, ornithopter, harmonize)
    };

    // Reach: the Lotus ability is a pure chosen-color production with no
    // recorded color.
    let (runner, lotus, _, _, harmonize) = build(true);
    assert!(matches!(
        produced_of(&runner, lotus, 0),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    assert_eq!(runner.state().objects[&lotus].chosen_color(), None);

    assert_eq!(
        priority_facts(&runner, harmonize),
        (true, false),
        "GGG from Nyx Lotus plus CC from Ironworks pays {{2}}{{G}}{{G}}"
    );

    // Control: without Ironworks, three mana cannot pay four.
    let (runner, _, _, _, harmonize) = build(false);
    assert!(!priority_facts(&runner, harmonize).0);

    // Control: tap the Lotus for green, sacrifice Ornithopter to Ironworks,
    // and the cast resolves with one green left over.
    let (mut runner, lotus, ironworks, ornithopter, harmonize) = build(true);
    runner
        .act(GameAction::ActivateAbility {
            source_id: lotus,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Nyx Lotus activates");
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("choose green");
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        3,
        "devotion to green is 3"
    );
    let _ = ironworks;
    let card_id = runner.state().objects[&harmonize].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: harmonize,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Harmonize is cast with manual payment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner
        .act(GameAction::ActivateAbility {
            source_id: ironworks.unwrap(),
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Ironworks activates during payment");
    runner
        .act(GameAction::SelectCards {
            cards: vec![ornithopter.unwrap()],
        })
        .expect("sacrifice Ornithopter");
    runner
        .act(GameAction::PassPriority)
        .expect("finalize the payment");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "GGG + CC paid {{2}}{{G}}{{G}} with one mana unspent"
    );
    runner.resolve_top();
    assert_eq!(spell_zone(&runner, harmonize), Zone::Graveyard);
    assert_eq!(
        runner.state().players[0].hand.len(),
        3,
        "Harmonize drew three cards"
    );
}

/// Where the Island sits relative to the Lotus. Profile grouping is
/// ObjectId-sorted, so this controls which source is tried first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IslandOrder {
    BeforeLotus,
    AfterLotus,
}

/// Shared Nyx Lotus board (devotion G3 / U1) for the per-color accounting
/// boards, with an optional Island at the given order.
struct LotusBoard {
    runner: GameRunner,
    lotus: ObjectId,
    island: Option<ObjectId>,
    spell: ObjectId,
}

fn lotus_board(
    island_order: Option<IslandOrder>,
    with_ironworks: bool,
    spell_name: &str,
    spell_oracle: &str,
    spell_cost: ManaCost,
    instant: bool,
) -> LotusBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
    let mut island = None;
    if island_order == Some(IslandOrder::BeforeLotus) {
        island = Some(scenario.add_basic_land(P0, ManaColor::Blue));
    }
    let lotus = scenario
        .add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS)
        .id();
    if island_order == Some(IslandOrder::AfterLotus) {
        island = Some(scenario.add_basic_land(P0, ManaColor::Blue));
    }
    scenario
        .add_creature(P0, "Leatherback Baloth", 4, 5)
        .with_mana_cost(cost(
            vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ));
    scenario
        .add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
    if with_ironworks {
        scenario.add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KCI);
        scenario
            .add_creature(P0, "Ornithopter", 0, 2)
            .as_artifact_creature()
            .from_oracle_text("Flying");
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, spell_name, instant, spell_oracle)
        .with_mana_cost(spell_cost)
        .id();
    LotusBoard {
        runner: scenario.build(),
        lotus,
        island,
        spell,
    }
}

/// Reviewer probe board (#8443 r2, R1a): green from Nyx Lotus covers {G} and
/// its two other green pay generic, while the {U} comes from an Island tried
/// after it. GGG + U + CC pays {3}{G}{U} with one to spare (CR 106.4,
/// CR 601.2g, CR 700.5).
#[test]
fn nyx_lotus_covers_part_of_the_colored_shards_and_leaves_the_rest() {
    const URBAN_EVOLUTION: &str = "Draw three cards. You may play an additional land this turn.";
    let urban_evolution = || cost(vec![ManaCostShard::Green, ManaCostShard::Blue], 3);

    let board = lotus_board(
        Some(IslandOrder::AfterLotus),
        true,
        "Urban Evolution",
        URBAN_EVOLUTION,
        urban_evolution(),
        false,
    );
    let island = board.island.unwrap();
    assert!(
        board.lotus < island,
        "reach: the Lotus is grouped before the Island"
    );
    assert!(matches!(
        produced_of(&board.runner, board.lotus, 0),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    assert_eq!(
        board.runner.state().objects[&board.lotus].chosen_color(),
        None
    );
    assert_eq!(
        priority_facts(&board.runner, board.spell),
        (true, false),
        "GGG + U + CC pays {{3}}{{G}}{{U}}"
    );

    // Control: without Ironworks, four mana cannot pay five.
    let board = lotus_board(
        Some(IslandOrder::AfterLotus),
        false,
        "Urban Evolution",
        URBAN_EVOLUTION,
        urban_evolution(),
        false,
    );
    assert!(!priority_facts(&board.runner, board.spell).0);

    // Control: Island created first.
    let board = lotus_board(
        Some(IslandOrder::BeforeLotus),
        true,
        "Urban Evolution",
        URBAN_EVOLUTION,
        urban_evolution(),
        false,
    );
    assert!(board.island.unwrap() < board.lotus);
    assert!(priority_facts(&board.runner, board.spell).0);
}

/// Reviewer probe board (#8443 r2, R1b): choosing blue from Nyx Lotus for a
/// {U} adds one mana, not green's three. Blue gives U + R (Ogre); green gives
/// GGG + R with no U. Treasonous Ogre's "Pay 3 life" works once at 4 life
/// (CR 119.4). Planning must credit the chosen color's amount (CR 700.5,
/// CR 601.2g).
#[test]
fn nyx_lotus_generic_capacity_follows_the_chosen_color() {
    const OGRE: &str = "Dethrone\nPay 3 life: Add {R}.";
    const INSPIRATION: &str = "Target player draws two cards.";
    struct Board {
        runner: GameRunner,
        lotus: ObjectId,
        ogre: ObjectId,
        inspiration: ObjectId,
        divination: ObjectId,
        blue_two: ObjectId,
    }
    let build = |floating_blue: bool| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 4);
        let lotus = scenario
            .add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS)
            .id();
        scenario
            .add_creature(P0, "Leatherback Baloth", 4, 5)
            .with_mana_cost(cost(
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                ],
                0,
            ));
        scenario
            .add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
        let ogre = scenario
            .add_creature(P0, "Treasonous Ogre", 2, 3)
            .from_oracle_text(OGRE)
            .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
            .id();
        if floating_blue {
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
            );
        }
        let inspiration = scenario
            .add_spell_to_hand_from_oracle(P0, "Inspiration", true, INSPIRATION)
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 3))
            .id();
        let divination = scenario
            .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 2))
            .id();
        let blue_two = scenario
            .add_spell_to_hand_from_oracle(P0, "Blue Two", true, "Draw a card.")
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
            .id();
        Board {
            runner: scenario.build(),
            lotus,
            ogre,
            inspiration,
            divination,
            blue_two,
        }
    };
    let offered = |board: &Board, spell: ObjectId| priority_facts(&board.runner, spell).0;

    let board = build(false);
    assert!(matches!(
        produced_of(&board.runner, board.lotus, 0),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    for (color, devotion) in [
        (ManaColor::Green, 3),
        (ManaColor::Blue, 1),
        (ManaColor::Red, 1),
    ] {
        assert_eq!(
            engine::game::devotion::count_devotion(board.runner.state(), P0, &[color]),
            devotion,
            "devotion to {color:?}"
        );
    }
    // Reach: an untargeted {1}{U} is payable only by Lotus blue plus the
    // Ogre's manual pay-life mana, so the activatable-profile path decides.
    assert!(
        offered(&board, board.blue_two),
        "Lotus U + Ogre R pays {{1}}{{U}}"
    );
    // The board verbatim: {3}{U} is out of reach.
    assert!(!offered(&board, board.inspiration));
    // Discriminating twin: an untargeted {2}{U}. Blue from the Lotus credits
    // one mana, so U + R is two of three; crediting green's three would offer it.
    assert!(
        !offered(&board, board.divination),
        "blue from the Lotus plus the Ogre's R is two mana, not four"
    );

    // Control: the Ogre activates once at 4 life and not again at 1.
    let mut board = build(false);
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.ogre,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Ogre activates at 4 life");
    assert_eq!(board.runner.state().players[0].life, 1);
    assert!(
        board
            .runner
            .act(GameAction::ActivateAbility {
                source_id: board.ogre,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "CR 119.4: 3 life cannot be paid at 1"
    );

    // Positive witness: one floating blue pays {U}; GGG + R covers {3}.
    let board = build(true);
    assert!(offered(&board, board.inspiration));
}

/// CR 106.1 + CR 700.5 + CR 601.2g: generic mana from Nyx Lotus uses the
/// color that adds the most (green, devotion 3), so {3} is payable by the
/// Lotus alone and the cast resolves.
#[test]
fn nyx_lotus_pays_generic_with_its_best_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lotus = scenario
        .add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS)
        .id();
    scenario
        .add_creature(P0, "Leatherback Baloth", 4, 5)
        .with_mana_cost(cost(
            vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ));
    scenario
        .add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
    let spell = add_instant(&mut scenario, cost(vec![], 3));
    let mut runner = scenario.build();
    assert!(matches!(
        produced_of(&runner, lotus, 0),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    assert_eq!(priority_facts(&runner, spell), (true, false));
    assert_eq!(
        opponent_turn_priority_facts(&mut runner, spell),
        (true, false)
    );
    runner.cast(spell).resolve();
    assert_eq!(spell_zone(&runner, spell), Zone::Graveyard);
    assert!(runner.state().objects[&lotus].tapped);
}

/// CR 107.3 + CR 601.2f + CR 700.5 (fail closed, #8443 r3 ruling): the {X}
/// ceiling sums mana capacities with no per-color accounting, so a source
/// that chooses its color on activation and adds a different amount per color
/// is not counted. Nyx Lotus (G3 / U1) must spend its blue on a {U}, so for
/// Mind Spring {X}{U}{U} with one Island the ceiling is 0 either way.
#[test]
fn x_ceiling_leaves_out_a_color_dependent_activation_amount() {
    const MIND_SPRING: &str = "Draw X cards.";
    let build = |floating_blue: u32| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
        let lotus = scenario
            .add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS)
            .id();
        scenario
            .add_creature(P0, "Leatherback Baloth", 4, 5)
            .with_mana_cost(cost(
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                ],
                0,
            ));
        scenario
            .add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
            .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
        scenario.add_basic_land(P0, ManaColor::Blue);
        if floating_blue > 0 {
            scenario.with_mana_pool(
                P0,
                (0..floating_blue)
                    .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                    .collect(),
            );
        }
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Mind Spring", false, MIND_SPRING)
            .with_mana_cost(cost(
                vec![ManaCostShard::X, ManaCostShard::Blue, ManaCostShard::Blue],
                0,
            ))
            .id();
        (scenario.build(), lotus, spell)
    };
    let x_ceiling = |runner: &mut GameRunner, spell: ObjectId| {
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("Mind Spring can be cast");
        match &runner.state().waiting_for {
            WaitingFor::ChooseXValue { max, .. } => *max,
            other => panic!("expected the X choice, got {other:?}"),
        }
    };

    let (mut runner, lotus, spell) = build(0);
    assert!(matches!(
        produced_of(&runner, lotus, 0),
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() == 5
    ));
    assert_eq!(runner.state().objects[&lotus].chosen_color(), None);
    for (color, devotion) in [(ManaColor::Green, 3), (ManaColor::Blue, 1)] {
        assert_eq!(
            engine::game::devotion::count_devotion(runner.state(), P0, &[color]),
            devotion
        );
    }
    assert_eq!(x_ceiling(&mut runner, spell), 0);
    runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("X = 0 is payable: blue from the Lotus and the Island");
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == spell),
        "Mind Spring is on the stack, paid"
    );
    assert!(
        runner.state().objects[&lotus].tapped,
        "the Lotus paid {{U}}"
    );
    assert_eq!(
        runner.state().objects[&lotus].zone,
        Zone::Battlefield,
        "reach: the Lotus is still in play"
    );

    // Control: with UU floating the Island alone counts toward X.
    let (mut runner, _, spell) = build(2);
    assert_eq!(x_ceiling(&mut runner, spell), 1);
}

/// CR 106.5 + CR 607.2d control: Sunken Citadel's "the chosen color" is linked
/// to its as-enters choice. Unset, it adds no mana; once a color is chosen it
/// pays that color.
#[test]
fn sunken_citadel_linked_choice_is_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let citadel = scenario
        .add_land_from_oracle(P0, "Sunken Citadel", SUNKEN_CITADEL)
        .id();
    let spell = add_instant(&mut scenario, cost(vec![ManaCostShard::Green], 0));
    let mut runner = scenario.build();

    let obj = &runner.state().objects[&citadel];
    assert!(
        obj.replacement_definitions
            .iter_unchecked()
            .filter_map(|replacement| replacement.execute.as_deref())
            .any(|execute| matches!(
                &*execute.effect,
                Effect::Choose {
                    choice_type: ChoiceType::Color { .. },
                    persist: true,
                    ..
                }
            )),
        "reach: Sunken Citadel carries its linked as-enters color chooser"
    );
    assert!(matches!(
        produced_of(&runner, citadel, 0),
        ManaProduction::ChosenColor {
            fixed_alternative: None,
            ..
        }
    ));

    assert_eq!(
        priority_facts(&runner, spell),
        (false, true),
        "CR 106.5: no linked color chosen, no mana"
    );
    assert_eq!(
        opponent_turn_priority_facts(&mut runner, spell),
        (false, true)
    );

    runner
        .state_mut()
        .objects
        .get_mut(&citadel)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Green));
    assert_eq!(priority_facts(&runner, spell), (true, false));
    runner.cast(spell).resolve();
    assert_eq!(spell_zone(&runner, spell), Zone::Graveyard);
}

/// CR 607.2d fail-closed control: Unknown Event Shores' deck-building choice is
/// a persisted labeled choice of color words. It counts as linked, so with no
/// color recorded the source adds no mana rather than any color.
#[test]
fn unknown_event_shores_deck_building_choice_counts_as_linked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shores = scenario
        .add_land_from_oracle(P0, "Unknown Event Shores", UNKNOWN_EVENT_SHORES)
        .id();
    let spell = add_instant(&mut scenario, cost(vec![ManaCostShard::Green], 0));
    let mut runner = scenario.build();

    let obj = &runner.state().objects[&shores];
    assert!(
        obj.trigger_definitions
            .iter_unchecked()
            .filter_map(|entry| entry.definition.execute.as_deref())
            .any(|execute| matches!(
                &*execute.effect,
                Effect::Choose {
                    choice_type: ChoiceType::Labeled { .. },
                    persist: true,
                    ..
                }
            )),
        "reach: the deck-building choice is a persisted labeled chooser"
    );
    assert_eq!(obj.chosen_color(), None);
    assert!(matches!(
        produced_of(&runner, shores, 0),
        ManaProduction::ChosenColor {
            fixed_alternative: None,
            ..
        }
    ));

    assert_eq!(priority_facts(&runner, spell), (false, true));
    assert_eq!(
        opponent_turn_priority_facts(&mut runner, spell),
        (false, true)
    );
}

/// Fixed-color controls: amount-from-counters (Everflowing Chalice) and a
/// fixed multi-mana source (Sol Ring) keep paying.
#[test]
fn fixed_color_sources_still_pay() {
    for (name, oracle, counters, mana_cost) in [
        ("Everflowing Chalice", CHALICE, 3, cost(vec![], 3)),
        ("Sol Ring", SOL_RING, 0, cost(vec![], 2)),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_artifact_from_oracle(P0, name, oracle).id();
        scenario.with_counter(source, charge(), counters);
        let spell = add_instant(&mut scenario, mana_cost);
        let mut runner = scenario.build();
        assert_eq!(priority_facts(&runner, spell), (true, false), "{name}");
        assert_eq!(
            opponent_turn_priority_facts(&mut runner, spell),
            (true, false),
            "{name}"
        );
        runner.cast(spell).resolve();
        assert_eq!(spell_zone(&runner, spell), Zone::Graveyard, "{name}");
    }
}

/// CR 105.1 + CR 106.5: "Choose a color of a permanent you control / of a card
/// in your graveyard" restricts the color domain to game state the engine does
/// not model. The clause stays unsupported, so neither card is a mana source
/// that could pay any color. The unrestricted "Choose a color. Add one mana of
/// that color." on the same board is the control: it pays the {R} spell.
#[test]
fn color_of_a_game_object_choice_stays_unsupported() {
    const PLAIN_CHOOSER: &str = "{T}: Choose a color. Add one mana of that color.";
    for (name, oracle, supported) in [
        ("Meteor Crater", METEOR_CRATER, false),
        ("Corrupted Grafstone", CORRUPTED_GRAFSTONE, false),
        ("Plain Color Chooser", PLAIN_CHOOSER, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_artifact_from_oracle(P0, name, oracle).id();
        scenario
            .add_creature(P0, "Red Permanent", 1, 1)
            .with_mana_cost(cost(vec![ManaCostShard::Red], 0));
        scenario.with_graveyard(P0, &["Graveyard Card"]);
        let spell = add_instant(&mut scenario, cost(vec![ManaCostShard::Red], 0));
        let runner = scenario.build();

        assert!(
            !runner.state().objects[&source].tapped,
            "{name}: the source is untapped, so only its ability decides"
        );
        let ability = runner.state().objects[&source]
            .abilities
            .last()
            .expect("the tap ability parses")
            .clone();
        assert_eq!(
            !matches!(&*ability.effect, Effect::Unimplemented { .. }),
            supported,
            "{name}: unexpected support for the color choice, got {:?}",
            ability.effect
        );
        assert_eq!(
            priority_facts(&runner, spell).0,
            supported,
            "{name}: the {{R}} spell is offered only from the unrestricted chooser"
        );
    }
}
