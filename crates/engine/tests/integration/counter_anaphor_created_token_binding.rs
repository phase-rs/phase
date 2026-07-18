//! BB-FU5 — a same-chain counter anaphor placed AFTER a token-creating clause
//! binds to the just-created token (`TargetFilter::LastCreated`), not the ability
//! source (`SelfRef`) or an unbound parent target (`ParentTarget`). Unifies two
//! parser call sites (`resolve_counter_placement_target`'s demonstrative arm and
//! `try_parse_for_each_effect`'s counter arm) on ONE helper,
//! `counter_anaphor_created_token_binding`.
//!
//! These drive the REAL parse/lower/resolve pipeline (`parse_effect_chain` plus
//! `resolve_ability_chain`) and assert RESOLVED counter placement, not AST bytes.
//! Each load-bearing FLIP names the assertion that reds when the fix is reverted
//! (measured by reverting the fix and re-running):
//!
//! `match_the_odds_for_each_it_binds_created_token` — reverting call site B to
//! `ParseContext::default()` re-binds "it" to `SelfRef`; token counters 3 -> 0.
//!
//! `grist_the_token_binds_created_token` — dropping "the token" from the helper
//! `alt` re-binds to unbound `ParentTarget`; token deathtouch counter 1 -> 0.
//!
//! `longstalk_brawl_the_creature_binds_chosen_target_not_token` — a no-regression
//! control proving "the creature" stays a chosen-target binding (resolves onto the
//! chosen creature, not a decoy token). The "the creature" EXCLUSION from the
//! `alt` is a forward-looking correctness measure: "the creature" is genuinely
//! ambiguous (Longstalk Brawl names a chosen target). Measured caveat: it is NOT
//! currently load-bearing — adding "the creature" to the `alt` flips ZERO corpus
//! cards, because Longstalk's `GiftDelivery` token is not seen by the
//! `token_created_in_chain` seeder (that flag is false here). The test reds only
//! if Longstalk's counter were ever (wrongly) parsed to `LastCreated` or if
//! `ParentTarget` resolution broke; it does not claim to discriminate the
//! exclusion itself.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::KeywordKind;
use engine::types::mana::ManaColor;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall) — a paraphrase can take a different parser
// branch, so the discriminating gate requires the real card's exact text.
const MATCH_THE_ODDS: &str =
    "Create a 1/1 white Ally creature token. Put a +1/+1 counter on it for each creature your opponents control.";
const GRIST_PLUS_ONE: &str = "Create a 1/1 black and green Insect creature token, then mill two cards. Put a deathtouch counter on the token if a black card was milled this way.";
const APPLIED_GEOMETRY: &str = "Create a token that's a copy of target non-Aura permanent you control, except it's a 0/0 Fractal creature in addition to its other types. Put six +1/+1 counters on it.";
const LONGSTALK_BRAWL: &str = "Gift a tapped Fish (You may promise an opponent a gift as you cast this spell. If you do, they create a tapped 1/1 blue Fish creature token before its other effects.)\nChoose target creature you control and target creature you don't control. Put a +1/+1 counter on the creature you control if the gift was promised. Then those creatures fight each other.";

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn deathtouch(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Keyword(KeywordKind::Deathtouch))
        .copied()
        .unwrap_or(0)
}

fn last_token(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .last_created_token_ids
        .last()
        .copied()
        .expect("a token was created earlier in the chain")
}

/// Create a bare source object (the resolving spell / planeswalker) so the
/// resolved chain has a valid `source_id`; also the `SelfRef` recipient a
/// reverted fix would wrongly target, so its counter count is a live guard.
fn add_source(runner: &mut GameRunner, name: &str) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    create_object(state, card_id, P0, name.to_string(), Zone::Battlefield)
}

fn add_creature_for(runner: &mut GameRunner, player: PlayerId) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        player,
        "Grizzly Bears".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    id
}

