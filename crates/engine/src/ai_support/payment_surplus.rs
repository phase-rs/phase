//! Payment-surplus certificate for AI mana activations.
//!
//! CR 601.2g–h: while a spell is being paid for, its caster may activate mana
//! abilities before paying the total cost. CR 605.3a + CR 117.3c: a mana ability
//! may be activated whenever its controller has priority, and the caster of an
//! ordinary cast receives priority once the spell becomes cast. So a mana
//! activation the current payment does not need can be deferred — unless taking
//! it now changes something the cast observes.
//!
//! [`surplus_mana_activation`] certifies that an activation offered at a spell's
//! `ManaPayment` step is such a surplus: passing already finalizes the payment,
//! the same activation stays available at the caster's next priority, it queues
//! no non-mana trigger, the one unit of mana it adds can't change what the
//! finished cast records (CR 601.2h payment stamps: amount, colors, source
//! characteristics) under any spend the caster could direct, and the finished
//! cast triggers the same abilities either way (CR 601.2i). Every step is
//! answered by the engine's own reducer and payment authorities; a step it can't
//! certify refuses with its reason, so consumers fail closed.
//!
//! Work: the decision's half ([`PaymentSurplusDecision::prepare`]) is one
//! direct reducer application (the finisher) plus one enumeration of the mana
//! actions the payer is offered afterwards; each activation adds at most two
//! direct applications, or one when it is seeded with the successor the root's
//! payment-continuation witness already applied. Those counts are direct
//! applications, not total work:
//! the enumeration runs a readiness simulation for every costed mana source the
//! payer controls, so a caller certifying every activation of one decision
//! prepares it once.

use std::borrow::Cow;

use crate::ai_support::activates_mana_ability;
use crate::ai_support::payment_continuation::{
    classify_payment_continuation, finalizes_in_one_step, PaymentContinuationRoot,
    PaymentContinuationState, WitnessedStepTrace,
};
use crate::game::casting::{
    activated_ability_definitions, cast_payment_mana_spend_permission, mana_spent_source_snapshot,
};
use crate::game::engine::apply_as_current_for_simulation;
use crate::game::mana_payment::{
    has_unspent_mana_continuous_effects, shard_to_mana_type, ShardRequirement,
};
use crate::game::mana_sources::activatable_mana_actions_for_player;
use crate::game::triggers::{
    simulated_action_would_queue_non_mana_trigger, DeferredTrigger, PendingTrigger,
};
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, StackEntryKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaCost, ManaPipId, ManaSupertype, ManaType, ManaUnit};
use crate::types::player::PlayerId;

/// Whether a mana activation at a spell's payment step is certified surplus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurplusManaActivation {
    /// Passing finalizes the payment now, and the activation can be deferred
    /// without changing anything the cast records.
    Surplus,
    NotCertified(SurplusNotCertified),
}

/// The first certificate step that could not be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurplusNotCertified {
    /// The decision is not a spell's `ManaPayment` step owned by the payment
    /// continuation oracle, or a [`PaymentSurplusDecision`] was prepared for
    /// another decision.
    Carrier,
    /// CR 602.2b: an activated ability's payment; its noted or source-qualified
    /// mana is out of the certificate's scope.
    AbilityRoot,
    /// The action activates no mana ability.
    NotManaActivation,
    /// CR 106.4: an effect on the board reads unspent mana, so a floating unit
    /// is not inert.
    UnspentManaRead,
    /// CR 609.4b: a spend-as-though concession lets the allocator stretch a
    /// unit across requirements the capacity count doesn't model.
    SpendPermission,
    /// Passing does not finalize the payment in one step.
    NoFinisher,
    /// CR 117.3c / CR 608.2g: after the pass the payer does not hold priority
    /// with the spell on the stack, so there is no window to defer into.
    NoPriorityWindow,
    /// After the pass, the same activation of the same source is not offered.
    DeferralUnavailable,
    /// CR 603.2: the activation queues a triggered ability that is
    /// not a mana ability — value of its own.
    QueuesTrigger,
    /// The activation does not return to the same payment step with exactly
    /// one new unit of mana (a color or cost choice, a multi-mana output).
    NotSingleUnitReturn,
    /// CR 106.6: the new unit carries spending restrictions or spell grants.
    UnitHasRiders,
    /// The pool doesn't already hold as many spend-equivalent units as the
    /// locked cost can absorb, so a directed spend could use the new unit.
    Unsaturated,
    /// The default finisher, after the activation, fails or records different
    /// payment stamps than it does without it.
    FinisherDiverges,
    /// CR 601.2i: the two finishers leave different abilities triggered by
    /// the cast — the activation changed what the spell is as it becomes cast.
    CastTriggersDiverge,
}

