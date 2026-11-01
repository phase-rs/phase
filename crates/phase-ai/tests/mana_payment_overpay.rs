//! phase-rs/phase#10488: at a spell's `WaitingFor::ManaPayment` step the AI
//! kept activating a costed mana ability (pay life, discard a card) after
//! passing would already pay the cost.
//!
//! CR 601.2: casting a spell — announcing it, activating mana abilities
//! (CR 601.2g) and paying the total cost (CR 601.2h) — is one process, and the
//! caster gets priority only once the spell becomes cast (CR 601.2i). The
//! search used to charge a ply for every payment step, so another activation
//! pushed the opponent's reply to the cast past its horizon. A targeted bounce
//! (which hands the opponent a card) therefore stalled at the payment step, the
//! AI paying 3 life at a time from 20 down to 8.
//!
//! The rows drive the cast through the AI's own ranking (`score_candidates` at
//! the default difficulty, top-ranked action at each decision), so they read
//! what the search prefers, not one draw of the chooser's softmax. The test
//! fixes the target (the opponent's creature): the payment is under test. The
//! last test keeps a payment-sensitive line (Slaying Fire's adamant) open to
//! the search.
//!
//! Oracle text verbatim from Scryfall `cards/named`. "Card A".."Card F" are
//! textless library placeholders, so the draw spells' resolution inside the
//! search doesn't deck anyone.

use engine::ai_support::{
    activates_mana_ability, classify_payment_continuation, witness_payment_continuations,
    PaymentContinuationRoot, PaymentContinuationState, PaymentContinuationStep,
};
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use phase_ai::config::AiConfig;
use phase_ai::search::{choose_action, score_candidates};
use rand::rngs::SmallRng;
use rand::SeedableRng;

const TREASONOUS_OGRE: &str = "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\nPay 3 life: Add {R}.";
const SKIRGE_FAMILIAR: &str = "Flying\nDiscard a card: Add {B}.";
const UNSUBSTANTIATE: &str = "Return target spell or creature to its owner's hand.";
const WORDS_OF_WISDOM: &str = "You draw two cards, then each other player draws a card.";
const DIVINATION: &str = "Draw two cards.";
const DISFIGURE: &str = "Target creature gets -2/-2 until end of turn.";
const SLAYING_FIRE: &str = "Slaying Fire deals 3 damage to any target.\nAdamant — If at least three red mana was spent to cast this spell, it deals 4 damage instead.";

const MAX_DECISIONS: usize = 20;
const SWEEP_SEEDS: u64 = 30;

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
#[derive(Clone, Copy)]
enum Board {
    /// An Island and `ogres` Treasonous Ogres; `spell` costs `generic` plus {U}.
    IslandOgre {
        spell: &'static str,
        text: &'static str,
        is_instant: bool,
        generic: u32,
        ogres: usize,
    },
    /// Skirge Familiar, Disfigure and three Divinations in hand (off-color
    /// discard fodder).
    SkirgeFamiliar,
}

const UNSUBSTANTIATE_BOARD: Board = Board::IslandOgre {
    spell: "Unsubstantiate",
    text: UNSUBSTANTIATE,
    is_instant: true,
    generic: 1,
    ogres: 1,
};
const WORDS_OF_WISDOM_BOARD: Board = Board::IslandOgre {
    spell: "Words of Wisdom",
    text: WORDS_OF_WISDOM,
    is_instant: true,
    generic: 1,
    ogres: 1,
};

/// How the AI's decision is taken from its scoring.
#[derive(Clone, Copy)]
enum Pick {
    /// The top-ranked action of `score_candidates`.
    TopRanked,
    /// `choose_action`'s softmax draw from this seed.
    Sampled(u64),
}

