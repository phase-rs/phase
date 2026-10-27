//! CR 100.7 + CR 608.2c: a put-to-battlefield clause whose qualifier is
//! "combined into one creature" (Monster Mash-Up; Grusilda, Monster Masher)
//! instructs the player to create a combined creature. The engine's merge
//! machinery does not cover it: `Effect::CombineHost` needs an existing
//! battlefield Host, mutate merges onto an existing permanent, and meld enters a
//! predefined result face. None enters two arbitrary creature cards as one
//! creature with summed power and toughness. CR 100.7 says casual-play cards
//! "may have features and text that aren't covered by these rules", and the
//! keyword that causes a CR 730 merge is mutate (CR 730.1), so the qualifier
//! must NOT be read as CR 730.
//! CR 608.2c requires reading the whole text, so silently lowering a plain
//! `ChangeZone` (two separate creatures) would execute a different instruction
//! than the card prints.
//!
//! These tests pin the fail-closed behavior through the PUBLIC coverage
//! authority (`card_face_gaps`) plus the parsed effect, with a paired control
//! that an ordinary put-to-battlefield clause is never refused.

use engine::game::coverage::card_face_gaps;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
use engine::types::card::CardFace;

/// The gap label the parser emits for the refused qualifier.
const COMBINE_GAP: &str = "put_battlefield_combined_creature";

const MONSTER_MASH_UP: &str = "Put two target creature cards from graveyards onto the battlefield combined into one creature under your control. (The creature has the combined power and toughness of its cards. It has their names, types, text boxes, etc. They separate again if the creature leaves the battlefield.)";

const GRUSILDA: &str = "Combined, enchanted, and equipped creatures you control have menace.\n{3}{B}{R}, {T}: Put two target creature cards from graveyards onto the battlefield combined into one creature under your control. (Its power is equal to their total power, its toughness is equal to their total toughness, and it has their names, mana costs, types, text boxes, etc.)";

/// The bare operative clause, reused by the class-not-card and boundary rows.
const OPERATIVE_CLAUSE: &str =
    "Put two target creature cards from graveyards onto the battlefield combined into one creature under your control.";

fn spell_face(name: &str, oracle: &str) -> CardFace {
    face(name, oracle, &["Sorcery".to_string()], &[])
}

fn creature_face(name: &str, oracle: &str, subtypes: &[&str]) -> CardFace {
    face(
        name,
        oracle,
        &["Creature".to_string()],
        &subtypes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
}

fn face(name: &str, oracle: &str, types: &[String], subtypes: &[String]) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], types, subtypes);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// Does this ability, or anything it chains to, carry the combine gap?
fn chain_has_combine_gap(def: &AbilityDefinition) -> bool {
    matches!(
        &*def.effect,
        Effect::Unimplemented { name, .. } if name == COMBINE_GAP
    ) || def
        .sub_ability
        .as_deref()
        .is_some_and(chain_has_combine_gap)
}

