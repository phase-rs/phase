//! CR 118.12 + CR 118.3: "If [a player] does" checks whether the player chose to
//! pay an optional cost or started to pay a mandatory one, regardless of what
//! events actually occurred. "Discard your hand" requires exactly the cards in
//! that hand, so with an empty hand it requires zero cards and is paid in full:
//! its "If you do" consequence still happens. Moonveil Regent ruling (2021-09-24):
//! "If you have no cards in hand as Moonveil Regent's first ability resolves, you
//! may still choose to discard your hand. You'll just draw a card for each of that
//! spell's colors." Knowledge Vault ruling (2009-10-01): "If you are able to do
//! so, you discard your hand and put all cards exiled with Knowledge Vault into
//! their owners' hands."
//!
//! CR 603.12 + CR 701.9a: a reflexive "When you do" triggers based on whether its
//! trigger event occurred, and a discard is moving a card from hand to graveyard,
//! so a discard that moved no card triggers no "When you do" (Miasma Demon ruling
//! 2024-09-20: the reflexive "triggers when you discard cards this way").
//!
//! Every fixture drives the real pipeline (cast, activation or trigger →
//! `apply`). Printed cards use their verbatim Oracle text; synthetic twins are
//! labelled as such and pinned to zero `Unimplemented` nodes.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MOONVEIL_REGENT: &str = "Flying\nWhenever you cast a spell, you may discard your hand. If you do, draw a card for each of that spell's colors.\nWhen this creature dies, it deals X damage to any target, where X is the number of colors among permanents you control.";

const KNOWLEDGE_VAULT: &str = "{2}, {T}: Exile the top card of your library face down.\n{0}: Sacrifice this artifact. If you do, discard your hand, then put all cards exiled with this artifact into their owner's hand.\nWhen this artifact leaves the battlefield, put all cards exiled with it into their owner's graveyard.";

const FLUSH_OUT: &str =
    "Discard a card. If you do, draw two cards. (Then shuffle this card into its owner's library.)";

const WHEEL_OF_FORTUNE: &str = "Each player discards their hand, then draws seven cards.";

const SENSATION_GORGER: &str = "Kinship — At the beginning of your upkeep, you may look at the top card of your library. If it shares a creature type with this creature, you may reveal it. If you do, each player discards their hand, then draws four cards.";

const MIASMA_DEMON: &str = "Flying\nWhen this creature enters, you may discard any number of cards. When you do, up to that many target creatures each get -2/-2 until end of turn.";

/// Library of Leng, verbatim Scryfall (fetched 2026-10-30). Its second line is an
/// optional discard replacement for its controller's discards, so each card
/// that player discards raises an apply-or-decline `ReplacementChoice`.
const LIBRARY_OF_LENG: &str = "You have no maximum hand size.\nIf an effect causes you to discard a card, discard it, but you may put it on top of your library instead of into your graveyard.";

/// Synthetic twin of Knowledge Vault's second ability with a plain draw member,
/// so the inherited "If you do" gate is observed without linked-exile tracking.
const TWIN_WHOLE: &str =
    "{0}: Sacrifice this artifact. If you do, discard your hand, then draw two cards.";

/// Synthetic twin of Sensation Gorger's "If you do, each player discards their
/// hand, then draws four cards" behind an activation: one inherited gate over
/// two multi-player actions — every player discards, then every player draws
/// (CR 608.2e).
const TWIN_EACH: &str = "{0}: Sacrifice this artifact. If you do, each player discards their hand, then draws two cards.";

/// Synthetic "you may discard your hand. When you do, …" — no printed card has
/// this exact shape; it isolates the reflexive half from the "If you do" half.
const REFLEXIVE_TWIN: &str =
    "Whenever you cast a spell, you may discard your hand. When you do, you gain 3 life.";

const TEST_SPELL: &str = "You gain 1 life.";

fn cast(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from hand");
}

