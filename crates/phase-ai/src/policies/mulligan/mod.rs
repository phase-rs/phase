//! Mulligan policies — sibling trait to `TacticalPolicy` for pre-game hand
//! evaluation.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): the mulligan process — each
//! player may take a mulligan; mulliganed hands shuffle back and the player
//! draws a new hand, putting `mulligan_count` cards on the bottom.
//! CR 103.6 (`docs/MagicCompRules.txt:305`): opening-hand actions after the
//! mulligan process is complete (companion reveals, "begin the game with ~"
//! abilities) — not modeled here, but motivates why the mulligan decision
//! is a first-class AI concern.
//!
//! Each `MulliganPolicy` returns a `MulliganScore` — `ForceKeep`, `ForceMulligan`
//! (hard veto), or `Score { delta, reason }` (additive). The registry runs all
//! registered policies and aggregates with three-way precedence:
//!
//! - Any `ForceKeep` → keep (overrides every other verdict including `ForceMulligan`).
//! - Otherwise any `ForceMulligan` → the hand is mulliganed (reason kept in trace).
//! - Otherwise `sum(delta) > 0.0` means keep.
//!
//! Structured `PolicyReason` values give observability parity with
//! `TacticalPolicy` — `RUST_LOG=phase_ai::decision_trace=debug` emits the
//! per-policy trace.

use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::{card::LayoutKind, card_type::CoreType};

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

pub mod aggro_keepables;
pub mod aristocrats_keepables;
pub mod card_floor;
pub mod cedh_keepables;
pub mod fixed_deck_keepables;
pub mod keepables_by_land_count;
pub mod landfall_keepables;
pub mod plus_one_counters_keepables;
pub mod ramp_keepables;
pub mod spellslinger_keepables;
pub mod tokens_wide_keepables;
pub mod tribal_density;

pub use aggro_keepables::AggroKeepablesMulligan;
pub use aristocrats_keepables::AristocratsKeepablesMulligan;
pub use card_floor::MulliganCardFloor;
pub use cedh_keepables::CedhKeepablesMulligan;
pub use fixed_deck_keepables::FixedDeckKeepMulligan;
pub use keepables_by_land_count::KeepablesByLandCount;
pub use landfall_keepables::LandfallKeepablesMulligan;
pub use plus_one_counters_keepables::PlusOneCountersMulligan;
pub use ramp_keepables::RampKeepablesMulligan;
pub use spellslinger_keepables::SpellslingerKeepablesMulligan;
pub use tokens_wide_keepables::TokensWideKeepablesMulligan;
pub use tribal_density::TribalDensityMulligan;

/// Returns the alternative face only for modal double-faced cards. Other
/// double-faced layouts cannot be played as either face from a hand (CR 712.12).
pub(super) fn modal_back_face(
    object: &engine::game::game_object::GameObject,
) -> Option<&engine::game::game_object::BackFaceData> {
    object
        .back_face
        .as_ref()
        .filter(|face| face.layout_kind == Some(LayoutKind::Modal))
}

/// Whether this hand card can be played as a land. MDFCs are one card, so an
/// alternative land face contributes one land source even while its spell face
/// remains available to the rest of mulligan evaluation (CR 712.12).
pub(super) fn is_land_source(object: &engine::game::game_object::GameObject) -> bool {
    object.card_types.core_types.contains(&CoreType::Land)
        || modal_back_face(object)
            .is_some_and(|face| face.card_types.core_types.contains(&CoreType::Land))
}

/// Whether this hand card has a nonland face that can be cast as a spell.
pub(super) fn has_spell_face(object: &engine::game::game_object::GameObject) -> bool {
    !object.card_types.core_types.contains(&CoreType::Land)
        || modal_back_face(object)
            .is_some_and(|face| !face.card_types.core_types.contains(&CoreType::Land))
}

/// Whether this hand card is a land without a spell face. Upper-bound land
/// heuristics use this rather than [`is_land_source`] so a flexible MDFC is not
/// treated as flood merely because it can also be played as a land.
pub(super) fn is_land_only_source(object: &engine::game::game_object::GameObject) -> bool {
    is_land_source(object) && !has_spell_face(object)
}

/// Conservative lower-bound forecast of actions available from an opening
/// hand through the player's first precombat main phase. It deliberately
/// excludes draws, opponent actions, and resources not already represented in
/// the hand: an inconclusive hand remains keepable rather than becoming a
/// false "dead hand" positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct OpeningHandActionForecast {
    probed: bool,
    has_legal_land_play: bool,
    usable_nonland_mana_source: bool,
    normal_action: bool,
}

