//! `oracle-gen data` must stamp `bracket_signals` onto every card. Game
//! Changers come from MTGJSON `isGameChanger`; the other axes come from
//! `data/bracket_lists.json`. This test verifies a known Game Changer
//! (Smothering Tithe), a known MLD (Armageddon), a known extra-turn card
//! (Time Warp), a known tutor (Demonic Tutor), and a clean card (Llanowar
//! Elves).

use std::fs;
use std::process::Command;

#[test]
fn oracle_gen_stamps_bracket_signals() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let data_dir = tmp.path().join("data");
    let mtgjson_dir = data_dir.join("mtgjson");
    let out = tmp.path().join("card-data.json");
    fs::create_dir_all(&mtgjson_dir).expect("create mtgjson dir");
    fs::write(
        data_dir.join("bracket_lists.json"),
        r#"{
            "version": "test",
            "source": "test fixture",
            "mass_land_denial": ["Armageddon"],
            "extra_turns": ["Time Warp"],
            "efficient_tutors": ["Demonic Tutor"]
        }"#,
    )
    .expect("write bracket lists");
    fs::write(
        mtgjson_dir.join("AtomicCards.json"),
        r#"{
            "data": {
                "Armageddon": [{
                    "name": "Armageddon",
                    "manaCost": "{3}{W}",
                    "colors": ["W"],
                    "colorIdentity": ["W"],
                    "layout": "normal",
                    "manaValue": 4.0,
                    "type": "Sorcery",
                    "types": ["Sorcery"],
                    "identifiers": { "scryfallOracleId": "00000000-0000-0000-0000-000000000001" }
                }],
                "Demonic Tutor": [{
                    "name": "Demonic Tutor",
                    "manaCost": "{1}{B}",
                    "colors": ["B"],
                    "colorIdentity": ["B"],
                    "layout": "normal",
                    "manaValue": 2.0,
                    "type": "Sorcery",
                    "types": ["Sorcery"],
                    "isGameChanger": true,
                    "identifiers": { "scryfallOracleId": "00000000-0000-0000-0000-000000000002" }
                }],
                "Llanowar Elves": [{
                    "name": "Llanowar Elves",
                    "manaCost": "{G}",
                    "colors": ["G"],
                    "colorIdentity": ["G"],
                    "layout": "normal",
                    "manaValue": 1.0,
                    "type": "Creature",
                    "types": ["Creature"],
                    "subtypes": ["Elf", "Druid"],
                    "power": "1",
                    "toughness": "1",
                    "identifiers": { "scryfallOracleId": "00000000-0000-0000-0000-000000000003" }
                }],
                "Smothering Tithe": [{
                    "name": "Smothering Tithe",
                    "manaCost": "{3}{W}",
                    "colors": ["W"],
                    "colorIdentity": ["W"],
                    "layout": "normal",
                    "manaValue": 4.0,
                    "type": "Enchantment",
                    "types": ["Enchantment"],
                    "isGameChanger": true,
                    "identifiers": { "scryfallOracleId": "00000000-0000-0000-0000-000000000004" }
                }],
                "Time Warp": [{
                    "name": "Time Warp",
                    "manaCost": "{3}{U}{U}",
                    "colors": ["U"],
                    "colorIdentity": ["U"],
                    "layout": "normal",
                    "manaValue": 5.0,
                    "type": "Sorcery",
                    "types": ["Sorcery"],
                    "identifiers": { "scryfallOracleId": "00000000-0000-0000-0000-000000000005" }
                }]
            }
        }"#,
    )
    .expect("write atomic cards");

    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "--features",
            "cli",
            "--bin",
            "oracle-gen",
            "--",
            data_dir.to_str().unwrap(),
            "--filter",
            "Smothering Tithe|Armageddon|Time Warp|Demonic Tutor|Llanowar Elves",
            "--output",
            out.to_str().unwrap(),
        ])
        .current_dir(&repo_root)
        .status()
        .expect("run oracle-gen");
    assert!(status.success());

    let raw = std::fs::read_to_string(&out).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();

    let tithe = &json["smothering tithe"]["bracket_signals"];
    assert_eq!(tithe["game_changer"], true);
    assert_eq!(tithe["efficient_tutor"], false);

    let armageddon = &json["armageddon"]["bracket_signals"];
    assert_eq!(armageddon["mass_land_denial"], true);

    let time_warp = &json["time warp"]["bracket_signals"];
    assert_eq!(time_warp["extra_turn"], true);

    let demonic = &json["demonic tutor"]["bracket_signals"];
    assert_eq!(demonic["efficient_tutor"], true);
    assert_eq!(demonic["game_changer"], true);

    let llanowar = &json["llanowar elves"].get("bracket_signals");
    // When all four signals are false, serde may skip the field entirely
    // (see `skip_serializing_if = is_clean_signals`) — either absent or all-false is acceptable.
    if let Some(sig) = llanowar {
        assert_eq!(sig["game_changer"], false);
        assert_eq!(sig["mass_land_denial"], false);
        assert_eq!(sig["extra_turn"], false);
        assert_eq!(sig["efficient_tutor"], false);
    }
}
