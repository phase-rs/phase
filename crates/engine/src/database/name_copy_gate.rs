//! CR 707.13 + CR 709.3c + CR 709.4a: database-build gate for copies created
//! from a card NAME (`Effect::CreateCardCopyByName`).
//!
//! CR 707.13 takes the copy's characteristics from the Oracle card reference for
//! the card with the chosen name. The engine materializes that copy from a single
//! `CardFace` (`state.card_face_registry`), which is the whole card only when the
//! card is single-faced. A name that is one half of a split card names the WHOLE
//! split card (CR 709.4a: an object has the chosen name if one of its names is
//! the chosen name), and a copy of a split card keeps every half (CR 709.3c) — a
//! one-face copy would be a different object than the rule creates. Interrogation
//! Robot's domain ("Who, What, When, Where, and Why") is exactly that: five halves
//! of one Unhinged split card, a layout `CardLayout::Split` cannot hold (CR 709.1
//! split cards have two halves).
//!
//! So a copy clause whose name domain contains a name the database cannot
//! materialize as a whole single-faced card is recorded as an honest
//! `Effect::unimplemented` gap at database build, rather than resolving as a
//! silent no-op (or a wrong one-half copy) while coverage reports the card as
//! supported. Garth One-Eye and Ersta, Friend to All name only single-faced cards
//! and are untouched.

use std::ops::ControlFlow;

use crate::types::ability::{AbilityDefinition, ChoiceType, Effect, TargetFilter};
use crate::types::ability_visit::{nodes_mut, DefinitionNodeMut};
use crate::types::card::CardFace;

/// Gap name recorded for a copy clause (and its dependent cast) the database
/// cannot materialize. Distinct from the effect's own coverage label.
pub const GAP_NAME: &str = "copy_by_name_unmaterializable";

/// Replace every `Effect::CreateCardCopyByName` on `face` whose name domain
/// includes a name `is_single_faced_card` rejects — and the "you may cast the
/// copy" clause that depends on it — with an honest gap. Returns whether
/// anything was replaced.
///
/// `is_single_faced_card` answers, for a printed card name, whether the database
/// holds it as a single-faced card (and so can materialize a copy of the whole
/// card from one face). The caller owns the lookup because the two database
/// builders index faces differently (`oracle_gen`'s export map, the MTGJSON
/// loader's layouts).
///
/// Every definition the face carries is walked — activated and spell abilities,
/// trigger executes, static and replacement payloads, and every nested
/// sub/else/mode ability — through the same exclusive walk
/// (`ability_visit::nodes_mut`) that mirrors the shared walk
/// `printed_cards::collect_conjure_names` seeds the registry with.
pub fn gate_unmaterializable_name_copies(
    face: &mut CardFace,
    is_single_faced_card: &impl Fn(&str) -> bool,
) -> bool {
    let mut changed = false;
    let mut on_node = |node: DefinitionNodeMut<'_>| {
        if let DefinitionNodeMut::Ability(def) = node {
            changed |= gate_from(def, is_single_faced_card);
        }
        ControlFlow::Continue(())
    };
    for ability in &mut face.abilities {
        let _ = nodes_mut::visit_ability_def(ability, &mut on_node);
    }
    for trigger in &mut face.triggers {
        let _ = nodes_mut::visit_trigger(trigger, &mut on_node);
    }
    for static_def in &mut face.static_abilities {
        let _ = nodes_mut::visit_static(static_def, &mut on_node);
    }
    for replacement in &mut face.replacements {
        let _ = nodes_mut::visit_replacement(replacement, &mut on_node);
    }
    changed
}

/// Gate the copy clause a single node starts, if it starts one: a card-name
/// choice whose domain is not materializable gaps the chosen-name copy that
/// follows it in the chain; a literal-name copy of an unmaterializable name gaps
/// itself. Either way the dependent "cast the copy" clause goes with it.
fn gate_from(def: &mut AbilityDefinition, is_single_faced_card: &impl Fn(&str) -> bool) -> bool {
    match &*def.effect {
        Effect::Choose {
            choice_type: ChoiceType::CardName { options, .. },
            ..
        } => {
            // CR 201.4: an OPEN domain ranges over every name in the Oracle card
            // reference, which the scoped face registry is never preloaded with —
            // so it is not materializable either.
            let materializable =
                !options.is_empty() && options.iter().all(|name| is_single_faced_card(name));
            if materializable {
                return false;
            }
            gap_chosen_name_copy_chain(def.sub_ability.as_deref_mut())
        }
        Effect::CreateCardCopyByName {
            name: Some(name), ..
        } if !is_single_faced_card(name) => {
            gap(def);
            gap_dependent_casts(def.sub_ability.as_deref_mut());
            true
        }
        _ => false,
    }
}

