//! Test harness for constructing game states with inline card definitions.
//!
//! Provides `GameScenario` (mutable builder), `CardBuilder` (fluent keyword/ability chaining),
//! `GameRunner` (step-by-step execution), and `GameSnapshot` (insta-compatible projections).
//! Zero filesystem dependencies -- all cards are constructed inline.

use serde::{Deserialize, Serialize};

use crate::game::engine::{apply, EngineError};
use crate::game::game_object::GameObject;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, DamageAmount, Effect, StaticDefinition, TargetFilter,
    TriggerDefinition,
};
use crate::types::actions::GameAction;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// Convenience constant for Player 0.
pub const P0: PlayerId = PlayerId(0);
/// Convenience constant for Player 1.
pub const P1: PlayerId = PlayerId(1);

// ---------------------------------------------------------------------------
// GameScenario (mutable builder)
// ---------------------------------------------------------------------------

/// Mutable builder that constructs a GameState with predefined board state,
/// phase, turn, and card objects -- all with zero filesystem dependencies.
pub struct GameScenario {
    state: GameState,
}

impl Default for GameScenario {
    fn default() -> Self {
        Self::new()
    }
}

impl GameScenario {
    /// Create a new scenario with a default two-player game (20 life each, seed 42).
    pub fn new() -> Self {
        GameScenario {
            state: GameState::new_two_player(42),
        }
    }

    /// Create a scenario with N players using the default format config (20 life each).
    pub fn new_n_player(count: u8, seed: u64) -> Self {
        GameScenario {
            state: GameState::new(crate::types::format::FormatConfig::standard(), count, seed),
        }
    }

    /// Set the game phase. Also sets `waiting_for`, `priority_player`, `active_player`,
    /// and `turn_number` consistently to avoid common test pitfalls.
    pub fn at_phase(&mut self, phase: Phase) -> &mut Self {
        self.state.phase = phase;
        self.state.turn_number = 2;
        self.state.waiting_for = WaitingFor::Priority {
            player: self.state.active_player,
        };
        self.state.priority_player = self.state.active_player;
        self
    }

    /// Set a player's life total.
    pub fn with_life(&mut self, player: PlayerId, life: i32) -> &mut Self {
        if let Some(p) = self.state.players.iter_mut().find(|p| p.id == player) {
            p.life = life;
        }
        self
    }

    /// Add a creature to the battlefield. Returns a `CardBuilder` for fluent chaining.
    pub fn add_creature(
        &mut self,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> CardBuilder<'_> {
        let card_id = CardId(self.state.next_object_id);
        let id = create_object(
            &mut self.state,
            card_id,
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let ts = self.state.next_timestamp();
        let entered_turn = self.state.turn_number.saturating_sub(1);
        let obj = self.state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(entered_turn);
        obj.timestamp = ts;

        CardBuilder {
            state: &mut self.state,
            id,
        }
    }

    /// Add a nameless vanilla creature to the battlefield. Returns its `ObjectId`.
    pub fn add_vanilla(&mut self, player: PlayerId, power: i32, toughness: i32) -> ObjectId {
        self.add_creature(
            player,
            &format!("{}/{} Vanilla", power, toughness),
            power,
            toughness,
        )
        .id()
    }

    /// Add a basic land to the battlefield. Returns its `ObjectId`.
    pub fn add_basic_land(&mut self, player: PlayerId, color: ManaColor) -> ObjectId {
        let name = match color {
            ManaColor::White => "Plains",
            ManaColor::Blue => "Island",
            ManaColor::Black => "Swamp",
            ManaColor::Red => "Mountain",
            ManaColor::Green => "Forest",
        };
        let card_id = CardId(self.state.next_object_id);
        let id = create_object(
            &mut self.state,
            card_id,
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = self.state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.entered_battlefield_turn = Some(self.state.turn_number.saturating_sub(1));
        // Add mana ability
        obj.abilities.push(AbilityDefinition {
            kind: AbilityKind::Activated,
            effect: Effect::Mana {
                produced: crate::types::ability::ManaProduction::Fixed {
                    colors: vec![color],
                },
            },
            cost: Some(crate::types::ability::AbilityCost::Tap),
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        });
        id
    }

    /// Add a "Lightning Bolt" instant to a player's hand. Returns its `ObjectId`.
    pub fn add_bolt_to_hand(&mut self, player: PlayerId) -> ObjectId {
        let card_id = CardId(self.state.next_object_id);
        let id = create_object(
            &mut self.state,
            card_id,
            player,
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        let obj = self.state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.abilities.push(AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        });
        id
    }

    /// Add a creature to a player's hand. Returns a `CardBuilder` for fluent chaining.
    pub fn add_creature_to_hand(
        &mut self,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> CardBuilder<'_> {
        let card_id = CardId(self.state.next_object_id);
        let id = create_object(
            &mut self.state,
            card_id,
            player,
            name.to_string(),
            Zone::Hand,
        );
        let obj = self.state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);

        CardBuilder {
            state: &mut self.state,
            id,
        }
    }

    /// Consume the builder, returning a `GameRunner` for step-by-step execution.
    pub fn build(self) -> GameRunner {
        GameRunner { state: self.state }
    }

    /// Convenience: build and immediately run a sequence of actions.
    pub fn build_and_run(self, actions: Vec<GameAction>) -> ScenarioResult {
        let mut runner = self.build();
        runner.run(actions)
    }
}

// ---------------------------------------------------------------------------
// CardBuilder (fluent keyword/ability chaining)
// ---------------------------------------------------------------------------

/// Fluent builder for modifying a newly-created game object.
/// Holds a mutable reference to the underlying `GameState` + the `ObjectId`.
pub struct CardBuilder<'a> {
    state: &'a mut GameState,
    id: ObjectId,
}

