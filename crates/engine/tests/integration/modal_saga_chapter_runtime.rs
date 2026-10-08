//! Modal Saga chapters through the production pipeline.
//!
//! A chapter whose body is "Choose one —" followed by bullets is a modal
//! triggered ability (CR 700.2, CR 714.2b): exactly one mode resolves. These
//! tests drive the real Saga pipeline — the lore counter added after the draw
//! step (CR 714.3c) fires the chapter-I `CounterAdded` trigger, the mode is
//! chosen as it is put on the stack (CR 700.2b), and the chapter resolves —
//! rather than inspecting parsed definitions.
//!
//! Before chapter bodies were recognized as modal, the bullets were parsed as
//! one sequential chain: no mode was chosen, Life of Toshiro Umezawa's chapter
//! never applied its chosen mode alone, and Summon: Magus Sisters gained 3 life
//! on every chapter with no shield counter and no +1/+1 counters.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TOSHIRO: &str = "(As this Saga enters and after your draw step, add a lore counter.)\nI, II \u{2014} Choose one \u{2014}\n\u{2022} Target creature gets +2/+2 until end of turn.\n\u{2022} Target creature gets -1/-1 until end of turn.\n\u{2022} You gain 2 life.\nIII \u{2014} Exile this Saga, then return it to the battlefield transformed under your control.";

const MAGUS_SISTERS: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II, III \u{2014} Choose one at random \u{2014}\n\u{2022} Combine Powers! \u{2014} Put three +1/+1 counters on target creature.\n\u{2022} Defense! \u{2014} Put a shield counter on target creature. You gain 3 life.\n\u{2022} Fight! \u{2014} This creature fights up to one target creature an opponent controls.\nHaste";

fn lore_count(runner: &GameRunner, saga: ObjectId) -> u32 {
    runner.state().objects[&saga]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0)
}

/// Advance from P0's upkeep, through the draw step, into P0's precombat main,
/// where CR 714.3c adds a lore counter and chapter I triggers. Parking in P0's
/// own upkeep keeps the opponent's turn (and its combat) out of the way. Then
/// choose `mode` if the game asks (a chosen-mode chapter), take the first legal
/// target for every target prompt, and resolve the stack.
fn fire_chapter_one(runner: &mut GameRunner, saga: ObjectId, mode: Option<usize>) {
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);

    assert_eq!(
        lore_count(runner, saga),
        1,
        "CR 714.3c must add the first lore counter, firing chapter I; saga: zone={:?} \
         types={:?} counters={:?} phase={:?} turn={} active={:?}",
        runner.state().objects[&saga].zone,
        runner.state().objects[&saga].card_types,
        runner.state().objects[&saga].counters,
        runner.state().phase,
        runner.state().turn_number,
        runner.state().active_player,
    );

    let mut chose_mode = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
                let index = mode.expect("a random-mode chapter must not ask for a mode");
                runner
                    .act(GameAction::SelectModes {
                        indices: vec![index],
                    })
                    .expect("select the mode");
                chose_mode = true;
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the first legal target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving chapter I: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "chapter I must resolve, waiting_for={:?}",
        runner.state().waiting_for
    );
    // Positive reach guard: a chosen-mode chapter really went through the
    // mode-choice window, so the result below is the chosen mode's.
    assert_eq!(
        chose_mode,
        mode.is_some(),
        "the mode-choice window must be offered exactly when the chapter lets the \
         controller choose"
    );
}

/// CR 700.2 + CR 700.2b: choosing the +2/+2 mode applies +2/+2 and nothing else.
/// Under the old sequential chain no mode was chosen and the creature did not
/// end up 4/4 (measured: it stayed 2/2).
#[test]
fn chosen_mode_of_a_modal_chapter_resolves_alone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "Life of Toshiro Umezawa", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(TOSHIRO)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    fire_chapter_one(&mut runner, saga, Some(0));

    let obj = &runner.state().objects[&bears];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(4), Some(4)),
        "only the chosen +2/+2 mode may apply; the -1/-1 mode must not"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "the unchosen life-gain mode must not apply"
    );
}

/// Which bullet of Summon: Magus Sisters' chapter resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MagusMode {
    CombinePowers,
    Defense,
    Fight,
}

