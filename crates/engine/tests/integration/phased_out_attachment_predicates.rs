//! CR 702.26b: a phased-out Aura or Equipment keeps its attachment link but is
//! treated as though it does not exist, so its host is not enchanted, equipped
//! or modified by it — live or in a departure snapshot.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::PhaseOutCause;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const WALL_OF_PUTRID_FLESH: &str = "Defender (This creature can't attack.)\nProtection from white\nPrevent all damage that would be dealt to this creature by enchanted creatures.";
const PRODIGAL_PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";
const FIREBREATHING: &str =
    "Enchant creature\n{R}: Enchanted creature gets +1/+0 until end of turn.";
const BONESPLITTER: &str = "Equipped creature gets +2/+0.\nEquip {1}";
const SYR_GWYN: &str = "Vigilance, menace\nWhenever an equipped creature you control attacks, you draw a card and you lose 1 life.\nEquipment you control have equip Knight {0}.";
const HALVAR: &str = "Creatures you control that are enchanted or equipped have double strike.\nAt the beginning of each combat, you may attach target Aura or Equipment attached to a creature you control to target creature you control.";
const ENVOY_OF_THE_ANCESTORS: &str = "Outlast {W} ({W}, {T}: Put a +1/+1 counter on this creature. Outlast only as a sorcery.)\nModified creatures you control have lifelink. (Equipment, Auras you control, and counters are modifications.)";
const KOLL: &str = "Whenever another nontoken creature you control dies, if it was enchanted or equipped, return it to its owner's hand.\nCreature tokens you control that are enchanted or equipped get +1/+1.";

fn add_firebreathing(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(controller, "Firebreathing", FIREBREATHING)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], FIREBREATHING)
        .id()
}

/// Attaches `attachment` to `host`, then (when `phased`) phases the attachment
/// out directly, asserting the link survives while only the attachment is out.
fn attach_and_maybe_phase_out(
    runner: &mut GameRunner,
    attachment: ObjectId,
    host: ObjectId,
    phased: bool,
) {
    attach_to(runner.state_mut(), attachment, host);
    if phased {
        let mut events = Vec::new();
        let out = phase_out_object(
            runner.state_mut(),
            attachment,
            PhaseOutCause::Directly,
            &mut events,
        );
        assert_eq!(out, vec![attachment]);
    }
    let state = runner.state();
    assert_eq!(state.objects[&attachment].is_phased_out(), phased);
    assert!(!state.objects[&host].is_phased_out());
    assert!(state.objects[&host].attachments.contains(&attachment));
}

fn assert_priority_with_empty_stack(state: &GameState) {
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
}

fn wall_damage_from_enchanted_pyromancer(phased: bool) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pyromancer = scenario
        .add_creature(P0, "Prodigal Pyromancer", 1, 1)
        .from_oracle_text(PRODIGAL_PYROMANCER)
        .id();
    let aura = add_firebreathing(&mut scenario, P0);
    let wall = scenario
        .add_creature(P1, "Wall of Putrid Flesh", 2, 4)
        .from_oracle_text_with_keywords(
            &["Defender", "Protection from white"],
            WALL_OF_PUTRID_FLESH,
        )
        .id();
    let mut runner = scenario.build();
    attach_and_maybe_phase_out(&mut runner, aura, pyromancer, phased);
    let outcome = runner.activate(pyromancer, 0).target_object(wall).resolve();
    assert_priority_with_empty_stack(outcome.state());
    outcome.damage_marked(wall)
}

#[test]
fn enchanted_by_fallback_ignores_phased_out_aura() {
    assert_eq!(wall_damage_from_enchanted_pyromancer(false), 0);
    assert_eq!(wall_damage_from_enchanted_pyromancer(true), 1);
}

/// Returns P0's (life, cards drawn) after attacking with the equipped creature.
fn syr_gwyn_attack(phased: bool) -> (i32, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A", "Filler B"]);
    scenario.with_library_top(P1, &["Filler C"]);
    scenario
        .add_creature(P0, "Syr Gwyn, Hero of Ashvale", 5, 5)
        .from_oracle_text_with_keywords(&["Vigilance", "Menace"], SYR_GWYN);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text_with_keywords(&["Equip"], BONESPLITTER)
        .id();
    let mut runner = scenario.build();
    attach_and_maybe_phase_out(&mut runner, equipment, bears, phased);
    let hand_before = runner.state().players[0].hand.len();
    runner.pass_both_players();
    runner
        .declare_attackers(&[(bears, AttackTarget::Player(P1))])
        .expect("Grizzly Bears can attack");
    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.pass_both_players();
    }
    assert_priority_with_empty_stack(runner.state());
    let p0 = &runner.state().players[0];
    (p0.life, p0.hand.len() - hand_before)
}

#[test]
fn equipped_by_fallback_ignores_phased_out_equipment() {
    assert_eq!(syr_gwyn_attack(false), (19, 1));
    assert_eq!(syr_gwyn_attack(true), (20, 0));
}

/// Whether an enchanted Grizzly Bears has `keyword` from the lord's static,
/// read after a priority pass re-derives layers.
fn enchanted_bears_has_keyword(
    lord: &str,
    lord_oracle: &str,
    keyword: Keyword,
    phased: bool,
) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, lord, 3, 3)
        .from_oracle_text(lord_oracle);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = add_firebreathing(&mut scenario, P0);
    let mut runner = scenario.build();
    attach_and_maybe_phase_out(&mut runner, aura, bears, phased);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 can pass priority");
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    runner.state().objects[&bears].has_keyword(&keyword)
}

#[test]
fn has_any_attachment_of_ignores_phased_out_aura() {
    let halvar = |phased| {
        enchanted_bears_has_keyword(
            "Halvar, God of Battle",
            HALVAR,
            Keyword::DoubleStrike,
            phased,
        )
    };
    assert!(halvar(false));
    assert!(!halvar(true));
}

#[test]
fn modified_ignores_phased_out_aura() {
    let envoy = |phased| {
        enchanted_bears_has_keyword(
            "Envoy of the Ancestors",
            ENVOY_OF_THE_ANCESTORS,
            Keyword::Lifelink,
            phased,
        )
    };
    assert!(envoy(false));
    assert!(!envoy(true));
}

fn bolted_enchanted_bears_zone_under_koll(phased: bool) -> Zone {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Koll, the Forgemaster", 2, 2)
        .from_oracle_text(KOLL);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = add_firebreathing(&mut scenario, P0);
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    attach_and_maybe_phase_out(&mut runner, aura, bears, phased);
    let outcome = runner.cast(bolt).target_object(bears).resolve();
    assert_priority_with_empty_stack(outcome.state());
    outcome.zone_of(bears)
}

#[test]
fn departure_snapshot_omits_phased_out_aura() {
    assert_eq!(bolted_enchanted_bears_zone_under_koll(false), Zone::Hand);
    assert_eq!(
        bolted_enchanted_bears_zone_under_koll(true),
        Zone::Graveyard
    );
}
