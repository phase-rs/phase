//! Standalone browser boundary around the Phase Oracle parser and unsupported-node audit.
//! This reports parser support, not official Oracle legality or intent fidelity.
use engine::game::coverage::card_face_has_unimplemented_parts;
use engine::parser::oracle_util::strip_reminder_text;
use engine::parser::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::card::CardFace;
use engine::types::card_type::CardType;
use engine::types::keywords::Keyword;
use serde::Deserialize;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

#[derive(Deserialize)]
struct Input {
    name: String,
    oracle_text: String,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    subtypes: Vec<String>,
}

fn check(input: Input) -> Value {
    // Phase expects MTGJSON keyword bookkeeping. Derive only whole keyword
    // phrases that round-trip through its own Keyword reader/display; no fuzzy
    // word search and no synthesized parameter defaults enter the metadata.
    let metadata: Vec<Keyword> = strip_reminder_text(&input.oracle_text)
        .lines()
        .flat_map(|s| s.split(','))
        .filter_map(|part| {
            let phrase = part.trim();
            let keyword = phrase.parse::<Keyword>().ok()?;
            if !matches!(keyword, Keyword::Unknown(_))
                && keyword.to_string().eq_ignore_ascii_case(phrase)
            {
                Some(keyword)
            } else {
                None
            }
        })
        .collect();
    let names: Vec<_> = metadata
        .iter()
        .map(|k| k.to_string().to_lowercase())
        .collect();
    let mut parsed = parse_oracle_text(
        &input.oracle_text,
        &input.name,
        &names,
        &input.types,
        &input.subtypes,
    );
    for keyword in metadata {
        if !parsed.extracted_keywords.contains(&keyword) {
            parsed.extracted_keywords.push(keyword);
        }
    }
    // This permission cannot preserve a library-top source restriction in CastFromZone.
    // Keep the known lossy fallback visible instead of a clean compatibility mark.
    let permission_loss = input.oracle_text.to_lowercase().contains("from the top of")
        && parsed
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::CastFromZone { .. }));
    let diagnostics: Vec<_> = parsed
        .parse_warnings
        .iter()
        .map(|d| json!({"severity": d.severity(), "diagnostic": d}))
        .collect();
    let recognized = !parsed.abilities.is_empty()
        || !parsed.triggers.is_empty()
        || !parsed.statics.is_empty()
        || !parsed.replacements.is_empty()
        || !parsed.extracted_keywords.is_empty()
        || parsed.additional_cost.is_some()
        || !parsed.casting_restrictions.is_empty()
        || !parsed.casting_options.is_empty();
    let face = CardFace {
        name: input.name,
        oracle_text: Some(input.oracle_text.clone()),
        card_type: CardType {
            supertypes: input.types.iter().filter_map(|s| s.parse().ok()).collect(),
            core_types: input.types.iter().filter_map(|s| s.parse().ok()).collect(),
            subtypes: input.subtypes,
        },
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        keywords: parsed.extracted_keywords,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
        strive_cost: parsed.strive_cost,
        parse_warnings: parsed.parse_warnings,
        ..Default::default()
    };
    let unsupported = card_face_has_unimplemented_parts(&face);
    let status = if input.oracle_text.trim().is_empty() {
        "empty"
    } else if unsupported || !recognized {
        "partial_or_unsupported"
    } else if !diagnostics.is_empty() || permission_loss {
        "needs_review"
    } else {
        "parsed"
    };
    json!({"status": status, "parse_complete": status == "parsed",
           "has_unimplemented_parts": unsupported, "diagnostics": diagnostics,
           "scope": "parser", "runtime_checked": false, "keyword_metadata": names,
           "representation_warning": if permission_loss {
               Some("The parsed permission does not preserve its library-top source restriction.")
           } else { None },
           "oracle_correctness": "not_proven", "intent_fidelity": "not_checked"})
}

/// Parse one face and audit unsupported nodes and parser warnings. No runtime or database is loaded.
#[wasm_bindgen(js_name = checkOracleCard)]
pub fn check_oracle_card(input: &str) -> String {
    if input.len() > 24000 {
        return json!({"status":"invalid_input","parse_complete":false,"error":"Input exceeds 24 KB."}).to_string();
    }
    match serde_json::from_str::<Input>(input) {
        Ok(input) if !input.name.trim().is_empty() && input.name.len() <= 800 => check(input).to_string(),
        _ => json!({"status":"invalid_input","parse_complete":false,"error":"Expected name, oracle_text, types and subtypes."}).to_string(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn probe(text: &str) -> Value {
        serde_json::from_str(&check_oracle_card(
            &json!({"name":"Probe","oracle_text":text,"types":["Sorcery"]}).to_string(),
        ))
        .unwrap()
    }
    #[test]
    fn checks_supported_and_unsupported_text() {
        assert_eq!(probe("Draw two cards.")["parse_complete"], true);
        assert_eq!(
            probe("Draw two cards. Gobble the moon.")["parse_complete"],
            false
        );
        assert_eq!(probe("")["status"], "empty");
        assert_eq!(
            probe("You may play cards from the top of each opponent's library.")["status"],
            "needs_review"
        );
        assert!(check_oracle_card("{}").contains("invalid_input"));
    }

    #[test]
    fn keyword_metadata_ignores_reminder_text() {
        for text in [
            "Flying",
            "Flying (This creature can't be blocked except by creatures with flying or reach.)",
        ] {
            let result: Value = serde_json::from_str(&check_oracle_card(
                &json!({"name":"Probe","oracle_text":text,"types":["Creature"]}).to_string(),
            ))
            .unwrap();
            assert_eq!(result["keyword_metadata"], json!(["flying"]));
            assert_eq!(result["parse_complete"], true);
        }
    }
}
