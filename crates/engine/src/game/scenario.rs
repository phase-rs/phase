//! Test harness for constructing game states with inline card definitions.
//!
//! Provides `GameScenario` (mutable builder), `CardBuilder` (fluent keyword/ability chaining),
//! `GameRunner` (step-by-step execution), and `GameSnapshot` (insta-compatible projections).
//! Zero filesystem dependencies -- all cards are constructed inline.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::database::synthesis::{
    merge_extracted_keywords, parse_oracle_with_cleave_brackets, synthesize_all,
};
use crate::game::engine::{apply_as_current, EngineError};
use crate::game::game_object::GameObject;
use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, AdditionalCost, Effect, PtValue, QuantityExpr,
    ReplacementDefinition, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef,
    TriggerDefinition,
};
use crate::types::actions::{AlternativeCastDecision, GameAction};
use crate::types::card::CardFace;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, CastOfferKind, CastPaymentMode, CastingVariant, CastingVariantChoiceOption,
    ConvokeMode, GameState, ManaChoice, ManaChoicePrompt, PendingCast, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaType, ManaUnit};
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
    // The parser's `extract_granted_keyword_list` requires keyword name hints to identify
    // keyword-only lines (returns None when hints are empty). Pre-scan each line
    // through Keyword::from_str to detect bare keywords like "Flying", "Haste".
    //
    // A line contributes inferred hints ONLY when it is a genuine keyword line —
    // every comma-separated part parses to a known keyword (e.g. "Flying,
    // vigilance, trample"). A line with any prose fragment is an effect/ability
    // line and contributes nothing, even if it lists keywords inside a sentence
    // (e.g. Super-Adaptoid / Kathril's "Do the same for flying, first strike,
    // …"): those keyword names are the OBJECT of an effect, not granted
    // keywords, and must not be inferred as static abilities on the card.
    let inferred_kw_names: Vec<String>;
    let effective_kw_names = if keyword_names.is_empty() {
        inferred_kw_names = oracle_text
            .lines()
            .flat_map(|line| {
                let parts: Vec<String> = line
                    .split(',')
                    .map(|part| part.trim().to_lowercase())
                    .collect();
                let is_keyword_line = !parts.is_empty()
                    && parts.iter().all(|lower| {
                        let kw: Keyword = lower.parse().unwrap_or(Keyword::Unknown(String::new()));
                        !matches!(kw, Keyword::Unknown(_))
                    });
                if is_keyword_line {
                    parts
                } else {
                    Vec::new()
                }
            })
            .collect();
        &inferred_kw_names
    } else {
        keyword_names
    };

    // CR 702.148a-b + CR 612: Route the cleave bracket prep through the SAME
    // authority the real card-data build pipeline uses
    // (`parse_oracle_with_cleave_brackets`) so test fixtures exercise the real
    // cleave flow and the two pipelines cannot silently diverge. The helper
    // gates the bracket strip on the keyword hints containing "cleave" (the
    // inline-Oracle analog of MTGJSON reporting the keyword) so loyalty/other
    // bracket usage is never stripped.
    let (parsed, cleave_variant) = parse_oracle_with_cleave_brackets(
        oracle_text,
        &obj.name,
        effective_kw_names,
        &type_strings,
        &subtype_strings,
    );

    // Parse the keyword-hint names into base `Keyword` values (the scenario
    // analog of MTGJSON's keywords array), then delegate the merge of the
    // parser-extracted keywords to the shared `merge_extracted_keywords`
    // authority. CR 113.2c: routing through the same helper as the production
    // pipeline guarantees the scenario path cannot diverge from production —
    // multi-instance keywords (Cascade/Storm/Myriad/Exalted) keep their printed
    // multiplicity instead of being presence-deduped.
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
    merge_extracted_keywords(&mut keywords, parsed.extracted_keywords);

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
        cleave_variant,
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

    /// Create a scenario with an explicit `FormatConfig` (the format axis), a
    /// player count, and a seed. This is the general constructor; `new_n_player`
    /// is its standard-format specialization. Enables team formats — e.g.
    /// `FormatConfig::two_headed_giant()` — in scenario-driven tests so team
    /// combat (CR 805.10) can be exercised through the production apply pipeline.
    pub fn new_with_format(
        format_config: crate::types::format::FormatConfig,
        player_count: u8,
        seed: u64,
    ) -> Self {
        GameScenario {
            state: GameState::new(format_config, player_count, seed),
        }
    }

    /// Create a scenario with N players using the default format config (20 life each).
    pub fn new_n_player(count: u8, seed: u64) -> Self {
        Self::new_with_format(crate::types::format::FormatConfig::standard(), count, seed)
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
            self.add_card_to_hand(player, name);
        }
        self
    }

    /// Add one generic named card to a player's hand without rules text.
    pub fn add_card_to_hand(&mut self, player: PlayerId, name: &str) -> ObjectId {
        let card_id = CardId(self.state.next_object_id);
        create_object(
            &mut self.state,
            card_id,
            player,
            name.to_string(),
            Zone::Hand,
        )
    }

    /// Add generic named cards to the top of a player's library.
    ///
    /// The first supplied name becomes the current top card, matching the
    /// engine's library-top convention (`library[0]`).
    pub fn with_library_top(&mut self, player: PlayerId, names_top_first: &[&str]) -> &mut Self {
        for &name in names_top_first.iter().rev() {
            self.add_card_to_library_top(player, name);
        }
        self
    }

    /// Add one generic named card to the top of a player's library.
    pub fn add_card_to_library_top(&mut self, player: PlayerId, name: &str) -> ObjectId {
        let card_id = CardId(self.state.next_object_id);
        let id = create_object(
            &mut self.state,
            card_id,
            player,
            name.to_string(),
            Zone::Library,
        );
        // Engine convention: `library[0]` is the top. `create_object` appends
        // to the bottom, so re-seat this card at index 0 for deterministic top
        // tests.
        let player_state = self
            .state
            .players
            .iter_mut()
            .find(|p| p.id == player)
            .expect("player exists");
        player_state.library.retain(|&oid| oid != id);
        player_state.library.insert(0, id);
        id
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
    ///
    /// CR 118.3a: routes each unit through `add_mana_to_pool` so every seeded
    /// unit receives a distinct `ManaPipId` (a direct `mana_pool.mana = mana`
    /// would leave all units at the `ManaPipId(0)` sentinel, so pins would
    /// collide). All callers seed a fresh pool exactly once per player, so
    /// appending is equivalent to replacing.
    pub fn with_mana_pool(&mut self, player: PlayerId, mana: Vec<ManaUnit>) -> &mut Self {
        for unit in mana {
            let _ = self.state.add_mana_to_pool(player, unit);
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
        // CR 302.6: Scenario builder places pre-existing creatures (entered
        // on a prior turn), so they are not summoning-sick. `create_object`
        // sets the flag true for battlefield ETB; override here to match
        // the "already on battlefield" semantics the builder expresses.
        obj.summoning_sick = false;
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
        // CR 205.4: Basic lands have a single land subtype matching their name
        // (e.g. Forest). Filters like Quirion Ranger's "return a Forest" cost
        // match on subtypes, not the card name.
        obj.card_types.subtypes.push(name.to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(self.state.turn_number.saturating_sub(1));
        // Pre-existing land — see `add_creature` for the parallel rationale.
        obj.summoning_sick = false;
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
                target: None,
            },
        )
        .cost(crate::types::ability::AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(ability);
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
                excess: None,
            },
        );
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(ability);
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

    /// Add a creature card to a player's graveyard. Returns a `CardBuilder` for
    /// fluent chaining (e.g. `.with_mana_cost(...)`). Used to stage targets for
    /// graveyard-return effects (CR 404 — the graveyard zone).
    pub fn add_creature_to_graveyard(
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
            Zone::Graveyard,
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

    /// Add a creature card to a player's exile. Returns a `CardBuilder` for
    /// fluent chaining. Used to stage cards tracked by source-linked exile
    /// effects.
    pub fn add_creature_to_exile(
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
            Zone::Exile,
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

    /// Add a nonbasic land to the battlefield with abilities parsed from Oracle
    /// text. Mirrors `add_creature_from_oracle`; no existing helper places a
    /// land on the battlefield with parsed Oracle text (`add_basic_land` wires a
    /// hard-coded mana ability; `add_land_to_hand` places in `Zone::Hand`).
    pub fn add_land_from_oracle(
        &mut self,
        player: PlayerId,
        name: &str,
        oracle_text: &str,
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
        let obj = self.state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
        obj.timestamp = ts;
        // CR 302.6 note: summoning sickness only gates creatures, but the
        // builder models a pre-existing permanent (entered on a prior turn),
        // matching `add_creature`'s override.
        obj.summoning_sick = false;

        let mut builder = CardBuilder {
            state: &mut self.state,
            id,
        };
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

    /// Add an instant or sorcery to a player's hand without Oracle text.
    ///
    /// Use `is_instant: true` for instants, `false` for sorceries.
    pub fn add_spell_to_hand(
        &mut self,
        player: PlayerId,
        name: &str,
        is_instant: bool,
    ) -> CardBuilder<'_> {
        self.add_spell_to_zone(player, name, is_instant, Zone::Hand)
    }

    /// Add an instant or sorcery to the top of a player's library without Oracle text.
    ///
    /// Use `is_instant: true` for instants, `false` for sorceries.
    pub fn add_spell_to_library_top(
        &mut self,
        player: PlayerId,
        name: &str,
        is_instant: bool,
    ) -> CardBuilder<'_> {
        self.add_spell_to_zone(player, name, is_instant, Zone::Library)
    }

    /// Add an instant or sorcery to a player's graveyard without Oracle text.
    ///
    /// Use `is_instant: true` for instants, `false` for sorceries.
    pub fn add_spell_to_graveyard(
        &mut self,
        player: PlayerId,
        name: &str,
        is_instant: bool,
    ) -> CardBuilder<'_> {
        self.add_spell_to_zone(player, name, is_instant, Zone::Graveyard)
    }

    fn add_spell_to_zone(
        &mut self,
        player: PlayerId,
        name: &str,
        is_instant: bool,
        zone: Zone,
    ) -> CardBuilder<'_> {
        let card_id = CardId(self.state.next_object_id);
        let id = create_object(&mut self.state, card_id, player, name.to_string(), zone);
        let obj = self.state.objects.get_mut(&id).unwrap();
        let core_type = if is_instant {
            CoreType::Instant
        } else {
            CoreType::Sorcery
        };
        obj.card_types.core_types.push(core_type);
        obj.base_card_types = obj.card_types.clone();

        if zone == Zone::Library {
            let player_state = self
                .state
                .players
                .iter_mut()
                .find(|p| p.id == player)
                .expect("player exists");
            player_state.library.retain(|&oid| oid != id);
            player_state.library.insert(0, id);
        }

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
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(ability);
        self
    }

    pub fn with_ability_definition(&mut self, ability: AbilityDefinition) -> &mut Self {
        let obj = self.obj();
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(ability);
        self
    }

    /// Attach a static ability definition.
    pub fn with_static(&mut self, mode: StaticMode) -> &mut Self {
        let static_def = StaticDefinition::new(mode);
        let obj = self.obj();
        obj.static_definitions.push(static_def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def);
        self
    }

    pub fn with_static_definition(&mut self, static_def: StaticDefinition) -> &mut Self {
        let obj = self.obj();
        obj.static_definitions.push(static_def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def);
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
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def);
        self
    }

    /// Attach a trigger definition (mode only, no execute).
    pub fn with_trigger(&mut self, mode: TriggerMode) -> &mut Self {
        self.with_trigger_definition(TriggerDefinition::new(mode))
    }

    /// Attach a fully constructed trigger definition (with execute, zones, etc.).
    ///
    /// Appends to the printed base set and re-materializes so the live entry
    /// carries a real `Printed` occurrence ref — never the `Unmaterialized`
    /// fixture sentinel, which is unserializable by design.
    pub fn with_trigger_definition(&mut self, trigger: TriggerDefinition) -> &mut Self {
        let obj = self.obj();
        Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
        obj.materialize_base_trigger_definitions();
        self
    }

    pub fn with_replacement(
        &mut self,
        event: crate::types::replacements::ReplacementEvent,
    ) -> &mut Self {
        let replacement = ReplacementDefinition::new(event);
        let obj = self.obj();
        obj.replacement_definitions.push(replacement.clone());
        Arc::make_mut(&mut obj.base_replacement_definitions).push(replacement);
        self
    }

    pub fn with_replacement_definition(&mut self, def: ReplacementDefinition) -> &mut Self {
        let obj = self.obj();
        obj.replacement_definitions.push(def.clone());
        Arc::make_mut(&mut obj.base_replacement_definitions).push(def);
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

    /// CR 305: Make this card a land. Strips the Creature core type pushed by
    /// `add_creature_to_graveyard` and adds Land. Mirrors `as_artifact`/
    /// `as_enchantment`; reusable for graveyard-return targeting tests.
    pub fn as_land(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            obj.card_types.core_types.push(CoreType::Land);
        }
        self.sync_base_card_types();
        self
    }

    /// CR 302: Make this card a creature. Strips the spell core types
    /// (Instant/Sorcery) pushed by `add_spell_to_library_top` and adds Creature.
    /// Mirrors `as_sorcery`/`as_instant`; reusable for search/tutor library tests.
    pub fn as_creature(&mut self) -> &mut Self {
        let obj = self.obj();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Instant && *t != CoreType::Sorcery);
        if !obj.card_types.core_types.contains(&CoreType::Creature) {
            obj.card_types.core_types.push(CoreType::Creature);
        }
        self.sync_base_card_types();
        self
    }

    /// CR 306: Make this card a planeswalker with a printed loyalty number.
    /// If the object is already on the battlefield, mirror the printed loyalty
    /// into counters to model a pre-existing planeswalker fixture.
    pub fn as_planeswalker_with_loyalty(&mut self, subtype: &str, loyalty: u32) -> &mut Self {
        let obj = self.obj();
        obj.card_types.core_types.retain(|t| {
            !matches!(
                t,
                CoreType::Creature | CoreType::Instant | CoreType::Sorcery
            )
        });
        if !obj.card_types.core_types.contains(&CoreType::Planeswalker) {
            obj.card_types.core_types.push(CoreType::Planeswalker);
        }
        obj.card_types.subtypes = vec![subtype.to_string()];
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.loyalty = Some(loyalty);
        obj.base_loyalty = Some(loyalty);
        if obj.zone == Zone::Battlefield {
            obj.counters.insert(CounterType::Loyalty, loyalty);
        }
        self.sync_base_card_types();
        self
    }

    /// Add the Legendary supertype (CR 205.4a: a card's supertypes are printed
    /// on the type line; CR 205.4d: a permanent with the legendary supertype is
    /// subject to the "legend rule" state-based action).
    pub fn as_legendary(&mut self) -> &mut Self {
        let obj = self.obj();
        if !obj.card_types.supertypes.contains(&Supertype::Legendary) {
            obj.card_types.supertypes.push(Supertype::Legendary);
        }
        self.sync_base_card_types();
        self
    }

    // --- Special modifiers ---

    /// Mark this creature as having summoning sickness (entered this turn).
    pub fn with_summoning_sickness(&mut self) -> &mut Self {
        let turn = self.state.turn_number;
        let obj = self.obj();
        obj.entered_battlefield_turn = Some(turn);
        obj.summoning_sick = true;
        self
    }

    /// Set the mana cost of this card and derive its color from that cost.
    pub fn with_mana_cost(&mut self, cost: crate::types::mana::ManaCost) -> &mut Self {
        let obj = self.obj();
        obj.mana_cost = cost.clone();
        obj.base_mana_cost = cost.clone();
        let color = crate::game::printed_cards::derive_colors_from_mana_cost(&cost);
        obj.color = color.clone();
        obj.base_color = color;
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

    /// Pin a Strive-style per-target cost surcharge (CR 207.2c + CR 601.2f) on
    /// this card directly, bypassing Oracle-text parsing. Use when the parser's
    /// recognition of the surcharge clause is out of scope for the test (e.g. a
    /// card whose printed text predates the "Strive —" ability-word template).
    pub fn with_strive_cost(&mut self, cost: crate::types::mana::ManaCost) -> &mut Self {
        self.obj().strive_cost = Some(cost);
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
        let zone = self.state.objects.get(&self.id).unwrap().zone;
        let obj = self.state.objects.get(&self.id).unwrap();
        let face = build_face_from_oracle(obj, &kw_strings, oracle_text);
        let obj = self.state.objects.get_mut(&self.id).unwrap();
        apply_card_face_to_object(obj, &face);
        // CR 603.6a: Scenario seeding uses `create_object` + `add_to_zone`, not
        // `move_to_zone`, so ETB registration never runs. Re-index after Oracle
        // text is applied so synthesized upkeep triggers are consultable.
        if zone == Zone::Battlefield {
            crate::game::trigger_index::reindex_object_triggers(self.state, self.id);
        }
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
    /// Wrap a raw `GameState` so tests that build state imperatively (rather
    /// than via `GameScenario`) can still drive casts through the fluent
    /// [`GameRunner::cast`] pipeline. Pure additive escape hatch — the caller
    /// owns construction of `state` (phase, mana, objects).
    pub fn from_state(state: GameState) -> GameRunner {
        GameRunner { state }
    }

    /// Begin a fluent cast of `spell` through the full casting pipeline
    /// (CR 601.2a–h). See [`SpellCast`] for the builder methods and the
    /// driving-loop contract.
    pub fn cast(&mut self, spell: ObjectId) -> SpellCast<'_> {
        SpellCast::new(self, spell)
    }

    /// Begin a fluent activation of `source`'s ability at `ability_index`
    /// through the activation pipeline (CR 602 — activating an activated
    /// ability; CR 602.2b: the announcement-through-payment steps mirror casting
    /// CR 601.2b–i). See [`AbilityActivation`] for the builder methods and the
    /// driving-loop contract.
    pub fn activate(&mut self, source: ObjectId, ability_index: usize) -> AbilityActivation<'_> {
        AbilityActivation::new(self, source, ability_index)
    }

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

    /// Enter a synthetic mana-payment prompt for subsystem tests.
    ///
    /// Production casting creates `pending_cast` before `WaitingFor::ManaPayment`.
    /// Tests that start at the payment subsystem use this helper to preserve that
    /// invariant without open-coding a fake cast at each call site.
    pub fn enter_mana_payment(
        &mut self,
        player: PlayerId,
        convoke_mode: Option<ConvokeMode>,
    ) -> &mut Self {
        if self.state.pending_cast.is_none() {
            self.state.pending_cast = Some(Box::new(PendingCast::new(
                ObjectId(0),
                CardId(0),
                ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "SyntheticPaymentTestSpell".to_string(),
                        description: None,
                    },
                    vec![],
                    ObjectId(0),
                    player,
                ),
                crate::types::mana::ManaCost::NoCost,
            )));
        }
        self.state.waiting_for = WaitingFor::ManaPayment {
            player,
            convoke_mode,
        };
        self
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

    /// Drive `auto_advance` and then drain Upkeep and Draw priority windows so
    /// callers can test PreCombatMain trigger behavior without rebuilding the
    /// "skip empty priority steps" optimization at every call site. Stops as
    /// soon as the active player receives priority during `Phase::PreCombatMain`
    /// (or earlier if a priority-bearing trigger fires in Upkeep / Draw).
    ///
    /// CR 117.1c: priority opens during Upkeep and Draw, so reaching PreCombat
    /// Main from Untap requires explicit priority passing — this helper is the
    /// test-side analogue of the FE's auto-pass loop.
    pub fn auto_advance_to_main_phase(&mut self) -> WaitingFor {
        let mut events = Vec::new();
        let mut waiting = crate::game::turns::auto_advance(&mut self.state, &mut events);

        // Drain priority windows until the active player has priority during
        // PreCombatMain. Each iteration passes both players through one step;
        // bounded loop guards against unexpected non-priority states.
        for _ in 0..8 {
            if self.state.phase == Phase::PreCombatMain {
                break;
            }
            if !matches!(waiting, WaitingFor::Priority { .. }) {
                break;
            }
            let r1 = apply_as_current(&mut self.state, GameAction::PassPriority);
            let r2 = apply_as_current(&mut self.state, GameAction::PassPriority);
            match (r1, r2) {
                (Ok(_), Ok(result)) => waiting = result.waiting_for,
                _ => break,
            }
        }
        waiting
    }

    /// Advance the turn structure until the active player reaches `phase`
    /// (CR 500.1 — turn phases/steps proceed in a fixed order). Drives the
    /// engine's own `auto_advance` turn machinery (CR 500.2) and passes both
    /// players' priority (CR 117) at each step that opens a priority window,
    /// stopping when the target phase is reached or when a non-priority prompt
    /// the helper can't auto-pass surfaces.
    ///
    /// Bounded loop: a turn has 12 phases/steps (CR 500.1), so a generous cap
    /// guards against an unexpectedly stuck transition rather than spinning.
    pub fn advance_to_phase(&mut self, phase: Phase) {
        let mut events = Vec::new();
        let mut waiting = crate::game::turns::auto_advance(&mut self.state, &mut events);
        for _ in 0..32 {
            if self.state.phase == phase {
                break;
            }
            if !matches!(waiting, WaitingFor::Priority { .. }) {
                break;
            }
            let r1 = apply_as_current(&mut self.state, GameAction::PassPriority);
            let r2 = apply_as_current(&mut self.state, GameAction::PassPriority);
            match (r1, r2) {
                (Ok(_), Ok(result)) => waiting = result.waiting_for,
                _ => break,
            }
        }
    }

    /// Advance to the declare-attackers step (CR 508). The engine surfaces
    /// `WaitingFor::DeclareAttackers` there as the declare-attackers turn-based
    /// action (CR 508.1).
    pub fn advance_to_combat(&mut self) {
        self.advance_to_phase(Phase::DeclareAttackers);
    }

    /// Advance to the end step (CR 513).
    pub fn advance_to_end_step(&mut self) {
        self.advance_to_phase(Phase::End);
    }

    /// Advance to the upkeep step (CR 503).
    pub fn advance_to_upkeep(&mut self) {
        self.advance_to_phase(Phase::Upkeep);
    }

    /// Declare attackers (CR 508.1). Must be called when the engine is at
    /// `WaitingFor::DeclareAttackers` (use [`GameRunner::advance_to_combat`]).
    /// Each entry is `(attacker, defender)` where `defender` is an
    /// [`AttackTarget`](crate::game::combat::AttackTarget) — a player,
    /// planeswalker, or battle (CR 508.1b).
    pub fn declare_attackers(
        &mut self,
        attacks: &[(ObjectId, crate::game::combat::AttackTarget)],
    ) -> Result<ActionResult, EngineError> {
        apply_as_current(
            &mut self.state,
            GameAction::DeclareAttackers {
                attacks: attacks.to_vec(),
                bands: vec![],
            },
        )
    }

    /// Declare blockers (CR 509.1). Must be called when the engine is at
    /// `WaitingFor::DeclareBlockers`. Each entry is `(blocker, attacker)` —
    /// the blocking creature and the attacker it blocks (CR 509.1a).
    pub fn declare_blockers(
        &mut self,
        blocks: &[(ObjectId, ObjectId)],
    ) -> Result<ActionResult, EngineError> {
        apply_as_current(
            &mut self.state,
            GameAction::DeclareBlockers {
                assignments: blocks.to_vec(),
            },
        )
    }

    /// Drive combat through the combat-damage step(s) to the end of combat,
    /// then return an [`Outcome`]. Passes both players' priority (CR 117) so
    /// the engine performs the combat-damage step (CR 510.1–510.2: assign then
    /// deal) and, if any first/double strike is present, the extra combat-damage
    /// step (CR 506.1 / CR 702.7b), draining any damage-triggered abilities
    /// (CR 510.3a) along the way, until the end-of-combat step (CR 511) opens a
    /// clean priority window or the stack settles.
    ///
    /// Life totals snapshot at the call (so `life_delta` reads the combat-damage
    /// delta) and the hand baseline is captured at the same point.
    pub fn combat_damage(&mut self) -> Outcome {
        let life_before: Vec<(PlayerId, i32)> =
            self.state.players.iter().map(|p| (p.id, p.life)).collect();
        let hand_baseline: Vec<(PlayerId, usize)> = self
            .state
            .players
            .iter()
            .map(|p| (p.id, p.hand.len()))
            .collect();
        let mut events = Vec::new();

        // Drive through the combat-damage step(s) to end of combat. The
        // combat-damage assignment and dealing are turn-based actions
        // (CR 510.1–510.2); the active player then gets priority (CR 510.3), and
        // passing it advances toward the end-of-combat step. Drain ordering
        // prompts (CR 603.3b) for damage triggers en route.
        for _ in 0..32 {
            if self.state.phase == Phase::EndCombat || self.state.phase == Phase::PostCombatMain {
                break;
            }
            if matches!(self.state.waiting_for, WaitingFor::OrderTriggers { .. }) {
                super::triggers::drain_order_triggers_with_identity(&mut self.state);
                continue;
            }
            if !matches!(self.state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            match apply_as_current(&mut self.state, GameAction::PassPriority) {
                Ok(result) => events.extend(result.events),
                Err(_) => break,
            }
        }

        Outcome {
            state: self.state.clone(),
            events,
            hand_baseline,
            life_before,
        }
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
            // CR 603.3b (#531): drain the per-controller ordering prompt with identity
            // before checking stack emptiness — the prompt can surface mid-resolution
            // with an empty stack while triggers wait to be dispatched.
            if matches!(self.state.waiting_for, WaitingFor::OrderTriggers { .. }) {
                super::triggers::drain_order_triggers_with_identity(&mut self.state);
                continue;
            }
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
            WaitingFor::MeldPairChoice { .. } => "MeldPairChoice",
            WaitingFor::MeldAttackTargetChoice { .. } => "MeldAttackTargetChoice",
            WaitingFor::MulliganDecision { .. } => "MulliganDecision",
            WaitingFor::OpeningHandBottomCards { .. } => "OpeningHandBottomCards",
            WaitingFor::ManaPayment { .. } => "ManaPayment",
            WaitingFor::TargetSelection { .. } => "TargetSelection",
            WaitingFor::DeclareAttackers { .. } => "DeclareAttackers",
            WaitingFor::DeclareBlockers { .. } => "DeclareBlockers",
            WaitingFor::UntapChoice { .. } => "UntapChoice",
            WaitingFor::ChooseUntapSubset { .. } => "ChooseUntapSubset",
            WaitingFor::ExertChoice { .. } => "ExertChoice",
            WaitingFor::EnlistChoice { .. } => "EnlistChoice",
            WaitingFor::GameOver { .. } => "GameOver",
            WaitingFor::ReplacementChoice { .. } => "ReplacementChoice",
            WaitingFor::OrderTriggers { .. } => "OrderTriggers",
            WaitingFor::CopyTargetChoice { .. } => "CopyTargetChoice",
            WaitingFor::ExploreChoice { .. } => "ExploreChoice",
            WaitingFor::ReturnAsAuraTarget { .. } => "ReturnAsAuraTarget",
            WaitingFor::EquipTarget { .. } => "EquipTarget",
            WaitingFor::ScryChoice { .. } => "ScryChoice",
            WaitingFor::ArrangePlanarDeckTopChoice { .. } => "ArrangePlanarDeckTopChoice",
            WaitingFor::RedistributeLifeTotals { .. } => "RedistributeLifeTotals",
            WaitingFor::CoinFlipKeepChoice { .. } => "CoinFlipKeepChoice",
            WaitingFor::DigChoice { .. } => "DigChoice",
            WaitingFor::SurveilChoice { .. } => "SurveilChoice",
            WaitingFor::RevealChoice { .. } => "RevealChoice",
            WaitingFor::SearchChoice { .. } => "SearchChoice",
            WaitingFor::SearchPartitionChoice { .. } => "SearchPartitionChoice",
            WaitingFor::OutsideGameChoice { .. } => "OutsideGameChoice",
            WaitingFor::ChooseFromZoneChoice { .. } => "ChooseFromZoneChoice",
            WaitingFor::BeholdChoice { .. } => "BeholdChoice",
            WaitingFor::ChooseOneOfBranch { .. } => "ChooseOneOfBranch",
            WaitingFor::ConniveDiscard { .. } => "ConniveDiscard",
            WaitingFor::DiscardChoice { .. } => "DiscardChoice",
            WaitingFor::EffectZoneChoice { .. } => "EffectZoneChoice",
            WaitingFor::DrawnThisTurnTopdeckChoice { .. } => "DrawnThisTurnTopdeckChoice",
            WaitingFor::ManifestDreadChoice { .. } => "ManifestDreadChoice",
            WaitingFor::TriggerTargetSelection { .. } => "TriggerTargetSelection",
            WaitingFor::BetweenGamesSideboard { .. } => "BetweenGamesSideboard",
            WaitingFor::BetweenGamesChoosePlayDraw { .. } => "BetweenGamesChoosePlayDraw",
            WaitingFor::NamedChoice { .. } => "NamedChoice",
            WaitingFor::OpponentGuess { .. } => "OpponentGuess",
            WaitingFor::SpellbookDraft { .. } => "SpellbookDraft",
            WaitingFor::DamageSourceChoice { .. } => "DamageSourceChoice",
            WaitingFor::ModeChoice { .. } => "ModeChoice",
            WaitingFor::DiscardToHandSize { .. } => "DiscardToHandSize",
            WaitingFor::OptionalCostChoice { .. } => "OptionalCostChoice",
            WaitingFor::CostTypeChoice { .. } => "CostTypeChoice",
            WaitingFor::SpliceOffer { .. } => "SpliceOffer",
            WaitingFor::DefilerPayment { .. } => "DefilerPayment",
            WaitingFor::CastOffer {
                kind: CastOfferKind::Adventure { .. },
                ..
            } => "AdventureCastChoice",
            WaitingFor::ModalFaceChoice { .. } => "ModalFaceChoice",
            WaitingFor::AlternativeCastChoice { keyword, .. } => match keyword {
                crate::types::game_state::AlternativeCastKeyword::Warp => {
                    "AlternativeCastChoice(Warp)"
                }
                crate::types::game_state::AlternativeCastKeyword::Evoke => {
                    "AlternativeCastChoice(Evoke)"
                }
                crate::types::game_state::AlternativeCastKeyword::Emerge => {
                    "AlternativeCastChoice(Emerge)"
                }
                crate::types::game_state::AlternativeCastKeyword::Dash => {
                    "AlternativeCastChoice(Dash)"
                }
                crate::types::game_state::AlternativeCastKeyword::Blitz => {
                    "AlternativeCastChoice(Blitz)"
                }
                crate::types::game_state::AlternativeCastKeyword::Spectacle => {
                    "AlternativeCastChoice(Spectacle)"
                }
                crate::types::game_state::AlternativeCastKeyword::Overload => {
                    "AlternativeCastChoice(Overload)"
                }
                crate::types::game_state::AlternativeCastKeyword::Bestow => {
                    "AlternativeCastChoice(Bestow)"
                }
                crate::types::game_state::AlternativeCastKeyword::Awaken => {
                    "AlternativeCastChoice(Awaken)"
                }
                crate::types::game_state::AlternativeCastKeyword::Cleave => {
                    "AlternativeCastChoice(Cleave)"
                }
                crate::types::game_state::AlternativeCastKeyword::MoreThanMeetsTheEye => {
                    "AlternativeCastChoice(MoreThanMeetsTheEye)"
                }
                crate::types::game_state::AlternativeCastKeyword::Impending => {
                    "AlternativeCastChoice(Impending)"
                }
                crate::types::game_state::AlternativeCastKeyword::Prototype => {
                    "AlternativeCastChoice(Prototype)"
                }
                crate::types::game_state::AlternativeCastKeyword::Mutate => {
                    "AlternativeCastChoice(Mutate)"
                }
                crate::types::game_state::AlternativeCastKeyword::Prowl => {
                    "AlternativeCastChoice(Prowl)"
                }
                crate::types::game_state::AlternativeCastKeyword::FaceDown => {
                    "AlternativeCastChoice(FaceDown)"
                }
            },
            WaitingFor::MutateMergeChoice { .. } => "MutateMergeChoice",
            WaitingFor::CipherEncodeChoice { .. } => "CipherEncodeChoice",
            WaitingFor::CastingVariantChoice { .. } => "CastingVariantChoice",
            WaitingFor::ChoosePermanentTypeSlot { .. } => "ChoosePermanentTypeSlot",
            WaitingFor::MultiTargetSelection { .. } => "MultiTargetSelection",
            WaitingFor::AbilityModeChoice { .. } => "AbilityModeChoice",
            WaitingFor::OptionalEffectChoice { .. } => "OptionalEffectChoice",
            WaitingFor::PairChoice { .. } => "PairChoice",
            WaitingFor::OpponentMayChoice { .. } => "OpponentMayChoice",
            WaitingFor::LoopShortcut { .. } => "LoopShortcut",
            WaitingFor::RespondToShortcut { .. } => "RespondToShortcut",
            WaitingFor::PrecastCopyShortcutOffer { .. } => "PrecastCopyShortcutOffer",
            WaitingFor::RespondToPrecastCopyShortcut { .. } => "RespondToPrecastCopyShortcut",
            WaitingFor::TributeChoice { .. } => "TributeChoice",
            WaitingFor::UnlessPayment { .. } => "UnlessPayment",
            WaitingFor::UnlessPaymentChooseCost { .. } => "UnlessPaymentChooseCost",
            WaitingFor::CompanionReveal { .. } => "CompanionReveal",
            WaitingFor::ChooseRingBearer { .. } => "ChooseRingBearer",
            WaitingFor::ChooseRoomDoor { .. } => "ChooseRoomDoor",
            WaitingFor::PayCost { .. } => "PayCost",
            WaitingFor::ChooseManaColor { .. } => "ChooseManaColor",
            WaitingFor::PayManaAbilityMana { .. } => "PayManaAbilityMana",
            WaitingFor::CollectEvidenceChoice { .. } => "CollectEvidenceChoice",
            WaitingFor::HarmonizeTapChoice { .. } => "HarmonizeTapChoice",
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { .. },
                ..
            } => "DiscoverChoice",
            WaitingFor::CastOffer {
                kind: CastOfferKind::GraveyardPaidCast { .. },
                ..
            } => "GraveyardPaidCastChoice",
            WaitingFor::RevealUntilKeptChoice { .. } => "RevealUntilKeptChoice",
            WaitingFor::RepeatDecision { .. } => "RepeatDecision",
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { .. },
                ..
            } => "CascadeChoice",
            WaitingFor::CastOffer {
                kind: CastOfferKind::Ripple { .. },
                ..
            } => "RippleChoice",
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            } => "FreeCastWindow",
            WaitingFor::TopOrBottomChoice { .. } => "TopOrBottomChoice",
            WaitingFor::ChooseLegend { .. } => "ChooseLegend",
            WaitingFor::BattleProtectorChoice { .. } => "BattleProtectorChoice",
            WaitingFor::ProliferateChoice { .. } => "ProliferateChoice",
            WaitingFor::TimeTravelChoice { .. } => "TimeTravelChoice",
            WaitingFor::AssistChoosePlayer { .. } => "AssistChoosePlayer",
            WaitingFor::AssistPayment { .. } => "AssistPayment",
            WaitingFor::ChooseObjectsSelection { .. } => "ChooseObjectsSelection",
            WaitingFor::CopyRetarget { .. } => "CopyRetarget",
            WaitingFor::AssignCombatDamage { .. } => "AssignCombatDamage",
            WaitingFor::AssignBlockerDamage { .. } => "AssignBlockerDamage",
            WaitingFor::DistributeAmong { .. } => "DistributeAmong",
            WaitingFor::MoveCountersDistribution { .. } => "MoveCountersDistribution",
            WaitingFor::RemoveCountersChoice { .. } => "RemoveCountersChoice",
            WaitingFor::PayAmountChoice { .. } => "PayAmountChoice",
            WaitingFor::RetargetChoice { .. } => "RetargetChoice",
            WaitingFor::WardDiscardChoice { .. } => "WardDiscardChoice",
            WaitingFor::WardSacrificeChoice { .. } => "WardSacrificeChoice",
            WaitingFor::UnlessBounceChoice { .. } => "UnlessBounceChoice",
            WaitingFor::LearnChoice { .. } => "LearnChoice",
            WaitingFor::CrewVehicle { .. } => "CrewVehicle",
            WaitingFor::StationTarget { .. } => "StationTarget",
            WaitingFor::SaddleMount { .. } => "SaddleMount",
            WaitingFor::ChooseDungeon { .. } => "ChooseDungeon",
            WaitingFor::ChooseDungeonRoom { .. } => "ChooseDungeonRoom",
            WaitingFor::SpecializeColor { .. } => "SpecializeColor",
            WaitingFor::PopulateChoice { .. } => "PopulateChoice",
            WaitingFor::ClashChooseOpponent { .. } => "ClashChooseOpponent",
            WaitingFor::ChooseFromZoneOpponentChooser { .. } => "ChooseFromZoneOpponentChooser",
            WaitingFor::ChooseAnnouncingOpponent { .. } => "ChooseAnnouncingOpponent",
            WaitingFor::ClashCardPlacement { .. } => "ClashCardPlacement",
            WaitingFor::VoteChoice { .. } => "VoteChoice",
            WaitingFor::CategoryChoice { .. } => "CategoryChoice",
            WaitingFor::EachPlayerCopyChosenSelection { .. } => "EachPlayerCopyChosenSelection",
            WaitingFor::KeepWithinTotalPowerChoice { .. } => "KeepWithinTotalPowerChoice",
            WaitingFor::KeepExactPermanentsChoice { .. } => "KeepExactPermanentsChoice",
            WaitingFor::ChooseXValue { .. } => "ChooseXValue",
            WaitingFor::CombatTaxPayment { .. } => "CombatTaxPayment",
            WaitingFor::PhyrexianPayment { .. } => "PhyrexianPayment",
            WaitingFor::BlightChoice { .. } => "BlightChoice",
            WaitingFor::CastOffer {
                kind: CastOfferKind::Paradigm { .. },
                ..
            } => "ParadigmCastOffer",
            WaitingFor::MiracleReveal { .. } => "MiracleReveal",
            WaitingFor::CastOffer {
                kind: CastOfferKind::Miracle { .. },
                ..
            } => "MiracleCastOffer",
            WaitingFor::CastOffer {
                kind: CastOfferKind::Madness { .. },
                ..
            } => "MadnessCastOffer",
            WaitingFor::CommanderZoneChoice { .. } => "CommanderZoneChoice",
            WaitingFor::SeparatePilesChooseOpponent { .. } => "SeparatePilesChooseOpponent",
            WaitingFor::SeparatePilesPartition { .. } => "SeparatePilesPartition",
            WaitingFor::SeparatePilesChoice { .. } => "SeparatePilesChoice",
            WaitingFor::ActivationCostOneOfChoice { .. } => "ActivationCostOneOfChoice",
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
// SpellCast (fluent cast-pipeline driver)
// ---------------------------------------------------------------------------

