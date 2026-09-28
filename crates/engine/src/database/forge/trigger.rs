use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{all_consuming, opt, recognize};
use nom::sequence::preceded;
use nom::Parser;

use crate::types::ability::{TargetFilter, TriggerDefinition};
use crate::types::triggers::TriggerMode;
use crate::types::Zone;

use super::filter::translate_filter;
use super::svar::SvarResolver;
use super::types::{ForgeAbilityLine, ForgeTranslateError};

fn validate_exploited_filter(filter: &str) -> Result<&str, ForgeTranslateError> {
    let filter = filter.trim();
    let mut parser = all_consuming(alt((
        tag::<_, _, nom::error::Error<&str>>("Any"),
        tag("Card.Self"),
        recognize((
            tag("Creature"),
            opt(preceded(tag("."), alt((tag("YouCtrl"), tag("OppCtrl"))))),
        )),
    )));
    parser
        .parse(filter)
        .map(|(_, matched)| matched)
        .map_err(|_| ForgeTranslateError::UnparsableFilter(filter.to_string()))
}

/// Translate a Forge `T:` line into a `TriggerDefinition`.
///
/// Forge trigger format uses `Mode$` for the trigger type, `Execute$` for the
/// SVar containing the effect, `ValidCard$` for filters, etc.
pub(crate) fn translate_trigger(
    line: &ForgeAbilityLine,
    resolver: &mut SvarResolver,
) -> Result<TriggerDefinition, ForgeTranslateError> {
    let params = &line.params;

    // CR 603.1: Map Forge Mode$ to TriggerMode.
    let mode_str = params
        .get("Mode")
        .ok_or_else(|| ForgeTranslateError::MissingParam {
            param: "Mode".to_string(),
            context: line.raw.clone(),
        })?;
    let mode = translate_trigger_mode(mode_str)?;

    let mut trigger = TriggerDefinition::new(mode);

    // Resolve Execute$ → ability chain
    if let Some(exec_name) = params.get("Execute") {
        match resolver.resolve_ability(exec_name) {
            Ok(ability) => {
                trigger.execute = Some(Box::new(ability));
            }
            Err(_) => {
                // Graceful degradation — trigger mode parsed but execute failed
            }
        }
    }

    if matches!(trigger.mode, TriggerMode::Exploited) {
        if params.has("InvertValidSource") || params.has("InvertValidCard") {
            return Err(ForgeTranslateError::Other(
                "inverted Exploited filters are unsupported".to_string(),
            ));
        }

        trigger.valid_source = Some(match params.get("ValidSource") {
            Some(filter_str) => translate_filter(validate_exploited_filter(filter_str)?)?,
            None => TargetFilter::Any,
        });
        trigger.valid_card = params
            .get("ValidCard")
            .map(validate_exploited_filter)
            .transpose()?
            .map(translate_filter)
            .transpose()?;
    } else if let Some(filter_str) = params.get("ValidCard") {
        // ValidCard$ → valid_card filter for non-Exploited modes.
        if let Ok(filter) = translate_filter(filter_str) {
            trigger.valid_card = Some(filter);
        }
    }

    // TriggerZones$ → trigger_zones
    if let Some(zones_str) = params.get("TriggerZones") {
        trigger.trigger_zones = parse_zone_list(zones_str);
    }

    // Origin$ → origin zone (for ChangesZone triggers)
    if let Some(origin_str) = params.get("Origin") {
        trigger.origin = parse_zone_single(origin_str);
    }

    // Destination$ → destination zone (for ChangesZone triggers)
    if let Some(dest_str) = params.get("Destination") {
        trigger.destination = parse_zone_single(dest_str);
    }

    // OptionalDecider$ → optional
    if params.has("OptionalDecider") {
        trigger.optional = true;
    }

    // Secondary$ → secondary
    if params.has("Secondary") || params.get("Secondary") == Some("True") {
        trigger.secondary = true;
    }

    // ValidTarget$ → valid_target filter (for BecomesTarget triggers)
    if let Some(vt_str) = params.get("ValidTarget") {
        if let Ok(filter) = translate_filter(vt_str) {
            trigger.valid_target = Some(filter);
        }
    }

    // TriggerDescription$ → description
    if let Some(desc) = params.get("TriggerDescription") {
        trigger.description = Some(desc.to_string());
    }

    Ok(trigger)
}

