//! Exact public-Oracle regressions for targets carried by populations and quantities.

use engine::game::ability_utils::{build_target_slots, validate_targets_in_chain};
use engine::game::effects::attach::attach_to;
use engine::game::effects::change_targets::legal_new_targets_for_stack_entry;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::player_has_hexproof;
use engine::types::ability::{Effect, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ShardChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::run_combat;

const P2: PlayerId = PlayerId(2);
// Verbatim MTGJSON AtomicCards 5.3.0+20261004 Oracle text.
const CLONE: &str =
    "For each creature target player controls, create a token that's a copy of that creature.";
const SCOUT: &str = "When this creature enters, you gain 2 life for each black and/or red creature target opponent controls.";
const ZEALOT: &str = "When this creature enters, you gain 1 life for each black and/or red permanent target opponent controls.";
const DAWN: &str = "You gain 2 life for each Mountain target opponent controls.";
const STARLIGHT: &str = "You gain 3 life for each black creature target opponent controls.";
const RIPTIDE: &str = "{1}{U}: This creature's base power becomes equal to target creature's power. (This effect lasts indefinitely.)";
const SPELLSKITE: &str = "{U/P}: Change a target of target spell or ability to this creature. ({U/P} can be paid with either {U} or 2 life.)";
const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
const LEYLINE_OF_SANCTITY: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou have hexproof. (You can't be the target of spells or abilities your opponents control.)";
const CLOAK: &str = "Flash\nEnchant creature\nEnchanted creature has shroud. (It can't be the target of spells or abilities.)";
const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
const AGGRESSION: &str = "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.";
const MOON_GIRL: &str = "Whenever you draw your second card each turn, until end of turn, Moon Girl and Devil Dinosaur's base power and toughness become 6/6 and they gain trample.\nWhenever an artifact you control enters, draw a card. This ability triggers only once each turn.";
const XENAGOS: &str = "Indestructible\nAs long as your devotion to red and green is less than seven, Xenagos isn't a creature.\nAt the beginning of combat on your turn, another target creature you control gains haste and gets +X/+X until end of turn, where X is that creature's power.";
const TEMPEST_CALLER: &str =
    "When this creature enters, tap all creatures target opponent controls.";
const PRODIGAL_PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";
const NATURES_WILL: &str = "Whenever one or more creatures you control deal combat damage to a player, tap all lands that player controls and untap all lands you control.";
const SIGIL_OF_SLEEP: &str = "Enchant creature\nWhenever enchanted creature deals damage to a player, return target creature that player controls to its owner's hand.";

fn cast_announce(runner: &mut GameRunner, card: ObjectId) {
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id: runner.state().objects[&card].card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast announcement");
}

fn choose(runner: &mut GameRunner, target: TargetRef) {
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(target_slots.len(), 1, "one actual announced role");
            assert!(
                target_slots[0].legal_targets.contains(&target),
                "legal intended target"
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(target),
                })
                .expect("choose target");
        }
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert_eq!(target_slots.len(), 1, "one actual triggered role");
            assert!(
                target_slots[0].legal_targets.contains(&target),
                "legal intended trigger target"
            );
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![target],
                })
                .expect("choose ETB target");
        }
        other => panic!("expected target announcement, got {other:?}"),
    }
}

fn priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass to responder");
    }
    panic!("responder never received priority");
}

fn resolve_one(runner: &mut GameRunner) -> Vec<GameEvent> {
    let count = runner.state().stack.len();
    assert!(count > 0, "reach guard: stack entry exists");
    let mut events = Vec::new();
    for _ in 0..8 {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority")
                .events,
        );
        if runner.state().stack.len() != count
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return events;
        }
    }
    panic!("stack failed to progress");
}

fn stack_carrier(runner: &GameRunner, source: ObjectId, target: TargetRef) -> ResolvedAbility {
    let ability = runner
        .state()
        .stack
        .back()
        .expect("announced entry")
        .ability()
        .expect("ability");
    assert_eq!(ability.source_id, source);
    assert_eq!(ability.targets, vec![target]);
    assert_eq!(
        build_target_slots(runner.state(), ability)
            .expect("slot rebuild")
            .len(),
        1
    );
    assert_eq!(
        validate_targets_in_chain(runner.state(), ability).targets,
        ability.targets
    );
    ability.clone()
}

