//! S07 increment B — Malamet Battle Glyph cast-path.
//!
//! Full cast→resolve tests driving the real pipeline (GameScenario + GameRunner)
//! for the "choose two target creatures, conditionally counter one, then those
//! creatures fight each other" class: Malamet Battle Glyph, Longstalk Brawl, Duel
//! for Dominance. Each asserts measured deltas (counters + fight damage) that flip
//! when the increment-B wiring is reverted, plus the b2 dual-fight guard's
//! non-regression on incumbent single-fighter `Fight` cards (Time to Feed).

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn damage(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].damage_marked as i32
}

/// Drive a free (no-mana-cost) spell cast to full resolution, answering every
/// window: gift promise, target selection (targets fed in declared order), and
/// priority passes. Returns once the stack empties.
fn drive_cast(runner: &mut GameRunner, spell: ObjectId, targets: &[ObjectId], promise_gift: bool) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast spell");

    let mut next_target = 0usize;
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("mana");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: promise_gift })
                    .expect("decide gift");
            }
            WaitingFor::TargetSelection { .. } => {
                let t = targets[next_target];
                next_target += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(t)),
                    })
                    .expect("choose target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("resolve");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    panic!("cast did not resolve within window budget");
}

const MALAMET: &str = "Choose target creature you control and target creature you don't control. \
If the creature you control entered this turn, put a +1/+1 counter on it. \
Then those creatures fight each other.";

/// Cast Malamet with a you-control fighter (2/3) that either entered this turn or
/// a prior turn, and an opponent fighter (4/4). Returns (counters_on_mine,
/// counters_on_opp, dmg_mine, dmg_opp).
fn run_malamet(mine_entered_this_turn: bool) -> (u32, u32, i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // High toughness so both fighters survive the fight and their counters stay
    // measurable (a dead creature's counters vanish on zone change).
    let mine = scenario.add_creature(P0, "Mine", 2, 10).id();
    let opp = scenario.add_creature(P1, "Opp", 4, 10).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Malamet Battle Glyph", false, MALAMET)
        .id();
    let mut runner = scenario.build();
    {
        let turn = runner.state().turn_number;
        let etb = if mine_entered_this_turn {
            turn
        } else {
            turn.saturating_sub(1)
        };
        runner
            .state_mut()
            .objects
            .get_mut(&mine)
            .unwrap()
            .entered_battlefield_turn = Some(etb);
        // Opponent creature entered a prior turn so a mis-scoped condition subject
        // (reading slot 1) would read false, not accidentally true.
        runner
            .state_mut()
            .objects
            .get_mut(&opp)
            .unwrap()
            .entered_battlefield_turn = Some(turn.saturating_sub(1));
    }
    drive_cast(&mut runner, spell, &[mine, opp], false);
    (
        p1p1(&runner, mine),
        p1p1(&runner, opp),
        damage(&runner, mine),
        damage(&runner, opp),
    )
}

#[test]
fn malamet_counter_on_you_control_when_entered_this_turn_and_both_fight() {
    // Positive: you-control fighter entered this turn → counter on IT (slot 0),
    // never the opponent (slot 1); both creatures take fight damage.
    let (mine_c, opp_c, mine_d, opp_d) = run_malamet(true);
    assert_eq!(
        mine_c, 1,
        "counter lands on the you-control creature (slot 0)"
    );
    assert_eq!(
        opp_c, 0,
        "counter must NOT land on the opponent creature (slot 1) — model-B guard"
    );
    // Fight: mine (now 3/4 after counter) deals 3 to opp; opp (4/4) deals 4 to mine.
    assert_eq!(
        opp_d, 3,
        "opponent takes fight damage from the buffed you-creature"
    );
    assert_eq!(
        mine_d, 4,
        "you-creature takes fight damage from the opponent"
    );
}

