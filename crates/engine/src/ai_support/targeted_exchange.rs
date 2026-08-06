//! Bounded, reducer-backed preview for a fully-targeted self-destructive exchange.
//!
//! This intentionally certifies only the narrow class whose complete target
//! declaration is available before mana payment.  It is an engine authority:
//! the AI supplies an issued candidate, while this module preserves interaction
//! ownership, replays the reducer, and reads the fully-bound pending ability.

use crate::ai_support::{validated_candidate_actions_for_semantic_owner, CandidateAction};
use crate::game::effects::resolve_ability_chain;
use crate::game::engine::apply_interaction_for_simulation;
use crate::game::layers::flush_layers;
use crate::game::sba::check_state_based_actions;
use crate::types::ability::{DamageSource, Effect, ResolvedAbility, TargetFilter, TargetRef};
use crate::types::ability_visit::visit_ability_def;
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{GameState, PendingCast, StackEntryKind, WaitingFor};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::ops::ControlFlow;

/// Root-cast tactical result. `Indeterminate` deliberately leaves the root
/// candidate available; the preview is a safety veto, not a second rules engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetedExchangeVerdict {
    Reject,
    Allow,
    Indeterminate,
}

const MAX_WITNESS_NODES: usize = 64;
const MAX_WITNESS_BRANCHES: usize = 16;

#[derive(Debug, Clone, Copy)]
enum RootBinding {
    Cast {
        object_id: ObjectId,
    },
    Activation {
        source_id: ObjectId,
        ability_index: usize,
    },
}

impl RootBinding {
    fn from_action(action: &GameAction) -> Option<Self> {
        match action {
            GameAction::CastSpell { object_id, .. } => Some(Self::Cast {
                object_id: *object_id,
            }),
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => Some(Self::Activation {
                source_id: *source_id,
                ability_index: *ability_index,
            }),
            _ => None,
        }
    }

    /// The object whose ability trees the announced spell or activated ability
    /// will bind. CR 601.2a: an announced spell "has all the characteristics of
    /// the card"; CR 602.2b: an activated ability's announcement is identical, so
    /// the same object supplies both.
    const fn source_object_id(self) -> ObjectId {
        match self {
            Self::Cast { object_id } => object_id,
            Self::Activation { source_id, .. } => source_id,
        }
    }

    fn matches_pending(self, pending: &PendingCast) -> bool {
        match self {
            // CR 601.2a vs CR 602.2b: a cast root must not authenticate against an
            // ACTIVATION-sourced pending for the same object. `PendingCast` is built
            // for activations too (the `PendingCast::new` sites in `casting`,
            // `engine_modes`, and `planeswalker`), and the `Activation` arm already checks
            // both fields; this arm checked only the object, so a cast root could be
            // judged against an activated ability's spine — a different tree, and one
            // that may carry runtime-synthesized content clause (b2)(i) rails the
            // guard against. This suppresses no legitimate `Reject`: any that fired
            // through the untightened arm was computed from an activated ability's
            // spine under a cast binding — the wrong tree by construction. Nor is
            // the effect merely fail-open: on a non-match `bound_root_ability` falls
            // through to the stack scan below it and can still bind the ANNOUNCED
            // spell entry (`StackEntryKind::Spell { ability: Some(..) }`), which is
            // the correct authority for a cast root.
            Self::Cast { object_id } => {
                pending.object_id == object_id && pending.activation_ability_index.is_none()
            }
            Self::Activation {
                source_id,
                ability_index,
            } => {
                pending.object_id == source_id
                    && pending.activation_ability_index == Some(ability_index)
            }
        }
    }
}

