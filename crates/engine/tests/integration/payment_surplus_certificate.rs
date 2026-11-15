//! The AI payment-surplus certificate (`ai_support::surplus_mana_activation`)
//! on printed boards, driven through production actions.
//!
//! CR 601.2g–h: while a spell is paid for, its caster may activate mana
//! abilities, then pays the total cost. CR 605.3a + CR 117.3c: a mana ability
//! may be activated whenever its controller has priority, and an ordinary
//! caster receives priority once the spell is cast. A mana activation the
//! payment does not need is a surplus only when deferring it changes nothing
//! the cast records; each row below pins one step of that certificate with the
//! reason it refuses (or `Surplus`), reach-guarded on the board it builds.
//!
//! Oracle text verbatim from Scryfall `cards/named`.

use engine::ai_support::{
    surplus_mana_activation, surplus_mana_activation_with_counters, SurplusCertificateCounters,
    SurplusManaActivation, SurplusNotCertified,
};
use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;

const TREASONOUS_OGRE: &str = "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\nPay 3 life: Add {R}.";
const UNSUBSTANTIATE: &str = "Return target spell or creature to its owner's hand.";

use SurplusManaActivation::{NotCertified, Surplus};

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn add_ogre(sc: &mut GameScenario, player: engine::types::player::PlayerId) -> ObjectId {
    sc.add_creature(player, "Treasonous Ogre", 2, 3)
        .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
        .id()
}

fn ogre_activation(ogre: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: ogre,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    }
}

/// The engine-issued mana action for `source` (producing `color`, when the
/// source offers a choice), as the payment step offers it.
fn mana_action(state: &GameState, source: ObjectId, color: Option<ManaType>) -> GameAction {
    activatable_mana_actions_for_player(state, P0)
        .into_iter()
        .find(|action| match action {
            GameAction::TapLandForMana { selection }
            | GameAction::ActivateManaSource { selection } => {
                selection.source.object_id == source
                    && color.is_none_or(|color| selection.mana_type == color)
            }
            GameAction::ActivateAbility { source_id, .. } => *source_id == source,
            _ => false,
        })
        .unwrap_or_else(|| panic!("{source:?} offers a mana action at {:?}", state.waiting_for))
}

fn act(runner: &mut GameRunner, action: GameAction) {
    runner
        .act(action.clone())
        .unwrap_or_else(|error| panic!("{action:?} is legal: {error:?}"));
}

fn cast(runner: &mut GameRunner, spell: ObjectId, mode: CastPaymentMode) {
    let card_id = runner.state().objects[&spell].card_id;
    act(
        runner,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: mode,
        },
    );
}

fn red_in_pool(state: &GameState) -> usize {
    state.players[0].mana_pool.count_color(ManaType::Red)
}

fn assert_payment_step(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::ManaPayment { player, .. } if player == P0),
        "reach: the cast waits at P0's mana-payment step, got {:?}",
        state.waiting_for
    );
}

/// Island + Treasonous Ogre; Unsubstantiate ({1}{U}) cast at P1's Grizzly
/// Bears. Returns the runner at the payment step, the Island, the Ogre.
fn unsubstantiate_board(extra: impl FnOnce(&mut GameScenario)) -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let ogre = add_ogre(&mut sc, P0);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    extra(&mut sc);
    let mut runner = sc.build();
    cast(&mut runner, spell, CastPaymentMode::Auto);
    act(
        &mut runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears)),
        },
    );
    assert_payment_step(runner.state());
    (runner, island, ogre)
}

/// The T1 board at its saturated decision: the Island paid {U}, one Ogre
/// activation paid {R}, so passing pays {1}{U}.
fn saturated_unsubstantiate(extra: impl FnOnce(&mut GameScenario)) -> (GameRunner, ObjectId) {
    let (mut runner, island, ogre) = unsubstantiate_board(extra);
    let tap_island = mana_action(runner.state(), island, None);
    act(&mut runner, tap_island);
    act(&mut runner, ogre_activation(ogre));
    assert_payment_step(runner.state());
    assert_eq!(
        runner.state().players[0].life,
        17,
        "reach: one Ogre activation paid 3 life"
    );
    assert_eq!(
        red_in_pool(runner.state()),
        1,
        "reach: the pool holds one red"
    );
    (runner, ogre)
}

#[test]
fn surplus_ogre_activation_is_certified_once_the_pass_pays() {
    let (runner, ogre) = saturated_unsubstantiate(|_| {});
    assert_eq!(
        surplus_mana_activation(runner.state(), &ogre_activation(ogre)),
        Surplus
    );
}

#[test]
fn needed_ogre_activation_is_not_certified() {
    let (mut runner, island, ogre) = unsubstantiate_board(|_| {});
    let tap_island = mana_action(runner.state(), island, None);
    act(&mut runner, tap_island);
    assert_eq!(
        surplus_mana_activation(runner.state(), &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::NoFinisher),
        "the Island alone doesn't pay {{1}}{{U}}: the Ogre is needed"
    );
}

