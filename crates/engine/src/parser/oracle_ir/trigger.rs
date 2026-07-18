//! Trigger IR types.
//!
//! `TriggerIr` represents the pre-lowering intermediate representation of a
//! parsed trigger line. IR production extracts the trigger condition, body, and
//! modifiers; lowering assembles them into the final `TriggerDefinition`.

use serde::Serialize;

use super::effect_chain::EffectChainIr;
use crate::types::ability::{
    AbilityDefinition, ControllerRef, ModalChoice, TargetFilter, TriggerCondition,
    TriggerConstraint, TriggerDefinition, UnlessPayModifier,
};
use crate::types::triggers::TriggerMode;

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
    /// CR 700.2: An inline modal's marker clause and its already-lowered mode
    /// bodies. The marker still flows through ordinary trigger-chain lowering;
    /// this payload carries the modal metadata no clause can represent.
    Modal(Box<ModalIr>),
    /// Pre-lowered ability (vote blocks produce `AbilityDefinition` directly).
    PreLowered(Box<AbilityDefinition>),
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
