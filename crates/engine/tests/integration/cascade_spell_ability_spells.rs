//! Cascade on spells that have a spell ability (instants, sorceries, and any
//! spell granted cascade), through the production cast pipeline with real cards.
//!
//! CR 702.85a: Cascade is a "when you cast this spell" trigger. The engine
//! synthesizes it with a `WasCast` guard that is rechecked when the trigger
//! resolves, while the spell is still on the stack. That recheck reads the
//! spell object's cast provenance, which `finalize_cast` used to stamp only on
//! spells without a spell ability (vanilla permanents), so every instant and
//! sorcery with cascade put a trigger on the stack that resolved as a no-op.
//!
//! https://github.com/phase-rs/phase/issues/8060
//! https://github.com/phase-rs/phase/issues/4762

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, EffectKind, TargetRef};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastOfferKind, CastPaymentMode, GameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use ManaType::{Black, Blue, Colorless, Green, Red};

/// The cascade library: a land miss on top, then a mana-value-1 hit.
struct CascadeBoard {
    runner: GameRunner,
    spell: ObjectId,
    victim: ObjectId,
    miss: ObjectId,
    hit: ObjectId,
}

fn pool(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|m| ManaUnit::new(*m, ObjectId(0), false, vec![]))
        .collect()
}

fn add_mana(runner: &mut GameRunner, player: engine::types::player::PlayerId, mana: &[ManaType]) {
    let player_state = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    for unit in pool(mana) {
        player_state.mana_pool.add(unit);
    }
}

fn board(
    spell_name: &str,
    mana: &[ManaType],
    setup: impl FnOnce(&mut GameScenario),
) -> CascadeBoard {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);
    let miss = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let hit = scenario.add_real_card(P0, "Dark Ritual", Zone::Library, db);
    let filler = scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    let victim = scenario.add_creature(P1, "Cascade Victim", 2, 2).id();
    scenario.with_mana_pool(P0, pool(mana));
    setup(&mut scenario);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library = vec![miss, hit, filler].into();
    CascadeBoard {
        runner,
        spell,
        victim,
        miss,
        hit,
    }
}

fn cascade_triggers(state: &GameState) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { ability, .. }
                if matches!(ability.effect, Effect::Cascade))
        })
        .count()
}

/// Reach guard: the cast went through the production pipeline and its one
/// cascade trigger sits on top of the spell.
fn assert_cascade_trigger_above_spell(state: &GameState, spell: ObjectId) {
    assert_eq!(cascade_triggers(state), 1, "stack = {:?}", state.stack);
    let top = state.stack.last().expect("stack has the trigger");
    assert!(
        matches!(&top.kind, StackEntryKind::TriggeredAbility { ability, .. }
            if matches!(ability.effect, Effect::Cascade) && ability.source_id == spell),
        "the cascade trigger must be on top, above its spell; stack = {:?}",
        state.stack
    );
    assert_eq!(state.objects[&spell].zone, Zone::Stack);
}

/// Pass priority until the cascade offer appears, collecting every event.
fn pass_until_offer(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }) {
            return;
        }
        if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
            let result = runner
                .act(GameAction::KeepAllCopyTargets)
                .expect("keep the copy's targets");
            events.extend(result.events);
            continue;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected prompt before the cascade offer: {:?}",
            runner.state().waiting_for
        );
        let result = runner.act(GameAction::PassPriority).expect("pass priority");
        events.extend(result.events);
    }
    panic!(
        "cascade never offered its hit; waiting_for = {:?}, stack = {:?}",
        runner.state().waiting_for,
        runner.state().stack
    );
}

/// CR 702.85a: the walk exiled the miss and stopped at the first nonland card
/// with lower mana value, and the controller is offered to cast it.
fn assert_offered_hit(board: &CascadeBoard) {
    let state = board.runner.state();
    assert_eq!(
        state.objects[&board.miss].zone,
        Zone::Exile,
        "the land miss is exiled"
    );
    assert_eq!(
        state.objects[&board.hit].zone,
        Zone::Exile,
        "the hit is exiled"
    );
    assert!(
        matches!(
            &state.waiting_for,
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Cascade { hit_card, exiled_misses, .. },
            } if *player == P0 && *hit_card == board.hit && exiled_misses == &vec![board.miss]
        ),
        "waiting_for = {:?}",
        state.waiting_for
    );
}

