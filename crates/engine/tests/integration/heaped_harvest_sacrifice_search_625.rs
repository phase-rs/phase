//! Heaped Harvest (DSK) — issue #625: its "when you sacrifice it" half must
//! search for a basic land on every sacrifice route, including a spell's
//! additional cost.
//!
//! > When this artifact enters and when you sacrifice it, you may search your
//! > library for a basic land card, put it onto the battlefield tapped, then
//! > shuffle.
//! > {2}, {T}, Sacrifice this artifact: You gain 3 life.
//!
//! CR 603.10a: abilities that trigger when a player sacrifices a permanent
//! look back in time, so the sacrificed permanent's own trigger fires even
//! though it is in the graveyard by then. CR 601.2h: a spell's additional
//! sacrifice cost is paid while casting, a route the Carrot Cake matrix
//! (`carrot_cake_self_sacrifice_trigger`) does not exercise. CR 603.5: the
//! "you may" lets the controller decline the search.
//!
//! Field report: sacrificing it to Eviscerator's Insight neither prompted nor
//! searched; only the ETB half worked.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

// Verbatim Oracle text (Scryfall, 2026-10-27).
const HEAPED_HARVEST: &str = "When this artifact enters and when you sacrifice it, you may search \
your library for a basic land card, put it onto the battlefield tapped, then shuffle.\n\
{2}, {T}, Sacrifice this artifact: You gain 3 life.";
const EVISCERATORS_INSIGHT: &str = "As an additional cost to cast this spell, sacrifice an \
artifact or creature.\nDraw two cards.\nFlashback {4}{B} (You may cast this card from your \
graveyard for its flashback cost and any additional costs. Then exile it.)";

enum Route {
    /// Heaped Harvest's own "{2}, {T}, Sacrifice this artifact" cost.
    OwnCost,
    /// Eviscerator's Insight's additional cost (CR 601.2h), the reported route.
    SpellAdditionalCost,
}

struct Outcome {
    harvest_zone: Option<Zone>,
    forest_zone: Zone,
    forest_tapped: bool,
    life_delta: i32,
    hand_delta: i32,
    search_offers: usize,
    prompts: Vec<String>,
}

/// Sacrifices Heaped Harvest through `route`, answers its optional trigger
/// with `accept`, and drives every prompt until the stack is empty.
fn run(route: Route, accept: bool) -> Outcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_land_to_library_top(P0, "Forest").id();
    // Cards above the Forest for Eviscerator's Insight to draw, so the land
    // only leaves the library through the search.
    scenario.with_library_top(P0, &["Top One", "Top Two", "Top Three"]);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    // Pre-existing permanent: its ETB half is not in play here.
    let harvest = scenario
        .add_artifact_from_oracle(P0, "Heaped Harvest", HEAPED_HARVEST)
        .id();
    let insight = match route {
        Route::OwnCost => None,
        Route::SpellAdditionalCost => Some(
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Eviscerator's Insight",
                    true,
                    EVISCERATORS_INSIGHT,
                )
                .id(),
        ),
    };

    let mut runner: GameRunner = scenario.build();
    {
        // No scenario builder stages a basic land in the library; direct
        // mutation mirrors `add_basic_land` (CR 205.4: Basic + Forest subtype).
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&forest)
            .expect("the Forest exists");
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.base_card_types = obj.card_types.clone();
    }
    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner.state().players[P0.0 as usize].hand.len() as i32;

    match insight {
        None => {
            let ability_index = runner.state().objects[&harvest]
                .abilities
                .iter()
                .position(|a| a.cost.is_some())
                .expect("reach-guard: Heaped Harvest's sacrifice ability must be parsed");
            runner
                .act(GameAction::ActivateAbility {
                    source_id: harvest,
                    ability_index,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("reach-guard: the sacrifice ability must be activatable");
        }
        Some(insight) => {
            let card_id = runner.state().objects[&insight].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: insight,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("reach-guard: Eviscerator's Insight must be castable");
        }
    }

    let mut prompts = Vec::new();
    let mut search_offers = 0;
    let mut settled = false;
    for _ in 0..32 {
        let waiting = runner.state().waiting_for.clone();
        prompts.push(format!("{waiting:?}").chars().take(60).collect::<String>());
        match waiting {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                ..
            } => {
                assert!(
                    choices.contains(&harvest),
                    "reach-guard: Heaped Harvest must be offered; got {choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![harvest],
                    })
                    .expect("sacrificing Heaped Harvest must succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                search_offers += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answering the optional search must succeed");
            }
            WaitingFor::SearchChoice { cards, .. } => {
                assert!(
                    cards.contains(&forest),
                    "CR 701.23a: the basic Forest must be findable; got {cards:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![forest],
                    })
                    .expect("choosing the Forest must succeed");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                settled = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            other => panic!("unexpected prompt: {other:?}; trail so far: {prompts:?}"),
        }
    }
    // A stalled resolution (e.g. a prompt that keeps re-opening) must fail
    // here rather than leave a half-resolved state for the assertions below.
    assert!(
        settled,
        "resolution never settled within the prompt budget; trail: {prompts:?}"
    );
    let state = runner.state();
    assert!(
        state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must finish with an empty stack at priority; got {:?}",
        state.waiting_for
    );
    let player = &state.players[P0.0 as usize];
    Outcome {
        harvest_zone: state.objects.get(&harvest).map(|obj| obj.zone),
        forest_zone: state.objects[&forest].zone,
        forest_tapped: state.objects[&forest].tapped,
        life_delta: player.life - life_before,
        hand_delta: player.hand.len() as i32 - hand_before,
        search_offers,
        prompts,
    }
}

