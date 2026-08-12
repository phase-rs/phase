//! Ordering-Significance Manifest.
//!
//! Every `Vec<T>` field in the engine's typed card-data hierarchy belongs
//! to one of three classes, depending on whether order is rules-meaningful:
//!
//! - **`OrderSignificant`** — Order encodes meaning. Diffs compare element
//!   by element, position by position. Reordering produces divergences.
//!   Examples: `mode_abilities` (mode order is the player-facing label
//!   order); `_chain` of replacement effects (CR 616 ordering layers).
//!
//! - **`SetEquivalent`** — Order is incidental. Diffs treat as multisets:
//!   any reordering is equivalent. Examples: filter conjunctions/disjunctions
//!   (`Or { filters }`, `And { filters }`); independent activation
//!   restrictions; trigger constraints stacked on the same trigger.
//!
//! - **`ConditionallySignificant`** — Context-dependent. Documented per
//!   entry. Rare. The classifier treats these as positional unless the
//!   per-entry note specifies otherwise.
//!
//! ## Why a flat const slice, not a HashMap
//! - Lookup is O(N) over a small, capped table (~30 entries today,
//!   hundreds at saturation). Profiling has not justified a hash table,
//!   and a const slice can be checked at compile time.
//! - BTreeMap throughout the diff path; see `diff/mod.rs`.
//!
//! ## Authoritative scope
//! Only `Vec<T>` (and `im::Vector<T>`) fields belong here. `Option<T>`,
//! scalars, and structs are positional by definition. The
//! `manifest_coverage.rs` test enforces that every `Vec<T>` field in the
//! engine type files has an entry below.

/// Whether a list field's element order carries meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderingClass {
    /// Order changes meaning — compared positionally.
    OrderSignificant,
    /// Order is incidental — compared as multiset.
    SetEquivalent,
    /// Rare — context-dependent. Documented per entry.
    ConditionallySignificant,
}

