use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::game_object::GameObject;
use super::players;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::types::ability::{StaticDefinition, TargetRef};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::statics::{
    AttackDefenderScope, BlockExceptionKind, CombatAloneAction, CombatAloneRequirement, StaticMode,
};
use crate::types::zones::Zone;

/// CR 604.1: loop-invariant presence facts for the combat-restriction statics.
///
/// Combat legality loops iterate N battlefield permanents and, per permanent,
/// call `check_static_ability` — itself an O(N) `game_functioning_statics`
/// sweep — making each loop O(N^2). `compute` does ONE sweep up front and
/// records, for each restriction mode, whether any functioning static of that
/// mode exists. The per-permanent `check_static_ability` call is then gated
/// behind the matching flag: when the flag is false the call would `continue`
/// past every definition and return false anyway (`check_static_ability`
/// rejects on `def.mode != mode` first), so `flag && check_static_ability(..)`
/// is byte-identical to the original call while skipping the redundant scan.
///
/// Representation: named compile-time presence flags, NOT a no-bool-flags
/// anti-pattern — these are independent existence facts, not one
/// choice-encoding bool. An `EnumSet` is rejected because `enumset` is not a
/// workspace dependency and `StaticMode` is not fieldless (it carries data
/// variants such as `MaxUntapPerType { filter, max }` and `Other(String)`).
/// Named flags read clearer than a runtime set lookup.
struct CombatStaticGates {
    has_cant_attack: bool,
    has_cant_attack_or_block: bool,
    has_must_attack: bool,
    has_goad: bool,
    has_can_attack_with_defender: bool,
    /// CR 508.1c: any functioning `StaticMode::AttackOnlyNeighbor` present.
    has_attack_only_neighbor: bool,
}

impl CombatStaticGates {
    /// One `game_functioning_statics` sweep computing the presence flags.
    /// Does NOT increment the static-full-scan perf counter: this is the single
    /// intended hoisted sweep, not a per-element legality scan.
    ///
    /// `has_attack_only_neighbor` is computed from the SAME
    /// `game_functioning_statics` (battlefield + command zone) sweep the CR
    /// 508.1c enforcement loop iterates, so the flag and the enforcement stay in
    /// lockstep AND a command-zone emblem granting `AttackOnlyNeighbor` is both
    /// detected and enforced (CR 113.6: command-zone static abilities function).
    fn compute(state: &GameState) -> Self {
        let mut gates = CombatStaticGates {
            has_cant_attack: false,
            has_cant_attack_or_block: false,
            has_must_attack: false,
            has_goad: false,
            has_can_attack_with_defender: false,
            has_attack_only_neighbor: false,
        };
        for (_, def) in super::functioning_abilities::game_functioning_statics(state) {
            match def.mode {
                StaticMode::CantAttack => gates.has_cant_attack = true,
                StaticMode::CantAttackOrBlock => gates.has_cant_attack_or_block = true,
                StaticMode::MustAttack => gates.has_must_attack = true,
                StaticMode::Goaded => gates.has_goad = true,
                StaticMode::CanAttackWithDefender => gates.has_can_attack_with_defender = true,
                StaticMode::AttackOnlyNeighbor => gates.has_attack_only_neighbor = true,
                _ => {}
            }
        }
        gates
    }
}

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
    /// CR 508.6 + CR 702.121a: For each attacking player, the defending players
    /// they attacked in this combat. Declaration history, not live attacker
    /// membership, so Melee counts remain stable if attackers leave combat
    /// before the trigger resolves.
    #[serde(default)]
    pub attacked_defenders_this_combat: HashMap<PlayerId, HashSet<PlayerId>>,
    /// CR 508.6 + CR 702.121a: Source-specific current-combat counterpart to
    /// `attacked_defenders_this_combat`.
    #[serde(default)]
    pub creature_attacked_defenders_this_combat: HashMap<ObjectId, HashSet<PlayerId>>,
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
            && self.attacked_defenders_this_combat == other.attacked_defenders_this_combat
            && self.creature_attacked_defenders_this_combat
                == other.creature_attacked_defenders_this_combat
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
    /// CR 702.22: Band identifier when this attacker was declared in a band.
    /// `None` = not in a band (attacks and assigns damage individually).
    #[serde(default)]
    pub band_id: Option<u32>,
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
            band_id: None,
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
/// (2) explicit "that player" event context, (3) this controller's declared
/// attackers in the current combat, (4) fallback to opponent.
pub fn enter_attacking(
    state: &mut GameState,
    object_id: ObjectId,
    source_id: ObjectId,
    controller: PlayerId,
) {
    // Determine defending player and attack target before mutable combat borrow.
    let (defending_player, attack_target) =
        defending_player_for_enters_attacking(state, source_id, controller);

    if let Some(combat) = state.combat.as_mut() {
        combat.attackers.push(AttackerInfo::new(
            object_id,
            attack_target,
            defending_player,
        ));
        // CR 508.4 + CR 506.4 + CR 613.1f: a permanent put onto the battlefield
        // attacking is an attacking creature; re-evaluate Layer 6
        // FilterProp::Attacking { defender: None } grants immediately.
        state.layers_dirty.mark_full();
    }
}

/// CR 508.4: Resolve which player/planeswalker a permanent that *enters*
/// attacking should attack. Unlike declared attackers, this path must not use
/// `extract_player_from_event` wholesale — `AttackersDeclared` and
/// `PermanentSacrificed` surface the attacking/sacrificing player, which would
/// make tokens attack their own controller (Caesar #944, Dalkovan Encampment).
fn defending_player_for_enters_attacking(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
) -> (PlayerId, AttackTarget) {
    if let Some(combat) = state.combat.as_ref() {
        if let Some(a) = combat.attackers.iter().find(|a| a.object_id == source_id) {
            return (a.defending_player, a.attack_target);
        }
    }

    if let Some(event) = state.current_trigger_event.as_ref() {
        match event {
            GameEvent::DamageDealt {
                target: TargetRef::Player(pid),
                ..
            }
            | GameEvent::BecomesTarget {
                target: TargetRef::Player(pid),
                ..
            } => return (*pid, AttackTarget::Player(*pid)),
            GameEvent::AttackersDeclared { attacks, .. } => {
                if let Some((_, target)) = attacks.iter().find(|(id, _)| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|obj| obj.controller == controller)
                }) {
                    return attack_target_defender(state, *target);
                }
            }
            _ => {}
        }
    }

    if let Some(combat) = state.combat.as_ref() {
        if let Some(a) = combat.attackers.iter().find(|a| {
            state
                .objects
                .get(&a.object_id)
                .is_some_and(|obj| obj.controller == controller)
        }) {
            return (a.defending_player, a.attack_target);
        }
    }

    let pid = players::opponents(state, controller)
        .first()
        .copied()
        .unwrap_or(controller);
    (pid, AttackTarget::Player(pid))
}

/// Map an `AttackTarget` to the defending player and the target pair stored on
/// `AttackerInfo` (planeswalker/battle controllers for blocking purposes).
fn attack_target_defender(state: &GameState, target: AttackTarget) -> (PlayerId, AttackTarget) {
    let defending = match target {
        AttackTarget::Player(pid) => pid,
        AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => state
            .objects
            .get(&id)
            .map(|obj| obj.controller)
            .unwrap_or(state.active_player),
    };
    (defending, target)
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
        // CR 702.49c + CR 702.190b + CR 506.4 + CR 613.1f: Ninjutsu/Sneak place a
        // creature already attacking; re-evaluate Layer 6 FilterProp::Attacking { defender: None }
        // grants.
        state.layers_dirty.mark_full();
    }
}

/// CR 509.1g + CR 506.3e + CR 509.1h: Put a permanent onto the battlefield as a
/// blocking creature for `attacker_id`. Used by effects that create or place a
/// creature already "blocking that creature" (Mirror Match's copy tokens).
///
/// Per CR 506.3e, the creature only becomes a blocking creature if `attacker_id`
/// is attacking the blocker's controller (or a planeswalker/battle they control,
/// captured by `AttackerInfo.defending_player`); otherwise the creature is on
/// the battlefield but is never considered a blocking creature, so this is a
/// no-op for the combat bookkeeping. Per CR 509.3a/509.3b, a creature put onto
/// the battlefield blocking does NOT cause "whenever ~ blocks" abilities to
/// trigger, so no `BlockersDeclared` event is emitted; combat damage reads the
/// recorded assignments directly. Returns `true` when the block was established.
pub fn place_blocking(state: &mut GameState, blocker_id: ObjectId, attacker_id: ObjectId) -> bool {
    let Some(blocker_controller) = state.objects.get(&blocker_id).map(|o| o.controller) else {
        return false;
    };
    let Some(combat) = state.combat.as_mut() else {
        return false;
    };
    // CR 506.3e: the entering creature only blocks an attacker that is attacking
    // its controller, a planeswalker they control, or a battle they protect —
    // exactly the side recorded as `AttackerInfo.defending_player`.
    let Some(info) = combat
        .attackers
        .iter_mut()
        .find(|a| a.object_id == attacker_id)
    else {
        return false;
    };
    if info.defending_player != blocker_controller {
        return false;
    }
    // CR 509.1h: an attacking creature with one or more blockers becomes blocked.
    info.blocked = true;
    // CR 509.1g: the creature becomes a blocking creature for the chosen attacker.
    combat
        .blocker_to_attacker
        .entry(blocker_id)
        .or_default()
        .push(attacker_id);
    combat
        .blocker_assignments
        .entry(attacker_id)
        .or_default()
        .push(blocker_id);
    // CR 509.1a tracking: record the blocker for per-turn "blocked this turn" queries.
    state.creatures_blocked_this_turn.insert(blocker_id);
    // CR 506.4 + CR 613.1f: a new blocking creature can satisfy Layer 6
    // `FilterProp::Blocking` grants; re-evaluate continuous effects.
    state.layers_dirty.mark_full();
    true
}

/// Validate attacker declarations per CR 508.1.
pub fn validate_attackers(state: &GameState, attacker_ids: &[ObjectId]) -> Result<(), String> {
    let active = state.active_player;

    // CR 508.1c: Attack restrictions make the declaration illegal if disobeyed.
    if let Some(max) = max_attackers_each_combat(state) {
        if attacker_ids.len() as u32 > max {
            return Err(format!(
                "No more than {} creature(s) can attack each combat",
                max
            ));
        }
    }

    // CR 604.1: hoist the combat-restriction existence gates once before the
    // per-attacker scan (collapses O(N^2) to O(N)).
    let gates = CombatStaticGates::compute(state);

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

        // CR 508.1 + CR 805.10a: Must be controlled by the active player or,
        // under the shared team turns option, by a teammate — "each team's
        // creatures attack the other team as a group... each player on the
        // active team is an attacking player."
        if obj.controller != active && !players::teammates(state, active).contains(&obj.controller)
        {
            return Err(format!(
                "{:?} is not controlled by the active player or their team",
                id
            ));
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
                    || (gates.has_can_attack_with_defender
                        && crate::game::static_abilities::check_static_ability(
                            state,
                            StaticMode::CanAttackWithDefender,
                            &crate::game::static_abilities::StaticCheckContext {
                                target_id: Some(id),
                                ..Default::default()
                            },
                        ));
            if !can_attack_with_defender {
                return Err(format!("{:?} has Defender", id));
            }
        }
        // CR 508.1 + CR 101.2 + CR 109.5: Intrinsic CantAttack statics live ON the
        // attacker; remote CantAttack statics (e.g. Angelic Arbiter restricting
        // opponents' creatures via an `affected` filter) live elsewhere and are
        // resolved through the shared `check_static_ability` building block, which
        // matches `def.affected` against this attacker and applies any
        // per-affected-player gate.
        if super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
            matches!(
                sd.mode,
                StaticMode::CantAttack | StaticMode::CantAttackOrBlock
            ) && sd.attack_defended.is_none()
        }) || (gates.has_cant_attack
            && crate::game::static_abilities::check_static_ability(
                state,
                StaticMode::CantAttack,
                &crate::game::static_abilities::StaticCheckContext {
                    target_id: Some(id),
                    ..Default::default()
                },
            ))
            || (gates.has_cant_attack_or_block
                && crate::game::static_abilities::check_static_ability(
                    state,
                    StaticMode::CantAttackOrBlock,
                    &crate::game::static_abilities::StaticCheckContext {
                        target_id: Some(id),
                        ..Default::default()
                    },
                ))
        {
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

    // CR 506.5 + CR 508.1c: CombatAlone(Attack, NeedsCompanion) — creature must
    // NOT be the sole attacker ("can't attack alone"). Two such creatures may
    // attack together (CR 506.5 Example), so only reject when exactly one attacker.
    if attacker_ids.len() == 1 {
        let id = attacker_ids[0];
        if let Some(obj) = state.objects.get(&id) {
            if super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
                sd.mode
                    == (StaticMode::CombatAlone {
                        action: CombatAloneAction::Attack,
                        requirement: CombatAloneRequirement::NeedsCompanion,
                    })
            }) {
                return Err(format!("{id:?} can't attack alone (CR 506.5)"));
            }
        }
    }

    // CR 506.5 + CR 508.1c: CombatAlone(Attack, MustBeSole) — creature must BE
    // the sole attacker ("can only attack alone"). Reject any multi-attacker
    // declaration that includes such a creature.
    if attacker_ids.len() > 1 {
        for &id in attacker_ids {
            if let Some(obj) = state.objects.get(&id) {
                if super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
                    sd.mode
                        == (StaticMode::CombatAlone {
                            action: CombatAloneAction::Attack,
                            requirement: CombatAloneRequirement::MustBeSole,
                        })
                }) {
                    return Err(format!(
                        "{id:?} can only attack alone — may not attack alongside other creatures (CR 506.5 + CR 508.1c)"
                    ));
                }
            }
        }
    }

    Ok(())
}

/// CR 508.1c: The global "no more than N creatures can attack each combat" cap
/// (`defender: None`). Defender-scoped caps ("...attack you each combat") are
/// enforced separately by `validate_per_defender_attacker_caps` because they
/// restrict only attacks against a specific player.
fn max_attackers_each_combat(state: &GameState) -> Option<u32> {
    super::functioning_abilities::battlefield_active_statics(state)
        .filter_map(|(_, def)| match def.mode {
            StaticMode::MaxAttackersEachCombat {
                max,
                defender: None,
            } => Some(max),
            _ => None,
        })
        .min()
}

/// CR 508.1c + CR 802.1: Enforce defender-scoped attacker caps
/// (`MaxAttackersEachCombat { defender: Some(_) }`, e.g. Judoon Enforcers'
/// "no more than one creature can attack you each combat"). Each such static
/// limits only creatures directly attacking the static's controller, so
/// opponents and non-player permanents may still be attacked freely. Returns an
/// error if any active defender-scoped cap is exceeded.
fn validate_per_defender_attacker_caps(
    state: &GameState,
    attacks: &[(ObjectId, AttackTarget)],
) -> Result<(), String> {
    for (source, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::MaxAttackersEachCombat {
            max,
            defender: Some(AttackDefenderScope::Controller),
        } = def.mode
        else {
            continue;
        };
        // CR 109.5: "you" resolves to the controller of the permanent carrying
        // the static.
        let protected_player = source.controller;
        let count = attacks
            .iter()
            .filter(|(_, target)| matches!(target, AttackTarget::Player(pid) if *pid == protected_player))
            .count() as u32;
        if count > max {
            return Err(format!(
                "No more than {max} creature(s) can attack {protected_player:?} each combat (CR 508.1c)"
            ));
        }
    }
    Ok(())
}

