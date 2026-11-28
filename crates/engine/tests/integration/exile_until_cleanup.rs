//! Random-bottom cleanup of a resolution-local exile batch.

use std::collections::BTreeSet;

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastOfferKind, ExileLinkKind, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const GRIMA: &str = "Gríma can't be blocked.\nWhenever Gríma deals combat damage to a player, that player exiles cards from the top of their library until they exile an instant or sorcery card. You may cast that card without paying its mana cost. Then that player puts the exiled cards that weren't cast this way on the bottom of their library in a random order.";

fn scenario(players: u8, seed: u64) -> GameScenario {
    let mut board = GameScenario::new_n_player(players, seed);
    board.at_phase(Phase::PreCombatMain);
    board
}

fn land(board: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    board.add_land_to_library_top(player, name).id()
}

fn same_cards(actual: &[ObjectId], expected: &[ObjectId], context: &str) {
    let mut actual = actual.to_vec();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "{context}");
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

fn assert_bottom(
    runner: &GameRunner,
    owner: PlayerId,
    prefix: &[ObjectId],
    bottom: &[ObjectId],
) -> Vec<ObjectId> {
    let library: Vec<_> = runner.state().players[owner.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert!(
        library.starts_with(prefix),
        "unseen cards must stay on top: {library:?}, expected prefix {prefix:?}"
    );
    same_cards(
        &library[prefix.len()..],
        bottom,
        "exactly the unchosen cards must reach this library's bottom",
    );
    library[prefix.len()..].to_vec()
}

enum GrimaHit {
    Accept,
    Decline,
    Uncastable,
    Exhausted,
    EmptyLibrary,
}

const PROWLER: &str = "Vigilance
Whenever this creature enters or attacks, exile a card from a graveyard.
Spells you cast cost {1} less to cast for each card type they share with cards exiled with this creature.";
const POLYMORPH: &str =
    "Target artifact or creature becomes a copy of another target artifact or creature.";

fn add_true_polymorph(board: &mut GameScenario) -> ObjectId {
    board
        .add_spell_to_hand_from_oracle(P0, "True Polymorph", true, POLYMORPH)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id()
}

fn copy_attacker_with_older_exile(
    runner: &mut GameRunner,
    attacker: ObjectId,
    donor: ObjectId,
    polymorph: ObjectId,
    stale: ObjectId,
    name: &str,
) {
    assert!(runner
        .state()
        .exile_links
        .iter()
        .any(|link| link.source_id == attacker
            && link.exiled_id == stale
            && link.kind == ExileLinkKind::TrackedBySource));
    // CR 707.2: copying changes the attacker's abilities without moving
    // it to another zone or clearing the older exile entry.
    let incarnation = runner.state().objects[&attacker].incarnation;
    runner
        .cast(polymorph)
        .target_objects(&[attacker, donor])
        .resolve();
    assert_eq!(runner.state().objects[&attacker].name, name);
    assert_eq!(runner.state().objects[&attacker].incarnation, incarnation);
    assert!(runner
        .state()
        .exile_links
        .iter()
        .any(|link| link.source_id == attacker
            && link.exiled_id == stale
            && link.kind == ExileLinkKind::TrackedBySource));
}

fn combat_damage_cleanup(branch: GrimaHit, name: &str, oracle: &str, power: i32, toughness: i32) {
    let mut orders = BTreeSet::new();
    for seed in [4, 19, 42, 93, 101, 109, 211, 495] {
        let mut board = scenario(2, seed);
        let attacker = board
            .add_creature_from_oracle(P0, "Cemetery Prowler", 3, 4, PROWLER)
            .id();
        let copy_of = board
            .add_creature_from_oracle(P1, name, power, toughness, oracle)
            .id();
        let stale = board.add_land_to_graveyard(P1, "Forest").id();
        let polymorph = add_true_polymorph(&mut board);
        for _ in 0..6 {
            board.add_basic_land(P0, ManaColor::Blue);
        }
        let mut prefix = Vec::new();
        let hit = if matches!(branch, GrimaHit::Exhausted | GrimaHit::EmptyLibrary) {
            None
        } else {
            prefix.push(
                board
                    .add_spell_to_library_top(P1, "Consider", true)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .from_oracle_text("Surveil 1. Draw a card.")
                    .id(),
            );
            Some(if matches!(branch, GrimaHit::Uncastable) {
                board
                    .add_spell_to_library_top(P1, "Smelt", true)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Red],
                    })
                    .from_oracle_text("Destroy target artifact.")
                    .id()
            } else {
                board
                    .add_spell_to_library_top(P1, "Revitalize", true)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::White],
                    })
                    .from_oracle_text("You gain 3 life. Draw a card.")
                    .id()
            })
        };
        let mut rest: Vec<_> = if matches!(branch, GrimaHit::EmptyLibrary) {
            Vec::new()
        } else {
            ["Plains", "Swamp", "Mountain"]
                .into_iter()
                .map(|name| land(&mut board, P1, name))
                .collect()
        };
        let mut runner = board.build();
        let life_before = runner.life(P1);
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .expect("Cemetery Prowler attacks P1");
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&stale].zone, Zone::Exile);
        copy_attacker_with_older_exile(&mut runner, attacker, copy_of, polymorph, stale, name);
        runner.advance_to_phase(Phase::DeclareBlockers);
        let mut damage = runner.combat_damage();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            runner.declare_blockers(&[]).expect("no blockers");
            damage = runner.combat_damage();
        }
        assert_eq!(
            runner.life(P1),
            life_before - power,
            "the copy must deal combat damage to P1"
        );
        match branch {
            GrimaHit::Accept => {
                let hit = hit.expect("castable fixture");
                open_optional(&mut runner);
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::CastOffer { .. } | WaitingFor::EffectZoneChoice { .. }
                ) {
                    cast_offer(&runner, &[hit]);
                    choose_cast(&mut runner, Some(hit));
                }
                assert_eq!(
                    runner.state().objects[&hit].zone,
                    Zone::Stack,
                    "the hit stays on the stack through cleanup"
                );
            }
            GrimaHit::Decline => {
                let hit = hit.expect("castable fixture");
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::OptionalEffectChoice { player: P0, .. }
                ) {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: false })
                        .expect("decline Gríma's hit");
                } else {
                    cast_offer(&runner, &[hit]);
                    choose_cast(&mut runner, None);
                }
                rest.push(hit);
            }
            GrimaHit::Uncastable => {
                open_optional(&mut runner);
                rest.push(hit.expect("uncastable fixture"));
            }
            GrimaHit::Exhausted => open_optional(&mut runner),
            GrimaHit::EmptyLibrary => {
                assert!(
                    damage.events().iter().any(|event| matches!(
                        event,
                        GameEvent::EffectResolved {
                            kind: EffectKind::ExileFromTopUntil,
                            source_id,
                            ..
                        } if *source_id == attacker
                    )),
                    "the empty-library trigger must resolve"
                );
            }
        }
        assert_priority(&runner, usize::from(matches!(branch, GrimaHit::Accept)));
        // CR 608.2c: the damaged player bottoms only cards exiled by this resolution.
        assert_eq!(
            runner.state().objects[&stale].zone,
            Zone::Exile,
            "an earlier same-source card must stay exiled"
        );
        orders.insert(assert_bottom(&runner, P1, &prefix, &rest));
    }
    if !matches!(branch, GrimaHit::EmptyLibrary) {
        assert!(
            orders.len() > 1,
            "bottom placement must vary across deterministic seeds"
        );
    }
}

