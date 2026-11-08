//! TEST-ONLY migrations for committed capture fixtures that predate a
//! persisted-provenance field the restore boundary now requires.
//!
//! This module is the same kind of concession as
//! [`crate::types::game_state::record_captured_onsets`], which declares an old
//! capture's CR 611.2e onset record and its CR 601.2c cast-history provenance.
//! The third field in that family is the announced-target latch a parked
//! [`PendingCast`] owes (`GameState::reject_unknown_pending_target_facts`):
//! CR 115.1 + CR 601.2c facts, captured at the target-declaration → payment
//! boundary and consulted by every carrier that resumes a cast.
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
//! `game_state.rs::prepare_for_restore_checks_the_activation_mill_carrier`).
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

use crate::types::game_state::{ActivationTargetFact, GameState, PendingCast, PendingCastOwner};
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

/// The whole committed-capture provenance migration, in the order restore
/// reads it: the CR 611.2e onset record plus the CR 601.2c cast-history
/// provenance first (so the root decodes), then the announced-target latch this
/// module owns. Both halves stay test-only; see the module docs.
pub fn record_captured_provenance(document: &mut Value) {
    crate::types::game_state::record_captured_onsets(document);
    record_captured_target_facts(document);
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
    use crate::types::mana::ManaCost;
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
}
