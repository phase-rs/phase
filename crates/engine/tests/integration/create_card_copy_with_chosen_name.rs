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
//! CR 201.4 — the domain is CLOSED. The six names are printed on the card, and
//! a prompt that accepts a seventh is not that card's ability.
//!
//! CR 608.2d — "that hasn't been chosen" makes each committed name an illegal
//! option for the next activation.
//!
//! CR 707.13 + CR 704.5e — the copy is a copy, not a conjured card: its
//! characteristics come from the Oracle card reference (the face registry), it
//! may be cast during the same resolution (CR 608.2g; no state-based action runs
//! before a player would receive priority, CR 704.3), and if it is not cast the
//! next SBA check removes it rather than leaving a stray object behind.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::ability::{
    CastFromZoneDriver, ChoiceType, Effect, NameDistinctness, TargetFilter,
};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::card::CardFace;
use engine::types::counter::CounterType;
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

    // CR 201.4: the closed domain, in PRINTED order and printed case. Lowercase
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
    // CR 608.2d: "that hasn't been chosen".
    assert_eq!(*distinctness, NameDistinctness::DistinctFromSourceHistory);
    // The chosen name has to survive the choice for the next clause to read it.
    assert!(*persist, "a card-name choice persists on the source");

    // CR 707.13: the copy, created from the name rather than from an object.
    let create = ability
        .sub_ability
        .as_deref()
        .expect("the copy clause follows the choice");
    let Effect::CreateCardCopyByName { name, .. } = &*create.effect else {
        panic!(
            "expected CreateCardCopyByName, got {:?} — the recorded gap was \
             `unparsed_verb_arguments` on \"Create a copy of the card with the chosen name\"",
            create.effect
        );
    };
    assert_eq!(*name, None, "the name comes from the choice, not the text");

    // CR 601.2f: "you may cast the copy" — a normal, PAID cast ("You still pay
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
    // CR 608.2d: Ersta prints no "that hasn't been chosen" cue, so repeats are legal.
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
    let (runner, garth, _) = garth_scenario(green, |_| None);
    (runner, garth)
}

/// Garth plus whatever `extra` adds to the same scenario (its id, if any).
fn garth_scenario(
    green: usize,
    extra: impl FnOnce(&mut GameScenario) -> Option<ObjectId>,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
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
    let extra_id = extra(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().card_face_registry = fixture_registry();
    (runner, garth, extra_id)
}

/// Activate the {T} ability and let it resolve up to the name prompt.
fn activate_to_the_prompt(runner: &mut GameRunner, garth: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: garth,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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

    // CR 707.13: a real Lightning Bolt object, materialized from the registry and
    // marked as a copy so CR 704.5e can sweep it.
    let copy = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == "Lightning Bolt")
        .map(|(id, _)| *id)
        .expect("the copy was created");
    let copy_obj = &runner.state().objects[&copy];
    assert!(
        copy_obj.is_copy,
        "CR 704.5e: it is a copy of a card, which the copy sweep reads"
    );
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

    // CR 707.12 + CR 601.2a: the copy moved to the stack, cast for its own cost.
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Stack,
        "the copy was cast: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .stack_names()
            .iter()
            .any(|i: &String| i == "Grizzly Bears"),
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
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional cast is legal");
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
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional cast is legal");
    runner.advance_until_stack_empty();

    // Untap for a second activation.
    runner.state_mut().objects.get_mut(&garth).unwrap().tapped = false;
    activate_to_the_prompt(&mut runner, garth);

    let offered = offered_names(&runner);
    assert!(
        !offered.iter().any(|i| i == "Disenchant"),
        "CR 608.2d: \"a card name that hasn't been chosen\" — got {offered:?}"
    );
    assert_eq!(
        offered.len(),
        2,
        "the other two are still legal: {offered:?}"
    );
}

// ---------------------------------------------------------------------------
// CR 707.13 + CR 704.5e: the exile staging of the copy is not observable.
//
// The rule creates the copy outside the game; the engine stages it in exile for
// the rest of the resolution (`STAGING_ZONE`). That is only sound if nothing can
// tell the difference: the copy must be CREATED there (no zone change, so no
// "put into exile" trigger), and an uncast copy must be gone at the first
// state-based check (CR 704.5e). The War Doctor is the observer (Oracle text
// from Scryfall): "Whenever one or more other permanents phase out and whenever
// one or more other cards are put into exile from anywhere, put a time counter
// on The War Doctor."
// ---------------------------------------------------------------------------

const THE_WAR_DOCTOR: &str = "Whenever one or more other permanents phase out and whenever one or more other cards are put into exile from anywhere, put a time counter on The War Doctor.
Whenever The War Doctor attacks, it deals damage equal to the number of time counters on it to any target. If a creature dealt damage this way would die this turn, exile it instead.";

fn add_war_doctor(scenario: &mut GameScenario) -> ObjectId {
    let mut card = scenario.add_creature(P0, "The War Doctor", 3, 5);
    card.from_oracle_text(THE_WAR_DOCTOR);
    card.id()
}

fn time_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Time)
        .copied()
        .unwrap_or(0)
}

/// Reach-guard for the two negatives below: The War Doctor really does see a
/// card put into exile, so its silence there is evidence, not a dead trigger.
#[test]
fn the_war_doctor_sees_a_real_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doctor = add_war_doctor(&mut scenario);
    let exiler = {
        let mut card = scenario.add_creature(P0, "Exiler", 1, 1);
        card.from_oracle_text("{T}: Exile the top card of your library.");
        card.id()
    };
    scenario.add_card_to_library_top(P0, "Lightning Bolt");
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: exiler,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the exile ability is activatable");
    runner.advance_until_stack_empty();

    assert_eq!(
        time_counters(&runner, doctor),
        1,
        "a card put into exile triggers The War Doctor"
    );
}

