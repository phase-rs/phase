//! Digital-only Alchemy (no CR entry): one-time boons (issue #7495).
//!
//! Runtime behavior: a grant installs a Persistent one-shot `WhenNextEvent`
//! delayed trigger for its holder; the next matching event whose gate holds
//! fires it once and consumes it; "if you have a boon" reads the held list;
//! an intervening-`if` on the granted trigger is checked at fire time and
//! again at resolution (CR 603.4). A false INITIAL check does not spend the
//! boon — CR 603.4 says the ability "does nothing", and the Tenacious Pup
//! ruling ("Once it triggers, it won't trigger again") spends the one
//! trigger on firing — so the boon stays installed for the next occurrence.
//! A boon that fired and then fails its resolution recheck is already
//! removed, never refunded.

use engine::ai_support::legal_actions;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CastingPermission, DelayedTriggerCondition, DelayedTriggerLifetime, Duration,
    PerpetualModification, TargetRef,
};
use engine::types::actions::{DebugAction, GameAction, ResolutionOptionalPaymentChoice};
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::keywords::{Keyword, KeywordKind, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::ObjectId;

use super::rules::AttackTarget;

const LASH: &str = "Illuminating Lash deals 3 damage to any target.\nYou get a one-time boon with \"When you cast a noncreature spell, draw a card.\"";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const TARGETED_GRANT: &str =
    "Target opponent gets a one-time boon with \"When you cast a noncreature spell, draw a card.\"";
const VALIANT: &str = "Flying\nWhenever Valiant Batrider deals combat damage to a player, that player gets a one-time boon with \"When you cast a noncreature spell, you may pay {1}. If you don't, each opponent draws a card.\"";
const WARLOCK: &str = "Deathtouch\nWhen Underbridge Warlock enters, you get a one-time boon with \"At the beginning of your end step, if three or more creatures died this turn, each opponent loses 5 life and you gain 5 life.\"\nAt the beginning of your end step, if you have a boon, you mill three cards, draw a card, and lose 2 life.";
const PUP: &str = "When Tenacious Pup enters the battlefield, you gain 1 life. You get a one-time boon with \"When you cast a creature spell, that creature enters the battlefield with an additional +1/+1 counter, trample counter, and vigilance counter on it.\"";
const DRAGONBORN: &str = "{2}{R}: This creature gets +1/+0 until end of turn.\nGift of Tiamat — When this creature dies, if its power is greater than 0, note its power. You get a one-time boon with \"When you cast a creature spell, it perpetually gets +X/+0, where X is the noted number.\"";
const MEPHITS: &str = "This sorcery deals 4 damage to target creature or planeswalker. If excess damage was dealt this way, note that excess damage, then you get a one-time boon with \"When you cast a creature spell, it perpetually gets +X/+0, where X is the noted number.\"";
const MOLTEN: &str = "This sorcery deals 4 damage to target creature or planeswalker. If excess damage was dealt this way, note that excess damage, then you get a one-time boon with \"When you cast an instant or sorcery spell, this boon deals damage equal to the noted number to target creature or planeswalker an opponent controls.\"";
const ROTHGA: &str = "Trample\nWhen Rothga, Bonded Engulfer enters, you get a one-time boon with \"When you cast a creature spell, it perpetually gets +X/+X, where X is its power.\"";
// Jaheira, Stirring Harper's boon body on a drivable ETB grantor: Jaheira
// herself grants on specialize, which has no scenario driver, so the runtime
// pins the shared inner shape while `boon_jaheira_stirring_harper` pins her
// text to that same shape.
const JAHEIRA_INNER_GRANT: &str = "When ~ enters, you get a one-time boon with \"When you cast a creature spell, it perpetually gets +1/+0 and gains haste.\"";
const DUNBARROW: &str = "When Dunbarrow Revivalist enters, you get a one-time boon with \"When one or more creatures enter under your control, create a Wicked Role token attached to one of them.\"";
const RAISE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

/// Resolve the stack fully, ordering simultaneous triggers in listed order.
/// Stops on an empty stack at priority — never passes into the next phase.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..100 {
        if runner.state().stack.is_empty()
            && !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. })
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order simultaneous triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt while draining stack: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "stack must drain: {:?}",
        runner.state().stack
    );
}

/// Advance to the end step, declaring no attackers if combat prompts.
/// Precondition: call at or after combat (these tests set up at
/// `PostCombatMain`) — crossing combat from a main phase with no legal
/// attackers lets the turn driver skip the declare step entirely.
fn advance_to_end_step_peaceful(runner: &mut GameRunner) {
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("declare no attackers to cross combat");
    }
    runner.advance_to_end_step();
}

/// Pass through the opponent's whole turn to the start of P0's next
/// precombat main phase, attacking and blocking with nothing.
fn pass_to_own_next_main(runner: &mut GameRunner) {
    let start = runner.state().turn_number;
    for _ in 0..600 {
        let state = runner.state();
        if state.turn_number >= start + 2
            && state.active_player == P0
            && state.phase == Phase::PreCombatMain
        {
            return;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt while crossing the turn: {other:?}"),
        };
        runner.act(action).expect("the turn cycle advances");
    }
    panic!("did not reach P0's next main phase");
}

/// Held boons and their controllers.
fn held_boons(runner: &GameRunner) -> Vec<engine::types::player::PlayerId> {
    runner
        .state()
        .delayed_triggers
        .iter()
        .filter(|dt| dt.kind.is_boon())
        .map(|dt| dt.controller)
        .collect()
}

/// The per-grant noted-number capture each held boon snapshotted at install
/// time, in install order. A boon reads its own capture when its body
/// resolves; a `None` capture (its resolution noted nothing) falls back to
/// the live global.
fn held_boon_captures(runner: &GameRunner) -> Vec<Option<i32>> {
    runner
        .state()
        .delayed_triggers
        .iter()
        .filter(|dt| dt.kind.is_boon())
        .map(|dt| dt.ability.context.boon_captured_noted_number)
        .collect()
}

/// The frozen perpetual P/T records on an object: dynamic deltas evaluate
/// once at application time and freeze to `Fixed`, so only `Fixed` exprs
/// may persist — never a live expression.
fn frozen_perpetual_pt(runner: &GameRunner, id: ObjectId) -> Vec<(i32, i32)> {
    use engine::types::ability::QuantityExpr;
    runner.state().objects[&id]
        .perpetual_mods
        .iter()
        .filter_map(|modification| match modification {
            PerpetualModification::ModifyPowerToughness {
                power: QuantityExpr::Fixed { value: power_delta },
                toughness:
                    QuantityExpr::Fixed {
                        value: toughness_delta,
                    },
                ..
            } => Some((*power_delta, *toughness_delta)),
            _ => None,
        })
        .collect()
}

fn assert_no_dynamic_perpetual(runner: &GameRunner, id: ObjectId) {
    use engine::types::ability::QuantityExpr;
    assert!(
        !runner.state().objects[&id]
            .perpetual_mods
            .iter()
            .any(|m| matches!(
                m,
                PerpetualModification::ModifyPowerToughness { power, toughness, .. }
                if !matches!(power, QuantityExpr::Fixed { .. })
                    || !matches!(toughness, QuantityExpr::Fixed { .. })
            )),
        "dynamic perpetuals must freeze at application, never persist live: {:?}",
        runner.state().objects[&id].perpetual_mods
    );
}

#[test]
fn lash_installs_persistent_one_shot_boon() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lash = scenario
        .add_spell_to_hand_from_oracle(P0, "Illuminating Lash", false, LASH)
        .id();
    let mut runner = scenario.build();
    runner.cast(lash).target_player(P1).resolve();

    assert_eq!(runner.state().players[1].life, 17);
    let boons: Vec<_> = runner
        .state()
        .delayed_triggers
        .iter()
        .filter(|dt| dt.kind.is_boon())
        .collect();
    assert_eq!(boons.len(), 1, "one boon must be held");
    let boon = boons[0];
    assert_eq!(boon.controller, P0);
    assert!(boon.one_shot);
    let DelayedTriggerCondition::WhenNextEvent {
        trigger, lifetime, ..
    } = &boon.condition
    else {
        panic!("boon must be a WhenNextEvent, got {:?}", boon.condition);
    };
    assert_eq!(*lifetime, DelayedTriggerLifetime::Persistent);
    assert_eq!(trigger.mode, TriggerMode::SpellCast);
}

#[test]
fn lash_boon_draws_on_first_cast_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);
    let lash = scenario
        .add_spell_to_hand_from_oracle(P0, "Illuminating Lash", false, LASH)
        .id();
    let bolt_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt One", true, BOLT)
        .id();
    let bolt_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Two", true, BOLT)
        .id();
    let mut runner = scenario.build();
    runner.cast(lash).target_player(P1).resolve();
    assert_eq!(held_boons(&runner), vec![P0]);

    // First noncreature cast fires the boon: cast (-1) then draw (+1).
    let hand_before = runner.state().players[0].hand.len();
    runner.cast(bolt_one).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].hand.len(), hand_before);
    assert!(
        held_boons(&runner).is_empty(),
        "the fired boon must be consumed"
    );

    // Second cast finds no boon: no draw.
    let hand_before = runner.state().players[0].hand.len();
    runner.cast(bolt_two).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].hand.len(), hand_before - 1);
    assert_eq!(runner.state().players[1].life, 11);
}

#[test]
fn targeted_grant_fires_for_holder_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Draw"]);
    scenario.with_library_top(P1, &["P1 Draw"]);
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Targeted Grant", false, TARGETED_GRANT)
        .id();
    let bolt_p0 = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt P0", true, BOLT)
        .id();
    let bolt_p1 = scenario
        .add_spell_to_hand_from_oracle(P1, "Bolt P1", true, BOLT)
        .id();
    let mut runner = scenario.build();
    runner.cast(grant).target_player(P1).resolve();
    assert_eq!(held_boons(&runner), vec![P1]);

    // The creator's own cast does not fire the opponent's boon.
    let hand_before = runner.state().players[0].hand.len();
    runner.cast(bolt_p0).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].hand.len(), hand_before - 1);
    assert_eq!(held_boons(&runner), vec![P1]);

    // The holder's cast fires it: cast (-1) then draw (+1), then consumed.
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to P1");
    let hand_before = runner.state().players[1].hand.len();
    runner.cast(bolt_p1).target_player(P0).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[1].hand.len(), hand_before);
    assert!(held_boons(&runner).is_empty());
    // The draw went to the holder, not the creator.
    assert_eq!(runner.state().players[1].library.len(), 0);
    assert_eq!(runner.state().players[0].library.len(), 1);
}

#[test]
fn valiant_batrider_grants_boon_to_damaged_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Draw"]);
    let valiant = scenario
        .add_creature_from_oracle(P0, "Valiant Batrider", 2, 2, VALIANT)
        .with_subtypes(vec!["Human", "Knight"])
        .id();
    let bolt_p1 = scenario
        .add_spell_to_hand_from_oracle(P1, "Bolt P1", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(valiant, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attack");
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blocks");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            _ if runner.state().phase == Phase::PostCombatMain => break,
            _ => runner.pass_both_players(),
        }
    }

    // Combat damage resolved: P1 was dealt 2 and now holds the boon.
    assert_eq!(runner.state().players[1].life, 18);
    assert_eq!(held_boons(&runner), vec![P1]);

    // P1's noncreature cast fires it; declining {1} makes each opponent draw.
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to P1");
    let hand_before = runner.state().players[0].hand.len();
    runner.cast(bolt_p1).target_player(P0).commit();
    for _ in 0..100 {
        if runner.state().stack.is_empty()
            && !matches!(
                runner.state().waiting_for,
                WaitingFor::OrderTriggers { .. }
                    | WaitingFor::ResolutionOptionalPaymentChoice { .. }
                    | WaitingFor::OptionalEffectChoice { .. }
            )
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the {1} payment");
            }
            WaitingFor::ResolutionOptionalPaymentChoice { .. } => {
                runner
                    .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                        choice: ResolutionOptionalPaymentChoice::Decline,
                    })
                    .expect("decline the {1} payment");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "P0 draws from the declined payment"
    );
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn warlock_false_gate_retains_boon_and_have_a_boon_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_library_top(P0, &["A", "B", "C", "D"]);
    scenario.with_library_top(P1, &["P1 A"]);
    let warlock = scenario
        .add_creature_to_hand_from_oracle(P0, "Underbridge Warlock", 2, 4, WARLOCK)
        .id();
    let mut runner = scenario.build();
    runner.cast(warlock).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // End step with nothing died: the boon's gate is false, so CR 603.4 says
    // it does nothing — the boon is NOT spent (Tenacious Pup ruling: "Once
    // it triggers, it won't trigger again") and stays installed. The
    // separate have-a-boon trigger still finds a held boon at both its
    // fire-time check and its CR 603.4 resolution recheck, so it mills
    // three, draws a card, and costs 2 life.
    let hand_before = runner.state().players[0].hand.len();
    let grave_before = runner.state().players[0].graveyard.len();
    advance_to_end_step_peaceful(&mut runner);
    drain_stack(&mut runner);
    assert_eq!(
        held_boons(&runner),
        vec![P0],
        "a false initial gate retains the boon"
    );
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        grave_before + 3,
        "mill three"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "draw a card"
    );
    assert_eq!(runner.state().players[0].life, 18);
    assert_eq!(runner.state().players[1].life, 20);
}

