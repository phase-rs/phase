//! Esper Terra (back face) — chapters I/II/III: "Create a token that's a copy of
//! target nonlegendary enchantment you control. It gains haste. If it's a Saga,
//! put up to three lore counters on it. Sacrifice it at the beginning of your
//! next end step." — plus chapter IV's conjunctive mana.
//!
//! Drives the REAL parse → lower → resolve pipeline (`parse_effect_chain` +
//! `resolve_ability_chain`). The load-bearing §B2 discriminator is COUNTER
//! LOCATION: the three lore counters must land on the CREATED TOKEN
//! (`TargetFilter::LastCreated`), NOT on the ability source (`SelfRef` = Esper
//! Terra). Reverting the §B2 bind reds `saga_copy_puts_three_lore_on_token`.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::game::{stack, zones};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaType;
use engine::types::zones::Zone;

const ESPER_CHAPTER: &str = "Create a token that's a copy of target nonlegendary enchantment you control. It gains haste. If it's a Saga, put up to three lore counters on it. Sacrifice it at the beginning of your next end step.";

/// A four-chapter Saga whose chapters gain distinct binary-weighted life so the
/// summed delta (1+2+4+8 = 15) uniquely proves ALL four resolved.
const TEST_SAGA_ORACLE: &str =
    "(As this Saga enters and after your draw step, add a lore counter.)\n\
I — You gain 1 life.\n\
II — You gain 2 life.\n\
III — You gain 4 life.\n\
IV — You gain 8 life.";

/// Create a bare enchantment on P0's battlefield. `saga` adds the Saga subtype —
/// the "if it's a Saga" gate reads the COPIED enchantment's type (CR 707.2).
fn add_enchantment(runner: &mut GameRunner, name: &str, saga: bool) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = zones::create_object(state, card_id, P0, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    if saga {
        obj.card_types.subtypes.push("Saga".to_string());
    }
    obj.base_card_types = obj.card_types.clone();
    id
}

fn lore(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0)
}

/// Resolve Esper's chapter (source `esper`) over a copy of `target`, returning the
/// created token id and the resolution events (for chapter-trigger processing).
fn resolve_chapter_over(
    runner: &mut GameRunner,
    esper: ObjectId,
    target: ObjectId,
) -> (ObjectId, Vec<engine::types::events::GameEvent>) {
    let def = parse_effect_chain(ESPER_CHAPTER, AbilityKind::Spell);
    let resolved =
        build_resolved_from_def_with_targets(&def, esper, P0, vec![TargetRef::Object(target)]);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter chain resolves");
    let token = runner
        .state()
        .last_created_token_ids
        .last()
        .copied()
        .expect("CopyTokenOf created a token");
    (token, events)
}

/// §B2 (Saga quadrant, PASSING discriminator): copying a SAGA lands the three
/// lore counters ON THE TOKEN (`LastCreated`), leaving Esper Terra's own lore at
/// 0. Revert-to-red = flipping the §B2 bind back to `SelfRef` sends the counters
/// to Esper (esper lore 3, token lore 0).
#[test]
fn saga_copy_puts_three_lore_on_token_not_esper() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let esper = add_enchantment(&mut runner, "Esper Terra", true);
    let saga_target = add_enchantment(&mut runner, "Copy Target Saga", true);

    let (token, _events) = resolve_chapter_over(&mut runner, esper, saga_target);

    assert_eq!(
        lore(&runner, token),
        3,
        "§B2: the three lore counters land on the created token"
    );
    assert_eq!(
        lore(&runner, esper),
        0,
        "§B2: Esper Terra's own lore is unchanged (counters do NOT hit the source)"
    );
}

/// B2.5 gate (non-Saga quadrant, PASSING discriminator): copying a NON-Saga
/// enchantment adds ZERO lore counters — the "if it's a Saga" gate reads the
/// copied enchantment's type (CR 707.2 ≡ the token's), which is false here.
/// Revert-to-red = a gate that (wrongly) read the always-Saga source would place
/// three counters (on token or source).
#[test]
fn non_saga_copy_adds_no_lore() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let esper = add_enchantment(&mut runner, "Esper Terra", true);
    let non_saga = add_enchantment(&mut runner, "Plain Enchantment", false);

    let (token, _events) = resolve_chapter_over(&mut runner, esper, non_saga);

    assert_eq!(
        lore(&runner, token),
        0,
        "gate false for a non-Saga copy → 0 lore on the token"
    );
    assert_eq!(
        lore(&runner, esper),
        0,
        "gate false → 0 lore on Esper Terra too (not routed to the always-Saga source)"
    );
}

