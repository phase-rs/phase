use std::collections::HashSet;

use crate::game::game_object::CounterType;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::phase::Phase;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

use super::combat;
use super::combat_damage;
use super::day_night;
use super::zones;

const PHASE_ORDER: [Phase; 12] = [
    Phase::Untap,
    Phase::Upkeep,
    Phase::Draw,
    Phase::PreCombatMain,
    Phase::BeginCombat,
    Phase::DeclareAttackers,
    Phase::DeclareBlockers,
    Phase::CombatDamage,
    Phase::EndCombat,
    Phase::PostCombatMain,
    Phase::End,
    Phase::Cleanup,
];

pub fn next_phase(phase: Phase) -> Phase {
    let idx = PHASE_ORDER.iter().position(|&p| p == phase).unwrap();
    PHASE_ORDER[(idx + 1) % PHASE_ORDER.len()]
}

/// CR 500.4: Advance to the next phase/step, clearing mana pools.
pub fn advance_phase(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let next = next_phase(state.phase);

    // If wrapping from Cleanup to Untap, start next turn
    if state.phase == Phase::Cleanup && next == Phase::Untap {
        start_next_turn(state, events);
    }

    state.phase = next;

    // CR 500.4: Mana pools empty between phases/steps.
    for player in &mut state.players {
        player.mana_pool.clear();
    }

    // Reset priority to active player at start of each phase
    state.priority_player = state.active_player;
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    state.players_attacked_this_step.clear();

    events.push(GameEvent::PhaseChanged { phase: next });
}

pub fn start_next_turn(state: &mut GameState, events: &mut Vec<GameEvent>) {
    state.turn_number += 1;

    // Advance to next living player in seat order (N-player aware)
    state.active_player = super::players::next_player(state, state.active_player);

    // Reset priority
    state.priority_player = state.active_player;
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    // Reset per-turn counters
    state.lands_played_this_turn = 0;
    state.spells_cast_this_turn = 0;
    state.triggers_fired_this_turn.clear();
    state.activated_abilities_this_turn.clear();
    state.spells_cast_this_turn_by_player.clear();
    state.players_who_cast_noncreature_spell_this_turn.clear();
    state.players_who_searched_library_this_turn.clear();
    state.players_attacked_this_step.clear();
    state.players_attacked_this_turn.clear();
    state.attacking_creatures_this_turn.clear();
    state.players_who_created_token_this_turn.clear();
    state.players_who_discarded_card_this_turn.clear();
    state.players_who_sacrificed_artifact_this_turn.clear();
    state.players_who_had_creature_etb_this_turn.clear();
    state
        .players_who_had_angel_or_berserker_etb_this_turn
        .clear();
    state.players_who_had_artifact_etb_this_turn.clear();
    state.cards_left_graveyard_this_turn.clear();
    state.creature_died_this_turn = false;
    for player in &mut state.players {
        player.has_drawn_this_turn = false;
        player.lands_played_this_turn = 0;
        player.life_gained_this_turn = 0;
        player.life_lost_this_turn = 0;
        player.descended_this_turn = false;
        player.cards_drawn_this_turn = 0;
    }

    // Reset loyalty_activated_this_turn for all permanents controlled by the active player
    let active = state.active_player;
    for obj in state.objects.values_mut() {
        if obj.controller == active && obj.loyalty_activated_this_turn {
            obj.loyalty_activated_this_turn = false;
        }
    }

    events.push(GameEvent::TurnStarted {
        player_id: state.active_player,
        turn_number: state.turn_number,
    });
}

/// CR 502.3: During the untap step, the active player untaps each permanent they control.
pub fn execute_untap(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let active = state.active_player;

    // CR 514.2: Prune "until your next turn" transient effects for the active player.
    super::layers::prune_until_next_turn_effects(state, active);
    let to_untap: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.controller == active && obj.tapped)
                .unwrap_or(false)
        })
        .collect();

    for id in to_untap {
        let proposed = ProposedEvent::Untap {
            object_id: id,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Untap { object_id, .. } = event {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        // CR 122.1g: If a permanent with a stun counter would become untapped,
                        // instead remove a stun counter from it.
                        if let Some(entry) = obj.counters.get_mut(&CounterType::Stun) {
                            *entry -= 1;
                            if *entry == 0 {
                                obj.counters.remove(&CounterType::Stun);
                            }
                            events.push(GameEvent::CounterRemoved {
                                object_id,
                                counter_type: "stun".to_string(),
                                count: 1,
                            });
                        } else {
                            obj.tapped = false;
                            events.push(GameEvent::PermanentUntapped { object_id });
                        }
                    }
                }
            }
            ReplacementResult::Prevented => {
                // "Doesn't untap during untap step" effects
            }
            ReplacementResult::NeedsChoice(_) => {
                // Edge case for untap step; skip for now
            }
        }
    }
}

