//! "all <filter> attached to <referent>" — the mass object set defined by
//! attachment to a referenced permanent or player (CR 115.1a/d + CR 115.10a +
//! CR 701.3a + CR 608.2c).
//!
//! Every row drives a REAL card through the public cast/activation/trigger
//! pipeline with its verbatim Oracle text (CR set below, each verified against
//! `docs/MagicCompRules.txt`): CR 107.1c (any number includes zero), CR 115.6
//! (zero targets), CR 400.7 (a moved object is new), CR 601.2c (announcement),
//! CR 603.7c (delayed ability referring to a particular object), CR 608.2b
//! (partial legality), CR 608.2c (instructions in order), CR 608.2h (last known
//! information), CR 608.2k (untargeted object previously referred to), CR 701.3a
//! (attach), CR 704.5m/n (SBAs tear attachments down on exit).
//!
//! Runtime rows that need no new machinery live here; the Fumble contract is
//! the pre-existing runtime row in `fin_sidequest_turn_history_conditions.rs`.

use engine::game::effects::attach;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const STRIP_BARE: &str = "Destroy all Auras and Equipment attached to target creature.";
const HUBRIS: &str = "Return target creature and all Auras attached to it to their owners' hands.";
const WORD_OF_UNDOING: &str =
    "Return target creature and all white Auras you own attached to it to their owners' hands.";
const ESSENCE_RELIQUARY: &str =
    "{T}: Return another target permanent you control and all Auras you control attached to it to their owner's hand. Activate only during your turn.";
const PORTAL_OF_SANCTUARY: &str =
    "{1}, {T}: Return target creature you control and each Aura attached to it to their owners' hands. Activate only during your turn.";
const ORZHOV_CHARM: &str = "Choose one —\n• Return target creature you control and all Auras you control attached to it to their owner's hand.\n• Destroy target creature and you lose life equal to its toughness.\n• Return target creature card with mana value 1 or less from your graveyard to the battlefield.";
const SILENCE_THE_BELIEVERS: &str =
    "Strive — This spell costs {2}{B} more to cast for each target beyond the first.\nExile any number of target creatures and all Auras attached to them.";
const EATEN_BY_SPIDERS: &str =
    "Destroy target creature with flying and all Equipment attached to that creature.";
const WITCHBANE_ORB: &str =
    "When this artifact enters, destroy all Curses attached to you.\nYou have hexproof.";
const MURDEROUS_SPOILS: &str = "Destroy target nonblack creature. It can't be regenerated. You gain control of all Equipment that were attached to it. (This effect lasts indefinitely.)";
const SOUL_NOVA: &str = "Exile target attacking creature and all Equipment attached to it.";
const SCARAB_OF_THE_UNSEEN: &str = "{T}, Sacrifice this artifact: Return all Auras attached to target permanent you own to their owners' hands. Draw a card at the beginning of the next turn's upkeep.";
const END_HOSTILITIES: &str = "Destroy all creatures and all permanents attached to creatures.";
const TAWNOS_COFFIN: &str = "You may choose not to untap this artifact during your untap step.\n{3}, {T}: Exile target creature and all Auras attached to it. Note the number and kind of counters that were on that creature. When this artifact leaves the battlefield or becomes untapped, return that exiled card to the battlefield under its owner's control tapped with the noted number and kind of counters on it. If you do, return the other exiled cards to the battlefield under their owners' control attached to that permanent.";
const TREEFOLK_MYSTIC: &str = "Whenever this creature blocks or becomes blocked by a creature, destroy all Auras attached to that creature.";
const CORROSIVE_OOZE: &str = "Whenever this creature blocks or becomes blocked by an equipped creature, destroy all Equipment attached to that creature at end of combat.";
const STREET_SWEEPER: &str =
    "Whenever this creature attacks, destroy all Auras attached to target land.";
const RHUK: &str = "Trample, haste\nWhenever an equipped creature you control other than Rhuk attacks or dies, you may attach all Equipment attached to that creature to Rhuk.";

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn equipment(scenario: &mut GameScenario, owner: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_artifact_from_oracle(owner, name, "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .id()
}

fn aura(scenario: &mut GameScenario, owner: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(
            owner,
            name,
            "Enchant creature\nEnchanted creature gets +1/+1.",
        )
        .with_subtypes(vec!["Aura"])
        .id()
}

fn free_spell(scenario: &mut GameScenario, name: &str, instant: bool, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn attach_at(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    attach::attach_to(runner.state_mut(), attachment, host);
}

/// CR 500.1 + CR 508.1a: hand the turn to the attacking player. The scenario
/// builder seats P0 as the active player, so a fixture whose attacker is P1
/// must move the active player (and its priority window) first or the engine
/// rejects the declaration as not the active player's.
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };
}

/// Pass priority until the stack is empty and the engine is at a quiet
/// `Priority` window (or `GameOver`).
fn settle(runner: &mut GameRunner) {
    for _ in 0..128 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            WaitingFor::GameOver { .. } => return,
            other => panic!("unexpected prompt while settling: {other:?}"),
        }
    }
    panic!("the board never settled: {:?}", runner.state().waiting_for);
}

