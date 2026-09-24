//! **Garth One-Eye** — "{T}: Choose a card name that hasn't been chosen from
//! among Disenchant, Braingeyser, Terror, Shivan Dragon, Regrowth, and Black
//! Lotus. Create a copy of the card with the chosen name. You may cast the copy.
//! (You still pay its costs.)"
//!
//! Field report (2026-09-21): tapping Garth asked for a card name with an EMPTY
//! domain — the generic "name any card" prompt, whose only listed answers were
//! the names already in the game — and then nothing happened. The middle
//! instruction was recorded as `Effect::Unimplemented { unparsed_verb_arguments
//! }`: "Create a copy of the card with the chosen name" had no verb grammar, so
//! no copy was ever created and the "you may cast the copy" tail had nothing to
//! cast.
//!
//! Three rules are under test.
//!
//! CR 201.2a — the domain is CLOSED. The six names are printed on the card, and
//! a prompt that accepts a seventh is not that card's ability.
//!
//! CR 609.3 — "that hasn't been chosen" removes each committed name from the
//! domain of the next activation.
//!
//! CR 707.12 + CR 704.5e — the copy is a copy, not a conjured card: it is
//! materialized from the card registry into exile, it may be cast during the
//! same resolution (CR 608.2: no state-based action runs mid-resolution), and if
//! it is not cast the next SBA check removes it rather than leaving a stray card
//! in exile for the rest of the game.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    CastFromZoneDriver, ChoiceType, Effect, NameDistinctness, TargetFilter,
};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::card::CardFace;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GARTH: &str = "{T}: Choose a card name that hasn't been chosen from among Disenchant, \
Braingeyser, Terror, Shivan Dragon, Regrowth, and Black Lotus. Create a copy of the card with \
the chosen name. You may cast the copy.";

/// The same three instructions over names the test fixture actually carries, so
/// the runtime tests can materialize a real card face.
const GARTH_FIXTURE: &str = "{T}: Choose a card name that hasn't been chosen from among \
Disenchant, Lightning Bolt, and Grizzly Bears. Create a copy of the card with the chosen name. \
You may cast the copy.";

const FIXTURE_NAMES: [&str; 3] = ["Disenchant", "Lightning Bolt", "Grizzly Bears"];

fn load_test_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json");
        CardDatabase::from_mtgjson(&path).expect("test fixture database should load")
    })
}

/// What `printed_cards::build_conjure_registry` seeds from the closed domain at
/// game init; a scenario builds no registry of its own.
fn fixture_registry() -> Arc<HashMap<String, CardFace>> {
    let db = load_test_db();
    Arc::new(
        FIXTURE_NAMES
            .iter()
            .map(|name| {
                let face = db
                    .get_face_by_name(name)
                    .unwrap_or_else(|| panic!("fixture has {name}"))
                    .clone();
                (name.to_lowercase(), face)
            })
            .collect(),
    )
}

