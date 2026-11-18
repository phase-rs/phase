//! Issue #10266: kicked Galadriel's Dismissal must phase out each creature the
//! targeted player controls, not the targeted player themself.
//!
//! https://github.com/phase-rs/phase/issues/10266

use engine::game::game_object::PhaseOutCause;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Galadriel's Dismissal (LTC 500) — verbatim Oracle text.
const GALADRIELS_DISMISSAL: &str = "Kicker {2}{W} (You may pay an additional {2}{W} as you cast \
     this spell.)\nTarget creature phases out. If this spell was kicked, each creature target \
     player controls phases out instead. (Treat phased-out creatures and anything attached to \
     them as though they don't exist until their controller's next turn.)";

/// Bolt Bend — verbatim Oracle text.
const BOLT_BEND: &str = "This spell costs {3} less to cast if you control a creature with power 4 \
     or greater.\nChange the target of target spell or ability with a single target.";

/// Teferi, Timeless Voyager — verbatim Oracle text.
const TEFERI_TIMELESS_VOYAGER: &str =
    "[+1]: Draw a card.\n[−3]: Put target creature on top of its \
     owner's library.\n[−8]: Each creature target opponent controls phases out. Until the end of \
     your next turn, they can't phase in.";

fn white_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]); n]
}

fn red_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); n]
}

/// CR 702.26b: only a permanent can phase out. The kicked branch's player
/// target is the controller anchor for "each creature target player controls",
/// so the player must remain phased in while every creature they control
/// phases out. Reverting the `collect_player_targets` player-scope gate makes
/// the player phased out and fails this test.
#[test]
fn kicked_galadriels_dismissal_phases_out_only_the_target_players_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Galadriel's Dismissal", true, GALADRIELS_DISMISSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let caster_creature = scenario.add_creature(P0, "Caster Bear", 2, 2).id();
    let target_creature = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    let other_target_creature = scenario.add_creature(P1, "Target Elf", 1, 1).id();
    // {W} base plus {2}{W} kicker.
    scenario.with_mana_pool(P0, white_mana(4));

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_player(P1)
        .resolve();

    let state = outcome.state();
    assert!(
        !state.players[P0.0 as usize].is_phased_out(),
        "the caster must never phase out"
    );
    assert!(
        !state.players[P1.0 as usize].is_phased_out(),
        "the targeted player is the controller anchor for the creature filter, not a phasing target"
    );
    assert!(
        state.objects[&target_creature].is_phased_out(),
        "the targeted player's first creature must phase out"
    );
    assert!(
        state.objects[&other_target_creature].is_phased_out(),
        "each creature the targeted player controls must phase out"
    );
    assert!(
        state.objects[&caster_creature].is_phased_in(),
        "the caster's creature must remain phased in"
    );
    assert!(
        state.stack.is_empty(),
        "the kicked spell must finish resolving, stack={:?}",
        state.stack.len()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must return priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// CR 115.7a + CR 115.7d: with the kicked branch reduced to its announced
/// player target, Bolt Bend can target the spell and must be offered the
/// player pool — never the unkicked base branch's creature pool. Retargeting
/// to the caster phases out the caster's creatures instead.
#[test]
fn kicked_galadriels_dismissal_retargets_through_the_player_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dismissal = scenario
        .add_spell_to_hand_from_oracle(P0, "Galadriel's Dismissal", true, GALADRIELS_DISMISSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let bolt_bend = scenario
        .add_spell_to_hand_from_oracle(P1, "Bolt Bend", true, BOLT_BEND)
        .id();
    let caster_creature = scenario.add_creature(P0, "Caster Bear", 2, 2).id();
    let target_creature = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    scenario.with_mana_pool(P0, white_mana(4));
    scenario.with_mana_pool(P1, red_mana(4));

    let mut runner = scenario.build();
    runner
        .cast(dismissal)
        .accept_optional()
        .target_player(P1)
        .commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 may respond");

    let outcome = runner.cast(bolt_bend).target_object(dismissal).resolve();
    let WaitingFor::RetargetChoice {
        legal_new_targets,
        slot_pools,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "Bolt Bend must present the retarget choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(
        legal_new_targets.contains(&TargetRef::Player(P0)),
        "the caster must be a legal replacement player, got {legal_new_targets:?}"
    );
    assert!(
        !legal_new_targets
            .iter()
            .any(|target| matches!(target, TargetRef::Object(_))),
        "the base branch's creature pool must not leak into the player slot: {legal_new_targets:?}"
    );
    assert!(
        slot_pools
            .first()
            .is_some_and(|pool| pool.contains(&TargetRef::Player(P0))),
        "the per-slot pool must be the companion player pool, got {slot_pools:?}"
    );

    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Player(P0))],
        })
        .expect("retarget the kicked spell to the caster");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert!(
        state.objects[&caster_creature].is_phased_out(),
        "the caster's creature is now the affected population"
    );
    assert!(
        state.objects[&target_creature].is_phased_in(),
        "the original target's creature must stay phased in"
    );
    assert!(
        !state.players[P0.0 as usize].is_phased_out(),
        "the retargeted player is still only the population anchor"
    );
}

