//! F1 wither/infect-to-creature counter-pause resume (PR #10016 follow-up).
//!
//! CR 120.3d + CR 702.80a + CR 616.1: a wither/infect-to-creature leg whose
//! `-1/-1` counter placement meets two co-applicable counter *modifiers*
//! (Winding Constrictor +1 and Doubling Season x2 force a CR 616.1 order)
//! parks a `NeedsChoice` pause. Pre-fix the leg is dropped: no deathtouch
//! flag, no `DamageDealt`, no CR 120.1 record, no lifelink — the resume below
//! preserves the leg across the pause, exactly once, on both the combat and
//! the non-combat paths.
//!
//! Fixture note (see the executor report): the reviewed plan paired Solemnity
//! prevention with the Constrictor modifier, but CR 614.17c ("if an event
//! can't happen, other replacement/prevention effects can't modify or replace
//! it") short-circuits that pairing to `Prevented` with no prompt — the
//! pipeline implements this correctly, so the pairing can never pause. The
//! rows below use two modifiers instead; the orders diverge arithmetically
//! (documented per order), which proves the CR 616.1 choice is real.
//!
//! Rows (every Oracle string below is verbatim card-data export text, except
//! the bare-`"Wither"` source shorthand documented at `WITHER_ORACLE`, and
//! the explicit keyword hints documented at `DARKSTEEL_COLOSSUS_ORACLE`):
//! - combat Doubling-first / Constrictor-first (both CR 616.1 orders, V10):
//!   prompt reach-guard, `DamageDealt` + record exactly once, lifelink exactly
//!   once, deathtouch flag exactly once with no marking; the victim is an
//!   indestructible Darksteel Colossus (CR 702.12) so its survival under a
//!   deathtouch source is rules-correct (the #11019 SBA gap is not pinned).
//! - V0 Necroskitter second vehicle: wither-keyworded, exercising the same
//!   `has_wither || has_infect` arm.
//! - V12 Constrictor-only single candidate: no prompt, remainder inline.
//! - non-combat single target (Puncture Blast) + multi target (Village
//!   Pillagers ETB, remaining chain resumes after the pause).
//! - V13 mid-pause save round-trip + old-save default compat.

use crate::support::shared_card_db as load_db;
use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{ReplacementCandidateSummary, ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Doubling Season's VERBATIM Oracle text (card-data export; the token line is
/// inert here — no tokens are created).
const DOUBLING_SEASON_ORACLE: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

/// Winding Constrictor's VERBATIM Oracle text ({B}{G} 2/3).
const WINDING_CONSTRICTOR_ORACLE: &str = "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.";

/// Wither-source Oracle shorthand (4/2 Smoldering Butcher; 1/4 Necroskitter).
/// The harness's keyword-hint inference only recognizes bare keyword lines, so
/// full Oracle text with reminder parentheticals does not grant the keyword
/// here — the darksteel file uses this same bare-`"Wither"` idiom for its
/// wither attackers (R0/R6), while the counter-replacement Oracles below stay
/// fully verbatim. Real names + stats preserved.
const WITHER_ORACLE: &str = "Wither";

/// Darksteel Colossus' VERBATIM Oracle text (card-data export; 11/11). The
/// keyword hints ride along explicitly (the darksteel-angel `add_creature` +
/// `from_oracle_text_with_keywords` idiom) because the reminder parentheticals
/// defeat the harness's bare-line inference. The graveyard-shuffle replacement
/// is inert here — nothing leaves the battlefield except by SBA destruction,
/// which indestructible does not stop.
const DARKSTEEL_COLOSSUS_ORACLE: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nIndestructible (Damage and effects that say \"destroy\" don't destroy this creature.)\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";

/// Gideon of the Trials' VERBATIM Oracle text (card-data export; loyalty 3).
/// Only the middle [0] matters here (animation + prevention shield); the other
/// two abilities ride along so the fixture parses the real card, not an
/// excerpt.
const GIDEON_OF_THE_TRIALS_ORACLE: &str = "[+1]: Until your next turn, prevent all damage target permanent would deal.\n[0]: Until end of turn, Gideon becomes a 4/4 Human Soldier creature with indestructible that's still a planeswalker. Prevent all damage that would be dealt to him this turn.\n[0]: You get an emblem with \"As long as you control a Gideon planeswalker, you can't lose the game and your opponents can't win the game.\"";

/// Sickle Ripper's VERBATIM Oracle text (card-data export; 2/1). The wither
/// keyword itself is granted via `.wither()` (same harness hint-inference gap
/// as the Pillagers/Puncture vehicles above); the damage line stays verbatim.
const SICKLE_RIPPER_ORACLE: &str =
    "Wither (This deals damage to creatures in the form of -1/-1 counters.)";

/// Bonecrusher Giant // Stomp faces (card-data export, via `add_real_card` +
/// `assert_oracle` so the test runs the printed adventure, not an excerpt).
const BONECRUSHER: &str = "Bonecrusher Giant";
const BONECRUSHER_ORACLE: &str = "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.";
const STOMP: &str = "Stomp";
const STOMP_ORACLE: &str =
    "Damage can't be prevented this turn. Stomp deals 2 damage to any target.";

/// Village Pillagers' VERBATIM Oracle text (5/5). The wither keyword itself is
/// granted via `.wither()` (same harness hint-inference gap as above); the
/// damage lines — the mechanism under test — stay fully verbatim.
const VILLAGE_PILLAGERS_ORACLE: &str = "Wither (This deals damage to creatures in the form of -1/-1 counters.)\nWhen this creature enters, it deals 1 damage to each creature your opponents control.\nWhenever a creature an opponent controls with a counter on it dies, you create a tapped Treasure token.";

/// Puncture Blast's VERBATIM Oracle text ({2}{R} instant). Same wither-grant
/// arrangement as Village Pillagers.
const PUNCTURE_BLAST_ORACLE: &str = "Wither (This deals damage to creatures in the form of -1/-1 counters.)\nPuncture Blast deals 3 damage to any target.";

fn mana(kinds: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .flat_map(|(kind, n)| {
            std::iter::repeat_with(move || ManaUnit::new(*kind, ObjectId(0), false, vec![]))
                .take(*n)
        })
        .collect()
}

fn counter_count(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(kind).copied())
        .unwrap_or(0)
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .unwrap_or(-999)
}

/// P1 attacks, P0 blocks, damage resolves: the shared combat driver. Caller
/// sets `active_player = P1` first.
fn run_combat_with_blocks(
    runner: &mut GameRunner,
    attackers: &[(ObjectId, AttackTarget)],
    blockers: &[(ObjectId, ObjectId)],
) -> Outcome {
    runner.advance_to_combat();
    runner
        .declare_attackers(attackers)
        .expect("declare attackers");
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    runner.declare_blockers(blockers).expect("declare blockers");
    runner.combat_damage()
}

/// The parked CR 616.1 counter-ordering prompt, or `None`.
fn prompt(
    runner: &GameRunner,
) -> Option<(
    PlayerId,
    ReplacementChoiceKind,
    Vec<ReplacementCandidateSummary>,
)> {
    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            player,
            kind,
            candidates,
            ..
        } => Some((player, kind, candidates)),
        _ => None,
    }
}

