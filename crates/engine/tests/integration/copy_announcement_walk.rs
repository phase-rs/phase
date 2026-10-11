//! CR 601.2c + CR 707.12 + CR 115.6: announcing the targets of a freshly cast
//! copy (`WaitingFor::CopyRetarget` in Announce mode). The walk is the
//! production casting walk: an optional slot ("up to N") may be declined, a
//! slot "of an opponent's choice" is announced by its chooser while the copy
//! stays its caster's, and a copy whose spell has no ability announces nothing
//! and is still cast.
//!
//! Oracle text is verbatim from Scryfall (Mizzix's Mastery, Frost Breath,
//! Volcanic Offering, Baron Helmut Zemo).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectKind, QuantityExpr, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, CopyChoiceMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DEATH_FRENZY: &str = "All creatures get -2/-2 until end of turn. Whenever a creature dies this turn, you gain 1 life.";
const KAMBAL: &str =
    "Whenever an opponent casts a noncreature spell, that player loses 2 life and you gain 2 life.";
const LAVA_SPIKE: &str = "Lava Spike deals 3 damage to target player or planeswalker.";
const CHAINS_OF_CUSTODY: &str = "Enchant creature you control\nWhen this Aura enters, exile target nonland permanent an opponent controls until this Aura leaves the battlefield.\nEnchanted creature has ward {2}.";

/// CR 603.2 + CR 603.2c + CR 603.7b + CR 610.3: one action's state-based
/// death and exile return reach a persistent delayed observer exactly once.
/// Chains of Custody (on Magnivore) exiled P1's Bear; Death Frenzy set up
/// "whenever a creature dies this turn" and left Magnivore 1/1 (three
/// sorceries in graveyards, -2/-2). Mizzix's Mastery exiles Lava Spike and the
/// copy is announced at P1: Magnivore is 0/0 and dies, Chains of Custody goes
/// to the graveyard, and the Bear returns, all in the action that finishes the
/// resolution. P1's Kambal observes the copy's cast, so the batch is P0's
/// Frenzy trigger and P1's Kambal trigger: put on the stack in APNAP order
/// with no ordering choice, which lets the same pass reach its final delayed
/// scan. The Frenzy triggers once, for Magnivore.
#[test]
fn a_persistent_delayed_observer_triggers_once_across_sbas_and_exile_returns() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let magnivore = s
        .add_creature_from_oracle(P0, "Magnivore", 0, 0, MAGNIVORE)
        .id();
    let bear = s.add_creature(P1, "Bear", 2, 2).id();
    let kambal = s
        .add_creature_from_oracle(P1, "Kambal, Consul of Allocation", 2, 3, KAMBAL)
        .id();
    let mut chains = s.add_creature_to_hand(P0, "Chains of Custody", 0, 0);
    chains
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Enchant creature you control"], CHAINS_OF_CUSTODY);
    let chains = chains.id();
    let frenzy = s
        .add_spell_to_hand_from_oracle(P0, "Death Frenzy", false, DEATH_FRENZY)
        .with_mana_cost(ManaCost::zero())
        .id();
    s.add_spell_to_graveyard(P1, "Divination", false);
    let spike = s
        .add_spell_to_graveyard(P0, "Lava Spike", false)
        .from_oracle_text(LAVA_SPIKE)
        .id();
    let mastery = s
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(chains).target_object(magnivore).commit();
    resolve_entry(&mut r, chains);
    if matches!(
        r.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("exile the Bear");
    }
    r.advance_until_stack_empty();
    assert_eq!(
        r.state().objects[&bear].zone,
        Zone::Exile,
        "reach: Bear exiled"
    );
    r.cast(frenzy).commit();
    resolve_entry(&mut r, frenzy);
    assert_eq!(
        r.state().objects[&magnivore].zone,
        Zone::Battlefield,
        "reach: Magnivore survives Death Frenzy"
    );
    r.cast(mastery).target_object(spike).commit();
    pass_to_choice(&mut r);
    r.act(GameAction::SelectCards { cards: vec![spike] })
        .expect("cast the Lava Spike copy");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::CopyRetarget { .. }),
        "reach: the copy announcement, got {:?}",
        r.state().waiting_for
    );
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Player(P1)),
    })
    .expect("announce P1");
    assert_ne!(
        r.state().objects[&magnivore].zone,
        Zone::Battlefield,
        "reach: Magnivore died to the state-based check"
    );
    assert_ne!(
        r.state().objects[&chains].zone,
        Zone::Battlefield,
        "reach: Chains of Custody left with its host"
    );
    assert!(
        r.state()
            .battlefield
            .iter()
            .any(|id| r.state().objects[id].name == "Bear"),
        "reach: the Bear returned from exile"
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the batch needs no ordering choice, got {:?}",
        r.state().waiting_for
    );
    let frenzy_triggers = r
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == frenzy)
        .count();
    assert_eq!(frenzy_triggers, 1, "Death Frenzy triggers once");
    let kambal_triggers = r
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == kambal)
        .count();
    assert_eq!(kambal_triggers, 1, "Kambal observed the copy's cast");
}
const MIZZIXS_MASTERY: &str = "Exile target card that's an instant or sorcery from your graveyard. For each card exiled this way, copy it, and you may cast the copy without paying its mana cost. Exile Mizzix's Mastery.\nOverload {5}{R}{R}{R} (You may cast this spell for its overload cost. If you do, change \"target\" in its text to \"each.\")";
const FROST_BREATH: &str = "Tap up to two target creatures. Those creatures don't untap during their controller's next untap step.";
const VOLCANIC_OFFERING: &str = "Destroy target nonbasic land you don't control and target nonbasic land of an opponent's choice you don't control.\nVolcanic Offering deals 7 damage to target creature you don't control and 7 damage to target creature of an opponent's choice you don't control.";
const BARON_HELMUT_ZEMO: &str = "Whenever you cast a black spell from your hand, Baron Helmut Zemo connives.\nBoast \u{2014} Exile any number of black cards from your graveyard with fifteen or more black mana symbols among their mana costs: Copy those exiled cards. You may cast up to three of the copies without paying their mana costs. (Activate only if this creature attacked this turn and only once each turn.)";