/// CR 707.13 + CR 704.5e: declining the cast leaves no trace — no "put into
/// exile" trigger, and the copy has ceased to exist by the time a player gets
/// priority, so nothing that counts cards in exile can see it.
#[test]
fn a_declined_copy_is_never_put_into_exile_and_is_gone_at_once() {
    let (mut runner, garth, doctor) = garth_scenario(0, |s| Some(add_war_doctor(s)));
    let doctor = doctor.expect("The War Doctor is on the battlefield");
    let exile_before = runner.state().exile.len();
    activate_to_the_prompt(&mut runner, garth);
    runner
        .act(GameAction::ChooseOption {
            choice: "Grizzly Bears".to_string(),
        })
        .expect("a printed name is a legal answer");
    // Reach-guard: the copy exists before the decision.
    let copy = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("the copy was created");
    assert!(runner.state().objects.contains_key(&copy));

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional cast is legal");

    // Immediately: the resolution ended, the first state-based check ran.
    assert!(
        !runner.state().objects.contains_key(&copy),
        "CR 704.5e: the uncast copy ceased to exist"
    );
    assert_eq!(
        runner.state().exile.len(),
        exile_before,
        "nothing that counts cards in exile can see the copy"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        time_counters(&runner, doctor),
        0,
        "creating the copy is not putting a card into exile"
    );
}

/// The cast branch: the copy goes from its staging straight to the stack, and
/// still no "put into exile" trigger fires.
#[test]
fn a_cast_copy_is_never_put_into_exile() {
    let (mut runner, garth, doctor) = garth_scenario(3, |s| Some(add_war_doctor(s)));
    let doctor = doctor.expect("The War Doctor is on the battlefield");
    activate_to_the_prompt(&mut runner, garth);
    runner
        .act(GameAction::ChooseOption {
            choice: "Grizzly Bears".to_string(),
        })
        .expect("a printed name is a legal answer");
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the cast is legal");
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
    // Reach-guard: the copy really was cast.
    assert!(
        runner
            .stack_names()
            .iter()
            .any(|i: &String| i == "Grizzly Bears"),
        "the copy is on the stack: {:?}",
        runner.stack_names()
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        time_counters(&runner, doctor),
        0,
        "creating and casting the copy puts no card into exile"
    );
}

// ---------------------------------------------------------------------------
// CR 201.4 + CR 608.2c: the copy names what THIS resolution chose.
// ---------------------------------------------------------------------------

/// Choose `name` from the open prompt and decline the cast.
fn choose_and_decline(runner: &mut GameRunner, garth: ObjectId, name: &str) {
    activate_to_the_prompt(runner, garth);
    runner
        .act(GameAction::ChooseOption {
            choice: name.to_string(),
        })
        .expect("a printed name is a legal answer");
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional cast is legal");
    runner.advance_until_stack_empty();
    runner.state_mut().objects.get_mut(&garth).unwrap().tapped = false;
}

/// CR 609.3: once every name has been chosen the choice is impossible and does
/// nothing — so "the card with the chosen name" names NO card. The last name
/// chosen in an EARLIER resolution must not be copied again.
#[test]
fn an_exhausted_domain_creates_no_copy_and_offers_no_cast() {
    let (mut runner, garth) = garth_on_the_battlefield();
    for name in FIXTURE_NAMES {
        choose_and_decline(&mut runner, garth, name);
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: garth,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the tap ability is still activatable");
    runner.resolve_top();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { .. } | WaitingFor::OptionalEffectChoice { .. }
        ),
        "no name is left to choose and no copy to cast: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().last_created_token_ids.is_empty(),
        "no copy was created"
    );
    assert!(
        !runner
            .state()
            .objects
            .values()
            .any(|o| o.is_copy && FIXTURE_NAMES.contains(&o.name.as_str())),
        "the previous resolution's name was not copied again"
    );
}

/// CR 113.7a: an ability whose source has left the battlefield still resolves.
/// Garth destroyed in response still lets its controller choose a name and copy
/// that card — the chosen name lives in the resolution, not on the gone source.
#[test]
fn garth_removed_in_response_still_copies_the_chosen_card() {
    let (mut runner, garth, removal) = garth_scenario(0, |scenario| {
        let mut card = scenario.add_spell_to_hand(P0, "Unmake", true);
        card.from_oracle_text("Destroy target creature.");
        Some(card.id())
    });
    let removal = removal.expect("the removal spell is in hand");

    runner
        .act(GameAction::ActivateAbility {
            source_id: garth,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the tap ability is activatable");
    let card_id = runner.state().objects[&removal].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: removal,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the removal can be cast in response");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(garth)),
            })
            .expect("Garth is a legal target");
    }
    // The removal resolves first, then Garth's ability.
    runner.resolve_top();
    assert!(
        runner
            .state()
            .objects
            .get(&garth)
            .is_none_or(|o| o.zone != Zone::Battlefield),
        "Garth left the battlefield before its ability resolved"
    );
    runner.resolve_top();

    assert_eq!(
        offered_names(&runner),
        FIXTURE_NAMES.map(str::to_string).to_vec(),
        "the ability still asks for a name"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "Lightning Bolt".to_string(),
        })
        .expect("a printed name is a legal answer");
    let copy = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("the copy was created from the resolution's choice");
    assert_eq!(runner.state().objects[&copy].name, "Lightning Bolt");
}

// ---------------------------------------------------------------------------
// Real Garth One-Eye and Ersta, Friend to All faces through the production
// loader (Oracle text from Scryfall), with the face registry seeded at game
// start by `rehydrate_game_from_card_db` rather than by hand.
// ---------------------------------------------------------------------------

const REAL_GARTH: &str = "{T}: Choose a card name that hasn't been chosen from among \
Disenchant, Braingeyser, Terror, Shivan Dragon, Regrowth, and Black Lotus. Create a copy of the \
card with the chosen name. You may cast the copy. (You still pay its costs.)";

/// Ersta's Oracle text with its JSON escapes already applied (newlines as `\n`,
/// quotes as `\"`), since it is spliced into an MTGJSON fixture.
const REAL_ERSTA_JSON: &str = "+1: Create a 1/1 Human Wizard creature token that's all colors.\\n\
\u{2212}3: Choose a card name from among Enlightened Tutor, Mystical Tutor, Booster Tutor, \
Imperial Recruiter, and Worldly Tutor. Create a copy of the card with the chosen name. You may \
cast the copy without paying its mana cost.\\n\
\u{2212}8: You get an emblem with \\\"At the beginning of your upkeep, if you control twenty or \
more Wizards, you win the game.\\\"";

