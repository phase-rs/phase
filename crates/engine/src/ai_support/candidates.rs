use std::collections::{BTreeMap, HashSet};

use crate::game::casting;
use crate::game::combat::AttackTarget;
use crate::game::deck_loading::DeckEntry;
use crate::game::game_object::RoomDoor;
use crate::game::keywords;
use crate::game::mana_abilities;
use crate::game::mana_sources;
use crate::types::ability::ChoiceType;
use crate::types::ability::TargetRef;
use crate::types::actions::{CastChoice, GameAction, LearnOption};
use crate::types::card::LayoutKind;
use crate::types::card_type::CoreType;
use crate::types::game_state::{ConvokeMode, GameState, TargetSelectionSlot, WaitingFor};
use crate::types::identifiers::ObjectId;
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

fn collect_evidence_candidate_combos(
    state: &GameState,
    cards: &[ObjectId],
    minimum_mana_value: u32,
) -> Vec<Vec<ObjectId>> {
    const MAX_COMBOS: usize = 16;
    fn push_collect_evidence_combo(
        state: &GameState,
        combos: &mut Vec<Vec<ObjectId>>,
        seen: &mut HashSet<Vec<u64>>,
        minimum_mana_value: u32,
        combo: Vec<ObjectId>,
    ) {
        if combo.is_empty() || combos.len() >= MAX_COMBOS {
            return;
        }
        let total: u32 = combo
            .iter()
            .filter_map(|id| state.objects.get(id))
            .map(|obj| obj.mana_cost.mana_value())
            .sum();
        if total < minimum_mana_value {
            return;
        }
        let mut key: Vec<u64> = combo.iter().map(|id| id.0).collect();
        key.sort_unstable();
        if seen.insert(key) {
            combos.push(combo);
        }
    }

    let mut valued_cards: Vec<(ObjectId, u32)> = cards
        .iter()
        .filter_map(|&id| {
            state
                .objects
                .get(&id)
                .map(|obj| (id, obj.mana_cost.mana_value()))
        })
        .collect();
    valued_cards.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0 .0.cmp(&a.0 .0)));

    let mut combos = Vec::new();
    let mut seen = HashSet::new();

    for &(id, value) in &valued_cards {
        if value >= minimum_mana_value {
            push_collect_evidence_combo(
                state,
                &mut combos,
                &mut seen,
                minimum_mana_value,
                vec![id],
            );
        }
    }

    for start_idx in 0..valued_cards.len() {
        if combos.len() >= MAX_COMBOS {
            break;
        }
        let mut combo = vec![valued_cards[start_idx].0];
        let mut total = valued_cards[start_idx].1;
        for &(id, value) in valued_cards.iter().skip(start_idx + 1) {
            if total >= minimum_mana_value {
                break;
            }
            combo.push(id);
            total += value;
        }
        push_collect_evidence_combo(state, &mut combos, &mut seen, minimum_mana_value, combo);
    }

    let mut ascending = valued_cards.clone();
    ascending.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0 .0.cmp(&b.0 .0)));
    let mut combo = Vec::new();
    let mut total = 0;
    for &(id, value) in &ascending {
        if total >= minimum_mana_value {
            break;
        }
        combo.push(id);
        total += value;
    }
    push_collect_evidence_combo(state, &mut combos, &mut seen, minimum_mana_value, combo);

    combos
}

