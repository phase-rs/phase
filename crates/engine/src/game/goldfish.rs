//! Goldfish (solitaire) playtest mode — drives the real engine in a two-player
//! configuration where the second seat auto-passes every decision.
//!
//! All zone operations, turn advancement, mulligan logic, and mana counting
//! go through the same `GameState` / `engine::apply` path used by real games.
//! No game rules are reimplemented here; this module is a thin orchestration
//! layer that presents a simplified view of the engine state to the WASM
//! surface.
//!
//! The human player is always `PlayerId(0)`; the auto-passing goldfish
//! opponent is `PlayerId(1)` (given a placeholder Island deck so the draw
//! step never causes an empty-library loss).
//!
//! For Monte Carlo aggregate statistics see [`run_simulation`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::game::deck_loading::{DeckEntry, DeckPayload, PlayerDeckPayload};
use crate::game::engine;
use crate::types::ability::PtValue;
use crate::types::actions::{GameAction, MulliganChoice};
use crate::types::card::CardFace;
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::format::FormatConfig;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

// ── View types ────────────────────────────────────────────────────────────────

/// Per-turn snapshot captured at the start of each human turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSnapshot {
    pub turn_number: u32,
    pub hand_size: usize,
    pub lands_in_play: usize,
    pub mana_sources_in_play: usize,
    pub available_mana: usize,
    pub cards_drawn: u32,
    pub lands_in_hand: usize,
    pub playable_count: usize,
}

/// Lightweight card descriptor surfaced to the WASM layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewCard {
    /// Stable object id — matches the argument expected by all `GoldfishGame`
    /// methods (narrowed to u32 because WASM/JS numbers are f64-safe to 2^53).
    pub id: u32,
    /// Original printed face (includes oracle text, mana cost, types, etc.).
    pub face: CardFace,
}

/// Permanent on the goldfish battlefield.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPermanent {
    pub card: ViewCard,
    pub tapped: bool,
    /// True when this permanent entered the battlefield this turn.
    pub entered_this_turn: bool,
}

/// Full view DTO serialised to the TypeScript layer.
///
/// Shape is intentionally identical to the old `PlaytestSession` JSON, with
/// three new engine-computed fields (`available_mana`, `legal_land_ids`,
/// `legal_cast_ids`) so the frontend never computes affordability itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoldfishView {
    pub library: Vec<ViewCard>,
    pub hand: Vec<ViewCard>,
    pub battlefield: Vec<ViewPermanent>,
    pub graveyard: Vec<ViewCard>,
    pub exile: Vec<ViewCard>,

    pub turn_number: u32,
    pub land_played_this_turn: bool,
    pub drew_this_turn: bool,
    pub going_first: bool,

    pub in_mulligan: bool,
    pub mulligan_count: u8,
    pub bottoming_required: u8,

    /// Untapped mana sources the human owns (engine-authoritative count).
    pub available_mana: usize,
    /// ObjectIds of lands in hand that can legally be played this turn.
    pub legal_land_ids: Vec<u32>,
    /// ObjectIds of non-land cards the human can afford to cast this turn.
    pub legal_cast_ids: Vec<u32>,

    pub history: Vec<TurnSnapshot>,
}

// ── GoldfishGame ──────────────────────────────────────────────────────────────

/// Active goldfish playtest session.  Wraps a real two-player `GameState`
/// where `PlayerId(0)` is the human and `PlayerId(1)` auto-passes everything.
pub struct GoldfishGame {
    state: GameState,
    human: PlayerId,
    goldfish: PlayerId,
    /// `ObjectId.0 → CardFace` map populated during deck loading so the view
    /// layer can access oracle text / mana cost that the engine stores on
    /// `GameObject` only as parsed fields.
    card_faces: HashMap<u64, CardFace>,
    deck: Vec<CardFace>,
    seed: u64,
    going_first: bool,
    history: Vec<TurnSnapshot>,
    drew_this_turn: bool,
    cards_drawn_this_turn: u32,
}

impl GoldfishGame {
    // ── Construction ──────────────────────────────────────────────────────────

