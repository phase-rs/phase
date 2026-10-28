//! A token instruction that names the defender its tokens attack ("attacking
//! that player", "attacking that opponent", or a following "The token enters
//! tapped and attacking that player") seats each created token there.
//!
//! CR 508.4: a creature put onto the battlefield attacking is never declared
//! as an attacker; the instruction (or its controller) decides what it attacks.
//! CR 508.4a: if the instruction names a player who has left the game, the
//! creature enters but is not attacking.
//! CR 506.3b: a creature put onto the battlefield attacking under the control of
//! a player who is not an attacking player enters but is never attacking.
//! CR 111.2: the player who creates a token controls it, so the legality of a
//! named destination is judged for that player.
//! CR 508.3e: "Whenever [a player] attacks [another player]" is about creatures
//! attacking that other player, so "that player" is the player a firing is for.

use engine::game::effects::{token, token_copy};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ContinuousModification, ControllerRef, Effect, NamedDefenderScope, PlayerFilter, PtValue,
    QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TargetRef, TokenAttackDestination,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const SOARING_LIGHTBRINGER: &str = "Flying\nOther enchantment creatures you control have flying.\nWhenever you attack a player, create a 1/1 white Glimmer enchantment creature token that's tapped and attacking that player.";
const SERAPHIC_GREATSWORD: &str = "Equipped creature gets +2/+2.\nWhenever equipped creature attacks the player with the most life or tied for most life, create a 4/4 white Angel creature token with flying that's tapped and attacking that player.\nEquip {4}";
const ECHOING_ASSAULT: &str = "Creature tokens you control have menace.\nWhenever you attack a player, choose target nontoken creature that's attacking that player. Create a token that's a copy of that creature, except it's 1/1. The token enters tapped and attacking that player. Sacrifice it at the beginning of the next end step.";
const ELLIE_BRICK_MASTER: &str = "Distract the Horde — Whenever a player attacks one of your opponents, that attacking player creates a tapped 1/1 black Fungus Zombie creature token named Cordyceps Infected that's attacking that opponent.\nPartner—Survivors (You can have two commanders if both have this ability.)";
const COMBAT_CALLIGRAPHER: &str = "Flying\nInklings can't attack you or planeswalkers you control.\nWhenever a player attacks one of your opponents, that attacking player creates a tapped 2/1 white and black Inkling creature token with flying that's attacking that opponent.";

/// Which order the driver answers a trigger-ordering prompt in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TriggerOrder {
    AsOffered,
    Reversed,
}

/// Drive the stack empty: order triggers, pick the first legal target for any
/// target prompt, and pass priority.
fn drain(runner: &mut GameRunner, order: TriggerOrder) {
    drain_with(runner, order, &mut |runner| {
        panic!(
            "unexpected waiting state while draining the stack: {:?}",
            runner.state().waiting_for
        )
    });
}

/// `drain`, handing a replacement-ordering prompt to `on_replacement`, which
/// must answer it or otherwise advance the game.
fn drain_with(
    runner: &mut GameRunner,
    order: TriggerOrder,
    on_replacement: &mut dyn FnMut(&mut GameRunner),
) {
    for _ in 0..120 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let indices: Vec<usize> = match order {
                    TriggerOrder::AsOffered => (0..triggers.len()).collect(),
                    TriggerOrder::Reversed => (0..triggers.len()).rev().collect(),
                };
                runner
                    .act(GameAction::OrderTriggers { order: indices })
                    .expect("order triggers");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner.choose_first_legal_target().expect("choose a target");
            }
            WaitingFor::ReplacementChoice { .. } => on_replacement(runner),
            other => panic!("unexpected waiting state while draining the stack: {other:?}"),
        }
    }
    panic!("the stack did not drain");
}

/// The tokens on the battlefield with `name`, in creation order.
fn tokens_named(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.is_token && o.name.eq_ignore_ascii_case(name))
        .map(|o| o.id)
        .collect();
    ids.sort();
    ids
}

/// What `id` is attacking, or `None` when it is not an attacking creature.
fn attacking(runner: &GameRunner, id: ObjectId) -> Option<AttackTarget> {
    runner
        .state()
        .combat
        .as_ref()
        .and_then(|combat| combat.attackers.iter().find(|a| a.object_id == id))
        .map(|a| a.attack_target)
}

/// The set of defenders the named tokens attack, sorted.
fn defenders_of(runner: &GameRunner, tokens: &[ObjectId]) -> Vec<Option<AttackTarget>> {
    let mut seats: Vec<Option<AttackTarget>> =
        tokens.iter().map(|t| attacking(runner, *t)).collect();
    seats.sort_by_key(|seat| format!("{seat:?}"));
    seats
}

fn make_active(runner: &mut GameRunner, player: PlayerId) {
    runner.state_mut().active_player = player;
    runner.state_mut().priority_player = player;
    runner.state_mut().waiting_for = WaitingFor::Priority { player };
}

/// Soaring Lightbringer is printed as an Enchantment Creature — Bird Glimmer.
fn add_soaring_lightbringer(scenario: &mut GameScenario) -> ObjectId {
    let mut builder =
        scenario.add_creature_from_oracle(P0, "Soaring Lightbringer", 4, 5, SOARING_LIGHTBRINGER);
    builder.with_subtypes(vec!["Bird", "Glimmer"]);
    builder.id()
}

/// The scenario builder seeds only Creature; add Enchantment to both the
/// current and base type lines.
fn make_enchantment_creature(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).expect("object");
    for types in [
        &mut obj.card_types.core_types,
        &mut obj.base_card_types.core_types,
    ] {
        if !types.contains(&CoreType::Enchantment) {
            types.push(CoreType::Enchantment);
        }
    }
}

// ---------------------------------------------------------------------------
// Ellie, Brick Master and Combat Calligrapher: the token's creator is the
// attacking player, not the ability's controller.
// ---------------------------------------------------------------------------

