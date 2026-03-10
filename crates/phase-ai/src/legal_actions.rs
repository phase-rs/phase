use engine::game::game_object::GameObject;
use engine::game::mana_payment;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Returns all legal actions for the current game state based on `waiting_for`.
pub fn get_legal_actions(state: &GameState) -> Vec<GameAction> {
    match &state.waiting_for {
        WaitingFor::Priority { player } => priority_actions(state, *player),
        WaitingFor::MulliganDecision { .. } => {
            vec![
                GameAction::MulliganDecision { keep: true },
                GameAction::MulliganDecision { keep: false },
            ]
        }
        WaitingFor::MulliganBottomCards { player, count } => {
            bottom_card_actions(state, *player, *count)
        }
        WaitingFor::ManaPayment { player } => mana_payment_actions(state, *player),
        WaitingFor::TargetSelection { player, .. } => target_selection_actions(state, *player),
        WaitingFor::DeclareAttackers { player, .. } => attacker_actions(state, *player),
        WaitingFor::DeclareBlockers { player, .. } => blocker_actions(state, *player),
        WaitingFor::ReplacementChoice {
            candidate_count, ..
        } => (0..*candidate_count)
            .map(|i| GameAction::ChooseReplacement { index: i })
            .collect(),
        WaitingFor::GameOver { .. } => Vec::new(),
        WaitingFor::EquipTarget { .. } => Vec::new(),
        WaitingFor::ScryChoice { cards, .. } => {
            // For scry, generate one action per possible subset of cards to put on top.
            // Simplification: return two options -- all on top, or all on bottom.
            // The AI search handles the choice via evaluation.
            let mut actions = Vec::new();
            // All on top
            actions.push(GameAction::SelectCards {
                cards: cards.clone(),
            });
            // All on bottom
            actions.push(GameAction::SelectCards { cards: Vec::new() });
            // Individual cards on top (for scry 2+)
            if cards.len() > 1 {
                for card in cards {
                    actions.push(GameAction::SelectCards { cards: vec![*card] });
                }
            }
            actions
        }
        WaitingFor::DigChoice {
            cards, keep_count, ..
        } => {
            // Generate combinations of keep_count cards from the revealed set
            let combos = combinations(cards, *keep_count);
            combos
                .into_iter()
                .map(|combo| GameAction::SelectCards { cards: combo })
                .collect()
        }
        WaitingFor::SurveilChoice { cards, .. } => {
            // Generate options: all to graveyard, none to graveyard, individual cards
            let mut actions = Vec::new();
            // All to graveyard
            actions.push(GameAction::SelectCards {
                cards: cards.clone(),
            });
            // None to graveyard (all stay on top)
            actions.push(GameAction::SelectCards { cards: Vec::new() });
            // Individual cards to graveyard
            if cards.len() > 1 {
                for card in cards {
                    actions.push(GameAction::SelectCards { cards: vec![*card] });
                }
            }
            actions
        }
    }
}

fn priority_actions(state: &GameState, player: PlayerId) -> Vec<GameAction> {
    let mut actions = vec![GameAction::PassPriority];

    let p = &state.players[player.0 as usize];
    let is_main_phase = matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain);
    let stack_empty = state.stack.is_empty();
    let is_active = state.active_player == player;

    // Playable lands: main phase, stack empty, active player, land drop available
    if is_main_phase && stack_empty && is_active {
        let lands_available = state.lands_played_this_turn < state.max_lands_per_turn;
        if lands_available {
            for &obj_id in &p.hand {
                if let Some(obj) = state.objects.get(&obj_id) {
                    if obj.card_types.core_types.contains(&CoreType::Land) {
                        actions.push(GameAction::PlayLand {
                            card_id: obj.card_id,
                        });
                    }
                }
            }
        }
    }

    // Castable spells from hand
    for &obj_id in &p.hand {
        if let Some(obj) = state.objects.get(&obj_id) {
            if can_cast(state, obj, player, is_main_phase, stack_empty, is_active) {
                actions.push(GameAction::CastSpell {
                    card_id: obj.card_id,
                    targets: Vec::new(),
                });
            }
        }
    }

    // Activatable abilities from battlefield permanents
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player {
                for (i, ability_def) in obj.abilities.iter().enumerate() {
                    if ability_def.kind == engine::types::ability::AbilityKind::Activated {
                        actions.push(GameAction::ActivateAbility {
                            source_id: obj_id,
                            ability_index: i,
                        });
                    }
                }
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
    // Lands are played, not cast
    if obj.card_types.core_types.contains(&CoreType::Land) {
        return false;
    }

    // Sorcery-speed: must be main phase, stack empty, active player
    let is_instant =
        obj.card_types.core_types.contains(&CoreType::Instant) || obj.has_keyword(&Keyword::Flash);

    if !(is_instant || is_main_phase && stack_empty && is_active) {
        return false;
    }

    // Check if player could afford the cost using pool + untapped lands
    let available = compute_available_mana(state, player);
    can_afford_with(&available, &obj.mana_cost)
}

