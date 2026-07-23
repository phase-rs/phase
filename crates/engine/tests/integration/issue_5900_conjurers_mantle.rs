//! Runtime + parser regression for GitHub issue #5900 — Conjurer's Mantle fails
//! to recognize matching creature types.
//!
//! https://github.com/phase-rs/phase/issues/5900
//!
//! Oracle (verified against client/public/card-data.json):
//!   "Equipped creature gets +1/+1 and has vigilance.
//!    Whenever equipped creature attacks, look at the top six cards of your
//!    library. You may reveal a card that shares a creature type with that
//!    creature from among them and put it into your hand. Put the rest on the
//!    bottom of your library in a random order.
//!    Equip {1}"
//!
//! Root cause: the "from among them" reveal filter ("a card that shares a
//! creature type with that creature") was parsed via the CTX-FREE `parse_target`
//! (`parse_dig_from_among`), so the trigger subject never reached
//! `parse_shared_quality_reference`. There, the demonstrative "that creature"
//! also fell through to the ctx-free `parse_target`, binding the shared-quality
//! reference to `TargetFilter::ParentTarget` instead of `TriggeringSource`.
//! In this attacks trigger there is no chosen target / recipient / effect-context
//! object, so `parent_target_shared_quality_values` (game/filter.rs) returns
//! `None` — the "shares a creature type" test then matches NO card, so no card
//! in the top six is ever selectable, exactly the reported symptom.
//!
//! Fix (two parts): (1) thread the enclosing `ParseContext` through
//! `parse_dig_from_among` so the reveal filter parses with the trigger subject
//! in scope; (2) resolve the singular demonstrative "that creature" via the same
//! ctx-aware `resolve_pronoun_target` the bare pronoun "it" already used, so it
//! binds to `TriggeringSource` (the attacking equipped creature) when a
//! non-source trigger subject exists and stays `ParentTarget` otherwise.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, FilterProp, SharedQuality, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const MANTLE_ORACLE: &str = "Equipped creature gets +1/+1 and has vigilance.\nWhenever equipped creature attacks, look at the top six cards of your library. You may reveal a card that shares a creature type with that creature from among them and put it into your hand. Put the rest on the bottom of your library in a random order.\nEquip {1}";

/// Turn Conjurer's Mantle (added via `add_creature_from_oracle` so its trigger
/// parses) into a real Equipment attached to `host`.
fn make_equipment_attached(runner: &mut GameRunner, mantle: ObjectId, host: ObjectId) {
    {
        let obj = runner.state_mut().objects.get_mut(&mantle).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 0,
        };
        obj.attached_to = Some(AttachTarget::Object(host));
    }
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(mantle);
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
}

/// Hand P0 priority and pass until the engine reaches DeclareAttackers.
fn advance_to_declare_attackers(runner: &mut GameRunner) {
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    for _ in 0..24 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!(
        "did not reach DeclareAttackers; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

/// After declaring attackers, resolve the attack trigger's stack until the
/// engine surfaces the Dig reveal-from-among choice.
fn advance_to_dig_choice(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("OrderTriggers should succeed");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should advance the trigger");
            }
            other => panic!("unexpected waiting state before DigChoice: {other:?}"),
        }
    }
    panic!("attack trigger never reached the Dig reveal choice");
}