#[test]
fn warlock_have_a_boon_resolves_while_lash_boon_held() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"]);
    scenario.with_library_top(P1, &["P1 A"]);
    let warlock = scenario
        .add_creature_to_hand_from_oracle(P0, "Underbridge Warlock", 2, 4, WARLOCK)
        .id();
    let lash = scenario
        .add_spell_to_hand_from_oracle(P0, "Illuminating Lash", false, LASH)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    // The Warlock's own end-step boon is held, and Lash grants a second boon
    // whose event (a cast) cannot occur during the end step — so have-a-boon
    // is true at fire time AND still true at its CR 603.4 resolution recheck.
    // The peaceful end step retains BOTH boons: the Lash boon's event never
    // occurs, and the end-step boon's false gate does not spend it.
    runner.cast(warlock).resolve();
    drain_stack(&mut runner);
    runner.cast(lash).target_player(P1).resolve();
    assert_eq!(held_boons(&runner).len(), 2);
    assert_eq!(runner.state().players[1].life, 17);
    let hand_before = runner.state().players[0].hand.len();
    let grave_before = runner.state().players[0].graveyard.len();
    advance_to_end_step_peaceful(&mut runner);
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        grave_before + 3,
        "mill three"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "draw a card"
    );
    assert_eq!(runner.state().players[0].life, 18);
    assert_eq!(
        held_boons(&runner),
        vec![P0, P0],
        "both boons survive the peaceful end step"
    );

    // The Lash boon still fires on the next noncreature cast (cast-then-draw
    // nets zero cards) while the retained end-step boon — whose event is not
    // a cast — stays installed.
    let hand_before = runner.state().players[0].hand.len();
    runner.cast(bolt).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].hand.len(), hand_before);
    assert_eq!(
        held_boons(&runner),
        vec![P0],
        "only the end-step boon remains after the Lash boon fires"
    );
}

/// A boon retained through a peaceful end step still fires exactly once on
/// a later qualifying end step, then is consumed: turn 1 ends peacefully
/// (retained), three creatures die on P0's next turn, and the turn-2 end
/// step drains once (P1 −5, P0 +5) with nothing left held.
#[test]
fn warlock_retained_boon_fires_once_on_later_qualifying_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_library_top(
        P0,
        &["A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L"],
    );
    scenario.with_library_top(P1, &["P1 A", "P1 B"]);
    let warlock = scenario
        .add_creature_to_hand_from_oracle(P0, "Underbridge Warlock", 2, 4, WARLOCK)
        .id();
    let mut victims = Vec::new();
    let mut removal = Vec::new();
    for index in 0..3 {
        victims.push(
            scenario
                .add_creature(P1, &format!("Doomed Witness {index}"), 2, 2)
                .id(),
        );
        removal.push(
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    &format!("Doom Blade {index}"),
                    true,
                    "Destroy target creature.",
                )
                .id(),
        );
    }
    let mut runner = scenario.build();
    runner.cast(warlock).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // Turn 1 end step, peaceful: the false gate retains the boon and the
    // separate have-a-boon payoff resolves (mill 3, draw 1, lose 2).
    advance_to_end_step_peaceful(&mut runner);
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);
    assert_eq!(runner.state().players[0].life, 18);

    // P1's turn passes quietly; on P0's next turn three creatures die.
    pass_to_own_next_main(&mut runner);
    for (spell, victim) in removal.into_iter().zip(victims) {
        runner.cast(spell).target_object(victim).resolve();
    }
    drain_stack(&mut runner);

    // Cross combat into the end step: the Warlock is a legal attacker on
    // this turn, so the crossing may stall at the declare prompt — retry
    // until the end step is actually reached.
    for _ in 0..50 {
        if runner.state().phase == Phase::End {
            break;
        }
        advance_to_end_step_peaceful(&mut runner);
    }
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the qualifying end step was reached"
    );

    // Qualifying end step: the retained boon fires exactly once — one
    // 5-life drain — and is consumed. (P0's own life total pins the
    // engine's listed trigger order: the boon resolves before have-a-boon,
    // so have-a-boon finds no held boon at its resolution recheck and the
    // +5 stands unreduced by a second lose-2.)
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().players[1].life,
        15,
        "one 5-life drain, exactly once"
    );
    assert_eq!(runner.state().players[0].life, 23);
    assert!(held_boons(&runner).is_empty(), "the fired boon is consumed");
}

#[test]
fn pup_boon_adds_counters_to_next_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pup = scenario
        .add_creature_to_hand_from_oracle(P0, "Tenacious Pup", 2, 2, PUP)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();
    runner.cast(pup).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].life, 21);
    assert_eq!(held_boons(&runner), vec![P0]);

    runner.cast(bear).resolve();
    drain_stack(&mut runner);
    let entered = runner.state().objects[&bear].clone();
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1),
        Some(&1),
        "enters with a +1/+1 counter: {:?}",
        entered.counters
    );
    assert_eq!(
        entered
            .counters
            .get(&CounterType::Keyword(KeywordKind::Trample)),
        Some(&1),
        "enters with a trample counter: {:?}",
        entered.counters
    );
    assert_eq!(
        entered
            .counters
            .get(&CounterType::Keyword(KeywordKind::Vigilance)),
        Some(&1),
        "enters with a vigilance counter: {:?}",
        entered.counters
    );
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn dragonborn_dies_notes_power_and_pumps_next_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragonborn = scenario
        .add_creature_from_oracle(P0, "Dragonborn Immolator", 3, 3, DRAGONBORN)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    // Bolt kills the 3/3: the dies trigger notes 3 and grants the boon.
    runner.cast(bolt).target_object(dragonborn).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(3));
    assert_eq!(held_boons(&runner), vec![P0]);

    // The next creature spell perpetually gets +3/+0, then the boon is spent.
    runner.cast(bear).resolve();
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(5), Some(2)));
    assert_eq!(frozen_perpetual_pt(&runner, bear), vec![(3, 0)]);
    assert_no_dynamic_perpetual(&runner, bear);
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn dragonborn_zero_power_never_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragonborn = scenario
        .add_creature_from_oracle(P0, "Dragonborn Immolator", 3, 3, DRAGONBORN)
        .id();
    // Same oracle text, distinct name (no name self-reference in the text,
    // so the rename is behavior-preserving and side-steps the legend rule).
    let dragonborn_zero = scenario
        .add_creature_from_oracle(P0, "Dragonborn Whelp", 0, 3, DRAGONBORN)
        .id();
    let bolt_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt One", true, BOLT)
        .id();
    let bolt_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Two", true, BOLT)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    // The 3/3 dies first: notes 3 and grants a boon, leaving a stale
    // nonzero note behind for the sting below.
    runner.cast(bolt_one).target_object(dragonborn).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(3));
    assert_eq!(held_boons(&runner), vec![P0]);

    // Power 0 fails the CR 603.4 intervening-if, so the trigger never
    // fires at all: no note, no grant — even with the stale note 3 live.
    runner
        .cast(bolt_two)
        .target_object(dragonborn_zero)
        .resolve();
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().players[0].noted_number,
        Some(3),
        "a gated-off trigger notes nothing"
    );
    assert_eq!(
        held_boons(&runner),
        vec![P0],
        "a gated-off trigger grants nothing"
    );
    assert_eq!(held_boon_captures(&runner), vec![Some(3)]);

    // Only the first boon fires on the bear: 2 + 3.
    runner.cast(bear).resolve();
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(5), Some(2)));
    assert_eq!(frozen_perpetual_pt(&runner, bear), vec![(3, 0)]);
    assert_no_dynamic_perpetual(&runner, bear);
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn mephits_excess_notes_and_pumps_next_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario.add_creature_from_oracle(P1, "Wall", 0, 2, "").id();
    let mephits = scenario
        .add_spell_to_hand_from_oracle(P0, "Mephit's Enthusiasm", false, MEPHITS)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    // 4 damage to a 2-toughness creature: excess 2 is noted, boon granted.
    runner.cast(mephits).target_object(wall).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(2));
    assert_eq!(held_boons(&runner), vec![P0]);

    runner.cast(bear).resolve();
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(4), Some(2)));
    assert_eq!(frozen_perpetual_pt(&runner, bear), vec![(2, 0)]);
    assert_no_dynamic_perpetual(&runner, bear);
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn mephits_no_excess_notes_nothing_and_grants_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_from_oracle(P1, "Giant", 4, 5, "")
        .id();
    let mephits = scenario
        .add_spell_to_hand_from_oracle(P0, "Mephit's Enthusiasm", false, MEPHITS)
        .id();
    let mut runner = scenario.build();

    // 4 damage to a 5-toughness creature: no excess, so both gated legs skip.
    runner.cast(mephits).target_object(giant).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, None);
    assert!(held_boons(&runner).is_empty(), "no excess means no grant");
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(
        runner.state().objects[&giant].zone,
        Zone::Battlefield,
        "the giant survives at 4 marked damage"
    );
}

#[test]
fn molten_boon_deals_noted_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall_one = scenario
        .add_creature_from_oracle(P1, "Wall One", 0, 2, "")
        .id();
    let wall_two = scenario
        .add_creature_from_oracle(P1, "Wall Two", 0, 2, "")
        .id();
    let molten = scenario
        .add_spell_to_hand_from_oracle(P0, "Molten Impact", false, MOLTEN)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.cast(molten).target_object(wall_one).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(2));
    assert_eq!(held_boons(&runner), vec![P0]);

    // The Bolt cast fires the boon; the boon trigger needs its own target.
    runner.cast(bolt).target_player(P1).commit();
    for _ in 0..100 {
        if runner.state().stack.is_empty()
            && !matches!(
                runner.state().waiting_for,
                WaitingFor::OrderTriggers { .. }
                    | WaitingFor::TriggerTargetSelection { .. }
                    | WaitingFor::TargetSelection { .. }
            )
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(wall_two)),
                    })
                    .expect("choose the boon target");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    // The boon dealt the noted 2 (lethal to Wall Two); Bolt dealt 3 to P1.
    assert_eq!(runner.state().objects[&wall_two].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[1].life, 17);
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn rothga_perpetual_reads_spell_power_not_granter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rothga = scenario
        .add_creature_to_hand_from_oracle(P0, "Rothga, Bonded Engulfer", 4, 4, ROTHGA)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(rothga).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // X is the 2-power spell's power — Rothga's own 4 must not leak in.
    runner.cast(bear).resolve();
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(4), Some(4)));
    assert_eq!(frozen_perpetual_pt(&runner, bear), vec![(2, 2)]);
    assert_no_dynamic_perpetual(&runner, bear);
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn jaheira_boon_grants_perpetual_pt_and_haste_to_cast_creature() {
    use engine::types::ability::QuantityExpr;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grantor = scenario
        .add_creature_to_hand_from_oracle(P0, "Jaheira Proxy", 2, 2, JAHEIRA_INNER_GRANT)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(grantor).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    runner.cast(bear).resolve();
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(3), Some(2)));
    assert_eq!(frozen_perpetual_pt(&runner, bear), vec![(1, 0)]);
    assert_no_dynamic_perpetual(&runner, bear);
    // One combined record — the +1/+0 and the haste ride a single
    // modification, not sibling records.
    let combined = runner.state().objects[&bear]
        .perpetual_mods
        .iter()
        .filter(|m| {
            matches!(
                m,
                PerpetualModification::ModifyPowerToughness {
                    power: QuantityExpr::Fixed { value: 1 },
                    toughness: QuantityExpr::Fixed { value: 0 },
                    keywords,
                } if keywords.iter().any(|k| matches!(k, Keyword::Haste))
            )
        })
        .count();
    assert_eq!(combined, 1);
    assert!(
        object_has_effective_keyword_kind(runner.state(), bear, KeywordKind::Haste),
        "the cast creature must have perpetual haste"
    );
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn sequential_notes_grant_independent_boons() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall_a = scenario
        .add_creature_from_oracle(P1, "Wall A", 0, 3, "")
        .id();
    let wall_b = scenario
        .add_creature_from_oracle(P1, "Wall B", 0, 1, "")
        .id();
    let mephits_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Mephit's One", false, MEPHITS)
        .id();
    let mephits_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Mephit's Two", false, MEPHITS)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(mephits_one).target_object(wall_a).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(1));
    runner.cast(mephits_two).target_object(wall_b).resolve();
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().players[0].noted_number,
        Some(3),
        "the live global still overwrites"
    );
    assert_eq!(held_boons(&runner).len(), 2);
    // Each grant captured what its own resolution noted — the second note
    // must not leak into the first boon.
    assert_eq!(held_boon_captures(&runner), vec![Some(1), Some(3)]);

    // Both boons fire on the one cast, each pumping its own captured note
    // (2 + 1 + 3). Two simultaneous triggers need manual driving: the cast
    // driver's commit loop does not order triggers.
    let bear_card = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Bear");
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(6), Some(2)));
    let mut frozen = frozen_perpetual_pt(&runner, bear);
    frozen.sort_unstable();
    assert_eq!(frozen, vec![(1, 0), (3, 0)]);
    assert_no_dynamic_perpetual(&runner, bear);
    assert!(held_boons(&runner).is_empty());
}

