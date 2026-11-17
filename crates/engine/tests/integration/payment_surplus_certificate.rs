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
    finalizes_in_one_step, surplus_mana_activation, surplus_mana_activation_with_counters,
    units_spend_equivalent, witness_payment_continuations, AppliedActivation,
    PaymentSurplusDecision, PaymentSurplusSeeds, SurplusCertificateCounters, SurplusManaActivation,
    SurplusNotCertified, WitnessedStepTrace,
};
use engine::game::casting::activated_ability_definitions;
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityDefinition;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

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
/// activation, finisher after the activation) plus the one offered-mana-action
/// enumeration after the finisher.
#[test]
fn certificate_direct_apply_counts() {
    let (mut runner, island, ogre) = unsubstantiate_board(|_| {});
    let tap_island = mana_action(runner.state(), island, None);
    act(&mut runner, tap_island);
    let mut first = SurplusCertificateCounters::default();
    surplus_mana_activation_with_counters(runner.state(), &ogre_activation(ogre), &mut first);
    assert_eq!((first.applies, first.offer_enumerations), (1, 0));

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
    assert_eq!((saturated.applies, saturated.offer_enumerations), (3, 1));

    // Seeded with the root witness's successor, the certificate reads the
    // activation instead of applying it: the finisher and the finisher after.
    let state = runner.state();
    let mut seeded = SurplusCertificateCounters::default();
    let decision = PaymentSurplusDecision::prepare_with_counters(
        state,
        Some(witnessed_seed(state, &ogre_activation(ogre))),
        &mut seeded,
    );
    assert_eq!(
        decision.certify_with_counters(state, &ogre_activation(ogre), &mut seeded),
        Surplus
    );
    assert_eq!((seeded.applies, seeded.offer_enumerations), (2, 1));

    let mut pass = SurplusCertificateCounters::default();
    surplus_mana_activation_with_counters(runner.state(), &GameAction::PassPriority, &mut pass);
    assert_eq!((pass.applies, pass.offer_enumerations), (0, 0));
}

/// The root payment witness's accepted successor of `activation`, as a
/// certificate seed. Reach: the witness accepts it, and its step is the one a
/// fresh simulation apply takes — the same events, deferred-trigger queue
/// length before, payment step, pool and deferred queue after.
fn witnessed_seed(state: &GameState, activation: &GameAction) -> PaymentSurplusSeeds {
    let accepted = witness_payment_continuations(state, std::slice::from_ref(activation))
        .successors
        .into_iter()
        .next()
        .flatten()
        .unwrap_or_else(|| panic!("reach: the root witness accepts {activation:?}"));
    let mut fresh = state.clone();
    let deferred_triggers_before = fresh.deferred_triggers.len();
    let result = apply_as_current_for_simulation(&mut fresh, activation.clone())
        .expect("a fresh apply of the activation succeeds");
    assert_eq!(
        accepted.trace,
        WitnessedStepTrace {
            events: result.events,
            deferred_triggers_before,
        },
        "the witnessed step trace is a fresh apply's"
    );
    assert_eq!(accepted.state.waiting_for, fresh.waiting_for);
    assert_eq!(
        accepted.state.deferred_triggers.len(),
        fresh.deferred_triggers.len()
    );
    for (witnessed, fresh) in accepted.state.players.iter().zip(&fresh.players) {
        assert_eq!(
            witnessed.mana_pool.units().collect::<Vec<_>>(),
            fresh.mana_pool.units().collect::<Vec<_>>(),
            "the witnessed pool is a fresh apply's"
        );
    }
    PaymentSurplusSeeds::new(
        state,
        vec![(
            activation.clone(),
            AppliedActivation {
                state: accepted.state,
                trace: accepted.trace,
            },
        )],
    )
}