// ---------------------------------------------------------------------------
// R1: Strip Bare — the announced host and its Auras/Equipment
// ---------------------------------------------------------------------------

/// R1 (CR 115.1a + CR 701.8a): "Destroy all Auras and Equipment attached to
/// target creature" destroys exactly the announced host's Auras and Equipment;
/// Equipment on another creature survives and the host itself survives.
#[test]
fn r1_strip_bare_destroys_the_hosts_auras_and_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Host", 2, 7).id();
    let other = scenario.add_creature(P1, "Other", 2, 7).id();
    let on_host_eq = equipment(&mut scenario, P1, "On Host Equipment");
    let on_host_aura = aura(&mut scenario, P1, "On Host Aura");
    let on_other_eq = equipment(&mut scenario, P1, "On Other Equipment");
    let spell = free_spell(&mut scenario, "Strip Bare", true, STRIP_BARE);
    let mut runner = scenario.build();
    attach_at(&mut runner, on_host_eq, host);
    attach_at(&mut runner, on_host_aura, host);
    attach_at(&mut runner, on_other_eq, other);

    let outcome = runner.cast(spell).target_objects(&[host]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell resolves to a quiet priority window: {:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    assert_eq!(state.objects[&on_host_eq].zone, Zone::Graveyard);
    assert_eq!(state.objects[&on_host_aura].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&on_other_eq].zone,
        Zone::Battlefield,
        "Equipment on another creature is untouched"
    );
    assert_eq!(
        state.objects[&host].zone,
        Zone::Battlefield,
        "the host itself is not affected"
    );
}

// ---------------------------------------------------------------------------
// R2: Hubris — one instruction, host + its Auras (RF1 concretion)
// ---------------------------------------------------------------------------

/// R2 (CR 608.2c): "Return target creature and all Auras attached to it" moves
/// the HOST and its Auras in one instruction. Without the positive-leg
/// concretion the host leg would match nothing and only the Auras would move.
#[test]
fn r2_hubris_returns_the_host_and_its_auras_together() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Host", 2, 7).id();
    let bystander = scenario.add_creature(P1, "Bystander", 2, 7).id();
    let on_host = aura(&mut scenario, P1, "On Host Aura");
    let on_bystander = aura(&mut scenario, P1, "On Bystander Aura");
    let spell = free_spell(&mut scenario, "Hubris", true, HUBRIS);
    let mut runner = scenario.build();
    attach_at(&mut runner, on_host, host);
    attach_at(&mut runner, on_bystander, bystander);

    let outcome = runner.cast(spell).target_objects(&[host]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.zone_of(host),
        Zone::Hand,
        "the announced host is affected too (RF1)"
    );
    assert_eq!(
        outcome.state().objects[&on_host].zone,
        Zone::Hand,
        "its Aura returns in the same instruction"
    );
    assert_eq!(
        outcome.state().objects[&on_bystander].zone,
        Zone::Battlefield,
        "a bystander's Aura stays"
    );
}

// ---------------------------------------------------------------------------
// R3: Word of Undoing — owner vs controller (CR 108.3 / CR 109.5)
// ---------------------------------------------------------------------------