impl<'a> CardBuilder<'a> {
    /// Get the ObjectId of the card being built.
    pub fn id(&self) -> ObjectId {
        self.id
    }

    fn obj(&mut self) -> &mut GameObject {
        self.state.objects.get_mut(&self.id).unwrap()
    }

    /// Push a keyword to both `keywords` (computed) and `base_keywords` (survives layer evaluation).
    fn push_keyword(&mut self, kw: Keyword) {
        let obj = self.obj();
        obj.keywords.push(kw.clone());
        obj.base_keywords.push(kw);
    }

    // --- Keyword convenience methods ---

    pub fn flying(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Flying);
        self
    }

    pub fn first_strike(&mut self) -> &mut Self {
        self.push_keyword(Keyword::FirstStrike);
        self
    }

    pub fn double_strike(&mut self) -> &mut Self {
        self.push_keyword(Keyword::DoubleStrike);
        self
    }

    pub fn trample(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Trample);
        self
    }

    pub fn deathtouch(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Deathtouch);
        self
    }

    pub fn lifelink(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Lifelink);
        self
    }

    pub fn vigilance(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Vigilance);
        self
    }

    pub fn haste(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Haste);
        self
    }

    pub fn reach(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Reach);
        self
    }

    pub fn defender(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Defender);
        self
    }

    pub fn menace(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Menace);
        self
    }

    pub fn indestructible(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Indestructible);
        self
    }

    pub fn hexproof(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Hexproof);
        self
    }

    pub fn flash(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Flash);
        self
    }

    pub fn wither(&mut self) -> &mut Self {
        self.push_keyword(Keyword::Wither);
        self
    }

    // --- Generic keyword fallback ---

    pub fn with_keyword(&mut self, kw: Keyword) -> &mut Self {
        self.push_keyword(kw);
        self
    }

    // --- Ability attachment ---

    /// Attach an ability definition with the given effect.
    pub fn with_ability(&mut self, effect: Effect) -> &mut Self {
        self.obj().abilities.push(AbilityDefinition {
            kind: AbilityKind::Spell,
            effect,
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        });
        self
    }

    /// Attach a static ability definition.
    pub fn with_static(&mut self, mode: StaticMode) -> &mut Self {
        self.obj().static_definitions.push(StaticDefinition {
            mode,
            affected: None,
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: None,
        });
        self
    }

    /// Attach a continuous static with typed modifications.
    pub fn with_continuous_static(
        &mut self,
        modifications: Vec<crate::types::ability::ContinuousModification>,
    ) -> &mut Self {
        self.obj().static_definitions.push(StaticDefinition {
            mode: StaticMode::Continuous,
            affected: None,
            modifications,
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: None,
        });
        self
    }

    /// Attach a trigger definition.
    pub fn with_trigger(&mut self, mode: TriggerMode) -> &mut Self {
        self.obj().trigger_definitions.push(TriggerDefinition {
            mode,
            execute: None,
            valid_card: None,
            origin: None,
            destination: None,
            trigger_zones: vec![],
            phase: None,
            optional: false,
            combat_damage: false,
            secondary: false,
            valid_target: None,
            valid_source: None,
            description: None,
            constraint: None,
        });
        self
    }

    // --- Type mutations ---

    pub fn as_instant(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Instant);
        self
    }

    pub fn as_enchantment(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Enchantment);
        self
    }

    pub fn as_sorcery(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Sorcery);
        self
    }

    pub fn as_artifact(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Artifact);
        self
    }

    // --- Special modifiers ---

    /// Mark this creature as having summoning sickness (entered this turn).
    pub fn with_summoning_sickness(&mut self) -> &mut Self {
        let turn = self.state.turn_number;
        self.obj().entered_battlefield_turn = Some(turn);
        self
    }

    /// Set the mana cost of this card.
    pub fn with_mana_cost(&mut self, cost: crate::types::mana::ManaCost) -> &mut Self {
        self.obj().mana_cost = cost;
        self
    }

    /// Add +1/+1 counters to this creature.
    pub fn with_plus_counters(&mut self, count: u32) -> &mut Self {
        let counter = crate::game::game_object::CounterType::Plus1Plus1;
        *self.obj().counters.entry(counter).or_insert(0) += count;
        self
    }

    /// Add -1/-1 counters to this creature.
    pub fn with_minus_counters(&mut self, count: u32) -> &mut Self {
        let counter = crate::game::game_object::CounterType::Minus1Minus1;
        *self.obj().counters.entry(counter).or_insert(0) += count;
        self
    }
}

