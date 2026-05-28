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
use std::sync::{Arc, RwLock};

use engine::game::DeckEntry;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;
use crate::plan::{derive_snapshot, PlanSnapshot};
use crate::planner::quick_state_hash;
use crate::policies::registry::PolicyId;
use crate::projection::{project_to, BailReason, Projection, ProjectionHorizon, ProjectionKey};
use crate::synergy::SynergyGraph;

/// Per-game cache shared by all decisions.
#[derive(Debug, Clone, Default)]
pub struct AiSession {
    pub features: HashMap<PlayerId, DeckFeatures>,
    pub plan: HashMap<PlayerId, PlanSnapshot>,
    pub synergy: HashMap<PlayerId, SynergyGraph>,
    pub memory: PolicyMemory,
    /// Turn-scoped cache for opponent-turn projections. Key includes
    /// `turn_number` + `active_player`, so stale entries from prior turns
    /// never match — no explicit invalidation needed.
    pub projection_cache: Arc<RwLock<HashMap<ProjectionKey, Arc<Projection>>>>,
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
            let player_features = DeckFeatures::analyze(deck, pool.bracket_tier);
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
            projection_cache: Arc::default(),
        }
    }

    /// Build a session for a single player from an explicit deck list.
    /// Used by `AiContext::analyze_with` when only one player's deck is known.
    /// `tier` is the declared bracket tier; callers without tier information
    /// (e.g., pure deck-analysis paths) should pass `CommanderBracketTier::Core`.
    pub fn from_single_deck(
        player: PlayerId,
        deck: &[DeckEntry],
        tier: engine::game::bracket_estimate::CommanderBracketTier,
    ) -> Self {
        let mut session = Self::default();
        let player_features = DeckFeatures::analyze(deck, tier);
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

    /// Populate per-player features on demand. No-op if already populated.
    /// Used by callers that build a session incrementally (e.g., via
    /// `AiContext::analyze_with`, which only seeds the AI's own deck).
    ///
    /// `tier` is the declared bracket tier from the player's `PlayerDeckPool`.
    /// Callers without pool access should pass `CommanderBracketTier::Core`.
    ///
    /// **Staleness note**: this no-ops on re-calls for an already-populated
    /// player. That's safe because `AiSession` is currently rebuilt per
    /// `choose_action` call (see `AiContext::analyze_with` in
    /// `crates/phase-ai/src/context.rs`), so cached features cannot outlive
    /// a single decision. If future work promotes `AiSession` to a
    /// cross-decision lifetime (e.g., Phase 4's `SessionCompute`), add an
    /// `invalidate_player_features(player)` hook and call it from any site
    /// that mutates `state.deck_pools`.
    pub fn ensure_player_features(
        &mut self,
        player: PlayerId,
        deck: &[DeckEntry],
        tier: engine::game::bracket_estimate::CommanderBracketTier,
    ) {
        if self.features.contains_key(&player) || deck.is_empty() {
            return;
        }
        let features = DeckFeatures::analyze(deck, tier);
        let snapshot = derive_snapshot(&features);
        self.features.insert(player, features);
        self.plan.insert(player, snapshot);
        self.synergy.insert(player, SynergyGraph::build(deck));
    }

    /// Drop cached per-player features so a subsequent `ensure_player_features`
    /// call repopulates from fresh deck data. Currently unused (see the
    /// staleness note on `ensure_player_features`); provided so future
    /// cross-decision session lifetimes have a ready hook.
    pub fn invalidate_player_features(&mut self, player: PlayerId) {
        self.features.remove(&player);
        self.plan.remove(&player);
        self.synergy.remove(&player);
    }

    /// Return a player's cached archetype, if present. Typed accessor that
    /// hides the internal `features` HashMap layout — callers should prefer
    /// this over direct field access.
    pub fn archetype(&self, player: PlayerId) -> Option<crate::deck_profile::DeckArchetype> {
        self.features.get(&player).map(|f| f.archetype)
    }

    /// Retrieve a cached projection, computing it on miss. Turn-scoped
    /// key means stale entries never match. Read-path is lock-free;
    /// write-path briefly acquires a write lock.
    pub fn get_or_project(
        &self,
        base: &GameState,
        ai_player: PlayerId,
        target_opponent: PlayerId,
        horizon: ProjectionHorizon,
    ) -> Result<Arc<Projection>, BailReason> {
        let key = ProjectionKey {
            state_hash: quick_state_hash(base),
            turn_number: base.turn_number,
            active_player: base.active_player,
            ai_player,
            target_opponent,
            horizon,
        };

        if let Ok(cache) = self.projection_cache.read() {
            if let Some(hit) = cache.get(&key) {
                return Ok(Arc::clone(hit));
            }
        }

        let projection = Arc::new(project_to(base, ai_player, target_opponent, horizon)?);

        if let Ok(mut cache) = self.projection_cache.write() {
            cache.insert(key, Arc::clone(&projection));
        }

        Ok(projection)
    }

    /// Cache-only projection lookup — returns `None` on miss without doing
    /// the expensive multi-turn simulation. Policies that want projection
    /// data but can't afford the miss cost (e.g., under a tight wall-clock
    /// budget) should use this and fall back to a cheaper heuristic when
    /// no cached projection exists. On `Ok(None)` the caller knows
    /// definitively "not cached" and does not run the simulator.
    pub fn cached_projection(
        &self,
        base: &GameState,
        ai_player: PlayerId,
        target_opponent: PlayerId,
        horizon: ProjectionHorizon,
    ) -> Option<Arc<Projection>> {
        let key = ProjectionKey {
            state_hash: quick_state_hash(base),
            turn_number: base.turn_number,
            active_player: base.active_player,
            ai_player,
            target_opponent,
            horizon,
        };
        self.projection_cache
            .read()
            .ok()
            .and_then(|cache| cache.get(&key).map(Arc::clone))
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

#[cfg(test)]
mod tests {
    use engine::game::bracket_estimate::CommanderBracketTier;
    use engine::types::game_state::GameState;
    use engine::types::player::PlayerId;

    use super::AiSession;

    fn make_pool_with_tier(
        player: PlayerId,
        tier: CommanderBracketTier,
    ) -> engine::types::game_state::PlayerDeckPool {
        engine::types::game_state::PlayerDeckPool {
            player,
            bracket_tier: tier,
            ..Default::default()
        }
    }

    #[test]
    fn cedh_tier_pool_records_cedh_bracket() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.clear();
        state
            .deck_pools
            .push(make_pool_with_tier(PlayerId(0), CommanderBracketTier::Cedh));
        state
            .deck_pools
            .push(make_pool_with_tier(PlayerId(1), CommanderBracketTier::Core));

        let session = AiSession::from_game(&state);

        let p0_features = session
            .features
            .get(&PlayerId(0))
            .expect("player 0 features should be populated");
        assert_eq!(
            p0_features.bracket_tier,
            CommanderBracketTier::Cedh,
            "PlayerDeckPool with CommanderBracketTier::Cedh must record the Cedh tier"
        );

        let p1_features = session
            .features
            .get(&PlayerId(1))
            .expect("player 1 features should be populated");
        assert_ne!(
            p1_features.bracket_tier,
            CommanderBracketTier::Cedh,
            "PlayerDeckPool with CommanderBracketTier::Core must not record Cedh"
        );
    }

    #[test]
    fn optimized_tier_pool_records_non_cedh_bracket() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.clear();
        state.deck_pools.push(make_pool_with_tier(
            PlayerId(0),
            CommanderBracketTier::Optimized,
        ));
        state
            .deck_pools
            .push(make_pool_with_tier(PlayerId(1), CommanderBracketTier::Core));

        let session = AiSession::from_game(&state);

        let p0_features = session
            .features
            .get(&PlayerId(0))
            .expect("player 0 features should be populated");
        assert_eq!(
            p0_features.bracket_tier,
            CommanderBracketTier::Optimized,
            "CommanderBracketTier::Optimized (highest non-cEDH tier) must be recorded as-is"
        );
        assert_ne!(p0_features.bracket_tier, CommanderBracketTier::Cedh);
    }

    #[test]
    fn bracket_tier_propagates_through_load_deck_into_state() {
        use engine::game::bracket_estimate::CommanderBracketTier;
        use engine::game::deck_loading::{load_deck_into_state, DeckPayload, PlayerDeckPayload};

        let mut state = GameState::new_two_player(42);
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                bracket_tier: CommanderBracketTier::Cedh,
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                bracket_tier: CommanderBracketTier::Optimized,
                ..Default::default()
            },
            ..Default::default()
        };
        load_deck_into_state(&mut state, &payload);

        let p0_pool = state
            .deck_pools
            .iter()
            .find(|p| p.player == PlayerId(0))
            .expect("player 0 pool must exist after load");
        assert_eq!(
            p0_pool.bracket_tier,
            CommanderBracketTier::Cedh,
            "bracket_tier must round-trip through load_deck_into_state for player 0"
        );

        let p1_pool = state
            .deck_pools
            .iter()
            .find(|p| p.player == PlayerId(1))
            .expect("player 1 pool must exist after load");
        assert_eq!(
            p1_pool.bracket_tier,
            CommanderBracketTier::Optimized,
            "bracket_tier must round-trip through load_deck_into_state for player 1"
        );
    }
}
