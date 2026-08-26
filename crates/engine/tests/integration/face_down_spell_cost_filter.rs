//! CR 601.2f + CR 708.4: a cost-modifier static filtered on "face-down creature
//! spells" must reduce the {3} a morph/megamorph/disguise card pays to be cast
//! face down (CR 702.37c / CR 702.168b).
//!
//! Class (measured against `card-data.json`, 3 cards): Kadena, Slinking Sorcerer
//! ("The first face-down creature spell you cast each turn costs {3} less to
//! cast"), Dream Chisel and Obscuring Aether ("Face-down creature spells you
//! cast cost {1} less to cast"). All three parse into a `StaticMode::ModifyCost`
//! whose `spell_filter` carries `FilterProp::FaceDown`.
//!
//! Before the fix that filter could never match: the live cost seam projects the
//! spell into a `SpellCastRecord`, and `FilterProp::FaceDown` failed closed
//! against that record — grouped with battlefield-only predicates, although
//! CR 708.4 puts a face-down spell on the stack as a real spell. The reduction
//! was therefore dead for every card in the class.
//!
//! These tests drive the real cast pipeline (`GameAction::CastSpell` →
//! `continue_cast_face_down` → cost calculation → mana payment) and read the
//! mana left unspent, so they measure what a player pays.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Kadena's Oracle text, parsed by the engine rather than hand-built, so the
/// test fails if the parser stops producing the filtered reduction.
const KADENA_TEXT: &str =
    "The first face-down creature spell you cast each turn costs {3} less to cast.";
/// Dream Chisel / Obscuring Aether — the unconditional half of the class.
const DREAM_CHISEL_TEXT: &str = "Face-down creature spells you cast cost {1} less to cast.";

fn colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn cast(runner: &mut engine::game::scenario::GameRunner, spell: ObjectId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_ok()
}

fn unspent(runner: &engine::game::scenario::GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

/// CR 601.2f + CR 708.4: the reduction reaches the face-down cast.
///
/// The morph card's printed cost is {5} and the pool holds 3, so the only legal
/// cast is the {3} face-down one (CR 702.37c) — which Kadena must reduce to {0}.
///
/// Discriminating: without the fix `FilterProp::FaceDown` fails closed against
/// the spell-cast record, the reduction never applies, and the 3 mana are spent.
#[test]
fn a_face_down_creature_spell_gets_kadenas_reduction() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Snake Sorcerer", 3, 3, KADENA_TEXT);
    let morph = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    assert!(cast(&mut runner, morph), "the face-down cast must be legal");
    assert!(
        runner.state().objects[&morph].face_down
            && runner.state().objects[&morph].zone == Zone::Stack,
        "reach-guard: the spell must be on the stack FACE DOWN, not cast face up"
    );
    assert_eq!(
        unspent(&runner),
        3,
        "Kadena must reduce the {{3}} face-down cost to {{0}}"
    );
}

/// CR 601.2f: Dream Chisel / Obscuring Aether — the same class without the
/// once-per-turn condition. {3} face-down cost minus {1} leaves {2}.
///
/// Discriminating in the same way, and it separates the two halves of the fix:
/// this one needs only the `spell_filter` to match, no ledger read.
#[test]
fn dream_chisels_unconditional_reduction_reaches_a_face_down_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chisel Bearer", 2, 2, DREAM_CHISEL_TEXT);
    let morph = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    assert!(cast(&mut runner, morph), "the face-down cast must be legal");
    assert_eq!(
        unspent(&runner),
        1,
        "a {{1}} reduction must leave 1 of the 3 mana unspent"
    );
}

/// CR 601.2f + CR 604.1: "the FIRST face-down creature spell you cast each turn"
/// — the second one that turn pays in full. The reduction is a static ability,
/// so its "first each turn" condition is simply true or false at the moment the
/// total cost is determined.
///
/// This is the ledger half. Kadena's condition counts face-down creature spells
/// cast this turn and requires zero; that count reads the same
/// `FilterProp::FaceDown` predicate against the stored cast records, so before
/// the fix it was permanently 0 and the condition was accidentally always true.
///
/// The pair of assertions is what discriminates, not either alone: a fix that
/// simply always applied the reduction would let BOTH casts through for free and
/// leave mana unspent; the unfixed engine cannot pay for the second cast at all.
#[test]
fn only_the_first_face_down_spell_each_turn_is_reduced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Snake Sorcerer", 3, 3, KADENA_TEXT);
    let first = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Second Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    // Exactly enough for a reduced first cast ({0}) plus a full second one ({3}).
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    assert!(
        cast(&mut runner, first),
        "the first face-down cast must be legal"
    );
    assert_eq!(
        unspent(&runner),
        3,
        "the FIRST face-down spell is the reduced one"
    );

    // CR 302.1: a creature spell is cast at sorcery speed, so the first one has
    // to leave the stack before the second can be cast at all.
    runner.resolve_top();

    assert!(
        cast(&mut runner, second),
        "the second face-down cast must still be payable from the untouched pool"
    );
    assert_eq!(
        unspent(&runner),
        0,
        "the SECOND face-down spell that turn pays the full {{3}}"
    );
}

