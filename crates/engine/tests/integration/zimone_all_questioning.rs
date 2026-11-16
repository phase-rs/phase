//! Zimone, All-Questioning — end to end through the real turn pipeline.
//!
//! Oracle: "At the beginning of your end step, if a land entered the battlefield
//! under your control this turn and you control a prime number of lands, create
//! Primo, the Indivisible, a legendary 0/0 green and blue Fractal creature token,
//! then put that many +1/+1 counters on it."
//!
//! - CR 513.1: the trigger fires as the end step begins.
//! - CR 603.4: the intervening-if is checked when the trigger event occurs and
//!   again on resolution; if false then, the ability does nothing.
//! - CR 608.2i: "a land entered … this turn" looks back; the land need not still
//!   be on the battlefield.
//! - CR 608.2h: "that many" is the land count as the effect is applied.
//! - CR 608.2c: the instructions are followed in the order written — Primo is
//!   created as a 0/0 first, then the counters are put on it (CR 122.1).

use engine::game::coverage::card_face_gaps;
use engine::game::game_object::GameObject;
use engine::game::restrictions::record_battlefield_entry;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, SubAbilityLink, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ZIMONE: &str = "At the beginning of your end step, if a land entered the battlefield under your control this turn and you control a prime number of lands, create Primo, the Indivisible, a legendary 0/0 green and blue Fractal creature token, then put that many +1/+1 counters on it. (2, 3, 5, 7, 11, 13, 17, 19, 23, 29, and 31 are prime numbers.)";

const PRIMO: &str = "Primo, the Indivisible";

const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";

const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";

const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";

const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";

const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

/// The unbuilt board for P0: Zimone, `forests` Forests already in play, and
/// (when `land_in_hand`) one land in hand. Returns the scenario and the hand land.
fn board_scenario(forests: usize, land_in_hand: bool) -> (GameScenario, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zimone, All-Questioning", 1, 1, ZIMONE);
    for _ in 0..forests {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let hand_land = land_in_hand.then(|| scenario.add_land_to_hand(P0, "Wastes").id());
    (scenario, hand_land)
}

/// A board for P0: Zimone, `forests` Forests already in play, and (when
/// `land_in_hand`) one land in hand. Returns the runner and the hand land.
fn board(forests: usize, land_in_hand: bool) -> (GameRunner, Option<ObjectId>) {
    let (scenario, hand_land) = board_scenario(forests, land_in_hand);
    (scenario.build(), hand_land)
}

/// `board_scenario` plus Chatterfang, Squirrel General under P0.
fn chatterfang_board(forests: usize, land_in_hand: bool) -> (GameScenario, Option<ObjectId>) {
    let (mut scenario, hand_land) = board_scenario(forests, land_in_hand);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    (scenario, hand_land)
}

/// Play `land` from hand through the real `PlayLand` action (which records the
/// battlefield entry).
fn play_land(runner: &mut GameRunner, land: ObjectId) {
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("the land is playable");
}

/// Put a land onto `player`'s battlefield with no entry-ledger stamp.
fn put_land(runner: &mut GameRunner, player: PlayerId) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        player,
        "Extra Land".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Every battlefield token named `name`.
fn tokens_named<'a>(runner: &'a GameRunner, name: &str) -> Vec<&'a GameObject> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.name == name)
        .collect()
}

/// Every battlefield token named Primo, the Indivisible.
fn primos(runner: &GameRunner) -> Vec<&GameObject> {
    tokens_named(runner, PRIMO)
}

fn plus_counters(obj: &GameObject) -> u32 {
    obj.counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Advance to the end step. Zimone can attack, so the turn stops at the
/// declare-attackers prompt (CR 508.1); declare no attackers and continue.
fn advance_to_end_step(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("declaring no attackers is legal");
        runner.advance_to_end_step();
    }
    assert_eq!(runner.state().phase, Phase::End);
}

/// Play the hand land, run the end step, drain the stack.
fn play_and_finish_turn(runner: &mut GameRunner, land: Option<ObjectId>) {
    if let Some(land) = land {
        play_land(runner, land);
    }
    advance_to_end_step(runner);
    runner.advance_until_stack_empty();
}

/// The one Primo on the battlefield and its +1/+1 counters.
fn the_primo_counters(runner: &GameRunner) -> u32 {
    let found = primos(runner);
    assert_eq!(found.len(), 1, "exactly one Primo");
    plus_counters(found[0])
}