/// CR 504.1: During the draw step, the active player draws a card.
pub fn execute_draw(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let active = state.active_player;
    let player = state
        .players
        .iter()
        .find(|p| p.id == active)
        .expect("active player exists");

    if player.library.is_empty() {
        return;
    }

    // Library top = index 0
    let top_card = player.library[0];
    zones::move_to_zone(state, top_card, Zone::Hand, events);

    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == active)
        .expect("active player exists");
    player.has_drawn_this_turn = true;
    player.cards_drawn_this_turn = player.cards_drawn_this_turn.saturating_add(1);
}

/// Execute the cleanup step. Returns `Some(WaitingFor)` if the player must
/// choose which cards to discard down to maximum hand size, or `None` if
/// cleanup completes immediately.
pub fn execute_cleanup(state: &mut GameState, events: &mut Vec<GameEvent>) -> Option<WaitingFor> {
    // CR 514.2: Prune "until end of turn" transient continuous effects.
    super::layers::prune_end_of_turn_effects(state);

    // CR 514.2: Remove end-of-turn game restrictions (e.g., "this turn" damage prevention disabled).
    state.restrictions.retain(|r| {
        use crate::types::ability::{GameRestriction, RestrictionExpiry};
        match r {
            GameRestriction::DamagePreventionDisabled { expiry, .. } => {
                !matches!(expiry, RestrictionExpiry::EndOfTurn)
            }
        }
    });

    // CR 727.2: Check day/night transition at cleanup.
    day_night::check_day_night_transition(state, events);

    let active = state.active_player;

    // CR 514.1: Discard down to maximum hand size.
    let player = state
        .players
        .iter()
        .find(|p| p.id == active)
        .expect("active player exists");

    let hand_size = player.hand.len();
    if hand_size > 7 {
        let count = hand_size - 7;
        let cards = player.hand.clone();
        return Some(WaitingFor::DiscardToHandSize {
            player: active,
            count,
            cards,
        });
    }

    // CR 514.3: Damage on creatures is removed at cleanup.
    let to_clear: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.damage_marked > 0)
                .unwrap_or(false)
        })
        .collect();

    for id in to_clear {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.damage_marked = 0;
            obj.dealt_deathtouch_damage = false;
            events.push(GameEvent::DamageCleared { object_id: id });
        }
    }

    None
}

/// Complete the cleanup step after the player has chosen cards to discard.
/// Discards the selected cards and clears damage (the parts of cleanup that
/// were deferred while waiting for player input).
pub fn finish_cleanup_discard(
    state: &mut GameState,
    chosen: &[crate::types::identifiers::ObjectId],
    events: &mut Vec<GameEvent>,
) {
    for &card_id in chosen {
        zones::move_to_zone(state, card_id, Zone::Graveyard, events);
        let player_id = state
            .objects
            .get(&card_id)
            .map(|obj| obj.owner)
            .unwrap_or(state.active_player);
        events.push(GameEvent::Discarded {
            player_id,
            object_id: card_id,
        });
    }

    // Clear damage on all battlefield creatures (deferred from execute_cleanup)
    let to_clear: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.damage_marked > 0)
                .unwrap_or(false)
        })
        .collect();

    for id in to_clear {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.damage_marked = 0;
            obj.dealt_deathtouch_damage = false;
            events.push(GameEvent::DamageCleared { object_id: id });
        }
    }
}

/// CR 103.7a: The player who goes first skips their first draw step.
pub fn should_skip_draw(state: &GameState) -> bool {
    state.turn_number == 1
}

