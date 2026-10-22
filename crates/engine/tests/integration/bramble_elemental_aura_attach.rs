//! End-to-end regression test: Bramble Elemental ("Whenever an Aura becomes
//! attached to ~, create two 1/1 green Saproling creature tokens.") must create
//! Saproling tokens when an Aura spell resolves attached to it.
//!
//! Root cause: `GameRunner` cast-resolution called `attach::attach_to` /
//! `attach::attach_to_player` directly (stack.rs) without emitting
//! `GameEvent::EffectResolved { kind: EffectKind::Attach }`, so the
//! `TriggerMode::Attached` matcher never saw the attach event and Bramble's
//! trigger never fired. The fix emits the event at both call sites.

use engine::game::game_object::GameObject;
use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn pool(generic: usize) -> Vec<ManaUnit> {
    (0..generic)
        .map(|_| ManaType::Colorless)
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn saproling_tokens(runner: &engine::game::scenario::GameRunner) -> Vec<&GameObject> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token && object.card_types.subtypes.iter().any(|s| s == "Saproling")
        })
        .collect()
}

/// Bramble Elemental Oracle text (verified against Scryfall).
const BRAMBLE_ELEMENTAL_ORACLE: &str =
    "Whenever an Aura becomes attached to ~, create two 1/1 green Saproling creature tokens.";

/// A minimal "Enchant creature" Aura with no other abilities.
const SIMPLE_AURA_ORACLE: &str = "Enchant creature";

#[test]
fn bramble_elemental_creates_saprolings_when_aura_attaches() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(2));

    // Bramble Elemental on the battlefield.
    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 3, 2, BRAMBLE_ELEMENTAL_ORACLE)
        .id();

    // An Aura in hand to cast at Bramble.
    let aura = scenario
        .add_spell_to_hand(P0, "Simple Aura", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .from_oracle_text_with_keywords(&["enchant"], SIMPLE_AURA_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Cast the Aura targeting Bramble Elemental.
    runner.cast(aura).target_object(bramble).resolve();
    runner.advance_until_stack_empty();

    // Two Saproling tokens must be created.
    let tokens = saproling_tokens(&runner);
    assert_eq!(
        tokens.len(),
        2,
        "Bramble Elemental must create two Saproling tokens when an Aura attaches to it"
    );
    for token in &tokens {
        assert_eq!(token.zone, Zone::Battlefield);
        assert_eq!(token.power, Some(1));
        assert_eq!(token.toughness, Some(1));
    }
}