fn resolve(
    runner: &mut GameRunner,
    def: &AbilityDefinition,
    source: ObjectId,
    targets: Vec<TargetRef>,
) {
    let resolved = build_resolved_from_def_with_targets(def, source, P0, targets);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0).expect("chain resolves");
}

/// Call site B (for-each dispatch) — LOAD-BEARING FLIP. "Put a +1/+1 counter on
/// IT for each creature your opponents control" after a token-creating clause
/// binds the dynamic counter to the created Ally token (`LastCreated`), placing
/// one counter per opponent creature (3) ON THE TOKEN. Reverting call site B to
/// `ParseContext::default()` re-binds "it" to `SelfRef` (the resolving sorcery)
/// and reds `token counters == 3` (token would get 0; the sorcery would get 3).
#[test]
fn match_the_odds_for_each_it_binds_created_token() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let source = add_source(&mut runner, "Match the Odds");
    // Opponent (P1) controls exactly 3 creatures — the dynamic count.
    for _ in 0..3 {
        add_creature_for(&mut runner, P1);
    }

    let def = parse_def(MATCH_THE_ODDS);
    resolve(&mut runner, &def, source, vec![]);
    let token = last_token(&runner);

    assert_eq!(
        p1p1(&runner, token),
        3,
        "for-each 'it' binds the created token: 3 opponent creatures -> 3 counters ON THE TOKEN"
    );
    assert_eq!(
        p1p1(&runner, source),
        0,
        "the resolving sorcery (SelfRef) receives NO counters (reverting call site B moves them here)"
    );
}

/// Call site A demonstrative arm, B2 definite-article class — LOAD-BEARING FLIP.
/// grist's "Put a deathtouch counter on THE TOKEN if a black card was milled this
/// way" binds to the just-created Insect token. Setup mills two black cards so the
/// `ZoneChangedThisWay(black card)` condition passes; the discriminator is the
/// binding. Dropping "the token" from the helper `alt` re-binds to the unbound
/// `ParentTarget` and reds `token deathtouch == 1` (token would get 0).
#[test]
fn grist_the_token_binds_created_token() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let source = add_source(&mut runner, "Grist, the Plague Swarm");

    // Two BLACK cards on top of P0's library so the mill satisfies the
    // "if a black card was milled this way" condition deterministically.
    let mut black_cards = Vec::new();
    for i in 0..2 {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, P0, format!("Black Card {i}"), Zone::Library);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.color = vec![ManaColor::Black];
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        black_cards.push(id);
    }
    // Force library order: the two black cards are the top two milled.
    runner.state_mut().players[0].library = black_cards.clone().into();

    let def = parse_def(GRIST_PLUS_ONE);
    resolve(&mut runner, &def, source, vec![]);
    let token = last_token(&runner);

    // Reach-guard (non-vacuous): the token exists and a black card actually hit
    // the graveyard, so the condition input is present and the count assertion
    // below is not vacuously satisfied by a short-circuit.
    assert!(
        runner.state().objects[&token].is_token,
        "the Insect token was created (LastCreated is populated)"
    );
    assert!(
        black_cards
            .iter()
            .any(|c| runner.state().objects[c].zone == Zone::Graveyard),
        "a black card was milled (the ZoneChangedThisWay condition input is satisfied)"
    );

    assert_eq!(
        deathtouch(&runner, token),
        1,
        "'the token' definite-article anaphor binds the created Insect token (deathtouch counter ON THE TOKEN)"
    );
    assert_eq!(
        deathtouch(&runner, source),
        0,
        "grist itself (ParentTarget/source) receives no counter"
    );
}

