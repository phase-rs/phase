//! CR 603.7a + CR 508.4: "Return it to the battlefield … tapped and attacking at
//! the beginning of <a declare attackers step>" is a delayed trigger that waits
//! for the named step and returns the card attacking (issue #9630), and a
//! delayed return after a self-exile finds only the card the exile moved.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MEANDERING_TOWERSHELL: &str = "Islandwalk (This creature can't be blocked as long as defending player controls an Island.)\nWhenever this creature attacks, exile it. Return it to the battlefield under your control tapped and attacking at the beginning of the declare attackers step on your next turn.";
const NOCTIS: &str = "Whenever a creature you control enters during combat, you may attach target Equipment you control to target creature you control.\nWarp-Strike \u{2014} {3}: Exile Noctis. Return it to the battlefield under its owner's control tapped and attacking at the beginning of that player's next declare attackers step. It can't be blocked that combat.";
const AETHERLING: &str = "{U}: Exile this creature. Return it to the battlefield under its owner's control at the beginning of the next end step.\n{U}: This creature can't be blocked this turn.\n{1}: This creature gets +1/-1 until end of turn.\n{1}: This creature gets -1/+1 until end of turn.";
const SWORDS_TO_PLOWSHARES: &str =
    "Exile target creature. Its controller gains life equal to its power.";
const MURDER: &str = "Destroy target creature.";
const RECKLESS_IMPULSE: &str = "Exile the top two cards of your library. Until the end of your next turn, you may play those cards.";
const TORMODS_CRYPT: &str = "{T}, Sacrifice this artifact: Exile target player's graveyard.";
const RECONNAISSANCE: &str = "{0}: Remove target attacking creature you control from combat and untap it. (If you activate during end of combat, the creature will untap after it deals combat damage.)";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 10]);
    scenario.with_library_top(P1, &["Forest"; 10]);
    scenario
}

/// Answer the one prompt in front of the game the way a player with no
/// further choices would: no attackers, no blockers, pass priority. Returns
/// the events the answer emitted.
fn step(runner: &mut GameRunner) -> Vec<GameEvent> {
    let action = match runner.state().waiting_for.clone() {
        WaitingFor::OrderTriggers { .. } => {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            return Vec::new();
        }
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::Priority { .. } => GameAction::PassPriority,
        other => panic!("unexpected prompt {other:?}"),
    };
    runner
        .act(action)
        .expect("the no-choice answer is legal")
        .events
}

fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..600 {
        if done(runner) {
            return;
        }
        step(runner);
    }
    panic!(
        "never reached the awaited state; turn {} phase {:?} waiting {:?}",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().waiting_for
    );
}

fn drain_stack(runner: &mut GameRunner) {
    pass_until(runner, |r| {
        r.state().stack.is_empty()
            && !matches!(r.state().waiting_for, WaitingFor::OrderTriggers { .. })
    });
}

fn awaiting_declaration(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    )
}

fn attacking(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == id))
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("an activated ability")
}

fn activate(runner: &mut GameRunner, source: ObjectId) {
    let ability_index = activated_index(runner, source);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is payable");
}

fn cast(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    runner.cast(spell).target_object(target).resolve();
    drain_stack(runner);
}

fn add_noctis(scenario: &mut GameScenario, owner: engine::types::player::PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, "Noctis, Heir Apparent", 2, 3, NOCTIS)
        .controlled_by(P0)
        .id()
}