/// Mana available from the current pool plus untapped lands on the battlefield.
/// Used for UI highlighting — shows what the player *could* cast, not just
/// what they can cast with currently floating mana.
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

    // Add potential mana from untapped lands on the battlefield
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller != player || obj.tapped {
                continue;
            }
            if !obj.card_types.core_types.contains(&CoreType::Land) {
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
            // Track how much of each color we consume for colored shards
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
                if remaining.count(color) > 0 {
                    match color {
                        ManaType::White => remaining.white -= 1,
                        ManaType::Blue => remaining.blue -= 1,
                        ManaType::Black => remaining.black -= 1,
                        ManaType::Red => remaining.red -= 1,
                        ManaType::Green => remaining.green -= 1,
                        ManaType::Colorless => remaining.colorless -= 1,
                    }
                } else {
                    return false;
                }
            }

            // Generic cost can be paid with any remaining mana
            remaining.total() >= *generic as usize
        }
    }
}

/// Map a mana cost shard to the ManaType it requires.
/// Hybrid/phyrexian shards return the first option for simplicity —
/// this is a conservative approximation for UI highlighting.
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
        // Hybrid: use first color (conservative — may miss some castable spells)
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
        // X costs and Snow cost nothing for affordability check
        ManaCostShard::X | ManaCostShard::Snow => ManaType::Colorless,
    }
}

fn bottom_card_actions(state: &GameState, player: PlayerId, count: u8) -> Vec<GameAction> {
    let p = &state.players[player.0 as usize];
    let hand: Vec<_> = p.hand.clone();

    if count == 0 || hand.is_empty() {
        return vec![GameAction::SelectCards { cards: Vec::new() }];
    }

    // For simplicity, return individual card selections rather than all combinations.
    // The AI search layer will evaluate which cards to bottom.
    // Return one action per possible card to bottom (the engine handles the full selection).
    let mut actions = Vec::new();
    let count = count as usize;

    if count >= hand.len() {
        // Must bottom all cards
        actions.push(GameAction::SelectCards { cards: hand });
    } else {
        // Generate combinations of size `count` from hand
        let combos = combinations(&hand, count);
        for combo in combos {
            actions.push(GameAction::SelectCards { cards: combo });
        }
    }

    actions
}

fn combinations(
    items: &[engine::types::identifiers::ObjectId],
    k: usize,
) -> Vec<Vec<engine::types::identifiers::ObjectId>> {
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
    // Include first element
    for mut combo in combinations(&items[1..], k - 1) {
        combo.insert(0, items[0]);
        result.push(combo);
    }
    // Exclude first element
    result.extend(combinations(&items[1..], k));
    result
}

fn mana_payment_actions(state: &GameState, player: PlayerId) -> Vec<GameAction> {
    let mut actions = Vec::new();

    // Find untapped lands controlled by this player
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player
                && !obj.tapped
                && obj.card_types.core_types.contains(&CoreType::Land)
            {
                actions.push(GameAction::TapLandForMana { object_id: obj_id });
            }
        }
    }

    actions
}

fn target_selection_actions(state: &GameState, player: PlayerId) -> Vec<GameAction> {
    use engine::types::ability::TargetRef;

    let mut actions = Vec::new();

    // Enumerate legal targets: creatures and players
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.card_types.core_types.contains(&CoreType::Creature) {
                // Don't target hexproof/shroud creatures controlled by opponents
                let is_opponent = obj.controller != player;
                if is_opponent
                    && (obj.has_keyword(&Keyword::Hexproof) || obj.has_keyword(&Keyword::Shroud))
                {
                    continue;
                }
                if !is_opponent && obj.has_keyword(&Keyword::Shroud) {
                    continue;
                }
                actions.push(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(obj_id)],
                });
            }
        }
    }

    // Players as targets
    for p in &state.players {
        actions.push(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(p.id)],
        });
    }

    // Stack spells as targets (for counterspells)
    for entry in &state.stack {
        actions.push(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(entry.id)],
        });
    }

    actions
}

