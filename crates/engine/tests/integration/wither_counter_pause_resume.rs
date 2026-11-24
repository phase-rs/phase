//! F1 wither/infect-to-creature counter-pause resume (PR #10016 follow-up).
//!
//! CR 120.3d + CR 702.80a + CR 616.1: a wither/infect-to-creature leg whose
//! `-1/-1` counter placement meets two co-applicable counter *modifiers*
//! parks a `NeedsChoice` pause. Pre-fix the leg is dropped: no deathtouch
//! flag, no `DamageDealt`, no CR 120.1 record, no lifelink — the resume below
//! preserves the leg across the pause, exactly once, on the non-combat path.
//!
//! CR 614.16 (M3+M4): combat wither is dealt by the CR 510.2 turn-based
//! action, not by a resolving effect, so round 1 admits only the bare-worded
//! Constrictor — whose modification then promotes the placement (M4),
//! admitting the effect-worded Doubling Season in round 2. Combat rows
//! converge on the sequential Constrictor-then-DS outcome with no prompt. The
//! two-modifier CR 616.1 pause coverage lives on the non-combat rows (Puncture
//! Blast, Village Pillagers ETB), where the resolving effect keeps both
//! modifiers applicable from round 1.
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
//! - combat Constrictor-then-Doubling-Season under Doubling Season +
//!   Constrictor (V9): no prompt, `DamageDealt` + record exactly once,
//!   lifelink exactly once, deathtouch flag exactly once with no marking; the
//!   victim is an indestructible Darksteel Colossus (CR 702.12) so its
//!   survival under a deathtouch source is rules-correct (the #11019 SBA gap
//!   is not pinned).
//! - V1 hostile: Ripper + Doubling Season + Constrictor combat lands 6,
//!   proving sequential eligibility (Constrictor +1 first, promoting the
//!   placement so Doubling Season doubles second — simultaneous eligibility
//!   would prompt since Plus-vs-Times is order-material).
//! - V0 Necroskitter second vehicle: wither-keyworded, exercising the same
//!   `has_wither || has_infect` arm, now Constrictor-then-DS in combat.
//! - V12 Constrictor-only single candidate: no prompt, remainder inline.
//! - non-combat single target (Puncture Blast) + multi target (Village
//!   Pillagers ETB, remaining chain resumes after the pause).
//! - V13 mid-pause save round-trip + old-save default compat (non-combat
//!   vehicle — combat no longer pauses).

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
/// indestructible Darksteel Colossus (11/11, so the modified wither counters
/// never kill it by toughness either — Butcher's 4 damage lands
/// Constrictor-then-DS 10; see the order math in each row); P1 holds a wither
/// attacker granted
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

