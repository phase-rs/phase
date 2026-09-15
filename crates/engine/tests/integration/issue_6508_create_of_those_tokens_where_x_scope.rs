//! Issue #6508 follow-up: the count-bound where-X leaf that "create X of those
//! tokens" uses must bind a third-person "they control" to the scoped player,
//! through the real Oracle pipeline (parse → "those tokens" rewrite → lowered
//! token count), not only through a hand-built `Effect::unimplemented`.
//!
//! No printed card reaches this leaf with a third-person count, so the fixture
//! is The Final Days' verbatim Oracle text with ONLY its where-X count swapped
//! for a controller-relative one. The swap is a grammar discriminator, not a
//! card claim: "they control" must lower to `ScopedPlayer` while the paired
//! "you control" must stay `You`, and each case is the other's reach-guard.
//!
//! A runtime assertion would not discriminate here: this is a spell, which binds
//! no scoped player, so `ScopedPlayer` resolves to the caster exactly like `You`.
//! The runtime binding of `ScopedPlayer` itself is pinned by
//! `issue_6508_citadel_of_pain_each_players_end_step`.

use engine::parser::oracle::parse_oracle_text;

// Verbatim Oracle text (Scryfall, 2026-09-15).
const THE_FINAL_DAYS: &str = "Create two tapped 2/2 black Horror creature tokens. If this spell was cast from a graveyard, instead create X of those tokens, where X is the number of creature cards in your graveyard.\nFlashback {4}{B}{B} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

const ORIGINAL_COUNT: &str = "the number of creature cards in your graveyard";

/// Every `ObjectCount` controller found in a `Token` effect's `count`, anywhere in
/// the parsed abilities. Walking the serialized tree keeps the test independent
/// of where the "instead" rewrite nests the overriding token clause.
fn token_count_controllers(oracle: &str) -> Vec<String> {
    let parsed = parse_oracle_text(oracle, "The Final Days", &[], &["Sorcery".to_string()], &[]);
    let tree = serde_json::to_value(&parsed.abilities).expect("abilities serialize");
    let mut found = Vec::new();
    walk(&tree, &mut found);
    found
}

fn walk(node: &serde_json::Value, found: &mut Vec<String>) {
    match node {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(|t| t.as_str()) == Some("Token") {
                if let Some(controller) = map
                    .get("count")
                    .and_then(|count| count.get("qty"))
                    .filter(|qty| qty.get("type").and_then(|t| t.as_str()) == Some("ObjectCount"))
                    .and_then(|qty| qty.pointer("/filter/controller"))
                    .and_then(|c| c.as_str())
                {
                    found.push(controller.to_string());
                }
            }
            map.values().for_each(|v| walk(v, found));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, found)),
        _ => {}
    }
}

#[test]
fn create_of_those_tokens_where_x_binds_they_to_the_scoped_player_through_the_parser() {
    assert!(
        THE_FINAL_DAYS.contains(ORIGINAL_COUNT),
        "fixture must still carry the verbatim count"
    );

    let they = THE_FINAL_DAYS.replace(ORIGINAL_COUNT, "the number of creatures they control");
    let you = THE_FINAL_DAYS.replace(ORIGINAL_COUNT, "the number of creatures you control");

    assert_eq!(
        token_count_controllers(&you),
        vec!["You".to_string()],
        "reach-guard: the \"you control\" variant must reach the rewritten token count as You"
    );
    assert_eq!(
        token_count_controllers(&they),
        vec!["ScopedPlayer".to_string()],
        "\"they control\" in the create-of-those-tokens where-X count must lower to ScopedPlayer"
    );
}
