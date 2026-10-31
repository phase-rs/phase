//! Turtle Team-Up (casual Universes Beyond co-op product): Boss and Event cards.
//!
//! Bosses (the `CoreType::Boss` card type) are nontraditional cards that wait
//! face down in the Boss deck and enter the command zone face up as stages
//! deploy, mirroring Archenemy schemes (`game::archenemy`). They are not
//! permanents and can't be cast — a face-up Boss applies its abilities from the
//! command zone until its stage is cleared. Events (the `CoreType::Event` card
//! type) are one-shot cards in the Boss event deck: revealed during the Boss
//! Action-phase wave, resolved, then put into the Boss graveyard.
//!
//! Neither type has a CR section of its own — the product booklet is the rules
//! authority — so this module names product rules rather than citing rule
//! numbers. Ability function from the command zone reuses the same seams as
//! the CR-backed command-zone types: grafted definitions stamp `Zone::Command`
//! (the `synthesize_conspiracy` / `synthesize_archenemy` precedent), the
//! per-definition opt-in gates live in `game::functioning_abilities`, and the
//! layer/trigger gathers admit a face-up Boss via
//! [`functions_from_command_zone`].
//!
//! This is the runtime sibling of `game::archenemy` (schemes),
//! `game::planechase` (planes/phenomena), and `game::conspiracy` — the other
//! command-zone card types — plus the Event one-shot dispatch the Horde wave
//! loop consults on each reveal.

use crate::game::effects::change_zone::{execute_zone_move, ZoneMoveResult};
use crate::game::game_object::GameObject;
use crate::types::card_type::CoreType;
use crate::types::game_state::{
    GameState, HeroEventAnswer, HeroEventApplyStep, HeroEventKind, PendingHeroEvent, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use rand::seq::SliceRandom;

/// True when the object is a Boss card.
pub fn is_boss(obj: &GameObject) -> bool {
    obj.card_types.core_types.contains(&CoreType::Boss)
}

/// True when the object is an Event card.
pub fn is_event(obj: &GameObject) -> bool {
    obj.card_types.core_types.contains(&CoreType::Event)
}

/// True when this object is a Boss that is currently functioning from the
/// command zone — i.e. a Boss that is face up in the command zone.
///
/// This is the command-zone ability-source gate for Bosses, mirroring the
/// `is_emblem` gate command-zone emblems use and
/// `conspiracy::functions_from_command_zone`. The layer gather
/// (`layers::for_each_static_effect_source`), its candidate index
/// (`static_source_index`), and the command-zone trigger scan admit a Boss as
/// a source iff this returns `true`. A face-down Boss (still waiting in the
/// Boss deck pile) does not yet function and so is excluded until deployed.
pub fn functions_from_command_zone(obj: &GameObject) -> bool {
    obj.zone == Zone::Command && !obj.face_down && is_boss(obj)
}

/// The Bosses currently deployed — face-up Boss cards in the command zone,
/// in command-zone order. Face-down Bosses (still in the Boss deck pile) are
/// excluded: they aren't yet functioning, and stage sizes, the "number of
/// bosses" counts, and Krang's wave bonus all read the deployed set.
pub fn bosses_in_command_zone(state: &GameState) -> Vec<ObjectId> {
    state
        .command_zone
        .iter()
        .copied()
        .filter(|&id| {
            state
                .objects
                .get(&id)
                .is_some_and(functions_from_command_zone)
        })
        .collect()
}

pub(crate) fn boss_name(state: &GameState, id: ObjectId) -> Option<&str> {
    state.objects.get(&id).map(|o| o.name.as_str())
}

/// True while Krang, Dimension X Overlord is deployed — the Boss wave plays
/// one additional card each turn (`horde::wave_seed` consults this).
pub(crate) fn is_krang_deployed(state: &GameState) -> bool {
    bosses_in_command_zone(state)
        .iter()
        .any(|&id| boss_name(state, id) == Some("Krang, Dimension X Overlord"))
}

/// The Boss-deck `(count, preset-id)` list for a challenge deck: empty for
/// every classic mill-race deck, the 11 Bosses for Turtle Team-Up. Exhaustive
/// so a future boss-deck game fails to compile until its pile is pinned here.
fn boss_deck_list(deck: crate::types::format::ChallengeDeck) -> &'static [(u32, &'static str)] {
    use crate::types::format::ChallengeDeck;
    match deck {
        ChallengeDeck::CybermanHorde => &[],
        ChallengeDeck::DndHorde => &[],
        ChallengeDeck::ZombiesHorde => &[],
        ChallengeDeck::SliversHorde => &[],
        ChallengeDeck::HumansGodzillaHorde => &[],
        ChallengeDeck::SauronHorde => &[],
        ChallengeDeck::SarumanHorde => &[],
        ChallengeDeck::LotrTwoTowersHorde => &[],
        ChallengeDeck::TurtleTeamUpHorde => {
            crate::game::decks::turtle_teamup_horde::TURTLE_TEAMUP_BOSS_DECK_TOKENS
        }
    }
}

/// Load the Boss pile for a boss-deck game: one face-down Boss object per deck
/// entry, shuffled, owned by the Boss seat. Called from deck setup alongside
/// `load_horde_library` (which loads the event deck as the library).
pub(crate) fn load_boss_deck(state: &mut GameState, owner: PlayerId) {
    use crate::game::token_presets::known_token_preset_by_id;

    state.boss_deck.clear();
    let Some(deck) = state
        .format_config
        .horde_ruleset
        .as_ref()
        .map(|r| r.challenge_deck)
    else {
        return;
    };
    // CR 400.2: expand the entries and shuffle BEFORE object allocation (a
    // Placement draw, like `shuffled_entry_faces` for hidden-order decks).
    // Sequential `ObjectId`s plus the viewer-visible pile order would
    // otherwise let an observer sort by ID and recover the assembly order.
    let mut assembled: Vec<(
        &'static crate::types::proposed_event::TokenCharacteristics,
        Option<crate::types::card::TokenImageRef>,
    )> = Vec::new();
    for (count, selector) in boss_deck_list(deck) {
        let preset = known_token_preset_by_id(selector).unwrap_or_else(|| {
            panic!("boss-deck selector '{selector}' must resolve to a token preset")
        });
        for _ in 0..*count {
            let image_ref = crate::game::deck_loading::horde_token_image_ref(preset);
            assembled.push((&preset.body, Some(image_ref)));
        }
    }
    assembled.shuffle(
        state
            .rng
            .draw(crate::types::game_state::RandomDraw::Placement),
    );
    for (body, image_ref) in assembled {
        let id = create_boss_deck_card(state, body, image_ref, owner);
        state.boss_deck.push_back(id);
    }
    // Shuffle the assembled pile (a hidden pile — Placement draw) via the
    // game rng, so the Boss order is random yet reproducible — the same helper
    // the Horde library shuffle uses.
    let GameState { boss_deck, rng, .. } = state;
    crate::util::im_ext::shuffle_vector(
        boss_deck,
        rng.draw(crate::types::game_state::RandomDraw::Placement),
    );
}

/// Create one face-down Boss object for the Boss pile, mirroring
/// `create_horde_library_token` (token body + pinned preset identity) and
/// `create_scheme_deck_card` (born into the command zone face down, then held
/// in the pile instead of the command-zone list).
fn create_boss_deck_card(
    state: &mut GameState,
    body: &crate::types::proposed_event::TokenCharacteristics,
    token_image_ref: Option<crate::types::card::TokenImageRef>,
    owner: PlayerId,
) -> ObjectId {
    use crate::types::identifiers::CardId;

    let obj_id = crate::game::zones::create_object(
        state,
        CardId(0),
        owner,
        body.display_name.clone(),
        Zone::Command,
    );
    let obj = state.objects.get_mut(&obj_id).expect("just created");
    obj.is_token = true;
    // A Boss IS a Horde deck card living outside the battlefield by design
    // (pile, then command zone, then exile on a clear) — flag it so the
    // CR 704.5d sweep spares it and the CR 111.8 movement freeze lets the
    // stage-clear exile proceed.
    obj.is_horde_deck_token = true;
    obj.token_image_ref = token_image_ref.clone();
    obj.source_related_token_ids = token_image_ref
        .map(|image_ref| vec![image_ref.preset_id])
        .unwrap_or_default();
    obj.power = body.power;
    obj.toughness = body.toughness;
    obj.base_power = body.power;
    obj.base_toughness = body.toughness;
    obj.card_types = crate::types::card_type::CardType {
        supertypes: body.supertypes.clone(),
        core_types: body.core_types.clone(),
        subtypes: body.subtypes.clone(),
    };
    obj.base_card_types = obj.card_types.clone();
    obj.base_name = body.display_name.clone();
    obj.color = body.colors.clone();
    obj.base_color = body.colors.clone();
    obj.keywords = body.keywords.clone();
    obj.base_keywords = body.keywords.clone();
    obj.face_down = true;
    // allow-raw-zone: new Boss pile object is born into command-zone bookkeeping (scheme-deck birth pattern).
    state.command_zone.retain(|id| *id != obj_id);
    obj_id
}

/// Set up the Boss side of a boss-deck game: load the shuffled Boss pile and
/// deploy stage 1. Called from deck setup after the libraries load.
pub(crate) fn setup_boss_game(
    state: &mut GameState,
    owner: PlayerId,
    events: &mut Vec<crate::types::events::GameEvent>,
) {
    load_boss_deck(state, owner);
    deploy_stage(state, owner, 1, events);
}

/// Boss life for the current game: the ruleset's life table read at the live
/// survivor count. `None` outside a boss-deck game.
fn boss_life_for_state(state: &GameState) -> Option<i32> {
    let stages = state
        .format_config
        .horde_ruleset
        .as_ref()?
        .boss_stages
        .as_ref()?;
    let survivors = state
        .players
        .len()
        .saturating_sub(crate::game::horde::horde_seats(state).len());
    Some(stages.life_for_survivor_count(survivors))
}

/// Deploy a Boss stage: move the stage's Boss count off the top of the Boss
/// pile into the command zone face up, graft each Boss's abilities, apply
/// Leatherhead's enter-gain, and record the stage.
fn deploy_stage(
    state: &mut GameState,
    owner: PlayerId,
    stage: u8,
    events: &mut Vec<crate::types::events::GameEvent>,
) {
    let count = state
        .format_config
        .horde_ruleset
        .as_ref()
        .and_then(|r| r.boss_stages.as_ref())
        .and_then(|b| {
            b.stage_sizes
                .get(usize::from(stage.saturating_sub(1)))
                .copied()
        })
        .unwrap_or(0);
    let mut deployed = Vec::new();
    for _ in 0..count {
        let Some(boss_id) = state.boss_deck.pop_front() else {
            break;
        };
        if let Some(obj) = state.objects.get_mut(&boss_id) {
            obj.face_down = false;
            obj.controller = owner;
        }
        // allow-raw-zone: Boss deployment stays in the command zone (face-down pile to face up; scheme set-in-motion pattern).
        state.command_zone.push_back(boss_id);
        graft_boss_abilities(state, boss_id);
        deployed.push(boss_id);
    }
    state.boss_stage = stage;
    // Leatherhead, Sewer Gator: "When this boss enters, the bosses gain 5 life
    // for each boss in play." Applied synchronously at deploy, not as a stack
    // trigger: setup placements never trigger, and stages advance inside the
    // SBA loop — a trigger would fire inconsistently across stages. The count
    // reads the command zone AFTER this stage's Bosses deploy.
    if deployed
        .iter()
        .any(|&id| boss_name(state, id) == Some("Leatherhead, Sewer Gator"))
    {
        let bosses = bosses_in_command_zone(state).len() as i32;
        gain_boss_life(state, owner, 5 * bosses, events);
    }
}

/// Gain life for the Boss seat through the life resolver (so life-gain events
/// and replacements behave exactly as for any other gain), sourced on the
/// first deployed Boss.
fn gain_boss_life(
    state: &mut GameState,
    horde: PlayerId,
    amount: i32,
    events: &mut Vec<crate::types::events::GameEvent>,
) {
    use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};

    let Some(source_id) = bosses_in_command_zone(state).into_iter().next() else {
        return;
    };
    let ability = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: amount },
            player: TargetFilter::Controller,
        },
        Vec::new(),
        source_id,
        horde,
    );
    let _ = crate::game::effects::life::resolve_gain(state, &ability, events);
}

