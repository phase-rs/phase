use serde::{Deserialize, Serialize};

use super::ability::{Duration, EntryAttackDestination, StaticDefinition};
use super::card_type::{CoreType, Supertype};
use super::counter::CounterType;
use super::identifiers::ObjectId;
use super::keywords::Keyword;
use super::mana::ManaColor;
use super::player::PlayerId;
use crate::types::attack_target::AttackTarget;
use crate::types::object_data::AttachTarget;

/// CR 111.1 + CR 111.4 + CR 111.10: The body characteristics of a token —
/// the fields that constitute its identity as a permanent, independent of
/// the runtime context in which it's created.
///
/// Shared by `TokenSpec` (runtime/resolved token creation), `TokenPreset`
/// (debug catalog entries), and `DebugAction::CreateToken` (debug-create
/// payload). Single source of truth for the token body shape — no parallel
/// field lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCharacteristics {
    /// CR 111.4: The token's display name (same as its subtype(s) + "Token"
    /// unless the creating effect specifies otherwise).
    pub display_name: String,
    /// CR 208.2: Fixed power, or `None` for non-creature tokens.
    pub power: Option<i32>,
    /// CR 208.2: Fixed toughness, or `None` for non-creature tokens.
    pub toughness: Option<i32>,
    /// CR 306.5b: Printed loyalty, or `None` for non-planeswalker tokens.
    ///
    /// The token's *printed* loyalty, as `CardFace::loyalty` is for a card-backed
    /// planeswalker. Battlefield loyalty itself remains counter-derived (CR 306.5c).
    /// Seeding a token's entry loyalty counters from this value (CR 306.5b) is not
    /// yet done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loyalty: Option<u32>,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub colors: Vec<ManaColor>,
    pub keywords: Vec<Keyword>,
}

/// CR 111.1 + CR 111.4 + CR 111.10: Fully-resolved token creation specification.
///
/// `Effect::Token` carries authoring-time fields (`PtValue`, `QuantityExpr`,
/// `TargetFilter owner`) that must be resolved against game state before the
/// token hits the replacement pipeline. `TokenSpec` captures the resolved,
/// self-describing form used by `ProposedEvent::CreateToken` and the
/// post-accept apply path, so replacement matchers and modifiers see the full
/// characteristics of the token that's about to be created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSpec {
    pub characteristics: TokenCharacteristics,
    /// Original Forge-style script name (or custom name) used by the token
    /// parser on the apply path to re-derive attributes. Preserved so the
    /// existing `parse_token_script` dispatch still fires after widening.
    pub script_name: String,
    /// CR 113.3d: Static abilities granted to the token (e.g., "This token
    /// can't block.").
    pub static_abilities: Vec<StaticDefinition>,
    /// CR 122.6a: Counters placed on the token as it enters the battlefield
    /// (resolved from `QuantityExpr` at propose time).
    pub enter_with_counters: Vec<(CounterType, u32)>,
    /// CR 614.1: Token enters tapped.
    pub tapped: bool,
    /// CR 508.4: Token enters the battlefield attacking (not declared as
    /// attacker).
    pub enters_attacking: bool,
    /// CR 508.4: the batch's resolved destination.
    #[serde(default, skip_serializing_if = "TokenAttackRequest::is_unspecified")]
    pub attack_request: TokenAttackRequest,
    /// CR 603.7: When set, the token is sacrificed at the end of the given
    /// duration (e.g., Mobilize tokens sacrificed at end of combat).
    pub sacrifice_at: Option<Duration>,
    /// CR 107.3a: Ability source — the object that created the token. Needed
    /// on the apply path for defending-player resolution (`enters_attacking`)
    /// and for the delayed-trigger source.
    pub source_id: ObjectId,
    /// CR 107.3a: Ability controller — the player who controls the effect
    /// creating the token (distinct from `owner`, the player to whom the
    /// token belongs).
    pub controller: PlayerId,
    /// CR 303.4 + CR 303.4i: The token instruction's "attached to …" clause and
    /// its binding outcome, resolved once at propose time so the
    /// replacement-safe apply path attaches each created token without
    /// re-reading `ability.targets`.
    #[serde(default, skip_serializing_if = "TokenHostRequest::is_not_requested")]
    pub attach_to: TokenHostRequest,
}