#[test]
fn grima_accepts_and_bottoms_only_uncast_cards() {
    combat_damage_cleanup(GrimaHit::Accept, "Gríma, Saruman's Footman", GRIMA, 1, 4);
}

#[test]
fn grima_declines_and_bottoms_including_the_hit() {
    combat_damage_cleanup(GrimaHit::Decline, "Gríma, Saruman's Footman", GRIMA, 1, 4);
}

#[test]
fn grima_bottoms_an_uncastable_hit() {
    combat_damage_cleanup(
        GrimaHit::Uncastable,
        "Gríma, Saruman's Footman",
        GRIMA,
        1,
        4,
    );
}

#[test]
fn grima_bottoms_a_fully_exhausted_library_without_a_hit() {
    combat_damage_cleanup(GrimaHit::Exhausted, "Gríma, Saruman's Footman", GRIMA, 1, 4);
}

#[test]
fn grima_empty_batch_leaves_the_older_card_exiled() {
    combat_damage_cleanup(
        GrimaHit::EmptyLibrary,
        "Gríma, Saruman's Footman",
        GRIMA,
        1,
        4,
    );
}

#[test]
fn dazzling_sphinx_returns_the_declined_hit() {
    const SPHINX: &str = "Flying
Whenever this creature deals combat damage to a player, that player exiles cards from the top of their library until they exile an instant or sorcery card. You may cast that card without paying its mana cost. Then that player puts the exiled cards that weren't cast this way on the bottom of their library in a random order.";
    combat_damage_cleanup(GrimaHit::Decline, "Dazzling Sphinx", SPHINX, 4, 5);
}

#[test]
fn chaos_wand_randomizes_the_target_opponents_exhausted_library() {
    const CHAOS_WAND: &str = "{4}, {T}: Target opponent exiles cards from the top of their library until they exile an instant or sorcery card. You may cast that card without paying its mana cost. Then put the exiled cards that weren't cast this way on the bottom of that library in a random order.";
    let mut orders = BTreeSet::new();
    for seed in [4, 19, 42, 93, 101, 109, 211, 495] {
        let mut board = scenario(2, seed);
        let wand = board
            .add_artifact_from_oracle(P0, "Chaos Wand", CHAOS_WAND)
            .id();
        for _ in 0..4 {
            board.add_basic_land(P0, ManaColor::Blue);
        }
        let untouched = land(&mut board, P0, "Island");
        let batch = [
            land(&mut board, P1, "Plains"),
            land(&mut board, P1, "Swamp"),
            land(&mut board, P1, "Mountain"),
        ];
        let mut runner = board.build();
        runner.activate(wand, 0).target_player(P1).resolve();
        assert!(runner.state().objects[&wand].tapped, "the Wand activated");
        assert_priority(&runner, 0);
        assert_bottom(&runner, P0, &[untouched], &[]);
        orders.insert(assert_bottom(&runner, P1, &[], &batch));
    }
    assert!(orders.len() > 1, "exhaustion still randomizes the batch");
}

