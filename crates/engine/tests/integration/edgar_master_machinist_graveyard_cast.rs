//! Edgar, Master Machinist (#9770): "Once during each of your turns, you may
//! cast an artifact spell from your graveyard. If you cast a spell this way,
//! that artifact enters tapped."
//!
//! CR 601.3 + CR 607.1c: the permission and its linked rider are one ability.
//! CR 614.1d: the rider is a replacement effect ("[Objects] enter …"), offered
//! as a CR 616.1 candidate when the spell cast this way enters the battlefield.
//! Edgar ruling (2025-06-06): Edgar "doesn't need to stay under your control
//! throughout that process or be under your control as the spell resolves".
//!
//! Every test drives production `GameAction`s after `build()`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CardPlayMode, StaticCondition};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, CastingVariantChoiceOption, GameState, PersistedGameState,
    PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::{EtbTapState, Zone};

const EDGAR: &str = "Once during each of your turns, you may cast an artifact spell from your graveyard. If you cast a spell this way, that artifact enters tapped.\nTools — Whenever Edgar attacks, it gets +X/+0 until end of turn, where X is the greatest mana value among artifacts you control.";
const ARCHELOS: &str = "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.";
const MULDROTHA: &str = "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard. (If a card has multiple permanent types, choose one as you play it.)";
const ASHNODS_ALTAR: &str = "Sacrifice a creature: Add {C}{C}.";
const DRAFNA: &str = "{1}{U}: Return target artifact you control to its owner's hand.\n{3}, {T}: Copy target artifact spell you control. (The copy becomes a token.)";
const MURDER: &str = "Destroy target creature.";
const CANCEL: &str = "Counter target spell.";
const REMAND: &str = "Counter target spell. If that spell is countered this way, put it into its owner's hand instead of into that player's graveyard.\nDraw a card.";
const PHYREXIAN_METAMORPH: &str = "({U/P} can be paid with either {U} or 2 life.)\nYou may have this creature enter as a copy of any artifact or creature on the battlefield, except it's an artifact in addition to its other types.";

const EDGAR_NAME: &str = "Edgar, Master Machinist";

fn add_edgar(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, EDGAR_NAME, 2, 4, EDGAR)
        .id()
}

/// A zero-cost artifact creature card in P0's graveyard.
fn graveyard_artifact(
    scenario: &mut GameScenario,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, name, power, toughness)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn add_colorless(runner: &mut GameRunner, player: usize, count: usize) {
    let pool = &mut runner.state_mut().players[player].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Reach guard (C1 at runtime): Edgar's permission is the parser's
/// `GraveyardCastPermission { OncePerTurn, Cast, enter_tapped: Tapped }`,
/// limited to its controller's turns.
fn assert_edgar_permission(runner: &GameRunner, edgar: ObjectId) {
    let obj = &runner.state().objects[&edgar];
    let def = obj
        .static_definitions
        .iter_all()
        .find(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. }))
        .expect("Edgar carries a graveyard cast permission");
    assert!(
        matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: CardPlayMode::Cast,
                enter_tapped: EtbTapState::Tapped,
                enters_with_counter: None,
                ..
            }
        ),
        "Edgar's permission shape, got {:?}",
        def.mode
    );
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
}

fn start_cast(
    runner: &mut GameRunner,
    id: ObjectId,
    payment_mode: CastPaymentMode,
) -> Result<WaitingFor, String> {
    let card_id = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],
            payment_mode,
        })
        .map(|result| result.waiting_for)
        .map_err(|e| format!("{e:?}"))
}

fn authority_source(option: &CastingVariantChoiceOption) -> Option<ObjectId> {
    option
        .authority
        .as_ref()
        .map(|authority| authority.announcement.permission.source)
}