fn pt(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = &runner.state().objects[&id];
    (obj.power.expect("power"), obj.toughness.expect("toughness"))
}

fn population_scenario() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(3, 73);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["One", "Two", "Three", "Four"]);
    }
    scenario.add_creature(P0, "Own", 9, 9);
    scenario.add_creature(P1, "Other one", 7, 7);
    scenario.add_creature(P1, "Other two", 8, 8);
    scenario
        .add_creature(P2, "Black", 2, 4)
        .with_color(vec![ManaColor::Black]);
    scenario
        .add_creature(P2, "Red", 3, 5)
        .with_color(vec![ManaColor::Red]);
    scenario
        .add_creature(P2, "Both", 4, 6)
        .with_color(vec![ManaColor::Black, ManaColor::Red]);
    scenario
        .add_artifact_from_oracle(P2, "Black relic", "")
        .with_color(vec![ManaColor::Black]);
    scenario.add_artifact_from_oracle(P2, "Colorless relic", "");
    for _ in 0..3 {
        scenario.add_basic_land(P2, ManaColor::Red);
    }
    scenario.add_basic_land(P2, ManaColor::Green);
    scenario.add_basic_land(P1, ManaColor::Red);
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    scenario
}

fn add_life_card(
    scenario: &mut GameScenario,
    name: &str,
    oracle: &str,
    creature: bool,
) -> ObjectId {
    if creature {
        scenario
            .add_creature_to_hand_from_oracle(P0, name, 1, 3, oracle)
            .with_mana_cost(ManaCost::zero())
            .id()
    } else {
        scenario
            .add_spell_to_hand_from_oracle(P0, name, false, oracle)
            .with_mana_cost(ManaCost::zero())
            .id()
    }
}

#[test]
fn clone_legion_copies_only_chosen_players_current_creatures() {
    let mut scenario = population_scenario();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Clone Legion", false, CLONE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P2).resolve();
    let mut tokens: Vec<_> = outcome
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.controller == P0)
        .map(|o| (o.name.as_str(), o.power, o.toughness))
        .collect();
    tokens.sort();
    // CR 707.2: copy exactly the chosen population's copiable values.
    assert_eq!(
        tokens,
        vec![
            ("Black", Some(2), Some(4)),
            ("Both", Some(4), Some(6)),
            ("Red", Some(3), Some(5))
        ]
    );
}

#[test]
fn exact_life_carriers_count_opponents_union_population_once() {
    for (name, oracle, creature, expected) in [
        ("Honorable Scout", SCOUT, true, 6),
        ("Kithkin Zealot", ZEALOT, true, 4),
        ("Renewing Dawn", DAWN, false, 6),
        ("Starlight", STARLIGHT, false, 6),
    ] {
        let mut scenario = population_scenario();
        let spell = add_life_card(&mut scenario, name, oracle, creature);
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).target_player(P2).resolve();
        // CR 608.2h: inspect the selected opponent's population at resolution.
        outcome.assert_life_delta(P0, expected);
        outcome.assert_life_delta(P1, 0);
        outcome.assert_life_delta(P2, 0);
    }
}

