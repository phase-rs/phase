//! Conditional self P/T equality: full Oracle cards through production actions.

use super::rules::{
    AttackTarget, GameAction, GameEvent, GameRunner, GameScenario, ObjectId, Phase, WaitingFor,
    Zone, P0, P1,
};
use engine::types::ability::{ChosenAttribute, EffectKind, TargetRef};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const LIEGE: &str = "As long as Gaea's Liege isn't attacking, its power and toughness are each equal to the number of Forests you control. As long as Gaea's Liege is attacking, its power and toughness are each equal to the number of Forests defending player controls.\n{T}: Target land becomes a Forest until this creature leaves the battlefield.";
const GODHEAD: &str = "Flying\nOther creatures have base power and toughness 1/1.";
const MOB: &str = "Trample\nDuring your turn, Angry Mob's power and toughness are each equal to 2 plus the number of Swamps your opponents control. During turns other than yours, Angry Mob's power and toughness are each 2.";
const MARO: &str = "Maro's power and toughness are each equal to the number of cards in your hand.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const MANDATE: &str = "Cast this spell only during combat.\nYour opponents can't cast spells this turn.\nEnd the combat phase. (Remove all attackers and blockers from combat. Exile all spells and abilities from the stack, including this spell.)";
const TIME_STOP: &str = "End the turn. (Exile all spells and abilities, including this spell. The player whose turn it is discards down to their maximum hand size. Damage heals and \"this turn\" and \"until end of turn\" effects end.)";

fn forests(s: &mut GameScenario, player: PlayerId, count: usize) {
    for _ in 0..count {
        s.add_basic_land(player, ManaColor::Green);
    }
}
fn scenario() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Filler A", "Filler B"]);
    s.with_library_top(P1, &["Filler C", "Filler D"]);
    s
}
fn liege(s: &mut GameScenario) -> ObjectId {
    // The battlefield builder places a pre-existing, untapped creature from a
    // prior turn, so combat and {T} costs exercise legal nonsick sources.
    s.add_creature_from_oracle(P0, "Gaea's Liege", 0, 0, LIEGE)
        .id()
}
fn fund(s: &mut GameScenario, color: ManaType, count: usize) {
    s.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
}
fn liege_spell(s: &mut GameScenario) -> ObjectId {
    s.add_creature_to_hand_from_oracle(P0, "Gaea's Liege", 0, 0, LIEGE)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green; 3],
        })
        .id()
}
fn pt(r: &GameRunner, id: ObjectId, n: i32) {
    assert_eq!(r.state().objects[&id].zone, Zone::Battlefield);
    assert_eq!(
        (
            r.state().objects[&id].power,
            r.state().objects[&id].toughness
        ),
        (Some(n), Some(n))
    );
}
fn complete(r: &GameRunner) {
    assert!(r.state().stack.is_empty());
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        r.state().waiting_for
    );
}
fn attack(r: &mut GameRunner, attacks: &[(ObjectId, AttackTarget)]) {
    r.advance_to_combat();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    r.declare_attackers(attacks)
        .expect("legal attack declaration");
    for (id, target) in attacks {
        assert!(r
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|a| a.object_id == *id && a.attack_target == *target));
    }
}
// CR 117.4 + CR 509.1: cross combat through public priority and blocker actions.
fn finish_combat(r: &mut GameRunner) {
    for _ in 0..48 {
        if r.state().phase == Phase::PostCombatMain {
            return;
        }
        match r.state().waiting_for {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::DeclareBlockers { .. } => {
                r.declare_blockers(&[]).unwrap();
            }
            ref other => panic!("unexpected combat window: {other:?}"),
        }
    }
    panic!("combat did not end");
}