/// Cast `id` from the graveyard under `source`'s permission, answering the
/// announcement menu when there is one (and Muldrotha's per-type slot with
/// Artifact). Returns the authority the cast committed to.
fn cast_under(runner: &mut GameRunner, id: ObjectId, source: ObjectId) -> ObjectId {
    let mut waiting = start_cast(runner, id, CastPaymentMode::Auto).expect("the cast starts");
    for _ in 0..4 {
        match waiting {
            WaitingFor::CastingVariantChoice { options, .. } => {
                let index = options
                    .iter()
                    .position(|option| {
                        authority_source(option) == Some(source)
                            && matches!(option.variant, CastingVariant::GraveyardPermission { .. })
                            && option
                                .authority
                                .as_ref()
                                .and_then(|a| a.announcement.slot_type)
                                .is_none_or(|slot| slot == CoreType::Artifact)
                    })
                    .unwrap_or_else(|| panic!("the menu offers {source:?}, got {options:?}"));
                waiting = runner
                    .act(GameAction::ChooseCastingVariant { index })
                    .expect("choosing an offered announcement")
                    .waiting_for;
            }
            // CR 110.4: Muldrotha's slot for a multi-type card is chosen as
            // it is played.
            WaitingFor::ChoosePermanentTypeSlot { .. } => {
                waiting = runner
                    .act(GameAction::ChoosePermanentTypeSlot {
                        slot: CoreType::Artifact,
                    })
                    .expect("choosing the artifact slot")
                    .waiting_for;
            }
            // CR 601.2b: choose mana for each Phyrexian symbol.
            WaitingFor::PhyrexianPayment { shards, .. } => {
                waiting = runner
                    .act(GameAction::SubmitPhyrexianChoices {
                        choices: vec![
                            engine::types::game_state::ShardChoice::PayMana;
                            shards.len()
                        ],
                    })
                    .expect("paying the Phyrexian shard with mana")
                    .waiting_for;
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().objects[&id].zone,
        Zone::Stack,
        "the cast completes, waiting for {:?}",
        runner.state().waiting_for
    );
    selected_authority(runner, id)
}

/// The pending cast's announced graveyard permission source and the
/// enters-tapped term its latch carries (CR 601.2a: fixed as casting begins).
fn pending_permission(runner: &GameRunner) -> (Option<ObjectId>, Option<EtbTapState>) {
    let pending = runner
        .state()
        .pending_cast
        .as_ref()
        .expect("a cast is pending");
    let context = &pending.ability.context;
    let source = match context.graveyard_permission_authority {
        Some(CastingVariant::GraveyardPermission { source, .. }) => Some(source),
        _ => None,
    };
    let latch = context.graveyard_permission_latch.as_ref().map(|latch| {
        assert_eq!(
            Some(latch.permission.source),
            source,
            "the latch belongs to the announced permission"
        );
        latch.enter_tapped
    });
    (source, latch)
}

/// The graveyard permission source the spell on the stack was cast under.
fn selected_authority(runner: &GameRunner, id: ObjectId) -> ObjectId {
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == id)
        .expect("the spell is on the stack");
    match entry.kind {
        engine::types::game_state::StackEntryKind::Spell {
            casting_variant: CastingVariant::GraveyardPermission { source, .. },
            ..
        } => source,
        ref other => panic!("cast under a graveyard permission, got {other:?}"),
    }
}

/// Pass priority until the stack is empty. Any non-priority prompt fails the
/// test: the boards that use this have no choice to make.
fn resolve_all(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

/// Pass priority until `player` holds it, without resolving anything.
fn give_priority_to(runner: &mut GameRunner, player: engine::types::player::PlayerId) {
    for _ in 0..runner.state().players.len() {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        let depth = runner.state().stack.len();
        runner.act(GameAction::PassPriority).expect("pass priority");
        assert_eq!(runner.state().stack.len(), depth, "a pass doesn't resolve");
    }
    panic!("priority never reached {player:?}");
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

fn carrier(runner: &GameRunner, id: ObjectId) -> EtbTapState {
    runner.state().objects[&id].cast_permission_enter_tapped
}

fn edgar_slot_used(runner: &GameRunner, edgar: ObjectId) -> bool {
    runner
        .state()
        .graveyard_cast_permissions_used
        .contains(&edgar)
}

fn round_trip(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    restore_json(&json)
}

fn restore_json(json: &str) -> GameRunner {
    let state = serde_json::from_str::<PersistedGameState>(json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

// --- C7: the issue repro -----------------------------------------------------

/// C7: the first graveyard artifact cast through Edgar enters tapped and spends
/// the once-per-turn slot; a second graveyard artifact cast that turn is
/// refused. Edgar 2025-06-06 ruling: costs and timing are the card's own.
#[test]
fn edgar_graveyard_artifact_enters_tapped_once_per_turn() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let first = graveyard_artifact(&mut s, "Ornithopter", 0, 2);
    let second = graveyard_artifact(&mut s, "Memnite", 1, 1);
    let mut r = s.build();
    assert_edgar_permission(&r, edgar);

    let waiting = start_cast(&mut r, first, CastPaymentMode::Auto).expect("the first cast starts");
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "one permission, no announcement menu: {waiting:?}"
    );
    assert_eq!(selected_authority(&r, first), edgar);
    assert_eq!(
        carrier(&r, first),
        EtbTapState::Tapped,
        "the rider rides the spell"
    );
    resolve_all(&mut r);
    assert_eq!(r.state().objects[&first].zone, Zone::Battlefield);
    assert!(tapped(&r, first), "CR 614.1d: that artifact enters tapped");
    assert_eq!(
        carrier(&r, first),
        EtbTapState::Unspecified,
        "the carrier ends with the spell"
    );
    assert!(edgar_slot_used(&r, edgar));

    assert!(
        start_cast(&mut r, second, CastPaymentMode::Auto).is_err(),
        "CR 601.3: once during each of your turns"
    );
    assert_eq!(r.state().objects[&second].zone, Zone::Graveyard);
}

// --- C8 / C9: CR 616.1 ordering against Archelos ---------------------------

/// Edgar, Archelos (`archelos_tapped`) and Ornithopter in the graveyard;
/// Ornithopter is cast through Edgar and is on the stack.
fn archelos_board(with_archelos: bool, archelos_tapped: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let archelos = with_archelos.then(|| {
        s.add_creature_from_oracle(P0, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS)
            .id()
    });
    let orni = graveyard_artifact(&mut s, "Ornithopter", 0, 2);
    let mut r = s.build();
    if let (Some(archelos), true) = (archelos, archelos_tapped) {
        r.state_mut().objects.get_mut(&archelos).unwrap().tapped = true;
    }
    assert_edgar_permission(&r, edgar);
    assert_eq!(cast_under(&mut r, orni, edgar), edgar);
    (r, edgar, orni)
}

/// Pass until the entry's CR 616.1 ordering prompt; return the index of the
/// candidate whose source is `first`.
fn ordering_index(runner: &mut GameRunner, first: &str) -> usize {
    for _ in 0..8 {
        if let WaitingFor::ReplacementChoice {
            player,
            candidates,
            candidate_count: 2,
            ..
        } = &runner.state().waiting_for
        {
            assert_eq!(
                *player, P0,
                "CR 616.1: the entering object's controller orders"
            );
            let names: Vec<_> = candidates
                .iter()
                .map(|c| (c.source_name.clone(), c.description.clone()))
                .collect();
            assert!(
                names.contains(&("Ornithopter".to_string(), "Enters tapped".to_string())),
                "Edgar's rider is a candidate, got {names:?}"
            );
            assert!(
                names
                    .iter()
                    .any(|(source, _)| source == "Archelos, Lagoon Mystic"),
                "Archelos is a candidate, got {names:?}"
            );
            let source = if first == "Edgar" {
                "Ornithopter"
            } else {
                first
            };
            return candidates
                .iter()
                .position(|c| c.source_name == source)
                .expect("candidate present");
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!(
        "expected the entry ordering prompt, got {:?}",
        runner.state().waiting_for
    );
}

/// C8: CR 616.1e/f. With Archelos untapped, Edgar's "enters tapped" and
/// Archelos's "enter untapped" both apply to Ornithopter's entry and the last
/// applied decides: Edgar then Archelos → untapped; Archelos then Edgar →
/// tapped. A seed (pre-applying Edgar's) would give "tapped" for order A.
#[test]
fn edgar_rider_orders_against_archelos() {
    for (first, expect_tapped) in [("Edgar", false), ("Archelos, Lagoon Mystic", true)] {
        let (mut r, _edgar, orni) = archelos_board(true, false);
        let index = ordering_index(&mut r, first);
        r.act(GameAction::ChooseReplacement { index })
            .expect("choose the first effect");
        resolve_all(&mut r);
        assert_eq!(r.state().objects[&orni].zone, Zone::Battlefield);
        assert_eq!(
            tapped(&r, orni),
            expect_tapped,
            "{first} applied first; the other effect's write is last"
        );
    }
}

/// C8 controls: without Archelos there is nothing to order and Ornithopter
/// enters tapped; with Archelos tapped both effects say "tapped", so the order
/// is immaterial (no prompt) and it enters tapped.
#[test]
fn edgar_rider_without_a_competitor_needs_no_ordering() {
    for (with_archelos, archelos_tapped) in [(false, false), (true, true)] {
        let (mut r, _edgar, orni) = archelos_board(with_archelos, archelos_tapped);
        resolve_all(&mut r);
        assert!(
            tapped(&r, orni),
            "enters tapped (archelos {with_archelos}, tapped {archelos_tapped})"
        );
    }
}

/// C9: the parked ordering survives a save and restore through the production
/// persistence pipeline; answering after the restore gives C8's results. The
/// spell is no longer a stack entry while parked, but is still in the Stack
/// zone carrying the rider.
#[test]
fn edgar_rider_ordering_survives_save_and_restore() {
    for (first, expect_tapped) in [("Edgar", false), ("Archelos, Lagoon Mystic", true)] {
        let (mut r, _edgar, orni) = archelos_board(true, false);
        let _ = ordering_index(&mut r, first);
        assert!(
            r.state().stack.iter().all(|entry| entry.id != orni),
            "reach guard: the parked spell's entry was popped"
        );
        assert_eq!(r.state().objects[&orni].zone, Zone::Stack);
        let mut restored = round_trip(&r);
        assert_eq!(carrier(&restored, orni), EtbTapState::Tapped);
        let index = ordering_index(&mut restored, first);
        restored
            .act(GameAction::ChooseReplacement { index })
            .expect("answer after restore");
        resolve_all(&mut restored);
        assert_eq!(tapped(&restored, orni), expect_tapped, "{first} first");
    }
}

// --- C10 / C11: the source leaves (Edgar 2025-06-06 ruling) ----------------

/// C10: Edgar is sacrificed to Ashnod's Altar to pay for Myr Retriever ({2})
/// during the cast. The rider was latched at announcement, so Retriever still
/// enters tapped. Control: paid from the pool, Edgar stays, also tapped.
#[test]
fn edgar_sacrificed_mid_payment_still_enters_tapped() {
    for sacrifice_edgar in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let edgar = add_edgar(&mut s);
        let altar = s
            .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
            .id();
        let retriever = s
            .add_creature_to_graveyard(P0, "Myr Retriever", 1, 1)
            .as_artifact_creature()
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mut r = s.build();
        assert_edgar_permission(&r, edgar);
        if !sacrifice_edgar {
            add_colorless(&mut r, 0, 2);
        }
        let waiting =
            start_cast(&mut r, retriever, CastPaymentMode::Manual).expect("the cast starts");
        if sacrifice_edgar {
            assert!(
                matches!(waiting, WaitingFor::ManaPayment { .. }),
                "manual payment, got {waiting:?}"
            );
            r.act(GameAction::ActivateAbility {
                source_id: altar,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("Ashnod's Altar during payment");
            r.act(GameAction::SelectCards { cards: vec![edgar] })
                .expect("sacrifice Edgar");
            assert_eq!(r.state().objects[&edgar].zone, Zone::Graveyard);
        }
        for _ in 0..4 {
            if matches!(r.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                r.act(GameAction::PassPriority).expect("commit the payment");
            }
        }
        assert_eq!(r.state().objects[&retriever].zone, Zone::Stack);
        assert_eq!(selected_authority(&r, retriever), edgar);
        resolve_all(&mut r);
        assert_eq!(r.state().objects[&retriever].zone, Zone::Battlefield);
        assert!(
            tapped(&r, retriever),
            "enters tapped (Edgar sacrificed: {sacrifice_edgar})"
        );
    }
}

/// C11: P1 destroys Edgar while the artifact is on the stack. Edgar is gone
/// before the spell resolves; the artifact still enters tapped.
#[test]
fn edgar_destroyed_in_response_still_enters_tapped() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let orni = graveyard_artifact(&mut s, "Ornithopter", 0, 2);
    let murder = s
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .id();
    let mut r = s.build();
    assert_edgar_permission(&r, edgar);
    assert_eq!(cast_under(&mut r, orni, edgar), edgar);
    give_priority_to(&mut r, P1);
    let _ = r.cast(murder).target_objects(&[edgar]).commit();
    assert_eq!(r.state().objects[&murder].zone, Zone::Stack);
    r.resolve_top();
    assert_eq!(
        r.state().objects[&edgar].zone,
        Zone::Graveyard,
        "reach guard: Edgar is gone before the artifact resolves"
    );
    assert_eq!(r.state().objects[&orni].zone, Zone::Stack);
    resolve_all(&mut r);
    assert_eq!(r.state().objects[&orni].zone, Zone::Battlefield);
    assert!(tapped(&r, orni));
}

// --- C12: another graveyard authority ---------------------------------------

/// C12: CR 601.2a. With Edgar and Muldrotha both able to authorize the cast,
/// the first artifact is announced under Muldrotha: it enters untapped and
/// Edgar's slot stays unused. The second is announced under Edgar and enters
/// tapped.
#[test]
fn muldrotha_cast_is_untapped_and_leaves_edgars_slot() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let muldrotha = s
        .add_creature_from_oracle(P0, "Muldrotha, the Gravetide", 6, 6, MULDROTHA)
        .id();
    let first = graveyard_artifact(&mut s, "Ornithopter", 0, 2);
    let second = graveyard_artifact(&mut s, "Memnite", 1, 1);
    let mut r = s.build();
    assert_edgar_permission(&r, edgar);

    assert_eq!(cast_under(&mut r, first, muldrotha), muldrotha);
    assert_eq!(carrier(&r, first), EtbTapState::Unspecified);
    resolve_all(&mut r);
    assert_eq!(r.state().objects[&first].zone, Zone::Battlefield);
    assert!(!tapped(&r, first), "Muldrotha's permission has no rider");
    assert!(!edgar_slot_used(&r, edgar), "Edgar's slot is unused");

    assert_eq!(cast_under(&mut r, second, edgar), edgar);
    resolve_all(&mut r);
    assert_eq!(r.state().objects[&second].zone, Zone::Battlefield);
    assert!(tapped(&r, second), "Edgar's announcement brings its rider");
}

// --- C13 / C24: a copy wasn't cast (CR 707.10) -------------------------------

/// C13: Drafna copies the Edgar-cast Ornithopter. The copy wasn't cast, so it
/// wasn't cast "this way": the token enters untapped, the original tapped.
#[test]
fn drafna_copy_of_edgar_cast_artifact_enters_untapped() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let drafna = s
        .add_creature_from_oracle(P0, "Drafna, Founder of Lat-Nam", 1, 4, DRAFNA)
        .id();
    let orni = graveyard_artifact(&mut s, "Ornithopter", 0, 2);
    let mut r = s.build();
    assert_edgar_permission(&r, edgar);
    assert_eq!(cast_under(&mut r, orni, edgar), edgar);
    add_colorless(&mut r, 0, 3);
    r.act(GameAction::ActivateAbility {
        source_id: drafna,
        ability_index: 1,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("Drafna's copy ability");
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        r.choose_first_legal_target()
            .expect("target the artifact spell");
    }
    let ability = r.state().stack.last().expect("Drafna's ability").id;
    assert_ne!(
        ability, orni,
        "reach guard: the copy ability is on the stack"
    );
    for _ in 0..4 {
        if r.state().stack.iter().all(|entry| entry.id != ability) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass priority");
    }
    let copy = r
        .state()
        .stack
        .iter()
        .map(|entry| entry.id)
        .find(|id| *id != orni)
        .expect("the copy is on the stack");
    assert!(
        r.state().objects[&copy].is_token
            && r.state().objects[&copy].card_id == r.state().objects[&orni].card_id,
        "reach guard: the new spell is a token copy of Ornithopter (its stack entry \
         inherits the original's casting variant, so only the object's own rider \
         field can tell them apart)"
    );
    assert_eq!(
        carrier(&r, copy),
        EtbTapState::Unspecified,
        "C24: the copy carries no rider when its push is recorded"
    );
    assert_eq!(carrier(&r, orni), EtbTapState::Tapped);
    resolve_all(&mut r);
    assert!(r.state().objects[&copy].is_token, "the copy became a token");
    assert!(
        !tapped(&r, copy),
        "CR 707.10: the copy wasn't cast this way"
    );
    assert!(tapped(&r, orni), "the original enters tapped");
}

// --- C14: scope -----------------------------------------------------------------

/// C14a: the same kind of artifact cast from hand with Edgar out enters
/// untapped and spends nothing (CR 110.5b).
#[test]
fn edgar_hand_cast_enters_untapped() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let orni = s
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    assert_edgar_permission(&r, edgar);
    start_cast(&mut r, orni, CastPaymentMode::Auto).expect("hand cast");
    assert_eq!(carrier(&r, orni), EtbTapState::Unspecified);
    resolve_all(&mut r);
    assert_eq!(r.state().objects[&orni].zone, Zone::Battlefield);
    assert!(!tapped(&r, orni));
    assert!(!edgar_slot_used(&r, edgar));
}

/// Shimmer Myr: an artifact with flash that lets artifact spells be cast as
/// though they had flash, so an opponent-turn refusal can't come from sorcery
/// timing.
const SHIMMER_MYR: &str = "Flash\nYou may cast artifact spells as though they had flash.";

/// Drive P0's turn to P1's upkeep through public actions (no attackers, no
/// blockers), then hand P0 priority there.
fn to_opponents_upkeep_with_priority(r: &mut GameRunner) {
    let turn = r.state().turn_number;
    for _ in 0..200 {
        if r.state().turn_number == turn + 1 && r.state().phase == Phase::Upkeep {
            break;
        }
        let action = match &r.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: Vec::new(),
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
            other => panic!("unexpected prompt while driving to P1's turn: {other:?}"),
        };
        r.act(action).expect("driver action accepted");
    }
    assert_eq!(r.state().active_player, P1, "reach guard: P1's turn");
    assert_eq!(r.state().phase, Phase::Upkeep);
    give_priority_to(r, P0);
}

/// C14b (Q3): "Once during each of your turns" (CR 102.1 + the parsed
/// `DuringYourTurn`). Shimmer Myr has flash, so on P1's turn only Edgar's turn
/// restriction can refuse the graveyard cast. Controls: (a) the same graveyard
/// cast on P0's own turn succeeds; (b) the same card from hand at the same
/// opponent-turn window succeeds (flash); (c) with only Edgar's
/// `DuringYourTurn` removed, the opponent-turn graveyard cast succeeds.
#[test]
fn edgar_permission_blocked_on_opponents_turn() {
    let board = |zone: Zone| {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let edgar = add_edgar(&mut s);
        let myr = match zone {
            Zone::Graveyard => s.add_creature_to_graveyard(P0, "Shimmer Myr", 2, 2),
            _ => s.add_creature_to_hand(P0, "Shimmer Myr", 2, 2),
        }
        .as_artifact_creature()
        .from_oracle_text(SHIMMER_MYR)
        .with_mana_cost(ManaCost::generic(3))
        .id();
        let r = s.build();
        assert_edgar_permission(&r, edgar);
        (r, edgar, myr)
    };

    // (a) own turn: Edgar authorizes the graveyard cast.
    let (mut r, edgar, myr) = board(Zone::Graveyard);
    add_colorless(&mut r, 0, 3);
    start_cast(&mut r, myr, CastPaymentMode::Auto).expect("own-turn graveyard cast");
    assert_eq!(selected_authority(&r, myr), edgar);

    // The negative: P1's turn, P0 holds priority, Edgar's slot unused.
    let (mut r, edgar, myr) = board(Zone::Graveyard);
    to_opponents_upkeep_with_priority(&mut r);
    assert!(
        !edgar_slot_used(&r, edgar),
        "reach guard: the slot is unused"
    );
    add_colorless(&mut r, 0, 3);
    assert!(
        start_cast(&mut r, myr, CastPaymentMode::Auto).is_err(),
        "Edgar's permission works only during its controller's turns"
    );
    assert_eq!(r.state().objects[&myr].zone, Zone::Graveyard);

    // (b) the same window admits the card from hand (it has flash).
    let (mut r, _edgar, myr) = board(Zone::Hand);
    to_opponents_upkeep_with_priority(&mut r);
    add_colorless(&mut r, 0, 3);
    start_cast(&mut r, myr, CastPaymentMode::Auto).expect("flash cast from hand");
    assert_eq!(r.state().objects[&myr].zone, Zone::Stack);

    // (c) mutation: only Edgar's DuringYourTurn removed → the graveyard cast
    // is admitted in the same window, so the refusal above is the condition.
    let (mut r, edgar, myr) = board(Zone::Graveyard);
    to_opponents_upkeep_with_priority(&mut r);
    {
        let obj = r.state_mut().objects.get_mut(&edgar).unwrap();
        let without_turn_gate = |def: &mut engine::types::ability::StaticDefinition| {
            if matches!(def.mode, StaticMode::GraveyardCastPermission { .. }) {
                def.condition = None;
            }
        };
        for index in 0..obj.static_definitions.len() {
            if let Some(def) = obj.static_definitions.get_mut(index) {
                without_turn_gate(def);
            }
        }
        std::sync::Arc::make_mut(&mut obj.base_static_definitions)
            .iter_mut()
            .for_each(without_turn_gate);
    }
    add_colorless(&mut r, 0, 3);
    start_cast(&mut r, myr, CastPaymentMode::Auto)
        .expect("without DuringYourTurn the opponent-turn graveyard cast is admitted");
    assert_eq!(selected_authority(&r, myr), edgar);
}

// --- C15 / C22: the carrier ends when the spell leaves the stack ------------

/// C15 (P5): P1 counters the Edgar-cast Ornithopter with Cancel. Right after
/// the counter, Ornithopter is in the graveyard with no carrier (CR 400.7).
/// It is then recast the same turn through Muldrotha (Edgar's slot is spent),
/// and enters untapped.
#[test]
fn countered_edgar_cast_drops_the_rider() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let muldrotha = s
        .add_creature_from_oracle(P0, "Muldrotha, the Gravetide", 6, 6, MULDROTHA)
        .id();
    let orni = graveyard_artifact(&mut s, "Ornithopter", 0, 2);
    let cancel = s
        .add_spell_to_hand_from_oracle(P1, "Cancel", true, CANCEL)
        .id();
    let mut r = s.build();
    assert_edgar_permission(&r, edgar);
    assert_eq!(cast_under(&mut r, orni, edgar), edgar);
    assert_eq!(carrier(&r, orni), EtbTapState::Tapped, "reach guard");
    give_priority_to(&mut r, P1);
    let _ = r.cast(cancel).target_objects(&[orni]).commit();
    r.resolve_top();
    assert_eq!(r.state().objects[&orni].zone, Zone::Graveyard);
    assert_eq!(
        carrier(&r, orni),
        EtbTapState::Unspecified,
        "the countered spell's carrier is cleared at the Stack exit"
    );
    assert!(edgar_slot_used(&r, edgar));

    assert_eq!(cast_under(&mut r, orni, muldrotha), muldrotha);
    resolve_all(&mut r);
    assert_eq!(r.state().objects[&orni].zone, Zone::Battlefield);
    assert!(!tapped(&r, orni));
}

/// C22: Remand returns the Edgar-cast Ornithopter to hand; the carrier is
/// cleared there, and a hand recast enters untapped.
#[test]
fn remanded_edgar_cast_recast_from_hand_enters_untapped() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let edgar = add_edgar(&mut s);
    let orni = graveyard_artifact(&mut s, "Ornithopter", 0, 2);
    let remand = s
        .add_spell_to_hand_from_oracle(P1, "Remand", true, REMAND)
        .id();
    s.with_library_top(P1, &["P1 Draw"]);
    let mut r = s.build();
    assert_edgar_permission(&r, edgar);
    assert_eq!(cast_under(&mut r, orni, edgar), edgar);
    give_priority_to(&mut r, P1);
    let _ = r.cast(remand).target_objects(&[orni]).commit();
    r.resolve_top();
    assert_eq!(r.state().objects[&orni].zone, Zone::Hand);
    assert_eq!(carrier(&r, orni), EtbTapState::Unspecified);
    start_cast(&mut r, orni, CastPaymentMode::Auto).expect("hand recast");
    resolve_all(&mut r);
    assert_eq!(r.state().objects[&orni].zone, Zone::Battlefield);
    assert!(!tapped(&r, orni));
}