/// Cast the hit for free, then let everything resolve.
fn cast_hit_and_resolve(board: &mut CascadeBoard) {
    board
        .runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("cast the cascade hit without paying its mana cost");
    assert_eq!(
        board.runner.state().objects[&board.hit].zone,
        Zone::Stack,
        "the hit is cast onto the stack"
    );
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(
        state.objects[&board.hit].zone,
        Zone::Graveyard,
        "the hit resolved"
    );
    assert_eq!(
        state.spells_cast_this_turn_by_player[&P0].len(),
        2,
        "the cascade spell and its hit were both cast"
    );
    // CR 702.85a: the uncast miss goes to the bottom of the library.
    assert_eq!(state.objects[&board.miss].zone, Zone::Library);
    assert_eq!(state.players[0].library.last().copied(), Some(board.miss));
}

/// Cast, check the trigger, walk to the offer, cast the hit.
fn cast_and_cascade(board: &mut CascadeBoard, target: Option<ObjectId>) {
    {
        let cast = board.runner.cast(board.spell);
        let cast = match target {
            Some(object) => cast.target_object(object),
            None => cast,
        };
        let _committed = cast.commit();
    }
    assert_cascade_trigger_above_spell(board.runner.state(), board.spell);
    pass_until_offer(&mut board.runner, &mut Vec::new());
    assert_offered_hit(board);
    cast_hit_and_resolve(board);
}

#[test]
fn violent_outburst_cascades_into_its_hit() {
    let mut board = board("Violent Outburst", &[Red, Green, Colorless], |_| {});
    cast_and_cascade(&mut board, None);
    assert_eq!(
        board.runner.state().objects[&board.spell].zone,
        Zone::Graveyard,
        "Violent Outburst resolves after its cascade"
    );
}

#[test]
fn bituminous_blast_cascades_into_its_hit_and_still_deals_damage() {
    let mut board = board(
        "Bituminous Blast",
        &[Black, Red, Colorless, Colorless, Colorless],
        |_| {},
    );
    let victim = board.victim;
    cast_and_cascade(&mut board, Some(victim));
    let state = board.runner.state();
    assert_eq!(state.objects[&board.spell].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&victim].zone,
        Zone::Graveyard,
        "Bituminous Blast's 4 damage kills its target after the cascade"
    );
}

#[test]
fn demonic_dread_sorcery_cascades_into_its_hit() {
    let mut board = board("Demonic Dread", &[Black, Red, Colorless], |_| {});
    let victim = board.victim;
    cast_and_cascade(&mut board, Some(victim));
    assert_eq!(
        board.runner.state().objects[&board.spell].zone,
        Zone::Graveyard
    );
}

/// Control: a permanent spell with cascade and no spell ability.
#[test]
fn bloodbraid_elf_cascades_into_its_hit() {
    let mut board = board(
        "Bloodbraid Elf",
        &[Red, Green, Colorless, Colorless],
        |_| {},
    );
    cast_and_cascade(&mut board, None);
    assert_eq!(
        board.runner.state().objects[&board.spell].zone,
        Zone::Battlefield
    );
}

/// Granted cascade: Quandrix, the Proof gives instant and sorcery spells cast
/// from hand cascade (phase-rs/phase#4762).
#[test]
fn quandrix_grants_cascade_to_an_instant_cast_from_hand() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut board = board("Think Twice", &[Blue, Colorless], |scenario| {
        scenario.add_real_card(P0, "Quandrix, the Proof", Zone::Battlefield, db);
    });
    assert!(
        !board.runner.state().objects[&board.spell]
            .keywords
            .contains(&Keyword::Cascade),
        "Think Twice has no printed cascade; the grant must supply it"
    );
    cast_and_cascade(&mut board, None);
}

