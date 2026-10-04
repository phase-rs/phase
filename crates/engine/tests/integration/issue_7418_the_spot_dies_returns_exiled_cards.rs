//! Issue #7418 — The Spot's dies trigger returns the cards its enters trigger exiled.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::{ExileLinkKind, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const THE_SPOT: &str = "When The Spot enters, exile up to one target nonland permanent and up to one target nonland permanent card from a graveyard.\nWhen The Spot dies, put him on the bottom of his owner's library. If you do, return the exiled cards to their owners' hands.";

const GRAVEGOUGER: &str = "When this creature enters, exile up to two target cards from a single graveyard.\nWhen this creature leaves the battlefield, return the exiled cards to their owner's graveyard.";

const COPY_CREATURE: &str = "Create a token that's a copy of target creature you control.";
const DESTROY: &str = "Destroy target creature.";
const BOUNCE: &str = "Return target creature to its owner's hand.";

struct SpotBoard {
    runner: GameRunner,
    spot: ObjectId,
    p1_bf: ObjectId,
    p0_gy: ObjectId,
    bystander: ObjectId,
    library_seed: ObjectId,
    destroy: ObjectId,
    bounce: ObjectId,
}

fn spot_board() -> SpotBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spot = scenario
        .add_creature_to_hand_from_oracle(P0, "The Spot, Living Portal", 0, 3, THE_SPOT)
        .id();
    let p1_bf = scenario.add_creature(P1, "Battlefield Bear", 2, 2).id();
    let p0_gy = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .id();
    let bystander = scenario
        .add_creature_to_exile(P1, "Exiled Bystander", 1, 1)
        .id();
    let library_seed = scenario.add_card_to_library_top(P0, "Library Seed");
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Destroy Spell", false, DESTROY)
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Bounce Spell", false, BOUNCE)
        .id();
    SpotBoard {
        runner: scenario.build(),
        spot,
        p1_bf,
        p0_gy,
        bystander,
        library_seed,
        destroy,
        bounce,
    }
}

fn zone(state: &GameState, id: ObjectId) -> Zone {
    state.objects.get(&id).expect("object exists").zone
}

fn in_hand(state: &GameState, player: PlayerId, id: ObjectId) -> bool {
    state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .expect("player")
        .hand
        .contains(&id)
}

fn tracked_by_source(state: &GameState, source: ObjectId, exiled: ObjectId) -> bool {
    state.exile_links.iter().any(|link| {
        link.source_id == source
            && link.exiled_id == exiled
            && link.kind == ExileLinkKind::TrackedBySource
    })
}

fn assert_spot_exiled_both(state: &GameState, source: ObjectId, p1_bf: ObjectId, p0_gy: ObjectId) {
    assert_eq!(zone(state, p1_bf), Zone::Exile, "permanent was exiled");
    assert_eq!(zone(state, p0_gy), Zone::Exile, "graveyard card was exiled");
    assert!(
        tracked_by_source(state, source, p1_bf),
        "permanent is linked to the exiling source"
    );
    assert!(
        tracked_by_source(state, source, p0_gy),
        "graveyard card is linked to the exiling source"
    );
}

/// CR 700.4 + CR 603.10a + CR 400.3: dying returns the linked exiled cards to
/// their owners' hands and puts The Spot on the bottom of its owner's library.
#[test]
fn the_spot_dies_returns_etb_exiled_cards_to_owners_hands() {
    let SpotBoard {
        mut runner,
        spot,
        p1_bf,
        p0_gy,
        bystander,
        library_seed,
        destroy,
        ..
    } = spot_board();

    runner.cast(spot).target_objects(&[p1_bf, p0_gy]).resolve();
    assert_spot_exiled_both(runner.state(), spot, p1_bf, p0_gy);

    runner.cast(destroy).target_objects(&[spot]).resolve();
    let state = runner.state();
    assert_eq!(zone(state, p1_bf), Zone::Hand);
    assert!(
        in_hand(state, P1, p1_bf),
        "opponent's permanent returns to its owner"
    );
    assert_eq!(zone(state, p0_gy), Zone::Hand);
    assert!(
        in_hand(state, P0, p0_gy),
        "graveyard card returns to its owner"
    );
    assert_eq!(zone(state, spot), Zone::Library);
    let library = &state.players[P0.0 as usize].library;
    assert_eq!(library.front().copied(), Some(library_seed));
    assert_eq!(library.back().copied(), Some(spot));
    assert!(!in_hand(state, P0, spot));
    assert!(!in_hand(state, P1, spot));
    assert_eq!(zone(state, bystander), Zone::Exile);
    assert!(
        state.exile_links.iter().all(|link| link.source_id != spot),
        "returned links are consumed"
    );
}

