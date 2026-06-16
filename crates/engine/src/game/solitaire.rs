//! Lightweight single-player playtest session for deck goldfishing.
//!
//! `PlaytestSession` models just enough of a Magic turn to support interactive
//! hand simulation: shuffling, drawing, playing lands, casting spells, advancing
//! through turns with cleanup discard. It is intentionally thinner than the
//! full `GameState` — no stack, no triggers, no priority — because playtesting
//! only needs to answer "can I play my hand?" rather than "do the rules resolve
//! correctly?". Full rules correctness is validated in real games; this module
//! gives deck builders fast feedback on curve and mana consistency.
//!
//! For Monte Carlo aggregate statistics, see [`crate::game::playtest_stats`].

use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use crate::types::card::CardFace;
use crate::types::card_type::CoreType;

// ── Card identity ─────────────────────────────────────────────────────────────

/// Unique handle for an object inside a `PlaytestSession`. Starts at 1 and
/// increments monotonically across the session lifetime, so ids never alias
/// across zones even after cards move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlaytestObjectId(pub u32);

// ── Per-card slot ─────────────────────────────────────────────────────────────

/// A card tracked in the hand, battlefield, graveyard, or exile of a
/// `PlaytestSession`. Carries its printed face and a stable runtime id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaytestCard {
    pub id: PlaytestObjectId,
    pub face: CardFace,
}

impl PlaytestCard {
    fn new(id: PlaytestObjectId, face: CardFace) -> Self {
        Self { id, face }
    }
}

// ── Battlefield permanent ─────────────────────────────────────────────────────

/// A permanent on the solitaire battlefield. Tracks tapped state so the UI
/// can render tapped/untapped and the mana counter can exclude tapped sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaytestPermanent {
    pub card: PlaytestCard,
    /// True when the permanent is tapped (CR 701.20).
    pub tapped: bool,
    /// True for lands played this turn (treated as summoning-sick proxies).
    pub entered_this_turn: bool,
}

// ── Turn snapshot ─────────────────────────────────────────────────────────────

/// Metrics captured at the end of each turn's draw step, before any plays.
/// Used to build the turn history timeline displayed in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSnapshot {
    pub turn_number: u32,
    /// Cards in hand immediately after drawing for the turn.
    pub hand_size: usize,
    /// Total lands on the battlefield at the start of the turn.
    pub lands_in_play: usize,
    /// Total mana sources on the battlefield (lands + mana dorks — approximated
    /// as any non-land permanent whose oracle text contains "add {" ).
    pub mana_sources_in_play: usize,
    /// Approximate maximum available mana (untapped sources).
    pub available_mana: usize,
    /// Cards drawn this turn (1 after mulligan, 0 on turn 1 when going first).
    pub cards_drawn: u32,
    /// Number of lands in hand at start of turn (before playing one).
    pub lands_in_hand: usize,
    /// Approximate "playable" cards: non-land cards whose CMC ≤ available_mana.
    pub playable_count: usize,
}

// ── Main session ──────────────────────────────────────────────────────────────

