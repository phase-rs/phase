use engine::ai_support::flat_priority_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use rand::rngs::SmallRng;
use rand::SeedableRng;

use super::{find_lethal_line, lethal_priority_action, lethal_prompt_action, LineStep};
use crate::config::{create_config, AiDifficulty, Platform};

const SEAL_OF_FIRE: &str = "Sacrifice this enchantment: It deals 2 damage to any target.";
const FLAME_RIFT: &str = "Flame Rift deals 4 damage to each player.";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const LAVA_SPIKE: &str = "Lava Spike deals 3 damage to target player or planeswalker.";
const BLAZE: &str = "Blaze deals X damage to any target.";
const SOULS_FIRE: &str =
    "Target creature you control deals damage equal to its power to any target.";
const BUMP_IN_THE_NIGHT: &str = "Target opponent loses 3 life.\n\
    Flashback {5}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const GUTTERSNIPE: &str =
    "Whenever you cast an instant or sorcery spell, this creature deals 2 damage to each opponent.";
const BOROS_CHARM: &str = "Choose one —\n\
    • Boros Charm deals 4 damage to target player or planeswalker.\n\
    • Permanents you control gain indestructible until end of turn.\n\
    • Target creature gains double strike until end of turn.";

fn red(generic: u32, red_pips: usize) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red; red_pips],
        generic,
    }
}

/// A two-player game in the AI's precombat main phase with `mountains`
/// untapped Mountains and the opponent at `opponent_life`.
fn scenario(mountains: usize, opponent_life: i32) -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, opponent_life);
    for _ in 0..mountains {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    scenario
}

fn seal_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Seal of Fire", false)
        .as_enchantment()
        .with_mana_cost(red(0, 1))
        .from_oracle_text(SEAL_OF_FIRE)
        .id()
}

fn seal_on_battlefield(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Seal of Fire", SEAL_OF_FIRE)
        .with_mana_cost(red(0, 1))
        .id()
}

fn flame_rift(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Flame Rift", false, FLAME_RIFT)
        .with_mana_cost(red(1, 1))
        .id()
}

fn lightning_bolt(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
        .with_mana_cost(red(0, 1))
        .id()
}

fn lava_spike(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Lava Spike", false, LAVA_SPIKE)
        .with_mana_cost(red(0, 1))
        .id()
}

/// The admission rule the real decision boundary applies (targeted-exchange
/// gate plus the AI loop guards).
fn production_admission(state: &GameState, action: &GameAction) -> bool {
    crate::search::root_action_is_admitted(state, P0, action)
}

fn admitted_actions(state: &GameState) -> Vec<GameAction> {
    flat_priority_actions(state)
        .into_iter()
        .filter(|action| production_admission(state, action))
        .collect()
}

fn cast_spell(spell: ObjectId, state: &GameState) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: state.objects[&spell].card_id,
        targets: Vec::new(),
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    }
}

/// Cast `spell` through the reducer with the opponent as its target, leaving
/// it (and anything it triggers) on the stack.
fn cast_at_opponent(runner: &mut GameRunner, spell: ObjectId) {
    let cast = cast_spell(spell, runner.state());
    runner.act(cast).expect("the spell is castable");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("the opponent is a legal target");
}

fn blaze(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Blaze", false, BLAZE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        })
        .id()
}

/// Answer the AI's own cast prompts through the reducer: announce `x` and aim
/// each target slot at the next of `targets`, until the spell is on the stack.
fn finish_cast(runner: &mut GameRunner, x: u32, targets: &[TargetRef]) {
    let mut targets = targets.iter();
    for _ in 0..8 {
        let answer = match runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: x },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(targets.next().expect("a target per slot").clone()),
            },
            _ => return,
        };
        runner
            .act(answer)
            .expect("the cast prompt accepts the answer");
    }
}

fn line(state: &GameState) -> Option<Vec<LineStep>> {
    let issued = flat_priority_actions(state);
    find_lethal_line(state, P0, &issued, &|_, _| true).map(|line| line.steps)
}

fn casts(steps: &[LineStep]) -> Vec<ObjectId> {
    steps
        .iter()
        .filter_map(|step| match step.action {
            GameAction::CastSpell { object_id, .. } => Some(object_id),
            _ => None,
        })
        .collect()
}