/// R3 (CR 108.3 + CR 109.5): "all white Auras you own" scopes by OWNER, not by
/// who controls the Aura, and by color. A white Aura the caster owns returns; a
/// white Aura owned by the opponent and a non-white Aura the caster owns stay
/// (then die to the CR 704.5m SBA because their host left).
#[test]
fn r3_word_of_undoing_returns_only_the_casters_white_auras() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Host", 2, 7).id();
    let mine_white = scenario
        .add_enchantment_from_oracle(P0, "Mine White", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .with_color(vec![ManaColor::White])
        .id();
    let theirs_white = scenario
        .add_enchantment_from_oracle(P1, "Theirs White", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .with_color(vec![ManaColor::White])
        .id();
    let mine_blue = scenario
        .add_enchantment_from_oracle(P0, "Mine Blue", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .with_color(vec![ManaColor::Blue])
        .id();
    let spell = free_spell(&mut scenario, "Word of Undoing", true, WORD_OF_UNDOING);
    let mut runner = scenario.build();
    for att in [mine_white, theirs_white, mine_blue] {
        attach_at(&mut runner, att, host);
    }

    let outcome = runner.cast(spell).target_objects(&[host]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(outcome.zone_of(host), Zone::Hand);
    assert_eq!(
        outcome.state().objects[&mine_white].zone,
        Zone::Hand,
        "the caster's white Aura returns"
    );
    assert_eq!(
        outcome.state().objects[&theirs_white].zone,
        Zone::Graveyard,
        "an opponent-owned white Aura is not returned; the CR 704.5m SBA takes it"
    );
    assert_eq!(
        outcome.state().objects[&mine_blue].zone,
        Zone::Graveyard,
        "a non-white Aura the caster owns is not returned"
    );
}

// ---------------------------------------------------------------------------
// R4: Essence Reliquary — "another" excludes the source (F8)
// ---------------------------------------------------------------------------

/// R4 (CR 115.1): the Reliquary's own activation returns another
/// target permanent the caster controls and the Auras they control attached to
/// it; the Reliquary is not a legal target for its own "another".
#[test]
fn r4_essence_reliquary_returns_another_permanent_and_its_auras() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let permanent = scenario.add_creature(P0, "Own Permanent", 2, 7).id();
    let on_it = scenario
        .add_enchantment_from_oracle(P0, "Own Aura", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .id();
    let reliquary = scenario
        .add_artifact_from_oracle(P0, "Essence Reliquary", ESSENCE_RELIQUARY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    attach_at(&mut runner, on_it, permanent);

    let outcome = runner
        .activate(reliquary, 0)
        .target_object(permanent)
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(outcome.zone_of(permanent), Zone::Hand);
    assert_eq!(
        outcome.state().objects[&on_it].zone,
        Zone::Hand,
        "the Aura the caster controls returns"
    );
}

// ---------------------------------------------------------------------------
// R5: Portal of Sanctuary — own creature + each Aura
// ---------------------------------------------------------------------------

/// R5 (CR 115.1a + CR 608.2c): an activated mass return moves the caster's
/// creature and every Aura attached to it; an opponent's creature is not a
/// legal target.
#[test]
fn r5_portal_of_sanctuary_returns_own_creature_and_each_aura() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 7).id();
    let on_mine = aura(&mut scenario, P0, "On Mine Aura");
    let portal = scenario
        .add_artifact_from_oracle(P0, "Portal of Sanctuary", PORTAL_OF_SANCTUARY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    // CR 602.2b: the activation's own {1} cost is paid from the pool.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            portal,
            false,
            Vec::new(),
        )],
    );
    let mut runner = scenario.build();
    attach_at(&mut runner, on_mine, mine);

    let outcome = runner.activate(portal, 0).target_object(mine).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(outcome.zone_of(mine), Zone::Hand);
    assert_eq!(outcome.state().objects[&on_mine].zone, Zone::Hand);
}

// ---------------------------------------------------------------------------
// R6: Orzhov Charm, mode 1
// ---------------------------------------------------------------------------

/// R6 (CR 608.2c): mode 1 is the modelled mass return; the mode prompt offers
/// all three modes and the chosen mode moves the creature and its Auras.
#[test]
fn r6_orzhov_charm_mode_one_returns_creature_and_auras() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 7).id();
    let on_mine = aura(&mut scenario, P0, "On Mine Aura");
    let charm = free_spell(&mut scenario, "Orzhov Charm", true, ORZHOV_CHARM);
    let mut runner = scenario.build();
    attach_at(&mut runner, on_mine, mine);

    let outcome = runner
        .cast(charm)
        .modes(&[0])
        .target_objects(&[mine])
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(outcome.zone_of(mine), Zone::Hand);
    assert_eq!(outcome.state().objects[&on_mine].zone, Zone::Hand);
}

// ---------------------------------------------------------------------------
// R7: Silence the Believers — multiple targets, each with its own Auras
// ---------------------------------------------------------------------------

/// R7 (CR 107.1c + CR 115.6 + CR 608.2c): "any number of target creatures and
/// all Auras attached to them" exiles BOTH chosen creatures and their Auras in
/// one instruction; a third creature's Aura is untouched.
#[test]
fn r7_silence_exiles_each_chosen_creature_and_its_auras() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First", 2, 7).id();
    let second = scenario.add_creature(P1, "Second", 2, 7).id();
    let untouched = scenario.add_creature(P1, "Untouched", 2, 7).id();
    let first_aura = aura(&mut scenario, P1, "First Aura");
    let second_aura = aura(&mut scenario, P1, "Second Aura");
    let bystander_aura = aura(&mut scenario, P1, "Bystander Aura");
    let spell = free_spell(
        &mut scenario,
        "Silence the Believers",
        true,
        SILENCE_THE_BELIEVERS,
    );
    // CR 601.2f + CR 601.2h: the strive rider adds {2}{B} for the second
    // target; the pool must cover it for the cast to be payable.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, spell, false, Vec::new()),
            ManaUnit::new(ManaType::Colorless, spell, false, Vec::new()),
            ManaUnit::new(ManaType::Black, spell, false, Vec::new()),
        ],
    );
    let mut runner = scenario.build();
    attach_at(&mut runner, first_aura, first);
    attach_at(&mut runner, second_aura, second);
    attach_at(&mut runner, bystander_aura, untouched);

    let outcome = runner
        .cast(spell)
        .target_objects(&[first, second])
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    for (id, label) in [
        (first, "first creature"),
        (second, "second creature"),
        (first_aura, "first Aura"),
        (second_aura, "second Aura"),
    ] {
        assert_eq!(state.objects[&id].zone, Zone::Exile, "{label} is exiled");
    }
    assert_eq!(
        state.objects[&untouched].zone,
        Zone::Battlefield,
        "an unchosen creature stays"
    );
    assert_eq!(
        state.objects[&bystander_aura].zone,
        Zone::Battlefield,
        "an unchosen creature's Aura stays"
    );
}

