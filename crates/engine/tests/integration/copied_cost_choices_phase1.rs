//! CR 707.10: a spell copy keeps announced cost choices while remaining a copy.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::ability::{AbilityCost, KickerVariant};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;
use engine::types::StaticDefinition;

const RITE: &str = "Kicker {5} (You may pay an additional {5} as you cast this spell.)\nCreate a token that's a copy of target creature. If this spell was kicked, create five of those tokens instead.";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const VINELASHER: &str = "Offspring {2} (You may pay an additional {2} as you cast this spell. If you do, when this creature enters, create a 1/1 token copy of it.)\nLandfall — Whenever a land you control enters, this creature deals 1 damage to target opponent.";
const DOUBLE_MAJOR: &str = "Copy target creature spell you control, except it isn't legendary if the spell is legendary. (A copy of a creature spell becomes a token.)";
const SECURITRON: &str = "Squad {3} (As an additional cost to cast this spell, you may pay {3} any number of times. When this creature enters, create that many tokens that are copies of it.)\nVigilance\nWhenever a creature token you control enters, put a +1/+1 counter on it.";
const INGENIOUS_MASTERY: &str = "You may pay {2}{U} rather than pay this spell's mana cost.\nIf the {2}{U} cost was paid, you draw three cards, then an opponent creates two Treasure tokens and they scry 2. If that cost wasn't paid, you draw X cards.";
const JOIN_THE_MAESTROS: &str = "Casualty 2 (As you cast this spell, you may sacrifice a creature with power 2 or greater. When you do, copy this spell.)\nCreate a 4/3 black Ogre Warrior creature token.";
const DEGAVOLVER: &str = "Kicker {1}{B} and/or {R} (You may pay an additional {1}{B} and/or {R} as you cast this spell.)\nIf this creature was kicked with its {1}{B} kicker, it enters with two +1/+1 counters on it and with \"Pay 3 life: Regenerate this creature.\"\nIf this creature was kicked with its {R} kicker, it enters with a +1/+1 counter on it and with first strike.";

fn copied_rite_token_count(pay_kicker: bool) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let mut rite_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Rite of Replication", false, RITE);
    rite_builder.with_mana_cost(ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let rite = rite_builder.id();
    let mut copy_builder = scenario.add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST);
    copy_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let twincast = copy_builder.id();
    let mana = (0..7)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .chain((0..4).map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    let cast = runner.cast(rite).target_object(first);
    let mut original = if pay_kicker {
        cast.accept_optional().commit()
    } else {
        cast.decline_optional().commit()
    };
    let copy_result = original
        .cast(twincast)
        .target_object(rite)
        .commit()
        .resolve();
    let copy_id = copy_result
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::SpellCopied {
                original_id,
                object_id,
                ..
            } if *original_id == rite => Some(*object_id),
            _ => None,
        })
        .expect("Twincast must create an independent Rite copy");
    assert!(
        !copy_result.events().iter().any(
            |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == copy_id)
        ),
        "the copied spell was not cast"
    );
    assert!(matches!(
        copy_result.final_waiting_for(),
        WaitingFor::CopyRetarget { .. }
    ));
    original
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep the copy's announced target");
    let outcome = original.resolve();
    assert!(
        outcome.events().iter().any(
            |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == rite)
        ),
        "the original spell was actually cast"
    );
    outcome
        .state()
        .battlefield
        .iter()
        .filter_map(|id| outcome.state().objects.get(id))
        .filter(|o| o.is_token && o.name == "First Bear")
        .count()
}

#[test]
fn copied_kicked_rite_preserves_paid_rider() {
    assert_eq!(
        copied_rite_token_count(true),
        10,
        "the original and copy independently resolve the five-copy kicked rider"
    );
}

#[test]
fn copied_unkicked_rite_stays_unkicked() {
    assert_eq!(
        copied_rite_token_count(false),
        2,
        "neither spell takes the kicked rider after an unpaid choice"
    );
}

