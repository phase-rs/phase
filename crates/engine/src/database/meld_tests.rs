//! Tests for Meld (CR 701.42 / CR 712.4) parsing + synthesis. Declared from
//! `database/mod.rs` so the implementation modules (`database/meld.rs`,
//! `game/meld.rs`) stay free of inline test scaffolding.
//!
//! These are building-block / AST-shape tests: they assert the parser derives
//! `Effect::Meld { source, partner, result }` with the correct names,
//! parameterized over the partner card type (creature partner / creature partner
//! from a LAND instigator) and trigger-vs-activated shape. The runtime
//! regression tests that drive the real resolve pipeline live in
//! `game/meld_tests.rs`.

use crate::database::mtgjson::{AtomicCard, AtomicIdentifiers};
use crate::types::ability::{AbilityKind, Effect};
use crate::types::card::CardFace;
use crate::types::triggers::TriggerMode;

/// Build an `AtomicCard` for a single card face from its oracle `text`.
fn atomic(name: &str, type_line: &str, types: &[&str], text: &str) -> AtomicCard {
    AtomicCard {
        name: name.to_string(),
        mana_cost: Some("{4}{W}{W}".to_string()),
        colors: vec!["W".to_string()],
        color_identity: vec!["W".to_string()],
        power: Some("4".to_string()),
        toughness: Some("3".to_string()),
        loyalty: None,
        defense: None,
        text: Some(text.to_string()),
        layout: "meld".to_string(),
        type_line: Some(type_line.to_string()),
        types: types.iter().map(|s| s.to_string()).collect(),
        subtypes: Vec::new(),
        supertypes: vec!["Legendary".to_string()],
        keywords: None,
        side: Some("a".to_string()),
        face_name: None,
        mana_value: 6.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some(format!("{}-oracle", name.to_lowercase())),
            scryfall_id: Some(format!("{}-face", name.to_lowercase())),
        },
        foreign_data: Vec::new(),
        related_cards: crate::database::mtgjson::SetRelatedCards::default(),
    }
}

fn parse_face(card: &AtomicCard) -> CardFace {
    crate::database::synthesis::build_oracle_face(card, None)
}

/// Find an `Effect::Meld` anywhere in a face's abilities or trigger payloads,
/// descending sub/else/mode branches so a gated meld sub-ability (the optional-cost
/// Vanille form, where the meld lives under an "If you do" `PayCost`) is found.
fn find_meld(face: &CardFace) -> Option<(String, String, String)> {
    fn in_def(def: &crate::types::ability::AbilityDefinition) -> Option<(String, String, String)> {
        if let Effect::Meld {
            source,
            partner,
            result,
        } = def.effect.as_ref()
        {
            return Some((source.clone(), partner.clone(), result.clone()));
        }
        def.sub_ability
            .as_deref()
            .and_then(in_def)
            .or_else(|| def.else_ability.as_deref().and_then(in_def))
            .or_else(|| def.mode_abilities.iter().find_map(in_def))
    }
    for a in &face.abilities {
        if let Some(m) = in_def(a) {
            return Some(m);
        }
    }
    for t in &face.triggers {
        if let Some(m) = t.execute.as_deref().and_then(in_def) {
            return Some(m);
        }
    }
    None
}

const GISELA_TEXT: &str = "Flying, first strike\n\
    At the beginning of your end step, if you both own and control Gisela, the Broken Blade \
    and a creature named Bruna, the Fading Light, exile them, then meld them into Brisela, \
    Voice of Nightmares.";

const HANWEIR_TEXT: &str = "{T}: Add {R}.\n\
    {3}{R}{R}, {T}: If you both own and control this land and a creature named Hanweir Garrison, \
    exile them, then meld them into Hanweir, the Writhing Township. Activate only as a sorcery.";

/// The optional-cost triggered meld form (Vanille / Fang): the own/control gate
/// is followed by a reflexive "you may pay {C}. If you do," additional cost before
/// the meld sentinel. The gate models this (CR 118.12): the own/control gate
/// becomes the trigger's intervening-if, the "you may pay {3}{B}{G}" lowers to an
/// optional `PayCost`, and the meld lands as a gated sub-ability — fully supported.
const VANILLE_TEXT: &str = "When Vanille enters, mill two cards, then return a permanent card \
    from your graveyard to your hand.\n\
    At the beginning of your first main phase, if you both own and control Vanille and a \
    creature named Fang, Fearless l'Cie, you may pay {3}{B}{G}. If you do, exile them, then \
    meld them into Ragnarok, Divine Deliverance.";