/// Manifest entries: `((struct_or_enum_variant_name, field_name), class)`.
///
/// `struct_or_enum_variant_name` is the *type* the field is declared on
/// (e.g., `"AbilityDefinition"`), not the wrapping module. For enum
/// variants with named fields (e.g., `TargetFilter::Or { filters }`),
/// use the enum name as the carrier (`"TargetFilter"`) — the manifest
/// coverage test resolves enum-variant fields to their parent enum.
///
/// Field names are the literal Rust identifier as it appears on the
/// declaration. Serde renames are NOT applied here; the canonicalizer
/// operates on JSON keys, but the manifest is keyed by source-level
/// names so the `syn`-based coverage test can verify exhaustiveness
/// without reasoning about `#[serde(rename)]`.
pub const ORDERING_MANIFEST: &[((&str, &str), OrderingClass)] = &[
    // ----- AbilityDefinition -----
    // Modes are presented to the player as an ordered list ("Choose one —
    // first / second / third"). Reordering renames mode 2 → mode 1 from
    // the player's perspective.
    (
        ("AbilityDefinition", "mode_abilities"),
        OrderingClass::OrderSignificant,
    ),
    // Activation restrictions are independent constraints ANDed together.
    // CR 602.5: order doesn't change which activations are legal.
    (
        ("AbilityDefinition", "activation_restrictions"),
        OrderingClass::SetEquivalent,
    ),
    // Target constraints are independent legality predicates ANDed together.
    // Reordering does not alter the target set accepted by validation.
    (
        ("AbilityDefinition", "target_constraints"),
        OrderingClass::SetEquivalent,
    ),
    // ----- TargetFilter -----
    // Or/And conjunctions are commutative (CR 700.2 / set semantics).
    // Keyed by both the enum name (for the manifest_coverage test) and
    // the JSON discriminant (for the runtime classifier, which only sees
    // the `type` tag and not the parent enum).
    (("TargetFilter", "filters"), OrderingClass::SetEquivalent),
    (("Or", "filters"), OrderingClass::SetEquivalent),
    (("And", "filters"), OrderingClass::SetEquivalent),
    // ----- TypedFilter -----
    // Independent type filters ANDed together; order has no rules effect.
    (
        ("TypedFilter", "type_filters"),
        OrderingClass::SetEquivalent,
    ),
    (("TypedFilter", "properties"), OrderingClass::SetEquivalent),
    // ----- TriggerDefinition -----
    // Disjunctive zone set (CR 603.10a "library and/or graveyard"). Set.
    (
        ("TriggerDefinition", "origin_zones"),
        OrderingClass::SetEquivalent,
    ),
    // Active zones for the trigger to function in (CR 603.6f). Set.
    (
        ("TriggerDefinition", "trigger_zones"),
        OrderingClass::SetEquivalent,
    ),
    // CR 603.2: disjunctive zone-change clauses — the trigger fires if the
    // event matches ANY clause, so clause order is incidental. Set.
    (
        ("TriggerDefinition", "zone_change_clauses"),
        OrderingClass::SetEquivalent,
    ),
    // Player actions enumerate equivalent triggers; order is incidental.
    (
        ("TriggerDefinition", "player_actions"),
        OrderingClass::SetEquivalent,
    ),
    // ----- StaticDefinition -----
    // CR 613: continuous modifications stack in layer order, BUT layer
    // assignment is by `ContinuousModification` variant — within a single
    // static the listed modifications are independent and re-sorted by
    // the layer system at apply time. Treat as set-equivalent.
    (
        ("StaticDefinition", "modifications"),
        OrderingClass::SetEquivalent,
    ),
    // CR 113.6 + CR 113.6b: list of zones the static functions in. Set.
    (
        ("StaticDefinition", "active_zones"),
        OrderingClass::SetEquivalent,
    ),
    // ----- ModalChoice -----
    // Mode descriptions are positional (matched 1:1 to mode_abilities).
    (
        ("ModalChoice", "mode_descriptions"),
        OrderingClass::OrderSignificant,
    ),
    // Modal selection constraints are independent — ANDed.
    (("ModalChoice", "constraints"), OrderingClass::SetEquivalent),
    // Per-mode mana costs (Spree). Positional — index matches the mode.
    (
        ("ModalChoice", "mode_costs"),
        OrderingClass::OrderSignificant,
    ),
    // ----- CardFace -----
    // Top-level card content lists. Each is a multiset of independent
    // abilities/triggers/etc.; the engine evaluates them per CR 614/603
    // with its own ordering rules at apply time, so order in the data
    // doesn't matter.
    (("CardFace", "keywords"), OrderingClass::SetEquivalent),
    (("CardFace", "abilities"), OrderingClass::SetEquivalent),
    (("CardFace", "triggers"), OrderingClass::SetEquivalent),
    (
        ("CardFace", "static_abilities"),
        OrderingClass::SetEquivalent,
    ),
    (("CardFace", "replacements"), OrderingClass::SetEquivalent),
    (("CardFace", "color_identity"), OrderingClass::SetEquivalent),
    (
        ("CardFace", "casting_restrictions"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("CardFace", "casting_options"),
        OrderingClass::SetEquivalent,
    ),
    // Parse warnings are diagnostic strings; not rules-meaningful.
    // Order is set-equivalent for diff purposes.
    (("CardFace", "parse_warnings"), OrderingClass::SetEquivalent),
    // ----- CardMetadata -----
    // Display/catalog identifiers generated from sets; only membership matters.
    (
        ("CardMetadata", "related_token_ids"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("CardMetadata", "source_printing_ids"),
        OrderingClass::SetEquivalent,
    ),
    // ----- ChooseFromZoneConstraint -----
    // Categories form a multiset of allowed types.
    (
        ("ChooseFromZoneConstraint", "categories"),
        OrderingClass::SetEquivalent,
    ),
    // Search selection qualities are conjunctive constraints on the chosen set;
    // their order does not change legality.
    (
        ("SearchSelectionConstraint", "qualities"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("SearchSelectionConstraint", "filters"),
        OrderingClass::SetEquivalent,
    ),
    // ----- ChoiceType / mana production -----
    // Player-facing string options. The player picks one; order is the
    // display order in the prompt UI. Treat as positional so the diff
    // surfaces "we reordered the menu" as a real divergence.
    (("ChoiceType", "options"), OrderingClass::OrderSignificant),
    // CR 106.1: the set of colors barred from a restricted color choice
    // (e.g. a Gate's "of a color of your choice" minus already-chosen
    // colors) is unordered — only membership matters.
    (("ChoiceType", "excluded"), OrderingClass::SetEquivalent),
    // ----- ManaProduction colors -----
    // CR 106.1: the color set produced is unordered.
    (("ManaProduction", "colors"), OrderingClass::SetEquivalent),
    (
        ("ManaProduction", "color_options"),
        OrderingClass::SetEquivalent,
    ),
    (("ManaProduction", "options"), OrderingClass::SetEquivalent),
    // ----- ProhibitedActivity allowed_zones -----
    (
        ("ProhibitedActivity", "allowed_zones"),
        OrderingClass::SetEquivalent,
    ),
    // ----- FilterProp variants with embedded Vec<...> -----
    (("FilterProp", "costs"), OrderingClass::SetEquivalent),
    (("FilterProp", "kinds"), OrderingClass::SetEquivalent),
    (("FilterProp", "props"), OrderingClass::SetEquivalent),
    (("FilterProp", "zones"), OrderingClass::SetEquivalent),
    // ----- QuantityRef devotion colors -----
    (("QuantityRef", "colors"), OrderingClass::SetEquivalent),
    (("QuantityRef", "card_types"), OrderingClass::SetEquivalent),
    // ----- QuantityRef ObjectCountDistinct dedup-key set -----
    // CR 201.2: `qualities` is the set of shared characteristics used to
    // deduplicate counted objects (e.g. `[Name]` for "different names",
    // `[ManaValue]` for "different mana values"). The order of qualities
    // doesn't change which objects coincide — it's a multiset key — so
    // diffs can ignore ordering safely.
    (("QuantityRef", "qualities"), OrderingClass::SetEquivalent),
    // ----- StaticCondition.colors / nested condition list -----
    (("StaticCondition", "colors"), OrderingClass::SetEquivalent),
    (
        ("StaticCondition", "conditions"),
        OrderingClass::SetEquivalent,
    ),
    // ----- AbilityCost composite -----
    (("AbilityCost", "costs"), OrderingClass::OrderSignificant),
    // ----- ParsedCondition counts / subtypes -----
    (("ParsedCondition", "counts"), OrderingClass::SetEquivalent),
    (
        ("ParsedCondition", "subtypes"),
        OrderingClass::SetEquivalent,
    ),
    // ----- StaticMode (SuppressTriggers / type-changing) -----
    (("StaticMode", "events"), OrderingClass::SetEquivalent),
    (("StaticMode", "core_types"), OrderingClass::SetEquivalent),
    // ----- Composite conditions -----
    (
        ("AbilityCondition", "conditions"),
        OrderingClass::SetEquivalent,
    ),
    (("AbilityCondition", "phases"), OrderingClass::SetEquivalent),
    (
        ("ParsedCondition", "conditions"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("TriggerCondition", "conditions"),
        OrderingClass::SetEquivalent,
    ),
    (("TriggerCondition", "phases"), OrderingClass::SetEquivalent),
    // ----- AdditionalCost -----
    // CR 702.33b: Kicker cost positions are referenced as first/second kicker.
    (("AdditionalCost", "costs"), OrderingClass::OrderSignificant),
    // ----- CardFace color override -----
    (("CardFace", "color_override"), OrderingClass::SetEquivalent),
    // ----- Cascade cleanup state -----
    (
        ("CastPermissionConstraint", "exiled_misses"),
        OrderingClass::SetEquivalent,
    ),
    // ----- Continuous/copiable color and keyword sets -----
    (
        ("ContinuousModification", "colors"),
        OrderingClass::SetEquivalent,
    ),
    // CR 205.1a + CR 613.1d: `SetCardTypes` replaces the object's entire core
    // card-type set (Layer 4). A type set is unordered — only membership
    // matters — so reordering is incidental. Set.
    (
        ("ContinuousModification", "core_types"),
        OrderingClass::SetEquivalent,
    ),
    (("CopiableValues", "color"), OrderingClass::SetEquivalent),
    (("CopiableValues", "keywords"), OrderingClass::SetEquivalent),
    // ----- CleaveVariant (alternate-text face; mirrors CardFace's lists) -----
    (("CleaveVariant", "abilities"), OrderingClass::SetEquivalent),
    (("CleaveVariant", "triggers"), OrderingClass::SetEquivalent),
    (
        ("CleaveVariant", "replacements"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("CleaveVariant", "static_abilities"),
        OrderingClass::SetEquivalent,
    ),
    // ----- Effect embedded lists -----
    (
        ("Effect", "additional_modifications"),
        OrderingClass::SetEquivalent,
    ),
    // `ChooseFromZone.additional_zones` unions extra search zones with the
    // primary `zone` (e.g. "choose a card from your hand or graveyard").
    // The choosable pool is a zone union — order is incidental. Set.
    (("Effect", "additional_zones"), OrderingClass::SetEquivalent),
    (("Effect", "branches"), OrderingClass::OrderSignificant),
    (("Effect", "cards"), OrderingClass::SetEquivalent),
    (("Effect", "categories"), OrderingClass::SetEquivalent),
    (("Effect", "choices"), OrderingClass::OrderSignificant),
    (("Effect", "colors"), OrderingClass::SetEquivalent),
    (
        ("Effect", "enter_with_counters"),
        OrderingClass::SetEquivalent,
    ),
    (("Effect", "followed_by"), OrderingClass::OrderSignificant),
    (("Effect", "extra_keywords"), OrderingClass::SetEquivalent),
    (("Effect", "grants"), OrderingClass::SetEquivalent),
    (("Effect", "keywords"), OrderingClass::SetEquivalent),
    (
        ("Effect", "per_choice_effect"),
        OrderingClass::OrderSignificant,
    ),
    (("Effect", "remove_types"), OrderingClass::SetEquivalent),
    (("Effect", "restrictions"), OrderingClass::SetEquivalent),
    (("Effect", "results"), OrderingClass::OrderSignificant),
    // CR 701.23a: `SearchLibrary.source_zones` is a disjunctive zone set
    // ("graveyard, hand, and/or library") — order carries no rules meaning.
    (("Effect", "source_zones"), OrderingClass::SetEquivalent),
    (("Effect", "static_abilities"), OrderingClass::SetEquivalent),
    (("Effect", "statics"), OrderingClass::SetEquivalent),
    (("Effect", "supertypes"), OrderingClass::SetEquivalent),
    (("Effect", "triggers"), OrderingClass::SetEquivalent),
    (("Effect", "types"), OrderingClass::SetEquivalent),
    // ----- Keyword/runtime selections -----
    (
        ("KeywordAction", "paid_creature_ids"),
        OrderingClass::OrderSignificant,
    ),
    // ----- Quantity / replacement / resolved ability lists -----
    (("QuantityExpr", "exprs"), OrderingClass::SetEquivalent),
    (
        ("ReplacementCondition", "subtypes"),
        OrderingClass::SetEquivalent,
    ),
    // Conjunctive condition list (And/Or) — independent predicates, order has
    // no rules effect (mirrors AbilityCondition/StaticCondition conjunctions).
    (
        ("ReplacementCondition", "conditions"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("ReplacementDefinition", "ensure_token_specs"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("ResolvedAbility", "distribution"),
        OrderingClass::OrderSignificant,
    ),
    (
        ("ResolvedAbility", "targets"),
        OrderingClass::OrderSignificant,
    ),
    // CR 608.2c: players chosen mid-resolution, in chain order.
    // `ControllerRef::ChosenPlayer { index }` reads this list positionally,
    // so reordering re-binds which player a given index resolves to.
    (
        ("ResolvedAbility", "chosen_players"),
        OrderingClass::OrderSignificant,
    ),
    (
        ("SpellContext", "kickers_paid"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("SpellContext", "controller_controlled_as_cast"),
        OrderingClass::SetEquivalent,
    ),
    // ----- Trigger cause filters -----
    (("TriggerCause", "core_types"), OrderingClass::SetEquivalent),
    // ----- AbilityBlockReason -----
    // CR 602.5: the field's own contract is "sorted, deduped permanents" — a
    // canonicalized set of independently-prohibiting sources. Set.
    (
        ("AbilityBlockReason", "sources"),
        OrderingClass::SetEquivalent,
    ),
    // ----- AbilityCondition (RevealedHasCardType) -----
    // CR 205.1a: disjunctive core-type match against the revealed card — only
    // membership decides the outcome. Matches `QuantityRef::card_types`. Set.
    (
        ("AbilityCondition", "card_types"),
        OrderingClass::SetEquivalent,
    ),
    // ----- AbilityDefinitionDe (serde mirror of AbilityDefinition) -----
    // The deserialization mirror must classify identically to the type it
    // reconstructs, or the same list would diff differently depending on which
    // shape the JSON deserialized through.
    (
        ("AbilityDefinitionDe", "activation_restrictions"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("AbilityDefinitionDe", "mode_abilities"),
        OrderingClass::OrderSignificant,
    ),
    (
        ("AbilityDefinitionDe", "target_constraints"),
        OrderingClass::SetEquivalent,
    ),
    // ----- CardFace / CardMetadata catalog lists -----
    // CR 717.1: the set of lit-up roll numbers on an Attraction variant. Which
    // numbers are lit decides the roll; their listing order does not. Set.
    (
        ("CardFace", "attraction_lights"),
        OrderingClass::SetEquivalent,
    ),
    // Alchemy spellbook — the fixed candidate pool a draft effect selects from.
    // Membership defines the pool; MTGJSON's listing order is incidental. Set.
    (("CardMetadata", "spellbook"), OrderingClass::SetEquivalent),
    // ----- CastingPermission -----
    // CR 613.1d: enters-with continuous modifications carried on a cast grant.
    // Layer assignment is by modification variant, so the layer system re-sorts
    // them at apply time. Mirrors `StaticDefinition::modifications`. Set.
    (
        ("CastingPermission", "enters_with_modifications"),
        OrderingClass::SetEquivalent,
    ),
    // ----- Effect embedded lists (continued) -----
    // CR 122.1: entry-time counters gated per-filter. Counter placement is
    // cumulative and filter-keyed, so reordering the riders cannot change the
    // resulting counter set. Mirrors `Effect::enter_with_counters`. Set.
    (
        ("Effect", "conditional_enter_with_counters"),
        OrderingClass::SetEquivalent,
    ),
    // CR 707.9: "except …" modifications applied to a created copy. Same
    // layer-resorted rationale as `StaticDefinition::modifications`. Set.
    (
        ("Effect", "copy_modifications"),
        OrderingClass::SetEquivalent,
    ),
    (("Effect", "modifications"), OrderingClass::SetEquivalent),
    // Zone union searched for candidate cards — membership, not order. Set.
    (("Effect", "zones"), OrderingClass::SetEquivalent),
    // ----- FaceDownProfile -----
    // CR 708.2a + CR 205.1a: the core types and subtypes a face-down permanent
    // is given. Type/subtype sets are unordered. Set.
    (
        ("FaceDownProfile", "extra_core_types"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("FaceDownProfile", "subtypes"),
        OrderingClass::SetEquivalent,
    ),
    // ----- ManaSpendRestriction -----
    // CR 106.6: the field's own contract is a DISJUNCTION of cost criteria —
    // any one match satisfies it, so order cannot matter. Set.
    (
        ("ManaSpendRestriction", "criteria"),
        OrderingClass::SetEquivalent,
    ),
    // ----- ModalChoice -----
    // CR 700.2i: per-mode pawprint weights are index-parallel with the modes
    // themselves — `mode_pawprints[i]` is the cost of mode `i`. Reordering
    // reprices every mode. Positional.
    (
        ("ModalChoice", "mode_pawprints"),
        OrderingClass::OrderSignificant,
    ),
    // ----- NotedManaPayment -----
    // CR 106.6: the multiset of mana types spent on one activation. Consumers
    // ask "was type T among them", never "which came first". Set.
    (("NotedManaPayment", "types"), OrderingClass::SetEquivalent),
    // ----- PerpetualModification -----
    // CR 205.1a + CR 113.2c: granted keyword and creature-subtype sets. Each
    // keyword instance functions independently (CR 113.2c). Set.
    (
        ("PerpetualModification", "creature_subtypes"),
        OrderingClass::SetEquivalent,
    ),
    (
        ("PerpetualModification", "keywords"),
        OrderingClass::SetEquivalent,
    ),
    // ----- QuantityRef -----
    // CR 305.2a: origin zones narrowing a land-play count — a membership test
    // over the play's recorded origin. Set.
    (("QuantityRef", "from_zones"), OrderingClass::SetEquivalent),
    // ----- ReplacementCondition -----
    // CR 111.1: core types the proposed token must overlap. Overlap is
    // symmetric in the listing order. Set.
    (
        ("ReplacementCondition", "core_types"),
        OrderingClass::SetEquivalent,
    ),
    // ----- During-resolution cast state (Cascade / Discover / Ripple) -----
    // CR 608.2g: dig cards that were not the hit. CR 702.60a bottoms the
    // non-cast reveals "in any order", so their recorded order is not
    // rules-meaningful. Mirrors `CastPermissionConstraint::exiled_misses`. Set.
    (
        ("ResolutionCastCleanup", "exiled_misses"),
        OrderingClass::SetEquivalent,
    ),
    // CR 607.2a: the "exiled this way" batch a re-offer's candidate set is
    // confined to — a membership restriction. Set.
    (
        ("ResolutionCastSuccessAction", "member_pool"),
        OrderingClass::SetEquivalent,
    ),
    // CR 702.60a: Ripple may cast ANY NUMBER of the same-named reveals, so the
    // remaining pool is a candidate set, not a queue with meaningful order. Set.
    (
        ("ResolutionCastSuccessAction", "remaining_hits"),
        OrderingClass::SetEquivalent,
    ),
    // Zone union searched for free-cast candidates. Matches `Effect::zones`. Set.
    (
        ("ResolutionCastSuccessAction", "zones"),
        OrderingClass::SetEquivalent,
    ),
    // ----- ResolvedAbility (continued) -----
    // Ids stripped from this ability's own candidate lists — read as a
    // membership test, never as a positional referent. Set.
    (
        ("ResolvedAbility", "cost_paid_object_ids"),
        OrderingClass::SetEquivalent,
    ),
    // CR 700.2b: one definition per mode; mode order is the player-facing
    // label order. Mirrors `AbilityDefinition::mode_abilities`. Positional.
    (
        ("ResolvedAbility", "mode_abilities"),
        OrderingClass::OrderSignificant,
    ),
    // CR 700.2d: the field's own contract is "in the printed instruction order
    // used to resolve them" — order IS the resolution sequence. Positional.
    (
        ("ResolvedAbility", "selected_mode_labels"),
        OrderingClass::OrderSignificant,
    ),
    // CR 115.1 + CR 601.2c: independent legality predicates ANDed together.
    // Mirrors `AbilityDefinition::target_constraints`. Set.
    (
        ("ResolvedAbility", "target_constraints"),
        OrderingClass::SetEquivalent,
    ),
    // CR 400.7 + CR 603.7c: incarnation pins for the referents in `targets`,
    // which is itself positional — pin `i` guards target `i`. Reordering
    // re-binds each pin to a different target. Positional.
    (
        ("ResolvedAbility", "target_incarnations"),
        OrderingClass::OrderSignificant,
    ),
    // ----- SpellContext (continued) -----
    // CR 113.2c + CR 601.2b: the non-kicker analogue of `kickers_paid`, read
    // the same way (was cost C paid, how many times). Set.
    (
        ("SpellContext", "additional_cost_payments"),
        OrderingClass::SetEquivalent,
    ),
    // CR 700.2d: mode indices are stored ASCENDING with repeats — the ordering
    // is a normalization, and the multiset of chosen modes is the meaning.
    // (Player-facing resolution order lives in `selected_mode_labels`.) Set.
    (
        ("SpellContext", "chosen_modes"),
        OrderingClass::SetEquivalent,
    ),
    // ----- StaticMode (continued) -----
    // CR 702.122a: which crew-like keyword actions a power/toughness
    // contribution modifier applies to. Membership. Set.
    (("StaticMode", "actions"), OrderingClass::SetEquivalent),
    // CR 122.2: destination zones excluded from counter persistence — the
    // "any zone other than [zones]" set. Set.
    (
        ("StaticMode", "excluded_zones"),
        OrderingClass::SetEquivalent,
    ),
    // ----- TriggerCause / TriggerDefinition -----
    // CR 603.6a: the field's own contract marks these qualifiers disjunctive. Set.
    (("TriggerCause", "qualifiers"), OrderingClass::SetEquivalent),
    // CR 106.1: the produced-mana set a "taps for {C}" trigger requires at
    // least one match in — an overlap test. Set.
    (
        ("TriggerDefinition", "taps_for_mana_produced"),
        OrderingClass::SetEquivalent,
    ),
    // ----- TriggerOccurrenceState -----
    // Grant instances are addressed exclusively by `producer` key (duplicate
    // producers are rejected outright) and by `instance` id — never by index.
    // A keyed set. Set.
    (
        ("TriggerOccurrenceState", "active_grants"),
        OrderingClass::SetEquivalent,
    ),
];

/// Look up the ordering class for a `(carrier, field)` pair.
/// Returns `None` if the field is not in the manifest — the diff binary
/// treats unknowns as `OrderSignificant` (the safer default: surface
/// reordering rather than silently accept it).
pub fn lookup_ordering(carrier: &str, field: &str) -> Option<OrderingClass> {
    ORDERING_MANIFEST
        .iter()
        .find(|((c, f), _)| *c == carrier && *f == field)
        .map(|(_, class)| *class)
}

/// Look up an ordering class by field name only when every manifest entry
/// for that field agrees. This is a conservative fallback for JSON structs
/// that do not carry a serde `type` discriminator, such as top-level
/// `CardFace` and nested `AbilityDefinition` objects.
pub fn lookup_ordering_by_field(field: &str) -> Option<OrderingClass> {
    let mut class = None;
    for ((_, candidate_field), candidate_class) in ORDERING_MANIFEST {
        if *candidate_field != field {
            continue;
        }
        match class {
            None => class = Some(*candidate_class),
            Some(existing) if existing == *candidate_class => {}
            Some(_) => return None,
        }
    }
    class
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_known_entry() {
        assert_eq!(
            lookup_ordering("AbilityDefinition", "mode_abilities"),
            Some(OrderingClass::OrderSignificant)
        );
        assert_eq!(
            lookup_ordering("TargetFilter", "filters"),
            Some(OrderingClass::SetEquivalent)
        );
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert_eq!(lookup_ordering("NoSuchType", "no_field"), None);
    }

    #[test]
    fn lookup_by_field_only_requires_unambiguous_class() {
        assert_eq!(
            lookup_ordering_by_field("activation_restrictions"),
            Some(OrderingClass::SetEquivalent)
        );
        assert_eq!(lookup_ordering_by_field("options"), None);
    }

    #[test]
    fn manifest_has_no_duplicate_keys() {
        let mut seen: std::collections::BTreeSet<(&str, &str)> = std::collections::BTreeSet::new();
        for ((c, f), _) in ORDERING_MANIFEST {
            assert!(
                seen.insert((*c, *f)),
                "duplicate manifest entry: ({}, {})",
                c,
                f
            );
        }
    }
}
