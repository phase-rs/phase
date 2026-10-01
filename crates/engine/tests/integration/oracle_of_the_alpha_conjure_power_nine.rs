//! Oracle of the Alpha — the collective-name conjure resolves end to end.
//!
//! Verbatim Oracle text:
//!   Flying
//!   When Oracle of the Alpha enters the battlefield, conjure the Power Nine
//!   into your library, then shuffle.
//!   Whenever Oracle of the Alpha attacks, scry 1.
//!
//! Conjure is digital-only with no Comprehensive Rules entry; the surrounding
//! chain (enters-the-battlefield trigger, library zone, shuffle) is existing
//! behavior exercised here through the normal cast pipeline. The attacks/scry
//! clause is untouched by this change (covered by existing scry tests).

use crate::support::shared_card_db;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ORACLE_OF_THE_ALPHA_ORACLE: &str = "Flying\n\
    When Oracle of the Alpha enters the battlefield, conjure the Power Nine into your library, then shuffle.\n\
    Whenever Oracle of the Alpha attacks, scry 1.";

const POWER_NINE: [&str; 9] = [
    "Ancestral Recall",
    "Black Lotus",
    "Mox Pearl",
    "Mox Sapphire",
    "Mox Jet",
    "Mox Ruby",
    "Mox Emerald",
    "Time Walk",
    "Timetwister",
];

fn library_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .library
        .len()
}

fn library_names(state: &GameState, player: PlayerId) -> Vec<String> {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .library
        .iter()
        .map(|id| state.objects[id].name.clone())
        .collect()
}

/// Casting Oracle of the Alpha conjures exactly the 9 Power Nine into the
/// controller's library, then the chained shuffle resolves (the full chain
/// drains back to priority). The opponent's library is untouched, proving the
/// "your library" recipient binds the controller only.
///
/// Pre-fix this input parses the conjure head to `Unimplemented` (conjuring
/// nothing while the shuffle still runs), so the +9 delta fails on revert.
#[test]
fn oracle_of_the_alpha_etb_conjures_power_nine_into_library_then_shuffles() {
    let Some(db) = shared_card_db() else {
        return;
    };
    // Precondition: the fixture carries every Power Nine face, so the conjured
    // cards resolve with real characteristics rather than nameless shells.
    for name in POWER_NINE {
        assert!(
            db.get_face_by_name(name).is_some(),
            "fixture must carry {name}"
        );
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oracle_id = scenario
        .add_creature_to_hand(P0, "Oracle of the Alpha", 2, 3)
        .from_oracle_text_with_keywords(&["Flying"], ORACLE_OF_THE_ALPHA_ORACLE)
        .id();
    let mut runner = scenario.build();

    // Seed the outside-the-game face registry with the 9 real faces. The
    // scenario-built Oracle carries no printed ref, so the printed-face walker
    // cannot seed them — these are the same faces production rehydration
    // installs for a deck that contains this card.
    for name in POWER_NINE {
        let face = db.get_face_by_name(name).expect("fixture carries the face");
        std::sync::Arc::make_mut(&mut runner.state_mut().card_face_registry)
            .insert(name.to_lowercase(), face.clone());
    }

    let p0_library_before = library_len(runner.state(), P0);
    let p1_library_before = library_len(runner.state(), P1);

    let outcome = runner.cast(oracle_id).resolve();
    outcome.assert_zone(&[oracle_id], Zone::Battlefield);

    // The Conjure→Shuffle chain drains fully: no prompt left behind.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the ETB chain must resolve back to priority, got {:?}",
        outcome.final_waiting_for()
    );

    // The ETB conjures exactly the 9 Power Nine into the controller's library.
    outcome.assert_zone_count(P0, Zone::Library, p0_library_before + 9);
    let p0_names = library_names(outcome.state(), P0);
    for name in POWER_NINE {
        assert_eq!(
            p0_names.iter().filter(|n| n.as_str() == name).count(),
            1,
            "controller library must contain exactly one {name}: {p0_names:?}"
        );
    }

    // Conjured cards are real cards, not tokens.
    for id in outcome
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("controller exists")
        .library
        .clone()
    {
        let obj = &outcome.state().objects[&id];
        if POWER_NINE.contains(&obj.name.as_str()) {
            assert!(!obj.is_token, "{} is a real card, not a token", obj.name);
        }
    }

    // Registry path, not nameless shells: a conjured Mox carries its printed
    // Artifact type from its real face.
    let lotus = outcome
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("controller exists")
        .library
        .iter()
        .map(|id| &outcome.state().objects[id])
        .find(|obj| obj.name == "Black Lotus")
        .expect("Black Lotus was conjured");
    assert!(
        lotus.card_types.core_types.contains(&CoreType::Artifact),
        "Black Lotus carries its printed Artifact type: {:?}",
        lotus.card_types
    );

    // Multi-authority hostile: the opponent's library is unchanged and holds
    // none of the 9 — "your library" binds the controller only.
    assert_eq!(
        library_len(outcome.state(), P1),
        p1_library_before,
        "opponent library is untouched by the controller-only conjure"
    );
    let p1_names = library_names(outcome.state(), P1);
    for name in POWER_NINE {
        assert!(
            !p1_names.contains(&name.to_string()),
            "opponent library must not contain {name}: {p1_names:?}"
        );
    }
}