    /// Create a new goldfish session from the human player's deck.
    ///
    /// Initialises a two-player `GameState`, loads the human deck into
    /// `PlayerId(0)` and a 40-Island placeholder into `PlayerId(1)`, starts
    /// the game, and auto-keeps for the goldfish so only the human's mulligan
    /// decision remains.
    pub fn new(deck: Vec<CardFace>, seed: u64, going_first: bool) -> Self {
        let mut state = GameState::new(FormatConfig::standard(), 2, seed);

        // One DeckEntry per card copy (count = 1 each).
        let human_entries: Vec<DeckEntry> = deck
            .iter()
            .map(|face| DeckEntry {
                card: face.clone(),
                count: 1,
            })
            .collect();

        // 40 basic Islands for the goldfish — enough that the draw step never
        // causes an empty-library loss in any realistic test session.
        let goldfish_entry = DeckEntry {
            card: basic_island_face(),
            count: 40,
        };

        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: human_entries.clone(),
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![goldfish_entry],
                ..Default::default()
            },
            ..Default::default()
        };

        // Record the counter value before loading so we can map ObjectId → face.
        // Human deck objects are always created first (load_deck_into_state
        // iterates player entries before opponent entries), so IDs run
        // consecutively from start_id for deck.len() objects.
        let start_id = state.next_object_id;
        crate::game::deck_loading::load_and_hydrate_decks(&mut state, &payload, None);

        let mut card_faces: HashMap<u64, CardFace> = HashMap::new();
        for (i, entry) in human_entries.iter().enumerate() {
            card_faces.insert(start_id + i as u64, entry.card.clone());
        }

        // CR 103.7a: First player skips their first draw step; the engine
        // handles this automatically once we call start_game_with_starting_player.
        let starting_player = if going_first {
            PlayerId(0)
        } else {
            PlayerId(1)
        };
        engine::start_game_with_starting_player(&mut state, starting_player);

        let mut game = GoldfishGame {
            state,
            human: PlayerId(0),
            goldfish: PlayerId(1),
            card_faces,
            deck,
            seed,
            going_first,
            history: Vec::new(),
            drew_this_turn: false,
            cards_drawn_this_turn: 0,
        };

        game.auto_advance_goldfish();
        game
    }

    /// Reset to a fresh game with the same deck and seed.
    pub fn reset(&mut self) {
        *self = Self::new(self.deck.clone(), self.seed, self.going_first);
    }

    /// Reset with a different RNG seed.
    pub fn reset_with_seed(&mut self, seed: u64) {
        *self = Self::new(self.deck.clone(), seed, self.going_first);
    }

    // ── Mulligan ──────────────────────────────────────────────────────────────

    /// Human keeps their current opening hand.
    pub fn keep_hand(&mut self) -> Result<(), String> {
        engine::apply(
            &mut self.state,
            self.human,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        )
        .map_err(|e| e.to_string())?;
        self.advance_past_automatic_phases();
        self.capture_snapshot();
        Ok(())
    }

    /// Human takes a mulligan (redraw seven, increment counter).
    pub fn take_mulligan(&mut self) -> Result<(), String> {
        engine::apply(
            &mut self.state,
            self.human,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Mulligan,
            },
        )
        .map_err(|e| e.to_string())?;
        self.auto_advance_goldfish();
        Ok(())
    }

    /// Place one card from hand on the bottom of the library (CR 103.5 —
    /// London-mulligan bottoming step).  Call once per required card.
    pub fn bottom_card(&mut self, object_id: u32) -> Result<(), String> {
        engine::apply(
            &mut self.state,
            self.human,
            GameAction::SelectCards {
                cards: vec![ObjectId(object_id as u64)],
            },
        )
        .map_err(|e| e.to_string())?;
        self.advance_past_automatic_phases();
        Ok(())
    }

    // ── Turn actions ──────────────────────────────────────────────────────────

    /// Play a land from hand (CR 305.2 — one per turn, enforced by the engine).
    pub fn play_land(&mut self, object_id: u32) -> Result<(), String> {
        let id = object_id as u64;
        engine::apply(
            &mut self.state,
            self.human,
            GameAction::PlayLand {
                object_id: ObjectId(id),
                card_id: CardId(id),
            },
        )
        .map_err(|e| e.to_string())
        .map(|_| ())
    }

    /// Cast a non-land card.  Uses auto-payment (engine taps cheapest mana sources).
    pub fn cast_spell(&mut self, object_id: u32) -> Result<(), String> {
        let id = object_id as u64;
        engine::apply(
            &mut self.state,
            self.human,
            GameAction::CastSpell {
                object_id: ObjectId(id),
                card_id: CardId(id),
                targets: Vec::new(),
                payment_mode: Default::default(),
            },
        )
        .map_err(|e| e.to_string())?;
        self.advance_past_automatic_phases();
        Ok(())
    }

    /// Discard a card from hand.
    ///
    /// If the engine is waiting for a cleanup-step discard, routes through the
    /// authoritative `SelectCards` action.  Otherwise moves the object directly
    /// (goldfish convenience — a freely chosen "analysis discard" outside rules).
    pub fn discard_card(&mut self, object_id: u32) -> Result<(), String> {
        let oid = ObjectId(object_id as u64);
        match &self.state.waiting_for.clone() {
            WaitingFor::DiscardToHandSize { player, .. } if *player == self.human => engine::apply(
                &mut self.state,
                self.human,
                GameAction::SelectCards { cards: vec![oid] },
            )
            .map_err(|e| e.to_string())
            .map(|_| ()),
            _ => {
                let mut events = Vec::new();
                crate::game::zones::move_to_zone(
                    &mut self.state,
                    oid,
                    Zone::Graveyard,
                    &mut events,
                );
                Ok(())
            }
        }
    }

    /// Tap a permanent on the battlefield.
    pub fn tap_permanent(&mut self, object_id: u32) -> Result<(), String> {
        let obj = self
            .state
            .objects
            .get_mut(&ObjectId(object_id as u64))
            .ok_or_else(|| format!("object {object_id} not found"))?;
        if obj.tapped {
            return Err("already tapped".to_string());
        }
        obj.tapped = true;
        Ok(())
    }

    /// Untap a permanent on the battlefield.
    pub fn untap_permanent(&mut self, object_id: u32) -> Result<(), String> {
        let obj = self
            .state
            .objects
            .get_mut(&ObjectId(object_id as u64))
            .ok_or_else(|| format!("object {object_id} not found"))?;
        obj.tapped = false;
        Ok(())
    }

    /// Move a battlefield permanent to the graveyard.
    pub fn destroy_permanent(&mut self, object_id: u32) -> Result<(), String> {
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(
            &mut self.state,
            ObjectId(object_id as u64),
            Zone::Graveyard,
            &mut events,
        );
        Ok(())
    }

    /// Move a battlefield permanent to exile.
    pub fn exile_permanent(&mut self, object_id: u32) -> Result<(), String> {
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(
            &mut self.state,
            ObjectId(object_id as u64),
            Zone::Exile,
            &mut events,
        );
        Ok(())
    }

    /// Return a battlefield permanent to hand.
    pub fn bounce_permanent(&mut self, object_id: u32) -> Result<(), String> {
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(
            &mut self.state,
            ObjectId(object_id as u64),
            Zone::Hand,
            &mut events,
        );
        Ok(())
    }

    /// Draw one card from the top of the human's library into hand.
    ///
    /// Convenience for analysis — bypasses the engine's normal draw step so the
    /// human can inspect additional draws without advancing the turn.
    pub fn draw_one(&mut self) {
        let top = self
            .state
            .players
            .iter()
            .find(|p| p.id == self.human)
            .and_then(|p| p.library.front().copied());
        if let Some(id) = top {
            let mut events = Vec::new();
            crate::game::zones::move_to_zone(&mut self.state, id, Zone::Hand, &mut events);
        }
    }

    /// Draw N cards from the top of the human's library into hand.
    pub fn draw_n(&mut self, n: u32) {
        for _ in 0..n {
            let top = self
                .state
                .players
                .iter()
                .find(|p| p.id == self.human)
                .and_then(|p| p.library.front().copied());
            let Some(id) = top else { break };
            let mut events = Vec::new();
            crate::game::zones::move_to_zone(&mut self.state, id, Zone::Hand, &mut events);
        }
    }

    /// Advance to the next turn.
    ///
    /// Auto-passes through the end step, goldfish's full turn, and back to the
    /// human's pre-combat main phase.  Returns `Err` if the human still needs
    /// to discard to hand size first.
    pub fn advance_turn(&mut self) -> Result<(), String> {
        if matches!(
            &self.state.waiting_for,
            WaitingFor::DiscardToHandSize { player, .. } if *player == self.human
        ) {
            return Err("Discard to hand size before advancing".to_string());
        }

        let start_turn = self.state.turn_number;
        self.drew_this_turn = false;
        self.cards_drawn_this_turn = 0;

        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 500 {
                break;
            }

            // Stop when we reach the human's PreCombatMain on a later turn.
            if self.state.active_player == self.human
                && self.state.turn_number > start_turn
                && self.state.phase == Phase::PreCombatMain
                && matches!(
                    &self.state.waiting_for,
                    WaitingFor::Priority { player } if *player == self.human
                )
            {
                break;
            }

            match self.state.waiting_for.clone() {
                WaitingFor::Priority { player } => {
                    if player == self.human
                        && self.state.phase == Phase::Draw
                        && self.state.turn_number > start_turn
                    {
                        self.drew_this_turn = true;
                        self.cards_drawn_this_turn += 1;
                    }
                    engine::apply(&mut self.state, player, GameAction::PassPriority)
                        .map_err(|e| e.to_string())?;
                }
                WaitingFor::DiscardToHandSize {
                    player,
                    cards,
                    count,
                } => {
                    if player == self.human {
                        // Surface to caller — human must discard first.
                        break;
                    }
                    // Goldfish auto-discards first `count` cards in hand.
                    let to_discard: Vec<ObjectId> = cards.iter().copied().take(count).collect();
                    engine::apply(
                        &mut self.state,
                        player,
                        GameAction::SelectCards { cards: to_discard },
                    )
                    .map_err(|e| e.to_string())?;
                }
                WaitingFor::GameOver { .. } => break,
                WaitingFor::MulliganDecision { .. } | WaitingFor::MulliganBottomCards { .. } => {
                    self.auto_advance_goldfish();
                }
                _ => break,
            }
        }

        self.capture_snapshot();
        Ok(())
    }

    // ── View ──────────────────────────────────────────────────────────────────

    /// Compute a view DTO from the current engine state.
    pub fn view(&self) -> GoldfishView {
        let player = self
            .state
            .players
            .iter()
            .find(|p| p.id == self.human)
            .expect("human player always present");

        let hand: Vec<ViewCard> = player
            .hand
            .iter()
            .filter_map(|&id| self.view_card(id))
            .collect();

        let current_turn = self.state.turn_number;
        let mut battlefield: Vec<ViewPermanent> = self
            .state
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Battlefield && obj.owner == self.human)
            .filter_map(|obj| {
                self.view_card(obj.id).map(|card| ViewPermanent {
                    card,
                    tapped: obj.tapped,
                    entered_this_turn: obj.entered_battlefield_turn == Some(current_turn),
                })
            })
            .collect();
        battlefield.sort_by_key(|p| p.card.id);

        let graveyard: Vec<ViewCard> = player
            .graveyard
            .iter()
            .filter_map(|&id| self.view_card(id))
            .collect();

        let mut exile: Vec<ViewCard> = self
            .state
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Exile && obj.owner == self.human)
            .filter_map(|obj| self.view_card(obj.id))
            .collect();
        exile.sort_by_key(|c| c.id);

        let library: Vec<ViewCard> = player
            .library
            .iter()
            .filter_map(|&id| self.view_card(id))
            .collect();

        let (in_mulligan, mulligan_count, bottoming_required) = self.mulligan_state();
        let available_mana = self.compute_available_mana();
        let land_played = player.lands_played_this_turn > 0;

        let legal_land_ids: Vec<u32> = if !in_mulligan && !land_played {
            hand.iter()
                .filter(|c| is_land(&c.face))
                .map(|c| c.id)
                .collect()
        } else {
            Vec::new()
        };

        let legal_cast_ids: Vec<u32> = if !in_mulligan {
            hand.iter()
                .filter(|c| {
                    !is_land(&c.face) && c.face.mana_cost.mana_value() as usize <= available_mana
                })
                .map(|c| c.id)
                .collect()
        } else {
            Vec::new()
        };

        let turn_number = if in_mulligan {
            0
        } else {
            let t = self.state.turn_number;
            (if self.going_first {
                t.div_ceil(2)
            } else {
                t / 2
            })
            .max(1)
        };

        GoldfishView {
            library,
            hand,
            battlefield,
            graveyard,
            exile,
            turn_number,
            land_played_this_turn: land_played,
            drew_this_turn: self.drew_this_turn,
            going_first: self.going_first,
            in_mulligan,
            mulligan_count,
            bottoming_required,
            available_mana,
            legal_land_ids,
            legal_cast_ids,
            history: self.history.clone(),
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn view_card(&self, id: ObjectId) -> Option<ViewCard> {
        let obj = self.state.objects.get(&id)?;
        let face = if let Some(f) = self.card_faces.get(&id.0) {
            f.clone()
        } else {
            // Fallback: reconstruct a minimal face from engine object data.
            CardFace {
                name: obj.name.clone(),
                mana_cost: obj.mana_cost.clone(),
                card_type: obj.card_types.clone(),
                power: obj.power.map(PtValue::Fixed),
                toughness: obj.toughness.map(PtValue::Fixed),
                ..Default::default()
            }
        };
        Some(ViewCard {
            id: id.0 as u32,
            face,
        })
    }

    fn mulligan_state(&self) -> (bool, u8, u8) {
        match &self.state.waiting_for {
            WaitingFor::MulliganDecision { pending, .. } => {
                let count = pending
                    .iter()
                    .find(|e| e.player == self.human)
                    .map(|e| e.mulligan_count)
                    .unwrap_or(0);
                (true, count, 0)
            }
            WaitingFor::MulliganBottomCards { pending } => {
                let bottoming = pending
                    .iter()
                    .find(|e| e.player == self.human)
                    .map(|e| e.count)
                    .unwrap_or(0);
                (true, bottoming, bottoming)
            }
            _ => (false, 0, 0),
        }
    }

    /// Count untapped lands the human owns on the battlefield.
    ///
    /// Conservative heuristic: only counts actual land permanents.  Mana dorks
    /// and rocks require activated-ability support not yet modelled in the
    /// goldfish path.
    pub(crate) fn compute_available_mana(&self) -> usize {
        self.state
            .objects
            .values()
            .filter(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.owner == self.human
                    && !obj.tapped
                    && obj.card_types.core_types.contains(&CoreType::Land)
            })
            .count()
    }

    fn capture_snapshot(&mut self) {
        let player = self
            .state
            .players
            .iter()
            .find(|p| p.id == self.human)
            .expect("human player always present");

        let hand_ids: Vec<ObjectId> = player.hand.iter().copied().collect();
        let hand_size = hand_ids.len();

        let lands_in_hand = hand_ids
            .iter()
            .filter(|&&id| {
                self.state
                    .objects
                    .get(&id)
                    .map(|o| o.card_types.core_types.contains(&CoreType::Land))
                    .unwrap_or(false)
            })
            .count();

        let available_mana = self.compute_available_mana();

        let lands_in_play = self
            .state
            .objects
            .values()
            .filter(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.owner == self.human
                    && obj.card_types.core_types.contains(&CoreType::Land)
            })
            .count();

        let playable_count = hand_ids
            .iter()
            .filter(|&&id| {
                self.state
                    .objects
                    .get(&id)
                    .map(|o| {
                        !o.card_types.core_types.contains(&CoreType::Land)
                            && o.mana_cost.mana_value() as usize <= available_mana
                    })
                    .unwrap_or(false)
            })
            .count();

        let turn_number = self.history.len() as u32 + 1;

        self.history.push(TurnSnapshot {
            turn_number,
            hand_size,
            lands_in_play,
            mana_sources_in_play: lands_in_play,
            available_mana,
            cards_drawn: self.cards_drawn_this_turn,
            lands_in_hand,
            playable_count,
        });
    }

    /// Auto-pass all pending goldfish mulligan decisions.
    fn auto_advance_goldfish(&mut self) {
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 200 {
                break;
            }
            match self.state.waiting_for.clone() {
                WaitingFor::MulliganDecision { pending, .. } => {
                    if !pending.iter().any(|e| e.player == self.goldfish) {
                        break;
                    }
                    let _ = engine::apply(
                        &mut self.state,
                        self.goldfish,
                        GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        },
                    );
                }
                WaitingFor::MulliganBottomCards { pending } => {
                    let Some(entry) = pending.iter().find(|e| e.player == self.goldfish) else {
                        break;
                    };
                    let count = entry.count as usize;
                    let hand_ids: Vec<ObjectId> = self
                        .state
                        .players
                        .iter()
                        .find(|p| p.id == self.goldfish)
                        .map(|p| p.hand.iter().copied().take(count).collect())
                        .unwrap_or_default();
                    let _ = engine::apply(
                        &mut self.state,
                        self.goldfish,
                        GameAction::SelectCards { cards: hand_ids },
                    );
                }
                _ => break,
            }
        }
    }

    /// Advance through phases that don't need human input until the human has
    /// priority in a main phase.
    fn advance_past_automatic_phases(&mut self) {
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 200 {
                break;
            }
            match self.state.waiting_for.clone() {
                WaitingFor::Priority { player } if player == self.human => {
                    if matches!(
                        self.state.phase,
                        Phase::PreCombatMain | Phase::PostCombatMain
                    ) {
                        break;
                    }
                    let _ = engine::apply(&mut self.state, self.human, GameAction::PassPriority);
                }
                WaitingFor::Priority { player } => {
                    let _ = engine::apply(&mut self.state, player, GameAction::PassPriority);
                }
                WaitingFor::MulliganDecision { .. } | WaitingFor::MulliganBottomCards { .. } => {
                    self.auto_advance_goldfish();
                }
                _ => break,
            }
        }
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// CR 305.1: True when the card face represents a land.
fn is_land(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Land)
}