/// CR 120.3d + CR 614.16 + CR 120.1 + CR 702.15b + CR 702.2b, combat
/// (V9): the combat wither leg stamps `Other` (CR 510.2 turn-based action, not
/// a resolving effect), so round 1 has a single candidate — Winding
/// Constrictor auto-applies with no CR 616.1 prompt — and the modified
/// placement then admits Doubling Season in round 2 (CR 614.16 second clause),
/// again unopposed. The leg runs inline with its deathtouch flag,
/// `DamageDealt` event, CR 120.1 record, and lifelink, exactly once.
///
/// Order math (4 wither damage): Constrictor 4+1 = 5, then Doubling Season
/// 5*2 = 10 counters. The 11/11 Colossus survives (11-10=1), and CR 702.12
/// indestructible makes that survival rules-correct under deathtouch (the
/// pre-existing SBA gap filed as #11019 is not pinned here).
///
/// REVERT-FAILING ASSERTIONS: at base the Constrictor's modification keeps the
/// `Other` cause, so Doubling Season stays inapplicable and the count is 5
/// (not 10). (Pre-tag the leg paused on a two-candidate CR 616.1 prompt with
/// 9 or 10; the corrected 10 equals one pre-tag order — the no-prompt guard is
/// what discriminates determinism now.) The two-modifier pause coverage lives
/// on the non-combat rows below.
#[test]
fn combat_wither_with_inapplicable_doubling_applies_constrictor_only() {
    let mut scenario = GameScenario::new();
    let (_doubling, _constrictor, colossus, butcher) =
        combat_board(&mut scenario, "Smoldering Butcher", 4, 2);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(colossus);
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(butcher, AttackTarget::Player(P0))],
        &[(colossus, butcher)],
    );

    // V9: a single applicable replacement per round auto-applies — no CR 616.1
    // prompt may ever open on this path.
    assert!(
        prompt(&runner).is_none(),
        "sequential single candidates must auto-apply with no ordering prompt, got {:?}",
        runner.state().waiting_for
    );
    advance_past_combat_damage(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the batch must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after combat"
    );
    // V9: Constrictor 4+1 = 5, then Doubling Season doubles 5 -> 10 — the
    // modified placement admits the effect-worded doubler (CR 614.16).
    assert_eq!(
        counter_count(&runner, colossus, &CounterType::Minus1Minus1),
        10,
        "Constrictor-then-Doubling-Season must land 10 counters on the 4 wither damage"
    );
    // The inline leg emits DamageDealt + the CR 120.1 record exactly once.
    let dealt = damage_dealt_events(outcome.events(), &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the inline leg must emit exactly one DamageDealt"
    );
    // Excess parity with the never-paused path (pinned in V12): the shared
    // excess formula reads unmarked damage, so a deathtouch wither leg
    // reports `actual - 1` (4 - 1 = 3) even with `damage_marked == 0`.
    // Pre-existing semantic, preserved verbatim by the core.
    assert!(
        matches!(
            dealt[0],
            GameEvent::DamageDealt {
                amount: 4,
                excess: 3,
                ..
            }
        ),
        "the inline DamageDealt carries the full dealt amount with formula-parity excess, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, butcher, &target, true),
        1,
        "CR 120.1 must record the inline leg exactly once"
    );
    // Lifelink exactly once — the inline leg's full share.
    assert_eq!(
        life_of(&runner, P1),
        24,
        "lifelink must gain exactly the 4 dealt (20 + 4)"
    );
    // Deathtouch flag exactly once, no marking. Survival is asserted
    // because CR 702.12 indestructible makes it rules-correct (not because
    // the engine destroys deathtouch victims — that gap is #11019).
    assert!(
        !runner.state().objects[&colossus].dealt_deathtouch_damage,
        "CR 704.5h: the check after combat damage consumed the inline leg's deathtouch record"
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

/// CR 614.16 multi-authority hostile (V1): Sickle Ripper's 2 wither combat
/// damage to a Darksteel Colossus under Doubling Season + Winding Constrictor
/// lands exactly 6 counters. Round 1: the combat leg stamps `Other` (CR 510.2
/// turn-based action), so only the bare-worded Constrictor applies (2+1 = 3)
/// and promotes the placement's cause; round 2: the effect-worded Doubling
/// Season becomes eligible on the replacement-made placement (CR 614.16
/// second clause) and doubles 3 -> 6.
///
/// DS-first is impossible, not a choice: Plus-vs-Times is order-material
/// (Additive vs Multiplicative commute classes), so had Doubling Season been
/// eligible in round 1 alongside the Constrictor, a CR 616.1 prompt would have
/// opened. The no-prompt guard + count 6 together prove the Constrictor
/// applied alone first and Doubling Season became eligible only after the
/// promotion — this row is the sequential-eligibility proof the R1 activation
/// row cites (Times x Times commute, so R1's own count + no-prompt cannot
/// prove it).
///
/// REVERT-FAILING: at base the Constrictor's modification keeps the `Other`
/// cause, so Doubling Season stays inapplicable and the count is 3 (not 6).
#[test]
fn combat_wither_doubling_plus_constrictor_lands_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    scenario.add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE);
    let mut victim = scenario.add_creature(P0, "Darksteel Colossus", 11, 11);
    victim
        .from_oracle_text_with_keywords(&["Trample", "Indestructible"], DARKSTEEL_COLOSSUS_ORACLE);
    let colossus = victim.id();
    let ripper = scenario
        .add_creature_from_oracle(P1, "Sickle Ripper", 2, 1, SICKLE_RIPPER_ORACLE)
        .wither()
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(colossus);
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(ripper, AttackTarget::Player(P0))],
        &[(colossus, ripper)],
    );

    assert!(
        prompt(&runner).is_none(),
        "a single applicable replacement auto-applies with no prompt, got {:?}",
        runner.state().waiting_for
    );
    advance_past_combat_damage(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the batch must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after combat"
    );
    assert_eq!(
        counter_count(&runner, colossus, &CounterType::Minus1Minus1),
        6,
        "CR 614.16: the Constrictor 2->3 promotes the placement, then Doubling Season doubles 3->6"
    );
    let dealt = damage_dealt_events(outcome.events(), &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "reach-guard: the combat leg must emit exactly one DamageDealt"
    );
    assert!(
        matches!(dealt[0], GameEvent::DamageDealt { amount: 2, .. }),
        "reach-guard: the DamageDealt carries the 2 dealt, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, ripper, &target, true),
        1,
        "reach-guard: CR 120.1 must record the combat leg exactly once"
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

/// V0 second-vehicle arm-identity row (N1): Necroskitter is wither-keyworded
/// (not Butcher), exercising the same `has_wither || has_infect` arm. Asserts
/// the inline `DamageDealt` + record; lifelink/flag siblings live in the
/// Constrictor row above. The combat leg stamps `Other`, so round 1 applies
/// the Constrictor alone (1+1 = 2, promoting the placement) and round 2
/// applies Doubling Season (2*2 = 4) — sequential single candidates, no prompt.
///
/// REVERT-FAILING ASSERTIONS: at base the Constrictor's modification keeps the
/// `Other` cause, so Doubling Season stays inapplicable and the count is 2
/// (not 4).
#[test]
fn necroskitter_second_vehicle_arm_identity() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    scenario.add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE);
    let shieldmate = scenario.add_creature(P0, "Shieldmate", 2, 8).id();
    // No deathtouch/lifelink grant: the arm identity is the point, not the
    // riders (Necroskitter's dies trigger is out of scope — nothing dies).
    let skitter = scenario
        .add_creature_from_oracle(P1, "Necroskitter", 1, 4, WITHER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(shieldmate);
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(skitter, AttackTarget::Player(P0))],
        &[(shieldmate, skitter)],
    );

    assert!(
        prompt(&runner).is_none(),
        "a single applicable replacement auto-applies with no prompt, got {:?}",
        runner.state().waiting_for
    );
    advance_past_combat_damage(&mut runner);

    // Order math (1 wither damage): Constrictor 1+1 = 2, then Doubling
    // Season 2*2 = 4 counters.
    assert_eq!(
        counter_count(&runner, shieldmate, &CounterType::Minus1Minus1),
        4,
        "Constrictor-then-Doubling-Season must land 4 counters on the 1 wither damage"
    );
    let dealt = damage_dealt_events(outcome.events(), &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the inline leg must emit exactly one DamageDealt"
    );
    assert!(
        matches!(dealt[0], GameEvent::DamageDealt { amount: 1, .. }),
        "the inline DamageDealt carries the 1 dealt, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, skitter, &target, true),
        1,
        "CR 120.1 must record the inline leg exactly once"
    );
    // No survival assertion: no deathtouch rides this leg, and the 4-counter
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
        !runner.state().objects[&colossus].dealt_deathtouch_damage,
        "CR 704.5h: the check after combat damage consumed the leg's deathtouch record"
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
            !runner.state().objects[&victim].dealt_deathtouch_damage,
            "CR 704.5h: the check after the resume consumed victim {name}'s deathtouch record"
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
///
/// CR 614.16 (V9): combat never pauses (sequential single candidates auto-apply),
/// so the mid-pause vehicle is the non-combat Puncture Blast two-modifier
/// pause — the same `PendingDamagePostCounter` stash with `is_combat` false,
/// drained through `drain_pending_damage_post_counter` on resume.
#[test]
fn mid_pause_save_round_trips_and_resumes() {
    use engine::types::game_state::GameState;

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
    // The cast driver stops at the CR 616.1 counter-ordering prompt, leaving
    // the pause open for the snapshot below.
    runner.cast(blast).target_object(victim).resolve();
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
    // Mid-pause round-trip: the non-combat counter stash survives serde.
    let revived: GameState = serde_json::from_str(&json).expect("mid-pause save must round-trip");
    assert!(
        revived
            .pending_damage_post_counter
            .as_ref()
            .is_some_and(|stashed| !stashed.is_combat),
        "the non-combat counter stash must survive the round-trip"
    );

    let mut runner = GameRunner::from_state(revived);
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
        "Doubling-first must double the 3 wither damage and add one after the round-trip"
    );
    let dealt = damage_dealt_events(&resume_events, &target, false);
    assert_eq!(
        dealt.len(),
        1,
        "the resumed leg must emit exactly one DamageDealt after the round-trip"
    );
    assert!(
        matches!(dealt[0], GameEvent::DamageDealt { amount: 3, .. }),
        "the resumed DamageDealt carries the 3 dealt, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, blast, &target, false),
        1,
        "CR 120.1 must record the paused leg exactly once after the round-trip"
    );
    // No lifelink on this vehicle: nobody's life moves.
    assert_eq!(life_of(&runner, P0), 20, "P0's life must not move");
    assert_eq!(life_of(&runner, P1), 20, "P1's life must not move");
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
/// holds Doubling Season + Winding Constrictor so the wither leg runs the
/// Constrictor-then-DS sequence inline (no CR 616.1 prompt — sequential single
/// candidates). Returns Gideon, Ripper, the Giant, and the mod pair.
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