/// CR 701.42a: the triggered instigator (Gisela, creature partner) parses to an
/// `Effect::Meld { source, partner, result }` carrying the correct source,
/// partner, and result names. The own/control gate is hoisted to the trigger's
/// intervening-if, so the bare residual "exile them, then meld them into R"
/// parses cleanly.
///
/// The activated / inline-gate form (Hanweir Battlements) is DEFERRED: its text
/// leads with the inline "if you both own and control ..." gate, which the meld
/// effect interception does not strip (stripping it would swallow the
/// `Condition_If` — a coverage-honesty regression). It therefore yields NO
/// `Effect::Meld` and remains Unimplemented until a real activated-ability
/// condition node is added (follow-up).
#[test]
fn synthesize_or_parse_derives_self_partner_result() {
    let gisela = parse_face(&atomic(
        "Gisela, the Broken Blade",
        "Legendary Creature — Angel Horror",
        &["Creature"],
        GISELA_TEXT,
    ));
    let (source, partner, result) = find_meld(&gisela).expect("Gisela parses an Effect::Meld");
    assert_eq!(source, "Gisela, the Broken Blade");
    assert_eq!(partner, "Bruna, the Fading Light");
    assert_eq!(result, "Brisela, Voice of Nightmares");

    let hanweir = parse_face(&atomic(
        "Hanweir Battlements",
        "Land",
        &["Land"],
        HANWEIR_TEXT,
    ));
    assert!(
        find_meld(&hanweir).is_none(),
        "the activated/inline-gate form is deferred (must NOT swallow the inline \
         Condition_If by emitting an Effect::Meld)"
    );
}

/// CR 118.12 + CR 701.42a: the optional-cost meld form (Vanille / Fang) carries a
/// reflexive "you may pay {C}. If you do," additional cost between the own/control
/// gate and the meld sentinel. The gate models it: the own/control gate hoists to
/// the trigger's intervening-if, the "you may pay {3}{B}{G}" becomes an optional
/// `PayCost`, and the meld lands as a gated sub-ability. The card is fully
/// supported — the `Effect::Meld` carries the real pair names and NO Unimplemented
/// survives. Reverting the `parse_meld_gate` rewrite (or the reflexive sub-clause
/// dispatch) drops the meld back to Unimplemented, flipping both assertions.
#[test]
fn optional_cost_meld_form_lowers_to_gated_meld() {
    let vanille = parse_face(&atomic(
        "Vanille, Cheerful l'Cie",
        "Legendary Creature — Human",
        &["Creature"],
        VANILLE_TEXT,
    ));
    let (source, partner, result) =
        find_meld(&vanille).expect("the optional-cost meld form lowers to a gated Effect::Meld");
    assert_eq!(source, "Vanille, Cheerful l'Cie");
    assert_eq!(partner, "Fang, Fearless l'Cie");
    assert_eq!(result, "Ragnarok, Divine Deliverance");
    assert!(
        !crate::game::coverage::card_face_has_unimplemented_parts(&vanille),
        "Vanille flips to fully supported — the 'you may pay' clause is modeled, not swallowed"
    );
}

/// A face whose Oracle text is only the partner-half reminder ("Melds with X.")
/// gets NO meld ability — only the instigator face (carrying the gate + meld
/// clause) produces `Effect::Meld`.
#[test]
fn partner_half_not_synthesized() {
    let bruna = parse_face(&atomic(
        "Bruna, the Fading Light",
        "Legendary Creature — Angel Horror",
        &["Creature"],
        "When Bruna, the Fading Light enters or attacks, you may return target Aura or \
         Angel creature card from your graveyard to the battlefield.\n\
         (Melds with Gisela, the Broken Blade.)",
    ));
    assert!(
        find_meld(&bruna).is_none(),
        "the partner half must not synthesize an Effect::Meld"
    );
}