fn activate(runner: &mut GameRunner, source_id: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the ability");
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

fn zone(runner: &GameRunner, object: ObjectId) -> Zone {
    runner.state().objects[&object].zone
}

/// The names of the cards in `player`'s `zone`, sorted so a comparison checks
/// exactly which cards are there regardless of the order they arrived in.
fn sorted_names(runner: &GameRunner, player: PlayerId, zone: Zone) -> Vec<String> {
    let state = runner.state();
    let player_state = &state.players[player.0 as usize];
    let ids: Vec<ObjectId> = match zone {
        Zone::Hand => player_state.hand.iter().copied().collect(),
        Zone::Graveyard => player_state.graveyard.iter().copied().collect(),
        Zone::Library => player_state.library.iter().copied().collect(),
        other => panic!("sorted_names does not read {other:?}"),
    };
    let mut names: Vec<String> = ids
        .iter()
        .map(|id| state.objects[id].name.clone())
        .collect();
    names.sort();
    names
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
    names.sort();
    names
}

/// `count` library names "`prefix` Lib 0", "`prefix` Lib 1", …, top first.
fn library_names(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|n| format!("{prefix} Lib {n}")).collect()
}

fn with_named_library(scenario: &mut GameScenario, player: PlayerId, names: &[String]) {
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    scenario.with_library_top(player, &names);
}

fn colorless() -> ManaUnit {
    ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])
}

fn assert_optional_prompt(runner: &GameRunner, player: PlayerId, why: &str) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: p, .. } if p == player
        ),
        "{why}: expected an OptionalEffectChoice for {player:?}, got {:?}",
        runner.state().waiting_for
    );
}

fn assert_settled_priority(runner: &GameRunner, why: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{why}: expected Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "{why}: the stack must be empty"
    );
}

fn assert_zero_unimplemented(oracle: &str, name: &str, types: &[&str]) {
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

/// The index of the source's activated ability whose root effect sacrifices.
fn sacrifice_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::Sacrifice { .. }))
        .expect("the artifact has a sacrifice ability")
}

// ---------------------------------------------------------------------------
// Moonveil Regent — optional "discard your hand. If you do, …"
// ---------------------------------------------------------------------------

/// Moonveil Regent under P0 beside a blue-red zero-cost spell, `extras` other
/// cards in hand and four cards to draw.
fn moonveil_board(extras: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Moonveil Regent", 4, 4, MOONVEIL_REGENT);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Insight", true, TEST_SPELL)
        .with_mana_cost(ManaCost::generic(0))
        .with_color(vec![ManaColor::Blue, ManaColor::Red])
        .id();
    let others = (0..extras)
        .map(|n| scenario.add_card_to_hand(P0, &format!("Other Card {n}")))
        .collect();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    (scenario.build(), spell, others)
}

/// Casts the Moonveil spell and answers the Regent's "you may discard your hand".
fn moonveil_cast_and_decide(runner: &mut GameRunner, spell: ObjectId, accept: bool) {
    cast(runner, spell);
    runner.advance_until_stack_empty();
    assert_optional_prompt(runner, P0, "the Regent offers its discard");
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("answer the Regent's discard");
    runner.advance_until_stack_empty();
    assert_settled_priority(runner, "the trigger and the spell resolved");
}

/// CR 118.12: accepting "discard your hand" with an empty hand pays it in full,
/// so P0 draws a card for each of the blue-red spell's two colors.
#[test]
fn moonveil_regent_empty_hand_discard_draws_for_each_color() {
    let (mut runner, spell, _) = moonveil_board(0);
    let life_before = runner.life(P0);
    let library_before = library_len(&runner, P0);

    moonveil_cast_and_decide(&mut runner, spell, true);
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "reach: the spell resolved"
    );
    assert_eq!(
        library_len(&runner, P0),
        library_before - 2,
        "the If you do drew one card per color of the spell"
    );
    assert_eq!(hand_len(&runner, P0), 2);
}

/// Declining the discard draws nothing — the "If you do" is not unconditional.
#[test]
fn moonveil_regent_declined_discard_draws_nothing() {
    let (mut runner, spell, _) = moonveil_board(0);
    let life_before = runner.life(P0);
    let library_before = library_len(&runner, P0);

    moonveil_cast_and_decide(&mut runner, spell, false);
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "reach: the spell resolved"
    );
    assert_eq!(library_len(&runner, P0), library_before, "nothing drawn");
    assert_eq!(hand_len(&runner, P0), 0);
}

