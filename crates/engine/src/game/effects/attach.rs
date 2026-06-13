use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::AttachTarget;
use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    Effect, EffectError, EffectKind, FilterProp, ResolvedAbility, TargetFilter, TargetRef,
    TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 701.3a + CR 701.3b: Attach — to place an Aura, Equipment, or Fortification on another object or player.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let source_id = ability.source_id;
    let (attachment_filter, target_filter) = match &ability.effect {
        Effect::Attach { attachment, target } => (attachment, target),
        _ => (&TargetFilter::SelfRef, &TargetFilter::Any),
    };

    // CR 608.2h + CR 608.2k: Typed attachment operands resolve from the
    // battlefield/LKI unless they are explicit player-chosen targets.
    // Typed/scan-based attachment filters (e.g. "Equipment attached to ~") resolve
    // from the battlefield/LKI, not from explicit target slots. Consuming
    // `ability.targets` here would steal the ParentTarget bearer (Zack Fair).
    // `Any`/`Any` pairs share one iterator so [equipment, host] slots stay ordered.
    let mut target_slots = ability.targets.iter();
    let attachment_id = if attachment_filter_uses_explicit_target_slot(attachment_filter) {
        resolve_object_filter(state, ability, attachment_filter, &mut target_slots)
    } else {
        resolve_object_filter(state, ability, attachment_filter, &mut std::iter::empty())
    }
    .ok_or_else(|| EffectError::MissingParam("No attachment for Attach".to_string()))?;
    let target_id = resolve_object_filter(state, ability, target_filter, &mut target_slots)
        .ok_or_else(|| EffectError::MissingParam("No target for Attach".to_string()))?;

    // CR 303.4j: If an effect attempts to attach an Aura on the battlefield to an
    // object it can't legally enchant, the Aura doesn't move. Delegate to the single
    // COMPLETE legality authority (sba::is_valid_attachment_target) — attachment_illegality
    // (protection/prohibition) + the Aura's Enchant filter + the zone gate. A bespoke
    // Enchant-only check would silently miss zone/protection mismatches. Scoped to Aura
    // attachments so Equipment/Fortification resolution is unchanged.
    let attacher_is_aura = state
        .objects
        .get(&attachment_id)
        .is_some_and(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"));
    if attacher_is_aura
        && !crate::game::sba::is_valid_attachment_target(state, attachment_id, target_id)
    {
        // CR 303.4j: the aura doesn't move.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id,
        });
        return Ok(());
    }

    if let Some(old_target) = attach_to(state, attachment_id, target_id) {
        events.push(GameEvent::Unattached {
            attachment_id,
            old_target,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id,
    });

    Ok(())
}

/// CR 701.3d: Unattach each matching Equipment from the matched host, leaving
/// it on the battlefield but no longer attached.
pub fn resolve_unattach_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (attachment_filter, target_filter) = match &ability.effect {
        Effect::UnattachAll { attachment, target } => (attachment, target),
        _ => (&TargetFilter::Any, &TargetFilter::Any),
    };

    let target_ids = resolved_object_ids_for_filter(state, ability, target_filter);

    let ctx = FilterContext::from_ability(ability);
    for target_id in target_ids {
        let attachments = state
            .objects
            .get(&target_id)
            .map(|target| target.attachments.clone())
            .unwrap_or_default();
        for attachment_id in attachments {
            if !matches_target_filter(state, attachment_id, attachment_filter, &ctx) {
                continue;
            }
            if let Some(old_target) = unattach(state, attachment_id) {
                events.push(GameEvent::Unattached {
                    attachment_id,
                    old_target,
                });
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

pub(crate) fn target_ref_from_attach_target(target: AttachTarget) -> TargetRef {
    match target {
        AttachTarget::Object(id) => TargetRef::Object(id),
        AttachTarget::Player(id) => TargetRef::Player(id),
    }
}

fn current_attachment_target(state: &GameState, attachment_id: ObjectId) -> Option<TargetRef> {
    state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
        .map(target_ref_from_attach_target)
}

/// Only explicit attachment choices consume player-chosen target slots.
/// Scan-based filters (e.g. "Equipment that was attached to ~") resolve from
/// the battlefield or LKI and must not steal `ParentTarget` slots.
fn attachment_filter_uses_explicit_target_slot(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(tf) => !tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::AttachedToSource)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(attachment_filter_uses_explicit_target_slot),
        TargetFilter::Not { filter } => attachment_filter_uses_explicit_target_slot(filter),
        _ => false,
    }
}

fn resolve_object_filter<'a>(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    target_slots: &mut impl Iterator<Item = &'a TargetRef>,
) -> Option<ObjectId> {
    match filter {
        TargetFilter::SelfRef => Some(ability.source_id),
        TargetFilter::LastCreated => target_slots
            .find_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .or_else(|| state.last_created_token_ids.first().copied()),
        TargetFilter::TriggeringSource | TargetFilter::AttachedTo => {
            crate::game::targeting::resolve_event_context_target(state, filter, ability.source_id)
                .and_then(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                })
        }
        TargetFilter::ParentTarget => ability.targets.iter().find_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        }),
        _ => target_slots
            .find_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .or_else(|| {
                resolved_object_ids_for_filter(state, ability, filter)
                    .into_iter()
                    .next()
            })
            .or_else(|| resolve_attached_to_source_lki_attachment(state, ability, filter)),
    }
}

