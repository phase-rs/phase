//! "Look at the top N cards of your library. You may cast a spell from among them ... Put the
//! rest on the bottom of your library": under the Dandân announcement "your library" is the one
//! shared pile whoever owns each card (CR 400.1, CR 401.2), so every looked-at card is offered
//! and every unchosen one goes to the bottom.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Svella's second ability: "{6}{R}{G}, {T}: Look at the top four cards of your library. You may
/// cast a spell from among them without paying its mana cost. Put the rest on the bottom of your
/// library in a random order."
const SVELLA_LOOK_ABILITY: usize = 1;

struct Peek {
    runner: GameRunner,
    /// The four looked-at cards, top first, as `(id, owner)`.
    top_four: Vec<(ObjectId, PlayerId)>,
}

/// P0 controls Svella; the top four cards of P0's library are Brainstorm, Island, Divination and
/// Opt with the given owners, followed by six Islands.
fn peek(format: FormatConfig, owners: [PlayerId; 4]) -> Peek {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_with_format(format, 2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let svella = scenario.add_real_card(P0, "Svella, Ice Shaper", Zone::Battlefield, db);
    let names = ["Brainstorm", "Island", "Divination", "Opt"];
    let top_four: Vec<_> = names
        .iter()
        .zip(owners)
        .map(|(name, owner)| {
            (
                scenario.add_real_card(owner, name, Zone::Library, db),
                owner,
            )
        })
        .collect();
    for _ in 0..6 {
        scenario.add_real_card(P0, "Island", Zone::Library, db);
    }
    scenario.with_mana_pool(
        P0,
        [
            (ManaType::Colorless, 6),
            (ManaType::Red, 1),
            (ManaType::Green, 1),
        ]
        .into_iter()
        .flat_map(|(color, n)| (0..n).map(move |_| ManaUnit::new(color, svella, false, vec![])))
        .collect(),
    );
    let mut runner = scenario.build();
    let state = runner.state();
    assert_eq!(
        state
            .library_of(P0)
            .iter()
            .take(4)
            .copied()
            .collect::<Vec<_>>(),
        top_four.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        "reach: the staged cards are the top four of P0's library"
    );
    let _ = runner
        .activate(svella, SVELLA_LOOK_ABILITY)
        .accept_optional()
        .resolve();
    Peek { runner, top_four }
}

fn offered(peek: &Peek) -> Vec<ObjectId> {
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = &peek.runner.state().waiting_for else {
        panic!("the peek must park its library cast choice");
    };
    assert_eq!(*zone, Zone::Library);
    cards.clone()
}

fn id_of(peek: &Peek, index: usize) -> ObjectId {
    peek.top_four[index].0
}

fn library(peek: &Peek) -> Vec<ObjectId> {
    peek.runner.state().library_of(P0).iter().copied().collect()
}

/// The three spells among the four looked-at cards (indices 0, 2, 3); the Island is a land and
/// is never cast.
const SPELLS: [usize; 3] = [0, 2, 3];

#[test]
fn dandan_every_looked_at_spell_is_offered_whoever_owns_it() {
    if shared_card_db().is_none() {
        return;
    }
    let p = peek(FormatConfig::dandan(), [P1, P1, P1, P0]);
    assert!(
        p.runner.state().players[1].library.is_empty(),
        "reach: the pile is held by the canonical seat"
    );
    let mut expected: Vec<_> = SPELLS.iter().map(|&i| id_of(&p, i)).collect();
    let mut got = offered(&p);
    expected.sort();
    got.sort();
    assert_eq!(got, expected, "P1-owned spells are P0's library cards too");
}

#[test]
fn dandan_casting_an_opponent_owned_hit_bottoms_every_other_looked_at_card() {
    if shared_card_db().is_none() {
        return;
    }
    let mut p = peek(FormatConfig::dandan(), [P1, P1, P1, P0]);
    let chosen = id_of(&p, 0);
    p.runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("choosing a P1-owned hit must succeed");
    assert_eq!(p.runner.state().objects[&chosen].zone, Zone::Stack);
    assert_eq!(
        p.runner.state().objects[&chosen].owner,
        P0,
        "the caster owns the spell it cast"
    );
    let lib = library(&p);
    let tail: Vec<_> = lib[lib.len() - 3..].to_vec();
    let mut rest: Vec<_> = (1..4).map(|i| id_of(&p, i)).collect();
    let mut tail_sorted = tail.clone();
    rest.sort();
    tail_sorted.sort();
    assert_eq!(
        tail_sorted, rest,
        "the unchosen looked-at cards are the bottom three"
    );
}

#[test]
fn dandan_declining_bottoms_all_four_looked_at_cards() {
    if shared_card_db().is_none() {
        return;
    }
    let mut p = peek(FormatConfig::dandan(), [P1, P1, P1, P0]);
    p.runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the cast must succeed");
    let lib = library(&p);
    let mut tail: Vec<_> = lib[lib.len() - 4..].to_vec();
    let mut looked: Vec<_> = (0..4).map(|i| id_of(&p, i)).collect();
    tail.sort();
    looked.sort();
    assert_eq!(tail, looked, "all four looked-at cards moved to the bottom");
}

#[test]
fn standard_offers_the_controllers_own_spells_and_leaves_the_opponent_library_alone() {
    if shared_card_db().is_none() {
        return;
    }
    let mut p = peek(FormatConfig::standard(), [P0, P0, P0, P0]);
    let mut expected: Vec<_> = SPELLS.iter().map(|&i| id_of(&p, i)).collect();
    let mut got = offered(&p);
    expected.sort();
    got.sort();
    assert_eq!(got, expected, "reach: the three spells are offered");
    let opponent_library: Vec<_> = p.runner.state().library_of(P1).iter().copied().collect();
    p.runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the cast must succeed");
    assert_eq!(
        p.runner
            .state()
            .library_of(P1)
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        opponent_library,
        "P1's own library is not part of P0's look"
    );
}

mod top_cast {
    use super::*;
    use engine::game::casting::{
        can_cast_object_now, effective_spell_cost, spell_objects_available_to_cast,
    };

    fn other(seat: PlayerId) -> PlayerId {
        if seat == P0 {
            P1
        } else {
            P0
        }
    }

    /// `holder` controls `permanent` ("Future Sight" or "Bolas's Citadel"); the top card of the
    /// library `holder` reads is Mental Note owned by `top_owner`, over a filler Island.
    fn staged(
        format: FormatConfig,
        holder: PlayerId,
        permanent: &str,
        top_owner: PlayerId,
        mana: bool,
    ) -> (GameRunner, ObjectId) {
        let db = shared_card_db().expect("card db");
        let mut scenario = GameScenario::new_with_format(format, 2, 11);
        scenario.at_phase(Phase::PreCombatMain);
        let top = scenario.add_real_card(top_owner, "Mental Note", Zone::Library, db);
        scenario.add_real_card(top_owner, "Island", Zone::Library, db);
        scenario.add_real_card(holder, permanent, Zone::Battlefield, db);
        scenario.with_life(holder, 20);
        if mana {
            scenario.with_mana_pool(
                holder,
                (0..3)
                    .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                    .collect(),
            );
        }
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = holder;
        state.priority_player = holder;
        state.waiting_for = WaitingFor::Priority { player: holder };
        (runner, top)
    }

    fn assert_top_is_offered_to(runner: &GameRunner, holder: PlayerId, top: ObjectId) {
        let state = runner.state();
        assert!(
            spell_objects_available_to_cast(state, holder).contains(&top),
            "the pile top is a candidate cast"
        );
        assert!(can_cast_object_now(state, holder, top));
        assert!(
            engine::ai_support::legal_actions(state)
                .iter()
                .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == top)),
            "legal actions offer the cast"
        );
    }

    #[test]
    fn dandan_future_sight_casts_the_pile_top_whoever_owns_it() {
        if shared_card_db().is_none() {
            return;
        }
        for holder in [P0, P1] {
            let owner = other(holder);
            let (mut runner, top) =
                staged(FormatConfig::dandan(), holder, "Future Sight", owner, true);
            let state = runner.state();
            assert_eq!(
                state.library_of(holder).front(),
                Some(&top),
                "reach: the top of the pile the holder reads"
            );
            assert_eq!(
                state.objects[&top].owner, owner,
                "reach: the opponent owns it"
            );
            assert_top_is_offered_to(&runner, holder, top);
            runner.cast(top).resolve();
            assert_ne!(
                runner.state().objects[&top].zone,
                Zone::Library,
                "{holder:?} cast it from the top"
            );
        }
    }

    #[test]
    fn dandan_citadel_pays_life_for_a_pile_top_the_opponent_owns() {
        if shared_card_db().is_none() {
            return;
        }
        for holder in [P0, P1] {
            let (mut runner, top) = staged(
                FormatConfig::dandan(),
                holder,
                "Bolas's Citadel",
                other(holder),
                false,
            );
            let state = runner.state();
            assert_eq!(
                state.library_of(holder).front(),
                Some(&top),
                "reach: pile top"
            );
            assert!(
                state.players[holder.0 as usize].mana_pool.mana.is_empty(),
                "reach: no mana to pay with"
            );
            assert!(
                effective_spell_cost(state, holder, top)
                    .expect("effective cost")
                    .is_without_paying_mana(),
                "the life rider replaces the mana cost"
            );
            assert_top_is_offered_to(&runner, holder, top);
            runner.cast(top).resolve();
            let state = runner.state();
            assert_ne!(state.objects[&top].zone, Zone::Library);
            assert_eq!(
                state.players[holder.0 as usize].life, 19,
                "{holder:?} paid life equal to the mana value"
            );
        }
    }

    #[test]
    fn standard_casts_only_the_holders_own_library_top() {
        if shared_card_db().is_none() {
            return;
        }
        let (mut runner, own_top) = staged(FormatConfig::standard(), P0, "Future Sight", P0, true);
        assert_top_is_offered_to(&runner, P0, own_top);
        runner.cast(own_top).resolve();
        assert_ne!(runner.state().objects[&own_top].zone, Zone::Library);

        let (runner, opponent_top) = staged(FormatConfig::standard(), P0, "Future Sight", P1, true);
        assert_eq!(
            runner.state().library_of(P1).front(),
            Some(&opponent_top),
            "reach: the card is the top of the opponent's own library"
        );
        assert!(
            !spell_objects_available_to_cast(runner.state(), P0).contains(&opponent_top),
            "P0's permission does not reach P1's library"
        );
        assert!(!can_cast_object_now(runner.state(), P0, opponent_top));
    }
}