fn attacker_actions(state: &GameState, player: PlayerId) -> Vec<GameAction> {
    // Always allow not attacking
    let mut actions = vec![GameAction::DeclareAttackers {
        attacker_ids: Vec::new(),
    }];

    // Find all creatures that can attack
    let mut candidates = Vec::new();
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !obj.tapped
                && !obj.has_keyword(&Keyword::Defender)
                && (obj.has_keyword(&Keyword::Haste)
                    || obj
                        .entered_battlefield_turn
                        .is_some_and(|etb| etb < state.turn_number))
            {
                candidates.push(obj_id);
            }
        }
    }

    // Return individual creature candidates -- the combat AI will decide the subset
    for &id in &candidates {
        actions.push(GameAction::DeclareAttackers {
            attacker_ids: vec![id],
        });
    }

    // Also return all-attack option
    if candidates.len() > 1 {
        actions.push(GameAction::DeclareAttackers {
            attacker_ids: candidates,
        });
    }

    actions
}

fn blocker_actions(state: &GameState, player: PlayerId) -> Vec<GameAction> {
    // Always allow not blocking
    let mut actions = vec![GameAction::DeclareBlockers {
        assignments: Vec::new(),
    }];

    let combat = match &state.combat {
        Some(c) => c,
        None => return actions,
    };

    // For each attacker, find legal blockers
    for attacker_info in &combat.attackers {
        let attacker_id = attacker_info.object_id;
        let attacker = match state.objects.get(&attacker_id) {
            Some(a) => a,
            None => continue,
        };

        for &obj_id in &state.battlefield {
            if let Some(obj) = state.objects.get(&obj_id) {
                if obj.controller == player
                    && obj.card_types.core_types.contains(&CoreType::Creature)
                    && !obj.tapped
                    && can_block_attacker(obj, attacker)
                {
                    actions.push(GameAction::DeclareBlockers {
                        assignments: vec![(obj_id, attacker_id)],
                    });
                }
            }
        }
    }

    actions
}

