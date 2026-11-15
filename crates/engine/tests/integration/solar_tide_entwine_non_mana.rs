//! Production pipeline test for non-mana Entwine costs (CR 702.42a).
//!
//! Covers:
//! - Solar Tide ("Entwine—Sacrifice two lands.") paying exactly the required lands with both modes chosen;
//! - Solar Tide casting a single mode with no land payment;
//! - Solar Tide being refused with too few lands;
//! - Solar Tide finishing resolution after payment (destroying both <=2 and >=3 power creatures);
//! - Betrayal of Flesh ("Entwine—Sacrifice three lands.") target-selection continuation across modes and land payment;
//! - Entwine spell discovering other casting costs (e.g., granted Casualty 2).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SOLAR_TIDE: &str = "Choose one —\n• Destroy all creatures with power 2 or less.\n• Destroy all creatures with power 3 or greater.\nEntwine—Sacrifice two lands.";

const BETRAYAL_OF_FLESH: &str = "Choose one —\n• Destroy target creature.\n• Return target creature card from your graveyard to the battlefield.\nEntwine—Sacrifice three lands.";

const ANHELO_STYLE_CASUALTY_STATIC: &str =
    "The first instant or sorcery spell you cast each turn has casualty 2.";

const BOLAS_ORACLE: &str = "You may look at the top card of your library any time.\n\
You may play lands and cast spells from the top of your library. If you cast a spell this way, pay life equal to its mana value rather than pay its mana cost.\n\
{T}, Sacrifice ten nonland permanents: Each opponent loses 10 life.";

const ANHELO_ORACLE: &str = "Deathtouch\nThe first instant or sorcery spell you cast each turn has casualty 2. (As you cast that spell, you may sacrifice a creature with power 2 or greater. When you do, copy the spell and you may choose new targets for the copy.)";

const WORT_ORACLE: &str = "Each red or green instant or sorcery spell you cast has conspire.";

fn add_mana(runner: &mut GameRunner, mana: ManaType, count: usize) {
    let dummy = ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(mana, dummy, false, vec![]));
    }
}

/// Scenario 1: Paying exactly the required lands with both modes chosen finishes resolution
/// and destroys creatures matching both modes while putting sacrificed lands into the graveyard.
#[test]
fn solar_tide_entwine_pay_lands_both_modes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land1 = scenario.add_basic_land(P0, ManaColor::White);
    let land2 = scenario.add_basic_land(P0, ManaColor::White);
    let small_creature = scenario.add_creature(P0, "Small Creature", 1, 1).id();
    let big_creature = scenario.add_creature(P1, "Big Creature", 4, 4).id();
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Solar Tide", false, SOLAR_TIDE)
        .id();
    let mut runner = scenario.build();

    // Solar Tide costs {4}{W}{W}
    add_mana(&mut runner, ManaType::White, 2);
    add_mana(&mut runner, ManaType::Colorless, 4);

    runner
        .cast(spell_id)
        .modes(&[0, 1])
        .pay_cost_with(&[land1, land2])
        .resolve();

    runner.advance_until_stack_empty();

    let state = runner.state();
    // Sacrificed lands are in the graveyard
    assert_eq!(
        state.objects[&land1].zone,
        Zone::Graveyard,
        "land1 must be in graveyard"
    );
    assert_eq!(
        state.objects[&land2].zone,
        Zone::Graveyard,
        "land2 must be in graveyard"
    );

    // Both modes resolved: both small and big creatures are destroyed
    assert_eq!(
        state.objects[&small_creature].zone,
        Zone::Graveyard,
        "power <= 2 creature must be destroyed"
    );
    assert_eq!(
        state.objects[&big_creature].zone,
        Zone::Graveyard,
        "power >= 3 creature must be destroyed"
    );
    assert!(state.stack.is_empty(), "stack must be empty");
    assert!(state.pending_cast.is_none(), "pending cast must be cleared");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "waiting_for must be priority"
    );
}

