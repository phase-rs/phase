use serde::Serialize;

use crate::game::game_object::LinkedAbilitySource;
use crate::types::ability::{AbilityProvenance, Duration, ResolvedAbility};
use crate::types::game_state::{
    ExileLink, ExileLinkKind, ExiledStopInput, GameState, LibrarySearchDeliveryResume,
    LinkedExileLki, LinkedExileSnapshot, LookGrant, RepeatUntilStopWitness, StackEntry,
    StackEntryKind,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

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

/// CR 601.2h + CR 607.2q: The kind of a tracked exile made by `source_id`. A
/// spell being cast (announced and not yet moved onto the stack) is paying its
/// costs, so a card it exiles now was exiled to pay a cost of casting it
/// (`CastCost`); any other source's tracked exile is `TrackedBySource`.
pub(crate) fn tracked_link_kind(state: &GameState, source_id: ObjectId) -> ExileLinkKind {
    if state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.spell_announcement.is_some() && obj.zone != Zone::Stack)
    {
        ExileLinkKind::CastCost
    } else {
        ExileLinkKind::TrackedBySource
    }
}

/// CR 400.7 + CR 733.1: End `source_id`'s cast-cost links. The single authority
/// for ending a `CastCost` link: the object the cost was paid for no longer
/// exists, or its cast was undone.
pub(crate) fn end_cast_cost_links(state: &mut GameState, source_id: ObjectId) {
    state
        .exile_links
        .retain(|link| link.source_id != source_id || link.kind != ExileLinkKind::CastCost);
}

/// CR 607.2q: The cards exiled to pay the cost of casting the spell that became
/// `source_id`.
pub(crate) fn cast_cost_pile(state: &GameState, source_id: ObjectId) -> Vec<ObjectId> {
    state
        .exile_links
        .iter()
        .filter(|link| link.source_id == source_id && link.kind == ExileLinkKind::CastCost)
        .map(|link| link.exiled_id)
        .collect()
}

pub(crate) fn push_tracked_by_source(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
) {
    push_with_kind(state, exiled_id, source_id, ExileLinkKind::TrackedBySource);
}

/// CR 406.3 + CR 702.75a: the player `grant` admits now to look at a card
/// exiled by `source_id`.
pub(crate) fn look_grant_admits(
    state: &GameState,
    source_id: ObjectId,
    grant: LookGrant,
    source_incarnation: u64,
) -> Option<PlayerId> {
    match grant {
        LookGrant::SourceController => state
            .objects
            .get(&source_id)
            .filter(|src| src.zone == Zone::Battlefield && src.incarnation == source_incarnation)
            .map(|src| src.controller),
        LookGrant::Player { .. } => None,
    }
}

/// CR 406.3: Link a card just exiled face down to `source_id` with a look
/// grant; the `instructed` player and whoever `grant` admits now may look.
pub(crate) fn push_look_link(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
    grant: LookGrant,
    instructed: PlayerId,
) {
    // CR 702.75a + CR 113.7a: an old hideaway or heist exile is the old
    // object's look link (CR 400.7).
    let source_incarnation = linked_exile_incarnation(state, source_id).unwrap_or(0);
    let lookers = std::iter::once(instructed)
        .chain(look_grant_admits(
            state,
            source_id,
            grant,
            source_incarnation,
        ))
        .collect();
    push_with_kind(
        state,
        exiled_id,
        source_id,
        ExileLinkKind::HideawayLookable {
            grant,
            lookers,
            source_incarnation,
        },
    );
}

/// CR 406.3 + CR 701.23a: the searcher looked at the card before exiling it
/// face down, so they may keep looking at it.
pub(crate) fn link_search_look(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: Option<ObjectId>,
) {
    // A scoped search names a set of searchers, not the one player a fixed grant needs.
    let searcher = match &state.pending_library_search_delivery {
        Some(LibrarySearchDeliveryResume::Standard { searcher, .. }) => Some(*searcher),
        Some(LibrarySearchDeliveryResume::Scoped { .. }) | None => None,
    };
    let (Some(searcher), Some(source_id)) = (searcher, source_id) else {
        return;
    };
    if !state
        .objects
        .get(&exiled_id)
        .is_some_and(|obj| obj.zone == Zone::Exile && obj.face_down)
    {
        return;
    }
    push_look_link(
        state,
        exiled_id,
        source_id,
        LookGrant::Player { player: searcher },
        searcher,
    );
}

/// CR 400.7 + CR 607.2a: `source_id`'s links that belong to the object as it
/// now exists.
pub(crate) fn live_links_for_source(
    state: &GameState,
    source_id: ObjectId,
) -> impl Iterator<Item = &ExileLink> {
    links_for_reader(
        state,
        source_id,
        state.objects.get(&source_id).map(|obj| obj.incarnation),
    )
}

/// CR 400.7 + CR 607.2a + CR 113.7a: `source_id`'s links that belong to the
/// incarnation `reader` (see `linked_exile_incarnation`).
pub(crate) fn links_for_reader(
    state: &GameState,
    source_id: ObjectId,
    reader: Option<u64>,
) -> impl Iterator<Item = &ExileLink> {
    let live = state.objects.get(&source_id).map(|obj| obj.incarnation);
    state.exile_links.iter().filter(move |link| {
        link.source_id == source_id
            && match &link.kind {
                // CR 400.7 + CR 607.2a: a look link's card belongs to the
                // incarnation that made it.
                ExileLinkKind::HideawayLookable {
                    source_incarnation, ..
                } => reader == Some(*source_incarnation),
                // CR 400.7 + CR 607.2a: a tracked link belongs to the object that
                // now holds it (kept for its in-exile successor, Mechtitan Core);
                // an older incarnation's ability reads its own pile from the
                // linked-exile LKI. CR 607.2q: a cast-cost link belongs to the
                // spell and the permanent it became, the live object while the
                // link exists.
                ExileLinkKind::TrackedBySource | ExileLinkKind::CastCost => reader == live,
                // CR 400.7d + CR 400.7j: the object these links were made for may
                // be a later incarnation of the one that made them.
                ExileLinkKind::UntilSourceLeaves { .. }
                | ExileLinkKind::UntilOpponentBecomesMonarch { .. }
                | ExileLinkKind::ParadigmSource { .. }
                | ExileLinkKind::Cipher
                | ExileLinkKind::Haunt
                | ExileLinkKind::CraftMaterial => true,
            }
    })
}