/// CR 601.2f + CR 702.37c: the reduced cost must also be what the OFFER is
/// judged against. Kadena takes the {3} face-down cost to {0}, so the cast has to
/// be legal with an EMPTY mana pool — the player-visible form of the same rule.
///
/// Playtest find: "eigentlich müsste ich auch mit 0 Mana auf dem Feld eine
/// Morph/Disguise wirken können". `can_afford_face_down_cast` asks whether the
/// fixed {3} is payable, so if that question is put before the reduction the
/// engine withholds a cast the player is entitled to make.
#[test]
fn kadena_lets_a_face_down_creature_be_cast_with_an_empty_pool() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Snake Sorcerer", 3, 3, KADENA_TEXT);
    let morph = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    // No mana at all.
    let mut runner = scenario.build();
    assert_eq!(unspent(&runner), 0, "reach-guard: the pool starts empty");

    assert!(
        cast(&mut runner, morph),
        "a {{0}} face-down cast must be legal with no mana available"
    );
    assert!(
        runner.state().objects[&morph].face_down
            && runner.state().objects[&morph].zone == Zone::Stack,
        "the creature must reach the stack face down"
    );
}

/// CR 601.2f: the cast offer must SHOW the reduced cost, not the printed {3}.
///
/// The client renders `alternative_cost` verbatim (the frontend is a display
/// layer and must not recompute game state), so a menu that says {3} while the
/// payment takes {0} is an engine defect, not a rendering one.
#[test]
fn the_face_down_offer_shows_the_reduced_cost() {
    use engine::types::game_state::WaitingFor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Snake Sorcerer", 3, 3, KADENA_TEXT);
    let morph = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    // Enough for the printed {5} too, so the player is given the CHOICE menu
    // rather than being routed straight into the face-down cast.
    scenario.with_mana_pool(P0, colorless(5));
    let mut runner = scenario.build();

    assert!(cast(&mut runner, morph), "the cast must be accepted");
    match &runner.state().waiting_for {
        WaitingFor::AlternativeCastChoice {
            alternative_cost: Some(cost),
            ..
        } => assert_eq!(
            cost.mana_value(),
            0,
            "Kadena takes the face-down cast to {{0}}; the offer must say so, got {cost:?}"
        ),
        other => panic!("expected the face-down AlternativeCastChoice, got {other:?}"),
    }
}

/// PROBE (playtest question, not a claim of this fix): CR 702.37c prices the
/// face-down cast at a fixed GENERIC {3}, so mana of any color pays it — 3 Islands
/// must cast a green morph creature face down. Pinned here because the offer gate
/// now runs cost modifiers, and nothing in that change may narrow which mana counts.
#[test]
fn off_color_mana_pays_the_generic_face_down_cost() {
    use engine::types::mana::ManaCostShard;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Ainok Survivalist", 2, 1)
        .with_keyword(Keyword::Morph(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        }))
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    // Three BLUE mana: the printed {1}{G} is unpayable, the generic {3} is not.
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    assert!(
        cast(&mut runner, morph),
        "CR 702.37c: the {{3}} face-down cost is generic — off-color mana pays it"
    );
    assert!(
        runner.state().objects[&morph].face_down,
        "the cast must be the FACE-DOWN one, not the printed {{1}}{{G}}"
    );
    assert_eq!(unspent(&runner), 0, "all three blue mana pay the {{3}}");
}

/// Positive counter-direction: the filter must not leak onto face-up casts.
///
/// A creature without morph is cast face up for its printed {2}; Kadena's
/// "face-down creature spell" filter must leave it alone. Over-application would
/// show up as unspent mana here.
#[test]
fn a_face_up_creature_spell_is_not_reduced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Snake Sorcerer", 3, 3, KADENA_TEXT);
    let plain = scenario
        .add_creature_to_hand(P0, "Plain Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();

    assert!(cast(&mut runner, plain), "the face-up cast must be legal");
    assert!(
        !runner.state().objects[&plain].face_down,
        "reach-guard: a creature without morph is cast FACE UP"
    );
    assert_eq!(
        unspent(&runner),
        0,
        "a face-up creature spell must pay its full {{2}}"
    );
}

