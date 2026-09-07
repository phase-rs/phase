//! `AiSession` — per-game cache shared across all decisions.
//!
//! Layered architecture:
//! - Layer 1 (`features`): structural deck data, computed once.
//! - Layer 2 (`plan`): static schedule prior, derived from features.
//! - Layer 3 (policies): consume features + plan + game state per-decision.
//!
//! `AiSession` is `Arc`-wrapped on `AiContext` so cloning the context stays
//! cheap (a refcount bump).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};

use engine::ai_support::{CertifiedFetchFollowUp, CertifiedFetchPrompt, CertifiedPactPlan};
use engine::game::DeckEntry;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::util::Deadline;

use crate::deck_profile::DeckProfile;
use crate::features::DeckFeatures;
use crate::plan::{derive_snapshot, PlanSnapshot};
use crate::planner::quick_state_hash;
use crate::policies::registry::PolicyId;
#[cfg(test)]
use crate::policies::PolicyRegistry;
use crate::projection::{project_to, BailReason, Projection, ProjectionHorizon, ProjectionKey};
use crate::strategy_profile::StrategyProfile;
use crate::synergy::SynergyGraph;

/// Commanders are reliably castable build-around cards, so feature/profile
/// detection should treat each commander face as more informative than a
/// singleton main-deck card.
const COMMANDER_ANALYSIS_WEIGHT: u32 = 4;

type ProspectiveFetchProposals = HashMap<PlayerId, Vec<(GameAction, CertifiedFetchPrompt)>>;
pub(crate) type PactRouteStore = HashMap<PlayerId, CertifiedPactPlan>;
pub(crate) type PactPlanProposals = HashMap<PlayerId, Vec<(GameAction, CertifiedPactPlan)>>;

/// Per-game cache shared by all decisions.
#[derive(Clone, Default)]
pub struct AiSession {
    pub deck_profile: HashMap<PlayerId, DeckProfile>,
    pub features: HashMap<PlayerId, DeckFeatures>,
    pub plan: HashMap<PlayerId, PlanSnapshot>,
    pub strategy: HashMap<PlayerId, StrategyProfile>,
    pub synergy: HashMap<PlayerId, SynergyGraph>,
    pub memory: Arc<RwLock<PolicyMemory>>,
    /// Turn-scoped cache for opponent-turn projections. Key includes
    /// `turn_number` + `active_player`, so stale entries from prior turns
    /// never match — no explicit invalidation needed.
    pub projection_cache: Arc<RwLock<HashMap<ProjectionKey, Arc<Projection>>>>,
    /// Reducer-certified fetch selection armed only after this session chose
    /// its corresponding root activation. The engine token contains no clone
    /// or hidden terminal state and rejects any stale prompt.
    pub(crate) prospective_fetch_prompt: Arc<RwLock<HashMap<PlayerId, CertifiedFetchPrompt>>>,
    /// One exact cast unlocked by a redeemed prospective fetch prompt. The
    /// engine validates the complete post-selection state before yielding it.
    pub(crate) prospective_fetch_follow_up: Arc<RwLock<HashMap<PlayerId, CertifiedFetchFollowUp>>>,
    /// Root-scoring proposals awaiting the same decision's final action. The
    /// chosen proposal is moved into `prospective_fetch_prompt`; all others
    /// are discarded immediately.
    pub(crate) prospective_fetch_proposals: Arc<RwLock<ProspectiveFetchProposals>>,
    /// Opaque Pact route retained only after this session selects the exact
    /// certified root. The engine owns delayed-trigger provenance and rejects
    /// every stale, aliased, or non-delayed redemption attempt.
    pub(crate) pact_routes: Arc<RwLock<PactRouteStore>>,
    /// Scoring drafts awaiting root selection. These are never durable routes:
    /// selection atomically transfers only the chosen root's certificate.
    pub(crate) pact_proposals: Arc<RwLock<PactPlanProposals>>,
    #[cfg(test)]
    pub(crate) policy_registry_override: Option<Arc<PolicyRegistry>>,
}

