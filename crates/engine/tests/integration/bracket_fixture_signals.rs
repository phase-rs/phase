//! Pins the committed integration fixture's current Commander bracket signals.

use engine::database::{BracketSignals, CardDatabase};

const AGGRAVATED_ASSAULT: &str = "Aggravated Assault";
const ANCIENT_TOMB: &str = "Ancient Tomb";
const ENLIGHTENED_TUTOR: &str = "Enlightened Tutor";
const NEXUS_OF_FATE: &str = "Nexus of Fate";
const OBLITERATE: &str = "Obliterate";
const TERGRID_GOD_OF_FRIGHT: &str = "Tergrid, God of Fright";
const TERGRIDS_LANTERN: &str = "Tergrid's Lantern";
const WISHCLAW_TALISMAN: &str = "Wishclaw Talisman";

fn shared_db() -> Option<&'static CardDatabase> {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: committed integration card fixture is unavailable");
        return None;
    };
    Some(db)
}

fn fixture_signals(db: &CardDatabase, name: &str) -> Option<BracketSignals> {
    if db.get_face_by_name(name).is_none() {
        eprintln!(
            "skipping: {name} is not in integration_cards.json.gz — regenerate the committed fixture"
        );
        return None;
    }
    Some(db.bracket_signals_for(name))
}

#[test]
fn known_game_changer_reads_true_through_database() {
    let Some(db) = shared_db() else {
        return;
    };
    let Some(signals) = fixture_signals(db, ANCIENT_TOMB) else {
        return;
    };

    assert!(
        signals.game_changer,
        "Ancient Tomb must retain its Game Changer signal in the committed fixture"
    );
}

#[test]
fn committed_fixture_represents_all_four_curated_axes() {
    let Some(db) = shared_db() else {
        return;
    };
    let Some(game_changer) = fixture_signals(db, ANCIENT_TOMB) else {
        return;
    };
    let Some(extra_turn) = fixture_signals(db, NEXUS_OF_FATE) else {
        return;
    };
    let Some(efficient_tutor) = fixture_signals(db, WISHCLAW_TALISMAN) else {
        return;
    };
    let Some(mass_land_denial) = fixture_signals(db, OBLITERATE) else {
        return;
    };

    assert!(game_changer.game_changer, "Game Changer axis disappeared");
    assert!(extra_turn.extra_turn, "extra-turn axis disappeared");
    assert!(
        efficient_tutor.efficient_tutor,
        "efficient-tutor axis disappeared"
    );
    assert!(
        mass_land_denial.mass_land_denial,
        "mass-land-denial axis disappeared"
    );
}

#[test]
fn aggravated_assault_reads_as_extra_combat_evidence_not_an_extra_turn() {
    let Some(db) = shared_db() else {
        return;
    };
    let Some(signals) = fixture_signals(db, AGGRAVATED_ASSAULT) else {
        return;
    };

    // The correction landed in 61-02 and reached the committed fixture in 61-06.
    assert!(
        !signals.extra_turn,
        "Aggravated Assault must not be classified as an extra-turn card"
    );
}

#[test]
fn enlightened_tutor_currently_counts_on_two_axes() {
    let Some(db) = shared_db() else {
        return;
    };
    let Some(signals) = fixture_signals(db, ENLIGHTENED_TUTOR) else {
        return;
    };

    // KNOWN DEFECT: step 61-02 records this overlap, and step 61-04 retires
    // the tutor floor after WotC removed tutor restrictions on 2025-10-21.
    assert!(
        signals.efficient_tutor,
        "current baseline classifies Enlightened Tutor as an efficient tutor"
    );
    assert!(
        signals.game_changer,
        "current baseline also classifies Enlightened Tutor as a Game Changer"
    );
}

#[test]
fn tergrid_modal_dfc_currently_flags_both_faces() {
    let Some(db) = shared_db() else {
        return;
    };
    let Some(front) = fixture_signals(db, TERGRID_GOD_OF_FRIGHT) else {
        return;
    };
    let Some(back) = fixture_signals(db, TERGRIDS_LANTERN) else {
        return;
    };

    // KNOWN ARTIFACT: 29 flagged faces represent 28 official cards because
    // both Tergrid faces carry the flag. The Game Changers axis forces Bracket
    // 3 at >=1 and Bracket 4 at >=4, so face-level counting would double-count
    // this modal DFC.
    assert!(
        front.game_changer,
        "Tergrid's front face must retain its Game Changer signal"
    );
    assert!(
        back.game_changer,
        "Tergrid's back face must retain its Game Changer signal"
    );
}
