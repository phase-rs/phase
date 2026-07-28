//! Trigger IR types.
//!
//! `TriggerIr` represents the pre-lowering intermediate representation of a
//! parsed trigger line. IR production extracts the trigger condition, body, and
//! modifiers; lowering assembles them into the final `TriggerDefinition`.

use serde::Serialize;

use super::ast::parsed_clause;
use super::context::ParseContext;
use super::effect_chain::EffectChainIr;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ControllerRef, Effect, ModalChoice,
    TargetFilter, TargetSelectionMode, TriggerCondition, TriggerConstraint, TriggerDefinition,
    UnlessPayModifier,
};
use crate::types::triggers::TriggerMode;

/// The document-node payload for a trigger: either a decomposition the parser
/// produced, or a definition a recognizer already assembled.
///
/// The escape hatch lives HERE and not on `TriggerIr`, because
/// `TriggerDefinition -> TriggerIr` has no inverse. Two fields of the
/// decomposition are pure parse-time inputs with no representation in the
/// output — `TriggerModifiers::trigger_subject` (CR 608.2k pronoun subject) and
/// `TriggerModifiers::effect_lower`, the latter load-bearing at three sites in
/// `lower_trigger_ir` — and `TriggerBody` has no variant carrying an
/// already-lowered ability (unit 3b-5 deliberately deleted the one that did).
/// Lowering then unconditionally overwrites nine `TriggerDefinition` fields, so
/// a `partial_def = definition` round-trip does not survive contact with it.
///
/// This is the `QuantityExpr`/`QuantityRef` split CLAUDE.md mandates: a
/// finished definition is a *constant*, not a decomposition, so it wraps the IR
/// rather than becoming a variant of it.
///
/// The variant is `Assembled` rather than reusing the `OracleNodeIr` debt
/// marker's name, on purpose: that name is what
/// `scripts/check-prelowered-ratchet.sh` greps for, and this type is the
/// mechanism that RETIRES that debt. Reusing it would make the burn-down metric
/// count the fix as debt, so the number would rise as the debt fell.
///
/// One variant today, deliberately. The IR-native sibling (`Parsed`, boxing a
/// `TriggerIr`) is added by the tranche that lands its first producer, not
/// before: adding it now would need a `#[allow(dead_code)]` and a CR 707.9a
/// slot-stamp arm that cannot be written until lowering runs (the stamp targets
/// `execute`, which does not exist pre-lowering), leaving a latent panic behind.
/// Adding the variant later instead turns every match site into a compile
/// error, which is the stronger forcing function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum TriggerNodeIr {
    /// Already-assembled definition from a recognizer that builds its own.
    /// `lower_trigger_node_ir` returns it untouched.
    Assembled {
        definition: TriggerDefinition,
        /// The recognizer's own input text — provenance only. Document lowering
        /// derives spans and fragments from the emitting line, never from here.
        source_text: String,
    },
}

impl TriggerNodeIr {
    /// Wrap a recognizer-produced trigger definition for source-ordered emission.
    pub(crate) fn from_definition(source_text: &str, definition: TriggerDefinition) -> Self {
        Self::Assembled {
            definition,
            source_text: source_text.to_string(),
        }
    }

    /// The assembled definition, when one exists.
    ///
    /// Returns `Option` rather than `&TriggerDefinition` because a future
    /// IR-native variant has no definition before lowering — CR 607.2d relation
    /// discovery must see `None` there, not a fabricated definition.
    pub(crate) fn definition(&self) -> Option<&TriggerDefinition> {
        match self {
            Self::Assembled { definition, .. } => Some(definition),
        }
    }
}

/// Trigger-level IR: the complete parsed representation of a trigger line
/// before final assembly into `TriggerDefinition`.
///
/// Output of `parse_trigger_line_with_index_ir`. Consumed by `lower_trigger_ir`
/// to produce a `TriggerDefinition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TriggerIr {
    /// The parsed trigger condition (ETB, dies, phase trigger, etc.).
    pub(crate) condition: TriggerMode,
    /// Partially-populated `TriggerDefinition` from `parse_trigger_condition`.
    /// Carries typed fields (`valid_card`, `origin`, `destination`, `phase`,
    /// `damage_kind`, etc.) that lowering merges into the final output.
    pub(crate) partial_def: TriggerDefinition,
    /// The parsed effect body as typed IR.
    pub(crate) body: Option<TriggerBody>,
    /// Extracted modifier fields.
    pub(crate) modifiers: TriggerModifiers,
    /// Original oracle text for description/provenance.
    pub(crate) source_text: String,
}