/// Reducer work spent by the certificate, for bound tests.
#[cfg(feature = "test-support")]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SurplusCertificateCounters {
    /// Direct reducer applications, each on a scratch copy of the state.
    pub applies: usize,
    /// Enumerations of the payer's offered mana actions after the finisher.
    /// Each one runs the readiness simulations of every costed mana source the
    /// payer controls, so this — not `applies` — is what scales with the board.
    pub offer_enumerations: usize,
}

/// One unit of the certificate's reducer work.
#[derive(Debug, Clone, Copy)]
enum Work {
    Apply,
    OfferEnumeration,
}

#[cfg(feature = "test-support")]
impl SurplusCertificateCounters {
    fn record(&mut self, work: Work) {
        match work {
            Work::Apply => self.applies += 1,
            Work::OfferEnumeration => self.offer_enumerations += 1,
        }
    }
}

/// Certify whether `activation`, offered at the current spell payment step, is
/// a surplus mana activation (see the module docs). A one-shot
/// [`PaymentSurplusDecision::prepare`] + [`PaymentSurplusDecision::certify`]
/// that skips the preparation when `activation` is not a mana activation. A
/// caller certifying several activations of one decision prepares once instead.
pub fn surplus_mana_activation(
    state: &GameState,
    activation: &GameAction,
) -> SurplusManaActivation {
    verdict(one_shot(state, activation, &mut |_| {}))
}

#[cfg(feature = "test-support")]
pub fn surplus_mana_activation_with_counters(
    state: &GameState,
    activation: &GameAction,
    counters: &mut SurplusCertificateCounters,
) -> SurplusManaActivation {
    verdict(one_shot(state, activation, &mut |work| {
        counters.record(work)
    }))
}

fn verdict(certificate: Result<(), SurplusNotCertified>) -> SurplusManaActivation {
    match certificate {
        Ok(()) => SurplusManaActivation::Surplus,
        Err(reason) => SurplusManaActivation::NotCertified(reason),
    }
}

fn one_shot(
    state: &GameState,
    activation: &GameAction,
    on_work: &mut dyn FnMut(Work),
) -> Result<(), SurplusNotCertified> {
    let (spell_id, payer) = carrier(state)?;
    mana_activation(state, activation)?;
    let prepared = prepare(state, spell_id, payer, on_work)?;
    certify_prepared(state, &prepared, activation, None, on_work)
}

/// The decision-level half of the certificate: the carrier, the finisher's
/// state without any further activation (S0), and the mana actions the payer
/// is offered in S0. It depends only on the payment decision, so a caller
/// certifying every activation offered at that decision prepares it once —
/// one reducer application and one offered-mana-action enumeration — and each
/// [`Self::certify`] adds at most two direct applications: one when the
/// activation was seeded already applied ([`Self::prepare_seeded`]).
#[derive(Debug, Clone)]
pub struct PaymentSurplusDecision {
    binding: DecisionBinding,
    prepared: Result<PreparedPayment, SurplusNotCertified>,
    /// Activations already applied to a copy of the decision's state.
    applied: Vec<(GameAction, AppliedActivation)>,
}

/// A mana activation already applied to a copy of a payment decision's state,
/// with that reducer step's trace, so [`PaymentSurplusDecision::certify`] reads
/// it instead of applying the activation again. The root's payment-continuation
/// witness keeps one for every candidate it accepts
/// (`AcceptedPaymentSuccessor`).
#[derive(Debug, Clone)]
pub struct AppliedActivation {
    pub state: GameState,
    pub trace: WitnessedStepTrace,
}