/// Turtle Team-Up stage clear, called from the SBA fixpoint loop BEFORE life
/// losers are collected: when the Boss life hits zero on a non-final stage,
/// the deployed Bosses leave play ("taken out of play" — exiled, NOT the
/// graveyard, so defeated Bosses are never reshuffled by Villain Scheme!),
/// Boss life resets to the stage value, and the next stage deploys. Returns
/// true when a stage cleared (life reset, so the Boss seat is not a life loser
/// this pass). On the final stage returns false so the normal CR 704.5a loss
/// proceeds and the heroes win.
pub(crate) fn maybe_clear_boss_stage(
    state: &mut GameState,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> bool {
    use crate::game::zone_pipeline::{move_objects_simultaneously, ZoneMoveRequest};

    if !crate::game::horde::has_boss_stages(state) {
        return false;
    }
    let Some(&horde) = crate::game::horde::horde_seats(state).first() else {
        return false;
    };
    let final_stage = state
        .format_config
        .horde_ruleset
        .as_ref()
        .and_then(|r| r.boss_stages.as_ref())
        .map(|b| b.stage_sizes.len() as u8)
        .unwrap_or(0);
    // Read through the team total — the same threshold `collect_life_losers`
    // checks — so a clear and a loss can never disagree about zero.
    if crate::game::players::team_life_total(state, horde) > 0 {
        return false;
    }
    if state.boss_stage == 0 || state.boss_stage >= final_stage {
        return false;
    }
    let bosses = bosses_in_command_zone(state);
    let reqs: Vec<ZoneMoveRequest> = bosses
        .iter()
        .map(|&id| ZoneMoveRequest::effect(id, Zone::Exile, id))
        .collect();
    let _ = move_objects_simultaneously(state, reqs, events);
    if let Some(full) = boss_life_for_state(state) {
        if let Some(seat) = state.players.iter_mut().find(|p| p.id == horde) {
            seat.life = full;
        }
    }
    deploy_stage(state, horde, state.boss_stage + 1, events);
    true
}

// ---- Boss ability grafts ----

/// Graft a deployed Boss's abilities onto its object: statics and triggers
/// synthesized from the product booklet. The catalog materializer only parses
/// static lines and cannot express the product vocabulary ("the bosses", "the
/// heroes", "named Foot Disciple"), so Bosses skip catalog injection
/// (`materialize_token_ability_payload` returns display-only for Boss/Event
/// presets) and this table is their sole function source. Every definition
/// stamps `Zone::Command` (the conspiracy/scheme precedent) so it functions
/// from the command zone via `functioning_abilities`.
fn graft_boss_abilities(state: &mut GameState, boss_id: ObjectId) {
    let Some(name) = boss_name(state, boss_id).map(str::to_string) else {
        return;
    };
    let Some(obj) = state.objects.get_mut(&boss_id) else {
        return;
    };
    match name.as_str() {
        // "Creatures named Foot Disciple have flying."
        "Baxter, Exzzperimental Zzientist" => {
            obj.static_definitions.push(grant_keyword_static(
                foot_disciple_filter(),
                crate::types::keywords::Keyword::Flying,
                "Creatures named Foot Disciple have flying.",
            ));
        }
        // "Whenever the bosses attack, put a +1/+1 counter on each creature
        // named Foot Disciple." ("The bosses attack" from the Boss seat's
        // perspective is `YouAttack`.)
        "Bebop, Porcine Punk" | "Rocksteady, Rowdy Rhino" => {
            obj.push_printed_trigger(bosses_attack_put_counter_trigger());
        }
        // "Ninjas the bosses control get +1/+0 for each boss in play."
        "Chrome Dome, Cyber-Ninja" => {
            obj.static_definitions.push(chrome_dome_static());
        }
        // "Whenever a creature the heroes control dies, the heroes lose 1 life."
        "Karai, Shadow Warrior" => {
            obj.push_printed_trigger(dies_trigger(
                hero_creature_filter(),
                "Whenever a creature the heroes control dies, the heroes lose 1 life.",
            ));
        }
        // "At the start of the Boss turn, the bosses lose 2 life." (The
        // additional-card clause is a wave-size rule, read live by
        // `horde::wave_seed` via `is_krang_deployed`, not a grafted ability.)
        "Krang, Dimension X Overlord" => {
            obj.push_printed_trigger(krang_upkeep_trigger());
        }
        // "When this boss enters, the bosses gain 5 life for each boss in
        // play." — applied synchronously by `deploy_stage`, not grafted: see
        // the note there.
        "Leatherhead, Sewer Gator" => {}
        // "Ninjas the bosses control have deathtouch."
        "Rat King, the Chaosbringer" => {
            obj.static_definitions.push(grant_keyword_static(
                ninjas_you_control_filter(),
                crate::types::keywords::Keyword::Deathtouch,
                "Ninjas the bosses control have deathtouch.",
            ));
        }
        // "Whenever one or more Ninjas the bosses control deal combat damage
        // to the heroes, the bosses play a card."
        "Savanti Romero, Temporal Rogue" => {
            obj.push_printed_trigger(savanti_combat_damage_trigger());
        }
        // "Whenever a creature the bosses control dies, the heroes lose 1 life."
        "Shredder, Foot Clan Overlord" => {
            obj.push_printed_trigger(dies_trigger(
                boss_creature_filter(),
                "Whenever a creature the bosses control dies, the heroes lose 1 life.",
            ));
        }
        // "Attacking creatures the bosses control have first strike."
        "Slash, the Dark Mirror" => {
            obj.static_definitions.push(grant_keyword_static(
                attacking_boss_creature_filter(),
                crate::types::keywords::Keyword::FirstStrike,
                "Attacking creatures the bosses control have first strike.",
            ));
        }
        other => {
            tracing::warn!("graft_boss_abilities: unknown Boss '{other}' — no abilities grafted");
        }
    }
    // The graft lands outside any creation pipeline, so the new triggers are
    // invisible to candidate collection until reindexed (the legendary-graft
    // precedent — `PHASE_TRIGGER_INDEX_AUDIT` catches a missing reindex, never
    // the matcher). Grafted statics need no equivalent call: the static index
    // rebuilds at top-of-pass.
    crate::game::trigger_index::reindex_object_triggers(state, boss_id);
}

/// "Creatures named Foot Disciple": Baxter's static and the Bebop/Rocksteady
/// counter target share this filter.
fn foot_disciple_filter() -> crate::types::ability::TargetFilter {
    use crate::types::ability::{TargetFilter, TypeFilter, TypedFilter};
    TargetFilter::And {
        filters: vec![
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            TargetFilter::Named {
                name: "Foot Disciple".to_string(),
            },
        ],
    }
}

/// "Ninjas the bosses control" — Ninjas controlled by the Boss seat.
fn ninjas_you_control_filter() -> crate::types::ability::TargetFilter {
    use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![
            TypeFilter::Creature,
            TypeFilter::Subtype("Ninja".to_string()),
        ],
        controller: Some(ControllerRef::You),
        properties: Vec::new(),
    })
}

/// "A creature the bosses control" — the Shredder dies subject.
fn boss_creature_filter() -> crate::types::ability::TargetFilter {
    use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: Some(ControllerRef::You),
        properties: Vec::new(),
    })
}

/// "A creature the heroes control" — the Karai dies subject (heroes are the
/// Boss seat's opponents).
fn hero_creature_filter() -> crate::types::ability::TargetFilter {
    use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: Some(ControllerRef::Opponent),
        properties: Vec::new(),
    })
}

/// "Attacking creatures the bosses control" — the Slash grant population.
fn attacking_boss_creature_filter() -> crate::types::ability::TargetFilter {
    use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter};
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: Some(ControllerRef::You),
        properties: vec![FilterProp::Attacking { defender: None }],
    })
}

/// A command-zone static granting a keyword to a population (Baxter, Rat King,
/// Slash).
fn grant_keyword_static(
    affected: crate::types::ability::TargetFilter,
    keyword: crate::types::keywords::Keyword,
    description: &str,
) -> crate::types::ability::StaticDefinition {
    use crate::types::ability::{ContinuousModification, StaticDefinition};
    let mut def = StaticDefinition::continuous()
        .affected(affected)
        .modifications(vec![ContinuousModification::AddKeyword { keyword }]);
    def.active_zones = vec![Zone::Command];
    def.description = Some(description.to_string());
    def
}

/// Chrome Dome's "+1/+0 for each boss in play" as a LIVE quantity: the
/// layer system re-resolves `DeployedBossCount` every pass, so the bonus
/// tracks stage transitions (deploy/clear) with no re-grafting — grafting
/// once at deploy is correct no matter when later stages arrive.
fn chrome_dome_static() -> crate::types::ability::StaticDefinition {
    use crate::types::ability::{
        ContinuousModification, QuantityExpr, QuantityRef, StaticDefinition,
    };
    let mut def = StaticDefinition::continuous()
        .affected(ninjas_you_control_filter())
        .modifications(vec![ContinuousModification::AddDynamicPower {
            value: QuantityExpr::Ref {
                qty: QuantityRef::DeployedBossCount,
            },
        }]);
    def.active_zones = vec![Zone::Command];
    def.description =
        Some("Ninjas the bosses control get +1/+0 for each boss in play.".to_string());
    def
}

/// A command-zone trigger skeleton: mode + zones + description; the caller
/// fills the mode-specific fields.
fn command_trigger(
    mode: crate::types::triggers::TriggerMode,
    description: &str,
) -> crate::types::ability::TriggerDefinition {
    let mut def = crate::types::ability::TriggerDefinition::new(mode);
    def.trigger_zones = vec![Zone::Command];
    def.description = Some(description.to_string());
    def
}

/// Trigger execute ability: each opponent (each hero) loses `amount` life —
/// the `player_scope: Opponent` encoding ("Each opponent loses N life",
/// resolved through `resolve_ability_chain`'s scope iteration).
fn heroes_lose_life_execute(amount: i32) -> crate::types::ability::AbilityDefinition {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PlayerFilter, QuantityExpr,
    };
    let mut def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: amount },
            target: None,
        },
    );
    def.player_scope = Some(PlayerFilter::Opponent);
    def
}

/// Bebop / Rocksteady: "Whenever the bosses attack, put a +1/+1 counter on
/// each creature named Foot Disciple."
fn bosses_attack_put_counter_trigger() -> crate::types::ability::TriggerDefinition {
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
    use crate::types::counter::CounterType;
    use crate::types::triggers::TriggerMode;
    let mut def = command_trigger(
        TriggerMode::YouAttack,
        "Whenever the bosses attack, put a +1/+1 counter on each creature named Foot Disciple.",
    );
    def.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: foot_disciple_filter(),
        },
    )));
    def
}

/// Shredder / Karai dies trigger: "Whenever [a creature the bosses/heroes
/// control] dies, the heroes lose 1 life."
fn dies_trigger(
    subject: crate::types::ability::TargetFilter,
    description: &str,
) -> crate::types::ability::TriggerDefinition {
    use crate::types::triggers::TriggerMode;
    let mut def = command_trigger(TriggerMode::ChangesZone, description);
    def.origin = Some(Zone::Battlefield);
    def.destination = Some(Zone::Graveyard);
    def.valid_card = Some(subject);
    def.execute = Some(Box::new(heroes_lose_life_execute(1)));
    def
}

/// Krang: "At the start of the Boss turn, the bosses lose 2 life." (Default
/// Phase scope — the controller's phases — is the Boss's turn.)
fn krang_upkeep_trigger() -> crate::types::ability::TriggerDefinition {
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
    use crate::types::phase::Phase;
    use crate::types::triggers::TriggerMode;
    let mut def = command_trigger(
        TriggerMode::Phase,
        "At the start of the Boss turn, the bosses lose 2 life.",
    );
    def.phase = Some(Phase::Upkeep);
    def.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: None,
        },
    )));
    def
}

/// Savanti Romero: "Whenever one or more Ninjas the bosses control deal combat
/// damage to the heroes, the bosses play a card." (`DamageDoneOnce` + batched
/// = once per combat batch, not per damaged hero.)
fn savanti_combat_damage_trigger() -> crate::types::ability::TriggerDefinition {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, DamageKindFilter, Effect, TargetFilter,
    };
    use crate::types::triggers::TriggerMode;
    let mut def = command_trigger(
        TriggerMode::DamageDoneOnce,
        "Whenever one or more Ninjas the bosses control deal combat damage to the heroes, the bosses play a card.",
    );
    def.batched = true;
    def.valid_source = Some(ninjas_you_control_filter());
    def.valid_target = Some(TargetFilter::Opponent);
    def.damage_kind = DamageKindFilter::CombatOnly;
    def.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::HordePlayCard { count: 1 },
    )));
    def
}

// ---- Ninja grafts ----

/// Graft a revealed Ninja's non-keyword statics: every Ninja "can't block"
/// and the Enforcer "can't be blocked by more than one creature". (Flying on
/// the Skirmisher rides the body keywords, which catalog injection installs;
/// these lines don't parse there — existing "can't block" catalog tokens are
/// `PartialMissingAbilities` — so the reveal path grafts them. Called from
/// `reveal_library_token` after catalog injection; a no-op for any other
/// object.)
pub(crate) fn graft_ninja_abilities(state: &mut GameState, id: ObjectId) {
    use crate::types::ability::{StaticDefinition, TargetFilter};
    use crate::types::statics::StaticMode;

    let Some(name) = boss_name(state, id).map(str::to_string) else {
        return;
    };
    let mut statics = Vec::new();
    match name.as_str() {
        "Foot Disciple" | "Foot Skirmisher" | "Foot Enforcer" => {
            let mut def =
                StaticDefinition::new(StaticMode::CantBlock).affected(TargetFilter::SelfRef);
            def.description = Some("This creature can't block.".to_string());
            statics.push(def);
        }
        _ => return,
    }
    if name == "Foot Enforcer" {
        let mut def = StaticDefinition::new(StaticMode::CantBeBlockedByMoreThan { max: 1 })
            .affected(TargetFilter::SelfRef);
        def.description =
            Some("This creature can't be blocked by more than one creature.".to_string());
        statics.push(def);
    }
    if let Some(obj) = state.objects.get_mut(&id) {
        for def in statics {
            obj.static_definitions.push(def);
        }
    }
}

// ---- Event one-shots ----

