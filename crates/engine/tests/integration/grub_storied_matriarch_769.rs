//! Grub, Storied Matriarch // Grub, Notorious Auntie (ECL) — issue #769.
//!
//! > Grub, Storied Matriarch: Menace. Whenever this creature enters or
//! > transforms into Grub, Storied Matriarch, return up to one target Goblin
//! > card from your graveyard to your hand. At the beginning of your first main
//! > phase, you may pay {R}. If you do, transform Grub.
//! >
//! > Grub, Notorious Auntie: Menace. Whenever Grub attacks, you may blight 1. If
//! > you do, create a tapped and attacking token that's a copy of the blighted
//! > creature, except it has "At the beginning of the end step, sacrifice this
//! > token." At the beginning of your first main phase, you may pay {B}. If you
//! > do, transform Grub.
//!
//! CR 701.68c: "the blighted creature" is the object chosen when blighting, so
//! the copy source comes from the blight in this resolution, not from a target.
//! CR 701.68b: a player unable to put the counters on a creature they control
//! can't choose to blight, so every optional blight is gated on that.
//! CR 508.4: the token is attacking without having been declared as an attacker.
//! CR 707.9a: the copy gains the "except" end-step sacrifice trigger.
//! CR 702.26b + CR 702.26d: a phased-out creature is treated as though it doesn't
//! exist, so it is neither offered for blight nor a reason blight is possible; a
//! phased-out "can't" source forbids nothing.
//! CR 118.12 + CR 118.3 + CR 608.2d: an optional pick from a zone with nothing
//! eligible is not offered, so an "If you do" rider stays closed and an "If you
//! don't" branch runs.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const MATRIARCH: &str = "Grub, Storied Matriarch";
const AUNTIE: &str = "Grub, Notorious Auntie";
const GOBLIN: &str = "Goblin Piker";
const SOLEMNITY: &str = "Solemnity";
const SLIP: &str = "Slip Out the Back";
const CONCEALMENT: &str = "Clever Concealment";

const AUNTIE_ORACLE: &str = "Menace\nWhenever Grub attacks, you may blight 1. If you do, create a tapped and attacking token that's a copy of the blighted creature, except it has \"At the beginning of the end step, sacrifice this token.\"\nAt the beginning of your first main phase, you may pay {B}. If you do, transform Grub.";
const NEXUS_ORACLE: &str = "At the beginning of combat on your turn, draw a card. Then you may exile an artifact or creature card from your hand. If you do, create a token that's a copy of the exiled card, except it's a 3/3 Golem artifact creature in addition to its other types.";
const HAKBAL_ORACLE: &str = "At the beginning of combat on your turn, each Merfolk creature you control explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on the exploring creature, then put the card back or put it into your graveyard.)\nWhenever Hakbal attacks, you may put a land card from your hand onto the battlefield. If you don't, draw a card.";
const GODS_GIFT_ORACLE: &str = "At the beginning of combat on your turn, you may exile a creature card from your graveyard. If you do, create a token that's a copy of that card, except it's a 4/4 black Zombie. It gains haste until end of turn.";
const AEGIS_ORACLE: &str = "Flying\nWard {2} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {2}.)\nAt the beginning of your upkeep, you may exile two cards from your graveyard. If you do, put a +1/+1 counter on this creature.";
const KEEPERS_ORACLE: &str = "This creature can't have counters put on it.";
const GANG_ORACLE: &str = "At the beginning of your first main phase, you may blight 2. If you don't, you lose 3 life. (To blight 2, put two -1/-1 counters on a creature you control.)";
const SCROUNGER_ORACLE: &str = "At the beginning of your first main phase, you may blight 1. If you do, create a Treasure token. (To blight 1, put a -1/-1 counter on a creature you control. A Treasure token is an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";
const TORCHMASTER_ORACLE: &str = "At the beginning of combat on your turn, you may blight 1. When you do, target creature gains haste until end of turn. (To blight 1, put a -1/-1 counter on a creature you control.)";