fn ai_pays(board: Board, pick: Pick) -> Paid {
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
            for _ in 0..3 {
                sc.add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
                    .with_mana_cost(cost(vec![ManaCostShard::Blue], 2));
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
    let mut rng = SmallRng::seed_from_u64(match pick {
        Pick::TopRanked => 0,
        Pick::Sampled(seed) => seed,
    });
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
        let action = if matches!(state.waiting_for, WaitingFor::TargetSelection { .. }) {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bears)),
            }
        } else {
            match pick {
                Pick::TopRanked => top_ranked(&state, &config),
                Pick::Sampled(_) => {
                    choose_action(&state, P0, &config, &mut rng).expect("the AI decides")
                }
            }
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

/// The first of the highest-scored actions, in the scorer's stable order.
fn top_ranked(state: &GameState, config: &AiConfig) -> GameAction {
    score_candidates(state, P0, config)
        .into_iter()
        .fold(
            None,
            |best: Option<(GameAction, f64)>, (action, score)| match best {
                Some((_, best_score)) if best_score >= score => best,
                _ => Some((action, score)),
            },
        )
        .map(|(action, _)| action)
        .expect("the AI scores at least one action")
}

#[test]
fn untargeted_cast_pays_one_life_activation() {
    assert_eq!(
        ai_pays(WORDS_OF_WISDOM_BOARD, Pick::TopRanked),
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
        ai_pays(UNSUBSTANTIATE_BOARD, Pick::TopRanked),
        Paid {
            life: 17,
            mana_activations: 1,
            cards_in_hand: 0,
        },
        "CR 601.2: the AI ranks passing above another Ogre activation once the \
         Island and one activation pay {{1}}{{U}}"
    );
}

#[test]
fn cast_pays_with_two_life_activations_when_it_needs_them() {
    assert_eq!(
        ai_pays(
            Board::IslandOgre {
                spell: "Divination",
                text: DIVINATION,
                is_instant: false,
                generic: 2,
                ogres: 2,
            },
            Pick::TopRanked
        ),
        Paid {
            life: 14,
            mana_activations: 2,
            cards_in_hand: 0,
        },
        "{{2}}{{U}} with an Island and two Ogres: two activations, then the pass"
    );
}

#[test]
fn targeted_cast_discards_one_card_for_its_one_mana() {
    assert_eq!(
        ai_pays(Board::SkirgeFamiliar, Pick::TopRanked),
        Paid {
            life: 20,
            mana_activations: 1,
            cards_in_hand: 2,
        },
        "{{B}} with Skirge Familiar: one activation, one discard, then the pass"
    );
}

/// The chooser samples a softmax over the scores, so a single seed can still
/// pick a surplus activation (the same on the untargeted path; reported
/// separately). The stall the issue reports was deterministic: every seed paid
/// four activations. Across the sweep, the usual outcome is one activation and
/// no seed reaches the stall.
#[test]
fn sampled_targeted_cast_no_longer_stalls() {
    for (name, board) in [
        ("Unsubstantiate", UNSUBSTANTIATE_BOARD),
        ("Words of Wisdom", WORDS_OF_WISDOM_BOARD),
    ] {
        let activations: Vec<_> = (0..SWEEP_SEEDS)
            .map(|seed| ai_pays(board, Pick::Sampled(seed)).mana_activations)
            .collect();
        let one = activations.iter().filter(|&&count| count == 1).count();
        eprintln!("[sweep] {name}: activations per seed {activations:?}");
        assert!(
            activations.iter().all(|&count| (1..4).contains(&count)),
            "{name}: every seed pays, and none stalls to four activations: {activations:?}"
        );
        assert!(
            2 * one > activations.len(),
            "{name}: most seeds pay with one activation: {activations:?}"
        );
    }
}

/// A Slaying Fire cast at Rumbling Baloth, paid so far with two floating
/// colorless and one Ogre activation: {R}{C}{C} pays {2}{R}.
fn slaying_fire_after_one_ogre() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    sc.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    let baloth = sc.add_creature(P1, "Rumbling Baloth", 4, 4).id();
    let ogre = sc
        .add_creature(P0, "Treasonous Ogre", 2, 3)
        .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
        .id();
    let fire = sc
        .add_spell_to_hand_from_oracle(P0, "Slaying Fire", true, SLAYING_FIRE)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 2))
        .id();
    sc.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ogre, false, Vec::new()))
            .collect(),
    );
    let mut runner = sc.build();
    let card_id = runner.state().objects[&fire].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fire,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Auto cast announces");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(baloth)),
        })
        .expect("Rumbling Baloth is a legal target");
    let ogre_activation = GameAction::ActivateAbility {
        source_id: ogre,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    };
    runner
        .act(ogre_activation)
        .expect("the Ogre pays 3 life for {R} while the spell is paid for");
    (runner, fire, ogre, baloth)
}

