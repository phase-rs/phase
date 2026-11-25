//! Garruk, Cursed Huntsman — the [0] Wolf tokens' granted dies trigger, "put a
//! loyalty counter on each Garruk you control", through the production
//! activation and cast pipelines with verbatim Oracle text.
//!
//! - S1: the full Oracle parses with no `Unimplemented`; the granted trigger's
//!   execute is a mass loyalty-counter placement over the Garruk planeswalker
//!   subtype the trigger's controller controls (CR 205.3j: "Garruk" in "each
//!   Garruk you control" is a planeswalker type, not the card's name).
//! - M1/M2: a Wolf dying (CR 700.4) puts one loyalty counter (CR 122.1) on each
//!   Garruk its controller controls, and loyalty follows the counters
//!   (CR 306.5c). The opponent's Garruk and the controller's non-Garruk
//!   planeswalker are untouched; the leaves-the-battlefield trigger looks back
//!   in time (CR 603.10a).
//! - M3: Garruk ruling — a Garruk whose loyalty hits 0 in the same
//!   state-based-action batch as the Wolves (CR 704.3 + CR 704.5g + CR 704.5i;
//!   damage to a planeswalker removes loyalty counters per CR 120.3c) is in the
//!   graveyard before the triggers resolve, so "each Garruk you control" is
//!   evaluated at resolution (CR 608.2h) and excludes it.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, ControllerRef, Effect, QuantityExpr,
    TargetFilter, TypeFilter,
};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text of Garruk, Cursed Huntsman (`\u{2212}` is the U+2212
/// MINUS SIGN of the printed loyalty costs).
const GARRUK_ORACLE: &str = "[0]: Create two 2/2 black and green Wolf creature tokens with \"When this token dies, put a loyalty counter on each Garruk you control.\"\n[\u{2212}3]: Destroy target creature. Draw a card.\n[\u{2212}6]: You get an emblem with \"Creatures you control get +3/+3 and have trample.\"";

/// Verbatim Oracle text of Garruk, Unleashed: a second Garruk for P0.
const GARRUK_UNLEASHED_ORACLE: &str = "[+1]: Up to one target creature gets +3/+3 and gains trample until end of turn.\n[\u{2212}2]: Create a 3/3 green Beast creature token. Then if an opponent controls more creatures than you, put a loyalty counter on Garruk.\n[\u{2212}7]: You get an emblem with \"At the beginning of your end step, you may search your library for a creature card, put it onto the battlefield, then shuffle.\"";

/// Verbatim Oracle text of Garruk Wildspeaker: the opponent's Garruk.
const GARRUK_WILDSPEAKER_ORACLE: &str = "[+1]: Untap two target lands.\n[\u{2212}1]: Create a 3/3 green Beast creature token.\n[\u{2212}4]: Creatures you control get +3/+3 and gain trample until end of turn.";

/// Verbatim Oracle text of Jace Beleren: P0's non-Garruk planeswalker.
const JACE_BELEREN_ORACLE: &str = "[+2]: Each player draws a card.\n[\u{2212}1]: Target player draws a card.\n[\u{2212}10]: Target player mills twenty cards.";

/// Verbatim Oracle text of Lightning Bolt (`{R}` instant).
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

/// Verbatim Oracle text of Storm's Wrath (`{2}{R}{R}` sorcery).
const STORMS_WRATH_ORACLE: &str =
    "Storm's Wrath deals 4 damage to each creature and each planeswalker.";

fn planeswalker(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    subtype: &str,
    loyalty: u32,
    oracle: &str,
) -> ObjectId {
    scenario
        .add_planeswalker_from_oracle(player, name, subtype, loyalty, oracle)
        .id()
}

fn cursed_huntsman(scenario: &mut GameScenario, loyalty: u32) -> ObjectId {
    planeswalker(
        scenario,
        P0,
        "Garruk, Cursed Huntsman",
        "Garruk",
        loyalty,
        GARRUK_ORACLE,
    )
}

/// Index of the loyalty ability with the given loyalty cost on `source`.
fn loyalty_index(runner: &GameRunner, source: ObjectId, amount: i32) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.cost == Some(AbilityCost::Loyalty { amount }))
        .unwrap_or_else(|| {
            panic!(
                "expected a [{amount}] loyalty ability; got {:?}",
                runner.state().objects[&source].abilities
            )
        })
}