// CR 611.3a + CR 704.5f: four live Forests keep the real */* card alive;
// the paired zero-Forest case must instead reach the graveyard after resolution.
#[test]
fn gaeas_liege_cast_survives_only_with_controlled_forests() {
    for count in [4, 0] {
        let mut s = scenario();
        if count == 4 {
            forests(&mut s, P0, 3);
            s.add_land_from_oracle(P0, "Tropical Island", "{T}: Add {G} or {U}.")
                .with_subtypes(vec!["Forest", "Island"]);
        }
        forests(&mut s, P1, 2);
        s.add_basic_land(P0, ManaColor::White);
        let hand_forest = s
            .add_land_to_hand(P0, "Forest")
            .with_subtypes(vec!["Forest"])
            .id();
        let graveyard_forest = s
            .add_land_to_graveyard(P0, "Forest")
            .with_subtypes(vec!["Forest"])
            .id();
        let id = liege_spell(&mut s);
        fund(&mut s, ManaType::Green, 6);
        let mut r = s.build();
        assert_eq!(r.state().objects[&hand_forest].zone, Zone::Hand);
        assert_eq!(
            r.state().objects[&hand_forest].card_types.subtypes,
            vec!["Forest"]
        );
        assert_eq!(r.state().objects[&graveyard_forest].zone, Zone::Graveyard);
        assert_eq!(
            r.state().objects[&graveyard_forest].card_types.subtypes,
            vec!["Forest"]
        );
        r.cast(id).resolve();
        complete(&r);
        if count == 0 {
            assert_eq!(r.state().objects[&id].zone, Zone::Graveyard);
        } else {
            pt(&r, id, 4);
        }
    }
}

// CR 508.5 + CR 611.3a: only this source's combat membership selects the
// defending-player census; ending combat restores the controller's census.
#[test]
fn gaeas_liege_attack_switches_immediately_and_restores() {
    for attacks_itself in [true, false] {
        let mut s = scenario();
        forests(&mut s, P0, 4);
        forests(&mut s, P1, 2);
        let id = liege(&mut s);
        let other = s.add_creature(P0, "Other attacker", 1, 1).id();
        let mut r = s.build();
        r.advance_to_combat();
        pt(&r, id, 4);
        attack(
            &mut r,
            &[(
                if attacks_itself { id } else { other },
                AttackTarget::Player(P1),
            )],
        );
        pt(&r, id, if attacks_itself { 2 } else { 4 });
        assert!(r.state().transient_continuous_effects.is_empty());
        finish_combat(&mut r);
        assert!(r.state().combat.is_none());
        complete(&r);
        pt(&r, id, 4);
    }
}

// CR 724.2d + CR 611.3a: forced combat exit must refresh live statics through
// the same removal authority as normal combat, without an expiring effect.
#[test]
fn gaeas_liege_mandate_restores_after_forced_combat_exit() {
    let mut s = scenario();
    forests(&mut s, P0, 4);
    forests(&mut s, P1, 2);
    let id = liege(&mut s);
    let mandate = s
        .add_spell_to_hand_from_oracle(P0, "Mandate of Peace", true, MANDATE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let mut r = s.build();
    attack(&mut r, &[(id, AttackTarget::Player(P1))]);
    pt(&r, id, 2);
    assert!(r.state().transient_continuous_effects.is_empty());
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // Phase transitions empty mana pools, so fund the actual cast window.
    for _ in 0..2 {
        r.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::White,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let outcome = r.cast(mandate).resolve();
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::EndCombatPhase, source_id, .. }
            if *source_id == mandate)));
    assert_eq!(r.state().objects[&mandate].zone, Zone::Exile);
    assert_eq!(r.state().phase, Phase::PostCombatMain);
    assert!(r.state().combat.is_none());
    complete(&r);
    pt(&r, id, 4);
}