#[test]
fn copied_root_target_can_change_without_retargeting_original() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut bolt_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT);
    bolt_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Red],
    });
    let bolt = bolt_builder.id();
    let mut copy_builder = scenario.add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST);
    copy_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let twincast = copy_builder.id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let mut original = runner.cast(bolt).target_player(P1).commit();
    let copy_result = original
        .cast(twincast)
        .target_object(bolt)
        .commit()
        .resolve();
    let WaitingFor::CopyRetarget { target_slots, .. } = copy_result.final_waiting_for() else {
        panic!("copy should offer retargeting after SpellCopied");
    };
    assert_eq!(target_slots[0].current, Some(TargetRef::Player(P1)));
    assert!(target_slots[0]
        .legal_alternatives
        .contains(&TargetRef::Player(P0)));
    assert!(copy_result.events().iter().any(
        |event| matches!(event, GameEvent::SpellCopied { original_id, .. } if *original_id == bolt)
    ));
    // CR 707.10c: a stack spell is not a legal damage target. A rejected
    // replacement must leave the copy's retarget prompt and original target
    // intact before the legal choice below completes the copy.
    assert!(original
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(twincast)),
        })
        .is_err());
    let WaitingFor::CopyRetarget { target_slots, .. } = &original.state().waiting_for else {
        panic!("illegal replacement must keep the copy retarget prompt open");
    };
    assert_eq!(target_slots[0].current, Some(TargetRef::Player(P1)));
    assert!(!target_slots[0]
        .legal_alternatives
        .contains(&TargetRef::Object(twincast)));
    original
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("retarget the copy to P0");
    let outcome = original.resolve();
    outcome.assert_life_delta(P0, -3);
    outcome.assert_life_delta(P1, -3);
}

/// CR 702.33b/f + CR 707.10: the second of two distinct kicker costs remains
/// the second variant on the copy. Degavolver's two printed entry replacements
/// make wrong-variant retention visible as two counters rather than one.
#[test]
fn copied_second_kicker_keeps_its_variant_specific_entry_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let degavolver = scenario
        .add_creature_to_hand_from_oracle(P0, "Degavolver", 1, 1, DEGAVOLVER)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let double_major = scenario
        .add_spell_to_hand_from_oracle(P0, "Double Major", true, DOUBLE_MAJOR)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::White,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&degavolver].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: degavolver,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin the two-kicker cast");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("decline first kicker");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("pay second kicker");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().objects[&degavolver].kickers_paid,
        vec![KickerVariant::Second]
    );

    let outcome = runner
        .cast(double_major)
        .target_object(degavolver)
        .resolve();
    let copy_id = outcome
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::SpellCopied {
                original_id,
                object_id,
                ..
            } if *original_id == degavolver => Some(*object_id),
            _ => None,
        })
        .expect("Double Major must copy the actually cast Degavolver");
    for id in [degavolver, copy_id] {
        let object = &outcome.state().objects[&id];
        assert_eq!(object.zone, Zone::Battlefield, "both spells must resolve");
        assert_eq!(object.kickers_paid, vec![KickerVariant::Second]);
        assert_eq!(
            object.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(1),
            "only the second kicker replacement places one counter"
        );
        assert!(object.has_keyword(&Keyword::FirstStrike));
    }
    assert!(outcome.state().objects[&copy_id].is_token);
}

/// CR 702.175a: Offspring asks whether its cost was paid when the permanent
/// enters. Both the original creature spell and its independent copy retain
/// that answer, although only the original was cast.
fn copy_vinelashers(pay_offspring: bool) -> (usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut creature_builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Iridescent Vinelasher", 1, 2, VINELASHER);
    creature_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Black],
    });
    let creature = creature_builder.id();
    let mut copy_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Double Major", true, DOUBLE_MAJOR);
    copy_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
    });
    let double_major = copy_builder.id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let cast = runner.cast(creature);
    let mut original = if pay_offspring {
        cast.accept_optional().commit()
    } else {
        cast.decline_optional().commit()
    };
    let outcome = original
        .cast(double_major)
        .target_object(creature)
        .resolve();
    assert!(outcome.events().iter().any(
        |event| matches!(event, GameEvent::SpellCopied { original_id, .. } if *original_id == creature)
    ));
    let permanents: Vec<_> = outcome
        .state()
        .battlefield
        .iter()
        .filter_map(|id| outcome.state().objects.get(id))
        .filter(|object| object.name == "Iridescent Vinelasher")
        .collect();
    assert_eq!(
        permanents.iter().filter(|object| !object.is_token).count(),
        1
    );
    (
        permanents.len(),
        permanents.iter().filter(|object| object.is_token).count(),
    )
}