/// CR 702.26b + CR 101.2: Teferi's follow-on "they can't phase in" must bind
/// the creatures that actually phased out, not the player-only anchor. At the
/// opponent's next untap step they remain phased out while the restriction is
/// active.
#[test]
fn teferi_timeless_voyager_restriction_binds_the_phased_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let teferi = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Teferi, Timeless Voyager",
            "Teferi",
            8,
            TEFERI_TIMELESS_VOYAGER,
        )
        .id();
    let first_opponent_creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let second_opponent_creature = scenario.add_creature(P1, "Opponent Elf", 1, 1).id();
    // A noncreature permanent controlled by the opponent: Teferi's creature
    // filter never phases it out, so it is an unrestricted phase-in control.
    let control_permanent = scenario
        .add_artifact_from_oracle(P1, "Control Relic", "")
        .id();
    let own_creature = scenario.add_creature(P0, "Own Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.activate(teferi, 2).target_player(P1).resolve();

    assert!(
        runner.state().objects[&first_opponent_creature].is_phased_out(),
        "the opponent's first creature must phase out"
    );
    assert!(
        runner.state().objects[&second_opponent_creature].is_phased_out(),
        "the opponent's second creature must phase out"
    );
    assert!(
        !runner.state().players[P1.0 as usize].is_phased_out(),
        "the targeted opponent is the population anchor, not a phasing target"
    );
    assert!(
        runner.state().objects[&own_creature].is_phased_in(),
        "the controller's own creature must remain phased in"
    );

    // A control creature phased out WITHOUT Teferi's restriction must phase in
    // at the same untap. This proves the untap actually ran, rather than the
    // test stopping at an earlier combat prompt.
    let mut setup_events = Vec::new();
    phase_out_object(
        runner.state_mut(),
        control_permanent,
        PhaseOutCause::Directly,
        &mut setup_events,
    );
    assert!(runner.state().objects[&control_permanent].is_phased_out());

    // Drive to the opponent's first upkeep after their untap. Declaring no
    // attackers handles the combat prompt `advance_to_phase` can stop on.
    let starting_turn = runner.state().turn_number;
    for _ in 0..96 {
        if runner.state().active_player == P1
            && runner.state().turn_number > starting_turn
            && runner.state().phase == Phase::Upkeep
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the opponent's untap");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            other => {
                panic!("unexpected waiting_for while advancing to the opponent's untap: {other:?}")
            }
        }
    }
    assert_eq!(
        runner.state().active_player,
        P1,
        "the loop must reach the opponent's turn"
    );
    assert!(
        runner.state().turn_number > starting_turn,
        "the opponent's untap must advance the turn number"
    );
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the opponent's untap must have completed"
    );
    assert!(
        runner.state().objects[&control_permanent].is_phased_in(),
        "an unrestricted phased-out permanent must phase in at the opponent's untap"
    );
    assert!(
        runner.state().objects[&first_opponent_creature].is_phased_out(),
        "the restriction must be bound to the first phased creature"
    );
    assert!(
        runner.state().objects[&second_opponent_creature].is_phased_out(),
        "the restriction must be bound to every phased creature during the opponent's untap"
    );
    assert!(
        runner.state().objects[&own_creature].is_phased_in(),
        "Teferi's restriction must not affect the controller's creature"
    );
}

/// CR 608.2c: a completed phase action with an EMPTY affected population must
/// forward an empty result to a following `ParentTarget` rider. It must not
/// fall back to the player-only anchor and install a restriction on the player.
#[test]
fn teferi_empty_creature_population_binds_no_player_restriction() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let teferi = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Teferi, Timeless Voyager",
            "Teferi",
            8,
            TEFERI_TIMELESS_VOYAGER,
        )
        .id();

    let mut runner = scenario.build();
    runner.activate(teferi, 2).target_player(P1).resolve();

    assert!(
        !runner.state().players[P1.0 as usize].is_phased_out(),
        "an empty creature population must not phase the player anchor"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "an empty phase result must bind no CantPhaseIn rider, got {:?}",
        runner.state().transient_continuous_effects
    );
}

/// The Doctor's Childhood Barn (WHO 579) — verbatim Oracle text.
const DOCTORS_CHILDHOOD_BARN: &str = "Creatures enter tapped.\nWhenever chaos ensues, for each \
     opponent, choose up to one target nonland permanent that opponent controls. Untap those \
     permanents. They phase out. They can't phase in for as long as The Doctor's Childhood Barn \
     remains face up. When a player planeswalks, those permanents phase in.";

/// Shuko (CHK 159) — verbatim Oracle text. Printed Equipment used as the
/// incidental attachment: it phases out indirectly with its host and phases
/// back in only along with that host (CR 702.26g).
const SHUKO: &str = "Equipped creature gets +1/+0.\nEquip {0}";

/// Cloudshift — verbatim Oracle text. A leave-and-return that keeps the storage
/// `ObjectId` and bumps the incarnation (CR 400.7).
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

const SPELLSKITE: &str = "{U/P}: Change a target of target spell or ability to this creature. \
({U/P} can be paid with either {U} or 2 life.)";

