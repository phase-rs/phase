//! Issue #10234: player-scoped protection and player "hexproof from" must be
//! read by the targeting authority.
//!
//! A resolution-created "you gain protection from [quality]" (CR 702.16b) and
//! "you gain hexproof from [quality]" (CR 702.11c/d) install a player-bound
//! transient continuous effect carrying the right keyword. Before this fix
//! nothing read those effects for a player: `player_protection_from_object`
//! honoured only `Protection(Everything)`, and
//! `transient_grants_static_mode_to_player` matched only bare `Hexproof`, so a
//! protected player stayed a legal target and took damage from the very
//! sources the grant names.
//!
//! Rows:
//! - R1/R1b: Seht's Tiger — protection from the chosen color blocks targeting
//!   from sources of that color, with no controller gate (CR 702.16b).
//! - R2: Seht's Tiger — damage from that color is prevented (CR 702.16e).
//! - R2b: Faith's Shield fateful hour — the self-replacement grants the player
//!   half; the base branch grants only the targeted permanent (CR 614.15).
//! - R3/R4/R5: Veil of Summer — hexproof from blue blocks opponents' blue
//!   sources only (CR 702.11c/d).
//! - R6: Seht's Tiger — a non-matching color still targets (quality specificity).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const SEHT: &str = "Flash\nWhen this creature enters, you gain protection from the color of your choice until end of turn.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const FLAME_RIFT: &str = "Flame Rift deals 4 damage to each player.";
const THOUGHT_SCOUR: &str = "Target player mills two cards.\nDraw a card.";
const VEIL: &str = "Draw a card if an opponent has cast a blue or black spell this turn. Spells you control can't be countered this turn. You and permanents you control gain hexproof from blue and from black until end of turn.";
const FAITHS_SHIELD: &str = "Target permanent you control gains protection from the color of your choice until end of turn.\nFateful hour — If you have 5 or less life, instead you and each permanent you control gain protection from the color of your choice until end of turn.";

fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_mana_pool(
            player,
            [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ]
            .into_iter()
            .flat_map(|mana| (0..4).map(move |_| ManaUnit::new(mana, ObjectId(0), false, vec![])))
            .collect(),
        );
    }
    scenario
}

/// CR 202.2: the source's color comes from its mana cost, so the staged spells
/// keep a single colored shard of their printed color (and the board is funded).
fn in_hand(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    is_instant: bool,
    text: &str,
    color: ManaCostShard,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, is_instant, text)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![color],
            generic: 0,
        })
        .id()
}

/// CR 117.1a: hand the active priority to `player` so the test can announce
/// that player's spell deterministically.
fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Announce `spell` with no declared targets and return the legal player
/// targets the engine offers.
fn legal_players_after_announce(runner: &mut GameRunner, spell: ObjectId) -> Vec<PlayerId> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell announcement is accepted");
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots
            .iter()
            .flat_map(|slot| slot.legal_targets.iter())
            .filter_map(|target| match target {
                TargetRef::Player(player) => Some(*player),
                _ => None,
            })
            .collect(),
        other => panic!("expected a target prompt, got {other:?}"),
    }
}

/// CR 601.2c + CR 115.1: announce `spell` and report whether `object` is in the
/// engine's offered object-target set.
fn try_target_object(runner: &mut GameRunner, spell: ObjectId, object: ObjectId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell announcement is accepted");
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal = target_slots
        .iter()
        .flat_map(|slot| slot.legal_targets.iter())
        .any(|target| *target == TargetRef::Object(object));
    if legal {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(object)],
            })
            .expect("the offered target is accepted");
    }
    legal
}

/// CR 601.2c + CR 115.1: announce `spell` and report whether `player` is in
/// the engine's offered player-target set. A legal offer is submitted and
/// resolved so the row can assert the follow-through too.
fn try_target_player(runner: &mut GameRunner, spell: ObjectId, player: PlayerId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell announcement is accepted");
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a target prompt, got {:?}; spell zone={:?}; stack={:?}",
            runner.state().waiting_for,
            runner.state().objects.get(&spell).map(|o| o.zone),
            runner
                .state()
                .stack
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        );
    };
    let legal = target_slots
        .iter()
        .flat_map(|slot| slot.legal_targets.iter())
        .any(|target| *target == TargetRef::Player(player));
    if legal {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(player)],
            })
            .expect("the offered target is accepted");
    }
    legal
}

