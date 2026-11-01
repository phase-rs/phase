//! Digital-only Alchemy (no CR entry): the printed "Starting intensity N" line
//! must be read as `Keyword::StartingIntensity(N)` and stamped onto the card's
//! `intensity` field.
//!
//! The variant (`types/keywords.rs`), the seeding gate
//! (`game/printed_cards.rs::apply_card_face_to_object`), the storage
//! (`GameObject::intensity`), the reader (`QuantityRef::Intensity` in
//! `game/quantity.rs`) and the increment (`Effect::Intensify`) all pre-existed.
//! What was missing was the PARSE: no arm of the single keyword-line authority
//! (`parse_keyword_line_core`) read the printed line, so `Ribald Shanty` — an
//! instant that "deals damage equal to its intensity to target creature or
//! planeswalker" — resolved its `DealDamage` amount from an unseeded intensity of
//! 0 and dealt nothing, while the coverage report rendered it
//! `supported: true, gap_count: 0`. That silent-wrong shape is why these are
//! RUNTIME witnesses through the real cast pipeline rather than AST-shape
//! assertions.
//!
//! "Intensity" has zero matches in `docs/MagicCompRules.txt`, so no CR number is
//! cited for the mechanic itself; the only CR rule referenced below is CR 704.5g
//! for the destruction the damage causes.
//!
//! Oracle text is the card's printed text as carried by the engine's export, not
//! a paraphrase.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, IntensityScope, ObjectScope, QuantityExpr, QuantityRef,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim `Ribald Shanty` Oracle text (YLCI, the Chorus precon face).
const RIBALD_SHANTY: &str = "Starting intensity 2\n\
Ribald Shanty deals damage equal to its intensity to target creature or planeswalker.\n\
All Chorus cards you own intensify by 1.";

fn red_mana() -> ManaUnit {
    ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])
}

/// Put `Ribald Shanty` in P0's hand through the PRODUCTION parse inputs: the
/// verbatim Oracle text plus MTGJSON's keyword name for the mechanic. The name
/// hint is load-bearing — `database/synthesis.rs` lowercases
/// `AtomicCards.keywords` ("Intensity") before handing it to the parser, and an
/// empty list takes the `parse_mtgjson_missing_standalone_keyword_line` branch
/// instead of the route this card takes in production.
fn add_ribald_shanty(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Ribald Shanty", true, RIBALD_SHANTY)
        .with_subtypes(vec!["Chorus"])
        .from_oracle_text_with_keywords(&["Intensity"], RIBALD_SHANTY)
        .id()
}

/// The spell's own printed intensity, read off the object the parse seeded.
fn spell_intensity(runner: &engine::game::scenario::GameRunner, spell: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&spell)
        .expect("the spell is in hand")
        .intensity
}

fn damage_marked(runner_state: &engine::types::game_state::GameState, id: ObjectId) -> u32 {
    runner_state
        .objects
        .get(&id)
        .expect("object exists")
        .damage_marked
}

/// Fails the test if any ability in the chain lowered to `Effect::Unimplemented`
/// — the reach guard that keeps the shape assertions below from passing on a
/// parse that silently produced nothing.
fn assert_no_unimplemented(definition: &AbilityDefinition, path: &str) {
    assert!(
        !matches!(&*definition.effect, Effect::Unimplemented { .. }),
        "{path} must not be an Effect::Unimplemented residual: {:?}",
        definition.effect
    );
    if let Some(sub) = definition.sub_ability.as_deref() {
        assert_no_unimplemented(sub, &format!("{path}/sub"));
    }
    if let Some(else_ability) = definition.else_ability.as_deref() {
        assert_no_unimplemented(else_ability, &format!("{path}/else"));
    }
}

/// RUNTIME: the printed N reaches the damage the spell deals.
///
/// Reverted (no parser arm), `obj.intensity` is never seeded: the amount resolves
/// to 0 and the 0/4 victim takes no damage at all. The second assertion pins the
/// binding to the SOURCE's intensity — a decoy carrying a different intensity (5)
/// must not be read instead.
#[test]
fn ribald_shanty_deals_its_starting_intensity() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_ribald_shanty(&mut scenario);
    let victim = scenario.add_creature(P1, "Victim", 0, 4).id();
    let decoy = scenario.add_creature(P1, "Decoy", 0, 4).id();
    scenario.with_mana_pool(P0, vec![red_mana()]);
    let mut runner = scenario.build();

    // Reach guard for the parse→state half: the printed 2 is on the card before
    // any cast happens, so a 0 here would make the damage assertion below pass
    // for the wrong reason.
    assert_eq!(
        spell_intensity(&runner, spell),
        2,
        "the printed \"Starting intensity 2\" must be stamped onto the card"
    );

    runner
        .state_mut()
        .objects
        .get_mut(&decoy)
        .expect("decoy on the battlefield")
        .intensity = 5;
    assert_eq!(
        spell_intensity(&runner, decoy),
        5,
        "the decoy's planted intensity must be observable, so the victim's 2 \
         cannot be satisfied by any other object's value"
    );

    let outcome = runner.cast(spell).target_object(victim).resolve();

    assert_eq!(
        damage_marked(outcome.state(), victim),
        2,
        "Ribald Shanty must deal damage equal to its printed starting intensity \
         (2). Reverted, the amount resolves from an unseeded intensity of 0 and \
         the victim takes nothing."
    );
    assert_eq!(
        damage_marked(outcome.state(), decoy),
        0,
        "an unrelated object's intensity (5) must never be read as the source's"
    );
}

