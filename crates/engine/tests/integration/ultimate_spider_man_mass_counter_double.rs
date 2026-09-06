//! Ultimate Spider-Man (back face) — "Whenever you attack, double the number of
//! each kind of counter on each Spider and legendary creature you control."
//!
//! CR 701.10e (counter multiplication) + CR 115.1 / CR 608.2d (the recipient is a
//! DESCRIBED population, not a target, so it is enumerated while the effect is
//! applied) + CR 205.4a (the right conjunct "legendary creature" leads with a
//! SUPERTYPE, so the union must keep both legs).
//!
//! Before the fix three independent defects stacked up:
//!   1. the parser gated the non-targeted "each kind of counter on <descriptor>"
//!      form to an honest `Effect::Unimplemented`, so the trigger did nothing;
//!   2. `Effect::Double { DoubleTarget::Counters }` had no battlefield-population
//!      tier at all, so even a lowered effect would have doubled nothing; and
//!   3. `parse_type_phrase_with_ctx`'s bare "and"/"or" branch rejected a
//!      supertype-led right conjunct, collapsing the population to
//!      `Typed{[Subtype("Spider")]}` with NO controller — which both misses the
//!      controller's legendary creatures and reaches across the table.
//!
//! This test drives the real combat pipeline (declare attackers → `YouAttack`
//! trigger onto the stack → resolve). The declared attackers are cleared before
//! the combat-damage step (the `archnemesis_you_attack_enchanted_player.rs`
//! recipe), so every asserted counter delta comes solely from the resolved
//! trigger.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

/// The back face's verbatim Oracle text (committed identically at
/// `crates/engine/src/parser/oracle_effect/counter.rs`'s parser test).
const ULTIMATE_SPIDER_MAN: &str = "First strike, haste\n\
     Camouflage — {2}: Put a +1/+1 counter on Ultimate Spider-Man. He gains hexproof and becomes colorless until end of turn.\n\
     Whenever you attack, double the number of each kind of counter on each Spider and legendary creature you control.";

/// The typed-counter sibling that already works today (`Effect::MultiplyCounter`'s
/// mass tier). It is the hostile fixture: the same board, the same trigger shape,
/// a population the runtime already enumerated before this change — it must stay
/// byte-identical, proving the new shared helper narrowed nothing that worked.
const CONTROL_MASS_MULTIPLY: &str =
    "Whenever you attack, double the number of +1/+1 counters on each creature you control.";

fn counters(runner: &GameRunner, id: ObjectId, counter: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&counter)
        .copied()
        .unwrap_or(0)
}

/// The board of §P2: one attacking trigger source plus five bystanders whose
/// counter counts are all distinct, so no assertion can pass by aliasing.
struct Board {
    runner: GameRunner,
    source: ObjectId,
    spider: ObjectId,
    legend: ObjectId,
    plain: ObjectId,
    opp_spider: ObjectId,
    opp_legend: ObjectId,
}

fn setup(trigger_oracle: &str) -> Board {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let source = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Ultimate Spider-Man", 4, 3, trigger_oracle);
        builder.as_legendary();
        builder.with_subtypes(vec!["Spider", "Human", "Hero"]);
        builder.id()
    };
    let spider = {
        let mut builder = scenario.add_creature(P0, "Web Weaver", 2, 2);
        builder.with_subtypes(vec!["Spider"]);
        builder.id()
    };
    let legend = {
        let mut builder = scenario.add_creature(P0, "Hero of Forest Lane", 3, 3);
        builder.as_legendary();
        builder.id()
    };
    let plain = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let opp_spider = {
        let mut builder = scenario.add_creature(P1, "Rival Spider", 2, 2);
        builder.with_subtypes(vec!["Spider"]);
        builder.id()
    };
    let opp_legend = {
        let mut builder = scenario.add_creature(P1, "Rival Legend", 3, 3);
        builder.as_legendary();
        builder.id()
    };

    scenario.with_counter(source, CounterType::Plus1Plus1, 1);
    scenario.with_counter(spider, CounterType::Plus1Plus1, 2);
    // CR 122.1: a second counter KIND on the same permanent. Lore counters are
    // inert on a non-Saga creature, so doubling them isolates the UNTYPED
    // "each kind of counter" semantics that `MultiplyCounter` cannot express.
    scenario.with_counter(spider, CounterType::Lore, 3);
    scenario.with_counter(legend, CounterType::Plus1Plus1, 4);
    scenario.with_counter(plain, CounterType::Plus1Plus1, 5);
    scenario.with_counter(opp_spider, CounterType::Plus1Plus1, 7);
    scenario.with_counter(opp_legend, CounterType::Plus1Plus1, 9);

    Board {
        runner: scenario.build(),
        source,
        spider,
        legend,
        plain,
        opp_spider,
        opp_legend,
    }
}