/// CR 120.10 + CR 614.16 (V9): the same wither leg under Doubling Season +
/// Winding Constrictor runs inline with no prompt — round 1 applies the
/// Constrictor alone (2+1 = 3, promoting the placement), round 2 applies
/// Doubling Season (3*2 = 6). The event must still report `excess: 0`,
/// proving the combat path measures against the pre-damage loyalty (3), not
/// the live post-damage value (1).
///
/// The 6 counters leave the 4/4 Gideon at toughness -2, so Gideon dies to SBA
/// 704.5f — asserted as Graveyard (contrast the base survival at 3 counters).
/// The 6 is asserted on the `CounterAdded` event stream, never on the
/// graveyard object: counters leave with the zone change (CR 122.2), so
/// post-combat `counter_count` reads 0.
///
/// Loyalty is intentionally unasserted post-combat: the battlefield-exit
/// cleanup reverts `loyalty` to `base_loyalty` (CR 613.1 + CR 400.7,
/// `revert_layered_characteristics_to_base` via `zones.rs`), and the scenario
/// builder seeds `loyalty` without `base_loyalty`, so the graveyard row reads
/// `None` — an off-battlefield fixture artifact, not a rules value (CR 400.7:
/// the graveyard card is a new object).
///
/// REVERT-FAILING ASSERTIONS: at base the Constrictor's modification keeps the
/// `Other` cause, so the count is 3 (not 6) and Gideon survives (not dies).
#[test]
fn gideon_wither_constrictor_only_reads_predamage_loyalty() {
    let db = load_db().expect("card db");
    assert_oracle(db, BONECRUSHER, BONECRUSHER_ORACLE);
    assert_oracle(db, STOMP, STOMP_ORACLE);
    let mut scenario = GameScenario::new();
    let (gideon, ripper, giant, mods) = gideon_board(&mut scenario, db, true);
    assert!(
        mods.is_some(),
        "the leg needs the counter-replacement pair on the board"
    );
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

    assert!(
        prompt(&runner).is_none(),
        "a single applicable replacement auto-applies with no prompt, got {:?}",
        runner.state().waiting_for
    );
    advance_past_combat_damage(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the batch must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after combat"
    );
    // CR 120.3d + CR 614.16: Constrictor 2+1 = 3, then Doubling Season
    // 3*2 = 6 — asserted on the `CounterAdded` event stream, which carries
    // the post-replacement count. This combat has a single wither leg
    // (gideon->ripper is non-wither damage), so exactly one such event fires.
    let added: Vec<&GameEvent> = outcome
        .events()
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::CounterAdded {
                    object_id,
                    counter_type: CounterType::Minus1Minus1,
                    ..
                } if *object_id == gideon
            )
        })
        .collect();
    assert_eq!(
        added.len(),
        1,
        "the wither leg must emit exactly one CounterAdded for Gideon"
    );
    assert!(
        matches!(added[0], GameEvent::CounterAdded { count: 6, .. }),
        "CR 614.16: Constrictor 2->3 then Doubling Season 3->6 must place 6 counters, got {:?}",
        added[0]
    );
    // CR 122.2 trap-guard: counters leave with the zone change, so the dead
    // Gideon's object carries none post-combat.
    assert_eq!(
        counter_count(&runner, gideon, &CounterType::Minus1Minus1),
        0,
        "CR 122.2: Gideon's counters must not survive the battlefield->graveyard move"
    );
    let dealt = damage_dealt_events(outcome.events(), &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "the inline leg must emit exactly one DamageDealt"
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
        "CR 120.10: the inline 2 damage still measures against pre-damage loyalty 3, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, ripper, &target, true),
        1,
        "CR 120.1 must record the inline leg exactly once"
    );
    // Loyalty intentionally unasserted (see the doc comment): the exit
    // cleanup reverts it to the scenario-unset `base_loyalty` (`None` here).
    assert_eq!(
        runner.state().objects[&gideon].zone,
        Zone::Graveyard,
        "SBA 704.5f: the 4/4 Gideon with 6 counters (toughness -2) must die"
    );
}