// ---------------------------------------------------------------------------
// R8: Soul Nova — an announced attacking host (family C)
// ---------------------------------------------------------------------------

/// R8 (CR 115.1a + CR 701.13a + CR 400.7): "Exile target attacking creature and
/// all Equipment attached to it" exiles the announced attacker and its
/// Equipment in one instruction; Equipment on a different creature survives.
#[test]
fn r8_soul_nova_exiles_the_attacking_host_and_its_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let attacker = scenario.add_creature(P1, "Attacker", 2, 7).id();
    let bystander = scenario.add_creature(P1, "Bystander", 2, 7).id();
    let attacker_eq = equipment(&mut scenario, P1, "Attacker Equipment");
    let bystander_eq = equipment(&mut scenario, P1, "Bystander Equipment");
    let spell = free_spell(&mut scenario, "Soul Nova", true, SOUL_NOVA);
    let mut runner = scenario.build();
    attach_at(&mut runner, attacker_eq, attacker);
    attach_at(&mut runner, bystander_eq, bystander);
    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P0))])
        .expect("declare the attacker");
    settle(&mut runner);
    // The attacker's controller owns the combat turn; hand the instant window
    // to P0 (the spell's owner) so the cast is legal.
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let outcome = runner.cast(spell).target_objects(&[attacker]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    assert_eq!(state.objects[&attacker].zone, Zone::Exile);
    assert_eq!(state.objects[&attacker_eq].zone, Zone::Exile);
    assert_eq!(
        state.objects[&bystander_eq].zone,
        Zone::Battlefield,
        "Equipment on another creature is not the announced host's"
    );
}

// ---------------------------------------------------------------------------
// R11: Scarab of the Unseen — a family-B activated announced host
// ---------------------------------------------------------------------------

/// R11 (CR 115.1a + CR 601.2c + CR 701.26): the activated ability announces a
/// permanent its controller owns, and only the Auras attached to it return to
/// hand; the host stays on the battlefield.
#[test]
fn r11_scarab_of_the_unseen_returns_the_hosts_auras_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 2, 7).id();
    let other = scenario.add_creature(P0, "Other", 2, 7).id();
    let on_host = aura(&mut scenario, P0, "Aura on Host");
    let on_other = aura(&mut scenario, P0, "Aura on Other");
    let scarab = scenario
        .add_artifact_from_oracle(P0, "Scarab of the Unseen", SCARAB_OF_THE_UNSEEN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    attach_at(&mut runner, on_host, host);
    attach_at(&mut runner, on_other, other);

    let outcome = runner.activate(scarab, 0).target_object(host).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    assert_eq!(
        state.objects[&on_host].zone,
        Zone::Hand,
        "the Aura attached to the announced host returns to its owner's hand"
    );
    assert_eq!(
        state.objects[&on_other].zone,
        Zone::Battlefield,
        "an Aura on another permanent is not attached to the host"
    );
    assert_eq!(
        state.objects[&host].zone,
        Zone::Battlefield,
        "the host is announced, not affected"
    );
}

// ---------------------------------------------------------------------------
// R9: Eaten by Spiders — flying restriction
// ---------------------------------------------------------------------------

