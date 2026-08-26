//! Runtime regression for Descendants' Fury (#4795), driven end-to-end through
//! the real `apply` pipeline (combat → optional-sacrifice trigger → RevealUntil).
//!
//! Descendants' Fury (Scryfall-verified Oracle): "Whenever one or more creatures
//! you control deal combat damage to a player, you may sacrifice one of them. If
//! you do, reveal cards from the top of your library until you reveal a creature
//! card that shares a creature type with the sacrificed creature. Put that card
//! onto the battlefield and the rest on the bottom of your library in a random
//! order."
//!
//! Two class-level defects were fixed:
//!
//!   1. The trigger's `Effect::Sacrifice { target: TrackedSet(0) }` derives its
//!      eligible pool ("one of them") from the `CombatDamageDealtToPlayer` event's
//!      source set. The sacrifice resolver matched the raw `TrackedSet(0)`
//!      sentinel directly against `matches_target_filter`, whose id-level ladder
//!      never reaches the combat-damage rung — so the pool was empty and the
//!      accepted sacrifice was a silent no-op. Fixed by routing the target filter
//!      through the single-authority `resolve_tracked_set_sentinel` (CR 510.2 +
//!      CR 608.2c), the same binder every other tracked-set consumer uses.
//!
//!   2. "the sacrificed creature" parses to `SharesQuality { reference:
//!      CostPaidObject }`. The sacrifice is an EFFECT (not a cost), so the chain
//!      resolver captures the sacrificed creature into `effect_context_object`,
//!      never `cost_paid_object`. The `TargetFilter::CostPaidObject` runtime arms
//!      (`filter.rs` + `targeting.rs`) read only `cost_paid_object`, so the
//!      reference resolved to nothing and the reveal dug past the shared-type
//!      card. Fixed by implementing the documented `cost_paid_object →
//!      effect_context_object` fallback ladder (CR 608.2k) that
//!      `ObjectScope::CostPaidObject`'s P/T and mana-value arms already use.
//!
//! REVERT-PROOF: reverting either fix flips a positive assertion below —
//!   * revert #1: the attacker stays on the battlefield (never sacrificed) and
//!     the Goblin never enters — `sacrificed to Graveyard` / `Goblin on
//!     Battlefield` both fail;
//!   * revert #2: the attacker IS sacrificed but the reveal digs the whole
//!     library and bottoms it — `Goblin on Battlefield` fails while the reach
//!     guard (attacker sacrificed) still holds.

use super::rules::{run_combat, GameScenario, Phase, P0, P1};
use engine::game::game_object::GameObject;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const DESCENDANTS_FURY: &str = "Whenever one or more creatures you control deal combat damage to a player, you may sacrifice one of them. If you do, reveal cards from the top of your library until you reveal a creature card that shares a creature type with the sacrificed creature. Put that card onto the battlefield and the rest on the bottom of your library in a random order.";

/// Push both `card_types` and `base_card_types` so the subtype survives a layer
/// recompute (which reverts `card_types` from `base_card_types`).
fn make_creature(obj: &mut GameObject, subtype: &str) {
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push(subtype.into());
    obj.base_card_types.core_types.push(CoreType::Creature);
    obj.base_card_types.subtypes.push(subtype.into());
}

fn make_noncreature(obj: &mut GameObject, core: CoreType) {
    obj.card_types.core_types.push(core);
    obj.base_card_types.core_types.push(core);
}

/// Accept the "you may sacrifice one of them" prompt and drive the resolution to
/// a settled priority, submitting any interactive sacrifice/reveal choice via the
/// supplied `sacrifice_pick` selector (chooses which eligible creature to sac when
/// an `EffectZoneChoice` surfaces — single-attacker cases fast-path with none).
fn resolve_trigger(
    runner: &mut super::rules::GameRunner,
    mut sacrifice_pick: impl FnMut(&[ObjectId]) -> Vec<ObjectId>,
) {
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional sacrifice");
            }
            WaitingFor::EffectZoneChoice { cards, count, .. } => {
                let pick = sacrifice_pick(&cards);
                assert!(!pick.is_empty() && pick.len() <= count.max(1));
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("submit sacrifice selection");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected WaitingFor while resolving Descendants' Fury: {other:?}"),
        }
    }
}

