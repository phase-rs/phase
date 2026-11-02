//! "{N}, Exile this card from your hand: Target land gains "{T}: Add …" until
//! this card is cast from exile. You may cast this card for as long as it
//! remains exiled." — Emrakul, the Exigent Doom and its five three-color
//! siblings (Glamorous Outlaw, Masked Bandits, Rakish Revelers, Shattered
//! Seraph, Spara's Adjudicators).
//!
//! Rules exercised, with verbatim Oracle text through the real activation and
//! cast pipeline:
//! - CR 400.7j: the activation's cost exiles the card, and the ability's
//!   effects find it there — the cast permission goes to that exiled card,
//!   never to the targeted land.
//! - CR 611.2a: the grant lasts as long as stated — until the card is cast
//!   from exile — so it survives cleanup (CR 514.2 ends only "until end of
//!   turn" effects).
//! - CR 601.2a + CR 601.2i + CR 605.3a: casting the card moves it to the stack
//!   and the granted mana ability can pay for it; the grant ends once the
//!   spell becomes cast. CR 400.7 + CR 400.7h: the spell is a new object, so
//!   the deadline is matched on the card the cast moved.
//! - CR 903.9a + CR 903.8: a commander that leaves exile uncast (to the
//!   command zone) loses the permission (CR 400.7), but the land keeps the
//!   ability, and a later cast from the command zone does not end it.
//! - CR 113.7a + CR 400.7: each activation's deadline is bound to the exile
//!   incarnation its own cost created — if the card leaves exile, returns to
//!   hand (CR 903.9b declined) and is exiled again, casting it ends only the
//!   later activation's grant.
//! - CR 400.7j: an ability that exiles its own source binds "this card" to the
//!   exiled object, so casting it from exile ends the grant.

use engine::ai_support::legal_actions_full;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityKind, CardPlayMode, CastingPermission, Duration, Effect, ManaProduction, QuantityExpr,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, TransientContinuousEffect, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EMRAKUL: &str = "When you cast this spell, untap all lands you control.\nFlying, trample\nWard—Sacrifice three permanents.\n{3}, Exile this card from your hand: Target land gains \"{T}: Add {C}{C}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.";
const COMMAND_BEACON: &str = "{T}: Add {C}.\n{T}, Sacrifice this land: Put your commander into your hand from the command zone.";
const GLAMOROUS_OUTLAW: &str = "When this creature enters, it deals 2 damage to each opponent and you scry 2.\n{2}, Exile this card from your hand: Target land gains \"{T}: Add {U}, {B}, or {R}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.";

/// An ability that exiles its own source during resolution, rather than as a cost.
const RESOLUTION_EXILER: &str = "{T}: Exile this creature. Target land gains \"{T}: Add {C}{C}.\" until this card is cast from exile. You may cast this card for as long as it remains exiled.";

fn colorless(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// Emrakul in P0's hand, its mana cost lowered to {2} so the granted
/// {C}{C} alone pays it.
fn add_emrakul(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Emrakul, the Exigent Doom", 12, 12, EMRAKUL)
        .with_mana_cost(ManaCost::generic(2))
        .id()
}

fn hand_ability_index(runner: &GameRunner, card: ObjectId) -> usize {
    runner.state().objects[&card]
        .abilities
        .iter()
        .position(|ability| ability.activation_zone == Some(Zone::Hand))
        .expect("the card has an ability activated from hand")
}

fn mana_ability_count(runner: &GameRunner, land: ObjectId) -> usize {
    runner.state().objects[&land]
        .abilities
        .iter()
        .filter(|ability| is_mana_ability(ability))
        .count()
}

/// Whether the land has the granted "{T}: Add {C}{C}" (a basic land's own
/// ability adds one colored mana).
fn has_colorless_pair_ability(runner: &GameRunner, land: ObjectId) -> bool {
    runner.state().objects[&land]
        .abilities
        .iter()
        .any(|ability| {
            matches!(
                ability.effect.as_ref(),
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 }
                    },
                    ..
                }
            )
        })
}

fn grant_effect_from(runner: &GameRunner, source: ObjectId) -> Option<&TransientContinuousEffect> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|effect| effect.source_id == source)
}

/// The transient effect `source` created on exactly `land`.
fn grant_effect_on(
    runner: &GameRunner,
    source: ObjectId,
    land: ObjectId,
) -> Option<&TransientContinuousEffect> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|effect| {
            effect.source_id == source
                && matches!(&effect.affected, TargetFilter::SpecificObject { id } if *id == land)
        })
}

