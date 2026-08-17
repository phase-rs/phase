//! Verification for the reported Esper Origins bug: cast from the graveyard,
//! the spell must exile itself and then re-enter the battlefield TRANSFORMED
//! (back face: Summon: Esper Maduin) with a finality counter — not end exiled.
//!
//! Front face Oracle (verbatim, MTGJSON):
//!   "Surveil 2. You gain 2 life. If this spell was cast from a graveyard,
//!    exile it, then put it onto the battlefield transformed under its owner's
//!    control with a finality counter on it. (...)
//!    Flashback {3}{G} (...)"
//!
//! The ONLY way to cast Esper Origins from a graveyard is via Flashback
//! (CR 702.34a), so the flashback path is exercised through the full cast
//! pipeline (`runner.cast(..).casting_variant(CastingVariant::Flashback)`.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::game_state::CastingVariant;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim front-face Oracle text from MTGJSON (FINAL — Esper Origins).
const ESPER_ORIGINS_ORACLE: &str = "Surveil 2. You gain 2 life. If this spell was cast from a \
graveyard, exile it, then put it onto the battlefield transformed under its owner's control with \
a finality counter on it. (If a creature with a finality counter on it would die, exile it \
instead.)\nFlashback {3}{G} (You may cast this card from your graveyard for its flashback cost. \
Then exile it.)";

/// Build the canonical board: Esper Origins in P0's graveyard (front face +
/// Flashback from the Oracle text), its back face (Summon: Esper Maduin, a
/// 4/4 Enchantment Creature — Saga Elemental), and a pool able to pay the
/// Flashback cost {3}{G}.
fn stage_esper_origins_in_graveyard() -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_graveyard(P0, "Esper Origins", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .from_oracle_text(ESPER_ORIGINS_ORACLE)
        .id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        let mut card_types = CardType::default();
        card_types.core_types.push(CoreType::Enchantment);
        card_types.core_types.push(CoreType::Creature);
        card_types.subtypes = vec!["Saga".to_string(), "Elemental".to_string()];
        obj.back_face = Some(BackFaceData {
            name: "Summon: Esper Maduin".to_string(),
            power: Some(4),
            toughness: Some(4),
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types,
            mana_cost: ManaCost::default(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
            parse_warnings: vec![],
        });
    }
    // Float {3}{G} to pay the Flashback cost.
    runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
    );
    for _ in 0..3 {
        runner.state_mut().add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        );
    }
    (runner, spell)
}

/// The reported behavior is "only exiled". Correct behavior (the card's Oracle
/// text): exile it, THEN put it onto the battlefield transformed with a
/// finality counter.
#[test]
fn esper_origins_flashback_cast_enters_transformed_with_finality_counter() {
    let (mut runner, spell) = stage_esper_origins_in_graveyard();

    let outcome = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .resolve();

    // If the bug reproduces, this assertion fails: the card sits in exile.
    outcome.assert_zone(&[spell], Zone::Battlefield);

    let obj = &outcome.state().objects[&spell];
    assert!(
        obj.transformed,
        "CR 701.27: the spell must enter on its back face (Summon: Esper Maduin)"
    );
    assert_eq!(
        obj.name, "Summon: Esper Maduin",
        "CR 701.27a: a DFC entering transformed shows its back face"
    );
    assert_eq!(
        outcome.counters(spell, CounterType::Finality),
        1,
        "CR 122.1h: must enter with a finality counter"
    );
    // The spell's spell-level effects still ran: Surveil 2 + gain 2 life.
    outcome.assert_life_delta(P0, 2);
    assert!(
        matches!(
            outcome.final_waiting_for(),
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "resolution must complete back to a priority window"
    );
}

/// Positive control for the same staged board: a spell with NO cast-from-zone
/// condition must never offer the flashback cast at all — proving the tester
/// cannot accidentally pass the transformed-entry assertion by the engine
/// ignoring the cast origin gate (foot-gun 6 reach-guard).
#[test]
fn esper_origins_flashback_requires_graveyard_origin_gate() {
    // Same verbatim text staged in the HAND instead: flashback is a
    // graveyard-only keyword (CR 702.34a), so the spell must NOT be castable
    // from hand via the flashback variant.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Esper Origins", false, ESPER_ORIGINS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    for _ in 0..4 {
        runner.state_mut().add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        );
    }

    // From hand there is no flashback offer; the normal cast is {1}{G} and
    // does NOT transform (the transformed-entry branch requires the
    // WasCast{Graveyard} condition). Drive the normal cast through the same
    // pipeline and assert the ordinary sorcery outcome.
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    let obj = &outcome.state().objects[&spell];
    assert!(
        !obj.transformed,
        "a hand cast must resolve as the front sorcery"
    );
}