/// A board at `phase`, with stocked libraries so a real draw step can't end the
/// game (CR 704.5b).
fn board<T>(
    phase: Phase,
    stage: impl FnOnce(&mut GameScenario, &'static CardDatabase) -> T,
) -> (GameRunner, T) {
    let db = load_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.with_library_top(P0, &["Top One", "Top Two", "Top Three"]);
    scenario.with_library_top(P1, &["Top One", "Top Two", "Top Three"]);
    let staged = stage(&mut scenario, db);
    (scenario.build(), staged)
}

/// A board with the real card on its front face, able to attack this turn.
fn grub_board<T>(
    phase: Phase,
    stage: impl FnOnce(&mut GameScenario, &'static CardDatabase) -> T,
) -> (GameRunner, ObjectId, T) {
    let (mut runner, (grub, staged)) = board(phase, |scenario, db| {
        let grub = scenario.add_real_card(P0, MATRIARCH, Zone::Battlefield, db);
        (grub, stage(scenario, db))
    });
    runner
        .state_mut()
        .objects
        .get_mut(&grub)
        .expect("Grub exists")
        .summoning_sick = false;
    (runner, grub, staged)
}

/// Setup only: puts a staged Grub on her back face without paying for it.
fn flip(runner: &mut GameRunner, grub: ObjectId) {
    engine::game::transform::transform_permanent(runner.state_mut(), grub, &mut Vec::new())
        .expect("Grub must transform");
    assert_eq!(runner.state().objects[&grub].name, AUNTIE);
}

fn attack_with(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("the attack must be legal");
}

/// Both players pass, resolving the top of the stack.
fn both_pass(runner: &mut GameRunner) {
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
    }
}

/// Passes through empty priority windows into `phase`. `advance_to_phase` can't
/// do this mid-step: its leading `auto_advance` re-enters the current step.
fn pass_into(runner: &mut GameRunner, phase: Phase) {
    for _ in 0..16 {
        if runner.state().phase == phase {
            return;
        }
        assert!(runner.state().stack.is_empty(), "the stack must be empty");
        both_pass(runner);
    }
    panic!("never reached {phase:?}; at {:?}", runner.state().phase);
}

fn assert_optional_prompt(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the option must be offered; got {:?}",
        runner.state().waiting_for
    );
}

fn decide(runner: &mut GameRunner, accept: bool) {
    assert_optional_prompt(runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("answering the optional effect must succeed");
}

/// Accepts the option, asserts the exact pool of the CR 701.68a choice, and
/// chooses `chosen`.
fn blight(runner: &mut GameRunner, pool: &[ObjectId], chosen: ObjectId) {
    decide(runner, true);
    let WaitingFor::EffectZoneChoice { player, cards, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 701.68a: blighting asks for a creature; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    let mut offered = cards;
    offered.sort();
    let mut expected = pool.to_vec();
    expected.sort();
    assert_eq!(offered, expected, "the blight pool");
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("blighting the chosen creature must succeed");
}

fn select_goblin(runner: &mut GameRunner, goblin: ObjectId) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the return trigger must ask for its target; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(goblin)],
        })
        .expect("targeting the Goblin must succeed");
}

fn assert_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(state.stack.is_empty(), "stack: {:?}", state.stack);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        state.waiting_for
    );
}

fn tokens(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token)
        .collect()
}

fn minus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Minus1Minus1)
        .copied()
        .unwrap_or(0)
}

fn is_attacking(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == id))
}

/// Live parser to runtime: this board is built from the Oracle text, so it is
/// the row a parser revert fails. `add_real_card` would read the baked fixture.
#[test]
fn auntie_from_oracle_text_creates_the_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let grub = scenario
        .add_creature_from_oracle(P0, AUNTIE, 2, 1, AUNTIE_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);

    assert_settled(&runner);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    assert_eq!(runner.state().objects[&created[0]].name, "Bear");
    assert!(is_attacking(&runner, created[0]));
}

/// CR 701.68c + CR 707.2 + CR 508.4: with two other creatures offered, the
/// token follows the choice and copies neither the counter nor the wrong body.
#[test]
fn auntie_copies_the_blighted_creature_as_a_tapped_attacking_token() {
    let (mut runner, grub, (bear, elf)) = grub_board(Phase::BeginCombat, |scenario, _| {
        (
            scenario.add_creature(P0, "Bear", 2, 2).id(),
            scenario.add_creature(P0, "Elf", 3, 3).id(),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear, elf], elf);

    assert_settled(&runner);
    assert_eq!(
        minus_counters(&runner, elf),
        1,
        "CR 701.68a: blight 1 puts one -1/-1 counter on the chosen creature"
    );
    assert_eq!(minus_counters(&runner, bear), 0);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, "Elf", "the token copies the blighted creature");
    assert!(token.tapped, "the token enters tapped");
    assert!(
        token.counters.is_empty(),
        "CR 707.2: counters aren't copied"
    );
    assert_eq!(
        (token.power, token.toughness),
        (Some(3), Some(3)),
        "CR 707.2: the copy has the printed P/T"
    );
    assert!(
        is_attacking(&runner, created[0]),
        "CR 508.4: the token enters attacking"
    );
}

