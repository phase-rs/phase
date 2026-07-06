//! Target phrase combinators for Oracle text parsing.
//!
//! Parses "target creature", "target creature or planeswalker you control", etc.
//! into typed `TargetFilter` values using nom 8.0 combinators.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::space1;
use nom::combinator::{map, not, opt, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::error::{oracle_err, OracleError, OracleResult};
use super::primitives::parse_color;
use crate::parser::oracle_util::{parse_subtype, GRANTING_SELF_PLACEHOLDER, OUTLAW_SUBTYPES};
use crate::types::ability::{
    Comparator, ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::Supertype;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

/// Parse a type phrase into a `TargetFilter`.
///
/// Handles: optional "non" prefix, optional supertype, optional color prefix,
/// core type(s) joined by " or ", and optional controller suffix. This is the
/// nom equivalent of `oracle_target::parse_type_phrase`.
pub fn parse_type_phrase(input: &str) -> OracleResult<'_, TargetFilter> {
    // Optional "non" prefix (consumed separately from type negation)
    let (rest, non_prefix) = opt(parse_non_prefix).parse(input)?;

    // Optional supertype prefix ("legendary", "basic", "snow")
    let (rest, supertype_opt) = opt(parse_supertype_prefix).parse(rest)?;

    // Optional color-quality prefix ("colorless ", "monocolored ", "multicolored ")
    let (rest, color_quality_opt) = opt(parse_color_quality_prefix).parse(rest)?;

    // Optional WUBRG color prefix (mutually exclusive with color-quality)
    let (rest, color_opt) = if color_quality_opt.is_some() {
        (rest, None)
    } else {
        opt(parse_color_prefix).parse(rest)?
    };

    // Core type(s) joined by " or "
    let (rest, types) = parse_type_list(rest)?;

    // Optional controller suffix
    let (rest, controller) = opt(preceded(space1, parse_controller_suffix)).parse(rest)?;

    let mut filter = build_type_filter(types, color_opt, supertype_opt, controller);

    if let Some(prop) = color_quality_opt {
        if let TargetFilter::Typed(ref mut tf) = filter {
            tf.properties.push(prop);
        }
    }

    // Wrap in Non if "non" prefix was present
    if non_prefix.is_some() {
        filter = match filter {
            TargetFilter::Typed(tf) => {
                if tf.type_filters.len() == 1 {
                    // Wrap the single type in Non
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Non(Box::new(
                            tf.type_filters.into_iter().next().unwrap(),
                        ))],
                        controller: tf.controller,
                        properties: tf.properties,
                    })
                } else {
                    // Wrap the AnyOf in Non
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::AnyOf(
                            tf.type_filters,
                        )))],
                        controller: tf.controller,
                        properties: tf.properties,
                    })
                }
            }
            other => other,
        };
    }

    Ok((rest, filter))
}

/// Parse a "non" prefix: "non" or "non-" followed by implicit word boundary.
fn parse_non_prefix(input: &str) -> OracleResult<'_, &str> {
    alt((tag("non-"), tag("non"))).parse(input)
}

/// CR 205.4a: Parse a bare supertype word ("legendary", "basic", "snow",
/// "world", "ongoing") without consuming any trailing boundary. Shared building
/// block for both the adjective-prefix form (`parse_supertype_prefix`, word +
/// space) and trailing relative-clause forms ("that aren't legendary", where
/// the word is at end-of-string). Callers that need a boundary apply their own
/// check. Covers the full CR 205.4a set the engine `Supertype` enum models
/// (Host is set-supplemental / not CR 205.4a and is excluded here). None of the
/// five words is a prefix of another, so `alt` ordering is boundary-safe.
pub fn parse_supertype_word(input: &str) -> OracleResult<'_, Supertype> {
    alt((
        value(Supertype::Legendary, tag("legendary")),
        value(Supertype::Basic, tag("basic")),
        value(Supertype::Snow, tag("snow")),
        value(Supertype::World, tag("world")),
        value(Supertype::Ongoing, tag("ongoing")),
    ))
    .parse(input)
}

/// Parse a supertype prefix ("legendary ", "basic ", "snow ") consuming trailing space.
pub fn parse_supertype_prefix(input: &str) -> OracleResult<'_, Supertype> {
    let (rest, st) = parse_supertype_word(input)?;
    let (rest, _) = space1.parse(rest)?;
    Ok((rest, st))
}

/// Parse color-quality adjective prefixes: "colorless ", "monocolored ",
/// "multicolored ".
fn parse_color_quality_prefix(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            },
            tag("colorless "),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 1,
            },
            tag("monocolored "),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            },
            tag("multicolored "),
        ),
    ))
    .parse(input)
}

/// Parse a color word followed by a space, consuming both.
fn parse_color_prefix(input: &str) -> OracleResult<'_, ManaColor> {
    let (rest, c) = parse_color(input)?;
    let (rest, _) = space1.parse(rest)?;
    Ok((rest, c))
}

/// Parse a controller suffix: "you control", "an opponent controls",
/// "target player controls".
///
/// CR 109.4 + CR 115.1: "target player controls" generates a filter referencing
/// a chosen player target; the enclosing ability must surface a companion
/// TargetFilter::Player slot so the player is selected as part of target
/// declaration.
pub fn parse_controller_suffix(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag("you control")),
        value(ControllerRef::Opponent, tag("an opponent controls")),
        value(ControllerRef::Opponent, tag("your opponents control")),
        value(ControllerRef::TargetPlayer, tag("target player controls")),
        // CR 109.4 + CR 102.2 / CR 102.3: opponent-constrained target-player scope.
        value(
            ControllerRef::TargetOpponent,
            tag("target opponent controls"),
        ),
    ))
    .parse(input)
}

/// Parse a list of type filters joined by " or ".
fn parse_type_list(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (rest, first) = parse_type_filter_word(input)?;
    let mut types = vec![first];

    let mut remaining = rest;
    loop {
        if let Ok((r, _)) = tag::<_, _, OracleError<'_>>(" or ").parse(remaining) {
            if let Ok((r2, t)) = parse_type_filter_word(r) {
                types.push(t);
                remaining = r2;
                continue;
            }
        }
        break;
    }

    Ok((remaining, types))
}

