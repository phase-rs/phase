//! CR 701.21a + CR 601.2h + CR 118.3: "sacrifice all <filter>" is a determined
//! set — every matching permanent the payer controls — paid whole on both the
//! spell additional-cost and activation channels. CR 404.3: the payer arranges
//! only the cards in it that the payer owns; it never chooses members.

use engine::game::engine::EngineError;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CostPaidObjectRecord, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::{ActionResult, CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::TargetRef;

const SOULBLAST: &str = "As an additional cost to cast this spell, sacrifice all creatures you control.\nSoulblast deals damage to any target equal to the total power of the sacrificed creatures.";
const TOMB_OF_URAMI: &str = "{T}: Add {B}. Tomb of Urami deals 1 damage to you if you don't control an Ogre.\n{2}{B}{B}, {T}, Sacrifice all lands you control: Create Urami, a legendary 5/5 black Demon Spirit creature token with flying.";
const RATHS_EDGE: &str =
    "{T}: Add {C}.\n{4}, {T}, Sacrifice a land: Rath's Edge deals 1 damage to any target.";
const BLOOD_PET: &str = "Sacrifice this creature: Add {B}.";
const ANGEL_OF_JUBILATION: &str = "Flying\nOther nonblack creatures you control get +1/+1.\nPlayers can't pay life or sacrifice creatures to cast spells or activate abilities.";

fn add_soulblast(sc: &mut GameScenario) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(P0, "Soulblast", true, SOULBLAST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red; 3],
            generic: 3,
        })
        .id()
}

fn cast(runner: &mut GameRunner, spell: ObjectId, mode: CastPaymentMode) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: mode,
        })
        .unwrap();
}

/// Answers target prompts with P1 until the cast waits on anything else.
fn settle_targets(runner: &mut GameRunner) {
    while let WaitingFor::TargetSelection { .. } = &runner.state().waiting_for {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .unwrap();
    }
}

/// Answers the CR 404.3 arrangement prompt — every advertised card, none optional — with
/// `order`, or with the advertised order; returns the order paid.
fn arrange(runner: &mut GameRunner, order: Option<&[ObjectId]>) -> Vec<ObjectId> {
    let WaitingFor::PayCost {
        kind: PayCostKind::Sacrifice,
        choices,
        count,
        min_count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the arrangement prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*min_count, *count), (choices.len(), choices.len()));
    let order = order.map_or_else(|| choices.clone(), <[_]>::to_vec);
    runner
        .act(GameAction::SelectCards {
            cards: order.clone(),
        })
        .unwrap();
    order
}

fn activate_mana(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn top_ability(runner: &GameRunner) -> &ResolvedAbility {
    runner
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability())
        .expect("a stack entry with a resolved ability")
}

fn paid_ids(ability: &ResolvedAbility) -> Vec<ObjectId> {
    ability
        .cost_paid_objects
        .iter()
        .map(CostPaidObjectRecord::object_id)
        .collect()
}

fn paid_snapshot_tapped(ability: &ResolvedAbility, id: ObjectId) -> bool {
    ability
        .cost_paid_objects
        .iter()
        .filter(|record| record.object_id() == id)
        .find_map(CostPaidObjectRecord::snapshot)
        .expect("captured snapshot")
        .lki
        .tapped
}

fn assert_spell_on_stack(runner: &GameRunner, spell: ObjectId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.back().map(|e| e.id), Some(spell));
}

