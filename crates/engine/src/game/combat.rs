use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::game_object::GameObject;
use super::players;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::parser::oracle_target::parse_target;
use crate::types::ability::StaticDefinition;
use crate::types::card_type::{CoreType, Supertype};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Defending players who have declared blockers this step.
    #[serde(default)]
    pub blockers_declared_by: Vec<PlayerId>,
    /// Blocker declaration events waiting for CR 509.2a trigger processing after
    /// every defending player has declared blockers.
    #[serde(default)]
    pub pending_blocker_declaration_events: Vec<GameEvent>,
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
            && self.blockers_declared_by == other.blockers_declared_by
            && self.pending_blocker_declaration_events == other.pending_blocker_declaration_events
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
                .map(|a| (a.defending_player, a.attack_target))
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

/// CR 702.49c + CR 702.190b: Place an object onto `combat.attackers` alongside
/// an existing attacker without firing `AttackersDeclared` (so "whenever ~
/// attacks" triggers do not fire). Sets the tapped bit and
/// `entered_battlefield_turn` for summoning-sickness tracking.
///
/// Shared authority for the Ninjutsu activation path (CR 702.49c) and the
/// Sneak cast path (CR 702.190b).
pub fn place_attacking_alongside(
    state: &mut GameState,
    object_id: ObjectId,
    defending_player: PlayerId,
    attack_target: AttackTarget,
    _events: &mut Vec<GameEvent>,
) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.tapped = true;
        obj.entered_battlefield_turn = Some(state.turn_number);
        // CR 302.6: Ninjutsu/Sneak places a new permanent already attacking.
        // The attack declaration itself bypasses the normal summoning-
        // sickness check for attacking, but the flag remains true so {T}
        // activations are still gated. Cannot call `reset_for_battlefield_entry`
        // here because `cast_variant_paid` was set by the Sneak/Ninjutsu
        // pipeline upstream and must be preserved (the reset clears it
        // under CR 400.7's new-object semantics, but these keywords set it
        // at entry time, not re-entry).
        obj.summoning_sick = true;
    }
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
        // CR 702.26b: Phased-out permanents are treated as though they don't
        // exist — they can't attack.
        if obj.is_phased_out() {
            return Err(format!("{:?} is phased out", id));
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
            let can_attack_with_defender =
                super::functioning_abilities::active_static_definitions(state, obj)
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
        if super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
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

        // CR 302.6: Summoning sickness — delegate to the canonical query
        // (folds in Haste + non-creature short-circuits).
        if has_summoning_sickness(obj) {
            return Err(format!("{:?} has summoning sickness", id));
        }
    }

    Ok(())
}

/// Iterate every battlefield `StaticDefinition` whose mode is a block-restriction
/// (`CantBeBlocked`, `CantBeBlockedExceptBy`, or `CantBeBlockedBy`) AND whose
/// `affected` filter matches `attacker_id`. Yields `(source, def)` pairs so
/// callers can build a `FilterContext::from_source(state, source.id)` for the
/// inner blocker filters on the latter two modes.
///
/// CR 509.1b: A blocker declaration is illegal if the attacker is "affected by
/// any restrictions (effects that say a creature can't block, or that it can't
/// block unless some condition is met)." The static may live on any
/// battlefield permanent — most commonly the attacker itself (intrinsic
/// SelfRef), an Equipment attached to it (CR 301.5a + `FilterProp::EquippedBy`),
/// or an Aura attached to it (CR 303.4 + `FilterProp::EnchantedBy`) — so this
/// scan iterates the whole battlefield rather than only the attacker's own
/// `static_definitions`. CR 604.1 / CR 613.1 condition gating and the
/// CR 702.26b / CR 114.4 functioning gate are applied via
/// `battlefield_active_statics`.
fn block_restriction_statics_against<'a>(
    state: &'a GameState,
    attacker_id: ObjectId,
) -> impl Iterator<Item = (&'a GameObject, &'a StaticDefinition)> + 'a {
    super::functioning_abilities::battlefield_active_statics(state)
        .filter(|(_, def)| {
            matches!(
                def.mode,
                StaticMode::CantBeBlocked
                    | StaticMode::CantBeBlockedExceptBy { .. }
                    | StaticMode::CantBeBlockedBy { .. }
            )
        })
        .filter(move |(src, def)| match def.affected.as_ref() {
            // CR 604.1: a static with no `affected` filter is implicitly about
            // its own source (intrinsic SelfRef semantics — preserves the
            // pre-fix behavior of `active_static_definitions(attacker)` for
            // statics constructed without an explicit filter).
            None => src.id == attacker_id,
            Some(filter) => matches_target_filter(
                state,
                attacker_id,
                filter,
                &FilterContext::from_source(state, src.id),
            ),
        })
}

/// Validate blocker declarations per CR 509.1.
/// Each assignment is (blocker_id, attacker_id).
pub fn validate_blockers(
    state: &GameState,
    assignments: &[(ObjectId, ObjectId)],
) -> Result<(), String> {
    let defending_player = next_defending_player_to_declare_blockers(state)
        .unwrap_or_else(|| players::next_player(state, state.active_player));
    validate_blockers_for_player(state, defending_player, assignments)
}