/// CR 508.5 + CR 310.8d: Resolve the defending player for an `AttackTarget` —
/// the player for a direct attack, a planeswalker's controller, or a battle's
/// protector.
fn defending_player_for_target(state: &GameState, target: AttackTarget) -> PlayerId {
    match target {
        AttackTarget::Player(pid) => pid,
        AttackTarget::Planeswalker(pw_id) => state
            .objects
            .get(&pw_id)
            .map(|pw| pw.controller)
            .unwrap_or(PlayerId(0)),
        AttackTarget::Battle(battle_id) => state
            .objects
            .get(&battle_id)
            .and_then(|b| b.protector())
            .unwrap_or(PlayerId(0)),
    }
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
/// `static_definitions`. CR 702.26b functioning gates are applied before
/// recipient-relative CR 604.1 / CR 613.1 condition gating.
/// CR 509.1b: Collect every functioning `CantBeBlocked*` static on the
/// battlefield once per legality pass. The relevant set is tiny relative to the
/// battlefield, so cloning the owned `StaticDefinition` is cheap and lets the
/// per-candidate `_from_precomputed` filters run without re-walking the whole
/// battlefield for every attacker. Mirrors the filter in
/// `block_restriction_statics_against`.
pub fn collect_block_restriction_statics(state: &GameState) -> Vec<(ObjectId, StaticDefinition)> {
    super::functioning_abilities::battlefield_functioning_statics(state)
        .filter(|(_, def)| {
            matches!(
                def.mode,
                StaticMode::CantBeBlocked
                    | StaticMode::CantBeBlockedExceptBy { .. }
                    | StaticMode::CantBeBlockedBy { .. }
                    | StaticMode::CantBeBlockedByMoreThan { .. }
            )
        })
        .map(|(src, def)| (src.id, def.clone()))
        .collect()
}

/// CR 509.1b: Collect every functioning `CantBlock` / `CantAttackOrBlock` static
/// once per legality pass. Mirrors the filter in
/// `blocker_restriction_statics_for`.
pub fn collect_blocker_restriction_statics(state: &GameState) -> Vec<(ObjectId, StaticDefinition)> {
    super::functioning_abilities::game_functioning_statics(state)
        .filter(|(_, def)| {
            matches!(
                def.mode,
                StaticMode::CantBlock | StaticMode::CantAttackOrBlock
            )
        })
        .map(|(src, def)| (src.id, def.clone()))
        .collect()
}

/// CR 509.1b: Collect every functioning `BlockRestriction` ("can block only
/// <filter>") static once per legality pass. Mirrors the filter in
/// `blocker_block_allowed_statics_for`.
pub fn collect_blocker_allowed_statics(state: &GameState) -> Vec<(ObjectId, StaticDefinition)> {
    super::functioning_abilities::game_functioning_statics(state)
        .filter(|(_, def)| matches!(def.mode, StaticMode::BlockRestriction { .. }))
        .map(|(src, def)| (src.id, def.clone()))
        .collect()
}

/// CR 509.1c: Collect every functioning `MustBeBlocked` / `MustBeBlockedByAll`
/// static once per legality pass. Mirrors the filter in
/// `must_be_blocked_statics_for_attacker`.
pub fn collect_must_be_blocked_statics(state: &GameState) -> Vec<(ObjectId, StaticDefinition)> {
    super::functioning_abilities::battlefield_functioning_statics(state)
        .filter(|(_, def)| {
            matches!(
                def.mode,
                StaticMode::MustBeBlocked | StaticMode::MustBeBlockedByAll
            )
        })
        .map(|(src, def)| (src.id, def.clone()))
        .collect()
}

/// CR 509.1b: Block restriction — these statics make a block declaration illegal.
/// Re-resolve a precomputed `CantBeBlocked*` static against `attacker_id`,
/// applying the SAME `affected` + `condition` filter stack as
/// `block_restriction_statics_against` but without re-walking the battlefield.
/// Yields `(&StaticDefinition, ObjectId)` — the source id re-resolves the
/// controller for `FilterContext`, so no `GameObject` field beyond `.id` is read.
fn block_restriction_statics_against_from_precomputed<'a>(
    state: &'a GameState,
    attacker_id: ObjectId,
    precomputed: &'a [(ObjectId, StaticDefinition)],
) -> impl Iterator<Item = (&'a StaticDefinition, ObjectId)> + 'a {
    precomputed.iter().filter_map(move |(src_id, def)| {
        let src = state.objects.get(src_id)?;
        // CR 604.1: a static with no `affected` filter is implicitly about its
        // own source (intrinsic SelfRef semantics).
        let affected_ok = match def.affected.as_ref() {
            None => src.id == attacker_id,
            Some(filter) => matches_target_filter(
                state,
                attacker_id,
                filter,
                &FilterContext::from_source(state, src.id),
            ),
        };
        if !affected_ok {
            return None;
        }
        let condition_ok = def.condition.as_ref().is_none_or(|condition| {
            crate::game::layers::evaluate_condition_with_recipient(
                state,
                condition,
                src.controller,
                src.id,
                attacker_id,
            )
        });
        condition_ok.then_some((def, *src_id))
    })
}

/// CR 509.1b: Blocker-side restriction ("~ can't block").
/// Precomputed counterpart of `blocker_restriction_statics_for`.
fn blocker_restriction_statics_for_from_precomputed<'a>(
    state: &'a GameState,
    blocker_id: ObjectId,
    precomputed: &'a [(ObjectId, StaticDefinition)],
) -> impl Iterator<Item = (&'a StaticDefinition, ObjectId)> + 'a {
    precomputed.iter().filter_map(move |(src_id, def)| {
        let src = state.objects.get(src_id)?;
        let affected_ok = match def.affected.as_ref() {
            None => src.id == blocker_id,
            Some(filter) => matches_target_filter(
                state,
                blocker_id,
                filter,
                &FilterContext::from_source(state, src.id),
            ),
        };
        if !affected_ok {
            return None;
        }
        let condition_ok = def.condition.as_ref().is_none_or(|condition| {
            crate::game::layers::evaluate_condition_with_recipient(
                state,
                condition,
                src.controller,
                src.id,
                blocker_id,
            )
        });
        condition_ok.then_some((def, *src_id))
    })
}

/// CR 509.1b: Block-restriction exception ("~ can't block except …").
/// Precomputed counterpart of `blocker_block_allowed_statics_for`.
fn blocker_allowed_statics_for_from_precomputed<'a>(
    state: &'a GameState,
    blocker_id: ObjectId,
    precomputed: &'a [(ObjectId, StaticDefinition)],
) -> impl Iterator<Item = (&'a StaticDefinition, ObjectId)> + 'a {
    precomputed.iter().filter_map(move |(src_id, def)| {
        let src = state.objects.get(src_id)?;
        let affected_ok = match def.affected.as_ref() {
            None => src.id == blocker_id,
            Some(filter) => matches_target_filter(
                state,
                blocker_id,
                filter,
                &FilterContext::from_source(state, src.id),
            ),
        };
        if !affected_ok {
            return None;
        }
        let condition_ok = def.condition.as_ref().is_none_or(|condition| {
            crate::game::layers::evaluate_condition_with_recipient(
                state,
                condition,
                src.controller,
                src.id,
                blocker_id,
            )
        });
        condition_ok.then_some((def, *src_id))
    })
}

/// CR 509.1c: Block requirement ("~ must be blocked if able").
/// Precomputed counterpart of `must_be_blocked_statics_for_attacker`.
fn must_be_blocked_statics_for_attacker_from_precomputed<'a>(
    state: &'a GameState,
    attacker_id: ObjectId,
    precomputed: &'a [(ObjectId, StaticDefinition)],
) -> impl Iterator<Item = (&'a StaticDefinition, ObjectId)> + 'a {
    precomputed.iter().filter_map(move |(src_id, def)| {
        let src = state.objects.get(src_id)?;
        let affected_ok = match def.affected.as_ref() {
            None => src.id == attacker_id,
            Some(filter) => matches_target_filter(
                state,
                attacker_id,
                filter,
                &FilterContext::from_source(state, src.id),
            ),
        };
        if !affected_ok {
            return None;
        }
        let condition_ok = def.condition.as_ref().is_none_or(|condition| {
            crate::game::layers::evaluate_condition_with_recipient(
                state,
                condition,
                src.controller,
                src.id,
                attacker_id,
            )
        });
        condition_ok.then_some((def, *src_id))
    })
}

/// CR 509.1b: precomputed-slice variant of `blocker_has_cant_block_static`.
fn blocker_has_cant_block_static_from_precomputed(
    state: &GameState,
    blocker_id: ObjectId,
    precomputed: &[(ObjectId, StaticDefinition)],
) -> bool {
    blocker_restriction_statics_for_from_precomputed(state, blocker_id, precomputed)
        .next()
        .is_some()
}

/// CR 509.1c: precomputed-slice variant of `attacker_has_must_be_blocked`.
fn attacker_has_must_be_blocked_from_precomputed(
    state: &GameState,
    attacker_id: ObjectId,
    precomputed: &[(ObjectId, StaticDefinition)],
) -> bool {
    must_be_blocked_statics_for_attacker_from_precomputed(state, attacker_id, precomputed)
        .any(|(def, _)| def.mode == StaticMode::MustBeBlocked)
}

/// CR 509.1c: precomputed-slice variant of `attacker_has_must_be_blocked_by_all`.
fn attacker_has_must_be_blocked_by_all_from_precomputed(
    state: &GameState,
    attacker_id: ObjectId,
    precomputed: &[(ObjectId, StaticDefinition)],
) -> bool {
    must_be_blocked_statics_for_attacker_from_precomputed(state, attacker_id, precomputed)
        .any(|(def, _)| def.mode == StaticMode::MustBeBlockedByAll)
}

/// CR 509.1b + CR 609.4 + CR 702.28b: A creature without shadow normally can't
/// block a creature with shadow. This returns `true` when the blocker has a
/// functioning `CanBlockShadow` static — "~ can block creatures with shadow as
/// though they didn't have shadow" / "as though it had shadow" — which lifts
/// the shadow blocker-side restriction for that affected creature.
///
/// Mirrors the `CanAttackWithDefender` lookup: intrinsic self statics are read
/// from the blocker, and remote affected filters are resolved through the shared
/// static-ability checker.
fn blocker_can_block_shadow(state: &GameState, blocker: &GameObject) -> bool {
    crate::game::perf_counters::record_combat_shadow_block_scan();
    super::functioning_abilities::active_static_definitions(state, blocker)
        .any(|sd| sd.mode == StaticMode::CanBlockShadow)
        || crate::game::static_abilities::check_static_ability(
            state,
            StaticMode::CanBlockShadow,
            &crate::game::static_abilities::StaticCheckContext {
                target_id: Some(blocker.id),
                ..Default::default()
            },
        )
}

// CR 604.1: static abilities are continuously "on"; if NO functioning
// CanBlockShadow static exists anywhere (the loop-invariant existence gate),
// both the blocker's intrinsic static scan and the remote check_static_ability
// sweep return false, so this is byte-identical to the full predicate while
// skipping the O(N) per-blocker sweep. CR 509.1b/609.4/702.28b: a CanBlockShadow
// static lifts the shadow block restriction for the affected blocker.
fn blocker_can_block_shadow_gated(
    state: &GameState,
    blocker: &GameObject,
    can_block_shadow_exists: bool,
) -> bool {
    can_block_shadow_exists && blocker_can_block_shadow(state, blocker)
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
    // CR 509.1b: Block restrictions make the declaration illegal if disobeyed.
    if let Some(max) = max_blockers_each_combat(state) {
        let already_declared = state
            .combat
            .as_ref()
            .map(|combat| combat.blocker_to_attacker.keys().count())
            .unwrap_or(0);
        let newly_declared = assignments
            .iter()
            .map(|(blocker_id, _)| *blocker_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        if (already_declared + newly_declared) as u32 > max {
            return Err(format!(
                "No more than {} creature(s) can block each combat",
                max
            ));
        }
    }

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

    // Hoist each kind of relevant static ONCE for this whole legality pass. Every
    // per-blocker / per-attacker / per-battlefield loop below reads from these
    // slices via the `_from_precomputed` helpers instead of re-walking the
    // battlefield, turning the O(battlefield²) scan into a single sweep.
    let blocker_restriction = collect_blocker_restriction_statics(state);
    let block_restriction = collect_block_restriction_statics(state);
    let blocker_allowed = collect_blocker_allowed_statics(state);
    let must_be_blocked = collect_must_be_blocked_statics(state);
    // CR 604.1: loop-invariant existence gate for the shadow block-lift (CR
    // 509.1b/609.4/702.28b). Hoisted once so the per-blocker shadow scan below
    // and every `can_block_pair_with_precomputed` call skip the O(N)
    // `check_static_ability` sweep when no `CanBlockShadow` static exists.
    let can_block_shadow_exists =
        super::functioning_abilities::any_functioning_static_mode(state, |m| {
            matches!(m, StaticMode::CanBlockShadow)
        });

    // Group assignments by attacker for menace validation and by blocker for
    // per-creature block-capacity checks.
    let mut blockers_per_attacker: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut attackers_per_blocker: HashMap<ObjectId, u32> = HashMap::new();

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
        //
        // CR 805.10d: Under the shared team turns option this is widened to
        // the whole defending team — "Creatures controlled by the defending
        // players can block creatures attacking any player on the defending
        // team, attacking a planeswalker controlled by one of those players,
        // or a battle protected by one of those players." So a blocker may
        // also defend an attack whose `defending_player` is its controller's
        // teammate, not just its own controller.
        if let Some(combat) = &state.combat {
            if let Some(attacker_info) =
                combat.attackers.iter().find(|a| a.object_id == attacker_id)
            {
                let defending_player = attacker_info.defending_player;
                let blocks_for_team = defending_player == player
                    || players::teammates(state, player).contains(&defending_player);
                if !blocks_for_team {
                    return Err(format!(
                        "{:?} cannot block {:?} (not attacking this player or their team)",
                        blocker_id, attacker_id
                    ));
                }
            }
        }

        // Must not be tapped
        if blocker.tapped {
            return Err(format!("{:?} is tapped", blocker_id));
        }
        // CR 702.147a: Decayed means "This creature can't block."
        if blocker.has_keyword(&Keyword::Decayed) {
            return Err(format!("{:?} has decayed and can't block", blocker_id));
        }
        if blocker_has_cant_block_static_from_precomputed(state, blocker_id, &blocker_restriction) {
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
        for (def, src_id) in block_restriction_statics_against_from_precomputed(
            state,
            attacker_id,
            &block_restriction,
        ) {
            match &def.mode {
                StaticMode::CantBeBlocked => {
                    return Err(format!(
                        "{:?} cannot block {:?} (can't be blocked)",
                        blocker_id, attacker_id
                    ));
                }
                StaticMode::CantBeBlockedExceptBy { kind } => match kind {
                    BlockExceptionKind::Quality(target_filter) => {
                        if !matches_target_filter(
                            state,
                            blocker_id,
                            target_filter,
                            &FilterContext::from_source(state, src_id),
                        ) {
                            return Err(format!(
                                "{:?} cannot block {:?} (can't be blocked except by {:?})",
                                blocker_id, attacker_id, target_filter
                            ));
                        }
                    }
                    // CR 509.1b: a count constraint is a multi-blocker check —
                    // enforced in the blockers_per_attacker count pass, not per-pair.
                    BlockExceptionKind::MinBlockers { .. } => {}
                },
                StaticMode::CantBeBlockedBy { filter }
                    if matches_target_filter(
                        state,
                        blocker_id,
                        filter,
                        &FilterContext::from_source(state, src_id),
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

        if ring_bearer_unblockable_by_greater_power(state, attacker, blocker) {
            return Err(format!(
                "{:?} cannot block {:?} (Ring-bearer can't be blocked by greater power)",
                blocker_id, attacker_id
            ));
        }

        // CR 702.16f: Protection — an attacking creature with protection can't
        // be blocked by creatures with the stated quality.
        for kw in &attacker.keywords {
            if let Keyword::Protection(target) = kw {
                if crate::game::keywords::source_matches_protection_target(
                    target, attacker, blocker,
                ) {
                    return Err(format!(
                        "{blocker_id:?} cannot block {attacker_id:?} (protection)",
                    ));
                }
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
        // CR 509.1b + CR 609.4 + CR 702.28b: a `CanBlockShadow` static lifts the
        // shadow restriction for this blocker (Heartwood Dryad, Wall of Diffusion).
        if attacker_has_shadow
            && !blocker_has_shadow
            && !blocker_can_block_shadow_gated(state, blocker, can_block_shadow_exists)
        {
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

        // CR 509.1b: blocker-side "can block only <filter>" restrictions.
        for (def, src_id) in
            blocker_allowed_statics_for_from_precomputed(state, blocker_id, &blocker_allowed)
        {
            let StaticMode::BlockRestriction { filter } = &def.mode else {
                continue;
            };
            if !matches_target_filter(
                state,
                attacker_id,
                filter,
                &FilterContext::from_source(state, src_id),
            ) {
                return Err(format!(
                    "{blocker_id:?} can block only creatures matching the block restriction"
                ));
            }
        }

        blockers_per_attacker
            .entry(attacker_id)
            .or_default()
            .push(blocker_id);
        *attackers_per_blocker.entry(blocker_id).or_default() += 1;
    }

    // CR 506.5 + CR 509.1b: CombatAlone(Block, NeedsCompanion) — creature must
    // NOT be the sole blocker ("can't block alone"). Two such creatures may block together.
    if attackers_per_blocker.len() == 1 {
        let (&blocker_id, _) = attackers_per_blocker.iter().next().expect("len checked");
        if let Some(obj) = state.objects.get(&blocker_id) {
            if super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
                sd.mode
                    == (StaticMode::CombatAlone {
                        action: CombatAloneAction::Block,
                        requirement: CombatAloneRequirement::NeedsCompanion,
                    })
            }) {
                return Err(format!("{blocker_id:?} can't block alone (CR 506.5)"));
            }
        }
    }

    // CR 509.1a + CR 509.1b: Enforce per-blocker limit on how many attackers it can block.
    // Default is 1; ExtraBlockers { count: Some(n) } allows 1 + n; count: None = unlimited.
    {
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

    // CR 702.111b (Menace) + CR 509.1b ("can't be blocked except by N or more
    // creatures"): an attacker that is blocked at all must be blocked by at least
    // its required number of creatures — "or not at all." `blockers_per_attacker`
    // only holds attackers with >= 1 assigned blocker, so iterating it enforces
    // the "or not at all" clause for free. `min_blockers_required` is the single
    // authority that unifies the menace floor (2) with any MinBlockers floor and
    // is the same value surfaced to the UI via `block_requirements`.
    for (attacker_id, blockers) in &blockers_per_attacker {
        let required =
            min_blockers_required_from_precomputed(state, *attacker_id, &block_restriction);
        if (blockers.len() as u32) < required {
            return Err(format!(
                "{:?} must be blocked by {} or more creatures",
                attacker_id, required
            ));
        }
        // CR 509.1b: "can't be blocked by more than N creatures" — a per-creature
        // blocker maximum (Stalking Tiger). Inverse of the menace minimum above;
        // an attacker with both must satisfy both.
        if let Some(max) =
            max_blockers_allowed_from_precomputed(state, *attacker_id, &block_restriction)
        {
            if (blockers.len() as u32) > max {
                return Err(format!(
                    "{:?} can't be blocked by more than {} creature(s)",
                    attacker_id, max
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

            if !attacker_has_must_be_blocked_from_precomputed(state, attacker_id, &must_be_blocked)
            {
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
                    && can_block_pair_with_precomputed(
                        state,
                        *id,
                        attacker_id,
                        &blocker_restriction,
                        &block_restriction,
                        &blocker_allowed,
                        can_block_shadow_exists,
                    )
            });

            if has_available_blocker {
                return Err(format!(
                    "{:?} must be blocked if able (CR 509.1c)",
                    attacker_id
                ));
            }
        }

        // CR 509.1c: MustBeBlockedByAll — the "lure" requirement ("All creatures
        // able to block ~ do so": Lure, Prized Unicorn, Breaker of Armies, …).
        // Every creature this defending player controls that could legally block
        // the lured attacker carries a "must block it" requirement, so — unlike
        // MustBeBlocked, which is satisfied by a single blocker — there must be
        // *no* creature with spare block capacity able to block it left off that
        // attacker. A blocker already at its per-creature block limit is not
        // able to add another block; a blocker with ExtraBlockers spare capacity
        // still must also block the lured attacker.
        for attacker_info in &combat.attackers {
            if attacker_info.defending_player != player {
                continue;
            }
            let attacker_id = attacker_info.object_id;
            if !attacker_has_must_be_blocked_by_all_from_precomputed(
                state,
                attacker_id,
                &must_be_blocked,
            ) {
                continue;
            }
            // Any untapped defender with spare block capacity that could legally
            // block the lured attacker should have been declared as its blocker.
            let has_idle_able_blocker = state.battlefield.iter().any(|id| {
                if blockers_per_attacker
                    .get(&attacker_id)
                    .is_some_and(|blockers| blockers.contains(id))
                {
                    return false;
                }
                let Some(obj) = state.objects.get(id) else {
                    return false;
                };
                if obj.controller != player
                    || !obj.card_types.core_types.contains(&CoreType::Creature)
                    || obj.tapped
                    || !can_block_pair_with_precomputed(
                        state,
                        *id,
                        attacker_id,
                        &blocker_restriction,
                        &block_restriction,
                        &blocker_allowed,
                        can_block_shadow_exists,
                    )
                {
                    return false;
                }
                let assigned_count = attackers_per_blocker.get(id).copied().unwrap_or(0);
                assigned_count < extra_block_limit(state, obj)
            });
            if has_idle_able_blocker {
                return Err(format!(
                    "{:?} must be blocked by every creature able to block it (CR 509.1c)",
                    attacker_id
                ));
            }
        }

        // CR 509.1c + CR 802.4b: Check MustBlock only for this defending
        // player's declaration.
        // If a defending creature has MustBlock and isn't assigned as a blocker,
        // verify it couldn't legally block any attacker.
        // CR 604.1: hoist the MustBlock existence gate once before iterating N
        // permanents so the per-permanent `check_static_ability` re-scan is
        // skipped when no functioning MustBlock static exists (O(N^2) -> O(N)).
        let has_must_block_static =
            super::functioning_abilities::any_functioning_static_mode(state, |m| {
                matches!(m, StaticMode::MustBlock)
            });
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
                    || (has_must_block_static
                        && crate::game::static_abilities::check_static_ability(
                            state,
                            StaticMode::MustBlock,
                            &crate::game::static_abilities::StaticCheckContext {
                                target_id: Some(obj_id),
                                ..Default::default()
                            },
                        ));
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
            // CR 702.147a: Decayed creatures can't block, so an unassigned
            // decayed MustBlock creature cannot make the declaration illegal.
            if obj.has_keyword(&Keyword::Decayed) {
                continue;
            }
            if blocker_has_cant_block_static_from_precomputed(state, obj_id, &blocker_restriction) {
                continue;
            }
            // CR 701.35a: Detained creatures can't block.
            if !obj.detained_by.is_empty() {
                continue;
            }
            // Check if this creature could legally block any attacker attacking its controller
            let can_block_any = combat.attackers.iter().any(|ai| {
                ai.defending_player == obj.controller
                    && can_block_pair_with_precomputed(
                        state,
                        obj_id,
                        ai.object_id,
                        &blocker_restriction,
                        &block_restriction,
                        &blocker_allowed,
                        can_block_shadow_exists,
                    )
            });
            if can_block_any {
                return Err(format!("{:?} must block if able (CR 509.1c)", obj_id));
            }
        }

        // CR 702.39a + CR 509.1c: MustBlockAttacker — a creature forced to block
        // a SPECIFIC attacker (Provoke; "target creature blocks ~ this turn if
        // able") must be declared as a blocker of *that* attacker when it can
        // legally block it. This is stricter than generic MustBlock: blocking a
        // different attacker does not satisfy the requirement.
        for &obj_id in &state.battlefield {
            let Some(obj) = state.objects.get(&obj_id) else {
                continue;
            };
            if obj.controller != player || !obj.card_types.core_types.contains(&CoreType::Creature)
            {
                continue;
            }
            // The specific attackers this creature is required to block.
            // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
            let required: Vec<ObjectId> =
                super::functioning_abilities::active_static_definitions(state, obj)
                    .filter_map(|sd| match sd.mode {
                        StaticMode::MustBlockAttacker { attacker } => Some(attacker),
                        _ => None,
                    })
                    .collect();
            if required.is_empty() {
                continue;
            }
            // A creature that can't block at all carries no requirement
            // (CR 509.1a tapped / CR 702.147a decayed / CR 701.35a detained /
            // CantBlock). `can_block_pair` does not itself reject tapped, so the
            // explicit guards mirror the generic MustBlock check above.
            if obj.tapped
                || obj.has_keyword(&Keyword::Decayed)
                || !obj.detained_by.is_empty()
                || blocker_has_cant_block_static_from_precomputed(
                    state,
                    obj_id,
                    &blocker_restriction,
                )
            {
                continue;
            }
            for attacker_id in required {
                // Only enforce while that attacker is actually attacking this
                // defending player this combat.
                let is_active_attacker = combat
                    .attackers
                    .iter()
                    .any(|ai| ai.object_id == attacker_id && ai.defending_player == player);
                if !is_active_attacker {
                    continue;
                }
                // If the creature can legally block the named attacker but isn't
                // declared as its blocker, the declaration is illegal.
                let assigned_to_attacker = assignments
                    .iter()
                    .any(|&(blocker, attacker)| blocker == obj_id && attacker == attacker_id);
                if !assigned_to_attacker
                    && can_block_pair_with_precomputed(
                        state,
                        obj_id,
                        attacker_id,
                        &blocker_restriction,
                        &block_restriction,
                        &blocker_allowed,
                        can_block_shadow_exists,
                    )
                {
                    return Err(format!(
                        "{obj_id:?} must block {attacker_id:?} this turn if able (CR 509.1c)"
                    ));
                }
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
    creatures: &[(ObjectId, Option<AttackTarget>)],
    context: crate::types::game_state::CombatTaxContext,
) -> Option<(
    crate::types::mana::ManaCost,
    Vec<(ObjectId, crate::types::mana::ManaCost)>,
)> {
    use crate::types::ability::UnlessPayScaling;
    use crate::types::game_state::CombatTaxContext;
    use crate::types::mana::ManaCost;

    if creatures.is_empty() {
        return None;
    }

    // Pre-collect the affected creature count for scaling — used by
    // PerAffectedCreature (count of declared creatures this static touches) so
    // the arithmetic is order-independent.
    let mut per_creature: Vec<(ObjectId, ManaCost)> = creatures
        .iter()
        .map(|&(id, _)| (id, ManaCost::zero()))
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
            // CR 611.3a + CR 118.12a: The combat-tax payload may live directly
            // on `def.condition` (Ghostly Prison) or nested inside an
            // `And { conditions }` paired with a gating predicate
            // (Archangel of Tithes — `And { [Not(SourceIsTapped),
            // UnlessPay {..}] }`). `extract_combat_tax_payload` walks the
            // tree, returning `None` when no payload exists OR when a paired
            // gating conjunct evaluates to `false` (the tax is dormant).
            let Some((base_cost, scaling, defended)) = def.condition.as_ref().and_then(|cond| {
                extract_combat_tax_payload(cond, state, source_obj.controller, source_id)
            }) else {
                continue;
            };

            // For each declared creature, determine if this static's affected filter matches.
            // CR 506.3 + CR 508.1d: When `defended` is set, also require the
            // declared `AttackTarget` to match the filter, scoped to the
            // static's source controller. This prevents Propaganda from taxing
            // attacks made against players OTHER than its controller (#302),
            // and allows Archangel of Tithes' "you or planeswalkers you
            // control" to match attacks against either the defender or one
            // of their planeswalkers.
            let mut affected_indices: Vec<usize> = Vec::with_capacity(creatures.len());
            let ctx = FilterContext::from_source(state, source_id);
            for (index, &(cid, attack_target)) in creatures.iter().enumerate() {
                if let Some(filter) = defended {
                    if !super::restrictions::attack_target_matches_defended_scope(
                        state,
                        attack_target.as_ref(),
                        filter,
                        source_obj.controller,
                        source_obj.owner,
                    ) {
                        continue;
                    }
                }
                let creature_matches = match &def.affected {
                    Some(filter) => matches_target_filter(state, cid, filter, &ctx),
                    // No affected filter — treat as "applies to all taxed creatures",
                    // matching the behavior of `check_static_ability` when `affected`
                    // is None.
                    None => true,
                };
                if !creature_matches {
                    continue;
                }
                affected_indices.push(index);
            }
            if affected_indices.is_empty() {
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
                    if let Some(&first_idx) = affected_indices.first() {
                        let slot = &mut per_creature[first_idx].1;
                        *slot = slot.plus(&total);
                        any_tax = true;
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
                    let mut cost = base_cost.clone();
                    cost.concretize_x(n.max(0) as u32);
                    cost
                }
                UnlessPayScaling::PerAffectedWithRef { quantity } => {
                    // CR 118.12a + CR 202.3e: Nils, Discipline Enforcer — "pays {X},
                    // where X is the number of counters on that creature". The
                    // scaling quantity is resolved per-affected-creature with that
                    // creature as the target, so each attacker pays base_cost times
                    // its own counter count. Attribute the resolved cost directly
                    // to each affected creature and continue (skip the shared
                    // per_match_cost distribution below).
                    for &affected_idx in &affected_indices {
                        let aid = per_creature[affected_idx].0;
                        let n = crate::game::quantity::resolve_quantity_with_targets_slice(
                            state,
                            &crate::types::ability::QuantityExpr::Ref {
                                qty: quantity.clone(),
                            },
                            source_obj.controller,
                            source_id,
                            &[crate::types::ability::TargetRef::Object(aid)],
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
                        let slot = &mut per_creature[affected_idx].1;
                        *slot = slot.plus(&cost);
                        any_tax = true;
                    }
                    continue;
                }
            };

            for &affected_idx in &affected_indices {
                let slot = &mut per_creature[affected_idx].1;
                *slot = slot.plus(&per_match_cost);
                any_tax = true;
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
///
/// Carries `AttackTarget` per attacker so the runtime can enforce
/// `StaticCondition::UnlessPay { defended, .. }` — Propaganda must only tax
/// attacks against its controller, not attacks against other opponents (CR 506.3).
pub fn compute_attack_tax(
    state: &GameState,
    attacks: &[(ObjectId, AttackTarget)],
) -> Option<(
    crate::types::mana::ManaCost,
    Vec<(ObjectId, crate::types::mana::ManaCost)>,
)> {
    let pairs: Vec<(ObjectId, Option<AttackTarget>)> =
        attacks.iter().map(|(id, t)| (*id, Some(*t))).collect();
    compute_combat_tax(
        state,
        &pairs,
        crate::types::game_state::CombatTaxContext::Attacking,
    )
}

/// CR 509.1c + CR 509.1d: Specialization of `compute_combat_tax` for the block step.
///
/// Block-side restrictions never carry a defender scope (the parser drops any
/// scope tail for `CantBlock`), so `attack_target` is uniformly `None` here.
pub fn compute_block_tax(
    state: &GameState,
    assignments: &[(ObjectId, ObjectId)],
) -> Option<(
    crate::types::mana::ManaCost,
    Vec<(ObjectId, crate::types::mana::ManaCost)>,
)> {
    let pairs: Vec<(ObjectId, Option<AttackTarget>)> =
        assignments.iter().map(|(b, _)| (*b, None)).collect();
    compute_combat_tax(
        state,
        &pairs,
        crate::types::game_state::CombatTaxContext::Blocking,
    )
}

/// CR 611.3a + CR 118.12a: Walk a `StaticCondition` tree to find an embedded
/// `UnlessPay` payload, evaluating any AND-paired gating conjuncts via
/// `evaluate_condition`. Returns the payload only when the gate is satisfied
/// (so Archangel of Tithes' tax is dormant while it's tapped). Returns the
/// first `UnlessPay` found in left-to-right order; the parser only emits one
/// per static.
fn extract_combat_tax_payload<'a>(
    cond: &'a crate::types::ability::StaticCondition,
    state: &GameState,
    controller: PlayerId,
    source_id: ObjectId,
) -> Option<(
    &'a crate::types::mana::ManaCost,
    &'a crate::types::ability::UnlessPayScaling,
    Option<&'a crate::types::triggers::AttackTargetFilter>,
)> {
    use crate::types::ability::StaticCondition;
    match cond {
        StaticCondition::UnlessPay {
            cost,
            scaling,
            defended,
        } => Some((cost, scaling, defended.as_ref())),
        StaticCondition::And { conditions } => {
            // CR 611.3a: Find the first UnlessPay payload, then verify every
            // non-UnlessPay sibling evaluates to true. If any non-payload gate
            // fails, the tax is dormant. Sibling UnlessPays are NOT treated as
            // gates (layers::evaluate_condition returns `false` for UnlessPay
            // by design — that would always make the tax dormant). The parser
            // does not produce multi-UnlessPay siblings today; if it ever does,
            // this code uses the first and ignores the rest, which is a
            // conservative under-count rather than an incorrect dormancy.
            let payload_idx = conditions
                .iter()
                .position(|c| matches!(c, StaticCondition::UnlessPay { .. }))?;
            for (i, sibling) in conditions.iter().enumerate() {
                if i == payload_idx {
                    continue;
                }
                if matches!(sibling, StaticCondition::UnlessPay { .. }) {
                    continue;
                }
                if !crate::game::layers::evaluate_condition(state, sibling, controller, source_id) {
                    return None;
                }
            }
            extract_combat_tax_payload(&conditions[payload_idx], state, controller, source_id)
        }
        _ => None,
    }
}

/// CR 508.1d / CR 701.15b: True if `obj_id` is a creature controlled by the
/// *active player* that carries a must-attack requirement it can currently
/// satisfy, and therefore must be declared as an attacker or the declaration
/// is illegal.
///
/// This is the single authority for the must-attack requirement + exemption
/// logic — both `declare_attackers` (validation) and the AI attacker selection
/// call it. Returns `false` (no requirement to satisfy) when ANY of:
///  - `obj.controller != state.active_player` (active-player guard)
///  - the object is not a `CoreType::Creature`
///  - it has neither a `StaticMode::MustAttack` static (CR 508.1d) nor a
///    goading player from `goading_players_for_creature` (CR 701.15b)
///  - `obj.tapped` (CR 508.1a: chosen attackers must be untapped)
///  - it has `Keyword::Defender` and no `StaticMode::CanAttackWithDefender`
///    override (CR 702.3b)
///  - `has_summoning_sickness(obj)` (CR 302.6)
pub fn creature_must_attack(state: &GameState, obj_id: ObjectId) -> bool {
    let attackable_players = attackable_player_targets(state);
    creature_must_attack_with_attackable_players(state, obj_id, &attackable_players)
}

pub fn attackable_player_targets(state: &GameState) -> Vec<PlayerId> {
    crate::game::perf_counters::record_attackable_player_sweep();
    get_valid_attack_targets(state)
        .into_iter()
        .filter_map(|target| match target {
            AttackTarget::Player(pid) => Some(pid),
            _ => None,
        })
        .collect()
}

/// CR 508.1b: The players this creature is required to attack directly via a
/// `StaticMode::MustAttackPlayer` static ("attacks ~ each combat if able"
/// directed at a specific player). Single authority for the requirement; the
/// declare-attackers validator enforces it and the AI candidate generator reuses
/// it to steer a forced-legal assignment toward the required player.
pub(crate) fn must_attack_players_for_creature(
    state: &GameState,
    obj: &GameObject,
) -> Vec<PlayerId> {
    super::functioning_abilities::active_static_definitions(state, obj)
        .filter_map(|sd| match sd.mode {
            StaticMode::MustAttackPlayer { player } => Some(player),
            _ => None,
        })
        .collect()
}

pub fn creature_must_attack_with_attackable_players(
    state: &GameState,
    obj_id: ObjectId,
    attackable_players: &[PlayerId],
) -> bool {
    // Single-permanent entry: compute the loop-invariant gates once, then
    // delegate. The single batch caller (`declare_attackers_with_bands`) reuses
    // its already-hoisted gates via the `_gated` form below.
    let gates = CombatStaticGates::compute(state);
    creature_must_attack_with_attackable_players_gated(state, obj_id, attackable_players, &gates)
}

fn creature_must_attack_with_attackable_players_gated(
    state: &GameState,
    obj_id: ObjectId,
    attackable_players: &[PlayerId],
    gates: &CombatStaticGates,
) -> bool {
    let Some(obj) = state.objects.get(&obj_id) else {
        return false;
    };
    // Active-player guard.
    if obj.controller != state.active_player {
        return false;
    }
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        return false;
    }
    // CR 508.1d: MustAttack — either directly on this creature or from a
    // cross-permanent static (e.g., "All creatures attack each combat if able").
    let has_must_attack = super::functioning_abilities::active_static_definitions(state, obj)
        .any(|sd| sd.mode == StaticMode::MustAttack)
        || (gates.has_must_attack
            && crate::game::static_abilities::check_static_ability(
                state,
                StaticMode::MustAttack,
                &crate::game::static_abilities::StaticCheckContext {
                    target_id: Some(obj_id),
                    ..Default::default()
                },
            ));
    // CR 701.15b: Goaded creatures must attack each combat if able.
    let is_goaded = !goading_players_for_creature_gated(state, obj_id, gates.has_goad).is_empty();
    let has_attackable_must_attack_player = must_attack_players_for_creature(state, obj)
        .iter()
        .any(|player| attackable_players.contains(player));
    if !has_must_attack && !is_goaded && !has_attackable_must_attack_player {
        return false;
    }
    // Exemptions: tapped, defender (no override), summoning sick.
    // CR 508.1a: chosen attackers must be untapped.
    if obj.tapped {
        return false;
    }
    // CR 702.3b: Defender — creature can't attack (unless overridden).
    if obj.has_keyword(&Keyword::Defender) {
        let can_attack_with_defender =
            super::functioning_abilities::active_static_definitions(state, obj)
                .any(|sd| sd.mode == StaticMode::CanAttackWithDefender)
                || (gates.has_can_attack_with_defender
                    && crate::game::static_abilities::check_static_ability(
                        state,
                        StaticMode::CanAttackWithDefender,
                        &crate::game::static_abilities::StaticCheckContext {
                            target_id: Some(obj_id),
                            ..Default::default()
                        },
                    ));
        if !can_attack_with_defender {
            return false;
        }
    }
    // CR 302.6: Summoning sickness — reuse existing helper.
    if has_summoning_sickness(obj) {
        return false;
    }
    true
}

/// CR 702.22: Whether a creature has the banding keyword.
pub fn has_banding(state: &GameState, object_id: ObjectId) -> bool {
    state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.has_keyword(&Keyword::Banding))
}

fn bands_with_other_qualities(state: &GameState, object_id: ObjectId) -> Vec<&str> {
    state
        .objects
        .get(&object_id)
        .map(|obj| {
            obj.keywords
                .iter()
                .filter_map(|keyword| match keyword {
                    Keyword::BandsWithOther(quality) => Some(quality.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn object_matches_bands_with_other_quality(
    state: &GameState,
    object_id: ObjectId,
    quality: &str,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if quality.eq_ignore_ascii_case("legend") {
        return obj.card_types.supertypes.contains(&Supertype::Legendary);
    }
    obj.card_types
        .subtypes
        .iter()
        .any(|subtype| subtype.eq_ignore_ascii_case(quality))
}

fn has_bands_with_other_quality(state: &GameState, object_id: ObjectId, quality: &str) -> bool {
    bands_with_other_qualities(state, object_id)
        .into_iter()
        .any(|other| other.eq_ignore_ascii_case(quality))
}

/// CR 702.22j/k: Damage-assignment control also applies to "bands with other"
/// when the relevant blocker/attacker set contains a qualifying keyword-holder
/// and another creature of the same quality.
pub fn has_bands_with_other_damage_assignment_group(
    state: &GameState,
    creatures: &[ObjectId],
) -> bool {
    creatures.iter().any(|&holder| {
        bands_with_other_qualities(state, holder)
            .into_iter()
            .any(|quality| {
                object_matches_bands_with_other_quality(state, holder, quality)
                    && creatures.iter().any(|&other| {
                        other != holder
                            && object_matches_bands_with_other_quality(state, other, quality)
                    })
            })
    })
}

fn validate_bands_with_other_declaration(
    state: &GameState,
    members: &[ObjectId],
) -> Result<(), String> {
    if members.is_empty() {
        return Err("empty bands-with-other declaration".to_string());
    }
    for &holder in members {
        for quality in bands_with_other_qualities(state, holder) {
            if object_matches_bands_with_other_quality(state, holder, quality)
                && has_bands_with_other_quality(state, holder, quality)
                && members
                    .iter()
                    .all(|&member| object_matches_bands_with_other_quality(state, member, quality))
            {
                return Ok(());
            }
        }
    }
    Err("band must satisfy ordinary banding or a shared bands-with-other quality".to_string())
}

/// CR 702.22: Attackers sharing a `band_id` (declaration order preserved).
pub fn band_members(combat: &CombatState, band_id: u32) -> Vec<&AttackerInfo> {
    combat
        .attackers
        .iter()
        .filter(|a| a.band_id == Some(band_id))
        .collect()
}

/// CR 702.22c/d: Validate explicit band declarations at declare attackers.
/// Each band must contain only declared attackers (702.22c), share one attack
/// target (702.22d), include at least one banding creature, and at most one
/// non-banding creature (702.22c), or satisfy a "bands with other [quality]"
/// declaration over a shared quality.
pub fn validate_attack_band_declarations(
    state: &GameState,
    attacks: &[(ObjectId, AttackTarget)],
    bands: &[Vec<ObjectId>],
) -> Result<(), String> {
    let attack_targets: HashMap<ObjectId, AttackTarget> = attacks.iter().copied().collect();
    let mut assigned = HashSet::new();

    for (band_index, members) in bands.iter().enumerate() {
        if members.is_empty() {
            return Err(format!("band {band_index} is empty"));
        }
        let mut banding_count = 0u32;
        let mut non_banding_count = 0u32;
        let mut band_target = None;

        for &member_id in members {
            if !assigned.insert(member_id) {
                return Err(format!(
                    "{member_id:?} appears in more than one declared band"
                ));
            }
            let Some(target) = attack_targets.get(&member_id).copied() else {
                return Err(format!(
                    "{member_id:?} is in band {band_index} but was not declared as an attacker"
                ));
            };
            // CR 702.22d: All creatures in an attacking band must attack the
            // same player, planeswalker, or battle.
            match band_target {
                None => band_target = Some(target),
                Some(existing) if existing != target => {
                    return Err(format!(
                        "band {band_index} mixes attack targets ({existing:?} vs {target:?})"
                    ));
                }
                _ => {}
            }
            if has_banding(state, member_id) {
                banding_count += 1;
            } else {
                non_banding_count += 1;
            }
        }

        if banding_count == 0 || non_banding_count > 1 {
            if validate_bands_with_other_declaration(state, members).is_ok() {
                continue;
            }
            if banding_count == 0 {
                return Err(format!(
                    "band {band_index} must include at least one banding creature"
                ));
            }
            return Err(format!(
                "band {band_index} has {non_banding_count} non-banding creatures (max 1)"
            ));
        }
    }

    Ok(())
}

fn apply_attack_band_ids(attackers: &mut [AttackerInfo], bands: &[Vec<ObjectId>]) {
    for (band_id, members) in (1u32..).zip(bands.iter()) {
        for attacker in attackers.iter_mut() {
            if members.contains(&attacker.object_id) {
                attacker.band_id = Some(band_id);
            }
        }
    }
}

/// CR 702.22h/i: Once any member of a band is blocked, the whole band is blocked
/// and each member is considered blocked by the union of blockers on any member.
/// Also keeps `blocker_to_attacker` in sync (inverse of `blocker_assignments`).
pub fn propagate_banding_block_state(combat: &mut CombatState) {
    if !combat.attackers.iter().any(|a| a.band_id.is_some()) {
        return;
    }

    let band_ids: HashSet<u32> = combat
        .attackers
        .iter()
        .filter(|a| a.blocked)
        .filter_map(|a| a.band_id)
        .collect();

    for band_id in band_ids {
        let member_ids: Vec<ObjectId> = band_members(combat, band_id)
            .into_iter()
            .map(|a| a.object_id)
            .collect();

        let mut union_blockers = Vec::new();
        for member_id in &member_ids {
            if let Some(blockers) = combat.blocker_assignments.get(member_id) {
                for &blocker_id in blockers {
                    if !union_blockers.contains(&blocker_id) {
                        union_blockers.push(blocker_id);
                    }
                }
            }
        }
        if union_blockers.is_empty() {
            continue;
        }

        for member_id in member_ids {
            if let Some(attacker) = combat
                .attackers
                .iter_mut()
                .find(|a| a.object_id == member_id)
            {
                attacker.blocked = true;
            }
            combat
                .blocker_assignments
                .insert(member_id, union_blockers.clone());
            for &blocker_id in &union_blockers {
                let attackers = combat.blocker_to_attacker.entry(blocker_id).or_default();
                if !attackers.contains(&member_id) {
                    attackers.push(member_id);
                }
            }
        }
    }
}

/// Declare attackers: validate, tap (unless vigilance), populate CombatState, emit event.
/// Accepts per-creature attack targets as (attacker_id, target) pairs.
///
/// CR 702.22b/c: Optional `bands` lists explicit attacking bands chosen by the
/// active player. When empty, no band ids are assigned (banding is inert until
/// `GameAction::DeclareAttackers` grows a band-declaration surface).
pub fn declare_attackers_with_bands(
    state: &mut GameState,
    attacks: &[(ObjectId, AttackTarget)],
    bands: &[Vec<ObjectId>],
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    let attacker_ids: Vec<ObjectId> = attacks.iter().map(|(id, _)| *id).collect();
    validate_attackers(state, &attacker_ids)?;
    // CR 508.1c + CR 508.5: defender-scoped attacker caps ("...attack you each
    // combat") need per-target defending players, which only the full `attacks`
    // slice carries — enforce them here rather than in `validate_attackers`.
    validate_per_defender_attacker_caps(state, attacks)?;
    if !bands.is_empty() {
        validate_attack_band_declarations(state, attacks, bands)?;
    }
    let attackable_players = attackable_player_targets(state);
    // CR 604.1: hoist the combat-restriction existence gates once; every
    // per-permanent / per-attacker static check below reuses them so each loop
    // stays O(N) instead of O(N^2).
    let gates = CombatStaticGates::compute(state);

    // CR 508.1d / CR 701.15b: Creatures that must attack each combat if able.
    // `creature_must_attack` is the single authority for the requirement +
    // exemption logic; this loop only adds the "already declared?" check and
    // the rejection error text.
    for &obj_id in &state.battlefield {
        if !creature_must_attack_with_attackable_players_gated(
            state,
            obj_id,
            &attackable_players,
            &gates,
        ) {
            continue;
        }
        // Already declared as attacker — constraint satisfied
        if attacker_ids.contains(&obj_id) {
            continue;
        }
        // Creature could legally attack but wasn't declared.
        // CR 701.15b: goad-specific error text; CR 508.1d otherwise.
        if !goading_players_for_creature_gated(state, obj_id, gates.has_goad).is_empty() {
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

    // Validate attack targets. CR 805.10a: under the shared team turns
    // option the active team's creatures attack the OTHER team as a group,
    // so a target controlled by/protected by either active-team member
    // (active player or teammate) is ineligible — not just the literal
    // active player.
    let active_team: Vec<PlayerId> = std::iter::once(state.active_player)
        .chain(players::teammates(state, state.active_player))
        .collect();
    for (attacker_id, target) in attacks {
        match target {
            AttackTarget::Player(pid) => {
                if !state.players.iter().any(|p| p.id == *pid)
                    || state.eliminated_players.contains(pid)
                    || active_team.contains(pid)
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
                // Can't attack your own (or, under team turns, your team's) planeswalker
                if active_team.contains(&pw.controller) {
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
                // Under team turns this extends to a teammate's protected battle.
                if battle
                    .protector()
                    .is_some_and(|protector| active_team.contains(&protector))
                {
                    return Err(format!("Protector cannot attack battle {:?}", battle_id));
                }
            }
        }
    }

    // CR 508.1d: Scoped remote CantAttack (Eriette — enchanted creatures can't
    // attack you or planeswalkers you control).
    for (attacker_id, target) in attacks {
        if (gates.has_cant_attack
            && crate::game::static_abilities::check_static_ability(
                state,
                StaticMode::CantAttack,
                &crate::game::static_abilities::StaticCheckContext {
                    target_id: Some(*attacker_id),
                    attack_target: Some(*target),
                    ..Default::default()
                },
            ))
            || (gates.has_cant_attack_or_block
                && crate::game::static_abilities::check_static_ability(
                    state,
                    StaticMode::CantAttackOrBlock,
                    &crate::game::static_abilities::StaticCheckContext {
                        target_id: Some(*attacker_id),
                        attack_target: Some(*target),
                        ..Default::default()
                    },
                ))
        {
            return Err(format!(
                "{attacker_id:?} can't attack {target:?} (CR 508.1d attack restriction)"
            ));
        }
    }

    // CR 508.1c + CR 109.5 + CR 607.2d: The directional attack restriction
    // (Pramikon, Sky Rampart; Mystic Barrier; Teyo, Geometric Tactician): "Each
    // player may attack only the nearest opponent in the [last] chosen direction
    // and planeswalkers controlled by that opponent." Each `AttackOnlyNeighbor`
    // static resolves, per attacker, the neighbor in the source's live chosen
    // direction and rejects any attack not aimed at that opponent or one of
    // their planeswalkers. The restriction is global (affects EVERY player,
    // CR 109.5 "each player"), so it is checked against every attacker regardless
    // of who controls the source. A source with no chosen direction yet is inert
    // (skipped). GAP-4: this deliberately does NOT reuse `TargetFilter::Neighbor`
    // — the direction is a live chosen value read off the source, and the
    // neighbor must be resolved per attacking player, so a static filter cannot
    // express it.
    if gates.has_attack_only_neighbor {
        // CR 508.1c + CR 113.6: collect the functioning directional restrictors
        // once (battlefield + command zone), rather than re-scanning every active
        // static per attacker — O(A + S) instead of O(A × S).
        let restrictors: Vec<_> = super::functioning_abilities::game_functioning_statics(state)
            .filter(|(_, def)| matches!(def.mode, StaticMode::AttackOnlyNeighbor))
            .collect();
        for (attacker_id, target) in attacks {
            let Some(attacker_controller) = state.objects.get(attacker_id).map(|o| o.controller)
            else {
                continue;
            };
            for &(source, _def) in &restrictors {
                // CR 607.2d: no direction chosen yet — the restriction is inert.
                let Some(dir) = source.chosen_direction() else {
                    continue;
                };
                // CR 508.1c + CR 102.2: the legal target is the nearest OPPONENT
                // in the chosen direction — skipping living teammates in team
                // formats (2HG, CR 810), not merely the next seat — and
                // planeswalkers that opponent controls.
                let Some(nearest) =
                    crate::game::players::nearest_opponent(state, attacker_controller, dir)
                else {
                    // No opponent in that direction (e.g. attacker is the sole
                    // survivor) — no restriction target to enforce; inert.
                    continue;
                };
                if !crate::game::restrictions::attack_target_matches_defended_scope(
                    state,
                    Some(target),
                    &crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker,
                    nearest,
                    nearest,
                ) {
                    return Err(format!(
                        "{attacker_id:?} can't attack {target:?} (CR 508.1c: may attack only the nearest opponent in the chosen direction and their planeswalkers)"
                    ));
                }
            }
        }
    }

    // CR 508.1c + CR 109.5: Player-scoped temporary attack prohibitions
    // (`GameRestriction::ProhibitActivity { activity: Attack { defended } }` —
    // Willie Lumpkin: "that player can't attack you or permanents you control
    // during their next turn"). Each restriction defends a specific player (the
    // grant's controller per CR 109.5) against the affected players. Reuse the
    // SAME `attack_target_matches_defended_scope` authority static `CantAttack`
    // uses, so both seams share one scope matcher.
    for (attacker_id, target) in attacks {
        let Some(attacker_controller) = state.objects.get(attacker_id).map(|o| o.controller) else {
            continue;
        };
        for restriction in &state.restrictions {
            let crate::types::ability::GameRestriction::ProhibitActivity {
                source,
                affected_players,
                activity: crate::types::ability::ProhibitedActivity::Attack { defended },
                ..
            } = restriction
            else {
                continue;
            };
            // CR 109.5: the protected player ("you") is the grant's controller.
            let Some(protected) = state.objects.get(source).map(|o| o.controller) else {
                continue;
            };
            // CR 101.2: only the affected players are prohibited. Targeted scopes
            // are resolved to `SpecificPlayer` by `add_restriction` before this
            // gate ever runs.
            let attacker_is_affected = match affected_players {
                crate::types::ability::RestrictionPlayerScope::AllPlayers => true,
                crate::types::ability::RestrictionPlayerScope::SpecificPlayer(p) => {
                    *p == attacker_controller
                }
                crate::types::ability::RestrictionPlayerScope::OpponentsOfSourceController => {
                    attacker_controller != protected
                }
                crate::types::ability::RestrictionPlayerScope::TargetedPlayer
                | crate::types::ability::RestrictionPlayerScope::ParentTargetedPlayer
                | crate::types::ability::RestrictionPlayerScope::DefendingPlayer
                // CR 109.5: resolved to `SpecificPlayer` by `add_restriction` at
                // creation time, so an unresolved scope here restricts no one.
                | crate::types::ability::RestrictionPlayerScope::ScopedPlayer => false,
            };
            if !attacker_is_affected {
                continue;
            }
            // CR 508.5: the defended planeswalker/battle compares on controller,
            // so pass `protected` as both source-controller and source-owner.
            if crate::game::restrictions::attack_target_matches_defended_scope(
                state,
                Some(target),
                defended,
                protected,
                protected,
            ) {
                return Err(format!(
                    "{attacker_id:?} can't attack {target:?} (CR 508.1c player-scoped attack prohibition)"
                ));
            }
        }
    }

    // CR 701.15b: a goaded creature must attack a player other than the goading
    // player *if able*. "Able" is measured against the players this creature
    // could legally be declared attacking: `get_valid_attack_targets` already
    // applies CR 506.2/506.3 plus the exclusions for the active player,
    // eliminated players, teammates, phased-out players, and players with
    // protection from everything. A non-goading player who is not a legal attack
    // target (e.g. a phased-out opponent or a teammate) does not make the
    // creature able to attack elsewhere, so attacking a goading player stays
    // legal. The previous check counted every non-eliminated player, wrongly
    // forcing the creature off a goading player toward a target it could not
    // actually attack.
    for (attacker_id, target) in attacks {
        if let AttackTarget::Player(defending_pid) = target {
            let goading_players =
                goading_players_for_creature_gated(state, *attacker_id, gates.has_goad);
            if goading_players.is_empty() {
                continue;
            }
            // Only enforce the redirect if a non-goading player is actually a
            // legal attack target for this creature.
            if goading_players.contains(defending_pid) {
                let has_attackable_non_goading_target = attackable_players
                    .iter()
                    .any(|pid| !goading_players.contains(pid));
                if has_attackable_non_goading_target {
                    return Err(format!(
                        "{:?} is goaded by {:?} and must attack a different player if able (CR 701.15b)",
                        attacker_id, defending_pid
                    ));
                }
            }
        }
    }

    // CR 508.1b + CR 508.1d: MustAttackPlayer requires attacking the specified
    // player directly when that player is a legal attack target. CR 508.5 maps
    // planeswalker/battle attacks to a defending player for effects that ask for
    // "defending player"; it does not make "attack a planeswalker that player
    // controls" satisfy an effect that specifically says to attack that player.
    for (attacker_id, target) in attacks {
        let Some(obj) = state.objects.get(attacker_id) else {
            continue;
        };
        for req_player in must_attack_players_for_creature(state, obj) {
            // Only enforce when the creature can legally attack the required player.
            if !attackable_players.contains(&req_player) {
                continue;
            }
            let attacks_required_player =
                matches!(target, AttackTarget::Player(pid) if *pid == req_player);
            if !attacks_required_player {
                return Err(format!(
                    "{attacker_id:?} must attack {req_player:?} this combat if able (CR 508.1d)"
                ));
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

    // CR 508.1a + CR 608.2c: Snapshot declaration-time characteristics before
    // later combat/SBA movement can make post-combat "attacked with <quality>"
    // queries chase stale or missing live objects.
    let attacker_declarations: Vec<_> = attacker_ids
        .iter()
        .filter_map(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.snapshot_for_attack_declaration(*id))
        })
        .collect();

    // Populate CombatState with per-creature defending players and attack targets
    let mut attackers: Vec<AttackerInfo> = attacks
        .iter()
        .map(|(object_id, target)| {
            // CR 508.5 + CR 310.8d: Defending player for a battle = its protector,
            // not its controller. For planeswalkers, defending player = controller.
            let defending_player = defending_player_for_target(state, *target);
            AttackerInfo::new(*object_id, *target, defending_player)
        })
        .collect();
    apply_attack_band_ids(&mut attackers, bands);
    let combat = state.combat.get_or_insert_with(CombatState::default);
    combat.attackers = attackers;
    state.players_attacked_this_step = combat
        .attackers
        .iter()
        .map(|a| a.defending_player)
        .collect();
    // CR 508.1k + CR 506.4 + CR 613.1f: A chosen creature becomes attacking and
    // stays attacking until removed from combat or the combat phase ends. Marking
    // layers dirty forces Layer 6 ability-adding effects (CR 613.1f) with
    // FilterProp::Attacking { defender: None } (e.g. Crossway Troublemakers) to re-evaluate now, so
    // the grant is live for the whole combat, not just after damage.
    state.layers_dirty.mark_full();
    let attacker_count = combat.attackers.len();
    let creature_attacked_defenders: Vec<(ObjectId, PlayerId)> = combat
        .attackers
        .iter()
        .map(|attacker| (attacker.object_id, attacker.defending_player))
        .collect();
    combat.attacked_defenders_this_combat.clear();
    combat.creature_attacked_defenders_this_combat.clear();
    for (attacker_id, defending_player) in &creature_attacked_defenders {
        if let Some(attacker) = state.objects.get(attacker_id) {
            combat
                .attacked_defenders_this_combat
                .entry(attacker.controller)
                .or_default()
                .insert(*defending_player);
        }
        combat
            .creature_attacked_defenders_this_combat
            .entry(*attacker_id)
            .or_default()
            .insert(*defending_player);
    }

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

    // CR 508.1a: Record attacker object IDs for per-turn tracking.
    state
        .creatures_attacked_this_turn
        .extend(attacker_ids.iter().copied());
    state
        .attacker_declarations_this_turn
        .extend(attacker_declarations);
    for (attacker_id, defending_player) in creature_attacked_defenders {
        state
            .creature_attacked_defenders_this_turn
            .entry(attacker_id)
            .or_default()
            .insert(defending_player);
    }

    super::restrictions::record_attackers_declared(state, attacker_count);

    Ok(())
}

/// Declare attackers without explicit band declarations.
pub fn declare_attackers(
    state: &mut GameState,
    attacks: &[(ObjectId, AttackTarget)],
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    declare_attackers_with_bands(state, attacks, &[], events)
}

/// CR 701.15b: The set of players that have goaded `creature_id` — both the
/// per-object `goaded_by` designations and any active `StaticMode::Goaded`
/// effects affecting it. This is the single authority for "who goaded this
/// creature"; the AI candidate generator reuses it to build a legal forced
/// attack assignment that avoids each goaded creature's goader.
pub(crate) fn goading_players_for_creature(
    state: &GameState,
    creature_id: ObjectId,
) -> HashSet<PlayerId> {
    goading_players_for_creature_gated(
        state,
        creature_id,
        super::functioning_abilities::any_functioning_static_mode(state, |m| {
            matches!(m, StaticMode::Goaded)
        }),
    )
}

/// Loop-invariant-gated form of [`goading_players_for_creature`].
///
/// CR 701.15b: with no functioning `Goaded` static, only the directly-goaded
/// `goaded_by` set applies, so combat loops that have already hoisted the
/// existence gate pass `has_goad_static = false` to skip the O(N) sweep. When
/// `true`, the exact existing sweep runs unchanged. The gate is computed over
/// `game_functioning_statics` (a superset of `battlefield_active_statics` for
/// `Goaded`), so it never produces a false negative.
pub(crate) fn goading_players_for_creature_gated(
    state: &GameState,
    creature_id: ObjectId,
    has_goad_static: bool,
) -> HashSet<PlayerId> {
    let mut players = state
        .objects
        .get(&creature_id)
        .map(|obj| obj.goaded_by.clone())
        .unwrap_or_default();

    if has_goad_static {
        crate::game::perf_counters::record_static_full_scan();
        for (source, def) in super::functioning_abilities::battlefield_active_statics(state) {
            if def.mode != StaticMode::Goaded {
                continue;
            }
            let Some(affected) = &def.affected else {
                continue;
            };
            let ctx = FilterContext::from_source(state, source.id);
            if matches_target_filter(state, creature_id, affected, &ctx) {
                players.insert(source.controller);
            }
        }
    }

    players
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

    propagate_banding_block_state(combat);

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

fn max_blockers_each_combat(state: &GameState) -> Option<u32> {
    super::functioning_abilities::battlefield_active_statics(state)
        .filter_map(|(_, def)| match def.mode {
            StaticMode::MaxBlockersEachCombat { max } => Some(max),
            _ => None,
        })
        .min()
}

/// CR 509.1h + CR 702.49a: Returns ObjectIds of attackers that were never blocked.
/// Per CR 509.1h, a creature remains blocked for the rest of combat even if all
/// blockers are removed. This function checks the `blocked` flag set at blocker
/// declaration, not the current blocker list.
///
/// CR 506.4 + CR 702.49a + CR 702.190a: Attackers that left the battlefield
/// (destroyed, exiled, bounced, etc.) are excluded — Ninjutsu/Sneak may only
/// return unblocked attackers still on the battlefield.
pub fn unblocked_attackers(state: &GameState) -> Vec<ObjectId> {
    let Some(combat) = &state.combat else {
        return Vec::new();
    };
    combat
        .attackers
        .iter()
        .filter(|a| !a.blocked)
        .filter(|a| is_attacker_in_play(state, a.object_id))
        .map(|a| a.object_id)
        .collect()
}

/// CR 506.5: A creature is attacking alone if it's attacking but no other
/// creatures are. This reads live combat (the sole declared attacker); callers
/// that must survive the attacker leaving combat (CR 506.4) capture the result
/// into the zone-change snapshot at zone-exit per the look-back rule CR 603.10a.
pub fn attacking_alone(state: &GameState, object_id: ObjectId) -> bool {
    state.combat.as_ref().is_some_and(|combat| {
        combat.attackers.len() == 1 && combat.attackers[0].object_id == object_id
    })
}

/// CR 506.5: A creature is blocking alone if it's blocking but no other
/// creatures are. `blocker_to_attacker` is keyed by blocker id (one entry per
/// distinct declared blocker), so a single entry that contains `object_id`
/// means it is the only blocker in combat. Like `attacking_alone`, this reads
/// live combat; look-back callers snapshot the result (CR 603.10a).
pub fn blocking_alone(state: &GameState, object_id: ObjectId) -> bool {
    state.combat.as_ref().is_some_and(|combat| {
        combat.blocker_to_attacker.len() == 1 && combat.blocker_to_attacker.contains_key(&object_id)
    })
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
    // CR 604.1: hoist the combat-restriction existence gates once before the
    // per-permanent scan (collapses O(N^2) to O(N)).
    let gates = CombatStaticGates::compute(state);

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
                    || (gates.has_can_attack_with_defender
                        && crate::game::static_abilities::check_static_ability(
                            state,
                            StaticMode::CanAttackWithDefender,
                            &crate::game::static_abilities::StaticCheckContext {
                                target_id: Some(*id),
                                ..Default::default()
                            },
                        )))
                && !super::functioning_abilities::active_static_definitions(state, obj).any(|sd| {
                    matches!(
                        sd.mode,
                        StaticMode::CantAttack | StaticMode::CantAttackOrBlock
                    ) && sd.attack_defended.is_none()
                })
                // CR 508.1 + CR 101.2 + CR 109.5: remote CantAttack statics
                // (Angelic Arbiter restricting opponents' creatures) resolved via
                // the shared `check_static_ability` building block.
                && !(gates.has_cant_attack
                    && crate::game::static_abilities::check_static_ability(
                        state,
                        StaticMode::CantAttack,
                        &crate::game::static_abilities::StaticCheckContext {
                            target_id: Some(*id),
                            ..Default::default()
                        },
                    ))
                && !(gates.has_cant_attack_or_block
                    && crate::game::static_abilities::check_static_ability(
                        state,
                        StaticMode::CantAttackOrBlock,
                        &crate::game::static_abilities::StaticCheckContext {
                            target_id: Some(*id),
                            ..Default::default()
                        },
                    ))
                // CR 302.6: delegate to the single authority for summoning
                // sickness — folds in Haste at query time without duplicating
                // the flag/keyword logic here.
                && !has_summoning_sickness(obj)
            {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

/// CR 508.1a / CR 509.1a: Rebuild the eligibility snapshot carried by the
/// `DeclareAttackers` / `DeclareBlockers` waiting states from the live game
/// queries. The declare-step waiting payloads are computed exactly once by
/// `turns::auto_advance` when combat enters the step, but a mid-step state
/// mutation (notably debug actions that flip summoning sickness — CR 302.6 —
/// tapped status, or grant/remove Haste/Defender) can change which creatures
/// are legal attackers/blockers. Re-deriving the payload mirrors the
/// `turns.rs` declare-step arms so there is a single authority for the payload
/// shape. A no-op for every non-declaration `WaitingFor` variant.
pub fn refresh_combat_declaration_waiting_for(state: &mut GameState) {
    match &state.waiting_for {
        crate::types::game_state::WaitingFor::DeclareAttackers { .. } => {
            // CR 508.1a: Mirror turns.rs:1369-1370 — rebuild both payload fields.
            let valid_attacker_ids = get_valid_attacker_ids(state);
            let valid_attack_targets = get_valid_attack_targets(state);
            if let crate::types::game_state::WaitingFor::DeclareAttackers {
                valid_attacker_ids: ids,
                valid_attack_targets: targets,
                ..
            } = &mut state.waiting_for
            {
                *ids = valid_attacker_ids;
                *targets = valid_attack_targets;
            }
        }
        crate::types::game_state::WaitingFor::DeclareBlockers { player, .. } => {
            // Copy `player` out before the immutable-borrowing queries below.
            let player = *player;
            // CR 509.1a: Mirror turns.rs:1394-1396 — player-scoped block targets.
            let valid_block_targets = get_valid_block_targets_for_player(state, player);
            let valid_blocker_ids: Vec<_> = valid_block_targets.keys().copied().collect();
            let block_requirements = block_requirements_for_player(state, player);
            if let crate::types::game_state::WaitingFor::DeclareBlockers {
                valid_blocker_ids: ids,
                valid_block_targets: targets,
                block_requirements: reqs,
                ..
            } = &mut state.waiting_for
            {
                *ids = valid_blocker_ids;
                *targets = valid_block_targets;
                *reqs = block_requirements;
            }
        }
        _ => {}
    }
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

    // CR 509.1b + CR 609.4 + CR 702.14c + CR 702.14d: Global
    // IgnoreLandwalkForBlocking statics cancel the landwalk *restriction* for
    // the named qualifier. The keyword itself remains on the attacker
    // (CR 609.4 — "as though" is scoped to this effect). `game_active_statics`
    // (battlefield + command zone) iterates both controllers' statics; the
    // static is global (`affected = None`), so a canceller under either
    // player's control suppresses the qualifier symmetrically.
    let cancelled: HashSet<&str> = super::functioning_abilities::game_active_statics(state)
        .filter_map(|(_src, sd)| match &sd.mode {
            StaticMode::IgnoreLandwalkForBlocking { qualifier: Some(q) } => Some(q.as_str()),
            _ => None,
        })
        .collect();

    #[cfg(debug_assertions)]
    for q in &cancelled {
        debug_assert!(
            !q.is_empty(),
            "IgnoreLandwalkForBlocking qualifier must match Keyword::Landwalk canonical form"
        );
    }

    let qualifiers: Vec<&str> = qualifiers
        .into_iter()
        .filter(|q| !cancelled.contains(q))
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
    let blocker_restriction = collect_blocker_restriction_statics(state);
    let block_restriction = collect_block_restriction_statics(state);
    let blocker_allowed = collect_blocker_allowed_statics(state);
    // CR 604.1: shadow block-lift existence gate (CR 509.1b/609.4/702.28b).
    let can_block_shadow_exists =
        super::functioning_abilities::any_functioning_static_mode(state, |m| {
            matches!(m, StaticMode::CanBlockShadow)
        });
    can_block_pair_with_precomputed(
        state,
        blocker_id,
        attacker_id,
        &blocker_restriction,
        &block_restriction,
        &blocker_allowed,
        can_block_shadow_exists,
    )
}

/// CR 509.1b: Pairwise block legality (restriction checks: can't-block,
/// can't-be-blocked-by, landwalk, horsemanship).
/// Precomputed-slice variant of [`can_block_pair`]: identical legality logic, but
/// the three static scans read from caller-collected slices instead of re-walking
/// the battlefield. Hoist [`collect_blocker_restriction_statics`],
/// [`collect_block_restriction_statics`], and [`collect_blocker_allowed_statics`]
/// once before any loop that calls this per pair.
pub fn can_block_pair_with_precomputed(
    state: &GameState,
    blocker_id: ObjectId,
    attacker_id: ObjectId,
    blocker_restriction: &[(ObjectId, StaticDefinition)],
    block_restriction: &[(ObjectId, StaticDefinition)],
    blocker_allowed: &[(ObjectId, StaticDefinition)],
    can_block_shadow_exists: bool,
) -> bool {
    let Some(blocker) = state.objects.get(&blocker_id) else {
        return false;
    };
    let Some(attacker) = state.objects.get(&attacker_id) else {
        return false;
    };
    if blocker_has_cant_block_static_from_precomputed(state, blocker_id, blocker_restriction) {
        return false;
    }
    // CR 702.147a: Decayed means "This creature can't block."
    if blocker.has_keyword(&Keyword::Decayed) {
        return false;
    }
    // CR 509.1b + CR 301.5a + CR 303.4: scan every battlefield static whose
    // `affected` filter matches the attacker — covers intrinsic, Equipment-
    // granted, and Aura-granted `CantBeBlocked*` uniformly. Mirrors the
    // declare-blockers validation in `validate_blockers_for_player`.
    for (def, src_id) in
        block_restriction_statics_against_from_precomputed(state, attacker_id, block_restriction)
    {
        match &def.mode {
            StaticMode::CantBeBlocked => return false,
            StaticMode::CantBeBlockedExceptBy { kind } => match kind {
                BlockExceptionKind::Quality(target_filter) => {
                    if !matches_target_filter(
                        state,
                        blocker_id,
                        target_filter,
                        &FilterContext::from_source(state, src_id),
                    ) {
                        return false;
                    }
                }
                // CR 509.1b: a count constraint is a multi-blocker check,
                // enforced in validate_blockers, not this per-pair predicate.
                BlockExceptionKind::MinBlockers { .. } => {}
            },
            StaticMode::CantBeBlockedBy { filter }
                if matches_target_filter(
                    state,
                    blocker_id,
                    filter,
                    &FilterContext::from_source(state, src_id),
                ) =>
            {
                return false;
            }
            _ => {}
        }
    }
    if ring_bearer_unblockable_by_greater_power(state, attacker, blocker) {
        return false;
    }
    for kw in &attacker.keywords {
        if let Keyword::Protection(target) = kw {
            if crate::game::keywords::source_matches_protection_target(target, attacker, blocker) {
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
    // CR 509.1b + CR 609.4 + CR 702.28b: a `CanBlockShadow` static lifts the
    // shadow restriction for this blocker (Heartwood Dryad, Wall of Diffusion).
    if attacker_has_shadow
        && !blocker_has_shadow
        && !blocker_can_block_shadow_gated(state, blocker, can_block_shadow_exists)
    {
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
    // CR 509.1b: blocker-side "can block only <filter>" restrictions.
    for (def, src_id) in
        blocker_allowed_statics_for_from_precomputed(state, blocker_id, blocker_allowed)
    {
        let StaticMode::BlockRestriction { filter } = &def.mode else {
            continue;
        };
        if !matches_target_filter(
            state,
            attacker_id,
            filter,
            &FilterContext::from_source(state, src_id),
        ) {
            return false;
        }
    }
    true
}

fn ring_bearer_unblockable_by_greater_power(
    state: &GameState,
    attacker: &GameObject,
    blocker: &GameObject,
) -> bool {
    // CR 701.54c: The Ring emblem says "Your Ring-bearer is legendary and
    // can't be blocked by creatures with greater power."
    super::effects::ring::is_current_ring_bearer(state, attacker.controller, attacker.id)
        && blocker.power.unwrap_or(0) > attacker.power.unwrap_or(0)
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

    // Hoist the three static slices once for the whole O(blockers × attackers)
    // legality sweep instead of re-walking the battlefield per pair.
    let blocker_restriction = collect_blocker_restriction_statics(state);
    let block_restriction = collect_block_restriction_statics(state);
    let blocker_allowed = collect_blocker_allowed_statics(state);
    // CR 604.1: shadow block-lift existence gate (CR 509.1b/609.4/702.28b),
    // hoisted once for the whole O(blockers × attackers) sweep.
    let can_block_shadow_exists =
        super::functioning_abilities::any_functioning_static_mode(state, |m| {
            matches!(m, StaticMode::CanBlockShadow)
        });

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
            .filter(|a| is_attacker_in_play(state, a.object_id))
            .filter(|a| {
                can_block_pair_with_precomputed(
                    state,
                    blocker_id,
                    a.object_id,
                    &blocker_restriction,
                    &block_restriction,
                    &blocker_allowed,
                    can_block_shadow_exists,
                )
            })
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

/// CR 702.111b (Menace) + CR 509.1b ("can't be blocked except by N or more
/// creatures"): the minimum number of creatures that must block this attacker
/// *if it is blocked at all*. Menace imposes a floor of 2; each applicable
/// `MinBlockers { min }` static imposes a floor of `min`; a creature with both
/// requires `max(2, min)`. Returns 1 when no such restriction applies (the
/// trivial case — blocking is then unconstrained in count).
///
/// This is the single authority for the requirement: `validate_blocks` enforces
/// it and `block_requirements_for_player` surfaces it to the UI, so the count a
/// player sees can never disagree with the count the engine enforces.
pub fn min_blockers_required(state: &GameState, attacker_id: ObjectId) -> u32 {
    let block_restriction = collect_block_restriction_statics(state);
    min_blockers_required_from_precomputed(state, attacker_id, &block_restriction)
}

/// CR 702.111b + CR 509.1b: Minimum blockers required (menace floor of 2 and any
/// `MinBlockers` restriction floor).
/// Precomputed-slice variant of [`min_blockers_required`]. Hoist
/// [`collect_block_restriction_statics`] once before any attacker loop.
pub fn min_blockers_required_from_precomputed(
    state: &GameState,
    attacker_id: ObjectId,
    block_restriction: &[(ObjectId, StaticDefinition)],
) -> u32 {
    let mut min = 1;
    if state
        .objects
        .get(&attacker_id)
        .is_some_and(|attacker| attacker.has_keyword(&Keyword::Menace))
    {
        min = min.max(2);
    }
    for (def, _src_id) in
        block_restriction_statics_against_from_precomputed(state, attacker_id, block_restriction)
    {
        if let StaticMode::CantBeBlockedExceptBy {
            kind: BlockExceptionKind::MinBlockers { min: n },
        } = &def.mode
        {
            min = min.max(*n);
        }
    }
    min
}

/// CR 509.1b: The maximum number of creatures that may block `attacker_id`, if
/// any `CantBeBlockedByMoreThan` restriction applies (Stalking Tiger, "can't be
/// blocked by more than N creatures"). When multiple such restrictions apply,
/// the most restrictive (smallest) maximum wins. `None` means unrestricted.
/// This is the inverse of [`min_blockers_required`]; an attacker carrying both a
/// minimum and a maximum must satisfy both.
pub fn max_blockers_allowed(state: &GameState, attacker_id: ObjectId) -> Option<u32> {
    let block_restriction = collect_block_restriction_statics(state);
    max_blockers_allowed_from_precomputed(state, attacker_id, &block_restriction)
}

/// CR 509.1b: Maximum blockers allowed (`CantBeBlockedByMoreThan` restriction).
/// Precomputed-slice variant of [`max_blockers_allowed`]. Hoist
/// [`collect_block_restriction_statics`] once before any attacker loop.
pub fn max_blockers_allowed_from_precomputed(
    state: &GameState,
    attacker_id: ObjectId,
    block_restriction: &[(ObjectId, StaticDefinition)],
) -> Option<u32> {
    block_restriction_statics_against_from_precomputed(state, attacker_id, block_restriction)
        .filter_map(|(def, _src_id)| match def.mode {
            StaticMode::CantBeBlockedByMoreThan { max } => Some(max),
            _ => None,
        })
        .min()
}

/// For one defending player, the per-attacker minimum-blocker requirement for
/// every attacker attacking them that needs more than one blocker. Attackers
/// with the trivial requirement of 1 are omitted so the map carries only the
/// cases the UI needs to surface (menace / "N or more creatures"). Mirrors the
/// shape of `get_valid_block_targets_for_player` for the `DeclareBlockers` state.
pub fn block_requirements_for_player(
    state: &GameState,
    player: PlayerId,
) -> HashMap<ObjectId, u32> {
    let combat = match state.combat.as_ref() {
        Some(c) => c,
        None => return HashMap::new(),
    };
    // Hoist the block-restriction slice once for the O(attackers × battlefield)
    // sweep — invoked from `engine_combat.rs` and `turns.rs` production paths.
    let block_restriction = collect_block_restriction_statics(state);
    combat
        .attackers
        .iter()
        .filter(|a| a.defending_player == player)
        .filter_map(|a| {
            let required =
                min_blockers_required_from_precomputed(state, a.object_id, &block_restriction);
            (required > 1).then_some((a.object_id, required))
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

pub fn defending_player_for_attacker(state: &GameState, attacker: ObjectId) -> Option<PlayerId> {
    state.combat.as_ref()?.attackers.iter().find_map(|info| {
        if info.object_id == attacker {
            Some(info.defending_player)
        } else {
            None
        }
    })
}

/// CR 508.5 + CR 508.5a: Single authority for resolving the defending player a
/// `ControllerRef::DefendingPlayer` reference points at, given the ability's source
/// object. Per CR 508.5, when an ability refers to both an attacking creature and a
/// defending player, the defending player is the one *that attacking creature* is
/// attacking.
///
/// For a creature whose own attack trigger refers to "defending player", the ability
/// source IS the attacker, so [`defending_player_for_attacker`] resolves it directly.
/// For an Equipment, Aura, or any other permanent whose attack trigger references the
/// defending player of a *different* creature (Greatsword of Tyr — "Whenever equipped
/// creature attacks, ... tap up to one target creature defending player controls"), the
/// source is not the attacker; fall back to the attacker carried by the current
/// triggering event and resolve *its* defending player individually (CR 508.5a — the
/// defending player is determined per attacking creature, not as a single batch value).
pub fn resolve_defending_player(state: &GameState, source_id: ObjectId) -> Option<PlayerId> {
    defending_player_for_attacker(state, source_id).or_else(|| {
        crate::game::quantity::triggering_event_source_object(state)
            .and_then(|attacker| defending_player_for_attacker(state, attacker))
    })
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
                && !obj.has_keyword(&Keyword::Decayed)
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

/// CR 506.4: A creature stops being an attacker when it leaves the battlefield
/// or phases out. Attackers that left during the declare-attackers step may
/// remain listed until pruned.
pub fn is_attacker_in_play(state: &GameState, attacker_id: ObjectId) -> bool {
    state.objects.get(&attacker_id).is_some_and(|obj| {
        obj.zone == Zone::Battlefield
            && !obj.is_phased_out()
            && obj.card_types.core_types.contains(&CoreType::Creature)
    })
}

/// CR 508.8: True when at least one declared attacker is still on the battlefield.
pub fn has_attackers_in_play(state: &GameState) -> bool {
    state.combat.as_ref().is_some_and(|combat| {
        combat
            .attackers
            .iter()
            .any(|attacker| is_attacker_in_play(state, attacker.object_id))
    })
}

/// CR 506.4: Drop attackers that are no longer on the battlefield.
pub fn prune_attackers_not_in_play(state: &mut GameState) {
    if let Some(combat) = state.combat.as_ref() {
        let stale: Vec<ObjectId> = combat
            .attackers
            .iter()
            .filter(|attacker| !is_attacker_in_play(state, attacker.object_id))
            .map(|attacker| attacker.object_id)
            .collect();
        for attacker_id in stale {
            super::effects::remove_from_combat::remove_object_from_combat(state, attacker_id);
        }
    }
}

/// Check if the active player controls any creatures that could legally attack.
pub fn has_potential_attackers(state: &GameState) -> bool {
    let active = state.active_player;
    let turn = state.turn_number;
    // CR 604.1: hoist the combat-restriction existence gates once before the
    // per-permanent scan (collapses O(N^2) to O(N)).
    let gates = CombatStaticGates::compute(state);

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
                        || (gates.has_can_attack_with_defender
                            && crate::game::static_abilities::check_static_ability(
                                state,
                                StaticMode::CanAttackWithDefender,
                                &crate::game::static_abilities::StaticCheckContext {
                                    target_id: Some(*id),
                                    ..Default::default()
                                },
                            )))
                    && !super::functioning_abilities::active_static_definitions(state, obj).any(
                        |sd| {
                            matches!(
                                sd.mode,
                                StaticMode::CantAttack | StaticMode::CantAttackOrBlock
                            ) && sd.attack_defended.is_none()
                        },
                    )
                    // CR 508.1 + CR 101.2 + CR 109.5: remote CantAttack statics
                    // (Angelic Arbiter) resolved via `check_static_ability`.
                    && !(gates.has_cant_attack
                        && crate::game::static_abilities::check_static_ability(
                            state,
                            StaticMode::CantAttack,
                            &crate::game::static_abilities::StaticCheckContext {
                                target_id: Some(*id),
                                ..Default::default()
                            },
                        ))
                    && !(gates.has_cant_attack_or_block
                        && crate::game::static_abilities::check_static_ability(
                            state,
                            StaticMode::CantAttackOrBlock,
                            &crate::game::static_abilities::StaticCheckContext {
                                target_id: Some(*id),
                                ..Default::default()
                            },
                        ))
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
    use crate::parser::oracle_static::parse_static_line;
    use crate::types::ability::{
        ChosenAttribute, Comparator, ControllerRef, FilterProp, ObjectScope, PtStat, PtValueScope,
        QuantityExpr, QuantityRef, SeatDirection, StaticCondition, StaticDefinition, TargetFilter,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::format::FormatConfig;
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

    fn create_planeswalker(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Planeswalker);
        id
    }

    fn create_battle(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        protector: PlayerId,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Battle);
        obj.chosen_attributes
            .push(crate::types::ability::ChosenAttribute::Player(protector));
        id
    }

    /// CR 604.1: a restriction-free board of K active-player attackers must NOT
    /// trigger any whole-battlefield `check_static_ability` scan in
    /// `get_valid_attacker_ids` — the `CombatStaticGates` hoist gates every
    /// per-permanent scan off. Reverting the gate makes `static_full_scans`
    /// jump to O(K) (2 scans per vanilla creature here — CantAttack and
    /// CantAttackOrBlock; CanAttackWithDefender is short-circuited by
    /// `!Defender`), failing the `== 0` assertion.
    #[test]
    fn get_valid_attacker_ids_no_static_scan_on_vanilla_board() {
        let mut state = setup();
        let ids: Vec<ObjectId> = (0..8)
            .map(|i| create_creature(&mut state, PlayerId(0), &format!("Bear {i}"), 2, 2))
            .collect();

        crate::game::perf_counters::reset();
        let valid = get_valid_attacker_ids(&state);
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        assert_eq!(valid.len(), ids.len(), "all vanilla creatures can attack");
        assert_eq!(
            scans, 0,
            "no static-ability whole-board scan on a vanilla board"
        );
    }

    /// CR 604.1: `has_potential_attackers` gates the same three per-permanent
    /// scans behind the hoisted existence flags.
    #[test]
    fn has_potential_attackers_no_static_scan_on_vanilla_board() {
        let mut state = setup();
        for i in 0..8 {
            create_creature(&mut state, PlayerId(0), &format!("Bear {i}"), 2, 2);
        }

        crate::game::perf_counters::reset();
        let any = has_potential_attackers(&state);
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        assert!(any, "vanilla untapped creatures are potential attackers");
        assert_eq!(
            scans, 0,
            "no static-ability whole-board scan on a vanilla board"
        );
    }

    /// CR 604.1: declaring K vanilla attackers exercises Sites 2A/2B/2C/2D
    /// (validate_attackers, the must-attack loop, the scoped CantAttack loop and
    /// the goad-redirect loop). With no functioning combat-restriction static,
    /// the single hoisted `CombatStaticGates` sweep gates every per-permanent /
    /// per-attacker `check_static_ability` off, so the declaration costs zero
    /// whole-board scans. Reverting any gate restores O(K) scans.
    #[test]
    fn declare_attackers_no_static_scan_on_vanilla_board() {
        let mut state = setup();
        let ids: Vec<ObjectId> = (0..8)
            .map(|i| create_creature(&mut state, PlayerId(0), &format!("Bear {i}"), 2, 2))
            .collect();
        let attacks: Vec<(ObjectId, AttackTarget)> = ids
            .iter()
            .map(|id| (*id, AttackTarget::Player(PlayerId(1))))
            .collect();

        crate::game::perf_counters::reset();
        let mut events = Vec::new();
        let result = declare_attackers_with_bands(&mut state, &attacks, &[], &mut events);
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        assert!(
            result.is_ok(),
            "declaring vanilla attackers is legal: {result:?}"
        );
        assert_eq!(
            scans, 0,
            "no static-ability whole-board scan on a vanilla declaration"
        );
    }

    /// CR 604.1: the `validate_blockers_for_player` MustBlock loop gates its
    /// per-permanent scan behind a hoisted `any_functioning_static_mode`
    /// existence check, so validating an empty block on a vanilla board costs
    /// zero whole-board scans.
    #[test]
    fn validate_blockers_no_static_scan_on_vanilla_board() {
        let mut state = setup();
        for i in 0..8 {
            create_creature(&mut state, PlayerId(1), &format!("Wall {i}"), 0, 4);
        }

        crate::game::perf_counters::reset();
        let result = validate_blockers_for_player(&state, PlayerId(1), &[]);
        let scans = crate::game::perf_counters::snapshot().static_full_scans;

        assert!(
            result.is_ok(),
            "an empty block is legal with no must-block: {result:?}"
        );
        assert_eq!(
            scans, 0,
            "no static-ability whole-board scan with no MustBlock static"
        );
    }

    #[test]
    fn valid_attacker_succeeds() {
        let mut state = setup();
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        assert!(validate_attackers(&state, &[id]).is_ok());
    }

    #[test]
    fn cant_attack_alone_rejects_sole_attacker() {
        // CR 506.5 + CR 508.1c: a CombatAlone(Attack, NeedsCompanion) creature is
        // illegal as the only attacker, but legal alongside another attacker.
        let mut state = setup();
        let a = create_creature(&mut state, PlayerId(0), "Bonded Construct", 2, 2);
        state
            .objects
            .get_mut(&a)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::NeedsCompanion,
            }));
        let b = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        assert!(validate_attackers(&state, &[a]).is_err());
        assert!(validate_attackers(&state, &[a, b]).is_ok());
    }

    #[test]
    fn can_only_attack_alone_rejects_multi_attacker() {
        // CR 506.5 + CR 508.1c: CombatAlone(Attack, MustBeSole) — the flagged
        // creature can attack alone but is rejected when declared alongside another.
        let mut state = setup();
        let master = create_creature(&mut state, PlayerId(0), "Master of Cruelties", 1, 4);
        state
            .objects
            .get_mut(&master)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::MustBeSole,
            }));
        let companion = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let unflagged = create_creature(&mut state, PlayerId(0), "Elk", 2, 2);

        // Sole attacker: legal.
        assert!(validate_attackers(&state, &[master]).is_ok());
        // With a companion: illegal.
        assert!(validate_attackers(&state, &[master, companion]).is_err());
        // Unflagged creature attacking alone: legal (restriction is not on it).
        assert!(validate_attackers(&state, &[unflagged]).is_ok());
        // Two unflagged creatures: legal.
        assert!(validate_attackers(&state, &[unflagged, companion]).is_ok());
    }

    #[test]
    fn defender_scoped_attacker_cap_limits_attacks_against_controller() {
        // CR 508.1c + CR 508.5 + CR 802.1: Judoon Enforcers — "No more than one
        // creature can attack you each combat." Player 1 controls the static;
        // player 0 (active) may send at most one attacker at player 1.
        let mut state = setup();
        let enforcers = create_creature(&mut state, PlayerId(1), "Judoon Enforcers", 8, 8);
        state
            .objects
            .get_mut(&enforcers)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MaxAttackersEachCombat {
                max: 1,
                defender: Some(AttackDefenderScope::Controller),
            }));
        let a = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let b = create_creature(&mut state, PlayerId(0), "Elk", 2, 2);

        // One attacker against player 1: legal.
        assert!(validate_per_defender_attacker_caps(
            &state,
            &[(a, AttackTarget::Player(PlayerId(1)))]
        )
        .is_ok());
        // Two attackers against player 1: illegal.
        assert!(validate_per_defender_attacker_caps(
            &state,
            &[
                (a, AttackTarget::Player(PlayerId(1))),
                (b, AttackTarget::Player(PlayerId(1))),
            ],
        )
        .is_err());
        // A defender-scoped cap must NOT register as a global per-combat cap —
        // the two enforcement paths are independent (CR 508.1c).
        assert_eq!(max_attackers_each_combat(&state), None);

        // Two attackers against an unprotected player: legal.
        assert!(validate_per_defender_attacker_caps(
            &state,
            &[
                (a, AttackTarget::Player(PlayerId(2))),
                (b, AttackTarget::Player(PlayerId(2))),
            ],
        )
        .is_ok());

        // "Attack you" is a direct-player scope. It does not include attacking a
        // planeswalker controlled by that player.
        let protected_planeswalker = create_planeswalker(&mut state, PlayerId(1), "Jace");
        assert!(validate_per_defender_attacker_caps(
            &state,
            &[
                (a, AttackTarget::Planeswalker(protected_planeswalker)),
                (b, AttackTarget::Planeswalker(protected_planeswalker)),
            ],
        )
        .is_ok());

        // Nor does it include attacking a battle protected by that player.
        let protected_battle =
            create_battle(&mut state, PlayerId(0), "Invasion of Test", PlayerId(1));
        assert!(validate_per_defender_attacker_caps(
            &state,
            &[
                (a, AttackTarget::Battle(protected_battle)),
                (b, AttackTarget::Battle(protected_battle)),
            ],
        )
        .is_ok());
    }

    #[test]
    fn cant_block_alone_rejects_sole_blocker() {
        // CR 506.5 + CR 509.1b: a CombatAlone(Block, NeedsCompanion) creature is
        // illegal as the only blocker, but legal alongside another blocker.
        let mut state = setup();
        let atk1 = create_creature(&mut state, PlayerId(0), "Atk1", 2, 2);
        let atk2 = create_creature(&mut state, PlayerId(0), "Atk2", 2, 2);
        let lone = create_creature(&mut state, PlayerId(1), "Mogg Flunkies", 3, 3);
        state
            .objects
            .get_mut(&lone)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CombatAlone {
                action: CombatAloneAction::Block,
                requirement: CombatAloneRequirement::NeedsCompanion,
            }));
        let other = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(atk1, PlayerId(1)),
                AttackerInfo::attacking_player(atk2, PlayerId(1)),
            ],
            ..Default::default()
        });

        assert!(validate_blockers(&state, &[(lone, atk1)]).is_err());
        assert!(validate_blockers(&state, &[(lone, atk1), (other, atk2)]).is_ok());
    }

    /// CR 508.1 + CR 109.5: Angelic Arbiter — "Each opponent who cast a spell this
    /// turn can't attack with creatures." The remote CantAttack static (with
    /// `affected = opponents' creatures`) must be enforced in combat, gated on the
    /// attacking creature's controller having cast a spell THIS turn. This
    /// discriminates the prior misparse (affected = SelfRef, which never restricted
    /// opponents' creatures, so the post-cast assertion would fail).
    #[test]
    fn angelic_arbiter_attack_lock_only_after_opponent_casts() {
        let mut state = setup();
        // Opponent-controlled (PlayerId(1)) Angelic Arbiter clause on battlefield.
        let arbiter = create_creature(&mut state, PlayerId(1), "Angelic Arbiter", 5, 6);
        let def = parse_static_line(
            "Each opponent who cast a spell this turn can't attack with creatures.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CantAttack);
        state
            .objects
            .get_mut(&arbiter)
            .unwrap()
            .static_definitions
            .push(def);

        // Player 0 (the Arbiter-controller's opponent, and the active player) has an
        // attack-ready creature.
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        // Player 0 has NOT cast a spell this turn -> creature is a valid attacker.
        // On main (SelfRef misparse) this also passes; the discriminator is below.
        assert!(
            get_valid_attacker_ids(&state).contains(&attacker),
            "creature must be a legal attacker before its controller casts a spell"
        );
        assert!(validate_attackers(&state, &[attacker]).is_ok());

        // Record a spell cast by player 0 this turn.
        let spell = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Some Spell".to_string(),
            crate::types::zones::Zone::Stack,
        );
        let spell_obj = state.objects.get(&spell).unwrap().clone();
        crate::game::restrictions::record_spell_cast(
            &mut state,
            PlayerId(0),
            &spell_obj,
            crate::types::game_state::CastingVariant::Normal,
        );

        // Now the remote CantAttack prohibition applies -> creature is excluded and
        // declaration is illegal. On main this assertion FAILS (SelfRef never
        // restricts opponents' creatures).
        assert!(
            !get_valid_attacker_ids(&state).contains(&attacker),
            "after its controller casts a spell, the creature can't attack"
        );
        assert!(validate_attackers(&state, &[attacker]).is_err());
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
    fn walking_bulwark_grants_defender_creature_attack_permission() {
        use crate::game::scenario::GameScenario;
        use crate::types::game_state::WaitingFor;
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::phase::Phase;

        // CR 702.3b + CR 611.2c: The resolved ability grants a targeted
        // defender an until-end-of-turn rule exception that lets it attack.
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bulwark = scenario
            .add_creature_from_oracle(
                PlayerId(0),
                "Walking Bulwark",
                0,
                3,
                "Defender\n{2}: Until end of turn, target creature with defender gains haste, can attack as though it didn't have defender, and assigns combat damage equal to its toughness rather than its power. Activate only as a sorcery.",
            )
            .id();
        let wall = scenario
            .add_creature(PlayerId(0), "Target Wall", 0, 4)
            .defender()
            .with_summoning_sickness()
            .id();
        scenario.with_mana_pool(
            PlayerId(0),
            vec![
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()),
            ],
        );

        let mut runner = scenario.build();
        runner.activate(bulwark, 0).target_object(wall).resolve();

        assert!(
            runner
                .state()
                .objects
                .get(&wall)
                .unwrap()
                .has_keyword(&Keyword::Haste),
            "Walking Bulwark must grant haste to the targeted defender"
        );
        assert!(
            validate_attackers(runner.state(), &[wall]).is_ok(),
            "Walking Bulwark's transient CanAttackWithDefender grant must let the targeted defender attack"
        );

        runner.advance_to_combat();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ));
        runner
            .declare_attackers(&[(wall, AttackTarget::Player(PlayerId(1)))])
            .expect("targeted defender should be legal to declare as an attacker");
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

    /// Issue #428 — CR 302.6 / CR 508.1a: A debug `SetSummoningSickness { sick:
    /// false }` applied while paused at `DeclareAttackers` must refresh the
    /// frozen `valid_attacker_ids` snapshot so the creature becomes a legal
    /// attacker. Drives the real `engine::apply` → `apply_debug_action` →
    /// `refresh_combat_declaration_waiting_for` pipeline. FAILS on pre-fix code:
    /// the snapshot stays stale and the creature is never surfaced.
    #[test]
    fn debug_clear_summoning_sickness_refreshes_valid_attackers() {
        use crate::types::actions::{DebugAction, GameAction};
        use crate::types::game_state::WaitingFor;

        let mut state = setup();
        state.debug_mode = true;
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        state.priority_player = PlayerId(0);
        state.combat = Some(CombatState::default());

        // Summoning-sick creature controlled by the active player.
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().summoning_sick = true;

        // Build the DeclareAttackers waiting state exactly as the engine does
        // (turns.rs:1369-1370): the creature is sick, so the snapshot is empty.
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: get_valid_attacker_ids(&state),
            valid_attack_targets: get_valid_attack_targets(&state),
        };
        match &state.waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids, ..
            } => assert!(
                !valid_attacker_ids.contains(&id),
                "precondition: sick creature must not be a valid attacker yet"
            ),
            other => panic!("expected DeclareAttackers, got {other:?}"),
        }

        // Lift summoning sickness via the debug pipeline.
        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::SetSummoningSickness {
                object_id: id,
                sick: false,
            }),
        )
        .expect("debug SetSummoningSickness should succeed");

        match &result.waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids, ..
            } => assert!(
                valid_attacker_ids.contains(&id),
                "refreshed snapshot must surface the no-longer-sick creature"
            ),
            other => panic!("expected refreshed DeclareAttackers, got {other:?}"),
        }

        // The creature can now actually be declared as an attacker.
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::DeclareAttackers {
                attacks: vec![(id, AttackTarget::Player(PlayerId(1)))],
                bands: vec![],
            },
        )
        .expect("declaring the no-longer-sick creature should succeed");
    }

    /// Issue #428 negative control — CR 302.6 / CR 508.1a: the refresh is
    /// bidirectional. A debug `SetSummoningSickness { sick: true }` on an
    /// otherwise-eligible creature must REMOVE it from the refreshed
    /// `valid_attacker_ids`, proving the refresh re-derives the live snapshot
    /// rather than one-way unlocking.
    #[test]
    fn debug_set_summoning_sickness_removes_from_valid_attackers() {
        use crate::types::actions::{DebugAction, GameAction};
        use crate::types::game_state::WaitingFor;

        let mut state = setup();
        state.debug_mode = true;
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        state.priority_player = PlayerId(0);
        state.combat = Some(CombatState::default());

        // Non-sick, eligible creature (create_creature leaves summoning_sick false).
        let id = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: get_valid_attacker_ids(&state),
            valid_attack_targets: get_valid_attack_targets(&state),
        };
        match &state.waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids, ..
            } => assert!(
                valid_attacker_ids.contains(&id),
                "precondition: eligible creature must be a valid attacker"
            ),
            other => panic!("expected DeclareAttackers, got {other:?}"),
        }

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::SetSummoningSickness {
                object_id: id,
                sick: true,
            }),
        )
        .expect("debug SetSummoningSickness should succeed");

        match &result.waiting_for {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids, ..
            } => assert!(
                !valid_attacker_ids.contains(&id),
                "refreshed snapshot must drop the now-sick creature"
            ),
            other => panic!("expected refreshed DeclareAttackers, got {other:?}"),
        }
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

    /// CR 702.22b/c: Bands are only assigned when explicitly declared.
    #[test]
    fn declare_attackers_does_not_auto_band_banding_creatures() {
        let mut state = setup();
        let hero = create_creature(&mut state, PlayerId(0), "Hero", 1, 1);
        let pegasus = create_creature(&mut state, PlayerId(0), "Pegasus", 1, 1);
        for id in [hero, pegasus] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .keywords
                .push(Keyword::Banding);
        }
        let target = AttackTarget::Player(PlayerId(1));
        declare_attackers(
            &mut state,
            &[(hero, target), (pegasus, target)],
            &mut Vec::new(),
        )
        .unwrap();
        let combat = state.combat.as_ref().unwrap();
        assert!(combat.attackers.iter().all(|a| a.band_id.is_none()));
    }

    #[test]
    fn explicit_band_declaration_assigns_shared_band_id() {
        let mut state = setup();
        let hero = create_creature(&mut state, PlayerId(0), "Hero", 1, 1);
        let pegasus = create_creature(&mut state, PlayerId(0), "Pegasus", 1, 1);
        for id in [hero, pegasus] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .keywords
                .push(Keyword::Banding);
        }
        let target = AttackTarget::Player(PlayerId(1));
        declare_attackers_with_bands(
            &mut state,
            &[(hero, target), (pegasus, target)],
            &[vec![hero, pegasus]],
            &mut Vec::new(),
        )
        .unwrap();
        let combat = state.combat.as_ref().unwrap();
        assert_eq!(
            combat.attackers[0].band_id, combat.attackers[1].band_id,
            "explicitly declared band must share an id"
        );
        assert_eq!(combat.attackers[0].band_id, Some(1));
    }

    fn add_wolf_subtype(state: &mut GameState, id: ObjectId) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .subtypes
            .push("Wolf".to_string());
    }

    fn grant_bands_with_other_wolves(state: &mut GameState, id: ObjectId) {
        add_wolf_subtype(state, id);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::BandsWithOther("Wolf".to_string()));
    }

    #[test]
    fn bands_with_other_declaration_assigns_shared_band_id() {
        let mut state = setup();
        let wolf_a = create_creature(&mut state, PlayerId(0), "Wolf A", 2, 2);
        let wolf_b = create_creature(&mut state, PlayerId(0), "Wolf B", 2, 2);
        grant_bands_with_other_wolves(&mut state, wolf_a);
        add_wolf_subtype(&mut state, wolf_b);

        let target = AttackTarget::Player(PlayerId(1));
        declare_attackers_with_bands(
            &mut state,
            &[(wolf_a, target), (wolf_b, target)],
            &[vec![wolf_a, wolf_b]],
            &mut Vec::new(),
        )
        .unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert_eq!(combat.attackers[0].band_id, Some(1));
        assert_eq!(combat.attackers[1].band_id, Some(1));
    }

    #[test]
    fn bands_with_other_declaration_rejects_mixed_quality() {
        let mut state = setup();
        let wolf = create_creature(&mut state, PlayerId(0), "Wolf", 2, 2);
        let bear = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        grant_bands_with_other_wolves(&mut state, wolf);
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .keywords
            .push(Keyword::BandsWithOther("Wolf".to_string()));

        let target = AttackTarget::Player(PlayerId(1));
        let err = declare_attackers_with_bands(
            &mut state,
            &[(wolf, target), (bear, target)],
            &[vec![wolf, bear]],
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            err.contains("banding creature"),
            "expected mixed-quality bands-with-other declaration to fail, got: {err}"
        );
    }

    #[test]
    fn band_declaration_rejects_two_non_banding_members() {
        let mut state = setup();
        let hero = create_creature(&mut state, PlayerId(0), "Hero", 1, 1);
        let bear = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let soldier = create_creature(&mut state, PlayerId(0), "Soldier", 1, 1);
        state
            .objects
            .get_mut(&hero)
            .unwrap()
            .keywords
            .push(Keyword::Banding);
        let target = AttackTarget::Player(PlayerId(1));
        let err = declare_attackers_with_bands(
            &mut state,
            &[(hero, target), (bear, target), (soldier, target)],
            &[vec![hero, bear, soldier]],
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            err.contains("non-banding"),
            "expected CR 702.22c violation, got: {err}"
        );
    }

    /// CR 702.22h/i: blocking one band member blocks the whole band.
    #[test]
    fn banding_block_propagates_to_all_members() {
        let mut state = setup();
        let hero = create_creature(&mut state, PlayerId(0), "Hero", 1, 1);
        let pegasus = create_creature(&mut state, PlayerId(0), "Pegasus", 1, 1);
        let blocker = create_creature(&mut state, PlayerId(1), "Soldier", 1, 1);
        for id in [hero, pegasus] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .keywords
                .push(Keyword::Banding);
        }
        let target = AttackTarget::Player(PlayerId(1));
        declare_attackers_with_bands(
            &mut state,
            &[(hero, target), (pegasus, target)],
            &[vec![hero, pegasus]],
            &mut Vec::new(),
        )
        .unwrap();
        declare_blockers(&mut state, &[(blocker, hero)], &mut Vec::new()).unwrap();
        let combat = state.combat.as_ref().unwrap();
        assert!(combat.attackers.iter().all(|a| a.blocked));
        assert_eq!(
            combat.blocker_assignments.get(&pegasus),
            combat.blocker_assignments.get(&hero),
            "blockers on one band member must propagate to the band"
        );
        assert!(
            combat
                .blocker_to_attacker
                .get(&blocker)
                .is_some_and(|attackers| attackers.contains(&hero) && attackers.contains(&pegasus)),
            "blocker_to_attacker must list every propagated band member"
        );
    }

    #[test]
    fn bands_with_other_block_propagates_to_all_members() {
        let mut state = setup();
        let wolf_a = create_creature(&mut state, PlayerId(0), "Wolf A", 2, 2);
        let wolf_b = create_creature(&mut state, PlayerId(0), "Wolf B", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Soldier", 1, 1);
        grant_bands_with_other_wolves(&mut state, wolf_a);
        grant_bands_with_other_wolves(&mut state, wolf_b);

        let target = AttackTarget::Player(PlayerId(1));
        declare_attackers_with_bands(
            &mut state,
            &[(wolf_a, target), (wolf_b, target)],
            &[vec![wolf_a, wolf_b]],
            &mut Vec::new(),
        )
        .unwrap();
        declare_blockers(&mut state, &[(blocker, wolf_a)], &mut Vec::new()).unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert!(combat.attackers.iter().all(|a| a.blocked));
        assert_eq!(
            combat.blocker_assignments.get(&wolf_b),
            combat.blocker_assignments.get(&wolf_a)
        );
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

    /// CR 509.1b + CR 609.4 + CR 702.14c: Ur-Drago's static cancels the swampwalk
    /// blocking restriction. Attacker with swampwalk + defender controls a Swamp +
    /// a permanent emitting `IgnoreLandwalkForBlocking(Swamp)` => blockable.
    #[test]
    fn swampwalk_cancelled_by_ur_drago_static() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Swampwalker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Swamp".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _swamp = create_land(&mut state, PlayerId(1), "Swamp", &["Swamp"], &[]);
        // Place an Ur-Drago-like permanent on the defender's side emitting the static.
        let ur_drago = create_creature(&mut state, PlayerId(1), "Ur-Drago", 4, 4);
        state
            .objects
            .get_mut(&ur_drago)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(
                StaticMode::IgnoreLandwalkForBlocking {
                    qualifier: Some("Swamp".to_string()),
                },
            ));

        assert!(can_block_pair(&state, blocker, attacker));
    }

    /// CR 702.14d: A swampwalk canceller leaves an unrelated islandwalk intact.
    #[test]
    fn islandwalk_unaffected_by_swamp_canceller() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Islandwalker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Island".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _island = create_land(&mut state, PlayerId(1), "Island", &["Island"], &[]);
        let canceller = create_creature(&mut state, PlayerId(1), "Ur-Drago", 4, 4);
        state
            .objects
            .get_mut(&canceller)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(
                StaticMode::IgnoreLandwalkForBlocking {
                    qualifier: Some("Swamp".to_string()),
                },
            ));

        assert!(!can_block_pair(&state, blocker, attacker));
    }

    /// CR 702.14d: Qualifiers cancel independently — an attacker with both
    /// swampwalk and islandwalk only loses the cancelled qualifier; the other
    /// landwalk path remains active.
    #[test]
    fn multi_qualifier_attacker_preserves_other_landwalks() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Dual Walker", 2, 2);
        let kws = &mut state.objects.get_mut(&attacker).unwrap().keywords;
        kws.push(Keyword::Landwalk("Swamp".to_string()));
        kws.push(Keyword::Landwalk("Island".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _swamp = create_land(&mut state, PlayerId(1), "Swamp", &["Swamp"], &[]);
        let _island = create_land(&mut state, PlayerId(1), "Island", &["Island"], &[]);
        let canceller = create_creature(&mut state, PlayerId(1), "Ur-Drago", 4, 4);
        state
            .objects
            .get_mut(&canceller)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(
                StaticMode::IgnoreLandwalkForBlocking {
                    qualifier: Some("Swamp".to_string()),
                },
            ));

        // Islandwalk path still active => attacker is unblockable.
        assert!(!can_block_pair(&state, blocker, attacker));
    }

    /// CR 509.1b + CR 609.4: The static is global (`affected = None`); a canceller
    /// on the ATTACKING player's side still suppresses the restriction.
    #[test]
    fn ur_drago_canceller_controller_independence() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Swampwalker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Landwalk("Swamp".to_string()));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let _swamp = create_land(&mut state, PlayerId(1), "Swamp", &["Swamp"], &[]);
        // Canceller is on the ATTACKING side, not defender.
        let canceller = create_creature(&mut state, PlayerId(0), "Ur-Drago", 4, 4);
        state
            .objects
            .get_mut(&canceller)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(
                StaticMode::IgnoreLandwalkForBlocking {
                    qualifier: Some("Swamp".to_string()),
                },
            ));

        // Cancellation still applies regardless of controller.
        assert!(can_block_pair(&state, blocker, attacker));
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
    fn declare_attackers_records_defenders_per_attacking_creature() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.combat = Some(CombatState::default());
        let angel = create_creature(&mut state, PlayerId(0), "Angel of Destiny", 2, 6);
        let bear = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let mut events = Vec::new();
        declare_attackers(
            &mut state,
            &[
                (angel, AttackTarget::Player(PlayerId(1))),
                (bear, AttackTarget::Player(PlayerId(2))),
            ],
            &mut events,
        )
        .unwrap();

        assert!(state.creature_attacked_player_this_turn(angel, PlayerId(1)));
        assert!(!state.creature_attacked_player_this_turn(angel, PlayerId(2)));
        assert!(state.creature_attacked_player_this_turn(bear, PlayerId(2)));
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

    /// CR 805.10a/b: "Each team's creatures attack the other team as a
    /// group... The active team has one combined attack." A creature
    /// controlled by the active player's teammate must be declarable as an
    /// attacker in the SAME declaration as the active player's own
    /// creatures, and neither may target the attacking team's own players.
    #[test]
    fn declare_attackers_two_headed_giant_combines_teammates_creatures() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.combat = Some(CombatState::default());
        let active_bear = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let teammate_wolf = create_creature(&mut state, PlayerId(1), "Wolf", 3, 3);

        let mut events = Vec::new();
        declare_attackers_with_bands(
            &mut state,
            &[
                (active_bear, AttackTarget::Player(PlayerId(2))),
                (teammate_wolf, AttackTarget::Player(PlayerId(3))),
            ],
            &[],
            &mut events,
        )
        .unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert_eq!(combat.attackers.len(), 2);

        // CR 805.10a: can't attack your own team (teammate as the target).
        let mut blocked_state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        blocked_state.turn_number = 2;
        blocked_state.active_player = PlayerId(0);
        blocked_state.combat = Some(CombatState::default());
        let bear2 = create_creature(&mut blocked_state, PlayerId(0), "Bear", 2, 2);
        let mut events2 = Vec::new();
        let err = declare_attackers_with_bands(
            &mut blocked_state,
            &[(bear2, AttackTarget::Player(PlayerId(1)))],
            &[],
            &mut events2,
        )
        .unwrap_err();
        assert!(err.contains("cannot attack player"), "err={err}");
    }

    /// CR 805.10d: "Creatures controlled by the defending players can block
    /// creatures attacking any player on the defending team." A creature
    /// controlled by one defending teammate may block an attacker that is
    /// attacking the OTHER defending teammate.
    #[test]
    fn declare_blockers_two_headed_giant_teammate_blocks_attack_on_other_teammate() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(2);
        // Attacker controlled by the active team (P2) is attacking P0; the
        // defending team is P0 + P1.
        let attacker = create_creature(&mut state, PlayerId(2), "Bear", 2, 2);
        let teammate_blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(0))],
            ..Default::default()
        });

        let mut events = Vec::new();
        // P1 (not the player being attacked, but P0's teammate) declares the block.
        declare_blockers_for_player(
            &mut state,
            PlayerId(1),
            &[(teammate_blocker, attacker)],
            &mut events,
        )
        .unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&attacker],
            vec![teammate_blocker]
        );
    }

    /// CR 506.5: the sole declared attacker is "attacking alone"; a co-attacker
    /// makes neither attacker alone.
    #[test]
    fn attacking_alone_authority() {
        let mut state = setup();
        let solo = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(solo, PlayerId(1))],
            ..Default::default()
        });
        assert!(attacking_alone(&state, solo));

        let other = create_creature(&mut state, PlayerId(0), "Wolf", 3, 3);
        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(solo, PlayerId(1)),
                AttackerInfo::attacking_player(other, PlayerId(1)),
            ],
            ..Default::default()
        });
        assert!(!attacking_alone(&state, solo));
        assert!(!attacking_alone(&state, other));
    }

    /// CR 506.5: the sole declared blocker is "blocking alone"; a co-blocker
    /// makes neither blocker alone.
    #[test]
    fn blocking_alone_authority() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let solo = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        let mut events = Vec::new();
        declare_blockers(&mut state, &[(solo, attacker)], &mut events).unwrap();
        assert!(blocking_alone(&state, solo));

        let second = create_creature(&mut state, PlayerId(1), "Guard", 1, 3);
        let mut events = Vec::new();
        declare_blockers(&mut state, &[(second, attacker)], &mut events).unwrap();
        assert!(!blocking_alone(&state, solo));
        assert!(!blocking_alone(&state, second));
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
    fn has_attackers_in_play_false_when_attacker_left_battlefield() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        assert!(has_attackers_in_play(&state));

        state.objects.get_mut(&attacker).unwrap().zone = Zone::Graveyard;
        assert!(
            !has_attackers_in_play(&state),
            "attackers in the graveyard must not count as in play (#1555)"
        );

        prune_attackers_not_in_play(&mut state);
        assert!(state.combat.as_ref().unwrap().attackers.is_empty());
    }

    #[test]
    fn unblocked_attackers_excludes_attackers_not_in_play() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        assert_eq!(unblocked_attackers(&state), vec![attacker]);

        state.objects.get_mut(&attacker).unwrap().zone = Zone::Graveyard;
        assert!(
            unblocked_attackers(&state).is_empty(),
            "dead attackers must not be returnable for Ninjutsu/Sneak (#1319)"
        );
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

    /// CR 509.1b + CR 609.4 + CR 702.28b: Heartwood Dryad / Wall of Diffusion —
    /// a non-shadow creature with `CanBlockShadow` may block a shadow attacker.
    /// Discriminating: an identical non-shadow creature WITHOUT the static cannot.
    #[test]
    fn can_block_shadow_static_lets_non_shadow_block_shadow() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Shadow A", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Shadow);

        // Plain non-shadow blocker: cannot block the shadow attacker.
        let plain_blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        assert!(validate_blockers(&state, &[(plain_blocker, attacker)]).is_err());
        assert!(!can_block_pair(&state, plain_blocker, attacker));

        // Non-shadow blocker with CanBlockShadow: now allowed by both seams.
        let dryad = create_creature(&mut state, PlayerId(1), "Heartwood Dryad", 2, 2);
        state
            .objects
            .get_mut(&dryad)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CanBlockShadow).affected(TargetFilter::SelfRef),
            );
        assert!(validate_blockers(&state, &[(dryad, attacker)]).is_ok());
        assert!(can_block_pair(&state, dryad, attacker));

        // A source whose affected filter matches another creature also grants
        // that creature the permission; the parser accepts subject-scoped
        // variants, so runtime must honor `affected` rather than only checking
        // the blocker's own statics.
        let standard_bearer = create_creature(&mut state, PlayerId(1), "Shadow Standard", 1, 1);
        state
            .objects
            .get_mut(&standard_bearer)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CanBlockShadow).affected(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
            );
        assert!(validate_blockers(&state, &[(plain_blocker, attacker)]).is_ok());
        assert!(can_block_pair(&state, plain_blocker, attacker));

        // The permission does NOT make the Dryad itself blockable-by-anything or
        // change the non-shadow attacker symmetry: a shadow blocker still can't
        // block a non-shadow attacker (the other CR 702.28b half is untouched).
        let normal_attacker = create_creature(&mut state, PlayerId(0), "Grizzly", 2, 2);
        let shadow_blocker = create_creature(&mut state, PlayerId(1), "Shadow B", 2, 2);
        state
            .objects
            .get_mut(&shadow_blocker)
            .unwrap()
            .keywords
            .push(Keyword::Shadow);
        assert!(validate_blockers(&state, &[(shadow_blocker, normal_attacker)]).is_err());
    }

    /// CR 604.1 + CR 509.1b/609.4/702.28b: with NO functioning `CanBlockShadow`
    /// static anywhere, the hoisted existence gate short-circuits every
    /// per-blocker shadow scan, so a shadow attacker facing K non-shadow blockers
    /// costs ZERO `blocker_can_block_shadow` full-body executions while remaining
    /// byte-identical (no non-shadow creature can block the shadow attacker).
    /// Reverting the gate restores the O(K) per-blocker `check_static_ability`
    /// sweep, flipping the `combat_shadow_block_scans == 0` assertion to K.
    #[test]
    fn get_valid_block_targets_no_shadow_scan_when_no_can_block_shadow_static() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Shadow Strider", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Shadow);

        // K=8 non-shadow defending creatures (PlayerId(1)).
        for i in 0..8 {
            create_creature(&mut state, PlayerId(1), &format!("Bear {i}"), 2, 2);
        }

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        crate::game::perf_counters::reset();
        let targets = get_valid_block_targets(&state);
        let scans = crate::game::perf_counters::snapshot().combat_shadow_block_scans;

        assert_eq!(
            scans, 0,
            "no CanBlockShadow static exists, so the gate must skip every per-blocker shadow scan"
        );
        assert!(
            targets.is_empty(),
            "no non-shadow creature can legally block the shadow attacker; got {targets:?}"
        );
    }

    /// CR 604.1 + CR 509.1b/609.4/702.28b equivalence: when a functioning
    /// `CanBlockShadow` static DOES exist, the gate is true and the full
    /// predicate runs, so the affected non-shadow creature is a legal blocker for
    /// the shadow attacker while plain non-shadow creatures are not. This proves
    /// the gate-true path preserves the shadow-lift (the gate is a pure
    /// existence short-circuit, not a behavior change).
    #[test]
    fn get_valid_block_targets_honors_can_block_shadow_static() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Shadow Strider", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Shadow);

        let plain = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let dryad = create_creature(&mut state, PlayerId(1), "Heartwood Dryad", 2, 2);
        state
            .objects
            .get_mut(&dryad)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CanBlockShadow).affected(TargetFilter::SelfRef),
            );

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        let targets = get_valid_block_targets(&state);
        assert_eq!(
            targets.get(&dryad).map(Vec::as_slice),
            Some([attacker].as_slice()),
            "the CanBlockShadow creature must be able to block the shadow attacker"
        );
        assert!(
            !targets.contains_key(&plain),
            "a plain non-shadow creature still cannot block the shadow attacker; got {targets:?}"
        );
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
    fn kappa_cannoneer_artifact_enter_trigger_makes_it_unblockable() {
        use crate::game::scenario::GameScenario;
        use crate::game::triggers::process_triggers;
        use crate::game::zones::move_to_zone;
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;
        use crate::types::zones::Zone;

        let mut scenario = GameScenario::new();
        let cannoneer = scenario
            .add_creature_from_oracle(
                PlayerId(0),
                "Kappa Cannoneer",
                4,
                4,
                "Improvise\nWard {4}\nWhenever this creature or another artifact you control enters, put a +1/+1 counter on this creature. It can't be blocked this turn.",
            )
            .as_artifact()
            .id();
        let artifact = scenario
            .add_creature_to_hand(PlayerId(0), "Servo", 1, 1)
            .as_artifact()
            .id();
        let blocker = scenario.add_creature(PlayerId(1), "Blocker", 2, 2).id();

        let mut runner = scenario.build();
        let mut events = Vec::new();
        move_to_zone(runner.state_mut(), artifact, Zone::Battlefield, &mut events);
        process_triggers(runner.state_mut(), &events);
        assert!(
            runner.state().stack.len() == 1,
            "artifact ETB should put Kappa Cannoneer's trigger on the stack"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("active player should pass priority");
        runner
            .act(GameAction::PassPriority)
            .expect("nonactive player should pass priority");

        assert_eq!(
            runner
                .state()
                .objects
                .get(&cannoneer)
                .unwrap()
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&1),
            "Kappa Cannoneer's trigger should put the counter on itself"
        );
        assert!(
            validate_blockers(runner.state(), &[(blocker, cannoneer)]).is_err(),
            "Kappa Cannoneer's resolved trigger should make it unblockable this turn"
        );
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

    /// CR 509.1b + CR 613.4c: Recipient-relative conditions on Equipment
    /// statics evaluate against the equipped attacker, not the Equipment.
    #[test]
    fn equipment_granted_cant_be_blocked_condition_reads_attacker_power() {
        use crate::types::ability::{
            Comparator, FilterProp, ObjectScope, QuantityExpr, QuantityRef, StaticCondition,
            StaticDefinition, TargetFilter, TypedFilter,
        };

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Small Rogue", 3, 3);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let tools = create_creature(&mut state, PlayerId(0), "Thieves' Tools", 0, 0);
        let tools_obj = state.objects.get_mut(&tools).unwrap();
        tools_obj.attached_to = Some(attacker.into());
        tools_obj.static_definitions.push(
            StaticDefinition::new(StaticMode::CantBeBlocked)
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
                ))
                .condition(StaticCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Recipient,
                        },
                    },
                    comparator: Comparator::LE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }),
        );

        assert!(!can_block_pair(&state, blocker, attacker));

        let attacker_obj = state.objects.get_mut(&attacker).unwrap();
        attacker_obj.power = Some(4);
        attacker_obj.base_power = Some(4);
        assert!(can_block_pair(&state, blocker, attacker));
    }

    /// CR 509.1b + CR 506.2 + CR 108.3: An Aura-style `CantBeBlocked` gated on
    /// `Not(RecipientAttackingOwnerTarget { OwnerOrPlaneswalker })` (Become the
    /// Pilot) — attached to a creature OWNED by B but CONTROLLED by A. The
    /// creature is unblockable when attacking anyone except its owner B (or a
    /// permanent B controls). Exercises the owner-vs-controller distinction.
    #[test]
    fn cant_be_blocked_unless_attacking_owner_reads_owner_not_controller() {
        use crate::types::ability::{
            FilterProp, StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
        };
        use crate::types::triggers::AttackTargetFilter;

        // Player B (1) owns the creature; player A (0) controls it (donated).
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(1), "Donated Beater", 4, 4);
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.controller = PlayerId(0); // owner B, controller A
        }
        // Blocker controlled by the defending player (the creature's owner B).
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        // Aura granting the conditional-evasion static, attached to the attacker.
        let aura = create_creature(&mut state, PlayerId(0), "Become the Pilot", 0, 0);
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.attached_to = Some(attacker.into());
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantBeBlocked)
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .condition(StaticCondition::Not {
                        condition: Box::new(StaticCondition::RecipientAttackingOwnerTarget {
                            target: AttackTargetFilter::OwnerOrPlaneswalker,
                        }),
                    }),
            );
        }

        // 1) Attacking a third party (defending player A's own ally seat is N/A
        //    in 2p, so attack a player who is NOT the owner): unblockable.
        //    Owner is B(1); attack player A(0) (not the owner) → exception not met
        //    → Not(false) = true → CantBeBlocked active.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(0))],
            ..Default::default()
        });
        assert!(
            !can_block_pair(&state, blocker, attacker),
            "attacking a non-owner player → unblockable"
        );

        // 2) Attacking its OWNER (player B = 1): exception met → Not(true) = false
        //    → static inactive → blockable.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        assert!(
            can_block_pair(&state, blocker, attacker),
            "attacking its owner → blockable"
        );

        // 3) Attacking a planeswalker CONTROLLED BY THE OWNER (B): "a permanent
        //    its owner controls" → exception met → blockable.
        let owner_pw = create_planeswalker(&mut state, PlayerId(1), "Owner's Walker");
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(owner_pw),
                PlayerId(1),
            )],
            ..Default::default()
        });
        assert!(
            can_block_pair(&state, blocker, attacker),
            "attacking a planeswalker its owner controls → blockable"
        );

        // 4) Attacking a planeswalker controlled by the CONTROLLER (A, not the
        //    owner): exception NOT met → still unblockable. Guards the
        //    owner-vs-controller distinction (CR 108.3).
        let controller_pw = create_planeswalker(&mut state, PlayerId(0), "Controller's Walker");
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(controller_pw),
                PlayerId(0),
            )],
            ..Default::default()
        });
        assert!(
            !can_block_pair(&state, blocker, attacker),
            "attacking a planeswalker the CONTROLLER (not owner) controls → unblockable"
        );
    }

    /// CR 509.1b + CR 506.2: The bare `Owner` parameter only matches a direct
    /// attack on the owning player, not a permanent the owner controls.
    #[test]
    fn cant_be_blocked_unless_attacking_owner_bare_player_only() {
        use crate::types::ability::{
            FilterProp, StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
        };
        use crate::types::triggers::AttackTargetFilter;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(1), "Donated Beater", 4, 4);
        state.objects.get_mut(&attacker).unwrap().controller = PlayerId(0);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let aura = create_creature(&mut state, PlayerId(0), "Bare Owner Aura", 0, 0);
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.attached_to = Some(attacker.into());
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantBeBlocked)
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .condition(StaticCondition::Not {
                        condition: Box::new(StaticCondition::RecipientAttackingOwnerTarget {
                            target: AttackTargetFilter::Owner,
                        }),
                    }),
            );
        }

        // Attacking the owner directly → blockable.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        assert!(can_block_pair(&state, blocker, attacker));

        // Attacking a planeswalker the owner controls → bare Owner does NOT
        // match a permanent → exception not met → still unblockable.
        let owner_pw = create_planeswalker(&mut state, PlayerId(1), "Owner's Walker");
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(owner_pw),
                PlayerId(1),
            )],
            ..Default::default()
        });
        assert!(
            !can_block_pair(&state, blocker, attacker),
            "bare Owner must not match owner-controlled permanents"
        );
    }

    /// CR 509.1b: No combat / not-an-attacker → the positive condition is
    /// `false`, so the `Not` makes the creature unblockable (defensive default).
    #[test]
    fn cant_be_blocked_unless_attacking_owner_no_combat_is_unblockable() {
        use crate::types::ability::{
            FilterProp, StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
        };
        use crate::types::triggers::AttackTargetFilter;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(1), "Donated Beater", 4, 4);
        state.objects.get_mut(&attacker).unwrap().controller = PlayerId(0);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let aura = create_creature(&mut state, PlayerId(0), "Evasion Aura", 0, 0);
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.attached_to = Some(attacker.into());
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantBeBlocked)
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .condition(StaticCondition::Not {
                        condition: Box::new(StaticCondition::RecipientAttackingOwnerTarget {
                            target: AttackTargetFilter::OwnerOrPlaneswalker,
                        }),
                    }),
            );
        }

        // No combat state at all → not attacking → exception not met → unblockable.
        assert!(state.combat.is_none());
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

    /// CR 509.1b + CR 105.4 + CR 609.6 (issue #327): Skrelv-class — when a
    /// creature is granted `CantBeBlockedBy { IsChosenColor }` via a
    /// transient continuous effect whose source has a `ChosenAttribute::Color`,
    /// the layer system resolves IsChosenColor → HasColor(<chosen>) at
    /// apply-time. A blocker whose colors contain the chosen color is
    /// prohibited; a blocker of a different color is allowed.
    ///
    /// Exercises the full apply-time pipeline: a granting source (Skrelv
    /// stand-in) with `ChosenAttribute::Color(Red)`, an
    /// `AddStaticMode { CantBeBlockedBy { IsChosenColor } }` transient
    /// continuous effect aimed at the granted creature, and
    /// `evaluate_layers` running the resolver. The runtime block check
    /// then reads the post-resolution filter via
    /// `block_restriction_statics_against`.
    #[test]
    fn granted_cant_be_blocked_by_chosen_color_resolves_at_apply_time() {
        use crate::types::ability::{
            ChosenAttribute, ContinuousModification, Duration, FilterProp, TargetFilter,
            TypedFilter,
        };
        use crate::types::mana::ManaColor;
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let source = create_creature(&mut state, PlayerId(0), "Granting Source", 1, 1);
        let granted = create_creature(&mut state, PlayerId(0), "Target Creature", 2, 2);
        let red_blocker = create_creature(&mut state, PlayerId(1), "Red Bear", 2, 2);
        let blue_blocker = create_creature(&mut state, PlayerId(1), "Blue Wizard", 2, 2);
        state
            .objects
            .get_mut(&red_blocker)
            .unwrap()
            .color
            .push(ManaColor::Red);
        state
            .objects
            .get_mut(&blue_blocker)
            .unwrap()
            .color
            .push(ManaColor::Blue);

        // Persist the chosen color on the granting source — this is what the
        // `Choose a color` resolver stores on the activating permanent.
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));

        // Register the transient continuous effect that grants
        // `AddStaticMode { CantBeBlockedBy { IsChosenColor } }` to the target.
        // This is exactly what `effect::resolve` produces when Skrelv's third
        // sub-ability resolves on its chosen creature target.
        let unresolved_mode = StaticMode::CantBeBlockedBy {
            filter: TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::IsChosenColor]),
            ),
        };
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: granted },
            vec![ContinuousModification::AddStaticMode {
                mode: unresolved_mode,
            }],
            None,
        );

        // Run the layer system. The `AddStaticMode` arm in
        // `apply_continuous_effect` resolves IsChosenColor → HasColor(Red)
        // using the source's chosen_attributes and pushes the resolved
        // static_def onto the granted creature.
        crate::game::layers::evaluate_layers(&mut state);

        // Red blocker matches the chosen color → block is illegal.
        assert!(
            validate_blockers(&state, &[(red_blocker, granted)]).is_err(),
            "red blocker should be prohibited (chosen color = red)"
        );
        assert!(
            !can_block_pair(&state, red_blocker, granted),
            "red blocker should not be able to block"
        );
        // Blue blocker does NOT match — block is legal.
        assert!(
            can_block_pair(&state, blue_blocker, granted),
            "blue blocker should be able to block (color differs from chosen)"
        );
    }

    /// CR 702.16 + CR 105.4 (issue #4371): Mother of Runes — when a creature is
    /// granted `Protection(ChosenColor)` via a transient continuous effect whose
    /// source carries a `ChosenAttribute::Color`, the layer applier bakes
    /// `Protection(ChosenColor)` → `Protection(Color(<chosen>))` at apply-time
    /// (layers.rs). The high-level `protection_prevents_from` query then prevents
    /// a source of the chosen color and allows a source of any other color.
    /// This proves the runtime half of the #4371 fix: the parser injects a
    /// `Choose(Color)` ahead of the grant so this `chosen_color` is populated.
    #[test]
    fn granted_protection_from_chosen_color_bakes_in_at_apply_time() {
        use crate::types::ability::{
            ChosenAttribute, ContinuousModification, Duration, TargetFilter,
        };
        use crate::types::keywords::{Keyword, ProtectionTarget};
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let source = create_creature(&mut state, PlayerId(0), "Mother of Runes", 1, 1);
        let granted = create_creature(&mut state, PlayerId(0), "Protected Creature", 2, 2);
        let red_source = create_creature(&mut state, PlayerId(1), "Red Source", 2, 2);
        let blue_source = create_creature(&mut state, PlayerId(1), "Blue Source", 2, 2);
        state
            .objects
            .get_mut(&red_source)
            .unwrap()
            .color
            .push(ManaColor::Red);
        state
            .objects
            .get_mut(&blue_source)
            .unwrap()
            .color
            .push(ManaColor::Blue);

        // The `Choose a color` resolver stores the chosen color on the granting
        // source (Mother of Runes). Issue #4371's parser fix injects that choice.
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));

        // Grant `Protection(ChosenColor)` to the target — exactly what the
        // injected grant sub-ability produces when it resolves.
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: granted },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::ChosenColor),
            }],
            None,
        );

        crate::game::layers::evaluate_layers(&mut state);

        let granted_obj = state.objects.get(&granted).unwrap();
        let red_obj = state.objects.get(&red_source).unwrap();
        let blue_obj = state.objects.get(&blue_source).unwrap();
        // Red source matches the chosen color → protection prevents it.
        assert!(
            crate::game::keywords::protection_prevents_from(granted_obj, red_obj),
            "protection from chosen color (red) should prevent a red source"
        );
        // Blue source differs → not prevented.
        assert!(
            !crate::game::keywords::protection_prevents_from(granted_obj, blue_obj),
            "protection from chosen color (red) should NOT prevent a blue source"
        );
    }

    /// CR 607.2d + CR 613.1 + CR 702.16 (issue #4371): end-to-end production
    /// path for Mother of Runes. Unlike `granted_protection_from_chosen_color_
    /// bakes_in_at_apply_time` (which hand-seeds the chosen color via
    /// `chosen_attributes.push`), this drives the REAL runtime: parse the Oracle
    /// text, activate the `{T}` ability, target the creature, resolve, then
    /// answer the injected `Choose(Color)` through the actual `ChooseOption`
    /// action. The injected choice must persist (`persist: true`) so the resolver
    /// stores its `source_id`; answering it routes through `bind_named_choice`,
    /// which writes `ChosenAttribute::Color` onto Mother of Runes and re-runs
    /// layers — baking `Protection(ChosenColor)` → `Protection(Color(White))` on
    /// the target. With `persist: false` the source carried no color and the
    /// grant was a silent no-op (the bug this test guards against).
    #[test]
    fn mother_of_runes_chosen_color_protection_resolves_through_choose_option() {
        use crate::game::scenario::GameScenario;
        use crate::types::ability::ChoiceType;
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::mana::ManaColor;
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mother = scenario
            .add_creature_from_oracle(
                PlayerId(0),
                "Mother of Runes",
                1,
                1,
                "{T}: Target creature you control gains protection from the color of your choice until end of turn.",
            )
            .id();
        let granted = scenario
            .add_creature(PlayerId(0), "Protected Creature", 2, 2)
            .id();
        let white_source = scenario
            .add_creature(PlayerId(1), "White Source", 2, 2)
            .id();
        let blue_source = scenario.add_creature(PlayerId(1), "Blue Source", 2, 2).id();

        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&white_source)
            .unwrap()
            .color
            .push(ManaColor::White);
        runner
            .state_mut()
            .objects
            .get_mut(&blue_source)
            .unwrap()
            .color
            .push(ManaColor::Blue);

        // Activate + target + resolve up to the injected color choice. The
        // resolution driver does not answer `NamedChoice`, so it stops there,
        // leaving `runner` parked on the prompt for the manual answer below.
        runner.activate(mother, 0).target_object(granted).resolve();

        assert!(
            matches!(
                &runner.state().waiting_for,
                WaitingFor::NamedChoice {
                    choice_type: ChoiceType::Color { .. },
                    source_id: Some(id),
                    ..
                } if *id == mother
            ),
            "resolving Mother of Runes' ability must pause on a persisted color \
             choice keyed to the granting source, got {:?}",
            runner.state().waiting_for
        );

        // Answer the prompt through the real production action.
        runner
            .act(GameAction::ChooseOption {
                choice: "White".to_string(),
            })
            .expect("choosing a color for the protection grant must be accepted");

        let granted_obj = runner.state().objects.get(&granted).unwrap();
        let white_obj = runner.state().objects.get(&white_source).unwrap();
        let blue_obj = runner.state().objects.get(&blue_source).unwrap();
        // The chosen color (white) is now baked in → a white source is prevented.
        assert!(
            crate::game::keywords::protection_prevents_from(granted_obj, white_obj),
            "after answering the color choice with White, the target must have \
             effective protection from white"
        );
        // An off-color (blue) source is unaffected.
        assert!(
            !crate::game::keywords::protection_prevents_from(granted_obj, blue_obj),
            "protection from the chosen color (white) must NOT prevent a blue source"
        );
    }

    #[test]
    fn source_power_block_restriction_scopes_to_attackers_you_control() {
        let mut state = setup();
        let champion = create_creature(&mut state, PlayerId(0), "Champion", 3, 3);
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 1, 1);
        let other_attacker = create_creature(&mut state, PlayerId(1), "Other Attacker", 1, 1);
        let small_blocker = create_creature(&mut state, PlayerId(1), "Small Blocker", 2, 2);
        let large_blocker = create_creature(&mut state, PlayerId(1), "Large Blocker", 4, 4);

        state
            .objects
            .get_mut(&champion)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantBeBlockedBy {
                    filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                        FilterProp::PtComparison {
                            stat: PtStat::Power,
                            scope: PtValueScope::Current,
                            comparator: Comparator::LT,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: ObjectScope::Source,
                                },
                            },
                        },
                    ])),
                })
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
            );

        assert!(
            !can_block_pair(&state, small_blocker, attacker),
            "blockers with power less than the source's power cannot block creatures its controller controls"
        );
        assert!(
            can_block_pair(&state, large_blocker, attacker),
            "blockers with power at least the source's power remain legal"
        );
        assert!(
            can_block_pair(&state, small_blocker, other_attacker),
            "the restriction only protects creatures controlled by the static source controller"
        );
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

    /// CR 508.1d: Eriette of the Charmed Apple — enchanted creatures may attack
    /// other players but not Eriette's player or planeswalkers.
    #[test]
    fn eriette_scoped_cant_attack_blocks_only_defender_scope() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        state.active_player = PlayerId(1);
        state.turn_number = 2;

        let eriette = create_creature(
            &mut state,
            PlayerId(0),
            "Eriette of the Charmed Apple",
            2,
            4,
        );
        let static_line = "Each creature that's enchanted by an Aura you control can't attack you or planeswalkers you control.";
        let def = parse_static_line(static_line).expect(static_line);
        assert_eq!(def.mode, StaticMode::CantAttack);
        assert_eq!(
            def.attack_defended.as_ref(),
            Some(&crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker)
        );
        state
            .objects
            .get_mut(&eriette)
            .unwrap()
            .static_definitions
            .push(def);

        let attacker = create_creature(&mut state, PlayerId(1), "Enchanted Goblin", 2, 1);
        let aura = create_creature(&mut state, PlayerId(0), "Pacifism", 0, 0);
        {
            let aura_obj = state.objects.get_mut(&aura).unwrap();
            aura_obj.card_types.core_types.push(CoreType::Enchantment);
            aura_obj.card_types.subtypes.push("Aura".into());
            aura_obj.attached_to = Some(attacker.into());
        }
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .attachments
            .push(aura);

        let pw_card = CardId(state.next_object_id);
        let pw = create_object(
            &mut state,
            pw_card,
            PlayerId(0),
            "Jace".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let pw_obj = state.objects.get_mut(&pw).unwrap();
        pw_obj.card_types.core_types.push(CoreType::Planeswalker);

        assert!(
            get_valid_attacker_ids(&state).contains(&attacker),
            "scoped restriction must not remove attacker from eligibility"
        );

        let mut other_player_state = state.clone();
        let mut events = Vec::new();
        assert!(
            declare_attackers(
                &mut other_player_state,
                &[(attacker, AttackTarget::Player(PlayerId(2)))],
                &mut events
            )
            .is_ok(),
            "enchanted creature must still be able to attack another player"
        );
        events.clear();
        assert!(
            declare_attackers(
                &mut state,
                &[(attacker, AttackTarget::Player(PlayerId(0)))],
                &mut events
            )
            .is_err(),
            "must not attack Eriette's controller"
        );
        events.clear();
        assert!(
            declare_attackers(
                &mut state,
                &[(attacker, AttackTarget::Planeswalker(pw))],
                &mut events
            )
            .is_err(),
            "must not attack Eriette's planeswalker"
        );

        {
            let attacker_obj = state.objects.get_mut(&attacker).unwrap();
            attacker_obj.attachments.retain(|&id| id != aura);
        }
        state.objects.get_mut(&aura).unwrap().attached_to = None;
        events.clear();
        assert!(
            declare_attackers(
                &mut state,
                &[(attacker, AttackTarget::Player(PlayerId(0)))],
                &mut events
            )
            .is_ok(),
            "without qualifying aura attachment, attack is legal"
        );
    }

    /// CR 508.1c: Build a directional attack-restriction source (Pramikon-style)
    /// controlled by `controller`, with `AttackOnlyNeighbor` static and the given
    /// chosen direction persisted on it. Mirrors how the parser + choose hijack
    /// wire the real card at runtime.
    fn create_directional_restrictor(
        state: &mut GameState,
        controller: PlayerId,
        direction: SeatDirection,
    ) -> ObjectId {
        let id = create_creature(state, controller, "Pramikon, Sky Rampart", 1, 5);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::AttackOnlyNeighbor));
        obj.chosen_attributes
            .push(ChosenAttribute::Direction(direction));
        id
    }

    /// CR 508.1c + CR 102.2 + CR 810: In Two-Headed Giant the seat adjacent to
    /// the attacker can be a TEAMMATE. The directional restriction must resolve
    /// the nearest OPPONENT (skipping teammates), not merely the next seat —
    /// otherwise the real legal target (the next opponent) is rejected and no
    /// legal attack exists. Regression for the team-format finding: P0's left
    /// neighbor P1 is a teammate; the nearest opponent is P2.
    #[test]
    fn attack_only_neighbor_skips_teammate_in_two_headed_giant() {
        // 2HG teams {P0,P1} and {P2,P3}, seat order [P0,P1,P2,P3].
        let mut state = GameState::new(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.phase = crate::types::phase::Phase::DeclareAttackers;

        let _pramikon = create_directional_restrictor(&mut state, PlayerId(0), SeatDirection::Left);
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        // The nearest opponent past teammate P1 is P2 — legal. Before the fix the
        // gate required attacking teammate P1 (an illegal target), leaving P0 with
        // no legal attack at all.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(2)))],
                &mut vec![]
            )
            .is_ok(),
            "attacking the nearest opponent P2 (past teammate P1) must be legal"
        );
        // The far opponent P3 is not the nearest in the Left direction — illegal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(3)))],
                &mut vec![]
            )
            .is_err(),
            "attacking the far opponent P3 must be rejected (P2 is the nearest)"
        );
    }

    /// CR 508.1c + CR 607.2d: Under Pramikon (chosen Left), the active player may
    /// attack only the nearest opponent to their left (P0's left = P1 in seat
    /// order [P0,P1,P2,P3]) and that opponent's planeswalkers. Attacking any
    /// other player, or another player's planeswalker, is illegal. Revert gate →
    /// the illegal non-neighbor attack succeeds, failing the `is_err` assertions.
    #[test]
    fn attack_only_neighbor_left_restricts_to_left_neighbor_and_their_planeswalkers() {
        let mut state = setup_multiplayer_combat(4);
        // Restrictor controlled by P0; chosen direction Left. P0's left = P1.
        let _pramikon = create_directional_restrictor(&mut state, PlayerId(0), SeatDirection::Left);

        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let neighbor_pw = create_planeswalker(&mut state, PlayerId(1), "Jace");
        let far_pw = create_planeswalker(&mut state, PlayerId(2), "Chandra");

        // Neighbor (P1) — legal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(1)))],
                &mut vec![]
            )
            .is_ok(),
            "attacking the left neighbor P1 is legal"
        );
        // Neighbor's planeswalker — legal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Planeswalker(neighbor_pw))],
                &mut vec![]
            )
            .is_ok(),
            "attacking the left neighbor's planeswalker is legal"
        );
        // Non-neighbor player (P2) — illegal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(2)))],
                &mut vec![]
            )
            .is_err(),
            "attacking a non-neighbor player must be rejected"
        );
        // Non-neighbor's planeswalker — illegal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Planeswalker(far_pw))],
                &mut vec![]
            )
            .is_err(),
            "attacking a non-neighbor's planeswalker must be rejected"
        );
    }

    /// CR 508.1c + CR 109.5: "Each player" — the restriction is global. P1's
    /// attack (a non-active demonstration via active_player swap) against a
    /// non-neighbor is rejected even though P0 controls the Pramikon. P1's left
    /// neighbor in [P0,P1,P2,P3] is P2; attacking P3 is illegal.
    #[test]
    fn attack_only_neighbor_is_global_across_all_players() {
        let mut state = setup_multiplayer_combat(4);
        let _pramikon = create_directional_restrictor(&mut state, PlayerId(0), SeatDirection::Left);
        state.active_player = PlayerId(1);

        let p1_attacker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        // P1's left neighbor is P2 → legal; P3 is not → illegal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(p1_attacker, AttackTarget::Player(PlayerId(2)))],
                &mut vec![]
            )
            .is_ok(),
            "P1 attacking its own left neighbor P2 is legal under the global restriction"
        );
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(p1_attacker, AttackTarget::Player(PlayerId(3)))],
                &mut vec![]
            )
            .is_err(),
            "P1 attacking a non-neighbor P3 is illegal under P0's global Pramikon"
        );
    }

    /// CR 607.2d: A restrictor with no chosen direction yet is inert — any attack
    /// is legal until a direction is chosen. Revert the `chosen_direction()`
    /// skip → this would wrongly reject.
    #[test]
    fn attack_only_neighbor_no_direction_is_inert() {
        let mut state = setup_multiplayer_combat(4);
        // Restrictor WITHOUT a chosen direction.
        let id = create_creature(&mut state, PlayerId(0), "Pramikon, Sky Rampart", 1, 5);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::AttackOnlyNeighbor));

        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        assert!(
            declare_attackers(
                &mut state,
                &[(attacker, AttackTarget::Player(PlayerId(2)))],
                &mut vec![]
            )
            .is_ok(),
            "with no direction chosen the restriction is inert"
        );
    }

    /// CR 102.2 collapse: in a two-player game the only opponent is always the
    /// neighbor in either direction, so the restriction never forbids anything.
    #[test]
    fn attack_only_neighbor_two_player_collapse_allows_any_attack() {
        let mut state = setup();
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        let _pramikon = create_directional_restrictor(&mut state, PlayerId(0), SeatDirection::Left);
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        assert!(
            declare_attackers(
                &mut state,
                &[(attacker, AttackTarget::Player(PlayerId(1)))],
                &mut vec![]
            )
            .is_ok(),
            "the sole opponent is always the neighbor, so any attack is legal"
        );
    }

    /// CR 607.2d "the last chosen direction": a re-choice flips legality. After
    /// re-choosing Right, P0's neighbor becomes P3 (previous in seat order), so
    /// the previously-legal P1 attack becomes illegal and the P3 attack legal.
    /// Exercises the replace-on-rechoose clear in `choose.rs` via the runtime
    /// `bind_named_choice` path.
    #[test]
    fn attack_only_neighbor_rechoice_flips_legality() {
        use crate::game::effects::choose::bind_named_choice;
        use crate::types::ability::ChoiceType;

        let mut state = setup_multiplayer_combat(4);
        let pramikon = create_directional_restrictor(&mut state, PlayerId(0), SeatDirection::Left);
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        // Initially Left → P1 legal, P3 illegal.
        assert!(declare_attackers(
            &mut state.clone(),
            &[(attacker, AttackTarget::Player(PlayerId(1)))],
            &mut vec![]
        )
        .is_ok());

        // Re-choose Right via the runtime binding authority.
        let labeled = ChoiceType::Labeled {
            options: vec!["Left".into(), "Right".into()],
        };
        bind_named_choice(&mut state, &labeled, "Right", Some(pramikon));

        // Exactly one Direction persists, and it is Right.
        assert_eq!(
            state.objects.get(&pramikon).unwrap().chosen_direction(),
            Some(SeatDirection::Right),
            "re-choice must replace the prior direction"
        );

        // Now Right → P0's neighbor is P3. P1 illegal, P3 legal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(1)))],
                &mut vec![]
            )
            .is_err(),
            "after flipping to Right, the old left neighbor P1 is illegal"
        );
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(3)))],
                &mut vec![]
            )
            .is_ok(),
            "after flipping to Right, the right neighbor P3 is legal"
        );
    }

    /// CR 508.1c + CR 611.2a/c + CR 607.2d: RUNTIME test for Teyo, Geometric
    /// Tactician's [−2] duration-bound grant — the seam a parser-shape test
    /// cannot reach. Teyo's [−2] resolves an `Effect::GenericEffect` that grants
    /// `StaticMode::AttackOnlyNeighbor` to itself until its controller's next
    /// turn. This test drives the full runtime chain, NOT the AST shape:
    ///
    /// 1. Install the grant through the SAME production storage the effect
    ///    resolver uses: `effect::register_transient_effect`'s `SelfRef` branch
    ///    calls `add_transient_continuous_effect` with a `SpecificObject{self}`
    ///    filter, a `GrantStaticAbility(AttackOnlyNeighbor)` modification, and the
    ///    `UntilNextTurnOf{Controller}` duration. We call that same function with
    ///    the same arguments, then run the real `evaluate_layers` so the granted
    ///    static is materialized onto Teyo's `static_definitions` and surfaced by
    ///    `battlefield_active_statics` (CR 611.2c object-set fixing).
    /// 2. Bind Teyo's direction through the production `bind_named_choice`
    ///    authority (source = Teyo) — the SAME {Left,Right} hijack the printed
    ///    card uses — so `Teyo.chosen_direction()` is set, NOT hand-pushed.
    /// 3. Assert the combat gate READS the layer-granted static: with Left chosen,
    ///    the non-neighbor attack via real `declare_attackers` is rejected and the
    ///    neighbor attack is legal (proves the granted static reaches the gate).
    /// 4. Assert EXPIRY: prune P0's `UntilNextTurnOf{Controller}` grant via the
    ///    production `prune_until_next_turn_effects` (the exact call the untap step
    ///    makes at the controller's next turn), re-evaluate layers, and confirm the
    ///    same non-neighbor attack is now legal — the restriction is gone.
    ///
    /// REVERT-FAILING: without the grant+layer wiring, the granted static never
    /// lands on Teyo and step 3's `is_err` flips to `is_ok`; without the duration
    /// prune the step-4 `is_ok` flips to `is_err`.
    #[test]
    fn teyo_minus_two_runtime_grant_gates_attacks_and_expires() {
        use crate::game::effects::choose::bind_named_choice;
        use crate::game::layers::evaluate_layers;
        use crate::types::ability::{ChoiceType, ContinuousModification, Duration, PlayerScope};

        let mut state = setup_multiplayer_combat(4);

        // Teyo, a planeswalker P0 controls. Its [−2] granted the directional
        // restriction to itself; no static is printed on Teyo — the grant will be
        // materialized by the layer system from the transient continuous effect.
        let teyo = create_planeswalker(&mut state, PlayerId(0), "Teyo, Geometric Tactician");

        // Step 1: install the grant exactly as `effect::register_transient_effect`
        // (SelfRef branch) does when Teyo's parsed [−2] `GenericEffect` resolves:
        // a `SpecificObject{self}`-affected transient continuous effect carrying a
        // `GrantStaticAbility(AttackOnlyNeighbor)` modification, bounded by the
        // `UntilNextTurnOf{Controller}` duration `try_parse_temporary_attack_only_neighbor`
        // attaches. This is the identical grant the [−2] `GenericEffect` produces.
        state.add_transient_continuous_effect(
            teyo,
            PlayerId(0),
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            },
            TargetFilter::SpecificObject { id: teyo },
            vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(StaticDefinition::new(StaticMode::AttackOnlyNeighbor)),
            }],
            None,
        );
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        // The granted static must now be present on Teyo (materialized by layer 6).
        assert!(
            crate::game::functioning_abilities::battlefield_active_statics(&state)
                .any(|(src, def)| src.id == teyo
                    && matches!(def.mode, StaticMode::AttackOnlyNeighbor)),
            "the [−2] grant must land on Teyo and be surfaced by battlefield_active_statics"
        );

        // Step 2: bind Teyo's direction through the production choice authority.
        let labeled = ChoiceType::Labeled {
            options: vec!["Left".into(), "Right".into()],
        };
        bind_named_choice(&mut state, &labeled, "Left", Some(teyo));
        assert_eq!(
            state.objects.get(&teyo).unwrap().chosen_direction(),
            Some(SeatDirection::Left),
            "bind_named_choice must set Teyo's chosen direction (source = Teyo)"
        );

        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        // Step 3: the gate reads the layer-granted static. P0's left neighbor is
        // P1; attacking P1 is legal, attacking non-neighbor P2 is rejected.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(1)))],
                &mut vec![]
            )
            .is_ok(),
            "neighbor attack is legal under the layer-granted Teyo restriction"
        );
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(2)))],
                &mut vec![]
            )
            .is_err(),
            "non-neighbor attack must be rejected while Teyo's grant is active"
        );

        // Step 4: expiry. Prune P0's `UntilNextTurnOf{Controller}` transient via
        // the production path the untap step runs at the controller's next turn.
        crate::game::layers::prune_until_next_turn_effects(&mut state, PlayerId(0));
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        // The grant is gone → the granted static no longer functions on Teyo.
        assert!(
            !crate::game::functioning_abilities::battlefield_active_statics(&state)
                .any(|(src, def)| src.id == teyo
                    && matches!(def.mode, StaticMode::AttackOnlyNeighbor)),
            "after the controller's next turn, the [−2] grant must be pruned off Teyo"
        );
        // The same previously-illegal non-neighbor attack is now legal.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(2)))],
                &mut vec![]
            )
            .is_ok(),
            "after expiry the directional restriction is gone; non-neighbor attack is legal"
        );
    }

    /// CR 508.1c: two restrictors intersect — an attack must satisfy EVERY
    /// functioning restriction. P0's Pramikon (Left → neighbor P1) and P2's
    /// Pramikon (Left → P2's neighbor P3) both apply globally. For P0's attacker,
    /// only P1 satisfies P0's restriction; but P2's restriction demands P0 attack
    /// P2's-neighbor... no — P2's restriction constrains attacks relative to the
    /// attacker's own left neighbor (P0's left = P1). Both restrictors resolve the
    /// neighbor per the ATTACKER (P0), so both demand P1. The attack on P1 is
    /// legal; an attack on P2 fails the first restrictor.
    #[test]
    fn attack_only_neighbor_two_sources_intersect() {
        let mut state = setup_multiplayer_combat(4);
        let _a = create_directional_restrictor(&mut state, PlayerId(0), SeatDirection::Left);
        let _b = create_directional_restrictor(&mut state, PlayerId(2), SeatDirection::Left);
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        // Both restrictors resolve the neighbor per the attacking player P0 → P1.
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(1)))],
                &mut vec![]
            )
            .is_ok(),
            "P1 satisfies both restrictors"
        );
        assert!(
            declare_attackers(
                &mut state.clone(),
                &[(attacker, AttackTarget::Player(PlayerId(2)))],
                &mut vec![]
            )
            .is_err(),
            "P2 violates both restrictors"
        );
    }

    /// Issue #2015: Predatory Impetus — `MustBeBlocked` on the Aura with
    /// `EnchantedBy` affected must reach declare-blockers enforcement.
    #[test]
    fn aura_must_be_blocked_on_enchanted_creature_reaches_enforcement() {
        use crate::types::ability::{FilterProp, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Enchanted Beast", 3, 3);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let aura = create_creature(&mut state, PlayerId(0), "Predatory Impetus", 0, 0);
        let aura_obj = state.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types.push(CoreType::Enchantment);
        aura_obj.attached_to = Some(attacker.into());
        aura_obj.static_definitions.push(
            StaticDefinition::new(StaticMode::MustBeBlocked).affected(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            )),
        );

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        assert!(
            validate_blockers(&state, &[]).is_err(),
            "enchanted attacker with a legal blocker must be blocked (CR 509.1c)"
        );
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
    fn affected_cant_block_static_checks_recipient_condition() {
        let mut state = setup();
        let source = create_creature(&mut state, PlayerId(0), "Unleash Grant", 3, 3);
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 2, 2);

        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantBlock)
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent),
                    ))
                    .condition(StaticCondition::RecipientHasCounters {
                        counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                        minimum: 1,
                        maximum: None,
                    }),
            );
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        assert!(
            can_block_pair(&state, blocker, attacker),
            "recipient-gated CantBlock should stay dormant before the blocker has a counter"
        );

        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        assert!(
            !can_block_pair(&state, blocker, attacker),
            "recipient-gated CantBlock should apply to the affected blocker with a +1/+1 counter"
        );
        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_err());
    }

    #[test]
    fn decayed_creature_cant_block() {
        let mut state = setup();
        state.combat = Some(CombatState::default());
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Decayed Zombie", 2, 2);
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .keywords
            .push(Keyword::Decayed);
        state
            .combat
            .as_mut()
            .unwrap()
            .attackers
            .push(AttackerInfo::new(
                attacker,
                AttackTarget::Player(PlayerId(1)),
                PlayerId(1),
            ));

        assert!(!get_valid_blocker_ids(&state).contains(&blocker));
        assert!(!can_block_pair(&state, blocker, attacker));
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

    #[test]
    fn protection_from_artifacts_prevents_artifact_creature_blocking() {
        use crate::types::card_type::CoreType;
        use crate::types::keywords::ProtectionTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Protected Attacker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::CardType(
                "artifacts".to_string(),
            )));

        let artifact_blocker = create_creature(&mut state, PlayerId(1), "Artifact Blocker", 1, 1);
        state
            .objects
            .get_mut(&artifact_blocker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        assert!(!can_block_pair(&state, artifact_blocker, attacker));
        assert!(validate_blockers(&state, &[(artifact_blocker, attacker)]).is_err());
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
    fn brave_the_sands_extra_blockers_grant_tracks_controller() {
        use crate::game::layers::evaluate_layers;
        use crate::parser::oracle_static::parse_static_line_multi;

        let mut state = setup();
        let brave_card_id = CardId(state.next_object_id);
        let brave = create_object(
            &mut state,
            brave_card_id,
            PlayerId(1),
            "Brave the Sands".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        for def in parse_static_line_multi(
            "Each creature you control can block an additional creature each combat.",
        ) {
            let obj = state.objects.get_mut(&brave).unwrap();
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def.clone());
            obj.static_definitions.push(def);
        }

        let p0_attacker1 = create_creature(&mut state, PlayerId(0), "P0 Bear A", 2, 2);
        let p0_attacker2 = create_creature(&mut state, PlayerId(0), "P0 Bear B", 2, 2);
        let p1_attacker1 = create_creature(&mut state, PlayerId(1), "P1 Bear A", 2, 2);
        let p1_attacker2 = create_creature(&mut state, PlayerId(1), "P1 Bear B", 2, 2);
        let p1_blocker = create_creature(&mut state, PlayerId(1), "P1 Guard", 1, 4);
        let p0_blocker = create_creature(&mut state, PlayerId(0), "P0 Guard", 1, 4);

        evaluate_layers(&mut state);

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(p0_attacker1, PlayerId(1)),
                AttackerInfo::attacking_player(p0_attacker2, PlayerId(1)),
            ],
            ..Default::default()
        });
        assert!(
            validate_blockers(
                &state,
                &[(p1_blocker, p0_attacker1), (p1_blocker, p0_attacker2)]
            )
            .is_ok(),
            "Brave controlled by player 1 must grant their creature one extra block"
        );

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(p1_attacker1, PlayerId(0)),
                AttackerInfo::attacking_player(p1_attacker2, PlayerId(0)),
            ],
            ..Default::default()
        });
        assert!(
            validate_blockers(
                &state,
                &[(p0_blocker, p1_attacker1), (p0_blocker, p1_attacker2)]
            )
            .is_err(),
            "Brave controlled by player 1 must not grant player 0's creature"
        );

        let mut state = setup();
        let brave_card_id = CardId(state.next_object_id);
        let brave = create_object(
            &mut state,
            brave_card_id,
            PlayerId(0),
            "Brave the Sands".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        for def in parse_static_line_multi(
            "Each creature you control can block an additional creature each combat.",
        ) {
            let obj = state.objects.get_mut(&brave).unwrap();
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def.clone());
            obj.static_definitions.push(def);
        }

        let p1_attacker1 = create_creature(&mut state, PlayerId(1), "P1 Bear A", 2, 2);
        let p1_attacker2 = create_creature(&mut state, PlayerId(1), "P1 Bear B", 2, 2);
        let p0_blocker = create_creature(&mut state, PlayerId(0), "P0 Guard", 1, 4);

        evaluate_layers(&mut state);

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(p1_attacker1, PlayerId(0)),
                AttackerInfo::attacking_player(p1_attacker2, PlayerId(0)),
            ],
            ..Default::default()
        });
        assert!(
            validate_blockers_for_player(
                &state,
                PlayerId(0),
                &[(p0_blocker, p1_attacker1), (p0_blocker, p1_attacker2)]
            )
            .is_ok(),
            "Brave controlled by player 0 must grant their creature one extra block"
        );
    }

    /// CR 508.1 + CR 509.1b + CR 611.3a: Runtime combat regression for Wirecat's
    /// gated "can't attack or block if an enchantment is on the battlefield". The
    /// parsed `CantAttackOrBlock` carries an `ObjectCount(enchantment) >= 1`
    /// condition, so the restriction must be inert while no enchantment exists
    /// (attacking and blocking both succeed) and functioning once one does (both
    /// fail). Drives the layer/legality seams (`evaluate_layers` →
    /// `validate_attackers` / `can_block_pair`), not just the parsed shape.
    #[test]
    fn wirecat_cant_attack_or_block_gate_honored_at_runtime() {
        use crate::game::layers::evaluate_layers;
        use crate::parser::oracle_static::parse_static_line_multi;

        let mut state = setup();

        // Wirecat controlled by the active player (P0) so it may attack.
        let wirecat = create_creature(&mut state, PlayerId(0), "Wirecat", 2, 2);
        for def in parse_static_line_multi(
            "This creature can't attack or block if an enchantment is on the battlefield.",
        ) {
            let obj = state.objects.get_mut(&wirecat).unwrap();
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def.clone());
            obj.static_definitions.push(def);
        }

        // An opponent's attacker for the block-legality seam.
        let opp_attacker = create_creature(&mut state, PlayerId(1), "Opp Bear", 2, 2);

        evaluate_layers(&mut state);

        // No enchantment on the battlefield → the gate is false → the restriction
        // is inert: Wirecat may attack and may block.
        assert!(
            validate_attackers(&state, &[wirecat]).is_ok(),
            "Wirecat must be able to attack with no enchantment on the battlefield"
        );
        assert!(
            can_block_pair(&state, wirecat, opp_attacker),
            "Wirecat must be able to block with no enchantment on the battlefield"
        );

        // Put an enchantment on the battlefield → the gate is true → the
        // restriction functions: Wirecat can neither attack nor block.
        let enchantment_card_id = CardId(state.next_object_id);
        let enchantment = create_object(
            &mut state,
            enchantment_card_id,
            PlayerId(1),
            "Some Aura".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        // Set BOTH the live and base card types: `evaluate_layers` reseeds live
        // `card_types` from `base_card_types` (see
        // `seed_live_characteristics_from_base` in layers.rs), so the enchantment
        // type must be present in the base to survive the second layer pass and
        // actually satisfy the `ObjectCount(enchantment) >= 1` gate.
        let ench_obj = state.objects.get_mut(&enchantment).unwrap();
        ench_obj.card_types.core_types.push(CoreType::Enchantment);
        ench_obj
            .base_card_types
            .core_types
            .push(CoreType::Enchantment);

        evaluate_layers(&mut state);

        assert!(
            validate_attackers(&state, &[wirecat]).is_err(),
            "Wirecat must not be able to attack while an enchantment is on the battlefield"
        );
        assert!(
            !can_block_pair(&state, wirecat, opp_attacker),
            "Wirecat must not be able to block while an enchantment is on the battlefield"
        );
    }

    #[test]
    fn max_blockers_each_combat_counts_previous_defending_players() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        let attacker1 = create_creature(&mut state, PlayerId(0), "Bear A", 2, 2);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Bear B", 2, 2);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Guard A", 1, 1);
        let blocker2 = create_creature(&mut state, PlayerId(2), "Guard B", 1, 1);
        let arbiter_card_id = CardId(state.next_object_id);
        let arbiter = create_object(
            &mut state,
            arbiter_card_id,
            PlayerId(0),
            "Silent Arbiter".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&arbiter)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MaxBlockersEachCombat {
                max: 1,
            }));

        let mut combat = CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(attacker1, PlayerId(1)),
                AttackerInfo::attacking_player(attacker2, PlayerId(2)),
            ],
            blockers_declared_by: vec![PlayerId(1)],
            ..Default::default()
        };
        combat.blocker_to_attacker.insert(blocker1, vec![attacker1]);
        state.combat = Some(combat);

        assert!(
            validate_blockers_for_player(&state, PlayerId(2), &[(blocker2, attacker2)]).is_err()
        );
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

    fn add_must_be_blocked_by_all(state: &mut GameState, id: ObjectId) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustBeBlockedByAll));
    }

    #[test]
    fn must_be_blocked_by_all_requires_every_able_blocker() {
        // CR 509.1c: a lured attacker ("All creatures able to block ~ do so",
        // Prized Unicorn / Lure) must be blocked by EVERY creature able to block
        // it — not just one, which is the distinction from MustBeBlocked.
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Prized Unicorn", 2, 2);
        add_must_be_blocked_by_all(&mut state, attacker);
        let blocker_a = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let blocker_b = create_creature(&mut state, PlayerId(1), "Elf", 1, 1);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // No blockers: illegal (two able creatures left idle).
        assert!(validate_blockers(&state, &[]).is_err());
        // Only one of two able blockers: still illegal — the other is idle & able.
        assert!(validate_blockers(&state, &[(blocker_a, attacker)]).is_err());
        // Every able blocker assigned: legal.
        assert!(validate_blockers(&state, &[(blocker_a, attacker), (blocker_b, attacker)]).is_ok());
    }

    /// CR 509.1c (issue #4949): END-TO-END proof that the PARSED Ochran Assassin
    /// forced-block static enforces at combat. We parse the printed "All creatures
    /// able to block Ochran Assassin do so" line via `parse_oracle_text`, install
    /// the resulting static on the attacker, and drive real block-declaration
    /// validation. Revert-discriminating: before the parser fix the line
    /// misclassifies to a one-shot effect, so `parse_oracle_text` yields NO
    /// `MustBeBlockedByAll` static and the `.expect(...)` below fails.
    #[test]
    fn parsed_ochran_assassin_lure_forces_every_able_blocker() {
        let parsed = crate::parser::parse_oracle_text(
            "Deathtouch\nAll creatures able to block Ochran Assassin do so.",
            "Ochran Assassin",
            &["Deathtouch".to_string()],
            &["Creature".to_string()],
            &["Human".to_string(), "Assassin".to_string()],
        );
        let lure = parsed
            .statics
            .into_iter()
            .find(|s| s.mode == StaticMode::MustBeBlockedByAll)
            .expect(
                "parser must produce a permanent MustBeBlockedByAll static for Ochran Assassin",
            );

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Ochran Assassin", 1, 1);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(lure);
        let blocker_a = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let blocker_b = create_creature(&mut state, PlayerId(1), "Elf", 1, 1);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // The parsed lure forces EVERY able blocker onto Ochran.
        assert!(
            validate_blockers(&state, &[]).is_err(),
            "no blockers must be illegal under the parsed lure"
        );
        assert!(
            validate_blockers(&state, &[(blocker_a, attacker)]).is_err(),
            "leaving one able blocker idle must be illegal"
        );
        assert!(
            validate_blockers(&state, &[(blocker_a, attacker), (blocker_b, attacker)]).is_ok(),
            "assigning every able blocker must be legal"
        );
    }

    #[test]
    fn must_be_blocked_by_all_exempts_unable_blockers() {
        // CR 509.1c "able to": a tapped creature carries no block requirement, so
        // blocking with only the untapped able creature is a legal declaration.
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Prized Unicorn", 2, 2);
        add_must_be_blocked_by_all(&mut state, attacker);
        let able = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let tapped = create_creature(&mut state, PlayerId(1), "Elf", 1, 1);
        state.objects.get_mut(&tapped).unwrap().tapped = true;

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });

        // The lone untapped able blocker must block; the tapped one is exempt.
        assert!(validate_blockers(&state, &[]).is_err());
        assert!(validate_blockers(&state, &[(able, attacker)]).is_ok());
    }

    #[test]
    fn must_be_blocked_by_all_counts_multi_blocker_spare_capacity() {
        // CR 509.1c: a creature that can block an additional attacker is still
        // "able to" block the lured attacker while blocking elsewhere.
        let mut state = setup();
        let lured = create_creature(&mut state, PlayerId(0), "Prized Unicorn", 2, 2);
        add_must_be_blocked_by_all(&mut state, lured);
        let other_attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let guard = create_creature(&mut state, PlayerId(1), "Palace Guard", 1, 4);
        state
            .objects
            .get_mut(&guard)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::ExtraBlockers {
                count: Some(1),
            }));

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(lured, PlayerId(1)),
                AttackerInfo::attacking_player(other_attacker, PlayerId(1)),
            ],
            ..Default::default()
        });

        assert!(validate_blockers(&state, &[(guard, other_attacker)]).is_err());
        assert!(validate_blockers(&state, &[(guard, other_attacker), (guard, lured)]).is_ok());
    }

    #[test]
    fn parsed_lure_effect_reaches_must_be_blocked_by_all_enforcement() {
        use crate::game::effects::effect::resolve;
        use crate::game::layers::evaluate_layers;
        use crate::types::ability::{Duration, Effect, ResolvedAbility, TargetFilter};

        let mut state = setup();
        let lured = create_creature(&mut state, PlayerId(0), "Prized Unicorn", 2, 2);
        let blocker_a = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let blocker_b = create_creature(&mut state, PlayerId(1), "Elf", 1, 1);

        let mut effect = crate::parser::oracle_effect::parse_effect(
            "All creatures able to block target creature this turn do so",
        );
        match &mut effect {
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } => {
                *target = Some(TargetFilter::SpecificObject { id: lured });
                for sd in static_abilities.iter_mut() {
                    sd.affected = Some(TargetFilter::SpecificObject { id: lured });
                }
            }
            other => panic!("expected GenericEffect from lure parser, got {other:?}"),
        }
        let ability = ResolvedAbility::new(effect, vec![], lured, PlayerId(0))
            .duration(Duration::UntilEndOfTurn);
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        evaluate_layers(&mut state);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(lured, PlayerId(1))],
            ..Default::default()
        });

        assert!(validate_blockers(&state, &[(blocker_a, lured)]).is_err());
        assert!(validate_blockers(&state, &[(blocker_a, lured), (blocker_b, lured)]).is_ok());
    }

    fn add_must_block_attacker(state: &mut GameState, creature: ObjectId, attacker: ObjectId) {
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustBlockAttacker {
                attacker,
            }));
    }

    #[test]
    fn must_block_attacker_requires_blocking_that_specific_attacker() {
        // CR 702.39a + CR 509.1c: a provoked creature must block the provoking
        // attacker specifically — not merely some attacker.
        let mut state = setup();
        let provoker = create_creature(&mut state, PlayerId(0), "Krosan Vorine", 3, 3);
        let other = create_creature(&mut state, PlayerId(0), "Hill Giant", 3, 3);
        let forced = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        add_must_block_attacker(&mut state, forced, provoker);

        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(provoker, PlayerId(1)),
                AttackerInfo::attacking_player(other, PlayerId(1)),
            ],
            ..Default::default()
        });

        // Not blocking at all — illegal (it can legally block the provoker).
        assert!(validate_blockers(&state, &[]).is_err());
        // Blocking the WRONG attacker — still illegal; generic MustBlock would
        // wrongly accept this.
        assert!(validate_blockers(&state, &[(forced, other)]).is_err());
        // Blocking the provoker — legal.
        assert!(validate_blockers(&state, &[(forced, provoker)]).is_ok());
    }

    #[test]
    fn must_block_attacker_exempt_when_cannot_block() {
        // CR 509.1a: a tapped creature can't block, so the provoke requirement
        // imposes nothing and an empty declaration is legal.
        let mut state = setup();
        let provoker = create_creature(&mut state, PlayerId(0), "Krosan Vorine", 3, 3);
        let forced = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        add_must_block_attacker(&mut state, forced, provoker);
        state.objects.get_mut(&forced).unwrap().tapped = true;

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(provoker, PlayerId(1))],
            ..Default::default()
        });

        assert!(validate_blockers(&state, &[]).is_ok());
    }

    /// CR 509.1c (GAP-7): a `MustBeBlocked` requirement granted *transiently*
    /// via `Effect::GenericEffect` (the Deadly Allure path) must reach
    /// `combat.rs` enforcement. Unlike `add_must_be_blocked` (which pushes onto
    /// the BASE `static_definitions`), this drives the full
    /// `resolve` → transient continuous effect → `evaluate_layers` →
    /// `static_definitions` pipeline. The `AddStaticMode` modification is what
    /// propagates the mode — `register_transient_effect` snapshots only
    /// `modifications`, never the inert `mode` field.
    #[test]
    fn generic_effect_granted_must_be_blocked_reaches_enforcement() {
        use crate::game::effects::effect::resolve;
        use crate::game::layers::evaluate_layers;
        use crate::types::ability::{
            ContinuousModification, Duration, Effect, ResolvedAbility, TargetFilter,
        };
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lure Beast", 3, 3);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let static_def = StaticDefinition::new(StaticMode::MustBeBlocked)
            .affected(TargetFilter::SpecificObject { id: attacker })
            .modifications(vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::MustBeBlocked,
            }]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            attacker,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        evaluate_layers(&mut state);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        // The transiently-granted MustBeBlocked must force a blocker assignment.
        assert!(
            validate_blockers(&state, &[]).is_err(),
            "transient MustBeBlocked must reach combat enforcement"
        );
        assert!(validate_blockers(&state, &[(blocker, attacker)]).is_ok());
    }

    /// CR 508.1d (GAP-7): the `MustAttack` carrier fix. This drives the
    /// *parser-produced* `GenericEffect` for "attack this combat if able"
    /// through `resolve` → transient continuous effect → `evaluate_layers` →
    /// `static_definitions` → `declare_attackers` enforcement. It FAILS against
    /// pre-fix code — `try_parse_attack_if_able` emitted a `StaticDefinition`
    /// with empty `modifications`, so `register_transient_effect` snapshotted
    /// nothing and the `MustAttack` mode never reached `combat.rs`. The step-4
    /// `AddStaticMode` carrier fix is what makes this pass.
    #[test]
    fn generic_effect_granted_must_attack_reaches_enforcement() {
        use crate::game::effects::effect::resolve;
        use crate::game::layers::evaluate_layers;
        use crate::types::ability::{Duration, Effect, ResolvedAbility, TargetFilter};
        let mut state = setup_combat_phase();
        let attacker = create_creature(&mut state, PlayerId(0), "Berserker", 3, 3);

        // The parser output for the standalone attack requirement — this is the
        // exact `GenericEffect` `try_parse_attack_if_able` builds.
        let mut effect = crate::parser::oracle_effect::parse_effect("attack this combat if able");
        // Point the requirement at the attacker (the standalone parser leaves
        // `affected` at the default — the conjunction-split / subject pipeline
        // fills it; here we set it directly to isolate the carrier behaviour).
        match &mut effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                for sd in static_abilities.iter_mut() {
                    sd.affected = Some(TargetFilter::SpecificObject { id: attacker });
                }
            }
            other => panic!("expected GenericEffect from parser, got {other:?}"),
        }
        let ability = ResolvedAbility::new(effect, vec![], attacker, PlayerId(0))
            .duration(Duration::UntilEndOfTurn);
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        evaluate_layers(&mut state);

        // Declaring no attackers must be illegal: the creature is forced to attack.
        let result = declare_attackers(&mut state, &[], &mut Vec::new());
        assert!(
            result.is_err(),
            "transient MustAttack must reach declare_attackers enforcement"
        );
    }

    /// CR 508.1d + CR 509.1c: Hustle — "Target creature attacks or blocks this
    /// turn if able." Drives the *full production parse* of Hustle's Oracle text
    /// through `resolve` → transient continuous effect → `evaluate_layers` →
    /// combat enforcement, and asserts BOTH the attack requirement (declaring no
    /// attackers is illegal on the controller's turn) AND the block requirement
    /// (declaring no blockers is illegal when the forced creature could block)
    /// reach `combat.rs`.
    ///
    /// Revert-proof: on pre-change code the parser does not recognize "attacks or
    /// blocks this turn if able" — the whole line lowers to `Effect::Unimplemented`,
    /// the `let Effect::GenericEffect = ...` destructure panics, and neither
    /// requirement is granted, so both `is_err()` assertions fail.
    #[test]
    fn hustle_attacks_or_blocks_reaches_combat_enforcement() {
        use crate::game::effects::effect::resolve;
        use crate::game::layers::evaluate_layers;
        use crate::types::ability::{Duration, Effect, ResolvedAbility, TargetRef};

        let mut state = setup_combat_phase();
        // The forced creature is controlled by the same player that will declare
        // attackers (PlayerId(0)); the spell caster is also PlayerId(0).
        let forced = create_creature(&mut state, PlayerId(0), "Forced Soldier", 2, 2);

        // Production parse of Hustle's full Oracle text — no hand-built effect.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Target creature attacks or blocks this turn if able.",
            "Hustle",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1, "Hustle parses one spell ability");
        let effect = (*parsed.abilities[0].effect).clone();
        let Effect::GenericEffect { .. } = &effect else {
            panic!("Hustle must parse to GenericEffect, got {effect:?}");
        };

        // Bind the spell to the chosen creature target; `affected: ParentTarget`
        // resolves against `ability.targets` at registration time.
        let ability =
            ResolvedAbility::new(effect, vec![TargetRef::Object(forced)], forced, PlayerId(0))
                .duration(Duration::UntilEndOfTurn);
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        evaluate_layers(&mut state);

        // CR 508.1d: declaring no attackers is illegal — the forced creature must attack.
        assert!(
            declare_attackers(&mut state, &[], &mut Vec::new()).is_err(),
            "Hustle MustAttack must reach declare_attackers enforcement"
        );

        // CR 509.1c: now stage a block scenario. PlayerId(1) attacks PlayerId(0);
        // the forced creature is an untapped, legal blocker, so declaring no
        // blockers must be illegal.
        let attacker = create_creature(&mut state, PlayerId(1), "Aggressor", 2, 2);
        state.active_player = PlayerId(1);
        state.phase = crate::types::phase::Phase::DeclareBlockers;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(0))],
            ..Default::default()
        });
        assert!(
            validate_blockers(&state, &[]).is_err(),
            "Hustle MustBlock must reach declare_blockers enforcement"
        );
        // Sanity: assigning the forced creature as a blocker satisfies the requirement.
        assert!(
            validate_blockers(&state, &[(forced, attacker)]).is_ok(),
            "blocking the attacker should satisfy the MustBlock requirement"
        );
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
    fn creature_must_attack_true_for_untapped_goaded_creature() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        assert!(creature_must_attack(&state, goaded));
    }

    #[test]
    fn creature_must_attack_false_when_tapped() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        state.objects.get_mut(&goaded).unwrap().tapped = true;
        assert!(!creature_must_attack(&state, goaded));
    }

    #[test]
    fn creature_must_attack_false_when_summoning_sick() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        state.objects.get_mut(&goaded).unwrap().summoning_sick = true;
        assert!(!creature_must_attack(&state, goaded));
    }

    #[test]
    fn creature_must_attack_false_for_defender_without_override() {
        let mut state = setup_combat_phase();
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        state
            .objects
            .get_mut(&goaded)
            .unwrap()
            .keywords
            .push(Keyword::Defender);
        assert!(!creature_must_attack(&state, goaded));
    }

    #[test]
    fn creature_must_attack_false_when_no_requirement() {
        let mut state = setup_combat_phase();
        // Plain creature, not goaded and no MustAttack static.
        let plain = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        assert!(!creature_must_attack(&state, plain));
    }

    #[test]
    fn creature_must_attack_false_for_non_active_controller() {
        let mut state = setup_combat_phase();
        // Goaded creature controlled by the non-active player.
        let goaded = create_goaded_creature(&mut state, PlayerId(1), PlayerId(0));
        assert!(!creature_must_attack(&state, goaded));
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
    fn static_goaded_enforces_source_controller_attack_restriction() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        let goaded = create_creature(&mut state, PlayerId(0), "Goaded Bear", 2, 2);
        let source = create_creature(&mut state, PlayerId(1), "Goad Aura", 0, 1);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Goaded).affected(
                    crate::types::ability::TargetFilter::Typed(
                        crate::types::ability::TypedFilter::creature()
                            .controller(crate::types::ability::ControllerRef::Opponent),
                    ),
                ),
            );

        let omitted = declare_attackers(&mut state, &[], &mut vec![]);
        assert!(omitted.is_err());
        assert!(omitted.unwrap_err().contains("goaded"));

        let attacks_goading_player = declare_attackers(
            &mut state,
            &[(goaded, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(attacks_goading_player.is_err());
        assert!(attacks_goading_player
            .unwrap_err()
            .contains("must attack a different player"));

        let attacks_other_player = declare_attackers(
            &mut state,
            &[(goaded, AttackTarget::Player(PlayerId(2)))],
            &mut vec![],
        );
        assert!(attacks_other_player.is_ok());
    }

    #[test]
    fn must_attack_player_enforces_specific_player() {
        // CR 508.1d: a creature with MustAttackPlayer{P2} must attack P2 when
        // P2 is a legal target; attacking a different player is illegal.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        let attacker = create_creature(&mut state, PlayerId(0), "Lured Bear", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: PlayerId(2),
            }));

        // Attacking the wrong player (P1) while P2 is a legal target: illegal.
        let wrong = declare_attackers(
            &mut state,
            &[(attacker, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(wrong.is_err());
        assert!(wrong.unwrap_err().contains("must attack"));

        // Attacking the required player (P2): legal.
        let right = declare_attackers(
            &mut state,
            &[(attacker, AttackTarget::Player(PlayerId(2)))],
            &mut vec![],
        );
        assert!(right.is_ok());
    }

    #[test]
    fn cant_attack_owner_blocks_only_owner_attack_target() {
        let mut state = setup_multiplayer_combat(3);
        let attacker = create_creature(&mut state, PlayerId(1), "Owner-Restricted Bear", 2, 2);
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.controller = PlayerId(0);
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantAttack)
                    .affected(TargetFilter::SelfRef)
                    .attack_defended(Some(crate::types::triggers::AttackTargetFilter::Owner)),
            );
        }

        let mut owner_attack_state = state.clone();
        let attacks_owner = declare_attackers(
            &mut owner_attack_state,
            &[(attacker, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(attacks_owner.is_err());
        assert!(attacks_owner
            .unwrap_err()
            .contains("can't attack Player(PlayerId(1))"));

        let attacks_non_owner = declare_attackers(
            &mut state,
            &[(attacker, AttackTarget::Player(PlayerId(2)))],
            &mut vec![],
        );
        assert!(attacks_non_owner.is_ok());
    }

    #[test]
    fn cant_attack_owner_or_planeswalker_blocks_owner_side_targets() {
        let mut state = setup_multiplayer_combat(3);
        let attacker = create_creature(&mut state, PlayerId(1), "Xantcha", 5, 5);
        let owner_walker = create_planeswalker(&mut state, PlayerId(1), "Owner Walker");
        let other_walker = create_planeswalker(&mut state, PlayerId(2), "Other Walker");
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.controller = PlayerId(0);
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantAttack)
                    .affected(TargetFilter::SelfRef)
                    .attack_defended(Some(
                        crate::types::triggers::AttackTargetFilter::OwnerOrPlaneswalker,
                    )),
            );
        }

        let attacks_owner_walker = declare_attackers(
            &mut state.clone(),
            &[(attacker, AttackTarget::Planeswalker(owner_walker))],
            &mut vec![],
        );
        assert!(attacks_owner_walker.is_err());
        assert!(attacks_owner_walker
            .unwrap_err()
            .contains("can't attack Planeswalker"));

        let attacks_owner_player = declare_attackers(
            &mut state.clone(),
            &[(attacker, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(attacks_owner_player.is_err());

        let attacks_other_walker = declare_attackers(
            &mut state,
            &[(attacker, AttackTarget::Planeswalker(other_walker))],
            &mut vec![],
        );
        assert!(attacks_other_walker.is_ok());
    }

    #[test]
    fn must_attack_player_omitted_creature_fails() {
        let mut state = setup_combat_phase();
        let attacker = create_creature(&mut state, PlayerId(0), "Lured Bear", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: PlayerId(1),
            }));

        let result = declare_attackers(&mut state, &[], &mut vec![]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must attack"));
    }

    #[test]
    fn must_attack_player_requires_attacking_player_not_planeswalker() {
        let mut state = setup_combat_phase();
        let attacker = create_creature(&mut state, PlayerId(0), "Lured Bear", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: PlayerId(1),
            }));
        let planeswalker = create_planeswalker(&mut state, PlayerId(1), "Required Player's Walker");

        let result = declare_attackers(
            &mut state,
            &[(attacker, AttackTarget::Planeswalker(planeswalker))],
            &mut vec![],
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must attack"));
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

    /// Build a multiplayer DeclareAttackers state (FFA, no teams).
    fn setup_multiplayer_combat(player_count: u8) -> GameState {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::standard(),
            player_count,
            42,
        );
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        state
    }

    #[test]
    fn goad_allows_attacking_goading_player_when_only_other_opponent_is_unattackable() {
        // 3-player game: P0 (active) controls a creature goaded by P1; the only
        // other opponent, P2, is phased out and therefore not a legal attack
        // target. CR 701.15b: with no *attackable* non-goading player, attacking
        // the goading player P1 is legal.
        let mut state = setup_multiplayer_combat(3);
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));
        state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(2))
            .unwrap()
            .status = crate::types::player::PlayerStatus::PhasedOut;

        let result = declare_attackers(
            &mut state,
            &[(goaded, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(
            result.is_ok(),
            "phased-out P2 is not attackable, so attacking goading P1 is legal: {result:?}"
        );
    }

    #[test]
    fn goad_still_forces_redirect_when_an_attackable_non_goading_player_exists() {
        // 3-player game: creature goaded only by P1, while P2 is a normal,
        // attackable opponent. CR 701.15b: the creature must attack P2, so
        // declaring it against the goading player P1 is illegal.
        let mut state = setup_multiplayer_combat(3);
        let goaded = create_goaded_creature(&mut state, PlayerId(0), PlayerId(1));

        let result = declare_attackers(
            &mut state,
            &[(goaded, AttackTarget::Player(PlayerId(1)))],
            &mut vec![],
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("must attack a different player if able"),
            "an attackable non-goading opponent (P2) must still force the redirect"
        );
    }

    #[test]
    fn cant_be_blocked_except_by_enforces_filter() {
        use crate::parser::oracle_target::parse_target;
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::{BlockExceptionKind, StaticMode};

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Phantom Warrior", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::Quality(parse_target("creatures with flying").0),
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

    /// Issue #2364: Pinnacle Emissary Drone tokens carry "can block only
    /// creatures with flying" — a blocker-side BlockRestriction, distinct
    /// from attacker-side CantBeBlockedExceptBy.
    #[test]
    fn issue_2364_block_restriction_limits_blocker_to_flying_attackers() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::{block_only_creatures_with_flying_filter, StaticMode};

        let mut state = setup();
        let ground_attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let flying_attacker = create_creature(&mut state, PlayerId(0), "Bird", 2, 2);
        state
            .objects
            .get_mut(&flying_attacker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        let drone = create_creature(&mut state, PlayerId(1), "Drone", 1, 1);
        {
            let drone_obj = state.objects.get_mut(&drone).unwrap();
            drone_obj.keywords.push(Keyword::Flying);
            drone_obj.static_definitions.push(
                StaticDefinition::new(StaticMode::BlockRestriction {
                    filter: block_only_creatures_with_flying_filter(),
                })
                .affected(TargetFilter::SelfRef),
            );
        }

        assert!(
            validate_blockers(&state, &[(drone, ground_attacker)]).is_err(),
            "Drone must not block non-flying attackers"
        );
        assert!(
            !can_block_pair(&state, drone, ground_attacker),
            "can_block_pair must reject non-flying attacker"
        );
        assert!(
            validate_blockers(&state, &[(drone, flying_attacker)]).is_ok(),
            "Drone may block flying attackers"
        );
        assert!(
            can_block_pair(&state, drone, flying_attacker),
            "can_block_pair must accept flying attacker"
        );
    }

    /// The precomputed `can_block_pair_with_precomputed` path must agree with the
    /// `can_block_pair` wrapper for a blocker-side BlockRestriction (flying-only):
    /// reject a ground attacker, accept a flyer. Reverted-fix discrimination: if
    /// the precomputed variant dropped the `blocker_allowed` slice, the ground
    /// attacker would be wrongly ACCEPTED, flipping the first assertion.
    #[test]
    fn block_restriction_precomputed_slice_rejects_and_accepts_correctly() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::{block_only_creatures_with_flying_filter, StaticMode};

        let mut state = setup();
        let ground_attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let flying_attacker = create_creature(&mut state, PlayerId(0), "Bird", 2, 2);
        state
            .objects
            .get_mut(&flying_attacker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        let drone = create_creature(&mut state, PlayerId(1), "Drone", 1, 1);
        {
            let drone_obj = state.objects.get_mut(&drone).unwrap();
            drone_obj.keywords.push(Keyword::Flying);
            drone_obj.static_definitions.push(
                StaticDefinition::new(StaticMode::BlockRestriction {
                    filter: block_only_creatures_with_flying_filter(),
                })
                .affected(TargetFilter::SelfRef),
            );
        }

        let blocker_restriction = collect_blocker_restriction_statics(&state);
        let block_restriction = collect_block_restriction_statics(&state);
        let blocker_allowed = collect_blocker_allowed_statics(&state);

        // The BlockRestriction static must be captured by the collect helper.
        assert!(
            !blocker_allowed.is_empty(),
            "collect_blocker_allowed_statics must capture the flying-only BlockRestriction"
        );

        // No CanBlockShadow static on this board, so the shadow-lift gate is false.
        let can_block_shadow_exists = false;
        let precomputed_ground = can_block_pair_with_precomputed(
            &state,
            drone,
            ground_attacker,
            &blocker_restriction,
            &block_restriction,
            &blocker_allowed,
            can_block_shadow_exists,
        );
        let precomputed_flyer = can_block_pair_with_precomputed(
            &state,
            drone,
            flying_attacker,
            &blocker_restriction,
            &block_restriction,
            &blocker_allowed,
            can_block_shadow_exists,
        );

        assert!(
            !precomputed_ground,
            "precomputed path must reject the ground attacker"
        );
        assert!(
            precomputed_flyer,
            "precomputed path must accept the flying attacker"
        );
        // Wrapper and precomputed path must agree byte-for-byte.
        assert_eq!(
            precomputed_ground,
            can_block_pair(&state, drone, ground_attacker)
        );
        assert_eq!(
            precomputed_flyer,
            can_block_pair(&state, drone, flying_attacker)
        );
    }

    /// Two DISTINCT CantBlock sources both affecting one blocker: the collect
    /// helper must capture BOTH, and the precomputed cant-block check / pair check
    /// must report the blocker as unable to block. Reverted-fix discrimination: a
    /// precomputed variant that re-resolved against only one source, or read the
    /// wrong `src_id` controller, would still report `true`/blockable for the
    /// remote-affected source.
    #[test]
    fn two_separate_cant_block_sources_both_enforce_from_precomputed() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);

        // Two distinct opponent-controlled sources, each granting CantBlock to
        // every creature the opponent (PlayerId(1)) controls — both touch the blocker.
        let source_a = create_creature(&mut state, PlayerId(0), "Suppressor A", 1, 1);
        let source_b = create_creature(&mut state, PlayerId(0), "Suppressor B", 1, 1);
        for source in [source_a, source_b] {
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .static_definitions
                .push(
                    StaticDefinition::new(StaticMode::CantBlock).affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent),
                    )),
                );
        }

        let blocker_restriction = collect_blocker_restriction_statics(&state);
        // Both sources must be captured (plus they are the only CantBlock statics).
        assert_eq!(
            blocker_restriction.len(),
            2,
            "collect_blocker_restriction_statics must capture both CantBlock sources"
        );

        assert!(
            blocker_has_cant_block_static_from_precomputed(&state, blocker, &blocker_restriction),
            "precomputed cant-block check must see the affected blocker"
        );

        let block_restriction = collect_block_restriction_statics(&state);
        let blocker_allowed = collect_blocker_allowed_statics(&state);
        assert!(
            !can_block_pair_with_precomputed(
                &state,
                blocker,
                attacker,
                &blocker_restriction,
                &block_restriction,
                &blocker_allowed,
                // No CanBlockShadow static on this board.
                false,
            ),
            "precomputed pair check must reject a blocker under CantBlock"
        );
    }

    /// Issue #496: "can't be blocked except by three or more creatures" must
    /// enforce the count. Reverted-fix discrimination: the old `String` path
    /// degrades `parse_target("three or more creatures")` to a permissive
    /// filter, so a single blocker passes — failing this test's "1 and 2 fail,
    /// 3 passes" boundary.
    #[test]
    fn cant_be_blocked_except_by_three_requires_three_blockers() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::{BlockExceptionKind, StaticMode};

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Troll of Khazad-dum", 4, 6);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::MinBlockers { min: 3 },
            }));

        let b1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let b2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        let b3 = create_creature(&mut state, PlayerId(1), "Bear3", 2, 2);

        // One blocker: illegal.
        assert!(validate_blockers(&state, &[(b1, attacker)]).is_err());
        // Two blockers: illegal.
        assert!(validate_blockers(&state, &[(b1, attacker), (b2, attacker)]).is_err());
        // Three blockers: legal.
        assert!(
            validate_blockers(&state, &[(b1, attacker), (b2, attacker), (b3, attacker)]).is_ok()
        );
    }

    /// Guards the 122-card quality class: a `Quality` exception is still a
    /// per-blocker filter check, unaffected by the count generalization.
    #[test]
    fn cant_be_blocked_except_by_quality_still_per_blocker() {
        use crate::parser::oracle_target::parse_target;
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::{BlockExceptionKind, StaticMode};

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Quality Attacker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::Quality(parse_target("artifact creatures").0),
            }));

        let plain = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let artifact = create_creature(&mut state, PlayerId(1), "Myr", 2, 2);
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        // Non-artifact blocker: illegal.
        assert!(validate_blockers(&state, &[(plain, attacker)]).is_err());
        // Artifact blocker: legal.
        assert!(validate_blockers(&state, &[(artifact, attacker)]).is_ok());
    }

    /// CR 509.1b + CR 702.111b: an attacker that is both Menace and
    /// `MinBlockers { min: 3 }` requires the stricter `max(2, 3)` = 3 blockers.
    #[test]
    fn cant_be_blocked_except_by_min_and_menace() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::{BlockExceptionKind, StaticMode};

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Menace Troll", 4, 6);
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.keywords.push(Keyword::Menace);
            obj.static_definitions
                .push(StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                    kind: BlockExceptionKind::MinBlockers { min: 3 },
                }));
        }

        let b1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let b2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        let b3 = create_creature(&mut state, PlayerId(1), "Bear3", 2, 2);

        // Two blockers satisfy Menace but not MinBlockers { min: 3 }: illegal.
        assert!(validate_blockers(&state, &[(b1, attacker), (b2, attacker)]).is_err());
        // Three blockers satisfy both: legal.
        assert!(
            validate_blockers(&state, &[(b1, attacker), (b2, attacker), (b3, attacker)]).is_ok()
        );
    }

    /// CR 509.1b: Stalking Tiger — "can't be blocked by more than one creature."
    /// A per-creature blocker maximum: one blocker is legal, two is illegal.
    #[test]
    fn cant_be_blocked_by_more_than_one_rejects_extra_blocker() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Stalking Tiger", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeBlockedByMoreThan {
                max: 1,
            }));

        let b1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let b2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);

        // Unblocked: legal (the maximum is a ceiling, not a requirement).
        assert!(validate_blockers(&state, &[]).is_ok());
        // One blocker: legal.
        assert!(validate_blockers(&state, &[(b1, attacker)]).is_ok());
        // Two blockers: illegal — exceeds the maximum.
        assert!(validate_blockers(&state, &[(b1, attacker), (b2, attacker)]).is_err());
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
            // CR 506.3: Ghostly Prison — "Creatures can't attack you unless..."
            // Tax applies only to attacks targeting the prison's controller
            // (CR 506.3 enumerates the legal attack target types: a player, a
            // planeswalker, or a battle).
            defended: Some(crate::types::triggers::AttackTargetFilter::Player),
        });
        obj.static_definitions.push(def);
        id
    }

    fn add_sphere_of_safety(state: &mut GameState, controller: PlayerId) -> ObjectId {
        use crate::parser::oracle_static::parse_static_line;

        let def = parse_static_line(
            "Creatures can't attack you or planeswalkers you control unless their controller pays {X} for each of those creatures, where X is the number of enchantments you control.",
        )
        .expect("Sphere of Safety should parse");
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Sphere of Safety".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.static_definitions.push(def);
        id
    }

    fn create_enchantment(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        id
    }

    #[test]
    fn compute_attack_tax_sphere_of_safety_concretizes_x_per_attacker() {
        let mut state = setup();
        let _sphere = add_sphere_of_safety(&mut state, PlayerId(1));
        let _other_ench = create_enchantment(&mut state, PlayerId(1), "Other Aura");
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let attacks = vec![(attacker, AttackTarget::Player(PlayerId(1)))];
        let (total, per_creature) = compute_attack_tax(&state, &attacks).expect("tax applies");
        assert_eq!(total.mana_value(), 2);
        assert_eq!(per_creature.len(), 1);
        assert_eq!(per_creature[0].1.mana_value(), 2);
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
                properties: vec![FilterProp::Counters {
                    counters: crate::types::counter::CounterMatch::Any,
                    comparator: crate::types::ability::Comparator::GE,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                }],
            }))
            .description("Nils static".to_string());
        def.condition = Some(StaticCondition::UnlessPay {
            // CR 202.3e: "{X}" base cost — resolved per-attacker via scaling.
            cost: ManaCost::Cost {
                shards: vec![crate::types::mana::ManaCostShard::X],
                generic: 0,
            },
            scaling: UnlessPayScaling::PerAffectedWithRef {
                quantity: QuantityRef::CountersOn {
                    scope: crate::types::ability::ObjectScope::Target,
                    counter_type: None,
                },
            },
            // CR 506.3: Nils — "...can't attack you or planeswalkers you control..."
            defended: Some(crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
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
            // CR 506.3: Norn's Annex — "Creatures can't attack you or
            // planeswalkers you control unless..."
            defended: Some(crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
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

    /// CR 506.3 + CR 508.1d: Propaganda multiplayer regression (issue #302).
    /// Three-player game: A controls Propaganda; B attacks C (NOT A). The tax
    /// must NOT apply because C is not the static's controller. Pre-fix, the
    /// tax incorrectly fired against any opponent-attacker. The `defended`
    /// filter scopes the tax to attacks on A.
    #[test]
    fn compute_attack_tax_propaganda_only_taxes_attacks_against_controller() {
        use crate::types::ability::{
            ControllerRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
            TypedFilter, UnlessPayScaling,
        };
        use crate::types::format::FormatConfig;
        use crate::types::statics::StaticMode;

        // Three players: A=PlayerId(0), B=PlayerId(1), C=PlayerId(2). B is active.
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.active_player = PlayerId(1);

        // Player A controls Propaganda.
        let next_card_id = CardId(state.next_object_id);
        let propaganda = create_object(
            &mut state,
            next_card_id,
            PlayerId(0),
            "Propaganda".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let prop_obj = state.objects.get_mut(&propaganda).unwrap();
        prop_obj.card_types.core_types.push(CoreType::Enchantment);
        let mut def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }))
            .description("Propaganda".to_string());
        def.condition = Some(StaticCondition::UnlessPay {
            cost: crate::types::mana::ManaCost::generic(2),
            scaling: UnlessPayScaling::PerAffectedCreature,
            defended: Some(crate::types::triggers::AttackTargetFilter::Player),
        });
        prop_obj.static_definitions.push(def);

        // Player B has a creature; declares attack on player C (not A).
        let attacker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        // Attack against C (PlayerId(2)). Propaganda's defender is A
        // (PlayerId(0)) — must NOT tax.
        let attacks_on_c = vec![(attacker, AttackTarget::Player(PlayerId(2)))];
        assert!(
            compute_attack_tax(&state, &attacks_on_c).is_none(),
            "Propaganda must not tax attacks against players other than its controller (#302)",
        );

        // Sanity: attacking A DOES trigger the tax.
        let attacks_on_a = vec![(attacker, AttackTarget::Player(PlayerId(0)))];
        let (total, _) = compute_attack_tax(&state, &attacks_on_a)
            .expect("Propaganda must tax attacks against its controller");
        assert_eq!(total.mana_value(), 2, "Propaganda taxes {{2}} per attacker");
    }

    /// CR 506.3 + CR 611.3a + CR 118.12a: Archangel of Tithes — when untapped,
    /// opponents pay {1} per attacker against the controller or their
    /// planeswalkers. When tapped, the gating condition fails, so the tax
    /// is dormant. Regression for issue #309 (tax not enforced).
    #[test]
    fn compute_attack_tax_archangel_of_tithes_gated_by_untapped() {
        use crate::types::ability::{
            ControllerRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
            TypedFilter, UnlessPayScaling,
        };
        use crate::types::statics::StaticMode;

        let mut state = setup();
        // Player B (PlayerId(1)) controls Archangel of Tithes.
        let next_card_id = CardId(state.next_object_id);
        let archangel = create_object(
            &mut state,
            next_card_id,
            PlayerId(1),
            "Archangel of Tithes".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let ang_obj = state.objects.get_mut(&archangel).unwrap();
        ang_obj.card_types.core_types.push(CoreType::Creature);
        let mut def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }))
            .description("Archangel of Tithes attack tax".to_string());
        def.condition = Some(StaticCondition::And {
            conditions: vec![
                // CR 611.2b: gate — only active while untapped.
                StaticCondition::Not {
                    condition: Box::new(StaticCondition::SourceIsTapped),
                },
                StaticCondition::UnlessPay {
                    cost: crate::types::mana::ManaCost::generic(1),
                    scaling: UnlessPayScaling::PerAffectedCreature,
                    defended: Some(
                        crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker,
                    ),
                },
            ],
        });
        ang_obj.static_definitions.push(def);

        // Player A (PlayerId(0)) attacks Player B with one creature.
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let attacks = vec![(attacker, AttackTarget::Player(PlayerId(1)))];

        // Untapped → tax applies.
        let (total, _) = compute_attack_tax(&state, &attacks)
            .expect("Archangel of Tithes must tax attacks while untapped (#309)");
        assert_eq!(total.mana_value(), 1, "Archangel taxes {{1}} per attacker");

        // Tap the Archangel → gate fails, tax becomes dormant.
        state.objects.get_mut(&archangel).unwrap().tapped = true;
        assert!(
            compute_attack_tax(&state, &attacks).is_none(),
            "tapped Archangel must not enforce its tax",
        );
    }

    /// Perf gate for issue #4334. Manual benchmark for the residual
    /// `compute_combat_tax` hot path after #4312/#4329: go-wide attackers
    /// against several active tax statics. Run explicitly with:
    ///
    /// `cargo test -p engine combat_tax_profile_gate_go_wide_board -- --ignored --nocapture`
    #[test]
    #[ignore = "perf benchmark; run manually"]
    fn combat_tax_profile_gate_go_wide_board() {
        use crate::types::ability::{
            FilterProp, QuantityRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
            TypedFilter, UnlessPayScaling,
        };
        use crate::types::counter::CounterType;
        use crate::types::mana::ManaCost;
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let defender = PlayerId(1);
        let attacker_controller = PlayerId(0);

        let _prison_a = create_ghostly_prison(&mut state, defender);
        let _prison_b = create_ghostly_prison(&mut state, defender);
        let _sphere = add_sphere_of_safety(&mut state, defender);
        let _ench_a = create_enchantment(&mut state, defender, "Bench Aura A");
        let _ench_b = create_enchantment(&mut state, defender, "Bench Aura B");

        let nils_card_id = CardId(state.next_object_id);
        let nils = create_object(
            &mut state,
            nils_card_id,
            defender,
            "Nils, Discipline Enforcer".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let nils_obj = state.objects.get_mut(&nils).unwrap();
        nils_obj.card_types.core_types.push(CoreType::Creature);
        let mut nils_def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![FilterProp::Counters {
                    counters: crate::types::counter::CounterMatch::Any,
                    comparator: crate::types::ability::Comparator::GE,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                }],
            }))
            .description("Nils benchmark tax".to_string());
        nils_def.condition = Some(StaticCondition::UnlessPay {
            cost: ManaCost::Cost {
                shards: vec![crate::types::mana::ManaCostShard::X],
                generic: 0,
            },
            scaling: UnlessPayScaling::PerAffectedWithRef {
                quantity: QuantityRef::CountersOn {
                    scope: crate::types::ability::ObjectScope::Target,
                    counter_type: None,
                },
            },
            defended: Some(crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker),
        });
        nils_obj.static_definitions.push(nils_def);

        let mut attacks = Vec::new();
        for index in 0..24 {
            let attacker = create_creature(
                &mut state,
                attacker_controller,
                &format!("Bench Attacker {index}"),
                2,
                2,
            );
            if index % 3 == 0 {
                state
                    .objects
                    .get_mut(&attacker)
                    .unwrap()
                    .counters
                    .insert(CounterType::Plus1Plus1, (index % 4 + 1) as u32);
            }
            attacks.push((attacker, AttackTarget::Player(defender)));
        }

        let iterations = 20_000;
        let start = std::time::Instant::now();
        let mut total_seen = 0;
        for _ in 0..iterations {
            let (total, per_creature) =
                compute_attack_tax(&state, &attacks).expect("benchmark board must be taxed");
            total_seen += total.mana_value() as usize + per_creature.len();
        }
        let elapsed = start.elapsed();
        eprintln!(
            "[bench] compute_attack_tax (24 attackers, 4 tax statics, {iterations} iters): {:?} total_seen={total_seen}",
            elapsed
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

    /// Issue #944 — Caesar, Legion's Emperor: reflexive Soldier tokens must
    /// attack the same defender as the active player's declared attackers, not
    /// the attacking player surfaced by `AttackersDeclared` /
    /// `PermanentSacrificed` trigger context.
    #[test]
    fn enter_attacking_matches_controller_declared_defender_not_trigger_actor() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let caesar = create_creature(&mut state, PlayerId(0), "Caesar", 4, 4);
        let token = create_creature(&mut state, PlayerId(0), "Soldier", 1, 1);

        state.combat = Some(CombatState::default());
        state
            .combat
            .as_mut()
            .unwrap()
            .attackers
            .push(AttackerInfo::new(
                attacker,
                AttackTarget::Player(PlayerId(1)),
                PlayerId(1),
            ));

        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(1),
            attacks: vec![(attacker, AttackTarget::Player(PlayerId(1)))],
        });

        enter_attacking(&mut state, token, caesar, PlayerId(0));

        let info = state
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .find(|a| a.object_id == token)
            .expect("token must be an attacking creature");
        assert_eq!(
            info.defending_player,
            PlayerId(1),
            "enters-attacking token must attack the declared defender, not its controller"
        );
        assert_eq!(info.attack_target, AttackTarget::Player(PlayerId(1)));
    }

    /// CR 508.4 + CR 613.1f: a creature put onto the battlefield attacking must
    /// dirty layers so Layer 6 FilterProp::Attacking { defender: None } grants re-evaluate. Fails on
    /// revert of the `enter_attacking` mark.
    #[test]
    fn enter_attacking_marks_layers_dirty() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let token = create_creature(&mut state, PlayerId(0), "Soldier", 1, 1);

        state.combat = Some(CombatState::default());
        state
            .combat
            .as_mut()
            .unwrap()
            .attackers
            .push(AttackerInfo::new(
                attacker,
                AttackTarget::Player(PlayerId(1)),
                PlayerId(1),
            ));

        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;
        enter_attacking(&mut state, token, attacker, PlayerId(0));

        assert!(
            state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .any(|a| a.object_id == token),
            "entered-attacking creature must be in combat.attackers"
        );
        assert!(
            state.layers_dirty.is_dirty(),
            "putting a creature onto the battlefield attacking must mark layers dirty"
        );
    }

    /// CR 702.49c + CR 702.190b + CR 613.1f: Ninjutsu/Sneak place a creature
    /// already attacking; the layers must re-evaluate Layer 6 FilterProp::Attacking { defender: None }
    /// grants. Fails on revert of the `place_attacking_alongside` mark.
    #[test]
    fn place_attacking_alongside_marks_layers_dirty() {
        let mut state = setup();
        let ninja = create_creature(&mut state, PlayerId(0), "Ninja", 2, 2);

        state.combat = Some(CombatState::default());
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        let mut events = Vec::new();
        place_attacking_alongside(
            &mut state,
            ninja,
            PlayerId(1),
            AttackTarget::Player(PlayerId(1)),
            &mut events,
        );

        assert!(
            state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .any(|a| a.object_id == ninja),
            "place_attacking_alongside must add the creature to combat.attackers"
        );
        assert!(
            state.layers_dirty.is_dirty(),
            "placing a creature already attacking must mark layers dirty"
        );
    }

    /// CR 508.1b + CR 702.19a (Oviya, Automech Artisan): the static
    /// "Each creature that's attacking one of your opponents has trample" must
    /// parse to a `Continuous` static whose affected filter carries
    /// `Attacking { defender: Some(Opponent) }` and grant Trample only to
    /// creatures attacking the controller's opponent — not to a creature that
    /// isn't attacking. Drives the REAL static parser (`parse_static_line`) →
    /// `evaluate_layers`.
    ///
    /// REVERT-PROOF: reverting the `parse_attacking_defender_suffix` "that's
    /// attacking one of your opponents" extension leaves the static line at
    /// `Effect::Unimplemented` (no `StaticDefinition` produced), so
    /// `parse_static_line` returns `None`, the `.expect` below panics, and the
    /// grant never reaches the attacker.
    #[test]
    fn oviya_grants_trample_only_to_creatures_attacking_an_opponent() {
        use crate::game::layers::evaluate_layers;
        use crate::types::keywords::Keyword;

        let mut state = setup();

        // Oviya on the battlefield, controlled by PlayerId(0). Static parsed
        // from its printed Oracle text.
        let oviya = create_creature(&mut state, PlayerId(0), "Oviya, Automech Artisan", 2, 2);
        let def =
            parse_static_line("Each creature that's attacking one of your opponents has trample.")
                .expect("Oviya static line must parse to a StaticDefinition");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::Attacking {
                    defender: Some(ControllerRef::Opponent),
                }]
            ))),
            "affected filter must scope to creatures attacking an opponent"
        );
        state
            .objects
            .get_mut(&oviya)
            .unwrap()
            .static_definitions
            .push(def);

        // An attacker controlled by PlayerId(0) attacking the opponent.
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        // A second creature that is NOT attacking — the negative control.
        let idle = create_creature(&mut state, PlayerId(0), "Wall", 0, 4);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        assert!(
            state
                .objects
                .get(&attacker)
                .unwrap()
                .has_keyword(&Keyword::Trample),
            "a creature attacking the controller's opponent must gain trample"
        );
        assert!(
            !state
                .objects
                .get(&idle)
                .unwrap()
                .has_keyword(&Keyword::Trample),
            "a creature that isn't attacking must NOT gain trample"
        );
    }
}
