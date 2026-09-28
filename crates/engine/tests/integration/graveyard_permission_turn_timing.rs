//! Graveyard cast permissions and whose turn it is.
//!
//! CR 601.3 + CR 702.8a + CR 117.1a: a permission with no turn words lets a
//! Flash or instant card be cast from the graveyard on any turn. "During your
//! turn" / "(once) during each of your turns" is the permission's own
//! `StaticCondition::DuringYourTurn` (CR 102.1), evaluated by every offer,
//! admission and prepare consumer through `active_static_definitions`, not a
//! blanket active-player gate in the casting pipeline.
//!
//! CR 113.1b + CR 109.5: a permission a resolved effect grants to a PLAYER is
//! that player's ability, so its "your turn" is the grantee's turn. A nested
//! condition is honored only when every leaf is proven to evaluate against the
//! holder. Others fail closed.

use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, StaticCondition, StaticDefinition};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const MULDROTHA: &str = "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard. (If a card has multiple permanent types, choose one as you play it.)";
const LURRUS: &str = "Lifelink\nOnce during each of your turns, you may cast a permanent spell with mana value 2 or less from your graveyard.";
const KARADOR: &str = "This spell costs {1} less to cast for each creature card in your graveyard.\nOnce during each of your turns, you may cast a creature spell from your graveyard.";
const KAGHA: &str = "Whenever Kagha attacks, it gains deathtouch until end of turn. Mill two cards. (Put the top two cards of your library into your graveyard.)\nOnce during each of your turns, you may play a land or cast a permanent spell from among cards in your graveyard that were put there from your library this turn.";
const BROODSHIP: &str = "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 8+.)\n3+ | You may play an additional land on each of your turns.\n8+ | Flying\nOnce during each of your turns, you may cast a permanent spell from your graveyard by sacrificing a land in addition to paying its other costs.";
const FESTIVAL: &str = "During your turn, you may cast instant and sorcery spells from your graveyard by paying 1 life in addition to their other costs.\nIf a card or token would be put into your graveyard from anywhere, exile it instead.\n{1}{R}: Sacrifice this enchantment.";
const AGENDA: &str = "You can't cast more than one spell each turn.\nYou may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere, exile it instead.";
const MAGUS: &str = "{2}{B}, {T}, Exile this creature: Until end of turn, you may play lands and cast spells from your graveyard. If a card would be put into your graveyard from anywhere this turn, exile that card instead.";
/// Jaya Ballard's −8 emblem text, created here by a spell through the
/// production `CreateEmblem` path.
const JAYA_EMBLEM_SPELL: &str = "You get an emblem with \"You may cast instant and sorcery spells from your graveyard. If a spell cast this way would be put into your graveyard, exile it instead.\"";
const RISEN_EXECUTIONER: &str = "This creature can't block.\nOther Zombie creatures you control get +1/+1.\nYou may cast this creature from your graveyard if you pay {1} more to cast it for each other creature card in your graveyard.";
/// Synthetic, accepted: a player-scoped grant whose nested permission says
/// "during your turn".
const TURN_GRANT: &str = "Target player gains \"During your turn, you may cast instant and sorcery spells from your graveyard\" until end of turn.";
/// Synthetic, accepted: a player-scoped grant whose nested permission reads a
/// board predicate (`IsPresent`).
const ZOMBIE_GRANT: &str = "Target player gains \"You may cast creature spells from your graveyard as long as you control a Zombie\" until end of turn.";
const OPP_ZOMBIE_GRANT: &str = "Target player gains \"You may cast creature spells from your graveyard as long as an opponent controls a Zombie\" until end of turn.";

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn offered_to(runner: &GameRunner, player: PlayerId, card: ObjectId) -> bool {
    let on_offer = spell_objects_available_to_cast(runner.state(), player).contains(&card);
    assert_eq!(
        on_offer,
        can_cast_object_now(runner.state(), player, card),
        "offer list and can_cast_object_now must agree for {card:?}"
    );
    on_offer
}