#[test]
fn priority_is_not_a_payment_step() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let ogre = add_ogre(&mut sc, P0);
    let runner = sc.build();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        surplus_mana_activation(runner.state(), &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::Carrier)
    );
}

#[test]
fn pass_is_not_a_mana_activation() {
    let (runner, _) = saturated_unsubstantiate(|_| {});
    assert_eq!(
        surplus_mana_activation(runner.state(), &GameAction::PassPriority),
        NotCertified(SurplusNotCertified::NotManaActivation)
    );
}

/// The certificate's direct reducer work: one apply when the pass doesn't pay
/// yet (the finisher check), three for a certified surplus (finisher,
/// activation, finisher after the activation).
#[test]
fn certificate_direct_apply_counts() {
    let (mut runner, island, ogre) = unsubstantiate_board(|_| {});
    let tap_island = mana_action(runner.state(), island, None);
    act(&mut runner, tap_island);
    let mut first = SurplusCertificateCounters::default();
    surplus_mana_activation_with_counters(runner.state(), &ogre_activation(ogre), &mut first);
    assert_eq!(first.applies, 1);

    act(&mut runner, ogre_activation(ogre));
    let mut saturated = SurplusCertificateCounters::default();
    assert_eq!(
        surplus_mana_activation_with_counters(
            runner.state(),
            &ogre_activation(ogre),
            &mut saturated
        ),
        Surplus
    );
    assert_eq!(saturated.applies, 3);

    let mut pass = SurplusCertificateCounters::default();
    surplus_mana_activation_with_counters(runner.state(), &GameAction::PassPriority, &mut pass);
    assert_eq!(pass.applies, 0);
}

// ---------------------------------------------------------------------------
// Shared drivers
// ---------------------------------------------------------------------------

const VILIS: &str = "Flying\n{B}, Pay 2 life: Target creature gets -1/-1 until end of turn.\nWhenever you lose life, draw that many cards. (Damage causes loss of life.)";
const OMNATH: &str = "You don't lose unspent green mana as steps and phases end.\nOmnath gets +1/+1 for each unspent green mana you have.";
const DIABOLIC_INTENT: &str = "As an additional cost to cast this spell, sacrifice a creature.\nSearch your library for a card, put that card into your hand, then shuffle.";
const EVACUATION: &str = "Return all creatures to their owners' hands.";
const BLOOD_CELEBRANT: &str = "{B}, Pay 1 life: Add one mana of any color.";
const TOXIC_DELUGE: &str = "As an additional cost to cast this spell, pay X life.\nAll creatures get -X/-X until end of turn.";
const PAINFUL_TRUTHS: &str = "Converge — You draw X cards and lose X life, where X is the number of colors of mana spent to cast this spell.";
const SUNTOUCHED_MYR: &str = "Sunburst (This creature enters with a +1/+1 counter on it for each color of mana spent to cast it.)";
const FIERY_ISLET: &str =
    "{T}, Pay 1 life: Add {U} or {R}.\n{1}, {T}, Sacrifice this land: Draw a card.";
const WASTES: &str = "{T}: Add {C}.";
const WORDS_OF_WISDOM: &str = "You draw two cards, then each other player draws a card.";
const HALL_OF_THE_BANDIT_LORD: &str = "Hall of the Bandit Lord enters tapped.\n{T}, Pay 3 life: Add {C}. If that mana is spent on a creature spell, it gains haste.";
const CHROMATIC_ORRERY: &str = "You may spend mana as though it were mana of any color.\n{T}: Add {C}{C}{C}{C}{C}.\n{5}, {T}: Draw a card for each color among permanents you control.";
const NIVMAGUS_ELEMENTAL: &str = "Exile an instant or sorcery spell you control: Put two +1/+1 counters on this creature. (That spell won't resolve.)";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

const LIBRARY: [&str; 6] = ["Card A", "Card B", "Card C", "Card D", "Card E", "Card F"];

fn add_wastes(sc: &mut GameScenario) -> ObjectId {
    sc.add_land_from_oracle(P0, "Wastes", WASTES).id()
}

fn add_islet(sc: &mut GameScenario) -> ObjectId {
    sc.add_land_from_oracle(P0, "Fiery Islet", FIERY_ISLET).id()
}

/// Prompt answers a cast needs before its payment step.
#[derive(Default)]
struct Answers {
    x: Option<u32>,
    target: Option<TargetRef>,
    cost_objects: Vec<ObjectId>,
}

/// Announce `spell` for a manual payment and answer its prompts until P0's
/// mana-payment step.
fn cast_to_payment(runner: &mut GameRunner, spell: ObjectId, answers: Answers) {
    cast(runner, spell, CastPaymentMode::Manual);
    for _ in 0..16 {
        let action = match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => return,
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX {
                value: answers.x.expect("the cast announces X"),
            },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: answers.target.clone(),
            },
            WaitingFor::PayCost { .. } => GameAction::SelectCards {
                cards: answers.cost_objects.clone(),
            },
            other => panic!("unexpected prompt before payment: {other:?}"),
        };
        act(runner, action);
    }
    panic!("the cast never reached its payment step");
}

