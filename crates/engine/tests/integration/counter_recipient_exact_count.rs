//! Phase 11 — counter-recipient exact count.
//!
//! CR 115.1 + CR 601.2c + CR 115.3: "put … counter(s) on each of <N|X> target
//! <objects>" announces exactly N different targets for its one instance of
//! "target" (X is chosen by the controller, CR 107.3a). On resolution each
//! declared object that is still a legal target gets the printed counters
//! (CR 122.6), a target that has become illegal gets none (CR 608.2b), and no
//! other object gets a counter. With X = 0 no object gets a counter.
//!
//! Every card here is staged from its verbatim Oracle text (MTGJSON
//! `AtomicCards.json`) through the from-Oracle builders, so each row runs the
//! parser under test:
//!
//! - Thrive ({X}{G}): "Put a +1/+1 counter on each of X target creatures."
//! - Argothian Uprooting ({X}{G}): "Put two +1/+1 counters on each of X target
//!   lands you control. They each become 0/0 Elemental creatures with reach,
//!   haste, and "When this creature leaves the battlefield, conjure a card named
//!   Forest onto the battlefield tapped." They're still lands."
//! - Rot-Curse Rakshasa ({1}{B}): "Trample\nDecayed (…)\nRenew — {X}{B}{B},
//!   Exile this card from your graveyard: Put a decayed counter on each of X
//!   target creatures. Activate only as a sorcery."
//! - Preservation neighbours: Travel Preparations ("each of up to two target"),
//!   Sweet-Gum Recluse ("each of any number of target"), Swelter (another verb's
//!   "each of two target").
//!
//! GREEN-AT-BASE LABELLING (phase 11 plan §13.1, §13.3): the `*_LEG` rows and
//! the three `preservation_*` rows are green at base; every other row is red at
//! base, where the placement exports no announced target-set spec and only the
//! first declared object gets counters (or, with X = 0, an undeclared object
//! gets one).

use engine::game::combat::{validate_blockers_for_player, AttackTarget};
use engine::game::engine::EngineError;
use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const THRIVE_ORACLE: &str = "Put a +1/+1 counter on each of X target creatures.";

const ARGOTHIAN_UPROOTING_ORACLE: &str = "Put two +1/+1 counters on each of X target lands you control. They each become 0/0 Elemental creatures with reach, haste, and \"When this creature leaves the battlefield, conjure a card named Forest onto the battlefield tapped.\" They're still lands.";

const ROT_CURSE_RAKSHASA_ORACLE: &str = "Trample\nDecayed (This creature can't block. When it attacks, sacrifice it at end of combat.)\nRenew \u{2014} {X}{B}{B}, Exile this card from your graveyard: Put a decayed counter on each of X target creatures. Activate only as a sorcery.";

const TRAVEL_PREPARATIONS_ORACLE: &str = "Put a +1/+1 counter on each of up to two target creatures.\nFlashback {1}{W} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

const SWEET_GUM_RECLUSE_ORACLE: &str = "Flash\nCascade\nReach\nWhen this creature enters, put three +1/+1 counters on each of any number of target creatures that entered this turn.";

const SWELTER_ORACLE: &str = "Swelter deals 2 damage to each of two target creatures.";

const FOREST_ORACLE: &str = "({T}: Add {G}.)";

const DECAYED_COUNTER: CounterType = CounterType::Keyword(KeywordKind::Decayed);

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

fn x_green_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Green],
        generic: 0,
    }
}

fn add_thrive(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Thrive", false, THRIVE_ORACLE)
        .with_mana_cost(x_green_cost())
        .id()
}

fn add_argothian_uprooting(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Argothian Uprooting", false, ARGOTHIAN_UPROOTING_ORACLE)
        .with_mana_cost(x_green_cost())
        .id()
}

fn add_rakshasa_to_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Rot-Curse Rakshasa", 5, 5)
        .with_subtypes(vec!["Demon"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .from_oracle_text(ROT_CURSE_RAKSHASA_ORACLE)
        .id()
}

fn add_forest(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_land_from_oracle(player, "Forest", FOREST_ORACLE)
        .with_subtypes(vec!["Forest"])
        .id()
}

/// Answer `ChooseXValue` with `x` whenever it is prompted (its position relative
/// to the target prompt is not asserted), then return the current target slot's
/// offered objects. Panics if no target prompt is reached.
fn choose_x_then_offered(runner: &mut GameRunner, x: u32) -> Vec<TargetRef> {
    while matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }) {
        runner
            .act(GameAction::ChooseX { value: x })
            .expect("announcing X must be accepted");
    }
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => target_slots[selection.current_slot].legal_targets.clone(),
        other => panic!("expected a target prompt, got {other:?}"),
    }
}

