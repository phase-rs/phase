//! Unified parsing context for pronoun and reference resolution.
//!
//! Flat superset of the former effect-chain and nom ParseContext structs.
//! All parser branches import from this single location (Phase 50, D-01).

use super::diagnostic::OracleDiagnostic;
use crate::types::ability::{ControllerRef, QuantityRef, TargetFilter, TargetSelectionMode};
use crate::types::zones::Zone;

/// Unified parsing context — threaded through all parser branches for
/// pronoun/reference resolution ("it", "that creature", "that many").
///
/// Callers set only the fields they need; all fields are Default-able (D-02).
#[derive(Debug, Clone, Default)]
pub(crate) struct ParseContext {
    /// The current subject (resolved target — "it", "that creature").
    pub subject: Option<TargetFilter>,
    /// Card name for self-reference (~) normalization.
    pub card_name: Option<String>,
    /// CR 707.9a + CR 603.1: Index of the printed trigger whose body is being
    /// parsed. Consumed by BecomeCopy "has this ability" arm.
    pub current_trigger_index: Option<usize>,
    /// CR 707.9a + CR 602.1: Index of the printed activated ability whose
    /// effect is being parsed. Consumed by BecomeCopy "has this ability" arm
    /// inside activated abilities (Thespian's Stage, Cytoshape, …).
    pub current_ability_index: Option<usize>,
    /// CR 701.21a + CR 608.2k: The actor performing the effect ("you", "an opponent").
    pub actor: Option<ControllerRef>,
    /// Resolved quantity reference ("that many", "that much").
    #[allow(dead_code)] // Retained for future nom combinator consumers (D-02).
    pub quantity_ref: Option<QuantityRef>,
    /// Whether we are inside a trigger effect (enables event context refs).
    #[allow(dead_code)] // Retained for future nom combinator consumers (D-02).
    pub in_trigger: bool,
    /// Whether we are inside a replacement effect.
    #[allow(dead_code)] // Retained for future nom combinator consumers (D-02).
    pub in_replacement: bool,
    /// Accumulated diagnostics for the current card parse (Phase 52, D-07).
    /// Replaces thread-local oracle_warnings accumulator.
    pub diagnostics: Vec<OracleDiagnostic>,
    /// CR 109.4 + CR 115.1: Relative-player scope for "that player controls"
    /// resolution inside trigger effects. Replaces thread-local oracle_target_scope.
    pub relative_player_scope: Option<ControllerRef>,
    /// CR 608.2c + CR 109.4: Count of `Effect::Choose { choice_type: Player }`
    /// clauses emitted so far in the current effect chain. Each "choose a
    /// player" / "choose a [second|third] player" clause increments this; the
    /// 0-based index of the *next* chosen player is the current value. Used to
    /// stamp `ControllerRef::ChosenPlayer { index }` so a dependent effect
    /// ("they put counters on a creature they control") binds to the player
    /// chosen by the immediately-preceding `Choose(Player)`.
    pub chosen_player_count: u8,
    /// CR 115.1 + CR 701.9b: Target selection mode for the most recent target
    /// phrase parsed via `parse_target_with_ctx`. The chunk loop in
    /// `parse_effect_chain_ir` snapshots this into the produced `ClauseIr` and
    /// resets it to `Chosen` for the next chunk so the marker is per-clause.
    pub target_selection_mode: TargetSelectionMode,
    /// CR 601.2c + CR 603.3d: When set, this player (not the controller) announces
    /// the most recent target phrase's target(s) at stack placement. Set when a
    /// targeted "of their choice" suffix is stripped from a `ScopedPlayer`-controlled
    /// filter ("destroy target X that player controls of their choice"). Snapshotted
    /// into the produced `ClauseIr` alongside `target_selection_mode`.
    pub target_chooser: Option<TargetFilter>,
    /// CR 601.2c + CR 608.2c: Ordered target slots declared by the current
    /// effect chain's "Choose target X and target Y" head. Index `i` is the
    /// filter announced for the `i`-th `target` word (slot 0 = A, slot 1 = B,
    /// …). Later clauses in the chain resolve definite anaphors ("that
    /// Equipment", "the chosen creature", "the artifact card") to
    /// `TargetFilter::ParentTargetSlot { index }` by matching the anaphor's noun
    /// phrase against these filters. Threaded across chunks via a chain
    /// loop-local and reset per effect chain in `parse_effect_chain_ir`
    /// (alongside the existing per-chain resets), so slots never leak across
    /// cards/abilities.
    pub declared_target_slots: Vec<TargetFilter>,
    /// CR 303.4 + CR 702.103: Typed self-reference for the enclosing card's
    /// attachment host. Set to `Some(TargetFilter::AttachedTo)` only when the
    /// card being parsed is an Aura or has the Bestow keyword (i.e. it can be
    /// attached to a permanent). When set, a `"that creature"` anaphor that the
    /// generic target parser resolves to `ParentTarget` is remapped to this
    /// host filter — for an Aura/bestow card "that creature" is the enchanted
    /// host (Springheart Nantuko's landfall copy-token). `None` for non-Aura
    /// cards, so `ParentTarget` keeps its chosen-target semantics (Twinflame).
    pub host_self_reference: Option<TargetFilter>,
    /// CR 603.4: Transient relative-clause filter parsed from a
    /// trigger subject ("an opponent **who controls F** draws a card"). Set by
    /// `parse_single_subject` when it consumes a "who controls <filter>"
    /// clause; consumed by `parse_trigger_condition`, which rewrites the
    /// filter's controller to `ControllerRef::TriggeringPlayer` and ANDs an
    /// `ObjectCount >= 1` intervening-if into the trigger's condition. Reset to
    /// `None` at the entry of every `parse_trigger_condition` call so stale
    /// clause state cannot leak across trigger lines.
    pub pending_trigger_subject_clause: Option<TargetFilter>,
    /// CR 608.2k: Source zone of the current ability's `AbilityCost::Exile`
    /// component, if any. Set by `parse_activated_ability_definition` after the
    /// cost is parsed and before the effect text is parsed, then restored after
    /// the ability. Consumed by `parse_cost_paid_object_reference` to
    /// disambiguate "the exiled card" — a cost-paid-object reference
    /// (`TargetFilter::CostPaidObject`) when the ability has a non-self exile
    /// cost, an effect-exiled tracked-set reference (`TrackedSet`) otherwise.
    pub current_ability_exile_cost_zone: Option<Zone>,
    /// CR 608.2c: The current effect-chain chunk has an earlier typed object
    /// referent that `ParentTarget` can legally bind to. Standalone clause
    /// parsing leaves this false so bare "it" defaults to SelfRef instead of
    /// inventing a parent target.
    pub parent_target_available: bool,
    /// CR 608.2c: The current effect-chain chunk's MOST-RECENT prior object
    /// referent is a just-created token (Token/CopyTokenOf/Populate), so a bare
    /// "it" anaphor in this chunk binds to that token (`TargetFilter::LastCreated`)
    /// rather than the ability source. Seeded only in the chunk loop via
    /// `chain_prior_referent_is_created_token`; a later explicit typed-target
    /// clause re-anchors "it" and clears it. Standalone and all other construction
    /// sites default `false` (`..Default::default()`), keeping bare "it" at
    /// `SelfRef` so non-token self-triggers ("Whenever ~ attacks, put a counter on
    /// it") are unaffected.
    pub token_created_in_chain: bool,
    /// CR 608.2c: Full lowercased effect-chain text for cross-clause features
    /// like cultivate/Final-Parting split-destination detection on a search
    /// clause that does not include the put-destination phrase in its chunk.
    pub effect_chain_full_lower: Option<String>,
    /// CR 608.2c + CR 601.2a: The chain's prior referent is an explicit target
    /// SELECTION (`Effect::TargetOnly`, e.g. Emry's "Choose target artifact
    /// card in your graveyard"), as distinct from an exile/impulse publisher
    /// (`ExileTop`, `ExileFromTopUntil`, …) whose "that card" anaphor is a
    /// tracked exile set. Only a chosen-target referent reroutes a "you may
    /// cast/play that card this turn" grant to `CastFromZone { ParentTarget }`;
    /// impulse publishers keep their `PlayFromExile { TrackedSet }` grant. This
    /// is a strict subset of `parent_target_available` — it stays false for the
    /// `ExileFromTopUntil` referent (Territorial Bruntar) that
    /// `parent_target_available` would otherwise include.
    pub parent_target_is_chosen: bool,
    /// CR 608.2c + CR 400.7: Source zone of the tracked set that a downstream
    /// "put those cards / put them onto the battlefield" anaphor (a
    /// `TargetFilter::TrackedSet`) must scan. Set by a producer clause that
    /// publishes its set from a NON-exile zone — e.g.
    /// `parse_for_each_player_choose_from_zone` derives `Some(Graveyard)` from
    /// the parsed `ChooseFromZone { zone: Graveyard }` so Breach the
    /// Multiverse's reanimation reads the chosen cards out of the graveyard
    /// rather than the impulse-default exile. Consumed by `parse_put_ast` when
    /// it lowers a `TrackedSet` put-onto-battlefield whose own clause text named
    /// no explicit origin; an impulse/cascade producer leaves this `None`, so
    /// the lowering keeps the exile default. Reset per effect chain in
    /// `parse_effect_chain_ir`.
    pub pending_tracked_set_origin: Option<Zone>,
    /// CR 701.42a: The partner card name extracted from a meld instigator's
    /// own/control gate ("if you both own and control [self] and a [type] named
    /// [partner], exile them, then meld them into [result]"). The gate is parsed
    /// as the trigger's intervening-if condition (carrying [partner] inside its
    /// `ControlCount` conjunct), but the meld EFFECT clause ("exile them, then
    /// meld them into [result]") must also stamp [partner] onto `Effect::Meld`.
    /// Set when the meld gate is recognized; consumed by the meld effect
    /// combinator. `None` for non-meld faces.
    pub pending_meld_partner: Option<String>,
    /// CR 107.4 + CR 202.1 + CR 603.4: The named color from a cast-trigger's
    /// "with one or more `<color>` mana symbol(s) in its mana cost" spell
    /// qualifier (Namor the Sub-Mariner). The qualifier is parsed into the
    /// trigger's `valid_card` (a `FilterProp::ManaSymbolCount`), but the EFFECT
    /// clause "create that many tokens" must back-reference the cast spell's
    /// colored-symbol count rather than the generic `EventContextAmount` (which
    /// has no SpellCast amount and resolves to 0). Set from the finalized
    /// condition/qualifier text before the effect body parses; consumed by the
    /// token-count override in `oracle_effect::token`. `None` for triggers
    /// without a colored-pip qualifier.
    pub pending_mana_symbol_count_color: Option<crate::types::mana::ManaColor>,
    /// CR 116.2b + CR 708.7: True while parsing the body of an explicit granted
    /// activated ability (a quoted `"{cost}: ..."` granted to another object).
    /// In that context, a head clause of "turn this/~ creature face up" is the
    /// printed resolving effect of the granted ability (Etrata, Deadly
    /// Fugitive's "{2}{U}{B}: Turn this creature face up. ..."), NOT the
    /// rule-based morph/disguise special action. The imperative parser uses this
    /// flag to lower such a clause to `Effect::TurnFaceUp { SelfRef }` instead of
    /// rejecting the self-referential subject (which it must keep rejecting for
    /// top-level morph reminder/special-action text). Set by
    /// `parse_quoted_ability`; defaults to `false` everywhere else.
    pub in_granted_activated_ability: bool,
}