fn atomic(name: &str, types: &str, mana_cost: &str, text: &str, extra: &str) -> String {
    format!(
        r#""{name}": [{{ "name": "{name}", "colors": [], "colorIdentity": [], "layout": "normal",
            "types": [{types}], "subtypes": [], "supertypes": [], "manaCost": "{mana_cost}",
            "manaValue": 1.0, "text": "{text}", {extra}
            "identifiers": {{ "scryfallOracleId": "o-{name}" }} }}]"#
    )
}

fn real_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let entries = [
            atomic(
                "Garth One-Eye",
                r#""Creature""#,
                "{W}{U}{B}{R}{G}",
                REAL_GARTH,
                r#""power": "5", "toughness": "5","#,
            ),
            atomic(
                "Ersta, Friend to All",
                r#""Planeswalker""#,
                "{W}{U}{B}{R}{G}",
                REAL_ERSTA_JSON,
                r#""loyalty": "5","#,
            ),
            atomic(
                "Disenchant",
                r#""Instant""#,
                "{1}{W}",
                "Destroy target artifact or enchantment.",
                "",
            ),
            atomic(
                "Braingeyser",
                r#""Sorcery""#,
                "{X}{U}{U}",
                "Target player draws X cards.",
                "",
            ),
            atomic(
                "Terror",
                r#""Instant""#,
                "{1}{B}",
                "Destroy target nonartifact, nonblack creature. It can't be regenerated.",
                "",
            ),
            atomic(
                "Shivan Dragon",
                r#""Creature""#,
                "{4}{R}{R}",
                "Flying\\n{R}: This creature gets +1/+0 until end of turn.",
                r#""power": "5", "toughness": "5","#,
            ),
            atomic(
                "Regrowth",
                r#""Sorcery""#,
                "{1}{G}",
                "Return target card from your graveyard to your hand.",
                "",
            ),
            atomic(
                "Black Lotus",
                r#""Artifact""#,
                "{0}",
                "{T}, Sacrifice this artifact: Add three mana of any one color.",
                "",
            ),
            // The cast-origin readers of the F2/F3 tests: printed Oracle text,
            // verbatim from card-data.json.
            atomic(
                "Surge of Brilliance",
                r#""Instant""#,
                "{1}{U}",
                "Paradox \u{2014} Draw a card for each spell you've cast this turn from anywhere \
                 other than your hand.\\nForetell {1}{U} (During your turn, you may pay {2} and \
                 exile this card from your hand face down. Cast it on a later turn for its \
                 foretell cost.)",
                "",
            ),
            atomic(
                "The Twelfth Doctor",
                r#""Creature""#,
                "{3}{U}{R}",
                "The first spell you cast from anywhere other than your hand each turn has \
                 demonstrate. (When you cast that spell, you may copy it. If you do, choose an \
                 opponent to also copy it. A copy of a permanent spell becomes a token.)\\n\
                 Whenever you copy a spell, put a +1/+1 counter on The Twelfth Doctor.",
                r#""power": "4", "toughness": "4","#,
            ),
            atomic(
                "Nalfeshnee",
                r#""Creature""#,
                "{5}{R}",
                "Flying\\nWhenever you cast a spell from exile, copy it. You may choose new \
                 targets for the copy. If it's a permanent spell, the copy gains haste and \
                 \\\"At the beginning of the end step, sacrifice this permanent.\\\" (A copy of a \
                 permanent spell becomes a token.)",
                r#""power": "4", "toughness": "6","#,
            ),
            atomic(
                "Wild-Magic Sorcerer",
                r#""Creature""#,
                "{3}{R}",
                "The first spell you cast from exile each turn has cascade. (When you cast your \
                 first spell from exile, exile cards from the top of your library until you \
                 exile a nonland card that costs less. You may cast it without paying its mana \
                 cost. Put the exiled cards on the bottom in a random order.)",
                r#""power": "4", "toughness": "3","#,
            ),
            atomic(
                "Interdimensional Web Watch",
                r#""Artifact""#,
                "{4}",
                "When this artifact enters, exile the top two cards of your library. Until the \
                 end of your next turn, you may play those cards.\\n{T}: Add two mana in any \
                 combination of colors. Spend this mana only to cast spells from exile.",
                "",
            ),
            atomic(
                "Enlightened Tutor",
                r#""Instant""#,
                "{W}",
                "Search your library for an artifact or enchantment card, reveal it, then shuffle \
                 and put that card on top.",
                "",
            ),
            atomic(
                "Mystical Tutor",
                r#""Instant""#,
                "{U}",
                "Search your library for an instant or sorcery card, reveal it, then shuffle and \
                 put that card on top.",
                "",
            ),
            atomic(
                "Booster Tutor",
                r#""Instant""#,
                "{B}",
                "Open a sealed Magic booster pack, reveal the cards, and put one of them into \
                 your hand. (Remove that card from your deck before beginning a new game.)",
                "",
            ),
            atomic(
                "Imperial Recruiter",
                r#""Creature""#,
                "{2}{R}",
                "When this creature enters, search your library for a creature card with power 2 \
                 or less, reveal it, put it into your hand, then shuffle.",
                r#""power": "1", "toughness": "1","#,
            ),
            atomic(
                "Worldly Tutor",
                r#""Instant""#,
                "{G}",
                "Search your library for a creature card, reveal it, then shuffle and put the \
                 card on top.",
                "",
            ),
        ];
        let json = format!(r#"{{ "data": {{ {} }} }}"#, entries.join(","));
        let dir = std::env::temp_dir().join(format!("garth-real-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("AtomicCards.json");
        std::fs::write(&path, json).expect("write AtomicCards fixture");
        CardDatabase::from_mtgjson(&path).expect("real Garth/Ersta fixture loads")
    })
}