/// Fire Summon: Magus Sisters' chapter I in a game seeded with `seed` and
/// return the mode that resolved.
///
/// CR 700.2 + CR 700.2b: a "choose one at random" chapter resolves exactly one
/// mode. The old sequential chain gained 3 life with no shield counter and no
/// +1/+1 counters, a result no single mode produces, so the two assertions
/// below hold for every mode and fail only for that chain.
fn magus_chapter_one_mode(seed: u64) -> MagusMode {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "Summon: Magus Sisters", 5, 5)
        .as_enchantment()
        .as_creature()
        .with_subtypes(vec!["Saga", "Faerie"])
        .from_oracle_text(MAGUS_SISTERS)
        .id();
    let ours = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    fire_chapter_one(&mut runner, saga, None);

    let state = runner.state();
    let counters_on = |id: ObjectId, counter: CounterType| {
        state
            .objects
            .get(&id)
            .and_then(|obj| obj.counters.get(&counter).copied())
            .unwrap_or(0)
    };
    let combine_powers = [saga, ours]
        .iter()
        .any(|&id| counters_on(id, CounterType::Plus1Plus1) == 3);
    let defense = [saga, ours]
        .iter()
        .any(|&id| counters_on(id, CounterType::Shield) == 1);
    let fight = state.objects[&theirs].zone != Zone::Battlefield
        || state.objects[&theirs].damage_marked > 0;
    let gained_life = state.players[0].life == life_before + 3;

    let applied = [combine_powers, defense, fight]
        .iter()
        .filter(|&&m| m)
        .count();
    assert_eq!(
        applied, 1,
        "seed {seed}: exactly one mode must resolve: combine_powers={combine_powers} \
         defense={defense} fight={fight}"
    );
    assert_eq!(
        gained_life, defense,
        "seed {seed}: 3 life belongs to the Defense! mode only: gained_life={gained_life} \
         defense={defense}"
    );
    if combine_powers {
        MagusMode::CombinePowers
    } else if defense {
        MagusMode::Defense
    } else {
        MagusMode::Fight
    }
}

/// CR 700.2 + CR 700.2b: a "choose one at random" chapter resolves exactly one
/// mode, whichever the game picks. Each seed below was measured to pick a
/// different mode, so all three bullets are exercised; the helper's two
/// assertions (exactly one mode applied, life only with Defense!) hold on every
/// run. Seed 42 is the default `GameScenario::new()` seed the single-run version
/// of this test used; pinning it too means a change in how the game draws the
/// random mode shows up here rather than silently shifting coverage.
#[test]
fn random_mode_of_a_modal_chapter_resolves_exactly_one_mode() {
    for (seed, expected) in [
        (0, MagusMode::CombinePowers),
        (1, MagusMode::Defense),
        (2, MagusMode::Fight),
        (42, MagusMode::CombinePowers),
    ] {
        assert_eq!(
            magus_chapter_one_mode(seed),
            expected,
            "seed {seed} must pick {expected:?}"
        );
    }
}

const OPTIONAL_CHAPTER: &str = "(As this Saga enters and after your draw step, add a lore counter.)\nI \u{2014} You may choose one \u{2014}\n\u{2022} Target creature gets +2/+2 until end of turn.\n\u{2022} You gain 2 life.\nII \u{2014} You gain 1 life.";

/// Fire the optional modal chapter I and answer its optional-trigger prompt
/// with `accept`. Returns whether a mode prompt followed, Grizzly Bears'
/// power/toughness and P0's life change.
fn fire_optional_chapter(accept: bool) -> (bool, (Option<i32>, Option<i32>), i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "Optional Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(OPTIONAL_CHAPTER)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(lore_count(&runner, saga), 1, "chapter I must trigger");

    let mut offered_optional = false;
    let mut mode_prompted = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                offered_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the optional chapter");
            }
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
                mode_prompted = true;
                runner
                    .act(GameAction::SelectModes { indices: vec![0] })
                    .expect("select the +2/+2 mode");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the first legal target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving chapter I: {other:?}"),
        }
    }
    assert!(offered_optional, "the optional chapter must ask first");
    assert!(runner.state().stack.is_empty(), "chapter I must finish");
    let obj = &runner.state().objects[&bears];
    (
        mode_prompted,
        (obj.power, obj.toughness),
        runner.state().players[0].life - life_before,
    )
}