// ---------------------------------------------------------------------------
// GameRunner (step-by-step execution)
// ---------------------------------------------------------------------------

/// Wraps a `GameState` for step-by-step action execution.
pub struct GameRunner {
    state: GameState,
}

impl GameRunner {
    /// Execute a single action. Returns the `ActionResult` from the engine.
    pub fn act(&mut self, action: GameAction) -> Result<ActionResult, EngineError> {
        apply(&mut self.state, action)
    }

    /// Get a reference to the current game state.
    pub fn state(&self) -> &GameState {
        &self.state
    }

    /// Pass priority for both players (P0 then P1, or whichever order is appropriate).
    pub fn pass_both_players(&mut self) {
        // Pass twice -- once for each player
        let _ = apply(&mut self.state, GameAction::PassPriority);
        let _ = apply(&mut self.state, GameAction::PassPriority);
    }

    /// Pass priority until the top of the stack resolves.
    pub fn resolve_top(&mut self) {
        // Keep passing priority until the stack shrinks or we can't pass anymore
        let initial_stack_len = self.state.stack.len();
        for _ in 0..10 {
            if self.state.stack.len() < initial_stack_len {
                break;
            }
            if apply(&mut self.state, GameAction::PassPriority).is_err() {
                break;
            }
        }
    }

    /// Produce a `GameSnapshot` of the current state (no events).
    pub fn snapshot(&self) -> GameSnapshot {
        GameSnapshot::from_state(&self.state, &[])
    }

    /// Execute all actions sequentially, collecting all events.
    pub fn run(&mut self, actions: Vec<GameAction>) -> ScenarioResult {
        let mut all_events = Vec::new();
        for action in actions {
            match apply(&mut self.state, action) {
                Ok(result) => {
                    all_events.extend(result.events);
                }
                Err(_) => break,
            }
        }
        ScenarioResult {
            state: self.state.clone(),
            events: all_events,
        }
    }
}

// ---------------------------------------------------------------------------
// ScenarioResult (query methods)
// ---------------------------------------------------------------------------

/// Holds the final game state and all collected events from an action sequence.
pub struct ScenarioResult {
    state: GameState,
    events: Vec<GameEvent>,
}

impl ScenarioResult {
    /// Get the zone of a specific object.
    pub fn zone(&self, id: ObjectId) -> Zone {
        self.state.objects[&id].zone
    }