const DOOM_BLADE: &str = "Destroy target nonblack creature.";

/// Pacifism — verbatim Oracle text.
const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";

/// The printed Barn board: a planechase game with The Doctor's Childhood Barn
/// face up and a second plane waiting, P1 controlling the chosen host and the
/// incidental printed Shuko attached to it, P2 controlling the chosen Pacifism
/// (also attached, so the host's phase-out cascades it, CR 702.26h).
struct BarnBoard {
    runner: GameRunner,
    barn: ObjectId,
    host: ObjectId,
    aura: ObjectId,
    equipment: ObjectId,
}

/// Install The Doctor's Childhood Barn face up plus a second plane waiting in
/// the planar deck, on an already-built scenario. Returns `(barn, departure)`.
fn install_barn_planes(runner: &mut GameRunner) -> (ObjectId, ObjectId) {
    use engine::database::synthesis::synthesize_planechase;
    use engine::game::deck_loading::create_planar_deck_card;
    use engine::game::planechase::restamp_planar_objects_to_controller;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::card::CardFace;
    use engine::types::card_type::CoreType;

    let parsed = parse_oracle_text(
        DOCTORS_CHILDHOOD_BARN,
        "The Doctor's Childhood Barn",
        &[],
        &["Plane".to_string()],
        &[],
    );
    let mut face = CardFace {
        name: "The Doctor's Childhood Barn".to_string(),
        card_type: engine::types::card_type::CardType {
            core_types: vec![CoreType::Plane],
            ..Default::default()
        },
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..CardFace::default()
    };
    synthesize_planechase(&mut face);

    let state = runner.state_mut();
    let barn = create_planar_deck_card(state, &face, P0);
    state.planar_deck.retain(|id| *id != barn);
    state.objects.get_mut(&barn).unwrap().face_down = false;
    state.command_zone.push_back(barn);
    state.planar_controller = Some(P0);

    let mut departure = CardFace {
        name: "Quiet Departure".to_string(),
        card_type: engine::types::card_type::CardType {
            core_types: vec![CoreType::Plane],
            ..Default::default()
        },
        ..CardFace::default()
    };
    synthesize_planechase(&mut departure);
    let departure_plane = create_planar_deck_card(state, &departure, P0);
    restamp_planar_objects_to_controller(state);
    (barn, departure_plane)
}

fn barn_board() -> BarnBoard {
    barn_board_inner(false).0
}

/// [`barn_board`] with P0 additionally controlling a printed Spellskite, for the
/// forced-retarget seam. Returns the board plus the Spellskite.
fn barn_board_with_spellskite() -> (BarnBoard, ObjectId) {
    let (board, spellskite) = barn_board_inner(true);
    (board, spellskite.expect("Spellskite requested"))
}

fn barn_board_inner(with_spellskite: bool) -> (BarnBoard, Option<ObjectId>) {
    use engine::types::format::FormatConfig;
    use engine::types::player::PlayerId;

    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_with_format(FormatConfig::planechase(), 3, 11);
    scenario.at_phase(Phase::PreCombatMain);

    let chosen_host = scenario.add_creature(P1, "Chosen Host", 3, 3).id();
    let chosen_aura = scenario
        .add_enchantment_from_oracle(p2, "Pacifism", PACIFISM)
        .with_subtypes(vec!["Aura"])
        .id();
    let incidental_equipment = scenario
        .add_artifact_from_oracle(P1, "Shuko", SHUKO)
        .with_subtypes(vec!["Equipment"])
        .id();
    let spellskite = with_spellskite.then(|| {
        scenario
            .add_creature_from_oracle(P0, "Spellskite", 0, 4, SPELLSKITE)
            .id()
    });

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&chosen_aura).unwrap().attached_to = Some(chosen_host.into());
        state
            .objects
            .get_mut(&chosen_host)
            .unwrap()
            .attachments
            .push(chosen_aura);
        state
            .objects
            .get_mut(&incidental_equipment)
            .unwrap()
            .attached_to = Some(chosen_host.into());
        state
            .objects
            .get_mut(&chosen_host)
            .unwrap()
            .attachments
            .push(incidental_equipment);
        engine::game::layers::mark_layers_full(state);
    }
    let (barn, departure_plane) = install_barn_planes(&mut runner);
    assert!(
        runner.state().planar_deck.contains(&departure_plane),
        "the second plane must be waiting in the planar deck"
    );

    (
        BarnBoard {
            runner,
            barn,
            host: chosen_host,
            aura: chosen_aura,
            equipment: incidental_equipment,
        },
        spellskite,
    )
}

/// CR 901.9b: raise the printed chaos ability and return its announced slots.
fn open_barn_prompt(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
) -> Vec<engine::types::game_state::TargetSelectionSlot> {
    use engine::game::planechase::chaos_ensues;
    use engine::game::triggers::process_triggers;

    chaos_ensues(runner.state_mut(), events);
    process_triggers(runner.state_mut(), events);
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => return target_slots,
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority toward Barn's target prompt")
                        .events,
                );
            }
            other => panic!("unexpected prompt before Barn target selection: {other:?}"),
        }
    }
    panic!("Barn's chaos trigger must raise a target prompt");
}

