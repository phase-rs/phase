//! Enter observers read the entrant as it existed immediately after its entry
//! event, not as later instructions of the same resolution left it.
//!
//! CR 603.10: objects that exist immediately after an event are checked
//! against trigger conditions, using the continuous effects that exist at that
//! time. CR 603.6a: each time an event puts one or more permanents onto the
//! battlefield, every permanent on the battlefield (newcomers included) is
//! checked. CR 611.3c: continuous effects that modify characteristics apply as
//! the permanent enters. CR 608.2c: instructions are followed in the order
//! written, so a counter a later instruction puts on the entrant is not one of
//! its entry characteristics (contrast CR 614.1c "enters with").
//!
//! Built via the `/card-test` recipe: `GameScenario` + `GameRunner::cast(..)
//! .resolve()` + `CastOutcome` deltas, on verbatim Oracle text. Every negative
//! assertion is paired with a positive reach guard in the same test.
//!
//! REVERT DISCRIMINATORS: route `trigger_matchers::zone_change_clause_matches`'s
//! enters observers back to the live entrant (or drop the instruction-boundary
//! settle in `effects::resolve_chain_body`) and the Match the Odds, Fractal
//! Harness, Welcoming Vampire, Miraculous Recovery, library-search resume,
//! single-candidate hand put, living weapon and departed-token rows fail. The
//! settle in `drain_pending_continuation` is discriminated only by the
//! `effects` unit test `resume_boundary_settles_completed_entry`. Drop
//! `drive_to_empty_within`'s postcondition and
//! `drive_to_empty_refuses_an_unsettled_runner` fails; read liveness by address
//! in the kind authority and `owner_zone_view_keeps_off_zone_granted_keyword_kind`
//! fails; read kind-level keywords off the live object by id instead of the
//! record skeleton and `attack_declaration_record_reads_declaration_time_keyword_kind`
//! fails.