/// The reported half of #769: the token is gone at the end step, the original
/// isn't.
#[test]
fn auntie_token_is_sacrificed_at_the_end_step() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);
    assert_eq!(tokens(&runner).len(), 1, "reach-guard: the token exists");

    pass_into(&mut runner, Phase::End);
    runner.advance_until_stack_empty();

    assert_settled(&runner);
    assert!(
        tokens(&runner).is_empty(),
        "CR 707.9a: the copy's end-step trigger must sacrifice it"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "only the token is sacrificed"
    );
}

/// CR 701.68c + CR 704.3: state-based actions wait until the ability finishes,
/// so a creature the counter kills still provides the copy.
#[test]
fn auntie_copies_a_creature_the_blight_kills() {
    let (mut runner, grub, elf) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Elf", 1, 1).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, elf], elf);

    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&elf].zone,
        Zone::Graveyard,
        "the -1/-1 counter kills the 1/1"
    );
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the token still comes in");
    assert_eq!(runner.state().objects[&created[0]].name, "Elf");
}

#[test]
fn declining_the_blight_makes_no_token() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    decide(&mut runner, false);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty(), "no blight, no token");
    assert_eq!(
        minus_counters(&runner, bear),
        0,
        "declining the blight places no counter"
    );
}

/// CR 707.8 + CR 704.5f: Grub can blight herself. The copy has the face she
/// shows, and the token outlives her.
#[test]
fn auntie_blighting_herself_leaves_a_token_auntie() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], grub);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Graveyard);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the token still comes in");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, AUNTIE, "CR 707.8: the copy is the face up");
    assert_eq!((token.power, token.toughness), (Some(2), Some(1)));
    assert!(
        token.counters.is_empty(),
        "CR 707.2: counters aren't copied"
    );
}

/// CR 614.1a: Doubling Season doubles the counters and the token, Vorinclex only
/// the counters. Two doublers commute, so the engine applies them with no
/// ordering pause, and the rider still has its referent.
#[test]
fn auntie_with_doublers_makes_two_tokens_of_the_creature_with_four_counters() {
    let (mut runner, grub, big) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_real_card(P0, "Doubling Season", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Vorinclex, Monstrous Raider", Zone::Battlefield, db);
        scenario.add_creature(P0, "Big", 6, 6).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    decide(&mut runner, true);
    runner
        .act(GameAction::SelectCards { cards: vec![big] })
        .expect("blighting the chosen creature must succeed");

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, big), 4, "each doubler doubles");
    let created = tokens(&runner);
    assert_eq!(created.len(), 2, "Doubling Season doubles the token");
    for token in created {
        assert_eq!(runner.state().objects[&token].name, "Big");
    }
}

/// Blights the Bear with Vizier of Remedies and Doubling Season out, whose counter
/// clauses don't commute, and applies `first` at the CR 616.1 pause. The blight
/// choice is answered before that pause, so the rider resumes on its binding.
fn blight_bear_applying_first(first: &str) -> (GameRunner, ObjectId) {
    let (mut runner, grub, (vizier, bear)) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_real_card(P0, "Doubling Season", Zone::Battlefield, db);
        (
            scenario.add_real_card(P0, "Vizier of Remedies", Zone::Battlefield, db),
            scenario.add_creature(P0, "Bear", 2, 2).id(),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, vizier, bear], bear);

    let WaitingFor::ReplacementChoice {
        player,
        candidates,
        kind,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 616.1: the order of these replacements matters; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "CR 616.1: the creature's controller chooses");
    assert_eq!(kind, ReplacementChoiceKind::Order);
    let mut offered: Vec<_> = candidates.iter().map(|c| c.source_name.as_str()).collect();
    offered.sort();
    assert_eq!(offered, ["Doubling Season", "Vizier of Remedies"]);
    let index = candidates
        .iter()
        .position(|c| c.source_name == first)
        .expect("the chosen replacement is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing a replacement must succeed");
    (runner, bear)
}

/// CR 701.68c: the rider copies the chosen creature whatever the counters came to.
/// Doubling Season's token clause doubles it in either order.
fn assert_two_attacking_bear_tokens(runner: &GameRunner) {
    assert_settled(runner);
    let created = tokens(runner);
    assert_eq!(created.len(), 2, "Doubling Season doubles the token");
    for id in created {
        let token = &runner.state().objects[&id];
        assert_eq!(token.name, "Bear", "the token copies the blighted creature");
        assert!(token.tapped, "the token enters tapped");
        assert!(
            is_attacking(runner, id),
            "CR 508.4: the token enters attacking"
        );
    }
}

