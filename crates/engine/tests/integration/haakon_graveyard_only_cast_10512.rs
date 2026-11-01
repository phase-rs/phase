//! Regression (issue #10512): Haakon, Stromgald Scourge — "You may cast this
//! card from your graveyard, but not from anywhere else."
//!
//! CR 601.3: a player can begin to cast a spell only if a rule or effect allows
//! it and no rule or effect prohibits it. CR 113.6f: an object's ability that
//! restricts what zones it can be cast from functions everywhere. CR 101.2: the
//! "can't" wins over every permission (2006-07-15 ruling: "It can't be cast from
//! any zone other than your graveyard, even if an effect such as that of
//! Spelljack or Muse Vessel would otherwise allow you to.").
//!
//! The line lowers to the card's own graveyard permission plus
//! `CastingRestriction::OnlyFromZones { [Graveyard] }`. Haakon is built from its
//! verbatim Oracle text, real cost and types (it is not in the integration
//! fixture); every other card is the printed fixture card. Costs are funded, so
//! a refusal can only come from the restriction, and every refusal row is paired
//! with a Grizzly Bears control through the same production route.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CastingRestriction, Effect, TargetFilter};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use ManaType::{Black, Blue, Colorless, Green, Red, White};

const HAAKON_NAME: &str = "Haakon, Stromgald Scourge";
const HAAKON: &str = "You may cast this card from your graveyard, but not from anywhere else.\nAs long as Haakon is on the battlefield, you may cast Knight spells from your graveyard.\nWhen Haakon dies, you lose 2 life.";
const BEARS: &str = "Grizzly Bears";
const APPRAISER_NAME: &str = "Geological Appraiser";
const APPRAISER: &str = "When this creature enters, if you cast it, discover 3. (Exile cards from the top of your library until you exile a nonland card with mana value 3 or less. Cast it without paying its mana cost or put it into your hand. Put the rest on the bottom in a random order.)";
const SHIKO_NAME: &str = "Shiko, Paragon of the Way";
const SHIKO: &str = "Flying, vigilance\nWhen Shiko enters, exile target nonland card with mana value 3 or less from your graveyard. Copy it, then you may cast the copy without paying its mana cost. (A copy of a permanent spell becomes a token.)";

fn db() -> &'static engine::database::card_db::CardDatabase {
    crate::support::shared_card_db().expect("integration card fixture must load")
}

fn units(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|m| ManaUnit::new(*m, ObjectId(0), false, vec![]))
        .collect()
}

/// Add mana to P0's pool right before a cast, so each cast pays exactly its own
/// cost from a pool holding nothing else.
fn add_mana(r: &mut GameRunner, mana: &[ManaType]) {
    let pool = &mut r.state_mut().players[0].mana_pool;
    for unit in units(mana) {
        pool.add(unit);
    }
}

fn pool_is_empty(r: &GameRunner) -> bool {
    r.state().players[0].mana_pool.is_empty()
}

/// Haakon's printed cost: {1}{B}{B}.
fn haakon_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 1,
    }
}

const HAAKON_MANA: &[ManaType] = &[Black, Black, Colorless];
const BEARS_MANA: &[ManaType] = &[Green, Colorless];

/// Put Haakon (verbatim Oracle, real cost and types) into `zone` (hand or graveyard).
fn add_haakon(s: &mut GameScenario, zone: Zone) -> ObjectId {
    let mut builder = match zone {
        Zone::Graveyard => s.add_creature_to_graveyard(P0, HAAKON_NAME, 3, 3),
        Zone::Hand => s.add_creature_to_hand(P0, HAAKON_NAME, 3, 3),
        other => panic!("stage Haakon in hand or graveyard, then move it: {other:?}"),
    };
    builder
        .from_oracle_text(HAAKON)
        .with_mana_cost(haakon_cost())
        .with_subtypes(vec!["Zombie", "Knight"]);
    builder.id()
}

fn build(s: GameScenario) -> GameRunner {
    let mut r = s.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    r
}