/// CR 614.16 + CR 120.3d + CR 510.2 (V1): Sickle Ripper's 2 wither combat
/// damage to a Darksteel Colossus under Doubling Season puts exactly 2
/// -1/-1 counters — the combat leg is a turn-based action (CR 510.2), not
/// "the effect of a resolving spell or ability", so the effect-worded
/// doubler must not apply. No CR 616.1 prompt opens either way (single
/// candidate at base, zero candidates after the fix).
///
/// REVERT-FAILING (RED at base): the count is 4 (doubled), not 2.
/// The DamageDealt + CR 120.1 record reach-guards prove the leg ran.
#[test]
fn combat_wither_under_doubling_season_is_not_doubled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    let mut victim = scenario.add_creature(P0, "Darksteel Colossus", 11, 11);
    victim
        .from_oracle_text_with_keywords(&["Trample", "Indestructible"], DARKSTEEL_COLOSSUS_ORACLE);
    let colossus = victim.id();
    let ripper = scenario
        .add_creature_from_oracle(P1, "Sickle Ripper", 2, 1, SICKLE_RIPPER_ORACLE)
        .wither()
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let target = TargetRef::Object(colossus);
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(ripper, AttackTarget::Player(P0))],
        &[(colossus, ripper)],
    );
    advance_past_combat_damage(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "combat must settle to priority with no prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after combat"
    );
    assert_eq!(
        counter_count(&runner, colossus, &CounterType::Minus1Minus1),
        2,
        "CR 614.16: combat wither is not a resolving effect, so Doubling Season must not double the 2 damage"
    );
    let dealt = damage_dealt_events(outcome.events(), &target, true);
    assert_eq!(
        dealt.len(),
        1,
        "reach-guard: the combat leg must emit exactly one DamageDealt"
    );
    assert!(
        matches!(dealt[0], GameEvent::DamageDealt { amount: 2, .. }),
        "reach-guard: the DamageDealt carries the 2 dealt, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, ripper, &target, true),
        1,
        "reach-guard: CR 120.1 must record the combat leg exactly once"
    );
    assert_eq!(
        runner.state().objects[&colossus].damage_marked,
        0,
        "CR 120.3d: wither damage is never marked"
    );
}

