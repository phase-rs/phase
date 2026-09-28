use std::collections::HashSet;
use std::process::Command;

use engine::ai_deck_manifest::{AiDeckManifest, ManifestError};
use engine::game::bracket_estimate::{ComboDeclaration, CommanderBracketTier};

#[test]
fn bundled_manifest_parses_and_every_deck_is_commander_shaped() {
    let manifest = AiDeckManifest::bundled();
    assert!(!manifest.version.trim().is_empty());
    assert_eq!(manifest.decks.len(), 13);

    let mut ids = HashSet::new();
    for deck in manifest.commander_decks() {
        assert!(!deck.commander.is_empty(), "{} has no commander", deck.id);
        assert!(
            deck.main_deck.len() >= 60,
            "{} has only {} main-deck cards",
            deck.id,
            deck.main_deck.len()
        );
        assert!(
            ids.insert(deck.id.to_lowercase()),
            "duplicate id: {}",
            deck.id
        );
    }
}

#[test]
fn bundled_manifest_carries_exactly_three_declared_cedh_decks_and_no_declared_b4() {
    let manifest = AiDeckManifest::bundled();
    assert_eq!(
        manifest
            .decks
            .iter()
            .filter(|deck| deck.declared == Some(CommanderBracketTier::Cedh))
            .count(),
        3
    );
    assert!(!manifest
        .decks
        .iter()
        .any(|deck| deck.declared == Some(CommanderBracketTier::Optimized)));
}

#[test]
fn manifest_find_is_case_insensitive() {
    let manifest = AiDeckManifest::bundled();
    let expected = &manifest.decks[0];
    assert_eq!(
        manifest
            .find(&expected.id.to_uppercase())
            .map(|deck| &deck.id),
        Some(&expected.id)
    );
    assert_eq!(
        manifest
            .find(&expected.name.to_uppercase())
            .map(|deck| &deck.id),
        Some(&expected.id)
    );
}

#[test]
fn manifest_rejects_duplicate_ids_and_empty_commander() {
    let main_deck = vec!["Card"; 60];
    let entry = |id: &str, commander: Vec<&str>| {
        serde_json::json!({
            "id": id,
            "name": id,
            "commander": commander,
            "main_deck": main_deck,
            "declared": null,
            "source": "test",
            "source_date": "2026-09-27"
        })
    };

    let duplicate = serde_json::json!({
        "version": "test",
        "decks": [entry("Deck", vec!["Commander"]), entry("deck", vec!["Commander"])]
    });
    assert!(matches!(
        AiDeckManifest::from_json_str(&duplicate.to_string()),
        Err(ManifestError::DuplicateId(_))
    ));

    let empty = serde_json::json!({
        "version": "test",
        "decks": [entry("empty", Vec::new())]
    });
    assert!(matches!(
        AiDeckManifest::from_json_str(&empty.to_string()),
        Err(ManifestError::EmptyCommander(_))
    ));
}

#[test]
fn to_deck_data_carries_declared_tier_and_defaults_the_rest() {
    let manifest = AiDeckManifest::bundled();
    let declared = manifest
        .decks
        .iter()
        .find(|deck| deck.declared == Some(CommanderBracketTier::Cedh))
        .expect("bundled cEDH deck");
    let unlabelled = manifest
        .decks
        .iter()
        .find(|deck| deck.declared.is_none())
        .expect("unlabelled feed deck");

    let declared_data = declared.to_deck_data();
    assert_eq!(declared_data.bracket_tier, CommanderBracketTier::Cedh);
    assert_eq!(
        declared_data.combo_declaration,
        ComboDeclaration::Undeclared
    );
    assert!(declared_data.sideboard.is_empty());

    let unlabelled_data = unlabelled.to_deck_data();
    assert_eq!(unlabelled_data.bracket_tier, CommanderBracketTier::Core);
    assert_eq!(
        unlabelled_data.combo_declaration,
        ComboDeclaration::Undeclared
    );
}

#[test]
fn generator_output_is_byte_stable() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("engine crate is nested under the repository root");
    let python = match Command::new("python3").arg("--version").output() {
        Ok(output) if output.status.success() => "python3",
        Ok(_) | Err(_) => {
            eprintln!("skipping generator byte-stability check: python3 is unavailable");
            return;
        }
    };
    let status = Command::new(python)
        .args(["scripts/gen-ai-commander-decks.py", "--check"])
        .current_dir(root)
        .status()
        .expect("run manifest generator");
    assert!(
        status.success(),
        "generated manifest differs from committed file"
    );
}