/// Lightweight solitaire playtest session. Owns:
///
/// - A shuffled library (remaining deck cards).
/// - A hand (drawn cards).
/// - A solitaire battlefield (permanents with tapped state).
/// - A graveyard.
/// - An exile pile.
/// - Turn tracking and per-turn snapshots.
///
/// All mutations return `&mut Self` for fluent chaining in the WASM bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaytestSession {
    /// Original deck, used for reset.
    deck: Vec<CardFace>,
    /// Current seed, used for reset / deterministic replay.
    seed: u64,

    /// Remaining cards (top = index 0).
    pub library: Vec<PlaytestCard>,
    /// Cards in hand.
    pub hand: Vec<PlaytestCard>,
    /// Permanents on the battlefield.
    pub battlefield: Vec<PlaytestPermanent>,
    /// Cards in the graveyard.
    pub graveyard: Vec<PlaytestCard>,
    /// Cards in exile.
    pub exile: Vec<PlaytestCard>,

    /// Current turn number (starts at 1).
    pub turn_number: u32,
    /// Whether a land has been played this turn (CR 305.2).
    pub land_played_this_turn: bool,
    /// Whether the draw step has been performed this turn.
    pub drew_this_turn: bool,
    /// True when the session is in the mulligan phase (before turn 1 actions).
    pub in_mulligan: bool,
    /// How many mulligans have been taken (for bottoming count).
    pub mulligan_count: u8,
    /// Cards to be placed on the bottom as part of a Scry/London-mulligan bottom.
    pub bottoming_required: u8,

    /// Next object id to assign.
    next_id: u32,

    /// Per-turn snapshots (one per completed turn).
    pub history: Vec<TurnSnapshot>,

    /// Whether the human is going first (no draw on turn 1, CR 103.7a).
    pub going_first: bool,
}

impl PlaytestSession {
    // ── Construction ────────────────────────────────────────────────────────

    /// Create a new session from a flat deck list (may contain duplicates).
    /// The deck is shuffled using a seeded RNG for reproducibility.
    pub fn new(deck: Vec<CardFace>, seed: u64, going_first: bool) -> Self {
        let mut session = Self {
            deck: deck.clone(),
            seed,
            library: Vec::new(),
            hand: Vec::new(),
            battlefield: Vec::new(),
            graveyard: Vec::new(),
            exile: Vec::new(),
            turn_number: 0,
            land_played_this_turn: false,
            drew_this_turn: false,
            in_mulligan: true,
            mulligan_count: 0,
            bottoming_required: 0,
            next_id: 1,
            history: Vec::new(),
            going_first,
        };
        session.shuffle_and_deal_opening_hand();
        session
    }

    /// Reset the session to a fresh shuffled game with the same deck and seed.
    pub fn reset(&mut self) {
        *self = Self::new(self.deck.clone(), self.seed, self.going_first);
    }

