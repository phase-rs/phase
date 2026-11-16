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
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::{ManaType, ManaUnit};
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