/// CR 508.4 + CR 508.3e: P1 attacks P2; P1 creates the token, and it attacks
/// "that opponent", P2. Revert-failing: at BASE the token attacks P1, its own controller.
#[test]
fn ellie_token_attacks_the_opponent_its_creator_attacked() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Ellie, Brick Master", 2, 1, ELLIE_BRICK_MASTER);
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();
    make_active(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(giant, AttackTarget::Player(P2))])
        .expect("P1 attacks P2");
    drain(&mut runner, TriggerOrder::AsOffered);

    let tokens = tokens_named(&runner, "Cordyceps Infected");
    assert_eq!(tokens.len(), 1, "reach guard: P1 created one token");
    let token = &runner.state().objects[&tokens[0]];
    assert_eq!(
        token.controller, P1,
        "the attacking player creates the token"
    );
    assert!(token.tapped, "the token enters tapped");
    assert_eq!(
        attacking(&runner, tokens[0]),
        Some(AttackTarget::Player(P2)),
        "the token attacks the opponent P1 attacked"
    );
}

/// CR 508.4: Combat Calligrapher in four players; P1 attacks P2 and P3. Two
/// firings, one Inkling each, each attacking the opponent its firing is for.
#[test]
fn calligrapher_tokens_attack_each_opponent_their_creator_attacked() {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Combat Calligrapher", 3, 3, COMBAT_CALLIGRAPHER);
    let x = scenario.add_creature(P1, "Giant X", 3, 3).id();
    let y = scenario.add_creature(P1, "Giant Y", 3, 3).id();
    let mut runner = scenario.build();
    make_active(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(x, AttackTarget::Player(P2)), (y, AttackTarget::Player(P3))])
        .expect("P1 attacks P2 and P3");
    drain(&mut runner, TriggerOrder::AsOffered);

    let tokens = tokens_named(&runner, "Inkling");
    assert_eq!(tokens.len(), 2, "reach guard: one Inkling per firing");
    assert_eq!(
        defenders_of(&runner, &tokens),
        vec![
            Some(AttackTarget::Player(P2)),
            Some(AttackTarget::Player(P3))
        ]
    );
}

/// CR 508.4a: "that opponent" names P2, who leaves the game while the trigger is
/// on the stack. The token is created but is not attacking, and is not seated at
/// another player.
#[test]
fn ellie_token_is_not_attacking_when_the_named_opponent_has_left() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Ellie, Brick Master", 2, 1, ELLIE_BRICK_MASTER);
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();
    make_active(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(giant, AttackTarget::Player(P2))])
        .expect("P1 attacks P2");
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    drain(&mut runner, TriggerOrder::AsOffered);

    assert!(
        runner.state().eliminated_players.contains(&P2),
        "reach guard: P2 left the game"
    );
    let tokens = tokens_named(&runner, "Cordyceps Infected");
    assert_eq!(tokens.len(), 1, "reach guard: the token was still created");
    assert_eq!(attacking(&runner, tokens[0]), None);
}

// ---------------------------------------------------------------------------
// Soaring Lightbringer: one firing per attacked player; "that player" is the
// player the firing is for.
// ---------------------------------------------------------------------------

fn soaring_board(players: u8, attacks: &[(&str, PlayerId)]) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(players, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let soaring = add_soaring_lightbringer(&mut scenario);
    let mut attackers = Vec::new();
    for (name, _) in attacks {
        if *name == "Soaring" {
            attackers.push(soaring);
        } else {
            attackers.push(scenario.add_creature(P0, name, 2, 2).id());
        }
    }
    let mut runner = scenario.build();
    make_enchantment_creature(&mut runner, soaring);
    runner.advance_to_combat();
    let declared: Vec<(ObjectId, AttackTarget)> = attackers
        .iter()
        .zip(attacks)
        .map(|(id, (_, defender))| (*id, AttackTarget::Player(*defender)))
        .collect();
    runner
        .declare_attackers(&declared)
        .expect("declare attackers");
    (runner, attackers)
}

/// CR 508.3e: attacking P1 and P2 fires the trigger once for each; each Glimmer
/// attacks the player its firing is for.
#[test]
fn soaring_lightbringer_glimmers_attack_each_player_attacked() {
    let (mut runner, _) = soaring_board(4, &[("Bear A", P1), ("Bear B", P2)]);
    drain(&mut runner, TriggerOrder::AsOffered);

    let glimmers = tokens_named(&runner, "Glimmer");
    assert_eq!(glimmers.len(), 2, "reach guard: one Glimmer per firing");
    assert_eq!(
        defenders_of(&runner, &glimmers),
        vec![
            Some(AttackTarget::Player(P1)),
            Some(AttackTarget::Player(P2))
        ]
    );
}

/// CR 508.4a: P1 leaves the game before the triggers resolve. P1's Glimmer is
/// created but is not attacking; P2's Glimmer attacks P2.
#[test]
fn soaring_lightbringer_glimmer_for_a_departed_player_is_not_attacking() {
    let (mut runner, _) = soaring_board(3, &[("Bear A", P1), ("Bear B", P2)]);
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");
    drain(&mut runner, TriggerOrder::AsOffered);

    assert!(
        runner.state().eliminated_players.contains(&P1),
        "reach guard: P1 left the game"
    );
    let glimmers = tokens_named(&runner, "Glimmer");
    assert_eq!(
        glimmers.len(),
        2,
        "reach guard: both firings created a Glimmer"
    );
    assert_eq!(
        defenders_of(&runner, &glimmers),
        vec![None, Some(AttackTarget::Player(P2))]
    );
}