/// One unattached creature per opponent — the shape a response-window test
/// needs, where blinking or destroying one opponent's pick must not disturb the
/// other's. P1 holds a free Cloudshift and a free Doom Blade.
struct BarnTwoBoard {
    runner: GameRunner,
    host: ObjectId,
    aura: ObjectId,
    cloudshift: ObjectId,
    removal: ObjectId,
}

fn barn_board_two_creatures() -> BarnTwoBoard {
    use engine::types::format::FormatConfig;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;

    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_with_format(FormatConfig::planechase(), 3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First Pick", 2, 2).id();
    let second = scenario.add_creature(p2, "Second Pick", 2, 2).id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P1, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P1, "Doom Blade", true, DOOM_BLADE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    install_barn_planes(&mut runner);
    assert_eq!(
        runner.state().objects[&cloudshift].zone,
        engine::types::zones::Zone::Hand,
        "fixture reach guard: Cloudshift is in P1's hand"
    );
    BarnTwoBoard {
        runner,
        host: first,
        aura: second,
        cloudshift,
        removal,
    }
}

/// CR 400.7 + CR 608.2b: a fanout occurrence must still be the object that was
/// ANNOUNCED there. Responding to the Barn's chaos ability with Cloudshift on
/// one opponent's pick makes a NEW object with the same storage id; that
/// occurrence is illegal and is not affected, while the other opponent's pick
/// still phases out, binds the delayed phase-in, and carries the can't-phase-in
/// rider.
#[test]
fn childhood_barn_blinked_pick_is_dropped_and_the_other_still_phases_out() {
    use engine::game::effects::phase_out::resolve_phase_in;
    use engine::types::player::PlayerId;

    let BarnTwoBoard {
        mut runner,
        host: first,
        aura: second,
        cloudshift,
        ..
    } = barn_board_two_creatures();
    let p2 = PlayerId(2);

    let mut events = Vec::new();
    let target_slots = open_barn_prompt(&mut runner, &mut events);
    assert_eq!(
        target_slots.len(),
        4,
        "one header + object slot per opponent"
    );

    // Announce both picks through the real prompt.
    let incarnation_before = runner.state().objects[&first].incarnation;
    events.extend(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(P1),
                    TargetRef::Object(first),
                    TargetRef::Player(p2),
                    TargetRef::Object(second),
                ],
            })
            .expect("Barn accepts one permanent from each opponent")
            .events,
    );

    // The trigger is on the stack. Pass priority until P1 (the pick's
    // controller, and the only seat that may Cloudshift it) holds it.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority toward P1's response window")
                .events,
        );
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ),
        "reach guard: P1 holds priority for the response, got {:?}",
        runner.state().waiting_for
    );
    runner.cast(cloudshift).target_object(first).commit();
    // Resolve Cloudshift and then the Barn trigger step by step, collecting the
    // events through the production action so the phase-out claims are observed.
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority through Cloudshift and the chaos ability")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "Cloudshift and the chaos ability must both resolve"
    );
    assert_ne!(
        runner.state().objects[&first].incarnation,
        incarnation_before,
        "reach guard: Cloudshift made a new object (CR 400.7)"
    );

    let phased_out = |object_id: ObjectId| {
        events.iter().find_map(|event| match event {
            GameEvent::PermanentPhasedOut {
                object_id: event_id,
                indirect,
            } if *event_id == object_id => Some(*indirect),
            _ => None,
        })
    };
    assert_eq!(
        phased_out(first),
        None,
        "the blinked pick is a new object, so that occurrence is not affected (CR 608.2b)"
    );
    assert_eq!(
        phased_out(second),
        Some(false),
        "the surviving opponent's pick still phases out"
    );

    let delayed_targets: Vec<ObjectId> = runner
        .state()
        .delayed_triggers
        .iter()
        .flat_map(|trigger| trigger.ability.targets.iter())
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    assert!(
        delayed_targets.contains(&second),
        "the surviving pick binds the delayed phase-in, got {delayed_targets:?}"
    );
    // The delayed clause snapshots the ANNOUNCED permanents, so it also carries
    // the blinked id. That is harmless: the blinked object was never phased out,
    // and `phase_in_object` early-returns on an already-phased-in object before
    // any event or state write. The precondition for that no-op is asserted
    // here; the no-op itself is `phasing.rs`'s invariant.
    assert!(
        runner.state().objects[&first].is_phased_in(),
        "the blinked pick was never phased out, so the delayed phase-in cannot affect it"
    );

    // CR 101.2 + CR 702.26b: the can't-phase-in rider holds on the survivor.
    let phase_in = engine::types::ability::ResolvedAbility::new(
        engine::types::ability::Effect::PhaseIn {
            target: engine::types::ability::TargetFilter::SpecificObject { id: second },
        },
        vec![TargetRef::Object(second)],
        second,
        P0,
    );
    let mut phase_in_events = Vec::new();
    resolve_phase_in(runner.state_mut(), &phase_in, &mut phase_in_events)
        .expect("explicit phase-in resolves");
    assert!(
        runner.state().objects[&second].is_phased_out(),
        "the survivor stays phased out while Barn is face up"
    );
}