/// Build a game around real faces, seeding the outside-the-game registry the
/// way a real game does.
fn real_game(
    mana: Vec<ManaType>,
    setup: impl FnOnce(&mut GameScenario, &CardDatabase) -> Vec<ObjectId>,
) -> (GameRunner, Vec<ObjectId>) {
    let db = real_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if !mana.is_empty() {
        scenario.with_mana_pool(
            P0,
            mana.into_iter()
                .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
                .collect(),
        );
    }
    let ids = setup(&mut scenario, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner, ids)
}

/// Answer every prompt of an accepted copy cast: accept, take the offer,
/// announce X, pick the target, pay from the pool.
fn drive_copy_cast(runner: &mut GameRunner, x: u32, target: Option<TargetRef>) {
    for _ in 0..24 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::CastOffer { .. } => GameAction::GraveyardPaidCastChoice {
                choice: CastChoice::Cast,
            },
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: x },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: target.clone(),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            _ => return,
        };
        runner.act(action).expect("the copy cast proceeds");
    }
}

fn garth_choose(runner: &mut GameRunner, garth: ObjectId, name: &str) {
    activate_to_the_prompt(runner, garth);
    runner
        .act(GameAction::ChooseOption {
            choice: name.to_string(),
        })
        .expect("a printed name is a legal answer");
}

#[test]
fn real_garth_registry_is_seeded_at_game_start() {
    let (runner, _) = real_game(Vec::new(), |scenario, db| {
        vec![scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db)]
    });
    for name in [
        "disenchant",
        "braingeyser",
        "terror",
        "shivan dragon",
        "regrowth",
        "black lotus",
    ] {
        assert!(
            runner.state().card_face_registry.contains_key(name),
            "{name} is preloaded from Garth's closed domain"
        );
    }
}

/// A targeted copy (Disenchant, {1}{W}) cast for its cost destroys its target.
#[test]
fn real_garth_casts_a_targeted_copy() {
    let (mut runner, ids) = real_game(vec![ManaType::White, ManaType::White], |scenario, db| {
        vec![
            scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db),
            scenario.add_real_card(P1, "Black Lotus", Zone::Battlefield, db),
        ]
    });
    let (garth, lotus) = (ids[0], ids[1]);
    garth_choose(&mut runner, garth, "Disenchant");
    drive_copy_cast(&mut runner, 0, Some(TargetRef::Object(lotus)));
    assert!(
        runner
            .stack_names()
            .iter()
            .any(|i: &String| i == "Disenchant"),
        "the Disenchant copy is on the stack: {:?} / {:?}",
        runner.stack_names(),
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert!(
        runner
            .state()
            .objects
            .get(&lotus)
            .is_none_or(|o| o.zone != Zone::Battlefield),
        "Disenchant's copy destroyed the targeted artifact"
    );
}

/// An X copy (Braingeyser, {X}{U}{U}) announces X and pays for it.
#[test]
fn real_garth_casts_an_x_copy() {
    let (mut runner, ids) = real_game(
        vec![
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
        ],
        |scenario, db| {
            for _ in 0..3 {
                scenario.add_real_card(P0, "Regrowth", Zone::Library, db);
            }
            vec![scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db)]
        },
    );
    let garth = ids[0];
    let hand_before = runner.state().players[0].hand.len();
    garth_choose(&mut runner, garth, "Braingeyser");
    drive_copy_cast(&mut runner, 2, Some(TargetRef::Player(P0)));
    assert!(
        runner
            .stack_names()
            .iter()
            .any(|i: &String| i == "Braingeyser"),
        "the Braingeyser copy is on the stack: {:?} / {:?}",
        runner.stack_names(),
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 2,
        "X = 2 drew two cards"
    );
}

/// A {0} permanent copy (Black Lotus) is cast for nothing and resolves onto the
/// battlefield.
#[test]
fn real_garth_casts_a_zero_cost_permanent_copy() {
    let (mut runner, ids) = real_game(Vec::new(), |scenario, db| {
        vec![scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db)]
    });
    let garth = ids[0];
    garth_choose(&mut runner, garth, "Black Lotus");
    drive_copy_cast(&mut runner, 0, None);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().battlefield.iter().any(|id| {
            let o = &runner.state().objects[id];
            o.name == "Black Lotus" && o.controller == P0
        }),
        "the cast Black Lotus copy resolved onto the battlefield: {:?}",
        runner.state().waiting_for
    );
}