/// CR 508.3e + CR 508.5a: Soaring Lightbringer attacks P1 while another creature
/// attacks P2. The firing for P2 is not answered by Soaring's own defender.
#[test]
fn soaring_lightbringer_attacking_one_player_while_another_creature_attacks_another() {
    let (mut runner, _) = soaring_board(3, &[("Soaring", P1), ("Bear B", P2)]);
    drain(&mut runner, TriggerOrder::AsOffered);

    let glimmers = tokens_named(&runner, "Glimmer");
    assert_eq!(glimmers.len(), 2, "reach guard: one Glimmer per firing");
    assert_eq!(
        defenders_of(&runner, &glimmers),
        vec![
            Some(AttackTarget::Player(P1)),
            Some(AttackTarget::Player(P2))
        ]
    );
}

/// CR 508.3e + CR 508.5a: Soaring Lightbringer attacks P1 while two creatures
/// attack P2. Those two share one defending player, so the firing for P2 has one
/// answer and it is not Soaring's own defender. Both trigger orders.
#[test]
fn soaring_lightbringer_attacking_one_player_while_two_creatures_attack_another() {
    for order in [TriggerOrder::AsOffered, TriggerOrder::Reversed] {
        let (mut runner, _) = soaring_board(3, &[("Soaring", P1), ("Bear B", P2), ("Bear C", P2)]);
        drain(&mut runner, order);

        let glimmers = tokens_named(&runner, "Glimmer");
        assert_eq!(glimmers.len(), 2, "reach guard: one Glimmer per firing");
        assert_eq!(
            defenders_of(&runner, &glimmers),
            vec![
                Some(AttackTarget::Player(P1)),
                Some(AttackTarget::Player(P2))
            ]
        );
    }
}

// ---------------------------------------------------------------------------
// Seraphic Greatsword and Echoing Assault: preserved readings.
// ---------------------------------------------------------------------------

/// CR 508.4: the equipped creature attacks P2 (the player with the most life)
/// while another creature attacks P1 and was declared first. The Angel attacks
/// the equipped creature's player, P2.
#[test]
fn seraphic_greatsword_angel_attacks_the_player_the_equipped_creature_attacked() {
    use engine::game::game_object::AttachTarget;

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P2, 30);
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let other = scenario.add_creature(P0, "Other", 2, 2).id();
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Seraphic Greatsword", SERAPHIC_GREATSWORD)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .attachments
            .push(equipment);
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (other, AttackTarget::Player(P1)),
            (host, AttackTarget::Player(P2)),
        ])
        .expect("declare attackers");
    drain(&mut runner, TriggerOrder::AsOffered);

    let angels = tokens_named(&runner, "Angel");
    assert_eq!(angels.len(), 1, "reach guard: the trigger created an Angel");
    assert_eq!(
        attacking(&runner, angels[0]),
        Some(AttackTarget::Player(P2))
    );
}

/// CR 508.4: Echoing Assault fires once per attacked player; each copy attacks
/// the player its original attacks.
#[test]
fn echoing_assault_copies_attack_the_player_their_original_attacks() {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Echoing Assault", ECHOING_ASSAULT);
    let bear = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let ogre = scenario.add_creature(P0, "Ogre B", 3, 3).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (bear, AttackTarget::Player(P1)),
            (ogre, AttackTarget::Player(P2)),
        ])
        .expect("declare attackers");
    drain(&mut runner, TriggerOrder::AsOffered);

    let bear_copies: Vec<ObjectId> = tokens_named(&runner, "Bear A");
    let ogre_copies: Vec<ObjectId> = tokens_named(&runner, "Ogre B");
    assert_eq!(bear_copies.len(), 1, "reach guard: the Bear was copied");
    assert_eq!(ogre_copies.len(), 1, "reach guard: the Ogre was copied");
    assert_eq!(
        attacking(&runner, bear_copies[0]),
        Some(AttackTarget::Player(P1))
    );
    assert_eq!(
        attacking(&runner, ogre_copies[0]),
        Some(AttackTarget::Player(P2))
    );
}

// ---------------------------------------------------------------------------
// Every route that seats a created token honors the destination. Three players,
// the host attacking P1, and the instruction naming P2: the ambient answer (P1)
// differs from the named one, so a seat that ignores the destination is visible.
// ---------------------------------------------------------------------------

const FAITHFUL_WATCHDOG: &str = "Vigilance\nThis creature enters with three +1/+1 counters on it.";
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";

/// The destination "attacking that player" resolves to P2 for these routes.
fn names_p2() -> TokenAttackDestination {
    TokenAttackDestination::Player {
        player: ControllerRef::SpecificPlayer { id: P2 },
        scope: NamedDefenderScope::Player,
    }
}

/// P0's combat with a host attacking P1, plus whatever `stage` adds for P0.
fn carrier_board(
    stage: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    carrier_board_of(3, stage)
}

/// `carrier_board` with `players` seats.
fn carrier_board_of(
    players: u8,
    stage: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(players, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Attacker", 2, 2).id();
    let staged = stage(&mut scenario);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(host, AttackTarget::Player(P1))])
        .expect("declare the host");
    drain(&mut runner, TriggerOrder::AsOffered);
    (runner, host, staged)
}

fn goblin_effect(destination: TokenAttackDestination) -> Effect {
    Effect::Token {
        name: "Goblin".to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string(), "Goblin".to_string()],
        colors: vec![],
        keywords: vec![],
        tapped: true,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: true,
        attack_destination: destination,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

fn copy_effect(
    destination: TokenAttackDestination,
    additional_modifications: Vec<ContinuousModification>,
) -> Effect {
    Effect::CopyTokenOf {
        target: TargetFilter::Any,
        owner: TargetFilter::Controller,
        source_filter: None,
        enters_attacking: true,
        attack_destination: destination,
        tapped: true,
        count: QuantityExpr::Fixed { value: 1 },
        extra_keywords: Vec::new(),
        additional_modifications,
    }
}

/// The most recently created token on the battlefield.
fn newest_token(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .max()
        .expect("a token was created")
}

/// Answer the replacement-ordering prompt the doubler pair raises, requiring
/// that it is raised so the route under test is the one that was reached.
fn answer_replacement_order(runner: &mut GameRunner) {
    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            assert_eq!(candidates.len(), 2, "both counter doublers compete");
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("choose a replacement order");
        }
        other => panic!("expected the replacement-ordering prompt, got {other:?}"),
    }
}