/// Pass priority until a non-priority prompt opens.
fn pass_to_choice(r: &mut GameRunner) {
    for _ in 0..32 {
        if !matches!(r.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        assert!(!r.state().stack.is_empty(), "reach: the stack drained");
        r.act(GameAction::PassPriority).expect("pass");
    }
    panic!("no choice opened");
}

/// Cast Mizzix's Mastery at `source` (an instant/sorcery in P0's graveyard),
/// choose to cast its copy, and return the copy's stack id at its first
/// announcement prompt.
fn mastery_copy(mut scenario: GameScenario, source: ObjectId) -> (GameRunner, ObjectId, ObjectId) {
    let mastery = scenario
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = scenario.build();
    r.cast(mastery).target_object(source).commit();
    pass_to_choice(&mut r);
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "reach: Mizzix offers the copy, got {:?}",
        r.state().waiting_for
    );
    r.act(GameAction::SelectCards {
        cards: vec![source],
    })
    .expect("cast the copy");
    let WaitingFor::CopyRetarget { copy_id, mode, .. } = r.state().waiting_for else {
        panic!(
            "expected the copy's announcement, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(
        mode,
        Some(CopyChoiceMode::Announce),
        "reach: an announcement"
    );
    (r, mastery, copy_id)
}

fn begin_cast(r: &mut GameRunner, spell: ObjectId) {
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id: r.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast begins");
}

fn declared(r: &GameRunner, entry: ObjectId) -> Vec<TargetRef> {
    let ability = r
        .state()
        .stack
        .iter()
        .find(|e| e.id == entry)
        .and_then(|e| e.ability())
        .expect("the announced spell is on the stack");
    engine::game::ability_utils::declared_targets_in_chain(ability)
}

fn resolve_entry(r: &mut GameRunner, entry: ObjectId) {
    for _ in 0..32 {
        if !r.state().stack.iter().any(|e| e.id == entry) {
            return;
        }
        r.act(GameAction::PassPriority).expect("resolve");
    }
    panic!("the spell did not resolve");
}

/// The engine-derived permissions of the current announcement slot:
/// `(can_decline, can_keep, can_keep_rest)`.
fn permissions(r: &GameRunner) -> (bool, bool, bool) {
    match &r.state().waiting_for {
        WaitingFor::CopyRetarget {
            target_slots,
            current_slot,
            can_keep_rest,
            ..
        } => (
            target_slots[*current_slot].can_decline,
            target_slots[*current_slot].can_keep,
            *can_keep_rest,
        ),
        other => panic!("expected the copy announcement, got {other:?}"),
    }
}

/// Frost Breath with `first` creatures chosen (0 or 1), then a decline.
/// `copy`: through Mizzix's Mastery, else the ordinary cast (control).
fn frost_breath(copy: bool, first: bool) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P1, "Creature A", 2, 9).id();
    let b = s.add_creature(P1, "Creature B", 2, 9).id();
    let (mut r, mastery, entry) = if copy {
        let frost = s
            .add_spell_to_graveyard(P0, "Frost Breath", true)
            .from_oracle_text(FROST_BREATH)
            .id();
        let (r, mastery, copy_id) = mastery_copy(s, frost);
        (r, Some(mastery), copy_id)
    } else {
        let frost = s
            .add_spell_to_hand_from_oracle(P0, "Frost Breath", true, FROST_BREATH)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut r = s.build();
        begin_cast(&mut r, frost);
        (r, None, frost)
    };
    if copy {
        assert_eq!(
            permissions(&r),
            (true, false, false),
            "the optional slot may be declined; there is nothing to keep"
        );
    }
    if first {
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a)),
        })
        .expect("choose A");
        if copy {
            assert!(
                permissions(&r).0,
                "the second optional slot may be declined"
            );
        }
    }
    r.act(GameAction::ChooseTarget { target: None })
        .expect("CR 115.6: declining the optional slot is accepted");
    assert!(
        !matches!(
            r.state().waiting_for,
            WaitingFor::CopyRetarget { .. } | WaitingFor::TargetSelection { .. }
        ),
        "declining ends the \"up to two\" announcement"
    );
    let announced = declared(&r, entry);
    let expected: Vec<TargetRef> = if first {
        vec![TargetRef::Object(a)]
    } else {
        Vec::new()
    };
    assert_eq!(announced, expected);
    resolve_entry(&mut r, entry);
    assert_eq!(r.state().objects[&a].tapped, first);
    assert!(!r.state().objects[&b].tapped);
    if let Some(mastery) = mastery {
        resolve_entry(&mut r, mastery);
        assert_eq!(r.state().objects[&mastery].zone, Zone::Exile);
    }
}

#[test]
fn copy_announcement_declines_an_optional_slot_with_no_target() {
    frost_breath(true, false);
}

#[test]
fn copy_announcement_declines_after_one_target() {
    frost_breath(true, true);
}

#[test]
fn ordinary_frost_breath_decline_controls() {
    frost_breath(false, false);
    frost_breath(false, true);
}

/// Volcanic Offering: slots 1 and 3 are "of an opponent's choice". Asserts the
/// player each announcement prompt asks, and the copy's resolved board.
fn volcanic_offering(copy: bool) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let l1 = s.add_land_from_oracle(P1, "Opponent Nonbasic A", "").id();
    let l2 = s.add_land_from_oracle(P1, "Opponent Nonbasic B", "").id();
    let c1 = s.add_creature(P1, "Opponent Creature A", 3, 12).id();
    let c2 = s.add_creature(P1, "Opponent Creature B", 3, 12).id();
    let (mut r, mastery, entry) = if copy {
        let offering = s
            .add_spell_to_graveyard(P0, "Volcanic Offering", true)
            .from_oracle_text(VOLCANIC_OFFERING)
            .id();
        let (r, mastery, copy_id) = mastery_copy(s, offering);
        (r, Some(mastery), copy_id)
    } else {
        let offering = s
            .add_spell_to_hand_from_oracle(P0, "Volcanic Offering", true, VOLCANIC_OFFERING)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut r = s.build();
        begin_cast(&mut r, offering);
        (r, None, offering)
    };
    let mut askers: Vec<PlayerId> = Vec::new();
    for target in [l1, l2, c1, c2] {
        let asker = match &r.state().waiting_for {
            WaitingFor::CopyRetarget {
                player, controller, ..
            } => {
                assert_eq!(
                    controller.unwrap_or(*player),
                    P0,
                    "the copy stays its caster's"
                );
                *player
            }
            WaitingFor::TargetSelection { player, .. } => *player,
            other => panic!("expected an announcement prompt, got {other:?}"),
        };
        askers.push(asker);
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("the asked player announces");
    }
    assert_eq!(
        askers,
        vec![P0, P1, P0, P1],
        "CR 601.2c: each slot is announced by its chooser"
    );
    let controller = r
        .state()
        .stack
        .iter()
        .find(|e| e.id == entry)
        .map(|e| e.controller)
        .expect("the announced spell is on the stack");
    assert_eq!(controller, P0, "CR 112.2: the caster controls the spell");
    if copy {
        assert!(
            matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
            "priority returns to the copy's controller, got {:?}",
            r.state().waiting_for
        );
    }
    resolve_entry(&mut r, entry);
    assert_eq!(
        [c1, c2].map(|c| r.state().objects[&c].damage_marked),
        [7, 7]
    );
    assert!([l1, l2]
        .iter()
        .all(|l| r.state().objects[l].zone == Zone::Graveyard));
    if let Some(mastery) = mastery {
        resolve_entry(&mut r, mastery);
        assert_eq!(r.state().objects[&mastery].zone, Zone::Exile);
        assert_eq!(r.state().objects[&mastery].controller, P0);
    }
}

#[test]
fn copy_announcement_routes_each_slot_to_its_chooser() {
    volcanic_offering(true);
}

#[test]
fn ordinary_volcanic_offering_chooser_control() {
    volcanic_offering(false);
}

