//! Issue #3246 (+ #3674): Hideaway lands' "play/cast the exiled card" must
//! bind durably to the card THIS source exiled via its ETB Hideaway trigger
//! (`TargetFilter::ExiledBySource`, CR 406.6 + CR 607.2a + CR 702.75a) — not
//! to whatever object happens to occupy the ephemeral `TrackedSet(0)` slot
//! when the activated ability later resolves.
//!
//! Windbrisk Heights: "Hideaway 4 ... {W}, {T}: You may play the exiled card
//! without paying its mana cost if you attacked with three or more creatures
//! this turn." The Hideaway ETB exile and the later activated-ability cast
//! are two SEPARATE resolutions — exactly the cross-resolution linked-ability
//! shape CR 607.1 describes (the second ability refers only to objects
//! affected by the first, linked ability — CR 607.2a). Pre-fix, "play the
//! exiled card" parsed to `CastFromZone { target: ParentTarget }`
//! unconditionally, with no mechanism tying the activated ability back to
//! Windbrisk's OWN Hideaway exile.
//!
//! This test drives the REAL Hideaway ETB trigger (via `GameAction::PlayLand`
//! and `WaitingFor::DigChoice` — no hand-seeded `exile_links`), then
//! simulates an UNRELATED resolution publishing a decoy card (Lightning
//! Bolt) to `TrackedSet(0)`, attacks with three creatures, and activates
//! Windbrisk's second ability for real. Post-fix, `ExiledBySource` correctly
//! resolves via `exile_links` to the hidden Shock regardless of what
//! unrelated activity clobbered `TrackedSet(0)`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Windbrisk Heights, verbatim Oracle text. Built from its text rather than the
/// committed card fixture so the play ability is lowered by the live parser.
const WINDBRISK_HEIGHTS: &str = "Hideaway 4 (When this land enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)
This land enters tapped.
{T}: Add {W}.
{W}, {T}: You may play the exiled card without paying its mana cost if you attacked with three or more creatures this turn.";

/// Add one white mana directly to `P0`'s pool — an external source, distinct
/// from Windbrisk Heights' own `{T}: Add {W}` ability (which cannot be used
/// here: the second ability also requires tapping Windbrisk, and a permanent
/// cannot pay two separate `{T}` costs in the same activation window).
fn fund_white(runner: &mut GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::White, dummy, false, vec![]));
}

/// Drive the real Hideaway 4 ETB trigger fired by playing Windbrisk Heights
/// from hand, answering the `DigChoice` by selecting `hidden`. Returns once
/// the trigger has fully resolved and priority is back at rest.
fn play_windbrisk_and_hide(
    runner: &mut GameRunner,
    windbrisk: ObjectId,
    card_id: engine::types::identifiers::CardId,
    hidden: ObjectId,
) -> bool {
    runner
        .act(GameAction::PlayLand {
            object_id: windbrisk,
            card_id,
        })
        .expect("playing Windbrisk Heights must be legal");

    let mut saw_dig_choice = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                saw_dig_choice = true;
                assert!(
                    cards.contains(&hidden),
                    "the hidden Shock must be among the Hideaway 4 look-at set"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![hidden],
                    })
                    .expect("SelectCards (Hideaway pick) accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && saw_dig_choice {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt while driving Windbrisk's Hideaway ETB: {other:?}"),
        }
    }
    saw_dig_choice
}

/// Drive Windbrisk's second ability once activated: accept the "you may
/// play" optional choice and pass priority until the stack settles. "You may
/// play the exiled card without paying its mana cost" states no duration, so
/// the hidden card is cast AS THE ABILITY RESOLVES (CR 701.18b + CR 608.2g):
/// the hidden Shock's own target prompt opens during the resolution and is
/// answered with P1.
fn drive_windbrisk_second_ability(runner: &mut GameRunner) {
    for step in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional free cast");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("choose Shock's target");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state at step {step}: {other:?}");
            }
            _ => {}
        }
    }
    panic!(
        "Windbrisk second-ability activation did not finish: stack={:?} waiting={:?}",
        runner.state().stack,
        runner.state().waiting_for
    );
}