/// Reach guard: line 1 parsed (no unimplemented gap), the restriction and the
/// card's own graveyard permission are on the object.
fn assert_haakon_parsed(r: &GameRunner, id: ObjectId) {
    let obj = &r.state().objects[&id];
    assert_eq!(obj.name, HAAKON_NAME);
    assert_eq!(
        obj.casting_restrictions,
        vec![CastingRestriction::OnlyFromZones {
            zones: vec![Zone::Graveyard],
        }],
        "reach: line 1 must lower to OnlyFromZones([Graveyard])",
    );
    assert!(
        obj.static_definitions.as_slice().iter().any(|s| matches!(
            s.mode,
            StaticMode::GraveyardCastPermission { .. }
        ) && s.affected
            == Some(TargetFilter::SelfRef)
            && s.active_zones == vec![Zone::Graveyard]),
        "reach: line 1 must carry the card's own graveyard permission",
    );
    assert!(
        !obj.abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::Unimplemented { .. })),
        "reach: no line of Haakon may stay unimplemented",
    );
}

/// Whether the engine's legal-action surface offers a cast of `id`.
fn offered(r: &GameRunner, id: ObjectId) -> bool {
    engine::ai_support::legal_actions(r.state())
        .iter()
        .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// Submit a cast of `id` through the public action and require the refusal to
/// leave the object, the stack and the pool untouched.
fn assert_cast_refused(r: &mut GameRunner, id: ObjectId) {
    let zone = r.state().objects[&id].zone;
    let stack = r.state().stack.len();
    let pool = r.state().players[0].mana_pool.total();
    let card_id = r.state().objects[&id].card_id;
    let result = r.act(GameAction::CastSpell {
        object_id: id,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(result.is_err(), "the cast must be refused, got {result:?}");
    assert_eq!(r.state().objects[&id].zone, zone, "refused card stays put");
    assert_eq!(r.state().stack.len(), stack, "nothing reached the stack");
    assert_eq!(
        r.state().players[0].mana_pool.total(),
        pool,
        "no mana spent"
    );
}

/// Cast `id` through the production pipeline and let the stack resolve.
fn cast_and_resolve(r: &mut GameRunner, id: ObjectId) {
    assert!(offered(r, id), "the cast must be offered");
    r.cast(id).resolve();
    settle(r);
}

/// Resolve the stack and require the completion boundary: `advance_until_stack_empty`
/// also stops on an unanswered prompt, a failed action or its iteration limit.
fn settle(r: &mut GameRunner) {
    r.advance_until_stack_empty();
    assert!(
        r.state().stack.is_empty(),
        "the stack must resolve; stack = {:?}",
        r.state().stack
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at a priority window; waiting_for = {:?}",
        r.state().waiting_for
    );
}

fn object_named_in(r: &GameRunner, name: &str, zone: Zone) -> ObjectId {
    r.state()
        .objects
        .values()
        .find(|o| o.name == name && o.zone == zone)
        .map(|o| o.id)
        .unwrap_or_else(|| panic!("{name} must be in {zone:?}"))
}

fn names_in(r: &GameRunner, name: &str, zone: Zone) -> usize {
    r.state()
        .objects
        .values()
        .filter(|o| o.name == name && o.zone == zone)
        .count()
}

/// Make `ids` (top first) the whole of P0's library through the zone pipeline.
fn stack_library(r: &mut GameRunner, ids_top_first: &[ObjectId]) {
    let mut events = Vec::new();
    for id in ids_top_first.iter().rev() {
        engine::game::zones::move_to_library_position(r.state_mut(), *id, true, &mut events);
    }
    let library: Vec<ObjectId> = r.state().players[0].library.iter().copied().collect();
    assert_eq!(library, ids_top_first, "reach: library staged top-first");
}

// --- Graveyard (the permission) -------------------------------------------

/// CR 601.3: Haakon's own permission allows the graveyard cast, at full cost.
#[test]
fn haakon_casts_from_graveyard() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let haakon = add_haakon(&mut s, Zone::Graveyard);
    let mut r = build(s);
    assert_haakon_parsed(&r, haakon);
    add_mana(&mut r, HAAKON_MANA);
    cast_and_resolve(&mut r, haakon);
    assert_eq!(
        names_in(&r, HAAKON_NAME, Zone::Battlefield),
        1,
        "Haakon resolves"
    );
    assert!(pool_is_empty(&r), "Haakon's {{1}}{{B}}{{B}} was paid");
}

// --- Hand -----------------------------------------------------------------

/// CR 601.3 + CR 113.6f: "but not from anywhere else" prohibits the hand cast;
/// Bears from the same hand, same timing, casts.
#[test]
fn haakon_cannot_be_cast_from_hand() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let haakon = add_haakon(&mut s, Zone::Hand);
    let bears = s.add_real_card(P0, BEARS, Zone::Hand, db());
    let mut r = build(s);
    assert_haakon_parsed(&r, haakon);
    add_mana(&mut r, HAAKON_MANA);
    assert!(!offered(&r, haakon), "hand cast must not be offered");
    assert_cast_refused(&mut r, haakon);

    r.state_mut().players[0].mana_pool.clear();
    add_mana(&mut r, BEARS_MANA);
    cast_and_resolve(&mut r, bears);
    assert_eq!(r.state().objects[&bears].zone, Zone::Battlefield);
}

// --- Exile: Wrenn's Resolve ---------------------------------------------

/// CR 101.2: an impulse permission (Wrenn's Resolve) can't override the
/// restriction; the Bears exiled by the same resolution casts.
#[test]
fn haakon_cannot_be_cast_from_exile_under_wrenns_resolve() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let haakon = add_haakon(&mut s, Zone::Hand);
    let bears = s.add_real_card(P0, BEARS, Zone::Hand, db());
    let resolve = s.add_real_card(P0, "Wrenn's Resolve", Zone::Hand, db());
    let mut r = build(s);
    stack_library(&mut r, &[haakon, bears]);
    add_mana(&mut r, &[Red, Colorless]);
    cast_and_resolve(&mut r, resolve);

    let haakon = object_named_in(&r, HAAKON_NAME, Zone::Exile);
    let bears = object_named_in(&r, BEARS, Zone::Exile);
    assert_haakon_parsed(&r, haakon);
    for id in [haakon, bears] {
        assert!(
            !r.state().objects[&id].casting_permissions.is_empty(),
            "reach: Wrenn's Resolve leaves a play permission on {:?}",
            r.state().objects[&id].name,
        );
    }
    add_mana(&mut r, HAAKON_MANA);
    assert!(!offered(&r, haakon), "exile cast must not be offered");
    assert_cast_refused(&mut r, haakon);

    r.state_mut().players[0].mana_pool.clear();
    add_mana(&mut r, BEARS_MANA);
    cast_and_resolve(&mut r, bears);
    assert_eq!(names_in(&r, BEARS, Zone::Battlefield), 1);
}