use engine::game::filter::{
    matches_target_filter_in_owner_zone, matches_target_filter_on_attack_declaration_record,
    FilterContext,
};
use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_keyword_kind;
use engine::game::scenario::GameRunner;
use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::ability::{
    Effect, FilterProp, PtValue, QuantityExpr, QuantityRef, SubAbilityLink, TargetFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{AttackDeclarationRecord, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Elemental Bond {2}{G} Enchantment — verbatim Oracle text.
const ELEMENTAL_BOND: &str =
    "Whenever a creature you control with power 3 or greater enters, draw a card.";
/// Match the Odds {2}{W} Sorcery — verbatim Oracle text.
const MATCH_THE_ODDS: &str = "Create a 1/1 white Ally creature token. Put a +1/+1 counter on it \
     for each creature your opponents control.";
/// Glorious Anthem {1}{W}{W} Enchantment — verbatim Oracle text.
const GLORIOUS_ANTHEM: &str = "Creatures you control get +1/+1.";
/// Welcoming Vampire {2}{W} Creature — Vampire 2/3 — verbatim Oracle text.
const WELCOMING_VAMPIRE: &str = "Flying\nWhenever one or more other creatures you control with \
     power 2 or less enter, draw a card. This ability triggers only once each turn.";
/// Miraculous Recovery {4}{W} Instant — verbatim Oracle text.
const MIRACULOUS_RECOVERY: &str =
    "Return target creature card from your graveyard to the battlefield. Put a +1/+1 counter on it.";
/// Rise from the Grave {4}{B} Sorcery — verbatim Oracle text.
const RISE_FROM_THE_GRAVE: &str = "Put target creature card from a graveyard onto the battlefield \
     under your control. That creature is a black Zombie in addition to its other colors and types.";
/// Wayward Servant {W}{B} Creature — Zombie 2/2 — verbatim Oracle text.
const WAYWARD_SERVANT: &str =
    "Whenever another Zombie you control enters, each opponent loses 1 life and you gain 1 life.";
/// Reckless Fireweaver {1}{R} Creature — Human Artificer 1/3 — verbatim Oracle text.
const RECKLESS_FIREWEAVER: &str =
    "Whenever an artifact you control enters, this creature deals 1 damage to each opponent.";
/// Helica Glider {2}{W} Creature — Nightmare Squirrel 2/2 — verbatim Oracle text.
const HELICA_GLIDER: &str =
    "This creature enters with your choice of a flying counter or a first strike counter on it.";
/// Dragon Tempest {1}{R} Enchantment — verbatim Oracle text.
const DRAGON_TEMPEST: &str = "Whenever a creature you control with flying enters, it gains haste until end of turn.
Whenever a Dragon you control enters, it deals X damage to any target, where X is the number of Dragons you control.";
/// Clone {3}{U} Creature — Shapeshifter 0/0 — verbatim Oracle text.
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
/// What Must Be Done {3}{W}{W} Sorcery — verbatim Oracle text.
const WHAT_MUST_BE_DONE: &str = "Choose one \u{2014}\n\
     \u{2022} Let the World Burn \u{2014} Destroy all artifacts and creatures.\n\
     \u{2022} Release Juno \u{2014} Return target historic permanent card from your graveyard \
     to the battlefield. It enters with two additional +1/+1 counters on it if it's a creature. \
     (Artifacts, legendaries, and Sagas are historic.)";
/// Batterskull {5} Artifact — Equipment — verbatim Oracle text.
const BATTERSKULL: &str = "Living weapon (When this Equipment enters, create a 0/0 black \
     Phyrexian Germ creature token, then attach this to it.)\n\
     Equipped creature gets +4/+4 and has vigilance and lifelink.\n\
     {3}: Return this Equipment to its owner's hand.\n\
     Equip {5}";
/// Fractal Harness {X}{2}{G} Artifact — Equipment — verbatim Oracle text.
const FRACTAL_HARNESS: &str = "When this Equipment enters, create a 0/0 green and blue Fractal \
     creature token. Put X +1/+1 counters on it and attach this Equipment to it.\n\
     Whenever equipped creature attacks, double the number of +1/+1 counters on it.\n\
     Equip {2}";
/// Thwart the Grave {4}{B}{B} Sorcery — verbatim Oracle text.
const THWART_THE_GRAVE: &str = "This spell costs {1} less to cast for each creature in your \
     party. (Your party consists of up to one each of Cleric, Rogue, Warrior, and Wizard.)\n\
     Return target creature card and up to one target Cleric, Rogue, Warrior, or Wizard creature \
     card from your graveyard to the battlefield.";
/// Maja, Bretagard Protector {2}{G}{W}{W} Legendary Creature — Human Warrior 2/3 —
/// verbatim Oracle text.
const MAJA: &str = "Other creatures you control get +1/+1.\n\
     Landfall \u{2014} Whenever a land you control enters, create a 1/1 white Human Warrior \
     creature token.";
/// Staff of the Death Magus {3} Artifact — verbatim Oracle text.
const STAFF_OF_THE_DEATH_MAGUS: &str =
    "Whenever you cast a black spell or a Swamp you control enters, you gain 1 life.";
/// Clear the Land {2}{G} Sorcery — verbatim Oracle text.
const CLEAR_THE_LAND: &str = "Each player reveals the top five cards of their library, puts all \
     land cards revealed this way onto the battlefield tapped, and exiles the rest.";
/// Urborg, Tomb of Yawgmoth Legendary Land — verbatim Oracle text.
const URBORG: &str = "Each land is a Swamp in addition to its other land types.";
/// Path of the Schemer {4}{B} Sorcery — verbatim Oracle text.
const PATH_OF_THE_SCHEMER: &str = "Each player mills two cards. Then you put a creature card from \
     a graveyard onto the battlefield under your control. It's an artifact in addition to its \
     other types.\n\
     Will of the Planeswalkers \u{2014} Starting with you, each player votes for planeswalk or \
     chaos. If planeswalk gets more votes, planeswalk. If chaos gets more votes or the vote is \
     tied, chaos ensues.";

/// Snapcaster Mage {1}{U} Creature — Human Wizard 2/1 — verbatim Oracle text.
const SNAPCASTER_MAGE: &str = "Flash\nWhen this creature enters, target instant or sorcery card \
     in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana \
     cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
/// Divination {2}{U} Sorcery — verbatim Oracle text.
const DIVINATION: &str = "Draw two cards.";

/// Synthetic (no printed card): a hidden-zone search whose found card enters
/// inside the resumed continuation, followed by a later counter instruction.
const SEARCH_THEN_COUNTERS: &str = "Search your library for a creature card and put it onto the battlefield. Put two +1/+1 counters on it. Then shuffle.";
/// Synthetic: an optional hand put followed by a later counter instruction.
const HAND_PICK_THEN_COUNTERS: &str = "You may put a creature card from your hand onto the battlefield. Put two +1/+1 counters on it.";
/// Synthetic: Match the Odds followed by a sacrifice of the token.
const MATCH_THE_ODDS_THEN_SACRIFICE: &str = "Create a 1/1 white Ally creature token. Put a +1/+1 counter on it for each creature your opponents control. Sacrifice it.";
/// Synthetic: a return with unconditional entry counters, then a later
/// instruction that does not touch the entrant.
const RETURN_WITH_COUNTERS_THEN_GAIN: &str = "Return target creature card from your graveyard to the battlefield with two +1/+1 counters on it. You gain 1 life.";
/// Synthetic: a return followed by a later instruction that does not touch the entrant.
const RETURN_THEN_GAIN: &str =
    "Return target creature card from your graveyard to the battlefield. You gain 2 life.";

const BEAST: &str = "Create a 3/3 green Beast creature token.";
const SOLDIER: &str = "Create a 2/2 white Soldier creature token.";

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

/// A main-phase board with a stocked P0 library (draws are counted).
fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    scenario.with_library_top(P1, &["Card E", "Card F", "Card G", "Card H"]);
    scenario
}

fn add_bond(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND)
        .id()
}

fn add_sorcery(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, false, text)
        .id()
}

fn battlefield_token(outcome: &CastOutcome, name_part: &str) -> Option<ObjectId> {
    outcome.find_object(|o| o.is_token && o.zone == Zone::Battlefield && o.name.contains(name_part))
}

fn power_of(outcome: &CastOutcome, id: ObjectId) -> Option<i32> {
    outcome.state().objects.get(&id).and_then(|o| o.power)
}

