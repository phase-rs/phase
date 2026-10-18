//! CR 603.2c + CR 603.7b: a multi-fire delayed trigger ("whenever … this turn",
//! "until end of turn, whenever …") triggers on EACH occurrence in its event —
//! once per declared attacker or blocker — exactly as the same printed trigger
//! does. One fan-out authority (`occurrence_trigger_events`) serves printed and
//! delayed triggers; batched ("one or more") triggers fire once.
//!
//! Oracle text is verbatim from Scryfall, except fixtures labelled synthetic.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SUMMON_LEVIATHAN: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\n\
I \u{2014} Return each creature that isn't a Kraken, Leviathan, Merfolk, Octopus, or Serpent to its owner's hand.\n\
II, III \u{2014} Until end of turn, whenever a Kraken, Leviathan, Merfolk, Octopus, or Serpent attacks, draw a card.\n\
Ward {2}";
const STEADY_PROGRESS: &str = "Proliferate.\nDraw a card.";
const RIGHTEOUS_CAUSE: &str = "Whenever a creature attacks, you gain 1 life.";
const BATTLE_CRY: &str = "Untap all white creatures you control.\nWhenever a creature blocks this turn, it gets +0/+1 until end of turn.";
const CONSUMING_RAGE: &str = "Whenever a Minotaur attacks this turn, it gets +2/+0 until end of turn. Destroy that creature at end of combat.";
const GARRUK: &str = "Whenever a creature you control with power 4 or greater enters, draw a card.\n+2: Untap up to two target lands.\n\u{2212}3: Create a 4/4 green Beast creature token with trample.\n\u{2212}4: Until your next turn, whenever one or more creatures attack one of your opponents, those creatures get +2/+2 and gain trample until end of turn.";
const BASRI_KET: &str = "+1: Put a +1/+1 counter on up to one target creature. It gains indestructible until end of turn.\n\u{2212}2: Whenever one or more nontoken creatures attack this turn, create that many 1/1 white Soldier creature tokens that are tapped and attacking.\n\u{2212}6: You get an emblem with \"At the beginning of combat on your turn, create a 1/1 white Soldier creature token, then put a +1/+1 counter on each creature you control.\"";
const TASHA: &str = "+1: Until your next turn, whenever a creature attacks you or Tasha, Unholy Archmage, put a -1/-1 counter on that creature.\n\u{2212}2: Target opponent puts a creature card of their choice from their graveyard onto the battlefield under your control. That creature gains ward {2}.\n\u{2212}6: Target opponent reveals cards from the top of their library until they reveal three creature cards. Put those cards onto the battlefield under your control. That player puts the rest into their graveyard.";
const FIRST_DAY_OF_CLASS: &str = "Whenever a creature you control enters this turn, put a +1/+1 counter on it and it gains haste until end of turn.\nLearn. (You may reveal a Lesson card you own from outside the game and put it into your hand, or discard a card to draw a card.)";
const DONT_MOVE: &str = "Destroy all tapped creatures. Until your next turn, whenever a creature becomes tapped, destroy it.";
const SHRIVELING_ROT: &str = "Choose one \u{2014}\n\u{2022} Until end of turn, whenever a creature is dealt damage, destroy it.\n\u{2022} Until end of turn, whenever a creature dies, that creature's controller loses life equal to its toughness.\nEntwine {2}{B} (Choose both if you pay the entwine cost.)";
const FALSE_CURE: &str = "Until end of turn, whenever a player gains life, that player loses 2 life for each 1 life they gained.";
const MIRARI: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\n\
I \u{2014} Return target instant card from your graveyard to your hand.\n\
II \u{2014} Return target sorcery card from your graveyard to your hand.\n\
III \u{2014} Until end of turn, whenever you cast an instant or sorcery spell, copy it. You may choose new targets for the copy.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";

fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let names_p0: Vec<String> = (0..10).map(|i| format!("P0 Card {i}")).collect();
    let names_p1: Vec<String> = (0..10).map(|i| format!("P1 Card {i}")).collect();
    scenario.with_library_top(P0, &names_p0.iter().map(String::as_str).collect::<Vec<_>>());
    scenario.with_library_top(P1, &names_p1.iter().map(String::as_str).collect::<Vec<_>>());
    scenario
}

fn typed(scenario: &mut GameScenario, owner: PlayerId, name: &str, subtype: &str) -> ObjectId {
    scenario
        .add_creature(owner, name, 2, 2)
        .with_subtypes(vec![subtype])
        .id()
}

fn free_spell(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    instant: bool,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(owner, name, instant, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn add_leviathan(scenario: &mut GameScenario, lore: u32) -> ObjectId {
    let saga = scenario
        .add_creature(P0, "Summon: Leviathan", 6, 6)
        .as_enchantment()
        .with_subtypes(vec!["Saga", "Leviathan"])
        .from_oracle_text_with_keywords(&["Ward"], SUMMON_LEVIATHAN)
        .id();
    scenario.with_counter(saga, CounterType::Lore, lore);
    saga
}

fn hand(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());
    let o = &runner.state().objects[&id];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

/// Resolve the stack, answering ordering, optional "you may" (accept), target
/// prompts (first legal) and proliferate choices (`proliferate_pick`). Stops at
/// the first other decision.
fn settle(runner: &mut GameRunner, proliferate_pick: &[ObjectId]) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional effect");
            }
            WaitingFor::LearnChoice { .. } => {
                runner
                    .act(GameAction::LearnDecision {
                        choice: engine::types::actions::LearnOption::Skip,
                    })
                    .expect("decline to learn");
            }
            WaitingFor::ProliferateChoice { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: proliferate_pick
                            .iter()
                            .map(|id| TargetRef::Object(*id))
                            .collect(),
                    })
                    .expect("proliferate choice");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the first legal target");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("stack did not settle: {:?}", runner.state().waiting_for);
}