/// The source incarnation (and the zone it was expected in) that the effect's
/// "until this card is cast" deadline was bound to.
fn captured_source(effect: &TransientContinuousEffect) -> (ObjectIncarnationRef, Zone) {
    let context = effect
        .duration_event_source
        .as_deref()
        .expect("an UntilEvent effect carries the source its deadline is matched against");
    (context.identity.reference, context.identity.expected_zone)
}

/// Pass priority until the stack holds at most `len` entries, asserting P0 or P1
/// holds priority on every pass.
fn pass_until_stack_len(runner: &mut GameRunner, len: usize) {
    for _ in 0..20 {
        if runner.state().stack.len() <= len {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is legal");
    }
    panic!("the stack never shrank to {len} entries");
}

fn has_until_event_grant_from(runner: &GameRunner, source: ObjectId) -> bool {
    grant_effect_from(runner, source)
        .is_some_and(|effect| matches!(effect.duration, Duration::UntilEvent { .. }))
}

fn p0_pool_is_empty(runner: &GameRunner) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0 exists")
        .mana_pool
        .units()
        .count()
        == 0
}

fn offers_cast(runner: &GameRunner, card: ObjectId) -> bool {
    let (actions, _, _) = legal_actions_full(runner.state());
    actions.iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == card),
    )
}

fn cast_with_auto_payment(runner: &mut GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the card is legal");
}

/// Pass priority (and declare no attackers) until P0's next precombat main
/// phase with an empty stack.
fn advance_to_next_own_main(runner: &mut GameRunner) {
    let start = runner.state().turn_number;
    for _ in 0..400 {
        let state = runner.state();
        if state.active_player == P0
            && state.phase == Phase::PreCombatMain
            && state.turn_number > start
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.is_empty()
        {
            return;
        }
        let action = if matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }) {
            GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }
        } else {
            GameAction::PassPriority
        };
        runner
            .act(action)
            .expect("an empty attack declaration or a priority pass is legal");
    }
    panic!("P0's next precombat main phase was never reached");
}

/// R1 (CR 400.7j): activating exiles the card as a cost, the targeted land
/// gains the mana ability and stays on the battlefield, and the exiled card
/// itself — not the land — gets the cast permission.
#[test]
fn activation_grants_the_land_and_lets_the_card_be_cast_from_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emrakul = add_emrakul(&mut scenario);
    let land_a = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    let index = hand_ability_index(&runner, emrakul);
    runner
        .activate(emrakul, index)
        .target_object(land_a)
        .resolve();

    // Reach: the cost was paid and the ability resolved.
    assert_eq!(runner.state().objects[&emrakul].zone, Zone::Exile);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert!(p0_pool_is_empty(&runner));

    assert_eq!(runner.state().objects[&land_a].zone, Zone::Battlefield);
    assert!(has_colorless_pair_ability(&runner, land_a));
    assert!(
        runner.state().objects[&emrakul]
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    mode: CardPlayMode::Cast,
                    granted_to,
                    ..
                } if *granted_to == P0
            )),
        "{:?}",
        runner.state().objects[&emrakul].casting_permissions
    );
    assert!(offers_cast(&runner, emrakul));
}

/// R2 (CR 611.2a vs CR 514.2): the grant is not an "until end of turn" effect,
/// so it is still there on P0's next turn.
#[test]
fn the_grant_survives_the_turn_until_the_card_is_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emrakul = add_emrakul(&mut scenario);
    let land_a = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_library_top(P0, &["P0 Draw"]);
    scenario.with_library_top(P1, &["P1 Draw"]);
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    let start = runner.state().turn_number;
    let index = hand_ability_index(&runner, emrakul);
    runner
        .activate(emrakul, index)
        .target_object(land_a)
        .resolve();
    advance_to_next_own_main(&mut runner);

    // Reach: a full turn cycle passed.
    assert!(runner.state().turn_number > start);
    assert_eq!(runner.state().active_player, P0);

    assert!(has_colorless_pair_ability(&runner, land_a));
    assert!(has_until_event_grant_from(&runner, emrakul));
}

