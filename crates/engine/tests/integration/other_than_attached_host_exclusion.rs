//! "<typed filter> other than enchanted creature" — the attached-host exclusion
//! suffix (Sporogenic Infection, Due Diligence, Secret Invasion, Kjeldoran Pride).
//!
//! CR 303.4b: the object an Aura is attached to is "enchanted"; the Aura
//! "enchants" that object. "Other than enchanted creature" therefore excludes
//! exactly the object THIS Aura is attached to. CR 301.5a is the Equipment twin
//! ("equipped creature").
//!
//! THE REPORTED DEFECT. The "other than" suffix of the type-phrase fold only
//! recognized a self-reference ("other than ~"). "enchanted creature" failed
//! that combinator, nothing was consumed, and the exclusion was silently
//! dropped: Sporogenic Infection's edict could make its victim sacrifice the
//! very creature the Aura enchants. The suffix now also accepts the attached
//! host and produces `FilterProp::Not { EnchantedBy }` (or `EquippedBy`), which
//! `game/filter.rs` evaluates against the ability source's `attached_to`.
//!
//! Every test drives the real cast pipeline and reads the engine's own prompt
//! fields (`EffectZoneChoice.cards`, `TriggerTargetSelection` /
//! `TargetSelection` `legal_targets`) or zones and P/T — never a parsed filter
//! shape (the `oracle_target.rs` unit tests pin the shape). Each negative is
//! paired with a positive in the same test.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SPOROGENIC_NAME: &str = "Sporogenic Infection";
const SPOROGENIC_ORACLE: &str = "Enchant creature\nWhen this Aura enters, target player sacrifices a creature of their choice other than enchanted creature.\nWhen enchanted creature is dealt damage, destroy it.";

const DUE_DILIGENCE_NAME: &str = "Due Diligence";
const DUE_DILIGENCE_ORACLE: &str = "Enchant creature\nWhen this Aura enters, target creature you control other than enchanted creature gets +2/+2 and gains vigilance until end of turn.\nEnchanted creature gets +2/+2 and has vigilance.";

const SECRET_INVASION_NAME: &str = "Secret Invasion";
const SECRET_INVASION_ORACLE: &str = "Enchant creature you control\nWhen this Aura enters, exile up to one target creature other than enchanted creature until this Aura leaves the battlefield. Enchanted creature becomes a copy of that creature until this Aura leaves the battlefield.\nEnchanted creature has ward {2}.";

const KJELDORAN_PRIDE_NAME: &str = "Kjeldoran Pride";
const KJELDORAN_PRIDE_ORACLE: &str = "Enchant creature\nEnchanted creature gets +1/+2.\n{2}{U}: Attach this Aura to target creature other than enchanted creature.";

/// A free "Enchant creature" Aura in `player`'s hand, built from verbatim Oracle.
fn aura_in_hand(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    oracle: &str,
) -> ObjectId {
    let mut builder = scenario.add_creature_to_hand(player, name, 0, 0);
    builder
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Enchant creature"], oracle);
    builder.id()
}

/// CR 701.21a + CR 608.2d + CR 303.4b: the edict's victim chooses among their
/// creatures OTHER THAN the enchanted one. When the host is their only creature
/// the pool is empty, so nothing is sacrificed and the Aura stays attached.
#[test]
fn sporogenic_infection_edict_spares_host_when_it_is_the_only_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Host", 2, 2).id();
    let aura = aura_in_hand(&mut scenario, P0, SPOROGENIC_NAME, SPOROGENIC_ORACLE);
    let mut runner = scenario.build();

    let out = runner
        .cast(aura)
        .target_object(host)
        .target_player(P1)
        .resolve();
    assert_eq!(
        out.zone_of(host),
        Zone::Battlefield,
        "the enchanted creature must not be sacrificed to its own Aura's edict"
    );
    assert!(
        matches!(out.final_waiting_for(), WaitingFor::Priority { .. }),
        "an empty sacrifice pool offers no choice; got {:?}",
        out.final_waiting_for()
    );

    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "the Aura stays on its surviving host"
    );
}