/// Blocker-1 regression guard (CR 714.3a): a token copy of a Saga enters with
/// EXACTLY ONE lore counter. Element 2 of the `token_copy.rs:447-454` etb chain
/// (`self_etb_counter_replacements`) already carries the CR 714.3a lore counter
/// that `oracle_saga.rs:200-211` emits; a future swap of `:448` to
/// `intrinsic_entry_counters_for_face` would double-seed to 2.
/// Revert-to-red = that swap → entry lore == 2.
#[test]
fn saga_token_enters_with_exactly_one_lore() {
    let mut scenario = GameScenario::new();
    let real_saga = scenario
        .add_creature(P0, "Test Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(TEST_SAGA_ORACLE)
        .id();
    let mut runner = scenario.build();
    let esper = add_enchantment(&mut runner, "Esper Terra", true);

    // Resolve ONLY the root CopyTokenOf (no sub-abilities) to isolate ETB seeding
    // from the chapter's explicit "put up to three lore counters".
    let def = parse_effect_chain(ESPER_CHAPTER, AbilityKind::Spell);
    let copy_only = ResolvedAbility::new(
        (*def.effect).clone(),
        vec![TargetRef::Object(real_saga)],
        esper,
        P0,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &copy_only, &mut events, 0)
        .expect("CopyTokenOf resolves");
    let token = runner
        .state()
        .last_created_token_ids
        .last()
        .copied()
        .expect("token created");

    assert_eq!(
        lore(&runner, token),
        1,
        "CR 714.3a: token copy of a Saga enters with EXACTLY one lore counter"
    );
}

/// Drain the trigger stack that `events` queue, re-processing any events emitted
/// by each resolution (a resolving chapter may itself add counters).
fn settle_triggers(runner: &mut GameRunner, events: &[GameEvent]) {
    process_triggers(runner.state_mut(), events);
    for _ in 0..48 {
        if runner.state().stack.is_empty() {
            break;
        }
        let mut ev = Vec::new();
        stack::resolve_top(runner.state_mut(), &mut ev);
        if !ev.is_empty() {
            process_triggers(runner.state_mut(), &ev);
        }
    }
}

/// PIN chapter-IV observable (CR 714.2b): once its lore counters land on the
/// token (the §B2 recipient — proven separately by
/// `saga_copy_puts_three_lore_on_token_not_esper`), a token copy of a 4-chapter
/// Saga advances ITS OWN chapters — the ETB seed fires chapter I, then each of the
/// three chapter counters (1→2→3→4) fires chapters II/III/IV ON THE TOKEN. The
/// binary-weighted life gains (1/2/4/8) sum to 15 iff all four resolved.
///
/// Counters are driven per CR 714.2b crossing order, one at a time, mirroring a
/// Saga's per-turn advancement (each +1 crosses exactly one chapter threshold).
/// Revert-to-red: if any chapter fails to resolve on the token, the life delta is
/// not 15. This is the "no shipping over a broken runtime" guard for the token's
/// chapter mechanism; the §B2 counter-RECIPIENT binding is the separate discriminator.
#[test]
fn saga_token_advances_through_chapter_iv() {
    let mut scenario = GameScenario::new();
    let real_saga = scenario
        .add_creature(P0, "Test Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(TEST_SAGA_ORACLE)
        .id();
    let mut runner = scenario.build();
    let esper = add_enchantment(&mut runner, "Esper Terra", true);

    let life_before = runner.state().players[0].life;

    // Step 1: resolve CopyTokenOf alone → token enters at 1 lore; settle chapter I.
    let def = parse_effect_chain(ESPER_CHAPTER, AbilityKind::Spell);
    let copy_only = ResolvedAbility::new(
        (*def.effect).clone(),
        vec![TargetRef::Object(real_saga)],
        esper,
        P0,
    );
    let mut copy_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &copy_only, &mut copy_events, 0)
        .expect("CopyTokenOf resolves");
    let token = runner
        .state()
        .last_created_token_ids
        .last()
        .copied()
        .expect("token created");
    settle_triggers(&mut runner, &copy_events);

    // Step 2: the chapter adds three lore to the TOKEN (the §B2-corrected recipient
    // — `LastCreated` binds the just-created token, not Esper). Added one at a time,
    // mirroring a Saga's per-turn advancement: 1→2 (chapter II), 2→3 (III), 3→4
    // (IV). Each +1 crossing resolves its chapter on the token.
    let token_recipient = TargetFilter::LastCreated;
    for _ in 0..3 {
        let put = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Lore,
                count: QuantityExpr::Fixed { value: 1 },
                target: token_recipient.clone(),
            },
            Vec::new(),
            esper,
            P0,
        );
        let mut put_events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &put, &mut put_events, 0)
            .expect("PutCounter resolves");
        settle_triggers(&mut runner, &put_events);
    }
    assert_eq!(lore(&runner, token), 4, "token reaches 4 lore (1 ETB + 3)");

    assert_eq!(
        runner.state().players[0].life - life_before,
        15,
        "chapters I(1)+II(2)+III(4)+IV(8) all resolved on the token (sum uniquely = 15)"
    );
    assert_eq!(
        lore(&runner, esper),
        0,
        "§B2: Esper Terra's own lore is unchanged"
    );
}

/// Gap-B runtime (CR 106.4): chapter IV's "Add {W}{W}, {U}{U}, {B}{B}, {R}{R},
/// and {G}{G}" fills the controller's mana pool with exactly two of each color
/// (ten total). Revert-to-red = without the conjunctive parser arm the clause is
/// `Unimplemented` and no mana is added.
#[test]
fn chapter_iv_conjunctive_mana_fills_pool_two_of_each() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    let def = parse_effect_chain(
        "Add {W}{W}, {U}{U}, {B}{B}, {R}{R}, and {G}{G}.",
        AbilityKind::Spell,
    );
    let resolved = ResolvedAbility::new((*def.effect).clone(), Vec::new(), ObjectId(0), P0);
    let mut events = Vec::new();
    engine::game::effects::mana::resolve(runner.state_mut(), &resolved, &mut events)
        .expect("conjunctive mana resolves");

    let pool = &runner.state().players[0].mana_pool;
    for color in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        assert_eq!(
            pool.count_color(color),
            2,
            "pool must gain exactly two {color:?} mana"
        );
    }
    assert_eq!(
        pool.total(),
        10,
        "ten mana total, two of each of five colors"
    );
}
