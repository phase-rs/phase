//! Regression tests for ability- and life-aware combat decisions: block-time
//! triggers (flanking, bushido), value gang blocks, life priced by the clock,
//! connect-payoff denial, and self-sacrifice timing. Keyword triggers are
//! installed through the engine's own card synthesis, so these tests exercise
//! exactly the trigger shape a real card carries.

use engine::database::synthesis::synthesize_all;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DamageKindFilter, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::combat_ai::choose_blockers;
use crate::combat_triggers::{block_trigger_shifts, connect_trigger_value, PtShift};

const AI: PlayerId = PlayerId(1);
const OPP: PlayerId = PlayerId(0);

fn setup(ai_life: i32) -> GameState {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 4;
    state.active_player = OPP;
    state.players[AI.0 as usize].life = ai_life;
    state
}

fn creature(state: &mut GameState, owner: PlayerId, name: &str, p: i32, t: i32) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(p);
    obj.toughness = Some(t);
    obj.entered_battlefield_turn = Some(1);
    id
}

/// Give `id` a keyword together with the triggers the card database
/// synthesizes for it (flanking, bushido and rampage are triggered abilities).
fn grant_keyword(state: &mut GameState, id: ObjectId, keyword: Keyword) {
    let mut face = CardFace {
        keywords: vec![keyword.clone()],
        ..CardFace::default()
    };
    synthesize_all(&mut face);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.keywords.push(keyword);
    for trigger in face.triggers {
        obj.push_printed_trigger(trigger);
    }
}

/// "Whenever this creature deals combat damage to a player, draw a card."
fn grant_draw_on_connect(state: &mut GameState, id: ObjectId) {
    let draw = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let mut trigger = TriggerDefinition::new(TriggerMode::DamageDone).execute(draw);
    trigger.valid_source = Some(TargetFilter::SelfRef);
    trigger.valid_target = Some(TargetFilter::Player);
    trigger.damage_kind = DamageKindFilter::CombatOnly;
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .push_printed_trigger(trigger);
}

fn blocks_on(assignments: &[(ObjectId, ObjectId)], attacker: ObjectId) -> Vec<ObjectId> {
    assignments
        .iter()
        .filter(|(_, a)| *a == attacker)
        .map(|(b, _)| *b)
        .collect()
}

// --- Block-time trigger projection -----------------------------------------

#[test]
fn flanking_shrinks_each_blocker_without_flanking() {
    let mut state = setup(20);
    let flanker = creature(&mut state, OPP, "Flanker", 2, 2);
    grant_keyword(&mut state, flanker, Keyword::Flanking);
    let a = creature(&mut state, AI, "Bear A", 2, 2);
    let b = creature(&mut state, AI, "Bear B", 2, 2);

    let shifts = block_trigger_shifts(&state, flanker, &[a, b]);
    let shrink = PtShift {
        power: -1,
        toughness: -1,
    };
    assert_eq!(shifts.blockers, vec![shrink, shrink]);
    assert_eq!(shifts.attacker, PtShift::default());

    // CR 702.25a: a blocker that itself has flanking is not affected.
    grant_keyword(&mut state, b, Keyword::Flanking);
    let shifts = block_trigger_shifts(&state, flanker, &[a, b]);
    assert_eq!(shifts.blockers, vec![shrink, PtShift::default()]);
}

#[test]
fn bushido_pumps_both_on_blocking_and_on_becoming_blocked() {
    let mut state = setup(20);
    let samurai = creature(&mut state, OPP, "Samurai", 2, 2);
    grant_keyword(&mut state, samurai, Keyword::Bushido(1));
    let bear = creature(&mut state, AI, "Bear", 2, 2);
    let pump = PtShift {
        power: 1,
        toughness: 1,
    };
    assert_eq!(
        block_trigger_shifts(&state, samurai, &[bear]).attacker,
        pump
    );
    assert_eq!(
        block_trigger_shifts(&state, bear, &[samurai]).blockers,
        vec![pump]
    );
}