fn cast_from_hand(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("casting must be accepted");
}

/// Pass priority until the stack is empty.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance resolution");
    }
    panic!("the stack did not empty: {:?}", runner.state().stack);
}

/// Every object in `player`'s graveyard has no counters (CR 400.7: an object
/// moved there is a new object).
fn assert_graveyard_counterless(runner: &GameRunner, player: PlayerId) {
    let state = runner.state();
    let graveyard = &state.players[player.0 as usize].graveyard;
    assert!(
        !graveyard.is_empty(),
        "reach: the moved target must be in the graveyard"
    );
    for id in graveyard {
        assert!(
            state.objects[id].counters.is_empty(),
            "CR 400.7: the moved target's new object must have no counters, got {:?}",
            state.objects[id].counters
        );
    }
}

/// The land became the 2/2 Land Creature — Forest Elemental with reach and haste
/// that Argothian Uprooting's later instructions make of a declared land with
/// its two counters (the phase-11 coupled record; CR 611.2c, CR 205.1b).
fn assert_uprooted(runner: &GameRunner, land: ObjectId, name: &str) {
    let obj = &runner.state().objects[&land];
    assert_eq!(
        counters(runner, land, CounterType::Plus1Plus1),
        2,
        "{name}: two +1/+1 counters"
    );
    assert!(
        obj.card_types.core_types.contains(&CoreType::Land)
            && obj.card_types.core_types.contains(&CoreType::Creature),
        "{name}: a Land Creature, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        ["Forest", "Elemental"]
            .iter()
            .all(|sub| obj.card_types.subtypes.iter().any(|s| s == sub)),
        "{name}: Forest Elemental, got {:?}",
        obj.card_types.subtypes
    );
    assert!(
        has_keyword(obj, &Keyword::Reach) && has_keyword(obj, &Keyword::Haste),
        "{name}: reach and haste, got {:?}",
        obj.keywords
    );
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2), Some(2)),
        "{name}: 2/2"
    );
}

fn assert_untouched_land(runner: &GameRunner, land: ObjectId, name: &str) {
    let obj = &runner.state().objects[&land];
    assert!(
        obj.counters.is_empty(),
        "{name} was not declared: no counters, got {:?}",
        obj.counters
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Creature),
        "{name} was not declared: not a creature"
    );
}

/// Q2-THR-2. Thrive, X = 2: both declared creatures get one +1/+1 counter; the
/// undeclared creature, the opponent's creature and the source get none.
/// RED AT BASE (only c1 gets a counter).
#[test]
fn thrive_x2_counters_each_declared_creature_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P0, 2, 2);
    let c2 = scenario.add_vanilla(P0, 2, 2);
    let c3 = scenario.add_vanilla(P0, 2, 2);
    let opp = scenario.add_vanilla(P1, 2, 2);
    let thrive = add_thrive(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Green));
    let mut runner = scenario.build();

    runner
        .cast(thrive)
        .x(2)
        .target_objects(&[c1, c2])
        .try_resolve()
        .expect("Thrive with X = 2 must be cast and resolve");

    for (id, name) in [(c1, "c1"), (c2, "c2")] {
        assert_eq!(
            counters(&runner, id, CounterType::Plus1Plus1),
            1,
            "{name} was declared and must get one +1/+1 counter"
        );
    }
    for (id, name) in [(c3, "c3"), (opp, "opponent creature"), (thrive, "Thrive")] {
        assert_eq!(
            counters(&runner, id, CounterType::Plus1Plus1),
            0,
            "{name} was not declared and must get no counter"
        );
    }
}

/// Q2-THR-3. Thrive, X = 3, one declared creature an opponent's: all three get
/// a counter, the undeclared c4 none. RED AT BASE (c1 only).
#[test]
fn thrive_x3_counters_all_three_declared_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P0, 2, 2);
    let c2 = scenario.add_vanilla(P0, 2, 2);
    let c3 = scenario.add_vanilla(P1, 2, 2);
    let c4 = scenario.add_vanilla(P0, 2, 2);
    let thrive = add_thrive(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Green));
    let mut runner = scenario.build();

    runner
        .cast(thrive)
        .x(3)
        .target_objects(&[c1, c2, c3])
        .try_resolve()
        .expect("Thrive with X = 3 must be cast and resolve");

    for (id, name) in [(c1, "c1"), (c2, "c2"), (c3, "opponent's c3")] {
        assert_eq!(
            counters(&runner, id, CounterType::Plus1Plus1),
            1,
            "{name} was declared"
        );
    }
    assert_eq!(
        counters(&runner, c4, CounterType::Plus1Plus1),
        0,
        "c4 was not declared"
    );
}