/// A full hand is discarded and the two cards drawn.
#[test]
fn moonveil_regent_full_hand_discards_it_and_draws() {
    let (mut runner, spell, others) = moonveil_board(2);
    let library_before = library_len(&runner, P0);

    moonveil_cast_and_decide(&mut runner, spell, true);
    for other in &others {
        assert_eq!(
            zone(&runner, *other),
            Zone::Graveyard,
            "the hand was discarded"
        );
    }
    assert_eq!(library_len(&runner, P0), library_before - 2);
    assert_eq!(hand_len(&runner, P0), 2);
}

// ---------------------------------------------------------------------------
// Inherited "If you do" gate after a mandatory whole-hand discard
// ---------------------------------------------------------------------------

/// Knowledge Vault with one card exiled by its first ability, P0 holding
/// `extras` other cards.
fn knowledge_vault_with_exiled_card(
    extras: usize,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vault = scenario
        .add_artifact_from_oracle(P0, "Knowledge Vault", KNOWLEDGE_VAULT)
        .id();
    let others = (0..extras)
        .map(|n| scenario.add_card_to_hand(P0, &format!("Other Card {n}")))
        .collect();
    scenario.with_library_top(P0, &["Vaulted Card", "Card B"]);
    scenario.with_mana_pool(P0, vec![colorless(), colorless()]);
    let mut runner = scenario.build();
    let vaulted = runner.state().players[0].library[0];
    assert_eq!(
        runner.state().objects[&vaulted].name,
        "Vaulted Card",
        "test precondition: the library top"
    );

    let exile_index = 1 - sacrifice_ability_index(&runner, vault);
    activate(&mut runner, vault, exile_index);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the exile ability resolved");
    assert_eq!(
        zone(&runner, vaulted),
        Zone::Exile,
        "reach: the Vault exiled the library top"
    );
    (runner, vault, vaulted, others)
}

/// CR 118.12 + Knowledge Vault ruling: with an empty hand the Vault's discard
/// requires zero cards, so its inherited "If you do" still returns the exiled
/// card to hand. Control: a held card is discarded and the exiled card returns.
#[test]
fn knowledge_vault_empty_hand_still_returns_exiled_cards() {
    let (mut runner, vault, vaulted, _) = knowledge_vault_with_exiled_card(0);
    let sacrifice_index = sacrifice_ability_index(&runner, vault);
    activate(&mut runner, vault, sacrifice_index);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the sacrifice ability resolved");
    assert_eq!(
        zone(&runner, vault),
        Zone::Graveyard,
        "reach: the Vault was sacrificed"
    );
    assert_eq!(
        zone(&runner, vaulted),
        Zone::Hand,
        "the exiled card returned to its owner's hand"
    );

    // Control: a held card is discarded, and the exiled card still returns.
    let (mut runner, vault, vaulted, others) = knowledge_vault_with_exiled_card(1);
    let sacrifice_index = sacrifice_ability_index(&runner, vault);
    activate(&mut runner, vault, sacrifice_index);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the control's sacrifice ability resolved");
    assert_eq!(
        zone(&runner, others[0]),
        Zone::Graveyard,
        "the hand was discarded"
    );
    assert_eq!(zone(&runner, vaulted), Zone::Hand);
}

/// CR 118.12 (synthetic twin): "Sacrifice this artifact. If you do, discard
/// your hand, then draw two cards." with an empty hand still draws two.
#[test]
fn sacrifice_if_you_do_discard_your_hand_empty_hand_still_draws() {
    assert_zero_unimplemented(TWIN_WHOLE, "Whole-Hand Twin", &["Artifact"]);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Whole-Hand Twin", TWIN_WHOLE)
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, P0);

    activate(&mut runner, artifact, 0);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the ability resolved");
    assert_eq!(
        zone(&runner, artifact),
        Zone::Graveyard,
        "reach: sacrificed"
    );
    assert_eq!(
        library_len(&runner, P0),
        library_before - 2,
        "drew two cards"
    );
    assert_eq!(hand_len(&runner, P0), 2);
}

