//! CR 510.3 + CR 510.4: the active player gets priority at the end of EVERY combat
//! damage step, so in a combat with first strike or double strike the players get a
//! priority window between the first-strike step and the regular step, whether or
//! not that step left anything on the stack.
//!
//! Before this window was unconditional, the engine opened it only when the first
//! step put something on the stack (#692, No Mercy); with an empty stack it ran the
//! regular step in the same action, so no player could respond in between (kill a
//! double striker, pump or give it double strike, activate an ability, ninjutsu).
//!
//! Oracle text of the printed cards used (Scryfall, verbatim):
//! - Fencing Ace {1}{W}, 1/1: "Double strike (This creature deals both first-strike and
//!   regular combat damage.)"
//! - Youthful Knight {1}{W}, 2/1: "First strike"
//! - Boros Swiftblade {R}{W}, 1/2: "Double strike"
//! - Lightwielder Paladin {3}{W}{W}, 4/4: "First strike (This creature deals combat damage
//!   before creatures without first strike.)" / "Whenever this creature deals combat damage
//!   to a player, you may exile target black or red permanent that player controls."
//! - Char-Rumbler {2}{R}{R}, -1/3: "Double strike" / "{R}: This creature gets +1/+0 until end
//!   of turn."
//! - Grizzly Bears {1}{G}, 2/2 and Goblin Piker {1}{R}, 2/1: no rules text.
//! - Lightning Bolt {R}: "Lightning Bolt deals 3 damage to any target."
//! - Giant Growth {G}: "Target creature gets +3/+3 until end of turn."
//! - Double Cleave {1}{R/W}: "Target creature gains double strike until end of turn. (It deals
//!   both first-strike and regular combat damage.)"
//!
//! Every row enters through the public action stream (`GameRunner::act`: passes, casts,
//! activations, `AssignCombatDamage`), never a direct `resolve_combat_damage` call.

use engine::ai_support::{auto_pass_recommended, legal_actions};
use engine::game::combat::{AttackTarget, CombatState};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::{run_combat, run_combat_first_damage_step};

const P2: PlayerId = PlayerId(2);

const FENCING_ACE: &str =
    "Double strike (This creature deals both first-strike and regular combat damage.)";
const YOUTHFUL_KNIGHT: &str = "First strike";
const BOROS_SWIFTBLADE: &str = "Double strike";
const LIGHTWIELDER_PALADIN: &str = "First strike (This creature deals combat damage before creatures without first strike.)\nWhenever this creature deals combat damage to a player, you may exile target black or red permanent that player controls.";
const CHAR_RUMBLER: &str = "Double strike\n{R}: This creature gets +1/+0 until end of turn.";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const DOUBLE_CLEAVE: &str = "Target creature gains double strike until end of turn. (It deals both first-strike and regular combat damage.)";

/// A printed creature built from its verbatim Oracle text; `keywords` is the card's
/// MTGJSON keyword list (the pipeline that feeds real card data), as a keyword line
/// with reminder text is not parsed from the text alone.
fn printed_creature(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
    oracle: &str,
    keywords: &[&str],
) -> ObjectId {
    scenario
        .add_creature(player, name, power, toughness)
        .from_oracle_text_with_keywords(keywords, oracle)
        .id()
}

/// Reach guard: the keyword each printed creature carries was parsed from its
/// verbatim Oracle text, so no row below silently degrades to a vanilla creature.
fn assert_parsed(runner: &GameRunner, id: ObjectId, keyword: Keyword) {
    assert!(
        runner.state().objects[&id].has_keyword(&keyword),
        "the printed Oracle text must parse to {keyword:?}"
    );
}

fn red() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

fn green() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    }
}

/// Lightning Bolt in `player`'s hand, with its printed {R} cost.
fn bolt(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Lightning Bolt", true, LIGHTNING_BOLT)
        .with_mana_cost(red())
        .id()
}

fn combat(runner: &GameRunner) -> &CombatState {
    runner
        .state()
        .combat
        .as_ref()
        .expect("combat is in progress")
}