/// Every effect in an ability's `sub_ability` chain, head first.
fn chain_effects(ability: &AbilityDefinition) -> Vec<&Effect> {
    let mut effects = vec![&*ability.effect];
    let mut node = ability.sub_ability.as_deref();
    while let Some(def) = node {
        effects.push(&def.effect);
        node = def.sub_ability.as_deref();
    }
    effects
}

/// The Wolf tokens `player` controls on the battlefield.
fn wolves(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| {
            object.controller == player && object.zone == Zone::Battlefield && object.name == "Wolf"
        })
        .map(|(id, _)| *id)
        .collect();
    ids.sort_by_key(|id| id.0);
    ids
}

fn loyalty(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id].loyalty
}

/// Fund P0's pool directly (mana sourcing is covered elsewhere); each cast
/// below still travels the full announce/pay/target pipeline.
fn add_mana(runner: &mut GameRunner, color: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(color, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

// ─── S1: the full Oracle lowers with no gaps ─────────────────────────────────

/// The whole card parses with zero `Unimplemented`, and the granted dies
/// trigger places one loyalty counter on each Garruk-subtype permanent its
/// controller controls (not on the token itself, not on "~").
#[test]
fn full_oracle_parses_with_no_unimplemented() {
    let parsed = parse_oracle_text(
        GARRUK_ORACLE,
        "Garruk, Cursed Huntsman",
        &[],
        &["Legendary".to_string(), "Planeswalker".to_string()],
        &["Garruk".to_string()],
    );
    assert_eq!(
        parsed.abilities.len(),
        3,
        "all three loyalty abilities must parse; got {:?}",
        parsed.abilities
    );
    for ability in &parsed.abilities {
        for effect in chain_effects(ability) {
            assert!(
                !matches!(effect, Effect::Unimplemented { .. }),
                "no honest gap allowed in Garruk's Oracle; got {effect:?}"
            );
        }
    }

    // Reach guard: the [0] is a Token whose statics carry the granted trigger.
    let Effect::Token {
        static_abilities, ..
    } = &*parsed.abilities[0].effect
    else {
        panic!(
            "the [0] must create the Wolf tokens; got {:?}",
            parsed.abilities[0].effect
        );
    };
    let granted: Vec<_> = static_abilities
        .iter()
        .flat_map(|def| def.modifications.iter())
        .filter_map(|modification| match modification {
            ContinuousModification::GrantTrigger { trigger } => Some(trigger),
            _ => None,
        })
        .collect();
    assert_eq!(
        granted.len(),
        1,
        "the Wolves carry exactly one granted trigger; got {static_abilities:?}"
    );
    let execute = granted[0]
        .execute
        .as_deref()
        .expect("the granted dies trigger has an execute");
    let Effect::PutCounterAll {
        counter_type,
        count,
        target,
    } = &*execute.effect
    else {
        panic!(
            "the dies trigger must put counters on each Garruk; got {:?}",
            execute.effect
        );
    };
    assert_eq!(*counter_type, CounterType::Loyalty);
    assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
    let TargetFilter::Typed(typed) = target else {
        panic!("each Garruk you control is a typed filter; got {target:?}");
    };
    assert_eq!(
        typed.type_filters,
        vec![TypeFilter::Subtype("Garruk".to_string())],
        "the recipient is the Garruk planeswalker type (fails on the `~` misparse)"
    );
    assert_eq!(typed.controller, Some(ControllerRef::You));
}

// ─── M1/M2: a Wolf's death adds loyalty to each Garruk its controller controls

#[test]
fn wolf_death_adds_loyalty_to_each_garruk_you_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let huntsman = cursed_huntsman(&mut scenario, 5);
    let unleashed = planeswalker(
        &mut scenario,
        P0,
        "Garruk, Unleashed",
        "Garruk",
        4,
        GARRUK_UNLEASHED_ORACLE,
    );
    let jace = planeswalker(
        &mut scenario,
        P0,
        "Jace Beleren",
        "Jace",
        3,
        JACE_BELEREN_ORACLE,
    );
    let wildspeaker = planeswalker(
        &mut scenario,
        P1,
        "Garruk Wildspeaker",
        "Garruk",
        3,
        GARRUK_WILDSPEAKER_ORACLE,
    );
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 1);

    let zero = loyalty_index(&runner, huntsman, 0);
    runner.activate(huntsman, zero).resolve();
    let created = wolves(&runner, P0);
    assert_eq!(
        created.len(),
        2,
        "the [0] creates two Wolves; got {created:?}"
    );
    assert_eq!(loyalty(&runner, huntsman), Some(5), "the [0] costs nothing");

    let (doomed, survivor) = (created[0], created[1]);
    let outcome = runner.cast(bolt).target_objects(&[doomed]).resolve();

    // Reach guard: the bolted Wolf died (a token that leaves the battlefield
    // ceases to exist, so it is gone from P0's Wolves), the other survived.
    assert_eq!(
        wolves(&runner, P0),
        vec![survivor],
        "only the bolted Wolf {doomed:?} died"
    );

    // CR 122.1 + CR 306.5c: one loyalty counter on each Garruk P0 controls.
    assert_eq!(
        loyalty(&runner, huntsman),
        Some(6),
        "Garruk, Cursed Huntsman gains one loyalty (fails if the trigger is unparsed)"
    );
    assert_eq!(
        loyalty(&runner, unleashed),
        Some(5),
        "every Garruk P0 controls gains one loyalty, not just the source"
    );
    // M2: "you" is the Wolf's controller — the opponent's Garruk is untouched.
    assert_eq!(
        loyalty(&runner, wildspeaker),
        Some(3),
        "an opponent's Garruk gains nothing"
    );
    // Subtype filter: P0's non-Garruk planeswalker is untouched.
    assert_eq!(loyalty(&runner, jace), Some(3), "Jace is not a Garruk");
    assert_eq!(
        outcome.counters(survivor, CounterType::Loyalty),
        0,
        "the surviving Wolf gets no loyalty counter"
    );
    assert!(runner.state().stack.is_empty(), "the trigger resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "back at priority; got {:?}",
        runner.state().waiting_for
    );
}

