//! Issue #6435: Mosswort Bridge hideaway "play the exiled card" must play the
//! face-down hidden card (CR 406.6 / 607.1 / 607.2a / 702.75a), both as:
//! - a hidden spell cast without paying its mana cost
//! - a hidden land played as a land drop
//!
//! "You may play the exiled card without paying its mana cost" states no
//! duration, so the card is played AS THE ABILITY RESOLVES (CR 701.18b +
//! CR 608.2g + CR 305.2a): the spell is cast during the resolution and the land
//! enters during it, with no separate `CastSpell` / `PlayLand` action and no
//! permission left behind.

use std::collections::BTreeSet;

use engine::game::players::linked_exile_cards_for_source;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CastingPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, ExileLink, ExileLinkKind, GameState, LookGrant, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Mosswort Bridge, verbatim Oracle text. Built from its text rather than the
/// committed card fixture so the play ability is lowered by the live parser.
const MOSSWORT_BRIDGE: &str = "Hideaway 4 (When this land enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)
This land enters tapped.
{T}: Add {G}.
{G}, {T}: You may play the exiled card without paying its mana cost if creatures you control have total power 10 or greater.";

fn fund_green(runner: &mut GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::Green, dummy, false, vec![]));
}

fn play_mosswort_and_hide(runner: &mut GameRunner, mosswort: ObjectId, hidden: ObjectId) {
    let mosswort_card_id = runner.state().objects[&mosswort].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: mosswort,
            card_id: mosswort_card_id,
        })
        .expect("playing Mosswort Bridge as land must be legal");

    let mut saw_dig_choice = false;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                saw_dig_choice = true;
                assert!(
                    cards.contains(&hidden),
                    "reach-guard: the hidden card must be offered by the Hideaway pick"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![hidden],
                    })
                    .expect("selecting the Hideaway card must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && saw_dig_choice {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state while driving Hideaway ETB: {other:?}"),
        }
    }
    assert!(saw_dig_choice, "Hideaway must surface a DigChoice prompt");
}

fn activate_mosswort_second_ability(
    runner: &mut GameRunner,
    mosswort: ObjectId,
    accept: bool,
) -> bool {
    let activate = runner.act(GameAction::ActivateAbility {
        source_id: mosswort,
        ability_index: 1,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    });
    if let Err(e) = &activate {
        panic!(
            "Mosswort Bridge second ability must be activatable: {:?}. phase={:?} waiting_for={:?} active={:?} priority_player={:?}",
            e,
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().active_player,
            runner.state().priority_player
        );
    }
    activate.expect("Mosswort Bridge second ability must be activatable");

    let mut saw_optional = false;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("deciding the optional offer must succeed");
            }
            // The hidden Shock, cast during the resolution, chooses its target.
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("choose Shock's target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => {
                panic!("unexpected waiting state while driving Mosswort activation: {other:?}")
            }
        }
    }
    saw_optional
}

fn assert_no_permissions(runner: &GameRunner, id: ObjectId) {
    assert!(
        runner.state().objects[&id].casting_permissions.is_empty(),
        "no permission outlives the resolution, got {:?}",
        runner.state().objects[&id].casting_permissions
    );
}

fn hidden_is_face_down_exiled_linked_to_source(
    runner: &GameRunner,
    hidden: ObjectId,
    mosswort: ObjectId,
) {
    let obj = runner.state().objects.get(&hidden).unwrap();
    assert_eq!(obj.zone, Zone::Exile, "hidden card must be in exile");
    assert!(obj.face_down, "hidden card must be face down (Hideaway)");
    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == hidden
                && link.source_id == mosswort
                && matches!(link.kind, ExileLinkKind::HideawayLookable { .. })
        }),
        "hidden card must be linked to Mosswort Bridge via HideawayLookable"
    );
}