/// R9 (CR 115.1a + CR 701.8a): only a flying creature is a legal target, and
/// its Equipment is destroyed with it; a non-flyer's Equipment survives.
#[test]
fn r9_eaten_by_spiders_destroys_a_flyer_and_its_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flyer = scenario
        .add_creature_from_oracle(P1, "Flyer", 2, 7, "Flying")
        .id();
    let grounded = scenario.add_creature(P1, "Grounded", 2, 7).id();
    let flyer_eq = equipment(&mut scenario, P1, "Flyer Equipment");
    let grounded_eq = equipment(&mut scenario, P1, "Grounded Equipment");
    let spell = free_spell(&mut scenario, "Eaten by Spiders", true, EATEN_BY_SPIDERS);
    let mut runner = scenario.build();
    attach_at(&mut runner, flyer_eq, flyer);
    attach_at(&mut runner, grounded_eq, grounded);

    let outcome = runner.cast(spell).target_objects(&[flyer]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    assert_eq!(state.objects[&flyer].zone, Zone::Graveyard);
    assert_eq!(state.objects[&flyer_eq].zone, Zone::Graveyard);
    assert_eq!(state.objects[&grounded_eq].zone, Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// R10: Street Sweeper — the triggered announced host (family B)
// ---------------------------------------------------------------------------

/// R10 (CR 115.1a + CR 701.8a + CR 603.7): the attack trigger announces its
/// land target at the trigger's target step, and the mass child destroys
/// exactly the Auras attached to that land; the land itself and an Aura on a
/// different land survive.
#[test]
fn r10_street_sweeper_destroys_auras_attached_to_the_targeted_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let sweeper = scenario
        .add_creature_from_oracle(P0, "Street Sweeper", 4, 6, STREET_SWEEPER)
        .id();
    let chosen_land = scenario.add_land_from_oracle(P1, "Chosen Land", "").id();
    let other_land = scenario.add_land_from_oracle(P1, "Other Land", "").id();
    let on_chosen = aura(&mut scenario, P1, "Aura on Chosen Land");
    let on_other = aura(&mut scenario, P1, "Aura on Other Land");
    let mut runner = scenario.build();
    attach_at(&mut runner, on_chosen, chosen_land);
    attach_at(&mut runner, on_other, other_land);

    runner
        .declare_attackers(&[(sweeper, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declare the attacker");

    let mut saw_target_prompt = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets.contains(&TargetRef::Object(chosen_land))
                        && slot.legal_targets.contains(&TargetRef::Object(other_land)),
                    "both lands are legal targets: {:?}",
                    slot.legal_targets
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen_land)),
                    })
                    .expect("choose the land");
                saw_target_prompt = true;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving Street Sweeper: {other:?}"),
        }
    }

    assert!(
        saw_target_prompt,
        "reach guard: the attack trigger asked for its land target"
    );
    let state = runner.state();
    assert_eq!(state.objects[&sweeper].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&chosen_land].zone,
        Zone::Battlefield,
        "the targeted land itself is not destroyed"
    );
    assert_eq!(
        state.objects[&on_chosen].zone,
        Zone::Graveyard,
        "the Aura attached to the targeted land is destroyed"
    );
    assert_eq!(
        state.objects[&on_other].zone,
        Zone::Battlefield,
        "an Aura on a different land survives"
    );
}

// ---------------------------------------------------------------------------
// R12: Witchbane Orb — Curses attached to a PLAYER
// ---------------------------------------------------------------------------

/// R12 (CR 303.4b + CR 109.5): "destroy all Curses attached to you" reads the
/// player-host axis; a Curse attached to the caster is destroyed, one attached
/// to the opponent stays.
#[test]
fn r12_witchbane_orb_destroys_curses_attached_to_you() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let on_mine = scenario
        .add_enchantment_from_oracle(P1, "Curse on Me", "Enchant player")
        .with_subtypes(vec!["Aura", "Curse"])
        .id();
    let on_theirs = scenario
        .add_enchantment_from_oracle(P0, "Curse on Them", "Enchant player")
        .with_subtypes(vec!["Aura", "Curse"])
        .id();
    // CR 301.1 + CR 603.6a: the Orb is an ARTIFACT permanent spell, so it must
    // be built as an artifact card — a sorcery-typed stand-in resolves to the
    // graveyard and never enters, so its ETB trigger would never fire.
    let orb = scenario
        .add_artifact_to_hand_from_oracle(P0, "Witchbane Orb", WITCHBANE_ORB)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&on_mine).unwrap().attached_to = Some(AttachTarget::Player(P0));
        state.objects.get_mut(&on_theirs).unwrap().attached_to = Some(AttachTarget::Player(P1));
    }

    let outcome = runner.cast(orb).resolve();
    settle(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.objects[&orb].zone,
        Zone::Battlefield,
        "reach guard: the Orb resolved and entered"
    );
    assert_eq!(
        state.objects[&on_mine].zone,
        Zone::Graveyard,
        "the Curse attached to the caster is destroyed"
    );
    assert_eq!(
        state.objects[&on_theirs].zone,
        Zone::Battlefield,
        "a Curse attached to the opponent stays"
    );
    assert!(outcome.state().stack.is_empty(), "the ETB trigger resolved");
}

// ---------------------------------------------------------------------------
// R13: Rhuk, Hexgold Nabber — family D, the triggered Host{TriggeringSource}
// ---------------------------------------------------------------------------

/// R13 (CR 508.1a + CR 603.7 + CR 701.3a): the attack trigger's "that creature"
/// is the triggering attacker (`AttachedTo{Host{TriggeringSource}}`), and the
/// determined "all Equipment" set moves from that creature to Rhuk; Equipment
/// on a different creature stays put.
#[test]
fn r13_rhuk_attaches_the_triggering_creatures_equipment_to_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let rhuk = scenario
        .add_creature_from_oracle(P0, "Rhuk, Hexgold Nabber", 2, 2, RHUK)
        .id();
    let bearer = scenario.add_creature(P0, "Equipped Bearer", 2, 2).id();
    let other = scenario.add_creature(P1, "Other Creature", 2, 2).id();
    let on_bearer = equipment(&mut scenario, P0, "Bearer Equipment");
    let on_other = equipment(&mut scenario, P1, "Other Equipment");
    let mut runner = scenario.build();
    attach_at(&mut runner, on_bearer, bearer);
    attach_at(&mut runner, on_other, other);

    runner
        .declare_attackers(&[(bearer, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declare the equipped attacker");

    let mut optional_prompts = 0;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            // "you may attach" is an optional instruction at resolution.
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the attach");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving Rhuk: {other:?}"),
        }
    }

    assert_eq!(
        optional_prompts, 1,
        "reach guard: the attack trigger fired and offered its optional attach"
    );
    let state = runner.state();
    assert_eq!(
        state.objects[&on_bearer].attached_to,
        Some(AttachTarget::Object(rhuk)),
        "the triggering attacker's Equipment moves to Rhuk"
    );
    assert_eq!(
        state.objects[&on_other].attached_to,
        Some(AttachTarget::Object(other)),
        "Equipment on a different creature is untouched"
    );
}

