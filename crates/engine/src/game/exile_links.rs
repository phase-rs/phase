use serde::Serialize;

use crate::types::ability::{CastingPermission, Duration, ResolvedAbility};
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState};
use crate::types::identifiers::ObjectId;

const LINKED_EXILE_CONSUMER_TAGS: &[&str] = &[
    "ExiledBySource",
    "CardsExiledBySource",
    "OwnersOfCardsExiledBySource",
    "ChoiceAmongExiledColors",
    "TargetSharesNameWithOtherExiledThisWay",
    // CR 601.2a + CR 113.6b: A source carrying `StaticMode::ExileCastPermission`
    // (Maralen, Fae Ascendant) consumes its own linked-exile pool to grant
    // casting permission. Detection by externally-tagged serde key ensures the
    // source-level scan (`source_contains_linked_exile_consumer`) marks the
    // permanent as a tracked-exile consumer even when the consuming reference
    // is on a static rather than on a target filter — no special-casing of the
    // static-definition shape required.
    "ExileCastPermission",
];

/// CR 607.1 / CR 607.2a + CR 406.6: A source only needs ordinary
/// `TrackedBySource` links when a typed ability on that source, or the
/// remaining resolving chain, can later refer to cards exiled with that source.
///
/// This intentionally preserves the engine's current source-level link model:
/// `ExileLink` is keyed by `source_id`, not by a printed ability identity.
/// That is less precise than CR 607's pairwise ability links, but avoids
/// displaying unrelated exile piles such as Bojuka Bog while preserving all
/// currently typed linked-exile consumers.
pub(crate) fn should_track_exiled_by_source(
    state: &GameState,
    source_id: ObjectId,
    ability: &ResolvedAbility,
) -> bool {
    ability_contains_linked_exile_consumer(ability)
        || state
            .objects
            .get(&source_id)
            .is_some_and(source_contains_linked_exile_consumer)
}

pub(crate) fn push_tracked_by_source(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
) {
    push_with_kind(state, exiled_id, source_id, ExileLinkKind::TrackedBySource);
}

/// CR 607.2a + CR 406.6: Record an exiled→source link with an explicit
/// `ExileLinkKind`, deduped on the `(exiled_id, source_id)` pair (mirrors
/// `push_tracked_by_source`, which delegates here for the plain tracked kind).
/// Used by Hideaway (`ExileLinkKind::HideawayLookable`, CR 702.75a) to mark the
/// exiled card as look-permitted for the source's controller while keeping it
/// discoverable by the kind-agnostic `ExiledBySource` companion-ability filter.
pub(crate) fn push_with_kind(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
    kind: ExileLinkKind,
) {
    if state
        .exile_links
        .iter()
        .any(|link| link.exiled_id == exiled_id && link.source_id == source_id)
    {
        return;
    }
    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind,
    });
    push_exiled_with_source_this_turn(state, exiled_id, source_id);
}

/// CR 601.2a + CR 113.6b: Record an `exiled_id` as exiled "with" `source_id`
/// during the current turn so the per-turn rolling list
/// (`GameState::cards_exiled_with_source_this_turn`) stays in lockstep with the
/// persistent `exile_links` pool. Callers that already populate `exile_links`
/// via `push_tracked_by_source` get this for free; callers that build typed
/// exile-link kinds directly (e.g. `UntilSourceLeaves`) and still need their
/// exiled cards to feed `StaticMode::ExileCastPermission` should call this
/// helper alongside the link push.
///
/// CR 607.2a: The ordering of cards in `cards_exiled_with_source_this_turn[source_id]`
/// is guaranteed to match the order they were exiled (via `Vec::push`). This is
/// an ENGINE INVARIANT, not a CR rule — the Vec::push convention ensures
/// first-in-first-out ordering for indexed access. This is critical for effects
/// like The Mimeoplasm that distinguish "the first card exiled this way" from
/// "the second card exiled this way" using indexed access.
///
/// Idempotent: a duplicate `(source_id, exiled_id)` pair is dropped, mirroring
/// `push_tracked_by_source`.
pub(crate) fn push_exiled_with_source_this_turn(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
) {
    let already_recorded = state
        .cards_exiled_with_source_this_turn
        .get(&source_id)
        .is_some_and(|entry| entry.contains(&exiled_id));
    if already_recorded {
        return;
    }

    expire_until_source_exiles_another_card_durations(state, source_id);

    let entry = state
        .cards_exiled_with_source_this_turn
        .entry(source_id)
        .or_default();
    entry.push(exiled_id);
}