fn token_with_counter_on_the_odds(opp_creatures: usize, observer: Observer) -> CastOutcome {
    let mut scenario = board();
    match observer {
        Observer::ElementalBond => {
            add_bond(&mut scenario);
        }
        Observer::WelcomingVampire => {
            scenario
                .add_creature_from_oracle(P0, "Welcoming Vampire", 2, 3, WELCOMING_VAMPIRE)
                .with_subtypes(vec!["Vampire"]);
        }
    }
    for i in 0..opp_creatures {
        scenario.add_creature(P1, &format!("Opponent Creature {i}"), 1, 1);
    }
    let spell = add_sorcery(&mut scenario, "Match the Odds", MATCH_THE_ODDS);
    let mut runner = scenario.build();
    runner.cast(spell).resolve()
}

#[derive(Clone, Copy)]
enum Observer {
    ElementalBond,
    WelcomingVampire,
}

/// A1 — CR 603.10 + CR 608.2c: Match the Odds' token enters as a 1/1; the
/// counters the second instruction puts on it are not entry characteristics,
/// so Elemental Bond does not trigger.
#[test]
fn match_the_odds_counters_after_entry_do_not_satisfy_bond() {
    let outcome = token_with_counter_on_the_odds(2, Observer::ElementalBond);
    let ally = battlefield_token(&outcome, "Ally").expect("the Ally token is on the battlefield");
    assert_eq!(outcome.counters(ally, CounterType::Plus1Plus1), 2);
    assert_eq!(
        power_of(&outcome, ally),
        Some(3),
        "reach: the token is 3/3 now"
    );
    outcome.assert_hand_drawn(P0, 0);
}

/// A2 — CR 603.10: a 3/3 token satisfies Bond at entry.
#[test]
fn three_power_token_satisfies_bond() {
    let mut scenario = board();
    add_bond(&mut scenario);
    let spell = add_sorcery(&mut scenario, "Beast Spell", BEAST);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(battlefield_token(&outcome, "Beast").is_some());
    outcome.assert_hand_drawn(P0, 1);
}

/// A3 — CR 611.3c: Glorious Anthem applies as the 2/2 token enters, so it is
/// a 3/3 immediately after the event and Bond triggers.
#[test]
fn anthem_in_force_at_entry_satisfies_bond() {
    let mut scenario = board();
    add_bond(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM);
    let spell = add_sorcery(&mut scenario, "Soldier Spell", SOLDIER);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    let soldier = battlefield_token(&outcome, "Soldier").expect("Soldier token");
    assert_eq!(power_of(&outcome, soldier), Some(3));
    outcome.assert_hand_drawn(P0, 1);
}

/// A4 — CR 608.2c + CR 107.3m + CR 704.3: Fractal Harness cast with X = 3.
/// The Fractal enters as a 0/0, so Bond does not trigger; the three counters
/// and the attach are later instructions, and state-based actions wait until
/// the trigger has resolved, so the token survives as a 3/3.
#[test]
fn fractal_harness_zero_zero_token_does_not_satisfy_bond_and_survives() {
    let mut scenario = board();
    add_bond(&mut scenario);
    let harness = scenario
        .add_artifact_to_hand_from_oracle(P0, "Fractal Harness", FRACTAL_HARNESS)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(P0, {
        let mut pool = mana(ManaType::Green, 1);
        pool.extend(mana(ManaType::Colorless, 5));
        pool
    });
    let mut runner = scenario.build();

    // Parse guard: Token(0/0) -> PutCounter(X, LastCreated) -> Attach.
    {
        let trigger = &runner.state().objects[&harness].trigger_definitions[0];
        let execute = trigger
            .definition
            .execute
            .as_ref()
            .expect("enters trigger body");
        match execute.effect.as_ref() {
            Effect::Token {
                power,
                enter_with_counters,
                ..
            } => {
                assert!(
                    matches!(
                        power,
                        PtValue::Fixed(0) | PtValue::Quantity(QuantityExpr::Fixed { value: 0 })
                    ),
                    "premise: a 0/0 token, got {power:?}"
                );
                assert!(enter_with_counters.is_empty(), "premise: no entry counters");
            }
            other => panic!("premise: Token head, got {other:?}"),
        }
        let put = execute.sub_ability.as_ref().expect("PutCounter sub");
        assert!(matches!(
            put.effect.as_ref(),
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                },
                target: TargetFilter::LastCreated,
                ..
            }
        ));
        assert_eq!(put.sub_link, SubAbilityLink::SequentialSibling);
        let attach = put.sub_ability.as_ref().expect("Attach sub");
        assert!(matches!(attach.effect.as_ref(), Effect::Attach { .. }));
    }

    let outcome = runner.cast(harness).x(3).resolve();
    let fractal = battlefield_token(&outcome, "Fractal").expect("the Fractal survives");
    assert_eq!(outcome.counters(fractal, CounterType::Plus1Plus1), 3);
    assert_eq!(power_of(&outcome, fractal), Some(3));
    assert_eq!(
        outcome.state().objects[&harness].attached_to,
        Some(AttachTarget::Object(fractal)),
        "reach: the trigger ran to its end"
    );
    outcome.assert_hand_drawn(P0, 0);
}

