//! Test harness for constructing game states with inline card definitions.
//!
//! Provides `GameScenario` (mutable builder), `CardBuilder` (fluent keyword/ability chaining),
//! `GameRunner` (step-by-step execution), and `GameSnapshot` (insta-compatible projections).
//! Zero filesystem dependencies -- all cards are constructed inline.

use serde::{Deserialize, Serialize};

use crate::database::synthesis::synthesize_all;
use crate::game::engine::{apply_as_current, EngineError};
use crate::game::game_object::GameObject;
use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::zones::create_object;
use crate::parser::oracle::parse_oracle_text;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, AdditionalCost, Effect, PtValue, QuantityExpr,
    ReplacementDefinition, StaticDefinition, TargetFilter, TriggerDefinition,
};
use crate::types::actions::GameAction;
use crate::types::card::CardFace;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaUnit};
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
// Oracle text → CardFace helper
// ---------------------------------------------------------------------------

/// Build a `CardFace` from a `GameObject`'s identity fields + parsed Oracle text.
///
/// Mirrors the real pipeline (`build_oracle_face` in `synthesis.rs`) but without
/// MTGJSON-specific processing (partner keyword upgrading, color override,
/// keyword deduplication, scryfall_oracle_id). Those require MTGJSON metadata
/// not available from inline Oracle text.
fn build_face_from_oracle(
    obj: &GameObject,
    keyword_names: &[String],
    oracle_text: &str,
) -> CardFace {
    let type_strings: Vec<String> = obj
        .card_types
        .core_types
        .iter()
        .map(|t| t.to_string())
        .collect();
    let subtype_strings: Vec<String> = obj.card_types.subtypes.clone();

    // Build keyword name hints if the caller didn't provide them.
    // The parser's `extract_keyword_line` requires keyword name hints to identify
    // keyword-only lines (returns None when hints are empty). Pre-scan each line
    // through Keyword::from_str to detect bare keywords like "Flying", "Haste".
    let inferred_kw_names: Vec<String>;
    let effective_kw_names = if keyword_names.is_empty() {
        inferred_kw_names = oracle_text
            .lines()
            .flat_map(|line| {
                line.split(',')
                    .map(|part| part.trim().to_lowercase())
                    .filter(|lower| {
                        let kw: Keyword = lower.parse().unwrap_or(Keyword::Unknown(String::new()));
                        !matches!(kw, Keyword::Unknown(_))
                    })
            })
            .collect();
        &inferred_kw_names
    } else {
        keyword_names
    };

    let parsed = parse_oracle_text(
        oracle_text,
        &obj.name,
        effective_kw_names,
        &type_strings,
        &subtype_strings,
    );

    // Merge keywords: parse keyword names into Keyword values (mirroring how
    // build_oracle_face merges MTGJSON keywords with extracted_keywords).
    // extract_keyword_line skips exact matches (since MTGJSON already has them),
    // so we need to parse them here and merge, just like the real pipeline.
    let mut keywords: Vec<Keyword> = effective_kw_names
        .iter()
        .filter_map(|s| {
            let kw: Keyword = s.parse().unwrap();
            if matches!(kw, Keyword::Unknown(_)) {
                None
            } else {
                Some(kw)
            }
        })
        .collect();
    // Merge parser-extracted keywords, skipping any already present from hints.
    for kw in parsed.extracted_keywords {
        if !keywords.contains(&kw) {
            keywords.push(kw);
        }
    }

    let mut face = CardFace {
        name: obj.name.clone(),
        power: obj.power.map(PtValue::Fixed),
        toughness: obj.toughness.map(PtValue::Fixed),
        card_type: obj.card_types.clone(),
        mana_cost: obj.mana_cost.clone(),
        oracle_text: Some(oracle_text.to_string()),
        keywords,
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
        strive_cost: parsed.strive_cost,
        ..Default::default()
    };
    synthesize_all(&mut face);
    face
}

// ---------------------------------------------------------------------------
// GameScenario (mutable builder)
// ---------------------------------------------------------------------------

/// Mutable builder that constructs a GameState with predefined board state,
/// phase, turn, and card objects -- all with zero filesystem dependencies.
pub struct GameScenario {
    pub(crate) state: GameState,
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

