//! CR 608.2c + CR 611.2c — the bare plural pronoun "they" as an anaphoric
//! continuous-clause subject in a bare-" and " conjunct.
//!
//! "Put a +1/+1 counter on each [of those] creature[s] and they gain
//! <keyword[s]> until end of turn" back-references the objects the counter
//! clause acted on. Before this fix the conjunct fell through the bare-and
//! clause splitter (which recognized "those creatures" / "that creature" but
//! not the bare pronoun "they"), so `try_split_targeted_compound` fed the
//! conjunct to the imperative-only parser, which emitted
//! `Effect::Unimplemented { name: "they", description: "they gain ..." }`.
//!
//! Splitting the conjunct routes "they gain <kw list>" through
//! `parse_clause_ast` → `try_parse_subject_continuous_clause`, where "they"
//! binds the frozen mass population as a `TrackedSet{0}` sentinel (CR 608.2c:
//! the population the counter clause acted on, frozen at resolution).
//! `ParentTarget` is target-relative — on a spell that declares a target
//! (Call for Aid's "target opponent") it misbinds to that target instead of
//! the population — so the sentinel is the uniformly-correct binding, and the
//! comma-and keyword list lowers to one `AddKeyword` per keyword carrying the
//! grant's duration (CR 611.2c). At resolution the sentinel binds the chain
//! tracked set the counter clause lazily publishes (`CounterAdded` harvest in
//! `affected_objects_from_events`).
//!
//! Cards unlocked: Unbreakable Formation (addendum), Overseer of Vault 76
//! (reflexive remove-counters trigger), and the multi-keyword list form shared
//! with Last Night Together.

use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityDefinition, ContinuousModification, Effect, TargetFilter};
use engine::types::keywords::Keyword;

/// Walk the sub_ability chain and collect every `AddKeyword` granted to an
/// anaphoric frozen-population subject (the "they gain ..." continuous grant).
/// After a same-chain mass ("each ...") clause, "they" binds the `TrackedSet`
/// sentinel for the frozen population — never the target-relative
/// `ParentTarget`, which misbinds on spells that declare a target.
fn they_gain_keywords(def: &AbilityDefinition) -> Vec<Keyword> {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Effect::GenericEffect {
            static_abilities, ..
        } = &*d.effect
        {
            let is_anaphor = static_abilities
                .iter()
                .any(|s| matches!(s.affected, Some(TargetFilter::TrackedSet { .. })));
            if is_anaphor {
                let kws: Vec<Keyword> = static_abilities
                    .iter()
                    .flat_map(|s| s.modifications.iter())
                    .filter_map(|m| match m {
                        ContinuousModification::AddKeyword { keyword } => Some(keyword.clone()),
                        _ => None,
                    })
                    .collect();
                if !kws.is_empty() {
                    return kws;
                }
            }
        }
        cur = d.sub_ability.as_deref();
    }
    Vec::new()
}

/// No clause in the parsed chain may remain `Effect::Unimplemented`.
fn assert_no_unimplemented(def: &AbilityDefinition) {
    let mut cur = Some(def);
    while let Some(d) = cur {
        assert!(
            !matches!(&*d.effect, Effect::Unimplemented { .. }),
            "unexpected Unimplemented clause: {:?}",
            d.effect
        );
        cur = d.sub_ability.as_deref();
    }
}

/// Unbreakable Formation addendum — single-keyword "they gain" after a
/// "put a +1/+1 counter on each of those creatures" conjunct.
#[test]
fn unbreakable_formation_addendum_they_gain_vigilance() {
    let def = parse_effect_chain(
        "Put a +1/+1 counter on each of those creatures and they gain vigilance until end of turn.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert_no_unimplemented(&def);
    assert_eq!(they_gain_keywords(&def), vec![Keyword::Vigilance]);
}

/// Overseer of Vault 76 reflexive clause — "they gain vigilance" after
/// "put a +1/+1 counter on each creature you control".
#[test]
fn overseer_of_vault_76_they_gain_vigilance() {
    let def = parse_effect_chain(
        "Put a +1/+1 counter on each creature you control and they gain vigilance until end of turn.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert_no_unimplemented(&def);
    assert_eq!(they_gain_keywords(&def), vec![Keyword::Vigilance]);
}

/// The multi-keyword comma-and list ("vigilance, indestructible, and haste")
/// after a bare-" and they gain" conjunct emits one AddKeyword per keyword.
#[test]
fn and_they_gain_emits_keyword_list() {
    let def = parse_effect_chain(
        "Put a +1/+1 counter on each creature you control and they gain vigilance, indestructible, and haste until end of turn.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert_no_unimplemented(&def);
    assert_eq!(
        they_gain_keywords(&def),
        vec![Keyword::Vigilance, Keyword::Indestructible, Keyword::Haste]
    );
}

/// Runtime proof for the frozen-set binding: the counter clause lazily
/// publishes its affected population, and the "they gain" grant bound by the
/// parser lands on exactly those creatures. The Unbreakable Formation
/// addendum shape ("put a +1/+1 counter on each creature you control and they
/// gain vigilance until end of turn"), driven from parsed spell text through
/// the production parse/lower/resolve pipeline (`parse_effect_chain` +
/// `build_resolved_from_def_with_targets` + `resolve_ability_chain`) so no
/// card-data fixture entry is needed — and so the test reds if the parser
/// ever binds "they" to `ParentTarget` instead of the frozen set.
#[test]
fn counter_chain_they_gain_vigilance_lands_on_countered_creatures() {
    use engine::game::ability_utils::build_resolved_from_def_with_targets;
    use engine::game::effects::resolve_ability_chain;
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    let mut state = GameState::new_two_player(42);
    let mk_creature = |state: &mut GameState, card: u64, owner: PlayerId, name: &str| {
        let id = create_object(
            state,
            CardId(card),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    };
    let ally_a = mk_creature(&mut state, 1, PlayerId(0), "Ally A");
    let ally_b = mk_creature(&mut state, 2, PlayerId(0), "Ally B");
    let foe = mk_creature(&mut state, 3, PlayerId(1), "Foe");
    // The spell source is not a creature, so "each creature you control" is
    // exactly the two allies (mirrors Unbreakable Formation's spell source).
    let source = create_object(
        &mut state,
        CardId(99),
        PlayerId(0),
        "Unbreakable Formation".to_string(),
        Zone::Battlefield,
    );

    let def = parse_effect_chain(
        "Put a +1/+1 counter on each creature you control and they gain vigilance until end of turn.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert_no_unimplemented(&def);
    assert_eq!(they_gain_keywords(&def), vec![Keyword::Vigilance]);
    let head = build_resolved_from_def_with_targets(&def, source, PlayerId(0), vec![]);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &head, &mut events, 0).unwrap();
    engine::game::layers::flush_layers(&mut state);

    for ally in [ally_a, ally_b] {
        assert_eq!(
            state.objects[&ally]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "ally must receive the +1/+1 counter"
        );
        assert!(
            state.objects[&ally].has_keyword(&Keyword::Vigilance),
            "ally must gain vigilance from the frozen-set grant"
        );
    }
    assert!(
        !state.objects[&foe]
            .counters
            .contains_key(&CounterType::Plus1Plus1),
        "opponent's creature must not receive a counter"
    );
    assert!(
        !state.objects[&foe].has_keyword(&Keyword::Vigilance),
        "opponent's creature must not gain vigilance"
    );
}
