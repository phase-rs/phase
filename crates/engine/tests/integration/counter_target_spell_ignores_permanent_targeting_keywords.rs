//! Hexproof, shroud and protection restrict PERMANENTS (and players), never a
//! spell on the stack or a card in a graveyard. CR 113.6: abilities of an
//! object other than an instant or sorcery usually function only while it is
//! on the battlefield, and CR 702.11b / CR 702.16b / CR 702.18a word each of
//! these keywords for "this permanent" (or "permanent or player").
//!
//! Field report: an opponent cast Narset, Enlightened Master (first strike,
//! hexproof) from the command zone and the player holding Counterspell had no
//! legal action but passing priority. Target enumeration ran every spell on the
//! stack through the battlefield targeting check, so every hexproof, shroud or
//! protection creature spell had no legal "target spell". The same check, in
//! its protection-only form, also kept graveyard recursion off a protection
//! creature card.
//!
//! Every card below is built from its Oracle text and driven through the real
//! casting, targeting and resolution pipeline.

use engine::ai_support::legal_actions;
use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COUNTERSPELL: &str = "Counter target spell.";
const ESSENCE_SCATTER: &str = "Counter target creature spell.";
const DISALLOW: &str = "Counter target spell, activated ability, or triggered ability. \
     (Mana abilities can't be targeted.)";
const MURDER: &str = "Destroy target creature.";
const UNEARTH: &str = "Return target creature card with mana value 3 or less from your graveyard to the battlefield.\n\
     Cycling {2} ({2}, Discard this card: Draw a card.)";

const NARSET: &str = "First strike, hexproof\n\
     Whenever Narset, Enlightened Master attacks, exile the top four cards of your library. \
     Until end of turn, you may cast noncreature spells from among cards exiled with \
     Narset, Enlightened Master this turn without paying their mana costs.";
const NACATL_OUTLANDER: &str = "Protection from blue";
const SCRAGNOTH: &str = "This spell can't be countered.\nProtection from blue";
const ARGOTHIAN_ENCHANTRESS: &str = "Shroud\nWhenever you cast an enchantment spell, draw a card.";
const MIRRAN_CRUSADER: &str = "Double strike, protection from black and from green";

fn blue_blue() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    }
}

fn cast(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|err| panic!("cast {object_id:?}: {err:?}"));
}

fn offers_cast_of(runner: &GameRunner, object_id: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id: id, .. } if *id == object_id),
    )
}

fn stack_targets_of(runner: &GameRunner, spell: ObjectId) -> Vec<TargetRef> {
    runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .map(|ability| ability.targets.clone())
        .unwrap_or_default()
}

/// Both players pass priority until the stack is empty.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..32 {
        if runner.state().stack.is_empty() {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected decision while draining the stack: {other:?}"),
        }
    }
    panic!("stack did not empty: {:?}", runner.state().stack);
}

/// P0 casts `creature` (no cost), passes priority, and P1 (holding `counter`)
/// must be offered the cast, which auto-targets the creature spell, the only
/// spell on the stack.
fn opponent_counters(runner: &mut GameRunner, creature: ObjectId, counter: ObjectId) {
    cast(runner, creature);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 must hold priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        offers_cast_of(runner, counter),
        "P1 must be offered the counterspell against the creature spell; legal actions: {:?}",
        legal_actions(runner.state())
    );
    cast(runner, counter);
    assert_eq!(
        stack_targets_of(runner, counter),
        vec![TargetRef::Object(creature)],
        "the counterspell targets the creature spell"
    );
}