/// Seeded and unseeded certificates agree — with each other and the one-shot —
/// and the seed replaces exactly the activation's apply whenever the
/// certificate reaches that step (`seed_saves`).
fn assert_seeded_agrees(
    state: &GameState,
    activation: &GameAction,
    expected: SurplusManaActivation,
    seed_saves: usize,
) {
    let mut unseeded_work = SurplusCertificateCounters::default();
    let unseeded = PaymentSurplusDecision::prepare_with_counters(state, None, &mut unseeded_work)
        .certify_with_counters(state, activation, &mut unseeded_work);
    let mut seeded_work = SurplusCertificateCounters::default();
    let seeded = PaymentSurplusDecision::prepare_with_counters(
        state,
        Some(witnessed_seed(state, activation)),
        &mut seeded_work,
    )
    .certify_with_counters(state, activation, &mut seeded_work);
    assert_eq!(unseeded, expected, "unseeded {activation:?}");
    assert_eq!(seeded, expected, "seeded {activation:?}");
    assert_eq!(surplus_mana_activation(state, activation), expected);
    assert_eq!(
        unseeded_work.applies - seeded_work.applies,
        seed_saves,
        "{activation:?}"
    );
}

/// The seeded and unseeded certificates give identical verdicts across the
/// refusal classes the activation step feeds: the CR 603.2 trigger check
/// (Vilis), a colour choice (Blood Celebrant), deferral (Q7), finisher
/// divergence (Nivmagus) and Surplus, for both the `ActivateAbility` and the
/// indexed `TapLandForMana` forms.
#[test]
fn seeded_and_unseeded_certificates_agree() {
    let (runner, ogre) = saturated_unsubstantiate(|_| {});
    assert_seeded_agrees(runner.state(), &ogre_activation(ogre), Surplus, 1);

    let (runner, ogre) = saturated_unsubstantiate(|sc| {
        sc.with_library_top(P0, &LIBRARY);
        sc.add_creature(P0, "Vilis, Broker of Blood", 8, 8)
            .from_oracle_text_with_keywords(&["Flying"], VILIS);
    });
    assert_seeded_agrees(
        runner.state(),
        &ogre_activation(ogre),
        NotCertified(SurplusNotCertified::QueuesTrigger),
        1,
    );

    let (runner, celebrant) = celebrant_board();
    assert_seeded_agrees(
        runner.state(),
        &celebrant,
        NotCertified(SurplusNotCertified::NotSingleUnitReturn),
        1,
    );

    let (runner, islet) = islet_board();
    assert_seeded_agrees(runner.state(), &islet, Surplus, 1);

    for (hand, expected) in [
        (
            NivmagusHand::Alone,
            NotCertified(SurplusNotCertified::FinisherDiverges),
        ),
        (NivmagusHand::WithLightningBolt, Surplus),
    ] {
        let (runner, ogre) = nivmagus_board(hand);
        assert_seeded_agrees(runner.state(), &ogre_activation(ogre), expected, 1);
    }

    for (sacrifice, expected, seed_saves) in [
        (
            HarrowSacrifice::Ruins,
            NotCertified(SurplusNotCertified::DeferralUnavailable),
            0,
        ),
        (HarrowSacrifice::Wastes, Surplus, 1),
    ] {
        let board = refractor_harrow_board(sacrifice);
        let b_red = GameAction::ActivateAbility {
            source_id: board.refractor_b,
            ability_index: board.ruins_red_index,
            payment_mode: CastPaymentMode::Auto,
        };
        assert_seeded_agrees(board.runner.state(), &b_red, expected, seed_saves);
    }
}