/// Already-applied activations for one payment decision, bound to that
/// decision the same way a preparation is: [`PaymentSurplusDecision::prepare_seeded`]
/// attaches them only to a preparation of the same decision.
#[derive(Debug, Clone)]
pub struct PaymentSurplusSeeds {
    binding: DecisionBinding,
    applied: Vec<(GameAction, AppliedActivation)>,
}

impl PaymentSurplusSeeds {
    /// `applied` pairs each action with the result of applying it, through
    /// the simulation reducer, to a copy of `state`.
    pub fn new(state: &GameState, applied: Vec<(GameAction, AppliedActivation)>) -> Self {
        Self {
            binding: DecisionBinding::capture(state),
            applied,
        }
    }
}

/// The finisher-side facts every activation of one decision is certified against.
#[derive(Debug, Clone)]
struct PreparedPayment {
    spell_id: ObjectId,
    payer: PlayerId,
    /// S0: the state after the pass that finalizes the payment.
    without: GameState,
    /// The mana actions the payer is offered in S0.
    offered: Vec<GameAction>,
}

/// The decision a preparation was made for: the reducer revision and the
/// interaction it answers, the payment step, the spell being paid for, and
/// every pool's units.
#[derive(Debug, Clone, PartialEq)]
struct DecisionBinding {
    state_revision: u64,
    interaction_generation: u64,
    waiting_for: WaitingFor,
    pending_spell: Option<ObjectId>,
    pools: Vec<Vec<ManaPipId>>,
}

impl DecisionBinding {
    fn capture(state: &GameState) -> Self {
        Self {
            state_revision: state.state_revision,
            interaction_generation: state.interaction_generation,
            waiting_for: state.waiting_for.clone(),
            pending_spell: state.pending_cast.as_ref().map(|pending| pending.object_id),
            pools: state
                .players
                .iter()
                .map(|player| player.mana_pool.units().map(|unit| unit.pip_id).collect())
                .collect(),
        }
    }
}

impl PaymentSurplusDecision {
    /// Prepare `state`'s payment decision (see the type docs).
    pub fn prepare(state: &GameState) -> Self {
        Self::prepare_counting(state, None, &mut |_| {})
    }

    /// [`Self::prepare`], keeping `seeds` when they were made for the same
    /// decision; otherwise they are dropped and every activation is applied.
    pub fn prepare_seeded(state: &GameState, seeds: PaymentSurplusSeeds) -> Self {
        Self::prepare_counting(state, Some(seeds), &mut |_| {})
    }

    #[cfg(feature = "test-support")]
    pub fn prepare_with_counters(
        state: &GameState,
        seeds: Option<PaymentSurplusSeeds>,
        counters: &mut SurplusCertificateCounters,
    ) -> Self {
        Self::prepare_counting(state, seeds, &mut |work| counters.record(work))
    }

    fn prepare_counting(
        state: &GameState,
        seeds: Option<PaymentSurplusSeeds>,
        on_work: &mut dyn FnMut(Work),
    ) -> Self {
        let binding = DecisionBinding::capture(state);
        let applied = seeds
            .filter(|seeds| seeds.binding == binding)
            .map(|seeds| seeds.applied)
            .unwrap_or_default();
        Self {
            binding,
            prepared: carrier(state)
                .and_then(|(spell_id, payer)| prepare(state, spell_id, payer, on_work)),
            applied,
        }
    }

    /// Whether this preparation was made for `state`'s decision.
    pub fn prepared_for(&self, state: &GameState) -> bool {
        self.binding == DecisionBinding::capture(state)
    }

    /// Certify `activation`, offered at the prepared decision. A `state` the
    /// preparation wasn't made for ([`Self::prepared_for`]) certifies nothing.
    pub fn certify(&self, state: &GameState, activation: &GameAction) -> SurplusManaActivation {
        verdict(self.certify_counting(state, activation, &mut |_| {}))
    }

    #[cfg(feature = "test-support")]
    pub fn certify_with_counters(
        &self,
        state: &GameState,
        activation: &GameAction,
        counters: &mut SurplusCertificateCounters,
    ) -> SurplusManaActivation {
        verdict(self.certify_counting(state, activation, &mut |work| counters.record(work)))
    }