/// Primary end-to-end: one Goblin attacker deals combat damage, is sacrificed,
/// and the reveal digs past a non-creature card to put the library Goblin (shares
/// a creature type with the sacrificed Goblin) onto the battlefield.
#[test]
fn descendants_fury_reveals_and_battlefields_shared_type_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Descendants' Fury", 0, 3, DESCENDANTS_FURY);
    let attacker = scenario.add_creature(P0, "Goblin Attacker", 2, 2).id();

    // Library cards (order fixed below): miss (non-creature), goblin_match
    // (shares type), non_match (creature, different type).
    let miss = scenario.add_card_to_library_top(P0, "Non Creature Miss");
    let goblin_match = scenario.add_card_to_library_top(P0, "Library Goblin");
    let non_match = scenario.add_card_to_library_top(P0, "Library Bear");

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Bear".into()];

    // Library top → bottom: miss, goblin_match, non_match.
    {
        let lib = &mut runner.state_mut().players[0].library;
        lib.retain(|&id| id != miss && id != goblin_match && id != non_match);
        lib.insert(0, non_match);
        lib.insert(0, goblin_match);
        lib.insert(0, miss);
    }
    {
        let s = runner.state_mut();
        make_creature(s.objects.get_mut(&attacker).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&goblin_match).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&non_match).unwrap(), "Bear");
        make_noncreature(s.objects.get_mut(&miss).unwrap(), CoreType::Sorcery);
    }

    let p1_before = runner.life(P1);
    run_combat(&mut runner, vec![attacker], vec![]);
    resolve_trigger(&mut runner, |_| unreachable!("single attacker fast-paths"));

    let s = runner.state();
    // Reach guard: combat actually happened.
    assert_eq!(
        runner.life(P1),
        p1_before - 2,
        "attacker must deal 2 combat damage to P1"
    );
    // Reach guard (fix #1): the attacker was sacrificed — proves the reveal was
    // gated on a real sacrifice, not a no-op accept.
    assert_eq!(
        s.objects[&attacker].zone,
        Zone::Graveyard,
        "the accepted 'sacrifice one of them' must actually sacrifice the attacker (fix #1)"
    );
    // Primary assertion (fix #2): the shared-type Goblin entered the battlefield,
    // and the reveal DUG PAST the non-creature miss to find it.
    assert_eq!(
        s.objects[&goblin_match].zone,
        Zone::Battlefield,
        "the revealed creature sharing a type with the sacrificed Goblin must enter the battlefield (fix #2)"
    );
    // The reveal stopped at the FIRST match — the later Bear must NOT enter and
    // the non-creature miss returns to the (bottom of the) library.
    assert_eq!(
        s.objects[&non_match].zone,
        Zone::Library,
        "the reveal stops at the first shared-type match; the later Bear stays in the library"
    );
    assert_eq!(
        s.objects[&miss].zone,
        Zone::Library,
        "the revealed non-creature miss goes to the bottom of the library"
    );
    // The rest pile (miss) is on the bottom, below the never-revealed remainder
    // (non_match). Both are in the library; miss is at or below non_match.
    let lib: Vec<ObjectId> = s.players[0].library.iter().copied().collect();
    assert!(
        lib.contains(&miss) && lib.contains(&non_match),
        "rest cards remain in the library"
    );
}

