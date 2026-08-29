//! Trigger IR types.
//!
//! `TriggerIr` represents the pre-lowering intermediate representation of a
//! parsed trigger line. IR production extracts the trigger condition, body, and
//! modifiers; lowering assembles them into the final `TriggerDefinition`.

use serde::Serialize;

use super::ast::parsed_clause;
use super::context::ParseContext;
use super::effect_chain::{DieResultBranchIr, EffectChainIr, ModalModeIr};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ControllerRef,
    Effect, ModalChoice, TargetFilter, TargetSelectionMode, TriggerCondition, TriggerConstraint,
    TriggerDefinition, UnlessPayModifier,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum TriggerNodeIr {
    /// Native parsed trigger decomposition, lowered only at the document seam.
    Parsed(Box<TriggerIr>),
    /// Already-assembled definition from a recognizer that builds its own.
    /// `lower_trigger_node_ir` returns it untouched.
    Assembled {
        definition: Box<TriggerDefinition>,
        /// The recognizer's own input text — provenance only. Document lowering
        /// derives spans and fragments from the emitting line, never from here.
        source_text: String,
    },
}

impl TriggerNodeIr {
    /// Wrap a recognizer-produced trigger definition for source-ordered emission.
    pub(crate) fn from_definition(source_text: &str, definition: TriggerDefinition) -> Self {
        Self::Assembled {
            definition: Box::new(definition),
            source_text: source_text.to_string(),
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
    /// CR 706.3b result-table rows for the terminal die roll in this trigger.
    /// They remain IR until trigger-body lowering attaches them before finalization.
    pub(crate) die_results: Vec<DieResultBranchIr>,
    /// Complete context established from the trigger condition before its body
    /// is parsed. Nested modal modes must start from this same seed so event
    /// anaphora (notably spell-cast "it") keep their trigger-body meaning.
    #[serde(skip)]
    pub(crate) body_context: ParseContext,
}

impl TriggerIr {
    /// Whether the body ends in the typed die-roll node that owns a result table.
    pub(crate) fn has_terminal_roll_die(&self) -> bool {
        match &self.body {
            Some(TriggerBody::EffectChain(chain)) => effect_chain_has_terminal_roll_die(chain),
            // CR 706.3b: the table belongs to the printed die-roll instruction,
            // not the reflexive `WhenYouDo` body it creates. A modal reflexive
            // body contains only the mode marker, so looking there drops rows
            // for a parent such as "roll a d20. When you do, choose one".
            Some(TriggerBody::Reflexive(reflexive)) => {
                effect_chain_has_terminal_roll_die(&reflexive.effect_chain)
                    || match &reflexive.parent {
                        ReflexiveParent::MayPay {
                            payment_chain: Some(chain),
                            ..
                        } => effect_chain_has_terminal_roll_die(chain),
                        ReflexiveParent::MayPay {
                            payment_chain: None,
                            ..
                        } => false,
                        ReflexiveParent::Mandatory { instruction } => {
                            effect_chain_has_terminal_roll_die(instruction)
                        }
                    }
            }
            Some(TriggerBody::Modal(_))
            | Some(TriggerBody::Vote(_))
            | Some(TriggerBody::Pile(_))
            | None => false,
        }
    }
}

/// Whether this exact chain ends at the typed die roll that owns its following
/// result table. Parent and reflexive chains are distinct printed instructions,
/// so callers use this to preserve the table on whichever one owns the roll.
pub(crate) fn effect_chain_has_terminal_roll_die(chain: &EffectChainIr) -> bool {
    chain
        .clauses
        .last()
        .is_some_and(|clause| matches!(clause.parsed.effect, Effect::RollDie { .. }))
}

/// The body of a trigger. Whole-body recognizers retain their typed payloads
/// here so trigger lowering owns all root-level transforms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum TriggerBody {
    /// Normal effect chain — lowering calls `lower_effect_chain_ir`.
    EffectChain(EffectChainIr),
    /// CR 603.12: A printed parent instruction and the reflexive
    /// effect that follows when its event occurred.
    Reflexive(Box<ReflexiveParentIr>),
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

/// CR 603.12: A reflexive "when you do" body together with the printed parent
/// instruction it rides on.
///
/// `parent` is the axis that used to be assumed rather than represented: this
/// node only existed for the `"you may <instruction>. When you do"` surface, so
/// a mandatory parent had nowhere to live. Keeping the parent as a parameterized
/// field rather than a sibling node means the reflexive lowering has exactly one
/// shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReflexiveParentIr {
    /// How the parent instruction is printed and offered.
    pub(crate) parent: ReflexiveParent,
    /// The connector between the parent instruction and its dependent body.
    pub(crate) connector: AbilityCondition,
    /// The reflexive body that follows the connector.
    pub(crate) effect_chain: EffectChainIr,
    /// CR 700.2b: modal metadata when the reflexive body is a mode choice.
    pub(crate) modal: Option<ModalIr>,
}

/// CR 603.12: the two printed forms a reflexive parent can take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ReflexiveParent {
    /// `"you may <instruction>. When you do, …"` — a resolution-time offer the
    /// controller may decline (Caesar, Legion's Emperor). `payment_chain`
    /// carries the printed instruction as an effect chain when one parsed; otherwise
    /// lowering synthesizes an `Effect::PayCost` from `cost`.
    MayPay {
        cost: AbilityCost,
        payment_chain: Option<EffectChainIr>,
    },
    /// `"<instruction>. When you do, …"` — the instruction is not an offer, it
    /// simply happens (Cemetery Desecrator). The trigger parser
    /// already lowered the printed instruction into this chain, so lowering
    /// reuses it instead of re-parsing the same words a second time.
    Mandatory { instruction: EffectChainIr },
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
    pub(crate) modes: Vec<ModalModeIr>,
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

    /// Compatibility lowering for callers outside the native spell router.
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
    /// CR 608.2d: Event-relative player explicitly named by the root optional
    /// subject, after any intervening-if wrapper has been removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) optional_player: Option<TargetFilter>,
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
    /// CR 109.4: The relative-player scope the trigger condition
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