#[test]
fn rampage_counts_blockers_beyond_the_first() {
    let mut state = setup(20);
    let rampager = creature(&mut state, OPP, "Rampager", 3, 3);
    grant_keyword(&mut state, rampager, Keyword::Rampage(2));
    let ids: Vec<ObjectId> = (0..3)
        .map(|i| creature(&mut state, AI, &format!("Bear {i}"), 2, 2))
        .collect();
    assert_eq!(
        block_trigger_shifts(&state, rampager, &ids[..1]).attacker,
        PtShift::default()
    );
    assert_eq!(
        block_trigger_shifts(&state, rampager, &ids).attacker,
        PtShift {
            power: 4,
            toughness: 4
        }
    );
}

// --- Blocking decisions ----------------------------------------------------

#[test]
fn does_not_block_a_flanker_with_an_equal_body_that_only_dies() {
    let mut state = setup(20);
    let flanker = creature(&mut state, OPP, "Flanker", 2, 2);
    grant_keyword(&mut state, flanker, Keyword::Flanking);
    creature(&mut state, AI, "Bear", 2, 2);

    let assignments = choose_blockers(&state, AI, &[flanker]);
    assert!(
        assignments.is_empty(),
        "flanking makes the 2/2 blocker a 1/1 that dies without killing: {assignments:?}"
    );
}

#[test]
fn still_trades_with_a_vanilla_attacker_of_the_same_size() {
    let mut state = setup(20);
    let attacker = creature(&mut state, OPP, "Bear", 2, 2);
    let bear = creature(&mut state, AI, "Bear", 2, 2);
    assert_eq!(
        blocks_on(&choose_blockers(&state, AI, &[attacker]), attacker),
        vec![bear]
    );
}

#[test]
fn does_not_double_block_a_flanker_that_kills_both_shrunken_blockers() {
    let mut state = setup(20);
    let flanker = creature(&mut state, OPP, "Flanker", 2, 2);
    grant_keyword(&mut state, flanker, Keyword::Flanking);
    creature(&mut state, AI, "Bear A", 2, 2);
    creature(&mut state, AI, "Bear B", 2, 2);

    let assignments = choose_blockers(&state, AI, &[flanker]);
    assert!(
        assignments.is_empty(),
        "two 1/1s-after-flanking both die to the 2/2: {assignments:?}"
    );
}

#[test]
fn double_blocks_a_bigger_attacker_that_can_only_kill_one_blocker() {
    let mut state = setup(20);
    let ogre = creature(&mut state, OPP, "Ogre", 3, 3);
    let a = creature(&mut state, AI, "Bear A", 2, 2);
    let b = creature(&mut state, AI, "Bear B", 2, 2);

    let mut gang = blocks_on(&choose_blockers(&state, AI, &[ogre]), ogre);
    gang.sort();
    assert_eq!(
        gang,
        vec![a, b],
        "a 3/3 kills one 2/2 at most, so trading one bear for it is a win"
    );
}

#[test]
fn bushido_turns_a_losing_block_into_a_trade() {
    let mut state = setup(20);
    let ogre = creature(&mut state, OPP, "Ogre", 3, 3);
    let samurai = creature(&mut state, AI, "Samurai", 2, 2);
    // Without bushido, a 2/2 in front of a 3/3 just dies.
    assert!(choose_blockers(&state, AI, &[ogre]).is_empty());

    grant_keyword(&mut state, samurai, Keyword::Bushido(1));
    assert_eq!(
        blocks_on(&choose_blockers(&state, AI, &[ogre]), ogre),
        vec![samurai],
        "bushido makes the blocker a 3/3 that trades"
    );
}

#[test]
fn does_not_chump_away_a_creature_to_save_a_little_life_while_racing() {
    // At 9 life against a lone 3/3, and out-powered, the old Race objective
    // chump-blocked anything with power >= 2. Three damage at 9 is not worth a
    // creature.
    let mut state = setup(9);
    let ogre = creature(&mut state, OPP, "Ogre", 3, 3);
    creature(&mut state, AI, "Bear", 2, 2);
    assert!(choose_blockers(&state, AI, &[ogre]).is_empty());
}

