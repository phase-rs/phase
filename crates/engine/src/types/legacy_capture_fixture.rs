//! TEST-ONLY migrations for committed capture fixtures that predate a
//! persisted-provenance field the restore boundary now requires.
//!
//! This module is the same kind of concession as
//! [`crate::types::game_state::record_captured_onsets`], which declares an old
//! capture's CR 611.2e onset record and its CR 601.2c cast-history provenance.
//! The third field in that family is the announced-target latch a parked
//! [`PendingCast`] owes (`GameState::reject_unknown_pending_target_facts`):
//! CR 115.1 + CR 601.2c facts, captured at the target-declaration → payment
//! boundary and consulted by every carrier that resumes a cast. The fourth is
//! the CR 106.3 producing epoch a floating [`ManaUnit`] owes
//! (`GameState::bind_or_reject_floating_mana_provenance`).
//!
//! Nothing here is production behaviour. Every entry point is
//! `#[cfg(any(test, feature = "test-support"))]` and carries the same honesty
//! statement as the onsets migration: it never runs in a product build, and it
//! does not weaken the production rule — a real save parked on a declared-but-
//! unlatchable cast is still refused at `PersistedGameState::prepare_for_restore`
//! (see `cast_history_restore_target_epoch.rs`:
//! `a_manual_payment_save_without_the_latch_is_refused`,
//! `a_return_cost_park_without_the_latch_is_refused`,
//! `a_mid_payment_characteristics_change_without_a_latch_is_refused`, and
//! `game_state.rs::prepare_for_restore_checks_the_activation_mill_carrier`), and
//! a real save holding floating mana whose producing epoch it cannot prove is
//! still refused there too (see `mana_producer_fact_continuity.rs`:
//! `legacy_pinless_pool_is_refused_and_pinned_pool_still_pays`, plus this
//! module's own `a_pinless_pool_the_capture_cannot_prove_is_still_refused`).
//!
//! # Why re-derivation is honest HERE and nowhere else
//!
//! Production must refuse rather than re-derive because a live read at restore
//! time (`state.objects`) describes the board *now*, not at CR 601.2c: CR 613.1
//! layers 4/6/7 rewrite characteristics with no zone change to invalidate an
//! incarnation pin, and a cost may have moved the target outright (CR 400.7).
//! A committed fixture is a different question: it is frozen test input whose
//! whole purpose is to be admitted by *some* honest reading, and the reading
//! this module takes is exactly the one the engine itself takes when it latches
//! live — `crate::game::casting::capture_target_facts` over
//! [`crate::game::ability_utils::declared_targets_in_chain`]. Reusing that pair
//! rather than assembling JSON by hand is what keeps the stamped facts
//! identical in shape, ordering and characteristic snapshot to what
//! `latch_committed_target_facts` would have written for the same capture.
//!
//! The same rule applies to the mana epoch, and it is why that step DERIVES
//! instead of defaulting. The epoch a pre-pin capture is missing is the one
//! `crate::game::mana_payment::producing_incarnation_for_ability` would have
//! pinned at production, and the capture carries one journal that can still
//! name it: `zone_changes_this_turn`, whose every record binds the object's
//! PRE-change identity (`ZoneChangeRecord::trigger_source_context`) — the exact
//! epoch that produced the mana — rather than only the object's present one.
//! Reading that journal is re-derivation from evidence the capture holds; it is
//! not a guess, and a unit whose epoch the journal does not settle is left
//! unstamped and still refused.

use crate::types::game_state::{ActivationTargetFact, GameState, PendingCast, PendingCastOwner};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaPool, ManaUnit};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use serde_json::Value;

/// Which persisted shape a game-state root of the committed capture is, decided
/// by the key it sits under — the same way a real restore decides.
///
/// The round trip goes through the engine's OWN ingress and egress rather than
/// hand-rolled JSON. Both shapes carry a
/// [`crate::types::resolution::ResolutionStateWire`]: a
/// `{gameState, …}` debug export (and each bare `turnCheckpoints` element, which
/// every committed export writes in the same form) is the raw persistence wire,
/// read with `GameStateDecodeMode::PersistedRaw` — the mode that infers a missing
/// version discriminator — while a `{state, …}` trusted envelope holds the same
/// wire and is read with `GameStateDecodeMode::TrustedEnvelope`, which requires
/// the declared version. Writing back goes through
/// `ResolutionStateWire::from_game_state`, the serializer
/// `PersistedGameState` itself uses, so the emitted payload is exactly what this
/// build would have written for that state.
#[derive(Clone, Copy)]
enum CaptureForm {
    /// A `{gameState, …}` export, a bare raw state, or a `turnCheckpoints`
    /// element.
    RawExport,
    /// A `{state, …}` trusted envelope.
    TrustedEnvelope,
}

