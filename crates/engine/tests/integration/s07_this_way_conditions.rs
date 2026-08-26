//! S07 Batch 1 — "this way" tracked-set condition wiring (8 cards).
//!
//! Each card's condition-bearing Oracle body is parsed with the real
//! `parse_effect_chain` (which routes the leading "if …" through
//! `strip_if_you_do_conditional` → the hoisted/new active-voice this-way
//! combinators) and resolved through `resolve_ability_chain`, gating a
//! measurable payoff on the resolution-context set the parent effect would
//! publish. The set (`last_zone_changed_ids` / `tracked_object_sets` /
//! `last_effect_amount`) is seeded directly — that is the parent effect's job,
//! covered elsewhere — so the assertion isolates the parser wiring under test.
//!
//! DISCRIMINATION: revert the parser wiring and `strip_if_you_do_conditional`
//! yields `condition = None`, so the payoff fires *unconditionally*; every
//! negative-sibling assertion below (condition not met → no payoff) then flips.
//!
//! CR ANCHORS: CR 608.2c ("this way" scopes to the current resolution;
//! "if you control a …" battlefield-presence gate) + CR 400.7 (moved-object
//! reference) + CR 608.2c (tracked-set / previous-effect amount).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::card_type::CoreType;
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn life(runner: &GameRunner, p: PlayerId) -> i32 {
    runner.state().players[p.0 as usize].life
}
fn hand_len(runner: &GameRunner, p: PlayerId) -> usize {
    runner.state().players[p.0 as usize].hand.len()
}
fn resolve_body(runner: &mut GameRunner, body: &str, source: ObjectId) {
    let def = parse_effect_chain(body, AbilityKind::Spell);
    assert!(
        def.condition.is_some(),
        "parser must attach a condition for {body:?} — else the gate is vacuous"
    );
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 1).expect("chain resolves");
}
/// Seed the resolution-local "moved this way" set with `ids`.
fn seed_moved(runner: &mut GameRunner, ids: Vec<ObjectId>) {
    runner.state_mut().last_zone_changed_ids = ids;
}
fn set_core_types(runner: &mut GameRunner, id: ObjectId, types: Vec<CoreType>) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types = types;
}
fn add_bf_creature(
    scenario: &mut GameScenario,
    p: PlayerId,
    name: &str,
    subs: Vec<&str>,
) -> ObjectId {
    scenario
        .add_creature(p, name, 1, 1)
        .with_subtypes(subs)
        .id()
}

// ---------------------------------------------------------------------------
// Oviya — "If you put an artifact onto the battlefield this way, …"
//   ZoneChangedThisWay { Artifact }. Payoff proxied to a self-contained gain-4
//   (the printed payoff "put two +1/+1 counters on it" targets the moved object;
//   the condition — the parser change under test — is unchanged by the proxy).
// ---------------------------------------------------------------------------
const OVIYA: &str = "if you put an artifact onto the battlefield this way, you gain 4 life";

#[test]
fn oviya_gains_only_when_artifact_put() {
    let mut scenario = GameScenario::new();
    let art = add_bf_creature(&mut scenario, P0, "Servo", vec![]);
    let crea = add_bf_creature(&mut scenario, P0, "Bear", vec![]);
    let mut runner = scenario.build();
    set_core_types(&mut runner, art, vec![CoreType::Artifact]);
    // positive: an artifact moved this way
    let l0 = life(&runner, P0);
    seed_moved(&mut runner, vec![art]);
    resolve_body(&mut runner, OVIYA, art);
    assert_eq!(life(&runner, P0) - l0, 4, "artifact put this way → +4 life");
    // negative: only a (non-artifact) creature moved this way
    let l1 = life(&runner, P0);
    seed_moved(&mut runner, vec![crea]);
    resolve_body(&mut runner, OVIYA, art);
    assert_eq!(
        life(&runner, P0) - l1,
        0,
        "non-artifact → no bonus (REVERT flips this)"
    );
}

// ---------------------------------------------------------------------------
// Spelunking — "If you put a Cave onto the battlefield this way, you gain 4 life"
// ---------------------------------------------------------------------------
const SPELUNKING: &str = "if you put a cave onto the battlefield this way, you gain 4 life";

