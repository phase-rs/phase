//! Ajani, Strength of the Pride — the three loyalty abilities through the
//! production activation pipeline with verbatim Oracle text.
//!
//! - Row 1 ([-2]): creates a 2/2 white Cat Soldier named Ajani's Pridemate
//!   under the controller (CR 111.3: the creating effect defines the token).
//! - Row 2 ([-2] granted trigger): "Whenever you gain life, put a +1/+1
//!   counter on this token" fires once per life-gain event on the
//!   controller's gain (CR 603.2), and only then.
//! - Row 3 ([0]): "If you have at least 15 life more than your starting life
//!   total" gates at resolution (CR 608.2c); at >= starting+15 Ajani and each
//!   artifact and creature the opponents control are exiled, otherwise nothing
//!   moves (CR 606.3: the declined activation still spends the once-per-turn
//!   loyalty cap).
//! - Row 4 ([+1]): life gain equals creatures plus planeswalkers the
//!   controller controls (CR 119.3).
//!
//! CR numbers verified against docs/MagicCompRules.txt (pinned edition
//! September 25, 2026): 107.3a, 111.3, 119.3, 120.3a, 603.2, 606.3, 608.2c.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, Effect};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text of Ajani, Strength of the Pride (`\u{2212}` is the
/// U+2212 MINUS SIGN of the printed [-2] cost).
const AJANI_ORACLE: &str = "[+1]: You gain life equal to the number of creatures you control plus the number of planeswalkers you control.\n[\u{2212}2]: Create a 2/2 white Cat Soldier creature token named Ajani's Pridemate with \"Whenever you gain life, put a +1/+1 counter on this token.\"\n[0]: If you have at least 15 life more than your starting life total, exile Ajani and each artifact and creature your opponents control.";

fn ajani(scenario: &mut GameScenario, loyalty: u32) -> ObjectId {
    scenario
        .add_planeswalker_from_oracle(
            P0,
            "Ajani, Strength of the Pride",
            "Ajani",
            loyalty,
            AJANI_ORACLE,
        )
        .id()
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
fn chain_effects(ability: &engine::types::ability::AbilityDefinition) -> Vec<&Effect> {
    let mut effects = vec![&*ability.effect];
    let mut node = ability.sub_ability.as_deref();
    while let Some(def) = node {
        effects.push(&def.effect);
        node = def.sub_ability.as_deref();
    }
    effects
}

/// The Pridemate token controlled by `player` with the exact card name.
fn pridemate(runner: &GameRunner, player: PlayerId) -> Option<ObjectId> {
    runner
        .state()
        .objects
        .iter()
        .find(|(_, object)| {
            object.controller == player
                && object.zone == Zone::Battlefield
                && object.name == "Ajani's Pridemate"
        })
        .map(|(id, _)| *id)
}

/// Verbatim Oracle text of Stream of Life (`{X}{G}` sorcery, Scryfall 9ED
/// #272): the printed gain witness for the Row 2 legs.
const STREAM_OF_LIFE_ORACLE: &str = "Target player gains X life.";

/// Verbatim Oracle text of Lightning Bolt (`{R}` instant): the printed
/// pure-loss witness — damage with no life-gain half.
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

fn stream_of_life(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Stream of Life", false, STREAM_OF_LIFE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id()
}

fn lightning_bolt(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
        .id()
}

/// Fund P0's pool directly (mana sourcing is covered elsewhere); each cast
/// below still travels the full announce/pay/target pipeline.
fn add_mana(runner: &mut GameRunner, color: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(color, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

// ─── Parse reach-guard: the full Oracle lowers with no honest gaps ───────────

/// The whole card parses with zero `Unimplemented`, so the runtime rows below
/// cannot pass vacuously through an early-return short-circuit.
#[test]
fn full_oracle_parses_with_no_unimplemented() {
    let parsed = parse_oracle_text(
        AJANI_ORACLE,
        "Ajani, Strength of the Pride",
        &[],
        &["Legendary".to_string(), "Planeswalker".to_string()],
        &["Ajani".to_string()],
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
                "no honest gap allowed in Ajani's Oracle; got {effect:?}"
            );
        }
    }
}

// ─── Row 1: [-2] creates the named 2/2 Cat Soldier ───────────────────────────

/// CR 111.3: the creating effect defines the token's characteristics.
#[test]
fn minus_two_creates_named_cat_soldier_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = ajani(&mut scenario, 5);
    let mut runner = scenario.build();
    let minus_two = loyalty_index(&runner, ajani, -2);

    let outcome = runner.activate(ajani, minus_two).resolve();

    // Paired reach-guard inside the runtime test: the token exists, so a
    // missing trigger below cannot masquerade as a missing token.
    let token = pridemate(&runner, P0).expect("the [-2] must create Ajani's Pridemate");
    outcome.assert_zone(&[token], Zone::Battlefield);
    let object = &runner.state().objects[&token];
    assert_eq!(
        (object.power, object.toughness),
        (Some(2), Some(2)),
        "Pridemate is 2/2"
    );
    assert!(
        object.card_types.subtypes.iter().any(|i| i == "Cat")
            && object.card_types.subtypes.iter().any(|i| i == "Soldier"),
        "Pridemate is a Cat Soldier; got {:?}",
        object.card_types.subtypes
    );
    assert_eq!(
        object.color,
        vec![ManaColor::White],
        "Pridemate is white; got {:?}",
        object.color
    );
    assert!(
        object.card_types.core_types.contains(&CoreType::Creature),
        "Pridemate is a Creature; got {:?}",
        object.card_types.core_types
    );
    assert_eq!(object.controller, P0, "the controller creates the token");
    // The [-2] spends two loyalty (CR 606.4-class payment at activation).
    assert_eq!(
        runner.state().objects[&ajani].loyalty,
        Some(3),
        "[-2] pays from 5 to 3"
    );
}

