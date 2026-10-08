//! Mizzix's Mastery (OTC #175) — runtime pipeline coverage.
//!
//! "Exile target card that's an instant or sorcery from your graveyard. For each
//! card exiled this way, copy it, and you may cast the copy without paying its
//! mana cost. Exile Mizzix's Mastery." + "Overload {5}{R}{R}{R}".
//!
//! These drive the REAL pipeline (parser fix → `GameRunner::act` cast →
//! `ChangeZone` exile → `CastCopyOfCard` → `ChooseFromZoneChoice`) through the
//! parser-backed card (no card-data fixture needed — `from_oracle_text` parses the
//! shipped Oracle text through the same code path as production).
//!
//! Revert probes:
//!  * Reverting `try_split_copy_cast_compound` drops the ", and you may cast the
//!    copy …" tail, so no `CastCopyOfCard` is produced and the copy is never
//!    cast — the accept test's `+3 life` and the overload test's copy/life
//!    assertions flip.
//!  * Reverting the `ChangeZone{SelfRef}` overload guard promotes the self-exile
//!    to `ChangeZoneAll`, which finds nothing on the battlefield, so Mizzix's
//!    Mastery falls to the graveyard instead of exile — the overload
//!    "ends in exile" assertions flip.
//!
//! CR 707.12a per-copy optionality is covered both by declining the only normal
//! cast copy and by accepting one of two overload copies (`+3`, not `+6`).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MIZZIX: &str = "Exile target card that's an instant or sorcery from your graveyard. For each card exiled this way, copy it, and you may cast the copy without paying its mana cost. Exile Mizzix's Mastery.\nOverload {5}{R}{R}{R} (You may cast this spell for its overload cost. If you do, change \"target\" in its text to \"each.\")";

/// A graveyard instant/sorcery whose copy has a target-free, observable effect
/// (gain a chosen life amount for the copy's controller). Gain-life avoids copy-target
/// selection so the `CastCopyOfCard` choice is the only interactive prompt.
fn add_gain_life_spell(
    scenario: &mut GameScenario,
    name: &str,
    is_instant: bool,
    amount: i32,
) -> ObjectId {
    let mut b = scenario.add_spell_to_graveyard(P0, name, is_instant);
    b.from_oracle_text(&format!("You gain {amount} life."));
    b.id()
}

fn add_mizzix(scenario: &mut GameScenario) -> ObjectId {
    let mut b = scenario.add_spell_to_hand(P0, "Mizzix's Mastery", /* is_instant */ false);
    b.from_oracle_text_with_keywords(&["Overload"], MIZZIX);
    b.id()
}

fn give_red(runner: &mut GameRunner, n: usize) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..n {
        pool.add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));
    }
}

/// P0 holds priority on an empty stack in a main phase.
fn with_p0_priority(runner: &mut GameRunner) {
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
}

/// Pass priority (auto both players) until a non-priority prompt opens or the
/// stack drains to a settled priority window with an empty stack.
fn settle(runner: &mut GameRunner) {
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    // Try to advance; if nothing changes we are settled.
                    let before = runner.state().waiting_for.clone();
                    if runner.act(GameAction::PassPriority).is_err() {
                        return;
                    }
                    if runner.state().stack.is_empty() && runner.state().waiting_for == before {
                        return;
                    }
                } else if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

fn p0_life(runner: &GameRunner) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .life
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Option<Zone> {
    runner.state().objects.get(&id).map(|o| o.zone)
}

/// Cast Mizzix normally, declaring `target` for the "exile target card" slot, and
/// drive priority to the resolution-time copy-cast choice.
fn cast_normal_to_copy_choice(runner: &mut GameRunner, mizzix: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&mizzix].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: mizzix,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Mizzix's Mastery");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("declare the graveyard exile target");
    }
    settle(runner);
}

#[test]
fn normal_cast_accepts_and_casts_the_copy_then_self_exiles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mizzix = add_mizzix(&mut scenario);
    let gy = add_gain_life_spell(&mut scenario, "Test Bolt", /* instant */ true, 3);
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);
    let life0 = p0_life(&runner);

    cast_normal_to_copy_choice(&mut runner, mizzix, gy);

    // The exile published a tracked set; the copy-cast choice is offered.
    let cards = match runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, up_to, .. } => {
            assert!(up_to, "CR 707.12a: casting each copy is optional");
            cards
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    };
    assert!(
        cards.contains(&gy),
        "the exiled instant is offered as a copy source"
    );
    // Accept: cast the copy.
    runner
        .act(GameAction::SelectCards { cards: vec![gy] })
        .expect("cast the copy");
    settle(&mut runner);

    // Observable: the copy of "You gain 3 life." resolved for P0.
    assert_eq!(
        p0_life(&runner),
        life0 + 3,
        "the cast copy must resolve and gain 3 life (revert probe: no gain)"
    );
    // The exiled source stays in exile; Mizzix's Mastery self-exiles.
    assert_eq!(
        zone_of(&runner, gy),
        Some(Zone::Exile),
        "exiled card stays exiled"
    );
    assert_eq!(
        zone_of(&runner, mizzix),
        Some(Zone::Exile),
        "Mizzix's Mastery exiles itself, not to the graveyard"
    );
}

