//! Production regressions for shared parser grammar surfaced by Queen-set cards.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, CastManaObjectScope, CastManaSpentMetric, Effect, QuantityExpr, QuantityRef,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::run_combat;

const QUEEN_OF_DALE: &str = "Whenever an opponent casts their first noncreature spell each turn, you recruit. (Draw a card, then discard a card. If you discarded a nonland card, create a 1/1 white Human Soldier creature token.)";
const ASSIMILATE_ESSENCE: &str = "Counter target creature or battle spell unless its controller pays {4}. If they do, you incubate 2. (Create an Incubator token with two +1/+1 counters on it and \"{2}: Transform this token.\" It transforms into a 0/0 Phyrexian artifact creature.)";
const EXCISE_THE_IMPERFECT: &str = "Exile target nonland permanent. Its controller incubates X, where X is its mana value. (They create an Incubator token with X +1/+1 counters on it and \"{2}: Transform this token.\" It transforms into a 0/0 Phyrexian artifact creature.)";
const NECROGEN_ROTPRIEST: &str = "Toxic 2 (Players dealt combat damage by this creature also get two poison counters.)\nWhenever a creature you control with toxic deals combat damage to a player, that player gets an additional poison counter.\n{1}{B}{G}: Target creature you control with toxic gains deathtouch until end of turn.";
const AWAKENED_INFERNO: &str = "This spell can't be countered.\n[+2]: Each opponent gets an emblem with \"At the beginning of your upkeep, this emblem deals 1 damage to you.\"\n[−3]: Chandra deals 3 damage to each non-Elemental creature.\n[−X]: Chandra deals X damage to target creature or planeswalker. If a permanent dealt damage this way would die this turn, exile it instead.";
const DRESSED_TO_KILL: &str = "[+1]: Add {R}. Chandra deals 1 damage to up to one target player or planeswalker.\n[+1]: Exile the top card of your library. If it's red, you may cast it this turn.\n[−7]: Exile the top five cards of your library. You may cast red spells from among them this turn. You get an emblem with \"Whenever you cast a red spell, this emblem deals X damage to any target, where X is the amount of mana spent to cast that spell.\"";
const SPARK_HUNTER: &str = "At the beginning of combat on your turn, choose up to one target Vehicle you control. Until end of turn, it becomes an artifact creature and gains haste.\n[+2]: You may sacrifice an artifact or discard a card. If you do, draw a card.\n[0]: Create a 3/2 colorless Vehicle artifact token with crew 1.\n[−7]: You get an emblem with \"Whenever an artifact you control enters, this emblem deals 3 damage to any target.\"";
const TORCH_OF_DEFIANCE: &str = "[+1]: Exile the top card of your library. You may cast that card. If you don't, Chandra deals 2 damage to each opponent.\n[+1]: Add {R}{R}.\n[−3]: Chandra deals 4 damage to target creature.\n[−7]: You get an emblem with \"Whenever you cast a spell, this emblem deals 5 damage to any target.\"";
const KOTH_FIRE_OF_RESISTANCE: &str = "[+2]: Search your library for a basic Mountain card, reveal it, put it into your hand, then shuffle.\n[−3]: Koth deals damage to target creature equal to the number of Mountains you control.\n[−7]: You get an emblem with \"Whenever a Mountain you control enters, this emblem deals 4 damage to any target.\"";
const NARSET: &str = "[+1]: You gain 2 life. Add {U}, {R}, or {W}. Spend this mana only to cast a noncreature spell.\n[−2]: Draw a card, then you may discard a card. When you discard a nonland card this way, Narset deals damage equal to that card's mana value to target creature or planeswalker.\n[−6]: You get an emblem with \"Whenever you cast a noncreature spell, this emblem deals 2 damage to any target.\"";

fn ability_has_unimplemented(ability: &engine::types::ability::AbilityDefinition) -> bool {
    matches!(*ability.effect, Effect::Unimplemented { .. })
        || matches!(ability.effect.as_ref(), Effect::CreateEmblem { triggers, .. } if triggers.iter().any(|trigger| trigger.execute.as_deref().is_some_and(ability_has_unimplemented)))
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_has_unimplemented)
}

