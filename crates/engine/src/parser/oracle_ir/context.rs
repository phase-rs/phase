//! Unified parsing context for pronoun and reference resolution.
//!
//! Flat superset of the former effect-chain and nom ParseContext structs.
//! All parser branches import from this single location (Phase 50, D-01).

use super::diagnostic::OracleDiagnostic;
use crate::types::ability::{
    ControllerRef, MultiTargetSpec, PlayerFilter, PtValue, QuantityExpr, QuantityRef, TargetFilter,
    TargetSelectionMode,
};
use crate::types::zones::Zone;

/// Parser-only lookahead for token body clauses split across adjacent sentences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenPtFollowup {
    PowerToughness { power: PtValue, toughness: PtValue },
}

/// Parser-internal scope flag: whether the trigger CONDITION currently being
/// parsed is a printed (card-text) trigger or a DELAYED trigger created from a
/// resolving effect chain. This is parser scaffolding, not a rule implementation,
/// so it carries no CR annotation. Anaphoric subjects that only bind as delayed
/// back-references to the creating ability — the gendered pronoun "he"/"she" (→
/// `SelfRef`) and the plural set "those creatures"/"any of those creatures" (→
/// `ParentTarget`) — are recognized ONLY under `Delayed`, so a standalone printed
/// trigger that happens to contain those words stays coverage-honest (`Unknown`)
/// instead of binding its source to `Any`. A typed scope rather than a bare bool
/// per the codebase's "typed enum over bool" convention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TriggerConditionScope {
    /// A trigger printed on the card. Delayed-only anaphoric subjects stay `Unknown`.
    #[default]
    Printed,
    /// A delayed trigger condition created from a resolving effect chain
    /// (set by `try_parse_whenever_this_turn`).
    Delayed,
}