/// Parse a single type filter word (singular or plural).
///
/// Uses a manual lookup for core/card types to avoid deep nom `alt` nesting which causes
/// stack overflow in debug builds, then falls back to the shared subtype table.
pub fn parse_type_filter_word(input: &str) -> OracleResult<'_, TypeFilter> {
    // Table of (prefix, TypeFilter) — longest-match-first within shared prefixes.
    static TYPE_WORDS: &[(&str, TypeFilter)] = &[
        ("creatures", TypeFilter::Creature),
        ("creature", TypeFilter::Creature),
        ("artifacts", TypeFilter::Artifact),
        ("artifact", TypeFilter::Artifact),
        ("enchantments", TypeFilter::Enchantment),
        ("enchantment", TypeFilter::Enchantment),
        ("instants", TypeFilter::Instant),
        ("instant", TypeFilter::Instant),
        ("sorceries", TypeFilter::Sorcery),
        ("sorcery", TypeFilter::Sorcery),
        ("planeswalkers", TypeFilter::Planeswalker),
        ("planeswalker", TypeFilter::Planeswalker),
        ("lands", TypeFilter::Land),
        ("land", TypeFilter::Land),
        // Plural before singular (longest-match-first): the word-boundary guard
        // rejects "battle" + trailing 's', and BATTLE_SUBTYPES has no "Battle"
        // entry, so the plural must be an explicit head-noun word here.
        ("battles", TypeFilter::Battle),
        ("battle", TypeFilter::Battle),
        ("permanents", TypeFilter::Permanent),
        ("permanent", TypeFilter::Permanent),
        ("cards", TypeFilter::Card),
        ("card", TypeFilter::Card),
        // CR 112.1: a spell is a card on the stack — "spell"/"spells" → Card.
        ("spells", TypeFilter::Card),
        ("spell", TypeFilter::Card),
    ];

    // CR 700.12: "outlaw"/"outlaws" is a head noun for the Assassin, Mercenary,
    // Pirate, Rogue, and/or Warlock creature types. Tried before the bare-prefix
    // TYPE_WORDS scan and the subtype table because it expands to a disjunction
    // rather than a single subtype. The word-boundary guard prevents "outlawry"
    // (and similar prefixed words) from matching.
    if let Ok((rest, tf)) = parse_outlaw_type(input) {
        return Ok((rest, tf));
    }

    for &(word, ref tf) in TYPE_WORDS {
        if let Some(rest) = input.strip_prefix(word) {
            // Word-boundary guard (mirrors parse_outlaw_type below and
            // parse_subtype_entry in oracle_util.rs): a head-noun type word must
            // be followed by end-of-input or a non-alphanumeric char. Without
            // this, a TYPE_WORD that prefixes a longer subtype shadows it — e.g.
            // "land" eating "lander" or "spell" eating "spellshaper" instead of
            // falling through to the boundary-guarded subtype table.
            if rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric()) {
                return Ok((rest, tf.clone()));
            }
        }
    }

    if let Some((subtype, consumed)) = parse_subtype(input) {
        return Ok((&input[consumed..], TypeFilter::Subtype(subtype)));
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// CR 700.12: Parse the "outlaw"/"outlaws" head noun into a disjunction of the
/// Assassin, Mercenary, Pirate, Rogue, and Warlock creature types. Matches the
/// plural form first (longest-match), then requires a non-alphanumeric word
/// boundary so words like "outlawry" never match.
fn parse_outlaw_type(input: &str) -> OracleResult<'_, TypeFilter> {
    let (rest, _) = alt((tag("outlaws"), tag("outlaw"))).parse(input)?;
    match rest.chars().next() {
        // Word boundary: end of input or non-alphanumeric follower.
        None => {}
        Some(c) if !c.is_alphanumeric() => {}
        _ => {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Fail,
            )))
        }
    }
    let any_of = OUTLAW_SUBTYPES
        .iter()
        .map(|s| TypeFilter::Subtype((*s).to_string()))
        .collect();
    Ok((rest, TypeFilter::AnyOf(any_of)))
}

/// Parse a self-reference from Oracle text: "~", "it", "itself",
/// "this creature", "this permanent", "this spell", "this enchantment",
/// "this artifact".
///
/// Returns `TargetFilter::SelfRef` when a self-reference is recognized.
///
/// CR 201.5a: a granted body's by-name reference to its GRANTING object is
/// masked to [`GRANTING_SELF_PLACEHOLDER`] by `normalize_card_name_refs` and
/// recognized here (first alt) as `TargetFilter::GrantingObject` — distinct
/// from the host `SelfRef`. This single edit covers the effect-target channel
/// (`parse_target` → here) for "Return/Destroy/gains control of <self>".
pub fn parse_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        parse_granting_object_ref,
        value(TargetFilter::SelfRef, tag("~")),
        parse_it_self_reference,
        // CR 201.5: "itself" is a self-reference to the object the ability is on.
        parse_itself_self_reference,
        value(TargetFilter::SelfRef, tag("this creature")),
        value(TargetFilter::SelfRef, tag("this permanent")),
        value(TargetFilter::SelfRef, tag("this spell")),
        value(TargetFilter::SelfRef, tag("this card")),
        value(TargetFilter::SelfRef, tag("this enchantment")),
        value(TargetFilter::SelfRef, tag("this aura")),
        value(TargetFilter::SelfRef, tag("this artifact")),
        value(TargetFilter::SelfRef, tag("this land")),
        value(TargetFilter::SelfRef, tag("this attraction")),
    ))
    .parse(input)
}

/// CR 201.5a: Single recognition authority for the granting-object by-name
/// self-reference placeholder emitted by the quote masker in
/// `normalize_card_name_refs`. Used as the first alt in both
/// [`parse_self_reference`] (effect-target channel) and
/// [`parse_cost_self_reference`] (cost channel).
pub fn parse_granting_object_ref(input: &str) -> OracleResult<'_, TargetFilter> {
    value(TargetFilter::GrantingObject, tag(GRANTING_SELF_PLACEHOLDER)).parse(input)
}

/// CR 201.5 / CR 201.5a: Shared self-reference combinator for *cost* positions
/// ("Sacrifice <self>", "Exile <self>", "Return <self> to its owner's hand").
/// Recognizes the granter placeholder → `GrantingObject` and the host tokens
/// (`~`, "cardname") → `SelfRef`, in one authority so every cost site routes
/// through the same logic instead of an ad-hoc per-site `tag("~")` copy.
pub fn parse_cost_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        parse_granting_object_ref,
        value(TargetFilter::SelfRef, tag("~")),
        value(TargetFilter::SelfRef, tag("cardname")),
    ))
    .parse(input)
}