/// Baron Helmut Zemo's Boast copies two exiled cards: `first` is a vanilla
/// creature card (no spell ability) or, as the control, an instant. Both
/// copies are cast, and the batch completes.
fn zemo_batch(vanilla_first: bool) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PostCombatMain);
    s.with_library_top(P0, &["Draw A", "Draw B", "Draw C", "Draw D"]);
    let zemo = s
        .add_creature_from_oracle(P0, "Baron Helmut Zemo", 3, 3, BARON_HELMUT_ZEMO)
        .id();
    // Fixture: fifteen black symbols per card meets the Boast cost.
    let black = ManaCost::Cost {
        shards: vec![ManaCostShard::Black; 15],
        generic: 0,
    };
    let draw = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let first = if vanilla_first {
        s.add_creature_to_graveyard(P0, "Walking Corpse", 2, 2)
            .from_oracle_text("")
            .with_mana_cost(black.clone())
            .id()
    } else {
        s.add_spell_to_graveyard(P0, "Draw Instant A", true)
            .with_ability_definition(draw.clone())
            .with_mana_cost(black.clone())
            .id()
    };
    let second = s
        .add_spell_to_graveyard(P0, "Draw Instant B", true)
        .with_ability_definition(draw)
        .with_mana_cost(black)
        .id();
    let mut r = s.build();
    if vanilla_first {
        assert!(
            r.state().objects[&first].abilities.is_empty(),
            "reach: the vanilla creature has no spell ability"
        );
    }
    r.state_mut().creatures_attacked_this_turn.insert(zemo);
    let index = r.state().objects[&zemo]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::CastCopyOfCard { .. }))
        .expect("reach: Zemo's Boast");
    r.act(GameAction::ActivateAbility {
        source_id: zemo,
        ability_index: index,
    })
    .expect("activate Boast");
    r.act(GameAction::SelectCards {
        cards: vec![first, second],
    })
    .expect("pay the exile cost");
    pass_to_choice(&mut r);
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "reach: Zemo offers the copies"
    );
    let result = r
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("CR 707.12: both copies are cast");
    let copies = r.state().objects.values().filter(|o| o.is_copy).count();
    assert_eq!(copies, 2, "both selected copies are cast");
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::CastCopyOfCard,
                ..
            }
        )),
        "the copy batch completes"
    );
    r.advance_until_stack_empty();
    assert!(
        r.state().stack.is_empty(),
        "the batch and the copies finish"
    );
}

#[test]
fn copy_with_no_spell_ability_is_cast_and_the_batch_continues() {
    zemo_batch(true);
}

#[test]
fn copy_batch_of_instants_control() {
    zemo_batch(false);
}

/// CR 601.2c + CR 115.1 + CR 707.12 (three players): a copy announcement runs
/// the announcing-opponent election of the casting authority. Volcanic
/// Offering's rulings: "you choose the opponents for Volcanic Offering as you
/// cast the spell", the same or different opponents for each effect. The
/// caster elects P2 for the land group and P1 for the creature group; the
/// slots are then announced P0/P2/P0/P1, and the copy stays P0's. `copy`:
/// through Mizzix's Mastery, else the ordinary cast (control).
fn restore(state: &engine::types::game_state::GameState) -> GameRunner {
    use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization};
    let wire = serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap();
    let restored = serde_json::from_value::<PersistedGameState>(wire)
        .expect("decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("publishable");
    GameRunner::from_state(restored)
}

fn volcanic_offering_three_players(copy: bool) {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 7);
    s.at_phase(Phase::PreCombatMain);
    let land_p1 = s.add_land_from_oracle(P1, "P1 Nonbasic", "").id();
    let land_p2 = s.add_land_from_oracle(p2, "P2 Nonbasic", "").id();
    let creature_p1 = s.add_creature(P1, "P1 Creature", 3, 12).id();
    let creature_p2 = s.add_creature(p2, "P2 Creature", 3, 12).id();
    let (mut r, mastery, entry) = if copy {
        let offering = s
            .add_spell_to_graveyard(P0, "Volcanic Offering", true)
            .from_oracle_text(VOLCANIC_OFFERING)
            .id();
        let mastery = s
            .add_spell_to_hand(P0, "Mizzix's Mastery", false)
            .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut r = s.build();
        r.cast(mastery).target_object(offering).commit();
        pass_to_choice(&mut r);
        r.act(GameAction::SelectCards {
            cards: vec![offering],
        })
        .expect("cast the copy");
        let WaitingFor::CopyRetarget { copy_id, .. } = r.state().waiting_for else {
            panic!("expected the copy walk, got {:?}", r.state().waiting_for);
        };
        (r, Some(mastery), copy_id)
    } else {
        let offering = s
            .add_spell_to_hand_from_oracle(P0, "Volcanic Offering", true, VOLCANIC_OFFERING)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut r = s.build();
        begin_cast(&mut r, offering);
        (r, None, offering)
    };
    for (index, elected) in [(1, p2), (2, P1)] {
        let election = match &r.state().waiting_for {
            WaitingFor::ChooseAnnouncingOpponent {
                player,
                candidates,
                choice_index,
                choice_count,
                ..
            } => (*player, candidates.clone(), *choice_index, *choice_count),
            WaitingFor::CopyRetarget {
                player,
                announcer_election: Some(election),
                target_slots,
                ..
            } => {
                assert!(target_slots.is_empty(), "no slot is answered mid-election");
                (
                    *player,
                    election.candidates.clone(),
                    election.choice_index,
                    election.choice_count,
                )
            }
            other => panic!("expected election {index}, got {other:?}"),
        };
        assert_eq!(election.0, P0, "the caster elects");
        assert!(election.1.contains(&P1) && election.1.contains(&p2));
        assert_eq!((election.2, election.3), (index, 2));
        if copy && index == 1 {
            assert!(
                GameRunner::from_state(r.state().clone())
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(land_p1)),
                    })
                    .is_err(),
                "no target is announced before the election"
            );
        }
        if copy {
            // A save mid-election restores to the same election.
            let restored = restore(r.state());
            assert_eq!(
                restored.state().waiting_for,
                r.state().waiting_for,
                "election {index} survives save and restore"
            );
            r = restored;
        }
        r.act(GameAction::ChooseAnnouncingOpponent { opponent: elected })
            .expect("the caster elects an announcing opponent");
    }
    let mut askers: Vec<PlayerId> = Vec::new();
    for target in [land_p1, land_p2, creature_p1, creature_p2] {
        let asker = match r.state().waiting_for.clone() {
            WaitingFor::CopyRetarget {
                player, controller, ..
            } => {
                assert_eq!(controller.unwrap_or(player), P0, "the copy stays P0's");
                if player != P0 {
                    // A save while an opponent answers keeps who answers and
                    // whose copy it is.
                    let restored = restore(r.state());
                    assert_eq!(restored.state().waiting_for, r.state().waiting_for);
                    assert!(matches!(
                        restored.state().waiting_for,
                        WaitingFor::CopyRetarget { player, controller: Some(c), .. }
                            if player != P0 && c == P0
                    ));
                    r = restored;
                }
                player
            }
            WaitingFor::TargetSelection { player, .. } => player,
            other => panic!("expected an announcement prompt, got {other:?}"),
        };
        askers.push(asker);
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("the asked player announces");
    }
    assert_eq!(askers, vec![P0, p2, P0, P1]);
    resolve_entry(&mut r, entry);
    assert!([land_p1, land_p2]
        .iter()
        .all(|l| r.state().objects[l].zone == Zone::Graveyard));
    assert_eq!(
        [creature_p1, creature_p2].map(|c| r.state().objects[&c].damage_marked),
        [7, 7]
    );
    if let Some(mastery) = mastery {
        resolve_entry(&mut r, mastery);
        assert_eq!(r.state().objects[&mastery].zone, Zone::Exile);
    }
}

#[test]
fn copy_announcement_elects_announcing_opponents_per_effect() {
    volcanic_offering_three_players(true);
}

#[test]
fn ordinary_volcanic_offering_three_player_election_control() {
    volcanic_offering_three_players(false);
}

