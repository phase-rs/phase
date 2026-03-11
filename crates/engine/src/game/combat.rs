use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::game_object::GameObject;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::{Keyword, ProtectionTarget};
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use super::players;
use crate::types::statics::StaticMode;

/// Represents who a creature is attacking: a player or a planeswalker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AttackTarget {
    Player(PlayerId),
    Planeswalker(ObjectId),
}

/// Tracks the state of the current combat phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CombatState {
    pub attackers: Vec<AttackerInfo>,
    /// attacker_id -> ordered list of blocker ids
    pub blocker_assignments: HashMap<ObjectId, Vec<ObjectId>>,
    /// blocker_id -> attacker_id (reverse lookup)
    pub blocker_to_attacker: HashMap<ObjectId, ObjectId>,
    pub damage_assignments: HashMap<ObjectId, Vec<DamageAssignment>>,
    pub first_strike_done: bool,
}

impl PartialEq for CombatState {
    fn eq(&self, other: &Self) -> bool {
        self.attackers == other.attackers
            && self.blocker_assignments == other.blocker_assignments
            && self.blocker_to_attacker == other.blocker_to_attacker
            && self.first_strike_done == other.first_strike_done
    }
}

impl Eq for CombatState {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttackerInfo {
    pub object_id: ObjectId,
    pub defending_player: PlayerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageAssignment {
    pub target: DamageTarget,
    pub amount: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageTarget {
    Object(ObjectId),
    Player(PlayerId),
}

/// Validate that the given attacker ids are legal attackers for the active player.
pub fn validate_attackers(state: &GameState, attacker_ids: &[ObjectId]) -> Result<(), String> {
    let active = state.active_player;

    for &id in attacker_ids {
        let obj = state
            .objects
            .get(&id)
            .ok_or_else(|| format!("Attacker {:?} not found", id))?;

        // Must be a creature on the battlefield
        if obj.zone != crate::types::zones::Zone::Battlefield {
            return Err(format!("{:?} is not on the battlefield", id));
        }
        if !obj.card_types.core_types.contains(&CoreType::Creature) {
            return Err(format!("{:?} is not a creature", id));
        }

        // Must be controlled by active player
        if obj.controller != active {
            return Err(format!("{:?} is not controlled by active player", id));
        }

        // Must not be tapped
        if obj.tapped {
            return Err(format!("{:?} is tapped", id));
        }

        // Must not have Defender
        if obj.has_keyword(&Keyword::Defender) {
            return Err(format!("{:?} has Defender", id));
        }

        // Summoning sickness: must have Haste or have been on battlefield since before this turn
        if !obj.has_keyword(&Keyword::Haste) {
            if let Some(etb_turn) = obj.entered_battlefield_turn {
                if etb_turn >= state.turn_number {
                    return Err(format!("{:?} has summoning sickness", id));
                }
            } else {
                // No ETB turn recorded -- treat as summoning sick
                return Err(format!("{:?} has summoning sickness (no ETB turn)", id));
            }
        }
    }

    Ok(())
}

/// Validate that the given blocker assignments are legal.
/// Each assignment is (blocker_id, attacker_id).
pub fn validate_blockers(
    state: &GameState,
    assignments: &[(ObjectId, ObjectId)],
) -> Result<(), String> {
    // Group assignments by attacker for menace validation
    let mut blockers_per_attacker: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();

    for &(blocker_id, attacker_id) in assignments {
        let blocker = state
            .objects
            .get(&blocker_id)
            .ok_or_else(|| format!("Blocker {:?} not found", blocker_id))?;

        // Must be a creature on the battlefield
        if blocker.zone != crate::types::zones::Zone::Battlefield {
            return Err(format!("{:?} is not on the battlefield", blocker_id));
        }
        if !blocker.card_types.core_types.contains(&CoreType::Creature) {
            return Err(format!("{:?} is not a creature", blocker_id));
        }

        // Must not be controlled by the active (attacking) player
        if blocker.controller == state.active_player {
            return Err(format!(
                "{:?} is not controlled by defending player",
                blocker_id
            ));
        }

        // In multiplayer, blocker must be blocking an attacker that is attacking
        // the blocker's controller
        if let Some(combat) = &state.combat {
            if let Some(attacker_info) =
                combat.attackers.iter().find(|a| a.object_id == attacker_id)
            {
                if attacker_info.defending_player != blocker.controller {
                    return Err(format!(
                        "{:?} cannot block {:?} (not attacking this player)",
                        blocker_id, attacker_id
                    ));
                }
            }
        }

        // Must not be tapped
        if blocker.tapped {
            return Err(format!("{:?} is tapped", blocker_id));
        }

        // Check attacker exists and is actually attacking
        let attacker = state
            .objects
            .get(&attacker_id)
            .ok_or_else(|| format!("Attacker {:?} not found", attacker_id))?;

        // CantBeBlocked static ability: creature is completely unblockable
        if attacker
            .static_definitions
            .iter()
            .any(|sd| sd.mode == StaticMode::Other("CantBeBlocked".into()))
        {
            return Err(format!(
                "{:?} cannot block {:?} (can't be blocked)",
                blocker_id, attacker_id
            ));
        }

        // Protection: creature with protection from a color can't be blocked by
        // creatures of that color
        for kw in &attacker.keywords {
            if let Keyword::Protection(ProtectionTarget::Color(color)) = kw {
                if blocker.color.contains(color) {
                    return Err(format!(
                        "{:?} cannot block {:?} (protection from {:?})",
                        blocker_id, attacker_id, color
                    ));
                }
            }
        }

        // Flying: can only be blocked by creatures with Flying or Reach
        if attacker.has_keyword(&Keyword::Flying)
            && !blocker.has_keyword(&Keyword::Flying)
            && !blocker.has_keyword(&Keyword::Reach)
        {
            return Err(format!(
                "{:?} cannot block {:?} (flying, no flying/reach)",
                blocker_id, attacker_id
            ));
        }

        // Shadow: shadow creatures can only be blocked by shadow creatures,
        // and non-shadow creatures can't be blocked by shadow creatures
        let attacker_has_shadow = attacker.has_keyword(&Keyword::Shadow);
        let blocker_has_shadow = blocker.has_keyword(&Keyword::Shadow);
        if attacker_has_shadow && !blocker_has_shadow {
            return Err(format!(
                "{:?} cannot block {:?} (shadow can only be blocked by shadow)",
                blocker_id, attacker_id
            ));
        }
        if !attacker_has_shadow && blocker_has_shadow {
            return Err(format!(
                "{:?} cannot block {:?} (shadow cannot block non-shadow)",
                blocker_id, attacker_id
            ));
        }

        // Fear (MTG 702.36): can only be blocked by artifact creatures or black creatures
        if attacker.has_keyword(&Keyword::Fear)
            && !blocker.card_types.core_types.contains(&CoreType::Artifact)
            && !blocker.color.contains(&ManaColor::Black)
        {
            return Err(format!(
                "{:?} cannot block {:?} (fear: must be artifact or black)",
                blocker_id, attacker_id
            ));
        }

        // Intimidate (MTG 702.13): can only be blocked by artifact creatures or
        // creatures sharing a color with the attacker
        if attacker.has_keyword(&Keyword::Intimidate)
            && !blocker.card_types.core_types.contains(&CoreType::Artifact)
            && !attacker.color.iter().any(|c| blocker.color.contains(c))
        {
            return Err(format!(
                "{:?} cannot block {:?} (intimidate: must be artifact or share a color)",
                blocker_id, attacker_id
            ));
        }

        // Skulk (MTG 702.120): cannot be blocked by creatures with strictly greater power
        if attacker.has_keyword(&Keyword::Skulk)
            && blocker.power.unwrap_or(0) > attacker.power.unwrap_or(0)
        {
            return Err(format!(
                "{:?} cannot block {:?} (skulk: blocker power {} > attacker power {})",
                blocker_id,
                attacker_id,
                blocker.power.unwrap_or(0),
                attacker.power.unwrap_or(0)
            ));
        }

        // Horsemanship (MTG 702.30): can only be blocked by creatures with horsemanship
        if attacker.has_keyword(&Keyword::Horsemanship)
            && !blocker.has_keyword(&Keyword::Horsemanship)
        {
            return Err(format!(
                "{:?} cannot block {:?} (horsemanship: blocker lacks horsemanship)",
                blocker_id, attacker_id
            ));
        }

        blockers_per_attacker
            .entry(attacker_id)
            .or_default()
            .push(blocker_id);
    }

    // Menace: if an attacker has Menace, it must be blocked by 2+ creatures or not at all
    for (attacker_id, blockers) in &blockers_per_attacker {
        if let Some(attacker) = state.objects.get(attacker_id) {
            if attacker.has_keyword(&Keyword::Menace) && blockers.len() < 2 {
                return Err(format!(
                    "{:?} has menace and must be blocked by 2+ creatures",
                    attacker_id
                ));
            }
        }
    }

    Ok(())
}

/// Declare attackers: validate, tap (unless vigilance), populate CombatState, emit event.
/// Accepts per-creature attack targets as (attacker_id, target) pairs.
pub fn declare_attackers(
    state: &mut GameState,
    attacks: &[(ObjectId, AttackTarget)],
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    let attacker_ids: Vec<ObjectId> = attacks.iter().map(|(id, _)| *id).collect();
    validate_attackers(state, &attacker_ids)?;

    // Validate attack targets
    for (attacker_id, target) in attacks {
        match target {
            AttackTarget::Player(pid) => {
                if !state.players.iter().any(|p| p.id == *pid)
                    || state.eliminated_players.contains(pid)
                    || *pid == state.active_player
                {
                    return Err(format!("{:?} cannot attack player {:?}", attacker_id, pid));
                }
            }
            AttackTarget::Planeswalker(pw_id) => {
                let pw = state
                    .objects
                    .get(pw_id)
                    .ok_or_else(|| format!("Planeswalker {:?} not found", pw_id))?;
                if pw.zone != crate::types::zones::Zone::Battlefield
                    || !pw
                        .card_types
                        .core_types
                        .contains(&crate::types::card_type::CoreType::Planeswalker)
                {
                    return Err(format!(
                        "{:?} is not a planeswalker on the battlefield",
                        pw_id
                    ));
                }
                // Can't attack your own planeswalker
                if pw.controller == state.active_player {
                    return Err(format!("Cannot attack your own planeswalker {:?}", pw_id));
                }
            }
        }
    }

    // Tap attackers (unless they have Vigilance)
    for &id in &attacker_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            if !obj.has_keyword(&Keyword::Vigilance) {
                obj.tapped = true;
                events.push(GameEvent::PermanentTapped { object_id: id });
            }
        }
    }

