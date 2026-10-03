//! Goader identity of the grafted goad designation.
//!
//! CR 701.15b: "A goaded creature attacks each combat if able and attacks a
//! player other than the controller of the permanent, spell, or ability that
//! caused it to be goaded if able." The goader is therefore the INSTALLING
//! player — never the goaded creature's own controller.
//!
//! Two route shapes graft `StaticMode::Goaded` onto the goaded creature through
//! `ContinuousModification::AddStaticMode { Goaded }`:
//!   1. a resolution-installed transient effect — "The tokens are goaded for the
//!      rest of the game." (Life of the Party, Rendmaw, The War Games). The goader
//!      is the controller of the triggered ability (CR 113.8), fixed when the
//!      effect began;
//!   2. a static ability of another permanent — "Enchanted creature is goaded."
//!      (The Sound of Drums). The goader is the Aura's CURRENT controller
//!      (CR 303.4e, CR 109.5, CR 611.3a).
//!
//! Untouched sibling routes are pinned as preservation rows: a printed
//! `StaticMode::Goaded` on the goading permanent itself (Psychic Impetus) and the
//! `Effect::Goad` designation (`goaded_by`, Laser Screwdriver).
//!
//! Fixture discipline (from `goad_badge_defender_gated_anchor.rs`): a REJECTED
//! declaration leaves the declare-attackers step open, so it may be followed by
//! another; every SUCCESSFUL declaration runs on a clone of the state.

use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::effects::attach::attach_to;
use engine::game::functioning_abilities::active_static_definitions;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ContinuousModification, Duration, Effect, StaticDefinition, TargetFilter,
};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Life of the Party (NCC), Scryfall verbatim.
const LIFE_OF_THE_PARTY: &str = "First strike, trample, haste\nWhenever this creature attacks, it gets +X/+0 until end of turn, where X is the number of creatures you control.\nWhen this creature enters, if it's not a token, each opponent creates a token that's a copy of it. The tokens are goaded for the rest of the game. (They attack each combat if able and attack a player other than you if able.)";

/// The Sound of Drums (WHO), Scryfall verbatim.
const THE_SOUND_OF_DRUMS: &str = "Enchant creature\nEnchanted creature is goaded.\nIf enchanted creature would deal combat damage to a permanent or player, it deals double that damage instead.\n{2}{R}: Return this card from your graveyard to your hand.";

/// Psychic Impetus (CLU), Scryfall verbatim.
const PSYCHIC_IMPETUS: &str = "Enchant creature\nEnchanted creature gets +2/+2 and is goaded. (It attacks each combat if able and attacks a player other than you if able.)\nWhenever enchanted creature attacks, you scry 2.";

/// Laser Screwdriver (WHO), Scryfall verbatim.
const LASER_SCREWDRIVER: &str = "{T}: Add one mana of any color.\n{1}, {T}: Tap target artifact.\n{2}, {T}: Surveil 1. (Look at the top card of your library. You may put that card into your graveyard.)\n{3}, {T}: Goad target creature. (Until your next turn, it attacks each combat if able and attacks a player other than you if able.)";

// --- shared helpers -----------------------------------------------------------

fn refresh_layers(state: &mut GameState) {
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

/// The functioning `Goaded` statics on `id` (grafted or printed).
fn goaded_defs(state: &GameState, id: ObjectId) -> Vec<StaticDefinition> {
    active_static_definitions(state, &state.objects[&id])
        .filter(|sd| sd.mode == StaticMode::Goaded)
        .cloned()
        .collect()
}

/// CR 613.1b: a layer-2 control change with no expiry, installed exactly as
/// `game/effects/gain_control.rs` installs one.
fn give_control(runner: &mut GameRunner, object: ObjectId, new_controller: PlayerId) {
    let state = runner.state_mut();
    state.add_transient_continuous_effect(
        object,
        new_controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: object },
        vec![ContinuousModification::ChangeController],
        None,
    );
    refresh_layers(state);
}