/// The triggered instigator yields a `TriggerDefinition` (the meld clause lives
/// inside the trigger's `execute`). The activated / inline-gate instigator is
/// DEFERRED: stripping its inline own/control gate would swallow the
/// `Condition_If`, so it produces NO activated `Effect::Meld` and falls through
/// to Unimplemented (follow-up: a real activated-ability condition node).
#[test]
fn triggered_vs_activated_shape() {
    let gisela = parse_face(&atomic(
        "Gisela, the Broken Blade",
        "Legendary Creature — Angel Horror",
        &["Creature"],
        GISELA_TEXT,
    ));
    assert!(
        gisela.triggers.iter().any(|t| t
            .execute
            .as_ref()
            .is_some_and(|e| matches!(e.effect.as_ref(), Effect::Meld { .. }))),
        "Gisela's meld lives inside a trigger's execute"
    );

    let hanweir = parse_face(&atomic(
        "Hanweir Battlements",
        "Land",
        &["Land"],
        HANWEIR_TEXT,
    ));
    assert!(
        !hanweir.abilities.iter().any(|a| {
            a.kind == AbilityKind::Activated && matches!(a.effect.as_ref(), Effect::Meld { .. })
        }),
        "the activated/inline-gate form is deferred — no activated Effect::Meld is emitted"
    );
}

/// The parsed `Effect::Meld` carries all pair names — partner is NOT dropped or
/// re-derived from a single field.
#[test]
fn effect_round_trips_partner_and_result() {
    let gisela = parse_face(&atomic(
        "Gisela, the Broken Blade",
        "Legendary Creature — Angel Horror",
        &["Creature"],
        GISELA_TEXT,
    ));
    let (source, partner, result) = find_meld(&gisela).expect("Effect::Meld present");
    assert!(!source.is_empty() && !partner.is_empty() && !result.is_empty());
    assert_ne!(source, partner);
    assert_ne!(partner, result);
}

/// For a triggered instigator, the parsed trigger's nested `execute` effect is
/// `Effect::Meld { .. }` and is NOT a residual `Effect::Unimplemented` — i.e. the
/// parser replaced the Unimplemented INSIDE the trigger's `execute`.
#[test]
fn meld_trigger_execute_is_meld_not_unimplemented() {
    let gisela = parse_face(&atomic(
        "Gisela, the Broken Blade",
        "Legendary Creature — Angel Horror",
        &["Creature"],
        GISELA_TEXT,
    ));
    let meld_trigger = gisela
        .triggers
        .iter()
        .find(|t| {
            t.execute
                .as_ref()
                .is_some_and(|e| matches!(e.effect.as_ref(), Effect::Meld { .. }))
        })
        .expect("a meld trigger exists");
    let exec = meld_trigger.execute.as_ref().unwrap();
    assert!(
        !matches!(exec.effect.as_ref(), Effect::Unimplemented { .. }),
        "the trigger's execute must not be Unimplemented"
    );
    // CR 603.4: the own/control gate is hoisted to the trigger's intervening-if.
    assert!(
        meld_trigger.condition.is_some(),
        "the own/control gate must attach as the trigger's intervening-if condition"
    );
    // The trigger mode is registry-recognized (Phase), with the end-step phase.
    assert_eq!(meld_trigger.mode, TriggerMode::Phase);
}

/// CR 701.42a: a meld instigator instantiated as a real `GameObject` (the
/// production `apply_card_face_to_object` path used when a card enters a zone)
/// carries NO unimplemented mechanics. This is the coverage contract: the meld
/// trigger's `TriggerMode::Phase` is registry-recognized and the trigger's
/// `execute` is a real `Effect::Meld`, so no residual `Effect::Unimplemented`
/// survives onto the object — the instigator is fully supported, not flagged
/// as a parse gap.
#[test]
fn meld_instigator_has_no_unimplemented_mechanics() {
    use crate::game::game_object::GameObject;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    // Mirror the real card-db entry: MTGJSON supplies the keyword names so the
    // "Flying, first strike" line is recognized as a keyword-only line (not a
    // residual `Effect::Unimplemented`). Without these the keyword line falls
    // through to Unimplemented — which would mask the very coverage signal this
    // test asserts. Production always has them for Gisela.
    let mut card = atomic(
        "Gisela, the Broken Blade",
        "Legendary Creature — Angel Horror",
        &["Creature"],
        GISELA_TEXT,
    );
    card.keywords = Some(vec!["Flying".to_string(), "First strike".to_string()]);
    let gisela = parse_face(&card);

    let mut obj = GameObject::new(
        ObjectId(1),
        CardId(1),
        PlayerId(0),
        gisela.name.clone(),
        Zone::Battlefield,
    );
    crate::game::printed_cards::apply_card_face_to_object(&mut obj, &gisela);

    let missing = crate::game::coverage::unimplemented_mechanics(&obj);
    assert!(
        missing.is_empty(),
        "meld instigator must not be flagged as having unimplemented mechanics, got: {missing:?}"
    );
}