/// CR 607.2a + CR 607.2b + CR 113.7a + CR 400.7: The linked-exile LKI piles the
/// incarnation `reader` may read (see `linked_exile_incarnation`), filtered to
/// cards still in exile as the object the entry captured (CR 400.7), so a card
/// that left exile contributes nothing, even if it was exiled again. `None`
/// admits every entry (the source no longer exists).
pub(crate) fn lki_snapshots_for_reader(
    state: &GameState,
    source_id: ObjectId,
    reader: Option<u64>,
) -> impl Iterator<Item = &LinkedExileSnapshot> {
    state
        .linked_exile_lki
        .get(&source_id)
        .into_iter()
        .flatten()
        .filter(move |lki| reader.is_none_or(|incarnation| lki.admits(incarnation)))
        .flat_map(|lki| &lki.snapshot)
        .filter(|snap| snap.is_still_exiled(state))
}

/// CR 607.2a + CR 113.7a: Whether `exiled_id` was exiled by the acting
/// incarnation of `source_id` as a tracked exile: a tracked link of that
/// incarnation (only the live object holds them), or a card kept in that
/// incarnation's linked-exile LKI because it acted after it was replaced. The
/// same-resolution "exiled this way" readers ask this.
pub(crate) fn tracked_exile_of_acting_incarnation(
    state: &GameState,
    source_id: ObjectId,
    exiled_id: ObjectId,
) -> bool {
    let reader = linked_exile_incarnation(state, source_id);
    links_for_reader(state, source_id, reader)
        .any(|link| link.exiled_id == exiled_id && link.kind == ExileLinkKind::TrackedBySource)
        || lki_snapshots_for_reader(state, source_id, reader)
            .any(|snap| snap.exiled_id == exiled_id)
}

/// CR 607.2a: `exiled_id` as an entry of a "cards exiled with" pile, or `None`
/// when it is not in exile.
pub(crate) fn exiled_card_snapshot(
    state: &GameState,
    exiled_id: ObjectId,
) -> Option<LinkedExileSnapshot> {
    state
        .objects
        .get(&exiled_id)
        .filter(|obj| obj.zone == Zone::Exile)
        .map(|obj| LinkedExileSnapshot {
            exiled_id,
            owner: obj.owner,
            // CR 202.3d + CR 709.4b: the exiled card is off the stack, so a
            // split card records its combined mana value.
            mana_value: obj.effective_mana_value(),
            incarnation: obj.incarnation,
        })
}

/// CR 406.3 + CR 613.1b: a source-controller look link latches its source's new
/// controller, since a player once allowed to look may keep looking.
pub(crate) fn latch_new_controllers(
    state: &mut GameState,
    prev_controllers: &[(ObjectId, PlayerId)],
) {
    let admitted: Vec<(usize, PlayerId)> = state
        .exile_links
        .iter()
        .enumerate()
        .filter_map(|(index, link)| {
            let ExileLinkKind::HideawayLookable {
                grant,
                lookers,
                source_incarnation,
            } = &link.kind
            else {
                return None;
            };
            let (_, prev) = prev_controllers
                .iter()
                .find(|(id, _)| *id == link.source_id)?;
            look_grant_admits(state, link.source_id, *grant, *source_incarnation)
                .filter(|player| player != prev && !lookers.contains(player))
                .map(|player| (index, player))
        })
        .collect();
    for (index, player) in admitted {
        if let ExileLinkKind::HideawayLookable { lookers, .. } = &mut state.exile_links[index].kind
        {
            lookers.insert(player);
        }
    }
}

/// CR 406.3 + CR 701.24a: a card that becomes part of a shuffled pile may be
/// looked at only by whom its link's live rule admits now.
pub(crate) fn reset_look_latches(state: &mut GameState, pile: &[ObjectId]) {
    let resets: Vec<(usize, Option<PlayerId>)> = state
        .exile_links
        .iter()
        .enumerate()
        .filter_map(|(index, link)| {
            let ExileLinkKind::HideawayLookable {
                grant,
                source_incarnation,
                ..
            } = &link.kind
            else {
                return None;
            };
            pile.contains(&link.exiled_id).then(|| {
                (
                    index,
                    look_grant_admits(state, link.source_id, *grant, *source_incarnation),
                )
            })
        })
        .collect();
    for (index, admitted) in resets {
        if let ExileLinkKind::HideawayLookable { lookers, .. } = &mut state.exile_links[index].kind
        {
            *lookers = admitted.into_iter().collect();
        }
    }
}

/// CR 607.2a + CR 406.6: Record an exiled→source link with an explicit
/// `ExileLinkKind`, deduped on the `(exiled_id, source_id)` pair (mirrors
/// `push_tracked_by_source`, which delegates here for the plain tracked kind).
/// A later, more specific link upgrades an existing `TrackedBySource` entry;
/// this is required when automatic linked-exile detection runs before a
/// mechanic-specific continuation such as Hideaway concealment.
/// Used by Hideaway (`ExileLinkKind::HideawayLookable`, CR 702.75a) to mark the
/// exiled card as look-permitted for the link's lookers while keeping it
/// discoverable by the kind-agnostic `ExiledBySource` companion-ability filter.
/// A newly recorded link is also mirrored to every command-zone emblem the
/// source created that consumes its exiles, when the exile was made by the
/// ability set that emblem is paired with (CR 607.1d, `mirror_link_to_linked_emblems`).
/// The exile is made by the incarnation `linked_exile_incarnation` names
/// (see [`push_with_kind_as`]).
pub(crate) fn push_with_kind(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
    kind: ExileLinkKind,
) {
    let creator = linked_exile_incarnation(state, source_id);
    push_with_kind_as(state, exiled_id, source_id, creator, kind);
}