fn filter_has_attached_to_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::AttachedToSource)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_has_attached_to_source)
        }
        TargetFilter::Not { filter } => filter_has_attached_to_source(filter),
        _ => false,
    }
}

fn strip_attached_to_source_prop(filter: &TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(TypedFilter {
            properties: tf
                .properties
                .iter()
                .filter(|p| !matches!(p, FilterProp::AttachedToSource))
                .cloned()
                .collect(),
            ..tf.clone()
        }),
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.iter().map(strip_attached_to_source_prop).collect(),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.iter().map(strip_attached_to_source_prop).collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(strip_attached_to_source_prop(filter)),
        },
        other => other.clone(),
    }
}

/// CR 400.7j + CR 608.2h: Zack Fair — after self-sacrifice the Equipment operand
/// is no longer `AttachedToSource` on the battlefield; resolve it from the
/// source's departure attachment snapshot instead of the global filter predicate.
fn resolve_attached_to_source_lki_attachment(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<ObjectId> {
    if !filter_has_attached_to_source(filter) {
        return None;
    }
    let source_id = ability.source_id;
    let ctx = FilterContext::from_ability(ability);
    let without_attached = strip_attached_to_source_prop(filter);
    state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|record| record.object_id == source_id)
        .and_then(|record| {
            record.attachments.iter().find_map(|snap| {
                if state.objects.contains_key(&snap.object_id)
                    && matches_target_filter(state, snap.object_id, &without_attached, &ctx)
                {
                    Some(snap.object_id)
                } else {
                    None
                }
            })
        })
}

/// CR 701.3c: Attaching to a different object gives the attachment a new timestamp.
/// Core attachment logic: attach `attachment_id` to `target_id`.
/// Handles detaching from a previous target if already attached.
pub fn attach_to(
    state: &mut GameState,
    attachment_id: ObjectId,
    target_id: ObjectId,
) -> Option<TargetRef> {
    if !can_attach_to_object(state, attachment_id, target_id) {
        return None;
    }

    let old_target = current_attachment_target(state, attachment_id)
        .filter(|target| *target != TargetRef::Object(target_id));

    // CR 701.3a: Attaching moves attachment onto target.
    // If already attached to something, detach first. We only need to clear an
    // Object host's `attachments` list — a Player host has no such list.
    if let Some(old_target_id) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
        .and_then(|t| t.as_object())
    {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }

    // Set attached_to on the attachment. `From<ObjectId> for AttachTarget`
    // selects the `Object` variant; player attachment has its own entry point
    // (`attach_to_player`).
    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = Some(target_id.into());
    }

    // Add to target's attachments list
    if let Some(target) = state.objects.get_mut(&target_id) {
        if !target.attachments.contains(&attachment_id) {
            target.attachments.push(attachment_id);
        }
    }

    crate::game::layers::mark_layers_full(state);
    old_target
}

/// Why a host forbids an attachment, independent of the Enchant filter and zone.
/// This is the single authority for protection ("E" of DEBT) plus
/// can't-be-attached legality, shared by attach gates and SBA re-checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachIllegality {
    /// CR 702.16c: A protected permanent or player can't be enchanted by an
    /// Aura of the protected quality.
    /// CR 702.16d: A protected permanent can't be equipped or fortified by an
    /// attachment of the protected quality.
    Protection,
    /// CR 303.4c: Other applicable effects can make an Aura's host illegal.
    /// CR 701.3a: An attachment can't be attached to something it couldn't
    /// enchant, equip, or fortify.
    Prohibited,
}