/// The certificate reads the seed rather than applying the activation: a seed
/// whose successor isn't the activation's (the decision's own state, with no
/// new unit) changes the verdict. Seeds made for another decision are dropped.
#[test]
fn certificate_reads_the_seeded_successor() {
    let (runner, ogre) = saturated_unsubstantiate(|_| {});
    let state = runner.state();
    let misread = PaymentSurplusSeeds::new(
        state,
        vec![(
            ogre_activation(ogre),
            AppliedActivation {
                state: state.clone(),
                trace: WitnessedStepTrace {
                    events: Vec::new(),
                    deferred_triggers_before: state.deferred_triggers.len(),
                },
            },
        )],
    );
    assert_eq!(
        PaymentSurplusDecision::prepare_seeded(state, misread)
            .certify(state, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::NotSingleUnitReturn)
    );

    let mut later = GameRunner::from_state(state.clone());
    act(&mut later, ogre_activation(ogre));
    let foreign = witnessed_seed(state, &ogre_activation(ogre));
    let mut work = SurplusCertificateCounters::default();
    let decision =
        PaymentSurplusDecision::prepare_with_counters(later.state(), Some(foreign), &mut work);
    assert_eq!(
        decision.certify_with_counters(later.state(), &ogre_activation(ogre), &mut work),
        Surplus
    );
    assert_eq!(
        work.applies, 3,
        "a seed for another decision is dropped, so the activation is applied"
    );
}