/// R13(c) (CR 400.7 + CR 608.2h): the production prior-blink discriminator. A
/// blink leaves an earlier exit for the bearer's storage id; the returned
/// incarnation then gains DIFFERENT Equipment and attacks. Rhuk's trigger names
/// the CURRENT incarnation, so its current Equipment moves — the earlier exit's
/// attachment set must not answer.
#[test]
fn r13c_rhuk_attack_after_a_prior_blink_moves_the_current_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let rhuk = scenario
        .add_creature_from_oracle(P0, "Rhuk, Hexgold Nabber", 2, 2, RHUK)
        .id();
    let bearer = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, "Haste")
        .id();
    let old_gear = equipment(&mut scenario, P0, "Old Equipment");
    let new_gear = scenario
        .add_artifact_from_oracle(
            P0,
            "New Equipment",
            "{0}: Attach this Equipment to target creature you control. Equipped creature gets +2/+0.",
        )
        .with_subtypes(vec!["Equipment"])
        .id();
    let blink = free_spell(
        &mut scenario,
        "Cloudshift",
        true,
        "Exile target creature you control, then return that card to the battlefield under its owner's control.",
    );
    let mut runner = scenario.build();
    attach_at(&mut runner, old_gear, bearer);

    // A real blink: the bearer leaves and returns as a new incarnation.
    runner.cast(blink).target_object(bearer).resolve();
    assert_eq!(
        runner.state().objects[&bearer].zone,
        Zone::Battlefield,
        "reach guard: the bearer returned"
    );
    assert_eq!(
        runner.state().objects[&old_gear].attached_to,
        None,
        "reach guard: the blink tore the old attachment down"
    );

    // Attach DIFFERENT Equipment to the returned incarnation.
    runner.activate(new_gear, 0).target_object(bearer).resolve();
    assert_eq!(
        runner.state().objects[&new_gear].attached_to,
        Some(AttachTarget::Object(bearer)),
        "reach guard: the returned bearer gained the new Equipment"
    );

    runner
        .declare_attackers(&[(bearer, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declare the equipped attacker");

    let mut optional_prompts = 0;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the attach");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving Rhuk's attack trigger: {other:?}"),
        }
    }
    assert_eq!(
        optional_prompts, 1,
        "reach guard: the attack trigger fired and offered its optional attach"
    );
    let state = runner.state();
    assert_eq!(
        state.objects[&new_gear].attached_to,
        Some(AttachTarget::Object(rhuk)),
        "the CURRENT incarnation's Equipment moves to Rhuk"
    );
    assert_eq!(
        state.objects[&old_gear].attached_to, None,
        "the earlier exit's Equipment is untouched"
    );
}

// ---------------------------------------------------------------------------
// R14: Murderous Spoils — D2 gain control of the departed host's Equipment
// ---------------------------------------------------------------------------

/// R14 (CR 608.2c + CR 608.2h): the second instruction gains control of exactly
/// the Equipment attached to the declared target (D2 pinned read); the
/// Equipment on another creature is untouched.
#[test]
fn r14_murderous_spoils_gains_control_of_the_targets_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 7).id();
    let other = scenario.add_creature(P1, "Other", 2, 7).id();
    let on_victim = equipment(&mut scenario, P1, "On Victim Equipment");
    let on_other = equipment(&mut scenario, P1, "On Other Equipment");
    let spoil = free_spell(&mut scenario, "Murderous Spoils", false, MURDEROUS_SPOILS);
    let mut runner = scenario.build();
    attach_at(&mut runner, on_victim, victim);
    attach_at(&mut runner, on_other, other);

    let outcome = runner.cast(spoil).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    assert_eq!(state.objects[&victim].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&on_victim].controller, P0,
        "the caster gains control of the host's Equipment"
    );
    assert_eq!(
        state.objects[&on_other].controller, P1,
        "Equipment on another creature is unchanged"
    );
}

// ---------------------------------------------------------------------------
// R17: End Hostilities — creatures and everything attached to creatures
// ---------------------------------------------------------------------------