/// The CR 510.3 window between the two combat damage steps is open: the active
/// player (P0) holds priority on an empty stack, the first-strike step is over and
/// the regular step has not begun (CR 510.4).
fn assert_window_open(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "CR 510.3: the active player holds priority after the first combat damage step, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the window is empty-stack");
    assert!(
        combat(runner).first_strike_done,
        "CR 510.4: the first-strike step has been performed"
    );
    assert!(
        !combat(runner).regular_damage_done,
        "CR 510.4: the regular step has not begun"
    );
    assert!(
        runner.between_combat_damage_steps(),
        "the shared window predicate agrees"
    );
}

fn combat_damage_steps(runner: &GameRunner) -> u32 {
    runner
        .state()
        .steps_started_this_turn
        .count(Phase::CombatDamage)
}

/// One P0 Fencing Ace attacking; P1 optionally holds Lightning Bolt with a Mountain.
fn ace_attack(p1_bolt: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ace = printed_creature(
        &mut scenario,
        P0,
        "Fencing Ace",
        1,
        1,
        FENCING_ACE,
        &["Double strike"],
    );
    let bolt_id = p1_bolt.then(|| {
        scenario.add_basic_land(P1, ManaColor::Red);
        bolt(&mut scenario, P1)
    });
    let runner = scenario.build();
    assert_parsed(&runner, ace, Keyword::DoubleStrike);
    (runner, ace, bolt_id)
}

/// N1: an instant cast in the empty first-strike window kills a double striker
/// before its second hit (CR 510.3, CR 510.4, CR 702.4b).
#[test]
fn bolt_in_the_empty_window_kills_a_double_striker_before_its_second_hit() {
    let (mut runner, ace, bolt_id) = ace_attack(true);
    let bolt_id = bolt_id.expect("P1 holds Lightning Bolt");

    run_combat_first_damage_step(&mut runner, vec![ace], vec![], &[]);

    assert_window_open(&runner);
    // Reach guard: the first hit landed, so the window is the one AFTER the first
    // step and nothing upstream short-circuited.
    assert_eq!(runner.life(P1), 19, "the Ace's first-strike hit landed");

    // P0 passes; P1 now holds priority inside the window and answers with Bolt.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes in the window");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "CR 117.3d: P1 receives priority next"
    );
    runner.cast(bolt_id).target_objects(&[ace]).resolve();
    assert_eq!(
        runner.state().objects[&ace].zone,
        Zone::Graveyard,
        "Bolt killed the Ace in the window"
    );

    runner.pass_combat_damage_window();
    assert!(combat(&runner).regular_damage_done);
    assert_eq!(
        runner.life(P1),
        19,
        "CR 702.4b: the dead double striker deals no regular-step damage"
    );
}

/// N2: control for N1. Nobody acts in the window, so the regular step follows.
#[test]
fn passing_the_window_lets_the_regular_step_follow() {
    let (mut runner, ace, _) = ace_attack(false);

    run_combat_first_damage_step(&mut runner, vec![ace], vec![], &[]);
    assert_window_open(&runner);
    assert_eq!(runner.life(P1), 19, "reach guard: the first hit landed");
    assert_eq!(
        combat_damage_steps(&runner),
        1,
        "one combat damage step has begun at the window"
    );

    runner.pass_combat_damage_window();

    assert!(combat(&runner).regular_damage_done);
    assert_eq!(runner.life(P1), 18, "CR 702.4b: the second hit lands");
    assert_eq!(combat_damage_steps(&runner), 2, "CR 510.4: two steps");

    runner.combat_damage();
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert_eq!(
        combat_damage_steps(&runner),
        2,
        "no hang and no third combat damage step"
    );
}

/// N3: a first striker alone still gets the window, then an (empty) second step
/// in which it does not strike again (CR 702.7b).
#[test]
fn first_striker_alone_gets_a_window_then_an_empty_second_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = printed_creature(
        &mut scenario,
        P0,
        "Youthful Knight",
        2,
        1,
        YOUTHFUL_KNIGHT,
        &["First strike"],
    );
    let mut runner = scenario.build();
    assert_parsed(&runner, knight, Keyword::FirstStrike);

    run_combat_first_damage_step(&mut runner, vec![knight], vec![], &[]);
    assert_window_open(&runner);
    assert_eq!(runner.life(P1), 18, "the Knight's first-strike hit landed");

    runner.pass_combat_damage_window();

    assert!(combat(&runner).regular_damage_done);
    assert_eq!(
        runner.life(P1),
        18,
        "CR 702.7b: a first striker does not deal regular combat damage"
    );
    assert_eq!(combat_damage_steps(&runner), 2);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the second step ends with its own priority window"
    );

    runner.combat_damage();
    assert_eq!(runner.state().phase, Phase::EndCombat);
}