/// A prepared decision does the finisher and the offered-mana-action
/// enumeration once; each activation certified against it adds two direct
/// applications and no enumeration. A preparation certifies nothing for a
/// state it wasn't made for.
#[test]
fn prepared_decision_does_the_finisher_work_once() {
    let mut second = None;
    let (runner, ogre) = saturated_unsubstantiate(|sc| second = Some(add_ogre(sc, P0)));
    let second = second.expect("reach: a second Ogre");
    let state = runner.state();

    let mut preparation = SurplusCertificateCounters::default();
    let decision = PaymentSurplusDecision::prepare_with_counters(state, None, &mut preparation);
    assert_eq!(
        (preparation.applies, preparation.offer_enumerations),
        (1, 1)
    );
    for source in [ogre, second] {
        let mut per_activation = SurplusCertificateCounters::default();
        assert_eq!(
            decision.certify_with_counters(state, &ogre_activation(source), &mut per_activation),
            Surplus,
            "{source:?}"
        );
        assert_eq!(
            (per_activation.applies, per_activation.offer_enumerations),
            (2, 0)
        );
        assert_eq!(
            decision.certify(state, &ogre_activation(source)),
            surplus_mana_activation(state, &ogre_activation(source)),
            "the prepared and one-shot certificates agree"
        );
    }

    let mut later = GameRunner::from_state(state.clone());
    act(&mut later, ogre_activation(second));
    assert!(!decision.prepared_for(later.state()));
    assert_eq!(
        decision.certify(later.state(), &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::Carrier)
    );
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
            // CR 601.2b: decline every optional additional cost (kicker).
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { index: 1 },
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

/// The T8 board: the Island and a Swamp pay Unsubstantiate's {1}{U}. Returns
/// Blood Celebrant's mana action.
fn celebrant_board() -> (GameRunner, GameAction) {
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
    (runner, activation)
}

/// T8 (refusal control): Blood Celebrant's mana opens a colour choice, so the
/// activation doesn't return to the payment step with one new unit.
#[test]
fn blood_celebrant_colour_choice_is_not_a_single_unit_return() {
    let (runner, activation) = celebrant_board();
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

/// The T7 board: the Island and the first Fiery Islet's {R} (pinned by the
/// payment step) pay Words of Wisdom. Returns the second Islet's {R} action.
fn islet_board() -> (GameRunner, GameAction) {
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
    (runner, second)
}

/// T7: two Fiery Islets each tapped for {R} (pinned). Their units are
/// spend-equivalent once both are tapped — the state the finisher spends from —
/// so the second {R} is a surplus.
#[test]
fn second_fiery_islet_red_is_surplus() {
    let (runner, second) = islet_board();
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

/// What else P0 holds on the Nivmagus board.
#[derive(Clone, Copy)]
enum NivmagusHand {
    /// Nivmagus Elemental is the only card in hand.
    Alone,
    /// Lightning Bolt's {R} demand joins it.
    WithLightningBolt,
}

fn nivmagus_board(hand: NivmagusHand) -> (GameRunner, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let ogre = add_ogre(&mut sc, P0);
    let nivmagus = sc
        .add_creature_to_hand_from_oracle(P0, "Nivmagus Elemental", 1, 2, NIVMAGUS_ELEMENTAL)
        .with_mana_cost(cost(vec![ManaCostShard::BlueRed], 0))
        .id();
    match hand {
        NivmagusHand::Alone => {}
        NivmagusHand::WithLightningBolt => {
            sc.add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
                .with_mana_cost(cost(vec![ManaCostShard::Red], 0));
        }
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
    let (runner, ogre) = nivmagus_board(NivmagusHand::Alone);
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::FinisherDiverges)
    );
}

/// R3-Q4 control: Lightning Bolt's {R} demand makes blue pay the hybrid in
/// both finishers, so the extra red is a surplus.
#[test]
fn nivmagus_with_bolt_in_hand_keeps_the_finisher() {
    let (runner, ogre) = nivmagus_board(NivmagusHand::WithLightningBolt);
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
    act(&mut runner, GameAction::DecideOptionalCost { index: 0 });
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

/// Q3 (a refusal control): four Treasure {R} plus three Wastes {C} pay Jaded
/// Sell-Sword, whose rider reads mana from a Treasure. An Ogre {R} is not
/// spend-equivalent to a Treasure {R}: a Treasure's unit already carries the
/// could-produce-two-or-more-colours marker, so the shapes differ before the
/// source characteristics are compared (Q3b isolates those).
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

const RAMUNAP_RUINS: &str = "{T}: Add {C}.\n{T}, Pay 1 life: Add {R}.\n{2}{R}{R}, {T}, Sacrifice a Desert: This land deals 2 damage to each opponent.";
const CATACLYSMIC_PROSPECTING: &str = "Cataclysmic Prospecting deals X damage to each creature. For each mana from a Desert spent to cast this spell, create a tapped Treasure token.";

/// Q3b: two Ramunap Ruins (Deserts) pay Cataclysmic Prospecting's {R}{R}
/// (X=0). An Ogre {R} has the same mana shape as a Desert {R} but not the same
/// source characteristics, which the spell reads ("for each mana from a
/// Desert"): a directed spend of the Ogre's red would make one Treasure fewer.
#[test]
fn ogre_red_is_not_equivalent_to_desert_red() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let deserts: Vec<_> = (0..2)
        .map(|_| {
            sc.add_land_from_oracle(P0, "Ramunap Ruins", RAMUNAP_RUINS)
                .with_subtypes(vec!["Desert"])
                .id()
        })
        .collect();
    let ogre = add_ogre(&mut sc, P0);
    let prospecting = sc
        .add_spell_to_hand_from_oracle(
            P0,
            "Cataclysmic Prospecting",
            false,
            CATACLYSMIC_PROSPECTING,
        )
        .with_mana_cost(cost(
            vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            0,
        ))
        .id();
    let mut runner = sc.build();
    cast_to_payment(
        &mut runner,
        prospecting,
        Answers {
            x: Some(0),
            ..Answers::default()
        },
    );
    produce(
        &mut runner,
        &[
            (deserts[0], Some(ManaType::Red)),
            (deserts[1], Some(ManaType::Red)),
        ],
    );
    let desert_red = runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.color == ManaType::Red && deserts.contains(&unit.source_id))
        .collect::<Vec<_>>();
    assert_eq!(
        desert_red.len(),
        2,
        "reach: two Desert-sourced red in the pool"
    );
    // CR 118.3a: pin both Desert units, so the default finisher spends them
    // with or without the Ogre's red and only the certificate's equivalence
    // can tell the two pools apart.
    for unit in &desert_red {
        act(
            &mut runner,
            GameAction::SpendPoolMana {
                pip_id: unit.pip_id,
            },
        );
    }
    let mut ogre_unit_probe = GameRunner::from_state(runner.state().clone());
    act(&mut ogre_unit_probe, ogre_activation(ogre));
    let ogre_red = ogre_unit_probe.state().players[0]
        .mana_pool
        .units()
        .find(|unit| unit.source_id == ogre)
        .expect("reach: the Ogre adds {R}");
    let (mut desert_shape, _) = desert_red[0].clone().into_shape_and_pip();
    let (mut ogre_shape, _) = ogre_red.into_shape_and_pip();
    desert_shape.source_id = ogre;
    ogre_shape.source_id = ogre;
    assert_eq!(
        desert_shape, ogre_shape,
        "reach: only the source characteristics tell the two reds apart"
    );
    assert_eq!(
        certificate(&runner, &ogre_activation(ogre)),
        NotCertified(SurplusNotCertified::Unsaturated)
    );
}

const HARROW: &str = "As an additional cost to cast this spell, sacrifice a land.\nSearch your library for up to two basic land cards, put them onto the battlefield, then shuffle.";

/// The Q7 board's ids and the Ruins red ability's index on each Refractor.
struct RefractorHarrow {
    runner: GameRunner,
    refractor_b: ObjectId,
    ruins: ObjectId,
    forest: ObjectId,
    sacrificed: ObjectId,
    ruins_red_index: usize,
}

/// Whether `copy`, an ability of a Refractor, is its donated copy of `land`'s
/// printed ability `printed`: stamped with `land` as its provider (CR 201.5a)
/// and printing the same ability.
fn donated_copy_of(copy: &AbilityDefinition, land: ObjectId, printed: &AbilityDefinition) -> bool {
    copy.granting_object
        .is_some_and(|provider| provider.object_id == land)
        && copy.description == printed.description
}

/// The ability index on `refractor` whose definition is its donated copy of
/// `land`'s ability `land_index` — discovered, not assumed.
fn granted_index(
    state: &GameState,
    refractor: ObjectId,
    land: ObjectId,
    land_index: usize,
) -> usize {
    let wanted = &activated_ability_definitions(state, land)[land_index].1;
    let matches: Vec<usize> = activated_ability_definitions(state, refractor)
        .into_iter()
        .filter(|(_, ability)| donated_copy_of(ability, land, wanted))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "reach: {refractor:?} has exactly one copy of {land:?}'s ability {land_index}"
    );
    matches[0]
}