/// Q2-THR-0 (C-3). Thrive, X = 0, beside one creature: no target is declared,
/// so no object gets a counter. RED AT BASE (the creature gets a counter).
#[test]
fn thrive_x0_places_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P0, 2, 2);
    let thrive = add_thrive(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Green));
    let mut runner = scenario.build();

    runner
        .cast(thrive)
        .x(0)
        .try_resolve()
        .expect("reach: Thrive with X = 0 must be cast and resolve");

    assert_eq!(
        counters(&runner, c1, CounterType::Plus1Plus1),
        0,
        "X = 0 declares no target, so no creature gets a counter"
    );
}

/// Q2-THR-ILL (CR 608.2b). Thrive, X = 2; the first declared target leaves the
/// battlefield before resolution, and the other declared target still gets its
/// counter. Reach guard: Q2-THR-2. RED AT BASE (c2 gets none).
#[test]
fn thrive_first_declared_target_illegal_still_counters_the_other() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P0, 2, 2);
    let c2 = scenario.add_vanilla(P0, 2, 2);
    let c3 = scenario.add_vanilla(P0, 2, 2);
    let thrive = add_thrive(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Green));
    let mut runner = scenario.build();

    let mut commit = runner.cast(thrive).x(2).target_objects(&[c1, c2]).commit();
    let mut events = Vec::new();
    move_to_zone(commit.state_mut(), c1, Zone::Graveyard, &mut events);
    commit
        .try_resolve()
        .expect("Thrive must resolve with one legal target left");

    assert_eq!(
        counters(&runner, c2, CounterType::Plus1Plus1),
        1,
        "CR 608.2b: c2 is still a legal target and gets its counter"
    );
    assert_eq!(
        counters(&runner, c3, CounterType::Plus1Plus1),
        0,
        "c3 was not declared"
    );
    // The graveyard holds c1's new object and the resolved Thrive.
    assert_graveyard_counterless(&runner, P0);
}

/// Q2-DUP (CR 115.3). Thrive, X = 2, the same creature submitted for both
/// slots: the second `ChooseTarget` is refused. Reach guard: Q2-THR-2.
/// RED AT BASE (one slot, so the repeat is never asked for and the cast
/// resolves).
#[test]
fn thrive_refuses_the_same_creature_for_two_slots() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P0, 2, 2);
    let _c2 = scenario.add_vanilla(P0, 2, 2);
    let thrive = add_thrive(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Green));
    let mut runner = scenario.build();

    let result = runner
        .cast(thrive)
        .x(2)
        .target_objects(&[c1, c1])
        .try_resolve();

    assert!(
        matches!(result, Err(EngineError::InvalidAction(_))),
        "CR 115.3: the repeated target must be refused, got {:?}",
        result.map(|_| ())
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "the second slot must still be waiting for a target, got {:?}",
        runner.state().waiting_for
    );
}

/// Q2-THR-LEG (C-4). Thrive, X = 1: a Forest is not offered and submitting it is
/// refused; submitting a creature in the same slot is accepted. GREEN AT BASE
/// (the slot's creature filter is unchanged by phase 11).
#[test]
fn thrive_offers_creatures_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P0, 2, 2);
    let _c2 = scenario.add_vanilla(P0, 2, 2);
    let land = add_forest(&mut scenario, P0);
    let thrive = add_thrive(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Green));
    let mut runner = scenario.build();

    cast_from_hand(&mut runner, thrive);
    let offered = choose_x_then_offered(&mut runner, 1);

    assert!(
        offered.contains(&TargetRef::Object(c1)),
        "reach: c1 is offered: {offered:?}"
    );
    assert!(
        !offered.contains(&TargetRef::Object(land)),
        "C-4: the Forest is not offered: {offered:?}"
    );
    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(land))
            })
            .is_err(),
        "C-4: choosing the Forest must be refused"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(c1)),
        })
        .expect("choosing c1 in the same slot must be accepted");
}

