//! Discriminating runtime regression for **Vannifar, Evolved Enigma** mode 1 —
//! "Cloak a card from your hand".
//!
//! Unlike Cryptic Coat / Ransom Note ("cloak the top card of your library"),
//! Vannifar cloaks a card the controller *chooses from hand*. This is NOT a
//! library-top cloak: it lowers to a `ChooseFromZone { zone: Hand }` parent
//! whose `Cloak { object_source: Some(ParentTarget) }` sub-ability turns the
//! CHOSEN card face down — reading the pick the choose forwarded onto the
//! resolving ability's `targets` (CR 608.2c), never the top of the library.
//!
//! This drives the SAME production path the runtime uses:
//! `resolve_ability_chain` -> `WaitingFor::ChooseFromZoneChoice` ->
//! `engine::game::apply(SelectCards)` -> `drain_pending_continuation` ->
//! `cloak::resolve` (the `object_source: Some` branch).
//!
//! DISCRIMINATOR (anti-hollow-win): hand card A is cloaked (face-down 2/2 with
//! ward {2}, leaving the hand); the distinguishable library-top card B is
//! UNTOUCHED (still on top of library, still face up). If `object_source` were
//! reverted to `None` (or pointed at the library top), the resolver would cloak
//! B and leave A in hand — every assertion below flips.
//!
//! CR 701.58a: Cloak — face-down 2/2 creature with ward {2}.
//! CR 608.2c: the Cloak sub-ability reads the chosen card the choose forwarded.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn vannifar_cloaks_chosen_hand_card_leaving_library_top_untouched() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The card the controller will choose from hand.
    let hand_a = scenario
        .add_creature_to_hand(P0, "Hand Creature A", 3, 3)
        .id();
    // A distinguishable card sitting on top of P0's library — the library-top
    // source a hollow (library-top) fix would wrongly cloak instead.
    let library_b = scenario.add_card_to_library_top(P0, "Library Card B");
    // The Vannifar permanent that owns the ability (source of the cloak).
    let source = scenario
        .add_creature(P0, "Vannifar, Evolved Enigma", 4, 5)
        .id();

    let mut runner = scenario.build();

    // Parser path the real card uses: the ChooseFromZone{Hand} + Cloak sub-chain.
    let def = parse_effect_chain("Cloak a card from your hand", AbilityKind::Spell);
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::ChooseFromZone {
                zone: Zone::Hand,
                ..
            }
        ),
        "from-hand cloak must lower to ChooseFromZone{{Hand}}, got {:?}",
        def.effect
    );
    let sub = def
        .sub_ability
        .as_ref()
        .expect("from-hand cloak chains a Cloak sub-ability");
    assert!(
        matches!(
            sub.effect.as_ref(),
            Effect::Cloak {
                object_source: Some(TargetFilter::ParentTarget),
                ..
            }
        ),
        "sub-ability must cloak the chosen object (object_source: Some(ParentTarget)), got {:?}",
        sub.effect
    );

    // Resolve through the production resolver — raises the interactive choose.
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("ChooseFromZone-then-Cloak chain must resolve to a prompt");

    // PRODUCTION STEP: the choose offers the hand card (A), never the library
    // card (B). Reverting the parse yields Unimplemented — no prompt — and this
    // WaitingFor match panics.
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the controller makes the choose");
            assert!(
                cards.contains(&hand_a),
                "the hand card must be offered, got {cards:?}"
            );
            assert!(
                !cards.contains(&library_b),
                "the library-top card must NOT be a from-hand candidate"
            );
        }
        other => {
            panic!("\"Cloak a card from your hand\" must raise ChooseFromZoneChoice, got {other:?}")
        }
    }

    // PRODUCTION STEP: answer with A through the same handler GameRunner uses.
    engine::game::apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![hand_a],
        },
    )
    .expect("selecting the offered hand card must be a legal answer");

    // DISCRIMINATOR — the CHOSEN hand card A is cloaked.
    let a = &runner.state().objects[&hand_a];
    assert_eq!(
        a.zone,
        Zone::Battlefield,
        "A must be cloaked onto the battlefield"
    );
    assert!(a.face_down, "A must be face down");
    assert_eq!(a.power, Some(2), "cloaked A is a 2/2");
    assert_eq!(a.toughness, Some(2), "cloaked A is a 2/2");
    // allow-raw-authority: asserts the exact Ward {2} the cloak profile grants.
    assert!(
        a.keywords.iter().any(|keyword| matches!(
            keyword,
            Keyword::Ward(WardCost::Mana(cost)) if *cost == ManaCost::generic(2)
        )),
        "cloaked A must have ward {{2}}, got {:?}",
        a.keywords
    );

    // A LEFT the hand.
    let p0 = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists");
    assert!(
        !p0.hand.contains(&hand_a),
        "the cloaked card must leave the hand"
    );

    // DISCRIMINATOR — the library-top card B is UNTOUCHED (still face up, still
    // on top of the library). A hollow library-top fix would have cloaked B here.
    let b = &runner.state().objects[&library_b];
    assert_eq!(b.zone, Zone::Library, "B must stay in the library");
    assert!(!b.face_down, "B must remain face up");
    assert_eq!(
        p0.library.front(),
        Some(&library_b),
        "B must remain on top of the library"
    );
}