/// CR 614.16 first clause + CR 120.3d (V2): Puncture Blast's 3 wither damage
/// is dealt by a resolving spell effect, so Doubling Season still doubles it
/// to 6 counters. Preservation pin: 6 both before and after the fix (the
/// single-candidate auto-apply path takes no prompt either way).
#[test]
fn effect_wither_under_doubling_season_still_doubles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    // Sized so the doubled wither counters never kill it (3 damage doubles
    // to 6 — no toughness pressure either way).
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
    let outcome = runner.cast(blast).target_object(victim).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the cast must settle to priority with no prompt, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the cast resolves"
    );
    assert_eq!(
        counter_count(&runner, victim, &CounterType::Minus1Minus1),
        6,
        "CR 614.16: resolving-spell wither is an eligible effect, so the 3 damage must double to 6"
    );
    // Reach-guard: the spell resolved (graveyard, no short-circuit) and the
    // leg provably ran (DamageDealt + CR 120.1 record).
    assert_eq!(
        outcome.zone_of(blast),
        Zone::Graveyard,
        "reach-guard: Puncture Blast must resolve to the graveyard"
    );
    let dealt = damage_dealt_events(outcome.events(), &target, false);
    assert_eq!(
        dealt.len(),
        1,
        "reach-guard: the leg must emit exactly one DamageDealt"
    );
    assert!(
        matches!(dealt[0], GameEvent::DamageDealt { amount: 3, .. }),
        "reach-guard: the DamageDealt carries the 3 dealt, got {:?}",
        dealt[0]
    );
    assert_eq!(
        record_count(&runner, blast, &target, false),
        1,
        "reach-guard: CR 120.1 must record the leg exactly once"
    );
    assert_eq!(
        runner.state().objects[&victim].damage_marked,
        0,
        "CR 120.3d: wither damage is never marked"
    );
}

/// Vorinclex, Monstrous Raider's VERBATIM Oracle text (card-data export; 6/6).
/// Actor-worded ("If you would put ...") with no "effect" restriction, so its
/// doubling is cause-blind by construction — the F1 gate must leave it alone.
const VORINCLEX_ORACLE: &str = "Trample, haste\nIf you would put one or more counters on a permanent or player, put twice that many of each of those kinds of counters on that permanent or player instead.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

