//! Birgi, God of Storytelling // Harnfel, Horn of Bounty (KHM #123) — integration
//! coverage for the three headline behaviors, each driven through the REAL
//! pipeline (cast / activation / turn machinery), not parser-shape assertions.
//!
//! Front — Birgi, God of Storytelling {2}{R} Legendary Creature — God 3/3.
//! Ability 1: "Whenever you cast a spell, add {R}. Until end of turn, you don't
//! lose this mana as steps and phases end." Ability 2: "Creatures you control
//! can boast twice during each of your turns rather than once."
//!
//! Back — Harnfel, Horn of Bounty {4}{R} Legendary Artifact. "Discard a card:
//! Exile the top two cards of your library. You may play those cards this turn."
//!
//! The card is ~fully implemented already; these tests are the regression net.
//! CR 605.1b (mana trigger uses the stack, not a mana ability), CR 614.17 +
//! CR 611.2a / CR 106.4 (the until-end-of-turn "don't lose this mana" can't-effect
//! keeps the {R} through steps/phases, then it drains at cleanup once that
//! duration ends), CR 702.142a + CR 602.5 (boast-twice activation limit,
//! controller-scoped), CR 611.2a play-from-exile permission duration.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityTag, CastingPermission, Duration};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaExpiry, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const BIRGI_FRONT: &str = "Whenever you cast a spell, add {R}. Until end of turn, you don't lose \
this mana as steps and phases end.\nCreatures you control can boast twice during each of your \
turns rather than once.";

const HARNFEL_BACK: &str =
    "Discard a card: Exile the top two cards of your library. You may play those cards this turn.";

const BOAST_ABILITY: &str = "Boast \u{2014} {1}: You gain 1 life.";

fn pidx(p: PlayerId) -> usize {
    p.0 as usize
}

/// Count of red mana units in `player`'s pool.
fn red_count(state: &GameState, player: PlayerId) -> usize {
    state.players[pidx(player)]
        .mana_pool
        .mana
        .iter()
        .filter(|u| u.color == ManaType::Red)
        .count()
}

/// The expiry stamped on the (single) red unit in `player`'s pool, if any.
fn red_expiry(state: &GameState, player: PlayerId) -> Option<ManaExpiry> {
    state.players[pidx(player)]
        .mana_pool
        .mana
        .iter()
        .find(|u| u.color == ManaType::Red)
        .and_then(|u| u.expiry)
}

/// True when a triggered ability sourced from `source` sits on the stack — the
/// observable that a "whenever you cast a spell" trigger used the stack rather
/// than resolving inline as a mana ability (CR 605.1b).
fn stack_has_trigger_from(state: &GameState, source: ObjectId) -> bool {
    state
        .stack
        .iter()
        .any(|e| e.source_id == source && matches!(e.kind, StackEntryKind::TriggeredAbility { .. }))
}

fn red_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(9_999), false, vec![]); count]
}

/// Index of the boast-tagged activated ability on `obj`.
fn boast_index(runner: &GameRunner, obj: ObjectId) -> usize {
    runner.state().objects[&obj]
        .abilities
        .iter()
        .position(|a| a.ability_tag == Some(AbilityTag::Boast))
        .expect("boast creature must carry a Boast-tagged activated ability")
}

/// Pump one turn-structure step: auto-declare no attackers/blockers, drain a
/// single trigger order, no-op a cleanup discard, or pass priority. Returns
/// false when the current wait isn't one of these (i.e. progress stalled).
fn pump(runner: &mut GameRunner) -> bool {
    match runner.state().waiting_for.clone() {
        WaitingFor::DeclareAttackers { .. } => runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_ok(),
        WaitingFor::DeclareBlockers { .. } => runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .is_ok(),
        WaitingFor::OrderTriggers { .. } => {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            true
        }
        WaitingFor::DiscardChoice { .. } => runner
            .act(GameAction::SelectCards { cards: vec![] })
            .is_ok(),
        WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority).is_ok(),
        _ => false,
    }
}