/// Returns `Some(reason)` when an object host forbids `attachment` via
/// protection or a can't-be-attached static, else `None`.
///
/// Does NOT evaluate the Enchant filter or zone — that legality is applied by
/// the caller (`is_valid_attachment_target` checks it for the SBA path). The
/// checks here are purely additive prohibitions that apply equally at attach
/// time and continuously thereafter, so routing both paths through this resolver
/// keeps them from drifting.
pub(crate) fn attachment_illegality(
    state: &GameState,
    attachment_id: ObjectId,
    host_id: ObjectId,
) -> Option<AttachIllegality> {
    // CR 301.5c: "An Equipment can't equip itself." (And no permanent can be
    // attached to itself.) Single-authority self-attach guard protecting both
    // `can_attach_to_object` and `attach_to`.
    if attachment_id == host_id {
        return Some(AttachIllegality::Prohibited);
    }
    // CR 701.3a: `CantBeAttached` blocks any attachment from being attached to
    // the host.
    if crate::game::static_abilities::object_has_static_other(state, host_id, "CantBeAttached") {
        return Some(AttachIllegality::Prohibited);
    }
    let (attacher_is_aura, attacher_is_equipment) =
        state
            .objects
            .get(&attachment_id)
            .map_or((false, false), |obj| {
                (
                    obj.card_types.subtypes.iter().any(|s| s == "Aura"),
                    obj.card_types.subtypes.iter().any(|s| s == "Equipment"),
                )
            });
    // CR 303.4c: Other applicable effects can make an Aura's host illegal.
    if attacher_is_aura
        && crate::game::static_abilities::object_has_static_other(state, host_id, "CantBeEnchanted")
    {
        return Some(AttachIllegality::Prohibited);
    }
    // CR 701.3a: Equipment can't be attached to an object it can't equip.
    if attacher_is_equipment
        && crate::game::static_abilities::object_has_static_other(state, host_id, "CantBeEquipped")
    {
        return Some(AttachIllegality::Prohibited);
    }

    // CR 301.5 + CR 303.4 + CR 701.3a: Positive attachment restriction. An
    // Aura/Equipment that "can be attached only to {filter}" may only attach to a
    // host matching that filter. Unlike the `CantBe*` host prohibitions above
    // (read from the HOST's statics), this restriction is carried by the
    // ATTACHMENT itself, so a candidate host failing the filter makes the attach
    // illegal (CR 301.5b / CR 303.4j: the attachment doesn't move).
    if !attachment_satisfies_restrictions(state, attachment_id, host_id) {
        return Some(AttachIllegality::Prohibited);
    }

    // CR 702.16c: Protection from a quality prevents Auras of that quality from
    // being attached to the protected permanent.
    // CR 702.16d: Protection from a quality prevents Equipment or Fortifications
    // of that quality from being attached to the protected permanent.
    if let (Some(host), Some(attachment)) = (
        state.objects.get(&host_id),
        state.objects.get(&attachment_id),
    ) {
        if crate::game::keywords::protection_prevents_from(host, attachment) {
            return Some(AttachIllegality::Protection);
        }
    }

    None
}

/// CR 301.5 + CR 303.4 + CR 701.3a: True unless `host_id` is forbidden by a
/// positive "can be attached only to {filter}" restriction on `attachment_id`.
///
/// The restriction is a `StaticMode::AttachmentRestriction { filter }` carried by
/// the attachment's own `static_definitions`. By analogy to CR 702.5c (an Aura
/// with multiple enchant instances must satisfy ALL of them), every active
/// restriction must match: the host is legal only if it matches the `filter` of
/// every `AttachmentRestriction` the attachment has. An attachment with no such
/// restriction is unconstrained here and returns `true`.
fn attachment_satisfies_restrictions(
    state: &GameState,
    attachment_id: ObjectId,
    host_id: ObjectId,
) -> bool {
    let Some(attachment) = state.objects.get(&attachment_id) else {
        return true;
    };
    let ctx = FilterContext::from_source(state, attachment_id);
    crate::game::functioning_abilities::active_static_definitions(state, attachment).all(|def| {
        match &def.mode {
            crate::types::statics::StaticMode::AttachmentRestriction { filter } => {
                matches_target_filter(state, host_id, filter, &ctx)
            }
            // Any other static imposes no positive attachment constraint.
            _ => true,
        }
    })
}

/// Returns `Some(reason)` when a player host forbids `attachment` via
/// player-scoped protection, else `None`.
pub(crate) fn player_attachment_illegality(
    state: &GameState,
    attachment_id: ObjectId,
    host: PlayerId,
) -> Option<AttachIllegality> {
    // CR 702.16c: A player with protection can't be enchanted by an Aura of the
    // protected quality.
    if crate::game::static_abilities::player_protection_from(state, host, Some(attachment_id)) {
        return Some(AttachIllegality::Protection);
    }
    None
}

pub(crate) fn can_attach_to_object(
    state: &GameState,
    attachment_id: ObjectId,
    target_id: ObjectId,
) -> bool {
    // CR 701.3a: A blocked attachment is not a legal host for an attach effect.
    attachment_illegality(state, attachment_id, target_id).is_none()
}