/// CR 701.21a + CR 608.2d + CR 303.4b: with other creatures available, the
/// victim's choice lists every creature except the enchanted host, and the
/// chosen one is sacrificed.
#[test]
fn sporogenic_infection_edict_offers_every_creature_but_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Host", 2, 2).id();
    let x = scenario.add_creature(P1, "X", 2, 2).id();
    let y = scenario.add_creature(P1, "Y", 2, 2).id();
    let aura = aura_in_hand(&mut scenario, P0, SPOROGENIC_NAME, SPOROGENIC_ORACLE);
    let mut runner = scenario.build();

    let out = runner
        .cast(aura)
        .target_object(host)
        .target_player(P1)
        .resolve();
    match out.final_waiting_for().clone() {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(player, P1, "the targeted player chooses the sacrifice");
            assert_eq!(count, 1);
            assert!(cards.contains(&x), "X must be offered: {cards:?}");
            assert!(cards.contains(&y), "Y must be offered: {cards:?}");
            assert!(
                !cards.contains(&host),
                "the enchanted creature must not be offered: {cards:?}"
            );
        }
        other => panic!("expected the sacrifice choice, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards { cards: vec![x] })
        .expect("sacrificing X is a legal choice");
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&x].zone, Zone::Graveyard, "X was sacrificed");
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
    assert_eq!(state.objects[&y].zone, Zone::Battlefield);
}

/// CR 608.2h + CR 113.7a: the trigger exists independently of its source, and
/// the "enchanted creature" it names is read from the source's last known
/// attachment. The Aura leaving while its trigger waits on the stack must not
/// widen the edict back onto the former host.
#[test]
fn sporogenic_infection_host_stays_excluded_after_aura_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Host", 2, 2).id();
    let x = scenario.add_creature(P1, "X", 2, 2).id();
    let aura = aura_in_hand(&mut scenario, P0, SPOROGENIC_NAME, SPOROGENIC_ORACLE);
    let mut runner = scenario.build();

    {
        let _committed = runner.cast(aura).target_object(host).commit();
    }
    runner.resolve_top();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the enters trigger asks for its target player; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("P1 is a legal target player");

    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(aura, Zone::Graveyard, aura),
            &mut Vec::new(),
        ),
        "the Aura's removal must complete without a replacement choice"
    );
    assert_eq!(runner.state().objects[&aura].zone, Zone::Graveyard);

    runner.advance_until_stack_empty();
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "with the host excluded only X is eligible, so no choice is needed; got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.objects[&x].zone,
        Zone::Graveyard,
        "the edict still resolves with its source gone"
    );
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
}

/// CR 115.1 + CR 601.2c via CR 603.3d: the enters trigger needs a target
/// creature you control other than the enchanted one. With only the host, it
/// has no legal target and is removed, so the host gets only the static
/// +2/+2 (4/4), not the trigger's as well (6/6).
#[test]
fn due_diligence_trigger_without_other_creature_is_removed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let aura = aura_in_hand(&mut scenario, P0, DUE_DILIGENCE_NAME, DUE_DILIGENCE_ORACLE);
    let mut runner = scenario.build();

    let _ = runner.cast(aura).target_object(host).resolve();
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&host].power, Some(4));
    assert_eq!(state.objects[&host].toughness, Some(4));
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
}