/// Resolve a revealed Event one-shot (no stack — the product has no response
/// step for Events). The wave moves the Event to the Boss graveyard BEFORE
/// calling (except Baxter's Experiments, which sets itself aside instead), so
/// "this card" reads (Villain Scheme!) and graveyard reads (Shredder's
/// Challenge) see the post-move zones.
///
/// Every arm funnels through the step driver ([`apply_hero_event_steps`]):
/// sync Events build their steps inline, hero-choice Events build a
/// [`WaitingFor::HeroEventChoice`] queue whose answers become steps at drain.
/// `fired` carries Baxter's Experiments cards that fired on THIS event (they
/// hit the graveyard after this resolution, before any copies);
/// `extra_copies` carries owed re-resolutions from an earlier drain. Returns
/// [`HeroEventProgress::Prompted`] when a hero choice (or a nested
/// replacement prompt) is now pending, [`HeroEventProgress::Settled`] when
/// the event (including all owed copies) resolved synchronously.
pub(crate) fn resolve_boss_event(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    fired: &[ObjectId],
    extra_copies: u32,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventProgress {
    let Some(name) = boss_name(state, event_id).map(str::to_string) else {
        return HeroEventProgress::Settled;
    };
    let copies = fired.len() as u32 + extra_copies;
    match name.as_str() {
        "Event: Bebop's Rampage" => apply_event_steps(
            state,
            horde,
            event_id,
            rampage_steps(state, horde, event_id),
            fired,
            copies,
            events,
        ),
        "Event: Karai's Planning" => apply_event_steps(
            state,
            horde,
            event_id,
            planning_steps(state, horde),
            fired,
            copies,
            events,
        ),
        "Event: Rocksteady's Beatdown" => apply_event_steps(
            state,
            horde,
            event_id,
            beatdown_steps(state, horde, event_id),
            fired,
            copies,
            events,
        ),
        "Event: Shredder's Challenge" => {
            let steps = challenge_steps(state, horde);
            apply_event_steps(state, horde, event_id, steps, fired, copies, events)
        }
        "Event: Slash's Smash" => apply_event_steps(
            state,
            horde,
            event_id,
            smash_steps(state, horde, event_id),
            fired,
            copies,
            events,
        ),
        "Event: Villain Scheme!" => apply_event_steps(
            state,
            horde,
            event_id,
            scheme_steps(state, horde),
            fired,
            copies,
            events,
        ),
        "Event: Baxter's Experiments" => {
            // "(Put this card beside the event deck.)" The wave left it out
            // of the graveyard; bank it here. Deduped: a copied Baxter
            // re-banks the same physical card, which must not multiply.
            if !state.baxter_set_aside.contains(&event_id) {
                state.baxter_set_aside.push(event_id);
            }
            apply_event_steps(state, horde, event_id, Vec::new(), fired, copies, events)
        }
        "Event: Chrome Dome's Overload" => start_hero_event_choice(
            state,
            horde,
            event_id,
            HeroEventKind::ChromeOverload,
            fired.to_vec(),
            copies,
            events,
        ),
        "Event: Krang's Stratagem" => start_hero_event_choice(
            state,
            horde,
            event_id,
            HeroEventKind::KrangStratagem { remaining: 2 },
            fired.to_vec(),
            copies,
            events,
        ),
        "Event: Leatherhead's Smackdown" => start_hero_event_choice(
            state,
            horde,
            event_id,
            HeroEventKind::Smackdown,
            fired.to_vec(),
            copies,
            events,
        ),
        "Event: Rat King's Revolution" => start_hero_event_choice(
            state,
            horde,
            event_id,
            HeroEventKind::RatKingRevolution,
            fired.to_vec(),
            copies,
            events,
        ),
        "Event: Savanti Romero's Curse" => start_hero_event_choice(
            state,
            horde,
            event_id,
            HeroEventKind::SavantiCurse,
            fired.to_vec(),
            copies,
            events,
        ),
        "Event: Villain Infiltration" => start_hero_event_choice(
            state,
            horde,
            event_id,
            HeroEventKind::Infiltration,
            fired.to_vec(),
            copies,
            events,
        ),
        "Event: Villain Mayhem!" => start_hero_event_choice(
            state,
            horde,
            event_id,
            HeroEventKind::VillainMayhem,
            fired.to_vec(),
            copies,
            events,
        ),
        other => {
            tracing::warn!("resolve_boss_event: unknown Event '{other}' — no effect");
            HeroEventProgress::Settled
        }
    }
}

/// Run one resolution's steps through the stepper, then its owed copies.
/// The fired Baxters ride as a trailing step so they hit the graveyard after
/// this resolution and before any copy re-dispatch.
fn apply_event_steps(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    mut steps: Vec<HeroEventApplyStep>,
    fired: &[ObjectId],
    copies: u32,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventProgress {
    // One step per Baxter: a CR 616.1 pause mid-list must park the remainder
    // as steps — the stepper can only suspend BETWEEN steps, not inside one.
    for card in fired {
        steps.push(HeroEventApplyStep::DiscardFiredBaxters { cards: vec![*card] });
    }
    apply_hero_event_steps(state, horde, event_id, steps, copies, events)
}

// ---------------------------------------------------------------------------
// Synchronous Event step builders
//
// The six always-synchronous Events build id-carrying step lists instead of
// resolving inline. A nested replacement prompt (regeneration, Library of
// Leng) can pause any batch mid-application, and the fired-Baxter discard
// plus owed copies must still run afterward — the stepper below
// (`apply_hero_event_steps`) with its parked record (`PendingHeroEvent`) is
// the single continuation point for both the sync and hero-choice paths. The
// builders preserve the old inline resolvers' targeting exactly (same
// filters, same sets); only the application path changed.
// ---------------------------------------------------------------------------

/// Battlefield objects matching `filter` in battlefield order, with
/// ability-context rebinding (`ControllerRef::Opponent` resolves against the
/// ability's controller) — the same evaluation `destroy::resolve_all` uses,
/// so builder sets equal the old inline-resolver sets by construction.
fn matching_battlefield(
    state: &GameState,
    ability: &crate::types::ability::ResolvedAbility,
    filter: &crate::types::ability::TargetFilter,
) -> Vec<ObjectId> {
    let effective = crate::game::effects::resolved_object_filter(state, ability, filter);
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| crate::game::filter::matches_target_filter(state, *id, &effective, &ctx))
        .collect()
}

/// "Bebop's Rampage deals 2 damage to each creature the heroes control." The
/// ability carries controller context for the filter only; each object's
/// damage applies as a pausable step sourced from the resolving Event.
fn rampage_steps(
    state: &GameState,
    horde: PlayerId,
    event_id: ObjectId,
) -> Vec<HeroEventApplyStep> {
    use crate::types::ability::{
        ControllerRef, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TypeFilter, TypedFilter,
    };
    let filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: Some(ControllerRef::Opponent),
        properties: Vec::new(),
    });
    let ability = ResolvedAbility::new(
        Effect::DamageAll {
            amount: QuantityExpr::Fixed { value: 2 },
            target: filter.clone(),
            player_filter: None,
            damage_source: None,
        },
        Vec::new(),
        event_id,
        horde,
    );
    matching_battlefield(state, &ability, &filter)
        .into_iter()
        .map(|card| HeroEventApplyStep::Damage { card, amount: 2 })
        .collect()
}

/// "The heroes lose 3 life and the bosses gain 3 life."
fn planning_steps(state: &GameState, horde: PlayerId) -> Vec<HeroEventApplyStep> {
    let mut steps: Vec<HeroEventApplyStep> = hero_seats_in_order(state, horde)
        .into_iter()
        .map(|hero| HeroEventApplyStep::LoseLife {
            player: hero,
            amount: 3,
        })
        .collect();
    steps.push(HeroEventApplyStep::GainLife {
        player: horde,
        amount: 3,
    });
    steps
}

/// "Destroy all creatures with flying the heroes control."
fn beatdown_steps(
    state: &GameState,
    horde: PlayerId,
    event_id: ObjectId,
) -> Vec<HeroEventApplyStep> {
    use crate::types::ability::{
        ControllerRef, Effect, FilterProp, ResolvedAbility, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::keywords::Keyword;
    let filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: Some(ControllerRef::Opponent),
        properties: vec![FilterProp::WithKeyword {
            value: Keyword::Flying,
        }],
    });
    let ability = ResolvedAbility::new(
        Effect::DestroyAll {
            target: filter.clone(),
            cant_regenerate: false,
        },
        Vec::new(),
        event_id,
        horde,
    );
    matching_battlefield(state, &ability, &filter)
        .into_iter()
        .map(|card| HeroEventApplyStep::Destroy { card })
        .collect()
}

/// "Destroy all Turtles." (No controller constraint: the Bosses control no
/// Turtles, so every Turtle is a hero's.)
fn smash_steps(state: &GameState, horde: PlayerId, event_id: ObjectId) -> Vec<HeroEventApplyStep> {
    use crate::types::ability::{Effect, ResolvedAbility, TargetFilter, TypeFilter, TypedFilter};
    let filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Subtype("Turtle".to_string())],
        controller: None,
        properties: Vec::new(),
    });
    let ability = ResolvedAbility::new(
        Effect::DestroyAll {
            target: filter.clone(),
            cant_regenerate: false,
        },
        Vec::new(),
        event_id,
        horde,
    );
    matching_battlefield(state, &ability, &filter)
        .into_iter()
        .map(|card| HeroEventApplyStep::Destroy { card })
        .collect()
}

/// "Return two Ninja cards at random from the boss graveyard to the
/// battlefield under the Boss team's control." Fewer than two available
/// returns what is there (the parenthetical); an empty graveyard is a no-op.
fn challenge_steps(state: &mut GameState, horde: PlayerId) -> Vec<HeroEventApplyStep> {
    use rand::seq::IndexedRandom; // rand 0.9: `choose_multiple` on `[T]`
    let ninjas: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == horde)
        .map(|seat| {
            seat.graveyard
                .iter()
                .copied()
                .filter(|&id| {
                    state
                        .objects
                        .get(&id)
                        .is_some_and(|o| o.card_types.subtypes.iter().any(|s| s == "Ninja"))
                })
                .collect()
        })
        .unwrap_or_default();
    // A random result (not a placement): Outcome draw.
    let mut outcome_rng = state
        .rng
        .draw(crate::types::game_state::RandomDraw::Outcome);
    ninjas
        .choose_multiple(&mut outcome_rng, 2)
        .copied()
        .map(|card| HeroEventApplyStep::ZoneMove {
            card,
            dest: Zone::Battlefield,
        })
        .collect()
}

/// "Shuffle this card and the boss graveyard into the boss library." The event
/// is already in the graveyard (moved pre-dispatch), so moving the whole
/// graveyard back and shuffling is exactly the instructed shuffle. An empty
/// graveyard builds no steps at all — not even the shuffle.
fn scheme_steps(state: &GameState, horde: PlayerId) -> Vec<HeroEventApplyStep> {
    let mut steps: Vec<HeroEventApplyStep> = state
        .players
        .iter()
        .find(|p| p.id == horde)
        .map(|seat| {
            seat.graveyard
                .iter()
                .copied()
                .map(|card| HeroEventApplyStep::ZoneMove {
                    card,
                    dest: Zone::Library,
                })
                .collect()
        })
        .unwrap_or_default();
    if !steps.is_empty() {
        steps.push(HeroEventApplyStep::ShuffleLibrary { player: horde });
    }
    steps
}

// ---------------------------------------------------------------------------
// Hero-choice Events
//
// Seven Events need hero decisions ("each hero …" sequences and "as a team"
// choices). One hero decides at a time through `WaitingFor::HeroEventChoice`,
// with the pending queue riding on the prompt itself (the vote-ledger
// precedent): `remaining_heroes` drains in seat order, `answers` accumulate,
// and the handler batch-applies them when the queue empties — so multi-hero
// Events stay simultaneous. After each application step the driver checks
// whether a nested replacement prompt took over, and if so parks the
// remainder in `PendingHeroEvent` for `drain_pending_hero_event`, which the
// post-replacement funnel calls exactly like `drain_pending_discard_batch`.
// ---------------------------------------------------------------------------

/// One resolution of work through the hero-event driver: either something now
/// needs a decision (a hero choice or a nested replacement prompt sits in
/// `state.waiting_for`) or the event — copies included — settled completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeroEventProgress {
    Prompted,
    Settled,
}

/// Post-replacement-funnel resume outcome, mirroring
/// `PendingDiscardBatchOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeroEventDrainOutcome {
    Idle,
    PausedForReplacement,
    Completed,
}

/// Living heroes in seat order. Horde format seats exactly one Horde force
/// plus heroes, so every living non-Horde seat is a hero.
fn hero_seats_in_order(state: &GameState, horde: PlayerId) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| p.id != horde && !p.is_eliminated)
        .map(|p| p.id)
        .collect()
}

/// The team representative for "as a team" choices: the first living hero in
/// seat order. One seat must click for the side; seat order is deterministic.
fn team_captain(state: &GameState, horde: PlayerId) -> Option<PlayerId> {
    hero_seats_in_order(state, horde).into_iter().next()
}

/// Battlefield creatures controlled by `hero`, in battlefield order.
fn hero_creatures(state: &GameState, hero: PlayerId) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                // CR 702.26b: phased-out permanents are treated as though
                // they don't exist; they are never eligible sacrifices.
                o.controller == hero
                    && o.card_types.core_types.contains(&CoreType::Creature)
                    && !o.is_phased_out()
            })
        })
        .collect()
}

/// Cards in `hero`'s hand, in hand order.
fn hero_hand(state: &GameState, hero: PlayerId) -> Vec<ObjectId> {
    state
        .players
        .iter()
        .find(|p| p.id == hero)
        .map(|seat| seat.hand.iter().copied().collect())
        .unwrap_or_default()
}

/// One hero's offer disposition: skip (nothing eligible — no prompt, no
/// answer), auto-resolve (the choice is forced — no prompt, record it), or
/// prompt (a real decision).
enum HeroEventOffer {
    Skip,
    Auto(Vec<ObjectId>),
    Prompt {
        cards: Vec<ObjectId>,
        count: usize,
        up_to: bool,
        /// Minimum selection when `up_to` (0 for exact prompts). Krang's
        /// Stratagem floors the current hero's contribution at what the
        /// later heroes cannot cover, so the team total cannot fall short.
        min: usize,
    },
}

/// Exact-one selection that auto-resolves when forced (0 → skip, 1 → take).
fn single_pick_offer(offer: Vec<ObjectId>) -> HeroEventOffer {
    match offer.len() {
        0 => HeroEventOffer::Skip,
        1 => HeroEventOffer::Auto(offer),
        _ => HeroEventOffer::Prompt {
            cards: offer,
            count: 1,
            up_to: false,
            min: 0,
        },
    }
}

/// Hero creatures tied for the greatest power across ALL heroes, in
/// battlefield order (Smackdown / Infiltration pool). Missing power reads as
/// 0, matching the filter evaluator's default.
fn max_power_ties(state: &GameState, horde: PlayerId) -> Vec<ObjectId> {
    let pool: Vec<ObjectId> = hero_seats_in_order(state, horde)
        .iter()
        .flat_map(|hero| hero_creatures(state, *hero))
        .collect();
    let max = pool
        .iter()
        .filter_map(|id| state.objects.get(id).and_then(|o| o.power))
        .max()
        .unwrap_or(0);
    pool.into_iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.power.unwrap_or(0) == max)
        })
        .collect()
}

/// Compute one hero's offer for the current event state. `heroes_after` is
/// the not-yet-prompted tail of the queue (Krang's Stratagem needs it: the
/// last hero holding cards covers the remainder instead of choosing freely).
fn hero_event_offer(
    state: &GameState,
    horde: PlayerId,
    hero: PlayerId,
    kind: &HeroEventKind,
    heroes_after: &[PlayerId],
) -> HeroEventOffer {
    match kind {
        HeroEventKind::ChromeOverload => {
            let tapped: Vec<ObjectId> = hero_creatures(state, hero)
                .into_iter()
                .filter(|id| state.objects.get(id).is_some_and(|o| o.tapped))
                .collect();
            single_pick_offer(tapped)
        }
        HeroEventKind::RatKingRevolution => single_pick_offer(hero_creatures(state, hero)),
        HeroEventKind::SavantiCurse => {
            let creatures = hero_creatures(state, hero);
            let want = bosses_in_command_zone(state).len();
            if creatures.is_empty() {
                HeroEventOffer::Skip
            } else if creatures.len() <= want {
                HeroEventOffer::Auto(creatures)
            } else {
                HeroEventOffer::Prompt {
                    cards: creatures,
                    count: want,
                    up_to: false,
                    min: 0,
                }
            }
        }
        HeroEventKind::KrangStratagem { remaining } => {
            // A spent remainder skips every hero left, so the queue drains
            // through to application with no further prompts.
            if *remaining == 0 {
                return HeroEventOffer::Skip;
            }
            let hand = hero_hand(state, hero);
            if hand.is_empty() {
                return HeroEventOffer::Skip;
            }
            let cover_after = heroes_after
                .iter()
                .any(|other| !hero_hand(state, *other).is_empty());
            if cover_after {
                // "As a team, the heroes discard a total of two cards" (CR
                // 701.9b): floor this hero's contribution at what the later
                // heroes cannot cover, so the team total cannot fall short
                // however the later prompts resolve. Floored at 0 (a hero
                // whose successors can cover everything may still pass), and
                // never above `remaining` (the prompt's own ceiling). No
                // soft-lock: cover_after means later_total >= 1 and
                // remaining <= 2, so min <= 1 <= hand.len() (empty hands
                // Skip above).
                let later_discardable: usize = heroes_after
                    .iter()
                    .map(|other| hero_hand(state, *other).len())
                    .sum();
                let min = remaining.saturating_sub(later_discardable);
                HeroEventOffer::Prompt {
                    cards: hand,
                    count: *remaining,
                    up_to: true,
                    min,
                }
            } else if hand.len() <= *remaining {
                HeroEventOffer::Auto(hand)
            } else {
                HeroEventOffer::Prompt {
                    cards: hand,
                    count: *remaining,
                    up_to: false,
                    min: 0,
                }
            }
        }
        // Team choices run as a one-hero queue (the captain), so the offer is
        // the team-wide pool regardless of whose seat clicks.
        HeroEventKind::Smackdown | HeroEventKind::Infiltration => {
            single_pick_offer(max_power_ties(state, horde))
        }
        HeroEventKind::VillainMayhem => {
            let pool: Vec<ObjectId> = hero_seats_in_order(state, horde)
                .iter()
                .flat_map(|other| hero_creatures(state, *other))
                .collect();
            if pool.len() <= 3 {
                HeroEventOffer::Auto(pool)
            } else {
                HeroEventOffer::Prompt {
                    cards: pool,
                    count: 3,
                    up_to: false,
                    min: 0,
                }
            }
        }
    }
}