fn emblem_trigger_damage(
    ability: &engine::types::ability::AbilityDefinition,
) -> Option<(&engine::types::ability::TriggerDefinition, &Effect)> {
    match ability.effect.as_ref() {
        Effect::CreateEmblem { triggers, .. } => triggers.iter().find_map(|trigger| {
            trigger
                .execute
                .as_deref()
                .map(|execute| (trigger, execute.effect.as_ref()))
        }),
        _ => ability
            .sub_ability
            .as_deref()
            .and_then(emblem_trigger_damage),
    }
}

fn put_creature_spell_on_stack(
    runner: &mut GameRunner,
    controller: engine::types::player::PlayerId,
) -> ObjectId {
    let spell = create_object(
        runner.state_mut(),
        CardId(9_101),
        controller,
        "Target spell".to_string(),
        Zone::Stack,
    );
    let target = runner.state_mut().objects.get_mut(&spell).unwrap();
    target.card_types.core_types = vec![CoreType::Creature];
    target.base_card_types = target.card_types.clone();
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(9_101),
            ability: None,
            casting_variant: engine::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

fn activate_emblem(runner: &mut GameRunner, source: ObjectId, amount: i32) -> ObjectId {
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: actual }) if actual == amount))
        .expect("emblem loyalty ability");
    runner.activate(source, index).resolve();
    *runner.state().command_zone.last().expect("created emblem")
}

#[test]
fn shape_all_nine_cards_reach_their_targeted_semantic_family() {
    let cards = [
        (
            "The Queen of Dale",
            QUEEN_OF_DALE,
            vec!["Creature"],
            vec!["Human", "Noble"],
        ),
        (
            "Assimilate Essence",
            ASSIMILATE_ESSENCE,
            vec!["Instant"],
            vec![],
        ),
        (
            "Necrogen Rotpriest",
            NECROGEN_ROTPRIEST,
            vec!["Creature"],
            vec!["Phyrexian", "Zombie", "Cleric"],
        ),
        (
            "Chandra, Awakened Inferno",
            AWAKENED_INFERNO,
            vec!["Planeswalker"],
            vec!["Chandra"],
        ),
        (
            "Chandra, Dressed to Kill",
            DRESSED_TO_KILL,
            vec!["Planeswalker"],
            vec!["Chandra"],
        ),
        (
            "Chandra, Spark Hunter",
            SPARK_HUNTER,
            vec!["Planeswalker"],
            vec!["Chandra"],
        ),
        (
            "Chandra, Torch of Defiance",
            TORCH_OF_DEFIANCE,
            vec!["Planeswalker"],
            vec!["Chandra"],
        ),
        (
            "Koth, Fire of Resistance",
            KOTH_FIRE_OF_RESISTANCE,
            vec!["Planeswalker"],
            vec!["Koth"],
        ),
        (
            "Narset of the Ancient Way",
            NARSET,
            vec!["Planeswalker"],
            vec!["Narset"],
        ),
    ];
    for (name, oracle, types, subtypes) in cards {
        let types = types.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let subtypes = subtypes.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
        assert!(
            parsed
                .abilities
                .iter()
                .all(|ability| !ability_has_unimplemented(ability))
                && parsed.triggers.iter().all(|trigger| trigger
                    .execute
                    .as_deref()
                    .is_none_or(|ability| !ability_has_unimplemented(ability))),
            "{name} must not hide an unsupported printed ability: {:#?}",
            parsed.abilities
        );
    }
    for (name, oracle, subtype, expected_mode, expected_phase, expected_target, expected_amount) in [
        (
            "Chandra, Awakened Inferno",
            AWAKENED_INFERNO,
            "Chandra",
            TriggerMode::Phase,
            Some(Phase::Upkeep),
            TargetFilter::Controller,
            QuantityExpr::Fixed { value: 1 },
        ),
        (
            "Chandra, Dressed to Kill",
            DRESSED_TO_KILL,
            "Chandra",
            TriggerMode::SpellCast,
            None,
            TargetFilter::Any,
            QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::TriggeringSpell,
                    metric: CastManaSpentMetric::Total,
                },
            },
        ),
        (
            "Chandra, Spark Hunter",
            SPARK_HUNTER,
            "Chandra",
            TriggerMode::ChangesZone,
            None,
            TargetFilter::Any,
            QuantityExpr::Fixed { value: 3 },
        ),
        (
            "Chandra, Torch of Defiance",
            TORCH_OF_DEFIANCE,
            "Chandra",
            TriggerMode::SpellCast,
            None,
            TargetFilter::Any,
            QuantityExpr::Fixed { value: 5 },
        ),
        (
            "Koth, Fire of Resistance",
            KOTH_FIRE_OF_RESISTANCE,
            "Koth",
            TriggerMode::ChangesZone,
            None,
            TargetFilter::Any,
            QuantityExpr::Fixed { value: 4 },
        ),
        (
            "Narset of the Ancient Way",
            NARSET,
            "Narset",
            TriggerMode::SpellCast,
            None,
            TargetFilter::Any,
            QuantityExpr::Fixed { value: 2 },
        ),
    ] {
        let parsed = parse_oracle_text(
            oracle,
            name,
            &[],
            &["Planeswalker".to_string()],
            &[subtype.to_string()],
        );
        let (trigger, damage) = parsed
            .abilities
            .iter()
            .find_map(emblem_trigger_damage)
            .unwrap_or_else(|| panic!("{name} must create an emblem damage trigger"));
        assert_eq!(
            &trigger.mode, &expected_mode,
            "{name} emblem must retain its printed trigger mode"
        );
        assert_eq!(
            &trigger.phase, &expected_phase,
            "{name} emblem must retain its printed phase constraint"
        );
        let Effect::DealDamage {
            amount,
            target,
            damage_source,
            ..
        } = damage
        else {
            panic!("{name} emblem trigger must deal damage, got {damage:?}");
        };
        assert_eq!(
            target, &expected_target,
            "{name} emblem damage must retain its printed target"
        );
        assert!(
            damage_source.is_none(),
            "{name} emblem damage must use the resolving emblem as its source"
        );
        assert_eq!(
            amount, &expected_amount,
            "{name} emblem damage must retain its printed amount"
        );
    }
}