fn advance_until(runner: &mut GameRunner, pred: impl Fn(&GameState) -> bool) {
    for _ in 0..400 {
        if pred(runner.state()) {
            return;
        }
        if !pump(runner) {
            break;
        }
    }
    assert!(
        pred(runner.state()),
        "advance_until stalled at turn {} phase {:?}",
        runner.state().turn_number,
        runner.state().phase
    );
}

fn cheap_spell(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_spell_to_hand(player, name, true)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Add a castable 0-cost sorcery ("You gain 1 life") to the top of `player`'s
/// library, returning its id. `add_spell_to_library_top` reseats to `library[0]`
/// exactly like `add_card_to_library_top` (both `insert(0, ..)`), so exile-top
/// ordering stays deterministic — but unlike a bare named card this one is a
/// real spell the play-from-exile permission can actually consume, which is what
/// lets the Harnfel tests drive the grant's CONSUMER, not just inspect it.
fn castable_top(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_spell_to_library_top(player, name, false)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Shared consumer + expiry drill for Harnfel's play-from-exile grant. Casts
/// `to_cast` from exile THIS turn through the real cast pipeline (asserting via
/// `CastOutcome` deltas that it resolves — gains 1 life, moves to the graveyard —
/// and leaves P0's cast path), then advances one turn and asserts the still
/// `unplayed` sibling is no longer castable and its grant was pruned. This
/// exercises the permission being spent and expiring, not merely produced.
///
/// CR 611.2a: "you may play those cards this turn" is a continuous effect that
/// lasts until end of turn; CR 514.2: the grant is pruned at that turn's cleanup.
fn assert_play_from_exile_consumed_then_expires(
    runner: &mut GameRunner,
    to_cast: ObjectId,
    unplayed: ObjectId,
) {
    // (a) CONSUMER — the grant surfaces the card on P0's cast path, and casting
    // it actually resolves and consumes that path.
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&to_cast),
        "the play-from-exile grant must surface the exiled card on P0's cast path"
    );
    let cast = runner.cast(to_cast).resolve();
    cast.assert_zone(&[to_cast], Zone::Graveyard);
    cast.assert_life_delta(P0, 1);
    runner.advance_until_stack_empty();
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&to_cast),
        "casting the granted card must consume its exile-cast path"
    );

    // (b) EXPIRY — the unplayed sibling is castable now, but not after the turn
    // ends. CR 611.2a duration ends at cleanup; CR 514.2 prunes the grant.
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&unplayed),
        "the still-unplayed exiled sibling must be castable before the turn ends"
    );
    let this_turn = runner.state().turn_number;
    advance_until(runner, |s| s.turn_number > this_turn);
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&unplayed),
        "CR 611.2a: the unplayed exiled card must no longer be castable once the turn changes"
    );
    assert!(
        !runner.state().objects[&unplayed]
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0)),
        "CR 514.2 + CR 611.2a: the expired PlayFromExile grant must be pruned at end of turn"
    );
}

// ---------------------------------------------------------------------------
// Test 1 — mana trigger: stack (not inline), {R} with EndOfTurn expiry,
// persistence through steps/phases, drain at cleanup.
// ---------------------------------------------------------------------------