/// Clone-free precondition for [`targeted_exchange_verdict`]. `false` PROVES the
/// verdict cannot be [`TargetedExchangeVerdict::Reject`], so the caller can skip
/// both the candidate enumeration and the bounded reducer replay.
///
/// `Reject` is returned from exactly two sites — `preview_target_sourced_self_damage`
/// and `preview_fight_exchange` — and both are reached only through
/// `preview_bound_exchange`, which first requires `is_target_sourced_self_damage`
/// or `find_fight_leaf` to match the BOUND ability. A root whose source carries
/// neither shape anywhere in the ability lists below therefore cannot be rejected
/// — SO LONG AS the bound ability is composed only of those stored lists. It is
/// not, in general: the binder composes STORED ⊕ SYNTHESIZED, and clause (b2)
/// below enumerates the three seams that build a definition instead of storing
/// one, with the disposition of each. That clause is part of this contract, not a
/// caveat to it; read it before treating a `false` here as a proof.
///
/// Completeness comes from [`crate::types::ability_visit::visit_ability_def`],
/// the engine's single wildcard-free `AbilityDefinition`/`Effect` traversal: it
/// reaches every nested carrier, including `Effect::ChooseOneOf` branches and
/// `AbilityCost::EffectCost` under `cost`/`unless_pay`. This is deliberately an
/// OVER-approximation of the two bound tests, which walk only the `.effect` +
/// `.sub_ability` spine. Looser is the safety property; tighter would silently
/// shrink the rejection set.
///
/// FALSIFIER (a SEAM CLASS, not one function): a new site that writes
/// `obj.abilities` or `obj.base_abilities` on LIVE state — by assignment OR via
/// `Arc::make_mut(&mut obj.abilities).push/extend(..)`, which is the idiom the
/// layer system itself uses (see the note at `game/layers.rs:2028`) — where the
/// installed content is NOT already inside the four fields entered below, is NOT
/// a shrink (`clear()` / empty `Vec`), is NOT a write to a local rather than a
/// `GameObject`, is NOT a layer write railed by the `layers_dirty` gate above,
/// and does NOT target a freshly created object (token/emblem). Also falsified by a
/// new production site that constructs one of the two adverse shapes outside
/// Oracle lowering. **AND** falsified by (c) below. **AND** falsified by (b2).
///
/// (b2) THE BASIS MISMATCH — this is the load-bearing correction to the sentence
/// above, and it applies to BOTH branches, not just activations. The guard's basis
/// is the object's STORED ability lists; the binder composes STORED ⊕ SYNTHESIZED.
/// The falsifier paragraph audits INSTALLATION, so a definition that is built
/// rather than stored reaches the bind unseen by it AND by its re-audit grep,
/// which matches only assignment and `&mut` borrow of an ability field. Three
/// confirmed seams, each disposed differently and deliberately:
///
/// (i) SYNTHESIZED ON READ, DATA-DRIVEN (Activation) —
/// `casting::activated_ability_definitions` (casting.rs:458) hands the AI indices
/// `printed_len + offset` whose definitions are rebuilt per call from
/// `effective_off_zone_keywords` + `database::synthesis` and stored in no field.
/// RAILED at runtime by the `RootBinding::Activation` index gate below. A rail is
/// required here rather than a note, because the payload is card-data-driven
/// across `database/synthesis.rs` (~26k lines): it cannot be discharged by reading
/// one function, so the gate must hold regardless of what those families come to
/// synthesize.
///
/// (ii) SYNTHESIZED FROM A SCALAR, FIXED PAYLOAD (Cast) —
/// the Awaken branch of `casting::prepare_spell_cast_with_variant_override_inner`
/// appends `awaken::build_awaken_rider(count: u32)` to the bound spine. NOT railed,
/// for two independent reasons, and BOTH are needed: (1) unreachable today —
/// Awaken is elected by a separate `AlternativeCastDecision` GameAction
/// (`handle_awaken_cost_choice_with_payment_mode`, casting.rs:8933), not inline
/// during the root `CastSpell`, and `explore_target_children` follows only
/// `GameAction::ChooseTarget { target: Some(_) }`; (2) the payload is CLOSED — a
/// fixed `PutCounter` → `Animate` built by one 20-line function that can be read
/// in full. Note what is deliberately NOT the argument: cost. A Cast-branch rail
/// could discriminate on `Keyword::Awaken` rather than falling open on every
/// cast, so it would be near-free, and "a rail is expensive" would be FALSE here.
/// The reason is epistemic, not economic — reading `build_awaken_rider` in full
/// discharges the question, which is precisely what cannot be done for (i).
/// Discharged by a back-reference AT that function — the site where the payload
/// would change — which is the direction (c) below records this comment as
/// failing to provide.
///
/// READ (ii) AND (iii) TOGETHER: both go live under the SAME widening of
/// `explore_target_children` past target selection, for DIFFERENT reasons — (ii)
/// because its election is a separate GameAction, (iii) because its pending parks
/// in `WaitingFor::SpliceOffer`. Anyone widening that exploration must rail BOTH.
/// (ii) additionally needs a rail only if its payload stops being fixed; (iii)
/// needs one unconditionally, because its payload is arbitrary card text.
///
/// (iii) MERGED FROM ANOTHER OBJECT (Cast) — `splice::append_to_sub_chain`
/// (splice.rs:145) mutates the bound `PendingCast.ability` with
/// `combined_spell_ability_def` read off a DIFFERENT object (the splice card in
/// hand), whose payload is arbitrary card text and so COULD carry an adverse
/// shape. Unreachable today: `target_selection_owner` returns `None` for
/// `WaitingFor::SpliceOffer` and `explore_target_children` follows only
/// `GameAction::ChooseTarget`. This one becomes live — and needs a rail, not a
/// note — the moment that exploration widens past target selection.
///
/// The fourth append site in that same function is NOT a seam: its Fuse branch
/// merges `obj.back_face.abilities`, which the `back_face`
/// arm below already chains, so it composes STORED content and is covered. It is
/// listed here because an audit of the append sites will find it and needs the
/// verdict, not because it falsifies anything.
///
/// So the falsifier is: a new site that makes a definition REACHABLE TO THE BIND
/// without writing it to a `GameObject` field — synthesized, merged, or displaced
/// (`GameObject::cleave_form`, a `CleaveFormState`, holds the displaced printed
/// list; `GameObject::specialize_faces` installs faces through the same
/// `printed_cards::apply_back_face_to_object` the `back_face` arm exists to cover,
/// and `specialize::specialize_permanent` gates it on the battlefield so it
/// resolves after this window).
/// AUDIT INSTRUMENT — deliberately NOT "what do
/// `combined_spell_ability_def` / `activation_ability_definition` RETURN". That
/// question cannot find (ii): the `combined_spell_ability_def` call sits near the
/// TOP of `prepare_spell_cast_with_variant_override_inner` and the
/// `awaken::append_awaken_rider` call ~950 lines LATER in that same function,
/// after that return has already happened. A return-site instrument
/// is blind to every post-return append by construction — the same shape of
/// error as the sticker grep dissected above, which is why it is called out here
/// rather than left as a footnote.
/// Ask instead: WHAT DOES THE ABILITY LOOK LIKE AT THE POINT `PendingCast` IS
/// CONSTRUCTED? That is the def the judges actually walk, it is downstream of
/// every append, and it is position-independent. Concretely: read
/// `prepare_spell_cast_with_variant_override_inner` from its
/// `combined_spell_ability_def` call to its `PreparedSpellCast` construction and
/// account for every mutation of `ability_def` on the way. At the time of writing
/// that is eight sites — the initial read, one rebind, the Overload transform
/// (`overload::transform_effect_in_place` DROPS `damage_source` on its
/// `DealDamage` -> `DamageAll` arm, so it can only remove the shape),
/// the Awaken append (ii), and the four-line Fuse merge (stored, covered).
/// PAYLOAD-DISMISSAL RULE — (i) and (ii) sit on OPPOSITE sides of this, so it is
/// stated as a criterion rather than left for the reader to infer; without it the
/// two clauses read as contradictory and a maintainer will pick whichever suits.
/// Dismissing a seam on the grounds that its payload carries no adverse shape is
/// ADMISSIBLE only when that payload is CLOSED — bounded by reading one
/// non-data-driven function in full, as in (ii) (`build_awaken_rider`, 20 lines,
/// fixed shape). It is NOT admissible over a data-driven surface: "no synthesized
/// ability carries Fight today" across `database/synthesis.rs` (~26k lines) is a
/// grep-level negative, not a closure over helper composition — which is exactly
/// why (i) gets a runtime rail and (ii) does not. This guard's contract is that
/// `false` PROVES non-rejection, and a grep is not a proof.
///
/// Note what is deliberately NOT on that list: "an installer the cast path
/// provably never calls." That clause was carried for three rounds with an
/// instrument — a grep for LEAF installer names over three files — that could
/// not have detected its own falsity: `stickers.rs:490` is reached through a
/// WRAPPER (`zones.rs:511 rebuild_public_zone_stickers`) named in a file the
/// grep already covered, so the grep returned zero while the route existed.
/// (The route is in fact dead on a cast — `zones.rs:511` is inside
/// `if from == Zone::Battlefield {` at `zones.rs:482` — but an instrument that
/// is right by luck is not an instrument.) Dispose of a new site by reading ONE
/// function: what does it write, and where does the content come from. If a
/// negative reach claim is genuinely needed, discharge it with a BOUNDED
/// TRANSITIVE CALLER CLOSURE — enumerate every caller of the installer by exact
/// name, then every caller of each wrapper, until each terminating site's gate
/// has been read in source, quoting the ENCLOSING CONDITIONAL of every line.
///
/// (c) CR 123.5 DEPENDENCY — recorded, not hypothetical. This note is
/// ONE-DIRECTIONAL: it is discoverable from the guard's side only, and
/// nothing in `game/zones.rs`, where such a fix would be made, points back
/// to it. CR 123.5
/// says stickers "are retained as that object moves to a public zone and
/// continue to apply to the new object it becomes in that zone." The engine
/// implements only half of that: `zones.rs:468-471` clears on a move to a
/// hidden zone, and `zones.rs:508-513` re-applies ONLY on a battlefield exit
/// (`from == Zone::Battlefield`, `zones.rs:482`). A card carrying a sticker in
/// hand or library therefore keeps `obj.stickers` while `obj.abilities` never
/// reflects it, so this predicate and the bind (`casting::combined_spell_ability_def`)
/// read the same list and agree. IF THAT GAP IS EVER CLOSED — by un-gating
/// `zones.rs:508-513`, by widening `zones.rs:482`, or by adding an entry-side
/// install (`zone_pipeline.rs` today contains zero occurrences of "sticker") —
/// then an ability sticker's granted abilities are installed into `abilities`
/// during the cast's own move to the stack, i.e. AFTER this predicate has read
/// the object, from a payload (`obj.stickers`, Oracle TEXT) that no traversal
/// arm can reach without re-running the parser. This predicate is then UNSOUND
/// and needs a fall-open rail immediately below the `layers_dirty` one:
///     if crate::game::stickers::object_has_sticker_kind(
///         source, crate::types::stickers::StickerKind::Ability) { return true; }
/// `object_has_sticker_kind` (`game/stickers.rs:101`, unconditionally `pub`) is
/// `obj.stickers.iter().any(|s| s.kind() == kind)` — no allocation, no clone.
/// `StickerKind::Ability` is the correct discriminant: `stickers.rs:483` is the
/// sole gate on the path to the `abilities` write at `:490`.
///
/// Deliberately stated WITHOUT a reachability qualifier: deciding which of those
/// dispositions applies needs one function read, whereas deciding "is this
/// reachable from the cast reducer" needs a call-chain trace, and that trace was
/// got wrong in four consecutive reviews of this guard.
///
/// To re-audit, sweep the CHANGE, not the tree — the tree census is 516 hits:
///   git diff <base>..HEAD -U0 -- crates/engine/src \
///     | rg '^\+.*(\.(base_)?abilities\s*=([^=]|$)|&mut\s+[A-Za-z_0-9.:&()\[\] ]*\.(base_)?abilities\b)'
/// The second alternation deliberately matches ANY `&mut <path>.abilities`, not
/// just `Arc::make_mut(&mut obj.abilities)`: the engine writes this field
/// through a complex receiver 84 times
/// (`Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities)`) and
/// takes a `&mut` binding to it (`let a = &mut obj.abilities;`) once, and a
/// `make_mut(&mut <ident>.abilities)` pattern sees neither. A `&mut` borrow of
/// this field taken to a local is itself a site worth surfacing.
/// A binding whose mutation happens on a LATER line is invisible to any
/// single-line regex; the first line of the pair matches, so read the whole
/// diff hunk when it fires.
/// Do NOT audit `casting::prepare_casting_variant` alone: the live-face swap at
/// `casting.rs:9860` (`handle_cast_spell_with_payment_mode:10902` → `:12013` →
/// `continue_cast_from_prepared:9827` → the Disturb branch at `:9835` →
/// `continue_cast_with_alternative_spell_face:9849`) never enters it, and the
/// flip-revert install at `flip.rs:345`/`:346` is reached through the zone
/// pipeline, not through `casting.rs` at all.
///
/// CR 601.2a + CR 602.2b: the bound ability is an ability of the root's source
/// object, so that object's ability trees bound the reachable effect shapes.
pub fn root_may_yield_adverse_exchange(state: &GameState, action: &GameAction) -> bool {
    let Some(root) = RootBinding::from_action(action) else {
        // Not a cast or an activation: `targeted_exchange_verdict` is
        // Indeterminate by construction (see `RootBinding::from_action`).
        return false;
    };
    let Some(source) = state.objects.get(&root.source_object_id()) else {
        return true;
    };
    // CR 613.1f: layer 6 ability-adding and ability-removing effects rewrite
    // `abilities`, and `layers::evaluate_layers` re-derives that list from
    // `base_abilities` on each pass. A pending flush can therefore leave the
    // live list out of step with what the reducer will bind, which this
    // predicate must never guess at. CR 704.3's state-based-action loop
    // (`game::sba::check_state_based_actions`) flushes before every priority
    // window, which is the only place this gate runs.
    if state.layers_dirty.is_dirty() {
        return true;
    }
    // CR 602.2b: an activated ability's announcement binds a definition chosen by
    // `ability_index`, and `casting::activation_ability_definition` (casting.rs:497)
    // resolves an index at or past `obj.abilities.len()` from four families that are
    // SYNTHESIZED ON READ and written into no field entered below:
    // `runtime_granted_cycling_abilities` (CR 702.29a),
    // `runtime_granted_graveyard_activated_abilities`,
    // `runtime_granted_top_of_library_plot_abilities` (CR 702.170f), and
    // `runtime_granted_equip_abilities` (CR 702.6). The index is the ONLY thing that
    // distinguishes them, so answering from the lists below would be answering about
    // a definition they provably cannot contain. Fall open instead.
    //
    // This rail is load-bearing by CONSTRUCTION, not by payload: it holds no matter
    // what those four families come to synthesize. Today none of them carries an
    // adverse shape (cycling draws, embalm/eternalize/encore copy, plot exiles, equip
    // attaches), so deleting this rail keeps every current fixture green — which is
    // exactly why `activation_beyond_printed_abilities_falls_open` pins the index
    // boundary directly rather than pinning a card.
    if let RootBinding::Activation { ability_index, .. } = root {
        if ability_index >= source.abilities.len() {
            return true;
        }
    }
    // CR 613.1: the union of the printed and post-layer lists is a superset of
    // either, so a live removal cannot hide a shape the printed list carries.
    source
        .base_abilities
        .iter()
        .chain(source.abilities.iter())
        // CR 712.11b / CR 715.3a / CR 720.3a: a cast-time face election replaces
        // `abilities` AND `base_abilities` with the alternative face's list
        // (`casting::swap_to_alternative_spell_face` ->
        // `printed_cards::apply_back_face_to_object`) while the reducer is still
        // applying the root `CastSpell` — `casting.rs:11028-11039` elects a
        // single surviving variant inline, with no second `GameAction`. The
        // pre-swap source list is therefore also reachable input to the bind.
        .chain(
            source
                .back_face
                .iter()
                .flat_map(|back| back.abilities.iter()),
        )
        // CR 702.148b + CR 612: cleave's second ability is a text-changing
        // effect; `casting::apply_cleave_text_change` replaces both `abilities`
        // and `base_abilities` with the bracket-removed variant on the same
        // inline-election path, so that list is reachable input too.
        .chain(
            source
                .cleave_variant
                .iter()
                .flat_map(|variant| variant.abilities.iter()),
        )
        .any(|def| {
            visit_ability_def(def, &mut |effect| {
                if effect_may_yield_adverse_exchange(effect) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .is_break()
        })
}

/// The leaf shape test. The `_ => false` arm is correct BECAUSE this predicate is
/// an over-approximation whose default answer is "carries no adverse-exchange
/// shape" — it is not a missed-arm hazard on `Effect`. The wildcard-free part of
/// the guard is the traversal above it, not this leaf. A NEW `Effect` variant
/// cannot create a new `Reject`, because both judges hard-match named variants
/// (`find_fight_leaf` → `Effect::Fight`; `is_target_sourced_self_damage` →
/// `Effect::DealDamage`), so the reject set is closed under enum growth.
///
/// THE COUPLING THAT IS REAL RUNS THE OTHER WAY. This arm set must remain a
/// SUPERSET of every shape those two judges can reject on. Widening either judge
/// without widening this leaf silently narrows the guard below the reject set —
/// no compile error, no failing test, and nothing in the FALSIFIER above fires,
/// because that list audits ability INSTALLATION, not judge shape. The same
/// invariant is restated at both judges; all three must move together.
///
/// CR 701.14a: a Fight instruction makes two creatures deal damage to each other.
/// CR 120.1: `damage_source: Some(DamageSource::Target)` attributes the damage to
/// the ability's first object target rather than to its source, which is the
/// wording class `is_target_sourced_self_damage` gates. `DamageAll` shares that
/// one axis with `DealDamage` (see
/// `ability_utils::one_sided_fight_source_supplies_quantity_creature`), so it is
/// admitted here to keep the guard no tighter than the class.
fn effect_may_yield_adverse_exchange(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Fight { .. }
            | Effect::DealDamage {
                damage_source: Some(DamageSource::Target),
                ..
            }
            | Effect::DamageAll {
                damage_source: Some(DamageSource::Target),
                ..
            }
    )
}

/// Preview whether every complete, supported target declaration for `root` is
/// the strictly bad exchange where the selected friendly creature dies and its
/// exact recipient survives.
///
/// CR 601.2c: target choices are enumerated from the current reducer-issued
/// candidate set. CR 608.2c: after each target action, `pending_cast.ability`
/// is the single fully-bound carrier; it is inspected before classifying a
/// successor prompt such as normal mana payment.
pub fn targeted_exchange_verdict(
    state: &GameState,
    root: &CandidateAction,
) -> TargetedExchangeVerdict {
    targeted_exchange_verdict_inner(state, root).0
}

/// Test-support view of the same computation: the bounded-witness budget records
/// exactly how much replay work the verdict cost. Mirrors
/// [`crate::ai_support::adversarial_swarm_witness_with_counters`].
#[cfg(feature = "test-support")]
pub fn targeted_exchange_verdict_with_budget(
    state: &GameState,
    root: &CandidateAction,
) -> (TargetedExchangeVerdict, TargetedExchangeBudget) {
    targeted_exchange_verdict_inner(state, root)
}

fn targeted_exchange_verdict_inner(
    state: &GameState,
    root: &CandidateAction,
) -> (TargetedExchangeVerdict, TargetedExchangeBudget) {
    let mut budget = TargetedExchangeBudget::default();
    let Some(root_binding) = RootBinding::from_action(&root.action) else {
        return (TargetedExchangeVerdict::Indeterminate, budget);
    };
    let Some(semantic_owner) = root.metadata.semantic_owner else {
        return (TargetedExchangeVerdict::Indeterminate, budget);
    };
    // Clone-free precondition: a root that cannot be rejected must not pay the
    // candidate enumeration or the reducer replay below.
    if !root_may_yield_adverse_exchange(state, &root.action) {
        return (TargetedExchangeVerdict::Indeterminate, budget);
    }
    let Some(mut next) = replay_exact_candidate(state, root, &mut budget) else {
        return (TargetedExchangeVerdict::Indeterminate, budget);
    };
    let verdict = inspect_successor(&mut next, root_binding, semantic_owner, &mut budget);
    (verdict, budget)
}

/// Bounded-witness budget for one `targeted_exchange_verdict` call: the caps the
/// preview enforces, plus what the preview actually spent. The spend fields are
/// maintained unconditionally (three `usize` increments against a `GameState`
/// clone is not measurable) and are read only through
/// `targeted_exchange_verdict_with_budget`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TargetedExchangeBudget {
    pub nodes: usize,
    pub branches: usize,
    /// `replay_exact_candidate` clone-and-applies performed (root + target children).
    pub replay_clone_applies: usize,
    /// `preview_*` state clones taken to resolve a bound exchange.
    pub preview_clone_resolves: usize,
    /// Full `validated_candidate_actions_for_semantic_owner` passes this call ran.
    pub candidate_enumerations: usize,
}

