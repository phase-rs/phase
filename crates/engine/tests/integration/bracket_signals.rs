//! `oracle-gen data` must stamp `bracket_signals` onto every card. Game
//! Changers come from MTGJSON `isGameChanger`; the other axes come from
//! `data/bracket_lists.json`. This test verifies a known Game Changer
//! (Smothering Tithe), a known MLD (Armageddon), a known extra-turn card
//! (Time Warp), a known tutor (Demonic Tutor), and a clean card (Llanowar
//! Elves).

use std::path::PathBuf;
use std::process::Command;

#[test]
fn oracle_gen_stamps_bracket_signals() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let out = tmp.path().join("card-data.json");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "--features",
            "cli",
            "--bin",
            "oracle-gen",
            "--",
            "data",
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