/// The three-player Mizzix's Mastery -> Volcanic Offering copy at its first
/// announcing-opponent election. Returns (runner, copy, [P1 land, P2 land]).
fn three_player_copy_at_election() -> (GameRunner, ObjectId, [ObjectId; 2]) {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 7);
    s.at_phase(Phase::PreCombatMain);
    let land_p1 = s.add_land_from_oracle(P1, "P1 Nonbasic", "").id();
    let land_p2 = s.add_land_from_oracle(p2, "P2 Nonbasic", "").id();
    s.add_creature(P1, "P1 Creature", 3, 12);
    s.add_creature(p2, "P2 Creature", 3, 12);
    let offering = s
        .add_spell_to_graveyard(P0, "Volcanic Offering", true)
        .from_oracle_text(VOLCANIC_OFFERING)
        .id();
    let mastery = s
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(mastery).target_object(offering).commit();
    pass_to_choice(&mut r);
    r.act(GameAction::SelectCards {
        cards: vec![offering],
    })
    .expect("cast the copy");
    let WaitingFor::CopyRetarget {
        copy_id,
        announcer_election: Some(_),
        ..
    } = r.state().waiting_for
    else {
        panic!(
            "expected the first election, got {:?}",
            r.state().waiting_for
        );
    };
    (r, copy_id, [land_p1, land_p2])
}

fn try_restore(
    wire: serde_json::Value,
) -> Result<engine::types::game_state::GameState, engine::types::game_state::PersistedRestoreError>
{
    use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization};
    serde_json::from_value::<PersistedGameState>(wire)
        .expect("decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)?
        .finalize_after_rehydration(|_| Ok(()))
}

fn save_wire(state: &engine::types::game_state::GameState) -> serde_json::Value {
    use engine::types::game_state::PersistedGameState;
    serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap()
}

/// The persisted `CopyRetarget` payload.
fn walk_wire(value: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    fn find(
        value: &mut serde_json::Value,
    ) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
        match value {
            serde_json::Value::Object(map) => {
                if map.contains_key("target_slots") && map.contains_key("copy_id") {
                    return Some(map);
                }
                map.values_mut().find_map(find)
            }
            serde_json::Value::Array(values) => values.iter_mut().find_map(find),
            _ => None,
        }
    }
    find(value).expect("a parked CopyRetarget payload")
}

/// CR 601.2c + CR 707.12 (?3 policy (a)): a saved copy announcement at its
/// first announcing-opponent election, on a board where no legal announcement
/// exists any more (every nonbasic land is gone), fails restore before
/// publication; the election is never published for an infeasible copy.
/// Control: the same save on the intact board restores to the election.
#[test]
fn copy_election_with_no_feasible_announcement_fails_closed_at_restore() {
    use engine::types::game_state::PersistedRestoreError;
    let (mut r, copy_id, lands) = three_player_copy_at_election();
    let control = try_restore(save_wire(r.state())).expect("control: the election restores");
    assert_eq!(control.waiting_for, r.state().waiting_for);
    let mut events = Vec::new();
    for land in lands {
        engine::game::zones::move_to_zone(r.state_mut(), land, Zone::Graveyard, &mut events);
    }
    assert_eq!(
        try_restore(save_wire(r.state())).map(|_| ()),
        Err(PersistedRestoreError::NonResumableCopyAnnouncement { copy_id }),
        "no election is published for an infeasible announcement"
    );
}

/// CR 601.2c + CR 115.1: a save from before the copy walk ran the election
/// (an r13-format announcement: a target already announced, both
/// opponent-choice groups unelected) is not resumable; it is refused with a
/// typed error before publication, never resumed with a seat-order announcer.
/// The election-time save (no targets yet) is the control: it restores to the
/// election.
#[test]
fn legacy_announcement_with_targets_before_the_election_is_refused() {
    use engine::types::game_state::PersistedRestoreError;
    let (r, copy_id, [land_p1, _]) = three_player_copy_at_election();
    let mut wire = save_wire(r.state());
    {
        let walk = walk_wire(&mut wire);
        walk.remove("announcer_election");
        walk.insert(
            "picks".into(),
            serde_json::json!([serde_json::to_value(TargetRef::Object(land_p1)).unwrap()]),
        );
        walk.insert("current_slot".into(), serde_json::json!(1));
        walk.insert("player".into(), serde_json::json!(1));
        walk.insert("controller".into(), serde_json::json!(0));
    }
    assert_eq!(
        try_restore(wire).map(|_| ()),
        Err(PersistedRestoreError::UnelectedCopyAnnouncer { copy_id })
    );
    assert!(
        try_restore(save_wire(r.state())).is_ok(),
        "control: the election-time save restores"
    );
}

/// The `n`-player Mizzix's Mastery -> Volcanic Offering copy after its
/// announcing-opponent elections (`electees`, one per opponent-choice group),
/// with P0's first land announced. P1 controls two nonbasic lands and two
/// creatures. Returns (runner, copy, [land a, land b], [creature a, creature b]).
fn copy_after_elections(
    n: u8,
    electees: [PlayerId; 2],
) -> (GameRunner, ObjectId, [ObjectId; 2], [ObjectId; 2]) {
    let mut s = GameScenario::new_n_player(n, 7);
    s.at_phase(Phase::PreCombatMain);
    let lands = [
        s.add_land_from_oracle(P1, "P1 Nonbasic A", "").id(),
        s.add_land_from_oracle(P1, "P1 Nonbasic B", "").id(),
    ];
    let creatures = [
        s.add_creature(P1, "P1 Creature A", 3, 12).id(),
        s.add_creature(P1, "P1 Creature B", 3, 12).id(),
    ];
    let offering = s
        .add_spell_to_graveyard(P0, "Volcanic Offering", true)
        .from_oracle_text(VOLCANIC_OFFERING)
        .id();
    let mastery = s
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(mastery).target_object(offering).commit();
    pass_to_choice(&mut r);
    r.act(GameAction::SelectCards {
        cards: vec![offering],
    })
    .expect("cast the copy");
    let WaitingFor::CopyRetarget { copy_id, .. } = r.state().waiting_for else {
        panic!("expected the copy walk, got {:?}", r.state().waiting_for);
    };
    for electee in electees {
        assert!(
            matches!(
                r.state().waiting_for,
                WaitingFor::CopyRetarget {
                    player: P0,
                    announcer_election: Some(_),
                    ..
                }
            ),
            "reach: the caster's election, got {:?}",
            r.state().waiting_for
        );
        r.act(GameAction::ChooseAnnouncingOpponent { opponent: electee })
            .expect("elect");
    }
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(lands[0])),
    })
    .expect("P0 announces its land");
    (r, copy_id, lands, creatures)
}

/// The current copy-announcement prompt: (answering player, the copy's
/// controller, the decided prefix, whether it is an election).
fn walk_prompt(r: &GameRunner) -> (PlayerId, PlayerId, Vec<Option<TargetRef>>, bool) {
    match &r.state().waiting_for {
        WaitingFor::CopyRetarget {
            player,
            controller,
            picks,
            current_slot,
            mode,
            announcer_election,
            ..
        } => {
            assert_eq!(*mode, Some(CopyChoiceMode::Announce));
            let picks = picks.clone().expect("an explicit prefix");
            assert_eq!(*current_slot, picks.len(), "the cursor follows the prefix");
            (
                *player,
                controller.unwrap_or(*player),
                picks,
                announcer_election.is_some(),
            )
        }
        other => panic!("expected the copy announcement, got {other:?}"),
    }
}

/// Answer the remaining announcement with `targets`, recording who answers.
fn announce_rest(r: &mut GameRunner, targets: &[ObjectId]) -> Vec<PlayerId> {
    targets
        .iter()
        .map(|target| {
            let (player, controller, _, election) = walk_prompt(r);
            assert!(!election);
            assert_eq!(controller, P0, "the copy stays P0's");
            r.act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(*target)),
            })
            .expect("the asked player announces");
            player
        })
        .collect()
}

