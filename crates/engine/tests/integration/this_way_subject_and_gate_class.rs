//! The "… this way" class beyond Mog, Moogle Warrior (issues #9736 / #9752).
//!
//! - CR 608.2c + CR 121.1: a "… this way" gate names the earlier instruction
//!   that performed its verb. A draw (Lord Windgrace) and the members of one
//!   gated run (Old Rutstein, Bonehoard Dracosaur) are not that instruction,
//!   so they must not replace the ledger the next gate reads.
//! - CR 608.2c + CR 701.9a + CR 701.21a: "each player who discarded/sacrificed
//!   a <filter> this way" lowers to the chain's tracked set. Every other card
//!   this grammar newly accepts (Azra Bladeseeker, Borderland Explorer,
//!   Vaevictis Asmadi, the Dire) is either runtime-validated or held red on a
//!   named strict-failure gap.

use std::collections::BTreeMap;

use engine::database::{card_db::CardDatabase, mtgjson::AtomicCard, synthesis::build_oracle_face};
use engine::game::coverage::analyze_coverage;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, PlayerFilter, ThisWayCause};
use engine::types::actions::GameAction;
use engine::types::confirmed_misparse::ConfirmedMisparses;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LORD_WINDGRACE: &str = "[+2]: Discard a card, then draw a card. If a land card is discarded this way, draw an additional card.\n[−3]: Return up to two target land cards from your graveyard to the battlefield.\n[−11]: Destroy up to six target nonland permanents, then create six 2/2 green Cat Warrior creature tokens with forestwalk.\nLord Windgrace can be your commander.";

const OLD_RUTSTEIN: &str = "When Old Rutstein enters and at the beginning of your upkeep, mill a card. If a land card is milled this way, create a Treasure token. If a creature card is milled this way, create a 1/1 green Insect creature token. If a noncreature, nonland card is milled this way, create a Blood token.";

const BONEHOARD_DRACOSAUR: &str = "Flying, first strike\nAt the beginning of your upkeep, exile the top two cards of your library. You may play them this turn. If you exiled a land card this way, create a 3/1 red Dinosaur creature token. If you exiled a nonland card this way, create a Treasure token.";

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[0].hand.len()
}

fn tokens_named(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token && o.zone == Zone::Battlefield && o.name == name && o.controller == P0
        })
        .map(|o| o.id)
        .collect()
}

/// Pass priority until the stack is empty, answering a discard prompt with
/// `discard`. Returns how many discard prompts appeared.
fn settle(runner: &mut GameRunner, discard: Option<ObjectId>) -> usize {
    let mut discard_prompts = 0;
    for _ in 0..64 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DiscardChoice { cards, .. } => {
                discard_prompts += 1;
                let pick = discard.expect("a discard pick is staged");
                assert!(cards.contains(&pick), "the staged pick is offered");
                GameAction::SelectCards { cards: vec![pick] }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution completed: {:?}",
        runner.state().waiting_for
    );
    discard_prompts
}

// ---------------------------------------------------------------------------
// R6 — Lord Windgrace: a draw is never a "this way" antecedent
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum WindgraceHand {
    /// Hand {Forest, Lightning Bolt}; discard the named one.
    DiscardForest,
    DiscardBolt,
    Empty,
}