/// The land Harrow's additional cost sacrifices on the Q7 board.
#[derive(Clone, Copy)]
enum HarrowSacrifice {
    /// Ramunap Ruins, the first provider: every later grant shifts down.
    Ruins,
    /// Wastes, the last provider: the Ruins grants keep their indices.
    Wastes,
}

/// Q7: Manascape Refractors A and B; the providers enter Ramunap Ruins →
/// Island → Forest → Wastes. A makes two {R} with its copy of the Ruins
/// ability (Twiddle untaps it between), Harrow is announced sacrificing
/// `sacrifice`, the Forest pays {G}, and A's two reds are pinned.
fn refractor_harrow_board(sacrifice: HarrowSacrifice) -> RefractorHarrow {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let refractor_a = sc
        .add_artifact_from_oracle(P0, "Manascape Refractor", MANASCAPE_REFRACTOR)
        .id();
    let refractor_b = sc
        .add_artifact_from_oracle(P0, "Manascape Refractor", MANASCAPE_REFRACTOR)
        .id();
    let ruins = sc
        .add_land_from_oracle(P0, "Ramunap Ruins", RAMUNAP_RUINS)
        .with_subtypes(vec!["Desert"])
        .id();
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let forest = sc.add_basic_land(P0, ManaColor::Green);
    let wastes = add_wastes(&mut sc);
    let twiddle = sc
        .add_spell_to_hand_from_oracle(P0, "Twiddle", true, TWIDDLE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 0))
        .id();
    let harrow = sc
        .add_spell_to_hand_from_oracle(P0, "Harrow", true, HARROW)
        .with_mana_cost(cost(vec![ManaCostShard::Green], 2))
        .id();
    sc.add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 0));
    sc.add_creature(P1, "Grizzly Bears", 2, 2);
    let mut runner = sc.build();
    runner.state_mut().objects.get_mut(&ruins).unwrap().tapped = true;
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::flush_layers(runner.state_mut());

    let ruins_red_index = granted_index(runner.state(), refractor_a, ruins, 1);
    assert_eq!(
        granted_index(runner.state(), refractor_b, ruins, 1),
        ruins_red_index,
        "reach: both Refractors list the providers in the same order"
    );
    let ruins_red = |refractor| GameAction::ActivateAbility {
        source_id: refractor,
        ability_index: ruins_red_index,
        payment_mode: CastPaymentMode::Auto,
    };

    act(&mut runner, ruins_red(refractor_a));
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
    act(&mut runner, ruins_red(refractor_a));

    let sacrificed = match sacrifice {
        HarrowSacrifice::Ruins => ruins,
        HarrowSacrifice::Wastes => wastes,
    };
    cast_to_payment(
        &mut runner,
        harrow,
        Answers {
            cost_objects: vec![sacrificed],
            ..Answers::default()
        },
    );
    produce(&mut runner, &[(forest, None)]);
    let a_red: Vec<_> = runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.source_id == refractor_a && unit.color == ManaType::Red)
        .collect();
    assert_eq!(a_red.len(), 2, "reach: two {{R}} from Refractor A");
    for unit in a_red {
        act(
            &mut runner,
            GameAction::SpendPoolMana {
                pip_id: unit.pip_id,
            },
        );
    }
    RefractorHarrow {
        runner,
        refractor_b,
        ruins,
        forest,
        sacrificed,
        ruins_red_index,
    }
}