#[test]
fn created_token_is_seated_at_the_named_player() {
    let (mut runner, host, _) = carrier_board(|_| vec![]);
    let ability = ResolvedAbility::new(goblin_effect(names_p2()), vec![], host, P0);
    let mut events = Vec::new();
    token::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");

    let token_id = newest_token(&runner);
    assert_eq!(
        runner.state().objects[&token_id].name,
        "Goblin",
        "reach guard"
    );
    assert_eq!(attacking(&runner, token_id), Some(AttackTarget::Player(P2)));
}

#[test]
fn copy_token_is_seated_at_the_named_player() {
    let (mut runner, host, staged) =
        carrier_board(|scenario| vec![scenario.add_creature(P0, "Grizzly Bears", 2, 2).id()]);
    let ability = ResolvedAbility::new(
        copy_effect(names_p2(), vec![]),
        vec![TargetRef::Object(staged[0])],
        host,
        P0,
    );
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");

    let token_id = newest_token(&runner);
    assert_eq!(
        runner.state().objects[&token_id].name,
        "Grizzly Bears",
        "reach guard: a copy was created"
    );
    assert_eq!(attacking(&runner, token_id), Some(AttackTarget::Player(P2)));
}

/// A copy of a creature that enters with counters takes the non-liminal tail.
#[test]
fn copy_token_with_entry_counters_is_seated_at_the_named_player() {
    let (mut runner, host, staged) = carrier_board(|scenario| {
        vec![scenario
            .add_creature(P0, "Faithful Watchdog", 0, 0)
            .with_plus_counters(3)
            .from_oracle_text_with_keywords(&["Vigilance"], FAITHFUL_WATCHDOG)
            .id()]
    });
    let ability = ResolvedAbility::new(
        copy_effect(names_p2(), vec![]),
        vec![TargetRef::Object(staged[0])],
        host,
        P0,
    );
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");

    let token_id = newest_token(&runner);
    assert_eq!(
        runner.state().objects[&token_id]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&3),
        "reach guard: the copy entered with its three counters"
    );
    assert_eq!(attacking(&runner, token_id), Some(AttackTarget::Player(P2)));
}

/// The counter-doubling pair pauses the entry for a replacement ordering; the
/// token is seated after the answer.
#[test]
fn copy_token_is_seated_at_the_named_player_after_a_counter_replacement_pause() {
    let (mut runner, host, staged) = carrier_board(|scenario| {
        let watchdog = scenario
            .add_creature(P0, "Faithful Watchdog", 0, 0)
            .with_plus_counters(3)
            .from_oracle_text_with_keywords(&["Vigilance"], FAITHFUL_WATCHDOG)
            .id();
        scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
        scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        vec![watchdog]
    });
    let ability = ResolvedAbility::new(
        copy_effect(names_p2(), vec![]),
        vec![TargetRef::Object(staged[0])],
        host,
        P0,
    );
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");
    answer_replacement_order(&mut runner);

    let token_id = newest_token(&runner);
    assert_eq!(attacking(&runner, token_id), Some(AttackTarget::Player(P2)));
}

/// A copy exception that adds a counter on entry is applied after the body
/// exists and can pause for the same ordering prompt.
#[test]
fn copy_token_with_an_entry_counter_exception_is_seated_at_the_named_player() {
    let (mut runner, host, staged) = carrier_board(|scenario| {
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
        scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        vec![bears]
    });
    let ability = ResolvedAbility::new(
        copy_effect(
            names_p2(),
            vec![ContinuousModification::AddCounterOnEnter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                if_type: None,
            }],
        ),
        vec![TargetRef::Object(staged[0])],
        host,
        P0,
    );
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");
    answer_replacement_order(&mut runner);

    let token_id = newest_token(&runner);
    assert_eq!(attacking(&runner, token_id), Some(AttackTarget::Player(P2)));
}

/// CR 506.3b: a creature put onto the battlefield attacking under the control
/// of a player who is not an attacking player is never attacking. P0 is the
/// attacking player and P2 is not, so the token P2 creates is not attacking even
/// though its instruction names P1.
#[test]
fn token_created_by_a_player_who_is_not_attacking_is_not_attacking() {
    let (mut runner, host, _) = carrier_board(|_| vec![]);
    let named_p1 = TokenAttackDestination::Player {
        player: ControllerRef::SpecificPlayer { id: P1 },
        scope: NamedDefenderScope::Player,
    };

    let by_attacker = ResolvedAbility::new(goblin_effect(named_p1.clone()), vec![], host, P0);
    let mut events = Vec::new();
    token::resolve(runner.state_mut(), &by_attacker, &mut events).expect("resolve");
    let attacker_token = newest_token(&runner);
    assert_eq!(
        attacking(&runner, attacker_token),
        Some(AttackTarget::Player(P1)),
        "reach guard: the attacking player's token attacks the named player"
    );

    let by_bystander = ResolvedAbility::new(goblin_effect(named_p1), vec![], host, P2);
    token::resolve(runner.state_mut(), &by_bystander, &mut events).expect("resolve");
    let bystander_token = newest_token(&runner);
    assert_ne!(
        bystander_token, attacker_token,
        "reach guard: a second token"
    );
    assert_eq!(runner.state().objects[&bystander_token].controller, P2);
    assert_eq!(attacking(&runner, bystander_token), None);
}

// ---------------------------------------------------------------------------
// "For each opponent, create a token that's tapped and attacking that player":
// one token for each opponent, each attacking its own opponent, whether or not
// that opponent was the one the controller attacked.
// ---------------------------------------------------------------------------