/// Unified parsing context — threaded through all parser branches for
/// pronoun/reference resolution ("it", "that creature", "that many").
///
/// Callers set only the fields they need; all fields are Default-able (D-02).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    ///
    /// Consumed by `oracle_replacement::parse_oneshot_target_source_prevent`
    /// (the Awe Strike one-shot target-source prevention branch): inside a
    /// trigger body "that creature" is an event-context anaphor resolved by
    /// the trigger machinery, not a target-source capture, so the branch must
    /// not claim trigger-body text (Ria Ivor keeps its fall-through shapes).
    pub in_trigger: bool,
    /// Whether we are inside a replacement effect.
    #[allow(dead_code)] // Retained for future nom combinator consumers (D-02).
    pub in_replacement: bool,
    /// Parser-internal scope: whether the trigger CONDITION being parsed is printed
    /// card text or a DELAYED trigger created from a resolving effect chain. Gates
    /// delayed-only anaphoric subject resolution; see [`TriggerConditionScope`].
    pub trigger_condition_scope: TriggerConditionScope,
    /// CR 608.2k + CR 601.2a: Event object that bare object pronouns in the
    /// current trigger body ("it", "them") should bind to. Spell-cast triggers
    /// set this to `TriggeringSource` so "Whenever you cast a spell, put it ..."
    /// moves the spell on the stack, not the trigger source or a parent target.
    pub object_pronoun_ref: Option<TargetFilter>,
    /// CR 608.2c (rules of English — number agreement) + CR 608.2k + CR 406.6:
    /// Antecedent for bare PLURAL object pronouns ("them"/"themselves") in the
    /// current trigger body, introduced by a plural noun phrase in the trigger's
    /// intervening-if ("if there are cards exiled with ~, put THEM …" → the
    /// linked-exile pool). Deliberately separate from the singular
    /// `object_pronoun_ref`: a plural antecedent must never capture a singular
    /// "it", whose antecedent is the nearer chained object (River Song's Diary:
    /// "choose one of them at random. You may cast IT" — "it" is the chosen card,
    /// not the pool).
    pub plural_object_pronoun_ref: Option<TargetFilter>,
    /// Accumulated diagnostics for the current card parse (Phase 52, D-07).
    /// Replaces thread-local oracle_warnings accumulator.
    pub diagnostics: Vec<OracleDiagnostic>,
    /// CR 109.4 + CR 115.1: Relative-player scope for "that player controls"
    /// resolution inside trigger effects. Replaces thread-local oracle_target_scope.
    pub relative_player_scope: Option<ControllerRef>,
    /// CR 608.2c + CR 109.4: Transient per-chunk `player_scope` lifted from a
    /// subject-predicate whose EFFECT carries no player field to stamp the
    /// subject onto (the fieldless `Effect::Investigate` — Declaration in Stone's
    /// "That player investigates"). `inject_subject_target` drops such a subject
    /// silently, so `lower_subject_predicate_ast` records it here instead; the
    /// effect-chain loop folds it into the chunk's `player_scope` local (→
    /// `ClauseIr.player_scope` → `AbilityDefinition.player_scope`) so resolution
    /// fans the effect out to the anchored player rather than the caster. Set and
    /// consumed within a single chunk parse; never serialized.
    pub pending_player_scope: Option<PlayerFilter>,
    /// CR 608.2c + CR 701.16a: Transient per-chunk `repeat_for` lifted from a
    /// fieldless-effect subject-predicate that carries a "for each <filter> …
    /// this way" SUFFIX count (Declaration in Stone's "investigate for each
    /// nontoken creature exiled this way"). `Effect::Investigate` has no count
    /// slot and the suffix `for each` handler is CopySpell-only, so the count is
    /// otherwise dropped. `lower_subject_predicate_ast` records it here; the
    /// effect-chain loop folds it into the chunk's `repeat_for` (→
    /// `AbilityDefinition.repeat_for`), composing with `player_scope` via the
    /// resolver's outermost-repeat driver. Set and consumed within a single
    /// chunk parse; never serialized.
    pub pending_repeat_for: Option<QuantityExpr>,
    /// CR 601.2c + CR 115.4: The announced target count recovered by
    /// `parse_each_of_target_distribution` for a "… damage to each of ⟨N⟩
    /// ⟨noun⟩" head, produced by the SAME parse that produced the target
    /// filter. CR 601.2c fixes the count; CR 115.4 fixes the class for the
    /// bare-plural `targets` noun.
    ///
    /// Single authority for that seam, with an explicit set/reset lifecycle:
    /// `lower_imperative_clause` clears this as the very first statement of its
    /// body and `take()`s it immediately after `parse_imperative_effect`, so it
    /// never outlives one clause and can never attach to an unrelated
    /// `DealDamage`. The reset is required because `parse_imperative_effect` is
    /// also called from sites that never consume this field (the shared-`ctx`
    /// `try_parse_reanimate_self_and_target` sub-parse) and from speculative
    /// sub-parses that mutate `ctx` and then discard their result.
    ///
    /// Two seams need more than the reset, because the reset alone is wrong in
    /// both directions:
    /// - The speculative recognizers that can abandon a parse mid-way
    ///   (`try_parse_multi_target_damage_chain`,
    ///   `try_parse_radiance_color_fanout_damage`) run against a cloned
    ///   `ParseContext` and commit with `*ctx = tentative_ctx` only on success,
    ///   so an abandoned attempt cannot leak a count forward.
    /// - `try_split_damage_compound` re-enters `lower_imperative_clause` for the
    ///   continuation, and that nested frame clears this field on entry. It
    ///   therefore saves the primary clause's count before the sub-parse and
    ///   restores it after, alongside the `target_chooser` save/restore.
    ///
    /// Set and consumed within a single chunk parse; never serialized.
    pub pending_damage_multi_target: Option<MultiTargetSpec>,
    /// CR 608.2c + CR 109.4: Count of `Effect::Choose { choice_type: Player }`
    /// clauses emitted so far in the current effect chain. Each "choose a
    /// player" / "choose a [second|third] player" clause increments this; the
    /// 0-based index of the *next* chosen player is the current value. Used to
    /// stamp `ControllerRef::ChosenPlayer { index }` so a dependent effect
    /// ("they put counters on a creature they control") binds to the player
    /// chosen by the immediately-preceding `Choose(Player)`.
    pub chosen_player_count: u8,
    /// CR 608.2d + CR 608.2c: Committed `ChoiceType` from a preceding
    /// `Effect::Choose` clause, threaded forward so a later "an opponent guesses
    /// which [value] you chose" clause embeds the printed domain in
    /// `GuessSubject::CommittedChoice`. The choose and the guess sit in the same
    /// ability resolution (CR 608.2c in-order instructions), not two distinct
    /// printed abilities (CR 607.2d). Mirrors `chosen_player_count` as a
    /// parse-time accumulator (not serialized).
    pub pending_choice_type: Option<crate::types::ability::ChoiceType>,
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
    /// component, if any. Set by `parse_activated_ability_ir` after the
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
    /// CR 608.2c + CR 406.6 + CR 607.2a: Whether the current effect-chain
    /// chunk has an EARLIER clause (in the SAME resolution chain) that
    /// produces an exile — a `ChangeZone`/`ChangeZoneAll` to `Zone::Exile`,
    /// `ExileTop`, `Dig { destination: Some(Zone::Exile), .. }`,
    /// `ExileFromTopUntil`, or any other exile-producer shape recognized by
    /// `chain_clause_is_exile_producer`. When true, a singular "the exiled
    /// card" anaphor in a LATER clause of this chain refers to that
    /// same-chain exile and keeps its pre-existing same-chain binding
    /// (`TrackedSet{0}` / `ParentTarget`). When false, the referenced exile
    /// happened in an earlier, SEPARATELY-RESOLVED ability (e.g. an ETB
    /// Imprint or synthesized Hideaway ETB), so the anaphor must bind
    /// durably via `TargetFilter::ExiledBySource` (CR 607.1 linked
    /// abilities) instead. Seeded per top-level `parse_effect_chain_ir` call
    /// from the chain-local clause accumulator; defaults `false` via
    /// `derive(Default)` so standalone clause parsing is unaffected.
    pub chain_has_prior_exile_producer: bool,
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
    /// CR 608.2c + CR 301.5 + CR 303.4: An EARLIER clause in this same effect
    /// chain turns the ability's own SOURCE into an attachable object — an Aura
    /// (CR 303.4: an enchantment with the Aura subtype, attached via its enchant
    /// ability) or an Equipment (CR 301.5: an artifact with the Equipment
    /// subtype). This is the animate-then-attach class, of which the 12 Licids
    /// are the canonical members ("This creature loses this ability and becomes
    /// an Aura enchantment with enchant creature. Attach **it** to target
    /// creature"). The source is the only object the chain has made attachable,
    /// so the following clause's bare "it" attachment anaphor names it
    /// (`TargetFilter::SelfRef`) rather than the ability's chosen target.
    /// Seeded only in the chunk loop via
    /// `parser::oracle_effect::chain_source_becomes_attachment`; every other
    /// construction site defaults `false` (`..Default::default()`), so an attach
    /// clause whose chain never animated its source keeps its pre-existing
    /// `parse_target` binding (Embercleave's Equipment-ETB `ParentTarget`; Aura
    /// Graft's chained-referent `ParentTarget`).
    pub source_becomes_attachment_in_chain: bool,
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
    /// CR 608.2c + CR 601.2a: the chain's prior chosen-target FILTER — the
    /// `Effect::TargetOnly { target }` filter that `parent_target_is_chosen`
    /// reports the presence of (Emry's / Conduit of Worlds' "Choose target …
    /// card in your graveyard"). It binds a downstream "you may cast that card"
    /// anaphor to the chosen object; timing comes independently from the
    /// instruction plus its duration, never from this target's zone. Seeded
    /// alongside `parent_target_is_chosen` in the chunk loop; `None` on every
    /// standalone and non-chosen parse.
    pub chain_prior_chosen_target: Option<TargetFilter>,
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
    /// CR 608.2c + CR 608.2h + CR 111.3: Immediate next-clause lookahead for
    /// token body characteristics printed in a separate sentence ("Its power
    /// is equal to this creature's power ..."). This is parser-local and
    /// one-shot per chunk; standalone token parsing keeps rejecting creature
    /// tokens whose P/T is not specified by the current clause or this marker.
    pub token_pt_followup: Option<TokenPtFollowup>,
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
    /// CR 400.1/400.2 + CR 601.2a + CR 608.2c: The player-referencing target of
    /// an EARLIER same-chain `Effect::RevealHand` clause ("look at that
    /// player's hand" / "reveal their hand"), e.g. `TriggeringPlayer`. When a
    /// LATER clause in the SAME chain references "them"/"those cards" in a
    /// cast-permission clause (Silent-Blade Oni: "You may cast a spell from
    /// among those cards without paying its mana cost"), the anaphor binds to
    /// THIS revealed player's hand instead of the exile-only
    /// `TargetFilter::ExiledBySource` default — no exile ever happened, so
    /// `ExiledBySource` would resolve to an empty set and silently swallow the
    /// cast permission. Mirrors `chain_has_prior_exile_producer`'s same-chain
    /// scan, but for the hand-reveal producer shape. `None` when no such
    /// producer exists in this chain, or during standalone clause parsing.
    pub chain_prior_hand_reveal_target: Option<TargetFilter>,
    /// CR 608.2c: The object POPULATION established by a mass ("each …") effect in
    /// an earlier clause of this same chain — Ardbert, Warrior of Darkness:
    /// "put a +1/+1 counter on each legendary creature you control. They gain
    /// vigilance until end of turn."
    ///
    /// Distinct from [`Self::parent_target_available`], which tracks a CHOSEN
    /// referent that `TargetFilter::ParentTarget` binds to (see
    /// `has_typed_target_widened`'s single-target whitelist). A mass effect
    /// chooses nothing, so an anaphor referring back to its population cannot use
    /// `ParentTarget` — it must inherit the population FILTER itself. `None` when
    /// no such producer exists in this chain, or during standalone clause parsing.
    pub chain_prior_mass_population: Option<TargetFilter>,
    /// True when the SAME chain's most recent producer was a self-library peek
    /// (look at the top N cards of YOUR library without exiling/moving them).
    /// The bare "from among them" cast anaphor that follows must route to the
    /// one-shot during-resolution cast (CR 608.2g), not the exile-and-grant
    /// lingering path. Mirrors `chain_has_prior_exile_producer`.
    // CR 608.2g + CR 701.20e
    pub chain_prior_self_library_peek: bool,
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
        // Both variants can be pushed from a combinator that a speculative `alt`
        // re-enters on a discarded alternative, so an identical entry is noise
        // rather than signal.
        if matches!(
            d,
            OracleDiagnostic::TargetFallback { .. } | OracleDiagnostic::IgnoredRemainder { .. }
        ) && self.diagnostics.iter().any(|existing| existing == &d)
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