fn assert_offering_resolved(
    r: &mut GameRunner,
    copy: ObjectId,
    lands: [ObjectId; 2],
    creatures: [ObjectId; 2],
) {
    resolve_entry(r, copy);
    assert!(
        lands
            .iter()
            .all(|land| r.state().objects[land].zone == Zone::Graveyard),
        "both announced lands are destroyed"
    );
    assert_eq!(
        creatures.map(|c| r.state().objects[&c].damage_marked),
        [7, 7]
    );
}

/// CR 800.4g + CR 601.2c (three players): P2, elected to announce the copy's
/// second land, concedes while answering. The copy's controller chooses
/// another opponent; with P1 the only one left there is no decision, so P1
/// answers at once. The walk, its decided prefix (P0's land) and the copy's
/// controller are kept, it continues P1/P0/P1, and the copy resolves in full.
#[test]
fn copy_announcement_replaces_a_departed_announcer_and_keeps_the_prefix() {
    let p2 = PlayerId(2);
    let (mut r, copy, lands, creatures) = copy_after_elections(3, [p2, P1]);
    assert_eq!(walk_prompt(&r).0, p2, "reach: P2 answers the second land");
    r.act(GameAction::Concede { player_id: p2 })
        .expect("P2 concedes");
    let (player, controller, picks, election) = walk_prompt(&r);
    assert_eq!(
        (player, controller, picks, election),
        (P1, P0, vec![Some(TargetRef::Object(lands[0]))], false),
        "the remaining opponent answers; the prefix and controller are kept"
    );
    let restored = restore(r.state());
    assert_eq!(restored.state().waiting_for, r.state().waiting_for);
    r = restored;
    let askers = announce_rest(&mut r, &[lands[1], creatures[0], creatures[1]]);
    assert_eq!(askers, vec![P1, P0, P1]);
    assert_offering_resolved(&mut r, copy, lands, creatures);
}

/// CR 800.4g + CR 601.2c (four players): P2 concedes while answering, and two
/// opponents remain, so the copy's controller chooses the replacement: P0 is
/// asked to elect between P1 and P3, with the decided prefix kept (no target
/// may be announced meanwhile; a save restores to the same election). P3 is
/// elected and announces the land; the walk continues and the copy resolves.
#[test]
fn copy_announcement_controller_elects_a_replacement_announcer() {
    let (p2, p3) = (PlayerId(2), PlayerId(3));
    let (mut r, copy, lands, creatures) = copy_after_elections(4, [p2, P1]);
    assert_eq!(walk_prompt(&r).0, p2, "reach: P2 answers the second land");
    r.act(GameAction::Concede { player_id: p2 })
        .expect("P2 concedes");
    let (player, controller, picks, election) = walk_prompt(&r);
    assert_eq!(
        (player, controller, picks, election),
        (P0, P0, vec![Some(TargetRef::Object(lands[0]))], true),
        "the copy's controller elects; the prefix is kept"
    );
    let WaitingFor::CopyRetarget {
        announcer_election: Some(election),
        ..
    } = &r.state().waiting_for
    else {
        unreachable!()
    };
    assert_eq!(election.candidates, vec![P1, p3], "another opponent");
    assert!(
        GameRunner::from_state(r.state().clone())
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(lands[1])),
            })
            .is_err(),
        "no target is announced mid-election"
    );
    let restored = restore(r.state());
    assert_eq!(restored.state().waiting_for, r.state().waiting_for);
    r = restored;
    r.act(GameAction::ChooseAnnouncingOpponent { opponent: p3 })
        .expect("P0 elects P3");
    let askers = announce_rest(&mut r, &[lands[1], creatures[0], creatures[1]]);
    assert_eq!(askers, vec![p3, P0, P1]);
    assert_offering_resolved(&mut r, copy, lands, creatures);
}

/// CR 800.4g control (four players): P2 announced the copy's second land and
/// then concedes while P3 answers the second creature. P2's choice is already
/// made, so nothing is re-asked: the prompt is unchanged, the walk completes
/// with P2's land, and the copy resolves.
#[test]
fn a_departed_announcer_whose_choice_is_made_is_not_replaced() {
    let (p2, p3) = (PlayerId(2), PlayerId(3));
    let (mut r, copy, lands, creatures) = copy_after_elections(4, [p2, p3]);
    let askers = announce_rest(&mut r, &[lands[1], creatures[0]]);
    assert_eq!(askers, vec![p2, P0]);
    assert_eq!(
        walk_prompt(&r).0,
        p3,
        "reach: P3 answers the second creature"
    );
    let before = r.state().waiting_for.clone();
    r.act(GameAction::Concede { player_id: p2 })
        .expect("P2 concedes");
    assert_eq!(r.state().waiting_for, before, "the walk is unchanged");
    assert_eq!(announce_rest(&mut r, &[creatures[1]]), vec![p3]);
    assert_offering_resolved(&mut r, copy, lands, creatures);
}

/// The `n`-player Mizzix's Mastery -> Volcanic Offering copy at its first
/// announcing-opponent election. Nonbasic land A is owned and controlled by
/// `land_a_owner`, land B by P1; P1 controls two creatures (toughness 12).
/// Returns (runner, copy, [A, B], [creature a, creature b]).
fn offering_copy_at_election(
    n: u8,
    land_a_owner: PlayerId,
) -> (GameRunner, ObjectId, [ObjectId; 2], [ObjectId; 2]) {
    let mut s = GameScenario::new_n_player(n, 7);
    s.at_phase(Phase::PreCombatMain);
    let lands = [
        s.add_land_from_oracle(land_a_owner, "Nonbasic A", "").id(),
        s.add_land_from_oracle(P1, "Nonbasic B", "").id(),
    ];
    let creatures = [
        s.add_creature(P1, "P1 Creature A", 3, 12).id(),
        s.add_creature(P1, "P1 Creature B", 3, 12).id(),
    ];
    let offering = s
        .add_spell_to_graveyard(P0, "Volcanic Offering", true)
        .from_oracle_text(VOLCANIC_OFFERING)
        .id();
    let mastery = s
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(mastery).target_object(offering).commit();
    pass_to_choice(&mut r);
    r.act(GameAction::SelectCards {
        cards: vec![offering],
    })
    .expect("cast the copy");
    let WaitingFor::CopyRetarget {
        copy_id,
        announcer_election: Some(_),
        ..
    } = r.state().waiting_for
    else {
        panic!(
            "expected the first election, got {:?}",
            r.state().waiting_for
        );
    };
    (r, copy_id, lands, creatures)
}

fn elect(r: &mut GameRunner, electees: [PlayerId; 2]) {
    for electee in electees {
        r.act(GameAction::ChooseAnnouncingOpponent { opponent: electee })
            .expect("elect");
    }
}