/// Reach-guard, then answer a CR 616.1 counter-ordering prompt by applying the
/// `first`-sourced replacement before the `second`-sourced one. Returns the
/// `act` events so the caller can assert the resumed remainder.
fn answer_counter_order(
    runner: &mut GameRunner,
    chooser: PlayerId,
    first: ObjectId,
    second: ObjectId,
) -> Vec<GameEvent> {
    let (actual_chooser, kind, candidates) = prompt(runner).unwrap_or_else(|| {
        panic!(
            "expected a CR 616.1 counter-ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        kind,
        ReplacementChoiceKind::Order,
        "reach-guard: expected an ordering prompt, got {candidates:?}"
    );
    assert_eq!(
        actual_chooser, chooser,
        "reach-guard: CR 616.1 names the damaged creature's controller, got {candidates:?}"
    );
    assert_eq!(
        candidates.len(),
        2,
        "reach-guard: Doubling Season + Constrictor must both apply, got {candidates:?}"
    );
    assert!(
        candidates.iter().any(|c| c.source_id == first)
            && candidates.iter().any(|c| c.source_id == second),
        "reach-guard: the prompt must hold both counter replacements, got {candidates:?}"
    );
    let index = candidates
        .iter()
        .position(|c| c.source_id == first)
        .expect("the first-applied source is present");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the ordering pick must be accepted")
        .events
}

/// CR 120.1 record count for one damage leg.
fn record_count(
    runner: &GameRunner,
    source: ObjectId,
    target: &TargetRef,
    is_combat: bool,
) -> usize {
    runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|r| r.source_id == source && &r.target == target && r.is_combat == is_combat)
        .count()
}

/// `DamageDealt` events for one damage leg across all collected events.
fn damage_dealt_events<'a>(
    events: &'a [GameEvent],
    target: &TargetRef,
    is_combat: bool,
) -> Vec<&'a GameEvent> {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::DamageDealt {
                    target: t,
                    is_combat: c,
                    ..
                } if t == target && *c == is_combat
            )
        })
        .collect()
}

/// Pump priority until combat settles at end-of-combat or post-combat main —
/// and no further. Pumping into Cleanup would run the turn-based
/// remove-all-damage action (CR 514.2), wiping the very `damage_marked == 0`
/// and deathtouch-flag assertions the rows below make.
fn advance_past_combat_damage(runner: &mut GameRunner) {
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                if matches!(
                    runner.state().phase,
                    Phase::EndCombat | Phase::PostCombatMain
                ) {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
    panic!("combat did not settle within the bounded pump");
}

/// Answer every CR 616.1 counter-ordering prompt that opens (multi-leg
/// non-combat batches re-prompt per leg), always applying `first` before
/// `second`. Returns (prompts answered, resumed events).
fn answer_all_counter_orders(
    runner: &mut GameRunner,
    chooser: PlayerId,
    first: ObjectId,
    second: ObjectId,
) -> (usize, Vec<GameEvent>) {
    let mut answered = 0;
    let mut events = Vec::new();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                events.extend(answer_counter_order(runner, chooser, first, second));
                answered += 1;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while draining orders: {other:?}"),
        }
    }
    (answered, events)
}