/// Resolution-local note provenance: a grant whose own resolution noted
/// nothing captures `None` even with a stale live note in place, so a
/// later note reaches its live fallback. Synthetic grantor (no printed
/// note-reading grant lacks a note leg); the inner mirrors Mephit's.
const NOTELESS_GRANT: &str = "When ~ enters, you get a one-time boon with \"When you cast a creature spell, it perpetually gets +X/+0, where X is the noted number.\"";

#[test]
fn noteless_grant_ignores_stale_note_and_reads_live() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall_a = scenario
        .add_creature_from_oracle(P1, "Wall A", 0, 3, "")
        .id();
    let wall_b = scenario
        .add_creature_from_oracle(P1, "Wall B", 0, 1, "")
        .id();
    let mephits_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Mephit's One", false, MEPHITS)
        .id();
    let mephits_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Mephit's Two", false, MEPHITS)
        .id();
    let proxy = scenario
        .add_creature_to_hand_from_oracle(P0, "Noteless Proxy", 2, 2, NOTELESS_GRANT)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    // Pre-existing note 1 in place, with its own grant capturing 1.
    runner.cast(mephits_one).target_object(wall_a).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(1));

    // Casting the proxy fires boonA first (positive control: it captured its
    // own 1). The noteless grant then resolves with the stale note live: it
    // must capture `None`, not 1. Manual cast: the SpellCast driver does not
    // order the two triggers (boonA's fire + the proxy's own ETB).
    let proxy_card = runner.state().objects[&proxy].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: proxy,
            card_id: proxy_card,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Proxy");
    drain_stack(&mut runner);
    let proxied = &runner.state().objects[&proxy];
    assert_eq!((proxied.power, proxied.toughness), (Some(3), Some(2)));
    assert_eq!(held_boon_captures(&runner), vec![None]);

    // A later note overwrites the live global (and grants its own boon).
    runner.cast(mephits_two).target_object(wall_b).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(3));
    assert_eq!(held_boon_captures(&runner), vec![None, Some(3)]);

    // One cast fires both: the noteless boon reads the live 3, not the
    // stale 1 (2 + 3 + 3). Manual cast again for the two simultaneous fires.
    let bear_card = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Bear");
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(8), Some(2)));
    let mut frozen = frozen_perpetual_pt(&runner, bear);
    frozen.sort_unstable();
    assert_eq!(frozen, vec![(3, 0), (3, 0)]);
    assert_no_dynamic_perpetual(&runner, bear);
    assert!(held_boons(&runner).is_empty());
}

/// Every Role token in the game, in every zone on purpose: a token that
/// reached the battlefield and was swept looks identical to one never
/// created under a battlefield-only query.
fn role_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.card_types.subtypes.iter().any(|s| s == "Role"))
        .map(|object| object.id)
        .collect()
}

#[test]
fn dunbarrow_two_entrants_offer_host_choice() {
    use engine::game::game_object::AttachTarget;
    use engine::types::ability::TargetRef;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // Two soldiers enter in one batch; the boon fires once. Manual driving:
    // the cast driver's commit loop does not answer the host choice.
    runner.cast(alarm).commit();
    let mut answered = false;
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseTokenHost { legal_targets, .. } => {
                assert_eq!(
                    legal_targets.len(),
                    2,
                    "both entrants must be offered, got {legal_targets:?}"
                );
                // Choose deterministically: the greater soldier id.
                let mut soldiers: Vec<ObjectId> = legal_targets
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                soldiers.sort();
                let chosen = soldiers[1];
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .expect("choose host");
                answered = true;
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(answered, "the host choice must have been offered");
    drain_stack(&mut runner);

    // The Role entered attached to the CHOSEN soldier — not the first.
    let roles = role_tokens(&runner);
    assert_eq!(roles.len(), 1, "one Role must be created");
    let role = &runner.state().objects[&roles[0]];
    assert_eq!(role.zone, Zone::Battlefield);
    let mut soldiers: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token && o.controller == P0 && o.card_types.subtypes.iter().any(|s| s == "Soldier")
        })
        .map(|o| o.id)
        .collect();
    soldiers.sort();
    assert_eq!(soldiers.len(), 2);
    assert_eq!(
        role.attached_to,
        Some(AttachTarget::Object(soldiers[1])),
        "the Role must attach to the chosen entrant"
    );
    assert!(held_boons(&runner).is_empty());
}

/// CR 400.2 production reach control: the real Dunbarrow
/// batch prompt pauses at `ChooseTokenHost` with the cloned creating
/// ability in `pending_ability`, and the viewer projections carry the
/// prompt through the payment redaction authority — opponent and
/// spectator views keep the prompt shape with the ability intact (no
/// snapshots exist on this path, so nothing blanks), the authoritative
/// state is untouched by projecting, and no hidden hand card leaks into
/// either projection's wire JSON. The synthetic census row in
/// `game::visibility` proves the blanking itself with nonempty snapshots.
#[test]
fn dunbarrow_batch_host_prompt_projects_without_leak() {
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    // A hidden hand card with a name no projection may repeat.
    scenario.add_creature_to_hand_from_oracle(P0, "Veiled Sentry Mole", 1, 1, "Defender");
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // Drive the batch to the host prompt and STOP there, unanswered.
    runner.cast(alarm).commit();
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseTokenHost { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    let raw = runner.state().clone();
    let (player, source_id, legal_targets, pending_ability) = match &raw.waiting_for {
        WaitingFor::ChooseTokenHost {
            player,
            source_id,
            legal_targets,
            pending_ability,
        } => (
            *player,
            *source_id,
            legal_targets.clone(),
            (**pending_ability).clone(),
        ),
        other => panic!("reach-guard: expected the batch host prompt, got {other:?}"),
    };
    assert_eq!(player, P0, "reach-guard: P0 chooses the host");
    assert_eq!(
        legal_targets.len(),
        2,
        "reach-guard: both entrants offered, got {legal_targets:?}"
    );

    for (label, view) in [
        ("opponent", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        match &view.waiting_for {
            WaitingFor::ChooseTokenHost {
                player: view_player,
                source_id: view_source,
                legal_targets: view_targets,
                pending_ability: view_pending,
            } => {
                assert_eq!(*view_player, player, "{label}: prompt player preserved");
                assert_eq!(*view_source, source_id, "{label}: prompt source preserved");
                assert_eq!(
                    *view_targets, legal_targets,
                    "{label}: offered entrants preserved"
                );
                assert_eq!(
                    format!("{:?}", **view_pending),
                    format!("{pending_ability:?}"),
                    "{label}: pending ability passes through with no snapshots to blank"
                );
            }
            other => panic!("{label}: prompt shape lost in projection: {other:?}"),
        }
        let json = serde_json::to_string(&view).expect("projection serializes");
        assert!(
            !json.contains("Veiled Sentry Mole"),
            "{label}: hidden hand card must not leak through the host prompt"
        );
    }

    // Authoritative state is untouched by projecting.
    assert_eq!(
        runner.state().waiting_for,
        raw.waiting_for,
        "projecting must not mutate the authoritative prompt"
    );
}

#[test]
fn dunbarrow_single_entrant_attaches_without_prompt() {
    use engine::game::game_object::AttachTarget;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // One entrant: auto-bound, no prompt (`drain_stack` panics on any
    // unexpected prompt, proving none was offered).
    runner.cast(bear).resolve();
    drain_stack(&mut runner);

    let roles = role_tokens(&runner);
    assert_eq!(roles.len(), 1, "one Role must be created");
    let role = &runner.state().objects[&roles[0]];
    assert_eq!(role.zone, Zone::Battlefield);
    assert_eq!(
        role.attached_to,
        Some(AttachTarget::Object(bear)),
        "the Role must attach to the lone entrant"
    );
    assert!(held_boons(&runner).is_empty());
}

/// CR 616.1: one Dunbarrow batch under the holder's Doubling Season and the
/// opponent's Halving Season, applying `first` first. Integer halving does
/// not commute with doubling, so double-then-halve yields one Role while
/// halve-then-double yields zero — the host-choice resume must preserve the
/// replacement pause instead of settling to Priority and dropping it.
fn dunbarrow_season_order(first: &str, expect_roles: usize) {
    use engine::game::game_object::AttachTarget;
    use engine::types::ability::TargetRef;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    let other = if first == "twice" { "half" } else { "twice" };
    let mut host_answered = false;
    let mut chosen_host = None;
    let mut role_prompts = 0;
    runner.cast(alarm).commit();
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseTokenHost { legal_targets, .. } => {
                assert!(
                    legal_targets.len() >= 2,
                    "the batch must offer a host choice, got {legal_targets:?}"
                );
                let mut soldiers: Vec<ObjectId> = legal_targets
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                soldiers.sort();
                let chosen = soldiers[soldiers.len() - 1];
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .expect("choose host");
                host_answered = true;
                chosen_host = Some(chosen);
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                // Pre-host prompts belong to the soldiers (either order lands
                // on two: 2*2/2 == 2 and 2/2*2 == 2); post-host prompts are
                // the Role's ordering and follow `first`.
                let want = if !host_answered {
                    None
                } else {
                    role_prompts += 1;
                    Some(if role_prompts == 1 { first } else { other })
                };
                let index = match want {
                    None => 0,
                    Some(want) => candidates
                        .iter()
                        .position(|c| c.description.contains(want))
                        .unwrap_or_else(|| {
                            panic!(
                                "expected a {want} candidate, got {:?}",
                                candidates
                                    .iter()
                                    .map(|c| c.description.as_str())
                                    .collect::<Vec<_>>()
                            )
                        }),
                };
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("choose replacement");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(host_answered, "the host choice must have been offered");
    assert!(
        role_prompts >= 1,
        "answering the host must surface the Role's replacement pause"
    );
    drain_stack(&mut runner);

    let roles = role_tokens(&runner);
    assert_eq!(
        roles.len(),
        expect_roles,
        "ordering {first} first must create exactly {expect_roles} Role(s)"
    );
    if expect_roles == 1 {
        let role = &runner.state().objects[&roles[0]];
        assert_eq!(role.zone, Zone::Battlefield);
        assert_eq!(
            role.attached_to,
            Some(AttachTarget::Object(chosen_host.expect("host"))),
            "the Role must attach to the chosen entrant"
        );
    }
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn dunbarrow_double_then_halve_creates_one_role() {
    dunbarrow_season_order("twice", 1);
}

#[test]
fn dunbarrow_halve_then_double_creates_no_role() {
    dunbarrow_season_order("half", 0);
}

/// CR 608.2c: a token-host instruction with a following instruction parks its
/// tail while the host choice is pending. Synthetic inner (no printed boon
/// pairs "one of them" with a tail): the life gain must NOT have applied
/// while the prompt is open, and must apply exactly once after it resolves.
const DUNBARROW_TAIL: &str = "When ~ enters, you get a one-time boon with \"When one or more creatures enter under your control, create a Wicked Role token attached to one of them. You gain 2 life.\"";

#[test]
fn dunbarrow_host_choice_parks_following_instruction() {
    use engine::types::ability::TargetRef;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grantor = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Proxy", 3, 3, DUNBARROW_TAIL)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    let mut runner = scenario.build();

    runner.cast(grantor).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);
    assert_eq!(runner.state().players[0].life, 20);

    runner.cast(alarm).commit();
    let mut answered = false;
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseTokenHost { legal_targets, .. } => {
                assert_eq!(
                    runner.state().players[0].life,
                    20,
                    "the tail must not run before the host is chosen"
                );
                let chosen = legal_targets
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        _ => None,
                    })
                    .max()
                    .expect("legal host");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .expect("choose host");
                answered = true;
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(answered, "the host choice must have been offered");
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().players[0].life,
        22,
        "the parked tail must run exactly once after the host resolves"
    );
    assert_eq!(role_tokens(&runner).len(), 1);
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn dunbarrow_departed_entrant_creates_no_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // The bear enters (firing the boon), then dies in response to the
    // trigger: at resolution no stamped entrant is legal, so CR 303.4i
    // denies the Aura token entirely.
    runner.cast(bear).commit();
    let mut bolted = false;
    for _ in 0..200 {
        if !bolted
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && !runner.state().stack.is_empty()
            && runner.state().objects[&bear].zone == Zone::Battlefield
        {
            // Reach-guard: the stack top must be the boon trigger WITH its
            // entrant stamped — otherwise a fire-without-stamp bug would
            // pass the no-token assertions below for the wrong reason.
            let top = runner
                .state()
                .stack
                .last()
                .expect("boon trigger on the stack");
            let StackEntryKind::TriggeredAbility { ability, .. } = &top.kind else {
                panic!("stack top must be the boon trigger, got {:?}", top.kind);
            };
            assert!(
                !ability.context.boon_trigger_batch_objects.is_empty(),
                "the boon trigger must stamp its entrant before Bolt is cast"
            );
            runner.cast(bolt).target_object(bear).resolve();
            bolted = true;
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(bolted, "the bear must have died in response");
    drain_stack(&mut runner);

    assert!(
        role_tokens(&runner).is_empty(),
        "no Role may be created without a legal host"
    );
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn cross_card_notes_stay_independent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragonborn = scenario
        .add_creature_from_oracle(P0, "Dragonborn Immolator", 3, 3, DRAGONBORN)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let wall = scenario.add_creature_from_oracle(P1, "Wall", 0, 2, "").id();
    let mephits = scenario
        .add_spell_to_hand_from_oracle(P0, "Mephit's Enthusiasm", false, MEPHITS)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    // Dragonborn dies noting its power 3; Mephit's notes excess 2. Notes
    // from different cards must not cross-contaminate their boons.
    runner.cast(bolt).target_object(dragonborn).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(3));
    runner.cast(mephits).target_object(wall).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(2));
    assert_eq!(held_boons(&runner).len(), 2);
    assert_eq!(held_boon_captures(&runner), vec![Some(3), Some(2)]);

    // Both fire on the bear: 2 + 3 + 2.
    let bear_card = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Bear");
    drain_stack(&mut runner);
    let entered = &runner.state().objects[&bear];
    assert_eq!((entered.power, entered.toughness), (Some(7), Some(2)));
    let mut frozen = frozen_perpetual_pt(&runner, bear);
    frozen.sort_unstable();
    assert_eq!(frozen, vec![(2, 0), (3, 0)]);
    assert_no_dynamic_perpetual(&runner, bear);
    assert!(held_boons(&runner).is_empty());
}

/// Soldier tokens on the battlefield, sorted by id. Shared by the M3-3
/// protection/hexproof rows so the cast-then-drive loops index the same
/// soldiers the tails assert on.
fn soldier_tokens_on_battlefield(runner: &GameRunner) -> Vec<ObjectId> {
    let mut soldiers: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Soldier")
        })
        .map(|o| o.id)
        .collect();
    soldiers.sort();
    soldiers
}