#[test]
fn normal_cast_can_decline_its_only_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mizzix = add_mizzix(&mut scenario);
    let gy = add_gain_life_spell(&mut scenario, "Test Bolt", /* instant */ true, 3);
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);
    let life0 = p0_life(&runner);

    cast_normal_to_copy_choice(&mut runner, mizzix, gy);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { up_to: true, .. }
    ));
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("decline the only copy");
    settle(&mut runner);

    // CR 707.12a: each produced copy is independently optional. With one
    // offered copy, selecting none must leave life unchanged and must not
    // rebind the continuation to the tracked source card.
    assert_eq!(
        p0_life(&runner),
        life0,
        "declining the only copy must not cast it"
    );
    assert_eq!(zone_of(&runner, gy), Some(Zone::Exile));
    assert_eq!(zone_of(&runner, mizzix), Some(Zone::Exile));
}

#[test]
fn creature_card_in_graveyard_is_not_a_legal_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mizzix = add_mizzix(&mut scenario);
    // Only a creature card in the graveyard — not an instant or sorcery.
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    // Move the creature card into the graveyard as a card (not a battlefield
    // permanent).
    move_to_graveyard(&mut runner, creature);
    with_p0_priority(&mut runner);

    let card_id = runner.state().objects[&mizzix].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: mizzix,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });

    // "Exile target card that's an instant or sorcery" has no legal target when
    // the only graveyard card is a creature, so the (non-overload) cast is
    // rejected — and if any target-selection prompt did open, the creature must
    // never be among its legal targets.
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let slot = &target_slots[selection.current_slot];
            assert!(
                !slot.legal_targets.contains(&TargetRef::Object(creature)),
                "a creature card is not a legal exile target for Mizzix's Mastery"
            );
        }
        // No legal target: the cast is rejected outright.
        _ => {
            assert!(
                result.is_err(),
                "casting with only a creature card in the graveyard must be rejected \
                 (no legal instant/sorcery target), got {result:?}"
            );
        }
    }
    assert_ne!(
        zone_of(&runner, creature),
        Some(Zone::Exile),
        "a creature card must never be exiled by Mizzix's Mastery"
    );
}

/// Move an already-created battlefield creature object into the graveyard as a
/// plain card, so it is a graveyard card (not a permanent) for targeting tests.
fn move_to_graveyard(runner: &mut GameRunner, id: ObjectId) {
    let state = runner.state_mut();
    let Some(obj) = state.objects.get_mut(&id) else {
        panic!("move_to_graveyard: object {id:?} not found");
    };
    obj.zone = Zone::Graveyard;
    let owner = obj.owner;
    state.battlefield.retain(|&o| o != id);
    if let Some(p) = state.players.iter_mut().find(|p| p.id == owner) {
        if !p.graveyard.contains(&id) {
            p.graveyard.push_back(id);
        }
    }
}

#[test]
fn overload_exiles_and_copies_each_spell_and_mizzix_ends_in_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mizzix = add_mizzix(&mut scenario);
    let inst = add_gain_life_spell(&mut scenario, "Test Bolt", true, 3);
    let sorc = add_gain_life_spell(&mut scenario, "Test Divination", false, 5);
    // A creature card in the graveyard that overload's "each instant or sorcery"
    // must NOT touch.
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    move_to_graveyard(&mut runner, creature);
    with_p0_priority(&mut runner);
    give_red(&mut runner, 8); // {5}{R}{R}{R}
    let life0 = p0_life(&runner);

    let card_id = runner.state().objects[&mizzix].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: mizzix,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Mizzix's Mastery");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice { .. }
        ),
        "overload cost must offer the alternative-cast choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("opt into overload");
    settle(&mut runner);

    // CR 702.96a: "exile EACH instant or sorcery" — both spells are exiled.
    let cards = match runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => cards,
        other => panic!("expected ChooseFromZoneChoice after overload exile, got {other:?}"),
    };
    assert!(
        cards.contains(&inst) && cards.contains(&sorc),
        "both the instant and sorcery must be exiled and offered as copy sources, got {cards:?}"
    );
    assert_eq!(zone_of(&runner, inst), Some(Zone::Exile));
    assert_eq!(zone_of(&runner, sorc), Some(Zone::Exile));
    // The creature card is untouched (overload only replaces "target" with "each").
    assert_eq!(
        zone_of(&runner, creature),
        Some(Zone::Graveyard),
        "a creature card must not be exiled by overloaded Mizzix's Mastery"
    );

    // CR 707.12a: per-copy independent choice — accept one copy, decline the other.
    runner
        .act(GameAction::SelectCards { cards: vec![inst] })
        .expect("cast exactly one of the two copies");
    settle(&mut runner);

    assert_eq!(
        p0_life(&runner),
        life0 + 3,
        "the selected +3 copy, rather than the declined +5 copy, was cast"
    );
    // Step 3b: the self-exile survives the overload transform.
    assert_eq!(
        zone_of(&runner, mizzix),
        Some(Zone::Exile),
        "overloaded Mizzix's Mastery must still exile ITSELF (not fall to the graveyard)"
    );
}

#[test]
fn overload_with_empty_graveyard_resolves_and_self_exiles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mizzix = add_mizzix(&mut scenario);
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);
    give_red(&mut runner, 8);

    let card_id = runner.state().objects[&mizzix].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: mizzix,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Mizzix's Mastery");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::AlternativeCastChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("opt into overload");
    }
    settle(&mut runner);

    // No instant/sorcery to exile — the spell resolves cleanly and self-exiles.
    assert_eq!(
        zone_of(&runner, mizzix),
        Some(Zone::Exile),
        "overloaded Mizzix's Mastery with an empty graveyard still exiles itself"
    );
}
