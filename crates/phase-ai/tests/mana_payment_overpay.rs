//! phase-rs/phase#10488: at a spell's `WaitingFor::ManaPayment` step the AI
//! must not activate a costed mana ability (pay life, discard a card, ...) the
//! payment no longer needs.
//!
//! CR 601.2g: the caster activates mana abilities, then (CR 601.2h) pays the
//! total cost. CR 118.3c: activating mana abilities is never mandatory, so once
//! passing pays the total cost, a further activation only pays that ability's
//! own cost for mana the spell won't spend (CR 106.4).
//!
//! Each row announces an Auto cast the engine's payer can't finish on its own
//! (it doesn't pay life or discard), lets `choose_action` make every decision
//! until the spell is cast (CR 601.2i), and counts what the AI paid. The
//! targeted bounce used to stall at the payment step: an extra activation kept
//! the bounce, which hands the opponent a card, beyond the search horizon, so
//! the AI paid 3 life at a time down to 8.
//!
//! Oracle text verbatim from Scryfall `cards/named`.

use engine::game::engine::apply_as_current_for_simulation;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use phase_ai::config::AiConfig;
use phase_ai::search::choose_action;
use rand::rngs::SmallRng;
use rand::SeedableRng;

const TREASONOUS_OGRE: &str = "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\nPay 3 life: Add {R}.";
const SKIRGE_FAMILIAR: &str = "Flying\nDiscard a card: Add {B}.";
const UNSUBSTANTIATE: &str = "Return target spell or creature to its owner's hand.";
const WORDS_OF_WISDOM: &str = "You draw two cards, then each other player draws a card.";
const DIVINATION: &str = "Draw two cards.";
const DISFIGURE: &str = "Target creature gets -2/-2 until end of turn.";

const MAX_DECISIONS: usize = 20;

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// What the AI spent to cast the spell.
#[derive(Debug, PartialEq, Eq)]
struct Paid {
    life: i32,
    mana_activations: usize,
    cards_in_hand: usize,
}

/// The spell under test and the costed mana source that can help pay for it.
enum Board {
    /// An Island and `ogres` Treasonous Ogres; `spell` costs `generic` plus {U}.
    IslandOgre {
        spell: &'static str,
        text: &'static str,
        is_instant: bool,
        generic: u32,
        ogres: usize,
    },
    /// Skirge Familiar, Disfigure and three other cards in hand.
    SkirgeFamiliar,
}

fn ai_pays(board: Board) -> Paid {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    sc.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let (sources, spell): (Vec<ObjectId>, ObjectId) = match board {
        Board::IslandOgre {
            spell,
            text,
            is_instant,
            generic,
            ogres,
        } => {
            sc.add_basic_land(P0, ManaColor::Blue);
            let ogres = (0..ogres)
                .map(|_| {
                    sc.add_creature(P0, "Treasonous Ogre", 2, 3)
                        .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
                        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
                        .id()
                })
                .collect();
            let spell = sc
                .add_spell_to_hand_from_oracle(P0, spell, is_instant, text)
                .with_mana_cost(cost(vec![ManaCostShard::Blue], generic))
                .id();
            (ogres, spell)
        }
        Board::SkirgeFamiliar => {
            let familiar = sc
                .add_creature(P0, "Skirge Familiar", 3, 2)
                .from_oracle_text_with_keywords(&["Flying"], SKIRGE_FAMILIAR)
                .with_mana_cost(cost(vec![ManaCostShard::Black], 4))
                .id();
            for name in ["Other Card A", "Other Card B", "Other Card C"] {
                sc.add_spell_to_hand_from_oracle(P0, name, true, "Draw a card.")
                    .with_mana_cost(cost(vec![ManaCostShard::Red, ManaCostShard::Red], 5));
            }
            let spell = sc
                .add_spell_to_hand_from_oracle(P0, "Disfigure", true, DISFIGURE)
                .with_mana_cost(cost(vec![ManaCostShard::Black], 0))
                .id();
            (vec![familiar], spell)
        }
    };
    let mut state = sc.build().state().clone();
    let card_id = state.objects[&spell].card_id;
    apply_as_current_for_simulation(
        &mut state,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("the Auto cast announces");

    let config = AiConfig::default();
    let mut rng = SmallRng::seed_from_u64(42);
    let mut mana_activations = 0;
    let mut reached_payment = false;
    for _ in 0..MAX_DECISIONS {
        if state.stack.iter().any(|entry| entry.id == spell)
            && state.pending_cast.is_none()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            assert!(
                reached_payment,
                "reach: the cast waited at the mana-payment step for the AI"
            );
            return Paid {
                life: state.players[0].life,
                mana_activations,
                cards_in_hand: state.players[0].hand.len(),
            };
        }
        reached_payment |= matches!(state.waiting_for, WaitingFor::ManaPayment { .. });
        // The payment step is under test, not target choice: aim at the
        // opponent's creature.
        let action = if matches!(state.waiting_for, WaitingFor::TargetSelection { .. }) {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bears)),
            }
        } else {
            choose_action(&state, P0, &config, &mut rng).expect("the AI decides")
        };
        assert_ne!(
            action,
            GameAction::CancelCast,
            "the AI abandoned the cast at {:?}",
            state.waiting_for
        );
        if matches!(action, GameAction::ActivateAbility { source_id, .. } if sources.contains(&source_id))
        {
            mana_activations += 1;
        }
        apply_as_current_for_simulation(&mut state, action).expect("the AI's choice is legal");
    }
    panic!("the AI didn't finish the cast within {MAX_DECISIONS} decisions")
}

#[test]
fn untargeted_cast_pays_one_life_activation() {
    assert_eq!(
        ai_pays(Board::IslandOgre {
            spell: "Words of Wisdom",
            text: WORDS_OF_WISDOM,
            is_instant: true,
            generic: 1,
            ogres: 1,
        }),
        Paid {
            life: 17,
            mana_activations: 1,
            cards_in_hand: 0,
        },
        "control: the Island and one Ogre activation pay {{1}}{{U}}"
    );
}

#[test]
fn targeted_cast_pays_one_life_activation() {
    assert_eq!(
        ai_pays(Board::IslandOgre {
            spell: "Unsubstantiate",
            text: UNSUBSTANTIATE,
            is_instant: true,
            generic: 1,
            ogres: 1,
        }),
        Paid {
            life: 17,
            mana_activations: 1,
            cards_in_hand: 0,
        },
        "CR 601.2h + CR 118.3c: the Island and one Ogre activation pay {{1}}{{U}}; \
         the AI must not pay more life than the cost needs"
    );
}

#[test]
fn cast_pays_every_life_activation_it_needs() {
    assert_eq!(
        ai_pays(Board::IslandOgre {
            spell: "Divination",
            text: DIVINATION,
            is_instant: false,
            generic: 2,
            ogres: 2,
        }),
        Paid {
            life: 14,
            mana_activations: 2,
            cards_in_hand: 0,
        },
        "CR 601.2h: {{2}}{{U}} needs the Island and two Ogre activations, no fewer, no more"
    );
}

#[test]
fn targeted_cast_discards_one_card_for_its_one_mana() {
    assert_eq!(
        ai_pays(Board::SkirgeFamiliar),
        Paid {
            life: 20,
            mana_activations: 1,
            cards_in_hand: 2,
        },
        "CR 601.2h + CR 118.3c: {{B}} needs one Familiar activation, so one discard"
    );
}