/// Angelic Intervention, verbatim Oracle text (ONE #2): the M3-3 Giver-side
/// witness. Its colorless branch makes one entrant attachment-illegal for
/// the colorless Role token, so the host offer must exclude that entrant.
const ANGELIC_INTERVENTION: &str = "Target creature or planeswalker you control gains protection from colorless or from the color of your choice until end of turn. If it's a creature, put a +1/+1 counter on it.";

/// Blossoming Defense, verbatim Oracle text (KLD #145): the hexproof
/// control. Hexproof is NOT one of the exclusion predicates, so its
/// recipient must stay offered and must be able to receive the Role.
const BLOSSOMING_DEFENSE: &str =
    "Target creature you control gets +2/+2 and gains hexproof until end of turn.";

#[test]
fn dunbarrow_protected_entrant_not_offered_role() {
    use engine::game::game_object::AttachTarget;
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    let angelic = scenario
        .add_spell_to_hand_from_oracle(P0, "Angelic Intervention", true, ANGELIC_INTERVENTION)
        .id();
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    runner.cast(alarm).commit();
    let mut angeliced = false;
    for _ in 0..400 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                let soldiers = soldier_tokens_on_battlefield(&runner);
                if !angeliced && soldiers.len() == 2 {
                    // The boon trigger is stacked (the stack is non-empty in
                    // this arm). Protect the first soldier before it resolves.
                    runner.cast(angelic).target_object(soldiers[0]).commit();
                    angeliced = true;
                } else {
                    runner.pass_both_players();
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseOneOfBranch {
                player, branches, ..
            } => {
                assert_eq!(*player, P0, "the spell's controller makes the choice");
                assert_eq!(branches.len(), 2, "colorless-or-color is two branches");
                let index = branches
                    .iter()
                    .position(|b| {
                        b.description
                            .as_deref()
                            .is_some_and(|d| d.to_lowercase().contains("colorless"))
                    })
                    .expect("a protection-from-colorless branch must exist");
                runner
                    .act(GameAction::ChooseBranch { index })
                    .expect("choose colorless");
            }
            WaitingFor::ChooseTokenHost { legal_targets, .. } => {
                panic!(
                    "M3-3: only one legal host remains, so no host choice may appear: {legal_targets:?}"
                );
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(angeliced, "Angelic Intervention must have been cast");
    drain_stack(&mut runner);

    let mut soldiers = soldier_tokens_on_battlefield(&runner);
    soldiers.sort();
    assert_eq!(soldiers.len(), 2);
    // Guard: the +1/+1 counter proves Angelic resolved targeting the first
    // soldier. The protection leg has no other board-observable mark; the
    // exclusion below is its behavioral proof (a mis-answered branch would
    // leave two legal hosts and trip the ChooseTokenHost panic arm).
    assert_eq!(
        runner.state().objects[&soldiers[0]]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "Angelic must have resolved on the protected soldier"
    );
    // One Role, auto-bound to the UNPROTECTED entrant with no prompt.
    let roles = role_tokens(&runner);
    assert_eq!(roles.len(), 1, "one Role must be created");
    let role = &runner.state().objects[&roles[0]];
    assert_eq!(
        role.attached_to,
        Some(AttachTarget::Object(soldiers[1])),
        "the Role must attach to the unprotected entrant"
    );
    assert!(held_boons(&runner).is_empty());
}

/// F1 "no legal hosts": BOTH entrants gain protection from colorless in
/// response to the boon trigger. The pins are current and both soldiers are
/// on the battlefield — the projection alone excludes everything, so no
/// prompt appears and CR 303.4i denies the Aura token entirely. (The departed
/// test covers zero candidates via staleness; this covers zero via
/// illegality.)
#[test]
fn dunbarrow_all_entrants_protected_creates_no_token() {
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    let angelic_first = scenario
        .add_spell_to_hand_from_oracle(P0, "Angelic Intervention", true, ANGELIC_INTERVENTION)
        .id();
    let angelic_second = scenario
        .add_spell_to_hand_from_oracle(P0, "Angelic Intervention", true, ANGELIC_INTERVENTION)
        .id();
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    runner.cast(alarm).commit();
    let mut first = false;
    let mut second = false;
    for _ in 0..400 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                let soldiers = soldier_tokens_on_battlefield(&runner);
                if soldiers.len() == 2 && !first {
                    runner
                        .cast(angelic_first)
                        .target_object(soldiers[0])
                        .commit();
                    first = true;
                } else if soldiers.len() == 2
                    && first
                    && !second
                    && runner.state().objects[&soldiers[0]]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        == Some(&1)
                {
                    // The first Angelic resolved (counter mark); protect the
                    // other entrant before the boon trigger resolves.
                    runner
                        .cast(angelic_second)
                        .target_object(soldiers[1])
                        .commit();
                    second = true;
                } else {
                    runner.pass_both_players();
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseOneOfBranch {
                player, branches, ..
            } => {
                assert_eq!(*player, P0, "the spell's controller makes the choice");
                assert_eq!(branches.len(), 2, "colorless-or-color is two branches");
                let index = branches
                    .iter()
                    .position(|b| {
                        b.description
                            .as_deref()
                            .is_some_and(|d| d.to_lowercase().contains("colorless"))
                    })
                    .expect("a protection-from-colorless branch must exist");
                runner
                    .act(GameAction::ChooseBranch { index })
                    .expect("choose colorless");
            }
            WaitingFor::ChooseTokenHost { legal_targets, .. } => {
                panic!(
                    "F1: no legal hosts remain, so no host choice may appear: {legal_targets:?}"
                );
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        first && second,
        "both Angelics must have been cast, first={first} second={second}"
    );
    drain_stack(&mut runner);

    // Guards: the counters prove both Angelics resolved on their soldiers.
    let soldiers = soldier_tokens_on_battlefield(&runner);
    assert_eq!(soldiers.len(), 2);
    for soldier in &soldiers {
        assert_eq!(
            runner.state().objects[soldier]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&1),
            "both soldiers must carry their Angelic counter"
        );
    }
    assert!(
        role_tokens(&runner).is_empty(),
        "no Role may be created without a legal host"
    );
    assert!(held_boons(&runner).is_empty());
}

#[test]
fn dunbarrow_hexproof_entrant_still_offered_role() {
    use engine::game::game_object::AttachTarget;
    use engine::game::layers::evaluate_layers;
    use engine::types::ability::TargetRef;
    use engine::types::keywords::Keyword;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    let blossoming = scenario
        .add_spell_to_hand_from_oracle(P0, "Blossoming Defense", true, BLOSSOMING_DEFENSE)
        .id();
    let mut runner = scenario.build();

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    runner.cast(alarm).commit();
    let mut defended = false;
    let mut answered = false;
    for _ in 0..400 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                let soldiers = soldier_tokens_on_battlefield(&runner);
                if !defended && soldiers.len() == 2 {
                    runner.cast(blossoming).target_object(soldiers[1]).commit();
                    defended = true;
                } else {
                    runner.pass_both_players();
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseTokenHost { legal_targets, .. } => {
                assert_eq!(
                    legal_targets.len(),
                    2,
                    "hexproof must not exclude the entrant, got {legal_targets:?}"
                );
                // Choose deterministically: the HEXPROOF soldier (greater id).
                let mut soldiers: Vec<ObjectId> = legal_targets
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                soldiers.sort();
                let chosen = soldiers[1];
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .expect("choose host");
                answered = true;
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(defended, "Blossoming Defense must have been cast");
    assert!(answered, "the host choice must have been offered");
    drain_stack(&mut runner);

    let mut soldiers = soldier_tokens_on_battlefield(&runner);
    soldiers.sort();
    assert_eq!(soldiers.len(), 2);
    // Guard: hexproof is really present on the chosen entrant — otherwise
    // the "still offered" assertion below would pass vacuously.
    evaluate_layers(runner.state_mut());
    assert!(
        runner.state().objects[&soldiers[1]].has_keyword(&Keyword::Hexproof),
        "Blossoming must have granted hexproof to the chosen entrant"
    );
    // The Role attaches to the hexproof entrant: hexproof neither excludes
    // the offer nor blocks the attachment.
    let roles = role_tokens(&runner);
    assert_eq!(roles.len(), 1, "one Role must be created");
    let role = &runner.state().objects[&roles[0]];
    assert_eq!(
        role.attached_to,
        Some(AttachTarget::Object(soldiers[1])),
        "the Role must attach to the chosen hexproof entrant"
    );
    assert!(held_boons(&runner).is_empty());
}

/// Reanimate-two witness: returns two nontoken creatures to the
/// battlefield in one batch, so the boon stamps two blinkable entrants.
/// (Tokens cannot blink — CR 111.8 strands them outside the battlefield.)
const REANIMATE_TWO: &str =
    "Return two target creature cards from your graveyard to the battlefield.";

/// F2 (CR 400.7): at the sandbox/debug boundary an offered host can leave
/// and return during the prompt with the same stored ObjectId and a new
/// incarnation — a new object. Answering with it must be rejected while
/// the prompt (and its continuation) is preserved; the untouched entrant
/// must still bind.
#[test]
fn dunbarrow_stale_incarnation_answer_rejected() {
    use engine::game::game_object::AttachTarget;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let bear1 = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let bear2 = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let bolt1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let bolt2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate Two", false, REANIMATE_TWO)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    // Park both bears in the graveyard BEFORE Dunbarrow arrives, so no boon
    // trigger fires and the held boon is intact for the batch entry below.
    for (bear, bolt) in [(bear1, bolt1), (bear2, bolt2)] {
        runner.cast(bear).resolve();
        drain_stack(&mut runner);
        runner.cast(bolt).target_object(bear).resolve();
        drain_stack(&mut runner);
        assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    }
    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // Both bears re-enter in one batch: one trigger, two stamped entrants.
    runner
        .cast(reanimate)
        .target_objects(&[bear1, bear2])
        .commit();
    for _ in 0..400 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("the host prompt must appear")
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::ChooseTokenHost { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    let (bears, pin) = match &runner.state().waiting_for {
        WaitingFor::ChooseTokenHost {
            legal_targets,
            pending_ability,
            ..
        } => {
            assert_eq!(legal_targets.len(), 2);
            let mut bears: Vec<ObjectId> = legal_targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect();
            bears.sort();
            assert_eq!(
                bears,
                {
                    let mut expected = vec![bear1, bear2];
                    expected.sort();
                    expected
                },
                "both reanimated bears must be offered"
            );
            let pin = pending_ability
                .context
                .boon_trigger_batch_objects
                .iter()
                .find(|pin| pin.object_id == bears[0])
                .cloned()
                .expect("the offered entrant must carry a stamp pin");
            (bears, pin)
        }
        other => panic!("expected the host prompt, got {other:?}"),
    };
    assert!(
        pin.is_current(runner.state()),
        "the pin must be current before the blink"
    );

    // Debug-blink the first bear: same ObjectId, new incarnation. Raw
    // placement (no triggers/SBAs) so the prompt itself is undisturbed.
    for to_zone in [Zone::Graveyard, Zone::Battlefield] {
        runner
            .act(GameAction::Debug(DebugAction::MoveToZone {
                object_id: bears[0],
                to_zone,
                library_position: None,
                simulate: false,
            }))
            .expect("debug move must succeed");
    }
    let blinked = &runner.state().objects[&bears[0]];
    assert_eq!(
        blinked.zone,
        Zone::Battlefield,
        "the blinked bear must be back on the battlefield (same id)"
    );
    assert!(
        !pin.is_current(runner.state()),
        "the blink must stale the stamp pin"
    );

    // The stale answer is rejected and the prompt is preserved.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears[0])),
        })
        .expect_err("a stale-incarnation answer must be rejected");
    match &runner.state().waiting_for {
        WaitingFor::ChooseTokenHost { legal_targets, .. } => {
            assert_eq!(legal_targets.len(), 2, "the offer must be preserved");
        }
        other => panic!("the prompt must be preserved, got {other:?}"),
    }

    // The untouched entrant still binds.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears[1])),
        })
        .expect("the current entrant must bind");
    drain_stack(&mut runner);
    let roles = role_tokens(&runner);
    assert_eq!(roles.len(), 1, "one Role must be created");
    let role = &runner.state().objects[&roles[0]];
    assert_eq!(
        role.attached_to,
        Some(AttachTarget::Object(bears[1])),
        "the Role must attach to the un-blinked entrant"
    );
    assert!(held_boons(&runner).is_empty());
}

/// F1-gap (Matt): a stamped entrant that ceases to be a creature before the
/// boon resolves must be excluded from the "one of them" offer — the Role's
/// "enchant creature" filter no longer admits it (CR 303.4). Song of the
/// Dryads is a sorcery-speed Aura, uncastable with the trigger stacked, so it
/// rides the debug pipeline to the battlefield and attaches through the real
/// `attach_to` authority; the layers system genuinely recomputes the victim
/// as a land (guarded below — the exclusion is not vacuous). One legal
/// candidate remains, so no prompt appears and the Role auto-binds to the
/// untouched entrant.
#[test]
fn dunbarrow_type_changed_entrant_excluded_from_offer() {
    use engine::game::effects::attach::attach_to;
    use engine::game::game_object::AttachTarget;
    use engine::types::card_type::CoreType;

    const SONG: &str = "Enchant permanent\nEnchanted permanent is a colorless Forest land.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dunbarrow = scenario
        .add_creature_to_hand_from_oracle(P0, "Dunbarrow Revivalist", 3, 3, DUNBARROW)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise Alarm", false, RAISE_ALARM)
        .id();
    let song = scenario
        .add_creature(P0, "Song of the Dryads", 0, 0)
        .as_enchantment()
        .from_oracle_text(SONG)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    // The builder leaves the Aura subtype off (mirrors issue_3279): restore
    // it before any action so SBAs treat the Song as the Aura it is.
    {
        let song_obj = runner
            .state_mut()
            .objects
            .get_mut(&song)
            .expect("Song present");
        if !song_obj.card_types.subtypes.iter().any(|s| s == "Aura") {
            song_obj.card_types.subtypes.push("Aura".to_string());
            song_obj.base_card_types = song_obj.card_types.clone();
        }
    }

    runner.cast(dunbarrow).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);
    // The unattached Song is binned by SBAs during setup; it returns via the
    // debug pipeline once there is a victim to enchant.
    assert_eq!(
        runner.state().objects[&song].zone,
        Zone::Graveyard,
        "the unattached Song must be in the graveyard after setup"
    );

    // Two soldiers enter in one batch; stop driving with the boon trigger
    // stacked, before it resolves.
    runner.cast(alarm).commit();
    let mut soldiers: Vec<ObjectId> = Vec::new();
    for _ in 0..100 {
        let entered: Vec<ObjectId> = runner
            .state()
            .objects
            .values()
            .filter(|o| {
                o.is_token
                    && o.controller == P0
                    && o.card_types.subtypes.iter().any(|s| s == "Soldier")
            })
            .map(|o| o.id)
            .collect();
        if entered.len() == 2 && !runner.state().stack.is_empty() {
            soldiers = entered;
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("the soldiers must enter with the boon trigger stacked")
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(soldiers.len(), 2, "both soldiers must have entered");
    soldiers.sort();

    // The Song returns to the battlefield (raw placement: no triggers/SBAs)
    // and enchants the first soldier through the real attach authority.
    runner
        .act(GameAction::Debug(DebugAction::MoveToZone {
            object_id: song,
            to_zone: Zone::Battlefield,
            library_position: None,
            simulate: false,
        }))
        .expect("debug move must succeed");
    attach_to(runner.state_mut(), song, soldiers[0]);

    // Guard: the victim really is a land now — the layers system applied the
    // Song, so the offer-time exclusion below is exercised, not vacuous.
    let victim = &runner.state().objects[&soldiers[0]];
    assert_eq!(
        victim.card_types.core_types,
        vec![CoreType::Land],
        "the Song must genuinely turn the victim into a land"
    );
    assert!(
        victim.card_types.subtypes.iter().any(|s| s == "Forest"),
        "the Song must grant the Forest subtype, got {:?}",
        victim.card_types.subtypes
    );

    // One legal candidate: no prompt — the Role auto-binds to the untouched
    // entrant. Driven explicitly (not `drain_stack`, which breaks silently on
    // an empty stack with a prompt open): any host choice here panics.
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseTokenHost { legal_targets, .. } => panic!(
                "no prompt must appear: the type-changed entrant must be excluded, got {legal_targets:?}"
            ),
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "stack must drain: {:?}",
        runner.state().stack
    );

    let roles = role_tokens(&runner);
    assert_eq!(roles.len(), 1, "one Role must be created");
    let role = &runner.state().objects[&roles[0]];
    assert_eq!(role.zone, Zone::Battlefield);
    assert_eq!(
        role.attached_to,
        Some(AttachTarget::Object(soldiers[1])),
        "the Role must attach to the untouched entrant"
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|o| { o.attached_to != Some(AttachTarget::Object(soldiers[0])) || o.id == song }),
        "nothing but the Song may be attached to the type-changed entrant"
    );
    assert!(held_boons(&runner).is_empty());
}