/// CR 702.11b + CR 113.6 + CR 701.6a: hexproof on a creature spell does not
/// stop an opponent's Counterspell, and the spell is countered.
#[test]
fn counterspell_counters_a_hexproof_creature_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let narset = scenario
        .add_creature_to_hand(P0, "Narset, Enlightened Master", 3, 2)
        .from_oracle_text_with_keywords(&["First strike", "Hexproof"], NARSET)
        .id();
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(blue_blue())
        .id();
    let mut runner = scenario.build();
    assert!(
        has_keyword(&runner.state().objects[&narset], &Keyword::Hexproof),
        "Narset's Oracle text gives her hexproof"
    );

    opponent_counters(&mut runner, narset, counterspell);
    assert!(
        has_keyword(&runner.state().objects[&narset], &Keyword::Hexproof),
        "the targeted spell still has hexproof on the stack"
    );
    drain_stack(&mut runner);

    assert_eq!(runner.state().objects[&narset].zone, Zone::Graveyard);
    assert!(!runner.state().battlefield.contains(&narset));
    assert_eq!(runner.state().objects[&counterspell].zone, Zone::Graveyard);
}

/// CR 702.18a + CR 113.6 + CR 701.6a: shroud on a creature spell does not stop
/// Counterspell either.
#[test]
fn counterspell_counters_a_shroud_creature_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let enchantress = scenario
        .add_creature_to_hand(P0, "Argothian Enchantress", 0, 1)
        .from_oracle_text_with_keywords(&["Shroud"], ARGOTHIAN_ENCHANTRESS)
        .id();
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(blue_blue())
        .id();
    let mut runner = scenario.build();
    assert!(has_keyword(
        &runner.state().objects[&enchantress],
        &Keyword::Shroud
    ));

    opponent_counters(&mut runner, enchantress, counterspell);
    drain_stack(&mut runner);

    assert_eq!(runner.state().objects[&enchantress].zone, Zone::Graveyard);
    assert!(!runner.state().battlefield.contains(&enchantress));
}

/// CR 702.16b + CR 113.6 + CR 701.6a: protection from blue on a creature spell
/// does not stop a blue Counterspell from targeting it, and it is countered.
#[test]
fn counterspell_counters_a_protection_from_blue_creature_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let outlander = scenario
        .add_creature_to_hand(P0, "Nacatl Outlander", 2, 2)
        .from_oracle_text_with_keywords(&["Protection"], NACATL_OUTLANDER)
        .id();
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(blue_blue())
        .id();
    let mut runner = scenario.build();
    assert!(
        has_keyword(
            &runner.state().objects[&outlander],
            &Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue))
        ),
        "Nacatl Outlander's Oracle text gives it protection from blue"
    );
    assert!(runner.state().objects[&counterspell]
        .color
        .contains(&ManaColor::Blue));

    opponent_counters(&mut runner, outlander, counterspell);
    drain_stack(&mut runner);

    assert_eq!(runner.state().objects[&outlander].zone, Zone::Graveyard);
    assert!(!runner.state().battlefield.contains(&outlander));
}

/// CR 702.16b + CR 113.6 + CR 701.6a: Disallow's "target spell, activated
/// ability, or triggered ability" names the stack zone explicitly, a separate
/// enumeration path from Counterspell's "target spell". Protection from blue
/// does not stop it either.
#[test]
fn disallow_counters_a_protection_from_blue_creature_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let outlander = scenario
        .add_creature_to_hand(P0, "Nacatl Outlander", 2, 2)
        .from_oracle_text_with_keywords(&["Protection"], NACATL_OUTLANDER)
        .id();
    let disallow = scenario
        .add_spell_to_hand_from_oracle(P1, "Disallow", true, DISALLOW)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    assert!(has_keyword(
        &runner.state().objects[&outlander],
        &Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue))
    ));

    opponent_counters(&mut runner, outlander, disallow);
    drain_stack(&mut runner);

    assert_eq!(runner.state().objects[&outlander].zone, Zone::Graveyard);
    assert!(!runner.state().battlefield.contains(&outlander));
}