#[test]
fn chumps_a_card_drawing_attacker_but_not_an_equal_vanilla_one() {
    let mut state = setup(20);
    let vanilla = creature(&mut state, OPP, "Bear", 2, 2);
    creature(&mut state, AI, "Wall", 0, 1);
    assert!(
        choose_blockers(&state, AI, &[vanilla]).is_empty(),
        "2 damage at 20 life is not worth even a 0/1"
    );

    let mut state = setup(20);
    let thief = creature(&mut state, OPP, "Thief", 2, 2);
    grant_draw_on_connect(&mut state, thief);
    let wall = creature(&mut state, AI, "Wall", 0, 1);
    assert!(connect_trigger_value(state.objects.get(&thief).unwrap()) > 0.0);
    assert_eq!(
        blocks_on(&choose_blockers(&state, AI, &[thief]), thief),
        vec![wall],
        "denying a card is worth a 0/1"
    );
}

#[test]
fn life_near_the_end_of_the_clock_justifies_a_block_it_would_skip_at_twenty() {
    // Against a 3/3 with the AI already out-powered, a 1/1 chump is declined at
    // a healthy total but taken when the 3 damage eats the last turns of life.
    let mut state = setup(20);
    let ogre = creature(&mut state, OPP, "Ogre", 3, 3);
    creature(&mut state, AI, "Token", 1, 1);
    assert!(choose_blockers(&state, AI, &[ogre]).is_empty());

    let mut state = setup(8);
    let ogre = creature(&mut state, OPP, "Ogre", 3, 3);
    creature(&mut state, OPP, "Second Ogre", 3, 3);
    let token = creature(&mut state, AI, "Token", 1, 1);
    assert_eq!(
        blocks_on(&choose_blockers(&state, AI, &[ogre]), ogre),
        vec![token]
    );
}

// --- Self-sacrificing creatures --------------------------------------------

/// "Sacrifice this creature: it deals `amount` damage to any target." — the
/// generic self-sacrifice ping shape (Mogg Fanatic is one of many).
fn grant_sacrifice_ping(state: &mut GameState, id: ObjectId, amount: i32) {
    use engine::types::ability::{AbilityCost, SacrificeCost};
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    );
    ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost::count(
        TargetFilter::SelfRef,
        1,
    )));
    std::sync::Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(ability);
}

#[test]
fn a_self_sacrificing_creature_is_a_cheap_chump_blocker() {
    // A 1/1 whose sacrifice can still kill an opposing 1/1 loses nothing by
    // blocking: once doomed it cashes the ability in before damage. The same
    // 1/1 without the ability is not worth spending to save 2 life at 20.
    let mut state = setup(20);
    let bear = creature(&mut state, OPP, "Bear", 2, 2);
    creature(&mut state, OPP, "Elf", 1, 1);
    let pinger = creature(&mut state, AI, "Pinger", 1, 1);
    assert!(choose_blockers(&state, AI, &[bear]).is_empty());

    grant_sacrifice_ping(&mut state, pinger, 1);
    assert_eq!(
        blocks_on(&choose_blockers(&state, AI, &[bear]), bear),
        vec![pinger]
    );
}