#[test]
fn windbrisk_heights_plays_correct_hidden_card_across_unrelated_tracked_set_activity() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Windbrisk Heights in hand, ready to play as this turn's land drop.
    let windbrisk = scenario
        .add_land_to_hand(P0, "Windbrisk Heights")
        .from_oracle_text(WINDBRISK_HEIGHTS)
        .id();
    // The library holds exactly one real card — Shock — so the Hideaway 4
    // look-at-top-4 deterministically finds and hides it (CR 701.20e: looking
    // at fewer than N cards when the library has fewer than N is legal).
    let shock = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    // The decoy sits in exile (a prerequisite for `copy_source_from_tracked_set`'s
    // `obj.zone == Zone::Exile` guard) as if some unrelated earlier resolution
    // in the same game had exiled and tracked it — completely unconnected to
    // Windbrisk Heights' own Hideaway exile.
    let bolt_decoy = scenario.add_real_card(P0, "Lightning Bolt", Zone::Exile, db);

    // Three attackers for the "attacked with three or more creatures" gate.
    let a1 = scenario.add_creature(P0, "Attacker One", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Attacker Two", 2, 2).id();
    let a3 = scenario.add_creature(P0, "Attacker Three", 2, 2).id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let windbrisk_card_id = runner.state().objects[&windbrisk].card_id;

    let saw_dig_choice = play_windbrisk_and_hide(&mut runner, windbrisk, windbrisk_card_id, shock);
    assert!(
        saw_dig_choice,
        "reach-guard: Windbrisk's real Hideaway 4 ETB must surface a DigChoice \
         before the discriminator below is meaningful"
    );
    assert_eq!(
        runner.state().objects.get(&shock).map(|o| o.zone),
        Some(Zone::Exile),
        "reach-guard: Hideaway must actually exile the hidden Shock"
    );
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == windbrisk && link.exiled_id == shock),
        "reach-guard: the real Hideaway resolution must link Shock to Windbrisk Heights \
         (CR 406.6 + CR 607.2a) — the discriminator depends on ExiledBySource finding it here"
    );

    // Unrelated activity clobbers the ephemeral chain-local TrackedSet(0)
    // sentinel with a decoy — simulating some other resolution in the same
    // game that has nothing to do with Windbrisk Heights.
    runner
        .state_mut()
        .tracked_object_sets
        .insert(TrackedSetId(0), vec![bolt_decoy]);

    // Attack with three creatures to satisfy Windbrisk's condition.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P1)),
            (a3, AttackTarget::Player(P1)),
        ])
        .expect("declaring three attackers must succeed");

    // Windbrisk entered tapped (and its Hideaway trigger did not tap it
    // further); force it untapped here to model it having already been
    // untapped going into this turn's combat (the land-entering-tapped
    // restriction is orthogonal to the anaphor-binding fix under test).
    runner
        .state_mut()
        .objects
        .get_mut(&windbrisk)
        .unwrap()
        .tapped = false;
    fund_white(&mut runner);
    let p1_life_before = runner.state().players[1].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: windbrisk,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Windbrisk's second ability must be legal after attacking with three creatures");

    drive_windbrisk_second_ability(&mut runner);

    // DISCRIMINATOR: the instruction played Windbrisk's OWN hidden Shock (via
    // `ExiledBySource` → `exile_links`), never the unrelated Lightning Bolt
    // decoy sitting in `TrackedSet(0)`. Shock was cast during the resolution
    // and dealt 2 to P1; the decoy was neither cast nor granted anything.
    assert_eq!(
        runner.state().players[1].life,
        p1_life_before - 2,
        "the hidden Shock was cast during the resolution and hit P1"
    );
    assert_eq!(
        runner.state().objects.get(&shock).map(|o| o.zone),
        Some(Zone::Graveyard),
        "the cast Shock resolved into its owner's graveyard"
    );
    assert!(
        runner.state().objects[&shock]
            .casting_permissions
            .is_empty(),
        "no permission outlives the resolution, got {:?}",
        runner.state().objects[&shock].casting_permissions
    );
    assert!(
        runner.state().objects[&bolt_decoy]
            .casting_permissions
            .is_empty(),
        "the unrelated TrackedSet(0) decoy must NOT receive a play permission, got {:?}",
        runner.state().objects[&bolt_decoy].casting_permissions
    );
    assert_eq!(
        runner.state().objects.get(&bolt_decoy).map(|o| o.zone),
        Some(Zone::Exile),
        "the unrelated decoy must be untouched by Windbrisk's activation"
    );
}