const ADELINE: &str = "Vigilance\nAdeline's power is equal to the number of creatures you control.\nWhenever you attack, for each opponent, create a 1/1 white Human creature token that's tapped and attacking that player or a planeswalker they control.";
const AINOK_STRIKE_LEADER: &str = "Whenever you attack with this creature and/or your commander, for each opponent, create a 1/1 red Goblin creature token that's tapped and attacking that player.\nSacrifice this creature: Creature tokens you control gain indestructible until end of turn.";
const ENDLESS_FOOT_ASSAULT: &str = "Squad {1}{W} (As an additional cost to cast this spell, you may pay {1}{W} any number of times. When this enchantment enters, create that many tokens that are copies of it.)\nWhenever you attack, for each opponent, create a 1/1 black Ninja creature token that's tapped and attacking that player.";
const STAMPEDE_SURFER: &str = "Haste\nWhenever this creature attacks, for each opponent, you create a 2/2 green Boar creature token that's tapped and attacking that opponent.";
const VETERAN_SOLDIER: &str = "Commander creatures you own have \"Whenever this creature attacks a player, if no opponent has more life than that player, for each opponent, create a 1/1 white Soldier creature token that's tapped and attacking that opponent.\"";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const GRUMGULLY: &str =
    "Each other non-Human creature you control enters with an additional +1/+1 counter on it.";
const JACE_BELEREN: &str = "+2: Each player draws a card.\n\u{2212}1: Target player draws a card.\n\u{2212}10: Target player mills twenty cards.";

/// An `n`-player combat in which P0 attacks P1 with the creature `stage`
/// returns; the attack triggers are on the stack.
fn per_opponent_board(
    players: u8,
    stage: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(players, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = stage(&mut scenario);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("P0 attacks P1");
    runner
}

/// What each token attacks, in creation order.
fn seats_in_creation_order(runner: &GameRunner, tokens: &[ObjectId]) -> Vec<Option<AttackTarget>> {
    tokens.iter().map(|t| attacking(runner, *t)).collect()
}

fn players_attacked(players: &[PlayerId]) -> Vec<Option<AttackTarget>> {
    players
        .iter()
        .map(|p| Some(AttackTarget::Player(*p)))
        .collect()
}

/// How many of `tokens` attack `player`.
fn attacking_player_count(runner: &GameRunner, tokens: &[ObjectId], player: PlayerId) -> usize {
    tokens
        .iter()
        .filter(|t| attacking(runner, **t) == Some(AttackTarget::Player(player)))
        .count()
}

fn assert_all_tapped(runner: &GameRunner, tokens: &[ObjectId]) {
    assert!(
        tokens.iter().all(|t| runner.state().objects[t].tapped),
        "the tokens enter tapped"
    );
}

/// CR 508.4 + CR 608.2c: Adeline in four players, attacking P1. One Human for
/// each opponent, each attacking its own opponent. Revert-failing: all three
/// attack P1.
#[test]
fn adeline_humans_attack_each_opponent() {
    let mut runner = per_opponent_board(4, |s| {
        s.add_creature(P0, "Adeline, Resplendent Cathar", 3, 3)
            .from_oracle_text_with_keywords(&["Vigilance"], ADELINE)
            .id()
    });
    drain(&mut runner, TriggerOrder::AsOffered);

    let humans = tokens_named(&runner, "Human");
    assert_eq!(humans.len(), 3, "reach guard: one Human for each opponent");
    assert_all_tapped(&runner, &humans);
    assert_eq!(
        seats_in_creation_order(&runner, &humans),
        players_attacked(&[P1, P2, P3])
    );
}

/// CR 508.4 + CR 608.2c: Ainok Strike Leader attacking P1 in four players.
#[test]
fn ainok_strike_leader_goblins_attack_each_opponent() {
    let mut runner = per_opponent_board(4, |s| {
        s.add_creature_from_oracle(P0, "Ainok Strike Leader", 2, 2, AINOK_STRIKE_LEADER)
            .id()
    });
    drain(&mut runner, TriggerOrder::AsOffered);

    let goblins = tokens_named(&runner, "Goblin");
    assert_eq!(
        goblins.len(),
        3,
        "reach guard: one Goblin for each opponent"
    );
    assert_all_tapped(&runner, &goblins);
    assert_eq!(
        seats_in_creation_order(&runner, &goblins),
        players_attacked(&[P1, P2, P3])
    );
}

/// CR 508.4 + CR 608.2c: Endless Foot Assault, a creature attacking P1.
#[test]
fn endless_foot_assault_ninjas_attack_each_opponent() {
    let mut runner = per_opponent_board(4, |s| {
        s.add_enchantment_from_oracle(P0, "Endless Foot Assault", ENDLESS_FOOT_ASSAULT)
            .from_oracle_text_with_keywords(&["Squad"], ENDLESS_FOOT_ASSAULT);
        s.add_creature(P0, "Grizzly Bears", 2, 2).id()
    });
    drain(&mut runner, TriggerOrder::AsOffered);

    let ninjas = tokens_named(&runner, "Ninja");
    assert_eq!(ninjas.len(), 3, "reach guard: one Ninja for each opponent");
    assert_all_tapped(&runner, &ninjas);
    assert_eq!(
        seats_in_creation_order(&runner, &ninjas),
        players_attacked(&[P1, P2, P3])
    );
}

/// CR 508.4 + CR 608.2c: Stampede Surfer attacking P1 in four players.
#[test]
fn stampede_surfer_boars_attack_each_opponent() {
    let mut runner = per_opponent_board(4, |s| {
        s.add_creature(P0, "Stampede Surfer", 2, 2)
            .from_oracle_text_with_keywords(&["Haste"], STAMPEDE_SURFER)
            .id()
    });
    drain(&mut runner, TriggerOrder::AsOffered);

    let boars = tokens_named(&runner, "Boar");
    assert_eq!(boars.len(), 3, "reach guard: one Boar for each opponent");
    assert_all_tapped(&runner, &boars);
    assert_eq!(
        seats_in_creation_order(&runner, &boars),
        players_attacked(&[P1, P2, P3])
    );
}

/// CR 508.4 + CR 608.2c: Veteran Soldier grants its trigger to a commander
/// creature, which attacks P1 in four players (everyone at 20 life).
#[test]
fn veteran_soldier_soldiers_attack_each_opponent() {
    let mut runner = per_opponent_board(4, |s| {
        s.add_enchantment_from_oracle(P0, "Veteran Soldier", VETERAN_SOLDIER);
        s.add_creature(P0, "Commander Bear", 2, 2).commander().id()
    });
    drain(&mut runner, TriggerOrder::AsOffered);

    let soldiers = tokens_named(&runner, "Soldier");
    assert_eq!(
        soldiers.len(),
        3,
        "reach guard: the granted trigger fired and made one Soldier for each opponent"
    );
    assert_all_tapped(&runner, &soldiers);
    assert_eq!(
        seats_in_creation_order(&runner, &soldiers),
        players_attacked(&[P1, P2, P3])
    );
}

/// CR 104.3a + CR 608.2h: P3 concedes before combat, so the opponents counted
/// when the trigger resolves are P1 and P2 only.
#[test]
fn a_departed_opponent_is_not_counted_for_the_tokens() {
    let mut scenario = GameScenario::new_n_player(4, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let adeline = scenario
        .add_creature(P0, "Adeline, Resplendent Cathar", 3, 3)
        .from_oracle_text_with_keywords(&["Vigilance"], ADELINE)
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(adeline, AttackTarget::Player(P1))])
        .expect("P0 attacks P1");
    drain(&mut runner, TriggerOrder::AsOffered);

    assert_eq!(
        runner.state().eliminated_players,
        vec![P3],
        "reach guard: P3 left the game"
    );
    let humans = tokens_named(&runner, "Human");
    assert_eq!(
        humans.len(),
        2,
        "reach guard: one Human for each remaining opponent"
    );
    assert_eq!(
        seats_in_creation_order(&runner, &humans),
        players_attacked(&[P1, P2])
    );
}