#[test]
fn derived_player_targets_recheck_hexproof_after_announcement() {
    for (name, oracle, creature) in [
        ("Clone Legion", CLONE, false),
        ("Honorable Scout", SCOUT, true),
        ("Kithkin Zealot", ZEALOT, true),
        ("Renewing Dawn", DAWN, false),
        ("Starlight", STARLIGHT, false),
    ] {
        let mut scenario = population_scenario();
        let spell = add_life_card(&mut scenario, name, oracle, creature);
        // Exercise player-target revalidation with a battlefield static. Lazotep
        // Plating's compound temporary player/permanent grant is a separate gap.
        scenario.add_enchantment_from_oracle(
            P2,
            "Leyline of Anticipation",
            LEYLINE_OF_ANTICIPATION,
        );
        let sanctity = scenario
            .add_spell_to_hand(P2, "Leyline of Sanctity", false)
            .as_enchantment()
            .from_oracle_text(LEYLINE_OF_SANCTITY)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        assert!(!player_has_hexproof(runner.state(), P2));
        cast_announce(&mut runner, spell);
        if creature {
            resolve_one(&mut runner);
        }
        choose(&mut runner, TargetRef::Player(P2));
        let carrier = stack_carrier(&runner, spell, TargetRef::Player(P2));
        assert!(
            matches!(
                carrier.effect,
                Effect::GainLife { .. } | Effect::CopyTokenOf { .. }
            ),
            "public parser reached intended effect for {name}"
        );
        priority_to(&mut runner, P2);
        cast_announce(&mut runner, sanctity);
        resolve_one(&mut runner);
        assert_eq!(runner.state().objects[&sanctity].zone, Zone::Battlefield);
        // CR 702.11c: the resolved response protects its controller as a player.
        assert!(
            player_has_hexproof(runner.state(), P2),
            "response really granted player hexproof"
        );
        assert_eq!(
            runner.state().stack.len(),
            1,
            "original carrier still pending"
        );
        let life = runner.state().players[0].life;
        resolve_one(&mut runner);
        // CR 608.2b: the sole declared player is illegal; the carrier cannot resolve.
        assert_eq!(
            runner.state().players[0].life,
            life,
            "{name} must not gain life"
        );
        assert!(
            !runner
                .state()
                .objects
                .values()
                .any(|o| o.is_token && o.controller == P0),
            "{name} must not copy protected player's creatures"
        );
        assert!(runner.state().stack.is_empty());
    }
}

fn riptide_scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["One", "Two", "Three"]);
    scenario.with_library_top(P1, &["One", "Two", "Three"]);
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let source = scenario
        .add_creature_from_oracle(P0, "Riptide Mangler", 0, 3, RIPTIDE)
        .id();
    let a = scenario.add_creature(P0, "Magnitude A", 5, 6).id();
    let b = scenario.add_creature(P1, "Magnitude B", 2, 7).id();
    (scenario, source, a, b)
}

fn announce_riptide(runner: &mut GameRunner, source: ObjectId, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate exact Riptide");
    choose(runner, TargetRef::Object(target));
    let carrier = stack_carrier(runner, source, TargetRef::Object(target));
    assert!(matches!(carrier.effect, Effect::GenericEffect { .. }));
    assert_eq!(
        carrier.source_incarnation,
        Some(runner.state().objects[&source].incarnation)
    );
}

#[test]
fn riptide_targets_magnitude_but_changes_its_own_base_power() {
    let (scenario, source, a, b) = riptide_scenario();
    let mut runner = scenario.build();
    announce_riptide(&mut runner, source, a);
    resolve_one(&mut runner);
    assert_eq!(pt(&runner, source), (5, 3));
    assert_eq!(pt(&runner, a), (5, 6));
    assert_eq!(pt(&runner, b), (2, 7));
}