fn chain_has_change_zone(def: &AbilityDefinition) -> bool {
    matches!(&*def.effect, Effect::ChangeZone { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_change_zone)
}

fn describe(face: &CardFace) -> Vec<String> {
    face.abilities
        .iter()
        .map(|d| format!("kind={:?} cost={:?} effect={:?}", d.kind, d.cost, d.effect))
        .collect()
}

/// (a) The card's own text reports the refused qualifier as a coverage gap, and
/// the located spell ability's effect is the honest `Unimplemented`.
#[test]
fn monster_mash_up_reports_the_combine_gap() {
    let face = spell_face("Monster Mash-Up", MONSTER_MASH_UP);

    // Reach-guard: the spell ability exists and ours is reached (not a collapsed
    // or absent chain), so the assertions below read a clause the parser saw.
    let spell = face
        .abilities
        .iter()
        .find(|d| d.kind == AbilityKind::Spell)
        .unwrap_or_else(|| panic!("reach-guard: no spell ability, got {:?}", describe(&face)));
    assert!(
        chain_has_combine_gap(spell),
        "the spell ability must refuse the combined-creature clause, got {:?}",
        describe(&face)
    );

    let gaps = card_face_gaps(&face);
    assert!(
        gaps.iter()
            .any(|gap| gap == &format!("Effect:{COMBINE_GAP}")),
        "the combined-creature clause must surface a coverage gap, got {gaps:?}"
    );
}

/// (b) Grusilda's ACTIVATED ability is the one that must carry the combine gap:
/// its static line has its own gap, so a card-wide check alone would pass for the
/// wrong reason.
#[test]
fn grusilda_activated_ability_reports_the_combine_gap() {
    let face = creature_face("Grusilda, Monster Masher", GRUSILDA, &["Zombie", "Villain"]);

    // Reach-guard: locate the activated ability and confirm its cost parsed, so
    // the combine gap below is read off the activated body and not the static
    // line's own gap.
    let activated = face
        .abilities
        .iter()
        .find(|d| d.kind == AbilityKind::Activated)
        .unwrap_or_else(|| {
            panic!(
                "reach-guard: no activated ability parsed, got {:?}",
                describe(&face)
            )
        });
    assert!(
        activated.cost.is_some(),
        "reach-guard: the activated ability's cost must parse, got {:?}",
        describe(&face)
    );
    assert!(
        chain_has_combine_gap(activated),
        "the activated ability must refuse the combined-creature clause, got {:?}",
        describe(&face)
    );

    let gaps = card_face_gaps(&face);
    assert!(
        gaps.iter().any(|gap| gap == &format!("Effect:{COMBINE_GAP}")),
        "the activated ability's combined-creature clause must surface a coverage gap, got {gaps:?}"
    );
}

/// (c) Class-not-card: the operative clause under a neutral name, wrapped both
/// as a spell and as an activated ability, reaches the same gap category.
#[test]
fn neutral_wrappers_reach_the_same_gap_class() {
    let spell = spell_face("Neutral Combine Spell", OPERATIVE_CLAUSE);
    assert!(
        spell.abilities.iter().any(chain_has_combine_gap),
        "spell wrapper: the clause must be refused on its pattern, got {:?}",
        describe(&spell)
    );

    let activated_text = format!("{{3}}{{B}}{{R}}, {{T}}: {OPERATIVE_CLAUSE}");
    let permanent = creature_face("Neutral Combine Permanent", &activated_text, &["Zombie"]);
    let activated = permanent
        .abilities
        .iter()
        .find(|d| d.kind == AbilityKind::Activated)
        .unwrap_or_else(|| {
            panic!(
                "activated wrapper: no activated ability parsed, got {:?}",
                describe(&permanent)
            )
        });
    assert!(
        chain_has_combine_gap(activated),
        "activated wrapper: the clause must be refused on its pattern, got {:?}",
        describe(&permanent)
    );
}

/// (d) Token boundary: the exact phrase and its comma-terminated form are
/// refused, while a longer final token keeps the ordinary parse.
#[test]
fn qualifier_token_boundary_is_respected() {
    // Reach-guard / positive: the exact phrase is refused.
    let exact = spell_face("Boundary Exact", OPERATIVE_CLAUSE);
    assert!(
        exact.abilities.iter().any(chain_has_combine_gap),
        "the exact qualifier must be refused, got {:?}",
        describe(&exact)
    );

    // Clause-boundary form: fail-closed must not fail open when the qualifier is
    // followed by a comma (the sequence splitter may hand the put clause over with
    // the comma already cut, so this row pins the outcome, not one lookahead arm).
    let comma_terminated = spell_face(
        "Boundary Comma",
        "Put two target creature cards from graveyards onto the battlefield combined into one creature, then draw a card.",
    );
    assert!(
        comma_terminated.abilities.iter().any(chain_has_combine_gap),
        "the comma-terminated qualifier must be refused, got {:?}",
        describe(&comma_terminated)
    );

    // Longer final token ("one creatures"): not the printed phrase, so the
    // ordinary put parse stands. Paired with the exact-phrase refusal above, this
    // shows the guard is a token boundary and not a substring test.
    let longer_token = spell_face(
        "Boundary Longer Token",
        "Put two target creature cards from graveyards onto the battlefield combined into one creatures under your control.",
    );
    assert!(
        !longer_token.abilities.iter().any(chain_has_combine_gap),
        "a longer final token must not be refused, got {:?}",
        describe(&longer_token)
    );
    assert!(
        longer_token.abilities.iter().any(chain_has_change_zone),
        "a longer final token must keep the ordinary ChangeZone parse, got {:?}",
        describe(&longer_token)
    );
}

/// (e) Control: an ordinary put-to-battlefield clause still parses to a
/// `ChangeZone` and is never refused.
#[test]
fn ordinary_put_to_battlefield_is_not_refused() {
    let face = spell_face(
        "Ordinary Reanimation",
        "Put target creature card from a graveyard onto the battlefield under your control.",
    );
    assert!(
        face.abilities.iter().any(chain_has_change_zone),
        "the ordinary put clause must parse to a ChangeZone, got {:?}",
        describe(&face)
    );
    assert!(
        !face.abilities.iter().any(chain_has_combine_gap),
        "the ordinary put clause must not be refused, got {:?}",
        describe(&face)
    );
    let gaps = card_face_gaps(&face);
    assert!(
        !gaps.iter().any(|gap| gap.contains(COMBINE_GAP)),
        "the ordinary put clause must not report the combine gap, got {gaps:?}"
    );
}