/// Open the hero-choice queue for one Event resolution. "Each hero" Events
/// prompt every living hero in seat order; "as a team" Events prompt the
/// captain alone with the team-wide pool.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_hero_event_choice(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    kind: HeroEventKind,
    fired_baxters: Vec<ObjectId>,
    copies_pending: u32,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventProgress {
    let queue = match kind {
        HeroEventKind::Smackdown | HeroEventKind::Infiltration | HeroEventKind::VillainMayhem => {
            team_captain(state, horde).into_iter().collect()
        }
        _ => hero_seats_in_order(state, horde),
    };
    advance_hero_event_queue(
        state,
        horde,
        event_id,
        kind,
        queue,
        Vec::new(),
        fired_baxters,
        copies_pending,
        events,
    )
}

/// Drain the queue: skip/auto/prompt each hero in turn until a real decision
/// parks on the prompt or the queue empties into batch application.
#[allow(clippy::too_many_arguments)]
fn advance_hero_event_queue(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    mut kind: HeroEventKind,
    mut queue: Vec<PlayerId>,
    mut answers: Vec<HeroEventAnswer>,
    fired_baxters: Vec<ObjectId>,
    copies_pending: u32,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventProgress {
    loop {
        let Some(hero) = queue.first().copied() else {
            return finish_hero_event(
                state,
                horde,
                event_id,
                kind,
                answers,
                fired_baxters,
                copies_pending,
                events,
            );
        };
        let rest = queue[1..].to_vec();
        match hero_event_offer(state, horde, hero, &kind, &rest) {
            HeroEventOffer::Skip => {
                queue.remove(0);
            }
            HeroEventOffer::Auto(cards) => {
                if let HeroEventKind::KrangStratagem { remaining } = &mut kind {
                    *remaining = remaining.saturating_sub(cards.len());
                }
                answers.push(HeroEventAnswer { hero, cards });
                queue.remove(0);
            }
            HeroEventOffer::Prompt {
                cards,
                count,
                up_to,
                min,
            } => {
                queue.remove(0);
                state.waiting_for = WaitingFor::HeroEventChoice {
                    player: hero,
                    event: kind,
                    event_id,
                    cards,
                    count,
                    up_to,
                    min,
                    remaining_heroes: queue,
                    answers,
                    copies_pending,
                    fired_baxters,
                };
                return HeroEventProgress::Prompted;
            }
        }
    }
}

/// Record one hero's `SelectCards` answer and continue the queue. Called from
/// the `GameAction` handler; validation (offer membership, exact/`up_to`
/// count) happens before this runs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn answer_hero_event_choice(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    mut kind: HeroEventKind,
    hero: PlayerId,
    cards: Vec<ObjectId>,
    remaining_heroes: Vec<PlayerId>,
    mut answers: Vec<HeroEventAnswer>,
    fired_baxters: Vec<ObjectId>,
    copies_pending: u32,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventProgress {
    if let HeroEventKind::KrangStratagem { remaining } = &mut kind {
        *remaining = remaining.saturating_sub(cards.len());
    }
    answers.push(HeroEventAnswer { hero, cards });
    advance_hero_event_queue(
        state,
        horde,
        event_id,
        kind,
        remaining_heroes,
        answers,
        fired_baxters,
        copies_pending,
        events,
    )
}

/// The queue drained: turn answers into steps, trail the fired-Baxter
/// discard, and apply the batch through the stepper.
#[allow(clippy::too_many_arguments)]
fn finish_hero_event(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    kind: HeroEventKind,
    answers: Vec<HeroEventAnswer>,
    fired_baxters: Vec<ObjectId>,
    copies_pending: u32,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventProgress {
    let mut steps = build_answer_steps(state, horde, &kind, &answers);
    // One step per Baxter (see `apply_event_steps`): pauses park BETWEEN steps.
    for card in fired_baxters {
        steps.push(HeroEventApplyStep::DiscardFiredBaxters { cards: vec![card] });
    }
    apply_hero_event_steps(state, horde, event_id, steps, copies_pending, events)
}

/// Turn recorded answers into application steps. Amounts read here (Smackdown
/// power, Rat King boss count) are last-known-information at drain time: no
/// priority passes between the choice and the batch, so nothing moves under
/// the answers.
fn build_answer_steps(
    state: &GameState,
    horde: PlayerId,
    kind: &HeroEventKind,
    answers: &[HeroEventAnswer],
) -> Vec<HeroEventApplyStep> {
    let mut steps = Vec::new();
    match kind {
        HeroEventKind::ChromeOverload => {
            for answer in answers {
                for card in &answer.cards {
                    steps.push(HeroEventApplyStep::Sacrifice {
                        hero: answer.hero,
                        card: *card,
                    });
                }
            }
        }
        HeroEventKind::KrangStratagem { .. } => {
            for answer in answers {
                for card in &answer.cards {
                    steps.push(HeroEventApplyStep::Discard {
                        hero: answer.hero,
                        card: *card,
                    });
                }
            }
        }
        HeroEventKind::Smackdown => {
            if let Some(card) = answers.iter().flat_map(|a| &a.cards).next() {
                let power = state
                    .objects
                    .get(card)
                    .and_then(|o| o.power)
                    .unwrap_or(0)
                    .max(0);
                steps.push(HeroEventApplyStep::Destroy { card: *card });
                steps.push(HeroEventApplyStep::LoseLife {
                    player: horde,
                    amount: power,
                });
            }
        }
        HeroEventKind::RatKingRevolution => {
            let bosses = bosses_in_command_zone(state).len() as u32;
            for answer in answers {
                for card in &answer.cards {
                    steps.push(HeroEventApplyStep::Tap { card: *card });
                    steps.push(HeroEventApplyStep::AddCounters {
                        card: *card,
                        counter: crate::types::counter::CounterType::Stun,
                        count: bosses,
                    });
                }
            }
        }
        HeroEventKind::SavantiCurse => {
            for answer in answers {
                for card in &answer.cards {
                    steps.push(HeroEventApplyStep::Bounce { card: *card });
                }
            }
        }
        HeroEventKind::Infiltration => {
            if let Some(card) = answers.iter().flat_map(|a| &a.cards).next() {
                steps.push(HeroEventApplyStep::Destroy { card: *card });
            }
        }
        HeroEventKind::VillainMayhem => {
            let chosen: Vec<ObjectId> = answers
                .iter()
                .flat_map(|a| a.cards.iter().copied())
                .collect();
            for hero in hero_seats_in_order(state, horde) {
                for card in hero_creatures(state, hero) {
                    if !chosen.contains(&card) {
                        steps.push(HeroEventApplyStep::Destroy { card });
                    }
                }
            }
        }
    }
    steps
}

/// Apply a step batch in order. After each step the driver checks whether the
/// waiting variant moved: a nested replacement prompt (regeneration, Library
/// of Leng) that took over mid-batch parks the remainder in
/// `PendingHeroEvent` for `drain_pending_hero_event`. Destroys that complete
/// unpaused mark as one simultaneous group (the `destroy_all` Blood Artist
/// handling); a parked call marks nothing, exactly like `destroy_all`'s pause
/// path.
fn apply_hero_event_steps(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    steps: Vec<HeroEventApplyStep>,
    copies_pending: u32,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventProgress {
    for (index, step) in steps.iter().enumerate() {
        // Stepping runs under Priority (fresh dispatch, funnel resume), the
        // consumed HeroEventChoice prompt (queue drain — still set), or
        // whatever state a trigger chain dispatches from (Savanti's
        // mid-combat "play a card") — so the park signal is a CHANGED variant,
        // not a fixed set. A step that parked a nested replacement prompt
        // (regeneration, Library of Leng) moves the variant; an untouched
        // state continues regardless of which variant it holds.
        let before = std::mem::discriminant(&state.waiting_for);
        apply_one_hero_event_step(state, horde, event_id, step, events);
        if std::mem::discriminant(&state.waiting_for) != before
            && !matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            state.pending_hero_event = Some(Box::new(PendingHeroEvent {
                event_id,
                horde,
                steps: steps[index + 1..].to_vec(),
                copies_pending,
            }));
            return HeroEventProgress::Prompted;
        }
    }
    let destroyed: Vec<ObjectId> = steps
        .iter()
        .filter_map(|step| match step {
            HeroEventApplyStep::Destroy { card } => Some(*card),
            _ => None,
        })
        .collect();
    if !destroyed.is_empty() {
        crate::game::zones::mark_simultaneous_departures(
            events,
            &crate::game::zones::departed_subset(state, &destroyed),
        );
    }
    if copies_pending > 0 {
        // Copies re-resolve the whole Event (fresh choices included — Baxter's
        // "resolve it again"), chained through the same driver.
        resolve_boss_event(state, horde, event_id, &[], copies_pending - 1, events)
    } else {
        HeroEventProgress::Settled
    }
}

/// One step through its existing single-object applier. Every applier is the
/// same function the equivalent spell instruction uses, so replacements,
/// restrictions, and triggers behave identically; outcomes fold to unit
/// because the stepper's only signal is whether a prompt arose.
fn apply_one_hero_event_step(
    state: &mut GameState,
    horde: PlayerId,
    event_id: ObjectId,
    step: &HeroEventApplyStep,
    events: &mut Vec<crate::types::events::GameEvent>,
) {
    match step {
        HeroEventApplyStep::Sacrifice { hero, card } => {
            let _ = crate::game::sacrifice::sacrifice_permanent(state, *card, *hero, events);
        }
        HeroEventApplyStep::Discard { hero, card } => {
            let _ = crate::game::effects::discard::discard_caused_by_effect_with_source(
                state,
                *card,
                *hero,
                Some(event_id),
                events,
            );
        }
        HeroEventApplyStep::Destroy { card } => {
            let _ = crate::game::effects::destroy::destroy_single_object(
                state, *card, event_id, false, events,
            );
        }
        HeroEventApplyStep::LoseLife { player, amount } => {
            use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility};
            // `target: None` loses for the CONTROLLER, so each step resolves
            // with the loser as controller — the same per-hero rebind the old
            // `player_scope: Opponent` chain performed, one hero at a time.
            let ability = ResolvedAbility::new(
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: *amount },
                    target: None,
                },
                Vec::new(),
                event_id,
                *player,
            );
            let _ = crate::game::effects::life::resolve_lose(state, &ability, events);
        }
        HeroEventApplyStep::GainLife { player, amount } => {
            use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
            let ability = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: *amount },
                    player: TargetFilter::Controller,
                },
                Vec::new(),
                event_id,
                *player,
            );
            let _ = crate::game::effects::life::resolve_gain(state, &ability, events);
        }
        HeroEventApplyStep::Damage { card, amount } => {
            use crate::game::effects::deal_damage::{apply_damage_to_target, DamageContext};
            use crate::types::ability::TargetRef;
            if let Some(ctx) = DamageContext::from_source(state, event_id) {
                let _ = apply_damage_to_target(
                    state,
                    &ctx,
                    TargetRef::Object(*card),
                    *amount,
                    false,
                    events,
                );
            }
        }
        HeroEventApplyStep::ZoneMove { card, dest } => {
            // The Event-caused move runs the full zone-change pipeline so
            // replacements consult (entry replacements for Battlefield
            // reanimation, graveyard replacements for discards). A CR 616.1
            // choice sets the replacement prompt; the stepper's park detector
            // suspends the remaining steps and the post-replacement funnel
            // drains them. The Event attributes the move.
            let Some(from) = state.objects.get(card).map(|o| o.zone) else {
                // Built from live objects; a missing card means an earlier
                // step consumed it — nothing to move.
                return;
            };
            if from == *dest {
                // Already there: a same-zone "move" is a non-event — no
                // prompt, no delivery.
                return;
            }
            match execute_zone_move(
                state,
                *card,
                from,
                *dest,
                event_id,
                None,
                false,
                crate::types::zones::EtbTapState::Unspecified,
                false,
                None,
                &[],
                None,
                false,
                None,
                None,
                events,
            ) {
                ZoneMoveResult::Done => {}
                ZoneMoveResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                }
                // ZoneMove producers move non-Aura cards (Ninjas, Events);
                // no Aura attachment choice can arise.
                ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    debug_assert!(false, "event ZoneMove cannot need an attach choice");
                }
            }
        }
        HeroEventApplyStep::ShuffleLibrary { player } => {
            let GameState { players, rng, .. } = state;
            if let Some(seat) = players.iter_mut().find(|p| p.id == *player) {
                let placement = rng.draw(crate::types::game_state::RandomDraw::Placement);
                crate::util::im_ext::shuffle_vector(&mut seat.library, placement);
            }
        }
        HeroEventApplyStep::DiscardFiredBaxters { cards } => {
            // Single-card invariant (both producers emit one step per
            // Baxter): a pause must leave the remainder as PARKED steps, and
            // cards inside this step would be dropped instead.
            debug_assert!(
                cards.len() <= 1,
                "DiscardFiredBaxters carries at most one card per step"
            );
            for card in cards {
                let Some(from) = state.objects.get(card).map(|o| o.zone) else {
                    // A banked Baxter that left exile under an earlier step
                    // has nothing to discard.
                    continue;
                };
                if from == Zone::Graveyard {
                    // Already discarded (e.g. a redirected bank): a same-zone
                    // "move" is a non-event — no prompt, no delivery.
                    continue;
                }
                match execute_zone_move(
                    state,
                    *card,
                    from,
                    Zone::Graveyard,
                    event_id,
                    None,
                    false,
                    crate::types::zones::EtbTapState::Unspecified,
                    false,
                    None,
                    &[],
                    None,
                    false,
                    None,
                    None,
                    events,
                ) {
                    ZoneMoveResult::Done => {}
                    ZoneMoveResult::NeedsChoice(player) => {
                        state.waiting_for =
                            crate::game::replacement::replacement_choice_waiting_for(player, state);
                        break;
                    }
                    // Graveyard destination: no Aura attachment choice can arise.
                    ZoneMoveResult::NeedsAuraAttachmentChoice => {
                        debug_assert!(false, "baxter discard cannot need an attach choice");
                        break;
                    }
                }
            }
        }
        HeroEventApplyStep::Tap { card } => {
            let _ =
                crate::game::effects::tap_untap::process_one_tap(state, *card, event_id, events);
        }
        HeroEventApplyStep::AddCounters {
            card,
            counter,
            count,
        } => {
            let _ = crate::game::effects::counters::add_counter_with_replacement(
                state,
                horde,
                *card,
                counter.clone(),
                *count,
                events,
            );
        }
        HeroEventApplyStep::Bounce { card } => {
            use crate::types::ability::{
                BounceSelection, ControllerRef, Effect, ResolvedAbility, TargetFilter, TargetRef,
                TypeFilter, TypedFilter,
            };
            // The filter describes the card for legality; the pre-targeted id
            // selects it. `destination: None` is owner's hand.
            let ability = ResolvedAbility::new(
                Effect::Bounce {
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: Some(ControllerRef::Opponent),
                        properties: Vec::new(),
                    }),
                    destination: None,
                    selection: BounceSelection::Targeted,
                },
                vec![TargetRef::Object(*card)],
                event_id,
                horde,
            );
            let _ = crate::game::effects::bounce::resolve(state, &ability, events);
        }
    }
}