// --- Library top: Future Sight --------------------------------------------

fn future_sight_board(top_is_haakon: bool) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_real_card(P0, "Future Sight", Zone::Battlefield, db());
    let card = if top_is_haakon {
        add_haakon(&mut s, Zone::Hand)
    } else {
        s.add_real_card(P0, BEARS, Zone::Hand, db())
    };
    let mut r = build(s);
    stack_library(&mut r, &[card]);
    (r, card)
}

/// CR 101.2: Future Sight's top-of-library permission can't override it.
#[test]
fn haakon_cannot_be_cast_from_library_top_under_future_sight() {
    let (mut r, haakon) = future_sight_board(true);
    assert_haakon_parsed(&r, haakon);
    add_mana(&mut r, HAAKON_MANA);
    assert!(!offered(&r, haakon), "library cast must not be offered");
    assert_cast_refused(&mut r, haakon);
}

#[test]
fn control_bears_casts_from_library_top_under_future_sight() {
    let (mut r, bears) = future_sight_board(false);
    add_mana(&mut r, BEARS_MANA);
    cast_and_resolve(&mut r, bears);
    assert_eq!(r.state().objects[&bears].zone, Zone::Battlefield);
}

// --- Command zone ---------------------------------------------------------

fn commander_board(haakon_commander: bool) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let card = if haakon_commander {
        add_haakon(&mut s, Zone::Hand)
    } else {
        s.add_real_card(P0, BEARS, Zone::Hand, db())
    };
    s.with_commander(card);
    let mut r = build(s);
    r.state_mut().format_config.command_zone = true;
    assert_eq!(r.state().objects[&card].zone, Zone::Command, "reach");
    (r, card)
}

/// CR 903.8 permission vs CR 113.6f restriction: the "can't" wins (CR 101.2).
#[test]
fn haakon_cannot_be_cast_from_the_command_zone() {
    let (mut r, haakon) = commander_board(true);
    assert_haakon_parsed(&r, haakon);
    add_mana(&mut r, HAAKON_MANA);
    assert!(
        !offered(&r, haakon),
        "command-zone cast must not be offered"
    );
    assert_cast_refused(&mut r, haakon);
}