/// Probe board 1 (CR 800.4a + CR 800.4g, three players): P2, elected to
/// announce the copy's second land, owns land A, which P0 announced first.
/// P2 concedes while answering: A leaves the game with its owner, so P0's
/// pick of A is no longer an answerable choice. The copy walk keeps going (no
/// panic, no fallback to Priority): the prefix is replayed on the current
/// board and re-asked from the first pick it refuses, the land group goes to
/// P1, the only remaining opponent, and the copy stays P0's and resolves.
/// Control: A owned by the surviving P1 keeps the prefix `[A]`, and P1
/// replaces P2.
#[test]
fn a_prefix_pick_owned_by_the_departed_announcer_is_re_asked() {
    for a_owner in [PlayerId(2), P1] {
        let label = format!("A owned by {a_owner:?}");
        let p2 = PlayerId(2);
        let (mut r, copy, [a, b], creatures) = offering_copy_at_election(3, a_owner);
        elect(&mut r, [p2, P1]);
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a)),
        })
        .expect("P0 announces A");
        assert_eq!(
            walk_prompt(&r),
            (p2, P0, vec![Some(TargetRef::Object(a))], false),
            "{label}: reach: P2 answers, the copy is P0's, the prefix is [A]"
        );
        r.act(GameAction::Concede { player_id: p2 })
            .expect("P2 concedes");
        let (player, controller, picks, election) = walk_prompt(&r);
        assert_eq!(controller, P0, "{label}: the copy stays P0's");
        assert!(!election, "{label}: one opponent remains, no election");
        let rest: Vec<ObjectId> = if a_owner == P1 {
            assert_eq!(
                (player, picks),
                (P1, vec![Some(TargetRef::Object(a))]),
                "{label}: the prefix is kept and P1 replaces P2"
            );
            vec![b, creatures[0], creatures[1]]
        } else {
            assert_eq!(
                (player, picks),
                (P0, vec![]),
                "{label}: A left the game, so the walk re-asks P0's first land"
            );
            vec![b, b, creatures[0], creatures[1]]
        };
        let askers = announce_rest(&mut r, &rest);
        if a_owner == P1 {
            assert_eq!(askers, vec![P1, P0, P1], "{label}");
        } else {
            assert_eq!(askers, vec![P0, P1, P0, P1], "{label}");
        }
        resolve_entry(&mut r, copy);
        let destroyed = if a_owner == P1 { vec![a, b] } else { vec![b] };
        assert!(
            destroyed
                .iter()
                .all(|land| r.state().objects[land].zone == Zone::Graveyard),
            "{label}: the announced lands are destroyed"
        );
        assert_eq!(
            creatures.map(|c| r.state().objects[&c].damage_marked),
            [7, 7],
            "{label}"
        );
    }
}

/// Probe board 2 (CR 800.4g, four players): P2 concedes while answering, so P0
/// is asked to elect a replacement from [P1, P3]; before P0 answers, P3
/// concedes too. The election is rebuilt against the players still in the
/// game: P1 is the only opponent left, so it is assigned with no prompt, P1
/// announces the second land, and the copy resolves. Control: P0 elects P3,
/// and then P3 concedes while answering; P1 replaces P3 the same way.
#[test]
fn a_second_departure_rebuilds_the_replacement_election() {
    let (p2, p3) = (PlayerId(2), PlayerId(3));
    for elect_p3_first in [false, true] {
        let label = format!("elect P3 first: {elect_p3_first}");
        let (mut r, copy, lands, creatures) = offering_copy_at_election(4, P1);
        elect(&mut r, [p2, P1]);
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(lands[0])),
        })
        .expect("P0 announces its land");
        r.act(GameAction::Concede { player_id: p2 })
            .expect("P2 concedes");
        let prefix = vec![Some(TargetRef::Object(lands[0]))];
        assert_eq!(
            walk_prompt(&r),
            (P0, P0, prefix.clone(), true),
            "{label}: reach: P0's replacement election, prefix kept"
        );
        let WaitingFor::CopyRetarget {
            copy_id,
            announcer_election: Some(election),
            ..
        } = &r.state().waiting_for
        else {
            unreachable!()
        };
        assert_eq!(
            (*copy_id, election.candidates.clone()),
            (copy, vec![P1, p3])
        );
        if elect_p3_first {
            r.act(GameAction::ChooseAnnouncingOpponent { opponent: p3 })
                .expect("P0 elects P3");
            assert_eq!(walk_prompt(&r).0, p3, "{label}: reach: P3 answers");
        }
        r.act(GameAction::Concede { player_id: p3 })
            .expect("P3 concedes");
        assert_eq!(
            walk_prompt(&r),
            (P1, P0, prefix, false),
            "{label}: P1, the only opponent left, answers; no stale election"
        );
        let askers = announce_rest(&mut r, &[lands[1], creatures[0], creatures[1]]);
        assert_eq!(askers, vec![P1, P0, P1], "{label}");
        assert_offering_resolved(&mut r, copy, lands, creatures);
    }
}

/// CR 800.4a + CR 601.2e (three players): P2 owns the only nonbasic land A,
/// which P0 announced first; P2 concedes while answering. A leaves the game
/// with its owner and no nonbasic land remains, so the copy can no longer be
/// announced: its cast is illegal and the copy ceases to exist (no panic, no
/// stranded prompt). Mizzix's Mastery finishes resolving and is exiled, and
/// nothing is destroyed or damaged.
#[test]
fn an_unannounceable_copy_after_a_departure_ceases_to_exist() {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 7);
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_land_from_oracle(p2, "Nonbasic A", "").id();
    let creatures = [
        s.add_creature(P1, "P1 Creature A", 3, 12).id(),
        s.add_creature(P1, "P1 Creature B", 3, 12).id(),
    ];
    let offering = s
        .add_spell_to_graveyard(P0, "Volcanic Offering", true)
        .from_oracle_text(VOLCANIC_OFFERING)
        .id();
    let mastery = s
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(mastery).target_object(offering).commit();
    pass_to_choice(&mut r);
    r.act(GameAction::SelectCards {
        cards: vec![offering],
    })
    .expect("cast the copy");
    let WaitingFor::CopyRetarget { copy_id: copy, .. } = r.state().waiting_for else {
        panic!("expected the copy walk, got {:?}", r.state().waiting_for);
    };
    elect(&mut r, [p2, P1]);
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(a)),
    })
    .expect("P0 announces A");
    assert_eq!(
        walk_prompt(&r),
        (p2, P0, vec![Some(TargetRef::Object(a))], false),
        "reach: P2 answers the second land"
    );
    r.act(GameAction::Concede { player_id: p2 })
        .expect("P2 concedes");
    assert!(
        !matches!(r.state().waiting_for, WaitingFor::CopyRetarget { .. }),
        "no stranded announcement: {:?}",
        r.state().waiting_for
    );
    assert!(
        !r.state().stack.iter().any(|entry| entry.id == copy),
        "the copy ceased to exist"
    );
    resolve_entry(&mut r, mastery);
    assert_eq!(
        r.state().objects[&mastery].zone,
        Zone::Exile,
        "Mastery resolves"
    );
    assert_eq!(
        creatures.map(|c| r.state().objects[&c].damage_marked),
        [0, 0]
    );
}

const YOUNG_PYROMANCER: &str =
    "Whenever you cast an instant or sorcery spell, create a 1/1 red Elemental creature token.";

fn elementals(r: &GameRunner) -> usize {
    r.state()
        .battlefield
        .iter()
        .filter(|id| {
            r.state().objects[id].controller == P0
                && r.state().objects[id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s == "Elemental")
        })
        .count()
}

/// P0's cast history: (spells cast this turn, P0's turn records, P0's game count).
fn cast_history(r: &GameRunner) -> (u8, usize, u32) {
    let s = r.state();
    (
        s.spells_cast_this_turn,
        s.spells_cast_this_turn_by_player
            .get(&P0)
            .map_or(0, |h| h.len()),
        s.spells_cast_this_game.get(&P0).copied().unwrap_or(0),
    )
}

const STORM_ENTITY: &str =
    "Haste\nThis creature enters with a +1/+1 counter on it for each other spell cast this turn.";

