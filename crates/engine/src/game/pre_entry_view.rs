//! CR 603.6d + CR 614.12a + CR 113.6 + CR 611.3b: the board an as-enters roll is
//! made on. "As [this permanent] enters" is part of the event that puts the
//! permanent onto the battlefield, so its roll comes before that event: none of
//! the event's entrants is on the battlefield, their abilities do not function,
//! and their continuous effects do not apply. The engine delivers a simultaneous
//! event member by member, so the live board at the roll can hold earlier
//! entrants and the roller itself.
//!
//! Residual assumption: no one-shot die shield ("the next time you would roll",
//! Bamboozling Beeble, Squid Fire Knight, The Big Idea) and no Probability Flux
//! is implemented. A replacement that consumes itself would be consumed on the
//! view, so if one is added, its consumption must be handed to the live game.

use crate::game::replacement::{self, ReplacementResult};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::object_map::ObjectMap;
#[cfg(test)]
use crate::types::zones::Zone;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

type BoardSlot = Arc<Mutex<Option<CachedBoard>>>;

/// A synchronous optimization only. Copies of a game never share scratch
/// mutation, and a save never carries a second copy of the game authority.
#[derive(Default)]
pub(crate) struct PreEntryBoardCache(Option<BoardSlot>);

impl Clone for PreEntryBoardCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for PreEntryBoardCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PreEntryBoardCache")
            .field(&self.0.is_some())
            .finish()
    }
}

struct CachedBoard {
    board: Box<GameState>,
    original_objects: ObjectMap,
    projected_objects: ObjectMap,
}

impl CachedBoard {
    // CR 614.12: entries delivered earlier in this event are absent. Any
    // other changed, added, or removed object requires a fresh resident view.
    fn residents_unchanged(&self, state: &GameState, entrants: &HashSet<ObjectId>) -> bool {
        self.original_objects.iter().all(|(id, object)| {
            entrants.contains(id)
                || self
                    .original_objects
                    .shares_object_with(&state.objects, *id)
                || state.objects.get(id) == Some(object)
        }) && state
            .objects
            .keys()
            .all(|id| entrants.contains(id) || self.original_objects.contains_key(id))
    }
}

/// Run one whole copy-owner segment with its own reusable allocation. A pause
/// ends the scope; a nested copy owner temporarily replaces the outer slot.
/// A resumed single-source batch also owns a segment when no whole copy
/// owner is currently draining. Reuse an enclosing owner rather than reset it.
pub(crate) fn with_copy_batch_board<R>(
    state: &mut GameState,
    deliver: impl FnOnce(&mut GameState) -> R,
) -> R {
    if state.pre_entry_board_cache.0.is_some() {
        deliver(state)
    } else {
        with_copy_owner_board(state, deliver)
    }
}

pub(crate) fn with_copy_owner_board<R>(
    state: &mut GameState,
    deliver: impl FnOnce(&mut GameState) -> R,
) -> R {
    let outer = std::mem::replace(
        &mut state.pre_entry_board_cache,
        PreEntryBoardCache(Some(Arc::new(Mutex::new(None)))),
    );
    let result = deliver(state);
    state.pre_entry_board_cache = outer;
    result
}

/// CR 603.6d + CR 614.12a + CR 611.3b + CR 113.6: the live board with every
/// entrant of an as-enters roll's event absent. Each one on the battlefield is
/// phased out, the engine's encoding of a permanent "treated as though it does
/// not exist" (CR 702.26b), which every battlefield reader honors, and layers
/// are re-derived. A scratch value, never the game: the roll's proposal and its
/// observer collection read it, hand a typed outcome back, and drop it.
pub(crate) struct PreEntryRulesView {
    board: Option<Box<GameState>>,
    reuse: Option<(BoardSlot, ObjectMap, ObjectMap)>,
}

/// CR 616.1: what a replacement pipeline step run on the view hands back: its
/// result and, when it paused, the ordering prompt rendered from the view.
pub(crate) struct PreEntryReplacement {
    pub(crate) result: ReplacementResult,
    pub(crate) choice: Option<WaitingFor>,
}