/// CR 118.12 + CR 608.2e (synthetic twin of Sensation Gorger): the sacrifice is
/// the "If you do" antecedent, so the empty-handed seat still draws two; the
/// seat holding cards discards them; every seat draws only after every seat has
/// discarded.
#[test]
fn each_player_discards_their_hand_empty_seat_still_draws() {
    assert_zero_unimplemented(TWIN_EACH, "Each-Player Twin", &["Artifact"]);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Each-Player Twin", TWIN_EACH)
        .id();
    let p1_cards = [
        scenario.add_card_to_hand(P1, "P1 Card A"),
        scenario.add_card_to_hand(P1, "P1 Card B"),
    ];
    scenario.with_library_top(P0, &["P0 Draw A", "P0 Draw B", "P0 Draw C"]);
    scenario.with_library_top(P1, &["P1 Draw A", "P1 Draw B", "P1 Draw C"]);
    let mut runner = scenario.build();
    assert_eq!(
        hand_len(&runner, P0),
        0,
        "test precondition: P0 holds no card"
    );
    let p0_library_before = library_len(&runner, P0);
    let p1_library_before = library_len(&runner, P1);

    activate(&mut runner, artifact, 0);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the ability resolved");
    assert_eq!(
        zone(&runner, artifact),
        Zone::Graveyard,
        "reach: sacrificed"
    );
    assert_eq!(
        library_len(&runner, P0),
        p0_library_before - 2,
        "the empty-handed seat still drew two"
    );
    assert_eq!(hand_len(&runner, P0), 2);
    for card in p1_cards {
        assert_eq!(
            zone(&runner, card),
            Zone::Graveyard,
            "P1 discarded its hand"
        );
    }
    assert_eq!(
        library_len(&runner, P1),
        p1_library_before - 2,
        "P1 drew two"
    );
    assert_eq!(hand_len(&runner, P1), 2);
}