/// R4 — CR 603.10 + CR 608.2c: Welcoming Vampire's batched "power 2 or less"
/// observer reads Match the Odds' token as the 1/1 it entered as, so it draws
/// even though the later counter instruction grew the token to 3/3.
#[test]
fn welcoming_vampire_reads_entry_power_before_later_counters() {
    let outcome = token_with_counter_on_the_odds(2, Observer::WelcomingVampire);
    let ally = battlefield_token(&outcome, "Ally").expect("Ally token");
    assert_eq!(outcome.counters(ally, CounterType::Plus1Plus1), 2);
    assert_eq!(
        power_of(&outcome, ally),
        Some(3),
        "reach: token grew to 3/3"
    );
    outcome.assert_hand_drawn(P0, 1);
}

/// R4 sibling: with no opposing creatures the token stays 1/1 and the Vampire draws.
#[test]
fn r4_sibling_zero_opponents() {
    let sibling = token_with_counter_on_the_odds(0, Observer::WelcomingVampire);
    sibling.assert_hand_drawn(P0, 1);
}

fn recovery_on(gy_name: &str, power: i32, toughness: i32) -> (CastOutcome, ObjectId) {
    let mut scenario = board();
    add_bond(&mut scenario);
    let returned = scenario
        .add_creature_to_graveyard(P0, gy_name, power, toughness)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Miraculous Recovery", true, MIRACULOUS_RECOVERY)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(returned).resolve();
    (outcome, returned)
}

/// R5 — CR 603.10 + CR 608.2c: Miraculous Recovery returns Grizzly Bears as a
/// 2/2; its +1/+1 counter is a later instruction, so Bond does not trigger.
#[test]
fn r5_bears() {
    let (outcome, bears) = recovery_on("Grizzly Bears", 2, 2);
    assert_eq!(outcome.zone_of(bears), Zone::Battlefield);
    assert_eq!(outcome.counters(bears, CounterType::Plus1Plus1), 1);
    assert_eq!(power_of(&outcome, bears), Some(3));
    outcome.assert_hand_drawn(P0, 0);
}

/// R5 sibling — CR 603.10: Hill Giant is 3/3 at entry, so Bond triggers.
#[test]
fn r5_giant_sibling() {
    let (outcome, giant) = recovery_on("Hill Giant", 3, 3);
    assert_eq!(outcome.zone_of(giant), Zone::Battlefield);
    assert_eq!(outcome.counters(giant, CounterType::Plus1Plus1), 1);
    outcome.assert_hand_drawn(P0, 1);
}

/// Answer every remaining prompt the rows below need; returns P0's draws and
/// the prompts answered (variant names, in order).
fn drive_to_empty(
    runner: &mut GameRunner,
    pick: &[ObjectId],
    branch: Option<&str>,
) -> (usize, Vec<String>) {
    drive_to_empty_within(runner, pick, branch, 40)
}

/// `drive_to_empty` with an explicit step budget. After the loop it asserts
/// the settled postcondition (Priority on an empty stack) on the state itself,
/// so a run that exhausts its budget unsettled fails instead of returning.
fn drive_to_empty_within(
    runner: &mut GameRunner,
    pick: &[ObjectId],
    branch: Option<&str>,
    max_steps: usize,
) -> (usize, Vec<String>) {
    let mut draws = 0;
    let mut prompts = Vec::new();
    for _ in 0..max_steps {
        let w = runner.state().waiting_for.clone();
        prompts.push(
            format!("{w:?}")
                .split([' ', '{', '('])
                .next()
                .unwrap_or("")
                .to_string(),
        );
        let action = match &w {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => GameAction::ChooseBranch {
                index: branch_descriptions
                    .iter()
                    .position(|d| d.to_lowercase().contains(branch.expect("a branch keyword")))
                    .expect("the named branch"),
            },
            WaitingFor::SearchChoice { cards, .. } => GameAction::SelectCards {
                cards: cards.iter().copied().filter(|c| pick.contains(c)).collect(),
            },
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::EffectZoneChoice { cards, count, .. } => GameAction::SelectCards {
                cards: cards
                    .iter()
                    .copied()
                    .filter(|c| runner.state().objects[c].zone != Zone::Battlefield)
                    .take(*count)
                    .collect(),
            },
            WaitingFor::VoteChoice { .. } => GameAction::ChooseOption {
                choice: "chaos".to_string(),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        let result = runner.act(action).expect("legal answer");
        draws += result
            .events
            .iter()
            .filter(|e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == P0))
            .count();
    }
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty(),
        "drive_to_empty did not reach Priority on an empty stack within {max_steps} steps; \
         prompts seen: {prompts:?}"
    );
    (draws, prompts)
}

fn glider_with_tempest(text: &str) -> (GameRunner, ObjectId) {
    let mut scenario = board();
    scenario.add_enchantment_from_oracle(P0, "Dragon Tempest", DRAGON_TEMPEST);
    let glider = scenario
        .add_creature_to_graveyard(P0, "Helica Glider", 2, 2)
        .from_oracle_text(HELICA_GLIDER)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Miraculous Recovery", true, text)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(glider).resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "reach: the counter choice pauses resolution, got {:?}",
        runner.state().waiting_for
    );
    drive_to_empty(&mut runner, &[], Some("flying"));
    (runner, glider)
}