impl PreEntryRulesView {
    /// CR 603.6a + CR 603.6d: the view for `roller`'s as-enters roll, or `None`
    /// outside a shared copy segment when no entrant is on the battlefield.
    /// A copy segment retains that first, already-correct board allocation too.
    pub(crate) fn of_roll(state: &GameState, roller: ObjectId) -> Option<Self> {
        let mut entrants = crate::game::zone_pipeline::simultaneous_entrants_of(state, roller);
        if !entrants.contains(&roller) {
            entrants.push(roller);
        }
        let slot = state.pre_entry_board_cache.0.as_ref().map(Arc::clone);
        let mut cached = slot.as_ref().and_then(|slot| {
            slot.lock()
                .expect("pre-entry scratch mutex is not poisoned")
                .take()
        });
        let members: HashSet<ObjectId> = entrants.iter().copied().collect();
        if cached
            .as_ref()
            .is_some_and(|cached| !cached.residents_unchanged(state, &members))
        {
            cached = None;
        }
        let (board, original_objects, projected_objects) = match cached {
            Some(mut cached) => {
                // All histories and resolution carriers are live, including trigger
                // admission ledgers and timestamp allocation. Only the resident
                // projection is reused. Re-evaluate layers because any of those
                // live facts can enable a resident continuous effect.
                cached.board.refresh_pre_entry_context_from(state);
                cached.board.objects = cached.projected_objects.clone();
                crate::game::layers::mark_layers_full(&mut cached.board);
                crate::game::layers::flush_layers(&mut cached.board);
                (
                    cached.board,
                    cached.original_objects,
                    cached.projected_objects,
                )
            }
            None => {
                let board = match crate::game::filter::pre_entry_board(state, &entrants) {
                    std::borrow::Cow::Borrowed(_) if slot.is_none() => return None,
                    std::borrow::Cow::Borrowed(board) => Box::new(board.clone()),
                    std::borrow::Cow::Owned(board) => Box::new(board),
                };
                #[cfg(feature = "test-support")]
                crate::game::perf_counters::record_pre_entry_board_build();
                let projected_objects = board.objects.clone();
                (board, state.objects.clone(), projected_objects)
            }
        };
        // The observer collection's journal is replayed on the live game, so a
        // timestamp it allocates on the view must be the one live allocates.
        debug_assert_eq!(
            board.next_timestamp, state.next_timestamp,
            "the view's layer pass allocates no timestamp"
        );
        Some(Self {
            board: Some(board),
            reuse: slot.map(|slot| (slot, original_objects, projected_objects)),
        })
    }

    /// The view for the as-enters roll the parked die-roll instruction is making,
    /// or `None` for any other roll (the roll-to-visit, a roll whose source has
    /// entered).
    pub(crate) fn of_pending_roll(state: &GameState) -> Option<Self> {
        let roller = state
            .pending_die_roll_instruction
            .as_ref()
            .map(|instruction| instruction.source_id)
            .filter(|source| {
                crate::game::engine_replacement::entrant_has_not_entered(state, *source)
            })?;
        Self::of_roll(state, roller)
    }

    /// CR 616.1 + CR 616.1f: run a replacement pipeline step on the view and hand
    /// its typed outcome to the game. A pause renders the choice prompt from the
    /// view, where the candidates' definitions are as the roll sees them.
    ///
    /// The die-roll pipeline writes nothing else the game keeps: its matcher reads
    /// nothing, its applier writes only the event, the `RollDice` / `RollDie` pair
    /// stashes no post-replacement continuation, and the replacement index is
    /// rebuilt per pipeline call. CR 104.4b + CR 732: a remembered ordering that
    /// answers a prompt inside the pipeline counts as a loop decision, so the
    /// decision tally is handed over too.
    pub(crate) fn replace(
        mut self,
        state: &mut GameState,
        step: impl FnOnce(&mut GameState, &mut Vec<GameEvent>) -> ReplacementResult,
        events: &mut Vec<GameEvent>,
    ) -> PreEntryReplacement {
        let result = step(self.board_mut(), events);
        // Rendered before the handover: the prompt reads the view's pending record.
        let choice = match result {
            ReplacementResult::NeedsChoice(player) => Some(
                replacement::replacement_choice_waiting_for(player, self.board()),
            ),
            ReplacementResult::Execute(_) | ReplacementResult::Prevented => None,
        };
        state.pending_replacement = self.board_mut().pending_replacement.take();
        state.replacement_auto_choice_tail = self.board_mut().replacement_auto_choice_tail.take();
        state.loop_decision_tally = self.board().loop_decision_tally;
        PreEntryReplacement { result, choice }
    }

    /// The view, for a reader that judges an entrant against the board before
    /// its event (CR 614.12 + CR 603.6d).
    pub(crate) fn board(&self) -> &GameState {
        self.board
            .as_deref()
            .expect("a live pre-entry view owns its board")
    }

    /// The view, for a collection whose ledger operations are committed to the
    /// game afterwards.
    pub(crate) fn board_mut(&mut self) -> &mut GameState {
        self.board
            .as_deref_mut()
            .expect("a live pre-entry view owns its board")
    }
}

