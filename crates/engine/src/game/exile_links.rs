use serde::Serialize;

use crate::types::ability::{Duration, ResolvedAbility};
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState};
use crate::types::identifiers::ObjectId;

const LINKED_EXILE_CONSUMER_TAGS: &[&str] = &[
    "ExiledBySource",
    "CardsExiledBySource",
    "OwnersOfCardsExiledBySource",
    "ChoiceAmongExiledColors",
    "TargetSharesNameWithOtherExiledThisWay",
    "SameNameAsExiledBySource",
    // CR 700.3: PileSource::ExiledThisWay — the pile-separation effect
    // consumes cards exiled earlier in the same resolution chain.
    "ExiledThisWay",
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
        || source_is_linked_exile_consumer(state, source_id)
}

/// CR 607.2b: True when `source_id`'s own printed abilities (activated,
/// triggered, replacement, or static — current or base) contain a linked-
/// exile-consumer reference (e.g. "cards exiled with [this object]"),
/// independent of whatever ability chain is currently resolving.
///
/// Shared by [`should_track_exiled_by_source`] (the ability-chain-aware
/// caller, which additionally checks whether the *resolving* ability itself
/// references the linked exile) and by
/// `zone_pipeline::apply_zone_delivery_tail`'s auto-detect for callers with no
/// `ResolvedAbility` in scope at all — a bare replacement-pipeline redirect
/// (SBA-driven death, `Effect::Destroy`, `Effect::Sacrifice` deliveries) has
/// no resolving effect chain to inspect, only the redirecting replacement's
/// source object.
pub(crate) fn source_is_linked_exile_consumer(state: &GameState, source_id: ObjectId) -> bool {
    state
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
/// A later, more specific link upgrades an existing `TrackedBySource` entry;
/// this is required when automatic linked-exile detection runs before a
/// mechanic-specific continuation such as Hideaway concealment.
/// Used by Hideaway (`ExileLinkKind::HideawayLookable`, CR 702.75a) to mark the
/// exiled card as look-permitted for the source's controller while keeping it
/// discoverable by the kind-agnostic `ExiledBySource` companion-ability filter.
pub(crate) fn push_with_kind(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
    kind: ExileLinkKind,
) {
    if let Some(existing) = state
        .exile_links
        .iter_mut()
        .find(|link| link.exiled_id == exiled_id && link.source_id == source_id)
    {
        if matches!(&existing.kind, ExileLinkKind::TrackedBySource)
            && !matches!(&kind, ExileLinkKind::TrackedBySource)
        {
            existing.kind = kind;
        }
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
        // CR 611.2a: read the lifetime through `CastingPermission::lifetime`,
        // the single place that knows which variants carry one. The hand-written
        // `PlayFromExile`-only pattern that stood here ended the duration on one
        // variant while `layers::casting_permission_duration_is_enforceable`
        // reported it enforceable for all of them — the same split between "who
        // may hold this lifetime" and "who ends it" that this change removes for
        // the turn-boundary seams.
        object.casting_permissions.retain(|permission| {
            let lifetime = permission.lifetime();
            !(lifetime.duration == Some(&Duration::UntilSourceExilesAnotherCard)
                && lifetime.source_id == Some(source_id))
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
        AbilityDefinition, AbilityKind, CastingPermission, Effect, ManaProduction, PlayerFilter,
        QuantityExpr, QuantityRef, TargetFilter,
    };
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::statics::CastFrequency;
    use crate::types::zones::{EtbTapState, Zone};

    /// CR 607.2a + CR 611.2a: the source-linked duration ends on EVERY
    /// permission variant that can carry it, not only `PlayFromExile`.
    ///
    /// This pass held the last hand-written per-variant list for a casting
    /// permission lifetime. `layers::casting_permission_duration_is_enforceable`
    /// answers `true` for `UntilSourceExilesAnotherCard` on any variant — it
    /// takes a `&Duration` and cannot say otherwise — so the pattern here was
    /// the one place that decided the answer differently. An `ExileWithAltCost`
    /// carrying the duration was ended by nothing at all.
    ///
    /// No printed card produces the duration today (zero nodes over the parsed
    /// corpus); the parser can, and both grant sites forward whatever it
    /// produces, which is why the split is closed rather than named.
    ///
    /// All three lifetime-bearing variants are present, because
    /// `CastingPermission::lifetime` answers for three: `ExileWithAltCost`,
    /// `ExileWithAltAbilityCost` (the one this change gives a `duration` field
    /// at all) and `PlayFromExile`.
    ///
    /// DISCRIMINATING: restoring the `PlayFromExile`-only pattern leaves both
    /// alternative-cost permissions in place and reds their assertions. The
    /// `PlayFromExile` half is the positive control that the old behaviour is
    /// unchanged, and the foreign-source half proves the pass still keys on the
    /// source that did the exiling.
    #[test]
    fn the_source_linked_duration_ends_on_every_permission_variant() {
        use crate::game::zones::create_object;
        use crate::types::game_state::GameState;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Linked Source".to_string(),
            Zone::Battlefield,
        );
        let other_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Unrelated Source".to_string(),
            Zone::Battlefield,
        );
        let mut exiled = |name: &str, permission: CastingPermission| {
            let id = create_object(
                &mut state,
                CardId(3),
                PlayerId(0),
                name.to_string(),
                Zone::Exile,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .casting_permissions
                .push(permission);
            id
        };

        let alt_cost = exiled(
            "Alt-cost grant",
            CastingPermission::ExileWithAltCost {
                cost: crate::types::mana::ManaCost::zero(),
                cost_provenance: crate::types::ability::ExileGrantCostProvenance::Alternative,
                cast_transformed: false,
                constraint: None,
                granted_to: Some(PlayerId(0)),
                resolution_cleanup: None,
                duration: Some(Duration::UntilSourceExilesAnotherCard),
                source_id: Some(source),
                graveyard_replacement: None,
                enters_with_counter: None,
                enters_with_modifications: Vec::new(),
                mana_spend_permission: None,
            },
        );
        let play_grant = exiled(
            "Play grant",
            CastingPermission::PlayFromExile {
                provenance: crate::types::ability::PlayFromExileProvenance::Impulse,
                mode: crate::types::ability::CardPlayMode::Play,
                duration: Duration::UntilSourceExilesAnotherCard,
                granted_to: PlayerId(0),
                frequency: CastFrequency::Unlimited,
                source_id: Some(source),
                invalidation: None,
                exiled_by_ability_controller: Some(PlayerId(0)),
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                alt_ability_cost: None,
                land_enter_tapped: EtbTapState::Unspecified,
            },
        );
        let alt_ability_cost = exiled(
            "Non-mana alt-cost grant",
            CastingPermission::ExileWithAltAbilityCost {
                cost: crate::types::ability::AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                constraint: None,
                granted_to: Some(PlayerId(0)),
                duration: Some(Duration::UntilSourceExilesAnotherCard),
                source_id: Some(source),
            },
        );
        let foreign = exiled(
            "Grant from another source",
            CastingPermission::ExileWithAltCost {
                cost: crate::types::mana::ManaCost::zero(),
                cost_provenance: crate::types::ability::ExileGrantCostProvenance::Alternative,
                cast_transformed: false,
                constraint: None,
                granted_to: Some(PlayerId(0)),
                resolution_cleanup: None,
                duration: Some(Duration::UntilSourceExilesAnotherCard),
                source_id: Some(other_source),
                graveyard_replacement: None,
                enters_with_counter: None,
                enters_with_modifications: Vec::new(),
                mana_spend_permission: None,
            },
        );

        // Production entry: the pass runs from `push_exiled_with_source_this_turn`,
        // the same call the exile pipeline makes when the source exiles its next
        // card. Calling the prune directly would test the helper, not the path.
        let next_card = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "The next card this source exiles".to_string(),
            Zone::Exile,
        );
        push_exiled_with_source_this_turn(&mut state, next_card, source);

        assert!(
            state.objects[&alt_cost].casting_permissions.is_empty(),
            "the non-mana-cost sibling carries the same duration and must end too; got {:?}",
            state.objects[&alt_cost].casting_permissions
        );
        assert!(
            state.objects[&alt_ability_cost]
                .casting_permissions
                .is_empty(),
            "the non-mana alt-cost variant — the one this change gives a `duration` \
             field at all — must end too; got {:?}",
            state.objects[&alt_ability_cost].casting_permissions
        );
        assert!(
            state.objects[&play_grant].casting_permissions.is_empty(),
            "the PlayFromExile half must keep ending as it did before; got {:?}",
            state.objects[&play_grant].casting_permissions
        );
        assert_eq!(
            state.objects[&foreign].casting_permissions.len(),
            1,
            "a grant linked to a different source must survive"
        );
    }

    /// CR 702.167a/c: a `CraftMaterial` link must survive the craft source's
    /// battlefield exit (it self-exiles mid-activation and returns with the same
    /// ObjectId), so the returned permanent can still read what it was crafted
    /// with. The contrast that motivates the dedicated kind is now a NON-exile
    /// exit: on a death (battlefield -> graveyard) `CraftMaterial` survives but a
    /// plain `TrackedBySource` link from the same source is pruned.
    ///
    /// CR 607.2a + CR 400.7: `TrackedBySource` links are NOT pruned on an exit TO
    /// EXILE — a self-exiled source stays the linked-ability referent for its
    /// pile (Mechtitan Core). The exile-exit arm below asserts that survival so a
    /// regression that reinstates the old blanket prune fails here.
    #[test]
    fn craft_material_link_survives_source_battlefield_exit() {
        use crate::game::zones::{create_object, move_to_zone};
        use crate::types::game_state::{ExileLinkKind, GameState};
        use crate::types::identifiers::CardId;

        // --- Non-exile exit (death): CraftMaterial survives, TrackedBySource pruned.
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

        let mut events = Vec::new();
        move_to_zone(&mut state, source, Zone::Graveyard, &mut events);

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
            "TrackedBySource link must be pruned on a non-exile battlefield exit (death)"
        );

        // --- Exit TO EXILE (self-exile cost): TrackedBySource survives (CR 607.2a).
        let mut state = GameState::new_two_player(1);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mechtitan Core".to_string(),
            Zone::Battlefield,
        );
        let tracked = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Exiled With Source".to_string(),
            Zone::Exile,
        );
        push_with_kind(&mut state, tracked, source, ExileLinkKind::TrackedBySource);

        let mut events = Vec::new();
        move_to_zone(&mut state, source, Zone::Exile, &mut events);

        assert!(
            state
                .exile_links
                .iter()
                .any(|l| l.exiled_id == tracked && l.source_id == source),
            "TrackedBySource link must survive the source's self-exile (CR 607.2a)"
        );
    }

    fn play_from_exile_permission(duration: Duration, source_id: ObjectId) -> CastingPermission {
        CastingPermission::PlayFromExile {
            provenance: crate::types::ability::PlayFromExileProvenance::Impulse,
            mode: crate::types::ability::CardPlayMode::Play,
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
            alt_ability_cost: None,
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

    /// CR 607.2a + CR 702.75a: automatic source tracking may create a plain
    /// link before Hideaway's conceal continuation marks the same card as
    /// lookable. The mechanic-specific kind must replace the plain marker,
    /// and a later generic push must not downgrade it again.
    #[test]
    fn specific_link_upgrades_plain_tracking_without_later_downgrade() {
        let mut state = GameState::new_two_player(1);
        let exiled = ObjectId(10);
        let source = ObjectId(20);

        push_with_kind(&mut state, exiled, source, ExileLinkKind::TrackedBySource);
        push_with_kind(&mut state, exiled, source, ExileLinkKind::HideawayLookable);
        push_with_kind(&mut state, exiled, source, ExileLinkKind::TrackedBySource);

        assert_eq!(state.exile_links.len(), 1);
        assert!(matches!(
            state.exile_links[0].kind,
            ExileLinkKind::HideawayLookable
        ));
    }

    /// CR 607.2b: `source_is_linked_exile_consumer` must detect a linked-exile
    /// reference living on an object's OWN printed ability (e.g. an activated
    /// ability targeting `TargetFilter::ExiledBySource`), independent of any
    /// currently-resolving ability chain — this is the primitive
    /// `zone_pipeline::apply_zone_delivery_tail` relies on for callers with no
    /// `ResolvedAbility` in scope (SBA-driven death, `Effect::Destroy`,
    /// `Effect::Sacrifice`).
    #[test]
    fn source_is_linked_exile_consumer_detects_own_exiled_by_source_ability() {
        use crate::game::zones::create_object;
        use crate::types::ability::TypedFilter;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(1);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "The Darkness Crystal (test)".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.abilities = std::sync::Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChangeZone {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::creature()),
                        TargetFilter::ExiledBySource,
                    ],
                },
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        )]);

        assert!(
            source_is_linked_exile_consumer(&state, source),
            "an object with an ExiledBySource-targeting ability must be detected as a linked-exile consumer"
        );

        // NEGATIVE: an unrelated ability (no ExiledBySource reference anywhere)
        // must not be detected.
        let unrelated = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Unrelated Permanent".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&unrelated).unwrap();
        obj.abilities = std::sync::Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )]);

        assert!(
            !source_is_linked_exile_consumer(&state, unrelated),
            "an object with no ExiledBySource reference must not be a linked-exile consumer"
        );
    }
}