/// CR 601.2e: an illegal proposed cast is rejected and the card stays put.
fn assert_cast_rejected(runner: &mut GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: card,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::default(),
    });
    assert!(result.is_err(), "the cast must be rejected, got {result:?}");
    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
}

fn flash_creature_in_graveyard(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
) -> ObjectId {
    scenario
        .add_creature_to_graveyard(owner, name, 2, 2)
        .flash()
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn instant_in_graveyard(scenario: &mut GameScenario, owner: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_spell_to_graveyard(owner, name, true)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn priority_holder(runner: &GameRunner) -> Option<PlayerId> {
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => Some(player),
        _ => None,
    }
}

/// Pass priority until `active` is the active player and holds priority, then
/// (if `holder != active`) let the active player pass once so `holder` has
/// priority in that same step (CR 117.3d).
fn to_turn_with_priority(runner: &mut GameRunner, active: PlayerId, holder: PlayerId) {
    for _ in 0..40 {
        if runner.state().active_player == active && priority_holder(runner) == Some(active) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority toward the target turn");
    }
    assert_eq!(runner.state().active_player, active, "reach: turn reached");
    if holder != active {
        runner.act(GameAction::PassPriority).expect("active passes");
    }
    assert_eq!(
        priority_holder(runner),
        Some(holder),
        "reach: {holder:?} holds priority"
    );
}

fn black_and_two(player_units: &mut GameRunner, player: PlayerId) {
    for color in [ManaType::Black, ManaType::Colorless, ManaType::Colorless] {
        let _ = player_units
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

// ── Printed "your turns" permissions stay on their controller's turn ─────────

/// Muldrotha: own turn → offered; the opponent's turn with a Flash permanent
/// card → not offered, and a direct cast is rejected.
#[test]
fn muldrotha_permission_is_limited_to_your_turns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario
        .add_creature_from_oracle(P0, "Muldrotha, the Gravetide", 6, 6, MULDROTHA)
        .as_legendary();
    let flash = flash_creature_in_graveyard(&mut scenario, P0, "Flash Bear");
    let mut runner = scenario.build();

    assert!(
        offered_to(&runner, P0, flash),
        "reach: offered on my own turn"
    );
    to_turn_with_priority(&mut runner, P1, P0);
    assert!(
        !offered_to(&runner, P0, flash),
        "\"during each of your turns\": not on the opponent's turn"
    );
    assert_cast_rejected(&mut runner, flash);
}

/// Lurrus: own turn → offered; the opponent's turn → not offered, and the cast
/// is rejected.
#[test]
fn lurrus_permission_is_limited_to_your_turns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario
        .add_creature_from_oracle(P0, "Lurrus of the Dream-Den", 3, 2, LURRUS)
        .as_legendary();
    let flash = flash_creature_in_graveyard(&mut scenario, P0, "Flash Bear");
    let mut runner = scenario.build();

    assert!(
        offered_to(&runner, P0, flash),
        "reach: offered on my own turn"
    );
    to_turn_with_priority(&mut runner, P1, P0);
    assert!(
        !offered_to(&runner, P0, flash),
        "\"once during each of your turns\": not on the opponent's turn"
    );
    assert_cast_rejected(&mut runner, flash);
}

/// Exploration Broodship at station 8+: own turn → offered; the opponent's turn
/// → not offered. Below the threshold, not offered even on its own turn. The
/// station gate composes with, rather than overwriting, the turn restriction.
#[test]
fn broodship_permission_needs_station_and_your_turn() {
    let build = |counters: u32| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        let ship = scenario
            .add_artifact_from_oracle(P0, "Exploration Broodship", BROODSHIP)
            .with_subtypes(vec!["Spacecraft"])
            .from_oracle_text(BROODSHIP)
            .id();
        scenario.with_counter(ship, CounterType::Generic("charge".to_string()), counters);
        scenario.add_basic_land(P0, ManaColor::Green);
        let flash = flash_creature_in_graveyard(&mut scenario, P0, "Flash Bear");
        (scenario.build(), flash)
    };

    let (runner, flash) = build(3);
    assert!(
        !offered_to(&runner, P0, flash),
        "below station 8+ the permission doesn't exist"
    );

    let (mut runner, flash) = build(8);
    assert!(offered_to(&runner, P0, flash), "reach: station 8+, my turn");
    to_turn_with_priority(&mut runner, P1, P0);
    assert!(
        !offered_to(&runner, P0, flash),
        "\"once during each of your turns\" survives the station gate"
    );
}