fn assert_searched(outcome: &Outcome, case: &str) {
    assert_eq!(
        outcome.harvest_zone,
        Some(Zone::Graveyard),
        "reach-guard: {case} — Heaped Harvest must have been sacrificed"
    );
    assert_eq!(
        outcome.search_offers, 1,
        "CR 603.10a + CR 603.5: {case} — the sacrifice trigger must offer its optional search \
         exactly once; prompts: {:?}",
        outcome.prompts
    );
    assert_eq!(
        outcome.forest_zone,
        Zone::Battlefield,
        "{case} — the basic land must be put onto the battlefield; prompts: {:?}",
        outcome.prompts
    );
    assert!(
        outcome.forest_tapped,
        "{case} — the basic land enters tapped"
    );
}

#[test]
fn own_cost_sacrifice_searches_for_a_basic_land() {
    let outcome = run(Route::OwnCost, true);
    assert_searched(&outcome, "its own cost");
    assert_eq!(
        outcome.life_delta, 3,
        "the {{2}}, {{T}} ability still gains 3 life"
    );
}

/// The reported route: Eviscerator's Insight's additional sacrifice cost.
#[test]
fn spell_additional_cost_sacrifice_searches_for_a_basic_land() {
    let outcome = run(Route::SpellAdditionalCost, true);
    assert_searched(&outcome, "a spell's additional cost (CR 601.2h)");
    assert_eq!(
        outcome.hand_delta, 1,
        "reach-guard: Eviscerator's Insight resolves (cast from hand, drew two)"
    );
}

/// CR 603.5: declining the "you may" leaves the land in the library.
#[test]
fn declining_the_search_leaves_the_land_in_the_library() {
    let outcome = run(Route::SpellAdditionalCost, false);
    assert_eq!(
        outcome.harvest_zone,
        Some(Zone::Graveyard),
        "reach-guard: Heaped Harvest must have been sacrificed"
    );
    assert_eq!(
        outcome.search_offers, 1,
        "reach-guard: the trigger must reach its optional choice exactly once; prompts: {:?}",
        outcome.prompts
    );
    assert_eq!(
        outcome.forest_zone,
        Zone::Library,
        "declining the search must not move the land"
    );
    assert_eq!(
        outcome.hand_delta, 1,
        "reach-guard: Eviscerator's Insight still resolves after the decline (drew two)"
    );
}
