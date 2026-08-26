//! CR 303.4i: an Aura token whose host is undefined or illegal is NOT created (#7302).
//!
//! > If an effect attempts to put an Aura onto the battlefield attached to
//! > either an object or player it can't legally enchant or an object or player
//! > that is undefined, the Aura remains in its current zone. … If the Aura is a
//! > token, it isn't created.
//!
//! The engine used to create the token hostless and let the CR 704.5m
//! state-based action sweep it to a graveyard. That is observably different: the
//! token existed for a beat, fired enters-the-battlefield triggers, and left a
//! graveyard entry.
//!
//! Questing Cosplayer is the card that surfaced it, and it needed the parser
//! half too — "create a Questing Role token **and attach it to** target
//! creature" is the ACTION surface of the same CR 303.4 relation Oracle
//! otherwise prints as "…token **attached to** target creature", and only the
//! state surface was recognised, so the token was created with no host at all.
//!
//! ## What each row proves
//!
//! The production rows cast the card and let its enters trigger resolve, so the
//! parser binding, target selection and the token seam are proven together. The
//! rows marked as building blocks call `token::resolve` directly: they reach
//! shapes a card's own targeting layer would never hand the resolver (an
//! `attach_to` filter with NO bound target) or that the replacement pipeline
//! would have to be taught to produce (an entrant that stopped being an Aura).
//! They are evidence about the seam, not about a card.
//!
//! The third discriminator the rule needs — an UNBOUND host reached through the
//! production pipeline — lives in `self_attached_aura_token_host.rs`, whose
//! `RoleChain` rows activate a real ability whose host authority names nothing
//! (`CostPaidObject`, `SourceOrPaired`, an unbound chosen player) and assert the
//! same CR 303.4i outcome behind a reach guard. It is not repeated here.
//!
//! There is deliberately NO replay of the shipped Questing Cosplayer here. The
//! curated fixture stores the export's PRE-BAKED parse, which the card-data
//! pipeline regenerates — so a fixture replay would assert the old parse until
//! that pipeline runs, and would prove nothing about a parser change either way.
//! The rows below build the same printed sentence from Oracle text, which is
//! parsed by the parser under test at run time; for this PR that is the stronger
//! evidence, not the weaker one.

use std::collections::HashSet;

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    Effect, PtValue, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypeFilter,
    TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::proposed_event::{
    ProposedEvent, TokenCharacteristics, TokenHostRequest, TokenSpec,
};
use engine::types::zones::{EtbTapState, Zone};

/// Questing Cosplayer's printed sentence with the reminder text dropped. Built
/// from Oracle text so the row runs in CI, where only the curated fixture
/// exists; the shipped card is replayed separately below.
const COSPLAYER_ETB: &str =
    "When this creature enters, create a Questing Role token and attach it to target creature.";

/// A host that is perfectly legal to TARGET and illegal to ENCHANT — CR 303.4i's
/// "can't legally enchant" half. Protection cannot express it: protection would
/// stop the trigger from targeting the creature at all, the trigger would never
/// resolve, and the empty board would prove nothing about the token seam.
const CANT_BE_ENCHANTED: &str = "This creature can't be enchanted.";

fn pool(generic: usize, colors: &[ManaType]) -> Vec<ManaUnit> {
    (0..generic)
        .map(|_| ManaType::Colorless)
        .chain(colors.iter().copied())
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

/// Every Role token in the game, in EVERY zone on purpose: the defect's
/// signature is a token that reached the battlefield and was swept, which a
/// battlefield-only query cannot tell apart from one that was never created.
fn role_tokens(
    runner: &engine::game::scenario::GameRunner,
) -> Vec<&engine::game::game_object::GameObject> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.card_types.subtypes.iter().any(|s| s == "Role"))
        .collect()
}

// ── Production pipeline ─────────────────────────────────────────────────────

/// The positive case, end to end: cast the creature, let its enters trigger
/// choose a target, and the Role must enter attached to it.
///
/// This is also the reach guard for the negative row below — the two share a
/// board, so a harness that could not create the Role at all would fail here
/// first rather than passing the negative vacuously.
#[test]
fn questing_cosplayers_role_enters_attached_to_the_chosen_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(4, &[]));
    let host = scenario.add_creature(P0, "Chosen Host", 2, 2).id();
    let cosplayer = scenario
        .add_creature_to_hand_from_oracle(P0, "Cosplaying Bard", 1, 1, COSPLAYER_ETB)
        .id();
    let mut runner = scenario.build();

    runner.cast(cosplayer).target_object(host).resolve();
    runner.advance_until_stack_empty();

    let tokens = role_tokens(&runner);
    assert_eq!(tokens.len(), 1, "the Role token must be created");
    assert_eq!(tokens[0].zone, Zone::Battlefield);
    assert_eq!(
        tokens[0].attached_to,
        Some(AttachTarget::Object(host)),
        "the Role enters attached to the creature the trigger targeted"
    );
}