#[test]
fn meandering_towershell_returns_attacking_at_its_controllers_next_declare_attackers_step() {
    let mut scenario = scenario();
    let towershell = scenario
        .add_creature_from_oracle(P0, "Meandering Towershell", 5, 9, MEANDERING_TOWERSHELL)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    pass_until(&mut runner, awaiting_declaration);
    let first_turn = runner.state().turn_number;
    runner
        .declare_attackers(&[(towershell, AttackTarget::Player(P1))])
        .expect("Meandering Towershell can attack");
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, towershell), Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_until(&mut runner, |r| {
        r.state().active_player == P1 && r.state().phase == Phase::End
    });
    assert_eq!(zone(&runner, towershell), Zone::Exile);

    pass_until(&mut runner, |r| {
        r.state().turn_number > first_turn
            && r.state().active_player == P0
            && awaiting_declaration(r)
    });
    assert_eq!(zone(&runner, towershell), Zone::Exile);
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("the bear can attack");
    drain_stack(&mut runner);
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    assert_eq!(zone(&runner, towershell), Zone::Battlefield);
    assert!(runner.state().objects[&towershell].tapped);
    assert!(attacking(&runner, towershell));

    let life = runner.state().players[1].life;
    pass_until(&mut runner, |r| r.state().phase == Phase::PostCombatMain);
    assert_eq!(runner.state().players[1].life, life - 7);
    // CR 508.3a: a creature put onto the battlefield attacking was never declared, so it is not exiled again.
    assert_eq!(zone(&runner, towershell), Zone::Battlefield);
    assert!(runner.state().delayed_triggers.is_empty());
}

#[test]
fn a_sole_returned_attacker_after_an_empty_declaration_deals_combat_damage() {
    let mut scenario = scenario();
    let towershell = scenario
        .add_creature_from_oracle(P0, "Meandering Towershell", 5, 9, MEANDERING_TOWERSHELL)
        .id();
    let mut runner = scenario.build();

    pass_until(&mut runner, awaiting_declaration);
    let first_turn = runner.state().turn_number;
    runner
        .declare_attackers(&[(towershell, AttackTarget::Player(P1))])
        .expect("Meandering Towershell can attack");
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, towershell), Zone::Exile);

    pass_until(&mut runner, |r| {
        r.state().turn_number > first_turn + 1 && r.state().phase == Phase::BeginCombat
    });
    let life = runner.state().players[1].life;
    pass_until(&mut runner, |r| zone(r, towershell) == Zone::Battlefield);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    assert!(attacking(&runner, towershell));

    pass_until(&mut runner, |r| r.state().phase == Phase::PostCombatMain);
    assert_eq!(runner.state().players[1].life, life - 5);
}

#[test]
fn noctis_returns_attacking_this_combat_and_cannot_be_blocked() {
    let mut scenario = scenario();
    let noctis = add_noctis(&mut scenario, P0);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();

    activate(&mut runner, noctis);
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, noctis), Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_until(&mut runner, awaiting_declaration);
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("the bear can attack");
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, noctis), Zone::Battlefield);
    assert!(runner.state().objects[&noctis].tapped);
    assert!(attacking(&runner, noctis));

    pass_until(&mut runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    assert!(runner.declare_blockers(&[(giant, noctis)]).is_err());
    runner
        .declare_blockers(&[(giant, bear)])
        .expect("the bear can be blocked");
}

#[test]
fn noctis_returns_on_its_owners_declare_attackers_step_under_its_owners_control() {
    let mut scenario = scenario();
    let noctis = add_noctis(&mut scenario, P1);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&noctis].controller, P0);

    activate(&mut runner, noctis);
    drain_stack(&mut runner);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_until(&mut runner, awaiting_declaration);
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("the bear can attack");
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, noctis), Zone::Exile);

    pass_until(&mut runner, |r| zone(r, noctis) == Zone::Battlefield);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    assert_eq!(runner.state().objects[&noctis].controller, P1);
    let attack = runner
        .state()
        .combat
        .as_ref()
        .and_then(|combat| combat.attackers.iter().find(|a| a.object_id == noctis))
        .expect("Noctis is attacking");
    assert_eq!(attack.defending_player, P0);
}

fn aetherling_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = scenario();
    let aetherling = scenario
        .add_creature_from_oracle(P0, "Aetherling", 4, 5, AETHERLING)
        .id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    (scenario, aetherling)
}

#[test]
fn aetherling_returns_at_the_next_end_step() {
    let (scenario, aetherling) = aetherling_scenario();
    let mut runner = scenario.build();

    activate(&mut runner, aetherling);
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, aetherling), Zone::Exile);

    pass_until(&mut runner, |r| r.state().phase == Phase::End);
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, aetherling), Zone::Battlefield);
}

