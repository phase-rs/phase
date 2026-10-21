//! Tam, the Possibility — count-distinct-subtypes X bindings (CR 205.3j).
//!
//! Tam's "{W}{U}{B}{R}{G}, {T}: Proliferate X times, where X is the number of
//! planeswalker types among planeswalkers you control" binds X through
//! `QuantityRef::DistinctSubtypes { restrict: Some(Planeswalker) }`.
//! Saavik, Stoic Student generalizes the same primitive along three axes at
//! once (trigger route, damage consumer, creature category + non-Borg
//! population).

use engine::game::combat::{AttackTarget, AttackerInfo};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const TAM: &str = "Planeswalker spells you cast cost {1} less to cast.\n{W}{U}{B}{R}{G}, {T}: Proliferate X times, where X is the number of planeswalker types among planeswalkers you control. (To proliferate, choose any number of permanents and/or players, then give each another counter of each kind already there.)";

// Verbatim Oracle text (`\u{2014}` is the printed em dash in "Federation —").
const SAAVIK: &str = "Flash\nFederation \u{2014} When Saavik enters, she deals X damage to target attacking or blocking creature an opponent controls, where X is the number of creature types among non-Borg creatures you control.";

fn wubrg_pool() -> Vec<ManaUnit> {
    [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ]
    .into_iter()
    .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
    .collect()
}

fn proliferate_action_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::Proliferate,
                    ..
                }
            )
        })
        .count()
}

/// CR 205.3j + CR 701.34a + CR 107.3c: Tam proliferates X=N times for N
/// distinct planeswalker types (R1), evaluated ONCE per resolution (R2 — the
/// exact-N prompt count IS the single-evaluation evidence; the ruling leaves
/// no response window between proliferatings).
#[test]
fn tam_proliferates_once_per_distinct_planeswalker_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tam = scenario
        .add_creature_from_oracle(P0, "Tam, the Possibility", 2, 4, TAM)
        .id();
    scenario.with_mana_pool(P0, wubrg_pool());
    // Two planeswalkers of distinct types (Jace + Chandra) → X=2.
    let jace = scenario
        .add_planeswalker_from_oracle(P0, "Jace, Test Mage", "Jace", 3, "")
        .id();
    let chandra = scenario
        .add_planeswalker_from_oracle(P0, "Chandra, Test Pyromancer", "Chandra", 3, "")
        .id();
    // Hostile (same test): an opponent's Teferi-subtype planeswalker must NOT
    // count (first branch: `controller: You` in Objects).
    let teferi = scenario
        .add_planeswalker_from_oracle(P1, "Teferi, Test Chronomancer", "Teferi", 3, "")
        .id();
    // The counter-bearing permanent: two kinds, so "each kind" is load-bearing.
    let vessel = scenario.add_creature(P0, "Counter Vessel", 2, 2).id();
    scenario.with_counter(vessel, CounterType::Plus1Plus1, 1);
    scenario.with_counter(vessel, CounterType::Generic("charge".to_string()), 1);
    let mut runner = scenario.build();

    let outcome = runner.activate(tam, 0).resolve();
    let mut events: Vec<GameEvent> = outcome.events().to_vec();
    // Answer each ProliferateChoice with a NON-IDENTICAL set (proves two
    // separate prompts, R2): first [vessel, jace], then [vessel, chandra].
    let mut prompts = 0;
    let mut settled = false;
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ProliferateChoice { player, .. } => {
                assert_eq!(player, P0, "Tam's controller proliferates (actor)");
                prompts += 1;
                let pick = if prompts == 1 {
                    vec![vessel, jace]
                } else {
                    vec![vessel, chandra]
                };
                let result = runner
                    .act(GameAction::SelectTargets {
                        targets: pick.into_iter().map(TargetRef::Object).collect(),
                    })
                    .expect("proliferate choice accepted");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                settled = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected Tam prompt: {other:?}"),
        }
    }
    assert!(
        settled,
        "the activation must settle at Priority with an empty stack"
    );
    assert_eq!(
        prompts, 2,
        "X=2 distinct planeswalker types → exactly 2 prompts (R1+R2)"
    );

    // The double-chosen vessel gains +2 of EACH kind.
    let vessel_obj = &runner.state().objects[&vessel];
    assert_eq!(
        vessel_obj.counters.get(&CounterType::Plus1Plus1),
        Some(&3),
        "vessel chosen twice → +2/+1 base"
    );
    assert_eq!(
        vessel_obj
            .counters
            .get(&CounterType::Generic("charge".to_string())),
        Some(&3),
        "vessel chosen twice → +2 charge"
    );
    // Each planeswalker chosen once gains +1 loyalty counter.
    assert_eq!(
        runner.state().objects[&jace]
            .counters
            .get(&CounterType::Loyalty),
        Some(&4),
        "Jace chosen once → +1 loyalty"
    );
    assert_eq!(
        runner.state().objects[&chandra]
            .counters
            .get(&CounterType::Loyalty),
        Some(&4),
        "Chandra chosen once → +1 loyalty"
    );
    // The opponent's planeswalker is untouched (never chosen, never counted).
    assert_eq!(
        runner.state().objects[&teferi]
            .counters
            .get(&CounterType::Loyalty),
        Some(&3),
        "opponent's Teferi untouched"
    );
    // Exactly 2 proliferate keyword actions (one per iteration).
    assert_eq!(
        proliferate_action_count(&events),
        2,
        "CR 701.34a: exactly one player-action event per proliferate action"
    );
    // Reach-guard: the activation actually resolved (cost paid).
    assert!(
        runner.state().objects[&tam].tapped,
        "Tam tapped → activation resolved"
    );
}