/// `GameAction::Concede` is intentionally NOT produced by any of the
/// `candidate_actions*` enumerators. Per CR 104.3a a player may concede "at any
/// time" regardless of priority or `WaitingFor` state, so `engine.rs::apply()`
/// dispatches it before the normal `(WaitingFor, action)` match. Exposing it as
/// a legal-action candidate would (a) let AI search prune toward suicide and
/// (b) duplicate the always-available UI affordance the network/UI layer
/// surfaces directly. Callers that need to submit a concede do so by
/// constructing `GameAction::Concede { player_id }` directly.
pub fn candidate_actions_exact(state: &GameState) -> Vec<CandidateAction> {
    match &state.waiting_for {
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
        WaitingFor::ExploreChoice {
            player, choosable, ..
        } => choosable
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
        WaitingFor::DiscoverChoice { player, .. } => vec![
            candidate(
                GameAction::DiscoverChoice {
                    choice: CastChoice::Cast,
                },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::DiscoverChoice {
                    choice: CastChoice::Decline,
                },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        // CR 702.85a: Cascade offers a binary cast/decline choice. Tactical
        // ordering: place Cast first when the hit has at least one legal
        // target (or no targets at all — typically a permanent or untargeted
        // spell). When the hit would fizzle (targeted spell with no legal
        // targets), place Decline first so the bottom-shuffle outcome is
        // preferred over a no-effect cast that still consumes the resource.
        // Both candidates remain legal — the selector / search may still
        // pick either based on deeper evaluation.
        WaitingFor::CascadeChoice {
            player, hit_card, ..
        } => {
            let cast_first = state.objects.get(hit_card).is_some_and(|obj| {
                crate::game::casting::spell_has_legal_targets(state, obj, *player)
            });
            let cast = candidate(
                GameAction::CascadeChoice {
                    choice: CastChoice::Cast,
                },
                TacticalClass::Selection,
                Some(*player),
            );
            let decline = candidate(
                GameAction::CascadeChoice {
                    choice: CastChoice::Decline,
                },
                TacticalClass::Selection,
                Some(*player),
            );
            if cast_first {
                vec![cast, decline]
            } else {
                vec![decline, cast]
            }
        }
        WaitingFor::LearnChoice { player, hand_cards } => {
            let mut actions: Vec<_> = hand_cards
                .iter()
                .map(|&card_id| {
                    candidate(
                        GameAction::LearnDecision {
                            choice: LearnOption::Rummage { card_id },
                        },
                        TacticalClass::Selection,
                        Some(*player),
                    )
                })
                .collect();
            actions.push(candidate(
                GameAction::LearnDecision {
                    choice: LearnOption::Skip,
                },
                TacticalClass::Selection,
                Some(*player),
            ));
            actions
        }
        WaitingFor::TopOrBottomChoice { player, .. }
        | WaitingFor::ClashCardPlacement { player, .. } => vec![
            candidate(
                GameAction::ChooseTopOrBottom { top: true },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::ChooseTopOrBottom { top: false },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
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
        _ => Vec::new(),
    }
}

pub fn candidate_actions_broad(state: &GameState) -> Vec<CandidateAction> {
    let actions = match &state.waiting_for {
        WaitingFor::Priority { player } => priority_actions(state, *player),
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
        // CR 702.122a: Generate valid creature subsets whose total power >= crew_power.
        WaitingFor::CrewVehicle {
            player,
            vehicle_id,
            crew_power,
            eligible_creatures,
        } => crew_vehicle_candidates(state, *player, *vehicle_id, *crew_power, eligible_creatures),
        // CR 702.184a: Offer each eligible creature as the station cost payer.
        WaitingFor::StationTarget {
            player,
            spacecraft_id,
            eligible_creatures,
        } => station_target_candidates(*player, *spacecraft_id, eligible_creatures),
        // CR 702.171a: Generate valid creature subsets whose total power >= saddle_power.
        WaitingFor::SaddleMount {
            player,
            mount_id,
            saddle_power,
            eligible_creatures,
        } => saddle_mount_candidates(state, *player, *mount_id, *saddle_power, eligible_creatures),
        WaitingFor::TapCreaturesForManaAbility {
            player,
            count,
            creatures,
            ..
        } => select_cards_variants(*player, creatures, Some(*count)),
        // CR 117.1 + CR 118.3 + CR 605.3b: Food Chain class — pick which
        // permanent(s) to exile to pay the mana ability cost.
        WaitingFor::ExileFromBattlefieldForManaAbility {
            player,
            count,
            permanents,
            ..
        } => select_cards_variants(*player, permanents, Some(*count)),
        WaitingFor::PayManaAbilityMana {
            player, options, ..
        } => options
            .iter()
            .map(|plan| {
                candidate(
                    GameAction::PayManaAbilityMana {
                        payment: plan.clone(),
                    },
                    TacticalClass::Mana,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::ChooseManaColor { player, choice, .. } => {
            use crate::types::game_state::{ManaChoice, ManaChoicePrompt};
            match choice {
                ManaChoicePrompt::SingleColor { options } => options
                    .iter()
                    .map(|&color| {
                        candidate(
                            GameAction::ChooseManaColor {
                                choice: ManaChoice::SingleColor(color),
                            },
                            TacticalClass::Mana,
                            Some(*player),
                        )
                    })
                    .collect(),
                ManaChoicePrompt::Combination { options } => options
                    .iter()
                    .map(|combo| {
                        candidate(
                            GameAction::ChooseManaColor {
                                choice: ManaChoice::Combination(combo.clone()),
                            },
                            TacticalClass::Mana,
                            Some(*player),
                        )
                    })
                    .collect(),
            }
        }
        WaitingFor::ScryChoice { player, cards } => select_cards_variants(*player, cards, None),
        WaitingFor::DigChoice {
            player,
            keep_count,
            up_to,
            selectable_cards,
            ..
        } => {
            // Use pre-filtered selectable_cards for combination generation
            let max_keep = (*keep_count).min(selectable_cards.len());
            if *up_to {
                // Generate combinations for all valid sizes 0..=max_keep
                (0..=max_keep)
                    .flat_map(|size| combinations(selectable_cards, size))
                    .map(|combo| {
                        candidate(
                            GameAction::SelectCards { cards: combo },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            } else {
                combinations(selectable_cards, max_keep)
                    .into_iter()
                    .map(|combo| {
                        candidate(
                            GameAction::SelectCards { cards: combo },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            }
        }
        WaitingFor::SurveilChoice { player, cards } => select_cards_variants(*player, cards, None),
        WaitingFor::RevealChoice {
            player,
            cards,
            optional,
            ..
        } => {
            // CR 701.20a: Normal reveal forces exactly one pick. Optional reveal
            // (e.g., reveal-lands) additionally permits an empty selection to
            // signal "I decline to reveal" — the source's decline branch fires.
            let mut variants = select_cards_variants(*player, cards, Some(1));
            if *optional {
                variants.push(candidate(
                    GameAction::SelectCards { cards: vec![] },
                    TacticalClass::Selection,
                    Some(*player),
                ));
            }
            variants
        }
        WaitingFor::SearchChoice {
            player,
            cards,
            count,
            up_to,
            constraint,
            ..
        } => {
            // CR 107.1c + CR 701.23d: "any number of" / "up to N" searches enumerate
            // combination sizes 0..=count; exact-count searches enumerate only `count`.
            let sizes: Vec<usize> = if *up_to {
                (0..=*count).collect()
            } else {
                vec![*count]
            };
            // Engine-side beam cap. Required (not optional) because every candidate
            // returned here flows into `PlannerServices::validate_candidates`, which
            // clones state + applies the action per candidate. Without a cap, a
            // count=4 search against an 80-card library produces ~C(80,4) ≈ 1.6M
            // combinations and stalls validation for hours. The cap is constraint-
            // aware so DistinctNames searches collapse duplicate-named entries
            // before combinatorial explosion (Gifts Ungiven against an 80-card pool
            // with 8 distinct names → 8 candidate ids, C(8,4)=70 legal combos).
            //
            // Correctness note: the cap may exclude legal moves the AI could
            // theoretically prefer, so it is a perf-bounded approximation, not a
            // legality filter. Player-driven SearchChoice flows through the
            // engine's submission guard regardless of what this list contains.
            const ENGINE_CANDIDATE_CAP: usize = 12;
            let beam_cards = cap_search_choice_pool(state, cards, constraint, ENGINE_CANDIDATE_CAP);
            sizes
                .into_iter()
                .flat_map(|size| combinations(&beam_cards, size))
                // CR 608.2c: Drop combinations that violate the printed-text
                // selection restriction (e.g., Gifts Ungiven's "with different
                // names") so the AI never scores or submits an illegal pick.
                .filter(|combo| {
                    crate::game::effects::search_library::selection_satisfies_constraint(
                        state, combo, constraint,
                    )
                })
                .map(|combo| {
                    candidate(
                        GameAction::SelectCards { cards: combo },
                        TacticalClass::Selection,
                        Some(*player),
                    )
                })
                .collect()
        }
        // CR 700.2: Choose card(s) from a tracked set (exiled/revealed cards).
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            up_to,
            constraint,
            ..
        } => {
            let sizes = if *up_to {
                (0..=*count).collect()
            } else {
                vec![*count]
            };
            sizes
                .into_iter()
                .flat_map(|size| combinations(cards, size))
                .filter(|combo| {
                    crate::game::effects::choose_from_zone::selection_satisfies_constraint(
                        state,
                        combo,
                        constraint.as_ref(),
                    )
                })
                .map(|combo| {
                    candidate(
                        GameAction::SelectCards { cards: combo },
                        TacticalClass::Selection,
                        Some(*player),
                    )
                })
                .collect()
        }
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            up_to,
            ..
        } => {
            if *up_to {
                (0..=*count)
                    .flat_map(|size| combinations(cards, size))
                    .map(|combo| {
                        candidate(
                            GameAction::SelectCards { cards: combo },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            } else {
                combinations(cards, *count)
                    .into_iter()
                    .map(|combo| {
                        candidate(
                            GameAction::SelectCards { cards: combo },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            }
        }
        // CR 101.4: Generate all valid per-category permanent assignments.
        WaitingFor::CategoryChoice {
            player,
            eligible_per_category,
            ..
        } => {
            // Generate all valid combinations: one choice per category (or None if empty).
            // For AI simplicity, enumerate the Cartesian product of per-category options.
            let mut all_combos: Vec<Vec<Option<ObjectId>>> = vec![vec![]];
            for category_eligible in eligible_per_category {
                let mut new_combos = Vec::new();
                let options: Vec<Option<ObjectId>> = if category_eligible.is_empty() {
                    vec![None]
                } else {
                    category_eligible.iter().map(|&id| Some(id)).collect()
                };
                for existing in &all_combos {
                    for opt in &options {
                        // Skip if this object was already chosen in a prior category.
                        if let Some(_id) = opt {
                            if existing.iter().any(|prev| prev == opt) {
                                // Allow None duplicates, but not object duplicates.
                                // However, also need None as fallback if all are taken.
                                continue;
                            }
                        }
                        let mut combo = existing.clone();
                        combo.push(*opt);
                        new_combos.push(combo);
                    }
                    // If all options for this category conflict, allow None.
                    if category_eligible
                        .iter()
                        .all(|id| existing.contains(&Some(*id)))
                    {
                        let mut combo = existing.clone();
                        combo.push(None);
                        new_combos.push(combo);
                    }
                }
                all_combos = new_combos;
            }
            // Cap at a reasonable number to avoid combinatorial explosion.
            all_combos.truncate(100);
            all_combos
                .into_iter()
                .map(|choices| {
                    candidate(
                        GameAction::SelectCategoryPermanents { choices },
                        TacticalClass::Selection,
                        Some(*player),
                    )
                })
                .collect()
        }
        WaitingFor::BetweenGamesSideboard { player, .. } => sideboard_actions(state, *player),
        WaitingFor::NamedChoice {
            player,
            options,
            choice_type,
            ..
        } => named_choice_actions(state, *player, options, choice_type),
        // CR 701.38: Vote — every option is a legal candidate; the AI picks via
        // the standard ChooseOption action. Each remaining vote produces an
        // identical action set (CR 701.38d allows repeats), so emitting one
        // candidate per option is correct: the engine re-enters VoteChoice for
        // each subsequent vote.
        WaitingFor::VoteChoice {
            player, options, ..
        } => options
            .iter()
            .map(|opt| {
                candidate(
                    GameAction::ChooseOption {
                        choice: opt.clone(),
                    },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::ModeChoice {
            player,
            modal,
            pending_cast,
        } => {
            let actions = if modal.allow_repeat_modes {
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
            };
            // CR 702.172b: For Spree spells, filter out mode combinations the player
            // cannot afford. Each mode has an additional cost that sums with the base cost.
            if modal.mode_costs.is_empty() {
                actions
            } else {
                actions
                    .into_iter()
                    .filter(|ca| {
                        let indices = match &ca.action {
                            GameAction::SelectModes { indices } => indices,
                            _ => return true,
                        };
                        let spree_total = indices.iter().fold(
                            crate::types::mana::ManaCost::zero(),
                            |acc, &idx| {
                                crate::game::restrictions::add_mana_cost(
                                    &acc,
                                    &modal.mode_costs[idx],
                                )
                            },
                        );
                        let total = crate::game::restrictions::add_mana_cost(
                            &pending_cast.cost,
                            &spree_total,
                        );
                        casting::can_pay_cost_after_auto_tap(
                            state,
                            *player,
                            pending_cast.object_id,
                            &total,
                        )
                    })
                    .collect()
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
        WaitingFor::ConniveDiscard {
            player,
            count,
            cards,
            ..
        }
        | WaitingFor::DiscardToHandSize {
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
        WaitingFor::DiscardChoice {
            player,
            count,
            cards,
            up_to,
            unless_filter,
            source_id,
            ..
        } => {
            // CR 701.9b: When up_to, generate combinations for all valid sizes 0..=count.
            let mut actions: Vec<_> = if *up_to {
                (0..=*count)
                    .flat_map(|size| combinations(cards, size))
                    .map(|combo| {
                        candidate(
                            GameAction::SelectCards { cards: combo },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            } else {
                combinations(cards, *count)
                    .into_iter()
                    .map(|combo| {
                        candidate(
                            GameAction::SelectCards { cards: combo },
                            TacticalClass::Selection,
                            Some(*player),
                        )
                    })
                    .collect()
            };
            // CR 608.2c: "discard N unless you discard a [type]" — also generate
            // single-card selections for cards matching the unless filter.
            // Guard: skip when count == 1, since combinations already covers all singles.
            if *count > 1 && !*up_to {
                if let Some(filter) = unless_filter {
                    let ctx = crate::game::filter::FilterContext::from_source(state, *source_id);
                    for &card_id in cards {
                        if crate::game::filter::matches_target_filter(state, card_id, filter, &ctx)
                        {
                            actions.push(candidate(
                                GameAction::SelectCards {
                                    cards: vec![card_id],
                                },
                                TacticalClass::Selection,
                                Some(*player),
                            ));
                        }
                    }
                }
            }
            actions
        }
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
        // CR 107.4f + CR 601.2f: AI picks per-shard Phyrexian payment.
        // Heuristic (life threshold): with life > 6, the AI prefers 2-life per shard for
        // tempo (keep mana for other plays); with life <= 6, the AI preserves life.
        // Shards with only one viable option use that option.
        WaitingFor::PhyrexianPayment { player, shards, .. } => {
            use crate::types::game_state::{ShardChoice, ShardOptions};
            let life = state
                .players
                .iter()
                .find(|p| p.id == *player)
                .map(|p| p.life)
                .unwrap_or(0);
            let prefer_life = life > 6;
            let choices: Vec<ShardChoice> = shards
                .iter()
                .map(|shard| match shard.options {
                    ShardOptions::ManaOnly => ShardChoice::PayMana,
                    ShardOptions::LifeOnly => ShardChoice::PayLife,
                    ShardOptions::ManaOrLife => {
                        if prefer_life {
                            ShardChoice::PayLife
                        } else {
                            ShardChoice::PayMana
                        }
                    }
                })
                .collect();
            vec![candidate(
                GameAction::SubmitPhyrexianChoices { choices },
                TacticalClass::Selection,
                Some(*player),
            )]
        }
        // CR 601.2b: Defiler cycle — accept or decline life payment for mana reduction.
        WaitingFor::DefilerPayment { player, .. } => vec![
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
        WaitingFor::DiscardForManaAbility {
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
        WaitingFor::ReturnToHandForCost {
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
        // Blight: AI selects creatures to put -1/-1 counters on as cost
        WaitingFor::BlightChoice {
            player,
            count,
            creatures,
            ..
        } => combinations(creatures, *count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 702.34a: AI selects creatures to tap as part of paying flashback tap cost.
        WaitingFor::TapCreaturesForSpellCost {
            player,
            count,
            creatures,
            ..
        } => combinations(creatures, *count)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 118.9a + CR 601.2b + CR 601.2h: AI selects cards to exile as part
        // of paying an alternative or additional casting cost — escape
        // (CR 702.138a, graveyard) or pitch spells (hand).
        WaitingFor::ExileForCost {
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
        WaitingFor::CollectEvidenceChoice {
            player,
            minimum_mana_value,
            cards,
            ..
        } => collect_evidence_candidate_combos(state, cards, *minimum_mana_value)
            .into_iter()
            .map(|combo| {
                candidate(
                    GameAction::SelectCards { cards: combo },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::HarmonizeTapChoice {
            player,
            eligible_creatures,
            ..
        } => {
            let mut actions = vec![candidate(
                GameAction::HarmonizeTap { creature_id: None },
                TacticalClass::Pass,
                Some(*player),
            )];
            for &cid in eligible_creatures {
                actions.push(candidate(
                    GameAction::HarmonizeTap {
                        creature_id: Some(cid),
                    },
                    TacticalClass::Selection,
                    Some(*player),
                ));
            }
            actions
        }
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
        // CR 712.12: Both MDFC land faces are playable — offer front or back
        WaitingFor::ModalFaceChoice { player, .. } => vec![
            candidate(
                GameAction::ChooseModalFace { back_face: false },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::ChooseModalFace { back_face: true },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        WaitingFor::WarpCostChoice { player, .. } => vec![
            candidate(
                GameAction::ChooseWarpCost { use_warp: true },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::ChooseWarpCost { use_warp: false },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        WaitingFor::EvokeCostChoice { player, .. } => vec![
            candidate(
                GameAction::ChooseEvokeCost { use_evoke: true },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::ChooseEvokeCost { use_evoke: false },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        WaitingFor::OverloadCostChoice { player, .. } => vec![
            candidate(
                GameAction::ChooseOverloadCost { use_overload: true },
                TacticalClass::Selection,
                Some(*player),
            ),
            candidate(
                GameAction::ChooseOverloadCost {
                    use_overload: false,
                },
                TacticalClass::Selection,
                Some(*player),
            ),
        ],
        WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::TributeChoice { .. } => {
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
        // CR 508.1d + CR 509.1c: Combat tax — active player (attacks) or defending
        // player (blocks) chooses to pay the locked-in aggregate cost or decline
        // (dropping the taxed creatures from the declaration).
        WaitingFor::CombatTaxPayment { player, .. } => {
            vec![
                candidate(
                    GameAction::PayCombatTax { accept: true },
                    TacticalClass::Selection,
                    Some(*player),
                ),
                candidate(
                    GameAction::PayCombatTax { accept: false },
                    TacticalClass::Selection,
                    Some(*player),
                ),
            ]
        }
        // CR 702.21a: Ward discard cost — choose a card from hand.
        WaitingFor::WardDiscardChoice { player, cards, .. } => cards
            .iter()
            .map(|&card| {
                candidate(
                    GameAction::SelectCards { cards: vec![card] },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 702.21a: Ward sacrifice cost — choose a permanent.
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => permanents
            .iter()
            .map(|&perm| {
                candidate(
                    GameAction::SelectCards { cards: vec![perm] },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 704.5j: Choose which legend to keep.
        WaitingFor::ChooseLegend {
            player, candidates, ..
        } => candidates
            .iter()
            .map(|&keep| {
                candidate(
                    GameAction::ChooseLegend { keep },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 310.10 + CR 704.5w + CR 704.5x: controller chooses a new protector.
        WaitingFor::BattleProtectorChoice {
            player, candidates, ..
        } => candidates
            .iter()
            .map(|&protector| {
                candidate(
                    GameAction::ChooseBattleProtector { protector },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 701.54a: Choose a ring-bearer from candidate creatures.
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
        // CR 701.49a: Choose which dungeon to venture into.
        WaitingFor::ChooseDungeon { player, options } => options
            .iter()
            .map(|&dungeon| {
                candidate(
                    GameAction::ChooseDungeon { dungeon },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 309.5a: Choose which room to advance to at a branch point.
        WaitingFor::ChooseDungeonRoom {
            player, options, ..
        } => options
            .iter()
            .map(|&room_index| {
                candidate(
                    GameAction::ChooseDungeonRoom { room_index },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 702.139a: Companion reveal candidates
        WaitingFor::CompanionReveal {
            player,
            eligible_companions,
        } => {
            let mut actions: Vec<CandidateAction> = eligible_companions
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    candidate(
                        GameAction::DeclareCompanion {
                            card_index: Some(i),
                        },
                        TacticalClass::Selection,
                        Some(*player),
                    )
                })
                .collect();
            // Always offer the option to decline
            actions.push(candidate(
                GameAction::DeclareCompanion { card_index: None },
                TacticalClass::Selection,
                Some(*player),
            ));
            actions
        }
        // CR 701.34a: Proliferate — choose any subset of eligible permanents/players.
        WaitingFor::ProliferateChoice { player, eligible } => {
            let mut actions = vec![
                candidate(
                    GameAction::SelectTargets {
                        targets: eligible.clone(),
                    },
                    TacticalClass::Selection,
                    Some(*player),
                ),
                candidate(
                    GameAction::SelectTargets {
                        targets: Vec::new(),
                    },
                    TacticalClass::Selection,
                    Some(*player),
                ),
            ];
            for target in eligible {
                actions.push(candidate(
                    GameAction::SelectTargets {
                        targets: vec![target.clone()],
                    },
                    TacticalClass::Selection,
                    Some(*player),
                ));
            }
            actions
        }
        // CR 701.36a: Populate — choose a creature token to copy.
        WaitingFor::PopulateChoice {
            player,
            valid_tokens,
            ..
        } => valid_tokens
            .iter()
            .map(|&token_id| {
                candidate(
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(token_id)),
                    },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 707.10c: Copy retargeting — pick the first legal alternative for
        // each slot when populated (initial target selection for Prepare /
        // Paradigm copies). Falls back to keeping `current` when no
        // alternatives are exposed (classic copy_spell::resolve path).
        WaitingFor::CopyRetarget {
            player,
            target_slots,
            ..
        } => {
            let targets: Vec<_> = target_slots
                .iter()
                .map(|s| {
                    s.legal_alternatives
                        .first()
                        .cloned()
                        .unwrap_or_else(|| s.current.clone())
                })
                .collect();
            vec![candidate(
                GameAction::SelectTargets { targets },
                TacticalClass::Selection,
                Some(*player),
            )]
        }
        // CR 510.1c/d: Assign combat damage — greedy (lethal to each in order, remainder to last).
        WaitingFor::AssignCombatDamage {
            player,
            total_damage,
            blockers,
            assignment_modes,
            trample,
            pw_loyalty,
            attack_target,
            ..
        } => {
            let mut remaining = *total_damage;
            let mut assignments = Vec::new();
            for slot in blockers {
                let assign = remaining.min(slot.lethal_minimum);
                assignments.push((slot.blocker_id, assign));
                remaining = remaining.saturating_sub(assign);
            }
            // Non-trample: dump remainder to last blocker so total == power.
            if trample.is_none() && remaining > 0 {
                if let Some(last) = assignments.last_mut() {
                    last.1 += remaining;
                    remaining = 0;
                }
            }
            // CR 702.19c: For trample-over-PW attacking a PW, split excess:
            // loyalty-worth to PW, remainder to controller.
            let (trample_dmg, ctrl_dmg) = if *trample
                == Some(crate::game::combat::TrampleKind::OverPlaneswalkers)
                && matches!(
                    attack_target,
                    crate::game::combat::AttackTarget::Planeswalker(_)
                ) {
                let loyalty = pw_loyalty.unwrap_or(0);
                let to_pw = remaining.min(loyalty);
                let to_ctrl = remaining.saturating_sub(to_pw);
                (to_pw, to_ctrl)
            } else {
                (if trample.is_some() { remaining } else { 0 }, 0)
            };
            let mut candidates = vec![candidate(
                GameAction::AssignCombatDamage {
                    mode: crate::types::game_state::CombatDamageAssignmentMode::Normal,
                    assignments,
                    trample_damage: trample_dmg,
                    controller_damage: ctrl_dmg,
                },
                TacticalClass::Selection,
                Some(*player),
            )];
            if assignment_modes
                .contains(&crate::types::game_state::CombatDamageAssignmentMode::AsThoughUnblocked)
            {
                candidates.push(candidate(
                    GameAction::AssignCombatDamage {
                        mode:
                            crate::types::game_state::CombatDamageAssignmentMode::AsThoughUnblocked,
                        assignments: Vec::new(),
                        trample_damage: 0,
                        controller_damage: 0,
                    },
                    TacticalClass::Selection,
                    Some(*player),
                ));
            }
            candidates
        }
        // CR 601.2d: Distribute — even split as default.
        WaitingFor::DistributeAmong {
            player,
            total,
            targets,
            ..
        } => {
            if targets.is_empty() {
                Vec::new()
            } else {
                let per_target = (*total as usize / targets.len()).max(1) as u32;
                let mut dist: Vec<_> = targets.iter().map(|t| (t.clone(), per_target)).collect();
                let assigned: u32 = dist.iter().map(|(_, a)| *a).sum();
                if assigned < *total {
                    if let Some(last) = dist.last_mut() {
                        last.1 += *total - assigned;
                    }
                }
                vec![candidate(
                    GameAction::DistributeAmong { distribution: dist },
                    TacticalClass::Selection,
                    Some(*player),
                )]
            }
        }
        // CR 115.7: Retarget — keep current targets as default.
        WaitingFor::RetargetChoice {
            player,
            current_targets,
            ..
        } => {
            vec![candidate(
                GameAction::RetargetSpell {
                    new_targets: current_targets.clone(),
                },
                TacticalClass::Selection,
                Some(*player),
            )]
        }
        // CR 701.62a: AI selects one card to manifest — one action per card option
        WaitingFor::ManifestDreadChoice { player, cards } => cards
            .iter()
            .map(|&card_id| {
                candidate(
                    GameAction::SelectCards {
                        cards: vec![card_id],
                    },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::ChooseXValue { player, max, .. } => (0..=*max)
            .map(|value| {
                candidate(
                    GameAction::ChooseX { value },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        // CR 107.1c + CR 107.14: Enumerate every legal amount in [min, max].
        // AI search layer picks among these; for a damage-scaling effect like
        // Galvanic Discharge the evaluator prefers the maximum (most damage).
        WaitingFor::PayAmountChoice {
            player, min, max, ..
        } => (*min..=*max)
            .map(|amount| {
                candidate(
                    GameAction::SubmitPayAmount { amount },
                    TacticalClass::Selection,
                    Some(*player),
                )
            })
            .collect(),
        WaitingFor::GameOver { .. } => Vec::new(),
        WaitingFor::ReplacementChoice { .. }
        | WaitingFor::CopyTargetChoice { .. }
        | WaitingFor::ExploreChoice { .. }
        | WaitingFor::DiscoverChoice { .. }
        | WaitingFor::CascadeChoice { .. }
        | WaitingFor::LearnChoice { .. }
        | WaitingFor::TopOrBottomChoice { .. }
        | WaitingFor::ClashCardPlacement { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. }
        | WaitingFor::MulliganDecision { .. }
        | WaitingFor::MulliganBottomCards { .. } => Vec::new(),
        // CR 702.xxx: Paradigm (Strixhaven) — enumerate each exiled paradigm
        // source as a cast candidate plus a pass option. Assign when WotC
        // publishes SOS CR update.
        // CR 702.94a + CR 603.11: Miracle reveal — offer accept (cast for the
        // miracle mana cost) and decline (DecideOptionalEffect { accept: false }).
        // AI heuristic: reveal-and-cast when the miracle cost is affordable from
        // the player's current mana pool including auto-tappable lands; otherwise
        // decline so the AI isn't blocked on an unaffordable offer.
        // CR 702.94a: Miracle reveal — AI always reveals (pushes a trigger
        // on the stack; cost is checked at MiracleCastOffer resolution).
        WaitingFor::MiracleReveal {
            player, object_id, ..
        } => {
            let card_id = state
                .objects
                .get(object_id)
                .map(|o| o.card_id)
                .unwrap_or(crate::types::identifiers::CardId(0));
            vec![
                candidate(
                    GameAction::CastSpellAsMiracle {
                        object_id: *object_id,
                        card_id,
                    },
                    TacticalClass::Spell,
                    Some(*player),
                ),
                candidate(
                    GameAction::DecideOptionalEffect { accept: false },
                    TacticalClass::Pass,
                    Some(*player),
                ),
            ]
        }
        // CR 702.94a: Miracle cast offer — the trigger has resolved; cast if
        // the miracle cost is affordable, otherwise decline.
        WaitingFor::MiracleCastOffer {
            player,
            object_id,
            cost,
        } => {
            let card_id = state
                .objects
                .get(object_id)
                .map(|o| o.card_id)
                .unwrap_or(crate::types::identifiers::CardId(0));
            let can_pay =
                crate::game::casting::can_pay_cost_after_auto_tap(state, *player, *object_id, cost);
            let mut v: Vec<CandidateAction> = Vec::new();
            if can_pay {
                v.push(candidate(
                    GameAction::CastSpellAsMiracle {
                        object_id: *object_id,
                        card_id,
                    },
                    TacticalClass::Spell,
                    Some(*player),
                ));
            }
            v.push(candidate(
                GameAction::DecideOptionalEffect { accept: false },
                TacticalClass::Pass,
                Some(*player),
            ));
            v
        }
        // CR 702.35a: Madness cast offer — cast if the madness cost is affordable,
        // otherwise decline and put the card into its owner's graveyard.
        WaitingFor::MadnessCastOffer {
            player,
            object_id,
            cost,
        } => {
            let card_id = state
                .objects
                .get(object_id)
                .map(|o| o.card_id)
                .unwrap_or(crate::types::identifiers::CardId(0));
            let can_pay =
                crate::game::casting::can_pay_cost_after_auto_tap(state, *player, *object_id, cost);
            let mut v: Vec<CandidateAction> = Vec::new();
            if can_pay {
                v.push(candidate(
                    GameAction::CastSpellAsMadness {
                        object_id: *object_id,
                        card_id,
                    },
                    TacticalClass::Spell,
                    Some(*player),
                ));
            }
            v.push(candidate(
                GameAction::DecideOptionalEffect { accept: false },
                TacticalClass::Pass,
                Some(*player),
            ));
            v
        }
        WaitingFor::ParadigmCastOffer { player, offers } => {
            let mut v: Vec<CandidateAction> = offers
                .iter()
                .map(|source| {
                    candidate(
                        GameAction::CastParadigmCopy { source: *source },
                        TacticalClass::Spell,
                        Some(*player),
                    )
                })
                .collect();
            v.push(candidate(
                GameAction::PassParadigmOffer,
                TacticalClass::Selection,
                Some(*player),
            ));
            v
        }
    };

    actions
}

pub fn candidate_actions(state: &GameState) -> Vec<CandidateAction> {
    let mut actions = candidate_actions_exact(state);
    actions.extend(candidate_actions_broad(state));

    if state.waiting_for.has_pending_cast() {
        if let Some(player) = state.waiting_for.acting_player() {
            actions.push(candidate(
                GameAction::CancelCast,
                TacticalClass::Pass,
                Some(player),
            ));
        }
    }

    for action in &mut actions {
        action.metadata.actor = action.metadata.actor.map(|player| {
            crate::game::turn_control::authorized_submitter_for_player(state, player)
        });
    }

    actions
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
        && state.lands_played_this_turn
            < state.max_lands_per_turn.saturating_add(
                crate::game::static_abilities::additional_land_drops(state, player),
            )
        // CR 305.2: Don't offer PlayLand candidates while the player is under a
        // CantPlayLand prohibition — mirrors the runtime guard in handle_play_land.
        && !crate::game::static_abilities::player_has_static_other(state, player, "CantPlayLand")
    {
        for &obj_id in &p.hand {
            if let Some(obj) = state.objects.get(&obj_id) {
                // CR 712.12: Also detect MDFCs where the back face is a land
                let is_playable_land = obj.card_types.core_types.contains(&CoreType::Land)
                    || obj.back_face.as_ref().is_some_and(|bf| {
                        bf.layout_kind == Some(LayoutKind::Modal)
                            && bf.card_types.core_types.contains(&CoreType::Land)
                    });
                if is_playable_land {
                    actions.push(candidate(
                        GameAction::PlayLand {
                            object_id: obj_id,
                            card_id: obj.card_id,
                        },
                        TacticalClass::Land,
                        Some(player),
                    ));
                }
            }
        }
        // CR 604.2 + CR 305.1: Lands playable from graveyard via static permission
        for (obj_id, _source) in casting::graveyard_lands_playable_by_permission(state, player) {
            if let Some(obj) = state.objects.get(&obj_id) {
                actions.push(candidate(
                    GameAction::PlayLand {
                        object_id: obj_id,
                        card_id: obj.card_id,
                    },
                    TacticalClass::Land,
                    Some(player),
                ));
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
                    object_id,
                    card_id: obj.card_id,
                    targets: Vec::new(),
                },
                TacticalClass::Spell,
                Some(player),
            ));
        }
    }

    // CR 601.2b + CR 118.9a: Opt-in CastFromHandFree once-per-turn candidates
    // (Zaffai and the Tempests). Each (hand spell, source) pair that passes the
    // filter AND hasn't had its slot consumed this turn yields one candidate.
    for (object_id, source_id, _freq) in casting::hand_cast_free_candidates(state, player) {
        let Some(obj) = state.objects.get(&object_id) else {
            continue;
        };
        actions.push(candidate(
            GameAction::CastSpellForFree {
                object_id,
                card_id: obj.card_id,
                source_id,
            },
            TacticalClass::Spell,
            Some(player),
        ));
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
                // CR 702.xxx: Prepare (Strixhaven) — priority-time offer to
                // cast a copy of the prepare-spell face. Gated on
                // `prepared.is_some()` (single-authority state flag managed
                // by `game::effects::prepare`). Assign when WotC publishes
                // SOS CR update.
                if obj.prepared.is_some() {
                    actions.push(candidate(
                        GameAction::CastPreparedCopy { source: obj_id },
                        TacticalClass::Spell,
                        Some(player),
                    ));
                }
            }
        }
    }

    if is_main_phase && stack_empty && is_active {
        for &obj_id in &state.battlefield {
            let Some(obj) = state.objects.get(&obj_id) else {
                continue;
            };
            if obj.controller != player || !obj.card_types.subtypes.iter().any(|s| s == "Room") {
                continue;
            }
            let unlocks = obj.room_unlocks.unwrap_or_default();
            if !unlocks.left_unlocked {
                actions.push(candidate(
                    GameAction::UnlockRoomDoor {
                        object_id: obj_id,
                        door: RoomDoor::Left,
                    },
                    TacticalClass::Ability,
                    Some(player),
                ));
            }
            if obj.back_face.is_some() && !unlocks.right_unlocked {
                actions.push(candidate(
                    GameAction::UnlockRoomDoor {
                        object_id: obj_id,
                        door: RoomDoor::Right,
                    },
                    TacticalClass::Ability,
                    Some(player),
                ));
            }
        }
    }

    // CR 602.1: Hand-activated abilities (Cycling per CR 702.29a, etc.)
    for &obj_id in &state.players[player.0 as usize].hand {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player {
                for (i, ability_def) in obj.abilities.iter().enumerate() {
                    if ability_def.kind == crate::types::ability::AbilityKind::Activated
                        && ability_def.activation_zone == Some(crate::types::zones::Zone::Hand)
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

    // CR 702.122a: Crew actions for Vehicles (keyword action, not ActivateAbility).
    // Unlike Equip/Saddle, Crew has no "Activate only as a sorcery" restriction —
    // it can be activated any time the controller has priority.
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player {
                for kw in &obj.keywords {
                    if let crate::types::keywords::Keyword::Crew(_) = kw {
                        let has_eligible = state.battlefield.iter().any(|&cid| {
                            cid != obj_id
                                && state.objects.get(&cid).is_some_and(|c| {
                                    c.controller == player
                                        && !c.tapped
                                        && c.card_types.core_types.contains(&CoreType::Creature)
                                })
                        });
                        if has_eligible {
                            actions.push(candidate(
                                GameAction::CrewVehicle {
                                    vehicle_id: obj_id,
                                    creature_ids: vec![],
                                },
                                TacticalClass::Utility,
                                Some(player),
                            ));
                        }
                        break; // One crew action per Vehicle
                    }
                }
            }
        }
    }

    // CR 702.171a: Saddle actions for Mounts (keyword action, not
    // ActivateAbility). Sorcery-speed only — the duplicate check here keeps the
    // AI search tree free of illegal candidates (mirrors the Station guard).
    if crate::game::restrictions::is_sorcery_speed_window(state, player) {
        for &obj_id in &state.battlefield {
            if let Some(obj) = state.objects.get(&obj_id) {
                if obj.controller != player {
                    continue;
                }
                if !obj
                    .keywords
                    .iter()
                    .any(|k| matches!(k, crate::types::keywords::Keyword::Saddle(_)))
                {
                    continue;
                }
                let has_eligible = state.battlefield.iter().any(|&cid| {
                    cid != obj_id
                        && state.objects.get(&cid).is_some_and(|c| {
                            c.controller == player
                                && !c.tapped
                                && c.card_types.core_types.contains(&CoreType::Creature)
                        })
                });
                if has_eligible {
                    actions.push(candidate(
                        GameAction::SaddleMount {
                            mount_id: obj_id,
                            creature_ids: vec![],
                        },
                        TacticalClass::Utility,
                        Some(player),
                    ));
                }
            }
        }
    }

    // CR 702.184a: Station actions for Spacecraft (keyword action, not
    // ActivateAbility). Sorcery-speed only — guarded by the priority arm of
    // `handle_station_activation`; duplicating the check here keeps the AI
    // search tree free of illegal candidates.
    if crate::game::restrictions::is_sorcery_speed_window(state, player) {
        for &obj_id in &state.battlefield {
            if let Some(obj) = state.objects.get(&obj_id) {
                if obj.controller != player {
                    continue;
                }
                if !obj
                    .keywords
                    .iter()
                    .any(|k| matches!(k, crate::types::keywords::Keyword::Station))
                {
                    continue;
                }
                let has_eligible = state.battlefield.iter().any(|&cid| {
                    cid != obj_id
                        && state.objects.get(&cid).is_some_and(|c| {
                            c.controller == player
                                && !c.tapped
                                && c.card_types.core_types.contains(&CoreType::Creature)
                        })
                });
                if has_eligible {
                    actions.push(candidate(
                        GameAction::ActivateStation {
                            spacecraft_id: obj_id,
                            creature_id: None,
                        },
                        TacticalClass::Utility,
                        Some(player),
                    ));
                }
            }
        }
    }

    // NOTE: TapLandForMana is intentionally excluded from priority candidates.
    // The engine auto-taps mana sources during mana payment (pay_mana_cost → auto_tap_mana_sources),
    // so the AI never needs to manually tap lands during priority. Including them
    // pollutes the search tree — shallow evaluations see "hand unchanged" for tapping
    // vs "hand shrinks" for casting, causing the AI to prefer tapping over casting.
    // Mana tap candidates are still generated for ManaPayment/UnlessPayment contexts
    // via mana_payment_actions().

    // CR 702.139a: Companion special action — pay {3} to put companion into hand.
    if crate::game::companion::can_activate_companion(state, player) {
        actions.push(candidate(
            GameAction::CompanionToHand,
            TacticalClass::Ability,
            Some(player),
        ));
    }

    // CR 702.49: Offer Ninjutsu-family activations during combat
    if state.active_player == player {
        let family_cards = keywords::ninjutsu_family_activatable_cards(state, player);
        for (card_id, variant) in &family_cards {
            let returnable = keywords::returnable_creatures_for_variant(state, player, variant);
            let timing_ok = keywords::ninjutsu_timing_ok(&state.phase, variant);
            if timing_ok {
                // CR 702.49b: Only offer ninjutsu if the player can afford it
                let can_afford = keywords::ninjutsu_family_cost_for_card(state, player, *card_id)
                    .is_some_and(|cost| {
                        let pool = &state.players[player.0 as usize].mana_pool;
                        let any_color =
                            crate::game::static_abilities::player_can_spend_as_any_color(
                                state, player,
                            );
                        // CR 107.4f + CR 118.3 + CR 119.8: honor the player's
                        // Phyrexian life budget for costs containing {C/P}.
                        let max_life =
                            crate::game::life_costs::max_phyrexian_life_payments(state, player);
                        crate::game::mana_payment::can_pay_for_spell(
                            pool, &cost, None, any_color, max_life,
                        )
                    });
                if !can_afford {
                    continue;
                }
                for &creature_id in &returnable {
                    actions.push(candidate(
                        GameAction::ActivateNinjutsu {
                            ninjutsu_card_id: *card_id,
                            creature_to_return: creature_id,
                        },
                        TacticalClass::Ability,
                        Some(player),
                    ));
                }
            }
        }
    }

    // CR 702.190a: Offer Sneak-casts from HAND during declare blockers. For
    // each hand object the player owns with an effective Sneak cost
    // (intrinsic or granted via an off-zone keyword rider), pair it with each
    // of the player's unblocked attackers as the cost-payment creature.
    // Applies to any card type — CR 702.190a does not restrict the printed
    // keyword to permanent spells; CR 702.190b's enter-attacking-alongside
    // only applies when the cast spell is a permanent (handled at
    // resolution).
    if state.active_player == player && state.phase == Phase::DeclareBlockers {
        let unblocked: Vec<ObjectId> = crate::game::combat::unblocked_attackers(state)
            .into_iter()
            .filter(|&id| {
                state
                    .objects
                    .get(&id)
                    .is_some_and(|o| o.controller == player)
            })
            .collect();
        if !unblocked.is_empty() {
            let hand_ids: Vec<ObjectId> = state
                .players
                .iter()
                .find(|p| p.id == player)
                .map(|p| p.hand.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            for hand_id in hand_ids {
                let Some(cost) = keywords::effective_sneak_cost(state, hand_id) else {
                    continue;
                };
                // CR 601.2f: Mana-cost affordability must consider mana that
                // can be produced by activating mana abilities during the cost
                // step, not just mana currently floating in the pool.
                // Delegates to the same auto-tap aware check used by the
                // normal `CastSpell` emitter (`can_cast_object_now` →
                // `can_pay_cost_after_auto_tap`) so a Sneak cast with 0
                // floating mana but enough untapped sources is surfaced.
                if !crate::game::casting::can_pay_cost_after_auto_tap(state, player, hand_id, &cost)
                {
                    continue;
                }
                let Some(card_id) = state.objects.get(&hand_id).map(|o| o.card_id) else {
                    continue;
                };
                for &creature_id in &unblocked {
                    actions.push(candidate(
                        GameAction::CastSpellAsSneak {
                            hand_object: hand_id,
                            card_id,
                            creature_to_return: creature_id,
                        },
                        TacticalClass::Ability,
                        Some(player),
                    ));
                }
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
                attacks: vec![(id, target)],
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
                    .map(|id| (id, target))
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
    let hand: Vec<_> = p.hand.iter().copied().collect();

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

/// CR 605.3a: Generate mana activation candidates for untapped permanents.
/// Used for ManaPayment/UnlessPayment contexts only — NOT for priority (the engine
/// auto-taps mana sources during spell casting via pay_mana_cost → auto_tap_mana_sources).
// Note: UntapLandForMana is intentionally omitted — it is a human-only undo action.
// AI never populates lands_tapped_for_mana, so the handler would reject it anyway.
fn mana_tap_actions(state: &GameState, player: PlayerId) -> Vec<CandidateAction> {
    let mut actions = Vec::new();
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller != player || obj.tapped {
                continue;
            }
            // Lands: single-option lands use TapLandForMana; multi-option lands
            // (duals, triomes) use ActivateAbility per mana ability so the AI
            // can choose which color to produce.
            if obj.card_types.core_types.contains(&CoreType::Land) {
                let land_options =
                    mana_sources::activatable_land_mana_options(state, obj_id, player);
                if land_options.len() == 1 {
                    actions.push(candidate(
                        GameAction::TapLandForMana { object_id: obj_id },
                        TacticalClass::Mana,
                        Some(player),
                    ));
                } else {
                    // Generate one ActivateAbility per distinct mana ability index
                    let mut seen_indices = Vec::new();
                    for opt in &land_options {
                        if let Some(idx) = opt.ability_index {
                            if !seen_indices.contains(&idx) {
                                seen_indices.push(idx);
                                actions.push(candidate(
                                    GameAction::ActivateAbility {
                                        source_id: obj_id,
                                        ability_index: idx,
                                    },
                                    TacticalClass::Mana,
                                    Some(player),
                                ));
                            }
                        }
                    }
                }
            // CR 605.1b: Non-land permanents with mana abilities use ActivateAbility
            } else if !obj.card_types.core_types.contains(&CoreType::Land)
                && !mana_sources::activatable_mana_options(state, obj_id, player).is_empty()
            {
                if let Some(idx) = obj
                    .abilities
                    .iter()
                    .position(mana_abilities::is_mana_ability)
                {
                    actions.push(candidate(
                        GameAction::ActivateAbility {
                            source_id: obj_id,
                            ability_index: idx,
                        },
                        TacticalClass::Mana,
                        Some(player),
                    ));
                }
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
    // Always include PassPriority to finalize payment
    actions.push(candidate(
        GameAction::PassPriority,
        TacticalClass::Pass,
        Some(player),
    ));
    if let Some(mode) = convoke_mode {
        // CR 702.51a + CR 302.6: Summoning sickness does not restrict tapping for convoke.
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
                        // CR 702.51a: Colorless (for generic) always available
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
/// CR 702.122a: Generate valid creature subsets whose total power >= crew_power.
///
/// Engine policy: emit only **minimal-size** subsets — the first subset size that
/// yields any valid cover. Emitting larger overcrewing options would let the AI
/// tap extra creatures unnecessarily; engine candidate generation is the right
/// place to constrain this because the rules forbid no minimum-cost crew (CR
/// 702.122a says "any number of creatures with total power >= N", not "all
/// creatures"). Within the chosen size, creatures are explored in
/// ascending-power order so the lowest-power valid cover is enumerated first;
/// downstream AI scoring breaks ties.
///
/// Capped at 20 candidates within the minimal size to keep search bounded —
/// `(subset_size, lex)` ordering is deterministic.
fn crew_vehicle_candidates(
    state: &GameState,
    player: PlayerId,
    vehicle_id: crate::types::identifiers::ObjectId,
    crew_power: u32,
    eligible_creatures: &[crate::types::identifiers::ObjectId],
) -> Vec<CandidateAction> {
    minimal_power_subset_candidates(
        state,
        player,
        eligible_creatures,
        crew_power as i32,
        |creature_ids| GameAction::CrewVehicle {
            vehicle_id,
            creature_ids,
        },
    )
}

/// CR 702.171a: Enumerate subsets of eligible creatures whose total power
/// meets the saddle threshold. Shares the minimal-cover policy with
/// `crew_vehicle_candidates`.
fn saddle_mount_candidates(
    state: &GameState,
    player: PlayerId,
    mount_id: crate::types::identifiers::ObjectId,
    saddle_power: u32,
    eligible_creatures: &[crate::types::identifiers::ObjectId],
) -> Vec<CandidateAction> {
    minimal_power_subset_candidates(
        state,
        player,
        eligible_creatures,
        saddle_power as i32,
        |creature_ids| GameAction::SaddleMount {
            mount_id,
            creature_ids,
        },
    )
}

/// Shared engine policy for power-threshold subset selection (Crew/Saddle).
/// Enumerates only the **minimal-size** valid covers, with creatures explored
/// in ascending-power order so the lowest-power valid cover is yielded first.
/// Capped at 20 candidates within the minimal size for search bounding.
fn minimal_power_subset_candidates<F>(
    state: &GameState,
    player: PlayerId,
    eligible_creatures: &[crate::types::identifiers::ObjectId],
    threshold: i32,
    wrap: F,
) -> Vec<CandidateAction>
where
    F: Fn(Vec<crate::types::identifiers::ObjectId>) -> GameAction,
{
    const MAX_CANDIDATES: usize = 20;

    let mut creatures_with_power: Vec<(crate::types::identifiers::ObjectId, i32)> =
        eligible_creatures
            .iter()
            .filter_map(|&id| {
                state
                    .objects
                    .get(&id)
                    .map(|o| (id, o.power.unwrap_or(0).max(0)))
            })
            .collect();
    // Ascending-power sort with id tie-break makes enumeration deterministic
    // and surfaces low-power covers first within each subset size.
    creatures_with_power.sort_by(|a, b| a.1.cmp(&b.1).then(a.0 .0.cmp(&b.0 .0)));

    let ids: Vec<crate::types::identifiers::ObjectId> =
        creatures_with_power.iter().map(|&(id, _)| id).collect();

    let mut actions = Vec::new();
    for size in 1..=creatures_with_power.len() {
        for combo in combinations(&ids, size) {
            let total: i32 = combo
                .iter()
                .filter_map(|id| {
                    creatures_with_power
                        .iter()
                        .find(|(cid, _)| cid == id)
                        .map(|(_, p)| *p)
                })
                .sum();
            if total >= threshold {
                actions.push(candidate(wrap(combo), TacticalClass::Utility, Some(player)));
                if actions.len() >= MAX_CANDIDATES {
                    return actions;
                }
            }
        }
        // Once any minimal-size cover is found, stop exploring larger sizes —
        // the AI must not overcrew (CR 702.122a permits any number meeting the
        // threshold; engine policy prefers minimum to preserve attackers/blockers).
        if !actions.is_empty() {
            break;
        }
    }
    actions
}

/// CR 702.184a: Offer each eligible creature as the creature tapped to station.
/// Each creature is an independent candidate — the player picks exactly one.
fn station_target_candidates(
    player: PlayerId,
    spacecraft_id: crate::types::identifiers::ObjectId,
    eligible_creatures: &[crate::types::identifiers::ObjectId],
) -> Vec<CandidateAction> {
    eligible_creatures
        .iter()
        .map(|&creature_id| {
            candidate(
                GameAction::ActivateStation {
                    spacecraft_id,
                    creature_id: Some(creature_id),
                },
                TacticalClass::Utility,
                Some(player),
            )
        })
        .collect()
}

/// CR 608.2c: Cap a SearchChoice candidate pool to at most `cap` ids before
/// the combinatorial enumerator runs. Constraint-aware: under
/// `DistinctNames` the canonical id per printed name is kept (further
/// duplicates are inert because they cannot legally appear in any chosen set
/// alongside their twin), so the cap collapses sized libraries with many
/// repeated names down to the unique-name set first. The cap exists strictly
/// to bound `PlannerServices::validate_candidates`, which clones state per
/// candidate; without it, multi-card searches against large libraries stall
/// the AI for hours. Player submissions are validated by the engine
/// submission guard, not by this enumeration, so capping here cannot make a
/// legal play unsubmittable — it only narrows the AI's *considered* set.
fn cap_search_choice_pool(
    state: &crate::types::game_state::GameState,
    cards: &[crate::types::identifiers::ObjectId],
    constraint: &crate::types::ability::SearchSelectionConstraint,
    cap: usize,
) -> Vec<crate::types::identifiers::ObjectId> {
    use crate::types::ability::SearchSelectionConstraint;
    // CR 201.2: Two cards "have the same name" iff their printed name strings
    // match. Under DistinctNames, keep the first id encountered per name —
    // later duplicates can never appear in a legal chosen set with their twin
    // and only inflate the candidate count.
    let collapsed: Vec<crate::types::identifiers::ObjectId> = match constraint {
        SearchSelectionConstraint::DistinctNames => {
            let mut seen = std::collections::HashSet::new();
            cards
                .iter()
                .copied()
                .filter(|id| match state.objects.get(id) {
                    Some(obj) => seen.insert(obj.name.clone()),
                    None => false,
                })
                .collect()
        }
        SearchSelectionConstraint::None => cards.to_vec(),
    };
    if collapsed.len() <= cap {
        collapsed
    } else {
        collapsed.into_iter().take(cap).collect()
    }
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
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, BasicLandType,
        ChoiceType, ChosenAttribute, ChosenSubtypeKind, ContinuousModification, Effect,
        ManaContribution, ManaProduction, QuantityExpr, StaticDefinition, TargetFilter, TargetRef,
    };
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaCostShard};
    use crate::types::zones::Zone;

    // CR 702.xxx: Prepare (Strixhaven) — the AI candidate enumerator must
    // surface a `CastPreparedCopy` action for every prepared creature under
    // the acting player's control while they hold priority. Without this an
    // AI opponent will never cast Prepared copies. Assign when WotC
    // publishes SOS CR update.
    /// CR 702.122a: With creatures of power 3 and 5 and a crew-3 Vehicle, the
    /// engine must offer the 3-power creature alone — never the 5-power alone
    /// (overcrew waste) and never the {3,5} pair (overcrew waste). The minimal-
    /// cover policy keeps tap pressure off the AI's best attackers/blockers.
    #[test]
    fn crew_candidates_emit_minimal_cover_only() {
        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let small = create_object(
            &mut state,
            CardId(1),
            p0,
            "Small".to_string(),
            Zone::Battlefield,
        );
        let big = create_object(
            &mut state,
            CardId(2),
            p0,
            "Big".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&small).unwrap().power = Some(3);
        state.objects.get_mut(&big).unwrap().power = Some(5);
        let vehicle = crate::types::identifiers::ObjectId(99);

        let actions = crew_vehicle_candidates(&state, p0, vehicle, 3, &[small, big]);

        // Exactly two minimal-size (size-1) covers: {small} and {big}.
        // No size-2 cover ({small, big}) — engine refuses to overcrew.
        assert_eq!(actions.len(), 2, "expected only minimal-size covers");
        for a in &actions {
            if let GameAction::CrewVehicle { creature_ids, .. } = &a.action {
                assert_eq!(creature_ids.len(), 1);
            } else {
                panic!("non-CrewVehicle candidate emitted");
            }
        }
        // Ascending-power ordering means {small} comes first.
        if let GameAction::CrewVehicle { creature_ids, .. } = &actions[0].action {
            assert_eq!(creature_ids[0], small, "smallest creature explored first");
        }
    }

    /// When no single creature meets the threshold, the engine must escalate
    /// to size 2 — but still refuse to add a third creature once a size-2
    /// cover exists.
    #[test]
    fn crew_candidates_escalate_to_size_two_when_needed() {
        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let a = create_object(&mut state, CardId(1), p0, "A".into(), Zone::Battlefield);
        let b = create_object(&mut state, CardId(2), p0, "B".into(), Zone::Battlefield);
        let c = create_object(&mut state, CardId(3), p0, "C".into(), Zone::Battlefield);
        state.objects.get_mut(&a).unwrap().power = Some(2);
        state.objects.get_mut(&b).unwrap().power = Some(2);
        state.objects.get_mut(&c).unwrap().power = Some(2);
        let vehicle = crate::types::identifiers::ObjectId(99);

        let actions = crew_vehicle_candidates(&state, p0, vehicle, 3, &[a, b, c]);

        assert!(!actions.is_empty(), "must find covers at size 2");
        for action in &actions {
            if let GameAction::CrewVehicle { creature_ids, .. } = &action.action {
                assert_eq!(
                    creature_ids.len(),
                    2,
                    "must not overcrew with three creatures"
                );
            }
        }
    }

    #[test]
    fn priority_actions_enumerate_cast_prepared_copy_for_prepared_creatures() {
        use crate::game::game_object::PreparedState;

        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        // Create a prepared creature on battlefield.
        let prepared_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Prepared One".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prepared_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&prepared_id).unwrap().prepared = Some(PreparedState);

        // Create an unprepared creature on battlefield (must NOT appear).
        let plain_id = create_object(
            &mut state,
            CardId(2),
            p0,
            "Plain One".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        state.waiting_for = WaitingFor::Priority { player: p0 };
        state.priority_player = p0;

        let actions = candidate_actions(&state);
        let has_prepared_cast = actions.iter().any(|c| {
            matches!(c.action, GameAction::CastPreparedCopy { source } if source == prepared_id)
        });
        assert!(
            has_prepared_cast,
            "expected CastPreparedCopy for the prepared creature"
        );
        // Unprepared creatures must not produce an offer.
        let has_plain_cast = actions.iter().any(
            |c| matches!(c.action, GameAction::CastPreparedCopy { source } if source == plain_id),
        );
        assert!(
            !has_plain_cast,
            "must not offer CastPreparedCopy for unprepared creatures"
        );
    }

    #[test]
    fn priority_actions_include_unlock_room_door_for_locked_room() {
        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let room = create_object(
            &mut state,
            CardId(3),
            p0,
            "Test Room".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&room).unwrap();
            obj.card_types.subtypes.push("Room".to_string());
            obj.room_unlocks = Some(Default::default());
        }
        state.phase = Phase::PreCombatMain;
        state.active_player = p0;
        state.priority_player = p0;
        state.waiting_for = WaitingFor::Priority { player: p0 };

        let actions = crate::ai_support::legal_actions(&state);

        assert!(actions.iter().any(|action| matches!(
            action,
            GameAction::UnlockRoomDoor {
                object_id,
                door: RoomDoor::Left,
            } if *object_id == room
        )));
    }

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
            source_id: None,
            description: None,
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
        ]
        .into();
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
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Black],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
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
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
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

    /// CR 608.2c + CR 701.23: SearchChoice candidate enumeration must drop
    /// combinations that violate `SearchSelectionConstraint::DistinctNames`.
    /// The engine pool cap is also constraint-aware: under DistinctNames the
    /// duplicate-named entry is collapsed to its canonical id before
    /// combinations are generated (a duplicate cannot legally appear in any
    /// chosen set with its twin), so a 5-card pool with one duplicate
    /// collapses to 4 unique-name ids → C(4,2) = 6 combinations.
    #[test]
    fn search_choice_candidates_filter_distinct_names() {
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::identifiers::ObjectId;

        let mut state = GameState::new_two_player(42);
        // Four uniquely-named cards plus one duplicate of the first name.
        let names = ["Alpha", "Beta", "Gamma", "Delta", "Alpha"];
        let mut ids: Vec<ObjectId> = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(100 + i as u64),
                PlayerId(0),
                (*name).to_string(),
                Zone::Library,
            );
            ids.push(id);
        }

        // Baseline: no constraint, pool ≤ cap → all C(5,2) = 10 combinations.
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: ids.clone(),
            count: 2,
            reveal: false,
            up_to: false,
            constraint: SearchSelectionConstraint::None,
        };
        let baseline = candidate_actions_broad(&state);
        assert_eq!(
            baseline.len(),
            10,
            "C(5,2) baseline must be 10 combinations when no constraint applies"
        );

        // With DistinctNames the engine pool cap collapses the duplicate
        // Alpha to a single canonical id (5 → 4 ids), and the post-hoc
        // selection-constraint filter then enumerates C(4,2) = 6 combos —
        // every one of which contains two distinct names.
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: ids,
            count: 2,
            reveal: false,
            up_to: false,
            constraint: SearchSelectionConstraint::DistinctNames,
        };
        let filtered = candidate_actions_broad(&state);
        assert_eq!(
            filtered.len(),
            6,
            "DistinctNames must collapse duplicate-named ids before enumeration"
        );
        for action in &filtered {
            let GameAction::SelectCards { cards } = &action.action else {
                panic!("expected SelectCards");
            };
            let names: std::collections::HashSet<_> = cards
                .iter()
                .map(|id| state.objects.get(id).unwrap().name.clone())
                .collect();
            assert_eq!(
                names.len(),
                cards.len(),
                "every emitted candidate must be name-unique"
            );
        }
    }

    /// CR 608.2c + CR 701.23: Engine pool cap must keep combinatorial
    /// enumeration tractable when the AI faces a Gifts-Ungiven-style search
    /// against a large library. With 80 ids spanning 8 distinct names and
    /// `count = 4 / up_to = true`, the constraint-aware cap collapses the
    /// pool to 8 unique-name ids before `combinations()` runs, so the
    /// candidate set fits inside a few hundred entries (Σ C(8, k) for k =
    /// 0..=4 = 163) instead of ~1.6M raw combos. This is the regression that
    /// previously stalled `validate_candidates` for hours.
    #[test]
    fn search_choice_distinct_names_caps_large_pool_to_unique_names() {
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::identifiers::ObjectId;

        let mut state = GameState::new_two_player(42);
        let mut ids: Vec<ObjectId> = Vec::with_capacity(80);
        for i in 0..80 {
            let name = format!("Card-{}", i % 8);
            let id = create_object(
                &mut state,
                CardId(1_000 + i as u64),
                PlayerId(0),
                name,
                Zone::Library,
            );
            ids.push(id);
        }
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: ids,
            count: 4,
            reveal: false,
            up_to: true,
            constraint: SearchSelectionConstraint::DistinctNames,
        };
        let actions = candidate_actions_broad(&state);
        // Σ_{k=0..=4} C(8, k) = 1 + 8 + 28 + 56 + 70 = 163.
        assert_eq!(
            actions.len(),
            163,
            "cap must collapse 80 ids → 8 unique names → 163 candidates"
        );
    }
}