// ─── Row 2: the granted life-gain trigger fires per controller gain ─────────

/// CR 603.2: the token's granted "Whenever you gain life" trigger fires on
/// the controller's life gain and puts exactly one +1/+1 counter on the token
/// per gain event.
#[test]
fn granted_trigger_puts_one_counter_per_controller_gain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = ajani(&mut scenario, 5);
    let stream_one = stream_of_life(&mut scenario);
    let stream_two = stream_of_life(&mut scenario);
    let mut runner = scenario.build();
    // X=1 costs {1}{G}, X=2 costs {2}{G}; spent mana stays spent across
    // the two casts in this turn.
    add_mana(&mut runner, ManaType::Green, 5);
    let minus_two = loyalty_index(&runner, ajani, -2);
    runner.activate(ajani, minus_two).resolve();
    let token = pridemate(&runner, P0).expect("row 1 proves the token exists");

    // CR 107.3a: X is announced as part of the cast; the text's X uses it.
    let outcome = runner.cast(stream_one).x(1).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_eq!(
        outcome.counters(token, CounterType::Plus1Plus1),
        1,
        "first gain puts one counter on the token (fails if the granted trigger never registered/fired)"
    );

    let outcome = runner.cast(stream_two).x(2).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 2);
    assert_eq!(
        outcome.counters(token, CounterType::Plus1Plus1),
        2,
        "one counter per gain event, not per life point"
    );
}

/// Same-test guard plus hostile rows: a controller gain first proves the
/// granted trigger is live (removing the grant fails there), then an
/// opponent's gain and a controller loss each leave that counter unchanged.
#[test]
fn granted_trigger_ignores_opponent_gain_and_life_loss() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = ajani(&mut scenario, 5);
    let stream_self = stream_of_life(&mut scenario);
    let stream_foe = stream_of_life(&mut scenario);
    let bolt = lightning_bolt(&mut scenario);
    let mut runner = scenario.build();
    // X=1 costs {1}{G}, X=3 costs {3}{G}; the Bolt is cast free.
    add_mana(&mut runner, ManaType::Green, 6);
    let minus_two = loyalty_index(&runner, ajani, -2);
    runner.activate(ajani, minus_two).resolve();
    let token = pridemate(&runner, P0).expect("row 1 proves the token exists");

    // Positive reach-guard: the controller's gain adds a counter, so the
    // trigger wiring is live before the negative legs run.
    let outcome = runner.cast(stream_self).x(1).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_eq!(
        outcome.counters(token, CounterType::Plus1Plus1),
        1,
        "controller gain proves the granted trigger fired (fails if the grant never installed)"
    );

    // An opponent's gain is a real life-gain event for someone else: life
    // moves (reach-guard) but the "you" binding excludes it.
    let outcome = runner.cast(stream_foe).x(3).target_player(P1).resolve();
    outcome.assert_life_delta(P1, 3);
    assert_eq!(
        outcome.counters(token, CounterType::Plus1Plus1),
        1,
        "an opponent's gain must not trigger the token"
    );

    // Controller life loss via damage (CR 120.3a: damage to a player causes
    // that much life loss): life moves (reach-guard) but
    // `match_life_gained` declines the non-positive amount. A damage spell
    // witnesses the loss rather than Soul Feast, whose unconditional "you
    // gain 4 life" half would fire the very trigger this leg excludes.
    let outcome = runner.cast(bolt).target_player(P0).resolve();
    outcome.assert_life_delta(P0, -3);
    assert_eq!(
        outcome.counters(token, CounterType::Plus1Plus1),
        1,
        "losing life must not trigger the token"
    );
}