/// R3 (CR 601.2a + CR 601.2i + CR 605.3a, CR 400.7h): the granted ability pays
/// for the card's own cast from exile, and the grant ends once the spell
/// becomes cast.
#[test]
fn the_granted_ability_pays_the_cast_and_ends_when_the_card_is_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emrakul = add_emrakul(&mut scenario);
    let land_a = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    let index = hand_ability_index(&runner, emrakul);
    runner
        .activate(emrakul, index)
        .target_object(land_a)
        .resolve();

    // Reach: nothing but land_a can pay the {2}.
    assert!(p0_pool_is_empty(&runner));
    assert!(!runner.state().objects[&land_a].tapped);
    assert!(has_colorless_pair_ability(&runner, land_a));

    cast_with_auto_payment(&mut runner, emrakul);

    // Reach, asserted before passing priority (the cast trigger untaps lands).
    let card = &runner.state().objects[&emrakul];
    assert_eq!(card.zone, Zone::Stack);
    assert_eq!(card.cast_from_zone, Some(Zone::Exile));
    assert!(runner.state().objects[&land_a].tapped);

    assert!(!has_colorless_pair_ability(&runner, land_a));
    assert_eq!(mana_ability_count(&runner, land_a), 1);
    assert!(grant_effect_from(&runner, emrakul).is_none());
}

/// R4: each card's deadline is its own cast. An unrelated cast ends neither
/// grant, and casting one exiled card leaves the other card's grant.
#[test]
fn casting_one_exiled_card_leaves_another_cards_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emrakul = add_emrakul(&mut scenario);
    let outlaw = scenario
        .add_creature_to_hand_from_oracle(P0, "Glamorous Outlaw", 4, 3, GLAMOROUS_OUTLAW)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let bystander = scenario
        .add_creature_to_hand_from_oracle(P0, "Bystander", 2, 2, "")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let land_a = scenario.add_basic_land(P0, ManaColor::Green);
    let land_b = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_mana_pool(P0, colorless(5));
    let mut runner = scenario.build();

    let emrakul_index = hand_ability_index(&runner, emrakul);
    let outlaw_index = hand_ability_index(&runner, outlaw);
    runner
        .activate(emrakul, emrakul_index)
        .target_object(land_a)
        .resolve();
    runner
        .activate(outlaw, outlaw_index)
        .target_object(land_b)
        .resolve();

    // (i) An unrelated spell's cast ends neither grant.
    runner.cast(bystander).resolve();
    assert_eq!(runner.state().objects[&bystander].zone, Zone::Battlefield);
    assert!(has_colorless_pair_ability(&runner, land_a));
    assert_eq!(mana_ability_count(&runner, land_b), 2);
    assert!(has_until_event_grant_from(&runner, emrakul));
    assert!(has_until_event_grant_from(&runner, outlaw));

    // (ii) Emrakul's cast ends only Emrakul's grant.
    assert!(p0_pool_is_empty(&runner));
    cast_with_auto_payment(&mut runner, emrakul);
    let card = &runner.state().objects[&emrakul];
    assert_eq!(card.zone, Zone::Stack);
    assert_eq!(card.cast_from_zone, Some(Zone::Exile));
    assert!(!has_colorless_pair_ability(&runner, land_a));
    assert!(grant_effect_from(&runner, emrakul).is_none());

    assert_eq!(mana_ability_count(&runner, land_b), 2);
    assert!(has_until_event_grant_from(&runner, outlaw));
}

/// R5 (CR 903.9a + CR 903.8, CR 400.7): the commander leaves exile uncast to
/// the command zone, so it gets no permission, but the land keeps the
/// ability — and casting it from the command zone is not a cast from exile,
/// so the grant survives that cast too.
#[test]
fn the_grant_survives_when_the_card_leaves_exile_uncast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emrakul = scenario
        .add_creature_to_hand_from_oracle(P0, "Emrakul, the Exigent Doom", 12, 12, EMRAKUL)
        .with_mana_cost(ManaCost::generic(2))
        .commander()
        .id();
    let land_a = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_library_top(P0, &["P0 Draw"]);
    scenario.with_library_top(P1, &["P1 Draw"]);
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    let index = hand_ability_index(&runner, emrakul);
    runner
        .act(GameAction::ActivateAbility {
            source_id: emrakul,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating from hand is legal");

    // land_a is the only land, so the target is chosen without a prompt; the
    // pool pays {3}. CR 704.3: the command-zone choice comes before P0
    // receives priority, while the ability is on the stack.
    let ability_targets = runner
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .map(|ability| ability.targets.clone());
    assert_eq!(ability_targets, Some(vec![TargetRef::Object(land_a)]));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice {
                commander_id,
                current_zone: Zone::Exile,
                ..
            } if commander_id == emrakul
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("moving the commander to the command zone is legal");

    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is legal");
    }
    assert!(runner.state().stack.is_empty());

    // The card left exile before the ability resolved: no permission, but
    // the land has the grant.
    assert_eq!(runner.state().objects[&emrakul].zone, Zone::Command);
    assert!(!runner.state().objects[&emrakul]
        .casting_permissions
        .iter()
        .any(|permission| matches!(permission, CastingPermission::PlayFromExile { .. })));
    assert!(has_colorless_pair_ability(&runner, land_a));
    assert!(has_until_event_grant_from(&runner, emrakul));

    // CR 113.7a + CR 400.7: the deadline is bound to the exile incarnation the
    // activation's cost created — its last known information — not to the
    // command-zone object now stored under the same id.
    let (captured, expected_zone) = captured_source(
        grant_effect_from(&runner, emrakul).expect("the activation installed its grant"),
    );
    assert_eq!(expected_zone, Zone::Exile);
    assert_ne!(
        captured,
        ObjectIncarnationRef::from_object(&runner.state().objects[&emrakul])
    );

    let start = runner.state().turn_number;
    advance_to_next_own_main(&mut runner);
    assert!(runner.state().turn_number > start);
    assert!(has_colorless_pair_ability(&runner, land_a));

    assert!(offers_cast(&runner, emrakul));
    assert!(p0_pool_is_empty(&runner));
    cast_with_auto_payment(&mut runner, emrakul);

    // Reach: the cast came from the command zone and the granted ability paid.
    let card = &runner.state().objects[&emrakul];
    assert_eq!(card.zone, Zone::Stack);
    assert_eq!(card.cast_from_zone, Some(Zone::Command));
    assert_eq!(runner.state().commander_cast_count.get(&emrakul), Some(&1));
    assert!(runner.state().objects[&land_a].tapped);

    assert!(has_colorless_pair_ability(&runner, land_a));
    assert!(has_until_event_grant_from(&runner, emrakul));
}