/// A Goblin card in the top six must be selectable — it shares a creature type
/// with the attacking equipped Goblin. Pre-fix the reveal filter matched no
/// card (reference bound to `ParentTarget`, which resolves to nothing in this
/// trigger), so `selectable_cards` was empty.
#[test]
fn conjurers_mantle_reveal_matches_shared_creature_type() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Stage P0's top six: one Goblin creature card (shares a type with the
    // attacking Goblin) buried among five non-matching cards. `add_card_to_
    // library_top` inserts at index 0, so add bottom-of-six first.
    let f5 = scenario.add_card_to_library_top(P0, "Filler 5");
    let f4 = scenario.add_card_to_library_top(P0, "Filler 4");
    let f3 = scenario.add_card_to_library_top(P0, "Filler 3");
    let goblin_card = scenario.add_card_to_library_top(P0, "Goblin Raider");
    let f2 = scenario.add_card_to_library_top(P0, "Filler 2");
    let f1 = scenario.add_card_to_library_top(P0, "Filler 1");

    // The equipped creature: a Goblin so "shares a creature type with that
    // creature" is meaningful.
    let host = scenario
        .add_creature(P0, "Goblin Bearer", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();
    // Conjurer's Mantle, built from real Oracle text so the attack trigger AST
    // is the parser's own output.
    let mantle = scenario
        .add_creature_from_oracle(P0, "Conjurer's Mantle", 0, 0, MANTLE_ORACLE)
        .id();
    // A wall so P0 has a legal defender to attack.
    scenario.add_creature(P1, "Wall", 0, 4);

    let mut runner = scenario.build();
    // CR 205.3m: "Goblin" must be a registered creature type for the
    // shared-creature-type filter to count it (real games load this from card
    // data; a synthetic scenario must seed it explicitly).
    runner
        .state_mut()
        .all_creature_types
        .push("Goblin".to_string());

    make_equipment_attached(&mut runner, mantle, host);

    // Type the staged library cards: the Goblin Raider is a Goblin creature
    // card; the fillers are non-creature so they can never share a creature type.
    {
        let g = runner.state_mut().objects.get_mut(&goblin_card).unwrap();
        g.card_types.core_types = vec![CoreType::Creature];
        g.card_types.subtypes = vec!["Goblin".to_string()];
        g.base_card_types = g.card_types.clone();
    }
    for &id in &[f1, f2, f3, f4, f5] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Sorcery];
        obj.base_card_types = obj.card_types.clone();
    }

    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(host, AttackTarget::Player(P1))])
        .expect("declaring the equipped Goblin as an attacker should succeed");

    advance_to_dig_choice(&mut runner);

    let (cards, selectable) = match runner.state().waiting_for.clone() {
        WaitingFor::DigChoice {
            cards,
            selectable_cards,
            ..
        } => (cards, selectable_cards),
        other => panic!("expected DigChoice, got {other:?}"),
    };

    assert!(
        cards.contains(&goblin_card),
        "the Goblin card must be among the six looked-at cards; got {cards:?}"
    );
    assert!(
        selectable.contains(&goblin_card),
        "the Goblin card shares a creature type with the attacking equipped Goblin, \
         so it MUST be selectable to reveal (issue #5900). selectable = {selectable:?}"
    );
    // The non-creature fillers share no creature type and must stay non-selectable.
    for &id in &[f1, f2, f3, f4, f5] {
        assert!(
            !selectable.contains(&id),
            "non-creature filler {id:?} shares no creature type and must not be selectable"
        );
    }

    // Reveal the Goblin card → it goes to hand.
    runner
        .act(GameAction::SelectCards {
            cards: vec![goblin_card],
        })
        .expect("selecting the shared-type Goblin card should succeed");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().players[0].hand.contains(&goblin_card),
        "the revealed Goblin card must be put into P0's hand"
    );
}

/// Parser-level revert guard: the reveal filter's shared-creature-type reference
/// must bind to the trigger subject (`TriggeringSource`), not `ParentTarget`.
#[test]
fn conjurers_mantle_shared_type_reference_binds_to_trigger_subject() {
    let parsed = parse_oracle_text(
        MANTLE_ORACLE,
        "Conjurer's Mantle",
        &[],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.as_ref())
        .expect("Conjurer's Mantle must have an attack trigger with an execute effect");

    let Effect::Dig { filter, .. } = trigger.effect.as_ref() else {
        panic!(
            "attack trigger must lower to a Dig, got {:?}",
            trigger.effect
        );
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("Dig filter must be a typed card filter, got {filter:?}");
    };
    let shares = tf
        .properties
        .iter()
        .find_map(|p| match p {
            FilterProp::SharesQuality {
                quality: SharedQuality::CreatureType,
                reference,
                ..
            } => Some(reference.clone()),
            _ => None,
        })
        .expect("Dig filter must carry a SharesQuality(CreatureType) property");

    assert_eq!(
        shares.as_deref(),
        Some(&TargetFilter::TriggeringSource),
        "\"that creature\" must bind to the attacking equipped creature \
         (TriggeringSource), not ParentTarget (which resolves to nothing here); \
         got {shares:?}"
    );
}
