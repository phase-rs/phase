//! Card-level regression for **Scroll of Fate** —
//! "{T}: Manifest a card from your hand."
//!
//! Drives the REAL registered ability end to end (maintainer round 1: no
//! synthetic `parse_effect_chain` + generic resolver): the artifact is built
//! from its printed Oracle text, its activated ability is activated through
//! `GameAction::ActivateAbility`, the `{T}` cost is paid (CR 602.2b), the
//! ability resolves off the stack, and the interactive
//! `WaitingFor::ChooseFromZoneChoice` is answered through
//! `GameAction::SelectCards` — the exact path the client takes.
//!
//! DISCRIMINATORS (anti-hollow-win):
//! - the chosen hand card is a NONCREATURE (a plain sorcery): CR 701.40a must
//!   still convert it to a face-down vanilla 2/2 CREATURE. Omitting the
//!   noncreature conversion fails the full-profile assertions below.
//! - the complete manifest profile is pinned: empty name, exactly the
//!   `[Creature]` core type (the printed Sorcery type is hidden), no
//!   subtypes/supertypes, 2/2, `ManaCost::NoCost`, and NO keywords — a lazy
//!   reuse of cloak's `cloaked_2_2` (ward {2}, CR 701.58a) fails here.
//! - the distinguishable library-top card B is UNTOUCHED: a hollow
//!   library-top fix would manifest B and leave A in hand.
//!
//! CR 701.40a: Manifest — face-down 2/2 creature with no text, no name, no
//! subtypes, and no mana cost.
//! CR 608.2c: the Manifest sub-ability reads the chosen card the choose
//! forwarded.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn scroll_of_fate_activated_ability_manifests_a_chosen_noncreature_hand_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // NONCREATURE hand card: a plain sorcery with no creature side.
    let hand_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Plain Sorcery A", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .with_keyword(Keyword::Flash)
        .id();
    // A distinguishable card sitting on top of P0's library — the library-top
    // source a hollow (library-top) fix would wrongly manifest instead.
    let library_b = scenario.add_card_to_library_top(P0, "Library Card B");
    // The REAL card: battlefield artifact with its registered activated
    // ability parsed from the printed Oracle text.
    let scroll = scenario
        .add_artifact_from_oracle(P0, "Scroll of Fate", "{T}: Manifest a card from your hand.")
        .id();

    let mut runner = scenario.build();

    // PRODUCTION STEP: activate the registered ability — index 0 is the
    // card's only ability. A parse regression (Unimplemented) makes the
    // activation illegal and this expect panics.
    runner
        .act(GameAction::ActivateAbility {
            source_id: scroll,
            ability_index: 0,
        })
        .expect("Scroll of Fate's registered ability must be activatable");

    // CR 602.2b: the {T} activation cost is paid on activation, before
    // resolution.
    assert!(
        runner.state().objects[&scroll].tapped,
        "activating {{T}}: … must tap Scroll of Fate"
    );

    // Let the ability resolve off the stack (CR 608): pass priority until the
    // resolution raises the interactive choose.
    let mut reached_choice = false;
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                // PRODUCTION STEP: the choose offers the hand card (A), never
                // the library card (B).
                assert_eq!(*player, P0, "the controller makes the choose");
                assert!(
                    cards.contains(&hand_a),
                    "the hand card must be offered, got {cards:?}"
                );
                assert!(
                    !cards.contains(&library_b),
                    "the library-top card must NOT be a from-hand candidate"
                );
                reached_choice = true;
                break;
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the ability's resolution");
            }
        }
    }
    assert!(
        reached_choice,
        "resolving Scroll of Fate's ability must raise ChooseFromZoneChoice, got {:?}",
        runner.state().waiting_for
    );

    // PRODUCTION STEP: answer with A through the same handler the client uses.
    runner
        .act(GameAction::SelectCards {
            cards: vec![hand_a],
        })
        .expect("selecting the offered hand card must be a legal answer");

    // DISCRIMINATOR — the CHOSEN noncreature hand card A is manifested with
    // the COMPLETE CR 701.40a profile.
    let a = &runner.state().objects[&hand_a];
    assert_eq!(
        a.zone,
        Zone::Battlefield,
        "A must be manifested onto the battlefield"
    );
    assert!(a.face_down, "A must be face down");
    assert_eq!(a.name, "", "a face-down card has no name (CR 701.40a)");
    assert_eq!(
        a.card_types.core_types,
        vec![CoreType::Creature],
        "the face-down body is exactly a Creature — the printed Sorcery type \
         is hidden (CR 708.2a)"
    );
    assert!(
        a.card_types.supertypes.is_empty() && a.card_types.subtypes.is_empty(),
        "a face-down card has no super-/subtypes (CR 701.40a), got {:?}",
        a.card_types
    );
    assert_eq!(a.power, Some(2), "manifested A is a 2/2");
    assert_eq!(a.toughness, Some(2), "manifested A is a 2/2");
    assert_eq!(
        a.mana_cost,
        ManaCost::NoCost,
        "a face-down card has no mana cost (CR 701.40a)"
    );
    assert!(
        a.color.is_empty(),
        "a manifested card is colorless (CR 701.40a + CR 202.2b), got {:?}",
        a.color
    );
    assert!(
        a.abilities.is_empty(),
        "a manifested card has no abilities (CR 701.40a), got {:?}",
        a.abilities
    );
    // DISCRIMINATOR — manifest is NOT cloak: no keywords at all, in
    // particular no ward {2} (CR 701.58a).
    assert!(
        a.keywords.is_empty(),
        "a manifested card has no abilities/keywords (CR 701.40a), got {:?}",
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
        "the manifested card must leave the hand"
    );

    // DISCRIMINATOR — the library-top card B is UNTOUCHED (still face up,
    // still on top of the library).
    let b = &runner.state().objects[&library_b];
    assert_eq!(b.zone, Zone::Library, "B must stay in the library");
    assert!(!b.face_down, "B must remain face up");
    assert_eq!(
        p0.library.front(),
        Some(&library_b),
        "B must remain on top of the library"
    );
}

#[test]
fn from_hand_manifest_is_not_a_library_move_for_the_mana_ability_classifier() {
    // CR 605.1a: the mana-ability classifier rejects abilities that could
    // move cards to or from a library. Scroll of Fate's chain reads the HAND
    // on both levels — the `ChooseFromZone { Hand }` parent and the
    // `Manifest { object_source: Some(ParentTarget) }` sub-ability — so
    // neither may classify as a library move. Reverting the classifier's
    // Manifest arm to an unconditional `true` fails the sub-ability line.
    let def = parse_effect_chain("Manifest a card from your hand", AbilityKind::Spell);
    assert!(
        !def.effect.moves_card_to_or_from_library(),
        "ChooseFromZone{{Hand}} must not classify as a library move"
    );
    let sub = def
        .sub_ability
        .as_ref()
        .expect("from-hand manifest chains a Manifest sub-ability");
    assert!(
        !sub.effect.moves_card_to_or_from_library(),
        "a from-hand Manifest (object_source: Some) must not classify as a \
         library move (CR 605.1a)"
    );

    // Control — the library-top default stays a real library move.
    let top = parse_effect_chain("Manifest the top card of your library.", AbilityKind::Spell);
    assert!(
        top.effect.moves_card_to_or_from_library(),
        "the library-top manifest (object_source: None) must stay classified \
         as a library move"
    );
}