/// Shared combat board: P0 holds Doubling Season + Winding Constrictor + an
/// indestructible Darksteel Colossus (11/11, so the doubled wither counters
/// never kill it by toughness either — Butcher's 4 damage doubles to at most
/// 10; see the order math in each row); P1 holds a wither attacker granted
/// deathtouch + lifelink directly (no Equip drive — the wither flag still
/// comes from the real wither card, isolating the mechanism under test).
///
/// CR 702.12: the victim is indestructible so its survival under a deathtouch
/// source is rules-correct (CR 702.2b would otherwise destroy it — a
/// pre-existing engine gap, filed as #11019, that these rows must not pin).
fn combat_board(
    scenario: &mut GameScenario,
    attacker_name: &str,
    power: i32,
    toughness: i32,
) -> (ObjectId, ObjectId, ObjectId, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    let doubling = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE)
        .id();
    let constrictor = scenario
        .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE)
        .id();
    let mut victim = scenario.add_creature(P0, "Darksteel Colossus", 11, 11);
    victim
        .from_oracle_text_with_keywords(&["Trample", "Indestructible"], DARKSTEEL_COLOSSUS_ORACLE);
    let colossus = victim.id();
    let attacker = scenario
        .add_creature_from_oracle(P1, attacker_name, power, toughness, WITHER_ORACLE)
        .with_keyword(Keyword::Deathtouch)
        .with_keyword(Keyword::Lifelink)
        .id();
    (doubling, constrictor, colossus, attacker)
}

/// CR 120.3d + CR 616.1 + CR 120.1 + CR 702.15b + CR 702.2b, combat,
/// Doubling-first order: the paused wither leg resumes with its deathtouch
/// flag, `DamageDealt` event, CR 120.1 record, and lifelink — exactly once.
///
/// Order math (4 wither damage): Doubling first 4x2=8, then Constrictor +1 =
/// 9 counters. The 11/11 Colossus survives (11-9=2), and CR 702.12
/// indestructible makes that survival rules-correct under deathtouch (the
/// pre-existing SBA gap filed as #11019 is not pinned here).
///
/// REVERT-FAILING ASSERTIONS: at base the leg is dropped, so the record count
/// is 0 (not 1), no `DamageDealt` names the Colossus, and P1's life stays
/// 20 (not 24). The prompt reach-guard passes at base (the pause exists); the
/// remainder assertions redden.
#[test]
fn combat_wither_pause_resumes_doubling_first() {
    let mut scenario = GameScenario::new();
    let (doubling, constrictor, colossus, butcher) =
        combat_board(&mut scenario, "Smoldering Butcher", 4, 2);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(colossus);
    run_combat_with_blocks(
        &mut runner,
        &[(butcher, AttackTarget::Player(P0))],
        &[(colossus, butcher)],
    );

    // V1 reach-guard: the wither AddCounter event really meets two
    // replacements (unconditional — an `if let` would skip the covered leg).
    let resume_events = answer_counter_order(&mut runner, P0, doubling, constrictor);
    advance_past_combat_damage(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the batch must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the resume"
    );
    // V10: Doubling-first order lands exactly 9 counters (4x2+1). The
    // divergent count (9 here vs 10 Constrictor-first) proves the CR 616.1
    // choice is real, not an identity order.
    assert_eq!(
        counter_count(&runner, colossus, &CounterType::Minus1Minus1),
        9,
        "Doubling-first must double the 4 wither damage and add one"
    );
    // V2: the resumed leg emits DamageDealt + the CR 120.1 record exactly once.
    let dealt = damage_dealt_events(&resume_events, &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the resume must emit exactly one DamageDealt for the paused leg"
    );
    // Excess parity with the never-paused path (pinned in V12): the shared
    // excess formula reads unmarked damage, so a deathtouch wither leg
    // reports `actual - 1` (4 - 1 = 3) even with `damage_marked == 0`.
    // Pre-existing semantic, preserved verbatim by the core — the resume
    // must recompute it identically, not "fix" it.
    assert!(
        matches!(
            dealt[0],
            GameEvent::DamageDealt {
                amount: 4,
                excess: 3,
                ..
            }
        ),
        "the resumed DamageDealt carries the full dealt amount with formula-parity excess, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, butcher, &target, true),
        1,
        "CR 120.1 must record the paused leg exactly once"
    );
    // V3: lifelink exactly once — the paused leg's folded share drains via
    // the lifelink tail (F1 split: the remnant runs the core only).
    assert_eq!(
        life_of(&runner, P1),
        24,
        "lifelink must gain exactly the 4 dealt (20 + 4)"
    );
    // V4: deathtouch flag exactly once, no marking. Survival is asserted
    // because CR 702.12 indestructible makes it rules-correct (not because
    // the engine destroys deathtouch victims — that gap is #11019).
    assert!(
        runner.state().objects[&colossus].dealt_deathtouch_damage,
        "CR 702.2b must mark the resumed leg's deathtouch source"
    );
    assert_eq!(
        runner.state().objects[&colossus].damage_marked,
        0,
        "CR 120.3d: wither damage is never marked"
    );
    assert_eq!(
        runner.state().objects[&colossus].zone,
        Zone::Battlefield,
        "indestructible CR 702.12 must keep the Colossus on the battlefield"
    );
}

