//! phase-rs/phase#9812: once the legal-action offer certifies a targeted Auto
//! cast that needs a mana ability the Auto payer doesn't activate (Treasonous
//! Ogre's "Pay 3 life: Add {R}", Skirge Familiar's "Discard a card: Add
//! {B}"), the AI that chose the offered action must be able to finish it.
//!
//! The offer's certificate is discarded; the AI pays through its own planner
//! (`prepare_payment_candidates` over the same reducer-backed payment
//! witness). Each row drives `choose_action` from the offered announcement and
//! requires the spell to be cast (CR 601.2i) within a bounded number of
//! decisions, without cancelling.
//!
//! Oracle text verbatim from Scryfall `cards/named`.

use engine::ai_support::legal_actions;
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use phase_ai::config::AiConfig;
use phase_ai::search::choose_action;
use rand::rngs::SmallRng;
use rand::SeedableRng;

const NYX_LOTUS: &str = "Nyx Lotus enters tapped.\n{T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color. (Your devotion to a color is the number of mana symbols of that color in the mana costs of permanents you control.)";
const TREASONOUS_OGRE: &str = "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\nPay 3 life: Add {R}.";
const SKIRGE_FAMILIAR: &str = "Flying\nDiscard a card: Add {B}.";
const UNSUBSTANTIATE: &str = "Return target spell or creature to its owner's hand.";
const DISFIGURE: &str = "Target creature gets -2/-2 until end of turn.";

/// Correct play needs a target, one or two mana activations (with a discard
/// choice for the Familiar) and the payment pass.
const MAX_DECISIONS: usize = 20;

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

#[derive(Clone, Copy, Debug)]
enum Board {
    IslandOgre,
    NyxLotusOgre,
    SkirgeFamiliar,
}

/// The issue's boards; returns the state and the targeted spell.
fn board(board: Board) -> (GameState, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.add_creature(P1, "Grizzly Bears", 2, 2);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let spell = match board {
        Board::IslandOgre | Board::NyxLotusOgre => {
            if let Board::NyxLotusOgre = board {
                let mut lotus = sc.add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS);
                lotus.as_legendary().with_mana_cost(cost(vec![], 4));
            } else {
                sc.add_basic_land(P0, ManaColor::Blue);
            }
            sc.add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
                .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
            sc.add_creature(P0, "Treasonous Ogre", 2, 3)
                .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
                .with_mana_cost(cost(vec![ManaCostShard::Red], 3));
            sc.add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
                .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
                .id()
        }
        Board::SkirgeFamiliar => {
            sc.add_creature(P0, "Skirge Familiar", 3, 2)
                .from_oracle_text_with_keywords(&["Flying"], SKIRGE_FAMILIAR)
                .with_mana_cost(cost(vec![ManaCostShard::Black], 4));
            sc.add_spell_to_hand_from_oracle(P0, "Other Card", true, "Draw a card.")
                .with_mana_cost(cost(vec![ManaCostShard::Red, ManaCostShard::Red], 5));
            sc.add_spell_to_hand_from_oracle(P0, "Disfigure", true, DISFIGURE)
                .with_mana_cost(cost(vec![ManaCostShard::Black], 0))
                .id()
        }
    };
    (sc.build().state().clone(), spell)
}

#[test]
fn ai_finishes_the_offered_targeted_cast_on_each_issue_board() {
    for which in [
        Board::IslandOgre,
        Board::NyxLotusOgre,
        Board::SkirgeFamiliar,
    ] {
        let (mut state, spell) = board(which);
        let action = GameAction::CastSpell {
            object_id: spell,
            card_id: state.objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        };
        assert!(
            legal_actions(&state).contains(&action),
            "{which:?}: reach: the targeted cast is offered"
        );
        apply_as_current_for_simulation(&mut state, action).expect("the offered cast announces");

        let config = AiConfig::default();
        let mut rng = SmallRng::seed_from_u64(42);
        let mut decisions = Vec::new();
        let cast = (0..MAX_DECISIONS).any(|_| {
            if state.stack.iter().any(|entry| entry.id == spell)
                && state.pending_cast.is_none()
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                return true;
            }
            let Some(chosen) = choose_action(&state, P0, &config, &mut rng) else {
                return false;
            };
            assert_ne!(
                chosen,
                GameAction::CancelCast,
                "{which:?}: the AI abandoned a certified cast after {decisions:?}"
            );
            decisions.push(format!("{chosen:?}"));
            apply_as_current_for_simulation(&mut state, chosen).expect("the AI's choice is legal");
            false
        });
        eprintln!("[{which:?}] cast={cast} decisions={decisions:?}");
        assert!(
            cast,
            "{which:?}: CR 601.2i: the AI finishes the offered cast within {MAX_DECISIONS} decisions; it took {decisions:?}, now at {:?}",
            state.waiting_for
        );
        assert_ne!(state.objects[&spell].zone, Zone::Hand);
    }
}