// ─── M3: a Garruk dying alongside the Wolves is not saved ────────────────────

/// Ruling: "If lethal damage is dealt to one of Garruk's Wolf tokens at the
/// same time that Garruk's loyalty is brought to 0 or less, Garruk is put into
/// your graveyard before the Wolf's triggered ability can save him."
#[test]
fn garruk_dying_with_wolves_is_not_saved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let huntsman = cursed_huntsman(&mut scenario, 4);
    let unleashed = planeswalker(
        &mut scenario,
        P0,
        "Garruk, Unleashed",
        "Garruk",
        5,
        GARRUK_UNLEASHED_ORACLE,
    );
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Storm's Wrath", false, STORMS_WRATH_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 4);

    let zero = loyalty_index(&runner, huntsman, 0);
    runner.activate(huntsman, zero).resolve();
    let created = wolves(&runner, P0);
    assert_eq!(
        created.len(),
        2,
        "the [0] creates two Wolves; got {created:?}"
    );

    let outcome = runner.cast(wrath).resolve();

    // Reach guard: both Wolves died.
    assert!(
        wolves(&runner, P0).is_empty(),
        "both Wolves {created:?} died"
    );
    // CR 704.3 + CR 704.5i: Garruk hit 0 loyalty in the same SBA batch.
    outcome.assert_zone(&[huntsman], Zone::Graveyard);
    // 5 − 4 damage = 1, then +1 from each Wolf trigger (proves both resolved).
    assert_eq!(
        loyalty(&runner, unleashed),
        Some(3),
        "the surviving Garruk gets one counter per Wolf trigger"
    );
    assert!(runner.state().stack.is_empty(), "both triggers resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "back at priority; got {:?}",
        runner.state().waiting_for
    );
}