/// R6 (CR 113.7a + CR 400.7 + CR 611.2a): two activations of one commander's
/// ability, with the card leaving exile and returning to hand in between
/// (CR 903.9b, declined), each bind their deadline to the exile incarnation
/// their own cost created. Casting the card from its later exile ends only the
/// later activation's grant; the earlier one is waiting on an object that no
/// longer exists.
#[test]
fn casting_the_card_again_after_it_left_exile_ends_only_its_own_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emrakul = scenario
        .add_creature_to_hand_from_oracle(P0, "Emrakul, the Exigent Doom", 12, 12, EMRAKUL)
        .with_mana_cost(ManaCost::generic(2))
        .commander()
        .id();
    let land_a = scenario.add_basic_land(P0, ManaColor::Green);
    let land_b = scenario.add_basic_land(P0, ManaColor::Green);
    let beacon = scenario
        .add_land_from_oracle(P0, "Command Beacon", COMMAND_BEACON)
        .id();
    scenario.with_library_top(P0, &["P0 Draw"]);
    scenario.with_library_top(P1, &["P1 Draw"]);
    scenario.with_mana_pool(P0, colorless(6));
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    let index = hand_ability_index(&runner, emrakul);

    // Activation 1: exile the commander as a cost, target land_a, and send the
    // commander to the command zone (CR 903.9a) while the ability is on the stack.
    runner
        .act(GameAction::ActivateAbility {
            source_id: emrakul,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating from hand is legal");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::TargetSelection { target_slots, .. }
                if target_slots.first().is_some_and(|slot| {
                    [land_a, land_b, beacon]
                        .iter()
                        .all(|land| slot.legal_targets.contains(&TargetRef::Object(*land)))
                })
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_a)),
        })
        .expect("targeting land_a is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice {
                commander_id,
                current_zone: Zone::Exile,
                ..
            } if commander_id == emrakul
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("moving the commander to the command zone is legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&emrakul].zone, Zone::Command);
    assert_eq!(runner.state().stack.len(), 1);

    // Command Beacon returns the commander to hand; the replacement is
    // optional (CR 903.9b), so P0 declines the command zone.
    let beacon_index = runner.state().objects[&beacon]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated && !is_mana_ability(ability))
        .expect("Command Beacon has its non-mana activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: beacon,
            ability_index: beacon_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Command Beacon is legal");
    assert_eq!(runner.state().stack.len(), 2);
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is legal");
    }
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Command Beacon's resolution asks where the commander goes: {:?}",
            runner.state().waiting_for
        );
    };
    let to_hand = candidates
        .iter()
        .position(|candidate| candidate.description == "Put into hand")
        .expect("the commander may be put into hand instead of the command zone");
    runner
        .act(GameAction::ChooseReplacement { index: to_hand })
        .expect("declining the command zone is legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&emrakul].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&beacon].zone, Zone::Graveyard);
    // Activation 1's ability is still on the stack, waiting on an exile
    // incarnation that no longer exists.
    assert_eq!(runner.state().stack.len(), 1);

    // Activation 2: exile it again as a cost, target land_b, and leave it in exile.
    runner
        .act(GameAction::ActivateAbility {
            source_id: emrakul,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating from hand again is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land_b)),
        })
        .expect("targeting land_b is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice {
                commander_id,
                current_zone: Zone::Exile,
                ..
            } if commander_id == emrakul
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("leaving the commander in exile is legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(runner.state().objects[&emrakul].zone, Zone::Exile);
    assert_eq!(runner.state().stack.len(), 2);

    pass_until_stack_len(&mut runner, 0);

    // Reach: both activations resolved. Each land has the grant, each grant has
    // its own deadline, and the card in exile holds exactly one permission
    // (activation 1 found no object to give one to).
    assert!(has_colorless_pair_ability(&runner, land_a));
    assert!(has_colorless_pair_ability(&runner, land_b));
    assert_eq!(runner.state().objects[&emrakul].zone, Zone::Exile);
    assert_eq!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .filter(|effect| effect.source_id == emrakul
                && matches!(effect.duration, Duration::UntilEvent { .. }))
            .count(),
        2
    );
    assert_eq!(
        runner.state().objects[&emrakul]
            .casting_permissions
            .iter()
            .filter(|permission| matches!(permission, CastingPermission::PlayFromExile { .. }))
            .count(),
        1,
        "{:?}",
        runner.state().objects[&emrakul].casting_permissions
    );
    assert!(p0_pool_is_empty(&runner));
    assert!(runner.state().stack.is_empty());

    // Each deadline is bound to its own activation's exile incarnation.
    let current = ObjectIncarnationRef::from_object(&runner.state().objects[&emrakul]);
    let (first_exile, first_zone) = captured_source(
        grant_effect_on(&runner, emrakul, land_a).expect("activation 1 installed its grant"),
    );
    let (second_exile, second_zone) = captured_source(
        grant_effect_on(&runner, emrakul, land_b).expect("activation 2 installed its grant"),
    );
    assert_eq!(first_zone, Zone::Exile);
    assert_ne!(first_exile, current);
    assert_eq!(second_zone, Zone::Exile);
    assert_eq!(second_exile, current);

    assert!(offers_cast(&runner, emrakul));
    cast_with_auto_payment(&mut runner, emrakul);

    // Reach, asserted before passing priority (the cast trigger untaps lands):
    // the card was cast from exile.
    let card = &runner.state().objects[&emrakul];
    assert_eq!(card.zone, Zone::Stack);
    assert_eq!(card.cast_from_zone, Some(Zone::Exile));

    // Casting that exile incarnation ends only the grant it created.
    assert!(!has_colorless_pair_ability(&runner, land_b));
    assert!(grant_effect_on(&runner, emrakul, land_b).is_none());
    assert!(has_colorless_pair_ability(&runner, land_a));
    assert!(grant_effect_on(&runner, emrakul, land_a).is_some());
}