/// CR 616.1 + CR 118.12: Vizier first leaves 1 minus 1, so no counter is put on the
/// Bear and there is nothing to double. "If you do" checks the choice to blight,
/// not what happened to the counter, so the tokens still come in.
#[test]
fn auntie_ordering_vizier_first_places_no_counter_and_still_makes_tokens() {
    let (runner, bear) = blight_bear_applying_first("Vizier of Remedies");

    assert_two_attacking_bear_tokens(&runner);
    assert_eq!(minus_counters(&runner, bear), 0, "1 minus 1, doubled");
}

/// CR 616.1: Doubling Season first makes 1 into 2, and Vizier takes one away.
#[test]
fn auntie_ordering_doubling_season_first_places_one_counter_and_makes_tokens() {
    let (runner, bear) = blight_bear_applying_first("Doubling Season");

    assert_two_attacking_bear_tokens(&runner);
    assert_eq!(minus_counters(&runner, bear), 1, "1 doubled, minus 1");
}

/// CR 701.68b + CR 608.2d: Grub dies with her trigger on the stack and she was
/// the only creature, so there is no blight to offer. The rider's referent is
/// never absent on an accepted blight.
#[test]
fn auntie_with_no_creature_left_cannot_blight() {
    let (mut runner, grub, bolt) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db)
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Grub's trigger is on the stack"
    );
    runner.cast(bolt).target_objects(&[grub]).commit();
    both_pass(&mut runner);
    assert_eq!(
        runner.state().objects[&grub].zone,
        Zone::Graveyard,
        "reach-guard: Lightning Bolt kills Grub"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty());
}

/// Slip Out the Back reads "Put a +1/+1 counter on target
/// creature. It phases out." Cast in response to Grub's trigger, it leaves the
/// Bear on the battlefield with its counter (CR 702.26d) but treated as though it
/// doesn't exist (CR 702.26b), so CR 701.68a's "a creature you control" can't be
/// the Bear, and the copy comes from a legal choice.
#[test]
fn blight_does_not_offer_a_phased_out_creature() {
    let (mut runner, grub, (bear, elf, slip)) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Blue);
        (
            scenario.add_creature(P0, "Bear", 2, 2).id(),
            scenario.add_creature(P0, "Elf", 3, 3).id(),
            scenario.add_real_card(P0, SLIP, Zone::Hand, db),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is on the stack"
    );
    runner.cast(slip).target_object(bear).commit();
    both_pass(&mut runner);
    let phased = &runner.state().objects[&bear];
    assert!(phased.is_phased_out(), "reach-guard: the Bear phased out");
    assert_eq!(phased.zone, Zone::Battlefield, "CR 702.26d: it didn't move");
    assert_eq!(
        phased.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "CR 702.26d: its counter stays on it"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);
    decide(&mut runner, true);
    let WaitingFor::EffectZoneChoice { mut cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "CR 701.68a: blighting asks for a creature; got {:?}",
            runner.state().waiting_for
        );
    };
    cards.sort();
    let mut expected = vec![grub, elf];
    expected.sort();
    assert_eq!(cards, expected, "the phased-out Bear is not offered");
    let prompt = runner.state().waiting_for.clone();
    assert!(
        runner
            .act(GameAction::SelectCards { cards: vec![bear] })
            .is_err(),
        "the phased-out Bear can't be chosen"
    );
    assert_eq!(
        runner.state().waiting_for,
        prompt,
        "a refused choice leaves the prompt open"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![elf] })
        .expect("blighting the Elf must succeed");

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, elf), 1);
    assert_eq!(minus_counters(&runner, bear), 0);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    assert_eq!(runner.state().objects[&created[0]].name, "Elf");
    assert!(
        is_attacking(&runner, created[0]),
        "CR 508.4: the token enters attacking"
    );
}

/// CR 701.68b + CR 608.2d + CR 702.26b: Slip Out the Back phases out Grub, her
/// controller's only creature, while her attack trigger waits. The trigger still
/// resolves (CR 113.7a), but no creature exists to blight, so the "you may" is
/// not offered and its rider stays closed.
#[test]
fn auntie_whose_only_creature_phased_out_cannot_blight() {
    let (mut runner, grub, slip) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Blue);
        scenario.add_real_card(P0, SLIP, Zone::Hand, db)
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    runner.cast(slip).target_object(grub).commit();
    both_pass(&mut runner);
    assert!(
        runner.state().objects[&grub].is_phased_out(),
        "reach-guard: Grub phased out"
    );
    assert!(
        !is_attacking(&runner, grub),
        "CR 506.4: a permanent that phases out is removed from combat"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: CR 113.7a, Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty(), "no blight, no token");
    assert_eq!(minus_counters(&runner, grub), 0);
}