/// Two distinguishable Pridemates each track their own counters: the granted
/// trigger binds per token (no shared sink, no cross-talk).
#[test]
fn two_pridemates_each_track_their_own_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = ajani(&mut scenario, 5);
    let stream = stream_of_life(&mut scenario);
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 2);
    let minus_two = loyalty_index(&runner, ajani, -2);
    runner.activate(ajani, minus_two).resolve();
    let first = pridemate(&runner, P0).expect("first [-2] creates a token");
    // Fresh once-per-turn cap, as on a later turn (CR 606.3): the second [-2]
    // still travels the full activation pipeline; only the cap bookkeeping is
    // reset as fixture setup.
    runner
        .state_mut()
        .objects
        .get_mut(&ajani)
        .expect("ajani")
        .loyalty_activations_this_turn = 0;
    runner.activate(ajani, minus_two).resolve();
    let both: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| {
            object.controller == P0
                && object.zone == Zone::Battlefield
                && object.name == "Ajani's Pridemate"
        })
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        both.len(),
        2,
        "two [-2] activations create two Pridemates; got {}",
        both.len()
    );
    assert!(
        both.contains(&first),
        "the first token survives the second creation"
    );

    let outcome = runner.cast(stream).x(1).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 1);
    for token in &both {
        assert_eq!(
            outcome.counters(*token, CounterType::Plus1Plus1),
            1,
            "each Pridemate gets exactly one counter (token {})",
            token.0
        );
    }
}

// ─── Row 3: [0] life-differential gate + mass exile ──────────────────────────

fn exile_board(scenario: &mut GameScenario) -> (ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let opp_creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let opp_artifact = scenario
        .add_artifact_from_oracle(P1, "Bauble", "{T}: You gain 1 life.")
        .id();
    let opp_enchantment = scenario
        .add_enchantment_from_oracle(P1, "Glorious Anthem", "Creatures you control get +1/+1.")
        .id();
    let own_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let own_artifact = scenario
        .add_artifact_from_oracle(P0, "Own Bauble", "{T}: You gain 1 life.")
        .id();
    (
        opp_creature,
        opp_artifact,
        opp_enchantment,
        own_creature,
        own_artifact,
    )
}

/// CR 608.2c: at exactly starting+15 the gate holds — Ajani and each artifact
/// and creature the opponents control are exiled; everything else stays.
#[test]
fn zero_at_exactly_fifteen_above_starting_exiles_ajani_and_opponent_artifacts_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 35);
    let ajani = ajani(&mut scenario, 5);
    let (opp_creature, opp_artifact, opp_enchantment, own_creature, own_artifact) =
        exile_board(&mut scenario);
    let mut runner = scenario.build();
    let zero = loyalty_index(&runner, ajani, 0);
    // The [0] costs no loyalty: Ajani holds its full 5 before activation.
    assert_eq!(
        runner.state().objects[&ajani].loyalty,
        Some(5),
        "Ajani starts at 5 loyalty"
    );

    let outcome = runner.activate(ajani, zero).resolve();

    outcome.assert_zone(&[ajani, opp_creature, opp_artifact], Zone::Exile);
    outcome.assert_zone(
        &[opp_enchantment, own_creature, own_artifact],
        Zone::Battlefield,
    );
}