#[test]
fn mosswort_bridge_hideaway_free_casts_hidden_spell_when_total_power_ge_10() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mosswort = scenario
        .add_land_to_hand(P0, "Mosswort Bridge")
        .from_oracle_text(MOSSWORT_BRIDGE)
        .id();
    let hidden_spell = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    // Hideaway 4 looks at the top 4 cards; seed enough so P0 doesn't lose
    // from an empty library after the hideaway exile removes the chosen card.
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    // Prevent the game from ending on P1's next draw step due to an empty library.
    scenario.add_real_card(P1, "Island", Zone::Library, db);

    // Total power >= 10 gate.
    scenario.add_creature(P0, "Power 10", 10, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Turn 1: play Mosswort; resolve Hideaway ETB to exile the Shock.
    play_mosswort_and_hide(&mut runner, mosswort, hidden_spell);
    hidden_is_face_down_exiled_linked_to_source(&runner, hidden_spell, mosswort);

    // Advance to the next turn so Mosswort is untapped.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        // There are no Hideaway-related requirements for combat in this test;
        // declare no attackers so we can reach the next untap/main window.
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("declare no attackers");
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    if runner.state().priority_player != P0 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers");
            runner.advance_to_phase(Phase::PreCombatMain);
        }
    }

    fund_green(&mut runner);
    let life_before = runner.state().players[1].life;

    // Activate, accept the optional "you may play" offer: the hidden Shock is
    // cast during the resolution (CR 608.2g) — no separate `CastSpell`.
    let saw_optional = activate_mosswort_second_ability(&mut runner, mosswort, true);
    assert!(saw_optional, "power>=10 must surface the optional offer");

    assert_eq!(
        runner.state().players[1].life,
        life_before - 2,
        "Shock must resolve and deal 2 damage to the targeted player"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "only {{G}} was spent (on the activation); the hidden spell was free"
    );
    assert_eq!(
        runner.state().objects[&hidden_spell].zone,
        Zone::Graveyard,
        "the cast Shock resolved into its owner's graveyard"
    );
    assert_no_permissions(&runner, hidden_spell);
}

#[test]
fn mosswort_bridge_hideaway_free_plays_hidden_land_when_total_power_ge_10() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mosswort = scenario
        .add_land_to_hand(P0, "Mosswort Bridge")
        .from_oracle_text(MOSSWORT_BRIDGE)
        .id();
    let hidden_land = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    scenario.add_real_card(P0, "Swamp", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    scenario.add_creature(P0, "Power 10", 10, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    play_mosswort_and_hide(&mut runner, mosswort, hidden_land);
    hidden_is_face_down_exiled_linked_to_source(&runner, hidden_land, mosswort);

    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("declare no attackers");
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    if runner.state().priority_player != P0 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers");
            runner.advance_to_phase(Phase::PreCombatMain);
        }
    }

    fund_green(&mut runner);
    assert!(
        runner.state().objects[&hidden_land].face_down,
        "reach-guard: the hidden land is still face down before playing it"
    );
    let lands_before = runner.state().lands_played_this_turn;

    // CR 305.2a: the hidden land is played during the resolution — no
    // separate `PlayLand` action.
    let saw_optional = activate_mosswort_second_ability(&mut runner, mosswort, true);
    assert!(saw_optional, "power>=10 must surface the optional offer");

    assert_eq!(
        runner.state().objects[&hidden_land].zone,
        Zone::Battlefield,
        "playing the hidden land must move it to the battlefield"
    );
    assert!(
        !runner.state().objects[&hidden_land].face_down,
        "playing a land must turn it face up on entry"
    );
    assert_eq!(
        runner.state().lands_played_this_turn,
        lands_before + 1,
        "playing the hidden land consumes exactly one land play action"
    );
    assert_no_permissions(&runner, hidden_land);
}

#[test]
fn mosswort_bridge_hideaway_with_power_lt_10_offers_nothing() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mosswort = scenario
        .add_land_to_hand(P0, "Mosswort Bridge")
        .from_oracle_text(MOSSWORT_BRIDGE)
        .id();
    let hidden_spell = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Mountain", Zone::Library, db);

    // Total power < 10.
    scenario.add_creature(P0, "Power 9", 9, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    play_mosswort_and_hide(&mut runner, mosswort, hidden_spell);
    hidden_is_face_down_exiled_linked_to_source(&runner, hidden_spell, mosswort);

    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("declare no attackers");
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    if runner.state().priority_player != P0 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers");
            runner.advance_to_phase(Phase::PreCombatMain);
        }
    }

    fund_green(&mut runner);
    let saw_optional = activate_mosswort_second_ability(&mut runner, mosswort, true);
    assert!(
        !saw_optional,
        "power<10 must not surface the optional \"you may play\" offer"
    );

    let obj = runner.state().objects.get(&hidden_spell).unwrap();
    assert!(
        obj.casting_permissions
            .iter()
            .all(|p| !matches!(p, CastingPermission::ExileWithAltCost { .. })),
        "power<10 must not grant ExileWithAltCost to the hidden spell"
    );
    assert!(
        obj.casting_permissions
            .iter()
            .all(|p| !matches!(p, CastingPermission::PlayFromExile { .. })),
        "power<10 must not grant PlayFromExile to the hidden spell"
    );
}

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, \
     then return those cards to the battlefield under your control.";