/// Put the game in `active`'s declare-attackers step with the production payload
/// (`build_declare_attackers_waiting_for`). Returns the payload's attackers.
fn arm_combat(runner: &mut GameRunner, active: PlayerId) -> Vec<ObjectId> {
    let state = runner.state_mut();
    state.active_player = active;
    state.priority_player = active;
    state.phase = Phase::DeclareAttackers;
    refresh_layers(state);
    state.waiting_for = build_declare_attackers_waiting_for(state);
    match &state.waiting_for {
        WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids,
            ..
        } => {
            assert_eq!(*player, active, "the payload must belong to {active:?}");
            valid_attacker_ids.clone()
        }
        other => panic!("expected DeclareAttackers, got {other:?}"),
    }
}

fn declare(runner: &mut GameRunner, attacks: &[(ObjectId, PlayerId)]) -> Result<(), String> {
    let attacks: Vec<(ObjectId, AttackTarget)> = attacks
        .iter()
        .map(|&(id, p)| (id, AttackTarget::Player(p)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// A successful declaration advances past the step, so it runs on a clone.
fn declare_on_clone(runner: &GameRunner, attacks: &[(ObjectId, PlayerId)]) -> Result<(), String> {
    let mut probe = GameRunner::from_state(runner.state().clone());
    declare(&mut probe, attacks)
}

/// CR 508.1d: declaring no attackers is refused — the reach-guard proving the
/// goad requirement reached combat.
fn assert_must_attack(runner: &mut GameRunner, label: &str) {
    assert!(
        declare(runner, &[]).is_err(),
        "REACH-GUARD ({label}): CR 701.15b + CR 508.1d — the goaded creature must \
         attack if able, so an empty declaration must be refused"
    );
}

/// Life of the Party tokens on the battlefield, keyed by controller.
fn party_token(state: &GameState, controller: PlayerId) -> ObjectId {
    let tokens: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.name == "Life of the Party" && obj.is_token && obj.controller == controller
        })
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "{controller:?} must control exactly one Life of the Party token"
    );
    tokens[0]
}

/// 3 players; P0 casts Life of the Party. Returns (runner, original, P1 token,
/// P2 token).
fn life_of_the_party_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let life = scenario
        .add_creature_to_hand(P0, "Life of the Party", 0, 1)
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text_with_keywords(&["first strike", "trample", "haste"], LIFE_OF_THE_PARTY)
        .id();
    let mut runner = scenario.build();
    runner.cast(life).resolve();
    refresh_layers(runner.state_mut());
    let p1_token = party_token(runner.state(), P1);
    let p2_token = party_token(runner.state(), P2);
    (runner, life, p1_token, p2_token)
}

/// The resolution-installed goad effects on `id`: `(effect controller, duration)`
/// of every transient effect adding `StaticMode::Goaded` to exactly that object.
fn goad_tces(state: &GameState, id: ObjectId) -> Vec<(PlayerId, Duration)> {
    state
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            tce.affected == TargetFilter::SpecificObject { id }
                && tce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::Goaded
                        }
                    )
                })
        })
        .map(|tce| (tce.controller, tce.duration.clone()))
        .collect()
}

// --- V1.1 / V1.2: resolution-installed graft -----------------------------------