/// CR 702.26b: Clever Concealment reads "Any number of target nonland
/// permanents you control phase out." With Solemnity ("Counters can't be put on
/// artifacts, creatures, enchantments, or lands.") phased out, Solemnity is
/// treated as though it doesn't exist and forbids nothing, so Grub can blight.
/// The paired negative is `auntie_cannot_blight_under_solemnity`.
#[test]
fn a_phased_out_solemnity_does_not_stop_the_blight() {
    let (mut runner, grub, (solemnity, bear, concealment)) =
        grub_board(Phase::BeginCombat, |scenario, db| {
            for color in [
                ManaColor::White,
                ManaColor::White,
                ManaColor::Blue,
                ManaColor::Blue,
            ] {
                scenario.add_basic_land(P0, color);
            }
            (
                scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db),
                scenario.add_creature(P0, "Bear", 2, 2).id(),
                scenario.add_real_card(P0, CONCEALMENT, Zone::Hand, db),
            )
        });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is on the stack"
    );
    runner
        .cast(concealment)
        .target_objects(&[solemnity])
        .commit();
    both_pass(&mut runner);
    assert!(
        runner.state().objects[&solemnity].is_phased_out(),
        "reach-guard: Solemnity phased out"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, bear), 1);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    assert_eq!(runner.state().objects[&created[0]].name, "Bear");
}

/// CR 701.68b + CR 614.17: Solemnity reads "Counters can't be put on artifacts,
/// creatures, enchantments, or lands." so Grub can't blight at all.
#[test]
fn auntie_cannot_blight_under_solemnity() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is on the stack"
    );
    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty(), "no blight, no token");
    assert_eq!(minus_counters(&runner, bear), 0);
    assert_eq!(minus_counters(&runner, grub), 0);
}

/// CR 701.68b + CR 608.2d: Melira's Keepers "can't have counters put on it", so
/// it is not a creature the player can put the counter on.
#[test]
fn blight_does_not_offer_a_creature_that_cannot_take_the_counter() {
    let (mut runner, grub, (bear, keepers)) = grub_board(Phase::BeginCombat, |scenario, _| {
        (
            scenario.add_creature(P0, "Bear", 2, 2).id(),
            scenario
                .add_creature_from_oracle(P0, "Melira's Keepers", 4, 4, KEEPERS_ORACLE)
                .id(),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, keepers), 0);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1);
    assert_eq!(runner.state().objects[&created[0]].name, "Bear");
}

/// CR 118.12 + CR 701.68b: Gutsplitter Gang reads "you may blight 2. If you
/// don't, you lose 3 life." A player who can't blight doesn't, so the life is
/// lost with no prompt.
#[test]
fn gutsplitter_gang_that_cannot_blight_loses_three_life() {
    let (mut runner, ()) = board(Phase::Upkeep, |scenario, db| {
        scenario.add_creature_from_oracle(P0, "Gutsplitter Gang", 6, 6, GANG_ORACLE);
        scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
    });

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: the first-main trigger is on the stack"
    );
    both_pass(&mut runner);

    assert_settled(&runner);
    assert_eq!(runner.state().players[0].life, 17);
}

/// CR 118.12 + CR 701.68b: Scuzzback Scrounger reads "you may blight 1. If you do,
/// create a Treasure token." A player who can't blight never does, so the rider
/// stays closed. The paired board is the same line with the blight available.
#[test]
fn scuzzback_scrounger_that_cannot_blight_makes_no_treasure() {
    let stage = |solemnity: bool| {
        move |scenario: &mut GameScenario, db: &'static CardDatabase| {
            if solemnity {
                scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
            }
            scenario
                .add_creature_from_oracle(P0, "Scuzzback Scrounger", 3, 2, SCROUNGER_ORACLE)
                .id()
        }
    };

    let (mut able, scrounger) = board(Phase::Upkeep, stage(false));
    able.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut able);
    blight(&mut able, &[scrounger], scrounger);
    assert_settled(&able);
    assert_eq!(tokens(&able).len(), 1, "blighting makes the Treasure");

    let (mut unable, _) = board(Phase::Upkeep, stage(true));
    unable.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        unable.state().stack.len(),
        1,
        "reach-guard: the first-main trigger is on the stack"
    );
    both_pass(&mut unable);

    assert_settled(&unable);
    assert!(tokens(&unable).is_empty(), "no blight, no Treasure");
}