/// Pass through the game, declaring no attackers or blockers, until `stop`.
fn drive_until(runner: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..400 {
        if stop(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }
    panic!(
        "never reached the stop condition: phase={:?} active={:?} waiting_for={:?}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
}

/// Declare `attackers` (each at `defender`) when the declaration is offered.
fn attack(runner: &mut GameRunner, attackers: &[ObjectId], defender: PlayerId) {
    let attacker_player = runner.state().objects[&attackers[0]].controller;
    drive_until(
        runner,
        |r| matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { player, .. } if player == attacker_player),
    );
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(defender)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
}

/// Declare `assignments` (blocker, attacker) when blocks are offered.
fn block(runner: &mut GameRunner, assignments: &[(ObjectId, ObjectId)]) {
    drive_until(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    runner
        .act(GameAction::DeclareBlockers {
            assignments: assignments.to_vec(),
        })
        .expect("declare blockers");
}

/// Start P0's turn 2 at upkeep and advance into its precombat main, where
/// CR 714.3c adds a lore counter to each Saga; resolve the chapters.
fn advance_sagas(runner: &mut GameRunner) {
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    settle(runner, &[]);
}

/// CR 714.2b + CR 603.7b: the chapter-II delayed trigger names no controller.
/// It is installed on P1's turn — P0's Steady Progress proliferates the Saga
/// from lore 1 to 2 — and each of P1's two listed attackers (Octopus, Kraken)
/// draws P0 a card; P1's Bear attacking alongside them does not.
#[test]
fn leviathan_chapter_two_fires_for_an_opponents_listed_attacker() {
    let mut scenario = board();
    let saga = add_leviathan(&mut scenario, 1);
    let octopus = typed(&mut scenario, P1, "Octopus", "Octopus");
    let kraken = typed(&mut scenario, P1, "Kraken", "Kraken");
    let bear = typed(&mut scenario, P1, "Bear", "Bear");
    let progress = free_spell(&mut scenario, P0, "Steady Progress", true, STEADY_PROGRESS);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.phase = Phase::PreCombatMain;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes to P0");
    runner.cast(progress).commit();
    settle(&mut runner, &[saga]);
    assert_eq!(
        runner.state().objects[&saga].counters[&CounterType::Lore],
        2,
        "reach guard: proliferate added a lore counter"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: chapter II installed its delayed trigger on P1's turn"
    );

    // Baseline after Steady Progress's own draw.
    let p0_hand = hand(&runner, P0);
    attack(&mut runner, &[octopus, kraken, bear], P0);
    settle(&mut runner, &[]);
    assert_eq!(
        hand(&runner, P0),
        p0_hand + 2,
        "one draw each for the Octopus and the Kraken"
    );
}

/// CR 603.2c: two Leviathans each installed a chapter-II trigger. Two listed
/// attackers → four draws (the plain cardinality control; identical no-input
/// draws auto-order, so no prompt is required).
#[test]
fn two_leviathan_generators_fire_four_times() {
    let mut scenario = board();
    add_leviathan(&mut scenario, 1);
    add_leviathan(&mut scenario, 1);
    let merfolk = typed(&mut scenario, P0, "Merfolk", "Merfolk");
    let serpent = typed(&mut scenario, P0, "Serpent", "Serpent");
    let mut runner = scenario.build();
    advance_sagas(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        2,
        "reach guard: two chapter-II generators installed"
    );
    let p0_hand = hand(&runner, P0);
    attack(&mut runner, &[merfolk, serpent], P1);
    settle(&mut runner, &[]);
    assert_eq!(
        hand(&runner, P0),
        p0_hand + 4,
        "two generators x two attackers"
    );
}

/// CR 603.3b: the ordering/resume witness. Two Leviathan generators and
/// Consuming Rage (a distinguishable delayed trigger) all trigger on two
/// listed Minotaur attackers, so the delayed batch needs a real ordering
/// prompt. After the order is submitted: four draws, each Minotaur pumped once,
/// and nothing re-fires when the game resumes. Righteous Cause (printed) gains
/// 1 life per attacker.
#[test]
fn two_leviathan_generators_fire_four_times_and_order_with_a_printed_trigger() {
    let mut scenario = board();
    add_leviathan(&mut scenario, 1);
    add_leviathan(&mut scenario, 1);
    scenario.add_enchantment_from_oracle(P0, "Righteous Cause", RIGHTEOUS_CAUSE);
    let merfolk = scenario
        .add_creature(P0, "Merfolk Minotaur", 2, 2)
        .with_subtypes(vec!["Merfolk", "Minotaur"])
        .id();
    let serpent = scenario
        .add_creature(P0, "Serpent Minotaur", 2, 2)
        .with_subtypes(vec!["Serpent", "Minotaur"])
        .id();
    let rage = free_spell(&mut scenario, P0, "Consuming Rage", false, CONSUMING_RAGE);
    let mut runner = scenario.build();
    advance_sagas(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        2,
        "reach guard: two chapter-II generators installed"
    );
    runner.cast(rage).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        3,
        "reach guard: Consuming Rage installed"
    );

    let (p0_hand, p0_life) = (hand(&runner, P0), runner.life(P0));
    attack(&mut runner, &[merfolk, serpent], P1);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "a real ordering prompt: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_trigger_order.is_some(),
        "the order is pending while the prompt is open"
    );
    settle(&mut runner, &[]);
    assert_eq!(
        hand(&runner, P0),
        p0_hand + 4,
        "two generators x two attackers"
    );
    assert_eq!(runner.life(P0), p0_life + 2, "Righteous Cause per attacker");
    assert_eq!(pt(&mut runner, merfolk), (4, 2), "pumped once");
    assert_eq!(pt(&mut runner, serpent), (4, 2), "pumped once");

    drive_until(&mut runner, |r| r.state().phase == Phase::PostCombatMain);
    assert_eq!(
        hand(&runner, P0),
        p0_hand + 4,
        "no re-firing after the resume"
    );
}

/// Battle Cry: "it" is each blocker, and each of two blockers gets +0/+1.
#[test]
fn battle_cry_pumps_each_blocker() {
    let mut scenario = board();
    let a1 = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let b1 = scenario.add_creature(P1, "Blocker A", 1, 1).id();
    let b2 = scenario.add_creature(P1, "Blocker B", 1, 1).id();
    let cry = free_spell(&mut scenario, P0, "Battle Cry", true, BATTLE_CRY);
    let mut runner = scenario.build();
    runner.cast(cry).resolve();
    attack(&mut runner, &[a1, a2], P1);
    block(&mut runner, &[(b1, a1), (b2, a2)]);
    settle(&mut runner, &[]);
    assert_eq!(pt(&mut runner, b1), (1, 2), "Blocker A +0/+1");
    assert_eq!(pt(&mut runner, b2), (1, 2), "Blocker B +0/+1");
    assert_eq!(pt(&mut runner, a1), (2, 2), "attackers untouched");
}

/// Consuming Rage: each attacking Minotaur gets +2/+0, and each is destroyed at
/// end of combat.
#[test]
fn consuming_rage_pumps_and_destroys_each_minotaur() {
    let mut scenario = board();
    let m1 = typed(&mut scenario, P0, "Minotaur A", "Minotaur");
    let m2 = typed(&mut scenario, P0, "Minotaur B", "Minotaur");
    let bear = typed(&mut scenario, P0, "Bear", "Bear");
    let rage = free_spell(&mut scenario, P0, "Consuming Rage", false, CONSUMING_RAGE);
    let mut runner = scenario.build();
    runner.cast(rage).resolve();
    attack(&mut runner, &[m1, m2, bear], P1);
    settle(&mut runner, &[]);
    assert_eq!(pt(&mut runner, m1), (4, 2), "Minotaur A +2/+0");
    assert_eq!(pt(&mut runner, m2), (4, 2), "Minotaur B +2/+0");
    assert_eq!(pt(&mut runner, bear), (2, 2), "the Bear isn't a Minotaur");
    drive_until(&mut runner, |r| r.state().phase == Phase::PostCombatMain);
    assert_eq!(
        zone(&runner, m1),
        Zone::Graveyard,
        "destroyed at end of combat"
    );
    assert_eq!(
        zone(&runner, m2),
        Zone::Graveyard,
        "destroyed at end of combat"
    );
    assert_eq!(zone(&runner, bear), Zone::Battlefield);
}

/// Hostile: one Minotaur is bounced in response to its trigger. The other
/// Minotaur's own firing still pumps it; the bounced one's firing affects
/// nothing.
#[test]
fn consuming_rage_survives_one_minotaur_leaving_before_its_trigger_resolves() {
    let mut scenario = board();
    let m1 = typed(&mut scenario, P0, "Minotaur A", "Minotaur");
    let m2 = typed(&mut scenario, P0, "Minotaur B", "Minotaur");
    let rage = free_spell(&mut scenario, P0, "Consuming Rage", false, CONSUMING_RAGE);
    let unsummon = free_spell(&mut scenario, P0, "Unsummon", true, UNSUMMON);
    let mut runner = scenario.build();
    runner.cast(rage).resolve();
    attack(&mut runner, &[m1, m2], P1);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: one firing per Minotaur"
    );
    runner.cast(unsummon).target_object(m1).commit();
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, m1), Zone::Hand, "reach guard: bounced");
    assert_eq!(
        pt(&mut runner, m2),
        (4, 2),
        "the remaining Minotaur is pumped"
    );
}

/// Garruk −4 (batched, "one or more"): one firing for the whole declaration;
/// each attacker gets +2/+2 exactly once.
#[test]
fn garruk_batched_attack_trigger_fires_once() {
    let mut scenario = board();
    let garruk = scenario
        .add_planeswalker_from_oracle(P0, "Garruk, Curse Breaker", "Garruk", 5, GARRUK)
        .as_legendary()
        .id();
    let a1 = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let mut runner = scenario.build();
    runner.activate(garruk, 2).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1, "reach guard");
    attack(&mut runner, &[a1, a2], P1);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "one firing for the whole declaration"
    );
    settle(&mut runner, &[]);
    assert_eq!(pt(&mut runner, a1), (4, 4), "+2/+2 once");
    assert_eq!(pt(&mut runner, a2), (4, 4), "+2/+2 once");
    for (id, name) in [(a1, "A"), (a2, "B")] {
        assert!(
            runner.state().objects[&id]
                .keywords
                .contains(&engine::types::keywords::Keyword::Trample),
            "Attacker {name} gained trample"
        );
    }
}

/// Batched ("one or more") delayed control with a non-additive body: one
/// firing for the whole declaration, so one draw for two attackers. Synthetic.
#[test]
fn batched_delayed_attack_trigger_draws_once() {
    let mut scenario = board();
    let a1 = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let spell = free_spell(
        &mut scenario,
        P0,
        "Synthetic Muster",
        false,
        "Until end of turn, whenever one or more creatures attack, draw a card.",
    );
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1, "reach guard");
    let p0_hand = hand(&runner, P0);
    attack(&mut runner, &[a1, a2], P1);
    settle(&mut runner, &[]);
    assert_eq!(hand(&runner, P0), p0_hand + 1, "one firing, one draw");
}