/// Fluent builder that drives a spell through the full casting pipeline
/// (CR 601.2a–h): announcement, mode choice, X announcement, target selection,
/// mana payment (including convoke), and resolution. Constructed via
/// [`GameRunner::cast`].
///
/// The driver exists to make five test-harness foot-guns structurally
/// impossible:
///
/// 1. **Hand-written `TargetRef` vectors.** Callers declare *intent*
///    (`.target_object`, `.target_player`); the driver matches that intent to
///    each slot's `legal_targets` itself. No flat `SelectTargets` vector is
///    ever built by hand.
/// 2. **Incomplete modal target submission.** The driver answers exactly one
///    slot per `ChooseTarget`, walking `target_slots` in written order
///    (CR 601.2c) so every mode's slot is covered.
/// 3. **Hand baseline captured at the wrong point.** CR 601.2a: the spell
///    leaves hand only when it is put on the stack. The driver captures the
///    per-player hand-size baseline at stack commit (the `Priority` window),
///    so [`CastOutcome::hand_drawn`] reports a clean resolution delta.
/// 4. **Keywords fed as inline Oracle text.** Convoke (and other keywords) must
///    be built via [`CardBuilder::from_oracle_text_with_keywords`]; the driver
///    pays convoke via `TapForConvoke` rather than reparsing reminder text.
/// 5. **Asserting representation-internal flags.** [`CastOutcome`] exposes
///    behavior/semantic deltas (`hand_drawn`, `zone_of`, `life_delta`,
///    `final_waiting_for`) instead of dual-encoded AST fields.
pub struct SpellCast<'a> {
    runner: &'a mut GameRunner,
    spell: ObjectId,
    alternative_cast: Option<AlternativeCastDecision>,
    adventure_creature: Option<bool>,
    casting_variant: Option<CastingVariant>,
    free_cast: bool,
    modes: Option<Vec<usize>>,
    x: Option<u32>,
    target_players: Vec<PlayerId>,
    target_objects: Vec<ObjectId>,
    cost_objects: Vec<ObjectId>,
    distribution: Option<Vec<(TargetRef, u32)>>,
    convoke_with: Vec<ObjectId>,
    delve_with: Vec<ObjectId>,
    optional: OptionalPolicy,
    search_pick: SearchPolicy,
    modal_back_face: Option<bool>,
    replacement_choice: Option<usize>,
    named_choice: Option<String>,
    discard_cards: Vec<ObjectId>,
    effect_zone_cards: Vec<ObjectId>,
    copy_target: Option<ObjectId>,
    spellbook_pick: Option<String>,
}