/// Scenario 2: Casting a single mode requires no land payment and leaves lands on the battlefield.
#[test]
fn solar_tide_single_mode_no_land_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land1 = scenario.add_basic_land(P0, ManaColor::White);
    let land2 = scenario.add_basic_land(P0, ManaColor::White);
    let small_creature = scenario.add_creature(P0, "Small Creature", 1, 1).id();
    let big_creature = scenario.add_creature(P1, "Big Creature", 4, 4).id();
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Solar Tide", false, SOLAR_TIDE)
        .id();
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::White, 2);
    add_mana(&mut runner, ManaType::Colorless, 4);

    // Cast only mode 0 (destroy all creatures with power 2 or less)
    runner.cast(spell_id).modes(&[0]).resolve();

    runner.advance_until_stack_empty();

    let state = runner.state();
    // Lands were NOT sacrificed
    assert_eq!(
        state.objects[&land1].zone,
        Zone::Battlefield,
        "land1 must remain on battlefield"
    );
    assert_eq!(
        state.objects[&land2].zone,
        Zone::Battlefield,
        "land2 must remain on battlefield"
    );

    // Only mode 0 resolved
    assert_eq!(
        state.objects[&small_creature].zone,
        Zone::Graveyard,
        "power <= 2 creature must be destroyed"
    );
    assert_eq!(
        state.objects[&big_creature].zone,
        Zone::Battlefield,
        "power >= 3 creature must survive"
    );
    assert!(state.stack.is_empty(), "stack must be empty");
    assert!(state.pending_cast.is_none(), "pending cast must be cleared");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "waiting_for must be priority"
    );
}

/// Scenario 3: Being refused with too few lands when attempting to choose both modes.
#[test]
fn solar_tide_entwine_refused_with_too_few_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 controls only 1 land, but Solar Tide entwine requires 2 lands
    let land1 = scenario.add_basic_land(P0, ManaColor::White);
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Solar Tide", false, SOLAR_TIDE)
        .id();
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::White, 2);
    add_mana(&mut runner, ManaType::Colorless, 4);

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(engine::types::actions::GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast initiation succeeds");

    assert!(matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::ModeChoice { .. }
    ));

    // Choosing both modes requires sacrificing 2 lands (CR 702.42a).
    // Since P0 controls only 1 land, this cost cannot be paid (CR 601.2h).
    // The cast is illegal and reversed under CR 733.1.
    runner
        .act(engine::types::actions::GameAction::SelectModes {
            indices: vec![0, 1],
        })
        .expect("action handled by reversal settlement");

    let state = runner.state();
    // Spell is returned to hand (not on stack, not resolved)
    assert_eq!(
        state.objects[&spell_id].zone,
        Zone::Hand,
        "spell must be returned to hand after reversal"
    );
    assert!(state.stack.is_empty(), "stack must be empty after reversal");
    assert!(state.pending_cast.is_none(), "pending cast must be cleared");
    assert_eq!(
        state.objects[&land1].zone,
        Zone::Battlefield,
        "land must remain on battlefield"
    );
    assert!(matches!(
        state.waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ));
}

/// Scenario 4: Betrayal of Flesh's target-selection continuation across both modes and land payment.
#[test]
fn betrayal_of_flesh_entwine_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land1 = scenario.add_basic_land(P0, ManaColor::Black);
    let land2 = scenario.add_basic_land(P0, ManaColor::Black);
    let land3 = scenario.add_basic_land(P0, ManaColor::Black);

    // Mode 0 target: creature on battlefield to destroy
    let enemy_creature = scenario.add_creature(P1, "Enemy Creature", 2, 2).id();
    // Mode 1 target: creature card in P0's graveyard to reanimate
    let reanimate_target = scenario
        .add_creature_to_graveyard(P0, "Reanimate Target", 3, 3)
        .id();

    let betrayal_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Betrayal of Flesh", true, BETRAYAL_OF_FLESH)
        .id();
    let mut runner = scenario.build();

    // Betrayal of Flesh costs {5}{B}
    add_mana(&mut runner, ManaType::Black, 1);
    add_mana(&mut runner, ManaType::Colorless, 5);

    // Both modes chosen: target enemy creature for mode 0, graveyard creature for mode 1,
    // and pay 3 lands for entwine.
    runner
        .cast(betrayal_id)
        .modes(&[0, 1])
        .target_objects(&[enemy_creature, reanimate_target])
        .pay_cost_with(&[land1, land2, land3])
        .resolve();

    runner.advance_until_stack_empty();

    let state = runner.state();
    // All three lands sacrificed
    assert_eq!(
        state.objects[&land1].zone,
        Zone::Graveyard,
        "land1 must be sacrificed"
    );
    assert_eq!(
        state.objects[&land2].zone,
        Zone::Graveyard,
        "land2 must be sacrificed"
    );
    assert_eq!(
        state.objects[&land3].zone,
        Zone::Graveyard,
        "land3 must be sacrificed"
    );

    // Mode 0 destroyed enemy creature
    assert_eq!(
        state.objects[&enemy_creature].zone,
        Zone::Graveyard,
        "enemy creature must be destroyed"
    );

    // Mode 1 returned reanimate_target to battlefield
    assert_eq!(
        state.objects[&reanimate_target].zone,
        Zone::Battlefield,
        "graveyard creature must be on battlefield"
    );
    assert_eq!(
        state.objects[&reanimate_target].controller, P0,
        "reanimated creature must be controlled by P0"
    );
    assert!(state.stack.is_empty(), "stack must be empty");
    assert!(state.pending_cast.is_none(), "pending cast must be cleared");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "waiting_for must be priority"
    );
}

