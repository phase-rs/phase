//! Issue #6517 (Dauthi Voidwalker) — the void pipeline end to end:
//! the graveyard-replacement exiles an opponent-owned card WITH its void
//! counter, and the {T}+sacrifice ability's "Choose an exiled card an
//! opponent owns with a void counter on it" pick (previously `Unimplemented`)
//! feeds the chained free-play permission.
//!
//! REVERT DISCRIMINATORS:
//! - without `try_parse_choose_exiled_card_with_counter` the activation's
//!   parent effect is `Unimplemented` — no `ChooseFromZoneChoice` ever
//!   appears and the free cast is refused;
//! - the replacement tests pin the #5443 behavior (exile + counter) that
//!   #6517 reports as invisible, separating engine truth from display.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const VOIDWALKER: &str = "If a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.";
const MURDER: &str = "Destroy target creature.";

fn void_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object)
        .and_then(|card| {
            card.counters
                .get(&CounterType::Generic("void".to_string()))
                .copied()
        })
        .unwrap_or(0)
}

fn zone_of(runner: &GameRunner, object: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&object)
        .expect("object exists")
        .zone
}

fn voidwalker_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, VOIDWALKER)
        .id();
    // Nonzero cost + P0's empty mana pool: the later cast from exile can
    // only succeed through the free-play permission, not by paying (review).
    let orc = scenario
        .add_creature(P1, "Doomed Orc", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let murder = scenario
        .add_spell_to_hand(P0, "Test Murder", false)
        .from_oracle_text(MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();
    runner.cast(murder).target_object(orc).resolve();
    (runner, walker, orc)
}

#[test]
fn an_opponents_dying_creature_is_exiled_with_a_void_counter() {
    let (runner, _, orc) = voidwalker_board();

    assert_eq!(
        zone_of(&runner, orc),
        Zone::Exile,
        "the replacement must exile instead of the graveyard move"
    );
    assert_eq!(
        void_counters(&runner, orc),
        1,
        "the exiled card must carry its void counter (engine truth for #6517's 'invisible counter')"
    );
    assert!(
        !runner.state().players[1].graveyard.contains(&orc),
        "the orc must never reach P1's graveyard"
    );
}

#[test]
fn your_own_dying_creature_still_goes_to_the_graveyard() {
    // "an opponent's graveyard" — the controller's own cards are untouched.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, VOIDWALKER)
        .id();
    let own = scenario.add_creature(P0, "Own Bear", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand(P0, "Test Murder", false)
        .from_oracle_text(MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();
    runner.cast(murder).target_object(own).resolve();

    assert_eq!(zone_of(&runner, own), Zone::Graveyard);
    assert_eq!(void_counters(&runner, own), 0);
}

/// Drive the activation to a settled empty stack; answers cost and pick
/// prompts, and verifies every expected card was offered before selecting `pick`.
fn drive_activation(
    runner: &mut GameRunner,
    walker: ObjectId,
    expected_cards: &[ObjectId],
    pick: ObjectId,
) -> bool {
    let mut pick_seen = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![walker],
                    })
                    .expect("paying the sacrifice cost must succeed");
            }
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                for expected in expected_cards {
                    assert!(
                        cards.contains(expected),
                        "every expected card must be offered, got {cards:?}"
                    );
                }
                pick_seen = true;
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("picking the exiled card must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return pick_seen;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority must be accepted mid-drive");
            }
            other => panic!("unexpected prompt during the activation: {other:?}"),
        }
    }
    panic!("activation never settled within 64 steps");
}