    fn certify_counting(
        &self,
        state: &GameState,
        activation: &GameAction,
        on_work: &mut dyn FnMut(Work),
    ) -> Result<(), SurplusNotCertified> {
        use SurplusNotCertified as Refused;
        if !self.prepared_for(state) {
            return Err(Refused::Carrier);
        }
        // The one-shot's refusal order: the carrier, then the action, then
        // the decision's remaining steps.
        if let Err(reason @ (Refused::Carrier | Refused::AbilityRoot)) = &self.prepared {
            return Err(*reason);
        }
        mana_activation(state, activation)?;
        let prepared = self.prepared.as_ref().map_err(|reason| *reason)?;
        let applied = self
            .applied
            .iter()
            .find_map(|(action, applied)| (action == activation).then_some(applied));
        certify_prepared(state, prepared, activation, applied, on_work)
    }
}

/// CR 601.2g: the spell's mana-payment step, with its root owned by the
/// payment continuation oracle.
fn carrier(state: &GameState) -> Result<(ObjectId, PlayerId), SurplusNotCertified> {
    use SurplusNotCertified as Refused;
    if !matches!(state.waiting_for, WaitingFor::ManaPayment { .. }) {
        return Err(Refused::Carrier);
    }
    match classify_payment_continuation(state) {
        PaymentContinuationState::Affiliated(PaymentContinuationRoot::Spell {
            object_id,
            payer,
            ..
        }) => Ok((object_id, payer)),
        PaymentContinuationState::Affiliated(PaymentContinuationRoot::Activation { .. }) => {
            Err(Refused::AbilityRoot)
        }
        PaymentContinuationState::NotAffiliated
        | PaymentContinuationState::UnsupportedAffiliated(_) => Err(Refused::Carrier),
    }
}

/// CR 605.1a + CR 605.3a: `activation` activates a mana ability.
fn mana_activation(state: &GameState, activation: &GameAction) -> Result<(), SurplusNotCertified> {
    if activation_source(activation).is_some() && activates_mana_ability(state, activation) {
        Ok(())
    } else {
        Err(SurplusNotCertified::NotManaActivation)
    }
}

/// The decision's steps: nothing reads unspent mana, no spend permission, the
/// pass finalizes, and the payer then holds priority. One reducer application
/// and one offered-mana-action enumeration.
fn prepare(
    state: &GameState,
    spell_id: ObjectId,
    payer: PlayerId,
    on_work: &mut dyn FnMut(Work),
) -> Result<PreparedPayment, SurplusNotCertified> {
    use SurplusNotCertified as Refused;
    // CR 106.4: floating mana is inert only when nothing reads it.
    if has_unspent_mana_continuous_effects(state) {
        return Err(Refused::UnspentManaRead);
    }
    // CR 609.4b: the concession the finalizer's allocator pays under.
    if cast_payment_mana_spend_permission(state, payer, spell_id).is_some() {
        return Err(Refused::SpendPermission);
    }

    // CR 601.2h–i: passing pays the total cost and finalizes the spell now.
    on_work(Work::Apply);
    let without =
        finalizes_in_one_step(state, &GameAction::PassPriority).ok_or(Refused::NoFinisher)?;
    // CR 117.3c + CR 608.2g: the caster holds priority with the spell on the
    // stack — the window a deferred mana activation would use (CR 605.3a).
    if without.waiting_for != (WaitingFor::Priority { player: payer })
        || !without.stack.iter().any(|entry| entry.id == spell_id)
    {
        return Err(Refused::NoPriorityWindow);
    }
    on_work(Work::OfferEnumeration);
    let offered = activatable_mana_actions_for_player(&without, payer);
    Ok(PreparedPayment {
        spell_id,
        payer,
        without,
        offered,
    })
}