/// CR 115.1 + CR 601.2c via CR 603.3d: the trigger's target prompt lists every
/// creature you control except the enchanted host, and the chosen creature
/// gets the bonus.
#[test]
fn due_diligence_trigger_targets_every_creature_you_control_but_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let c = scenario.add_creature(P0, "C", 2, 2).id();
    let aura = aura_in_hand(&mut scenario, P0, DUE_DILIGENCE_NAME, DUE_DILIGENCE_ORACLE);
    let mut runner = scenario.build();

    {
        let _committed = runner.cast(aura).target_object(host).commit();
    }
    runner.resolve_top();
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert_eq!(target_slots.len(), 1);
            let legal = &target_slots[0].legal_targets;
            assert!(legal.contains(&TargetRef::Object(b)), "B: {legal:?}");
            assert!(legal.contains(&TargetRef::Object(c)), "C: {legal:?}");
            assert!(
                !legal.contains(&TargetRef::Object(host)),
                "the enchanted creature must not be a legal target: {legal:?}"
            );
        }
        other => panic!("expected the trigger target prompt, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("B is a legal target");
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(
        (state.objects[&host].power, state.objects[&host].toughness),
        (Some(4), Some(4))
    );
    assert_eq!(
        (state.objects[&b].power, state.objects[&b].toughness),
        (Some(4), Some(4))
    );
    assert_eq!(
        (state.objects[&c].power, state.objects[&c].toughness),
        (Some(2), Some(2))
    );
}

/// CR 602.2b + CR 601.2c + CR 701.3a: the activated Attach needs a target
/// creature other than the one the Aura currently enchants. The prompt lists
/// every other creature, and choosing one moves the Aura there.
#[test]
fn kjeldoran_pride_reattach_targets_every_creature_but_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let c = scenario.add_creature(P0, "C", 2, 2).id();
    let aura = aura_in_hand(
        &mut scenario,
        P0,
        KJELDORAN_PRIDE_NAME,
        KJELDORAN_PRIDE_ORACLE,
    );
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    let _ = runner.cast(aura).target_object(a).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(a))
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index: 0,
        })
        .expect("the attach ability can be activated");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            let legal = &target_slots[0].legal_targets;
            assert!(legal.contains(&TargetRef::Object(b)), "B: {legal:?}");
            assert!(legal.contains(&TargetRef::Object(c)), "C: {legal:?}");
            assert!(
                !legal.contains(&TargetRef::Object(a)),
                "the enchanted creature must not be a legal target: {legal:?}"
            );
        }
        other => panic!("expected the activation target prompt, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("B is a legal target");
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(
        state.objects[&aura].attached_to,
        Some(AttachTarget::Object(b)),
        "the Aura moved to B"
    );
    assert!(
        state.players[0].mana_pool.mana.is_empty(),
        "the {{2}}{{U}} cost was paid"
    );
}

/// CR 115.1d + CR 601.2c via CR 603.3d + CR 303.4b: Secret Invasion's enters
/// trigger has an optional ("up to one") target slot for a creature other than
/// the one the Aura enchants. The slot offers every other creature but not the
/// host, and announcing zero targets is legal: nothing is exiled and the Aura
/// stays on its host.
#[test]
fn secret_invasion_up_to_one_target_excludes_host_and_allows_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let x = scenario.add_creature(P1, "X", 3, 3).id();
    let mut builder = scenario.add_creature_to_hand(P0, SECRET_INVASION_NAME, 0, 0);
    builder
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Enchant creature you control"], SECRET_INVASION_ORACLE);
    let aura = builder.id();
    let mut runner = scenario.build();

    {
        let _committed = runner.cast(aura).target_object(host).commit();
    }
    runner.resolve_top();
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert_eq!(target_slots.len(), 1);
            assert_eq!(
                target_slots[0].legal_targets,
                vec![TargetRef::Object(x)],
                "X is offered and the enchanted host is not"
            );
            assert!(
                target_slots[0].optional,
                "\"up to one\" allows zero targets"
            );
        }
        other => panic!("expected the trigger target prompt, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("choosing zero targets is legal for \"up to one\"");
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert_eq!(
        state.objects[&x].zone,
        Zone::Battlefield,
        "nothing was exiled"
    );
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "the Aura stays on its host"
    );
}