// ── Permissions without turn words work on any turn ─────────────────────────

/// Yawgmoth's Agenda (battlefield grant, no turn words): an instant is cast
/// from the graveyard on the opponent's turn and resolves.
#[test]
fn yawgmoths_agenda_allows_an_instant_on_the_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.add_enchantment_from_oracle(P0, "Yawgmoth's Agenda", AGENDA);
    let instant = instant_in_graveyard(&mut scenario, P0, "Graveyard Instant");
    let mut runner = scenario.build();

    to_turn_with_priority(&mut runner, P1, P0);
    assert!(offered_to(&runner, P0, instant), "offered on P1's turn");
    let outcome = runner.cast(instant).resolve();
    assert_eq!(
        outcome.zone_of(instant),
        Zone::Exile,
        "it resolved, and Agenda exiles it instead of the graveyard"
    );
}

/// Jaya Ballard's emblem (command-zone static route): a graveyard instant is
/// cast on the opponent's turn and resolves. Reach: not offered before the
/// emblem exists.
#[test]
fn jaya_emblem_allows_an_instant_on_the_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let emblem_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Emblem Spell", true, JAYA_EMBLEM_SPELL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let instant = instant_in_graveyard(&mut scenario, P0, "Graveyard Instant");
    let mut runner = scenario.build();

    assert!(!offered_to(&runner, P0, instant), "reach: no emblem yet");
    let outcome = runner.cast(emblem_spell).resolve();
    assert_eq!(
        outcome.state().command_zone.len(),
        1,
        "reach: emblem created"
    );

    to_turn_with_priority(&mut runner, P1, P0);
    assert!(offered_to(&runner, P0, instant), "offered on P1's turn");
    let outcome = runner.cast(instant).resolve();
    assert_eq!(
        outcome.zone_of(instant),
        Zone::Exile,
        "it resolved, and the emblem exiles it"
    );
}

/// Magus of the Will (resolution-created TCE route), activated on the
/// opponent's turn: the grant has no turn words, so an instant is cast from the
/// graveyard in that window and resolves. Reach: not offered before activation.
#[test]
fn magus_of_the_will_activated_on_the_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let magus = scenario
        .add_creature_from_oracle(P0, "Magus of the Will", 3, 3, MAGUS)
        .id();
    let instant = instant_in_graveyard(&mut scenario, P0, "Graveyard Instant");
    let mut runner = scenario.build();

    to_turn_with_priority(&mut runner, P1, P0);
    assert!(
        !offered_to(&runner, P0, instant),
        "reach: not before activation"
    );
    black_and_two(&mut runner, P0);
    let outcome = runner.activate(magus, 0).resolve();
    assert_eq!(
        outcome.state().transient_continuous_effects.len(),
        1,
        "reach: the grant is installed"
    );
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert_eq!(priority_holder(&runner), Some(P0));
    assert!(
        offered_to(&runner, P0, instant),
        "offered in Magus's window"
    );
    let outcome = runner.cast(instant).resolve();
    assert_eq!(
        outcome.zone_of(instant),
        Zone::Exile,
        "it resolved, and Magus's replacement exiles it"
    );
}