#[test]
fn riptide_forced_retarget_uses_magnitude_slot_legality() {
    for shrouded in [false, true] {
        let (mut scenario, source, a, _) = riptide_scenario();
        let mut spellskite = scenario.add_creature_from_oracle(P0, "Spellskite", 0, 4, SPELLSKITE);
        if shrouded {
            spellskite.with_keyword(Keyword::Shroud);
        }
        let spellskite = spellskite.id();
        let mut runner = scenario.build();
        announce_riptide(&mut runner, source, a);
        let stack_id = runner.state().stack.back().unwrap().id;
        assert_eq!(pt(&runner, a), (5, 6));
        assert_eq!(pt(&runner, spellskite), (0, 4));
        let legal_targets = &build_target_slots(
            runner.state(),
            runner.state().stack.back().unwrap().ability().unwrap(),
        )
        .expect("Riptide magnitude slot")[0]
            .legal_targets;
        assert!(legal_targets.contains(&TargetRef::Object(a)));
        assert_eq!(
            legal_targets.contains(&TargetRef::Object(spellskite)),
            !shrouded
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: spellskite,
                ability_index: 0,
            })
            .expect("activate exact Spellskite");
        let WaitingFor::PhyrexianPayment { player, shards, .. } = &runner.state().waiting_for
        else {
            panic!("Spellskite must ask how to pay its Phyrexian mana cost")
        };
        assert_eq!(*player, P0);
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].color, ManaColor::Blue);
        assert_eq!(
            runner
                .state()
                .pending_cast
                .as_ref()
                .unwrap()
                .ability
                .targets,
            vec![TargetRef::Object(stack_id)],
            "the only legal stack target is selected before payment"
        );
        // CR 107.4f + CR 602.2b: pay blue mana after selecting the sole legal target.
        runner
            .act(GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayMana],
            })
            .expect("pay Spellskite's activation with blue mana");
        assert_eq!(runner.state().stack.len(), 2);
        assert_eq!(runner.state().stack.back().unwrap().source_id, spellskite);
        assert_eq!(
            runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .targets,
            vec![TargetRef::Object(stack_id)],
            "payment preserves the already selected target"
        );
        assert!(matches!(
            runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .effect,
            Effect::ChangeTargets {
                forced_to: Some(_),
                ..
            }
        ));
        resolve_one(&mut runner);
        assert_eq!(runner.state().stack.len(), 1);
        let expected_target = if shrouded { a } else { spellskite };
        // CR 115.7a/b: change the magnitude target only when the new creature
        // is legal for that exposed slot; a failed change leaves it unchanged.
        stack_carrier(&runner, source, TargetRef::Object(expected_target));
        resolve_one(&mut runner);
        assert_eq!(pt(&runner, source), (if shrouded { 5 } else { 0 }, 3));
        assert_eq!(pt(&runner, a), (5, 6));
        assert_eq!(pt(&runner, spellskite), (0, 4));
    }
}

#[test]
fn riptide_rechecks_shroud_on_live_same_incarnation_magnitude() {
    let (mut scenario, source, a, _) = riptide_scenario();
    let cloak = scenario
        .add_spell_to_hand_from_oracle(P0, "Alexi's Cloak", true, "")
        .as_enchantment()
        // CR 303.4a: the Aura subtype makes its enchant ability declare a target.
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Flash", "Enchant"], CLOAK)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let incarnation = runner.state().objects[&a].incarnation;
    announce_riptide(&mut runner, source, a);
    cast_announce(&mut runner, cloak);
    choose(&mut runner, TargetRef::Object(a));
    resolve_one(&mut runner);
    assert!(runner.state().objects[&a]
        .keywords
        .contains(&Keyword::Shroud));
    assert_eq!(runner.state().objects[&a].incarnation, incarnation);
    assert_eq!(runner.state().objects[&a].zone, Zone::Battlefield);
    resolve_one(&mut runner);
    // CR 608.2b: shroud invalidates the only announced target without any zone move.
    assert_eq!(pt(&runner, source), (0, 3));
}

#[test]
fn riptide_source_blink_before_resolution_does_not_bind_returned_object() {
    let (mut scenario, source, a, _) = riptide_scenario();
    let blink = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let incarnation = runner.state().objects[&source].incarnation;
    announce_riptide(&mut runner, source, a);
    cast_announce(&mut runner, blink);
    choose(&mut runner, TargetRef::Object(source));
    resolve_one(&mut runner);
    assert_ne!(runner.state().objects[&source].incarnation, incarnation);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(pt(&runner, a), (5, 6));
    assert_eq!(
        validate_targets_in_chain(
            runner.state(),
            runner.state().stack.back().unwrap().ability().unwrap()
        )
        .targets,
        vec![TargetRef::Object(a)]
    );
    resolve_one(&mut runner);
    // CR 400.7 + CR 113.7a: ability resolves, but cannot modify the new source object.
    assert_eq!(pt(&runner, source), (0, 3));
}