/// Minimal basic Island face used as the goldfish placeholder deck.
fn basic_island_face() -> CardFace {
    CardFace {
        name: "Island".to_string(),
        card_type: CardType {
            supertypes: vec![Supertype::Basic],
            core_types: vec![CoreType::Land],
            subtypes: vec!["Island".to_string()],
        },
        // CR 305.6: Basic lands have no mana cost.
        mana_cost: ManaCost::NoCost,
        oracle_text: Some("{T}: Add {U}.".to_string()),
        ..Default::default()
    }
}

// ── Monte Carlo simulation ────────────────────────────────────────────────────

/// Parameters for a Monte Carlo playtest simulation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationConfig {
    pub num_games: u32,
    pub num_turns: u32,
    pub base_seed: u64,
    pub going_first: bool,
    pub auto_keep: bool,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            num_games: 200,
            num_turns: 10,
            base_seed: 0xdead_beef,
            going_first: true,
            auto_keep: false,
        }
    }
}

/// Aggregated statistics for a single turn across all simulated games.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAggregate {
    pub turn_number: u32,
    pub avg_hand_size: f64,
    pub avg_lands_in_play: f64,
    pub avg_mana_sources: f64,
    pub avg_available_mana: f64,
    pub avg_playable_count: f64,
    pub pct_land_in_hand: f64,
    pub pct_empty_library: f64,
    pub min_available_mana: f64,
    pub max_available_mana: f64,
    pub stddev_available_mana: f64,
}