impl OpeningHandActionForecast {
    pub(super) fn for_hand(hand: &[ObjectId], state: &GameState) -> Self {
        let Some(player) = hand
            .iter()
            .find_map(|object_id| state.objects.get(object_id).map(|object| object.controller))
        else {
            return Self::default();
        };

        let player_count = state.players.len() as u32;
        if player_count == 0 {
            return Self::default();
        }
        let mut forecast = Self::default();
        let first_turn = 1
            + (u32::from(player.0) + player_count - u32::from(state.current_starting_player.0))
                % player_count;
        for turn_number in [first_turn] {
            // The engine owns cast/activation legality, target availability, and
            // payment. The probe is the player's first precombat-main priority
            // window, with no draw, opponent action, or speculative
            // resource added to the clone. A positive result is therefore a
            // conservative lower-bound witness from this opening hand.
            let mut opening_main = state.clone();
            opening_main.turn_number = turn_number;
            opening_main.phase = Phase::PreCombatMain;
            opening_main.active_player = player;
            opening_main.priority_player = player;
            opening_main.waiting_for = WaitingFor::Priority { player };
            let (actions, _, actions_by_object) =
                engine::ai_support::legal_actions_full(&opening_main);

            for action in &actions {
                match action {
                    GameAction::PlayLand { object_id, .. } if hand.contains(object_id) => {
                        forecast.has_legal_land_play = true;
                    }
                    action if is_normal_opening_hand_action(action, hand) => {
                        forecast.normal_action = true;
                    }
                    _ => {}
                }
            }
            forecast.usable_nonland_mana_source |=
                actions_by_object.iter().any(|(object_id, actions)| {
                    hand.contains(object_id)
                        && opening_main.objects.get(object_id).is_some_and(|object| {
                            !is_land_source(object) && actions.iter().any(|action| {
                            matches!(
                                action,
                                GameAction::ActivateAbility { ability_index, .. }
                                    if object
                                        .abilities
                                        .get(*ability_index)
                                        .is_some_and(engine::game::mana_abilities::is_mana_ability)
                            )
                        })
                        })
                });
        }

        forecast.probed = true;
        forecast
    }

    /// A certified-dead hand has no legal land play (including an MDFC land),
    /// no immediately usable nonland mana source, and no normal cast or
    /// activation within this deliberately bounded model.
    pub(super) fn is_certified_dead_landless(self) -> bool {
        self.probed
            && !self.has_legal_land_play
            && !self.usable_nonland_mana_source
            && !self.normal_action
    }
}

fn is_normal_opening_hand_action(action: &GameAction, hand: &[ObjectId]) -> bool {
    match action {
        GameAction::CastSpell { object_id, .. }
        | GameAction::CastSpellForFree { object_id, .. }
        | GameAction::CastSpellAsMiracle { object_id, .. }
        | GameAction::CastSpellAsMadness { object_id, .. }
        | GameAction::Foretell { object_id, .. }
        | GameAction::PlayFaceDown { object_id, .. }
        | GameAction::ActivateAbility {
            source_id: object_id,
            ..
        } => hand.contains(object_id),
        GameAction::CastSpellAsSneak { hand_object, .. }
        | GameAction::CastSpellAsWebSlinging { hand_object, .. } => hand.contains(hand_object),
        _ => false,
    }
}

/// Whether the player under consideration is on the play or on the draw this
/// game. Derived from `GameState::current_starting_player` at call time —
/// `OnPlay` when the mulliganing player started the game, otherwise `OnDraw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOrder {
    OnPlay,
    OnDraw,
}

/// A single mulligan policy's verdict on an opening hand.
#[derive(Debug, Clone)]
pub enum MulliganScore {
    /// Hard veto toward keeping — outranks `ForceMulligan`. A policy emits this
    /// when the hand must not be mulliganed regardless of other verdicts
    /// (e.g. a card-count floor).
    ForceKeep { reason: PolicyReason },
    /// Hard veto — if any policy returns this (and none returns `ForceKeep`),
    /// the hand is mulliganed.
    ForceMulligan { reason: PolicyReason },
    /// Additive score contribution. Positive = prefer keeping; negative =
    /// prefer mulliganing.
    Score { delta: f64, reason: PolicyReason },
}

/// Aggregated decision produced by `MulliganRegistry::evaluate_hand`.
#[derive(Debug, Clone)]
pub struct MulliganDecision {
    pub keep: bool,
    pub trace: Vec<(PolicyId, MulliganScore)>,
}

/// Pre-game hand evaluation. Shares inputs with `TacticalPolicy` (features,
/// plan) but uses a different scoring interface — mulligan is a one-shot
/// choice, not a ranking over candidates.
pub trait MulliganPolicy: Send + Sync {
    fn id(&self) -> PolicyId;
    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        plan: &PlanSnapshot,
        turn_order: TurnOrder,
        mulligans_taken: u8,
    ) -> MulliganScore;
}