// CR 611.2a + CR 607.2a: Source-linked durations expire when that same source
// exiles another card, whether stored as a play permission or a transient effect.
fn expire_until_source_exiles_another_card_durations(state: &mut GameState, source_id: ObjectId) {
    for (_, object) in state.objects.iter_mut() {
        object.casting_permissions.retain(|permission| {
            !matches!(
                permission,
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilSourceExilesAnotherCard,
                    source_id: Some(permission_source),
                    ..
                } if *permission_source == source_id
            )
        });
    }

    let before = state.transient_continuous_effects.len();
    state.transient_continuous_effects.retain(|effect| {
        !(effect.duration == Duration::UntilSourceExilesAnotherCard
            && effect.source_id == source_id)
    });
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty.mark_full();
    }
}

pub(crate) fn ability_contains_linked_exile_consumer(ability: &ResolvedAbility) -> bool {
    contains_linked_exile_consumer(ability)
}

/// CR 607.2a: True when at least two distinct cards exiled with `source_id`
/// share a name (case-insensitive).
pub(crate) fn duplicate_name_among_exiled_by_source(
    state: &GameState,
    source_id: ObjectId,
) -> bool {
    let mut names: Vec<&str> = state
        .exile_links
        .iter()
        .filter(|link| link.source_id == source_id)
        .filter_map(|link| state.objects.get(&link.exiled_id))
        .map(|obj| obj.name.as_str())
        .collect();
    names.sort_unstable();
    names
        .windows(2)
        .any(|pair| pair[0].eq_ignore_ascii_case(pair[1]))
}

/// CR 607.2a: True when `card_id` shares a name with another card linked to
/// `source_id` via `exile_links`.
pub(crate) fn shares_name_with_other_exiled_by_source(
    state: &GameState,
    source_id: ObjectId,
    card_id: ObjectId,
) -> bool {
    let Some(card) = state.objects.get(&card_id) else {
        return false;
    };
    state
        .exile_links
        .iter()
        .filter(|link| link.source_id == source_id && link.exiled_id != card_id)
        .filter_map(|link| state.objects.get(&link.exiled_id))
        .any(|other| other.name.eq_ignore_ascii_case(&card.name))
}

fn source_contains_linked_exile_consumer(obj: &crate::game::GameObject) -> bool {
    obj.abilities.iter().any(contains_linked_exile_consumer)
        || obj
            .trigger_definitions
            .iter_all()
            .any(contains_linked_exile_consumer)
        || obj
            .replacement_definitions
            .iter_all()
            .any(contains_linked_exile_consumer)
        || obj
            .static_definitions
            .iter_all()
            .any(contains_linked_exile_consumer)
        || obj
            .base_abilities
            .iter()
            .any(contains_linked_exile_consumer)
        || obj
            .base_trigger_definitions
            .iter()
            .any(contains_linked_exile_consumer)
        || obj
            .base_replacement_definitions
            .iter()
            .any(contains_linked_exile_consumer)
        || obj
            .base_static_definitions
            .iter()
            .any(contains_linked_exile_consumer)
}

fn contains_linked_exile_consumer<T: Serialize>(value: &T) -> bool {
    serde_json::to_value(value)
        .ok()
        .is_some_and(|json| contains_linked_exile_consumer_value(&json))
}