#[test]
fn aetherling_moved_out_of_exile_is_not_returned() {
    let (scenario, aetherling) = aetherling_scenario();
    let mut runner = scenario.build();

    activate(&mut runner, aetherling);
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, aetherling), Zone::Exile);
    put_into_graveyard(&mut runner, aetherling);

    resolve_the_end_step_return(&mut runner);
    // CR 603.7c: the card left the zone the delayed return expects it in.
    assert_eq!(zone(&runner, aetherling), Zone::Graveyard);
}

/// Move `id` to its owner's graveyard through the zone pipeline, so
/// replacement effects apply.
fn put_into_graveyard(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(id, Zone::Graveyard, id),
        &mut events,
    );
    assert_eq!(zone(runner, id), Zone::Graveyard);
}

/// Drive to the next end step, where the delayed return must be the one
/// trigger on the stack, and resolve it.
fn resolve_the_end_step_return(runner: &mut GameRunner) {
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    pass_until(runner, |r| r.state().phase == Phase::End);
    let stack: Vec<_> = runner.state().stack.iter().collect();
    assert!(
        matches!(
            stack.as_slice(),
            [entry] if matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        ),
        "the delayed return is on the stack at the end step: {stack:?}"
    );
    // CR 603.7b: a delayed triggered ability triggers only once.
    assert!(runner.state().delayed_triggers.is_empty());
    drain_stack(runner);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn add_spell(
    scenario: &mut GameScenario,
    name: &str,
    is_instant: bool,
    text: &str,
    shards: Vec<ManaCostShard>,
    generic: u32,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, is_instant, text)
        .with_mana_cost(ManaCost::Cost { shards, generic })
        .id()
}

#[test]
fn aetherling_destroyed_in_response_returns_nothing_from_an_earlier_exile() {
    let (mut scenario, aetherling) = aetherling_scenario();
    let impulse = add_spell(
        &mut scenario,
        "Reckless Impulse",
        false,
        RECKLESS_IMPULSE,
        vec![ManaCostShard::Red],
        1,
    );
    let murder = add_spell(
        &mut scenario,
        "Murder",
        true,
        MURDER,
        vec![ManaCostShard::Black, ManaCostShard::Black],
        1,
    );
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Blue);
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let mut runner = scenario.build();

    runner.cast(impulse).resolve();
    drain_stack(&mut runner);
    let impulse_cards: Vec<ObjectId> = runner
        .state()
        .tracked_object_sets
        .values()
        .find(|set| !set.is_empty())
        .cloned()
        .expect("Reckless Impulse publishes the cards it exiled");
    assert!(impulse_cards
        .iter()
        .all(|&card| zone(&runner, card) == Zone::Exile));

    activate(&mut runner, aetherling);
    cast(&mut runner, murder, aetherling);
    assert_eq!(zone(&runner, aetherling), Zone::Graveyard);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_until(&mut runner, |r| r.state().phase == Phase::End);
    drain_stack(&mut runner);
    assert!(runner.state().delayed_triggers.is_empty());
    // CR 400.7j: the return names only what this ability's own exile moved, and it moved nothing.
    assert!(impulse_cards
        .iter()
        .all(|&card| zone(&runner, card) == Zone::Exile));
    assert_eq!(zone(&runner, aetherling), Zone::Graveyard);
}