/// CR 508.4 + CR 608.2h: a token instruction's named destination, resolved
/// once, when the instruction resolves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenAttackRequest {
    /// No named destination.
    #[default]
    Unspecified,
    /// The named players, in the order the batch's tokens are created.
    Named {
        first: PlayerId,
        rest: Vec<PlayerId>,
    },
}

impl TokenAttackRequest {
    /// Keeping this default omitted preserves the wire shape of every token
    /// event that names no destination.
    pub fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }

    /// `None` when `players` is empty.
    pub fn named(players: impl IntoIterator<Item = PlayerId>) -> Option<Self> {
        let mut players = players.into_iter();
        let first = players.next()?;
        Some(Self::Named {
            first,
            rest: players.collect(),
        })
    }

    /// CR 508.4: the destination of the batch's `ordinal`-th created token.
    pub fn for_entrant(&self, ordinal: usize) -> EntryAttackDestination {
        match self {
            Self::Unspecified => EntryAttackDestination::AnyDefender,
            Self::Named { first, rest } => {
                let player = match ordinal % (1 + rest.len()) {
                    0 => *first,
                    n => rest[n - 1],
                };
                EntryAttackDestination::Exact {
                    target: AttackTarget::Player(player),
                }
            }
        }
    }

    /// The request for the batch's remaining tokens once `consumed` of its tokens
    /// have been created, for a resume that counts its own tokens from zero:
    /// `after_entrants(n).for_entrant(k) == self.for_entrant(n + k)`.
    pub fn after_entrants(&self, consumed: usize) -> Self {
        match self {
            Self::Unspecified => Self::Unspecified,
            Self::Named { first, rest } => {
                let mut players: Vec<PlayerId> = std::iter::once(*first)
                    .chain(rest.iter().copied())
                    .collect();
                let shift = consumed % players.len();
                players.rotate_left(shift);
                Self::Named {
                    first: players[0],
                    rest: players[1..].to_vec(),
                }
            }
        }
    }
}

/// CR 303.4i: what the token instruction asked for as a host, and whether
/// anything bound it.
///
/// The distinction is load-bearing, which is why it is a type rather than an
/// `Option<AttachTarget>`: CR 303.4i denies the entry of an Aura token whose
/// named host is *undefined*, while an ordinary token that never named a host
/// is created normally. Both were `None` before, so the seam that had to tell
/// them apart could not. [`TokenHostRequest::Unbound`] is the state that
/// `None` could not express.
///
/// Carried through the CR 614 replacement pipeline rather than consumed before
/// it: a replacement effect may change the entering token's characteristics,
/// so whether CR 303.4i applies is a question about the ACTUAL entrant and can
/// only be answered per token, after replacements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TokenHostRequest {
    /// The instruction named no host. An ordinary token.
    #[default]
    NotRequested,
    /// The instruction named a host and it resolved to this object or player.
    /// Whether that host can legally be enchanted is a separate question,
    /// owned by `effects::attach`.
    Bound(AttachTarget),
    /// CR 303.4i: the instruction named a host and nothing bound it — the host
    /// is undefined.
    Unbound,
}

impl TokenHostRequest {
    /// Whether the instruction named no host. Keeping this default omitted
    /// preserves the existing wire shape for ordinary token creation events.
    pub fn is_not_requested(&self) -> bool {
        matches!(self, Self::NotRequested)
    }

    /// The resolved host, if one bound. `None` for both of the other states —
    /// use the variant itself when the difference matters.
    pub fn bound(self) -> Option<AttachTarget> {
        match self {
            Self::Bound(target) => Some(target),
            Self::NotRequested | Self::Unbound => None,
        }
    }

    /// Did the instruction name a host at all?
    pub fn is_requested(self) -> bool {
        !matches!(self, Self::NotRequested)
    }

    /// Build the request from a named-host flag and its binding outcome. The
    /// single place the three states are derived, so no caller re-encodes the
    /// mapping.
    pub fn from_binding(named: bool, bound: Option<AttachTarget>) -> Self {
        match (named, bound) {
            (_, Some(target)) => Self::Bound(target),
            (true, None) => Self::Unbound,
            (false, None) => Self::NotRequested,
        }
    }
}