    // Populate CombatState with per-creature defending players
    let combat = state.combat.get_or_insert_with(CombatState::default);
    combat.attackers = attacks
        .iter()
        .map(|(object_id, target)| {
            let defending_player = match target {
                AttackTarget::Player(pid) => *pid,
                AttackTarget::Planeswalker(pw_id) => state
                    .objects
                    .get(pw_id)
                    .map(|pw| pw.controller)
                    .unwrap_or(PlayerId(0)),
            };
            AttackerInfo {
                object_id: *object_id,
                defending_player,
            }
        })
        .collect();

    // Use the first attacker's defending player for the event
    let defending_player = combat
        .attackers
        .first()
        .map(|a| a.defending_player)
        .unwrap_or_else(|| players::next_player(state, state.active_player));

    events.push(GameEvent::AttackersDeclared {
        attacker_ids,
        defending_player,
    });

    Ok(())
}

/// Declare blockers: validate, populate CombatState, emit event, auto-order by ObjectId.
pub fn declare_blockers(
    state: &mut GameState,
    assignments: &[(ObjectId, ObjectId)],
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    validate_blockers(state, assignments)?;

    let combat = state
        .combat
        .as_mut()
        .ok_or("No combat state (attackers not declared)")?;

    // Populate blocker assignments grouped by attacker
    let mut grouped: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for &(blocker_id, attacker_id) in assignments {
        grouped.entry(attacker_id).or_default().push(blocker_id);
        combat.blocker_to_attacker.insert(blocker_id, attacker_id);
    }

    // Auto-order blockers by ObjectId ascending (deterministic default)
    for (attacker_id, mut blockers) in grouped {
        blockers.sort_by_key(|id| id.0);
        combat.blocker_assignments.insert(attacker_id, blockers);
    }

    events.push(GameEvent::BlockersDeclared {
        assignments: assignments.to_vec(),
    });

    Ok(())
}

/// Check if a creature has summoning sickness (entered this turn without Haste).
pub fn has_summoning_sickness(obj: &GameObject, turn_number: u32) -> bool {
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        return false;
    }
    if obj.has_keyword(&Keyword::Haste) {
        return false;
    }
    obj.entered_battlefield_turn
        .is_some_and(|etb| etb >= turn_number)
}