impl CaptureForm {
    fn decode(self, root: &Value) -> Option<GameState> {
        let mode = match self {
            Self::RawExport => crate::types::game_state::GameStateDecodeMode::PersistedRaw,
            Self::TrustedEnvelope => crate::types::game_state::GameStateDecodeMode::TrustedEnvelope,
        };
        crate::types::game_state::GameStateDecode::decode_persisted_resolution_state(
            root.clone(),
            mode,
        )
        .ok()
    }
}

/// The persisted form of `state`, as [`crate::types::game_state::PersistedGameState`]
/// writes it.
fn encode_wire(state: GameState) -> Option<Value> {
    serde_json::to_value(crate::types::resolution::ResolutionStateWire::from_game_state(state)).ok()
}

/// Test support: stamp the CR 115.1 + CR 601.2c announced-target facts a
/// pre-latch committed capture's parked cast owes, so the capture is admitted
/// by the restore boundary it predates.
///
/// This is FIXTURE MIGRATION, NOT PRODUCTION BEHAVIOUR: it is
/// `#[cfg(any(test, feature = "test-support"))]`, it never runs in a product
/// build, and it does not weaken the production rule — production restore
/// still refuses a save whose declared-but-unlatched cast it cannot describe,
/// because a live board read there would be a post-announcement reading
/// (`PersistedRestoreError::UnknownPendingTargetFacts`).
///
/// # What it stamps, and what it refuses to stamp
///
/// A fact is only produced when the capture itself records the declaration:
/// the cast's own announced ability chain is the same record
/// [`crate::game::ability_utils::declared_targets_in_chain`] reads for the live
/// latch, so a non-empty list means "this cast declared these slots". A pending
/// cast on that list is stamped on whichever carrier is missing the latch — its
/// own `PendingCast::committed_target_facts`, or the
/// `prepaid_finalization.pre_payment_checks.committed_target_facts` the
/// production guard also inspects (both when both are unknown).
///
/// An EMPTY declared list is left untouched, deliberately. `None` there means
/// "not declared yet" (CR 601.2a–b) or "the capture predates the latch on a
/// cast that declared nothing", and the two are not distinguishable from the
/// capture alone. Stamping `Some(vec![])` would claim a declaration the capture
/// does not record — or, worse, suppress the *later* declaration a pre-declaration
/// park still owes, because `latch_committed_target_facts` is first-capture-wins.
/// "Unknown" and "known-empty" stay different values here for the same reason
/// the production ledger keeps them apart. A fixture that trips the refusal with
/// an empty declared list therefore has to be regenerated, not migrated.
///
/// # Scope
///
/// Patches the game-state roots the committed capture shapes carry, marking
/// exactly the same roots as [`crate::types::game_state::record_captured_onsets`]:
/// a debug export `{gameState, turnCheckpoints?, …}`, a trusted envelope
/// `{state, …}`, or a bare state. Any other document (a card export, a CR 733
/// census map) is left unchanged, as is any root that does not decode as a
/// [`GameState`]. A root is rewritten only when something was stamped, and then
/// through the persisted-wire serializer above, so a capture that owes nothing
/// stays byte-identical.
///
/// It walks every `PendingCastOwner` rather than restating the custody list,
/// so a carrier added to `GameState::pending_cast_owners` is covered for free.
/// The two cost-move *completion* payloads that production also refuses
/// (`PendingCostMoveCompletion::FinalizeCast` / `FinalizeDelvedCast` and
/// `PendingSacrificeCostCompletion::FinalizeSpell`) are a different carrier
/// shape holding their checks on the resume root rather than in a `PendingCast`;
/// they are NOT stamped here. No committed capture carries one, and a fixture
/// that did would need regenerating.
pub fn record_captured_target_facts(document: &mut Value) {
    /// The facts a parked cast owes, or `None` when the capture records no
    /// declaration for it (see the module docs on why that stays unstamped).
    fn owed_facts(state: &GameState, pending: &PendingCast) -> Option<Vec<ActivationTargetFact>> {
        // CR 601.2c: read the declaration, never infer one.
        let declared = crate::game::ability_utils::declared_targets_in_chain(&pending.ability);
        if declared.is_empty() {
            return None;
        }
        let own_unknown = pending.committed_target_facts.is_none();
        let prepaid_unknown = pending
            .prepaid_finalization
            .as_deref()
            .and_then(|prepared| prepared.pre_payment_checks.as_deref())
            .is_some_and(|checks| checks.committed_target_facts.is_none());
        if !own_unknown && !prepaid_unknown {
            return None;
        }
        // CR 115.1 + CR 400.7: the engine's own capture authority, so the
        // stamped facts are byte-identical to what the live latch writes — an
        // object target carries its public-characteristic snapshot, a player
        // target carries the player, and a target the capture cannot describe
        // is dropped rather than invented.
        Some(crate::game::casting::capture_target_facts(state, &declared))
    }

    fn mark(root: &mut Value, form: CaptureForm) {
        let Some(state) = form.decode(root) else {
            return;
        };
        let mut owed: Vec<(PendingCastOwner, Vec<ActivationTargetFact>)> = Vec::new();
        for owner in state.pending_cast_owners() {
            let Some(pending) = state.pending_cast_at(owner) else {
                continue;
            };
            if let Some(facts) = owed_facts(&state, pending) {
                owed.push((owner, facts));
            }
        }
        if owed.is_empty() {
            return;
        }
        let mut patched = state;
        for (owner, facts) in owed {
            let Some(pending) = patched.pending_cast_at_mut(owner) else {
                continue;
            };
            if let Some(prepared) = pending.prepaid_finalization.as_deref_mut() {
                if let Some(checks) = prepared.pre_payment_checks.as_deref_mut() {
                    if checks.committed_target_facts.is_none() {
                        checks.committed_target_facts = Some(facts.clone());
                    }
                }
            }
            if pending.committed_target_facts.is_none() {
                pending.committed_target_facts = Some(facts);
            }
        }
        if let Some(value) = encode_wire(patched) {
            *root = value;
        }
    }

    let Some(object) = document.as_object_mut() else {
        return;
    };
    if let Some(game_state) = object.get_mut("gameState") {
        mark(game_state, CaptureForm::RawExport);
        if let Some(checkpoints) = object
            .get_mut("turnCheckpoints")
            .and_then(Value::as_array_mut)
        {
            checkpoints
                .iter_mut()
                .for_each(|checkpoint| mark(checkpoint, CaptureForm::RawExport));
        }
    } else if let Some(state) = object.get_mut("state") {
        mark(state, CaptureForm::TrustedEnvelope);
    } else if object.contains_key("objects") && object.contains_key("turn_number") {
        mark(document, CaptureForm::RawExport);
    }
}