/// Q7: Harrow's additional cost sacrifices Ramunap Ruins as the payment
/// finishes. Refractor B is the same object afterwards, but its granted
/// abilities are rebuilt in provider order, so the index of its copy of the
/// Ruins red ability now names the Forest's {G} ability: the activation the
/// certificate would defer no longer exists (CR 605.3a).
#[test]
fn refractor_ability_index_renamed_by_the_finisher_refuses_deferral() {
    let board = refractor_harrow_board(HarrowSacrifice::Ruins);
    let state = board.runner.state();
    let b_red = GameAction::ActivateAbility {
        source_id: board.refractor_b,
        ability_index: board.ruins_red_index,
        payment_mode: CastPaymentMode::Auto,
    };

    let mut activated = GameRunner::from_state(state.clone());
    act(&mut activated, b_red.clone());
    let pool = &activated.state().players[0].mana_pool;
    let b_unit = pool
        .units()
        .find(|unit| unit.source_id == board.refractor_b)
        .expect("reach: Refractor B adds a unit");
    assert_eq!(b_unit.color, ManaType::Red, "reach: B's copy adds {{R}}");
    assert!(
        b_unit.restrictions.is_empty() && b_unit.grants.is_empty(),
        "reach: the new red is rider-free"
    );
    let a_red: Vec<_> = pool
        .units()
        .filter(|unit| unit.pip_id != b_unit.pip_id && unit.color == ManaType::Red)
        .collect();
    assert_eq!(a_red.len(), 2, "reach: A's two reds are still pooled");
    assert!(
        a_red
            .iter()
            .all(|unit| units_spend_equivalent(activated.state(), unit, &b_unit)),
        "reach: Refractor A's and B's reds are spend-equivalent in S'"
    );

    let without = finalizes_in_one_step(state, &GameAction::PassPriority)
        .expect("reach: a pass finalizes Harrow");
    assert_eq!(
        without.objects[&board.refractor_b].incarnation,
        state.objects[&board.refractor_b].incarnation,
        "reach: Refractor B is the same object after the pass"
    );
    assert!(
        without
            .objects
            .get(&board.ruins)
            .is_none_or(|ruins| ruins.zone != Zone::Battlefield),
        "reach: the Ruins is gone after the pass"
    );
    let named_after = activated_ability_definitions(&without, board.refractor_b)
        .into_iter()
        .find(|(index, _)| *index == board.ruins_red_index)
        .map(|(_, ability)| ability)
        .expect("reach: B still has an ability at the old index");
    assert!(
        !donated_copy_of(
            &named_after,
            board.ruins,
            &activated_ability_definitions(state, board.ruins)[1].1
        ),
        "reach: B's old index no longer names the Ruins red ability"
    );
    assert!(
        donated_copy_of(
            &named_after,
            board.forest,
            &activated_ability_definitions(state, board.forest)[0].1
        ),
        "reach: B's old index now names the Forest's {{G}} ability"
    );

    assert_eq!(
        certificate(&board.runner, &b_red),
        NotCertified(SurplusNotCertified::DeferralUnavailable)
    );
}