#[test]
fn garth_parses_a_closed_distinct_name_domain_and_a_card_copy() {
    let parsed = parse_oracle_text(GARTH, "Garth One-Eye", &[], &["Creature".to_string()], &[]);
    let ability = parsed
        .abilities
        .first()
        .expect("Garth has one activated ability");

    // CR 201.2a: the closed domain, in PRINTED order and printed case. Lowercase
    // names here would mean the parser read them off its own lowercased copy of
    // the text — and the prompt would then offer "black lotus".
    let Effect::Choose {
        choice_type: ChoiceType::CardName {
            options,
            distinctness,
        },
        persist,
        ..
    } = &*ability.effect
    else {
        panic!("expected a card-name Choose, got {:?}", ability.effect);
    };
    assert_eq!(
        options,
        &[
            "Disenchant".to_string(),
            "Braingeyser".to_string(),
            "Terror".to_string(),
            "Shivan Dragon".to_string(),
            "Regrowth".to_string(),
            "Black Lotus".to_string(),
        ],
        "the six printed names, in printed order and case"
    );
    // CR 609.3: "that hasn't been chosen".
    assert_eq!(*distinctness, NameDistinctness::DistinctFromSourceHistory);
    // The chosen name has to survive the choice for the next clause to read it.
    assert!(*persist, "a card-name choice persists on the source");

    // CR 707.12: the copy, created from the name rather than from an object.
    let create = ability
        .sub_ability
        .as_deref()
        .expect("the copy clause follows the choice");
    let Effect::CreateCardCopyByName {
        name, destination, ..
    } = &*create.effect
    else {
        panic!(
            "expected CreateCardCopyByName, got {:?} — the recorded gap was \
             `unparsed_verb_arguments` on \"Create a copy of the card with the chosen name\"",
            create.effect
        );
    };
    assert_eq!(*name, None, "the name comes from the choice, not the text");
    assert_eq!(*destination, Zone::Exile);

    // CR 118.9b: "you may cast the copy" — a normal, PAID cast ("You still pay
    // its costs"), bound to the copy this chain created. `ParentTarget` would
    // bind to the ability's declared targets, and it declares none.
    let cast = create
        .sub_ability
        .as_deref()
        .expect("the cast permission follows the copy");
    let Effect::CastFromZone {
        target,
        without_paying_mana_cost,
        driver,
        ..
    } = &*cast.effect
    else {
        panic!("expected CastFromZone, got {:?}", cast.effect);
    };
    assert_eq!(*target, TargetFilter::LastCreated);
    assert!(
        !*without_paying_mana_cost,
        "the copy is cast for its own cost"
    );
    // CR 608.2g: cast as the ability resolves. A lingering permission would
    // outlive the copy — CR 704.5e removes it at the next SBA check.
    assert_eq!(*driver, CastFromZoneDriver::DuringResolution);
    assert!(cast.optional, "\"you MAY cast the copy\"");
}

/// The rest of the class, measured rather than assumed — these are every other
/// card in the corpus whose text carries one of the two phrases this PR teaches
/// the parser, and they are the whole of its claimed parse impact.
///
/// Ersta is Garth without the distinctness cue and with a free recast. The
/// Interrogation Robot prints the SAME closed domain one word shorter ("chosen
/// among", no "from"); accepting only the longer wording would parse its copy
/// clause while leaving its domain open, which is a worse shape than either
/// whole answer.
#[test]
fn the_rest_of_the_closed_domain_class_parses_the_same_way() {
    let ersta = parse_oracle_text(
        "[-3]: Choose a card name from among Enlightened Tutor, Mystical Tutor, Booster Tutor, \
         Imperial Recruiter, and Worldly Tutor. Create a copy of the card with the chosen name. \
         You may cast the copy without paying its mana cost.",
        "Ersta, Friend to All",
        &[],
        &["Planeswalker".to_string()],
        &[],
    );
    let ability = ersta.abilities.first().expect("the minus ability parsed");
    let Effect::Choose {
        choice_type: ChoiceType::CardName {
            options,
            distinctness,
        },
        ..
    } = &*ability.effect
    else {
        panic!("expected a card-name Choose, got {:?}", ability.effect);
    };
    assert_eq!(options.len(), 5, "the five printed names: {options:?}");
    assert_eq!(
        options[0], "Enlightened Tutor",
        "printed order and case: {options:?}"
    );
    // CR 609.3: Ersta prints no "that hasn't been chosen" cue, so repeats are legal.
    assert_eq!(*distinctness, NameDistinctness::Repeatable);
    assert!(
        matches!(
            ability.sub_ability.as_deref().map(|d| &*d.effect),
            Some(Effect::CreateCardCopyByName { .. })
        ),
        "the copy clause: {:?}",
        ability.sub_ability.as_deref().map(|d| &*d.effect)
    );

    let robot = parse_oracle_text(
        "{T}, Discard a card: Choose a card name that hasn't been chosen among Who, What, When, \
         Where, and Why. Create a copy of the card with the chosen name. You may cast the copy.",
        "Interrogation Robot",
        &[],
        &["Artifact".to_string(), "Creature".to_string()],
        &[],
    );
    let ability = robot.abilities.first().expect("the tap ability parsed");
    let Effect::Choose {
        choice_type: ChoiceType::CardName {
            options,
            distinctness,
        },
        ..
    } = &*ability.effect
    else {
        panic!("expected a card-name Choose, got {:?}", ability.effect);
    };
    assert_eq!(
        options,
        &[
            "Who".to_string(),
            "What".to_string(),
            "When".to_string(),
            "Where".to_string(),
            "Why".to_string(),
        ],
        "\"chosen among\" is the same closed domain as \"chosen from among\""
    );
    assert_eq!(*distinctness, NameDistinctness::DistinctFromSourceHistory);
}