/// V1.1 — CR 701.15b + CR 113.8: the goader of a Life of the Party token is the
/// controller of the triggered ability that goaded it (the caster P0), not the
/// token's controller.
#[test]
fn life_of_the_party_token_is_goaded_by_the_caster() {
    let (mut runner, life, p1_token, p2_token) = life_of_the_party_board();

    // Reach-guards: each opponent controls one token carrying a functioning
    // Goaded def; the caster's original carries none.
    for token in [p1_token, p2_token] {
        assert_eq!(
            goad_tces(runner.state(), token),
            vec![(P0, Duration::Permanent)],
            "REACH-GUARD: every opponent's token carries the resolution-installed goad              effect, controlled by the caster P0, for the rest of the game"
        );
        assert!(
            !goaded_defs(runner.state(), token).is_empty(),
            "REACH-GUARD: each Life of the Party token must carry a functioning Goaded static"
        );
    }
    assert!(
        goaded_defs(runner.state(), life).is_empty(),
        "the caster's original Life of the Party is not goaded"
    );
    let state = runner.state();
    assert_eq!(state.objects[&p1_token].owner, P1);
    assert_eq!(state.objects[&p1_token].controller, P1);

    let mut p2_view = GameRunner::from_state(runner.state().clone());

    // P1's combat.
    let attackers = arm_combat(&mut runner, P1);
    assert!(
        attackers.contains(&p1_token),
        "REACH-GUARD: P1's token must be an eligible attacker"
    );
    assert_must_attack(&mut runner, "V1.1 P1");
    assert!(
        declare(&mut runner, &[(p1_token, P0)]).is_err(),
        "CR 701.15b + CR 508.1d: P1's token was goaded by P0 (the controller of \
         Life of the Party's triggered ability, CR 113.8), so it must attack a \
         player other than P0 while P2 is attackable"
    );
    declare_on_clone(&runner, &[(p1_token, P2)])
        .expect("CR 701.15b: attacking P2 obeys both goad requirements");

    // Symmetric arm: P2's combat.
    let attackers = arm_combat(&mut p2_view, P2);
    assert!(attackers.contains(&p2_token));
    assert_must_attack(&mut p2_view, "V1.1 P2");
    assert!(
        declare(&mut p2_view, &[(p2_token, P0)]).is_err(),
        "CR 701.15b: P2's token was goaded by P0 as well"
    );
    declare_on_clone(&p2_view, &[(p2_token, P1)])
        .expect("CR 701.15b: attacking P1 obeys both goad requirements");
}

/// V1.2a — CR 701.15b: the goader is fixed when the goad is applied; a later
/// change of control of the goaded token does not move it to the new controller.
#[test]
fn life_of_the_party_goader_survives_control_change_of_the_token() {
    let (mut runner, _life, p1_token, p2_token) = life_of_the_party_board();
    give_control(&mut runner, p1_token, P2);
    {
        let obj = &runner.state().objects[&p1_token];
        assert_eq!(obj.owner, P1, "owner is unchanged");
        assert_eq!(obj.controller, P2, "REACH-GUARD: the layer-2 steal applied");
    }

    let attackers = arm_combat(&mut runner, P2);
    assert!(attackers.contains(&p1_token) && attackers.contains(&p2_token));
    assert_must_attack(&mut runner, "V1.2a");
    // P2's own token attacks P1 (legal for it) in every declaration below, so
    // only the stolen token's target varies.
    assert!(
        declare(&mut runner, &[(p1_token, P0), (p2_token, P1)]).is_err(),
        "CR 701.15b: the stolen token is still goaded by P0 (owner P1, \
         controller P2, goader P0), so it must attack a player other than P0"
    );
    declare_on_clone(&runner, &[(p1_token, P1), (p2_token, P1)])
        .expect("CR 701.15b: attacking P1 obeys the stolen token's goad");
}

/// V1.2b — CR 701.15b + CR 113.8: the resolution-installed goader is a snapshot
/// of the ability's controller; a later change of control of the SOURCE (the
/// original Life of the Party) does not move it either.
#[test]
fn life_of_the_party_goader_survives_control_change_of_the_source() {
    let (mut runner, life, p1_token, _p2_token) = life_of_the_party_board();
    give_control(&mut runner, life, P1);
    assert_eq!(
        runner.state().objects[&life].controller,
        P1,
        "REACH-GUARD: the original Life of the Party is now controlled by P1"
    );

    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&p1_token));
    assert_must_attack(&mut runner, "V1.2b");
    assert!(
        declare(&mut runner, &[(p1_token, P0)]).is_err(),
        "CR 701.15b + CR 113.8: the goader stays P0 — the controller of the \
         ability when it goaded — not the source's current controller P1"
    );
    declare_on_clone(&runner, &[(p1_token, P2)]).expect("CR 701.15b: attacking P2 obeys the goad");
}

// --- V1.3 / V1.4: graft from a static on another permanent ---------------------

/// Attach `aura` to `host` and re-evaluate layers.
fn attach_aura(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    attach_to(runner.state_mut(), aura, host);
    assert_eq!(
        runner.state().objects[&aura]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(host),
        "REACH-GUARD: the Aura must legally attach to its host"
    );
    refresh_layers(runner.state_mut());
}

