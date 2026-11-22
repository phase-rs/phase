use engine::game::casting::display_spell_cost;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CardPlayMode, CastingPermission, Duration, PlayFromExileProvenance};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::{ObjectId, PlayerId};

const GORECLAW: &str = "Creature spells you cast with power 4 or greater cost {2} less to cast.\nWhenever Goreclaw attacks, each creature you control with power 4 or greater gets +1/+1 and gains trample until end of turn.";
const DORAN: &str = "Each creature spell you cast with toughness greater than its power costs {1} less to cast.\nWhenever a creature you control attacks or blocks, it gets +X/+X until end of turn, where X is the difference between its power and toughness.";
const WURM: &str = "Boneyard Wurm's power and toughness are each equal to the number of creature cards in your graveyard.";
const MARO: &str = "Maro's power and toughness are each equal to the number of cards in your hand.";
const FOREST: &str = "({T}: Add {G}.)";
const KARADOR: &str = "This spell costs {1} less to cast for each creature card in your graveyard.\nOnce during each of your turns, you may cast a creature spell from your graveyard.";
const DEFILER: &str = "Trample\nAs an additional cost to cast green permanent spells, you may pay 2 life. Those spells cost {G} less to cast if you paid life this way. This effect reduces only the amount of green mana you pay.\nWhenever you cast a green permanent spell, put a +1/+1 counter on each creature you control.";

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: shards.to_vec(),
    }
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn goreclaw(scenario: &mut GameScenario) {
    scenario
        .add_creature_from_oracle(P0, "Goreclaw, Terror of Qal Sisma", 4, 3, GORECLAW)
        .as_legendary();
}

