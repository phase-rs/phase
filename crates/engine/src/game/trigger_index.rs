//! CR 603.2 + CR 603.6a + CR 611.2e: `TriggerIndex` — battlefield-scoped
//! candidate pre-filter for `collect_pending_triggers`. Replaces a full
//! battlefield scan with an event-keyed lookup so trigger-firing cost scales
//! with the number of *relevant* triggers, not with `|battlefield|`.
//!
//! # Correctness model
//!
//! The index maps `TriggerEventKey` → `SmallVec<ObjectId>` ("which permanents
//! could match an event with this shape"). The consult site unions the
//! relevant buckets with `unclassified` and asks each candidate's per-trigger
//! matcher whether it actually matches — the matcher itself is unchanged.
//!
//! Two derivers maintain the index:
//!
//! - `keys_from_event(event, state)`: the keys an event hits at consult time.
//! - `keys_from_trigger_def(def)`: the keys a trigger definition registers
//!   into at maintain time.
//!
//! CR 603.2 over-approximation invariant: it is correctness-preserving to
//! emit more keys than strictly necessary at either site; it is a silent
//! trigger-drop bug to emit fewer. Both derivers are exhaustive `match`es
//! with NO `_` wildcard arms — adding a new `TriggerMode`, `GameEvent`, or
//! `EffectKind` variant is a compile error until the deriver classifies it.
//!
//! # Authority
//!
//! The authoritative correctness path is the rebuild at the end of
//! `evaluate_layers` (CR 611.2e): every continuous-effect-driven mutation of
//! `obj.trigger_definitions` (sliver lords, Changeling, Bramble Sovereign,
//! suppress-triggers statics) flows through the layer pipeline, and
//! `collect_pending_triggers` flushes pending layers before reading the index.
//! The `move_to_zone` incremental hooks are best-effort optimization between
//! layer flushes — they are NOT the safety net.
//!
//! CR 113.6: because those hooks are best-effort and `rebuild_from_battlefield`
//! trusts `state.battlefield` alone (it never reads `obj.zone`), the index can
//! hold an entry for a permanent that has already left the battlefield.
//! `candidates_for_event` therefore filters candidates by the LIVE `obj.zone`
//! before returning them, so the consult honours its own battlefield-candidate
//! contract regardless of how the stale entry arose. This is the same predicate
//! `reindex_object_triggers` enforces on the maintenance side. In
//! `debug_assertions` builds a stale entry panics with the object id, its live
//! zone, and the event; in release builds — including the `server-release`
//! profile the multiplayer server ships, which inherits
//! `debug-assertions = false` — it is corrected silently and recorded only by a
//! `tracing::warn!` on the drop path. That log line is the sole recurrence
//! evidence on the SERVER; `engine-wasm` initialises no `tracing` subscriber, so
//! the browser build records nothing at all.
//!
//! SCOPE: this contains the TRIGGER consequence of the desync, not the desync.
//! The underlying maintenance defect is not found. If the disagreement is
//! "still in `state.battlefield`, live zone elsewhere", then every other
//! consumer of `state.battlefield` — the CR 704 SBA pass, combat declaration,
//! `game/filter.rs` battlefield queries, and the rendered board — still treats
//! the object as a permanent. Only the trigger path is guarded here.
//! Reconciling at `battlefield_phased_in_ids` would contain all consumers at one
//! seam; that is deliberately out of scope for this change.

use smallvec::SmallVec;

use crate::types::ability::{EffectKind, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, TriggerIndex};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::triggers::{TriggerEventKey, TriggerMode};
use crate::types::zones::Zone;

use super::game_object::GameObject;

/// Maximum keys a single trigger definition or event can emit. ETB-with-narrow
/// is 1; `Sacrificed` emits 3 (`Sacrificed` + `LeaveBattlefield` + `Dies`); a
/// `ZoneChanged` to graveyard with 3 core types emits 1 (broad LBF) + 3
/// (narrow LBF) + 1 (broad Dies) + 3 (narrow Dies) = 8. Inline `[..; 8]`
/// covers every observed shape without heap allocation in the hot path.
pub(crate) type Keys = SmallVec<[TriggerEventKey; 8]>;

/// CR 205: Narrow a trigger's `valid_card` filter to exactly one `CoreType`
/// when the filter is `Typed { type_filters: [single CoreType-bearing filter] }`.
/// Any other shape (`Permanent`, `AnyOf`, `Non(_)`, multi-element, missing,
/// non-`Typed`) yields `None` — the broader `EnterBattlefield(None)` key is
/// emitted by the trigger AND the event-side broad emission catches it. Stays
/// conservative.
fn narrow_core_type(filter: &Option<TargetFilter>) -> Option<CoreType> {
    let TargetFilter::Typed(TypedFilter { type_filters, .. }) = filter.as_ref()? else {
        return None;
    };
    if type_filters.len() != 1 {
        return None;
    }
    match &type_filters[0] {
        TypeFilter::Creature => Some(CoreType::Creature),
        TypeFilter::Artifact => Some(CoreType::Artifact),
        TypeFilter::Enchantment => Some(CoreType::Enchantment),
        TypeFilter::Land => Some(CoreType::Land),
        TypeFilter::Planeswalker => Some(CoreType::Planeswalker),
        TypeFilter::Battle => Some(CoreType::Battle),
        // Non-narrow filter shapes — broad emission carries the trigger.
        // CR 308.1: Kindred is a non-permanent supplemental type, never a
        // narrowing battlefield-trigger card type.
        TypeFilter::Instant
        | TypeFilter::Sorcery
        | TypeFilter::Kindred
        | TypeFilter::Permanent
        | TypeFilter::Card
        | TypeFilter::Any
        | TypeFilter::Non(_)
        | TypeFilter::Subtype(_)
        | TypeFilter::AnyOf(_) => None,
    }
}