/// R5b — CR 614.12a + CR 614.1c: the counter choice is made before the Glider
/// enters, so it enters with flying and Dragon Tempest gives it haste. A
/// pre-choice settlement would observe a non-flying entrant.
#[test]
fn r5b_glider_flying_counter_gives_haste() {
    let (runner, glider) = glider_with_tempest(MIRACULOUS_RECOVERY);
    let g = &runner.state().objects[&glider];
    assert_eq!(g.zone, Zone::Battlefield);
    assert!(g.has_keyword(&Keyword::Flying));
    assert_eq!(
        g.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "reach: the later instruction ran"
    );
    assert!(
        g.has_keyword(&Keyword::Haste),
        "Dragon Tempest saw a flying entrant"
    );
}

fn clone_return(text: &str, copy_bears: bool) -> (CastOutcome, ObjectId) {
    let mut scenario = board();
    add_bond(&mut scenario);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let clone = scenario
        .add_creature_to_graveyard(P0, "Clone", 0, 0)
        .from_oracle_text(CLONE)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Spell", true, text)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_object(clone)
        .replacement_choice(0)
        .copy_target(if copy_bears { bears } else { giant })
        .resolve();
    assert_eq!(outcome.zone_of(clone), Zone::Battlefield);
    (outcome, clone)
}

/// R5c — CR 707.5 + CR 614.12a: Clone enters as the chosen copy. As Hill Giant
/// it is 3/3 (Bond triggers); as Grizzly Bears it is 2/2 (it does not).
#[test]
fn r5c_recovery_clone_giant() {
    let (outcome, clone) = clone_return(MIRACULOUS_RECOVERY, false);
    assert_eq!(outcome.state().objects[&clone].name, "Hill Giant");
    outcome.assert_hand_drawn(P0, 1);
}

#[test]
fn r5c_recovery_clone_bears() {
    let (outcome, clone) = clone_return(MIRACULOUS_RECOVERY, true);
    assert_eq!(outcome.state().objects[&clone].name, "Grizzly Bears");
    outcome.assert_hand_drawn(P0, 0);
}

/// R5c — CR 707.5 + CR 608.2c: a later instruction that leaves the copy alone
/// still runs after the copy entry; the copy is read as it entered.
#[test]
fn r5c_gain_clone_giant() {
    let (outcome, clone) = clone_return(RETURN_THEN_GAIN, false);
    assert_eq!(outcome.state().objects[&clone].name, "Hill Giant");
    outcome.assert_life_delta(P0, 2);
    outcome.assert_hand_drawn(P0, 1);
}

#[test]
fn r5c_gain_clone_bears() {
    let (outcome, clone) = clone_return(RETURN_THEN_GAIN, true);
    assert_eq!(outcome.state().objects[&clone].name, "Grizzly Bears");
    outcome.assert_life_delta(P0, 2);
    outcome.assert_hand_drawn(P0, 0);
}

fn library_creature(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_spell_to_library_top(P0, name, false)
        .as_creature()
        .id()
}

fn set_pt(runner: &mut GameRunner, id: ObjectId, p: i32, t: i32) {
    let o = runner.state_mut().objects.get_mut(&id).unwrap();
    o.power = Some(p);
    o.toughness = Some(t);
    o.base_power = Some(p);
    o.base_toughness = Some(t);
}

fn search_then_counters(pick_giant: bool) -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Card E", "Card F", "Card G", "Card H"]);
    add_bond(&mut scenario);
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    let giant = library_creature(&mut scenario, "Hill Giant");
    let bears = library_creature(&mut scenario, "Grizzly Bears");
    let spell = add_sorcery(&mut scenario, "Search Then Counters", SEARCH_THEN_COUNTERS);
    let mut runner = scenario.build();
    set_pt(&mut runner, giant, 3, 3);
    set_pt(&mut runner, bears, 2, 2);
    // Parse guard: SearchLibrary -> ChangeZone{Library -> Battlefield, forward_result} -> PutCounter -> Shuffle.
    {
        let a = &runner.state().objects[&spell].abilities[0];
        assert!(matches!(a.effect.as_ref(), Effect::SearchLibrary { .. }));
        let cz = a.sub_ability.as_ref().expect("ChangeZone");
        assert!(matches!(
            cz.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        ));
        assert!(cz.forward_result);
        let put = cz.sub_ability.as_ref().expect("PutCounter");
        assert!(matches!(
            put.effect.as_ref(),
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                ..
            }
        ));
        assert_eq!(put.sub_link, SubAbilityLink::SequentialSibling);
    }
    let chosen = if pick_giant { giant } else { bears };
    let c = runner.cast(spell).commit();
    drop(c);
    let (draws, prompts) = drive_to_empty(&mut runner, &[chosen], None);
    assert!(
        prompts.iter().any(|p| p == "SearchChoice"),
        "reach: the search paused for a choice: {prompts:?}"
    );
    (runner, chosen, draws)
}

/// R5d — CR 603.10 + CR 608.2c: the searched-for Bears enter inside the resumed
/// chain as a 2/2; the two counters are a later instruction.
#[test]
fn r5d_search_bears() {
    let (runner, bears, draws) = search_then_counters(false);
    let o = &runner.state().objects[&bears];
    assert_eq!(o.zone, Zone::Battlefield);
    assert_eq!(o.counters.get(&CounterType::Plus1Plus1).copied(), Some(2));
    assert_eq!(o.power, Some(4));
    assert_eq!(draws, 0);
}

