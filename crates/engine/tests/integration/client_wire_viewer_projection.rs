//! `ClientGameStateRef::wrap` serializes the viewer projection it derives its views from:
//! the same envelope `wrap_filtered` produces from that viewer's filter output, for every
//! seat and for the unseated audience.

use std::io::Read;

use engine::game::derived_views::ClientGameStateRef;
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::game_state::{GameState, PersistedGameState};
use engine::types::player::PlayerId;

const HIDDEN_CARD_NAME: &str = "Hidden Card";

fn load_dump() -> GameState {
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        include_bytes!("../fixtures/dina_conqueror_4p.json.gz").as_slice(),
    )
    .read_to_string(&mut json)
    .expect("fixture .json.gz inflates to UTF-8 JSON");
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        .expect("persisted snapshot satisfies the checked restore contract")
}

#[test]
fn wrap_serializes_the_viewer_projection_for_every_audience() {
    let state = load_dump();
    let mut audiences: Vec<Option<PlayerId>> =
        state.players.iter().map(|player| Some(player.id)).collect();
    audiences.push(None);
    assert!(audiences.len() > 2, "the fixture seats several players");

    for viewer in audiences {
        let projection = match viewer {
            Some(viewer) => filter_state_for_viewer(&state, viewer),
            None => filter_state_for_unseated_viewer(&state),
        };
        let wrapped = serde_json::to_value(ClientGameStateRef::wrap(&state, viewer))
            .expect("wrap envelope serializes");
        let reference = serde_json::to_value(ClientGameStateRef::wrap_filtered(
            &state,
            &projection,
            viewer,
        ))
        .expect("wrap_filtered envelope serializes");
        // Rendered through `serde_json::Value`: `DerivedViews` holds std `HashMap`s whose
        // direct serialization order varies between calls.
        assert!(
            serde_json::to_string(&wrapped).unwrap() == serde_json::to_string(&reference).unwrap(),
            "{viewer:?}: wrap must serialize the projection wrap_filtered serializes"
        );

        let mut hidden = 0;
        for (id, projected) in projection.objects.iter() {
            if projected.name != HIDDEN_CARD_NAME || state.objects[id].name == HIDDEN_CARD_NAME {
                continue;
            }
            assert_eq!(
                wrapped["state"]["objects"][id.0.to_string()]["name"],
                HIDDEN_CARD_NAME,
                "{viewer:?}: object {id:?}'s identity must not reach the wire"
            );
            hidden += 1;
        }
        assert!(
            hidden > 0,
            "{viewer:?}: the projection hides identities the raw state names"
        );
    }
}
