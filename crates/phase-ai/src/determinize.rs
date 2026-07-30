//! Determinized opponent sampling for the AI planning path.
//!
//! The engine's authoritative `GameState` carries every player's *real* hidden
//! zones (hand + library). If the AI search reads them during simulation it is
//! cheating with perfect information. This module produces an
//! **AI-simulation-only** clone in which every opponent's genuinely-unknown
//! hidden-zone cards are resampled to a plausible assignment consistent with
//! public information + the sanctioned `deck_knowledge` model + engine reveal
//! sets. The untouched beam/rollout search then runs on the sampled state, and
//! the ensemble wrapper (`search::score_candidates_with_session`) averages
//! across K samples.
//!
//! CR 400.2 (hand and library are hidden zones), CR 401.2 (library order is
//! unknown), CR 401.3 / CR 402.1 (library and hand SIZE are public — preserved
//! exactly), CR 701.20a (revealed cards are known to all players — pinned),
//! CR 701.20e (a looked-at card is known only to the looking player).
//!
//! # Why the AI's own zones are never resampled
//!
//! Determinization leaves the AI player's OWN hand/library — and all public
//! zones — **byte-identical**. This is what makes own-hidden-zone candidate
//! enumeration safe: candidate generators that read the AI's own hidden cards by
//! identity (e.g. `card_name_choice_candidates`, `candidates.rs:4053`, or any
//! cast-from-hand enumeration) see exactly the real cards, so the K sampled
//! worlds share an identical AI-legal-action set. Only cards the AI cannot
//! legitimately know are swapped, which is precisely the set no AI candidate can
//! reference by identity (see the pin-invariant in `search.rs`).
//!
//! # Identity-swap caveat (residual fields not rewritten by the primitive)
//!
//! `apply_card_face_to_object` (the engine's in-place face-application
//! primitive) overwrites the castability-relevant characteristics —
//! `name`, `power`/`toughness`/`loyalty`/`defense` (+ `base_*` mirrors),
//! `card_types`, `mana_cost`, `keywords`, `abilities`/`triggers`/`replacements`/
//! `static_definitions` (+ `base_*`), `color`, `printed_ref`/`base_printed_ref`,
//! `casting_restrictions`, `casting_options`, `modal`, `additional_cost`,
//! `strive_cost`, `cleave_variant`, `spellbook`, and (conditionally)
//! `class_level`/`intensity`/`case_state`/`room_unlocks`/`attraction_lights`. It
//! also sets `base_characteristics_initialized = true`. It does NOT rewrite the
//! following residual fields, each verified inert in the v1 score path:
//!
//! | Residual field | Why inert in the v1 score path |
//! |---|---|
//! | `card_id` | The score path performs zero `card_id`-keyed `CardDatabase` lookups; cast actions read the object's (overwritten) characteristics self-consistently, so a stale id never resolves the object back to its real identity. A `debug_assert` below documents this invariant. |
//! | `back_face` | Needs a `CardDatabase` rehydrate not threaded into scoring; front-face cost/abilities are correct, alternate-face casting stays inert (P6-followup-B). |
//! | `perpetual_mods` / `intensity` | Persist across hidden zones by explicit engine design (`game_object.rs`); zeroing them would fight that invariant. Rare (digital-only Alchemy), not read by hidden-zone candidate gen/eval in v1. |
//! | `counters` / `stickers` | Not carried by hidden-zone cards in normal play; candidate gen/eval read them only for battlefield permanents. |
//! | `casting_permissions` | Governs whether a specific object may be cast; no AI candidate references a resampled unknown card by identity (pin-invariant), so a stale permission cannot enable an illegal cheat candidate. |
//!
//! # Shipping status (2026-07-18)
//!
//! All difficulty tier presets ship `determinization_samples = 0`
//! (perfect-information search) as a deliberate strength decision while players
//! win ~80% of games vs the AI. This module remains the config-reachable
//! sampling primitive for experiments and measurement runs — reached by setting
//! `SearchConfig::determinization_samples > 0` directly — and is exercised by the
//! `search.rs` ensemble tests, which set K manually.

use std::collections::HashSet;

use rand::seq::SliceRandom;
use rand_chacha::ChaCha20Rng;

use engine::game::derived::sync_continuous_reveals;
use engine::game::players::opponents;
use engine::game::printed_cards::apply_card_face_to_object;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::deck_knowledge;