#[test]
fn control_bears_casts_from_the_command_zone() {
    let (mut r, bears) = commander_board(false);
    add_mana(&mut r, BEARS_MANA);
    cast_and_resolve(&mut r, bears);
    assert_eq!(r.state().objects[&bears].zone, Zone::Battlefield);
}

// --- Omniscience ------------------------------------------------------------

/// CR 101.2: a free-cast permission from hand (Omniscience) doesn't admit it;
/// the Bears in the same hand casts for free.
#[test]
fn haakon_cannot_be_cast_from_hand_under_omniscience() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_real_card(P0, "Omniscience", Zone::Battlefield, db());
    let haakon = add_haakon(&mut s, Zone::Hand);
    let bears = s.add_real_card(P0, BEARS, Zone::Hand, db());
    let mut r = build(s);
    assert_haakon_parsed(&r, haakon);
    assert!(!offered(&r, haakon), "Omniscience must not offer Haakon");
    assert_cast_refused(&mut r, haakon);
    cast_and_resolve(&mut r, bears);
    assert_eq!(r.state().objects[&bears].zone, Zone::Battlefield);
    assert!(pool_is_empty(&r));
}

// --- Resolution-owned casts: cascade and discover ---------------------------

/// Pass priority (draining trigger ordering and choosing the first legal trigger
/// target) until a cast offer appears.
fn pass_until_cast_offer(r: &mut GameRunner) {
    for _ in 0..16 {
        match &r.state().waiting_for {
            WaitingFor::CastOffer { .. } => return,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                r.choose_first_legal_target()
                    .expect("choose a trigger target");
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt before the cast offer: {other:?}"),
        }
    }
    panic!("no cast offer; waiting_for = {:?}", r.state().waiting_for);
}

/// The hit `id` is offered; a submitted cast must not put it on the stack.
/// Declining (if the refused cast left the offer open) finishes the walk.
fn refuse_offered_hit(r: &mut GameRunner, id: ObjectId, cast: GameAction, decline: GameAction) {
    let _ = r.act(cast);
    assert_ne!(
        r.state().objects[&id].zone,
        Zone::Stack,
        "the hit must not be cast"
    );
    if matches!(r.state().waiting_for, WaitingFor::CastOffer { .. }) {
        r.act(decline).expect("decline the offer");
    }
    settle(r);
}

fn cascade_board(hit_is_haakon: bool) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let elf = s.add_real_card(P0, "Bloodbraid Elf", Zone::Hand, db());
    let hit = if hit_is_haakon {
        add_haakon(&mut s, Zone::Hand)
    } else {
        s.add_real_card(P0, BEARS, Zone::Hand, db())
    };
    let filler = s.add_real_card(P0, "Swamp", Zone::Hand, db());
    let mut r = build(s);
    stack_library(&mut r, &[hit, filler]);
    add_mana(&mut r, &[Red, Green, Colorless, Colorless]);
    assert!(offered(&r, elf));
    r.cast(elf).commit();
    pass_until_cast_offer(&mut r);
    let hit = object_named_in(
        &r,
        if hit_is_haakon { HAAKON_NAME } else { BEARS },
        Zone::Exile,
    );
    assert!(
        matches!(
            &r.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { hit_card, .. },
                ..
            } if *hit_card == hit
        ),
        "reach: cascade offers the hit; waiting_for = {:?}",
        r.state().waiting_for,
    );
    (r, hit)
}

/// CR 702.85a + CR 101.2: Bloodbraid Elf cascades into Haakon; the free cast is
/// refused and Haakon goes to the bottom of the library with the misses.
#[test]
fn bloodbraid_elf_cascade_cannot_cast_haakon() {
    let (mut r, haakon) = cascade_board(true);
    assert_haakon_parsed(&r, haakon);
    refuse_offered_hit(
        &mut r,
        haakon,
        GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        },
        GameAction::CascadeChoice {
            choice: CastChoice::Decline,
        },
    );
    let haakon = object_named_in(&r, HAAKON_NAME, Zone::Library);
    assert_eq!(
        r.state().players[0].library.back(),
        Some(&haakon),
        "Haakon is put on the bottom (CR 702.85a)"
    );
    assert_eq!(names_in(&r, "Bloodbraid Elf", Zone::Battlefield), 1);
}

#[test]
fn control_bloodbraid_elf_cascade_casts_bears() {
    let (mut r, bears) = cascade_board(false);
    r.act(GameAction::CascadeChoice {
        choice: CastChoice::Cast,
    })
    .expect("cast the cascade hit");
    assert_eq!(r.state().objects[&bears].zone, Zone::Stack);
    settle(&mut r);
    assert_eq!(names_in(&r, BEARS, Zone::Battlefield), 1);
}