/// F4: a composed fractional note in a non-self trigger records the EVENT
/// subject's power, not the source's (CR 608.2c + CR 608.2k). The 2-power
/// Chronicler notes half the died 5-power victim's power, rounded up: 3, not
/// the Source-misbound 1. The parse-scope pins live beside the M3-1 parser
/// tests; this is the differing-stats runtime control.
#[test]
fn fractional_note_in_non_self_trigger_records_event_power() {
    const CHRONICLER: &str = "Whenever another creature dies, note half its power, rounded up.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chronicler = scenario
        .add_creature_from_oracle(P0, "Fractional Chronicler", 2, 2, CHRONICLER)
        .id();
    let victim = scenario
        .add_creature_from_oracle(P0, "Mighty Lynx", 5, 2, "")
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    assert_eq!(runner.state().objects[&chronicler].power, Some(2));
    assert_eq!(runner.state().objects[&victim].power, Some(5));

    runner.cast(bolt).target_object(victim).resolve();
    drain_stack(&mut runner);

    assert_eq!(
        runner.state().players[0].noted_number,
        Some(3),
        "half of the DIED creature's 5 power, rounded up — not half of the source's 2"
    );
}

// ---------- B1: Swiftspear's Teachings (indefinite choice grant) ----------

const SWIFT_TEACHINGS: &str = "You get a one-time boon with \"When you cast a creature spell, it gains your choice of prowess or haste.\"\nDraw a card.";

/// Drain the stack, answering a `ChooseOneOfBranch` by branch-description
/// substring when it appears (the choice is made at trigger resolution, not
/// at cast time).
///
/// NOTE (run boon-blockers-20261010 fix round): boon-granted keyword choices
/// bind to the triggering spell via the `affected: TriggeringSource`
/// early-return (the `resolve_branch` push gate skips event-context-bound
/// branches). The driver itself is unchanged.
fn drain_answering_choice(runner: &mut GameRunner, needle: &str) {
    for _ in 0..200 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player, branches, ..
            } => {
                assert_eq!(*player, P0, "the boon holder chooses");
                let index = branches
                    .iter()
                    .position(|branch| {
                        branch
                            .description
                            .as_deref()
                            .is_some_and(|description| description.to_lowercase().contains(needle))
                    })
                    .unwrap_or_else(|| panic!("a {needle} branch must exist"));
                runner
                    .act(GameAction::ChooseBranch { index })
                    .expect("choosing the branch must succeed");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt while draining: {other:?}"),
        }
    }
    panic!("drain loop did not settle");
}

/// Cross cleanup into the next turn's upkeep, declaring no attackers or
/// blockers on the way. Panics on any other non-priority prompt.
fn advance_to_next_upkeep(runner: &mut GameRunner) {
    for _ in 0..64 {
        runner.advance_to_phase(Phase::Upkeep);
        if runner.state().phase == Phase::Upkeep && runner.state().active_player != P0 {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                runner.pass_both_players();
            }
            other => panic!("unexpected prompt while crossing cleanup: {other:?}"),
        }
    }
    panic!("did not reach the next upkeep");
}