/// Q2-ARG-2. Argothian Uprooting, X = 2: both declared Forests get two +1/+1
/// counters and, per the coupled record, become 2/2 Forest Elemental Land
/// Creatures with reach and haste; the undeclared Forest, the opponent's Forest
/// and the source are unchanged. RED AT BASE (F2 unchanged).
#[test]
fn argothian_uprooting_x2_uproots_each_declared_land_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let f1 = add_forest(&mut scenario, P0);
    let f2 = add_forest(&mut scenario, P0);
    let f3 = add_forest(&mut scenario, P0);
    let opp = add_forest(&mut scenario, P1);
    let spell = add_argothian_uprooting(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Green));
    let mut runner = scenario.build();

    runner
        .cast(spell)
        .x(2)
        .target_objects(&[f1, f2])
        .try_resolve()
        .expect("Argothian Uprooting with X = 2 must be cast and resolve");

    assert_uprooted(&runner, f1, "F1");
    assert_uprooted(&runner, f2, "F2");
    assert_untouched_land(&runner, f3, "F3");
    assert_untouched_land(&runner, opp, "the opponent's Forest");
    assert_eq!(
        counters(&runner, spell, CounterType::Plus1Plus1),
        0,
        "the source gets none"
    );
}

/// Q2-ARG-0 (C-3). Argothian Uprooting, X = 0, beside one Forest: no land gets a
/// counter or changes type. RED AT BASE (F1 gets two counters and becomes a
/// creature).
#[test]
fn argothian_uprooting_x0_changes_no_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let f1 = add_forest(&mut scenario, P0);
    let spell = add_argothian_uprooting(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Green));
    let mut runner = scenario.build();

    runner
        .cast(spell)
        .x(0)
        .try_resolve()
        .expect("reach: Argothian Uprooting with X = 0 must be cast and resolve");

    assert_untouched_land(&runner, f1, "F1");
}

/// Q2-ARG-ILL (CR 608.2b). Argothian Uprooting, X = 2; the first declared Forest
/// leaves the battlefield before resolution, and the other declared Forest is
/// still uprooted. Reach guard: Q2-ARG-2. RED AT BASE (F2 unchanged).
#[test]
fn argothian_uprooting_first_declared_land_illegal_still_uproots_the_other() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let f1 = add_forest(&mut scenario, P0);
    let f2 = add_forest(&mut scenario, P0);
    let f3 = add_forest(&mut scenario, P0);
    let spell = add_argothian_uprooting(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Green));
    let mut runner = scenario.build();

    let mut commit = runner.cast(spell).x(2).target_objects(&[f1, f2]).commit();
    let mut events = Vec::new();
    move_to_zone(commit.state_mut(), f1, Zone::Graveyard, &mut events);
    commit
        .try_resolve()
        .expect("Argothian Uprooting must resolve with one legal target left");

    assert_uprooted(&runner, f2, "F2");
    assert_untouched_land(&runner, f3, "F3");
    assert_graveyard_counterless(&runner, P0);
}

/// Q2-ARG-LEG (C-4). Argothian Uprooting, X = 1, two own Forests and an
/// opponent's: the opponent's Forest is not offered and submitting it is
/// refused; an own Forest is accepted in the same slot. GREEN AT BASE (the
/// slot's "you control" filter is unchanged by phase 11).
#[test]
fn argothian_uprooting_offers_lands_you_control_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = add_forest(&mut scenario, P0);
    let _own2 = add_forest(&mut scenario, P0);
    let opp = add_forest(&mut scenario, P1);
    let spell = add_argothian_uprooting(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Green));
    let mut runner = scenario.build();

    cast_from_hand(&mut runner, spell);
    let offered = choose_x_then_offered(&mut runner, 1);

    assert!(
        offered.contains(&TargetRef::Object(own)),
        "reach: an own Forest is offered: {offered:?}"
    );
    assert!(
        !offered.contains(&TargetRef::Object(opp)),
        "C-4: the opponent's Forest is not offered: {offered:?}"
    );
    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(opp))
            })
            .is_err(),
        "C-4: choosing the opponent's Forest must be refused"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(own)),
        })
        .expect("choosing an own Forest in the same slot must be accepted");
}