/// Basri Ket −2 (batched): two nontoken attackers and one token attacker →
/// one firing creating two Soldiers, which enter attacking and don't re-fire
/// (CR 508.3a: they were never declared as attackers).
#[test]
fn basri_ket_batched_attack_trigger_counts_nontoken_attackers_once() {
    let mut scenario = board();
    let basri = scenario
        .add_planeswalker_from_oracle(P0, "Basri Ket", "Basri", 5, BASRI_KET)
        .as_legendary()
        .id();
    let a1 = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let token = scenario.add_creature(P0, "Token Attacker", 1, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    runner.activate(basri, 1).resolve();
    let soldiers = |r: &GameRunner| {
        r.state()
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.name == "Soldier")
            .count()
    };
    attack(&mut runner, &[a1, a2, token], P1);
    settle(&mut runner, &[]);
    assert_eq!(soldiers(&runner), 2, "that many = two nontoken attackers");
    drive_until(&mut runner, |r| r.state().phase == Phase::PostCombatMain);
    assert_eq!(soldiers(&runner), 2, "the Soldiers don't re-fire it");
}

/// CR 509.3c (bare form): one firing per blocked attacker, however many
/// creatures block it. Two blockers on one attacker and one on another → each
/// attacker gets exactly one +1/+1. Synthetic delayed text.
#[test]
fn bare_becomes_blocked_fires_once_per_attacker() {
    let mut scenario = board();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Attacker", 2, 2).id();
    let b1 = scenario.add_creature(P1, "Blocker A", 1, 1).id();
    let b2 = scenario.add_creature(P1, "Blocker B", 1, 1).id();
    let b3 = scenario.add_creature(P1, "Blocker C", 1, 1).id();
    let spell = free_spell(
        &mut scenario,
        P0,
        "Synthetic Rally",
        false,
        "Until end of turn, whenever a creature becomes blocked, it gets +1/+1 until end of turn.",
    );
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    attack(&mut runner, &[attacker, second], P1);
    block(&mut runner, &[(b1, attacker), (b2, attacker), (b3, second)]);
    settle(&mut runner, &[]);
    assert_eq!(pt(&mut runner, attacker), (3, 3), "exactly one +1/+1");
    assert_eq!(pt(&mut runner, second), (3, 3), "exactly one +1/+1");
}

/// CR 509.3d (qualified form): one firing for each matching blocker, binding
/// that blocker. Synthetic delayed text shaped like Zombie Boa's (whose own
/// "creature of that color" qualifier is not parsed; no Boa support claimed).
#[test]
fn qualified_becomes_blocked_fires_once_per_matching_blocker() {
    let mut scenario = board();
    let attacker = scenario.add_creature(P0, "Attacker", 5, 5).id();
    let w1 = scenario
        .add_creature(P1, "White A", 1, 1)
        .with_color(vec![ManaColor::White])
        .id();
    let w2 = scenario
        .add_creature(P1, "White B", 1, 1)
        .with_color(vec![ManaColor::White])
        .id();
    let red = scenario
        .add_creature(P1, "Red", 1, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let spell = free_spell(
        &mut scenario,
        P0,
        "Synthetic Boa Ward",
        false,
        "Until end of turn, whenever a creature becomes blocked by a white creature, destroy that creature.",
    );
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    attack(&mut runner, &[attacker], P1);
    block(
        &mut runner,
        &[(w1, attacker), (w2, attacker), (red, attacker)],
    );
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, w1), Zone::Graveyard);
    assert_eq!(zone(&runner, w2), Zone::Graveyard);
    assert_eq!(zone(&runner, red), Zone::Battlefield, "not white");
    assert_eq!(
        zone(&runner, attacker),
        Zone::Battlefield,
        "not the attacker"
    );
}

/// Tasha +1: each creature attacking P0 gets a -1/-1 counter (on P1's turn,
/// within "until your next turn").
#[test]
fn tasha_puts_a_counter_on_each_attacker() {
    let mut scenario = board();
    let tasha = scenario
        .add_planeswalker_from_oracle(P0, "Tasha, Unholy Archmage", "Tasha", 4, TASHA)
        .as_legendary()
        .id();
    let a1 = scenario.add_creature(P1, "Attacker A", 3, 3).id();
    let a2 = scenario.add_creature(P1, "Attacker B", 3, 3).id();
    let mut runner = scenario.build();
    runner.activate(tasha, 0).resolve();
    attack(&mut runner, &[a1, a2], P0);
    settle(&mut runner, &[]);
    for (id, name) in [(a1, "A"), (a2, "B")] {
        assert_eq!(
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Minus1Minus1),
            Some(&1),
            "Attacker {name} has one -1/-1 counter"
        );
    }
}

/// First Day of Class: each creature that enters gets the counter and haste.
#[test]
fn first_day_of_class_marks_each_entering_creature() {
    let mut scenario = board();
    let class = free_spell(
        &mut scenario,
        P0,
        "First Day of Class",
        true,
        FIRST_DAY_OF_CLASS,
    );
    let t1 = free_spell(
        &mut scenario,
        P0,
        "Token A",
        true,
        "Create a 1/1 green Saproling creature token.",
    );
    let t2 = free_spell(
        &mut scenario,
        P0,
        "Token B",
        true,
        "Create a 1/1 green Saproling creature token.",
    );
    let mut runner = scenario.build();
    runner.cast(class).resolve();
    settle(&mut runner, &[]);
    runner.cast(t1).resolve();
    settle(&mut runner, &[]);
    runner.cast(t2).resolve();
    settle(&mut runner, &[]);
    let saprolings: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.name == "Saproling")
        .map(|o| o.id)
        .collect();
    assert_eq!(saprolings.len(), 2, "reach guard");
    for id in saprolings {
        assert_eq!(
            pt(&mut runner, id),
            (2, 2),
            "each Saproling has the counter"
        );
        assert!(
            runner.state().objects[&id]
                .keywords
                .contains(&engine::types::keywords::Keyword::Haste),
            "each Saproling has haste"
        );
    }
}

/// Don't Move: a creature that becomes tapped is destroyed (it, not the spell).
#[test]
fn dont_move_destroys_the_creature_that_becomes_tapped() {
    let mut scenario = board();
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
    let dont_move = free_spell(&mut scenario, P0, "Don't Move", false, DONT_MOVE);
    let tap = free_spell(&mut scenario, P0, "Tap It", true, "Tap target creature.");
    let mut runner = scenario.build();
    runner.cast(dont_move).resolve();
    runner.cast(tap).target_object(victim).resolve();
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, victim), Zone::Graveyard);
    assert_eq!(zone(&runner, bystander), Zone::Battlefield);
}

/// Shriveling Rot (first mode): the creature dealt damage is destroyed, not the
/// creature that dealt it (CR 120.1: "it" names the recipient).
#[test]
fn shriveling_rot_destroys_the_damaged_creature_not_the_dealer() {
    let mut scenario = board();
    let pyromancer = scenario
        .add_creature_from_oracle(P0, "Prodigal Pyromancer", 1, 1, PYROMANCER)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 3, 3).id();
    let rot = free_spell(&mut scenario, P0, "Shriveling Rot", true, SHRIVELING_ROT);
    let mut runner = scenario.build();
    runner.cast(rot).modes(&[0]).resolve();
    runner
        .activate(pyromancer, 0)
        .target_object(victim)
        .resolve();
    settle(&mut runner, &[]);
    assert_eq!(
        zone(&runner, victim),
        Zone::Graveyard,
        "the damaged creature"
    );
    assert_eq!(
        zone(&runner, pyromancer),
        Zone::Battlefield,
        "not the dealer"
    );
}

