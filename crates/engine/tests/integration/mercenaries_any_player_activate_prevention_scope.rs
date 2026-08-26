//! Mercenaries (Ice Age) — "{3}: The next time this creature would deal damage
//! to you this turn, prevent that damage. Any player may activate this ability."
//!
//! Misparse-backlog category #9 ("Wrong player/controller scope"): the "to you"
//! recipient of the prevention shield is the ABILITY'S CONTROLLER — and because
//! "Any player may activate this ability", the controller is whoever activated
//! it (CR 602.2a), NOT Mercenaries' static controller. Before the fix,
//! `parse_oneshot_damage_replacement` hardcoded `target: TargetFilter::Any` in
//! its prevention branch (and `parse_damage_target_phrase` had no "to you" arm),
//! so the shield had ZERO recipient restriction — it would prevent Mercenaries'
//! damage to ANY player.
//!
//! The fix parses "to you" → `Player { Controller }` and bridges it into
//! `Effect::PreventDamage { target: TargetFilter::Controller }`; at runtime
//! `resolve_player_for_context_ref` resolves `Controller` to the activator, and
//! `untargeted_damage_filter` installs the shield scoped to
//! `Player { Specific(activator) }` (CR 615.1a).
//!
//! This drives the REAL pipeline: build Mercenaries from verbatim Oracle text,
//! have the OPPONENT (P1) activate the ability through `apply()` (legal only via
//! `activator_filter = All` from "Any player may activate"), resolve it, then
//! push actual damage events through the production replacement pipeline
//! (`replace_event`).

use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, DamageTargetFilter, DamageTargetPlayerScope, TargetRef};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;

// Verbatim Oracle text (data/card-data.json).
const MERCENARIES: &str = "{3}: The next time this creature would deal damage to you this turn, \
prevent that damage. Any player may activate this ability.";

/// Locate the runtime index of Mercenaries' activated ability.
fn activated_ability_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated)
        .expect("Mercenaries must carry an activated ability")
}

/// Fund `player`'s mana pool with `n` units (the activation driver finalizes the
/// pool via PassPriority; source auto-tap is not modeled).
fn add_mana(runner: &mut GameRunner, player: PlayerId, n: usize) {
    for _ in 0..n {
        let unit = ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]);
        runner.state_mut().players[usize::from(player.0)]
            .mana_pool
            .add(unit);
    }
}

fn damage_from(source: ObjectId, target: TargetRef, amount: u32) -> ProposedEvent {
    ProposedEvent::Damage {
        source_id: source,
        target,
        amount,
        is_combat: false,
        applied: Default::default(),
    }
}

/// Discriminating regression: when the OPPONENT (P1) activates Mercenaries'
/// ability, the installed prevention shield scopes to P1 (the activator), not P0
/// (Mercenaries' static controller).
///
/// Revert-failing assertions:
///   - the installed shield's `damage_target_filter` is `Some(Player{Specific(P1)})`
///     — pre-fix (`target: Any`) it is `None` (no recipient restriction);
///   - Mercenaries' damage to P0 is NOT prevented (Execute) — pre-fix the
///     unrestricted shield WOULD prevent it (the negative that flips).
#[test]
fn opponent_activated_shield_scopes_to_activator_not_source_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let merc = scenario
        .add_creature_from_oracle(P0, "Mercenaries", 2, 2, MERCENARIES)
        .id();
    let mut runner = scenario.build();

    // P1 (the opponent) activates the ability. Give P1 priority on their own turn
    // and fund the {3} cost. Activation is legal for P1 only because
    // "Any player may activate this ability" set `activator_filter = All`.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    add_mana(&mut runner, P1, 3);

    let idx = activated_ability_index(&runner, merc);
    runner.activate(merc, idx).resolve();

    // The installed prevention shield must scope its recipient to the ACTIVATOR
    // (P1). Pre-fix, `target: Any` yields `damage_target_filter = None`.
    let shields: Vec<&DamageTargetFilter> = runner.state().objects[&merc]
        .replacement_definitions
        .iter_unchecked()
        .filter_map(|r| r.damage_target_filter.as_ref())
        .collect();
    assert_eq!(
        shields.len(),
        1,
        "exactly one prevention shield with a recipient filter must be installed, got {shields:?}"
    );
    assert_eq!(
        shields[0],
        &DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Specific(P1),
        },
        "the shield must scope to the activator (P1), not Mercenaries' controller (P0)"
    );

    // Behavioral proof through the production replacement pipeline. Feed P0's
    // damage FIRST: the activator-scoped shield must NOT match it (this is the
    // assertion that flips under the pre-fix unrestricted shield, which would
    // prevent damage to anyone — and would also consume the one-shot here).
    let mut events = Vec::new();
    let to_controller = replace_event(
        runner.state_mut(),
        damage_from(merc, TargetRef::Player(P0), 3),
        &mut events,
    );
    assert!(
        matches!(to_controller, ReplacementResult::Execute(_)),
        "Mercenaries' damage to P0 (source controller) must NOT be prevented by an \
         activator(P1)-scoped shield, got {to_controller:?}"
    );

    // The shield DOES prevent Mercenaries' damage to the activator (P1).
    let to_activator = replace_event(
        runner.state_mut(),
        damage_from(merc, TargetRef::Player(P1), 2),
        &mut events,
    );
    assert!(
        matches!(to_activator, ReplacementResult::Prevented),
        "Mercenaries' damage to P1 (the activator, 'you') must be prevented, got {to_activator:?}"
    );
}