fn mana(scenario: &mut GameScenario, player: PlayerId, generic: usize, green: usize, blue: usize) {
    let units = [
        (ManaType::Colorless, generic),
        (ManaType::Green, green),
        (ManaType::Blue, blue),
    ]
    .into_iter()
    .flat_map(|(kind, count)| {
        (0..count).map(move |_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
    })
    .collect();
    scenario.with_mana_pool(player, units);
}

fn remaining_mana(runner: &GameRunner, player: PlayerId) -> [usize; 3] {
    [ManaType::Colorless, ManaType::Green, ManaType::Blue].map(|kind| {
        runner.state().players[player.0 as usize]
            .mana_pool
            .count_color(kind)
    })
}

fn assert_cast(
    runner: &mut GameRunner,
    player: PlayerId,
    spell: ObjectId,
    displayed: ManaCost,
    paid: [usize; 3],
    pt: Option<(i32, i32)>,
) {
    assert_eq!(
        display_spell_cost(runner.state(), player, spell),
        Some(displayed)
    );
    let before = remaining_mana(runner, player);
    runner.cast(spell).commit();
    assert_stack(runner, player, spell, pt);
    let after = remaining_mana(runner, player);
    assert_eq!(
        std::array::from_fn::<_, 3, _>(|i| before[i] - after[i]),
        paid
    );
}

fn assert_stack(runner: &GameRunner, player: PlayerId, spell: ObjectId, pt: Option<(i32, i32)>) {
    let state = runner.state();
    let object = &state.objects[&spell];
    assert_eq!(object.zone, Zone::Stack);
    assert_eq!(object.controller, player);
    let mut entries = state.stack.iter().filter(|entry| entry.id == spell);
    assert_eq!(entries.next().expect("committed spell").controller, player);
    assert!(entries.next().is_none());
    if let Some((power, toughness)) = pt {
        assert_eq!(
            (object.power, object.toughness),
            (Some(power), Some(toughness))
        );
    }
}

fn graveyard_creatures(scenario: &mut GameScenario, count: usize) {
    for _ in 0..count {
        scenario.add_creature_to_graveyard(P0, "Centaur Courser", 3, 3);
    }
}

#[test]
fn goreclaw_cost_reduction_observes_power_and_card_type() {
    use ManaCostShard::{Blue, Green};

    for (name, power, printed, reducer, expected) in [
        (
            "Centaur Courser",
            3,
            (2, &[Green][..]),
            true,
            (2, &[Green][..]),
        ),
        (
            "Rumbling Baloth",
            4,
            (2, &[Green, Green][..]),
            true,
            (0, &[Green, Green][..]),
        ),
        (
            "Rumbling Baloth",
            4,
            (2, &[Green, Green][..]),
            false,
            (2, &[Green, Green][..]),
        ),
    ] {
        let mut scenario = scenario();
        if reducer {
            goreclaw(&mut scenario);
        }
        let spell = scenario
            .add_creature_to_hand(P0, name, power, power)
            .with_mana_cost(cost(printed.0, printed.1))
            .id();
        mana(&mut scenario, P0, 3, 3, 0);
        assert_cast(
            &mut scenario.build(),
            P0,
            spell,
            cost(expected.0, expected.1),
            [expected.0 as usize, expected.1.len(), 0],
            Some((power, power)),
        );
    }

    let mut divination = scenario();
    goreclaw(&mut divination);
    let spell = divination
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_mana_cost(cost(2, &[Blue]))
        .id();
    mana(&mut divination, P0, 3, 0, 2);
    assert_cast(
        &mut divination.build(),
        P0,
        spell,
        cost(2, &[Blue]),
        [2, 0, 1],
        None,
    );

    let mut vehicle = scenario();
    goreclaw(&mut vehicle);
    let spell = vehicle.add_artifact_to_hand_from_oracle(P0, "Aradara Express",
        "Menace\nCrew 4 (Tap any number of creatures you control with total power 4 or more: This Vehicle becomes an artifact creature until end of turn.)")
        .with_subtypes(vec!["Vehicle"])
        .with_mana_cost(cost(5, &[])).id();
    mana(&mut vehicle, P0, 6, 0, 0);
    let mut runner = vehicle.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    object.power = Some(8);
    object.toughness = Some(6);
    object.base_power = Some(8);
    object.base_toughness = Some(6);
    assert_cast(&mut runner, P0, spell, cost(5, &[]), [5, 0, 0], None);
}

#[test]
fn goreclaw_does_not_reduce_an_opponents_spell() {
    use ManaCostShard::Green;
    let mut scenario = scenario();
    goreclaw(&mut scenario);
    let spell = scenario
        .add_creature_to_hand(P1, "Rumbling Baloth", 4, 4)
        .with_mana_cost(cost(2, &[Green, Green]))
        .id();
    mana(&mut scenario, P1, 3, 3, 0);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    assert_cast(
        &mut runner,
        P1,
        spell,
        cost(2, &[Green, Green]),
        [2, 2, 0],
        Some((4, 4)),
    );
}

#[test]
fn maro_reads_the_hand_after_leaving_it() {
    use ManaCostShard::Green;
    for (other_cards, generic) in [(3, 2), (4, 0)] {
        let mut scenario = scenario();
        goreclaw(&mut scenario);
        let spell = scenario
            .add_creature_to_hand_from_oracle(P0, "Maro", 0, 0, MARO)
            .with_mana_cost(cost(2, &[Green, Green]))
            .id();
        for _ in 0..other_cards {
            scenario
                .add_land_to_hand(P0, "Forest")
                .from_oracle_text(FOREST);
        }
        mana(&mut scenario, P0, 3, 3, 0);
        let mut runner = scenario.build();
        assert!(runner.state().players[0].hand.contains(&spell));
        assert_cast(
            &mut runner,
            P0,
            spell,
            cost(generic, &[Green, Green]),
            [generic as usize, 2, 0],
            Some((other_cards, other_cards)),
        );
        assert!(!runner.state().players[0].hand.contains(&spell));
    }
}

#[test]
fn maro_reads_the_non_owner_casters_hand() {
    use ManaCostShard::Green;
    let mut scenario = scenario();
    goreclaw(&mut scenario);
    for _ in 0..4 {
        scenario
            .add_land_to_hand(P0, "Forest")
            .from_oracle_text(FOREST);
    }
    let spell = scenario
        .add_creature_to_exile(P1, "Maro", 0, 0)
        .from_oracle_text(MARO)
        .with_mana_cost(cost(2, &[Green, Green]))
        .id();
    mana(&mut scenario, P0, 3, 3, 0);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions
        .push(CastingPermission::PlayFromExile {
            provenance: PlayFromExileProvenance::Impulse,
            duration: Duration::UntilEndOfTurn,
            granted_to: P0,
            mode: CardPlayMode::Play,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
            land_enter_tapped: EtbTapState::Unspecified,
        });
    assert_cast(
        &mut runner,
        P0,
        spell,
        cost(0, &[Green, Green]),
        [0, 2, 0],
        Some((4, 4)),
    );
    assert_eq!(runner.state().objects[&spell].owner, P1);
}

#[test]
fn wurm_cost_and_stack_pt_follow_graveyard_count() {
    use ManaCostShard::Green;
    for (count, generic) in [(3, 1), (4, 0)] {
        let mut scenario = scenario();
        goreclaw(&mut scenario);
        graveyard_creatures(&mut scenario, count);
        let spell = scenario
            .add_creature_to_hand_from_oracle(P0, "Boneyard Wurm", 0, 0, WURM)
            .with_mana_cost(cost(1, &[Green]))
            .id();
        mana(&mut scenario, P0, 2, 2, 0);
        assert_cast(
            &mut scenario.build(),
            P0,
            spell,
            cost(generic, &[Green]),
            [generic as usize, 1, 0],
            Some((count as i32, count as i32)),
        );
    }
}

#[test]
fn graveyard_wurm_lock_recollects_after_defiler_election() {
    use ManaCostShard::Green;
    for (other_creatures, generic) in [(3, 1), (4, 0)] {
        let mut scenario = scenario();
        goreclaw(&mut scenario);
        graveyard_creatures(&mut scenario, other_creatures);
        scenario
            .add_creature_from_oracle(P0, "Karador, Ghost Chieftain", 3, 4, KARADOR)
            .as_legendary();
        scenario.add_creature_from_oracle(P0, "Defiler of Vigor", 6, 6, DEFILER);
        let spell = scenario
            .add_creature_to_graveyard(P0, "Boneyard Wurm", 0, 0)
            .from_oracle_text(WURM)
            .with_mana_cost(cost(1, &[Green]))
            .id();
        mana(&mut scenario, P0, 2, 2, 0);
        let mut runner = scenario.build();
        assert_eq!(
            display_spell_cost(runner.state(), P0, spell),
            Some(cost(generic, &[Green]))
        );
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("announce graveyard cast");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DefilerPayment { .. }
        ));
        let life = runner.state().players[0].life;
        runner
            .act(GameAction::DecideOptionalCost { index: 0 })
            .expect("accept Defiler's life cost");
        assert_eq!(runner.state().players[0].life, life - 2);
        assert_stack(
            &runner,
            P0,
            spell,
            Some((other_creatures as i32, other_creatures as i32)),
        );
        assert_eq!(remaining_mana(&runner, P0), [2 - generic as usize, 2, 0]);
    }
}