#[test]
fn spelunking_gains_only_when_cave_put() {
    let mut scenario = GameScenario::new();
    let cave = scenario
        .add_creature(P0, "Cave Land", 0, 0)
        .with_subtypes(vec!["Cave"])
        .id();
    let other = scenario
        .add_creature(P0, "Plains Land", 0, 0)
        .with_subtypes(vec!["Plains"])
        .id();
    let mut runner = scenario.build();
    let l0 = life(&runner, P0);
    seed_moved(&mut runner, vec![cave]);
    resolve_body(&mut runner, SPELUNKING, cave);
    assert_eq!(life(&runner, P0) - l0, 4, "Cave put this way → +4");
    let l1 = life(&runner, P0);
    seed_moved(&mut runner, vec![other]);
    resolve_body(&mut runner, SPELUNKING, cave);
    assert_eq!(life(&runner, P0) - l1, 0, "non-Cave → no bonus");
}

// ---------------------------------------------------------------------------
// Town Greeter — "If you put a Town card into your hand this way, you gain 2 life"
//   New put-into-hand combinator → ZoneChangedThisWay { Town }.
// ---------------------------------------------------------------------------
const TOWN_GREETER: &str = "if you put a town card into your hand this way, you gain 2 life";

#[test]
fn town_greeter_gains_only_when_town_put_to_hand() {
    let mut scenario = GameScenario::new();
    // Objects placed in hand to reflect the realistic post-put zone.
    let town = scenario
        .add_creature_to_hand(P0, "Town Card", 0, 0)
        .with_subtypes(vec!["Town"])
        .id();
    let plains = scenario
        .add_creature_to_hand(P0, "Plains", 0, 0)
        .with_subtypes(vec!["Plains"])
        .id();
    let mut runner = scenario.build();
    let l0 = life(&runner, P0);
    seed_moved(&mut runner, vec![town]);
    resolve_body(&mut runner, TOWN_GREETER, town);
    assert_eq!(life(&runner, P0) - l0, 2, "Town put to hand → +2");
    let l1 = life(&runner, P0);
    seed_moved(&mut runner, vec![plains]);
    resolve_body(&mut runner, TOWN_GREETER, town);
    assert_eq!(life(&runner, P0) - l1, 0, "non-Town → no bonus");
}

// ---------------------------------------------------------------------------
// Nashi — "If you put no cards into your hand this way, put a +1/+1 counter …"
//   QuantityCheck { TrackedSetSize == 0 }. Payoff proxied to gain-4 (the printed
//   counter targets Nashi; the count condition is the change under test).
// ---------------------------------------------------------------------------
const NASHI: &str = "if you put no cards into your hand this way, you gain 4 life";

fn seed_tracked(runner: &mut GameRunner, ids: Vec<ObjectId>) {
    runner.state_mut().tracked_object_sets.clear();
    runner
        .state_mut()
        .tracked_object_sets
        .insert(TrackedSetId(1), ids);
}

#[test]
fn nashi_bonus_only_when_no_cards_put() {
    let mut scenario = GameScenario::new();
    let nashi = add_bf_creature(&mut scenario, P0, "Nashi", vec![]);
    let card = scenario.add_creature_to_hand(P0, "Some Card", 0, 0).id();
    let mut runner = scenario.build();
    // positive: empty put-set → count 0 → bonus.
    let l0 = life(&runner, P0);
    seed_tracked(&mut runner, vec![]);
    resolve_body(&mut runner, NASHI, nashi);
    assert_eq!(life(&runner, P0) - l0, 4, "put no cards → bonus fires");
    // negative: one card put → count 1 → no bonus (REVERT: EQ0 gate absent → fires).
    let l1 = life(&runner, P0);
    seed_tracked(&mut runner, vec![card]);
    resolve_body(&mut runner, NASHI, nashi);
    assert_eq!(life(&runner, P0) - l1, 0, "put ≥1 card → no bonus");
}