    /// Reset with a new seed (for rerunning with different variance).
    pub fn reset_with_seed(&mut self, seed: u64) {
        self.seed = seed;
        self.reset();
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    fn next_id(&mut self) -> PlaytestObjectId {
        let id = PlaytestObjectId(self.next_id);
        self.next_id += 1;
        id
    }

    fn shuffle_and_deal_opening_hand(&mut self) {
        let mut rng = ChaCha20Rng::seed_from_u64(self.seed);
        let mut cards = self.deck.clone();
        cards.shuffle(&mut rng);
        self.library = cards
            .into_iter()
            .map(|face| {
                let id = self.next_id();
                PlaytestCard::new(id, face)
            })
            .collect();

        // Deal 7 cards into hand.
        for _ in 0..7 {
            if let Some(card) = self.library.first().cloned() {
                self.library.remove(0);
                self.hand.push(card);
            }
        }
        self.in_mulligan = true;
        self.mulligan_count = 0;
        self.bottoming_required = 0;
    }

    /// Return true if this card face represents a land (CR 305.1).
    pub(crate) fn is_land(face: &CardFace) -> bool {
        face.card_type.core_types.contains(&CoreType::Land)
    }

    /// Rough mana-source heuristic: lands plus creatures/artifacts whose oracle
    /// text starts with "T: Add" or contains "add {" (mana dorks / rocks).
    fn is_mana_source(face: &CardFace) -> bool {
        if Self::is_land(face) {
            return true;
        }
        let oracle = face.oracle_text.as_deref().unwrap_or("").to_lowercase();
        oracle.contains("add {") || oracle.starts_with("{t}: add")
    }

    fn count_untapped_mana_sources(&self) -> usize {
        self.battlefield
            .iter()
            .filter(|p| !p.tapped && Self::is_mana_source(&p.card.face))
            .count()
    }

    fn count_lands_in_play(&self) -> usize {
        self.battlefield
            .iter()
            .filter(|p| Self::is_land(&p.card.face))
            .count()
    }

    fn capture_snapshot(&self, cards_drawn: u32) -> TurnSnapshot {
        let available_mana = self.count_untapped_mana_sources();
        let lands_in_hand = self.hand.iter().filter(|c| Self::is_land(&c.face)).count();
        let playable_count = self
            .hand
            .iter()
            .filter(|c| {
                !Self::is_land(&c.face) && c.face.mana_cost.mana_value() as usize <= available_mana
            })
            .count();
        TurnSnapshot {
            turn_number: self.turn_number,
            hand_size: self.hand.len(),
            lands_in_play: self.count_lands_in_play(),
            mana_sources_in_play: self
                .battlefield
                .iter()
                .filter(|p| Self::is_mana_source(&p.card.face))
                .count(),
            available_mana,
            cards_drawn,
            lands_in_hand,
            playable_count,
        }
    }

    // ── Mulligan ─────────────────────────────────────────────────────────────

    /// Take a mulligan: put all cards from hand to bottom of library, reshuffle
    /// (London mulligan — CR 103.5), deal a new 7, increment mulligan count.
    /// Returns an error if mulligans are exhausted (CR 103.5: down to 0 cards).
    pub fn take_mulligan(&mut self) -> Result<(), String> {
        if !self.in_mulligan {
            return Err("Mulligan is only available before the first turn".to_string());
        }
        if self.mulligan_count >= 7 {
            return Err("No more mulligans available".to_string());
        }

        // Return hand to library bottom, then reshuffle.
        let hand_cards = std::mem::take(&mut self.hand);
        self.library.extend(hand_cards);

        let mut rng =
            ChaCha20Rng::seed_from_u64(self.seed.wrapping_add(self.mulligan_count as u64 + 1));
        self.library.shuffle(&mut rng);

        // Deal 7 new cards.
        for _ in 0..7 {
            if let Some(card) = self.library.first().cloned() {
                self.library.remove(0);
                self.hand.push(card);
            }
        }

        self.mulligan_count += 1;
        // London mulligan: put mulligan_count cards on the bottom after keeping.
        self.bottoming_required = self.mulligan_count;
        Ok(())
    }

    /// Bottom a card from hand (for London-mulligan bottom step, CR 103.5).
    pub fn bottom_card(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        if self.bottoming_required == 0 {
            return Err("No cards need to be bottomed".to_string());
        }
        let pos = self
            .hand
            .iter()
            .position(|c| c.id == card_id)
            .ok_or_else(|| format!("Card {:?} not in hand", card_id))?;
        let card = self.hand.remove(pos);
        self.library.push(card);
        self.bottoming_required -= 1;
        Ok(())
    }

    /// Keep the current hand and transition out of mulligan phase.
    /// If `bottoming_required > 0` this returns an error — the user must bottom
    /// cards first via `bottom_card`.
    pub fn keep_hand(&mut self) -> Result<(), String> {
        if !self.in_mulligan {
            return Err("Not in mulligan phase".to_string());
        }
        if self.bottoming_required > 0 {
            return Err(format!(
                "Must bottom {} card(s) before keeping",
                self.bottoming_required
            ));
        }
        self.in_mulligan = false;
        self.turn_number = 1;
        // Going first: no draw on turn 1 (CR 103.7a). Going second: draw at
        // the start of turn 1, handled by `advance_turn`.
        if !self.going_first {
            // Simulate drawing 1 card as the "going second" advantage draw.
            self.draw_one();
        }
        let snapshot = self.capture_snapshot(if self.going_first { 0 } else { 1 });
        self.history.push(snapshot);
        Ok(())
    }

    // ── Turn actions ─────────────────────────────────────────────────────────

    /// Draw one card from the library into hand. Returns `Ok(card_id)` or an
    /// error when the library is empty (would lose to mill, CR 704.5b — but
    /// in playtest mode we simply report the error rather than ending the game).
    pub fn draw_one(&mut self) -> Option<PlaytestObjectId> {
        if let Some(card) = self.library.first().cloned() {
            let id = card.id;
            self.library.remove(0);
            self.hand.push(card);
            Some(id)
        } else {
            None
        }
    }

    /// Manually draw N cards (for effects like Ancestral Recall in goldfishing).
    pub fn draw_n(&mut self, n: u32) -> Vec<PlaytestObjectId> {
        (0..n).filter_map(|_| self.draw_one()).collect()
    }

    /// Play a land from hand to battlefield. Enforces the one-land-per-turn rule
    /// (CR 305.2). Returns an error if:
    /// - The specified card is not in hand.
    /// - The card is not a land.
    /// - A land has already been played this turn.
    pub fn play_land(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        if self.in_mulligan {
            return Err("Cannot play lands during mulligan".to_string());
        }
        if self.land_played_this_turn {
            return Err("Already played a land this turn (CR 305.2)".to_string());
        }
        let pos = self
            .hand
            .iter()
            .position(|c| c.id == card_id)
            .ok_or_else(|| format!("Card {:?} not in hand", card_id))?;
        if !Self::is_land(&self.hand[pos].face) {
            return Err("That card is not a land".to_string());
        }
        let card = self.hand.remove(pos);
        self.battlefield.push(PlaytestPermanent {
            card,
            tapped: false,
            entered_this_turn: true,
        });
        self.land_played_this_turn = true;
        Ok(())
    }

    /// Move a non-land card from hand to the battlefield (simulating casting
    /// without full rules processing). Useful for goldfishing creatures and
    /// artifacts where the user just wants to track board state.
    pub fn cast_permanent(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        if self.in_mulligan {
            return Err("Cannot cast during mulligan".to_string());
        }
        let pos = self
            .hand
            .iter()
            .position(|c| c.id == card_id)
            .ok_or_else(|| format!("Card {:?} not in hand", card_id))?;
        let face = &self.hand[pos].face;
        // Only permanents go to the battlefield; instants/sorceries go to graveyard.
        let is_permanent = face.card_type.core_types.iter().any(|t| {
            matches!(
                t,
                CoreType::Creature
                    | CoreType::Artifact
                    | CoreType::Enchantment
                    | CoreType::Land
                    | CoreType::Planeswalker
                    | CoreType::Battle
            )
        });
        let card = self.hand.remove(pos);
        if is_permanent {
            self.battlefield.push(PlaytestPermanent {
                card,
                tapped: false,
                entered_this_turn: true,
            });
        } else {
            // Instant or sorcery — goes to graveyard after resolution.
            self.graveyard.push(card);
        }
        Ok(())
    }

    /// Move a card from hand directly to the graveyard (discard).
    pub fn discard(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        let pos = self
            .hand
            .iter()
            .position(|c| c.id == card_id)
            .ok_or_else(|| format!("Card {:?} not in hand", card_id))?;
        let card = self.hand.remove(pos);
        self.graveyard.push(card);
        Ok(())
    }

    /// Tap a permanent on the battlefield (e.g., to tap a land for mana or
    /// declare an attacker). Returns an error if already tapped or not found.
    pub fn tap_permanent(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        let perm = self
            .battlefield
            .iter_mut()
            .find(|p| p.card.id == card_id)
            .ok_or_else(|| format!("Permanent {:?} not on battlefield", card_id))?;
        if perm.tapped {
            return Err("Permanent is already tapped".to_string());
        }
        perm.tapped = true;
        Ok(())
    }

    /// Untap a permanent (e.g., with Seedborn Muse effect during goldfishing).
    pub fn untap_permanent(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        let perm = self
            .battlefield
            .iter_mut()
            .find(|p| p.card.id == card_id)
            .ok_or_else(|| format!("Permanent {:?} not on battlefield", card_id))?;
        perm.tapped = false;
        Ok(())
    }

    /// Move a permanent from the battlefield to the graveyard (destroy/die).
    pub fn destroy_permanent(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        let pos = self
            .battlefield
            .iter()
            .position(|p| p.card.id == card_id)
            .ok_or_else(|| format!("Permanent {:?} not on battlefield", card_id))?;
        let perm = self.battlefield.remove(pos);
        self.graveyard.push(perm.card);
        Ok(())
    }

    /// Move a permanent from the battlefield to exile.
    pub fn exile_permanent(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        let pos = self
            .battlefield
            .iter()
            .position(|p| p.card.id == card_id)
            .ok_or_else(|| format!("Permanent {:?} not on battlefield", card_id))?;
        let perm = self.battlefield.remove(pos);
        self.exile.push(perm.card);
        Ok(())
    }

    /// Bounce a permanent from battlefield back to hand.
    pub fn bounce_permanent(&mut self, card_id: PlaytestObjectId) -> Result<(), String> {
        let pos = self
            .battlefield
            .iter()
            .position(|p| p.card.id == card_id)
            .ok_or_else(|| format!("Permanent {:?} not on battlefield", card_id))?;
        let perm = self.battlefield.remove(pos);
        self.hand.push(perm.card);
        Ok(())
    }

    /// Advance to the next turn. Performs (in order):
    /// 1. End step: if hand size > 7, the session marks `cleanup_discard_needed` so
    ///    the UI can prompt for discards.
    /// 2. Untap step: all permanents untap (CR 502.2).
    /// 3. Draw step: active player draws one card (CR 504.1).
    /// 4. Capture turn snapshot.
    ///
    /// Returns the cards that must still be discarded to reach hand size 7, if any.
    /// The caller is responsible for calling `discard` for each of those cards before
    /// they can call `advance_turn` again (enforced via `cleanup_discard_needed`).
    pub fn advance_turn(&mut self) -> Result<AdvanceTurnResult, String> {
        if self.in_mulligan {
            return Err("Must keep or mulligan before advancing turns".to_string());
        }

        // CR 514.1: Cleanup — discard to hand size 7 (maximum hand size CR 402.2).
        let max_hand = 7usize;
        if self.hand.len() > max_hand {
            return Err(format!(
                "Must discard {} card(s) to hand size 7 before advancing (CR 514.1)",
                self.hand.len() - max_hand
            ));
        }

        // CR 502.2: Untap all permanents.
        for perm in &mut self.battlefield {
            perm.tapped = false;
            perm.entered_this_turn = false;
        }

        self.turn_number += 1;
        self.land_played_this_turn = false;
        self.drew_this_turn = false;

        // CR 504.1: Draw a card at the beginning of the draw step.
        let drew = self.draw_one().is_some();
        if drew {
            self.drew_this_turn = true;
        }

        let snapshot = self.capture_snapshot(u32::from(drew));
        self.history.push(snapshot.clone());

        Ok(AdvanceTurnResult {
            turn_number: self.turn_number,
            drew,
            snapshot,
        })
    }

    // ── Derived views ────────────────────────────────────────────────────────

    /// Available mana this turn (untapped sources on battlefield).
    pub fn available_mana(&self) -> usize {
        self.count_untapped_mana_sources()
    }

    /// Lands in hand.
    pub fn lands_in_hand(&self) -> usize {
        self.hand.iter().filter(|c| Self::is_land(&c.face)).count()
    }

    /// Non-land cards in hand whose CMC ≤ available_mana (playable this turn).
    pub fn playable_in_hand(&self) -> Vec<PlaytestObjectId> {
        let mana = self.available_mana();
        self.hand
            .iter()
            .filter(|c| !Self::is_land(&c.face) && c.face.mana_cost.mana_value() as usize <= mana)
            .map(|c| c.id)
            .collect()
    }

    /// True when the hand is over 7 and a cleanup discard is needed.
    pub fn needs_cleanup_discard(&self) -> bool {
        self.hand.len() > 7
    }

    /// Number of cards to discard for cleanup.
    pub fn discard_count_needed(&self) -> usize {
        self.hand.len().saturating_sub(7)
    }
}

// ── Advance-turn result ───────────────────────────────────────────────────────

/// Return value of `PlaytestSession::advance_turn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvanceTurnResult {
    pub turn_number: u32,
    /// Whether a card was successfully drawn from a non-empty library.
    pub drew: bool,
    pub snapshot: TurnSnapshot,
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::mana::ManaCost;