/// TWIN_EACH with P0 holding one card and P1 two, named library tops for both,
/// and Notion Thief under P1 when `notion_thief` is set.
fn each_player_twin_board(notion_thief: bool) -> Option<(GameRunner, ObjectId)> {
    let db = if notion_thief {
        Some(crate::support::shared_card_db()?)
    } else {
        None
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Each-Player Twin", TWIN_EACH)
        .id();
    scenario.add_card_to_hand(P0, "P0 Held");
    scenario.add_card_to_hand(P1, "P1 Held A");
    scenario.add_card_to_hand(P1, "P1 Held B");
    with_named_library(&mut scenario, P0, &library_names("P0", 6));
    with_named_library(&mut scenario, P1, &library_names("P1", 6));
    if let Some(db) = db {
        scenario.add_real_card(P1, "Notion Thief", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    if let Some(db) = db {
        engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    }
    Some((runner, artifact))
}

/// CR 608.2e (synthetic twin of Sensation Gorger): one resolution, two
/// multi-player actions. Every seat discards the hand it held, then every seat
/// draws two exactly once, so each hand is exactly its own two library tops and
/// no seat discards a card this effect drew.
#[test]
fn each_player_discards_their_hand_then_each_draws_once() {
    assert_zero_unimplemented(TWIN_EACH, "Each-Player Twin", &["Artifact"]);
    let (mut runner, artifact) = each_player_twin_board(false).expect("no card DB needed");
    let p0_library_before = library_len(&runner, P0);
    let p1_library_before = library_len(&runner, P1);

    activate(&mut runner, artifact, 0);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the ability resolved");
    assert_eq!(
        zone(&runner, artifact),
        Zone::Graveyard,
        "reach: sacrificed"
    );
    assert_eq!(
        sorted_names(&runner, P0, Zone::Hand),
        sorted(&["P0 Lib 0", "P0 Lib 1"]),
        "P0's hand is exactly its two library tops"
    );
    assert_eq!(
        sorted_names(&runner, P1, Zone::Hand),
        sorted(&["P1 Lib 0", "P1 Lib 1"]),
        "P1's hand is exactly its two library tops"
    );
    assert_eq!(
        sorted_names(&runner, P0, Zone::Graveyard),
        sorted(&["Each-Player Twin", "P0 Held"]),
        "P0's graveyard holds only the sacrificed artifact and the card it held"
    );
    assert_eq!(
        sorted_names(&runner, P1, Zone::Graveyard),
        sorted(&["P1 Held A", "P1 Held B"]),
        "P1's graveyard holds only the cards it held before resolution"
    );
    assert_eq!(library_len(&runner, P0), p0_library_before - 2);
    assert_eq!(library_len(&runner, P1), p1_library_before - 2);
}

/// CR 608.2e + CR 614.1 + CR 118.12 (synthetic twin of Sensation Gorger, with
/// P1's discard paused): P1 controls Library of Leng, so each card P1 discards
/// raises an apply-or-decline replacement choice, which parks P1's whole-hand
/// discard mid-batch. P1 declines each one, so its cards go to its graveyard as
/// usual. The paused discard is still one action and the sacrifice is still the
/// "If you do" antecedent, so once every seat has discarded every seat draws
/// two exactly once: each hand is exactly its own two library tops.
#[test]
fn each_player_discards_then_each_draws_once_after_a_paused_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Each-Player Twin", TWIN_EACH)
        .id();
    let leng = scenario
        .add_artifact_from_oracle(P1, "Library of Leng", LIBRARY_OF_LENG)
        .id();
    scenario.add_card_to_hand(P0, "P0 Held");
    scenario.add_card_to_hand(P1, "P1 Held A");
    scenario.add_card_to_hand(P1, "P1 Held B");
    with_named_library(&mut scenario, P0, &library_names("P0", 6));
    with_named_library(&mut scenario, P1, &library_names("P1", 6));
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&leng].replacement_definitions.len(),
        1,
        "test precondition: Library of Leng's discard line is one replacement"
    );
    let p0_library_before = library_len(&runner, P0);
    let p1_library_before = library_len(&runner, P1);

    activate(&mut runner, artifact, 0);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().pending_discard_batch.is_some(),
        "reach: the replacement choice parked a whole-hand discard batch, got {:?}",
        runner.state().waiting_for
    );
    let mut prompts = 0;
    while let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    {
        assert_eq!(player, P1, "only Library of Leng's controller is asked");
        let decline = candidates
            .iter()
            .position(|candidate| candidate.description == "Decline")
            .expect("Library of Leng's choice offers Decline");
        runner
            .act(GameAction::ChooseReplacement { index: decline })
            .expect("decline Library of Leng");
        prompts += 1;
        assert!(prompts <= 2, "one choice per card P1 discards");
    }
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the ability resolved");
    assert_eq!(prompts, 2, "reach: each of P1's two discards was paused");
    assert_eq!(
        zone(&runner, artifact),
        Zone::Graveyard,
        "reach: sacrificed"
    );
    assert_eq!(
        sorted_names(&runner, P0, Zone::Hand),
        sorted(&["P0 Lib 0", "P0 Lib 1"]),
        "P0's hand is exactly its two library tops"
    );
    assert_eq!(
        sorted_names(&runner, P1, Zone::Hand),
        sorted(&["P1 Lib 0", "P1 Lib 1"]),
        "P1's hand is exactly its two library tops"
    );
    assert_eq!(
        sorted_names(&runner, P0, Zone::Graveyard),
        sorted(&["Each-Player Twin", "P0 Held"]),
        "P0's graveyard holds only the sacrificed artifact and the card it held"
    );
    assert_eq!(
        sorted_names(&runner, P1, Zone::Graveyard),
        sorted(&["P1 Held A", "P1 Held B"]),
        "P1's graveyard holds only the cards it held, declined onto it"
    );
    assert_eq!(library_len(&runner, P0), p0_library_before - 2);
    assert_eq!(library_len(&runner, P1), p1_library_before - 2);
}

/// CR 608.2e + CR 121.2c + CR 614.6: every seat's discard completes before any
/// seat draws. Notion Thief under P1 replaces each of P0's two draws with a
/// draw by P1, so P1 draws four after it has already discarded: P1's hand is
/// exactly its top four and its graveyard only the two cards it held. A
/// per-seat discard-then-draw would have P1 discard the cards Notion Thief drew.
#[test]
fn each_player_discard_completes_before_any_draw() {
    let Some((mut runner, artifact)) = each_player_twin_board(true) else {
        return;
    };
    let p0_library_before = library_len(&runner, P0);
    let p1_library_before = library_len(&runner, P1);

    activate(&mut runner, artifact, 0);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the ability resolved");
    assert_eq!(
        zone(&runner, artifact),
        Zone::Graveyard,
        "reach: sacrificed"
    );
    assert_eq!(
        sorted_names(&runner, P0, Zone::Graveyard),
        sorted(&["Each-Player Twin", "P0 Held"]),
        "reach: P0 discarded the card it held"
    );
    assert_eq!(
        hand_len(&runner, P0),
        0,
        "Notion Thief replaced both of P0's draws"
    );
    assert_eq!(
        library_len(&runner, P0),
        p0_library_before,
        "P0 drew no card"
    );
    assert_eq!(
        sorted_names(&runner, P1, Zone::Hand),
        sorted(&["P1 Lib 0", "P1 Lib 1", "P1 Lib 2", "P1 Lib 3"]),
        "P1 holds its own two draws plus the two Notion Thief redirected"
    );
    assert_eq!(
        sorted_names(&runner, P1, Zone::Graveyard),
        sorted(&["P1 Held A", "P1 Held B"]),
        "P1 discarded only the cards it held before any draw"
    );
    assert_eq!(library_len(&runner, P1), p1_library_before - 4);
}