#[test]
fn soulblast_sacrifices_every_creature_its_caster_controls_at_payment() {
    for mode in [CastPaymentMode::Manual, CastPaymentMode::Auto] {
        let mut sc = GameScenario::new();
        sc.at_phase(Phase::PreCombatMain);
        let dork = sc
            .add_creature_from_oracle(P0, "Red Dork", 1, 1, "{T}: Add {R}.")
            .id();
        let vanilla = sc.add_vanilla(P0, 2, 2);
        let mountains: Vec<_> = (0..5)
            .map(|_| sc.add_basic_land(P0, ManaColor::Red))
            .collect();
        let theirs = sc.add_vanilla(P1, 3, 3);
        let spell = add_soulblast(&mut sc);
        let mut runner = sc.build();

        cast(&mut runner, spell, mode);
        settle_targets(&mut runner);
        if mode == CastPaymentMode::Manual {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
                "{:?}",
                runner.state().waiting_for
            );
            activate_mana(&mut runner, dork);
            for &land in &mountains {
                activate_mana(&mut runner, land);
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(arrange(&mut runner, None), vec![dork, vanilla], "{mode:?}");

        assert_spell_on_stack(&runner, spell);
        assert_eq!(zone(&runner, vanilla), Zone::Graveyard, "{mode:?}");
        assert_eq!(zone(&runner, dork), Zone::Graveyard, "{mode:?}");
        assert_eq!(zone(&runner, theirs), Zone::Battlefield, "{mode:?}");

        // The placeholder spell carries no stack ability, so the spell object's
        // singular referent (the first member, the dork) is the observable record.
        let referent = runner.state().objects[&spell]
            .cast_cost_paid_object
            .as_ref()
            .expect("the determined set publishes the spell's cost-paid referent");
        assert_eq!(referent.object_id, dork, "{mode:?}");
        if mode == CastPaymentMode::Manual {
            // CR 601.2g before CR 601.2h: the dork tapped for mana, then was paid.
            assert!(referent.lki.tapped);
        }
    }
}

#[test]
fn soulblast_member_sacrificed_for_mana_in_the_window_is_not_reserved() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let pet = sc
        .add_creature_from_oracle(P0, "Blood Pet", 1, 1, BLOOD_PET)
        .id();
    let vanilla = sc.add_vanilla(P0, 2, 2);
    let mountains: Vec<_> = (0..5)
        .map(|_| sc.add_basic_land(P0, ManaColor::Red))
        .collect();
    let spell = add_soulblast(&mut sc);
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    settle_targets(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    activate_mana(&mut runner, pet);
    assert_eq!(zone(&runner, pet), Zone::Graveyard);
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Black),
        1
    );
    for &land in &mountains {
        activate_mana(&mut runner, land);
    }
    runner.act(GameAction::PassPriority).unwrap();

    assert_spell_on_stack(&runner, spell);
    assert_eq!(zone(&runner, vanilla), Zone::Graveyard);
    let p0_graveyard = &runner.state().players[0].graveyard;
    assert_eq!(p0_graveyard.iter().filter(|&&id| id == pet).count(), 1);
}

#[test]
fn soulblast_is_payable_with_no_creatures() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let mountains: Vec<_> = (0..6)
        .map(|_| sc.add_basic_land(P0, ManaColor::Red))
        .collect();
    let spell = add_soulblast(&mut sc);
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Auto);
    settle_targets(&mut runner);

    assert_spell_on_stack(&runner, spell);
    assert!(mountains
        .iter()
        .all(|land| runner.state().objects[land].tapped));
}

/// Soulblast with P0's 2/2 and six Mountains, plus Angel of Jubilation when
/// `with_angel`; returns the result of casting it.
fn cast_soulblast_beside(
    with_angel: bool,
) -> (
    GameRunner,
    ObjectId,
    ObjectId,
    Result<ActionResult, EngineError>,
) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    if with_angel {
        sc.add_creature_from_oracle(P0, "Angel of Jubilation", 3, 3, ANGEL_OF_JUBILATION);
    }
    let vanilla = sc.add_vanilla(P0, 2, 2);
    for _ in 0..6 {
        sc.add_basic_land(P0, ManaColor::Red);
    }
    let spell = add_soulblast(&mut sc);
    let mut runner = sc.build();
    let card_id = runner.state().objects[&spell].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    (runner, spell, vanilla, result)
}

#[test]
fn soulblast_over_a_member_that_cannot_be_sacrificed_is_unpayable() {
    let (runner, spell, vanilla, result) = cast_soulblast_beside(true);

    let Err(EngineError::ActionNotAllowed(message)) = result else {
        panic!("the cast must be refused, got {result:?}");
    };
    assert_eq!(
        message,
        "A permanent in the determined sacrifice set can't be sacrificed"
    );
    assert_eq!(zone(&runner, spell), Zone::Hand);
    assert_eq!(zone(&runner, vanilla), Zone::Battlefield);
}

#[test]
fn soulblast_without_the_angel_sacrifices_its_casters_creatures() {
    let (mut runner, spell, vanilla, result) = cast_soulblast_beside(false);

    result.expect("the cast is offered");
    settle_targets(&mut runner);
    assert_spell_on_stack(&runner, spell);
    assert_eq!(zone(&runner, vanilla), Zone::Graveyard);
}