fn cross_cleanup(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    for _ in 0..100 {
        if runner.state().turn_number != turn && runner.state().phase == Phase::PreCombatMain {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::DiscardToHandSize { count, cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            _ => GameAction::PassPriority,
        };
        runner
            .act(action)
            .expect("advance through cleanup using apply");
    }
    panic!("next main phase was not reached");
}

#[test]
fn riptide_snapshots_independent_sources_and_survives_cleanup() {
    let (mut scenario, source, a, b) = riptide_scenario();
    let second = scenario
        .add_creature_from_oracle(P0, "Riptide Mangler", 0, 3, RIPTIDE)
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Giant Growth",
            true,
            "Target creature gets +3/+3 until end of turn.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    announce_riptide(&mut runner, source, a);
    resolve_one(&mut runner);
    announce_riptide(&mut runner, second, b);
    resolve_one(&mut runner);
    runner.cast(growth).target_object(a).resolve();
    assert_eq!(pt(&runner, a), (8, 9));
    // CR 608.2h + CR 611.2d: each value is read once from its own target.
    assert_eq!(pt(&runner, source), (5, 3));
    assert_eq!(pt(&runner, second), (2, 3));
    cross_cleanup(&mut runner);
    // CR 611.2a: absent a duration, the set effect lasts indefinitely.
    assert_eq!(pt(&runner, source), (5, 3));
    assert_eq!(pt(&runner, second), (2, 3));
    assert_eq!(pt(&runner, a), (5, 6));
}

#[test]
fn riptide_source_blink_after_resolution_removes_existing_set_effect() {
    let (mut scenario, source, a, _) = riptide_scenario();
    let blink = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    announce_riptide(&mut runner, source, a);
    resolve_one(&mut runner);
    assert_eq!(pt(&runner, source), (5, 3));
    let incarnation = runner.state().objects[&source].incarnation;
    runner.cast(blink).target_object(source).resolve();
    // CR 400.7: already installed effects do not follow the new object either.
    assert_ne!(runner.state().objects[&source].incarnation, incarnation);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(pt(&runner, source), (0, 3));
}

#[test]
fn riptide_source_control_change_preserves_its_incarnation_binding() {
    let (mut scenario, source, a, _) = riptide_scenario();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Aggression", true, AGGRESSION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let incarnation = runner.state().objects[&source].incarnation;
    announce_riptide(&mut runner, source, a);
    priority_to(&mut runner, P1);
    cast_announce(&mut runner, steal);
    choose(&mut runner, TargetRef::Object(source));
    resolve_one(&mut runner);
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert_eq!(runner.state().objects[&source].owner, P0);
    assert_eq!(runner.state().objects[&source].incarnation, incarnation);
    resolve_one(&mut runner);
    assert_eq!(pt(&runner, source), (5, 3));
}

#[test]
fn stale_self_definition_does_not_skip_independent_definition_or_subchain() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Duration, ObjectScope,
        QuantityExpr, QuantityRef, StaticDefinition, TargetFilter,
    };
    for blink_before in [false, true] {
        let (mut scenario, _, a, b) = riptide_scenario();
        // Deliberately synthetic composition: three independent instructions.
        // GenericEffect.target is None so the SpecificObject affected definition
        // is independent of the first SelfRef definition's recipient.
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(vec![ContinuousModification::SetPowerDynamic {
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: ObjectScope::Target,
                                },
                            },
                        }]),
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SpecificObject { id: b })
                        .modifications(vec![ContinuousModification::AddPower { value: 4 }]),
                ],
                duration: Some(Duration::Permanent),
                target: None,
                end_cost: None,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        ));
        let source = scenario
            .add_creature(P0, "Synthetic multiple definitions", 0, 3)
            .with_ability_definition(ability)
            .id();
        let blink = scenario
            .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let incarnation = runner.state().objects[&source].incarnation;
        announce_riptide(&mut runner, source, a);
        if blink_before {
            cast_announce(&mut runner, blink);
            choose(&mut runner, TargetRef::Object(source));
            resolve_one(&mut runner);
            assert_ne!(runner.state().objects[&source].incarnation, incarnation);
        }
        resolve_one(&mut runner);
        // CR 113.7a: independent instructions survive a missing source recipient.
        assert_eq!(pt(&runner, source), (if blink_before { 0 } else { 5 }, 3));
        assert_eq!(pt(&runner, b), (6, 7));
        assert_eq!(runner.state().players[0].life, 23);
    }
}