#[test]
fn r5d_search_giant_sibling() {
    let (runner, giant, draws) = search_then_counters(true);
    assert_eq!(runner.state().objects[&giant].power, Some(5));
    assert_eq!(draws, 1);
}

fn hand_pick(pick_giant: bool) -> (CastOutcome, ObjectId) {
    let mut scenario = board();
    add_bond(&mut scenario);
    let giant = scenario.add_creature_to_hand(P0, "Hill Giant", 3, 3).id();
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let spell = add_sorcery(&mut scenario, "Hand Pick", HAND_PICK_THEN_COUNTERS);
    let mut runner = scenario.build();
    let chosen = if pick_giant { giant } else { bears };
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .effect_zone(&[chosen])
        .resolve();
    (outcome, chosen)
}

fn hand_pick_single() -> (CastOutcome, ObjectId) {
    let mut scenario = board();
    add_bond(&mut scenario);
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let spell = add_sorcery(&mut scenario, "Hand Pick", HAND_PICK_THEN_COUNTERS);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).accept_optional().resolve();
    (outcome, bears)
}

/// R5e — CR 603.10 + CR 608.2c: the single-candidate optional put enters the
/// Bears as a 2/2 inside the accepted chain; the counters are a later instruction.
#[test]
fn r5e_optional_single_candidate_bears() {
    let (outcome, bears) = hand_pick_single();
    assert_eq!(outcome.zone_of(bears), Zone::Battlefield);
    assert_eq!(outcome.counters(bears, CounterType::Plus1Plus1), 2);
    assert_eq!(power_of(&outcome, bears), Some(4));
    assert_eq!(
        outcome.hand_drawn(P0),
        -1,
        "hand delta -1 = the Bears left the hand and Bond drew nothing"
    );
}

/// R5f — CR 603.10 + CR 608.2c: a chosen hand put (`EffectZoneChoice`) reads
/// the Bears as the 2/2 they entered as.
#[test]
fn r5f_effect_zone_choice_bears() {
    let (outcome, bears) = hand_pick(false);
    assert_eq!(outcome.counters(bears, CounterType::Plus1Plus1), 2);
    assert_eq!(power_of(&outcome, bears), Some(4));
    assert_eq!(
        outcome.hand_drawn(P0),
        -1,
        "hand delta -1 = the Bears left the hand and Bond drew nothing"
    );
}

/// R5f sibling — the Giant is 3/3 at entry, so Bond draws.
#[test]
fn r5f_effect_zone_choice_giant_sibling() {
    let (outcome, giant) = hand_pick(true);
    assert_eq!(power_of(&outcome, giant), Some(5));
    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "hand delta 0 = the Giant left the hand and Bond drew one card"
    );
}

/// R7 — CR 611.2e: "That creature is a black Zombie" applies as the card enters,
/// so Wayward Servant triggers.
#[test]
fn r7_rise_from_the_grave() {
    let mut scenario = board();
    scenario
        .add_creature_from_oracle(P0, "Wayward Servant", 2, 2, WAYWARD_SERVANT)
        .with_subtypes(vec!["Zombie"]);
    let giant = scenario
        .add_creature_to_graveyard(P1, "Hill Giant", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let spell = add_sorcery(&mut scenario, "Rise from the Grave", RISE_FROM_THE_GRAVE);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(giant).resolve();
    let obj = &outcome.state().objects[&giant];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.controller, P0);
    assert!(obj.card_types.subtypes.iter().any(|s| s == "Zombie"));
    outcome.assert_life_delta(P1, -1);
    outcome.assert_life_delta(P0, 1);
}

/// R8 — CR 702.92a: living weapon creates a 0/0 Germ, then attaches; the Germ
/// entered as a 0/0, so Bond does not trigger.
#[test]
fn r8_batterskull() {
    let mut scenario = board();
    add_bond(&mut scenario);
    let skull = scenario
        .add_artifact_to_hand_from_oracle(P0, "Batterskull", BATTERSKULL)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(skull).resolve();
    let germ = battlefield_token(&outcome, "Germ").expect("the Germ survives");
    assert_eq!(
        outcome.state().objects[&skull].attached_to,
        Some(AttachTarget::Object(germ))
    );
    assert_eq!(power_of(&outcome, germ), Some(4));
    outcome.assert_hand_drawn(P0, 0);
}

/// R9 — CR 603.10 + CR 611.3c: under two anthems the 1/1 token is 3/3
/// immediately after its entry, so Bond triggers although a later instruction
/// sacrificed it.
#[test]
fn r9_departed_token() {
    let mut scenario = board();
    add_bond(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM);
    scenario.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM);
    let spell = add_sorcery(
        &mut scenario,
        "Odds Then Sacrifice",
        MATCH_THE_ODDS_THEN_SACRIFICE,
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(
        battlefield_token(&outcome, "Ally").is_none(),
        "reach: token is gone"
    );
    assert!(outcome.events().iter().any(|e| matches!(
        e,
        GameEvent::ZoneChanged {
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            ..
        }
    )));
    outcome.assert_hand_drawn(P0, 1);
}