fn red_in_pool(state: &GameState) -> Vec<engine::types::mana::ManaPipId> {
    state.players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.color == ManaType::Red)
        .map(|unit| unit.pip_id)
        .collect()
}

fn resolve(runner: &mut GameRunner) {
    runner
        .act(GameAction::PassPriority)
        .expect("the pass pays the total cost");
    runner.advance_until_stack_empty();
}

/// CR 601.2h + CR 118.3c + CR 207.2c: once {R}{C}{C} floats, passing pays
/// Slaying Fire's {2}{R}; two more Ogre activations would let three red pay it
/// and make the adamant rider deal 4. Both continuations stay open to the AI.
#[test]
fn adamant_continuation_stays_searchable_once_the_pass_pays() {
    let (runner, fire, ogre, baloth) = slaying_fire_after_one_ogre();
    let state = runner.state().clone();
    assert_eq!(state.players[0].life, 17, "reach: one Ogre activation paid");
    assert_eq!(state.players[0].mana_pool.total(), 3);
    assert_eq!(
        red_in_pool(&state).len(),
        1,
        "reach: the pool is {{R}}{{C}}{{C}}"
    );
    assert!(matches!(state.waiting_for, WaitingFor::ManaPayment { .. }));
    assert!(matches!(
        classify_payment_continuation(&state),
        PaymentContinuationState::Affiliated(PaymentContinuationRoot::Spell { object_id, .. })
            if object_id == fire
    ));
    let ogre_activation = GameAction::ActivateAbility {
        source_id: ogre,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(activates_mana_ability(&state, &ogre_activation));
    let steps: Vec<_> =
        witness_payment_continuations(&state, &[GameAction::PassPriority, ogre_activation.clone()])
            .successors
            .into_iter()
            .map(|accepted| accepted.map(|accepted| accepted.step))
            .collect();
    assert_eq!(
        steps,
        vec![
            Some(PaymentContinuationStep::Finalizes),
            Some(PaymentContinuationStep::Continues),
        ],
        "reach: the pass finalizes and another activation continues the payment"
    );

    let config = AiConfig::default();
    let mut heuristic = config.clone();
    heuristic.search.enabled = false;
    for (label, config) in [("search", &config), ("heuristic", &heuristic)] {
        let scored = score_candidates(&state, P0, config);
        for action in [&GameAction::PassPriority, &ogre_activation] {
            assert!(
                scored.iter().any(|(scored, _)| scored == action),
                "{label}: {action:?} stays a scored payment continuation: {scored:?}"
            );
        }
    }

    // Semantic controls: what each continuation does.
    let mut now = GameRunner::from_state(state.clone());
    resolve(&mut now);
    assert_eq!(now.state().objects[&baloth].zone, Zone::Battlefield);
    assert_eq!(
        now.state().objects[&baloth].damage_marked,
        3,
        "passing now spends one red: 3 damage, the Baloth survives"
    );

    let mut adamant = GameRunner::from_state(state);
    for _ in 0..2 {
        adamant
            .act(ogre_activation.clone())
            .expect("the Ogre activates again while paying");
    }
    assert_eq!(adamant.state().players[0].life, 11);
    for pip_id in red_in_pool(adamant.state()) {
        adamant
            .act(GameAction::SpendPoolMana { pip_id })
            .expect("each red unit can be directed at the cost");
    }
    resolve(&mut adamant);
    assert_eq!(
        adamant.state().objects[&baloth].zone,
        Zone::Graveyard,
        "three red spent: adamant deals 4 and the Baloth dies"
    );
}