/// CR 614.16 Selesnya-ruling row (V4): activating Gideon's [+1] under
/// Doubling Season adds exactly 1 loyalty — paying a loyalty cost is not "an
/// effect", so the effect-worded doubler must not apply. The Vorinclex
/// sibling pins the cause-blind actor wording (same activation → +2, both
/// before and after the fix).
///
/// REVERT-FAILING (RED at base): loyalty is 5 (doubled), not 4.
/// Reach-guard: the activation completed and loyalty moved up (no
/// short-circuit), so only the count discriminates.
#[test]
fn loyalty_cost_under_doubling_season_is_not_doubled() {
    for (label, doubler, expected) in [("Doubling Season", false, 4), ("Vorinclex", true, 5)] {
        let mut scenario = GameScenario::new();
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
        if doubler {
            scenario.add_creature_from_oracle(
                P0,
                "Vorinclex, Monstrous Raider",
                6,
                6,
                VORINCLEX_ORACLE,
            );
        } else {
            scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
        }

        let mut runner = scenario.build();
        give_priority(&mut runner, P0);
        runner.activate(gideon, 0).resolve();

        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{label}: the activation must settle to priority, got {:?}",
            runner.state().waiting_for
        );
        let loyalty = runner.state().objects[&gideon].loyalty;
        assert!(
            loyalty.is_some_and(|l| l > 3),
            "{label} reach-guard: the [+1] must resolve and raise loyalty, got {loyalty:?}"
        );
        assert_eq!(
            loyalty,
            Some(expected),
            "{label}: the [+1] loyalty cost must land exactly (4 under Doubling Season per CR 614.16, 5 under cause-blind Vorinclex), got {loyalty:?}"
        );
    }
}

/// CR 606.4 + CR 614.16 second clause (M4-F1 closes-when): activating Gideon's
/// [+1] under Doubling Season + Vorinclex, Monstrous Raider lands +4 loyalty
/// (3 -> 7). The 1-counter cost (CR 606.4, `Other` cause) is doubled to 2 by
/// the cause-blind Vorinclex ("you would put"), and that replacement-made
/// placement then satisfies Doubling Season's "an effect would put" via CR
/// 614.16's "they also apply if another replacement or prevention effect does
/// so", doubling 2 -> 4. Round trace: round 1 {Vorinclex} auto-applies (DS
/// gated out), round 2 {DS} auto-applies on the promoted placement — a
/// singleton each round, so no CR 616.1 prompt opens.
///
/// Caveat: both doublers parse "twice that many" to `Times{2}`, and
/// Times x Times commute, so simultaneous round-1 eligibility would also yield
/// 7 with no prompt — the count + no-prompt here do NOT prove sequential
/// eligibility. That proof lives in
/// `combat_wither_doubling_plus_constrictor_lands_three`, where Plus-vs-Times
/// is order-material (simultaneous eligibility would prompt).
///
/// REVERT-FAILING: at base the modified placement keeps its `Other` cause, so
/// Doubling Season stays inapplicable and loyalty reads `Some(5)`, not
/// `Some(7)`.
#[test]
fn loyalty_cost_under_doubling_season_and_vorinclex_compounds_to_seven() {
    let mut scenario = GameScenario::new();
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
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    scenario.add_creature_from_oracle(P0, "Vorinclex, Monstrous Raider", 6, 6, VORINCLEX_ORACLE);

    let mut runner = scenario.build();
    give_priority(&mut runner, P0);
    runner.activate(gideon, 0).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the activation"
    );
    let loyalty = runner.state().objects[&gideon].loyalty;
    assert!(
        loyalty.is_some_and(|l| l > 3),
        "reach-guard: the [+1] must resolve and raise loyalty, got {loyalty:?}"
    );
    assert!(
        prompt(&runner).is_none(),
        "singleton candidates each round must auto-apply with no CR 616.1 prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        loyalty,
        Some(7),
        "CR 614.16: Vorinclex 1->2 then Doubling Season 2->4 must land the [+1] at 7 loyalty, got {loyalty:?}"
    );
}