/// CR 701.68b + CR 118.12: a blight nested under another accepted "you may"
/// still can't be chosen when no creature can take the counter, so its own
/// "If you do" stays closed. No printed card nests an optional blight this way
/// (every printed optional blight starts its ability, as Scuzzback Scrounger's
/// does), so this line is synthetic. It guards the inherited "performed" flag
/// from the accepted draw.
#[test]
fn nested_blight_that_cannot_happen_keeps_its_rider_closed() {
    const NESTED_ORACLE: &str = "At the beginning of your first main phase, you may draw a card. \
If you do, you may blight 1. If you do, create a Treasure token.";
    let (mut runner, _) = board(Phase::Upkeep, |scenario, _| {
        scenario
            .add_artifact_from_oracle(P0, "Nested Blight Test Artifact", NESTED_ORACLE)
            .id()
    });
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: the first-main trigger is on the stack"
    );
    both_pass(&mut runner);
    // The draw is the only choice: no creature is on the battlefield, so the
    // nested blight is never offered.
    decide(&mut runner, true);

    assert_settled(&runner);
    assert!(
        tokens(&runner).is_empty(),
        "the blight never happened, so its rider makes no Treasure"
    );
}

/// CR 603.12: Warren Torchmaster reads "you may blight 1. When you do, target
/// creature gains haste until end of turn." The paired board is the same line
/// with the blight available.
#[test]
fn warren_torchmaster_that_cannot_blight_grants_no_haste() {
    let stage = |solemnity: bool| {
        move |scenario: &mut GameScenario, db: &'static CardDatabase| {
            if solemnity {
                scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
            }
            (
                scenario
                    .add_creature_from_oracle(P0, "Warren Torchmaster", 2, 2, TORCHMASTER_ORACLE)
                    .id(),
                scenario.add_creature(P0, "Bear", 2, 2).id(),
            )
        }
    };

    let (mut able, (torchmaster, bear)) = board(Phase::PreCombatMain, stage(false));
    able.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut able);
    blight(&mut able, &[torchmaster, bear], torchmaster);
    assert!(
        matches!(
            able.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "CR 603.12: the blight fires the reflexive trigger; got {:?}",
        able.state().waiting_for
    );

    let (mut unable, (torchmaster, bear)) = board(Phase::PreCombatMain, stage(true));
    unable.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        unable.state().stack.len(),
        1,
        "reach-guard: the beginning-of-combat trigger is on the stack"
    );
    both_pass(&mut unable);

    assert_settled(&unable);
    for id in [torchmaster, bear] {
        assert!(!unable.state().objects[&id].has_keyword(&Keyword::Haste));
    }
}

/// CR 400.7j + CR 707.9: Nexus of Becoming's copy source is the card its own
/// exile just moved, so a referent bound by an effect reads the same ladder as
/// one bound by a cost.
#[test]
fn nexus_of_becoming_copies_the_card_exiled_this_way() {
    let (mut runner, (goblin, bears)) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        scenario.add_land_to_library_top(P0, "Forest");
        (
            scenario.add_real_card(P0, GOBLIN, Zone::Hand, db),
            scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db),
        )
    });

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, true);
    let WaitingFor::EffectZoneChoice { mut cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the exile must ask for a card; got {:?}",
            runner.state().waiting_for
        );
    };
    cards.sort();
    let mut expected = vec![goblin, bears];
    expected.sort();
    assert_eq!(cards, expected, "the land drawn is not offered");
    runner
        .act(GameAction::SelectCards {
            cards: vec![goblin],
        })
        .expect("exiling the Goblin must succeed");

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Exile);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the exiled card provides one token");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, GOBLIN);
    assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Golem"));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Goblin"));
}

/// With one eligible card the exile takes it without a selection prompt, and the
/// referent still reaches the copy.
#[test]
fn nexus_of_becoming_copies_the_only_eligible_card() {
    let (mut runner, goblin) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        scenario.add_land_to_library_top(P0, "Forest");
        scenario.add_real_card(P0, GOBLIN, Zone::Hand, db)
    });

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Exile);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the exiled card provides one token");
    assert_eq!(runner.state().objects[&created[0]].name, GOBLIN);
}

/// "If you do" stays closed when the Nexus is declined with a legal card in hand.
#[test]
fn nexus_of_becoming_declined_makes_no_token() {
    let (mut runner, goblin) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        scenario.add_land_to_library_top(P0, "Forest");
        scenario.add_real_card(P0, GOBLIN, Zone::Hand, db)
    });

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, false);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty());
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Hand);
}