fn inspect_successor(
    state: &mut GameState,
    root: RootBinding,
    semantic_owner: PlayerId,
    budget: &mut TargetedExchangeBudget,
) -> TargetedExchangeVerdict {
    if budget.nodes >= MAX_WITNESS_NODES {
        return TargetedExchangeVerdict::Indeterminate;
    }
    budget.nodes += 1;

    // CR 601.2h: automatic payment finalizes a normal-cost spell immediately
    // after its final target is declared. The target-bound carrier is therefore
    // either the matching PendingCast (manual payment) or the exact announced
    // Spell stack entry (automatic payment), before prompt classification.
    if let Some(ability) = bound_root_ability(state, root) {
        if let Some(verdict) = preview_bound_exchange(state, ability, semantic_owner, budget) {
            return verdict;
        }
    }

    match &state.waiting_for {
        WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
            explore_target_children(state, root, semantic_owner, budget)
        }
        // legal_actions intentionally represents only all-targets (and possibly
        // empty) here; it is not an exhaustive subset enumerator.
        WaitingFor::MultiTargetSelection { .. }
        | WaitingFor::ManaPayment { .. }
        | WaitingFor::ChooseXValue { .. }
        | WaitingFor::ModeChoice { .. }
        | WaitingFor::AbilityModeChoice { .. }
        | WaitingFor::OptionalEffectChoice { .. } => TargetedExchangeVerdict::Indeterminate,
        _ => TargetedExchangeVerdict::Indeterminate,
    }
}

/// Return the finalized target-bound ability only when it can be authenticated
/// to this exact root. Casts retain that authority on their announcement stack
/// entry (CR 601.2a/h); activations use PendingCast because a stack entry does
/// not retain the originating ability index.
fn bound_root_ability(state: &GameState, root: RootBinding) -> Option<&ResolvedAbility> {
    if let Some(pending) = state
        .pending_cast
        .as_deref()
        .filter(|pending| root.matches_pending(pending))
    {
        return Some(pending.ability.as_ref());
    }

    let RootBinding::Cast { object_id } = root else {
        return None;
    };
    state.stack.iter().rev().find_map(|entry| {
        (entry.id == object_id && entry.source_id == object_id)
            .then_some(&entry.kind)
            .and_then(|kind| match kind {
                StackEntryKind::Spell {
                    ability: Some(ability),
                    ..
                } => Some(ability.as_ref()),
                StackEntryKind::Spell { ability: None, .. }
                | StackEntryKind::ActivatedAbility { .. }
                | StackEntryKind::TriggeredAbility { .. }
                | StackEntryKind::KeywordAction { .. } => None,
            })
    })
}