    /// Add generic named cards to a player's hand without rules text.
    ///
    /// Intended for count/visibility/setup tests where full card semantics are not needed.
    pub fn with_cards_in_hand(&mut self, player: PlayerId, names: &[&str]) -> &mut Self {
        for &name in names {
            let card_id = CardId(self.state.next_object_id);
            create_object(
                &mut self.state,
                card_id,
                player,
                name.to_string(),
                Zone::Hand,
            );
        }
        self
    }

    /// Add generic named cards to the top of a player's library.
    ///
    /// The last supplied name becomes the current top card, matching the engine's
    /// library-top convention (`Vec::last()` / pop-from-end flows).
    pub fn with_library_top(&mut self, player: PlayerId, names_top_first: &[&str]) -> &mut Self {
        for &name in names_top_first.iter().rev() {
            let card_id = CardId(self.state.next_object_id);
            create_object(
                &mut self.state,
                card_id,
                player,
                name.to_string(),
                Zone::Library,
            );
        }
        self
    }

    /// Add generic named cards to a player's graveyard without rules text.
    pub fn with_graveyard(&mut self, player: PlayerId, names: &[&str]) -> &mut Self {
        for &name in names {
            let card_id = CardId(self.state.next_object_id);
            create_object(
                &mut self.state,
                card_id,
                player,
                name.to_string(),
                Zone::Graveyard,
            );
        }
        self
    }

    /// Replace a player's mana pool for deterministic payment tests.
    pub fn with_mana_pool(&mut self, player: PlayerId, mana: Vec<ManaUnit>) -> &mut Self {
        if let Some(p) = self.state.players.iter_mut().find(|p| p.id == player) {
            p.mana_pool.mana = mana;
        }
        self
    }

    /// Add counters to an existing object.
    pub fn with_counter(
        &mut self,
        object_id: ObjectId,
        counter: CounterType,
        count: u32,
    ) -> &mut Self {
        if count > 0 {
            *self
                .state
                .objects
                .get_mut(&object_id)
                .expect("object must exist")
                .counters
                .entry(counter)
                .or_insert(0) += count;
        }
        self
    }

    /// Mark an existing object as a commander and move it to the command zone.
    pub fn with_commander(&mut self, object_id: ObjectId) -> &mut Self {
        let (owner, current_zone) = self
            .state
            .objects
            .get(&object_id)
            .map(|obj| (obj.owner, obj.zone))
            .expect("object must exist");
        crate::game::zones::remove_from_zone(&mut self.state, object_id, current_zone, owner);
        crate::game::zones::add_to_zone(&mut self.state, object_id, Zone::Command, owner);
        let obj = self
            .state
            .objects
            .get_mut(&object_id)
            .expect("object must exist");
        obj.zone = Zone::Command;
        obj.is_commander = true;
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
        obj.base_card_types = obj.card_types.clone();
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
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(self.state.turn_number.saturating_sub(1));
        // Add mana ability
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: crate::types::ability::ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: crate::types::ability::ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(crate::types::ability::AbilityCost::Tap);
        obj.abilities.push(ability.clone());
        obj.base_abilities.push(ability);
        id
    }

    /// Add a land to a player's hand. Returns a `CardBuilder` for fluent chaining.
    pub fn add_land_to_hand(&mut self, player: PlayerId, name: &str) -> CardBuilder<'_> {
        let card_id = CardId(self.state.next_object_id);
        let id = create_object(
            &mut self.state,
            card_id,
            player,
            name.to_string(),
            Zone::Hand,
        );
        let obj = self.state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();