impl std::fmt::Debug for AiSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiSession")
            .field("deck_profile", &self.deck_profile)
            .field("features", &self.features)
            .field("plan", &self.plan)
            .field("strategy", &self.strategy)
            .field("synergy", &self.synergy)
            .field("memory", &self.memory)
            .field("projection_cache", &self.projection_cache)
            .field("prospective_fetch_prompt", &self.prospective_fetch_prompt)
            .field(
                "prospective_fetch_follow_up",
                &self.prospective_fetch_follow_up,
            )
            .field(
                "prospective_fetch_proposals",
                &self.prospective_fetch_proposals,
            )
            .field("pact_routes", &self.pact_routes)
            .field("pact_proposals", &self.pact_proposals)
            .finish()
    }
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
        let mut deck_profile = HashMap::new();
        let mut plan = HashMap::new();
        let mut strategy = HashMap::new();
        let mut synergy = HashMap::new();

        for pool in &state.deck_pools {
            let deck = analysis_deck(&pool.current_main, &pool.current_commander);
            let player_profile = DeckProfile::analyze(&deck);
            let player_features = DeckFeatures::analyze(&deck, pool.bracket_tier);
            let snapshot = derive_snapshot(&player_features);
            let player_strategy = StrategyProfile::for_profile(&player_profile);
            let graph = SynergyGraph::build(&deck);
            deck_profile.insert(pool.player, player_profile);
            features.insert(pool.player, player_features);
            plan.insert(pool.player, snapshot);
            strategy.insert(pool.player, player_strategy);
            synergy.insert(pool.player, graph);
        }

        Self {
            deck_profile,
            features,
            plan,
            strategy,
            synergy,
            memory: Arc::default(),
            projection_cache: Arc::default(),
            prospective_fetch_prompt: Arc::default(),
            prospective_fetch_follow_up: Arc::default(),
            prospective_fetch_proposals: Arc::default(),
            pact_routes: Arc::default(),
            pact_proposals: Arc::default(),
            #[cfg(test)]
            policy_registry_override: None,
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
        let player_profile = DeckProfile::analyze(deck);
        let player_features = DeckFeatures::analyze(deck, tier);
        let snapshot = derive_snapshot(&player_features);
        let player_strategy = StrategyProfile::for_profile(&player_profile);
        let graph = SynergyGraph::build(deck);
        session.deck_profile.insert(player, player_profile);
        session.features.insert(player, player_features);
        session.plan.insert(player, snapshot);
        session.strategy.insert(player, player_strategy);
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
    /// player. The production auto-play path builds one `AiSession` at game
    /// start and threads it through decisions, so callers that mutate
    /// `state.deck_pools` must call `invalidate_player_features(player)`
    /// before repopulating.
    pub fn ensure_player_features(
        &mut self,
        player: PlayerId,
        deck: &[DeckEntry],
        tier: engine::game::bracket_estimate::CommanderBracketTier,
    ) {
        if self.features.contains_key(&player) || deck.is_empty() {
            return;
        }
        let profile = DeckProfile::analyze(deck);
        let features = DeckFeatures::analyze(deck, tier);
        let snapshot = derive_snapshot(&features);
        let strategy = StrategyProfile::for_profile(&profile);
        self.deck_profile.insert(player, profile);
        self.features.insert(player, features);
        self.plan.insert(player, snapshot);
        self.strategy.insert(player, strategy);
        self.synergy.insert(player, SynergyGraph::build(deck));
    }

    /// Drop cached per-player features so a subsequent `ensure_player_features`
    /// call repopulates from fresh deck data.
    pub fn invalidate_player_features(&mut self, player: PlayerId) {
        self.deck_profile.remove(&player);
        self.features.remove(&player);
        self.plan.remove(&player);
        self.strategy.remove(&player);
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
    ///
    /// `deadline` bounds only the cache-miss computation; a cache HIT is
    /// returned regardless of expiry. Callers holding an `AiConfig` must pass
    /// `projection::projection_deadline(config.execution_mode)` — evaluated
    /// inline at the call, never hoisted into a `let` that spans multiple
    /// projections.
    pub fn get_or_project(
        &self,
        base: &GameState,
        ai_player: PlayerId,
        target_opponent: PlayerId,
        horizon: ProjectionHorizon,
        deadline: Deadline,
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

        let projection = Arc::new(project_to(
            base,
            ai_player,
            target_opponent,
            horizon,
            deadline,
        )?);

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

/// Digest of exactly the inputs `AiSession::from_game` reads: each pool's
/// player id, bracket tier, and (name, count) of every main-deck and
/// commander entry. Sideboard/planar/scheme/signature and all board/hand
/// state are deliberately excluded — equal fingerprint ⇒ byte-identical
/// session analysis, so a session keyed on this value is safe to reuse.
/// Stable across serde round-trips (hashes content, not Arc identity).
pub fn deck_pools_fingerprint(state: &GameState) -> u64 {
    let mut h = DefaultHasher::new();
    for pool in &state.deck_pools {
        pool.player.0.hash(&mut h);
        pool.bracket_tier.hash(&mut h);
        pool.current_main.len().hash(&mut h);
        for entry in pool.current_main.iter() {
            entry.card.name.hash(&mut h);
            entry.count.hash(&mut h);
        }
        pool.current_commander.len().hash(&mut h);
        for entry in pool.current_commander.iter() {
            entry.card.name.hash(&mut h);
            entry.count.hash(&mut h);
        }
    }
    h.finish()
}

/// Per-thread cache of the last-built session, keyed by deck-composition
/// fingerprint. Mirrors the projection cache's self-invalidation: a changed
/// fingerprint rebuilds; an unchanged one reuses. Used by the WASM bridge,
/// which (unlike native `run_ai_actions`) cannot hold the session on the stack.
#[derive(Debug, Default)]
pub struct SessionCache {
    entry: Option<(u64, Arc<AiSession>)>,
}

impl SessionCache {
    pub const fn new_empty() -> Self {
        Self { entry: None }
    }

    pub fn get_or_build(&mut self, state: &GameState) -> Arc<AiSession> {
        let fp = deck_pools_fingerprint(state);
        if let Some((cached_fp, session)) = &self.entry {
            if *cached_fp == fp {
                return Arc::clone(session);
            }
        }
        let session = AiSession::arc_from_game(state);
        self.entry = Some((fp, Arc::clone(&session)));
        session
    }

    pub fn clear(&mut self) {
        self.entry = None;
    }
}

fn analysis_deck(main: &[DeckEntry], commander: &[DeckEntry]) -> Vec<DeckEntry> {
    let mut deck = Vec::with_capacity(main.len() + commander.len());
    deck.extend_from_slice(main);
    deck.extend(commander.iter().cloned().map(|mut entry| {
        entry.count = entry.count.saturating_mul(COMMANDER_ANALYSIS_WEIGHT);
        entry
    }));
    deck
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
    use engine::game::DeckEntry;
    use engine::types::ability::{
        ContinuousModification, ControllerRef, StaticDefinition, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, PersistedGameState, PlayerDeckPool, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::util::Deadline;

    use crate::projection::BailReason;
    use engine::types::player::PlayerId;
    use engine::types::statics::StaticMode;
    use std::sync::Arc;

    use crate::projection::ProjectionHorizon;

    use super::{deck_pools_fingerprint, AiSession, SessionCache};

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

    fn face(name: &str, core_types: Vec<CoreType>, subtypes: Vec<&str>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types,
                subtypes: subtypes.into_iter().map(str::to_string).collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn deck_entry(card: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card, count }
    }

    fn elf_lord_commander() -> DeckEntry {
        let mut card = face("Elf Lord Commander", vec![CoreType::Creature], vec!["Elf"]);
        card.static_abilities.push(
            StaticDefinition::new(StaticMode::Continuous)
                .affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Subtype("Elf".to_string()))
                        .controller(ControllerRef::You),
                ))
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );
        deck_entry(card, 1)
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
    fn commander_counts_toward_feature_detection_with_buildaround_weight() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.clear();
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(vec![deck_entry(
                face("Neutral Spell", vec![CoreType::Sorcery], Vec::new()),
                99,
            )]),
            current_commander: std::sync::Arc::new(vec![elf_lord_commander()]),
            bracket_tier: CommanderBracketTier::Core,
            ..Default::default()
        });

        let session = AiSession::from_game(&state);
        let tribal = &session
            .features
            .get(&PlayerId(0))
            .expect("player features should be populated")
            .tribal;
        let elf = tribal
            .tribes
            .iter()
            .find(|tribe| tribe.subtype == "Elf")
            .expect("commander tribe should be detected");

        assert_eq!(tribal.dominant_tribe.as_deref(), Some("Elf"));
        assert!(
            tribal.commitment >= crate::features::tribal::LORD_PRIORITY_FLOOR,
            "weighted commander lord should clear tribal feature floors"
        );
        assert_eq!(elf.member_count, 4);
        assert_eq!(elf.lord_count, 4);
    }

    /// Test C — cache-hit primitive: two identical `get_or_project` calls
    /// return the same cached `Arc` (pointer equality) and populate exactly one
    /// cache entry. The already-at-horizon fixture makes `project_to`
    /// short-circuit to `Confidence::Exact` deterministically. A third call
    /// differing only in `ai_player` is a distinct key, so it neither collides
    /// with the cached entry nor reuses its `Arc` — proving key sensitivity.
    #[test]
    fn get_or_project_caches_and_reuses_arc_on_identical_key() {
        let mut s = GameState::new_two_player(42);
        s.turn_number = 2;
        s.active_player = PlayerId(1);
        // reached_horizon only checks non-emptiness of this HashSet.
        s.creatures_attacked_this_turn.insert(ObjectId(1));
        s.stack.clear();
        s.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let session = AiSession::empty();
        let a = session
            .get_or_project(
                &s,
                PlayerId(0),
                PlayerId(1),
                ProjectionHorizon::OpponentAttackersDeclared,
                Deadline::none(),
            )
            .unwrap();
        let b = session
            .get_or_project(
                &s,
                PlayerId(0),
                PlayerId(1),
                ProjectionHorizon::OpponentAttackersDeclared,
                Deadline::none(),
            )
            .unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "second identical get_or_project must return the cached Arc, not recompute"
        );
        assert_eq!(
            session.projection_cache.read().unwrap().len(),
            1,
            "identical keys must collapse to a single cache entry"
        );

        // Key sensitivity: same short-circuiting state but a different
        // `ai_player` is a distinct ProjectionKey (reached_horizon is
        // ai_player-independent, so this still resolves to Exact deterministically).
        let c = session
            .get_or_project(
                &s,
                PlayerId(1),
                PlayerId(1),
                ProjectionHorizon::OpponentAttackersDeclared,
                Deadline::none(),
            )
            .unwrap();
        assert!(
            !Arc::ptr_eq(&a, &c),
            "a different ai_player is a different key and must not reuse the cached Arc"
        );
        assert_eq!(
            session.projection_cache.read().unwrap().len(),
            2,
            "a distinct key must add a second cache entry"
        );
    }

    /// T3 — the deadline reaches `project_to` THROUGH the cache wrapper, and a
    /// bail is not cached.
    ///
    /// Two arms on the same full-loop fixture. Arm 1 is not optional garnish:
    /// without it, arm 2's `len() == 0` cannot distinguish "a bail is not
    /// cached" from "nothing ever caches on this fixture."
    #[test]
    fn get_or_project_forwards_deadline_and_does_not_cache_a_bail() {
        let state = crate::projection::projection_fixtures::opponent_turn_precombat_fixture();
        crate::projection::projection_fixtures::assert_traverses_to(
            &state,
            PlayerId(0),
            PlayerId(1),
            ProjectionHorizon::OpponentAttackersDeclared,
        );

        // Arm 1 — a non-expiring deadline forwards through the wrapper, the
        // projection completes, and the result is cached.
        let session = AiSession::empty();
        let ok = session.get_or_project(
            &state,
            PlayerId(0),
            PlayerId(1),
            ProjectionHorizon::OpponentAttackersDeclared,
            Deadline::none(),
        );
        assert!(
            ok.is_ok(),
            "arm 1: a non-expiring deadline must let the wrapped projection complete; got {:?}",
            ok.err()
        );
        assert_eq!(
            session.projection_cache.read().unwrap().len(),
            1,
            "arm 1: a successful projection must be cached"
        );

        // Arm 2 — a fresh session with a pre-expired deadline: the wrapper must
        // forward it (so the bail is the wall-clock one) and must not cache it.
        let bailing = AiSession::empty();
        let err = bailing.get_or_project(
            &state,
            PlayerId(0),
            PlayerId(1),
            ProjectionHorizon::OpponentAttackersDeclared,
            Deadline::after(0),
        );
        assert!(
            matches!(err, Err(BailReason::TimeCapExceeded { .. })),
            "arm 2: get_or_project must FORWARD its deadline to project_to — ignoring the \
             parameter leaves the projection to complete; got {err:?}"
        );
        assert_eq!(
            bailing.projection_cache.read().unwrap().len(),
            0,
            "arm 2: a bail must not be cached"
        );
    }

    /// T4 — multi-authority hostile fixture: the cache and the deadline both
    /// govern "should this call do work", and on a HIT the cache wins.
    ///
    /// Fails only for one specific wrong implementation — adding an
    /// `if deadline.expired() { return Err(..) }` check ahead of the cache read
    /// in `get_or_project` — which passes every other test in this change.
    /// Deliberately uses the already-at-horizon fixture class, the opposite of
    /// T3's, so the first call is deterministic and free.
    #[test]
    fn get_or_project_serves_cache_hit_under_expired_deadline() {
        let mut s = GameState::new_two_player(42);
        s.turn_number = 2;
        s.active_player = PlayerId(1);
        s.creatures_attacked_this_turn.insert(ObjectId(1));
        s.stack.clear();
        s.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        crate::projection::projection_fixtures::assert_already_at_horizon(
            &s,
            PlayerId(0),
            PlayerId(1),
            ProjectionHorizon::OpponentAttackersDeclared,
        );

        let session = AiSession::empty();
        let first = session
            .get_or_project(
                &s,
                PlayerId(0),
                PlayerId(1),
                ProjectionHorizon::OpponentAttackersDeclared,
                Deadline::none(),
            )
            .expect("the already-at-horizon fixture must project");
        assert_eq!(
            session.projection_cache.read().unwrap().len(),
            1,
            "the first call must populate the cache"
        );

        let second = session
            .get_or_project(
                &s,
                PlayerId(0),
                PlayerId(1),
                ProjectionHorizon::OpponentAttackersDeclared,
                Deadline::after(0),
            )
            .expect(
                "a cache HIT must be served regardless of expiry — the deadline gates \
                 computation, never lookup",
            );
        assert!(
            Arc::ptr_eq(&first, &second),
            "the expired-deadline call must return the cached Arc, not recompute"
        );
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

    /// Cache hit: an unchanged `deck_pools` must reuse the same `Arc<AiSession>`.
    #[test]
    fn session_cache_reuses_when_deck_pools_unchanged() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.clear();
        state
            .deck_pools
            .push(make_pool_with_tier(PlayerId(0), CommanderBracketTier::Core));

        // Reach-guard: pool presence means `from_game` populates features, so
        // the cached session is non-trivial — otherwise `ptr_eq` could pass
        // vacuously on two identical empty sessions.
        assert!(
            AiSession::from_game(&state)
                .archetype(PlayerId(0))
                .is_some(),
            "pool present ⇒ session must populate player features"
        );

        let mut cache = SessionCache::new_empty();
        let first = cache.get_or_build(&state);
        let second = cache.get_or_build(&state);
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged deck_pools must reuse the same Arc<AiSession>"
        );
    }

    /// Deck-composition change: replacing `current_main` must change the
    /// fingerprint and force a rebuild (distinct Arc).
    #[test]
    fn session_cache_rebuilds_when_deck_composition_changes() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.clear();
        state
            .deck_pools
            .push(make_pool_with_tier(PlayerId(0), CommanderBracketTier::Core));

        let mut cache = SessionCache::new_empty();
        let before_fp = deck_pools_fingerprint(&state);
        let first = cache.get_or_build(&state);

        state.deck_pools[0].current_main = Arc::new(vec![deck_entry(
            face("Fresh Card", vec![CoreType::Creature], vec!["Elf"]),
            3,
        )]);
        let after_fp = deck_pools_fingerprint(&state);
        let second = cache.get_or_build(&state);

        assert_ne!(
            before_fp, after_fp,
            "changing current_main must change the fingerprint"
        );
        assert!(
            !Arc::ptr_eq(&first, &second),
            "changed deck composition must rebuild the session"
        );
    }

    /// Serde stability: the fingerprint hashes deck content, not Arc identity,
    /// so it must survive the production persistence round-trip.
    #[test]
    fn fingerprint_is_stable_across_serde_round_trip() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.clear();
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: Arc::new(vec![deck_entry(
                face("Serde Card", vec![CoreType::Creature], vec!["Elf"]),
                2,
            )]),
            current_commander: Arc::new(vec![elf_lord_commander()]),
            bracket_tier: CommanderBracketTier::Core,
            ..Default::default()
        });

        let before = deck_pools_fingerprint(&state);
        let json = serde_json::to_string(&PersistedGameState::capture(state))
            .expect("persisted game state serializes");
        let restored = serde_json::from_str::<PersistedGameState>(&json)
            .expect("persisted game state deserializes")
            .into_game_state()
            .expect("persisted game state satisfies the checked restore contract");
        let after = deck_pools_fingerprint(&restored);

        assert_eq!(
            before, after,
            "fingerprint must survive a serde round-trip (content hash, not Arc identity)"
        );
    }

    /// Bracket-tier axis: two otherwise-identical pools differing only in
    /// `bracket_tier` must produce different fingerprints, because tier is a
    /// `from_game` input.
    #[test]
    fn fingerprint_distinguishes_bracket_tier() {
        let mut core = GameState::new_two_player(42);
        core.deck_pools.clear();
        core.deck_pools
            .push(make_pool_with_tier(PlayerId(0), CommanderBracketTier::Core));

        let mut cedh = GameState::new_two_player(42);
        cedh.deck_pools.clear();
        cedh.deck_pools
            .push(make_pool_with_tier(PlayerId(0), CommanderBracketTier::Cedh));

        assert_ne!(
            deck_pools_fingerprint(&core),
            deck_pools_fingerprint(&cedh),
            "bracket_tier is a session input, so it must be part of the fingerprint"
        );
    }

    /// Hostile: empty `deck_pools` must fingerprint without panic, be
    /// deterministic across calls, and still reuse the cached session.
    #[test]
    fn empty_deck_pools_fingerprint_is_safe_and_deterministic() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools.clear();

        let a = deck_pools_fingerprint(&state);
        let b = deck_pools_fingerprint(&state);
        assert_eq!(a, b, "empty deck_pools must fingerprint deterministically");

        let mut cache = SessionCache::new_empty();
        let first = cache.get_or_build(&state);
        let second = cache.get_or_build(&state);
        assert!(
            Arc::ptr_eq(&first, &second),
            "empty deck_pools must still reuse the cached session"
        );
    }
}