/// CR 616.1 order convergence (V10): Constrictor-first must leave the same
/// remainder rows V2–V4 holding, with its own order math — Constrictor first
/// 4+1=5, then Doubling x2 = 10 counters (vs 9 Doubling-first).
#[test]
fn combat_wither_pause_resumes_constrictor_first() {
    let mut scenario = GameScenario::new();
    let (doubling, constrictor, colossus, butcher) =
        combat_board(&mut scenario, "Smoldering Butcher", 4, 2);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(colossus);
    run_combat_with_blocks(
        &mut runner,
        &[(butcher, AttackTarget::Player(P0))],
        &[(colossus, butcher)],
    );

    let resume_events = answer_counter_order(&mut runner, P0, constrictor, doubling);
    advance_past_combat_damage(&mut runner);

    assert_eq!(
        counter_count(&runner, colossus, &CounterType::Minus1Minus1),
        10,
        "Constrictor-first must add one to the 4 wither damage and double it"
    );
    assert_eq!(
        damage_dealt_events(&resume_events, &target, true).len(),
        1,
        "the resume must emit exactly one DamageDealt either order"
    );
    assert_eq!(
        record_count(&runner, butcher, &target, true),
        1,
        "CR 120.1 must record the paused leg exactly once either order"
    );
    assert_eq!(
        life_of(&runner, P1),
        24,
        "lifelink must gain exactly the 4 dealt either order"
    );
    assert!(
        runner.state().objects[&colossus].dealt_deathtouch_damage,
        "the deathtouch flag must set either order"
    );
    assert_eq!(
        runner.state().objects[&colossus].damage_marked,
        0,
        "CR 120.3d: wither damage is never marked either order"
    );
    assert_eq!(
        runner.state().objects[&colossus].zone,
        Zone::Battlefield,
        "indestructible CR 702.12 must keep the Colossus either order"
    );
}

/// V0 second-vehicle arm-identity row (N1): Necroskitter is wither-keyworded
/// (not Butcher), exercising the same `has_wither || has_infect` arm. Asserts
/// the resumed `DamageDealt` + record; lifelink/flag siblings live in V2–V4.
///
/// REVERT-FAILING ASSERTION: the record count (1 vs 0 at base).
#[test]
fn necroskitter_second_vehicle_arm_identity() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doubling = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE)
        .id();
    let constrictor = scenario
        .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE)
        .id();
    let shieldmate = scenario.add_creature(P0, "Shieldmate", 2, 8).id();
    // No deathtouch/lifelink grant: the arm identity is the point, not the
    // riders (Necroskitter's dies trigger is out of scope — nothing dies).
    let skitter = scenario
        .add_creature_from_oracle(P1, "Necroskitter", 1, 4, WITHER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(shieldmate);
    run_combat_with_blocks(
        &mut runner,
        &[(skitter, AttackTarget::Player(P0))],
        &[(shieldmate, skitter)],
    );

    let resume_events = answer_counter_order(&mut runner, P0, doubling, constrictor);
    advance_past_combat_damage(&mut runner);

    // Order math (1 wither damage, Doubling-first): 1x2+1 = 3 counters.
    assert_eq!(
        counter_count(&runner, shieldmate, &CounterType::Minus1Minus1),
        3,
        "Doubling-first must double the 1 wither damage and add one"
    );
    let dealt = damage_dealt_events(&resume_events, &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the resume must emit exactly one DamageDealt for the paused leg"
    );
    assert!(
        matches!(dealt[0], GameEvent::DamageDealt { amount: 1, .. }),
        "the resumed DamageDealt carries the 1 dealt, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, skitter, &target, true),
        1,
        "CR 120.1 must record the paused leg exactly once"
    );
    // No survival assertion: no deathtouch rides this leg, and the 3-counter
    // count above already proves the victim is still present.
}