/// CR 400.7 + CR 607.2a + CR 113.7a: Link a card exiled by `creator`, an
/// incarnation of `source_id`. A tracked exile, or an exile "until [this
/// object] leaves the battlefield", made by an incarnation that is no longer
/// the live object is that incarnation's: it is kept in the linked-exile LKI
/// and never becomes a link of the object that replaced it.
pub(crate) fn push_with_kind_as(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
    creator: Option<u64>,
    kind: ExileLinkKind,
) {
    let live = state.objects.get(&source_id).map(|obj| obj.incarnation);
    let departed_creator = creator.filter(|creator| live != Some(*creator));
    let recorded = match (&kind, departed_creator) {
        // CR 610.3 + CR 400.7: the incarnation that made an "until it leaves"
        // exile had already left the battlefield (a copy of its ability put
        // onto the stack afterwards still exiles, CR 610.3a + CR 707.10). The
        // specified event is past, so no return is ever created; the object
        // that replaced it is a new object whose departure is not that event.
        (
            ExileLinkKind::TrackedBySource | ExileLinkKind::UntilSourceLeaves { .. },
            Some(creator),
        ) => record_departed_linked_exile(state, exiled_id, source_id, creator),
        // The live object's own exile, or a kind that is never diverted: a
        // look link carries its creator (`push_look_link`); a craft material is
        // linked at cost time; a cast-cost link is made while the spell is the
        // live object (CR 601.2h); cipher, haunt and paradigm links are keyed to
        // the object they belong to; and a monarch-bounded exile returns on an
        // event that is not about its source (CR 610.3).
        (ExileLinkKind::TrackedBySource | ExileLinkKind::UntilSourceLeaves { .. }, None)
        | (
            ExileLinkKind::HideawayLookable { .. }
            | ExileLinkKind::UntilOpponentBecomesMonarch { .. }
            | ExileLinkKind::ParadigmSource { .. }
            | ExileLinkKind::Cipher
            | ExileLinkKind::Haunt
            | ExileLinkKind::CraftMaterial
            | ExileLinkKind::CastCost,
            _,
        ) => record_link(state, exiled_id, source_id, kind),
    };
    if recorded {
        mirror_link_to_linked_emblems(state, exiled_id, source_id);
    }
}

/// CR 607.2a + CR 113.7a + CR 400.7: Keep a card the departed incarnation
/// `creator` of `source_id` exiled in that incarnation's linked-exile LKI, once
/// per card. The entry is the departure entry that admits `creator`, or a new
/// entry admitting only `creator`: an ability of the same departed object
/// stamped with another incarnation does not read it. Returns `true` when the
/// card was newly recorded. CR 400.7: the live object did not exile it, so it
/// gets no link and no per-turn ledger entry.
fn record_departed_linked_exile(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
    creator: u64,
) -> bool {
    // The exile was replaced: nothing was exiled with any incarnation.
    let Some(snapshot) = exiled_card_snapshot(state, exiled_id) else {
        return false;
    };
    let entries = state.linked_exile_lki.entry(source_id).or_default();
    match entries.iter_mut().find(|lki| lki.admits(creator)) {
        // CR 400.7: a card exiled again is a new object, so it is recorded even
        // when an earlier incarnation of it is already in the pile (readers drop
        // that stale entry).
        Some(lki)
            if lki.snapshot.iter().any(|snap| {
                snap.exiled_id == snapshot.exiled_id && snap.incarnation == snapshot.incarnation
            }) =>
        {
            false
        }
        Some(lki) => {
            lki.snapshot.push(snapshot);
            true
        }
        None => {
            entries.push(LinkedExileLki {
                departed: creator,
                successor: creator,
                snapshot: vec![snapshot],
            });
            true
        }
    }
}

/// The dedupe/upgrade body of [`push_with_kind`]. Returns `true` only when a
/// new `(exiled_id, source_id)` link was pushed; an existing link (upgraded or
/// not) returns `false`.
fn record_link(
    state: &mut GameState,
    exiled_id: ObjectId,
    source_id: ObjectId,
    kind: ExileLinkKind,
) -> bool {
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
        return false;
    }
    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind,
    });
    push_exiled_with_source_this_turn(state, exiled_id, source_id);
    true
}

/// CR 400.7 + CR 113.7a: The incarnation of `source_id` a linked-exile read or
/// write is made for. A resolving ability of that source acts as the object that
/// put it on the stack (its push-time capture); anything else acts as the object
/// as it now exists. `None` when the object no longer exists.
pub(crate) fn linked_exile_incarnation(state: &GameState, source_id: ObjectId) -> Option<u64> {
    state
        .resolving_stack_entry
        .as_ref()
        .filter(|entry| entry.source_id == source_id)
        .and_then(StackEntry::ability)
        .and_then(|ability| ability.source_ref(state))
        .map(|source| source.incarnation)
        .or_else(|| state.objects.get(&source_id).map(|obj| obj.incarnation))
}

/// CR 607.1 + CR 607.2a + CR 607.5 + CR 113.7a: the pairing identity of the
/// ability of `source_id` whose instruction is performing the exile being
/// linked now — the incarnation that put it on the stack and the copiable
/// set it is characteristic of. `None` when no activated or triggered ability
/// of `source_id` is resolving (a replacement redirect, a state-based action,
/// another object's effect, a spell), or when that ability is granted or its
/// provenance is not attributable: such an exile pairs with no emblem.
fn resolving_exile_supplier(state: &GameState, source_id: ObjectId) -> Option<LinkedAbilitySource> {
    let entry = state
        .resolving_stack_entry
        .as_ref()
        .filter(|entry| entry.source_id == source_id)?;
    let ability = match &entry.kind {
        StackEntryKind::ActivatedAbility { ability, .. }
        | StackEntryKind::TriggeredAbility { ability, .. } => ability,
        StackEntryKind::Spell { .. }
        | StackEntryKind::KeywordAction { .. }
        | StackEntryKind::CombatDamage { .. } => return None,
    };
    let AbilityProvenance::Characteristic(characteristic_set) =
        ability.source_ability_provenance()?
    else {
        return None;
    };
    // CR 400.7 + CR 113.7a: the push-time capture names the object that put
    // the ability on the stack even after it changed zones.
    Some(LinkedAbilitySource {
        creator: ability.source_ref(state)?,
        characteristic_set,
    })
}

/// CR 607.1d + CR 607.2a + CR 114.4: a card exiled by an object is also
/// "exiled with" that object for every command-zone emblem that object
/// created and whose abilities refer to cards exiled with it. The mirror
/// link is keyed to the emblem — neither a card nor a permanent (CR 114.5),
/// its abilities function in the command zone (CR 114.4) — so it survives the
/// creator leaving the battlefield. Only an exile made by one of the
/// creator's characteristic activated or triggered abilities of the emblem's
/// copiable set is mirrored (CR 607.1: "and not by any other ability"; CR
/// 607.5; CR 607.2a); a replacement-effect supplier (CR 607.2b) is not
/// attributed and fails closed — no card pairs one with an emblem.
fn mirror_link_to_linked_emblems(state: &mut GameState, exiled_id: ObjectId, source_id: ObjectId) {
    let Some(supplier) = resolving_exile_supplier(state, source_id) else {
        return;
    };
    let emblems: Vec<ObjectId> = state
        .command_zone
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_emblem
                    && obj.linked_ability_source == Some(supplier)
                    && source_contains_linked_exile_consumer(obj)
            })
        })
        .collect();
    for emblem_id in emblems {
        // `record_link`, not `push_with_kind`: a mirror link never mirrors again.
        record_link(state, exiled_id, emblem_id, ExileLinkKind::TrackedBySource);
    }
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