#[test]
fn zimone_face_is_coverage_supported() {
    let parsed = parse_oracle_text(
        ZIMONE,
        "Zimone, All-Questioning",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    );
    // Reach-guard: the face carries the one end-step trigger, whose token is
    // followed directly by its counter placement (CR 608.2c + CR 122.1).
    assert_eq!(parsed.triggers.len(), 1);
    let execute = parsed.triggers[0].execute.as_deref().expect("trigger body");
    let placement = execute
        .sub_ability
        .as_deref()
        .expect("the counter placement under the token");
    assert!(matches!(&*execute.effect, Effect::Token { .. }));
    assert!(matches!(
        &*placement.effect,
        Effect::PutCounter {
            target: TargetFilter::LastCreated,
            ..
        }
    ));
    assert_eq!(placement.sub_link, SubAbilityLink::ContinuationStep);
    let face = CardFace {
        name: "Zimone, All-Questioning".to_string(),
        card_type: CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Wizard".to_string()],
        },
        oracle_text: Some(ZIMONE.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    };
    assert_eq!(card_face_gaps(&face), Vec::<String>::new());
}

/// 1 Forest + 1 played = 2 lands (prime): Primo is created and then receives
/// two counters.
#[test]
fn two_lands_create_primo_with_two_counters() {
    let (mut runner, land) = board(1, true);
    play_and_finish_turn(&mut runner, land);
    let found = primos(&runner);
    assert_eq!(found.len(), 1, "exactly one Primo");
    let primo = found[0];
    assert!(primo.card_types.supertypes.contains(&Supertype::Legendary));
    assert!(primo.card_types.core_types.contains(&CoreType::Creature));
    assert!(primo.card_types.subtypes.iter().any(|s| s == "Fractal"));
    assert!(primo.color.contains(&ManaColor::Green));
    assert!(primo.color.contains(&ManaColor::Blue));
    // CR 704.5f: with its counters the 0/0 survives state-based actions.
    assert_eq!(plus_counters(primo), 2);
    // With no replacement appending tokens, the created referent is Primo alone.
    assert_eq!(runner.state().last_created_token_ids, vec![primo.id]);
}

/// CR 603.4 (ruling 1): no land entered this turn — no trigger, even with a
/// prime land count. Paired with the same board plus a played land.
#[test]
fn no_land_entered_means_no_primo() {
    let (mut runner, _) = board(3, false);
    play_and_finish_turn(&mut runner, None);
    assert!(primos(&runner).is_empty());

    // Reach-guard: 2 Forests + 1 played = 3 lands (prime).
    let (mut runner, land) = board(2, true);
    play_and_finish_turn(&mut runner, land);
    assert_eq!(the_primo_counters(&runner), 3);
}

/// True primality, not parity or "more than one": 4, 1 and 9 lands make nothing.
#[test]
fn non_prime_land_counts_make_no_primo() {
    for forests in [3, 0, 8] {
        let (mut runner, land) = board(forests, true);
        play_and_finish_turn(&mut runner, land);
        assert!(primos(&runner).is_empty(), "{} lands", forests + 1);
    }
}

/// CR 603.4 + CR 608.2h: the count is read as the effect is applied, and a
/// count that stops being prime while on the stack removes the ability.
#[test]
fn resolution_recheck_and_live_count() {
    // (a) 2 lands at fire time, 3 at resolution → three counters.
    let (mut runner, land) = board(1, true);
    play_land(&mut runner, land.unwrap());
    advance_to_end_step(&mut runner);
    assert_eq!(runner.state().stack.len(), 1, "the trigger is on the stack");
    put_land(&mut runner, P0);
    runner.advance_until_stack_empty();
    assert_eq!(the_primo_counters(&runner), 3);

    // (b) 3 lands at fire time, 4 at resolution → the ability does nothing.
    let (mut runner, land) = board(2, true);
    play_land(&mut runner, land.unwrap());
    advance_to_end_step(&mut runner);
    assert_eq!(runner.state().stack.len(), 1, "the trigger is on the stack");
    put_land(&mut runner, P0);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(primos(&runner).is_empty());
}

/// Ruling 3: primes continue past the reminder list's 31.
#[test]
fn thirty_seven_lands_is_prime() {
    let (mut runner, land) = board(36, true);
    play_and_finish_turn(&mut runner, land);
    assert_eq!(the_primo_counters(&runner), 37);
}

/// One blue mana in P0's pool.
fn one_blue() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])]
}

