use crate::types::ability::ReplacementDefinition;
use crate::types::replacements::ReplacementEvent;

use super::filter::translate_filter;
use super::svar::SvarResolver;
use super::types::{ForgeAbilityLine, ForgeTranslateError};

/// Translate a Forge `R:` line into a `ReplacementDefinition`.
///
/// Forge replacement format uses `Event$` for the event type, `ReplaceWith$`
/// for the SVar containing the replacement effect.
pub(crate) fn translate_replacement(
    line: &ForgeAbilityLine,
    resolver: &mut SvarResolver,
) -> Result<ReplacementDefinition, ForgeTranslateError> {
    let params = &line.params;

    // CR 614.1: Map Forge Event$ to ReplacementEvent.
    let event_str = params
        .get("Event")
        .ok_or_else(|| ForgeTranslateError::MissingParam {
            param: "Event".to_string(),
            context: line.raw.clone(),
        })?;
    let event = translate_replacement_event(event_str)?;

    let mut def = ReplacementDefinition::new(event);

    // ReplaceWith$ → execute (resolve SVar)
    if let Some(replace_name) = params.get("ReplaceWith") {
        match resolver.resolve_ability(replace_name) {
            Ok(ability) => {
                def.execute = Some(Box::new(ability));
            }
            Err(_) => {
                // Graceful degradation — event type parsed but replacement failed
            }
        }
    }

    // ValidCard$ → valid_card filter
    if let Some(filter_str) = params.get("ValidCard") {
        if let Ok(filter) = translate_filter(filter_str) {
            def.valid_card = Some(filter);
        }
    }

    // ValidToken$ → valid_card filter (for CreateToken events)
    if let Some(filter_str) = params.get("ValidToken") {
        if let Ok(filter) = translate_filter(filter_str) {
            def.valid_card = Some(filter);
        }
    }

    // Description$ → description
    if let Some(desc) = params.get("Description") {
        def.description = Some(desc.to_string());
    }

    Ok(def)
}

/// Map Forge `Event$` string to `ReplacementEvent`.
///
/// CR 614.1: Each replacement event maps to a specific game event type.
fn translate_replacement_event(event: &str) -> Result<ReplacementEvent, ForgeTranslateError> {
    match event {
        // CR 614.1a: Damage replacement
        "DamageDone" => Ok(ReplacementEvent::DamageDone),
        "DealtDamage" => Ok(ReplacementEvent::DealtDamage),
        // CR 614.8: Destruction replacement
        "Destroy" => Ok(ReplacementEvent::Destroy),
        // CR 614.1a: Discard replacement
        "Discard" => Ok(ReplacementEvent::Discard),
        // CR 614.11: Draw replacement
        "Draw" => Ok(ReplacementEvent::Draw),
        // CR 614.1a: Life loss replacement
        "LoseLife" => Ok(ReplacementEvent::LoseLife),
        // CR 614.1a: Life gain replacement
        "GainLife" => Ok(ReplacementEvent::GainLife),
        // CR 614.12: Zone change replacement
        "ChangeZone" | "Moved" => Ok(ReplacementEvent::ChangeZone),
        // CR 614.1a: Counter placement replacement
        "AddCounter" => Ok(ReplacementEvent::AddCounter),
        // CR 614.1a: Counter removal replacement
        "RemoveCounter" => Ok(ReplacementEvent::RemoveCounter),
        // CR 614.1a: Token creation replacement
        "CreateToken" => Ok(ReplacementEvent::CreateToken),
        // CR 614.1a: Tap replacement
        "Tap" => Ok(ReplacementEvent::Tap),
        // CR 614.1a: Untap replacement
        "Untap" => Ok(ReplacementEvent::Untap),
        // CR 614.1e: Turn face up replacement
        "TurnFaceUp" => Ok(ReplacementEvent::TurnFaceUp),
        // CR 614.1a: Counter replacement
        "Counter" => Ok(ReplacementEvent::Counter),

        _ => Err(ForgeTranslateError::UnsupportedReplacementEvent(
            event.to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::forge::loader::parse_params;
    use crate::database::forge::types::ForgeAbilityLine;
    use std::collections::HashMap;

    fn make_resolver(svars: &[(&str, &str)]) -> SvarResolver<'static> {
        let map: HashMap<String, String> = svars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let leaked = Box::leak(Box::new(map));
        SvarResolver::new(leaked)
    }

    #[test]
    fn test_create_token_replacement() {
        let raw = "Event$ CreateToken | ActiveZones$ Battlefield | ValidToken$ Card.YouCtrl | ReplaceWith$ DoubleToken | EffectOnly$ True | Description$ If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let mut resolver = make_resolver(&[("DoubleToken", "DB$ ReplaceToken | Type$ Amount")]);
        let def = translate_replacement(&line, &mut resolver).unwrap();

        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn test_zone_change_replacement() {
        let raw = "Event$ Moved | ValidCard$ Card.Self | Destination$ Graveyard | ReplaceWith$ Exile | Description$ If CARDNAME would be put into a graveyard from anywhere, exile it instead.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let mut resolver = make_resolver(&[("Exile", "DB$ ChangeZone | Hidden$ True | Origin$ All | Destination$ Exile | Defined$ ReplacedCard")]);
        let def = translate_replacement(&line, &mut resolver).unwrap();

        assert!(matches!(def.event, ReplacementEvent::ChangeZone));
    }
}