// --- C19 (Q2): an original that enters as a copy -------------------------------

const GLORIOUS_ANTHEM: &str = "Creatures you control get +1/+1.";

/// Resolve the stack, answering Phyrexian Metamorph's entry explicitly: the
/// "you may have it enter as a copy" choice is accepted with `copy` (or
/// declined with `None`). Returns the prompts seen, in order.
fn resolve_metamorph_entry(r: &mut GameRunner, copy: Option<ObjectId>) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..24 {
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return seen,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                seen.push(format!("CopyTargetChoice({valid_targets:?})"));
                if let Some(model) = copy {
                    assert!(valid_targets.contains(&model), "the model is offered");
                }
                r.act(GameAction::ChooseTarget {
                    target: copy.map(engine::types::ability::TargetRef::Object),
                })
                .expect("answer the copy choice");
            }
            WaitingFor::ReplacementChoice {
                candidates, kind, ..
            } => {
                seen.push(format!(
                    "ReplacementChoice({kind:?}: {:?})",
                    candidates
                        .iter()
                        .map(|c| format!("{}: {}", c.source_name, c.description))
                        .collect::<Vec<_>>()
                ));
                let index = if copy.is_some() { 0 } else { 1 };
                r.act(GameAction::ChooseReplacement { index })
                    .expect("answer the optional copy");
            }
            other => panic!("unexpected prompt {other:?} after {seen:?}"),
        }
    }
    panic!("the stack never emptied after {seen:?}");
}