/// CR 700.4: leaving for a hand is not dying, so the linked cards stay exiled.
#[test]
fn the_spot_bounced_leaves_exiled_cards_in_exile() {
    let SpotBoard {
        mut runner,
        spot,
        p1_bf,
        p0_gy,
        bounce,
        ..
    } = spot_board();

    runner.cast(spot).target_objects(&[p1_bf, p0_gy]).resolve();
    assert_spot_exiled_both(runner.state(), spot, p1_bf, p0_gy);

    runner.cast(bounce).target_objects(&[spot]).resolve();
    let state = runner.state();
    assert!(in_hand(state, P0, spot), "The Spot was returned to hand");
    assert_eq!(zone(state, p1_bf), Zone::Exile);
    assert_eq!(zone(state, p0_gy), Zone::Exile);
}

/// CR 603.6c: leaving the battlefield returns Gravegouger's exiled cards to
/// their owner's graveyard.
#[test]
fn gravegouger_ltb_returns_exiled_cards_to_owners_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gouger = scenario
        .add_creature_to_hand_from_oracle(P0, "Gravegouger", 2, 2, GRAVEGOUGER)
        .id();
    let first = scenario
        .add_creature_to_graveyard(P1, "First Corpse", 1, 1)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P1, "Second Corpse", 1, 1)
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Bounce Spell", false, BOUNCE)
        .id();
    let mut runner = scenario.build();

    runner
        .cast(gouger)
        .target_objects(&[first, second])
        .resolve();
    assert_eq!(zone(runner.state(), first), Zone::Exile);
    assert_eq!(zone(runner.state(), second), Zone::Exile);

    runner.cast(bounce).target_objects(&[gouger]).resolve();
    let state = runner.state();
    assert_eq!(zone(state, first), Zone::Graveyard);
    assert_eq!(zone(state, second), Zone::Graveyard);
    assert!(
        state.players[P1.0 as usize].graveyard.contains(&first)
            && state.players[P1.0 as usize].graveyard.contains(&second),
        "both cards return to their owner's graveyard"
    );
}

/// CR 704.5d + CR 118.12 + CR 111.7 + CR 111.8: a token Spot dies, but it cannot
/// move to its owner's library, so the cards it exiled stay in exile.
/// CR 707.2: the token copies The Spot's rules text.
#[test]
fn token_spot_dies_leaves_exiled_cards_in_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spot = scenario
        .add_creature_from_oracle(P0, "The Spot, Living Portal", 0, 3, THE_SPOT)
        .id();
    let p1_bf = scenario.add_creature(P1, "Battlefield Bear", 2, 2).id();
    let p0_gy = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .id();
    let copy = scenario
        .add_spell_to_hand_from_oracle(P0, "Copy Spell", false, COPY_CREATURE)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Destroy Spell", false, DESTROY)
        .id();
    let mut runner = scenario.build();

    // The copy spell's only legal "creature you control" is the original Spot,
    // so it does not need to be in this list. Putting it here makes the token's
    // enters trigger exile that Spot: the resolution pool still offers it as
    // the first legal nonland permanent. These two objects are the enters
    // trigger's targets.
    runner.cast(copy).target_objects(&[p1_bf, p0_gy]).resolve();
    let token = runner
        .state()
        .objects
        .values()
        .find(|obj| {
            obj.is_token && obj.id != spot && obj.owner == P0 && obj.zone == Zone::Battlefield
        })
        .map(|obj| obj.id)
        .expect("token copy of The Spot");
    assert_spot_exiled_both(runner.state(), token, p1_bf, p0_gy);
    assert_ne!(zone(runner.state(), spot), Zone::Exile);

    runner.cast(destroy).target_objects(&[token]).resolve();
    let state = runner.state();
    assert_eq!(zone(state, p1_bf), Zone::Exile);
    assert_eq!(zone(state, p0_gy), Zone::Exile);
    assert!(!in_hand(state, P0, p1_bf));
    assert!(!in_hand(state, P1, p1_bf));
    assert!(!in_hand(state, P0, p0_gy));
    assert!(!in_hand(state, P1, p0_gy));
    assert!(
        state.objects.get(&token).is_none(),
        "the token has ceased to exist"
    );
}