/// Registry of mulligan policies. Aggregates per-policy verdicts into a
/// single `MulliganDecision` with three-way precedence:
/// any `ForceKeep` → keep (overrides everything); else any `ForceMulligan` →
/// mulligan; else `sum(delta) > 0.0` → keep.
pub struct MulliganRegistry {
    policies: Vec<Box<dyn MulliganPolicy>>,
}

impl Default for MulliganRegistry {
    fn default() -> Self {
        Self {
            policies: vec![
                // First so the decision trace reads floor-first; the position is
                // cosmetic — `evaluate_hand` consults every policy before deciding.
                Box::new(MulliganCardFloor),
                Box::new(KeepablesByLandCount),
                Box::new(LandfallKeepablesMulligan),
                Box::new(RampKeepablesMulligan),
                Box::new(TribalDensityMulligan),
                Box::new(AristocratsKeepablesMulligan),
                Box::new(AggroKeepablesMulligan),
                Box::new(TokensWideKeepablesMulligan),
                Box::new(PlusOneCountersMulligan),
                Box::new(SpellslingerKeepablesMulligan),
                Box::new(CedhKeepablesMulligan::new()),
                Box::new(FixedDeckKeepMulligan),
            ],
        }
    }
}

impl MulliganRegistry {
    pub fn evaluate_hand(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        plan: &PlanSnapshot,
        turn_order: TurnOrder,
        mulligans_taken: u8,
    ) -> MulliganDecision {
        let mut trace = Vec::with_capacity(self.policies.len());
        let mut forced_keep = false;
        let mut forced_mulligan = false;
        let mut total: f64 = 0.0;
        for policy in &self.policies {
            let score = policy.evaluate(hand, state, features, plan, turn_order, mulligans_taken);
            match &score {
                MulliganScore::ForceKeep { .. } => forced_keep = true,
                MulliganScore::ForceMulligan { .. } => forced_mulligan = true,
                MulliganScore::Score { delta, .. } => total += *delta,
            }
            trace.push((policy.id(), score));
        }

        let keep = if forced_keep {
            true
        } else if forced_mulligan {
            false
        } else {
            total > 0.0
        };

        if tracing::event_enabled!(target: "phase_ai::decision_trace", tracing::Level::DEBUG) {
            tracing::debug!(
                target: "phase_ai::decision_trace",
                ?trace,
                keep,
                mulligans_taken,
                "mulligan decision"
            );
        }

        MulliganDecision { keep, trace }
    }
}