/// Parse "it" as a self-reference, requiring a word boundary after "it"
/// to prevent false matches on words like "item", "iterate".
fn parse_it_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("it").parse(input)?;
    match rest.chars().next() {
        None | Some(' ' | ',' | ';' | '.' | ':' | ')' | '/' | '\'' | '"') => {
            Ok((rest, TargetFilter::SelfRef))
        }
        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

/// Parse "itself" as a self-reference, requiring a word boundary after "itself"
/// to prevent false matches on words like "itselfless".
fn parse_itself_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("itself").parse(input)?;
    match rest.chars().next() {
        None => Ok((rest, TargetFilter::SelfRef)),
        Some(c) if !c.is_alphanumeric() => Ok((rest, TargetFilter::SelfRef)),
        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

/// Parse an event context reference from Oracle text.
///
/// CR 506.2 + CR 603.7c: "that attacking player" — the player who declared
/// attackers in the triggering `AttackersDeclared` event (Ellie, Brick Master;
/// Breena, the Demagogue).
pub fn parse_attacking_player_event_ref(input: &str) -> OracleResult<'_, TargetFilter> {
    value(TargetFilter::TriggeringPlayer, tag("that attacking player")).parse(input)
}

/// CR 506.3d + CR 508.1: "that opponent" inside an attack trigger's effect —
/// the opponent being attacked in the triggering event (token enters attacking
/// that opponent; Adeline / Ellie class).
pub fn parse_attacked_opponent_event_ref(input: &str) -> OracleResult<'_, TargetFilter> {
    value(TargetFilter::DefendingPlayer, tag("that opponent")).parse(input)
}

/// Matches "that spell", "that player", "that creature", "defending player",
/// "the defending player", "that card", "that permanent".
/// Returns a `TargetFilter` for the referenced entity.
pub fn parse_event_context_ref(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        // Longest-match-first: "that spell's controller" before "that spell"
        value(
            TargetFilter::TriggeringSpellController,
            tag("that spell's controller"),
        ),
        value(
            TargetFilter::TriggeringSpellOwner,
            tag("that spell's owner"),
        ),
        value(TargetFilter::TriggeringSource, tag("that spell")),
        value(TargetFilter::TriggeringSource, tag("that creature")),
        value(
            TargetFilter::TriggeringSource,
            terminated(
                tag("that permanent"),
                not(preceded(
                    tag(" "),
                    alt((tag("or player"), tag("or a player"))),
                )),
            ),
        ),
        value(TargetFilter::TriggeringSource, tag("that card")),
        parse_attacking_player_event_ref,
        // CR 506.3d: "that opponent" before the shorter "that player" arm.
        parse_attacked_opponent_event_ref,
        value(TargetFilter::TriggeringPlayer, tag("that player")),
        // CR 506.3d: "defending player" / "the defending player"
        value(TargetFilter::DefendingPlayer, tag("the defending player")),
        value(TargetFilter::DefendingPlayer, tag("defending player")),
        // CR 603.7c + CR 109.4: "the attacking player" on a DamageReceived
        // trigger — the controller of the creature that dealt combat damage
        // (Contested Game Ball). Distinct from "that attacking player" (an
        // attack-declared referent → TriggeringPlayer): the wanted player here
        // is the controller of the triggering damage *source*, not the
        // damaged player. Ordered before the bare "the player" arm.
        value(
            TargetFilter::TriggeringSourceController,
            tag("the attacking player"),
        ),
        // CR 608.2k: "the player" in trigger context is synonymous with
        // "that player" — anaphoric reference to the triggering player.
        // Ordered after "the defending player" so longest-match-first is
        // preserved for the specific defending-player phrasing.
        value(TargetFilter::TriggeringPlayer, tag("the player")),
    ))
    .parse(input)
}

/// Parse a "stack-object" target phrase — the disjunction of spells and/or
/// activated/triggered abilities currently on the stack that a counter effect
/// (or a retarget effect) can name as a target.
///
/// CR 701.6a: "To counter a spell or ability means to cancel it…" — the legal
/// target set of a counter effect is one of: spells on the stack, abilities on
/// the stack, or both. CR 113.3b/113.3c: activated and triggered abilities are
/// the two kinds of ability that exist as objects on the stack and can be
/// countered. CR 115.1: the target is chosen from the legal set the effect
/// defines, so the parser must reproduce that legal set faithfully — including
/// any type restriction on the spell disjunct ("noncreature spell").
///
/// Handles the full three-way disjunction "activated ability, triggered
/// ability, or noncreature spell" (Louisoix's Sacrifice) by composing two
/// independent axes:
///   1. the ability-kind phrase — "activated ability, triggered ability",
///      "activated or triggered ability", or "activated ability"; and
///   2. an optional ", or <type> spell" / "or <type> spell" tail describing a
///      restricted spell disjunct (e.g. "noncreature spell").
///
/// Also recognizes the "spell or ability" / "spell and/or ability" /
/// "ability or spell" form used by other counter cards →
/// `Or{StackSpell, StackAbility}`.
///
/// Deliberately does NOT match a phrase that is purely a spell type
/// restriction with no ability disjunct ("noncreature spell", "artifact or
/// enchantment spell", plain "spell"): those are already handled by
/// `parse_target` + `constrain_filter_to_stack`. This combinator only fires for
/// the cases bare `parse_target` cannot — an "activated/triggered ability"
/// disjunct. It returns a nom `Err` otherwise so callers fall back cleanly.
pub fn parse_stack_object_target(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        // "spell or ability" / "spell and/or ability" → both spells and abilities.
        // This literal arm stays FIRST so the simple two-word form keeps its
        // canonical `Or { StackSpell, StackAbility }` shape (asserted by
        // `test_stack_object_spell_or_ability`). The general driver below would
        // otherwise emit a `Typed { [Card], InZone{Stack} }` bare-spell leg for
        // the same phrase — runtime-equivalent for legality (CR 112.1; see
        // `parse_ability_spell_disjunction`), but the literal arm preserves the
        // asserted canonical shape with zero risk.
        value(
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::StackSpell,
                    TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: None,
                    },
                ],
            },
            alt((
                tag("spell or ability"),
                tag("spell and/or ability"),
                tag("ability or spell"),
            )),
        ),
        // Order-free disjunction of any mix of (type-restricted) spell legs and
        // activated/triggered-ability legs. Each leg kind is composed as one
        // axis and tried at every list position, so leg order is free.
        parse_ability_spell_disjunction,
    ))
    .parse(input)
}