fn discover_board(hit_is_haakon: bool) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let appraiser = s
        .add_creature_to_hand(P0, APPRAISER_NAME, 3, 2)
        .from_oracle_text(APPRAISER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 2,
        })
        .with_subtypes(vec!["Human", "Artificer"])
        .id();
    let hit = if hit_is_haakon {
        add_haakon(&mut s, Zone::Hand)
    } else {
        s.add_real_card(P0, BEARS, Zone::Hand, db())
    };
    let filler = s.add_real_card(P0, "Swamp", Zone::Hand, db());
    let mut r = build(s);
    stack_library(&mut r, &[hit, filler]);
    add_mana(&mut r, &[Red, Red, Colorless, Colorless]);
    cast_and_resolve_until_offer(&mut r, appraiser);
    let hit = object_named_in(
        &r,
        if hit_is_haakon { HAAKON_NAME } else { BEARS },
        Zone::Exile,
    );
    assert!(
        matches!(
            &r.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if *hit_card == hit
        ),
        "reach: discover offers the hit; waiting_for = {:?}",
        r.state().waiting_for,
    );
    (r, hit)
}

fn cast_and_resolve_until_offer(r: &mut GameRunner, id: ObjectId) {
    assert!(offered(r, id), "the cast must be offered");
    r.cast(id).commit();
    pass_until_cast_offer(r);
}

/// CR 701.57a + CR 101.2: Geological Appraiser discovers Haakon; the free cast
/// is refused, so Haakon is put into its owner's hand.
#[test]
fn geological_appraiser_discover_cannot_cast_haakon() {
    let (mut r, haakon) = discover_board(true);
    assert_haakon_parsed(&r, haakon);
    refuse_offered_hit(
        &mut r,
        haakon,
        GameAction::DiscoverChoice {
            choice: CastChoice::Cast,
        },
        GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        },
    );
    assert_eq!(
        names_in(&r, HAAKON_NAME, Zone::Hand),
        1,
        "an uncast discover hit goes to hand (CR 701.57a)"
    );
    assert_eq!(names_in(&r, HAAKON_NAME, Zone::Battlefield), 0);
}

#[test]
fn control_geological_appraiser_discover_casts_bears() {
    let (mut r, bears) = discover_board(false);
    r.act(GameAction::DiscoverChoice {
        choice: CastChoice::Cast,
    })
    .expect("cast the discover hit");
    assert_eq!(r.state().objects[&bears].zone, Zone::Stack);
    settle(&mut r);
    assert_eq!(names_in(&r, BEARS, Zone::Battlefield), 1);
}

// --- Cast a copy of a card (CR 707.12): Shiko ------------------------------

fn shiko_board(yard_is_haakon: bool) -> (GameRunner, Vec<ObjectId>) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let shiko = s
        .add_creature_to_hand(P0, SHIKO_NAME, 4, 5)
        .from_oracle_text_with_keywords(&["Flying", "Vigilance"], SHIKO)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Red,
                ManaCostShard::White,
            ],
            generic: 2,
        })
        .with_subtypes(vec!["Spirit", "Dragon"])
        .id();
    if yard_is_haakon {
        add_haakon(&mut s, Zone::Graveyard);
    } else {
        s.add_real_card(P0, BEARS, Zone::Graveyard, db());
    }
    let mut r = build(s);
    add_mana(&mut r, &[Blue, Red, White, Colorless, Colorless]);
    assert!(offered(&r, shiko));
    r.cast(shiko).resolve();
    let offered_copies = drive_shiko_trigger(&mut r);
    (r, offered_copies)
}

/// Resolve Shiko's enters trigger: target the only graveyard card, cast every
/// copy offered, and accept any optional cast. Returns the copy sources offered.
fn drive_shiko_trigger(r: &mut GameRunner) -> Vec<ObjectId> {
    let mut offered_copies = Vec::new();
    for _ in 0..24 {
        match &r.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                r.choose_first_legal_target()
                    .expect("choose Shiko's target");
            }
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                let cards = cards.clone();
                offered_copies.extend(cards.iter().copied());
                r.act(GameAction::SelectCards { cards })
                    .expect("cast the offered copy");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                r.act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional cast");
            }
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return offered_copies,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving Shiko: {other:?}"),
        }
    }
    panic!(
        "Shiko's trigger never finished: {:?}",
        r.state().waiting_for
    );
}