/// Below the gate (starting+14) nothing moves; the declined activation still
/// consumed the once-per-turn loyalty allowance (CR 606.3), so a second [0]
/// attempt the same turn is refused — distinguishing gate-decline from a
/// failure to activate at all.
#[test]
fn zero_below_gate_exiles_nothing_and_spends_the_loyalty_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 34);
    let ajani = ajani(&mut scenario, 5);
    let (opp_creature, opp_artifact, opp_enchantment, own_creature, own_artifact) =
        exile_board(&mut scenario);
    let mut runner = scenario.build();
    let zero = loyalty_index(&runner, ajani, 0);

    let outcome = runner.activate(ajani, zero).resolve();

    outcome.assert_zone(
        &[
            ajani,
            opp_creature,
            opp_artifact,
            opp_enchantment,
            own_creature,
            own_artifact,
        ],
        Zone::Battlefield,
    );
    assert_eq!(
        runner.state().objects[&ajani].loyalty_activations_this_turn,
        1,
        "the gate-declined [0] still counts as this turn's loyalty activation"
    );
    // The public apply path refuses a second loyalty activation in the same
    // turn (the driver's try_resolve panics at announcement, so drive the
    // action directly and assert the rejection).
    let refused = runner.act(GameAction::ActivateAbility {
        source_id: ajani,
        ability_index: zero,
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        refused.is_err(),
        "a second [0] in the same turn is refused (CR 606.3 cap)"
    );
}

/// Three players: both opponents' artifacts and creatures are exiled while
/// each player's own non-matching permanents stay.
#[test]
fn zero_exiles_both_opponents_boards_in_three_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 35);
    let ajani = ajani(&mut scenario, 5);
    let p1_creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let p2_artifact = scenario
        .add_artifact_from_oracle(P2, "Bauble", "{T}: You gain 1 life.")
        .id();
    let p2_creature = scenario.add_creature(P2, "Grizzly Bears", 2, 2).id();
    let p1_enchantment = scenario
        .add_enchantment_from_oracle(P1, "Glorious Anthem", "Creatures you control get +1/+1.")
        .id();
    let mut runner = scenario.build();
    let zero = loyalty_index(&runner, ajani, 0);

    let outcome = runner.activate(ajani, zero).resolve();

    outcome.assert_zone(&[ajani, p1_creature, p2_artifact, p2_creature], Zone::Exile);
    outcome.assert_zone(&[p1_enchantment], Zone::Battlefield);
}

// ─── Row 4: [+1] computed life gain ──────────────────────────────────────────

/// CR 119.3: the [+1] gains life equal to creatures plus planeswalkers the
/// controller controls — Ajani itself counts as a planeswalker, the
/// opponent's creatures do not.
#[test]
fn plus_one_gains_creatures_plus_planeswalkers_controlled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = ajani(&mut scenario, 5);
    // A second planeswalker under the controller's control: the walker summand
    // must count every planeswalker, not just Ajani itself (cf.
    // bolas_dragon_god_grants.rs "Donor Walker" furniture).
    scenario
        .add_planeswalker_from_oracle(P0, "Donor Walker", "Test", 3, "[+1]: You gain 1 life.")
        .id();
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let mut runner = scenario.build();
    let plus_one = loyalty_index(&runner, ajani, 1);

    let outcome = runner.activate(ajani, plus_one).resolve();

    // 2 own creatures + Ajani itself + the donor walker; the opponent's 3
    // creatures add nothing.
    outcome.assert_life_delta(P0, 4);
    assert_eq!(
        runner.state().objects[&ajani].loyalty,
        Some(6),
        "[+1] adds one loyalty"
    );
}

/// Zero-of-a-type leg: with no other creatures, only Ajani itself (a
/// planeswalker the controller controls) is counted.
#[test]
fn plus_one_counts_ajani_itself_with_no_other_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ajani = ajani(&mut scenario, 5);
    let mut runner = scenario.build();
    let plus_one = loyalty_index(&runner, ajani, 1);

    let outcome = runner.activate(ajani, plus_one).resolve();

    outcome.assert_life_delta(P0, 1);
}