/// C19 (Q2): Phyrexian Metamorph cast through Edgar for its printed
/// {3}{U/P}, on a board with Glorious Anthem (so a declined 0/0 Metamorph
/// survives). Accepting the copy of Ornithopter (CR 616.1c applies the copy
/// first) or declining it, it enters tapped, is still a nontoken card, its
/// carrier is cleared, and the game is back at Priority with an empty stack.
#[test]
fn edgar_metamorph_enters_tapped_whether_or_not_it_copies() {
    for accept in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let edgar = add_edgar(&mut s);
        s.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM);
        let model = s
            .add_creature(P0, "Ornithopter", 0, 2)
            .as_artifact_creature()
            .id();
        let metamorph = s
            .add_creature_to_graveyard(P0, "Phyrexian Metamorph", 0, 0)
            .as_artifact_creature()
            .from_oracle_text(PHYREXIAN_METAMORPH)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![engine::types::mana::ManaCostShard::PhyrexianBlue],
            })
            .id();
        let mut r = s.build();
        assert_edgar_permission(&r, edgar);
        add_colorless(&mut r, 0, 3);
        r.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(0),
            false,
            vec![],
        ));
        assert_eq!(cast_under(&mut r, metamorph, edgar), edgar);
        assert_eq!(
            r.state().players[0].mana_pool.total(),
            0,
            "the printed {{3}}{{U/P}} was paid"
        );
        let seen = resolve_metamorph_entry(&mut r, accept.then_some(model));
        let optional = seen
            .first()
            .unwrap_or_else(|| panic!("the optional copy prompt (accept {accept})"));
        assert!(
            optional.starts_with("ReplacementChoice(OptionalBranch")
                && optional.contains("Phyrexian Metamorph: You may have ~ enter as a copy")
                && optional.contains("Phyrexian Metamorph: Decline"),
            "reach guard: the copy is offered as an accept/decline choice: {seen:?}"
        );
        if accept {
            assert_eq!(seen.len(), 2, "{seen:?}");
            assert!(seen[1].starts_with("CopyTargetChoice"), "{seen:?}");
        } else {
            assert_eq!(seen.len(), 1, "declining asks nothing more: {seen:?}");
        }
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
        assert!(r.state().stack.is_empty());
        let entered = &r.state().objects[&metamorph];
        assert_eq!(entered.zone, Zone::Battlefield, "accept {accept}: {seen:?}");
        assert_eq!(
            entered.name,
            if accept {
                "Ornithopter"
            } else {
                "Phyrexian Metamorph"
            },
            "accept {accept}"
        );
        assert!(!entered.is_token, "still a nontoken card (accept {accept})");
        assert!(
            entered.tapped,
            "Edgar's rider applies (accept {accept}): {seen:?}"
        );
        assert_eq!(
            entered.cast_permission_enter_tapped,
            EtbTapState::Unspecified
        );
    }
}

