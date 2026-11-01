//! Revenant Patriarch: "When this creature enters, if {W} was spent to cast
//! it, target player skips their next combat phase." and the rest of the
//! whole-combat-phase skips, driven through `GameAction::PassPriority` so they
//! exercise the path players use (CR 500.11, CR 614.10, CR 614.10a).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Duration, TargetRef};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, ExtraPhase, PendingCombatPhaseSkip, WaitingFor};
use engine::types::identifiers::{ExtraPhaseId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall).
const PATRIARCH_ORACLE: &str = "When this creature enters, if {W} was spent to cast it, target player skips their next combat phase.\nThis creature can't block.";
const MOMENT_OF_SILENCE_ORACLE: &str = "Target player skips their next combat phase this turn.";
const VEDALKEN_ORRERY_ORACLE: &str = "You may cast spells as though they had flash.";
const FALSE_PEACE_ORACLE: &str = "Target player skips all combat phases of their next turn.";
const SINISTER_MONOLITH_ORACLE: &str = "At the beginning of combat on your turn, each opponent loses 1 life and you gain 1 life.\n{T}, Pay 2 life, Sacrifice this artifact: Draw two cards. Activate only as a sorcery.";
const TRENCH_BEHEMOTH_ORACLE: &str = "Return a land you control to its owner's hand: Untap this creature. It gains hexproof until end of turn.\nLandfall — Whenever a land you control enters, target creature an opponent controls attacks during its controller's next combat phase if able.";

const BEAR_POWER: i32 = 2;
/// P0's life lost to one P1 combat: the bear plus Sinister Monolith's trigger.
const ONE_COMBAT: i32 = BEAR_POWER + 1;
const P0_STARTING_LIFE: i32 = 20;
const P1_STARTING_LIFE: i32 = 20;

/// P0 holds the skip effects and P1 has what a combat phase would use: an
/// attacker and a beginning of combat trigger.
struct Table {
    runner: GameRunner,
    patriarch: ObjectId,
    moment: ObjectId,
    false_peace: ObjectId,
    forest: ObjectId,
    bear: ObjectId,
}

fn table() -> Table {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let library = ["Island"; 12];
    scenario.with_library_top(P0, &library);
    scenario.with_library_top(P1, &library);
    let patriarch = {
        let mut builder = scenario.add_creature_to_hand(P0, "Revenant Patriarch", 4, 3);
        builder.from_oracle_text(PATRIARCH_ORACLE);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        });
        builder.id()
    };
    let moment = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Moment of Silence",
            true,
            MOMENT_OF_SILENCE_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        });
        builder.id()
    };
    let false_peace = {
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(P0, "False Peace", false, FALSE_PEACE_ORACLE);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        });
        builder.id()
    };
    scenario.add_artifact_from_oracle(P0, "Vedalken Orrery", VEDALKEN_ORRERY_ORACLE);
    scenario.add_creature_from_oracle(P0, "Trench Behemoth", 7, 7, TRENCH_BEHEMOTH_ORACLE);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let bear = scenario.add_vanilla(P1, BEAR_POWER, 2);
    scenario.add_artifact_from_oracle(P1, "Sinister Monolith", SINISTER_MONOLITH_ORACLE);
    Table {
        runner: scenario.build(),
        patriarch,
        moment,
        false_peace,
        forest,
        bear,
    }
}

fn add_mana(runner: &mut GameRunner, player: PlayerId, mana_type: ManaType, amount: u32) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }
}

/// Cast `spell`, answer every target prompt with `target`, and resolve
/// everything back to an empty stack. Returns whether a target prompt or the
/// spell's own ability was answered (reach guard).
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId, target: PlayerId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    let phase = runner.state().phase;
    let active = runner.state().active_player;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast accepted");

    let mut target_chosen = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::TargetSelection { .. }
            | WaitingFor::MultiTargetSelection { .. } => {
                target_chosen = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(target)),
                    })
                    .or_else(|_| {
                        runner.act(GameAction::SelectTargets {
                            targets: vec![TargetRef::Player(target)],
                        })
                    })
                    .expect("target accepted");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }
    assert!(runner.state().stack.is_empty(), "everything resolved");
    assert_eq!(
        (runner.state().phase, runner.state().active_player),
        (phase, active),
        "resolving must not leave the step the effect was cast in"
    );
    target_chosen
}