/// Reference reading for the class (verbatim Wheel of Fortune, the ungated
/// twin): P0 and P1 discard, then P0's seven draws are replaced by Notion
/// Thief's controller's, then P1 draws seven — P0 holds nothing and P1 holds
/// fourteen, and P1's graveyard is only the cards it held.
#[test]
fn wheel_of_fortune_discards_before_draws_reference() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wheel = scenario
        .add_spell_to_hand_from_oracle(P0, "Wheel of Fortune", false, WHEEL_OF_FORTUNE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.add_card_to_hand(P0, "P0 Held");
    scenario.add_card_to_hand(P1, "P1 Held A");
    scenario.add_card_to_hand(P1, "P1 Held B");
    with_named_library(&mut scenario, P0, &library_names("P0", 20));
    with_named_library(&mut scenario, P1, &library_names("P1", 20));
    scenario.add_real_card(P1, "Notion Thief", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let p0_library_before = library_len(&runner, P0);
    let p1_library_before = library_len(&runner, P1);

    cast(&mut runner, wheel);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "Wheel of Fortune resolved");
    assert_eq!(
        zone(&runner, wheel),
        Zone::Graveyard,
        "reach: the Wheel resolved"
    );
    assert_eq!(hand_len(&runner, P0), 0);
    assert_eq!(library_len(&runner, P0), p0_library_before);
    assert_eq!(hand_len(&runner, P1), 14);
    assert_eq!(library_len(&runner, P1), p1_library_before - 14);
    assert_eq!(
        sorted_names(&runner, P1, Zone::Graveyard),
        sorted(&["P1 Held A", "P1 Held B"]),
    );
}

/// Sensation Gorger (verbatim Oracle) under P0, a Goblin on top of P0's library,
/// P0 holding `p0_held` cards and P1 two, advanced into P0's upkeep with the
/// Kinship trigger pending.
fn sensation_gorger_at_upkeep(p0_held: usize) -> Option<GameRunner> {
    let db = crate::support::shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario
        .add_creature_from_oracle(P0, "Sensation Gorger", 2, 2, SENSATION_GORGER)
        .with_subtypes(vec!["Goblin", "Shaman"]);
    for n in 0..p0_held {
        scenario.add_card_to_hand(P0, &format!("P0 Held {n}"));
    }
    scenario.add_card_to_hand(P1, "P1 Held A");
    scenario.add_card_to_hand(P1, "P1 Held B");
    with_named_library(&mut scenario, P0, &library_names("P0", 8));
    with_named_library(&mut scenario, P1, &library_names("P1", 8));
    let guide = scenario.add_real_card(P0, "Goblin Guide", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    {
        let library = &mut runner.state_mut().players[0].library;
        library.retain(|&id| id != guide);
        library.insert(0, guide);
    }
    assert_eq!(
        runner.state().players[0].library[0],
        guide,
        "test precondition: Goblin Guide is on top of P0's library"
    );
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep, "reach: P0's upkeep");
    Some(runner)
}

/// Answers every OptionalEffectChoice with `accept` and passes priority until
/// the stack is empty; returns how many optional prompts surfaced.
fn resolve_gorger_trigger(runner: &mut GameRunner, accept: bool) -> usize {
    let mut prompts = 0;
    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the optional effect");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && prompts > 0 => {
                break;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            ref other => panic!("unexpected prompt while resolving Kinship: {other:?}"),
        }
    }
    assert_settled_priority(runner, "the Kinship trigger resolved");
    // P0's draw-step card must not leak into the counts below.
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the trigger resolved within P0's upkeep"
    );
    prompts
}

