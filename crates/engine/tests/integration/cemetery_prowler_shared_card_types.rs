//! Cemetery Prowler's cost reduction counts card TYPES shared with its own
//! linked-exile population, through the production Oracle-to-cast pipeline.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Vigilance\nWhenever this creature enters or attacks, exile a card from a graveyard.\nSpells you cast cost {1} less to cast for each card type they share with cards exiled with this creature.";

#[derive(Clone, Copy)]
enum ExileCase {
    LinkedToProwler,
    Empty,
    Unlinked,
    LinkedToOtherSource,
}

/// CR 205.2a + CR 607.2a + CR 601.2f (#6898): the full Oracle card must reduce
/// a creature spell only for a type shared with cards exiled with THAT Prowler.
///
/// The first row is the positive reach guard for every negative row below: if
/// parsing, static registration, cost collection, or spell binding is absent,
/// it spends all three mana and fails this one table-driven test.
#[test]
fn cemetery_prowler_counts_only_shared_types_from_its_own_linked_exile() {
    let cases = [
        ("linked creature", ExileCase::LinkedToProwler, true, 1),
        ("empty linked population", ExileCase::Empty, true, 0),
        ("unlinked creature in exile", ExileCase::Unlinked, true, 0),
        (
            "creature linked to another source",
            ExileCase::LinkedToOtherSource,
            true,
            0,
        ),
        (
            "linked creature and sorcery spell",
            ExileCase::LinkedToProwler,
            false,
            0,
        ),
    ];

    for (label, exile_case, creature_spell, expected_mana) in cases {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let prowler = {
            let mut builder = scenario.add_creature(P0, "Cemetery Prowler", 3, 4);
            builder.from_oracle_text_with_keywords(&["Vigilance"], ORACLE);
            builder.id()
        };
        let other_source = scenario.add_creature(P0, "Other Exiler", 1, 1).id();
        let exiled = scenario
            .add_creature_to_exile(P0, "Exiled Creature", 1, 1)
            .id();
        let spell = if creature_spell {
            scenario
                .add_creature_to_hand(P0, "Creature Spell", 1, 1)
                .with_mana_cost(ManaCost::generic(3))
                .id()
        } else {
            scenario
                .add_spell_to_hand(P0, "Sorcery Spell", false)
                .with_mana_cost(ManaCost::generic(3))
                .id()
        };
        scenario.with_mana_pool(
            P0,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect(),
        );

        let mut runner = scenario.build();
        match exile_case {
            ExileCase::LinkedToProwler => runner.state_mut().exile_links.push(ExileLink {
                source_id: prowler,
                exiled_id: exiled,
                kind: ExileLinkKind::TrackedBySource,
            }),
            ExileCase::Empty => {}
            ExileCase::Unlinked => {}
            ExileCase::LinkedToOtherSource => runner.state_mut().exile_links.push(ExileLink {
                source_id: other_source,
                exiled_id: exiled,
                kind: ExileLinkKind::TrackedBySource,
            }),
        }

        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(
            &[spell],
            if creature_spell {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
        );
        assert_eq!(
            outcome.mana_pool_total(P0),
            expected_mana,
            "{label}: only a card type shared with Cemetery Prowler's own linked-exile population reduces {{3}}"
        );
    }
}