/// CR 603.2: Derive the `TriggerEventKey`s that the given trigger definition
/// could match. The deriver is an exhaustive `match` on `TriggerMode` — adding
/// a new variant becomes a compile error until classified here. Triggers that
/// are inherently catch-all (`Immediate`, `Always`) or whose match shape is
/// dynamic emit a single `None` so the caller routes them to `unclassified`.
/// `StateCondition` and `Unknown(_)` return an EMPTY result *and* the caller
/// must NOT push such objects to `unclassified` — state triggers run through
/// the dedicated `check_state_triggers` path, never event-driven dispatch.
///
/// Returns `(keys, route_to_unclassified)`:
/// - non-empty keys → object goes into each bucket
/// - `route_to_unclassified == true` → object also goes into `unclassified`
///   (for catch-all modes like `Always`/`Immediate` and for genuinely
///   unclassified TriggerModes)
/// - both empty/false → object is NOT registered for this trigger (state
///   conditions, Unknown).
pub(crate) fn keys_from_trigger_def(def: &TriggerDefinition) -> (Keys, bool) {
    let mut keys: Keys = SmallVec::new();
    let narrow = narrow_core_type(&def.valid_card);

    // Macro to push without manual contains checks. Order doesn't matter for
    // correctness (dedup is handled at the index level).
    let mut push = |k: TriggerEventKey| {
        if !keys.contains(&k) {
            keys.push(k);
        }
    };

    match &def.mode {
        // --- Zone-change family ---
        TriggerMode::ChangesZone | TriggerMode::ChangesZoneAll => {
            // CR 603.6a/c: destination=Battlefield → ETB; origin=Battlefield
            // → LBF (with Dies subkey for graveyard destination).
            match (def.origin, def.destination) {
                (_, Some(Zone::Battlefield)) => push(TriggerEventKey::EnterBattlefield(narrow)),
                (Some(Zone::Battlefield), Some(Zone::Graveyard)) => {
                    push(TriggerEventKey::Dies(narrow));
                    push(TriggerEventKey::LeaveBattlefield(narrow));
                }
                (Some(Zone::Battlefield), _) => push(TriggerEventKey::LeaveBattlefield(narrow)),
                // CR 603.6c: destination=Graveyard with unrestricted origin
                // ("from anywhere") must match both battlefield→graveyard and
                // non-battlefield→graveyard events. Add the battlefield fast-path
                // keys, but keep unclassified routing for library/hand/stack
                // origins because there is no generic "to graveyard" event key.
                (None, Some(Zone::Graveyard)) => {
                    push(TriggerEventKey::Dies(narrow));
                    push(TriggerEventKey::LeaveBattlefield(narrow));
                    return (keys, true);
                }
                _ => {
                    // Non-battlefield zone change (e.g. cast-from-graveyard
                    // observers). Route to unclassified — these are rare and
                    // not the target of this optimization.
                    return (keys, true);
                }
            }
        }
        // CR 702.100a: Evolve — entry-only, narrow filter unused (evolve is
        // SelfRef-on-source). Route as broad ETB so a controller's incoming
        // creature is considered.
        TriggerMode::Evolve => push(TriggerEventKey::EnterBattlefield(Some(CoreType::Creature))),
        // CR 702.100b: Evolved → matcher consumes the dedicated
        // `GameEvent::Evolved`. Route to unclassified — Evolved-listening
        // permanents are rare and the dedicated event keeps the consult cost
        // bounded.
        TriggerMode::Evolved => return (keys, true),
        TriggerMode::ChangesController => push(TriggerEventKey::ChangesController),
        TriggerMode::LeavesBattlefield => push(TriggerEventKey::LeaveBattlefield(narrow)),

        // --- Damage family ---
        TriggerMode::DamageDone
        | TriggerMode::DamageDoneOnce
        | TriggerMode::DamageAll
        | TriggerMode::DamageDealtOnce
        | TriggerMode::DamageDoneOnceByController
        | TriggerMode::DamageReceived
        | TriggerMode::ExcessDamage
        | TriggerMode::ExcessDamageAll => push(TriggerEventKey::DealsDamage),
        TriggerMode::DamagePreventedOnce => return (keys, true),

        // --- Spells / abilities ---
        TriggerMode::SpellCast | TriggerMode::SpellCastOrCopy | TriggerMode::SpellCopy => {
            push(TriggerEventKey::SpellCast(narrow));
        }
        TriggerMode::AbilityCast
        | TriggerMode::AbilityResolves
        | TriggerMode::AbilityTriggered
        | TriggerMode::SpellAbilityCast
        | TriggerMode::SpellAbilityCopy
        | TriggerMode::AbilityActivated
        | TriggerMode::LoyaltyAbilityActivated
        | TriggerMode::NinjutsuActivated
        | TriggerMode::KeywordAbilityActivated(_) => push(TriggerEventKey::AbilityOrCopyActivated),
        TriggerMode::Countered => {
            // CR 701.6: counter-targeting filter is dynamic; rare.
            return (keys, true);
        }
        // CR 702.55c: Haunt payoff triggers live on a card in the EXILE zone and
        // fire via the off-zone scan, never through this battlefield-scoped
        // index. Route to `unclassified` so the index stays exhaustive without
        // claiming a battlefield bucket these triggers can never occupy.
        TriggerMode::HauntedCreatureDies => return (keys, true),

        // --- Combat ---
        TriggerMode::Attacks
        | TriggerMode::AttackersDeclared
        | TriggerMode::YouAttack
        | TriggerMode::AttackersDeclaredOneTarget => push(TriggerEventKey::Attacks),
        TriggerMode::AttackerBlocked
        | TriggerMode::AttackerBlockedOnce
        | TriggerMode::AttackerBlockedByCreature
        | TriggerMode::AttackerUnblocked
        | TriggerMode::AttackerUnblockedOnce
        | TriggerMode::YouAttackUnblocked
        | TriggerMode::Blocks
        | TriggerMode::BlockersDeclared
        | TriggerMode::BlocksOrBecomesBlocked
        | TriggerMode::BecomesBlocked => {
            push(TriggerEventKey::Blocks);
        }

        // --- Counters ---
        TriggerMode::CounterAdded
        | TriggerMode::CounterAddedOnce
        | TriggerMode::CounterAddedAll
        | TriggerMode::CounterTypeAddedAll => push(TriggerEventKey::CounterAdded),
        // CR 714.2d + CR 714.2e: a final-chapter meta-trigger's match shape is
        // dynamic — the final chapter number is derived from the OBSERVED Saga's
        // own chapter abilities, not from anything statically on this trigger.
        // Route to `unclassified` (the documented safety net for dynamic
        // shapes); the three printed cards in the class make the consult cost
        // irrelevant.
        TriggerMode::FinalSagaChapterAbility { .. } => return (keys, true),
        // CR 107.14: "Whenever you get one or more {E}" — energy uses the
        // player-counter event key, not the object-counter key.
        TriggerMode::CounterPlayerAddedAll => push(TriggerEventKey::PlayerCounterChanged),
        TriggerMode::CounterRemoved | TriggerMode::CounterRemovedOnce => {
            push(TriggerEventKey::CounterRemoved);
        }

        // --- Permanents ---
        TriggerMode::Sacrificed | TriggerMode::SacrificedOnce => {
            // CR 701.21 (sacrifice) + CR 603.6c (leaves) + CR 700/404
            // (graveyard destination): a sacrifice is a leave-to-graveyard.
            // Per-event dedup at the consult site is safe (LOW 10).
            push(TriggerEventKey::Sacrificed);
            push(TriggerEventKey::LeaveBattlefield(narrow));
            push(TriggerEventKey::Dies(narrow));
        }
        TriggerMode::Destroyed => {
            push(TriggerEventKey::Destroyed);
            push(TriggerEventKey::LeaveBattlefield(narrow));
            push(TriggerEventKey::Dies(narrow));
        }
        TriggerMode::Taps | TriggerMode::TapAll => push(TriggerEventKey::Taps),
        TriggerMode::TapsForMana => push(TriggerEventKey::TapsForMana),
        TriggerMode::Untaps | TriggerMode::UntapAll => push(TriggerEventKey::Untaps),

        // --- Targeting ---
        TriggerMode::BecomesTarget | TriggerMode::BecomesTargetOnce => {
            push(TriggerEventKey::BecomesTarget);
        }

        // --- Cards ---
        TriggerMode::Drawn => push(TriggerEventKey::CardsDrawn),
        TriggerMode::Discarded | TriggerMode::DiscardedAll => push(TriggerEventKey::Discarded),
        TriggerMode::Milled | TriggerMode::MilledOnce | TriggerMode::MilledAll => {
            push(TriggerEventKey::Milled);
        }
        TriggerMode::Exiled => push(TriggerEventKey::Exiled),
        TriggerMode::Revealed => push(TriggerEventKey::Revealed),
        // CR 701.24a: Shuffled matcher consumes
        // `GameEvent::PlayerPerformedAction { ShuffledLibrary }`, not a
        // dedicated `Shuffled` event. Route via the shared player-action key.
        TriggerMode::Shuffled => push(TriggerEventKey::PlayerActionPerformed),

        // --- Life ---
        TriggerMode::LifeGained
        | TriggerMode::LifeLost
        | TriggerMode::LifeLostAll
        | TriggerMode::LifeChanged => push(TriggerEventKey::LifeChanged),
        TriggerMode::PayLife => return (keys, true),
        // CR 702.24a (cumulative upkeep) + CR 702.30 (echo): both synthesized
        // with `def.phase = Some(Upkeep)`, both matchers dispatch on
        // `PhaseChanged { phase }`.
        TriggerMode::PayCumulativeUpkeep | TriggerMode::PayEcho => {
            push(TriggerEventKey::BeginningOfPhase(
                crate::types::phase::Phase::Upkeep,
            ));
        }

        // --- Tokens ---
        TriggerMode::TokenCreated | TriggerMode::TokenCreatedOnce | TriggerMode::ConjureAll => {
            push(TriggerEventKey::TokenCreated);
        }

        // --- Face / transform ---
        TriggerMode::TurnFaceUp | TriggerMode::Transformed => {
            push(TriggerEventKey::FaceOrTransform);
        }

        // --- Phase / turn ---
        TriggerMode::Phase => match def.phase {
            Some(phase) => push(TriggerEventKey::BeginningOfPhase(phase)),
            // Parser can produce `def.phase = None` when phase text is
            // unrecognized (CR 603.2b fallback). Stay safe via unclassified.
            None => return (keys, true),
        },
        // CR 702.26c: Phasing triggers fire when a permanent phases in.
        TriggerMode::PhaseIn => push(TriggerEventKey::PhaseIn),
        // CR 702.26b: Phasing triggers fire when a permanent phases out.
        TriggerMode::PhaseOut | TriggerMode::PhaseOutAll => push(TriggerEventKey::PhaseOut),
        TriggerMode::TurnBegin => push(TriggerEventKey::TurnStarted),
        TriggerMode::NewGame => return (keys, true),

        // --- Monarch / initiative ---
        TriggerMode::BecomeMonarch | TriggerMode::TakesInitiative => {
            push(TriggerEventKey::MonarchOrInitiative);
        }

        // CR 701.52a + CR 702.159a: Visit abilities on Attractions.
        TriggerMode::VisitAttraction => push(TriggerEventKey::VisitAttraction),
        TriggerMode::Specializes => push(TriggerEventKey::Specializes),

        // --- Game state ---
        TriggerMode::LosesGame => push(TriggerEventKey::PlayerLost),

        // --- Mana ---
        TriggerMode::ManaAdded | TriggerMode::ManaAbilityProduced => {
            push(TriggerEventKey::ManaProduced)
        }
        TriggerMode::ManaExpend => push(TriggerEventKey::ManaSpent),

        // --- Land ---
        // CR 305.1: LandPlayed event is global (few battlefield triggers
        // listen). Route to unclassified — cost is one consult per such card.
        TriggerMode::LandPlayed => return (keys, true),

        // CR 601.1a + CR 701.18b: "play a card" fires on a SpellCast OR a LandPlayed event
        // (`match_play_card`). Because it spans two distinct event keys, route
        // to unclassified so the trigger is consulted for both — narrowing to a
        // single TriggerEventKey would silently drop one of the two events.
        TriggerMode::PlayCard => return (keys, true),

        // --- Equipment / aura ---
        TriggerMode::Attached | TriggerMode::Unattach => push(TriggerEventKey::AttachmentChanged),

        // --- Dungeon / Class / Case ---
        TriggerMode::DungeonCompleted
        | TriggerMode::RoomEntered
        | TriggerMode::ClassLevelGained
        | TriggerMode::CaseSolved => push(TriggerEventKey::DungeonOrClassOrCase),

        // --- Planar ---
        TriggerMode::PlanarDice | TriggerMode::Planeswalked { .. } | TriggerMode::ChaosEnsues => {
            return (keys, true)
        }

        // --- Dice / coin ---
        TriggerMode::RolledDie | TriggerMode::RolledDieOnce | TriggerMode::FlippedCoin => {
            push(TriggerEventKey::DieOrCoin);
        }
        TriggerMode::Clashed => push(TriggerEventKey::Clashed),

        // --- Day/night ---
        TriggerMode::DayTimeChanges => push(TriggerEventKey::DayNightChanged),

        // --- Copy ---
        TriggerMode::Copied => return (keys, true),

        // --- Vote ---
        TriggerMode::Vote => push(TriggerEventKey::Voted),

        // --- Renown / monstrous ---
        TriggerMode::BecomeRenowned => push(TriggerEventKey::Renowned),
        TriggerMode::BecomeMonstrous => push(TriggerEventKey::BecomesMonstrous),

        // --- Player actions ---
        TriggerMode::Proliferate
        | TriggerMode::RingTemptsYou
        | TriggerMode::Surveil
        | TriggerMode::Scry
        | TriggerMode::PlayerPerformedAction
        | TriggerMode::SearchedLibrary
        | TriggerMode::CollectEvidence
        | TriggerMode::CommitCrime
        | TriggerMode::Investigated => push(TriggerEventKey::PlayerActionPerformed),

        // --- Combat events ---
        TriggerMode::Fight | TriggerMode::FightOnce => push(TriggerEventKey::Fight),

        // --- Set-specific / sparse mechanics: route to unclassified. ---
        TriggerMode::Abandoned
        | TriggerMode::ClaimPrize
        | TriggerMode::CrankContraption
        | TriggerMode::Devoured
        | TriggerMode::Forage
        | TriggerMode::FullyUnlock
        | TriggerMode::GiveGift
        | TriggerMode::Mentored
        | TriggerMode::Mutates
        | TriggerMode::SeekAll
        | TriggerMode::SetInMotion
        | TriggerMode::Stationed
        | TriggerMode::Trains
        | TriggerMode::UnlockDoor
        | TriggerMode::BecomesCrewed
        | TriggerMode::BecomesPlotted
        | TriggerMode::BecomesSaddled
        | TriggerMode::Championed
        | TriggerMode::Crewed
        | TriggerMode::Crews
        | TriggerMode::Saddled
        | TriggerMode::Saddles
        | TriggerMode::SaddlesOrCrews
        | TriggerMode::Cycled
        | TriggerMode::CycledOrDiscarded
        | TriggerMode::Exploited => return (keys, true),

        // --- Triggered mechanics with dedicated event keys ---
        TriggerMode::Explored => push(TriggerEventKey::Explored),
        TriggerMode::Discover => push(TriggerEventKey::DiscoverResolved),
        TriggerMode::Adapt => push(TriggerEventKey::AdaptResolved),
        TriggerMode::Connives => push(TriggerEventKey::ConniveResolved),
        TriggerMode::Exerted => push(TriggerEventKey::Exerted),
        TriggerMode::Enlisted => push(TriggerEventKey::Enlisted),
        TriggerMode::Foretell => push(TriggerEventKey::Foretold),
        TriggerMode::ManifestDread => push(TriggerEventKey::ManifestDreadResolved),

        // --- Catch-all matchers — fires on every event, must always be
        // considered. Route to unclassified. ---
        TriggerMode::Immediate | TriggerMode::Always => return (keys, true),

        // --- Compound triggers ---
        TriggerMode::EntersOrAttacks => {
            push(TriggerEventKey::EnterBattlefield(narrow));
            push(TriggerEventKey::Attacks);
        }
        // CR 702.55c: Haunt creature ETB half fires on entering the battlefield.
        TriggerMode::EntersOrHauntedCreatureDies => {
            push(TriggerEventKey::EnterBattlefield(narrow));
        }
        TriggerMode::AttacksOrBlocks => {
            push(TriggerEventKey::Attacks);
            push(TriggerEventKey::Blocks);
        }

        // --- Bending (Avatar crossover) ---
        TriggerMode::Airbend
        | TriggerMode::Earthbend
        | TriggerMode::Firebend
        | TriggerMode::Waterbend
        | TriggerMode::ElementalBend => push(TriggerEventKey::Bending),

        // CR 603.8: state triggers are processed by the dedicated
        // `check_state_triggers` path, NOT by event-driven dispatch. The
        // matcher dispatch returns `None` for them. Skip entirely — neither
        // a key nor unclassified routing.
        TriggerMode::StateCondition => return (SmallVec::new(), false),
        // No matcher registered; never fires through events.
        TriggerMode::Unknown(_) => return (SmallVec::new(), false),
    }

    (keys, false)
}