#[test]
fn queen_of_dale_recruit_draws_discards_and_mints_token_for_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Queen of Dale", 3, 3, QUEEN_OF_DALE);
    let discarded = scenario.add_spell_to_hand(P0, "Nonland discard", true).id();
    scenario.with_library_top(P0, &["Recruit draw"]);
    let trigger = scenario
        .add_spell_to_hand(P1, "Opponent noncreature", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.cast(trigger).commit();
    runner.resolve_top();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "Recruit must reach its discard choice"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![discarded],
        })
        .expect("P0 selects Recruit discard");
    assert_eq!(
        runner.state().objects[&discarded].zone,
        Zone::Graveyard,
        "P0 must discard the selected nonland"
    );
    let tokens = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token && object.zone == Zone::Battlefield && object.name == "Human Soldier"
        })
        .collect::<Vec<_>>();
    assert_eq!(tokens.len(), 1, "Recruit must make exactly one token");
    assert_eq!(
        tokens[0].controller, P0,
        "the triggering opponent must not receive Recruit's token"
    );
}

#[test]
fn assimilate_essence_paid_unless_creates_controller_incubator_with_two_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P1,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let assimilate = scenario
        .add_spell_to_hand_from_oracle(P0, "Assimilate Essence", true, ASSIMILATE_ESSENCE)
        .id();
    let mut runner = scenario.build();
    let target = put_creature_spell_on_stack(&mut runner, P1);
    runner.cast(assimilate).target_object(target).commit();
    runner.resolve_top();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "P1 must receive the unless-payment choice"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays the unless cost");
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Stack,
        "paid unless must leave target spell on stack"
    );
    let incubators = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token && object.zone == Zone::Battlefield && object.name == "Incubator"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        incubators.len(),
        1,
        "the spell controller must create one Incubator"
    );
    assert_eq!(incubators[0].controller, P0);
    assert_eq!(
        incubators[0]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2)
    );
}