/// CR 603.3c + CR 700.2b: declining a "You may choose one" chapter chooses no
/// mode: no mode prompt follows and no mode's effect applies.
#[test]
fn declining_an_optional_modal_chapter_applies_no_mode() {
    let (mode_prompted, bears, life_change) = fire_optional_chapter(false);
    assert!(!mode_prompted, "no mode prompt after declining");
    assert_eq!(bears, (Some(2), Some(2)), "the +2/+2 mode must not apply");
    assert_eq!(life_change, 0, "the life mode must not apply");
}

/// Control: accepting it leads to the mode prompt, and the chosen +2/+2 applies.
#[test]
fn accepting_an_optional_modal_chapter_asks_for_a_mode() {
    let (mode_prompted, bears, life_change) = fire_optional_chapter(true);
    assert!(mode_prompted, "accepting leads to the mode prompt");
    assert_eq!(bears, (Some(4), Some(4)), "the chosen +2/+2 applies");
    assert_eq!(life_change, 0);
}

const FLIGHT_SAGA: &str = "(As this Saga enters and after your draw step, add a lore counter.)\nI \u{2014} Choose one \u{2014}\n\u{2022} Draw a card. Creatures you control gain flying.\n\u{2022} Draw a card. Creatures you control gain flying until end of turn.\nII \u{2014} You gain 1 life.";

/// A Saga whose modal chapter I grants P0's creatures flying, either with no
/// stated duration (`mode` 0) or until end of turn (`mode` 1), fired and
/// resolved. Returns the Saga and P0's creature.
fn fire_flight_saga(mode: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "Flight Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(FLIGHT_SAGA)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    fire_chapter_one(&mut runner, saga, Some(mode));
    assert!(
        has_flying(&runner, bears),
        "reach: the chosen mode granted flying"
    );
    (runner, saga, bears)
}

fn has_flying(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .keywords
        .iter()
        .any(|keyword| matches!(keyword, engine::types::keywords::Keyword::Flying))
}

/// Run P0's cleanup step (CR 514), where "until end of turn" effects end
/// (CR 514.2).
fn pass_cleanup(runner: &mut GameRunner) {
    let mut events = Vec::new();
    runner.state_mut().phase = Phase::Cleanup;
    let _ = engine::game::turns::execute_cleanup(runner.state_mut(), &mut events);
    engine::game::layers::flush_layers(runner.state_mut());
}

/// CR 611.2a: "If no duration is stated, it lasts until the end of the game."
/// A modal chapter's independent grant to other objects ("Creatures you control
/// gain flying.") survives cleanup and survives the Saga leaving the
/// battlefield: it isn't the Saga's grant, so its lifetime isn't the Saga's.
#[test]
fn a_duration_free_chapter_grant_to_creatures_outlives_the_turn_and_the_saga() {
    let (mut runner, saga, bears) = fire_flight_saga(0);
    pass_cleanup(&mut runner);
    assert!(has_flying(&runner, bears), "the grant survives cleanup");

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), saga, Zone::Graveyard, &mut events);
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&saga].zone, Zone::Graveyard);
    assert!(
        has_flying(&runner, bears),
        "the grant survives the Saga leaving the battlefield"
    );
}

/// Control: the same grant stated "until end of turn" ends at cleanup.
#[test]
fn an_end_of_turn_chapter_grant_to_creatures_ends_at_cleanup() {
    let (mut runner, _saga, bears) = fire_flight_saga(1);
    pass_cleanup(&mut runner);
    assert!(
        !has_flying(&runner, bears),
        "the stated duration ends at cleanup"
    );
}

const TOKEN_FLIGHT_SAGA: &str = "(As this Saga enters and after your draw step, add a lore counter.)\nI \u{2014} Create a 1/1 white Soldier creature token, then it gains flying.\nII \u{2014} You gain 1 life.";