/// CR 707.10: a copy of a spell isn't cast, so a copied cascade instant must
/// neither carry cast provenance nor cascade again.
#[test]
fn copy_of_a_cascade_instant_does_not_cascade() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut twincast = None;
    let mut board = board(
        "Bituminous Blast",
        &[Black, Red, Colorless, Colorless, Colorless],
        |scenario| {
            twincast = Some(scenario.add_real_card(P0, "Twincast", Zone::Hand, db));
        },
    );
    let twincast = twincast.expect("Twincast added");
    let (spell, victim) = (board.spell, board.victim);
    {
        let _committed = board.runner.cast(spell).target_object(victim).commit();
    }
    assert_cascade_trigger_above_spell(board.runner.state(), spell);
    add_mana(&mut board.runner, P0, &[Blue, Blue]);
    {
        let _committed = board.runner.cast(twincast).target_object(spell).commit();
    }

    let mut events = Vec::new();
    // Twincast resolves first and puts the copy on the stack.
    let result = board.runner.act(GameAction::PassPriority).expect("p0 pass");
    events.extend(result.events);
    let result = board.runner.act(GameAction::PassPriority).expect("p1 pass");
    events.extend(result.events);
    if matches!(
        board.runner.state().waiting_for,
        WaitingFor::CopyRetarget { .. }
    ) {
        let result = board
            .runner
            .act(GameAction::KeepAllCopyTargets)
            .expect("keep the copy's targets");
        events.extend(result.events);
    }
    let state = board.runner.state();
    let copy = state
        .stack
        .iter()
        .find(|entry| {
            entry.id != spell
                && matches!(entry.kind, StackEntryKind::Spell { .. })
                && state
                    .objects
                    .get(&entry.id)
                    .is_some_and(|o| o.name == "Bituminous Blast")
        })
        .map(|entry| entry.id)
        .expect("reach guard: Twincast put a copy of Bituminous Blast on the stack");
    assert!(
        state.objects[&copy].keywords.contains(&Keyword::Cascade),
        "reach guard: the copy has the cascade keyword"
    );
    assert_eq!(
        state.objects[&copy].cast_from_zone, None,
        "CR 707.10: copy not cast"
    );
    assert_eq!(
        state.objects[&copy].cast_controller, None,
        "CR 707.10: copy not cast"
    );
    assert_eq!(
        cascade_triggers(state),
        1,
        "the copy adds no cascade trigger"
    );

    pass_until_offer(&mut board.runner, &mut events);
    assert_offered_hit(&board);
    board
        .runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline the hit");
    board.runner.advance_until_stack_empty();
    let cascade_kind = EffectKind::from(&Effect::Cascade);
    assert_eq!(
        events
            .iter()
            .filter(
                |e| matches!(e, GameEvent::EffectResolved { kind, .. } if *kind == cascade_kind)
            )
            .count(),
        1,
        "exactly one cascade (the original's) resolves"
    );
    assert_eq!(board.runner.state().objects[&spell].zone, Zone::Graveyard);
}

/// CR 113.7a: once triggered, cascade exists independently of its source. A
/// cascade instant countered in response still cascades.
#[test]
fn countered_cascade_instant_still_cascades() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut counterspell = None;
    let mut board = board(
        "Bituminous Blast",
        &[Black, Red, Colorless, Colorless, Colorless],
        |scenario| {
            counterspell = Some(scenario.add_real_card(P1, "Counterspell", Zone::Hand, db));
            scenario.with_mana_pool(P1, pool(&[Blue, Blue]));
        },
    );
    let counterspell = counterspell.expect("Counterspell added");
    let (spell, victim) = (board.spell, board.victim);
    {
        let _committed = board.runner.cast(spell).target_object(victim).commit();
    }
    assert_cascade_trigger_above_spell(board.runner.state(), spell);
    board.runner.act(GameAction::PassPriority).expect("p0 pass");
    let card_id = board.runner.state().objects[&counterspell].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: counterspell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Counterspell");
    for _ in 0..4 {
        match board.runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                board
                    .runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(spell)),
                    })
                    .expect("target Bituminous Blast");
            }
            WaitingFor::ManaPayment { .. } => {
                board.runner.act(GameAction::PassPriority).expect("pay");
            }
            _ => break,
        }
    }

    pass_until_offer(&mut board.runner, &mut Vec::new());
    assert_eq!(
        board.runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "reach guard: Bituminous Blast was countered before its cascade resolved"
    );
    assert_eq!(
        board.runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "the countered Blast dealt no damage"
    );
    assert_offered_hit(&board);
    board
        .runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("cast the cascade hit");
    assert_eq!(board.runner.state().objects[&board.hit].zone, Zone::Stack);
}