#[test]
fn copied_paid_offspring_permanent_triggers_on_entry() {
    assert_eq!(copy_vinelashers(true), (4, 3));
}

#[test]
fn copied_unpaid_offspring_permanent_has_no_offspring_trigger() {
    assert_eq!(copy_vinelashers(false), (2, 1));
}

/// CR 702.157a + CR 707.10: repeatable non-kicker payments are copied as a
/// count. Each of the two creature spells independently makes two Squad tokens.
fn copy_squad_with_payments(payments: u32) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut creature_builder = scenario.add_creature_to_hand(P0, "Securitron Squadron", 2, 2);
    creature_builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::White],
    });
    creature_builder.from_oracle_text_with_keywords(&["squad:{3}", "Vigilance"], SECURITRON);
    let creature = creature_builder.id();
    let mut copy_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Double Major", true, DOUBLE_MAJOR);
    copy_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
    });
    let double_major = copy_builder.id();
    let mana = [ManaType::White, ManaType::Green, ManaType::Blue]
        .into_iter()
        .chain(std::iter::repeat_n(ManaType::Colorless, 7))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&creature].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: creature,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin the Squad cast");
    for pay in std::iter::repeat_n(true, payments as usize).chain(std::iter::once(false)) {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ));
        runner
            .act(GameAction::DecideOptionalCost { pay })
            .expect("declare Squad payment count");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let outcome = runner.cast(double_major).target_object(creature).resolve();
    assert!(outcome.events().iter().any(
        |event| matches!(event, GameEvent::SpellCopied { original_id, .. } if *original_id == creature)
    ));
    outcome
        .state()
        .battlefield
        .iter()
        .filter_map(|id| outcome.state().objects.get(id))
        .filter(|object| object.name == "Securitron Squadron")
        .count()
}

#[test]
fn copied_squad_spell_keeps_two_payment_count() {
    assert_eq!(
        copy_squad_with_payments(2),
        6,
        "original and copy each get two Squad tokens"
    );
}

#[test]
fn copied_unpaid_squad_spell_creates_no_squad_tokens() {
    assert_eq!(copy_squad_with_payments(0), 2);
}

/// CR 707.10 + CR 118.9: the alternative-cost decision is copied. With X=0,
/// only that branch draws cards, so the copy must independently draw three.
fn copied_mastery_cards_drawn(pay_alternative: bool) -> i64 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E", "F", "G", "H"]);
    let mut mastery_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Ingenious Mastery", false, INGENIOUS_MASTERY);
    mastery_builder.with_mana_cost(ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::X, ManaCostShard::Blue],
    });
    let mastery = mastery_builder.id();
    let mut copy_builder = scenario.add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST);
    copy_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let twincast = copy_builder.id();
    let mana = std::iter::repeat_n(ManaType::Colorless, 2)
        .chain(std::iter::repeat_n(ManaType::Blue, 3))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    let cast = runner.cast(mastery).x(0);
    let mut original = if pay_alternative {
        cast.accept_optional().commit()
    } else {
        cast.decline_optional().commit()
    };
    let outcome = original.cast(twincast).target_object(mastery).resolve();
    assert!(outcome.events().iter().any(
        |event| matches!(event, GameEvent::SpellCopied { original_id, .. } if *original_id == mastery)
    ));
    outcome.hand_drawn(P0)
}

#[test]
fn copied_alternative_cost_spell_takes_paid_branch() {
    assert_eq!(copied_mastery_cards_drawn(true), 6);
}

#[test]
fn copied_normal_cost_spell_does_not_take_alternative_branch() {
    assert_eq!(copied_mastery_cards_drawn(false), 0);
}