/// CR 120.4b + CR 608.2c: a batched ("one or more") damage trigger's "that
/// much" is the damage dealt, not the number of creatures dealt damage. A
/// Lightning Bolt dealing 3 to one creature gains 3 life, from both a delayed
/// trigger and the same printed trigger (synthetic text). The matching-subject
/// count stays a headcount where the event has no magnitude (the Basri Ket row).
#[test]
fn batched_damage_trigger_that_much_reads_the_damage_not_the_headcount() {
    const DELAYED: &str = "Until end of turn, whenever one or more creatures you control are dealt damage, you gain that much life.";
    const PRINTED: &str =
        "Whenever one or more creatures you control are dealt damage, you gain that much life.";
    const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
    for printed in [false, true] {
        let label = if printed { "printed" } else { "delayed" };
        let mut scenario = board();
        let wall = scenario.add_creature(P0, "Sturdy Wall", 2, 5).id();
        let spell = if printed {
            scenario.add_enchantment_from_oracle(P0, "Synthetic Mending", PRINTED);
            None
        } else {
            Some(free_spell(
                &mut scenario,
                P0,
                "Synthetic Mending",
                true,
                DELAYED,
            ))
        };
        let bolt = free_spell(&mut scenario, P0, "Lightning Bolt", true, BOLT);
        let mut runner = scenario.build();
        if let Some(spell) = spell {
            runner.cast(spell).resolve();
            assert_eq!(
                runner.state().delayed_triggers.len(),
                1,
                "[{label}] reach guard"
            );
        }
        let life = runner.life(P0);
        runner.cast(bolt).target_object(wall).resolve();
        settle(&mut runner, &[]);
        assert_eq!(
            runner.state().objects[&wall].damage_marked,
            3,
            "[{label}] reach guard: the Bolt dealt 3"
        );
        assert_eq!(runner.life(P0), life + 3, "[{label}] that much = 3 damage");
    }
}

/// False Cure: "that player loses 2 life for each 1 life they gained." The
/// player who gained life loses twice what they gained (CR 119.3: +2 → −4),
/// in either orientation; the other player is unaffected.
#[test]
fn false_cure_takes_twice_the_gain_from_the_player_who_gained() {
    for gainer in [P1, P0] {
        let other = if gainer == P0 { P1 } else { P0 };
        let mut scenario = board();
        let cure = free_spell(&mut scenario, P0, "False Cure", true, FALSE_CURE);
        let gift = free_spell(
            &mut scenario,
            P0,
            "Gift",
            true,
            "Target player gains 2 life.",
        );
        let mut runner = scenario.build();
        runner.cast(cure).resolve();
        let gift_outcome = runner.cast(gift).target_player(gainer).resolve();
        assert!(
            gift_outcome.events().iter().any(|e| matches!(
                e,
                engine::types::events::GameEvent::LifeChanged { player_id, amount: 2, .. }
                    if *player_id == gainer
            )),
            "reach guard: {gainer:?} gained 2 life"
        );
        settle(&mut runner, &[]);
        assert_eq!(runner.life(gainer), 20 + 2 - 4, "{gainer:?}: +2 then -4");
        assert_eq!(runner.life(other), 20, "{other:?} is unaffected");
    }
}
/// The Mirari Conjecture III: each instant cast is copied once.
#[test]
fn mirari_conjecture_copies_each_instant_once() {
    let mut scenario = board();
    let saga = scenario
        .add_enchantment_from_oracle(P0, "The Mirari Conjecture", "")
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(MIRARI)
        .id();
    scenario.with_counter(saga, CounterType::Lore, 2);
    let gain = free_spell(&mut scenario, P0, "Gain", true, "You gain 1 life.");
    let mut runner = scenario.build();
    advance_sagas(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: chapter III"
    );
    let life = runner.life(P0);
    runner.cast(gain).commit();
    settle(&mut runner, &[]);
    assert_eq!(runner.life(P0), life + 2, "the spell and one copy");
}

const ZOMBIE_BOA: &str = "{1}{B}: Choose a color. Whenever this creature becomes blocked by a creature of that color this turn, destroy that creature. Activate only as a sorcery.";

/// Zombie Boa activated once per color in `colors` (P0's main), then attacks;
/// P1 blocks with `blockers`. Returns the runner after the blocks are declared
/// and the firing count collected, before any firing resolves.
fn boa_board(
    colors: &[&str],
    blockers: &[(&str, ManaColor)],
) -> (GameRunner, ObjectId, Vec<ObjectId>, usize) {
    let mut scenario = board();
    let boa = scenario
        .add_creature_from_oracle(P0, "Zombie Boa", 3, 3, ZOMBIE_BOA)
        .with_subtypes(vec!["Zombie", "Snake"])
        .id();
    scenario.with_mana_pool(
        P0,
        (0..colors.len() * 2)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Black,
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let blocker_ids: Vec<ObjectId> = blockers
        .iter()
        .map(|(name, color)| {
            scenario
                .add_creature(P1, name, 1, 1)
                .with_color(vec![*color])
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for color in colors {
        runner.activate(boa, 0).choose_option(color).resolve();
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        colors.len(),
        "reach guard: one installed generator per activation"
    );
    attack(&mut runner, &[boa], P1);
    let assignments: Vec<(ObjectId, ObjectId)> = blocker_ids.iter().map(|&b| (b, boa)).collect();
    block(&mut runner, &assignments);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    let firings = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == boa)
        .count();
    (runner, boa, blocker_ids, firings)
}

/// CR 509.3d + CR 105.4: one firing per blocker of the chosen color; the
/// blocker is destroyed, not Boa or a blocker of another color.
#[test]
fn zombie_boa_destroys_each_blocker_of_the_chosen_color() {
    let (mut runner, boa, blockers, firings) = boa_board(
        &["White"],
        &[
            ("White Blocker", ManaColor::White),
            ("Red Blocker", ManaColor::Red),
        ],
    );
    assert_eq!(firings, 1, "one firing: one white blocker");
    settle(&mut runner, &[]);
    assert_eq!(
        zone(&runner, blockers[0]),
        Zone::Graveyard,
        "the white blocker"
    );
    assert_eq!(zone(&runner, blockers[1]), Zone::Battlefield, "not red");
    assert_eq!(zone(&runner, boa), Zone::Battlefield, "not Boa");
}

/// Each generator keeps the color chosen when it was created: white, then red,
/// destroys the white blocker and the red blocker, each by its own generator.
#[test]
fn zombie_boa_generators_keep_their_own_chosen_color() {
    let (mut runner, boa, blockers, firings) = boa_board(
        &["White", "Red"],
        &[
            ("White Blocker", ManaColor::White),
            ("Red Blocker", ManaColor::Red),
        ],
    );
    assert_eq!(
        firings, 2,
        "one firing per generator, each for its own color"
    );
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, blockers[0]), Zone::Graveyard);
    assert_eq!(zone(&runner, blockers[1]), Zone::Graveyard);
    assert_eq!(zone(&runner, boa), Zone::Battlefield);
}

/// Two white generators and one white blocker: two firings are collected
/// before either resolves; the blocker is destroyed once and the second firing
/// finds nothing to destroy.
#[test]
fn zombie_boa_two_white_generators_fire_twice_for_one_white_blocker() {
    let (mut runner, boa, blockers, firings) =
        boa_board(&["White", "White"], &[("White Blocker", ManaColor::White)]);
    assert_eq!(firings, 2, "two generators, two firings");
    let graveyard_before = runner.state().players[1].graveyard.len();
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, blockers[0]), Zone::Graveyard);
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        graveyard_before + 1,
        "destroyed once"
    );
    assert_eq!(zone(&runner, boa), Zone::Battlefield);
}

/// CR 603.7b: a generator lasts "this turn"; on Boa's next turn a white
/// blocker is not destroyed.
#[test]
fn zombie_boa_generator_expires_at_end_of_turn() {
    let mut scenario = board();
    let boa = scenario
        .add_creature_from_oracle(P0, "Zombie Boa", 3, 3, ZOMBIE_BOA)
        .with_subtypes(vec!["Zombie", "Snake"])
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Black,
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let white = scenario
        .add_creature(P1, "White Blocker", 1, 1)
        .with_color(vec![ManaColor::White])
        .id();
    let mut runner = scenario.build();
    runner.activate(boa, 0).choose_option("White").resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1, "reach guard");
    let turn = runner.state().turn_number;
    drive_until(&mut runner, |r| {
        r.state().turn_number >= turn + 2 && r.state().phase == Phase::PreCombatMain
    });
    assert!(runner.state().delayed_triggers.is_empty(), "expired");
    attack(&mut runner, &[boa], P1);
    block(&mut runner, &[(white, boa)]);
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, white), Zone::Battlefield);
}

// ---- PLAN-m3 §C: condition timing in the shared firing authority ----