#[test]
fn aetherling_exiled_in_response_stays_exiled() {
    let (mut scenario, aetherling) = aetherling_scenario();
    let swords = add_spell(
        &mut scenario,
        "Swords to Plowshares",
        true,
        SWORDS_TO_PLOWSHARES,
        vec![ManaCostShard::White],
        0,
    );
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();

    activate(&mut runner, aetherling);
    cast(&mut runner, swords, aetherling);
    assert_eq!(zone(&runner, aetherling), Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_until(&mut runner, |r| r.state().phase == Phase::End);
    drain_stack(&mut runner);
    assert!(runner.state().delayed_triggers.is_empty());
    assert_eq!(zone(&runner, aetherling), Zone::Exile);
}

#[test]
fn aetherling_put_into_the_graveyard_and_exiled_again_before_the_end_step_is_not_returned() {
    let (mut scenario, aetherling) = aetherling_scenario();
    let crypt = scenario
        .add_artifact_from_oracle(P0, "Tormod's Crypt", TORMODS_CRYPT)
        .id();
    let mut runner = scenario.build();

    activate(&mut runner, aetherling);
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, aetherling), Zone::Exile);
    // Pull from Eternity's "target face-up exiled card" does not parse, so its move goes through the zone pipeline.
    put_into_graveyard(&mut runner, aetherling);
    let crypt_ability = activated_index(&runner, crypt);
    runner
        .activate(crypt, crypt_ability)
        .target_player(P0)
        .resolve();
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, aetherling), Zone::Exile);

    resolve_the_end_step_return(&mut runner);
    // CR 400.7 + CR 603.7c: the re-exiled card is a new object the delayed return cannot find.
    assert_eq!(zone(&runner, aetherling), Zone::Exile);
}

/// What P0 does about Noctis's delayed return after declaring no attackers.
enum Response {
    LetItResolve,
    CounterIt,
    RemoveNoctisFromCombat,
}

/// Noctis warp-strikes in the first main phase, P0 declares no attackers, and
/// Noctis's delayed return goes on the stack, where P0 makes `response`.
/// Drives to the postcombat main phase and returns the events emitted from
/// the declaration on, P1's life total at the declaration, and the
/// [`finish_combat`] record.
fn noctis_return_after_an_empty_declaration(
    response: Response,
) -> (GameRunner, ObjectId, Vec<GameEvent>, i32, CombatRecord) {
    let mut scenario = scenario();
    let noctis = add_noctis(&mut scenario, P0);
    let stifle = add_spell(&mut scenario, "Stifle", true, STIFLE, vec![], 0);
    let reconnaissance = scenario
        .add_enchantment_from_oracle(P0, "Reconnaissance", RECONNAISSANCE)
        .id();
    // A creature that could attack, so the declaration is a choice P0 makes.
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();

    activate(&mut runner, noctis);
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, noctis), Zone::Exile);
    pass_until(&mut runner, awaiting_declaration);
    let life = runner.state().players[1].life;
    let mut events = runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("an empty declaration is legal")
        .events;
    let stack: Vec<_> = runner.state().stack.iter().collect();
    let delayed_return = match stack.as_slice() {
        [entry] if matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }) => entry.id,
        other => panic!("expected only the delayed return on the stack, got {other:?}"),
    };
    match response {
        Response::LetItResolve => {}
        Response::CounterIt => {
            runner.cast(stifle).target_object(delayed_return).commit();
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Green,
                ObjectId(0),
                false,
                Vec::new(),
            ));
        }
        Response::RemoveNoctisFromCombat => {
            while !runner.state().stack.is_empty() {
                events.extend(step(&mut runner));
            }
            assert!(attacking(&runner, noctis));
            let ability = activated_index(&runner, reconnaissance);
            runner
                .activate(reconnaissance, ability)
                .target_object(noctis)
                .resolve();
            assert!(!attacking(&runner, noctis));
        }
    }
    let record = finish_combat(&mut runner, &mut events);
    (runner, noctis, events, life, record)
}

/// The steps in which a priority prompt appeared, and P0's mana pool size as
/// the end of combat step began.
struct CombatRecord {
    priority_steps: Vec<Phase>,
    pool_at_end_of_combat: Option<usize>,
}

/// Steps to the postcombat main phase, appending the emitted events.
fn finish_combat(runner: &mut GameRunner, events: &mut Vec<GameEvent>) -> CombatRecord {
    let mut record = CombatRecord {
        priority_steps: Vec::new(),
        pool_at_end_of_combat: None,
    };
    while runner.state().phase != Phase::PostCombatMain {
        let phase = runner.state().phase;
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            record.priority_steps.push(phase);
        }
        if phase == Phase::EndCombat {
            record
                .pool_at_end_of_combat
                .get_or_insert(runner.state().players[0].mana_pool.total());
        }
        events.extend(step(runner));
    }
    record
}