/// The activation's steps against a prepared decision: deferral, the
/// activation itself (read from `applied` when it was already applied),
/// saturation, and the finisher after it. At most two reducer applications.
fn certify_prepared(
    state: &GameState,
    prepared: &PreparedPayment,
    activation: &GameAction,
    applied: Option<&AppliedActivation>,
    on_work: &mut dyn FnMut(Work),
) -> Result<(), SurplusNotCertified> {
    use SurplusNotCertified as Refused;
    let PreparedPayment {
        spell_id, payer, ..
    } = *prepared;
    if !deferral_available(state, prepared, activation) {
        return Err(Refused::DeferralUnavailable);
    }

    // The activation itself. A seeded successor is cloned, because the
    // trigger check below collects into the state it reads.
    let (mut activated, trace) = match applied {
        Some(applied) => (applied.state.clone(), Cow::Borrowed(&applied.trace)),
        None => {
            on_work(Work::Apply);
            let mut activated = state.clone();
            let deferred_triggers_before = activated.deferred_triggers.len();
            let result = apply_as_current_for_simulation(&mut activated, activation.clone())
                .map_err(|_| Refused::NotSingleUnitReturn)?;
            (
                activated,
                Cow::Owned(WitnessedStepTrace {
                    events: result.events,
                    deferred_triggers_before,
                }),
            )
        }
    };
    // CR 603.2: a non-mana trigger of the activation is its own value.
    if simulated_action_would_queue_non_mana_trigger(
        &mut activated,
        trace.deferred_triggers_before,
        &trace.events,
    ) {
        return Err(Refused::QueuesTrigger);
    }
    if !matches!(activated.waiting_for, WaitingFor::ManaPayment { .. })
        || classify_payment_continuation(&activated) != classify_payment_continuation(state)
    {
        return Err(Refused::NotSingleUnitReturn);
    }
    let new_unit = single_new_unit(state, &activated, payer).ok_or(Refused::NotSingleUnitReturn)?;
    // CR 106.6: spending restrictions and spell grants ride the unit.
    if !new_unit.restrictions.is_empty() || !new_unit.grants.is_empty() {
        return Err(Refused::UnitHasRiders);
    }
    if !saturated(&activated, payer, spell_id, &new_unit) {
        return Err(Refused::Unsaturated);
    }

    // The default finisher after the activation pays the same way.
    on_work(Work::Apply);
    let with = finalizes_in_one_step(&activated, &GameAction::PassPriority)
        .ok_or(Refused::FinisherDiverges)?;
    match (
        prepared.without.objects.get(&spell_id),
        with.objects.get(&spell_id),
    ) {
        (Some(without_spell), Some(with_spell))
            if without_spell.cast_payment_stamps_equivalent(with_spell) => {}
        _ => return Err(Refused::FinisherDiverges),
    }
    // CR 601.2i: abilities that trigger on the cast trigger as it finishes,
    // reading the spell as it is then (CR 604.3: a characteristic-defining
    // ability such as Scourge of the Skyclaves's can read the life the
    // activation paid). Deferring the activation can't recreate those, so both
    // finishers must leave the same ones.
    if CastTimeTriggers::observe(state, &prepared.without, spell_id)
        != CastTimeTriggers::observe(state, &with, spell_id)
    {
        return Err(Refused::CastTriggersDiverge);
    }
    Ok(())
}

/// What a finisher left of the abilities that triggered as the spell became
/// cast, read from the reducer's own output: the entries it put on the stack
/// above `decision`'s (identity, controller and resolved ability with its
/// trigger event, but not the freshly allocated stack id), the triggers still
/// deferred or pending, and the decision it then waits on.
#[derive(PartialEq)]
struct CastTimeTriggers<'a> {
    stacked: Vec<(ObjectId, PlayerId, &'a StackEntryKind)>,
    deferred: &'a [DeferredTrigger],
    pending: Option<&'a PendingTrigger>,
    waiting_for: &'a WaitingFor,
}

impl<'a> CastTimeTriggers<'a> {
    fn observe(decision: &GameState, finished: &'a GameState, spell_id: ObjectId) -> Self {
        let before: Vec<ObjectId> = decision.stack.iter().map(|entry| entry.id).collect();
        Self {
            stacked: finished
                .stack
                .iter()
                .filter(|entry| entry.id != spell_id && !before.contains(&entry.id))
                .map(|entry| (entry.source_id, entry.controller, &entry.kind))
                .collect(),
            deferred: finished
                .deferred_triggers
                .get(decision.deferred_triggers.len()..)
                .unwrap_or_default(),
            pending: finished.pending_trigger.as_deref(),
            waiting_for: &finished.waiting_for,
        }
    }
}