/// Asserts the real card's Oracle text is `oracle`, so the fixture parse the
/// test runs is the card quoted in its doc.
fn assert_oracle(db: &CardDatabase, name: &str, oracle: &str) {
    assert_eq!(
        db.get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the fixture"))
            .oracle_text
            .as_deref(),
        Some(oracle)
    );
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Nexus of Becoming: "At the beginning of combat on your turn, draw a card.
/// Then you may exile an artifact or creature card from your hand. If you do,
/// create a token that's a copy of the exiled card, except it's a 3/3 Golem
/// artifact creature in addition to its other types."
///
/// CR 118.12 + CR 118.3 + CR 608.2d: with no artifact or creature card in hand
/// after the draw, the exile is impossible, so it is not offered and "If you do"
/// stays closed.
#[test]
fn nexus_with_no_eligible_card_offers_no_exile() {
    let (mut runner, (plains, forest)) = board(Phase::PreCombatMain, |scenario, _| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        let forest = scenario.add_land_to_library_top(P0, "Forest").id();
        let plains = scenario.add_land_to_hand(P0, "Plains").id();
        (plains, forest)
    });

    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().stack.len(), 1, "reach: the Nexus trigger");
    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty());
    assert_eq!(runner.state().objects[&plains].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Hand,
        "the draw ran, so the trigger resolved"
    );
}

/// Nexus of Becoming (Oracle text above): CR 608.2c, "draw a card" happens
/// before "Then you may exile", so the drawn creature card is in the pool.
#[test]
fn nexus_draw_feeds_the_exile_pool() {
    let (mut runner, (plains, bears)) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
        let plains = scenario.add_land_to_hand(P0, "Plains").id();
        (plains, bears)
    });
    // Setup only: Grizzly Bears is the card the draw takes.
    {
        let library = &mut runner.state_mut().players[0].library;
        library.retain(|id| *id != bears);
        library.insert(0, bears);
    }

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&plains].zone, Zone::Hand);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the exiled card provides one token");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, "Grizzly Bears");
    assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Golem"));
}

/// Hakbal of the Surging Soul: "At the beginning of combat on your turn, each
/// Merfolk creature you control explores. (…) Whenever Hakbal attacks, you may
/// put a land card from your hand onto the battlefield. If you don't, draw a
/// card."
///
/// CR 118.12 + CR 608.2d: with no land card in hand the put can't be chosen, so
/// "If you don't" draws without a prompt. With a land card the prompt opens.
#[test]
fn hakbal_with_no_land_in_hand_draws_without_a_prompt() {
    for land_in_hand in [false, true] {
        let (mut runner, (hakbal, card)) = board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, "Hakbal of the Surging Soul", HAKBAL_ORACLE);
            let hakbal =
                scenario.add_real_card(P0, "Hakbal of the Surging Soul", Zone::Battlefield, db);
            let card = if land_in_hand {
                scenario.add_land_to_hand(P0, "Forest").id()
            } else {
                scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db)
            };
            (hakbal, card)
        });

        runner.advance_to_phase(Phase::BeginCombat);
        assert_eq!(runner.state().stack.len(), 1, "reach: the explore trigger");
        both_pass(&mut runner);
        let WaitingFor::DigChoice { cards, .. } = runner.state().waiting_for.clone() else {
            panic!(
                "the explore asks about the revealed card; got {:?}",
                runner.state().waiting_for
            );
        };
        runner
            .act(GameAction::SelectCards { cards })
            .expect("keeping the revealed card on top must succeed");
        pass_into(&mut runner, Phase::DeclareAttackers);
        let hand_before = runner.state().players[0].hand.len();
        runner
            .declare_attackers(&[(hakbal, AttackTarget::Player(P1))])
            .expect("the attack must be legal");
        assert_eq!(runner.state().stack.len(), 1, "reach: the attack trigger");
        both_pass(&mut runner);

        if land_in_hand {
            decide(&mut runner, false);
        }
        assert_settled(&runner);
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + 1,
            "land in hand: {land_in_hand}; \"If you don't\" draws a card"
        );
        assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    }
}

