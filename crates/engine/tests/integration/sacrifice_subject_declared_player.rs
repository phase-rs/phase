//! CR 608.2c + CR 701.21a: a "that player sacrifices ..." clause whose subject
//! names a player an earlier clause announced makes THAT player sacrifice a
//! permanent they control, and the choice is theirs.
//!
//! Driving cards: **Speedbrood Stalker** (the subject reaches back past an
//! unparsed "Secretly choose ..." clause to the declared target opponent) and
//! **Undercity Plague** (a conjugated "then sacrifices" continuing a targeted
//! player's instructions).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SPEEDBROOD_STALKER: &str = "Flying, lifelink\nWhen this creature enters, choose target \
opponent. Secretly choose a creature or planeswalker that player controls. That player \
sacrifices a creature or planeswalker of their choice, then sacrifices the chosen permanent.";

const UNDERCITY_PLAGUE: &str = "Target player loses 1 life, discards a card, then sacrifices a \
permanent of their choice.";

/// Grizzly Bears is a printed vanilla creature, so it has no Oracle text to reproduce.
const BEARS: &str = "Grizzly Bears";

fn free() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![],
    }
}

/// One sacrifice prompt the engine raised: who chooses and what is offered.
struct SacrificePrompt {
    player: PlayerId,
    cards: Vec<ObjectId>,
}

/// Drive the stack to empty, answering every prompt the way the test needs:
/// target prompts take `target_player`, discard prompts the first card, and
/// sacrifice prompts (recorded) the first offered permanent.
fn drive(runner: &mut GameRunner, target_player: PlayerId) -> Vec<SacrificePrompt> {
    let mut prompts = Vec::new();
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Player(target_player)),
                    })
                    .expect("choose the target player");
            }
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                let pick = cards[0];
                prompts.push(SacrificePrompt { player, cards });
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("select the sacrificed permanent");
            }
            WaitingFor::DiscardChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("discard a card");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return prompts;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        }
    }
    panic!("the stack did not empty");
}

/// T1. CR 608.2c + CR 701.21a: Speedbrood Stalker's "That player sacrifices a
/// creature or planeswalker of their choice" is the chosen opponent's sacrifice.
/// Only that opponent's permanents are offered, the choice is theirs, and the
/// Stalker itself (the caster's own creature) is never offered. Fails with the
/// declared-player arm of `proven_subject_player_ref` reverted: the filter then
/// carries no controller and the caster sacrifices one of their own creatures.
#[test]
fn speedbrood_stalker_opponent_sacrifices_their_own_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, BEARS, 2, 2).id();
    let theirs_a = scenario.add_creature(P1, BEARS, 2, 2).id();
    let theirs_b = scenario.add_creature(P1, BEARS, 2, 2).id();
    let stalker = scenario
        .add_creature_to_hand(P0, "Speedbrood Stalker", 3, 3)
        .from_oracle_text_with_keywords(&["Flying", "Lifelink"], SPEEDBROOD_STALKER)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    runner.cast(stalker).commit();
    let prompts = drive(&mut runner, P1);

    let first = prompts
        .first()
        .expect("the first sacrifice must prompt a choice");
    assert_eq!(first.player, P1, "the chosen opponent makes the choice");
    assert!(
        first.cards.iter().all(|c| [theirs_a, theirs_b].contains(c)),
        "only the opponent's permanents are offered: {:?}",
        first.cards
    );
    for prompt in &prompts {
        assert!(
            !prompt.cards.contains(&stalker) && !prompt.cards.contains(&mine),
            "the caster's permanents are never offered: {:?}",
            prompt.cards
        );
    }
    assert_eq!(runner.state().objects[&stalker].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mine].zone, Zone::Battlefield);
    let sacrificed = [theirs_a, theirs_b]
        .iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Graveyard)
        .count();
    assert!(sacrificed >= 1, "the opponent sacrificed a creature");
}

/// T2. CR 608.2c + CR 701.21a: Undercity Plague's "then sacrifices a permanent of
/// their choice" continues the target player's instructions: the targeted player,
/// not the caster, sacrifices. Fails with the declared-player arm reverted.
#[test]
fn undercity_plague_target_player_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, BEARS, 2, 2).id();
    let theirs = scenario.add_creature(P1, BEARS, 2, 2).id();
    let hand_card = scenario.add_creature_to_hand(P1, BEARS, 2, 2).id();
    let plague = scenario
        .add_spell_to_hand_from_oracle(P0, "Undercity Plague", false, UNDERCITY_PLAGUE)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[1].life;
    runner.cast(plague).target_player(P1).commit();
    let prompts = drive(&mut runner, P1);

    assert_eq!(runner.state().players[1].life, life_before - 1);
    assert_eq!(runner.state().objects[&hand_card].zone, Zone::Graveyard);
    for prompt in &prompts {
        assert_eq!(prompt.player, P1, "the targeted player chooses");
        assert!(
            !prompt.cards.contains(&mine),
            "the caster's permanent is never offered: {:?}",
            prompt.cards
        );
    }
    assert_eq!(runner.state().objects[&mine].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Graveyard);
}