/// CR 605.1b: Birgi's "whenever you cast a spell, add {R}" is a TRIGGERED
/// ability, not a mana ability — it uses the stack. A mana ability would add the
/// {R} inline during casting; assert the pool has no red at commit (negative)
/// while a Birgi triggered ability sits on the stack (positive reach-guard).
/// After it resolves the pool holds exactly one {R} carrying
/// `ManaExpiry::EndOfTurn` — the CR 614.17 "don't lose this mana" can't-effect
/// bounded by the CR 611.2a until-end-of-turn duration (a misparse that drops
/// the expiry fails the `red_expiry` assertion).
#[test]
fn birgi_mana_trigger_uses_stack_and_stamps_end_of_turn_expiry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let birgi = scenario
        .add_creature_from_oracle(P0, "Birgi, God of Storytelling", 3, 3, BIRGI_FRONT)
        .id();
    let spell = cheap_spell(&mut scenario, P0, "Trigger Spell");

    let mut runner = scenario.build();

    // Commit: the spell + Birgi's trigger are on the stack, nothing resolved.
    let commit = runner.cast(spell).commit();
    assert_eq!(
        red_count(commit.state(), P0),
        0,
        "CR 605.1b: the mana trigger must NOT resolve inline during casting"
    );
    assert!(
        stack_has_trigger_from(commit.state(), birgi),
        "CR 605.1b: Birgi's cast trigger must be placed on the stack"
    );
    let _ = commit.resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        red_count(runner.state(), P0),
        1,
        "one cast → exactly one {{R}} added when the trigger resolves"
    );
    assert_eq!(
        red_expiry(runner.state(), P0),
        Some(ManaExpiry::EndOfTurn),
        "CR 614.17 + CR 611.2a: the added {{R}} must carry the until-end-of-turn 'don't lose this mana' expiry"
    );

    let cast_turn = runner.state().turn_number;

    // Persistence: cross combat into the end step of the same turn — the {R}
    // survives every step/phase drain (CR 614.17 overriding CR 106.4).
    advance_until(&mut runner, |s| s.phase == Phase::End);
    assert_eq!(
        red_count(runner.state(), P0),
        1,
        "CR 614.17: the {{R}} persists through steps/phases within the turn"
    );

    // Drain: crossing this turn's cleanup empties it. CR 611.2a: the
    // until-end-of-turn "don't lose this mana" continuous effect's duration ends
    // at cleanup, so CR 106.4's default step/phase mana-emptying is restored.
    advance_until(&mut runner, |s| s.turn_number > cast_turn);
    assert_eq!(
        red_count(runner.state(), P0),
        0,
        "CR 611.2a + CR 106.4: the {{R}} drains at cleanup once the until-end-of-turn duration ends"
    );
}

/// Two casts → two independent triggers → two {R} in the pool.
#[test]
fn birgi_two_casts_add_two_red() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Birgi, God of Storytelling", 3, 3, BIRGI_FRONT);
    let a = cheap_spell(&mut scenario, P0, "Spell A");
    let b = cheap_spell(&mut scenario, P0, "Spell B");

    let mut runner = scenario.build();
    runner.cast(a).resolve();
    runner.advance_until_stack_empty();
    runner.cast(b).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        red_count(runner.state(), P0),
        2,
        "two spell casts must add {{R}}{{R}} (two independent Birgi triggers)"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — boast twice with Birgi; once without (CR 702.142a + CR 602.5).
// ---------------------------------------------------------------------------

/// With Birgi out, a boast creature you control may boast TWICE per turn; the
/// third activation is rejected on the limit. CR 702.142a defines boast's base
/// "Activate ... only once each turn" restriction (an activation restriction,
/// CR 602.5); Birgi's static raises that limit to twice, so a third activation
/// exceeds even the raised limit. Mana is available for all three attempts, so a
/// rejected third proves the LIMIT — not payability — blocked it.
#[test]
fn boast_twice_with_birgi_rejects_third() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Birgi, God of Storytelling", 3, 3, BIRGI_FRONT);
    let boaster = scenario
        .add_creature_from_oracle(P0, "Boaster", 2, 2, BOAST_ABILITY)
        .id();
    scenario.with_mana_pool(P0, red_pool(5));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .creatures_attacked_this_turn
        .insert(boaster);
    let idx = boast_index(&runner, boaster);

    runner.activate(boaster, idx).resolve();
    runner.advance_until_stack_empty();
    runner.activate(boaster, idx).resolve();
    runner.advance_until_stack_empty();

    let third = runner.act(GameAction::ActivateAbility {
        source_id: boaster,
        ability_index: idx,
    });
    assert!(
        third.is_err(),
        "CR 702.142a + CR 602.5: with Birgi the boast limit is raised to twice — the third activation exceeds it and must be rejected"
    );
}