/// [`record_captured_target_facts`] over a JSON string, for loaders that hand a
/// string onward.
pub fn record_captured_target_facts_json(json: &str) -> String {
    let mut document: Value = serde_json::from_str(json).expect("a committed capture is JSON");
    record_captured_target_facts(&mut document);
    serde_json::to_string(&document).expect("a committed capture re-serializes")
}

/// Test support: stamp the CR 106.3 producing epoch a pre-pin committed
/// capture's floating mana owes, so the capture is admitted by the restore
/// boundary it predates.
///
/// This is FIXTURE MIGRATION, NOT PRODUCTION BEHAVIOUR: it is
/// `#[cfg(any(test, feature = "test-support"))]`, it never runs in a product
/// build, and it does not weaken the production rule — production restore still
/// refuses floating mana whose producing epoch the save cannot prove, because a
/// live read there would price the unit against whatever object occupies the
/// storage id now (`PersistedRestoreError::AmbiguousFloatingManaProvenance`).
///
/// # What it stamps, and how the epoch is derived
///
/// Only the shape that guard convicts is touched: a unit with no
/// `ManaUnit::source_incarnation` (and not a CR 702.51a convoke marker) whose
/// `source_id` still names a LIVE object that is not a battlefield permanent.
/// Every other pin-less unit is already admitted and is left byte-identical.
///
/// CR 106.3 makes a unit's source the source of the ability that produced it,
/// and CR 113.7 makes that the object that generated the ability — the object
/// as it stood when the ability resolved, which CR 400.7 says is a different
/// object from whatever later occupies the same storage id. So the epoch to
/// stamp is the one the producer had while it made the mana, and from the
/// capture alone that is settled by exactly one record: this turn's zone-change
/// journal. Each [`crate::types::game_state::ZoneChangeRecord`] binds the
/// object's PRE-change identity (`trigger_source_context.identity`), so the
/// record that took the source out of the zone it produced the mana in still
/// names the producing epoch — the same value
/// `crate::game::mana_payment::producing_incarnation_for_ability` would have
/// pinned live.
///
/// The derivation runs on typed state, not on JSON: the root is decoded through
/// the engine's own ingress ([`CaptureForm`]) and the journal is read as
/// [`crate::types::game_state::ZoneChangeRecord`]s, so a record the engine
/// cannot represent is a root the migration declines rather than a hand-parsed
/// field. A unit is stamped only when ALL of the following hold, and is
/// otherwise left unknown:
///
/// * exactly ONE entry this turn departs the source from a zone a mana ability
///   can produce mana in — the battlefield (CR 605.1a) or the stack
///   (CR 106.3: a spell is the source of mana it produces). Two such departures
///   mean two tenures and the capture does not say which one produced the
///   mana; zero means the producing tenure is not in this turn at all;
/// * that departure is the one that left the object where it is now
///   (`to_zone` is the live object's zone), and its captured pre-change
///   identity is this very object, observed in the zone it left;
/// * one zone change bumps the incarnation exactly once
///   (`crate::game::zones`: a move's resulting incarnation is
///   `occurrence.incarnation + 1`), so the producing epoch must be exactly
///   `live incarnation - 1`. Anything else means another epoch bump happened
///   between production and this capture — a blink, a face-down turn-up — and
///   the journal no longer settles the question;
/// * floating mana is this turn's: CR 106.4 empties a pool at the end of each
///   step and phase and CR 500.5 does the same at a step's end, so a unit can
///   only outlive a boundary through a retention effect, and the engine's
///   retention markers expire no later than the cleanup action (CR 514.2).
///   That is what makes "the producing tenure is one of this turn's" a fact
///   about the capture rather than an assumption;
/// * if the capture also carries that epoch's exit last-known information
///   (`lki_by_incarnation`, written by the same departure), it agrees with the
///   journal record about what object it was. A disagreement is a corrupt
///   capture, and the unit stays unknown.
///
/// A unit this step declines is NOT guessed at. It keeps `None`, the guard still
/// refuses it with `AmbiguousFloatingManaProvenance`, and the fixture has to be
/// regenerated rather than migrated — the same "regenerate, don't migrate"
/// outcome [`record_captured_target_facts`] gives a cast that declared nothing.
///
/// # Scope
///
/// Walks the same game-state roots as [`record_captured_target_facts`] and
/// rewrites one only when a unit was stamped, through the persisted-wire
/// serializer, so a capture that owes nothing stays byte-identical. A stamped
/// pool is rebuilt with [`crate::types::mana::ManaPool::from_units`] — the
/// constructor the pool's own wire codec uses — so slot order, pips and shape
/// merging are the engine's, not a second implementation's.
pub fn record_captured_floating_mana_provenance(document: &mut Value) {
    /// CR 106.3 + CR 113.7 + CR 605.1a: the epoch `source_id` produced the mana
    /// at, or `None` when the capture cannot prove it. See the function docs.
    fn producing_incarnation(state: &GameState, source_id: ObjectId) -> Option<u64> {
        let object = state.objects.get(&source_id)?;
        // A source that is still a battlefield permanent is admitted on the live read
        // only while this turn's journal records NO departure from a producing zone for
        // its storage id (`GameState::unpinned_producer_epoch`). When it does
        // record one, the id has already had an earlier tenure and this migration
        // declines too: the journal names what that tenure was, but not whether IT or
        // the current object made the unit, and stamping the earlier epoch would be a
        // guess. The guard refuses the unit instead.
        if object.zone == Zone::Battlefield {
            return None;
        }
        let mut departures = state
            .zone_changes_this_turn
            .iter()
            .filter(|record| record.object_id == source_id)
            .filter(|record| matches!(record.from_zone, Some(Zone::Battlefield | Zone::Stack)));
        let departure = departures.next()?;
        // Two producing tenures this turn: the journal does not say which made
        // the mana, so nothing is stamped.
        if departures.next().is_some() {
            return None;
        }
        // The qualifying departure must be the one that left the object in the
        // zone it occupies now.
        if departure.to_zone != object.zone {
            return None;
        }
        // CR 400.7: the pre-change identity the journal bound is the producer's.
        let observed = departure.trigger_source_context.as_ref()?.identity;
        if observed.reference.object_id != source_id {
            return None;
        }
        if Some(observed.expected_zone) != departure.from_zone {
            return None;
        }
        let producing = observed.reference.incarnation;
        // Exactly one bump per zone change: anything else means an epoch bump
        // the journal does not account for between production and this capture.
        if object.incarnation != producing + 1 {
            return None;
        }
        // CR 400.7 + CR 608.2h: if the same departure's exit LKI is still in the
        // capture, the two records must describe the same object.
        let journal_name = &departure.trigger_source_context.as_ref()?.lki.name;
        if state
            .lki_by_incarnation
            .get(&source_id)
            .and_then(|epochs| epochs.get(&producing))
            .is_some_and(|exit| &exit.name != journal_name)
        {
            return None;
        }
        Some(producing)
    }

    fn mark(root: &mut Value, form: CaptureForm) {
        let Some(state) = form.decode(root) else {
            return;
        };
        // Pool position, not pip: `ManaPipId(0)` is the unstamped sentinel, so
        // two unjournaled units of one pool share a pip and only pool order
        // tells them apart.
        let mut owed: Vec<(PlayerId, usize, u64)> = Vec::new();
        for player in &state.players {
            for (index, unit) in player.mana_pool.units().enumerate() {
                if unit.source_incarnation.is_some() || unit.is_convoke_payment() {
                    continue;
                }
                let Some(producing) = producing_incarnation(&state, unit.source_id) else {
                    continue;
                };
                owed.push((player.id, index, producing));
            }
        }
        if owed.is_empty() {
            return;
        }
        let mut patched = state;
        for player in &mut patched.players {
            let stamped: Vec<(usize, u64)> = owed
                .iter()
                .filter(|(owner, _, _)| *owner == player.id)
                .map(|(_, index, producing)| (*index, *producing))
                .collect();
            if stamped.is_empty() {
                continue;
            }
            let units: Vec<ManaUnit> = player
                .mana_pool
                .units()
                .enumerate()
                .map(
                    |(index, unit)| match stamped.iter().find(|(at, _)| *at == index) {
                        Some((_, producing)) => ManaUnit {
                            source_incarnation: Some(*producing),
                            ..unit
                        },
                        None => unit,
                    },
                )
                .collect();
            player.mana_pool = ManaPool::from_units(units);
        }
        if let Some(value) = encode_wire(patched) {
            *root = value;
        }
    }

    let Some(object) = document.as_object_mut() else {
        return;
    };
    if let Some(game_state) = object.get_mut("gameState") {
        mark(game_state, CaptureForm::RawExport);
        if let Some(checkpoints) = object
            .get_mut("turnCheckpoints")
            .and_then(Value::as_array_mut)
        {
            checkpoints
                .iter_mut()
                .for_each(|checkpoint| mark(checkpoint, CaptureForm::RawExport));
        }
    } else if let Some(state) = object.get_mut("state") {
        mark(state, CaptureForm::TrustedEnvelope);
    } else if object.contains_key("objects") && object.contains_key("turn_number") {
        mark(document, CaptureForm::RawExport);
    }
}