/// 4 players; Victim owned by P0, controlled by P1, enchanted by The Sound of
/// Drums owned and controlled by P2. Returns (runner, victim, aura).
fn drums_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P0, "Victim", 2, 2)
        .controlled_by(P1)
        .haste()
        .id();
    let aura = scenario
        .add_enchantment_from_oracle(P2, "The Sound of Drums", THE_SOUND_OF_DRUMS)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    attach_aura(&mut runner, aura, victim);
    assert_eq!(
        goaded_defs(runner.state(), victim).len(),
        1,
        "REACH-GUARD: the enchanted creature carries one functioning Goaded static"
    );
    (runner, victim, aura)
}

/// V1.3a — CR 701.15b + CR 303.4e + CR 109.5: "Enchanted creature is goaded."
/// is goaded by the Aura's controller (P2), not the creature's controller (P1)
/// or owner (P0).
#[test]
fn sound_of_drums_goader_is_the_auras_controller() {
    let (mut runner, victim, _aura) = drums_board();
    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.3a");
    assert!(
        declare(&mut runner, &[(victim, P2)]).is_err(),
        "CR 701.15b + CR 303.4e: the Aura's controller P2 goaded the creature, so \
         it must attack a player other than P2 while P0/P3 are attackable"
    );
    declare_on_clone(&runner, &[(victim, P0)]).expect("attacking P0 obeys the goad");
    declare_on_clone(&runner, &[(victim, P3)]).expect("attacking P3 obeys the goad");
}

/// V1.3b — CR 701.15b: a change of control of the enchanted creature does not
/// move the goader (still the Aura's controller P2).
#[test]
fn sound_of_drums_goader_survives_control_change_of_the_creature() {
    let (mut runner, victim, _aura) = drums_board();
    give_control(&mut runner, victim, P3);
    {
        let obj = &runner.state().objects[&victim];
        assert_eq!(obj.owner, P0);
        assert_eq!(obj.controller, P3, "REACH-GUARD: the layer-2 steal applied");
    }
    let attackers = arm_combat(&mut runner, P3);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.3b");
    assert!(
        declare(&mut runner, &[(victim, P2)]).is_err(),
        "CR 701.15b: owner P0, controller P3, goader P2 — the creature must attack \
         a player other than P2"
    );
    declare_on_clone(&runner, &[(victim, P1)]).expect("attacking P1 obeys the goad");
}

/// V1.3c — CR 109.5 + CR 611.3a: the static route names the Aura's CURRENT
/// controller; when the Aura itself is stolen (owner P2, controller P3), the
/// goader becomes P3.
#[test]
fn sound_of_drums_goader_follows_the_auras_current_controller() {
    let (mut runner, victim, aura) = drums_board();
    give_control(&mut runner, aura, P3);
    {
        let obj = &runner.state().objects[&aura];
        assert_eq!(obj.owner, P2);
        assert_eq!(obj.controller, P3, "REACH-GUARD: the Aura was stolen");
        assert_eq!(
            obj.attached_to.and_then(|t| t.as_object()),
            Some(victim),
            "REACH-GUARD: the stolen Aura still enchants the creature"
        );
    }
    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.3c");
    assert!(
        declare(&mut runner, &[(victim, P3)]).is_err(),
        "CR 109.5 + CR 611.3a: the Aura's current controller P3 is the goader"
    );
    declare_on_clone(&runner, &[(victim, P2)])
        .expect("CR 611.3a: the Aura's former controller P2 no longer goads it");
}