    /// Get a player's life total.
    pub fn life(&self, player: PlayerId) -> i32 {
        self.state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.life)
            .unwrap_or(0)
    }

    /// Count objects on the battlefield owned by a player.
    pub fn battlefield_count(&self, player: PlayerId) -> usize {
        self.state
            .battlefield
            .iter()
            .filter(|&&id| {
                self.state
                    .objects
                    .get(&id)
                    .map(|o| o.owner == player)
                    .unwrap_or(false)
            })
            .count()
    }

    /// Count objects in a player's graveyard.
    pub fn graveyard_count(&self, player: PlayerId) -> usize {
        self.state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.graveyard.len())
            .unwrap_or(0)
    }

    /// Count objects in a player's hand.
    pub fn hand_count(&self, player: PlayerId) -> usize {
        self.state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.hand.len())
            .unwrap_or(0)
    }

    /// Get a reference to a specific game object.
    pub fn object(&self, id: ObjectId) -> &GameObject {
        &self.state.objects[&id]
    }

    /// Get all collected events.
    pub fn events(&self) -> &[GameEvent] {
        &self.events
    }

    /// Produce a `GameSnapshot` for insta snapshot testing.
    pub fn snapshot(&self) -> GameSnapshot {
        GameSnapshot::from_state(&self.state, &self.events)
    }
}

// ---------------------------------------------------------------------------
// GameSnapshot (insta-compatible projection)
// ---------------------------------------------------------------------------