/// [`record_captured_floating_mana_provenance`] over a JSON string, for loaders
/// that hand a string onward.
pub fn record_captured_floating_mana_provenance_json(json: &str) -> String {
    let mut document: Value = serde_json::from_str(json).expect("a committed capture is JSON");
    record_captured_floating_mana_provenance(&mut document);
    serde_json::to_string(&document).expect("a committed capture re-serializes")
}

/// The whole committed-capture provenance migration, in the order restore
/// reads it: the CR 611.2e onset record plus the CR 601.2c cast-history
/// provenance first (so the root decodes), then the announced-target latch this
/// module owns, then the CR 106.3 producing epoch of any floating mana. All
/// three halves stay test-only; see the module docs.
pub fn record_captured_provenance(document: &mut Value) {
    crate::types::game_state::record_captured_onsets(document);
    record_captured_target_facts(document);
    record_captured_floating_mana_provenance(document);
}

/// [`record_captured_provenance`] over a JSON string.
pub fn record_captured_provenance_json(json: &str) -> String {
    let mut document: Value = serde_json::from_str(json).expect("a committed capture is JSON");
    record_captured_provenance(&mut document);
    serde_json::to_string(&document).expect("a committed capture re-serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
    use crate::types::game_state::{PendingCast, PersistedGameState, PersistedRestoreError};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaCost, ManaType};
    use crate::types::player::PlayerId;

    /// The parked spell under test — Galvanic Blast's shape, "deals 2 damage to
    /// any target".
    const SPELL: ObjectId = ObjectId(9);

    /// A cast parked at the ordinary manual `ManaPayment` prompt (`pending_cast`,
    /// the `External` owner). `declares` chooses which capture shape this is:
    ///
    /// * `true` — the CR 601.2c declaration is recorded on the ability chain but
    ///   no latch carries it: exactly the pre-latch save the migration exists for;
    /// * `false` — a park taken before CR 601.2c, whose `None` is legitimate.
    fn parked_bolt(declares: bool) -> PendingCast {
        let targets = if declares {
            vec![TargetRef::Player(PlayerId(0))]
        } else {
            Vec::new()
        };
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            targets,
            SPELL,
            PlayerId(1),
        );
        PendingCast::new(SPELL, CardId(SPELL.0), ability, ManaCost::zero())
    }

    /// The committed capture a save of this park produces.
    fn capture_json(declares: bool) -> Value {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(parked_bolt(declares)));
        serde_json::to_value(PersistedGameState::capture(state)).expect("the park serializes")
    }

    /// The production restore chokepoint, with no fixture migration applied.
    fn admitted(value: Value) -> Result<GameState, PersistedRestoreError> {
        serde_json::from_value::<PersistedGameState>(value)
            .expect("the park decodes")
            .into_game_state()
    }

    /// The honesty pin for the whole module: the production boundary refuses
    /// this shape, and `record_captured_target_facts` — and only that — is what
    /// admits it. Deleting the guard, or letting the migration run outside
    /// test builds, fails one half or the other.
    #[test]
    fn a_declared_but_unlatched_park_is_refused_until_the_fixture_migration_runs() {
        let json = capture_json(true);
        assert_eq!(
            admitted(json.clone()).err(),
            Some(PersistedRestoreError::UnknownPendingTargetFacts {
                spell_object_id: Some(SPELL),
            }),
            "production restore must still refuse an unknown announced-target reading"
        );

        let mut migrated = json;
        record_captured_target_facts(&mut migrated);
        let state = admitted(migrated).expect("the migrated capture restores");
        let pending = state.pending_cast.as_deref().expect("the park survives");
        assert_eq!(
            pending.committed_target_facts.as_deref(),
            Some([ActivationTargetFact::Player(PlayerId(0))].as_slice()),
            "the stamped facts are the declaration the capture itself records"
        );
    }

    /// The stamped facts are the engine's own capture, and first capture wins:
    /// a second pass over an already-stamped capture is a no-op, exactly like
    /// `latch_committed_target_facts`.
    #[test]
    fn the_stamped_facts_are_the_engines_own_capture_and_the_migration_is_idempotent() {
        let mut json = capture_json(true);
        record_captured_target_facts(&mut json);
        let once = admitted(json.clone()).expect("the migrated capture restores");
        let facts = once
            .pending_cast
            .as_deref()
            .and_then(|pending| pending.committed_target_facts.clone())
            .expect("the latch is stamped");

        let mut twice = json;
        record_captured_target_facts(&mut twice);
        let again = admitted(twice).expect("a re-migrated capture still restores");
        assert_eq!(
            again
                .pending_cast
                .as_deref()
                .and_then(|pending| pending.committed_target_facts.clone()),
            Some(facts),
            "first capture wins, exactly as the live latch does"
        );
    }

    /// The over-rejection guard on the other side: a park whose capture records
    /// no declaration keeps its `None`, because stamping `Some(vec![])` would
    /// claim a declaration the capture does not record — and would suppress the
    /// declaration a pre-CR-601.2c park still owes.
    #[test]
    fn a_pre_declaration_park_is_left_untouched() {
        let json = capture_json(false);
        let mut migrated = json.clone();
        record_captured_target_facts(&mut migrated);
        assert_eq!(
            migrated, json,
            "a capture that records no declaration must be left byte-identical"
        );
        admitted(json).expect("a pre-declaration park needs no migration and is admitted");
    }

    // -----------------------------------------------------------------------
    // Floating-mana provenance
    // -----------------------------------------------------------------------

    /// The stuck seat, and the artifact whose own ability produced its mana —
    /// Chromatic Star, "{1}, {T}, Sacrifice ~: Add one mana of any color."
    const SEAT: PlayerId = PlayerId(1);
    const STAR_CARD: u64 = 108;

    /// A battlefield object, created the way the engine creates one.
    fn source_on_battlefield(state: &mut GameState) -> ObjectId {
        crate::game::zones::create_object(
            state,
            CardId(STAR_CARD),
            SEAT,
            "Chromatic Star".to_string(),
            Zone::Battlefield,
        )
    }

    /// The pre-pin pool: a Red unit as an engine without
    /// `ManaUnit::source_incarnation` would have written it, so no epoch.
    fn add_pinless_red_unit(state: &mut GameState, source: ObjectId) {
        state
            .players
            .iter_mut()
            .find(|player| player.id == SEAT)
            .expect("the seat is in the state")
            .mana_pool
            .add(ManaUnit::new(ManaType::Red, source, false, Vec::new()));
    }

    /// The committed capture this state would have produced.
    fn capture_of(state: GameState) -> Value {
        serde_json::to_value(PersistedGameState::capture(state)).expect("the state serializes")
    }

    /// The pinned epoch of the seat's single floating unit, if the capture
    /// restores at all.
    fn restored_pin(json: Value) -> Option<Option<u64>> {
        let state = admitted(json).ok()?;
        Some(
            state
                .players
                .iter()
                .find(|player| player.id == SEAT)
                .and_then(|player| player.mana_pool.units().next())
                .expect("the floating unit survives")
                .source_incarnation,
        )
    }

    /// CR 106.3 + CR 113.7 + CR 400.7: the epoch stamped is the one the source
    /// held while it PRODUCED the mana, read from the capture's own zone-change
    /// journal — not the epoch the live object holds now, which CR 400.7 makes a
    /// different object.
    #[test]
    fn a_pinless_unit_whose_producing_epoch_the_journal_proves_is_stamped() {
        let mut state = GameState::new_two_player(42);
        let star = source_on_battlefield(&mut state);
        let producing = state.objects[&star].incarnation;
        add_pinless_red_unit(&mut state, star);
        // CR 601.2h: the sacrifice is the mana ability's own cost, so the mana
        // and the departure are one event's two halves.
        crate::game::zones::move_to_zone(&mut state, star, Zone::Graveyard, &mut Vec::new());
        let landed = state.objects[&star].incarnation;
        assert_eq!(
            landed,
            producing + 1,
            "one zone change bumps the incarnation exactly once"
        );

        let json = capture_of(state);
        assert_eq!(
            admitted(json.clone()).err(),
            Some(PersistedRestoreError::AmbiguousFloatingManaProvenance {
                player: SEAT,
                source_id: star,
            }),
            "production restore must still refuse a pool it cannot prove"
        );

        let mut migrated = json.clone();
        record_captured_floating_mana_provenance(&mut migrated);
        assert_eq!(
            restored_pin(migrated.clone()),
            Some(Some(producing)),
            "the stamped epoch is the one the journal recorded BEFORE the move, \
             not the live object's {landed}"
        );

        let mut twice = migrated.clone();
        record_captured_floating_mana_provenance(&mut twice);
        assert_eq!(twice, migrated, "a re-migrated capture is a no-op");
        assert_eq!(
            restored_pin(twice),
            Some(Some(producing)),
            "the epoch survives a second pass unchanged"
        );
    }

    /// The honesty pin for this step, half one: a capture that records no
    /// departure for the producer cannot prove the epoch, so the unit keeps its
    /// `None` — the capture stays byte-identical and production still refuses
    /// it. Such a fixture has to be REGENERATED, not migrated.
    #[test]
    fn a_pinless_pool_the_capture_cannot_prove_is_still_refused() {
        let mut state = GameState::new_two_player(42);
        // Already in the graveyard as the capture is taken: no departure from a
        // zone a mana ability could have produced the mana in is recorded.
        let star = crate::game::zones::create_object(
            &mut state,
            CardId(STAR_CARD),
            SEAT,
            "Chromatic Star".to_string(),
            Zone::Graveyard,
        );
        add_pinless_red_unit(&mut state, star);

        let json = capture_of(state);
        let mut migrated = json.clone();
        record_captured_floating_mana_provenance(&mut migrated);
        assert_eq!(
            migrated, json,
            "a capture that proves nothing must be left byte-identical"
        );
        assert_eq!(
            admitted(migrated).err(),
            Some(PersistedRestoreError::AmbiguousFloatingManaProvenance {
                player: SEAT,
                source_id: star,
            }),
            "an unprovable pool is still refused, not guessed at"
        );
    }

    /// The honesty pin for this step, half two: a source the journal shows in
    /// TWO producing tenures (exiled, returned, then gone, per CR 400.7's
    /// new-object-per-move) no longer says which tenure made the mana, so
    /// nothing is stamped and the refusal stands.
    #[test]
    fn a_source_with_two_producing_tenures_is_left_unstamped() {
        let mut state = GameState::new_two_player(42);
        let star = source_on_battlefield(&mut state);
        add_pinless_red_unit(&mut state, star);
        crate::game::zones::move_to_zone(&mut state, star, Zone::Exile, &mut Vec::new());
        crate::game::zones::move_to_zone(&mut state, star, Zone::Battlefield, &mut Vec::new());
        crate::game::zones::move_to_zone(&mut state, star, Zone::Graveyard, &mut Vec::new());

        let json = capture_of(state);
        let mut migrated = json.clone();
        record_captured_floating_mana_provenance(&mut migrated);
        assert_eq!(
            migrated, json,
            "two producing tenures leave the epoch unproven"
        );
        assert_eq!(
            admitted(migrated).err(),
            Some(PersistedRestoreError::AmbiguousFloatingManaProvenance {
                player: SEAT,
                source_id: star,
            })
        );
    }

    /// The over-rejection guard, matching the production rule's own: a pin-less
    /// unit whose source is still a battlefield permanent needs no epoch from the
    /// MIGRATION — the capture already proves it — so the migration leaves the
    /// payload byte-identical. Admission then BINDS the unit to the live object's
    /// own epoch (`GameState::bind_or_reject_floating_mana_provenance`), because a
    /// unit left pin-less would re-derive its source from whatever holds the
    /// storage id later (CR 400.7).
    #[test]
    fn a_unit_whose_source_is_still_a_battlefield_permanent_is_left_untouched() {
        let mut state = GameState::new_two_player(42);
        let star = source_on_battlefield(&mut state);
        add_pinless_red_unit(&mut state, star);
        let live_incarnation = state.objects[&star].incarnation;

        let json = capture_of(state);
        let mut migrated = json.clone();
        record_captured_floating_mana_provenance(&mut migrated);
        assert_eq!(
            migrated, json,
            "an admitted pool must be left byte-identical"
        );
        assert_eq!(
            restored_pin(migrated),
            Some(Some(live_incarnation)),
            "CR 106.3: admission binds the epoch the capture proved — the live \
             object's own — rather than leaving the unit to re-derive its source"
        );
    }
}