const LAST_RONIN: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\n\
I \u{2014} Destroy all creatures.\n\
II \u{2014} Mill four cards. When you do, return target creature card from your graveyard to your hand.\n\
III \u{2014} Whenever a creature you control attacks alone this turn, put three +1/+1 counters on it. It gains trample, lifelink, and indestructible until end of turn.";
const RECKLESS_BLAZE: &str = "Reckless Blaze deals 5 damage to each creature. Whenever a creature you control dealt damage this way dies this turn, add {R}.";
const PYROCLASM: &str = "Pyroclasm deals 2 damage to each creature.";
const ENTER_ATTACKING: &str =
    "Create a 1/1 white Soldier creature token that's tapped and attacking.";

/// Pass priority until the top stack item has resolved.
fn resolve_top(runner: &mut GameRunner) {
    let top = runner.state().stack.last().expect("a stack item").id;
    for _ in 0..16 {
        if !runner.state().stack.iter().any(|entry| entry.id == top) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }
    panic!("the top item never resolved");
}

/// Put the Last Ronin on chapter III (installed on P0's turn 2) with the given
/// P0 attackers, and declare them at P1. Returns the attackers' ids.
fn last_ronin_attack(
    attackers: usize,
    extra: impl FnOnce(&mut GameScenario),
) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = board();
    let saga = scenario
        .add_enchantment_from_oracle(P0, "The Last Ronin", "")
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(LAST_RONIN)
        .id();
    scenario.with_counter(saga, CounterType::Lore, 2);
    let ids: Vec<ObjectId> = (0..attackers)
        .map(|i| {
            scenario
                .add_creature(P0, &format!("Ronin Attacker {i}"), 2, 2)
                .id()
        })
        .collect();
    extra(&mut scenario);
    let mut runner = scenario.build();
    advance_sagas(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: chapter III installed"
    );
    attack(&mut runner, &ids, P1);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    (runner, ids)
}

fn library(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// CR 506.5 + CR 603.4: The Last Ronin III ("attacks alone") is a head
/// qualifier on the delayed definition, decided at admission. One attacker
/// fires; two attackers fire nothing (the collector used to ignore the head
/// condition and fire for each attacker).
#[test]
fn last_ronin_fires_only_for_a_creature_attacking_alone() {
    let (mut runner, ids) = last_ronin_attack(1, |_| {});
    assert_eq!(runner.state().stack.len(), 1, "reach guard: it triggered");
    settle(&mut runner, &[]);
    assert_eq!(plus_one_counters(&runner, ids[0]), 3, "attacking alone");

    let (mut runner, ids) = last_ronin_attack(2, |_| {});
    assert!(runner.state().stack.is_empty(), "two attackers: no firing");
    settle(&mut runner, &[]);
    for id in ids {
        assert_eq!(plus_one_counters(&runner, id), 0, "not attacking alone");
    }
}

/// CR 608.2c + CR 611.2a: The Last Ronin III's second sentence ("It gains
/// trample, lifelink, and indestructible until end of turn.") is part of the
/// delayed trigger's effect and "it" is the creature attacking alone, not the
/// Saga. The attacker gains all three; a creature that didn't attack doesn't;
/// the keywords end with the turn.
#[test]
fn last_ronin_grants_its_keywords_to_the_lone_attacker_until_end_of_turn() {
    use engine::types::keywords::Keyword;
    let mut bystander = None;
    let (mut runner, ids) = last_ronin_attack(1, |scenario| {
        bystander = Some(scenario.add_creature(P0, "Bystander", 2, 2).id());
    });
    let bystander = bystander.expect("bystander");
    settle(&mut runner, &[]);
    let keywords = |r: &mut GameRunner, id: ObjectId| {
        r.state_mut().layers_dirty.mark_full();
        engine::game::layers::evaluate_layers(r.state_mut());
        let object = &r.state().objects[&id];
        [Keyword::Trample, Keyword::Lifelink, Keyword::Indestructible]
            .map(|keyword| object.keywords.contains(&keyword))
    };
    assert_eq!(
        plus_one_counters(&runner, ids[0]),
        3,
        "reach guard: counters"
    );
    assert_eq!(
        keywords(&mut runner, ids[0]),
        [true; 3],
        "the lone attacker"
    );
    assert_eq!(
        keywords(&mut runner, bystander),
        [false; 3],
        "not the bystander"
    );
    let saga_keywords = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "The Last Ronin")
        .map(|o| o.keywords.contains(&Keyword::Indestructible));
    assert_ne!(saga_keywords, Some(true), "not the Saga");
    drive_until(&mut runner, |r| r.state().turn_number > 2);
    assert_eq!(
        keywords(&mut runner, ids[0]),
        [false; 3],
        "until end of turn"
    );
}

/// CR 506.5 + CR 508.4 + CR 603.4: "attacks alone" is part of the trigger
/// event, not an intervening-if, so it is not rechecked on resolution. A
/// creature put onto the battlefield attacking after the trigger fired (it was
/// never declared as an attacker) does not stop the trigger from resolving.
#[test]
fn last_ronin_resolves_after_a_creature_enters_attacking() {
    let (mut runner, ids) = last_ronin_attack(1, |scenario| {
        free_spell(
            scenario,
            P0,
            "Synthetic Reinforcement",
            true,
            ENTER_ATTACKING,
        );
    });
    assert_eq!(runner.state().stack.len(), 1, "reach guard: it triggered");
    let reinforcement = runner.state().players[0]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Synthetic Reinforcement")
        .expect("the reinforcement is in hand");
    runner.cast(reinforcement).commit();
    resolve_top(&mut runner);
    assert_eq!(
        runner.state().combat.as_ref().map(|c| c.attackers.len()),
        Some(2),
        "reach guard: a second creature is now attacking"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the Ronin trigger still waits"
    );
    settle(&mut runner, &[]);
    assert_eq!(plus_one_counters(&runner, ids[0]), 3, "it still resolves");
}

/// CR 603.4: a delayed body intervening-if ("if you control an artifact") is
/// checked when the trigger would fire AND rechecked on resolution, alongside
/// the "attacks alone" head qualifier. Synthetic delayed text.
#[test]
fn delayed_attacks_alone_with_a_body_if_checks_the_body_twice() {
    const TEXT: &str = "Until end of turn, whenever a creature you control attacks alone, if you control an artifact, draw a card.";
    const SHATTER: &str = "Destroy target artifact.";
    #[derive(Debug, Clone, Copy)]
    enum Artifact {
        Kept,
        DestroyedInResponse,
        Absent,
    }
    for case in [
        Artifact::Kept,
        Artifact::DestroyedInResponse,
        Artifact::Absent,
    ] {
        let mut scenario = board();
        let attacker = scenario.add_creature(P0, "Lone Attacker", 2, 2).id();
        let relic = match case {
            Artifact::Absent => None,
            _ => Some(scenario.add_artifact_from_oracle(P0, "Relic", "").id()),
        };
        let spell = free_spell(&mut scenario, P0, "Synthetic Vigil", false, TEXT);
        let shatter = free_spell(&mut scenario, P0, "Shatter", true, SHATTER);
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        let library_before = library(&runner, P0);
        attack(&mut runner, &[attacker], P1);
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
        }
        match case {
            Artifact::Absent => {
                assert!(
                    runner.state().stack.is_empty(),
                    "{case:?}: a false body-if at fire time: no trigger"
                );
            }
            Artifact::Kept => {
                assert_eq!(runner.state().stack.len(), 1, "{case:?}: it triggered");
            }
            Artifact::DestroyedInResponse => {
                assert_eq!(runner.state().stack.len(), 1, "{case:?}: it triggered");
                runner
                    .cast(shatter)
                    .target_object(relic.expect("artifact"))
                    .commit();
                resolve_top(&mut runner);
                assert_eq!(zone(&runner, relic.unwrap()), Zone::Graveyard);
            }
        }
        settle(&mut runner, &[]);
        let expected = match case {
            Artifact::Kept => 1,
            Artifact::DestroyedInResponse | Artifact::Absent => 0,
        };
        assert_eq!(
            library_before - library(&runner, P0),
            expected,
            "{case:?}: cards drawn"
        );
    }
}

