//! Issue #6017 regression guard — Bound by Moonsilver's activated cost
//! "Sacrifice another permanent" must stay SOURCE-relative.
//!
//! Oracle:
//!   Enchant creature
//!   Enchanted creature can't attack, block, or transform.
//!   Sacrifice another permanent: Attach this Aura to target creature.
//!   Activate only as a sorcery and only once each turn.
//!
//! The cost filter parses to `Typed(Permanent, [Another])`. `FilterProp::Another`
//! is *source-relative* (CR 613.4c / the reverted global change): the ability's
//! source is the AURA, so "another permanent" means "a permanent other than the
//! Aura." After the Aura is attached, it is NOT a legal sacrifice for its own
//! cost, while every OTHER permanent — including the enchanted host — IS legal.
//!
//! This pins the invariant that the Martial Impetus fix (#6025) must NOT touch:
//! the attachment-trigger "each other creature" retarget is scoped to a
//! trigger's mass-effect population (`valid_card == AttachedTo`), never to this
//! activated-ability sacrifice cost. Driving the REAL activation pipeline, the
//! engine surfaces the eligible sacrifice set as `WaitingFor::PayCost.choices`
//! (game/casting.rs::find_eligible_sacrifice_targets →
//! FilterContext::from_source, casting.rs:14715).
//!
//! CR 118.3: a sacrifice cost is paid by moving a permanent you control to its
//! owner's graveyard (verified: docs/MagicCompRules.txt).
//! CR 613.4c: outside per-recipient layer contexts, "other" is source-relative.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::phase::Phase;

const BOUND_BY_MOONSILVER: &str = "Enchant creature\n\
Enchanted creature can't attack, block, or transform.\n\
Sacrifice another permanent: Attach this Aura to target creature. Activate only as a sorcery and only once each turn.";

#[test]
fn bound_by_moonsilver_sacrifice_another_is_source_relative() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The enchanted host (the creature the Aura is attached to).
    let host = scenario.add_creature(P0, "Enchanted Host", 2, 2).id();

    // A second permanent P0 controls — must be a legal sacrifice.
    let other = scenario.add_creature(P0, "Other Permanent", 3, 3).id();

    // Bound by Moonsilver, carrying the real activated "Sacrifice another
    // permanent" cost.
    let aura = scenario
        .add_creature(P0, "Bound by Moonsilver", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(BOUND_BY_MOONSILVER)
        .id();

    let mut runner = scenario.build();

    // CR 303.4: attach the Aura to the host (same relation the Martial Impetus
    // and Vow-of-Lightning aura regressions set directly).
    {
        let state = runner.state_mut();
        state.objects.get_mut(&aura).unwrap().attached_to = Some(host.into());
        state.objects.get_mut(&host).unwrap().attachments.push(aura);
        state.layers_dirty.mark_full();
    }

    // Activate the Aura's "Sacrifice another permanent: Attach ..." ability.
    // Sacrifice-cost payment is prompted BEFORE target selection, so the
    // eligible sacrifice set is surfaced immediately.
    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index: 0,
        })
        .expect("activating Bound by Moonsilver's sacrifice ability must succeed");

    let choices = match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            choices,
            ..
        } => choices.clone(),
        other => panic!("expected a Sacrifice PayCost prompt, got {other:?}"),
    };

    // CR 613.4c: "another permanent" excludes the SOURCE (the Aura), NOT the
    // enchanted host — the reverse of the Martial Impetus trigger meaning.
    assert!(
        !choices.contains(&aura),
        "the Aura must NOT be a legal sacrifice for its own \
         'Sacrifice another permanent' cost (source-relative Another), got {choices:?}"
    );
    assert!(
        choices.contains(&host),
        "the enchanted host IS a legal sacrifice — 'another permanent' is \
         relative to the Aura source, not the host, got {choices:?}"
    );
    assert!(
        choices.contains(&other),
        "another controlled permanent IS a legal sacrifice, got {choices:?}"
    );
}