#[test]
fn tomb_of_urami_sacrifices_all_its_controllers_lands_itself_included() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let tomb = sc
        .add_land_from_oracle(P0, "Tomb of Urami", TOMB_OF_URAMI)
        .id();
    let mut lands: Vec<_> = (0..4)
        .map(|_| sc.add_basic_land(P0, ManaColor::Black))
        .collect();
    lands.push(sc.add_basic_land(P0, ManaColor::Green));
    let theirs = sc.add_basic_land(P1, ManaColor::Black);
    let mut runner = sc.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: tomb,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    settle_targets(&mut runner);
    arrange(&mut runner, None);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, tomb), Zone::Graveyard);
    assert!(lands
        .iter()
        .all(|&land| zone(&runner, land) == Zone::Graveyard));
    assert_eq!(zone(&runner, theirs), Zone::Battlefield);
    let ids = paid_ids(top_ability(&runner));
    assert!(ids.contains(&tomb) && lands.iter().all(|land| ids.contains(land)));
    assert!(!ids.contains(&theirs));
    // CR 602.2b: Tomb's own {T} was paid before it was sacrificed.
    assert!(paid_snapshot_tapped(top_ability(&runner), tomb));

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(runner
        .battlefield_names()
        .iter()
        .any(|name| name.starts_with("Urami")));
}

/// Rath's Edge plus four other lands; activates its damage ability at P1 and
/// selects itself or another land for the "Sacrifice a land" cost.
fn activate_raths_edge(pick_self: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let edge = sc.add_land_from_oracle(P0, "Rath's Edge", RATHS_EDGE).id();
    let other = sc.add_basic_land(P0, ManaColor::Red);
    for _ in 0..3 {
        sc.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = sc.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: edge,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    loop {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .unwrap();
            }
            WaitingFor::PayCost { choices, .. } => {
                assert!(choices.contains(&edge) && choices.contains(&other));
                assert!(
                    !runner.state().objects[&edge].tapped,
                    "hoist left Rath's Edge untapped"
                );
                let pick = if pick_self { edge } else { other };
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .unwrap();
            }
            _ => break,
        }
    }
    (runner, edge, other)
}

#[test]
fn raths_edge_may_sacrifice_itself_after_paying_its_own_tap() {
    let (mut runner, edge, _) = activate_raths_edge(true);
    let life_before = runner.state().players[1].life;
    assert_eq!(zone(&runner, edge), Zone::Graveyard);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[1].life, life_before - 1);
}

#[test]
fn raths_edge_sacrificing_another_land_stays_tapped_on_the_battlefield() {
    let (mut runner, edge, other) = activate_raths_edge(false);
    let life_before = runner.state().players[1].life;
    assert_eq!(zone(&runner, other), Zone::Graveyard);
    assert_eq!(zone(&runner, edge), Zone::Battlefield);
    assert!(runner.state().objects[&edge].tapped);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[1].life, life_before - 1);
}

const ASSAULT_SUIT: &str = "Equipped creature gets +2/+2, has haste, can't attack you or planeswalkers you control, and can't be sacrificed.\nAt the beginning of each opponent's upkeep, you may have that player gain control of equipped creature until end of turn. If you do, untap it.\nEquip {3}";
const KRARK_CLAN_IRONWORKS: &str = "Sacrifice an artifact: Add {C}{C}.";

fn equip(runner: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&equipment)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
    runner.state_mut().layers_dirty.mark_full();
}

fn add_assault_suit(sc: &mut GameScenario) -> ObjectId {
    sc.add_artifact_from_oracle(P0, "Assault Suit", ASSAULT_SUIT)
        .with_subtypes(vec!["Equipment"])
        .id()
}

/// CR 601.2g + CR 601.2h: whether a member can be sacrificed is read when the
/// cost is paid, after the mana window. Sacrificing Assault Suit to Krark-Clan
/// Ironworks there lets its creature be sacrificed; without that, the payment
/// is refused and nothing is sacrificed.
#[test]
fn soulblast_pays_for_a_creature_freed_by_sacrificing_its_assault_suit_for_mana() {
    for sacrifice_suit in [true, false] {
        let mut sc = GameScenario::new();
        sc.at_phase(Phase::PreCombatMain);
        let suited = sc.add_vanilla(P0, 2, 2);
        let other = sc.add_vanilla(P0, 3, 3);
        let suit = add_assault_suit(&mut sc);
        let ironworks = sc
            .add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KRARK_CLAN_IRONWORKS)
            .id();
        let mountains: Vec<_> = (0..6)
            .map(|_| sc.add_basic_land(P0, ManaColor::Red))
            .collect();
        let spell = add_soulblast(&mut sc);
        let mut runner = sc.build();
        equip(&mut runner, suit, suited);

        cast(&mut runner, spell, CastPaymentMode::Manual);
        settle_targets(&mut runner);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
            "CR 601.2g: the mana window opens, got {:?}",
            runner.state().waiting_for
        );
        if sacrifice_suit {
            activate_mana(&mut runner, ironworks);
            runner
                .act(GameAction::SelectCards { cards: vec![suit] })
                .expect("Assault Suit is sacrificed for {C}{C}");
        }
        for &land in &mountains {
            activate_mana(&mut runner, land);
        }
        let result = runner.act(GameAction::PassPriority);

        if sacrifice_suit {
            result.expect("the determined set is paid");
            arrange(&mut runner, None);
            assert_spell_on_stack(&runner, spell);
            assert_eq!(zone(&runner, suit), Zone::Graveyard);
            assert_eq!(zone(&runner, suited), Zone::Graveyard);
            assert_eq!(zone(&runner, other), Zone::Graveyard);
        } else {
            let Err(EngineError::ActionNotAllowed(message)) = result else {
                panic!("the payment must be refused, got {result:?}");
            };
            assert_eq!(
                message,
                "A permanent in the determined sacrifice set can't be sacrificed"
            );
            assert_eq!(zone(&runner, suited), Zone::Battlefield);
            assert_eq!(zone(&runner, other), Zone::Battlefield);
        }
    }
}

