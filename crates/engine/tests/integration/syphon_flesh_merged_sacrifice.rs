//! CR 730.3 + CR 701.21a: a merged permanent is one permanent, so sacrificing
//! it is one sacrifice, however many components it leaves behind.
//!
//! Syphon Flesh counts "each creature sacrificed this way" through the tracked
//! set its sacrifice published. A merged creature's component cards go to the
//! graveyard with the survivor (CR 730.3) and are found with it (CR 730.3c),
//! but a count reads the raw members (the D3 aggregate): one permanent was
//! sacrificed, so one Zombie is created.

use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SYPHON_FLESH: &str = "Each other player sacrifices a creature of their choice. You create a 2/2 black Zombie creature token for each creature sacrificed this way.";
const GEMRAZER: &str = "Mutate {1}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nReach, trample\nWhenever this creature mutates, destroy target artifact or enchantment an opponent controls.";

/// Printed cost {4}{B}.
fn syphon_flesh_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 4,
        shards: vec![ManaCostShard::Black],
    }
}

fn zombie_count(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|object| object.is_token && object.name == "Zombie")
        })
        .count()
}

/// Syphon Flesh against P1's one merged creature (host plus Gemrazer).
///
/// SYNTHETIC STEP: the merge is staged with `merge_object_onto` instead of a
/// real mutate cast (precedent: `exile_attribution_10414.rs`); Syphon Flesh and
/// Gemrazer carry their real Oracle text, and the host is a vanilla Beast.
///
/// Revert sensitivity: routing the quantity count through the component
/// enumeration (the finding form) would count the host and Gemrazer, creating
/// two Zombies.
#[test]
fn one_merged_creature_sacrificed_creates_one_zombie() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let syphon = scenario
        .add_spell_to_hand_from_oracle(P0, "Syphon Flesh", false, SYPHON_FLESH)
        .with_mana_cost(syphon_flesh_cost())
        .id();
    let host = scenario
        .add_creature(P1, "Host Beast", 2, 2)
        .with_subtypes(vec!["Beast"])
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P1, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        for player in state.players.iter_mut() {
            player.hand.retain(|&id| id != gemrazer);
        }
        state.battlefield.retain(|&id| id != gemrazer);
        let mut events = Vec::new();
        merge_object_onto(state, gemrazer, host, MergeSide::Top, &mut events);
        state.battlefield.retain(|&id| id != gemrazer);
    }
    assert_eq!(
        runner.state().objects[&host].merged_components.len(),
        2,
        "reach guard: P1 controls one merged permanent of two cards"
    );

    runner.cast(syphon).resolve();
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => {
                runner.advance_until_stack_empty()
            }
            other => panic!("Syphon Flesh: unexpected prompt {other:?}"),
        }
    }

    // Reach guard: the merged creature was sacrificed, both cards are in the
    // graveyard, so the count below is not vacuous.
    assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&gemrazer].zone, Zone::Graveyard);
    assert_eq!(
        zombie_count(&runner),
        1,
        "CR 730.3 + CR 701.21a: one permanent was sacrificed, so one Zombie"
    );
}