// --- EM5-1 (Q1): the latch outlives a copied source -----------------------------

/// Q1: CR 601.2a + Edgar's 2025-06-06 ruling. P0's Phyrexian Metamorph copies
/// P1's Edgar, so P0 has Edgar's permission. P0 announces Myr Retriever under
/// it, then sacrifices the copy to Ashnod's Altar while paying. CR 400.7: the
/// graveyard object no longer retains the battlefield copy effect, so only the
/// rider latched at announcement can make Retriever
/// enter tapped. Control: the copy stays and the cast is paid from the pool.
#[test]
fn copied_edgar_sacrificed_mid_payment_still_enters_tapped() {
    for sacrifice in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let edgar = s.add_creature_from_oracle(P1, EDGAR_NAME, 2, 4, EDGAR).id();
        let altar = s
            .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
            .id();
        let metamorph = s
            .add_creature_to_hand(P0, "Phyrexian Metamorph", 0, 0)
            .as_artifact_creature()
            .from_oracle_text(PHYREXIAN_METAMORPH)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![engine::types::mana::ManaCostShard::PhyrexianBlue],
            })
            .id();
        let retriever = s
            .add_creature_to_graveyard(P0, "Myr Retriever", 1, 1)
            .as_artifact_creature()
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mut r = s.build();
        assert_edgar_permission(&r, edgar);

        add_colorless(&mut r, 0, 3);
        r.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(0),
            false,
            vec![],
        ));
        let waiting = start_cast(&mut r, metamorph, CastPaymentMode::Auto).expect("cast Metamorph");
        if let WaitingFor::PhyrexianPayment { shards, .. } = waiting {
            r.act(GameAction::SubmitPhyrexianChoices {
                choices: vec![engine::types::game_state::ShardChoice::PayMana; shards.len()],
            })
            .expect("pay the Phyrexian shard with mana");
        }
        assert_eq!(r.state().objects[&metamorph].zone, Zone::Stack);
        let seen = resolve_metamorph_entry(&mut r, Some(edgar));
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(
            seen[0].starts_with("ReplacementChoice(OptionalBranch")
                && seen[1].starts_with("CopyTargetChoice"),
            "reach guard: the copy prompt, accepted with P1's Edgar: {seen:?}"
        );
        assert_eq!(r.state().objects[&metamorph].zone, Zone::Battlefield);
        assert_edgar_permission(&r, metamorph);

        if !sacrifice {
            add_colorless(&mut r, 0, 2);
        }
        let waiting =
            start_cast(&mut r, retriever, CastPaymentMode::Manual).expect("the cast starts");
        if sacrifice {
            assert!(
                matches!(waiting, WaitingFor::ManaPayment { .. }),
                "manual payment, got {waiting:?}"
            );
            assert_eq!(
                pending_permission(&r),
                (Some(metamorph), Some(EtbTapState::Tapped)),
                "before payment: the cast is announced under the copied Edgar and its \
                 latch carries the rider"
            );
            r.act(GameAction::ActivateAbility {
                source_id: altar,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("Ashnod's Altar during payment");
            r.act(GameAction::SelectCards {
                cards: vec![metamorph],
            })
            .expect("sacrifice the copied source");
            assert!(
                matches!(r.state().waiting_for, WaitingFor::ManaPayment { .. }),
                "still paying after the sacrifice, got {:?}",
                r.state().waiting_for
            );
            assert_eq!(
                pending_permission(&r),
                (Some(metamorph), Some(EtbTapState::Tapped)),
                "after the sacrifice: the announced authority and its latch are preserved"
            );
            let departed = &r.state().objects[&metamorph];
            assert_eq!(departed.zone, Zone::Graveyard, "the source departed first");
            assert!(
                departed
                    .static_definitions
                    .iter_all()
                    .all(|def| !matches!(def.mode, StaticMode::GraveyardCastPermission { .. })),
                "reach guard: the departed Metamorph no longer carries Edgar's permission"
            );
        }
        for _ in 0..4 {
            if matches!(r.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                r.act(GameAction::PassPriority).expect("commit the payment");
            }
        }
        assert_eq!(r.state().objects[&retriever].zone, Zone::Stack);
        assert_eq!(selected_authority(&r, retriever), metamorph);
        assert_eq!(
            carrier(&r, retriever),
            EtbTapState::Tapped,
            "the latched rider reached the spell (sacrifice {sacrifice})"
        );
        resolve_all(&mut r);
        assert_eq!(r.state().objects[&retriever].zone, Zone::Battlefield);
        assert!(
            tapped(&r, retriever),
            "enters tapped (sacrifice {sacrifice})"
        );
    }
}