/// Cast Patriarch paying `white` W and `black` B, targeting `target` with its
/// enters trigger.
fn cast_patriarch(table: &mut Table, white: u32, black: u32, target: PlayerId) -> bool {
    add_mana(&mut table.runner, P0, ManaType::White, white);
    add_mana(&mut table.runner, P0, ManaType::Black, black);
    cast_and_resolve(&mut table.runner, table.patriarch, target)
}

fn cast_moment(table: &mut Table, target: PlayerId) {
    add_mana(&mut table.runner, P0, ManaType::White, 1);
    assert!(cast_and_resolve(&mut table.runner, table.moment, target));
    assert_eq!(
        table.runner.state().objects[&table.moment].zone,
        Zone::Graveyard,
        "Moment of Silence resolved"
    );
}

/// Pass priority from wherever the game is until `active`'s turn reaches
/// `phase` with an empty stack, attacking with P1's bear whenever P1 is asked.
/// Returns how many times P1 was asked to declare attackers.
fn pass_to(table: &mut Table, active: PlayerId, phase: Phase) -> u32 {
    let mut attack_prompts = 0;
    for _ in 0..400 {
        let state = table.runner.state();
        let waiting = state.waiting_for.clone();
        if state.active_player == active
            && state.phase == phase
            && state.stack.is_empty()
            && matches!(waiting, WaitingFor::Priority { .. })
        {
            return attack_prompts;
        }
        match waiting {
            WaitingFor::DeclareAttackers { player, .. } => {
                let attacks = if player == P1 {
                    attack_prompts += 1;
                    vec![(table.bear, AttackTarget::Player(P0))]
                } else {
                    vec![]
                };
                table
                    .runner
                    .declare_attackers(&attacks)
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                table.runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::Priority { .. } => {
                table.runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }
    panic!("never reached {phase:?} on {active:?}'s turn");
}

/// Play P1's whole combat from their precombat main phase. Returns the attack
/// prompts P1 saw there.
fn play_p1_combat(table: &mut Table) -> u32 {
    pass_to(table, P1, Phase::PreCombatMain);
    pass_to(table, P1, Phase::PostCombatMain)
}

fn begin_combats_this_turn(table: &Table) -> u32 {
    table
        .runner
        .state()
        .steps_started_this_turn
        .count(Phase::BeginCombat)
}

fn pending_skips(table: &Table, player: PlayerId) -> usize {
    table
        .runner
        .state()
        .pending_combat_phase_skips
        .iter()
        .filter(|skip| skip.player == player)
        .count()
}

/// Asserts P1's combat just ran in full, once.
fn assert_combat_ran(table: &Table, prompts: u32, combats: u32) {
    assert_eq!(prompts, combats, "P1 is asked to attack");
    assert_eq!(begin_combats_this_turn(table), combats);
}

/// Asserts the combat P1 was due just now did not exist: no trigger, no prompt,
/// no attack, and the postcombat main phase follows the precombat one.
fn assert_combat_skipped(table: &Table, prompts: u32, life: (i32, i32)) {
    let state = table.runner.state();
    assert_eq!(state.phase, Phase::PostCombatMain);
    assert_eq!(prompts, 0, "no DeclareAttackers prompt");
    assert_eq!(
        begin_combats_this_turn(table),
        0,
        "no beginning of combat step"
    );
    assert_eq!(
        (table.runner.life(P0), table.runner.life(P1)),
        life,
        "no beginning of combat trigger and no combat damage"
    );
    assert!(state.combat.is_none(), "no combat state was created");
    assert!(
        !state.objects[&table.bear].tapped,
        "the bear did not attack"
    );
}

#[test]
fn white_spent_makes_target_player_skip_next_combat() {
    let mut table = table();
    assert!(cast_patriarch(&mut table, 1, 4, P1), "trigger must target");
    assert_eq!(pending_skips(&table, P1), 1, "P1 must skip next combat");
    assert_eq!(pending_skips(&table, P0), 0);
    assert!(
        table.runner.state().steps_to_skip[P1.0 as usize].is_empty(),
        "a whole phase is not armed as step counters"
    );

    // P1's next combat is skipped straight to postcombat main.
    let prompts = play_p1_combat(&mut table);
    assert_combat_skipped(&table, prompts, (P0_STARTING_LIFE, P1_STARTING_LIFE));
    assert_eq!(pending_skips(&table, P1), 0, "skip fully consumed");

    // P1's following combat runs: prompt, trigger and damage.
    let prompts = {
        pass_to(&mut table, P0, Phase::End);
        play_p1_combat(&mut table)
    };
    assert_combat_ran(&table, prompts, 1);
    assert_eq!(table.runner.life(P0), P0_STARTING_LIFE - ONE_COMBAT);
    assert_eq!(table.runner.life(P1), P1_STARTING_LIFE + 1);
}

#[test]
fn no_white_spent_does_not_skip_combat() {
    let mut table = table();
    cast_patriarch(&mut table, 0, 5, P1);
    assert_eq!(pending_skips(&table, P1), 0);
    assert_eq!(pending_skips(&table, P0), 0);

    let prompts = play_p1_combat(&mut table);
    assert_combat_ran(&table, prompts, 1);
    assert_eq!(table.runner.life(P0), P0_STARTING_LIFE - ONE_COMBAT);
}

/// The caster's own combat this turn is another player's: it neither uses up
/// nor is affected by P1's skip.
#[test]
fn another_players_combat_does_not_use_up_the_skip() {
    let mut table = table();
    cast_patriarch(&mut table, 1, 4, P1);

    pass_to(&mut table, P0, Phase::PostCombatMain);
    assert_eq!(begin_combats_this_turn(&table), 1, "P0's combat runs");
    assert_eq!(pending_skips(&table, P1), 1);
}

/// CR 614.10: a skip created after a combat phase has started waits for the
/// next occurrence. Revenant Patriarch cast with Vedalken Orrery during P1's
/// beginning of combat leaves that combat untouched and takes P1's next one.
#[test]
fn skip_created_during_a_combat_leaves_that_combat_untouched() {
    let mut table = table();
    pass_to(&mut table, P1, Phase::BeginCombat);
    table.runner.act(GameAction::PassPriority).expect("pass");
    assert_eq!(table.runner.state().phase, Phase::BeginCombat);
    assert!(cast_patriarch(&mut table, 1, 4, P1));
    assert_eq!(pending_skips(&table, P1), 1);

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_ran(&table, prompts, 1);
    assert_eq!(table.runner.life(P0), P0_STARTING_LIFE - ONE_COMBAT);
    assert_eq!(pending_skips(&table, P1), 1, "still waiting");

    let life = (table.runner.life(P0), table.runner.life(P1));
    pass_to(&mut table, P0, Phase::End);
    let prompts = play_p1_combat(&mut table);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(pending_skips(&table, P1), 0);
}

/// CR 500.8 + CR 614.10: the skip lands on the next combat phase, an additional
/// combat included, and CR 614.10a it is satisfied there: Moment of Silence is
/// used up within the turn it was cast.
#[test]
fn skip_lands_on_an_additional_combat_phase_of_the_same_turn() {
    let mut table = table();
    pass_to(&mut table, P1, Phase::BeginCombat);
    table.runner.state_mut().extra_phases.push(ExtraPhase {
        anchor: Phase::EndCombat,
        segment: TurnSegment::Phase(PhaseGroup::Combat),
        attacker_restriction: None,
        attacker_restriction_source: None,
        id: ExtraPhaseId::default(),
    });
    table.runner.act(GameAction::PassPriority).expect("pass");
    cast_moment(&mut table, P1);
    assert_eq!(pending_skips(&table, P1), 1);

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_ran(&table, prompts, 1);
    assert_eq!(pending_skips(&table, P1), 0, "used up by the added combat");
    assert!(table.runner.state().extra_phases.is_empty());
    assert_eq!(table.runner.life(P1), P1_STARTING_LIFE + 1, "one trigger");
}

/// CR 514.2: Moment of Silence cast on a player whose turn it isn't ends at
/// cleanup without touching that player's next turn.
#[test]
fn moment_of_silence_expires_at_cleanup_on_another_players_turn() {
    let mut table = table();
    cast_moment(&mut table, P1);
    assert_eq!(pending_skips(&table, P1), 1, "reach guard: the skip armed");

    pass_to(&mut table, P1, Phase::Upkeep);
    assert_eq!(pending_skips(&table, P1), 0, "ended with the turn");

    let prompts = play_p1_combat(&mut table);
    assert_combat_ran(&table, prompts, 1);
}

/// CR 514.2 + CR 614.10: cast during the target's only combat of the turn, the
/// skip finds no further combat and does not carry over.
#[test]
fn moment_of_silence_during_the_only_combat_expires_unused() {
    let mut table = table();
    pass_to(&mut table, P1, Phase::BeginCombat);
    table.runner.act(GameAction::PassPriority).expect("pass");
    cast_moment(&mut table, P1);
    assert_eq!(pending_skips(&table, P1), 1, "reach guard: the skip armed");

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_ran(&table, prompts, 1);

    pass_to(&mut table, P0, Phase::Upkeep);
    assert_eq!(pending_skips(&table, P1), 0);
}

/// CR 614.10a: two skips cover P1's next two combat phases, one each.
#[test]
fn two_skips_cover_the_next_two_combat_phases() {
    let mut table = table();
    let skip = PendingCombatPhaseSkip {
        player: P1,
        duration: None,
    };
    table.runner.state_mut().pending_combat_phase_skips = vec![skip.clone(), skip];

    let life = (P0_STARTING_LIFE, P1_STARTING_LIFE);
    for remaining in [1, 0] {
        let prompts = play_p1_combat(&mut table);
        assert_combat_skipped(&table, prompts, life);
        assert_eq!(pending_skips(&table, P1), remaining);
        pass_to(&mut table, P0, Phase::End);
    }

    let prompts = play_p1_combat(&mut table);
    assert_combat_ran(&table, prompts, 1);
}

/// A label of a skip offered at P1's CR 616.1 prompt.
const NEXT_COMBAT: &str = "Skip your next combat phase";
const NEXT_COMBAT_THIS_TURN: &str = "Skip your next combat phase this turn";
const ALL_COMBATS_THIS_TURN: &str = "Skip all combat phases this turn";

/// Pass priority until the combat P1 is due asks them which skip applies, and
/// return the offered labels. The game stands before that combat: nothing of it
/// has begun, and the prompt names no unrelated object as a source.
fn pass_to_skip_choice(table: &mut Table) -> Vec<String> {
    for _ in 0..40 {
        let state = table.runner.state();
        if let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &state.waiting_for
        {
            assert_eq!(*player, P1, "the affected player chooses");
            assert!(candidates.iter().all(|c| c.source_name.is_empty()));
            assert_eq!(
                begin_combats_this_turn(table),
                0,
                "the combat has not begun"
            );
            return candidates.iter().map(|c| c.description.clone()).collect();
        }
        table.runner.act(GameAction::PassPriority).expect("pass");
    }
    panic!("P1 was never asked which skip applies");
}

/// Answer the prompt with the skip labelled `label`.
fn choose_skip(table: &mut Table, offered: &[String], label: &str) {
    let index = offered
        .iter()
        .position(|description| description == label)
        .expect("the skip is offered");
    table
        .runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choice accepted");
}

/// P0 puts Revenant Patriarch's skip and then Moment of Silence on P1 in P1's
/// precombat main phase. Patriarch's is the older, so it comes first at the prompt.
fn both_skips_on_p1_in_their_main_phase() -> Table {
    let mut table = table();
    pass_to(&mut table, P1, Phase::PreCombatMain);
    table.runner.act(GameAction::PassPriority).expect("pass");
    assert!(cast_patriarch(&mut table, 1, 4, P1));
    table.runner.act(GameAction::PassPriority).expect("pass");
    cast_moment(&mut table, P1);
    assert_eq!(pending_skips(&table, P1), 2);
    table
}

fn remaining_skip_duration(table: &Table) -> Option<Duration> {
    let skips = &table.runner.state().pending_combat_phase_skips;
    assert_eq!(skips.len(), 1, "only the unchosen skip stays pending");
    skips[0].duration.clone()
}

/// CR 616.1: applying Moment of Silence to this combat keeps Patriarch's skip for
/// P1's next one. CR 614.10a: the unchosen effect waits for its own occurrence.
#[test]
fn choosing_the_limited_skip_keeps_the_unlimited_one_for_a_later_combat() {
    let mut table = both_skips_on_p1_in_their_main_phase();
    let life = (table.runner.life(P0), table.runner.life(P1));

    let offered = pass_to_skip_choice(&mut table);
    assert_eq!(offered, [NEXT_COMBAT, NEXT_COMBAT_THIS_TURN]);
    choose_skip(&mut table, &offered, NEXT_COMBAT_THIS_TURN);

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(remaining_skip_duration(&table), None);

    pass_to(&mut table, P0, Phase::End);
    let prompts = play_p1_combat(&mut table);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(pending_skips(&table, P1), 0);

    pass_to(&mut table, P0, Phase::End);
    let prompts = play_p1_combat(&mut table);
    assert_combat_ran(&table, prompts, 1);
}

/// CR 616.1: applying Patriarch's skip to this combat leaves Moment of Silence
/// pending until the turn ends (CR 514.2), so P1's next combat runs. The chosen
/// skip is the older one so this isn't what first-listed order would pick.
#[test]
fn choosing_the_unlimited_skip_leaves_the_limited_one_to_expire() {
    let mut table = both_skips_on_p1_in_their_main_phase();
    let life = (table.runner.life(P0), table.runner.life(P1));

    let offered = pass_to_skip_choice(&mut table);
    choose_skip(&mut table, &offered, NEXT_COMBAT);

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(
        remaining_skip_duration(&table),
        Some(Duration::UntilEndOfTurn)
    );

    pass_to(&mut table, P0, Phase::Upkeep);
    assert_eq!(pending_skips(&table, P1), 0, "ended with the turn");

    pass_to(&mut table, P0, Phase::End);
    let prompts = play_p1_combat(&mut table);
    assert_combat_ran(&table, prompts, 1);
}

/// P0 casts False Peace and Revenant Patriarch on P1, so P1's next turn starts
/// with a turn-scoped skip and a one-shot skip competing.
fn false_peace_and_patriarch_on_p1() -> Table {
    let mut table = table();
    add_mana(&mut table.runner, P0, ManaType::White, 1);
    assert!(cast_and_resolve(&mut table.runner, table.false_peace, P1));
    assert!(cast_patriarch(&mut table, 1, 4, P1));
    pass_to(&mut table, P1, Phase::PreCombatMain);
    table
}

/// CR 616.1 + CR 616.1e: False Peace may replace the combat, which keeps the
/// one-shot for P1's next turn.
#[test]
fn choosing_false_peace_keeps_the_one_shot_for_a_later_turn() {
    let mut table = false_peace_and_patriarch_on_p1();
    let life = (P0_STARTING_LIFE, P1_STARTING_LIFE);

    let offered = pass_to_skip_choice(&mut table);
    assert_eq!(offered, [ALL_COMBATS_THIS_TURN, NEXT_COMBAT]);
    choose_skip(&mut table, &offered, ALL_COMBATS_THIS_TURN);

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(pending_skips(&table, P1), 1, "the one-shot was not used");

    pass_to(&mut table, P0, Phase::End);
    let prompts = play_p1_combat(&mut table);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(pending_skips(&table, P1), 0);
}

/// CR 616.1 + CR 616.1e: the one-shot may replace the combat instead. It is used
/// up, and False Peace, which covers that turn only, leaves P1's next turn alone.
#[test]
fn choosing_the_one_shot_uses_it_up_on_a_false_peace_turn() {
    let mut table = false_peace_and_patriarch_on_p1();
    let life = (P0_STARTING_LIFE, P1_STARTING_LIFE);

    let offered = pass_to_skip_choice(&mut table);
    choose_skip(&mut table, &offered, NEXT_COMBAT);

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(pending_skips(&table, P1), 0, "applied to that combat");

    pass_to(&mut table, P0, Phase::End);
    let prompts = play_p1_combat(&mut table);
    assert_combat_ran(&table, prompts, 1);
}

/// CR 500.8 + CR 616.1: each combat phase would begin on its own. An additional
/// combat after a skipped one asks again, resuming through the same transition;
/// False Peace also covers it, so P1 keeps the one-shot through both.
#[test]
fn an_additional_combat_is_a_fresh_choice_among_the_skips_left() {
    let mut table = false_peace_and_patriarch_on_p1();
    table.runner.state_mut().extra_phases.push(ExtraPhase {
        anchor: Phase::EndCombat,
        segment: TurnSegment::Phase(PhaseGroup::Combat),
        attacker_restriction: None,
        attacker_restriction_source: None,
        id: ExtraPhaseId::default(),
    });

    for _ in 0..2 {
        let offered = pass_to_skip_choice(&mut table);
        choose_skip(&mut table, &offered, ALL_COMBATS_THIS_TURN);
    }

    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    let life = (P0_STARTING_LIFE, P1_STARTING_LIFE);
    assert_combat_skipped(&table, prompts, life);
    assert!(table.runner.state().extra_phases.is_empty());
    assert_eq!(pending_skips(&table, P1), 1);
}

/// CR 616.1: with the same effect twice there is nothing to choose between.
#[test]
fn identical_skips_are_not_a_choice() {
    let mut table = table();
    pass_to(&mut table, P1, Phase::PreCombatMain);
    let skip = PendingCombatPhaseSkip {
        player: P1,
        duration: Some(Duration::UntilEndOfTurn),
    };
    table.runner.state_mut().pending_combat_phase_skips = vec![skip.clone(), skip];

    let life = (P0_STARTING_LIFE, P1_STARTING_LIFE);
    let prompts = pass_to(&mut table, P1, Phase::PostCombatMain);
    assert_combat_skipped(&table, prompts, life);
    assert_eq!(pending_skips(&table, P1), 1, "one skip used up");
}

/// CR 800.4a + CR 800.4j: P1 leaves with the choice unanswered. The prompt goes
/// with them and the turn runs out for the player left in the game.
#[test]
fn the_affected_player_leaving_at_the_choice_ends_their_turn() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, PlayerId(2)] {
        scenario.with_library_top(player, &["Island"; 12]);
    }
    scenario.add_vanilla(PlayerId(2), BEAR_POWER, 2);
    let mut runner = scenario.build();
    pass_team_turn_to(&mut runner, P1, Phase::PreCombatMain);
    runner.state_mut().pending_combat_phase_skips = vec![
        PendingCombatPhaseSkip {
            player: P1,
            duration: None,
        },
        PendingCombatPhaseSkip {
            player: P1,
            duration: Some(Duration::UntilEndOfTurn),
        },
    ];
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach guard: P1 is asked, not {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("concede");
    assert!(runner.state().pending_phase_transition_progress.is_none());

    let prompts = pass_team_turn_to(&mut runner, PlayerId(2), Phase::PostCombatMain);
    assert_eq!(prompts, 1, "the next player's combat runs");
}

/// CR 500.11: False Peace alone, through normal priority passing.
#[test]
fn turn_scoped_skip_runs_no_skipped_step() {
    let mut table = table();
    add_mana(&mut table.runner, P0, ManaType::White, 1);
    assert!(cast_and_resolve(&mut table.runner, table.false_peace, P1));

    let prompts = play_p1_combat(&mut table);
    assert_combat_skipped(&table, prompts, (P0_STARTING_LIFE, P1_STARTING_LIFE));
}

/// CR 614.10a: what is scheduled for a player's next combat phase waits past a
/// skipped one. Trench Behemoth's requirement outlives P1's skipped combat, and
/// CR 508.1d holds P1 to it in the first combat that happens.
#[test]
fn next_combat_requirement_waits_past_a_skipped_combat_phase() {
    let mut table = table();
    let forest = table.forest;
    let card_id = table.runner.state().objects[&forest].card_id;
    table
        .runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("play land");
    for _ in 0..10 {
        match table.runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if table.runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                table.runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                table
                    .runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(table.bear)),
                    })
                    .expect("target the bear");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }
    assert!(cast_patriarch(&mut table, 1, 4, P1));

    let prompts = play_p1_combat(&mut table);
    assert_combat_skipped(&table, prompts, (P0_STARTING_LIFE, P1_STARTING_LIFE));

    pass_to(&mut table, P0, Phase::End);
    pass_to(&mut table, P1, Phase::BeginCombat);
    for _ in 0..4 {
        if matches!(
            table.runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            break;
        }
        table.runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        table.runner.declare_attackers(&[]).is_err(),
        "the bear must attack in the first combat that happens"
    );
    table
        .runner
        .declare_attackers(&[(table.bear, AttackTarget::Player(P0))])
        .expect("the bear attacks");
}