/// Parse a single ability-kind leg of a stack-object phrase.
///
/// CR 113.3b/113.3c: activated and triggered abilities are distinct stack
/// objects; a lone "triggered ability" or "activated ability" leg narrows
/// `kind`, while combined phrases accept both.
fn parse_ability_kind_leg(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        map(tag("triggered ability"), |_| TargetFilter::StackAbility {
            controller: None,
            tag: None,
            kind: Some(crate::types::ability::StackAbilityKind::Triggered),
        }),
        map(tag("activated ability"), |_| TargetFilter::StackAbility {
            controller: None,
            tag: None,
            kind: Some(crate::types::ability::StackAbilityKind::Activated),
        }),
        map(
            alt((
                tag("activated ability, triggered ability"),
                tag("activated or triggered ability"),
                tag("triggered or activated ability"),
                tag("triggered ability or activated ability"),
                tag("activated ability or triggered ability"),
            )),
            |_| TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            },
        ),
    ))
    .parse(input)
}

/// Parse an order-free disjunction of stack-object legs — any mix of
/// (type-restricted) spell phrases and activated/triggered-ability phrases,
/// joined by commas and a final "or" / "and/or".
///
/// CR 701.6a: the legal target set of a counter effect is one of spells and/or
/// abilities on the stack. CR 112.1: a "spell" is a card on the stack.
/// CR 113.3b/113.3c: activated and triggered abilities are the two ability
/// kinds that exist as objects on the stack. CR 115.1: the target is chosen
/// from the legal set the effect defines, so each listed leg (including each
/// spell-type restriction) must be reproduced faithfully.
///
/// Covers Spider-Sense ("instant spell, sorcery spell, or triggered ability"),
/// the 3-way "spell, activated ability, or triggered ability" (Disallow /
/// Voidslime / Overcharged Amalgam / Ertai Resurrected), the 4-way "instant
/// spell, sorcery spell, activated ability, or triggered ability", and
/// Louisoix's Sacrifice ("activated ability, triggered ability, or noncreature
/// spell") — in any leg order, with any number of spell legs.
///
/// Ability-kind legs fold into a single `StackAbility` (CR 113.3b/113.3c: both
/// kinds are just abilities on the stack). Spell legs accumulate as distinct
/// stack-pinned `Typed` legs. A bare "spell" leg becomes
/// `Typed { [TypeFilter::Card], InZone{Stack} }`, which is runtime-equivalent
/// to `StackSpell` for legality: a spell stack entry's id is registered in
/// `state.objects`, while an ability stack entry's id is not (its entry id is
/// allocated fresh and never inserted as an object), so a stack-residency-gated
/// `TypeFilter::Card` predicate matches spells but never abilities. CR 112.1
/// (a spell is a card on the stack) + CR 113.3b/113.3c (abilities on the stack
/// are not card-backed objects) justify the encoding.
///
/// Legs are assembled in source-encounter order, with all ability-kind legs
/// folded into the FIRST ability slot seen, so an ability-first phrasing yields
/// `[StackAbility, <spell legs…>]` (preserving the exact Louisoix AST shape).
///
/// CONTRACT (preserved): returns `Err` unless at least one ABILITY leg is
/// present. A purely-spell phrase ("noncreature spell", "instant or sorcery
/// spell", "spell") is owned by `parse_target` + `constrain_filter_to_stack`;
/// this combinator only fires for what bare `parse_target` cannot represent —
/// an ability disjunct.
fn parse_ability_spell_disjunction(input: &str) -> OracleResult<'_, TargetFilter> {
    enum StackLeg {
        Spell(TargetFilter),
        Ability(TargetFilter),
    }

    fn parse_leg(input: &str) -> OracleResult<'_, StackLeg> {
        // Ability-kind leg tried first: "spell" is a prefix of nothing the
        // ability combinator matches, and an ability phrase is never a valid
        // spell phrase, so the two leg kinds are disjoint and the order is for
        // determinism only.
        alt((
            map(parse_ability_kind_leg, StackLeg::Ability),
            map(parse_restricted_spell, StackLeg::Spell),
        ))
        .parse(input)
    }

    fn merge_ability_kind(
        existing: Option<crate::types::ability::StackAbilityKind>,
        incoming: Option<crate::types::ability::StackAbilityKind>,
    ) -> Option<crate::types::ability::StackAbilityKind> {
        match (existing, incoming) {
            (None, k) | (k, None) => k,
            (Some(a), Some(b)) if a == b => Some(a),
            _ => None,
        }
    }

    // Source-encounter-ordered assembly. Ability legs fold into the first
    // ability slot: push a single `StackAbility` marker at the position it is
    // first seen, and merge later ability legs (widening `kind` when mixed).
    fn push_leg(
        filters: &mut Vec<TargetFilter>,
        ability_slot: &mut Option<usize>,
        ability_kind: &mut Option<crate::types::ability::StackAbilityKind>,
        leg: StackLeg,
    ) {
        match leg {
            StackLeg::Spell(f) => filters.push(f),
            StackLeg::Ability(ability_filter) => {
                let TargetFilter::StackAbility { kind, .. } = ability_filter else {
                    return;
                };
                if let Some(slot) = ability_slot {
                    *ability_kind = merge_ability_kind(*ability_kind, kind);
                    if let TargetFilter::StackAbility {
                        kind: slot_kind, ..
                    } = &mut filters[*slot]
                    {
                        *slot_kind = *ability_kind;
                    }
                } else {
                    *ability_slot = Some(filters.len());
                    *ability_kind = kind;
                    filters.push(ability_filter);
                }
            }
        }
    }

    // First leg is mandatory.
    let (mut rest, first) = parse_leg(input)?;
    let mut filters: Vec<TargetFilter> = Vec::new();
    let mut ability_slot = None;
    let mut ability_kind = None;
    push_leg(&mut filters, &mut ability_slot, &mut ability_kind, first);

    // Subsequent legs joined by a list connector. Longest-match-first so
    // ", or " / ", and/or " win over the bare ", " separator.
    loop {
        let connector = alt((
            tag(", or "),
            tag(", and/or "),
            tag(" and/or "),
            tag(" or "),
            tag(", "),
        ));
        match opt(preceded(connector, parse_leg)).parse(rest)? {
            (next, Some(leg)) => {
                rest = next;
                push_leg(&mut filters, &mut ability_slot, &mut ability_kind, leg);
            }
            (next, None) => {
                rest = next;
                break;
            }
        }
    }

    // CONTRACT: an ability disjunct is required — pure-spell phrases go to
    // `parse_target` so the existing single-leg contracts hold.
    if ability_slot.is_none() {
        return Err(oracle_err(input));
    }

    let filter = match filters.len() {
        // Unreachable given the `saw_ability` guard (a true `saw_ability`
        // implies at least one pushed leg), but kept total.
        0 => return Err(oracle_err(input)),
        1 => filters.into_iter().next().unwrap(),
        _ => TargetFilter::Or { filters },
    };
    Ok((rest, filter))
}