/// Svega names its domain by planeswalker type ("Choose an Elspeth, Teferi,
/// Liliana, Chandra, or Garruk planeswalker card name with mana value X"), which
/// no card-name phrase-table arm claims. Its copy clause must therefore stay
/// `Unimplemented`: a `CreateCardCopyByName` with no name to read would resolve
/// as a silent no-op AND remove the gap that keeps `cargo coverage` honest about
/// this card. Coverage honesty is the reason this is a test and not a comment.
#[test]
fn a_copy_clause_without_a_card_name_choice_stays_unimplemented() {
    let svega = parse_oracle_text(
        "[-X]: Choose an Elspeth, Teferi, Liliana, Chandra, or Garruk planeswalker card name with \
         mana value X. Create a copy of the card with the chosen name. You may cast the copy \
         without paying its mana cost.",
        "Svega, the Unconventional",
        &[],
        &["Planeswalker".to_string()],
        &[],
    );
    let mut saw_copy = false;
    let mut saw_gap = false;
    for a in &svega.abilities {
        let mut node = Some(a);
        while let Some(d) = node {
            match &*d.effect {
                Effect::CreateCardCopyByName { .. } => saw_copy = true,
                Effect::Unimplemented { name, .. } if name == "unparsed_verb_arguments" => {
                    saw_gap = true
                }
                _ => {}
            }
            node = d.sub_ability.as_deref();
        }
    }
    assert!(
        !saw_copy,
        "no card-name choice reaches this copy clause, so it must not claim to \
         create one: {:?}",
        svega.abilities
    );
    // Reach-guard for the negative above: the clause was parsed and classified,
    // not simply absent from the tree.
    assert!(
        saw_gap,
        "the copy clause is still recorded as an honest gap: {:?}",
        svega.abilities
    );
}

/// The open "choose a card name" prompt (Pithing Needle, Meddling Mage) must be
/// untouched: no option list, and the client still supplies the domain.
#[test]
fn an_open_card_name_choice_keeps_its_empty_domain() {
    let parsed = parse_oracle_text(
        "Choose a card name.",
        "Open Name Choice",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let found = parsed.abilities.iter().any(|a| {
        matches!(
            &*a.effect,
            Effect::Choose {
                choice_type: ChoiceType::CardName { options, distinctness },
                ..
            } if options.is_empty() && *distinctness == NameDistinctness::Repeatable
        )
    });
    assert!(found, "the open form stays open: {:?}", parsed.abilities);
}

fn garth_on_the_battlefield() -> (GameRunner, ObjectId) {
    garth_with_mana(0)
}

/// `green` units of green mana in the pool — enough to actually pay for the copy
/// when the test exercises the cast. With an empty pool the copy is uncastable,
/// so declining is the only outcome the engine can reach.
fn garth_with_mana(green: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if green > 0 {
        scenario.with_mana_pool(
            P0,
            (0..green)
                .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
                .collect(),
        );
    }
    let garth = {
        let mut card = scenario.add_creature(P0, "Garth One-Eye", 5, 5);
        card.from_oracle_text(GARTH_FIXTURE);
        card.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().card_face_registry = fixture_registry();
    (runner, garth)
}

/// Activate the {T} ability and let it resolve up to the name prompt.
fn activate_to_the_prompt(runner: &mut GameRunner, garth: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: garth,
            ability_index: 0,
        })
        .expect("the tap ability is activatable");
    runner.resolve_top();
}

fn offered_names(runner: &GameRunner) -> Vec<String> {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice { options, .. } => options.clone(),
        other => panic!("expected a NamedChoice, got {other:?}"),
    }
}

#[test]
fn the_prompt_offers_exactly_the_printed_names_and_creates_the_chosen_copy() {
    let (mut runner, garth) = garth_on_the_battlefield();
    activate_to_the_prompt(&mut runner, garth);

    assert_eq!(
        offered_names(&runner),
        FIXTURE_NAMES.map(str::to_string).to_vec(),
        "the closed domain reaches the prompt — before the fix it was empty, and \
         the only answers offered were the names already in the game"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: "Lightning Bolt".to_string(),
        })
        .expect("a printed name is a legal answer");

    // CR 707.12: a real Lightning Bolt object, materialized from the registry and
    // marked as a copy so CR 704.5e can sweep it.
    let copy = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == "Lightning Bolt")
        .map(|(id, _)| *id)
        .expect("the copy was created");
    let copy_obj = &runner.state().objects[&copy];
    assert_eq!(copy_obj.zone, Zone::Exile);
    assert!(copy_obj.is_copy, "CR 707.12a: it is a copy, not a card");
    assert!(!copy_obj.is_token, "CR 704.5d is the wrong sweep for it");
    assert_eq!(
        runner.state().last_created_token_ids,
        vec![copy],
        "the copy is the chain's created referent, which is what the cast binds to"
    );
}

