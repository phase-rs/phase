//! Production-path tests for casting a spell from the cards just milled.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{TargetRef, ThisWayCause};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const SUMMONS: &str = "Amass Orcs X. Mill X cards. You may cast an instant or sorcery spell with mana value X or less from among them without paying its mana cost. (To amass Orcs X, put X +1/+1 counters on an Army you control. It's also an Orc. If you don't control an Army, create a 0/0 black Orc Army creature token first.)\nFlashback—{3}{U}{R}, Exile X cards from your graveyard.";
const JACE: &str = "Flying\nWhen this creature enters, target opponent mills five cards. You may cast an instant or sorcery spell from among them without paying its mana cost.";

fn scenario(players: u8, seed: u64) -> GameScenario {
    let mut board = GameScenario::new_n_player(players, seed);
    board.at_phase(Phase::PreCombatMain);
    board
}

fn mana(board: &mut GameScenario, types: impl IntoIterator<Item = ManaType>) {
    board.with_mana_pool(
        P0,
        types
            .into_iter()
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn land(board: &mut GameScenario, player: PlayerId) -> ObjectId {
    board.add_land_to_library_top(player, "Forest").id()
}

fn opt(board: &mut GameScenario, player: PlayerId) -> ObjectId {
    board
        .add_spell_to_library_top(player, "Opt", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text(
            "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)
Draw a card.",
        )
        .id()
}

fn jaces_ingenuity(board: &mut GameScenario, player: PlayerId) -> ObjectId {
    board
        .add_spell_to_library_top(player, "Jace's Ingenuity", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue; 2],
        })
        .from_oracle_text("Draw three cards.")
        .id()
}

fn concentrate(board: &mut GameScenario, player: PlayerId) -> ObjectId {
    board
        .add_spell_to_library_top(player, "Concentrate", false)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue; 2],
        })
        .from_oracle_text("Draw three cards.")
        .id()
}

fn summons_in_hand(board: &mut GameScenario) -> ObjectId {
    board
        .add_spell_to_hand_from_oracle(P0, "Summons of Saruman", false, SUMMONS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue, ManaCostShard::Red],
            generic: 0,
        })
        .id()
}

fn mindseeker_in_hand(board: &mut GameScenario) -> ObjectId {
    board
        .add_creature_to_hand(P0, "Jace's Mindseeker", 4, 4)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Blue; 2],
        })
        .from_oracle_text_with_keywords(&["Flying"], JACE)
        .id()
}

fn mindseeker_mana() -> impl Iterator<Item = ManaType> {
    [ManaType::Blue; 2]
        .into_iter()
        .chain([ManaType::Colorless; 4])
}

fn same_cards(actual: &[ObjectId], expected: &[ObjectId], context: &str) {
    let mut actual = actual.to_vec();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "{context}");
}

// CR 608.2c: only pass while the stack is still resolving. A priority
// window after cleanup is observable before the selected spell resolves.
fn pass_until_prompt(runner: &mut GameRunner) {
    for _ in 0..24 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass during resolution");
    }
    panic!("resolution did not reach a prompt or finish");
}

fn open_optional(runner: &mut GameRunner) {
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("open the optional cast window");
    }
}

fn cast_offer(runner: &GameRunner, expected: &[ObjectId]) {
    let candidates = match &runner.state().waiting_for {
        WaitingFor::CastOffer {
            player: P0,
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
        }
        | WaitingFor::EffectZoneChoice {
            player: P0,
            cards: candidates,
            zone: Zone::Exile | Zone::Graveyard,
            ..
        } => candidates,
        other => panic!(
            "expected an immediate free-cast offer for {expected:?}, got {}",
            other.variant_name()
        ),
    };
    same_cards(
        candidates,
        expected,
        "the immediate offer must contain exactly the eligible cards in this batch",
    );
}

fn select(runner: &mut GameRunner, cards: Vec<ObjectId>) {
    runner
        .act(GameAction::SelectCards { cards })
        .expect("select the resolution cards");
}

fn choose_cast(runner: &mut GameRunner, selection: Option<ObjectId>) {
    match runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { .. } => select(runner, selection.into_iter().collect()),
        _ => {
            runner
                .act(GameAction::FreeCastWindowChoice { selection })
                .expect("answer the free-cast offer");
        }
    }
}