/// CR 707.12 + CR 113.6f: Shiko exiles Haakon and copies it; the copy is created
/// in exile and copies the restriction, so it is neither offered nor cast.
#[test]
fn shiko_cannot_cast_a_copy_of_haakon_from_exile() {
    let (r, offered_copies) = shiko_board(true);
    let haakon = object_named_in(&r, HAAKON_NAME, Zone::Exile);
    assert_haakon_parsed(&r, haakon);
    assert!(
        offered_copies.is_empty(),
        "no copy of Haakon may be offered: {offered_copies:?}"
    );
    assert_eq!(
        names_in(&r, HAAKON_NAME, Zone::Battlefield),
        0,
        "no copy of Haakon was cast"
    );
    assert_eq!(names_in(&r, HAAKON_NAME, Zone::Stack), 0);
}

#[test]
fn control_shiko_casts_a_copy_of_bears() {
    let (r, offered_copies) = shiko_board(false);
    let bears = object_named_in(&r, BEARS, Zone::Exile);
    assert_eq!(offered_copies, vec![bears], "reach: Shiko offered the copy");
    assert_eq!(
        names_in(&r, BEARS, Zone::Battlefield),
        1,
        "the copy of Bears was cast and resolved"
    );
}

// --- Incarnations (CR 400.7) ----------------------------------------------

/// Hostile: the restriction follows the card through new objects. Cast from the
/// graveyard; Lightning Bolt kills it (its dies trigger costs 2 life); cast it
/// from the graveyard again; Unsummon returns it to hand, where it can't be cast.
#[test]
fn haakon_restriction_survives_death_and_bounce() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let haakon = add_haakon(&mut s, Zone::Graveyard);
    let bolt = s.add_real_card(P0, "Lightning Bolt", Zone::Hand, db());
    let unsummon = s.add_real_card(P0, "Unsummon", Zone::Hand, db());
    let mut r = build(s);

    add_mana(&mut r, HAAKON_MANA);
    cast_and_resolve(&mut r, haakon);
    let on_battlefield = object_named_in(&r, HAAKON_NAME, Zone::Battlefield);
    let life = r.life(P0);

    add_mana(&mut r, &[Red]);
    r.cast(bolt).target_object(on_battlefield).resolve();
    settle(&mut r);
    let back = object_named_in(&r, HAAKON_NAME, Zone::Graveyard);
    assert_eq!(
        r.life(P0),
        life - 2,
        "reach: Haakon died (its dies trigger)"
    );
    assert_haakon_parsed(&r, back);

    add_mana(&mut r, HAAKON_MANA);
    cast_and_resolve(&mut r, back);
    let on_battlefield = object_named_in(&r, HAAKON_NAME, Zone::Battlefield);

    add_mana(&mut r, &[Blue]);
    r.cast(unsummon).target_object(on_battlefield).resolve();
    settle(&mut r);
    let in_hand = object_named_in(&r, HAAKON_NAME, Zone::Hand);
    assert_haakon_parsed(&r, in_hand);
    add_mana(&mut r, HAAKON_MANA);
    assert!(!offered(&r, in_hand), "bounced Haakon must not be castable");
    assert_cast_refused(&mut r, in_hand);
}

/// The restriction is Haakon's own: White Knight in hand stays castable, and
/// line 2's Knight permission casts White Knight from the graveyard.
#[test]
fn haakon_restriction_does_not_leak_to_other_knights() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, HAAKON_NAME, 3, 3, HAAKON)
        .with_subtypes(vec!["Zombie", "Knight"]);
    let hand_knight = s.add_real_card(P0, "White Knight", Zone::Hand, db());
    let yard_knight = s.add_real_card(P0, "White Knight", Zone::Graveyard, db());
    let mut r = build(s);
    assert!(r.state().objects[&hand_knight]
        .casting_restrictions
        .is_empty());
    add_mana(&mut r, &[White, White]);
    cast_and_resolve(&mut r, yard_knight);
    assert_eq!(r.state().objects[&yard_knight].zone, Zone::Battlefield);
    add_mana(&mut r, &[White, White]);
    cast_and_resolve(&mut r, hand_knight);
    assert_eq!(names_in(&r, "White Knight", Zone::Battlefield), 2);
}