impl<'a> SpellCast<'a> {
    fn new(runner: &'a mut GameRunner, spell: ObjectId) -> Self {
        SpellCast {
            runner,
            spell,
            alternative_cast: None,
            adventure_creature: None,
            casting_variant: None,
            free_cast: false,
            modes: None,
            x: None,
            target_players: Vec::new(),
            target_objects: Vec::new(),
            cost_objects: Vec::new(),
            distribution: None,
            convoke_with: Vec::new(),
            delve_with: Vec::new(),
            optional: OptionalPolicy::default(),
            search_pick: SearchPolicy::default(),
            modal_back_face: None,
            replacement_choice: None,
            named_choice: None,
            discard_cards: Vec::new(),
            effect_zone_cards: Vec::new(),
            copy_target: None,
            spellbook_pick: None,
        }
    }

    /// Submit the first legal candidates at any `SearchChoice` during
    /// resolution (CR 701.23).
    pub fn search_first_legal(mut self) -> Self {
        self.search_pick = SearchPolicy::FirstLegal;
        self
    }

    /// Submit an empty candidate set at any `SearchChoice` during resolution
    /// (CR 701.23d fail-to-find).
    pub fn search_none(mut self) -> Self {
        self.search_pick = SearchPolicy::None;
        self
    }

    /// Accept optional ("you may") effects/costs during resolution
    /// (CR 608.2d / CR 601.2f). Mirrors [`AbilityActivation::accept_optional`].
    pub fn accept_optional(mut self) -> Self {
        self.optional = OptionalPolicy::Accept;
        self
    }

    /// Decline optional ("you may") effects/costs during resolution.
    pub fn decline_optional(mut self) -> Self {
        self.optional = OptionalPolicy::Decline;
        self
    }

    /// Declare the modal "choose N" mode indices for a modal spell (CR 700.2).
    /// Omit for non-modal spells.
    pub fn modes(mut self, modes: &[usize]) -> Self {
        self.modes = Some(modes.to_vec());
        self
    }

    /// Announce the value of X (CR 107.3a / CR 601.2b). Omit for non-X spells.
    pub fn x(mut self, value: u32) -> Self {
        self.x = Some(value);
        self
    }

    /// Choose a cast variant when the engine offers a `CastingVariantChoice`
    /// (CR 601.2b). Omit for ordinary casts; the driver panics if a spell
    /// surfaces a variant choice without an explicit test intent.
    pub fn casting_variant(mut self, variant: CastingVariant) -> Self {
        self.casting_variant = Some(variant);
        self
    }

    /// Elect the free `HandPermission` option at a `CastingVariantChoice` without
    /// having to name the granting source's `ObjectId` (CR 118.9 / CR 118.9a). The
    /// driver picks the first `CastingVariant::HandPermission` option the menu
    /// offers — the Omniscience-class "cast without paying its mana cost" branch.
    pub fn free_cast(mut self) -> Self {
        self.free_cast = true;
        self
    }

    /// Choose whether to pay a keyword-granted alternative cost offered during
    /// announcement (CR 601.2b / CR 118.9).
    pub fn alternative_cast(mut self, decision: AlternativeCastDecision) -> Self {
        self.alternative_cast = Some(decision);
        self
    }

    /// Choose the creature face (`true`) or Adventure face (`false`) when the
    /// engine offers an Adventure cast choice (CR 715.3a).
    pub fn adventure_face(mut self, creature: bool) -> Self {
        self.adventure_creature = Some(creature);
        self
    }

    /// Declare a player as an intended target (CR 601.2c). Matched to the first
    /// slot whose `legal_targets` contains it.
    pub fn target_player(mut self, player: PlayerId) -> Self {
        self.target_players.push(player);
        self
    }