/// CR 603.2: Derive the `TriggerEventKey`s that the given event hits at
/// consult time. Exhaustive `match` on `GameEvent` — adding a new variant is
/// a compile error until classified. The nested `EffectResolved { kind }`
/// dispatch on `EffectKind` is similarly exhaustive (no `_` arm).
pub(crate) fn keys_from_event(event: &GameEvent, state: &GameState) -> Keys {
    let mut out: Keys = SmallVec::new();
    let mut push = |k: TriggerEventKey| {
        if !out.contains(&k) {
            out.push(k);
        }
    };

    match event {
        // CR 732.2: a halted-resolution notification produces no trigger keys.
        GameEvent::GameStarted
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::ResolutionHalted { .. } => {}
        GameEvent::TurnStarted { .. } => push(TriggerEventKey::TurnStarted),
        GameEvent::PhaseChanged { phase } => push(TriggerEventKey::BeginningOfPhase(*phase)),
        GameEvent::PriorityPassed { .. } => {}
        GameEvent::StickerPlaced { .. } => {}
        GameEvent::CreatureExerted { .. } => push(TriggerEventKey::Exerted),
        GameEvent::CreatureEnlisted { .. } => push(TriggerEventKey::Enlisted),
        GameEvent::ArmyAmassed { .. } => {}
        GameEvent::Foretold { .. } => push(TriggerEventKey::Foretold),
        // CR 702.143c: "becomes foretold" via an effect is NOT the foretell
        // special action, so it produces no trigger key (a "whenever you
        // foretell a card" trigger must not fire).
        GameEvent::BecameForetold { .. } => {}
        GameEvent::SpellCast { object_id, .. } => {
            push(TriggerEventKey::SpellCast(None));
            if let Some(obj) = state.objects.get(object_id) {
                for ct in &obj.card_types.core_types {
                    push(TriggerEventKey::SpellCast(Some(*ct)));
                }
            }
        }
        GameEvent::SpellCopied {
            object_id,
            original_id,
            ..
        } => {
            push(TriggerEventKey::SpellCast(None));
            // CR 707.10: copy carries the original's characteristics. Read
            // whichever side is currently live (copies on the stack mirror
            // their original; if missing, try the original id).
            let obj = state
                .objects
                .get(object_id)
                .or_else(|| state.objects.get(original_id));
            if let Some(obj) = obj {
                for ct in &obj.card_types.core_types {
                    push(TriggerEventKey::SpellCast(Some(*ct)));
                }
            }
        }
        GameEvent::XValueChosen { .. } => {}
        GameEvent::AbilityActivated { .. } => push(TriggerEventKey::AbilityOrCopyActivated),
        GameEvent::ZoneChanged {
            from,
            to,
            record,
            object_id,
        } => {
            // CR 603.6a: ETB. Emit broad + per-core-type narrow.
            if *to == Zone::Battlefield {
                push(TriggerEventKey::EnterBattlefield(None));
                // Use the live object's post-layer core_types if available (e.g., after
                // Ashaya's layer effect adds the Land type). Fall back to the record's
                // pre-layer types if the object is not in state.objects.
                let core_types = if let Some(obj) = state.objects.get(object_id) {
                    &obj.card_types.core_types
                } else {
                    &record.core_types
                };
                for ct in core_types {
                    push(TriggerEventKey::EnterBattlefield(Some(*ct)));
                }
            }
            // CR 603.6c: leaves battlefield (any destination).
            if *from == Some(Zone::Battlefield) {
                push(TriggerEventKey::LeaveBattlefield(None));
                for ct in &record.core_types {
                    push(TriggerEventKey::LeaveBattlefield(Some(*ct)));
                }
                // CR 700/404: leaves to graveyard is also a Dies event.
                if *to == Zone::Graveyard {
                    push(TriggerEventKey::Dies(None));
                    for ct in &record.core_types {
                        push(TriggerEventKey::Dies(Some(*ct)));
                    }
                }
            }
            // CR 701.13: `match_exiled` consumes `ZoneChanged { to: Exile }`
            // directly — emit the Exiled key whenever any object lands in
            // exile, regardless of origin.
            if *to == Zone::Exile {
                push(TriggerEventKey::Exiled);
            }
        }
        // CR 701.17a: the mill's own action event is what `match_milled`
        // consumes; the library→graveyard zone shape above no longer routes here.
        GameEvent::Milled { .. } => push(TriggerEventKey::Milled),
        GameEvent::LifeChanged { .. } => push(TriggerEventKey::LifeChanged),
        GameEvent::ControllerChanged { .. } => push(TriggerEventKey::ChangesController),
        GameEvent::ManaAdded { .. } => push(TriggerEventKey::ManaProduced),
        GameEvent::ManaAbilityProduced { .. } => push(TriggerEventKey::ManaProduced),
        GameEvent::TappedForMana { .. } => {
            push(TriggerEventKey::ManaProduced);
            push(TriggerEventKey::TapsForMana);
        }
        GameEvent::ManaPoolEmptied { .. } | GameEvent::ManaRecolored { .. } => {}
        GameEvent::PermanentTapped { .. } => push(TriggerEventKey::Taps),
        GameEvent::PlayerLost { .. } => push(TriggerEventKey::PlayerLost),
        // CR 800.4: Administrative control transfers on elimination do NOT
        // flow through ChangesController. PlayerLost only.
        GameEvent::PlayerEliminated { .. } => push(TriggerEventKey::PlayerLost),
        GameEvent::MulliganStarted => {}
        GameEvent::CardsDrawn { .. } | GameEvent::CardDrawn { .. } => {
            push(TriggerEventKey::CardsDrawn);
        }
        GameEvent::PermanentUntapped { .. } => push(TriggerEventKey::Untaps),
        // CR 702.26c: Phasing triggers fire when a permanent phases in.
        GameEvent::PermanentPhasedIn { .. } => push(TriggerEventKey::PhaseIn),
        // CR 702.26b: Phasing triggers fire when a permanent phases out.
        GameEvent::PermanentPhasedOut { .. } => push(TriggerEventKey::PhaseOut),
        GameEvent::PlayerPhasedOut { .. } | GameEvent::PlayerPhasedIn { .. } => {}
        GameEvent::LandPlayed { .. } => {}
        GameEvent::StackPushed { .. } | GameEvent::StackResolved { .. } => {}
        GameEvent::Discarded { .. } => push(TriggerEventKey::Discarded),
        GameEvent::DamageCleared { .. } => {}
        GameEvent::GameOver { .. } => {}
        GameEvent::DamageDealt { .. } | GameEvent::CombatDamageDealtToPlayer { .. } => {
            push(TriggerEventKey::DealsDamage);
        }
        GameEvent::DamagePrevented { .. } => push(TriggerEventKey::DamagePrevented),
        GameEvent::SpellCountered { .. } => {}
        GameEvent::CounterAdded { .. } => push(TriggerEventKey::CounterAdded),
        // CR 714.2e: consumed only by
        // `FinalSagaChapterAbility { lifecycle: Resolved }` triggers, which live
        // in the `unclassified` bucket. No key of its own.
        GameEvent::SagaChapterAbilityResolved { .. } => {}
        GameEvent::Evolved { .. } => {}
        GameEvent::ObjectIntensified { .. } => {}
        GameEvent::CounterRemoved { .. } => push(TriggerEventKey::CounterRemoved),
        GameEvent::TokenCreated { .. } | GameEvent::ObjectConjured { .. } => {
            push(TriggerEventKey::TokenCreated);
        }
        GameEvent::CreatureDestroyed { .. } => push(TriggerEventKey::Destroyed),
        GameEvent::PermanentSacrificed { .. } => push(TriggerEventKey::Sacrificed),
        GameEvent::EffectResolved { kind, .. } => keys_from_effect_kind(*kind, &mut push),
        GameEvent::Unattached { .. } => push(TriggerEventKey::AttachmentChanged),
        // CR 116.2c + CR 116.1: no printed trigger condition matches "a
        // continuous effect ended". The special action doesn't use the stack, and
        // any consequential board change (a Licid reverting to a creature and
        // being unattached under CR 704.5p) emits its OWN indexed event.
        // Explicitly inert rather than absent, so a future trigger family must
        // classify it.
        GameEvent::ContinuousEffectEnded { .. } => {}
        GameEvent::AttackersDeclared { .. } => push(TriggerEventKey::Attacks),
        GameEvent::BlockersDeclared { .. } => push(TriggerEventKey::Blocks),
        // CR 509.3c: an effect-driven "becomes blocked" is a Blocks-key event so
        // "whenever ~ becomes blocked" triggers are indexed for it.
        GameEvent::AttackerBecameBlockedByEffect { .. } => push(TriggerEventKey::Blocks),
        GameEvent::AttackerBecameBlockedByFilteredBlocker { .. } => push(TriggerEventKey::Blocks),
        GameEvent::CombatTaxPaid { .. } | GameEvent::CombatTaxDeclined { .. } => {}
        GameEvent::BecomesTarget { .. } => push(TriggerEventKey::BecomesTarget),
        GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. } => {}
        GameEvent::ReplacementApplied { .. } => {}
        GameEvent::Transformed { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::TurnedFaceDown { .. } => {
            push(TriggerEventKey::FaceOrTransform);
        }
        // CR 701.27b (by analogy): transforming and turning a permanent face
        // up/down are distinct game actions that don't share triggers even
        // though they use the same physical action; flipping is likewise its
        // own game action. No printed flip card has a trigger that fires on
        // flipping (a design fact about the card pool, not a CR statement).
        // Deliberately dispatches NO trigger key — folding it into
        // `FaceOrTransform` would consult transform/face-change triggers for an
        // event none of them can match.
        GameEvent::Flipped { .. } => {}
        GameEvent::DayNightChanged { .. } => push(TriggerEventKey::DayNightChanged),
        GameEvent::CardsRevealed { .. } => push(TriggerEventKey::Revealed),
        // CR 101.4: publishing a chosen number is not CR 701.20 "reveal a card",
        // and no printed trigger watches for it, so it keys nothing. Listed
        // explicitly (not folded into `Revealed`) so a future "whenever a player
        // reveals a card" trigger cannot start firing on a number.
        GameEvent::ChosenNumbersRevealed { .. } => {}
        GameEvent::CrimeCommitted { .. } => push(TriggerEventKey::PlayerActionPerformed),
        GameEvent::Cycled { .. } => {}
        GameEvent::PlayerPerformedAction { .. } => push(TriggerEventKey::PlayerActionPerformed),
        GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::CreatureNoLongerSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. } => {}
        GameEvent::CaseSolved { .. } | GameEvent::ClassLevelGained { .. } => {
            push(TriggerEventKey::DungeonOrClassOrCase);
        }
        GameEvent::MonarchChanged { .. } => push(TriggerEventKey::MonarchOrInitiative),
        // CR 702.195b-c: Enduring story is a designation, not an inherent trigger
        // event; continuous effects reapply before trigger conditions are checked.
        GameEvent::CityBlessingGained { .. } | GameEvent::EnduringStoryGained { .. } => {}
        // CR 103.1: setup determination, not a CR 706 die-roll trigger source.
        GameEvent::StartingPlayerContest { .. } => {}
        GameEvent::DieRolled { .. } | GameEvent::CoinFlipped { .. } => {
            push(TriggerEventKey::DieOrCoin);
        }
        GameEvent::RingTemptsYou { .. } => push(TriggerEventKey::PlayerActionPerformed),
        GameEvent::RoomEntered { .. } | GameEvent::DungeonCompleted { .. } => {
            push(TriggerEventKey::DungeonOrClassOrCase);
        }
        // Planechase trigger modes (Planeswalked { role }, ChaosEnsues) route to the
        // always-checked unclassified bucket in `keys_from_trigger_def`, so these
        // events need no dedicated index key — their matchers are always consulted.
        GameEvent::Planeswalked { .. }
        | GameEvent::ChaosEnsued { .. }
        | GameEvent::PlanarDieRolled { .. } => {}
        // Archenemy trigger modes (SetInMotion/Abandoned) route to the
        // always-checked unclassified bucket in `keys_from_trigger_def`, so these
        // events need no dedicated index key — their matchers are always consulted.
        GameEvent::SchemeSetInMotion { .. } | GameEvent::SchemeAbandoned { .. } => {}
        GameEvent::RoomDoorUnlocked { .. } | GameEvent::BecomesPlotted { .. } => {}
        GameEvent::InitiativeTaken { .. } => push(TriggerEventKey::MonarchOrInitiative),
        GameEvent::AttractionOpened { .. }
        | GameEvent::AttractionsRolledToVisit { .. }
        | GameEvent::ContraptionAssembled { .. }
        | GameEvent::ContraptionCranked { .. } => {}
        GameEvent::AttractionVisited { .. } => push(TriggerEventKey::VisitAttraction),
        GameEvent::Specialized { .. } => push(TriggerEventKey::Specializes),
        // CR 702.140c-d: `TriggerMode::Mutates` is routed to the always-checked
        // unclassified bucket (see `keys_from_trigger_def`), so the `Mutated`
        // event needs no dedicated index key — `match_mutates` is always consulted.
        GameEvent::Mutated { .. } => {}
        // Unstable Host/Augment combine is a distinct mechanic and has no
        // dedicated trigger mode today.
        GameEvent::Augmented { .. } => {}
        GameEvent::Firebend { .. }
        | GameEvent::Airbend { .. }
        | GameEvent::Earthbend { .. }
        | GameEvent::Waterbend { .. } => push(TriggerEventKey::Bending),
        GameEvent::CompanionRevealed { .. } | GameEvent::CompanionMovedToHand { .. } => {}
        GameEvent::NinjutsuActivated { .. } | GameEvent::KeywordAbilityActivated { .. } => {
            push(TriggerEventKey::AbilityOrCopyActivated);
        }
        GameEvent::CreatureExploited { .. } => {}
        GameEvent::EnergyChanged { .. }
        | GameEvent::SpeedChanged { .. }
        | GameEvent::PlayerCounterChanged { .. } => push(TriggerEventKey::PlayerCounterChanged),
        GameEvent::ManaExpended { .. } => push(TriggerEventKey::ManaSpent),
        GameEvent::Clash { .. } => push(TriggerEventKey::Clashed),
        GameEvent::VoteCast { .. } | GameEvent::VoteResolved { .. } => {
            push(TriggerEventKey::Voted);
        }
        GameEvent::PowerToughnessChanged { .. } => {}
        GameEvent::CascadeMissed { .. }
        | GameEvent::CardPredicateGuessMade { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => {}
    }

    out
}