/// Produce an AI-simulation-only clone of `state` in which every opponent's
/// genuinely-unknown hidden-zone cards are resampled. The AI player's own zones,
/// all public zones, and all revealed/known cards are left byte-identical.
///
/// CR 400.2 / CR 401.2 / CR 701.20a — see the module docs.
pub fn determinize_opponents(
    state: &GameState,
    ai_player: PlayerId,
    rng: &mut ChaCha20Rng,
) -> GameState {
    let mut sim = state.clone();

    // CR 400.2 / CR 701.20a: `revealed_cards` is a DERIVED field that
    // `apply_action` clears at each action boundary; only a derive pass
    // repopulates continuous "play with X revealed" statics. The score-path
    // state is not guaranteed to have been derived, so re-sync here (engine owns
    // the visibility rule) BEFORE pinning — otherwise a statically-revealed
    // opponent card could be wrongly resampled. We deliberately avoid the full
    // `derive_display_state` (it runs an expensive board-global mana sweep, wrong
    // layer + K x cost); the dedicated sync is cheap and sufficient.
    sync_continuous_reveals(&mut sim);

    // CR 400.2 / CR 701.20a / CR 701.20e: the set of ids `ai_player` legitimately
    // knows is opponent-independent (global reveal sets + AI's own private
    // looks), so compute it once.
    let known = pinned_known_ids(&sim, ai_player);

    for opponent in opponents(&sim, ai_player) {
        // Ordered unknown slots (hand slots then library slots, in zone order).
        let slots = unknown_slots(&sim, opponent, &known);
        // Candidate faces from the opponent's decklist (decklist order).
        let mut pool = deck_knowledge::unknown_hidden_pool(&sim, opponent, &known);
        if pool.is_empty() || slots.is_empty() {
            // No decklist / nothing unknown for this opponent: keep real state.
            continue;
        }

        // CR 401.2: which unknown card sits in the hand vs. the library is itself
        // unknown, so hand and library slots draw from ONE shuffled pool.
        pool.shuffle(rng);
        for (slot, face) in slots.iter().zip(pool.iter()) {
            if let Some(obj) = sim.objects.get_mut(slot) {
                debug_assert!(
                    obj.zone == Zone::Hand || obj.zone == Zone::Library,
                    "determinizer only overwrites hidden-zone objects"
                );
                // The stale `card_id` (see module docs) is never resolved back to
                // the real card by the score path — cast actions read these
                // overwritten characteristics self-consistently.
                apply_card_face_to_object(obj, face);
            }
        }
        // `slots.len()` may exceed `pool.len()` only on decklist inconsistency
        // (e.g. copies/tokens counted elsewhere); `zip` truncates and the
        // trailing slots keep their real identity — a rare, bounded residual.
    }

    sim
}

/// CR 400.2 / CR 701.20a / CR 701.20e: the set of object ids whose identity
/// `ai_player` legitimately knows. Read from engine-derived reveal state (after
/// `sync_continuous_reveals`). Membership-only — never iterated for output order
/// (the #4878 determinism discipline).
fn pinned_known_ids(state: &GameState, ai_player: PlayerId) -> HashSet<ObjectId> {
    let mut ids: HashSet<ObjectId> = HashSet::new();
    // Continuous + momentary reveals (post-sync).
    ids.extend(state.revealed_cards.iter().copied());
    // One-shot reveals, never cleared.
    ids.extend(state.public_revealed_cards.iter().copied());
    // CR 701.20e: a looked-at card is known only to the looking player.
    if state.private_look_player == Some(ai_player) {
        ids.extend(state.private_look_ids.iter().copied());
    }
    ids
}

/// Ordered unknown hidden-zone slots for `opponent`: its `hand` (in order) then
/// its `library` (in order), skipping cards `ai_player` legitimately knows
/// (`known`), tokens, and cards not owned by `opponent` (a borrowed card's
/// identity is already public). Both zones are ordered `im::Vector`s, so the
/// slot order is deterministic (#4878 discipline).
fn unknown_slots(
    state: &GameState,
    opponent: PlayerId,
    known: &HashSet<ObjectId>,
) -> Vec<ObjectId> {
    let player = &state.players[opponent.0 as usize];
    player
        .hand
        .iter()
        .chain(player.library.iter())
        .copied()
        .filter(|id| {
            if known.contains(id) {
                return false;
            }
            state
                .objects
                .get(id)
                .is_some_and(|obj| !obj.is_token && obj.owner == opponent)
        })
        .collect()
}