/// Ersta, Friend to All shares the effect with a FREE cast: "You may cast the
/// copy without paying its mana cost." With no mana available the copy is still
/// cast.
#[test]
fn real_ersta_casts_her_copy_for_free() {
    let (mut runner, ids) = real_game(Vec::new(), |scenario, db| {
        vec![scenario.add_real_card(P0, "Ersta, Friend to All", Zone::Battlefield, db)]
    });
    let ersta = ids[0];
    runner
        .act(GameAction::ActivateAbility {
            source_id: ersta,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Ersta's -3 is activatable");
    runner.resolve_top();
    runner
        .act(GameAction::ChooseOption {
            choice: "Enlightened Tutor".to_string(),
        })
        .expect("a printed name is a legal answer");
    drive_copy_cast(&mut runner, 0, None);
    assert!(
        runner
            .stack_names()
            .iter()
            .any(|i: &String| i == "Enlightened Tutor"),
        "the free copy is on the stack: {:?} / {:?}",
        runner.stack_names(),
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// CR 601.2a + CR 707.13 + CR 400.11: the copy is cast from OUTSIDE the game, so
// no "cast from [zone]" reader may see the staging zone as its origin.
// ---------------------------------------------------------------------------

const EXILE_CAST_OBSERVER: &str = "Whenever you cast a spell from exile, you gain 1 life.";
const NOT_HAND_CAST_OBSERVER: &str =
    "Whenever you cast a spell from anywhere other than your hand, you gain 10 life.";

fn life_of_p0(runner: &GameRunner) -> i32 {
    runner.state().players[0].life
}

/// Real Garth casts its Black Lotus copy with a cast-origin observer for each
/// polarity on the battlefield: "from exile" (Nalfeshnee's class) must stay
/// silent, "from anywhere other than your hand" must fire — an outside-the-game
/// cast is from no zone (CR 400.11), so it satisfies the negative constraint and
/// fails every named zone. `clear_provenance` strips the outside-the-game marker
/// first, as the reach guard: with the marker gone the SAME cast is recorded
/// from the staging zone and the exile observer fires, proving the observer is
/// live and that the marker alone is what silences it.
fn cast_black_lotus_copy_with_observers(clear_provenance: bool) -> (i32, i32) {
    let (mut runner, ids) = real_game(Vec::new(), |scenario, db| {
        let garth = scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db);
        {
            let mut card = scenario.add_creature(P0, "Exile Observer", 1, 1);
            card.from_oracle_text(EXILE_CAST_OBSERVER);
        }
        {
            let mut card = scenario.add_creature(P0, "Not Hand Observer", 1, 1);
            card.from_oracle_text(NOT_HAND_CAST_OBSERVER);
        }
        vec![garth]
    });
    let garth = ids[0];
    let life_before = life_of_p0(&runner);
    garth_choose(&mut runner, garth, "Black Lotus");
    if clear_provenance {
        let copy = runner
            .state()
            .last_created_token_ids
            .first()
            .copied()
            .expect("the copy was created");
        let object = runner
            .state_mut()
            .objects
            .get_mut(&copy)
            .expect("the copy exists");
        assert!(
            object.created_outside_game,
            "the copy is marked as created outside the game"
        );
        object.created_outside_game = false;
    }
    drive_copy_cast(&mut runner, 0, None);
    // Both observers trigger on the SpellCast event; resolve them and the copy.
    runner.advance_until_stack_empty();
    assert!(
        runner.state().battlefield.iter().any(|id| {
            let o = &runner.state().objects[id];
            o.name == "Black Lotus" && o.controller == P0
        }),
        "the Black Lotus copy was cast and resolved: {:?}",
        runner.state().waiting_for
    );
    let gained = life_of_p0(&runner) - life_before;
    // 1 from the exile observer, 10 from the not-hand observer.
    (gained % 10, gained / 10)
}

#[test]
fn a_cast_of_garths_copy_is_not_a_cast_from_exile() {
    let (exile_triggers, not_hand_triggers) = cast_black_lotus_copy_with_observers(false);
    assert_eq!(
        exile_triggers, 0,
        "CR 707.13 + CR 400.11: the copy is cast from outside the game, not from exile"
    );
    assert_eq!(
        not_hand_triggers, 1,
        "CR 601.2a: a cast from outside the game is a cast from anywhere other than the hand"
    );
}

/// Reach guard for the test above: without the outside-the-game marker the very
/// same cast IS observed as a cast from exile.
#[test]
fn the_exile_cast_observer_is_live_when_the_provenance_is_a_real_exile_cast() {
    let (exile_triggers, not_hand_triggers) = cast_black_lotus_copy_with_observers(true);
    assert_eq!(exile_triggers, 1, "a real exile cast fires the observer");
    assert_eq!(not_hand_triggers, 1);
}

// ---------------------------------------------------------------------------
// CR 113.7a + CR 400.7 + CR 608.2h: "that hasn't been chosen" reads the history
// of the source incarnation the ability was activated for.
// ---------------------------------------------------------------------------

/// Garth chose Lightning Bolt, then is activated again and blinked before the
/// ability resolves. The returned Garth is a NEW object with no history
/// (CR 400.7), but the pending ability belongs to the old incarnation, whose
/// last known information still has Lightning Bolt as chosen (CR 113.7a): the
/// returned Garth's empty history must not re-offer it.
#[test]
fn a_blinked_garth_ability_reads_the_old_incarnations_chosen_names() {
    let (mut runner, garth, flicker) = garth_scenario(0, |scenario| {
        let mut card = scenario.add_spell_to_hand(P0, "Flicker", true);
        card.from_oracle_text(
            "Exile target creature you control, then return it to the battlefield under its \
             owner's control.",
        );
        Some(card.id())
    });
    let flicker = flicker.expect("the blink spell is in hand");

    // First activation commits Lightning Bolt to this incarnation's history.
    choose_and_decline(&mut runner, garth, "Lightning Bolt");
    let incarnation_before = runner.state().objects[&garth].incarnation;

    // Second activation, blinked in response.
    runner
        .act(GameAction::ActivateAbility {
            source_id: garth,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the tap ability is activatable again");
    let card_id = runner.state().objects[&flicker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flicker,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the blink can be cast in response");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(garth)),
            })
            .expect("Garth is a legal target");
    }
    runner.resolve_top();
    let returned = &runner.state().objects[&garth];
    assert_eq!(returned.zone, Zone::Battlefield, "Garth came back");
    assert_ne!(
        returned.incarnation, incarnation_before,
        "the returned Garth is a new object (CR 400.7)"
    );
    assert!(
        returned.chosen_attributes.is_empty(),
        "the returned Garth carries no choice history"
    );

    runner.resolve_top();
    assert_eq!(
        offered_names(&runner),
        vec!["Disenchant".to_string(), "Grizzly Bears".to_string()],
        "the old incarnation's committed name stays illegal"
    );
}

/// A cast clause that is NOT "the copy" keeps its own target and window: the
/// rewrite that binds "you may cast the copy" to this chain's created copy and
/// to cast-during-resolution applies only to the anaphor, never to an
/// independent later permission (CR 608.2c: each instruction follows its own
/// text).
#[test]
fn an_independent_later_cast_clause_keeps_its_own_target_and_window() {
    let parsed = parse_oracle_text(
        "{T}: Choose a card name that hasn't been chosen from among Disenchant, Braingeyser, \
         and Terror. Create a copy of the card with the chosen name. You may cast the copy. \
         You may cast target instant card from your graveyard this turn.",
        "Test Card",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let mut node = parsed.abilities.first();
    let mut casts = Vec::new();
    while let Some(def) = node {
        if let Effect::CastFromZone { target, driver, .. } = &*def.effect {
            casts.push((target.clone(), *driver));
        }
        node = def.sub_ability.as_deref();
    }
    assert_eq!(
        casts.first(),
        Some(&(
            TargetFilter::LastCreated,
            CastFromZoneDriver::DuringResolution
        )),
        "the copy cast is bound to the created copy: {casts:?}"
    );
    for (target, driver) in casts.iter().skip(1) {
        assert_ne!(
            *target,
            TargetFilter::LastCreated,
            "an independent cast clause is not rebound to the created copy"
        );
        assert_eq!(
            *driver,
            CastFromZoneDriver::LingeringPermission,
            "an independent cast clause keeps its window"
        );
    }
    assert!(
        casts.len() >= 2,
        "the independent clause parsed as a second cast: {casts:?}"
    );
}

// ---------------------------------------------------------------------------
// CR 601.2a + CR 707.13 + CR 400.11: Garth's copy is cast from OUTSIDE the game.
// Outside the game is not a zone, so the cast satisfies every "from anywhere other
// than your hand" reader and fails every "from exile" reader — in the history
// counts, the static keyword grants, the cast triggers, and the mana that may pay.
// Every test drives the printed cards through the production cast pipeline, with
// a real cast from exile as the positive control.
// ---------------------------------------------------------------------------

use engine::game::derived_views::derive_filtered_views;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{CardPlayMode, CastingPermission, Duration, PlayFromExileProvenance};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::CastFrequency;
use engine::types::zones::EtbTapState;

/// Real Garth casts the copy of its closed domain's Black Lotus (cost {0}); the
/// copy is left on the stack and returned.
fn garth_casts_black_lotus_copy(runner: &mut GameRunner, garth: ObjectId) -> ObjectId {
    garth_choose(runner, garth, "Black Lotus");
    let copy = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("the copy was created");
    drive_copy_cast(runner, 0, None);
    assert_eq!(
        runner.state().objects[&copy].zone,
        Zone::Stack,
        "reach guard: the copy was cast: {:?}",
        runner.state().waiting_for
    );
    copy
}

/// A plain "cast this card from exile" permission for P0 (the impulse-draw shape).
fn grant_exile_cast(runner: &mut GameRunner, card: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&card)
        .expect("the exiled card exists")
        .casting_permissions
        .push(CastingPermission::PlayFromExile {
            provenance: PlayFromExileProvenance::Impulse,
            duration: Duration::UntilEndOfTurn,
            granted_to: P0,
            mode: CardPlayMode::Cast,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
            land_enter_tapped: EtbTapState::Unspecified,
        });
}

/// Cast a card from exile for its own cost ({0} here) through the production cast
/// action, then settle the cast triggers' ordering prompt (CR 603.3b) so every
/// trigger the cast caused is on the stack.
fn cast_from_exile(runner: &mut GameRunner, card: ObjectId) {
    grant_exile_cast(runner, card);
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: Vec::new(),
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the exiled card is castable under its permission");
    settle_cast_triggers(runner);
}

fn settle_cast_triggers(runner: &mut GameRunner) {
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    }
}

fn spell_has_keyword(
    runner: &GameRunner,
    spell: ObjectId,
    is_keyword: fn(&Keyword) -> bool,
) -> bool {
    runner.state().objects[&spell]
        .cast_spell_keywords
        .iter()
        .any(is_keyword)
}

/// Surge of Brilliance after a first spell cast the given way, in a two-player
/// game with three cards to draw. Returns how many cards Surge drew.
fn surge_draw_after_first_spell(first_cast: FirstCast) -> i64 {
    let (mut runner, ids) = real_game(vec![ManaType::Blue, ManaType::Blue], |scenario, db| {
        let garth = scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db);
        let surge = scenario.add_real_card(P0, "Surge of Brilliance", Zone::Hand, db);
        let exiled_lotus = scenario.add_real_card(P0, "Black Lotus", Zone::Exile, db);
        for _ in 0..3 {
            scenario.add_spell_to_library_top(P0, "Library Filler", true);
        }
        vec![garth, surge, exiled_lotus]
    });
    let (garth, surge, exiled_lotus) = (ids[0], ids[1], ids[2]);
    match first_cast {
        FirstCast::None => {}
        FirstCast::HandCast => unreachable!("Surge is measured against exile and Garth casts"),
        FirstCast::GarthsCopy => {
            garth_casts_black_lotus_copy(&mut runner, garth);
        }
        FirstCast::RealExileCast => cast_from_exile(&mut runner, exiled_lotus),
    }
    runner.advance_until_stack_empty();
    runner.cast(surge).commit().resolve().hand_drawn(P0)
}

#[derive(Clone, Copy)]
enum FirstCast {
    None,
    /// A real cast from hand: the negative control for every cast-origin reader.
    HandCast,
    GarthsCopy,
    RealExileCast,
}

/// F2 (CR 601.2a + CR 707.13 + CR 400.11): Surge of Brilliance — "Draw a card for
/// each spell you've cast this turn from anywhere other than your hand." Garth's
/// Black Lotus copy is cast from outside the game, which is not a zone, so it is
/// one such spell: Surge, cast from hand, draws exactly one card. The positive
/// control (a real exile cast) draws one; with no earlier spell Surge draws none.
#[test]
fn surge_of_brilliance_counts_garths_outside_game_cast() {
    assert_eq!(
        surge_draw_after_first_spell(FirstCast::None),
        0,
        "control: Surge's own hand cast counts for nothing"
    );
    assert_eq!(
        surge_draw_after_first_spell(FirstCast::RealExileCast),
        1,
        "control: a real cast from exile is a cast from anywhere other than the hand"
    );
    assert_eq!(
        surge_draw_after_first_spell(FirstCast::GarthsCopy),
        1,
        "CR 400.11: a cast from outside the game is not from the hand, so Surge draws one"
    );
}

/// What the first spell of the turn gets from the cast-origin statics, however it
/// was cast.
struct FirstCastReaders {
    demonstrate: bool,
    cascade: bool,
    nalfeshnee_triggered: bool,
}

/// The Twelfth Doctor ("The first spell you cast from anywhere other than your hand
/// each turn has demonstrate"), Wild-Magic Sorcerer ("The first spell you cast from
/// exile each turn has cascade") and Nalfeshnee ("Whenever you cast a spell from
/// exile, copy it") on the battlefield, then the first spell of the turn is cast.
fn first_cast_readers(first_cast: FirstCast) -> FirstCastReaders {
    let (mut runner, ids) = real_game(Vec::new(), |scenario, db| {
        for name in ["The Twelfth Doctor", "Wild-Magic Sorcerer", "Nalfeshnee"] {
            scenario.add_real_card(P0, name, Zone::Battlefield, db);
        }
        vec![
            scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db),
            scenario.add_real_card(P0, "Black Lotus", Zone::Exile, db),
            scenario.add_real_card(P0, "Black Lotus", Zone::Hand, db),
        ]
    });
    let (garth, exiled_lotus, hand_lotus) = (ids[0], ids[1], ids[2]);
    let spell = match first_cast {
        FirstCast::HandCast => {
            let card_id = runner.state().objects[&hand_lotus].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: hand_lotus,
                    card_id,
                    targets: Vec::new(),
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                })
                .expect("the Black Lotus in hand is castable for {0}");
            hand_lotus
        }
        FirstCast::GarthsCopy => garth_casts_black_lotus_copy(&mut runner, garth),
        FirstCast::RealExileCast => {
            cast_from_exile(&mut runner, exiled_lotus);
            exiled_lotus
        }
        FirstCast::None => unreachable!("the readers are measured on a cast"),
    };
    settle_cast_triggers(&mut runner);
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "reach guard: the first spell of the turn is on the stack"
    );
    FirstCastReaders {
        demonstrate: spell_has_keyword(&runner, spell, |k| matches!(k, Keyword::Demonstrate)),
        cascade: spell_has_keyword(&runner, spell, |k| matches!(k, Keyword::Cascade)),
        nalfeshnee_triggered: runner
            .stack_names()
            .iter()
            .any(|i: &String| i == "Nalfeshnee"),
    }
}