/// N3b: multi-authority. In one combat the first striker assigns in step one only
/// and the regular creature in step two only (CR 510.4).
#[test]
fn first_striker_and_regular_creature_assign_in_different_steps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = printed_creature(
        &mut scenario,
        P0,
        "Youthful Knight",
        2,
        1,
        YOUTHFUL_KNIGHT,
        &["First strike"],
    );
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    assert_parsed(&runner, knight, Keyword::FirstStrike);

    run_combat_first_damage_step(&mut runner, vec![knight, bears], vec![], &[]);
    assert_window_open(&runner);
    assert_eq!(
        runner.life(P1),
        18,
        "only the Knight's 2 so far: Grizzly Bears has no first strike, so it did not deal damage"
    );

    runner.pass_combat_damage_window();

    assert!(combat(&runner).regular_damage_done);
    assert_eq!(
        runner.life(P1),
        16,
        "CR 510.4: the Bears' 2 lands in step two; the Knight does not strike twice"
    );
    assert_eq!(combat_damage_steps(&runner), 2);
}

/// N4: without first strike there is exactly one combat damage step and one
/// window, so the extra window is first-strike-only.
#[test]
fn no_first_strike_means_one_step_and_one_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    run_combat_first_damage_step(&mut runner, vec![bears], vec![], &[]);

    assert_eq!(
        runner.life(P1),
        18,
        "reach guard: the (only) damage step ran in the first action"
    );
    let state_combat = combat(&runner);
    assert_eq!(
        state_combat.first_strike_participants,
        Some(Default::default()),
        "no first striker or double striker is in this combat"
    );
    assert!(!state_combat.first_strike_done);
    assert!(state_combat.regular_damage_done);
    assert!(!runner.between_combat_damage_steps());
    assert_eq!(combat_damage_steps(&runner), 1);

    // One window: two passes (one per seat) reach the end of combat, not a third.
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert_eq!(runner.state().phase, Phase::CombatDamage);
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert_eq!(combat_damage_steps(&runner), 1);
}

/// N5: a trick cast between the steps changes the second step of a blocked double
/// striker (CR 510.4, CR 702.4b).
#[test]
fn trick_between_the_steps_changes_the_second_step_of_a_blocked_double_striker() {
    // Returns (runner, ace, bears, trick).
    fn blocked(with_trick: bool) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ace = printed_creature(
            &mut scenario,
            P0,
            "Fencing Ace",
            1,
            1,
            FENCING_ACE,
            &["Double strike"],
        );
        let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let trick = with_trick.then(|| {
            scenario.add_basic_land(P0, ManaColor::Green);
            scenario
                .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
                .with_mana_cost(green())
                .id()
        });
        let runner = scenario.build();
        assert_parsed(&runner, ace, Keyword::DoubleStrike);
        (runner, ace, bears, trick)
    }

    // Control: no trick. The Ace deals 1 in each step, the Bears 2 in step two.
    let (mut runner, ace, bears, _) = blocked(false);
    run_combat_first_damage_step(&mut runner, vec![ace], vec![(bears, ace)], &[]);
    assert_window_open(&runner);
    assert_eq!(runner.state().objects[&bears].damage_marked, 1);
    runner.pass_combat_damage_window();
    assert_eq!(runner.state().objects[&ace].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "control: 1 + 1 damage kills the Bears; the Bears' 2 kills the Ace"
    );

    // With Giant Growth in the window the Ace survives and kills the Bears.
    let (mut runner, ace, bears, trick) = blocked(true);
    let trick = trick.expect("P0 holds Giant Growth");
    run_combat_first_damage_step(&mut runner, vec![ace], vec![(bears, ace)], &[]);
    assert_window_open(&runner);
    assert_eq!(
        runner.state().objects[&bears].damage_marked,
        1,
        "reach guard: the Ace's first-strike damage is marked on the Bears"
    );
    assert_eq!(
        runner.state().objects[&ace].damage_marked,
        0,
        "the Bears have no first strike, so the Ace has taken nothing yet"
    );
    runner.cast(trick).target_objects(&[ace]).resolve();
    runner.pass_combat_damage_window();
    assert!(combat(&runner).regular_damage_done);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "the 4/4 Ace's second hit kills the Bears"
    );
    assert_eq!(
        runner.state().objects[&ace].zone,
        Zone::Battlefield,
        "a 4/4 survives the Bears' 2"
    );
    assert_eq!(runner.state().objects[&ace].damage_marked, 2);
}