/// V12 single-candidate sibling: Constrictor alone auto-applies (no prompt)
/// and the remainder runs inline — 5 counters (4+1) with V2–V4 holding.
/// Green before and after the fix (characterization of the 1-candidate path).
/// The victim is an indestructible Darksteel Colossus (CR 702.12) so the
/// flag/counter assertions below stay rules-correct under deathtouch (the
/// pre-existing SBA gap filed as #11019 is not pinned here).
#[test]
fn constrictor_only_auto_applies_with_remainder_inline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE);
    let mut victim = scenario.add_creature(P0, "Darksteel Colossus", 11, 11);
    victim
        .from_oracle_text_with_keywords(&["Trample", "Indestructible"], DARKSTEEL_COLOSSUS_ORACLE);
    let colossus = victim.id();
    let butcher = scenario
        .add_creature_from_oracle(P1, "Smoldering Butcher", 4, 2, WITHER_ORACLE)
        .with_keyword(Keyword::Deathtouch)
        .with_keyword(Keyword::Lifelink)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(colossus);
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(butcher, AttackTarget::Player(P0))],
        &[(colossus, butcher)],
    );
    advance_past_combat_damage(&mut runner);

    // No prompt may ever open on the single-candidate path: the auto-apply
    // runs inline, so combat must already rest at priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "single-candidate replacement auto-applies with no prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        counter_count(&runner, colossus, &CounterType::Minus1Minus1),
        5,
        "Constrictor alone must add one to the 4 wither damage"
    );
    let dealt = damage_dealt_events(outcome.events(), &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the inline remainder must emit exactly one DamageDealt"
    );
    // Never-paused excess pin: a deathtouch wither leg reports `actual - 1`
    // (4 - 1 = 3) — the resumed rows must recompute this identically.
    assert!(
        matches!(
            dealt[0],
            GameEvent::DamageDealt {
                amount: 4,
                excess: 3,
                ..
            }
        ),
        "the inline DamageDealt pins the pre-existing excess formula, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, butcher, &target, true),
        1,
        "CR 120.1 must record the leg exactly once"
    );
    assert_eq!(
        life_of(&runner, P1),
        24,
        "lifelink must gain exactly the 4 dealt"
    );
    assert!(
        runner.state().objects[&colossus].dealt_deathtouch_damage,
        "CR 702.2b must mark the leg's deathtouch source"
    );
    assert_eq!(
        runner.state().objects[&colossus].damage_marked,
        0,
        "CR 120.3d: wither damage is never marked"
    );
}

/// Give priority to `player` (bounded test pump seed).
fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// CR 120.3d + CR 616.1 non-combat, single target (V5–V7): Puncture Blast at
/// a Constrictor-controlled creature under Doubling Season pauses on the counter
/// order and resumes with `DamageDealt` + record — no remaining chain.
///
/// REVERT-FAILING ASSERTION: the record count (1 vs 0 at base).
#[test]
fn puncture_blast_single_target_pause_resumes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doubling = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE)
        .id();
    let constrictor = scenario
        .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE)
        .id();
    // Sized so the doubled wither counters never kill it (3 damage doubles to
    // at most 8 — see the order math below).
    let victim = scenario.add_creature(P0, "Victim", 2, 10).id();
    let blast = scenario
        .add_spell_to_hand_from_oracle(P1, "Puncture Blast", true, PUNCTURE_BLAST_ORACLE)
        .wither()
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{R}"))
        .id();
    scenario.with_mana_pool(P1, mana(&[(ManaType::Red, 1), (ManaType::Colorless, 2)]));

    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    let target = TargetRef::Object(victim);
    runner.cast(blast).target_object(victim).resolve();

    // V5 reach-guard: the wither AddCounter event really meets two
    // replacements on the non-combat path.
    let resume_events = answer_counter_order(&mut runner, P0, doubling, constrictor);
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the resume"
    );
    // Order math (3 wither damage, Doubling-first): 3x2+1 = 7 counters.
    assert_eq!(
        counter_count(&runner, victim, &CounterType::Minus1Minus1),
        7,
        "Doubling-first must double the 3 wither damage and add one"
    );
    let dealt = damage_dealt_events(&resume_events, &target, false);
    assert_eq!(
        dealt.len(),
        1,
        "the resume must emit exactly one DamageDealt for the paused leg"
    );
    assert!(
        matches!(dealt[0], GameEvent::DamageDealt { amount: 3, .. }),
        "the resumed DamageDealt carries the 3 dealt, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, blast, &target, false),
        1,
        "CR 120.1 must record the paused leg exactly once"
    );
    // No deathtouch on this vehicle: the negative pairs with the
    // reach-guard record above (the leg provably ran).
    assert!(
        !runner.state().objects[&victim].dealt_deathtouch_damage,
        "no deathtouch source, no flag — paired with the record guard proving the leg ran"
    );
    assert_eq!(
        runner.state().objects[&victim].damage_marked,
        0,
        "CR 120.3d: wither damage is never marked"
    );
    // No survival assertion: no deathtouch rides this leg, and the 7-counter
    // count above already proves the victim is still present.
    // No lifelink on this vehicle either: nobody's life moves.
    assert_eq!(life_of(&runner, P0), 20, "P0's life must not move");
    assert_eq!(life_of(&runner, P1), 20, "P1's life must not move");
}

