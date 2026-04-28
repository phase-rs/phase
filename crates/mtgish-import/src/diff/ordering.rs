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
    // ----- ChooseFromZoneConstraint -----
    // Categories form a multiset of allowed types.
    (
        ("ChooseFromZoneConstraint", "categories"),
        OrderingClass::SetEquivalent,
    ),
    // ----- ChoiceType / mana production -----
    // Player-facing string options. The player picks one; order is the
    // display order in the prompt UI. Treat as positional so the diff
    // surfaces "we reordered the menu" as a real divergence.
    (("ChoiceType", "options"), OrderingClass::OrderSignificant),
    // ----- ManaProduction colors -----
    // CR 106.1: the color set produced is unordered.
    (("ManaProduction", "colors"), OrderingClass::SetEquivalent),
    (
        ("ManaProduction", "color_options"),
        OrderingClass::SetEquivalent,
    ),
    (("ManaProduction", "options"), OrderingClass::SetEquivalent),
    // ----- GameRestriction allowed_zones -----
    (
        ("GameRestriction", "allowed_zones"),
        OrderingClass::SetEquivalent,
    ),
    // ----- FilterProp variants with embedded Vec<...> -----
    (("FilterProp", "kinds"), OrderingClass::SetEquivalent),
    (("FilterProp", "props"), OrderingClass::SetEquivalent),
    (("FilterProp", "zones"), OrderingClass::SetEquivalent),
    // ----- QuantityRef devotion colors -----
    (("QuantityRef", "colors"), OrderingClass::SetEquivalent),
    (("QuantityRef", "card_types"), OrderingClass::SetEquivalent),
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
    // -----------------------------------------------------------------
    // TODO(manifest): the following `(carrier, field)` pairs were
    // surfaced by `tests/manifest_coverage.rs` and require classification
    // by an agent that knows the rules-meaning of each list.
    //
    // Each entry should land here with an `OrderingClass` and a one-line
    // CR-grounded justification. Do NOT silence the coverage test by
    // adding placeholder `OrderSignificant` entries blindly — the safe
    // default already applies on lookup miss; what's missing is the
    // *intentional* classification.
    //
    //   (AbilityCondition, conditions)        — AndAll/OrAny over conditions; likely SetEquivalent
    //   (CardFace, color_override)            — color set on the face; SetEquivalent
    //   (CastPermissionConstraint, exiled_misses) — runtime ID list; ConditionallySignificant
    //   (ContinuousModification, colors)      — CR 105.1: unordered color set
    //   (CopiableValues, color)               — copy-effect characteristic; SetEquivalent
    //   (CopiableValues, keywords)            — copy-effect keyword set; SetEquivalent
    //   (Effect, additional_modifications)    — verify per-variant whether positional
    //   (Effect, branches)                    — modal branch order matches mode order; OrderSignificant
    //   (Effect, cards)                       — depends on variant (search list vs sequenced reveal)
    //   (Effect, categories)                  — distinct-types categories; SetEquivalent
    //   (Effect, choices)                     — player-facing choice list; OrderSignificant
    //   (Effect, colors)                      — color set; SetEquivalent
    //   (Effect, enter_with_counters)         — counter spec list; SetEquivalent (sum semantics)
    //   (Effect, extra_keywords)              — granted keyword set; SetEquivalent
    //   (Effect, grants)                      — granted-ability list; CR 613 layer-managed: SetEquivalent
    //   (Effect, keywords)                    — keyword set; SetEquivalent
    //   (Effect, per_choice_effect)           — per-mode effect list; OrderSignificant
    //   (Effect, remove_types)                — CR 305.7: type set; SetEquivalent
    //   (Effect, restrictions)                — independent restrictions; SetEquivalent
    //   (Effect, results)                     — die-result branches; OrderSignificant
    //   (Effect, static_abilities)            — granted statics; SetEquivalent
    //   (Effect, statics)                     — alias of static_abilities; SetEquivalent
    //   (Effect, supertypes)                  — CR 205.4: type set; SetEquivalent
    //   (Effect, triggers)                    — granted triggers; SetEquivalent
    //   (Effect, types)                       — CR 300.1 type set; SetEquivalent
    //   (KeywordAction, paid_creature_ids)    — runtime ID list; OrderSignificant (selection order)
    //   (ReplacementCondition, subtypes)      — subtype set; SetEquivalent
    //   (ReplacementDefinition, ensure_token_specs) — Manufactor: SetEquivalent (per CR 616 idempotence)
    //   (ResolvedAbility, distribution)       — DistributionUnit list; OrderSignificant (target index)
    //   (ResolvedAbility, targets)            — target list, indexed positionally; OrderSignificant
    //   (TriggerCause, core_types)            — CR 300.1 type set; SetEquivalent
    //   (TriggerCondition, conditions)        — AndAll/OrAny composite; SetEquivalent
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