#[test]
fn exact_moon_girl_outer_duration_overrides_base_pt_default() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four"]);
    scenario.with_library_top(P1, &["One", "Two", "Three"]);
    let moon = scenario
        .add_creature_from_oracle(P0, "Moon Girl and Devil Dinosaur", 2, 2, MOON_GIRL)
        .id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(divination).resolve().assert_hand_drawn(P0, 2);
    assert_eq!(pt(&runner, moon), (6, 6));
    assert!(runner.state().objects[&moon]
        .keywords
        .contains(&Keyword::Trample));
    cross_cleanup(&mut runner);
    // CR 514.2: the explicit outer duration still ends at cleanup.
    assert_eq!(pt(&runner, moon), (2, 2));
    assert!(!runner.state().objects[&moon]
        .keywords
        .contains(&Keyword::Trample));
}

#[test]
fn exact_xenagos_uses_one_primary_target_and_expires_at_cleanup() {
    for power in [3, 5] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["One", "Two"]);
        scenario.with_library_top(P1, &["One", "Two"]);
        let xenagos = scenario
            .add_creature(P0, "Xenagos, God of Revels", 6, 5)
            .from_oracle_text_with_keywords(&["Indestructible"], XENAGOS)
            .id();
        let chosen = scenario.add_creature(P0, "Chosen", power, power).id();
        scenario.add_creature(P0, "Other legal choice", 7, 7);
        let opponent = scenario.add_creature(P1, "Opponent", 9, 9).id();
        let mut runner = scenario.build();
        runner.pass_both_players();
        let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for
        else {
            panic!("Xenagos trigger must announce target")
        };
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(chosen)));
        assert!(!target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(opponent)));
        choose(&mut runner, TargetRef::Object(chosen));
        stack_carrier(&runner, xenagos, TargetRef::Object(chosen));
        resolve_one(&mut runner);
        assert_eq!(pt(&runner, chosen), (2 * power, 2 * power));
        assert!(runner.state().objects[&chosen]
            .keywords
            .contains(&Keyword::Haste));
        cross_cleanup(&mut runner);
        assert_eq!(pt(&runner, chosen), (power, power));
        assert!(!runner.state().objects[&chosen]
            .keywords
            .contains(&Keyword::Haste));
    }
}

#[test]
fn opponent_population_target_rejects_self_and_mandatory_decline() {
    for (name, oracle, creature) in [
        ("Honorable Scout", SCOUT, true),
        ("Kithkin Zealot", ZEALOT, true),
        ("Renewing Dawn", DAWN, false),
        ("Starlight", STARLIGHT, false),
    ] {
        let mut scenario = population_scenario();
        let spell = add_life_card(&mut scenario, name, oracle, creature);
        let mut runner = scenario.build();
        cast_announce(&mut runner, spell);
        if creature {
            resolve_one(&mut runner);
        }
        let slots = match &runner.state().waiting_for {
            WaitingFor::TargetSelection { target_slots, .. }
            | WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots,
            other => panic!("expected announced {name} target, got {other:?}"),
        };
        assert_eq!(slots.len(), 1);
        assert!(slots[0].legal_targets.contains(&TargetRef::Player(P2)));
        assert!(!slots[0].legal_targets.contains(&TargetRef::Player(P0)));
        let invalid = if creature {
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P0)],
            }
        } else {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P0)),
            }
        };
        assert!(runner.act(invalid).is_err());
        assert!(runner
            .act(GameAction::ChooseTarget { target: None })
            .is_err());
        choose(&mut runner, TargetRef::Player(P2));
        resolve_one(&mut runner);
        assert!(
            runner.state().players[0].life > 20,
            "legal same-fixture control"
        );
    }
}

