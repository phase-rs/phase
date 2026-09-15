//! Issue #6902 — Sneak Attack activated twice in one turn must sacrifice BOTH
//! creatures at the beginning of the next end step.
//!
//! > {R}: You may put a creature card from your hand onto the battlefield. That
//! > creature gains haste. Sacrifice the creature at the beginning of the next
//! > end step.
//!
//! Reported (and reproduced on v0.81.2) as two delayed sacrifice triggers going
//! on the stack where the second carried an empty target list, stranding one
//! creature on the battlefield.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall, 2026-09-15).
const SNEAK_ATTACK: &str = "{R}: You may put a creature card from your hand onto the battlefield. That creature gains haste. Sacrifice the creature at the beginning of the next end step.";

/// Drive one activation to completion on the real action path, choosing `pick`
/// from hand. Every prompt seen is recorded; an unexpected one fails loudly.
fn activate_putting(runner: &mut GameRunner, sneak_attack: ObjectId, pick: ObjectId) {
    resolve_putting(
        runner,
        GameAction::ActivateAbility {
            source_id: sneak_attack,
            ability_index: 0,
        },
        pick,
    );
}

/// Submit `first` (an activation or a cast) and drive it to completion on the
/// real action path, choosing `pick` from hand. An unexpected prompt fails loudly.
fn resolve_putting(runner: &mut GameRunner, first: GameAction, pick: ObjectId) {
    let mut waiting = runner
        .act(first)
        .expect("the put-from-hand instruction must be accepted")
        .waiting_for;
    let mut seen = Vec::new();
    for _ in 0..20 {
        seen.push(format!("{waiting:?}").chars().take(50).collect::<String>());
        let action = match &waiting {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::EffectZoneChoice { cards, .. }
            | WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&pick),
                    "the chosen creature must be offered; offered {cards:?}"
                );
                GameAction::SelectCards { cards: vec![pick] }
            }
            other => {
                panic!("unexpected prompt during activation: {other:?}; prompts so far: {seen:?}")
            }
        };
        waiting = runner
            .act(action)
            .unwrap_or_else(|e| panic!("action rejected: {e:?}; prompts so far: {seen:?}"))
            .waiting_for;
    }
    panic!("activation did not settle; prompts: {seen:?}");
}

/// Pass priority on the real action path until `player`'s end step has begun.
fn pass_priority_into_end_step_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..60 {
        let state = runner.state();
        if state.active_player == player && state.phase == Phase::End {
            return;
        }
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            // The creatures have haste; this test does not attack.
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            other => panic!("unexpected prompt while advancing: {other:?}"),
        }
    }
    panic!("did not reach {player:?}'s end step");
}

#[test]
fn sneak_attack_twice_sacrifices_both_creatures_at_the_next_end_step() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);

    let sneak_attack = scenario
        .add_creature(P0, "Sneak Attack", 0, 0)
        .as_enchantment()
        .from_oracle_text(SNEAK_ATTACK)
        .id();
    let first = scenario
        .add_creature_to_hand(P0, "First Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Second Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, sneak_attack, false, Vec::new()),
            ManaUnit::new(ManaType::Red, sneak_attack, false, Vec::new()),
        ],
    );
    let mut runner = scenario.build();
    activate_putting(&mut runner, sneak_attack, first);
    activate_putting(&mut runner, sneak_attack, second);

    // Reach-guards: both creatures entered, and each activation installed its
    // own delayed sacrifice trigger.
    for bear in [first, second] {
        assert_eq!(
            runner.state().objects[&bear].zone,
            Zone::Battlefield,
            "reach-guard: {bear:?} entered"
        );
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        2,
        "reach-guard: one delayed trigger per activation"
    );

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    for bear in [first, second] {
        assert_eq!(
            runner.state().objects[&bear].zone,
            Zone::Graveyard,
            "{bear:?} must be sacrificed at the beginning of the next end step (CR 603.7)"
        );
    }
}

// Verbatim Oracle text (Scryfall, 2026-09-15).
const THROUGH_THE_BREACH: &str = "You may put a creature card from your hand onto the battlefield. That creature gains haste. Sacrifice that creature at the beginning of the next end step.\nSplice onto Arcane {2}{R}{R}";

/// CR 603.7 + CR 608.2c: the same forwarding gap reached from a SPELL with the
/// "that creature" phrasing. Two creature cards in hand force the choice prompt
/// that used to drop the referent; the one not chosen stays in hand.
#[test]
fn through_the_breach_sacrifices_the_chosen_creature_after_a_choice_prompt() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);

    let breach = scenario
        .add_spell_to_hand_from_oracle(P0, "Through the Breach", true, THROUGH_THE_BREACH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let chosen = scenario
        .add_creature_to_hand(P0, "Chosen Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let left_behind = scenario
        .add_creature_to_hand(P0, "Other Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Red, breach, false, Vec::new()))
            .collect(),
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&breach].card_id;

    resolve_putting(
        &mut runner,
        GameAction::CastSpell {
            object_id: breach,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        },
        chosen,
    );
    assert_eq!(
        runner.state().objects[&chosen].zone,
        Zone::Battlefield,
        "reach-guard: the chosen creature entered"
    );
    assert_eq!(
        runner.state().objects[&left_behind].zone,
        Zone::Hand,
        "reach-guard: the other creature stayed in hand"
    );

    pass_priority_into_end_step_of(&mut runner, P0);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&chosen].zone,
        Zone::Graveyard,
        "the creature chosen through the prompt must be sacrificed at the next end step"
    );
    assert_eq!(
        runner.state().objects[&left_behind].zone,
        Zone::Hand,
        "the unchosen creature is untouched"
    );
}