/// Let the AI play its turn against an opponent who only ever passes, and
/// report whether the AI won before the turn ended.
fn ai_wins_this_turn(runner: &mut GameRunner, difficulty: AiDifficulty) -> bool {
    let config = create_config(difficulty, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let turn = runner.state().turn_number;
    for _ in 0..80 {
        let state = runner.state();
        if let WaitingFor::GameOver { winner } = state.waiting_for {
            return winner == Some(P0);
        }
        if state.turn_number != turn {
            return false;
        }
        let action = if state.waiting_for.acting_player() == Some(P0) {
            crate::search::choose_action(state, P0, &config, &mut rng)
                .expect("the AI owes this decision")
        } else {
            GameAction::PassPriority
        };
        runner.act(action).expect("the chosen action applies");
    }
    false
}

/// The reported defect: two Seals look cheaper one at a time, but only
/// Seal + Flame Rift reaches 6.
#[test]
fn seal_seal_rift_takes_the_lethal_pair_not_the_cheap_pair() {
    let mut scenario = scenario(3, 6);
    let seal_a = seal_in_hand(&mut scenario);
    let seal_b = seal_in_hand(&mut scenario);
    let rift = flame_rift(&mut scenario);
    let runner = scenario.build();

    let steps = line(runner.state()).expect("Seal + Flame Rift is lethal with three lands");
    let cast = casts(&steps);
    assert!(
        cast.contains(&rift),
        "the line must cast Flame Rift: {steps:?}"
    );
    assert!(
        !(cast.contains(&seal_a) && cast.contains(&seal_b)),
        "two Seals deal 4 and cost the mana Flame Rift needs: {steps:?}"
    );
}

#[test]
fn seal_seal_rift_is_played_out_to_a_win() {
    let mut scenario = scenario(3, 6);
    seal_in_hand(&mut scenario);
    seal_in_hand(&mut scenario);
    flame_rift(&mut scenario);
    let mut runner = scenario.build();

    assert!(
        ai_wins_this_turn(&mut runner, AiDifficulty::Medium),
        "the AI must burn the opponent out this turn"
    );
}

/// A Seal already on the battlefield is damage that costs no mana: it is
/// folded into the total before any new spell is bought.
#[test]
fn battlefield_seal_is_free_reach() {
    let mut scenario = scenario(1, 5);
    let seal = seal_on_battlefield(&mut scenario);
    let bolt = lightning_bolt(&mut scenario);
    let mut runner = scenario.build();

    let steps = line(runner.state()).expect("Bolt (3) + the free Seal (2) is lethal");
    assert_eq!(casts(&steps), vec![bolt]);
    assert!(
        steps.iter().any(|step| matches!(
            step.action,
            GameAction::ActivateAbility { source_id, .. } if source_id == seal
        )),
        "the line must sacrifice the Seal already in play: {steps:?}"
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

#[test]
fn battlefield_seal_alone_finishes_without_mana() {
    let mut scenario = scenario(0, 2);
    let seal = seal_on_battlefield(&mut scenario);
    let runner = scenario.build();

    let issued = flat_priority_actions(runner.state());
    let action = lethal_priority_action(runner.state(), P0, &issued, &|_, _| true);
    assert!(
        matches!(action, Some(GameAction::ActivateAbility { source_id, .. }) if source_id == seal),
        "the free Seal is the whole line: {action:?}"
    );
}

#[test]
fn no_line_when_reach_falls_short() {
    let mut scenario = scenario(1, 5);
    lightning_bolt(&mut scenario);
    let runner = scenario.build();

    assert!(line(runner.state()).is_none());
}

/// CR 104.4a: Flame Rift taking both players to 0 is a draw, not a win.
#[test]
fn mutual_destruction_is_not_lethal() {
    let mut scenario = scenario(2, 4);
    scenario.with_life(P0, 4);
    flame_rift(&mut scenario);
    let runner = scenario.build();

    assert!(line(runner.state()).is_none());
}

/// CR 107.3a: an X burn spell is cast last and sized to the mana the rest of
/// the line leaves over.
#[test]
fn x_burn_spell_absorbs_the_leftover_mana() {
    let mut scenario = scenario(5, 6);
    let bolt = lightning_bolt(&mut scenario);
    let blaze = blaze(&mut scenario);
    let mut runner = scenario.build();

    let steps = line(runner.state()).expect("Bolt (3) + Blaze for X=3 is lethal");
    assert_eq!(casts(&steps), vec![bolt, blaze]);
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// CR 702.11c: a player with hexproof can't be targeted by a burn spell, but
/// non-targeted damage still reaches them.
#[test]
fn player_hexproof_leaves_only_untargeted_reach() {
    let mut scenario = scenario(3, 4);
    scenario.add_enchantment_from_oracle(P1, "Leyline of Sanctity", "You have hexproof.");
    let bolt = lightning_bolt(&mut scenario);
    let rift = flame_rift(&mut scenario);
    let runner = scenario.build();

    let steps = line(runner.state()).expect("Flame Rift needs no target");
    let cast = casts(&steps);
    assert_eq!(cast, vec![rift]);
    assert!(!cast.contains(&bolt));
}

/// A burn spell that is not lethal on its own still goes to the face when the
/// rest of the line finishes the job.
#[test]
fn line_member_targets_the_opponent_over_a_creature() {
    let mut scenario = scenario(1, 5);
    seal_on_battlefield(&mut scenario);
    let bolt = lightning_bolt(&mut scenario);
    scenario.add_creature(P1, "Goblin Guide", 2, 2);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: Vec::new(),
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Bolt is castable");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    let issued: Vec<GameAction> = engine::ai_support::build_decision_context(runner.state())
        .candidates
        .into_iter()
        .map(|candidate| candidate.action)
        .collect();
    let answer = lethal_prompt_action(runner.state(), P0, &issued, &|_, _| true);
    assert_eq!(
        answer,
        Some(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
    );
}

#[test]
fn sorcery_burn_pair_is_played_out_to_a_win() {
    let mut scenario = scenario(2, 6);
    lava_spike(&mut scenario);
    lava_spike(&mut scenario);
    scenario.add_creature(P1, "Goblin Guide", 2, 2);
    let mut runner = scenario.build();

    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// CR 700.2a: a modal burn spell is cast for its damaging mode.
#[test]
fn modal_burn_spell_is_cast_for_its_damage_mode() {
    let mut scenario = scenario(1, 4);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Boros Charm", true, BOROS_CHARM)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::White],
            generic: 0,
        });
    scenario.add_creature(P1, "Goblin Guide", 2, 2);
    let mut runner = scenario.build();

    let steps = line(runner.state()).expect("the 4-damage mode is lethal");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].mode, Some(0), "the damage mode is the first mode");
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// CR 602.2b: a mana-only activated ability is repeatable for as long as the
/// mana lasts, so two activations count toward the total.
#[test]
fn repeatable_activation_is_counted_per_activation() {
    let mut scenario = scenario(4, 2);
    let rod = scenario
        .add_artifact_from_oracle(
            P0,
            "Ember Rod",
            "{1}{R}: Ember Rod deals 1 damage to any target.",
        )
        .id();
    let mut runner = scenario.build();

    let steps = line(runner.state()).expect("two activations deal 2");
    assert_eq!(steps.len(), 2);
    assert!(steps.iter().all(|step| matches!(
        step.action,
        GameAction::ActivateAbility { source_id, .. } if source_id == rod
    )));
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// The line obeys the same admission rule as the real decision boundary at
/// every step it simulates: after two Lava Spikes this turn the third cast is
/// admitted and the fourth is refused (the search's same-card cast cap), so two
/// more Spikes against an opponent at 6 are not a lethal line.
#[test]
fn line_respects_the_same_card_cast_cap_at_every_step() {
    let mut scenario = scenario(4, 12);
    let spikes: Vec<ObjectId> = (0..4).map(|_| lava_spike(&mut scenario)).collect();
    let mut runner = scenario.build();
    for &spike in &spikes[..2] {
        cast_at_opponent(&mut runner, spike);
        runner.advance_until_stack_empty();
    }
    assert_eq!(
        runner.life(P1),
        6,
        "two Spikes resolved through the reducer"
    );

    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    assert!(
        admitted.contains(&cast_spell(spikes[2], &state))
            && admitted.contains(&cast_spell(spikes[3], &state)),
        "both remaining Spikes are admitted now: only two have been cast"
    );
    // Reach guard: without the admission rule, the two remaining Spikes would
    // certify — the certificate depends on the fourth cast.
    let permissive = flat_priority_actions(&state);
    assert!(find_lethal_line(&state, P0, &permissive, &|_, _| true).is_some());
    assert!(find_lethal_line(&state, P0, &admitted, &production_admission).is_none());
    assert_eq!(
        lethal_priority_action(&state, P0, &admitted, &production_admission),
        None
    );

    // The reducer agrees: the third cast is admitted, the fourth is not.
    let mut third = GameRunner::from_state(state.clone());
    cast_at_opponent(&mut third, spikes[2]);
    third.advance_until_stack_empty();
    assert!(flat_priority_actions(third.state()).contains(&cast_spell(spikes[3], third.state())));
    assert!(!production_admission(
        third.state(),
        &cast_spell(spikes[3], third.state())
    ));

    // And the chooser, playing the turn out, cannot burn the opponent out.
    let mut played = GameRunner::from_state(state);
    assert!(!ai_wins_this_turn(&mut played, AiDifficulty::Medium));
}

/// A Lightning Bolt cast with Guttersnipe in play, both still on the stack:
/// the Bolt and the Guttersnipe trigger above it are pending, and a second
/// Bolt is in hand with one Mountain to cast it.
fn bolt_and_guttersnipe_trigger_on_stack(opponent_life: i32) -> GameRunner {
    let mut scenario = scenario(2, opponent_life);
    // Summoning sick, so the line is about burn alone: Guttersnipe can't add
    // combat damage this turn (CR 508.1a).
    let guttersnipe = scenario
        .add_creature_from_oracle(P0, "Guttersnipe", 2, 2, GUTTERSNIPE)
        .with_mana_cost(red(2, 1))
        .with_summoning_sickness()
        .id();
    let first = lightning_bolt(&mut scenario);
    lightning_bolt(&mut scenario);
    let mut runner = scenario.build();
    cast_at_opponent(&mut runner, first);

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the AI holds priority with its Bolt on the stack"
    );
    assert!(
        state
            .stack
            .iter()
            .any(|entry| entry.source_id == guttersnipe
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
        "Guttersnipe's cast trigger is on the stack: {:?}",
        state.stack
    );
    runner
}

/// Pending stack work counts toward reach even when no card the AI holds
/// defines it: Guttersnipe's trigger (2) + the Bolt it rides on (3) + the
/// second Bolt (3) is 8 against 7 — before its own new trigger.
#[test]
fn pending_trigger_on_the_stack_counts_toward_lethal() {
    let mut runner = bolt_and_guttersnipe_trigger_on_stack(7);
    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    assert!(
        find_lethal_line(&state, P0, &admitted, &production_admission).is_some(),
        "the stack-aware line must not be pruned before it settles"
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// Everything available — both Bolts and both Guttersnipe triggers — is 10,
/// short of 11.
#[test]
fn pending_trigger_does_not_invent_lethal() {
    let mut runner = bolt_and_guttersnipe_trigger_on_stack(11);
    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    assert!(find_lethal_line(&state, P0, &admitted, &production_admission).is_none());
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// CR 107.3a: the X of a spell already on the stack was announced and is
/// locked. Blaze cast for X=2 at the opponent (5) leaves one Mountain, which
/// casts Lava Spike for the last 3 once Blaze resolves — the settled position
/// reflects the announced 2, not an X sized to the one Mountain still untapped.
#[test]
fn pending_x_spell_counts_its_announced_x() {
    let mut scenario = scenario(4, 5);
    let blaze = blaze(&mut scenario);
    let spike = lava_spike(&mut scenario);
    let mut runner = scenario.build();
    let cast = cast_spell(blaze, runner.state());
    runner.act(cast).expect("Blaze is castable");
    finish_cast(&mut runner, 2, &[TargetRef::Player(P1)]);

    let state = runner.state().clone();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(
        state
            .stack
            .iter()
            .find(|entry| entry.source_id == blaze)
            .and_then(|entry| entry.ability())
            .and_then(|ability| ability.chosen_x),
        Some(2),
        "Blaze is on the stack with X=2 locked"
    );
    assert_eq!(crate::zone_eval::available_mana(&state, P0), 1);

    let admitted = admitted_actions(&state);
    assert!(
        !admitted.contains(&cast_spell(spike, &state)),
        "Lava Spike waits for an empty stack (CR 117.1a)"
    );
    let line = find_lethal_line(&state, P0, &admitted, &production_admission)
        .expect("Blaze's locked 2 plus Lava Spike's 3 is lethal");
    assert_eq!(casts(&line.steps), vec![spike]);
    assert_eq!(
        lethal_priority_action(&state, P0, &admitted, &production_admission),
        Some(GameAction::PassPriority),
        "the line lets Blaze resolve before casting the sorcery"
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// CR 120.7: Soul's Fire on the stack has the targeted creature as its damage
/// source, which this pricing does not read — so the line must not drop it.
/// A 4-power creature's Soul's Fire at the opponent (7) plus Lightning Bolt is
/// lethal.
#[test]
fn pending_source_override_damage_is_not_dropped() {
    let mut scenario = scenario(4, 7);
    let giant = scenario.add_creature(P0, "Hill Giant", 4, 4).id();
    let fire = scenario
        .add_spell_to_hand_from_oracle(P0, "Soul's Fire", true, SOULS_FIRE)
        .with_mana_cost(red(2, 1))
        .id();
    lightning_bolt(&mut scenario);
    let mut runner = scenario.build();
    let cast = cast_spell(fire, runner.state());
    runner.act(cast).expect("Soul's Fire is castable");
    finish_cast(
        &mut runner,
        0,
        &[TargetRef::Object(giant), TargetRef::Player(P1)],
    );

    let state = runner.state().clone();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(state.stack.iter().any(|entry| entry.source_id == fire));
    // Discriminating guard: the pending override damage, whose amount this
    // pricing does not read, still reaches the opponent.
    let pending = state
        .stack
        .iter()
        .find(|entry| entry.source_id == fire)
        .and_then(|entry| entry.ability())
        .expect("Soul's Fire carries its resolved instructions");
    assert!(super::sources::resolved_reaches(&state, P0, P1, pending));
    let admitted = admitted_actions(&state);
    assert!(find_lethal_line(&state, P0, &admitted, &production_admission).is_some());
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// Lightning Bolt aimed at the opponent on the stack, Bump in the Night in the
/// graveyard, and six untapped Mountains for its flashback cost.
fn bolt_on_stack_with_flashback_bump(opponent_life: i32) -> (GameRunner, ObjectId) {
    let mut scenario = scenario(7, opponent_life);
    let bump = scenario
        .add_spell_to_graveyard(P0, "Bump in the Night", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .from_oracle_text(BUMP_IN_THE_NIGHT)
        .id();
    let bolt = lightning_bolt(&mut scenario);
    let mut runner = scenario.build();
    cast_at_opponent(&mut runner, bolt);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(crate::zone_eval::available_mana(runner.state(), P0), 6);
    (runner, bump)
}

/// CR 702.34a: a sorcery castable by flashback counts toward reach while the
/// stack still holds the line's first spell — it becomes castable once Bolt
/// resolves, and its 3 life loss finishes the opponent at 6.
#[test]
fn flashback_burn_counts_once_the_stack_settles() {
    let (mut runner, bump) = bolt_on_stack_with_flashback_bump(6);
    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    assert!(
        !admitted.contains(&cast_spell(bump, &state)),
        "a sorcery is not castable while Bolt is on the stack"
    );

    // Reach guard: once the stack settles, flashback issues the cast.
    let mut settled = GameRunner::from_state(state.clone());
    settled.advance_until_stack_empty();
    assert_eq!(settled.life(P1), 3);
    assert!(admitted_actions(settled.state()).contains(&cast_spell(bump, settled.state())));

    let line = find_lethal_line(&state, P0, &admitted, &production_admission)
        .expect("Bolt's 3 plus flashback Bump's 3 is lethal");
    assert_eq!(casts(&line.steps), vec![bump]);
    assert_eq!(
        lethal_priority_action(&state, P0, &admitted, &production_admission),
        Some(GameAction::PassPriority)
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// Bolt's 3 and Bump's 3 are 6, short of 7.
#[test]
fn flashback_burn_does_not_invent_lethal() {
    let (mut runner, _) = bolt_on_stack_with_flashback_bump(7);
    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    assert!(find_lethal_line(&state, P0, &admitted, &production_admission).is_none());
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// `VeryEasy` does not plan multi-action plays.
#[test]
fn very_easy_does_not_commit_to_lines() {
    let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
    assert!(!config.play_lookahead);
}

// ── Settlement is the authority over pending work (CR 608.2h) ──

const GEISTFLAME: &str = "Geistflame deals 1 damage to any target.\n\
    Flashback {3}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const GOBLIN_WAR_STRIKE: &str = "Goblin War Strike deals damage to target player or planeswalker equal to the number of Goblins you control.";
const KRENKO_MOB_BOSS: &str =
    "{T}: Create X 1/1 red Goblin creature tokens, where X is the number of Goblins you control.";
const CULMINATION_OF_STUDIES: &str = "Exile the top X cards of your library. For each land card exiled this way, create a Treasure token. For each blue card exiled this way, draw a card. For each red card exiled this way, Culmination of Studies deals 1 damage to each opponent.";
const LIGHTNING_HELIX: &str = "Lightning Helix deals 3 damage to any target and you gain 3 life.";
const EARTHQUAKE: &str =
    "Earthquake deals X damage to each creature without flying and each player.";
const HYMN_TO_TOURACH: &str = "Target player discards two cards at random.";
const OBSTINATE_BALOTH: &str = "When this creature enters, you gain 4 life.\n\
    If a spell or ability an opponent controls causes you to discard this card, put it onto the battlefield instead of putting it into your graveyard.";

fn pending_line_is_found(state: &GameState) -> bool {
    let admitted = admitted_actions(state);
    find_lethal_line(state, P0, &admitted, &production_admission).is_some()
}

/// A normally cast Geistflame aimed at the opponent on the stack, Lightning
/// Bolt in hand, and five untapped Mountains: once Geistflame resolves into the
/// graveyard (CR 608.2n) it can be cast again by flashback (CR 702.34a), so
/// 1 + 3 + 1 is reachable from a position whose pending work reads 1.
fn geistflame_on_stack_with_bolt(opponent_life: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = scenario(6, opponent_life);
    let geistflame = scenario
        .add_spell_to_hand(P0, "Geistflame", true)
        .with_mana_cost(red(0, 1))
        .from_oracle_text(GEISTFLAME)
        .id();
    let bolt = lightning_bolt(&mut scenario);
    let mut runner = scenario.build();
    cast_at_opponent(&mut runner, geistflame);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(crate::zone_eval::available_mana(runner.state(), P0), 5);
    (runner, geistflame, bolt)
}

#[test]
fn a_resolving_spell_recast_by_flashback_is_not_pruned_before_settlement() {
    let (mut runner, geistflame, bolt) = geistflame_on_stack_with_bolt(5);
    let state = runner.state().clone();

    // Reach guard: once the stack settles, flashback issues Geistflame's cast.
    let mut settled = GameRunner::from_state(state.clone());
    settled.advance_until_stack_empty();
    assert_eq!(settled.life(P1), 4);
    assert!(admitted_actions(settled.state()).contains(&cast_spell(geistflame, settled.state())));

    let admitted = admitted_actions(&state);
    let line = find_lethal_line(&state, P0, &admitted, &production_admission)
        .expect("Geistflame's 1, Bolt's 3 and Geistflame's flashback 1 are lethal");
    let mut cast = casts(&line.steps);
    cast.sort();
    let mut expected = vec![geistflame, bolt];
    expected.sort();
    assert_eq!(cast, expected);
    assert!(lethal_priority_action(&state, P0, &admitted, &production_admission).is_some());
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

#[test]
fn a_flashback_recast_does_not_invent_lethal() {
    let (mut runner, ..) = geistflame_on_stack_with_bolt(6);
    assert!(!pending_line_is_found(runner.state()));
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// Goblin War Strike on the stack below a Krenko activation: two Goblins are
/// on the battlefield as Strike is priced, Krenko makes two more first, and
/// CR 608.2h has Strike count four as it resolves.
fn strike_below_krenko(opponent_life: i32) -> GameRunner {
    let mut scenario = scenario(1, opponent_life);
    let krenko = scenario
        .add_creature_from_oracle(P0, "Krenko, Mob Boss", 3, 3, KRENKO_MOB_BOSS)
        .with_subtypes(vec!["Goblin", "Warrior"])
        .id();
    // Summoning sick, so the line is about burn alone (CR 302.6).
    scenario
        .add_creature(P0, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin", "Warrior"])
        .with_summoning_sickness();
    let strike = scenario
        .add_spell_to_hand_from_oracle(P0, "Goblin War Strike", false, GOBLIN_WAR_STRIKE)
        .with_mana_cost(red(0, 1))
        .id();
    let mut runner = scenario.build();
    cast_at_opponent(&mut runner, strike);
    runner
        .act(GameAction::ActivateAbility {
            source_id: krenko,
            ability_index: 0,
        })
        .expect("Krenko's ability is activatable");
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(
        state.stack.len(),
        2,
        "Strike and Krenko's ability are pending"
    );
    runner
}

#[test]
fn a_pending_quantity_is_read_as_it_resolves_not_as_priced() {
    let mut runner = strike_below_krenko(4);
    assert!(
        pending_line_is_found(runner.state()),
        "the four Goblins Strike counts as it resolves are lethal"
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

#[test]
fn a_pending_quantity_does_not_invent_lethal() {
    let mut runner = strike_below_krenko(5);
    assert!(!pending_line_is_found(runner.state()));
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// Culmination of Studies cast for X=3 over three red cards: its damage
/// instruction runs once per red card exiled (CR 608.2c), three times.
fn culmination_on_stack_over_three_red_cards(opponent_life: i32) -> GameRunner {
    let mut scenario = scenario(4, opponent_life);
    scenario.add_basic_land(P0, ManaColor::Blue);
    for name in ["Red Card A", "Red Card B", "Red Card C"] {
        scenario
            .add_spell_to_library_top(P0, name, true)
            .with_color(vec![ManaColor::Red]);
    }
    let culmination = scenario
        .add_spell_to_hand_from_oracle(P0, "Culmination of Studies", false, CULMINATION_OF_STUDIES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let cast = cast_spell(culmination, runner.state());
    runner.act(cast).expect("Culmination is castable");
    finish_cast(&mut runner, 3, &[]);
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(state
        .stack
        .iter()
        .any(|entry| entry.source_id == culmination));
    runner
}

#[test]
fn pending_repetition_counts_every_run() {
    let mut runner = culmination_on_stack_over_three_red_cards(3);
    assert!(
        pending_line_is_found(runner.state()),
        "three red cards exiled deal three damage"
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

#[test]
fn pending_repetition_does_not_invent_lethal() {
    let mut runner = culmination_on_stack_over_three_red_cards(4);
    assert!(!pending_line_is_found(runner.state()));
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

// ── Life gained along the line (CR 119.3) ──

/// Flame Rift and Lightning Helix with the AI at `ai_life` and the opponent at
/// 7: the sorcery is cast first, so the instant cast on top of it resolves
/// first (CR 117.4) — Helix's 3 life lands before Rift's 4 damage.
fn helix_and_rift(ai_life: i32) -> GameRunner {
    let mut scenario = scenario(3, 7);
    scenario.with_life(P0, ai_life);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Helix", true, LIGHTNING_HELIX)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::White],
            generic: 0,
        });
    flame_rift(&mut scenario);
    scenario.build()
}

#[test]
fn life_gained_before_symmetric_damage_keeps_the_ai_alive() {
    let mut runner = helix_and_rift(4);
    let steps = line(runner.state()).expect("Helix gains 3 before Rift's 4 lands");
    assert_eq!(casts(&steps).len(), 2);
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

#[test]
fn life_gained_that_cannot_cover_the_loss_is_not_lethal() {
    // 1 + 3 gained − 4 taken is 0: a draw at best (CR 104.4a).
    let mut runner = helix_and_rift(1);
    assert!(line(runner.state()).is_none());
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

// ── A survivable X (CR 107.3a) ──

fn earthquake(ai_life: i32, opponent_life: i32) -> GameRunner {
    earthquake_with_mountains(6, ai_life, opponent_life)
}

fn earthquake_with_mountains(mountains: usize, ai_life: i32, opponent_life: i32) -> GameRunner {
    let mut scenario = scenario(mountains, opponent_life);
    scenario.with_life(P0, ai_life);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Earthquake", false, EARTHQUAKE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        });
    scenario.build()
}

/// Six Mountains could pay X=5, which would take both players to 0 or less;
/// X=2 takes the opponent from 2 to 0 and leaves the AI at 1.
#[test]
fn symmetric_x_damage_announces_the_least_lethal_x() {
    let mut runner = earthquake(3, 2);
    let steps = line(runner.state()).expect("Earthquake for X=2 is lethal and survivable");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].x, Some(2));
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
    assert_eq!(runner.life(P0), 1);
}

#[test]
fn symmetric_x_damage_with_no_survivable_x_is_not_lethal() {
    let mut runner = earthquake(2, 2);
    assert!(line(runner.state()).is_none());
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

/// Cast the root line's Earthquake through the reducer and return the AI's
/// real ChooseX prompt with its engine-issued answers.
fn earthquake_x_prompt(root: &GameState, cast: &GameAction) -> (GameState, Vec<GameAction>) {
    let mut runner = GameRunner::from_state(root.clone());
    runner.act(cast.clone()).expect("Earthquake is castable");
    let state = runner.state().clone();
    assert!(
        matches!(state.waiting_for, WaitingFor::ChooseXValue { player, max: 14, .. } if player == P0),
        "fifteen Mountains announce X up to 14: {:?}",
        state.waiting_for
    );
    let issued = engine::ai_support::build_decision_context(&state)
        .candidates
        .into_iter()
        .map(|candidate| candidate.action)
        .collect();
    (state, issued)
}

/// CR 107.3a + CR 104.4a: with fifteen Mountains the X prompt offers 0–14.
/// X=14 takes both players out; only X=10 or 11 takes the opponent (10) out
/// and leaves the AI (12) alive — both beyond the first values a bounded scan
/// reaches. The prompt must announce the X the root certified.
#[test]
fn x_prompt_announces_the_certified_x_across_a_wide_range() {
    let mut runner = earthquake_with_mountains(15, 12, 10);
    let root = runner.state().clone();
    let steps = line(&root).expect("Earthquake for X=10 is lethal and survivable");
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0].x,
        Some(10),
        "the root certifies the least lethal X"
    );

    let (prompt, issued) = earthquake_x_prompt(&root, &steps[0].action);
    let offered: Vec<u32> = issued
        .iter()
        .filter_map(|action| match action {
            GameAction::ChooseX { value } => Some(*value),
            _ => None,
        })
        .collect();
    assert_eq!(
        offered,
        (0..=14).collect::<Vec<_>>(),
        "the engine issues the full range"
    );
    assert_eq!(
        lethal_prompt_action(&prompt, P0, &issued, &production_admission),
        Some(GameAction::ChooseX { value: 10 })
    );

    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
    assert_eq!(runner.life(P0), 2, "X=10 leaves the AI at 12 − 10");
}

/// The same wide range with the AI at 10: every X that takes the opponent out
/// takes the AI out with them, so no X wins.
#[test]
fn x_prompt_with_no_survivable_x_in_a_wide_range_declines() {
    let mut runner = earthquake_with_mountains(15, 10, 10);
    let root = runner.state().clone();
    assert!(line(&root).is_none());
    let quake = root.players[P0.0 as usize].hand[0];
    let (prompt, issued) = earthquake_x_prompt(&root, &cast_spell(quake, &root));
    assert_eq!(
        lethal_prompt_action(&prompt, P0, &issued, &production_admission),
        None
    );
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

// ── One certification per decision, inside the shared budget ──

fn certification_runs() -> u32 {
    crate::search::CERTIFICATION_RUNS.with(std::cell::Cell::get)
}

/// With determinization enabled (K=3) and reach the line search rejects
/// (Bolt for 3 against 5), the decision certifies at its ensemble boundary
/// only: the first sampled world declines, and no sampled world's scorer
/// re-runs certification — through the scored/parallel-worker entry and
/// through `choose_action` alike.
#[test]
fn certification_runs_once_per_decision_at_the_ensemble_boundary() {
    let mut scenario = scenario(1, 5);
    lightning_bolt(&mut scenario);
    let state = scenario.build().state().clone();
    let mut config = create_config(AiDifficulty::Medium, Platform::Native);
    config.search.determinization_samples = 3;
    config.search.time_budget_ms = None;

    let before = certification_runs();
    let scored = crate::search::score_candidates_for_parallel_worker(&state, P0, &config, None);
    assert!(!scored.is_empty(), "the sampled ensemble still scores");
    assert_eq!(certification_runs() - before, 1);

    let before = certification_runs();
    let mut rng = SmallRng::seed_from_u64(7);
    crate::search::choose_action(&state, P0, &config, &mut rng).expect("the AI acts");
    assert_eq!(certification_runs() - before, 1);
}

/// The decision's shared wall-clock ceiling is authoritative over
/// certification too: with that budget already spent, no sampled world is
/// certified (outside measurement mode, which runs without a wall clock).
#[test]
fn certification_spends_from_the_shared_ensemble_budget() {
    let mut scenario = scenario(1, 5);
    lightning_bolt(&mut scenario);
    let state = scenario.build().state().clone();
    let mut config = create_config(AiDifficulty::Medium, Platform::Native);
    config.search.determinization_samples = 3;
    config.search.time_budget_ms = Some(0);
    assert!(!config.execution_mode.is_measurement());

    let before = certification_runs();
    crate::search::score_candidates_for_parallel_worker(&state, P0, &config, None);
    assert_eq!(certification_runs() - before, 0);
}

// ── Certification stays inside the selected information model (CR 400.2) ──

/// Hymn to Tourach aimed at the opponent on the stack, Lava Spike in hand,
/// and the opponent at 3 holding exactly two cards — `hidden` and a filler —
/// against a registered decklist of vanilla cards. When the hidden card is
/// Obstinate Baloth, Hymn's discard puts it onto the battlefield instead
/// (CR 614.1a) and its controller gains 4 life — out of Spike's reach.
fn hymn_over_hidden_card(hidden_is_baloth: bool) -> GameState {
    use engine::game::deck_loading::DeckEntry;
    use engine::types::card::CardFace;
    use engine::types::game_state::PlayerDeckPool;
    use std::sync::Arc;

    let mut scenario = scenario(1, 3);
    scenario.with_life(P0, 5);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);
    let hymn = scenario
        .add_spell_to_hand_from_oracle(P0, "Hymn to Tourach", false, HYMN_TO_TOURACH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    lava_spike(&mut scenario);
    if hidden_is_baloth {
        scenario
            .add_creature_to_hand_from_oracle(P1, "Obstinate Baloth", 4, 4, OBSTINATE_BALOTH)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 2,
            });
    } else {
        scenario.add_spell_to_hand(P1, "Hidden Vanilla", false);
    }
    scenario.add_spell_to_hand(P1, "Opponent Filler", false);
    let mut runner = scenario.build();
    cast_at_opponent(&mut runner, hymn);
    let mut state = runner.state().clone();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    state.deck_pools.push(PlayerDeckPool {
        player: P1,
        current_main: Arc::new(vec![DeckEntry {
            card: CardFace {
                name: "Grizzly Bears".to_string(),
                mana_cost: ManaCost::zero(),
                ..Default::default()
            },
            count: 40,
        }]),
        ..Default::default()
    });
    state
}

fn certified_step(state: &GameState, samples: u32) -> Option<GameAction> {
    let mut config = create_config(AiDifficulty::Medium, Platform::Native);
    config.search.determinization_samples = samples;
    let actions: Vec<GameAction> = flat_priority_actions(state)
        .into_iter()
        .filter(|action| production_admission(state, action))
        .collect();
    crate::search::certified_lethal_priority_action(
        state,
        P0,
        &config,
        &actions,
        engine::util::Deadline::none(),
    )
}

#[test]
fn determinized_certification_does_not_read_the_real_hidden_hand() {
    let baloth = hymn_over_hidden_card(true);
    let vanilla = hymn_over_hidden_card(false);

    // Reach guard: with perfect information the hidden card decides the line —
    // a discarded Baloth enters and gains its controller 4 life.
    let mut settled = GameRunner::from_state(baloth.clone());
    settled.advance_until_stack_empty();
    assert_eq!(
        settled.life(P1),
        7,
        "Baloth entered instead of being discarded"
    );
    assert_eq!(certified_step(&vanilla, 0), Some(GameAction::PassPriority));
    assert_eq!(certified_step(&baloth, 0), None);

    // With K samples the opponent's unknown cards are resampled from their
    // decklist, so the real hidden identity cannot change the decision.
    assert_eq!(
        certified_step(&baloth, 3),
        certified_step(&vanilla, 3),
        "the K>0 decision reads sampled worlds, not the real hidden hand"
    );
    assert_eq!(certified_step(&baloth, 3), Some(GameAction::PassPriority));
}

// ── A certified line's response survives the cast that opens it (CR 117.3c) ──

const SPEAR_SPEWER: &str = "Defender\n{T}: This creature deals 1 damage to each player.";

/// The AI at 4 against an opponent at 5 with two Mountains, Flame Rift in
/// hand, and an untapped Spear Spewer. Rift alone takes the AI to 0 and the
/// opponent to 1; Spewer's 1 damage to each player is the 5th point, and its
/// lifelink (the effect of the Lifelink Aura, applied here as the keyword it
/// grants) gains the AI 2 life before Rift resolves — so the AI wins at 1.
/// Without lifelink the same line costs the AI 5 life and wins nothing.
fn rift_and_spewer(lifelink: bool) -> GameRunner {
    let mut scenario = scenario(2, 5);
    scenario.with_life(P0, 4);
    let mut spewer = scenario.add_creature_from_oracle(P0, "Spear Spewer", 0, 3, SPEAR_SPEWER);
    spewer.with_mana_cost(red(0, 1));
    if lifelink {
        spewer.lifelink();
    }
    flame_rift(&mut scenario);
    scenario.build()
}

/// Cast Flame Rift through the reducer so it sits on the stack and the AI
/// holds priority again, as the real cast leaves it (CR 117.3c).
fn with_rift_cast(mut runner: GameRunner) -> GameRunner {
    let rift = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == "Flame Rift")
        .expect("Flame Rift is in the scenario")
        .id;
    let cast = cast_spell(rift, runner.state());
    runner.act(cast).expect("Flame Rift is castable");
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(state.stack.len(), 1, "Flame Rift is pending");
    runner
}

#[test]
fn the_certified_response_is_found_again_after_its_opening_cast() {
    let runner = rift_and_spewer(true);
    // Root: Rift, then Spewer on top of it.
    let steps = line(runner.state()).expect("Rift + a lifelinked Spewer wins from the root");
    assert_eq!(steps.len(), 2);

    // The real post-cast priority: the pending Rift alone would lose the AI,
    // so settling it proves nothing — the response is what wins.
    let mut runner = with_rift_cast(runner);
    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    let next = lethal_priority_action(&state, P0, &admitted, &production_admission);
    assert!(
        matches!(next, Some(GameAction::ActivateAbility { .. })),
        "the certified next step is Spewer's activation, got {next:?}"
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
    assert_eq!(runner.life(P0), 1);
}

#[test]
fn a_response_that_cannot_save_the_ai_is_not_certified() {
    let runner = with_rift_cast(rift_and_spewer(false));
    let state = runner.state();
    let admitted = admitted_actions(state);
    assert!(find_lethal_line(state, P0, &admitted, &production_admission).is_none());
    let mut runner = runner;
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

// ── The X a line was certified with survives the stack below it (CR 107.3a) ──

const SQUALL_LINE: &str =
    "Squall Line deals X damage to each creature with flying and each player.";

/// Fourteen Forests and two Mountains, Lightning Bolt and Squall Line in hand,
/// the AI at 12 against `opponent_life`.
fn bolt_and_squall(opponent_life: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 12);
    scenario.with_life(P1, opponent_life);
    for _ in 0..14 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let bolt = lightning_bolt(&mut scenario);
    let squall = scenario
        .add_spell_to_hand_from_oracle(P0, "Squall Line", true, SQUALL_LINE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    (scenario.build(), bolt, squall)
}

/// Bolt cast at the opponent, then Squall Line started above it: the AI's real
/// ChooseX prompt with its engine-issued answers.
fn squall_x_prompt(runner: &mut GameRunner, bolt: ObjectId, squall: ObjectId) -> Vec<GameAction> {
    cast_at_opponent(runner, bolt);
    let cast = cast_spell(squall, runner.state());
    runner.act(cast).expect("Squall Line is castable");
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ChooseXValue { player, max: 13, .. } if player == P0),
        "fifteen lands left after Bolt announce X up to 13: {:?}",
        state.waiting_for
    );
    engine::ai_support::build_decision_context(state)
        .candidates
        .into_iter()
        .map(|candidate| candidate.action)
        .collect()
}

/// CR 117.4 + CR 104.3b: Squall Line resolves above Bolt. X=11 leaves the AI
/// at 1 and the opponent at 3 for Bolt to finish; the maximum, 13, kills the
/// AI before Bolt resolves, and every X a bounded scan from 0 reaches leaves
/// the opponent out of Bolt's range. The prompt must announce the certified 11.
#[test]
fn x_prompt_announces_the_certified_x_with_the_stack_still_below_it() {
    let (runner, bolt, squall) = bolt_and_squall(14);
    let root = line(runner.state()).expect("Bolt + Squall Line for X=11 wins from the root");
    assert_eq!(casts(&root).len(), 2);
    let squall_step = root
        .iter()
        .find(|step| matches!(step.action, GameAction::CastSpell { object_id, .. } if object_id == squall))
        .expect("Squall Line is in the line");
    assert_eq!(squall_step.x, Some(11));

    let mut runner = runner;
    let issued = squall_x_prompt(&mut runner, bolt, squall);
    let prompt = runner.state().clone();
    assert!(issued.contains(&GameAction::ChooseX { value: 11 }));
    assert_eq!(
        lethal_prompt_action(&prompt, P0, &issued, &production_admission),
        Some(GameAction::ChooseX { value: 11 })
    );

    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
    assert_eq!(runner.life(P0), 1, "X=11 leaves the AI at 12 − 11");
}

/// At 15 the opponent needs X=12 beside Bolt, which takes the AI (12) out
/// first: no line from the root, and the same prompt declines every X.
#[test]
fn x_prompt_with_no_certified_x_below_the_stack_declines() {
    let (runner, bolt, squall) = bolt_and_squall(15);
    assert!(line(runner.state()).is_none());
    let mut runner = runner;
    let issued = squall_x_prompt(&mut runner, bolt, squall);
    let prompt = runner.state().clone();
    assert_eq!(
        lethal_prompt_action(&prompt, P0, &issued, &production_admission),
        None
    );
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

// ── A response survives a settlement that destroys its source (CR 704.5g) ──

const FLAMEBREAK: &str = "Flamebreak deals 3 damage to each creature without flying and each player. Creatures dealt damage this way can't be regenerated this turn.";

/// The AI and the opponent at 4, three Mountains, Flamebreak in hand, and an
/// untapped 0/2 Spear Spewer — with lifelink (as the Lifelink Aura grants it)
/// when `lifelink` — then Flamebreak cast, so it is pending with the AI
/// holding priority again (CR 117.3c).
fn flamebreak_cast_over_spewer(lifelink: bool) -> (GameRunner, ObjectId) {
    let mut scenario = scenario(3, 4);
    scenario.with_life(P0, 4);
    let mut spewer = scenario.add_creature_from_oracle(P0, "Spear Spewer", 0, 2, SPEAR_SPEWER);
    spewer.with_mana_cost(red(0, 1));
    if lifelink {
        spewer.lifelink();
    }
    let spewer = spewer.id();
    let flamebreak = scenario
        .add_spell_to_hand_from_oracle(P0, "Flamebreak", false, FLAMEBREAK)
        .with_mana_cost(red(0, 3))
        .id();
    let mut runner = scenario.build();
    assert!(
        line(runner.state()).is_some() == lifelink,
        "Flamebreak, then Spewer above it, wins from the root only with lifelink"
    );
    let cast = cast_spell(flamebreak, runner.state());
    runner.act(cast).expect("Flamebreak is castable");
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(state.stack.len(), 1, "Flamebreak is pending");
    (runner, spewer)
}

/// Spewer's 1 damage to each player gains the AI 2 (CR 702.15b), so Flamebreak
/// then takes it from 5 to 2 and the opponent from 3 to 0. Settling Flamebreak
/// first instead leaves both players at 1 and destroys the 0/2 Spewer
/// (CR 704.5g), so the settled position holds no reach at all — the response
/// has to be certified from the pending position.
#[test]
fn a_response_whose_source_the_settlement_destroys_is_still_certified() {
    let (mut runner, spewer) = flamebreak_cast_over_spewer(true);

    let mut settled = GameRunner::from_state(runner.state().clone());
    settled.advance_until_stack_empty();
    assert_eq!((settled.life(P0), settled.life(P1)), (1, 1));
    assert!(
        !settled.state().battlefield.contains(&spewer),
        "Flamebreak destroyed the Spewer"
    );

    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    assert_eq!(
        lethal_priority_action(&state, P0, &admitted, &production_admission),
        Some(GameAction::ActivateAbility {
            source_id: spewer,
            ability_index: 0,
        })
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
    assert_eq!(runner.life(P0), 2);
}

/// Without lifelink the response costs the AI the life Flamebreak then takes:
/// both players reach 0 together (CR 104.4a), so nothing is certified.
#[test]
fn a_response_that_cannot_outlast_the_settlement_is_not_certified() {
    let (mut runner, _) = flamebreak_cast_over_spewer(false);
    let state = runner.state().clone();
    let admitted = admitted_actions(&state);
    assert!(find_lethal_line(&state, P0, &admitted, &production_admission).is_none());
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}

// ── A sampled certificate completes inside the shared deadline ──

/// K=2 sampled worlds that both certify the same action. When the final
/// world's certification completes after the shared deadline, the agreement is
/// not accepted; the same agreement inside the budget is.
#[test]
fn a_sampled_certificate_completed_past_the_deadline_is_rejected() {
    use std::cell::Cell;
    use std::time::Duration;

    let mut scenario = scenario(1, 5);
    lightning_bolt(&mut scenario);
    let state = scenario.build().state().clone();

    let agree = |deadline, final_decision_overruns: bool| {
        let calls = Cell::new(0);
        crate::search::agreed_across_samples(&state, P0, 2, deadline, |_| {
            calls.set(calls.get() + 1);
            if final_decision_overruns && calls.get() == 2 {
                std::thread::sleep(Duration::from_millis(60));
            }
            Some(GameAction::PassPriority)
        })
    };

    assert_eq!(
        agree(engine::util::Deadline::after(10_000), false),
        Some(GameAction::PassPriority),
        "in budget, the agreement is the decision"
    );
    assert_eq!(
        agree(engine::util::Deadline::after(30), true),
        None,
        "the final world began in budget and completed past it"
    );
}

// ── A certified response survives the cast's own target prompt (CR 117.3c) ──

const CHAR: &str = "Char deals 4 damage to any target and 2 damage to you.";

/// The AI at 2 against an opponent at 5 with three Mountains, Char in hand,
/// and an untapped 0/2 Spear Spewer — with lifelink (as the Lifelink Aura
/// grants it) when `lifelink` — then Char cast, so its target prompt is the
/// AI's real decision.
fn char_target_prompt_over_spewer(lifelink: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = scenario(3, 5);
    scenario.with_life(P0, 2);
    let mut spewer = scenario.add_creature_from_oracle(P0, "Spear Spewer", 0, 2, SPEAR_SPEWER);
    spewer.with_mana_cost(red(0, 1));
    if lifelink {
        spewer.lifelink();
    }
    let spewer = spewer.id();
    let char = scenario
        .add_spell_to_hand_from_oracle(P0, "Char", true, CHAR)
        .with_mana_cost(red(2, 1))
        .id();
    let mut runner = scenario.build();
    let root = line(runner.state());
    assert_eq!(
        root.is_some(),
        lifelink,
        "Char and Spewer win from the root only with lifelink"
    );
    if let Some(root) = root {
        assert_eq!(casts(&root), vec![char]);
        assert!(root.iter().any(|step| matches!(
            step.action,
            GameAction::ActivateAbility { source_id, .. } if source_id == spewer
        )));
    }
    let cast = cast_spell(char, runner.state());
    runner.act(cast).expect("Char is castable");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::TargetSelection { player, .. } if player == P0),
        "Char asks for its target: {:?}",
        runner.state().waiting_for
    );
    (runner, char, spewer)
}

fn issued_at_prompt(state: &GameState) -> Vec<GameAction> {
    engine::ai_support::build_decision_context(state)
        .candidates
        .into_iter()
        .map(|candidate| candidate.action)
        .collect()
}

/// Spewer above Char takes the AI from 2 to 3 (CR 702.15b) and the opponent to
/// 4, then Char takes them to 1 and 0. Settling Char alone first would take
/// the AI to 0 (CR 104.3b), so the target prompt must be judged from the
/// priority it returns to, where the Spewer response is still legal.
#[test]
fn a_certified_response_survives_the_casts_target_prompt() {
    let (mut runner, _, spewer) = char_target_prompt_over_spewer(true);
    let prompt = runner.state().clone();
    let aim = GameAction::ChooseTarget {
        target: Some(TargetRef::Player(P1)),
    };
    assert_eq!(
        lethal_prompt_action(
            &prompt,
            P0,
            &issued_at_prompt(&prompt),
            &production_admission
        ),
        Some(aim.clone()),
        "the target prompt keeps the certified line"
    );

    let mut settled = GameRunner::from_state(prompt);
    settled
        .act(aim.clone())
        .expect("the opponent is a legal target");
    settled.advance_until_stack_empty();
    assert!(
        matches!(
            settled.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P1) }
        ),
        "settling Char alone loses the AI"
    );

    runner.act(aim).expect("the opponent is a legal target");
    let state = runner.state().clone();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    let admitted = admitted_actions(&state);
    assert_eq!(
        lethal_priority_action(&state, P0, &admitted, &production_admission),
        Some(GameAction::ActivateAbility {
            source_id: spewer,
            ability_index: 0,
        })
    );
    assert!(ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
    assert_eq!(runner.life(P0), 1);
}

/// Without lifelink the Spewer only costs the AI a further point: no answer
/// to Char's prompt keeps the AI alive through it.
#[test]
fn a_target_prompt_with_no_surviving_response_declines() {
    let (mut runner, _, _) = char_target_prompt_over_spewer(false);
    let prompt = runner.state().clone();
    assert_eq!(
        lethal_prompt_action(
            &prompt,
            P0,
            &issued_at_prompt(&prompt),
            &production_admission
        ),
        None
    );
    assert!(!ai_wins_this_turn(&mut runner, AiDifficulty::Medium));
}