/// A focused, stable projection of game state for snapshot testing.
/// Uses card names and descriptions (not raw ObjectIds) to avoid brittleness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSnapshot {
    pub players: Vec<PlayerSnapshot>,
    pub battlefield: Vec<BattlefieldEntry>,
    pub stack: Vec<StackSnapshot>,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    pub life: i32,
    pub hand: Vec<String>,
    pub graveyard: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BattlefieldEntry {
    pub name: String,
    pub owner: u8,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub tapped: bool,
    pub damage: u32,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackSnapshot {
    pub description: String,
}

impl GameSnapshot {
    fn from_state(state: &GameState, events: &[GameEvent]) -> Self {
        // Build per-player snapshots
        let players: Vec<PlayerSnapshot> = state
            .players
            .iter()
            .map(|p| {
                let hand: Vec<String> = p
                    .hand
                    .iter()
                    .filter_map(|id| state.objects.get(id))
                    .map(|o| o.name.clone())
                    .collect();
                let graveyard: Vec<String> = p
                    .graveyard
                    .iter()
                    .filter_map(|id| state.objects.get(id))
                    .map(|o| o.name.clone())
                    .collect();
                PlayerSnapshot {
                    life: p.life,
                    hand,
                    graveyard,
                }
            })
            .collect();

        // Build battlefield entries sorted by owner then name for stability
        let mut battlefield: Vec<BattlefieldEntry> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .map(|o| BattlefieldEntry {
                name: o.name.clone(),
                owner: o.owner.0,
                power: o.power,
                toughness: o.toughness,
                tapped: o.tapped,
                damage: o.damage_marked,
                keywords: o.keywords.iter().map(|k| format!("{:?}", k)).collect(),
            })
            .collect();
        battlefield.sort_by(|a, b| a.owner.cmp(&b.owner).then(a.name.cmp(&b.name)));

        // Build stack entries
        let stack: Vec<StackSnapshot> = state
            .stack
            .iter()
            .map(|entry| {
                let source_name = state
                    .objects
                    .get(&entry.source_id)
                    .map(|o| o.name.clone())
                    .unwrap_or_else(|| format!("Unknown({})", entry.source_id.0));
                StackSnapshot {
                    description: format!("{} (by P{})", source_name, entry.controller.0),
                }
            })
            .collect();

        // Summarize events as strings
        let event_descriptions: Vec<String> = events.iter().map(|e| format!("{:?}", e)).collect();

        GameSnapshot {
            players,
            battlefield,
            stack,
            events: event_descriptions,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_new_creates_valid_game_state() {
        let scenario = GameScenario::new();
        let runner = scenario.build();
        let state = runner.state();
        assert_eq!(state.players.len(), 2);
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn add_creature_returns_object_id_on_battlefield() {
        let mut scenario = GameScenario::new();
        let bear_id = scenario.add_creature(P0, "Bear", 2, 2).id();
        let runner = scenario.build();
        let state = runner.state();

        let obj = &state.objects[&bear_id];
        assert_eq!(obj.name, "Bear");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.base_power, Some(2));
        assert_eq!(obj.base_toughness, Some(2));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(obj.zone, Zone::Battlefield);
        // Not summoning sick by default (entered previous turn)
        assert_eq!(
            obj.entered_battlefield_turn,
            Some(state.turn_number.saturating_sub(1))
        );
    }

    #[test]
    fn add_vanilla_returns_object_id() {
        let mut scenario = GameScenario::new();
        let id = scenario.add_vanilla(P0, 2, 2);
        let runner = scenario.build();
        let state = runner.state();

        let obj = &state.objects[&id];
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.zone, Zone::Battlefield);
    }

    #[test]
    fn add_basic_land_on_battlefield_with_land_type() {
        let mut scenario = GameScenario::new();
        let id = scenario.add_basic_land(P0, ManaColor::Green);
        let runner = scenario.build();
        let state = runner.state();

        let obj = &state.objects[&id];
        assert_eq!(obj.name, "Forest");
        assert!(obj.card_types.core_types.contains(&CoreType::Land));
        assert_eq!(obj.zone, Zone::Battlefield);
    }

    #[test]
    fn add_bolt_to_hand_creates_instant_with_deal_damage() {
        let mut scenario = GameScenario::new();
        let id = scenario.add_bolt_to_hand(P0);
        let runner = scenario.build();
        let state = runner.state();

        let obj = &state.objects[&id];
        assert_eq!(obj.name, "Lightning Bolt");
        assert!(obj.card_types.core_types.contains(&CoreType::Instant));
        assert_eq!(obj.zone, Zone::Hand);
        assert!(!obj.abilities.is_empty());
        assert_eq!(
            crate::types::ability::effect_variant_name(&obj.abilities[0].effect),
            "DealDamage"
        );
    }

    #[test]
    fn card_builder_keyword_chaining() {
        let mut scenario = GameScenario::new();
        let id = {
            let mut builder = scenario.add_creature(P0, "Angel", 4, 4);
            builder.flying().deathtouch().trample();
            builder.id()
        };
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(obj.keywords.contains(&Keyword::Flying));
        assert!(obj.keywords.contains(&Keyword::Deathtouch));
        assert!(obj.keywords.contains(&Keyword::Trample));
    }

    #[test]
    fn card_builder_ability_chaining() {
        let mut scenario = GameScenario::new();
        let id = {
            let mut builder = scenario.add_creature(P0, "Wizard", 1, 1);
            builder.with_ability(Effect::Draw { count: 1 });
            builder.with_static(StaticMode::Continuous);
            builder.id()
        };
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(!obj.abilities.is_empty());
        assert!(!obj.static_definitions.is_empty());
    }

    #[test]
    fn card_builder_as_instant_changes_type() {
        let mut scenario = GameScenario::new();
        let id = {
            let mut builder = scenario.add_creature(P0, "Spell", 0, 0);
            builder.as_instant();
            builder.id()
        };
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(obj.card_types.core_types.contains(&CoreType::Instant));
        assert!(!obj.card_types.core_types.contains(&CoreType::Creature));
    }

    #[test]
    fn with_keyword_generic_fallback() {
        let mut scenario = GameScenario::new();
        let id = {
            let mut builder = scenario.add_creature(P0, "Wither Beast", 3, 3);
            builder.with_keyword(Keyword::Wither);
            builder.id()
        };
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(obj.keywords.contains(&Keyword::Wither));
    }

    #[test]
    fn at_phase_sets_phase_waiting_for_and_priority() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let runner = scenario.build();
        let state = runner.state();

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority {
                player: state.active_player,
            }
        );
        assert_eq!(state.priority_player, state.active_player);
    }

    #[test]
    fn build_and_run_executes_actions_and_returns_result() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // Just pass priority as a minimal action
        let result = scenario.build_and_run(vec![GameAction::PassPriority]);

        // Should have at least one event
        assert!(!result.events().is_empty());
    }

    #[test]
    fn scenario_result_zone_returns_correct_zone() {
        let mut scenario = GameScenario::new();
        let bear_id = scenario.add_creature(P0, "Bear", 2, 2).id();
        let bolt_id = scenario.add_bolt_to_hand(P0);
        let result = scenario.build_and_run(vec![]);

        assert_eq!(result.zone(bear_id), Zone::Battlefield);
        assert_eq!(result.zone(bolt_id), Zone::Hand);
    }

    #[test]
    fn scenario_result_life_returns_life_total() {
        let mut scenario = GameScenario::new();
        scenario.with_life(P0, 15);
        let result = scenario.build_and_run(vec![]);

        assert_eq!(result.life(P0), 15);
        assert_eq!(result.life(P1), 20);
    }

    #[test]
    fn scenario_result_battlefield_count() {
        let mut scenario = GameScenario::new();
        scenario.add_creature(P0, "Bear", 2, 2);
        scenario.add_creature(P0, "Elf", 1, 1);
        scenario.add_creature(P1, "Goblin", 1, 1);
        let result = scenario.build_and_run(vec![]);

        assert_eq!(result.battlefield_count(P0), 2);
        assert_eq!(result.battlefield_count(P1), 1);
    }

    #[test]
    fn game_runner_act_returns_action_result() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();

        let result = runner.act(GameAction::PassPriority);
        assert!(result.is_ok());
        let action_result = result.unwrap();
        assert!(!action_result.events.is_empty());
    }

    #[test]
    fn game_runner_state_returns_current_state() {
        let mut scenario = GameScenario::new();
        scenario.add_creature(P0, "Bear", 2, 2);
        let runner = scenario.build();

        assert_eq!(runner.state().battlefield.len(), 1);
    }

    #[test]
    fn snapshot_serializes_to_json() {
        let mut scenario = GameScenario::new();
        scenario.add_creature(P0, "Bear", 2, 2);
        scenario.add_bolt_to_hand(P1);
        let result = scenario.build_and_run(vec![]);

        let snapshot = result.snapshot();

        // Verify snapshot structure
        assert_eq!(snapshot.players.len(), 2);
        assert_eq!(snapshot.players[0].life, 20);
        assert_eq!(snapshot.players[1].hand.len(), 1);
        assert_eq!(snapshot.players[1].hand[0], "Lightning Bolt");
        assert_eq!(snapshot.battlefield.len(), 1);
        assert_eq!(snapshot.battlefield[0].name, "Bear");
        assert_eq!(snapshot.battlefield[0].owner, 0);
        assert_eq!(snapshot.battlefield[0].power, Some(2));
        assert_eq!(snapshot.battlefield[0].toughness, Some(2));

        // Verify it serializes to JSON (insta requirement)
        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(json.get("players").is_some());
        assert!(json.get("battlefield").is_some());
        assert!(json.get("stack").is_some());
        assert!(json.get("events").is_some());
    }

    #[test]
    fn snapshot_works_with_insta() {
        let mut scenario = GameScenario::new();
        scenario.add_creature(P0, "Bear", 2, 2);
        let result = scenario.build_and_run(vec![]);
        let snapshot = result.snapshot();

        // This will create/verify a snapshot file
        insta::assert_json_snapshot!("scenario_basic_bear", snapshot);
    }

    #[test]
    fn card_builder_with_trigger() {
        let mut scenario = GameScenario::new();
        let id = {
            let mut builder = scenario.add_creature(P0, "Soul Warden", 1, 1);
            builder.with_trigger(TriggerMode::ChangesZone);
            builder.id()
        };
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(!obj.trigger_definitions.is_empty());
        assert_eq!(obj.trigger_definitions[0].mode, TriggerMode::ChangesZone);
    }

    #[test]
    fn card_builder_with_summoning_sickness() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let id = {
            let mut builder = scenario.add_creature(P0, "Fresh Bear", 2, 2);
            builder.with_summoning_sickness();
            builder.id()
        };
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        // Entered this turn (turn 2), so has summoning sickness
        assert_eq!(obj.entered_battlefield_turn, Some(2));
    }

    #[test]
    fn new_n_player_creates_correct_player_count() {
        let scenario = GameScenario::new_n_player(4, 99);
        let runner = scenario.build();
        let state = runner.state();
        assert_eq!(state.players.len(), 4);
        assert_eq!(state.seat_order.len(), 4);
        for i in 0..4 {
            assert_eq!(state.players[i].id, PlayerId(i as u8));
            assert_eq!(state.players[i].life, 20);
        }
    }
}