/// Produce mana at the payment step: each source's engine-issued action.
fn produce(runner: &mut GameRunner, sources: &[(ObjectId, Option<ManaType>)]) {
    for &(source, color) in sources {
        let action = mana_action(runner.state(), source, color);
        act(runner, action);
    }
    assert_payment_step(runner.state());
}

fn certificate(runner: &GameRunner, action: &GameAction) -> SurplusManaActivation {
    surplus_mana_activation(runner.state(), action)
}

// ---------------------------------------------------------------------------
// Priority window (CR 117.3c) and deferral (CR 605.3a)
// ---------------------------------------------------------------------------

/// P5: an ordinary cast. After the pass, P0 holds priority with Evacuation on
/// the stack and can still activate the Ogre: the deferral window exists.
#[test]
fn evacuation_cast_leaves_a_priority_window_for_the_deferred_ogre() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let islands = [
        sc.add_basic_land(P0, ManaColor::Blue),
        sc.add_basic_land(P0, ManaColor::Blue),
    ];
    let ogre = add_ogre(&mut sc, P0);
    let evacuation = sc
        .add_spell_to_hand_from_oracle(P0, "Evacuation", true, EVACUATION)
        .with_mana_cost(cost(vec![ManaCostShard::Blue, ManaCostShard::Blue], 3))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, evacuation, Answers::default());
    produce(&mut runner, &[(islands[0], None), (islands[1], None)]);
    for _ in 0..3 {
        act(&mut runner, ogre_activation(ogre));
    }
    assert_payment_step(runner.state());
    assert_eq!(
        runner.state().players[0].life,
        11,
        "reach: three activations"
    );
    assert_eq!(
        red_in_pool(runner.state()),
        3,
        "reach: {{U}}{{U}}{{R}}{{R}}{{R}}"
    );

    assert_eq!(certificate(&runner, &ogre_activation(ogre)), Surplus);

    let mut passed = GameRunner::from_state(runner.state().clone());
    act(&mut passed, GameAction::PassPriority);
    assert_eq!(
        passed.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert!(
        passed
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == evacuation),
        "Evacuation is on the stack while P0 holds priority"
    );
    assert!(
        activatable_mana_actions_for_player(passed.state(), P0).contains(&ogre_activation(ogre)),
        "CR 605.3a: the Ogre can still be activated before Evacuation resolves"
    );
}

/// P4: Diabolic Intent's additional cost sacrifices the Ogre as the payment
/// finishes, so the activation cannot be deferred.
#[test]
fn diabolic_intent_sacrificing_the_ogre_refuses_deferral() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &["Lightning Bolt"]);
    sc.with_life(P1, 3);
    let swamp = sc.add_basic_land(P0, ManaColor::Black);
    let ogre = add_ogre(&mut sc, P0);
    let intent = sc
        .add_spell_to_hand_from_oracle(P0, "Diabolic Intent", false, DIABOLIC_INTENT)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 1))
        .id();
    let mut runner = sc.build();
    cast_to_payment(
        &mut runner,
        intent,
        Answers {
            cost_objects: vec![ogre],
            ..Answers::default()
        },
    );
    produce(&mut runner, &[(swamp, None)]);
    act(&mut runner, ogre_activation(ogre));
    assert_payment_step(runner.state());

    let mut passed = GameRunner::from_state(runner.state().clone());
    act(&mut passed, GameAction::PassPriority);
    assert_eq!(
        passed.state().objects[&ogre].zone,
        engine::types::zones::Zone::Graveyard,
        "reach: finishing the payment sacrifices the Ogre"
    );
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::DeferralUnavailable)
    );
}

/// T6 (a control for the same step): Toxic Deluge's X life leaves too little
/// to pay the Ogre's 3 life afterwards (CR 119.4).
#[test]
fn toxic_deluge_life_payment_refuses_deferral() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_life(P0, 6);
    let swamp = sc.add_basic_land(P0, ManaColor::Black);
    let wastes = [add_wastes(&mut sc), add_wastes(&mut sc)];
    let ogre = add_ogre(&mut sc, P0);
    let deluge = sc
        .add_spell_to_hand_from_oracle(P0, "Toxic Deluge", false, TOXIC_DELUGE)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 2))
        .id();
    let mut runner = sc.build();
    cast_to_payment(
        &mut runner,
        deluge,
        Answers {
            x: Some(4),
            ..Answers::default()
        },
    );
    produce(
        &mut runner,
        &[(swamp, None), (wastes[0], None), (wastes[1], None)],
    );
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::DeferralUnavailable)
    );
}

// ---------------------------------------------------------------------------
// Triggers, unspent-mana readers, choices (CR 603.2, CR 106.4)
// ---------------------------------------------------------------------------

/// P3: with Vilis, Broker of Blood out, each Ogre activation's life loss draws
/// cards — the activation queues a non-mana trigger.
#[test]
fn vilis_draw_trigger_refuses_the_certificate() {
    let (runner, ogre) = saturated_unsubstantiate(|sc| {
        sc.with_library_top(P0, &LIBRARY);
        sc.add_creature(P0, "Vilis, Broker of Blood", 8, 8)
            .from_oracle_text_with_keywords(&["Flying"], VILIS);
    });
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::QueuesTrigger)
    );
}