#[test]
fn doran_requires_toughness_strictly_greater_than_power() {
    use ManaCostShard::Green;
    for (name, oracle, printed, stack_pt, generic) in [
        (
            "Wall of Blossoms",
            "Defender\nWhen this creature enters, draw a card.",
            (0, 4),
            (0, 4),
            0,
        ),
        ("Boneyard Wurm", WURM, (0, 0), (4, 4), 1),
    ] {
        let mut scenario = scenario();
        scenario
            .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN)
            .as_legendary();
        graveyard_creatures(&mut scenario, 4);
        let spell = scenario
            .add_creature_to_hand_from_oracle(P0, name, printed.0, printed.1, oracle)
            .with_mana_cost(cost(1, &[Green]))
            .id();
        mana(&mut scenario, P0, 2, 2, 0);
        assert_cast(
            &mut scenario.build(),
            P0,
            spell,
            cost(generic, &[Green]),
            [generic as usize, 1, 0],
            Some(stack_pt),
        );
    }
}

#[test]
fn wurm_stack_pt_updates_when_an_instant_mills_a_creature() {
    use ManaCostShard::{Blue, Green};

    let mut scenario = scenario();
    goreclaw(&mut scenario);
    graveyard_creatures(&mut scenario, 3);
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Boneyard Wurm", 0, 0, WURM)
        .with_mana_cost(cost(1, &[Green]))
        .id();
    scenario
        .add_land_to_library_top(P0, "Forest")
        .from_oracle_text(FOREST);
    let milled = scenario
        .add_spell_to_library_top(P0, "Centaur Courser", false)
        .as_creature()
        .with_subtypes(vec!["Centaur", "Warrior"])
        .with_mana_cost(cost(2, &[Green]))
        .id();
    scenario
        .add_land_to_library_top(P1, "Forest")
        .from_oracle_text(FOREST);
    let scour = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Thought Scour",
            true,
            "Target player mills two cards.\nDraw a card.",
        )
        .with_mana_cost(cost(0, &[Blue]))
        .id();
    mana(&mut scenario, P0, 2, 2, 0);
    mana(&mut scenario, P1, 0, 0, 1);
    let mut runner = scenario.build();
    assert_cast(
        &mut runner,
        P0,
        spell,
        cost(1, &[Green]),
        [1, 1, 0],
        Some((3, 3)),
    );
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0) {
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to opponent");
    }
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    runner.cast(scour).target_player(P0).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&milled].zone, Zone::Graveyard);
    assert_stack(&runner, P0, spell, Some((4, 4)));
}