/// CR 406.6 + CR 607.2b + CR 608.2c: Record the player who performed the exile
/// that put `exiled_id` into exile — the resolving instruction's acting player
/// (its "that player" / "each player" subject when one is bound, otherwise the
/// ability's controller). This is who "cards *they* exiled with ~" refers to,
/// independent of who owns the card. A card that did not settle in exile (a
/// replacement redirected it) gets no record.
pub(crate) fn record_exiling_player(state: &mut GameState, exiled_id: ObjectId, player: PlayerId) {
    if let Some(obj) = state.objects.get_mut(&exiled_id) {
        if obj.zone == Zone::Exile {
            obj.exiled_by = Some(player);
        }
    }
}

/// CR 611.2a + CR 607.2a: Source-linked durations expire when that same source
/// exiles another card, whether stored as a play permission or a transient effect.
fn expire_until_source_exiles_another_card_durations(state: &mut GameState, source_id: ObjectId) {
    expire_source_exile_linked_casting_permissions(state, source_id);

    let before = state.transient_continuous_effects.len();
    state.transient_continuous_effects.retain(|effect| {
        !(effect.duration == Duration::UntilSourceExilesAnotherCard
            && effect.source_id == source_id)
    });
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty.mark_full();
    }
}

/// Removes every casting permission whose duration lasts until `source_id`
/// exiles another card and that is linked to that source. This is the
/// permission half of `expire_until_source_exiles_another_card_durations`, its
/// only caller. Objects holding no casting permission are not written, so they
/// stay shared with clones of the state.
fn expire_source_exile_linked_casting_permissions(state: &mut GameState, source_id: ObjectId) {
    state.objects.update_where(
        |_, object| !object.casting_permissions.is_empty(),
        |_, object| {
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
        },
    );
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
    use std::collections::BTreeSet;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::CharacteristicSetRef;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, Effect, ManaProduction, PlayerFilter,
        QuantityExpr, QuantityRef, TargetFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
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
                enters_with_modifications: Default::default(),
                mana_spend_permission: None,
                cast_cost_modifier: None,
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
                cast_cost_modifier: None,
                alt_ability_cost: None,
                without_paying_mana_cost: false,
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
                cast_cost_modifier: None,
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
                enters_with_modifications: Default::default(),
                mana_spend_permission: None,
                cast_cost_modifier: None,
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
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
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
                additional_cost: None,
                cast_cost_modifier: None,
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
        push_with_kind(
            &mut state,
            exiled,
            source,
            ExileLinkKind::HideawayLookable {
                grant: LookGrant::Player {
                    player: PlayerId(0),
                },
                lookers: [PlayerId(0)].into(),
                source_incarnation: 0,
            },
        );
        push_with_kind(&mut state, exiled, source, ExileLinkKind::TrackedBySource);

        assert_eq!(state.exile_links.len(), 1);
        assert!(matches!(
            state.exile_links[0].kind,
            ExileLinkKind::HideawayLookable { .. }
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

    // ── CR 607.1d emblem↔creator mirror ─────────────────────────────────

    /// A creator object `S` (incarnation 4) on the battlefield and one
    /// command-zone emblem owned by P0 whose static is (or is not) a
    /// linked-exile consumer, latched to `(S, 4 + creator_incarnation_offset)`
    /// and the copiable set `emblem_set`.
    fn mirror_fixture(
        consumer: bool,
        creator_incarnation_offset: u64,
        emblem_set: CharacteristicSetRef,
    ) -> (crate::types::game_state::GameState, ObjectId, ObjectId) {
        use crate::game::effects::create_emblem::grant_emblem;
        use crate::game::zones::create_object;
        use crate::types::ability::{CardPlayMode, StaticDefinition};
        use crate::types::game_state::GameState;
        use crate::types::identifiers::CardId;
        use crate::types::statics::{
            ExileCardPool, ExileCastCost, ExileCastGrantee, ExileCastTiming, StaticMode,
        };

        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creator".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().incarnation = 4;
        let mode = if consumer {
            StaticMode::ExileCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
                cost: ExileCastCost::PayNormalCost,
                pool: ExileCardPool::Persistent,
                timing: ExileCastTiming::AnyTime,
                mana_spend_permission: None,
                grants_flash: false,
                extra_cost: None,
                enters_with_counter: None,
                grantee: ExileCastGrantee::SourceController,
            }
        } else {
            StaticMode::CantLoseTheGame
        };
        let emblem = grant_emblem(
            &mut state,
            PlayerId(0),
            vec![StaticDefinition::new(mode).affected(TargetFilter::Any)],
            Vec::new(),
            Vec::new(),
        );
        state
            .objects
            .get_mut(&emblem)
            .unwrap()
            .linked_ability_source = Some(LinkedAbilitySource {
            creator: ObjectIncarnationRef::of(source, 4 + creator_incarnation_offset),
            characteristic_set: emblem_set,
        });
        (state, source, emblem)
    }

    /// Which kind of stack entry of the source is resolving.
    #[derive(Clone, Copy)]
    enum ResolvingKind {
        Activated,
        Triggered,
        Spell,
    }

    /// Make an entry of `source` (pushed while it had incarnation 4) the
    /// resolving stack entry; `set_up` shapes its ability's provenance.
    fn resolving_as(
        state: &mut crate::types::game_state::GameState,
        source: ObjectId,
        kind: ResolvingKind,
        set_up: impl FnOnce(&mut ResolvedAbility),
    ) {
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.source_incarnation = Some(4);
        set_up(&mut ability);
        let kind = match kind {
            ResolvingKind::Activated => StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(ability),
            },
            ResolvingKind::Triggered => StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
            ResolvingKind::Spell => StackEntryKind::Spell {
                card_id: crate::types::identifiers::CardId(1),
                ability: Some(Box::new(ability)),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        };
        state.resolving_stack_entry = Some(StackEntry {
            id: ObjectId(900),
            source_id: source,
            controller: PlayerId(0),
            kind,
        });
    }

    /// An activated ability of the source, characteristic of `set`, is resolving.
    fn resolving_characteristic(
        state: &mut crate::types::game_state::GameState,
        source: ObjectId,
        set: CharacteristicSetRef,
    ) {
        resolving_as(state, source, ResolvingKind::Activated, |ability| {
            ability.set_source_ability_provenance_recursive(Some(
                AbilityProvenance::Characteristic(set),
            ));
        });
    }

    fn copy_set(continuous_effect_id: u64) -> CharacteristicSetRef {
        CharacteristicSetRef::Copied(crate::types::ability::CopyEffectInstanceRef {
            continuous_effect_id,
            modification_index: 0,
        })
    }

    fn exile_card(state: &mut crate::types::game_state::GameState) -> ObjectId {
        crate::game::zones::create_object(
            state,
            crate::types::identifiers::CardId(9),
            PlayerId(1),
            "Exiled Card".to_string(),
            Zone::Exile,
        )
    }

    fn has_link(
        state: &crate::types::game_state::GameState,
        exiled: ObjectId,
        source: ObjectId,
    ) -> bool {
        state
            .exile_links
            .iter()
            .any(|link| link.exiled_id == exiled && link.source_id == source)
    }

    /// CR 607.1d + CR 607.2a: a card exiled by a characteristic activated
    /// ability of the creator is mirrored to the creator's consumer emblem when
    /// the incarnation and the copiable set match.
    #[test]
    fn mirror_links_exile_to_matching_creator_emblem() {
        let (mut state, source, emblem) = mirror_fixture(true, 0, CharacteristicSetRef::Own);
        resolving_characteristic(&mut state, source, CharacteristicSetRef::Own);
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source));
        assert!(has_link(&state, card, emblem), "mirrored to the emblem");
        assert_eq!(
            state.exile_links.len(),
            2,
            "one source link plus exactly one mirror (no recursion)"
        );
    }

    /// CR 607.1 + CR 613.1f: an exile made by an ability the creator gained in
    /// layer 6 is not made by the paired ability; no mirror.
    #[test]
    fn mirror_skips_granted_supplier() {
        let (mut state, source, emblem) = mirror_fixture(true, 0, CharacteristicSetRef::Own);
        resolving_as(&mut state, source, ResolvingKind::Activated, |ability| {
            ability.set_source_ability_provenance_recursive(Some(AbilityProvenance::Granted));
        });
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source), "reach: the source link");
        assert!(!has_link(&state, card, emblem), "granted supplier");
    }

    /// CR 607.1: an ability with no attributable provenance (a fixture that
    /// skipped announcement, a delayed trigger) fails closed.
    #[test]
    fn mirror_skips_unattributed_supplier() {
        let (mut state, source, emblem) = mirror_fixture(true, 0, CharacteristicSetRef::Own);
        resolving_as(&mut state, source, ResolvingKind::Activated, |_| {});
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source), "reach: the source link");
        assert!(!has_link(&state, card, emblem), "unattributed supplier");
    }

    /// CR 607.2a + CR 607.2b: only an activated or triggered ability of the
    /// creator that is resolving supplies the pair — not an exile with no
    /// resolving entry (a replacement redirect, an SBA), not one while another
    /// object's ability resolves, and not a spell.
    #[test]
    fn mirror_skips_without_resolving_entry_of_source() {
        // No resolving entry at all.
        let (mut state, source, emblem) = mirror_fixture(true, 0, CharacteristicSetRef::Own);
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source), "reach: the source link");
        assert!(!has_link(&state, card, emblem), "no resolving entry");

        // Another object's characteristic ability is resolving.
        let (mut state, source, emblem) = mirror_fixture(true, 0, CharacteristicSetRef::Own);
        let other = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );
        resolving_characteristic(&mut state, other, CharacteristicSetRef::Own);
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source), "reach: the source link");
        assert!(!has_link(&state, card, emblem), "another object's entry");

        // A spell of the source is resolving, even one carrying a provenance.
        let (mut state, source, emblem) = mirror_fixture(true, 0, CharacteristicSetRef::Own);
        resolving_as(&mut state, source, ResolvingKind::Spell, |ability| {
            ability.set_source_ability_provenance_recursive(Some(
                AbilityProvenance::Characteristic(CharacteristicSetRef::Own),
            ));
        });
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source), "reach: the source link");
        assert!(!has_link(&state, card, emblem), "a spell entry");
    }

    /// CR 607.5: abilities acquired from one copy effect pair only with one
    /// another — the supplier's copiable set must equal the emblem's.
    #[test]
    fn mirror_requires_matching_copiable_set() {
        for (emblem_set, supplier_set, mirrored) in [
            (copy_set(7), copy_set(7), true),
            (copy_set(7), copy_set(8), false),
            (copy_set(7), CharacteristicSetRef::Own, false),
            (CharacteristicSetRef::Own, copy_set(7), false),
        ] {
            let (mut state, source, emblem) = mirror_fixture(true, 0, emblem_set);
            resolving_characteristic(&mut state, source, supplier_set);
            let card = exile_card(&mut state);
            push_tracked_by_source(&mut state, card, source);
            assert!(has_link(&state, card, source), "reach: the source link");
            assert_eq!(
                has_link(&state, card, emblem),
                mirrored,
                "emblem {emblem_set:?} / supplier {supplier_set:?}"
            );
        }
    }

    /// CR 607.1 + CR 607.5: a triggered supplier carries no context latch; its
    /// trigger occurrence answers — printed pairs with `Own`, a copied value
    /// with its copy effect's set, a grant or a retained copy never.
    #[test]
    fn mirror_trigger_occurrence_provenance() {
        use crate::types::ability::{
            CopyEffectInstanceRef, TriggerBaseSetInstanceRef, TriggerDefinitionOccurrenceRef,
            TriggerDefinitionRef, TriggerGrantInstanceRef,
        };

        let copy = CopyEffectInstanceRef {
            continuous_effect_id: 7,
            modification_index: 0,
        };
        for (emblem_set, occurrence, mirrored) in [
            (
                CharacteristicSetRef::Own,
                TriggerDefinitionOccurrenceRef::Printed {
                    base_set: TriggerBaseSetInstanceRef::INITIAL,
                    printed_index: 0,
                },
                true,
            ),
            (
                CharacteristicSetRef::Copied(copy),
                TriggerDefinitionOccurrenceRef::CopiedValue {
                    copy_effect: copy,
                    copied_slot: 0,
                    printed_origin: None,
                },
                true,
            ),
            (
                CharacteristicSetRef::Own,
                TriggerDefinitionOccurrenceRef::Granted {
                    grant_instance: TriggerGrantInstanceRef(1),
                },
                false,
            ),
            (
                CharacteristicSetRef::Own,
                TriggerDefinitionOccurrenceRef::CopyRetained {
                    grant_instance: TriggerGrantInstanceRef(1),
                    source_base_set: TriggerBaseSetInstanceRef::INITIAL,
                    source_printed_index: 0,
                },
                false,
            ),
        ] {
            let (mut state, source, emblem) = mirror_fixture(true, 0, emblem_set);
            let definition_source = ObjectIncarnationRef::of(source, 4);
            resolving_as(&mut state, source, ResolvingKind::Triggered, |ability| {
                assert_eq!(ability.context, Default::default(), "no context latch");
                ability.trigger_definition_ref = Some(TriggerDefinitionRef {
                    source: definition_source,
                    occurrence: occurrence.clone(),
                });
            });
            let card = exile_card(&mut state);
            push_tracked_by_source(&mut state, card, source);
            assert!(has_link(&state, card, source), "reach: the source link");
            assert_eq!(has_link(&state, card, emblem), mirrored, "{occurrence:?}");
        }
    }

    /// CR 400.7: an emblem created by an earlier or later object of the same
    /// storage id is a different creator; no mirror.
    #[test]
    fn mirror_skips_emblem_of_other_incarnation() {
        let (mut state, source, emblem) = mirror_fixture(true, 1, CharacteristicSetRef::Own);
        resolving_characteristic(&mut state, source, CharacteristicSetRef::Own);
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source), "reach: the source link");
        assert!(!has_link(&state, card, emblem), "no mirror on mismatch");
    }

    /// CR 607.1d: only an emblem whose abilities refer to cards exiled with its
    /// creator receives the mirror.
    #[test]
    fn mirror_skips_non_consumer_emblem() {
        let (mut state, source, emblem) = mirror_fixture(false, 0, CharacteristicSetRef::Own);
        resolving_characteristic(&mut state, source, CharacteristicSetRef::Own);
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(has_link(&state, card, source), "reach: the source link");
        assert!(!has_link(&state, card, emblem), "non-consumer emblem");

        // Consumer twin: the same shape with the permission static mirrors.
        let (mut twin, twin_source, twin_emblem) =
            mirror_fixture(true, 0, CharacteristicSetRef::Own);
        resolving_characteristic(&mut twin, twin_source, CharacteristicSetRef::Own);
        let twin_card = exile_card(&mut twin);
        push_tracked_by_source(&mut twin, twin_card, twin_source);
        assert!(has_link(&twin, twin_card, twin_emblem));
    }

    /// CR 113.7a + CR 400.7: while an ability of the source resolves, its
    /// push-time incarnation capture names the exiling object even after the
    /// source changed zones; without a capture the live object answers.
    #[test]
    fn resolving_exile_supplier_uses_push_time_incarnation() {
        let (mut state, source, _) = mirror_fixture(true, 0, CharacteristicSetRef::Own);
        resolving_characteristic(&mut state, source, CharacteristicSetRef::Own);
        state.objects.get_mut(&source).unwrap().incarnation = 5;
        assert_eq!(
            resolving_exile_supplier(&state, source),
            Some(LinkedAbilitySource {
                creator: ObjectIncarnationRef::of(source, 4),
                characteristic_set: CharacteristicSetRef::Own,
            })
        );
        resolving_as(&mut state, source, ResolvingKind::Activated, |ability| {
            ability.source_incarnation = None;
            ability.set_source_ability_provenance_recursive(Some(
                AbilityProvenance::Characteristic(CharacteristicSetRef::Own),
            ));
        });
        assert_eq!(
            resolving_exile_supplier(&state, source),
            Some(LinkedAbilitySource {
                creator: ObjectIncarnationRef::of(source, 5),
                characteristic_set: CharacteristicSetRef::Own,
            })
        );
        state.resolving_stack_entry = None;
        assert_eq!(resolving_exile_supplier(&state, source), None);
    }

    // ── CR 113.7a write attribution ─────────────────────────────────────

    /// A source on the battlefield as incarnation 6 while an activated ability
    /// it put on the stack as incarnation 4 resolves.
    fn departed_creator_fixture() -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().incarnation = 6;
        resolving_as(&mut state, source, ResolvingKind::Activated, |_| {});
        (state, source)
    }

    /// The resolving ability of `source` acts as `incarnation`.
    fn stamp_resolving(state: &mut GameState, source: ObjectId, incarnation: u64) {
        resolving_as(state, source, ResolvingKind::Activated, |ability| {
            ability.source_incarnation = Some(incarnation);
        });
    }

    fn snapshot_of(state: &GameState, card: ObjectId) -> LinkedExileSnapshot {
        exiled_card_snapshot(state, card).expect("the card is in exile")
    }

    /// CR 400.7 + CR 113.7a + CR 607.2a: a tracked exile made by an ability of
    /// a departed incarnation is kept in that incarnation's linked-exile LKI,
    /// once per card, with no live link and no per-turn ledger entry. Made by
    /// the live object, it is an ordinary link.
    #[test]
    fn tracked_exile_by_a_departed_incarnation_is_kept_in_its_lki() {
        // Stamped 4, live 6, pushed twice: one creator entry holding the card once.
        let (mut state, source) = departed_creator_fixture();
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        push_tracked_by_source(&mut state, card, source);
        assert!(
            !has_link(&state, card, source),
            "no link to the live object"
        );
        assert!(
            !state
                .cards_exiled_with_source_this_turn
                .contains_key(&source),
            "the live object did not exile it this turn"
        );
        assert_eq!(
            state.linked_exile_lki[&source],
            vec![LinkedExileLki {
                departed: 4,
                successor: 4,
                snapshot: vec![snapshot_of(&state, card)],
            }],
            "one entry admitting only the creator, holding the card once"
        );

        // A departure entry that admits the creator receives the card.
        let (mut state, source) = departed_creator_fixture();
        let earlier = exile_card(&mut state);
        let departure = LinkedExileLki {
            departed: 4,
            successor: 5,
            snapshot: vec![snapshot_of(&state, earlier)],
        };
        state
            .linked_exile_lki
            .insert(source, vec![departure.clone()]);
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert_eq!(
            state.linked_exile_lki[&source],
            vec![LinkedExileLki {
                snapshot: vec![snapshot_of(&state, earlier), snapshot_of(&state, card)],
                ..departure
            }]
        );

        // Stamped 6 (the live object): an ordinary link, the LKI untouched.
        let (mut state, source) = departed_creator_fixture();
        stamp_resolving(&mut state, source, 6);
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        assert!(
            has_link(&state, card, source),
            "the live object's own exile"
        );
        assert_eq!(
            state.cards_exiled_with_source_this_turn.get(&source),
            Some(&vec![card])
        );
        assert!(!state.linked_exile_lki.contains_key(&source));

        // Stamped 4, but the exile was replaced: nothing is recorded.
        let (mut state, source) = departed_creator_fixture();
        let card = create_object(
            &mut state,
            CardId(9),
            PlayerId(1),
            "Redirected Card".to_string(),
            Zone::Graveyard,
        );
        push_tracked_by_source(&mut state, card, source);
        assert!(!has_link(&state, card, source));
        assert!(!state.linked_exile_lki.contains_key(&source));
    }

    /// CR 400.7 + CR 607.2a: a card the departed incarnation exiles, which then
    /// leaves exile and is exiled again by that same incarnation, is a new
    /// object, so it is recorded again and its pile reads it.
    #[test]
    fn a_card_the_departed_incarnation_exiles_again_is_recorded_again() {
        // Stamped 4, live 6.
        let (mut state, source) = departed_creator_fixture();
        let card = exile_card(&mut state);
        push_tracked_by_source(&mut state, card, source);
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, card, Zone::Graveyard, &mut events);
        crate::game::zones::move_to_zone(&mut state, card, Zone::Exile, &mut events);
        push_tracked_by_source(&mut state, card, source);

        let pile: Vec<ObjectId> =
            crate::game::players::linked_exile_cards_for_source(&state, source)
                .iter()
                .map(|snap| snap.exiled_id)
                .collect();
        assert_eq!(pile, vec![card], "the card exiled again is in the pile");
        assert!(
            state.linked_exile_lki[&source]
                .iter()
                .any(|lki| lki.snapshot.contains(&snapshot_of(&state, card))),
            "the LKI records the card as the object now in exile"
        );
    }

    /// CR 400.7: a snapshot saved before snapshots carried an incarnation cannot
    /// prove which object it named, so it decodes to the legacy sentinel and
    /// never matches a live card (fail-closed, like `CostPaidObjectSnapshot`).
    #[test]
    fn a_legacy_linked_exile_snapshot_matches_no_live_card() {
        let mut state = GameState::new_two_player(7);
        let card = exile_card(&mut state);
        let current = snapshot_of(&state, card);
        assert!(
            current.is_still_exiled(&state),
            "positive: a captured snapshot names the card in exile"
        );

        let mut legacy = serde_json::to_value(&current).expect("the snapshot serializes");
        legacy
            .as_object_mut()
            .expect("the snapshot is a JSON object")
            .remove("incarnation")
            .expect("the snapshot serializes its incarnation");
        let legacy: LinkedExileSnapshot =
            serde_json::from_value(legacy).expect("a legacy snapshot decodes");
        assert_eq!(
            legacy.incarnation,
            crate::types::identifiers::LEGACY_INCARNATION
        );
        assert!(!legacy.is_still_exiled(&state));
    }

    /// CR 610.3 + CR 400.7 + CR 113.7a: an "until it leaves" exile made by an
    /// ability of a departed incarnation installs no return link and no per-turn
    /// ledger entry; the card is kept in that incarnation's linked-exile LKI. Made
    /// by the live object, it is an ordinary return link. A monarch-bounded exile
    /// is never diverted: its event is not about its source.
    #[test]
    fn until_source_leaves_exile_by_a_departed_incarnation_installs_no_return() {
        let until = || ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        };

        // Stamped 4, live 6.
        let (mut state, source) = departed_creator_fixture();
        let card = exile_card(&mut state);
        push_with_kind(&mut state, card, source, until());
        assert!(!has_link(&state, card, source), "no return link");
        assert!(
            !state
                .cards_exiled_with_source_this_turn
                .contains_key(&source),
            "the live object did not exile it this turn"
        );
        assert_eq!(
            state.linked_exile_lki[&source],
            vec![LinkedExileLki {
                departed: 4,
                successor: 4,
                snapshot: vec![snapshot_of(&state, card)],
            }],
            "kept for the incarnation that exiled it"
        );

        // Stamped 6 (the live object): the return link, the LKI untouched.
        let (mut state, source) = departed_creator_fixture();
        stamp_resolving(&mut state, source, 6);
        let card = exile_card(&mut state);
        push_with_kind(&mut state, card, source, until());
        assert!(
            state.exile_links.iter().any(|link| link.exiled_id == card
                && link.source_id == source
                && link.kind == until()),
            "the live object's return link"
        );
        assert!(!state.linked_exile_lki.contains_key(&source));

        // Stamped 4, monarch-bounded: linked regardless of the departure.
        let (mut state, source) = departed_creator_fixture();
        let card = exile_card(&mut state);
        let monarch = ExileLinkKind::UntilOpponentBecomesMonarch {
            return_zone: Zone::Battlefield,
            controller: PlayerId(0),
        };
        push_with_kind(&mut state, card, source, monarch.clone());
        assert!(state.exile_links.iter().any(|link| link.exiled_id == card
            && link.source_id == source
            && link.kind == monarch));
        assert!(!state.linked_exile_lki.contains_key(&source));
    }

    /// CR 607.2a + CR 113.7a: the membership view answers for the acting
    /// incarnation: its own LKI pile while its ability resolves, the live
    /// links for the live object, and never another incarnation's pile.
    #[test]
    fn tracked_exile_membership_answers_for_the_acting_incarnation() {
        let (mut state, source) = departed_creator_fixture();
        let live_card = exile_card(&mut state);
        let creator_card = exile_card(&mut state);
        let older_card = exile_card(&mut state);
        state.exile_links.push(ExileLink {
            exiled_id: live_card,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });
        state.linked_exile_lki.insert(
            source,
            vec![
                LinkedExileLki {
                    departed: 4,
                    successor: 4,
                    snapshot: vec![snapshot_of(&state, creator_card)],
                },
                LinkedExileLki {
                    departed: 2,
                    successor: 3,
                    snapshot: vec![snapshot_of(&state, older_card)],
                },
            ],
        );
        let member =
            |state: &GameState, card| tracked_exile_of_acting_incarnation(state, source, card);

        // The ability stamped 4 is resolving.
        assert!(member(&state, creator_card), "its own pile");
        assert!(
            !member(&state, live_card),
            "the live object's link is not the acting incarnation's"
        );
        assert!(!member(&state, older_card), "another incarnation's pile");

        stamp_resolving(&mut state, source, 2);
        assert!(
            member(&state, older_card),
            "that pile, read by its own incarnation"
        );

        // No ability of the source is resolving: the live object reads.
        state.resolving_stack_entry = None;
        assert!(member(&state, live_card), "the live object's link");
        assert!(
            !member(&state, creator_card),
            "a departed incarnation's pile"
        );
    }

    /// CR 400.7 + CR 607.2a + CR 113.7a: each link kind is read by the
    /// incarnations it belongs to. A tracked link is the live object's, a look
    /// link is its creator's, and every other kind is read by any incarnation.
    #[test]
    fn links_for_reader_admits_each_kind_to_its_incarnation() {
        fn kinds_of<'a>(links: impl Iterator<Item = &'a ExileLink>) -> Vec<ExileLinkKind> {
            links.map(|link| link.kind.clone()).collect()
        }

        // The source is live as incarnation 6; incarnation 4 is a departed one.
        let (mut state, source) = departed_creator_fixture();
        let look = |source_incarnation| ExileLinkKind::HideawayLookable {
            grant: LookGrant::SourceController,
            lookers: BTreeSet::from([PlayerId(0)]),
            source_incarnation,
        };
        let every_reader = [
            ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
            ExileLinkKind::UntilOpponentBecomesMonarch {
                return_zone: Zone::Battlefield,
                controller: PlayerId(0),
            },
            ExileLinkKind::ParadigmSource {
                player: PlayerId(0),
            },
            ExileLinkKind::Cipher,
            ExileLinkKind::Haunt,
            ExileLinkKind::CraftMaterial,
        ];
        let kinds = [ExileLinkKind::TrackedBySource, look(6), look(4)]
            .into_iter()
            .chain(every_reader.clone());
        for kind in kinds {
            let exiled_id = exile_card(&mut state);
            state.exile_links.push(ExileLink {
                exiled_id,
                source_id: source,
                kind,
            });
        }
        let read = |reader| kinds_of(links_for_reader(&state, source, Some(reader)));

        let live_reading: Vec<ExileLinkKind> = [ExileLinkKind::TrackedBySource, look(6)]
            .into_iter()
            .chain(every_reader.clone())
            .collect();
        assert_eq!(
            read(6),
            live_reading,
            "the live object reads its tracked link and its own look link, not the old look link"
        );
        assert_eq!(
            read(4),
            [look(4)]
                .into_iter()
                .chain(every_reader)
                .collect::<Vec<_>>(),
            "the departed incarnation reads its own look link, not the tracked or new look link"
        );
        assert_eq!(
            kinds_of(live_links_for_source(&state, source)),
            live_reading,
            "the live links are the live object's reading"
        );
    }
}

