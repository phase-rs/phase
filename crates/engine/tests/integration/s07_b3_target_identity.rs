//! S07 Batch 3 — target / identity / misc `AbilityCondition` runtime tests.
//!
//! Each test drives the real cast/activate pipeline (GameScenario + GameRunner
//! builder) and asserts a measured delta that flips when the Batch-3 wiring is
//! reverted. See `.planning/coverage-analysis/S07-PLAN-FINAL.md` §3/§6.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
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
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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

// ─────────────────────────────────────────────────────────────────────────
// Issue #10143 — Yenna's NEGATED name comparison on an activated target filter:
// "Choose target enchantment you control that doesn't have the same name as
// another permanent you control." CR 201.2a + CR 201.2c define the comparison;
// CR 602.2b announces the target and CR 608.2b re-judges it at resolution
// against the live board.
//
// Before the fix the whole relative clause was dropped from Yenna's stored
// target filter (`Typed{[Enchantment], You, []}`), so every enchantment she
// controlled was legal — including ones that share a name with another
// permanent.
// ─────────────────────────────────────────────────────────────────────────

struct YennaBoard {
    runner: GameRunner,
    yenna: ObjectId,
    /// P0's creature named "Gamma" — the NON-enchantment that shares its name
    /// with the enchantment "Gamma" on the hostile board.
    gamma_creature: ObjectId,
    beta: ObjectId,
}

/// The legal targets of the slot the runner is currently asking about.
fn current_legal_targets(runner: &GameRunner) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        }
        | WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => target_slots
            .get(selection.current_slot)
            .map(|slot| slot.legal_targets.clone())
            .unwrap_or_default(),
        other => panic!("expected a target-declaration window, got {other:?}"),
    }
}

/// Sorted names of the legal targets the runner currently offers.
///
/// The engine auto-announces a forced unique target (CR 601.2c: a single
/// possible assignment is not a choice), so every fixture here keeps at least
/// TWO legal targets; that is also what makes the set comparison meaningful.
fn legal_target_names(runner: &GameRunner) -> Vec<String> {
    let mut names: Vec<String> = current_legal_targets(runner)
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => runner.state().objects[id].name.clone(),
            other => panic!("expected object targets, got {other:?}"),
        })
        .collect();
    names.sort();
    names
}

/// Builds the Yenna board and drives the activation to the target-declaration
/// window (CR 602.2b).
///
/// `duplicate_names` selects the hostile board — enchantments "Alpha" ×2 and a
/// creature "Gamma" beside an enchantment "Gamma" — or its unique-name sibling.
/// "Beta" and "Epsilon" are plain (non-Aura) enchantments with unique names, so
/// the narrowed filter still offers two choices and the activation resolves
/// without the untap/scry rider.
fn yenna_name_comparison_board(duplicate_names: bool) -> YennaBoard {
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

    let (alpha_one_name, alpha_two_name, gamma_enchantment_name) = if duplicate_names {
        ("Alpha", "Alpha", "Gamma")
    } else {
        ("Alpha One", "Alpha Two", "Gamma Enchantment")
    };
    let add_enchantment = |scenario: &mut GameScenario, name: &str| {
        let mut builder = scenario.add_creature(P0, name, 0, 0);
        builder.as_enchantment();
        builder.id()
    };
    add_enchantment(&mut scenario, alpha_one_name);
    add_enchantment(&mut scenario, alpha_two_name);
    let gamma_creature = scenario.add_creature(P0, "Gamma", 2, 2).id();
    add_enchantment(&mut scenario, gamma_enchantment_name);
    let beta = add_enchantment(&mut scenario, "Beta");
    // A second uniquely named enchantment keeps the forced-unique auto-announce
    // out of the way, so the legal SET is observable.
    add_enchantment(&mut scenario, "Epsilon");

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&yenna).unwrap().tapped = false;
    runner
        .act(engine::types::actions::GameAction::ActivateAbility {
            source_id: yenna,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Yenna");
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .expect("pay the {2} activation cost");
            }
            WaitingFor::TargetSelection { .. } => break,
            other => panic!("unexpected window before Yenna's target declaration: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "Yenna's activation must reach target declaration, got {:?}",
        runner.state().waiting_for
    );
    YennaBoard {
        runner,
        yenna,
        gamma_creature,
        beta,
    }
}

/// Passes priority (paying the mana cost if the engine still asks for it) until
/// the stack is empty with priority back to a player, returning every event the
/// driven actions emitted.
fn drive_to_empty_stack(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                let result = runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .expect("pay the {2} activation cost");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return events;
                }
                let result = runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .expect("resolve the activation");
                events.extend(result.events);
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected window while resolving Yenna's activation: {other:?}"),
        }
    }
    panic!(
        "Yenna's activation never finished, waiting_for={:?}, stack={:?}",
        runner.state().waiting_for,
        runner.state().stack.len()
    );
}

/// CR 405.1 + CR 608: the stack-entry ids the driven actions pushed and resolved.
fn stack_entry_lifecycle(events: &[GameEvent]) -> (Vec<ObjectId>, Vec<ObjectId>) {
    let mut pushed = Vec::new();
    let mut resolved = Vec::new();
    for event in events {
        match event {
            GameEvent::StackPushed { object_id } => pushed.push(*object_id),
            GameEvent::StackResolved { object_id } => resolved.push(*object_id),
            _ => {}
        }
    }
    (pushed, resolved)
}