#[test]
fn ryan_sinclair_declines_and_bottoms_only_this_batch() {
    const RYAN: &str = "Whenever Ryan attacks, exile cards from the top of your library until you exile a nonland card. You may cast the exiled card without paying its mana cost if it's a spell with mana value less than or equal to Ryan's power. Put the exiled cards not cast this way on the bottom of your library in a random order.";
    let mut orders = BTreeSet::new();
    for seed in [4, 19, 42, 93, 101, 109, 211, 495] {
        let mut board = scenario(2, seed);
        let prowler = board
            .add_creature_to_hand_from_oracle(P0, "Cemetery Prowler", 3, 4, PROWLER)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            })
            .id();
        board.add_enchantment_from_oracle(P0, "Fervor", "Creatures you control have haste.");
        let ryan = board
            .add_creature_from_oracle(P1, "Ryan Sinclair", 2, 2, RYAN)
            .id();
        let stale = board.add_land_to_graveyard(P0, "Forest").id();
        let polymorph = add_true_polymorph(&mut board);
        for _ in 0..2 {
            board.add_basic_land(P0, ManaColor::Green);
        }
        for _ in 0..7 {
            board.add_basic_land(P0, ManaColor::Blue);
        }

        let unreached = land(&mut board, P0, "Island");
        let hit = board
            .add_spell_to_library_top(P0, "Revitalize", true)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::White],
            })
            .from_oracle_text("You gain 3 life. Draw a card.")
            .id();
        let batch = [
            land(&mut board, P0, "Plains"),
            land(&mut board, P0, "Swamp"),
            hit,
        ];

        let mut runner = board.build();
        runner.cast(prowler).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&stale].zone, Zone::Exile);
        copy_attacker_with_older_exile(
            &mut runner,
            prowler,
            ryan,
            polymorph,
            stale,
            "Ryan Sinclair",
        );

        runner.advance_to_combat();
        runner
            .declare_attackers(&[(prowler, AttackTarget::Player(P1))])
            .expect("the Ryan copy attacks");
        runner.advance_until_stack_empty();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player: P0, .. }
            ),
            "expected the cast decision, got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("decline Ryan's hit");
        assert_priority(&runner, 0);
        assert_eq!(runner.state().objects[&stale].zone, Zone::Exile);
        orders.insert(assert_bottom(&runner, P0, &[unreached], &batch));
    }
    assert!(
        orders.len() > 1,
        "Ryan's bottom placement must vary across deterministic seeds"
    );
}

#[test]
fn jodah_randomizes_the_exhausted_library_when_its_trigger_resolves() {
    const JODAH: &str = "Legendary creatures you control get +X/+X, where X is the number of legendary creatures you control.\nWhenever you cast a legendary spell from your hand, exile cards from the top of your library until you exile a legendary nonland card with lesser mana value. You may cast that card without paying its mana cost. Put the rest on the bottom of your library in a random order.";
    let mut orders = BTreeSet::new();
    for seed in [4, 19, 42, 93, 101, 109, 211, 495] {
        let mut board = scenario(2, seed);
        let jodah = board
            .add_creature_from_oracle(P0, "Jodah, the Unifier", 5, 5, JODAH)
            .as_legendary()
            .id();
        let isamaru = board
            .add_creature_to_hand(P0, "Isamaru, Hound of Konda", 2, 2)
            .as_legendary()
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::White],
            })
            .id();
        let mana_source = board.add_basic_land(P0, ManaColor::White);
        let batch = [
            land(&mut board, P0, "Forest"),
            land(&mut board, P0, "Island"),
            land(&mut board, P0, "Mountain"),
        ];

        let mut runner = board.build();
        runner.cast(isamaru).commit();
        assert_priority(&runner, 2);
        assert_eq!(runner.state().objects[&isamaru].zone, Zone::Stack);
        assert!(
            runner.state().objects[&mana_source].tapped,
            "Isamaru costs white mana"
        );
        assert_eq!(
            runner.state().stack.last().map(|entry| entry.source_id),
            Some(jodah)
        );

        runner.resolve_top();
        assert_priority(&runner, 1);
        assert_eq!(runner.state().objects[&isamaru].zone, Zone::Stack);
        for id in batch {
            assert!(
                runner
                    .state()
                    .zone_changes_this_turn
                    .iter()
                    .any(|movement| movement.object_id == id
                        && movement.from_zone == Some(Zone::Library)
                        && movement.to_zone == Zone::Exile),
                "each returned card must have been exiled"
            );
        }
        orders.insert(assert_bottom(&runner, P0, &[], &batch));
    }
    assert!(
        orders.len() > 1,
        "Jodah must randomize the exhausted library"
    );
}