/// Set the active player and pass priority until the declare-attackers step.
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

/// CR 508.1: the `YouAttack` trigger fires at attack declaration. Drop the
/// declared attackers so the combat-damage step contributes nothing.
fn attack_and_resolve_trigger_only(board: &mut Board) {
    hand_turn_to(&mut board.runner, P0);
    board
        .runner
        .declare_attackers(&[(board.source, AttackTarget::Player(P1))])
        .expect("declaring an attack should succeed");
    if let Some(combat) = &mut board.runner.state_mut().combat {
        combat.attackers.clear();
    }
    board.runner.advance_until_stack_empty();
}

/// NAMED FIX (CR 701.10e + CR 205.4a): every kind of counter on each Spider AND
/// each legendary creature the trigger's controller controls is doubled.
///
/// Revert-failing: on the pre-fix tree the trigger's effect is an honest
/// `Effect::Unimplemented`, so all four positive deltas are `n → n`.
#[test]
fn ultimate_spider_man_attack_trigger_doubles_every_counter_kind_on_spiders_and_legends() {
    let mut board = setup(ULTIMATE_SPIDER_MAN);
    attack_and_resolve_trigger_only(&mut board);

    // --- Positive deltas: the described population.
    assert_eq!(
        counters(&board.runner, board.source, CounterType::Plus1Plus1),
        2,
        "the source is a legendary Spider it controls: 1 → 2"
    );
    assert_eq!(
        counters(&board.runner, board.spider, CounterType::Plus1Plus1),
        4,
        "a controlled Spider is in the population: 2 → 4"
    );
    // CR 701.10e: "each kind of counter" — not just +1/+1.
    assert_eq!(
        counters(&board.runner, board.spider, CounterType::Lore),
        6,
        "the UNTYPED form doubles every counter kind, including Lore: 3 → 6"
    );
    assert_eq!(
        counters(&board.runner, board.legend, CounterType::Plus1Plus1),
        8,
        "the supertype-led right conjunct must survive the union: 4 → 8"
    );

    // --- Negatives, in the same test so none can pass vacuously.
    assert_eq!(
        counters(&board.runner, board.plain, CounterType::Plus1Plus1),
        5,
        "a creature that is neither a Spider nor legendary is outside the population"
    );
    // CR 109.4 + CR 205.4a: the trailing "you control" scopes BOTH legs.
    assert_eq!(
        counters(&board.runner, board.opp_spider, CounterType::Plus1Plus1),
        7,
        "the opponent's Spider must not be doubled — the controller suffix scopes both legs"
    );
    assert_eq!(
        counters(&board.runner, board.opp_legend, CounterType::Plus1Plus1),
        9,
        "the opponent's legendary creature must not be doubled"
    );
}

/// HOSTILE FIXTURE / positive reach-guard: the typed sibling
/// (`Effect::MultiplyCounter`'s pre-existing mass tier, `counters.rs`
/// `resolve_defined_or_targets`) on the identical board. It must keep doubling
/// exactly the controller's creatures after the tier moved into the shared
/// `nontargeted_counter_population_ids` helper — if the new gate were
/// over-broad or over-narrow, this row moves.
#[test]
fn multiply_counter_mass_tier_still_doubles_each_creature_you_control() {
    let mut board = setup(CONTROL_MASS_MULTIPLY);
    attack_and_resolve_trigger_only(&mut board);

    assert_eq!(
        counters(&board.runner, board.source, CounterType::Plus1Plus1),
        2,
        "source 1 → 2"
    );
    assert_eq!(
        counters(&board.runner, board.spider, CounterType::Plus1Plus1),
        4,
        "controlled Spider 2 → 4"
    );
    assert_eq!(
        counters(&board.runner, board.spider, CounterType::Lore),
        3,
        "the TYPED form touches only +1/+1 counters — Lore stays 3"
    );
    assert_eq!(
        counters(&board.runner, board.legend, CounterType::Plus1Plus1),
        8,
        "controlled legend 4 → 8"
    );
    assert_eq!(
        counters(&board.runner, board.plain, CounterType::Plus1Plus1),
        10,
        "every creature you control is in this population: 5 → 10"
    );
    assert_eq!(
        counters(&board.runner, board.opp_spider, CounterType::Plus1Plus1),
        7,
        "opponent untouched"
    );
    assert_eq!(
        counters(&board.runner, board.opp_legend, CounterType::Plus1Plus1),
        9,
        "opponent untouched"
    );
}