/// Pass priority until `active`'s turn reaches `phase` with an empty stack,
/// declaring no attackers. Returns how many times `active` was asked to attack.
fn pass_team_turn_to(runner: &mut GameRunner, active: PlayerId, phase: Phase) -> u32 {
    let mut attack_prompts = 0;
    for _ in 0..400 {
        let state = runner.state();
        let waiting = state.waiting_for.clone();
        if state.active_player == active
            && state.phase == phase
            && state.stack.is_empty()
            && matches!(waiting, WaitingFor::Priority { .. })
        {
            return attack_prompts;
        }
        match waiting {
            WaitingFor::DeclareAttackers { player, .. } => {
                attack_prompts += u32::from(player == active);
                runner.declare_attackers(&[]).expect("no attackers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }
    panic!("never reached {phase:?} on {active:?}'s turn");
}

/// A game in `format` where P0 has cast Revenant Patriarch with {W} on `target`.
/// `attacker` controls a creature, so their combat would prompt.
fn patriarch_cast_at(format: FormatConfig, target: PlayerId, attacker: PlayerId) -> GameRunner {
    let mut scenario = GameScenario::new_with_format(format, 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, PlayerId(2), PlayerId(3)] {
        scenario.with_library_top(player, &["Island"; 12]);
    }
    let patriarch = {
        let mut builder = scenario.add_creature_to_hand(P0, "Revenant Patriarch", 4, 3);
        builder.from_oracle_text(PATRIARCH_ORACLE);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        });
        builder.id()
    };
    scenario.add_vanilla(attacker, BEAR_POWER, 2);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::White, 1);
    add_mana(&mut runner, P0, ManaType::Black, 4);
    assert!(cast_and_resolve(&mut runner, patriarch, target));
    assert_eq!(runner.state().pending_combat_phase_skips.len(), 1);
    runner
}

/// Asserts the skip took `team`'s next combat, then that their following one runs.
fn assert_team_combat_skipped_once(runner: &mut GameRunner, team: PlayerId) {
    let prompts = pass_team_turn_to(runner, team, Phase::PostCombatMain);
    assert_eq!(prompts, 0, "the team is not asked to attack");
    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::BeginCombat),
        0
    );
    assert!(runner.state().pending_combat_phase_skips.is_empty());

    pass_team_turn_to(runner, P0, Phase::End);
    let prompts = pass_team_turn_to(runner, team, Phase::PostCombatMain);
    assert_eq!(prompts, 1, "the team's following combat runs");
}