    /// Declare several players as intended targets, in order.
    pub fn target_players(mut self, players: &[PlayerId]) -> Self {
        self.target_players.extend_from_slice(players);
        self
    }

    /// Declare an object as an intended target (CR 601.2c). Matched to the first
    /// unused slot whose `legal_targets` contains it.
    pub fn target_object(mut self, object: ObjectId) -> Self {
        self.target_objects.push(object);
        self
    }

    /// Declare several objects as intended targets, in order.
    pub fn target_objects(mut self, objects: &[ObjectId]) -> Self {
        self.target_objects.extend_from_slice(objects);
        self
    }

    /// Select objects for an announcement-time `PayCost` prompt such as an
    /// additional sacrifice cost (CR 601.2f / CR 118.3).
    pub fn pay_cost_with(mut self, objects: &[ObjectId]) -> Self {
        self.cost_objects.extend_from_slice(objects);
        self
    }

    /// Select permanents for an announcement-time sacrifice cost (CR 701.21a).
    pub fn sacrifice_with(self, objects: &[ObjectId]) -> Self {
        self.pay_cost_with(objects)
    }

    /// Submit an explicit distribution for a `DistributeAmong` prompt
    /// (CR 601.2d / CR 608.2d).
    pub fn distribute_among(mut self, distribution: &[(TargetRef, u32)]) -> Self {
        self.distribution = Some(distribution.to_vec());
        self
    }

    /// Choose which face of a spell//spell MDFC or split card to cast (CR
    /// 712.11b / CR 709.3). Required when the engine surfaces
    /// `WaitingFor::ModalFaceChoice`.
    pub fn modal_back_face(mut self, back: bool) -> Self {
        self.modal_back_face = Some(back);
        self
    }

    /// Alias for [`SpellCast::modal_back_face`] matching the engine prompt name.
    pub fn modal_face(self, back: bool) -> Self {
        self.modal_back_face(back)
    }

    /// Choose a replacement candidate index at a `ReplacementChoice` prompt
    /// during resolution (CR 616.1).
    pub fn replacement_choice(mut self, index: usize) -> Self {
        self.replacement_choice = Some(index);
        self
    }

    /// Choose a named/string option at a `NamedChoice` prompt.
    pub fn choose_option(mut self, choice: &str) -> Self {
        self.named_choice = Some(choice.to_string());
        self
    }

    /// Select cards for a resolution-time discard prompt (CR 701.9b).
    pub fn discard(mut self, cards: &[ObjectId]) -> Self {
        self.discard_cards.extend_from_slice(cards);
        self
    }

    /// Select cards/permanents for a resolution-time zone choice.
    pub fn effect_zone(mut self, cards: &[ObjectId]) -> Self {
        self.effect_zone_cards.extend_from_slice(cards);
        self
    }

    /// Choose a permanent for a copy-as-enters prompt (CR 707.9).
    pub fn copy_target(mut self, target: ObjectId) -> Self {
        self.copy_target = Some(target);
        self
    }

    /// Draft this card name at any `SpellbookDraft` prompt during resolution
    /// (Alchemy `Effect::DraftFromSpellbook`). Mirrors [`choose_option`].
    pub fn spellbook_pick(mut self, name: &str) -> Self {
        self.spellbook_pick = Some(name.to_string());
        self
    }

    /// Tap these creatures to pay the cost via Convoke (CR 702.51a). Each is
    /// tapped during the `ManaPayment { convoke_mode }` window with mana of the
    /// creature's first declared color (falling back to colorless for the
    /// generic portion of the cost — CR 702.51b).
    pub fn convoke_with(mut self, creatures: &[ObjectId]) -> Self {
        self.convoke_with.extend_from_slice(creatures);
        self
    }

    /// Exile these cards from the caster's graveyard to pay generic mana via
    /// Delve (CR 702.66a).
    pub fn delve_with(mut self, cards: &[ObjectId]) -> Self {
        self.delve_with.extend_from_slice(cards);
        self
    }

    /// Drive the cast pipeline until the spell is committed to the stack and a
    /// priority window opens (CR 601.2i). Use this when a test must inspect the
    /// live stack object before resolution.
    pub fn commit(self) -> CastCommit<'a> {
        self.try_commit()
            .expect("SpellCast commit must be accepted by the engine")
    }

    fn try_commit(self) -> Result<CastCommit<'a>, EngineError> {
        let SpellCast {
            runner,
            spell,
            alternative_cast,
            adventure_creature,
            casting_variant,
            free_cast,
            modes,
            x,
            target_players,
            target_objects,
            cost_objects,
            distribution,
            convoke_with,
            delve_with,
            optional,
            search_pick,
            modal_back_face,
            replacement_choice,
            named_choice,
            discard_cards,
            effect_zone_cards,
            copy_target,
            spellbook_pick,
        } = self;

        // CR 119.3: snapshot life totals before the cast so `life_delta` reads a
        // clean pre-cast → final difference.
        let life_before: Vec<(PlayerId, i32)> = runner
            .state
            .players
            .iter()
            .map(|p| (p.id, p.life))
            .collect();

        let card_id = runner.state.objects[&spell].card_id;
        let mut events = Vec::new();
        act_collect(
            runner,
            GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            },
            &mut events,
        )?;

        // Intent the driver matches as it walks slots: object targets are
        // consumed one per slot (most slots are object slots), while player
        // targets are reusable across slots (one player may be targeted by
        // several modes — see `pick_slot_target`).
        let mut remaining_objects: Vec<ObjectId> = target_objects;
        let declared_players: Vec<PlayerId> = target_players;
        let mut remaining_cost_objects: Vec<ObjectId> = cost_objects;

        // CR 601.2a: the spell leaves hand only at stack commit. Captured when
        // the driver reaches the post-cast `Priority` window.
        let mut hand_at_commit: Option<Vec<(PlayerId, usize)>> = None;
        let mut selected_casting_variant: Option<CastingVariantChoiceOption> = None;

        for _ in 0..64 {
            match &runner.state.waiting_for {
                WaitingFor::CastOffer {
                    kind: CastOfferKind::Adventure { .. },
                    ..
                } => {
                    let creature = adventure_creature.unwrap_or_else(|| {
                        panic!(
                            "SpellCast reached WaitingFor::CastOffer(Adventure) but no \
                             .adventure_face(..) was declared — declare which face to cast"
                        )
                    });
                    act_collect(
                        runner,
                        GameAction::ChooseAdventureFace { creature },
                        &mut events,
                    )?;
                }
                WaitingFor::ModalFaceChoice { .. } => {
                    let back = modal_back_face.unwrap_or_else(|| {
                        panic!(
                            "SpellCast reached WaitingFor::ModalFaceChoice but no \
                             .modal_back_face(..) was declared — declare which face to cast"
                        )
                    });
                    act_collect(
                        runner,
                        GameAction::ChooseModalFace { back_face: back },
                        &mut events,
                    )?;
                }
                WaitingFor::AlternativeCastChoice { .. } => {
                    let choice = alternative_cast.unwrap_or_else(|| {
                        panic!(
                            "SpellCast reached WaitingFor::AlternativeCastChoice but no \
                             .alternative_cast(..) was declared — declare normal vs alternative"
                        )
                    });
                    act_collect(
                        runner,
                        GameAction::ChooseAlternativeCast { choice },
                        &mut events,
                    )?;
                }
                WaitingFor::CastingVariantChoice { options, .. } => {
                    // CR 118.9 / CR 118.9a: `.free_cast()` elects the first
                    // `HandPermission` option without naming the source id;
                    // otherwise match the explicitly declared `.casting_variant(..)`.
                    let index = if free_cast {
                        options
                            .iter()
                            .position(|option| {
                                matches!(option.variant, CastingVariant::HandPermission { .. })
                            })
                            .unwrap_or_else(|| {
                                panic!(
                                    "SpellCast .free_cast() found no HandPermission option in {:?}",
                                    options
                                )
                            })
                    } else {
                        let variant = casting_variant.unwrap_or_else(|| {
                            panic!(
                                "SpellCast reached WaitingFor::CastingVariantChoice but no \
                                 .casting_variant(..) was declared — declare the intended cast variant"
                            )
                        });
                        options
                            .iter()
                            .position(|option| option.variant == variant)
                            .unwrap_or_else(|| {
                                panic!(
                                    "SpellCast could not find requested cast variant {:?} in options {:?}",
                                    variant, options
                                )
                            })
                    };
                    selected_casting_variant = Some(options[index].clone());
                    act_collect(
                        runner,
                        GameAction::ChooseCastingVariant { index },
                        &mut events,
                    )?;
                }
                // CR 601.2b: modal spell announces its mode choice.
                WaitingFor::ModeChoice { .. } => {
                    let indices = modes.clone().unwrap_or_else(|| {
                        panic!(
                            "SpellCast reached WaitingFor::ModeChoice but no .modes(..) were \
                             declared — this is a modal spell; declare its chosen mode indices"
                        )
                    });
                    act_collect(runner, GameAction::SelectModes { indices }, &mut events)?;
                }
                // CR 107.3a / CR 601.2b: announce X.
                WaitingFor::ChooseXValue { .. } => {
                    let value = x.unwrap_or_else(|| {
                        panic!(
                            "SpellCast reached WaitingFor::ChooseXValue but no .x(..) was \
                             declared — this spell needs X announced"
                        )
                    });
                    act_collect(runner, GameAction::ChooseX { value }, &mut events)?;
                }
                // CR 601.2f: optional additional costs are chosen during
                // announcement, before the spell is committed to the stack.
                WaitingFor::OptionalCostChoice { .. } => {
                    let pay = matches!(optional, OptionalPolicy::Accept);
                    act_collect(runner, GameAction::DecideOptionalCost { pay }, &mut events)?;
                }
                // CR 601.2f / CR 118.3: additional non-mana costs that require
                // selecting objects, such as sacrificing a creature.
                WaitingFor::PayCost {
                    choices,
                    count,
                    min_count,
                    ..
                } => {
                    let chosen = pick_declared_cards(
                        choices,
                        *min_count,
                        *count,
                        &mut remaining_cost_objects,
                        "PayCost",
                    );
                    act_collect(
                        runner,
                        GameAction::SelectCards { cards: chosen },
                        &mut events,
                    )?;
                }
                WaitingFor::DistributeAmong { total, targets, .. } => {
                    let distribution = distribution
                        .clone()
                        .unwrap_or_else(|| default_distribution(*total, targets));
                    act_collect(
                        runner,
                        GameAction::DistributeAmong { distribution },
                        &mut events,
                    )?;
                }
                // CR 702.51a / CR 702.66a / CR 601.2g–h: mana payment, possibly
                // via convoke or delve.
                //
                // Most pool-funded casts auto-pay and never surface this window.
                // Convoke (CR 702.51) and Delve (CR 702.66a) each open a
                // `ManaPayment { convoke_mode }` window even when the controller
                // intends to pay entirely from their pool. With no declared
                // contributors, their loops are no-ops and the trailing
                // `PassPriority` finalizes the remaining cost from the pool. If
                // the pool can't cover it, `PassPriority` errors and the `.expect`
                // below fails loudly — fund the pool in the scenario.
                WaitingFor::ManaPayment { convoke_mode, .. } => {
                    if matches!(convoke_mode, Some(ConvokeMode::Delve)) {
                        for &card in &delve_with {
                            act_collect(
                                runner,
                                GameAction::TapForConvoke {
                                    object_id: card,
                                    mana_type: ManaType::Colorless,
                                },
                                &mut events,
                            )?;
                        }
                    } else {
                        for &creature in &convoke_with {
                            // CR 702.51b: pay one mana of the creature's color, or
                            // colorless toward the generic portion of the cost.
                            let mana_type = runner
                                .state
                                .objects
                                .get(&creature)
                                .and_then(|obj| obj.color.first().copied())
                                .map(ManaType::from)
                                .unwrap_or(ManaType::Colorless);
                            act_collect(
                                runner,
                                GameAction::TapForConvoke {
                                    object_id: creature,
                                    mana_type,
                                },
                                &mut events,
                            )?;
                        }
                    }
                    // CR 601.2h: finalize the (now fully convoke-paid) cost.
                    act_collect(runner, GameAction::PassPriority, &mut events)?;
                }
                // CR 601.2c: declare one target per slot, in written order.
                WaitingFor::TargetSelection {
                    target_slots,
                    selection,
                    ..
                } => {
                    let slot = &target_slots[selection.current_slot];
                    let choice = pick_slot_target(
                        slot,
                        &mut remaining_objects,
                        &declared_players,
                        selection.current_slot,
                    );
                    act_collect(
                        runner,
                        GameAction::ChooseTarget { target: choice },
                        &mut events,
                    )?;
                }
                // CR 601.2a: spell is on the stack — capture the hand baseline.
                WaitingFor::Priority { .. } => {
                    hand_at_commit = Some(
                        runner
                            .state
                            .players
                            .iter()
                            .map(|p| (p.id, p.hand.len()))
                            .collect(),
                    );
                    break;
                }
                other => panic!(
                    "SpellCast driver does not handle WaitingFor::{} yet — extend the driver \
                     or drive this case manually",
                    waiting_for_variant_name(other)
                ),
            }
        }

        let hand_baseline = hand_at_commit.unwrap_or_else(|| {
            panic!(
                "SpellCast never reached a Priority window after committing the cast \
                 (loop cap exceeded) — the spell did not commit to the stack"
            )
        });

        Ok(CastCommit {
            runner,
            hand_baseline,
            life_before,
            remaining_objects,
            declared_players,
            selected_casting_variant,
            events,
            distribution,
            optional,
            search_pick,
            replacement_choice,
            named_choice,
            discard_cards,
            effect_zone_cards,
            copy_target,
            spellbook_pick,
        })
    }

    /// Drive the full cast pipeline to its conclusion and return the outcome.
    ///
    /// Panics with a clear, extend-me message on any pipeline state the driver
    /// is not yet taught to handle, or when a declared intent cannot be matched
    /// to a required slot. A panic here is a *test-harness* signal: extend the
    /// driver or drive the case manually — never assert around a silent skip.
    pub fn resolve(self) -> CastOutcome {
        self.commit().resolve()
    }

    /// Drive the full cast pipeline and return engine rejection instead of
    /// panicking when the reducer rejects a step.
    pub fn try_resolve(self) -> Result<CastOutcome, EngineError> {
        self.try_commit()?.try_resolve()
    }
}

/// A spell committed to the stack, before resolution starts.
pub struct CastCommit<'a> {
    runner: &'a mut GameRunner,
    hand_baseline: Vec<(PlayerId, usize)>,
    life_before: Vec<(PlayerId, i32)>,
    remaining_objects: Vec<ObjectId>,
    declared_players: Vec<PlayerId>,
    selected_casting_variant: Option<CastingVariantChoiceOption>,
    events: Vec<GameEvent>,
    distribution: Option<Vec<(TargetRef, u32)>>,
    optional: OptionalPolicy,
    search_pick: SearchPolicy,
    replacement_choice: Option<usize>,
    named_choice: Option<String>,
    discard_cards: Vec<ObjectId>,
    effect_zone_cards: Vec<ObjectId>,
    copy_target: Option<ObjectId>,
    spellbook_pick: Option<String>,
}