// ---------------------------------------------------------------------------
// Arid Archway — "If another Desert was returned this way, …"
//   ZoneChangedThisWay { Subtype(Desert) + Another }. "another" excludes source.
// ---------------------------------------------------------------------------
const ARID: &str = "if another desert was returned this way, you gain 4 life";

#[test]
fn arid_archway_bonus_only_for_another_desert() {
    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature(P0, "Arid Archway", 0, 0)
        .with_subtypes(vec!["Desert"])
        .id();
    let other_desert = scenario
        .add_creature_to_hand(P0, "Other Desert", 0, 0)
        .with_subtypes(vec!["Desert"])
        .id();
    let non_desert = scenario
        .add_creature_to_hand(P0, "Forest", 0, 0)
        .with_subtypes(vec!["Forest"])
        .id();
    let mut runner = scenario.build();
    // positive: a DIFFERENT Desert returned this way.
    let l0 = life(&runner, P0);
    seed_moved(&mut runner, vec![other_desert]);
    resolve_body(&mut runner, ARID, source);
    assert_eq!(life(&runner, P0) - l0, 4, "another Desert returned → bonus");
    // negative A: a non-Desert returned.
    let l1 = life(&runner, P0);
    seed_moved(&mut runner, vec![non_desert]);
    resolve_body(&mut runner, ARID, source);
    assert_eq!(life(&runner, P0) - l1, 0, "non-Desert → no bonus");
    // negative B: the SOURCE Desert itself is excluded by "another".
    let l2 = life(&runner, P0);
    seed_moved(&mut runner, vec![source]);
    resolve_body(&mut runner, ARID, source);
    assert_eq!(
        life(&runner, P0) - l2,
        0,
        "source Desert excluded by 'another'"
    );
}

// ---------------------------------------------------------------------------
// Break the Spell — "If a permanent you controlled or a token was destroyed this
//   way, draw a card." ZoneChangedThisWay { Or([Permanent+You, Token]) }.
//   Seeds reflect the realistic post-destroy state (object in graveyard) to prove
//   the gate is not vacuous under lost-controller LKI.
// ---------------------------------------------------------------------------
const BREAK_SPELL: &str =
    "if a permanent you controlled or a token was destroyed this way, draw a card";

fn move_to_graveyard(runner: &mut GameRunner, id: ObjectId, owner: PlayerId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.zone = Zone::Graveyard;
    let _ = owner;
}

#[test]
fn break_the_spell_draws_for_controlled_or_token() {
    let mut scenario = GameScenario::new();
    // A permanent P0 controlled, an opponent's permanent, and a token, all created
    // on the battlefield so controller/token characteristics are set, then moved
    // to graveyard to reflect "was destroyed".
    let mine = add_bf_creature(&mut scenario, P0, "My Enchantment", vec![]);
    let theirs = add_bf_creature(&mut scenario, P1, "Their Enchantment", vec![]);
    scenario.with_library_top(P0, &["A", "B", "C", "D"]);
    let mut runner = scenario.build();
    set_core_types(&mut runner, mine, vec![CoreType::Enchantment]);
    set_core_types(&mut runner, theirs, vec![CoreType::Enchantment]);
    runner.state_mut().objects.get_mut(&mine).unwrap().is_token = true; // exercise token branch too

    // positive: my controlled permanent was destroyed → draw.
    let h0 = hand_len(&runner, P0);
    move_to_graveyard(&mut runner, mine, P0);
    seed_moved(&mut runner, vec![mine]);
    resolve_body(&mut runner, BREAK_SPELL, mine);
    assert_eq!(
        hand_len(&runner, P0) - h0,
        1,
        "controlled/token permanent destroyed → draw"
    );

    // negative: only an opponent's non-token permanent destroyed → no draw.
    move_to_graveyard(&mut runner, theirs, P1);
    seed_moved(&mut runner, vec![theirs]);
    let h1 = hand_len(&runner, P0);
    resolve_body(&mut runner, BREAK_SPELL, mine);
    assert_eq!(
        hand_len(&runner, P0) - h1,
        0,
        "opponent's non-token permanent → no draw"
    );
}