/// T5: Omnath reads unspent green mana, so floating mana is not inert.
#[test]
fn omnath_unspent_mana_reader_refuses_the_certificate() {
    let (runner, ogre) = saturated_unsubstantiate(|sc| {
        sc.add_creature(P0, "Omnath, Locus of Mana", 1, 1)
            .from_oracle_text(OMNATH);
    });
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::UnspentManaRead)
    );
}

/// T8 (refusal control): Blood Celebrant's mana opens a colour choice, so the
/// activation doesn't return to the payment step with one new unit.
#[test]
fn blood_celebrant_colour_choice_is_not_a_single_unit_return() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let swamps = [
        sc.add_basic_land(P0, ManaColor::Black),
        sc.add_basic_land(P0, ManaColor::Black),
    ];
    let celebrant = sc
        .add_creature(P0, "Blood Celebrant", 1, 1)
        .from_oracle_text(BLOOD_CELEBRANT)
        .id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    let mut runner = sc.build();
    cast_to_payment(
        &mut runner,
        spell,
        Answers {
            target: Some(TargetRef::Object(bears)),
            ..Answers::default()
        },
    );
    produce(&mut runner, &[(island, None), (swamps[0], None)]);
    let activation = mana_action(runner.state(), celebrant, None);
    assert_eq!(
        certificate(&runner, &activation),
        NotCertified(SurplusNotCertified::NotSingleUnitReturn)
    );
}

// ---------------------------------------------------------------------------
// Saturation (SM1-2) and spend equivalence
// ---------------------------------------------------------------------------

/// T4: Painful Truths (converge) paid {B}{C}{C}; Fiery Islet's {U} would add a
/// colour a directed spend could use.
#[test]
fn islet_blue_for_painful_truths_is_unsaturated() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &LIBRARY);
    let swamp = sc.add_basic_land(P0, ManaColor::Black);
    let wastes = [add_wastes(&mut sc), add_wastes(&mut sc)];
    let islet = add_islet(&mut sc);
    let truths = sc
        .add_spell_to_hand_from_oracle(P0, "Painful Truths", false, PAINFUL_TRUTHS)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 2))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, truths, Answers::default());
    produce(
        &mut runner,
        &[(swamp, None), (wastes[0], None), (wastes[1], None)],
    );
    let blue = mana_action(runner.state(), islet, Some(ManaType::Blue));
    assert_eq!(
        certificate(&runner, &blue),
        NotCertified(SurplusNotCertified::Unsaturated)
    );
}

/// T4b: Suntouched Myr (sunburst) paid {C}{C}{C}; a coloured unit is not
/// spend-equivalent to anything in the pool.
#[test]
fn islet_blue_for_suntouched_myr_is_unsaturated() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let wastes = [
        add_wastes(&mut sc),
        add_wastes(&mut sc),
        add_wastes(&mut sc),
    ];
    let islet = add_islet(&mut sc);
    let myr = sc
        .add_creature_to_hand_from_oracle(P0, "Suntouched Myr", 0, 0, SUNTOUCHED_MYR)
        .with_mana_cost(cost(vec![], 3))
        .as_artifact_creature()
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, myr, Answers::default());
    produce(
        &mut runner,
        &[(wastes[0], None), (wastes[1], None), (wastes[2], None)],
    );
    let blue = mana_action(runner.state(), islet, Some(ManaType::Blue));
    assert_eq!(
        certificate(&runner, &blue),
        NotCertified(SurplusNotCertified::Unsaturated)
    );
}

/// T7: two Fiery Islets each tapped for {R} (pinned). Their units are
/// spend-equivalent once both are tapped — the state the finisher spends from —
/// so the second {R} is a surplus.
#[test]
fn second_fiery_islet_red_is_surplus() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &LIBRARY);
    sc.with_library_top(P1, &LIBRARY);
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let islets = [add_islet(&mut sc), add_islet(&mut sc)];
    let wisdom = sc
        .add_spell_to_hand_from_oracle(P0, "Words of Wisdom", true, WORDS_OF_WISDOM)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, wisdom, Answers::default());
    produce(
        &mut runner,
        &[(island, None), (islets[0], Some(ManaType::Red))],
    );
    let second = mana_action(runner.state(), islets[1], Some(ManaType::Red));
    assert!(
        matches!(&second, GameAction::TapLandForMana { selection } if selection.ability_index.is_some()),
        "reach: the Islet's costed ability is an indexed TapLandForMana: {second:?}"
    );
    assert_eq!(certificate(&runner, &second), Surplus);
}