/// CR 603.2: Map an `EffectKind` carried by `GameEvent::EffectResolved` to
/// the `TriggerEventKey`(s) the matched matchers consume. Exhaustive `match`
/// — every variant either dispatches (mapped to a key) or maps explicitly to
/// no-op. Adding a new `EffectKind` is a compile error until classified.
///
/// Only kinds with at least one PRODUCTION `EffectResolved`-dispatching
/// matcher in `trigger_matchers.rs` emit keys; all others are no-ops.
fn keys_from_effect_kind(kind: EffectKind, push: &mut impl FnMut(TriggerEventKey)) {
    match kind {
        // Production EffectResolved matchers — see `trigger_matchers.rs` lines
        // 1896, 2072, 2126, 2172, 2198, 2234, 2261, 2313, 2338.
        EffectKind::Attach | EffectKind::AttachAll | EffectKind::Equip => {
            push(TriggerEventKey::AttachmentChanged);
        }
        EffectKind::Reveal => push(TriggerEventKey::Revealed),
        EffectKind::GainControl | EffectKind::GainControlAll => {
            push(TriggerEventKey::ChangesController)
        }
        EffectKind::Fight => push(TriggerEventKey::Fight),
        EffectKind::Explore => push(TriggerEventKey::Explored),
        EffectKind::Discover => push(TriggerEventKey::DiscoverResolved),
        EffectKind::Adapt => push(TriggerEventKey::AdaptResolved),
        EffectKind::Connive => push(TriggerEventKey::ConniveResolved),
        EffectKind::Renown => push(TriggerEventKey::Renowned),
        EffectKind::Monstrosity => push(TriggerEventKey::BecomesMonstrous),
        EffectKind::ManifestDread => push(TriggerEventKey::ManifestDreadResolved),
        EffectKind::DayTimeChange => push(TriggerEventKey::DayNightChanged),
        EffectKind::PutSticker | EffectKind::ApplySticker => {}
        // All other variants: not dispatched on by any production
        // EffectResolved matcher (verified against `trigger_matchers.rs` 1-3216).
        // Explicit `&[]`-equivalent arms — a future contributor who adds a
        // new EffectResolved-dispatching matcher will force this match to be
        // re-classified.
        EffectKind::StartYourEngines
        | EffectKind::ChangeSpeed
        | EffectKind::DealDamage
        | EffectKind::ApplyPostReplacementDamage
        | EffectKind::EachDealsDamageEqualToPower
        | EffectKind::EachSourceDealsDamage
        | EffectKind::Draw
        | EffectKind::Pump
        | EffectKind::PairWith
        | EffectKind::Destroy
        | EffectKind::Counter
        | EffectKind::CounterAll
        | EffectKind::Token
        | EffectKind::GainLife
        | EffectKind::LoseLife
        | EffectKind::Tap
        | EffectKind::Untap
        | EffectKind::RemoveCounter
        | EffectKind::Sacrifice
        | EffectKind::DiscardCard
        | EffectKind::Mill
        | EffectKind::Scry
        | EffectKind::PumpAll
        | EffectKind::DamageAll
        | EffectKind::DamageEachPlayer
        | EffectKind::DestroyAll
        | EffectKind::TapAll
        | EffectKind::UntapAll
        | EffectKind::ChangeZone
        | EffectKind::ChangeZoneAll
        | EffectKind::Dig
        | EffectKind::ControlNextTurn
        | EffectKind::UnattachAll
        | EffectKind::Surveil
        | EffectKind::Bounce
        | EffectKind::BounceAll
        | EffectKind::ExploreAll
        | EffectKind::Investigate
        | EffectKind::Tribute
        | EffectKind::TimeTravel
        | EffectKind::BecomeMonarch
        | EffectKind::NoOp
        | EffectKind::Proliferate
        | EffectKind::ProliferateTarget
        | EffectKind::EndTheTurn
        | EffectKind::EndCombatPhase
        | EffectKind::Populate
        | EffectKind::Clash
        | EffectKind::Behold
        | EffectKind::Vote
        | EffectKind::SeparateIntoPiles
        | EffectKind::SwitchPT
        | EffectKind::CopySpell
        | EffectKind::EpicCopy
        | EffectKind::CopyTokenOf
        | EffectKind::CreateTokenCopyFromPool
        | EffectKind::Myriad
        | EffectKind::Encore
        | EffectKind::Meld
        | EffectKind::ExileHaunting
        | EffectKind::HideawayConceal
        | EffectKind::BecomeCopy
        | EffectKind::GainActivatedAbilitiesOfTarget
        | EffectKind::ChooseCard
        | EffectKind::PutCounter
        | EffectKind::PutCounterAll
        | EffectKind::MultiplyCounter
        | EffectKind::ReproduceEventCounters
        | EffectKind::DoublePT
        | EffectKind::DoublePTAll
        | EffectKind::MoveCounters
        | EffectKind::Animate
        | EffectKind::ReturnAsAura
        | EffectKind::RegisterBending
        | EffectKind::GenericEffect
        | EffectKind::Cleanup
        | EffectKind::Mana
        | EffectKind::Discard
        | EffectKind::Shuffle
        | EffectKind::SearchLibrary
        | EffectKind::SearchOutsideGame
        | EffectKind::OpenBoosterPack
        | EffectKind::ExileTop
        | EffectKind::ExileFaceDownPile
        | EffectKind::TargetOnly
        | EffectKind::Choose
        | EffectKind::ChoosePermanent
        | EffectKind::OpponentGuess
        | EffectKind::ChooseDamageSource
        | EffectKind::Suspect
        | EffectKind::Unsuspect
        | EffectKind::PhaseOut
        | EffectKind::PhaseIn
        | EffectKind::ForceBlock
        | EffectKind::ForceAttack
        | EffectKind::SolveCase
        | EffectKind::BecomePrepared
        | EffectKind::BecomeUnprepared
        | EffectKind::SetClassLevel
        | EffectKind::CreateDelayedTrigger
        | EffectKind::AddTargetReplacement
        | EffectKind::AddRestriction
        | EffectKind::ReduceNextSpellCost
        | EffectKind::GrantNextSpellAbility
        | EffectKind::AddPendingETBCounters
        | EffectKind::AddPendingEntersModifications
        | EffectKind::CreateEmblem
        | EffectKind::PayCost
        | EffectKind::CastFromZone
        | EffectKind::FreeCastFromZones
        | EffectKind::ExileResolvingSpellInsteadOfGraveyard
        | EffectKind::PreventDamage
        | EffectKind::CreateDamageReplacement
        | EffectKind::CreateDrawReplacement
        | EffectKind::CreatePlaneswalkReplacement
        | EffectKind::Regenerate
        | EffectKind::RemoveAllDamage
        | EffectKind::LoseTheGame
        | EffectKind::WinTheGame
        | EffectKind::RollDie
        | EffectKind::FlipCoin
        | EffectKind::FlipCoins
        | EffectKind::FlipCoinUntilLose
        | EffectKind::RingTemptsYou
        | EffectKind::VentureIntoDungeon
        | EffectKind::VentureInto
        | EffectKind::TakeTheInitiative
        | EffectKind::ArrangePlanarDeckTop
        | EffectKind::Planeswalk
        | EffectKind::ChaosEnsues
        // Redistribute emits LifeChanged handled by that event's own arm; no
        // EffectResolved-dispatching matcher. No-op here.
        | EffectKind::RedistributeLifeTotals
        | EffectKind::ReverseTurnOrder
        | EffectKind::OpenAttractions
        | EffectKind::RollToVisitAttractions
        | EffectKind::ProcessRadCounters
        | EffectKind::GrantCastingPermission
        | EffectKind::ChooseFromZone
        | EffectKind::RememberCard
        | EffectKind::NoteManaSpent
        | EffectKind::ChooseObjectsIntoTrackedSet
        // CR 608.2d + CR 122.1: counter-kind choice / consume — the actual
        // counter placement fires `GameEvent::CounterAdded`, so no matcher
        // dispatches on these `EffectResolved` kinds directly.
        | EffectKind::ChooseCounterKind
        | EffectKind::PutChosenCounter
        | EffectKind::ChooseAndSacrificeRest
        | EffectKind::EachPlayerCopyChosen
        | EffectKind::Exploit
        | EffectKind::GainEnergy
        | EffectKind::GivePlayerCounter
        | EffectKind::LoseAllPlayerCounters
        | EffectKind::ExileFromTopUntil
        | EffectKind::RevealUntil
        | EffectKind::Cascade
        | EffectKind::Ripple
        | EffectKind::MiracleCast
        | EffectKind::MadnessCast
        | EffectKind::PutAtLibraryPosition
        | EffectKind::ChooseDrawnThisTurnPayOrTopdeck
        | EffectKind::PutOnTopOrBottom
        | EffectKind::GiftDelivery
        | EffectKind::Goad
        | EffectKind::GoadAll
        | EffectKind::Detain
        // CR 709.5h-i unlock/fully-unlock triggers fire on the
        // `RoomDoorUnlocked` event, not on this `EffectResolved` kind.
        | EffectKind::SetRoomDoorLock
        | EffectKind::ExchangeControl
        | EffectKind::ChangeTargets
        | EffectKind::Incubate
        | EffectKind::Amass
        | EffectKind::Bolster
        | EffectKind::Manifest
        | EffectKind::Cloak
        | EffectKind::ExtraTurn
        | EffectKind::GrantExtraLoyaltyActivations
        | EffectKind::SkipNextTurn
        | EffectKind::SkipNextStep
        | EffectKind::AdditionalPhase
        | EffectKind::Double
        | EffectKind::RuntimeHandled
        | EffectKind::Learn
        | EffectKind::Forage
        | EffectKind::CompletePlayerAction
        | EffectKind::Harness
        | EffectKind::CollectEvidence
        | EffectKind::Endure
        | EffectKind::BlightEffect
        | EffectKind::Seek
        | EffectKind::SetLifeTotal
        | EffectKind::SetDayNight
        | EffectKind::GiveControl
        | EffectKind::RemoveFromCombat
        // CR 509.3c: the "becomes blocked" trigger from an effect-block is keyed
        // off the `AttackerBecameBlockedByEffect` GameEvent (see the event→key
        // map above), not off `EffectResolved`, so this kind emits no key here.
        | EffectKind::BecomeBlocked
        | EffectKind::Conjure
        | EffectKind::Intensify
        | EffectKind::ApplyPerpetual
        | EffectKind::DraftFromSpellbook
        | EffectKind::ChooseOneOf
        | EffectKind::ChooseCounterAdjustment
        | EffectKind::Specialize
        | EffectKind::Unimplemented
        | EffectKind::Crew
        | EffectKind::Station
        | EffectKind::Saddle
        // CR 702.171b: the BecomeSaddled effect fires the saddled trigger via the
        // separately-emitted `GameEvent::Saddled`, mirroring the keyword `Saddle`
        // action; its own `EffectResolved` dispatches no trigger key.
        | EffectKind::BecomeSaddled
        | EffectKind::Transform
        // No printed flip card has a trigger that fires on flipping (a design
        // fact about the card pool, not a CR statement), so — mirroring
        // `Transform` above — this effect's `EffectResolved` dispatches no key;
        // `GameEvent::Flipped` is a log/display notification and dispatches no
        // key either.
        | EffectKind::FlipPermanent
        | EffectKind::TurnFaceUp
        // CR 701.27b: a turned-face-down permanent fires any face-down trigger
        // via the dedicated `GameEvent::TurnedFaceDown`, not via this effect's
        // `EffectResolved`. No-op here, mirroring `TurnFaceUp`.
        | EffectKind::TurnFaceDown
        // Added on origin/main after this branch point. No production
        // EffectResolved-dispatching matcher consumes either: cast-copy fires
        // on cast events (CastCopyOfCard, Mizzix's Mastery), and life/P-T
        // exchange emits LifeChanged/PowerToughnessChanged handled by their own
        // event arms (ExchangeLifeWithStat). No-op here.
        | EffectKind::CastCopyOfCard
        | EffectKind::ExchangeLifeWithStat
        | EffectKind::ExchangeLifeTotals
        // Heist/HeistExile have no production EffectResolved-dispatching matcher.
        | EffectKind::Heist
        | EffectKind::HeistExile
        | EffectKind::CombineHost
        | EffectKind::ChooseAugmentAndCombineWithHost
        | EffectKind::AssembleContraptions
        | EffectKind::AssembleContraptionsFromRollDifference
        | EffectKind::CrankContraptions
        | EffectKind::ReassembleContraption
        | EffectKind::AssembleContraptionOnSprocket
        | EffectKind::ReassembleContraptionOnSprocket => {}
    }
}

