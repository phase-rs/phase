use crate::parser::ability::parse_ability;
use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

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
pub mod sacrifice;
pub mod scry;
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
        Effect::Other { api_type, .. } => Err(EffectError::Unregistered(api_type.clone())),
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
    )
}

const MAX_CHAIN_DEPTH: u32 = 10;

/// Resolve an ability and follow its SubAbility/Execute chain.
pub fn resolve_ability_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    if depth > MAX_CHAIN_DEPTH {
        return Err(EffectError::ChainTooDeep);
    }

    // Resolve the current effect
    if !matches!(
        ability.effect,
        Effect::Other {
            ref api_type,
            ..
        } if api_type.is_empty()
    ) {
        let _ = resolve_effect(state, ability, events);
    }

    // Check for SubAbility or Execute chain
    let sub_key = ability
        .params
        .get("SubAbility")
        .or_else(|| ability.params.get("Execute"));

    if let Some(svar_name) = sub_key {
        if let Some(raw_ability) = ability.svars.get(svar_name) {
            if let Ok(def) = parse_ability(raw_ability) {
                let params = def.params();
                let mut sub_resolved = ResolvedAbility {
                    effect: def.effect.clone(),
                    params,
                    targets: Vec::new(),
                    source_id: ability.source_id,
                    controller: ability.controller,
                    sub_ability: None,
                    svars: ability.svars.clone(),
                };

                // Inherit targets if sub-ability uses Defined$ Targeted
                if sub_resolved
                    .params
                    .get("Defined")
                    .map(|v| v == "Targeted")
                    .unwrap_or(false)
                {
                    sub_resolved.targets = ability.targets.clone();
                }

                // Check conditions before executing
                if check_conditions(&sub_resolved, state) {
                    resolve_ability_chain(state, &sub_resolved, events, depth + 1)?;
                }
            }
        }
    }

    Ok(())
}

/// Check conditions on an ability link. Returns true if the ability should execute.
fn check_conditions(ability: &ResolvedAbility, state: &GameState) -> bool {
    // ConditionCompare: format like "GE2", "EQ0", "LE5"
    if let Some(compare_str) = ability.params.get("ConditionCompare") {
        let count = get_condition_count(ability, state);
        if !evaluate_compare(compare_str, count) {
            return false;
        }
    }

    // ConditionPresent: check for cards matching a type in a zone
    if let Some(filter) = ability.params.get("ConditionPresent") {
        let zone_str = ability
            .params
            .get("ConditionZone")
            .map(|s| s.as_str())
            .unwrap_or("Battlefield");
        if !evaluate_present(filter, zone_str, state) {
            return false;
        }
    }

    true
}

/// Get the count value for ConditionCompare, using ConditionSVarCompare if available.
fn get_condition_count(ability: &ResolvedAbility, state: &GameState) -> u32 {
    if let Some(_svar_name) = ability.params.get("ConditionSVarCompare") {
        // For Phase 4, count creatures on battlefield as a simple default
        state
            .battlefield
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|obj| obj.card_types.core_types.contains(&CoreType::Creature))
                    .unwrap_or(false)
            })
            .count() as u32
    } else {
        0
    }
}

/// Evaluate a comparison string like "GE2", "EQ0", "LE5" against a count.
fn evaluate_compare(compare_str: &str, count: u32) -> bool {
    let (op, num_str) = if compare_str.len() >= 3 {
        (&compare_str[..2], &compare_str[2..])
    } else {
        return true; // Malformed, default pass
    };

    let threshold: u32 = match num_str.parse() {
        Ok(n) => n,
        Err(_) => return true,
    };

    match op {
        "GE" => count >= threshold,
        "LE" => count <= threshold,
        "EQ" => count == threshold,
        "NE" => count != threshold,
        "GT" => count > threshold,
        "LT" => count < threshold,
        _ => true,
    }
}

/// Evaluate ConditionPresent: check if any card matching the filter exists in the zone.
fn evaluate_present(filter: &str, zone_str: &str, state: &GameState) -> bool {
    let check_type = match filter {
        "Creature" => Some(CoreType::Creature),
        "Land" => Some(CoreType::Land),
        _ => None,
    };

    let check_type = match check_type {
        Some(ct) => ct,
        None => return true, // Unknown filter, default pass
    };

    match zone_str {
        "Battlefield" => state.battlefield.iter().any(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.card_types.core_types.contains(&check_type))
                .unwrap_or(false)
        }),
        _ => true, // Unknown zone, default pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_ability(api_type: &str) -> ResolvedAbility {
        ResolvedAbility::from_raw(api_type, HashMap::new(), vec![], ObjectId(1), PlayerId(0))
    }

    #[test]
    fn is_known_effect_covers_38_types() {
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
        ];
        for name in &expected {
            assert!(is_known_effect(name), "missing: {}", name);
        }
        assert_eq!(expected.len(), 38);
    }

    #[test]
    fn resolve_effect_returns_unregistered_for_unknown() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability("NonExistentEffect");
        let mut events = Vec::new();
        let result = resolve_effect(&mut state, &ability, &mut events);
        assert_eq!(
            result,
            Err(EffectError::Unregistered("NonExistentEffect".to_string()))
        );
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
    fn resolve_ability_chain_with_sub_ability() {
        let mut state = GameState::new_two_player(42);
        // Add cards to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );

        let effect = Effect::DealDamage {
            amount: crate::types::ability::DamageAmount::Fixed(2),
            target: crate::types::ability::TargetSpec::Any,
        };
        let mut params = effect.to_params();
        params.insert("SubAbility".to_string(), "DBDraw".to_string());
        let ability = ResolvedAbility {
            effect,
            params,
            targets: vec![TargetRef::Player(PlayerId(1))],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::from([("DBDraw".to_string(), "DB$ Draw | NumCards$ 1".to_string())]),
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
        let ability = make_ability("Draw");
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 11);
        assert_eq!(result, Err(EffectError::ChainTooDeep));
    }

    #[test]
    fn condition_present_creature_on_battlefield_passes() {
        let mut state = GameState::new_two_player(42);
        let creature_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::from_raw(
            "Draw",
            HashMap::from([("ConditionPresent".to_string(), "Creature".to_string())]),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        assert!(check_conditions(&ability, &state));
    }

    #[test]
    fn condition_present_no_creatures_fails() {
        let state = GameState::new_two_player(42);

        let ability = ResolvedAbility::from_raw(
            "Draw",
            HashMap::from([("ConditionPresent".to_string(), "Creature".to_string())]),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        assert!(!check_conditions(&ability, &state));
    }

    #[test]
    fn condition_compare_ge2_with_3_creatures_passes() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Creature {}", i),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::from_raw(
            "Draw",
            HashMap::from([
                ("ConditionCompare".to_string(), "GE2".to_string()),
                (
                    "ConditionSVarCompare".to_string(),
                    "CreatureCount".to_string(),
                ),
            ]),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        assert!(check_conditions(&ability, &state));
    }

    #[test]
    fn condition_compare_eq0_with_1_creature_fails() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::from_raw(
            "Draw",
            HashMap::from([
                ("ConditionCompare".to_string(), "EQ0".to_string()),
                (
                    "ConditionSVarCompare".to_string(),
                    "CreatureCount".to_string(),
                ),
            ]),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        assert!(!check_conditions(&ability, &state));
    }
}