/// CR 508.4: "that player or a planeswalker they control" allows the player's
/// planeswalker, but no such choice is offered, so each Human attacks its
/// player and none attacks P1's Jace.
#[test]
fn adeline_humans_attack_players_not_their_planeswalkers() {
    let mut jace = None;
    let mut runner = per_opponent_board(3, |s| {
        jace = Some(
            s.add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, JACE_BELEREN)
                .id(),
        );
        s.add_creature(P0, "Adeline, Resplendent Cathar", 3, 3)
            .from_oracle_text_with_keywords(&["Vigilance"], ADELINE)
            .id()
    });
    let jace = jace.expect("Jace was staged");
    drain(&mut runner, TriggerOrder::AsOffered);

    assert_eq!(
        runner.state().objects[&jace].zone,
        Zone::Battlefield,
        "reach guard: P1's planeswalker is on the battlefield"
    );
    assert_eq!(runner.state().objects[&jace].controller, P1);
    let humans = tokens_named(&runner, "Human");
    assert_eq!(humans.len(), 2, "reach guard: one Human for each opponent");
    assert_eq!(
        seats_in_creation_order(&runner, &humans),
        players_attacked(&[P1, P2])
    );
}

/// CR 614.1a: a token doubler doubles the one batch; the extra tokens are
/// distributed evenly over the opponents (Parallel Lives ruling: what the effect
/// specifies for the original tokens is true of the additional ones).
#[test]
fn doubled_per_opponent_tokens_are_shared_out_evenly() {
    let mut runner = per_opponent_board(4, |s| {
        s.add_enchantment_from_oracle(P0, "Endless Foot Assault", ENDLESS_FOOT_ASSAULT)
            .from_oracle_text_with_keywords(&["Squad"], ENDLESS_FOOT_ASSAULT);
        s.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
        s.add_creature(P0, "Grizzly Bears", 2, 2).id()
    });
    drain(&mut runner, TriggerOrder::AsOffered);

    let ninjas = tokens_named(&runner, "Ninja");
    assert_eq!(ninjas.len(), 6, "reach guard: the batch was doubled");
    assert_all_tapped(&runner, &ninjas);
    for opponent in [P1, P2, P3] {
        assert_eq!(
            attacking_player_count(&runner, &ninjas, opponent),
            2,
            "two Ninjas attack {opponent:?}"
        );
    }
}

/// Two doublers quadruple the batch, and each opponent gets four.
#[test]
fn twice_doubled_per_opponent_tokens_are_shared_out_evenly() {
    let mut runner = per_opponent_board(4, |s| {
        s.add_enchantment_from_oracle(P0, "Endless Foot Assault", ENDLESS_FOOT_ASSAULT)
            .from_oracle_text_with_keywords(&["Squad"], ENDLESS_FOOT_ASSAULT);
        s.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
        s.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
        s.add_creature(P0, "Grizzly Bears", 2, 2).id()
    });
    drain(&mut runner, TriggerOrder::AsOffered);

    let ninjas = tokens_named(&runner, "Ninja");
    assert_eq!(ninjas.len(), 12, "reach guard: the batch was quadrupled");
    assert_all_tapped(&runner, &ninjas);
    for opponent in [P1, P2, P3] {
        assert_eq!(
            attacking_player_count(&runner, &ninjas, opponent),
            4,
            "four Ninjas attack {opponent:?}"
        );
    }
}

/// Ainok Strike Leader with the +1/+1 counter replacements that raise an
/// ordering prompt for every Goblin: P0 attacks P1 in four players.
fn counter_pause_board() -> GameRunner {
    per_opponent_board(4, |s| {
        s.add_creature_from_oracle(P0, "Grumgully, the Generous", 3, 3, GRUMGULLY);
        s.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
        s.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        s.add_creature_from_oracle(P0, "Ainok Strike Leader", 2, 2, AINOK_STRIKE_LEADER)
            .id()
    })
}

