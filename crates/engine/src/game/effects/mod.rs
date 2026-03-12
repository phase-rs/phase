use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

pub mod animate;
pub mod attach;
pub mod bounce;
pub mod change_zone;
pub mod choose_card;
pub mod cleanup;
pub mod copy_spell;
pub mod counter;
pub mod counters;
pub mod deal_damage;
pub mod destroy;
pub mod dig;
pub mod discard;
pub mod draw;
pub mod effect;
pub mod explore;
pub mod fight;
pub mod gain_control;
pub mod life;
pub mod mana;
pub mod mill;
pub mod proliferate;
pub mod pump;
pub mod reveal_hand;
pub mod sacrifice;
pub mod scry;
pub mod shuffle;
pub mod surveil;
pub mod tap_untap;
pub mod token;

/// Dispatch to the appropriate effect handler using typed pattern matching.
pub fn resolve_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::DealDamage { .. } => deal_damage::resolve(state, ability, events),
        Effect::Draw { .. } => draw::resolve(state, ability, events),
        Effect::Pump { .. } => pump::resolve(state, ability, events),
        Effect::Destroy { .. } => destroy::resolve(state, ability, events),
        Effect::Counter { .. } => counter::resolve(state, ability, events),
        Effect::Token { .. } => token::resolve(state, ability, events),
        Effect::GainLife { .. } => life::resolve_gain(state, ability, events),
        Effect::LoseLife { .. } => life::resolve_lose(state, ability, events),
        Effect::Tap { .. } => tap_untap::resolve_tap(state, ability, events),
        Effect::Untap { .. } => tap_untap::resolve_untap(state, ability, events),
        Effect::AddCounter { .. } => counters::resolve_add(state, ability, events),
        Effect::RemoveCounter { .. } => counters::resolve_remove(state, ability, events),
        Effect::Sacrifice { .. } => sacrifice::resolve(state, ability, events),
        Effect::DiscardCard { .. } => discard::resolve(state, ability, events),
        Effect::Mill { .. } => mill::resolve(state, ability, events),
        Effect::Scry { .. } => scry::resolve(state, ability, events),
        Effect::PumpAll { .. } => pump::resolve_all(state, ability, events),
        Effect::DamageAll { .. } => deal_damage::resolve_all(state, ability, events),
        Effect::DestroyAll { .. } => destroy::resolve_all(state, ability, events),
        Effect::ChangeZone { .. } => change_zone::resolve(state, ability, events),
        Effect::ChangeZoneAll { .. } => change_zone::resolve_all(state, ability, events),
        Effect::Dig { .. } => dig::resolve(state, ability, events),
        Effect::GainControl { .. } => gain_control::resolve(state, ability, events),
        Effect::Attach { .. } => attach::resolve(state, ability, events),
        Effect::Surveil { .. } => surveil::resolve(state, ability, events),
        Effect::Fight { .. } => fight::resolve(state, ability, events),
        Effect::Bounce { .. } => bounce::resolve(state, ability, events),
        Effect::Explore => explore::resolve(state, ability, events),
        Effect::Proliferate => proliferate::resolve(state, ability, events),
        Effect::CopySpell { .. } => copy_spell::resolve(state, ability, events),
        Effect::ChooseCard { .. } => choose_card::resolve(state, ability, events),
        Effect::PutCounter { .. } => counters::resolve_add(state, ability, events),
        Effect::MultiplyCounter { .. } => counters::resolve_multiply(state, ability, events),
        Effect::Animate { .. } => animate::resolve(state, ability, events),
        Effect::GenericEffect { .. } => effect::resolve(state, ability, events),
        Effect::Cleanup { .. } => cleanup::resolve(state, ability, events),
        Effect::Mana { .. } => mana::resolve(state, ability, events),
        Effect::Discard { .. } => discard::resolve(state, ability, events),
        Effect::Shuffle { .. } => shuffle::resolve(state, ability, events),
        Effect::RevealHand { .. } => reveal_hand::resolve(state, ability, events),
        Effect::Unimplemented { name, .. } => {
            // Log warning and return Ok (no-op) for unimplemented effects
            eprintln!("Warning: Unimplemented effect: {}", name);
            Ok(())
        }
    }
}

