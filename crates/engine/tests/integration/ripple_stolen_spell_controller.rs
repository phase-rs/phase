//! CR 603.3a + CR 109.5: Ripple (CR 702.60a) is a "when you cast this spell"
//! trigger, so it is controlled by whoever controlled the spell when it was
//! cast. If Commandeer steals the Ripple spell before its trigger resolves, the
//! original caster still reveals from their own library, gets the free-cast
//! offer, orders the bottom, and the trigger settles for them; the thief's
//! library is untouched.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn pool(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|m| ManaUnit::new(*m, ObjectId(0), false, vec![]))
        .collect()
}

/// P0 cast Surging Flame (Ripple 4), P1 stole it with Commandeer, and the
/// ripple trigger has just resolved into P0's reveal prompt.
struct StolenRipple {
    runner: GameRunner,
    spell: ObjectId,
    /// P0's library, top first: the same-named hit, three misses, then one card
    /// below the Ripple 4 reveal.
    p0_library: Vec<ObjectId>,
    p1_library: Vec<ObjectId>,
}

impl StolenRipple {
    fn p0_hit(&self) -> ObjectId {
        self.p0_library[0]
    }

    fn library(&self, player: engine::types::player::PlayerId) -> Vec<ObjectId> {
        self.runner
            .state()
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists")
            .library
            .iter()
            .copied()
            .collect()
    }

    /// The ripple trigger finished for P0: P0 is back at priority, nothing is
    /// mid-resolution, the trigger is gone, and only the stolen spell remains.
    fn assert_settled_for_caster(&self) {
        let state = self.runner.state();
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority { player: P0 },
            "the caster is back at priority after the ripple trigger"
        );
        assert!(
            state.resolving_stack_entry.is_none(),
            "the ripple trigger's resolution carrier settled; carrier = {:?}",
            state.resolving_stack_entry
        );
        assert!(
            state.pending_resolution_completion.is_none(),
            "no ripple completion is left pending"
        );
        assert_eq!(state.stack.len(), 1, "stack = {:?}", state.stack);
        assert_eq!(
            state.stack[0].id, self.spell,
            "only the stolen Surging Flame remains"
        );
        assert_eq!(
            self.library(P1),
            self.p1_library,
            "the thief's library is untouched"
        );
    }
}

fn steal_then_resolve_ripple() -> StolenRipple {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_real_card(P0, "Surging Flame", Zone::Hand, db);
    let p0_library = vec![
        scenario.add_real_card(P0, "Surging Flame", Zone::Library, db),
        scenario.add_real_card(P0, "Mountain", Zone::Library, db),
        scenario.add_real_card(P0, "Island", Zone::Library, db),
        scenario.add_real_card(P0, "Swamp", Zone::Library, db),
        scenario.add_real_card(P0, "Forest", Zone::Library, db),
    ];
    let commandeer = scenario.add_real_card(P1, "Commandeer", Zone::Hand, db);
    let p1_library = vec![
        scenario.add_real_card(P1, "Surging Flame", Zone::Library, db),
        scenario.add_real_card(P1, "Mountain", Zone::Library, db),
    ];
    scenario.with_mana_pool(P0, pool(&[ManaType::Red, ManaType::Colorless]));
    scenario.with_mana_pool(
        P1,
        pool(&[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    for (player, library) in [(P0, &p0_library), (P1, &p1_library)] {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == player)
            .expect("player exists")
            .library = library.clone().into();
    }

    {
        let _committed = runner.cast(spell).target_player(P1).commit();
    }
    let top = runner
        .state()
        .stack
        .last()
        .expect("ripple trigger on the stack");
    assert!(
        matches!(&top.kind, StackEntryKind::TriggeredAbility { ability, .. }
            if matches!(ability.effect, Effect::Ripple { .. }) && ability.source_id == spell),
        "reach guard: Surging Flame's ripple trigger is on top; stack = {:?}",
        runner.state().stack
    );

    runner.act(GameAction::PassPriority).expect("p0 pass");
    let card_id = runner.state().objects[&commandeer].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: commandeer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Commandeer");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(spell)),
                    })
                    .expect("target Surging Flame");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected Commandeer cast prompt: {other:?}"),
        }
    }
    for _ in 0..6 {
        if runner.state().objects[&commandeer].zone == Zone::Graveyard
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            // "You may choose new targets for it": P1 keeps the targets.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("keep the stolen spell's targets");
            }
            other => panic!("unexpected prompt while Commandeer resolves: {other:?}"),
        }
    }
    let state = runner.state();
    assert_eq!(
        state.objects[&commandeer].zone,
        Zone::Graveyard,
        "Commandeer resolved"
    );
    assert_eq!(
        state.objects[&spell].controller, P1,
        "reach guard: Commandeer gave P1 control of Surging Flame"
    );
    let trigger = state
        .stack
        .last()
        .expect("ripple trigger still on the stack");
    assert!(
        matches!(&trigger.kind, StackEntryKind::TriggeredAbility { ability, .. }
            if matches!(ability.effect, Effect::Ripple { .. }) && ability.controller == P0),
        "reach guard: the ripple trigger is still P0's; stack = {:?}",
        state.stack
    );

    // Both players pass; the ripple trigger resolves.
    for _ in 0..2 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RippleRevealChoice { player, source_id, count: 4 }
                if player == P0 && source_id == spell
        ),
        "CR 603.3a: the caster, not the thief, decides the reveal; waiting_for = {:?}",
        runner.state().waiting_for
    );
    StolenRipple {
        runner,
        spell,
        p0_library,
        p1_library,
    }
}