pub(crate) fn can_attach_to_player(
    state: &GameState,
    attachment_id: ObjectId,
    target_player: PlayerId,
) -> bool {
    // CR 303.4c: A player who has left the game is an illegal Aura host.
    if !state
        .players
        .get(target_player.0 as usize)
        .is_some_and(|p| !p.is_eliminated)
    {
        return false;
    }
    // CR 702.16c: Protection from a quality prevents Auras of that quality from
    // being attached to the protected player.
    player_attachment_illegality(state, attachment_id, target_player).is_none()
}

/// CR 303.4: Attach an Aura to a player (Curse cycle, Faith's Fetters-class).
/// Mirrors `attach_to`'s "detach from previous host" cleanup for Object hosts,
/// but no host-side `attachments` list is touched (a player is not a
/// `GameObject` and has no such field).
///
/// CR 303.4i: An Aura can't enter attached to a player it can't legally
/// enchant.
/// CR 301.5: Equipment can't legally be attached to a player.
/// Mirroring `attach_to`'s silent-no-op gating pattern, an illegal
/// Aura/Equipment pairing here is a no-op rather than an error: a caller that
/// has already validated the source sees no change in state, and a buggy caller
/// that hasn't validated cannot drive the engine into an illegal state.
pub fn attach_to_player(
    state: &mut GameState,
    attachment_id: ObjectId,
    target_player: PlayerId,
) -> Option<TargetRef> {
    // CR 301.5: Equipment or Fortification cannot attach to a player.
    // CR 303.4: Only Auras may have a player host. Any non-Aura attachment is
    // silently rejected here so the only paths into a `Player` `attached_to`
    // value are legitimate Aura attachments. The Equipment/Fortification check
    // is redundant given the Aura whitelist but is named explicitly so future
    // attachment subtypes cannot slip through by
    // accident — the contract is "Auras only", not "anything that isn't
    // currently equipment".
    let is_aura = state
        .objects
        .get(&attachment_id)
        .is_some_and(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"));
    if !is_aura {
        return None;
    }
    if !can_attach_to_player(state, attachment_id, target_player) {
        return None;
    }

    let old_target = current_attachment_target(state, attachment_id)
        .filter(|target| *target != TargetRef::Player(target_player));

    // CR 701.3a: If already attached to an object, detach from that object's
    // `attachments` list. Re-attaching to a player has no symmetric cleanup —
    // the previous Player host has no list to clear.
    if let Some(AttachTarget::Object(old_target_id)) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
    {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }

    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = Some(AttachTarget::Player(target_player));
    }

    crate::game::layers::mark_layers_full(state);
    old_target
}