/// Validate one defending player's blocker declaration per CR 509.1 and CR 802.4.
pub fn validate_blockers_for_player(
    state: &GameState,
    player: PlayerId,
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
        // CR 702.26b: Phased-out permanents are treated as though they don't
        // exist — they can't block.
        if blocker.is_phased_out() {
            return Err(format!("{:?} is phased out", blocker_id));
        }

        // CR 509.1a + CR 802.4a: Only untapped creatures controlled by this
        // defending player may block in this declaration.
        if blocker.controller != player {
            return Err(format!(
                "{:?} is not controlled by defending player {:?}",
                blocker_id, player
            ));
        }

        // CR 802.4a: In multiplayer, blocker must block a creature attacking
        // this player, a planeswalker they control, or a battle they protect.
        if let Some(combat) = &state.combat {
            if let Some(attacker_info) =
                combat.attackers.iter().find(|a| a.object_id == attacker_id)
            {
                if attacker_info.defending_player != player {
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
        if super::functioning_abilities::active_static_definitions(state, blocker).any(|sd| {
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

        // CR 509.1b + CR 301.5a + CR 303.4: scan every battlefield static whose
        // `affected` filter matches the attacker — covers intrinsic
        // (`SelfRef`), Equipment-granted (`EquippedBy`), and Aura-granted
        // (`EnchantedBy`) `CantBeBlocked*` modes uniformly. The static's own
        // source supplies the `FilterContext` so inner filters like "creatures
        // you control" resolve against the granting permanent's controller.
        for (src, sd) in block_restriction_statics_against(state, attacker_id) {
            match &sd.mode {
                StaticMode::CantBeBlocked => {
                    return Err(format!(
                        "{:?} cannot block {:?} (can't be blocked)",
                        blocker_id, attacker_id
                    ));
                }
                StaticMode::CantBeBlockedExceptBy { filter } => {
                    let (target_filter, _) = parse_target(filter);
                    if !matches_target_filter(
                        state,
                        blocker_id,
                        &target_filter,
                        &FilterContext::from_source(state, src.id),
                    ) {
                        return Err(format!(
                            "{:?} cannot block {:?} (can't be blocked except by {})",
                            blocker_id, attacker_id, filter
                        ));
                    }
                }
                StaticMode::CantBeBlockedBy { filter }
                    if matches_target_filter(
                        state,
                        blocker_id,
                        filter,
                        &FilterContext::from_source(state, src.id),
                    ) =>
                {
                    return Err(format!(
                        "{:?} cannot block {:?} (can't be blocked by {filter:?})",
                        blocker_id, attacker_id
                    ));
                }
                _ => {}
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
                // CR 702.16j: Protection from everything — blocked by no creature.
                Keyword::Protection(ProtectionTarget::Everything) => {
                    return Err(format!(
                        "{:?} cannot block {:?} (protection from everything)",
                        blocker_id, attacker_id
                    ));
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

        // CR 702.14c: Landwalk — attacker can't be blocked as long as the
        // defending player (blocker's controller per CR 509.1a) controls a land
        // of the specified type.
        if is_landwalk_unblockable(state, attacker, blocker.controller) {
            return Err(format!(
                "{:?} cannot block {:?} (landwalk: defending player controls a matching land)",
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
            let max_allowed = extra_block_limit(state, blocker);
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
            if attacker_info.defending_player != player {
                continue;
            }
            let attacker_id = attacker_info.object_id;
            let attacker = match state.objects.get(&attacker_id) {
                Some(obj) => obj,
                None => continue,
            };

            // Check if this attacker has MustBeBlocked.
            // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
            let has_must_be_blocked =
                super::functioning_abilities::active_static_definitions(state, attacker)
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
            let has_available_blocker = state.battlefield.iter().any(|id| {
                if assigned_blockers.contains(id) {
                    return false;
                }
                let Some(obj) = state.objects.get(id) else {
                    return false;
                };
                obj.controller == player
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

        // CR 509.1c + CR 802.4b: Check MustBlock only for this defending
        // player's declaration.
        // If a defending creature has MustBlock and isn't assigned as a blocker,
        // verify it couldn't legally block any attacker.
        for &obj_id in &state.battlefield {
            let Some(obj) = state.objects.get(&obj_id) else {
                continue;
            };
            if obj.controller != player {
                continue;
            }
            if !obj.card_types.core_types.contains(&CoreType::Creature) {
                continue;
            }
            // CR 509.1c: Check MustBlock — directly on this creature or from
            // a cross-permanent static (e.g., "All creatures block each combat if able").
            // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
            let has_must_block =
                super::functioning_abilities::active_static_definitions(state, obj)
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
            if super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
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

/// CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Walk every battlefield / command-zone
/// static ability that imposes `CantAttack`/`CantAttackOrBlock` or `CantBlock`/
/// `CantAttackOrBlock` with a `StaticCondition::UnlessPay` condition, compute the
/// per-creature cost that the taxed player owes for each declared attacker/blocker,
/// and aggregate the locked-in total.
///
/// `context` selects which side of combat we're computing for. For `Attacking` the
/// mode filter is `CantAttack | CantAttackOrBlock` and the candidates are attackers.
/// For `Blocking` the mode filter is `CantBlock | CantAttackOrBlock` and the
/// candidates are blockers.
///
/// Returns `None` when no UnlessPay statics apply (the declaration should proceed
/// without pausing). Returns `Some((total, per_creature))` otherwise — callers pause
/// with `WaitingFor::CombatTaxPayment`, and the per-creature breakdown drives the
/// decline branch (which removes taxed creatures from the declaration).
pub fn compute_combat_tax(
    state: &GameState,
    creature_ids: &[ObjectId],
    context: crate::types::game_state::CombatTaxContext,
) -> Option<(
    crate::types::mana::ManaCost,
    Vec<(ObjectId, crate::types::mana::ManaCost)>,
)> {
    use crate::types::ability::{StaticCondition, UnlessPayScaling};
    use crate::types::game_state::CombatTaxContext;
    use crate::types::mana::ManaCost;

    if creature_ids.is_empty() {
        return None;
    }

    // Pre-collect the affected creature count for scaling — used by
    // PerAffectedCreature (count of declared creatures this static touches) so
    // the arithmetic is order-independent.
    let mut per_creature: Vec<(ObjectId, ManaCost)> = creature_ids
        .iter()
        .map(|&id| (id, ManaCost::zero()))
        .collect();
    let mut any_tax = false;

    // CR 114.4: Emblems in the command zone contribute their statics too.
    let zones = state.battlefield.iter().chain(state.command_zone.iter());
    for &source_id in zones {
        let Some(source_obj) = state.objects.get(&source_id) else {
            continue;
        };
        if source_obj.zone == Zone::Command && !source_obj.is_emblem {
            continue;
        }
        // CR 702.26b: Phased-out permanents' statics don't function.
        if source_obj.is_phased_out() {
            continue;
        }

        // CR 118.12a: UnlessPay conditions are data-carrying — the combat tax
        // code specifically inspects them, so iterating with `iter_all` (no
        // condition gate) is intentional here. Phased-out / command-zone
        // gates are enforced by the outer `if obj.is_phased_out()` / command-
        // zone check above this loop.
        for def in source_obj.static_definitions.iter_all() {
            let mode_matches = match context {
                CombatTaxContext::Attacking => matches!(
                    def.mode,
                    StaticMode::CantAttack | StaticMode::CantAttackOrBlock
                ),
                CombatTaxContext::Blocking => matches!(
                    def.mode,
                    StaticMode::CantBlock | StaticMode::CantAttackOrBlock
                ),
            };
            if !mode_matches {
                continue;
            }
            let Some(StaticCondition::UnlessPay {
                cost: base_cost,
                scaling,
            }) = def.condition.as_ref()
            else {
                continue;
            };

            // For each declared creature, determine if this static's affected filter matches.
            let mut affected_ids: Vec<ObjectId> = Vec::new();
            let ctx = FilterContext::from_source(state, source_id);
            for &cid in creature_ids {
                let creature_matches = match &def.affected {
                    Some(filter) => matches_target_filter(state, cid, filter, &ctx),
                    // No affected filter — treat as "applies to all taxed creatures",
                    // matching the behavior of `check_static_ability` when `affected`
                    // is None.
                    None => true,
                };
                if creature_matches {
                    affected_ids.push(cid);
                }
            }
            if affected_ids.is_empty() {
                continue;
            }

            // Compute per-creature contribution for this static.
            let per_match_cost: ManaCost = match scaling {
                UnlessPayScaling::Flat => {
                    // CR 118.12a: Flat "pays {N}" — for taxes, distribute across all
                    // affected creatures so the decline branch can drop individuals
                    // cleanly. Brainwash has exactly one affected creature by
                    // construction (the enchanted creature), so the distribution
                    // collapses to a single per-creature cost.
                    base_cost.clone()
                }
                UnlessPayScaling::PerAffectedCreature => {
                    // CR 508.1h: "pays {N} for each of those creatures" — every affected
                    // creature contributes base_cost. Distributed as base_cost per
                    // affected id so the decline branch can drop individuals cleanly.
                    base_cost.clone()
                }
                UnlessPayScaling::PerQuantityRef { quantity } => {
                    // CR 202.3e: X-style dynamic cost resolved once for the whole
                    // static (no per-affected multiplier). The full scaled cost is
                    // attributed to the first affected creature so the decline branch
                    // drops all affected creatures together (they share one logical
                    // tax).
                    let n = crate::game::quantity::resolve_quantity(
                        state,
                        &crate::types::ability::QuantityExpr::Ref {
                            qty: quantity.clone(),
                        },
                        source_obj.controller,
                        source_id,
                    );
                    let total = base_cost.scaled(n.max(0) as u32);
                    if let Some(first) = affected_ids.first() {
                        if let Some((_, slot)) =
                            per_creature.iter_mut().find(|(cid, _)| cid == first)
                        {
                            *slot = slot.plus(&total);
                            any_tax = true;
                        }
                    }
                    continue;
                }
                UnlessPayScaling::PerAffectedAndQuantityRef { quantity } => {
                    // CR 508.1h + CR 202.3e: Sphere of Safety — "pays {X} for each of
                    // those creatures, where X is the number of enchantments you
                    // control". Resolve X once, multiply base_cost, then attribute to
                    // each affected creature.
                    let n = crate::game::quantity::resolve_quantity(
                        state,
                        &crate::types::ability::QuantityExpr::Ref {
                            qty: quantity.clone(),
                        },
                        source_obj.controller,
                        source_id,
                    );
                    base_cost.scaled(n.max(0) as u32)
                }
                UnlessPayScaling::PerAffectedWithRef { quantity } => {
                    // CR 118.12a + CR 202.3e: Nils, Discipline Enforcer — "pays {X},
                    // where X is the number of counters on that creature". The
                    // scaling quantity is resolved per-affected-creature with that
                    // creature as the target, so each attacker pays base_cost times
                    // its own counter count. Attribute the resolved cost directly
                    // to each affected creature and continue (skip the shared
                    // per_match_cost distribution below).
                    for aid in &affected_ids {
                        let n = crate::game::quantity::resolve_quantity_with_targets_slice(
                            state,
                            &crate::types::ability::QuantityExpr::Ref {
                                qty: quantity.clone(),
                            },
                            source_obj.controller,
                            source_id,
                            &[crate::types::ability::TargetRef::Object(*aid)],
                        );
                        // CR 107.1b + CR 202.3e: Concretize any `{X}` in base_cost by
                        // substituting the resolved per-attacker quantity. This yields
                        // a locked-in generic-mana amount; callers see a `mana_value()`
                        // equal to N (or N × X-shard-count), matching what the player
                        // actually owes at the decision point.
                        let mut cost = base_cost.clone();
                        cost.concretize_x(n.max(0) as u32);
                        if cost.mana_value() == 0 {
                            continue;
                        }
                        if let Some((_, slot)) = per_creature.iter_mut().find(|(cid, _)| cid == aid)
                        {
                            *slot = slot.plus(&cost);
                            any_tax = true;
                        }
                    }
                    continue;
                }
            };

            for aid in &affected_ids {
                if let Some((_, slot)) = per_creature.iter_mut().find(|(cid, _)| cid == aid) {
                    *slot = slot.plus(&per_match_cost);
                    any_tax = true;
                }
            }
        }
    }

    if !any_tax {
        return None;
    }

    // Drop creatures with no tax — keep per_creature as the subset that is actually taxed.
    per_creature.retain(|(_, cost)| cost.mana_value() > 0 || !matches!(cost, ManaCost::NoCost));
    if per_creature.is_empty() {
        return None;
    }
    let total = per_creature
        .iter()
        .fold(ManaCost::zero(), |acc, (_, c)| acc.plus(c));
    if total.mana_value() == 0 {
        return None;
    }
    Some((total, per_creature))
}

/// CR 508.1d + CR 508.1h: Specialization of `compute_combat_tax` for the attack step.
pub fn compute_attack_tax(
    state: &GameState,
    attacks: &[(ObjectId, AttackTarget)],
) -> Option<(
    crate::types::mana::ManaCost,
    Vec<(ObjectId, crate::types::mana::ManaCost)>,
)> {
    let ids: Vec<ObjectId> = attacks.iter().map(|(id, _)| *id).collect();
    compute_combat_tax(
        state,
        &ids,
        crate::types::game_state::CombatTaxContext::Attacking,
    )
}

/// CR 509.1c + CR 509.1d: Specialization of `compute_combat_tax` for the block step.
pub fn compute_block_tax(
    state: &GameState,
    assignments: &[(ObjectId, ObjectId)],
) -> Option<(
    crate::types::mana::ManaCost,
    Vec<(ObjectId, crate::types::mana::ManaCost)>,
)> {
    let ids: Vec<ObjectId> = assignments.iter().map(|(b, _)| *b).collect();
    compute_combat_tax(
        state,
        &ids,
        crate::types::game_state::CombatTaxContext::Blocking,
    )
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
        let has_must_attack = super::functioning_abilities::active_static_definitions(state, obj)
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
            let can_attack_with_defender =
                super::functioning_abilities::active_static_definitions(state, obj)
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
        if has_summoning_sickness(obj) {
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
                // CR 310.8b: A battle's protector can never attack it. Notably a
                // Siege's controller CAN attack it if they are not the protector.
                if battle.protector() == Some(state.active_player) {
                    return Err(format!("Protector cannot attack battle {:?}", battle_id));
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
            // CR 508.5 + CR 310.8d: Defending player for a battle = its protector,
            // not its controller. For planeswalkers, defending player = controller.
            let defending_player = match target {
                AttackTarget::Player(pid) => *pid,
                AttackTarget::Planeswalker(pw_id) => state
                    .objects
                    .get(pw_id)
                    .map(|pw| pw.controller)
                    .unwrap_or(PlayerId(0)),
                AttackTarget::Battle(battle_id) => state
                    .objects
                    .get(battle_id)
                    .and_then(|b| b.protector())
                    .unwrap_or(PlayerId(0)),
            };
            AttackerInfo::new(*object_id, *target, defending_player)
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
    let defending_player = next_defending_player_to_declare_blockers(state)
        .unwrap_or_else(|| players::next_player(state, state.active_player));
    declare_blockers_for_player(state, defending_player, assignments, events)
}

/// Declare one defending player's blockers.
pub fn declare_blockers_for_player(
    state: &mut GameState,
    player: PlayerId,
    assignments: &[(ObjectId, ObjectId)],
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    validate_blockers_for_player(state, player, assignments)?;

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
    if !combat.blockers_declared_by.contains(&player) {
        combat.blockers_declared_by.push(player);
    }

    // CR 509.1a: Record blocker object IDs for per-turn tracking.
    state
        .creatures_blocked_this_turn
        .extend(assignments.iter().map(|(blocker_id, _)| *blocker_id));

    let event = GameEvent::BlockersDeclared {
        assignments: assignments.to_vec(),
    };
    combat
        .pending_blocker_declaration_events
        .push(event.clone());
    events.push(event);

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

/// CR 302.6: Returns true iff this creature can't attack or pay `{T}`/`{Q}`
/// costs due to summoning sickness — i.e., it has NOT been continuously under
/// its controller's control since that player's most recent turn began.
///
/// Implementation reads the persistent `GameObject::summoning_sick` flag,
/// which is set true on ETB (`reset_for_battlefield_entry` +
/// `create_object_in_zone`) and cleared to false at the start of
/// controller's next turn by `turns::start_next_turn`. Haste is folded in
/// here at query time so dynamically-granted haste (e.g., "creatures you
/// control gain haste" statics) takes effect without mutating the flag.
pub fn has_summoning_sickness(obj: &GameObject) -> bool {
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        return false;
    }
    if obj.has_keyword(&Keyword::Haste) {
        return false;
    }
    obj.summoning_sick
}

/// CR 508.1a / CR 302.6: Untapped creature controlled since turn started, without Defender.
/// CR 702.26b: Phased-out creatures can't attack.
pub fn get_valid_attacker_ids(state: &GameState) -> Vec<ObjectId> {
    let active = state.active_player;
    let turn = state.turn_number;

    state
        .battlefield_phased_in_ids()
        .iter()
        .filter_map(|id| {
            let obj = state.objects.get(id)?;
            if obj.controller == active
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !obj.tapped
                && (!obj.has_keyword(&Keyword::Defender)
                    || super::functioning_abilities::active_static_definitions(state, obj)
                        .any(|sd| sd.mode == StaticMode::CanAttackWithDefender)
                    || crate::game::static_abilities::check_static_ability(
                        state,
                        StaticMode::CanAttackWithDefender,
                        &crate::game::static_abilities::StaticCheckContext {
                            target_id: Some(*id),
                            ..Default::default()
                        },
                    ))
                && !super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
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

/// CR 702.14c: A creature with landwalk can't be blocked as long as the defending
/// player controls a land with the matching type/supertype. The `Keyword::Landwalk`
/// variant's inner string is the qualifier: basic land subtypes ("Plains", "Island",
/// "Swamp", "Mountain", "Forest"), other subtypes ("Desert"), or supertype qualifiers
/// ("Legendary", "Snow", "Nonbasic").
///
/// Returns `true` if the attacker is unblockable by the defending player due to
/// some form of landwalk.
pub fn is_landwalk_unblockable(
    state: &GameState,
    attacker: &GameObject,
    defending_player: PlayerId,
) -> bool {
    // Collect all landwalk qualifiers the attacker has. Multiple instances of the
    // same landwalk are redundant (CR 702.14e) but multiple *kinds* can co-exist
    // (e.g., "plainswalk and islandwalk") — any match makes the attacker unblockable.
    let qualifiers: Vec<&str> = attacker
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Landwalk(q) => Some(q.as_str()),
            _ => None,
        })
        .collect();
    if qualifiers.is_empty() {
        return false;
    }

    // CR 702.14c: Check every land the defending player controls on the battlefield.
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if obj.controller != defending_player {
            continue;
        }
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            continue;
        }
        // CR 702.26b: Phased-out permanents don't exist for this check.
        if obj.is_phased_out() {
            continue;
        }
        for qualifier in &qualifiers {
            if land_matches_landwalk_qualifier(obj, qualifier) {
                return true;
            }
        }
    }
    false
}

/// CR 702.14a: Match a land against a landwalk qualifier.
/// Basic/non-basic land subtypes match via `subtypes`; "Legendary"/"Snow" match via
/// supertypes; "Nonbasic" matches any land lacking the Basic supertype.
fn land_matches_landwalk_qualifier(land: &GameObject, qualifier: &str) -> bool {
    match qualifier {
        "Legendary" => land.card_types.supertypes.contains(&Supertype::Legendary),
        "Snow" => land.card_types.supertypes.contains(&Supertype::Snow),
        "Nonbasic" => !land.card_types.supertypes.contains(&Supertype::Basic),
        subtype => land.card_types.subtypes.iter().any(|s| s == subtype),
    }
}

/// Check per-pair blocking legality (evasion abilities, CR 509.1b).
/// Does NOT check menace (which is a multi-blocker constraint).
/// CR 509.1a–b: Check if a specific blocker can legally block a specific attacker,
/// accounting for all blocking restrictions (CantBeBlocked, CantBeBlockedExceptBy,
/// CantBeBlockedBy, Protection, Flying/Reach, Shadow, Fear, Intimidate, Skulk,
/// Horsemanship, Landwalk, CantBlock/CantAttackOrBlock).
pub fn can_block_pair(state: &GameState, blocker_id: ObjectId, attacker_id: ObjectId) -> bool {
    let Some(blocker) = state.objects.get(&blocker_id) else {
        return false;
    };
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return false;
    };
    if super::functioning_abilities::active_static_definitions(state, blocker).any(|sd| {
        matches!(
            sd.mode,
            StaticMode::CantBlock | StaticMode::CantAttackOrBlock
        )
    }) {
        return false;
    }
    // CR 509.1b + CR 301.5a + CR 303.4: scan every battlefield static whose
    // `affected` filter matches the attacker — covers intrinsic, Equipment-
    // granted, and Aura-granted `CantBeBlocked*` uniformly. Mirrors the
    // declare-blockers validation in `validate_blockers_for_player`.
    for (src, sd) in block_restriction_statics_against(state, attacker_id) {
        match &sd.mode {
            StaticMode::CantBeBlocked => return false,
            StaticMode::CantBeBlockedExceptBy { filter } => {
                let (target_filter, _) = parse_target(filter);
                if !matches_target_filter(
                    state,
                    blocker_id,
                    &target_filter,
                    &FilterContext::from_source(state, src.id),
                ) {
                    return false;
                }
            }
            StaticMode::CantBeBlockedBy { filter }
                if matches_target_filter(
                    state,
                    blocker_id,
                    filter,
                    &FilterContext::from_source(state, src.id),
                ) =>
            {
                return false;
            }
            _ => {}
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
            // CR 702.16j: Protection from everything — blocked by no creature.
            Keyword::Protection(ProtectionTarget::Everything) => {
                return false;
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
    // CR 702.14c: Landwalk — unblockable as long as defending player (blocker's
    // controller per CR 509.1a) controls a land of the matching type.
    if is_landwalk_unblockable(state, attacker, blocker.controller) {
        return false;
    }
    true
}

/// CR 509.1a + CR 509.1b: Compute the maximum number of attackers a creature can block.
/// Default is 1. ExtraBlockers { count: Some(n) } adds n (so 1+n). count: None = unlimited (u32::MAX).
/// Multiple ExtraBlockers stack: the best (highest) limit wins.
fn extra_block_limit(state: &GameState, blocker: &GameObject) -> u32 {
    let mut max: u32 = 1;
    // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
    for sd in super::functioning_abilities::active_static_definitions(state, blocker) {
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

/// For one defending player, compute which of their blockers can legally block which attackers.
pub fn get_valid_block_targets_for_player(
    state: &GameState,
    player: PlayerId,
) -> HashMap<ObjectId, Vec<ObjectId>> {
    get_valid_block_targets(state)
        .into_iter()
        .filter(|(blocker_id, _)| {
            state
                .objects
                .get(blocker_id)
                .is_some_and(|blocker| blocker.controller == player)
        })
        .collect()
}

/// Return players who are actually defending this combat, in APNAP order.
pub fn defending_players_in_turn_order(state: &GameState) -> Vec<PlayerId> {
    let defending_players: HashSet<PlayerId> = state
        .combat
        .as_ref()
        .map(|combat| {
            combat
                .attackers
                .iter()
                .map(|a| a.defending_player)
                .collect()
        })
        .unwrap_or_default();

    players::apnap_order(state)
        .into_iter()
        .filter(|player| *player != state.active_player && defending_players.contains(player))
        .collect()
}

/// Return the first player who is actually defending this combat, in APNAP order.
pub fn first_defending_player_in_turn_order(state: &GameState) -> Option<PlayerId> {
    defending_players_in_turn_order(state).into_iter().next()
}

/// Return the next defending player who still needs to declare blockers.
pub fn next_defending_player_to_declare_blockers(state: &GameState) -> Option<PlayerId> {
    let declared: HashSet<PlayerId> = state
        .combat
        .as_ref()?
        .blockers_declared_by
        .iter()
        .copied()
        .collect();

    defending_players_in_turn_order(state)
        .into_iter()
        .find(|player| !declared.contains(player))
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

    // CR 702.26b: Phased-out creatures can't block.
    state
        .battlefield_phased_in_ids()
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
///
/// Player-phasing exclusion: a phased-out player can't be attacked, and neither
/// can their planeswalkers nor any battles they protect — they're all treated
/// as though they don't exist for combat purposes (mirrors CR 702.26b for
/// permanents, applied to players via card Oracle text).
pub fn get_valid_attack_targets(state: &GameState) -> Vec<AttackTarget> {
    let active = state.active_player;
    let allies = players::teammates(state, active);
    let phased_out = |pid: PlayerId| -> bool {
        state
            .players
            .iter()
            .find(|p| p.id == pid)
            .is_some_and(|p| p.is_phased_out())
    };
    let mut targets = Vec::new();

    // CR 508.1b + CR 702.16j: A player with protection from everything can't
    // be declared as the player each attacking creature is attacking — the
    // attack declaration would fail because the protected player is not a
    // legal attack target.
    let protected = |pid: PlayerId| -> bool {
        super::static_abilities::player_has_protection_from_everything(state, pid)
    };

    // All non-eliminated, phased-in opponents (excluding teammates)
    for player in &state.players {
        if player.id != active
            && !state.eliminated_players.contains(&player.id)
            && !allies.contains(&player.id)
            && player.is_phased_in()
            && !protected(player.id)
        {
            targets.push(AttackTarget::Player(player.id));
        }
    }

    // All planeswalkers controlled by opponents (excluding teammates' and
    // controllers that are phased out)
    for &id in &state.battlefield {
        if let Some(obj) = state.objects.get(&id) {
            if obj.controller != active
                && !allies.contains(&obj.controller)
                && obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Planeswalker)
                && !state.eliminated_players.contains(&obj.controller)
                && !phased_out(obj.controller)
            {
                targets.push(AttackTarget::Planeswalker(id));
            }
        }
    }

    // CR 310.8b + CR 506.2: A battle can be attacked by any attacking player for whom
    // its protector is a defending player. Notably a Siege can be attacked by its own
    // controller if the protector is a different player (CR 310.8b "Siege battle can
    // be attacked by its own controller"). The only player who cannot attack is the
    // battle's protector.
    for &id in &state.battlefield {
        if let Some(obj) = state.objects.get(&id) {
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Battle)
            {
                continue;
            }
            let Some(protector) = obj.protector() else {
                continue;
            };
            if protector == active || allies.contains(&protector) {
                continue;
            }
            if state.eliminated_players.contains(&protector) {
                continue;
            }
            // If the protector is phased out, the battle itself can't be
            // attacked (the protector "doesn't exist" for combat routing).
            if phased_out(protector) {
                continue;
            }
            targets.push(AttackTarget::Battle(id));
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
                        || super::functioning_abilities::active_static_definitions(state, obj)
                            .any(|sd| sd.mode == StaticMode::CanAttackWithDefender)
                        || crate::game::static_abilities::check_static_ability(
                            state,
                            StaticMode::CanAttackWithDefender,
                            &crate::game::static_abilities::StaticCheckContext {
                                target_id: Some(*id),
                                ..Default::default()
                            },
                        ))
                    && !super::functioning_abilities::active_static_definitions(state, obj).any(
                        |sd| {
                            matches!(
                                sd.mode,
                                StaticMode::CantAttack | StaticMode::CantAttackOrBlock
                            )
                        },
                    )
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

    /// CR 702.3b + CR 122.1: Demon Wall — "as long as this creature has a
    /// counter on it, it can attack as though it didn't have defender".
    /// Exercises the `CanAttackWithDefender` static gated on
    /// `StaticCondition::HasCounters { counters: Any, minimum: 1 }`.
    /// With zero counters the condition is false and Defender still applies;
    /// with a +1/+1 counter the condition holds and the attack is legal.
    #[test]
    fn demon_wall_attacks_only_with_counters_on_it() {
        use crate::types::ability::{StaticCondition, StaticDefinition, TargetFilter};
        use crate::types::counter::{CounterMatch, CounterType};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Demon Wall", 3, 3);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.keywords.push(Keyword::Defender);
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CanAttackWithDefender)
                    .affected(TargetFilter::SelfRef)
                    .condition(StaticCondition::HasCounters {
                        counters: CounterMatch::Any,
                        minimum: 1,
                        maximum: None,
                    })
                    .description(
                        "As long as ~ has a counter on it, it can attack as though it \
                         didn't have defender."
                            .to_string(),
                    ),
            );
        }

        // No counters yet — Defender blocks the attack.
        assert!(
            validate_attackers(&state, &[id]).is_err(),
            "Demon Wall with 0 counters must not attack (Defender)"
        );

        // Add a +1/+1 counter — the condition becomes true and the grant applies.
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        assert!(
            validate_attackers(&state, &[id]).is_ok(),
            "Demon Wall with a counter must be able to attack"
        );

        // Generic counter type should also satisfy CounterMatch::Any.
        state.objects.get_mut(&id).unwrap().counters.clear();
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Generic("page".to_string()), 1);
        assert!(
            validate_attackers(&state, &[id]).is_ok(),
            "CounterMatch::Any must accept any counter type"
        );
    }

    #[test]
    fn summoning_sick_creature_cannot_attack() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.entered_battlefield_turn = Some(2);
        obj.summoning_sick = true;
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

    /// Helper for landwalk tests — create a land with the given subtypes/supertypes
    /// on the battlefield controlled by `owner`.
    fn create_land(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        subtypes: &[&str],
        supertypes: &[crate::types::card_type::Supertype],
    ) -> ObjectId {
        let id = create_object(
            state,
            crate::types::identifiers::CardId(state.next_object_id),
            owner,
            name.to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        for st in subtypes {
            obj.card_types.subtypes.push((*st).to_string());
        }
        for sp in supertypes {
            obj.card_types.supertypes.push(*sp);
        }
        id
    }

    /// CR 702.14c: Plainswalk makes an attacker unblockable when defender controls a Plains.
    #[test]
    fn plainswalk_unblockable_when_defender_controls_plains() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Plainswalker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Plains".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _plains = create_land(&mut state, PlayerId(1), "Plains", &["Plains"], &[]);

        assert!(!can_block_pair(&state, blocker, attacker));
        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
    }

    /// CR 702.14c: Plainswalk does nothing when defender controls no Plains.
    #[test]
    fn plainswalk_blockable_when_defender_has_no_plains() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Plainswalker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Plains".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        assert!(can_block_pair(&state, blocker, attacker));
        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    /// CR 702.14c: Landwalk only cares about land type it specifies — islandwalk
    /// is not evaded by the defender controlling a Plains.
    #[test]
    fn islandwalk_unaffected_by_plains() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Islandwalker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Island".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _plains = create_land(&mut state, PlayerId(1), "Plains", &["Plains"], &[]);

        assert!(can_block_pair(&state, blocker, attacker));
    }

    /// CR 702.14d: Landwalk only considers defending player's lands — if the
    /// attacker's controller has a Plains, plainswalk does nothing.
    #[test]
    fn plainswalk_ignores_attackers_lands() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Plainswalker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Plains".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        // Attacker's owner controls the Plains, not defender.
        let _plains = create_land(&mut state, PlayerId(0), "Plains", &["Plains"], &[]);

        assert!(can_block_pair(&state, blocker, attacker));
    }

    /// CR 702.14a: Multiple landwalk kinds — any matching type makes attacker unblockable.
    #[test]
    fn multiple_landwalk_any_match_unblockable() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Dual Walker", 2, 2);
        let kws = &mut state.objects.get_mut(&attacker).unwrap().keywords;
        kws.push(Keyword::Landwalk("Plains".to_string()));
        kws.push(Keyword::Landwalk("Island".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _island = create_land(&mut state, PlayerId(1), "Island", &["Island"], &[]);

        assert!(!can_block_pair(&state, blocker, attacker));
    }

    /// CR 702.14a: Legendary landwalk — defender controlling a legendary land makes
    /// attacker unblockable regardless of subtype.
    #[test]
    fn legendary_landwalk_matches_legendary_land() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Legend Walker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Legendary".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _karakas = create_land(
            &mut state,
            PlayerId(1),
            "Karakas",
            &["Plains"],
            &[Supertype::Legendary],
        );

        assert!(!can_block_pair(&state, blocker, attacker));
    }

    /// CR 702.14a: Nonbasic landwalk — defender controlling any nonbasic land
    /// (no Basic supertype) makes the attacker unblockable.
    #[test]
    fn nonbasic_landwalk_matches_nonbasic_land() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Nonbasic Walker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Nonbasic".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        // Nonbasic land (no Basic supertype).
        let _underground_sea = create_land(
            &mut state,
            PlayerId(1),
            "Underground Sea",
            &["Island", "Swamp"],
            &[],
        );

        assert!(!can_block_pair(&state, blocker, attacker));
    }

    /// CR 702.14a: Nonbasic landwalk does nothing if defender only controls basic lands.
    #[test]
    fn nonbasic_landwalk_blockable_when_only_basics() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Nonbasic Walker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Nonbasic".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _plains = create_land(
            &mut state,
            PlayerId(1),
            "Plains",
            &["Plains"],
            &[Supertype::Basic],
        );

        assert!(can_block_pair(&state, blocker, attacker));
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

    /// CR 509.1b + CR 301.5a: An Equipment-owned `CantBeBlocked` static must
    /// propagate to the equipped creature. Mirrors Silver Shroud Costume,
    /// Whispersilk Cloak, Trailblazer's Boots, etc.
    #[test]
    fn equipment_granted_cant_be_blocked_propagates_to_attacker() {
        use crate::types::ability::{FilterProp, StaticDefinition, TargetFilter, TypedFilter};

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Equipped Creature", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        // Silver Shroud Costume: "Equipped creature can't be blocked."
        let costume = create_creature(&mut state, PlayerId(0), "Silver Shroud Costume", 0, 0);
        let costume_obj = state.objects.get_mut(&costume).unwrap();
        costume_obj.attached_to = Some(attacker.into());
        costume_obj.static_definitions.push(
            StaticDefinition::new(StaticMode::CantBeBlocked).affected(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
            )),
        );

        // Block declaration should be rejected via global static scan.
        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
        // Symbolic per-pair check must agree.
        assert!(!can_block_pair(&state, blocker, attacker));
    }

    /// CR 509.1b: Detaching the Equipment must restore blockability — the
    /// global scan's `affected` filter no longer matches the attacker.
    #[test]
    fn unattached_equipment_does_not_grant_unblockable() {
        use crate::types::ability::{FilterProp, StaticDefinition, TargetFilter, TypedFilter};

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        // Costume exists but is NOT attached to the attacker (or anything).
        let costume = create_creature(&mut state, PlayerId(0), "Silver Shroud Costume", 0, 0);
        state
            .objects
            .get_mut(&costume)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantBeBlocked).affected(TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
                )),
            );

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
        assert!(can_block_pair(&state, blocker, attacker));
    }

    /// CR 509.1b: A `CantBeBlocked` static on a different battlefield object
    /// (intrinsic SelfRef) must NOT bleed across to unrelated attackers. The
    /// global scan still anchors `affected` to the static's own source.
    #[test]
    fn intrinsic_cant_be_blocked_on_other_creature_does_not_propagate() {
        use crate::types::ability::{StaticDefinition, TargetFilter};

        let mut state = setup();
        // Two attackers; one has the intrinsic static, the other is plain.
        let stalker = create_creature(&mut state, PlayerId(0), "Invisible Stalker", 1, 1);
        state
            .objects
            .get_mut(&stalker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlocked).affected(TargetFilter::SelfRef));
        let bear = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        // Stalker is unblockable.
        assert!(validate_blockers(&state, &[(blocker, stalker)]).is_err());
        // Plain Bear remains blockable — Stalker's static does not bleed across.
        assert!(validate_blockers(&state, &[(blocker, bear)]).is_ok());
        assert!(can_block_pair(&state, blocker, bear));
    }

    /// CR 509.1b: Two Equipments granting the same `CantBeBlocked` to the
    /// attacker must still produce a single rejection — no double-count or
    /// iterator overflow.
    #[test]
    fn multiple_equipments_granting_unblockable_block_correctly() {
        use crate::types::ability::{FilterProp, StaticDefinition, TargetFilter, TypedFilter};

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Doubly Equipped", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let make_equipment = |state: &mut GameState, name: &str| -> ObjectId {
            let id = create_creature(state, PlayerId(0), name, 0, 0);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.attached_to = Some(attacker.into());
            obj.static_definitions
                .push(StaticDefinition::new(StaticMode::CantBeBlocked).affected(
                    TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
                    ),
                ));
            id
        };
        let _e1 = make_equipment(&mut state, "Whispersilk Cloak");
        let _e2 = make_equipment(&mut state, "Silver Shroud Costume");

        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
        assert!(!can_block_pair(&state, blocker, attacker));
    }

    /// CR 509.1b + CR 303.4: An Aura-granted `CantBeBlockedBy` (e.g., Snake
    /// Cult Initiation, Pemmin's Aura-class) must propagate to the enchanted
    /// creature, with the inner blocker filter resolved against the Aura's
    /// own controller (CR 109.4).
    #[test]
    fn aura_granted_cant_be_blocked_by_propagates_to_enchanted() {
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::card_type::CoreType;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Enchanted Creature", 2, 2);
        let ground_blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let flying_blocker = create_creature(&mut state, PlayerId(1), "Bird", 1, 1);
        state
            .objects
            .get_mut(&flying_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        // Aura with "enchanted creature can't be blocked by creatures without flying"
        // — modelled by `CantBeBlockedBy { filter: creatures-without-flying }`.
        let aura = create_creature(&mut state, PlayerId(0), "Aqueous Form", 0, 0);
        let aura_obj = state.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types.push(CoreType::Enchantment);
        aura_obj.attached_to = Some(attacker.into());
        // Inner filter: creature without flying (negative keyword filter is
        // hard to express crisply in tests; use a controller restriction
        // instead — "can't be blocked by creatures the active player
        // controls" is structurally identical for the global-scan check).
        let blocker_filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
        aura_obj.static_definitions.push(
            StaticDefinition::new(StaticMode::CantBeBlockedBy {
                filter: blocker_filter,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::creature()
                    .properties(vec![crate::types::ability::FilterProp::EnchantedBy]),
            )),
        );

        // Both blockers controlled by PlayerId(1) — opponent of the Aura's
        // controller (PlayerId(0)) — so both match `Opponent` from the Aura's
        // perspective and are prohibited. (Pre-fix: this scan never ran
        // because the static lives on the Aura, not the attacker.)
        assert!(validate_blockers(&state, &[(ground_blocker, attacker)]).is_err());
        assert!(!can_block_pair(&state, ground_blocker, attacker));
        assert!(validate_blockers(&state, &[(flying_blocker, attacker)]).is_err());
        assert!(!can_block_pair(&state, flying_blocker, attacker));
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
        let obj = state.objects.get_mut(&must_attacker).unwrap();
        obj.entered_battlefield_turn = Some(state.turn_number);
        obj.summoning_sick = true;
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
        let obj = state.objects.get_mut(&goaded).unwrap();
        obj.entered_battlefield_turn = Some(state.turn_number);
        obj.summoning_sick = true;
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

    // --- Combat tax computation (CR 508.1d + 508.1h + 509.1c + 509.1d) ---

    fn create_ghostly_prison(state: &mut GameState, controller: PlayerId) -> ObjectId {
        use crate::types::ability::{
            ControllerRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
            TypedFilter, UnlessPayScaling,
        };
        use crate::types::mana::ManaCost;
        use crate::types::statics::StaticMode;

        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Ghostly Prison".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        let mut def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }))
            .description("Ghostly Prison".to_string());
        def.condition = Some(StaticCondition::UnlessPay {
            cost: ManaCost::generic(2),
            scaling: UnlessPayScaling::PerAffectedCreature,
        });
        obj.static_definitions.push(def);
        id
    }

    #[test]
    fn compute_attack_tax_aggregates_per_attacker_with_ghostly_prison() {
        let mut state = setup();
        // Defender (PlayerId(1)) controls Ghostly Prison.
        let _prison = create_ghostly_prison(&mut state, PlayerId(1));
        // Active player declares two attackers.
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        let a2 = create_creature(&mut state, PlayerId(0), "A2", 2, 2);
        let attacks = vec![
            (a1, AttackTarget::Player(PlayerId(1))),
            (a2, AttackTarget::Player(PlayerId(1))),
        ];
        let (total, per_creature) = compute_attack_tax(&state, &attacks).expect("tax applies");
        // Two attackers × {2} each = {4} total.
        assert_eq!(total.mana_value(), 4);
        assert_eq!(per_creature.len(), 2);
        assert!(per_creature.iter().all(|(_, c)| c.mana_value() == 2));
    }

    /// CR 118.12a + CR 202.3e: Nils, Discipline Enforcer — per-attacker counter-scaled tax.
    /// Builds a Nils-style static (`PerAffectedWithRef` + `AnyCountersOnTarget`) on defender,
    /// gives two attackers different counter counts, and verifies each pays its own counter
    /// count in mana. Uncountered creatures are excluded from the tax (filter guard).
    #[test]
    fn compute_attack_tax_nils_per_attacker_counter_scaling() {
        use crate::types::ability::{
            FilterProp, QuantityRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
            TypedFilter, UnlessPayScaling,
        };
        use crate::types::counter::CounterType;
        use crate::types::mana::ManaCost;
        use crate::types::statics::StaticMode;

        let mut state = setup();

        // Defender (PlayerId(1)) controls Nils — counter-gated attack tax.
        let next_card_id = CardId(state.next_object_id);
        let nils = create_object(
            &mut state,
            next_card_id,
            PlayerId(1),
            "Nils, Discipline Enforcer".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let nils_obj = state.objects.get_mut(&nils).unwrap();
        nils_obj.card_types.core_types.push(CoreType::Creature);
        let mut def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![FilterProp::HasAnyCounter],
            }))
            .description("Nils static".to_string());
        def.condition = Some(StaticCondition::UnlessPay {
            // CR 202.3e: "{X}" base cost — resolved per-attacker via scaling.
            cost: ManaCost::Cost {
                shards: vec![crate::types::mana::ManaCostShard::X],
                generic: 0,
            },
            scaling: UnlessPayScaling::PerAffectedWithRef {
                quantity: QuantityRef::AnyCountersOnTarget,
            },
        });
        nils_obj.static_definitions.push(def);

        // Active player: three creatures — two carrying counters, one bare.
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        let a2 = create_creature(&mut state, PlayerId(0), "A2", 2, 2);
        let a3 = create_creature(&mut state, PlayerId(0), "A3 (no counters)", 2, 2);
        state
            .objects
            .get_mut(&a1)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 3);
        state
            .objects
            .get_mut(&a2)
            .unwrap()
            .counters
            .insert(CounterType::Generic("oil".to_string()), 2);

        let attacks = vec![
            (a1, AttackTarget::Player(PlayerId(1))),
            (a2, AttackTarget::Player(PlayerId(1))),
            (a3, AttackTarget::Player(PlayerId(1))),
        ];
        let (total, per_creature) = compute_attack_tax(&state, &attacks).expect("Nils tax applies");
        // a1 pays {3} (three +1/+1 counters), a2 pays {2} (two oil counters),
        // a3 pays {0} (no counters — filter excludes it). Total = {5}.
        assert_eq!(total.mana_value(), 5, "total Nils tax should be {{5}}");
        let a1_cost = per_creature
            .iter()
            .find(|(id, _)| *id == a1)
            .map(|(_, c)| c.mana_value());
        let a2_cost = per_creature
            .iter()
            .find(|(id, _)| *id == a2)
            .map(|(_, c)| c.mana_value());
        let a3_cost = per_creature
            .iter()
            .find(|(id, _)| *id == a3)
            .map(|(_, c)| c.mana_value())
            .unwrap_or(0);
        assert_eq!(a1_cost, Some(3), "three +1/+1 counters → {{3}}");
        assert_eq!(a2_cost, Some(2), "two oil counters → {{2}}");
        assert_eq!(a3_cost, 0, "no counters → no tax");
    }

    #[test]
    fn compute_attack_tax_stacks_two_prisons() {
        let mut state = setup();
        let _p1 = create_ghostly_prison(&mut state, PlayerId(1));
        let _p2 = create_ghostly_prison(&mut state, PlayerId(1));
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        let attacks = vec![(a1, AttackTarget::Player(PlayerId(1)))];
        let (total, per_creature) = compute_attack_tax(&state, &attacks).expect("tax applies");
        // One attacker × {2} × 2 prisons = {4}.
        assert_eq!(total.mana_value(), 4);
        assert_eq!(per_creature.len(), 1);
        assert_eq!(per_creature[0].1.mana_value(), 4);
    }

    #[test]
    fn compute_attack_tax_returns_none_when_no_static_applies() {
        let mut state = setup();
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        let attacks = vec![(a1, AttackTarget::Player(PlayerId(1)))];
        assert!(compute_attack_tax(&state, &attacks).is_none());
    }

    #[test]
    fn compute_attack_tax_skips_own_creatures() {
        let mut state = setup();
        // Active player controls their own prison (hypothetical) — their own
        // creatures shouldn't be filtered since `ControllerRef::Opponent` is
        // relative to the static's controller (the active player).
        let _prison = create_ghostly_prison(&mut state, PlayerId(0));
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        let attacks = vec![(a1, AttackTarget::Player(PlayerId(1)))];
        // The static's controller (PlayerId(0)) is the attacker's controller;
        // their creature is NOT an opponent's creature → filter doesn't match.
        assert!(compute_attack_tax(&state, &attacks).is_none());
    }

    /// CR 508.1d + CR 702.36 + CR 117.5: Norn's Annex — Phyrexian-cost combat tax.
    /// Regression for L9-52: the AST is structurally identical to Ghostly Prison
    /// except the cost contains `{W/P}` shards rather than generic mana. The tax
    /// must compute and the per-creature cost must report `mana_value() == 1` per
    /// `{W/P}` shard (CR 202.3g).
    #[test]
    fn compute_attack_tax_norns_annex_phyrexian_cost() {
        use crate::types::ability::{
            ControllerRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
            TypedFilter, UnlessPayScaling,
        };
        use crate::types::mana::{ManaCost, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        // Defender (PlayerId(1)) controls Norn's Annex.
        let next_card_id = CardId(state.next_object_id);
        let annex = create_object(
            &mut state,
            next_card_id,
            PlayerId(1),
            "Norn's Annex".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let annex_obj = state.objects.get_mut(&annex).unwrap();
        annex_obj.card_types.core_types.push(CoreType::Artifact);
        let mut def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }))
            .description("Norn's Annex".to_string());
        def.condition = Some(StaticCondition::UnlessPay {
            // CR 202.3g: {W/P} — Phyrexian white shard.
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::PhyrexianWhite],
                generic: 0,
            },
            scaling: UnlessPayScaling::PerAffectedCreature,
        });
        annex_obj.static_definitions.push(def);

        // Active player declares two attackers.
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        let a2 = create_creature(&mut state, PlayerId(0), "A2", 2, 2);
        let attacks = vec![
            (a1, AttackTarget::Player(PlayerId(1))),
            (a2, AttackTarget::Player(PlayerId(1))),
        ];
        let (total, per_creature) =
            compute_attack_tax(&state, &attacks).expect("Norn's Annex tax applies");
        // CR 202.3g: each {W/P} contributes mana_value 1; two attackers ⇒ total 2.
        assert_eq!(total.mana_value(), 2, "two attackers × {{W/P}} ⇒ total 2");
        assert_eq!(per_creature.len(), 2);
        assert!(
            per_creature.iter().all(|(_, c)| c.mana_value() == 1),
            "each attacker pays one {{W/P}}"
        );
    }

    /// CR 508.1b + CR 702.16j: A player with protection from everything is
    /// not a legal attack target. `get_valid_attack_targets` must exclude
    /// them from the list opposing creatures can declare as their attack
    /// target.
    #[test]
    fn get_valid_attack_targets_excludes_protected_player() {
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter};
        use crate::types::keywords::{Keyword, ProtectionTarget};

        let mut state = setup();
        // Source — a battlefield object to hang the transient effect off.
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Teferi's Protection source".to_string(),
            Zone::Battlefield,
        );
        state.add_transient_continuous_effect(
            source,
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );

        // Active player is PlayerId(0) (default for new_two_player).
        let targets = get_valid_attack_targets(&state);
        assert!(
            !targets
                .iter()
                .any(|t| matches!(t, AttackTarget::Player(id) if *id == PlayerId(1))),
            "protected PlayerId(1) must not be a valid attack target, got {:?}",
            targets
        );
    }
}