impl Drop for PreEntryRulesView {
    fn drop(&mut self) {
        if let Some((slot, original_objects, projected_objects)) = self.reuse.take() {
            // Move the leased allocation back without allocating another GameState.
            // An empty slot cannot be used concurrently with this synchronous lease.
            let board = self.board.take().expect("a returning lease owns its board");
            *slot
                .lock()
                .expect("pre-entry scratch mutex is not poisoned") = Some(CachedBoard {
                board,
                original_objects,
                projected_objects,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0};
    use crate::types::events::GameEvent;
    use crate::types::player::PlayerId;

    const BRAZEN_DWARF: &str =
        "Whenever you roll one or more dice, this creature deals 1 damage to each opponent.";

    fn a_roll() -> GameEvent {
        GameEvent::DieRolled {
            player_id: PlayerId(0),
            sides: 6,
            result: Some(3),
            pre_entry_source: None,
        }
    }

    /// U-V1. CR 603.6a + CR 603.6d + CR 702.26b: the view phases out exactly the
    /// roll's event's entrants on the battlefield, re-derives its own layers and
    /// trigger index, and leaves the game untouched.
    #[test]
    fn the_view_hides_exactly_the_entrants_on_the_battlefield() {
        let mut scenario = GameScenario::new();
        let co_entrant = scenario
            .add_creature_from_oracle(P0, "Brazen Dwarf", 1, 3, BRAZEN_DWARF)
            .id();
        let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
        let resident = scenario
            .add_creature_from_oracle(P0, "Resident Dwarf", 1, 3, BRAZEN_DWARF)
            .id();
        let later = scenario.add_card_to_hand(P0, "Later");
        let mut state = scenario.build().state().clone();
        crate::game::layers::flush_layers(&mut state);
        state.active_simultaneous_entrants = vec![co_entrant, roller, later];
        let before = state.clone();

        let view = PreEntryRulesView::of_roll(&state, roller).expect("entrants are present");
        assert!(
            state == before,
            "building the view leaves the game untouched"
        );
        for id in [co_entrant, roller] {
            assert!(
                !view.board().objects[&id].is_phased_in(),
                "{id:?} is hidden"
            );
        }
        assert!(view.board().objects[&resident].is_phased_in());
        assert_eq!(view.board().objects[&later].zone, Zone::Hand, "untouched");
        assert!(view.board().objects[&later].is_phased_in());

        let live = crate::game::trigger_index::candidates_for_event(&state, &a_roll());
        assert!(live.contains(&co_entrant) && live.contains(&resident));
        let viewed = crate::game::trigger_index::candidates_for_event(view.board(), &a_roll());
        assert!(
            !viewed.contains(&co_entrant),
            "the view's index has no entrant"
        );
        assert!(viewed.contains(&resident), "and keeps every resident");

        // No entrant of the event on the battlefield: the live board is the view.
        state.active_simultaneous_entrants.clear();
        assert!(PreEntryRulesView::of_roll(&state, later).is_none());
    }

    #[test]
    fn a_shared_board_refreshes_live_context_and_rebuilds_changed_residents() {
        let mut scenario = GameScenario::new();
        let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
        let resident = scenario.add_creature(P0, "Resident", 2, 2).id();
        let mut state = scenario.build().state().clone();
        crate::game::layers::flush_layers(&mut state);
        state.active_simultaneous_entrants = vec![roller];
        with_copy_owner_board(&mut state, |state| {
            let first = PreEntryRulesView::of_roll(state, roller).unwrap();
            let slot = state.pre_entry_board_cache.0.as_ref().unwrap().clone();
            drop(first);
            assert!(slot.lock().unwrap().is_some());
            let original_projection = slot
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .original_objects
                .clone();
            state.next_timestamp += 1;
            state.players[0].life -= 1;
            let next = PreEntryRulesView::of_roll(state, roller).unwrap();
            assert_eq!(next.board().next_timestamp, state.next_timestamp);
            assert_eq!(next.board().players[0].life, state.players[0].life);
            assert!(!next.board().objects[&roller].is_phased_in());
            drop(next);
            assert!(slot
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .original_objects
                .ptr_eq(&original_projection));

            state
                .objects
                .get_mut(&resident)
                .unwrap()
                .counters
                .insert(crate::types::counter::CounterType::Plus1Plus1, 3);
            let changed = PreEntryRulesView::of_roll(state, roller).unwrap();
            assert_eq!(
                changed.board().objects[&resident].counters
                    [&crate::types::counter::CounterType::Plus1Plus1],
                3
            );
            drop(changed);
            assert!(!slot
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .original_objects
                .ptr_eq(&original_projection));
            assert!(
                state.clone().pre_entry_board_cache.0.is_none(),
                "a cloned game never shares a scratch lease"
            );
            assert!(serde_json::to_value(&*state)
                .unwrap()
                .get("pre_entry_board_cache")
                .is_none());
        });
        assert!(state.pre_entry_board_cache.0.is_none());
    }

    /// CR 104.4b + CR 732.2a: a decision answered while a step runs on the view
    /// (a remembered order replayed inside the pipeline) counts for the game, so the
    /// handover carries the loop-decision tally with the pending record and tail.
    #[test]
    fn the_handover_carries_the_loop_decision_tally() {
        let mut scenario = GameScenario::new();
        let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
        let mut state = scenario.build().state().clone();
        state.active_simultaneous_entrants = vec![roller];
        let before = state.loop_decision_tally;
        let view = PreEntryRulesView::of_roll(&state, roller).expect("the roller is present");
        let mut events = Vec::new();
        let outcome = view.replace(
            &mut state,
            |board, _| {
                board.record_remembered_answer(
                    P0,
                    crate::game::play_trace::AnswerOptionality::Mandatory,
                );
                ReplacementResult::Prevented
            },
            &mut events,
        );
        assert!(matches!(outcome.result, ReplacementResult::Prevented));
        assert!(outcome.choice.is_none());
        assert_ne!(
            state.loop_decision_tally, before,
            "the decision answered on the view is the game's"
        );
    }
}