/// T4d: Divination with two Ogres. The surplus third activation's unit is
/// spend-equivalent to the other Ogre's.
#[test]
fn ogre_units_are_spend_equivalent_across_two_ogres() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &LIBRARY);
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let ogres = [add_ogre(&mut sc, P0), add_ogre(&mut sc, P0)];
    let divination = sc
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 2))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, divination, Answers::default());
    produce(&mut runner, &[(island, None)]);
    act(&mut runner, ogre_activation(ogres[0]));
    act(&mut runner, ogre_activation(ogres[1]));
    assert_eq!(red_in_pool(runner.state()), 2);
    assert_eq!(certificate(&runner, &ogre_activation(ogres[0])), Surplus);
    assert_eq!(certificate(&runner, &ogre_activation(ogres[1])), Surplus);
}

/// E4: Hall of the Bandit Lord's {C} carries a haste grant (CR 106.6), so it
/// is refused before saturation.
#[test]
fn hall_of_the_bandit_lord_rider_unit_is_refused() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let ogre = add_ogre(&mut sc, P0);
    let hall = sc
        .add_land_from_oracle(P0, "Hall of the Bandit Lord", HALL_OF_THE_BANDIT_LORD)
        .as_legendary()
        .id();
    let merfolk = sc
        .add_creature_to_hand(P0, "Coral Merfolk", 2, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, merfolk, Answers::default());
    produce(&mut runner, &[(island, None)]);
    act(&mut runner, ogre_activation(ogre));
    let tap_hall = mana_action(runner.state(), hall, None);

    let mut probe = GameRunner::from_state(runner.state().clone());
    act(&mut probe, tap_hall.clone());
    let hall_unit = probe.state().players[0]
        .mana_pool
        .units()
        .find(|unit| unit.source_id == hall)
        .expect("reach: the Hall's unit is in the pool");
    assert!(
        !hall_unit.grants.is_empty(),
        "reach: the Hall's {{C}} carries its haste grant"
    );
    assert_eq!(
        certificate(&runner, &tap_hall),
        NotCertified(SurplusNotCertified::UnitHasRiders)
    );
}

// ---------------------------------------------------------------------------
// Spend permission (CR 609.4b) and the default finisher
// ---------------------------------------------------------------------------

fn orrery_painful_truths(
    produce_island: bool,
    ogre_activations: usize,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &LIBRARY);
    sc.add_artifact_from_oracle(P0, "Chromatic Orrery", CHROMATIC_ORRERY)
        .as_legendary();
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let ogre = add_ogre(&mut sc, P0);
    let truths = sc
        .add_spell_to_hand_from_oracle(P0, "Painful Truths", false, PAINFUL_TRUTHS)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 2))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, truths, Answers::default());
    if produce_island {
        produce(&mut runner, &[(island, None)]);
    }
    for _ in 0..ogre_activations {
        act(&mut runner, ogre_activation(ogre));
    }
    assert_payment_step(runner.state());
    (runner, ogre, truths)
}

fn colours_spent(runner: &GameRunner, spell: ObjectId) -> usize {
    runner.state().objects[&spell]
        .colors_spent_to_cast
        .distinct_colors()
}

/// Q1: Chromatic Orrery lets {U}{R}{R} pay Painful Truths' {2}{B}. Another red
/// is payment-sensitive under the permission: directed {R}{R}{R} reads one
/// colour where the default finisher reads two.
#[test]
fn chromatic_orrery_permission_refuses_the_certificate() {
    let (runner, ogre, truths) = orrery_painful_truths(true, 2);
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::SpendPermission)
    );

    let mut default_finish = GameRunner::from_state(runner.state().clone());
    act(&mut default_finish, ogre_activation(ogre));
    act(&mut default_finish, GameAction::PassPriority);
    assert_eq!(colours_spent(&default_finish, truths), 2);

    let mut directed = GameRunner::from_state(runner.state().clone());
    act(&mut directed, ogre_activation(ogre));
    let reds: Vec<_> = directed.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.color == ManaType::Red)
        .map(|unit| unit.pip_id)
        .collect();
    assert_eq!(reds.len(), 3);
    for pip_id in reds {
        act(&mut directed, GameAction::SpendPoolMana { pip_id });
    }
    act(&mut directed, GameAction::PassPriority);
    assert_eq!(
        colours_spent(&directed, truths),
        1,
        "CR 609.4b: the permission lets three red pay {{2}}{{B}}; converge reads one colour"
    );
}

/// Q1-red: the same permission over an all-red pool.
#[test]
fn chromatic_orrery_permission_refuses_an_all_red_pool() {
    let (runner, ogre, _) = orrery_painful_truths(false, 3);
    assert_eq!(red_in_pool(runner.state()), 3);
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::SpendPermission)
    );
}

fn nivmagus_board(with_bolt: bool) -> (GameRunner, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let ogre = add_ogre(&mut sc, P0);
    let nivmagus = sc
        .add_creature_to_hand_from_oracle(P0, "Nivmagus Elemental", 1, 2, NIVMAGUS_ELEMENTAL)
        .with_mana_cost(cost(vec![ManaCostShard::BlueRed], 0))
        .id();
    if with_bolt {
        sc.add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
            .with_mana_cost(cost(vec![ManaCostShard::Red], 0));
    }
    let mut runner = sc.build();
    cast_to_payment(&mut runner, nivmagus, Answers::default());
    produce(&mut runner, &[(island, None)]);
    act(&mut runner, ogre_activation(ogre));
    assert_payment_step(runner.state());
    (runner, ogre)
}