/// F2 (CR 601.2a + CR 707.13 + CR 400.11): Garth's first qualifying cast receives
/// The Twelfth Doctor's demonstrate (a cast from anywhere other than the hand), but
/// neither Wild-Magic Sorcerer's exile-only cascade nor Nalfeshnee's exile trigger.
/// A real cast from exile is the positive control for all three readers.
#[test]
fn garths_cast_gets_the_not_hand_grant_but_no_exile_only_reader() {
    // Negative control: a real cast from the hand is read by none of the three.
    let hand = first_cast_readers(FirstCast::HandCast);
    assert!(
        !hand.demonstrate && !hand.cascade && !hand.nalfeshnee_triggered,
        "control: a cast from the hand is neither from exile nor from anywhere other than          the hand"
    );

    let real_exile = first_cast_readers(FirstCast::RealExileCast);
    assert!(
        real_exile.demonstrate,
        "control: an exile cast is not from the hand"
    );
    assert!(
        real_exile.cascade,
        "control: Wild-Magic Sorcerer grants an exile cast cascade"
    );
    assert!(
        real_exile.nalfeshnee_triggered,
        "control: Nalfeshnee triggers on a cast from exile"
    );

    let garth = first_cast_readers(FirstCast::GarthsCopy);
    assert!(
        garth.demonstrate,
        "CR 400.11: Garth's cast is from anywhere other than the hand — demonstrate applies"
    );
    assert!(
        !garth.cascade,
        "CR 707.13: Garth's cast is not a cast from exile — no cascade"
    );
    assert!(
        !garth.nalfeshnee_triggered,
        "CR 707.13: Garth's cast is not a cast from exile — Nalfeshnee stays silent"
    );
}