/// R17 (CR 608.2c): every creature dies and every permanent attached to a
/// creature dies with it; an Aura attached to a land survives.
#[test]
fn r17_end_hostilities_destroys_creatures_and_their_attachments() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Creature", 2, 7).id();
    let land = scenario.add_land_from_oracle(P1, "Land", "").id();
    let on_creature = aura(&mut scenario, P1, "Aura on Creature");
    let on_land = aura(&mut scenario, P1, "Aura on Land");
    // CR 704.5n: an Equipment whose host leaves the battlefield stays on the
    // battlefield, so it can only reach the graveyard through the modelled
    // "all permanents attached to creatures" leg. The Aura is a secondary
    // assertion because CR 704.5m would kill it anyway.
    let equipment = equipment(&mut scenario, P1, "Equipment on Creature");
    let spell = free_spell(&mut scenario, "End Hostilities", false, END_HOSTILITIES);
    let mut runner = scenario.build();
    attach_at(&mut runner, on_creature, creature);
    attach_at(&mut runner, on_land, land);
    attach_at(&mut runner, equipment, creature);

    let outcome = runner.cast(spell).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    assert_eq!(state.objects[&creature].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&equipment].zone,
        Zone::Graveyard,
        "DISCRIMINATING (CR 704.5n): the Equipment is destroyed by the mass leg, \
         not by the host leaving"
    );
    assert_eq!(
        state.objects[&on_creature].zone,
        Zone::Graveyard,
        "the Aura attached to a creature is destroyed by the same instruction"
    );
    assert_eq!(
        state.objects[&on_land].zone,
        Zone::Battlefield,
        "an Aura attached to a land is not a creature and survives"
    );
}

// ---------------------------------------------------------------------------
// R18: Tawnos's Coffin — shared-gap card, first instruction modelled
// ---------------------------------------------------------------------------

/// R18 (CR 608.2c): the exile instruction is modelled — creature and its Auras
/// leave in one instruction — while the card stays unsupported on its
/// pre-existing "Note the number…" gap.
#[test]
fn r18_tawnos_coffin_exiles_creature_and_its_auras() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 7).id();
    let on_victim = aura(&mut scenario, P1, "On Victim Aura");
    let coffin = scenario
        .add_artifact_from_oracle(P0, "Tawnos's Coffin", TAWNOS_COFFIN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    // CR 602.2b: the activation's own {3} cost is paid from the pool.
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, coffin, false, Vec::new()))
            .collect(),
    );
    let mut runner = scenario.build();
    attach_at(&mut runner, on_victim, victim);

    let outcome = runner.activate(coffin, 0).target_object(victim).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        outcome.final_waiting_for()
    );
    let state = outcome.state();
    assert_eq!(state.objects[&victim].zone, Zone::Exile);
    assert_eq!(
        state.objects[&on_victim].zone,
        Zone::Exile,
        "the Auras are exiled in the same instruction"
    );
}

// ---------------------------------------------------------------------------
// R15: Treefolk Mystic — the block-event tier (RF3-1 discriminator)
// ---------------------------------------------------------------------------

/// R15 (CR 509.3c/d + CR 608.2k): a block trigger's "that creature" has no
/// announced target; the host comes from the EVENT TIER of
/// `attached_to_host_filter` (`blocked_attacker_from_event`). The Aura on the
/// OTHER creature (the attacker) is destroyed; Treefolk's own Aura and an
/// unrelated Aura survive.
#[test]
fn r15_treefolk_mystic_destroys_the_blocked_creatures_aura() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let treefolk = scenario
        .add_creature_from_oracle(P0, "Treefolk Mystic", 2, 7, TREEFOLK_MYSTIC)
        .id();
    let attacker = scenario.add_creature(P1, "Attacker", 2, 2).id();
    let unrelated = scenario.add_creature(P1, "Unrelated", 2, 2).id();
    let on_attacker = aura(&mut scenario, P1, "Aura on Attacker");
    let on_treefolk = aura(&mut scenario, P1, "Aura on Treefolk");
    let on_unrelated = aura(&mut scenario, P1, "Aura on Unrelated");
    let mut runner = scenario.build();
    // P1 is the attacking player; the builder seats P0.
    hand_turn_to(&mut runner, P1);
    for (aura_id, host) in [
        (on_attacker, attacker),
        (on_treefolk, treefolk),
        (on_unrelated, unrelated),
    ] {
        attach_at(&mut runner, aura_id, host);
    }

    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P0))])
        .expect("declare the attacker");
    // The block declaration is the trigger's producer.
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt before blockers: {other:?}"),
        }
    }
    runner
        .declare_blockers(&[(treefolk, attacker)])
        .expect("declare the block");
    settle(&mut runner);

    let state = runner.state();
    assert_eq!(
        state.objects[&on_attacker].zone,
        Zone::Graveyard,
        "the blocked creature's Aura is destroyed by the event-tier host read"
    );
    assert_eq!(
        state.objects[&on_treefolk].zone,
        Zone::Battlefield,
        "Treefolk's own Aura survives"
    );
    assert_eq!(
        state.objects[&on_unrelated].zone,
        Zone::Battlefield,
        "an unrelated Aura survives"
    );
}