/// CR 115.6 + CR 115.1 + CR 608.2c: each opponent's "up to one" slot is an
/// INDEPENDENT optional choice. Declining the first opponent's slot through the
/// production walk (`GameAction::ChooseTarget { target: None }`) must leave the
/// later opponent's already-announced permanent selected — the header pairing
/// keys each object to the opponent that declared it, so the declined slot
/// cannot consume the other opponent's pick.
#[test]
fn childhood_barn_declining_one_opponents_slot_keeps_the_other() {
    use engine::types::player::PlayerId;

    let BarnBoard {
        mut runner,
        host: chosen_host,
        aura: chosen_aura,
        equipment: incidental_equipment,
        ..
    } = barn_board();
    let p2 = PlayerId(2);

    let mut events = Vec::new();
    let target_slots = open_barn_prompt(&mut runner, &mut events);
    assert_eq!(
        target_slots.len(),
        4,
        "Barn must declare a header + object slot per opponent, got {target_slots:?}"
    );

    // Decline the FIRST opponent's optional object slot through the walk.
    let outcome = runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("declining an up-to-one slot is legal (CR 115.6)");
    events.extend(outcome.events);
    // Then pick the SECOND opponent's Pacifism.
    let outcome = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen_aura)),
        })
        .expect("the second opponent's Pacifism is a legal pick");
    events.extend(outcome.events);

    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected prompt while Barn's chaos ability resolves: {:?}",
            runner.state().waiting_for
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority through Barn's chaos ability")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "Barn's chaos ability must resolve"
    );

    let phased_out = |object_id: ObjectId| {
        events.iter().find_map(|event| match event {
            GameEvent::PermanentPhasedOut {
                object_id: event_id,
                indirect,
            } if *event_id == object_id => Some(*indirect),
            _ => None,
        })
    };
    assert_eq!(
        phased_out(chosen_aura),
        Some(false),
        "the second opponent's pick is a CHOSEN permanent, so it phases out directly \
         (CR 702.26b) — the declined slot must not swallow it"
    );
    assert_eq!(
        phased_out(chosen_host),
        None,
        "the declined opponent's permanent was never targeted"
    );
    assert_eq!(
        phased_out(incidental_equipment),
        None,
        "Shuko is incidental to a permanent that was never chosen"
    );

    // The delayed phase-in binds exactly the one chosen permanent.
    let delayed_targets: Vec<ObjectId> = runner
        .state()
        .delayed_triggers
        .iter()
        .flat_map(|trigger| trigger.ability.targets.iter())
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    assert_eq!(
        delayed_targets,
        vec![chosen_aura],
        "only the surviving pick is bound, got {delayed_targets:?}"
    );
    assert!(
        runner.state().objects[&chosen_aura].is_phased_out(),
        "the chosen permanent is phased out"
    );
    assert!(
        !runner.state().objects[&chosen_host].is_phased_out(),
        "the declined opponent's host is untouched"
    );
    // Keep the opponent id meaningful even if the fixture changes seats.
    assert_eq!(
        runner.state().objects[&chosen_aura].controller,
        p2,
        "reach guard: the pick belongs to the second opponent"
    );

    // CR 101.2 + CR 702.26b: the can't-phase-in rider is bound in this COMPACTED
    // shape too (two auto-filled headers + one object), which is a different
    // position layout from the two-pick shape.
    let phase_in = engine::types::ability::ResolvedAbility::new(
        engine::types::ability::Effect::PhaseIn {
            target: engine::types::ability::TargetFilter::SpecificObject { id: chosen_aura },
        },
        vec![TargetRef::Object(chosen_aura)],
        chosen_aura,
        P0,
    );
    use engine::game::effects::phase_out::resolve_phase_in;
    let mut phase_in_events = Vec::new();
    resolve_phase_in(runner.state_mut(), &phase_in, &mut phase_in_events)
        .expect("explicit phase-in resolves");
    assert!(
        runner.state().objects[&chosen_aura].is_phased_out(),
        "the surviving pick stays locked while Barn is face up"
    );
}