// --- C25a: repo-owned saved games still load ---------------------------------

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture inflates");
    engine::types::game_state::record_captured_onsets_json(&json)
}

fn assert_objects_without_rider(state: &GameState, fixture: &str) {
    assert!(
        !state.objects.is_empty(),
        "reach guard: {fixture} has objects"
    );
    for obj in state.objects.values() {
        assert_eq!(
            obj.cast_permission_enter_tapped,
            EtbTapState::Unspecified,
            "{fixture}: {} reads Unspecified",
            obj.name
        );
        let json = serde_json::to_value(obj).expect("object serializes");
        assert!(
            json.get("cast_permission_enter_tapped").is_none(),
            "{fixture}: {} re-serializes without the new key",
            obj.name
        );
    }
}

/// C25a: the committed saved-game fixtures (written before the field existed)
/// still load through the production restore path, and every object reads the
/// default with no new key on re-serialization.
#[test]
fn saved_game_fixtures_load_without_the_rider() {
    let json = gunzip(include_bytes!("../fixtures/codie_turn14.json.gz"));
    let envelope: serde_json::Value = serde_json::from_str(&json).expect("envelope");
    let codie = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("codie decodes")
        .into_game_state()
        .expect("codie restores");
    assert_objects_without_rider(&codie, "codie_turn14");

    let breya = restore_json(&engine::types::game_state::record_captured_onsets_json(
        include_str!("../fixtures/activation_cost/breya_mode_choice_v78.json"),
    ));
    assert_objects_without_rider(breya.state(), "breya_mode_choice_v78");
}