/// CR 702.108 (Prowess), CR 702.156 (Ravenous), CR 702.147 (Decayed),
/// CR 702.110 (Exploit), CR 702.21 (Ward), Avatar crossover (Firebending):
/// these keywords synthesize triggered abilities at the consult site of
/// `collect_pending_triggers` (`game::triggers`) instead of materializing a
/// `TriggerDefinition` on the object. The index must therefore consider every
/// battlefield permanent carrying one of these keywords on every event, even
/// if its printed `trigger_definitions` is empty.
///
/// Returns `true` if `obj` carries a keyword whose triggered behavior is
/// synthesized outside `obj.trigger_definitions`. Such objects are routed to
/// `unclassified` so the per-candidate loop always visits them.
pub fn has_synthetic_keyword_trigger_for(obj: &GameObject) -> bool {
    obj.keywords.iter().any(|k| {
        matches!(
            k,
            Keyword::Prowess
                | Keyword::Ravenous
                | Keyword::Decayed
                | Keyword::Exploit
                | Keyword::Ward(_)
                | Keyword::Firebending(_)
        )
    })
}

/// CR 603.6a: Re-register one permanent's trigger definitions in the derived
/// index after they are applied outside the ETB pipeline (scenario seeding,
/// card-db rehydration, Oracle-text overlays, etc.).
pub fn reindex_object_triggers(state: &mut GameState, object_id: ObjectId) {
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };
    if obj.zone != Zone::Battlefield || obj.is_phased_out() {
        state.trigger_index.remove(object_id);
        return;
    }
    let defs: SmallVec<[TriggerDefinition; 4]> = obj
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| entry.definition.clone())
        .collect();
    let synthetic = has_synthetic_keyword_trigger_for(obj);
    state.trigger_index.remove(object_id);
    state.trigger_index.add(object_id, &defs, synthetic);
}

