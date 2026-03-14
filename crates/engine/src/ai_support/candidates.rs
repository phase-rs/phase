use std::collections::{BTreeMap, HashSet};

use crate::game::combat::AttackTarget;
use crate::game::deck_loading::DeckEntry;
use crate::game::game_object::GameObject;
use crate::game::mana_payment;
use crate::types::ability::ChoiceType;
use crate::types::ability::TargetRef;
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{GameState, TargetSelectionSlot, WaitingFor};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaCostShard, ManaType};
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
                actor(state),
            ),
            candidate(
                GameAction::MulliganDecision { keep: false },
                TacticalClass::Selection,
                actor(state),
            ),
        ],
        WaitingFor::MulliganBottomCards { player, count } => {
            bottom_card_actions(state, *player, *count)
        }
        WaitingFor::ManaPayment { player } => mana_payment_actions(state, *player),
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
        WaitingFor::ModeChoice { player, modal, .. } => mode_actions(
            *player,
            modal.mode_count,
            modal.min_choices,
            modal.max_choices,
        ),
        WaitingFor::AbilityModeChoice { player, modal, .. } => mode_actions(
            *player,
            modal.mode_count,
            modal.min_choices,
            modal.max_choices,
        ),
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

fn actor(state: &GameState) -> Option<PlayerId> {
    match &state.waiting_for {
        WaitingFor::Priority { player }
        | WaitingFor::MulliganDecision { player, .. }
        | WaitingFor::MulliganBottomCards { player, .. }
        | WaitingFor::ManaPayment { player }
        | WaitingFor::TargetSelection { player, .. }
        | WaitingFor::DeclareAttackers { player, .. }
        | WaitingFor::DeclareBlockers { player, .. }
        | WaitingFor::ReplacementChoice { player, .. }
        | WaitingFor::EquipTarget { player, .. }
        | WaitingFor::ScryChoice { player, .. }
        | WaitingFor::DigChoice { player, .. }
        | WaitingFor::SurveilChoice { player, .. }
        | WaitingFor::RevealChoice { player, .. }
        | WaitingFor::SearchChoice { player, .. }
        | WaitingFor::TriggerTargetSelection { player, .. }
        | WaitingFor::BetweenGamesSideboard { player, .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { player, .. }
        | WaitingFor::NamedChoice { player, .. }
        | WaitingFor::ModeChoice { player, .. }
        | WaitingFor::DiscardToHandSize { player, .. }
        | WaitingFor::OptionalCostChoice { player, .. }
        | WaitingFor::AbilityModeChoice { player, .. } => Some(*player),
        WaitingFor::GameOver { .. } => None,
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

    for &obj_id in &p.hand {
        if let Some(obj) = state.objects.get(&obj_id) {
            if can_cast(state, obj, player, is_main_phase, stack_empty, is_active) {
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
    }

    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player {
                for (i, ability_def) in obj.abilities.iter().enumerate() {
                    if ability_def.kind == crate::types::ability::AbilityKind::Activated
                        && !crate::game::mana_abilities::is_mana_ability(ability_def)
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
    let mut actions = Vec::new();
    for pick_count in min..=max.min(mode_count) {
        for combo in combinations_usize(&indices, pick_count) {
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

fn mana_payment_actions(state: &GameState, player: PlayerId) -> Vec<CandidateAction> {
    let mut actions = Vec::new();
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player
                && !obj.tapped
                && obj.card_types.core_types.contains(&CoreType::Land)
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

fn can_cast(
    state: &GameState,
    obj: &GameObject,
    player: PlayerId,
    is_main_phase: bool,
    stack_empty: bool,
    is_active: bool,
) -> bool {
    if obj.card_types.core_types.contains(&CoreType::Land) {
        return false;
    }

    let is_instant =
        obj.card_types.core_types.contains(&CoreType::Instant) || obj.has_keyword(&Keyword::Flash);
    if !(is_instant || is_main_phase && stack_empty && is_active) {
        return false;
    }

    let available = compute_available_mana(state, player);
    can_afford_with(&available, &obj.mana_cost)
}

struct AvailableMana {
    white: usize,
    blue: usize,
    black: usize,
    red: usize,
    green: usize,
    colorless: usize,
}

impl AvailableMana {
    fn total(&self) -> usize {
        self.white + self.blue + self.black + self.red + self.green + self.colorless
    }

    fn count(&self, color: ManaType) -> usize {
        match color {
            ManaType::White => self.white,
            ManaType::Blue => self.blue,
            ManaType::Black => self.black,
            ManaType::Red => self.red,
            ManaType::Green => self.green,
            ManaType::Colorless => self.colorless,
        }
    }
}

fn compute_available_mana(state: &GameState, player: PlayerId) -> AvailableMana {
    let p = &state.players[player.0 as usize];
    let pool = &p.mana_pool;

    let mut available = AvailableMana {
        white: pool.count_color(ManaType::White),
        blue: pool.count_color(ManaType::Blue),
        black: pool.count_color(ManaType::Black),
        red: pool.count_color(ManaType::Red),
        green: pool.count_color(ManaType::Green),
        colorless: pool.count_color(ManaType::Colorless),
    };

    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller != player
                || obj.tapped
                || !obj.card_types.core_types.contains(&CoreType::Land)
            {
                continue;
            }
            if let Some(mana_type) = obj
                .card_types
                .subtypes
                .iter()
                .find_map(|s| mana_payment::land_subtype_to_mana_type(s))
            {
                match mana_type {
                    ManaType::White => available.white += 1,
                    ManaType::Blue => available.blue += 1,
                    ManaType::Black => available.black += 1,
                    ManaType::Red => available.red += 1,
                    ManaType::Green => available.green += 1,
                    ManaType::Colorless => available.colorless += 1,
                }
            }
        }
    }

    available
}

fn can_afford_with(available: &AvailableMana, cost: &ManaCost) -> bool {
    match cost {
        ManaCost::NoCost => false,
        ManaCost::Cost { shards, generic } => {
            let mut remaining = AvailableMana {
                white: available.white,
                blue: available.blue,
                black: available.black,
                red: available.red,
                green: available.green,
                colorless: available.colorless,
            };

            for shard in shards {
                let color = shard_to_mana_type(shard);
                if remaining.count(color) == 0 {
                    return false;
                }
                match color {
                    ManaType::White => remaining.white -= 1,
                    ManaType::Blue => remaining.blue -= 1,
                    ManaType::Black => remaining.black -= 1,
                    ManaType::Red => remaining.red -= 1,
                    ManaType::Green => remaining.green -= 1,
                    ManaType::Colorless => remaining.colorless -= 1,
                }
            }

            remaining.total() >= *generic as usize
        }
    }
}

fn shard_to_mana_type(shard: &ManaCostShard) -> ManaType {
    match shard {
        ManaCostShard::White | ManaCostShard::PhyrexianWhite | ManaCostShard::TwoWhite => {
            ManaType::White
        }
        ManaCostShard::Blue | ManaCostShard::PhyrexianBlue | ManaCostShard::TwoBlue => {
            ManaType::Blue
        }
        ManaCostShard::Black | ManaCostShard::PhyrexianBlack | ManaCostShard::TwoBlack => {
            ManaType::Black
        }
        ManaCostShard::Red | ManaCostShard::PhyrexianRed | ManaCostShard::TwoRed => ManaType::Red,
        ManaCostShard::Green | ManaCostShard::PhyrexianGreen | ManaCostShard::TwoGreen => {
            ManaType::Green
        }
        ManaCostShard::Colorless => ManaType::Colorless,
        ManaCostShard::WhiteBlue
        | ManaCostShard::PhyrexianWhiteBlue
        | ManaCostShard::ColorlessWhite => ManaType::White,
        ManaCostShard::WhiteBlack | ManaCostShard::PhyrexianWhiteBlack => ManaType::White,
        ManaCostShard::BlueBlack
        | ManaCostShard::PhyrexianBlueBlack
        | ManaCostShard::ColorlessBlue => ManaType::Blue,
        ManaCostShard::BlueRed | ManaCostShard::PhyrexianBlueRed => ManaType::Blue,
        ManaCostShard::BlackRed
        | ManaCostShard::PhyrexianBlackRed
        | ManaCostShard::ColorlessBlack => ManaType::Black,
        ManaCostShard::BlackGreen | ManaCostShard::PhyrexianBlackGreen => ManaType::Black,
        ManaCostShard::RedWhite
        | ManaCostShard::PhyrexianRedWhite
        | ManaCostShard::ColorlessRed => ManaType::Red,
        ManaCostShard::RedGreen | ManaCostShard::PhyrexianRedGreen => ManaType::Red,
        ManaCostShard::GreenWhite
        | ManaCostShard::PhyrexianGreenWhite
        | ManaCostShard::ColorlessGreen => ManaType::Green,
        ManaCostShard::GreenBlue | ManaCostShard::PhyrexianGreenBlue => ManaType::Green,
        ManaCostShard::X | ManaCostShard::Snow => ManaType::Colorless,
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
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::ChoiceType;
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::CardId;
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
}