/// Board for the multi-Aura rows: Victim owned and controlled by P1, one The
/// Sound of Drums per entry of `aura_controllers` attached to it.
fn multi_drums_board(players: u8, aura_controllers: &[PlayerId]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let auras: Vec<ObjectId> = aura_controllers
        .iter()
        .map(|&p| {
            scenario
                .add_enchantment_from_oracle(p, "The Sound of Drums", THE_SOUND_OF_DRUMS)
                .with_subtypes(vec!["Aura"])
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for aura in auras {
        attach_aura(&mut runner, aura, victim);
    }
    (runner, victim)
}

fn anchors(defs: &[StaticDefinition]) -> Vec<Option<PlayerId>> {
    let mut a: Vec<Option<PlayerId>> = defs.iter().map(|d| d.source_controller).collect();
    a.sort_unstable_by_key(|p| p.map(|p| p.0));
    a
}

/// V1.4a — CR 701.15c: a creature goaded by two distinct players carries one
/// requirement per goader.
#[test]
fn two_distinct_goaders_both_bind() {
    let (mut runner, victim) = multi_drums_board(4, &[P0, P2]);
    let defs = goaded_defs(runner.state(), victim);
    assert_eq!(
        anchors(&defs),
        vec![Some(P0), Some(P2)],
        "CR 701.15c: two distinct goaders yield two grafted Goaded statics"
    );
    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.4a");
    for goader in [P0, P2] {
        assert!(
            declare(&mut runner, &[(victim, goader)]).is_err(),
            "CR 701.15c: attacking goader {goader:?} disobeys that goader's \
             requirement while P3 is attackable"
        );
    }
    declare_on_clone(&runner, &[(victim, P3)])
        .expect("attacking the one player neither goader is obeys both requirements");
}

/// V1.4b — CR 701.15d: the same player goading again adds nothing.
#[test]
fn same_goader_twice_collapses() {
    let (mut runner, victim) = multi_drums_board(3, &[P0, P0]);
    let defs = goaded_defs(runner.state(), victim);
    assert_eq!(
        anchors(&defs),
        vec![Some(P0)],
        "CR 701.15d: the same goader twice yields one grafted Goaded static"
    );
    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.4b");
    assert!(
        declare(&mut runner, &[(victim, P0)]).is_err(),
        "CR 701.15b: goaded by P0, the creature must attack P2"
    );
    declare_on_clone(&runner, &[(victim, P2)]).expect("attacking P2 obeys the goad");
}

/// V1.4c — CR 701.15b + CR 508.1d: a creature goaded by its own controller
/// carries the must-attack requirement only. NOT revert-failing by construction
/// (the base goader is the carrier's controller, which here IS the goader); it
/// pins wrong fixes that read the Aura's OWNER (P2) or drop the requirement.
#[test]
fn controller_is_also_goader_keeps_only_must_attack() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P0, "Victim", 2, 2)
        .controlled_by(P1)
        .haste()
        .id();
    let aura = scenario
        .add_enchantment_from_oracle(P2, "The Sound of Drums", THE_SOUND_OF_DRUMS)
        .with_subtypes(vec!["Aura"])
        .controlled_by(P1)
        .id();
    let mut runner = scenario.build();
    attach_aura(&mut runner, aura, victim);
    assert_eq!(runner.state().objects[&aura].owner, P2);

    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.4c");
    declare_on_clone(&runner, &[(victim, P0)])
        .expect("CR 701.15b: goaded by its own controller, any opponent may be attacked");
    declare_on_clone(&runner, &[(victim, P2)])
        .expect("CR 701.15b: the Aura's owner P2 is not the goader");
}

/// V1.4d — CR 701.15b + CR 701.15c: one Aura controlled by the creature's
/// controller (P1) and one by P0 — only P0's goad avoids a player.
#[test]
fn own_controller_and_another_player_both_goad() {
    let (mut runner, victim) = multi_drums_board(3, &[P1, P0]);
    assert_eq!(
        anchors(&goaded_defs(runner.state(), victim)),
        vec![Some(P0), Some(P1)],
        "CR 701.15c: two distinct goaders yield two grafted Goaded statics"
    );
    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.4d");
    assert!(
        declare(&mut runner, &[(victim, P0)]).is_err(),
        "CR 701.15b: P0 goaded the creature, so it must attack P2"
    );
    declare_on_clone(&runner, &[(victim, P2)]).expect("attacking P2 obeys both goads");
}

// --- V1.5: untouched routes (preservation) ---------------------------------------