/// CR 118.12 + CR 608.2e (printed card): revealing the Goblin is the "If you
/// do" antecedent, so every player discards their hand and then every player
/// draws four — once each, whether or not P0 held a card. Decline control: no
/// reveal, so nothing happens.
#[test]
fn sensation_gorger_each_player_discards_then_draws_four() {
    for p0_held in [1usize, 0] {
        let Some(mut runner) = sensation_gorger_at_upkeep(p0_held) else {
            return;
        };
        let p0_library_before = library_len(&runner, P0);
        let p1_library_before = library_len(&runner, P1);
        let prompts = resolve_gorger_trigger(&mut runner, true);
        assert!(prompts >= 1, "reach: Kinship offered its optional look");
        assert_eq!(
            sorted_names(&runner, P0, Zone::Hand),
            sorted(&["Goblin Guide", "P0 Lib 0", "P0 Lib 1", "P0 Lib 2"]),
            "P0 (holding {p0_held}) draws exactly four after discarding"
        );
        let p0_held_names: Vec<String> = (0..p0_held).map(|n| format!("P0 Held {n}")).collect();
        let p0_held_refs: Vec<&str> = p0_held_names.iter().map(String::as_str).collect();
        assert_eq!(
            sorted_names(&runner, P0, Zone::Graveyard),
            sorted(&p0_held_refs),
            "P0's graveyard holds only the cards it held"
        );
        assert_eq!(
            sorted_names(&runner, P1, Zone::Hand),
            sorted(&["P1 Lib 0", "P1 Lib 1", "P1 Lib 2", "P1 Lib 3"]),
            "P1 draws exactly four after discarding (P0 holding {p0_held})"
        );
        assert_eq!(
            sorted_names(&runner, P1, Zone::Graveyard),
            sorted(&["P1 Held A", "P1 Held B"]),
            "P1's graveyard holds only the cards it held"
        );
        assert_eq!(library_len(&runner, P0), p0_library_before - 4);
        assert_eq!(library_len(&runner, P1), p1_library_before - 4);
    }

    // Decline control: no look, no reveal, so the "If you do" is false.
    let Some(mut runner) = sensation_gorger_at_upkeep(1) else {
        return;
    };
    let guide = runner.state().players[0].library[0];
    let p0_library_before = library_len(&runner, P0);
    let p1_library_before = library_len(&runner, P1);
    let prompts = resolve_gorger_trigger(&mut runner, false);
    assert!(prompts >= 1, "reach: Kinship offered its optional look");
    assert_eq!(
        sorted_names(&runner, P0, Zone::Hand),
        sorted(&["P0 Held 0"])
    );
    assert_eq!(
        sorted_names(&runner, P1, Zone::Hand),
        sorted(&["P1 Held A", "P1 Held B"])
    );
    assert!(sorted_names(&runner, P0, Zone::Graveyard).is_empty());
    assert!(sorted_names(&runner, P1, Zone::Graveyard).is_empty());
    assert_eq!(library_len(&runner, P0), p0_library_before);
    assert_eq!(library_len(&runner, P1), p1_library_before);
    assert_eq!(
        runner.state().players[0].library[0],
        guide,
        "Goblin Guide is still on top of P0's library"
    );
}

// ---------------------------------------------------------------------------
// Numeric mandatory discard still vetoes
// ---------------------------------------------------------------------------

fn flush_out_board(extras: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Flush Out", false, FLUSH_OUT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let others = (0..extras)
        .map(|n| scenario.add_card_to_hand(P0, &format!("Other Card {n}")))
        .collect();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    (scenario.build(), spell, others)
}

/// CR 118.3: "Discard a card" requires a card, so with an empty hand it never
/// started and its "If you do" draws nothing. Control: a held card is
/// discarded and two cards drawn.
#[test]
fn flush_out_empty_hand_draws_nothing() {
    let (mut runner, spell, _) = flush_out_board(0);
    let library_before = library_len(&runner, P0);
    cast(&mut runner, spell);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "Flush Out resolved");
    assert_ne!(
        zone(&runner, spell),
        Zone::Stack,
        "reach: Flush Out left the stack"
    );
    assert_eq!(
        library_len(&runner, P0),
        library_before,
        "a discard that could not be made draws nothing"
    );

    // Control: a held card pays the discard.
    let (mut runner, spell, others) = flush_out_board(1);
    let library_before = library_len(&runner, P0);
    cast(&mut runner, spell);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the control's Flush Out resolved");
    assert_eq!(
        zone(&runner, others[0]),
        Zone::Graveyard,
        "the card was discarded"
    );
    assert_eq!(
        library_len(&runner, P0),
        library_before - 2,
        "drew two cards"
    );
}