/// Resume a hero-event batch parked by a nested replacement prompt, through
/// the post-replacement funnel — the `drain_pending_discard_batch` shape:
/// re-pauses re-park through the same hook, completion drains like any batch.
pub(crate) fn drain_pending_hero_event(
    state: &mut GameState,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> HeroEventDrainOutcome {
    let Some(pending) = state.pending_hero_event.take() else {
        return HeroEventDrainOutcome::Idle;
    };
    match apply_hero_event_steps(
        state,
        pending.horde,
        pending.event_id,
        pending.steps,
        pending.copies_pending,
        events,
    ) {
        HeroEventProgress::Prompted => HeroEventDrainOutcome::PausedForReplacement,
        HeroEventProgress::Settled => HeroEventDrainOutcome::Completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::card_db::CardDatabase;
    use crate::game::deck_loading::{load_deck_into_state, load_horde_library, DeckPayload};
    use crate::game::token_presets::known_token_preset_by_id;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::format::{ChallengeDeck, FormatConfig};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::phase::Phase;

    /// Full Turtle Team-Up game with `heroes` hero seats: seeded life, the
    /// Horde emblem, the Boss pile + stage 1 (the `load_deck_into_state`
    /// setup hook), and the 27-card event library. No card DB needed — the
    /// whole enemy deck is token-layout.
    fn turtle_teamup_game(heroes: u8) -> (GameState, PlayerId) {
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::TurtleTeamUpHorde.default_ruleset()),
            heroes + 1,
            42,
        );
        let horde = state.format_config.archenemy_player().expect("Horde seat");
        load_deck_into_state(&mut state, &DeckPayload::default());
        load_horde_library(
            &mut state,
            &CardDatabase::default(),
            horde,
            ChallengeDeck::TurtleTeamUpHorde,
        );
        (state, horde)
    }

    fn hero_seat(state: &GameState, horde: PlayerId) -> PlayerId {
        state
            .players
            .iter()
            .map(|p| p.id)
            .find(|&id| id != horde)
            .expect("a hero seat")
    }

    fn seat_life(state: &GameState, seat: PlayerId) -> i32 {
        state
            .players
            .iter()
            .find(|p| p.id == seat)
            .expect("seat")
            .life
    }

    fn boss_library_len(state: &GameState, horde: PlayerId) -> usize {
        state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .library
            .len()
    }

    /// Leatherhead's "gain 5 life for each boss in play" applies synchronously
    /// at deploy, so the expected life after a deploy is the stage value plus
    /// the gain when he is among the deployed Bosses.
    fn expected_boss_life(state: &GameState, stage_value: i32) -> i32 {
        let deployed = bosses_in_command_zone(state);
        let leatherhead = deployed
            .iter()
            .any(|&id| boss_name(state, id) == Some("Leatherhead, Sewer Gator"));
        stage_value
            + if leatherhead {
                5 * deployed.len() as i32
            } else {
                0
            }
    }

    #[test]
    fn setup_deploys_stage_1_with_seeded_life() {
        let (state, horde) = turtle_teamup_game(2);
        assert_eq!(state.boss_stage, 1, "setup deploys stage 1");
        assert_eq!(
            bosses_in_command_zone(&state).len(),
            1,
            "stage 1 deploys one Boss"
        );
        assert_eq!(
            state.boss_deck.len(),
            10,
            "11 Bosses minus the deployed one stay in the pile"
        );
        assert_eq!(
            seat_life(&state, horde),
            expected_boss_life(&state, 20),
            "2 heroes → 20 Boss life (+ Leatherhead gain when he deploys)"
        );
        assert_eq!(
            boss_library_len(&state, horde),
            27,
            "the event deck loads as the Boss library"
        );
        assert!(
            !crate::game::horde::player_has_no_life_total(&state, horde),
            "a boss-deck game keeps a real Boss life total"
        );
    }

    /// Challenge-deck library loaded raw — no setup hook, no deploy — so the
    /// pile holds the full deck and the id→card mapping is comparable across
    /// seeds.
    fn horde_library_state_for_deck(
        deck: ChallengeDeck,
        seed: u64,
    ) -> (GameState, PlayerId, PlayerId) {
        let mut state = GameState::new(FormatConfig::horde(deck.default_ruleset()), 3, seed);
        let horde = state.format_config.archenemy_player().expect("Horde seat");
        load_horde_library(
            &mut state,
            crate::test_support::shared_card_db(),
            horde,
            deck,
        );
        let hero = hero_seat(&state, horde);
        (state, horde, hero)
    }

    /// Cyberman library (100 nontokens + 200 tokens) loaded raw.
    fn cyberman_library_state(seed: u64) -> (GameState, PlayerId, PlayerId) {
        horde_library_state_for_deck(ChallengeDeck::CybermanHorde, seed)
    }

    /// Turtle Boss pile (11 Bosses) loaded raw — no deploy — for the same
    /// cross-seed comparison. Token-layout: no card DB needed.
    fn turtle_boss_pile_state(seed: u64) -> (GameState, PlayerId, PlayerId) {
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::TurtleTeamUpHorde.default_ruleset()),
            3,
            seed,
        );
        let horde = state.format_config.archenemy_player().expect("Horde seat");
        load_boss_deck(&mut state, horde);
        let hero = hero_seat(&state, horde);
        (state, horde, hero)
    }

    /// The pile's id→card mapping in ID order: the allocation order made
    /// observable. Pre-fix this is the deck-list order under every seed.
    fn pile_id_name_map(state: &GameState, pile: &[ObjectId]) -> Vec<(ObjectId, String)> {
        let mut entries: Vec<(ObjectId, String)> = pile
            .iter()
            .map(|id| {
                (
                    *id,
                    state
                        .objects
                        .get(id)
                        .map(|o| o.name.clone())
                        .unwrap_or_default(),
                )
            })
            .collect();
        entries.sort();
        entries
    }

    /// R6-#5 — Horde library IDs must not encode deck-list order: the same
    /// 300-card Cyberman deck under two seeds must map IDs to different
    /// cards. Pre-fix, allocation ran in list order (100 nontokens then 200
    /// tokens) under every seed, and the viewer's pile-order projection
    /// exposes pile order — sorting by ID recovered the assembly order.
    #[test]
    fn horde_library_allocation_order_is_seed_dependent() {
        let (state_a, horde_a, hero_a) = cyberman_library_state(42);
        let (state_b, horde_b, _) = cyberman_library_state(43);
        let pile_a: Vec<ObjectId> = state_a
            .players
            .iter()
            .find(|p| p.id == horde_a)
            .expect("horde")
            .library
            .iter()
            .copied()
            .collect();
        let pile_b: Vec<ObjectId> = state_b
            .players
            .iter()
            .find(|p| p.id == horde_b)
            .expect("horde")
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(pile_a.len(), 300, "Cyberman library holds 100 + 200");
        assert_eq!(pile_b.len(), 300, "Cyberman library holds 100 + 200");
        let map_a = pile_id_name_map(&state_a, &pile_a);
        let (state_a2, horde_a2, _) = cyberman_library_state(42);
        let pile_a2: Vec<ObjectId> = state_a2
            .players
            .iter()
            .find(|p| p.id == horde_a2)
            .expect("horde")
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(
            pile_id_name_map(&state_a2, &pile_a2),
            map_a,
            "the same seed reproduces the same allocation (reproducibility)"
        );
        assert_ne!(
            map_a,
            pile_id_name_map(&state_b, &pile_b),
            "allocation order must depend on the seed, not the deck list"
        );
        // The combined shuffle interleaves nontokens and tokens in ID order
        // (pre-fix: the first 100 IDs are all nontokens).
        let token_flags: Vec<bool> = map_a
            .iter()
            .map(|(id, _)| state_a.objects.get(id).is_some_and(|o| o.is_token))
            .collect();
        assert!(
            token_flags[..100].iter().any(|t| *t) && token_flags[100..].iter().any(|t| !t),
            "ID order must mix nontokens and tokens, not list them group by group"
        );
        // Viewer reach-guard: the hero's snapshot really does project the
        // pile ID order (the surface the fixed mapping would have exploited).
        let viewer = crate::game::visibility::filter_state_for_viewer(&state_a, hero_a);
        let seen: Vec<ObjectId> = viewer
            .players
            .iter()
            .find(|p| p.id == horde_a)
            .expect("horde in view")
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(seen, pile_a, "the viewer sees the pile ID order");
        assert!(
            seen.iter().all(|id| viewer
                .objects
                .get(id)
                .is_some_and(|o| o.name == "Hidden Card")),
            "pile identities stay redacted in the view"
        );
    }

    /// R6-#6 — a hidden Horde library card must not leak token metadata in
    /// viewer states: `is_token` and `is_horde_deck_token` both redact.
    /// Exercised on the token-bearing Cyberman deck (100 cards + 200
    /// tokens), asserting both the projected structs and their serialized
    /// form (all three marker keys skip-serialize when cleared).
    #[test]
    fn hidden_horde_library_redacts_token_metadata() {
        let (state, horde, hero) = horde_library_state_for_deck(ChallengeDeck::CybermanHorde, 42);
        let pile: Vec<ObjectId> = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde")
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(pile.len(), 300, "Cyberman library holds 100 + 200");
        // Reach guards: authoritatively, the pile really does mix tokens in.
        let token_count = pile
            .iter()
            .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
            .count();
        assert_eq!(
            token_count, 200,
            "200 pile cards are tokens authoritatively"
        );
        assert!(
            pile.iter().all(|id| {
                let obj = state.objects.get(id).expect("pile object");
                obj.is_horde_deck_token == obj.is_token
            }),
            "authoritatively, exactly the tokens carry the horde-deck flag"
        );

        let viewer = crate::game::visibility::filter_state_for_viewer(&state, hero);
        for id in &pile {
            let obj = viewer.objects.get(id).expect("pile object projected");
            assert!(
                !obj.is_token,
                "hidden pile card {id:?} must not show token-ness"
            );
            assert!(
                !obj.is_horde_deck_token,
                "hidden pile card {id:?} must not show the horde-deck flag"
            );
            assert!(
                obj.horde_library_rarity.is_none(),
                "Cyberman pins no rarity; the view must not invent one"
            );
            let json = serde_json::to_string(obj).expect("pile object serializes");
            for key in [
                "\"is_token\":",
                "\"is_horde_deck_token\":",
                "\"horde_library_rarity\":",
            ] {
                assert!(
                    !json.contains(key),
                    "serialized hidden pile card {id:?} must omit {key}"
                );
            }
        }
    }

    /// R6-#6 — the rarity-terminated D&D deck gets the same guarantee plus
    /// its pinned per-card rarities: all 62 nontokens pin a rarity
    /// authoritatively (the wave rule reads it), and the view redacts every
    /// one alongside the token flags on the 38 Ooze tokens.
    #[test]
    fn hidden_horde_library_redacts_pinned_rarity() {
        let (state, horde, hero) = horde_library_state_for_deck(ChallengeDeck::DndHorde, 42);
        let pile: Vec<ObjectId> = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde")
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(pile.len(), 100, "D&D library holds 62 + 38");
        let pinned = pile
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.horde_library_rarity.is_some())
            })
            .count();
        assert_eq!(pinned, 62, "all 62 nontokens pin a rarity authoritatively");
        let token_count = pile
            .iter()
            .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
            .count();
        assert_eq!(token_count, 38, "38 pile cards are tokens authoritatively");

        let viewer = crate::game::visibility::filter_state_for_viewer(&state, hero);
        for id in &pile {
            let obj = viewer.objects.get(id).expect("pile object projected");
            assert!(
                !obj.is_token,
                "hidden pile card {id:?} must not show token-ness"
            );
            assert!(
                !obj.is_horde_deck_token,
                "hidden pile card {id:?} must not show the horde-deck flag"
            );
            assert!(
                obj.horde_library_rarity.is_none(),
                "hidden pile card {id:?} must not show its pinned rarity"
            );
            let json = serde_json::to_string(obj).expect("pile object serializes");
            for key in [
                "\"is_token\":",
                "\"is_horde_deck_token\":",
                "\"horde_library_rarity\":",
            ] {
                assert!(
                    !json.contains(key),
                    "serialized hidden pile card {id:?} must omit {key}"
                );
            }
        }
    }

    /// R6-#5 — the Boss pile gets the same guarantee: the 11-Boss Turtle
    /// pile under two seeds must map IDs to different Bosses.
    #[test]
    fn boss_pile_allocation_order_is_seed_dependent() {
        let (state_a, _, hero_a) = turtle_boss_pile_state(42);
        let (state_b, _, _) = turtle_boss_pile_state(43);
        let pile_a: Vec<ObjectId> = state_a.boss_deck.iter().copied().collect();
        let pile_b: Vec<ObjectId> = state_b.boss_deck.iter().copied().collect();
        assert_eq!(pile_a.len(), 11, "the Boss pile holds 11 Bosses");
        assert_eq!(pile_b.len(), 11, "the Boss pile holds 11 Bosses");
        let map_a = pile_id_name_map(&state_a, &pile_a);
        let (state_a2, _, _) = turtle_boss_pile_state(42);
        let pile_a2: Vec<ObjectId> = state_a2.boss_deck.iter().copied().collect();
        assert_eq!(
            pile_id_name_map(&state_a2, &pile_a2),
            map_a,
            "the same seed reproduces the same allocation (reproducibility)"
        );
        assert_ne!(
            map_a,
            pile_id_name_map(&state_b, &pile_b),
            "allocation order must depend on the seed, not the deck list"
        );
        let viewer = crate::game::visibility::filter_state_for_viewer(&state_a, hero_a);
        let seen: Vec<ObjectId> = viewer.boss_deck.iter().copied().collect();
        assert_eq!(seen, pile_a, "the viewer sees the pile ID order");
        assert!(
            seen.iter().all(|id| viewer
                .objects
                .get(id)
                .is_some_and(|o| o.name == "Hidden Card")),
            "pile identities stay redacted in the view"
        );
    }

    #[test]
    fn wave_seed_scales_with_heroes_and_krang() {
        // The seed-42 pile order is not pinned, so the Krang term is read
        // live: base 2 (+ survivor bonus at 3+ heroes) + Krang when deployed.
        let (mut two, horde) = turtle_teamup_game(2);
        two.active_player = horde;
        let krang = u32::from(is_krang_deployed(&two));
        crate::game::horde::begin_wave(&mut two, &mut Vec::new());
        assert_eq!(two.horde_wave_remaining, 2 + krang);

        let (mut three, horde) = turtle_teamup_game(3);
        three.active_player = horde;
        let krang = u32::from(is_krang_deployed(&three));
        crate::game::horde::begin_wave(&mut three, &mut Vec::new());
        assert_eq!(three.horde_wave_remaining, 3 + krang);

        // The Krang delta itself, deterministically: force him away, then back.
        // (The bonus reads deployed names, so renaming exercises the seam.)
        let boss = bosses_in_command_zone(&three)[0];
        three.objects.get_mut(&boss).expect("deployed Boss").name =
            "Slash, the Dark Mirror".to_string();
        assert!(!is_krang_deployed(&three));
        crate::game::horde::begin_wave(&mut three, &mut Vec::new());
        assert_eq!(three.horde_wave_remaining, 3);
        three.objects.get_mut(&boss).expect("deployed Boss").name =
            "Krang, Dimension X Overlord".to_string();
        assert!(is_krang_deployed(&three));
        crate::game::horde::begin_wave(&mut three, &mut Vec::new());
        assert_eq!(three.horde_wave_remaining, 4);
    }

    #[test]
    fn stage_clear_advances_and_resets_life() {
        let (mut state, horde) = turtle_teamup_game(2);
        let stage_1 = bosses_in_command_zone(&state);
        state
            .players
            .iter_mut()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .life = 0;
        crate::game::sba::check_state_based_actions(&mut state, &mut Vec::new());

        assert_eq!(
            state.boss_stage, 2,
            "zero Boss life clears a non-final stage"
        );
        assert_eq!(
            seat_life(&state, horde),
            expected_boss_life(&state, 20),
            "life resets to the stage value on a clear"
        );
        assert_eq!(
            bosses_in_command_zone(&state).len(),
            2,
            "stage 2 deploys two Bosses"
        );
        assert_eq!(state.boss_deck.len(), 8, "1 + 2 Bosses deployed");
        for id in stage_1 {
            assert_eq!(
                state.objects.get(&id).expect("cleared Boss").zone,
                Zone::Exile,
                "cleared Bosses are exiled, not graveyarded — Villain Scheme! \
                 must never reshuffle them"
            );
        }
        assert!(
            !state
                .players
                .iter()
                .find(|p| p.id == horde)
                .expect("horde seat")
                .is_eliminated,
            "a cleared stage is not an elimination"
        );
        assert!(
            !crate::game::horde::horde_is_defeated(&state),
            "the Horde is not defeated mid-fight"
        );
    }

    /// Chrome Dome's "+1/+0 for each boss in play" is a LIVE count: rigged
    /// first in the two-boss stage 2, the grafted static grants +2/+0 once
    /// the second Boss deploys (no re-graft), and the bonus is gone after
    /// the next clear exiles Chrome Dome with its stage.
    ///
    /// Revert guard: baking the graft-time count grants +1/+0 (Chrome Dome
    /// grafts first, when only one Boss is deployed) — the +2 assertion flips.
    #[test]
    fn chrome_dome_bonus_tracks_deployed_boss_count_live() {
        let (mut state, horde) = turtle_teamup_game(2);
        // Rig stage 2's first deploy to Chrome Dome (stage sizes are [1,2,3];
        // stage 1 already deployed one Boss at setup).
        let chrome = state
            .boss_deck
            .iter()
            .copied()
            .find(|id| boss_name(&state, *id) == Some("Chrome Dome, Cyber-Ninja"))
            .expect("the Boss pile holds Chrome Dome");
        let mut pile: Vec<ObjectId> = state.boss_deck.iter().copied().collect();
        pile.retain(|id| *id != chrome);
        pile.insert(0, chrome);
        state.boss_deck = pile.into_iter().collect();

        let ninja = mk_creature(&mut state, horde, "Foot Ninja", &[], &["Ninja"]);

        // Clear stage 1 through the real SBA loop → stage 2 deploys
        // [Chrome Dome, next], Chrome Dome grafted first at count 1.
        state
            .players
            .iter_mut()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .life = 0;
        crate::game::sba::check_state_based_actions(&mut state, &mut Vec::new());
        assert_eq!(state.boss_stage, 2, "the clear must advance to stage 2");
        let deployed = bosses_in_command_zone(&state);
        assert_eq!(deployed.len(), 2, "stage 2 deploys two Bosses");
        assert_eq!(
            deployed[0], chrome,
            "Chrome Dome deploys (and grafts) first"
        );

        // The rigged deploy path marks nothing dirty (production zone moves
        // do), so force the full pass explicitly.
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        let ninja_obj = state.objects.get(&ninja).expect("ninja");
        assert_eq!(
            (ninja_obj.power, ninja_obj.toughness),
            (Some(4), Some(2)),
            "a 2/2 Ninja gets +2/+0 with two bosses deployed — the graft-time \
             count (1) must not stick"
        );

        // Clear stage 2 → stage 3 deploys; Chrome Dome leaves with its stage
        // and the bonus drops to nothing (no stale graft).
        state
            .players
            .iter_mut()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .life = 0;
        crate::game::sba::check_state_based_actions(&mut state, &mut Vec::new());
        assert_eq!(state.boss_stage, 3, "the clear must advance to stage 3");
        assert!(
            !bosses_in_command_zone(&state).contains(&chrome),
            "Chrome Dome leaves play with its cleared stage"
        );
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        let ninja_obj = state.objects.get(&ninja).expect("ninja");
        assert_eq!(
            (ninja_obj.power, ninja_obj.toughness),
            (Some(2), Some(2)),
            "with Chrome Dome gone the Ninja falls back to base P/T"
        );
    }

    #[test]
    fn final_stage_loss_defeats_the_horde() {
        let (mut state, horde) = turtle_teamup_game(2);
        for expected_stage in [2u8, 3u8] {
            state
                .players
                .iter_mut()
                .find(|p| p.id == horde)
                .expect("horde seat")
                .life = 0;
            crate::game::sba::check_state_based_actions(&mut state, &mut Vec::new());
            assert_eq!(state.boss_stage, expected_stage);
        }
        assert_eq!(
            bosses_in_command_zone(&state).len(),
            3,
            "stage 3 deploys three Bosses"
        );
        assert_eq!(state.boss_deck.len(), 5, "1 + 2 + 3 Bosses deployed");

        // Fell the final stage: the normal CR 704.5a loss proceeds.
        state
            .players
            .iter_mut()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .life = 0;
        crate::game::sba::check_state_based_actions(&mut state, &mut Vec::new());
        assert!(
            state
                .players
                .iter()
                .find(|p| p.id == horde)
                .expect("horde seat")
                .is_eliminated,
            "the final-stage Boss at 0 life loses"
        );
        assert!(
            crate::game::horde::horde_is_defeated(&state),
            "the fallen final Boss defeats the Horde — the heroes win"
        );
    }

    #[test]
    fn empty_library_does_not_defeat_a_boss_game() {
        let (mut state, horde) = turtle_teamup_game(2);
        let ids: Vec<ObjectId> = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .library
            .iter()
            .copied()
            .collect();
        for id in ids {
            move_to_zone(&mut state, id, Zone::Graveyard, &mut Vec::new());
        }
        assert!(boss_library_len(&state, horde) == 0);
        assert!(
            !crate::game::horde::horde_is_defeated(&state),
            "an emptied event deck is not a loss — only the final Boss falling is"
        );
    }

    /// A synthetic hero creature for Event probes and reveal checks.
    fn mk_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        keywords: &[Keyword],
        subtypes: &[&str],
    ) -> ObjectId {
        let id = create_object(state, CardId(0), owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).expect("just created");
        obj.controller = owner;
        obj.card_types.core_types.push(CoreType::Creature);
        for subtype in subtypes {
            obj.card_types.subtypes.push((*subtype).to_string());
        }
        obj.keywords.extend(keywords.iter().cloned());
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        id
    }

    #[test]
    fn revealed_ninjas_gain_their_static_lines() {
        let (mut state, horde) = turtle_teamup_game(2);
        let library: Vec<ObjectId> = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .library
            .iter()
            .copied()
            .collect();
        let find_in_library = |state: &GameState, name: &str| {
            library
                .iter()
                .copied()
                .find(|id| state.objects.get(id).is_some_and(|o| o.name == name))
                .unwrap_or_else(|| panic!("event deck must contain {name}"))
        };
        let enforcer = find_in_library(&state, "Foot Enforcer");
        let disciple = find_in_library(&state, "Foot Disciple");
        // Rig the top of the event deck: the Enforcer reveals first.
        {
            let seat = state
                .players
                .iter_mut()
                .find(|p| p.id == horde)
                .expect("horde seat");
            seat.library.retain(|id| *id != enforcer && *id != disciple);
            seat.library.push_front(disciple);
            seat.library.push_front(enforcer);
        }
        state.phase = Phase::PreCombatMain;
        state.active_player = horde;
        state.horde_wave_remaining = 2;
        let waiting = crate::game::horde::maybe_reveal_next(&mut state, &mut Vec::new());
        assert!(
            waiting.is_none(),
            "synchronous token reveals do not pause the wave"
        );
        assert_eq!(state.horde_wave_remaining, 0);

        // The reveal consumed the placeholders and entered fresh tokens.
        let find_boss_creature = |state: &GameState, name: &str| {
            state
                .battlefield
                .iter()
                .copied()
                .find(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|o| o.name == name && o.controller == horde)
                })
                .unwrap_or_else(|| panic!("{name} must have entered under the Boss"))
        };
        let enforcer = find_boss_creature(&state, "Foot Enforcer");
        let disciple = find_boss_creature(&state, "Foot Disciple");
        let modes = |state: &GameState, id: ObjectId| {
            state
                .objects
                .get(&id)
                .expect("revealed Ninja")
                .static_definitions
                .iter_unchecked()
                .map(|d| std::mem::discriminant(&d.mode))
                .collect::<Vec<_>>()
        };
        use crate::types::statics::StaticMode;
        let cant_block = std::mem::discriminant(&StaticMode::CantBlock);
        let capped = std::mem::discriminant(&StaticMode::CantBeBlockedByMoreThan { max: 1 });
        assert!(
            modes(&state, disciple).contains(&cant_block),
            "a revealed Disciple can't block"
        );
        assert!(
            modes(&state, enforcer).contains(&cant_block),
            "a revealed Enforcer can't block"
        );
        assert!(
            modes(&state, enforcer).contains(&capped),
            "a revealed Enforcer can't be blocked by more than one creature"
        );
        assert!(
            !modes(&state, disciple).contains(&capped),
            "the blocker cap is the Enforcer's line alone"
        );
    }

    /// An Event probe game: a hero vanilla 2/2, a hero flyer, a hero Turtle,
    /// and two Ninja cards in the Boss graveyard (Shredder's Challenge fuel).
    fn event_probe_game() -> (GameState, PlayerId, PlayerId) {
        let (mut state, horde) = turtle_teamup_game(2);
        let hero = hero_seat(&state, horde);
        mk_creature(&mut state, hero, "Hero Bear", &[], &[]);
        mk_creature(&mut state, hero, "Hero Griffin", &[Keyword::Flying], &[]);
        mk_creature(&mut state, hero, "Hero Turtle", &[], &["Turtle"]);
        let ninjas: Vec<ObjectId> = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .library
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.card_types.subtypes.iter().any(|s| s == "Ninja"))
            })
            .take(2)
            .collect();
        assert_eq!(ninjas.len(), 2, "the event deck must hold Ninja cards");
        for id in ninjas {
            move_to_zone(&mut state, id, Zone::Graveyard, &mut Vec::new());
        }
        (state, horde, hero)
    }

    /// Move one copy of the named Event from the Boss library to its
    /// pre-dispatch zone — exile for Baxter's Experiments (banked beside the
    /// event deck), the Boss graveyard for everything else — exactly like the
    /// wave's `resolve_revealed_event`.
    fn move_event_to_predispatch_zone(
        state: &mut GameState,
        horde: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat")
            .library
            .iter()
            .copied()
            .find(|id| state.objects.get(id).is_some_and(|o| o.name == name))
            .unwrap_or_else(|| panic!("event deck must contain {name}"));
        let dest = if name == "Event: Baxter's Experiments" {
            Zone::Exile
        } else {
            Zone::Graveyard
        };
        move_to_zone(state, id, dest, &mut Vec::new());
        id
    }

    /// Move one copy of the named Event to its pre-dispatch zone and resolve
    /// it with an empty bank. Returns the Event id and the driver's progress.
    fn resolve_event_from_library(
        state: &mut GameState,
        horde: PlayerId,
        name: &str,
    ) -> (ObjectId, HeroEventProgress) {
        let id = move_event_to_predispatch_zone(state, horde, name);
        let progress = resolve_boss_event(state, horde, id, &[], 0, &mut Vec::new());
        (id, progress)
    }

    /// Answer the pending HeroEventChoice prompt with `chosen`, returning the
    /// driver's progress. Panics unless a hero-event prompt is actually pending.
    fn answer_hero_prompt(
        state: &mut GameState,
        horde: PlayerId,
        chosen: Vec<ObjectId>,
    ) -> HeroEventProgress {
        let WaitingFor::HeroEventChoice {
            player,
            event: kind,
            event_id,
            remaining_heroes,
            answers,
            copies_pending,
            fired_baxters,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected a HeroEventChoice prompt, got {:?}",
                state.waiting_for
            );
        };
        answer_hero_event_choice(
            state,
            horde,
            event_id,
            kind,
            player,
            chosen,
            remaining_heroes,
            answers,
            fired_baxters,
            copies_pending,
            &mut Vec::new(),
        )
    }

    /// A hero-owned card in `owner`'s hand (Krang's Stratagem fuel).
    fn mk_hand_card(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        create_object(state, CardId(0), owner, name.to_string(), Zone::Hand)
    }

    fn find_battlefield(state: &GameState, name: &str) -> ObjectId {
        state
            .battlefield
            .iter()
            .copied()
            .find(|id| state.objects.get(id).is_some_and(|o| o.name == name))
            .unwrap_or_else(|| panic!("battlefield must contain {name}"))
    }

    fn in_graveyard(state: &GameState, seat: PlayerId, id: ObjectId) -> bool {
        state
            .players
            .iter()
            .find(|p| p.id == seat)
            .is_some_and(|p| p.graveyard.contains(&id))
    }

    fn hand_of(state: &GameState, seat: PlayerId) -> Vec<ObjectId> {
        state
            .players
            .iter()
            .find(|p| p.id == seat)
            .map(|p| p.hand.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Live deployed-boss count, deploying stage 1 first when the probe game
    /// hasn't (setup-hook drift must not silently zero the Rat King / Savanti
    /// expectations — a stage always has at least one boss).
    fn deployed_boss_count(state: &mut GameState, horde: PlayerId) -> usize {
        if bosses_in_command_zone(state).is_empty() {
            setup_boss_game(state, horde, &mut Vec::new());
        }
        let count = bosses_in_command_zone(state).len();
        assert!(count >= 1, "a deployed stage holds at least one boss");
        count
    }

    fn battlefield_names(state: &GameState) -> Vec<String> {
        let mut names: Vec<String> = state
            .battlefield
            .iter()
            .copied()
            .filter_map(|id| state.objects.get(&id).map(|o| o.name.clone()))
            .collect();
        names.sort();
        names
    }

    /// The Event resolution pin: all 14 Events resolve (per-Event effect or
    /// prompt-shape asserts on the default probe board); nothing stays
    /// deferred. Either list changing — a new Event or a newly silent one —
    /// fails here and forces the lists below to move the Event deliberately.
    /// Rich choice flows (multi-hero queues, tie-breaks, Baxter firing) live
    /// in the dedicated tests below; the pin arms only pin the default-board
    /// shape each Event resolves to.
    #[test]
    fn event_resolution_status_is_pinned() {
        const RESOLVED: &[&str] = &[
            "Event: Baxter's Experiments",
            "Event: Bebop's Rampage",
            "Event: Chrome Dome's Overload",
            "Event: Karai's Planning",
            "Event: Krang's Stratagem",
            "Event: Leatherhead's Smackdown",
            "Event: Rat King's Revolution",
            "Event: Rocksteady's Beatdown",
            "Event: Savanti Romero's Curse",
            "Event: Shredder's Challenge",
            "Event: Slash's Smash",
            "Event: Villain Infiltration",
            "Event: Villain Mayhem!",
            "Event: Villain Scheme!",
        ];
        const DEFERRED: &[&str] = &[];
        assert_eq!(RESOLVED.len(), 14);
        assert_eq!(DEFERRED.len(), 0);

        // Every distinct event-deck card is classified exactly once, and every
        // listed name really is an event-deck card (catches list typos).
        let mut seen: Vec<String> = Vec::new();
        for (_, selector) in
            crate::game::decks::turtle_teamup_horde::TURTLE_TEAMUP_EVENT_DECK_TOKENS
        {
            let preset = known_token_preset_by_id(selector).expect("event selector resolves");
            let name = preset.body.display_name.clone();
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        let events: Vec<&String> = seen.iter().filter(|n| n.starts_with("Event: ")).collect();
        assert_eq!(events.len(), 14, "the event deck holds 14 distinct Events");
        for name in &events {
            assert!(
                RESOLVED.contains(&name.as_str()) || DEFERRED.contains(&name.as_str()),
                "unclassified Event {name} — add it to RESOLVED"
            );
        }
        for name in RESOLVED.iter().chain(DEFERRED.iter()) {
            assert!(
                seen.iter().any(|s| s == name),
                "listed Event {name} is not in the event deck — typo?"
            );
        }

        for name in RESOLVED {
            let (mut state, horde, hero) = event_probe_game();
            let hero_life = seat_life(&state, hero);
            let boss_life = seat_life(&state, horde);
            let (event_id, progress) = resolve_event_from_library(&mut state, horde, name);
            match *name {
                "Event: Bebop's Rampage" => {
                    let bear = state
                        .battlefield
                        .iter()
                        .copied()
                        .find(|id| state.objects.get(id).is_some_and(|o| o.name == "Hero Bear"))
                        .expect("the Bear survives to carry damage");
                    assert_eq!(
                        state.objects.get(&bear).expect("bear").damage_marked,
                        2,
                        "Rampage deals 2 to each hero creature"
                    );
                }
                "Event: Karai's Planning" => {
                    assert_eq!(
                        seat_life(&state, hero),
                        hero_life - 3,
                        "each hero loses 3 life"
                    );
                    assert_eq!(
                        seat_life(&state, horde),
                        boss_life + 3,
                        "the Bosses gain 3 life"
                    );
                }
                "Event: Rocksteady's Beatdown" => {
                    let names = battlefield_names(&state);
                    assert!(
                        !names.iter().any(|n| n == "Hero Griffin"),
                        "the flyer is destroyed"
                    );
                    assert!(
                        names.iter().any(|n| n == "Hero Bear"),
                        "the non-flyer survives"
                    );
                }
                "Event: Shredder's Challenge" => {
                    let returned = state
                        .battlefield
                        .iter()
                        .copied()
                        .filter(|id| {
                            state.objects.get(id).is_some_and(|o| {
                                o.controller == horde
                                    && o.card_types.subtypes.iter().any(|s| s == "Ninja")
                            })
                        })
                        .count();
                    assert_eq!(
                        returned, 2,
                        "two random Ninjas return to the Boss battlefield"
                    );
                }
                "Event: Slash's Smash" => {
                    let names = battlefield_names(&state);
                    assert!(
                        !names.iter().any(|n| n == "Hero Turtle"),
                        "Turtles are destroyed"
                    );
                    assert!(
                        names.iter().any(|n| n == "Hero Bear"),
                        "non-Turtles survive"
                    );
                }
                "Event: Villain Scheme!" => {
                    let seat = state
                        .players
                        .iter()
                        .find(|p| p.id == horde)
                        .expect("horde seat");
                    assert!(
                        seat.graveyard.is_empty(),
                        "the Boss graveyard (incl. the Event) shuffles back"
                    );
                    assert_eq!(
                        seat.library.len(),
                        27,
                        "the two Challenge-fuel Ninjas plus the Event itself \
                         all shuffle back — the full round trip"
                    );
                }
                "Event: Baxter's Experiments" => {
                    assert_eq!(progress, HeroEventProgress::Settled);
                    assert_eq!(
                        state.objects.get(&event_id).expect("baxter").zone,
                        Zone::Exile,
                        "Baxter is banked beside the event deck, not discarded"
                    );
                    assert_eq!(
                        state.baxter_set_aside,
                        vec![event_id],
                        "resolving Baxter banks it for the next event"
                    );
                }
                "Event: Chrome Dome's Overload" => {
                    // Default board: nothing tapped, so both heroes skip.
                    assert_eq!(progress, HeroEventProgress::Settled);
                    assert_eq!(
                        battlefield_names(&state),
                        [
                            "Hero Bear".to_string(),
                            "Hero Griffin".to_string(),
                            "Hero Turtle".to_string()
                        ],
                        "with nothing tapped there is nothing to sacrifice"
                    );
                }
                "Event: Krang's Stratagem" => {
                    // Default board: empty hands, so both heroes skip.
                    assert_eq!(progress, HeroEventProgress::Settled);
                }
                "Event: Leatherhead's Smackdown" => {
                    // Default board: a three-way 2/2 tie, so the captain picks.
                    assert_eq!(progress, HeroEventProgress::Prompted);
                    let WaitingFor::HeroEventChoice {
                        player,
                        cards,
                        count,
                        up_to,
                        ..
                    } = &state.waiting_for
                    else {
                        panic!("Smackdown must prompt, got {:?}", state.waiting_for);
                    };
                    assert_eq!(*player, hero, "the first hero calls for the team");
                    assert_eq!(*count, 1);
                    assert!(!*up_to);
                    assert_eq!(cards.len(), 3, "all three 2/2s tie for greatest power");
                }
                "Event: Rat King's Revolution" => {
                    // Default board: three creatures, so the first hero picks.
                    assert_eq!(progress, HeroEventProgress::Prompted);
                    let WaitingFor::HeroEventChoice {
                        player,
                        cards,
                        count,
                        ..
                    } = &state.waiting_for
                    else {
                        panic!("Rat King must prompt, got {:?}", state.waiting_for);
                    };
                    assert_eq!(*player, hero);
                    assert_eq!(*count, 1);
                    assert_eq!(cards.len(), 3);
                }
                "Event: Savanti Romero's Curse" => {
                    // Default board: three creatures against the live boss
                    // count — a prompt when bosses are fewer, an auto-bounce
                    // of all three when they cover.
                    let bosses = bosses_in_command_zone(&state).len();
                    if 3 <= bosses {
                        assert_eq!(progress, HeroEventProgress::Settled);
                        assert!(
                            state.battlefield.is_empty(),
                            "every hero creature bounces when bosses cover all three"
                        );
                    } else {
                        assert_eq!(progress, HeroEventProgress::Prompted);
                        let WaitingFor::HeroEventChoice { count, .. } = &state.waiting_for else {
                            panic!("Savanti must prompt, got {:?}", state.waiting_for);
                        };
                        assert_eq!(*count, bosses, "X is the number of bosses");
                    }
                }
                "Event: Villain Infiltration" => {
                    // Default board: the same three-way tie as Smackdown.
                    assert_eq!(progress, HeroEventProgress::Prompted);
                    let WaitingFor::HeroEventChoice { cards, count, .. } = &state.waiting_for
                    else {
                        panic!("Infiltration must prompt, got {:?}", state.waiting_for);
                    };
                    assert_eq!(*count, 1);
                    assert_eq!(cards.len(), 3);
                }
                "Event: Villain Mayhem!" => {
                    // Default board: exactly three hero creatures, so the
                    // captain auto-spares all three and nothing is destroyed.
                    assert_eq!(progress, HeroEventProgress::Settled);
                    assert_eq!(
                        battlefield_names(&state),
                        [
                            "Hero Bear".to_string(),
                            "Hero Griffin".to_string(),
                            "Hero Turtle".to_string()
                        ],
                        "sparing all three destroys nothing"
                    );
                }
                other => panic!("RESOLVED lists {other} but asserts nothing for it"),
            }
        }
    }

    /// Chrome Dome's Overload through the full prompt: two tapped creatures
    /// offer both, the answer sacrifices exactly the chosen one.
    #[test]
    fn chrome_overload_sacrifices_the_chosen_tapped_creature() {
        let (mut state, horde, hero) = event_probe_game();
        let bear = find_battlefield(&state, "Hero Bear");
        let griffin = find_battlefield(&state, "Hero Griffin");
        for id in [bear, griffin] {
            state.objects.get_mut(&id).expect("creature").tapped = true;
        }
        let (_, progress) =
            resolve_event_from_library(&mut state, horde, "Event: Chrome Dome's Overload");
        assert_eq!(progress, HeroEventProgress::Prompted);
        let WaitingFor::HeroEventChoice {
            player,
            cards,
            count,
            ..
        } = &state.waiting_for
        else {
            panic!("Overload must prompt, got {:?}", state.waiting_for);
        };
        assert_eq!(*player, hero);
        assert_eq!(*count, 1);
        assert_eq!(cards.len(), 2, "both tapped creatures are offered");
        assert!(cards.contains(&bear) && cards.contains(&griffin));
        assert_eq!(
            answer_hero_prompt(&mut state, horde, vec![bear]),
            HeroEventProgress::Settled,
            "the bare second hero skips, so one answer settles"
        );
        assert!(
            in_graveyard(&state, hero, bear),
            "the chosen one is sacrificed"
        );
        find_battlefield(&state, "Hero Griffin");
    }

    /// Krang's Stratagem across two heroes: the first answers up to two, and
    /// the last hero holding cards auto-covers the remainder.
    #[test]
    fn krang_stratagem_discards_a_team_total_of_two() {
        let (mut state, horde, hero) = event_probe_game();
        let hero2 = hero_seats_in_order(&state, horde)
            .into_iter()
            .find(|&h| h != hero)
            .expect("second hero");
        let a1 = mk_hand_card(&mut state, hero, "Alpha One");
        let a2 = mk_hand_card(&mut state, hero, "Alpha Two");
        let b1 = mk_hand_card(&mut state, hero2, "Bravo One");
        let (_, progress) =
            resolve_event_from_library(&mut state, horde, "Event: Krang's Stratagem");
        assert_eq!(progress, HeroEventProgress::Prompted);
        let WaitingFor::HeroEventChoice {
            player,
            count,
            up_to,
            ..
        } = &state.waiting_for
        else {
            panic!("Stratagem must prompt, got {:?}", state.waiting_for);
        };
        assert_eq!((*player, *count, *up_to), (hero, 2, true));
        assert_eq!(
            answer_hero_prompt(&mut state, horde, vec![a1]),
            HeroEventProgress::Settled,
            "the last loaded hero auto-covers the remainder"
        );
        assert_eq!(hand_of(&state, hero), vec![a2]);
        assert!(hand_of(&state, hero2).is_empty());
        assert!(in_graveyard(&state, hero, a1));
        assert!(in_graveyard(&state, hero2, b1));
    }

    /// Krang's Stratagem when the first hero declines everything: the up-to
    /// answer may be empty, and the team discards only what exists.
    #[test]
    fn krang_stratagem_decline_discards_only_what_exists() {
        let (mut state, horde, hero) = event_probe_game();
        let hero2 = hero_seats_in_order(&state, horde)
            .into_iter()
            .find(|&h| h != hero)
            .expect("second hero");
        let a1 = mk_hand_card(&mut state, hero, "Alpha One");
        let a2 = mk_hand_card(&mut state, hero, "Alpha Two");
        let b1 = mk_hand_card(&mut state, hero2, "Bravo One");
        let (_, progress) =
            resolve_event_from_library(&mut state, horde, "Event: Krang's Stratagem");
        assert_eq!(progress, HeroEventProgress::Prompted);
        assert_eq!(
            answer_hero_prompt(&mut state, horde, Vec::new()),
            HeroEventProgress::Settled
        );
        assert_eq!(hand_of(&state, hero), vec![a1, a2], "declined cards stay");
        assert!(
            in_graveyard(&state, hero2, b1),
            "the shortfall still discards"
        );
    }

    /// Leatherhead's Smackdown breaks a tie by team choice, destroys the pick,
    /// and drains the Bosses for its power.
    #[test]
    fn smackdown_destroys_the_strongest_and_drains_its_power() {
        let (mut state, horde, hero) = event_probe_game();
        for name in ["Hero Griffin", "Hero Turtle"] {
            let id = find_battlefield(&state, name);
            let obj = state.objects.get_mut(&id).expect("creature");
            obj.power = Some(5);
            obj.base_power = Some(5);
        }
        let boss_life = seat_life(&state, horde);
        let (_, progress) =
            resolve_event_from_library(&mut state, horde, "Event: Leatherhead's Smackdown");
        assert_eq!(progress, HeroEventProgress::Prompted);
        let WaitingFor::HeroEventChoice {
            player,
            cards,
            count,
            ..
        } = &state.waiting_for
        else {
            panic!("Smackdown must prompt, got {:?}", state.waiting_for);
        };
        assert_eq!(*player, hero, "the first hero calls for the team");
        assert_eq!(*count, 1);
        assert_eq!(cards.len(), 2, "only the tied 5s are offered, not the 2/2");
        let griffin = find_battlefield(&state, "Hero Griffin");
        assert_eq!(
            answer_hero_prompt(&mut state, horde, vec![griffin]),
            HeroEventProgress::Settled
        );
        assert!(in_graveyard(&state, hero, griffin), "the pick is destroyed");
        find_battlefield(&state, "Hero Turtle");
        assert_eq!(
            seat_life(&state, horde),
            boss_life - 5,
            "the Bosses lose life equal to the destroyed power"
        );
    }

    /// Rat King's Revolution taps the pick and stuns it once per boss.
    #[test]
    fn rat_king_taps_and_stuns_for_each_boss() {
        let (mut state, horde, _) = event_probe_game();
        let bosses = deployed_boss_count(&mut state, horde);
        let (_, progress) =
            resolve_event_from_library(&mut state, horde, "Event: Rat King's Revolution");
        assert_eq!(progress, HeroEventProgress::Prompted);
        let bear = find_battlefield(&state, "Hero Bear");
        assert_eq!(
            answer_hero_prompt(&mut state, horde, vec![bear]),
            HeroEventProgress::Settled
        );
        let obj = state.objects.get(&bear).expect("bear");
        assert!(obj.tapped, "the pick taps");
        assert_eq!(
            obj.counters.get(&CounterType::Stun).copied().unwrap_or(0),
            bosses as u32,
            "one stun counter per boss"
        );
        let griffin = state
            .objects
            .get(&find_battlefield(&state, "Hero Griffin"))
            .expect("griffin");
        assert!(!griffin.tapped);
        assert!(!griffin.counters.contains_key(&CounterType::Stun));
    }

    /// Savanti Romero's Curse bounces each hero's pick to its owner's hand.
    #[test]
    fn savanti_curse_bounces_each_heros_pick_to_hand() {
        let (mut state, horde, hero) = event_probe_game();
        let bosses = deployed_boss_count(&mut state, horde);
        // Force the prompt: more creatures than bosses.
        let mut extra = 0;
        while state
            .battlefield
            .iter()
            .filter(|id| state.objects.get(id).is_some_and(|o| o.controller == hero))
            .count()
            <= bosses
        {
            mk_creature(&mut state, hero, &format!("Extra {extra}"), &[], &[]);
            extra += 1;
        }
        let (_, progress) =
            resolve_event_from_library(&mut state, horde, "Event: Savanti Romero's Curse");
        assert_eq!(progress, HeroEventProgress::Prompted);
        let WaitingFor::HeroEventChoice { cards, count, .. } = state.waiting_for.clone() else {
            panic!("Savanti must prompt, got {:?}", state.waiting_for);
        };
        assert_eq!(count, bosses, "X is the number of bosses");
        let picked: Vec<ObjectId> = cards.into_iter().take(bosses).collect();
        assert_eq!(
            answer_hero_prompt(&mut state, horde, picked.clone()),
            HeroEventProgress::Settled
        );
        let hand = hand_of(&state, hero);
        for id in &picked {
            assert!(hand.contains(id), "the pick returns to its owner's hand");
        }
        assert!(
            !state.battlefield.is_empty(),
            "unpicked creatures stay on the battlefield"
        );
    }

    /// Villain Infiltration destroys the strongest by team choice but drains no
    /// life — the Smackdown discriminator.
    #[test]
    fn infiltration_destroys_the_strongest_without_life_loss() {
        let (mut state, horde, hero) = event_probe_game();
        for name in ["Hero Griffin", "Hero Turtle"] {
            let id = find_battlefield(&state, name);
            let obj = state.objects.get_mut(&id).expect("creature");
            obj.power = Some(5);
            obj.base_power = Some(5);
        }
        let boss_life = seat_life(&state, horde);
        let (_, progress) =
            resolve_event_from_library(&mut state, horde, "Event: Villain Infiltration");
        assert_eq!(progress, HeroEventProgress::Prompted);
        let turtle = find_battlefield(&state, "Hero Turtle");
        assert_eq!(
            answer_hero_prompt(&mut state, horde, vec![turtle]),
            HeroEventProgress::Settled
        );
        assert!(in_graveyard(&state, hero, turtle));
        assert_eq!(
            seat_life(&state, horde),
            boss_life,
            "Infiltration drains no life, unlike Smackdown"
        );
    }

    /// Villain Mayhem! spares exactly the three team picks and destroys every
    /// other hero creature.
    #[test]
    fn villain_mayhem_spares_exactly_three() {
        let (mut state, horde, hero) = event_probe_game();
        let hero2 = hero_seats_in_order(&state, horde)
            .into_iter()
            .find(|&h| h != hero)
            .expect("second hero");
        let h2a = mk_creature(&mut state, hero2, "Second Hero A", &[], &[]);
        let h2b = mk_creature(&mut state, hero2, "Second Hero B", &[], &[]);
        let (_, progress) = resolve_event_from_library(&mut state, horde, "Event: Villain Mayhem!");
        assert_eq!(progress, HeroEventProgress::Prompted);
        let WaitingFor::HeroEventChoice {
            player,
            cards,
            count,
            ..
        } = &state.waiting_for
        else {
            panic!("Mayhem must prompt, got {:?}", state.waiting_for);
        };
        assert_eq!(*player, hero, "the first hero calls for the team");
        assert_eq!(*count, 3);
        assert_eq!(cards.len(), 5, "the pool spans both heroes");
        let bear = find_battlefield(&state, "Hero Bear");
        let griffin = find_battlefield(&state, "Hero Griffin");
        assert_eq!(
            answer_hero_prompt(&mut state, horde, vec![bear, griffin, h2a]),
            HeroEventProgress::Settled
        );
        for survivor in [bear, griffin, h2a] {
            assert!(
                state.battlefield.contains(&survivor),
                "the spared three survive"
            );
        }
        let turtle = state
            .objects
            .iter()
            .find(|(_, o)| o.name == "Hero Turtle")
            .map(|(id, _)| *id)
            .expect("turtle");
        assert!(in_graveyard(&state, hero, turtle), "the rest are destroyed");
        assert!(in_graveyard(&state, hero2, h2b));
    }

    /// A banked Baxter fires on the next event: the Event resolves again, then
    /// the Baxter is discarded.
    #[test]
    fn banked_baxter_resolves_the_next_event_again() {
        let (mut state, horde, hero) = event_probe_game();
        let (baxter, _) =
            resolve_event_from_library(&mut state, horde, "Event: Baxter's Experiments");
        assert_eq!(state.baxter_set_aside, vec![baxter]);
        let rampage = move_event_to_predispatch_zone(&mut state, horde, "Event: Bebop's Rampage");
        let fired = std::mem::take(&mut state.baxter_set_aside);
        let progress = resolve_boss_event(&mut state, horde, rampage, &fired, 0, &mut Vec::new());
        assert_eq!(progress, HeroEventProgress::Settled);
        for name in ["Hero Bear", "Hero Griffin", "Hero Turtle"] {
            let id = find_battlefield(&state, name);
            assert_eq!(
                state.objects.get(&id).expect("creature").damage_marked,
                4,
                "Rampage resolved twice (2 + 2)"
            );
        }
        assert!(
            in_graveyard(&state, horde, baxter),
            "the fired Baxter is discarded after the resolution"
        );
        assert!(state.baxter_set_aside.is_empty());
        let _ = hero;
    }

    /// The fired-Baxter discard runs after the resolution's own steps but
    /// BEFORE owed copies: the copy of a Scheme! recycles the just-fired
    /// Baxter back into the library instead of stranding it.
    #[test]
    fn fired_baxter_hits_the_graveyard_before_copies_resolve() {
        let (mut state, horde, _) = event_probe_game();
        let (baxter, _) =
            resolve_event_from_library(&mut state, horde, "Event: Baxter's Experiments");
        let scheme = move_event_to_predispatch_zone(&mut state, horde, "Event: Villain Scheme!");
        let fired = std::mem::take(&mut state.baxter_set_aside);
        let progress = resolve_boss_event(&mut state, horde, scheme, &fired, 0, &mut Vec::new());
        assert_eq!(progress, HeroEventProgress::Settled);
        let seat = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat");
        assert!(
            seat.graveyard.is_empty(),
            "the copy recycled the fired Baxter too — nothing strands"
        );
        assert!(
            seat.library.contains(&baxter),
            "the fired Baxter is back in the library via the copy's restock"
        );
        assert!(seat.library.contains(&scheme));
        assert!(state.baxter_set_aside.is_empty());
    }

    /// Battlefield replacement source redirecting `card`'s moves destined for
    /// `from_dest` to `to`, as one Optional def (accept = 0, decline = 1).
    fn optional_redirect_source(
        state: &mut GameState,
        owner: PlayerId,
        card: ObjectId,
        from_dest: Zone,
        to: Zone,
    ) {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
            TargetFilter,
        };
        use crate::types::replacements::ReplacementEvent;
        let source = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Redirector".to_string(),
            Zone::Battlefield,
        );
        let def = ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: to,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .destination_zone(from_dest)
            .valid_card(TargetFilter::SpecificObject { id: card })
            .mode(ReplacementMode::Optional { decline: None });
        state
            .objects
            .get_mut(&source)
            .expect("source")
            .replacement_definitions
            .push(def);
    }

    /// A CR 616.1 pause on a ZoneMove step parks the remaining steps; after
    /// the choice declines the redirect, the post-replacement funnel drains
    /// them and every card lands. Revert guard: the raw mover never prompts,
    /// so the park assertion fails without the pipeline.
    #[test]
    fn hero_event_zonemove_pauses_for_choice_and_resumes_remaining_steps() {
        use crate::types::actions::GameAction;
        let (mut state, horde) = turtle_teamup_game(2);
        let event = move_event_to_predispatch_zone(&mut state, horde, "Event: Villain Scheme!");
        let first_id = CardId(state.next_object_id);
        let first = create_object(
            &mut state,
            first_id,
            horde,
            "Stranded One".to_string(),
            Zone::Graveyard,
        );
        let second_id = CardId(state.next_object_id);
        let second = create_object(
            &mut state,
            second_id,
            horde,
            "Stranded Two".to_string(),
            Zone::Graveyard,
        );
        optional_redirect_source(&mut state, horde, first, Zone::Library, Zone::Exile);
        let steps = vec![
            HeroEventApplyStep::ZoneMove {
                card: first,
                dest: Zone::Library,
            },
            HeroEventApplyStep::ZoneMove {
                card: second,
                dest: Zone::Library,
            },
        ];

        let mut events = Vec::new();
        let progress = apply_hero_event_steps(&mut state, horde, event, steps, 0, &mut events);
        assert_eq!(
            progress,
            HeroEventProgress::Prompted,
            "the redirect pauses the batch"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "the pause parks a replacement prompt, got {:?}",
            state.waiting_for
        );
        let parked = state
            .pending_hero_event
            .as_deref()
            .expect("parked remainder");
        assert_eq!(
            parked.steps,
            vec![HeroEventApplyStep::ZoneMove {
                card: second,
                dest: Zone::Library,
            }],
            "only the unapplied step parks"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("decline the redirect");
        let seat = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat");
        assert!(seat.library.contains(&first), "the first card completes");
        assert!(
            seat.library.contains(&second),
            "the drained step moves the second"
        );
        assert!(
            state.pending_hero_event.is_none(),
            "the funnel drains the park"
        );
    }

    /// Baxter discards apply one step per card, so a CR 616.1 pause on the
    /// first Baxter parks the second as its own step; declining resumes it.
    #[test]
    fn fired_baxter_discard_parks_second_baxter_across_pause() {
        use crate::types::actions::GameAction;
        let (mut state, horde) = turtle_teamup_game(2);
        let event = move_event_to_predispatch_zone(&mut state, horde, "Event: Villain Scheme!");
        let banked: Vec<ObjectId> = ["Baxter One", "Baxter Two"]
            .into_iter()
            .map(|name| {
                let cid = CardId(state.next_object_id);
                let id = create_object(&mut state, cid, horde, name.to_string(), Zone::Exile);
                let obj = state.objects.get_mut(&id).expect("banked");
                obj.is_token = true;
                obj.is_horde_deck_token = true;
                id
            })
            .collect();
        optional_redirect_source(&mut state, horde, banked[0], Zone::Graveyard, Zone::Exile);

        let mut events = Vec::new();
        let progress = apply_event_steps(
            &mut state,
            horde,
            event,
            Vec::new(),
            &banked,
            0,
            &mut events,
        );
        assert_eq!(
            progress,
            HeroEventProgress::Prompted,
            "the redirect pauses the batch"
        );
        let parked = state
            .pending_hero_event
            .as_deref()
            .expect("parked remainder");
        assert_eq!(
            parked.steps,
            vec![HeroEventApplyStep::DiscardFiredBaxters {
                cards: vec![banked[1]],
            }],
            "the second Baxter parks as its own single-card step"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("decline the redirect");
        let seat = state
            .players
            .iter()
            .find(|p| p.id == horde)
            .expect("horde seat");
        assert!(
            seat.graveyard.contains(&banked[0]),
            "the first Baxter discards"
        );
        assert!(
            seat.graveyard.contains(&banked[1]),
            "the drained step discards the second"
        );
        assert!(
            state.pending_hero_event.is_none(),
            "the funnel drains the park"
        );
    }
}