/// A two-player Barn board with exactly ONE announced pick, plus a free
/// Cloudshift in that pick's controller's hand.
fn barn_board_single_pick() -> (GameRunner, ObjectId, ObjectId) {
    use engine::types::format::FormatConfig;
    use engine::types::mana::ManaCost;

    let mut scenario = GameScenario::new_with_format(FormatConfig::planechase(), 2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let pick = scenario.add_creature(P1, "Only Pick", 2, 2).id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P1, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    install_barn_planes(&mut runner);
    (runner, pick, cloudshift)
}

/// CR 608.2b: when EVERY announced occurrence of the chaos ability is illegal
/// at resolution, no observable part of the ability happens — no phase-out, no
/// can't-phase-in rider, and no delayed phase-in. This is the all-`Dropped`
/// shape of the fanout arm, reached through the production entry.
#[test]
fn childhood_barn_single_blinked_pick_lets_the_whole_ability_do_nothing() {
    let (mut runner, pick, cloudshift) = barn_board_single_pick();

    let mut events = Vec::new();
    let target_slots = open_barn_prompt(&mut runner, &mut events);
    assert_eq!(
        target_slots.len(),
        2,
        "a two-player Barn announces one header + object slot, got {target_slots:?}"
    );

    let incarnation_before = runner.state().objects[&pick].incarnation;
    events.extend(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1), TargetRef::Object(pick)],
            })
            .expect("Barn accepts its one pick")
            .events,
    );

    // Blink the ONLY pick in response, through the real priority window.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority toward P1's response window")
                .events,
        );
    }
    runner.cast(cloudshift).target_object(pick).commit();
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority through Cloudshift and the chaos ability")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "Cloudshift and the chaos ability must both leave the stack"
    );
    assert_ne!(
        runner.state().objects[&pick].incarnation,
        incarnation_before,
        "reach guard: Cloudshift made a new object (CR 400.7)"
    );

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::PermanentPhasedOut { .. })),
        "CR 608.2b: nothing is affected, so nothing phases out"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 608.2b: the delayed phase-in clause never runs, got {:?}",
        runner
            .state()
            .delayed_triggers
            .iter()
            .map(|t| t.ability.targets.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        runner.state().objects[&pick].is_phased_in(),
        "the blinked pick is phased in"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "no can't-phase-in rider is installed, got {:?}",
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .map(|effect| effect.affected.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        !runner.state().players[P1.0 as usize].is_phased_out(),
        "the empty population never falls back to the player anchor"
    );
    // No tappedness assertion here on purpose: this fixture copies only the
    // plane's triggers and statics, not its "Creatures enter tapped"
    // `ReplacementDefinition`, so the blinked object returns untapped whether or
    // not the untap instruction ran. Tappedness therefore cannot witness it.
    // The fizzle's own evidence (no phase-out, no delayed clause) does.
}

/// CR 608.2b + CR 115.1: the EMPTY-POOL response. The first opponent's only
/// announced permanent is destroyed in response, so that header's slot has no
/// legal object at resolution; the later opponent's announced pick must still
/// phase out, still carry the can't-phase-in rider, and still receive the
/// delayed phase-in.
///
/// This is the half the blink cases cannot reach: Cloudshift returns a legal
/// permanent to that opponent, so their pool is never empty.
#[test]
fn childhood_barn_removal_response_keeps_the_other_opponents_pick() {
    use engine::game::effects::phase_out::resolve_phase_in;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    let BarnTwoBoard {
        mut runner,
        host: first,
        aura: second,
        removal,
        ..
    } = barn_board_two_creatures();
    let p2 = PlayerId(2);

    let mut events = Vec::new();
    let target_slots = open_barn_prompt(&mut runner, &mut events);
    assert_eq!(
        target_slots.len(),
        4,
        "one header + object slot per opponent"
    );

    events.extend(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(P1),
                    TargetRef::Object(first),
                    TargetRef::Player(p2),
                    TargetRef::Object(second),
                ],
            })
            .expect("Barn accepts one permanent from each opponent")
            .events,
    );

    // P1 destroys its OWN announced pick in response, emptying its pool.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority toward P1's response window")
                .events,
        );
    }
    runner.cast(removal).target_object(first).commit();
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority through Doom Blade and the chaos ability")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "Doom Blade and the chaos ability must both resolve"
    );
    assert_eq!(
        runner.state().objects[&first].zone,
        Zone::Graveyard,
        "reach guard: the first opponent's pool is empty at resolution"
    );

    let phased_out = |object_id: ObjectId| {
        events.iter().find_map(|event| match event {
            GameEvent::PermanentPhasedOut {
                object_id: event_id,
                indirect,
            } if *event_id == object_id => Some(*indirect),
            _ => None,
        })
    };
    assert_eq!(
        phased_out(first),
        None,
        "the destroyed pick is not on the battlefield to phase out"
    );
    assert_eq!(
        phased_out(second),
        Some(false),
        "the later opponent's pick still phases out"
    );

    let delayed_targets: Vec<ObjectId> = runner
        .state()
        .delayed_triggers
        .iter()
        .flat_map(|trigger| trigger.ability.targets.iter())
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    assert!(
        delayed_targets.contains(&second),
        "the surviving pick receives the delayed phase-in, got {delayed_targets:?}"
    );

    // CR 101.2 + CR 702.26b: the rider holds for the survivor.
    let phase_in = engine::types::ability::ResolvedAbility::new(
        engine::types::ability::Effect::PhaseIn {
            target: engine::types::ability::TargetFilter::SpecificObject { id: second },
        },
        vec![TargetRef::Object(second)],
        second,
        P0,
    );
    let mut phase_in_events = Vec::new();
    resolve_phase_in(runner.state_mut(), &phase_in, &mut phase_in_events)
        .expect("explicit phase-in resolves");
    assert!(
        runner.state().objects[&second].is_phased_out(),
        "the survivor keeps its can't-phase-in restriction while Barn is face up"
    );
}