    fn make_land(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Land],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn make_creature(name: &str, cmc: u32) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            mana_cost: ManaCost::generic(cmc),
            ..Default::default()
        }
    }

    fn deck_60() -> Vec<CardFace> {
        let mut deck = Vec::new();
        for i in 0..24 {
            deck.push(make_land(&format!("Forest {i}")));
        }
        for i in 0..36 {
            deck.push(make_creature(&format!("Llanowar Elves {i}"), 1));
        }
        deck
    }

    #[test]
    fn opening_hand_has_seven_cards() {
        let session = PlaytestSession::new(deck_60(), 42, true);
        assert_eq!(session.hand.len(), 7);
    }

    #[test]
    fn library_shrinks_on_draw() {
        let mut session = PlaytestSession::new(deck_60(), 42, true);
        session.keep_hand().unwrap();
        let before = session.library.len();
        session.advance_turn().unwrap();
        assert_eq!(session.library.len(), before - 1);
    }

    #[test]
    fn one_land_per_turn_enforced() {
        let mut session = PlaytestSession::new(deck_60(), 42, true);
        session.keep_hand().unwrap();
        // Find a land in hand.
        let land_id = session
            .hand
            .iter()
            .find(|c| PlaytestSession::is_land(&c.face))
            .map(|c| c.id);
        if let Some(id) = land_id {
            session.play_land(id).unwrap();
            // Find another land.
            let land_id2 = session
                .hand
                .iter()
                .find(|c| PlaytestSession::is_land(&c.face))
                .map(|c| c.id);
            if let Some(id2) = land_id2 {
                assert!(session.play_land(id2).is_err());
            }
        }
    }

    #[test]
    fn advance_turn_resets_land_played() {
        let mut session = PlaytestSession::new(deck_60(), 42, true);
        session.keep_hand().unwrap();
        let land_id = session
            .hand
            .iter()
            .find(|c| PlaytestSession::is_land(&c.face))
            .map(|c| c.id);
        if let Some(id) = land_id {
            session.play_land(id).unwrap();
            assert!(session.land_played_this_turn);
        }
        session.advance_turn().unwrap();
        assert!(!session.land_played_this_turn);
    }

    #[test]
    fn mulligan_redeals_seven() {
        let mut session = PlaytestSession::new(deck_60(), 42, true);
        assert!(session.in_mulligan);
        session.take_mulligan().unwrap();
        assert_eq!(session.hand.len(), 7);
        assert_eq!(session.mulligan_count, 1);
        assert_eq!(session.bottoming_required, 1);
    }

    #[test]
    fn keep_after_mulligan_requires_bottoming() {
        let mut session = PlaytestSession::new(deck_60(), 42, true);
        session.take_mulligan().unwrap();
        // Must bottom 1 card before keeping.
        assert!(session.keep_hand().is_err());
        let id = session.hand[0].id;
        session.bottom_card(id).unwrap();
        assert!(session.keep_hand().is_ok());
    }

    #[test]
    fn reset_returns_to_fresh_state() {
        let mut session = PlaytestSession::new(deck_60(), 42, true);
        session.keep_hand().unwrap();
        session.advance_turn().unwrap();
        session.reset();
        assert_eq!(session.turn_number, 0);
        assert!(session.in_mulligan);
        assert_eq!(session.hand.len(), 7);
    }
}