// ---------------------------------------------------------------------------
// "When you do" needs a discarded card
// ---------------------------------------------------------------------------

fn reflexive_twin_board(extras: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Reflexive Twin", 2, 2, REFLEXIVE_TWIN);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Insight", true, TEST_SPELL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let others = (0..extras)
        .map(|n| scenario.add_card_to_hand(P0, &format!("Other Card {n}")))
        .collect();
    (scenario.build(), spell, others)
}

/// CR 603.12 + CR 701.9a (synthetic twin): accepting "discard your hand" with
/// an empty hand moves no card, so no discard event occurred and its "When you
/// do" does not trigger. Control: a held card is discarded and the reflexive
/// gains 3 life.
#[test]
fn you_may_discard_your_hand_when_you_do_needs_a_discarded_card() {
    assert_zero_unimplemented(REFLEXIVE_TWIN, "Reflexive Twin", &["Creature"]);
    let (mut runner, spell, _) = reflexive_twin_board(0);
    let life_before = runner.life(P0);
    moonveil_cast_and_decide(&mut runner, spell, true);
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "only the spell's life gain — the reflexive did not trigger"
    );

    // Control: a held card is discarded, so the reflexive triggers.
    let (mut runner, spell, others) = reflexive_twin_board(1);
    let life_before = runner.life(P0);
    moonveil_cast_and_decide(&mut runner, spell, true);
    assert_eq!(
        zone(&runner, others[0]),
        Zone::Graveyard,
        "the hand was discarded"
    );
    assert_eq!(runner.life(P0), life_before + 4, "spell 1 + reflexive 3");
}

// ---------------------------------------------------------------------------
// Miasma Demon — "you may discard any number of cards. When you do, …"
// ---------------------------------------------------------------------------

/// Miasma Demon in P0's hand at zero cost, `extras` other cards in P0's hand,
/// and an opposing 2/2. Casts the Demon and resolves it up to its ETB prompt.
fn miasma_demon_at_prompt(extras: usize) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let demon = scenario
        .add_creature_to_hand_from_oracle(P0, "Miasma Demon", 5, 4, MIASMA_DEMON)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let others = (0..extras)
        .map(|n| scenario.add_card_to_hand(P0, &format!("Other Card {n}")))
        .collect();
    let bear = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let mut runner = scenario.build();
    cast(&mut runner, demon);
    runner.advance_until_stack_empty();
    assert_eq!(
        zone(&runner, demon),
        Zone::Battlefield,
        "reach: the Demon resolved"
    );
    assert_optional_prompt(&runner, P0, "the Demon's ETB offers its discard");
    (runner, demon, bear, others)
}

/// CR 603.12 + CR 701.9a: accepting "discard any number of cards" with an empty
/// hand discards nothing, so the reflexive does not trigger: no target prompt,
/// no Demon-sourced ability on the stack, the 2/2 untouched. Control: a
/// discarded card lets the reflexive kill the 2/2.
#[test]
fn miasma_demon_empty_hand_discard_triggers_no_reflexive() {
    let (mut runner, demon, bear, _) = miasma_demon_at_prompt(0);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the Demon's discard");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
        ),
        "no reflexive target prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == demon),
        "no Demon-sourced ability on the stack"
    );
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the trigger resolved");
    assert_eq!(
        zone(&runner, bear),
        Zone::Battlefield,
        "the 2/2 is untouched"
    );

    // Positive control: one discarded card lets the reflexive target the 2/2.
    let (mut runner, _, bear, others) = miasma_demon_at_prompt(1);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the Demon's discard");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "\"any number\" offers the discard choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![others[0]],
        })
        .expect("discard the held card");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the reflexive asks for its target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("target the 2/2");
    runner.advance_until_stack_empty();
    assert_eq!(
        zone(&runner, others[0]),
        Zone::Graveyard,
        "the card was discarded"
    );
    assert_eq!(
        zone(&runner, bear),
        Zone::Graveyard,
        "the -2/-2 killed the 2/2"
    );
}