impl<'a> CastCommit<'a> {
    /// Read the current pre-resolution state.
    pub fn state(&self) -> &GameState {
        &self.runner.state
    }

    /// Mutate the board WHILE the committed spell is still on the stack.
    ///
    /// The spell has been announced (CR 601.2a-i) but not resolved (CR 608.2), which
    /// is the only window in which "did this value LOCK at announcement, or is it
    /// re-read at resolution?" is answerable. A test that merely casts and resolves
    /// against a static board cannot tell a locked snapshot from a live re-read —
    /// both produce the same number. Changing the board here and then resolving is
    /// what discriminates them.
    pub fn state_mut(&mut self) -> &mut GameState {
        &mut self.runner.state
    }

    /// The cast variant option selected during `CastingVariantChoice`, if the
    /// cast surfaced that prompt.
    pub fn selected_casting_variant(&self) -> Option<&CastingVariantChoiceOption> {
        self.selected_casting_variant.as_ref()
    }

    /// Resolve the committed spell and return the usual behavior delta.
    pub fn resolve(self) -> CastOutcome {
        self.try_resolve()
            .expect("SpellCast resolution must be accepted by the engine")
    }

    /// Resolve the committed spell, returning reducer errors instead of
    /// panicking.
    pub fn try_resolve(self) -> Result<CastOutcome, EngineError> {
        let CastCommit {
            runner,
            hand_baseline,
            life_before,
            remaining_objects,
            declared_players,
            mut events,
            distribution,
            optional,
            search_pick,
            replacement_choice,
            named_choice,
            discard_cards,
            effect_zone_cards,
            copy_target,
            spellbook_pick,
            ..
        } = self;

        // Resolution. The shared driver auto-answers ordering/scry/trigger-
        // target/multi-target/optional prompts from the declared intent and
        // stops at any prompt it is not taught to answer (e.g. a `SearchChoice`
        // fail-to-find boundary) so the caller can assert on it via
        // `final_waiting_for()`. Preserves the original `#2051` behavior: object
        // targets are reused (declared cast-time targets may also satisfy a
        // resolution-time trigger slot), and the default search/optional
        // policies are `Stop`/`Decline`.
        let policy = ResolutionPolicy {
            targets_objects: remaining_objects,
            targets_players: declared_players,
            distribution,
            optional,
            search_pick,
            replacement_choice,
            named_choice,
            discard_cards,
            effect_zone_cards,
            copy_target,
            spellbook_pick,
        };
        events.extend(drive_resolution(runner, &policy)?);

        Ok(Outcome {
            state: runner.state.clone(),
            events,
            hand_baseline,
            life_before,
        })
    }
}

/// Pick the `ChooseTarget` payload for a single slot from declared intent,
/// matching CR 601.2c (targets declared one per slot, in written order).
///
/// Object intent is *consumed* (each declared object satisfies at most one
/// slot, so distinct exile/destroy targets never alias). Player intent is
/// *reusable* — the same player is routinely targeted by several modes of one
/// modal spell (e.g. Kozilek's Command mode 1 scries *and* draws for the same
/// target player), so a declared player may satisfy multiple player slots.
/// Falls back to `None` for optional slots; panics for an unsatisfiable
/// required slot.
fn pick_slot_target(
    slot: &crate::types::game_state::TargetSelectionSlot,
    remaining_objects: &mut Vec<ObjectId>,
    declared_players: &[PlayerId],
    slot_index: usize,
) -> Option<TargetRef> {
    if let Some(pos) = remaining_objects
        .iter()
        .position(|&o| slot.legal_targets.contains(&TargetRef::Object(o)))
    {
        return Some(TargetRef::Object(remaining_objects.remove(pos)));
    }
    if let Some(&player) = declared_players
        .iter()
        .find(|&&p| slot.legal_targets.contains(&TargetRef::Player(p)))
    {
        return Some(TargetRef::Player(player));
    }
    if slot.optional {
        return None;
    }
    panic!(
        "SpellCast could not satisfy required target slot {slot_index}: no declared target \
         matches its legal set.\n  legal_targets: {:?}\n  remaining declared objects: {:?}\n  \
         declared players: {:?}",
        slot.legal_targets, remaining_objects, declared_players
    );
}

/// Stable variant name for the extend-me panic messages (mirrors the engine's
/// own discriminant naming so failures point at the exact unhandled prompt).
fn waiting_for_variant_name(waiting: &WaitingFor) -> &'static str {
    // Reuse the runner's authoritative mapping by wrapping in a throwaway
    // borrow-free match. Kept in sync with `GameRunner::waiting_for_kind`.
    match waiting {
        WaitingFor::ManaPayment { .. } => "ManaPayment",
        WaitingFor::ChooseXValue { .. } => "ChooseXValue",
        WaitingFor::TargetSelection { .. } => "TargetSelection",
        WaitingFor::MultiTargetSelection { .. } => "MultiTargetSelection",
        WaitingFor::TriggerTargetSelection { .. } => "TriggerTargetSelection",
        WaitingFor::ModeChoice { .. } => "ModeChoice",
        WaitingFor::AbilityModeChoice { .. } => "AbilityModeChoice",
        WaitingFor::Priority { .. } => "Priority",
        WaitingFor::OrderTriggers { .. } => "OrderTriggers",
        WaitingFor::ScryChoice { .. } => "ScryChoice",
        WaitingFor::ArrangePlanarDeckTopChoice { .. } => "ArrangePlanarDeckTopChoice",
        WaitingFor::SearchChoice { .. } => "SearchChoice",
        WaitingFor::OptionalCostChoice { .. } => "OptionalCostChoice",
        WaitingFor::CastOffer { .. } => "CastOffer",
        WaitingFor::ModalFaceChoice { .. } => "ModalFaceChoice",
        WaitingFor::AlternativeCastChoice { .. } => "AlternativeCastChoice",
        WaitingFor::CastingVariantChoice { .. } => "CastingVariantChoice",
        WaitingFor::PayCost { .. } => "PayCost",
        WaitingFor::DistributeAmong { .. } => "DistributeAmong",
        WaitingFor::SurveilChoice { .. } => "SurveilChoice",
        WaitingFor::RedistributeLifeTotals { .. } => "RedistributeLifeTotals",
        WaitingFor::CoinFlipKeepChoice { .. } => "CoinFlipKeepChoice",
        WaitingFor::ReplacementChoice { .. } => "ReplacementChoice",
        WaitingFor::NamedChoice { .. } => "NamedChoice",
        WaitingFor::TributeChoice { .. } => "TributeChoice",
        WaitingFor::DiscardChoice { .. } => "DiscardChoice",
        WaitingFor::EffectZoneChoice { .. } => "EffectZoneChoice",
        WaitingFor::CopyTargetChoice { .. } => "CopyTargetChoice",
        WaitingFor::CopyRetarget { .. } => "CopyRetarget",
        WaitingFor::GameOver { .. } => "GameOver",
        _ => "<other>",
    }
}

fn act_collect(
    runner: &mut GameRunner,
    action: GameAction,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let result = runner.act(action)?;
    events.extend(result.events);
    Ok(())
}

fn pick_declared_cards(
    legal: &[ObjectId],
    min_count: usize,
    count: usize,
    declared: &mut Vec<ObjectId>,
    prompt: &str,
) -> Vec<ObjectId> {
    let selected: Vec<ObjectId> = declared
        .iter()
        .filter(|id| legal.contains(id))
        .take(count)
        .copied()
        .collect();
    assert!(
        selected.len() >= min_count,
        "{prompt} needs at least {min_count} declared legal object(s), found {}.\n  legal: \
         {legal:?}\n  declared: {declared:?}",
        selected.len()
    );
    declared.retain(|id| !selected.contains(id));
    selected
}

fn default_distribution(total: u32, targets: &[TargetRef]) -> Vec<(TargetRef, u32)> {
    if total == 0 || targets.is_empty() {
        return Vec::new();
    }

    let chosen_len = targets.len().min(total as usize);
    let mut distribution: Vec<_> = targets
        .iter()
        .take(chosen_len)
        .cloned()
        .map(|target| (target, 1))
        .collect();
    let assigned = distribution.len() as u32;
    if assigned < total {
        if let Some((_, amount)) = distribution.last_mut() {
            *amount += total - assigned;
        }
    }
    distribution
}

// ---------------------------------------------------------------------------
// AbilityActivation (fluent activated-ability driver — H1)
// ---------------------------------------------------------------------------

/// Fluent builder that drives an activated ability through the activation
/// pipeline (CR 602). Constructed via [`GameRunner::activate`].
///
/// Mirrors [`SpellCast`]: the caller declares *intent* (X value, chosen modes,
/// targets) and the driver answers each pipeline prompt itself — X announcement
/// (CR 601.2f via CR 602.2b), modal choice (CR 602.2b / CR 700.2), one target
/// per slot in written order (CR 601.2c), and the cost window — then drives
/// resolution via the shared [`drive_resolution`] with a [`ResolutionPolicy`]
/// built from the declared targets and policy setters.
///
/// Like the cast driver, it panics with an extend-me message on any pipeline
/// state it is not yet taught to handle, or when a declared intent cannot be
/// matched to a required slot.
pub struct AbilityActivation<'a> {
    runner: &'a mut GameRunner,
    source: ObjectId,
    ability_index: usize,
    modes: Option<Vec<usize>>,
    x: Option<u32>,
    target_players: Vec<PlayerId>,
    target_objects: Vec<ObjectId>,
    /// Mana sources the caller intends to tap to pay the activation cost. See
    /// the blind-authoring caveat: auto-tap is NOT performed by the driver; a
    /// surfaced `ManaPayment` falls through to the resolution driver's stop, so
    /// this field is recorded for forward-compatibility and the caller drives
    /// payment manually if a `ManaPayment` window opens.
    pay_with: Vec<ObjectId>,
    search_pick: SearchPolicy,
    optional: OptionalPolicy,
    spellbook_pick: Option<String>,
}

impl<'a> AbilityActivation<'a> {
    fn new(runner: &'a mut GameRunner, source: ObjectId, ability_index: usize) -> Self {
        AbilityActivation {
            runner,
            source,
            ability_index,
            modes: None,
            x: None,
            target_players: Vec::new(),
            target_objects: Vec::new(),
            pay_with: Vec::new(),
            search_pick: SearchPolicy::default(),
            optional: OptionalPolicy::default(),
            spellbook_pick: None,
        }
    }

    /// Announce the value of X for an X-cost ability (CR 601.2f / CR 602.2b).
    pub fn x(mut self, value: u32) -> Self {
        self.x = Some(value);
        self
    }

    /// Declare the chosen mode indices for a modal activated ability
    /// (CR 602.2b / CR 700.2) — answers `AbilityModeChoice`.
    pub fn modes(mut self, modes: &[usize]) -> Self {
        self.modes = Some(modes.to_vec());
        self
    }

    /// Declare a player as an intended target (CR 601.2c).
    pub fn target_player(mut self, player: PlayerId) -> Self {
        self.target_players.push(player);
        self
    }

    /// Declare several players as intended targets, in order.
    pub fn target_players(mut self, players: &[PlayerId]) -> Self {
        self.target_players.extend_from_slice(players);
        self
    }

    /// Declare an object as an intended target (CR 601.2c).
    pub fn target_object(mut self, object: ObjectId) -> Self {
        self.target_objects.push(object);
        self
    }

    /// Declare several objects as intended targets, in order.
    pub fn target_objects(mut self, objects: &[ObjectId]) -> Self {
        self.target_objects.extend_from_slice(objects);
        self
    }

    /// Record mana sources to tap for the activation cost (CR 602.2b / CR
    /// 601.2g). NOTE: auto-tap is not modeled — see the type-level caveat.
    pub fn pay_with(mut self, sources: &[ObjectId]) -> Self {
        self.pay_with.extend_from_slice(sources);
        self
    }

    /// Submit the first legal candidates at any `SearchChoice` during
    /// resolution (CR 701.23).
    pub fn search_first_legal(mut self) -> Self {
        self.search_pick = SearchPolicy::FirstLegal;
        self
    }

    /// Decline optional ("you may") effects/costs during resolution
    /// (CR 608.2d / CR 601.2f). This is already the default; provided for
    /// explicitness at call sites.
    pub fn decline_optional(mut self) -> Self {
        self.optional = OptionalPolicy::Decline;
        self
    }

    /// Accept optional ("you may") effects/costs during resolution
    /// (CR 608.2d / CR 601.2f). Mirrors [`SpellCast`]'s decline default with an
    /// opt-in accept, for abilities whose payoff is gated behind a "you may".
    pub fn accept_optional(mut self) -> Self {
        self.optional = OptionalPolicy::Accept;
        self
    }

    /// Draft this card name at any `SpellbookDraft` prompt during resolution
    /// (Alchemy `Effect::DraftFromSpellbook`). Mirrors [`SpellCast::spellbook_pick`].
    pub fn spellbook_pick(mut self, name: &str) -> Self {
        self.spellbook_pick = Some(name.to_string());
        self
    }

    /// Drive the full activation pipeline to its conclusion and return the
    /// outcome. See [`SpellCast::resolve`] for the shared contract.
    pub fn resolve(self) -> Outcome {
        let AbilityActivation {
            runner,
            source,
            ability_index,
            modes,
            x,
            target_players,
            target_objects,
            pay_with,
            search_pick,
            optional,
            spellbook_pick,
        } = self;

        // CR 119.3: snapshot life totals before activation for `life_delta`.
        let life_before: Vec<(PlayerId, i32)> = runner
            .state
            .players
            .iter()
            .map(|p| (p.id, p.life))
            .collect();

        // CR 602.2a: announce the activation. The engine routes through the
        // same announcement-to-payment steps as casting (CR 602.2b).
        let mut events = Vec::new();
        act_collect(
            runner,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index,
            },
            &mut events,
        )
        .expect("ActivateAbility must be accepted by the engine");

        let mut remaining_objects: Vec<ObjectId> = target_objects;
        let declared_players: Vec<PlayerId> = target_players;

        // CR 602.2b: the ability is on the stack at the post-announcement
        // Priority window — capture the hand baseline there (mirrors SpellCast).
        let mut hand_at_commit: Option<Vec<(PlayerId, usize)>> = None;