impl TriggerIndex {
    /// CR 603.6a: Register a permanent's trigger definitions in the index when
    /// it enters the battlefield. The caller is responsible for invoking this
    /// AFTER `reset_for_battlefield_entry` so `obj.trigger_definitions`
    /// reflects the post-entry initial trigger set.
    ///
    /// `synthetic_keyword_trigger` is set when the object carries a keyword
    /// whose triggered behavior is materialized inside `collect_pending_triggers`
    /// (Prowess, Ravenous, Decayed, Exploit, Ward, Firebending) — such objects
    /// are also routed to `unclassified` so the per-candidate loop visits them
    /// even when their printed trigger set does not register a key.
    pub fn add(
        &mut self,
        object_id: ObjectId,
        defs: &[TriggerDefinition],
        synthetic_keyword_trigger: bool,
    ) {
        for def in defs {
            let (keys, route_unclassified) = keys_from_trigger_def(def);
            for k in keys {
                let bucket = self.by_key.entry(k).or_default();
                if !bucket.contains(&object_id) {
                    bucket.push(object_id);
                }
            }
            if route_unclassified && !self.unclassified.contains(&object_id) {
                self.unclassified.push(object_id);
            }
        }
        if synthetic_keyword_trigger && !self.unclassified.contains(&object_id) {
            self.unclassified.push(object_id);
        }
    }

    /// CR 603.6c: Remove a permanent from every bucket when it leaves the
    /// battlefield.
    pub fn remove(&mut self, object_id: ObjectId) {
        self.unclassified.retain(|id| *id != object_id);
        // im::HashMap::iter_mut materializes via copy-on-write per touched
        // entry; for the typical bucket-count (≤ a few dozen) the bookkeeping
        // is negligible compared to the previous full battlefield rescan.
        let mut empty_keys: SmallVec<[TriggerEventKey; 4]> = SmallVec::new();
        for (k, bucket) in self.by_key.iter_mut() {
            bucket.retain(|id| *id != object_id);
            if bucket.is_empty() {
                empty_keys.push(k.clone());
            }
        }
        for k in empty_keys {
            self.by_key.remove(&k);
        }
    }

    /// CR 603.6a + CR 611.2e: Rebuild from scratch by scanning every phased-in
    /// battlefield permanent and re-deriving its keys via
    /// `keys_from_trigger_def`. Called at the end of `evaluate_layers` and
    /// lazily on first consult after deserialize.
    pub fn rebuild_from_battlefield(state: &mut GameState) {
        let mut fresh = TriggerIndex::default();
        // CR 702.26: phased-out permanents don't trigger.
        for obj_id in state.battlefield_phased_in_ids() {
            if let Some(obj) = state.objects.get(&obj_id) {
                // `as_slice()` exposes the materialized post-layer trigger
                // set (base + granted) without any CR gate. The per-event
                // matcher gating in `active_trigger_definitions` runs at
                // consult time — classification can register on the full set.
                let synthetic = has_synthetic_keyword_trigger_for(obj);
                let defs: SmallVec<[TriggerDefinition; 4]> = obj
                    .trigger_definitions
                    .as_slice()
                    .iter()
                    .map(|entry| entry.definition.clone())
                    .collect();
                fresh.add(obj_id, &defs, synthetic);
            }
        }
        state.trigger_index = fresh;
    }
}

/// CR 603.2 + CR 611.2e: Ensure the serde-skipped candidate index is available
/// before a consult. The layer pipeline remains the authoritative rebuild path
/// for live granted and removed definitions; this only restores the empty
/// derived index after deserialize when battlefield state is already present.
pub fn ensure_ready(state: &mut GameState) {
    if state.trigger_index.by_key.is_empty()
        && state.trigger_index.unclassified.is_empty()
        && !state.battlefield.is_empty()
    {
        TriggerIndex::rebuild_from_battlefield(state);
    }
}

