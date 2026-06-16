//! Integration tests for Springheart Nantuko's bestow landfall trigger
//! (GitHub issue #437).
//!
//! Oracle text:
//!   Bestow {1}{G}
//!   Enchanted creature gets +1/+1.
//!   Landfall — Whenever a land you control enters, you may pay {1}{G} if this
//!   permanent is attached to a creature you control. If you do, create a token
//!   that's a copy of that creature. If you didn't create a token this way,
//!   create a 1/1 green Insect creature token.
//!
//! Three defects were fixed:
//!   (a) the parser emitted `CopyTokenOf { target: ParentTarget }` — "that
//!       creature" is the enchanted host, so it must be `AttachedTo`
//!       (CR 303.4 + CR 702.103);
//!   (c) on the optional-trigger decline path the `Not(IfYouDo)` Insect
//!       fallback was never reached (CR 609.3);
//!   plus an empty-host `CopyTokenOf` no-op (CR 609.3) so an unattached
//!   Springheart does not error.
//!
//! These tests drive the real `apply` pipeline — land drop, trigger
//! resolution, and the optional-effect decision are all submitted as
//! `GameAction`s.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SPRINGHEART_ORACLE: &str = "Bestow {1}{G}\n\
Enchanted creature gets +1/+1.\n\
Landfall — Whenever a land you control enters, you may pay {1}{G} if this \
permanent is attached to a creature you control. If you do, create a token \
that's a copy of that creature. If you didn't create a token this way, create \
a 1/1 green Insect creature token.";

/// Drive the engine to a settled state (Priority with empty stack), resolving
/// the landfall trigger's optional-effect prompt with `accept`.
fn resolve_landfall(runner: &mut engine::game::scenario::GameRunner, accept: bool) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional-effect decision should succeed");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// Count battlefield Insect creature tokens controlled by P0.
fn insect_token_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Insect")
        })
        .count()
}

// ---------------------------------------------------------------------------
// Parser: the copy target must be `AttachedTo`, not `ParentTarget`.
// ---------------------------------------------------------------------------

#[test]
fn springheart_landfall_copy_target_is_attached_to() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{Effect, TargetFilter};
    use engine::types::triggers::TriggerMode;

    // Bestow keyword is supplied so `parse_oracle_ir` flags the card as an
    // attachment-capable permanent and sets the typed host self-reference.
    let parsed = parse_oracle_text(
        SPRINGHEART_ORACLE,
        "Springheart Nantuko",
        &["Bestow".to_string()],
        &["Enchantment".to_string(), "Creature".to_string()],
        &["Insect".to_string(), "Monk".to_string()],
    );

    let trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::ChangesZone))
        .expect("Springheart has a Landfall (ChangesZone) trigger");

    // Walk the execute chain to the `CopyTokenOf` link.
    let execute = trigger.execute.as_deref().expect("trigger has execute");
    let mut copy_target: Option<TargetFilter> = None;
    let mut node = Some(execute);
    while let Some(ability) = node {
        if let Effect::CopyTokenOf { target, .. } = ability.effect.as_ref() {
            copy_target = Some(target.clone());
            break;
        }
        node = ability.sub_ability.as_deref();
    }

    assert_eq!(
        copy_target,
        Some(TargetFilter::AttachedTo),
        "the copy-token target must be the enchanted host (AttachedTo), \
         not a chosen target (ParentTarget)"
    );
}

// ---------------------------------------------------------------------------
// Runtime: bestowed + accept → token copy of the enchanted creature.
// ---------------------------------------------------------------------------

#[test]
fn springheart_bestowed_accept_creates_copy_of_enchanted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The host creature Springheart is bestowed onto.
    let host_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // Springheart Nantuko as a bestowed Aura attached to the host.
    let springheart_id = scenario
        .add_creature(P0, "Springheart Nantuko", 1, 1)
        .as_enchantment()
        .from_oracle_text_with_keywords(&["Bestow"], SPRINGHEART_ORACLE)
        .id();

    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    // {1}{G} available so the optional pay succeeds.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&springheart_id)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host_id));

    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("P0 plays Forest");

    resolve_landfall(&mut runner, true);

    // A token copy of the enchanted creature ("Grizzly Bears") must exist.
    let copy_count = runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield && o.name == "Grizzly Bears")
        .count();
    assert_eq!(
        copy_count, 1,
        "accepting the pay must create one token copy of the enchanted creature"
    );

    // The `Not(IfYouDo)` Insect fallback must NOT fire — a token was created.
    assert_eq!(
        insect_token_count(&runner),
        0,
        "no Insect fallback token when the copy was created"
    );
}

// ---------------------------------------------------------------------------
// Runtime: bestowed + decline → 1/1 green Insect token (core decline bug).
// ---------------------------------------------------------------------------

#[test]
fn springheart_bestowed_decline_creates_insect_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let springheart_id = scenario
        .add_creature(P0, "Springheart Nantuko", 1, 1)
        .as_enchantment()
        .from_oracle_text_with_keywords(&["Bestow"], SPRINGHEART_ORACLE)
        .id();
    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&springheart_id)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host_id));

    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("P0 plays Forest");

    resolve_landfall(&mut runner, false);

    // Declining the optional pay must still create the 1/1 green Insect token
    // via the `Not(IfYouDo)` fallback — the core decline-path bug.
    assert_eq!(
        insect_token_count(&runner),
        1,
        "declining the optional pay must create the 1/1 green Insect token"
    );

    // No copy of the enchanted creature was made.
    let copy_count = runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield && o.name == "Grizzly Bears")
        .count();
    assert_eq!(copy_count, 0, "no token copy when the pay is declined");
}

// ---------------------------------------------------------------------------
// Runtime: unattached → 1/1 green Insect token, no copy, no error.
// ---------------------------------------------------------------------------

#[test]
fn springheart_unattached_creates_insect_token_without_error() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Springheart on the battlefield as a creature — not bestowed, no host.
    scenario
        .add_creature(P0, "Springheart Nantuko", 1, 1)
        .from_oracle_text_with_keywords(&["Bestow"], SPRINGHEART_ORACLE);

    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("P0 plays Forest");

    // Declining the optional pay on an unattached Springheart must still
    // create the Insect token — the empty-host `CopyTokenOf` is a clean
    // zero-token no-op, not an error.
    resolve_landfall(&mut runner, false);

    assert_eq!(
        insect_token_count(&runner),
        1,
        "an unattached Springheart must still create the 1/1 green Insect token"
    );
}