        for _ in 0..64 {
            match &runner.state.waiting_for {
                // CR 602.2b / CR 700.2: modal activated ability mode choice.
                WaitingFor::AbilityModeChoice { .. } => {
                    let indices = modes.clone().unwrap_or_else(|| {
                        panic!(
                            "AbilityActivation reached WaitingFor::AbilityModeChoice but no \
                             .modes(..) were declared — declare its chosen mode indices"
                        )
                    });
                    act_collect(runner, GameAction::SelectModes { indices }, &mut events)
                        .expect("SelectModes (ability mode) must be accepted");
                }
                // CR 601.2f / CR 602.2b: announce X.
                WaitingFor::ChooseXValue { .. } => {
                    let value = x.unwrap_or_else(|| {
                        panic!(
                            "AbilityActivation reached WaitingFor::ChooseXValue but no .x(..) \
                             was declared — this ability needs X announced"
                        )
                    });
                    act_collect(runner, GameAction::ChooseX { value }, &mut events)
                        .expect("ChooseX must be accepted");
                }
                // CR 601.2c: declare one target per slot, in written order.
                WaitingFor::TargetSelection {
                    target_slots,
                    selection,
                    ..
                } => {
                    let slot = &target_slots[selection.current_slot];
                    let choice = pick_slot_target(
                        slot,
                        &mut remaining_objects,
                        &declared_players,
                        selection.current_slot,
                    );
                    act_collect(
                        runner,
                        GameAction::ChooseTarget { target: choice },
                        &mut events,
                    )
                    .expect("ChooseTarget must be accepted");
                }
                // CR 602.2b: ability is on the stack — capture the baseline.
                WaitingFor::Priority { .. } => {
                    hand_at_commit = Some(
                        runner
                            .state
                            .players
                            .iter()
                            .map(|p| (p.id, p.hand.len()))
                            .collect(),
                    );
                    break;
                }
                // CR 602.1b: pay the ability's mana cost. Finalize from the pool
                // via PassPriority (mirrors SpellCast). Source auto-tap is not
                // modeled, so fund the pool with GameScenario::with_mana_pool; if
                // it can't cover the cost, PassPriority errors and the `.expect`
                // below fails loudly.
                WaitingFor::ManaPayment { .. } => {
                    act_collect(runner, GameAction::PassPriority, &mut events)
                        .expect("finalizing the ability's mana payment must be accepted");
                }
                // CR 602.2b + CR 601.2h: pay a non-mana cost (e.g. sacrificing or
                // exiling a specific permanent named by the cost). Activating an
                // ability follows the 601.2 process (602.2b); 601.2h is the
                // object-moving cost-payment step. Submit the pre-declared
                // `.pay_with(..)` object(s).
                WaitingFor::PayCost { .. } => {
                    if pay_with.is_empty() {
                        panic!(
                            "AbilityActivation reached WaitingFor::PayCost but no .pay_with(..) \
                             objects were declared — declare the object(s) that pay this cost"
                        );
                    }
                    runner
                        .act(GameAction::SelectCards {
                            cards: pay_with.clone(),
                        })
                        .expect("SelectCards (cost payment) must be accepted");
                }
                other => panic!(
                    "AbilityActivation driver does not handle WaitingFor::{} yet — extend the \
                     driver or drive this case manually",
                    waiting_for_variant_name(other)
                ),
            }
        }

        let hand_baseline = hand_at_commit.unwrap_or_else(|| {
            panic!(
                "AbilityActivation never reached a Priority window after announcing the ability \
                 (loop cap exceeded) — the ability did not commit to the stack"
            )
        });

        // Resolution via the shared driver with the declared policy.
        let policy = ResolutionPolicy {
            targets_objects: remaining_objects,
            targets_players: declared_players,
            distribution: None,
            search_pick,
            optional,
            replacement_choice: None,
            named_choice: None,
            discard_cards: Vec::new(),
            effect_zone_cards: Vec::new(),
            copy_target: None,
            spellbook_pick,
        };
        events.extend(
            drive_resolution(runner, &policy).expect("ability resolution must be accepted"),
        );

        Outcome {
            state: runner.state.clone(),
            events,
            hand_baseline,
            life_before,
        }
    }
}

// ---------------------------------------------------------------------------
// ResolutionPolicy + drive_resolution (shared resolution driver — H3)
// ---------------------------------------------------------------------------

/// What the resolution driver does when it reaches a `SearchChoice`
/// (CR 701.23 — Search) prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchPolicy {
    /// Submit the first `count` legal candidates (a deterministic tutor).
    FirstLegal,
    /// Submit an empty selection — fail to find (CR 701.23d permits finding
    /// nothing for an "up to" search; for a mandatory search this exercises the
    /// no-legal-card path).
    None,
    /// Leave the prompt for the caller to inspect via `final_waiting_for()`.
    /// This is the default — it preserves the original `#2051` driver behavior
    /// of stopping at any unhandled search boundary.
    #[default]
    Stop,
}

/// What the resolution driver does at an optional "you may" decision
/// (`OptionalEffectChoice` per CR 608.2d, `OptionalCostChoice` per CR 601.2f).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptionalPolicy {
    /// Accept the optional effect / pay the optional cost.
    Accept,
    /// Decline — the safe default, since many "you may" prompts default to no.
    #[default]
    Decline,
}

/// Declared intent reused across resolution prompts. Built once per
/// resolve-call and threaded through [`drive_resolution`] so the driver answers
/// trigger-target / multi-target / search / optional prompts from the same
/// declared set, mirroring the [`SpellCast`] slot-matching discipline.
#[derive(Debug, Clone, Default)]
pub struct ResolutionPolicy {
    /// Object targets the driver may assign to a slot, consumed one per slot
    /// (CR 601.2c — each declared object satisfies at most one slot).
    pub targets_objects: Vec<ObjectId>,
    /// Player targets the driver may assign, reusable across slots (one player
    /// may be targeted by several modes — see [`pick_slot_target`]).
    pub targets_players: Vec<PlayerId>,
    /// Explicit distribution for `DistributeAmong`; otherwise the driver uses a
    /// deterministic legal split.
    pub distribution: Option<Vec<(TargetRef, u32)>>,
    /// How to answer a `SearchChoice` (CR 701.23). Defaults to `Stop`.
    pub search_pick: SearchPolicy,
    /// How to answer an optional effect / cost prompt. Defaults to `Decline`.
    pub optional: OptionalPolicy,
    /// Replacement candidate index to choose at `ReplacementChoice` (CR 616.1).
    pub replacement_choice: Option<usize>,
    /// Option label/name to choose at `NamedChoice`.
    pub named_choice: Option<String>,
    /// Cards to submit at `DiscardChoice`.
    pub discard_cards: Vec<ObjectId>,
    /// Objects to submit at `EffectZoneChoice`.
    pub effect_zone_cards: Vec<ObjectId>,
    /// Permanent to choose at `CopyTargetChoice`.
    pub copy_target: Option<ObjectId>,
    /// Card name to draft at a `SpellbookDraft` prompt (Alchemy
    /// `Effect::DraftFromSpellbook`). `None` halts the driver so tests without a
    /// pick can inspect the offered `options` via `final_waiting_for()`.
    pub spellbook_pick: Option<String>,
}

/// Drive the engine through resolution, answering the prompts the harness knows
/// how to answer from a declared [`ResolutionPolicy`], and stopping at anything
/// it is not taught to handle so the caller can inspect it via the final
/// waiting state.
///
/// This is the shared loop extracted from the original `#2051` cast driver so
/// [`SpellCast::resolve`], [`AbilityActivation::resolve`], and combat helpers
/// all use one resolution policy. It panics with an extend-me message only on
/// an *unsatisfiable required slot* — any other unhandled prompt simply breaks
/// the loop, leaving the state for the caller.
fn drive_resolution(
    runner: &mut GameRunner,
    policy: &ResolutionPolicy,
) -> Result<Vec<GameEvent>, EngineError> {
    // Object intent is consumed per slot; player intent is reusable. Mirrors
    // the SpellCast cast-time loop.
    let mut remaining_objects: Vec<ObjectId> = policy.targets_objects.clone();
    let declared_players: &[PlayerId] = &policy.targets_players;
    let mut discard_cards = policy.discard_cards.clone();
    let mut effect_zone_cards = policy.effect_zone_cards.clone();
    let mut events = Vec::new();

    for _ in 0..64 {
        match &runner.state.waiting_for {
            // CR 603.3b: drain the per-controller ordering prompt.
            WaitingFor::OrderTriggers { .. } => {
                super::triggers::drain_order_triggers_with_identity(&mut runner.state);
            }
            // CR 701.22a: default scry policy keeps the looked-at cards on top.
            WaitingFor::ScryChoice { cards, .. } => {
                let cards = cards.clone();
                act_collect(runner, GameAction::SelectCards { cards }, &mut events)?;
            }
            WaitingFor::ArrangePlanarDeckTopChoice {
                cards, keep_on_top, ..
            } => {
                let keep: Vec<_> = cards.iter().take(*keep_on_top).copied().collect();
                act_collect(runner, GameAction::SelectCards { cards: keep }, &mut events)?;
            }
            // CR 701.25a: default surveil policy keeps all looked-at cards on
            // top, mirroring the scry default.
            WaitingFor::SurveilChoice { cards, .. } => {
                let cards = cards.clone();
                act_collect(runner, GameAction::SelectCards { cards }, &mut events)?;
            }
            // CR 705.1 + CR 614.1a: with replacement-created multiple flip
            // results, keep the first required result deterministically.
            WaitingFor::CoinFlipKeepChoice { keep_count, .. } => {
                let keep_indices = (0..*keep_count).collect();
                act_collect(
                    runner,
                    GameAction::SelectCoinFlips { keep_indices },
                    &mut events,
                )?;
            }
            // CR 603.3d: a triggered ability declares one target per slot, in
            // written order — identical mechanics to cast-time TargetSelection.
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = pick_slot_target(
                    slot,
                    &mut remaining_objects,
                    declared_players,
                    selection.current_slot,
                );
                act_collect(
                    runner,
                    GameAction::ChooseTarget { target: choice },
                    &mut events,
                )?;
            }
            // CR 608.2c: Some resolving spell abilities choose targets during
            // resolution. Reuse the same slot-matching policy as cast-time
            // targeting so tests can declare the intended object/player once.
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = pick_slot_target(
                    slot,
                    &mut remaining_objects,
                    declared_players,
                    selection.current_slot,
                );
                act_collect(
                    runner,
                    GameAction::ChooseTarget { target: choice },
                    &mut events,
                )?;
            }
            // CR 601.2c: a variable-count multi-target set is submitted as one
            // SelectCards of the declared object targets that are legal here.
            WaitingFor::MultiTargetSelection {
                legal_targets,
                min_targets,
                max_targets,
                ..
            } => {
                let legal_targets = legal_targets.clone();
                let min = *min_targets;
                let max = *max_targets;
                let chosen: Vec<ObjectId> = remaining_objects
                    .iter()
                    .filter(|o| legal_targets.contains(o))
                    .take(max)
                    .copied()
                    .collect();
                assert!(
                    chosen.len() >= min,
                    "MultiTargetSelection needs at least {min} declared object targets in its \
                     legal set, found {} — declare more via ResolutionPolicy.targets_objects.\n  \
                     legal: {legal_targets:?}\n  declared: {remaining_objects:?}",
                    chosen.len()
                );
                remaining_objects.retain(|o| !chosen.contains(o));
                act_collect(
                    runner,
                    GameAction::SelectCards { cards: chosen },
                    &mut events,
                )?;
            }
            WaitingFor::DistributeAmong { total, targets, .. } => {
                let distribution = policy
                    .distribution
                    .clone()
                    .unwrap_or_else(|| default_distribution(*total, targets));
                act_collect(
                    runner,
                    GameAction::DistributeAmong { distribution },
                    &mut events,
                )?;
            }
            // CR 701.23: search the library per the declared search policy.
            WaitingFor::SearchChoice { cards, count, .. } => match policy.search_pick {
                SearchPolicy::Stop => break,
                SearchPolicy::None => {
                    act_collect(
                        runner,
                        GameAction::SelectCards { cards: vec![] },
                        &mut events,
                    )?;
                }
                SearchPolicy::FirstLegal => {
                    let picked: Vec<ObjectId> = cards.iter().take(*count).copied().collect();
                    act_collect(
                        runner,
                        GameAction::SelectCards { cards: picked },
                        &mut events,
                    )?;
                }
            },
            // CR 608.2d: accept or decline an optional ("you may") effect.
            WaitingFor::OptionalEffectChoice { .. } => {
                let accept = matches!(policy.optional, OptionalPolicy::Accept);
                act_collect(
                    runner,
                    GameAction::DecideOptionalEffect { accept },
                    &mut events,
                )?;
            }
            WaitingFor::TributeChoice { .. } => {
                let accept = matches!(policy.optional, OptionalPolicy::Accept);
                act_collect(
                    runner,
                    GameAction::DecideOptionalEffect { accept },
                    &mut events,
                )?;
            }
            // CR 601.2f: pay or decline an optional cost during resolution.
            WaitingFor::OptionalCostChoice { .. } => {
                let pay = matches!(policy.optional, OptionalPolicy::Accept);
                act_collect(runner, GameAction::DecideOptionalCost { pay }, &mut events)?;
            }
            WaitingFor::ReplacementChoice { .. } => {
                let Some(index) = policy.replacement_choice else {
                    break;
                };
                act_collect(runner, GameAction::ChooseReplacement { index }, &mut events)?;
            }
            WaitingFor::NamedChoice { .. } => {
                let Some(choice) = policy.named_choice.clone() else {
                    break;
                };
                act_collect(runner, GameAction::ChooseOption { choice }, &mut events)?;
            }
            // Alchemy `Effect::DraftFromSpellbook`: draft the declared card name
            // from the drafting source's spellbook `options`. No pick declared →
            // halt so the caller can assert the offered options and the draft
            // boundary via `final_waiting_for()`.
            WaitingFor::SpellbookDraft { options, .. } => {
                let Some(card) = policy.spellbook_pick.clone() else {
                    break;
                };
                debug_assert!(
                    options.contains(&card),
                    "spellbook_pick {card:?} is not in the drafting source's spellbook {options:?}"
                );
                act_collect(
                    runner,
                    GameAction::SubmitSpellbookDraft { card },
                    &mut events,
                )?;
            }
            WaitingFor::DiscardChoice {
                cards,
                count,
                up_to,
                ..
            } => {
                if discard_cards.is_empty() {
                    break;
                }
                let min = if *up_to { 0 } else { *count };
                let chosen =
                    pick_declared_cards(cards, min, *count, &mut discard_cards, "DiscardChoice");
                act_collect(
                    runner,
                    GameAction::SelectCards { cards: chosen },
                    &mut events,
                )?;
            }
            WaitingFor::EffectZoneChoice {
                cards,
                count,
                min_count,
                ..
            } => {
                if effect_zone_cards.is_empty() {
                    break;
                }
                let chosen = pick_declared_cards(
                    cards,
                    *min_count,
                    *count,
                    &mut effect_zone_cards,
                    "EffectZoneChoice",
                );
                act_collect(
                    runner,
                    GameAction::SelectCards { cards: chosen },
                    &mut events,
                )?;
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                let Some(target) = policy.copy_target else {
                    break;
                };
                assert!(
                    valid_targets.contains(&target),
                    "CopyTargetChoice target {target:?} is not in legal set {valid_targets:?}"
                );
                act_collect(
                    runner,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    },
                    &mut events,
                )?;
            }
            // CR 605.3b + CR 608.2d: complete a mana-color choice during effect
            // resolution (Vexing Puzzlebox: add one mana of any color, then roll
            // a d20). Default policy picks the first legal option deterministically.
            WaitingFor::ChooseManaColor { choice: prompt, .. } => {
                let mana_choice = match prompt {
                    ManaChoicePrompt::SingleColor { options } => {
                        ManaChoice::SingleColor(*options.first().unwrap_or(&ManaType::Colorless))
                    }
                    ManaChoicePrompt::AnyCombination { count, options } => {
                        let color = *options.first().unwrap_or(&ManaType::Colorless);
                        ManaChoice::Combination(vec![color; *count])
                    }
                    ManaChoicePrompt::Combination { options } => {
                        ManaChoice::Combination(options.first().cloned().unwrap_or_default())
                    }
                };
                act_collect(
                    runner,
                    GameAction::ChooseManaColor {
                        choice: mana_choice,
                        count: 1,
                    },
                    &mut events,
                )?;
            }
            WaitingFor::Priority { .. } => {
                if runner.state.stack.is_empty() {
                    break;
                }
                match runner.act(GameAction::PassPriority) {
                    Ok(result) => events.extend(result.events),
                    Err(_) => {
                        break;
                    }
                }
            }
            // Any other prompt (e.g. a non-convoke ManaPayment, or a choice the
            // driver does not model) is left for the caller to inspect via
            // `final_waiting_for()`; the driver stops here. ManaPayment auto-tap
            // is intentionally NOT modeled — a blind tap loop is fragile (see
            // report) — so a surfaced ManaPayment falls through to this break.
            _ => break,
        }
    }
    Ok(events)
}