/// CR 701.3d: Move an attachment away from the object or player it was attached
/// to while it remains on the battlefield. This is the single graph update
/// primitive for explicit unattach costs and effects.
pub(crate) fn unattach(state: &mut GameState, attachment_id: ObjectId) -> Option<TargetRef> {
    let old_target = current_attachment_target(state, attachment_id)?;
    let old_target_id = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
        .and_then(|target| target.as_object());

    if let Some(old_target_id) = old_target_id {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }
    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = None;
    }
    crate::game::layers::mark_layers_full(state);
    Some(old_target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AttachmentKind, ControllerRef, FilterProp, StaticDefinition, TargetFilter, TargetRef,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{AttachmentSnapshot, ZoneChangeRecord};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    /// Build a permanent with the given subtype on the battlefield.
    fn spawn_with_subtype(state: &mut GameState, name: &str, subtype: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.subtypes.push(subtype.to_string());
        id
    }

    fn spawn_creature(state: &mut GameState, name: &str) -> ObjectId {
        spawn_creature_for(state, name, PlayerId(0))
    }

    fn spawn_creature_for(state: &mut GameState, name: &str, owner: PlayerId) -> ObjectId {
        let id = create_object(state, CardId(2), owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    fn apply_static(state: &mut GameState, id: ObjectId, mode_name: &str) {
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::Other(mode_name.to_string()))
                .affected(TargetFilter::SelfRef),
        );
    }

    #[test]
    fn attachment_illegality_protection_blocks_aura() {
        // CR 702.16c: a host with protection from white forbids a white Aura.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Pacifism", "Aura");
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.color.push(crate::types::mana::ManaColor::White);
        }
        let creature = spawn_creature(&mut state, "Bear");
        state.objects.get_mut(&creature).unwrap().keywords.push(
            crate::types::keywords::Keyword::Protection(
                crate::types::keywords::ProtectionTarget::Color(
                    crate::types::mana::ManaColor::White,
                ),
            ),
        );

        assert_eq!(
            attachment_illegality(&state, aura, creature),
            Some(AttachIllegality::Protection)
        );
        assert!(!can_attach_to_object(&state, aura, creature));
    }

    #[test]
    fn attachment_illegality_cant_be_enchanted_blocks_aura() {
        // CR 303.4c: other applicable effects can make an Aura's host illegal.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let creature = spawn_creature(&mut state, "Bear");
        apply_static(&mut state, creature, "CantBeEnchanted");

        assert_eq!(
            attachment_illegality(&state, aura, creature),
            Some(AttachIllegality::Prohibited)
        );
        assert!(!can_attach_to_object(&state, aura, creature));
    }

    #[test]
    fn player_attachment_illegality_protection_blocks_aura() {
        // CR 702.16c: a player with protection from everything can't be
        // enchanted by an Aura.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Curse", "Aura");
        state.add_transient_continuous_effect(
            aura,
            PlayerId(0),
            crate::types::ability::Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![crate::types::ability::ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Protection(
                    crate::types::keywords::ProtectionTarget::Everything,
                ),
            }],
            None,
        );

        assert_eq!(
            player_attachment_illegality(&state, aura, PlayerId(1)),
            Some(AttachIllegality::Protection)
        );
        assert!(!can_attach_to_player(&state, aura, PlayerId(1)));
    }

    #[test]
    fn attachment_illegality_none_for_legal_host() {
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let creature = spawn_creature(&mut state, "Bear");

        assert_eq!(attachment_illegality(&state, aura, creature), None);
        assert!(can_attach_to_object(&state, aura, creature));
    }

    #[test]
    fn test_attach_sets_attached_to_and_attachments() {
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .subtypes
            .push("Equipment".to_string());

        let creature_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        attach_to(&mut state, equipment_id, creature_id);

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_id))
        );
        assert!(state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn multiple_distinct_equipment_can_attach_to_same_creature() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let creature = spawn_creature(&mut state, "Bear");

        attach_to(&mut state, sword, creature);
        attach_to(&mut state, shield, creature);

        assert_eq!(
            state.objects.get(&sword).unwrap().attached_to,
            Some(AttachTarget::Object(creature))
        );
        assert_eq!(
            state.objects.get(&shield).unwrap().attached_to,
            Some(AttachTarget::Object(creature))
        );
        assert_eq!(
            state.objects.get(&creature).unwrap().attachments,
            vec![sword, shield]
        );
    }

    #[test]
    fn test_attach_re_equip_moves_equipment() {
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );

        let creature_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let creature_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Attach to creature A
        attach_to(&mut state, equipment_id, creature_a);
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_a))
        );

        // Re-equip to creature B
        let old_target = attach_to(&mut state, equipment_id, creature_b);
        assert_eq!(old_target, Some(TargetRef::Object(creature_a)));

        // Should be attached to B now
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_b))
        );
        assert!(state
            .objects
            .get(&creature_b)
            .unwrap()
            .attachments
            .contains(&equipment_id));

        // Should no longer be on A's attachments
        assert!(!state
            .objects
            .get(&creature_a)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn reattach_to_same_creature_returns_no_unattach_target() {
        let mut state = setup();
        let equipment_id = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let creature_id = spawn_creature(&mut state, "Bear");

        assert_eq!(attach_to(&mut state, equipment_id, creature_id), None);
        assert_eq!(attach_to(&mut state, equipment_id, creature_id), None);
    }

    #[test]
    fn unattach_returns_previous_host() {
        let mut state = setup();
        let equipment_id = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let creature_id = spawn_creature(&mut state, "Bear");
        attach_to(&mut state, equipment_id, creature_id);

        let old_target = unattach(&mut state, equipment_id);

        assert_eq!(old_target, Some(TargetRef::Object(creature_id)));
        assert_eq!(state.objects.get(&equipment_id).unwrap().attached_to, None);
        assert!(!state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn unattach_all_removes_matching_equipment_from_target() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let aura = spawn_with_subtype(&mut state, "Pacifism", "Aura");
        let creature = spawn_creature(&mut state, "Bear");
        attach_to(&mut state, sword, creature);
        attach_to(&mut state, shield, creature);
        attach_to(&mut state, aura, creature);

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(creature)],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects.get(&sword).unwrap().attached_to, None);
        assert_eq!(state.objects.get(&shield).unwrap().attached_to, None);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(AttachTarget::Object(creature))
        );
        assert_eq!(
            state.objects.get(&creature).unwrap().attachments,
            vec![aura]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::Unattached { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn unattach_all_parent_target_removes_equipment_from_each_parent_host() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let bear = spawn_creature(&mut state, "Bear");
        let elf = spawn_creature(&mut state, "Elf");
        attach_to(&mut state, sword, bear);
        attach_to(&mut state, shield, elf);

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(bear), TargetRef::Object(elf)],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects.get(&sword).unwrap().attached_to, None);
        assert_eq!(state.objects.get(&shield).unwrap().attached_to, None);
        assert!(state.objects.get(&bear).unwrap().attachments.is_empty());
        assert!(state.objects.get(&elf).unwrap().attachments.is_empty());
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::Unattached { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn unattach_all_empty_target_set_resolves_noop() {
        let mut state = setup();
        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature()
                        .controller(ControllerRef::Opponent),
                ),
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            events.as_slice(),
            [GameEvent::EffectResolved {
                kind: EffectKind::UnattachAll,
                source_id: ObjectId(999)
            }]
        ));
    }

    #[test]
    fn unattach_all_filters_explicit_object_targets() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let own_creature = spawn_creature(&mut state, "Bear");
        let opponent_creature = spawn_creature_for(&mut state, "Elf", PlayerId(1));
        attach_to(&mut state, sword, own_creature);
        attach_to(&mut state, shield, opponent_creature);

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().controller(ControllerRef::You),
                ),
            },
            vec![
                TargetRef::Object(own_creature),
                TargetRef::Object(opponent_creature),
            ],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects.get(&sword).unwrap().attached_to, None);
        assert_eq!(
            state.objects.get(&shield).unwrap().attached_to,
            Some(AttachTarget::Object(opponent_creature))
        );
        assert!(state
            .objects
            .get(&opponent_creature)
            .unwrap()
            .attachments
            .contains(&shield));
    }

    #[test]
    fn cant_be_attached_blocks_any_attachment() {
        // CR 701.3: "Can't be attached" blocks any attachment (Aura/Equipment).
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let victim = spawn_creature(&mut state, "Victim");
        apply_static(&mut state, victim, "CantBeAttached");

        attach_to(&mut state, aura, victim);

        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);
        assert!(state.objects.get(&victim).unwrap().attachments.is_empty());
    }

    #[test]
    fn cant_be_enchanted_blocks_aura() {
        // CR 702.5: "Can't be enchanted" blocks Auras specifically.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Pacifism", "Aura");
        let victim = spawn_creature(&mut state, "Kira");
        apply_static(&mut state, victim, "CantBeEnchanted");

        attach_to(&mut state, aura, victim);

        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);
    }

    #[test]
    fn cant_be_equipped_blocks_equipment() {
        // CR 702.6: "Can't be equipped" blocks Equipment specifically.
        let mut state = setup();
        let equipment = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let victim = spawn_creature(&mut state, "Skittering Surveyor");
        apply_static(&mut state, victim, "CantBeEquipped");

        attach_to(&mut state, equipment, victim);

        assert_eq!(state.objects.get(&equipment).unwrap().attached_to, None);
    }

    #[test]
    fn attach_resolves_last_created_target() {
        // CR 702.182a: Attach sub-ability with TargetFilter::LastCreated resolves
        // target from state.last_created_token_ids (Job select pattern).
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rod".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .subtypes
            .push("Equipment".to_string());

        let token_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Hero".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&token_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.last_created_token_ids = vec![token_id];

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::LastCreated,
            },
            vec![], // No explicit targets — should fall back to LastCreated
            equipment_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(token_id))
        );
        assert!(state
            .objects
            .get(&token_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn attach_with_explicit_targets_ignores_last_created() {
        // Regression: when explicit targets exist, LastCreated on the effect
        // should NOT be used — explicit targets take precedence.
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        let creature_a = spawn_creature(&mut state, "Bear A");
        let creature_b = spawn_creature(&mut state, "Bear B");
        state.last_created_token_ids = vec![creature_b];

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::LastCreated,
            },
            vec![crate::types::ability::TargetRef::Object(creature_a)],
            equipment_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should attach to creature_a (explicit target), not creature_b (LastCreated)
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_a))
        );
    }

    #[test]
    fn attach_resolves_non_source_attachment_from_target_slot() {
        let mut state = setup();
        let source_id = spawn_creature(&mut state, "Windwalker");
        let equipment_id = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let creature_id = spawn_creature(&mut state, "Bear");

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Any,
                target: TargetFilter::Any,
            },
            vec![
                crate::types::ability::TargetRef::Object(equipment_id),
                crate::types::ability::TargetRef::Object(creature_id),
            ],
            source_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_id))
        );
        assert!(state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
        assert_eq!(state.objects.get(&source_id).unwrap().attached_to, None);
    }

    #[test]
    fn attach_prohibitions_distinguish_aura_vs_equipment() {
        // CantBeEnchanted allows Equipment; CantBeEquipped allows Auras;
        // CantBeAttached blocks both.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let equipment = spawn_with_subtype(&mut state, "Sword", "Equipment");

        // Case A: CantBeEnchanted creature accepts Equipment.
        let cant_be_enchanted = spawn_creature(&mut state, "Kira");
        apply_static(&mut state, cant_be_enchanted, "CantBeEnchanted");
        attach_to(&mut state, equipment, cant_be_enchanted);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(cant_be_enchanted)),
            "Equipment should attach to a creature with CantBeEnchanted"
        );
        // Aura is rejected
        attach_to(&mut state, aura, cant_be_enchanted);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            None,
            "Aura should be blocked by CantBeEnchanted"
        );

        // Case B: CantBeEquipped creature accepts Auras.
        let cant_be_equipped = spawn_creature(&mut state, "Citanul Druid");
        apply_static(&mut state, cant_be_equipped, "CantBeEquipped");
        attach_to(&mut state, aura, cant_be_equipped);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(AttachTarget::Object(cant_be_equipped)),
            "Aura should attach to a creature with CantBeEquipped"
        );

        // Case C: CantBeAttached creature rejects both.
        // Detach the aura and equipment first by removing the static from earlier cases.
        let cant_be_attached = spawn_creature(&mut state, "Warded Keep");
        apply_static(&mut state, cant_be_attached, "CantBeAttached");
        let aura2 = spawn_with_subtype(&mut state, "Aura2", "Aura");
        let equipment2 = spawn_with_subtype(&mut state, "Sword2", "Equipment");
        attach_to(&mut state, aura2, cant_be_attached);
        attach_to(&mut state, equipment2, cant_be_attached);
        assert_eq!(state.objects.get(&aura2).unwrap().attached_to, None);
        assert_eq!(state.objects.get(&equipment2).unwrap().attached_to, None);
    }

    /// Add a positive `AttachmentRestriction` static to an attachment, carrying
    /// the given legal-host `TargetFilter`.
    fn apply_attach_restriction(state: &mut GameState, id: ObjectId, filter: TargetFilter) {
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::AttachmentRestriction { filter })
                .affected(TargetFilter::SelfRef),
        );
    }

    /// Build a creature with the given power on the battlefield.
    fn spawn_creature_with_power(state: &mut GameState, name: &str, power: i32) -> ObjectId {
        let id = spawn_creature(state, name);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(power);
        obj.toughness = Some(power.max(1));
        crate::game::layers::mark_layers_full(state);
        id
    }

    #[test]
    fn attachment_restriction_power_ge_blocks_weak_host_allows_strong_host() {
        // CR 301.5b + CR 701.3a: Strata Scythe class — Equipment that "can be
        // attached only to a creature with power 3 or greater" may not attach to a
        // power-2 creature, but may attach to a power-3 creature. The restriction
        // lives on the ATTACHMENT, not the host (contrast CantBeEquipped).
        let mut state = setup();
        let equipment = spawn_with_subtype(&mut state, "Strata Scythe", "Equipment");
        let power_filter = TargetFilter::Typed(
            crate::types::ability::TypedFilter::creature().properties(vec![
                crate::types::ability::FilterProp::PtComparison {
                    stat: crate::types::ability::PtStat::Power,
                    scope: crate::types::ability::PtValueScope::Current,
                    comparator: crate::types::ability::Comparator::GE,
                    value: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                },
            ]),
        );
        apply_attach_restriction(&mut state, equipment, power_filter);

        let weak = spawn_creature_with_power(&mut state, "Grizzly Bears", 2);
        let strong = spawn_creature_with_power(&mut state, "Hill Giant", 3);

        // Non-matching host (power 2) is an illegal attach target — attach is a no-op.
        assert_eq!(
            attachment_illegality(&state, equipment, weak),
            Some(AttachIllegality::Prohibited),
            "power-2 host must fail the power>=3 attachment restriction"
        );
        assert!(!can_attach_to_object(&state, equipment, weak));
        attach_to(&mut state, equipment, weak);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            None,
            "Equipment must not move onto a non-matching host (CR 701.3b)"
        );

        // Matching host (power 3) is a legal attach target.
        assert_eq!(attachment_illegality(&state, equipment, strong), None);
        assert!(can_attach_to_object(&state, equipment, strong));
        attach_to(&mut state, equipment, strong);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(strong)),
            "Equipment must attach onto a host matching the restriction filter"
        );
        assert!(state
            .objects
            .get(&strong)
            .unwrap()
            .attachments
            .contains(&equipment));
    }

    #[test]
    fn attachment_restriction_legendary_gates_aura_host() {
        // CR 303.4j + CR 701.3a: Konda's Banner class — an attachment restricted
        // to "a legendary creature" may not attach to a nonlegendary host.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Konda's Banner", "Aura");
        let legendary_filter = TargetFilter::Typed(
            crate::types::ability::TypedFilter::creature().properties(vec![
                crate::types::ability::FilterProp::HasSupertype {
                    value: crate::types::card_type::Supertype::Legendary,
                },
            ]),
        );
        apply_attach_restriction(&mut state, aura, legendary_filter);

        let nonlegendary = spawn_creature(&mut state, "Bear");
        let legendary = spawn_creature(&mut state, "Konda, Lord of Eiganjo");
        state
            .objects
            .get_mut(&legendary)
            .unwrap()
            .card_types
            .supertypes
            .push(crate::types::card_type::Supertype::Legendary);
        crate::game::layers::mark_layers_full(&mut state);

        assert!(!can_attach_to_object(&state, aura, nonlegendary));
        attach_to(&mut state, aura, nonlegendary);
        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);

        assert!(can_attach_to_object(&state, aura, legendary));
        attach_to(&mut state, aura, legendary);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(AttachTarget::Object(legendary))
        );
    }

    /// CR 301.5c: "An Equipment can't equip itself." The single-authority
    /// `attachment_illegality` self-attach guard makes `can_attach_to_object`
    /// reject a self target and `attach_to(id, id)` a no-op.
    #[test]
    fn self_attach_is_prohibited_and_no_op() {
        let mut state = setup();
        // A reconfigure Equipment is itself a creature while unattached.
        let equip = spawn_with_subtype(&mut state, "Self-Equip", "Equipment");
        state
            .objects
            .get_mut(&equip)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert_eq!(
            attachment_illegality(&state, equip, equip),
            Some(AttachIllegality::Prohibited),
            "self-attach is illegal (CR 301.5c)"
        );
        assert!(
            !can_attach_to_object(&state, equip, equip),
            "can_attach_to_object rejects self target"
        );

        assert_eq!(
            attach_to(&mut state, equip, equip),
            None,
            "attach_to(id, id) is a no-op returning None"
        );
        assert_eq!(
            state.objects.get(&equip).unwrap().attached_to,
            None,
            "self-attach leaves attached_to unset"
        );
        assert!(
            state.objects.get(&equip).unwrap().attachments.is_empty(),
            "self-attach adds nothing to attachments"
        );
    }

    #[test]
    fn attach_equipment_was_attached_to_sacrificed_source() {
        use crate::game::sacrifice::sacrifice_permanent;

        let mut state = setup();
        let zack = spawn_creature(&mut state, "Zack Fair");
        let bearer = spawn_creature(&mut state, "Bearer");
        let equipment = spawn_with_subtype(&mut state, "Hero's Sword", "Equipment");
        attach_to(&mut state, equipment, zack);

        let mut events = Vec::new();
        sacrifice_permanent(&mut state, zack, PlayerId(0), &mut events).unwrap();

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .properties(vec![FilterProp::AttachedToSource]),
                ),
                target: TargetFilter::ParentTarget,
            },
            vec![crate::types::ability::TargetRef::Object(bearer)],
            zack,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(bearer))
        );
    }

    #[test]
    fn attach_equipment_was_attached_uses_latest_departure_snapshot() {
        let mut state = setup();
        let zack = spawn_creature(&mut state, "Zack Fair");
        let bearer = spawn_creature(&mut state, "Bearer");
        let old_equipment = spawn_with_subtype(&mut state, "Old Sword", "Equipment");
        let new_equipment = spawn_with_subtype(&mut state, "New Sword", "Equipment");

        state.zone_changes_this_turn.push(ZoneChangeRecord {
            attachments: vec![AttachmentSnapshot {
                object_id: old_equipment,
                controller: PlayerId(0),
                kind: AttachmentKind::Equipment,
            }],
            ..ZoneChangeRecord::test_minimal(zack, Some(Zone::Battlefield), Zone::Graveyard)
        });
        state.zone_changes_this_turn.push(ZoneChangeRecord {
            attachments: vec![AttachmentSnapshot {
                object_id: new_equipment,
                controller: PlayerId(0),
                kind: AttachmentKind::Equipment,
            }],
            ..ZoneChangeRecord::test_minimal(zack, Some(Zone::Battlefield), Zone::Graveyard)
        });

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .properties(vec![FilterProp::AttachedToSource]),
                ),
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(bearer)],
            zack,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&new_equipment).unwrap().attached_to,
            Some(AttachTarget::Object(bearer))
        );
        assert_eq!(state.objects.get(&old_equipment).unwrap().attached_to, None);
    }
}