/// Why the Watch's mana can or cannot pay for a given cast.
struct WatchTable {
    runner: GameRunner,
    garth: ObjectId,
    watch: ObjectId,
    exiled_disenchant: ObjectId,
}

fn watch_table(unrestricted_white: usize) -> WatchTable {
    let (runner, ids) = real_game(vec![ManaType::White; unrestricted_white], |scenario, db| {
        vec![
            scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db),
            scenario.add_real_card(P0, "Interdimensional Web Watch", Zone::Battlefield, db),
            scenario.add_real_card(P0, "Disenchant", Zone::Exile, db),
        ]
    });
    let mut table = WatchTable {
        runner,
        garth: ids[0],
        watch: ids[1],
        exiled_disenchant: ids[2],
    };
    grant_exile_cast(&mut table.runner, table.exiled_disenchant);
    table
}

/// Tap the real Watch for its restricted white mana ("Spend this mana only to cast
/// spells from exile") through its printed activated mana ability.
fn tap_watch_for_white(table: &mut WatchTable) {
    table
        .runner
        .act(GameAction::ActivateAbility {
            source_id: table.watch,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the Watch's mana ability is activatable");
    assert!(
        matches!(
            table.runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "the Watch asks for its color combination: {:?}",
        table.runner.state().waiting_for
    );
    table
        .runner
        .act(GameAction::ChooseManaColor {
            choice: engine::types::game_state::ManaChoice::Combination(vec![
                ManaType::White,
                ManaType::White,
            ]),
            count: 1,
        })
        .expect("two white is a legal combination");
    let restricted = table.runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.color == ManaType::White && !unit.restrictions.is_empty())
        .count();
    assert_eq!(
        restricted, 2,
        "reach guard: the Watch's two restricted white mana are in the pool"
    );
}

fn restricted_units_in_pool(runner: &GameRunner) -> usize {
    runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| !unit.restrictions.is_empty())
        .count()
}

/// Drive a Garth copy cast like [`drive_copy_cast`], but report a refusal instead of
/// panicking — a cast the pool cannot pay for is refused.
fn try_drive_copy_cast(runner: &mut GameRunner, target: TargetRef) -> bool {
    for _ in 0..24 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::CastOffer { .. } => GameAction::GraveyardPaidCastChoice {
                choice: CastChoice::Cast,
            },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(target.clone()),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            _ => return true,
        };
        if runner.act(action).is_err() {
            return false;
        }
    }
    true
}