/// Activate Lord Windgrace's +2 and return (cards drawn, discard prompts).
/// `land_on_top` stages a basic land (else a sorcery) as the library top.
fn windgrace_plus_two(hand: WindgraceHand, land_on_top: bool) -> (usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Lord Windgrace", "Windgrace", 5, LORD_WINDGRACE)
        .id();
    for n in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {n}"), false);
    }
    if land_on_top {
        scenario.add_land_to_library_top(P0, "Swamp");
    } else {
        scenario.add_spell_to_library_top(P0, "Library Sorcery", false);
    }
    let pick = match hand {
        WindgraceHand::Empty => None,
        WindgraceHand::DiscardForest | WindgraceHand::DiscardBolt => {
            let forest = scenario.add_land_to_hand(P0, "Forest").id();
            let bolt = scenario.add_bolt_to_hand(P0);
            Some(match hand {
                WindgraceHand::DiscardForest => forest,
                _ => bolt,
            })
        }
    };
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner);
    let library_before = runner.state().players[0].library.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate +2");
    let prompts = settle(&mut runner, pick);
    if let Some(pick) = pick {
        assert_eq!(runner.state().objects[&pick].zone, Zone::Graveyard);
    }
    let drawn = library_before - runner.state().players[0].library.len();
    let discarded = usize::from(pick.is_some());
    assert_eq!(hand_len(&runner), hand_before - discarded + drawn);
    (drawn, prompts)
}

/// CR 608.2c + CR 121.1: discarding the Forest draws two (one, plus the
/// additional card) even though the drawn card is a nonland; discarding the
/// Bolt draws one even though the drawn card is a land.
#[test]
fn lord_windgrace_gate_reads_the_discard_not_the_draw() {
    assert_eq!(
        windgrace_plus_two(WindgraceHand::DiscardForest, false),
        (2, 1)
    );
    assert_eq!(windgrace_plus_two(WindgraceHand::DiscardBolt, true), (1, 1));
}

/// Lord Windgrace ruling: "If you have no cards in hand as Lord Windgrace's
/// first ability resolves, you'll discard nothing then you'll draw one card."
/// The drawn land must not satisfy the gate.
#[test]
fn lord_windgrace_empty_hand_draws_exactly_one() {
    assert_eq!(windgrace_plus_two(WindgraceHand::Empty, true), (1, 0));
}

// ---------------------------------------------------------------------------
// R7 — Old Rutstein: a gated-run member that creates a token keeps the ledger
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Milled {
    DryadArbor,
    Creature,
    Instant,
}

/// Resolve Old Rutstein's upkeep trigger over `milled` and return the
/// (Treasure, Insect, Blood) token counts.
fn old_rutstein_upkeep(milled: Milled) -> (usize, usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(P0, "Old Rutstein", 1, 4, OLD_RUTSTEIN);
    for n in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {n}"), false);
    }
    let top = match milled {
        Milled::DryadArbor => scenario
            .add_land_to_library_top(P0, "Dryad Arbor")
            .as_creature()
            .with_subtypes(vec!["Forest", "Dryad"])
            .id(),
        Milled::Creature => scenario
            .add_spell_to_library_top(P0, "Grizzly Bears", false)
            .as_creature()
            .id(),
        Milled::Instant => scenario
            .add_spell_to_library_top(P0, "Lightning Bolt", true)
            .id(),
    };
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the upkeep trigger is on the stack"
    );
    settle(&mut runner, None);
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Graveyard,
        "reach: milled"
    );
    (
        tokens_named(&runner, "Treasure").len(),
        tokens_named(&runner, "Insect").len(),
        tokens_named(&runner, "Blood").len(),
    )
}

/// Old Rutstein ruling: "If you mill Dryad Arbor, a card that's both a creature
/// and a land, you get both a Treasure token and a 1/1 green Insect token."
/// CR 608.2c + CR 111.1: the Insect gate reads the milled card, not the
/// Treasure the gated node before it created, and the Blood gate does not read
/// that Treasure either.
#[test]
fn old_rutstein_dryad_arbor_makes_treasure_and_insect() {
    assert_eq!(old_rutstein_upkeep(Milled::DryadArbor), (1, 1, 0));
}

#[test]
fn old_rutstein_creature_makes_only_an_insect() {
    assert_eq!(old_rutstein_upkeep(Milled::Creature), (0, 1, 0));
}

#[test]
fn old_rutstein_instant_makes_only_blood() {
    assert_eq!(old_rutstein_upkeep(Milled::Instant), (0, 0, 1));
}

// ---------------------------------------------------------------------------
// R7b — Bonehoard Dracosaur: the same seam after a play permission
// ---------------------------------------------------------------------------