/// CR 508.5 + CR 603.4: an Attacks trigger's intervening-if reads each
/// narrowed attack, not the whole declaration. "If it's a Wizard" with a Wizard
/// and a Bear attacking draws exactly one card. Evaluated on the declaration,
/// "it" names no single attacker and nothing would be drawn. Printed synthetic
/// enchantment, through the authority the delayed collector shares.
#[test]
fn attack_intervening_if_reads_each_narrowed_attacker() {
    const TEXT: &str = "Whenever a creature attacks, if it's a Wizard, draw a card.";
    let mut scenario = board();
    scenario.add_enchantment_from_oracle(P0, "Synthetic Academy", TEXT);
    let wizard = typed(&mut scenario, P0, "Wizard", "Wizard");
    let bear = typed(&mut scenario, P0, "Bear", "Bear");
    let mut runner = scenario.build();
    let drawn = hand(&runner, P0);
    attack(&mut runner, &[wizard, bear], P1);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    assert_eq!(runner.state().stack.len(), 1, "only the Wizard's firing");
    settle(&mut runner, &[]);
    assert_eq!(hand(&runner, P0), drawn + 1);
}

fn red_mana(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize]
        .mana_pool
        .count_color(engine::types::mana::ManaType::Red)
}

/// CR 603.2 + CR 603.7b: Reckless Blaze's "a creature you control dealt
/// damage this way dies" is decided at admission. P0's Bear dealt damage by
/// the Blaze adds {R}; P1's creature isn't P0's; a token that entered after
/// the Blaze and is then destroyed was not dealt damage this way.
#[test]
fn reckless_blaze_adds_mana_only_for_a_creature_it_damaged() {
    let mut scenario = board();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Their Bear", 2, 2).id();
    let blaze = free_spell(&mut scenario, P0, "Reckless Blaze", false, RECKLESS_BLAZE);
    let token = free_spell(
        &mut scenario,
        P0,
        "Token Maker",
        true,
        "Create a 2/2 black Zombie creature token.",
    );
    let murder = free_spell(
        &mut scenario,
        P0,
        "Murder",
        true,
        "Destroy target creature.",
    );
    let mut runner = scenario.build();
    runner.cast(blaze).resolve();
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, bear), Zone::Graveyard, "reach guard");
    assert_eq!(zone(&runner, theirs), Zone::Graveyard, "reach guard");
    assert_eq!(red_mana(&runner, P0), 1, "only P0's Bear");
    runner.cast(token).resolve();
    settle(&mut runner, &[]);
    let zombie = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Zombie")
        .expect("reach guard: the Zombie entered");
    runner.cast(murder).target_object(zombie).resolve();
    settle(&mut runner, &[]);
    assert!(
        !runner.state().battlefield.contains(&zombie),
        "reach guard: the Zombie died"
    );
    assert_eq!(
        red_mana(&runner, P0),
        1,
        "the Zombie wasn't dealt damage this way"
    );
}

// ---- PLAN-m2 §4: the delayed batch reuses the printed partition ----

const MENDING_DELAYED: &str = "Until end of turn, whenever one or more creatures you control are dealt damage, you gain that much life.";
const MENDING_PRINTED: &str =
    "Whenever one or more creatures you control are dealt damage, you gain that much life.";

/// CR 603.2c + CR 120.4b: a batched damage trigger sees the whole matching
/// batch. Pyroclasm deals 2 to each of P0's two walls and to P1's wall: +4
/// from both the delayed and the printed trigger, the opposing wall excluded.
/// Control: only P1 has a wall, so nothing matches. Synthetic text.
#[test]
fn pyroclasm_batched_damage_reads_the_whole_matching_batch() {
    for printed in [false, true] {
        for own_walls in [2usize, 0] {
            let label = format!(
                "{} / {own_walls} own walls",
                if printed { "printed" } else { "delayed" }
            );
            let mut scenario = board();
            for i in 0..own_walls {
                scenario.add_creature(P0, &format!("Own Wall {i}"), 0, 4);
            }
            scenario.add_creature(P1, "Their Wall", 0, 4);
            let mending = if printed {
                scenario.add_enchantment_from_oracle(P0, "Synthetic Mending", MENDING_PRINTED);
                None
            } else {
                Some(free_spell(
                    &mut scenario,
                    P0,
                    "Synthetic Mending",
                    true,
                    MENDING_DELAYED,
                ))
            };
            let pyroclasm = free_spell(&mut scenario, P0, "Pyroclasm", false, PYROCLASM);
            let mut runner = scenario.build();
            if let Some(mending) = mending {
                runner.cast(mending).resolve();
            }
            let life = runner.life(P0);
            runner.cast(pyroclasm).commit();
            resolve_top(&mut runner);
            if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            assert_eq!(
                runner.state().stack.len(),
                usize::from(own_walls > 0),
                "[{label}] exactly one firing for the whole batch, none without a match"
            );
            settle(&mut runner, &[]);
            let expected = 2 * own_walls as i32;
            assert_eq!(runner.life(P0), life + expected, "[{label}]");
        }
    }
}

/// Two installed generators each read the whole batch once: +8.
#[test]
fn two_batched_damage_generators_each_fire_once() {
    let mut scenario = board();
    scenario.add_creature(P0, "Own Wall A", 0, 4);
    scenario.add_creature(P0, "Own Wall B", 0, 4);
    let first = free_spell(&mut scenario, P0, "Mending A", true, MENDING_DELAYED);
    let second = free_spell(&mut scenario, P0, "Mending B", true, MENDING_DELAYED);
    let pyroclasm = free_spell(&mut scenario, P0, "Pyroclasm", false, PYROCLASM);
    let mut runner = scenario.build();
    runner.cast(first).resolve();
    runner.cast(second).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 2, "reach guard");
    let life = runner.life(P0);
    runner.cast(pyroclasm).commit();
    resolve_top(&mut runner);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    assert_eq!(runner.state().stack.len(), 2, "one firing per generator");
    settle(&mut runner, &[]);
    assert_eq!(runner.life(P0), life + 8);
}

/// CR 603.2c + CR 603.7b: a multi-fire delayed trigger fires for every
/// matching event in one batch, not only the first. Real Shriveling Rot (mode
/// two) + Pyroclasm: a P0 2/2 and a P1 2/2 die, and each controller loses 2.
#[test]
fn shriveling_rot_drains_every_dying_creatures_controller() {
    let mut scenario = board();
    let own = scenario.add_creature(P0, "Own Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Their Bear", 2, 2).id();
    let rot = free_spell(&mut scenario, P0, "Shriveling Rot", true, SHRIVELING_ROT);
    let pyroclasm = free_spell(&mut scenario, P0, "Pyroclasm", false, PYROCLASM);
    let mut runner = scenario.build();
    runner.cast(rot).modes(&[1]).resolve();
    runner.cast(pyroclasm).resolve();
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, own), Zone::Graveyard, "reach guard");
    assert_eq!(zone(&runner, theirs), Zone::Graveyard, "reach guard");
    assert_eq!(runner.life(P0), 18, "P0's creature died");
    assert_eq!(runner.life(P1), 18, "P1's creature died");
}

/// Non-batched: "whenever a creature dies" draws once per death (two).
/// Batched: "whenever one or more creatures die" draws once, and its raw
/// members are consumed, so a later priority pass doesn't fire it again.
/// Synthetic text.
#[test]
fn delayed_death_triggers_fire_per_death_or_once_per_batch() {
    for (text, expected) in [
        (
            "Until end of turn, whenever a creature dies, draw a card.",
            2,
        ),
        (
            "Until end of turn, whenever one or more creatures die, draw a card.",
            1,
        ),
    ] {
        let mut scenario = board();
        scenario.add_creature(P0, "Bear A", 2, 2);
        scenario.add_creature(P1, "Bear B", 2, 2);
        let watch = free_spell(&mut scenario, P0, "Synthetic Watch", true, text);
        let pyroclasm = free_spell(&mut scenario, P0, "Pyroclasm", false, PYROCLASM);
        let mut runner = scenario.build();
        runner.cast(watch).resolve();
        let library_before = library(&runner, P0);
        runner.cast(pyroclasm).resolve();
        settle(&mut runner, &[]);
        assert_eq!(library_before - library(&runner, P0), expected, "{text}");
        drive_until(&mut runner, |r| r.state().phase == Phase::End);
        assert_eq!(
            library_before - library(&runner, P0),
            expected,
            "{text}: no re-fire"
        );
    }
}