fn memnite_return(text: &str, modal: bool) -> (CastOutcome, ObjectId) {
    let mut scenario = board();
    add_bond(&mut scenario);
    let memnite = scenario
        .add_creature_to_graveyard(P0, "Memnite", 1, 1)
        .as_artifact_creature()
        .id();
    let spell = add_sorcery(&mut scenario, "Test Spell", text);
    let mut runner = scenario.build();
    if !modal {
        // Parse guard: ChangeZone{enter_with_counters: [(P1P1, 2)]} -> GainLife.
        let a = &runner.state().objects[&spell].abilities[0];
        match a.effect.as_ref() {
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                enter_with_counters,
                ..
            } => assert_eq!(
                enter_with_counters.len(),
                1,
                "premise: one entry-counter slot"
            ),
            other => panic!("premise: ChangeZone head, got {other:?}"),
        }
        let gain = a.sub_ability.as_ref().expect("GainLife sub");
        assert!(matches!(gain.effect.as_ref(), Effect::GainLife { .. }));
        assert_eq!(gain.sub_link, SubAbilityLink::SequentialSibling);
    }
    let cast = runner.cast(spell).target_object(memnite);
    let outcome = if modal { cast.modes(&[1]) } else { cast }.resolve();
    (outcome, memnite)
}

/// R10a — CR 614.1c: What Must Be Done's "enters with two additional +1/+1
/// counters" are entry characteristics, so Memnite enters 3/3 and Bond triggers.
#[test]
fn r10a_what_must_be_done() {
    let (outcome, memnite) = memnite_return(WHAT_MUST_BE_DONE, true);
    assert_eq!(outcome.zone_of(memnite), Zone::Battlefield);
    assert_eq!(outcome.counters(memnite, CounterType::Plus1Plus1), 2);
    outcome.assert_hand_drawn(P0, 1);
}

/// R10b — CR 614.1c + CR 603.10: settlement at the boundary before a later
/// instruction never runs ahead of the entry-slot counters: the observation
/// already holds Memnite's two counters and power 3.
#[test]
fn r10b_gain() {
    let (outcome, memnite) = memnite_return(RETURN_WITH_COUNTERS_THEN_GAIN, false);
    assert_eq!(outcome.counters(memnite, CounterType::Plus1Plus1), 2);
    assert_eq!(power_of(&outcome, memnite), Some(3));
    outcome.assert_life_delta(P0, 1);
    outcome.assert_hand_drawn(P0, 1);

    let observations = &outcome.state().battlefield_entry_observations;
    assert_eq!(observations.len(), 1, "reach: the entry was settled");
    let row = observations.values().next().expect("one observation");
    assert_eq!(row.identity.reference.object_id, memnite);
    assert_eq!(row.lki.power, Some(3));
    assert_eq!(
        row.lki.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2)
    );
}

/// R11 — CR 101.4 + CR 603.6a + CR 611.3c: both players' lands enter in one
/// simultaneous event; Urborg's static applies as they enter, so the Plains is
/// a Swamp and the Staff triggers.
#[test]
fn r11_clear_the_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Staff of the Death Magus", STAFF_OF_THE_DEATH_MAGUS);
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("P0 Spell {i}"), false);
        scenario.add_spell_to_library_top(P1, &format!("P1 Spell {i}"), false);
    }
    let plains = scenario
        .add_land_to_library_top(P0, "Plains")
        .with_subtypes(vec!["Plains"])
        .id();
    let urborg = scenario
        .add_land_to_library_top(P1, "Urborg, Tomb of Yawgmoth")
        .as_legendary()
        .from_oracle_text(URBORG)
        .id();
    let spell = add_sorcery(&mut scenario, "Clear the Land", CLEAR_THE_LAND);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    runner.cast(spell).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ));
    let _ = drive_to_empty(&mut runner, &[], None);
    let st = runner.state();
    assert_eq!(st.objects[&plains].zone, Zone::Battlefield);
    assert_eq!(st.objects[&urborg].zone, Zone::Battlefield);
    assert!(st.objects[&plains].tapped && st.objects[&urborg].tapped);
    assert!(st.objects[&plains]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Swamp"));
    assert_eq!(runner.life(P0) - life_before, 1);
}

/// R12 — CR 611.2e: "It's an artifact in addition to its other types" applies
/// as the creature enters, so Reckless Fireweaver triggers.
#[test]
fn r12_path_of_the_schemer() {
    let mut scenario = board();
    scenario.add_creature_from_oracle(P0, "Reckless Fireweaver", 1, 3, RECKLESS_FIREWEAVER);
    let giant = scenario
        .add_creature_to_graveyard(P1, "Hill Giant", 3, 3)
        .id();
    let spell = add_sorcery(&mut scenario, "Path of the Schemer", PATH_OF_THE_SCHEMER);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);
    runner.cast(spell).effect_zone(&[giant]).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::VoteChoice { .. }
    ));
    let _ = drive_to_empty(&mut runner, &[], None);
    let obj = &runner.state().objects[&giant];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.controller, P0);
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!(runner.life(P1) - p1_before, -1);
}