/// RUNTIME: the damage is real damage, not just a number on the state.
///
/// CR 704.5g: a creature with damage marked on it greater than or equal to its
/// toughness has been dealt lethal damage and is destroyed. 2 damage kills a 2/2;
/// on revert the victim takes 0 and stays on the battlefield.
#[test]
fn ribald_shanty_destroys_a_two_toughness_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_ribald_shanty(&mut scenario);
    let victim = scenario.add_creature(P1, "Two Two", 2, 2).id();
    scenario.with_mana_pool(P0, vec![red_mana()]);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(victim).resolve();

    // CR 704.5g: lethal damage destroys the 2/2.
    outcome.assert_zone(&[victim], Zone::Graveyard);
    // The damage went to the creature and not to its controller: a wrong
    // recipient that still "dealt 2" would move P1's life total.
    outcome.assert_life_delta(P1, 0);
}

/// SHAPE: the parse of the real card. The runtime rows above depend on this
/// chain, and this row pins that the chain was not produced by gluing the
/// keyword line into a parsed ability's `description` — the pre-fix shape, where
/// `description` began with "Starting intensity 2\n".
#[test]
fn ribald_shanty_parses_the_keyword_and_keeps_its_damage_chain() {
    let parsed = parse_oracle_text(
        RIBALD_SHANTY,
        "Ribald Shanty",
        &["intensity".to_string()],
        &["Instant".to_string()],
        &["Chorus".to_string()],
    );

    let keyword = parsed
        .extracted_keywords
        .iter()
        .find(|keyword| matches!(keyword, Keyword::StartingIntensity(_)))
        .unwrap_or_else(|| {
            panic!(
                "the printed line must be extracted as a keyword; got {:?}",
                parsed.extracted_keywords
            )
        });
    assert_eq!(*keyword, Keyword::StartingIntensity(2));

    // Reach guard: this card parses to exactly one spell ability and nothing
    // else, so the chain assertions below cover the whole parse.
    assert_eq!(
        (
            parsed.abilities.len(),
            parsed.triggers.len(),
            parsed.statics.len(),
            parsed.replacements.len()
        ),
        (1, 0, 0, 0),
        "Ribald Shanty is a single spell ability and nothing else"
    );

    let ability = &parsed.abilities[0];
    assert_no_unimplemented(ability, "ability");

    let Effect::DealDamage { amount, .. } = &*ability.effect else {
        panic!("expected DealDamage, got {:?}", ability.effect);
    };
    assert_eq!(
        amount,
        &QuantityExpr::Ref {
            qty: QuantityRef::Intensity {
                scope: ObjectScope::Source
            }
        },
        "the amount must still read the source's intensity"
    );

    let sub = ability
        .sub_ability
        .as_deref()
        .expect("the Chorus intensify sub-ability is part of the printed chain");
    let Effect::Intensify { scope, amount } = &*sub.effect else {
        panic!("expected the Intensify sub-ability, got {:?}", sub.effect);
    };
    assert_eq!(
        scope,
        &IntensityScope::OwnedSubtype {
            subtype: "Chorus".to_string()
        }
    );
    assert_eq!(amount, &QuantityExpr::Fixed { value: 1 });

    // The consumed line feeds the keyword, NOT the ability description. Pre-fix
    // the description was the glued text and opened with "Starting intensity 2".
    let description = ability
        .description
        .as_deref()
        .expect("the spell ability keeps its printed rules text");
    assert_eq!(
        description,
        "~ deals damage equal to its intensity to target creature or planeswalker.\n\
         All Chorus cards you own intensify by 1."
    );
    assert!(
        !description.contains("Starting intensity"),
        "the consumed keyword line must not survive inside the ability description: {description:?}"
    );
}

/// Verbatim `Legion's Chant` Oracle text (YLCI, the Chorus precon face).
const LEGIONS_CHANT: &str = "Starting intensity 3\n\
Return any number of creature cards with total mana value X or less from your graveyard to the battlefield, where X is Legion's Chant's intensity.\n\
All Chorus cards you own intensify by 1.";