/// CR 603.2c: two equal-looking damage events in one batch are two
/// occurrences, each consumed by its own ordinal. A synthetic spell deals 1
/// damage to the same creature twice; "whenever a creature is dealt damage,
/// you gain 1 life" gains 2, and never again.
#[test]
fn equal_looking_damage_events_are_separate_occurrences() {
    let mut scenario = board();
    let target = scenario.add_creature(P1, "Target", 0, 5).id();
    let watch = free_spell(
        &mut scenario,
        P0,
        "Synthetic Watch",
        true,
        "Until end of turn, whenever a creature is dealt damage, you gain 1 life.",
    );
    let twin = free_spell(
        &mut scenario,
        P0,
        "Twin Ping",
        true,
        "Twin Ping deals 1 damage to target creature. Twin Ping deals 1 damage to that creature.",
    );
    let mut runner = scenario.build();
    runner.cast(watch).resolve();
    let life = runner.life(P0);
    runner.cast(twin).target_object(target).resolve();
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        2,
        "reach guard: two damage events"
    );
    settle(&mut runner, &[]);
    assert_eq!(runner.life(P0), life + 2);
    drive_until(&mut runner, |r| r.state().phase == Phase::End);
    assert_eq!(runner.life(P0), life + 2, "no re-fire");
}

/// CR 105.4 + CR 608.2c: "that color" names the resolution's color choice
/// even when another kind of choice (a number) comes between it and the
/// generator. Synthetic Boa-shaped text: White, then 3, then one generator
/// that destroys the white blocker only.
#[test]
fn boa_color_survives_an_intervening_number_choice() {
    const TEXT: &str = "{0}: Choose a color. Choose a number between 1 and 3. Whenever this creature becomes blocked by a creature of that color this turn, destroy that creature.";
    let mut scenario = board();
    let boa = scenario
        .add_creature_from_oracle(P0, "Synthetic Boa", 3, 3, TEXT)
        .id();
    let white = scenario
        .add_creature(P1, "White Blocker", 1, 1)
        .with_color(vec![ManaColor::White])
        .id();
    let red = scenario
        .add_creature(P1, "Red Blocker", 1, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: boa,
            ability_index: 0,
        })
        .expect("activate");
    let mut answers = vec!["White", "3"].into_iter();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => {
                let choice = answers.next().expect("only two choices").to_string();
                runner
                    .act(GameAction::ChooseOption { choice })
                    .expect("answer the choice");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => break,
        }
    }
    assert!(
        answers.next().is_none(),
        "reach guard: both choices were asked"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: one generator"
    );
    attack(&mut runner, &[boa], P1);
    block(&mut runner, &[(white, boa), (red, boa)]);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    let firings = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == boa)
        .count();
    assert_eq!(firings, 1, "one firing: the white blocker");
    settle(&mut runner, &[]);
    assert_eq!(zone(&runner, white), Zone::Graveyard);
    assert_eq!(zone(&runner, red), Zone::Battlefield);
}

// ---- PLAN-m3 §D: per-player delayed damage, three players ----

const JACE_CUNNING_CASTAWAY: &str = "+1: Whenever one or more creatures you control deal combat damage to a player this turn, draw a card, then discard a card.\n\u{2212}2: Create a 2/2 blue Illusion creature token with \"When this token becomes the target of a spell, sacrifice it.\"\n\u{2212}5: Create two tokens that are copies of Jace, except they're not legendary.";
const P2: PlayerId = PlayerId(2);

fn three_player_board() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(3, 9656);
    scenario.at_phase(Phase::PreCombatMain);
    let names: Vec<String> = (0..10).map(|i| format!("P0 Card {i}")).collect();
    scenario.with_library_top(P0, &names.iter().map(String::as_str).collect::<Vec<_>>());
    scenario
}

/// Pass priority (no attackers declared yet) until P0 may declare attackers,
/// then declare `attacks`, decline every block, and stop once `source` has a
/// firing on the stack or combat is over.
fn declare_and_collect(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
    source: ObjectId,
) -> usize {
    drive_until(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    runner
        .declare_attackers(attacks)
        .expect("declare attackers");
    for _ in 0..64 {
        let fired = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == source)
            .count();
        if fired > 0 {
            return fired;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blockers");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::PostCombatMain => {
                return 0;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }
    panic!("combat never finished");
}

/// Resolve the stack, discarding the first card offered at each discard prompt.
fn resolve_discarding(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DiscardChoice { cards, count, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.into_iter().take(count).collect(),
                    })
                    .expect("discard");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("stack did not settle");
}

/// CR 603.2c + Jace, Cunning Castaway ruling (2017-09-29): the +1's delayed
/// trigger fires once for EACH player dealt combat damage. Three players; P0's
/// creatures hit P1 and P2 → two firings, two draws and two discards. Control:
/// both hit P1 → one firing.
#[test]
fn jace_cunning_castaway_fires_once_per_damaged_player() {
    for (defenders, expected) in [([P1, P2], 2usize), ([P1, P1], 1)] {
        let mut scenario = three_player_board();
        let jace = scenario
            .add_planeswalker_from_oracle(
                P0,
                "Jace, Cunning Castaway",
                "Jace",
                3,
                JACE_CUNNING_CASTAWAY,
            )
            .as_legendary()
            .id();
        let a1 = scenario.add_creature(P0, "Attacker A", 2, 2).id();
        let a2 = scenario.add_creature(P0, "Attacker B", 2, 2).id();
        let mut runner = scenario.build();
        runner.activate(jace, 0).resolve();
        assert_eq!(runner.state().delayed_triggers.len(), 1, "reach guard");
        let library_before = library(&runner, P0);
        let graveyard = runner.state().players[0].graveyard.len();
        let fired = declare_and_collect(
            &mut runner,
            &[
                (a1, AttackTarget::Player(defenders[0])),
                (a2, AttackTarget::Player(defenders[1])),
            ],
            jace,
        );
        assert_eq!(fired, expected, "{defenders:?}: firings");
        resolve_discarding(&mut runner);
        assert_eq!(
            library_before - library(&runner, P0),
            expected,
            "{defenders:?}: draws"
        );
        assert_eq!(
            runner.state().players[0].graveyard.len() - graveyard,
            expected,
            "{defenders:?}: discards"
        );
    }
}

/// CR 603.2c + CR 608.2c: Garruk −4 in a three-player game. One attacker at P2
/// (an opponent) and one at P1's planeswalker: one firing, and only the
/// creature attacking an opponent is narrowed in (+2/+2 and trample).
#[test]
fn garruk_narrows_to_the_creature_attacking_an_opponent() {
    let mut scenario = three_player_board();
    let garruk = scenario
        .add_planeswalker_from_oracle(P0, "Garruk, Curse Breaker", "Garruk", 5, GARRUK)
        .as_legendary()
        .id();
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Opposing Walker", "Walker", 5, "+1: You gain 1 life.")
        .id();
    let at_player = scenario.add_creature(P0, "At Player", 2, 2).id();
    let at_walker = scenario.add_creature(P0, "At Walker", 2, 2).id();
    let mut runner = scenario.build();
    runner.activate(garruk, 2).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1, "reach guard");
    let fired = declare_and_collect(
        &mut runner,
        &[
            (at_player, AttackTarget::Player(P2)),
            (at_walker, AttackTarget::Planeswalker(walker)),
        ],
        garruk,
    );
    assert_eq!(fired, 1, "one firing");
    settle(&mut runner, &[]);
    assert_eq!(pt(&mut runner, at_player), (4, 4), "attacking an opponent");
    assert_eq!(
        pt(&mut runner, at_walker),
        (2, 2),
        "attacking a planeswalker"
    );
    assert!(runner.state().objects[&at_player]
        .keywords
        .contains(&engine::types::keywords::Keyword::Trample));
    assert!(!runner.state().objects[&at_walker]
        .keywords
        .contains(&engine::types::keywords::Keyword::Trample));
}