#[test]
fn a_blocked_creature_that_dies_in_combat_is_doomed_and_sacrifices_for_free() {
    use crate::config::PolicyPenalties;
    use crate::policies::self_cost::{self_sacrifice_option_premium, source_is_doomed};
    use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use engine::types::ability::{AbilityCost, SacrificeCost};

    let mut state = setup(20);
    let bear = creature(&mut state, OPP, "Bear", 2, 2);
    let pinger = creature(&mut state, AI, "Pinger", 1, 1);
    grant_sacrifice_ping(&mut state, pinger, 1);
    let cost = AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1));
    let penalties = PolicyPenalties::default();

    // Outside combat the ability is an option worth holding.
    assert!(!source_is_doomed(&state, AI, pinger));
    assert!(self_sacrifice_option_premium(&state, AI, pinger, &cost, &penalties) > 0.0);

    // CR 509.1h + CR 510.1c: blocking the 2/2, the 1/1 dies to combat damage.
    state.phase = engine::types::phase::Phase::DeclareBlockers;
    let mut combat = CombatState::default();
    let mut info = AttackerInfo::new(bear, AttackTarget::Player(AI), AI);
    info.blocked = true;
    combat.attackers.push(info);
    combat.blocker_assignments.insert(bear, vec![pinger]);
    combat.blocker_to_attacker.insert(pinger, vec![bear]);
    state.combat = Some(combat);

    assert!(source_is_doomed(&state, AI, pinger));
    assert_eq!(
        self_sacrifice_option_premium(&state, AI, pinger, &cost, &penalties),
        0.0
    );
    // The attacker survives the pinger's 1 damage, so it is not doomed.
    assert!(!source_is_doomed(&state, OPP, bear));
}

#[test]
fn a_blocks_trigger_aimed_at_its_triggering_object_pumps_the_blocker() {
    // CR 509.1h: a blocker declaration's triggering object is the blocker, so
    // "whenever this blocks, the triggering creature gets +2/+2" pumps the
    // blocker, not the creature it blocks.
    use engine::types::ability::PtValue;
    let mut state = setup(20);
    let attacker = creature(&mut state, OPP, "Bear", 2, 2);
    let guard = creature(&mut state, AI, "Guard", 1, 1);
    let pump = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Pump {
            power: PtValue::Fixed(2),
            toughness: PtValue::Fixed(2),
            target: TargetFilter::TriggeringSource,
        },
    );
    let trigger = TriggerDefinition::new(TriggerMode::Blocks)
        .valid_card(TargetFilter::SelfRef)
        .execute(pump);
    state
        .objects
        .get_mut(&guard)
        .unwrap()
        .push_printed_trigger(trigger);

    let shifts = block_trigger_shifts(&state, attacker, &[guard]);
    assert_eq!(
        shifts.blockers,
        vec![PtShift {
            power: 2,
            toughness: 2
        }]
    );
    assert_eq!(shifts.attacker, PtShift::default());
}

#[test]
fn damage_already_dealt_is_not_counted_again_when_judging_doom() {
    use crate::combat_ai::creature_dies_in_current_combat;
    use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};

    // A 2/2 blocked by a 3/3 dies in the damage still to come.
    let mut state = setup(20);
    state.phase = engine::types::phase::Phase::CombatDamage;
    let bear = creature(&mut state, OPP, "Bear", 2, 2);
    let ogre = creature(&mut state, AI, "Ogre", 3, 3);
    let mut combat = CombatState::default();
    let mut info = AttackerInfo::new(bear, AttackTarget::Player(AI), AI);
    info.blocked = true;
    combat.attackers.push(info);
    combat.blocker_assignments.insert(bear, vec![ogre]);
    combat.blocker_to_attacker.insert(ogre, vec![bear]);
    state.combat = Some(combat);
    assert!(creature_dies_in_current_combat(&state, bear));
    assert!(!creature_dies_in_current_combat(&state, ogre));

    // After regular damage the surviving 3/3 carries 2 marked damage; nothing
    // more is coming, so it must not be judged doomed by re-dealing the 2.
    state.objects.get_mut(&ogre).unwrap().damage_marked = 2;
    state.combat.as_mut().unwrap().regular_damage_done = true;
    assert!(!creature_dies_in_current_combat(&state, ogre));

    // After a first-strike step with no first strikers, only the regular step
    // remains: a 3/3 with 2 marked from elsewhere still dies to the 2/2.
    state.combat.as_mut().unwrap().regular_damage_done = false;
    state.combat.as_mut().unwrap().first_strike_done = true;
    assert!(creature_dies_in_current_combat(&state, ogre));
}

// --- Review regressions ----------------------------------------------------