/// Walk from the declare-attackers step to the postcombat main phase,
/// declaring no blocks. For combat-trigger tests that must reach a
/// sorcery-speed window past combat.
fn advance_past_combat(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().phase == Phase::PostCombatMain {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("the defending player may always decline to block");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the combat-damage trigger");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is always legal");
            }
            other => panic!("combat stalled on an unexpected prompt: {other:?}"),
        }
        runner.advance_until_stack_empty();
    }
    panic!("combat did not reach the postcombat main phase");
}

/// V1: the undurated choice grant is indefinite — it survives cleanup and its
/// prowess still fires on a later-turn noncreature cast.
///
/// DISCRIMINATING: with the end-of-turn stamp reverted, prowess is pruned at
/// cleanup, so both the post-crossing presence assertion and the later pump
/// assertion fail.
///
/// The installed grant binds to the triggering spell via the
/// `affected: TriggeringSource` early-return (the `resolve_branch` push gate
/// skips event-context-bound branches; run boon-blockers-20261010 fix round).
#[test]
fn swiftspear_choice_grant_survives_cleanup_and_fires_later() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B"]);
    scenario.with_library_top(P1, &["P1 A"]);
    let teachings = scenario
        .add_spell_to_hand_from_oracle(P0, "Swiftspear's Teachings", false, SWIFT_TEACHINGS)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.cast(teachings).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);

    // Fire the boon with a creature cast and take prowess.
    runner.cast(bear).commit();
    drain_answering_choice(&mut runner, "prowess");
    assert!(
        held_boons(&runner).is_empty(),
        "the fired boon must be consumed"
    );
    assert!(
        runner.state().objects[&bear].has_keyword(&Keyword::Prowess),
        "reach guard: prowess is granted the same turn"
    );

    // Cross cleanup: an indefinite grant survives.
    advance_to_next_upkeep(&mut runner);
    assert_eq!(runner.state().active_player, P1);
    assert!(
        runner.state().objects[&bear].has_keyword(&Keyword::Prowess),
        "CR 611.2a: the unstated choice grant must survive cleanup"
    );

    // Prowess still fires on a later-turn noncreature cast: P1 passes so P0
    // (holding priority) can cast its instant.
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to P0");
    runner.cast(bolt).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&bear].power,
        Some(3),
        "prowess must fire next turn: the 2/2 becomes 3/3"
    );
    assert_eq!(runner.state().objects[&bear].toughness, Some(3));
}

/// V1 sibling: the haste branch grants only haste.
///
/// Same event-context binding as the prowess branch above (the `resolve_branch`
/// push gate skips `affected: TriggeringSource` branches; run
/// boon-blockers-20261010 fix round).
#[test]
fn swiftspear_haste_branch_grants_haste_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A"]);
    let teachings = scenario
        .add_spell_to_hand_from_oracle(P0, "Swiftspear's Teachings", false, SWIFT_TEACHINGS)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(teachings).resolve();
    drain_stack(&mut runner);
    runner.cast(bear).commit();
    drain_answering_choice(&mut runner, "haste");

    let obj = &runner.state().objects[&bear];
    assert!(
        obj.has_keyword(&Keyword::Haste),
        "the chosen haste must be granted"
    );
    assert!(
        !obj.has_keyword(&Keyword::Prowess),
        "the unchosen prowess must not be granted"
    );
}

/// V9a: sequential boon firings bind each grant to its own triggering spell
/// (per-firing capture isolation).
///
/// Two teachings boons are held; bear one fires boon one (prowess), then bear
/// two fires boon two (prowess). Each bear carries exactly its own grant: a
/// later bolt grows both to 3/3. If the second firing overwrote a shared slot,
/// the first bear would never have been bound and would stay 2/2.
#[test]
fn swiftspear_sequential_boons_bind_each_grant_to_its_own_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);
    let teachings_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Teachings One", false, SWIFT_TEACHINGS)
        .id();
    let teachings_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Teachings Two", false, SWIFT_TEACHINGS)
        .id();
    let bear_one = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let bear_two = scenario
        .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    // First boon fires on the first bear.
    runner.cast(teachings_one).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);
    runner.cast(bear_one).commit();
    drain_answering_choice(&mut runner, "prowess");
    assert!(
        held_boons(&runner).is_empty(),
        "the first fired boon must be consumed"
    );
    assert!(
        runner.state().objects[&bear_one].has_keyword(&Keyword::Prowess),
        "reach guard: the first bear is granted prowess the same turn"
    );

    // Second boon fires on the second bear.
    runner.cast(teachings_two).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);
    runner.cast(bear_two).commit();
    drain_answering_choice(&mut runner, "prowess");
    assert!(
        held_boons(&runner).is_empty(),
        "the second fired boon must be consumed"
    );
    assert!(
        runner.state().objects[&bear_two].has_keyword(&Keyword::Prowess),
        "reach guard: the second bear is granted prowess the same turn"
    );

    // One bolt fires both bears' own grants.
    advance_to_next_upkeep(&mut runner);
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to P0");
    runner.cast(bolt).target_player(P1).resolve();
    drain_stack(&mut runner);
    for (label, id) in [("first bear", bear_one), ("second bear", bear_two)] {
        assert_eq!(
            runner.state().objects[&id].power,
            Some(3),
            "{label} must fire its own bound prowess: 2/2 becomes 3/3"
        );
        assert_eq!(runner.state().objects[&id].toughness, Some(3));
    }
}

/// Verbatim Oracle text (`client/public/card-data.json`, key `the valeyard`).
const VALEYARD: &str = "If an opponent would face a villainous choice, they face that choice an additional time. (They can make the same or different choices.)\nWhile voting, you may vote an additional time.";

/// V9c (CR 608.2d vs CR 701.55): Swiftspear's "choice of prowess or haste"
/// is an ORDINARY choice, not a villainous-choice instruction — The
/// Valeyard's extra-instance static (CR 701.55c) doubles only "faces a
/// villainous choice", so the boon choice resolves ONCE: exactly one
/// branch prompt, and only the selected keyword is granted. (Pre-fix, the
/// shared choice authority doubled every `ChooseOneOf` and this test
/// required two prompts.)
///
/// The second-instance continuation itself is exercised by the explicitly
/// synthetic `villainous_second_instance_continuation_preserves_trigger_context`
/// in `game::effects::choose_one_of` — no printed card pairs a villainous
/// instruction with a boon-style trigger context.
#[test]
fn swiftspear_ordinary_choice_not_doubled_by_valeyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A"]);
    scenario.add_creature_from_oracle(P1, "The Valeyard", 4, 4, VALEYARD);
    let teachings = scenario
        .add_spell_to_hand_from_oracle(P0, "Swiftspear's Teachings", false, SWIFT_TEACHINGS)
        .id();
    let bear = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.cast(teachings).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);
    runner.cast(bear).commit();
    // Exactly ONE branch prompt is expected: answer it with prowess.
    let mut instances = 0;
    for _ in 0..200 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player, branches, ..
            } => {
                assert_eq!(*player, P0, "the boon holder chooses");
                instances += 1;
                let index = branches
                    .iter()
                    .position(|branch| {
                        branch.description.as_deref().is_some_and(|description| {
                            description.to_lowercase().contains("prowess")
                        })
                    })
                    .unwrap_or_else(|| panic!("a prowess branch must exist"));
                runner
                    .act(GameAction::ChooseBranch { index })
                    .expect("choosing the branch must succeed");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt while draining: {other:?}"),
        }
    }
    assert_eq!(
        instances, 1,
        "the Valeyard must NOT double an ordinary choice: exactly one branch prompt"
    );
    assert!(
        held_boons(&runner).is_empty(),
        "the fired boon must be consumed"
    );
    assert!(
        runner.state().objects[&bear].has_keyword(&Keyword::Prowess),
        "the selected keyword is granted the same turn"
    );
    assert!(
        !runner.state().objects[&bear].has_keyword(&Keyword::Haste),
        "the unselected branch must NOT grant: one prompt, one keyword"
    );

    // The prowess half still fires on a later noncreature cast.
    advance_to_next_upkeep(&mut runner);
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to P0");
    runner.cast(bolt).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&bear].power,
        Some(3),
        "the bound prowess must fire next turn: the 2/2 becomes 3/3"
    );
    assert_eq!(runner.state().objects[&bear].toughness, Some(3));
}

// ---------- B1: Bloodrage Alpha (indefinite trigger grant) ----------

const BLOODRAGE: &str = "When this creature enters, choose one —\n• Another target Wolf or Werewolf you control fights target creature you don't control.\n• You get a one-time boon with \"When you cast a Wolf or Werewolf spell, it gains 'When this creature enters, it fights up to one target creature you don't control.'\"";

/// The granted ChangesZone triggers installed on an object (the wolf prints
/// none, so every one found is the boon's grant).
fn granted_entry_triggers(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .trigger_definitions
        .iter_unchecked()
        .filter(|entry| entry.definition.mode == TriggerMode::ChangesZone)
        .count()
}

/// V3: the undurated trigger grant is indefinite — still installed after a
/// cleanup crossing. A blinked wolf loses it (zone-exit prune).
///
/// DISCRIMINATING: with the end-of-turn stamp reverted, cleanup prunes the
/// grant, so the post-crossing count assertion fails.
///
/// DEVIATION (documented): the plan's "re-entering the Wolf fights again" is
/// not taken literally — a re-entry is itself a zone exit, which prunes the
/// grant by design (the plan's own blink hostile below). The discriminating
/// post-cleanup check is structural (the trigger is still installed); the
/// behavioral proof that an installed trigger fights is the pre-cleanup
/// reach guard.
#[test]
fn bloodrage_granted_trigger_survives_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bloodrage = scenario
        .add_creature_to_hand_from_oracle(P0, "Bloodrage Alpha", 4, 4, BLOODRAGE)
        .with_subtypes(vec!["Wolf"])
        .id();
    let wolf = scenario
        .add_creature_to_hand_from_oracle(P0, "Wolf Cub", 2, 2, "")
        .with_subtypes(vec!["Wolf"])
        .id();
    let wall = scenario.add_creature(P1, "Wall", 0, 4).id();
    let mut runner = scenario.build();

    // ETB modal: take the boon branch (chosen at trigger resolution, when the
    // modal trigger resolves — not at cast time).
    runner.cast(bloodrage).commit();
    let mut mode_chosen = false;
    for _ in 0..200 {
        if mode_chosen
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::AbilityModeChoice {
                player,
                mode_abilities,
                ..
            } => {
                assert_eq!(*player, P0);
                assert!(!mode_chosen, "the modal fires once");
                let index = mode_abilities
                    .iter()
                    .position(|mode| format!("{mode:?}").contains("CreateBoon"))
                    .expect("a boon branch must exist");
                runner
                    .act(GameAction::SelectModes {
                        indices: vec![index],
                    })
                    .expect("selecting the boon mode must succeed");
                mode_chosen = true;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt while resolving ETB: {other:?}"),
        }
    }
    assert!(mode_chosen, "the modal branch must have been chosen");
    assert_eq!(held_boons(&runner), vec![P0]);

    // Fire the boon with a Wolf cast; the granted ETB trigger fights.
    runner.cast(wolf).commit();
    for _ in 0..100 {
        if runner.state().stack.is_empty()
            && !matches!(
                runner.state().waiting_for,
                WaitingFor::OrderTriggers { .. }
                    | WaitingFor::TriggerTargetSelection { .. }
                    | WaitingFor::TargetSelection { .. }
            )
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(wall)),
                    })
                    .expect("choose the fight target");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        held_boons(&runner).is_empty(),
        "the fired boon must be consumed"
    );
    assert_eq!(
        runner.state().objects[&wall].damage_marked,
        2,
        "reach guard: the granted fight dealt 2 to the wall pre-cleanup"
    );
    assert_eq!(
        granted_entry_triggers(&runner, wolf),
        1,
        "reach guard: the wolf carries the granted entry trigger"
    );

    // Cross cleanup: the indefinite grant stays installed.
    advance_to_next_upkeep(&mut runner);
    assert_eq!(
        granted_entry_triggers(&runner, wolf),
        1,
        "CR 611.2a: the unstated trigger grant must survive cleanup"
    );

    // Hostile: a zone exit prunes the object-bound grant.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), wolf, Zone::Exile, &mut events);
    assert_eq!(
        granted_entry_triggers(&runner, wolf),
        0,
        "a blinked wolf must lose the object-bound grant"
    );
}