#[test]
fn malamet_no_counter_when_entered_prior_turn_but_still_fights() {
    // Negative: you-control fighter entered a PRIOR turn → EnteredThisTurn false →
    // no counter on either creature; the fight still happens.
    let (mine_c, opp_c, mine_d, opp_d) = run_malamet(false);
    assert_eq!(
        mine_c, 0,
        "no counter when the creature did not enter this turn"
    );
    assert_eq!(opp_c, 0, "no counter on the opponent either");
    // No counter → mine stays 2/3 → deals 2 to opp; opp (4/4) deals 4 to mine.
    assert_eq!(opp_d, 2, "unbuffed you-creature deals 2");
    assert_eq!(mine_d, 4, "you-creature still takes fight damage");
}

const LONGSTALK: &str = "Gift a tapped Fish (You may promise an opponent a gift as you cast this \
spell. If you do, they create a tapped 1/1 blue Fish creature token before its other effects.)\n\
Choose target creature you control and target creature you don't control. \
Put a +1/+1 counter on the creature you control if the gift was promised. \
Then those creatures fight each other.";

/// Cast Longstalk Brawl (Gift keyword) with a 2/3 you-control fighter and a 4/4
/// opponent fighter, promising the gift or not. Returns (counters_mine,
/// counters_opp, dmg_mine, dmg_opp).
fn run_longstalk(promise_gift: bool) -> (u32, u32, i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 10).id();
    let opp = scenario.add_creature(P1, "Opp", 4, 10).id();
    let spell = {
        let mut b = scenario.add_spell_to_hand(P0, "Longstalk Brawl", false);
        b.from_oracle_text_with_keywords(&["Gift"], LONGSTALK);
        b.id()
    };
    let mut runner = scenario.build();
    drive_cast(&mut runner, spell, &[mine, opp], promise_gift);
    (
        p1p1(&runner, mine),
        p1p1(&runner, opp),
        damage(&runner, mine),
        damage(&runner, opp),
    )
}

#[test]
fn longstalk_counter_on_you_control_when_gift_promised_and_both_fight_no_panic() {
    // Gift promised → counter on the you-control creature (slot 0), both fight.
    let (mine_c, opp_c, mine_d, opp_d) = run_longstalk(true);
    assert_eq!(mine_c, 1, "gift promised → counter on you-control creature");
    assert_eq!(opp_c, 0, "counter never on the opponent creature");
    assert_eq!(opp_d, 3, "buffed you-creature (3/4) deals 3");
    assert_eq!(mine_d, 4, "opponent (4/4) deals 4");
}

#[test]
fn longstalk_no_counter_when_gift_not_promised_but_still_fights() {
    let (mine_c, opp_c, mine_d, opp_d) = run_longstalk(false);
    assert_eq!(mine_c, 0, "gift not promised → no counter");
    assert_eq!(opp_c, 0, "no counter on opponent");
    assert_eq!(opp_d, 2, "unbuffed you-creature deals 2");
    assert_eq!(mine_d, 4, "opponent deals 4");
}

const DUEL: &str = "Choose target creature you control and target creature you don't control. \
If you control three or more creatures with different powers, put a +1/+1 counter on the chosen \
creature you control. Then the chosen creatures fight each other.";

#[test]
fn duel_for_dominance_counter_under_coven_and_both_fight() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 10).id();
    let opp = scenario.add_creature(P1, "Opp", 4, 10).id();
    // Coven: three you-control creatures with DIFFERENT powers (2, 5, 6). `mine`
    // is one of them (power 2).
    scenario.add_creature(P0, "CovenA", 5, 5);
    scenario.add_creature(P0, "CovenB", 6, 6);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Duel for Dominance", true, DUEL)
        .id();
    let mut runner = scenario.build();
    drive_cast(&mut runner, spell, &[mine, opp], false);
    assert_eq!(
        p1p1(&runner, mine),
        1,
        "coven satisfied → counter on you-control creature"
    );
    assert_eq!(p1p1(&runner, opp), 0, "counter never on opponent");
    assert_eq!(damage(&runner, opp), 3, "buffed you-creature (3/4) deals 3");
    assert_eq!(damage(&runner, mine), 4, "opponent (4/4) deals 4");
}