#[test]
fn a_block_that_already_saves_the_player_is_not_credited_to_the_next_one() {
    // At 8 life a 7/7 and a 1/2 attack for exactly lethal. Chumping the 7/7
    // saves the game; after that the 1/2 only threatens 1 of the remaining 8
    // life, so the second 1/1 is not thrown in front of it (it would die
    // without killing). Pricing every block against the original lethal total
    // credited that survival twice.
    let mut state = setup(8);
    let giant = creature(&mut state, OPP, "Giant", 7, 7);
    let elf = creature(&mut state, OPP, "Elf", 1, 2);
    creature(&mut state, AI, "Token A", 1, 1);
    creature(&mut state, AI, "Token B", 1, 1);

    let assignments = choose_blockers(&state, AI, &[giant, elf]);
    assert_eq!(blocks_on(&assignments, giant).len(), 1, "{assignments:?}");
    assert!(blocks_on(&assignments, elf).is_empty(), "{assignments:?}");
}

/// A connect trigger with `execute` as its effect chain.
fn connect_trigger(execute: AbilityDefinition) -> TriggerDefinition {
    let mut trigger = TriggerDefinition::new(TriggerMode::DamageDone).execute(execute);
    trigger.valid_source = Some(TargetFilter::SelfRef);
    trigger.valid_target = Some(TargetFilter::Player);
    trigger.damage_kind = DamageKindFilter::CombatOnly;
    trigger
}

fn draw_one() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
}

#[test]
fn only_established_positive_connect_payoffs_are_priced() {
    use engine::types::ability::TriggerCondition;
    let mut state = setup(20);
    let thief = creature(&mut state, OPP, "Thief", 2, 2);
    let value_with = |state: &mut GameState, trigger: TriggerDefinition| {
        let obj = state.objects.get_mut(&thief).unwrap();
        obj.trigger_definitions = Default::default();
        obj.push_printed_trigger(trigger);
        connect_trigger_value(state.objects.get(&thief).unwrap())
    };

    assert!(value_with(&mut state, connect_trigger(draw_one())) > 0.0);
    // CR 603.4: an intervening-if the block-time state cannot confirm.
    assert_eq!(
        value_with(
            &mut state,
            connect_trigger(draw_one()).condition(TriggerCondition::LostLife)
        ),
        0.0
    );
    // A harmful payoff for its controller.
    let lose_life = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: None,
        },
    );
    assert_eq!(value_with(&mut state, connect_trigger(lose_life)), 0.0);
    // A chain with an unpriced rider is not priced off its draw.
    let loot = draw_one().sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            filter: None,
            selection: Default::default(),
            unless_filter: None,
        },
    ));
    assert_eq!(value_with(&mut state, connect_trigger(loot)), 0.0);

    // End to end: a conditional draw does not buy a chump.
    let obj = state.objects.get_mut(&thief).unwrap();
    obj.trigger_definitions = Default::default();
    obj.push_printed_trigger(connect_trigger(draw_one()).condition(TriggerCondition::LostLife));
    creature(&mut state, AI, "Wall", 0, 1);
    assert!(choose_blockers(&state, AI, &[thief]).is_empty());
}

fn push_opponent_spell(state: &mut GameState, effect: Effect, target: ObjectId) {
    use engine::types::ability::{ResolvedAbility, TargetRef};
    use engine::types::game_state::{StackEntry, StackEntryKind};
    state.stack.push_back(StackEntry {
        id: ObjectId(900),
        source_id: ObjectId(901),
        controller: OPP,
        kind: StackEntryKind::Spell {
            ability: Some(Box::new(ResolvedAbility::new(
                effect,
                vec![TargetRef::Object(target)],
                ObjectId(901),
                OPP,
            ))),
            card_id: CardId(901),
            casting_variant: Default::default(),
            actual_mana_spent: 0,
        },
    });
}