/// Call site A it-branch refactor (behavior-preserving) + NO-DOUBLE-SEED control.
/// Applied Geometry's "Put six +1/+1 counters on IT" already bound `LastCreated`
/// via the pre-existing it-branch; the refactor to route it through the shared
/// helper must not change the count. The copy token has EXACTLY 6 counters (a
/// double-seed regression would show 12). CR 707.2: a copy carries no counters,
/// so the redirected PutCounter is the sole source.
#[test]
fn applied_geometry_it_exactly_six_no_double_seed() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let source = add_source(&mut runner, "Applied Geometry");
    let copy_target = add_creature_for(&mut runner, P0);

    let def = parse_def(APPLIED_GEOMETRY);
    resolve(
        &mut runner,
        &def,
        source,
        vec![TargetRef::Object(copy_target)],
    );
    let token = last_token(&runner);

    assert_eq!(
        p1p1(&runner, token),
        6,
        "exactly six counters on the copy token — no double-seed (would be 12), it-branch refactor is count-preserving"
    );
}

/// No-regression control for the "the creature" EXCLUSION. Longstalk Brawl's
/// "Put a +1/+1 counter on THE CREATURE you control" binds the CHOSEN target
/// (`ParentTarget` here in isolation; `ParentTargetSlot { 0 }` in the full-card
/// parse), NOT the gift-created Fish token. "the creature" is deliberately kept
/// OUT of the helper `alt` because it legitimately names a chosen target and is
/// ambiguous — keeping it out is a forward-looking correctness measure. Measured
/// caveat: it is not currently load-bearing (adding "the creature" flips ZERO
/// corpus cards, since Longstalk's `GiftDelivery` token is not seen by the
/// `token_created_in_chain` seeder). This test asserts the RESOLVED placement:
/// the counter lands on the chosen creature, not the decoy `LastCreated` token.
#[test]
fn longstalk_brawl_the_creature_binds_chosen_target_not_token() {
    let def = parse_def(LONGSTALK_BRAWL);

    // Parse-level boundary proof: the real card's counter still binds the chosen
    // slot, not the created token.
    let put = find_put_counter(&def).expect("longstalk brawl parses a PutCounter");
    let Effect::PutCounter { target, .. } = put else {
        unreachable!("find_put_counter returns a PutCounter effect")
    };
    // "the creature" is EXCLUDED from the anaphor set, so the counter keeps a
    // CHOSEN-TARGET binding (`ParentTarget` in isolation; the full-card parse
    // seeds the slot registry and yields `ParentTargetSlot { 0 }`) and does NOT
    // become `LastCreated`. Reds if Longstalk's counter were ever parsed to the
    // created token.
    assert!(
        !matches!(target, TargetFilter::LastCreated),
        "'the creature you control' must NOT bind the created token (got {target:?})"
    );
    assert!(
        matches!(
            target,
            TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. }
        ),
        "'the creature you control' binds the chosen target, not the token (got {target:?})"
    );

    // Resolved-delta proof: resolve the REAL parsed PutCounter over a chosen
    // target while a decoy token sits in `last_created_token_ids`. The counter
    // must land on the chosen creature, not the decoy token.
    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let source = add_source(&mut runner, "Longstalk Brawl");
    let chosen = add_creature_for(&mut runner, P0);
    let decoy_token = {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, P1, "Fish".to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.is_token = true;
        id
    };
    runner.state_mut().last_created_token_ids = vec![decoy_token];

    let resolved = ResolvedAbility::new(put.clone(), vec![TargetRef::Object(chosen)], source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("PutCounter resolves");

    assert_eq!(
        p1p1(&runner, chosen),
        1,
        "'the creature you control' -> the chosen target slot receives the counter"
    );
    assert_eq!(
        p1p1(&runner, decoy_token),
        0,
        "the created Fish token (LastCreated) receives NO counter — 'the creature' is excluded from the anaphor set"
    );
}

fn parse_def(oracle: &str) -> AbilityDefinition {
    engine::parser::oracle_effect::parse_effect_chain(oracle, AbilityKind::Spell)
}

/// Recurse the parsed definition's sub-ability spine to find the first
/// `Effect::PutCounter`.
fn find_put_counter(def: &AbilityDefinition) -> Option<&Effect> {
    if matches!(&*def.effect, Effect::PutCounter { .. }) {
        return Some(&def.effect);
    }
    def.sub_ability.as_deref().and_then(find_put_counter)
}