// CR 724.1d + CR 611.3a + CR 514.1: observe restoration at the mandatory
// cleanup discard, before a new turn could incidentally refresh the layers.
#[test]
fn gaeas_liege_time_stop_restores_before_cleanup_discard() {
    for during_combat in [true, false] {
        let mut s = scenario();
        forests(&mut s, P0, 3);
        let forest = s.add_basic_land(P0, ManaColor::Green);
        forests(&mut s, P1, 2);
        let id = liege(&mut s);
        let hand: Vec<_> = (0..8)
            .map(|_| {
                s.add_land_to_hand(P0, "Forest")
                    .with_subtypes(vec!["Forest"])
                    .id()
            })
            .collect();
        let time_stop = s
            .add_spell_to_hand_from_oracle(P0, "Time Stop", true, TIME_STOP)
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Blue; 2],
            })
            .id();
        let mut r = s.build();
        if during_combat {
            attack(&mut r, &[(id, AttackTarget::Player(P1))]);
            pt(&r, id, 2);
        } else {
            // An ordinary mana activation settles the scenario's initial
            // derived state while retaining precombat priority for the cast.
            r.activate(forest, 0).resolve();
            complete(&r);
            assert_eq!(r.state().phase, Phase::PreCombatMain);
            pt(&r, id, 4);
        }
        assert!(r.state().transient_continuous_effects.is_empty());
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        let turn = r.state().turn_number;
        for _ in 0..6 {
            r.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Blue,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        let outcome = r.cast(time_stop).resolve();
        assert!(outcome.events().iter().any(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::EndTheTurn, source_id, .. }
                if *source_id == time_stop)));
        // The resolution driver stops at empty-stack priority. Two public
        // passes re-enter cleanup's hand-size action; guard every observation
        // so a later turn or incidental layer refresh cannot rescue this test.
        for passes in 0..=2 {
            assert_eq!(r.state().phase, Phase::Cleanup);
            assert_eq!(r.state().active_player, P0);
            assert_eq!(r.state().turn_number, turn);
            assert!(r.state().stack.is_empty());
            assert!(r.state().combat.is_none());
            assert_eq!(r.state().players[0].hand.len(), hand.len());
            assert!(hand.iter().all(|id| r.state().players[0].hand.contains(id)));
            pt(&r, id, 4);
            if matches!(r.state().waiting_for, WaitingFor::DiscardToHandSize { .. }) {
                break;
            }
            assert!(
                matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
                "unexpected cleanup window: {:?}",
                r.state().waiting_for
            );
            assert!(passes < 2, "cleanup did not request mandatory discard");
            r.act(GameAction::PassPriority)
                .expect("pass priority to execute cleanup's hand-size action");
        }
        match &r.state().waiting_for {
            WaitingFor::DiscardToHandSize {
                player,
                count,
                cards,
            } => {
                assert_eq!(*player, P0);
                assert_eq!(*count, 1);
                assert_eq!(cards.len(), hand.len());
                assert!(hand.iter().all(|id| cards.contains(id)));
                assert_eq!(r.state().players[0].hand.len(), hand.len());
                assert!(hand.iter().all(|id| r.state().players[0].hand.contains(id)));
            }
            other => panic!("expected mandatory cleanup discard, got {other:?}"),
        }
        assert_eq!(r.state().phase, Phase::Cleanup);
        assert_eq!(r.state().active_player, P0);
        assert_eq!(r.state().turn_number, turn);
        assert_eq!(r.state().objects[&time_stop].zone, Zone::Exile);
        assert!(r.state().stack.is_empty());
        assert!(r.state().combat.is_none());
        pt(&r, id, 4);
        r.act(GameAction::SelectCards {
            cards: vec![hand[0]],
        })
        .unwrap();
        for _ in 0..16 {
            if matches!(r.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            r.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(r.state().objects[&hand[0]].zone, Zone::Graveyard);
        complete(&r);
    }
}