/// Q7 control: Harrow sacrifices the Wastes, the last provider, so the Ruins
/// red ability keeps its index on Refractor B and B's extra red is a surplus.
#[test]
fn refractor_ability_index_kept_by_the_finisher_is_surplus() {
    let board = refractor_harrow_board(HarrowSacrifice::Wastes);
    let state = board.runner.state();
    let without = finalizes_in_one_step(state, &GameAction::PassPriority)
        .expect("reach: a pass finalizes Harrow");
    assert!(
        without
            .objects
            .get(&board.sacrificed)
            .is_none_or(|wastes| wastes.zone != Zone::Battlefield),
        "reach: the Wastes is gone after the pass"
    );
    assert_eq!(
        granted_index(&without, board.refractor_b, board.ruins, 1),
        board.ruins_red_index,
        "reach: B's index still names the Ruins red ability"
    );
    assert_eq!(
        certificate(
            &board.runner,
            &GameAction::ActivateAbility {
                source_id: board.refractor_b,
                ability_index: board.ruins_red_index,
                payment_mode: CastPaymentMode::Auto,
            }
        ),
        Surplus
    );
}

/// The abilities `source` put on the stack.
fn triggers_from(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

const LIBERATOR: &str = "Flash\nFlying\nYou may cast colorless spells and artifact spells as though they had flash.\nWhenever you cast a spell, if the amount of mana spent to cast that spell is greater than Liberator's power, put a +1/+1 counter on Liberator.";
const THRABEN_DOOMSAYER: &str = "{T}: Create a 1/1 white Human creature token.\nFateful hour — As long as you have 5 or less life, other creatures you control get +2/+2.";

/// The printed F1 board: P0 at `life` controls Liberator, Urza's
/// Battlethopter (1/2), Thraben Doomsayer, a Forest and two Fiery Islets, and
/// announces Grizzly Bears ({1}{G}); the Forest's {G} and Islet A's {R} (1
/// life) pay it. Returns the runner, Liberator and Islet B's {R} action.
fn liberator_board(life: i32) -> (GameRunner, ObjectId, GameAction) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_life(P0, life);
    let liberator = sc
        .add_creature(P0, "Liberator, Urza's Battlethopter", 1, 2)
        .from_oracle_text_with_keywords(&["Flash", "Flying"], LIBERATOR)
        .id();
    sc.add_creature(P0, "Thraben Doomsayer", 2, 2)
        .from_oracle_text(THRABEN_DOOMSAYER);
    let forest = sc.add_basic_land(P0, ManaColor::Green);
    let islets = [add_islet(&mut sc), add_islet(&mut sc)];
    let bears = sc
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(cost(vec![ManaCostShard::Green], 1))
        .id();
    let mut runner = sc.build();
    cast_to_payment(&mut runner, bears, Answers::default());
    produce(
        &mut runner,
        &[(forest, None), (islets[0], Some(ManaType::Red))],
    );
    let islet_b = mana_action(runner.state(), islets[1], Some(ManaType::Red));
    (runner, liberator, islet_b)
}