/// The honest-gap name of the resolution aggregate-budget refusal
/// (`parser::oracle_effect::lower::UNENFORCED_AGGREGATE_RESOLUTION_SELECTION_GAP`).
const UNENFORCED_AGGREGATE_SELECTION_GAP: &str = "unenforced_aggregate_resolution_selection";

/// Whether any node of the lowered chain carries a `ChangeZone` zone selection.
fn chain_has_change_zone(definition: &AbilityDefinition) -> bool {
    if matches!(&*definition.effect, Effect::ChangeZone { .. }) {
        return true;
    }
    definition
        .sub_ability
        .as_deref()
        .is_some_and(chain_has_change_zone)
        || definition
            .else_ability
            .as_deref()
            .is_some_and(chain_has_change_zone)
        || definition.mode_abilities.iter().any(chain_has_change_zone)
}

/// SHAPE + COVERAGE HONESTY: `Legion's Chant`'s clause is an UNTARGETED,
/// resolution-timed pick whose printed cap is AGGREGATE — "any number of creature
/// cards with total mana value X or less", X being the spell's intensity (3).
/// CR 202.3 makes that cap a property of the CHOSEN SET, so it cannot be folded
/// into the per-card filter: at intensity 3 two eligible mana-value-2 creature
/// cards pass every individual filter while totalling 4, and the resolution
/// selection (`WaitingFor::EffectZoneChoice` → `GameAction::SelectCards`) checks
/// only cardinality, distinctness and membership — neither it nor the prompt
/// carries `target_constraints`, whose only enforcement site is the
/// target-announcement validator (CR 115.1 + CR 601.2c), unreachable for a
/// resolution pick.
///
/// So the clause is refused at the lowering authority and the card is honestly
/// unsupported: coverage keys `supported` on `Effect::Unimplemented`, which is
/// exactly what the gap-name assertion pins. Reverted, the head is a `ChangeZone`
/// again and BOTH the gap-name and the no-selection assertions fail.
///
/// A cast-level `SelectCards` row distinguishing the legal totals {0, 3} from the
/// illegal 4 is deliberately absent: it cannot exist while support is withheld,
/// because the parser no longer produces the `ChangeZone` selection such a cast
/// would submit. Reverting the refusal is what re-enables that test.
#[test]
fn legions_chant_class_fails_closed_instead_of_claiming_an_unenforced_budget() {
    let parsed = parse_oracle_text(
        LEGIONS_CHANT,
        "Legion's Chant",
        &["intensity".to_string()],
        &["Sorcery".to_string()],
        &["Chorus".to_string()],
    );

    // Reach guard 1: the keyword line is still read, so a failure below cannot be
    // "the whole card failed to parse".
    assert!(
        parsed
            .extracted_keywords
            .contains(&Keyword::StartingIntensity(3)),
        "the printed \"Starting intensity 3\" must still be extracted; got {:?}",
        parsed.extracted_keywords
    );

    // Reach guard 2: the effect clause lowered to exactly one ability chain, so the
    // assertions below are about that clause and not about an empty parse.
    assert_eq!(
        (
            parsed.abilities.len(),
            parsed.triggers.len(),
            parsed.statics.len(),
            parsed.replacements.len()
        ),
        (1, 0, 0, 0),
        "Legion's Chant is a single spell ability and nothing else"
    );

    let ability = &parsed.abilities[0];
    let Effect::Unimplemented { name, description } = &*ability.effect else {
        panic!(
            "the aggregate-budget clause must strict-fail, not lower to a \
             selection the resolver cannot police; got {:?}",
            ability.effect
        );
    };
    assert_eq!(
        name, UNENFORCED_AGGREGATE_SELECTION_GAP,
        "the gap must be the sanctioned strict-failure marker for this class"
    );
    assert!(
        description
            .as_deref()
            .is_some_and(|text| text.contains("total mana value X or less")),
        "the gap must carry the printed clause so the report names what is \
         missing; got {description:?}"
    );

    // Positive reach-guard for the negative assertion below: the rest of the
    // printed chain survived, so "no ChangeZone" is a statement about the refused
    // clause rather than about a chain that was dropped wholesale.
    let sub = ability
        .sub_ability
        .as_deref()
        .expect("the Chorus intensify sub-ability is part of the printed chain");
    assert!(
        matches!(&*sub.effect, Effect::Intensify { .. }),
        "the intensify sibling must survive the refusal; got {:?}",
        sub.effect
    );

    // No silently-supported zone selection may remain anywhere in the chain: that
    // is the shape whose submission authority ignores the aggregate constraint.
    assert!(
        !chain_has_change_zone(ability),
        "no ChangeZone selection may survive once the aggregate budget is refused"
    );
}