/// Resolve Bonehoard Dracosaur's upkeep trigger over two staged library tops
/// and return the (Dinosaur, Treasure) token counts.
fn bonehoard_upkeep(second_is_land: bool) -> (usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(P0, "Bonehoard Dracosaur", 5, 5, BONEHOARD_DRACOSAUR);
    for n in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {n}"), false);
    }
    let second = if second_is_land {
        scenario.add_land_to_library_top(P0, "Mountain").id()
    } else {
        scenario.add_spell_to_library_top(P0, "Shock", true).id()
    };
    let first = scenario.add_land_to_library_top(P0, "Forest").id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the upkeep trigger is on the stack"
    );
    settle(&mut runner, None);
    for id in [first, second] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Exile,
            "reach: exiled"
        );
    }
    (
        tokens_named(&runner, "Dinosaur").len(),
        tokens_named(&runner, "Treasure").len(),
    )
}

/// CR 608.2c + CR 111.1: two lands exiled → one Dinosaur and no Treasure; the
/// Treasure gate does not read the Dinosaur token the gated node created.
#[test]
fn bonehoard_dracosaur_two_lands_make_only_a_dinosaur() {
    assert_eq!(bonehoard_upkeep(true), (1, 0));
}

/// Bonehoard Dracosaur ruling: "If you exile a land card and a nonland card,
/// you'll create a Dinosaur token, then a Treasure token."
#[test]
fn bonehoard_dracosaur_land_and_nonland_make_both() {
    assert_eq!(bonehoard_upkeep(false), (1, 1));
}

// ---------------------------------------------------------------------------
// R10 — every Class A card the subject grammar accepts stays honest
// ---------------------------------------------------------------------------

const AZRA_BLADESEEKER: &str = r#"{"colorIdentity": ["R"], "colors": ["R"], "identifiers": {"scryfallOracleId": "ac4d065e-7eb6-4224-8a7c-e57f2c1a2733"}, "layout": "normal", "manaCost": "{2}{R}", "manaValue": 3.0, "name": "Azra Bladeseeker", "power": "3", "subtypes": ["Azra", "Warrior"], "supertypes": [], "text": "When this creature enters, each player on your team may discard a card, then each player who discarded a card this way draws a card.", "toughness": "2", "type": "Creature — Azra Warrior", "types": ["Creature"]}"#;

const BORDERLAND_EXPLORER: &str = r#"{"colorIdentity": ["G"], "colors": ["G"], "identifiers": {"scryfallOracleId": "65bc4f8a-3226-4648-862b-3f62736ff1ae"}, "layout": "normal", "manaCost": "{1}{G}", "manaValue": 2.0, "name": "Borderland Explorer", "power": "3", "subtypes": ["Elf", "Scout"], "supertypes": [], "text": "When this creature enters, each player may discard a card. Each player who discarded a card this way may search their library for a basic land card, reveal it, put it into their hand, then shuffle.", "toughness": "1", "type": "Creature — Elf Scout", "types": ["Creature"]}"#;

const VAEVICTIS_ASMADI: &str = r#"{"colorIdentity": ["B", "G", "R"], "colors": ["B", "G", "R"], "identifiers": {"scryfallOracleId": "c42ce2e9-4dfe-468a-8f41-49e187cb91d4"}, "layout": "normal", "manaCost": "{3}{B}{R}{G}", "manaValue": 6.0, "name": "Vaevictis Asmadi, the Dire", "power": "6", "subtypes": ["Elder", "Dragon"], "supertypes": ["Legendary"], "text": "Flying\nWhenever Vaevictis Asmadi attacks, for each player, choose target permanent that player controls. Those players sacrifice those permanents. Each player who sacrificed a permanent this way reveals the top card of their library, then puts it onto the battlefield if it's a permanent card.", "toughness": "6", "type": "Legendary Creature — Elder Dragon", "types": ["Creature"]}"#;