fn explore_target_children(
    state: &GameState,
    root: RootBinding,
    semantic_owner: PlayerId,
    budget: &mut TargetedExchangeBudget,
) -> TargetedExchangeVerdict {
    let owner = target_selection_owner(&state.waiting_for);
    let Some(owner) = owner else {
        return TargetedExchangeVerdict::Indeterminate;
    };
    budget.candidate_enumerations += 1;
    let candidates = validated_candidate_actions_for_semantic_owner(state, owner);
    if candidates
        .iter()
        .any(|candidate| matches!(candidate.action, GameAction::ChooseTarget { target: None }))
    {
        return TargetedExchangeVerdict::Indeterminate;
    }
    let target_children: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| {
            matches!(
                candidate.action,
                GameAction::ChooseTarget { target: Some(_) }
            )
        })
        .collect();
    if target_children.is_empty() || target_children.len() > MAX_WITNESS_BRANCHES {
        return TargetedExchangeVerdict::Indeterminate;
    }

    let mut saw_reject = false;
    let mut saw_indeterminate = false;
    for child in target_children {
        if budget.branches >= MAX_WITNESS_BRANCHES {
            return TargetedExchangeVerdict::Indeterminate;
        }
        budget.branches += 1;
        let Some(mut next) = replay_exact_candidate(state, &child, budget) else {
            return TargetedExchangeVerdict::Indeterminate;
        };
        match inspect_successor(&mut next, root, semantic_owner, budget) {
            TargetedExchangeVerdict::Reject => saw_reject = true,
            TargetedExchangeVerdict::Allow => return TargetedExchangeVerdict::Allow,
            TargetedExchangeVerdict::Indeterminate => saw_indeterminate = true,
        }
    }
    if saw_indeterminate {
        TargetedExchangeVerdict::Indeterminate
    } else if saw_reject {
        TargetedExchangeVerdict::Reject
    } else {
        TargetedExchangeVerdict::Indeterminate
    }
}

fn target_selection_owner(waiting_for: &WaitingFor) -> Option<PlayerId> {
    match waiting_for {
        WaitingFor::TargetSelection { player, .. }
        | WaitingFor::TriggerTargetSelection { player, .. } => Some(*player),
        _ => None,
    }
}

fn replay_exact_candidate(
    state: &GameState,
    wanted: &CandidateAction,
    budget: &mut TargetedExchangeBudget,
) -> Option<GameState> {
    let semantic_owner = wanted.metadata.semantic_owner?;
    let actor = wanted.metadata.actor?;
    budget.candidate_enumerations += 1;
    let current = validated_candidate_actions_for_semantic_owner(state, semantic_owner);
    current
        .iter()
        .any(|candidate| {
            candidate.action.cmp_stable(&wanted.action).is_eq()
                && candidate.metadata.semantic_owner == Some(semantic_owner)
                && candidate.metadata.actor == Some(actor)
                && candidate.metadata.tactical_class == wanted.metadata.tactical_class
        })
        .then(|| {
            budget.replay_clone_applies += 1;
            let mut next = state.clone();
            apply_interaction_for_simulation(
                &mut next,
                actor,
                semantic_owner,
                wanted.action.clone(),
            )
            .ok()
            .map(|_| next)
        })?
}

fn preview_bound_exchange(
    state: &GameState,
    ability: &ResolvedAbility,
    semantic_owner: PlayerId,
    budget: &mut TargetedExchangeBudget,
) -> Option<TargetedExchangeVerdict> {
    if is_target_sourced_self_damage(ability) {
        return preview_target_sourced_self_damage(state, ability, semantic_owner, budget);
    }
    let fight = find_fight_leaf(ability)?;
    preview_fight_exchange(state, ability, fight, semantic_owner, budget)
}

fn preview_target_sourced_self_damage(
    state: &GameState,
    ability: &ResolvedAbility,
    semantic_owner: PlayerId,
    budget: &mut TargetedExchangeBudget,
) -> Option<TargetedExchangeVerdict> {
    let (source, recipient) = exchange_participants(state, ability, semantic_owner)?;
    budget.preview_clone_resolves += 1;
    let mut preview = state.clone();
    flush_layers(&mut preview);
    let source_ref = ObjectIncarnationRef::from_object(preview.objects.get(&source)?);
    let recipient_ref = match recipient {
        TargetRef::Object(recipient) => ExchangeRecipient::Object(
            ObjectIncarnationRef::from_object(preview.objects.get(&recipient)?),
        ),
        TargetRef::Player(recipient) => ExchangeRecipient::Player(recipient),
    };
    let mut events = Vec::new();
    resolve_ability_chain(&mut preview, ability, &mut events, 0).ok()?;
    check_state_based_actions(&mut preview, &mut events);

    let source_left = !same_battlefield_incarnation(&preview, source_ref);
    let recipient_remains = recipient_ref.remains_in_game(&preview);
    Some(if source_left && recipient_remains {
        TargetedExchangeVerdict::Reject
    } else {
        TargetedExchangeVerdict::Allow
    })
}

/// INVARIANT (shared with `is_target_sourced_self_damage` and
/// `effect_may_yield_adverse_exchange`): widening the shapes this judge can
/// reject on REQUIRES widening `effect_may_yield_adverse_exchange` in the same
/// change. That leaf is the clone-free precondition
/// `root_may_yield_adverse_exchange` answers from, and it must stay a superset of
/// this judge; if it narrows below, `search::root_action_is_allowed` returns early
/// and this judge never runs, silently dropping the `Reject`. Nothing enforces
/// this at compile time.
fn find_fight_leaf(ability: &ResolvedAbility) -> Option<&ResolvedAbility> {
    if matches!(&ability.effect, Effect::Fight { .. }) {
        return Some(ability);
    }
    ability.sub_ability.as_deref().and_then(find_fight_leaf)
}

fn preview_fight_exchange(
    state: &GameState,
    ability: &ResolvedAbility,
    fight: &ResolvedAbility,
    semantic_owner: PlayerId,
    budget: &mut TargetedExchangeBudget,
) -> Option<TargetedExchangeVerdict> {
    let (first, second) =
        crate::game::effects::fight::resolve_fight_fighters(state, fight).ok()??;
    let first_controller = state.objects.get(&first)?.controller;
    let second_controller = state.objects.get(&second)?.controller;
    let (ai_fighter, opposing_fighter) = match (
        first_controller == semantic_owner,
        second_controller == semantic_owner,
    ) {
        (true, false) => (first, second),
        (false, true) => (second, first),
        // The tactical veto owns only an adverse exchange between one AI
        // creature and one opposing creature. Every other control layout stays
        // available for the normal evaluator.
        (false, false) | (true, true) => return Some(TargetedExchangeVerdict::Allow),
    };
    if !valid_exchange_participants(state, ai_fighter, opposing_fighter) {
        return None;
    }

    budget.preview_clone_resolves += 1;
    let mut preview = state.clone();
    flush_layers(&mut preview);
    let ai_ref = ObjectIncarnationRef::from_object(preview.objects.get(&ai_fighter)?);
    let opposing_ref = ObjectIncarnationRef::from_object(preview.objects.get(&opposing_fighter)?);
    // CR 608.2c + CR 701.14a: replay every already-bound instruction that
    // precedes this Fight (for example, a +2/+2 modifier), then stop at the
    // Fight itself. Later effects must not rewrite the fight's tactical result.
    let mut fight_prefix = ability.clone();
    truncate_after_fight(&mut fight_prefix)?;
    let mut events = Vec::new();
    resolve_ability_chain(&mut preview, &fight_prefix, &mut events, 0).ok()?;
    check_state_based_actions(&mut preview, &mut events);

    let ai_left = !same_battlefield_incarnation(&preview, ai_ref);
    let opposing_remains = same_battlefield_incarnation(&preview, opposing_ref);
    Some(if ai_left && opposing_remains {
        TargetedExchangeVerdict::Reject
    } else {
        TargetedExchangeVerdict::Allow
    })
}

/// Keep the root-to-Fight prefix of an already-bound chain, then remove only
/// the continuation after that Fight. `find_fight_leaf` and this helper share
/// the same continuation traversal, so a preview cannot sever a predecessor.
fn truncate_after_fight(ability: &mut ResolvedAbility) -> Option<()> {
    if matches!(&ability.effect, Effect::Fight { .. }) {
        ability.sub_ability = None;
        ability.else_ability = None;
        return Some(());
    }
    ability
        .sub_ability
        .as_deref_mut()
        .and_then(truncate_after_fight)
}

#[derive(Debug, Clone, Copy)]
enum ExchangeRecipient {
    Object(ObjectIncarnationRef),
    Player(PlayerId),
}

impl ExchangeRecipient {
    fn remains_in_game(self, state: &GameState) -> bool {
        match self {
            Self::Object(reference) => same_battlefield_incarnation(state, reference),
            // CR 704.5a: `check_state_based_actions` marks a player who took
            // lethal damage as eliminated, while a prevention or can't-lose
            // effect correctly leaves that player in the game.
            Self::Player(player) => crate::game::players::is_alive(state, player),
        }
    }
}