const TIME_TO_FEED: &str = "Choose target creature an opponent controls. \
When that creature dies this turn, you gain 3 life. Target creature you control fights that creature.";

#[test]
fn time_to_feed_incumbent_fight_unchanged_by_b2_guard() {
    // b2 non-regression: Time to Feed declares two fighters (opponent creature +
    // you-control creature). Whether the divert fires or falls through, both
    // creatures must take correct mutual fight damage — the incumbent behavior.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opp = scenario.add_creature(P1, "OppTarget", 4, 4).id();
    let mine = scenario.add_creature(P0, "MyFighter", 2, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Time to Feed", false, TIME_TO_FEED)
        .id();
    let mut runner = scenario.build();
    // Declared order: opponent creature first ("Choose target creature an opponent
    // controls"), then the you-control fighter.
    drive_cast(&mut runner, spell, &[opp, mine], false);
    assert_eq!(
        damage(&runner, opp),
        2,
        "opponent (4/4) takes 2 from your 2/3 fighter"
    );
    assert_eq!(
        damage(&runner, mine),
        4,
        "your fighter takes 4 from the opponent 4/4"
    );
}

const TAIL_SWIPE: &str =
    "Choose target creature you control and target creature you don't control. \
If you cast this spell during your main phase, the creature you control gets +1/+1 until end of \
turn. Then those creatures fight each other.";

/// Tail Swipe stays RED — the class-shape guards (c)/(d) must NOT touch it. Its
/// non-counter `Pump` node is not rekeyed to `ParentTargetSlot`, gets no
/// `subject_slot`, and it keeps an `Unimplemented` node (its unparsed main-phase
/// pump gate keeps gap>0). Parse-level only — casting is out of scope.
#[test]
fn tail_swipe_class_guards_do_not_touch_it_and_stays_red() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};

    fn walk<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
        out.push(def);
        if let Some(s) = def.sub_ability.as_deref() {
            walk(s, out);
        }
        if let Some(e) = def.else_ability.as_deref() {
            walk(e, out);
        }
    }

    let parsed = parse_oracle_text(TAIL_SWIPE, "Tail Swipe", &[], &["Instant".to_string()], &[]);
    let mut nodes = Vec::new();
    for ab in &parsed.abilities {
        walk(ab, &mut nodes);
    }

    // Exclusion witness: only 1 TargetOnly slot (+ an Unimplemented for the second
    // target), so the >=2-TargetOnly rewrite guard never fires.
    let target_only = nodes
        .iter()
        .filter(|n| {
            matches!(
                &*n.effect,
                Effect::TargetOnly {
                    target: TargetFilter::Typed(_)
                }
            )
        })
        .count();
    assert!(
        target_only < 2,
        "Tail Swipe has <2 Typed TargetOnly slots (guard excludes it)"
    );

    // The Pump is untouched: no ParentTargetSlot target, no subject_slot condition.
    let has_pump = nodes
        .iter()
        .any(|n| matches!(&*n.effect, Effect::Pump { .. }));
    assert!(has_pump, "Tail Swipe still carries its Pump node");
    assert!(
        !nodes.iter().any(|n| matches!(
            &*n.effect,
            Effect::Pump {
                target: TargetFilter::ParentTargetSlot { .. },
                ..
            }
        )),
        "Pump must NOT be rekeyed to ParentTargetSlot (PutCounter-only guard)"
    );
    assert!(
        !nodes.iter().any(|n| matches!(
            &n.condition,
            Some(
                engine::types::ability::AbilityCondition::TargetMatchesFilter {
                    subject_slot: Some(_),
                    ..
                }
            )
        )),
        "no node gets a subject_slot (class-shape guard excludes Tail Swipe)"
    );
    // Stays unsupported: an Unimplemented node remains (gap > 0).
    assert!(
        nodes
            .iter()
            .any(|n| matches!(&*n.effect, Effect::Unimplemented { .. })),
        "Tail Swipe keeps an Unimplemented node — stays RED"
    );
}