/// Scenario 5: Entwine cast discovers granted additional costs (Casualty 2), ensuring other costs are not skipped.
/// CR 702.153a + CR 702.42a: Casualty is offered before Entwine; paying Casualty sacrifices fodder and
/// paying Entwine sacrifices lands. The fodder's graveyard assertion is checked BEFORE resolution
/// to discriminate sacrifice payment from Solar Tide's board wipe.
#[test]
fn solar_tide_entwine_discovers_granted_casualty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Anhelo's verbatim text granting casualty 2 to the first instant or sorcery cast each turn
    scenario.add_creature_from_oracle(P0, "Anhelo Proxy", 1, 3, ANHELO_STYLE_CASUALTY_STATIC);
    // Casualty fodder: creature with power >= 2
    let casualty_fodder = scenario.add_creature(P0, "Casualty Fodder", 2, 2).id();
    let land1 = scenario.add_basic_land(P0, ManaColor::White);
    let land2 = scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Solar Tide", false, SOLAR_TIDE)
        .id();
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::White, 2);
    add_mana(&mut runner, ManaType::Colorless, 4);

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("initiate cast of Solar Tide");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
    runner
        .act(GameAction::SelectModes {
            indices: vec![0, 1],
        })
        .expect("select both modes for entwine");

    // CR 702.153a + CR 601.2b: Casualty is an optional additional cost in the queue; assert it is offered
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "casualty cost must be offered, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("elect to pay casualty");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "waiting for casualty sacrifice fodder, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![casualty_fodder],
        })
        .expect("sacrifice casualty fodder");

    // Deferred Entwine required additional cost is paid next
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "waiting for entwine lands, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![land1, land2],
        })
        .expect("sacrifice lands for entwine");

    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).expect("pay mana");
    }

    // DISCRIMINATE PAYMENT FROM RESOLUTION:
    // Assert BEFORE resolution that fodder was sacrificed for casualty (not destroyed by Solar Tide),
    // lands were sacrificed for entwine, and Solar Tide is on the stack.
    let state = runner.state();
    assert_eq!(
        state.objects[&casualty_fodder].zone,
        Zone::Graveyard,
        "casualty fodder must be in graveyard before resolution (paid as cost, not destroyed)"
    );
    assert_eq!(
        state.objects[&land1].zone,
        Zone::Graveyard,
        "land1 must be in graveyard before resolution"
    );
    assert_eq!(
        state.objects[&land2].zone,
        Zone::Graveyard,
        "land2 must be in graveyard before resolution"
    );
    assert_eq!(
        state.objects[&spell_id].zone,
        Zone::Stack,
        "Solar Tide must be on the stack"
    );

    // Verify copy trigger and resolution
    let mut spell_copied = false;
    for _ in 0..20 {
        drain_order_triggers_with_identity(runner.state_mut());
        if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
            runner
                .act(GameAction::KeepAllCopyTargets)
                .expect("keep targets");
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        let events = runner.act(GameAction::PassPriority).expect("pass");
        if events.events.iter().any(
            |e| matches!(e, GameEvent::SpellCopied { original_id, .. } if original_id == &spell_id),
        ) {
            spell_copied = true;
        }
    }
    assert!(
        spell_copied,
        "casualty trigger must copy Solar Tide (SpellCopied event)"
    );

    let state = runner.state();
    assert!(state.stack.is_empty(), "stack must be empty");
    assert!(state.pending_cast.is_none(), "pending cast must be cleared");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "waiting_for must be priority"
    );
}

