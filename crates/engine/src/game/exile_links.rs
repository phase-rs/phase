use serde::Serialize;

use crate::types::ability::{Duration, ResolvedAbility};
use crate::types::game_state::{
    ExileLink, ExileLinkKind, ExiledStopInput, GameState, RepeatUntilStopWitness,
};
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

/// CR 104.4b + CR 607.2a: snapshot every input the `UntilStopConditions` stop
/// predicates read for `source_id`.
///
/// Reads BOTH ledgers unconditionally — `state.cards_exiled_with_source_this_turn`
/// (which `should_stop_repeat_until`'s put-to-hand half consults) and
/// `state.exile_links` (which `duplicate_name_among_exiled_by_source`
/// immediately above consults). Reading both regardless of which stop flags are
/// set keeps the witness correct for every flag combination the grammar can
/// produce, including a body that sets only one.
///
/// Objects absent from `state.objects` are skipped, mirroring the `filter_map`
/// in `duplicate_name_among_exiled_by_source`; that is safe for the delta
/// comparison because an id that disappears from `state.objects` changes the
/// witness either way. Each ledger's rows are sorted and deduped by `ObjectId`
/// so equality never depends on that ledger's insertion order.
///
/// ROWS ARE KEYED BY `(ledger, object_id)`, NOT BY `object_id` ALONE. The two
/// stop predicates read the two ledgers SEPARATELY, so an object recorded in
/// both contributes one row to EACH vec rather than one deduped row overall.
/// Deduping the union would hold the witness byte-identical when a row is added
/// to one ledger for an object the other ledger already holds — which really
/// does move `duplicate_name_among_exiled_by_source`'s input — and would
/// therefore stop a repeat that had advanced. That is a truncation, the unsafe
/// direction to fail.
pub(crate) fn repeat_until_stop_witness(
    state: &GameState,
    source_id: ObjectId,
) -> RepeatUntilStopWitness {
    let row = |exiled_id: ObjectId| {
        state.objects.get(&exiled_id).map(|obj| ExiledStopInput {
            object_id: exiled_id,
            zone: obj.zone,
            controller: obj.controller,
            name: obj.name.clone(),
        })
    };
    let sorted = |mut rows: Vec<ExiledStopInput>| {
        rows.sort_unstable_by_key(|entry| entry.object_id);
        rows.dedup_by_key(|entry| entry.object_id);
        rows
    };

    let exiled_this_turn = sorted(
        state
            .cards_exiled_with_source_this_turn
            .get(&source_id)
            .into_iter()
            .flatten()
            .copied()
            .filter_map(row)
            .collect(),
    );
    let linked = sorted(
        state
            .exile_links
            .iter()
            .filter(|link| link.source_id == source_id)
            .map(|link| link.exiled_id)
            .filter_map(row)
            .collect(),
    );
    RepeatUntilStopWitness {
        exiled_this_turn,
        linked,
    }
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

    /// CR 607.2a + CR 104.4b: the progress witness is keyed on the resolving
    /// ability's `source_id`, exactly like both stop predicates. A witness built
    /// from the whole `exile_links` table instead of the source's slice would
    /// let one source's exiling look like another source's progress.
    ///
    /// Hostile fixture: two live sources whose exile ledgers OVERLAP on a card
    /// linked to both.
    #[test]
    fn repeat_until_stop_witness_is_scoped_to_its_source() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(7);
        let source_a = ObjectId(900);
        let source_b = ObjectId(901);
        let card_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );
        let card_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Exile,
        );
        let shared = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Shared Card".to_string(),
            Zone::Exile,
        );

        push_tracked_by_source(&mut state, card_a, source_a);
        push_tracked_by_source(&mut state, shared, source_a);
        push_tracked_by_source(&mut state, card_b, source_b);
        push_tracked_by_source(&mut state, shared, source_b);

        let witness_a = repeat_until_stop_witness(&state, source_a);
        let witness_b = repeat_until_stop_witness(&state, source_b);

        // Positive reach-guard: both witnesses actually observed rows in both
        // ledgers, so "they differ" is not two near-empty vectors compared
        // vacuously.
        assert_eq!(
            (witness_a.exiled_this_turn.len(), witness_a.linked.len()),
            (2, 2),
            "source A witnesses exactly its own two cards in each ledger, got {witness_a:?}"
        );
        assert_eq!(
            (witness_b.exiled_this_turn.len(), witness_b.linked.len()),
            (2, 2),
            "source B witnesses exactly its own two cards in each ledger, got {witness_b:?}"
        );
        assert_ne!(
            witness_a, witness_b,
            "two sources with different exile ledgers must not share a witness"
        );

        // Source A exiling one more card must leave B's witness byte-identical:
        // A's progress is not B's progress.
        let later = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Later Card".to_string(),
            Zone::Exile,
        );
        push_tracked_by_source(&mut state, later, source_a);
        assert_eq!(
            repeat_until_stop_witness(&state, source_b),
            witness_b,
            "source A exiling a card must not move source B's witness"
        );
        assert_ne!(
            repeat_until_stop_witness(&state, source_a),
            witness_a,
            "source A exiling a card must move source A's own witness"
        );
    }

    /// CR 607.2a + CR 104.4b: witness equality must not depend on either
    /// ledger's insertion order, an object recorded in BOTH ledgers contributes
    /// one row to EACH ledger's vec, and an object recorded twice within ONE
    /// ledger still contributes a single row there. `push_tracked_by_source`
    /// never writes such a duplicate, but a restored snapshot's ledgers are not
    /// validated for it, so the fixture writes one directly. A witness built by
    /// concatenating `cards_exiled_with_source_this_turn` and `exile_links`
    /// fails every half.
    #[test]
    fn repeat_until_stop_witness_is_order_independent_and_deduped() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let source = ObjectId(900);
        let stage = |link_order: [usize; 3]| {
            let mut state = GameState::new_two_player(7);
            let ids: Vec<ObjectId> = ["Alpha", "Beta", "Gamma"]
                .iter()
                .enumerate()
                .map(|(idx, name)| {
                    create_object(
                        &mut state,
                        CardId(idx as u64 + 1),
                        PlayerId(0),
                        (*name).to_string(),
                        Zone::Exile,
                    )
                })
                .collect();
            for idx in link_order {
                push_tracked_by_source(&mut state, ids[idx], source);
            }
            (state, ids)
        };

        let (mut forward, ids) = stage([0, 1, 2]);
        let (reverse, reverse_ids) = stage([2, 1, 0]);
        assert_eq!(
            ids, reverse_ids,
            "precondition: both stagings must allocate the same object ids"
        );

        // Intra-ledger duplicates, written directly because
        // `push_tracked_by_source` dedups each `(exiled_id, source_id)` pair.
        let duplicate_link = forward
            .exile_links
            .iter()
            .find(|link| link.source_id == source && link.exiled_id == ids[0])
            .cloned()
            .expect("precondition: the first card is linked to the source");
        forward.exile_links.push(duplicate_link);
        forward
            .cards_exiled_with_source_this_turn
            .get_mut(&source)
            .expect("precondition: the source has a per-turn ledger")
            .push(ids[0]);

        // Reach-guard: each card really is recorded in BOTH ledgers, and the
        // first one twice in each, so the dedup path below is genuinely
        // exercised.
        assert_eq!(
            forward
                .exile_links
                .iter()
                .filter(|link| link.source_id == source)
                .count(),
            4,
            "precondition: three cards linked to the source, one of them twice"
        );
        assert_eq!(
            forward.cards_exiled_with_source_this_turn[&source].len(),
            4,
            "precondition: the per-turn ledger holds the same duplicate"
        );

        let forward_witness = repeat_until_stop_witness(&forward, source);
        let reverse_witness = repeat_until_stop_witness(&reverse, source);

        assert_eq!(
            (
                forward_witness.exiled_this_turn.len(),
                forward_witness.linked.len()
            ),
            (3, 3),
            "one row per distinct object PER LEDGER, not one per ledger entry, got {forward_witness:?}"
        );
        assert_eq!(
            forward_witness, reverse_witness,
            "witness equality must not depend on ledger insertion order"
        );
    }

    /// CR 104.4b + CR 608.2c: the guard's real predicate is *the stop-predicate
    /// inputs are unchanged*, not *the exiled set did not grow*. Both fields
    /// that exist for that distinction are pinned here.
    ///
    /// Case 1 (`zone`): an already-exiled card moving to hand adds no ledger
    /// row — a count-only or id-only witness cannot see it.
    ///
    /// Case 2 (`controller`) is the standing argument for
    /// `ExiledStopInput::controller`, and it is built so that DELETING that
    /// field makes this assertion fail: the card sits in `Zone::Hand` under a
    /// player who is not the repeat's controller, so
    /// `should_stop_repeat_until`'s second conjunct
    /// (`obj.controller == ability.controller`) is false before and after, and
    /// only `controller` changes. A card reaching a *different player's* hand
    /// does NOT justify the field — `Zone` is player-agnostic
    /// (`types/zones.rs` declares a bare `Hand`), so `zone` already sees that.
    #[test]
    fn repeat_until_stop_witness_tracks_zone_and_controller_of_exiled_cards() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        // ---- Case 1: zone, with the ledgers held constant. ----
        let mut state = GameState::new_two_player(7);
        let source = ObjectId(900);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Moved Card".to_string(),
            Zone::Exile,
        );
        push_tracked_by_source(&mut state, card, source);

        let before = repeat_until_stop_witness(&state, source);
        assert_eq!(
            (before.exiled_this_turn.len(), before.linked.len()),
            (1, 1),
            "reach-guard: the pre-move witness must be non-empty"
        );

        state.objects.get_mut(&card).expect("card exists").zone = Zone::Hand;
        let after = repeat_until_stop_witness(&state, source);
        assert_eq!(
            (after.exiled_this_turn.len(), after.linked.len()),
            (1, 1),
            "precondition: the move added no ledger row — only the zone changed"
        );
        assert_ne!(
            before, after,
            "an exiled card changing zone must move the witness"
        );

        // ---- Case 2: controller, with zone AND name held constant. ----
        // Three players so the post-change controller is a real seat.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 7);
        let held = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Held Card".to_string(),
            Zone::Hand,
        );
        push_tracked_by_source(&mut state, held, source);
        state
            .objects
            .get_mut(&held)
            .expect("card exists")
            .controller = PlayerId(1);

        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let before = repeat_until_stop_witness(&state, source);
        assert_eq!(
            (before.exiled_this_turn.len(), before.linked.len()),
            (1, 1),
            "reach-guard: the pre-change witness must be non-empty"
        );
        assert!(
            !crate::game::effects::should_stop_repeat_until(&state, &ability, true, true),
            "precondition: the card is in another player's hand, so the printed \
             put-to-hand predicate is false"
        );

        state
            .objects
            .get_mut(&held)
            .expect("card exists")
            .controller = PlayerId(2);
        let after = repeat_until_stop_witness(&state, source);
        assert!(
            !crate::game::effects::should_stop_repeat_until(&state, &ability, true, true),
            "the printed predicate stays false across the control change — only \
             the witness can observe it"
        );
        assert_eq!(
            after.exiled_this_turn[0].zone, before.exiled_this_turn[0].zone,
            "precondition: zone is held constant across the control change"
        );
        assert_eq!(
            after.exiled_this_turn[0].name, before.exiled_this_turn[0].name,
            "precondition: name is held constant across the control change"
        );
        assert_ne!(
            before, after,
            "a control change with the zone held constant must move the witness"
        );
    }

    /// CR 607.2a + CR 104.4b: the witness keys its rows by `(ledger,
    /// object_id)`, not by `object_id` alone.
    ///
    /// The two stop predicates read the two ledgers SEPARATELY:
    /// `should_stop_repeat_until`'s put-to-hand half reads
    /// `cards_exiled_with_source_this_turn`, while
    /// `duplicate_name_among_exiled_by_source` builds its name list from
    /// `exile_links`. So adding an `exile_links` row for an object the per-turn
    /// ledger ALREADY holds really does move the duplicate-name predicate's
    /// input — and a witness that deduped the UNION of the two ledgers by
    /// `ObjectId` would be byte-identical across exactly that change, stopping a
    /// repeat that had advanced. That is a truncation, the UNSAFE direction to
    /// fail, which is why this row is pinned even though no body in today's pool
    /// reaches it (Tainted Pact's `ExileTop` writes both ledgers together via
    /// `push_with_kind`; the turn-ledger-only writers in `game/costs.rs` and
    /// `game/engine_resolution_choices.rs` sit outside any
    /// `UntilStopConditions` body).
    #[test]
    fn repeat_until_stop_witness_keys_rows_by_ledger_not_by_object_alone() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(7);
        let source = ObjectId(900);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Twice Recorded".to_string(),
            Zone::Exile,
        );

        // Turn ledger only — the shape `game/costs.rs` and
        // `game/engine_resolution_choices.rs` write.
        push_exiled_with_source_this_turn(&mut state, card, source);
        let before = repeat_until_stop_witness(&state, source);
        assert_eq!(
            (before.exiled_this_turn.len(), before.linked.len()),
            (1, 0),
            "reach-guard: the baseline really is turn-ledger-only, got {before:?}"
        );
        assert!(
            !duplicate_name_among_exiled_by_source(&state, source),
            "precondition: the duplicate-name predicate reads `exile_links`, \
             which is still empty"
        );

        // Now add the SAME object to the other ledger.
        push_with_kind(&mut state, card, source, ExileLinkKind::TrackedBySource);
        let after = repeat_until_stop_witness(&state, source);
        assert_eq!(
            (after.exiled_this_turn.len(), after.linked.len()),
            (1, 1),
            "the new row lands in the `exile_links` vec, got {after:?}"
        );

        // This is the assertion a union-deduped witness fails: the union is
        // unchanged (same single object), so only per-ledger keying sees it.
        assert_eq!(
            before.exiled_this_turn, after.exiled_this_turn,
            "precondition: the union of the two ledgers is the SAME one object \
             before and after — a witness deduping that union stays byte-identical"
        );
        assert_ne!(
            before, after,
            "a row added to one ledger for an object the other ledger already \
             holds must move the witness"
        );
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
