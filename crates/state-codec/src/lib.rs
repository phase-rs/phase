//! Exact JSON-value storage and FULL frames for viewer-filtered engine states.
//!
//! [`WireState::from_game_state`] delegates to the engine's deterministic compact
//! serializer. [`WireState::from_json`] instead preserves the exact JSON value
//! span: internal whitespace, key order, duplicate keys, and numeric spelling.
//! Only whitespace outside the value is discarded. [`state_hash`] is SHA-256 of
//! that retained span, not the frame envelope. Arbitrary input is not canonicalized.
//!
//! FULL encoding and decoding ignore every baseline, including a different
//! viewer's baseline. Frame output is `{"type":"Full","data":{"state":...}}`;
//! readers accept either envelope-key order. This is representation, not integrity
//! authentication or engine-state validation.
//!
//! A viewer projection must **not** be decoded through `GameState` to implement
//! this contract: the authoritative decoder can normalize allocators or reject
//! redacted firing authority. Callers choosing a separate typed conversion retain
//! that limitation; decoding JSON does not establish trusted restore authority.
//!
//! Private state/frame representations leave room for B3 baseline metadata and
//! deltas without changing these signatures. Engine revisions are not serialized;
//! future metadata must be carried separately, never inferred from this JSON.
//!
//! Native verification: `cargo test -p state-codec --test integration`.
//! Opt-in full games: `cargo test -p state-codec self_play::full_ -- --ignored --nocapture`.
//! This crate's native tests do not claim independent native/WASM parity (B2b).

use std::{borrow::Cow, fmt, sync::Arc};

use engine::types::game_state::GameState;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

/// An immutable, syntactically valid JSON value; not an authoritative game restore.
#[derive(Clone, Debug)]
pub struct WireState {
    json: Arc<RawValue>,
}

/// An opaque codec frame. FULL is the only currently supported payload.
#[derive(Clone, Debug)]
pub struct Frame {
    payload: FramePayload,
}

#[derive(Clone, Debug)]
enum FramePayload {
    Full(WireState),
}

/// SHA-256 of the complete retained JSON value bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateHash([u8; 32]);

impl fmt::Display for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// JSON errors retain their serde source; malformed framing and unsupported tags carry a codec diagnostic.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    #[error("invalid codec JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A frame the JSON grammar accepts but this codec's named-object grammar does not.
    #[error("malformed codec frame: {0}")]
    Malformed(&'static str),
}

impl WireState {
    /// Serialize using the engine's existing deterministic serializer.
    pub fn from_game_state(state: &GameState) -> Result<Self, CodecError> {
        Ok(Self {
            json: Arc::from(serde_json::value::to_raw_value(state)?),
        })
    }

    /// Validate and retain exactly one JSON value, excluding outer whitespace.
    pub fn from_json(bytes: &[u8]) -> Result<Self, CodecError> {
        let json: Box<RawValue> = serde_json::from_slice(bytes)?;
        Ok(Self {
            json: Arc::from(json),
        })
    }

    pub fn as_json(&self) -> &str {
        self.json.get()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.as_json().as_bytes()
    }
}

/// Produce a FULL frame, independently of the supplied baseline.
pub fn encode(_baseline: Option<&WireState>, state: &WireState) -> Result<Frame, CodecError> {
    Ok(Frame {
        payload: FramePayload::Full(state.clone()),
    })
}

/// Recover a FULL state without importing anything from the supplied baseline.
pub fn decode(_baseline: Option<&WireState>, frame: &Frame) -> Result<WireState, CodecError> {
    match &frame.payload {
        FramePayload::Full(state) => Ok(state.clone()),
    }
}

pub fn state_hash(state: &WireState) -> StateHash {
    StateHash(Sha256::digest(state.as_bytes()).into())
}

#[derive(Serialize)]
enum FrameTag {
    Full,
}

#[derive(Deserialize)]
struct Envelope<'a> {
    // `Cow` so a JSON-escaped spelling such as "\u0046ull" (decoded into scratch
    // storage, never borrowed) is accepted like the plain tag.
    #[serde(rename = "type", borrow)]
    tag: Cow<'a, str>,
    #[serde(borrow)]
    data: &'a RawValue,
}

#[derive(Deserialize, Serialize)]
struct FullData<'a> {
    #[serde(borrow)]
    state: &'a RawValue,
}

#[derive(Serialize)]
struct FullEnvelope<'a> {
    #[serde(rename = "type")]
    tag: FrameTag,
    data: FullData<'a>,
}

impl Frame {
    /// Parse the tag and borrowed payload separately, accepting either key order.
    ///
    /// The grammar is named JSON objects only. Serde's derived readers would also
    /// accept positional arrays and skip unknown values without validating their
    /// UTF-8, so the whole frame is validated as UTF-8 first and both objects are
    /// required to be objects.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CodecError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| CodecError::Malformed("frame is not valid UTF-8"))?;
        if !text.trim_start().starts_with('{') {
            return Err(CodecError::Malformed(
                "frame envelope must be a JSON object",
            ));
        }
        let envelope: Envelope<'_> = serde_json::from_str(text)?;
        match envelope.tag.as_ref() {
            "Full" => {
                if !envelope.data.get().starts_with('{') {
                    return Err(CodecError::Malformed("frame payload must be a JSON object"));
                }
                let data: FullData<'_> = serde_json::from_str(envelope.data.get())?;
                Ok(Self {
                    payload: FramePayload::Full(WireState {
                        json: Arc::from(data.state.to_owned()),
                    }),
                })
            }
            _ => Err(CodecError::Malformed("unsupported frame tag")),
        }
    }

    /// Write compact, type-first framing without reinterpreting the state JSON.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CodecError> {
        match &self.payload {
            FramePayload::Full(state) => Ok(serde_json::to_vec(&FullEnvelope {
                tag: FrameTag::Full,
                data: FullData { state: &state.json },
            })?),
        }
    }
}