/// Elspeth Conquers Death's VERBATIM Oracle text (card-data export; Saga).
/// No Oracle clause places lore counters — lore arrives via the CR 714
/// turn-based action, cited instead of Oracle (em dashes as \u{2014}).
const ELSPETH_CONQUERS_DEATH_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI \u{2014} Exile target permanent an opponent controls with mana value 3 or greater.\nII \u{2014} Noncreature spells your opponents cast cost {2} more to cast until your next turn.\nIII \u{2014} Return target creature or planeswalker card from your graveyard to the battlefield. Put a +1/+1 counter or a loyalty counter on it.";

/// CR 714.3c + CR 614.16 (V5): advancing into the precombat main with a Saga
/// under Doubling Season places exactly 1 lore counter — the turn-based
/// action is not "an effect", so the effect-worded doubler must not apply.
/// The Vorinclex sibling pins the cause-blind actor wording (same advance →
/// 2 lore, both before and after the fix).
///
/// REVERT-FAILING (RED pre-tag): lore is 2 (doubled), not 1.
/// Reach-guard: the phase advanced and the chapter-I trigger queued and
/// resolved (its exile proves the Saga machinery ran), so only the count
/// discriminates. The ordering prompt itself is per-sibling: 1 lore resolves
/// chapter I directly, 2 lore (Vorinclex) orders chapters I and II.
#[test]
fn saga_lore_turn_based_action_is_not_doubled() {
    for (label, use_vorinclex, expected) in [("Doubling Season", false, 1), ("Vorinclex", true, 2)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Upkeep);
        // Stage the subtype BEFORE parsing: the parser's saga route
        // (`parse_saga_chapters`) only runs when the staged subtypes include
        // "Saga" (modal-saga idiom — `add_enchantment_from_oracle` parses
        // with no subtypes, which yields zero chapters).
        let saga = scenario
            .add_creature(P0, "Elspeth Conquers Death", 0, 0)
            .as_enchantment()
            .with_subtypes(vec!["Saga"])
            .from_oracle_text(ELSPETH_CONQUERS_DEATH_ORACLE)
            .id();
        if use_vorinclex {
            scenario.add_creature_from_oracle(
                P0,
                "Vorinclex, Monstrous Raider",
                6,
                6,
                VORINCLEX_ORACLE,
            );
        } else {
            scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
        }
        // Chapter-I exile target: MV 3 or greater, controlled by the opponent.
        let dummy = scenario
            .add_creature(P1, "Target Dummy", 2, 2)
            .with_mana_cost(parse_mtgjson_mana_cost("{3}"))
            .id();
        scenario.with_library_top(P0, &["Plains"; 10]);
        scenario.with_library_top(P1, &["Plains"; 10]);

        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.turn_number = 2;
            state.active_player = P0;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }
        runner.advance_to_phase(Phase::PreCombatMain);

        // Drain the chapter triggers the lore spins up: order (identity),
        // then priority passes. Chapter I's exile finds exactly one legal
        // target (the MV-3 dummy) and the engine applies it without a
        // target prompt — the queued order plus the exile itself prove
        // chapter I ran, so both paths are accepted below.
        let mut saw_order_prompt = false;
        for _ in 0..64 {
            match runner.state().waiting_for.clone() {
                WaitingFor::OrderTriggers { triggers, .. } => {
                    saw_order_prompt = true;
                    assert!(
                        triggers.iter().any(|t| t.description.contains("Chapter 1")),
                        "{label} reach-guard: the lore must queue chapter I, got {triggers:?}"
                    );
                    let order = (0..triggers.len()).collect();
                    runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("order triggers");
                }
                WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(dummy)),
                        })
                        .expect("exile the MV-3 dummy");
                }
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).expect("pass priority");
                }
                other => panic!("{label}: unexpected prompt while settling chapter I: {other:?}"),
            }
        }

        // The ordering prompt is per-sibling: 1 lore queues only chapter I,
        // which resolves directly with no prompt; 2 lore (Vorinclex) queues
        // chapters I and II, which P0 must order. Either way the exile below
        // proves chapter I ran.
        if use_vorinclex {
            assert!(
                saw_order_prompt,
                "{label} reach-guard: 2 lore must queue chapters I and II behind an ordering prompt"
            );
        } else {
            assert!(
                !saw_order_prompt,
                "{label}: a single chapter trigger resolves directly with no ordering prompt"
            );
        }
        assert_eq!(
            runner.state().objects[&dummy].zone,
            Zone::Exile,
            "{label} reach-guard: chapter I must exile the dummy"
        );
        assert_eq!(
            runner
                .state()
                .objects
                .get(&saga)
                .and_then(|o| o.counters.get(&CounterType::Lore).copied())
                .unwrap_or(0),
            expected,
            "{label}: the CR 714.3c turn-based action must place exactly 1 lore (2 under cause-blind Vorinclex), got {:?}",
            runner.state().objects.get(&saga).and_then(|o| o.counters.get(&CounterType::Lore).copied())
        );
    }
}