/// CR 115.7a + CR 115.1: Spellskite forces "a target" of the Barn's chaos
/// ability to itself, which is P0's — so it is not a legal new target for
/// either opponent's slot, and both announced picks stay unchanged. This is the
/// FORCED-retarget seam (`change_targets`'s forced branch recomputes its own
/// slot pools), a different call site from the interactive prompt.
///
/// Revert-failing: with a table-wide slot pool the change is admitted, the
/// header-keyed validator then drops Spellskite, and the host it replaced is no
/// longer affected — so `phased_out(host)` flips from `Some(false)` to `None`.
#[test]
fn childhood_barn_spellskite_keeps_the_announced_picks() {
    use engine::types::game_state::CastPaymentMode;
    use engine::types::player::PlayerId;

    let (
        BarnBoard {
            mut runner,
            host: chosen_host,
            aura: chosen_aura,
            ..
        },
        spellskite,
    ) = barn_board_with_spellskite();
    let p2 = PlayerId(2);
    let life_before = runner.life(P0);

    let mut events = Vec::new();
    let target_slots = open_barn_prompt(&mut runner, &mut events);
    assert_eq!(
        target_slots.len(),
        4,
        "one header + object slot per opponent"
    );

    events.extend(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(P1),
                    TargetRef::Object(chosen_host),
                    TargetRef::Player(p2),
                    TargetRef::Object(chosen_aura),
                ],
            })
            .expect("Barn accepts one permanent from each opponent")
            .events,
    );

    // P0 activates its own Spellskite, paying 2 life for the {U/P} shard.
    runner
        .act(GameAction::ActivateAbility {
            source_id: spellskite,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Spellskite");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::PhyrexianPayment { .. }
    ) {
        runner
            .act(GameAction::SubmitPhyrexianChoices {
                choices: vec![engine::types::game_state::ShardChoice::PayLife],
            })
            .expect("pay 2 life");
    }
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: Spellskite's ability is on the chaos ability"
    );

    for _ in 0..24 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority through Spellskite and the chaos ability")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "Spellskite's ability and the chaos ability must both resolve"
    );
    assert_eq!(
        runner.life(P0),
        life_before - 2,
        "reach guard: the {{U/P}} shard was paid with life"
    );

    let phased_out = |object_id: ObjectId| {
        events.iter().find_map(|event| match event {
            GameEvent::PermanentPhasedOut {
                object_id: event_id,
                indirect,
            } if *event_id == object_id => Some(*indirect),
            _ => None,
        })
    };
    assert_eq!(
        phased_out(chosen_host),
        Some(false),
        "CR 115.7a: the pick could not be changed to a legal target, so the \
         original stays and still phases out"
    );
    assert_eq!(
        phased_out(chosen_aura),
        Some(true),
        "the second opponent's pick is untouched"
    );
    assert!(
        !runner.state().objects[&spellskite].is_phased_out(),
        "Spellskite is not an affected permanent"
    );
}

