use std::collections::{BTreeMap, HashSet};

use crate::game::casting;
use crate::game::combat;
use crate::game::combat::AttackTarget;
use crate::game::deck_loading::DeckEntry;
use crate::game::keywords;
use crate::game::mana_sources;
use crate::types::ability::ChoiceType;
use crate::types::ability::TargetRef;
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{ConvokeMode, GameState, TargetSelectionSlot, WaitingFor};
use crate::types::match_config::DeckCardCount;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TacticalClass {
    Pass,
    Land,
    Spell,
    Ability,
    Attack,
    Block,
    Target,
    Selection,
    Replacement,
    Mana,
    Utility,
}

#[derive(Debug, Clone)]
pub struct ActionMetadata {
    pub actor: Option<PlayerId>,
    pub tactical_class: TacticalClass,
}

#[derive(Debug, Clone)]
pub struct CandidateAction {
    pub action: GameAction,
    pub metadata: ActionMetadata,
}

pub fn candidate_actions(state: &GameState) -> Vec<CandidateAction> {
    match &state.waiting_for {
        WaitingFor::Priority { player } => priority_actions(state, *player),
        WaitingFor::MulliganDecision { .. } => vec![
            candidate(
                GameAction::MulliganDecision { keep: true },
                TacticalClass::Selection,
                state.waiting_for.acting_player(),
            ),
            candidate(
                GameAction::MulliganDecision { keep: false },
                TacticalClass::Selection,
                state.waiting_for.acting_player(),
            ),
        ],
        WaitingFor::MulliganBottomCards { player, count } => {
            bottom_card_actions(state, *player, *count)
        }
        WaitingFor::ManaPayment {
            player,
            convoke_mode,
        } => mana_payment_actions(state, *player, *convoke_mode),
        WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } => target_step_actions(
            *player,
            target_slots,
            selection.current_slot,
            &selection.current_legal_targets,
        ),
        WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            selection,
            ..
        } => target_step_actions(
            *player,
            target_slots,
            selection.current_slot,
            &selection.current_legal_targets,
        ),
        WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids,
            valid_attack_targets,
        } => attacker_actions(*player, valid_attacker_ids, valid_attack_targets),
        WaitingFor::DeclareBlockers {
            player,
            valid_blocker_ids,
            valid_block_targets,
        } => blocker_actions(*player, valid_blocker_ids, valid_block_targets),
        WaitingFor::ReplacementChoice {
            candidate_count,
            player,
            ..
        } => (0..*candidate_count)
            .map(|i| {
                candidate(
                    GameAction::ChooseReplacement { index: i },
                    TacticalClass::Replacement,
                    Some(*player),
                )
            })
            .collect(),
        // CR 707.9: AI chooses a permanent to copy for "enter as a copy of" replacements.
        WaitingFor::CopyTargetChoice {
            player,
            valid_targets,
            ..
        } => valid_targets
            .iter()
            .map(|&target_id| {
                candidate(
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target_id)),
                    },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::EquipTarget {
            player,
            equipment_id,
            valid_targets,
        } => valid_targets
            .iter()
            .map(|&target_id| {
                candidate(
                    GameAction::Equip {
                        equipment_id: *equipment_id,
                        target_id,
                    },
                    TacticalClass::Utility,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::ScryChoice { player, cards } => select_cards_variants(*player, cards, None),
        WaitingFor::DigChoice {
            player,
            cards,
            keep_count,
        } => combinations(cards, *keep_count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::SurveilChoice { player, cards } => select_cards_variants(*player, cards, None),
        WaitingFor::RevealChoice { player, cards, .. } => {
            select_cards_variants(*player, cards, Some(1))
        }
        WaitingFor::SearchChoice {
            player,
            cards,
            count,
        } => combinations(cards, *count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::BetweenGamesSideboard { player, .. } => sideboard_actions(state, *player),
        WaitingFor::BetweenGamesChoosePlayDraw { player, .. } => vec![
            candidate(
                GameAction::ChoosePlayDraw { play_first: true },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::ChoosePlayDraw { play_first: false },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        WaitingFor::NamedChoice {
            player,
            options,
            choice_type,
            ..
        } => named_choice_actions(state, *player, options, choice_type),
        WaitingFor::ModeChoice { player, modal, .. } => {
            if modal.allow_repeat_modes {
                // CR 700.2d: Use sequence generation that allows repeated indices.
                crate::game::ability_utils::generate_modal_index_sequences(modal)
                    .into_iter()
                    .map(|indices| {
                        candidate(
                            GameAction::SelectModes { indices },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            } else {
                mode_actions(
                    *player,
                    modal.mode_count,
                    modal.min_choices,
                    modal.max_choices,
                )
            }
        }
        WaitingFor::AbilityModeChoice {
            player,
            modal,
            unavailable_modes,
            ..
        } => {
            let available: Vec<usize> = (0..modal.mode_count)
                .filter(|i| !unavailable_modes.contains(i))
                .collect();
            if modal.allow_repeat_modes {
                // Build a filtered ModalChoice for sequence generation with repeats.
                let filtered = crate::types::ability::ModalChoice {
                    mode_count: available.len(),
                    min_choices: modal.min_choices,
                    max_choices: modal.max_choices,
                    allow_repeat_modes: true,
                    ..modal.clone()
                };
                crate::game::ability_utils::generate_modal_index_sequences(&filtered)
                    .into_iter()
                    .map(|local_indices| {
                        // Map local indices back to original mode indices.
                        let indices = local_indices.into_iter().map(|i| available[i]).collect();
                        candidate(
                            GameAction::SelectModes { indices },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            } else {
                mode_actions_from_available(
                    *player,
                    &available,
                    modal.min_choices,
                    modal.max_choices,
                )
            }
        }
        WaitingFor::DiscardToHandSize {
            player,
            count,
            cards,
        } => combinations(cards, *count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::OptionalCostChoice { player, .. } => vec![
            candidate(
                GameAction::DecideOptionalCost { pay: true },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::DecideOptionalCost { pay: false },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        WaitingFor::DiscardForCost {
            player,
            count,
            cards,
            ..
        } => combinations(cards, *count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 118.3: AI selects permanents to sacrifice as cost
        WaitingFor::SacrificeForCost {
            player,
            count,
            permanents,
            ..
        } => combinations(permanents, *count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::MultiTargetSelection {
            player,
            legal_targets,
            min_targets,
            ..
        } => {
            let mut actions = Vec::new();
            actions.push(candidate(
                GameAction::SelectCards {
                    cards: legal_targets.clone(),
                },
                TacticalClass::Selection,
                Some(*player),
            ));
            if *min_targets == 0 {
                actions.push(candidate(
                    GameAction::SelectCards { cards: vec![] },
                    TacticalClass::Selection,
                    Some(*player),
                ));
            }
            actions
        }
        WaitingFor::AdventureCastChoice { player, .. } => vec![
            candidate(
                GameAction::ChooseAdventureFace { creature: true },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::ChooseAdventureFace { creature: false },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        WaitingFor::NinjutsuActivation {
            player,
            ninjutsu_cards,
            unblocked_attackers,
        } => {
            let mut actions = vec![candidate(
                GameAction::PassPriority,
                TacticalClass::Pass,
                Some(*player),
            )];
            for &card_id in ninjutsu_cards {
                for &attacker_id in unblocked_attackers {
                    actions.push(candidate(
                        GameAction::ActivateNinjutsu {
                            ninjutsu_card_id: card_id,
                            attacker_to_return: attacker_id,
                        },
                        TacticalClass::Ability,
                        Some(*player),
                    ));
                }
            }
            actions
        }
        WaitingFor::OptionalEffectChoice { .. } => {
            vec![
                candidate(
                    GameAction::DecideOptionalEffect { accept: true },
                    TacticalClass::Utility,
                    state.waiting_for.acting_player(),
                ),
                candidate(
                    GameAction::DecideOptionalEffect { accept: false },
                    TacticalClass::Utility,
                    state.waiting_for.acting_player(),
                ),
            ]
        }
        // CR 118.12: "Counter unless pays" — opponent chooses pay or decline.
        WaitingFor::UnlessPayment { player, .. } => {
            vec![
                candidate(
                    GameAction::PayUnlessCost { pay: true },
                    TacticalClass::Selection,
                    Some(*player),
                ),
                candidate(
                    GameAction::PayUnlessCost { pay: false },
                    TacticalClass::Selection,
                    Some(*player),
                ),
            ]
        }
        // CR 701.52: Choose a ring-bearer from candidate creatures.
        WaitingFor::ChooseRingBearer { player, candidates } => candidates
            .iter()
            .map(|&target| {
                candidate(
                    GameAction::ChooseRingBearer { target },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::GameOver { .. } => Vec::new(),
    }
}

fn candidate(
    action: GameAction,
    tactical_class: TacticalClass,
    actor: Option<PlayerId>,
) -> CandidateAction {
    CandidateAction {
        action,
        metadata: ActionMetadata {
            actor,
            tactical_class,
        },
    }
}

fn priority_actions(state: &GameState, player: PlayerId) -> Vec<CandidateAction> {
    let mut actions = vec![candidate(
        GameAction::PassPriority,
        TacticalClass::Pass,
        Some(player),
    )];

    let p = &state.players[player.0 as usize];
    let is_main_phase = matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain);
    let stack_empty = state.stack.is_empty();
    let is_active = state.active_player == player;

    if is_main_phase
        && stack_empty
        && is_active
        && state.lands_played_this_turn < state.max_lands_per_turn
    {
        for &obj_id in &p.hand {
            if let Some(obj) = state.objects.get(&obj_id) {
                if obj.card_types.core_types.contains(&CoreType::Land) {
                    actions.push(candidate(
                        GameAction::PlayLand {
                            card_id: obj.card_id,
                        },
                        TacticalClass::Land,
                        Some(player),
                    ));
                }
            }
        }
    }

    for object_id in casting::spell_objects_available_to_cast(state, player) {
        let Some(obj) = state.objects.get(&object_id) else {
            continue;
        };
        if casting::can_cast_object_now(state, player, object_id) {
            actions.push(candidate(
                GameAction::CastSpell {
                    card_id: obj.card_id,
                    targets: Vec::new(),
                },
                TacticalClass::Spell,
                Some(player),
            ));
        }
    }

    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player {
                for (i, ability_def) in obj.abilities.iter().enumerate() {
                    if ability_def.kind == crate::types::ability::AbilityKind::Activated
                        && !crate::game::mana_abilities::is_mana_ability(ability_def)
                        && casting::can_activate_ability_now(state, player, obj_id, i)
                    {
                        actions.push(candidate(
                            GameAction::ActivateAbility {
                                source_id: obj_id,
                                ability_index: i,
                            },
                            TacticalClass::Ability,
                            Some(player),
                        ));
                    }
                }
            }
        }
    }

    // NOTE: TapLandForMana is intentionally excluded from priority candidates.
    // The engine auto-taps lands during mana payment (pay_mana_cost → auto_tap_lands),
    // so the AI never needs to manually tap lands during priority. Including them
    // pollutes the search tree — shallow evaluations see "hand unchanged" for tapping
    // vs "hand shrinks" for casting, causing the AI to prefer tapping over casting.
    // Mana tap candidates are still generated for ManaPayment/UnlessPayment contexts
    // via mana_payment_actions().

    // CR 702.49a: Offer Ninjutsu activations during declare blockers step
    if matches!(state.phase, Phase::DeclareBlockers | Phase::CombatDamage)
        && state.active_player == player
    {
        let ninjutsu_cards = keywords::ninjutsu_cards_in_hand(state, player);
        let unblocked = combat::unblocked_attackers(state);
        for card_id in &ninjutsu_cards {
            for &attacker_id in &unblocked {
                actions.push(candidate(
                    GameAction::ActivateNinjutsu {
                        ninjutsu_card_id: *card_id,
                        attacker_to_return: attacker_id,
                    },
                    TacticalClass::Ability,
                    Some(player),
                ));
            }
        }
    }

    actions
}

fn target_step_actions(
    player: PlayerId,
    target_slots: &[TargetSelectionSlot],
    current_slot: usize,
    current_legal_targets: &[TargetRef],
) -> Vec<CandidateAction> {
    let legal_targets: Vec<TargetRef> = if !current_legal_targets.is_empty() {
        current_legal_targets.to_vec()
    } else {
        target_slots
            .get(current_slot)
            .map(|slot| slot.legal_targets.clone())
            .unwrap_or_default()
    };

    let mut actions: Vec<CandidateAction> = legal_targets
        .into_iter()
        .map(|target| {
            candidate(
                GameAction::ChooseTarget {
                    target: Some(target),
                },
                TacticalClass::Target,
                Some(player),
            )
        })
        .collect();

    if target_slots
        .get(current_slot)
        .is_some_and(|slot| slot.optional)
    {
        actions.push(candidate(
            GameAction::ChooseTarget { target: None },
            TacticalClass::Target,
            Some(player),
        ));
    }

    actions
}

fn attacker_actions(
    player: PlayerId,
    valid_attacker_ids: &[crate::types::identifiers::ObjectId],
    valid_attack_targets: &[AttackTarget],
) -> Vec<CandidateAction> {
    let default_target = valid_attack_targets.first().cloned();
    let mut actions = vec![candidate(
        GameAction::DeclareAttackers {
            attacks: Vec::new(),
        },
        TacticalClass::Attack,
        Some(player),
    )];

    let Some(target) = default_target else {
        return actions;
    };

    for &id in valid_attacker_ids {
        actions.push(candidate(
            GameAction::DeclareAttackers {
                attacks: vec![(id, target.clone())],
            },
            TacticalClass::Attack,
            Some(player),
        ));
    }

    if valid_attacker_ids.len() > 1 {
        actions.push(candidate(
            GameAction::DeclareAttackers {
                attacks: valid_attacker_ids
                    .iter()
                    .copied()
                    .map(|id| (id, target.clone()))
                    .collect(),
            },
            TacticalClass::Attack,
            Some(player),
        ));
    }

    actions
}

fn blocker_actions(
    player: PlayerId,
    valid_blocker_ids: &[crate::types::identifiers::ObjectId],
    valid_block_targets: &std::collections::HashMap<
        crate::types::identifiers::ObjectId,
        Vec<crate::types::identifiers::ObjectId>,
    >,
) -> Vec<CandidateAction> {
    let mut actions = vec![candidate(
        GameAction::DeclareBlockers {
            assignments: Vec::new(),
        },
        TacticalClass::Block,
        Some(player),
    )];

    for &blocker_id in valid_blocker_ids {
        if let Some(targets) = valid_block_targets.get(&blocker_id) {
            for &attacker_id in targets {
                actions.push(candidate(
                    GameAction::DeclareBlockers {
                        assignments: vec![(blocker_id, attacker_id)],
                    },
                    TacticalClass::Block,
                    Some(player),
                ));
            }
        }
    }

    actions
}

fn select_cards_variants(
    player: PlayerId,
    cards: &[crate::types::identifiers::ObjectId],
    exact_count: Option<usize>,
) -> Vec<CandidateAction> {
    match exact_count {
        Some(count) => combinations(cards, count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(player),
                )
            })
            .collect(),
        None => {
            let mut actions = vec![candidate(
                GameAction::SelectCards { cards: Vec::new() },
                TacticalClass::Selection,
                Some(player),
            )];
            actions.push(candidate(
                GameAction::SelectCards {
                    cards: cards.to_vec(),
                },
                TacticalClass::Selection,
                Some(player),
            ));
            if cards.len() > 1 {
                for &card in cards {
                    actions.push(candidate(
                        GameAction::SelectCards { cards: vec![card] },
                        TacticalClass::Selection,
                        Some(player),
                    ));
                }
            }
            actions
        }
    }
}

fn mode_actions(
    player: PlayerId,
    mode_count: usize,
    min: usize,
    max: usize,
) -> Vec<CandidateAction> {
    let indices: Vec<usize> = (0..mode_count).collect();
    mode_actions_from_available(player, &indices, min, max)
}

fn mode_actions_from_available(
    player: PlayerId,
    available: &[usize],
    min: usize,
    max: usize,
) -> Vec<CandidateAction> {
    let mut actions = Vec::new();
    for pick_count in min..=max.min(available.len()) {
        for combo in combinations_usize(available, pick_count) {
            actions.push(candidate(
                GameAction::SelectModes { indices: combo },
                TacticalClass::Selection,
                Some(player),
            ));
        }
    }
    actions
}

fn sideboard_actions(state: &GameState, player: PlayerId) -> Vec<CandidateAction> {
    let Some(pool) = state.deck_pools.iter().find(|pool| pool.player == player) else {
        return Vec::new();
    };

    vec![candidate(
        GameAction::SubmitSideboard {
            main: deck_entries_to_counts(&pool.current_main),
            sideboard: deck_entries_to_counts(&pool.current_sideboard),
        },
        TacticalClass::Selection,
        Some(player),
    )]
}

fn deck_entries_to_counts(entries: &[DeckEntry]) -> Vec<DeckCardCount> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for entry in entries {
        if entry.count > 0 {
            *counts.entry(entry.card.name.clone()).or_insert(0) += entry.count;
        }
    }

    counts
        .into_iter()
        .map(|(name, count)| DeckCardCount { name, count })
        .collect()
}

fn named_choice_actions(
    state: &GameState,
    player: PlayerId,
    options: &[String],
    choice_type: &ChoiceType,
) -> Vec<CandidateAction> {
    if options.is_empty() && matches!(choice_type, ChoiceType::CardName) {
        let mut seen = HashSet::new();
        return state
            .all_card_names
            .iter()
            .filter(|name| seen.insert(name.to_ascii_lowercase()))
            .cloned()
            .map(|choice| {
                candidate(
                    GameAction::ChooseOption { choice },
                    TacticalClass::Selection,
                    Some(player),
                )
            })
            .collect();
    }

    options
        .iter()
        .cloned()
        .map(|choice| {
            candidate(
                GameAction::ChooseOption { choice },
                TacticalClass::Selection,
                Some(player),
            )
        })
        .collect()
}

fn bottom_card_actions(state: &GameState, player: PlayerId, count: u8) -> Vec<CandidateAction> {
    let p = &state.players[player.0 as usize];
    let hand: Vec<_> = p.hand.clone();

    if count == 0 || hand.is_empty() {
        return vec![candidate(
            GameAction::SelectCards { cards: Vec::new() },
            TacticalClass::Selection,
            Some(player),
        )];
    }

    combinations(&hand, count as usize)
        .into_iter()
        .map(|combo| {
            candidate(
                GameAction::SelectCards { cards: combo },
                TacticalClass::Selection,
                Some(player),
            )
        })
        .collect()
}

/// CR 605.3a: Generate TapLandForMana candidates for untapped lands with activatable mana.
/// Used for ManaPayment/UnlessPayment contexts only — NOT for priority (the engine
/// auto-taps lands during spell casting via pay_mana_cost → auto_tap_lands).
// Note: UntapLandForMana is intentionally omitted — it is a human-only undo action.
// AI never populates lands_tapped_for_mana, so the handler would reject it anyway.
fn mana_tap_actions(state: &GameState, player: PlayerId) -> Vec<CandidateAction> {
    let mut actions = Vec::new();
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player
                && !obj.tapped
                && obj.card_types.core_types.contains(&CoreType::Land)
                && !mana_sources::activatable_land_mana_options(state, obj_id, player).is_empty()
            {
                actions.push(candidate(
                    GameAction::TapLandForMana { object_id: obj_id },
                    TacticalClass::Mana,
                    Some(player),
                ));
            }
        }
    }
    actions
}

fn mana_payment_actions(
    state: &GameState,
    player: PlayerId,
    convoke_mode: Option<ConvokeMode>,
) -> Vec<CandidateAction> {
    let mut actions = mana_tap_actions(state, player);
    // Always include PassPriority to finalize payment and CancelCast to back out
    actions.push(candidate(
        GameAction::PassPriority,
        TacticalClass::Pass,
        Some(player),
    ));
    actions.push(candidate(
        GameAction::CancelCast,
        TacticalClass::Pass,
        Some(player),
    ));
    if let Some(mode) = convoke_mode {
        // CR 702.6b: Summoning sickness does not restrict tapping for convoke.
        for (obj_id, obj) in &state.objects {
            if obj.is_convoke_eligible(player) {
                match mode {
                    ConvokeMode::Waterbend => {
                        // Waterbend: always colorless
                        actions.push(candidate(
                            GameAction::TapForConvoke {
                                object_id: *obj_id,
                                mana_type: crate::types::mana::ManaType::Colorless,
                            },
                            TacticalClass::Mana,
                            Some(player),
                        ));
                    }
                    ConvokeMode::Convoke => {
                        // CR 702.6a: Colorless (for generic) always available
                        actions.push(candidate(
                            GameAction::TapForConvoke {
                                object_id: *obj_id,
                                mana_type: crate::types::mana::ManaType::Colorless,
                            },
                            TacticalClass::Mana,
                            Some(player),
                        ));
                        // Plus one per color the creature has
                        for color in &obj.color {
                            actions.push(candidate(
                                GameAction::TapForConvoke {
                                    object_id: *obj_id,
                                    mana_type: mana_sources::mana_color_to_type(color),
                                },
                                TacticalClass::Mana,
                                Some(player),
                            ));
                        }
                    }
                }
            }
        }
    }
    actions
}
fn combinations(
    items: &[crate::types::identifiers::ObjectId],
    k: usize,
) -> Vec<Vec<crate::types::identifiers::ObjectId>> {
    if k == 0 {
        return vec![Vec::new()];
    }
    if items.len() < k {
        return Vec::new();
    }
    if items.len() == k {
        return vec![items.to_vec()];
    }

    let mut result = Vec::new();
    for mut combo in combinations(&items[1..], k - 1) {
        combo.insert(0, items[0]);
        result.push(combo);
    }
    result.extend(combinations(&items[1..], k));
    result
}

fn combinations_usize(items: &[usize], k: usize) -> Vec<Vec<usize>> {
    if k == 0 {
        return vec![Vec::new()];
    }
    if items.len() < k {
        return Vec::new();
    }
    if items.len() == k {
        return vec![items.to_vec()];
    }

    let mut result = Vec::new();
    for mut combo in combinations_usize(&items[1..], k - 1) {
        combo.insert(0, items[0]);
        result.push(combo);
    }
    result.extend(combinations_usize(&items[1..], k));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, BasicLandType,
        ChoiceType, ChosenAttribute, ChosenSubtypeKind, ContinuousModification, Effect,
        ManaProduction, QuantityExpr, StaticDefinition, TargetFilter, TargetRef,
    };
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaCostShard};
    use crate::types::zones::Zone;

    #[test]
    fn target_selection_uses_current_slot_legality() {
        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let target_a = create_object(
            &mut state,
            CardId(1),
            p0,
            "A".to_string(),
            Zone::Battlefield,
        );
        let target_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "B".to_string(),
            Zone::Battlefield,
        );

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: p0,
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(target_a), TargetRef::Object(target_b)],
                optional: false,
            }],
            target_constraints: Vec::new(),
            selection: Default::default(),
        };

        let actions = candidate_actions(&state);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0].action, GameAction::ChooseTarget { .. }));
    }

    #[test]
    fn declare_attackers_includes_pass_and_all_attack() {
        let state = GameState {
            waiting_for: WaitingFor::DeclareAttackers {
                player: PlayerId(0),
                valid_attacker_ids: vec![
                    crate::types::identifiers::ObjectId(1),
                    crate::types::identifiers::ObjectId(2),
                ],
                valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
            },
            ..GameState::new_two_player(42)
        };

        let actions = candidate_actions(&state);
        assert!(actions.iter().any(|a| matches!(a.action, GameAction::DeclareAttackers { ref attacks } if attacks.is_empty())));
        assert!(actions.iter().any(|a| matches!(a.action, GameAction::DeclareAttackers { ref attacks } if attacks.len() == 2)));
    }

    #[test]
    fn named_card_choice_uses_global_card_names() {
        let mut state = GameState::new_two_player(42);
        state.all_card_names = vec![
            "Lightning Bolt".to_string(),
            "Counterspell".to_string(),
            "lightning bolt".to_string(),
        ];
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source_id: None,
        };

        let actions = candidate_actions(&state);
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::ChooseOption { ref choice } if choice == "Lightning Bolt"
            )
        }));
    }

    #[test]
    fn sideboard_context_submits_current_lists() {
        let mut state = GameState::new_two_player(42);
        state.deck_pools = vec![crate::types::game_state::PlayerDeckPool {
            player: PlayerId(0),
            ..Default::default()
        }];
        state.waiting_for = WaitingFor::BetweenGamesSideboard {
            player: PlayerId(0),
            game_number: 2,
            score: Default::default(),
        };

        let actions = candidate_actions(&state);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            actions[0].action,
            GameAction::SubmitSideboard {
                ref main,
                ref sideboard,
            } if main.is_empty() && sideboard.is_empty()
        ));
    }

    #[test]
    fn priority_actions_include_spell_castable_via_gloomlake_verge_blue_mana() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let verge = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Gloomlake Verge".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&verge).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Blue],
                        },
                        restrictions: vec![],
                        expiry: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Black],
                        },
                        restrictions: vec![],
                        expiry: None,
                    },
                )
                .cost(AbilityCost::Tap)
                .sub_ability(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Unimplemented {
                        name: "activate_only_if_controls_land_subtype_any".to_string(),
                        description: Some("Island|Swamp".to_string()),
                    },
                )),
            );
        }

        create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Spyglass Siren".to_string(),
            Zone::Hand,
        );
        {
            let siren = state.players[0].hand[0];
            let obj = state.objects.get_mut(&siren).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = crate::types::mana::ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            };
        }

        let actions = candidate_actions(&state);
        assert!(actions.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::CastSpell {
                    card_id: CardId(101),
                    ..
                }
            )
        }));
    }

    #[test]
    fn priority_actions_include_spell_castable_via_multiversal_passage_chosen_swamp() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let passage = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Multiversal Passage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&passage).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.chosen_attributes
                .push(ChosenAttribute::BasicLandType(BasicLandType::Swamp));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::BasicLandType,
                    }]),
            );
        }

        let forest = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
        }

        create_object(
            &mut state,
            CardId(202),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Hand,
        );
        {
            let bat = state.players[0].hand[0];
            let obj = state.objects.get_mut(&bat).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = crate::types::mana::ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 1,
            };
        }

        state.layers_dirty = true;

        let actions = candidate_actions(&state);
        assert!(actions.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::CastSpell {
                    card_id: CardId(202),
                    ..
                }
            )
        }));
    }

    #[test]
    fn priority_actions_exclude_activated_ability_with_unmet_restriction() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let source = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Relic".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                )
                .activation_restrictions(vec![ActivationRestriction::OnlyOnceEachTurn]),
            );
        }
        state.activated_abilities_this_turn.insert((source, 0), 1);

        let actions = candidate_actions(&state);
        assert!(!actions.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::ActivateAbility {
                    source_id,
                    ability_index: 0,
                } if source_id == source
            )
        }));
    }

    #[test]
    fn mana_payment_actions_exclude_lands_without_activatable_mana() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        let blank_land = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Blank Land".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&blank_land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
        }

        let island = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&island).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
        }

        let actions = candidate_actions(&state);
        assert!(actions.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::TapLandForMana { object_id } if object_id == island
            )
        }));
        assert!(!actions.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::TapLandForMana { object_id } if object_id == blank_land
            )
        }));
    }

    #[test]
    fn priority_actions_do_not_offer_lands_as_cast_spells() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Hand,
        );
        let land = state.players[0].hand[0];
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Plains".to_string());
        }

        let actions = candidate_actions(&state);
        assert!(!actions.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::CastSpell {
                    card_id: CardId(400),
                    ..
                }
            )
        }));
    }

    #[test]
    fn ai_adventure_generates_face_choice() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::AdventureCastChoice {
            player: PlayerId(0),
            object_id: crate::types::identifiers::ObjectId(1),
            card_id: CardId(70),
        };

        let actions = candidate_actions(&state);
        assert_eq!(
            actions.len(),
            2,
            "Should generate creature and adventure face options"
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a.action, GameAction::ChooseAdventureFace { creature: true })));
        assert!(actions.iter().any(|a| matches!(
            a.action,
            GameAction::ChooseAdventureFace { creature: false }
        )));
    }
}