/// CR 602.2b + CR 601.2g: an activation's determined set is read at payment
/// too. Dryad Arbor wearing Assault Suit can be sacrificed to Tomb of Urami
/// once the Suit is sacrificed to Krark-Clan Ironworks in the mana window.
#[test]
fn tomb_of_urami_pays_for_a_land_freed_by_sacrificing_its_assault_suit_for_mana() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let tomb = sc
        .add_land_from_oracle(P0, "Tomb of Urami", TOMB_OF_URAMI)
        .id();
    let swamps: Vec<_> = (0..4)
        .map(|_| sc.add_basic_land(P0, ManaColor::Black))
        .collect();
    let arbor = sc
        .add_creature(P0, "Dryad Arbor", 1, 1)
        .as_land()
        .as_creature()
        .id();
    let suit = add_assault_suit(&mut sc);
    let ironworks = sc
        .add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KRARK_CLAN_IRONWORKS)
        .id();
    let mut runner = sc.build();
    equip(&mut runner, suit, arbor);

    runner
        .act(GameAction::ActivateAbility {
            source_id: tomb,
            ability_index: 1,
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the activation is offered");
    activate_mana(&mut runner, ironworks);
    runner
        .act(GameAction::SelectCards { cards: vec![suit] })
        .expect("Assault Suit is sacrificed for {C}{C}");
    for &swamp in &swamps {
        activate_mana(&mut runner, swamp);
    }
    runner.act(GameAction::PassPriority).unwrap();
    arrange(&mut runner, None);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    for id in swamps.iter().chain([&tomb, &arbor]) {
        assert_eq!(zone(&runner, *id), Zone::Graveyard);
    }
}

/// CR 404.3 + CR 602.2b: Tomb of Urami's sacrificed lands reach the graveyard in the order its
/// controller arranged, Tomb's own {T} paid first.
#[test]
fn tomb_of_urami_owner_orders_its_sacrificed_lands() {
    for tomb_first in [false, true] {
        let mut sc = GameScenario::new();
        sc.at_phase(Phase::PreCombatMain);
        let tomb = sc
            .add_land_from_oracle(P0, "Tomb of Urami", TOMB_OF_URAMI)
            .id();
        let swamp = sc.add_basic_land(P0, ManaColor::Black);
        sc.with_mana_pool(
            P0,
            (0..4)
                .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = sc.build();
        let order = if tomb_first {
            [tomb, swamp]
        } else {
            [swamp, tomb]
        };

        runner
            .act(GameAction::ActivateAbility {
                source_id: tomb,
                ability_index: 1,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        settle_targets(&mut runner);
        arrange(&mut runner, Some(&order));

        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        let graveyard: Vec<ObjectId> = runner.state().players[0]
            .graveyard
            .iter()
            .copied()
            .filter(|id| order.contains(id))
            .collect();
        assert_eq!(graveyard, order);
        assert!(paid_snapshot_tapped(top_ability(&runner), tomb));
    }
}

/// CR 404.3 + CR 701.21a: Tomb of Urami is its controller's only card in the set, so no
/// arrangement is asked; the stolen land goes to its owner's graveyard with it.
#[test]
fn tomb_of_urami_with_a_stolen_land_sacrifices_without_a_prompt() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let tomb = sc
        .add_land_from_oracle(P0, "Tomb of Urami", TOMB_OF_URAMI)
        .id();
    let stolen = sc
        .add_land_from_oracle(P1, "Swamp", "({T}: Add {B}.)")
        .controlled_by(P0)
        .id();
    sc.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = sc.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: tomb,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    settle_targets(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, tomb), Zone::Graveyard);
    assert!(runner.state().players[1].graveyard.contains(&stolen));
}
