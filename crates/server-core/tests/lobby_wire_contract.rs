//! Wire-compatibility contract between the broker's lobby-subset enums
//! (`lobby_broker::protocol`) and the canonical transport enums
//! (`server_core::protocol`).
//!
//! The broker (de)serializes `LobbyClientMessage`/`LobbyServerMessage`; the
//! shell (de)serializes `ClientMessage`/`ServerMessage`. For zero behavior
//! change, a given frame must produce byte-identical JSON regardless of which
//! enum wrote it, and each side must be able to read what the other wrote. This
//! guards against silent drift if either enum's serde shape changes.

use lobby_broker::protocol as lb;
use server_core::protocol as sc;

/// A lobby client frame serialized by the broker's enum must deserialize into
/// the canonical `ClientMessage` (same tag + fields).
#[test]
fn lobby_client_messages_roundtrip_into_canonical() {
    let ping = lb::LobbyClientMessage::Ping { timestamp: 99 };
    let json = serde_json::to_string(&ping).unwrap();
    let canonical: sc::ClientMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        canonical,
        sc::ClientMessage::Ping { timestamp: 99 }
    ));

    let sub = lb::LobbyClientMessage::SubscribeLobby;
    let json = serde_json::to_string(&sub).unwrap();
    let canonical: sc::ClientMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(canonical, sc::ClientMessage::SubscribeLobby));

    let unreg = lb::LobbyClientMessage::UnregisterLobby {
        game_code: "GAME01".into(),
    };
    let json = serde_json::to_string(&unreg).unwrap();
    let canonical: sc::ClientMessage = serde_json::from_str(&json).unwrap();
    assert!(
        matches!(canonical, sc::ClientMessage::UnregisterLobby { game_code } if game_code == "GAME01")
    );
}

/// The reverse: a canonical client frame must parse via the broker's two-stage
/// parser into the matching lobby variant.
#[test]
fn canonical_client_frames_parse_via_broker() {
    let canonical = sc::ClientMessage::ClientHello {
        client_version: "0.1.0".into(),
        build_commit: "abc".into(),
        protocol_version: sc::PROTOCOL_VERSION,
    };
    let json = serde_json::to_string(&canonical).unwrap();
    match lb::parse_lobby_client_message(&json) {
        lb::ParsedFrame::Message(msg) => match *msg {
            lb::LobbyClientMessage::ClientHello {
                client_version,
                build_commit,
                ..
            } => {
                assert_eq!(client_version, "0.1.0");
                assert_eq!(build_commit, "abc");
            }
            other => panic!("expected ClientHello, got {other:?}"),
        },
        other => panic!("expected ClientHello, got {other:?}"),
    }
}

/// A canonical NON-lobby frame (e.g. game `Action`) must route to the broker's
/// reject path, not silently parse into a lobby variant.
#[test]
fn non_lobby_frame_routes_to_reject() {
    let action = sc::ClientMessage::Action {
        action: engine::types::actions::GameAction::PassPriority,
    };
    let json = serde_json::to_string(&action).unwrap();
    match lb::parse_lobby_client_message(&json) {
        lb::ParsedFrame::UnknownTag(tag) => assert_eq!(tag, "Action"),
        other => panic!("expected UnknownTag for Action, got {other:?}"),
    }
}

/// Server frames serialized by the broker must be byte-identical to the same
/// frame serialized by the canonical enum.
#[test]
fn lobby_server_messages_byte_identical_to_canonical() {
    // Pong.
    let lb_pong = lb::LobbyServerMessage::Pong { timestamp: 7 };
    let sc_pong = sc::ServerMessage::Pong { timestamp: 7 };
    assert_eq!(
        serde_json::to_string(&lb_pong).unwrap(),
        serde_json::to_string(&sc_pong).unwrap()
    );

    // PasswordRequired.
    let lb_pw = lb::LobbyServerMessage::PasswordRequired {
        game_code: "GAME01".into(),
    };
    let sc_pw = sc::ServerMessage::PasswordRequired {
        game_code: "GAME01".into(),
    };
    assert_eq!(
        serde_json::to_string(&lb_pw).unwrap(),
        serde_json::to_string(&sc_pw).unwrap()
    );

    // GameCreated.
    let lb_gc = lb::LobbyServerMessage::GameCreated {
        game_code: "GAME01".into(),
        player_token: "tok".into(),
    };
    let sc_gc = sc::ServerMessage::GameCreated {
        game_code: "GAME01".into(),
        player_token: "tok".into(),
    };
    assert_eq!(
        serde_json::to_string(&lb_gc).unwrap(),
        serde_json::to_string(&sc_gc).unwrap()
    );

    // PlayerCount.
    let lb_pc = lb::LobbyServerMessage::PlayerCount { count: 42 };
    let sc_pc = sc::ServerMessage::PlayerCount { count: 42 };
    assert_eq!(
        serde_json::to_string(&lb_pc).unwrap(),
        serde_json::to_string(&sc_pc).unwrap()
    );

    // LobbyGameRemoved.
    let lb_rm = lb::LobbyServerMessage::LobbyGameRemoved {
        game_code: "GAME01".into(),
    };
    let sc_rm = sc::ServerMessage::LobbyGameRemoved {
        game_code: "GAME01".into(),
    };
    assert_eq!(
        serde_json::to_string(&lb_rm).unwrap(),
        serde_json::to_string(&sc_rm).unwrap()
    );
}

/// `ServerHello` carries the `ServerMode` enum — verify the broker's copy
/// serializes identically to the canonical one (the shell maps between them).
#[test]
fn server_hello_mode_byte_identical() {
    let lb_hello = lb::LobbyServerMessage::ServerHello {
        server_version: "0.1.0".into(),
        build_commit: "abc".into(),
        protocol_version: 7,
        mode: lb::ServerMode::LobbyOnly,
    };
    let sc_hello = sc::ServerMessage::ServerHello {
        server_version: "0.1.0".into(),
        build_commit: "abc".into(),
        protocol_version: 7,
        mode: sc::ServerMode::LobbyOnly,
        // None + skip_serializing_if keeps the wire identical to the lobby
        // broker's ServerHello, which has no public_url field.
        public_url: None,
    };
    assert_eq!(
        serde_json::to_string(&lb_hello).unwrap(),
        serde_json::to_string(&sc_hello).unwrap()
    );
}