mod top_plot {
    use super::*;
    use engine::game::casting::activated_ability_definitions;
    use engine::types::ability::CastingPermission;

    fn other(seat: PlayerId) -> PlayerId {
        if seat == P0 {
            P1
        } else {
            P0
        }
    }

    /// `holder` controls Fblthp and has priority, both seats can pay; the top card of the library `holder` reads is
    /// Mental Note owned by `top_owner`, over a filler Island.
    fn staged(
        format: FormatConfig,
        holder: PlayerId,
        top_owner: PlayerId,
    ) -> (GameRunner, ObjectId) {
        let db = shared_card_db().expect("card db");
        let mut scenario = GameScenario::new_with_format(format, 2, 11);
        scenario.at_phase(Phase::PreCombatMain);
        let top = scenario.add_real_card(top_owner, "Mental Note", Zone::Library, db);
        scenario.add_real_card(top_owner, "Island", Zone::Library, db);
        scenario.add_real_card(holder, "Fblthp, Lost on the Range", Zone::Battlefield, db);
        for seat in [P0, P1] {
            scenario.with_mana_pool(
                seat,
                (0..3)
                    .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                    .collect(),
            );
        }
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = holder;
        state.priority_player = holder;
        state.waiting_for = WaitingFor::Priority { player: holder };
        (runner, top)
    }