/// The whole point of the card: the copy is castable. Before the fix the cast
/// permission bound to `ParentTarget`, and the ability declares no targets, so
/// accepting "you may cast the copy" reached nothing.
#[test]
fn accepting_the_cast_puts_the_copy_on_the_stack() {
    // Grizzly Bears costs {1}{G}; "You still pay its costs".
    let (mut runner, garth) = garth_with_mana(3);
    activate_to_the_prompt(&mut runner, garth);
    runner
        .act(GameAction::ChooseOption {
            choice: "Grizzly Bears".to_string(),
        })
        .expect("a printed name is a legal answer");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "\"you may cast the copy\" is asked: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the cast is legal");

    // CR 608.2g: the permission is exercised while the ability is still
    // resolving, so the engine opens the paid-cast offer over the copy.
    let hit = match &runner.state().waiting_for {
        WaitingFor::CastOffer {
            kind: CastOfferKind::GraveyardPaidCast { hit_card, .. },
            ..
        } => *hit_card,
        other => panic!("expected a during-resolution cast offer, got {other:?}"),
    };
    assert_eq!(
        runner.state().objects[&hit].name,
        "Grizzly Bears",
        "the offer is over the copy — before the LastCreated bind it was over nothing"
    );

    runner
        .act(GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        })
        .expect("taking the offered cast is legal");
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pay {1}{G} from the pool");
    }

    // CR 707.12: the copy left exile for the stack, cast for its own cost.
    assert_ne!(
        runner.state().objects[&hit].zone,
        Zone::Exile,
        "the copy was cast: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.stack_names().contains(&"Grizzly Bears".to_string()),
        "the copy is on the stack: {:?}",
        runner.stack_names()
    );
}

#[test]
fn a_copy_nobody_casts_ceases_to_exist() {
    let (mut runner, garth) = garth_on_the_battlefield();
    activate_to_the_prompt(&mut runner, garth);
    runner
        .act(GameAction::ChooseOption {
            choice: "Grizzly Bears".to_string(),
        })
        .expect("a printed name is a legal answer");

    // Reach-guard for the negative assertion below: the copy must EXIST first,
    // or "it is gone" would pass on a card that never created one — which is
    // precisely how the unfixed Garth behaved.
    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|o| o.name == "Grizzly Bears"),
        "the copy was created before anyone declined to cast it"
    );

    // Decline the cast, then let the game reach a point where state-based actions
    // are checked (CR 704.3).
    let _ = runner.act(GameAction::DecideOptionalEffect { accept: false });
    runner.advance_until_stack_empty();

    assert!(
        !runner
            .state()
            .objects
            .values()
            .any(|o| o.name == "Grizzly Bears"),
        "CR 704.5e: a copy of a card outside the stack and the battlefield ceases \
         to exist — it must not sit in exile for the rest of the game"
    );
}

#[test]
fn a_name_already_chosen_leaves_the_domain() {
    let (mut runner, garth) = garth_on_the_battlefield();
    activate_to_the_prompt(&mut runner, garth);
    runner
        .act(GameAction::ChooseOption {
            choice: "Disenchant".to_string(),
        })
        .expect("a printed name is a legal answer");
    let _ = runner.act(GameAction::DecideOptionalEffect { accept: false });
    runner.advance_until_stack_empty();

    // Untap for a second activation.
    runner.state_mut().objects.get_mut(&garth).unwrap().tapped = false;
    activate_to_the_prompt(&mut runner, garth);

    let offered = offered_names(&runner);
    assert!(
        !offered.contains(&"Disenchant".to_string()),
        "CR 609.3: \"a card name that hasn't been chosen\" — got {offered:?}"
    );
    assert_eq!(
        offered.len(),
        2,
        "the other two are still legal: {offered:?}"
    );
}