/// Negative: no creature in the library shares a type with the sacrificed
/// creature. Per CR 701.20a the reveal exhausts the library, nothing enters, and
/// every revealed card returns to the bottom. Discriminates fix #2: if the
/// `CostPaidObject` reference wrongly matched (or matched nothing and any
/// creature satisfied a vacuous filter) a card could still enter.
#[test]
fn descendants_fury_no_shared_type_reveals_whole_library_nothing_enters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Descendants' Fury", 0, 3, DESCENDANTS_FURY);
    let attacker = scenario.add_creature(P0, "Goblin Attacker", 2, 2).id();

    // Library: a Bear creature (different type) and a non-creature — no shared
    // type with the sacrificed Goblin.
    let bear = scenario.add_card_to_library_top(P0, "Library Bear");
    let sorcery = scenario.add_card_to_library_top(P0, "Library Sorcery");

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Bear".into()];
    {
        let lib = &mut runner.state_mut().players[0].library;
        lib.retain(|&id| id != bear && id != sorcery);
        lib.insert(0, bear);
        lib.insert(0, sorcery);
    }
    {
        let s = runner.state_mut();
        make_creature(s.objects.get_mut(&attacker).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&bear).unwrap(), "Bear");
        make_noncreature(s.objects.get_mut(&sorcery).unwrap(), CoreType::Sorcery);
    }

    run_combat(&mut runner, vec![attacker], vec![]);
    resolve_trigger(&mut runner, |_| unreachable!("single attacker fast-paths"));

    let s = runner.state();
    // Reach guard: the sacrifice still happens.
    assert_eq!(
        s.objects[&attacker].zone,
        Zone::Graveyard,
        "the attacker is sacrificed even when no library creature shares its type"
    );
    // Nothing enters the battlefield; both revealed cards stay in the library.
    assert_eq!(
        s.objects[&bear].zone,
        Zone::Library,
        "no shared type → Bear does not enter"
    );
    assert_eq!(
        s.objects[&sorcery].zone,
        Zone::Library,
        "non-creature never enters"
    );
    assert!(
        !s.objects.values().any(|o| o.zone == Zone::Battlefield
            && o.owner == P0
            && (o.id == bear || o.id == sorcery)),
        "no revealed card was put onto the battlefield"
    );
}

/// Multi-eligible interactive path: two attackers of DIFFERENT creature types
/// deal combat damage. The "sacrifice one of them" surfaces an `EffectZoneChoice`
/// over both; the CHOSEN creature's type drives the reveal. Choosing the Goblin
/// must battlefield the library Goblin (not the Elf), proving the reveal reads the
/// SELECTED sacrifice's type via `effect_context_object`.
#[test]
fn descendants_fury_multi_attacker_chosen_type_drives_reveal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Descendants' Fury", 0, 3, DESCENDANTS_FURY);
    let goblin_attacker = scenario.add_creature(P0, "Goblin Attacker", 2, 2).id();
    let elf_attacker = scenario.add_creature(P0, "Elf Attacker", 2, 2).id();

    // Library top: an Elf (would match if the Elf were sacrificed), then a Goblin
    // (matches the chosen Goblin). Choosing the Goblin must skip the top Elf and
    // battlefield the Goblin.
    let lib_goblin = scenario.add_card_to_library_top(P0, "Library Goblin");
    let lib_elf = scenario.add_card_to_library_top(P0, "Library Elf");

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Elf".into()];
    {
        let lib = &mut runner.state_mut().players[0].library;
        lib.retain(|&id| id != lib_goblin && id != lib_elf);
        lib.insert(0, lib_goblin);
        lib.insert(0, lib_elf);
    }
    {
        let s = runner.state_mut();
        make_creature(s.objects.get_mut(&goblin_attacker).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&elf_attacker).unwrap(), "Elf");
        make_creature(s.objects.get_mut(&lib_goblin).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&lib_elf).unwrap(), "Elf");
    }

    run_combat(&mut runner, vec![goblin_attacker, elf_attacker], vec![]);

    // The interactive path must surface an EffectZoneChoice over BOTH attackers;
    // choose the Goblin.
    let mut saw_choice = false;
    resolve_trigger(&mut runner, |eligible| {
        saw_choice = true;
        assert!(
            eligible.contains(&goblin_attacker) && eligible.contains(&elf_attacker),
            "both combat-damaging attackers are eligible to sacrifice; got {eligible:?}"
        );
        vec![goblin_attacker]
    });
    assert!(
        saw_choice,
        "two eligible attackers must surface an interactive sacrifice choice"
    );

    let s = runner.state();
    assert_eq!(
        s.objects[&goblin_attacker].zone,
        Zone::Graveyard,
        "the chosen Goblin is sacrificed"
    );
    assert_eq!(
        s.objects[&elf_attacker].zone,
        Zone::Battlefield,
        "the unchosen Elf attacker is not sacrificed"
    );
    // The CHOSEN Goblin's type drives the reveal: the library Goblin enters, the
    // top Library Elf is skipped (dug past → returns to the library).
    assert_eq!(
        s.objects[&lib_goblin].zone,
        Zone::Battlefield,
        "the reveal reads the CHOSEN sacrifice's (Goblin) type and battlefields the library Goblin"
    );
    assert_eq!(
        s.objects[&lib_elf].zone,
        Zone::Library,
        "the top Library Elf does not share the chosen Goblin's type — dug past, returned to library"
    );
}