/// R10 (CR 400.7j): the ability exiles its own source as part of its effect, so
/// the effect's other parts find the exiled card: "this card" in the deadline is
/// that exiled object, not the battlefield creature the ability came from.
/// Casting it from exile ends the grant.
#[test]
fn an_ability_that_exiles_its_own_source_binds_the_deadline_to_the_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Resolution Exiler", 1, 1, RESOLUTION_EXILER)
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("the source has an activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the activation is legal");
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(land)),
            })
            .expect("the land is a legal target");
    }
    runner.advance_until_stack_empty();

    // Reach: the ability exiled its source, granted the land, and let the
    // exiled card be cast.
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&source]
            .casting_permissions
            .iter()
            .filter(|permission| matches!(permission, CastingPermission::PlayFromExile { .. }))
            .count(),
        1,
        "{:?}",
        runner.state().objects[&source].casting_permissions
    );
    assert!(has_colorless_pair_ability(&runner, land));
    assert_eq!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .filter(|effect| effect.source_id == source
                && matches!(effect.duration, Duration::UntilEvent { .. }))
            .count(),
        1
    );

    // CR 400.7j: the deadline is bound to the exiled card.
    let captured = captured_source(
        grant_effect_from(&runner, source).expect("the ability installed its grant"),
    );
    assert_eq!(
        captured,
        (
            ObjectIncarnationRef::from_object(&runner.state().objects[&source]),
            Zone::Exile
        )
    );

    let _ = runner.cast(source).commit();

    // Reach: the card was cast from exile.
    let card = &runner.state().objects[&source];
    assert_eq!(card.zone, Zone::Stack);
    assert_eq!(card.cast_from_zone, Some(Zone::Exile));

    assert!(!has_colorless_pair_ability(&runner, land));
    assert!(grant_effect_from(&runner, source).is_none());
}