/// Parse a (possibly type-restricted) spell phrase into a stack-constrained
/// `Typed` filter.
///
/// CR 112.1: a "spell" is a card (or copy of a card) on the stack. The leading
/// type phrase ("noncreature", "instant or sorcery", a bare "spell") is parsed
/// with the shared `parse_type_phrase` combinator, then the result is pinned to
/// the stack with an `InZone { Stack }` property so the runtime resolves it
/// against stack objects rather than the battlefield.
fn parse_restricted_spell(input: &str) -> OracleResult<'_, TargetFilter> {
    // `parse_type_phrase` consumes the leading type words. The phrase MUST
    // describe a spell: either it ends in an explicit " spell" noun (e.g.
    // "noncreature spell", "instant or sorcery spell") or `parse_type_phrase`
    // mapped a bare "spell" → `TypeFilter::Card`. Requiring this prevents the
    // combinator from swallowing battlefield type phrases ("creature you
    // control") as if they were stack spells.
    let (rest, filter) = parse_type_phrase(input)?;
    let is_bare_spell = matches!(
        &filter,
        TargetFilter::Typed(TypedFilter { type_filters, .. })
            if type_filters.as_slice() == [TypeFilter::Card]
    );
    let rest = match tag::<_, _, OracleError<'_>>(" spell").parse(rest) {
        Ok((r, _)) => r,
        Err(e) => {
            if is_bare_spell {
                // Phrase was just "spell" — already a spell, nothing to consume.
                rest
            } else {
                // A type phrase with no "spell" noun is not a spell phrase.
                return Err(e);
            }
        }
    };
    Ok((rest, constrain_typed_to_stack(filter)))
}

/// Add an `InZone { Stack }` property to a `Typed` filter so it resolves
/// against stack objects. Mirrors `oracle_effect::constrain_filter_to_stack`
/// but operates on the `oracle_nom` layer for the stack-object combinator.
fn constrain_typed_to_stack(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            mut properties,
        }) => {
            if !properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
            {
                properties.push(FilterProp::InZone { zone: Zone::Stack });
            }
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                properties,
            })
        }
        other => other,
    }
}

