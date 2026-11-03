//! Manual native/WASM portability corpus; no workflow runs this producer.
//! From the repo root (STATE_CODEC_CORPUS must be an absolute path):
//! STATE_CODEC_CORPUS=/tmp/loop186-corpus.json cargo test -p state-codec --test integration \
//!   portability::produce_native_portability_corpus -- --ignored --exact --nocapture
//! Uses the committed integration_cards.json.gz, not generated card data.
//! Build same-HEAD WASM with ./scripts/build-wasm.sh release (or fresh Tilt wasm),
//! then replay with the generated JS/WASM artifacts present:
//! cd client && STATE_CODEC_CORPUS=/tmp/loop186-corpus.json pnpm exec vitest run \
//!   --config vitest.integration.config.ts --coverage.enabled=false \
//!   src/adapter/__tests__/stateCodec.integration.test.ts
//! The complete corpus is limited to 64 MiB and the script to 64 accepted actions.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

use engine::ai_support::legal_actions_for_viewer;
use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckList, PlayerDeckList};
use engine::game::engine::apply;
use engine::game::replay::reconstruct_initial_state;
use engine::game::turn_control::authorized_submitters;
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::format::FormatConfig;
use engine::types::game_state::{ActionDisposition, GameState, WaitingFor};
use engine::types::match_config::MatchConfig;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replay::ReplayHeader;
use serde::Serialize;
use state_codec::{state_hash, WireState};