/// F3 (CR 106.6 + CR 601.2h + CR 707.13 + CR 400.11): Interdimensional Web Watch —
/// "Spend this mana only to cast spells from exile." Its mana pays for a real cast
/// from exile, but cannot pay for Garth's Disenchant copy, which is cast from
/// outside the game even though it is staged in exile. Unrestricted mana still
/// pays for the copy.
#[test]
fn web_watch_mana_pays_for_an_exile_cast_but_not_for_garths_copy() {
    // Control: a real cast of Disenchant from exile is paid with the Watch's mana.
    let mut table = watch_table(0);
    tap_watch_for_white(&mut table);
    let watch = table.watch;
    let disenchant = table.exiled_disenchant;
    table.runner.cast(disenchant).target_object(watch).commit();
    assert_eq!(
        table.runner.state().objects[&disenchant].zone,
        Zone::Stack,
        "control: exile-only mana pays for a spell cast from exile"
    );
    assert_eq!(
        restricted_units_in_pool(&table.runner),
        0,
        "control: the Watch's mana was spent on it"
    );

    // Garth's copy of Disenchant cannot be paid with the Watch's mana.
    let mut table = watch_table(0);
    tap_watch_for_white(&mut table);
    garth_choose(&mut table.runner, table.garth, "Disenchant");
    assert_eq!(
        table.runner.state().last_created_token_ids.len(),
        1,
        "reach guard: the copy was created"
    );
    let cast = try_drive_copy_cast(&mut table.runner, TargetRef::Object(table.watch));
    // CR 601.2h: the announced copy stays unpaid — the Watch's mana is refused.
    assert!(
        !cast
            && matches!(
                table.runner.state().waiting_for,
                WaitingFor::ManaPayment { .. }
            ),
        "CR 707.13: exile-only mana cannot pay for a cast from outside the game (cast          accepted: {cast}): {:?}",
        table.runner.state().waiting_for
    );
    assert_eq!(
        restricted_units_in_pool(&table.runner),
        2,
        "the Watch's mana was never spent on the copy"
    );

    // Unrestricted mana still pays for the copy.
    let mut table = watch_table(2);
    garth_choose(&mut table.runner, table.garth, "Disenchant");
    assert!(try_drive_copy_cast(
        &mut table.runner,
        TargetRef::Object(table.watch)
    ));
    assert!(
        table.runner.stack_names().iter().any(|i| i == "Disenchant"),
        "unrestricted mana pays for the copy: {:?}",
        table.runner.state().waiting_for
    );
}

/// The exile lists each viewer is shown: the projected zone list (what the client
/// counts) and the derived public pile.
fn exile_projection(
    runner: &GameRunner,
    viewer: PlayerId,
) -> (Vec<ObjectId>, Vec<(PlayerId, Vec<ObjectId>)>) {
    let filtered = filter_state_for_viewer(runner.state(), viewer);
    let derived = derive_filtered_views(runner.state(), &filtered, Some(viewer));
    (
        filtered.exile.iter().copied().collect(),
        derived.visible_exile_object_ids.into_iter().collect(),
    )
}

/// F4 (CR 707.13 + CR 400.11): while Garth's "You may cast the copy" prompt is
/// pending, the staged copy is outside the game, so neither viewer's exile list or
/// public pile shows it — and the prompt can still cast it.
#[test]
fn the_pending_garth_copy_is_not_in_either_viewers_exile() {
    let (mut runner, garth, _) = garth_scenario(3, |scenario| {
        scenario.add_creature_to_exile(P0, "Exiled Of P0", 1, 1);
        scenario.add_creature_to_exile(P1, "Exiled Of P1", 1, 1);
        None
    });
    let before = (exile_projection(&runner, P0), exile_projection(&runner, P1));
    assert_eq!(
        before.0 .0.len(),
        2,
        "reach guard: both exiled cards are listed"
    );
    assert!(
        before.0 .1.iter().any(|(owner, _)| *owner == P0)
            && before.1 .1.iter().any(|(owner, _)| *owner == P1),
        "reach guard: both public piles are populated: {before:?}"
    );

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
        "the optional cast prompt is pending: {:?}",
        runner.state().waiting_for
    );
    let copy = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("the copy was created");
    // Reach guard: the authoritative state really holds the staged copy in exile.
    assert!(runner.state().exile.contains(&copy));
    assert!(runner.state().objects[&copy].created_outside_game);

    let at_prompt = (exile_projection(&runner, P0), exile_projection(&runner, P1));
    assert_eq!(
        at_prompt, before,
        "CR 400.11: the outside-the-game copy is in neither viewer's exile"
    );

    // The public pile is also withheld when the projection is handed the authoritative
    // state itself (the staged copy is in its exile list there).
    for viewer in [P0, P1] {
        let authoritative = derive_filtered_views(runner.state(), runner.state(), Some(viewer));
        assert_eq!(
            authoritative
                .visible_exile_object_ids
                .into_iter()
                .collect::<Vec<_>>(),
            exile_projection(&runner, viewer).1,
            "the derived pile skips the outside-the-game copy by itself"
        );
    }

    // The prompt can still cast the copy.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the cast is legal");
    let hit = match &runner.state().waiting_for {
        WaitingFor::CastOffer {
            kind: CastOfferKind::GraveyardPaidCast { hit_card, .. },
            ..
        } => *hit_card,
        other => panic!("expected the cast offer over the copy, got {other:?}"),
    };
    assert_eq!(hit, copy, "the offer is over the staged copy");
    assert_eq!(
        (exile_projection(&runner, P0), exile_projection(&runner, P1)),
        before,
        "the exile lists are unchanged at the paid-cast offer too"
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
        runner.act(GameAction::PassPriority).expect("pay {1}{G}");
    }
    assert_eq!(runner.state().objects[&copy].zone, Zone::Stack);
}