#[test]
fn only_a_threatening_targeted_effect_dooms_the_source() {
    use crate::policies::self_cost::source_is_doomed;
    use engine::types::ability::PtValue;

    let mut state = setup(20);
    let pinger = creature(&mut state, AI, "Pinger", 1, 1);
    push_opponent_spell(
        &mut state,
        Effect::Pump {
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            target: TargetFilter::Any,
        },
        pinger,
    );
    assert!(
        !source_is_doomed(&state, AI, pinger),
        "a buff dooms nothing"
    );

    let destroy = Effect::Destroy {
        target: TargetFilter::Any,
        cant_regenerate: false,
    };
    state.stack.clear();
    push_opponent_spell(&mut state, destroy.clone(), pinger);
    assert!(source_is_doomed(&state, AI, pinger));

    // CR 702.12b: destroy does not remove an indestructible creature.
    state
        .objects
        .get_mut(&pinger)
        .unwrap()
        .keywords
        .push(Keyword::Indestructible);
    assert!(!source_is_doomed(&state, AI, pinger));
}

#[test]
fn a_choice_of_costs_sacrifices_the_source_only_when_every_payable_branch_does() {
    use crate::config::PolicyPenalties;
    use crate::policies::self_cost::self_sacrifice_option_premium;
    use engine::types::ability::{AbilityCost, SacrificeCost};

    let state = setup(20);
    let mut state = state;
    let pinger = creature(&mut state, AI, "Pinger", 1, 1);
    let penalties = PolicyPenalties::default();
    let choice = |life: i32| AbilityCost::OneOf {
        costs: vec![
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: life },
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    };
    // Paying 2 life keeps the source, so no option is surrendered.
    assert_eq!(
        self_sacrifice_option_premium(&state, AI, pinger, &choice(2), &penalties),
        0.0
    );
    // CR 118.3: 30 life cannot be paid at 20, so the sacrifice is forced.
    assert!(self_sacrifice_option_premium(&state, AI, pinger, &choice(30), &penalties) > 0.0);

    // CR 118.3: likewise a mana branch with no mana to pay it.
    let mana_or_sacrifice = AbilityCost::OneOf {
        costs: vec![
            AbilityCost::Mana {
                cost: engine::types::mana::ManaCost::generic(2),
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    };
    assert!(
        self_sacrifice_option_premium(&state, AI, pinger, &mana_or_sacrifice, &penalties) > 0.0
    );
    for _ in 0..2 {
        let _ = state.add_mana_to_pool(
            AI,
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                pinger,
                false,
                vec![],
            ),
        );
    }
    assert_eq!(
        self_sacrifice_option_premium(&state, AI, pinger, &mana_or_sacrifice, &penalties),
        0.0
    );
}

#[test]
fn salvage_counts_only_legal_targets_and_activations_in_the_combat_window() {
    use crate::config::PolicyPenalties;
    use crate::policies::self_cost::self_sacrifice_salvage_value;
    use engine::types::ability::ActivationRestriction;

    let penalties = PolicyPenalties::default();
    let mut state = setup(20);
    state.phase = engine::types::phase::Phase::DeclareBlockers;
    let elf = creature(&mut state, OPP, "Elf", 1, 1);
    let pinger = creature(&mut state, AI, "Pinger", 1, 1);
    grant_sacrifice_ping(&mut state, pinger, 1);
    assert!(self_sacrifice_salvage_value(&state, AI, pinger, &penalties) > 0.0);

    // CR 702.11b: the only killable creature has hexproof, so the ping cannot
    // target it.
    state
        .objects
        .get_mut(&elf)
        .unwrap()
        .keywords
        .push(Keyword::Hexproof);
    assert_eq!(
        self_sacrifice_salvage_value(&state, AI, pinger, &penalties),
        0.0
    );
    state.objects.get_mut(&elf).unwrap().keywords.clear();

    // CR 602.5: a sorcery-speed sacrifice cannot be activated in the
    // opponent's combat.
    let abilities = &mut state.objects.get_mut(&pinger).unwrap().abilities;
    std::sync::Arc::make_mut(abilities)[0].activation_restrictions =
        vec![ActivationRestriction::AsSorcery];
    assert_eq!(
        self_sacrifice_salvage_value(&state, AI, pinger, &penalties),
        0.0
    );
}