/// Probe board (CR 601.2e + CR 601.2i + CR 707.12 + CR 733.1, three players):
/// P0 controls Young Pyromancer. Mizzix's Mastery is cast (its Pyromancer
/// trigger resolves: one Elemental), then the Volcanic Offering copy is cast;
/// P2 owns the only nonbasic land A, which P0 announces first, and P2 concedes
/// while answering. The copy can no longer be announced, so it never becomes
/// cast: it ceases to exist (CR 704.5e), no ability triggers from it (no second
/// Elemental), and no cast is recorded: the cast history shows only Mastery,
/// and a later Storm Entity (a cast-history consumer) enters with one counter.
/// Control: A owned by the surviving P1, the announcement completes legally,
/// and the Offering copy adds one Elemental, one cast record and one Storm
/// Entity counter.
#[test]
fn an_aborted_copy_is_never_cast() {
    let p2 = PlayerId(2);
    for a_owner in [p2, P1] {
        let label = format!("A owned by {a_owner:?}");
        let mut s = GameScenario::new_n_player(3, 7);
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Young Pyromancer", 2, 1, YOUNG_PYROMANCER);
        let entity = s
            .add_creature_to_hand_from_oracle(P0, "Storm Entity", 1, 1, STORM_ENTITY)
            .with_mana_cost(ManaCost::zero())
            .id();
        let a = s.add_land_from_oracle(a_owner, "Nonbasic A", "").id();
        let creatures = [
            s.add_creature(P1, "P1 Creature A", 3, 12).id(),
            s.add_creature(P1, "P1 Creature B", 3, 12).id(),
        ];
        let offering = s
            .add_spell_to_graveyard(P0, "Volcanic Offering", true)
            .from_oracle_text(VOLCANIC_OFFERING)
            .id();
        let mastery = s
            .add_spell_to_hand(P0, "Mizzix's Mastery", false)
            .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut r = s.build();
        r.cast(mastery).target_object(offering).commit();
        // Resolve Mastery's Pyromancer trigger, then reach the copy offer.
        pass_to_choice(&mut r);
        assert_eq!(elementals(&r), 1, "{label}: reach: Mastery's Elemental");
        let baseline = cast_history(&r);
        assert_eq!(baseline.1, 1, "{label}: reach: Mastery is P0's one cast");
        r.act(GameAction::SelectCards {
            cards: vec![offering],
        })
        .expect("cast the copy");
        let WaitingFor::CopyRetarget { copy_id: copy, .. } = r.state().waiting_for else {
            panic!(
                "{label}: expected the copy walk, got {:?}",
                r.state().waiting_for
            );
        };
        elect(&mut r, [p2, P1]);
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a)),
        })
        .expect("P0 announces A");
        assert_eq!(
            walk_prompt(&r),
            (p2, P0, vec![Some(TargetRef::Object(a))], false),
            "{label}: reach: P2 answers, P0's copy, prefix [A]"
        );
        assert!(
            matches!(r.state().waiting_for, WaitingFor::CopyRetarget { copy_id, .. } if copy_id == copy)
        );
        if a_owner == P1 {
            // Control: the announcement completes legally.
            let askers = announce_rest(&mut r, &[a, creatures[0], creatures[1]]);
            assert_eq!(askers, vec![p2, P0, P1], "{label}");
        } else {
            r.act(GameAction::Concede { player_id: p2 })
                .expect("P2 concedes");
            assert!(
                !r.state().stack.iter().any(|entry| entry.id == copy),
                "{label}: the copy is gone"
            );
            assert!(
                !r.state().deferred_triggers.iter().any(|parked| parked
                    .trigger_events
                    .iter()
                    .any(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == copy))),
                "{label}: no observer of the undone cast stays parked"
            );
        }
        for _ in 0..64 {
            if r.state().stack.is_empty() && r.state().deferred_triggers.is_empty() {
                break;
            }
            r.act(GameAction::PassPriority).expect("resolve");
        }
        assert!(r.state().stack.is_empty(), "{label}: the stack drains");
        assert_eq!(r.state().objects[&mastery].zone, Zone::Exile, "{label}");
        let casts: u8 = if a_owner == P1 { 1 } else { 0 };
        let extra_elementals = usize::from(casts);
        assert_eq!(elementals(&r), 1 + extra_elementals, "{label}: Elementals");
        assert_eq!(
            cast_history(&r),
            (
                baseline.0 + casts,
                baseline.1 + usize::from(casts),
                baseline.2 + u32::from(casts)
            ),
            "{label}: cast history"
        );
        // CR 601.2i: a later cast-history reader sees only completed casts.
        r.cast(entity).commit();
        resolve_entry(&mut r, entity);
        for _ in 0..64 {
            if r.state().stack.is_empty() && r.state().deferred_triggers.is_empty() {
                break;
            }
            r.act(GameAction::PassPriority).expect("resolve");
        }
        assert_eq!(
            r.state().objects[&entity].zone,
            Zone::Battlefield,
            "{label}: reach"
        );
        assert_eq!(
            r.state().objects[&entity]
                .counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1 + u32::from(casts),
            "{label}: Storm Entity counts the other spells cast this turn"
        );
    }
}

const GRAPESHOT: &str = "Grapeshot deals 1 damage to any target.\nStorm (When you cast this spell, copy it for each spell cast before it this turn. You may choose new targets for the copies.)";
const DARK_RITUAL: &str = "Add {B}{B}{B}.";

/// CR 702.40a + CR 603.2 + CR 601.2i: overloaded Mizzix's Mastery exiles
/// Grapeshot and Dark Ritual and casts their copies in the chosen order.
/// Grapeshot's copy becomes cast when its announcement (P1) completes, and its
/// Storm trigger triggers then, counting the spells cast before it: with
/// Grapeshot first that is Mastery alone (one Storm copy, 2 damage to P1),
/// even though the Ritual copy is cast later in the same resolution. Control:
/// Ritual first, so Storm counts Mastery and the Ritual (two copies, 3 damage).
#[test]
fn storm_counts_only_spells_cast_before_the_copy_announced_it() {
    for grapeshot_first in [true, false] {
        let label = format!("Grapeshot first: {grapeshot_first}");
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Young Pyromancer", 2, 1, YOUNG_PYROMANCER);
        let grapeshot = s
            .add_spell_to_graveyard(P0, "Grapeshot", false)
            .from_oracle_text_with_keywords(&["Storm"], GRAPESHOT)
            .id();
        let ritual = s
            .add_spell_to_graveyard(P0, "Dark Ritual", true)
            .from_oracle_text(DARK_RITUAL)
            .id();
        let mastery = s
            .add_spell_to_hand(P0, "Mizzix's Mastery", false)
            .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut r = s.build();
        let p1_life = |r: &GameRunner| r.state().players.iter().find(|p| p.id == P1).unwrap().life;
        let life_before = p1_life(&r);
        // Overload {5}{R}{R}{R}.
        {
            let pool = &mut r
                .state_mut()
                .players
                .iter_mut()
                .find(|p| p.id == P0)
                .unwrap()
                .mana_pool;
            for _ in 0..8 {
                pool.add(engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Red,
                    ObjectId(0),
                    false,
                    vec![],
                ));
            }
        }
        begin_cast(&mut r, mastery);
        assert!(
            matches!(
                r.state().waiting_for,
                WaitingFor::AlternativeCastChoice { .. }
            ),
            "{label}: reach: overload is offered, got {:?}",
            r.state().waiting_for
        );
        r.act(GameAction::ChooseAlternativeCast {
            choice: engine::types::actions::AlternativeCastDecision::Alternative,
        })
        .expect("overload");
        pass_to_choice(&mut r);
        let mut groups: Vec<Vec<String>> = Vec::new();
        let order = if grapeshot_first {
            vec![grapeshot, ritual]
        } else {
            vec![ritual, grapeshot]
        };
        r.act(GameAction::SelectCards { cards: order })
            .expect("cast both copies");
        for _ in 0..64 {
            match r.state().waiting_for.clone() {
                WaitingFor::CopyRetarget {
                    mode: Some(CopyChoiceMode::Announce),
                    ..
                } => {
                    r.act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("announce P1 for Grapeshot");
                }
                WaitingFor::CopyRetarget { .. } => {
                    r.act(GameAction::KeepAllCopyTargets)
                        .expect("Storm copies keep their target");
                }
                WaitingFor::Priority { .. } => {
                    if r.state().stack.is_empty() && r.state().deferred_triggers.is_empty() {
                        break;
                    }
                    r.act(GameAction::PassPriority).expect("resolve");
                }
                WaitingFor::OrderTriggers { triggers, .. } => {
                    groups.push(triggers.iter().map(|s| s.source_name.clone()).collect());
                    r.act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order the cast triggers");
                }
                other => panic!("{label}: unexpected prompt {other:?}"),
            }
        }
        assert!(r.state().stack.is_empty(), "{label}: the stack drains");
        assert_eq!(
            r.state().objects[&mastery].zone,
            Zone::Exile,
            "{label}: reach"
        );
        let expected = if grapeshot_first { 2 } else { 3 };
        assert_eq!(
            life_before - p1_life(&r),
            expected,
            "{label}: Grapeshot damage"
        );
        // CR 603.2 + CR 603.3b: Young Pyromancer triggers on Mastery and on
        // both copies, wherever they fall in the resolution.
        assert_eq!(elementals(&r), 3, "{label}: Elementals");
        // CR 603.3b: both copies' observers (two Pyromancer triggers and
        // Grapeshot's Storm) are put on the stack as one ordering group.
        assert_eq!(
            groups.len(),
            1,
            "{label}: one ordering group, got {groups:?}"
        );
        let mut group = groups[0].clone();
        group.sort();
        assert_eq!(
            group,
            vec![
                "Grapeshot".to_string(),
                "Young Pyromancer".to_string(),
                "Young Pyromancer".to_string()
            ],
            "{label}: the group's members"
        );
    }
}