/// Map Forge `Mode$` string to `TriggerMode`.
///
/// CR 603: Each trigger mode maps to a specific game event type.
fn translate_trigger_mode(mode: &str) -> Result<TriggerMode, ForgeTranslateError> {
    match mode {
        // Zone changes (CR 603.6)
        "ChangesZone" => Ok(TriggerMode::ChangesZone),
        "ChangesZoneAll" => Ok(TriggerMode::ChangesZoneAll),
        "ChangesController" => Ok(TriggerMode::ChangesController),
        "LeavesBattlefield" => Ok(TriggerMode::LeavesBattlefield),

        // Damage (CR 120)
        "DamageDone" => Ok(TriggerMode::DamageDone),
        "DamageDoneOnce" => Ok(TriggerMode::DamageDoneOnce),
        "DamageAll" => Ok(TriggerMode::DamageAll),
        "DamageDealtOnce" => Ok(TriggerMode::DamageDealtOnce),

        // Spells (CR 601.2i)
        "SpellCast" => Ok(TriggerMode::SpellCast),
        "SpellAbilityCast" => Ok(TriggerMode::SpellAbilityCast),
        "Countered" => Ok(TriggerMode::Countered),

        // Combat — attackers (CR 508.3)
        "Attacks" => Ok(TriggerMode::Attacks),
        "AttackersDeclared" => Ok(TriggerMode::AttackersDeclared),
        "AttackerBlocked" => Ok(TriggerMode::AttackerBlocked),
        "AttackerBlockedByCreature" => Ok(TriggerMode::AttackerBlockedByCreature),
        "AttackerUnblocked" => Ok(TriggerMode::AttackerUnblocked),
        "YouAttackUnblocked" => Ok(TriggerMode::YouAttackUnblocked),

        // Combat — blockers (CR 509)
        "Blocks" => Ok(TriggerMode::Blocks),
        "BlockersDeclared" => Ok(TriggerMode::BlockersDeclared),
        "BecomesBlocked" => Ok(TriggerMode::BecomesBlocked),

        // Counters (CR 122)
        "CounterAdded" => Ok(TriggerMode::CounterAdded),
        "CounterAddedOnce" => Ok(TriggerMode::CounterAddedOnce),
        "CounterRemoved" => Ok(TriggerMode::CounterRemoved),

        // Permanents
        "Sacrificed" => Ok(TriggerMode::Sacrificed),
        "Destroyed" => Ok(TriggerMode::Destroyed),
        "Taps" => Ok(TriggerMode::Taps),
        "Untaps" => Ok(TriggerMode::Untaps),

        // Targeting (CR 115)
        "BecomesTarget" => Ok(TriggerMode::BecomesTarget),
        "BecomesTargetOnce" => Ok(TriggerMode::BecomesTargetOnce),

        // Cards
        "Drawn" => Ok(TriggerMode::Drawn),
        "Discarded" => Ok(TriggerMode::Discarded),
        "Milled" => Ok(TriggerMode::Milled),
        "Exiled" => Ok(TriggerMode::Exiled),
        "Revealed" => Ok(TriggerMode::Revealed),

        // Life (CR 119)
        "LifeGained" => Ok(TriggerMode::LifeGained),
        "LifeLost" => Ok(TriggerMode::LifeLost),

        // Tokens (CR 111)
        "TokenCreated" => Ok(TriggerMode::TokenCreated),

        // Phase/Turn (CR 603.2b)
        "Phase" => Ok(TriggerMode::Phase),
        "TurnBegin" => Ok(TriggerMode::TurnBegin),
        "TurnFaceUp" => Ok(TriggerMode::TurnFaceUp),
        "Transformed" => Ok(TriggerMode::Transformed),

        // Monarch/Initiative
        "BecomeMonarch" => Ok(TriggerMode::BecomeMonarch),

        // Triggered mechanics
        "Cycled" => Ok(TriggerMode::Cycled),
        "Evolved" => Ok(TriggerMode::Evolved),
        "Explored" => Ok(TriggerMode::Explored),
        "Exploited" => Ok(TriggerMode::Exploited),
        "Crewed" => Ok(TriggerMode::Crewed),

        _ => Err(ForgeTranslateError::UnsupportedTriggerMode(
            mode.to_string(),
        )),
    }
}

fn parse_zone_list(zones_str: &str) -> Vec<Zone> {
    zones_str
        .split(',')
        .filter_map(|s| parse_zone_single(s.trim()))
        .collect()
}