/// Accepted synthetic shape: "once each turn" names no turn, so it isn't limited
/// to its controller's turns.
#[test]
fn once_each_turn_disjunctive_permission_works_on_any_turn() {
    const TEXT: &str =
        "Once each turn, you may play a land or cast a creature spell from your graveyard.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.add_enchantment_from_oracle(P0, "Test Relic", TEXT);
    let flash = flash_creature_in_graveyard(&mut scenario, P0, "Flash Bear");
    let second = flash_creature_in_graveyard(&mut scenario, P0, "Second Flash Bear");
    let mut runner = scenario.build();

    to_turn_with_priority(&mut runner, P1, P0);
    assert!(offered_to(&runner, P0, flash), "offered on P1's turn");
    assert!(
        offered_to(&runner, P0, second),
        "reach: both offered before"
    );
    let outcome = runner.cast(flash).resolve();
    assert_eq!(outcome.zone_of(flash), Zone::Battlefield);
    // CR 601.2a: the off-turn cast was made under this permission, so it spent
    // the permission's once-per-turn slot.
    assert!(
        !offered_to(&runner, P0, second),
        "\"once each turn\": the slot is spent"
    );
}

// ── Player-scoped (transient) grants: the holder is the grantee ─────────────

fn turn_grant_scenario(at: Phase) -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(at);
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Grant Spell", true, TURN_GRANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mine = instant_in_graveyard(&mut scenario, P0, "My Instant");
    let theirs = instant_in_graveyard(&mut scenario, P1, "Their Instant");
    (scenario, grant, mine, theirs)
}

/// w4: my turn, I grant myself "during your turn" → offered, and the cast
/// resolves.
#[test]
fn turn_grant_to_myself_on_my_turn_is_usable() {
    let (scenario, grant, mine, _) = turn_grant_scenario(Phase::PreCombatMain);
    let mut runner = scenario.build();
    assert!(
        !offered_to(&runner, P0, mine),
        "reach: not before the grant"
    );
    runner.cast(grant).target_player(P0).resolve();
    assert!(offered_to(&runner, P0, mine), "my turn, my grant");
    let outcome = runner.cast(mine).resolve();
    outcome.assert_life_delta(P0, 1);
}

/// w1: my turn, I grant P1 "during your turn" → P1's graveyard instant is NOT
/// offered to P1, because it isn't P1's turn. The source's controller (me) is
/// the active player, so a source-bound evaluation would wrongly pass.
#[test]
fn turn_grant_to_the_opponent_on_my_turn_is_not_usable() {
    let (scenario, grant, _, theirs) = turn_grant_scenario(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let outcome = runner.cast(grant).target_player(P1).resolve();
    assert_eq!(
        outcome.state().transient_continuous_effects.len(),
        1,
        "reach: the grant is installed"
    );
    assert!(
        !offered_to(&runner, P1, theirs),
        "\"your turn\" is the holder P1's turn"
    );
}

/// w2: on P1's turn, I grant P1 "during your turn" → P1 may cast its graveyard
/// instant, and it resolves.
#[test]
fn turn_grant_to_the_opponent_on_their_turn_is_usable() {
    let (scenario, grant, _, theirs) = turn_grant_scenario(Phase::End);
    let mut runner = scenario.build();
    to_turn_with_priority(&mut runner, P1, P0);
    runner.cast(grant).target_player(P1).resolve();
    assert_eq!(priority_holder(&runner), Some(P1), "CR 117.3b");
    assert!(offered_to(&runner, P1, theirs), "P1's turn, P1's grant");
    let outcome = runner.cast(theirs).resolve();
    outcome.assert_life_delta(P1, 1);
}

/// w3: on P1's turn, I grant myself "during your turn" → not usable, because
/// it isn't my turn.
#[test]
fn turn_grant_to_myself_on_the_opponents_turn_is_not_usable() {
    let (scenario, grant, mine, _) = turn_grant_scenario(Phase::End);
    let mut runner = scenario.build();
    to_turn_with_priority(&mut runner, P1, P0);
    runner.cast(grant).target_player(P0).resolve();
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert_eq!(priority_holder(&runner), Some(P0));
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        1,
        "reach: the grant is installed"
    );
    assert!(!offered_to(&runner, P0, mine), "not my turn");
}