/// Per-site tests for `expire_source_exile_linked_casting_permissions`: a
/// sharing witness, a targeted test and a reference-equality test against the
/// BASE-shaped unguarded loop, plus the call-site pin through
/// `expire_until_source_exiles_another_card_durations`.
#[cfg(test)]
mod object_sweep_sharing_tests {
    use super::*;
    use crate::types::ability::CastingPermission;
    use crate::types::object_map::sweep_test_support::{
        assert_sweep_keeps_store_shared, assert_sweep_matches_reference, object_mut,
        run_sweep_targeting, state_with_objects,
    };

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    /// Three exiled cards and the source (on the battlefield) that exiled them.
    fn exiled_fixture() -> (GameState, Vec<ObjectId>) {
        state_with_objects(&[
            (P0, Zone::Exile),
            (P1, Zone::Exile),
            (P0, Zone::Exile),
            (P0, Zone::Battlefield),
        ])
    }

    /// A "play from exile" permission with the given lifetime and source.
    fn play_from_exile(duration: Duration, source_id: ObjectId) -> CastingPermission {
        CastingPermission::PlayFromExile {
            provenance: crate::types::ability::PlayFromExileProvenance::Impulse,
            mode: crate::types::ability::CardPlayMode::Play,
            duration,
            granted_to: P0,
            frequency: crate::types::statics::CastFrequency::Unlimited,
            source_id: Some(source_id),
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
            land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        }
    }

