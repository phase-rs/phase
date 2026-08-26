//! S07 Batch 3 — target / identity / misc `AbilityCondition` runtime tests.
//!
//! Each test drives the real cast/activate pipeline (GameScenario + GameRunner
//! builder) and asserts a measured delta that flips when the Batch-3 wiring is
//! reverted. See `.planning/coverage-analysis/S07-PLAN-FINAL.md` §3/§6.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::counter::CounterType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&kind).copied())
        .unwrap_or(0)
}

fn is_suspected(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.is_suspected)
        .unwrap_or(false)
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

fn power(runner: &GameRunner, id: ObjectId) -> Option<i32> {
    runner.state().objects[&id].power
}

// NOTE: Malamet Battle Glyph is STOP-AND-RETURN (see the S07 Batch-3 report).
// Its "put a +1/+1 counter on the creature you control if it entered this turn"
// cannot be verified at runtime: the pre-existing "two targets, then put a
// counter on the creature you control" class (shared with Longstalk Brawl / Duel
// for Dominance / Tail Swipe) propagates ONLY the most-recent target to the
// counter sub-ability, so the counter — and any condition reading targets[0] —
// binds to the OPPONENT's creature, not the you-control creature. The condition
// recognizer was reverted so Malamet stays honestly RED until that target-model
// gap is fixed.

// ─────────────────────────────────────────────────────────────────────────
// Fear of Immobility — "tap up to one target creature. If an opponent controls
// that creature, put a stun counter on it."
// ─────────────────────────────────────────────────────────────────────────
const FEAR: &str = "When this creature enters, tap up to one target creature. \
If an opponent controls that creature, put a stun counter on it.";

/// Casts Fear tapping `tap_target`; returns its stun-counter count.
fn run_fear(target_is_opponents: bool) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim_owner = if target_is_opponents { P1 } else { P0 };
    let victim = scenario.add_creature(victim_owner, "Victim", 2, 2).id();
    let fear = scenario
        .add_creature_to_hand_from_oracle(P0, "Fear of Immobility", 2, 2, FEAR)
        .id();
    let mut runner = scenario.build();
    runner.cast(fear).target_object(victim).resolve();
    counters(&runner, victim, CounterType::Stun)
}

#[test]
fn fear_stuns_only_opponent_controlled_tapped_creature() {
    assert_eq!(
        run_fear(true),
        1,
        "opponent controls the tapped creature → stun counter (CR 122.1d)"
    );
    assert_eq!(
        run_fear(false),
        0,
        "you control the tapped creature → no stun counter"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Eliminate the Impossible — "Creatures your opponents control get -2/-0 ...
// If any of them are suspected, they're no longer suspected."
// ─────────────────────────────────────────────────────────────────────────
const ELIMINATE: &str =
    "Investigate. Creatures your opponents control get -2/-0 until end of turn. \
If any of them are suspected, they're no longer suspected.";

#[test]
fn eliminate_unsuspects_opponent_creatures_and_pumps_them() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opp_suspected = scenario.add_creature(P1, "OppSuspected", 4, 4).id();
    let opp_clean = scenario.add_creature(P1, "OppClean", 4, 4).id();
    let mine = scenario.add_creature(P0, "Mine", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eliminate the Impossible", false, ELIMINATE)
        .id();
    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&opp_suspected).unwrap().is_suspected = true;
        // A suspected creature YOU control must be untouched (opponent scope).
        s.objects.get_mut(&mine).unwrap().is_suspected = true;
    }
    runner.cast(spell).resolve();

    // Discriminating: reverting the population-anaphor rewrite leaves
    // Unsuspect{ParentTarget, Single} (a no-op over a non-targeting PumpAll),
    // so the opponent's suspected creature would stay suspected.
    assert!(
        !is_suspected(&runner, opp_suspected),
        "opponent's suspected creature is un-suspected (CR 701.60a)"
    );
    // Scope: your suspected creature is unaffected (opponent-only).
    assert!(
        is_suspected(&runner, mine),
        "your own suspected creature is NOT touched"
    );
    // The -2/-0 lands on opponents' creatures (both), not yours.
    assert_eq!(power(&runner, opp_suspected), Some(2), "opponent -2/-0");
    assert_eq!(power(&runner, opp_clean), Some(2), "opponent -2/-0");
    assert_eq!(power(&runner, mine), Some(4), "your creature unchanged");
}

// ─────────────────────────────────────────────────────────────────────────
// Yenna, Redtooth Regent — "Create a token that's a copy of it ... If the token
// is an Aura, untap Yenna, then scry 2." {2},{T} activated; the {T} cost taps
// Yenna, the Aura-gated untap reverses it only when the copy is an Aura.
// ─────────────────────────────────────────────────────────────────────────
const YENNA: &str =
    "{2}, {T}: Choose target enchantment you control that doesn't have the same name \
as another permanent you control. Create a token that's a copy of it, except it isn't legendary. \
If the token is an Aura, untap Yenna, then scry 2. Activate only as a sorcery.";