/// Regression leg WITHOUT Birgi: the base CR 702.142a limit is once per turn, so
/// the SECOND boast activation is rejected. This is the discriminator that the
/// prior test's second success came from Birgi's static, not a blanket boast fix.
#[test]
fn boast_once_without_birgi_rejects_second() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let boaster = scenario
        .add_creature_from_oracle(P0, "Boaster", 2, 2, BOAST_ABILITY)
        .id();
    scenario.with_mana_pool(P0, red_pool(5));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .creatures_attacked_this_turn
        .insert(boaster);
    let idx = boast_index(&runner, boaster);

    runner.activate(boaster, idx).resolve();
    runner.advance_until_stack_empty();

    let second = runner.act(GameAction::ActivateAbility {
        source_id: boaster,
        ability_index: idx,
    });
    assert!(
        second.is_err(),
        "CR 702.142a: without Birgi the base boast limit is once — the second activation is rejected"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — Harnfel back-face ability: discard → exile top two → play-this-turn.
// ---------------------------------------------------------------------------

/// Harnfel's activated ability exiles the top two library cards and grants their
/// controller a `PlayFromExile` permission for the rest of the turn (CR 611.2a —
/// a continuous effect from resolution lasting "until end of turn").
/// A third, already-exiled card that was NOT exiled by this ability carries no
/// grant, proving the permission is scoped to the exiled set, not the exile zone.
///
/// Drives the real activation pipeline (discard cost + exile + grant). The MDFC
/// back-face cast wrapper (`ChooseModalFace`) is exercised by the dedicated MDFC
/// tests (peter_parker_modal_back_face_cast / issue_1985 / issue_2377); here the
/// artifact is placed directly so the ability behavior is the unit under test.
#[test]
fn harnfel_exiles_top_two_and_grants_play_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let harnfel = scenario
        .add_creature(P0, "Harnfel, Horn of Bounty", 0, 0)
        .as_artifact()
        .from_oracle_text(HARNFEL_BACK)
        .id();
    // Cost fodder to discard.
    let discard = scenario.add_card_to_hand(P0, "Discard Fodder");
    // Buried card added FIRST so it sinks to the bottom; the two exiled cards go
    // on top afterward (each `add_card_to_library_top` pushes onto library[0]).
    let buried = scenario.add_card_to_library_top(P0, "Buried");
    let top_two: Vec<ObjectId> = ["Top B", "Top A"]
        .into_iter()
        .map(|n| castable_top(&mut scenario, P0, n))
        .collect();
    // A control card ALREADY in exile from an unrelated source (no grant).
    let control = scenario.add_creature_to_exile(P0, "Prior Exile", 1, 1).id();

    let mut runner = scenario.build();
    let idx = runner.state().objects[&harnfel]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Harnfel must carry the discard-cost activated ability");

    runner
        .activate(harnfel, idx)
        .pay_with(&[discard])
        .accept_optional()
        .resolve();
    runner.advance_until_stack_empty();

    // Both top cards are exiled with a P0 PlayFromExile grant that ends this turn.
    for id in &top_two {
        let obj = &runner.state().objects[id];
        assert_eq!(obj.zone, Zone::Exile, "the top two cards must be exiled");
        let grant = obj
            .casting_permissions
            .iter()
            .find(|p| matches!(p, CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0))
            .unwrap_or_else(|| panic!("exiled card must carry P0's PlayFromExile grant, got {:?}", obj.casting_permissions));
        match grant {
            CastingPermission::PlayFromExile { duration, .. } => assert_eq!(
                *duration,
                Duration::UntilEndOfTurn,
                "CR 611.2a: 'play those cards this turn' creates a continuous effect lasting until end of turn"
            ),
            _ => unreachable!(),
        }
    }

    // The buried library card is untouched and ungranted.
    assert_eq!(runner.state().objects[&buried].zone, Zone::Library);
    assert!(runner.state().objects[&buried]
        .casting_permissions
        .is_empty());

    // The unrelated prior-exile card carries NO grant — the permission is scoped
    // to the cards this ability exiled, not to everything in the exile zone.
    assert!(
        !runner.state().objects[&control]
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0)),
        "a card exiled by an unrelated source must not receive Harnfel's play permission"
    );

    // The discard cost consumed the hand card.
    assert_eq!(
        runner.state().objects[&discard].zone,
        Zone::Graveyard,
        "the 'Discard a card' cost must move the fodder to the graveyard"
    );

    // Consumer + expiry: spend the grant on one exiled card this turn, then
    // confirm the unplayed sibling stops being castable once the turn ends.
    assert_play_from_exile_consumed_then_expires(&mut runner, top_two[0], top_two[1]);
}