        CardBuilder {
            state: &mut self.state,
            id,
        }
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
        obj.base_card_types = obj.card_types.clone();
        let ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        obj.abilities.push(ability.clone());
        obj.base_abilities.push(ability);
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
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);

        CardBuilder {
            state: &mut self.state,
            id,
        }
    }

    // --- Oracle text convenience constructors ---

    /// Add a creature to the battlefield with abilities parsed from Oracle text.
    pub fn add_creature_from_oracle(
        &mut self,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
        oracle_text: &str,
    ) -> CardBuilder<'_> {
        let mut builder = self.add_creature(player, name, power, toughness);
        builder.from_oracle_text(oracle_text);
        builder
    }

    /// Add a creature to hand with abilities parsed from Oracle text.
    pub fn add_creature_to_hand_from_oracle(
        &mut self,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
        oracle_text: &str,
    ) -> CardBuilder<'_> {
        let mut builder = self.add_creature_to_hand(player, name, power, toughness);
        builder.from_oracle_text(oracle_text);
        builder
    }

    /// Add a spell (instant/sorcery) to hand with abilities parsed from Oracle text.
    ///
    /// Use `is_instant: true` for instants, `false` for sorceries.
    pub fn add_spell_to_hand_from_oracle(
        &mut self,
        player: PlayerId,
        name: &str,
        is_instant: bool,
        oracle_text: &str,
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
        let core_type = if is_instant {
            CoreType::Instant
        } else {
            CoreType::Sorcery
        };
        obj.card_types.core_types.push(core_type);
        obj.base_card_types = obj.card_types.clone();
        // Instants/sorceries have no power/toughness (unlike creatures)

        let mut builder = CardBuilder {
            state: &mut self.state,
            id,
        };
        builder.from_oracle_text(oracle_text);
        builder
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

    fn sync_base_card_types(&mut self) {
        let obj = self.obj();
        obj.base_card_types = obj.card_types.clone();
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
        let ability = AbilityDefinition::new(AbilityKind::Spell, effect);
        let obj = self.obj();
        obj.abilities.push(ability.clone());
        obj.base_abilities.push(ability);
        self
    }

    pub fn with_ability_definition(&mut self, ability: AbilityDefinition) -> &mut Self {
        let obj = self.obj();
        obj.abilities.push(ability.clone());
        obj.base_abilities.push(ability);
        self
    }

    /// Attach a static ability definition.
    pub fn with_static(&mut self, mode: StaticMode) -> &mut Self {
        let static_def = StaticDefinition::new(mode);
        let obj = self.obj();
        obj.static_definitions.push(static_def.clone());
        obj.base_static_definitions.push(static_def);
        self
    }

    pub fn with_static_definition(&mut self, static_def: StaticDefinition) -> &mut Self {
        let obj = self.obj();
        obj.static_definitions.push(static_def.clone());
        obj.base_static_definitions.push(static_def);
        self
    }

    /// Attach a continuous static with typed modifications.
    pub fn with_continuous_static(
        &mut self,
        modifications: Vec<crate::types::ability::ContinuousModification>,
    ) -> &mut Self {
        let static_def = StaticDefinition::continuous().modifications(modifications);
        let obj = self.obj();
        obj.static_definitions.push(static_def.clone());
        obj.base_static_definitions.push(static_def);
        self
    }

    /// Attach a trigger definition (mode only, no execute).
    pub fn with_trigger(&mut self, mode: TriggerMode) -> &mut Self {
        let trigger = TriggerDefinition::new(mode);
        let obj = self.obj();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions.push(trigger);
        self
    }

    /// Attach a fully constructed trigger definition (with execute, zones, etc.).
    pub fn with_trigger_definition(&mut self, trigger: TriggerDefinition) -> &mut Self {
        let obj = self.obj();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions.push(trigger);
        self
    }

    pub fn with_replacement(
        &mut self,
        event: crate::types::replacements::ReplacementEvent,
    ) -> &mut Self {
        let replacement = ReplacementDefinition::new(event);
        let obj = self.obj();
        obj.replacement_definitions.push(replacement.clone());
        obj.base_replacement_definitions.push(replacement);
        self
    }

    pub fn with_replacement_definition(&mut self, def: ReplacementDefinition) -> &mut Self {
        let obj = self.obj();
        obj.replacement_definitions.push(def.clone());
        obj.base_replacement_definitions.push(def);
        self
    }

    // --- Type mutations ---

    pub fn as_instant(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Instant);
        self.sync_base_card_types();
        self
    }

    pub fn as_enchantment(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Enchantment);
        self.sync_base_card_types();
        self
    }

    pub fn as_sorcery(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Sorcery);
        self.sync_base_card_types();
        self
    }

    pub fn as_artifact(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Artifact);
        self.sync_base_card_types();
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
        let counter = crate::types::counter::CounterType::Plus1Plus1;
        *self.obj().counters.entry(counter).or_insert(0) += count;
        self
    }

    /// Add -1/-1 counters to this creature.
    pub fn with_minus_counters(&mut self, count: u32) -> &mut Self {
        let counter = crate::types::counter::CounterType::Minus1Minus1;
        *self.obj().counters.entry(counter).or_insert(0) += count;
        self
    }

    /// Set an additional cost on this card (kicker, blight, "or pay").
    pub fn with_additional_cost(&mut self, cost: AdditionalCost) -> &mut Self {
        self.obj().additional_cost = Some(cost);
        self
    }

    /// Pre-mark damage on this permanent (for SBA / deathtouch tests).
    pub fn with_damage_marked(&mut self, damage: u32) -> &mut Self {
        self.obj().damage_marked = damage;
        self
    }

    /// Mark that this permanent has been dealt damage from a deathtouch source.
    pub fn with_deathtouch_damage(&mut self) -> &mut Self {
        self.obj().dealt_deathtouch_damage = true;
        self
    }

    /// Set creature subtypes (e.g., `["Goblin", "Warrior"]`).
    pub fn with_subtypes(&mut self, subtypes: Vec<&str>) -> &mut Self {
        let obj = self.obj();
        obj.card_types.subtypes = subtypes.into_iter().map(String::from).collect();
        self.sync_base_card_types();
        self
    }

    // --- Oracle text parsing ---

    /// Replace all abilities, triggers, statics, replacements, and keywords on this
    /// object with those parsed from Oracle text. Runs the full synthesis pipeline
    /// (`parse_oracle_text` → `synthesize_all` → `apply_card_face_to_object`).
    ///
    /// **Warning:** This overwrites any keywords, abilities, triggers, statics, or
    /// replacements previously set via builder methods (e.g., `.flying()`,
    /// `.with_ability(...)`). Call `from_oracle_text` before any manual additions,
    /// or use it as the sole ability source.
    ///
    /// Identity fields (name, power, toughness, card_types, mana_cost) are preserved
    /// from the builder — they are round-tripped through a `CardFace` so
    /// `apply_card_face_to_object` writes back the same values. Counters, zone,
    /// entered_battlefield_turn, and other non-ability state are also preserved.
    ///
    /// Note: Unlike `build_oracle_face` in the card data pipeline, this does not
    /// perform MTGJSON-specific processing (partner keyword upgrading, color override,
    /// keyword deduplication). Those require MTGJSON metadata not available from
    /// inline Oracle text.
    pub fn from_oracle_text(&mut self, oracle_text: &str) -> &mut Self {
        self.from_oracle_text_with_keywords(&[], oracle_text)
    }

    /// Like `from_oracle_text`, but accepts explicit MTGJSON-style keyword names
    /// for precise keyword-only line detection. Use when Oracle text contains
    /// multi-keyword lines like "Flying, vigilance" that require keyword name
    /// hints to parse correctly.
    pub fn from_oracle_text_with_keywords(
        &mut self,
        keyword_names: &[&str],
        oracle_text: &str,
    ) -> &mut Self {
        let kw_strings: Vec<String> = keyword_names.iter().map(|s| s.to_string()).collect();
        let obj = self.state.objects.get(&self.id).unwrap();
        let face = build_face_from_oracle(obj, &kw_strings, oracle_text);
        let obj = self.state.objects.get_mut(&self.id).unwrap();
        apply_card_face_to_object(obj, &face);
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
        apply_as_current(&mut self.state, action)
    }

    /// Get a reference to the current game state.
    pub fn state(&self) -> &GameState {
        &self.state
    }

    /// Get a mutable reference to the current game state.
    ///
    /// Use this escape hatch to configure game state that the builder doesn't
    /// expose (e.g., `waiting_for`, `combat`, `active_player`).
    pub fn state_mut(&mut self) -> &mut GameState {
        &mut self.state
    }

    /// Pass priority until a priority window is reached, or stop if progress stalls.
    pub fn advance_to_priority_window(&mut self) {
        for _ in 0..20 {
            if matches!(self.state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            if apply_as_current(&mut self.state, GameAction::PassPriority).is_err() {
                break;
            }
        }
    }

    /// Pass priority for both players (P0 then P1, or whichever order is appropriate).
    pub fn pass_both_players(&mut self) {
        // Pass twice -- once for each player
        let _ = apply_as_current(&mut self.state, GameAction::PassPriority);
        let _ = apply_as_current(&mut self.state, GameAction::PassPriority);
    }

    /// Pass priority until the top of the stack resolves.
    pub fn resolve_top(&mut self) {
        // Keep passing priority until the stack shrinks or we can't pass anymore
        let initial_stack_len = self.state.stack.len();
        for _ in 0..10 {
            if self.state.stack.len() < initial_stack_len {
                break;
            }
            if apply_as_current(&mut self.state, GameAction::PassPriority).is_err() {
                break;
            }
        }
    }

    /// Pass priority until the stack is empty, or stop if the engine no longer advances.
    pub fn advance_until_stack_empty(&mut self) {
        for _ in 0..40 {
            if self.state.stack.is_empty() {
                break;
            }
            if apply_as_current(&mut self.state, GameAction::PassPriority).is_err() {
                break;
            }
        }
    }

    /// Choose the first legal target for the current targeting-style waiting state.
    pub fn choose_first_legal_target(&mut self) -> Result<ActionResult, EngineError> {
        match &self.state.waiting_for {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let target = slot.legal_targets.first().cloned();
                if target.is_none() && !slot.optional {
                    return Err(EngineError::InvalidAction(
                        "no legal target available for required slot".to_string(),
                    ));
                }
                apply_as_current(&mut self.state, GameAction::ChooseTarget { target })
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let target = slot.legal_targets.first().cloned();
                if target.is_none() && !slot.optional {
                    return Err(EngineError::InvalidAction(
                        "no legal target available for required trigger slot".to_string(),
                    ));
                }
                apply_as_current(&mut self.state, GameAction::ChooseTarget { target })
            }
            _ => Err(EngineError::InvalidAction(
                "choose_first_legal_target requires a targeting waiting state".to_string(),
            )),
        }
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

    /// Stable battlefield names for lightweight assertions.
    pub fn battlefield_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .state
            .battlefield
            .iter()
            .filter_map(|id| self.state.objects.get(id))
            .map(|obj| obj.name.clone())
            .collect();
        names.sort();
        names
    }

    /// Stable stack source names for lightweight assertions.
    pub fn stack_names(&self) -> Vec<String> {
        self.state
            .stack
            .iter()
            .filter_map(|entry| self.state.objects.get(&entry.source_id))
            .map(|obj| obj.name.clone())
            .collect()
    }

    /// Returns the current waiting-state variant name for lightweight assertions.
    pub fn waiting_for_kind(&self) -> &'static str {
        match &self.state.waiting_for {
            WaitingFor::Priority { .. } => "Priority",
            WaitingFor::MulliganDecision { .. } => "MulliganDecision",
            WaitingFor::MulliganBottomCards { .. } => "MulliganBottomCards",
            WaitingFor::ManaPayment { .. } => "ManaPayment",
            WaitingFor::TargetSelection { .. } => "TargetSelection",
            WaitingFor::DeclareAttackers { .. } => "DeclareAttackers",
            WaitingFor::DeclareBlockers { .. } => "DeclareBlockers",
            WaitingFor::GameOver { .. } => "GameOver",
            WaitingFor::ReplacementChoice { .. } => "ReplacementChoice",
            WaitingFor::CopyTargetChoice { .. } => "CopyTargetChoice",
            WaitingFor::ExploreChoice { .. } => "ExploreChoice",
            WaitingFor::EquipTarget { .. } => "EquipTarget",
            WaitingFor::ScryChoice { .. } => "ScryChoice",
            WaitingFor::DigChoice { .. } => "DigChoice",
            WaitingFor::SurveilChoice { .. } => "SurveilChoice",
            WaitingFor::RevealChoice { .. } => "RevealChoice",
            WaitingFor::SearchChoice { .. } => "SearchChoice",
            WaitingFor::ChooseFromZoneChoice { .. } => "ChooseFromZoneChoice",
            WaitingFor::ConniveDiscard { .. } => "ConniveDiscard",
            WaitingFor::DiscardChoice { .. } => "DiscardChoice",
            WaitingFor::EffectZoneChoice { .. } => "EffectZoneChoice",
            WaitingFor::ManifestDreadChoice { .. } => "ManifestDreadChoice",
            WaitingFor::TriggerTargetSelection { .. } => "TriggerTargetSelection",
            WaitingFor::BetweenGamesSideboard { .. } => "BetweenGamesSideboard",
            WaitingFor::BetweenGamesChoosePlayDraw { .. } => "BetweenGamesChoosePlayDraw",
            WaitingFor::NamedChoice { .. } => "NamedChoice",
            WaitingFor::ModeChoice { .. } => "ModeChoice",
            WaitingFor::DiscardToHandSize { .. } => "DiscardToHandSize",
            WaitingFor::OptionalCostChoice { .. } => "OptionalCostChoice",
            WaitingFor::DefilerPayment { .. } => "DefilerPayment",
            WaitingFor::AdventureCastChoice { .. } => "AdventureCastChoice",
            WaitingFor::ModalFaceChoice { .. } => "ModalFaceChoice",
            WaitingFor::WarpCostChoice { .. } => "WarpCostChoice",
            WaitingFor::EvokeCostChoice { .. } => "EvokeCostChoice",
            WaitingFor::MultiTargetSelection { .. } => "MultiTargetSelection",
            WaitingFor::AbilityModeChoice { .. } => "AbilityModeChoice",
            WaitingFor::OptionalEffectChoice { .. } => "OptionalEffectChoice",
            WaitingFor::OpponentMayChoice { .. } => "OpponentMayChoice",
            WaitingFor::TributeChoice { .. } => "TributeChoice",
            WaitingFor::UnlessPayment { .. } => "UnlessPayment",
            WaitingFor::CompanionReveal { .. } => "CompanionReveal",
            WaitingFor::ChooseRingBearer { .. } => "ChooseRingBearer",
            WaitingFor::DiscardForCost { .. } => "DiscardForCost",
            WaitingFor::SacrificeForCost { .. } => "SacrificeForCost",
            WaitingFor::ReturnToHandForCost { .. } => "ReturnToHandForCost",
            WaitingFor::TapCreaturesForSpellCost { .. } => "TapCreaturesForSpellCost",
            WaitingFor::TapCreaturesForManaAbility { .. } => "TapCreaturesForManaAbility",
            WaitingFor::DiscardForManaAbility { .. } => "DiscardForManaAbility",
            WaitingFor::ChooseManaColor { .. } => "ChooseManaColor",
            WaitingFor::PayManaAbilityMana { .. } => "PayManaAbilityMana",
            WaitingFor::ExileFromGraveyardForCost { .. } => "ExileFromGraveyardForCost",
            WaitingFor::CollectEvidenceChoice { .. } => "CollectEvidenceChoice",
            WaitingFor::HarmonizeTapChoice { .. } => "HarmonizeTapChoice",
            WaitingFor::DiscoverChoice { .. } => "DiscoverChoice",
            WaitingFor::CascadeChoice { .. } => "CascadeChoice",
            WaitingFor::TopOrBottomChoice { .. } => "TopOrBottomChoice",
            WaitingFor::ChooseLegend { .. } => "ChooseLegend",
            WaitingFor::BattleProtectorChoice { .. } => "BattleProtectorChoice",
            WaitingFor::ProliferateChoice { .. } => "ProliferateChoice",
            WaitingFor::CopyRetarget { .. } => "CopyRetarget",
            WaitingFor::AssignCombatDamage { .. } => "AssignCombatDamage",
            WaitingFor::DistributeAmong { .. } => "DistributeAmong",
            WaitingFor::RetargetChoice { .. } => "RetargetChoice",
            WaitingFor::WardDiscardChoice { .. } => "WardDiscardChoice",
            WaitingFor::WardSacrificeChoice { .. } => "WardSacrificeChoice",
            WaitingFor::LearnChoice { .. } => "LearnChoice",
            WaitingFor::CrewVehicle { .. } => "CrewVehicle",
            WaitingFor::StationTarget { .. } => "StationTarget",
            WaitingFor::SaddleMount { .. } => "SaddleMount",
            WaitingFor::ChooseDungeon { .. } => "ChooseDungeon",
            WaitingFor::ChooseDungeonRoom { .. } => "ChooseDungeonRoom",
            WaitingFor::PopulateChoice { .. } => "PopulateChoice",
            WaitingFor::ClashCardPlacement { .. } => "ClashCardPlacement",
            WaitingFor::CategoryChoice { .. } => "CategoryChoice",
            WaitingFor::ChooseXValue { .. } => "ChooseXValue",
            WaitingFor::CombatTaxPayment { .. } => "CombatTaxPayment",
            WaitingFor::PhyrexianPayment { .. } => "PhyrexianPayment",
            WaitingFor::BlightChoice { .. } => "BlightChoice",
            WaitingFor::ParadigmCastOffer { .. } => "ParadigmCastOffer",
            WaitingFor::MiracleReveal { .. } => "MiracleReveal",
            WaitingFor::MiracleCastOffer { .. } => "MiracleCastOffer",
            WaitingFor::MadnessCastOffer { .. } => "MadnessCastOffer",
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
            match apply_as_current(&mut self.state, action) {
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
            builder.with_ability(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            });
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

    // --- from_oracle_text tests ---

    #[test]
    fn from_oracle_text_keywords() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Bird", 1, 1)
            .from_oracle_text("Haste\nFlying")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(obj.keywords.contains(&Keyword::Haste));
        assert!(obj.keywords.contains(&Keyword::Flying));
        assert!(obj.base_keywords.contains(&Keyword::Haste));
        assert!(obj.base_keywords.contains(&Keyword::Flying));
    }

    #[test]
    fn from_oracle_text_trigger() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Goblin Guide", 2, 2)
            .from_oracle_text("Whenever Goblin Guide attacks, defending player reveals the top card of their library. If it's a land card, that player puts it into their hand.")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(
            !obj.trigger_definitions.is_empty(),
            "should have at least one trigger definition"
        );
        assert!(
            !obj.base_trigger_definitions.is_empty(),
            "base triggers should also be populated"
        );
    }

    #[test]
    fn from_oracle_text_static() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Glorious Anthem", 0, 0)
            .as_enchantment()
            .from_oracle_text("Creatures you control get +1/+1.")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(
            !obj.static_definitions.is_empty(),
            "should have at least one static definition"
        );
        assert!(
            !obj.base_static_definitions.is_empty(),
            "base statics should also be populated"
        );
    }

    #[test]
    fn from_oracle_text_preserves_identity() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Bear", 2, 2)
            .from_oracle_text("Flying")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert_eq!(obj.name, "Bear");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.base_power, Some(2));
        assert_eq!(obj.base_toughness, Some(2));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    }

    #[test]
    fn from_oracle_text_spell_effect() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature_to_hand(P0, "Lightning Bolt", 0, 0)
            .as_instant()
            .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(!obj.abilities.is_empty(), "should have a spell ability");
        assert_eq!(
            crate::types::ability::effect_variant_name(&obj.abilities[0].effect),
            "DealDamage"
        );
    }

    #[test]
    fn from_oracle_text_color_derived() {
        use crate::types::mana::{ManaCost, ManaCostShard};

        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Goblin", 1, 1)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .from_oracle_text("Haste")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(
            obj.color.contains(&ManaColor::Red),
            "color should be derived from mana cost"
        );
    }

    #[test]
    fn from_oracle_text_with_keywords_multi_keyword_line() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Serra Angel", 4, 4)
            .from_oracle_text_with_keywords(&["flying", "vigilance"], "Flying, vigilance")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(obj.keywords.contains(&Keyword::Flying));
        assert!(obj.keywords.contains(&Keyword::Vigilance));
    }

    #[test]
    fn from_oracle_text_convenience_creature_on_battlefield() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.name, "Llanowar Elves");
        assert!(
            !obj.abilities.is_empty(),
            "should have a mana ability from Oracle text"
        );
    }

    #[test]
    fn from_oracle_text_convenience_spell_to_hand() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Lightning Bolt",
                true,
                "Lightning Bolt deals 3 damage to any target.",
            )
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert_eq!(obj.zone, Zone::Hand);
        assert!(obj.card_types.core_types.contains(&CoreType::Instant));
        assert!(!obj.abilities.is_empty());
        // Instants/sorceries must not have power/toughness
        assert_eq!(obj.power, None, "instants should not have power");
        assert_eq!(obj.toughness, None, "instants should not have toughness");
    }

    #[test]
    fn from_oracle_text_counters_survive() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Bear", 2, 2)
            .with_plus_counters(3)
            .from_oracle_text("Flying")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert!(obj.keywords.contains(&Keyword::Flying));
        assert_eq!(
            obj.counters
                .get(&crate::types::counter::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            3,
            "+1/+1 counters should survive from_oracle_text"
        );
    }

    #[test]
    fn from_oracle_text_empty_string() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Vanilla Bear", 2, 2)
            .from_oracle_text("")
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert_eq!(obj.name, "Vanilla Bear");
        assert_eq!(obj.power, Some(2));
        assert!(obj.abilities.is_empty());
        assert!(obj.keywords.is_empty());
    }
}