/// CR 702.75a + CR 113.7a + CR 607.2a + CR 400.7: Bridge is played and its
/// hideaway trigger waits; Ghostly Flicker returns Bridge, whose new hideaway
/// trigger resolves first and exiles N. The old trigger then exiles O for the
/// Bridge that left. The returned Bridge's "the exiled card" is N alone.
#[test]
fn old_hideaway_trigger_after_a_blink_does_not_feed_the_new_bridge() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mosswort = scenario.add_real_card(P0, "Mosswort Bridge", Zone::Hand, db);
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    for name in [
        "Forest", "Island", "Mountain", "Plains", "Swamp", "Wastes", "Forest", "Island",
    ] {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let mosswort_card_id = runner.state().objects[&mosswort].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: mosswort,
            card_id: mosswort_card_id,
        })
        .expect("play Mosswort Bridge");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the hideaway trigger waits on the stack"
    );
    let incarnation_played = runner.state().objects[&mosswort].incarnation;

    let flicker_card_id = runner.state().objects[&flicker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flicker,
            card_id: flicker_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Ghostly Flicker");
    for target in [mosswort, land] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("target Bridge and the land");
    }

    // Each hideaway pick takes the first card it is offered: the re-entry
    // trigger resolves first (N), the old trigger second (O).
    let mut picks = Vec::new();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DigChoice { cards, .. } => {
                picks.push(cards[0]);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("hide the first offered card");
            }
            other => panic!("unexpected waiting state while resolving the blink: {other:?}"),
        }
    }

    let state = runner.state();
    assert_eq!(picks.len(), 2, "both hideaway triggers resolved");
    for pick in &picks {
        assert_eq!(
            state.objects[pick].zone,
            Zone::Exile,
            "each pick was hidden"
        );
    }
    assert_eq!(
        state.objects[&mosswort].incarnation,
        incarnation_played + 2,
        "Bridge left and returned as a new object"
    );
    let linked: Vec<ObjectId> = linked_exile_cards_for_source(state, mosswort)
        .iter()
        .map(|snapshot| snapshot.exiled_id)
        .collect();
    assert_eq!(
        linked,
        vec![picks[0]],
        "only the returned Bridge's own hideaway card is exiled with it"
    );
}

/// A Mosswort Bridge whose "{G}, {T}: You may play the exiled card ..."
/// activation waits while Bridge is blinked.
struct BlinkedBridge {
    runner: GameRunner,
    mosswort: ObjectId,
    /// The Shock the activating Bridge hid.
    hidden: ObjectId,
    /// The card the returned Bridge's hideaway hid, when its library had one.
    rehidden: Option<ObjectId>,
}

