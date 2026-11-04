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

use crate::types::ability::{AbilityDefinition, ChoiceType, Effect};
use crate::types::card::CardFace;

/// Gap name recorded for a copy clause the database cannot materialize.
const GAP_NAME: &str = "CreateCardCopyByName";

/// Replace every `Effect::CreateCardCopyByName` on `face` whose name domain
/// includes a name `is_single_faced_card` rejects with an honest gap. Returns
/// whether anything was replaced.
///
/// `is_single_faced_card` answers, for a printed card name, whether the database
/// holds it as a single-faced card (and so can materialize a copy of the whole
/// card from one face). The caller owns the lookup because the two database
/// builders index faces differently (`oracle_gen`'s export map, the MTGJSON
/// loader's layouts).
pub fn gate_unmaterializable_name_copies(
    face: &mut CardFace,
    is_single_faced_card: &impl Fn(&str) -> bool,
) -> bool {
    let mut changed = false;
    for ability in &mut face.abilities {
        changed |= gate_chain(ability, is_single_faced_card);
    }
    for trigger in &mut face.triggers {
        if let Some(execute) = trigger.execute.as_deref_mut() {
            changed |= gate_chain(execute, is_single_faced_card);
        }
    }
    changed
}

/// Walk one ability chain in resolution order. The most recent card-name choice
/// in the chain is the domain a following "the card with the chosen name" copy
/// reads.
fn gate_chain(head: &mut AbilityDefinition, is_single_faced_card: &impl Fn(&str) -> bool) -> bool {
    let mut changed = false;
    // `None` = no card-name choice yet in this chain.
    let mut domain_materializable: Option<bool> = None;
    let mut node = Some(head);
    while let Some(def) = node {
        let materializable = match &*def.effect {
            Effect::Choose {
                choice_type: ChoiceType::CardName { options, .. },
                ..
            } => {
                // CR 201.4: an OPEN domain ranges over every name in the Oracle card
                // reference, which the scoped face registry is never preloaded
                // with — so it is not materializable either.
                domain_materializable = Some(
                    !options.is_empty() && options.iter().all(|name| is_single_faced_card(name)),
                );
                None
            }
            Effect::CreateCardCopyByName {
                name: Some(name), ..
            } => Some(is_single_faced_card(name)),
            // The parser already gaps a chosen-name copy with no choice before it
            // (`unimplement_card_copy_without_a_name_choice`); a missing choice
            // here is that same unreadable name.
            Effect::CreateCardCopyByName { name: None, .. } => {
                Some(domain_materializable.unwrap_or(false))
            }
            _ => None,
        };
        if materializable == Some(false) {
            let fragment = def
                .description
                .clone()
                .unwrap_or_else(|| "create a copy of the card with the chosen name".to_string());
            *def.effect = Effect::unimplemented(GAP_NAME, fragment);
            changed = true;
        }
        if let Some(else_branch) = def.else_ability.as_deref_mut() {
            changed |= gate_chain(else_branch, is_single_faced_card);
        }
        node = def.sub_ability.as_deref_mut();
    }
    changed
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
        assert_eq!(copy_clauses(robot), (0, 1), "the copy clause is a gap");
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
        assert_eq!(copy_clauses(rules_face), (0, 1));
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