/// CR 120.3d + CR 616.1 non-combat, multi target (V5–V7): Village Pillagers'
/// ETB deals 1 to each creature P0 controls — Colossi A and B *and* the
/// Constrictor itself. Each leg pauses and the remaining chain resumes behind
/// it — one prompt per victim, remainder exactly once per leg.
///
/// The Constrictor (2/3) cannot survive its own 3 doubled counters, so it
/// dies at the post-batch SBA and Pillagers' Treasure trigger fires — the
/// pump below settles that too. Its flag/counters are NOT asserted (CR 400.7
/// clears both on zone exit); its `DamageDealt` + record prove its leg ran.
/// The Colossi carry deathtouch-marked legs, so they are indestructible
/// Darksteel Colossi (CR 702.12) rather than vanilla victims — their survival
/// is rules-correct, not a pin of the #11019 SBA gap.
///
/// REVERT-FAILING ASSERTIONS: three prompts answered (not a stall), one
/// record per victim (not zero), P1 at 23 (not 20).
#[test]
fn village_pillagers_etb_each_leg_pauses_and_resumes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doubling = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE)
        .id();
    let constrictor = scenario
        .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE)
        .id();
    let mut colossus_a = scenario.add_creature(P0, "Darksteel Colossus A", 11, 11);
    colossus_a
        .from_oracle_text_with_keywords(&["Trample", "Indestructible"], DARKSTEEL_COLOSSUS_ORACLE);
    let victim_a = colossus_a.id();
    let mut colossus_b = scenario.add_creature(P0, "Darksteel Colossus B", 11, 11);
    colossus_b
        .from_oracle_text_with_keywords(&["Trample", "Indestructible"], DARKSTEEL_COLOSSUS_ORACLE);
    let victim_b = colossus_b.id();
    let pillagers = scenario
        .add_creature_to_hand_from_oracle(P1, "Village Pillagers", 5, 5, VILLAGE_PILLAGERS_ORACLE)
        .wither()
        .with_keyword(Keyword::Deathtouch)
        .with_keyword(Keyword::Lifelink)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{R}{R}"))
        .id();
    scenario.with_mana_pool(P1, mana(&[(ManaType::Red, 2), (ManaType::Colorless, 3)]));

    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    runner.cast(pillagers).resolve();

    // Three victims ⇒ three counter-ordering prompts: the second and third
    // prove the remaining chain resumed behind each pause. (Mid-resolution SBA
    // does not run, so the Constrictor is alive for every prompt even though
    // its own counters are lethal.)
    let (answered, resume_events) =
        answer_all_counter_orders(&mut runner, P0, doubling, constrictor);
    assert_eq!(
        answered, 3,
        "each victim's wither leg must pause exactly once, got {answered}"
    );
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the ETB must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the resume"
    );
    for (name, victim) in [("A", victim_a), ("B", victim_b)] {
        let target = TargetRef::Object(victim);
        // Order math (1 wither damage, Doubling-first): 1x2+1 = 3 counters.
        assert_eq!(
            counter_count(&runner, victim, &CounterType::Minus1Minus1),
            3,
            "Doubling-first must double victim {name}'s 1 wither damage and add one"
        );
        assert_eq!(
            damage_dealt_events(&resume_events, &target, false).len(),
            1,
            "victim {name}'s paused leg must emit exactly one DamageDealt"
        );
        assert_eq!(
            record_count(&runner, pillagers, &target, false),
            1,
            "CR 120.1 must record victim {name}'s leg exactly once"
        );
        assert!(
            runner.state().objects[&victim].dealt_deathtouch_damage,
            "CR 702.2b must mark victim {name}'s resumed leg"
        );
        assert_eq!(
            runner.state().objects[&victim].damage_marked,
            0,
            "CR 120.3d: wither damage is never marked ({name})"
        );
        assert_eq!(
            runner.state().objects[&victim].zone,
            Zone::Battlefield,
            "indestructible CR 702.12 must keep victim {name} on the battlefield"
        );
    }
    // The Constrictor's own leg ran (event + record), then its 3 counters
    // killed its 3 toughness at the post-batch SBA — flag and counters are
    // CR 400.7-cleared on exit, so only the zone, event, and record pin it.
    let constrictor_target = TargetRef::Object(constrictor);
    assert_eq!(
        runner.state().objects[&constrictor].zone,
        Zone::Graveyard,
        "the Constrictor must die to its own 3 doubled counters"
    );
    assert_eq!(
        damage_dealt_events(&resume_events, &constrictor_target, false).len(),
        1,
        "the Constrictor's paused leg must emit exactly one DamageDealt"
    );
    assert_eq!(
        record_count(&runner, pillagers, &constrictor_target, false),
        1,
        "CR 120.1 must record the Constrictor's leg exactly once"
    );
    // One lifelink per leg: 20 + 1 + 1 + 1. The Treasure trigger fires but
    // moves no life.
    assert_eq!(
        life_of(&runner, P1),
        23,
        "lifelink must gain exactly once per resumed leg (20 + 1 + 1 + 1)"
    );
}