// CR 508.5 + CR 310.9d: enumerate Player, Planeswalker and Battle. A battle
// reads its protector, whose Forest count deliberately differs from controller.
#[test]
fn gaeas_liege_all_defender_scopes_and_per_source_binding() {
    for kind in 0..3 {
        let mut s = GameScenario::new_n_player(3, 42);
        s.at_phase(Phase::PreCombatMain);
        forests(&mut s, P0, 4);
        forests(&mut s, P1, 1);
        forests(&mut s, P2, 3);
        let id = liege(&mut s);
        let second = liege(&mut s);
        let walker = s
            .add_planeswalker_from_oracle(P2, "Karn", "Karn", 10, "+1: You gain 1 life.")
            .id();
        let battle = s.add_creature(P1, "Siege fixture", 0, 0).id();
        let mut r = s.build();
        // Fixture only: different owner must not alter the static's "you".
        r.state_mut().objects.get_mut(&id).unwrap().owner = P1;
        let obj = r.state_mut().objects.get_mut(&battle).unwrap();
        obj.card_types.core_types = vec![CoreType::Battle];
        obj.card_types.subtypes = vec!["Siege".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.defense = Some(10);
        obj.base_defense = Some(10);
        obj.counters.insert(CounterType::Defense, 10);
        obj.chosen_attributes.push(ChosenAttribute::Player(P2));
        r.advance_to_combat();
        pt(&r, id, 4);
        let target = match kind {
            0 => AttackTarget::Player(P2),
            1 => AttackTarget::Planeswalker(walker),
            _ => AttackTarget::Battle(battle),
        };
        attack(&mut r, &[(id, target), (second, AttackTarget::Player(P1))]);
        pt(&r, id, 3);
        pt(&r, second, 1);
        assert_eq!(
            r.state()
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .find(|a| a.object_id == id)
                .unwrap()
                .defending_player,
            P2
        );
    }
}

// CR 508.5 + CR 704.5f: an empty defending census kills only after an attack.
#[test]
fn gaeas_liege_zero_defending_forests_dies_after_declaration() {
    let mut s = scenario();
    forests(&mut s, P0, 4);
    let id = liege(&mut s);
    let mut r = s.build();
    r.advance_to_combat();
    pt(&r, id, 4);
    let outcome = r
        .declare_attackers(&[(id, AttackTarget::Player(P1))])
        .unwrap();
    assert!(outcome.events.iter().any(|e| matches!(e, GameEvent::AttackersDeclared { attacks, .. } if attacks.contains(&(id, AttackTarget::Player(P1))))));
    assert_eq!(r.state().objects[&id].zone, Zone::Graveyard);
}

// CR 604.3a + CR 613.4b + CR 613.7a: both effects are layer-7b setters;
// the later source wins. Godhead affects the ordinary 3/3 sentinel in both.
#[test]
fn gaeas_liege_conditional_setter_uses_timestamp_not_cda_layer() {
    for liege_later in [true, false] {
        let mut s = scenario();
        forests(&mut s, P0, 4);
        let sentinel = s.add_creature(P0, "Ordinary sentinel", 3, 3).id();
        let id;
        let entrant;
        if liege_later {
            s.add_creature_from_oracle(P0, "Godhead of Awe", 4, 4, GODHEAD);
            id = liege_spell(&mut s);
            entrant = id;
            fund(&mut s, ManaType::Green, 6);
        } else {
            id = liege(&mut s);
            entrant = s
                .add_creature_to_hand_from_oracle(P0, "Godhead of Awe", 4, 4, GODHEAD)
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::WhiteBlue; 5],
                })
                .id();
            fund(&mut s, ManaType::White, 5);
        }
        let mut r = s.build();
        r.cast(entrant).resolve();
        complete(&r);
        pt(&r, id, if liege_later { 4 } else { 1 });
        pt(&r, sentinel, 1);
    }
}