/// CR 805.8: a skip aimed at either player of a team is the team's. P3 is
/// targeted so the skip isn't on P2, the active player of the team's turns.
#[test]
fn two_headed_giant_skip_on_the_second_seat_skips_the_teams_combat() {
    let (p2, p3) = (PlayerId(2), PlayerId(3));
    let mut runner = patriarch_cast_at(FormatConfig::two_headed_giant(), p3, p2);
    assert_team_combat_skipped_once(&mut runner, p2);
}

/// CR 805.8: the skip stays the team's after the seat that represented the team
/// leaves the game. Archenemy's heroes share turns and leave one by one.
#[test]
fn archenemy_skip_survives_the_team_representative_leaving() {
    let p2 = PlayerId(2);
    let mut runner = patriarch_cast_at(FormatConfig::archenemy(), P1, p2);
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("concede");
    assert_team_combat_skipped_once(&mut runner, p2);
}

/// CR 800.4j + CR 514.2: when the active player leaves, their turn still ends, so
/// Moment of Silence on a player whose turn it isn't doesn't reach that player's
/// next turn.
#[test]
fn moment_of_silence_ends_with_a_turn_its_active_player_left() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, PlayerId(2)] {
        scenario.with_library_top(player, &["Island"; 12]);
    }
    let moment = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Moment of Silence",
            true,
            MOMENT_OF_SILENCE_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        });
        builder.id()
    };
    scenario.add_vanilla(P1, BEAR_POWER, 2);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::White, 1);
    assert!(cast_and_resolve(&mut runner, moment, P1));
    assert_eq!(
        runner.state().pending_combat_phase_skips.len(),
        1,
        "reach guard: the skip armed"
    );

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("concede");

    let prompts = pass_team_turn_to(&mut runner, P1, Phase::PostCombatMain);
    assert_eq!(prompts, 1, "P1 is asked to attack");
    assert!(runner.state().pending_combat_phase_skips.is_empty());
}