/// CR 303.4i, the "can't legally enchant" half — the half the first head of this
/// PR left unfixed.
///
/// The host is a legal TARGET (nothing here restricts targeting), so the trigger
/// goes on the stack and resolves; the Role is simply not created. The reach
/// guard is the row above, which runs the identical board with an enchantable
/// host and gets its token.
#[test]
fn a_defined_but_illegal_host_creates_no_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(4, &[]));
    let host = scenario
        .add_creature_from_oracle(P0, "Unenchantable Host", 2, 2, CANT_BE_ENCHANTED)
        .id();
    let cosplayer = scenario
        .add_creature_to_hand_from_oracle(P0, "Cosplaying Bard", 1, 1, COSPLAYER_ETB)
        .id();
    let mut runner = scenario.build();

    runner.cast(cosplayer).target_object(host).resolve();
    runner.advance_until_stack_empty();

    assert!(
        runner.state().battlefield.contains(&cosplayer),
        "reach guard: the creature spell resolved, so its enters trigger ran"
    );
    assert!(
        role_tokens(&runner).is_empty(),
        "CR 303.4i: an Aura token whose host can't legally be enchanted is not created"
    );
    assert!(
        runner.state().objects[&host].attachments.is_empty(),
        "and nothing was attached to the host on the way past"
    );
    // The discriminating assertion. "No Role token in any zone" alone is
    // VACUOUS here: without the gate the token is created, `attach::attach_to`
    // refuses the illegal host, the CR 704.5m unattached-Aura state-based action
    // moves it, and CR 111.7 ends it there — leaving the same empty board. What
    // separates "never created" from "created and swept" is the trail: the
    // anaphora slot a later `TargetFilter::LastCreated` reads.
    assert!(
        runner.state().last_created_token_ids.is_empty(),
        "CR 303.4i: an entry the rule denies leaves no created-token row behind, \
         got {:?}",
        runner.state().last_created_token_ids
    );
}

// ── Building blocks at the token seam ───────────────────────────────────────

/// A "Cursed Role" Aura token created attached to whatever `attach_to` names.
fn aura_token_effect(attach_to: Option<TargetFilter>) -> Effect {
    token_effect(
        "Cursed Role",
        vec![
            "Enchantment".to_string(),
            "Aura".to_string(),
            "Role".to_string(),
        ],
        attach_to,
    )
}

fn token_effect(name: &str, types: Vec<String>, attach_to: Option<TargetFilter>) -> Effect {
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(0),
        toughness: PtValue::Fixed(0),
        types,
        colors: vec![],
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

fn creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: None,
        properties: Vec::new(),
    })
}

struct Board {
    runner: engine::game::scenario::GameRunner,
    source: ObjectId,
    host: ObjectId,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let mut runner = scenario.build();
    let source = ObjectId(9001);
    runner.state_mut().objects.insert(
        source,
        engine::game::game_object::GameObject::new(
            source,
            CardId(9001),
            P0,
            "Token Source".to_string(),
            Zone::Battlefield,
        ),
    );
    Board {
        runner,
        source,
        host,
    }
}

fn resolve(board: &mut Board, effect: Effect, targets: Vec<TargetRef>) {
    let ability = ResolvedAbility::new(effect, targets, board.source, P0);
    let mut events = Vec::new();
    engine::game::effects::token::resolve(board.runner.state_mut(), &ability, &mut events)
        .expect("token effect resolves");
}

fn tokens_named(board: &Board, needle: &str) -> Vec<ObjectId> {
    board
        .runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.name.contains(needle))
        .map(|object| object.id)
        .collect()
}

/// CR 303.4i, the "undefined" half: the instruction names a host, nothing binds
/// it, so no token is created — and, unlike the create-then-sweep path, nothing
/// lands in a graveyard either.
///
/// Reverting the gate turns this red: the token is created, the CR 704.5m SBA
/// moves it, and the graveyard half of the assertion fails.
#[test]
fn an_aura_token_with_an_unbound_host_is_not_created() {
    let mut board = board();
    resolve(
        &mut board,
        aura_token_effect(Some(creature_filter())),
        vec![],
    );

    assert!(
        tokens_named(&board, "Role").is_empty(),
        "CR 303.4i: an Aura token with an undefined host is not created, in any zone"
    );
    assert!(
        board.runner.state().last_created_token_ids.is_empty(),
        "CR 303.4i: an entry the rule denies leaves no created-token row behind, got {:?}",
        board.runner.state().last_created_token_ids
    );
}