/// CR 205.3j neg: Tam with NO planeswalkers → X=0 → zero prompts; the ability
/// still resolves (first branch: `total_iterations=0` loop skip).
#[test]
fn tam_with_no_planeswalkers_proliferates_zero_times() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tam = scenario
        .add_creature_from_oracle(P0, "Tam, the Possibility", 2, 4, TAM)
        .id();
    scenario.with_mana_pool(P0, wubrg_pool());
    // A counter-bearing permanent is present, so any prompt WOULD open — its
    // absence proves X=0, not an empty board.
    let vessel = scenario.add_creature(P0, "Counter Vessel", 2, 2).id();
    scenario.with_counter(vessel, CounterType::Plus1Plus1, 1);
    let mut runner = scenario.build();

    let outcome = runner.activate(tam, 0).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "X=0 completes straight to Priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(runner.state().stack.is_empty(), "X=0 leaves an empty stack");
    assert_eq!(
        proliferate_action_count(outcome.events()),
        0,
        "zero iterations → zero proliferate actions"
    );
    // Reach-guards: the ability still resolved (announced + costs paid +
    // completed to Priority/empty). NOTE (plan-tree delta): the zero-iteration
    // repeat emits no `EffectResolved` (completion fires per inner effect and
    // the loop body never runs), so announcement + paid costs + the Priority /
    // empty-stack boundary carry the "still resolves" proof instead.
    assert!(
        outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::AbilityActivated { .. })),
        "the activation was announced"
    );
    assert!(
        runner.state().objects[&tam].tapped,
        "Tam tapped → activation resolved"
    );
    assert!(
        runner.state().players[P0.0 as usize].mana_pool.is_empty(),
        "WUBRG paid → activation resolved"
    );
    // Counters untouched.
    assert_eq!(
        runner.state().objects[&vessel]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "no proliferation → counters unchanged"
    );
}

/// CR 205.3m + CR 603: Saavik generalizes the primitive (trigger route, damage
/// consumer, creature category, non-Borg population) — 2 distinct non-Borg
/// creature types → 2 damage to the victim. The damage IS the trigger-bound
/// proof (an `Unimplemented` trigger deals 0).
#[test]
fn saavik_deals_damage_equal_to_distinct_non_borg_creature_types() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saavik = scenario
        .add_creature_to_hand_from_oracle(P0, "Saavik, Stoic Student", 2, 2, SAAVIK)
        .id();
    scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"]);
    scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"]);
    // Hostile (same test): a Borg Drone of a 3rd creature type — filtered from
    // the population (`Non(Subtype(Borg))`), so damage stays 2.
    scenario
        .add_creature(P0, "Borg Drone", 2, 2)
        .with_subtypes(vec!["Borg"]);
    let victim = scenario.add_creature(P1, "Attacking Goblin", 3, 3).id();
    let mut runner = scenario.build();
    // Creature-type DB for `restrict: Creature` (the scenario default is
    // empty). Saavik herself carries no subtypes in this fixture (the builder
    // stamps no type line), so the count is exactly the staged Bear/Elf.
    runner.state_mut().all_creature_types =
        vec!["Bear".to_string(), "Elf".to_string(), "Borg".to_string()];
    // Stage the victim as attacking (`FilterProp::Attacking` reads state.combat).
    runner
        .state_mut()
        .combat
        .get_or_insert_with(Default::default)
        .attackers
        .push(AttackerInfo::new(victim, AttackTarget::Player(P0), P0));

    let outcome = runner.cast(saavik).target_object(victim).resolve();
    assert_eq!(
        outcome.state().objects[&victim].damage_marked,
        2,
        "2 distinct non-Borg creature types → 2 damage (trigger bound)"
    );
    // Boundary: Priority + empty stack; no life misrouting.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "cast completes to Priority, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_stack_size(0);
    outcome.assert_life_delta(P0, 0);
    outcome.assert_life_delta(P1, 0);
}

/// Non-Borg scope: Saavik's text has NO changeling rider, but none is needed —
/// a changeling IS every creature type (CR 702.73a), hence Borg, so
/// `Non(Subtype("Borg"))` excludes it from the population (CR 205.3m scope).
/// One Bear + a changeling (DB {Bear, Elf, Borg}) → 1 damage: the Bear
/// contributes 1 type, the Vandal contributes 0.
#[test]
fn saavik_excludes_changelings_as_borg() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saavik = scenario
        .add_creature_to_hand_from_oracle(P0, "Saavik, Stoic Student", 2, 2, SAAVIK)
        .id();
    scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"]);
    scenario
        .add_creature(P0, "Masked Vandal", 1, 3)
        .with_subtypes(vec!["Shapeshifter"])
        .with_keyword(Keyword::Changeling);
    let victim = scenario.add_creature(P1, "Attacking Goblin", 3, 3).id();
    let mut runner = scenario.build();
    // DB WITH Borg (production vocabulary always includes it): the changeling
    // matches Borg → excluded by Non(Subtype("Borg")) → contributes 0.
    runner.state_mut().all_creature_types =
        vec!["Bear".to_string(), "Elf".to_string(), "Borg".to_string()];
    runner
        .state_mut()
        .combat
        .get_or_insert_with(Default::default)
        .attackers
        .push(AttackerInfo::new(victim, AttackTarget::Player(P0), P0));

    let outcome = runner.cast(saavik).target_object(victim).resolve();
    assert_eq!(
        outcome.state().objects[&victim].damage_marked,
        1,
        "changeling is Borg → excluded; Bear alone contributes 1"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "cast completes to Priority, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_stack_size(0);
}