#[test]
fn derived_player_target_is_invalid_after_production_concession() {
    for (name, oracle) in [
        ("Clone Legion", CLONE),
        ("Renewing Dawn", DAWN),
        ("Starlight", STARLIGHT),
    ] {
        let mut scenario = population_scenario();
        let spell = add_life_card(&mut scenario, name, oracle, false);
        let mut runner = scenario.build();
        cast_announce(&mut runner, spell);
        choose(&mut runner, TargetRef::Player(P2));
        let carrier = stack_carrier(&runner, spell, TargetRef::Player(P2));
        runner
            .act(GameAction::Concede { player_id: P2 })
            .expect("production elimination");
        assert!(runner.state().players[2].is_eliminated);
        assert!(!runner.state().players[1].is_eliminated);
        assert!(validate_targets_in_chain(runner.state(), &carrier)
            .targets
            .is_empty());
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().players[0].life, 20);
        assert!(!runner
            .state()
            .objects
            .values()
            .any(|o| o.is_token && o.controller == P0));
    }
}

#[test]
fn companion_player_recheck_ignores_a_later_triggers_damage_batch() {
    let mut scenario = population_scenario();
    let caller = add_life_card(&mut scenario, "Tempest Caller", TEMPEST_CALLER, true);
    let pyromancer = scenario
        .add_creature_from_oracle(P0, "Prodigal Pyromancer", 1, 1, PRODIGAL_PYROMANCER)
        .id();
    let sigil = scenario
        .add_creature(P0, "Sigil of Sleep", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(SIGIL_OF_SLEEP)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), sigil, pyromancer);
    evaluate_layers(runner.state_mut());
    let p2_creatures: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P2 && o.power.is_some() && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .collect();
    assert_eq!(
        p2_creatures.len(),
        3,
        "reach guard: P2 controls three creatures"
    );

    cast_announce(&mut runner, caller);
    resolve_one(&mut runner);
    choose(&mut runner, TargetRef::Player(P2));
    stack_carrier(&runner, caller, TargetRef::Player(P2));

    // Respond: the Sigil-enchanted Pyromancer damages P1, whose Sigil trigger
    // pauses for its creature choice while Tempest Caller's trigger waits below.
    runner
        .act(GameAction::ActivateAbility {
            source_id: pyromancer,
            ability_index: 0,
        })
        .expect("activate Pyromancer");
    choose(&mut runner, TargetRef::Player(P1));
    resolve_one(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            let chosen = target_slots[0].legal_targets[0].clone();
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![chosen],
                })
                .expect("choose Sigil target");
        }
        other => panic!("Sigil trigger must pause for its target, got {other:?}"),
    }
    assert_eq!(runner.state().stack.len(), 2, "Sigil above Tempest Caller");
    resolve_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Tempest Caller still pending"
    );

    // CR 608.2b: P2 is still a legal opponent; another trigger's damage to P1
    // is not this trigger's event and must not narrow its declared target.
    resolve_one(&mut runner);
    for creature in p2_creatures {
        assert!(
            runner.state().objects[&creature].tapped,
            "Tempest Caller must tap every creature P2 controls"
        );
    }
}

#[test]
fn retarget_keeps_damaged_player_binding_from_entry_trigger_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Nature's Will", NATURES_WILL);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    for player in [P0, P1] {
        scenario.add_basic_land(player, ManaColor::Green);
    }
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![]);
    choose(&mut runner, TargetRef::Player(P1));

    let index = runner.state().stack.len() - 1;
    let entry = &runner.state().stack[index];
    assert_eq!(
        entry.ability().map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Player(P1)]),
        "reach guard: Nature's Will is on the stack bound to P1"
    );
    assert!(
        !runner
            .state()
            .stack_trigger_event_batches
            .contains_key(&entry.id),
        "reach guard: a single triggering event stays on the entry"
    );

    // CR 115.7a: "that player" is the damaged player, so retargeting the
    // trigger offers only P1, read from the entry's own triggering event.
    assert_eq!(
        legal_new_targets_for_stack_entry(runner.state(), index),
        vec![TargetRef::Player(P1)]
    );
}