/// Walk the chain after an unmaterializable card-name choice: its chosen-name
/// copy and every cast of that copy become gaps. A later card-name choice opens
/// a new domain, which its own node decides.
fn gap_chosen_name_copy_chain(mut node: Option<&mut AbilityDefinition>) -> bool {
    let mut changed = false;
    while let Some(def) = node {
        match &*def.effect {
            Effect::Choose {
                choice_type: ChoiceType::CardName { .. },
                ..
            } => break,
            Effect::CreateCardCopyByName { name: None, .. } => {
                gap(def);
                changed = true;
                gap_dependent_casts(def.sub_ability.as_deref_mut());
                break;
            }
            _ => {}
        }
        node = def.sub_ability.as_deref_mut();
    }
    changed
}

/// The "you may cast the copy" clause bound to the gapped copy
/// (`CastFromZone { LastCreated }`) has nothing to cast; it is gapped with it.
fn gap_dependent_casts(mut node: Option<&mut AbilityDefinition>) {
    while let Some(def) = node {
        if matches!(
            &*def.effect,
            Effect::CastFromZone {
                target: TargetFilter::LastCreated,
                ..
            }
        ) {
            gap(def);
        }
        node = def.sub_ability.as_deref_mut();
    }
}

fn gap(def: &mut AbilityDefinition) {
    let fragment = def
        .description
        .clone()
        .unwrap_or_else(|| "create a copy of the card with the chosen name".to_string());
    *def.effect = Effect::unimplemented(GAP_NAME, fragment);
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use crate::database::card_db::CardDatabase;
    use crate::game::coverage::card_face_has_unimplemented_parts;
    use crate::types::ability::{AbilityDefinition, ChoiceType, Effect};
    use crate::types::card::{CardFace, CardLayout};

    fn single(name: &str, types: &str, mana_cost: &str, text: &str) -> String {
        format!(
            r#""{name}": [{{
                "name": "{name}", "colors": [], "colorIdentity": [], "layout": "normal",
                "types": [{types}], "subtypes": [], "supertypes": [],
                "manaCost": "{mana_cost}", "manaValue": 1.0, "text": "{text}",
                "identifiers": {{ "scryfallOracleId": "o-{name}" }}
            }}]"#
        )
    }

    fn split_half(name: &str, mana_cost: &str, text: &str) -> String {
        format!(
            r#"{{
                "name": "Who // What // When // Where // Why", "faceName": "{name}",
                "colors": [], "colorIdentity": [], "layout": "split", "types": ["Instant"],
                "subtypes": [], "supertypes": [], "manaCost": "{mana_cost}",
                "manaValue": 2.0, "text": "{text}",
                "identifiers": {{ "scryfallOracleId": "o-who-what" }}
            }}"#
        )
    }

    /// A raw MTGJSON database through the production loader: Garth One-Eye with
    /// its six single-faced names, Interrogation Robot, and the five-half Unhinged
    /// split card Who // What // When // Where // Why (Oracle text from Scryfall).
    fn load_fixture() -> CardDatabase {
        let robot = "{T}, Discard a card: Choose a card name that hasn't been chosen among Who, \
                     What, When, Where, and Why. Create a copy of the card with the chosen name. \
                     You may cast the copy. (You still pay its costs. Who//What//When//Where//Why \
                     is a split card from Unhinged.)";
        let garth = "{T}: Choose a card name that hasn't been chosen from among Disenchant, \
                     Braingeyser, Terror, Shivan Dragon, Regrowth, and Black Lotus. Create a copy \
                     of the card with the chosen name. You may cast the copy. (You still pay its \
                     costs.)";
        let robot_echo = "When this creature enters, choose a card name from among Who, What,                           and When. Create a copy of the card with the chosen name. You may                           cast the copy.";
        let halves = [
            split_half("Who", "{X}{W}", "Target player gains X life."),
            split_half("What", "{2}{R}", "Destroy target artifact."),
            split_half("When", "{2}{U}", "Counter target creature spell."),
            split_half("Where", "{3}{B}", "Destroy target land."),
            split_half("Why", "{1}{G}", "Destroy target enchantment."),
        ];
        let entries = [
            single(
                "Interrogation Robot",
                r#""Artifact", "Creature""#,
                "{3}",
                robot,
            ),
            single("Garth One-Eye", r#""Creature""#, "{W}{U}{B}{R}{G}", garth),
            // The same copy clause reached through a trigger's execute rather
            // than an activated ability, so the gate's walk is exercised past the
            // top-level ability list.
            single("Robot Echo", r#""Artifact", "Creature""#, "{3}", robot_echo),
            single(
                "Disenchant",
                r#""Instant""#,
                "{1}{W}",
                "Destroy target artifact or enchantment.",
            ),
            single(
                "Braingeyser",
                r#""Sorcery""#,
                "{X}{U}{U}",
                "Target player draws X cards.",
            ),
            single(
                "Terror",
                r#""Instant""#,
                "{1}{B}",
                "Destroy target nonartifact, nonblack creature.",
            ),
            single("Shivan Dragon", r#""Creature""#, "{4}{R}{R}", "Flying"),
            single(
                "Regrowth",
                r#""Sorcery""#,
                "{1}{G}",
                "Return target card from your graveyard to your hand.",
            ),
            single(
                "Black Lotus",
                r#""Artifact""#,
                "{0}",
                "{T}, Sacrifice this artifact: Add three mana of any one color.",
            ),
            format!(
                r#""Who // What // When // Where // Why": [{}]"#,
                halves.join(",")
            ),
        ];
        let json = format!(r#"{{ "data": {{ {} }} }}"#, entries.join(","));
        let mut file = NamedTempFile::new().expect("temporary AtomicCards fixture");
        file.write_all(json.as_bytes())
            .expect("write AtomicCards fixture");
        CardDatabase::from_mtgjson(file.path()).expect("raw MTGJSON fixture loads")
    }

    fn chain(def: &AbilityDefinition) -> Vec<&Effect> {
        let mut out = Vec::new();
        let mut node = Some(def);
        while let Some(d) = node {
            out.push(&*d.effect);
            node = d.sub_ability.as_deref();
        }
        out
    }

    /// `(CreateCardCopyByName clauses, copy-clause gaps)` across the face's
    /// abilities.
    fn copy_clauses(face: &CardFace) -> (usize, usize) {
        let effects: Vec<&Effect> = face.abilities.iter().flat_map(chain).collect();
        let copies = effects
            .iter()
            .filter(|e| matches!(e, Effect::CreateCardCopyByName { .. }))
            .count();
        let gaps = effects
            .iter()
            .filter(|e| matches!(e, Effect::Unimplemented { name, .. } if name == super::GAP_NAME))
            .count();
        (copies, gaps)
    }

    fn closed_domain(face: &CardFace) -> Vec<String> {
        face.abilities
            .iter()
            .flat_map(chain)
            .find_map(|e| match e {
                Effect::Choose {
                    choice_type: ChoiceType::CardName { options, .. },
                    ..
                } => Some(options.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// CR 709.4a + CR 709.3c: every name in Interrogation Robot's domain is a half
    /// of one five-half split card, so "the card with the chosen name" is that
    /// whole split card — which neither `CardLayout::Split` (two halves) nor a
    /// one-face registry copy can represent. The copy clause must be an honest gap
    /// so the card reads as unsupported, not as a silent no-op ("When", "Where"
    /// and "Why" are not even indexed) or a one-half copy ("Who", "What").
    #[test]
    fn interrogation_robot_copy_of_split_halves_stays_unsupported() {
        let db = load_fixture();
        // Reach-guard: the loader indexes only two of the five halves, and as
        // split halves — the premise the gate exists for.
        assert!(db.get_face_by_name("When").is_none());
        assert!(db.get_face_by_name("Who").is_some());

        let robot = db
            .get_face_by_name("Interrogation Robot")
            .expect("Interrogation Robot is indexed");
        // The parse itself still recognises the closed domain; only the copy the
        // engine cannot materialize is gapped.
        assert_eq!(
            closed_domain(robot),
            ["Who", "What", "When", "Where", "Why"]
        );
        assert_eq!(
            copy_clauses(robot),
            (0, 2),
            "the copy clause and the cast that depends on it are gaps"
        );
        assert!(
            card_face_has_unimplemented_parts(robot),
            "coverage reports Interrogation Robot as unsupported"
        );
        // Both stores of the face agree.
        let rules = db
            .get_by_name("Interrogation Robot")
            .expect("Interrogation Robot has rules");
        let CardLayout::Single(rules_face) = &rules.layout else {
            panic!("Interrogation Robot is single-faced");
        };
        assert_eq!(copy_clauses(rules_face), (0, 2));
    }

    /// The walk reaches a copy clause nested in a trigger's execute.
    #[test]
    fn a_triggered_copy_of_split_halves_is_gapped_too() {
        let db = load_fixture();
        let echo = db
            .get_face_by_name("Robot Echo")
            .expect("Robot Echo is indexed");
        let effects: Vec<&Effect> = echo
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .flat_map(chain)
            .collect();
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Choose { .. })),
            "reach guard: the trigger parsed its card-name choice: {effects:?}"
        );
        assert!(
            !effects.iter().any(|e| matches!(
                e,
                Effect::CreateCardCopyByName { .. } | Effect::CastFromZone { .. }
            )),
            "the triggered copy and its cast are gapped: {effects:?}"
        );
        assert!(
            effects.iter().any(
                |e| matches!(e, Effect::Unimplemented { name, .. } if name == super::GAP_NAME)
            ),
            "the gap is the gate's, not a parse failure: {effects:?}"
        );
    }

    /// Garth One-Eye names six single-faced cards: every copy is materializable,
    /// so the gate leaves its copy clause and its support status alone.
    #[test]
    fn garth_one_eye_single_faced_domain_is_untouched() {
        let db = load_fixture();
        let garth = db
            .get_face_by_name("Garth One-Eye")
            .expect("Garth One-Eye is indexed");
        assert_eq!(closed_domain(garth).len(), 6);
        assert_eq!(copy_clauses(garth), (1, 0), "the copy clause survives");
        assert!(
            !card_face_has_unimplemented_parts(garth),
            "Garth One-Eye stays supported: {:?}",
            garth.abilities
        );
    }
}