/// V13 serialized compat: an old save without `pending_damage_post_counter`
/// deserializes (`#[serde(default)]` → `None`), and a mid-pause save
/// round-trips and still resumes.
#[test]
fn mid_pause_save_round_trips_and_resumes() {
    use engine::types::game_state::GameState;

    let mut scenario = GameScenario::new();
    let (doubling, constrictor, colossus, butcher) =
        combat_board(&mut scenario, "Smoldering Butcher", 4, 2);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(colossus);
    run_combat_with_blocks(
        &mut runner,
        &[(butcher, AttackTarget::Player(P0))],
        &[(colossus, butcher)],
    );
    assert!(
        prompt(&runner).is_some(),
        "reach-guard: the pause must be open before snapshotting, got {:?}",
        runner.state().waiting_for
    );

    let json = serde_json::to_string(runner.state()).expect("mid-pause state serializes");
    // Old-save compat: a save written before the slot existed defaults it.
    let mut legacy_value: serde_json::Value = serde_json::from_str(&json).expect("save is JSON");
    legacy_value
        .as_object_mut()
        .expect("save is an object")
        .remove("pending_damage_post_counter");
    let legacy: GameState =
        serde_json::from_value(legacy_value).expect("old save must deserialize");
    assert!(
        legacy.pending_damage_post_counter.is_none(),
        "the new slot must default to None on old saves"
    );
    // Mid-pause round-trip: the combat-flagged stash survives serde.
    let revived: GameState = serde_json::from_str(&json).expect("mid-pause save must round-trip");
    assert!(
        revived
            .pending_damage_post_counter
            .as_ref()
            .is_some_and(|stashed| stashed.is_combat),
        "the combat-flagged counter stash must survive the round-trip"
    );

    let mut runner = GameRunner::from_state(revived);
    let resume_events = answer_counter_order(&mut runner, P0, doubling, constrictor);
    advance_past_combat_damage(&mut runner);

    assert_eq!(
        counter_count(&runner, colossus, &CounterType::Minus1Minus1),
        9,
        "Doubling-first must double the 4 wither damage and add one after the round-trip"
    );
    assert_eq!(
        damage_dealt_events(&resume_events, &target, true).len(),
        1,
        "the resumed leg must emit exactly one DamageDealt after the round-trip"
    );
    assert_eq!(
        record_count(&runner, butcher, &target, true),
        1,
        "CR 120.1 must record the paused leg exactly once after the round-trip"
    );
    assert_eq!(
        life_of(&runner, P1),
        24,
        "lifelink must gain exactly the 4 dealt after the round-trip"
    );
}

/// Asserts the real card's face text is `oracle`, so the Gideon/Stomp rows run
/// the printed cards rather than excerpts (grub-file idiom).
fn assert_oracle(db: &CardDatabase, name: &str, oracle: &str) {
    assert_eq!(
        db.get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the fixture"))
            .oracle_text
            .as_deref(),
        Some(oracle)
    );
}

/// M2-F1 Gideon witness board: P0 holds Gideon of the Trials; P1 holds Sickle
/// Ripper (2/1 wither) and a Bonecrusher Giant whose Stomp face turns Gideon's
/// prevention shield off for the turn (CR 615.12). With `pause_mods`, P0 also
/// holds Doubling Season + Winding Constrictor so the wither leg pauses on a
/// CR 616.1 choice. Returns Gideon, Ripper, the Giant, and the mod pair.
fn gideon_board(
    scenario: &mut GameScenario,
    db: &'static CardDatabase,
    pause_mods: bool,
) -> (ObjectId, ObjectId, ObjectId, Option<(ObjectId, ObjectId)>) {
    scenario.at_phase(Phase::PreCombatMain);
    let gideon = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Gideon of the Trials",
            "Gideon",
            3,
            GIDEON_OF_THE_TRIALS_ORACLE,
        )
        .id();
    let ripper = scenario
        .add_creature_from_oracle(P1, "Sickle Ripper", 2, 1, SICKLE_RIPPER_ORACLE)
        .wither()
        .id();
    let giant = scenario.add_real_card(P1, BONECRUSHER, Zone::Hand, db);
    scenario.with_mana_pool(P1, mana(&[(ManaType::Red, 1), (ManaType::Colorless, 1)]));
    let mods = pause_mods.then(|| {
        let doubling = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE)
            .id();
        let constrictor = scenario
            .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE)
            .id();
        (doubling, constrictor)
    });
    (gideon, ripper, giant, mods)
}

/// Activate Gideon's middle [0] (ability index 1) and reach-guard the
/// animation: a 4/4 Human Soldier creature that's still a planeswalker
/// (CR 205.1b) at three loyalty.
fn animate_gideon(runner: &mut GameRunner, gideon: ObjectId) {
    runner.activate(gideon, 1).resolve();
    let obj = &runner.state().objects[&gideon];
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "the [0] must animate Gideon into a creature, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        obj.card_types.core_types.contains(&CoreType::Planeswalker),
        "CR 205.1b: Gideon must remain a planeswalker, got {:?}",
        obj.card_types.core_types
    );
    assert_eq!(obj.power, Some(4), "animated Gideon must be 4/4");
    assert_eq!(obj.toughness, Some(4), "animated Gideon must be 4/4");
    assert_eq!(
        obj.loyalty,
        Some(3),
        "the [0] costs no loyalty: Gideon must still sit at three"
    );
}

/// P1 casts Stomp (adventure face, CR 715.3a) at P0: the 2 damage is the
/// reach-guard that the cast resolved, and CR 615.12 turns Gideon's
/// prevention shield off so the wither leg below actually applies.
fn cast_stomp_off(runner: &mut GameRunner, giant: ObjectId) {
    give_priority(runner, P1);
    runner
        .cast(giant)
        .adventure_face(false)
        .target_player(P0)
        .resolve();
    assert_eq!(
        life_of(runner, P0),
        18,
        "Stomp must resolve for 2 to P0, switching prevention off (CR 615.12)"
    );
}