const MAX_ACTIONS: usize = 64;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const AUDIENCES: [Option<PlayerId>; 4] = [
    Some(PlayerId(0)),
    Some(PlayerId(1)),
    Some(PlayerId(u8::MAX)),
    None,
];
const LEXICAL_ROWS: [(&str, &str); 5] = [
    ("nested_ordering", r#"{"z":{"y":2,"x":1},"a":0}"#),
    ("u64", r#"{"u64":18446744073709551615}"#),
    (
        "u128",
        r#"{"u128":340282366920938463463374607431768211455}"#,
    ),
    ("unicode", r#"{"unicode":"Éowyn 雪 🧙 \u0061"}"#),
    (
        "whitespace",
        r#"{ "z" : [ 1, 2 ], "a" : { "y":0, "x":1 } }"#,
    ),
];

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Milestone {
    Initial,
    PostMulligan,
    #[serde(rename = "land_seat_0")]
    LandSeat0,
    #[serde(rename = "land_seat_1")]
    LandSeat1,
    Final,
}

#[derive(Serialize)]
struct AudienceRow {
    viewer: Option<PlayerId>,
    json: String,
    hash: String,
    revision: String,
}

#[derive(Serialize)]
struct Checkpoint {
    milestone: Milestone,
    after_actions: usize,
    audiences: Vec<AudienceRow>,
}

#[derive(Serialize)]
struct AcceptedAction {
    actor: PlayerId,
    action: GameAction,
}

#[derive(Serialize)]
struct LexicalRow {
    name: &'static str,
    json: String,
    hash: String,
}

#[derive(Serialize)]
struct OwnershipRow {
    name: &'static str,
    json: String,
    hash: String,
    revision: String,
}

#[derive(Serialize)]
struct Corpus {
    schema_version: u8,
    header: ReplayHeader,
    actions: Vec<AcceptedAction>,
    checkpoints: Vec<Checkpoint>,
    lexical_rows: Vec<LexicalRow>,
    ownership_rows: Vec<OwnershipRow>,
}

fn capture_checkpoint(state: &GameState, milestone: Milestone, after_actions: usize) -> Checkpoint {
    let audiences: Vec<_> = AUDIENCES
        .into_iter()
        .map(|viewer| {
            let projected = match viewer {
                Some(player) => filter_state_for_viewer(state, player),
                None => filter_state_for_unseated_viewer(state),
            };
            let wire = WireState::from_game_state(&projected).unwrap();
            AudienceRow {
                viewer,
                json: wire.as_json().to_owned(),
                hash: state_hash(&wire).to_string(),
                revision: wire.revision().unwrap().to_string(),
            }
        })
        .collect();
    assert_ne!(
        audiences[0].json, audiences[1].json,
        "distinct private seat projections"
    );
    Checkpoint {
        milestone,
        after_actions,
        audiences,
    }
}

fn accept(
    state: &mut GameState,
    actions: &mut Vec<AcceptedAction>,
    actor: PlayerId,
    action: GameAction,
) {
    assert!(
        actions.len() < MAX_ACTIONS,
        "script exceeded {MAX_ACTIONS} actions"
    );
    let context = format!(
        "action={} actor={actor:?} turn={} phase={:?} prompt={:?}",
        actions.len(),
        state.turn_number,
        state.phase,
        state.waiting_for
    );
    let result =
        apply(state, actor, action.clone()).unwrap_or_else(|error| panic!("{context}: {error:?}"));
    assert_eq!(result.disposition, ActionDisposition::Applied, "{context}");
    actions.push(AcceptedAction { actor, action });
}

fn ownership_row(name: &'static str, life: i32, revision: u64) -> OwnershipRow {
    let json = format!(
        r#"{{"objects":{{"7":{{"name":"Shared permanent","text":"{}","lexical":1e0}}}},"life":{life},"opaque":{{ "n":1e9999,"n":-0 }}}}"#,
        "Unchanged object text. ".repeat(64)
    );
    let wire = WireState::from_json(json.as_bytes())
        .unwrap()
        .with_revision(revision);
    assert_eq!(wire.as_json(), json);
    OwnershipRow {
        name,
        json,
        hash: state_hash(&wire).to_string(),
        revision: wire.revision().unwrap().to_string(),
    }
}

#[test]
#[ignore = "manual native/WASM corpus producer; requires STATE_CODEC_CORPUS"]
fn produce_native_portability_corpus() {
    let destination = std::env::var_os("STATE_CODEC_CORPUS")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .expect("STATE_CODEC_CORPUS must be a nonempty absolute path");
    assert!(
        destination.is_absolute(),
        "STATE_CODEC_CORPUS must be an absolute path"
    );
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/integration_cards.json.gz");
    let db = Arc::new(
        CardDatabase::from_export_reader(flate2::read::GzDecoder::new(BufReader::new(
            File::open(fixture).expect("committed card fixture opens"),
        )))
        .expect("committed card fixture loads"),
    );
    let deck = |name: &str| {
        assert!(
            db.get_face_by_name(name).is_some(),
            "missing fixture card: {name}"
        );
        PlayerDeckList {
            main_deck: vec![name.to_owned(); 60],
            ..Default::default()
        }
    };
    let header = ReplayHeader {
        seed: 88,
        player_count: 2,
        first_player: Some(0),
        format_config: FormatConfig {
            allow_debug_actions: true,
            ..FormatConfig::free_for_all()
        },
        match_config: MatchConfig::default(),
        deck_data: Some(DeckList {
            player: deck("Forest"),
            opponent: deck("Mountain"),
            ..Default::default()
        }),
    };
    let mut state = reconstruct_initial_state(&header, Some(&db)).unwrap();
    for player in &state.players {
        assert!(
            !player.hand.is_empty(),
            "opening hand loaded for {:?}",
            player.id
        );
        assert!(
            !player.library.is_empty(),
            "library loaded for {:?}",
            player.id
        );
    }
    assert!(matches!(
        state.waiting_for,
        WaitingFor::MulliganDecision { .. }
    ));
    let mut actions = Vec::new();
    let mut checkpoints = vec![capture_checkpoint(&state, Milestone::Initial, 0)];
    for seat in [PlayerId(0), PlayerId(1)] {
        assert!(
            matches!(&state.waiting_for, WaitingFor::MulliganDecision { pending, .. } if pending.iter().any(|entry| entry.player == seat))
        );
        assert!(authorized_submitters(&state).contains(&seat));
        accept(
            &mut state,
            &mut actions,
            seat,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        );
    }
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    checkpoints.push(capture_checkpoint(
        &state,
        Milestone::PostMulligan,
        actions.len(),
    ));
    let mut land_turns = [None, None];
    loop {
        if land_turns.iter().all(Option::is_some)
            && state.active_player == PlayerId(0)
            && state.turn_number > land_turns[0].unwrap()
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.is_empty()
        {
            break;
        }
        let actors = authorized_submitters(&state);
        assert_eq!(
            actors.len(),
            1,
            "script requires one actor: {:?}",
            state.waiting_for
        );
        let actor = actors[0];
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                if actor == state.active_player && land_turns[usize::from(actor.0)].is_none() {
                    legal_actions_for_viewer(&state, actor)
                        .0
                        .into_iter()
                        .find(|action| matches!(action, GameAction::PlayLand { .. }))
                        .unwrap_or(GameAction::PassPriority)
                } else {
                    GameAction::PassPriority
                }
            }
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            prompt => panic!(
                "unexpected prompt: action={} actor={actor:?} turn={} phase={:?} prompt={prompt:?}",
                actions.len(),
                state.turn_number,
                state.phase
            ),
        };
        let played = if let GameAction::PlayLand { object_id, card_id } = &action {
            assert!(
                object_id.0 <= 9_007_199_254_740_991 && card_id.0 <= 9_007_199_254_740_991,
                "action IDs must be JS safe integers"
            );
            assert!(state.players[usize::from(actor.0)].hand.contains(object_id));
            assert_eq!(state.objects[object_id].card_id, *card_id);
            Some((*object_id, *card_id, state.turn_number))
        } else {
            None
        };
        accept(&mut state, &mut actions, actor, action);
        if let Some((object_id, card_id, turn)) = played {
            assert!(!state.players[usize::from(actor.0)]
                .hand
                .contains(&object_id));
            assert!(
                state
                    .battlefield
                    .iter()
                    .any(|id| state.objects[id].card_id == card_id),
                "played card reached battlefield"
            );
            land_turns[usize::from(actor.0)] = Some(turn);
            let milestone = match actor.0 {
                0 => Milestone::LandSeat0,
                1 => Milestone::LandSeat1,
                _ => unreachable!(),
            };
            checkpoints.push(capture_checkpoint(&state, milestone, actions.len()));
        }
    }
    checkpoints.push(capture_checkpoint(&state, Milestone::Final, actions.len()));
    assert_eq!(
        checkpoints
            .iter()
            .map(|row| &row.milestone)
            .collect::<Vec<_>>(),
        vec![
            &Milestone::Initial,
            &Milestone::PostMulligan,
            &Milestone::LandSeat0,
            &Milestone::LandSeat1,
            &Milestone::Final
        ]
    );
    assert!(checkpoints
        .windows(2)
        .all(|rows| rows[0].after_actions < rows[1].after_actions));
    assert_eq!(checkpoints.last().unwrap().after_actions, actions.len());
    for row in &checkpoints {
        assert_eq!(
            row.audiences
                .iter()
                .map(|audience| audience.viewer)
                .collect::<Vec<_>>(),
            AUDIENCES
        );
    }
    let lexical_rows = LEXICAL_ROWS
        .into_iter()
        .map(|(name, json)| {
            let wire = WireState::from_json(json.as_bytes()).unwrap();
            assert_eq!(wire.as_json(), json, "literal lexical provenance: {name}");
            LexicalRow {
                name,
                json: json.to_owned(),
                hash: state_hash(&wire).to_string(),
            }
        })
        .collect();
    let corpus = Corpus {
        schema_version: 1,
        header,
        actions,
        checkpoints,
        lexical_rows,
        ownership_rows: vec![
            ownership_row("base", 20, 9_007_199_254_740_993),
            ownership_row("current", 19, 9_007_199_254_740_994),
        ],
    };
    let bytes = serde_json::to_vec(&corpus).unwrap();
    assert!(
        bytes.len() <= MAX_BYTES,
        "corpus exceeds 64 MiB: {} bytes",
        bytes.len()
    );
    std::fs::write(&destination, &bytes).expect("complete bounded corpus writes");
    assert_eq!(
        std::fs::metadata(&destination).unwrap().len(),
        bytes.len() as u64
    );
    println!("destination={} accepted_actions={} checkpoints={} audience_comparisons={} lexical_rows={} corpus_bytes={}", destination.display(), corpus.actions.len(), corpus.checkpoints.len(), corpus.checkpoints.len() * AUDIENCES.len(), corpus.lexical_rows.len(), bytes.len());
}
