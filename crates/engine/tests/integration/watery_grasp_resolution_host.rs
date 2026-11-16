//! Printed Oracle regressions for untargeted owner-shuffle attachment operands.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// MTGJSON AtomicCards Oracle text, including the printed reminder text.
const WATERY_GRASP: &str = "Enchant creature\nEnchanted creature doesn't untap during its controller's untap step.\nWaterbend {5}: Enchanted creature's owner shuffles it into their library. (While paying a waterbend cost, you can tap your artifacts and creatures to help. Each one pays for {1}.)";
const VEIL_OF_SECRECY: &str = "Target creature gains shroud until end of turn and can't be blocked this turn. (A creature with shroud can't be the target of spells or abilities.)\nSplice onto Arcane—Return a blue creature you control to its owner's hand. (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)";

#[test]
fn watery_grasp_shuffles_its_shrouded_host_into_the_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .controlled_by(P0)
        .id();
    let aura = scenario
        .add_spell_to_hand(P0, "Watery Grasp", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["enchant"], WATERY_GRASP)
        .id();
    let veil = scenario
        .add_spell_to_hand_from_oracle(P0, "Veil of Secrecy", true, VEIL_OF_SECRECY)
        .id();
    scenario.add_card_to_library_top(P0, "P0 library card");
    scenario.add_card_to_library_top(P1, "P1 library card");
    scenario.with_mana_pool(
        P0,
        std::iter::once(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .chain(std::iter::repeat_n(
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                6,
            ))
            .collect(),
    );
    let mut runner = scenario.build();
    // Fixture ownership differs from control; neither the Aura nor its ability
    // may confuse the controller's library with the creature owner's library.
    runner.cast(aura).target_object(host).resolve();
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host))
    );
    runner.cast(veil).target_object(host).resolve();
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host))
    );
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // No target intent is supplied: any TargetSelection prompt is a regression.
    // Floating mana covers the actual printed Waterbend {5} activation cost.
    assert!(runner.state().objects[&host].has_keyword(&Keyword::Shroud));
    let outcome = runner.activate(aura, 0).resolve();
    let state = outcome.state();
    assert_eq!(state.objects[&host].zone, Zone::Library);
    assert!(state.players[P1.0 as usize].library.contains(&host));
    assert!(!state.players[P0.0 as usize].library.contains(&host));
    // CR 701.24a + CR 701.24d: delivery must not randomize as well; the terminal
    // instruction owns the single owner shuffle.
    let shuffled_players: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shuffled_players,
        vec![P1],
        "exactly one shuffle, performed by the owner, not the controller"
    );
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}