#[test]
fn a_sole_void_card_still_prompts_and_plays_for_free() {
    // CR 608.2d: the controller announces the choice while applying the
    // effect — this seam surfaces the `ChooseFromZoneChoice` even for a
    // single legal candidate (no auto-pick path exists for it). Pinning that
    // the prompt APPEARS is the regression guard against the original bug,
    // where the pick was silently skipped; the chosen card must then play
    // for free.
    let (mut runner, walker, orc) = voidwalker_board();

    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
        })
        .expect("activating the {T}+sacrifice ability must succeed");
    let pick_seen = drive_activation(&mut runner, walker, &[orc], orc);
    assert!(
        pick_seen,
        "the pick prompt must be offered even for a single legal candidate — \
         its absence is the original silent-skip bug"
    );

    assert_eq!(
        zone_of(&runner, walker),
        Zone::Graveyard,
        "the sacrificed Voidwalker (own card) goes to its owner's graveyard"
    );
    runner.cast(orc).resolve();
    assert_eq!(
        zone_of(&runner, orc),
        Zone::Battlefield,
        "the exiled card must be playable this turn without paying its cost"
    );
}

#[test]
fn with_two_void_cards_the_pick_is_offered_and_the_choice_plays_free() {
    // Two void-countered cards make the pick a real choice — the interactive
    // `ChooseFromZoneChoice` prompt must appear and the CHOSEN card plays free.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, VOIDWALKER)
        .id();
    // Nonzero costs + P0's empty mana pool: only the free-play permission
    // can carry the chosen cast (review).
    let orc = scenario
        .add_creature(P1, "Doomed Orc", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let grunt = scenario
        .add_creature(P1, "Doomed Grunt", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let murder_a = scenario
        .add_spell_to_hand(P0, "Test Murder A", false)
        .from_oracle_text(MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let murder_b = scenario
        .add_spell_to_hand(P0, "Test Murder B", false)
        .from_oracle_text(MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();
    runner.cast(murder_a).target_object(orc).resolve();
    runner.cast(murder_b).target_object(grunt).resolve();
    assert_eq!(zone_of(&runner, orc), Zone::Exile);
    assert_eq!(zone_of(&runner, grunt), Zone::Exile);

    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
        })
        .expect("activating the {T}+sacrifice ability must succeed");
    let pick_seen = drive_activation(&mut runner, walker, &[orc, grunt], orc);

    assert!(
        pick_seen,
        "two candidates must produce the interactive pick"
    );
    runner.cast(orc).resolve();
    assert_eq!(
        zone_of(&runner, orc),
        Zone::Battlefield,
        "the chosen card must be playable for free"
    );
    assert_eq!(
        zone_of(&runner, grunt),
        Zone::Exile,
        "the unchosen card stays exiled"
    );
}

#[test]
fn a_later_opponents_card_is_offered_in_three_player() {
    // CR 102.3: in multiplayer every player not on the controller's team is
    // an opponent, so the pool must span ALL opponents' exile partitions.
    // P1 (the first opponent) owns nothing eligible; P2 owns the only
    // void-countered card. REVERT DISCRIMINATOR: a single-owner scope
    // (`ZoneOwner::Opponent`) resolves to `players::opponents(...).next()`
    // = P1 alone, finds zero candidates, and silently skips the pick — this
    // test then fails at the prompt assertion.
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, VOIDWALKER)
        .id();
    // Nonzero cost + P0's empty mana pool: the free play must carry the cast.
    let far_orc = scenario
        .add_creature(P2, "Far Orc", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let murder = scenario
        .add_spell_to_hand(P0, "Test Murder", false)
        .from_oracle_text(MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();
    runner.cast(murder).target_object(far_orc).resolve();
    assert_eq!(
        zone_of(&runner, far_orc),
        Zone::Exile,
        "the replacement must exile the later opponent's dying creature"
    );
    assert_eq!(void_counters(&runner, far_orc), 1);

    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
        })
        .expect("activating the {T}+sacrifice ability must succeed");
    let pick_seen = drive_activation(&mut runner, walker, &[far_orc], far_orc);
    assert!(
        pick_seen,
        "the later opponent's card must be offered — a first-opponent-only \
         scan finds zero candidates and silently skips the pick"
    );
    runner.cast(far_orc).resolve();
    assert_eq!(
        zone_of(&runner, far_orc),
        Zone::Battlefield,
        "the later opponent's card must be playable for free"
    );
}