// CR 604.3a + CR 611.3a: Angry Mob uses live turn gates, while the
// unconditional Maro CDA is still overridden by Godhead's layer-7b setter.
#[test]
fn gaeas_liege_conditional_sibling_and_unconditional_cda_preservation() {
    let mut s = scenario();
    for _ in 0..2 {
        s.add_basic_land(P1, ManaColor::Black);
    }
    let mob = s.add_creature_from_oracle(P0, "Angry Mob", 2, 2, MOB).id();
    let mut r = s.build();
    r.advance_to_combat();
    pt(&r, mob, 4);
    r.declare_attackers(&[]).unwrap();
    finish_combat(&mut r);
    for _ in 0..32 {
        if r.state().active_player == P1
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        r.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(r.state().active_player, P1);
    pt(&r, mob, 2);

    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Godhead of Awe", 4, 4, GODHEAD);
    s.with_cards_in_hand(P0, &["A", "B", "C"]);
    let maro = s
        .add_creature_to_hand_from_oracle(P0, "Maro", 0, 0, MARO)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green; 2],
        })
        .id();
    fund(&mut s, ManaType::Green, 4);
    let mut r = s.build();
    r.cast(maro).resolve();
    complete(&r);
    pt(&r, maro, 1);
}

// CR 305.7 + CR 611.2a + CR 400.7: the unchanged activation replaces a
// land's type/mana ability and expires only with its own source incarnation.
#[test]
fn gaeas_liege_land_conversion_tracks_live_count_and_independent_sources() {
    let mut s = scenario();
    forests(&mut s, P0, 4);
    let first = liege(&mut s);
    let second = liege(&mut s);
    let own = s.add_basic_land(P0, ManaColor::White);
    let enemy = s.add_basic_land(P1, ManaColor::Red);
    let bounce = s
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    fund(&mut s, ManaType::Blue, 1);
    let mut r = s.build();
    let original_abilities = r.state().objects[&own].abilities.clone();
    assert_eq!(original_abilities.len(), 1);
    r.activate(first, 0).target_object(own).resolve();
    complete(&r);
    assert_eq!(r.state().objects[&own].card_types.subtypes, vec!["Forest"]);
    assert_eq!(r.state().objects[&own].abilities.len(), 1);
    assert_ne!(r.state().objects[&own].abilities, original_abilities);
    pt(&r, first, 5);
    pt(&r, second, 5);
    r.activate(second, 0).target_object(enemy).resolve();
    complete(&r);
    assert_eq!(
        r.state().objects[&enemy].card_types.subtypes,
        vec!["Forest"]
    );
    let green_before = r.state().players[0].mana_pool.count_color(ManaType::Green);
    r.activate(own, 0).resolve();
    complete(&r);
    assert_eq!(
        r.state().players[0].mana_pool.count_color(ManaType::Green),
        green_before + 1
    );
    r.cast(bounce).target_object(first).resolve();
    complete(&r);
    assert_eq!(r.state().objects[&first].zone, Zone::Hand);
    assert_eq!(r.state().objects[&own].card_types.subtypes, vec!["Plains"]);
    assert_eq!(r.state().objects[&own].abilities, original_abilities);
    assert_eq!(
        r.state().objects[&enemy].card_types.subtypes,
        vec!["Forest"]
    );
    pt(&r, second, 4);
}

// CR 305.7: the target must be a land. Reject the creature through the public
// target action, then complete the same activation with its legal land target.
#[test]
fn gaeas_liege_land_activation_rejects_nonland_target() {
    let mut s = scenario();
    forests(&mut s, P0, 4);
    let id = liege(&mut s);
    let land = s.add_basic_land(P1, ManaColor::Red);
    let creature = s.add_creature(P1, "Ordinary target", 3, 3).id();
    let mut r = s.build();
    r.act(GameAction::ActivateAbility {
        source_id: id,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    assert!(r
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(creature))
        })
        .is_err());
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(land)),
    })
    .unwrap();
    for _ in 0..12 {
        if r.state().stack.is_empty()
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        r.act(GameAction::PassPriority).unwrap();
    }
    complete(&r);
    assert_eq!(r.state().objects[&land].card_types.subtypes, vec!["Forest"]);
    assert!(r.state().objects[&creature]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
}