/// V1.5a — preservation: a printed `StaticMode::Goaded` on the Aura itself
/// (Psychic Impetus) already names the Aura's controller (CR 701.15b, CR 109.5).
#[test]
fn psychic_impetus_printed_goad_names_the_auras_controller() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P0, "Victim", 2, 2)
        .controlled_by(P1)
        .haste()
        .id();
    let aura = scenario
        .add_enchantment_from_oracle(P2, "Psychic Impetus", PSYCHIC_IMPETUS)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    attach_aura(&mut runner, aura, victim);

    // Route reach-guard: the printed def lives on the Aura; the creature carries
    // no grafted Goaded static.
    assert!(
        !goaded_defs(runner.state(), aura).is_empty(),
        "REACH-GUARD: Psychic Impetus carries a printed StaticMode::Goaded def"
    );
    assert!(
        goaded_defs(runner.state(), victim).is_empty(),
        "REACH-GUARD: the printed route grafts nothing onto the creature"
    );

    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.5a");
    assert!(
        declare(&mut runner, &[(victim, P2)]).is_err(),
        "CR 701.15b: Psychic Impetus's controller P2 is the goader"
    );
    declare_on_clone(&runner, &[(victim, P0)]).expect("attacking P0 obeys the goad");
}

/// V1.5b — preservation: the `Effect::Goad` designation route names the
/// activator (CR 113.8, CR 701.15a/b).
#[test]
fn laser_screwdriver_goad_names_the_activator() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P2,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let screwdriver = scenario
        .add_artifact_from_oracle(P2, "Laser Screwdriver", LASER_SCREWDRIVER)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P2;
        state.priority_player = P2;
        state.waiting_for = WaitingFor::Priority { player: P2 };
    }
    let goad_index = runner.state().objects[&screwdriver]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::Goad { .. }))
        .expect("REACH-GUARD: Laser Screwdriver exposes an Effect::Goad ability");
    runner
        .activate(screwdriver, goad_index)
        .target_object(victim)
        .resolve();
    assert_eq!(
        runner.state().objects[&victim]
            .goaded_by
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![P2],
        "REACH-GUARD: CR 701.15a — the activator P2 goaded the creature"
    );

    let attackers = arm_combat(&mut runner, P1);
    assert!(attackers.contains(&victim));
    assert_must_attack(&mut runner, "V1.5b");
    assert!(
        declare(&mut runner, &[(victim, P2)]).is_err(),
        "CR 701.15b: the activator P2 is the goader"
    );
    declare_on_clone(&runner, &[(victim, P0)]).expect("attacking P0 obeys the goad");
}

// --- V1.6: layer-level anchor ------------------------------------------------------

fn assert_graft_anchor(state: &GameState, carrier: ObjectId, expected: PlayerId, label: &str) {
    let defs = goaded_defs(state, carrier);
    assert_eq!(
        defs.len(),
        1,
        "REACH-GUARD ({label}): one grafted Goaded def"
    );
    let def = &defs[0];
    assert_eq!(
        def.affected,
        Some(TargetFilter::SelfRef),
        "REACH-GUARD ({label}): the graft is anchored to its carrier"
    );
    assert_eq!(
        def.source_controller,
        Some(expected),
        "CR 701.15b ({label}): the grafted Goaded def names its installing player"
    );
    assert_eq!(
        def.source_object, None,
        "({label}): the directing-source stamp is intentionally absent for Goaded"
    );
}

/// V1.6 — the layer-6 graft (CR 613.1f) mints the installing player as the
/// Goaded def's anchor on both routes.
#[test]
fn grafted_goaded_def_carries_the_installing_player() {
    let (runner, _life, p1_token, _p2_token) = life_of_the_party_board();
    assert_graft_anchor(runner.state(), p1_token, P0, "Life of the Party token");

    let (runner, victim, _aura) = drums_board();
    assert_graft_anchor(runner.state(), victim, P2, "The Sound of Drums");

    let (mut runner, victim, aura) = drums_board();
    give_control(&mut runner, aura, P3);
    assert_graft_anchor(runner.state(), victim, P3, "stolen The Sound of Drums");
}