/// CR 702.16b + CR 113.6: protection from blue does not stop a blue "counter
/// target creature spell" from targeting Scragnoth on the stack. CR 101.2:
/// "This spell can't be countered" still wins at resolution, so the target is
/// legal and Scragnoth resolves anyway.
#[test]
fn essence_scatter_can_target_scragnoth_but_cannot_counter_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let scragnoth = scenario
        .add_creature_to_hand(P0, "Scragnoth", 3, 4)
        .from_oracle_text_with_keywords(&["Protection"], SCRAGNOTH)
        .id();
    let essence_scatter = scenario
        .add_spell_to_hand_from_oracle(P1, "Essence Scatter", true, ESSENCE_SCATTER)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    assert!(has_keyword(
        &runner.state().objects[&scragnoth],
        &Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue))
    ));

    opponent_counters(&mut runner, scragnoth, essence_scatter);
    drain_stack(&mut runner);

    assert_eq!(
        runner.state().objects[&essence_scatter].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&scragnoth].zone,
        Zone::Battlefield,
        "Scragnoth can't be countered, so it resolves"
    );
}

/// CR 702.16b + CR 113.6: protection from black on a creature CARD in a
/// graveyard does not stop a black spell from targeting it.
#[test]
fn unearth_returns_a_protection_from_black_creature_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Black);
    let crusader = scenario
        .add_creature_to_graveyard(P0, "Mirran Crusader", 2, 2)
        .from_oracle_text_with_keywords(&["Double strike", "Protection"], MIRRAN_CRUSADER)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    let unearth = scenario
        .add_spell_to_hand_from_oracle(P0, "Unearth", false, UNEARTH)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let mut runner = scenario.build();
    assert!(
        has_keyword(
            &runner.state().objects[&crusader],
            &Keyword::Protection(ProtectionTarget::Color(ManaColor::Black))
        ),
        "Mirran Crusader's Oracle text gives it protection from black"
    );

    assert!(
        offers_cast_of(&runner, unearth),
        "Unearth must be castable with Mirran Crusader in the graveyard; legal actions: {:?}",
        legal_actions(runner.state())
    );
    cast(&mut runner, unearth);
    assert_eq!(
        stack_targets_of(&runner, unearth),
        vec![TargetRef::Object(crusader)]
    );
    drain_stack(&mut runner);

    assert_eq!(runner.state().objects[&crusader].zone, Zone::Battlefield);
}

/// Control: on the battlefield the same keywords still apply (CR 702.11b,
/// CR 702.16b). An opponent's Murder can't target Narset (hexproof) or Mirran
/// Crusader (protection from black); a third creature makes Murder castable
/// again, and still only that creature is a legal target.
#[test]
fn hexproof_and_protection_permanents_stay_untargetable() {
    fn board(with_bear: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P1, ManaColor::Black);
        scenario.add_basic_land(P1, ManaColor::Black);
        scenario.add_basic_land(P1, ManaColor::Black);
        let narset = scenario
            .add_creature(P0, "Narset, Enlightened Master", 3, 2)
            .from_oracle_text_with_keywords(&["First strike", "Hexproof"], NARSET)
            .id();
        scenario
            .add_creature(P0, "Mirran Crusader", 2, 2)
            .from_oracle_text_with_keywords(&["Double strike", "Protection"], MIRRAN_CRUSADER);
        let bear = with_bear.then(|| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id());
        let murder = scenario
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            })
            .id();
        let mut runner = scenario.build();
        assert!(has_keyword(
            &runner.state().objects[&narset],
            &Keyword::Hexproof
        ));
        runner.act(GameAction::PassPriority).expect("P0 passes");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1)
        );
        (runner, murder, bear)
    }

    let (runner, murder, _) = board(false);
    assert!(
        !offers_cast_of(&runner, murder),
        "Murder has no legal target among a hexproof and a protection-from-black creature"
    );

    let (mut runner, murder, bear) = board(true);
    let bear = bear.expect("bear");
    assert!(
        offers_cast_of(&runner, murder),
        "a targetable creature makes Murder castable"
    );
    cast(&mut runner, murder);
    assert_eq!(
        stack_targets_of(&runner, murder),
        vec![TargetRef::Object(bear)],
        "the bear is the only legal target"
    );
}