fn can_block_attacker(blocker: &GameObject, attacker: &GameObject) -> bool {
    // Flying check: flying attacker can only be blocked by flying or reach
    if attacker.has_keyword(&Keyword::Flying)
        && !blocker.has_keyword(&Keyword::Flying)
        && !blocker.has_keyword(&Keyword::Reach)
    {
        return false;
    }

    // Shadow check
    let attacker_shadow = attacker.has_keyword(&Keyword::Shadow);
    let blocker_shadow = blocker.has_keyword(&Keyword::Shadow);
    if attacker_shadow != blocker_shadow {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn add_creature_to_battlefield(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1);
        id
    }

    fn add_card_to_hand(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        core_type: CoreType,
        cost: ManaCost,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core_type);
        obj.mana_cost = cost;
        id
    }

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let p = &mut state.players[player.0 as usize];
        for _ in 0..count {
            p.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }
    }

    #[test]
    fn priority_always_includes_pass() {
        let state = make_state();
        let actions = get_legal_actions(&state);
        assert!(actions.contains(&GameAction::PassPriority));
    }

    #[test]
    fn priority_includes_playable_land() {
        let mut state = make_state();
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Forest",
            CoreType::Land,
            ManaCost::NoCost,
        );
        let actions = get_legal_actions(&state);
        assert!(actions
            .iter()
            .any(|a| matches!(a, GameAction::PlayLand { .. })));
    }

    #[test]
    fn priority_no_land_when_already_played() {
        let mut state = make_state();
        state.lands_played_this_turn = 1;
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Forest",
            CoreType::Land,
            ManaCost::NoCost,
        );
        let actions = get_legal_actions(&state);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, GameAction::PlayLand { .. })));
    }

    #[test]
    fn priority_includes_castable_spell_with_mana() {
        let mut state = make_state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Lightning Bolt",
            CoreType::Instant,
            cost,
        );
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);
        let actions = get_legal_actions(&state);
        assert!(actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn priority_no_spell_without_mana() {
        let mut state = make_state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Lightning Bolt",
            CoreType::Instant,
            cost,
        );
        let actions = get_legal_actions(&state);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn mulligan_decision_returns_keep_and_mull() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::MulliganDecision {
            player: PlayerId(0),
            mulligan_count: 0,
        };
        let actions = get_legal_actions(&state);
        assert_eq!(actions.len(), 2);
        assert!(actions.contains(&GameAction::MulliganDecision { keep: true }));
        assert!(actions.contains(&GameAction::MulliganDecision { keep: false }));
    }

    #[test]
    fn game_over_returns_empty() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        let actions = get_legal_actions(&state);
        assert!(actions.is_empty());
    }

    #[test]
    fn replacement_choice_returns_all_indices() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 3,
        };
        let actions = get_legal_actions(&state);
        assert_eq!(actions.len(), 3);
    }

    #[test]
    fn declare_attackers_includes_no_attack_option() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
        };
        add_creature_to_battlefield(&mut state, PlayerId(0), "Bear", 2, 2);
        let actions = get_legal_actions(&state);
        assert!(actions.contains(&GameAction::DeclareAttackers {
            attacker_ids: Vec::new(),
        }));
    }

    #[test]
    fn declare_attackers_includes_eligible_creatures() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
        };
        let id = add_creature_to_battlefield(&mut state, PlayerId(0), "Bear", 2, 2);
        let actions = get_legal_actions(&state);
        assert!(actions.iter().any(|a| matches!(
            a,
            GameAction::DeclareAttackers { attacker_ids } if attacker_ids == &[id]
        )));
    }

    #[test]
    fn declare_blockers_includes_no_block_option() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::DeclareBlockers {
            player: PlayerId(1),
            valid_blocker_ids: vec![],
            valid_block_targets: std::collections::HashMap::new(),
        };
        let actions = get_legal_actions(&state);
        assert!(actions.contains(&GameAction::DeclareBlockers {
            assignments: Vec::new(),
        }));
    }

    #[test]
    fn mana_payment_lists_untapped_lands() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
        };
        let id = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let actions = get_legal_actions(&state);
        assert!(actions.iter().any(|a| matches!(
            a,
            GameAction::TapLandForMana { object_id } if *object_id == id
        )));
    }

    fn add_land_to_battlefield(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        subtype: &str,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push(subtype.to_string());
        id
    }

    #[test]
    fn spell_castable_with_untapped_land_on_battlefield() {
        let mut state = make_state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Lightning Bolt",
            CoreType::Instant,
            cost,
        );
        add_land_to_battlefield(&mut state, PlayerId(0), "Mountain", "Mountain");
        let actions = get_legal_actions(&state);
        assert!(actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn spell_not_castable_with_wrong_color_land() {
        let mut state = make_state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };
        add_card_to_hand(&mut state, PlayerId(0), "Opt", CoreType::Instant, cost);
        // Only have a Forest, need Blue
        add_land_to_battlefield(&mut state, PlayerId(0), "Forest", "Forest");
        let actions = get_legal_actions(&state);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn spell_castable_with_generic_plus_colored_from_lands() {
        let mut state = make_state();
        // Cost: 1U (one blue + one generic)
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            cost,
        );
        add_land_to_battlefield(&mut state, PlayerId(0), "Island", "Island");
        add_land_to_battlefield(&mut state, PlayerId(0), "Plains", "Plains");
        let actions = get_legal_actions(&state);
        assert!(actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn spell_not_castable_with_insufficient_lands() {
        let mut state = make_state();
        // Cost: 1U (need 2 total, 1 must be blue)
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            cost,
        );
        // Only one Island — need 2 total mana
        add_land_to_battlefield(&mut state, PlayerId(0), "Island", "Island");
        let actions = get_legal_actions(&state);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn tapped_lands_not_counted_as_available() {
        let mut state = make_state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Lightning Bolt",
            CoreType::Instant,
            cost,
        );
        let land_id = add_land_to_battlefield(&mut state, PlayerId(0), "Mountain", "Mountain");
        state.objects.get_mut(&land_id).unwrap().tapped = true;
        let actions = get_legal_actions(&state);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn pool_mana_plus_lands_combine_for_affordability() {
        let mut state = make_state();
        // Cost: UU (two blue)
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        };
        add_card_to_hand(
            &mut state,
            PlayerId(0),
            "Counterspell",
            CoreType::Instant,
            cost,
        );
        // One blue floating + one untapped Island = 2 blue
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_land_to_battlefield(&mut state, PlayerId(0), "Island", "Island");
        let actions = get_legal_actions(&state);
        assert!(actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })));
    }

    #[test]
    fn target_selection_includes_creatures_and_players() {
        let mut state = make_state();
        let pending = engine::types::game_state::PendingCast {
            object_id: ObjectId(99),
            card_id: CardId(99),
            ability: engine::types::ability::ResolvedAbility::from_raw(
                "",
                std::collections::HashMap::new(),
                Vec::new(),
                ObjectId(99),
                PlayerId(0),
            ),
            cost: ManaCost::zero(),
        };
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast: Box::new(pending),
            legal_targets: vec![],
        };
        add_creature_to_battlefield(&mut state, PlayerId(1), "Bear", 2, 2);
        let actions = get_legal_actions(&state);
        // Should have creature targets + 2 player targets
        assert!(actions.len() >= 3);
    }
}