fn copy_token_resolutions(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CopyTokenOf,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

fn token_count_named(runner: &GameRunner, name: &str, controller: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.name == name && obj.controller == controller)
        .count()
}

#[test]
fn yenna_target_excludes_same_named_permanents_including_non_enchantments() {
    let mut board = yenna_name_comparison_board(true);
    // Discriminating: at base the filter carries no name comparison, so the
    // legal set is all five enchantments. After the fix "Alpha" (twice) and
    // "Gamma" (shared with a CREATURE you control) are illegal.
    assert_eq!(
        legal_target_names(&board.runner),
        vec!["Beta", "Epsilon"],
        "only the uniquely named enchantments are legal; a same-named non-enchantment \
         permanent you control disqualifies its enchantment namesake (CR 201.2a)"
    );
    // Positive reach-guard: choosing the legal target still resolves the copy.
    board
        .runner
        .act(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.beta)),
        })
        .expect("choose Beta");
    drive_to_empty_stack(&mut board.runner);
    assert!(
        board.runner.state().stack.is_empty(),
        "the activation must leave the stack"
    );
    assert_eq!(
        token_count_named(&board.runner, "Beta", P0),
        1,
        "choosing the legal target must still create the copy token"
    );
}

#[test]
fn yenna_target_allows_every_enchantment_when_names_are_unique() {
    // Sibling of the hostile board: with unique names every enchantment Yenna's
    // controller controls is legal. This proves the narrowed filter still admits
    // targets rather than rejecting everything.
    let mut board = yenna_name_comparison_board(false);
    assert_eq!(
        legal_target_names(&board.runner),
        vec![
            "Alpha One",
            "Alpha Two",
            "Beta",
            "Epsilon",
            "Gamma Enchantment"
        ],
        "no duplicate names → every enchantment is legal"
    );
    // Reach-guard: the window is live and the activation resolves a copy.
    board
        .runner
        .act(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.beta)),
        })
        .expect("choose Beta");
    drive_to_empty_stack(&mut board.runner);
    assert!(board.runner.state().stack.is_empty());
}

#[test]
fn yenna_target_is_rejudged_against_the_live_reference_at_resolution() {
    let mut board = yenna_name_comparison_board(true);
    // (a) announcement-time reach-guard: the two uniquely named enchantments.
    assert_eq!(
        legal_target_names(&board.runner),
        vec!["Beta", "Epsilon"],
        "the uniquely named enchantments are the only legal targets at announcement"
    );
    let mut events = board
        .runner
        .act(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.beta)),
        })
        .expect("choose Beta")
        .events;
    // CR 201.2a: a same-named permanent that arrives after announcement makes
    // the chosen target illegal (CR 608.2b), so the ability does nothing.
    board
        .runner
        .state_mut()
        .objects
        .get_mut(&board.gamma_creature)
        .unwrap()
        .name = "Beta".to_string();
    events.extend(drive_to_empty_stack(&mut board.runner));
    // (b) the perturbation is observable.
    assert_eq!(
        board.runner.state().objects[&board.gamma_creature].name,
        "Beta",
        "the post-announcement rename must take effect"
    );
    // (c) the activation reached the stack and left it (StackResolved is
    // emitted for fizzles too, so this is not an early-return artifact).
    let (pushed, resolved) = stack_entry_lifecycle(&events);
    assert_eq!(
        pushed.len(),
        1,
        "exactly one activation must reach the stack, events={events:?}"
    );
    assert!(
        resolved.contains(&pushed[0]),
        "the activation must have left the stack, events={events:?}"
    );
    // Discriminating: with a latched announcement-time verdict the copy resolves
    // and the token appears.
    assert_eq!(
        copy_token_resolutions(&events, board.yenna),
        0,
        "an illegal target at resolution means the ability does nothing (CR 608.2b), events={events:?}"
    );
    assert_eq!(
        token_count_named(&board.runner, "Beta", P0),
        0,
        "no copy token may be created after the target became illegal"
    );
}

#[test]
fn yenna_target_stays_legal_when_a_different_name_appears_at_resolution() {
    // Negative sibling for the re-read above: a DIFFERENT name arriving after
    // announcement does not make the chosen target illegal, so the copy still
    // resolves. Without this row the fizzle row could pass for the wrong reason
    // (e.g. a mutation that breaks the ability outright).
    let mut board = yenna_name_comparison_board(true);
    let mut events = board
        .runner
        .act(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.beta)),
        })
        .expect("choose Beta")
        .events;
    board
        .runner
        .state_mut()
        .objects
        .get_mut(&board.gamma_creature)
        .unwrap()
        .name = "Delta".to_string();
    events.extend(drive_to_empty_stack(&mut board.runner));
    assert_eq!(
        board.runner.state().objects[&board.gamma_creature].name,
        "Delta"
    );
    let (pushed, resolved) = stack_entry_lifecycle(&events);
    assert_eq!(pushed.len(), 1, "exactly one activation, events={events:?}");
    assert!(
        resolved.contains(&pushed[0]),
        "the activation must have left the stack, events={events:?}"
    );
    assert_eq!(
        copy_token_resolutions(&events, board.yenna),
        1,
        "a differently named permanent does not make the chosen target illegal, events={events:?}"
    );
    assert_eq!(
        token_count_named(&board.runner, "Beta", P0),
        1,
        "the copy must resolve"
    );
}