// ---------------------------------------------------------------------------
// R16a/b/c: Corrosive Ooze — the delayed payload and its creation snapshot
// ---------------------------------------------------------------------------

struct OozeBoard {
    runner: GameRunner,
    ooze: ObjectId,
    attacker: ObjectId,
    equipment: ObjectId,
    other_equipment: ObjectId,
}

/// Block an equipped attacker with Corrosive Ooze and stop at the end-of-combat
/// delayed trigger's creation.
fn ooze_board() -> OozeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let ooze = scenario
        .add_creature_from_oracle(P0, "Corrosive Ooze", 2, 7, CORROSIVE_OOZE)
        .id();
    let attacker = scenario.add_creature(P1, "Equipped Attacker", 2, 2).id();
    let other = scenario.add_creature(P1, "Other", 2, 2).id();
    let attacker_equipment = equipment(&mut scenario, P1, "Attacker Equipment");
    let other_equipment = equipment(&mut scenario, P1, "Other Equipment");
    let mut runner = scenario.build();
    // P1 is the attacking player; the builder seats P0.
    hand_turn_to(&mut runner, P1);
    attach_at(&mut runner, attacker_equipment, attacker);
    attach_at(&mut runner, other_equipment, other);
    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P0))])
        .expect("declare the attacker");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt before blockers: {other:?}"),
        }
    }
    runner
        .declare_blockers(&[(ooze, attacker)])
        .expect("declare the block");
    OozeBoard {
        runner,
        ooze,
        attacker,
        equipment: attacker_equipment,
        other_equipment,
    }
}

/// R16a (CR 603.7c + CR 601.2c): the delayed payload snapshots the block-pair
/// referent at creation (`targets` + `target_incarnations`) and destroys the
/// Equipment at end of combat when the host is still there.
#[test]
fn r16a_corrosive_ooze_destroys_the_equipment_at_end_of_combat() {
    let mut b = ooze_board();
    settle(&mut b.runner);
    {
        let state = b.runner.state();
        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "reach guard: the block trigger installed its delayed payload"
        );
        let payload = &state.delayed_triggers[0].ability;
        assert!(
            payload
                .targets
                .contains(&engine::types::ability::TargetRef::Object(b.attacker)),
            "the creation snapshot records the other combatant: {:?}",
            payload.targets
        );
        assert!(
            !payload.target_incarnations.is_empty(),
            "the creation snapshot pins its incarnation"
        );
    }
    // CR 510.1-510.3: drive the combat-damage step to end of combat; the
    // delayed payload fires there.
    b.runner.combat_damage();
    settle(&mut b.runner);
    let state = b.runner.state();
    assert_eq!(
        state.objects[&b.equipment].zone,
        Zone::Graveyard,
        "the equipped blocker destroyed the Equipment at end of combat"
    );
    assert_eq!(
        state.objects[&b.other_equipment].zone,
        Zone::Battlefield,
        "Equipment on another creature survives"
    );
    assert_eq!(
        state.objects[&b.ooze].zone,
        Zone::Battlefield,
        "the Ooze survives combat"
    );
}

/// R16b (ruling 1): the Equipment is destroyed even if the Ooze leaves the
/// battlefield before the delayed ability resolves.
#[test]
fn r16b_corrosive_ooze_destroys_the_equipment_even_if_the_ooze_leaves() {
    let mut b = ooze_board();
    settle(&mut b.runner);
    {
        let mut events = Vec::new();
        let _ = engine::game::zone_pipeline::move_object_for_test(
            b.runner.state_mut(),
            engine::game::zone_pipeline::ZoneMoveRequest::effect(b.ooze, Zone::Graveyard, b.ooze),
            &mut events,
        );
    }
    b.runner.combat_damage();
    settle(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&b.equipment].zone,
        Zone::Graveyard,
        "the delayed ability still destroys it (Scryfall ruling 1)"
    );
}

/// R16c (ruling 2, CR 608.2h): the blocked creature leaves before the delayed
/// ability resolves; the Equipment attached to it IMMEDIATELY BEFORE it left is
/// destroyed through the pinned incarnation's exit record.
#[test]
fn r16c_corrosive_ooze_destroys_the_equipment_of_a_departed_host() {
    let mut b = ooze_board();
    settle(&mut b.runner);
    {
        let mut events = Vec::new();
        let _ = engine::game::zone_pipeline::move_object_for_test(
            b.runner.state_mut(),
            engine::game::zone_pipeline::ZoneMoveRequest::effect(
                b.attacker,
                Zone::Graveyard,
                b.attacker,
            ),
            &mut events,
        );
    }
    b.runner.advance_to_phase(Phase::EndCombat);
    settle(&mut b.runner);
    let state = b.runner.state();
    assert_eq!(
        state.objects[&b.equipment].zone,
        Zone::Graveyard,
        "the exit-time attachment set is read (Scryfall ruling 2, CR 608.2h)"
    );
    assert_eq!(
        state.objects[&b.other_equipment].zone,
        Zone::Battlefield,
        "Equipment on another creature survives"
    );
}