/// Returns whether Yenna is tapped after activating, copying an enchantment that
/// is (or isn't) an Aura. Yenna is tapped by the {T} cost; the Aura-gated untap
/// reverses it only in the Aura case.
fn run_yenna(copy_is_aura: bool) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // {2} generic for the activation cost.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let yenna = scenario
        .add_creature_from_oracle(P0, "Yenna, Redtooth Regent", 3, 4, YENNA)
        .id();
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let mut charm = scenario.add_creature(P0, "Charm", 0, 0);
    charm.as_enchantment();
    if copy_is_aura {
        charm.with_subtypes(vec!["Aura"]);
    }
    let ench = charm.id();
    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&yenna).unwrap().tapped = false;
        if copy_is_aura {
            // CR 704.5m: an unattached Aura is put into its owner's graveyard by SBAs before the ability
            // resolves. Attach it to a host so it stays a legal copy source.
            s.objects.get_mut(&ench).unwrap().attached_to =
                Some(engine::game::game_object::AttachTarget::Object(host));
        }
    }
    runner
        .act(engine::types::actions::GameAction::ActivateAbility {
            source_id: yenna,
            ability_index: 0,
        })
        .expect("activate Yenna");
    for _ in 0..40 {
        match &runner.state().waiting_for {
            engine::types::game_state::WaitingFor::ManaPayment { .. } => {
                runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .expect("mana");
            }
            engine::types::game_state::WaitingFor::TargetSelection { .. } => {
                runner
                    .act(engine::types::actions::GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(ench)),
                    })
                    .expect("choose enchantment");
            }
            engine::types::game_state::WaitingFor::ScryChoice { cards, .. } => {
                let cards = cards.clone();
                runner
                    .act(engine::types::actions::GameAction::SelectCards { cards })
                    .expect("scry keep");
            }
            engine::types::game_state::WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .expect("resolve");
            }
            other => panic!("yenna unexpected window: {other:?}"),
        }
    }
    tapped(&runner, yenna)
}

#[test]
fn yenna_untaps_only_when_copied_token_is_an_aura() {
    // Aura copy → untap fires → Yenna ends untapped (the {T} cost tap reversed).
    assert!(
        !run_yenna(true),
        "copying an Aura → untap Yenna (token-is-Aura gate true)"
    );
    // Non-Aura copy → untap does NOT fire → Yenna stays tapped from the cost.
    assert!(
        run_yenna(false),
        "copying a non-Aura enchantment → no untap → Yenna stays tapped"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Steer Clear — "deals 2 damage to target attacking or blocking creature. ...
// deals 4 damage to that creature instead if you controlled a Mount as you cast
// this spell." The line-level strip_instead_clause multi-sentence guard defers
// the "instead if <as-cast condition>" to the chain, which builds the
// ConditionInstead{ControllerControlledMatchingAsCast{Mount}}.
// ─────────────────────────────────────────────────────────────────────────
const STEER: &str = "Steer Clear deals 2 damage to target attacking or blocking creature. \
Steer Clear deals 4 damage to that creature instead if you controlled a Mount as you cast this spell.";

fn run_steer(control_mount: bool) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );
    // P0's attacker survives 4 damage so damage_marked stays observable.
    let attacker = scenario.add_creature(P0, "Attacker", 2, 9).id();
    if control_mount {
        let mut m = scenario.add_creature(P0, "Steed", 2, 2);
        m.with_subtypes(vec!["Mount"]);
    }
    let steer = scenario
        .add_spell_to_hand_from_oracle(P0, "Steer Clear", true, STEER)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declare attacker");
    // Cast Steer Clear at instant speed targeting the attacking creature.
    runner.cast(steer).target_object(attacker).resolve();
    runner.state().objects[&attacker].damage_marked
}

#[test]
fn steer_clear_deals_four_with_mount_two_without() {
    assert_eq!(
        run_steer(true),
        4,
        "controlled a Mount as you cast → 4 damage instead (CR 608.2c)"
    );
    assert_eq!(
        run_steer(false),
        2,
        "no Mount controlled as you cast → base 2 damage"
    );
    // Discriminating: reverting the strip_instead_clause multi-sentence guard
    // drops the condition, so BOTH damage instructions fire sequentially (6);
    // both asserts above (4 and 2) then fail.
}

// ─────────────────────────────────────────────────────────────────────────
// Charging Hooligan — "Whenever this creature attacks, it gets +1/+0 ... for
// each attacking creature. If a Rat is attacking, this creature gains trample."
// ─────────────────────────────────────────────────────────────────────────
const HOOLIGAN: &str =
    "Whenever this creature attacks, it gets +1/+0 until end of turn for each attacking creature. \
If a Rat is attacking, this creature gains trample until end of turn.";

fn run_hooligan(rat_attacks: bool) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hooligan = scenario
        .add_creature_from_oracle(P0, "Charging Hooligan", 3, 3, HOOLIGAN)
        .id();
    let rat = {
        let mut r = scenario.add_creature(P0, "Ratling", 1, 1);
        r.with_subtypes(vec!["Rat"]);
        r.id()
    };
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut attacks = vec![(hooligan, engine::game::combat::AttackTarget::Player(P1))];
    if rat_attacks {
        attacks.push((rat, engine::game::combat::AttackTarget::Player(P1)));
    }
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
    // Resolve the attack trigger.
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(engine::types::actions::GameAction::PassPriority)
            .expect("resolve trigger");
    }
    engine::game::keywords::has_keyword(
        &runner.state().objects[&hooligan],
        &engine::types::keywords::Keyword::Trample,
    )
}

#[test]
fn charging_hooligan_gains_trample_only_when_a_rat_is_attacking() {
    assert!(
        run_hooligan(true),
        "a Rat is attacking → Charging Hooligan gains trample (CR 702.19)"
    );
    assert!(!run_hooligan(false), "no Rat attacking → no trample");
    // Discriminating: reverting parse_a_type_is_in_combat makes the whole trigger
    // unparseable (empty abilities), so trample is never granted → the positive
    // assert flips.
}