/// Return the IDs of all creatures the active player could legally declare as attackers.
pub fn get_valid_attacker_ids(state: &GameState) -> Vec<ObjectId> {
    let active = state.active_player;
    let turn = state.turn_number;

    state
        .battlefield
        .iter()
        .filter_map(|id| {
            let obj = state.objects.get(id)?;
            if obj.controller == active
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !obj.tapped
                && !obj.has_keyword(&Keyword::Defender)
                && (obj.has_keyword(&Keyword::Haste)
                    || obj.entered_battlefield_turn.is_some_and(|etb| etb < turn))
            {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

/// Check if a specific blocker can legally block a specific attacker (per-pair restrictions only).
/// Does NOT check menace (which is a multi-blocker constraint).
fn can_block_pair(blocker: &GameObject, attacker: &GameObject) -> bool {
    if attacker
        .static_definitions
        .iter()
        .any(|sd| sd.mode == StaticMode::Other("CantBeBlocked".into()))
    {
        return false;
    }
    for kw in &attacker.keywords {
        if let Keyword::Protection(ProtectionTarget::Color(color)) = kw {
            if blocker.color.contains(color) {
                return false;
            }
        }
    }
    if attacker.has_keyword(&Keyword::Flying)
        && !blocker.has_keyword(&Keyword::Flying)
        && !blocker.has_keyword(&Keyword::Reach)
    {
        return false;
    }
    let attacker_has_shadow = attacker.has_keyword(&Keyword::Shadow);
    let blocker_has_shadow = blocker.has_keyword(&Keyword::Shadow);
    if attacker_has_shadow && !blocker_has_shadow {
        return false;
    }
    if !attacker_has_shadow && blocker_has_shadow {
        return false;
    }
    if attacker.has_keyword(&Keyword::Fear)
        && !blocker.card_types.core_types.contains(&CoreType::Artifact)
        && !blocker.color.contains(&ManaColor::Black)
    {
        return false;
    }
    if attacker.has_keyword(&Keyword::Intimidate)
        && !blocker.card_types.core_types.contains(&CoreType::Artifact)
        && !attacker.color.iter().any(|c| blocker.color.contains(c))
    {
        return false;
    }
    if attacker.has_keyword(&Keyword::Skulk)
        && blocker.power.unwrap_or(0) > attacker.power.unwrap_or(0)
    {
        return false;
    }
    if attacker.has_keyword(&Keyword::Horsemanship) && !blocker.has_keyword(&Keyword::Horsemanship)
    {
        return false;
    }
    true
}

/// For each valid blocker, compute which attackers it can legally block.
/// In multiplayer, blockers can only block creatures attacking them (their controller).
pub fn get_valid_block_targets(state: &GameState) -> HashMap<ObjectId, Vec<ObjectId>> {
    let valid_blockers = get_valid_blocker_ids(state);
    let combat = match state.combat.as_ref() {
        Some(c) => c,
        None => return HashMap::new(),
    };

    let mut result = HashMap::new();
    for &blocker_id in &valid_blockers {
        let blocker = match state.objects.get(&blocker_id) {
            Some(obj) => obj,
            None => continue,
        };
        let blocker_controller = blocker.controller;
        // Filter attackers to only those attacking this blocker's controller
        let valid_targets: Vec<ObjectId> = combat
            .attackers
            .iter()
            .filter(|a| a.defending_player == blocker_controller)
            .filter(|a| {
                state
                    .objects
                    .get(&a.object_id)
                    .map(|attacker| can_block_pair(blocker, attacker))
                    .unwrap_or(false)
            })
            .map(|a| a.object_id)
            .collect();
        if !valid_targets.is_empty() {
            result.insert(blocker_id, valid_targets);
        }
    }
    result
}

/// Return the IDs of all creatures that could legally be assigned as blockers.
/// A creature is a valid blocker if it's an untapped creature controlled by a defending player
/// (any player being attacked in the current combat).
pub fn get_valid_blocker_ids(state: &GameState) -> Vec<ObjectId> {
    // Collect all defending players from combat state
    let defending_players: Vec<PlayerId> = state
        .combat
        .as_ref()
        .map(|c| {
            let mut players: Vec<PlayerId> =
                c.attackers.iter().map(|a| a.defending_player).collect();
            players.sort();
            players.dedup();
            players
        })
        .unwrap_or_else(|| {
            // Fallback for pre-combat: all non-active players
            state
                .players
                .iter()
                .filter(|p| p.id != state.active_player)
                .map(|p| p.id)
                .collect()
        });

    state
        .battlefield
        .iter()
        .filter_map(|id| {
            let obj = state.objects.get(id)?;
            if defending_players.contains(&obj.controller)
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !obj.tapped
            {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

/// Return all valid attack targets for the active player: opposing players and their planeswalkers.
pub fn get_valid_attack_targets(state: &GameState) -> Vec<AttackTarget> {
    let active = state.active_player;
    let mut targets = Vec::new();

    // All non-eliminated opponents
    for player in &state.players {
        if player.id != active && !state.eliminated_players.contains(&player.id) {
            targets.push(AttackTarget::Player(player.id));
        }
    }

    // All planeswalkers controlled by opponents
    for &id in &state.battlefield {
        if let Some(obj) = state.objects.get(&id) {
            if obj.controller != active
                && obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Planeswalker)
                && !state.eliminated_players.contains(&obj.controller)
            {
                targets.push(AttackTarget::Planeswalker(id));
            }
        }
    }

    targets
}

/// Check if the active player controls any creatures that could legally attack.
pub fn has_potential_attackers(state: &GameState) -> bool {
    let active = state.active_player;
    let turn = state.turn_number;

    state.battlefield.iter().any(|id| {
        state
            .objects
            .get(id)
            .map(|obj| {
                obj.controller == active
                    && obj.card_types.core_types.contains(&CoreType::Creature)
                    && !obj.tapped
                    && !obj.has_keyword(&Keyword::Defender)
                    && (obj.has_keyword(&Keyword::Haste)
                        || obj.entered_battlefield_turn.is_some_and(|etb| etb < turn))
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state
    }

    fn create_creature(
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
            crate::types::zones::Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1); // entered last turn, not summoning sick
        id
    }

    #[test]
    fn valid_attacker_succeeds() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        assert!(validate_attackers(&state, &[id]).is_ok());
    }

    #[test]
    fn tapped_creature_cannot_attack() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().tapped = true;
        assert!(validate_attackers(&state, &[id]).is_err());
    }

    #[test]
    fn creature_with_defender_cannot_attack() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Wall", 0, 4);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::Defender);
        assert!(validate_attackers(&state, &[id]).is_err());
    }

    #[test]
    fn summoning_sick_creature_cannot_attack() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        // Entered this turn
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(2);
        assert!(validate_attackers(&state, &[id]).is_err());
    }

    #[test]
    fn creature_with_haste_can_attack_immediately() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Hasty", 3, 1);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(2); // this turn
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::Haste);
        assert!(validate_attackers(&state, &[id]).is_ok());
    }

    #[test]
    fn flying_attacker_blocked_only_by_flying_or_reach() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bird", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        let ground_blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let flying_blocker = create_creature(&mut state, PlayerId(1), "Hawk", 1, 1);
        state
            .objects
            .get_mut(&flying_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);
        let reach_blocker = create_creature(&mut state, PlayerId(1), "Spider", 1, 3);
        state
            .objects
            .get_mut(&reach_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Reach);

        // Ground creature can't block flying
        assert!(validate_blockers(&state, &[(ground_blocker, attacker)]).is_err());
        // Flying can block flying
        assert!(validate_blockers(&state, &[(flying_blocker, attacker)]).is_ok());
        // Reach can block flying
        assert!(validate_blockers(&state, &[(reach_blocker, attacker)]).is_ok());
    }

    #[test]
    fn menace_requires_two_or_more_blockers() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Menace Guy", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Menace);

        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);

        // One blocker: illegal
        assert!(validate_blockers(&state, &[(blocker1, attacker)]).is_err());
        // Two blockers: legal
        assert!(validate_blockers(&state, &[(blocker1, attacker), (blocker2, attacker)]).is_ok());
    }

    #[test]
    fn vigilance_prevents_tapping_on_attack() {
        let mut state = setup();
        state.combat = Some(CombatState::default());
        let id = create_creature(&mut state, PlayerId(0), "Knight", 2, 2);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::Vigilance);

        let mut events = Vec::new();
        declare_attackers(
            &mut state,
            &[(id, AttackTarget::Player(PlayerId(1)))],
            &mut events,
        )
        .unwrap();

        assert!(!state.objects[&id].tapped);
    }

    #[test]
    fn attacker_without_vigilance_taps() {
        let mut state = setup();
        state.combat = Some(CombatState::default());
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let mut events = Vec::new();
        declare_attackers(
            &mut state,
            &[(id, AttackTarget::Player(PlayerId(1)))],
            &mut events,
        )
        .unwrap();

        assert!(state.objects[&id].tapped);
    }

    #[test]
    fn declare_attackers_emits_event() {
        let mut state = setup();
        state.combat = Some(CombatState::default());
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let mut events = Vec::new();
        declare_attackers(
            &mut state,
            &[(id, AttackTarget::Player(PlayerId(1)))],
            &mut events,
        )
        .unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::AttackersDeclared { attacker_ids, .. } if attacker_ids == &[id]
        )));
    }

    #[test]
    fn declare_blockers_populates_combat_state() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: PlayerId(1),
            }],
            ..Default::default()
        });

        let mut events = Vec::new();
        declare_blockers(&mut state, &[(blocker, attacker)], &mut events).unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert_eq!(combat.blocker_assignments[&attacker], vec![blocker]);
        assert_eq!(combat.blocker_to_attacker[&blocker], attacker);
    }

    #[test]
    fn has_potential_attackers_with_valid_creature() {
        let mut state = setup();
        create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        assert!(has_potential_attackers(&state));
    }

    #[test]
    fn has_potential_attackers_false_when_no_creatures() {
        let state = setup();
        assert!(!has_potential_attackers(&state));
    }

    #[test]
    fn has_potential_attackers_false_for_summoning_sick() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(2); // this turn
        assert!(!has_potential_attackers(&state));
    }

    #[test]
    fn has_potential_attackers_true_for_haste() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(2);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::Haste);
        assert!(has_potential_attackers(&state));
    }

    #[test]
    fn combat_state_defaults() {
        let combat = CombatState::default();
        assert!(combat.attackers.is_empty());
        assert!(combat.blocker_assignments.is_empty());
        assert!(combat.blocker_to_attacker.is_empty());
        assert!(combat.damage_assignments.is_empty());
        assert!(!combat.first_strike_done);
    }

    #[test]
    fn shadow_blocks_shadow() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Shadow A", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Shadow);

        let shadow_blocker = create_creature(&mut state, PlayerId(1), "Shadow B", 2, 2);
        state
            .objects
            .get_mut(&shadow_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Shadow);

        let normal_blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        // Shadow can block shadow
        assert!(validate_blockers(&state, &[(shadow_blocker, attacker)]).is_ok());
        // Non-shadow cannot block shadow
        assert!(validate_blockers(&state, &[(normal_blocker, attacker)]).is_err());
    }

    #[test]
    fn shadow_cannot_block_non_shadow() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let shadow_blocker = create_creature(&mut state, PlayerId(1), "Shadow B", 2, 2);
        state
            .objects
            .get_mut(&shadow_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Shadow);

        // Shadow creature can't block non-shadow attacker
        assert!(validate_blockers(&state, &[(shadow_blocker, attacker)]).is_err());
    }

    #[test]
    fn cant_be_blocked_creature_is_unblockable() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Invisible Stalker", 1, 1);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition {
                mode: StaticMode::Other("CantBeBlocked".to_string()),
                affected: None,
                modifications: vec![],
                condition: None,
                affected_zone: None,
                effect_zone: None,
                characteristic_defining: false,
                description: None,
            });

        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
    }

    #[test]
    fn creature_without_cant_be_blocked_can_be_blocked() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    #[test]
    fn protection_from_red_prevents_red_creature_blocking() {
        use crate::types::keywords::ProtectionTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "White Knight", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));

        let red_blocker = create_creature(&mut state, PlayerId(1), "Goblin", 1, 1);
        state
            .objects
            .get_mut(&red_blocker)
            .unwrap()
            .color
            .push(ManaColor::Red);

        assert!(validate_blockers(&state, &[(red_blocker, attacker)]).is_err());
    }

    #[test]
    fn protection_from_red_allows_green_creature_blocking() {
        use crate::types::keywords::ProtectionTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "White Knight", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));

        let green_blocker = create_creature(&mut state, PlayerId(1), "Elf", 1, 1);
        state
            .objects
            .get_mut(&green_blocker)
            .unwrap()
            .color
            .push(ManaColor::Green);

        assert!(validate_blockers(&state, &[(green_blocker, attacker)]).is_ok());
    }

    // --- Fear tests ---

    #[test]
    fn fear_cannot_be_blocked_by_non_artifact_non_black() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fear Guy", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Fear);

        let blocker = create_creature(&mut state, PlayerId(1), "Green Bear", 2, 2);
        state.objects.get_mut(&blocker).unwrap().color = vec![ManaColor::Green];

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
    }

    #[test]
    fn fear_can_be_blocked_by_black_creature() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fear Guy", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Fear);

        let blocker = create_creature(&mut state, PlayerId(1), "Black Knight", 2, 2);
        state.objects.get_mut(&blocker).unwrap().color = vec![ManaColor::Black];

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    #[test]
    fn fear_can_be_blocked_by_artifact_creature() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fear Guy", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Fear);

        let blocker = create_creature(&mut state, PlayerId(1), "Golem", 3, 3);
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    // --- Intimidate tests ---

    #[test]
    fn intimidate_cannot_be_blocked_by_non_artifact_no_shared_color() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Intimidate Guy", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Intimidate);
        state.objects.get_mut(&attacker).unwrap().color = vec![ManaColor::Red];

        let blocker = create_creature(&mut state, PlayerId(1), "Green Bear", 2, 2);
        state.objects.get_mut(&blocker).unwrap().color = vec![ManaColor::Green];

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
    }

    #[test]
    fn intimidate_can_be_blocked_by_creature_sharing_color() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Intimidate Guy", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Intimidate);
        state.objects.get_mut(&attacker).unwrap().color = vec![ManaColor::Red, ManaColor::Green];

        let blocker = create_creature(&mut state, PlayerId(1), "Green Bear", 2, 2);
        state.objects.get_mut(&blocker).unwrap().color = vec![ManaColor::Green];

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    // --- Skulk tests ---

    #[test]
    fn skulk_cannot_be_blocked_by_greater_power() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Skulk Guy", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Skulk);

        let blocker = create_creature(&mut state, PlayerId(1), "Big Bear", 3, 3);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
    }

    #[test]
    fn skulk_can_be_blocked_by_equal_power() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Skulk Guy", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Skulk);

        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    #[test]
    fn skulk_can_be_blocked_by_lesser_power() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Skulk Guy", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Skulk);

        let blocker = create_creature(&mut state, PlayerId(1), "Small", 1, 1);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    // --- Horsemanship tests ---

    #[test]
    fn horsemanship_cannot_be_blocked_by_non_horsemanship() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lu Bu", 4, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Horsemanship);

        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
    }

    #[test]
    fn horsemanship_can_be_blocked_by_horsemanship() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lu Bu", 4, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Horsemanship);

        let blocker = create_creature(&mut state, PlayerId(1), "Cao Cao", 3, 3);
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .keywords
            .push(Keyword::Horsemanship);

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }
}