/// Helper for the exile-parity pair below: a morph creature in EXILE that is
/// both foretold (castable face up for {1}) and granted `PlayFromExile` with a
/// {2} `cast_cost_raise`. The explicit face-down election routes through
/// `PlayFromExile` (CR 601.2a-b), so the real face-down cast pays {3}+{2}={5};
/// a variant-less projection infers Foretell first and would price {3}.
fn exile_morph_with_competing_permissions(
    pool: usize,
) -> (engine::game::scenario::GameRunner, ObjectId) {
    use engine::types::ability::{CastingPermission, Duration};
    use engine::types::statics::CastFrequency;
    use engine::types::zones::EtbTapState;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_exile(P0, "Foretold Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_mana_pool(P0, colorless(pool));
    let mut runner = scenario.build();
    let obj = runner.state_mut().objects.get_mut(&morph).unwrap();
    obj.casting_permissions.push(CastingPermission::Foretold {
        cost: ManaCost::generic(1),
        turn_foretold: 0,
    });
    obj.casting_permissions
        .push(CastingPermission::PlayFromExile {
            duration: Duration::UntilEndOfTurn,
            granted_to: P0,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: Some(ManaCost::generic(2)),
            land_enter_tapped: EtbTapState::Unspecified,
        });
    (runner, morph)
}

/// CR 601.2a-b + CR 601.2f: the face-down OFFER from exile must be priced by the
/// permission the real cast elects. The menu has to show {3}+{2}={5} (the
/// `PlayFromExile` raise), and choosing it has to charge exactly that.
///
/// Discriminating: a variant-less projection infers the Foretold permission
/// (no raise) and displays {3} while the payment takes {5}.
#[test]
fn the_face_down_offer_from_exile_prices_the_play_from_exile_raise() {
    use engine::types::actions::AlternativeCastDecision;
    use engine::types::game_state::{AlternativeCastKeyword, WaitingFor};

    let (mut runner, morph) = exile_morph_with_competing_permissions(5);

    assert!(cast(&mut runner, morph), "the cast must be accepted");
    match &runner.state().waiting_for {
        WaitingFor::AlternativeCastChoice {
            keyword: AlternativeCastKeyword::FaceDown,
            alternative_cost: Some(cost),
            ..
        } => assert_eq!(
            cost.mana_value(),
            5,
            "the offer must price the PlayFromExile raise: {{3}}+{{2}}, got {cost:?}"
        ),
        other => panic!("expected the face-down AlternativeCastChoice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("the priced face-down cast must complete");
    assert!(
        runner.state().objects[&morph].face_down
            && runner.state().objects[&morph].zone == Zone::Stack,
        "the spell must be on the stack face down"
    );
    assert_eq!(
        unspent(&runner),
        0,
        "the displayed {{5}} is what is charged"
    );
}

/// CR 601.2f: the same board with only 3 mana — the real face-down cost {5} is
/// NOT payable, so the face-down offer must be withheld and the cast must fall
/// through to the legal face-up Foretell cast for {1}.
///
/// Discriminating: the variant-less projection prices {3}, offers/auto-routes
/// the face-down cast, and the payment then fails — the accepted action dies.
#[test]
fn an_unpayable_exile_raise_withholds_the_face_down_offer() {
    let (mut runner, morph) = exile_morph_with_competing_permissions(3);

    assert!(
        cast(&mut runner, morph),
        "the cast must be accepted (face up via Foretell)"
    );
    assert!(
        runner.state().objects[&morph].zone == Zone::Stack
            && !runner.state().objects[&morph].face_down,
        "with the {{5}} unpayable the legal cast is the FACE-UP foretell one"
    );
}
/// CR 702.37b (Megamorph) + CR 601.2f: the keyword-sibling coverage for the
/// COST path — Kadena's reduction must reach a Megamorph card's face-down
/// cast exactly as it reaches Morph. Discriminating through the real cast:
/// without the record fix the 3 mana are spent.
#[test]
fn kadenas_reduction_reaches_a_megamorph_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Snake Sorcerer", 3, 3, KADENA_TEXT);
    let morph = scenario
        .add_creature_to_hand(P0, "Mega Beast", 4, 5)
        .with_keyword(Keyword::Megamorph(ManaCost::generic(5)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    assert!(cast(&mut runner, morph), "the face-down cast must be legal");
    assert!(
        runner.state().objects[&morph].face_down
            && runner.state().objects[&morph].zone == Zone::Stack,
        "reach-guard: the Megamorph spell must be on the stack FACE DOWN"
    );
    assert_eq!(
        unspent(&runner),
        3,
        "Kadena must reduce a Megamorph face-down cast to {{0}}"
    );
}

/// CR 702.168b (Disguise) + CR 601.2f: the third keyword of the class on the
/// COST path — Dream Chisel's {1} reduction must reach a Disguise card's
/// face-down cast ({3} − {1} leaves 1 of 3).
#[test]
fn dream_chisels_reduction_reaches_a_disguise_spell() {
    use engine::types::keywords::DisguiseCost;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chisel Bearer", 2, 2, DREAM_CHISEL_TEXT);
    let disguised = scenario
        .add_creature_to_hand(P0, "Cloaked Beast", 4, 5)
        .with_keyword(Keyword::Disguise(DisguiseCost::Mana(ManaCost::generic(5))))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    assert!(
        cast(&mut runner, disguised),
        "the face-down cast must be legal"
    );
    assert!(
        runner.state().objects[&disguised].face_down
            && runner.state().objects[&disguised].zone == Zone::Stack,
        "reach-guard: the Disguise spell must be on the stack FACE DOWN"
    );
    assert_eq!(
        unspent(&runner),
        1,
        "a {{1}} reduction must reach a Disguise face-down cast"
    );
}