/// SHAPE: the quoted-grant route nests the board predicate as `IsPresent`
/// inside a player-scoped `GrantStaticAbility{GraveyardCastPermission}`.
#[test]
fn zombie_grant_nests_is_present() {
    let parsed = parse_oracle_text(
        ZOMBIE_GRANT,
        "Grant Spell",
        &[],
        &strings(&["Instant"]),
        &[],
    );
    let Some(Effect::GenericEffect {
        static_abilities, ..
    }) = parsed.abilities.first().map(|a| &*a.effect)
    else {
        panic!("expected a GenericEffect, got {:?}", parsed.abilities);
    };
    let nested: Vec<&StaticDefinition> = static_abilities
        .iter()
        .flat_map(|s| s.modifications.iter())
        .filter_map(|m| match m {
            ContinuousModification::GrantStaticAbility { definition } => Some(&**definition),
            _ => None,
        })
        .collect();
    let [permission] = nested.as_slice() else {
        panic!("expected one granted static, got {nested:?}");
    };
    assert!(matches!(
        permission.mode,
        StaticMode::GraveyardCastPermission { .. }
    ));
    assert!(
        matches!(
            permission.condition,
            Some(StaticCondition::IsPresent { .. })
        ),
        "got {:?}",
        permission.condition
    );
}

/// Player-scoped grant with a board predicate, source ≠ holder: I grant P1
/// "as long as you control a Zombie". `IsPresent` isn't holder-bound, so the
/// permission fails closed on BOTH boards:
/// - I control the Zombie (a source-bound evaluation would wrongly pass): not
///   offered.
/// - P1 controls the Zombie: also not offered. This is the documented
///   fail-closed limitation (a holder-bound `IsPresent` is a logged follow-up).
///   Refusing is safer than guessing whose Zombies count.
#[test]
fn player_grant_with_a_board_condition_fails_closed() {
    for zombie_controller in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let grant = scenario
            .add_spell_to_hand_from_oracle(P0, "Grant Spell", true, ZOMBIE_GRANT)
            .with_mana_cost(ManaCost::zero())
            .id();
        scenario
            .add_creature(zombie_controller, "Walking Corpse", 2, 2)
            .with_subtypes(vec!["Zombie"]);
        let theirs = flash_creature_in_graveyard(&mut scenario, P1, "Flash Bear");
        let mut runner = scenario.build();
        let outcome = runner.cast(grant).target_player(P1).resolve();
        assert_eq!(
            outcome.state().transient_continuous_effects.len(),
            1,
            "reach: the grant is installed"
        );
        assert!(
            !offered_to(&runner, P1, theirs),
            "Zombie under {zombie_controller:?}: a non-holder-bound gate fails closed"
        );
    }
}

/// Sibling: "as long as an opponent controls a Zombie", granted to P1, with the
/// Zombie under P1 (an opponent of the granting spell's controller, but not of
/// the holder). Fails closed, so it's not offered.
#[test]
fn player_grant_with_an_opponent_board_condition_fails_closed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Grant Spell", true, OPP_ZOMBIE_GRANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario
        .add_creature(P1, "Walking Corpse", 2, 2)
        .with_subtypes(vec!["Zombie"]);
    let theirs = flash_creature_in_graveyard(&mut scenario, P1, "Flash Bear");
    let mut runner = scenario.build();
    let outcome = runner.cast(grant).target_player(P1).resolve();
    assert_eq!(
        outcome.state().transient_continuous_effects.len(),
        1,
        "reach: the grant is installed"
    );
    assert!(!offered_to(&runner, P1, theirs), "fails closed");
}

// ── Parse shapes ─────────────────────────────────────────────────────────────