/// Scenario 6: Casting entwined Solar Tide from the top of the library with Bolas's Citadel
/// pays 6 life (its mana value, CR 118.9 + CR 118.9d) and sacrifices 2 lands for Entwine.
#[test]
fn solar_tide_entwine_with_bolas_citadel_pays_life_and_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _citadel = scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_ORACLE)
        .id();
    let land1 = scenario.add_basic_land(P0, ManaColor::White);
    let land2 = scenario.add_basic_land(P0, ManaColor::White);
    let small_creature = scenario.add_creature(P0, "Small Creature", 1, 1).id();
    let big_creature = scenario.add_creature(P1, "Big Creature", 4, 4).id();

    let spell_id = scenario
        .add_spell_to_library_top(P0, "Solar Tide", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 4,
        })
        .from_oracle_text(SOLAR_TIDE)
        .id();
    scenario.with_life(P0, 20);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("initiate cast from top of library via Citadel");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
    runner
        .act(GameAction::SelectModes {
            indices: vec![0, 1],
        })
        .expect("select both modes for entwine");

    // Citadel alternative cost (pay 6 life) is paid first, followed by Entwine sacrifice
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "waiting for entwine lands sacrifice, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![land1, land2],
        })
        .expect("sacrifice two lands for entwine");

    // CR 118.9d + CR 601.2h: Assert BEFORE resolution that 6 life was paid AND both lands were sacrificed
    let state = runner.state();
    assert_eq!(
        state.players[0].life, 14,
        "Casting Solar Tide (MV 6) via Citadel must pay 6 life (20 - 6 = 14)"
    );
    assert_eq!(
        state.objects[&land1].zone,
        Zone::Graveyard,
        "land1 must be in graveyard before resolution"
    );
    assert_eq!(
        state.objects[&land2].zone,
        Zone::Graveyard,
        "land2 must be in graveyard before resolution"
    );
    assert_eq!(
        state.objects[&spell_id].zone,
        Zone::Stack,
        "Solar Tide must be on the stack"
    );

    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.objects[&small_creature].zone,
        Zone::Graveyard,
        "mode 0 must destroy small creature"
    );
    assert_eq!(
        state.objects[&big_creature].zone,
        Zone::Graveyard,
        "mode 1 must destroy big creature"
    );
    assert!(state.stack.is_empty(), "stack must be empty");
    assert!(state.pending_cast.is_none(), "pending cast must be cleared");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "waiting_for must be priority"
    );
}