/// R3-Q4: Nivmagus Elemental's {U/R} — the default finisher pays it from the
/// larger pool colour, so one more red flips the payment from blue to red
/// while capacity (1) is already met.
#[test]
fn nivmagus_hybrid_flip_refuses_as_finisher_divergence() {
    let (runner, ogre) = nivmagus_board(false);
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::FinisherDiverges)
    );
}

/// R3-Q4 control: Lightning Bolt's {R} demand makes blue pay the hybrid in
/// both finishers, so the extra red is a surplus.
#[test]
fn nivmagus_with_bolt_in_hand_keeps_the_finisher() {
    let (runner, ogre) = nivmagus_board(true);
    assert_eq!(certificate(&runner, &ogre_activation(ogre)), Surplus);
}

// ---------------------------------------------------------------------------
// Priority after a cast during resolution (CR 608.2g), Treasure readers,
// graveyard-cast permissions, granted land abilities, ability roots
// ---------------------------------------------------------------------------

const ELECTRODOMINANCE: &str = "Electrodominance deals X damage to any target. You may cast a spell with mana value X or less from your hand without paying its mana cost.";
const INTO_THE_ROIL: &str = "Kicker {1}{U} (You may pay an additional {1}{U} as you cast this spell.)\nReturn target nonland permanent to its owner's hand. If this spell was kicked, draw a card.";
const TREASURE: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";
const JADED_SELL_SWORD: &str = "When this creature enters, if mana from a Treasure was spent to cast it, it gains first strike and haste until end of turn.";
const WHISPERSTEEL_DAGGER: &str = "Equipped creature gets +2/+0.\nWhenever equipped creature deals combat damage to a player, you may cast a creature spell from that player's graveyard this turn, and you may spend mana as though it were mana of any color to cast that spell.\nEquip {3}";
const WOODLAND_WANDERER: &str = "Vigilance, trample\nConverge — This creature enters with a +1/+1 counter on it for each color of mana spent to cast it.";
const MANASCAPE_REFRACTOR: &str = "This artifact enters tapped.\nThis artifact has all activated abilities of all lands on the battlefield.\nYou may spend mana as though it were mana of any color to pay the activation costs of this artifact's abilities.";
const TWIDDLE: &str = "You may tap or untap target artifact, creature, or land.";
const RUMBLING_BALOTH: &str = "";
const JEWELED_AMULET: &str = "{1}, {T}: Put a charge counter on this artifact. Note the type of mana spent to pay this activation cost. Activate only if there are no charge counters on this artifact.\n{T}, Remove a charge counter from this artifact: Add one mana of this artifact's last noted type.";
const JETMIRS_FIXER: &str = "{R}{G}: This creature gets +1/+1 until end of turn. If mana from a Treasure was spent to activate this ability, put a +1/+1 counter on this creature instead.";

/// Pass priority until `done` holds, choosing nothing else.
fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameState) -> bool) {
    for _ in 0..24 {
        if done(runner.state()) {
            return;
        }
        act(runner, GameAction::PassPriority);
    }
    panic!(
        "never reached the expected state: {:?}",
        runner.state().waiting_for
    );
}

/// Q2: on P1's turn, P0's Electrodominance (X=2) lets P0 cast Into the Roil
/// during its resolution. CR 608.2g: no player receives priority after that
/// cast, so after Into the Roil's kicker payment the deferral window doesn't
/// exist.
#[test]
fn cast_during_resolution_has_no_priority_window() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::End);
    sc.with_library_top(P0, &LIBRARY);
    sc.with_library_top(P1, &LIBRARY);
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mountains: Vec<_> = (0..4)
        .map(|_| sc.add_basic_land(P0, ManaColor::Red))
        .collect();
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let ogre = add_ogre(&mut sc, P0);
    let electrodominance = sc
        .add_spell_to_hand_from_oracle(P0, "Electrodominance", true, ELECTRODOMINANCE)
        .with_mana_cost(cost(
            vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            0,
        ))
        .id();
    let roil = sc
        .add_spell_to_hand_from_oracle(P0, "Into the Roil", true, INTO_THE_ROIL)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    let mut runner = sc.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    cast_to_payment(
        &mut runner,
        electrodominance,
        Answers {
            x: Some(2),
            target: Some(TargetRef::Player(P1)),
            ..Answers::default()
        },
    );
    let sources: Vec<_> = mountains.iter().map(|&mountain| (mountain, None)).collect();
    produce(&mut runner, &sources);
    act(&mut runner, GameAction::PassPriority);
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. })
    });
    act(
        &mut runner,
        GameAction::DecideOptionalEffect { accept: true },
    );
    act(&mut runner, GameAction::SelectCards { cards: vec![roil] });
    act(
        &mut runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears)),
        },
    );
    act(&mut runner, GameAction::DecideOptionalCost { pay: true });
    produce(&mut runner, &[(island, None)]);
    act(&mut runner, ogre_activation(ogre));
    assert_eq!(
        runner.state().pending_cast.as_ref().map(|pending| pending.object_id),
        Some(roil),
        "reach: Into the Roil's kicker {{1}}{{U}} is being paid during Electrodominance's resolution"
    );

    let mut passed = GameRunner::from_state(runner.state().clone());
    act(&mut passed, GameAction::PassPriority);
    assert_ne!(
        passed.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach: P0 doesn't hold priority right after the cast during resolution"
    );
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::NoPriorityWindow)
    );
}

