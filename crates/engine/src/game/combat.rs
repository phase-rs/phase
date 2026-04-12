use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::game_object::GameObject;
use super::players;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::parser::oracle_target::parse_target;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::{Keyword, ProtectionTarget};
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// CR 702.19: Which trample variant applies to combat damage assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrampleKind {
    /// CR 702.19b: Standard trample — excess to attack target.
    Standard,
    /// CR 702.19c: Trample over planeswalkers — excess can spill to PW controller.
    OverPlaneswalkers,
}

/// Represents who a creature is attacking: a player, planeswalker, or battle (CR 506.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AttackTarget {
    Player(PlayerId),
    Planeswalker(ObjectId),
    Battle(ObjectId),
}

/// Serde default for `AttackerInfo.attack_target` — backward-compatible with states
/// serialized before this field existed (all legacy attacks targeted a player).
pub fn default_attack_target() -> AttackTarget {
    AttackTarget::Player(PlayerId(0))
}

/// Tracks the state of the current combat phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CombatState {
    pub attackers: Vec<AttackerInfo>,
    /// attacker_id -> list of blocker ids
    pub blocker_assignments: HashMap<ObjectId, Vec<ObjectId>>,
    /// blocker_id -> attacker_ids (reverse lookup; Vec supports multi-blocking via ExtraBlockers)
    pub blocker_to_attacker: HashMap<ObjectId, Vec<ObjectId>>,
    pub damage_assignments: HashMap<ObjectId, Vec<DamageAssignment>>,
    pub first_strike_done: bool,
    /// Index into attacker list for resumable damage assignment iteration.
    pub damage_step_index: Option<usize>,
    /// CR 510.2: Collected assignments awaiting simultaneous application.
    pub pending_damage: Vec<(ObjectId, DamageAssignment)>,
    /// Whether regular damage has been applied (guards against re-entry from triggers).
    pub regular_damage_done: bool,
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
    /// The full attack target — preserves planeswalker/battle identity through combat.
    #[serde(default = "default_attack_target")]
    pub attack_target: AttackTarget,
    /// CR 509.1h: Once a creature is blocked, it remains blocked for the rest of combat
    /// even if all blockers are removed. Set to `true` during blocker declaration and
    /// never cleared — `unblocked_attackers` checks this flag, not the current blocker list.
    #[serde(default)]
    pub blocked: bool,
}

impl AttackerInfo {
    pub fn new(
        object_id: ObjectId,
        attack_target: AttackTarget,
        defending_player: PlayerId,
    ) -> Self {
        Self {
            object_id,
            defending_player,
            attack_target,
            blocked: false,
        }
    }

    /// Convenience for the common case of attacking a player directly.
    pub fn attacking_player(object_id: ObjectId, player: PlayerId) -> Self {
        Self::new(object_id, AttackTarget::Player(player), player)
    }

    /// Resolve the DamageTarget for this attacker's combat damage (CR 510.1b).
    /// Returns `None` if attacking a planeswalker/battle that left the battlefield (CR 506.4c),
    /// unless `trample_over_pw` is true — then PW removal falls back to the defending
    /// player per CR 702.19e (exception to CR 506.4c).
    pub fn resolve_damage_target(
        &self,
        state: &GameState,
        trample_over_pw: bool,
    ) -> Option<DamageTarget> {
        match &self.attack_target {
            AttackTarget::Player(pid) => Some(DamageTarget::Player(*pid)),
            // CR 506.4c: If the planeswalker left the battlefield, creature assigns no damage.
            // Check zone == Battlefield, not just contains_key — objects persist after zone changes.
            AttackTarget::Planeswalker(pw_id) => match state.objects.get(pw_id) {
                Some(obj) if obj.zone == Zone::Battlefield => Some(DamageTarget::Object(*pw_id)),
                // CR 702.19e: Trample-over-PW falls back to defending player.
                _ if trample_over_pw => Some(DamageTarget::Player(self.defending_player)),
                // CR 506.4c: Without trample-over-PW, no damage.
                _ => None,
            },
            // CR 310.6: Damage to a battle removes defense counters — same Object routing.
            AttackTarget::Battle(battle_id) => match state.objects.get(battle_id) {
                Some(obj) if obj.zone == Zone::Battlefield => {
                    Some(DamageTarget::Object(*battle_id))
                }
                _ => None,
            },
        }
    }
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

/// CR 508.4: Place a permanent onto the battlefield attacking.
/// The creature is not "declared as an attacker" — attack triggers do not fire.
/// Determines the defending player from: (1) source creature's combat info,
/// (2) current trigger event context, (3) fallback to opponent.
pub fn enter_attacking(
    state: &mut GameState,
    object_id: ObjectId,
    source_id: ObjectId,
    controller: PlayerId,
) {
    // CR 508.4: Attacking creatures enter tapped.
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.tapped = true;
    }

    // Determine defending player and attack target before mutable combat borrow.
    let (defending_player, attack_target) = state
        .combat
        .as_ref()
        .and_then(|c| {
            c.attackers
                .iter()
                .find(|a| a.object_id == source_id)
                .map(|a| (a.defending_player, a.attack_target.clone()))
        })
        .or_else(|| {
            state
                .current_trigger_event
                .as_ref()
                .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
                .map(|pid| (pid, AttackTarget::Player(pid)))
        })
        .unwrap_or_else(|| {
            // CR 508.4: Fallback to first opponent in seat order (multiplayer-aware).
            // In 2-player, this returns the sole opponent — identical to the old arithmetic.
            let pid = players::opponents(state, controller)
                .first()
                .copied()
                .unwrap_or(controller);
            (pid, AttackTarget::Player(pid))
        });

    if let Some(combat) = state.combat.as_mut() {
        combat.attackers.push(AttackerInfo::new(
            object_id,
            attack_target,
            defending_player,
        ));
    }
}