/// Scenario 7: Granted Conspire (Wort / Rassilon) with entwined Solar Tide:
/// Conspire optional cost is offered, player pays Conspire by tapping 2 creatures,
/// player sacrifices 2 lands for Entwine, and Solar Tide is copied upon cast.
#[test]
fn solar_tide_entwine_with_granted_conspire_taps_and_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Wort grants Conspire to red or green instant/sorcery spells
    scenario.add_creature_from_oracle(P0, "Wort, the Raidmother", 3, 3, WORT_ORACLE);
    let tap1 = scenario
        .add_creature(P0, "Red Goblin 1", 1, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let tap2 = scenario
        .add_creature(P0, "Red Goblin 2", 1, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let land1 = scenario.add_basic_land(P0, ManaColor::White);
    let land2 = scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Solar Tide", false, SOLAR_TIDE)
        .with_color(vec![ManaColor::Red]) // Red so it shares color with goblins and matches Wort's static
        .id();
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::White, 2);
    add_mana(&mut runner, ManaType::Colorless, 4);

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("initiate cast");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
    runner
        .act(GameAction::SelectModes {
            indices: vec![0, 1],
        })
        .expect("select both modes");

    // Conspire is an optional additional cost in the queue; assert it is offered
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "conspire cost must be offered, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("elect to pay conspire");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            }
        ),
        "waiting for creatures to tap for conspire, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![tap1, tap2],
        })
        .expect("tap creatures for conspire");

    // Next, the deferred Entwine required cost is paid
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "waiting for entwine lands, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![land1, land2],
        })
        .expect("sacrifice lands for entwine");

    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).expect("pay mana");
    }

    // Assert BEFORE resolution: creatures tapped, lands sacrificed, spell on stack
    let state = runner.state();
    assert!(
        state.objects[&tap1].tapped,
        "conspire creature 1 must be tapped"
    );
    assert!(
        state.objects[&tap2].tapped,
        "conspire creature 2 must be tapped"
    );
    assert_eq!(
        state.objects[&land1].zone,
        Zone::Graveyard,
        "land1 must be sacrificed"
    );
    assert_eq!(
        state.objects[&land2].zone,
        Zone::Graveyard,
        "land2 must be sacrificed"
    );
    assert_eq!(
        state.objects[&spell_id].zone,
        Zone::Stack,
        "Solar Tide must be on stack"
    );

    // Verify copy trigger and resolution
    let mut spell_copied = false;
    for _ in 0..20 {
        drain_order_triggers_with_identity(runner.state_mut());
        if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
            runner
                .act(GameAction::KeepAllCopyTargets)
                .expect("keep targets");
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        let events = runner.act(GameAction::PassPriority).expect("pass");
        if events.events.iter().any(
            |e| matches!(e, GameEvent::SpellCopied { original_id, .. } if original_id == &spell_id),
        ) {
            spell_copied = true;
        }
    }
    assert!(
        spell_copied,
        "conspire must copy Solar Tide (SpellCopied event)"
    );

    let state = runner.state();
    assert!(state.stack.is_empty(), "stack must be empty");
    assert!(state.pending_cast.is_none(), "pending cast must be cleared");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "waiting_for must be priority"
    );
}

const TERROR_OF_THE_PEAKS: &str = "Flying\nSpells your opponents cast that target this creature cost an additional 3 life to cast.";

/// CR 118.9d: Bolas's Citadel's life payment is the alternative cost, and Terror of
/// the Peaks' imposed 3 life applies to it even though the bolt has no other
/// additional cost. Life is asserted BEFORE resolution (bolt's damage is not to the caster).
#[test]
fn citadel_alternative_cost_keeps_imposed_terror_tax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_ORACLE);
    let terror = scenario
        .add_creature_from_oracle(P1, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    scenario.with_life(P0, 20);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("initiate cast from library top");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![engine::types::ability::TargetRef::Object(terror)],
            })
            .expect("target Terror of the Peaks");
    }

    let state = runner.state();
    assert_eq!(
        state.objects[&bolt].zone,
        Zone::Stack,
        "bolt must be on the stack, found {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.players[0].life, 16,
        "Citadel pays MV 1 plus Terror's 3 imposed life (20 - 4 = 16)"
    );
}

/// CR 702.78a + CR 702.153a: when casualty (Anhelo) was announced before targets,
/// granted Conspire (Wort) is still offered afterwards and its payment taps the creatures.
#[test]
fn targeted_spell_offers_conspire_after_casualty_queue() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Wort, the Raidmother", 3, 3, WORT_ORACLE)
        .id();
    scenario
        .add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO_ORACLE)
        .id();
    scenario.add_creature(P0, "Casualty Fodder", 2, 2);
    let tap1 = scenario
        .add_creature(P0, "Red Goblin 1", 1, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let tap2 = scenario
        .add_creature(P0, "Red Goblin 2", 1, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 1);

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("initiate cast");

    let mut declined_casualty = false;
    let mut accepted_conspire = false;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } if !declined_casualty => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: false })
                    .expect("decline casualty");
                declined_casualty = true;
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("accept conspire");
                accepted_conspire = true;
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![tap1, tap2],
                    })
                    .expect("tap creatures for conspire");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Object(victim)],
                    })
                    .expect("target victim");
            }
            _ => break,
        }
    }

    assert!(declined_casualty, "casualty must be offered first");
    assert!(
        accepted_conspire,
        "conspire must still be offered, found {:?}",
        runner.state().waiting_for
    );
    let state = runner.state();
    assert!(
        state.objects[&tap1].tapped && state.objects[&tap2].tapped,
        "conspire creatures must be tapped before resolution"
    );
    assert_eq!(state.objects[&bolt].zone, Zone::Stack);
}