/// N7: CR 702.4d. Giving double strike to a first striker after it dealt its
/// first-step damage lets it assign in the second step.
#[test]
fn double_cleave_in_the_window_gives_a_first_striker_a_second_hit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = printed_creature(
        &mut scenario,
        P0,
        "Youthful Knight",
        2,
        1,
        YOUTHFUL_KNIGHT,
        &["First strike"],
    );
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    let cleave = scenario
        .add_spell_to_hand_from_oracle(P0, "Double Cleave", true, DOUBLE_CLEAVE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::RedWhite],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    assert_parsed(&runner, knight, Keyword::FirstStrike);

    run_combat_first_damage_step(&mut runner, vec![knight], vec![], &[]);
    assert_window_open(&runner);
    assert_eq!(
        runner.life(P1),
        18,
        "reach guard: the first-step hit landed"
    );

    runner.cast(cleave).target_objects(&[knight]).resolve();
    assert!(
        runner.state().objects[&knight].has_keyword(&Keyword::DoubleStrike),
        "Double Cleave granted double strike"
    );
    runner.pass_combat_damage_window();

    assert_eq!(
        runner.life(P1),
        16,
        "CR 702.4d: the Knight deals its second hit in the regular step"
    );
}

/// N8: an interactive damage assignment in the first step resumes into the window,
/// and the regular step then raises its own prompt.
#[test]
fn damage_assignment_prompt_in_the_first_step_resumes_into_the_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swiftblade = printed_creature(
        &mut scenario,
        P0,
        "Boros Swiftblade",
        1,
        2,
        BOROS_SWIFTBLADE,
        &["Double strike"],
    );
    let bear_a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let bear_b = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    assert_parsed(&runner, swiftblade, Keyword::DoubleStrike);

    // The harness answers the first-step prompt itself (lethal-minimum split).
    run_combat_first_damage_step(
        &mut runner,
        vec![swiftblade],
        vec![(bear_a, swiftblade), (bear_b, swiftblade)],
        &[],
    );

    assert_window_open(&runner);
    assert_eq!(
        combat(&runner).damage_step_index,
        None,
        "the first step's assignment cursor is cleared"
    );
    let damaged: Vec<ObjectId> = [bear_a, bear_b]
        .into_iter()
        .filter(|bear| runner.state().objects[bear].damage_marked == 1)
        .collect();
    assert_eq!(
        damaged.len(),
        1,
        "reach guard: the first step's 1 damage went to exactly one Bear"
    );
    let hit = damaged[0];
    let other = if hit == bear_a { bear_b } else { bear_a };

    runner.pass_combat_damage_window();

    assert_eq!(combat_damage_steps(&runner), 2);
    let WaitingFor::AssignCombatDamage {
        attacker_id,
        total_damage,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the regular step raises the damage assignment prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*attacker_id, swiftblade);
    assert_eq!(*total_damage, 1);

    runner
        .act(GameAction::AssignCombatDamage {
            mode: engine::types::game_state::CombatDamageAssignmentMode::Normal,
            assignments: vec![(hit, 1)],
            trample_damage: 0,
            controller_damage: 0,
        })
        .expect("the hand-submitted regular-step assignment");

    assert!(combat(&runner).regular_damage_done);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Graveyard,
        "1 + 1 damage kills the Bear that took the first-step damage"
    );
    assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&swiftblade].zone,
        Zone::Graveyard,
        "the two Bears' regular-step damage (2 + 2) kills the 1/2 Swiftblade"
    );
}

