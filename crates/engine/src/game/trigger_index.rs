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
        TypeFilter::Instant
        | TypeFilter::Sorcery
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
        | TriggerMode::BecomesBlocked => {
            push(TriggerEventKey::Blocks);
        }

        // --- Counters ---
        TriggerMode::CounterAdded
        | TriggerMode::CounterAddedOnce
        | TriggerMode::CounterAddedAll
        | TriggerMode::CounterTypeAddedAll => push(TriggerEventKey::CounterAdded),
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
        TriggerMode::ManaAdded => push(TriggerEventKey::ManaProduced),
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
        TriggerMode::PlanarDice
        | TriggerMode::PlaneswalkedFrom
        | TriggerMode::PlaneswalkedTo
        | TriggerMode::ChaosEnsues => return (keys, true),

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
fn keys_from_event(event: &GameEvent, state: &GameState) -> Keys {
    let mut out: Keys = SmallVec::new();
    let mut push = |k: TriggerEventKey| {
        if !out.contains(&k) {
            out.push(k);
        }
    };

    match event {
        // CR 732.2: a halted-resolution notification produces no trigger keys.
        GameEvent::GameStarted | GameEvent::ResolutionHalted { .. } => {}
        GameEvent::TurnStarted { .. } => push(TriggerEventKey::TurnStarted),
        GameEvent::PhaseChanged { phase } => push(TriggerEventKey::BeginningOfPhase(*phase)),
        GameEvent::PriorityPassed { .. } => {}
        GameEvent::CreatureExerted { .. } => push(TriggerEventKey::Exerted),
        GameEvent::CreatureEnlisted { .. } => push(TriggerEventKey::Enlisted),
        GameEvent::Foretold { .. } => push(TriggerEventKey::Foretold),
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
            from, to, record, ..
        } => {
            // CR 603.6a: ETB. Emit broad + per-core-type narrow.
            if *to == Zone::Battlefield {
                push(TriggerEventKey::EnterBattlefield(None));
                for ct in &record.core_types {
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
            // CR 701.17: `match_milled` consumes `ZoneChanged { from: Library,
            // to: Graveyard }`. Emit Milled key for that exact shape.
            if *from == Some(Zone::Library) && *to == Zone::Graveyard {
                push(TriggerEventKey::Milled);
            }
        }
        GameEvent::LifeChanged { .. } => push(TriggerEventKey::LifeChanged),
        GameEvent::ControllerChanged { .. } => push(TriggerEventKey::ChangesController),
        GameEvent::ManaAdded { .. } => push(TriggerEventKey::ManaProduced),
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
        GameEvent::AttackersDeclared { .. } => push(TriggerEventKey::Attacks),
        GameEvent::BlockersDeclared { .. } => push(TriggerEventKey::Blocks),
        GameEvent::CombatTaxPaid { .. } | GameEvent::CombatTaxDeclined { .. } => {}
        GameEvent::BecomesTarget { .. } => push(TriggerEventKey::BecomesTarget),
        GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. } => {}
        GameEvent::ReplacementApplied { .. } => {}
        GameEvent::Transformed { .. } | GameEvent::TurnedFaceUp { .. } => {
            push(TriggerEventKey::FaceOrTransform);
        }
        GameEvent::DayNightChanged { .. } => push(TriggerEventKey::DayNightChanged),
        GameEvent::CardsRevealed { .. } => push(TriggerEventKey::Revealed),
        GameEvent::CrimeCommitted { .. } => push(TriggerEventKey::PlayerActionPerformed),
        GameEvent::Cycled { .. } => {}
        GameEvent::PlayerPerformedAction { .. } => push(TriggerEventKey::PlayerActionPerformed),
        GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. } => {}
        GameEvent::CaseSolved { .. } | GameEvent::ClassLevelGained { .. } => {
            push(TriggerEventKey::DungeonOrClassOrCase);
        }
        GameEvent::MonarchChanged { .. } => push(TriggerEventKey::MonarchOrInitiative),
        GameEvent::CityBlessingGained { .. } => {}
        // CR 103.1: setup determination, not a CR 706 die-roll trigger source.
        GameEvent::StartingPlayerContest { .. } => {}
        GameEvent::DieRolled { .. } | GameEvent::CoinFlipped { .. } => {
            push(TriggerEventKey::DieOrCoin);
        }
        GameEvent::RingTemptsYou { .. } => push(TriggerEventKey::PlayerActionPerformed),
        GameEvent::RoomEntered { .. } | GameEvent::DungeonCompleted { .. } => {
            push(TriggerEventKey::DungeonOrClassOrCase);
        }
        GameEvent::RoomDoorUnlocked { .. } | GameEvent::BecomesPlotted { .. } => {}
        GameEvent::InitiativeTaken { .. } => push(TriggerEventKey::MonarchOrInitiative),
        GameEvent::AttractionOpened { .. } | GameEvent::AttractionsRolledToVisit { .. } => {}
        GameEvent::AttractionVisited { .. } => push(TriggerEventKey::VisitAttraction),
        GameEvent::Specialized { .. } => push(TriggerEventKey::Specializes),
        // CR 702.140c-d: `TriggerMode::Mutates` is routed to the always-checked
        // unclassified bucket (see `keys_from_trigger_def`), so the `Mutated`
        // event needs no dedicated index key — `match_mutates` is always consulted.
        GameEvent::Mutated { .. } => {}
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
        EffectKind::Renown => push(TriggerEventKey::Renowned),
        EffectKind::Monstrosity => push(TriggerEventKey::BecomesMonstrous),
        EffectKind::ManifestDread => push(TriggerEventKey::ManifestDreadResolved),
        EffectKind::DayTimeChange => push(TriggerEventKey::DayNightChanged),
        // All other variants: not dispatched on by any production
        // EffectResolved matcher (verified against `trigger_matchers.rs` 1-3216).
        // Explicit `&[]`-equivalent arms — a future contributor who adds a
        // new EffectResolved-dispatching matcher will force this match to be
        // re-classified.
        EffectKind::StartYourEngines
        | EffectKind::ChangeSpeed
        | EffectKind::DealDamage
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
        | EffectKind::Proliferate
        | EffectKind::ProliferateTarget
        | EffectKind::EndTheTurn
        | EffectKind::EndCombatPhase
        | EffectKind::Populate
        | EffectKind::Clash
        | EffectKind::Vote
        | EffectKind::SeparateIntoPiles
        | EffectKind::SwitchPT
        | EffectKind::CopySpell
        | EffectKind::EpicCopy
        | EffectKind::CopyTokenOf
        | EffectKind::Myriad
        | EffectKind::Encore
        | EffectKind::Meld
        | EffectKind::ExileHaunting
        | EffectKind::HideawayConceal
        | EffectKind::BecomeCopy
        | EffectKind::ChooseCard
        | EffectKind::PutCounter
        | EffectKind::PutCounterAll
        | EffectKind::MultiplyCounter
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
        | EffectKind::ExileTop
        | EffectKind::TargetOnly
        | EffectKind::Choose
        | EffectKind::ChooseDamageSource
        | EffectKind::Suspect
        | EffectKind::Connive
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
        | EffectKind::CreateEmblem
        | EffectKind::PayCost
        | EffectKind::CastFromZone
        | EffectKind::FreeCastFromZones
        | EffectKind::ExileResolvingSpellInsteadOfGraveyard
        | EffectKind::PreventDamage
        | EffectKind::CreateDamageReplacement
        | EffectKind::Regenerate
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
        | EffectKind::OpenAttractions
        | EffectKind::RollToVisitAttractions
        | EffectKind::ProcessRadCounters
        | EffectKind::GrantCastingPermission
        | EffectKind::ChooseFromZone
        | EffectKind::ChooseObjectsIntoTrackedSet
        | EffectKind::ChooseAndSacrificeRest
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
        | EffectKind::CollectEvidence
        | EffectKind::Endure
        | EffectKind::BlightEffect
        | EffectKind::Seek
        | EffectKind::SetLifeTotal
        | EffectKind::SetDayNight
        | EffectKind::GiveControl
        | EffectKind::RemoveFromCombat
        | EffectKind::Conjure
        | EffectKind::Intensify
        | EffectKind::DraftFromSpellbook
        | EffectKind::ChooseOneOf
        | EffectKind::Specialize
        | EffectKind::Unimplemented
        | EffectKind::Crew
        | EffectKind::Station
        | EffectKind::Saddle
        | EffectKind::Transform
        | EffectKind::TurnFaceUp
        // Added on origin/main after this branch point. No production
        // EffectResolved-dispatching matcher consumes either: cast-copy fires
        // on cast events (CastCopyOfCard, Mizzix's Mastery), and life/P-T
        // exchange emits LifeChanged/PowerToughnessChanged handled by their own
        // event arms (ExchangeLifeWithStat). No-op here.
        | EffectKind::CastCopyOfCard
        | EffectKind::ExchangeLifeWithStat => {}
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
    let defs: SmallVec<[TriggerDefinition; 4]> =
        obj.trigger_definitions.as_slice().iter().cloned().collect();
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
                fresh.add(obj_id, obj.trigger_definitions.as_slice(), synthetic);
            }
        }
        state.trigger_index = fresh;
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
    out.retain(|id| {
        state
            .objects
            .get(id)
            .is_none_or(|obj| !obj.is_phased_out() || phase_out_source == Some(*id))
    });
    out.sort_unstable_by_key(|id| id.0);
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{TargetFilter, TypedFilter};
    use crate::types::game_state::ZoneChangeRecord;
    use crate::types::triggers::TriggerEventKey;

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
        // event side emits only Milled for this shape, so this class must stay
        // in the unclassified safety bucket until a generic graveyard key exists.
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
}