/// CR 603.3a + CR 109.5: cascade is controlled by whoever controlled the spell
/// when it was cast. If Commandeer steals the cascade spell before its trigger
/// resolves, the original caster still exiles from their own library and gets
/// the offer.
#[test]
fn stolen_cascade_spell_still_cascades_for_its_caster() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut stolen_ids = None;
    let mut board = board(
        "Bituminous Blast",
        &[Black, Red, Colorless, Colorless, Colorless],
        |scenario| {
            let commandeer = scenario.add_real_card(P1, "Commandeer", Zone::Hand, db);
            let p1_miss = scenario.add_real_card(P1, "Forest", Zone::Library, db);
            let p1_hit = scenario.add_real_card(P1, "Dark Ritual", Zone::Library, db);
            scenario.with_mana_pool(
                P1,
                pool(&[
                    Blue, Blue, Colorless, Colorless, Colorless, Colorless, Colorless,
                ]),
            );
            stolen_ids = Some((commandeer, p1_miss, p1_hit));
        },
    );
    let (commandeer, p1_miss, p1_hit) = stolen_ids.expect("P1 cards added");
    board
        .runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .library = vec![p1_miss, p1_hit].into();
    let (spell, victim) = (board.spell, board.victim);
    {
        let _committed = board.runner.cast(spell).target_object(victim).commit();
    }
    assert_cascade_trigger_above_spell(board.runner.state(), spell);
    board.runner.act(GameAction::PassPriority).expect("p0 pass");
    let card_id = board.runner.state().objects[&commandeer].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: commandeer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Commandeer");
    for _ in 0..8 {
        match &board.runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                board
                    .runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(spell)),
                    })
                    .expect("target Bituminous Blast");
            }
            WaitingFor::ManaPayment { .. } => {
                board.runner.act(GameAction::PassPriority).expect("pay");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected Commandeer cast prompt: {other:?}"),
        }
    }
    // Both players pass; Commandeer resolves and P1 gains control of the Blast.
    for _ in 0..6 {
        if board.runner.state().objects[&commandeer].zone == Zone::Graveyard
            && matches!(
                board.runner.state().waiting_for,
                WaitingFor::Priority { .. }
            )
        {
            break;
        }
        match &board.runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                board.runner.act(GameAction::PassPriority).expect("pass");
            }
            // "You may choose new targets for it": P1 keeps the targets.
            WaitingFor::OptionalEffectChoice { .. } => {
                board
                    .runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("keep the stolen spell's targets");
            }
            WaitingFor::CopyRetarget { .. } => {
                board
                    .runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keep the stolen spell's targets");
            }
            other => panic!("unexpected prompt while Commandeer resolves: {other:?}"),
        }
    }
    let state = board.runner.state();
    assert_eq!(
        state.objects[&commandeer].zone,
        Zone::Graveyard,
        "Commandeer resolved"
    );
    assert_eq!(
        state.objects[&spell].controller, P1,
        "reach guard: Commandeer gave P1 control of Bituminous Blast"
    );
    let trigger = state
        .stack
        .last()
        .expect("cascade trigger still on the stack");
    assert!(
        matches!(&trigger.kind, StackEntryKind::TriggeredAbility { ability, .. }
            if matches!(ability.effect, Effect::Cascade) && ability.controller == P0),
        "reach guard: the cascade trigger is still P0's; stack = {:?}",
        state.stack
    );
    assert_eq!(trigger.controller, P0);

    pass_until_offer(&mut board.runner, &mut Vec::new());
    assert_offered_hit(&board);
    let state = board.runner.state();
    assert_eq!(
        state.objects[&p1_miss].zone,
        Zone::Library,
        "P1's library is untouched"
    );
    assert_eq!(
        state.objects[&p1_hit].zone,
        Zone::Library,
        "P1's library is untouched"
    );
}