fn choose_first_replacement(runner: &mut GameRunner) {
    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            assert_eq!(candidates.len(), 2, "both counter doublers compete");
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("choose a replacement order");
        }
        other => panic!("expected the replacement-ordering prompt, got {other:?}"),
    }
}

/// Every Goblin pauses for a replacement ordering and is seated after the
/// answer, so the seat survives the continuation.
#[test]
fn per_opponent_tokens_are_seated_after_replacement_pauses() {
    let mut runner = counter_pause_board();
    let mut prompts = 0;
    drain_with(&mut runner, TriggerOrder::AsOffered, &mut |runner| {
        prompts += 1;
        choose_first_replacement(runner);
    });

    assert_eq!(prompts, 3, "reach guard: each Goblin raised a prompt");
    let goblins = tokens_named(&runner, "Goblin");
    assert_eq!(
        goblins.len(),
        3,
        "reach guard: one Goblin for each opponent"
    );
    for goblin in &goblins {
        assert_eq!(
            runner.state().objects[goblin]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&3),
            "reach guard: the counter replacements applied"
        );
    }
    assert_eq!(
        seats_in_creation_order(&runner, &goblins),
        players_attacked(&[P1, P2, P3])
    );
}

/// CR 608.2h + CR 508.4a: the opponents are counted once, when the trigger
/// resolves. P2 concedes at the first prompt, after the first Goblin exists and
/// before P2's own; P2's Goblin enters but is not attacking, and nobody else's
/// seat changes.
#[test]
fn an_opponent_who_leaves_mid_batch_keeps_a_token_that_is_not_attacking() {
    let mut runner = counter_pause_board();
    let mut prompts = 0;
    drain_with(&mut runner, TriggerOrder::AsOffered, &mut |runner| {
        prompts += 1;
        if prompts == 1 {
            runner
                .act(GameAction::Concede { player_id: P2 })
                .expect("P2 concedes");
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ),
                "reach guard: the prompt is still pending after the concession"
            );
        }
        choose_first_replacement(runner);
    });

    assert_eq!(
        runner.state().eliminated_players,
        vec![P2],
        "reach guard: P2 left the game"
    );
    let goblins = tokens_named(&runner, "Goblin");
    assert_eq!(
        goblins.len(),
        3,
        "reach guard: the list was fixed at resolution"
    );
    assert_eq!(
        seats_in_creation_order(&runner, &goblins),
        vec![
            Some(AttackTarget::Player(P1)),
            None,
            Some(AttackTarget::Player(P3))
        ]
    );
}

// ---------------------------------------------------------------------------
// The resolvers themselves: a destination that names each player a filter
// lists makes the token count that list's length, whatever count expression the
// instruction carries.
// ---------------------------------------------------------------------------

fn each_player(filter: PlayerFilter) -> TokenAttackDestination {
    TokenAttackDestination::EachPlayer {
        filter,
        scope: NamedDefenderScope::Player,
    }
}

fn goblin_effect_counting(destination: TokenAttackDestination, count: QuantityExpr) -> Effect {
    let mut effect = goblin_effect(destination);
    if let Effect::Token {
        count: effect_count,
        ..
    } = &mut effect
    {
        *effect_count = count;
    }
    effect
}

/// CR 508.4: the copy resolver lists the players once, so a count that
/// disagrees with the list does not change how many copies are made.
#[test]
fn copy_tokens_are_created_and_seated_for_each_listed_player() {
    let (mut runner, host, staged) = carrier_board_of(4, |scenario| {
        vec![scenario.add_creature(P0, "Grizzly Bears", 2, 2).id()]
    });
    let mut effect = copy_effect(each_player(PlayerFilter::Opponent), vec![]);
    if let Effect::CopyTokenOf { count, .. } = &mut effect {
        *count = QuantityExpr::Fixed { value: 1 };
    }
    let ability = ResolvedAbility::new(effect, vec![TargetRef::Object(staged[0])], host, P0);
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");

    let copies: Vec<ObjectId> = tokens_named(&runner, "Grizzly Bears");
    assert_eq!(copies.len(), 3, "one copy for each opponent");
    assert_eq!(
        seats_in_creation_order(&runner, &copies),
        players_attacked(&[P1, P2, P3])
    );
}

/// CR 508.4: the token resolver makes one token for each listed player even
/// when the instruction's count says five.
#[test]
fn tokens_are_created_and_seated_for_each_listed_player_not_the_count() {
    let (mut runner, host, _) = carrier_board(|_| vec![]);
    let ability = ResolvedAbility::new(
        goblin_effect_counting(
            each_player(PlayerFilter::Opponent),
            QuantityExpr::Fixed { value: 5 },
        ),
        vec![],
        host,
        P0,
    );
    let mut events = Vec::new();
    token::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");

    let goblins = tokens_named(&runner, "Goblin");
    assert_eq!(goblins.len(), 2, "one Goblin for each opponent");
    assert_eq!(
        seats_in_creation_order(&runner, &goblins),
        players_attacked(&[P1, P2])
    );
}

/// A list that includes the controller seats the controller's own token nowhere
/// and does not hand it to anyone else.
#[test]
fn a_listed_player_who_cannot_be_attacked_keeps_a_token_that_is_not_attacking() {
    let (mut runner, host, _) = carrier_board(|_| vec![]);
    let all_players = QuantityExpr::Ref {
        qty: QuantityRef::PlayerCount {
            filter: PlayerFilter::All,
        },
    };
    let ability = ResolvedAbility::new(
        goblin_effect_counting(each_player(PlayerFilter::All), all_players),
        vec![],
        host,
        P0,
    );
    let mut events = Vec::new();
    token::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");

    let goblins = tokens_named(&runner, "Goblin");
    assert_eq!(goblins.len(), 3, "one Goblin for each player");
    assert_eq!(
        seats_in_creation_order(&runner, &goblins),
        vec![
            None,
            Some(AttackTarget::Player(P1)),
            Some(AttackTarget::Player(P2))
        ]
    );
}