fn entered(runner: &GameRunner, events: &[GameEvent], step: Phase) -> bool {
    let changed = events
        .iter()
        .any(|event| matches!(event, GameEvent::PhaseChanged { phase } if *phase == step));
    assert_eq!(
        changed,
        runner.state().steps_started_this_turn.count(step) == 1,
        "{step:?}: PhaseChanged and the step ledger disagree"
    );
    changed
}

#[test]
fn a_countered_return_after_an_empty_declaration_skips_blockers_and_damage() {
    let (runner, noctis, events, _, record) =
        noctis_return_after_an_empty_declaration(Response::CounterIt);
    assert_eq!(zone(&runner, noctis), Zone::Exile);
    assert!(runner.state().delayed_triggers.is_empty());
    assert!(entered(&runner, &events, Phase::EndCombat));
    // CR 508.8 + CR 500.11: with no attacker the two steps never begin.
    assert!(!entered(&runner, &events, Phase::DeclareBlockers));
    assert!(!entered(&runner, &events, Phase::CombatDamage));
    // CR 500.5: mana floated in the declare attackers step empties as that step ends.
    assert_eq!(record.pool_at_end_of_combat, Some(0));
}

#[test]
fn a_resolved_return_after_an_empty_declaration_keeps_blockers_and_damage() {
    let (runner, noctis, events, life, _) =
        noctis_return_after_an_empty_declaration(Response::LetItResolve);
    assert_eq!(zone(&runner, noctis), Zone::Battlefield);
    assert!(entered(&runner, &events, Phase::DeclareBlockers));
    assert!(entered(&runner, &events, Phase::CombatDamage));
    assert!(entered(&runner, &events, Phase::EndCombat));
    assert_eq!(runner.state().players[1].life, life - 2);
}

#[test]
fn a_returned_attacker_removed_from_combat_keeps_the_declare_blockers_step() {
    let (runner, noctis, events, _, record) =
        noctis_return_after_an_empty_declaration(Response::RemoveNoctisFromCombat);
    assert_eq!(zone(&runner, noctis), Zone::Battlefield);
    // CR 508.8: Noctis was put onto the battlefield attacking, so the steps are not skipped.
    assert!(entered(&runner, &events, Phase::DeclareBlockers));
    assert!(record.priority_steps.contains(&Phase::DeclareBlockers));
    assert!(entered(&runner, &events, Phase::CombatDamage));
    assert!(record.priority_steps.contains(&Phase::CombatDamage));
    assert!(entered(&runner, &events, Phase::EndCombat));
}

#[test]
fn a_declared_attacker_removed_from_combat_keeps_blockers_and_damage() {
    let mut scenario = scenario();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let reconnaissance = scenario
        .add_enchantment_from_oracle(P0, "Reconnaissance", RECONNAISSANCE)
        .id();
    let mut runner = scenario.build();

    pass_until(&mut runner, awaiting_declaration);
    let life = runner.state().players[1].life;
    let mut events = runner
        .declare_attackers(&[(bears, AttackTarget::Player(P1))])
        .expect("Grizzly Bears can attack")
        .events;
    assert!(attacking(&runner, bears));
    let ability = activated_index(&runner, reconnaissance);
    runner
        .activate(reconnaissance, ability)
        .target_object(bears)
        .resolve();
    assert!(!attacking(&runner, bears));
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);

    let record = finish_combat(&mut runner, &mut events);
    // CR 508.8 + CR 509.2 + CR 510.3: Grizzly Bears was declared, so both steps
    // happen and give priority even though nothing is attacking.
    assert!(entered(&runner, &events, Phase::DeclareBlockers));
    assert!(record.priority_steps.contains(&Phase::DeclareBlockers));
    assert!(entered(&runner, &events, Phase::CombatDamage));
    assert!(record.priority_steps.contains(&Phase::CombatDamage));
    assert!(entered(&runner, &events, Phase::EndCombat));
    assert_eq!(runner.state().players[1].life, life);
}