const TEVAL: &str = "Flying\nWhenever Teval attacks, mill three cards. Then you may return a land card from your graveyard to the battlefield tapped.\nWhenever one or more cards leave your graveyard, create a 2/2 black Zombie Druid creature token.";

/// CR 603.3 + CR 603.3b: Mizzix's Mastery exiles Grapeshot from P0's
/// graveyard (Teval, the Balanced Scale triggers) and casts its copy, which P0
/// announces at P1 (Storm triggers). Teval's triggers were parked during
/// Mastery's resolution and Storm was latched when the copy became cast; once
/// the resolution finishes they are put on the stack as one ordering group,
/// above the Grapeshot copy, before anyone gets priority.
#[test]
fn a_resolution_s_parked_observers_join_the_copy_cast_batch() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Teval, the Balanced Scale", 4, 4, TEVAL);
    let grapeshot = s
        .add_spell_to_graveyard(P0, "Grapeshot", false)
        .from_oracle_text_with_keywords(&["Storm"], GRAPESHOT)
        .id();
    let mastery = s
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(mastery).target_object(grapeshot).commit();
    pass_to_choice(&mut r);
    r.act(GameAction::SelectCards {
        cards: vec![grapeshot],
    })
    .expect("cast the Grapeshot copy");
    let WaitingFor::CopyRetarget { copy_id: copy, .. } = r.state().waiting_for else {
        panic!(
            "expected the copy announcement, got {:?}",
            r.state().waiting_for
        );
    };
    let parked_teval = r.state().deferred_triggers.len();
    assert!(
        parked_teval >= 1,
        "reach: Teval's trigger is parked during the resolution"
    );
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Player(P1)),
    })
    .expect("announce P1");
    let WaitingFor::OrderTriggers { triggers, .. } = &r.state().waiting_for else {
        panic!(
            "expected one ordering group before priority, got {:?}",
            r.state().waiting_for
        );
    };
    // Every observer of the finished resolution is in the group: Storm, and
    // each Teval trigger parked while Mastery resolved.
    let group: Vec<String> = triggers.iter().map(|s| s.source_name.clone()).collect();
    let count = |name: &str| group.iter().filter(|member| *member == name).count();
    assert_eq!(count("Grapeshot"), 1, "Storm is in the group: {group:?}");
    assert_eq!(
        count("Teval, the Balanced Scale"),
        parked_teval,
        "every parked Teval trigger is in the same group: {group:?}"
    );
    assert_eq!(group.len(), parked_teval + 1, "nothing else: {group:?}");
    assert!(
        r.state().stack.iter().any(|entry| entry.id == copy),
        "the group is put on the stack while the copy is still on it"
    );
}

const MAGNIVORE: &str = "Haste\nMagnivore's power and toughness are each equal to the number of sorcery cards in all graveyards.";
const MAKE_YOUR_MARK: &str = "Target creature gets +1/+0 until end of turn. When that creature dies this turn, create a 3/2 red and white Spirit creature token.";

/// CR 603.2 + CR 603.3b + CR 704.3: a delayed observer of a state-based
/// action that follows a finished copy-casting resolution joins the same
/// ordering group as the copy's cast observers. Make Your Mark set up "when
/// Magnivore dies this turn"; Mizzix's Mastery exiles Grapeshot, the only
/// sorcery in a graveyard, so Magnivore is 0/0 once the resolution ends, and
/// the Grapeshot copy is announced at P1 (Storm). Magnivore dies to the
/// state-based check of that action, and the delayed trigger and Storm are
/// ordered together before anyone gets priority.
#[test]
fn an_sba_delayed_observer_joins_the_copy_cast_batch() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let magnivore = s
        .add_creature_from_oracle(P0, "Magnivore", 0, 0, MAGNIVORE)
        .id();
    let mark = s
        .add_spell_to_hand_from_oracle(P0, "Make Your Mark", true, MAKE_YOUR_MARK)
        .with_mana_cost(ManaCost::zero())
        .id();
    let grapeshot = s
        .add_spell_to_graveyard(P0, "Grapeshot", false)
        .from_oracle_text_with_keywords(&["Storm"], GRAPESHOT)
        .id();
    let mastery = s
        .add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    r.cast(mark).target_object(magnivore).commit();
    resolve_entry(&mut r, mark);
    assert_eq!(
        r.state().objects[&magnivore].zone,
        Zone::Battlefield,
        "reach: Magnivore lives while Grapeshot is in the graveyard"
    );
    r.cast(mastery).target_object(grapeshot).commit();
    pass_to_choice(&mut r);
    r.act(GameAction::SelectCards {
        cards: vec![grapeshot],
    })
    .expect("cast the Grapeshot copy");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::CopyRetarget { .. }),
        "reach: the copy announcement, got {:?}",
        r.state().waiting_for
    );
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Player(P1)),
    })
    .expect("announce P1");
    assert_ne!(
        r.state().objects[&magnivore].zone,
        Zone::Battlefield,
        "reach: Magnivore died to the state-based check"
    );
    let WaitingFor::OrderTriggers { triggers, .. } = &r.state().waiting_for else {
        panic!(
            "expected one ordering group before priority, got {:?}",
            r.state().waiting_for
        );
    };
    let mut group: Vec<String> = triggers.iter().map(|s| s.source_name.clone()).collect();
    group.sort();
    assert_eq!(
        group,
        vec!["Grapeshot".to_string(), "Make Your Mark".to_string()],
        "Storm and the delayed death trigger share the ordering group"
    );
}