/// An instruction whose tokens do not enter attacking lists its players for the
/// count and seats none of them. The twin attacks in the same board.
#[test]
fn listed_tokens_that_do_not_enter_attacking_attack_nothing() {
    let (mut runner, host, _) = carrier_board(|_| vec![]);
    let mut effect = goblin_effect(each_player(PlayerFilter::Opponent));
    if let Effect::Token {
        enters_attacking, ..
    } = &mut effect
    {
        *enters_attacking = false;
    }
    let ability = ResolvedAbility::new(effect, vec![], host, P0);
    let mut events = Vec::new();
    token::resolve(runner.state_mut(), &ability, &mut events).expect("resolve");
    let waiting = tokens_named(&runner, "Goblin");
    assert_eq!(waiting.len(), 2, "one Goblin for each opponent");
    assert_eq!(seats_in_creation_order(&runner, &waiting), vec![None, None]);

    let twin = ResolvedAbility::new(
        goblin_effect(each_player(PlayerFilter::Opponent)),
        vec![],
        host,
        P0,
    );
    token::resolve(runner.state_mut(), &twin, &mut events).expect("resolve");
    let attacking_goblins: Vec<ObjectId> = tokens_named(&runner, "Goblin")
        .into_iter()
        .filter(|id| !waiting.contains(id))
        .collect();
    assert_eq!(
        seats_in_creation_order(&runner, &attacking_goblins),
        players_attacked(&[P1, P2]),
        "reach guard: the same instruction attacks when its tokens enter attacking"
    );
}

const PEREGRIN_TOOK: &str = "If one or more tokens would be created under your control, those tokens plus an additional Food token are created instead.";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

/// The Goblins' seats, in creation order, after Ainok Strike Leader's trigger
/// resolves in a four-player game where P0 controls the counter doublers that
/// pause each Goblin and, optionally, Peregrin Took. With `halved`, P1 also
/// controls Halving Season and P0 controls Parallel Lives, so the batch is
/// sized by replacements applied around Peregrin Took's. Replacement prompts are
/// answered in the order Halving Season, Peregrin Took, Parallel Lives.
fn goblin_seats_with_a_replacement_adding_a_token(
    peregrin: bool,
    halved: bool,
) -> (usize, usize, Vec<Option<AttackTarget>>) {
    let mut runner = per_opponent_board(4, |s| {
        if halved {
            s.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
            s.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
        }
        s.add_creature_from_oracle(P0, "Grumgully, the Generous", 3, 3, GRUMGULLY);
        s.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
        s.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        if peregrin {
            s.add_creature_from_oracle(P0, "Peregrin Took", 2, 2, PEREGRIN_TOOK);
        }
        s.add_creature_from_oracle(P0, "Ainok Strike Leader", 2, 2, AINOK_STRIKE_LEADER)
            .id()
    });
    let order = ["Halving Season", "Peregrin Took", "Parallel Lives"];
    let mut prompts = 0;
    drain_with(&mut runner, TriggerOrder::AsOffered, &mut |runner| {
        let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
        else {
            panic!("expected a replacement prompt");
        };
        let index = order
            .iter()
            .find_map(|name| candidates.iter().position(|c| c.source_name == *name))
            .unwrap_or(0);
        prompts += 1;
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("choose a replacement order");
    });
    assert!(
        prompts > 0,
        "reach guard: a replacement prompt paused the batch"
    );
    let goblins = tokens_named(&runner, "Goblin");
    let foods = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.card_types.subtypes.iter().any(|s| s == "Food")
        })
        .count();
    let seats = seats_in_creation_order(&runner, &goblins);
    (goblins.len(), foods, seats)
}

/// CR 508.4 + CR 608.2c: the batch's own tokens are counted for seating. P1's
/// Halving Season halves the three Goblins to one and Parallel Lives doubles
/// that to two, so the Goblins go to P1 and P2 even though Peregrin Took's
/// appended Foods lengthen the shared created-token list.
/// Revert-failing: both Goblins attack P1.
#[test]
fn per_opponent_tokens_keep_their_seats_when_a_replacement_adds_tokens_and_the_batch_pauses() {
    let (goblins, foods, seats) = goblin_seats_with_a_replacement_adding_a_token(true, true);
    assert_eq!(goblins, 2, "reach guard: halved, then doubled");
    assert!(foods > 0, "reach guard: Peregrin Took added Food tokens");
    assert_eq!(seats, players_attacked(&[P1, P2]));
}

/// The same board without Peregrin Took: no token is added to the event.
#[test]
fn halved_and_doubled_per_opponent_tokens_keep_their_seats_when_the_batch_pauses() {
    let (goblins, foods, seats) = goblin_seats_with_a_replacement_adding_a_token(false, true);
    assert_eq!(goblins, 2, "reach guard: halved, then doubled");
    assert_eq!(foods, 0, "reach guard: no replacement added a token");
    assert_eq!(seats, players_attacked(&[P1, P2]));
}

/// Peregrin Took with the batch's size unchanged: every opponent's Goblin is
/// seated at that opponent.
#[test]
fn per_opponent_tokens_keep_their_seats_when_a_replacement_adds_tokens_and_the_size_is_unchanged() {
    let (goblins, foods, seats) = goblin_seats_with_a_replacement_adding_a_token(true, false);
    assert_eq!(goblins, 3, "reach guard: one Goblin for each opponent");
    assert!(foods > 0, "reach guard: Peregrin Took added Food tokens");
    assert_eq!(seats, players_attacked(&[P1, P2, P3]));
}