// ---------------------------------------------------------------------------
// Test 5 — negative scoping (controller-relative "you control" / "you cast").
// ---------------------------------------------------------------------------

/// CR 602.5b: Birgi's boast-twice static is controller-scoped ("Creatures YOU
/// control"). An OPPONENT's boast creature is not affected, so the opponent's
/// second boast is rejected even with your Birgi on the battlefield.
#[test]
fn opponents_boast_creature_not_raised_by_your_birgi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Birgi, God of Storytelling", 3, 3, BIRGI_FRONT);
    let opp_boaster = scenario
        .add_creature_from_oracle(P1, "Opp Boaster", 2, 2, BOAST_ABILITY)
        .id();
    scenario.with_mana_pool(P1, red_pool(5));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .creatures_attacked_this_turn
        .insert(opp_boaster);
    // Hand priority to P1 so the opponent legitimately activates their ability.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let idx = boast_index(&runner, opp_boaster);

    // Reach-guard: the first activation succeeds (boast is available).
    runner
        .act(GameAction::ActivateAbility {
            source_id: opp_boaster,
            ability_index: idx,
        })
        .expect("first boast activation by the opponent must be legal");
    let second = runner.act(GameAction::ActivateAbility {
        source_id: opp_boaster,
        ability_index: idx,
    });
    assert!(
        second.is_err(),
        "CR 602.5b: your Birgi must not raise the boast limit for an OPPONENT's creature"
    );
}

/// Birgi's mana trigger is "whenever YOU cast a spell" — an opponent's cast does
/// not trigger it. Birgi is P1's; P0 (the active player) casts a spell; P1's pool
/// stays empty of red. Reach-guard: P0's spell actually resolved (gained life).
#[test]
fn opponents_cast_does_not_trigger_birgi_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Birgi, God of Storytelling", 3, 3, BIRGI_FRONT);
    let spell = cheap_spell(&mut scenario, P0, "P0 Spell");

    let mut runner = scenario.build();
    let p0_life_before = runner.state().players[pidx(P0)].life;

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        red_count(runner.state(), P1),
        0,
        "CR 605.1b: an opponent's cast must not trigger Birgi's controller's mana ability"
    );
    assert_eq!(
        runner.state().players[pidx(P0)].life,
        p0_life_before + 1,
        "reach-guard: P0's spell resolved (gained 1 life), so the negative above is not vacuous"
    );
}

// ---------------------------------------------------------------------------
// Test 4b — MDFC back-face cast of the REAL card, pinning the modal face choice.
// ---------------------------------------------------------------------------