fn assert_priority(runner: &GameRunner, stack_size: usize) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected priority after the chain, got {}",
        runner.state().waiting_for.variant_name()
    );
    assert_eq!(
        runner.state().stack.len(),
        stack_size,
        "unexpected number of spells left on the stack"
    );
}
fn resolve_targeted_etb(runner: &mut GameRunner, permanent: ObjectId, target: PlayerId) {
    runner.cast(permanent).commit();
    pass_until_prompt(runner);
    let WaitingFor::TriggerTargetSelection {
        player: P0,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected an ETB target choice, got {}",
            runner.state().waiting_for.variant_name()
        );
    };
    // CR 115.1d: the chosen opponent must be a legal target of the ETB.
    let opponent = TargetRef::Player(target);
    assert!(
        target_slots[selection.current_slot]
            .legal_targets
            .contains(&opponent),
        "requested opponent must be a legal trigger target"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(opponent),
        })
        .expect("choose ETB opponent");
    pass_until_prompt(runner);
}

fn old_graveyard_spell(board: &mut GameScenario, player: PlayerId) -> ObjectId {
    board
        .add_spell_to_graveyard(player, "Divination", false)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text("Draw two cards.")
        .id()
}

// CR 202.3 + CR 608.2g: X gates the milled spells by mana value and the
// selected free spell goes onto the stack during the original resolution.
fn summons_mills_and_offers_one_spell(accept: bool) {
    let mut board = scenario(2, 42);
    let summons = summons_in_hand(&mut board);
    mana(
        &mut board,
        [ManaType::Blue, ManaType::Red]
            .into_iter()
            .chain([ManaType::Colorless; 4]),
    );
    let older = old_graveyard_spell(&mut board, P0);
    let deeper = concentrate(&mut board, P0);
    let creature = board
        .add_spell_to_library_top(P0, "Grizzly Bears", false)
        .as_creature()
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let expensive = jaces_ingenuity(&mut board, P0);
    let sorcery = concentrate(&mut board, P0);
    let instant = opt(&mut board, P0);
    let mut runner = board.build();
    runner
        .cast(summons)
        .x(4)
        .commit()
        .stop_at_optional()
        .resolve();
    for card in [instant, sorcery, creature, expensive] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Graveyard,
            "X=4 must mill exactly the staged batch"
        );
    }
    assert!(
        runner.state().objects.values().any(|obj| {
            obj.zone == Zone::Battlefield
                && obj.is_token
                && obj
                    .counters
                    .get(&engine::types::counter::CounterType::Plus1Plus1)
                    == Some(&4)
        }),
        "amass must create an Army before the cast offer"
    );
    open_optional(&mut runner);
    cast_offer(&runner, &[instant, sorcery]);
    choose_cast(&mut runner, accept.then_some(sorcery));
    assert_priority(&runner, usize::from(accept));
    assert_eq!(
        runner.state().objects[&sorcery].zone,
        if accept { Zone::Stack } else { Zone::Graveyard },
        "the offered spell moves to the stack only when chosen"
    );
    assert_eq!(
        runner.state().objects[&instant].zone,
        Zone::Graveyard,
        "the second eligible spell must remain milled"
    );
    assert_eq!(runner.state().objects[&older].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&deeper].zone, Zone::Library);
}

#[test]
fn summons_offers_only_eligible_milled_spells_and_casts_one() {
    summons_mills_and_offers_one_spell(true);
}

#[test]
fn summons_can_decline_the_milled_spells() {
    summons_mills_and_offers_one_spell(false);
}

#[test]
fn mindseeker_offers_only_spells_from_its_opponents_mill_batch() {
    let mut board = scenario(3, 42);
    let untargeted = opt(&mut board, P1);
    let jace = mindseeker_in_hand(&mut board);
    mana(&mut board, mindseeker_mana());
    let older = old_graveyard_spell(&mut board, P2);
    let deeper = opt(&mut board, P2);
    let instant = jaces_ingenuity(&mut board, P2);
    let sorcery = concentrate(&mut board, P2);
    let lands: Vec<_> = (0..3).map(|_| land(&mut board, P2)).collect();
    let mut runner = board.build();
    resolve_targeted_etb(&mut runner, jace, P2);
    for card in lands {
        assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
    }
    open_optional(&mut runner);
    // CR 608.2c + CR 608.2g: only spells from the targeted opponent's mill batch qualify.
    cast_offer(&runner, &[instant, sorcery]);
    choose_cast(&mut runner, Some(instant));
    assert_priority(&runner, 1);
    assert_eq!(runner.state().objects[&instant].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&sorcery].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&older].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&deeper].zone, Zone::Library);
    assert_eq!(runner.state().objects[&untargeted].zone, Zone::Library);
}