/// The body of a trigger. Whole-body recognizers retain their typed payloads
/// here so trigger lowering owns all root-level transforms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum TriggerBody {
    /// Normal effect chain — lowering calls `lower_effect_chain_ir`.
    EffectChain(EffectChainIr),
    /// CR 118.12 + CR 603.12: A resolution-time optional cost and the
    /// reflexive effect that follows when the player pays it.
    ReflexivePayment(Box<ReflexivePaymentIr>),
    /// CR 700.2: An inline modal's marker clause and its already-lowered mode
    /// bodies. The marker still flows through ordinary trigger-chain lowering;
    /// this payload carries the modal metadata no clause can represent.
    Modal(Box<ModalIr>),
    /// CR 701.38: A vote block with its typed ballot effect and optional
    /// pre-ballot random choice.
    Vote(Box<VoteIr>),
    /// CR 700.3: A pile-separation block retains its semantic root effect.
    Pile(Box<PileIr>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReflexivePaymentIr {
    pub(crate) cost: AbilityCost,
    pub(crate) effect_chain: EffectChainIr,
}

/// CR 700.2: Typed inline-modal trigger body.
///
/// The root marker is an ordinary effect chain so trigger lowering applies the
/// same finalization, mana-scope, optional-targeting, and optional transforms
/// as every other trigger. `ModalChoice` and the independently parsed mode
/// bodies are root metadata rather than a pre-lowered root definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ModalIr {
    pub(crate) marker: EffectChainIr,
    pub(crate) choice: ModalChoice,
    pub(crate) mode_abilities: Vec<AbilityDefinition>,
}

/// CR 701.38: Typed vote trigger body.
///
/// `vote` is always an `Effect::Vote`; `pre_vote_choose` captures the one
/// structural wrapper in this class (Truth or Consequences' random opponent
/// choice). Lowering reconstructs that wrapper around the typed vote effect and
/// then sends the root through ordinary trigger-chain lowering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct VoteIr {
    source_text: String,
    vote: Effect,
    pre_vote_choose: Option<ChoiceType>,
    actor: Option<ControllerRef>,
    in_trigger: bool,
}

impl VoteIr {
    pub(crate) fn new(vote: Effect, pre_vote_choose: Option<ChoiceType>) -> Self {
        debug_assert!(matches!(vote, Effect::Vote { .. }));
        Self {
            source_text: String::new(),
            vote,
            pre_vote_choose,
            actor: None,
            in_trigger: false,
        }
    }

    pub(crate) fn with_source(mut self, source_text: &str) -> Self {
        self.source_text = source_text.to_string();
        self
    }

    pub(crate) fn with_context(mut self, ctx: &ParseContext) -> Self {
        self.actor = ctx.actor.clone();
        self.in_trigger = ctx.in_trigger;
        self
    }

    /// Construct the trigger-context chain without allocating a pre-lowered
    /// root definition. The nested vote definition is a continuation payload
    /// of the typed random-choice wrapper, not the trigger body itself.
    pub(crate) fn effect_chain(&self, kind: AbilityKind) -> EffectChainIr {
        let parsed = match &self.pre_vote_choose {
            Some(choice_type) => {
                let mut root = parsed_clause(Effect::Choose {
                    choice_type: choice_type.clone(),
                    persist: true,
                    selection: TargetSelectionMode::Random,
                });
                root.sub_ability = Some(Box::new(AbilityDefinition::new(kind, self.vote.clone())));
                root
            }
            None => parsed_clause(self.vote.clone()),
        };
        EffectChainIr::single_clause(
            &self.source_text,
            kind,
            parsed,
            None,
            self.actor.clone(),
            self.in_trigger,
        )
    }

