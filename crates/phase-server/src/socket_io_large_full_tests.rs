//! Manual native fixtures for the client's large-Full integration proof.
//! Uses real spectator projection, guards, connection encoding and gzip, without
//! modifying production source or committing generated multi-megabyte frames.
use super::*;
use crate::game_executor::GameExecutor;
use engine::database::CardDatabase;
use engine::game::scenario::GameScenario;
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use server_core::game_state_snapshot_wire_guard::{
    guard_frame_bytes, MAX_SERVER_FRAME_BYTES, MAX_SNAPSHOT_OBJECTS,
};
use server_core::session::{FullRuntime, SessionManager};
use state_codec::{Frame, FrameKind};
use std::io::BufReader;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

async fn write_frame(
    executor: &GameExecutor,
    destination: &Path,
    name: &str,
    message: ServerMessage,
) -> serde_json::Value {
    let revision = match &message {
        ServerMessage::GameStarted { state_revision, .. }
        | ServerMessage::StateUpdate { state_revision, .. } => *state_revision,
        _ => unreachable!(),
    };
    let envelope = codec_tests::envelope(message, None);
    let expected = serde_json::to_vec(&envelope).unwrap();
    guard_frame_bytes(expected.len(), MAX_SERVER_FRAME_BYTES).unwrap();
    let outcome = encode_application(
        executor,
        ConnectionEncoder::codec(),
        envelope,
        Encoding::GzipEnvelope,
        MAX_SERVER_FRAME_BYTES,
    )
    .await
    .unwrap();
    let prepared = outcome.prepared.unwrap();
    assert!(prepared.candidate.is_some(), "real state baseline prepared");
    let Message::Binary(bytes) = prepared.frame else {
        panic!("expected native gzip envelope")
    };
    assert_eq!(bytes[0], 0x01);
    guard_frame_bytes(bytes.len(), MAX_SERVER_FRAME_BYTES).unwrap();
    let text = wire::decode_client_envelope(bytes.to_vec(), MAX_SERVER_FRAME_BYTES)
        .await
        .unwrap();
    guard_frame_bytes(text.len(), MAX_SERVER_FRAME_BYTES).unwrap();
    let frame = Frame::from_bytes(text.as_bytes()).unwrap();
    assert_eq!(frame.kind(), FrameKind::Full);
    let decoded = state_codec::decode(None, &frame).unwrap();
    assert_eq!(decoded.as_bytes(), expected);
    assert_eq!(decoded.revision(), Some(revision));
    std::fs::write(destination.join(format!("{name}.wire")), &bytes).unwrap();
    serde_json::json!({
        "wire_bytes": bytes.len(), "frame_utf16_units": text.encode_utf16().count(),
        "decoded_bytes": expected.len(), "revision": revision,
        "decoded_sha256": state_codec::state_hash(&decoded).to_string(),
    })
}

#[tokio::test]
#[ignore = "manual native spectator Full producer; writes fixed target/state-codec-fixtures"]
async fn produce_native_large_spectator_full() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let destination = manifest_dir.join("../../target/state-codec-fixtures/large-spectator-full");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::create_dir(&destination).expect("generated fixture directory must not already exist");
    let fixture = manifest_dir.join("../engine/tests/fixtures/integration_cards.json.gz");
    let db = CardDatabase::from_export_reader(flate2::read::GzDecoder::new(BufReader::new(
        std::fs::File::open(fixture).unwrap(),
    )))
    .unwrap();
    assert!(!db
        .get_face_by_name("Llanowar Elves")
        .unwrap()
        .abilities
        .is_empty());
    let executor = GameExecutor::new(NonZeroUsize::MIN);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut manager = SessionManager::new();
    let (game_code, _) = manager.create_game(Default::default(), None);
    let mut session = manager.try_session(&game_code).unwrap();
    session.full_runtime = Some(FullRuntime {
        key: server_core::FullSessionKey {
            game_code: game_code.clone(),
            generation: 1,
        },
        activation_epoch: None,
    });
    session.state = scenario.build().state().clone();
    session.state_revision = 99;
    session.game_started = true;
    let small = crate::build_spectator_game_started_message(&session).unwrap();
    let small_stats = write_frame(&executor, &destination, "small", small).await;

    // Scenario-constructed real permanents with their actual ability payloads;
    // not padded JSON, invented abilities, or a gameplay-reachability claim.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    const OBJECTS: usize = 9_000;
    const { assert!(OBJECTS <= MAX_SNAPSHOT_OBJECTS) };
    for _ in 0..OBJECTS {
        scenario.add_real_card(PlayerId(0), "Llanowar Elves", Zone::Battlefield, &db);
    }
    session.state = scenario.build().state().clone();
    session.state_revision = 100;
    let large = crate::build_spectator_game_started_message(&session).unwrap();
    let ServerMessage::GameStarted {
        state,
        your_player,
        legal_actions,
        player_token,
        ..
    } = &large
    else {
        unreachable!()
    };
    assert_eq!(*your_player, crate::SPECTATOR_PLAYER_ID);
    assert!(legal_actions.is_empty());
    assert!(player_token.is_none());
    assert_eq!(state.objects.len(), OBJECTS);
    assert!(state
        .objects
        .values()
        .all(|object| !object.abilities.is_empty()));
    let large_stats = write_frame(&executor, &destination, "large", large).await;
    assert!(large_stats["frame_utf16_units"].as_u64().unwrap() > 8_388_608);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(PlayerId(0), "Llanowar Elves", Zone::Battlefield, &db);
    let after = scenario.build().state().clone();
    let key = &session.full_runtime.as_ref().unwrap().key;
    let update = crate::build_spectator_state_update_message(key, &after, &[], &[], 101).unwrap();
    let after_stats = write_frame(&executor, &destination, "after", update).await;
    let manifest = serde_json::json!({
        "schema_version": 1,
        "producer": "phase-server native spectator builder + encode_application + ConnectionEncoder + native gzip",
        "card": "Llanowar Elves", "objects": OBJECTS, "game_code": game_code,
        "small": small_stats, "large": large_stats, "after": after_stats,
    });
    std::fs::write(
        destination.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    println!("large spectator Full: {manifest}");
}