/// The source object a mana action activates.
fn activation_source(action: &GameAction) -> Option<ObjectId> {
    match action {
        GameAction::ActivateAbility { source_id, .. } => Some(*source_id),
        GameAction::TapLandForMana { selection } | GameAction::ActivateManaSource { selection } => {
            Some(selection.source.object_id)
        }
        _ => None,
    }
}

/// CR 605.3a: after the finisher, the payer can still activate the same mana
/// ability of the same object (CR 400.7: same incarnation).
fn deferral_available(
    state: &GameState,
    prepared: &PreparedPayment,
    activation: &GameAction,
) -> bool {
    let Some(source) = activation_source(activation) else {
        return false;
    };
    let incarnation =
        |state: &GameState| state.objects.get(&source).map(|object| object.incarnation);
    incarnation(state).is_some()
        && incarnation(state) == incarnation(&prepared.without)
        && prepared
            .offered
            .iter()
            .any(|offered| same_activation(state, &prepared.without, activation, offered))
}

/// Whether `offered`, in `without`, activates the ability `activation` does in
/// `state`. An index alone doesn't name an ability across states: a granted
/// list is rebuilt in provider order (Manascape Refractor's lands), so when a
/// provider leaves, the same index names a different ability. The definition
/// at the index must be the same in both.
fn same_activation(
    state: &GameState,
    without: &GameState,
    activation: &GameAction,
    offered: &GameAction,
) -> bool {
    let same_definition = |source: ObjectId, index: usize| {
        let named = |state: &GameState| {
            activated_ability_definitions(state, source)
                .into_iter()
                .find_map(|(at, ability)| (at == index).then_some(ability))
        };
        let before = named(state);
        before.is_some() && before == named(without)
    };
    match (activation, offered) {
        (
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            },
            GameAction::ActivateAbility {
                source_id: offered_source,
                ability_index: offered_index,
                ..
            },
        ) => {
            source_id == offered_source
                && ability_index == offered_index
                && same_definition(*source_id, *ability_index)
        }
        (
            GameAction::TapLandForMana { selection },
            GameAction::TapLandForMana {
                selection: offered_selection,
            },
        )
        | (
            GameAction::ActivateManaSource { selection },
            GameAction::ActivateManaSource {
                selection: offered_selection,
            },
        ) => {
            selection == offered_selection
                && selection
                    .ability_index
                    .is_none_or(|index| same_definition(selection.source.object_id, index))
        }
        _ => false,
    }
}

/// The one unit `activated`'s pool holds that `state`'s pool didn't.
fn single_new_unit(state: &GameState, activated: &GameState, payer: PlayerId) -> Option<ManaUnit> {
    let before: Vec<ManaPipId> = state
        .players
        .iter()
        .find(|player| player.id == payer)?
        .mana_pool
        .units()
        .map(|unit| unit.pip_id)
        .collect();
    let mut added = activated
        .players
        .iter()
        .find(|player| player.id == payer)?
        .mana_pool
        .units()
        .filter(|unit| !before.contains(&unit.pip_id));
    let unit = added.next()?;
    added.next().is_none().then_some(unit)
}

/// CR 106.3 + CR 106.6 + CR 601.2h: Whether spending `a` or `b` records the
/// same thing on a spell — the same mana shape (type, snow, multicolor-source
/// marker, restrictions, grants, expiry) and the same source characteristics,
/// both read through the spend stamp's own snapshot authority in `state`. A unit
/// whose source has neither a live object nor last-known information is never
/// equivalent to anything.
pub(crate) fn spend_equivalent(state: &GameState, a: &ManaUnit, b: &ManaUnit) -> bool {
    let (mut a_shape, _) = a.clone().into_shape_and_pip();
    let (mut b_shape, _) = b.clone().into_shape_and_pip();
    a_shape.source_id = ObjectId(0);
    b_shape.source_id = ObjectId(0);
    if a_shape != b_shape {
        return false;
    }
    match (
        mana_spent_source_snapshot(state, a),
        mana_spent_source_snapshot(state, b),
    ) {
        (Some(a_snapshot), Some(b_snapshot)) => a_snapshot.lki == b_snapshot.lki,
        _ => false,
    }
}

/// [`spend_equivalent`], for printed-board reach guards.
#[cfg(feature = "test-support")]
pub fn units_spend_equivalent(state: &GameState, a: &ManaUnit, b: &ManaUnit) -> bool {
    spend_equivalent(state, a, b)
}

