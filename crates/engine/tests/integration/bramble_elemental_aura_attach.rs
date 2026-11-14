//! End-to-end regression test: Bramble Elemental ("Whenever an Aura becomes
//! attached to ~, create two 1/1 green Saproling creature tokens.") must create
//! Saproling tokens when an Aura spell resolves attached to it.
//!
//! Root cause: `GameRunner` cast-resolution called `attach::attach_to` /
//! `attach::attach_to_player` directly (stack.rs) without emitting
//! `GameEvent::EffectResolved { kind: EffectKind::Attach }`, so the
//! `TriggerMode::Attached` matcher never saw the attach event and Bramble's
//! trigger never fired. The fix emits the event at both call sites.

use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CopyTargetPurpose, Effect, EffectScope, ReplacementDefinition,
    ReplacementMode, TapStateChange, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
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

const METAMORPHIC_ALTERATION_ORACLE: &str = "Enchant creature\nAs this Aura enters, choose a creature.\nEnchanted creature is a copy of the chosen creature.";

/// CR 614.12a + CR 603.10: When Metamorphic Alteration enters and enchants
/// Bramble Elemental, choosing a different creature (e.g. a vanilla Bear) causes
/// the enchanted creature to become a copy of that creature as the Aura enters.
/// Continuous effects from the choice exist immediately after the event, so
/// the host has no "becomes attached" trigger at trigger-check time and produces
/// zero Saprolings.
#[test]
fn bramble_elemental_produces_no_saprolings_when_metamorphic_alteration_copies_vanilla_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 3, 2, BRAMBLE_ELEMENTAL_ORACLE)
        .id();
    let vanilla_bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = scenario
        .add_spell_to_hand(P0, "Metamorphic Alteration", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["enchant"], METAMORPHIC_ALTERATION_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_992), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(aura).target_object(bramble).resolve();

    match &runner.state().waiting_for {
        WaitingFor::CopyTargetChoice {
            player,
            source_id,
            purpose,
            valid_targets,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*source_id, aura);
            assert!(matches!(purpose, CopyTargetPurpose::PersistChosenAttribute));
            assert!(valid_targets.contains(&bramble));
            assert!(valid_targets.contains(&vanilla_bear));
        }
        other => panic!("expected the Aura's copy-choice prompt, got {other:?}"),
    }
    // Choose the vanilla creature as the copy donor.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(vanilla_bear)),
        })
        .expect("choose Grizzly Bears through the pending Aura's copy prompt");
    runner.advance_until_stack_empty();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(bramble))
    );
    // Bramble Elemental is now a copy of Grizzly Bears (a vanilla creature),
    // so its attachment trigger does not exist immediately after the event.
    assert_eq!(
        saproling_tokens(&runner).len(),
        0,
        "choosing a vanilla creature donor must produce zero Saproling tokens"
    );
}

/// CR 603.2e + CR 701.3b: When Metamorphic Alteration enchants Bramble Elemental
/// and chooses Bramble Elemental itself, its attachment trigger is preserved.
/// The resume re-attach to the same host is a CR 701.3b no-op, so "becomes attached"
/// fires exactly once: two Saprolings, not four.
#[test]
fn bramble_elemental_triggers_once_for_aura_with_copy_choice_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 3, 2, BRAMBLE_ELEMENTAL_ORACLE)
        .id();
    let aura = scenario
        .add_spell_to_hand(P0, "Metamorphic Alteration", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["enchant"], METAMORPHIC_ALTERATION_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_992), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(aura).target_object(bramble).resolve();

    match &runner.state().waiting_for {
        WaitingFor::CopyTargetChoice {
            player,
            source_id,
            purpose,
            valid_targets,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*source_id, aura);
            assert!(matches!(purpose, CopyTargetPurpose::PersistChosenAttribute));
            assert!(valid_targets.contains(&bramble));
        }
        other => panic!("expected the Aura's copy-choice prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bramble)),
        })
        .expect("choose Bramble through the pending Aura's copy prompt");
    runner.advance_until_stack_empty();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().active_spell_resolution().is_none());
    assert!(!runner.state().has_post_replacement_drain());
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(bramble))
    );
    assert_eq!(
        saproling_tokens(&runner).len(),
        2,
        "one attachment must trigger Bramble Elemental once, even across the copy-choice resume"
    );
}

/// CR 608.3c / CR 603.2e: An Aura whose spell resolution pauses for a pre-entry
/// replacement choice (e.g. an optional "enter tapped" replacement) attaches
/// for the FIRST time on the `apply_pending_spell_resolution` resume path.
/// Bramble Elemental's "becomes attached" trigger must fire on this genuine
/// first-attachment resume.
#[test]
fn bramble_elemental_triggers_on_first_attachment_after_replacement_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(2));

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 3, 2, BRAMBLE_ELEMENTAL_ORACLE)
        .id();

    // An Aura carrying an optional self-ETB replacement ("may enter tapped").
    // This forces ReplacementResult::NeedsChoice BEFORE the permanent enters
    // the battlefield or attaches.
    let repl = ReplacementDefinition::new(ReplacementEvent::ChangeZone)
        .mode(ReplacementMode::Optional { decline: None })
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        ));

    let aura = scenario
        .add_spell_to_hand(P0, "Optional Tap Aura", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["enchant"], SIMPLE_AURA_ORACLE)
        .with_replacement_definition(repl)
        .id();

    let mut runner = scenario.build();
    runner.cast(aura).target_object(bramble).resolve();

    // Verify the resolution paused on the pre-entry replacement choice,
    // and that the Aura has NOT attached yet.
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected pre-entry ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        None,
        "Aura must not be attached while pre-entry replacement choice is pending"
    );

    // Accept the replacement.
    let pick = candidates
        .iter()
        .position(|c| c.description != "Decline")
        .unwrap_or(0);
    runner
        .act(GameAction::ChooseReplacement { index: pick })
        .expect("accept the optional tap replacement");
    runner.advance_until_stack_empty();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(bramble))
    );
    // The first attachment genuinely occurred on the replacement resume path,
    // emitting EffectResolved { Attach } and triggering Bramble Elemental once.
    let tokens = saproling_tokens(&runner);
    assert_eq!(
        tokens.len(),
        2,
        "first attachment on replacement resume must trigger Bramble Elemental once"
    );
}