pub fn auto_advance(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    loop {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return state.waiting_for.clone();
        }

        match state.phase {
            Phase::Untap => {
                execute_untap(state, events);
                advance_phase(state, events);
            }
            Phase::Upkeep => {
                // No triggers in Phase 3
                advance_phase(state, events);
            }
            Phase::Draw => {
                if !should_skip_draw(state) {
                    execute_draw(state, events);
                }
                advance_phase(state, events);
            }
            Phase::PreCombatMain | Phase::PostCombatMain => {
                return WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            Phase::BeginCombat => {
                if combat::has_potential_attackers(state) {
                    state.combat = Some(crate::game::combat::CombatState::default());
                    advance_phase(state, events);
                    // Continue to DeclareAttackers
                } else {
                    // Skip all combat phases
                    state.combat = None;
                    state.phase = Phase::PostCombatMain;
                    state.priority_player = state.active_player;
                    state.priority_passes.clear();
                    state.priority_pass_count = 0;
                    events.push(GameEvent::PhaseChanged {
                        phase: Phase::PostCombatMain,
                    });
                    return WaitingFor::Priority {
                        player: state.active_player,
                    };
                }
            }
            Phase::DeclareAttackers => {
                let valid_attacker_ids = super::combat::get_valid_attacker_ids(state);
                let valid_attack_targets = super::combat::get_valid_attack_targets(state);
                return WaitingFor::DeclareAttackers {
                    player: state.active_player,
                    valid_attacker_ids,
                    valid_attack_targets,
                };
            }
            Phase::DeclareBlockers => {
                // Check if any attackers were declared
                let has_attackers = state
                    .combat
                    .as_ref()
                    .is_some_and(|c| !c.attackers.is_empty());
                if has_attackers {
                    let defending = super::players::next_player(state, state.active_player);
                    let valid_blocker_ids = super::combat::get_valid_blocker_ids(state);
                    if !valid_blocker_ids.is_empty() {
                        let valid_block_targets = super::combat::get_valid_block_targets(state);
                        return WaitingFor::DeclareBlockers {
                            player: defending,
                            valid_blocker_ids,
                            valid_block_targets,
                        };
                    }
                    // No valid blockers — auto-advance past declare blockers
                    advance_phase(state, events);
                } else {
                    // No attackers, skip to EndCombat
                    state.phase = Phase::EndCombat;
                    events.push(GameEvent::PhaseChanged {
                        phase: Phase::EndCombat,
                    });
                    // Continue loop to process EndCombat
                }
            }
            Phase::CombatDamage => {
                combat_damage::resolve_combat_damage(state, events);
                advance_phase(state, events);
                // Continue to EndCombat
            }
            Phase::EndCombat => {
                state.combat = None;
                advance_phase(state, events);
                // Continue to PostCombatMain
            }
            Phase::End => {
                return WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            Phase::Cleanup => {
                if let Some(waiting) = execute_cleanup(state, events) {
                    return waiting;
                }
                advance_phase(state, events);
                // advance_phase handles start_next_turn when wrapping Cleanup -> Untap
                // Continue loop to process next turn's phases
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state
    }

    #[test]
    fn next_phase_advances_in_order() {
        assert_eq!(next_phase(Phase::Untap), Phase::Upkeep);
        assert_eq!(next_phase(Phase::Upkeep), Phase::Draw);
        assert_eq!(next_phase(Phase::Draw), Phase::PreCombatMain);
        assert_eq!(next_phase(Phase::PreCombatMain), Phase::BeginCombat);
        assert_eq!(next_phase(Phase::PostCombatMain), Phase::End);
        assert_eq!(next_phase(Phase::End), Phase::Cleanup);
    }

    #[test]
    fn next_phase_wraps_cleanup_to_untap() {
        assert_eq!(next_phase(Phase::Cleanup), Phase::Untap);
    }

    #[test]
    fn advance_phase_changes_phase_and_emits_event() {
        let mut state = setup();
        state.phase = Phase::Untap;
        let mut events = Vec::new();

        advance_phase(&mut state, &mut events);

        assert_eq!(state.phase, Phase::Upkeep);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PhaseChanged {
                phase: Phase::Upkeep
            }
        )));
    }

    #[test]
    fn advance_phase_clears_mana_pools() {
        use crate::types::identifiers::ObjectId;
        use crate::types::mana::{ManaType, ManaUnit};

        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            snow: false,
            restrictions: Vec::new(),
        });

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn advance_phase_resets_priority_to_active_player() {
        let mut state = setup();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(1); // Was opponent's priority

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.priority_player, PlayerId(0));
        assert_eq!(state.priority_pass_count, 0);
    }

    #[test]
    fn start_next_turn_increments_turn_and_swaps_player() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.turn_number = 1;

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.priority_player, PlayerId(1));
    }

    #[test]
    fn start_next_turn_resets_per_turn_counters() {
        let mut state = setup();
        state.lands_played_this_turn = 1;
        state.players[0].has_drawn_this_turn = true;
        state.players[0].lands_played_this_turn = 1;

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.lands_played_this_turn, 0);
        assert!(!state.players[0].has_drawn_this_turn);
        assert_eq!(state.players[0].lands_played_this_turn, 0);
    }

    #[test]
    fn start_next_turn_emits_turn_started_event() {
        let mut state = setup();
        let mut events = Vec::new();

        start_next_turn(&mut state, &mut events);

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::TurnStarted { turn_number: 2, .. })));
    }

    #[test]
    fn execute_untap_untaps_active_player_permanents() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(!state.objects[&id].tapped);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == id)));
    }

    #[test]
    fn execute_untap_does_not_untap_opponents_permanents() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(state.objects[&id].tapped);
    }

    #[test]
    fn execute_draw_moves_top_of_library_to_hand() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        execute_draw(&mut state, &mut events);

        assert!(state.players[0].hand.contains(&id));
        assert!(!state.players[0].library.contains(&id));
        assert!(state.players[0].has_drawn_this_turn);
    }

    #[test]
    fn should_skip_draw_on_turn_1() {
        let mut state = setup();
        state.turn_number = 1;
        assert!(should_skip_draw(&state));

        state.turn_number = 2;
        assert!(!should_skip_draw(&state));
    }

    #[test]
    fn execute_cleanup_returns_discard_choice_when_over_seven() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give player 9 cards in hand
        let mut hand_ids = Vec::new();
        for i in 0..9 {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
            hand_ids.push(id);
        }

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);

        match result {
            Some(WaitingFor::DiscardToHandSize {
                player,
                count,
                cards,
            }) => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(cards.len(), 9);
            }
            other => panic!("Expected DiscardToHandSize, got {:?}", other),
        }

        // Hand unchanged until player makes a choice
        assert_eq!(state.players[0].hand.len(), 9);
    }

    #[test]
    fn execute_cleanup_returns_none_when_at_or_below_seven() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Give player exactly 7 cards
        for i in 0..7 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }

        let mut events = Vec::new();
        let result = execute_cleanup(&mut state, &mut events);
        assert!(result.is_none());
    }

    #[test]
    fn finish_cleanup_discard_moves_selected_cards() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let mut hand_ids = Vec::new();
        for i in 0..9 {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
            hand_ids.push(id);
        }

        // Player chooses to discard the last 2 cards
        let to_discard = vec![hand_ids[7], hand_ids[8]];
        let mut events = Vec::new();
        finish_cleanup_discard(&mut state, &to_discard, &mut events);

        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[0].graveyard.len(), 2);
        assert!(state.players[0].graveyard.contains(&hand_ids[7]));
        assert!(state.players[0].graveyard.contains(&hand_ids[8]));
        // The first 7 cards should still be in hand
        for &id in &hand_ids[..7] {
            assert!(state.players[0].hand.contains(&id));
        }
    }

    #[test]
    fn execute_cleanup_clears_damage() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().damage_marked = 3;

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);

        assert_eq!(state.objects[&id].damage_marked, 0);
    }

    #[test]
    fn auto_advance_skips_to_precombat_main() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 2; // Not first turn, so draw happens

        // Add a card to library so draw works
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert!(matches!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn auto_advance_skips_draw_on_first_turn() {
        let mut state = setup();
        state.phase = Phase::Untap;
        state.turn_number = 1;

        // Add a card to library (should NOT be drawn)
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        auto_advance(&mut state, &mut events);

        // Card should still be in library
        assert!(state.players[0].library.contains(&id));
        assert!(!state.players[0].hand.contains(&id));
    }

    #[test]
    fn auto_advance_skips_combat_phases() {
        let mut state = setup();
        state.phase = Phase::BeginCombat;

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }

    #[test]
    fn auto_advance_stops_at_end_step() {
        let mut state = setup();
        state.phase = Phase::End;

        let mut events = Vec::new();
        let waiting = auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::End);
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }

    #[test]
    fn advance_phase_from_cleanup_starts_next_turn() {
        let mut state = setup();
        state.phase = Phase::Cleanup;
        state.active_player = PlayerId(0);
        state.turn_number = 1;

        let mut events = Vec::new();
        advance_phase(&mut state, &mut events);

        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
        assert_eq!(state.phase, Phase::Untap);
    }

    #[test]
    fn start_next_turn_resets_spells_cast_this_turn() {
        let mut state = setup();
        state.spells_cast_this_turn = 3;

        let mut events = Vec::new();
        start_next_turn(&mut state, &mut events);

        assert_eq!(state.spells_cast_this_turn, 0);
    }

    /// Regression: combat damage that reduces a player to 0-or-less life must end the game even
    /// when auto_advance drives the CombatDamage phase automatically (i.e. without a separate
    /// PassPriority action) and triggers were already processed inline before combat resolved.
    ///
    /// Previously `auto_advance` ignored the GameOver set by SBA and kept looping through
    /// EndCombat → PostCombatMain, returning WaitingFor::Priority which overwrote the GameOver.
    #[test]
    fn auto_advance_game_over_from_combat_damage_stops_loop() {
        use crate::game::combat::{AttackerInfo, CombatState};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.phase = Phase::CombatDamage;

        // Create an unblocked attacker with lethal power (20, enough to kill from full life)
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Big Creature".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(20);
            obj.toughness = Some(20);
            obj.entered_battlefield_turn = Some(1);
        }

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker_id,
                defending_player: PlayerId(1),
            }],
            ..Default::default()
        });

        let mut events = Vec::new();
        let wf = auto_advance(&mut state, &mut events);

        assert!(
            matches!(
                wf,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "auto_advance should propagate GameOver when combat damage kills opponent, got {:?}",
            wf
        );
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "state.waiting_for should be GameOver, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn stun_counter_prevents_untap_and_removes_counter() {
        // CR 122.1g: A stun counter prevents a permanent from untapping;
        // instead, one stun counter is removed.
        use crate::types::zones::Zone;

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.tapped = true;
        obj.counters.insert(CounterType::Stun, 2);

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        let obj = &state.objects[&obj_id];
        assert!(
            obj.tapped,
            "creature should remain tapped after stun counter removal"
        );
        assert_eq!(
            obj.counters.get(&CounterType::Stun).copied().unwrap_or(0),
            1,
            "one stun counter should be removed"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::CounterRemoved { object_id, counter_type, count: 1 }
                    if *object_id == obj_id && counter_type == "stun"
            )),
            "CounterRemoved event should be emitted"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentUntapped { .. })),
            "PermanentUntapped should not be emitted when stun counter is present"
        );
    }

    #[test]
    fn stun_counter_removed_at_zero_cleans_up_entry() {
        // When the last stun counter is removed, the entry should be gone from the map.
        use crate::types::zones::Zone;

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.tapped = true;
        obj.counters.insert(CounterType::Stun, 1);

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        let obj = &state.objects[&obj_id];
        assert!(
            !obj.counters.contains_key(&CounterType::Stun),
            "stun entry should be removed at zero"
        );
        assert!(
            obj.tapped,
            "creature still tapped after final stun counter removed"
        );
    }

    #[test]
    fn no_stun_counter_untaps_normally() {
        use crate::types::zones::Zone;

        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let mut events = Vec::new();
        execute_untap(&mut state, &mut events);

        assert!(
            !state.objects[&obj_id].tapped,
            "creature should untap normally"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == obj_id)
            ),
            "PermanentUntapped event should be emitted"
        );
    }

    #[test]
    fn restriction_cleanup_end_of_turn() {
        use crate::types::ability::{GameRestriction, RestrictionExpiry};
        use crate::types::identifiers::ObjectId;

        let mut state = GameState::new_two_player(42);
        state.phase = Phase::End;

        // Add an EndOfTurn restriction
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(1),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });
        // Add an EndOfCombat restriction (should survive cleanup)
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(2),
                expiry: RestrictionExpiry::EndOfCombat,
                scope: None,
            });

        assert_eq!(state.restrictions.len(), 2);

        let mut events = Vec::new();
        execute_cleanup(&mut state, &mut events);

        // EndOfTurn restriction should be removed, EndOfCombat should remain
        assert_eq!(state.restrictions.len(), 1);
        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::DamagePreventionDisabled {
                expiry: RestrictionExpiry::EndOfCombat,
                ..
            }
        ));
    }
}