/// Derive `TurnOrder` from the game state for a given player. CR 103.5 —
/// the starting player declares first; subsequent mulligans follow turn
/// order. For the purpose of evaluating hand quality, what matters is
/// whether this player will be on the play (extra tempo, no free draw) or
/// on the draw (free card, slower clock).
pub fn turn_order_for(state: &GameState, player: PlayerId) -> TurnOrder {
    if state.current_starting_player == player {
        TurnOrder::OnPlay
    } else {
        TurnOrder::OnDraw
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod cedh_registration_tests {
    use std::sync::Arc;

    use engine::game::bracket_estimate::CommanderBracketTier;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr,
        TargetFilter, TypedFilter,
    };
    use engine::types::actions::MulliganChoice;
    use engine::types::card_type::CardType;
    use engine::types::game_state::{MulliganDecisionEntry, MulliganDecisionPhase, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    use super::*;
    use crate::features::DeckFeatures;
    use crate::plan::PlanSnapshot;
    use crate::policies::registry::PolicyId;

    #[test]
    fn default_registry_contains_cedh_keepables() {
        let reg = MulliganRegistry::default();
        let has = reg
            .policies
            .iter()
            .any(|p| p.id() == PolicyId::CedhKeepablesMulligan);
        assert!(
            has,
            "MulliganRegistry::default() must register CedhKeepablesMulligan"
        );
    }

    #[test]
    fn default_registry_contains_fixed_deck_keepables() {
        let reg = MulliganRegistry::default();
        let has = reg
            .policies
            .iter()
            .any(|p| p.id() == PolicyId::FixedDeckKeepMulligan);
        assert!(
            has,
            "MulliganRegistry::default() must register FixedDeckKeepMulligan \
             so Momir-family all-land hands are kept, not mulliganed to zero"
        );
    }

    #[test]
    fn default_registry_contains_card_floor() {
        let reg = MulliganRegistry::default();
        assert!(
            reg.policies
                .iter()
                .any(|p| p.id() == PolicyId::MulliganCardFloor),
            "MulliganRegistry::default() must register MulliganCardFloor so no deck \
             can chain-mulligan below the card floor"
        );
    }

    /// Minimal policy that always emits `ForceKeep`.
    struct AlwaysForceKeep;
    impl MulliganPolicy for AlwaysForceKeep {
        fn id(&self) -> PolicyId {
            PolicyId::CedhKeepablesMulligan
        }
        fn evaluate(
            &self,
            _hand: &[engine::types::identifiers::ObjectId],
            _state: &GameState,
            _features: &DeckFeatures,
            _plan: &PlanSnapshot,
            _turn_order: TurnOrder,
            _mulligans_taken: u8,
        ) -> MulliganScore {
            MulliganScore::ForceKeep {
                reason: PolicyReason::new("test_force_keep"),
            }
        }
    }

    /// Minimal policy that always emits `ForceMulligan`.
    struct AlwaysForceMulligan;
    impl MulliganPolicy for AlwaysForceMulligan {
        fn id(&self) -> PolicyId {
            PolicyId::KeepablesByLandCount
        }
        fn evaluate(
            &self,
            _hand: &[engine::types::identifiers::ObjectId],
            _state: &GameState,
            _features: &DeckFeatures,
            _plan: &PlanSnapshot,
            _turn_order: TurnOrder,
            _mulligans_taken: u8,
        ) -> MulliganScore {
            MulliganScore::ForceMulligan {
                reason: PolicyReason::new("test_force_mulligan"),
            }
        }
    }

    /// Minimal policy that always emits a strongly negative additive score —
    /// stands in for the archetype policies' depth-blind negatives, which sum to
    /// roughly −7.9 in the worst case and can outvote `KeepablesByLandCount`'s
    /// lone `Score { +2.0 }` at any mulligan depth.
    struct AlwaysNegativeScore;
    impl MulliganPolicy for AlwaysNegativeScore {
        fn id(&self) -> PolicyId {
            PolicyId::TribalDensityMulligan
        }
        fn evaluate(
            &self,
            _hand: &[engine::types::identifiers::ObjectId],
            _state: &GameState,
            _features: &DeckFeatures,
            _plan: &PlanSnapshot,
            _turn_order: TurnOrder,
            _mulligans_taken: u8,
        ) -> MulliganScore {
            MulliganScore::Score {
                delta: -9.0,
                reason: PolicyReason::new("test_always_negative"),
            }
        }
    }

    /// Add a card to `state` in `Zone::Hand` for player 0; returns its
    /// `ObjectId`. Mirrors `cedh_keepables.rs`'s private helper, with a distinct
    /// `CardId` base so the two test modules cannot be confused when read side
    /// by side.
    fn add_hand_card(
        state: &mut GameState,
        idx: u64,
        name: &str,
        core_types: Vec<CoreType>,
    ) -> ObjectId {
        let oid = create_object(
            state,
            CardId(4000 + idx),
            PlayerId(0),
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).expect("just created");
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types,
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(1);
        obj.base_card_types = obj.card_types.clone();
        obj.base_mana_cost = obj.mana_cost.clone();
        oid
    }

    fn add_zero_cost_action(state: &mut GameState, idx: u64) -> ObjectId {
        let object_id = add_hand_card(state, idx, "Zero-Cost Action", vec![CoreType::Artifact]);
        let object = state.objects.get_mut(&object_id).expect("just created");
        // `NoCost` is an absent, unpayable mana cost; this fixture models a
        // castable `{0}` spell for the opening-action witness.
        object.mana_cost = ManaCost::generic(0);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        object.base_mana_cost = object.mana_cost.clone();
        object.base_abilities = Arc::clone(&object.abilities);
        object_id
    }

    fn add_unaffordable_hand_activation(state: &mut GameState, idx: u64) -> ObjectId {
        let object_id = add_hand_card(
            state,
            idx,
            "Unaffordable Hand Activation",
            vec![CoreType::Artifact],
        );
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        ability.activation_zone = Some(Zone::Hand);
        ability.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(2),
        });
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&object_id)
                .expect("just created")
                .abilities,
        )
        .push(ability);
        let object = state.objects.get_mut(&object_id).expect("just created");
        object.base_abilities = Arc::clone(&object.abilities);
        object_id
    }

    fn add_targetless_zero_cost_spell(state: &mut GameState, idx: u64) -> ObjectId {
        let object_id = add_hand_card(
            state,
            idx,
            "Targetless Zero-Cost Spell",
            vec![CoreType::Instant],
        );
        let object = state.objects.get_mut(&object_id).expect("just created");
        object.mana_cost = ManaCost::generic(0);
        let abilities = Arc::make_mut(&mut object.abilities);
        abilities.clear();
        abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
        ));
        object.base_mana_cost = object.mana_cost.clone();
        object.base_abilities = Arc::clone(&object.abilities);
        object_id
    }

    fn add_zero_cost_mana_source(state: &mut GameState, idx: u64) -> ObjectId {
        let object_id = add_hand_card(
            state,
            idx,
            "Zero-Cost Mana Source",
            vec![CoreType::Artifact],
        );
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            engine::types::ability::Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability.activation_zone = Some(Zone::Hand);
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&object_id)
                .expect("just created")
                .abilities,
        )
        .push(ability);
        let object = state.objects.get_mut(&object_id).expect("just created");
        object.base_abilities = Arc::clone(&object.abilities);
        object_id
    }

    /// A `GameState` sitting on the mulligan step, non-free-first, plus a real
    /// landless hand of `hand_size` nonland spells (naming the first card, for
    /// the Serum Powder pin).
    ///
    /// The hand must be real, not `&[]`: `KeepablesByLandCount` reads it, and
    /// an empty hand would take a different branch. And `waiting_for` must be
    /// set explicitly — the `GameState::new_two_player` default is not
    /// `MulliganDecision`, under which `MulliganCardFloor` abstains and every
    /// `keep == true` assertion below would fail for an unrelated reason.
    ///
    /// `hand_size` is a real axis, not a convenience: `7` is the only size
    /// production presents at a first `Declare` under the London mulligan, while
    /// `<= 4` covers the short-hand shapes that must not receive an automatic
    /// floor keep when they are certified dead.
    fn landless_hand_on_mulligan_step(
        first_card_name: &str,
        hand_size: u64,
    ) -> (GameState, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(0);
        state.players[0].hand.clear();
        let mut hand = vec![add_hand_card(
            &mut state,
            0,
            first_card_name,
            vec![CoreType::Artifact],
        )];
        for i in 1..hand_size {
            hand.push(add_hand_card(
                &mut state,
                i,
                &format!("Filler {i}"),
                vec![CoreType::Creature],
            ));
        }
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };
        (state, hand)
    }

    fn live_mulligan_declare_state(first_card_name: &str) -> (GameState, Vec<ObjectId>) {
        let (mut state, hand) = landless_hand_on_mulligan_step(first_card_name, 7);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 4,
                phase: MulliganDecisionPhase::Declare,
            }],
            free_first_mulligan: false,
        };
        (state, hand)
    }

    /// `ForceKeep` must override a co-occurring `ForceMulligan` — the hand is kept.
    #[test]
    fn force_keep_overrides_force_mulligan() {
        let registry = MulliganRegistry {
            policies: vec![Box::new(AlwaysForceKeep), Box::new(AlwaysForceMulligan)],
        };
        let state = GameState::new_two_player(0);
        let decision = registry.evaluate_hand(
            &[],
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            decision.keep,
            "ForceKeep must override ForceMulligan; expected keep=true, got keep=false"
        );
    }

    /// Without `ForceKeep`, a lone `ForceMulligan` produces `keep=false`.
    #[test]
    fn force_mulligan_alone_produces_mulligan() {
        let registry = MulliganRegistry {
            policies: vec![Box::new(AlwaysForceMulligan)],
        };
        let state = GameState::new_two_player(0);
        let decision = registry.evaluate_hand(
            &[],
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            !decision.keep,
            "ForceMulligan without ForceKeep must produce keep=false"
        );
    }

    /// V7 — end-to-end: after the floor was lifted out of
    /// `CedhKeepablesMulligan` into `MulliganCardFloor`, a cEDH deck's `keep`
    /// is unchanged. The real floor policy's `ForceKeep` must still override the
    /// real cEDH policy's `ForceMulligan` through the registry's three-way
    /// aggregation — the whole point of the feature.
    #[test]
    fn cedh_floor_force_keep_overrides_force_mulligan_in_registry() {
        let cedh_features = DeckFeatures {
            bracket_tier: CommanderBracketTier::Cedh,
            ..DeckFeatures::default()
        };
        // `MulliganCardFloor` abstains off-step, so `waiting_for` must be set
        // explicitly: with `free_first_mulligan: false` the floor engages at
        // mulligans_taken == 3 (`kept_hand_size_after(4, false) == 3 < 4`). An
        // A free normal action makes this hand non-dead, so this test still
        // isolates the registry's ForceKeep precedence.
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };
        let hand = vec![add_zero_cost_action(&mut state, 99)];

        let registry = MulliganRegistry {
            policies: vec![
                Box::new(MulliganCardFloor),
                Box::new(CedhKeepablesMulligan::new()),
                Box::new(AlwaysForceMulligan),
            ],
        };
        let decision = registry.evaluate_hand(
            &hand,
            &state,
            &cedh_features,
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            3,
        );
        assert!(
            decision.keep,
            "the universal floor's ForceKeep must override a real ForceMulligan \
             for a cEDH deck; expected keep=true at mulligans_taken=3, got keep=false"
        );

        // Contrast: at mulligans_taken == 0 the floor is not engaged, so the
        // real cEDH policy force-mulligans the empty hand (< 2 lands) and the
        // registry mulligans.
        let decision_no_floor = registry.evaluate_hand(
            &hand,
            &state,
            &cedh_features,
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            !decision_no_floor.keep,
            "without the floor engaged, the real cEDH policy must mulligan; \
             expected keep=false at mulligans_taken=0, got keep=true"
        );
    }

    /// V5 — the floor wins the three-way aggregation for an ORDINARY
    /// (non-cEDH, non-fixed-deck) deck, through the REAL
    /// `MulliganRegistry::default()` rather than a hand-built vec, so
    /// de-registration is caught too.
    ///
    /// Before the floor existed this landless 7-card hand at `mulligans_taken = 4`
    /// hit `KeepablesByLandCount`'s `hand_lenient_reject` `ForceMulligan` and
    /// `keep` was false — this is the AI chain-mulliganing an ordinary deck
    /// toward a zero-card hand.
    #[test]
    fn default_registry_mulligans_certified_dead_hand_at_floor() {
        let (state, hand) = landless_hand_on_mulligan_step("Opening Spell", 7);
        let registry = MulliganRegistry::default();

        let decision = registry.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            4,
        );
        assert!(
            !decision.keep,
            "a certified dead landless hand must remain mulliganable at the card floor; \
             expected keep=false at mulligans_taken=4"
        );

        // Contrast: outside the floor band the same hand is still mulliganed, so
        // the floor is depth-bounded rather than an unconditional keep. This
        // reuses the SAME `state` deliberately — a bare `GameState` would also
        // yield keep=false (the floor would abstain off-step), making the
        // contrast pass for the wrong reason.
        let decision_above_floor = registry.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            2,
        );
        assert!(
            !decision_above_floor.keep,
            "above the floor band the landless hand must still be mulliganed; \
             expected keep=false at mulligans_taken=2 (kept_hand_size_after(3,false)==4)"
        );
    }

    #[test]
    fn actionable_mdfc_and_fast_mana_hands_stay_keepable_at_floor() {
        let registry = MulliganRegistry::default();

        let mut mdfc_state = GameState::new_two_player(0);
        let mdfc = add_hand_card(&mut mdfc_state, 10, "Modal Land", vec![CoreType::Instant]);
        let mut modal_back_face = engine::game::printed_cards::snapshot_object_face(
            mdfc_state.objects.get(&mdfc).expect("just created"),
        );
        modal_back_face.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Land],
            subtypes: Vec::new(),
        };
        modal_back_face.layout_kind = Some(engine::types::card::LayoutKind::Modal);
        let mdfc_object = mdfc_state.objects.get_mut(&mdfc).expect("just created");
        mdfc_object.back_face = Some(modal_back_face);
        mdfc_state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };

        let mdfc_forecast = OpeningHandActionForecast::for_hand(&[mdfc], &mdfc_state);
        assert!(
            !mdfc_forecast.is_certified_dead_landless(),
            "an MDFC with a legal land face must not be CertifiedDeadLandless"
        );
        assert!(
            matches!(
                MulliganCardFloor.evaluate(
                    &[mdfc],
                    &mdfc_state,
                    &DeckFeatures::default(),
                    &PlanSnapshot::default(),
                    TurnOrder::OnPlay,
                    3,
                ),
                MulliganScore::ForceKeep { ref reason } if reason.kind == "mulligan_card_floor"
            ),
            "the floor itself must ForceKeep the MDFC at depth three; the registry's land-count policy is not evidence for this classification"
        );

        let mdfc_decision = registry.evaluate_hand(
            &[mdfc],
            &mdfc_state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            3,
        );
        assert!(
            mdfc_decision.keep,
            "an MDFC land play must not be certified dead"
        );

        let mut fast_mana_state = GameState::new_two_player(0);
        let fast_mana = add_zero_cost_mana_source(&mut fast_mana_state, 11);
        fast_mana_state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };
        let fast_mana_forecast =
            OpeningHandActionForecast::for_hand(&[fast_mana], &fast_mana_state);
        assert!(
            fast_mana_forecast.usable_nonland_mana_source,
            "the fixture must witness a hand-activatable mana source, not a castable spell"
        );

        let fast_mana_decision = registry.evaluate_hand(
            &[fast_mana],
            &fast_mana_state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            3,
        );
        assert!(
            fast_mana_decision.keep,
            "an immediate nonland action must not be certified dead"
        );
    }

    #[test]
    fn forecast_rejects_unaffordable_or_targetless_opening_actions() {
        let mut activation_state = GameState::new_two_player(0);
        let activation = add_unaffordable_hand_activation(&mut activation_state, 50);
        activation_state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };
        assert!(
            OpeningHandActionForecast::for_hand(&[activation], &activation_state)
                .is_certified_dead_landless(),
            "a hand-zone activation with an unpaid {{2}} cost is not an opening action witness"
        );

        let mut targeted_spell_state = GameState::new_two_player(0);
        let targeted_spell = add_targetless_zero_cost_spell(&mut targeted_spell_state, 51);
        targeted_spell_state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };
        assert!(
            OpeningHandActionForecast::for_hand(&[targeted_spell], &targeted_spell_state)
                .is_certified_dead_landless(),
            "a zero-cost spell with no legal creature target is not an opening action witness"
        );
    }

    #[test]
    fn momir_all_land_hand_force_keeps_without_dead_hand_override() {
        let mut state = GameState::new_two_player(0);
        state.format_config = engine::types::format::FormatConfig::momir();
        let hand: Vec<_> = (0..7)
            .map(|index| {
                add_hand_card(
                    &mut state,
                    index,
                    &format!("Momir Land {index}"),
                    vec![CoreType::Land],
                )
            })
            .collect();
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };

        assert!(
            !OpeningHandActionForecast::for_hand(&hand, &state).is_certified_dead_landless(),
            "an all-land Momir hand has a legal opening land play"
        );

        let decision = MulliganRegistry::default().evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            4,
        );
        assert!(
            decision.keep,
            "the fixed-deck force keep must retain global precedence over ForceMulligan"
        );
    }

    /// V12 — a short, production-reachable hand is not granted a floor keep
    /// when its bounded opening forecast proves it cannot act.
    ///
    /// That branch was reachable in production, contrary to the
    /// "unreachable from production paths today" comment removed with it:
    /// `handle_serum_powder` redraws `exiled_count` cards and
    /// `handle_mulligan_bottom` credits `prepaid_mulligan_bottoms`, so a
    /// post-Powder re-entry reaches `Declare` holding
    /// `kept_hand_size_after(mulligans_taken, free_first)` cards — four or fewer
    /// once `mulligans_taken >= 3`. A 4-card landless hand at
    /// `mulligans_taken = 3` is the only fixture shape that distinguishes "the
    /// floor covers the deleted branch's domain" from "nobody covers it": every
    /// other `KeepablesByLandCount` fixture in the tree holds 5 cards or more and
    /// never entered that branch.
    ///
    #[test]
    fn certified_dead_short_hand_is_not_kept_by_floor() {
        let (state, hand) = landless_hand_on_mulligan_step("Post-Powder Spell", 4);
        assert_eq!(
            hand.len(),
            4,
            "fixture must sit inside the deleted hand_size<=4 branch's domain"
        );
        let registry = MulliganRegistry::default();

        let decision = registry.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            3,
        );
        assert!(
            !decision.keep,
            "a 4-card landless hand at mulligans_taken=3 must remain mulliganable when \
             the opening forecast certifies no action; expected keep=false"
        );

        // Band-dependence instrument, on the SAME state: hold hand size at 4 and
        // move only the depth to 2 — outside the floor band — and the hand is
        // mulliganed. This isolates depth as the deciding variable, so the keep
        // above cannot be an unconditional one.
        //
        // NOTE this combination is NOT production-reachable: a non-free-first
        // `Declare` hand of 4 implies mulligans_taken == 3, because the hand is
        // `kept_hand_size_after(mulligans_taken, false) == 7 - mulligans_taken`.
        // It therefore carries evidence about the *predicate*, not about shipped
        // behaviour, and must not be counted as production-path coverage. The
        // production-reachable negative is the third assertion below.
        let decision_above_floor = registry.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            2,
        );
        assert!(
            !decision_above_floor.keep,
            "outside the floor band a 4-card landless hand is mulliganed; the deleted \
             branch's unconditional short-hand keep is intentionally not restored"
        );

        // Production-reachable negative, and the real boundary. A post-Powder
        // re-entry at `mulligans_taken = 2` holds
        // `kept_hand_size_after(2, false) == 5` cards — one card above the deleted
        // branch's `<= 4` domain, and one depth above the floor's band — so it is
        // mulliganed by both the old code and the new. Paired with the keep at
        // (4 cards, depth 3) this pins that the floor takes over exactly where the
        // hand first drops into the deleted branch's domain, with no gap and no
        // overlap.
        let (state_five, hand_five) = landless_hand_on_mulligan_step("Post-Powder Spell", 5);
        let decision_five = registry.evaluate_hand(
            &hand_five,
            &state_five,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            2,
        );
        assert!(
            !decision_five.keep,
            "a 5-card landless hand at mulligans_taken=2 — the production post-Powder \
             shape one step above the floor band — must still be mulliganed; expected \
             keep=false (kept_hand_size_after(3,false)==4, so the floor abstains)"
        );
    }

    /// V6 — the floor also beats the ADDITIVE-NEGATIVE path, which no existing
    /// test covers. `KeepablesByLandCount`'s dominant deep-mulligan outcome is
    /// `Score { +2.0 }`, not `ForceMulligan`, and the eight archetype policies —
    /// every one of which binds `_mulligans_taken` as `input-unused` — can
    /// outvote it at any depth. `ForceKeep` is checked before the
    /// `else { total > 0.0 }` arm, so one floor closes both routes.
    #[test]
    fn floor_overrides_negative_total() {
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan: false,
        };
        let hand = vec![add_zero_cost_action(&mut state, 99)];

        let with_floor = MulliganRegistry {
            policies: vec![Box::new(MulliganCardFloor), Box::new(AlwaysNegativeScore)],
        };
        let decision = with_floor.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            4,
        );
        assert!(
            decision.keep,
            "ForceKeep must outrank a strongly negative additive total; \
             expected keep=true at mulligans_taken=4, got keep=false"
        );

        // Non-vacuity: the same registry WITHOUT the floor mulligans on the
        // identical state, so the keep above is the floor's doing and not an
        // artifact of the aggregation.
        let without_floor = MulliganRegistry {
            policies: vec![Box::new(AlwaysNegativeScore)],
        };
        let decision_unfloored = without_floor.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            4,
        );
        assert!(
            !decision_unfloored.keep,
            "without the floor a -9.0 total must mulligan; expected keep=false"
        );

        // Contrast: with the floor present but outside its band the negative
        // total still wins. Reuses the SAME `state` — a bare `GameState` would
        // also yield keep=false, which would make this pass for the wrong reason.
        let decision_above_floor = with_floor.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            2,
        );
        assert!(
            !decision_above_floor.keep,
            "above the floor band the negative total must still mulligan; \
             expected keep=false at mulligans_taken=2"
        );
    }

    /// A dead Serum Powder-shaped hand is still eligible for the existing
    /// mulligan-time action path because the floor abstains rather than keeping
    /// it solely for card count.
    #[test]
    fn certified_dead_hand_keeps_serum_powder_path_reachable() {
        let (state, hand) = landless_hand_on_mulligan_step("Serum Powder", 7);
        let registry = MulliganRegistry::default();

        let decision = registry.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            4,
        );
        assert!(
            !decision.keep,
            "a certified dead hand must stay mulliganable in the floor band so the \
             existing Serum Powder dispatch remains reachable"
        );

        let decision_above_floor = registry.evaluate_hand(
            &hand,
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            2,
        );
        assert!(
            !decision_above_floor.keep,
            "outside the floor band the Serum Powder path must remain reachable; \
             expected keep=false at mulligans_taken=2"
        );
    }

    /// Production-path pin for the actual chooser. The public search entry
    /// reads the real pending `Declare` entry, evaluates the default registry,
    /// and then maps a non-keep to Mulligan or Serum Powder. If card-floor
    /// abstention is removed, both rows regress to `Keep`.
    #[test]
    fn live_mulligan_chooser_mulligans_dead_hand_and_uses_serum_powder() {
        use rand::rngs::SmallRng;
        use rand::SeedableRng;

        let config = crate::config::create_config(
            crate::config::AiDifficulty::VeryHard,
            crate::config::Platform::Native,
        );

        let (dead_state, _) = live_mulligan_declare_state("Dead Opening Spell");
        let mut dead_rng = SmallRng::seed_from_u64(1);
        assert_eq!(
            crate::search::choose_action(&dead_state, PlayerId(0), &config, &mut dead_rng),
            Some(GameAction::MulliganDecision {
                choice: MulliganChoice::Mulligan,
            }),
            "the live chooser must carry the certified-dead floor abstention through to Mulligan"
        );

        let (powder_state, powder_hand) = live_mulligan_declare_state("Serum Powder");
        let powder_id = powder_hand[0];
        let mut powder_rng = SmallRng::seed_from_u64(2);
        assert_eq!(
            crate::search::choose_action(&powder_state, PlayerId(0), &config, &mut powder_rng),
            Some(GameAction::MulliganDecision {
                choice: MulliganChoice::UseSerumPowder {
                    object_id: powder_id,
                },
            }),
            "the live chooser must leave the existing Serum Powder dispatch reachable for a certified-dead hand"
        );
    }
}