// --- Parser ----------------------------------------------------------------
//
// Every line passed to `parse_line` ("Probe") is a SYNTHETIC parser-structure or
// strict-refusal diagnostic, not a supported card. The card-data sweep found no
// printed card other than Haakon carrying the ", but not from anywhere else"
// rider. The nearest printed card is Haakon, Stromgald Scourge: "You may cast
// this card from your graveyard, but not from anywhere else." The positives pin
// that the restriction's zones come from the permission's own zone tail; the
// negatives pin that every other head fails closed.

fn parse_line(line: &str) -> engine::parser::oracle::ParsedAbilities {
    engine::parser::oracle::parse_oracle_text(line, "Probe", &[], &["Creature".to_string()], &[])
}

#[test]
fn haakon_line_one_parses_to_permission_and_restriction() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        HAAKON,
        HAAKON_NAME,
        &[],
        &["Creature".to_string()],
        &["Zombie".to_string(), "Knight".to_string()],
    );
    assert_eq!(
        parsed.casting_restrictions,
        vec![CastingRestriction::OnlyFromZones {
            zones: vec![Zone::Graveyard],
        }],
    );
    assert!(
        parsed.statics.iter().any(
            |s| matches!(s.mode, StaticMode::GraveyardCastPermission { .. })
                && s.affected == Some(TargetFilter::SelfRef)
                && s.active_zones == vec![Zone::Graveyard]
        ),
        "line 1 must emit the card's own graveyard permission: {:?}",
        parsed.statics,
    );
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::Unimplemented { .. })),
        "no line may stay unimplemented: {:?}",
        parsed.abilities,
    );
}

/// Synthetic diagnostic (see the section note): the zone list comes from the
/// permission itself, so every own-permission zone tail is the same class.
#[test]
fn exclusive_own_permission_zone_tails() {
    for (line, zones) in [
        (
            "You may cast this card from exile, but not from anywhere else.",
            vec![Zone::Exile],
        ),
        (
            "You may cast this card from your graveyard or from exile, but not from anywhere else.",
            vec![Zone::Graveyard, Zone::Exile],
        ),
    ] {
        let parsed = parse_line(line);
        assert_eq!(
            parsed.casting_restrictions,
            vec![CastingRestriction::OnlyFromZones {
                zones: zones.clone()
            }],
            "{line}",
        );
        assert!(
            parsed
                .statics
                .iter()
                .any(|s| s.active_zones == zones && s.affected == Some(TargetFilter::SelfRef)),
            "{line}: {:?}",
            parsed.statics,
        );
    }
}

/// Synthetic strict-refusal diagnostics (see the section note): a rider on
/// anything but the card's own unlimited, rider-free permission stays an honest
/// gap, with neither half emitted.
#[test]
fn exclusive_rider_on_other_permissions_fails_closed() {
    for line in [
        "You may cast creature spells from your graveyard, but not from anywhere else.",
        "Once during each of your turns, you may cast this card from your graveyard, but not from anywhere else.",
        "You may cast this card from your graveyard, but not from anywhere else unless you control a Knight.",
        "You may play this card from your graveyard, but not from anywhere else.",
        // CR 611.2a: the permission parser discards a leading duration, so a timed
        // head must decline rather than become a permanent permission + restriction.
        "Until end of turn, you may cast this card from your graveyard, but not from anywhere else.",
    ] {
        let parsed = parse_line(line);
        assert!(parsed.casting_restrictions.is_empty(), "{line}");
        assert!(
            !parsed
                .statics
                .iter()
                .any(|s| matches!(s.mode, StaticMode::GraveyardCastPermission { .. })),
            "{line}: {:?}",
            parsed.statics,
        );
        assert!(
            parsed
                .abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::Unimplemented { .. })),
            "{line} must stay an unimplemented gap: {:?}",
            parsed.abilities,
        );
    }
}

// --- Serialized shape --------------------------------------------------------

/// The new variant's wire shape (card data and game-object state) round-trips.
#[test]
fn only_from_zones_serde_round_trip() {
    let restriction = CastingRestriction::OnlyFromZones {
        zones: vec![Zone::Graveyard],
    };
    let json = serde_json::to_value(&restriction).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({ "type": "OnlyFromZones", "data": { "zones": ["Graveyard"] } }),
    );
    let back: CastingRestriction = serde_json::from_value(json).expect("deserializes");
    assert_eq!(back, restriction);
}