fn parse_zone_single(s: &str) -> Option<Zone> {
    match s.trim() {
        "Battlefield" => Some(Zone::Battlefield),
        "Hand" => Some(Zone::Hand),
        "Graveyard" => Some(Zone::Graveyard),
        "Library" => Some(Zone::Library),
        "Exile" => Some(Zone::Exile),
        "Stack" => Some(Zone::Stack),
        "Command" => Some(Zone::Command),
        "Any" | "All" => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::database::forge::loader::parse_params;
    use crate::database::forge::types::ForgeAbilityLine;
    use crate::game::trigger_matchers::{test_trigger_source_context, trigger_matcher};
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn make_resolver(svars: &[(&str, &str)]) -> SvarResolver<'static> {
        let map: HashMap<String, String> = svars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        // Leak to get 'static lifetime for tests
        let leaked = Box::leak(Box::new(map));
        SvarResolver::new(leaked)
    }

    #[test]
    fn test_etb_trigger() {
        let raw =
            "Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield | ValidCard$ Card.Self | Execute$ TrigDraw | TriggerDescription$ When CARDNAME enters, draw a card.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let mut resolver = make_resolver(&[("TrigDraw", "DB$ Draw | NumCards$ 1")]);
        let trigger = translate_trigger(&line, &mut resolver).unwrap();

        assert_eq!(trigger.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
        assert!(trigger.execute.is_some());
    }

    #[test]
    fn test_attack_trigger() {
        let raw = "Mode$ Attacks | ValidCard$ Card.Self | Execute$ TrigToken | TriggerDescription$ Whenever CARDNAME attacks, create a Treasure token.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let mut resolver =
            make_resolver(&[("TrigToken", "DB$ Token | TokenScript$ c_a_treasure_sac")]);
        let trigger = translate_trigger(&line, &mut resolver).unwrap();

        assert_eq!(trigger.mode, TriggerMode::Attacks);
        assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn test_drawn_trigger() {
        let raw = "Mode$ Drawn | ValidCard$ Card.YouCtrl | TriggerZones$ Battlefield | Execute$ TrigGainLife | TriggerDescription$ Whenever you draw a card, you gain 2 life.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let mut resolver = make_resolver(&[(
            "TrigGainLife",
            "DB$ GainLife | Defined$ You | LifeAmount$ 2",
        )]);
        let trigger = translate_trigger(&line, &mut resolver).unwrap();

        assert_eq!(trigger.mode, TriggerMode::Drawn);
        assert_eq!(trigger.trigger_zones, vec![Zone::Battlefield]);
    }

    fn translate_exploited(raw: &str) -> Result<TriggerDefinition, ForgeTranslateError> {
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let mut resolver = make_resolver(&[("TrigDraw", "DB$ Draw | NumCards$ 1")]);
        translate_trigger(&line, &mut resolver)
    }

    #[test]
    fn exploited_roles_are_strict_and_lossless() {
        let trigger = translate_exploited(
            "Mode$ Exploited | ValidSource$ Creature.YouCtrl | ValidCard$ Creature.OppCtrl | Execute$ TrigDraw",
        )
        .unwrap();
        assert_eq!(
            trigger.valid_source,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
        assert_eq!(
            trigger.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent)
            ))
        );
        assert!(trigger.execute.is_some());

        let omitted = translate_exploited("Mode$ Exploited | Execute$ TrigDraw").unwrap();
        assert_eq!(omitted.valid_source, Some(TargetFilter::Any));
        assert_eq!(omitted.valid_card, None);

        for invalid in [
            "ValidSource$ ",
            "ValidSource$ Creature.!token",
            "ValidSource$ Creature.YouOwn",
            "ValidSource$ Creature.YouCtrl.OppCtrl",
            "ValidSource$ Creature,Card.Self",
            "ValidCard$ Creature.nonHuman",
            "ValidCard$ Creature.Unknown",
            "InvertValidSource$ True",
            "InvertValidCard$ True",
        ] {
            let raw = format!("Mode$ Exploited | {invalid} | Execute$ TrigDraw");
            assert!(translate_exploited(&raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn translated_exploited_roles_reach_the_registered_matcher() {
        let trigger = translate_exploited(
            "Mode$ Exploited | ValidSource$ Creature.YouCtrl | ValidCard$ Creature.OppCtrl | Execute$ TrigDraw",
        )
        .unwrap();
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Observer".to_string(),
            Zone::Battlefield,
        );
        let actor = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Exploiter".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        let friendly_victim = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Friendly Victim".to_string(),
            Zone::Battlefield,
        );
        for object_id in [actor, victim, friendly_victim] {
            let object = state.objects.get_mut(&object_id).unwrap();
            object.card_types.core_types.push(CoreType::Creature);
            object.base_card_types = object.card_types.clone();
        }

        let mut zone_events = Vec::new();
        move_to_zone(&mut state, victim, Zone::Graveyard, &mut zone_events);
        let record = zone_events
            .iter()
            .find_map(|event| match event {
                GameEvent::ZoneChanged { record, .. } => Some(record.clone()),
                _ => None,
            })
            .expect("the production move captured the victim record");
        let event = GameEvent::CreatureExploited {
            exploiter: actor,
            sacrificed: victim,
            record,
        };
        let matcher = trigger_matcher(TriggerMode::Exploited)
            .expect("Exploited has a registered runtime matcher");

        assert!(matcher(
            &event,
            &trigger,
            &test_trigger_source_context(&state, source),
            &state,
        ));

        state.objects.get_mut(&actor).unwrap().controller = PlayerId(1);
        assert!(!matcher(
            &event,
            &trigger,
            &test_trigger_source_context(&state, source),
            &state,
        ));

        state.objects.get_mut(&actor).unwrap().controller = PlayerId(0);
        let mut friendly_zone_events = Vec::new();
        move_to_zone(
            &mut state,
            friendly_victim,
            Zone::Graveyard,
            &mut friendly_zone_events,
        );
        let friendly_record = friendly_zone_events
            .iter()
            .find_map(|event| match event {
                GameEvent::ZoneChanged { record, .. } => Some(record.clone()),
                _ => None,
            })
            .expect("the production move captured the friendly victim record");
        let friendly_event = GameEvent::CreatureExploited {
            exploiter: actor,
            sacrificed: friendly_victim,
            record: friendly_record,
        };
        assert!(!matcher(
            &friendly_event,
            &trigger,
            &test_trigger_source_context(&state, source),
            &state,
        ));
    }
}