/// Hypnotic Grifter's VERBATIM Oracle text ({U} 1/2; verified against
/// MTGJSON: ability line plus reminder parenthetical, keyword `Connive`).
const HYPNOTIC_GRIFTER_ABILITY: &str =
    "{3}: This creature connives. (Draw a card, then discard a card. If you discarded a nonland card, put a +1/+1 counter on this creature.)";

/// CR 701.50a + CR 614.16 (V11): a resolving connive ability's +1/+1 counter
/// is placed by an effect, so Doubling Season still doubles it (2 counters).
/// Preservation pin: 2 both before and after the fix — a default-`Other`
/// slip in the connive direct construction would un-double it to 1 with no
/// other row catching it.
#[test]
fn connive_counter_under_doubling_season_still_doubles() {
    use engine::types::ability::Effect;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    scenario.with_library_top(P0, &["Nonland Top"]);
    // Empty hand plus the drawn card: the auto-discard path, no
    // ConniveDiscard choice.
    let grifter = scenario
        .add_creature_from_oracle(P0, "Hypnotic Grifter", 1, 2, HYPNOTIC_GRIFTER_ABILITY)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 3)]));
    let mut runner = scenario.build();
    let idx = runner.state().objects[&grifter]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::Connive { .. }))
        .expect("Hypnotic Grifter must carry a Connive activated ability");

    // P0 pays {3} from the pool staged above.
    give_priority(&mut runner, P0);
    runner.activate(grifter, idx).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the connive activation must settle to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after connive resolves"
    );
    // Reach-guard: the full connive process ran (drew the only library card,
    // discarded it from hand).
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "reach-guard: connive must draw the stocked card, library: {:?}",
        runner.state().players[P0.0 as usize].library.len()
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "reach-guard: connive must discard the drawn card"
    );
    assert_eq!(
        counter_count(&runner, grifter, &CounterType::Plus1Plus1),
        2,
        "CR 701.50a + CR 614.16: the resolving connive effect's counter must double to 2 under Doubling Season"
    );
}

/// Forgotten Ancient's VERBATIM Oracle text (0/3; verified against MTGJSON).
const FORGOTTEN_ANCIENT_ORACLE: &str = "Whenever a player casts a spell, you may put a +1/+1 counter on this creature.\nAt the beginning of your upkeep, you may move any number of +1/+1 counters from this creature onto other creatures.";

/// CR 122.5 + CR 614.16 (V12): moving 2 counters under Doubling Season lands
/// 4 on the destination — the move's Add stage inherits the initiating
/// cause (a resolving upkeep trigger ability), which stays eligible.
/// Preservation pin: 4 both before and after the fix — a dropped cause on
/// `PendingCounterMove` regresses this to 2 with no other row catching it.
/// Reach-guard: the source is emptied AND the destination holds counters
/// (a no-op move fails both).
#[test]
fn moved_counters_under_doubling_season_still_double() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    let ancient = scenario
        .add_creature_from_oracle(P0, "Forgotten Ancient", 0, 3, FORGOTTEN_ANCIENT_ORACLE)
        .with_plus_counters(2)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Pump the turn machinery once: beginning-of-upkeep triggers queue
    // lazily on the first pass after upkeep entry, never at scenario setup
    // (issue-680 idiom — without this the loop below breaks immediately).
    runner.auto_advance_to_main_phase();

    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 50, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept upkeep move");
            }
            WaitingFor::MoveCountersDistribution { .. } => {
                runner
                    .act(GameAction::ChooseCounterMoveDistribution {
                        selections: vec![engine::types::game_state::CounterMoveChoice {
                            destination_id: bear,
                            counter_type: CounterType::Plus1Plus1,
                            count: 2,
                        }],
                    })
                    .expect("move both counters to bear");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    assert_eq!(
        counter_count(&runner, ancient, &CounterType::Plus1Plus1),
        0,
        "reach-guard: the move must empty the Ancient"
    );
    assert_eq!(
        counter_count(&runner, bear, &CounterType::Plus1Plus1),
        4,
        "CR 122.5 + CR 614.16: the moved 2 counters must double to 4 on the destination"
    );
}