/// CR 120.10 + CR 120.3c/d + CR 120.1, inline: Sickle Ripper's 2 wither combat
/// damage to the animated Gideon removes two loyalty (3 -> 1) and reports
/// zero excess — the pre-damage loyalty (3) exceeds the 2 dealt.
///
/// REVERT-FAILING ASSERTION: the `excess: 0` pin (pre-fix the shared core
/// rereads the already-reduced loyalty 1 and reports `excess: 1`).
#[test]
fn gideon_wither_inline_records_no_excess() {
    let db = load_db().expect("card db");
    assert_oracle(db, BONECRUSHER, BONECRUSHER_ORACLE);
    assert_oracle(db, STOMP, STOMP_ORACLE);
    let mut scenario = GameScenario::new();
    let (gideon, ripper, giant, mods) = gideon_board(&mut scenario, db, false);
    assert!(mods.is_none());
    let mut runner = scenario.build();

    animate_gideon(&mut runner, gideon);
    cast_stomp_off(&mut runner, giant);

    runner.state_mut().active_player = P0;
    let target = TargetRef::Object(gideon);
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(gideon, AttackTarget::Player(P1))],
        &[(ripper, gideon)],
    );
    advance_past_combat_damage(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the batch must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the resume"
    );
    // CR 120.3d: 2 wither damage puts 2 -1/-1 counters on the 4/4 Gideon.
    assert_eq!(
        counter_count(&runner, gideon, &CounterType::Minus1Minus1),
        2,
        "the 2 wither damage must land as 2 counters"
    );
    let dealt = damage_dealt_events(outcome.events(), &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the wither leg must emit exactly one DamageDealt"
    );
    assert!(
        matches!(
            dealt[0],
            GameEvent::DamageDealt {
                amount: 2,
                excess: 0,
                ..
            }
        ),
        "CR 120.10: 2 damage against pre-damage loyalty 3 reports no excess, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, ripper, &target, true),
        1,
        "CR 120.1 must record the wither leg exactly once"
    );
    // CR 120.3c: planeswalker damage removes loyalty (3 -> 1).
    assert_eq!(
        runner.state().objects[&gideon].loyalty,
        Some(1),
        "the 2 damage must leave Gideon at one loyalty"
    );
    assert_eq!(
        runner.state().objects[&gideon].zone,
        Zone::Battlefield,
        "Gideon must survive (4/4 with 2 counters, loyalty 1)"
    );
}

/// CR 120.10 + CR 616.1, paused: the same wither leg under Doubling Season +
/// Winding Constrictor pauses on the counter-ordering choice; the resume must
/// still report `excess: 0` — proving the parked stash carried the pre-damage
/// loyalty (3), not the live post-damage value (1).
///
/// The 5 doubled counters put the 4/4 Gideon at toughness -1, so CR 704.5f
/// destroys it at the post-batch SBA; the death is deterministic
/// counter-lethality and is asserted as destruction. Flag/counter liveness
/// assertions are impossible post-exit (CR 400.7 clears them), so the event +
/// record pin the leg.
///
/// REVERT-FAILING ASSERTION: the `excess: 0` pin on the resumed event.
#[test]
fn gideon_wither_pause_resumes_with_predamage_loyalty() {
    let db = load_db().expect("card db");
    assert_oracle(db, BONECRUSHER, BONECRUSHER_ORACLE);
    assert_oracle(db, STOMP, STOMP_ORACLE);
    let mut scenario = GameScenario::new();
    let (gideon, ripper, giant, mods) = gideon_board(&mut scenario, db, true);
    let (doubling, constrictor) = mods.expect("the paused leg needs the counter-replacement pair");
    let mut runner = scenario.build();

    animate_gideon(&mut runner, gideon);
    cast_stomp_off(&mut runner, giant);

    runner.state_mut().active_player = P0;
    let target = TargetRef::Object(gideon);
    run_combat_with_blocks(
        &mut runner,
        &[(gideon, AttackTarget::Player(P1))],
        &[(ripper, gideon)],
    );

    // V1 reach-guard: the wither AddCounter event really meets two
    // replacements (chooser P0 per CR 616.1).
    let resume_events = answer_counter_order(&mut runner, P0, doubling, constrictor);
    advance_past_combat_damage(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the batch must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the resume"
    );
    let dealt = damage_dealt_events(&resume_events, &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the resume must emit exactly one DamageDealt for the paused leg"
    );
    assert!(
        matches!(
            dealt[0],
            GameEvent::DamageDealt {
                amount: 2,
                excess: 0,
                ..
            }
        ),
        "CR 120.10: the resumed 2 damage still measures against pre-damage loyalty 3, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, ripper, &target, true),
        1,
        "CR 120.1 must record the paused leg exactly once"
    );
    // Doubling-first order lands 2x2+1 = 5 counters on the 4/4 Gideon:
    // CR 704.5f destroys it at the post-batch SBA — asserted as destruction.
    assert_eq!(
        runner.state().objects[&gideon].zone,
        Zone::Graveyard,
        "the 5 doubled counters must destroy the 4/4 Gideon (CR 704.5f)"
    );
}