// ---------------------------------------------------------------------------
// Outcome (behavior/semantic delta accessors)
// ---------------------------------------------------------------------------

/// Post-cast snapshot exposing behavior/semantic deltas — never AST-internal
/// flags. Produced by [`SpellCast::resolve`], [`AbilityActivation::resolve`],
/// and [`GameRunner::combat_damage`].
pub struct Outcome {
    state: GameState,
    events: Vec<GameEvent>,
    /// Per-player hand sizes captured at stack commit (CR 601.2a) — the clean
    /// baseline for resolution-draw deltas (foot-gun 3 fix).
    hand_baseline: Vec<(PlayerId, usize)>,
    /// Per-player life totals captured before the cast (CR 119.3).
    life_before: Vec<(PlayerId, i32)>,
}

/// Backwards-compatible alias: the original `#2051` cast driver named this type
/// `CastOutcome`. Kept so existing tests compile unchanged while new harness
/// surfaces (activation, combat) share the single [`Outcome`] accessor set.
pub type CastOutcome = Outcome;

impl Outcome {
    fn hand_baseline_for(&self, player: PlayerId) -> usize {
        self.hand_baseline
            .iter()
            .find(|(p, _)| *p == player)
            .map(|(_, n)| *n)
            .expect("player must have a hand baseline")
    }

    fn life_before_for(&self, player: PlayerId) -> i32 {
        self.life_before
            .iter()
            .find(|(p, _)| *p == player)
            .map(|(_, l)| *l)
            .expect("player must have a pre-cast life snapshot")
    }