/// P0's board for `entered_land_need_not_remain`: Zimone, `forests` Forests,
/// a Wastes in hand, and a {U} sorcery with `oracle` in hand. Returns the
/// runner, the Wastes and the sorcery.
fn board_with_removal(forests: usize, oracle: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zimone, All-Questioning", 1, 1, ZIMONE);
    for _ in 0..forests {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let land = scenario.add_land_to_hand(P0, "Wastes").id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Removal", false, oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, one_blue());
    (scenario.build(), land, spell)
}

/// CR 608.2i: the entry still counts after the land has left the battlefield.
/// Both departures go through a real cast, so the zone change runs the
/// production replacement and event pipeline.
#[test]
fn entered_land_need_not_remain() {
    // (a) 2 Forests + 1 played, then the played land is returned to hand: the
    // entry counts and 2 lands remain (prime).
    let (mut runner, land, bounce) =
        board_with_removal(2, "Return target land to its owner's hand.");
    play_land(&mut runner, land);
    let outcome = runner.cast(bounce).target_object(land).resolve();
    // Reach-guard: the land really left the battlefield.
    outcome.assert_zone(&[land], Zone::Hand);
    play_and_finish_turn(&mut runner, None);
    assert_eq!(the_primo_counters(&runner), 2);

    // (b) 0 Forests + 1 played, then the land is destroyed: the entry counts
    // but 0 lands remain (not prime).
    let (mut runner, land, destroy) = board_with_removal(0, "Destroy target land.");
    play_land(&mut runner, land);
    let outcome = runner.cast(destroy).target_object(land).resolve();
    outcome.assert_zone(&[land], Zone::Graveyard);
    play_and_finish_turn(&mut runner, None);
    assert!(primos(&runner).is_empty());
}

/// Every event while the stack drains by passing priority.
fn drain_collecting_events(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..60 {
        if runner.state().stack.is_empty() {
            return events;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority is legal");
        events.extend(result.events);
    }
    panic!("the stack did not empty");
}

/// CR 608.2c + CR 122.1 (D2.4): in one end-step resolution Primo enters the
/// battlefield as a 0/0 and only then receives "that many" counters — its
/// battlefield `ZoneChanged` precedes every `CounterAdded` for it. It survives:
/// state-based actions are not checked mid-resolution (CR 704.4), and by the
/// next check it is a 3/3 (CR 704.5f).
#[test]
fn primo_enters_as_a_0_0_then_receives_that_many_counters() {
    // 2 Forests + 1 played = 3 lands (prime).
    let (mut runner, land) = board(2, true);
    play_land(&mut runner, land.unwrap());
    advance_to_end_step(&mut runner);
    assert_eq!(runner.state().stack.len(), 1, "the trigger is on the stack");
    let events = drain_collecting_events(&mut runner);

    let found = primos(&runner);
    assert_eq!(found.len(), 1, "exactly one Primo");
    let primo = found[0].id;
    let (entry, record) = events
        .iter()
        .enumerate()
        .find_map(|(index, e)| match e {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                record,
                ..
            } if *object_id == primo => Some((index, record)),
            _ => None,
        })
        .expect("reach-guard: Primo's battlefield entry event");
    let first_counter = events
        .iter()
        .position(|e| matches!(e, GameEvent::CounterAdded { object_id, .. } if *object_id == primo))
        .expect("reach-guard: Primo's counter event");
    assert!(
        entry < first_counter,
        "Primo enters before it receives counters (entry {entry}, first counter {first_counter})"
    );
    assert_eq!((record.power, record.base_power), (Some(0), Some(0)));

    assert_eq!(plus_counters(found[0]), 3);
    assert_eq!((found[0].power, found[0].toughness), (Some(3), Some(3)));
}

/// CR 616.1 + CR 704.4: two replacements that both modify the counter
/// placement make P0 choose an order while the placement resolves. At that
/// pause Primo is already on the battlefield as a 0/0 with no counters, and it
/// survives (no state-based actions mid-resolution). Hardened Scales first:
/// (3 + 1) × 2 = 8.
#[test]
fn replacement_order_pause_leaves_a_surviving_0_0_primo() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zimone, All-Questioning", 1, 1, ZIMONE);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let land = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();
    play_land(&mut runner, land);
    advance_to_end_step(&mut runner);

    let mut paused = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let names: Vec<&str> = candidates
                    .iter()
                    .map(|candidate| candidate.source_name.as_str())
                    .collect();
                // Reach-guard: both replacements are offered.
                assert!(
                    names.contains(&"Hardened Scales") && names.contains(&"Branching Evolution"),
                    "{names:?}"
                );
                let found = primos(&runner);
                assert_eq!(found.len(), 1, "Primo is on the battlefield at the pause");
                assert_eq!(plus_counters(found[0]), 0, "and has no counters yet");
                let index = names
                    .iter()
                    .position(|name| *name == "Hardened Scales")
                    .expect("Hardened Scales is offered");
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("choosing a replacement is legal");
                paused = true;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        paused,
        "reach-guard: the replacement-order prompt was observed"
    );
    let found = primos(&runner);
    assert_eq!(found.len(), 1, "Primo survives");
    assert_eq!(plus_counters(found[0]), 8);
    assert_eq!((found[0].power, found[0].toughness), (Some(8), Some(8)));
}