/// CR 712.11b: Birgi // Harnfel is a modal DFC, so its BACK face Harnfel is cast
/// directly from hand by choosing it at cast time (`ChooseModalFace{back_face:
/// true}`). Harnfel enters as a Legendary Artifact; its "Discard a card: exile
/// the top two, you may play those cards this turn" activated ability then exiles
/// the top two library cards with a P0 `PlayFromExile` grant that ends this turn
/// (CR 611.2a), while a card exiled by an unrelated source carries no grant.
///
/// Drives the REAL card through the actual pipeline: `CastSpell` →
/// `ModalFaceChoice` → `ChooseModalFace` → resolution → the discard-cost
/// activation. This is the coverage for the modal face-choice machinery that the
/// synthetic sibling above (which places the artifact directly) cannot pin.
#[test]
fn harnfel_back_face_cast_grants_play_from_exile_this_turn() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_life(P0, 20);
    let card = scenario.add_real_card(P0, "Birgi, God of Storytelling", Zone::Hand, db);
    let discard = scenario.add_card_to_hand(P0, "Discard Fodder");
    // Buried added first so it sinks below the two cards Harnfel will exile.
    let buried = scenario.add_card_to_library_top(P0, "Buried");
    let top_two: Vec<ObjectId> = ["Top B", "Top A"]
        .into_iter()
        .map(|n| castable_top(&mut scenario, P0, n))
        .collect();
    let control = scenario.add_creature_to_exile(P0, "Prior Exile", 1, 1).id();
    // {4}{R} for the Harnfel back face.
    scenario.with_mana_pool(P0, red_pool(5));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // CR 712.3: the back face must be hydrated as Modal for the face choice.
    let back = runner
        .state()
        .objects
        .get(&card)
        .and_then(|o| o.back_face.clone())
        .expect("Harnfel back face must be hydrated in hand");
    assert_eq!(back.name, "Harnfel, Horn of Bounty");
    assert_eq!(back.layout_kind, Some(LayoutKind::Modal));

    // Cast the modal DFC and choose the back face (CR 712.11b).
    let card_id = runner.state().objects[&card].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell on Birgi accepted");
    assert!(
        matches!(result.waiting_for, WaitingFor::ModalFaceChoice { .. }),
        "casting a modal DFC must enter ModalFaceChoice; got {:?}",
        result.waiting_for
    );
    runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("ChooseModalFace{{back}} accepted");
    runner.advance_until_stack_empty();

    // Harnfel resolved onto the battlefield as the artifact.
    let harnfel = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.zone == Zone::Battlefield && o.name == "Harnfel, Horn of Bounty")
        .map(|(id, _)| *id)
        .expect("Harnfel, Horn of Bounty must be on the battlefield after the back-face cast");

    // Activate the discard-cost ability through the real activation pipeline.
    let idx = runner.state().objects[&harnfel]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Harnfel must carry the discard-cost activated ability");
    runner
        .activate(harnfel, idx)
        .pay_with(&[discard])
        .accept_optional()
        .resolve();
    runner.advance_until_stack_empty();

    // The top two library cards are exiled with a P0 play-this-turn grant.
    for id in &top_two {
        let obj = &runner.state().objects[id];
        assert_eq!(obj.zone, Zone::Exile, "the top two cards must be exiled");
        let grant = obj
            .casting_permissions
            .iter()
            .find(|p| matches!(p, CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0))
            .unwrap_or_else(|| panic!("exiled card must carry P0's PlayFromExile grant, got {:?}", obj.casting_permissions));
        match grant {
            CastingPermission::PlayFromExile { duration, .. } => assert_eq!(
                *duration,
                Duration::UntilEndOfTurn,
                "CR 611.2a: 'play those cards this turn' creates an until-end-of-turn continuous effect (gone next turn)"
            ),
            _ => unreachable!(),
        }
    }

    // The buried library card is untouched and ungranted.
    assert_eq!(runner.state().objects[&buried].zone, Zone::Library);
    assert!(runner.state().objects[&buried]
        .casting_permissions
        .is_empty());

    // The unrelated prior-exile card carries no grant — the permission is scoped
    // to the cards this ability exiled, not to everything in the exile zone.
    assert!(
        !runner.state().objects[&control]
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0)),
        "a card exiled by an unrelated source must not receive Harnfel's play permission"
    );

    // Consumer + expiry through the REAL modal-back-face path: spend the grant on
    // one exiled card this turn, then confirm the unplayed sibling expires.
    assert_play_from_exile_consumed_then_expires(&mut runner, top_two[0], top_two[1]);
}