// ---------------------------------------------------------------------------
// Cache Grab — "If you control a Squirrel or returned a Squirrel card to your hand
//   this way, create a Food token." Or([ControllerControlsMatching{Squirrel},
//   ZoneChangedThisWay{Squirrel}]). Payoff proxied to gain-4 (Food count is
//   awkward to measure; the disjunctive condition is the change under test).
// ---------------------------------------------------------------------------
const CACHE_GRAB: &str =
    "if you control a squirrel or returned a squirrel card to your hand this way, you gain 4 life";

#[test]
fn cache_grab_disjunction_both_arms() {
    // Arm A: control a Squirrel on the battlefield, none returned.
    {
        let mut scenario = GameScenario::new();
        let sq = add_bf_creature(&mut scenario, P0, "Squirrel", vec!["Squirrel"]);
        let src = add_bf_creature(&mut scenario, P0, "Cache", vec![]);
        let _ = sq;
        let mut runner = scenario.build();
        let l0 = life(&runner, P0);
        seed_moved(&mut runner, vec![]); // nothing returned
        resolve_body(&mut runner, CACHE_GRAB, src);
        assert_eq!(
            life(&runner, P0) - l0,
            4,
            "control a Squirrel → bonus (control arm)"
        );
    }
    // Arm B: control none, but returned a Squirrel card to hand.
    {
        let mut scenario = GameScenario::new();
        let src = add_bf_creature(&mut scenario, P0, "Cache", vec![]);
        let returned = scenario
            .add_creature_to_hand(P0, "Squirrel Card", 1, 1)
            .with_subtypes(vec!["Squirrel"])
            .id();
        let mut runner = scenario.build();
        let l0 = life(&runner, P0);
        seed_moved(&mut runner, vec![returned]);
        resolve_body(&mut runner, CACHE_GRAB, src);
        assert_eq!(
            life(&runner, P0) - l0,
            4,
            "returned a Squirrel → bonus (returned arm)"
        );
    }
    // Arm C (negative): control none, returned a non-Squirrel.
    {
        let mut scenario = GameScenario::new();
        let src = add_bf_creature(&mut scenario, P0, "Cache", vec![]);
        let returned = scenario
            .add_creature_to_hand(P0, "Beast Card", 1, 1)
            .with_subtypes(vec!["Beast"])
            .id();
        let mut runner = scenario.build();
        let l0 = life(&runner, P0);
        seed_moved(&mut runner, vec![returned]);
        resolve_body(&mut runner, CACHE_GRAB, src);
        assert_eq!(
            life(&runner, P0) - l0,
            0,
            "neither arm → no bonus (REVERT flips)"
        );
    }
}

// ---------------------------------------------------------------------------
// Transcendent Archaic — "If you draw one or more cards this way, discard two cards"
//   QuantityCheck { PreviousEffectAmount >= 1 }. Payoff proxied to gain-4 (the
//   printed discard is interactive — the caster chooses cards — and pauses the
//   headless chain; the draw-count condition is the change under test).
// ---------------------------------------------------------------------------
const TRANSCENDENT: &str = "if you draw one or more cards this way, you gain 4 life";

#[test]
fn transcendent_bonus_only_when_drew() {
    let mut scenario = GameScenario::new();
    let src = add_bf_creature(&mut scenario, P0, "Transcendent Archaic", vec![]);
    let mut runner = scenario.build();

    // positive: drew >=1 this way (last_effect_amount = 2) → bonus.
    runner.state_mut().last_effect_amount = Some(2);
    let l0 = life(&runner, P0);
    resolve_body(&mut runner, TRANSCENDENT, src);
    assert_eq!(life(&runner, P0) - l0, 4, "drew ≥1 this way → bonus");

    // negative: drew 0 (may-draw declined) → no bonus.
    runner.state_mut().last_effect_amount = Some(0);
    let l1 = life(&runner, P0);
    resolve_body(&mut runner, TRANSCENDENT, src);
    assert_eq!(
        life(&runner, P0) - l1,
        0,
        "drew 0 → no bonus (REVERT: GE1 gate absent → fires)"
    );
}