#[test]
fn mindseeker_offers_replacement_exiled_milled_spells() {
    let mut board = scenario(3, 42);
    let rest_in_peace = board
        .add_enchantment_to_hand_from_oracle(
            P0,
            "Rest in Peace",
            "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let jace = mindseeker_in_hand(&mut board);
    mana(
        &mut board,
        mindseeker_mana().chain([ManaType::White, ManaType::Colorless]),
    );
    let older = old_graveyard_spell(&mut board, P1);
    let deeper = concentrate(&mut board, P1);
    let sorcery = concentrate(&mut board, P1);
    let lands: Vec<_> = (0..4).map(|_| land(&mut board, P1)).collect();
    let mut runner = board.build();
    runner.cast(rest_in_peace).resolve();
    assert_priority(&runner, 0);
    assert_eq!(runner.state().objects[&older].zone, Zone::Exile);
    resolve_targeted_etb(&mut runner, jace, P1);
    // CR 701.17c: the cast can find a milled card in the public replacement destination.
    for card in lands.into_iter().chain([sorcery]) {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    }
    open_optional(&mut runner);
    cast_offer(&runner, &[sorcery]);
    choose_cast(&mut runner, Some(sorcery));
    assert_priority(&runner, 1);
    assert_eq!(runner.state().objects[&sorcery].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&deeper].zone, Zone::Library);
}

#[test]
fn summons_at_zero_does_not_offer_a_prior_mill_batch() {
    let mut board = scenario(3, 42);
    let jace = mindseeker_in_hand(&mut board);
    let summons = summons_in_hand(&mut board);
    mana(
        &mut board,
        mindseeker_mana().chain([ManaType::Blue, ManaType::Red]),
    );
    let untouched = opt(&mut board, P0);
    let stale = board
        .add_spell_to_library_top(P1, "Ancestral Vision", false)
        .with_mana_cost(ManaCost::NoCost)
        .from_oracle_text("Suspend 4—{U} (Rather than cast this card from your hand, pay {U} and exile it with four time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\nTarget player draws three cards.")
        .id();
    let mut runner = board.build();
    resolve_targeted_etb(&mut runner, jace, P1);
    open_optional(&mut runner);
    cast_offer(&runner, &[stale]);
    choose_cast(&mut runner, None);
    assert_priority(&runner, 0);
    assert!(
        runner
            .state()
            .tracked_set_member_causes
            .values()
            .any(|causes| { causes.get(&stale) == Some(&ThisWayCause::Milled) }),
        "prior cast window must leave a real, milled tracked-set member"
    );

    runner
        .cast(summons)
        .x(0)
        .commit()
        .stop_at_optional()
        .resolve();
    open_optional(&mut runner);
    assert_priority(&runner, 0);
    assert_eq!(runner.state().objects[&stale].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&untouched].zone, Zone::Library);
}

#[test]
fn mindseeker_offers_only_the_eligible_modal_spell_face() {
    let mut board = scenario(3, 42);
    let jace = mindseeker_in_hand(&mut board);
    mana(&mut board, mindseeker_mana());
    let db = shared_card_db().expect("printed MDFCs must be available in the fixture");
    let extus = board.add_real_card(P1, "Extus, Oriq Overlord", Zone::Library, db);
    let florahedron = board.add_real_card(P1, "Tangled Florahedron", Zone::Library, db);
    let mut runner = board.build();
    let library = runner.state().library_of(P1);
    assert_eq!(library.back(), Some(&florahedron));
    assert_eq!(library.iter().rev().nth(1), Some(&extus));

    resolve_targeted_etb(&mut runner, jace, P1);
    assert_eq!(runner.state().objects[&extus].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&florahedron].zone, Zone::Graveyard);
    open_optional(&mut runner);
    // CR 712.11c: the allowed spell type is evaluated on the chosen face.
    cast_offer(&runner, &[extus]);
    choose_cast(&mut runner, Some(extus));
    // CR 118.8b + CR 118.9d: the free cast still offers the back face's optional sacrifice.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::DecideOptionalCost { index: 1 })
        .expect("cast Awaken the Blood Avatar without sacrificing a creature");
    assert_priority(&runner, 1);
    let cast = &runner.state().objects[&extus];
    assert_eq!(cast.zone, Zone::Stack);
    assert_eq!(cast.name, "Awaken the Blood Avatar");
    assert_eq!(runner.state().objects[&florahedron].zone, Zone::Graveyard);
}