    fn current_hand(&self, player: PlayerId) -> usize {
        self.state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.hand.len())
            .expect("player must exist")
    }

    fn current_life(&self, player: PlayerId) -> i32 {
        self.state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.life)
            .expect("player must exist")
    }

    /// Net cards drawn during resolution: final hand minus the stack-commit
    /// baseline (CR 601.2a). Positive = net draw, negative = net discard.
    pub fn hand_drawn(&self, player: PlayerId) -> i64 {
        self.current_hand(player) as i64 - self.hand_baseline_for(player) as i64
    }

    /// The zone an object currently occupies (CR 400.1).
    pub fn zone_of(&self, object: ObjectId) -> Zone {
        self.state.objects[&object].zone
    }

    /// Net life change for a player: final minus pre-cast (CR 119.3).
    pub fn life_delta(&self, player: PlayerId) -> i32 {
        self.current_life(player) - self.life_before_for(player)
    }

    /// The waiting state the pipeline halted in (e.g. `Priority` for a clean
    /// resolve, or a `SearchChoice` the driver left for the caller to inspect).
    pub fn final_waiting_for(&self) -> &WaitingFor {
        &self.state.waiting_for
    }

    /// Engine events emitted by every action the harness drove.
    pub fn events(&self) -> &[GameEvent] {
        &self.events
    }

    /// Read-only view of the final game state for assertions the typed
    /// accessors don't yet cover.
    pub fn state(&self) -> &GameState {
        &self.state
    }

    /// Find an object in the final state by predicate.
    pub fn find_object(&self, mut pred: impl FnMut(&GameObject) -> bool) -> Option<ObjectId> {
        self.state
            .objects
            .iter()
            .find_map(|(id, obj)| pred(obj).then_some(*id))
    }

    /// Assert net cards drawn since stack commit (foot-gun 3 fix).
    pub fn assert_hand_drawn(&self, player: PlayerId, expected: i64) {
        let actual = self.hand_drawn(player);
        assert_eq!(
            actual,
            expected,
            "P{} hand delta since stack commit: expected {expected}, got {actual} \
             (baseline {}, final {})",
            player.0,
            self.hand_baseline_for(player),
            self.current_hand(player)
        );
    }

    /// Assert every listed object now occupies `zone`.
    pub fn assert_zone(&self, objects: &[ObjectId], zone: Zone) {
        for &object in objects {
            let actual = self.zone_of(object);
            assert_eq!(
                actual, zone,
                "object {} expected in {zone:?}, found in {actual:?}",
                object.0
            );
        }
    }

    /// Assert a player's net life change since before the cast (CR 119.3).
    pub fn assert_life_delta(&self, player: PlayerId, expected: i32) {
        let actual = self.life_delta(player);
        assert_eq!(
            actual,
            expected,
            "P{} life delta: expected {expected}, got {actual} \
             (before {}, final {})",
            player.0,
            self.life_before_for(player),
            self.current_life(player)
        );
    }

    // -- H0 shared read accessors (pure reads off `self.state`) --

    /// Source names of the objects currently on the stack, bottom-to-top
    /// (CR 405.1 — the stack is the zone where spells and abilities wait to
    /// resolve). Mirrors [`GameRunner::stack_names`].
    pub fn stack_names(&self) -> Vec<String> {
        self.state
            .stack
            .iter()
            .filter_map(|entry| self.state.objects.get(&entry.source_id))
            .map(|obj| obj.name.clone())
            .collect()
    }

    /// Number of objects on the stack (CR 405.1).
    pub fn stack_size(&self) -> usize {
        self.state.stack.len()
    }

    /// Count of a specific counter kind on an object (CR 122.1), `0` if absent.
    pub fn counters(&self, obj: ObjectId, kind: CounterType) -> u32 {
        self.state
            .objects
            .get(&obj)
            .and_then(|o| o.counters.get(&kind).copied())
            .unwrap_or(0)
    }

    /// Total floating mana in a player's pool (CR 106.4 — mana held in the pool
    /// until spent or the step/phase ends).
    pub fn mana_pool_total(&self, player: PlayerId) -> u32 {
        self.state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.mana_pool.total() as u32)
            .unwrap_or(0)
    }

    /// Floating mana of a single color in a player's pool (CR 106.4).
    pub fn mana_pool_color(&self, player: PlayerId, color: ManaType) -> u32 {
        self.state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.mana_pool.count_color(color) as u32)
            .unwrap_or(0)
    }

    /// Whether a permanent is tapped (CR 110.5a — a permanent is either tapped
    /// or untapped). `false` if the object no longer exists.
    pub fn is_tapped(&self, obj: ObjectId) -> bool {
        self.state
            .objects
            .get(&obj)
            .map(|o| o.tapped)
            .unwrap_or(false)
    }

    /// Effective power and toughness of a permanent (CR 208 / CR 209).
    ///
    /// **Effective, not raw:** the layer system (`game::layers`) materializes
    /// the post-continuous-effect values back into `GameObject::power` /
    /// `GameObject::toughness` during `apply`, so reading those fields off the
    /// post-pipeline `self.state` yields the value after counters, Auras,
    /// pumps, and CDAs are applied — not the printed base (which lives in
    /// `base_power` / `base_toughness`). A creature with no P/T (e.g. a
    /// non-creature permanent) reports `(0, 0)`.
    pub fn power_toughness(&self, obj: ObjectId) -> (i32, i32) {
        self.state
            .objects
            .get(&obj)
            .map(|o| (o.power.unwrap_or(0), o.toughness.unwrap_or(0)))
            .unwrap_or((0, 0))
    }

    /// The player who currently controls an object (CR 109.4).
    pub fn controller(&self, obj: ObjectId) -> PlayerId {
        self.state.objects[&obj].controller
    }

    /// Damage marked on a permanent this turn (CR 120.3 — damage is marked on
    /// the creature/permanent; CR 514.2 — marked damage is removed during the
    /// cleanup step). `0` if the object no longer exists.
    pub fn damage_marked(&self, obj: ObjectId) -> u32 {
        self.state
            .objects
            .get(&obj)
            .map(|o| o.damage_marked)
            .unwrap_or(0)
    }

    /// Number of objects in a given zone for a player (CR 400.1).
    ///
    /// Battlefield and stack are shared zones (CR 403 / CR 405), so those filter
    /// the shared object lists by `controller`. Hand, library, and graveyard are
    /// per-player owned zones (CR 401 / CR 402 / CR 404) read directly off the
    /// `Player` Vecs. Exile and command (CR 406 / CR 408) are shared zones with
    /// no per-player Vec, so they are counted by scanning `objects` for the
    /// owner in that zone.
    pub fn zone_count(&self, player: PlayerId, zone: Zone) -> usize {
        match zone {
            Zone::Battlefield => self
                .state
                .battlefield
                .iter()
                .filter(|&&id| {
                    self.state
                        .objects
                        .get(&id)
                        .map(|o| o.controller == player)
                        .unwrap_or(false)
                })
                .count(),
            Zone::Stack => self
                .state
                .stack
                .iter()
                .filter(|entry| {
                    self.state
                        .objects
                        .get(&entry.source_id)
                        .map(|o| o.controller == player)
                        .unwrap_or(false)
                })
                .count(),
            Zone::Hand | Zone::Library | Zone::Graveyard => self
                .state
                .players
                .iter()
                .find(|p| p.id == player)
                .map(|p| match zone {
                    Zone::Hand => p.hand.len(),
                    Zone::Library => p.library.len(),
                    Zone::Graveyard => p.graveyard.len(),
                    _ => unreachable!("outer match restricts to per-player zones"),
                })
                .unwrap_or(0),
            // CR 406 / CR 408: exile and command have no per-player Vec — scan
            // the shared object map for objects this player owns in that zone.
            Zone::Exile | Zone::Command => self
                .state
                .objects
                .values()
                .filter(|o| o.zone == zone && o.owner == player)
                .count(),
        }
    }

    // -- H0 high-demand assertions (expected-vs-got, mirrors existing style) --

    /// Assert the count of a counter kind on an object (CR 122.1).
    pub fn assert_counters(&self, obj: ObjectId, kind: CounterType, expected: u32) {
        let actual = self.counters(obj, kind.clone());
        assert_eq!(
            actual, expected,
            "object {} {kind:?} counters: expected {expected}, got {actual}",
            obj.0
        );
    }

    /// Assert a permanent's tapped state (CR 110.5a).
    pub fn assert_tapped(&self, obj: ObjectId, expected: bool) {
        let actual = self.is_tapped(obj);
        assert_eq!(
            actual, expected,
            "object {} tapped: expected {expected}, got {actual}",
            obj.0
        );
    }

    /// Assert a permanent's effective power and toughness (CR 208 / CR 209).
    pub fn assert_power_toughness(&self, obj: ObjectId, power: i32, toughness: i32) {
        let (actual_p, actual_t) = self.power_toughness(obj);
        assert_eq!(
            (actual_p, actual_t),
            (power, toughness),
            "object {} effective P/T: expected {power}/{toughness}, got {actual_p}/{actual_t}",
            obj.0
        );
    }

    /// Assert that `player` controls `obj` (CR 109.4).
    pub fn assert_controls(&self, player: PlayerId, obj: ObjectId) {
        let actual = self.controller(obj);
        assert_eq!(
            actual, player,
            "object {} controller: expected P{}, got P{}",
            obj.0, player.0, actual.0
        );
    }

    /// Assert the number of objects a player has in `zone` (CR 400.1).
    pub fn assert_zone_count(&self, player: PlayerId, zone: Zone, expected: usize) {
        let actual = self.zone_count(player, zone);
        assert_eq!(
            actual, expected,
            "P{} {zone:?} count: expected {expected}, got {actual}",
            player.0
        );
    }

    /// Assert the number of objects on the stack (CR 405.1).
    pub fn assert_stack_size(&self, expected: usize) {
        let actual = self.stack_size();
        assert_eq!(
            actual,
            expected,
            "stack size: expected {expected}, got {actual} ({:?})",
            self.stack_names()
        );
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

    /// Cast a free (no-mana-cost) sorcery (already in the active player's hand)
    /// and commit it to the stack, returning the `WaitingFor` the engine halts
    /// in (the first resolution prompt). Helper for the "any player may"
    /// group-bargain tests that must drive the per-player APNAP prompt loop by
    /// hand.
    fn cast_free_sorcery_to_prompt(
        runner: &mut GameRunner,
        spell: ObjectId,
        preferred_target: Option<TargetRef>,
    ) -> WaitingFor {
        let card_id = runner.state.objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("CastSpell must be accepted");
        // CR 601.2a–c: answer any cast-time target slot (the "target player"
        // reward of Browbeat-class cards), then commit and begin resolving.
        for _ in 0..16 {
            match runner.state.waiting_for.clone() {
                WaitingFor::TargetSelection { target_slots, .. } => {
                    let slot = &target_slots[0];
                    let target = preferred_target
                        .as_ref()
                        .and_then(|preferred| {
                            slot.legal_targets
                                .iter()
                                .find(|candidate| *candidate == preferred)
                                .cloned()
                        })
                        .unwrap_or_else(|| slot.legal_targets[0].clone());
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(target),
                        })
                        .expect("cast-time target must be accepted");
                }
                WaitingFor::Priority { .. } => {
                    if runner.state.stack.is_empty() {
                        break;
                    }
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        runner.state.waiting_for.clone()
    }

    /// CR 608.2d + CR 101.4 (issue #3236): "any player may sacrifice a land of
    /// their choice. If a player does, …" must offer EVERY player INCLUDING the
    /// controller, in APNAP order (active player first), and the controller —
    /// when they accept — must be able to sacrifice their OWN land.
    ///
    /// This is the BLOCKER 1 regression guard: if `chunk_actor` stamped the
    /// sacrifice TargetFilter with `controller: Opponent`, the controller's own
    /// land would be filtered out and `legal_targets` would be empty.
    #[test]
    fn any_player_may_sacrifice_controller_accepts_own_land() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // P0 is the active player (controller). Give each player a land.
        let p0_land = scenario.add_basic_land(P0, ManaColor::Green);
        let _p1_land = scenario.add_basic_land(P1, ManaColor::White);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Group Bargain Test",
                false,
                "any player may sacrifice a land of their choice. \
                 if a player does, you draw a card",
            )
            .id();
        // A card to draw when the accept-consequence fires.
        scenario.with_library_top(P0, &["Reward Card"]);
        let mut runner = scenario.build();

        let wf = cast_free_sorcery_to_prompt(&mut runner, spell, None);

        // CR 101.4: the controller (active player P0) is offered FIRST, and the
        // other player remains queued.
        match wf {
            WaitingFor::OpponentMayChoice {
                player, remaining, ..
            } => {
                assert_eq!(
                    player, P0,
                    "controller (active player) must be offered first under AnyPlayer"
                );
                assert!(
                    remaining.contains(&P1),
                    "the non-controller must still be queued, got {remaining:?}"
                );
            }
            other => panic!("expected OpponentMayChoice, got {other:?}"),
        }

        // The controller accepts → must be offered their OWN land as a legal
        // sacrifice (BLOCKER 1).
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("controller accept must be handled");

        match &runner.state.waiting_for {
            WaitingFor::MultiTargetSelection {
                player,
                legal_targets,
                ..
            } => {
                assert_eq!(*player, P0, "the accepting controller picks the sacrifice");
                assert!(
                    legal_targets.contains(&p0_land),
                    "BLOCKER 1: controller's OWN land must be a legal sacrifice, \
                     got legal_targets={legal_targets:?}"
                );
            }
            other => panic!("expected MultiTargetSelection for the sacrifice, got {other:?}"),
        }

        // Submit the controller's own land and confirm it is sacrificed and the
        // "if a player does" consequence (draw a card) fires.
        let hand_before = runner
            .state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .hand
            .len();
        runner
            .act(GameAction::SelectCards {
                cards: vec![p0_land],
            })
            .expect("sacrificing the controller's own land must be legal");
        // Drain any remaining priority.
        for _ in 0..8 {
            if let WaitingFor::Priority { .. } = runner.state.waiting_for {
                if runner.state.stack.is_empty() || runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            } else {
                break;
            }
        }
        assert_eq!(
            runner.state.objects[&p0_land].zone,
            Zone::Graveyard,
            "controller's land must be sacrificed to the graveyard"
        );
        let hand_after = runner
            .state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .hand
            .len();
        assert_eq!(
            hand_after,
            hand_before + 1,
            "the 'if a player does' consequence must fire after the sacrifice"
        );
    }

    /// CR 608.2d + CR 701.21a: choosing to perform an impossible optional
    /// sacrifice is not "a player does". If the current promptee controls no
    /// legal permanent, the APNAP offer must continue to the next player rather
    /// than treating the empty selection as an accepted sacrifice.
    #[test]
    fn any_player_may_impossible_sacrifice_accept_continues_offer() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let _p1_land = scenario.add_basic_land(P1, ManaColor::White);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Group Bargain No Land Test",
                false,
                "any player may sacrifice a land of their choice. \
                 if a player does, you draw a card",
            )
            .id();
        scenario.with_library_top(P0, &["Reward Card"]);
        let mut runner = scenario.build();

        let wf = cast_free_sorcery_to_prompt(&mut runner, spell, None);
        match wf {
            WaitingFor::OpponentMayChoice {
                player, remaining, ..
            } => {
                assert_eq!(player, P0, "controller is still offered first");
                assert_eq!(remaining, vec![P1], "opponent remains queued");
            }
            other => panic!("expected first OpponentMayChoice, got {other:?}"),
        }

        let hand_before = runner
            .state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .hand
            .len();
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("empty sacrifice accept should advance to next player");

        match &runner.state.waiting_for {
            WaitingFor::OpponentMayChoice { player, .. } => {
                assert_eq!(
                    *player, P1,
                    "empty accept must not end the offer or fire the if-a-player-does rider"
                );
            }
            other => panic!("expected opponent offer after impossible accept, got {other:?}"),
        }
        let hand_after = runner
            .state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .hand
            .len();
        assert_eq!(
            hand_after, hand_before,
            "no card should be drawn because no sacrifice happened"
        );
    }

    /// CR 608.2d + CR 101.4 (issue #3236): Browbeat-class — when ALL players
    /// (including the controller) decline "any player may have ~ deal 5 damage
    /// to them", the "if no one does" reward fires.
    ///
    /// This is the step-3 (controller-inclusion) regression guard: the loop must
    /// prompt the controller too, and only after the controller AND the opponent
    /// both decline does the reward resolve.
    #[test]
    fn any_player_may_all_decline_fires_reward() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Browbeat Test",
                false,
                "any player may have ~ deal 5 damage to them. \
                 if no one does, target player draws three cards",
            )
            .id();
        // Library to draw from for the chosen reward target.
        scenario.with_library_top(P1, &["A", "B", "C", "D"]);
        let mut runner = scenario.build();

        let wf = cast_free_sorcery_to_prompt(&mut runner, spell, Some(TargetRef::Player(P1)));

        // Controller (P0) offered first.
        match wf {
            WaitingFor::OpponentMayChoice {
                player, remaining, ..
            } => {
                assert_eq!(player, P0, "controller must be offered first");
                assert!(remaining.contains(&P1), "opponent must be queued");
            }
            other => panic!("expected OpponentMayChoice, got {other:?}"),
        }

        let hand_before = runner
            .state
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .hand
            .len();

        // Controller declines.
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("controller decline must be handled");
        // Now the opponent must be prompted (proves the controller did NOT end
        // the loop alone).
        match &runner.state.waiting_for {
            WaitingFor::OpponentMayChoice { player, .. } => {
                assert_eq!(
                    *player, P1,
                    "opponent must be prompted after controller declines"
                );
            }
            other => panic!("expected opponent's OpponentMayChoice, got {other:?}"),
        }
        // Opponent declines → reward fires.
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("opponent decline must be handled");
        for _ in 0..8 {
            if let WaitingFor::Priority { .. } = runner.state.waiting_for {
                if runner.state.stack.is_empty() || runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            } else {
                break;
            }
        }
        let hand_after = runner
            .state
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .hand
            .len();
        assert_eq!(
            hand_after,
            hand_before + 3,
            "all players declined → the 'if no one does' reward (draw three) must fire"
        );
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
                target: TargetFilter::Controller,
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
        assert_eq!(
            obj.trigger_definitions[0].definition.mode,
            TriggerMode::ChangesZone
        );
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
    fn from_oracle_text_ixhel_carries_poison_scoped_trigger() {
        use crate::types::ability::{Effect, PlayerFilter, PlayerRelation};

        const IXHEL: &str = "Flying, vigilance, toxic 2\nCorrupted — At the beginning of your end step, each opponent who has three or more poison counters exiles the top card of their library face down. You may look at and play those cards for as long as they remain exiled, and you may spend mana as though it were mana of any color to cast those spells.";

        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature_from_oracle(P0, "Ixhel, Scion of Atraxa", 4, 4, IXHEL)
            .id();
        let runner = scenario.build();
        let trigger = runner.state().objects[&id]
            .trigger_definitions
            .iter_all()
            .find(|t| {
                t.definition
                    .description
                    .as_deref()
                    .is_some_and(|d| d.contains("poison counters"))
            })
            .expect("ixhel end-step trigger");
        let execute = trigger.definition.execute.as_ref().expect("execute");
        assert!(
            matches!(
                &*execute.effect,
                Effect::ExileTop {
                    face_down: true,
                    ..
                }
            ),
            "scenario oracle path must lower to face-down ExileTop, got {:?}",
            execute.effect
        );
        assert!(
            matches!(
                execute.player_scope,
                Some(PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::Opponent,
                    ..
                })
            ),
            "scenario oracle path must preserve poison player scope, got {:?}",
            execute.player_scope
        );
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
        assert!(
            obj.base_color.contains(&ManaColor::Red),
            "base color should be derived from mana cost"
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

    /// CR 113.2c / CR 702.116b: the scenario harness routes its keyword merge
    /// through the shared `merge_extracted_keywords` authority, so a creature whose
    /// Oracle text prints "Myriad, myriad" must carry two Keyword::Myriad instances
    /// — locking the scenario path to the production multiplicity behavior.
    #[test]
    fn from_oracle_text_recovers_repeated_myriad_instances() {
        let mut scenario = GameScenario::new();
        let id = scenario
            .add_creature(P0, "Scurry of Squirrels", 3, 3)
            .from_oracle_text_with_keywords(
                &["myriad"],
                "Myriad, myriad (Whenever this creature attacks, for each opponent other than defending player, you may create a token that's a copy of this creature that's tapped and attacking that player or a planeswalker they control. Then do it again. Exile the tokens at end of combat.)",
            )
            .id();
        let runner = scenario.build();
        let obj = &runner.state().objects[&id];

        assert_eq!(
            obj.keywords
                .iter()
                .filter(|k| matches!(k, Keyword::Myriad))
                .count(),
            2,
            "scenario face must carry two Myriad instances via the shared merge"
        );
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

    /// Build a scenario with a 2/2 victim (P1) that carries a regeneration
    /// shield and an Incinerate-class spell (P0) carrying `oracle_text`. Casts
    /// the spell at the victim through the full pipeline and returns the
    /// `(CastOutcome, victim_id)`.
    fn cast_damage_at_shielded_victim(oracle_text: &str) -> (CastOutcome, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // CR 701.19a: a 2/2 creature carrying a one-shot regeneration shield.
        let victim = scenario
            .add_creature(P1, "Shielded Bear", 2, 2)
            .with_replacement_definition(
                ReplacementDefinition::new(crate::types::replacements::ReplacementEvent::Destroy)
                    .valid_card(TargetFilter::SelfRef)
                    .description("Regenerate".to_string())
                    .regeneration_shield(),
            )
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Incinerate Test", true, oracle_text)
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).target_object(victim).resolve();
        (outcome, victim)
    }

    /// CR 701.19c + CR 608.2c + CR 614.8 (issue #3333) — DISCRIMINATING runtime
    /// gate. Incinerate deals LETHAL damage to a creature that HAS a regeneration
    /// shield. The "A creature dealt damage this way can't be regenerated this
    /// turn." rider must publish the damaged creature (non-empty tracked set) and
    /// grant it `CantBeRegenerated`, so the SBA destruction BYPASSES the shield
    /// and the creature DIES. This fails if the Part 1 collector arm OR the Part 3
    /// TrackedSet binding is reverted (the set is empty → no static → shield
    /// saves the creature).
    #[test]
    fn incinerate_rider_bypasses_regeneration_shield() {
        let (outcome, victim) = cast_damage_at_shielded_victim(
            "Incinerate deals 3 damage to any target. A creature dealt damage \
             this way can't be regenerated this turn.",
        );

        // CR 701.19c: the shield is not applied — the creature is destroyed.
        outcome.assert_zone(&[victim], Zone::Graveyard);

        // DIRECTLY catches the empty-bind regression: the damage clause must have
        // published a NON-EMPTY tracked set containing the damaged creature's id.
        let published_any = outcome
            .state()
            .tracked_object_sets
            .values()
            .any(|set| set.contains(&victim));
        assert!(
            published_any,
            "the damage clause must publish a non-empty tracked set containing \
             the damaged creature (got {:?})",
            outcome.state().tracked_object_sets
        );
    }

    /// CR 701.19a/b — CONTROL for the discriminating gate. The same lethal damage
    /// to the same shielded creature WITHOUT the regen rider: the regeneration
    /// shield SAVES the creature (it stays on the battlefield, tapped, with damage
    /// removed). Confirms the bypass above is caused specifically by the rider,
    /// not by the damage alone.
    #[test]
    fn damage_without_rider_lets_regeneration_shield_save() {
        let (outcome, victim) =
            cast_damage_at_shielded_victim("Incinerate Test deals 3 damage to any target.");

        // CR 701.19a/b: the shield regenerates the creature — it survives.
        outcome.assert_zone(&[victim], Zone::Battlefield);
        assert!(
            outcome.state().objects[&victim].tapped,
            "CR 701.19b: a regenerated creature is tapped"
        );
        assert_eq!(
            outcome.state().objects[&victim].damage_marked,
            0,
            "CR 701.19a: regeneration removes all marked damage"
        );
    }

    /// CR 613.1f + CR 613.4b + CR 205.1b: Curious Colossus end-to-end. Its ETB
    /// targets an opponent; each creature that opponent controls "loses all
    /// abilities, becomes a Coward in addition to its other types, and has base
    /// power and toughness 1/1". After casting through the real pipeline and
    /// evaluating layers, the affected creature must (a) have no abilities/
    /// keywords (layer 6 RemoveAllAbilities), (b) gain the Coward subtype while
    /// KEEPING its prior subtype (CR 205.1b additive), and (c) have effective
    /// power/toughness 1/1 (layer 7b set). The comma-split guards are what keep
    /// the trailing base-P/T conjunct in the GenericEffect; if reverted, the base
    /// P/T conjunct orphans to Unimplemented and the creature stays 3/4.
    #[test]
    fn curious_colossus_etb_strips_abilities_adds_subtype_sets_base_pt() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // The opponent's victim: a 3/4 Bird with flying.
        let victim = scenario
            .add_creature(P1, "Victim Bird", 3, 4)
            .with_subtypes(vec!["Bird"])
            .flying()
            .id();

        // Curious Colossus in P0's hand, parsed from its real Oracle text.
        let colossus = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Curious Colossus",
                7,
                7,
                "When this creature enters, each creature target opponent controls \
                 loses all abilities, becomes a Coward in addition to its other types, \
                 and has base power and toughness 1/1.",
            )
            .id();

        // Fund {5}{W}{W} from the pool (2 White + 5 Colorless).
        let mut mana = vec![ManaUnit::new(ManaType::White, ObjectId(9_999), false, vec![]); 2];
        mana.extend(vec![
            ManaUnit::new(
                ManaType::Colorless,
                ObjectId(9_999),
                false,
                vec![]
            );
            5
        ]);
        scenario.with_mana_pool(P0, mana);

        let mut runner = scenario.build();

        // Cast Curious Colossus; its ETB targets opponent P1.
        let outcome = runner.cast(colossus).target_player(P1).resolve();

        // Evaluate layers on the resolved state and inspect the affected creature.
        let mut state = outcome.state().clone();
        state.layers_dirty.mark_full();
        crate::game::layers::evaluate_layers(&mut state);
        let obj = state
            .objects
            .get(&victim)
            .expect("victim still on battlefield");

        // (a) CR 613.1f: all abilities/keywords removed.
        assert!(
            !crate::game::keywords::has_keyword(obj, &Keyword::Flying),
            "RemoveAllAbilities must strip flying, keywords={:?}",
            obj.keywords
        );
        assert!(
            obj.abilities.is_empty(),
            "RemoveAllAbilities must clear printed abilities, got {:?}",
            obj.abilities
        );

        // (b) CR 205.1b: Coward added "in addition to its other types" — prior
        // Bird subtype is KEPT.
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Coward"),
            "must gain the Coward subtype, subtypes={:?}",
            obj.card_types.subtypes
        );
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Bird"),
            "must KEEP the prior Bird subtype (additive), subtypes={:?}",
            obj.card_types.subtypes
        );

        // (c) CR 613.4b: base power and toughness set to 1/1.
        assert_eq!(
            obj.power,
            Some(1),
            "effective power must be 1, got {:?}",
            obj.power
        );
        assert_eq!(
            obj.toughness,
            Some(1),
            "effective toughness must be 1, got {:?}",
            obj.toughness
        );
    }
}