/// N10: `auto_pass_recommended` at the new window: a player holding a castable
/// instant is held, a player with nothing to do is auto-passed.
#[test]
fn auto_pass_holds_a_castable_instant_in_the_window() {
    let recommendation =
        |runner: &GameRunner| auto_pass_recommended(runner.state(), &legal_actions(runner.state()));

    // P0's own window with nothing castable.
    let (mut runner, ace, _) = ace_attack(true);
    run_combat_first_damage_step(&mut runner, vec![ace], vec![], &[]);
    assert_window_open(&runner);
    assert!(
        recommendation(&runner),
        "P0 has nothing to do in the window: auto-pass"
    );

    // P1, holding Bolt with a Mountain, is held.
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    assert!(
        !recommendation(&runner),
        "CR 117.1: P1 can cast Lightning Bolt, so the window is held"
    );

    // The same window without the Bolt differs only in the Bolt.
    let (mut runner, ace, _) = ace_attack(false);
    run_combat_first_damage_step(&mut runner, vec![ace], vec![], &[]);
    assert_window_open(&runner);
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    assert!(
        recommendation(&runner),
        "P1 has nothing castable: auto-pass"
    );
}

/// N12: with three players the window needs one pass per living seat, and the
/// regular step starts only after the last. Combat is driven by hand because
/// `run_combat*` is 2-seat only.
#[test]
fn three_player_window_needs_a_pass_from_every_seat() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ace = printed_creature(
        &mut scenario,
        P0,
        "Fencing Ace",
        1,
        1,
        FENCING_ACE,
        &["Double strike"],
    );
    let mut runner = scenario.build();
    assert_parsed(&runner, ace, Keyword::DoubleStrike);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(ace, AttackTarget::Player(P1))])
        .expect("declare the Ace attacking P1");

    let mut reached_window = false;
    for _ in 0..80 {
        if runner.between_combat_damage_steps() {
            reached_window = true;
            break;
        }
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected window while driving combat: {other:?}"),
        }
    }
    assert!(
        reached_window,
        "the loop must end at the window, not at its cap"
    );

    assert_window_open(&runner);
    assert_eq!(runner.life(P1), 19, "the first-strike hit landed");

    let regular_done = |runner: &GameRunner| combat(runner).regular_damage_done;
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    assert!(!regular_done(&runner));
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P2),
        "the window is still open for the third seat"
    );
    assert!(!regular_done(&runner));
    runner.act(GameAction::PassPriority).expect("P2 passes");
    assert!(regular_done(&runner));
    assert_eq!(
        runner.life(P1),
        18,
        "the regular step ran after the last pass"
    );
}

/// N13: a first-strike step in which nothing is dealt still opens the window, and a
/// live power change inside it changes the second step (Char-Rumbler rulings;
/// CR 510.4, CR 702.4b, CR 510.1a).
#[test]
fn zero_power_first_step_still_opens_a_window_and_a_pump_changes_the_second_step() {
    fn rumbler_attack() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let rumbler = printed_creature(
            &mut scenario,
            P0,
            "Char-Rumbler",
            -1,
            3,
            CHAR_RUMBLER,
            &["Double strike"],
        );
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_basic_land(P0, ManaColor::Red);
        let runner = scenario.build();
        assert_parsed(&runner, rumbler, Keyword::DoubleStrike);
        (runner, rumbler)
    }

    // Control: no activation. The step still happens; nothing is ever dealt.
    let (mut runner, rumbler) = rumbler_attack();
    run_combat_first_damage_step(&mut runner, vec![rumbler], vec![], &[]);
    assert_window_open(&runner);
    runner.pass_combat_damage_window();
    assert!(combat(&runner).regular_damage_done);
    assert_eq!(runner.life(P1), 20, "control: power -1 deals no damage");
    assert_eq!(combat_damage_steps(&runner), 2, "the second step happened");

    // Pump twice inside the window.
    let (mut runner, rumbler) = rumbler_attack();
    run_combat_first_damage_step(&mut runner, vec![rumbler], vec![], &[]);
    assert_window_open(&runner);
    assert_eq!(
        runner.life(P1),
        20,
        "CR 510.1a: power -1 assigns no damage in the first step"
    );
    runner.activate(rumbler, 0).resolve();
    runner.activate(rumbler, 0).resolve();
    assert_eq!(runner.state().objects[&rumbler].power, Some(1));
    runner.pass_combat_damage_window();
    assert_eq!(
        runner.life(P1),
        19,
        "the power-1 Char-Rumbler deals 1 in the regular step"
    );
    assert_eq!(combat_damage_steps(&runner), 2);
}