/// Q2-RAK-2. Rot-Curse Rakshasa's renew from the graveyard at sorcery speed,
/// X = 2: both declared creatures get a decayed counter and so gain decayed
/// (CR 122.1b); the undeclared creature gets none, and the source is exiled as
/// the cost. RED AT BASE (c2 gets none).
#[test]
fn rot_curse_rakshasa_renew_x2_decays_each_declared_creature_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P1, 2, 2);
    let c2 = scenario.add_vanilla(P1, 2, 2);
    let c3 = scenario.add_vanilla(P1, 2, 2);
    let rakshasa = add_rakshasa_to_graveyard(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Black));
    let mut runner = scenario.build();

    runner
        .activate(rakshasa, 0)
        .x(2)
        .target_objects(&[c1, c2])
        .resolve();

    assert_eq!(
        runner.state().objects[&rakshasa].zone,
        Zone::Exile,
        "reach: renew exiled the source"
    );
    for (id, name) in [(c1, "c1"), (c2, "c2")] {
        assert_eq!(
            counters(&runner, id, DECAYED_COUNTER),
            1,
            "{name} was declared"
        );
        assert!(
            has_keyword(&runner.state().objects[&id], &Keyword::Decayed),
            "CR 122.1b: {name}'s decayed counter gives it decayed"
        );
    }
    assert_eq!(
        counters(&runner, c3, DECAYED_COUNTER),
        0,
        "c3 was not declared"
    );
}

/// Q2-RAK-0 (C-3). Rot-Curse Rakshasa's renew, X = 0, beside one creature: no
/// creature gets a decayed counter. RED AT BASE (the creature gets one).
#[test]
fn rot_curse_rakshasa_renew_x0_decays_no_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P1, 2, 2);
    let rakshasa = add_rakshasa_to_graveyard(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Black));
    let mut runner = scenario.build();

    runner.activate(rakshasa, 0).x(0).resolve();

    assert_eq!(
        runner.state().objects[&rakshasa].zone,
        Zone::Exile,
        "reach: renew exiled the source"
    );
    assert_eq!(
        counters(&runner, c1, DECAYED_COUNTER),
        0,
        "X = 0 declares no target"
    );
}

/// Q2-RAK-ILL (CR 608.2b). Rot-Curse Rakshasa's renew, X = 2, driven one
/// `ChooseTarget` per offered slot (c1, then c2); c1 leaves the battlefield
/// before resolution, and c2 still gets its decayed counter. Reach guard:
/// Q2-RAK-2. RED AT BASE (one slot, so c2 is never declared).
#[test]
fn rot_curse_rakshasa_first_declared_target_illegal_still_decays_the_other() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P1, 2, 2);
    let c2 = scenario.add_vanilla(P1, 2, 2);
    let c3 = scenario.add_vanilla(P1, 2, 2);
    let rakshasa = add_rakshasa_to_graveyard(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Black));
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: rakshasa,
            ability_index: 0,
        })
        .expect("renew must be activatable from the graveyard at sorcery speed");
    choose_x_then_offered(&mut runner, 2);
    for target in [c1, c2] {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .expect("declaring an offered creature must be accepted");
        }
    }
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), c1, Zone::Graveyard, &mut events);
    drain_stack(&mut runner);

    assert_eq!(
        counters(&runner, c2, DECAYED_COUNTER),
        1,
        "CR 608.2b: c2 is still a legal target and gets its decayed counter"
    );
    assert_eq!(
        counters(&runner, c3, DECAYED_COUNTER),
        0,
        "c3 was not declared"
    );
    assert_graveyard_counterless(&runner, P1);
}

/// Q2-RAK-LEG (C-4). Rot-Curse Rakshasa's renew, X = 1, two creatures and a
/// Forest: the Forest is not offered and submitting it is refused; a creature is
/// accepted in the same slot. GREEN AT BASE (the slot's creature filter is
/// unchanged by phase 11).
#[test]
fn rot_curse_rakshasa_renew_offers_creatures_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P1, 2, 2);
    let _c2 = scenario.add_vanilla(P1, 2, 2);
    let land = add_forest(&mut scenario, P1);
    let rakshasa = add_rakshasa_to_graveyard(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Black));
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: rakshasa,
            ability_index: 0,
        })
        .expect("renew must be activatable from the graveyard at sorcery speed");
    let offered = choose_x_then_offered(&mut runner, 1);

    assert!(
        offered.contains(&TargetRef::Object(c1)),
        "reach: c1 is offered: {offered:?}"
    );
    assert!(
        !offered.contains(&TargetRef::Object(land)),
        "C-4: the Forest is not offered: {offered:?}"
    );
    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(land))
            })
            .is_err(),
        "C-4: choosing the Forest must be refused"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(c1)),
        })
        .expect("choosing c1 in the same slot must be accepted");
}