/// CR 118.9 + CR 707.10: the actual cast uses a once-per-turn alternative
/// cost grant. Its copy keeps the alternative-cost decision and resolves, but
/// creates no second cast and cannot spend the same grant again.
#[test]
fn copied_granted_alternative_cost_spell_does_not_cast_or_spend_grant_again() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grant = scenario
        .add_creature(P0, "Alternative Cost Grant", 2, 2)
        .with_static_definition(StaticDefinition::new(StaticMode::CastWithAlternativeCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![],
                },
            },
            timing_permission: None,
            frequency: CastFrequency::OncePerTurn,
        }))
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Blue, ManaType::Blue]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    assert!(!runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&grant));
    let mut original = runner
        .cast(bolt)
        .target_player(P1)
        .accept_optional()
        .commit();
    assert!(
        original
            .state()
            .alt_cost_grant_permissions_used
            .contains(&grant),
        "the actual cast must consume its elected once-per-turn grant"
    );
    let copy_result = original
        .cast(twincast)
        .target_object(bolt)
        .commit()
        .resolve();
    let copy_id = copy_result
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::SpellCopied {
                original_id,
                object_id,
                ..
            } if *original_id == bolt => Some(*object_id),
            _ => None,
        })
        .expect("Twincast must copy the granted-cost Bolt");
    let copy_entry = original.state().stack.back().expect("copy on the stack");
    assert_eq!(copy_entry.id, copy_id);
    let StackEntryKind::Spell {
        ability: Some(copy_ability),
        ..
    } = &copy_entry.kind
    else {
        panic!("copied Bolt must remain a spell with its paid-cost decision");
    };
    assert!(copy_ability.context.alternative_mana_cost_paid);
    assert_eq!(copy_ability.context.alt_cost_grant_source, None);
    assert!(copy_result.events().iter().all(|event| {
        !matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == copy_id)
    }));
    assert_eq!(
        original.state().alt_cost_grant_permissions_used.len(),
        1,
        "copying must not record another grant use"
    );
    original
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep the copy's legal player target");
    let outcome = original.resolve();
    outcome.assert_life_delta(P1, -6);
    assert!(outcome.events().iter().any(
        |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == bolt)
    ));
    assert_eq!(outcome.state().alt_cost_grant_permissions_used.len(), 1);
    assert!(outcome
        .state()
        .alt_cost_grant_permissions_used
        .contains(&grant));
}

/// CR 702.153a + CR 707.10: Casualty is a cast trigger. Its copied cost
/// decision must not cause another Casualty trigger on SpellCopied.
fn casualty_copy_outcome(pay_casualty: bool) -> (usize, usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fodder = scenario.add_creature(P0, "Casualty Fodder", 2, 2).id();
    let mut spell_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Join the Maestros", false, JOIN_THE_MAESTROS);
    spell_builder.with_mana_cost(ManaCost::Cost {
        generic: 4,
        shards: vec![ManaCostShard::Black],
    });
    let spell = spell_builder.id();
    let mana = std::iter::repeat_n(ManaType::Colorless, 4)
        .chain(std::iter::once(ManaType::Black))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    let cast = runner.cast(spell).sacrifice_with(&[fodder]);
    let outcome = if pay_casualty {
        cast.accept_optional().resolve()
    } else {
        cast.decline_optional().resolve()
    };
    let spell_cast_count = outcome
        .events()
        .iter()
        .filter(
            |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell),
        )
        .count();
    let spell_copy_count = outcome.events().iter().filter(
        |event| matches!(event, GameEvent::SpellCopied { original_id, .. } if *original_id == spell)
    ).count();
    let tokens = outcome
        .state()
        .battlefield
        .iter()
        .filter_map(|id| outcome.state().objects.get(id))
        .filter(|object| object.is_token && object.name == "Ogre Warrior")
        .count();
    (tokens, spell_cast_count, spell_copy_count)
}

#[test]
fn paid_casualty_copy_has_no_second_cast_trigger() {
    assert_eq!(casualty_copy_outcome(true), (2, 1, 1));
}

#[test]
fn unpaid_casualty_spell_makes_no_copy() {
    assert_eq!(casualty_copy_outcome(false), (1, 1, 0));
}