/// CR 605.3a + CR 605.3b + CR 608.2c: a mana ability activated to pay a cost
/// mid-resolution resolves inline and is not a new resolution, so the outer
/// "that color" survives it, even when the mana ability has a non-mana
/// clause of its own. Synthetic Boa-shaped text; the {1} is paid by tapping a
/// "{T}: Add {C}. You gain 1 life." artifact. Control: the same board with the
/// cost paid from a floating {C}.
#[test]
fn boa_color_survives_an_inline_mana_ability_paying_its_cost() {
    const TEXT: &str = "{0}: Choose a color. You may pay {1}. Whenever this creature becomes blocked by a creature of that color this turn, destroy that creature.";
    for floated in [false, true] {
        let mut scenario = board();
        let boa = scenario
            .add_creature_from_oracle(P0, "Synthetic Boa", 3, 3, TEXT)
            .id();
        scenario.add_artifact_from_oracle(P0, "Synthetic Prism", "{T}: Add {C}. You gain 1 life.");
        if floated {
            scenario.with_mana_pool(
                P0,
                vec![engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
        }
        let white = scenario
            .add_creature(P1, "White Blocker", 1, 1)
            .with_color(vec![ManaColor::White])
            .id();
        let red = scenario
            .add_creature(P1, "Red Blocker", 1, 1)
            .with_color(vec![ManaColor::Red])
            .id();
        let mut runner = scenario.build();
        let life = runner.life(P0);
        runner
            .act(GameAction::ActivateAbility {
                source_id: boa,
                ability_index: 0,
            })
            .expect("activate");
        for _ in 0..32 {
            match runner.state().waiting_for.clone() {
                WaitingFor::NamedChoice { .. } => {
                    runner
                        .act(GameAction::ChooseOption {
                            choice: "White".to_string(),
                        })
                        .expect("choose White");
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("pay {1}");
                }
                WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                    runner.act(GameAction::PassPriority).expect("pass priority");
                }
                WaitingFor::Priority { .. } => break,
                other => panic!("unexpected decision: {other:?}"),
            }
        }
        let label = if floated {
            "floated {C}"
        } else {
            "inline mana ability"
        };
        if !floated {
            assert_eq!(runner.life(P0), life + 1, "reach guard: the Prism paid");
        }
        assert_eq!(
            runner.state().delayed_triggers.len(),
            1,
            "[{label}] reach guard: one generator"
        );
        attack(&mut runner, &[boa], P1);
        block(&mut runner, &[(white, boa), (red, boa)]);
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
        }
        let firings = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == boa)
            .count();
        assert_eq!(firings, 1, "[{label}] one firing: the white blocker");
        settle(&mut runner, &[]);
        assert_eq!(zone(&runner, white), Zone::Graveyard, "[{label}]");
        assert_eq!(zone(&runner, red), Zone::Battlefield, "[{label}]");
    }
}

/// CR 603.2c: two installed Jace +1 generators, P1 and P2 dealt combat
/// damage → four firings, each resolving to one draw and one discard. Nothing is collected again afterwards (no re-fire through the end
/// step). The second Jace is a non-legendary copy, as Jace's −5 makes.
#[test]
fn two_jace_generators_fire_once_per_damaged_player_each() {
    let mut scenario = three_player_board();
    let jace = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Jace, Cunning Castaway",
            "Jace",
            3,
            JACE_CUNNING_CASTAWAY,
        )
        .as_legendary()
        .id();
    let copy = scenario
        .add_planeswalker_from_oracle(P0, "Jace Copy", "Jace", 3, JACE_CUNNING_CASTAWAY)
        .id();
    let a1 = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let mut runner = scenario.build();
    runner.activate(jace, 0).resolve();
    runner.activate(copy, 0).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 2, "reach guard");
    let library_before = library(&runner, P0);
    let graveyard = runner.state().players[0].graveyard.len();
    drive_until(&mut runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    runner
        .declare_attackers(&[
            (a1, AttackTarget::Player(PlayerId(1))),
            (a2, AttackTarget::Player(P2)),
        ])
        .expect("declare attackers");
    let fired = loop {
        let fired = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == jace || entry.source_id == copy)
            .count();
        if fired > 0 {
            break fired;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    };
    assert_eq!(fired, 4, "two generators × two damaged players");
    resolve_discarding(&mut runner);
    assert_eq!(library_before - library(&runner, P0), 4, "four draws");
    assert_eq!(
        runner.state().players[0].graveyard.len() - graveyard,
        4,
        "four discards"
    );
    drive_until(&mut runner, |r| r.state().phase == Phase::End);
    assert_eq!(library_before - library(&runner, P0), 4, "no re-fire");
}

// ---- Maintainer round 3, finding 1: one ordering choice per declaration ----

const NEKUSAR: &str = "At the beginning of each player's draw step, that player draws an additional card.\nWhenever an opponent draws a card, Nekusar deals 1 damage to that player.";

#[derive(Debug, Clone, Copy, PartialEq)]
enum FirstToResolve {
    LeviathanDraw,
    RighteousCauseGain,
}

/// P0 controls Summon: Leviathan (chapter II installed) and Righteous Cause;
/// P1 controls Nekusar, the Mindrazer. P0 is at 1 life and attacks with one
/// Octopus. Returns (whether P0 lost the game, P0's life) after P0 orders the
/// declaration's two triggers so that `first` resolves first.
fn leviathan_and_righteous_cause_ordered(first: FirstToResolve) -> (bool, i32) {
    let mut scenario = board();
    add_leviathan(&mut scenario, 1);
    scenario.add_enchantment_from_oracle(P0, "Righteous Cause", RIGHTEOUS_CAUSE);
    scenario
        .add_creature_from_oracle(P1, "Nekusar, the Mindrazer", 2, 4, NEKUSAR)
        .as_legendary();
    let octopus = typed(&mut scenario, P0, "Octopus", "Octopus");
    let mut runner = scenario.build();
    advance_sagas(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: chapter II installed"
    );
    runner.state_mut().players[0].life = 1;

    attack(&mut runner, &[octopus], P1);
    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "the delayed and printed triggers share one ordering choice: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    let index_of = |name: &str| {
        triggers
            .iter()
            .position(|t| t.source_name == name)
            .unwrap_or_else(|| panic!("{name} is in the ordering choice: {triggers:?}"))
    };
    let (draw, gain) = (index_of("Summon: Leviathan"), index_of("Righteous Cause"));
    assert_eq!(triggers.len(), 2, "exactly the two triggers: {triggers:?}");
    // Index 0 is placed first (bottom of the group), so it resolves last.
    let order = match first {
        FirstToResolve::LeviathanDraw => vec![gain, draw],
        FirstToResolve::RighteousCauseGain => vec![draw, gain],
    };
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("submit the order");
    settle(&mut runner, &[]);
    (runner.state().game_end.is_some(), runner.life(P0))
}

/// CR 603.3b: Summon: Leviathan's delayed chapter II trigger and Righteous
/// Cause's printed trigger fire from the same attack declaration, so P0 orders
/// them in one choice, and the order matters with an opposing Nekusar. Draw
/// first: Nekusar's 1 damage resolves above the life gain and P0, at 1 life,
/// loses. Gain first: P0 goes to 2, then takes 1 and survives at 1.
#[test]
fn leviathan_and_righteous_cause_are_ordered_in_one_choice() {
    let (lost, _) = leviathan_and_righteous_cause_ordered(FirstToResolve::LeviathanDraw);
    assert!(
        lost,
        "draw first: Nekusar's damage kills P0 before the gain"
    );
    let (lost, life) = leviathan_and_righteous_cause_ordered(FirstToResolve::RighteousCauseGain);
    assert!(!lost, "gain first: P0 survives");
    assert_eq!(life, 1, "1 + 1 gained - 1 from Nekusar");
}

/// Maintainer round 3, finding 4. CR 603.4: a delayed body gated on the
/// triggering object ("if it's a Wizard") has no fire-time check yet, so the
/// shape fails closed instead of putting a respondable ability on the stack
/// for a Bear. Control: a body-if that does bridge ("if you control an
/// artifact") stays supported. Synthetic text; no printed card produces the
/// failing shape.
#[test]
fn delayed_event_subject_intervening_if_fails_closed() {
    let abilities = |text: &str| {
        let mut scenario = board();
        let spell = free_spell(&mut scenario, P0, "Synthetic Gate", true, text);
        let runner = scenario.build();
        format!("{:?}", runner.state().objects[&spell].abilities)
    };
    let gated =
        abilities("Until end of turn, whenever a creature attacks, if it's a Wizard, draw a card.");
    assert!(
        gated.contains("delayed_event_subject_intervening_if"),
        "the event-subject gate fails closed: {gated}"
    );
    let bridged = abilities(
        "Until end of turn, whenever a creature you control attacks alone, if you control an artifact, draw a card.",
    );
    assert!(
        bridged.contains("CreateDelayedTrigger"),
        "reach guard: {bridged}"
    );
    assert!(
        !bridged.contains("Unimplemented"),
        "a bridged body-if stays supported: {bridged}"
    );
}