    fn plot_index(runner: &GameRunner, top: ObjectId) -> Option<usize> {
        activated_ability_definitions(runner.state(), top)
            .into_iter()
            .find(|(_, def)| def.activation_zone == Some(Zone::Library))
            .map(|(i, _)| i)
    }

    fn offers_plot(runner: &GameRunner, top: ObjectId) -> bool {
        engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(
                |a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == top),
            )
    }

    #[test]
    fn dandan_fblthp_plots_the_pile_top_whoever_owns_it() {
        if shared_card_db().is_none() {
            return;
        }
        for holder in [P0, P1] {
            let owner = other(holder);
            let (mut runner, top) = staged(FormatConfig::dandan(), holder, owner);
            let state = runner.state();
            assert_eq!(
                state.library_of(holder).front(),
                Some(&top),
                "reach: the top of the pile the holder reads"
            );
            assert_eq!(
                state.objects[&top].owner, owner,
                "reach: the opponent owns it"
            );
            let index = plot_index(&runner, top).expect("the pile top carries the plot ability");
            assert!(offers_plot(&runner, top), "legal actions offer the plot");
            runner
                .act(GameAction::ActivateAbility {
                    source_id: top,
                    ability_index: index,
                })
                .expect("the holder plots the pile top");
            let plotted = &runner.state().objects[&top];
            assert_eq!(
                plotted.zone,
                Zone::Exile,
                "{holder:?} exiled it from the pile"
            );
            assert!(plotted
                .casting_permissions
                .iter()
                .any(|p| matches!(p, CastingPermission::Plotted { .. })));
        }
    }

    #[test]
    fn dandan_plot_is_not_granted_to_the_non_holder_owner() {
        if shared_card_db().is_none() {
            return;
        }
        let (mut runner, top) = staged(FormatConfig::dandan(), P0, P1);
        assert!(
            offers_plot(&runner, top),
            "reach: the holder is offered the plot"
        );
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        assert!(!offers_plot(&runner, top), "P1 controls no Fblthp");
        let index = plot_index(&runner, top).unwrap_or(0);
        assert!(
            runner
                .act(GameAction::ActivateAbility {
                    source_id: top,
                    ability_index: index,
                })
                .is_err(),
            "the owner of the pile top cannot plot it without the permission"
        );
        assert_eq!(runner.state().objects[&top].zone, Zone::Library);
    }

    #[test]
    fn standard_plots_only_the_holders_own_library_top() {
        if shared_card_db().is_none() {
            return;
        }
        let (mut runner, own_top) = staged(FormatConfig::standard(), P0, P0);
        let index = plot_index(&runner, own_top).expect("own top carries the plot ability");
        assert!(offers_plot(&runner, own_top));
        runner
            .act(GameAction::ActivateAbility {
                source_id: own_top,
                ability_index: index,
            })
            .expect("the holder plots its own top");
        assert_eq!(runner.state().objects[&own_top].zone, Zone::Exile);

        let (runner, opponent_top) = staged(FormatConfig::standard(), P0, P1);
        assert_eq!(
            runner.state().library_of(P1).front(),
            Some(&opponent_top),
            "reach: the card is the top of the opponent's own library"
        );
        assert!(plot_index(&runner, opponent_top).is_none());
        assert!(!offers_plot(&runner, opponent_top));
    }
}