// ---------- B2: Electrostatic Blast (shared single-use budget) ----------

const ELECTROSTATIC: &str = "Electrostatic Blast deals 2 damage to any target. You get a one-time boon with \"When you cast an instant or sorcery spell, exile the top three cards of your library. You may play one of those cards until end of turn.\"";
const RECKLESS: &str =
    "Exile the top two cards of your library. Until the end of your next turn, you may play those cards.";
const STAGED_BOLT: &str = "Staged Bolt deals 3 damage to any target.";

fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

fn can_play_land(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == id))
}

/// The `single_use` `PlayFromExile` grants on `id` (Locke idiom: reading the
/// permission distinguishes "never installed" from "installed but gated").
fn single_use_grant_durations(runner: &GameRunner, id: ObjectId) -> Vec<Duration> {
    runner.state().objects[&id]
        .casting_permissions
        .iter()
        .filter_map(|permission| match permission {
            CastingPermission::PlayFromExile {
                duration,
                single_use: true,
                ..
            } => Some(duration.clone()),
            _ => None,
        })
        .collect()
}

struct BoonBatch {
    runner: GameRunner,
    spell_a: ObjectId,
    land: ObjectId,
    spell_b: ObjectId,
}

/// Fire one Electrostatic Blast boon over a staged {spell, land, spell} top
/// three. Returns the runner with the batch exiled and granted.
fn electrostatic_fires() -> BoonBatch {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Added bottom-first: each push lands above the previous, so spell A is
    // on top and exiled first — order inside the batch is irrelevant.
    let mut b = scenario.add_spell_to_library_top(P0, "Staged Bolt B", true);
    b.from_oracle_text(STAGED_BOLT);
    b.with_mana_cost(ManaCost::zero());
    let spell_b = b.id();
    let land = scenario.add_land_to_library_top(P0, "Staged Land").id();
    let mut a = scenario.add_spell_to_library_top(P0, "Staged Bolt A", true);
    a.from_oracle_text(STAGED_BOLT);
    a.with_mana_cost(ManaCost::zero());
    let spell_a = a.id();
    let blast = scenario
        .add_spell_to_hand_from_oracle(P0, "Electrostatic Blast", true, ELECTROSTATIC)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.cast(blast).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert_eq!(held_boons(&runner), vec![P0]);
    runner.cast(bolt).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert!(
        held_boons(&runner).is_empty(),
        "the bolt cast must fire the boon"
    );
    BoonBatch {
        runner,
        spell_a,
        land,
        spell_b,
    }
}

fn assert_batch_granted(runner: &GameRunner, spell_a: ObjectId, land: ObjectId, spell_b: ObjectId) {
    for (label, id) in [("spell A", spell_a), ("land", land), ("spell B", spell_b)] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Exile,
            "reach guard ({label}): the batch must be exiled"
        );
        assert_eq!(
            single_use_grant_durations(runner, id),
            vec![Duration::UntilEndOfTurn],
            "reach guard ({label}): every batch member must hold the grant"
        );
    }
    assert!(
        can_cast(runner, spell_a),
        "reach guard: spell A must be castable before the budget is spent"
    );
    assert!(
        can_play_land(runner, land),
        "reach guard: the land must be playable before the budget is spent"
    );
    assert!(
        can_cast(runner, spell_b),
        "reach guard: spell B must be castable before the budget is spent"
    );
}

/// V5a: casting one batch spell spends the shared budget on the cast route —
/// the sibling spell and the land reject on both routes.
///
/// DISCRIMINATING: with the uncapped `CastFromZone` reverted, every member
/// stays playable and the rejection assertions fail.
#[test]
fn electrostatic_one_of_those_spends_shared_budget_on_cast() {
    let BoonBatch {
        mut runner,
        spell_a,
        land,
        spell_b,
    } = electrostatic_fires();
    assert_batch_granted(&runner, spell_a, land, spell_b);

    // Cast spell A through the production pipeline.
    runner.cast(spell_a).target_player(P1).commit();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell_a].zone,
        Zone::Graveyard,
        "reach guard: spell A resolved (CR 608.2n returns it to graveyard)"
    );

    // Both routes reject for the remainder of the batch.
    assert!(
        !can_cast(&runner, spell_b),
        "the sibling spell must reject on the cast route once the budget is spent"
    );
    assert!(
        !can_play_land(&runner, land),
        "the land must reject on the land route once the budget is spent"
    );
    assert_eq!(
        runner.state().objects[&spell_b].zone,
        Zone::Exile,
        "the sibling spell must not have moved"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Exile,
        "the land must not have moved"
    );
    assert!(
        single_use_grant_durations(&runner, spell_b).is_empty()
            && single_use_grant_durations(&runner, land).is_empty(),
        "the spent budget must strip the grant from every sibling"
    );
}

/// V5b: playing the batch land spends the shared budget on the land route —
/// both spells reject on the cast route.
#[test]
fn electrostatic_one_of_those_spends_shared_budget_on_land_play() {
    let BoonBatch {
        mut runner,
        spell_a,
        land,
        spell_b,
    } = electrostatic_fires();
    assert_batch_granted(&runner, spell_a, land, spell_b);

    // Play the land through the production pipeline.
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("the first granted land play must be accepted");
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "reach guard: the land must actually have been played"
    );

    // Both spells reject; neither moved.
    assert!(
        !can_cast(&runner, spell_a) && !can_cast(&runner, spell_b),
        "both spells must reject on the cast route once the land spent the budget"
    );
    for (label, id) in [("spell A", spell_a), ("spell B", spell_b)] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Exile,
            "{label} must not have moved"
        );
        assert!(
            single_use_grant_durations(&runner, id).is_empty(),
            "{label} must lose the spent grant"
        );
    }
}

/// V5-cancel: beginning a batch cast and cancelling leaves the group
/// unconsumed — the sibling is still offered.
#[test]
fn electrostatic_cancelled_cast_leaves_shared_budget_intact() {
    let BoonBatch {
        mut runner,
        spell_a,
        land,
        spell_b,
    } = electrostatic_fires();
    assert_batch_granted(&runner, spell_a, land, spell_b);

    // Begin spell A's cast with a manual announce (parks at its target prompt
    // without the driver's auto-answer), then cancel.
    let card_id = runner.state().objects[&spell_a].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_a,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announcing the begun cast must be accepted");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the begun cast must park at target selection, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancelling the begun cast must succeed");

    // Nothing spent: the card never left exile and the group is intact.
    assert_eq!(
        runner.state().objects[&spell_a].zone,
        Zone::Exile,
        "the cancelled cast must not move its card"
    );
    assert!(
        can_cast(&runner, spell_a) && can_cast(&runner, spell_b),
        "both batch spells must still be offered after the cancel"
    );
    assert_eq!(
        single_use_grant_durations(&runner, spell_a).len(),
        1,
        "the cancelled cast must not consume the group"
    );
}

/// V5-pools: two boons fired in sequence hold independent groups — spending
/// the first batch leaves the second offered.
#[test]
fn electrostatic_sequential_boons_hold_independent_budgets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two batches of two zero-mana spells, staged bottom-first per batch.
    let mut b2 = scenario.add_spell_to_library_top(P0, "Batch2 B", true);
    b2.from_oracle_text(STAGED_BOLT);
    b2.with_mana_cost(ManaCost::zero());
    let batch2_b = b2.id();
    let mut a2 = scenario.add_spell_to_library_top(P0, "Batch2 A", true);
    a2.from_oracle_text(STAGED_BOLT);
    a2.with_mana_cost(ManaCost::zero());
    let batch2_a = a2.id();
    let mut b1 = scenario.add_spell_to_library_top(P0, "Batch1 B", true);
    b1.from_oracle_text(STAGED_BOLT);
    b1.with_mana_cost(ManaCost::zero());
    let batch1_b = b1.id();
    let mut a1 = scenario.add_spell_to_library_top(P0, "Batch1 A", true);
    a1.from_oracle_text(STAGED_BOLT);
    a1.with_mana_cost(ManaCost::zero());
    let batch1_a = a1.id();
    // A filler so the second blast's boon has a distinct top three.
    let mut f = scenario.add_spell_to_library_top(P0, "Filler", true);
    f.from_oracle_text(STAGED_BOLT);
    f.with_mana_cost(ManaCost::zero());
    let filler = f.id();
    let blast_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Blast One", true, ELECTROSTATIC)
        .id();
    let blast_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Blast Two", true, ELECTROSTATIC)
        .id();
    let bolt_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt One", true, BOLT)
        .id();
    let bolt_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Two", true, BOLT)
        .id();
    let mut runner = scenario.build();

    // First boon fires over {filler, batch1 A, batch1 B}.
    runner.cast(blast_one).target_player(P1).resolve();
    drain_stack(&mut runner);
    runner.cast(bolt_one).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert!(
        held_boons(&runner).is_empty(),
        "the first boon must have fired"
    );
    for (label, id) in [
        ("filler", filler),
        ("batch1 A", batch1_a),
        ("batch1 B", batch1_b),
    ] {
        assert_eq!(
            single_use_grant_durations(&runner, id).len(),
            1,
            "reach guard ({label}): the first batch must hold the first grant"
        );
    }

    // Second boon fires over the next top three.
    runner.cast(blast_two).target_player(P1).resolve();
    drain_stack(&mut runner);
    runner.cast(bolt_two).target_player(P1).resolve();
    drain_stack(&mut runner);
    assert!(
        held_boons(&runner).is_empty(),
        "the second boon must have fired"
    );
    for (label, id) in [("batch2 A", batch2_a), ("batch2 B", batch2_b)] {
        assert_eq!(
            single_use_grant_durations(&runner, id).len(),
            1,
            "reach guard ({label}): the second batch must hold the second grant"
        );
    }

    // Spend the first batch; the second stays offered.
    runner.cast(batch1_a).target_player(P1).commit();
    runner.advance_until_stack_empty();
    assert!(
        can_cast(&runner, batch2_a) && can_cast(&runner, batch2_b),
        "spending the first batch must not touch the independent second group"
    );
    assert!(
        !can_cast(&runner, batch1_b),
        "the first batch's own sibling must reject"
    );
}

/// V5-control: the uncapped plural ("those cards", no cap) plays twice —
/// the shared budget is a property of the printed "one of", not of every
/// exile-then-play grant.
#[test]
fn reckless_impulse_uncapped_batch_plays_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_spell_to_library_top(P0, "Reckless B", true);
    b.from_oracle_text(STAGED_BOLT);
    b.with_mana_cost(ManaCost::zero());
    let spell_b = b.id();
    let land = scenario.add_land_to_library_top(P0, "Reckless Land").id();
    let reckless = scenario
        .add_spell_to_hand_from_oracle(P0, "Reckless Impulse", false, RECKLESS)
        .id();
    let mut runner = scenario.build();

    runner.cast(reckless).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell_b].zone,
        Zone::Exile,
        "reach guard: the batch must be exiled"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Exile,
        "reach guard: the land must be exiled"
    );

    // First play succeeds (the land, on the land route).
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("the first uncapped play must be accepted");
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "the land must actually have been played"
    );

    // Second play succeeds too — no shared budget of one here. The land drop
    // is spent, so the second play goes through the cast route.
    assert!(
        can_cast(&runner, spell_b),
        "the uncapped sibling must still be offered after the first play"
    );
    runner.cast(spell_b).target_player(P1).commit();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell_b].zone,
        Zone::Graveyard,
        "the second uncapped play must resolve"
    );
}

// ---------- B2/V6: Nashi, Moon Sage's Scion (rider + expiry) ----------

const NASHI: &str = "Ninjutsu {3}{B} ({3}{B}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever Nashi deals combat damage to a player, exile the top card of each player's library. Until end of turn, you may play one of those cards. If you cast a spell this way, pay life equal to its mana value rather than paying its mana cost.";