fn reveal(board: &mut StolenRipple) {
    board
        .runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("reveal");
    let hit = board.p0_hit();
    assert!(
        matches!(
            &board.runner.state().waiting_for,
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Ripple { hit_card, .. },
            } if *player == P0 && *hit_card == hit
        ),
        "the caster is offered the same-named card from their own library; waiting_for = {:?}",
        board.runner.state().waiting_for
    );
}

#[test]
fn stolen_ripple_spell_still_ripples_for_its_caster() {
    let mut board = steal_then_resolve_ripple();
    reveal(&mut board);
    assert_eq!(
        board.library(P1),
        board.p1_library,
        "the thief's library is untouched"
    );
}

/// Terminal path with nothing revealed: declining the reveal settles the
/// trigger for the caster and leaves their library as it was.
#[test]
fn stolen_ripple_declined_reveal_settles_for_its_caster() {
    let mut board = steal_then_resolve_ripple();
    board
        .runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline the reveal");
    board.assert_settled_for_caster();
    assert_eq!(
        board.library(P0),
        board.p0_library,
        "nothing was revealed or moved"
    );
}

/// Terminal path with a bottom order: declining the hit puts all four revealed
/// cards on the bottom "in any order", chosen by the caster.
#[test]
fn stolen_ripple_declined_hit_bottom_order_settles_for_its_caster() {
    let mut board = steal_then_resolve_ripple();
    reveal(&mut board);
    board
        .runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline the free cast");
    let revealed: Vec<ObjectId> = board.p0_library[..4].to_vec();
    let WaitingFor::RippleBottomOrder { player, cards, .. } =
        board.runner.state().waiting_for.clone()
    else {
        panic!(
            "declining the hit must ask for the bottom order; waiting_for = {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "CR 603.3a: the caster orders the bottom");
    let mut offered = cards.clone();
    offered.sort();
    let mut expected = revealed.clone();
    expected.sort();
    assert_eq!(
        offered, expected,
        "all four revealed cards are offered for ordering"
    );

    let order = vec![revealed[2], revealed[0], revealed[3], revealed[1]];
    board
        .runner
        .act(GameAction::SelectCards {
            cards: order.clone(),
        })
        .expect("submit the bottom order");
    board.assert_settled_for_caster();
    let mut expected_library = vec![board.p0_library[4]];
    expected_library.extend(order);
    assert_eq!(
        board.library(P0),
        expected_library,
        "the unrevealed card stays on top and the revealed cards go to the bottom in the chosen order"
    );
}