/// CR 702.26g/h + CR 115.1 + CR 608.2c: the printed Barn chaos ability resolves
/// through the engine's normal entry points — the "for each opponent" target
/// prompt, real target submission, and a real planeswalk. One opponent's choice
/// is a host with printed Shuko attached; the other opponent's choice is a
/// Pacifism attached to that same host. Both chosen permanents bind the
/// can't-phase-in restriction and the delayed phase-in. The Pacifism phases out
/// indirectly with the host (CR 702.26h); the incidental Shuko is never added
/// to the delayed population and stays phased out until its host phases in.
#[test]
fn childhood_barn_binds_selected_objects_not_incidental_attachments() {
    use engine::game::effects::phase_out::resolve_phase_in;
    use engine::game::planechase::PlanarDieFace;
    use engine::types::game_state::{GameRng, RandomDraw};
    use engine::types::player::PlayerId;
    use rand::Rng;

    let BarnBoard {
        mut runner,
        barn,
        host: chosen_host,
        aura: chosen_aura,
        equipment: incidental_equipment,
        ..
    } = barn_board();

    // CR 901.9b: chaos ensues and the printed trigger raises its target prompt.
    let mut events = Vec::new();
    let target_slots = open_barn_prompt(&mut runner, &mut events);
    let p2 = PlayerId(2);

    // CR 601.2c + CR 115.1: "for each opponent, choose up to one target nonland
    // permanent that opponent controls" declares one target per opponent. Each
    // per-opponent group is a non-optional player header (auto-filled at the
    // prompt — the printed text names no player target) followed by that
    // opponent's "up to one" object slot.
    assert_eq!(
        target_slots.len(),
        4,
        "Barn must declare a header + object slot per opponent, got {target_slots:?}"
    );
    assert_eq!(
        target_slots[0].legal_targets,
        vec![TargetRef::Player(P1)],
        "the first group's header binds the first opponent"
    );
    assert_eq!(
        target_slots[2].legal_targets,
        vec![TargetRef::Player(p2)],
        "the second group's header binds the second opponent"
    );
    let objects = |slot: &engine::types::game_state::TargetSelectionSlot| {
        slot.legal_targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect::<Vec<_>>()
    };
    let first_slot = objects(&target_slots[1]);
    let second_slot = objects(&target_slots[3]);
    assert!(
        first_slot.contains(&chosen_host) && first_slot.contains(&incidental_equipment),
        "the first opponent's slot must offer its host and incidental Shuko, got {first_slot:?}"
    );
    assert_eq!(
        second_slot,
        vec![chosen_aura],
        "the second opponent's slot must offer its Pacifism"
    );

    // Submit both choices through the real prompt, in the announced slot shape:
    // the two auto-filled headers plus each opponent's chosen permanent. The
    // prompt already carries the header decisions, so assert them rather than
    // guessing.
    let WaitingFor::TriggerTargetSelection { selection, .. } = runner.state().waiting_for.clone()
    else {
        panic!("expected the Barn target prompt to still be open");
    };
    assert_eq!(
        selection.selected_slots,
        vec![Some(TargetRef::Player(P1))],
        "the first opponent's header is auto-filled on the controller's behalf"
    );
    assert_eq!(
        selection.current_slot, 1,
        "the walk opens on the first opponent's own permanent slot"
    );

    events.extend(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(P1),
                    TargetRef::Object(chosen_host),
                    TargetRef::Player(p2),
                    TargetRef::Object(chosen_aura),
                ],
            })
            .expect("Barn accepts one permanent from each opponent")
            .events,
    );
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected prompt while Barn's chaos ability resolves: {:?}",
            runner.state().waiting_for
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority through Barn's chaos ability")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "Barn's chaos ability must resolve"
    );

    let phased_out = |object_id: ObjectId| {
        events.iter().find_map(|event| match event {
            GameEvent::PermanentPhasedOut {
                object_id: event_id,
                indirect,
            } if *event_id == object_id => Some(*indirect),
            _ => None,
        })
    };
    assert_eq!(
        phased_out(chosen_host),
        Some(false),
        "the chosen host phases out directly"
    );
    assert_eq!(
        phased_out(chosen_aura),
        Some(true),
        "the chosen Pacifism phases out indirectly with the host (CR 702.26h)"
    );
    assert_eq!(
        phased_out(incidental_equipment),
        Some(true),
        "the incidental Shuko phases out indirectly with its host (CR 702.26g)"
    );

    {
        let state = runner.state();
        let delayed_targets: Vec<ObjectId> = state
            .delayed_triggers
            .iter()
            .flat_map(|trigger| trigger.ability.targets.iter())
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect();
        assert!(
            delayed_targets.contains(&chosen_host) && delayed_targets.contains(&chosen_aura),
            "the delayed phase-in must bind both chosen permanents, got {delayed_targets:?}"
        );
        assert!(
            !delayed_targets.contains(&incidental_equipment),
            "the delayed phase-in must not bind the incidental Shuko, got {delayed_targets:?}"
        );
    }

    // CR 702.26b + CR 611.2b: while Barn remains face up, an explicit phase-in of
    // either chosen permanent is a no-op.
    for object_id in [chosen_host, chosen_aura] {
        let phase_in = engine::types::ability::ResolvedAbility::new(
            engine::types::ability::Effect::PhaseIn {
                target: engine::types::ability::TargetFilter::SpecificObject { id: object_id },
            },
            vec![TargetRef::Object(object_id)],
            barn,
            P0,
        );
        let mut phase_in_events = Vec::new();
        resolve_phase_in(runner.state_mut(), &phase_in, &mut phase_in_events)
            .expect("explicit phase-in resolves");
        assert!(
            runner.state().objects[&object_id].is_phased_out(),
            "a chosen permanent must remain locked while Barn is face up"
        );
    }
    assert!(
        runner.state().objects[&incidental_equipment].is_phased_out(),
        "Shuko must stay phased out: it rides its host and is not named by the delayed phase-in (CR 702.26g)"
    );

    // CR 901.9a/c: roll the planar die through the production action until the
    // Planeswalker symbol comes up, then resolve the real planeswalk.
    let seed = (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            rng.draw(RandomDraw::Outcome).random_range(1..=6) == 1
        })
        .expect("a seed that rolls the Planeswalker symbol");
    {
        let state = runner.state_mut();
        state.rng_seed = seed;
        state.rng_word_pos = 0;
        state.rng = GameRng::seed_from_u64(seed);
    }
    events.extend(
        runner
            .act(GameAction::RollPlanarDie)
            .expect("roll the planar die through the production action")
            .events,
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::PlanarDieRolled {
                face: PlanarDieFace::Planeswalk,
                ..
            }
        )),
        "reach: the roll must planeswalk"
    );
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected prompt while the planeswalk resolves: {:?}",
            runner.state().waiting_for
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority through the planeswalk")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "the planeswalk and its delayed instruction must resolve"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })),
        "the production roll must emit a real Planeswalked event"
    );

    let state = runner.state();
    assert!(
        state.objects[&barn].face_down,
        "planeswalking turns the departing plane face down"
    );
    assert!(
        state.objects[&chosen_host].is_phased_in(),
        "the delayed instruction must phase in the chosen host"
    );
    assert!(
        state.objects[&chosen_aura].is_phased_in(),
        "the delayed instruction must phase in the chosen Pacifism"
    );
    assert!(
        state.objects[&incidental_equipment].is_phased_in(),
        "Shuko phases in along with its host, not as a separate delayed target (CR 702.26g)"
    );
}