/// Q3: four Treasure {R} plus three Wastes {C} pay Jaded Sell-Sword, whose
/// rider reads mana from a Treasure. An Ogre {R} is not spend-equivalent to a
/// Treasure {R} (different source characteristics), so it is unsaturated.
#[test]
fn ogre_red_is_not_equivalent_to_treasure_red() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let treasures: Vec<_> = (0..4)
        .map(|_| {
            sc.add_artifact_from_oracle(P0, "Treasure", TREASURE)
                .with_subtypes(vec!["Treasure"])
                .id()
        })
        .collect();
    let wastes = [
        add_wastes(&mut sc),
        add_wastes(&mut sc),
        add_wastes(&mut sc),
    ];
    let ogre = add_ogre(&mut sc, P0);
    let sell_sword = sc
        .add_creature_to_hand_from_oracle(P0, "Jaded Sell-Sword", 4, 3, JADED_SELL_SWORD)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, sell_sword, Answers::default());
    for &treasure in &treasures {
        act(
            &mut runner,
            GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            },
        );
        act(
            &mut runner,
            GameAction::ChooseManaColor {
                choice: engine::types::game_state::ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            },
        );
    }
    produce(
        &mut runner,
        &[(wastes[0], None), (wastes[1], None), (wastes[2], None)],
    );
    let treasure_red = runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.color == ManaType::Red && treasures.contains(&unit.source_id))
        .count();
    assert_eq!(
        treasure_red, 4,
        "reach: four Treasure-sourced red in the pool"
    );
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::Unsaturated)
    );
}

/// R3-Q5: Whispersteel Dagger's combat-damage trigger lets P0 cast Woodland
/// Wanderer from P1's graveyard spending mana as any colour (CR 609.4b). No
/// static grants that permission; the cast's elected graveyard latch does.
#[test]
fn whispersteel_dagger_latch_permission_refuses_the_certificate() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &LIBRARY);
    sc.with_library_top(P1, &LIBRARY);
    let bear = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let dagger = sc
        .add_artifact_from_oracle(P0, "Whispersteel Dagger", WHISPERSTEEL_DAGGER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let wastes: Vec<_> = (0..3).map(|_| add_wastes(&mut sc)).collect();
    // Untapped Mountains keep the graveyard cast affordable for the offer; the
    // payment itself is made with the Ogre.
    for _ in 0..4 {
        sc.add_basic_land(P0, ManaColor::Red);
    }
    let ogre = add_ogre(&mut sc, P0);
    let wanderer = sc
        .add_creature_to_graveyard(P1, "Woodland Wanderer", 2, 2)
        .from_oracle_text_with_keywords(&["Vigilance", "Trample"], WOODLAND_WANDERER)
        .with_mana_cost(cost(vec![ManaCostShard::Green], 3))
        .id();
    let mut runner = sc.build();
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: dagger,
            ability_index: 0,
            payment_mode: CastPaymentMode::Manual,
        },
    );
    act(
        &mut runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        },
    );
    let wastes_sources: Vec<_> = wastes.iter().map(|&land| (land, None)).collect();
    produce(&mut runner, &wastes_sources);
    act(&mut runner, GameAction::PassPriority);
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    act(
        &mut runner,
        GameAction::DeclareAttackers {
            attacks: vec![(bear, engine::game::combat::AttackTarget::Player(P1))],
            bands: vec![],
        },
    );
    pass_until(&mut runner, |state| {
        state.phase == Phase::PostCombatMain
            && state.waiting_for == WaitingFor::Priority { player: P0 }
    });
    assert_eq!(
        runner.state().players[1].life,
        16,
        "reach: the equipped Bears hit P1"
    );

    cast_to_payment(&mut runner, wanderer, Answers::default());
    for _ in 0..4 {
        act(&mut runner, ogre_activation(ogre));
    }
    assert_payment_step(runner.state());
    assert_eq!(
        red_in_pool(runner.state()),
        4,
        "reach: {{R}}{{R}}{{R}}{{R}} from the Ogre"
    );
    assert_eq!(
        engine::game::static_abilities::player_mana_spend_permission_for_spell_object(
            runner.state(),
            P0,
            wanderer
        ),
        None,
        "reach: no static permission"
    );
    assert_eq!(
        engine::game::casting::cast_payment_mana_spend_permission(runner.state(), P0, wanderer),
        Some(engine::types::ability::ManaSpendPermission::AnyColor),
        "reach: the cast's graveyard latch carries the any-colour concession"
    );
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::SpendPermission)
    );
}