/// Pass priority until `done` holds, hiding the first card each hideaway
/// offers and accepting every optional play.
fn resolve_until(runner: &mut GameRunner, done: impl Fn(&GameState) -> bool) {
    for _ in 0..64 {
        if done(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DigChoice { cards, .. } => GameAction::SelectCards {
                cards: vec![cards[0]],
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("prompt answer accepted");
    }
    panic!("prompt loop did not settle");
}

fn priority_with_stack(len: usize) -> impl Fn(&GameState) -> bool {
    move |state| {
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.len() == len
    }
}

/// Mosswort Bridge on the battlefield hiding a Shock face down (the look link
/// its hideaway makes), a 10-power creature and {G} in pool. Activate Bridge's
/// second ability and, with it waiting, blink Bridge and a land with Ghostly
/// Flicker. With `rehide`, P0's library holds one Lightning Bolt for the
/// returned Bridge's hideaway; otherwise it is empty. Returns with the
/// activation still waiting.
fn blink_bridge_under_its_activation(rehide: bool) -> Option<BlinkedBridge> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mosswort = scenario.add_real_card(P0, "Mosswort Bridge", Zone::Battlefield, db);
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let hidden = scenario.add_real_card(P0, "Shock", Zone::Exile, db);
    let rehidden = rehide.then(|| scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db));
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    scenario.add_creature(P0, "Power 10", 10, 1);
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let incarnation_activated = runner.state().objects[&mosswort].incarnation;
    let state = runner.state_mut();
    // Bridge has been on the battlefield since an earlier turn: untapped.
    state.objects.get_mut(&mosswort).unwrap().tapped = false;
    state.objects.get_mut(&hidden).unwrap().face_down = true;
    state.exile_links.push(ExileLink {
        exiled_id: hidden,
        source_id: mosswort,
        kind: ExileLinkKind::HideawayLookable {
            grant: LookGrant::SourceController,
            lookers: BTreeSet::from([P0]),
            source_incarnation: incarnation_activated,
        },
    });
    fund_green(&mut runner);

    runner
        .act(GameAction::ActivateAbility {
            source_id: mosswort,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Mosswort Bridge's second ability");
    assert!(
        priority_with_stack(1)(runner.state()),
        "the activation waits on the stack; waiting_for = {}",
        runner.waiting_for_kind()
    );

    runner
        .cast(flicker)
        .target_objects(&[mosswort, land])
        .commit();
    resolve_until(&mut runner, priority_with_stack(1));
    let state = runner.state();
    assert_eq!(
        state.objects[&flicker].zone,
        Zone::Graveyard,
        "the flicker resolved"
    );
    assert_ne!(
        state.objects[&mosswort].incarnation, incarnation_activated,
        "Bridge came back as a new object"
    );
    Some(BlinkedBridge {
        runner,
        mosswort,
        hidden,
        rehidden,
    })
}

fn has_free_play(state: &GameState, card: ObjectId) -> bool {
    state.objects[&card]
        .casting_permissions
        .iter()
        .any(|permission| {
            matches!(
                permission,
                CastingPermission::ExileWithAltCost { cost, .. } if cost.is_without_paying_mana()
            )
        })
}

/// CR 113.7a + CR 607.2a + CR 400.7: Bridge's activation waits while Bridge is
/// blinked, and the returned Bridge's hideaway hides a new card. The activation
/// is the activating Bridge's, so "the exiled card" is the card that Bridge
/// hid: it plays that card free and not the new one.
#[test]
fn old_bridge_activation_plays_its_own_card_not_the_new_one() {
    let Some(mut blink) = blink_bridge_under_its_activation(true) else {
        return;
    };
    let rehidden = blink.rehidden.expect("a card for the returned Bridge");
    let state = blink.runner.state();
    let live = state.objects[&blink.mosswort].incarnation;
    assert_eq!(
        state.objects[&rehidden].zone,
        Zone::Exile,
        "the returned Bridge's hideaway hid the new card"
    );
    assert!(
        state
            .exile_links
            .iter()
            .any(|link| link.exiled_id == rehidden
                && link.source_id == blink.mosswort
                && matches!(
                    link.kind,
                    ExileLinkKind::HideawayLookable { source_incarnation, .. }
                        if source_incarnation == live
                )),
        "the new card is the returned Bridge's look link"
    );

    resolve_until(&mut blink.runner, priority_with_stack(0));
    let state = blink.runner.state();
    assert!(
        has_free_play(state, blink.hidden),
        "the activation plays the card the activating Bridge hid"
    );
    assert!(
        !has_free_play(state, rehidden),
        "the returned Bridge's card is not the activating Bridge's exiled card"
    );
}

/// CR 113.7a + CR 607.2a: as above, but the returned Bridge's hideaway finds
/// nothing to hide. The activation still plays the card its Bridge hid.
#[test]
fn old_bridge_activation_after_an_empty_rehide_still_plays_its_card() {
    let Some(mut blink) = blink_bridge_under_its_activation(false) else {
        return;
    };
    let state = blink.runner.state();
    let live = state.objects[&blink.mosswort].incarnation;
    assert!(
        state
            .exile_links
            .iter()
            .filter(|link| link.source_id == blink.mosswort)
            .all(|link| link.exiled_id == blink.hidden
                && matches!(
                    link.kind,
                    ExileLinkKind::HideawayLookable { source_incarnation, .. }
                        if source_incarnation != live
                )),
        "the returned Bridge hid nothing; only the old look link remains"
    );

    resolve_until(&mut blink.runner, priority_with_stack(0));
    assert!(
        has_free_play(blink.runner.state(), blink.hidden),
        "the activation plays the card the activating Bridge hid"
    );
}