/// "you"/"your": only P0's lands count, and only an entry under P0's control.
#[test]
fn only_your_lands_and_your_entries_count() {
    // (a) P0: 2 Forests + 1 played = 3; P1's two Forests are not counted.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zimone, All-Questioning", 1, 1, ZIMONE);
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let land = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();
    play_and_finish_turn(&mut runner, Some(land));
    assert_eq!(the_primo_counters(&runner), 3);

    // (b) P0: 3 Forests, no land played; a land entered under P1's control.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zimone, All-Questioning", 1, 1, ZIMONE);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();
    let opponent_land = put_land(&mut runner, P1);
    // Stamp the production entry ledger, as `record_zone_change` does.
    record_battlefield_entry(runner.state_mut(), opponent_land);
    play_and_finish_turn(&mut runner, None);
    assert!(primos(&runner).is_empty());
}

/// The one battlefield token named `name`.
fn the_token<'a>(runner: &'a GameRunner, name: &str) -> &'a GameObject {
    let found = tokens_named(runner, name);
    assert_eq!(found.len(), 1, "exactly one {name} token");
    found[0]
}

/// CR 614.1a + CR 608.2c: Chatterfang replaces Primo's creation, so its
/// Squirrel is created by Zimone's own "create" instruction, and "put that many
/// +1/+1 counters on it" is a later instruction of the same effect. Chatterfang's
/// ruling: anything else the creating effect specifies applies to both the
/// original tokens and the Squirrels. 1 Forest + 1 played = 2 lands (prime):
/// Primo becomes a 2/2 and the 1/1 Squirrel a 3/3.
#[test]
fn chatterfang_squirrel_gets_primos_counters() {
    let (scenario, land) = chatterfang_board(1, true);
    let mut runner = scenario.build();
    play_and_finish_turn(&mut runner, land);

    // Reach-guards: the trigger resolved (Primo has its counters) and
    // Chatterfang's replacement applied (exactly one Squirrel).
    let primo = the_token(&runner, PRIMO);
    assert_eq!(plus_counters(primo), 2);
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(
        plus_counters(squirrel),
        2,
        "the Squirrel gets Primo's counters"
    );
    assert_eq!((squirrel.power, squirrel.toughness), (Some(3), Some(3)));
    // The instruction's own token comes first, then the appended one.
    assert_eq!(
        runner.state().last_created_token_ids,
        vec![primo.id, squirrel.id]
    );
}

/// CR 603.4: with Chatterfang out but no land entered this turn (3 lands,
/// prime), Zimone does not trigger: no Primo and no Squirrel. Paired with the
/// same board plus a played land.
#[test]
fn chatterfang_no_trigger_no_squirrel() {
    let (scenario, _) = chatterfang_board(3, false);
    let mut runner = scenario.build();
    play_and_finish_turn(&mut runner, None);
    assert!(primos(&runner).is_empty());
    assert!(tokens_named(&runner, "Squirrel").is_empty());

    // Reach-guard: 2 Forests + 1 played = 3 lands (prime).
    let (scenario, land) = chatterfang_board(2, true);
    let mut runner = scenario.build();
    play_and_finish_turn(&mut runner, land);
    assert_eq!(plus_counters(the_token(&runner, PRIMO)), 3);
    assert_eq!(plus_counters(the_token(&runner, "Squirrel")), 3);
}

/// CR 616.1 + CR 616.1e + CR 616.1f: Chatterfang and Parallel Lives (P0) and
/// Halving Season (P1) all apply to Primo's creation, so P0 orders them; P0
/// applies Parallel Lives first. The created batch is one Primo and one
/// Squirrel. Halving Season's second ability halves the counters P0 (its
/// opponent) puts on each: "that many" (2) becomes 1, on Primo and on the
/// Squirrel alike (Chatterfang's ruling).
#[test]
fn chatterfang_squirrel_gets_counters_after_replacement_choice() {
    let (mut scenario, land) = chatterfang_board(1, true);
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
    let mut runner = scenario.build();
    play_land(&mut runner, land.expect("a land in hand"));
    advance_to_end_step(&mut runner);

    let mut choices = 0;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let index = candidates
                    .iter()
                    .position(|candidate| candidate.description.contains("twice"))
                    .expect("Parallel Lives is a candidate");
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("choosing a replacement is legal");
                choices += 1;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    // Reach-guard: the creation paused for exactly one ordering choice, so the
    // tokens were created on the replacement-choice resume.
    assert_eq!(choices, 1, "one replacement-order choice");
    assert!(runner.state().stack.is_empty());

    let primo = the_token(&runner, PRIMO);
    assert_eq!(
        plus_counters(primo),
        1,
        "Halving Season halves Primo's counters"
    );
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(
        plus_counters(squirrel),
        1,
        "the Squirrel gets Primo's counters"
    );
    assert_eq!(
        runner.state().last_created_token_ids,
        vec![primo.id, squirrel.id]
    );
}