/// Build a `TargetFilter` from parsed components.
fn build_type_filter(
    types: Vec<TypeFilter>,
    color: Option<ManaColor>,
    supertype: Option<Supertype>,
    controller: Option<ControllerRef>,
) -> TargetFilter {
    let type_filters: Vec<TypeFilter> = if types.len() == 1 {
        types
    } else {
        vec![TypeFilter::AnyOf(types)]
    };

    let mut properties = Vec::new();
    if let Some(c) = color {
        properties.push(FilterProp::HasColor { color: c });
    }
    if let Some(st) = supertype {
        properties.push(FilterProp::HasSupertype { value: st });
    }

    TargetFilter::Typed(TypedFilter {
        type_filters,
        controller,
        properties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_type_phrase_creature() {
        let (rest, filter) = parse_type_phrase("creature with power").unwrap();
        assert_eq!(rest, " with power");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_artifact_or_enchantment() {
        let (rest, filter) = parse_type_phrase("artifact or enchantment you control").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::AnyOf(vec![
                        TypeFilter::Artifact,
                        TypeFilter::Enchantment
                    ])]
                );
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_controller_suffix() {
        let (rest, c) = parse_controller_suffix("you control stuff").unwrap();
        assert_eq!(c, ControllerRef::You);
        assert_eq!(rest, " stuff");

        let (rest2, c2) = parse_controller_suffix("an opponent controls").unwrap();
        assert_eq!(c2, ControllerRef::Opponent);
        assert_eq!(rest2, "");
    }

    // No-regression guard: the "defending player controls" controller scope is
    // added only to the high-level `oracle_nom::filter::parse_zone_controller`
    // path (the bug-card path), NOT to this nom combinator consumed by
    // `parse_type_phrase`. Folding it in here would change the remainder handed
    // to the five remainder-coupled `parse_type_phrase` callers. This test
    // documents that deliberate divergence (it also does not match
    // "you don't control") so a future edit to this combinator is a conscious
    // choice, not an accident.
    #[test]
    fn test_parse_controller_suffix_excludes_defending_player_and_negated() {
        assert!(
            parse_controller_suffix("defending player controls").is_err(),
            "nom parse_controller_suffix must NOT match 'defending player controls' \
             (handled by parse_zone_controller on the bug-card path)"
        );
        assert!(
            parse_controller_suffix("you don't control").is_err(),
            "nom parse_controller_suffix must NOT match 'you don't control' \
             (pre-existing deliberate divergence from parse_zone_controller)"
        );
    }

    #[test]
    fn test_parse_type_phrase_single() {
        let (rest, filter) = parse_type_phrase("creature you control").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_multi() {
        let (rest, filter) = parse_type_phrase("instant or sorcery").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::AnyOf(vec![
                        TypeFilter::Instant,
                        TypeFilter::Sorcery
                    ])]
                );
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_with_color() {
        let (rest, filter) = parse_type_phrase("white creature").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert!(tf.properties.contains(&FilterProp::HasColor {
                    color: ManaColor::White
                }));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_with_supertype() {
        let (rest, filter) = parse_type_phrase("legendary creature").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    /// CR 205.4a: the shared supertype-word recognizer is the building block for
    /// every CR 205.4a supertype the engine `Supertype` enum models. World and
    /// Ongoing were previously missing, so the "general" supertype-grant path
    /// silently dropped them; this pins that the recognizer now maps all five
    /// (Host is set-supplemental and intentionally excluded). None of the five
    /// words is a prefix of another, so the `alt` order is boundary-safe.
    #[test]
    fn test_parse_supertype_word_covers_world_and_ongoing() {
        assert_eq!(parse_supertype_word("world").unwrap().1, Supertype::World);
        assert_eq!(
            parse_supertype_word("ongoing").unwrap().1,
            Supertype::Ongoing
        );
        // pre-existing arms remain recognized (no regression).
        assert_eq!(
            parse_supertype_word("legendary").unwrap().1,
            Supertype::Legendary
        );
        assert_eq!(parse_supertype_word("basic").unwrap().1, Supertype::Basic);
        assert_eq!(parse_supertype_word("snow").unwrap().1, Supertype::Snow);
        // Host is NOT a CR 205.4a word here, so the recognizer must reject it.
        assert!(parse_supertype_word("host").is_err());
    }

    #[test]
    fn test_parse_type_phrase_nonland() {
        // "nonland" → Non(Land) with trailing text unconsumed
        let (rest, filter) = parse_type_phrase("nonland permanent").unwrap();
        // The parser reads "non" prefix, then "land" as type, leaving " permanent"
        // It wraps the parsed type in Non
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::Non(Box::new(TypeFilter::Land))]
                );
            }
            _ => panic!("expected Typed filter"),
        }
        assert_eq!(rest, " permanent");
    }

    #[test]
    fn test_parse_self_reference() {
        let (rest, f) = parse_self_reference("~ gets").unwrap();
        assert_eq!(rest, " gets");
        assert_eq!(f, TargetFilter::SelfRef);

        let (rest2, f2) = parse_self_reference("it deals").unwrap();
        assert_eq!(rest2, " deals");
        assert_eq!(f2, TargetFilter::SelfRef);

        let (rest3, f3) = parse_self_reference("this creature gets").unwrap();
        assert_eq!(rest3, " gets");
        assert_eq!(f3, TargetFilter::SelfRef);

        // "this card" used when the ability source is in a non-battlefield zone
        // (e.g. Ichorid: "other than this card from your graveyard").
        let (rest4, f4) = parse_self_reference("this card from your graveyard").unwrap();
        assert_eq!(rest4, " from your graveyard");
        assert_eq!(f4, TargetFilter::SelfRef);
    }

    #[test]
    fn test_parse_self_reference_it_word_boundary() {
        // "item" should NOT match as "it" self-reference
        assert!(parse_self_reference("item").is_err());
        assert!(parse_self_reference("iterate").is_err());

        // "it" at end of input should match
        let (rest, f) = parse_self_reference("it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(f, TargetFilter::SelfRef);
    }

    #[test]
    fn test_parse_self_reference_itself() {
        // "itself" at end of input should match
        let (rest, f) = parse_self_reference("itself").unwrap();
        assert_eq!(rest, "");
        assert_eq!(f, TargetFilter::SelfRef);

        // "itself" followed by word boundary should match
        let (rest2, f2) = parse_self_reference("itself.").unwrap();
        assert_eq!(rest2, ".");
        assert_eq!(f2, TargetFilter::SelfRef);

        let (rest3, f3) = parse_self_reference("itself-damage").unwrap();
        assert_eq!(rest3, "-damage");
        assert_eq!(f3, TargetFilter::SelfRef);

        // "itselfless" should NOT match as an "itself" self-reference.
        assert!(parse_self_reference("itselfless").is_err());
    }

    #[test]
    fn test_parse_event_context_ref() {
        let (rest, f) = parse_event_context_ref("that spell's controller gains").unwrap();
        assert_eq!(rest, " gains");
        assert_eq!(f, TargetFilter::TriggeringSpellController);

        let (rest2, f2) = parse_event_context_ref("that player loses").unwrap();
        assert_eq!(rest2, " loses");
        assert_eq!(f2, TargetFilter::TriggeringPlayer);

        let (rest3, f3) = parse_event_context_ref("defending player").unwrap();
        assert_eq!(rest3, "");
        assert_eq!(f3, TargetFilter::DefendingPlayer);

        let (rest4, f4) = parse_event_context_ref("that spell is countered").unwrap();
        assert_eq!(rest4, " is countered");
        assert_eq!(f4, TargetFilter::TriggeringSource);

        // CR 608.2k: "the player" in trigger context is anaphoric to
        // the triggering player (synonym for "that player").
        let (rest5, f5) = parse_event_context_ref("the player loses").unwrap();
        assert_eq!(rest5, " loses");
        assert_eq!(f5, TargetFilter::TriggeringPlayer);

        // "the defending player" still wins over "the player" (longest-match).
        let (rest6, f6) = parse_event_context_ref("the defending player gains").unwrap();
        assert_eq!(rest6, " gains");
        assert_eq!(f6, TargetFilter::DefendingPlayer);

        // CR 506.2 + CR 603.7c: attack-trigger actor anaphor (Ellie, Breena).
        let (rest7, f7) = parse_event_context_ref("that attacking player creates").unwrap();
        assert_eq!(rest7, " creates");
        assert_eq!(f7, TargetFilter::TriggeringPlayer);

        // CR 506.3d: attacked opponent anaphor in token-enter-attacking clauses.
        let (rest8, f8) = parse_event_context_ref("that opponent.").unwrap();
        assert_eq!(rest8, ".");
        assert_eq!(f8, TargetFilter::DefendingPlayer);

        let (rest9, f9) = parse_event_context_ref("that permanent").unwrap();
        assert_eq!(rest9, "");
        assert_eq!(f9, TargetFilter::TriggeringSource);

        assert!(parse_event_context_ref("that permanent or player").is_err());
        assert!(parse_event_context_ref("that permanent or a player").is_err());
    }

    #[test]
    fn test_parse_type_filter_word_plurals() {
        let r = parse_type_filter_word("creatures you");
        assert!(r.is_ok());
        let (rest, _t) = r.unwrap();
        assert_eq!(rest, " you");
    }

    #[test]
    fn test_parse_type_filter_word_spell() {
        // CR 112.1: a spell is a card on the stack — "spell" maps to Card.
        let (rest, t) = parse_type_filter_word("spell").unwrap();
        assert!(matches!(t, TypeFilter::Card), "expected Card for spell");
        assert_eq!(rest, "");
    }

    /// CR 700.12: the expected outlaw disjunction (Assassin, Mercenary, Pirate,
    /// Rogue, Warlock) produced by the "outlaw[s]" head noun.
    fn outlaw_any_of() -> TypeFilter {
        TypeFilter::AnyOf(
            ["Assassin", "Mercenary", "Pirate", "Rogue", "Warlock"]
                .iter()
                .map(|s| TypeFilter::Subtype((*s).to_string()))
                .collect(),
        )
    }

    #[test]
    fn test_parse_type_filter_word_outlaws() {
        // CR 700.12: "outlaws" expands to the five outlaw creature types.
        let (rest, t) = parse_type_filter_word("outlaws you control").unwrap();
        assert_eq!(t, outlaw_any_of());
        assert_eq!(rest, " you control");
    }

    #[test]
    fn test_parse_type_filter_word_outlaw_singular() {
        let (rest, t) = parse_type_filter_word("outlaw").unwrap();
        assert_eq!(t, outlaw_any_of());
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_type_filter_word_outlawry_does_not_match_outlaw() {
        // Word-boundary guard: "outlawry" must NOT match the "outlaw" head noun.
        // An `Err` is also acceptable — "outlawry" is not a type word at all.
        if let Ok((_, tf)) = parse_type_filter_word("outlawry") {
            assert_ne!(
                tf,
                outlaw_any_of(),
                "outlawry must not parse as the outlaw disjunction"
            );
        }
    }

    // --- TYPE_WORDS word-boundary guard (the head-noun-prefix class) ---
    //
    // Without the boundary guard on the TYPE_WORDS scan, a head-noun entry like
    // "land" or "spell" strip_prefix-matches longer subtypes ("lander",
    // "spellshaper") and returns the wrong card-type filter instead of falling
    // through to the boundary-guarded subtype table. "Plan" itself is an
    // enchantment subtype (CR 205.3h), so it resolves via that subtype table.

    #[test]
    fn test_type_word_plant_is_subtype_not_plan() {
        // The "plant" head noun resolves to the Plant subtype; no head-noun
        // TYPE_WORD prefix shadows it.
        let (rest, tf) = parse_type_filter_word("plant").unwrap();
        assert_eq!(tf, TypeFilter::Subtype("Plant".to_string()));
        assert_eq!(rest, "");
    }

    #[test]
    fn test_type_word_plants_is_subtype_not_plan() {
        // Regular plural: "plants" must resolve to the Plant subtype, not Plan.
        let (rest, tf) = parse_type_filter_word("plants").unwrap();
        assert_eq!(tf, TypeFilter::Subtype("Plant".to_string()));
        assert_eq!(rest, "");
    }

    #[test]
    fn test_type_word_planet_is_subtype_not_plan() {
        // "planet" is a land subtype; the "plan" prefix must not shadow it.
        let (rest, tf) = parse_type_filter_word("planet").unwrap();
        assert_eq!(tf, TypeFilter::Subtype("Planet".to_string()));
        assert_eq!(rest, "");
    }

    #[test]
    fn test_type_word_power_plant_never_plan() {
        // "power-plant" is a land subtype (Power-Plant). The hyphenated form must
        // resolve as a subtype.
        let (_, tf) = parse_type_filter_word("power-plant").unwrap();
        assert!(
            matches!(tf, TypeFilter::Subtype(_)),
            "power-plant must be a Subtype, got {tf:?}"
        );
    }

    #[test]
    fn test_type_word_power_space_plant_never_plan() {
        // The space-separated "power plant" head noun must never classify as the
        // "Plan" subtype. Either it fails to parse or it resolves to a non-Plan
        // filter — both are acceptable; "Plan" is the prohibited result.
        if let Ok((_, tf)) = parse_type_filter_word("power plant") {
            assert_ne!(
                tf,
                TypeFilter::Subtype("Plan".to_string()),
                "power plant must never be the Plan subtype"
            );
        }
    }

    #[test]
    fn test_type_word_plan_still_plan() {
        // "Plan" is an enchantment subtype (CR 205.3h): bare "plan" resolves to
        // Subtype("Plan") via the subtype table.
        let (rest, tf) = parse_type_filter_word("plan").unwrap();
        assert_eq!(tf, TypeFilter::Subtype("Plan".to_string()));
        assert_eq!(rest, "");
    }

    #[test]
    fn test_type_word_plan_trailing_space_still_plan() {
        // "plan" followed by a space boundary resolves to the Plan subtype and
        // leaves the trailing context unconsumed.
        let (rest, tf) = parse_type_filter_word("plan you control").unwrap();
        assert_eq!(tf, TypeFilter::Subtype("Plan".to_string()));
        assert_eq!(rest, " you control");
    }

    #[test]
    fn test_type_word_spellshaper_is_subtype_not_card() {
        // Class-lock: pre-fix the "spell" entry (→ Card) eats "spellshaper".
        // Post-fix the boundary guard lets the subtype table resolve Spellshaper.
        let (rest, tf) = parse_type_filter_word("spellshaper").unwrap();
        assert_ne!(tf, TypeFilter::Card, "spellshaper must not be Card");
        assert_eq!(tf, TypeFilter::Subtype("Spellshaper".to_string()));
        assert_eq!(rest, "");
    }

    #[test]
    fn test_type_word_lander_is_subtype_not_land() {
        // Class-lock: pre-fix the "land" entry (→ Land) eats "lander".
        // Post-fix the boundary guard lets the subtype table resolve Lander.
        let (_, tf) = parse_type_filter_word("lander").unwrap();
        assert_ne!(tf, TypeFilter::Land, "lander must not be Land");
        assert!(
            matches!(tf, TypeFilter::Subtype(_)),
            "lander must be a Subtype, got {tf:?}"
        );
    }

    #[test]
    fn test_type_word_battles_is_battle() {
        // Regression guard for the word-boundary fix: "battle" is the only
        // TYPE_WORDS entry that was missing its plural sibling. The boundary
        // guard rejects "battle" + trailing 's', and BATTLE_SUBTYPES has no
        // "Battle" entry, so parse_subtype cannot recover it — without the
        // explicit "battles" TYPE_WORDS entry this returns Err.
        let (rest, tf) = parse_type_filter_word("battles").unwrap();
        assert_eq!(tf, TypeFilter::Battle);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_type_word_battles_trailing_context() {
        // Regression guard for the word-boundary fix: "battles you control" is a
        // supported head-noun phrase (see oracle_static grammar/type_change/
        // restriction). The "battles" plural entry must classify as Battle and
        // leave the trailing context unconsumed.
        let (rest, tf) = parse_type_filter_word("battles you control").unwrap();
        assert_eq!(tf, TypeFilter::Battle);
        assert_eq!(rest, " you control");
    }

    // --- parse_stack_object_target (CR 701.6a + CR 115.1) ---

    /// The noncreature-spell disjunct: a stack-pinned `Typed` filter that
    /// excludes creature spells via `TypeFilter::Non(Creature)`.
    fn noncreature_spell_leg() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
            controller: None,
            properties: vec![FilterProp::InZone { zone: Zone::Stack }],
        })
    }

    #[test]
    fn test_stack_object_three_way_disjunction() {
        // Louisoix's Sacrifice — the full three-way disjunction.
        let (rest, filter) =
            parse_stack_object_target("activated ability, triggered ability, or noncreature spell")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: None,
                    },
                    noncreature_spell_leg(),
                ],
            }
        );
    }

    #[test]
    fn test_stack_object_noncreature_excludes_creature_spell() {
        // The noncreature restriction must be carried as a typed `Non` leg —
        // a creature spell is NOT a member of the legal target set.
        let (_, filter) =
            parse_stack_object_target("activated ability, triggered ability, or noncreature spell")
                .unwrap();
        let TargetFilter::Or { filters } = &filter else {
            panic!("expected Or filter");
        };
        let spell_leg = &filters[1];
        let TargetFilter::Typed(tf) = spell_leg else {
            panic!("expected Typed spell leg");
        };
        assert_eq!(
            tf.type_filters,
            vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
            "creature spells must be excluded by the noncreature restriction"
        );
        assert!(
            tf.properties
                .contains(&FilterProp::InZone { zone: Zone::Stack }),
            "the spell leg must be pinned to the stack zone"
        );
    }

    #[test]
    fn test_stack_object_activated_or_triggered_ability() {
        // Ability-only counter (e.g. Stifle / Disallow's ability disjunct).
        let (rest, filter) = parse_stack_object_target("activated or triggered ability").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            }
        );
    }

    #[test]
    fn test_stack_object_activated_ability_only() {
        let (rest, filter) = parse_stack_object_target("activated ability").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: Some(crate::types::ability::StackAbilityKind::Activated),
            }
        );
    }

    #[test]
    fn test_stack_object_triggered_ability_only() {
        let (rest, filter) = parse_stack_object_target("triggered ability").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: Some(crate::types::ability::StackAbilityKind::Triggered),
            }
        );
    }

    #[test]
    fn test_stack_object_triggered_ability_or_colorless_spell() {
        let (rest, filter) =
            parse_stack_object_target("triggered ability or colorless spell").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            filter,
            TargetFilter::Or {
                filters: legs
            } if legs.len() == 2
                && matches!(
                    &legs[0],
                    TargetFilter::StackAbility {
                        kind: Some(crate::types::ability::StackAbilityKind::Triggered),
                        ..
                    }
                )
                && matches!(&legs[1], TargetFilter::Typed(_))
        ));
    }

    #[test]
    fn test_stack_object_spell_or_ability() {
        // Disallow / Voidslime — "counter target spell, activated ability, or
        // triggered ability" reduces (in the simple two-way form) to the
        // "spell or ability" phrasing.
        let (rest, filter) = parse_stack_object_target("spell or ability").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::StackSpell,
                    TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: None,
                    },
                ],
            }
        );
    }

    #[test]
    fn test_stack_object_rejects_pure_spell_phrase() {
        // A phrase that is purely a spell type restriction (no ability
        // disjunct) must NOT be matched — `parse_target` handles those.
        assert!(parse_stack_object_target("noncreature spell").is_err());
        // The inner " or " of "artifact or enchantment spell" is consumed whole
        // by `parse_type_phrase` BEFORE the connector loop runs, so it is never
        // mistaken for a leg connector and the phrase stays a single (rejected)
        // spell leg.
        assert!(parse_stack_object_target("artifact or enchantment spell").is_err());
        assert!(parse_stack_object_target("spell").is_err());
        // A battlefield type phrase must likewise not be swallowed.
        assert!(parse_stack_object_target("creature you control").is_err());
        assert!(parse_stack_object_target("permanent").is_err());
        // A multi-spell phrase with NO ability leg is still pure-spell and must
        // be rejected — the ability-disjunct contract guards the spell-first
        // path too, not just the single-leg one.
        assert!(parse_stack_object_target("instant spell, or sorcery spell").is_err());
    }

    /// A single typed spell leg with its `InZone{Stack}` constraint — the shape
    /// each accumulated spell leg takes for a concrete card type.
    fn typed_spell_leg(ty: TypeFilter) -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![ty],
            controller: None,
            properties: vec![FilterProp::InZone { zone: Zone::Stack }],
        })
    }

    #[test]
    fn test_stack_object_spell_first_disjunction() {
        // Spider-Sense — spell-first phrasing. Legs appear in source-encounter
        // order: instant spell, sorcery spell, then the folded ability.
        let (rest, filter) =
            parse_stack_object_target("instant spell, sorcery spell, or triggered ability")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Or {
                filters: vec![
                    typed_spell_leg(TypeFilter::Instant),
                    typed_spell_leg(TypeFilter::Sorcery),
                    TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: Some(crate::types::ability::StackAbilityKind::Triggered),
                    },
                ],
            }
        );
    }

    #[test]
    fn test_stack_object_four_way_disjunction() {
        // Two spell legs + two ability spellings → the two ability legs fold
        // into exactly ONE `StackAbility`.
        let (rest, filter) = parse_stack_object_target(
            "instant spell, sorcery spell, activated ability, or triggered ability",
        )
        .unwrap();
        assert_eq!(rest, "");
        let TargetFilter::Or { filters } = &filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert!(
            filters.contains(&typed_spell_leg(TypeFilter::Instant)),
            "missing instant spell leg: {filters:?}"
        );
        assert!(
            filters.contains(&typed_spell_leg(TypeFilter::Sorcery)),
            "missing sorcery spell leg: {filters:?}"
        );
        assert_eq!(
            filters
                .iter()
                .filter(|f| matches!(f, TargetFilter::StackAbility { .. }))
                .count(),
            1,
            "the two ability spellings must fold to exactly one StackAbility: {filters:?}"
        );
    }

    #[test]
    fn test_stack_object_spell_first_three_way() {
        // Disallow's phrasing — bare "spell" first, then both ability spellings.
        let (rest, filter) =
            parse_stack_object_target("spell, activated ability, or triggered ability").unwrap();
        assert_eq!(rest, "");
        let TargetFilter::Or { filters } = &filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        // First leg is the bare-spell stack-pinned `Typed { [Card], Stack }`.
        assert_eq!(
            filters.first(),
            Some(&typed_spell_leg(TypeFilter::Card)),
            "first leg must be the stack-pinned bare-spell Typed leg: {filters:?}"
        );
        assert_eq!(
            filters
                .iter()
                .filter(|f| matches!(f, TargetFilter::StackAbility { .. }))
                .count(),
            1,
            "both ability spellings must fold to exactly one StackAbility: {filters:?}"
        );
    }
}
