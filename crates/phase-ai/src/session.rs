//! `AiSession` — per-game cache shared across all decisions.
//!
//! Layered architecture:
//! - Layer 1 (`features`): structural deck data, computed once.
//! - Layer 2 (`plan`): static schedule prior, derived from features.
//! - Layer 3 (policies): consume features + plan + game state per-decision.
//!
//! `AiSession` is `Arc`-wrapped on `AiContext` so cloning the context stays
//! cheap (a refcount bump).

use std::collections::HashMap;
use std::sync::Arc;

use engine::game::DeckEntry;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::deck_profile::{ArchetypeClassification, DeckProfile};
use crate::features::{
    aggro_pressure, aristocrats, control, landfall, mana_ramp, plus_one_counters, tokens_wide,
    tribal, DeckFeatures,
};
use crate::plan::{derive_snapshot, PlanSnapshot};
use crate::policies::registry::PolicyId;
use crate::strategy_profile::StrategyProfile;
use crate::synergy::SynergyGraph;

fn features_for(deck: &[DeckEntry]) -> DeckFeatures {
    let profile = DeckProfile::analyze(deck);
    let archetype = match &profile.classification {
        ArchetypeClassification::Pure(arch) => *arch,
        ArchetypeClassification::Hybrid { primary, .. } => *primary,
    };
    let strategy = StrategyProfile::for_profile(&profile);
    DeckFeatures {
        archetype,
        strategy,
        landfall: landfall::detect(deck),
        mana_ramp: mana_ramp::detect(deck),
        tribal: tribal::detect(deck),
        control: control::detect(deck),
        aristocrats: aristocrats::detect(deck),
        aggro_pressure: aggro_pressure::detect(deck),
        tokens_wide: tokens_wide::detect(deck),
        plus_one_counters: plus_one_counters::detect(deck),
    }
}

/// Per-game cache shared by all decisions.
#[derive(Debug, Clone, Default)]
pub struct AiSession {
    pub features: HashMap<PlayerId, DeckFeatures>,
    pub plan: HashMap<PlayerId, PlanSnapshot>,
    pub synergy: HashMap<PlayerId, SynergyGraph>,
    pub memory: PolicyMemory,
}

impl AiSession {
    /// Construct a neutral session with no per-player data.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a session from the current game state — populates per-player
    /// `synergy`, `features`, and `plan` maps from each player's deck pool.
    /// Decks not present in `state.deck_pools` get default (empty) entries.
    pub fn from_game(state: &GameState) -> Self {
        let mut features = HashMap::new();
        let mut plan = HashMap::new();
        let mut synergy = HashMap::new();

        for pool in &state.deck_pools {
            let deck: &[DeckEntry] = &pool.current_main;
            let player_features = features_for(deck);
            let snapshot = derive_snapshot(&player_features);
            let graph = SynergyGraph::build(deck);
            features.insert(pool.player, player_features);
            plan.insert(pool.player, snapshot);
            synergy.insert(pool.player, graph);
        }

        Self {
            features,
            plan,
            synergy,
            memory: PolicyMemory::default(),
        }
    }

    /// Build a session for a single player from an explicit deck list.
    /// Used by `AiContext::analyze_with` when only one player's deck is known.
    pub fn from_single_deck(player: PlayerId, deck: &[DeckEntry]) -> Self {
        let mut session = Self::default();
        let player_features = features_for(deck);
        let snapshot = derive_snapshot(&player_features);
        let graph = SynergyGraph::build(deck);
        session.features.insert(player, player_features);
        session.plan.insert(player, snapshot);
        session.synergy.insert(player, graph);
        session
    }

    /// Convenience constructor returning an `Arc<AiSession>` directly.
    pub fn arc_from_game(state: &GameState) -> Arc<Self> {
        Arc::new(Self::from_game(state))
    }
}

/// Typed cross-decision policy memory. Adding new memory-carrying policies
/// requires adding a `PolicyState` variant — intentional friction that keeps
/// memory shapes auditable and `AiSession: Clone + Debug`.
#[derive(Debug, Clone, Default)]
pub struct PolicyMemory {
    pub by_policy: HashMap<PolicyId, PolicyState>,
}

/// Typed per-policy memory — no `Box<dyn Any>` and no runtime downcasting.
#[derive(Debug, Clone)]
pub enum PolicyState {
    None,
    LandfallTiming {
        held_fetch_count: u8,
        last_held_turn: u32,
    },
}