/// R3-Q6: two Manascape Refractors share Hall of the Bandit Lord's and Wastes'
/// abilities. Refractor A makes two plain {C} (untapped by Twiddle between);
/// Refractor B's copy of the Hall ability makes a {C} carrying the haste grant.
#[test]
fn refractor_hall_unit_with_a_grant_is_refused() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let refractor_a = sc
        .add_artifact_from_oracle(P0, "Manascape Refractor", MANASCAPE_REFRACTOR)
        .id();
    let refractor_b = sc
        .add_artifact_from_oracle(P0, "Manascape Refractor", MANASCAPE_REFRACTOR)
        .id();
    sc.add_land_from_oracle(P0, "Hall of the Bandit Lord", HALL_OF_THE_BANDIT_LORD)
        .as_legendary();
    add_wastes(&mut sc);
    let forests = [
        sc.add_basic_land(P0, ManaColor::Green),
        sc.add_basic_land(P0, ManaColor::Green),
    ];
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let twiddle = sc
        .add_spell_to_hand_from_oracle(P0, "Twiddle", true, TWIDDLE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 0))
        .id();
    let baloth = sc
        .add_creature_to_hand_from_oracle(P0, "Rumbling Baloth", 4, 4, RUMBLING_BALOTH)
        .with_mana_cost(cost(vec![ManaCostShard::Green, ManaCostShard::Green], 2))
        .id();
    let mut runner = sc.build();
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::flush_layers(runner.state_mut());

    // Ability 1 of each Refractor is the granted Wastes ability; 0 is the Hall's.
    let wastes_ability = |refractor| GameAction::ActivateAbility {
        source_id: refractor,
        ability_index: 1,
        payment_mode: CastPaymentMode::Auto,
    };
    let hall_ability = |refractor| GameAction::ActivateAbility {
        source_id: refractor,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    };
    act(&mut runner, wastes_ability(refractor_a));
    cast_to_payment(
        &mut runner,
        twiddle,
        Answers {
            target: Some(TargetRef::Object(refractor_a)),
            ..Answers::default()
        },
    );
    produce(&mut runner, &[(island, None)]);
    act(&mut runner, GameAction::PassPriority);
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. })
    });
    act(
        &mut runner,
        GameAction::DecideOptionalEffect { accept: true },
    );
    act(&mut runner, GameAction::ChooseBranch { index: 1 });
    assert!(
        !runner.state().objects[&refractor_a].tapped,
        "reach: Twiddle untapped Refractor A"
    );
    act(&mut runner, wastes_ability(refractor_a));

    cast_to_payment(&mut runner, baloth, Answers::default());
    produce(&mut runner, &[(forests[0], None), (forests[1], None)]);
    let plain: Vec<_> = runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.source_id == refractor_a)
        .collect();
    assert_eq!(plain.len(), 2, "reach: two {{C}} from Refractor A");
    assert!(plain.iter().all(|unit| unit.grants.is_empty()));
    for unit in plain {
        act(
            &mut runner,
            GameAction::SpendPoolMana {
                pip_id: unit.pip_id,
            },
        );
    }

    let mut probe = GameRunner::from_state(runner.state().clone());
    act(&mut probe, hall_ability(refractor_b));
    let hall_unit = probe.state().players[0]
        .mana_pool
        .units()
        .find(|unit| unit.source_id == refractor_b)
        .expect("reach: Refractor B's Hall ability adds a unit");
    assert!(
        !hall_unit.grants.is_empty(),
        "reach: the granted Hall unit carries haste"
    );
    assert_eq!(
        certificate(&runner, &hall_ability(refractor_b)),
        NotCertified(SurplusNotCertified::UnitHasRiders)
    );
}

/// E1: an activated ability's payment (CR 602.2b) is not certified — Jeweled
/// Amulet notes the mana type paid, and Jetmir's Fixer reads Treasure mana.
#[test]
fn ability_roots_are_not_certified() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let amulet = sc
        .add_artifact_from_oracle(P0, "Jeweled Amulet", JEWELED_AMULET)
        .id();
    let fixer = sc
        .add_creature_from_oracle(P0, "Jetmir's Fixer", 2, 2, JETMIRS_FIXER)
        .id();
    let forest = sc.add_basic_land(P0, ManaColor::Green);
    let ogre = add_ogre(&mut sc, P0);
    let mut runner = sc.build();

    for (source, produce_first) in [(amulet, None), (fixer, Some(forest))] {
        let mut activation = GameRunner::from_state(runner.state().clone());
        act(
            &mut activation,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Manual,
            },
        );
        if let Some(land) = produce_first {
            produce(&mut activation, &[(land, None)]);
        }
        act(&mut activation, ogre_activation(ogre));
        assert_payment_step(activation.state());
        assert_eq!(
            certificate(&activation, &ogre_activation(ogre)),
            NotCertified(SurplusNotCertified::AbilityRoot),
            "{source:?}"
        );
    }
    let _ = &mut runner;
}