/// INVARIANT (shared with `find_fight_leaf` and
/// `effect_may_yield_adverse_exchange`): widening the shapes this judge can reject
/// on REQUIRES widening `effect_may_yield_adverse_exchange` in the same change.
/// That leaf is the clone-free precondition `root_may_yield_adverse_exchange`
/// answers from, and it must stay a superset of this judge; if it narrows below,
/// `search::root_action_is_allowed` returns early and this judge never runs,
/// silently dropping the `Reject`. Nothing enforces this at compile time.
fn is_target_sourced_self_damage(ability: &ResolvedAbility) -> bool {
    let ability = match &ability.effect {
        // CR 601.2c: target-subject wording declares its damage-source target
        // on an outer picker node. The actual consecutive damage instructions
        // remain beneath that declaration.
        Effect::TargetOnly { .. } => match ability.sub_ability.as_deref() {
            Some(sub_ability) => sub_ability,
            None => return false,
        },
        _ => ability,
    };
    matches!(
        (&ability.effect, ability.sub_ability.as_deref()),
        (
            Effect::DealDamage {
                damage_source: Some(DamageSource::Target),
                ..
            },
            Some(ResolvedAbility {
                effect: Effect::DealDamage {
                    damage_source: Some(DamageSource::Target),
                    target: TargetFilter::ParentTargetSlot { index: 0 },
                    ..
                },
                sub_ability: None,
                ..
            })
        )
    )
}

fn exchange_participants(
    state: &GameState,
    ability: &ResolvedAbility,
    semantic_owner: PlayerId,
) -> Option<(ObjectId, TargetRef)> {
    let mut targets = crate::game::ability_utils::flatten_targets_in_chain(ability).into_iter();
    let TargetRef::Object(source) = targets.next()? else {
        return None;
    };
    let recipient = targets.next()?;
    valid_targeted_exchange_participants(state, source, &recipient, semantic_owner)
        .then_some((source, recipient))
}

fn valid_targeted_exchange_participants(
    state: &GameState,
    source: ObjectId,
    recipient: &TargetRef,
    semantic_owner: PlayerId,
) -> bool {
    let Some(source_object) = state.objects.get(&source) else {
        return false;
    };
    source_object.zone == Zone::Battlefield
        && source_object.controller == semantic_owner
        && source_object
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        && match recipient {
            // `any other target` may legally select a friendly permanent or the
            // controller. They must still be replayed: a source that destroys
            // itself while that selected recipient remains is an adverse outcome,
            // not an unsupported branch that turns the whole root indeterminate.
            TargetRef::Object(recipient) => {
                source != *recipient
                    && state
                        .objects
                        .get(recipient)
                        .is_some_and(|object| object.zone == Zone::Battlefield)
            }
            TargetRef::Player(recipient) => crate::game::players::is_alive(state, *recipient),
        }
}

fn valid_exchange_participants(state: &GameState, source: ObjectId, recipient: ObjectId) -> bool {
    let Some(source_object) = state.objects.get(&source) else {
        return false;
    };
    let Some(recipient_object) = state.objects.get(&recipient) else {
        return false;
    };
    source != recipient
        && source_object.zone == Zone::Battlefield
        && recipient_object.zone == Zone::Battlefield
        && source_object
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        && recipient_object
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        && source_object.controller != recipient_object.controller
}