fn contains_linked_exile_consumer_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => LINKED_EXILE_CONSUMER_TAGS.contains(&s.as_str()),
        serde_json::Value::Array(values) => values.iter().any(contains_linked_exile_consumer_value),
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            LINKED_EXILE_CONSUMER_TAGS.contains(&key.as_str())
                || contains_linked_exile_consumer_value(value)
        }),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ManaProduction, PlayerFilter, QuantityExpr,
        QuantityRef, TargetFilter,
    };
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::statics::CastFrequency;
    use crate::types::zones::{EtbTapState, Zone};

    /// CR 702.167a/c: a `CraftMaterial` link must survive the craft source's
    /// battlefield exit (it self-exiles mid-activation and returns with the same
    /// ObjectId), so the returned permanent can still read what it was crafted
    /// with. A plain `TrackedBySource` link from the same source is pruned on
    /// that exit — the contrast that motivates the dedicated kind.
    #[test]
    fn craft_material_link_survives_source_battlefield_exit() {
        use crate::game::zones::{create_object, move_to_zone};
        use crate::types::game_state::{ExileLinkKind, GameState};
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(1);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Crafted Artifact".to_string(),
            Zone::Battlefield,
        );
        let material = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Craft Material".to_string(),
            Zone::Exile,
        );
        push_with_kind(&mut state, material, source, ExileLinkKind::CraftMaterial);
        let tracked = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Tracked".to_string(),
            Zone::Exile,
        );
        push_with_kind(&mut state, tracked, source, ExileLinkKind::TrackedBySource);

        // The craft source self-exiles mid-activation (battlefield -> exile).
        let mut events = Vec::new();
        move_to_zone(&mut state, source, Zone::Exile, &mut events);

        assert!(
            state.exile_links.iter().any(|l| l.exiled_id == material
                && l.source_id == source
                && matches!(l.kind, ExileLinkKind::CraftMaterial)),
            "CraftMaterial link must survive the source's battlefield exit"
        );
        assert!(
            !state
                .exile_links
                .iter()
                .any(|l| l.exiled_id == tracked && l.source_id == source),
            "TrackedBySource link must be pruned on the source's battlefield exit"
        );
    }

    fn play_from_exile_permission(duration: Duration, source_id: ObjectId) -> CastingPermission {
        CastingPermission::PlayFromExile {
            duration,
            granted_to: PlayerId(0),
            frequency: CastFrequency::Unlimited,
            source_id: Some(source_id),
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: EtbTapState::Unspecified,
            invalidation: None,
        }
    }

    #[test]
    fn source_exile_duration_expires_previous_permission_on_next_source_exile() {
        use crate::game::zones::create_object;
        use crate::types::game_state::GameState;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(1);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let other_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Source".to_string(),
            Zone::Battlefield,
        );
        let first = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "First Exiled Card".to_string(),
            Zone::Exile,
        );
        let second = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Second Exiled Card".to_string(),
            Zone::Exile,
        );
        let other_card = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Other Exiled Card".to_string(),
            Zone::Exile,
        );

        push_exiled_with_source_this_turn(&mut state, first, source);
        state
            .objects
            .get_mut(&first)
            .unwrap()
            .casting_permissions
            .extend([
                play_from_exile_permission(Duration::UntilSourceExilesAnotherCard, source),
                play_from_exile_permission(Duration::Permanent, source),
            ]);
        state
            .objects
            .get_mut(&other_card)
            .unwrap()
            .casting_permissions
            .push(play_from_exile_permission(
                Duration::UntilSourceExilesAnotherCard,
                other_source,
            ));
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilSourceExilesAnotherCard,
            TargetFilter::SelfRef,
            vec![],
            None,
        );
        state.add_transient_continuous_effect(
            other_source,
            PlayerId(0),
            Duration::UntilSourceExilesAnotherCard,
            TargetFilter::SelfRef,
            vec![],
            None,
        );

        push_exiled_with_source_this_turn(&mut state, first, source);
        assert_eq!(
            state.objects[&first].casting_permissions.len(),
            2,
            "duplicate source/exiled pair must not expire its own freshly granted permission"
        );
        assert_eq!(
            state.transient_continuous_effects.len(),
            2,
            "duplicate source/exiled pair must not expire source-event durations"
        );

        push_exiled_with_source_this_turn(&mut state, second, source);

        let first_permissions = &state.objects[&first].casting_permissions;
        assert_eq!(first_permissions.len(), 1);
        assert!(
            matches!(
                first_permissions.as_slice(),
                [CastingPermission::PlayFromExile {
                    duration: Duration::Permanent,
                    ..
                }]
            ),
            "second source exile should prune only the source-exile duration grant, got {first_permissions:?}"
        );
        assert_eq!(
            state.objects[&other_card].casting_permissions.len(),
            1,
            "same duration from a different source must survive"
        );
        assert_eq!(
            state.transient_continuous_effects.len(),
            1,
            "source-event transient duration from a different source must survive"
        );
        assert_eq!(
            state.transient_continuous_effects[0].source_id,
            other_source
        );
    }

    #[test]
    fn plain_exile_effect_has_no_linked_exile_consumer() {
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Player,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        assert!(!contains_linked_exile_consumer(&ability));
    }

    #[test]
    fn target_filter_or_branch_counts_as_linked_exile_consumer() {
        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Or {
                    filters: vec![TargetFilter::ExiledBySource, TargetFilter::Any],
                },
                without_paying_mana_cost: true,
                mode: crate::types::ability::CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
                mana_spend_permission: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        assert!(contains_linked_exile_consumer(&ability));
    }

    #[test]
    fn player_scope_counts_as_linked_exile_consumer() {
        let mut ability = ResolvedAbility::new(
            Effect::Token {
                name: "Illusion".to_string(),
                power: crate::types::ability::PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::CardsExiledBySource,
                }),
                toughness: crate::types::ability::PtValue::Quantity(QuantityExpr::Fixed {
                    value: 1,
                }),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::OwnersOfCardsExiledBySource);

        assert!(contains_linked_exile_consumer(&ability));
    }

    #[test]
    fn mana_production_counts_as_linked_exile_consumer() {
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::ChoiceAmongExiledColors {
                    source: crate::types::ability::LinkedExileScope::ThisObject,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );

        assert!(contains_linked_exile_consumer(&ability));
    }
}