/// Pinned AtomicCards Oracle: Excise's second instruction belongs to the
/// controller of the permanent exiled by the first. CR 608.2c applies the
/// instructions in order; CR 701.53a makes that controller's Incubator with
/// the exiled permanent's mana value in +1/+1 counters.
#[test]
fn excise_the_imperfect_exiled_permanents_controller_incubates_its_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P1, "P1's Four-Mana Permanent", 2, 2)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let excise = scenario
        .add_spell_to_hand_from_oracle(P0, "Excise the Imperfect", true, EXCISE_THE_IMPERFECT)
        .id();
    let mut runner = scenario.build();

    runner.cast(excise).target_object(target).resolve();
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Exile,
        "Excise must exile the chosen nonland permanent before incubating"
    );

    let incubators = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token && object.zone == Zone::Battlefield && object.name == "Incubator"
        })
        .collect::<Vec<_>>();
    assert_eq!(incubators.len(), 1, "exactly one Incubator must be created");
    assert_eq!(
        incubators[0].controller, P1,
        "the exiled permanent's controller, not Excise's caster, incubates"
    );
    assert_eq!(
        incubators[0]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(4),
        "Incubator counters equal the exiled permanent's known mana value"
    );
    assert!(
        incubators
            .iter()
            .all(|incubator| incubator.controller != P0),
        "P0 must not receive an Incubator from P1's permanent"
    );
}

#[test]
fn narset_emblem_is_command_zone_source_of_trigger_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let narset = scenario
        .add_planeswalker_from_oracle(P0, "Narset of the Ancient Way", "Narset", 6, NARSET)
        .as_planeswalker_with_loyalty("Narset", 6)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Noncreature trigger", true)
        .id();
    let mut runner = scenario.build();
    let emblem = activate_emblem(&mut runner, narset, -6);
    let p1_before = runner.life(P1);
    let commit = runner.cast(spell).target_player(P1).commit();
    assert!(
        commit.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { .. }
        ) && entry.source_id == emblem),
        "the command-zone emblem must source its trigger"
    );
    let outcome = commit.resolve();
    assert_eq!(
        outcome.state().players[P1.0 as usize].life,
        p1_before - 2,
        "the emblem trigger must deal two damage to the selected player"
    );
}

#[test]
fn awakened_inferno_opponent_emblem_damages_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chandra = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Chandra, Awakened Inferno",
            "Chandra",
            8,
            AWAKENED_INFERNO,
        )
        .as_planeswalker_with_loyalty("Chandra", 8)
        .id();
    for player in [P0, P1] {
        scenario.with_library_top(player, &["A", "B", "C"]);
    }
    let mut runner = scenario.build();
    let emblem = activate_emblem(&mut runner, chandra, 2);
    assert_eq!(
        runner.state().objects[&emblem].controller,
        P1,
        "Each opponent gets an emblem controlled by that opponent"
    );
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);
    runner.advance_to_upkeep();
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { .. }
        ) && entry.source_id == emblem
            && entry.controller == P1),
        "P1's upkeep trigger must be sourced by P1's emblem"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P1),
        p1_before - 1,
        "the opponent-owned emblem damages its controller"
    );
    assert_eq!(runner.life(P0), p0_before, "the creator takes no damage");
}

#[test]
fn necrogen_rotpriest_combat_damage_adds_one_extra_poison_to_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let necrogen = scenario
        .add_creature_from_oracle(P0, "Necrogen Rotpriest", 1, 3, NECROGEN_ROTPRIEST)
        .id();
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![necrogen], vec![]);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        3,
        "Toxic 2 plus one additional poison counter must equal three"
    );
    assert_eq!(runner.state().players[P0.0 as usize].poison_counters, 0);
}