/// R1: Seht's Tiger (Red), then P1's red spell cannot target P0.
#[test]
fn r1_player_protection_from_red_blocks_an_opponents_red_spell() {
    let mut scenario = board();
    let seht = scenario
        .add_creature_to_hand_from_oracle(P0, "Seht's Tiger", 3, 3, SEHT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let bolt = in_hand(
        &mut scenario,
        P1,
        "Lightning Bolt",
        true,
        BOLT,
        ManaCostShard::Red,
    );
    let mut runner = scenario.build();

    runner.cast(seht).choose_option("Red").resolve();
    assert!(
        runner.state().objects[&seht].zone == engine::types::zones::Zone::Battlefield,
        "reach guard: Seht's Tiger resolved onto the battlefield"
    );
    let granted = runner.state().transient_continuous_effects.iter().any(|tce| {
        matches!(tce.affected, engine::types::ability::TargetFilter::SpecificPlayer { id } if id == P0)
            && tce.modifications.iter().any(|m| {
                matches!(
                    m,
                    engine::types::ability::ContinuousModification::AddKeyword {
                        keyword: engine::types::keywords::Keyword::Protection(
                            engine::types::keywords::ProtectionTarget::Color(
                                engine::types::mana::ManaColor::Red
                            )
                        )
                    }
                )
            })
    });
    assert!(
        granted,
        "reach guard: P0 holds a transient Protection(Red): {:?}",
        runner.state().transient_continuous_effects
    );

    give_priority(&mut runner, P1);
    let legal = legal_players_after_announce(&mut runner, bolt);

    assert!(
        !legal.contains(&P0),
        "CR 702.16b: P0 has protection from red, so a red spell cannot target them: {legal:?}"
    );
    assert!(
        legal.contains(&P1),
        "reach guard: the player enumeration ran and P1 is still targetable: {legal:?}"
    );
}

/// R1b: protection has no controller gate — P0's own red spell is blocked too.
#[test]
fn r1b_player_protection_from_red_blocks_the_protected_players_own_red_spell() {
    let mut scenario = board();
    let seht = scenario
        .add_creature_to_hand_from_oracle(P0, "Seht's Tiger", 3, 3, SEHT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let bolt = in_hand(
        &mut scenario,
        P0,
        "Lightning Bolt",
        true,
        BOLT,
        ManaCostShard::Red,
    );
    let mut runner = scenario.build();

    runner.cast(seht).choose_option("Red").resolve();
    give_priority(&mut runner, P0);
    let legal = legal_players_after_announce(&mut runner, bolt);

    assert!(
        !legal.contains(&P0),
        "CR 702.16b: protection is controller-blind: {legal:?}"
    );
}

/// R2: damage from the protected color is prevented (CR 702.16e).
#[test]
fn r2_player_protection_from_red_prevents_red_damage() {
    let mut scenario = board();
    let seht = scenario
        .add_creature_to_hand_from_oracle(P0, "Seht's Tiger", 3, 3, SEHT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let rift = in_hand(
        &mut scenario,
        P0,
        "Flame Rift",
        false,
        FLAME_RIFT,
        ManaCostShard::Red,
    );
    let mut runner = scenario.build();

    runner.cast(seht).choose_option("Red").resolve();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(rift).resolve();

    assert_eq!(
        outcome.life_delta(P0),
        0,
        "CR 702.16e: red damage to the protected player is prevented"
    );
    assert_eq!(
        outcome.life_delta(P1),
        -4,
        "reach guard: the damage event executed for the unprotected player"
    );
}

/// R2b: Faith's Shield's fateful-hour self-replacement grants the player half.
#[test]
fn r2b_faiths_shield_fateful_hour_grants_the_player_protection() {
    let mut scenario = board();
    scenario.with_life(P0, 5);
    let bears = scenario.add_creature(P0, "Bears", 2, 2).id();
    let shield = in_hand(
        &mut scenario,
        P0,
        "Faith's Shield",
        true,
        FAITHS_SHIELD,
        ManaCostShard::White,
    );
    let bolt = in_hand(
        &mut scenario,
        P1,
        "Lightning Bolt",
        true,
        BOLT,
        ManaCostShard::Red,
    );
    // A second legal Bolt target so the engine surfaces the target prompt
    // instead of auto-selecting the only legal target.
    scenario.add_creature(P1, "P1 Bears", 2, 2);
    let mut runner = scenario.build();

    runner
        .cast(shield)
        .target_object(bears)
        .choose_option("Red")
        .resolve();

    give_priority(&mut runner, P1);
    let offered = try_target_player(&mut runner, bolt, P0);

    assert!(
        !offered,
        "CR 614.15 + CR 702.16b: the swapped-in effect grants P0 protection from red"
    );
}

/// R2b-negative: above 5 life the base clause grants only the permanent.
#[test]
fn r2b_above_fateful_hour_grants_only_the_targeted_permanent() {
    let mut scenario = board();
    let bears = scenario.add_creature(P0, "Bears", 2, 2).id();
    let shield = in_hand(
        &mut scenario,
        P0,
        "Faith's Shield",
        true,
        FAITHS_SHIELD,
        ManaCostShard::White,
    );
    let bolt = in_hand(
        &mut scenario,
        P1,
        "Lightning Bolt",
        true,
        BOLT,
        ManaCostShard::Red,
    );
    // A second legal Bolt target so the engine surfaces the target prompt
    // instead of auto-selecting the only legal target.
    scenario.add_creature(P1, "P1 Bears", 2, 2);
    let mut runner = scenario.build();

    runner
        .cast(shield)
        .target_object(bears)
        .choose_option("Red")
        .resolve();

    give_priority(&mut runner, P1);
    let legal = legal_players_after_announce(&mut runner, bolt);

    assert!(
        legal.contains(&P0),
        "the base clause never protects the player: {legal:?}"
    );
}

/// R3: Veil of Summer's hexproof from blue blocks an opponent's blue spell.
#[test]
fn r3_player_hexproof_from_blue_blocks_an_opponents_blue_spell() {
    // Three players: with P0 hexproof the blue spell still has two legal player
    // targets, so the engine surfaces the prompt instead of auto-selecting.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, PlayerId(2)] {
        scenario.with_mana_pool(
            player,
            [ManaType::Blue, ManaType::Green]
                .into_iter()
                .flat_map(|mana| {
                    (0..4).map(move |_| ManaUnit::new(mana, ObjectId(0), false, vec![]))
                })
                .collect(),
        );
    }
    let veil = in_hand(
        &mut scenario,
        P0,
        "Veil of Summer",
        true,
        VEIL,
        ManaCostShard::Green,
    );
    let scour = in_hand(
        &mut scenario,
        P1,
        "Thought Scour",
        true,
        THOUGHT_SCOUR,
        ManaCostShard::Blue,
    );
    let mut runner = scenario.build();

    runner.cast(veil).resolve();
    give_priority(&mut runner, P1);
    let offered = try_target_player(&mut runner, scour, P0);

    assert!(!offered, "CR 702.11c/d: P0 has hexproof from blue");
}

/// R4: the opponent gate — the protected player's own blue spell still works.
#[test]
fn r4_player_hexproof_from_blue_does_not_block_the_players_own_spell() {
    let mut scenario = board();
    let veil = in_hand(
        &mut scenario,
        P0,
        "Veil of Summer",
        true,
        VEIL,
        ManaCostShard::Green,
    );
    let scour = in_hand(
        &mut scenario,
        P0,
        "Thought Scour",
        true,
        THOUGHT_SCOUR,
        ManaCostShard::Blue,
    );
    let mut runner = scenario.build();

    runner.cast(veil).resolve();
    give_priority(&mut runner, P0);
    let legal = legal_players_after_announce(&mut runner, scour);

    assert!(
        legal.contains(&P0),
        "CR 702.11c: hexproof from a quality only stops opponents' sources: {legal:?}"
    );
}

/// R5: quality specificity — a red spell from the opponent still targets.
#[test]
fn r5_player_hexproof_from_blue_allows_a_non_matching_opponent_spell() {
    let mut scenario = board();
    let veil = in_hand(
        &mut scenario,
        P0,
        "Veil of Summer",
        true,
        VEIL,
        ManaCostShard::Green,
    );
    let bolt = in_hand(
        &mut scenario,
        P1,
        "Lightning Bolt",
        true,
        BOLT,
        ManaCostShard::Red,
    );
    let mut runner = scenario.build();

    runner.cast(veil).resolve();
    give_priority(&mut runner, P1);
    let legal = legal_players_after_announce(&mut runner, bolt);

    assert!(
        legal.contains(&P0),
        "CR 702.11d: only blue/black sources are blocked: {legal:?}"
    );
}

/// R6: quality specificity on the protection half — blue still targets.
#[test]
fn r6_player_protection_from_red_allows_a_blue_spell() {
    let mut scenario = board();
    let seht = scenario
        .add_creature_to_hand_from_oracle(P0, "Seht's Tiger", 3, 3, SEHT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let scour = in_hand(
        &mut scenario,
        P1,
        "Thought Scour",
        true,
        THOUGHT_SCOUR,
        ManaCostShard::Blue,
    );
    let mut runner = scenario.build();

    runner.cast(seht).choose_option("Red").resolve();
    give_priority(&mut runner, P1);
    let legal = legal_players_after_announce(&mut runner, scour);

    assert!(
        legal.contains(&P0),
        "CR 702.16b: protection from red does not stop a blue spell: {legal:?}"
    );
}

const EON: &str = "When this creature enters, if you cast it, target opponent takes an extra turn after this one. Until your next turn, you and planeswalkers you control gain protection from that player.";

/// R9: Eon Frolicker's "protection from that player" latches the declared
/// target and blocks only that player's sources (CR 702.16k, CR 608.2c).
#[test]
fn r9_eon_frolicker_protects_only_from_the_declared_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);
    for player in [P0, P1, p2] {
        scenario.with_mana_pool(
            player,
            [ManaType::Blue, ManaType::Red, ManaType::White]
                .into_iter()
                .flat_map(|mana| {
                    (0..4).map(move |_| ManaUnit::new(mana, ObjectId(0), false, vec![]))
                })
                .collect(),
        );
    }
    let eon = scenario
        .add_creature_to_hand_from_oracle(P0, "Eon Frolicker", 4, 4, EON)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let p1_scour = in_hand(
        &mut scenario,
        P1,
        "Thought Scour",
        true,
        THOUGHT_SCOUR,
        ManaCostShard::Blue,
    );
    let p2_scour = in_hand(
        &mut scenario,
        p2,
        "Thought Scour",
        true,
        THOUGHT_SCOUR,
        ManaCostShard::Blue,
    );
    let mut runner = scenario.build();

    runner.cast(eon).target_player(P1).resolve();
    let latched = runner.state().transient_continuous_effects.iter().any(|tce| {
        matches!(tce.affected, engine::types::ability::TargetFilter::SpecificPlayer { id } if id == P0)
            && tce.modifications.iter().any(|m| {
                matches!(
                    m,
                    engine::types::ability::ContinuousModification::AddKeyword {
                        keyword: engine::types::keywords::Keyword::Protection(
                            engine::types::keywords::ProtectionTarget::FromPlayer(
                                engine::types::ability::ControllerRef::SpecificPlayer { id }
                            )
                        )
                    } if *id == P1
                )
            })
    });
    assert!(
        latched,
        "reach guard: the declared target is latched into the grant: {:?}",
        runner.state().transient_continuous_effects
    );

    give_priority(&mut runner, P1);
    let offered = try_target_player(&mut runner, p1_scour, P0);
    assert!(
        !offered,
        "CR 702.16k: the declared player's source cannot target the protected player"
    );

    give_priority(&mut runner, p2);
    let offered = try_target_player(&mut runner, p2_scour, P0);
    assert!(
        offered,
        "a third player's source is outside protection from that player"
    );
}

/// R9b: Eon's planeswalker half — the object bearer's `FromPlayer(SpecificPlayer)`
/// arm blocks the declared player's sources only (CR 702.16k).
#[test]
fn r9b_eon_frolicker_protects_a_planeswalker_from_the_declared_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);
    for player in [P0, P1, p2] {
        scenario.with_mana_pool(
            player,
            [ManaType::Blue, ManaType::Red, ManaType::White]
                .into_iter()
                .flat_map(|mana| {
                    (0..4).map(move |_| ManaUnit::new(mana, ObjectId(0), false, vec![]))
                })
                .collect(),
        );
    }
    let walker = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Test Walker",
            "Test",
            4,
            "0: Put a loyalty counter on this permanent.",
        )
        .id();
    let eon = scenario
        .add_creature_to_hand_from_oracle(P0, "Eon Frolicker", 4, 4, EON)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let p1_bolt = in_hand(
        &mut scenario,
        P1,
        "Lightning Bolt",
        true,
        BOLT,
        ManaCostShard::Red,
    );
    let p2_bolt = in_hand(
        &mut scenario,
        p2,
        "Lightning Bolt",
        true,
        BOLT,
        ManaCostShard::Red,
    );
    let mut runner = scenario.build();

    runner.cast(eon).target_player(P1).resolve();

    let baked = runner.state().objects[&walker].keywords.iter().any(|kw| {
        matches!(
            kw,
            engine::types::keywords::Keyword::Protection(
                engine::types::keywords::ProtectionTarget::FromPlayer(
                    engine::types::ability::ControllerRef::SpecificPlayer { id }
                )
            ) if *id == P1
        )
    });
    assert!(
        baked,
        "reach guard: the layer applier baked the latched quality onto the planeswalker: {:?}",
        runner.state().objects[&walker].keywords
    );

    give_priority(&mut runner, P1);
    let offered = try_target_object(&mut runner, p1_bolt, walker);
    assert!(
        !offered,
        "CR 702.16k: the declared player's source cannot target the protected planeswalker"
    );

    give_priority(&mut runner, p2);
    let offered = try_target_object(&mut runner, p2_bolt, walker);
    assert!(
        offered,
        "a third player's source is outside protection from that player"
    );
}