/// Positive counter-direction at the seam: a bound host still creates the token
/// and attaches it.
#[test]
fn a_bound_host_still_gets_its_aura_token() {
    let mut board = board();
    let host = board.host;
    resolve(
        &mut board,
        aura_token_effect(Some(creature_filter())),
        vec![TargetRef::Object(host)],
    );

    let tokens = tokens_named(&board, "Role");
    assert_eq!(tokens.len(), 1, "the Role token must be created");
    assert_eq!(
        board.runner.state().objects[&tokens[0]]
            .attached_to
            .as_ref()
            .and_then(|attached| attached.as_object()),
        Some(host),
        "the Role token must enter attached to its host"
    );
}

/// CR 303.4h: "If an effect attempts to put a permanent that isn't an Aura,
/// Equipment, or Fortification onto the battlefield attached to an object or
/// player, it enters the battlefield unattached."
///
/// Same undefined host as the row above, on a token that is not an Aura. The
/// rule's disposition is different — it is created, just unattached — so this is
/// what keeps the gate from spreading past the class CR 303.4i names.
#[test]
fn a_non_aura_token_with_an_unbound_host_is_created_unattached() {
    let mut board = board();
    resolve(
        &mut board,
        token_effect(
            "Spirit",
            vec!["Creature".to_string(), "Spirit".to_string()],
            Some(creature_filter()),
        ),
        vec![],
    );

    let tokens = tokens_named(&board, "Spirit");
    assert_eq!(tokens.len(), 1, "CR 303.4h: the token is created anyway");
    let token = &board.runner.state().objects[&tokens[0]];
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(token.attached_to, None, "and it enters unattached");
}

/// An ordinary token that names no host at all is untouched, so nothing about
/// the common create-a-token path changes.
#[test]
fn an_ordinary_token_without_a_host_is_still_created() {
    let mut board = board();
    let before = board.runner.state().battlefield.len();
    resolve(&mut board, aura_token_effect(None), vec![]);

    assert_eq!(
        board.runner.state().battlefield.len(),
        before + 1,
        "a token that names no host is created as before"
    );
}

/// WHY the gate sits after the CR 614 replacement pipeline rather than before
/// the proposal.
///
/// A replacement effect may change what is actually entering. The event below is
/// what the pipeline hands the apply path in that case: the instruction named a
/// host and nothing bound it (`TokenHostRequest::Unbound`), but the entrant is
/// no longer an Aura — so CR 303.4i is not its rule and it must be created. A
/// gate keyed on the pre-replacement announcement, which is what the first head
/// of this PR had, would have suppressed it.
///
/// What this row does NOT prove: that any shipped card replaces an Aura token
/// with a non-Aura one. None does today. It proves that the seam reads the
/// ENTRANT rather than the announcement, which is the property that makes the
/// placement correct rather than incidental.
#[test]
fn a_replacement_that_leaves_a_non_aura_entrant_still_creates_the_token() {
    let mut board = board();
    let spec = TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: "Spirit".to_string(),
            power: Some(1),
            toughness: Some(1),
            core_types: vec![CoreType::Creature],
            // What the replacement left behind: no Aura subtype.
            subtypes: vec!["Spirit".to_string()],
            supertypes: vec![],
            colors: vec![],
            keywords: vec![],
        },
        script_name: "Spirit".to_string(),
        static_abilities: vec![],
        enter_with_counters: vec![],
        tapped: false,
        enters_attacking: false,
        sacrifice_at: None,
        source_id: board.source,
        controller: P0,
        // The announcement is untouched by the replacement: the instruction
        // named a host and nothing bound it.
        attach_to: TokenHostRequest::Unbound,
    };

    let mut events = Vec::new();
    engine::game::effects::token::apply_create_token_after_replacement(
        board.runner.state_mut(),
        ProposedEvent::CreateToken {
            owner: P0,
            spec: Box::new(spec),
            copy: None,
            enter_tapped: EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        },
        &mut events,
    );

    let tokens = tokens_named(&board, "Spirit");
    assert_eq!(
        tokens.len(),
        1,
        "the entrant is not an Aura, so CR 303.4i does not deny it"
    );
    assert_eq!(
        board.runner.state().objects[&tokens[0]].attached_to,
        None,
        "CR 303.4h: it enters unattached"
    );
}