impl ParseContext {
    /// Resolve third-person player pronouns ("they", "their") against the
    /// nearest parser context that introduced a player referent.
    pub fn third_person_player_controller_ref(&self) -> Option<ControllerRef> {
        self.relative_player_scope
            .clone()
            .or_else(|| self.actor.clone())
    }

    /// Push a diagnostic (replaces oracle_warnings::push_diagnostic).
    pub fn push_diagnostic(&mut self, d: OracleDiagnostic) {
        if matches!(d, OracleDiagnostic::TargetFallback { .. })
            && self.diagnostics.iter().any(|existing| existing == &d)
        {
            return;
        }
        self.diagnostics.push(d);
    }

    /// Execute `f` with a temporary relative-player scope, restoring the prior
    /// value on return. Replaces thread-local ScopeGuard RAII pattern.
    #[allow(dead_code)] // Available for nested-scope uses (e.g., nested triggers).
    pub fn with_player_scope<R>(
        &mut self,
        scope: ControllerRef,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prev = self.relative_player_scope.take();
        self.relative_player_scope = Some(scope);
        let result = f(self);
        self.relative_player_scope = prev;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_fallback_diagnostics_are_idempotent() {
        let mut ctx = ParseContext::default();
        let diagnostic = OracleDiagnostic::TargetFallback {
            context: "search-filter-suffix unmatched".into(),
            text: "with an unsupported clause".into(),
            line_index: 0,
        };

        ctx.push_diagnostic(diagnostic.clone());
        ctx.push_diagnostic(diagnostic);

        assert_eq!(ctx.diagnostics.len(), 1);
    }

    #[test]
    fn distinct_target_fallback_diagnostics_are_preserved() {
        let mut ctx = ParseContext::default();

        ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
            context: "search-filter-suffix unmatched".into(),
            text: "first clause".into(),
            line_index: 0,
        });
        ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
            context: "search-filter-suffix unmatched".into(),
            text: "second clause".into(),
            line_index: 0,
        });

        assert_eq!(ctx.diagnostics.len(), 2);
    }
}