/// Saturation: `activated`'s pool already holds, besides `new_unit`, as many
/// units spend-equivalent to it as the locked cost can absorb. Then any spend
/// that uses `new_unit` has an equivalent spend without it, so no directed
/// payment can record anything different.
fn saturated(
    activated: &GameState,
    payer: PlayerId,
    spell_id: ObjectId,
    new_unit: &ManaUnit,
) -> bool {
    let Some(pending) = activated
        .pending_cast
        .as_deref()
        .filter(|pending| pending.object_id == spell_id)
    else {
        return false;
    };
    let Some(capacity) = absorption_capacity(&pending.cost, pending.ability.chosen_x, new_unit)
    else {
        return false;
    };
    let Some(player) = activated.players.iter().find(|player| player.id == payer) else {
        return false;
    };
    let equivalent = player
        .mana_pool
        .units()
        .filter(|unit| {
            unit.pip_id != new_unit.pip_id && spend_equivalent(activated, unit, new_unit)
        })
        .count();
    equivalent >= capacity
}

/// CR 107.4 + CR 601.2h: The most units shaped like `unit` that a payment of
/// `cost` can absorb — every generic pip, plus each shard `unit` can pay.
/// Over-counting only withholds the certificate. `None` for costs not paid
/// from a concrete shard list.
pub(crate) fn absorption_capacity(
    cost: &ManaCost,
    chosen_x: Option<u32>,
    unit: &ManaUnit,
) -> Option<usize> {
    let ManaCost::Cost { shards, generic } = cost else {
        return match cost {
            ManaCost::NoCost => Some(0),
            _ => None,
        };
    };
    let fits = |color: ManaType| unit.color == color;
    let mut capacity = *generic as usize;
    for shard in shards {
        capacity += match shard_to_mana_type(*shard) {
            ShardRequirement::Single(color) | ShardRequirement::Phyrexian(color) => {
                usize::from(fits(color))
            }
            ShardRequirement::Hybrid(a, b) | ShardRequirement::HybridPhyrexian(a, b) => {
                usize::from(fits(a) || fits(b))
            }
            ShardRequirement::ColorlessHybrid(color) => {
                usize::from(fits(color) || fits(ManaType::Colorless))
            }
            // CR 107.4e: a {2/B} symbol can be paid with two mana of any type.
            ShardRequirement::TwoGenericHybrid(_)
            | ShardRequirement::TwoGenericHybridPhyrexian(_) => 2,
            // CR 107.4h.
            ShardRequirement::Snow => {
                usize::from(matches!(unit.supertype, Some(ManaSupertype::Snow)))
            }
            ShardRequirement::TwoOrMoreColorSource => {
                usize::from(unit.source_could_produce_two_or_more_colors)
            }
            // CR 107.3: an unchosen X is unbounded for this count.
            ShardRequirement::X => chosen_x.map_or(usize::MAX / 2, |x| x as usize),
        };
    }
    Some(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0};
    use crate::types::ability::TargetFilter;
    use crate::types::mana::{ManaCostShard, ManaExpiry, ManaRestriction, ManaSpellGrant};

    fn unit(color: ManaType, source: ObjectId) -> ManaUnit {
        ManaUnit::new(color, source, false, Vec::new())
    }

    /// Two identical battlefield sources and their state.
    fn twin_sources() -> (GameState, ObjectId, ObjectId) {
        let mut sc = GameScenario::new();
        let a = sc.add_creature(P0, "Twin Source", 1, 1).id();
        let b = sc.add_creature(P0, "Twin Source", 1, 1).id();
        (sc.build().state().clone(), a, b)
    }

    /// CR 106.3 + CR 106.6: equivalence compares every `ManaShape` field and
    /// the source's characteristics, never the pip or the source id itself.
    #[test]
    fn spend_equivalent_compares_every_shape_field_and_the_source_snapshot() {
        let (state, a, b) = twin_sources();
        let base = unit(ManaType::Red, a);
        assert!(spend_equivalent(&state, &base, &unit(ManaType::Red, b)));
        let mut repipped = base.clone();
        repipped.pip_id = ManaPipId(77);
        assert!(spend_equivalent(&state, &base, &repipped));

        /// Changes one field of a unit's shape.
        type Flip = fn(&mut ManaUnit);
        let flips: Vec<(&str, Flip)> = vec![
            ("color", |unit| unit.color = ManaType::Green),
            ("supertype", |unit| {
                unit.supertype = Some(ManaSupertype::Snow)
            }),
            ("source_could_produce_two_or_more_colors", |unit| {
                unit.source_could_produce_two_or_more_colors = true
            }),
            ("restrictions", |unit| {
                unit.restrictions.push(ManaRestriction::Impossible)
            }),
            ("grants", |unit| {
                unit.grants.push(ManaSpellGrant::CantBeCountered {
                    filter: TargetFilter::Any,
                })
            }),
            ("expiry", |unit| unit.expiry = Some(ManaExpiry::EndOfTurn)),
        ];
        for (field, flip) in flips {
            let mut changed = unit(ManaType::Red, b);
            flip(&mut changed);
            assert!(
                !spend_equivalent(&state, &base, &changed),
                "{field} alone must break equivalence"
            );
        }

        let mut other = state.clone();
        other.objects.get_mut(&b).unwrap().name = "Other Source".to_string();
        assert!(
            !spend_equivalent(&other, &base, &unit(ManaType::Red, b)),
            "different source characteristics are not equivalent"
        );
        let missing = unit(ManaType::Red, ObjectId(9_999));
        assert!(
            !spend_equivalent(&state, &missing, &missing),
            "a unit with no live source and no last-known information is never equivalent"
        );
    }

    /// CR 107.4 + CR 601.2h: absorption per shard requirement.
    #[test]
    fn absorption_capacity_counts_each_shard_the_unit_can_pay() {
        let (_, a, _) = twin_sources();
        let red = unit(ManaType::Red, a);
        let colorless = unit(ManaType::Colorless, a);
        let cost = |shards: Vec<ManaCostShard>, generic: u32| ManaCost::Cost { shards, generic };
        let rows: [(&str, ManaCost, &ManaUnit, Option<usize>); 13] = [
            ("generic", cost(vec![], 3), &red, Some(3)),
            ("{R}", cost(vec![ManaCostShard::Red], 0), &red, Some(1)),
            ("{U}", cost(vec![ManaCostShard::Blue], 0), &red, Some(0)),
            (
                "{R/G}",
                cost(vec![ManaCostShard::RedGreen], 0),
                &red,
                Some(1),
            ),
            ("{2/R}", cost(vec![ManaCostShard::TwoRed], 0), &red, Some(2)),
            (
                "{2/U}",
                cost(vec![ManaCostShard::TwoBlue], 0),
                &red,
                Some(2),
            ),
            (
                "{R/P}",
                cost(vec![ManaCostShard::PhyrexianRed], 0),
                &red,
                Some(1),
            ),
            (
                "{C/R}",
                cost(vec![ManaCostShard::ColorlessRed], 0),
                &colorless,
                Some(1),
            ),
            (
                "{C}",
                cost(vec![ManaCostShard::Colorless], 0),
                &red,
                Some(0),
            ),
            ("{S}", cost(vec![ManaCostShard::Snow], 0), &red, Some(0)),
            ("{X} chosen", cost(vec![ManaCostShard::X], 0), &red, Some(0)),
            ("no cost", ManaCost::NoCost, &red, Some(0)),
            ("self mana cost", ManaCost::SelfManaCost, &red, None),
        ];
        for (label, cost, unit, expected) in rows {
            let chosen_x = matches!(label, "{X} chosen").then_some(0);
            assert_eq!(
                absorption_capacity(&cost, chosen_x, unit),
                expected,
                "{label}"
            );
        }
        assert!(
            absorption_capacity(&cost(vec![ManaCostShard::X], 0), None, &red)
                .is_some_and(|capacity| capacity > 1_000),
            "an unchosen X is never saturated"
        );
        let mut snow = red.clone();
        snow.supertype = Some(ManaSupertype::Snow);
        assert_eq!(
            absorption_capacity(&cost(vec![ManaCostShard::Snow], 0), None, &snow),
            Some(1)
        );
    }
}