/// Statistics about the opening hand distribution across simulated games.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpeningHandStats {
    pub avg_lands: f64,
    pub avg_spells: f64,
    pub avg_hand_size: f64,
    pub hand_size_distribution: Vec<u32>,
    pub avg_mulligans: f64,
    pub pct_keep_first: f64,
}

/// Full simulation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationResult {
    pub turns: Vec<TurnAggregate>,
    pub opening_hand: OpeningHandStats,
    pub games_simulated: u32,
    pub config: SimulationConfig,
}

/// Simple land-count mulligan heuristic: keep if hand has a reasonable land count.
/// Thresholds widen on subsequent mulligans (smaller hands tolerate more variance).
fn sim_should_keep(hand: &[ViewCard], mulligan_count: u8) -> bool {
    let lands = hand.iter().filter(|c| is_land(&c.face)).count();
    let size = hand.len();
    if size == 0 {
        return true;
    }
    let (min_l, max_l) = match mulligan_count {
        0 | 1 => (2, 5),
        2 => (1, 5),
        _ => (1, size.saturating_sub(1)),
    };
    lands >= min_l && lands <= max_l
}

/// Run a Monte Carlo simulation, returning per-turn aggregate statistics.
///
/// Each game drives a real `GoldfishGame` (real engine zones and turn flow)
/// with a greedy strategy: play a land if legal, cast whatever is affordable.
pub fn run_simulation(deck: &[CardFace], config: &SimulationConfig) -> SimulationResult {
    if deck.is_empty() || config.num_games == 0 || config.num_turns == 0 {
        return SimulationResult {
            turns: Vec::new(),
            opening_hand: OpeningHandStats::default(),
            games_simulated: 0,
            config: config.clone(),
        };
    }

    let n_turns = config.num_turns as usize;

    let mut acc_hand = vec![0.0f64; n_turns];
    let mut acc_lands_play = vec![0.0f64; n_turns];
    let mut acc_mana = vec![0.0f64; n_turns];
    let mut acc_playable = vec![0.0f64; n_turns];
    let mut acc_land_in_hand = vec![0.0f64; n_turns];
    let mut acc_empty_lib = vec![0.0f64; n_turns];
    let mut acc_mana_sq = vec![0.0f64; n_turns];
    let mut acc_mana_min = vec![f64::MAX; n_turns];
    let mut acc_mana_max = vec![f64::MIN; n_turns];

    let mut oh_lands = 0.0f64;
    let mut oh_hand_size = 0.0f64;
    let mut oh_mulligans = 0.0f64;
    let mut oh_keep_first: u32 = 0;
    let mut oh_hand_dist = vec![0u32; 8];

    let mut games_simulated: u32 = 0;

    for game_idx in 0..config.num_games {
        let seed = config.base_seed ^ (game_idx as u64);
        let mut game = GoldfishGame::new(deck.to_vec(), seed, config.going_first);

        // Mulligan phase.
        let mut mull_count: u8 = 0;
        if !config.auto_keep {
            loop {
                let v = game.view();
                if !v.in_mulligan || mull_count >= 7 {
                    break;
                }
                if sim_should_keep(&v.hand, mull_count) {
                    break;
                }
                if game.take_mulligan().is_err() {
                    break;
                }
                mull_count += 1;

                // Bottom excess lands first; otherwise bottom highest-CMC spells.
                let v2 = game.view();
                let to_bottom = v2.bottoming_required as usize;
                let land_heavy = v2.hand.iter().filter(|c| is_land(&c.face)).count() > 4;
                let mut candidates: Vec<(u32, u32)> = v2
                    .hand
                    .iter()
                    .filter(|c| {
                        if land_heavy {
                            is_land(&c.face)
                        } else {
                            !is_land(&c.face)
                        }
                    })
                    .map(|c| (c.id, c.face.mana_cost.mana_value()))
                    .collect();
                candidates.sort_by_key(|b| std::cmp::Reverse(b.1));
                for (id, _) in candidates.into_iter().take(to_bottom) {
                    let _ = game.bottom_card(id);
                }
            }
        }

        if game.keep_hand().is_err() {
            continue;
        }

        // Opening-hand stats.
        let v = game.view();
        let kept_lands = v.hand.iter().filter(|c| is_land(&c.face)).count();
        oh_lands += kept_lands as f64;
        oh_hand_size += v.hand.len() as f64;
        oh_mulligans += mull_count as f64;
        if mull_count == 0 {
            oh_keep_first += 1;
        }
        let dist_idx = 7usize.saturating_sub(v.hand.len()).min(7);
        oh_hand_dist[dist_idx] += 1;

        for turn_idx in 0..n_turns {
            if turn_idx > 0 {
                // Discard to hand size if needed.
                let v2 = game.view();
                if v2.hand.len() > 7 {
                    let excess = v2.hand.len() - 7;
                    let mut spells: Vec<(u32, u32)> = v2
                        .hand
                        .iter()
                        .filter(|c| !is_land(&c.face))
                        .map(|c| (c.id, c.face.mana_cost.mana_value()))
                        .collect();
                    spells.sort_by_key(|b| std::cmp::Reverse(b.1));
                    for (id, _) in spells.into_iter().take(excess) {
                        let _ = game.discard_card(id);
                    }
                }
                if game.advance_turn().is_err() {
                    break;
                }
            }

            // Greedy land drop.
            let v3 = game.view();
            if let Some(land_id) = v3.legal_land_ids.first().copied() {
                let _ = game.play_land(land_id);
            }

            // Record per-turn snapshot.
            let snap = game.history.last().cloned().unwrap_or(TurnSnapshot {
                turn_number: (turn_idx + 1) as u32,
                hand_size: game.view().hand.len(),
                lands_in_play: 0,
                mana_sources_in_play: 0,
                available_mana: game.compute_available_mana(),
                cards_drawn: 0,
                lands_in_hand: 0,
                playable_count: 0,
            });

            let m = snap.available_mana as f64;
            acc_hand[turn_idx] += snap.hand_size as f64;
            acc_lands_play[turn_idx] += snap.lands_in_play as f64;
            acc_mana[turn_idx] += m;
            acc_playable[turn_idx] += snap.playable_count as f64;
            acc_land_in_hand[turn_idx] += if snap.lands_in_hand > 0 { 1.0 } else { 0.0 };
            acc_empty_lib[turn_idx] += if game.view().library.is_empty() {
                1.0
            } else {
                0.0
            };
            acc_mana_sq[turn_idx] += m * m;
            if m < acc_mana_min[turn_idx] {
                acc_mana_min[turn_idx] = m;
            }
            if m > acc_mana_max[turn_idx] {
                acc_mana_max[turn_idx] = m;
            }
        }

        games_simulated += 1;
    }

    let n = games_simulated as f64;
    let turns = (0..n_turns)
        .map(|i| {
            let avg = acc_mana[i] / n;
            let variance = (acc_mana_sq[i] / n) - avg * avg;
            TurnAggregate {
                turn_number: (i + 1) as u32,
                avg_hand_size: acc_hand[i] / n,
                avg_lands_in_play: acc_lands_play[i] / n,
                avg_mana_sources: acc_lands_play[i] / n,
                avg_available_mana: avg,
                avg_playable_count: acc_playable[i] / n,
                pct_land_in_hand: acc_land_in_hand[i] / n,
                pct_empty_library: acc_empty_lib[i] / n,
                min_available_mana: if acc_mana_min[i] == f64::MAX {
                    0.0
                } else {
                    acc_mana_min[i]
                },
                max_available_mana: if acc_mana_max[i] == f64::MIN {
                    0.0
                } else {
                    acc_mana_max[i]
                },
                stddev_available_mana: variance.max(0.0).sqrt(),
            }
        })
        .collect();

    let oh = if games_simulated > 0 {
        OpeningHandStats {
            avg_lands: oh_lands / n,
            avg_spells: (oh_hand_size - oh_lands) / n,
            avg_hand_size: oh_hand_size / n,
            hand_size_distribution: oh_hand_dist,
            avg_mulligans: oh_mulligans / n,
            pct_keep_first: oh_keep_first as f64 / n,
        }
    } else {
        OpeningHandStats::default()
    };

    SimulationResult {
        turns,
        opening_hand: oh,
        games_simulated,
        config: config.clone(),
    }
}