/// CR 603.2: Public consult helper. Returns the union of buckets the event
/// keys hit, plus the `unclassified` bucket. Caller dedups against the
/// per-event `registered_this_event` set as usual.
pub fn candidates_for_event(state: &GameState, event: &GameEvent) -> SmallVec<[ObjectId; 16]> {
    let mut out: SmallVec<[ObjectId; 16]> = SmallVec::new();
    out.extend(state.trigger_index.unclassified.iter().copied());
    let keys = keys_from_event(event, state);
    for k in &keys {
        if let Some(bucket) = state.trigger_index.by_key.get(k) {
            out.extend(bucket.iter().copied());
        }
    }
    // CR 702.26b: a phased-out permanent is treated as though it doesn't exist,
    // so it normally cannot trigger. The event source is the one exception for
    // its own "phases out" trigger: the event is emitted after the status flip,
    // and collection applies the matching definition-level carve-out.
    let phase_out_source = match event {
        GameEvent::PermanentPhasedOut { object_id, .. } => Some(*object_id),
        _ => None,
    };
    if let Some(object_id) = phase_out_source {
        out.push(object_id);
    }
    // CR 113.6: a candidate whose live zone is not the battlefield is a stale
    // index entry. The `retain` below corrects it and logs; in debug builds we
    // additionally panic with the diagnosis so the underlying maintenance defect
    // is not masked indefinitely. `not(test)` mirrors the `ReplacementIndex`
    // differential gate in `indexed_object_replacement_candidates` — without it,
    // every hostile-state unit test in this module's own `#[cfg(test)] mod tests`
    // would panic before reaching the code under test. Note that `cfg(test)` is
    // NOT set for the engine lib when it is linked by the integration-test
    // binaries under `crates/engine/tests/`, so this assertion IS live there —
    // deliberately: it makes the whole integration suite a recurrence detector.
    // O(candidates); the Vec is confined to this cfg so release and unit-test
    // builds allocate nothing. `out` is not yet deduped, so the same
    // (object_id, live_zone) pair can appear more than once in `stale`; that is
    // duplication, not multiple defects.
    #[cfg(all(debug_assertions, not(test)))]
    {
        let stale: Vec<(ObjectId, Zone)> = out
            .iter()
            .filter_map(|id| {
                state
                    .objects
                    .get(id)
                    .and_then(|obj| (obj.zone != Zone::Battlefield).then_some((*id, obj.zone)))
            })
            .collect();
        // `stale` and `event` are reported as SEPARATE fields. A consult reached
        // via the batch-safety probe in `observers_are_batch_safe` carries a
        // synthetic probe event that has nothing to do with the stale object;
        // conflating them would misdirect the first person to hit this.
        debug_assert!(
            stale.is_empty(),
            "TriggerIndex holds off-battlefield candidates (CR 113.6): \
             stale=(object_id, live_zone){stale:?} consulted_for_event={event:?}",
        );
    }
    out.retain(|id| {
        let Some(obj) = state.objects.get(id) else {
            // Absent objects are RETAINED, preserving the previous `is_none_or`
            // semantics exactly: the production candidate loop already
            // `continue`s on a missing object, `observers_are_batch_safe`'s
            // inertness check does the same, and `DebugAction::RemoveObject`
            // deletes an object without an index removal — so dropping here
            // would change no behavior while breaking existing fixtures.
            return true;
        };
        if obj.is_phased_out() && phase_out_source != Some(*id) {
            return false;
        }
        if obj.zone != Zone::Battlefield {
            // CR 113.6: "Abilities of all other objects usually function only
            // while that object is on the battlefield." This consult returns
            // BATTLEFIELD candidates — every caller scans them with
            // `zone_filter = Some(Zone::Battlefield)`. CR 113.6b / CR 113.6k
            // opt-ins (`trigger_zones`) are honoured by the dedicated off-zone
            // scan in `triggers.rs`, which populates itself from the
            // graveyard/exile/stack/command zone lists and never consults this
            // index. So a candidate whose LIVE zone is no longer the battlefield
            // is a stale entry and must not be handed back. This is the same
            // predicate `reindex_object_triggers` already enforces on the
            // maintenance side of this module.
            //
            // ONE DIRECTION, not an equivalence. `derived_views::is_live_battlefield_object`
            // is the CONJUNCTION of `state.battlefield.contains(id)` and
            // `obj.zone == Battlefield`; this checks only the second conjunct.
            // Dropping the first is not merely an O(|battlefield|) saving — it
            // is a weaker predicate, and the gap has a real producer:
            // `zones::absorb_component` removes a merged/melded component from
            // `state.battlefield` and then sets its zone BACK to `Battlefield`,
            // so a component can be zone-battlefield while not a battlefield
            // member. `reindex_object_triggers` admits on `obj.zone` alone, so
            // that shape can enter the index and this retain passes it through.
            // Uncontained here by choice: the reported bug is the opposite
            // direction, and a `contains` on every candidate is a real hot-path
            // cost for a shape with no current reindexing caller.
            //
            // CR 603.10a look-back sources are unaffected: a permanent observing
            // its own departure, a self-exploiting creature, and co-departed
            // observers are each produced by dedicated `collect_matching_triggers`
            // blocks in `triggers.rs` fed from the event / `ZoneChangeRecord`,
            // not from this index.
            //
            // `debug_assertions` builds have already panicked above with the full
            // diagnosis; release builds (including `server-release`, which the
            // multiplayer server ships and which inherits
            // `debug-assertions = false`) silently correct it here. It is on the
            // drop branch, so it costs nothing unless the defect actually occurs.
            //
            // This is the only recurrence evidence ON THE SERVER. It is NOT
            // evidence in the browser: `engine-wasm` has no `tracing-subscriber`
            // dependency and initialises no subscriber, so `tracing` events there
            // go to the no-op dispatcher and this line emits nothing. A recurrence
            // in a WASM release build is corrected silently and invisibly — and
            // that is the surface most players are on, so absence of warnings in
            // production is NOT evidence the desync stopped happening.
            // NOTE: this runs BEFORE the `sort_unstable_by_key`/`dedup` below, so
            // one stale id in several buckets logs more than once per consult, and
            // every subsequent consult re-logs while the desync persists — count
            // distinct object ids, not lines.
            tracing::warn!(
                object_id = ?id,
                live_zone = ?obj.zone,
                event = ?event,
                "TriggerIndex held an off-battlefield candidate; dropped (CR 113.6)"
            );
            return false;
        }
        true
    });
    out.sort_unstable_by_key(|id| id.0);
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::{GameObject, PhaseOutCause, PhaseStatus};
    use crate::types::ability::{TargetFilter, TypedFilter};
    use crate::types::game_state::ZoneChangeRecord;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::triggers::{TriggerEventKey, TriggerMode};

    fn etb_creature_def() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::Typed(TypedFilter::creature()))
    }

    #[test]
    fn etb_creature_emits_narrow_and_broad_keys_via_event() {
        // A `TriggerMode::ChangesZone` with `destination=Battlefield,
        // valid_card=Creature` registers under `EnterBattlefield(Creature)`.
        let def = etb_creature_def();
        let (keys, route) = keys_from_trigger_def(&def);
        assert!(keys.contains(&TriggerEventKey::EnterBattlefield(Some(CoreType::Creature))));
        assert!(!route);
    }

    #[test]
    fn sacrificed_emits_three_keys() {
        // CR 701.21 + CR 603.6c: sacrifice triggers reach via three keys.
        let def = TriggerDefinition::new(TriggerMode::Sacrificed)
            .valid_card(TargetFilter::Typed(TypedFilter::creature()));
        let (keys, _) = keys_from_trigger_def(&def);
        assert!(keys.contains(&TriggerEventKey::Sacrificed));
        assert!(keys.contains(&TriggerEventKey::LeaveBattlefield(Some(CoreType::Creature))));
        assert!(keys.contains(&TriggerEventKey::Dies(Some(CoreType::Creature))));
    }

    #[test]
    fn state_condition_emits_no_keys_and_no_unclassified() {
        let def = TriggerDefinition::new(TriggerMode::StateCondition);
        let (keys, route) = keys_from_trigger_def(&def);
        assert!(keys.is_empty());
        assert!(!route);
    }

    #[test]
    fn always_routes_to_unclassified() {
        let def = TriggerDefinition::new(TriggerMode::Always);
        let (keys, route) = keys_from_trigger_def(&def);
        assert!(keys.is_empty());
        assert!(route);
    }

    #[test]
    fn cumulative_upkeep_emits_upkeep_phase_key() {
        let def = TriggerDefinition::new(TriggerMode::PayCumulativeUpkeep);
        let (keys, _) = keys_from_trigger_def(&def);
        assert!(keys.contains(&TriggerEventKey::BeginningOfPhase(
            crate::types::phase::Phase::Upkeep
        )));
    }

    #[test]
    fn phase_in_uses_narrow_trigger_key_for_def_and_event() {
        let def = TriggerDefinition::new(TriggerMode::PhaseIn);
        let (keys, route) = keys_from_trigger_def(&def);
        assert!(keys.contains(&TriggerEventKey::PhaseIn));
        assert!(!route);

        let state = GameState::new_two_player(42);
        let event_keys = keys_from_event(
            &GameEvent::PermanentPhasedIn {
                object_id: crate::types::identifiers::ObjectId(1),
            },
            &state,
        );
        assert!(event_keys.contains(&TriggerEventKey::PhaseIn));
    }

    #[test]
    fn phase_out_uses_narrow_trigger_key_for_def_and_event() {
        let def = TriggerDefinition::new(TriggerMode::PhaseOut);
        let (keys, route) = keys_from_trigger_def(&def);
        assert!(keys.contains(&TriggerEventKey::PhaseOut));
        assert!(!route);

        let state = GameState::new_two_player(42);
        let event_keys = keys_from_event(
            &GameEvent::PermanentPhasedOut {
                object_id: crate::types::identifiers::ObjectId(1),
                indirect: false,
            },
            &state,
        );
        assert!(event_keys.contains(&TriggerEventKey::PhaseOut));
    }

    #[test]
    fn from_anywhere_to_graveyard_emits_battlefield_keys_and_stays_unclassified() {
        // CR 603.6c: A trigger with destination=Graveyard and unrestricted origin
        // ("from anywhere") should emit Dies and LeaveBattlefield keys for
        // battlefield-origin events, but must still route through unclassified
        // for non-battlefield origins such as library→graveyard or hand→graveyard.
        let def = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Graveyard)
            .valid_card(TargetFilter::Typed(TypedFilter::card()));
        let (keys, route) = keys_from_trigger_def(&def);
        assert!(keys.contains(&TriggerEventKey::Dies(None)));
        assert!(keys.contains(&TriggerEventKey::LeaveBattlefield(None)));
        assert!(route);
    }

    #[test]
    fn from_anywhere_to_graveyard_candidate_survives_library_origin_event() {
        // CR 603.6c: "from anywhere" includes library→graveyard moves. The
        // event side emits NO key at all for this shape, so the unclassified
        // safety bucket — which `candidates_for_event` unions unconditionally —
        // is what carries this class until a generic graveyard key exists.
        let mut state = GameState::new_two_player(42);
        let watcher = ObjectId(99);
        let def = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Graveyard)
            .valid_card(TargetFilter::Typed(TypedFilter::card()));
        state.trigger_index.add(watcher, &[def], false);

        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(7),
            from: Some(Zone::Library),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord::test_minimal(
                ObjectId(7),
                Some(Zone::Library),
                Zone::Graveyard,
            )),
        };

        let candidates = candidates_for_event(&state, &event);
        assert!(candidates.contains(&watcher));
    }

    /// CR 701.17a: the mill key's event-side source moved off the zone shape and
    /// onto the action event. Both legs run in one invocation, so a
    /// `keys_from_event` that answered nothing at all cannot pass.
    #[test]
    fn milled_key_comes_from_the_action_event_not_the_zone_shape() {
        let state = GameState::new_two_player(42);

        let zone_change = GameEvent::ZoneChanged {
            object_id: ObjectId(7),
            from: Some(Zone::Library),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord::test_minimal(
                ObjectId(7),
                Some(Zone::Library),
                Zone::Graveyard,
            )),
        };
        assert!(
            !keys_from_event(&zone_change, &state).contains(&TriggerEventKey::Milled),
            "the library→graveyard zone shape must no longer carry the Milled key"
        );

        for to in [Zone::Graveyard, Zone::Exile] {
            let milled = GameEvent::Milled {
                player_id: PlayerId(0),
                object_id: ObjectId(7),
                to,
            };
            assert!(
                keys_from_event(&milled, &state).contains(&TriggerEventKey::Milled),
                "the CR 701.17a action event carries the Milled key whatever zone \
                 the card reached (CR 701.17c); {to:?} did not"
            );
        }
    }

    #[test]
    fn rebuild_preserves_materialized_trigger_occurrence_refs() {
        let mut state = GameState::new_two_player(42);
        let object_id = ObjectId(77);
        let mut object = GameObject::new(
            object_id,
            CardId(77),
            PlayerId(0),
            "Indexed Trigger".to_string(),
            Zone::Battlefield,
        );
        object.base_trigger_definitions = std::sync::Arc::new(vec![etb_creature_def()]);
        object.materialize_base_trigger_definitions();
        let before = object.trigger_definition_ref(&object.trigger_definitions[0]);
        state.objects.insert(object_id, object);
        state.battlefield.push_back(object_id);

        TriggerIndex::rebuild_from_battlefield(&mut state);

        let object = &state.objects[&object_id];
        assert_eq!(
            before,
            object.trigger_definition_ref(&object.trigger_definitions[0]),
            "index rebuild is classification-only and must not reallocate trigger identity"
        );
    }

    #[test]
    fn ensure_ready_rebuilds_deserialized_taps_for_mana_index() {
        let mut state = GameState::new_two_player(42);
        let object_id = ObjectId(78);
        let mut object = GameObject::new(
            object_id,
            CardId(78),
            PlayerId(0),
            "Deserialized Mana Trigger".to_string(),
            Zone::Battlefield,
        );
        object.base_trigger_definitions =
            std::sync::Arc::new(vec![TriggerDefinition::new(TriggerMode::TapsForMana)]);
        object.materialize_base_trigger_definitions();
        state.objects.insert(object_id, object);
        state.battlefield.push_back(object_id);

        let serialized = serde_json::to_value(&state).expect("state serializes");
        let mut restored: GameState = serde_json::from_value(serialized).expect("state restores");
        assert!(restored.trigger_index.by_key.is_empty());
        assert!(restored.trigger_index.unclassified.is_empty());

        ensure_ready(&mut restored);

        let event = GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: ObjectId(79),
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: crate::types::events::ManaTapState::FromTap,
        };
        assert_eq!(
            candidates_for_event(&restored, &event).as_slice(),
            &[object_id],
            "the first post-deserialize inline-mana consult must restore its candidate"
        );
    }

    #[test]
    fn tapped_for_mana_candidates_exclude_irrelevant_battlefield_objects() {
        let mut state = GameState::new_two_player(42);
        for id in 0..64 {
            let object_id = ObjectId(id);
            state.objects.insert(
                object_id,
                GameObject::new(
                    object_id,
                    CardId(id),
                    PlayerId(0),
                    format!("Irrelevant {id}"),
                    Zone::Battlefield,
                ),
            );
            state.battlefield.push_back(object_id);
        }
        let relevant = [ObjectId(100), ObjectId(101)];
        for object_id in relevant {
            let mut object = GameObject::new(
                object_id,
                CardId(object_id.0),
                PlayerId(0),
                format!("Mana Trigger {}", object_id.0),
                Zone::Battlefield,
            );
            object.base_trigger_definitions =
                std::sync::Arc::new(vec![TriggerDefinition::new(TriggerMode::TapsForMana)]);
            object.materialize_base_trigger_definitions();
            state.objects.insert(object_id, object);
            state.battlefield.push_back(object_id);
        }
        TriggerIndex::rebuild_from_battlefield(&mut state);

        let event = GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: ObjectId(999),
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: crate::types::events::ManaTapState::FromTap,
        };
        let candidates = candidates_for_event(&state, &event);

        assert_eq!(state.battlefield.len(), 66);
        assert_eq!(candidates.as_slice(), &relevant);
        assert_eq!(
            candidates.len(),
            2,
            "only TapsForMana candidates are visited"
        );
    }

    // -----------------------------------------------------------------------
    // CR 113.6 live-zone guard on the consult path.
    //
    // Every negative below induces the `state.battlefield` / `obj.zone` desync
    // by writing `obj.zone` DIRECTLY, leaving `state.battlefield` and the index
    // untouched. `zones::move_to_zone` cannot be used to induce it: its
    // CR 603.6c hook drops the object from the index whenever it leaves the
    // battlefield, so the stale entry these rows require would never exist.
    // -----------------------------------------------------------------------

    /// A phased-in battlefield permanent carrying one `TapsForMana` trigger,
    /// registered through the production rebuild path.
    fn indexed_mana_trigger_source(state: &mut GameState, object_id: ObjectId) {
        let mut object = GameObject::new(
            object_id,
            CardId(object_id.0),
            PlayerId(0),
            format!("Indexed Source {}", object_id.0),
            Zone::Battlefield,
        );
        object.base_trigger_definitions =
            std::sync::Arc::new(vec![TriggerDefinition::new(TriggerMode::TapsForMana)]);
        object.materialize_base_trigger_definitions();
        state.objects.insert(object_id, object);
        state.battlefield.push_back(object_id);
        TriggerIndex::rebuild_from_battlefield(state);
    }

    fn tapped_for_mana_event() -> GameEvent {
        GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: ObjectId(999),
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: crate::types::events::ManaTapState::FromTap,
        }
    }

    /// Row 1 — the paired positive reach-guard for the zone rows below. Without
    /// it, a guard that dropped every candidate would satisfy them vacuously.
    #[test]
    fn on_battlefield_source_is_still_a_candidate() {
        let mut state = GameState::new_two_player(42);
        let object_id = ObjectId(200);
        indexed_mana_trigger_source(&mut state, object_id);

        assert!(
            candidates_for_event(&state, &tapped_for_mana_event()).contains(&object_id),
            "an indexed source whose live zone IS the battlefield must remain a candidate"
        );
    }

    /// Rows 2-6 — The Locust God (Hand), Lightning Rift (Graveyard), and the
    /// sibling zones. CR 113.6: abilities of a permanent function only while it
    /// is on the battlefield, so a stale index entry pointing off-battlefield
    /// must not be handed back to the candidate loop. Command is included
    /// because command-zone triggers are owned by the dedicated off-zone scan
    /// in `triggers.rs`, never by this index.
    #[test]
    fn off_battlefield_sources_are_not_candidates() {
        let leaked: Vec<Zone> = [
            Zone::Hand,
            Zone::Graveyard,
            Zone::Exile,
            Zone::Library,
            Zone::Command,
        ]
        .into_iter()
        .filter(|zone| {
            let mut state = GameState::new_two_player(42);
            let object_id = ObjectId(200);
            indexed_mana_trigger_source(&mut state, object_id);
            // Induce the desync: live zone moves, `state.battlefield` and the
            // index are deliberately left stale.
            state.objects.get_mut(&object_id).unwrap().zone = *zone;
            candidates_for_event(&state, &tapped_for_mana_event()).contains(&object_id)
        })
        .collect();

        assert!(
            leaked.is_empty(),
            "CR 113.6: a stale index entry whose live zone is off the battlefield \
             must not be returned as a candidate; leaked from {leaked:?}"
        );
    }

    /// Row 7 — CR 702.26b: a phased-out permanent is treated as though it does
    /// not exist, so it is not a candidate. Unchanged by the zone guard.
    #[test]
    fn phased_out_source_is_not_a_candidate() {
        let mut state = GameState::new_two_player(42);
        let object_id = ObjectId(200);
        indexed_mana_trigger_source(&mut state, object_id);
        state.objects.get_mut(&object_id).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };

        assert!(
            !candidates_for_event(&state, &tapped_for_mana_event()).contains(&object_id),
            "CR 702.26b: a phased-out permanent must not be a candidate"
        );
    }

    /// Row 8 — CR 702.26b carve-out: the permanent that just phased out is the
    /// source of its own `PermanentPhasedOut` event and MUST survive. This row
    /// is multi-authority on purpose (phased out AND the event source), and it
    /// is the row that proves the zone guard is not collateral damage: phasing
    /// changes status, not zone, so the carve-out object is still
    /// `Zone::Battlefield` and passes the new check.
    #[test]
    fn phase_out_event_source_survives_the_carve_out() {
        let mut state = GameState::new_two_player(42);
        let object_id = ObjectId(200);
        indexed_mana_trigger_source(&mut state, object_id);
        state.objects.get_mut(&object_id).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };

        let event = GameEvent::PermanentPhasedOut {
            object_id,
            indirect: false,
        };
        assert!(
            candidates_for_event(&state, &event).contains(&object_id),
            "CR 702.26b: the phase-out event's own source must still be a candidate"
        );
    }

    /// Row 9 — fail-open is preserved. An id in the index with no `GameObject`
    /// at all is RETAINED, pinning the `let ... else { return true }` rewrite
    /// against the previous `is_none_or` semantics. Two production routes reach
    /// this: the candidate loop's own missing-object `continue`, and
    /// `DebugAction::RemoveObject`, which deletes an object without an index
    /// removal.
    #[test]
    fn indexed_id_without_an_object_is_retained() {
        let mut state = GameState::new_two_player(42);
        let ghost = ObjectId(201);
        state.trigger_index.add(
            ghost,
            &[TriggerDefinition::new(TriggerMode::TapsForMana)],
            false,
        );

        assert!(
            candidates_for_event(&state, &tapped_for_mana_event()).contains(&ghost),
            "an indexed id with no GameObject must be retained (fail-open)"
        );
    }

    /// Row 11 — the exact desync shape the env-gated differential is
    /// structurally blind to: the id is off-battlefield by `obj.zone` while
    /// still present in `state.battlefield`. The differential's shadow is drawn
    /// from the live `obj.zone`, so this object is absent from the shadow and
    /// present in production candidates — it lands only in `production - shadow`,
    /// the direction that was removed for having a false-positive floor. So the
    /// differential cannot see THIS shape at all, and this guard is the only
    /// detector for it.
    #[test]
    fn off_battlefield_source_still_in_the_battlefield_vector_is_not_a_candidate() {
        let mut state = GameState::new_two_player(42);
        let object_id = ObjectId(200);
        indexed_mana_trigger_source(&mut state, object_id);
        state.objects.get_mut(&object_id).unwrap().zone = Zone::Hand;

        assert!(
            state.battlefield.contains(&object_id),
            "reach-guard: this row is only meaningful while the id is STILL in \
             `state.battlefield` — that is the desync under test"
        );
        assert!(
            !candidates_for_event(&state, &tapped_for_mana_event()).contains(&object_id),
            "CR 113.6: `state.battlefield` membership is not the authority; the \
             live `obj.zone` is"
        );
    }
}