/// CR 611.2a: a grant continuing a token creation ("?, then it gains flying.")
/// states no duration, and nothing it continues does, so the token keeps
/// flying past cleanup.
#[test]
fn a_continuation_grant_to_a_created_token_outlives_the_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "Token Flight Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(TOKEN_FLIGHT_SAGA)
        .id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    let execute = runner.state().objects[&saga]
        .trigger_definitions
        .as_slice()
        .iter()
        .find_map(|trigger| {
            (trigger.definition.saga_chapter == Some(1)).then(|| trigger.definition.execute.clone())
        })
        .flatten()
        .expect("chapter I");
    let grant = execute
        .sub_ability
        .as_deref()
        .expect("reach: the grant clause");
    assert_eq!(
        grant.sub_link,
        engine::types::ability::SubAbilityLink::ContinuationStep,
        "reach: the grant continues the token creation"
    );
    assert!(
        format!("{:?}", grant.effect).contains("LastCreated"),
        "reach: the grant affects the created token, got {:?}",
        grant.effect
    );
    let before: Vec<ObjectId> = runner.state().battlefield.iter().copied().collect();
    fire_chapter_one(&mut runner, saga, None);
    let token = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| !before.contains(id))
        .expect("the token was created");
    assert!(has_flying(&runner, token), "reach: the token gained flying");
    pass_cleanup(&mut runner);
    assert!(has_flying(&runner, token), "the grant survives cleanup");
}

const BEARS_OF_LITTJARA: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI \u{2014} Create a 2/2 blue Shapeshifter creature token with changeling.\nII \u{2014} Any number of target Shapeshifter creatures you control have base power and toughness 4/4.\nIII \u{2014} Choose up to one target creature or planeswalker. Each creature with power 4 or greater you control deals damage equal to its power to that permanent.";

fn base_pt(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = &runner.state().objects[&id];
    (obj.power, obj.toughness)
}

/// The Bears of Littjara at one lore counter beside two 2/2 Shapeshifters;
/// chapter II fires and resolves targeting only the first.
fn bears_chapter_two() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "The Bears of Littjara", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(BEARS_OF_LITTJARA)
        .id();
    let chosen = scenario
        .add_creature(P0, "Chosen Shifter", 2, 2)
        .with_subtypes(vec!["Shapeshifter"])
        .id();
    let other = scenario
        .add_creature(P0, "Other Shifter", 2, 2)
        .with_subtypes(vec!["Shapeshifter"])
        .id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&saga)
        .unwrap()
        .counters
        .insert(CounterType::Lore, 1);
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(lore_count(&runner, saga), 2, "reach: chapter II fires");
    let mut targeted = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                let target =
                    (!targeted).then_some(engine::types::ability::TargetRef::Object(chosen));
                targeted = true;
                runner
                    .act(GameAction::ChooseTarget { target })
                    .expect("choose one Shapeshifter, then finish");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving chapter II: {other:?}"),
        }
    }
    assert!(targeted, "reach: chapter II asked for its targets");
    assert!(runner.state().stack.is_empty(), "chapter II resolved");
    (runner, saga, chosen, other)
}

/// CR 611.2a: The Bears of Littjara II states no duration, so the chosen
/// Shapeshifter is base 4/4 until the end of the game: through cleanup and
/// after the Saga leaves. The unchosen one is unaffected. The effect ends with
/// its object: the chosen Shapeshifter leaving the battlefield comes back a new
/// object (CR 400.7) with its printed 2/2.
#[test]
fn the_bears_of_littjara_chapter_two_lasts_until_the_end_of_the_game() {
    let (mut runner, saga, chosen, other) = bears_chapter_two();
    assert_eq!(
        base_pt(&runner, chosen),
        (Some(4), Some(4)),
        "reach: chapter II applied"
    );
    assert_eq!(
        base_pt(&runner, other),
        (Some(2), Some(2)),
        "the unchosen one is unaffected"
    );

    pass_cleanup(&mut runner);
    assert_eq!(
        base_pt(&runner, chosen),
        (Some(4), Some(4)),
        "survives cleanup"
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), saga, Zone::Graveyard, &mut events);
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&saga].zone, Zone::Graveyard);
    assert_eq!(
        base_pt(&runner, chosen),
        (Some(4), Some(4)),
        "survives the Saga leaving the battlefield"
    );

    engine::game::zones::move_to_zone(runner.state_mut(), chosen, Zone::Hand, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), chosen, Zone::Battlefield, &mut events);
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Battlefield);
    assert_eq!(
        base_pt(&runner, chosen),
        (Some(2), Some(2)),
        "the effect ended when its object left the battlefield"
    );
}