/// God-Pharaoh's Gift: "At the beginning of combat on your turn, you may exile a
/// creature card from your graveyard. If you do, create a token that's a copy of
/// that card, except it's a 4/4 black Zombie. It gains haste until end of turn."
///
/// CR 118.12 + CR 608.2d: an empty graveyard can't supply the exile, so it is
/// not offered and no token is made. A creature card there opens the prompt.
#[test]
fn gods_gift_with_an_empty_graveyard_offers_no_exile() {
    let (mut empty, ()) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, "God-Pharaoh's Gift", GODS_GIFT_ORACLE);
        scenario.add_real_card(P0, "God-Pharaoh's Gift", Zone::Battlefield, db);
    });
    empty.advance_to_phase(Phase::BeginCombat);
    assert_eq!(empty.state().stack.len(), 1, "reach: the Gift trigger");
    both_pass(&mut empty);
    assert_settled(&empty);
    assert!(tokens(&empty).is_empty());

    let (mut stocked, ()) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_real_card(P0, "God-Pharaoh's Gift", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    });
    stocked.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut stocked);
    assert_optional_prompt(&stocked);
}

/// Aegis Sculptor: "Flying / Ward {2} (…) / At the beginning of your upkeep, you
/// may exile two cards from your graveyard. If you do, put a +1/+1 counter on
/// this creature."
///
/// CR 118.3 + CR 608.2d + CR 118.12: "two cards" needs two whatever the
/// graveyard holds, so an empty graveyard can't supply the exile: it is not
/// offered and no counter is put. Two cards there open the prompt.
#[test]
fn aegis_sculptor_with_an_empty_graveyard_offers_no_exile() {
    let (mut empty, aegis) = board(Phase::Untap, |scenario, db| {
        assert_oracle(db, "Aegis Sculptor", AEGIS_ORACLE);
        scenario.add_real_card(P0, "Aegis Sculptor", Zone::Battlefield, db)
    });
    empty.advance_to_phase(Phase::Upkeep);
    assert_eq!(empty.state().stack.len(), 1, "reach: the upkeep trigger");
    assert_eq!(empty.state().stack[0].source_id, aegis);
    both_pass(&mut empty);
    assert_settled(&empty);
    assert_eq!(
        plus_counters(&empty, aegis),
        0,
        "\"If you do\" stays closed"
    );

    let (mut stocked, (aegis, cards)) = board(Phase::Untap, |scenario, db| {
        let aegis = scenario.add_real_card(P0, "Aegis Sculptor", Zone::Battlefield, db);
        let cards: Vec<ObjectId> = (0..2)
            .map(|_| scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db))
            .collect();
        (aegis, cards)
    });
    stocked.advance_to_phase(Phase::Upkeep);
    both_pass(&mut stocked);
    decide(&mut stocked, false);
    assert_settled(&stocked);
    assert_eq!(plus_counters(&stocked, aegis), 0);
    for id in cards {
        assert_eq!(stocked.state().objects[&id].zone, Zone::Graveyard);
    }
}

/// The reported "doesn't trigger on cast": casting Grub reaches the enters half
/// of "enters or transforms into".
#[test]
fn matriarch_returns_a_goblin_when_cast() {
    let (mut runner, (grub, goblin)) = board(Phase::PreCombatMain, |scenario, db| {
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        (
            scenario.add_real_card(P0, MATRIARCH, Zone::Hand, db),
            scenario.add_real_card(P0, GOBLIN, Zone::Graveyard, db),
        )
    });

    runner.cast(grub).commit();
    both_pass(&mut runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Battlefield);
    select_goblin(&mut runner, goblin);
    both_pass(&mut runner);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Hand);
}

/// The reported "doesn't trigger on transform" (CR 701.27e): paying {B} on
/// Auntie transforms her into the Matriarch through her own first-main trigger.
#[test]
fn matriarch_returns_a_goblin_when_auntie_transforms_into_her() {
    let (mut runner, grub, goblin) = grub_board(Phase::Upkeep, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Black);
        scenario.add_real_card(P0, GOBLIN, Zone::Graveyard, db)
    });
    flip(&mut runner, grub);

    runner.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_eq!(
        runner.state().objects[&grub].name,
        MATRIARCH,
        "reach-guard: paying {{B}} must transform Grub"
    );
    select_goblin(&mut runner, goblin);
    both_pass(&mut runner);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Hand);
}

/// "Transforms into Grub, Storied Matriarch" is not "transforms": flipping to
/// Auntie leaves the graveyard alone.
#[test]
fn transforming_into_auntie_does_not_return_a_goblin() {
    let (mut runner, grub, goblin) = grub_board(Phase::Upkeep, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_real_card(P0, GOBLIN, Zone::Graveyard, db)
    });

    runner.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_eq!(
        runner.state().objects[&grub].name,
        AUNTIE,
        "reach-guard: paying {{R}} must transform Grub"
    );
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Graveyard);
}
