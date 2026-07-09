//! CR 724.2: integration coverage for the "end the combat phase" effect
//! (`Effect::EndCombatPhase`, Mandate of Peace). The resolver lives in
//! `engine::game::effects::end_combat_phase`; these tests exercise its
//! public behavior through the engine's public API.

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::effects::end_combat_phase::resolve;
use engine::game::stack::resolve_top;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, EffectKind, ResolvedAbility};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn spell_entry(id: ObjectId, controller: PlayerId, card_id: CardId) -> StackEntry {
    StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    }
}

fn resolved(emitted: &[GameEvent]) -> bool {
    emitted.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::EndCombatPhase,
                ..
            }
        )
    })
}

/// CR 724.2b/c/d: ending the combat phase exiles other objects on the stack,
/// removes everything from combat, and skips straight to the postcombat main
/// phase (CR 724.2e: the end-of-combat step is skipped).
#[test]
fn end_combat_phase_exiles_stack_clears_combat_and_enters_postcombat_main() {
    let mut state = GameState::new_two_player(42);
    state.phase = Phase::DeclareAttackers;

    // CR 724.2b: a non-source spell on the stack must be exiled.
    let other_spell = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Bolt".to_string(),
        Zone::Stack,
    );
    state
        .stack
        .push_back(spell_entry(other_spell, PlayerId(1), CardId(1)));

    // CR 724.2d: an attacking creature must be removed from combat.
    let attacker = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    state.combat = Some(CombatState {
        attackers: vec![AttackerInfo {
            object_id: attacker,
            defending_player: PlayerId(1),
            attack_target: AttackTarget::Player(PlayerId(1)),
            blocked: false,
            band_id: None,
        }],
        ..Default::default()
    });

    // The end-the-combat-phase source (e.g. Mandate of Peace). `resolve_top`
    // pops its own stack entry before invoking the resolver, so only the other
    // spell is on the stack here.
    let ability = ResolvedAbility::new(Effect::EndCombatPhase, vec![], ObjectId(999), PlayerId(0));
    let mut events = Vec::new();

    resolve(&mut state, &ability, &mut events).unwrap();

    // CR 724.2b: stack exiled/emptied.
    assert!(state.stack.is_empty(), "stack should be emptied");
    assert_eq!(
        state.objects.get(&other_spell).map(|o| o.zone),
        Some(Zone::Exile),
        "the other spell should be exiled"
    );
    // CR 724.2d: combat removed and we skipped straight to postcombat main.
    assert!(state.combat.is_none(), "combat should be cleared");
    assert_eq!(
        state.phase,
        Phase::PostCombatMain,
        "should skip to the postcombat main phase"
    );
    assert!(
        resolved(&events),
        "should emit EffectResolved(EndCombatPhase)"
    );
}

/// CR 724.2b: resolving "end the combat phase" during combat exiles the
/// resolving object itself through the production stack-resolution path.
#[test]
fn end_combat_phase_spell_exiles_resolving_object_during_combat() {
    let mut state = GameState::new_two_player(42);
    state.phase = Phase::DeclareAttackers;

    let spell_id = create_object(
        &mut state,
        CardId(7242),
        PlayerId(0),
        "Mandate of Peace".to_string(),
        Zone::Stack,
    );
    let ability = ResolvedAbility::new(Effect::EndCombatPhase, vec![], spell_id, PlayerId(0));
    state.stack.push_back(StackEntry {
        id: spell_id,
        source_id: spell_id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(7242),
            ability: Some(ability),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let mut events = Vec::new();
    resolve_top(&mut state, &mut events);

    assert_eq!(
        state.objects.get(&spell_id).map(|o| o.zone),
        Some(Zone::Exile),
        "the resolving spell should be exiled during the CR 724.2 process"
    );
    assert!(state.exile.contains(&spell_id));
    assert!(!state.players[0].graveyard.contains(&spell_id));
    assert_eq!(state.phase, Phase::PostCombatMain);
}

/// CR 724.2g: if it isn't a combat phase, nothing happens — the stack is left
/// intact and the phase is unchanged. This is the behavior that distinguishes
/// "end the combat phase" from "end the turn".
#[test]
fn end_combat_phase_outside_combat_does_nothing() {
    let mut state = GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;

    let bystander = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Bolt".to_string(),
        Zone::Stack,
    );
    state
        .stack
        .push_back(spell_entry(bystander, PlayerId(1), CardId(1)));

    let ability = ResolvedAbility::new(Effect::EndCombatPhase, vec![], ObjectId(999), PlayerId(0));
    let mut events = Vec::new();

    resolve(&mut state, &ability, &mut events).unwrap();

    // CR 724.2g: nothing happens — the stack and phase are untouched.
    assert_eq!(state.stack.len(), 1, "stack should be untouched");
    assert_eq!(
        state.objects.get(&bystander).map(|o| o.zone),
        Some(Zone::Stack),
        "the bystander spell should remain on the stack"
    );
    assert_eq!(
        state.phase,
        Phase::PreCombatMain,
        "phase should be unchanged outside combat"
    );
    assert!(
        resolved(&events),
        "the effect still resolves (as a no-op) and emits EffectResolved"
    );
}

/// CR 724.2g + CR 608.2n: outside combat, the end-combat procedure does
/// nothing, so the resolving instant follows normal post-resolution routing.
#[test]
fn end_combat_phase_spell_goes_to_graveyard_outside_combat() {
    let mut state = GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;

    let spell_id = create_object(
        &mut state,
        CardId(7243),
        PlayerId(0),
        "Mandate of Peace".to_string(),
        Zone::Stack,
    );
    let ability = ResolvedAbility::new(Effect::EndCombatPhase, vec![], spell_id, PlayerId(0));
    state.stack.push_back(StackEntry {
        id: spell_id,
        source_id: spell_id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(7243),
            ability: Some(ability),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let mut events = Vec::new();
    resolve_top(&mut state, &mut events);

    assert_eq!(
        state.objects.get(&spell_id).map(|o| o.zone),
        Some(Zone::Graveyard),
        "outside combat, the spell should use normal instant/sorcery routing"
    );
    assert!(state.players[0].graveyard.contains(&spell_id));
    assert!(!state.exile.contains(&spell_id));
    assert_eq!(state.phase, Phase::PreCombatMain);
}