/// Both finishers of a decision: the pass, and `activation` then the pass.
/// Reach: each finalizes the cast and returns the payer priority.
fn finishers_after(runner: &GameRunner, activation: &GameAction) -> (GameState, GameState) {
    let without = finalizes_in_one_step(runner.state(), &GameAction::PassPriority)
        .expect("reach: a pass finalizes the cast");
    let mut activated = GameRunner::from_state(runner.state().clone());
    act(&mut activated, activation.clone());
    let with = finalizes_in_one_step(activated.state(), &GameAction::PassPriority)
        .expect("reach: a pass after the activation finalizes the cast");
    for finished in [&without, &with] {
        assert_eq!(
            finished.waiting_for,
            WaitingFor::Priority { player: P0 },
            "reach: the pass returns the payer priority"
        );
    }
    (without, with)
}

/// F1, printed: from 7 life the Forest and Islet A leave P0 at 6. The pass
/// spends 2 mana against Liberator's power 1, so its cast trigger fires
/// (CR 601.2i + CR 603.4). Islet B's {R} first costs 1 more life: at 5,
/// Thraben Doomsayer's fateful hour makes Liberator 3/4 and the trigger
/// doesn't fire, though the extra {R} isn't spent and both finishers record
/// the same payment stamps.
#[test]
fn islet_life_payment_that_turns_off_liberator_is_not_certified() {
    let (runner, liberator, islet_b) = liberator_board(7);
    let state = runner.state();
    assert_eq!(
        state.players[0].life, 6,
        "reach: P0 is at 6 at the decision"
    );
    let spell = state.pending_cast.as_ref().unwrap().object_id;

    let mut activated = GameRunner::from_state(state.clone());
    act(&mut activated, islet_b.clone());
    let new_units: Vec<_> = activated.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| {
            !state.players[0]
                .mana_pool
                .units()
                .any(|before| before.pip_id == unit.pip_id)
        })
        .collect();
    assert_eq!(new_units.len(), 1, "reach: Islet B adds one unit");
    let unit = &new_units[0];
    assert_eq!(unit.color, ManaType::Red);
    assert!(
        unit.restrictions.is_empty() && unit.grants.is_empty(),
        "reach: the new {{R}} is rider-free"
    );
    assert!(
        activated.state().players[0]
            .mana_pool
            .units()
            .any(|other| other.pip_id != unit.pip_id
                && units_spend_equivalent(activated.state(), &other, unit)),
        "reach: Islet A's {{R}} is spend-equivalent to it, saturating the {{1}}"
    );

    let (without, with) = finishers_after(&runner, &islet_b);
    assert!(
        without.objects[&spell].cast_payment_stamps_equivalent(&with.objects[&spell]),
        "reach: both finishers record the same payment stamps"
    );
    assert_eq!(
        (
            without.objects[&liberator].power,
            with.objects[&liberator].power
        ),
        (Some(1), Some(3)),
        "reach: fateful hour makes Liberator 3/4 only after the activation"
    );
    assert_eq!(
        (
            triggers_from(&without, liberator),
            triggers_from(&with, liberator)
        ),
        (1, 0),
        "reach: Liberator's cast trigger fires only without the activation"
    );
    assert_eq!(
        certificate(&runner, &islet_b),
        NotCertified(SurplusNotCertified::CastTriggersDiverge)
    );
}

/// Control: from 8 life P0 is at 7 at the decision and 6 after Islet B, above
/// fateful hour either way, so Liberator's trigger fires in both finishers and
/// the extra {R} is a surplus.
#[test]
fn islet_life_payment_that_keeps_liberator_is_surplus() {
    let (runner, liberator, islet_b) = liberator_board(8);
    let (without, with) = finishers_after(&runner, &islet_b);
    assert_eq!(
        (
            triggers_from(&without, liberator),
            triggers_from(&with, liberator)
        ),
        (1, 1),
        "reach: Liberator's cast trigger fires in both finishers"
    );
    assert_eq!(certificate(&runner, &islet_b), Surplus);
}