/// Validate attacker declarations per CR 508.1.
pub fn validate_attackers(state: &GameState, attacker_ids: &[ObjectId]) -> Result<(), String> {
    let active = state.active_player;

    for &id in attacker_ids {
        let obj = state
            .objects
            .get(&id)
            .ok_or_else(|| format!("Attacker {:?} not found", id))?;

        // CR 508.1: Only battlefield creatures controlled by active player can attack.
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

        // CR 702.3b: Defender — a creature with defender can't attack,
        // unless overridden by CanAttackWithDefender (e.g., Assault Formation).
        if obj.has_keyword(&Keyword::Defender) {
            let can_attack_with_defender = obj
                .static_definitions
                .iter()
                .any(|sd| sd.mode == StaticMode::CanAttackWithDefender)
                || crate::game::static_abilities::check_static_ability(
                    state,
                    StaticMode::CanAttackWithDefender,
                    &crate::game::static_abilities::StaticCheckContext {
                        target_id: Some(id),
                        ..Default::default()
                    },
                );
            if !can_attack_with_defender {
                return Err(format!("{:?} has Defender", id));
            }
        }
        if obj.static_definitions.iter().any(|sd| {
            matches!(
                sd.mode,
                StaticMode::CantAttack | StaticMode::CantAttackOrBlock
            )
        }) {
            return Err(format!("{:?} can't attack", id));
        }

        // CR 701.35a: Detained creatures can't attack.
        if !obj.detained_by.is_empty() {
            return Err(format!("{:?} is detained", id));
        }

        // CR 302.6: Summoning sickness — must have haste or have been under controller's
        // control since the beginning of the turn.
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

/// Validate blocker declarations per CR 509.1.
/// Each assignment is (blocker_id, attacker_id).
pub fn validate_blockers(
    state: &GameState,
    assignments: &[(ObjectId, ObjectId)],
) -> Result<(), String> {
    // Detect duplicate (blocker, attacker) pairs — the Vec-based blocker_to_attacker
    // no longer prevents this implicitly like the old HashMap<ObjectId, ObjectId> did.
    {
        let mut seen = std::collections::HashSet::new();
        for &pair in assignments {
            if !seen.insert(pair) {
                return Err(format!(
                    "Duplicate block assignment: {:?} blocking {:?}",
                    pair.0, pair.1
                ));
            }
        }
    }

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

        // CR 509.1a: Only untapped creatures controlled by the defending player may block.
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
        if blocker.static_definitions.iter().any(|sd| {
            matches!(
                sd.mode,
                StaticMode::CantBlock | StaticMode::CantAttackOrBlock
            )
        }) {
            return Err(format!("{:?} can't block", blocker_id));
        }

        // CR 701.35a: Detained creatures can't block.
        if !blocker.detained_by.is_empty() {
            return Err(format!("{:?} is detained", blocker_id));
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
            .any(|sd| sd.mode == StaticMode::CantBeBlocked)
        {
            return Err(format!(
                "{:?} cannot block {:?} (can't be blocked)",
                blocker_id, attacker_id
            ));
        }

        // CR 509.1b: "can't be blocked except by X" — blocker must match the exception filter.
        for sd in &attacker.static_definitions {
            if let StaticMode::CantBeBlockedExceptBy { filter } = &sd.mode {
                let (target_filter, _) = parse_target(filter);
                if !matches_target_filter(
                    state,
                    blocker_id,
                    &target_filter,
                    &FilterContext::from_source(state, attacker_id),
                ) {
                    return Err(format!(
                        "{:?} cannot block {:?} (can't be blocked except by {})",
                        blocker_id, attacker_id, filter
                    ));
                }
            }
        }

        // CR 509.1b: "can't be blocked by X" — blocker matching the filter is prohibited.
        for sd in &attacker.static_definitions {
            if let StaticMode::CantBeBlockedBy { filter } = &sd.mode {
                if matches_target_filter(
                    state,
                    blocker_id,
                    filter,
                    &FilterContext::from_source(state, attacker_id),
                ) {
                    return Err(format!(
                        "{:?} cannot block {:?} (can't be blocked by {filter:?})",
                        blocker_id, attacker_id
                    ));
                }
            }
        }

        // CR 702.16e: Protection — a creature with protection can't be blocked by
        // creatures with the specified quality.
        for kw in &attacker.keywords {
            match kw {
                Keyword::Protection(ProtectionTarget::Color(color))
                    if blocker.color.contains(color) =>
                {
                    return Err(format!(
                        "{:?} cannot block {:?} (protection from {:?})",
                        blocker_id, attacker_id, color
                    ));
                }
                Keyword::Protection(ProtectionTarget::Multicolored) if blocker.color.len() > 1 => {
                    return Err(format!(
                        "{:?} cannot block {:?} (protection from multicolored)",
                        blocker_id, attacker_id
                    ));
                }
                // CR 702.16: ChosenColor resolves from the source permanent's chosen_attributes
                Keyword::Protection(ProtectionTarget::ChosenColor) => {
                    if let Some(color) = attacker.chosen_color() {
                        if blocker.color.contains(&color) {
                            return Err(format!(
                                "{:?} cannot block {:?} (protection from chosen color {:?})",
                                blocker_id, attacker_id, color
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        // CR 702.9b: Flying — can only be blocked by creatures with flying or reach.
        if attacker.has_keyword(&Keyword::Flying)
            && !blocker.has_keyword(&Keyword::Flying)
            && !blocker.has_keyword(&Keyword::Reach)
        {
            return Err(format!(
                "{:?} cannot block {:?} (flying, no flying/reach)",
                blocker_id, attacker_id
            ));
        }

        // CR 702.28b: Shadow — can only be blocked by creatures with shadow,
        // and cannot block creatures without shadow.
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

        // CR 702.36: Fear — can only be blocked by artifact creatures or black creatures.
        if attacker.has_keyword(&Keyword::Fear)
            && !blocker.card_types.core_types.contains(&CoreType::Artifact)
            && !blocker.color.contains(&ManaColor::Black)
        {
            return Err(format!(
                "{:?} cannot block {:?} (fear: must be artifact or black)",
                blocker_id, attacker_id
            ));
        }

        // CR 702.13: Intimidate — can only be blocked by artifact creatures or creatures
        // sharing a color with the attacker.
        if attacker.has_keyword(&Keyword::Intimidate)
            && !blocker.card_types.core_types.contains(&CoreType::Artifact)
            && !attacker.color.iter().any(|c| blocker.color.contains(c))
        {
            return Err(format!(
                "{:?} cannot block {:?} (intimidate: must be artifact or share a color)",
                blocker_id, attacker_id
            ));
        }

        // CR 702.118b: Skulk — cannot be blocked by creatures with strictly greater power.
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

        // CR 702.31b: Horsemanship — can only be blocked by creatures with horsemanship.
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

    // CR 509.1a + CR 509.1b: Enforce per-blocker limit on how many attackers it can block.
    // Default is 1; ExtraBlockers { count: Some(n) } allows 1 + n; count: None = unlimited.
    {
        let mut attackers_per_blocker: HashMap<ObjectId, u32> = HashMap::new();
        for &(blocker_id, _) in assignments {
            *attackers_per_blocker.entry(blocker_id).or_default() += 1;
        }
        for (&blocker_id, &num_blocked) in &attackers_per_blocker {
            if num_blocked <= 1 {
                continue;
            }
            let blocker = state
                .objects
                .get(&blocker_id)
                .ok_or_else(|| format!("Blocker {:?} not found during limit check", blocker_id))?;
            // Find the best ExtraBlockers grant on this creature
            let max_allowed = extra_block_limit(blocker);
            if num_blocked > max_allowed {
                return Err(format!(
                    "{:?} is blocking {} attackers but can only block {}",
                    blocker_id, num_blocked, max_allowed
                ));
            }
        }
    }

    // CR 702.111b: Menace — must be blocked by two or more creatures or not at all.
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

    // CR 509.1c: MustBeBlocked — if a creature with "must be blocked if able" is attacking,
    // the defending player must assign at least one blocker to it, provided a legal blocker
    // exists that isn't already required elsewhere.
    if let Some(combat) = &state.combat {
        // Collect all assigned blocker IDs for quick lookup
        let assigned_blockers: std::collections::HashSet<ObjectId> = assignments
            .iter()
            .map(|&(blocker_id, _)| blocker_id)
            .collect();

        for attacker_info in &combat.attackers {
            let attacker_id = attacker_info.object_id;
            let attacker = match state.objects.get(&attacker_id) {
                Some(obj) => obj,
                None => continue,
            };

            // Check if this attacker has MustBeBlocked
            let has_must_be_blocked = attacker
                .static_definitions
                .iter()
                .any(|sd| sd.mode == StaticMode::MustBeBlocked);
            if !has_must_be_blocked {
                continue;
            }

            // Already has at least one blocker assigned — constraint satisfied
            if blockers_per_attacker.contains_key(&attacker_id) {
                continue;
            }

            // Check if any unassigned defending creature could legally block this attacker.
            // If so, the assignment is invalid because that creature should have been assigned.
            let defending_player = attacker_info.defending_player;
            let has_available_blocker = state.battlefield.iter().any(|id| {
                if assigned_blockers.contains(id) {
                    return false;
                }
                let Some(obj) = state.objects.get(id) else {
                    return false;
                };
                obj.controller == defending_player
                    && obj.card_types.core_types.contains(&CoreType::Creature)
                    && !obj.tapped
                    && can_block_pair(state, *id, attacker_id)
            });

            if has_available_blocker {
                return Err(format!(
                    "{:?} must be blocked if able (CR 509.1c)",
                    attacker_id
                ));
            }
        }

        // CR 509.1c: Check MustBlock — creatures that must block if able.
        // If a defending creature has MustBlock and isn't assigned as a blocker,
        // verify it couldn't legally block any attacker.
        // Collect all defending players from combat state (multiplayer-safe).
        let defending_players: std::collections::HashSet<PlayerId> = combat
            .attackers
            .iter()
            .map(|a| a.defending_player)
            .collect();

        for &obj_id in &state.battlefield {
            let Some(obj) = state.objects.get(&obj_id) else {
                continue;
            };
            if !defending_players.contains(&obj.controller) {
                continue;
            }
            if !obj.card_types.core_types.contains(&CoreType::Creature) {
                continue;
            }
            // CR 509.1c: Check MustBlock — directly on this creature or from
            // a cross-permanent static (e.g., "All creatures block each combat if able").
            let has_must_block = obj
                .static_definitions
                .iter()
                .any(|sd| sd.mode == StaticMode::MustBlock)
                || crate::game::static_abilities::check_static_ability(
                    state,
                    StaticMode::MustBlock,
                    &crate::game::static_abilities::StaticCheckContext {
                        target_id: Some(obj_id),
                        ..Default::default()
                    },
                );
            if !has_must_block {
                continue;
            }
            // Already assigned as a blocker — constraint satisfied
            if assigned_blockers.contains(&obj_id) {
                continue;
            }
            // Tapped creatures can't block (CR 509.1a)
            if obj.tapped {
                continue;
            }
            if obj.static_definitions.iter().any(|sd| {
                matches!(
                    sd.mode,
                    StaticMode::CantBlock | StaticMode::CantAttackOrBlock
                )
            }) {
                continue;
            }
            // CR 701.35a: Detained creatures can't block.
            if !obj.detained_by.is_empty() {
                continue;
            }
            // Check if this creature could legally block any attacker attacking its controller
            let can_block_any = combat.attackers.iter().any(|ai| {
                ai.defending_player == obj.controller && can_block_pair(state, obj_id, ai.object_id)
            });
            if can_block_any {
                return Err(format!("{:?} must block if able (CR 509.1c)", obj_id));
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

    // CR 508.1d: Creatures that must attack each combat if able.
    // If a creature has MustAttack, is untapped, has no summoning sickness,
    // no defender, and is controlled by the active player, it must be in the
    // attacker list.
    let active = state.active_player;
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if obj.controller != active {
            continue;
        }
        if !obj.card_types.core_types.contains(&CoreType::Creature) {
            continue;
        }
        // CR 508.1d: Check for MustAttack — either directly on this creature
        // or from a cross-permanent static (e.g., "All creatures attack each combat if able").
        let has_must_attack = obj
            .static_definitions
            .iter()
            .any(|sd| sd.mode == StaticMode::MustAttack)
            || crate::game::static_abilities::check_static_ability(
                state,
                StaticMode::MustAttack,
                &crate::game::static_abilities::StaticCheckContext {
                    target_id: Some(obj_id),
                    ..Default::default()
                },
            );
        // CR 701.15b: Goaded creatures must attack each combat if able.
        let is_goaded = !obj.goaded_by.is_empty();
        if !has_must_attack && !is_goaded {
            continue;
        }
        // Already declared as attacker — constraint satisfied
        if attacker_ids.contains(&obj_id) {
            continue;
        }
        // Exemptions: tapped, summoning sick, defender, can't attack statics
        if obj.tapped {
            continue;
        }
        // CR 702.3b: Defender — creature can't attack (unless overridden).
        if obj.has_keyword(&Keyword::Defender) {
            let can_attack_with_defender = obj
                .static_definitions
                .iter()
                .any(|sd| sd.mode == StaticMode::CanAttackWithDefender)
                || crate::game::static_abilities::check_static_ability(
                    state,
                    StaticMode::CanAttackWithDefender,
                    &crate::game::static_abilities::StaticCheckContext {
                        target_id: Some(obj_id),
                        ..Default::default()
                    },
                );
            if !can_attack_with_defender {
                continue;
            }
        }
        // CR 302.6: Summoning sickness — reuse existing helper.
        if has_summoning_sickness(obj, state.turn_number) {
            continue;
        }
        // Creature could legally attack but wasn't declared
        if is_goaded {
            return Err(format!(
                "{:?} is goaded and must attack this combat if able (CR 701.15b)",
                obj_id
            ));
        }
        return Err(format!(
            "{:?} must attack this combat if able (CR 508.1d)",
            obj_id
        ));
    }

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
            AttackTarget::Battle(battle_id) => {
                // CR 310.5: Battles can be attacked.
                let battle = state
                    .objects
                    .get(battle_id)
                    .ok_or_else(|| format!("Battle {:?} not found", battle_id))?;
                if battle.zone != crate::types::zones::Zone::Battlefield
                    || !battle
                        .card_types
                        .core_types
                        .contains(&crate::types::card_type::CoreType::Battle)
                {
                    return Err(format!(
                        "{:?} is not a battle on the battlefield",
                        battle_id
                    ));
                }
                // CR 310.8b: A battle's protector can never attack it.
                if battle.controller == state.active_player {
                    return Err(format!("Cannot attack your own battle {:?}", battle_id));
                }
            }
        }
    }

    // CR 701.15b: Goaded creatures must attack a player other than the goading player
    // if able. If all legal attack targets are goading players, the creature can still
    // attack any of them.
    let non_eliminated_opponents: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| p.id != state.active_player && !state.eliminated_players.contains(&p.id))
        .map(|p| p.id)
        .collect();

    for (attacker_id, target) in attacks {
        if let AttackTarget::Player(defending_pid) = target {
            let Some(obj) = state.objects.get(attacker_id) else {
                continue;
            };
            if obj.goaded_by.is_empty() {
                continue;
            }
            // Check if this creature is attacking a goading player
            if obj.goaded_by.contains(defending_pid) {
                // CR 701.15b: Check if there's at least one non-goading opponent
                let has_non_goading_target = non_eliminated_opponents
                    .iter()
                    .any(|pid| !obj.goaded_by.contains(pid));
                if has_non_goading_target {
                    return Err(format!(
                        "{:?} is goaded by {:?} and must attack a different player if able (CR 701.15b)",
                        attacker_id, defending_pid
                    ));
                }
            }
        }
    }

    // CR 508.1f: Tap attackers. CR 508.1k: Creatures become attacking creatures.
    for &id in &attacker_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            // CR 702.20a: Vigilance prevents tapping on attack.
            if !obj.has_keyword(&Keyword::Vigilance) {
                obj.tapped = true;
                events.push(GameEvent::PermanentTapped {
                    object_id: id,
                    caused_by: None,
                });
            }
        }
    }

    // Populate CombatState with per-creature defending players and attack targets
    let combat = state.combat.get_or_insert_with(CombatState::default);
    combat.attackers = attacks
        .iter()
        .map(|(object_id, target)| {
            let defending_player = match target {
                AttackTarget::Player(pid) => *pid,
                AttackTarget::Planeswalker(pw_id) | AttackTarget::Battle(pw_id) => state
                    .objects
                    .get(pw_id)
                    .map(|pw| pw.controller)
                    .unwrap_or(PlayerId(0)),
            };
            AttackerInfo::new(*object_id, target.clone(), defending_player)
        })
        .collect();
    state.players_attacked_this_step = combat
        .attackers
        .iter()
        .map(|a| a.defending_player)
        .collect();
    let attacker_count = combat.attackers.len();

    // Use the first attacker's defending player for the event
    let defending_player = combat
        .attackers
        .first()
        .map(|a| a.defending_player)
        .unwrap_or_else(|| players::next_player(state, state.active_player));

    events.push(GameEvent::AttackersDeclared {
        attacker_ids: attacker_ids.clone(),
        defending_player,
        attacks: attacks.to_vec(),
    });

    // Emit Firebend events for each attacking creature with firebending.
    // These go into the same events batch so process_triggers catches both
    // AttackersDeclared (for the mana trigger) and Firebend (for Avatar Aang).
    for &obj_id in &attacker_ids {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj
                .keywords
                .iter()
                .any(|k| matches!(k, Keyword::Firebending(_)))
            {
                super::bending::record_bending(
                    state,
                    events,
                    crate::types::events::BendingType::Fire,
                    obj_id,
                    obj.controller,
                );
            }
        }
    }

    // CR 508.1a: Record attacker object IDs for per-turn tracking.
    state
        .creatures_attacked_this_turn
        .extend(attacker_ids.iter().copied());

    super::restrictions::record_attackers_declared(state, attacker_count);

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

    // CR 509.1g: Chosen creatures become blocking creatures.
    let mut grouped: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for &(blocker_id, attacker_id) in assignments {
        grouped.entry(attacker_id).or_default().push(blocker_id);
        combat
            .blocker_to_attacker
            .entry(blocker_id)
            .or_default()
            .push(attacker_id);
    }

    // Auto-order blockers by ObjectId ascending (deterministic default)
    for (attacker_id, mut blockers) in grouped {
        blockers.sort_by_key(|id| id.0);
        combat.blocker_assignments.insert(attacker_id, blockers);
        // CR 509.1h: Mark the attacker as blocked — this flag is permanent for the rest of combat.
        if let Some(info) = combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker_id)
        {
            info.blocked = true;
        }
    }

    // CR 509.1a: Record blocker object IDs for per-turn tracking.
    state
        .creatures_blocked_this_turn
        .extend(assignments.iter().map(|(blocker_id, _)| *blocker_id));

    events.push(GameEvent::BlockersDeclared {
        assignments: assignments.to_vec(),
    });

    Ok(())
}

/// CR 509.1h + CR 702.49a: Returns ObjectIds of attackers that were never blocked.
/// Per CR 509.1h, a creature remains blocked for the rest of combat even if all
/// blockers are removed. This function checks the `blocked` flag set at blocker
/// declaration, not the current blocker list.
pub fn unblocked_attackers(state: &GameState) -> Vec<ObjectId> {
    let Some(combat) = &state.combat else {
        return Vec::new();
    };
    combat
        .attackers
        .iter()
        .filter(|a| !a.blocked)
        .map(|a| a.object_id)
        .collect()
}

/// Check if a creature has summoning sickness (entered this turn without Haste).
/// CR 302.6: Creature must have been under controller's control continuously since turn began.
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

/// CR 508.1a / CR 302.6: Untapped creature controlled since turn started, without Defender.
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
                && (!obj.has_keyword(&Keyword::Defender)
                    || obj
                        .static_definitions
                        .iter()
                        .any(|sd| sd.mode == StaticMode::CanAttackWithDefender)
                    || crate::game::static_abilities::check_static_ability(
                        state,
                        StaticMode::CanAttackWithDefender,
                        &crate::game::static_abilities::StaticCheckContext {
                            target_id: Some(*id),
                            ..Default::default()
                        },
                    ))
                && !obj.static_definitions.iter().any(|sd| {
                    matches!(
                        sd.mode,
                        StaticMode::CantAttack | StaticMode::CantAttackOrBlock
                    )
                })
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

/// Check per-pair blocking legality (evasion abilities, CR 509.1b).
/// Does NOT check menace (which is a multi-blocker constraint).
/// CR 509.1a–b: Check if a specific blocker can legally block a specific attacker,
/// accounting for all blocking restrictions (CantBeBlocked, CantBeBlockedExceptBy,
/// CantBeBlockedBy, Protection, Flying/Reach, Shadow, Fear, Intimidate, Skulk,
/// Horsemanship, CantBlock/CantAttackOrBlock).
pub fn can_block_pair(state: &GameState, blocker_id: ObjectId, attacker_id: ObjectId) -> bool {
    let Some(blocker) = state.objects.get(&blocker_id) else {
        return false;
    };
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return false;
    };
    if blocker.static_definitions.iter().any(|sd| {
        matches!(
            sd.mode,
            StaticMode::CantBlock | StaticMode::CantAttackOrBlock
        )
    }) {
        return false;
    }
    if attacker
        .static_definitions
        .iter()
        .any(|sd| sd.mode == StaticMode::CantBeBlocked)
    {
        return false;
    }
    // CR 509.1b: "can't be blocked except by X" — blocker must match the exception filter.
    for sd in &attacker.static_definitions {
        if let StaticMode::CantBeBlockedExceptBy { filter } = &sd.mode {
            let (target_filter, _) = parse_target(filter);
            if !matches_target_filter(
                state,
                blocker_id,
                &target_filter,
                &FilterContext::from_source(state, attacker_id),
            ) {
                return false;
            }
        }
    }
    // CR 509.1b: "can't be blocked by X" — blocker matching the filter is prohibited.
    for sd in &attacker.static_definitions {
        if let StaticMode::CantBeBlockedBy { filter } = &sd.mode {
            if matches_target_filter(
                state,
                blocker_id,
                filter,
                &FilterContext::from_source(state, attacker_id),
            ) {
                return false;
            }
        }
    }
    for kw in &attacker.keywords {
        match kw {
            Keyword::Protection(ProtectionTarget::Color(color))
                if blocker.color.contains(color) =>
            {
                return false;
            }
            Keyword::Protection(ProtectionTarget::Multicolored) if blocker.color.len() > 1 => {
                return false;
            }
            // CR 702.16: ChosenColor resolves from the source permanent's chosen_attributes
            Keyword::Protection(ProtectionTarget::ChosenColor) => {
                if let Some(color) = attacker.chosen_color() {
                    if blocker.color.contains(&color) {
                        return false;
                    }
                }
            }
            _ => {}
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

/// CR 509.1a + CR 509.1b: Compute the maximum number of attackers a creature can block.
/// Default is 1. ExtraBlockers { count: Some(n) } adds n (so 1+n). count: None = unlimited (u32::MAX).
/// Multiple ExtraBlockers stack: the best (highest) limit wins.
fn extra_block_limit(blocker: &GameObject) -> u32 {
    let mut max: u32 = 1;
    for sd in &blocker.static_definitions {
        if let StaticMode::ExtraBlockers { count } = &sd.mode {
            match count {
                None => return u32::MAX, // unlimited
                Some(n) => max = max.max(1 + n),
            }
        }
    }
    max
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
        // CR 509.1a: Blocker must block a creature attacking the blocker's controller.
        let valid_targets: Vec<ObjectId> = combat
            .attackers
            .iter()
            .filter(|a| a.defending_player == blocker_controller)
            .filter(|a| can_block_pair(state, blocker_id, a.object_id))
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

/// CR 506.2 / CR 506.3: Valid attack targets are opposing players and planeswalkers/battles.
pub fn get_valid_attack_targets(state: &GameState) -> Vec<AttackTarget> {
    let active = state.active_player;
    let allies = players::teammates(state, active);
    let mut targets = Vec::new();

    // All non-eliminated opponents (excluding teammates)
    for player in &state.players {
        if player.id != active
            && !state.eliminated_players.contains(&player.id)
            && !allies.contains(&player.id)
        {
            targets.push(AttackTarget::Player(player.id));
        }
    }

    // All planeswalkers controlled by opponents (excluding teammates')
    for &id in &state.battlefield {
        if let Some(obj) = state.objects.get(&id) {
            if obj.controller != active
                && !allies.contains(&obj.controller)
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

    // CR 310.11 + CR 506.2: Battles controlled by opponents are valid attack targets.
    for &id in &state.battlefield {
        if let Some(obj) = state.objects.get(&id) {
            if obj.controller != active
                && !allies.contains(&obj.controller)
                && obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Battle)
                && !state.eliminated_players.contains(&obj.controller)
            {
                targets.push(AttackTarget::Battle(id));
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
                    && (!obj.has_keyword(&Keyword::Defender)
                        || obj
                            .static_definitions
                            .iter()
                            .any(|sd| sd.mode == StaticMode::CanAttackWithDefender)
                        || crate::game::static_abilities::check_static_ability(
                            state,
                            StaticMode::CanAttackWithDefender,
                            &crate::game::static_abilities::StaticCheckContext {
                                target_id: Some(*id),
                                ..Default::default()
                            },
                        ))
                    && !obj.static_definitions.iter().any(|sd| {
                        matches!(
                            sd.mode,
                            StaticMode::CantAttack | StaticMode::CantAttackOrBlock
                        )
                    })
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
    use crate::types::ability::StaticDefinition;
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
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        let mut events = Vec::new();
        declare_blockers(&mut state, &[(blocker, attacker)], &mut events).unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert_eq!(combat.blocker_assignments[&attacker], vec![blocker]);
        assert_eq!(combat.blocker_to_attacker[&blocker], vec![attacker]);
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
            .push(StaticDefinition::new(StaticMode::CantBeBlocked));

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
    fn cant_block_static_prevents_creature_from_blocking() {
        use crate::types::ability::StaticDefinition;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBlock));

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
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

    #[test]
    fn extra_blockers_allows_blocking_two_attackers() {
        use crate::types::ability::StaticDefinition;

        let mut state = setup();
        let attacker1 = create_creature(&mut state, PlayerId(0), "Bear A", 2, 2);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Bear B", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Palace Guard", 1, 4);

        // CR 509.1b: "can block an additional creature" → ExtraBlockers { count: Some(1) }
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::ExtraBlockers {
                count: Some(1),
            }));

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(attacker1, PlayerId(1)),
                AttackerInfo::attacking_player(attacker2, PlayerId(1)),
            ],
            ..Default::default()
        });

        // Blocking two attackers should succeed with ExtraBlockers { count: Some(1) }
        assert!(validate_blockers(&state, &[(blocker, attacker1), (blocker, attacker2)]).is_ok());
    }

    #[test]
    fn extra_blockers_rejects_exceeding_limit() {
        use crate::types::ability::StaticDefinition;

        let mut state = setup();
        let attacker1 = create_creature(&mut state, PlayerId(0), "Bear A", 2, 2);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Bear B", 2, 2);
        let attacker3 = create_creature(&mut state, PlayerId(0), "Bear C", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Palace Guard", 1, 4);

        // "can block an additional creature" → can block 2, not 3
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::ExtraBlockers {
                count: Some(1),
            }));

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(attacker1, PlayerId(1)),
                AttackerInfo::attacking_player(attacker2, PlayerId(1)),
                AttackerInfo::attacking_player(attacker3, PlayerId(1)),
            ],
            ..Default::default()
        });

        // Blocking three attackers should fail
        assert!(validate_blockers(
            &state,
            &[
                (blocker, attacker1),
                (blocker, attacker2),
                (blocker, attacker3)
            ]
        )
        .is_err());
    }

    #[test]
    fn extra_blockers_unlimited_allows_many() {
        use crate::types::ability::StaticDefinition;

        let mut state = setup();
        let attacker1 = create_creature(&mut state, PlayerId(0), "Bear A", 2, 2);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Bear B", 2, 2);
        let attacker3 = create_creature(&mut state, PlayerId(0), "Bear C", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Hundred-Handed One", 3, 5);

        // "can block any number of creatures" → ExtraBlockers { count: None }
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::ExtraBlockers {
                count: None,
            }));

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(attacker1, PlayerId(1)),
                AttackerInfo::attacking_player(attacker2, PlayerId(1)),
                AttackerInfo::attacking_player(attacker3, PlayerId(1)),
            ],
            ..Default::default()
        });

        // Blocking three attackers should succeed with unlimited
        assert!(validate_blockers(
            &state,
            &[
                (blocker, attacker1),
                (blocker, attacker2),
                (blocker, attacker3)
            ]
        )
        .is_ok());
    }

    #[test]
    fn normal_creature_cannot_block_two_attackers() {
        let mut state = setup();
        let attacker1 = create_creature(&mut state, PlayerId(0), "Bear A", 2, 2);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Bear B", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(attacker1, PlayerId(1)),
                AttackerInfo::attacking_player(attacker2, PlayerId(1)),
            ],
            ..Default::default()
        });

        // CR 509.1a: Default is blocking only one creature
        assert!(validate_blockers(&state, &[(blocker, attacker1), (blocker, attacker2)]).is_err());
    }

    #[test]
    fn duplicate_block_assignment_rejected() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // Same (blocker, attacker) pair submitted twice
        assert!(validate_blockers(&state, &[(blocker, attacker), (blocker, attacker)]).is_err());
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

    // -----------------------------------------------------------------------
    // MustBeBlocked (CR 509.1c) tests
    // -----------------------------------------------------------------------

    /// Helper: add MustBeBlocked static to a creature's base definitions.
    fn add_must_be_blocked(state: &mut GameState, id: ObjectId) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustBeBlocked));
    }

    #[test]
    fn must_be_blocked_requires_blocker_assignment() {
        // CR 509.1c: If a MustBeBlocked creature attacks and a legal blocker exists,
        // the defending player must assign at least one blocker to it.
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lure Beast", 3, 3);
        add_must_be_blocked(&mut state, attacker);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // Empty blockers: illegal because blocker exists
        assert!(validate_blockers(&state, &[]).is_err());
        // Assigning the blocker: legal
        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    #[test]
    fn must_be_blocked_ok_when_no_legal_blockers() {
        // CR 509.1c "if able": no legal blockers means empty assignment is fine.
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lure Beast", 3, 3);
        add_must_be_blocked(&mut state, attacker);

        // Defender has only tapped creatures
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        state.objects.get_mut(&blocker).unwrap().tapped = true;

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // No untapped blockers available — constraint satisfied
        assert!(validate_blockers(&state, &[]).is_ok());
    }

    #[test]
    fn must_be_blocked_respects_flying_evasion() {
        // MustBeBlocked doesn't force illegal blocks: flying attacker can't be
        // blocked by ground creature.
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Flying Lure", 3, 3);
        add_must_be_blocked(&mut state, attacker);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        // Defender has only ground creatures
        let _ground = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // No legal blocker (ground can't block flying) — empty is OK
        assert!(validate_blockers(&state, &[]).is_ok());
    }

    #[test]
    fn must_be_blocked_with_menace_needs_two() {
        // CR 509.1c + CR 702.111b: MustBeBlocked + Menace still needs 2+ blockers.
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Menace Lure", 3, 3);
        add_must_be_blocked(&mut state, attacker);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Menace);

        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // One blocker: fails menace even though must-be-blocked
        assert!(validate_blockers(&state, &[(blocker1, attacker)]).is_err());
        // Two blockers: satisfies both menace and must-be-blocked
        assert!(validate_blockers(&state, &[(blocker1, attacker), (blocker2, attacker)]).is_ok());
    }

    #[test]
    fn two_must_be_blocked_one_available_blocker() {
        // CR 509.1c "if able": two MustBeBlocked attackers but only one blocker —
        // assigning the blocker to either satisfies the constraint.
        let mut state = setup();
        let attacker1 = create_creature(&mut state, PlayerId(0), "Lure1", 3, 3);
        add_must_be_blocked(&mut state, attacker1);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Lure2", 2, 2);
        add_must_be_blocked(&mut state, attacker2);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(attacker1, PlayerId(1)),
                AttackerInfo::attacking_player(attacker2, PlayerId(1)),
            ],
            ..Default::default()
        });

        // Blocking either one is fine — can't block both with one creature
        assert!(validate_blockers(&state, &[(blocker, attacker1)]).is_ok());
        assert!(validate_blockers(&state, &[(blocker, attacker2)]).is_ok());
        // Blocking neither is illegal — the blocker could have blocked one
        assert!(validate_blockers(&state, &[]).is_err());
    }

    // --- MustBlock tests (CR 509.1c) ---

    #[test]
    fn must_block_rejects_empty_blockers_when_legal_block_available() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        // Grant MustBlock to the blocker
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustBlock));

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // Not blocking: illegal — blocker could legally block
        assert!(validate_blockers(&state, &[]).is_err());
        // Blocking: legal
        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    #[test]
    fn must_block_accepts_when_tapped() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustBlock));
        state.objects.get_mut(&blocker).unwrap().tapped = true;

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // Tapped creature can't block — constraint satisfied
        assert!(validate_blockers(&state, &[]).is_ok());
    }

    #[test]
    fn must_block_accepts_when_no_legal_target() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Flyer", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        // Ground creature with MustBlock can't block flying — constraint satisfied
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustBlock));

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        assert!(validate_blockers(&state, &[]).is_ok());
    }

    // ---- MustAttack enforcement tests ----

    fn setup_combat_phase() -> GameState {
        let mut state = setup();
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        state
    }

    fn create_must_attack_creature(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = create_creature(state, owner, "Berserker", 3, 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustAttack));
        id
    }

    #[test]
    fn must_attack_enforcement_omitted_creature_fails() {
        let mut state = setup_combat_phase();
        let must_attacker = create_must_attack_creature(&mut state, PlayerId(0));
        // Declare no attackers — should fail because must_attacker can legally attack
        let result = declare_attackers(&mut state, &[], &mut vec![]);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("must attack this combat if able"),
            "Error should mention must-attack requirement"
        );
        // Suppress unused variable warning
        let _ = must_attacker;
    }

    #[test]
    fn must_attack_enforcement_tapped_creature_exempt() {
        let mut state = setup_combat_phase();
        let must_attacker = create_must_attack_creature(&mut state, PlayerId(0));
        state.objects.get_mut(&must_attacker).unwrap().tapped = true;
        // Tapped creature is exempt — empty attacker list should be fine
        assert!(declare_attackers(&mut state, &[], &mut vec![]).is_ok());
    }

    #[test]
    fn must_attack_enforcement_summoning_sick_exempt() {
        let mut state = setup_combat_phase();
        let must_attacker = create_must_attack_creature(&mut state, PlayerId(0));
        // Set entered_battlefield_turn to current turn (summoning sick)
        state
            .objects
            .get_mut(&must_attacker)
            .unwrap()
            .entered_battlefield_turn = Some(state.turn_number);
        assert!(declare_attackers(&mut state, &[], &mut vec![]).is_ok());
    }

    #[test]
    fn must_attack_enforcement_defender_exempt() {
        let mut state = setup_combat_phase();
        let must_attacker = create_must_attack_creature(&mut state, PlayerId(0));
        state
            .objects
            .get_mut(&must_attacker)
            .unwrap()
            .keywords
            .push(Keyword::Defender);
        assert!(declare_attackers(&mut state, &[], &mut vec![]).is_ok());
    }

    #[test]
    fn must_attack_enforcement_included_in_attackers_passes() {
        let mut state = setup_combat_phase();
        let must_attacker = create_must_attack_creature(&mut state, PlayerId(0));
        // Declare the must-attack creature as an attacker — should pass
        let result = declare_attackers(
            &mut state,
            &[(must_attacker, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn must_attack_enforcement_no_must_attack_creatures_passes() {
        let mut state = setup_combat_phase();
        // Regular creature without MustAttack — can skip attacking
        let _normal = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        assert!(declare_attackers(&mut state, &[], &mut vec![]).is_ok());
    }

    // ---- Goad enforcement tests ----

    fn create_goaded_creature(
        state: &mut GameState,
        owner: PlayerId,
        goading_player: PlayerId,
    ) -> ObjectId {
        let id = create_creature(state, owner, "Goaded Bear", 2, 2);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .goaded_by
            .insert(goading_player);
        id
    }

    #[test]
    fn goad_enforcement_omitted_creature_fails() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        // Declare no attackers — goaded creature must attack if able.
        let result = declare_attackers(&mut state, &[], &mut vec![]);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("goaded"),
            "Error should mention goaded"
        );
        let _ = goaded;
    }

    #[test]
    fn goad_enforcement_attacking_passes() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        // Declare goaded creature as attacker attacking non-goading player.
        let result = declare_attackers(
            &mut state,
            &[(goaded, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn goad_enforcement_tapped_creature_exempt() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        state.objects.get_mut(&goaded).unwrap().tapped = true;
        // Tapped creature can't attack — goad constraint satisfied.
        assert!(declare_attackers(&mut state, &[], &mut vec![]).is_ok());
    }

    #[test]
    fn goad_enforcement_summoning_sick_exempt() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        // Set ETB turn to current turn → summoning sick.
        state
            .objects
            .get_mut(&goaded)
            .unwrap()
            .entered_battlefield_turn = Some(state.turn_number);
        assert!(declare_attackers(&mut state, &[], &mut vec![]).is_ok());
    }

    #[test]
    fn goad_enforcement_defender_exempt() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        state
            .objects
            .get_mut(&goaded)
            .unwrap()
            .keywords
            .push(Keyword::Defender);
        // Creature with Defender can't attack — goad constraint satisfied.
        assert!(declare_attackers(&mut state, &[], &mut vec![]).is_ok());
    }

    #[test]
    fn goad_enforcement_cant_attack_goading_player() {
        let mut state = setup_combat_phase();
        // Goaded by player 1 — must attack someone other than player 1 if able.
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        // CR 701.15b: Attacking the goading player when another target exists is invalid.
        // In a 2-player game, the only opponent IS the goading player, so it should be allowed.
        let result = declare_attackers(
            &mut state,
            &[(goaded, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        // In a 2-player game, player 1 is the only valid attack target, so this is fine.
        assert!(result.is_ok());
    }

    #[test]
    fn cant_be_blocked_except_by_enforces_filter() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Phantom Warrior", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                filter: "creatures with flying".to_string(),
            }));

        let ground_blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let flying_blocker = create_creature(&mut state, PlayerId(1), "Bird", 1, 1);
        state
            .objects
            .get_mut(&flying_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        // Ground creature cannot block (doesn't match "creatures with flying")
        assert!(validate_blockers(&state, &[(ground_blocker, attacker)]).is_err());
        // Flying creature can block (matches the exception filter)
        assert!(validate_blockers(&state, &[(flying_blocker, attacker)]).is_ok());
    }

    #[test]
    fn goad_duration_cleanup_clears_goaded_by() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));

        // Verify goaded_by is set.
        assert!(!state.objects.get(&goaded).unwrap().goaded_by.is_empty());

        // Simulate goading player's next turn by calling prune_until_next_turn_effects.
        crate::game::layers::prune_until_next_turn_effects(&mut state, PlayerId(1));

        // CR 701.15a: Goad expires at the goading player's next turn.
        assert!(state.objects.get(&goaded).unwrap().goaded_by.is_empty());
    }
}