const CAPSIZE: &str = "Buyback {3} (You may pay an additional {3} as you cast this spell. If the buyback cost was paid, put this spell into its owner's hand instead of into that player's graveyard as it resolves.)\nReturn target permanent to its owner's hand.";

/// Cast Capsize from the top of the library with Bolas's Citadel, targeting an
/// opponent's Terror of the Peaks, answering the Buyback offer with `pay_buyback`.
/// Returns the runner with Capsize on the stack and whether Buyback was offered.
fn cast_capsize_with_citadel_under_terror(pay_buyback: bool) -> (GameRunner, ObjectId, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_ORACLE);
    let terror = scenario
        .add_creature_from_oracle(P1, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let capsize = scenario
        .add_spell_to_library_top(P0, "Capsize", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 1,
        })
        .from_oracle_text(CAPSIZE)
        .id();
    scenario.with_life(P0, 20);
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 3);

    let card_id = runner.state().objects[&capsize].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: capsize,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("initiate cast from library top");

    let mut buyback_offered = false;
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Object(terror)],
                    })
                    .expect("target Terror of the Peaks");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                assert!(!buyback_offered, "Buyback must be offered exactly once");
                buyback_offered = true;
                runner
                    .act(GameAction::DecideOptionalCost { pay: pay_buyback })
                    .expect("decide Buyback");
            }
            _ => break,
        }
    }
    (runner, capsize, buyback_offered)
}

/// CR 702.27a + CR 118.9d: Buyback stays offered when Bolas's Citadel's life
/// payment is the alternative cost, and accepting it adds {3} while Citadel's
/// MV life and Terror of the Peaks' imposed 3 life are both still paid.
#[test]
fn citadel_capsize_accepting_buyback_pays_mana_and_terror_tax() {
    let (runner, capsize, buyback_offered) = cast_capsize_with_citadel_under_terror(true);
    let state = runner.state();
    assert!(buyback_offered, "Buyback must be offered under Citadel");
    assert_eq!(
        state.objects[&capsize].zone,
        Zone::Stack,
        "Capsize must be on the stack, found {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.players[0].life, 14,
        "Citadel pays MV 3 plus Terror's 3 imposed life (20 - 6 = 14)"
    );
    assert_eq!(
        state.players[0].mana_pool.total(),
        0,
        "accepted Buyback {{3}} must be paid from the pool"
    );
    let engine::types::game_state::StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &state.stack[0].kind
    else {
        panic!("expected Capsize on the stack");
    };
    assert!(
        ability.context.additional_cost_paid,
        "Buyback payment must be recorded"
    );
}

/// CR 702.27a + CR 118.9d: declining Buyback under Citadel leaves the pool
/// untouched but still pays Citadel's MV life plus Terror's imposed 3 life.
#[test]
fn citadel_capsize_declining_buyback_still_pays_terror_tax() {
    let (runner, capsize, buyback_offered) = cast_capsize_with_citadel_under_terror(false);
    let state = runner.state();
    assert!(buyback_offered, "Buyback must be offered under Citadel");
    assert_eq!(
        state.objects[&capsize].zone,
        Zone::Stack,
        "Capsize must be on the stack, found {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.players[0].life, 14,
        "Citadel pays MV 3 plus Terror's 3 imposed life (20 - 6 = 14)"
    );
    assert_eq!(
        state.players[0].mana_pool.total(),
        3,
        "declined Buyback must not spend mana"
    );
    let engine::types::game_state::StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &state.stack[0].kind
    else {
        panic!("expected Capsize on the stack");
    };
    assert!(
        !ability.context.additional_cost_paid,
        "declined Buyback must not be recorded as paid"
    );
}
