//! Bring to Light — "Converge — Search your library for a creature, instant, or
//! sorcery card with mana value less than or equal to the number of colors of
//! mana spent to cast this spell, exile that card, ..."
//!
//! Regression for issue #2892. The search filter is an `Or` of three type legs
//! ("creature, instant, or sorcery card") with a single trailing "with mana
//! value ≤ N" suffix. The parser builds each comma/or disjunct independently
//! (CR 701.23a), so only the FINAL (Sorcery) leg carried the `Cmc ≤ N`
//! predicate — the Creature and Instant legs were left unconstrained. As a
//! result a high-mana-value creature was wrongly findable at any converge.
//!
//! The fix distributes the trailing predicate back onto every `Typed` leg of the
//! disjunction. This test drives the REAL parsed Bring to Light search through
//! the resolver, arranging a converge of 5 (the maximum — converge counts
//! DISTINCT colors of mana spent and only five colors exist), and asserts that:
//!   * an MV-5 creature IS a legal find (constraint does not over-reject), and
//!   * an MV-6 creature (mana cost {2/B}{2/R}{2/G}, CR 202.3f) is NOT findable.
//!
//! The MV-6-creature exclusion is the load-bearing assertion: pre-fix the
//! Creature leg is unconstrained, so the MV-6 creature is WRONGLY offered and
//! the test fails. This is a pipeline test (parse → build → resolve_ability_chain
//! → SearchChoice candidate set), not an AST-shape test.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ColoredManaCount, ManaColor, ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BRING_TO_LIGHT: &str = "Converge — Search your library for a creature, instant, or sorcery \
card with mana value less than or equal to the number of colors of mana spent to cast this spell, \
exile that card, then shuffle. You may cast that card without paying its mana cost.";

fn add_library_creature(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
    mana_cost: ManaCost,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.mana_cost = mana_cost;
    id
}

#[test]
fn bring_to_light_converge_five_excludes_mana_value_six_creature() {
    let parsed = parse_oracle_text(
        BRING_TO_LIGHT,
        "Bring to Light",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let definition = parsed
        .abilities
        .first()
        .expect("Bring to Light should parse as a spell ability");
    // Sanity: the root effect is the SearchLibrary disjunction under test.
    assert!(
        matches!(definition.effect.as_ref(), Effect::SearchLibrary { .. }),
        "expected SearchLibrary root effect, got {:?}",
        definition.effect
    );

    let mut state = GameState::new_two_player(42);

    // The resolving Bring to Light spell object. Converge = 5 distinct colors of
    // mana spent (the physical maximum). The ability is anchored on this object's
    // own id (below), and `ManaSpentToCast { scope: SelfObject }` reads its
    // `colors_spent_to_cast`. `create_object` assigns ids sequentially, so the
    // spell's concrete id must be threaded through — not assumed.
    let spell = create_object(
        &mut state,
        CardId(100),
        PlayerId(0),
        "Bring to Light".to_string(),
        Zone::Stack,
    );
    let mut converge = ColoredManaCount::default();
    for color in ManaColor::ALL {
        converge.add(color, 1);
    }
    state.objects.get_mut(&spell).unwrap().colors_spent_to_cast = converge;

    // MV-5 creature: a legal find at converge 5 (control proving no over-reject).
    let mv5 = add_library_creature(
        &mut state,
        1,
        PlayerId(0),
        "MV5 Creature",
        ManaCost::generic(5),
    );
    // CR 202.3f: {2/B}{2/R}{2/G} is mana value 6 (largest component of each
    // monocolored-hybrid symbol). The Reaper analog — MUST be excluded at
    // converge 5, since 6 > 5.
    let mv6 = add_library_creature(
        &mut state,
        2,
        PlayerId(0),
        "MV6 Creature",
        ManaCost::Cost {
            shards: vec![
                ManaCostShard::TwoBlack,
                ManaCostShard::TwoRed,
                ManaCostShard::TwoGreen,
            ],
            generic: 0,
        },
    );

    let ability = build_resolved_from_def(definition, spell, PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    match &state.waiting_for {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(
                cards.contains(&mv5),
                "MV-5 creature must be a legal Bring to Light find at converge 5 (5 ≤ 5); \
                 the distributed Cmc constraint must not over-reject. Got {cards:?}"
            );
            assert!(
                !cards.contains(&mv6),
                "MV-6 creature ({{2/B}}{{2/R}}{{2/G}}, CR 202.3f) must NOT be findable at \
                 converge 5 (6 > 5). Pre-fix the unconstrained Creature leg wrongly offers it. \
                 Got {cards:?}"
            );
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }
}