/// N9: hostile. A targeted "may" combat-damage trigger of a first striker (two legal
/// targets, so the target is chosen at the prompt) is collected exactly once at the
/// end of the first step, resolves inside the window, and the regular step still
/// runs afterwards without re-collecting it. This is a guard, not a revert-failing
/// row: the trigger is already on the stack while its target is chosen (CR 603.3d),
/// so the first-step window was already open for it before the window became
/// unconditional.
#[test]
fn targeted_first_strike_damage_trigger_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let paladin = printed_creature(
        &mut scenario,
        P0,
        "Lightwielder Paladin",
        4,
        4,
        LIGHTWIELDER_PALADIN,
        &["First strike"],
    );
    // Goblin Piker is a red creature with no rules text.
    let piker_a = scenario
        .add_creature(P1, "Goblin Piker", 2, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let piker_b = scenario
        .add_creature(P1, "Goblin Piker", 2, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    assert_parsed(&runner, paladin, Keyword::FirstStrike);

    run_combat(&mut runner, vec![paladin], vec![]);

    // Reach guard: the first-strike hit landed and the trigger is awaiting its target.
    assert_eq!(runner.life(P1), 16, "the Paladin's first-strike hit landed");
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for
    else {
        panic!(
            "expected the exile trigger's target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "one trigger, one target slot");
    assert_eq!(
        target_slots[0].legal_targets.len(),
        2,
        "both red Goblin Pikers are legal targets"
    );
    assert!(
        combat(&runner).first_strike_done && !combat(&runner).regular_damage_done,
        "the target prompt is raised between the two combat damage steps"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(piker_a)),
        })
        .expect("choose the exile target");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "exactly one trigger instance"
    );

    // Both players pass: the trigger resolves; accept its "may".
    runner.act(GameAction::PassPriority).expect("P0 passes");
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the resolving trigger asks whether to exile, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the exile");

    assert_eq!(runner.state().objects[&piker_a].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&piker_b].zone,
        Zone::Battlefield,
        "the trigger was collected once: one exile, no second instance"
    );
    assert_window_open(&runner);

    runner.pass_combat_damage_window();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no further prompt: the trigger is not re-collected from the pass, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(combat(&runner).regular_damage_done);
    assert_eq!(combat_damage_steps(&runner), 2);
    assert_eq!(runner.state().objects[&piker_b].zone, Zone::Battlefield);
    assert_eq!(
        runner.life(P1),
        16,
        "a first striker deals no regular-step damage"
    );
}

/// N14: ninjutsu in the empty first-strike window (Blade-Blizzard Kitsune ruling:
/// "If a creature in combat has first strike or double strike, you can activate the
/// ninjutsu ability during the first-strike combat damage step. The Ninja will deal
/// combat damage during the regular combat damage step, even if it has first
/// strike."). The Kitsune enters after the first step's participants were fixed
/// (CR 510.4), so it assigns in the regular step only.
#[test]
fn ninjutsu_in_the_window_puts_a_ninja_into_the_second_step() {
    const KITSUNE: &str = "Ninjutsu {3}{W} ({3}{W}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nDouble strike";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ace = printed_creature(
        &mut scenario,
        P0,
        "Fencing Ace",
        1,
        1,
        FENCING_ACE,
        &["Double strike"],
    );
    let kitsune = scenario
        .add_creature_to_hand(P0, "Blade-Blizzard Kitsune", 2, 2)
        .from_oracle_text_with_keywords(&["Ninjutsu", "Double strike"], KITSUNE)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();
    assert_parsed(&runner, ace, Keyword::DoubleStrike);
    assert_parsed(&runner, kitsune, Keyword::DoubleStrike);

    run_combat_first_damage_step(&mut runner, vec![ace], vec![], &[]);
    assert_window_open(&runner);
    assert_eq!(
        runner.life(P1),
        19,
        "reach guard: the Ace's first hit landed"
    );

    runner
        .act(GameAction::ActivateNinjutsu {
            ninjutsu_object_id: kitsune,
            creature_to_return: ace,
        })
        .expect("ninjutsu is legal in the first combat damage step's window");
    // Both players pass: the ninjutsu ability resolves.
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve ninjutsu");
    }
    assert_eq!(runner.state().objects[&kitsune].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&ace].zone, Zone::Hand);
    assert_window_open(&runner);

    runner.pass_combat_damage_window();

    assert!(combat(&runner).regular_damage_done);
    assert_eq!(
        runner.life(P1),
        17,
        "the Kitsune deals 2 in the regular step; the returned Ace deals no second hit"
    );
    assert_eq!(combat_damage_steps(&runner), 2);
}
