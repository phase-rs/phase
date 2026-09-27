use serde::{Deserialize, Deserializer, Serialize};

use engine::game::bracket_estimate::CommanderBracketTier;
use engine::types::mana::ManaColor;

use crate::config::AiDifficulty;
use crate::deck_profile::DeckArchetype;

/// Where a candidate's tier came from. Declared outranks estimated because a
/// declaration is the only source that can produce `Cedh`; the estimator never
/// does. Declaration order is used only through `provenance_rank`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelProvenance {
    Estimated,
    Declared,
}

/// A candidate's known tier and its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketLabel {
    pub tier: CommanderBracketTier,
    pub provenance: LabelProvenance,
    /// The bracket-list data version current when this label was produced.
    pub data_version: String,
}

/// A canonical, deduplicated set of accepted tiers.
///
/// This is deliberately not a `BTreeSet`: `CommanderBracketTier` has no `Ord`
/// implementation and must not gain one merely for a container. Canonical order
/// follows the tier's numeric ordering through the selector's sole `as_u8` call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<CommanderBracketTier>")]
pub struct TierSet(Vec<CommanderBracketTier>);

impl TierSet {
    pub fn new(tiers: Vec<CommanderBracketTier>) -> Self {
        tiers.into()
    }

    pub fn as_slice(&self) -> &[CommanderBracketTier] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, tier: &CommanderBracketTier) -> bool {
        self.0.contains(tier)
    }
}

impl From<Vec<CommanderBracketTier>> for TierSet {
    fn from(mut tiers: Vec<CommanderBracketTier>) -> Self {
        tiers.sort_by_key(|tier| tier_distance(*tier, CommanderBracketTier::Exhibition));
        tiers.dedup();
        Self(tiers)
    }
}

/// The attribute two seats must not share. This parameterises the distinctness
/// axis instead of proliferating three sibling constraint variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatAttribute {
    Deck,
    Commander,
    ColorIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PodConstraint {
    Distinct(SeatAttribute),
    BracketDistance,
    LabelProvenance,
    CoverageFloor,
    Archetype,
}

/// Whether accepted tiers guide selection or form a hard gate. This is a named
/// policy axis rather than a boolean or an inference from the accepted set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierEnforcement {
    Advisory,
    HardGate,
}

/// A seat already filled by an explicit choice that automatic selection must
/// not collide with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodSeatOccupant {
    pub deck_id: String,
    pub commander: Vec<String>,
    pub color_identity: Vec<ManaColor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodSelectionRequest {
    /// Empty means no tier constraint; non-contiguous tier sets are supported.
    pub allowed: TierSet,
    pub prefer: Option<CommanderBracketTier>,
    pub enforcement: TierEnforcement,
    /// Number of AI seats to fill, not the total player count.
    pub seats: u8,
    #[serde(deserialize_with = "deserialize_constraints")]
    pub constraints: Vec<PodConstraint>,
    pub coverage_floor_pct: u8,
    pub archetype: Option<DeckArchetype>,
    /// Must remain in the exact integer range of the transport carrying it.
    pub seed: u64,
    /// Seats already filled by explicit choices that selection must not collide with.
    #[serde(default)]
    pub occupied: Vec<PodSeatOccupant>,
}

impl PodSelectionRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        allowed: TierSet,
        prefer: Option<CommanderBracketTier>,
        enforcement: TierEnforcement,
        seats: u8,
        constraints: Vec<PodConstraint>,
        coverage_floor_pct: u8,
        archetype: Option<DeckArchetype>,
        seed: u64,
    ) -> Self {
        Self {
            allowed,
            prefer,
            enforcement,
            seats,
            constraints: canonical_constraints(constraints),
            coverage_floor_pct,
            archetype,
            seed,
            occupied: Vec::new(),
        }
    }

    pub fn with_occupied(mut self, occupied: Vec<PodSeatOccupant>) -> Self {
        self.occupied = occupied;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiDeckCandidate {
    pub id: String,
    /// Commander card names; color identity is resolved from the card database.
    pub commander: Vec<String>,
    pub label: Option<BracketLabel>,
    pub coverage_pct: Option<u8>,
    pub archetype: Option<DeckArchetype>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodSeat {
    pub seat_index: u8,
    pub candidate_id: String,
    /// The tier actually served; unlabelled candidates remain unlabelled.
    pub tier: Option<CommanderBracketTier>,
    pub provenance: Option<LabelProvenance>,
    /// The bracket-derived default. Advisory callers may override it explicitly.
    pub difficulty: AiDifficulty,
    /// Engine-resolved color identity in canonical WUBRG order.
    pub color_identity: Vec<ManaColor>,
}

/// One recorded degradation. There is no `served` field because every seat
/// already reports its actual tier and some constraints have no tier meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodRelaxation {
    pub seat_index: u8,
    pub relaxed: PodConstraint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodAssignment {
    pub seats: Vec<PodSeat>,
    pub relaxations: Vec<PodRelaxation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PodSelectionError {
    InsufficientCandidates { requested: u8, available: u8 },
    HardGateUnsatisfiable { requested: u8, available: u8 },
    TooManySeats { seats: u8 },
    UnknownCommander { candidate_id: String, name: String },
}

pub(super) fn tier_distance(tier: CommanderBracketTier, reference: CommanderBracketTier) -> u8 {
    tier.as_u8().abs_diff(tier_rank(reference))
}

fn tier_rank(tier: CommanderBracketTier) -> u8 {
    match tier {
        CommanderBracketTier::Exhibition => 1,
        CommanderBracketTier::Core => 2,
        CommanderBracketTier::Upgraded => 3,
        CommanderBracketTier::Optimized => 4,
        CommanderBracketTier::Cedh => 5,
    }
}

pub(super) fn canonical_constraints(mut constraints: Vec<PodConstraint>) -> Vec<PodConstraint> {
    constraints.sort();
    constraints.dedup();
    constraints
}

fn deserialize_constraints<'de, D>(deserializer: D) -> Result<Vec<PodConstraint>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<PodConstraint>::deserialize(deserializer).map(canonical_constraints)
}