fn graveyard_permission_conditions(
    text: &str,
    name: &str,
    types: &[&str],
) -> Vec<Option<StaticCondition>> {
    graveyard_permission_conditions_with_subtypes(text, name, types, &[])
}

fn graveyard_permission_conditions_with_subtypes(
    text: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> Vec<Option<StaticCondition>> {
    parse_oracle_text(text, name, &[], &strings(types), &strings(subtypes))
        .statics
        .into_iter()
        .filter(|s| matches!(s.mode, StaticMode::GraveyardCastPermission { .. }))
        .map(|s| s.condition)
        .collect()
}

fn contains_during_your_turn(condition: &StaticCondition) -> bool {
    match condition {
        StaticCondition::DuringYourTurn => true,
        StaticCondition::And { conditions } => conditions.iter().any(contains_during_your_turn),
        _ => false,
    }
}

/// SHAPE: every printed "during (each of) your turn(s)" permission branch
/// carries `DuringYourTurn`. "Once each turn" does not.
#[test]
fn your_turns_permissions_carry_during_your_turn() {
    for (name, text, types) in [
        ("Muldrotha, the Gravetide", MULDROTHA, &["Creature"][..]),
        ("Lurrus of the Dream-Den", LURRUS, &["Creature"][..]),
        ("Karador, Ghost Chieftain", KARADOR, &["Creature"][..]),
        ("Kagha, Shadow Archdruid", KAGHA, &["Creature"][..]),
        ("Festival of Embers", FESTIVAL, &["Enchantment"][..]),
    ] {
        let conditions = graveyard_permission_conditions(text, name, types);
        assert_eq!(
            conditions.len(),
            1,
            "{name}: one permission, got {conditions:?}"
        );
        assert_eq!(
            conditions[0],
            Some(StaticCondition::DuringYourTurn),
            "{name}"
        );
    }

    let broodship = graveyard_permission_conditions_with_subtypes(
        BROODSHIP,
        "Exploration Broodship",
        &["Artifact"],
        &["Spacecraft"],
    );
    let [Some(StaticCondition::And { conditions })] = broodship.as_slice() else {
        panic!("Broodship: expected And[station, DuringYourTurn], got {broodship:?}");
    };
    assert!(
        conditions.iter().any(contains_during_your_turn),
        "{conditions:?}"
    );
    assert!(
        conditions
            .iter()
            .any(|c| matches!(c, StaticCondition::HasCounters { minimum: 8, .. })),
        "{conditions:?}"
    );

    let once_each_turn = graveyard_permission_conditions(
        "Once each turn, you may play a land or cast a creature spell from your graveyard.",
        "Test Relic",
        &["Enchantment"],
    );
    assert_eq!(
        once_each_turn,
        vec![None],
        "\"once each turn\" names no turn"
    );
}

/// Leg C: a printed gate the parser can't fully model declines the whole
/// permission, so the card isn't castable from the graveyard at the wrong
/// price. Positive control: a modelled gate keeps its permission.
#[test]
fn unmodelled_permission_gate_declines_the_permission() {
    let risen =
        graveyard_permission_conditions(RISEN_EXECUTIONER, "Risen Executioner", &["Creature"]);
    assert!(
        risen.is_empty(),
        "Risen Executioner's cost-shaped gate is unmodelled: no permission, got {risen:?}"
    );
    let parsed = parse_oracle_text(
        RISEN_EXECUTIONER,
        "Risen Executioner",
        &[],
        &strings(&["Creature"]),
        &strings(&["Zombie", "Warrior"]),
    );
    assert!(
        serde_json::to_string(&parsed.abilities)
            .unwrap()
            .contains("Unimplemented"),
        "the line falls through to an honest Unimplemented"
    );

    let oathsworn = graveyard_permission_conditions(
        "This creature enters tapped.\nYou may cast this card from your graveyard if you gained life this turn.",
        "Oathsworn Vampire",
        &["Creature"],
    );
    assert!(
        matches!(
            oathsworn.as_slice(),
            [Some(StaticCondition::QuantityComparison { .. })]
        ),
        "control: a modelled gate keeps its permission, got {oathsworn:?}"
    );
}

/// An unconsumed linked "If …" rider after the permission declines the whole
/// permission. Otherwise the lead's populated `DuringYourTurn` condition would
/// discharge the swallowed-clause expectation while the rider silently did
/// nothing. Positive control: a rider a modelled parser consumes (Kess's
/// stack-exit replacement) keeps the permission.
#[test]
fn unconsumed_if_rider_declines_the_permission() {
    const EDGAR: &str = "Once during each of your turns, you may cast an artifact spell from your graveyard. If you cast a spell this way, that artifact enters tapped.\nTools — Whenever Edgar attacks, it gets +X/+0 until end of turn, where X is the greatest mana value among artifacts you control.";
    const LOOKOUT: &str = "Once during each of your turns, you may cast a noncreature, non-Aura permanent spell from your graveyard. If you do, it perpetually becomes a 2/1 Rat creature in addition to its other types.";
    const KESS: &str = "Flying\nOnce during each of your turns, you may cast an instant or sorcery spell from your graveyard. If a spell cast this way would be put into your graveyard, exile it instead.";
    for (name, text, types) in [
        ("Edgar, Master Machinist", EDGAR, &["Creature"][..]),
        (
            "Mischievous Lookout",
            LOOKOUT,
            &["Enchantment", "Creature"][..],
        ),
    ] {
        assert!(
            graveyard_permission_conditions(text, name, types).is_empty(),
            "{name}: the unmodelled rider declines the permission"
        );
        let parsed = parse_oracle_text(text, name, &[], &strings(types), &[]);
        assert!(
            serde_json::to_string(&parsed.abilities)
                .unwrap()
                .contains("Unimplemented"),
            "{name}: the line falls through to an honest Unimplemented"
        );
    }
    assert_eq!(
        graveyard_permission_conditions(KESS, "Kess, Dissident Mage", &["Creature"]),
        vec![Some(StaticCondition::DuringYourTurn)],
        "control: a consumed rider keeps the permission"
    );
}

// ── Unconsumed permission tails fail closed ──────────────────────────────────

const MULDROTHA_IF: &str = "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard if you control a Zombie.";
const ONCE_UNLESS: &str = "Once during each of your turns, you may cast a creature spell from your graveyard unless you control a Zombie.";
const COORDINATED_AS_LONG_AS: &str = "Until end of turn, you may play lands and cast spells from your graveyard as long as you control a Zombie.";
const COORDINATED: &str =
    "Until end of turn, you may play lands and cast spells from your graveyard.";

fn warnings_of(text: &str, types: &[&str]) -> Vec<String> {
    parse_oracle_text(text, "Tail Probe", &[], &strings(types), &[])
        .parse_warnings
        .iter()
        .map(|w| format!("{w:?}"))
        .collect()
}

/// A permission text with an unconsumed tail ("… if you control a Zombie",
/// "… unless you control a Zombie") yields NO permission. That's an honest
/// Unimplemented, not a grant carrying only the lead's turn restriction.
/// Control: the exact printed Muldrotha / Karador text still yields the
/// permission with `DuringYourTurn`.
#[test]
fn permission_with_an_unconsumed_tail_yields_no_grant() {
    for (text, types) in [
        (MULDROTHA_IF, &["Creature"][..]),
        (ONCE_UNLESS, &["Creature"][..]),
    ] {
        let conditions = graveyard_permission_conditions(text, "Tail Probe", types);
        assert!(
            conditions.is_empty(),
            "{text}: no permission, got {conditions:?}"
        );
        let parsed = parse_oracle_text(text, "Tail Probe", &[], &strings(types), &[]);
        assert!(
            serde_json::to_string(&parsed.abilities)
                .unwrap()
                .contains("Unimplemented"),
            "{text}: honest Unimplemented; warnings {:?}",
            warnings_of(text, types)
        );
    }
    assert_eq!(
        graveyard_permission_conditions(MULDROTHA, "Muldrotha, the Gravetide", &["Creature"]),
        vec![Some(StaticCondition::DuringYourTurn)]
    );
    assert_eq!(
        graveyard_permission_conditions(KARADOR, "Karador, Ghost Chieftain", &["Creature"]),
        vec![Some(StaticCondition::DuringYourTurn)]
    );
}

/// Full dispatch on the same board. The unconsumed-tail permissions offer no
/// graveyard cast on their controller's own turn, while the printed sibling
/// (Karador) does.
#[test]
fn permission_with_an_unconsumed_tail_offers_no_cast() {
    for text in [MULDROTHA_IF, ONCE_UNLESS] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        scenario.add_creature_from_oracle(P0, "Tail Probe", 2, 2, text);
        let flash = flash_creature_in_graveyard(&mut scenario, P0, "Flash Bear");
        let runner = scenario.build();
        assert!(
            !offered_to(&runner, P0, flash),
            "{text}: nothing is granted"
        );
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario
        .add_creature_from_oracle(P0, "Karador, Ghost Chieftain", 3, 4, KARADOR)
        .as_legendary();
    let flash = flash_creature_in_graveyard(&mut scenario, P0, "Flash Bear");
    let runner = scenario.build();
    assert!(offered_to(&runner, P0, flash), "control: Karador grants it");
}

fn coordinated_grant_on_opponents_turn(text: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Will Probe", true, text)
        .with_mana_cost(ManaCost::zero())
        .id();
    let instant = instant_in_graveyard(&mut scenario, P0, "Graveyard Instant");
    let mut runner = scenario.build();
    to_turn_with_priority(&mut runner, P1, P0);
    runner.cast(grant).resolve();
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert_eq!(
        priority_holder(&runner),
        Some(P0),
        "reach: P0 priority on P1's turn"
    );
    (runner, instant)
}

/// CR 611.2a + CR 601.3: a coordinated "play lands and cast spells from your
/// graveyard" grant whose sentence carries an unconsumed gate ("as long as you
/// control a Zombie") grants NOTHING. Otherwise the gate is dropped and the
/// grant admits an off-turn graveyard instant with no Zombie. Control: the
/// ungated Yawgmoth's Will-style grant resolved the same way admits it, and the
/// cast resolves.
#[test]
fn coordinated_grant_with_an_unconsumed_gate_grants_nothing() {
    let (mut runner, instant) = coordinated_grant_on_opponents_turn(COORDINATED_AS_LONG_AS);
    assert!(!offered_to(&runner, P0, instant), "no Zombie, no cast");
    assert_cast_rejected(&mut runner, instant);
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "no grant is installed for the gated sentence"
    );

    let (mut runner, instant) = coordinated_grant_on_opponents_turn(COORDINATED);
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        1,
        "control: the ungated grant is installed"
    );
    assert!(
        offered_to(&runner, P0, instant),
        "control: offered off-turn"
    );
    let outcome = runner.cast(instant).resolve();
    outcome.assert_life_delta(P0, 1);
}

/// Measurement pin: the gated coordinated sentence keeps its honest parse gap
/// (an Unimplemented and a swallowed-clause warning), so coverage stays
/// unsupported.
#[test]
fn coordinated_grant_with_an_unconsumed_gate_stays_unsupported() {
    let parsed = parse_oracle_text(
        COORDINATED_AS_LONG_AS,
        "Will Probe",
        &[],
        &strings(&["Instant"]),
        &[],
    );
    let json = serde_json::to_string(&parsed.abilities).unwrap();
    assert!(
        !json.contains("GraveyardCastPermission"),
        "no permission is synthesized: {json}"
    );
    assert!(json.contains("Unimplemented"), "honest gap: {json}");
}