/// R13 — CR 603.6a + CR 611.3c: Thwart the Grave returns both cards in one
/// event; Maja's static applies as the Bears enter, so they are 3/3 and Bond
/// triggers.
#[test]
fn r13_thwart_the_grave() {
    let mut scenario = board();
    add_bond(&mut scenario);
    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let maja = scenario
        .add_creature_to_graveyard(P0, "Maja, Bretagard Protector", 2, 3)
        .as_legendary()
        .with_subtypes(vec!["Human", "Warrior"])
        .from_oracle_text(MAJA)
        .id();
    let spell = add_sorcery(&mut scenario, "Thwart the Grave", THWART_THE_GRAVE);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[bears, maja]).resolve();
    assert_eq!(outcome.zone_of(bears), Zone::Battlefield);
    assert_eq!(outcome.zone_of(maja), Zone::Battlefield);
    assert_eq!(power_of(&outcome, bears), Some(3));
    outcome.assert_hand_drawn(P0, 1);
}

/// L1/C5: the harness refuses a runner it could not settle. A committed,
/// unresolved spell leaves Priority with a non-empty stack; with no steps
/// allowed the loop cannot resolve it, so the postcondition fires.
#[test]
#[should_panic(expected = "did not reach Priority on an empty stack")]
fn drive_to_empty_refuses_an_unsettled_runner() {
    let mut scenario = board();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .id();
    let mut runner = scenario.build();
    drop(runner.cast(spell).commit());
    let _ = drive_to_empty_within(&mut runner, &[], None, 0);
}

/// L1/C5 paired positive: a runner already at Priority on an empty stack has
/// nothing left to answer, so the postcondition holds with no steps taken.
#[test]
fn drive_to_empty_accepts_a_settled_runner() {
    let mut runner = board().build();
    assert_eq!(
        drive_to_empty_within(&mut runner, &[], None, 0),
        (0, vec![])
    );
}

/// B2 (CR 702.34a + CR 109.5 + CR 400.3): Snapcaster Mage's enters trigger gives
/// Divination in P0's graveyard flashback until end of turn. The owner-zone door
/// reads "your" as the card's owner; when the card's `controller` field names
/// another player the door evaluates a rescoped view of the same live card, which
/// keeps the off-zone grant: `HasKeywordKind(Flashback)` stays true. The Bear was
/// never targeted and stays false on both paths.
#[test]
fn owner_zone_view_keeps_off_zone_granted_keyword_kind() {
    let mut scenario = board();
    let snapcaster = scenario
        .add_creature_to_hand_from_oracle(P0, "Snapcaster Mage", 2, 1, SNAPCASTER_MAGE)
        .id();
    let divination = scenario
        .add_spell_to_graveyard(P0, "Divination", false)
        .from_oracle_text(DIVINATION)
        .id();
    let bear = scenario.add_creature_to_graveyard(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    let _ = runner.cast(snapcaster).resolve();
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner.choose_first_legal_target().expect("target");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        1,
        "reach: Snapcaster Mage's trigger resolved and installed its grant"
    );
    assert!(
        !has_keyword_kind(&runner.state().objects[&divination], KeywordKind::Flashback),
        "guard: the grant reaches Divination only through the off-zone path"
    );
    let flashback =
        TargetFilter::Typed(
            TypedFilter::card().properties(vec![FilterProp::HasKeywordKind {
                value: KeywordKind::Flashback,
            }]),
        );
    let reads = |runner: &GameRunner| {
        let state = runner.state();
        let ctx = FilterContext::from_source(state, snapcaster);
        (
            matches_target_filter_in_owner_zone(state, divination, &flashback, &ctx),
            matches_target_filter_in_owner_zone(state, bear, &flashback, &ctx),
        )
    };
    assert_eq!(
        reads(&runner),
        (true, false),
        "fast path: controller == owner"
    );
    for id in [divination, bear] {
        runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
    }
    assert_eq!(
        reads(&runner),
        (true, false),
        "clone path: controller != owner"
    );
}

/// B2 (CR 508.1a + CR 608.2i): an attack-declaration record answers kind-level
/// keyword predicates from its declaration-time snapshot, as `WithKeyword`
/// already does, not from the live attacker that has since lost flying.
#[test]
fn attack_declaration_record_reads_declaration_time_keyword_kind() {
    let mut scenario = board();
    let bird = scenario.add_creature(P0, "Bird", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&bird)
        .unwrap()
        .keywords
        .push(Keyword::Flying);
    let obj = runner.state().objects[&bird].clone();
    let record = AttackDeclarationRecord {
        object_id: bird,
        incarnation: Some(obj.incarnation),
        lki: obj.snapshot_public_characteristics(),
        is_token: false,
        is_commander: false,
    };
    runner
        .state_mut()
        .objects
        .get_mut(&bird)
        .unwrap()
        .keywords
        .clear();
    let state = runner.state();
    let ctx = FilterContext::from_source(state, bird);
    let creature_with =
        |prop: FilterProp| TargetFilter::Typed(TypedFilter::creature().properties(vec![prop]));
    let reads = |prop: FilterProp| {
        matches_target_filter_on_attack_declaration_record(
            state,
            &record,
            &creature_with(prop),
            &ctx,
        )
    };
    assert!(
        reads(FilterProp::WithKeyword {
            value: Keyword::Flying
        }),
        "positive: the snapshot recorded flying"
    );
    assert!(reads(FilterProp::HasKeywordKind {
        value: KeywordKind::Flying
    }));
    assert!(!reads(FilterProp::WithoutKeywordKind {
        value: KeywordKind::Flying
    }));
    assert!(
        !reads(FilterProp::HasKeywordKind {
            value: KeywordKind::FirstStrike
        }),
        "negative: the attacker never had first strike"
    );
}