/// Returns true if the given api_type string is a known effect handler.
/// Used by coverage analysis to check card support.
pub fn is_known_effect(api_type: &str) -> bool {
    matches!(
        api_type,
        "DealDamage"
            | "Draw"
            | "Pump"
            | "Destroy"
            | "Counter"
            | "Token"
            | "GainLife"
            | "LoseLife"
            | "Tap"
            | "Untap"
            | "AddCounter"
            | "RemoveCounter"
            | "Sacrifice"
            | "DiscardCard"
            | "Mill"
            | "Scry"
            | "PumpAll"
            | "DamageAll"
            | "DestroyAll"
            | "ChangeZone"
            | "ChangeZoneAll"
            | "Dig"
            | "GainControl"
            | "Attach"
            | "Surveil"
            | "Fight"
            | "Bounce"
            | "Explore"
            | "Proliferate"
            | "CopySpell"
            | "ChooseCard"
            | "PutCounter"
            | "MultiplyCounter"
            | "Animate"
            | "Effect"
            | "Cleanup"
            | "Mana"
            | "Discard"
            | "Shuffle"
            | "RevealHand"
    )
}

/// Resolve an ability and follow its sub_ability chain using typed nested structs.
/// No SVar lookup, no parse_ability(). The depth is bounded by the data structure.
pub fn resolve_ability_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    // Safety limit to prevent stack overflow on pathological data
    if depth > 20 {
        return Err(EffectError::ChainTooDeep);
    }

    // Skip no-op unimplemented effects
    if !matches!(ability.effect, Effect::Unimplemented { .. }) {
        let _ = resolve_effect(state, ability, events);
    }

    // Follow typed sub_ability chain, propagating parent targets when sub has none.
    // This allows sub-abilities like "its controller gains life" to access the object
    // targeted by the parent (e.g. the exiled creature in Swords to Plowshares).
    if let Some(ref sub) = ability.sub_ability {
        // If resolve_effect just entered a player-choice state (Scry/Dig/Surveil),
        // save the sub-ability as a continuation to execute after the player responds,
        // rather than immediately processing it (which would bypass the UI).
        if matches!(
            state.waiting_for,
            WaitingFor::ScryChoice { .. }
                | WaitingFor::DigChoice { .. }
                | WaitingFor::SurveilChoice { .. }
                | WaitingFor::RevealChoice { .. }
        ) {
            let mut sub_clone = sub.as_ref().clone();
            if sub_clone.targets.is_empty() && !ability.targets.is_empty() {
                sub_clone.targets = ability.targets.clone();
            }
            state.pending_continuation = Some(Box::new(sub_clone));
            return Ok(());
        }

        if sub.targets.is_empty() && !ability.targets.is_empty() {
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = ability.targets.clone();
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else {
            resolve_ability_chain(state, sub, events, depth + 1)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{DamageAmount, TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn is_known_effect_covers_39_types() {
        let expected = [
            "DealDamage",
            "Draw",
            "ChangeZone",
            "Pump",
            "Destroy",
            "Counter",
            "Token",
            "GainLife",
            "LoseLife",
            "Tap",
            "Untap",
            "AddCounter",
            "RemoveCounter",
            "Sacrifice",
            "DiscardCard",
            "Mill",
            "Scry",
            "PumpAll",
            "DamageAll",
            "DestroyAll",
            "ChangeZoneAll",
            "Dig",
            "GainControl",
            "Attach",
            "Surveil",
            "Fight",
            "Bounce",
            "Explore",
            "Proliferate",
            "CopySpell",
            "ChooseCard",
            "PutCounter",
            "MultiplyCounter",
            "Animate",
            "Effect",
            "Cleanup",
            "Mana",
            "Discard",
            "Shuffle",
            "RevealHand",
        ];
        for name in &expected {
            assert!(is_known_effect(name), "missing: {}", name);
        }
        assert_eq!(expected.len(), 40);
    }

    #[test]
    fn resolve_effect_returns_ok_for_unimplemented() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "NonExistentEffect".to_string(),
                description: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve_effect(&mut state, &ability, &mut events);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_ability_chain_single_effect() {
        let mut state = GameState::new_two_player(42);
        // Add a card in library so Draw has something to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Draw { count: 1 },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn resolve_ability_chain_with_typed_sub_ability() {
        let mut state = GameState::new_two_player(42);
        // Add cards to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );

        // Build a chain: DealDamage -> Draw using typed sub_ability
        let sub = ResolvedAbility::new(
            Effect::Draw { count: 1 },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility {
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(2),
                target: TargetFilter::Any,
            },
            targets: vec![TargetRef::Player(PlayerId(1))],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: Some(Box::new(sub)),
            duration: None,
        };
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());
        // Damage dealt to player 1
        assert_eq!(state.players[1].life, 18);
        // Controller drew a card
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn chain_depth_exceeds_limit_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability =
            ResolvedAbility::new(Effect::Draw { count: 1 }, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 21);
        assert_eq!(result, Err(EffectError::ChainTooDeep));
    }
}