    /// Compatibility lowering for non-trigger callers that have not yet moved
    /// to trigger-body IR. Trigger parsing uses [`Self::effect_chain`] instead.
    pub(crate) fn into_ability(self, kind: AbilityKind) -> AbilityDefinition {
        let vote = AbilityDefinition::new(kind, self.vote);
        match self.pre_vote_choose {
            Some(choice_type) => AbilityDefinition::new(
                kind,
                Effect::Choose {
                    choice_type,
                    persist: true,
                    selection: TargetSelectionMode::Random,
                },
            )
            .sub_ability(vote),
            None => vote,
        }
    }
}

/// CR 700.3: Typed pile-separation trigger body.
///
/// The root `Effect::SeparateIntoPiles` is an ordinary one-clause chain at
/// trigger lowering, preserving every root-level transform applied to a normal
/// trigger effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PileIr {
    source_text: String,
    effect: Effect,
    actor: Option<ControllerRef>,
    in_trigger: bool,
}

impl PileIr {
    pub(crate) fn new(effect: Effect) -> Self {
        debug_assert!(matches!(effect, Effect::SeparateIntoPiles { .. }));
        Self {
            source_text: String::new(),
            effect,
            actor: None,
            in_trigger: false,
        }
    }

    pub(crate) fn with_source(mut self, source_text: &str) -> Self {
        self.source_text = source_text.to_string();
        self
    }

    pub(crate) fn with_context(mut self, ctx: &ParseContext) -> Self {
        self.actor = ctx.actor.clone();
        self.in_trigger = ctx.in_trigger;
        self
    }

    /// Construct the trigger-context chain without lowering the root outside
    /// ordinary trigger lowering.
    pub(crate) fn effect_chain(&self, kind: AbilityKind) -> EffectChainIr {
        EffectChainIr::single_clause(
            &self.source_text,
            kind,
            parsed_clause(self.effect.clone()),
            None,
            self.actor.clone(),
            self.in_trigger,
        )
    }

    /// Compatibility lowering for non-trigger callers that still consume a
    /// lowered definition. Trigger parsing uses [`Self::effect_chain`] instead.
    pub(crate) fn into_ability(self, kind: AbilityKind) -> AbilityDefinition {
        AbilityDefinition::new(kind, self.effect)
    }
}

/// Modifier fields extracted during IR production.
///
/// These are consumed during lowering to set fields on the final
/// `TriggerDefinition` or compose with the body ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TriggerModifiers {
    /// CR 603.5: Some triggered abilities' effects are optional (they contain
    /// "may"). They go on the stack regardless; the choice is made on resolution.
    pub(crate) optional: bool,
    /// CR 118.12: "unless [player] pays {cost}" tax modifier.
    pub(crate) unless_pay: Option<UnlessPayModifier>,
    /// Intervening-if condition extracted from effect text.
    pub(crate) intervening_if: Option<TriggerCondition>,
    /// CR 608.2k: Trigger subject for pronoun resolution in effect text.
    pub(crate) trigger_subject: TargetFilter,
    /// CR 603.2: "for the first time ..." qualifier in the trigger event.
    pub(crate) first_time_limit: Option<FirstTimeLimit>,
    /// Constraint parsed from full trigger text.
    pub(crate) constraint: Option<TriggerConstraint>,
    /// Whether effect text contains "up to one".
    pub(crate) has_up_to: bool,
    /// Lowered effect text (after comma split), for `effect_adds_mana_to_triggering_player`.
    pub(crate) effect_lower: String,
    /// CR 109.4 + CR 603.7c: The relative-player scope the trigger condition
    /// established for its effect body (`TargetPlayer` for "deals [combat]
    /// damage to a player" / "attacks a player", `ParentTargetController` for
    /// damage-source-controller triggers, `ScopedPlayer` for scoped-phase
    /// triggers). Lowering reads this to rebind the body's `PlayerScope::Target`
    /// possessive quantities ("they lose half their life") to
    /// `PlayerScope::ScopedPlayer` for the `TargetPlayer` case, which resolves
    /// against the damaged/attacked player stamped on the resolving ability from
    /// the triggering event.
    pub(crate) relative_player_scope: Option<ControllerRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum FirstTimeLimit {
    EachTurn,
    EachOpponentTurn,
}