fn same_battlefield_incarnation(state: &GameState, reference: ObjectIncarnationRef) -> bool {
    state
        .objects
        .get(&reference.object_id)
        .is_some_and(|object| {
            object.zone == Zone::Battlefield
                && ObjectIncarnationRef::from_object(object) == reference
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef,
        CopiableValues, CounterSourceRider, DelayedTriggerCondition, DieResultBranch, Duration,
        PileSource, PlayerFilter, PlayerScope, PtValue, QuantityExpr, ReplacementDefinition,
        ReplacementMode, StaticDefinition, TriggerDefinition, TypeFilter, TypedFilter,
        UnlessPayModifier, VoteSubject, VoteTally, VoteVisibility, VoterScope,
    };
    use crate::types::card::CleaveVariant;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::CastPaymentMode;
    use crate::types::identifiers::CardId;
    use crate::types::phase::Phase;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;
    use std::sync::Arc;

    fn add_creature(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Exchange Test Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&object_id)
            .expect("created creature must exist")
            .card_types
            .core_types
            .push(CoreType::Creature);
        object_id
    }

    #[test]
    fn self_damage_exchange_ignores_an_opposing_source_that_destroys_a_friendly_recipient() {
        let mut state = GameState::new_two_player(0);
        let opposing_source = add_creature(&mut state, PlayerId(1));
        let friendly_recipient = add_creature(&mut state, PlayerId(0));

        assert!(
            !valid_targeted_exchange_participants(
                &state,
                opposing_source,
                &TargetRef::Object(friendly_recipient),
                PlayerId(0),
            ),
            "an opposing creature dying to preserve the AI's creature is favorable, so it is outside this adverse-exchange veto"
        );
    }

    /// Root replay must retain the candidate's semantic owner while inspecting
    /// a target-sourced damage exchange. If the selected source belongs to the
    /// opponent and the recipient is friendly, that branch is favorable and
    /// must never trigger the adverse-exchange veto.
    #[test]
    fn root_replay_does_not_reject_opposing_source_and_friendly_recipient() {
        let mut state = GameState::new_two_player(0);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let opposing_source = add_creature(&mut state, PlayerId(1));
        let friendly_recipient = add_creature(&mut state, PlayerId(0));
        state.objects.get_mut(&opposing_source).unwrap().power = Some(1);
        state.objects.get_mut(&opposing_source).unwrap().toughness = Some(1);
        state.objects.get_mut(&friendly_recipient).unwrap().power = Some(3);
        state
            .objects
            .get_mut(&friendly_recipient)
            .unwrap()
            .toughness = Some(3);
        let card_id = CardId(state.next_object_id);
        let spell = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Exchange Replay Test".to_string(),
            Zone::Hand,
        );
        let spell_object = state
            .objects
            .get_mut(&spell)
            .expect("created spell must exist");
        spell_object.card_types.core_types.push(CoreType::Sorcery);
        *Arc::make_mut(&mut spell_object.abilities) = parse_oracle_text(
            "Target creature deals 2 damage to any other target and 2 damage to itself.",
            "Exchange Replay Test",
            &[],
            &["Sorcery".to_string()],
            &[],
        )
        .abilities;

        let root = validated_candidate_actions_for_semantic_owner(&state, PlayerId(0))
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == spell)
            })
            .expect("the engine must issue the root cast candidate");

        assert_eq!(root.metadata.semantic_owner, Some(PlayerId(0)));
        assert!(
            !valid_targeted_exchange_participants(
                &state,
                opposing_source,
                &TargetRef::Object(friendly_recipient),
                PlayerId(0),
            ),
            "reach guard: the 1/1 opposing source dies while the 3/3 friendly recipient survives, but this branch is favorable to the semantic owner"
        );
        assert!(
            !matches!(
                targeted_exchange_verdict(&state, &root),
                TargetedExchangeVerdict::Reject
            ),
            "semantic-owner-aware root replay must not veto a favorable opposing-source branch"
        );
    }

    // ---------------------------------------------------------------------
    // `root_may_yield_adverse_exchange` — the clone-free precondition.
    //
    // Every fixture below flushes the layer lattice and asserts it is `Clean`
    // before reading the guard's answer. That reach guard is load-bearing in
    // both directions: the guard's first rail is `layers_dirty.is_dirty() =>
    // return true`, so a dirty lattice makes every `true` assertion vacuous and
    // every `false` assertion fail for the wrong reason. `a_pending_layer_grant_
    // falls_open` is the one test that deliberately sits on the other side of
    // that rail.
    // ---------------------------------------------------------------------

    fn fight_effect() -> Effect {
        Effect::Fight {
            target: TargetFilter::Any,
            subject: TargetFilter::SelfRef,
        }
    }

    fn fight_def() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, fight_effect())
    }

    fn benign_def() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate)
    }

    fn fight_cost() -> AbilityCost {
        AbilityCost::EffectCost {
            effect: Box::new(fight_effect()),
        }
    }

    fn grant_fight_static() -> StaticDefinition {
        StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
            .modifications(vec![ContinuousModification::GrantAbility {
                definition: Box::new(fight_def()),
            }])
    }

    /// A priority window with a sorcery in P0's hand carrying `abilities`, and a
    /// `Clean` layer lattice.
    fn guard_fixture(abilities: Vec<AbilityDefinition>) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(0);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let card_id = CardId(state.next_object_id);
        let spell = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Guard Test Spell".to_string(),
            Zone::Hand,
        );
        let spell_object = state
            .objects
            .get_mut(&spell)
            .expect("created spell must exist");
        spell_object.card_types.core_types.push(CoreType::Sorcery);
        *Arc::make_mut(&mut spell_object.abilities) = abilities;
        flush_layers(&mut state);
        (state, spell)
    }

    fn cast_action(state: &GameState, object_id: ObjectId) -> GameAction {
        GameAction::CastSpell {
            object_id,
            card_id: state
                .objects
                .get(&object_id)
                .expect("fixture object must exist")
                .card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        }
    }

    /// Read the guard's answer for the root cast of `object_id`, after proving
    /// the `layers_dirty` rail is not what answers.
    fn guard_answer(state: &GameState, object_id: ObjectId) -> bool {
        assert!(
            !state.layers_dirty.is_dirty(),
            "reach guard: a dirty lattice makes `root_may_yield_adverse_exchange` fall open before it reads any ability list"
        );
        root_may_yield_adverse_exchange(state, &cast_action(state, object_id))
    }

    fn guard_sees(def: AbilityDefinition) -> bool {
        let (state, spell) = guard_fixture(vec![def]);
        guard_answer(&state, spell)
    }

    fn carries_fight(object: &crate::game::game_object::GameObject) -> bool {
        object
            .base_abilities
            .iter()
            .chain(object.abilities.iter())
            .any(|def| matches!(&*def.effect, Effect::Fight { .. }))
    }

    /// N1 — `damage_source: None` is ordinary spell-sourced damage and must not
    /// admit; `is_target_sourced_self_damage` cannot match it.
    #[test]
    fn plain_spell_sourced_damage_is_not_an_adverse_exchange_shape() {
        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        );
        assert!(
            !guard_sees(def),
            "spell-sourced damage carries no adverse-exchange shape; admitting it would make the guard inert"
        );
    }

    /// N2 — `DamageAll` shares the `damage_source` axis with `DealDamage`, so the
    /// guard must be no tighter than that class.
    #[test]
    fn damage_all_from_a_target_source_is_admitted() {
        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                player_filter: None,
                damage_source: Some(DamageSource::Target),
            },
        );
        assert!(
            guard_sees(def),
            "target-sourced DamageAll is in the gated class (CR 120.1); dropping the arm makes the guard tighter than the class"
        );
    }

    /// H1 — hostile: the named source object is absent. An over-approximating
    /// guard must fall open, never claim the root is safe.
    #[test]
    fn missing_source_object_falls_open() {
        let (state, _spell) = guard_fixture(vec![fight_def()]);
        assert!(!state.layers_dirty.is_dirty(), "reach guard: lattice Clean");
        let action = GameAction::CastSpell {
            object_id: ObjectId(9999),
            card_id: CardId(9999),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        };
        assert!(
            root_may_yield_adverse_exchange(&state, &action),
            "an absent source proves nothing about the bound ability, so the guard must fall open"
        );
    }

    /// H2 — hostile: all four entered lists are empty. The paired positive in the
    /// same test is what keeps the `false` from being a tautology.
    #[test]
    fn source_with_no_abilities_cannot_reject() {
        let (mut state, spell) = guard_fixture(vec![]);
        {
            let source = state.objects.get(&spell).expect("fixture spell must exist");
            assert!(
                source.base_abilities.is_empty(),
                "precondition: base_abilities empty"
            );
            assert!(source.abilities.is_empty(), "precondition: abilities empty");
            assert!(source.back_face.is_none(), "precondition: back_face absent");
            assert!(
                source.cleave_variant.is_none(),
                "precondition: cleave_variant absent"
            );
        }
        assert!(
            !guard_answer(&state, spell),
            "an object with no ability definitions anywhere cannot bind an adverse-exchange shape"
        );

        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&spell)
                .expect("fixture spell must exist")
                .abilities,
        )
        .push(fight_def());
        assert!(
            guard_answer(&state, spell),
            "paired positive: the same fixture must flip once a Fight definition is present"
        );
    }

    /// H3 — hostile multi-authority: two ability trees on one object, in both
    /// push orders. Scanning only the first entry fails the benign-first case.
    #[test]
    fn a_second_ability_carrying_the_shape_still_admits() {
        for (label, abilities) in [
            ("benign first", vec![benign_def(), fight_def()]),
            ("shape first", vec![fight_def(), benign_def()]),
        ] {
            let (state, spell) = guard_fixture(abilities);
            assert!(
                guard_answer(&state, spell),
                "{label}: every ability tree on the object must be walked, not just the first"
            );
        }
    }

    /// H4 — assumption A's rail. A layer-6 grant that has not been flushed is not
    /// in `abilities` yet, so the guard must refuse to answer from a stale list.
    ///
    /// Scope: this row decides the `layers_dirty` rail only. It deliberately
    /// marks the lattice dirty and so never reaches assumption B (the
    /// stale-`Clean` case), which `installing_a_layer_six_grant_marks_the_lattice_dirty`
    /// pins from the other side.
    #[test]
    fn a_pending_layer_grant_falls_open() {
        let mut state = GameState::new_two_player(0);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let creature = add_creature(&mut state, PlayerId(0));
        let granter = add_creature(&mut state, PlayerId(0));
        state
            .objects
            .get_mut(&granter)
            .expect("granter must exist")
            .static_definitions
            .push(grant_fight_static());
        // Deliberately NOT flushed: the grant must still be pending when the
        // guard reads the object, which is the whole point of this row.
        crate::game::layers::mark_layers_full(&mut state);

        {
            let source = state.objects.get(&creature).expect("creature must exist");
            assert!(
                !carries_fight(source),
                "precondition: the grant is still pending, so neither entered list carries the shape — without the rail the guard would answer `false`"
            );
        }
        assert!(
            state.layers_dirty.is_dirty(),
            "precondition: the fixture is on the dirty side of the rail"
        );
        assert!(
            root_may_yield_adverse_exchange(
                &state,
                &GameAction::ActivateAbility {
                    source_id: creature,
                    ability_index: 0,
                }
            ),
            "CR 613.1f: a pending layer-6 grant can add the shape after this read, so the guard must fall open"
        );

        // Non-vacuity: the grant is real. On a flushed copy of the same state it
        // lands in `abilities`, which is what the dirty case was hiding.
        let mut flushed = state.clone();
        flush_layers(&mut flushed);
        assert!(
            carries_fight(flushed.objects.get(&creature).expect("creature must exist")),
            "the fixture's grant must actually install once the lattice is flushed, or the dirty case proves nothing"
        );
    }

    /// B1-fx — assumption B's tracking fixture. Moving a permanent that carries a
    /// layer-6 grant onto the battlefield must mark the lattice dirty. This does
    /// not prove the engine-wide invariant (no fixture can); it pins the specific
    /// mutation class the guard's `Clean` reading depends on.
    #[test]
    fn installing_a_layer_six_grant_marks_the_lattice_dirty() {
        let mut state = GameState::new_two_player(0);
        let card_id = CardId(state.next_object_id);
        let granter = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Grant Test Enchantment".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&granter)
            .expect("granter must exist")
            .static_definitions
            .push(grant_fight_static());
        flush_layers(&mut state);
        assert!(
            !state.layers_dirty.is_dirty(),
            "precondition: the lattice starts Clean, so the mark below is the one under test"
        );

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, granter, Zone::Battlefield, &mut events);
        assert!(
            state.layers_dirty.is_dirty(),
            "CR 613.1f: installing a layer-6 grant must mark the lattice, or the guard could read a stale `Clean` list"
        );
    }

    /// H5 — hostile: the shape is reachable only through a branch the bound
    /// tests' `.effect` + `.sub_ability` spine never walks.
    #[test]
    fn shape_behind_else_or_mode_branch_still_admits() {
        let mut with_else = benign_def();
        with_else.else_ability = Some(Box::new(fight_def()));
        assert!(
            guard_sees(with_else),
            "CR 608.2c: an `else` continuation is part of the definition tree the bind reads"
        );

        let mut with_mode = benign_def();
        with_mode.mode_abilities.push(benign_def());
        with_mode.mode_abilities.push(fight_def());
        assert!(
            guard_sees(with_mode),
            "CR 700.2a: modes are chosen as part of casting, so a mode branch really can feed the bound spine"
        );
    }

    /// H6 — hostile branch precedence: a non-cast/non-activate action is answered
    /// by `RootBinding::from_action`, before the guard reads anything. The fixture
    /// deliberately holds a Fight spell so the `false` is about the action kind.
    #[test]
    fn non_root_action_short_circuits_before_the_guard() {
        let (state, spell) = guard_fixture(vec![fight_def()]);
        assert!(
            guard_answer(&state, spell),
            "precondition: this state DOES carry an adverse shape on its cast root"
        );
        assert!(
            !root_may_yield_adverse_exchange(&state, &GameAction::PassPriority),
            "a non-root action is Indeterminate by construction; the guard must not read a source for it"
        );
        let pass = validated_candidate_actions_for_semantic_owner(&state, PlayerId(0))
            .into_iter()
            .find(|candidate| matches!(candidate.action, GameAction::PassPriority))
            .expect("the engine must issue a pass-priority candidate at a priority window");
        assert_eq!(
            targeted_exchange_verdict(&state, &pass),
            TargetedExchangeVerdict::Indeterminate,
            "the verdict's own `RootBinding::from_action` early-out keeps its current precedence"
        );
    }

    /// H7 — carrier completeness. Mirrors
    /// `printed_cards::tests::walker_covers_every_nested_carrier` with
    /// `Effect::Fight` as the marker instead of `Effect::Conjure`. A future nested
    /// struct field is not caught by the compiler; these two fixtures are the
    /// safety net, and a dropped descent fails both.
    #[test]
    fn predicate_sees_a_fight_in_every_nested_carrier() {
        let mut cases: Vec<(&str, AbilityDefinition)> = Vec::new();

        // --- AbilityDefinition level ---
        let mut sub = benign_def();
        sub.sub_ability = Some(Box::new(fight_def()));
        cases.push(("sub_ability", sub));

        let mut else_branch = benign_def();
        else_branch.else_ability = Some(Box::new(fight_def()));
        cases.push(("else_ability", else_branch));

        let mut mode = benign_def();
        mode.mode_abilities.push(fight_def());
        cases.push(("mode_abilities", mode));

        let mut cost = benign_def();
        cost.cost = Some(fight_cost());
        cases.push(("cost (EffectCost)", cost));

        let mut unless_pay = benign_def();
        unless_pay.unless_pay = Some(UnlessPayModifier {
            cost: fight_cost(),
            payer: TargetFilter::Controller,
        });
        cases.push(("unless_pay.cost", unless_pay));

        // --- AbilityCost level ---
        let mut composite = benign_def();
        composite.cost = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, fight_cost()],
        });
        cases.push(("AbilityCost::Composite", composite));

        let mut one_of = benign_def();
        one_of.cost = Some(AbilityCost::OneOf {
            costs: vec![AbilityCost::Tap, fight_cost()],
        });
        cases.push(("AbilityCost::OneOf", one_of));

        let mut per_counter = benign_def();
        per_counter.cost = Some(AbilityCost::PerCounter {
            counter: CounterType::Plus1Plus1,
            target: TargetFilter::SelfRef,
            base: Box::new(fight_cost()),
        });
        cases.push(("AbilityCost::PerCounter.base", per_counter));

        // --- Effect level ---
        cases.push((
            "Vote::per_choice_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Vote {
                    choices: vec!["x".into()],
                    per_choice_effect: vec![Box::new(fight_def())],
                    starting_with: ControllerRef::You,
                    voter_scope: VoterScope::AllPlayers,
                    tally_mode: VoteTally::PerVote,
                    subject: VoteSubject::Named,
                    visibility: VoteVisibility::Open,
                },
            ),
        ));
        cases.push((
            "VoteSubject::Objects::outcome_template",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Vote {
                    choices: vec![],
                    per_choice_effect: vec![],
                    starting_with: ControllerRef::You,
                    voter_scope: VoterScope::AllPlayers,
                    tally_mode: VoteTally::PerVote,
                    subject: VoteSubject::Objects {
                        candidate_filter: TargetFilter::Any,
                        outcome_template: Box::new(fight_def()),
                    },
                    visibility: VoteVisibility::Open,
                },
            ),
        ));
        cases.push((
            "SeparateIntoPiles::chosen_pile_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SeparateIntoPiles {
                    partition_subject: VoterScope::EachOpponent,
                    object_filter: TargetFilter::Any,
                    chooser: PlayerScope::Controller,
                    chosen_pile_effect: Box::new(fight_def()),
                    pile_source: PileSource::Battlefield,
                    unchosen_pile_effect: None,
                },
            ),
        ));
        cases.push((
            "SeparateIntoPiles::unchosen_pile_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SeparateIntoPiles {
                    partition_subject: VoterScope::EachOpponent,
                    object_filter: TargetFilter::Any,
                    chooser: PlayerScope::Controller,
                    chosen_pile_effect: Box::new(benign_def()),
                    pile_source: PileSource::Battlefield,
                    unchosen_pile_effect: Some(Box::new(fight_def())),
                },
            ),
        ));
        cases.push((
            "RevealFromHand::on_decline",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RevealFromHand {
                    filter: TargetFilter::Any,
                    on_decline: Some(Box::new(fight_def())),
                },
            ),
        ));
        cases.push((
            "CreateDelayedTrigger::effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreateDelayedTrigger {
                    condition: DelayedTriggerCondition::AtNextPhase {
                        phase: Phase::Upkeep,
                    },
                    effect: Box::new(fight_def()),
                    uses_tracked_set: false,
                },
            ),
        ));
        cases.push((
            "FlipCoin::win_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoin {
                    win_effect: Some(Box::new(fight_def())),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
            ),
        ));
        cases.push((
            "FlipCoin::lose_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoin {
                    win_effect: None,
                    lose_effect: Some(Box::new(fight_def())),
                    flipper: TargetFilter::Controller,
                },
            ),
        ));
        cases.push((
            "FlipCoins::win_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    count: QuantityExpr::Fixed { value: 2 },
                    win_effect: Some(Box::new(fight_def())),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
            ),
        ));
        cases.push((
            "FlipCoins::lose_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    count: QuantityExpr::Fixed { value: 2 },
                    win_effect: None,
                    lose_effect: Some(Box::new(fight_def())),
                    flipper: TargetFilter::Controller,
                },
            ),
        ));
        cases.push((
            "FlipCoinUntilLose::win_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoinUntilLose {
                    win_effect: Box::new(fight_def()),
                },
            ),
        ));
        cases.push((
            "RollDie::results[].effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RollDie {
                    count: QuantityExpr::Fixed { value: 1 },
                    sides: 6,
                    results: vec![DieResultBranch {
                        min: 1,
                        max: 6,
                        effect: Box::new(fight_def()),
                    }],
                    modifier: None,
                },
            ),
        ));
        cases.push((
            "ChooseOneOf::branches",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![fight_def()],
                },
            ),
        ));
        cases.push((
            "CreateDrawReplacement::replacement_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreateDrawReplacement {
                    replacement_effect: Box::new(fight_effect()),
                },
            ),
        ));
        cases.push((
            "CreatePlaneswalkReplacement::replacement_effect",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreatePlaneswalkReplacement {
                    replacement_effect: Box::new(fight_effect()),
                },
            ),
        ));
        cases.push((
            "GenericEffect::static_abilities",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GenericEffect {
                    static_abilities: vec![grant_fight_static()],
                    duration: None,
                    target: None,
                    end_cost: None,
                },
            ),
        ));
        cases.push((
            "Token::static_abilities",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Token {
                    name: "T".to_string(),
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(1),
                    types: vec!["Creature".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![grant_fight_static()],
                    enter_with_counters: vec![],
                },
            ),
        ));
        cases.push((
            "CreateEmblem::statics",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreateEmblem {
                    statics: vec![grant_fight_static()],
                    triggers: vec![],
                },
            ),
        ));

        let mut emblem_trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        emblem_trigger.execute = Some(Box::new(fight_def()));
        cases.push((
            "CreateEmblem::triggers -> TriggerDefinition::execute",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreateEmblem {
                    statics: vec![],
                    triggers: vec![emblem_trigger],
                },
            ),
        ));

        let mut trigger_unless_pay = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger_unless_pay.unless_pay = Some(UnlessPayModifier {
            cost: fight_cost(),
            payer: TargetFilter::Controller,
        });
        cases.push((
            "TriggerDefinition::unless_pay.cost",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreateEmblem {
                    statics: vec![],
                    triggers: vec![trigger_unless_pay],
                },
            ),
        ));

        let mut repl_execute = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        repl_execute.execute = Some(Box::new(fight_def()));
        cases.push((
            "AddTargetReplacement -> ReplacementDefinition::execute",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::AddTargetReplacement {
                    replacement: Box::new(repl_execute),
                    target: TargetFilter::Any,
                },
            ),
        ));

        let mut repl_maycost_cost = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        repl_maycost_cost.mode = ReplacementMode::MayCost {
            cost: fight_cost(),
            decline: None,
        };
        cases.push((
            "ReplacementMode::MayCost.cost",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::AddTargetReplacement {
                    replacement: Box::new(repl_maycost_cost),
                    target: TargetFilter::Any,
                },
            ),
        ));

        let mut repl_maycost_decline = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        repl_maycost_decline.mode = ReplacementMode::MayCost {
            cost: AbilityCost::Tap,
            decline: Some(Box::new(fight_def())),
        };
        cases.push((
            "ReplacementMode::MayCost.decline",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::AddTargetReplacement {
                    replacement: Box::new(repl_maycost_decline),
                    target: TargetFilter::Any,
                },
            ),
        ));

        let mut repl_optional = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        repl_optional.mode = ReplacementMode::Optional {
            decline: Some(Box::new(fight_def())),
        };
        cases.push((
            "ReplacementMode::Optional.decline",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::AddTargetReplacement {
                    replacement: Box::new(repl_optional),
                    target: TargetFilter::Any,
                },
            ),
        ));

        cases.push((
            "Counter::source_rider::LosesAbilities::static_def",
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: Some(CounterSourceRider::LosesAbilities {
                        static_def: Box::new(grant_fight_static()),
                        duration: Box::new(Duration::UntilHostLeavesPlay),
                    }),
                    countered_spell_zone: None,
                },
            ),
        ));

        // --- ContinuousModification level (reached through a static) ---
        let mut grant_trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        grant_trigger.execute = Some(Box::new(fight_def()));
        cases.push((
            "ContinuousModification::GrantTrigger",
            static_carrier(ContinuousModification::GrantTrigger {
                trigger: Box::new(grant_trigger),
            }),
        ));

        let mut grant_replacement = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        grant_replacement.execute = Some(Box::new(fight_def()));
        cases.push((
            "ContinuousModification::GrantReplacement",
            static_carrier(ContinuousModification::GrantReplacement {
                replacement: Box::new(grant_replacement),
            }),
        ));

        cases.push((
            "ContinuousModification::GrantStaticAbility",
            static_carrier(ContinuousModification::GrantStaticAbility {
                definition: Box::new(grant_fight_static()),
            }),
        ));

        // --- CopiableValues level (reached through CopyValues) ---
        let mut copy_abilities = empty_copiable_values();
        copy_abilities.abilities = Arc::new(vec![fight_def()]);
        cases.push((
            "CopiableValues::abilities",
            static_carrier(copy_values(copy_abilities)),
        ));

        let mut copy_trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        copy_trigger.execute = Some(Box::new(fight_def()));
        let mut copy_triggers = empty_copiable_values();
        copy_triggers.trigger_definitions = Arc::new(vec![copy_trigger]);
        cases.push((
            "CopiableValues::trigger_definitions",
            static_carrier(copy_values(copy_triggers)),
        ));

        let mut copy_statics = empty_copiable_values();
        copy_statics.static_definitions = Arc::new(vec![grant_fight_static()]);
        cases.push((
            "CopiableValues::static_definitions",
            static_carrier(copy_values(copy_statics)),
        ));

        let mut copy_repl = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        copy_repl.execute = Some(Box::new(fight_def()));
        let mut copy_replacements = empty_copiable_values();
        copy_replacements.replacement_definitions = Arc::new(vec![copy_repl]);
        cases.push((
            "CopiableValues::replacement_definitions",
            static_carrier(copy_values(copy_replacements)),
        ));

        for (carrier, def) in cases {
            assert!(
                guard_sees(def),
                "the predicate missed an `Effect::Fight` planted in carrier '{carrier}'"
            );
        }
    }

    /// Wrap a `ContinuousModification` in the static-ability carrier
    /// `visit_ability_def` reaches from an `AbilityDefinition`.
    fn static_carrier(modification: ContinuousModification) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GenericEffect {
                static_abilities: vec![
                    StaticDefinition::new(StaticMode::Continuous).modifications(vec![modification])
                ],
                duration: None,
                target: None,
                end_cost: None,
            },
        )
    }

    fn copy_values(values: CopiableValues) -> ContinuousModification {
        ContinuousModification::CopyValues {
            values: Box::new(values),
            display_source: crate::game::game_object::DisplaySource::default(),
            printed_ref: None,
            token_image_ref: None,
        }
    }

    /// A `CopiableValues` whose four definition lists are empty, so only the list
    /// a test overwrites can carry the marker.
    fn empty_copiable_values() -> CopiableValues {
        let mut state = GameState::new_two_player(0);
        let object_id = add_creature(&mut state, PlayerId(0));
        let values = crate::game::printed_cards::intrinsic_copiable_values(
            state.objects.get(&object_id).expect("creature must exist"),
        );
        assert!(
            values.abilities.is_empty()
                && values.trigger_definitions.is_empty()
                && values.static_definitions.is_empty()
                && values.replacement_definitions.is_empty(),
            "precondition: the base CopiableValues must be marker-free"
        );
        values
    }

    /// H8 — `Effect::ChooseOneOf` is the shape real card data exercises (Sycorax
    /// Commander). The guard admits it; the bound spine then finds nothing,
    /// because the branch is chosen at resolution, so behavior is unchanged.
    #[test]
    fn fight_inside_choose_one_of_branches_is_admitted() {
        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![fight_def()],
            },
        );
        let (state, spell) = guard_fixture(vec![def]);
        assert!(
            guard_answer(&state, spell),
            "the guard must not rely on the bound-spine reduction; dropping the ChooseOneOf descent makes this `false`"
        );
        let root = validated_candidate_actions_for_semantic_owner(&state, PlayerId(0))
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == spell)
            })
            .expect("the engine must issue the root cast candidate");
        assert_eq!(
            targeted_exchange_verdict(&state, &root),
            TargetedExchangeVerdict::Indeterminate,
            "the announcement-time spine's root effect is still ChooseOneOf, so `find_fight_leaf` finds nothing and the verdict is unchanged"
        );
    }

    /// H9 — hostile second-face authority. The front lists carry no shape; the
    /// back face does. Both live cast-time face-election routes install from
    /// exactly this field (CR 712.11b / CR 715.3a / CR 720.3a), and Fuse reads it
    /// without installing at all.
    #[test]
    fn alternative_face_only_shape_still_admits() {
        let (mut state, spell) = guard_fixture(vec![benign_def()]);
        let mut face = crate::game::printed_cards::snapshot_object_base_face(
            state.objects.get(&spell).expect("fixture spell must exist"),
        );
        face.abilities = vec![fight_def()];
        {
            let source = state.objects.get(&spell).expect("fixture spell must exist");
            assert!(
                !carries_fight(source),
                "precondition: the front lists must be shape-free, or this row passes for the wrong reason"
            );
        }
        state
            .objects
            .get_mut(&spell)
            .expect("fixture spell must exist")
            .back_face = Some(face);
        assert!(
            guard_answer(&state, spell),
            "deleting the `back_face` chain arm silently loses every Reject reachable through a cast-time face swap"
        );
    }

    /// H9b — hostile second *text* authority on the same face. Cleave's second
    /// ability is a text-changing effect (CR 702.148b + CR 612), and
    /// `apply_cleave_text_change` installs both ability lists from this field.
    #[test]
    fn cleave_variant_only_shape_still_admits() {
        let (mut state, spell) = guard_fixture(vec![benign_def()]);
        {
            let source = state.objects.get(&spell).expect("fixture spell must exist");
            assert!(
                !carries_fight(source),
                "precondition: the front lists must be shape-free, or this row passes for the wrong reason"
            );
            assert!(
                source.back_face.is_none(),
                "precondition: no back face, so only the cleave arm can answer"
            );
        }
        state
            .objects
            .get_mut(&spell)
            .expect("fixture spell must exist")
            .cleave_variant = Some(CleaveVariant {
            abilities: vec![fight_def()],
            ..CleaveVariant::default()
        });
        assert!(
            guard_answer(&state, spell),
            "deleting the `cleave_variant` chain arm silently loses every Reject reachable through a cleave text change"
        );
    }

    /// H9c — the printed list is the layer-6 *input*, not the bind. CR 613.1: a
    /// `RemoveAllAbilities` grant (Humility, Turn to Frog) empties `abilities`
    /// while `base_abilities` still carries the printed shape, and the reducer
    /// re-derives from the printed list. Pins the `base_abilities` chain arm,
    /// which was the one arm of the four with no fixture of its own.
    #[test]
    fn base_abilities_only_shape_still_admits() {
        let (mut state, spell) = guard_fixture(vec![]);
        {
            let source = state
                .objects
                .get_mut(&spell)
                .expect("fixture spell must exist");
            *Arc::make_mut(&mut source.base_abilities) = vec![fight_def()];
        }
        {
            let source = state.objects.get(&spell).expect("fixture spell must exist");
            assert!(
                source.abilities.is_empty(),
                "precondition: the post-layer list must be empty, or the `abilities` arm answers and this row passes for the wrong reason"
            );
            assert!(
                source.back_face.is_none() && source.cleave_variant.is_none(),
                "precondition: no second text authority, so only the `base_abilities` arm can answer"
            );
        }
        assert!(
            guard_answer(&state, spell),
            "deleting the `base_abilities` chain arm silently loses every Reject on a source under a layer-6 ability removal"
        );
    }

    /// H10 — the `RootBinding::Activation` index rail. CR 602.2b: the announced
    /// ability is chosen by `ability_index`, and
    /// `casting::activation_ability_definition` resolves an index at or past
    /// `obj.abilities.len()` from four SYNTHESIZED-ON-READ families
    /// (cycling CR 702.29a, graveyard-activated, plot CR 702.170f, equip CR 702.6)
    /// that are written into no field the guard chains.
    ///
    /// Pins the INDEX BOUNDARY, not a card: none of those four families carries an
    /// adverse shape today, so a card-shaped fixture would stay green with the rail
    /// deleted. Both halves are load-bearing — the in-range half proves the fixture
    /// still discriminates, so the out-of-range half cannot pass vacuously.
    #[test]
    fn activation_beyond_printed_abilities_falls_open() {
        let (state, spell) = guard_fixture(vec![benign_def()]);
        assert!(
            !state.layers_dirty.is_dirty(),
            "reach guard: a dirty lattice makes the guard fall open before it reads the index"
        );
        let printed_len = state
            .objects
            .get(&spell)
            .expect("fixture spell must exist")
            .abilities
            .len();
        assert_eq!(
            printed_len, 1,
            "precondition: exactly one printed ability, so index 1 is the first synthesized slot"
        );
        let activation = |ability_index: usize| GameAction::ActivateAbility {
            source_id: spell,
            ability_index,
        };
        assert!(
            !root_may_yield_adverse_exchange(&state, &activation(0)),
            "non-vacuity: an in-range index over a benign printed list must still answer `false`, or the out-of-range row proves nothing"
        );
        assert!(
            root_may_yield_adverse_exchange(&state, &activation(printed_len)),
            "deleting the `RootBinding::Activation` index rail answers a runtime-granted activation from lists that provably cannot contain its definition"
        );
    }
}