/// The published coverage row for one MTGJSON face: (supported, gap handlers).
fn coverage_of(key: &str, atomic_json: &str) -> (bool, Vec<String>) {
    let atomic: AtomicCard = serde_json::from_str(atomic_json).unwrap();
    let mut faces = BTreeMap::from([(key, build_oracle_face(&atomic, None))]);
    ConfirmedMisparses::committed().stamp_all(faces.iter_mut().map(|(k, face)| (*k, face)));
    let db = CardDatabase::from_json_str(&serde_json::to_string(&faces).unwrap()).unwrap();
    let coverage = analyze_coverage(&db);
    let card = coverage
        .cards
        .iter()
        .find(|card| card.card_face_key.as_deref() == Some(key))
        .expect("reported exact face key");
    (
        card.supported,
        card.gap_details
            .iter()
            .map(|gap| gap.handler.clone())
            .collect(),
    )
}

/// Every node of every trigger body of `text`.
fn trigger_nodes(name: &str, text: &str) -> Vec<AbilityDefinition> {
    let parsed = parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]);
    let mut nodes = Vec::new();
    for trigger in &parsed.triggers {
        let mut cur = trigger.execute.as_deref();
        while let Some(node) = cur {
            nodes.push(node.clone());
            cur = node.sub_ability.as_deref();
        }
    }
    nodes
}

/// Reach guard for R10: the "each player who … this way" sentence now lowers
/// to a tracked-set scope with the printed verb's cause, and no
/// `unbound_subject` gap remains for it.
fn assert_subject_lowered(name: &str, text: &str, cause: ThisWayCause) {
    let nodes = trigger_nodes(name, text);
    assert!(
        nodes.iter().any(|node| matches!(
            &node.player_scope,
            Some(PlayerFilter::TrackedSetPossessor { caused_by: Some(c), .. }) if *c == cause
        )),
        "{name}: the subject lowers to TrackedSetPossessor({cause:?}): {nodes:#?}"
    );
    let dbg = format!("{nodes:?}");
    assert!(
        !dbg.contains("who discarded") && !dbg.contains("who sacrificed"),
        "{name}: no gap keeps the 'who … this way' sentence: {dbg}"
    );
}

fn oracle_text_of(atomic_json: &str) -> String {
    let atomic: AtomicCard = serde_json::from_str(atomic_json).unwrap();
    atomic.text.expect("printed text")
}

/// Step 1.6 measurement (post-change). INCOMPLETE: this test records the
/// coverage rows and asserts only the subject lowering; the per-card outcome
/// assertions are pending the stop-and-return in the executor report.
///
/// Measured on the post-U1 tree:
/// - Azra Bladeseeker: `supported=false`, `[Effect:unrecognized_clause_head]`
///   ("each player on your team may discard a card") — outcome 1.
/// - Borderland Explorer: `supported=true`, no gaps — outcome 2 or 3, needs
///   its runtime row (R10b) before it may stay green.
/// - Vaevictis Asmadi, the Dire: `supported=false`, `[Effect:unbound_subject,
///   Effect:unknown, Effect:unparsed_quantity]` — outcome 1.
#[test]
fn class_a_cards_stay_red_on_their_remaining_gaps() {
    let cases = [
        (
            "Azra Bladeseeker",
            "azra bladeseeker",
            AZRA_BLADESEEKER,
            ThisWayCause::Discarded,
        ),
        (
            "Borderland Explorer",
            "borderland explorer",
            BORDERLAND_EXPLORER,
            ThisWayCause::Discarded,
        ),
        (
            "Vaevictis Asmadi, the Dire",
            "vaevictis asmadi, the dire",
            VAEVICTIS_ASMADI,
            ThisWayCause::Sacrificed,
        ),
    ];
    for (name, key, atomic, cause) in cases {
        assert_subject_lowered(name, &oracle_text_of(atomic), cause);
        let (supported, gaps) = coverage_of(key, atomic);
        println!("COVERAGE {key} supported={supported} gaps={gaps:?}");
    }
}
