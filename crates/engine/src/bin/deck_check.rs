//! Evaluate a nonempty JSON array of `DeckCompatibilityRequest` values.
//!
//! Usage: `deck-check <card-data.json> <requests.json>`
//! Stdout is an ordered JSON array of unchanged `DeckCompatibilityResult` values.
//! Exit 0 means evaluation/output succeeded, including incompatible decks; input
//! or output errors are reported on stderr with exit 2. The caller supplies a
//! complete card-data export and chooses the format and `summary_only` flag.
//! Existing request defaults and null selected-format verdicts are preserved.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use engine::database::CardDatabase;
use engine::game::deck_validation::{
    evaluate_deck_compatibility, DeckCompatibilityRequest, DeckCompatibilityResult,
};

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("deck-check: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(mut args: impl Iterator<Item = OsString>, mut output: impl Write) -> Result<(), String> {
    let (Some(card_data_path), Some(requests_path), None) = (args.next(), args.next(), args.next())
    else {
        return Err("Usage: deck-check <card-data.json> <requests.json>".to_string());
    };
    let card_data_path = PathBuf::from(card_data_path);
    let requests_path = PathBuf::from(requests_path);
    let input = std::fs::read(&requests_path)
        .map_err(|error| format!("could not read {}: {error}", requests_path.display()))?;
    let requests: Vec<DeckCompatibilityRequest> = serde_json::from_slice(&input)
        .map_err(|error| format!("invalid requests in {}: {error}", requests_path.display()))?;
    if requests.is_empty() {
        return Err(format!(
            "{} must contain at least one request",
            requests_path.display()
        ));
    }
    let db = CardDatabase::from_export(&card_data_path)
        .map_err(|error| format!("could not load {}: {error}", card_data_path.display()))?;
    let results: Vec<DeckCompatibilityResult> = requests
        .iter()
        .map(|request| evaluate_deck_compatibility(&db, request))
        .collect();
    let mut json = serde_json::to_vec(&results)
        .map_err(|error| format!("could not serialize results: {error}"))?;
    json.push(b'\n');
    output
        .write_all(&json)
        .and_then(|()| output.flush())
        .map_err(|error| format!("could not write results: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    const CARD_DATA: &str = include_str!("../../tests/fixtures/runtime_card_export_fixture.json");

    fn input_files(card_data: &str, requests: &str) -> (TempDir, [OsString; 2]) {
        let dir = tempfile::tempdir().unwrap();
        let card_data_path = dir.path().join("card data.json");
        let requests_path = dir.path().join("requests é.json");
        std::fs::write(&card_data_path, card_data).unwrap();
        std::fs::write(&requests_path, requests).unwrap();
        (
            dir,
            [
                card_data_path.into_os_string(),
                requests_path.into_os_string(),
            ],
        )
    }

    #[test]
    fn preserves_order_and_complete_engine_results() {
        let input = json!([
            {
                "main_deck": vec!["Forest"; 60],
                "selected_format": "Standard",
                "summary_only": false
            },
            {
                "main_deck": ["Forest", "Unknown transport fixture"],
                "selected_format": "Commander",
                "player_count": 4,
                "summary_only": false
            },
            {
                "main_deck": vec!["Forest"; 60],
                "selected_format": "Standard",
                "summary_only": true
            },
            {
                "selected_format": "Commander",
                "player_count": 4,
                "summary_only": true
            },
            {}
        ]);
        let (_dir, paths) = input_files(CARD_DATA, &input.to_string());
        let mut output = Vec::new();
        run(paths.into_iter(), &mut output).unwrap();
        assert_eq!(output.last(), Some(&b'\n'));
        let actual: Vec<DeckCompatibilityResult> = serde_json::from_slice(&output).unwrap();
        assert_eq!(actual.len(), 5);
        assert_eq!(actual[0].selected_format_compatible, Some(true));
        assert!(actual[0].coverage.is_some());
        assert_eq!(actual[1].selected_format_compatible, Some(false));
        assert!(!actual[1].selected_format_reasons.is_empty());
        assert_eq!(actual[1].unknown_cards, ["Unknown transport fixture"]);
        assert!(actual[1].coverage.is_some());
        assert_eq!(actual[2].selected_format_compatible, Some(true));
        assert!(actual[2].coverage.is_none());
        assert_eq!(actual[3].selected_format_compatible, Some(false));
        assert!(!actual[3].selected_format_reasons.is_empty());
        assert!(actual[3].coverage.is_none());
        assert_eq!(actual[4].selected_format_compatible, None);

        let db = CardDatabase::from_json_str(CARD_DATA).unwrap();
        let requests: Vec<DeckCompatibilityRequest> = serde_json::from_value(input).unwrap();
        let expected: Vec<_> = requests
            .iter()
            .map(|request| evaluate_deck_compatibility(&db, request))
            .collect();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }

    #[test]
    fn rejects_malformed_request_batches_without_output() {
        for input in [
            "",
            "[",
            "{}",
            "null",
            "[null]",
            r#"[{"main_deck": "Forest"}]"#,
            r#"[{"selected_format": "not-a-format"}]"#,
            r#"[{"summary_only": "true"}]"#,
            r#"[{}, {"commander": [42]}]"#,
            "[{}] trailing",
        ] {
            let (_dir, paths) = input_files(CARD_DATA, input);
            let mut output = Vec::new();
            let error = run(paths.into_iter(), &mut output).unwrap_err();
            assert!(error.contains("invalid requests"), "{input:?}: {error}");
            assert!(output.is_empty(), "{input:?}");
        }
    }

    #[test]
    fn rejects_empty_request_batch_without_output() {
        let (_dir, paths) = input_files(CARD_DATA, "[]");
        let mut output = Vec::new();
        let error = run(paths.into_iter(), &mut output).unwrap_err();
        assert!(error.contains("at least one request"));
        assert!(output.is_empty());
    }

    #[test]
    fn requires_exactly_two_paths() {
        for args in [vec![], vec!["cards.json"], vec!["a", "b", "extra"]] {
            let mut output = Vec::new();
            let error = run(args.into_iter().map(OsString::from), &mut output).unwrap_err();
            assert!(error.contains("Usage: deck-check"));
            assert!(output.is_empty());
        }
    }

    #[test]
    fn reports_missing_input_path_without_output() {
        for index in [0, 1] {
            let (dir, mut paths) = input_files(CARD_DATA, "[{}]");
            let missing = dir.path().join("missing.json");
            paths[index] = missing.clone().into_os_string();
            let mut output = Vec::new();
            let error = run(paths.into_iter(), &mut output).unwrap_err();
            assert!(error.contains(&missing.display().to_string()));
            assert!(output.is_empty());
        }
    }

    #[test]
    fn rejects_malformed_card_data_without_output() {
        for card_data in ["[", "[]", r#"{"broken": {}}"#] {
            let (_dir, paths) = input_files(card_data, "[{}]");
            let mut output = Vec::new();
            let error = run(paths.into_iter(), &mut output).unwrap_err();
            assert!(error.contains("could not load"));
            assert!(output.is_empty());
        }
    }

    #[test]
    fn reports_output_failure() {
        let (_dir, paths) = input_files(CARD_DATA, "[{}]");
        let mut no_space = [0_u8; 0];
        let error = run(paths.into_iter(), no_space.as_mut_slice()).unwrap_err();
        assert!(error.contains("could not write results"));
    }
}