/// SplitMix64 finalizer — mixes the per-sample index into the ensemble seed so
/// samples `0..K` draw distinct assignments from the same base seed. Standard
/// constants (Steele et al.).
pub(crate) fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::StaticDefinition;
    use engine::types::card::CardFace;
    use engine::types::counter::CounterType;
    use engine::types::game_state::PlayerDeckPool;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::statics::{ProhibitionScope, StaticMode};
    use rand::SeedableRng;
    use std::sync::Arc;

    use engine::game::deck_loading::DeckEntry;

    fn face(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::zero(),
            ..Default::default()
        }
    }

    fn deck_entry(name: &str, count: u32) -> DeckEntry {
        DeckEntry {
            card: face(name),
            count,
        }
    }

    fn set_deck(state: &mut GameState, player: PlayerId, entries: Vec<DeckEntry>) {
        state.deck_pools.push(PlayerDeckPool {
            player,
            current_main: Arc::new(entries),
            ..Default::default()
        });
    }

    fn add(state: &mut GameState, owner: PlayerId, name: &str, zone: Zone) -> ObjectId {
        create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            zone,
        )
    }

    fn name_of(state: &GameState, id: ObjectId) -> String {
        state.objects[&id].name.clone()
    }

    fn rng(seed: u64) -> ChaCha20Rng {
        ChaCha20Rng::seed_from_u64(seed)
    }

    /// A1: public + AI-own zones preserved; at least one opponent hidden card
    /// actually changed (guards against a vacuous "nothing moved" pass).
    #[test]
    fn preserves_public_and_own_zones() {
        let mut state = GameState::new_two_player(42);
        set_deck(
            &mut state,
            PlayerId(1),
            vec![
                deck_entry("Alpha", 1),
                deck_entry("Beta", 1),
                deck_entry("Gamma", 1),
                deck_entry("Delta", 1),
            ],
        );
        let opp_public = add(&mut state, PlayerId(1), "PublicCreature", Zone::Battlefield);
        let opp_h1 = add(&mut state, PlayerId(1), "HiddenA", Zone::Hand);
        let opp_h2 = add(&mut state, PlayerId(1), "HiddenB", Zone::Hand);
        let opp_lib = add(&mut state, PlayerId(1), "HiddenC", Zone::Library);
        let my_hand = add(&mut state, PlayerId(0), "MyCard", Zone::Hand);
        let my_lib = add(&mut state, PlayerId(0), "MyLib", Zone::Library);

        let sim = determinize_opponents(&state, PlayerId(0), &mut rng(1));

        // Public + own zones byte-identical (by identity name).
        assert_eq!(name_of(&sim, opp_public), "PublicCreature");
        assert_eq!(name_of(&sim, my_hand), "MyCard");
        assert_eq!(name_of(&sim, my_lib), "MyLib");
        // Reach-guard: at least one opponent hidden card was resampled.
        let changed = [opp_h1, opp_h2, opp_lib]
            .iter()
            .filter(|&&id| {
                !matches!(
                    name_of(&sim, id).as_str(),
                    "HiddenA" | "HiddenB" | "HiddenC"
                )
            })
            .count();
        assert!(
            changed >= 1,
            "expected >=1 opponent hidden card to be resampled"
        );
        // Every resampled identity is drawn from the decklist.
        for &id in &[opp_h1, opp_h2, opp_lib] {
            let n = name_of(&sim, id);
            assert!(
                ["Alpha", "Beta", "Gamma", "Delta"].contains(&n.as_str()),
                "resampled name {n} not from decklist"
            );
        }
    }

    /// A2 (CR 401.3 / CR 402.1): hidden-zone SIZES preserved; empty hand/library
    /// is a no-op, not a panic.
    #[test]
    fn preserves_hidden_zone_sizes() {
        let mut state = GameState::new_two_player(42);
        set_deck(&mut state, PlayerId(1), vec![deck_entry("Alpha", 3)]);
        add(&mut state, PlayerId(1), "HiddenA", Zone::Hand);
        add(&mut state, PlayerId(1), "HiddenB", Zone::Library);

        let hand_len = state.players[1].hand.len();
        let lib_len = state.players[1].library.len();
        let sim = determinize_opponents(&state, PlayerId(0), &mut rng(2));
        assert_eq!(sim.players[1].hand.len(), hand_len);
        assert_eq!(sim.players[1].library.len(), lib_len);

        // Opponent with empty hidden zones: no panic, no change.
        let mut empty = GameState::new_two_player(42);
        set_deck(&mut empty, PlayerId(1), vec![deck_entry("Alpha", 1)]);
        let sim2 = determinize_opponents(&empty, PlayerId(0), &mut rng(3));
        assert_eq!(sim2.players[1].hand.len(), 0);
        assert_eq!(sim2.players[1].library.len(), 0);
    }

    /// A3 (CR 400.2 / CR 701.20a / CR 701.20e): revealed / one-shot-revealed
    /// cards are pinned; a peeked card is pinned only for the looking player.
    #[test]
    fn pins_revealed_cards() {
        let mut state = GameState::new_two_player(42);
        set_deck(
            &mut state,
            PlayerId(1),
            vec![deck_entry("Alpha", 1), deck_entry("Beta", 1)],
        );
        let pinned = add(&mut state, PlayerId(1), "RevealedCard", Zone::Hand);
        let free = add(&mut state, PlayerId(1), "HiddenCard", Zone::Hand);
        state.public_revealed_cards.insert(pinned);

        let sim = determinize_opponents(&state, PlayerId(0), &mut rng(4));
        assert_eq!(
            name_of(&sim, pinned),
            "RevealedCard",
            "revealed card must be pinned"
        );
        assert_ne!(
            name_of(&sim, free),
            "HiddenCard",
            "unrevealed card must resample"
        );

        // CR 701.20e: private look pins ONLY for the looking player.
        let mut peeked = state.clone();
        peeked.public_revealed_cards.clear();
        peeked.private_look_ids = vec![pinned];
        // Looker == AI -> pinned.
        peeked.private_look_player = Some(PlayerId(0));
        let sim_ai = determinize_opponents(&peeked, PlayerId(0), &mut rng(5));
        assert_eq!(name_of(&sim_ai, pinned), "RevealedCard");
        // Looker == opponent -> AI does NOT know it, so it resamples.
        peeked.private_look_player = Some(PlayerId(1));
        let sim_opp = determinize_opponents(&peeked, PlayerId(0), &mut rng(5));
        assert_ne!(name_of(&sim_opp, pinned), "RevealedCard");
    }

    /// A3b (CR 400.2 / CR 701.20a) — the F4 fix is load-bearing. A continuous
    /// reveal static on a NON-DERIVED input state must still pin the revealed
    /// card: the determinizer calls the engine-owned `sync_continuous_reveals`
    /// itself. If that call were removed, `RevealedByStatic` would be an unknown
    /// slot and get resampled -> the `assert_eq` below fails.
    #[test]
    fn pins_continuous_reveal_static_on_non_derived_state() {
        let mut state = GameState::new_two_player(42);
        set_deck(
            &mut state,
            PlayerId(1),
            vec![deck_entry("Alpha", 1), deck_entry("Beta", 1)],
        );
        // Opponent controls "play with your hand revealed" (RevealHand{Controller}).
        let revealer = add(&mut state, PlayerId(1), "Revealer", Zone::Battlefield);
        state
            .objects
            .get_mut(&revealer)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::RevealHand {
                who: ProhibitionScope::Controller,
            }));
        let revealed = add(&mut state, PlayerId(1), "RevealedByStatic", Zone::Hand);
        let unrevealed = add(&mut state, PlayerId(1), "HiddenInLibrary", Zone::Library);
        // Deliberately DO NOT call derive_display_state — `revealed_cards` is empty.
        assert!(state.revealed_cards.is_empty());

        let sim = determinize_opponents(&state, PlayerId(0), &mut rng(6));
        assert_eq!(
            name_of(&sim, revealed),
            "RevealedByStatic",
            "continuous-reveal-static card must be pinned via sync_continuous_reveals (F4)"
        );
        assert_ne!(
            name_of(&sim, unrevealed),
            "HiddenInLibrary",
            "a non-revealed hidden card must still resample (proves resampling ran)"
        );
    }

    /// A4 (conservation): every resampled identity is a decklist card; a token in
    /// a hidden zone is left untouched (no deck identity).
    #[test]
    fn samples_only_from_decklist_and_skips_tokens() {
        let mut state = GameState::new_two_player(42);
        set_deck(
            &mut state,
            PlayerId(1),
            vec![deck_entry("Alpha", 2), deck_entry("Beta", 2)],
        );
        let real = add(&mut state, PlayerId(1), "HiddenReal", Zone::Hand);
        let token = add(&mut state, PlayerId(1), "Goblin", Zone::Hand);
        state.objects.get_mut(&token).unwrap().is_token = true;

        let sim = determinize_opponents(&state, PlayerId(0), &mut rng(7));
        let n = name_of(&sim, real);
        assert!(
            ["Alpha", "Beta"].contains(&n.as_str()),
            "resampled from decklist"
        );
        assert_eq!(name_of(&sim, token), "Goblin", "token left untouched");
    }

    /// A5: seeding is deterministic (same seed -> same sample) and
    /// position-varying (different seeds -> generally different samples).
    #[test]
    fn seed_deterministic() {
        let mut state = GameState::new_two_player(42);
        set_deck(
            &mut state,
            PlayerId(1),
            vec![
                deck_entry("Alpha", 1),
                deck_entry("Beta", 1),
                deck_entry("Gamma", 1),
                deck_entry("Delta", 1),
            ],
        );
        for i in 0..4 {
            add(&mut state, PlayerId(1), &format!("Hidden{i}"), Zone::Hand);
        }
        let a = determinize_opponents(&state, PlayerId(0), &mut rng(99));
        let b = determinize_opponents(&state, PlayerId(0), &mut rng(99));
        let ids: Vec<_> = state.players[1].hand.iter().copied().collect();
        for id in ids {
            assert_eq!(name_of(&a, id), name_of(&b, id), "same seed -> same sample");
        }
    }

    /// A6: an opponent with no `deck_pools` entry is a no-op; a pool smaller than
    /// the unknown-slot set leaves trailing slots at their real identity.
    #[test]
    fn empty_pool_and_undersized_pool() {
        // No deck pool at all.
        let mut no_pool = GameState::new_two_player(42);
        let h = add(&mut no_pool, PlayerId(1), "HiddenA", Zone::Hand);
        let sim = determinize_opponents(&no_pool, PlayerId(0), &mut rng(8));
        assert_eq!(name_of(&sim, h), "HiddenA", "no decklist -> unchanged");

        // Pool smaller than slots.
        let mut small = GameState::new_two_player(42);
        set_deck(&mut small, PlayerId(1), vec![deck_entry("Alpha", 1)]);
        let s1 = add(&mut small, PlayerId(1), "HiddenA", Zone::Hand);
        let s2 = add(&mut small, PlayerId(1), "HiddenB", Zone::Hand);
        let sim2 = determinize_opponents(&small, PlayerId(0), &mut rng(9));
        // Exactly one slot resampled; the other keeps its real identity.
        let resampled = [s1, s2]
            .iter()
            .filter(|&&id| name_of(&sim2, id) == "Alpha")
            .count();
        assert_eq!(
            resampled, 1,
            "undersized pool resamples exactly pool.len() slots"
        );
    }

    /// A7 (F2): a stale residual field the primitive does not rewrite (a marked
    /// counter) is left as-is after a resample — documents the deliberate
    /// non-reset — while the card's identity fields DID change (resample ran).
    #[test]
    fn residual_fields_are_not_reset() {
        let mut state = GameState::new_two_player(42);
        set_deck(&mut state, PlayerId(1), vec![deck_entry("Alpha", 1)]);
        let hidden = add(&mut state, PlayerId(1), "HiddenA", Zone::Hand);
        let charge = CounterType::Generic("charge".to_string());
        state
            .objects
            .get_mut(&hidden)
            .unwrap()
            .counters
            .insert(charge.clone(), 3);

        let sim = determinize_opponents(&state, PlayerId(0), &mut rng(10));
        // Identity fields changed (resample ran).
        assert_eq!(name_of(&sim, hidden), "Alpha");
        // Residual counter deliberately preserved (not reset by the primitive).
        assert_eq!(
            sim.objects[&hidden].counters.get(&charge),
            Some(&3),
            "determinizer must not defensively reset residual counters (F2)"
        );
    }
}