/// CR 603.4 + CR 607.2a + CR 608.2g + CR 702.75a: Evercoat Ursine's
/// combat-damage trigger must offer exactly one card from its own Hideaway
/// exile links. The exact printed Oracle text is parsed through the production
/// scenario pipeline; no ability AST is constructed by this test.
#[test]
fn evercoat_ursine_plays_one_linked_hideaway_card_after_combat_damage() {
    let Some(db) = load_db() else {
        return;
    };
    const EVERCOAT_URSINE: &str = "Trample
Hideaway 3, hideaway 3 (When this creature enters, look at the top three cards of your library, exile one face down, then put the rest on the bottom in a random order. Then do it again.)
Whenever this creature deals combat damage to a player, if there are cards exiled with it, you may play one of them without paying its mana cost.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ursine = scenario
        .add_creature_to_hand(P0, "Evercoat Ursine", 4, 4)
        .from_oracle_text(EVERCOAT_URSINE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let _hidden_a = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    let _hidden_b = scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    for _ in 0..4 {
        scenario.add_real_card(P0, "Forest", Zone::Library, db);
        scenario.add_real_card(P1, "Forest", Zone::Library, db);
    }
    let decoy = scenario.add_real_card(P0, "Shock", Zone::Exile, db);

    let mut runner = scenario.build();
    runner.cast(ursine).commit();

    // Drive both real Hideaway ETB triggers. The selected cards are recorded
    // from the production DigChoice prompts, so the test remains independent
    // of the library ordering chosen by the scenario builder.
    let mut hidden = Vec::new();
    let mut hideaway_setup_complete = false;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order the Hideaway triggers");
            }
            WaitingFor::DigChoice { cards, .. } => {
                let selected = cards[0];
                hidden.push(selected);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![selected],
                    })
                    .expect("Hideaway selection accepted");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && hidden.len() == 2 => {
                hideaway_setup_complete = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass while resolving Hideaway");
            }
            other => panic!("unexpected Hideaway state: {other:?}"),
        }
    }
    assert_eq!(
        hidden.len(),
        2,
        "both Hideaway instances must exile one card each"
    );
    assert!(
        hideaway_setup_complete,
        "both Hideaway instances must finish before combat setup"
    );
    let linked_ids: Vec<_> = runner
        .state()
        .exile_links
        .iter()
        .filter(|link| link.source_id == ursine)
        .map(|link| link.exiled_id)
        .collect();
    assert_eq!(
        linked_ids.len(),
        hidden.len(),
        "both Hideaway selections must be source-linked"
    );
    assert!(
        hidden.iter().all(|id| linked_ids.contains(id))
            && linked_ids.iter().all(|id| hidden.contains(id)),
        "the source-linked exile ledger must contain exactly both Hideaway selections"
    );

    let life_before = runner.state().players[P1.0 as usize].life;
    runner.advance_to_end_step();
    runner.advance_to_combat();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    runner
        .declare_attackers(&[(ursine, AttackTarget::Player(P1))])
        .expect("Evercoat Ursine attacks P1");

    let mut offered = false;
    let mut selected = None;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("P1 declares no blockers");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Evercoat Ursine's optional cast");
            }
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { candidates, .. },
                ..
            } => {
                offered = true;
                assert!(
                    candidates
                        .iter()
                        .all(|candidate| hidden.contains(candidate)),
                    "the linked-exile window must not offer unrelated cards: {candidates:?}"
                );
                assert_eq!(
                    candidates.len(),
                    hidden.len(),
                    "the linked-exile window must offer every linked Hideaway card"
                );
                assert!(
                    hidden.iter().all(|id| candidates.contains(id)),
                    "the linked-exile window must not omit a linked Hideaway card"
                );
                assert!(!candidates.contains(&decoy));
                let choice = hidden
                    .iter()
                    .copied()
                    .find(|candidate| candidates.contains(candidate))
                    .expect("a linked Hideaway card must be offered");
                selected = Some(choice);
                runner
                    .act(GameAction::FreeCastWindowChoice {
                        selection: Some(choice),
                    })
                    .expect("choose one linked Hideaway card");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("choose Shock or Lightning Bolt's target");
            }
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty()
                    && matches!(
                        runner.state().phase,
                        Phase::EndCombat | Phase::PostCombatMain
                    ) =>
            {
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass after combat damage");
            }
            other => panic!("unexpected Evercoat Ursine state: {other:?}"),
        }
    }

    let selected = selected.expect("the cast window must select a linked card");
    assert!(
        offered,
        "reach-guard: combat damage must reach the linked cast window"
    );
    assert!(
        runner.state().players[P1.0 as usize].life < life_before,
        "Evercoat Ursine must deal combat damage before its trigger resolves"
    );
    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Exile);
    for hidden_id in hidden {
        if hidden_id != selected {
            assert_eq!(runner.state().objects[&hidden_id].zone, Zone::Exile);
        }
    }
}