    /// The permission that ends when `source` exiles another card.
    fn linked(source: ObjectId) -> CastingPermission {
        play_from_exile(Duration::UntilSourceExilesAnotherCard, source)
    }

    /// Adds `permission` to the casting permissions of `id`.
    fn grant(state: &mut GameState, id: ObjectId, permission: CastingPermission) {
        object_mut(state, id).casting_permissions.push(permission);
    }

    /// BASE-shaped unguarded sweep.
    fn reference_expire_linked_permissions(state: &mut GameState, source_id: ObjectId) {
        for (_, object) in state.objects.iter_mut() {
            object.casting_permissions.retain(|permission| {
                let lifetime = permission.lifetime();
                !(lifetime.duration == Some(&Duration::UntilSourceExilesAnotherCard)
                    && lifetime.source_id == Some(source_id))
            });
        }
    }

    /// Sharing witness: no object holds a casting permission.
    #[test]
    fn linked_permission_expiry_without_permissions_keeps_store_shared() {
        let (state, ids) = exiled_fixture();
        let source = ids[3];
        assert_sweep_keeps_store_shared(state, |state| {
            expire_source_exile_linked_casting_permissions(state, source)
        });
    }

    /// Targeted test: only the holder of the linked permission is copied.
    #[test]
    fn linked_permission_expiry_copies_only_the_holder() {
        let (mut state, ids) = exiled_fixture();
        let source = ids[3];
        grant(&mut state, ids[1], linked(source));
        let (before, after) = run_sweep_targeting(state, ids[1], |state| {
            expire_source_exile_linked_casting_permissions(state, source)
        });
        assert_eq!(before.objects[&ids[1]].casting_permissions.len(), 1);
        assert!(after.objects[&ids[1]].casting_permissions.is_empty());
    }

    /// Reference equality, including a permission linked to another source
    /// and one with a different lifetime from the same source.
    #[test]
    fn linked_permission_expiry_matches_unguarded_sweep() {
        let (mut state, ids) = exiled_fixture();
        let source = ids[3];
        grant(&mut state, ids[0], linked(source));
        grant(&mut state, ids[1], linked(source));
        grant(&mut state, ids[1], linked(ids[0]));
        // Needs the sweep (holds a permission) but keeps it.
        grant(
            &mut state,
            ids[2],
            play_from_exile(Duration::UntilEndOfTurn, source),
        );
        assert_sweep_matches_reference(
            &state,
            &[ids[0], ids[1]],
            |state| expire_source_exile_linked_casting_permissions(state, source),
            |state| reference_expire_linked_permissions(state, source),
        );
    }

    /// Call-site pin: the enclosing duration expiry still removes the linked
    /// permission.
    #[test]
    fn source_exile_duration_expiry_removes_linked_casting_permissions() {
        let (mut state, ids) = exiled_fixture();
        let source = ids[3];
        grant(&mut state, ids[0], linked(source));
        expire_until_source_exiles_another_card_durations(&mut state, source);
        assert!(state.objects[&ids[0]].casting_permissions.is_empty());
    }
}