/// Q2-RAK-BLK (CR 702.147a). After renew with X = 2, neither declared creature
/// can block; the undeclared creature can. RED AT BASE (c2 can block).
#[test]
fn rot_curse_rakshasa_decayed_creatures_cannot_block() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_vanilla(P0, 1, 1);
    let c1 = scenario.add_vanilla(P1, 2, 2);
    let c2 = scenario.add_vanilla(P1, 2, 2);
    let c3 = scenario.add_vanilla(P1, 2, 2);
    let rakshasa = add_rakshasa_to_graveyard(&mut scenario);
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Black));
    let mut runner = scenario.build();

    runner
        .activate(rakshasa, 0)
        .x(2)
        .target_objects(&[c1, c2])
        .resolve();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the attack must be declared");

    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(c3, attacker)]).is_ok(),
        "reach: the undeclared c3 can block"
    );
    for (id, name) in [(c1, "c1"), (c2, "c2")] {
        assert!(
            validate_blockers_for_player(runner.state(), P1, &[(id, attacker)]).is_err(),
            "CR 702.147a: the decayed {name} can't block"
        );
    }
}

/// Q6-A. Travel Preparations ("each of up to two target creatures"), one
/// declared: that creature gets its counter, the other none. GREEN AT BASE (the
/// "up to" arm of the same recovery).
#[test]
fn preservation_travel_preparations_one_declared_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P0, 2, 2);
    let c2 = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Travel Preparations", false, TRAVEL_PREPARATIONS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Green));
    let mut runner = scenario.build();

    runner
        .cast(spell)
        .target_objects(&[c1])
        .try_resolve()
        .expect("Travel Preparations with one target must be cast and resolve");

    assert_eq!(
        counters(&runner, c1, CounterType::Plus1Plus1),
        1,
        "c1 was declared"
    );
    assert_eq!(
        counters(&runner, c2, CounterType::Plus1Plus1),
        0,
        "c2 was not declared"
    );
}

/// Q6-B. Sweet-Gum Recluse ("each of any number of target creatures that entered
/// this turn"), two declared: both get three +1/+1 counters. GREEN AT BASE (the
/// "any number" arm of the same recovery).
#[test]
fn preservation_sweet_gum_recluse_two_declared_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fresh_a = scenario
        .add_creature(P0, "Fresh Bear A", 2, 2)
        .with_summoning_sickness()
        .id();
    let fresh_b = scenario
        .add_creature(P0, "Fresh Bear B", 2, 2)
        .with_summoning_sickness()
        .id();
    let recluse = scenario
        .add_creature_to_hand_from_oracle(P0, "Sweet-Gum Recluse", 0, 3, SWEET_GUM_RECLUSE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 4,
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(6, ManaType::Green));
    let mut runner = scenario.build();

    runner.cast(recluse).commit();
    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => break,
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance the Recluse's resolution");
            }
            ref other => panic!("expected the Recluse's ETB target prompt, got {other:?}"),
        }
    }
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(fresh_a), TargetRef::Object(fresh_b)],
        })
        .expect("declaring both fresh creatures must be accepted");
    drain_stack(&mut runner);

    for (id, name) in [(fresh_a, "fresh_a"), (fresh_b, "fresh_b")] {
        assert_eq!(
            counters(&runner, id, CounterType::Plus1Plus1),
            3,
            "{name} was declared"
        );
    }
}

/// Q6-C. Swelter ("deals 2 damage to each of two target creatures"), two
/// declared: each takes 2 damage, the third creature none. GREEN AT BASE (the
/// DealDamage exact-count recovery; no mutation through phase 11's site can
/// reach this verb).
#[test]
fn preservation_swelter_damages_each_declared_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_vanilla(P1, 3, 3);
    let c2 = scenario.add_vanilla(P1, 3, 3);
    let c3 = scenario.add_vanilla(P1, 3, 3);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swelter", false, SWELTER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Red));
    let mut runner = scenario.build();

    runner
        .cast(spell)
        .target_objects(&[c1, c2])
        .try_resolve()
        .expect("Swelter with two targets must be cast and resolve");

    let damage = |id: ObjectId| runner.state().objects[&id].damage_marked;
    assert_eq!(
        (damage(c1), damage(c2)),
        (2, 2),
        "each declared creature takes 2 damage"
    );
    assert_eq!(damage(c3), 0, "c3 was not declared");
}