/// V6: Nashi's capped batch play promotes with its life-payment rider — one
/// cast succeeds (paying life), the second rejects — and the printed
/// end-of-turn window prunes the remainder at cleanup.
///
/// DISCRIMINATING: with the uncapped `CastFromZone` reverted, the second
/// cast stays offered and the rejection assertion fails.
///
/// The leading window reaches the promotion via the pre-`YouMay` guard
/// (`try_parse_one_of_batch_cast_preface`; run boon-blockers-20261010 fix round).
#[test]
fn nashi_promoted_grant_carries_life_rider_and_single_budget() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nashi = scenario
        .add_creature_from_oracle(P0, "Nashi, Moon Sage's Scion", 2, 2, NASHI)
        .with_subtypes(vec!["Rat", "Ninja"])
        .id();
    // Staged tops: each library contributes one {1} instant.
    let mut s0 = scenario.add_spell_to_library_top(P0, "Staged P0", true);
    s0.from_oracle_text(STAGED_BOLT);
    s0.with_mana_cost(ManaCost::generic(1));
    let staged_p0 = s0.id();
    let mut s1 = scenario.add_spell_to_library_top(P1, "Staged P1", true);
    s1.from_oracle_text(STAGED_BOLT);
    s1.with_mana_cost(ManaCost::generic(1));
    let staged_p1 = s1.id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(nashi, AttackTarget::Player(P1))])
        .expect("Nashi must be able to attack");
    advance_past_combat(&mut runner);

    // Reach guards: both tops exiled, both holding the end-of-turn grant.
    for (label, id) in [("P0 staged", staged_p0), ("P1 staged", staged_p1)] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Exile,
            "reach guard ({label}): combat damage must exile both tops"
        );
        assert_eq!(
            single_use_grant_durations(&runner, id),
            vec![Duration::UntilEndOfTurn],
            "reach guard ({label}): both exiled cards must hold the grant"
        );
        assert!(
            can_cast(&runner, id),
            "reach guard ({label}): both must be offered before the budget is spent"
        );
    }

    // First cast succeeds through the life-payment rider: MV 1 costs 1 life.
    let life_before = runner.state().players[0].life;
    runner.cast(staged_p0).target_player(P1).commit();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&staged_p0].zone,
        Zone::Graveyard,
        "reach guard: the first cast must resolve"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before - 1,
        "the rider replaces mana with life equal to mana value"
    );

    // The second rejects on the shared budget; it never moved.
    assert!(
        !can_cast(&runner, staged_p1),
        "the second batch spell must reject once the budget is spent"
    );
    assert_eq!(
        runner.state().objects[&staged_p1].zone,
        Zone::Exile,
        "the rejected spell must not have moved"
    );
    assert!(
        single_use_grant_durations(&runner, staged_p1).is_empty(),
        "the spent budget must strip the sibling grant"
    );

    // The printed window still ends the remainder at cleanup.
    advance_to_next_upkeep(&mut runner);
    assert!(
        single_use_grant_durations(&runner, staged_p1).is_empty(),
        "the end-of-turn window must prune the remainder at cleanup"
    );
}

// ---------- F2 fixture + source-less boon rows (run boon-followup-20261013) ----------

/// Verbatim Oracle texts (card-data.json `silver knight` / `prodigal
/// pyromancer`, corroborated this run): Silver Knight is 2/2 with first
/// strike + protection from red; Prodigal Pyromancer is 1/1 with the tap
/// ability.
const SILVER_KNIGHT: &str = "First strike, protection from red";
const PRODIGAL_PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";

/// T1 SHAPE gate: the T2/T3 fixture parses to live protection from red with
/// zero unimplemented mechanics. Every negative below (knight excluded /
/// protected) is vacuous without this row.
#[test]
fn silver_knight_parses_to_protection_from_red() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Multi-keyword comma line: pass the MTGJSON-style keyword hints (the
    // card-test recipe for foot-gun #4 — Oracle text stays verbatim).
    let knight = scenario
        .add_creature(P1, "Silver Knight", 2, 2)
        .from_oracle_text_with_keywords(&["first strike", "protection"], SILVER_KNIGHT)
        .id();
    let runner = scenario.build();

    let obj = &runner.state().objects[&knight];
    assert!(
        obj.keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            ))),
        "verbatim Silver Knight text must yield live protection from red, got {:?}",
        obj.keywords
    );
    assert!(
        obj.keywords.contains(&Keyword::FirstStrike),
        "reach guard: first strike must parse alongside protection, got {:?}",
        obj.keywords
    );
    assert!(
        !obj.has_unimplemented_mechanics(),
        "gate: zero unimplemented mechanics (no Unknown keyword / Unimplemented effect)"
    );
}

/// T2: a Molten Impact boon is source-less at gameplay — protection from red
/// neither excludes the knight at targeting nor prevents the damage.
/// Pre-fix this fails twice: no `TriggerTargetSelection` prompt appears (the
/// boon auto-targets the sole legal wall) and the knight survives.
#[test]
fn molten_boon_ignores_protection_from_red() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall_one = scenario
        .add_creature_from_oracle(P1, "Wall One", 0, 2, "")
        .id();
    let knight = scenario
        .add_creature(P1, "Silver Knight", 2, 2)
        .from_oracle_text_with_keywords(&["first strike", "protection"], SILVER_KNIGHT)
        .id();
    let wall_two = scenario
        .add_creature_from_oracle(P1, "Wall Two", 0, 2, "")
        .id();
    // Molten Impact is a red sorcery; the staged card carries no color until
    // stamped, so stamp it (mirrors the plan probe — without red the
    // pre-fix exclusion cannot reproduce and the test is vacuous).
    let mut molten_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Molten Impact", false, MOLTEN);
    molten_builder.with_color(vec![ManaColor::Red]);
    let molten = molten_builder.id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    // Reach guard (T1 shape, live): the knight must carry live protection,
    // or the prompt assertion below passes for the wrong reason.
    assert!(
        runner.state().objects[&knight]
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            ))),
        "reach guard: the staged knight must hold live protection from red"
    );

    // Grant: Molten → Wall One (4 damage, 2 excess noted).
    runner.cast(molten).target_object(wall_one).resolve();
    drain_stack(&mut runner);
    assert_eq!(runner.state().players[0].noted_number, Some(2));
    assert_eq!(held_boons(&runner), vec![P0]);

    // Fire: Bolt → P1 (life-17 control). The boon trigger needs its target.
    // Drive the cast manually rather than via `commit()`: post-fix the boon
    // offers BOTH walls, so the commit driver's required-slot picker would
    // panic on its empty trigger-intent pool (`scenario.rs:3158`) — and
    // declaring the knight as commit intent would auto-answer the very
    // prompt this test must observe. Manual driving surfaces the
    // `TriggerTargetSelection` prompt to the loop below; every assertion
    // below is unchanged.
    let bolt_card = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: bolt_card,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announce Bolt");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            let legal: Vec<TargetRef> = target_slots
                .iter()
                .flat_map(|slot| slot.legal_targets.clone())
                .collect();
            assert!(
                legal.contains(&TargetRef::Player(P1)),
                "reach guard: Bolt (\"any target\") must offer P1, got {legal:?}"
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                })
                .expect("bolt P1");
        }
        other => panic!("expected TargetSelection for Bolt, got {other:?}"),
    }
    let mut prompt_seen = false;
    for _ in 0..100 {
        if runner.state().stack.is_empty()
            && !matches!(
                runner.state().waiting_for,
                WaitingFor::OrderTriggers { .. }
                    | WaitingFor::TriggerTargetSelection { .. }
                    | WaitingFor::TargetSelection { .. }
            )
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                prompt_seen = true;
                let legal: Vec<TargetRef> = target_slots
                    .iter()
                    .flat_map(|slot| slot.legal_targets.clone())
                    .collect();
                assert!(
                    legal.contains(&TargetRef::Object(knight)),
                    "the source-less boon must offer the pro-red knight, got {legal:?}"
                );
                assert!(
                    legal.contains(&TargetRef::Object(wall_two)),
                    "the boon must still offer the unprotected wall, got {legal:?}"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(knight)),
                    })
                    .expect("choose the boon target");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    // The boon dealt the noted 2 to the knight (lethal to the 2/2 — damage
    // NOT prevented); Bolt dealt 3 to P1; the spare wall stands untouched.
    assert!(
        prompt_seen,
        "the boon must prompt for its target: pre-fix it auto-targets the sole legal wall"
    );
    assert_eq!(
        runner.state().objects[&knight].zone,
        Zone::Graveyard,
        "source-less boon damage must ignore protection from red"
    );
    assert_eq!(
        runner.state().objects[&wall_two].zone,
        Zone::Battlefield,
        "hostile: the unchosen wall must stand untouched"
    );
    assert_eq!(runner.state().players[1].life, 17);
    assert!(held_boons(&runner).is_empty());
}

/// T3 over-correction guard: ordinary SOURCED abilities still respect
/// protection from red — the Pyromancer's tap ability and a red Bolt both
/// exclude the knight, while the unprotected wall stays legal. Passes
/// identically with and without the F2 fix (no consumer changes); it flips
/// only if someone "fixes" F2 in the consumers.
#[test]
fn sourced_ability_still_respects_protection_from_red() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = scenario
        .add_creature(P1, "Silver Knight", 2, 2)
        .from_oracle_text_with_keywords(&["first strike", "protection"], SILVER_KNIGHT)
        .id();
    let wall = scenario.add_creature_from_oracle(P1, "Wall", 0, 2, "").id();
    // Prodigal Pyromancer is a red 1/1; stamp the color so the protection
    // has something to read (same honesty rule as the Molten stamp in T2).
    let mut pyro_builder =
        scenario.add_creature_from_oracle(P0, "Prodigal Pyromancer", 1, 1, PRODIGAL_PYROMANCER);
    pyro_builder.with_color(vec![ManaColor::Red]);
    let pyro = pyro_builder.id();
    let mut bolt_builder = scenario.add_spell_to_hand_from_oracle(P0, "Bolt", true, BOLT);
    bolt_builder.with_color(vec![ManaColor::Red]);
    let bolt = bolt_builder.id();
    let mut runner = scenario.build();

    // Reach guards: both fixtures parse (tap ability present; knight protected).
    assert!(
        !runner.state().objects[&pyro].abilities.is_empty(),
        "reach guard: the staged Pyromancer must carry its tap ability"
    );
    assert!(
        runner.state().objects[&knight]
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            ))),
        "reach guard: the staged knight must hold live protection from red"
    );

    // Tap → knight rejected (excluded from the sourced ability's legal set);
    // tap → wall succeeds (reach guard: 1 damage marked, wall survives).
    runner
        .act(GameAction::ActivateAbility {
            source_id: pyro,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announce the Pyromancer activation");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            let legal: Vec<TargetRef> = target_slots
                .iter()
                .flat_map(|slot| slot.legal_targets.clone())
                .collect();
            assert!(
                !legal.contains(&TargetRef::Object(knight)),
                "the sourced red ability must still exclude the pro-red knight, got {legal:?}"
            );
            assert!(
                legal.contains(&TargetRef::Object(wall)),
                "reach guard: the unprotected wall must stay legal, got {legal:?}"
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(wall)),
                })
                .expect("tap the Pyromancer at the wall");
        }
        other => panic!("expected TargetSelection for the tap ability, got {other:?}"),
    }
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&wall].damage_marked,
        1,
        "reach guard: the tapped Pyromancer must deal its 1 to the wall"
    );
    assert_eq!(
        runner.state().objects[&wall].zone,
        Zone::Battlefield,
        "the 0/2 wall survives 1 damage"
    );

    // Hostile: a red SPELL still excludes the knight too — Bolt's prompt must
    // list the wall but not the knight; the wall takes the 3 and dies.
    let bolt_card = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: bolt_card,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announce Bolt");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            let legal: Vec<TargetRef> = target_slots
                .iter()
                .flat_map(|slot| slot.legal_targets.clone())
                .collect();
            assert!(
                !legal.contains(&TargetRef::Object(knight)),
                "the red spell must still exclude the pro-red knight, got {legal:?}"
            );
            assert!(
                legal.contains(&TargetRef::Object(wall)),
                "reach guard: the wall must stay a legal spell target, got {legal:?}"
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(wall)),
                })
                .expect("bolt the wall");
        }
        other => panic!("expected TargetSelection for Bolt, got {other:?}"),
    }
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&wall].zone,
        Zone::Graveyard,
        "reach guard: Bolt must deal its 3 to the wall"
    );
    assert_eq!(
        runner.state().objects[&knight].zone,
        Zone::Battlefield,
        "the knight stands untouched throughout"
    );
}
