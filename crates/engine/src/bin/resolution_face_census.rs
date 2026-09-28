use engine::ai_support;
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::engine as game_engine;
use engine::game::game_object::GameObject;
use engine::game::printed_cards::populate_back_face_if_dfc;
use engine::game::scenario::GameScenario;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardPlayMode, CastFromZoneDriver, ControllerRef,
    Effect, FilterProp, ManaSpendPermission, MultiTargetSpec, PlayerFilter, QuantityExpr,
    QuantityRef, ResolutionCastWindow, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::game_state::{
    CastOfferKind, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCHEMA: &str = "resolution-face-census-v1";
const WITNESS_SCHEMA: &str = "resolution-face-witness-v2";
const CANDIDATE: ObjectId = ObjectId(100);
const P0: PlayerId = PlayerId(0);

const PROFILE_TABLE: &str = "\
cascade-exile-lt8|Exile|typed Not(Land)|resulting spell MV < 8|Free/Auto|single|ordinary
discover-exile-le6|Exile|typed Not(Land)|resulting spell MV <= 6|Free/Auto|single|ordinary
ripple-library|Library|printed-name equality plus typed Not(Land)|none|Free/Auto|single|ordinary
target-free-any-origin|Hand,Exile,Graveyard,Library|Any exact target|none|Free/Auto|single|ordinary
target-free-transformed-control|Exile|Any exact target|none|Free/Auto|single|cast_transformed
target-alt-hand-2u|Hand|Any exact hand pick|none|AlternativeMana {2}{U}/Auto|single|ordinary
target-paid-normal|Hand,Exile,Graveyard,Library|typed Not(Land) exact target|none|FullCost normal/Manual|single|ordinary
target-paid-any-type|Hand,Exile,Graveyard,Library|typed Not(Land) exact target|none|FullCost AnyTypeOrColor/Manual|single|ordinary
window-hand-graveyard-is6|Hand,Graveyard|Or(Instant,Sorcery)|remaining total MV 6|Free/Auto|empty;2|ordinary
window-batch-four-zone|Hand,Exile,Graveyard,Library|Any exact batch|none|Free/Auto|[100];2|ordinary
window-opponent-graveyard-is|Graveyard|Or(Instant,Sorcery) exact pool|none|Free/Auto|[100];1|ordinary";

const WITNESS: &str = "\
schema=resolution-face-witness-v2
players=caster/active PlayerId(0), opponent PlayerId(1)
turn=1, precombat-main, parent resolving, no priority
life=100 each
candidate=ObjectId(100), caster owner/controller except opponent-graveyard profile (opponent owner/controller), inserted after support, moved to origin
stack=ObjectId(800) inert targetable support below resolving ObjectId(900)
mana=64 unrestricted units each of W,U,B,R,G,C; ManaUnit with snow supertype and source ids 200..205
energy=caster 64, opponent 0
battlefield=caster 200..205 and opponent 300..305; creature,artifact,enchantment,planeswalker,battle,land; untapped,targetable,ability-free
graveyard=caster 400 creature + 401 noncreature; opponent 410 creature + 411 noncreature
hand-cost=caster 500..507 ability-free nonland cards with generic mana values 0..7
cost-fodder=caster 600..615 untapped ability-free creature tokens
ordering=support inserted by ascending ObjectId; choices resolve by ascending player/ObjectId";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Origin {
    Hand,
    Exile,
    Graveyard,
    Library,
}

impl Origin {
    const ALL: [Self; 4] = [Self::Hand, Self::Exile, Self::Graveyard, Self::Library];

    fn zone(self) -> Zone {
        match self {
            Self::Hand => Zone::Hand,
            Self::Exile => Zone::Exile,
            Self::Graveyard => Zone::Graveyard,
            Self::Library => Zone::Library,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Hand => "Hand",
            Self::Exile => "Exile",
            Self::Graveyard => "Graveyard",
            Self::Library => "Library",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Profile {
    Cascade,
    Discover,
    Ripple,
    TargetFree,
    Transformed,
    AltHand,
    PaidNormal,
    PaidAnyType,
    WindowInvoke,
    WindowBatch,
    WindowOpponent,
}

impl Profile {
    const ALL: [Self; 11] = [
        Self::Cascade,
        Self::Discover,
        Self::Ripple,
        Self::TargetFree,
        Self::Transformed,
        Self::AltHand,
        Self::PaidNormal,
        Self::PaidAnyType,
        Self::WindowInvoke,
        Self::WindowBatch,
        Self::WindowOpponent,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Cascade => "cascade-exile-lt8",
            Self::Discover => "discover-exile-le6",
            Self::Ripple => "ripple-library",
            Self::TargetFree => "target-free-any-origin",
            Self::Transformed => "target-free-transformed-control",
            Self::AltHand => "target-alt-hand-2u",
            Self::PaidNormal => "target-paid-normal",
            Self::PaidAnyType => "target-paid-any-type",
            Self::WindowInvoke => "window-hand-graveyard-is6",
            Self::WindowBatch => "window-batch-four-zone",
            Self::WindowOpponent => "window-opponent-graveyard-is",
        }
    }

    fn supports(self, origin: Origin) -> bool {
        match self {
            Self::Cascade | Self::Discover | Self::Transformed => origin == Origin::Exile,
            Self::Ripple => origin == Origin::Library,
            Self::AltHand => origin == Origin::Hand,
            Self::TargetFree | Self::PaidNormal | Self::PaidAnyType | Self::WindowBatch => true,
            Self::WindowInvoke => matches!(origin, Origin::Hand | Origin::Graveyard),
            Self::WindowOpponent => origin == Origin::Graveyard,
        }
    }
}

#[derive(Clone, Debug)]
struct Identity {
    oracle_id: String,
    canonical_name: String,
    front_key: String,
    back_key: String,
    front_name: String,
    bucket: String,
    /// Structural printed-Fuse classification across both faces. This is kept
    /// with the raw identity so the census never treats hydrated runtime
    /// keywords as an authority for an export-level inventory.
    has_fuse: bool,
}

#[derive(Clone, Debug)]
struct RawFace {
    storage_key: String,
    name: String,
    layout: String,
    face_index: Option<usize>,
    lands: bool,
    room: bool,
    spell: bool,
    has_fuse: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Observation {
    NotOffered,
    OfferedNoFaceAction,
    FrontAction,
    BackAction,
    BothActions,
    AutoFront,
    AutoBack,
    Rejected,
    RouteError,
}

impl Observation {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotOffered => "not-offered",
            Self::OfferedNoFaceAction => "offered-no-face-action",
            Self::FrontAction => "front-action",
            Self::BackAction => "back-action",
            Self::BothActions => "both-actions",
            Self::AutoFront => "auto-front",
            Self::AutoBack => "auto-back",
            Self::Rejected => "rejected",
            Self::RouteError => "route-error",
        }
    }
}

#[derive(Clone, Debug)]
struct Probe {
    included: bool,
    mask: u8,
    front: Observation,
    back: Observation,
    diagnostic: Option<String>,
}

impl Probe {
    fn unsupported() -> Self {
        Self {
            included: false,
            mask: 0,
            front: Observation::RouteError,
            back: Observation::RouteError,
            diagnostic: Some("unsupported-origin".to_string()),
        }
    }

    fn route_error(message: impl Into<String>) -> Self {
        Self {
            included: false,
            mask: 0,
            front: Observation::RouteError,
            back: Observation::RouteError,
            diagnostic: Some(message.into()),
        }
    }
}

#[derive(Clone, Debug)]
struct ProfileRow {
    oracle_id: String,
    canonical_name: String,
    bucket: String,
    origin: String,
    route: String,
    included: bool,
    mask: u8,
    front: String,
    back: String,
}

type ProfileKey = (String, String, String);

#[derive(Clone, Debug, Eq, PartialEq)]
struct HandFuse {
    structural: BTreeMap<String, String>,
    eligible: BTreeMap<String, String>,
    ineligible: BTreeMap<String, String>,
}

type CaptureParts = (
    BTreeMap<String, String>,
    BTreeMap<ProfileKey, ProfileRow>,
    HandFuse,
);

impl ProfileRow {
    fn key(&self) -> ProfileKey {
        (
            self.oracle_id.clone(),
            self.origin.clone(),
            self.route.clone(),
        )
    }

    fn encode(&self) -> String {
        format!(
            "PROFILE\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.oracle_id,
            json_string(&self.canonical_name),
            self.bucket,
            self.origin,
            self.route,
            u8::from(self.included),
            self.mask,
            self.front,
            self.back
        )
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

fn sha256(bytes: impl AsRef<[u8]>) -> String {
    let digest = Sha256::digest(bytes.as_ref());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn section_hash(lines: &[String]) -> String {
    let mut bytes = lines.join("\n").into_bytes();
    bytes.push(b'\n');
    sha256(bytes)
}

fn face_index_diagnostic(face: &RawFace) -> String {
    face.face_index
        .map(|index| index.to_string())
        .unwrap_or_else(|| "missing".to_string())
}

fn enumerate_identities(raw: &[u8]) -> Result<(Vec<Identity>, Vec<String>), String> {
    let root: BTreeMap<String, Value> =
        serde_json::from_slice(raw).map_err(|error| format!("invalid card export: {error}"))?;
    let mut grouped: BTreeMap<String, Vec<RawFace>> = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for (storage_key, value) in root {
        let Some(oracle_id) = value.get("scryfall_oracle_id").and_then(Value::as_str) else {
            // Keep malformed export entries visible in the capture instead of
            // silently dropping them before the identity census.
            diagnostics.push(format!(
                "DIAGNOSTIC\texport-key:{storage_key}\tExport\tentry\tmissing-oracle-id"
            ));
            grouped
                .entry(format!("export-key:{storage_key}"))
                .or_default();
            continue;
        };
        let Some(name) = value.get("name").and_then(Value::as_str) else {
            diagnostics.push(format!(
                "DIAGNOSTIC\t{oracle_id}\tExport\tentry\tmissing-name"
            ));
            grouped.entry(oracle_id.to_string()).or_default();
            continue;
        };
        let layout = value
            .get("layout")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let core_types = value
            .pointer("/card_type/core_types")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let subtypes = value
            .pointer("/card_type/subtypes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let spell = value
            .get("abilities")
            .and_then(Value::as_array)
            .is_some_and(|abilities| {
                abilities
                    .iter()
                    .any(|ability| ability.get("kind").and_then(Value::as_str) == Some("Spell"))
            });
        // allow-raw-authority: this is an export-structure census, not a game
        // legality decision. The printed `keywords` field is the sole source
        // for the cross-face Fuse inventory; no GameState/object authority
        // exists before a fixture object is created from this raw export.
        let has_fuse = value
            .get("keywords")
            .and_then(Value::as_array)
            .is_some_and(|keywords| {
                keywords
                    .iter()
                    .any(|keyword| keyword.as_str() == Some("Fuse"))
            });
        grouped
            .entry(oracle_id.to_string())
            .or_default()
            .push(RawFace {
                storage_key,
                name: name.to_string(),
                layout: layout.to_string(),
                face_index: value
                    .get("face_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok()),
                lands: core_types.iter().any(|kind| kind.as_str() == Some("Land")),
                room: subtypes.iter().any(|kind| kind.as_str() == Some("Room")),
                spell,
                has_fuse,
            });
    }

    let mut identities = Vec::new();
    for (oracle_id, mut faces) in grouped {
        faces.sort_by(|left, right| {
            left.face_index
                .cmp(&right.face_index)
                .then_with(|| left.name.cmp(&right.name))
        });
        let audited_faces: Vec<_> = faces
            .iter()
            .filter(|face| matches!(face.layout.as_str(), "modal_dfc" | "split"))
            .collect();
        if audited_faces.len() != 2 {
            diagnostics.push(format!(
                "DIAGNOSTIC\t{oracle_id}\tExport\tidentity\taudited-face-count={}",
                audited_faces.len()
            ));
            continue;
        }
        let front = audited_faces[0];
        let back = audited_faces[1];
        if front.layout != back.layout {
            diagnostics.push(format!(
                "DIAGNOSTIC\t{oracle_id}\tExport\tidentity\tmixed-audited-face-layouts={},{}",
                front.layout, back.layout
            ));
            continue;
        }
        if (front.face_index, back.face_index) != (Some(0), Some(1)) {
            diagnostics.push(format!(
                "DIAGNOSTIC\t{oracle_id}\tExport\tidentity\tinvalid-audited-face-indices={},{}",
                face_index_diagnostic(front),
                face_index_diagnostic(back)
            ));
            continue;
        }
        let bucket = if front.layout == "modal_dfc" {
            if front.spell && back.lands {
                "modal-spell-land"
            } else if audited_faces.iter().all(|face| face.spell && !face.lands) {
                "modal-spell-spell"
            } else {
                diagnostics.push(format!(
                    "DIAGNOSTIC\t{oracle_id}\tExport\tidentity\tunsupported-modal-face-class"
                ));
                continue;
            }
        } else if audited_faces.iter().any(|face| face.room) {
            "room"
        } else if audited_faces.iter().all(|face| face.spell && !face.lands) {
            "split-non-room"
        } else {
            diagnostics.push(format!(
                "DIAGNOSTIC\t{oracle_id}\tExport\tidentity\tunsupported-split-face-class"
            ));
            continue;
        };
        identities.push(Identity {
            oracle_id,
            canonical_name: format!("{} // {}", front.name, back.name),
            front_key: front.storage_key.clone(),
            back_key: back.storage_key.clone(),
            front_name: front.name.clone(),
            bucket: bucket.to_string(),
            // A split card is Fuse-eligible when either printed half bears
            // Fuse; the export's per-face storage has no privileged side.
            has_fuse: front.has_fuse || back.has_fuse,
        });
    }
    Ok((identities, diagnostics))
}

fn insert_support(
    state: &mut engine::types::game_state::GameState,
    id: u64,
    owner: PlayerId,
    zone: Zone,
    name: &str,
    types: &[CoreType],
    mana_value: u32,
) {
    let object_id = ObjectId(id);
    // allow-raw-zone: standalone census fixture constructs disposable witness objects, not gameplay events.
    let mut object = GameObject::new(object_id, CardId(id), owner, name.to_string(), zone);
    object.card_types = CardType {
        core_types: types.to_vec(),
        ..CardType::default()
    };
    object.base_card_types = object.card_types.clone();
    object.mana_cost = ManaCost::Cost {
        shards: Vec::new(),
        generic: mana_value,
    };
    object.base_mana_cost = object.mana_cost.clone();
    state.objects.insert(object_id, object);
    if zone != Zone::Stack {
        // allow-raw-zone: census fixture registers its newly constructed disposable witness outside gameplay.
        add_to_zone(state, object_id, zone, owner);
    }
}

fn canonical_witness() -> engine::types::game_state::GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.with_life(P0, 100).with_life(PlayerId(1), 100);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.turn_number = 1;

    let permanent_types = [
        CoreType::Creature,
        CoreType::Artifact,
        CoreType::Enchantment,
        CoreType::Planeswalker,
        CoreType::Battle,
        CoreType::Land,
    ];
    for (offset, kind) in permanent_types.iter().enumerate() {
        insert_support(
            state,
            200 + offset as u64,
            P0,
            Zone::Battlefield,
            &format!("Caster support {kind:?}"),
            &[*kind],
            0,
        );
        let object = state
            .objects
            .get_mut(&ObjectId(200 + offset as u64))
            .expect("caster support exists");
        object.card_types.supertypes.push(Supertype::Snow);
        object.base_card_types = object.card_types.clone();
    }
    for (offset, kind) in permanent_types.iter().enumerate() {
        insert_support(
            state,
            300 + offset as u64,
            PlayerId(1),
            Zone::Battlefield,
            &format!("Opponent support {kind:?}"),
            &[*kind],
            0,
        );
    }
    insert_support(
        state,
        400,
        P0,
        Zone::Graveyard,
        "Caster grave creature",
        &[CoreType::Creature],
        1,
    );
    insert_support(
        state,
        401,
        P0,
        Zone::Graveyard,
        "Caster grave spell",
        &[CoreType::Sorcery],
        1,
    );
    insert_support(
        state,
        410,
        PlayerId(1),
        Zone::Graveyard,
        "Opponent grave creature",
        &[CoreType::Creature],
        1,
    );
    insert_support(
        state,
        411,
        PlayerId(1),
        Zone::Graveyard,
        "Opponent grave spell",
        &[CoreType::Instant],
        1,
    );
    for id in 500..=507 {
        insert_support(
            state,
            id,
            P0,
            Zone::Hand,
            &format!("Hand cost {}", id - 500),
            &[CoreType::Instant],
            (id - 500) as u32,
        );
    }
    for id in 600..=615 {
        insert_support(
            state,
            id,
            P0,
            Zone::Battlefield,
            &format!("Cost fodder {id}"),
            &[CoreType::Creature],
            0,
        );
        let object = state
            .objects
            .get_mut(&ObjectId(id))
            .expect("cost fodder exists");
        object.is_token = true;
        object.color = vec![ManaColor::ALL[((id - 600) as usize) % ManaColor::ALL.len()]];
        object.base_color = object.color.clone();
    }
    insert_support(
        state,
        800,
        P0,
        Zone::Stack,
        "Inert support spell",
        &[CoreType::Instant],
        1,
    );
    state.stack.push_back(StackEntry {
        id: ObjectId(800),
        source_id: ObjectId(800),
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(800),
            ability: None,
            casting_variant: Default::default(),
            actual_mana_spent: 0,
        },
    });
    let colors = [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ];
    for (index, color) in colors.into_iter().enumerate() {
        for _ in 0..64 {
            let _ = state.add_mana_to_pool(
                P0,
                ManaUnit::new(color, ObjectId(200 + index as u64), true, Vec::new()),
            );
        }
    }
    state
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("canonical caster exists")
        .energy = 64;
    state.next_object_id = 100;
    state.clone()
}

fn place_candidate(
    state: &mut engine::types::game_state::GameState,
    identity: &Identity,
    origin: Origin,
    db: &CardDatabase,
    alt_hand: bool,
    owner: PlayerId,
) -> Result<(), String> {
    state.next_object_id = CANDIDATE.0;
    let face = db
        .face_iter()
        .find_map(|(key, face)| (key == identity.front_key).then_some(face))
        .ok_or_else(|| format!("hydrated front face missing: {}", identity.front_key))?;
    let id = create_object_from_card_face(state, face, owner);
    if id != CANDIDATE {
        return Err(format!("candidate allocator returned {id:?}"));
    }
    populate_back_face_if_dfc(
        state
            .objects
            .get_mut(&id)
            .ok_or_else(|| "candidate object missing after hydration".to_string())?,
        db,
        face,
    );
    // allow-raw-zone: census relocates a disposable hydrated candidate before game actions can observe it.
    remove_from_zone(state, id, Zone::Library, owner);
    // allow-raw-zone: paired census-fixture bookkeeping, not a replaceable gameplay zone event.
    add_to_zone(state, id, origin.zone(), owner);
    let object = state
        .objects
        .get_mut(&id)
        .ok_or_else(|| "candidate object missing after placement".to_string())?;
    // allow-raw-zone: keep the synthetic object coherent with census fixture collections, outside gameplay.
    object.zone = origin.zone();
    if alt_hand {
        let keyword = Keyword::Suspend {
            count: 1,
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
        };
        object.keywords.push(keyword.clone());
        object.base_keywords.push(keyword);
    }
    if origin == Origin::Library {
        let library = &mut state
            .players
            .iter_mut()
            .find(|player| player.id == owner)
            .ok_or_else(|| format!("candidate owner {owner:?} is not in the witness"))?
            .library;
        library.retain(|id| *id != CANDIDATE);
        library.push_front(CANDIDATE);
    }
    state.next_object_id = 901;
    Ok(())
}

fn nonland_filter() -> TargetFilter {
    TargetFilter::Not {
        filter: Box::new(TargetFilter::Typed(TypedFilter::land())),
    }
}

/// A nonland target constrained to the four non-battlefield origins that this
/// census advertises.  `Not(Land)` alone is a battlefield target shape; using
/// it for the paid profiles made every hand/exile/graveyard/library row claim
/// a route that its public target prompt could not actually select.
fn nonland_in_any_origin_filter() -> TargetFilter {
    TargetFilter::And {
        filters: vec![nonland_filter(), exact_any_target_filter()],
    }
}

/// A real, single-object filter over every origin the census observes.
/// `TargetFilter::Any` is the engine's resolution-time broadcast sentinel, so
/// it deliberately produces no `WaitingFor::TargetSelection`. A typed filter
/// with no zone property only enumerates the battlefield (and an otherwise
/// empty typed shape denotes players), which would make an exiled candidate
/// absent from the source's public target-declaration slot. Name the census
/// origins explicitly so the normal target enumerator exposes the selected
/// candidate without fabricating a slot.
fn exact_any_target_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: Vec::new(),
        controller: None,
        properties: vec![FilterProp::InAnyZone {
            zones: Origin::ALL.into_iter().map(Origin::zone).collect(),
        }],
    })
}

fn instant_sorcery_filter() -> TargetFilter {
    TargetFilter::Or {
        filters: vec![
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
        ],
    }
}

fn opponent_instant_sorcery_filter() -> TargetFilter {
    TargetFilter::Or {
        filters: [TypeFilter::Instant, TypeFilter::Sorcery]
            .into_iter()
            .map(|kind| {
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![kind],
                    controller: Some(ControllerRef::TargetPlayer),
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                })
            })
            .collect(),
    }
}

fn cast_effect(
    target: TargetFilter,
    free: bool,
    transformed: bool,
    alt: Option<AbilityCost>,
    driver: CastFromZoneDriver,
    mana_spend_permission: Option<ManaSpendPermission>,
) -> Effect {
    Effect::CastFromZone {
        target,
        without_paying_mana_cost: free,
        mode: CardPlayMode::Cast,
        cast_transformed: transformed,
        alt_ability_cost: alt,
        constraint: None,
        duration: None,
        driver,
        mana_spend_permission,
        additional_cost: None,
        cast_cost_modifier: None,
    }
}

fn source_effect(profile: Profile) -> Effect {
    match profile {
        Profile::Cascade => Effect::Cascade,
        Profile::Discover => Effect::Discover {
            mana_value_limit: QuantityExpr::Fixed { value: 6 },
            player: TargetFilter::Controller,
        },
        Profile::Ripple => Effect::Ripple { count: 1 },
        Profile::TargetFree => cast_effect(
            exact_any_target_filter(),
            true,
            false,
            None,
            CastFromZoneDriver::DuringResolution,
            None,
        ),
        Profile::Transformed => cast_effect(
            exact_any_target_filter(),
            true,
            true,
            None,
            CastFromZoneDriver::DuringResolution,
            None,
        ),
        Profile::AltHand => cast_effect(
            TargetFilter::Typed(TypedFilter {
                type_filters: Vec::new(),
                controller: None,
                properties: vec![FilterProp::InZone { zone: Zone::Hand }],
            }),
            false,
            false,
            Some(AbilityCost::KeywordCostOfCastSpell {
                keyword: KeywordKind::Suspend,
            }),
            CastFromZoneDriver::DuringResolution,
            None,
        ),
        Profile::PaidNormal => cast_effect(
            nonland_in_any_origin_filter(),
            false,
            false,
            None,
            CastFromZoneDriver::DuringResolution,
            None,
        ),
        Profile::PaidAnyType => cast_effect(
            nonland_in_any_origin_filter(),
            false,
            false,
            None,
            CastFromZoneDriver::DuringResolution,
            Some(ManaSpendPermission::AnyTypeOrColor),
        ),
        Profile::WindowInvoke => Effect::FreeCastFromZones {
            count: Some(2),
            max_total_mv: Some(6),
            filter: instant_sorcery_filter(),
            zones: vec![Zone::Hand, Zone::Graveyard],
            graveyard_replacement: None,
        },
        Profile::WindowBatch => cast_effect(
            exact_any_target_filter(),
            true,
            false,
            None,
            CastFromZoneDriver::ResolutionWindow {
                bounds: ResolutionCastWindow {
                    max_casts: Some(2),
                    max_total_mv: None,
                },
            },
            None,
        ),
        Profile::WindowOpponent => cast_effect(
            opponent_instant_sorcery_filter(),
            true,
            false,
            None,
            CastFromZoneDriver::DuringResolution,
            None,
        ),
    }
}

fn source_ability(profile: Profile) -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(AbilityKind::Spell, source_effect(profile));
    if profile == Profile::WindowOpponent {
        ability.multi_target = Some(MultiTargetSpec::up_to(QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent,
            },
        }));
    }
    ability
}

fn prepare_source(
    state: &mut engine::types::game_state::GameState,
    identity: &Identity,
    profile: Profile,
) -> Result<ObjectId, String> {
    let source = ObjectId(900);
    insert_support(
        state,
        source.0,
        P0,
        Zone::Hand,
        "Resolution census source",
        &[CoreType::Instant],
        // Cascade's first hit must have MV strictly below the resolving
        // source. Keep this fixed at the public route's `< 8` boundary.
        if profile == Profile::Cascade { 8 } else { 0 },
    );
    let object = state
        .objects
        .get_mut(&source)
        .ok_or_else(|| "source object missing after placement".to_string())?;
    if profile == Profile::Ripple {
        object.name = identity.front_name.clone();
        object.base_name = identity.front_name.clone();
    }
    let ability = source_ability(profile);
    object.abilities = Arc::new(vec![ability.clone()]);
    object.base_abilities = Arc::new(vec![ability]);
    Ok(source)
}

/// Candidate membership is defined only by actual cast-offer windows. A source
/// spell's ordinary target-selection prompt is deliberately not a candidate offer.
fn candidate_offer_window_membership(state: &engine::types::game_state::GameState) -> bool {
    match &state.waiting_for {
        WaitingFor::CastOffer { kind, .. } => candidate_offer_membership(kind),
        WaitingFor::EffectZoneChoice { cards, .. } => candidate_in_effect_zone_choice(cards),
        WaitingFor::ModalFaceChoice { object_id, .. } => {
            candidate_modal_face_membership(*object_id)
        }
        WaitingFor::TargetSelection { .. } => source_target_selection_is_candidate_offer(),
        _ => false,
    }
}

fn candidate_offer_membership(kind: &CastOfferKind) -> bool {
    match kind {
        CastOfferKind::Adventure { object_id, .. }
        | CastOfferKind::Miracle { object_id, .. }
        | CastOfferKind::Madness { object_id, .. } => *object_id == CANDIDATE,
        CastOfferKind::Paradigm { offers } => offers.contains(&CANDIDATE),
        CastOfferKind::Cascade { hit_card, .. }
        | CastOfferKind::Discover { hit_card, .. }
        | CastOfferKind::GraveyardPaidCast { hit_card, .. } => *hit_card == CANDIDATE,
        CastOfferKind::Ripple {
            hit_card,
            remaining_hits,
            ..
        } => *hit_card == CANDIDATE || remaining_hits.contains(&CANDIDATE),
        CastOfferKind::FreeCastWindow { candidates, .. } => candidates.contains(&CANDIDATE),
    }
}

fn candidate_in_effect_zone_choice(cards: &[ObjectId]) -> bool {
    cards.contains(&CANDIDATE)
}

fn candidate_modal_face_membership(object_id: ObjectId) -> bool {
    object_id == CANDIDATE
}

fn source_target_selection_is_candidate_offer() -> bool {
    false
}

#[derive(Clone, Copy, Debug, Default)]
struct RouteProgress {
    source_target_prompt: bool,
    source_target_action: bool,
    offer_accepted: bool,
}

fn source_targets(profile: Profile) -> Option<Vec<TargetRef>> {
    match profile {
        Profile::Cascade | Profile::Discover | Profile::Ripple | Profile::WindowInvoke => None,
        Profile::AltHand => Some(vec![TargetRef::Object(CANDIDATE)]),
        Profile::WindowOpponent => Some(vec![
            TargetRef::Player(PlayerId(1)),
            TargetRef::Object(CANDIDATE),
        ]),
        Profile::TargetFree
        | Profile::Transformed
        | Profile::PaidNormal
        | Profile::PaidAnyType
        | Profile::WindowBatch => Some(vec![TargetRef::Object(CANDIDATE)]),
    }
}

/// The opponent-graveyard route is a paired player/object target.  The object
/// must really belong to that announced opponent; otherwise the public target
/// validator rejects the pair before the census reaches the window.
fn candidate_owner(profile: Profile) -> PlayerId {
    if profile == Profile::WindowOpponent {
        PlayerId(1)
    } else {
        P0
    }
}

const MAX_PUBLIC_ROUTE_ACTIONS: usize = 64;

/// A census route may drive only a small, known sequence of public actions.
/// Keep a compact state fingerprint at every boundary so a future engine
/// continuation cannot turn a capture into an unbounded action cycle.  This
/// is intentionally diagnostic rather than a fallback: a repeated state is
/// reported to the row as `route-error`.
struct PublicRouteGuard {
    route: String,
    steps: usize,
    seen: BTreeSet<String>,
}

impl PublicRouteGuard {
    fn new(
        route: impl Into<String>,
        state: &engine::types::game_state::GameState,
        source: ObjectId,
    ) -> Self {
        let mut seen = BTreeSet::new();
        seen.insert(Self::fingerprint(state, source));
        Self {
            route: route.into(),
            steps: 0,
            seen,
        }
    }

    fn fingerprint(state: &engine::types::game_state::GameState, source: ObjectId) -> String {
        let zone = |id| state.objects.get(&id).map(|object| object.zone);
        let stack: Vec<_> = state.stack.iter().map(|entry| entry.id).collect();
        format!(
            "waiting={:?};priority={:?};source={:?};candidate={:?};stack={stack:?};resolving={:?}",
            state.waiting_for,
            state.priority_player,
            zone(source),
            zone(CANDIDATE),
            state.resolving_stack_entry,
        )
    }

    fn apply(
        &mut self,
        state: &mut engine::types::game_state::GameState,
        source: ObjectId,
        player: PlayerId,
        action: GameAction,
        label: &str,
    ) -> Result<(), String> {
        if self.steps == MAX_PUBLIC_ROUTE_ACTIONS {
            return Err(format!(
                "{} exceeded {MAX_PUBLIC_ROUTE_ACTIONS} public actions before candidate stack endpoint",
                self.route
            ));
        }
        game_engine::apply(state, player, action)
            .map_err(|error| format!("{label} rejected: {error:?}"))?;
        self.record_after_action(state, source, label)
    }

    fn record_after_action(
        &mut self,
        state: &engine::types::game_state::GameState,
        source: ObjectId,
        label: &str,
    ) -> Result<(), String> {
        self.steps += 1;
        if !self.seen.insert(Self::fingerprint(state, source)) {
            return Err(format!(
                "{} repeated a public continuation state after {label} (step {})",
                self.route, self.steps
            ));
        }
        Ok(())
    }
}

fn accept_offer(
    state: &mut engine::types::game_state::GameState,
    profile: Profile,
    progress: &mut RouteProgress,
) -> Result<(), String> {
    if progress.offer_accepted {
        return Err("candidate offer was accepted more than once".to_string());
    }
    let action = match profile {
        Profile::Cascade => GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        },
        Profile::Discover => GameAction::DiscoverChoice {
            choice: CastChoice::Cast,
        },
        Profile::Ripple => GameAction::RippleChoice {
            choice: CastChoice::Cast,
        },
        Profile::AltHand => GameAction::SelectCards {
            cards: vec![CANDIDATE],
        },
        Profile::PaidNormal | Profile::PaidAnyType => GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        },
        Profile::WindowInvoke | Profile::WindowBatch | Profile::WindowOpponent => {
            GameAction::FreeCastWindowChoice {
                selection: Some(CANDIDATE),
            }
        }
        Profile::TargetFree | Profile::Transformed => return Ok(()),
    };
    game_engine::apply(state, P0, action)
        .map(|_| progress.offer_accepted = true)
        .map_err(|error| format!("{error:?}"))
}

enum CandidateStackEndpoint {
    Absent,
    Spell(CastingVariant),
}

/// Inspect only the stack entry that proves the candidate spell was created.
/// The object itself is not the endpoint evidence: `DuringResolution` can
/// retain the object's source-zone marker while the resolving spell entry is
/// already present on the stack.
fn candidate_stack_endpoint(
    state: &engine::types::game_state::GameState,
) -> Result<CandidateStackEndpoint, String> {
    let mut matching = state
        .stack
        .iter()
        .filter(|entry| entry.id == CANDIDATE && entry.source_id == CANDIDATE);
    let Some(entry) = matching.next() else {
        return Ok(CandidateStackEndpoint::Absent);
    };
    if matching.next().is_some() {
        return Err("candidate stack endpoint has duplicate matching entries".to_string());
    }
    match &entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => Ok(CandidateStackEndpoint::Spell(*casting_variant)),
        _ => Err("candidate stack endpoint is not a Spell entry".to_string()),
    }
}

fn target_free_stack_endpoint(state: &engine::types::game_state::GameState) -> Result<(), String> {
    match candidate_stack_endpoint(state)? {
        CandidateStackEndpoint::Spell(CastingVariant::Normal) => Ok(()),
        CandidateStackEndpoint::Spell(variant) => Err(format!(
            "target-free candidate stack endpoint used non-Normal casting variant {variant:?}"
        )),
        CandidateStackEndpoint::Absent => {
            Err("target-free candidate stack endpoint is missing".to_string())
        }
    }
}

/// The TargetFree witness is a `DuringResolution` cast: once its source has
/// resolved, the engine can put the candidate directly onto the stack without
/// ever presenting a candidate offer or face-choice prompt. Its exact, normal
/// Spell stack entry is the only evidence this profile accepts.
fn target_free_auto_stack_probe(
    state: &engine::types::game_state::GameState,
) -> Result<Probe, String> {
    if candidate_offer_window_membership(state) {
        return Err(
            "target-free direct stack cast unexpectedly opened a candidate offer".to_string(),
        );
    }
    target_free_stack_endpoint(state)?;
    Ok(Probe {
        included: false,
        mask: 1,
        front: Observation::AutoFront,
        back: Observation::NotOffered,
        diagnostic: None,
    })
}

/// Pick the one continuation that is both public and deterministic for the
/// census witness.  In particular, a manual-payment prompt is completed by
/// its public `PassPriority` confirmation: the witness already owns a pool
/// that can pay every advertised profile.  Asking `legal_actions` to enumerate
/// every mana-source or card-specific alternative here is unnecessary work and
/// can dominate a full-card-data capture.  A state outside this deliberately
/// small route alphabet is not guessed: the probe records it as `route-error`.
fn selected_cast_continuation_action(
    state: &engine::types::game_state::GameState,
) -> Result<GameAction, String> {
    match &state.waiting_for {
        WaitingFor::ManaPayment { .. } => Ok(GameAction::PassPriority),
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => selection
            .current_legal_targets
            .first()
            .cloned()
            .map(|target| GameAction::ChooseTarget {
                target: Some(target),
            })
            .or_else(|| {
                target_slots
                    .get(selection.current_slot)
                    .filter(|slot| slot.optional)
                    .map(|_| GameAction::ChooseTarget { target: None })
            })
            .ok_or_else(|| {
                "route-error: target selection has no deterministic legal answer".to_string()
            }),
        other => Err(format!(
            "route-error: unclassified candidate cast continuation {other:?}"
        )),
    }
}

/// Continue a selected cast through the public action surface until the
/// candidate reaches the stack.
fn complete_selected_cast(
    state: &mut engine::types::game_state::GameState,
    selected_back_face: Option<bool>,
) -> Result<(), String> {
    let mut guard = PublicRouteGuard::new("selected cast", state, CANDIDATE);
    if let Some(back_face) = selected_back_face {
        guard
            .apply(
                state,
                CANDIDATE,
                P0,
                GameAction::ChooseModalFace { back_face },
                "face choice",
            )
            .map_err(|error| format!("route-error: {error}"))?;
    }
    while guard.steps < MAX_PUBLIC_ROUTE_ACTIONS {
        match candidate_stack_endpoint(state) {
            Ok(CandidateStackEndpoint::Spell(_)) => return Ok(()),
            Ok(CandidateStackEndpoint::Absent) => {}
            Err(error) => return Err(format!("route-error: {error}")),
        }
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Err("candidate returned to priority before reaching stack".to_string());
        }
        let action = selected_cast_continuation_action(state)?;
        let player = state.priority_player;
        guard
            .apply(state, CANDIDATE, player, action, "cast continuation")
            .map_err(|error| format!("route-error: {error}"))?;
    }
    Err(format!(
        "route-error: selected cast exceeded {MAX_PUBLIC_ROUTE_ACTIONS} public actions"
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceAdvance {
    Offer,
    NoOffer,
}

fn advance_source_to_candidate_window(
    state: &mut engine::types::game_state::GameState,
    source: ObjectId,
    profile: Profile,
    progress: RouteProgress,
) -> Result<SourceAdvance, String> {
    if progress.source_target_action && !progress.source_target_prompt {
        return Err("source target action occurred without its public prompt".to_string());
    }
    let mut guard = PublicRouteGuard::new(
        format!("{} source continuation", profile.name()),
        state,
        source,
    );
    while guard.steps < MAX_PUBLIC_ROUTE_ACTIONS {
        if candidate_offer_window_membership(state) {
            return Ok(SourceAdvance::Offer);
        }
        let source_on_stack = state
            .objects
            .get(&source)
            .is_some_and(|object| object.zone == Zone::Stack);
        if profile == Profile::TargetFree && !source_on_stack {
            // This route is intentionally special: it casts directly to the
            // stack. Keep the endpoint guard so an absent candidate remains a
            // genuine route error rather than a fabricated no-offer row.
            target_free_stack_endpoint(state)?;
            return Ok(SourceAdvance::NoOffer);
        }
        if matches!(state.waiting_for, WaitingFor::RippleRevealChoice { .. }) {
            guard.apply(
                state,
                source,
                P0,
                GameAction::RippleChoice {
                    choice: CastChoice::Cast,
                },
                "ripple reveal choice",
            )?;
            continue;
        }
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            if !source_on_stack {
                return Ok(SourceAdvance::NoOffer);
            }
            let player = state.priority_player;
            guard.apply(
                state,
                source,
                player,
                GameAction::PassPriority,
                "source priority pass",
            )?;
            continue;
        }
        return Err(format!(
            "unclassified {} source continuation {:?}",
            profile.name(),
            state.waiting_for
        ));
    }
    Err(format!(
        "{} source continuation exceeded {MAX_PUBLIC_ROUTE_ACTIONS} public actions before a candidate offer window",
        profile.name()
    ))
}

fn probe(
    template: &engine::types::game_state::GameState,
    identity: &Identity,
    origin: Origin,
    profile: Profile,
    db: &CardDatabase,
) -> Result<Probe, String> {
    if !profile.supports(origin) {
        return Ok(Probe::unsupported());
    }
    let mut initial = template.clone();
    let seed_origin = if matches!(profile, Profile::Cascade | Profile::Discover) {
        Origin::Library
    } else {
        origin
    };
    place_candidate(
        &mut initial,
        identity,
        seed_origin,
        db,
        profile == Profile::AltHand,
        candidate_owner(profile),
    )?;
    let sentinel = template.next_object_id;
    let source = prepare_source(&mut initial, identity, profile)?;
    let source_card_id = initial
        .objects
        .get(&source)
        .ok_or_else(|| "source object missing before cast".to_string())?
        .card_id;
    let mut progress = RouteProgress::default();
    if let Err(error) = game_engine::apply(
        &mut initial,
        P0,
        GameAction::CastSpell {
            object_id: source,
            card_id: source_card_id,
            targets: vec![CANDIDATE],
            payment_mode: Default::default(),
        },
    ) {
        return Ok(Probe::route_error(format!(
            "source cast rejected: {error:?}"
        )));
    }
    if let Some(targets) = source_targets(profile) {
        if !matches!(initial.waiting_for, WaitingFor::TargetSelection { .. }) {
            return Ok(Probe::route_error(
                "ordinary source cast did not produce target selection",
            ));
        }
        progress.source_target_prompt = true;
        if profile == Profile::WindowOpponent {
            let expected = vec![TargetRef::Player(PlayerId(1)), TargetRef::Object(CANDIDATE)];
            if targets != expected {
                return Ok(Probe::route_error(
                    "window-opponent target vector lost public ordering",
                ));
            }
        }
        if let Err(error) =
            game_engine::apply(&mut initial, P0, GameAction::SelectTargets { targets })
        {
            return Ok(Probe::route_error(format!(
                "source targets rejected: {error:?}"
            )));
        }
        progress.source_target_action = true;
    }
    let advance = match advance_source_to_candidate_window(&mut initial, source, profile, progress)
    {
        Ok(advance) => advance,
        Err(error) => return Ok(Probe::route_error(error)),
    };
    if matches!(profile, Profile::Cascade | Profile::Discover)
        && initial.objects.get(&CANDIDATE).map(|object| object.zone) != Some(Zone::Exile)
    {
        return Ok(Probe::route_error(
            "library-seeded candidate did not reach reported Exile origin",
        ));
    }

    let included = advance == SourceAdvance::Offer;
    match advance {
        SourceAdvance::Offer => {}
        SourceAdvance::NoOffer if profile == Profile::TargetFree => {
            return match target_free_auto_stack_probe(&initial) {
                Ok(probe) => Ok(probe),
                Err(error) => Ok(Probe::route_error(error)),
            };
        }
        // A cleanly completed source route is census data. Every other route
        // failure above remains an error instead of being collapsed here.
        SourceAdvance::NoOffer => {
            return Ok(Probe {
                included: false,
                mask: 0,
                front: Observation::NotOffered,
                back: Observation::NotOffered,
                diagnostic: None,
            });
        }
    }
    if !matches!(initial.waiting_for, WaitingFor::ModalFaceChoice { .. }) {
        if let Err(error) = accept_offer(&mut initial, profile, &mut progress) {
            return Ok(Probe {
                included: true,
                mask: 0,
                front: Observation::Rejected,
                back: Observation::Rejected,
                diagnostic: Some(error),
            });
        }
    }

    // The census only needs the generic action enumerator at the dedicated
    // modal-face prompt.  Calling it for an arbitrary card's later casting
    // prompt can expand that card's full target/mode/payment action set before
    // the bounded continuation classifier gets to reject unsupported shapes.
    let (front_action, back_action) =
        if matches!(initial.waiting_for, WaitingFor::ModalFaceChoice { .. }) {
            let legal = ai_support::legal_actions(&initial);
            (
                legal.iter().any(|action| {
                    matches!(action, GameAction::ChooseModalFace { back_face: false })
                }),
                legal.iter().any(|action| {
                    matches!(action, GameAction::ChooseModalFace { back_face: true })
                }),
            )
        } else {
            (false, false)
        };
    let mut completion_diagnostics = Vec::new();
    let mut observe = |selected_back_face, success, failure| {
        let mut continuation = initial.clone();
        match complete_selected_cast(&mut continuation, selected_back_face) {
            Ok(()) => success,
            Err(error) => {
                let observation = if error.starts_with("route-error:") {
                    Observation::RouteError
                } else {
                    failure
                };
                completion_diagnostics.push(error);
                observation
            }
        }
    };
    let (front, back) = match (front_action, back_action) {
        (true, true) => {
            let front = observe(Some(false), Observation::FrontAction, Observation::Rejected);
            let back = observe(Some(true), Observation::BackAction, Observation::Rejected);
            if front == Observation::FrontAction && back == Observation::BackAction {
                (Observation::BothActions, Observation::BothActions)
            } else {
                (front, back)
            }
        }
        (true, false) => (
            observe(Some(false), Observation::FrontAction, Observation::Rejected),
            Observation::OfferedNoFaceAction,
        ),
        (false, true) => (
            Observation::OfferedNoFaceAction,
            observe(Some(true), Observation::BackAction, Observation::Rejected),
        ),
        (false, false) => {
            let mut continuation = initial.clone();
            match complete_selected_cast(&mut continuation, None) {
                Ok(()) => {
                    let committed = continuation.objects.get(&CANDIDATE).and_then(|object| {
                        (object.zone == Zone::Stack || object.cast_face_committed)
                            .then_some(object.modal_back_face || object.transformed)
                    });
                    match committed {
                        Some(false) => (Observation::AutoFront, Observation::OfferedNoFaceAction),
                        Some(true) => (Observation::OfferedNoFaceAction, Observation::AutoBack),
                        None => (
                            Observation::OfferedNoFaceAction,
                            Observation::OfferedNoFaceAction,
                        ),
                    }
                }
                Err(error) => {
                    let observation = if error.starts_with("route-error:") {
                        Observation::RouteError
                    } else {
                        Observation::Rejected
                    };
                    completion_diagnostics.push(error);
                    (observation, observation)
                }
            }
        }
    };
    let mask = u8::from(matches!(
        front,
        Observation::FrontAction | Observation::BothActions | Observation::AutoFront
    )) | (u8::from(matches!(
        back,
        Observation::BackAction | Observation::BothActions | Observation::AutoBack
    )) << 1);
    if template.next_object_id != sentinel {
        return Err("immutable witness template mutated across probe".to_string());
    }
    Ok(Probe {
        included,
        mask,
        front,
        back,
        diagnostic: (!completion_diagnostics.is_empty()).then(|| completion_diagnostics.join("; ")),
    })
}

fn hand_fuse_line(label: &str, entries: &[(String, String)]) -> String {
    let ids: Vec<_> = entries.iter().map(|entry| entry.0.clone()).collect();
    let names: Vec<_> = entries.iter().map(|entry| entry.1.clone()).collect();
    format!(
        "HAND_FUSE\t{label}\t{}\t{}\t{}\t{}\t{}",
        entries.len(),
        serde_json::to_string(&names).expect("Fuse names serialize"),
        serde_json::to_string(&ids).expect("Fuse ids serialize"),
        section_hash(&ids),
        section_hash(&names)
    )
}

/// Tests the one public action this census records, without constructing the
/// complete AI candidate universe.  The latter is intentionally much broader
/// than a hand cast (it includes every mana-payment combination and unrelated
/// action family), and doing that once per exported identity makes capture
/// impractical.
///
/// This remains a public-route test: it submits the ordinary `CastSpell`
/// action as P0 through the production engine boundary.  A reducer rejection
/// is the deterministic negative eligibility result; malformed witness setup
/// remains an error for the caller rather than being silently omitted.
fn public_hand_cast_eligible(
    state: &mut engine::types::game_state::GameState,
) -> Result<bool, String> {
    let card_id = state
        .objects
        .get(&CANDIDATE)
        .map(|object| object.card_id)
        .ok_or_else(|| "hand Fuse candidate disappeared before public cast".to_string())?;
    match game_engine::apply(
        state,
        P0,
        GameAction::CastSpell {
            object_id: CANDIDATE,
            card_id,
            targets: Vec::new(),
            payment_mode: Default::default(),
        },
    ) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

fn hand_fuse_section(
    identities: &[Identity],
    db: &CardDatabase,
    template: &engine::types::game_state::GameState,
) -> Result<Vec<String>, String> {
    let mut structural = Vec::new();
    let mut eligible = Vec::new();
    for identity in identities {
        if identity.bucket != "split-non-room" || !identity.has_fuse {
            continue;
        }
        // The raw structural classification above is only meaningful when both
        // exact export keys hydrate into the same database the public cast
        // probe will use. This validates that bridge without inspecting any
        // hydrated keyword authority.
        for key in [&identity.front_key, &identity.back_key] {
            if !db
                .face_iter()
                .any(|(candidate_key, _)| candidate_key == key)
            {
                return Err(format!("missing hydrated Fuse face {key}"));
            }
        }
        structural.push((identity.oracle_id.clone(), identity.canonical_name.clone()));
        let mut state = template.clone();
        place_candidate(&mut state, identity, Origin::Hand, db, false, P0)?;
        state.stack.clear();
        state.resolving_stack_entry = None;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.priority_player = P0;
        if public_hand_cast_eligible(&mut state)? {
            eligible.push((identity.oracle_id.clone(), identity.canonical_name.clone()));
        }
    }
    let eligible_ids: BTreeSet<_> = eligible.iter().map(|entry| entry.0.as_str()).collect();
    let ineligible: Vec<_> = structural
        .iter()
        .filter(|entry| !eligible_ids.contains(entry.0.as_str()))
        .cloned()
        .collect();
    Ok(vec![
        hand_fuse_line("structural", &structural),
        hand_fuse_line("eligible", &eligible),
        hand_fuse_line("ineligible", &ineligible),
    ])
}

/// A tiny, deterministic two-face export for the wrapper's hermetic
/// capture/compare positive control.  It intentionally exercises split-face
/// identity reconstruction without depending on the browser card-data export.
fn self_test_fixture_export() -> String {
    let face = |name: &str, face_index: usize, fuse: bool| {
        let face = CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..CardType::default()
            },
            abilities: vec![AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)],
            keywords: fuse.then_some(Keyword::Fuse).into_iter().collect(),
            scryfall_oracle_id: Some("fixture-split-oracle".to_string()),
            ..CardFace::default()
        };
        let mut entry = serde_json::to_value(face)
            .expect("fixture CardFace serializes")
            .as_object()
            .expect("fixture CardFace is an object")
            .clone();
        entry.insert("layout".to_string(), Value::String("split".to_string()));
        entry.insert("face_index".to_string(), Value::from(face_index));
        Value::Object(entry)
    };
    let mut export = serde_json::Map::new();
    export.insert(
        "fixture-front-storage-key".to_string(),
        face("Fixture Front", 0, false),
    );
    export.insert(
        "fixture-back-storage-key".to_string(),
        face("Fixture Back", 1, true),
    );
    Value::Object(export).to_string()
}

fn capture(card_data: &Path, candidate_sha: &str) -> Result<String, String> {
    let raw = fs::read(card_data)
        .map_err(|error| format!("cannot read {}: {error}", card_data.display()))?;
    let db = CardDatabase::from_export(card_data)
        .map_err(|error| format!("cannot hydrate {}: {error}", card_data.display()))?;
    let (identities, mut diagnostics) = enumerate_identities(&raw)?;
    if identities.is_empty() {
        return Err("generated export contains no audited two-face identities".to_string());
    }
    let template = canonical_witness();
    let mut rows = Vec::new();
    let mut keys = BTreeSet::new();
    for identity in &identities {
        for origin in Origin::ALL {
            for profile in Profile::ALL {
                let result = probe(&template, identity, origin, profile, &db)?;
                if profile.supports(origin) {
                    diagnostics.push(format!(
                        "REACH\t{}\t{}\t{}\treached",
                        identity.oracle_id,
                        origin.name(),
                        profile.name()
                    ));
                }
                let row = ProfileRow {
                    oracle_id: identity.oracle_id.clone(),
                    canonical_name: identity.canonical_name.clone(),
                    bucket: identity.bucket.clone(),
                    origin: origin.name().to_string(),
                    route: profile.name().to_string(),
                    included: result.included,
                    mask: result.mask,
                    front: result.front.as_str().to_string(),
                    back: result.back.as_str().to_string(),
                };
                if !keys.insert(row.key()) {
                    return Err(format!("duplicate PROFILE key: {:?}", row.key()));
                }
                if let Some(message) = result.diagnostic {
                    diagnostics.push(format!(
                        "DIAGNOSTIC\t{}\t{}\t{}\t{}",
                        identity.oracle_id,
                        origin.name(),
                        profile.name(),
                        message.replace(['\t', '\n'], " ")
                    ));
                }
                rows.push(row);
            }
        }
    }
    rows.sort_by(|left, right| {
        let origin_rank = |origin: &str| match origin {
            "Hand" => 0,
            "Exile" => 1,
            "Graveyard" => 2,
            "Library" => 3,
            _ => 4,
        };
        left.oracle_id
            .cmp(&right.oracle_id)
            .then_with(|| origin_rank(&left.origin).cmp(&origin_rank(&right.origin)))
            .then_with(|| left.route.cmp(&right.route))
    });
    let profile_lines: Vec<_> = rows.iter().map(ProfileRow::encode).collect();
    let hand_fuse_lines = hand_fuse_section(&identities, &db, &template)?;
    let invariant_lines = vec![
        format!("INVARIANT\tprofile-keys-unique\t{}\tPASS", keys.len()),
        format!(
            "INVARIANT\tprofile-matrix-complete\t{}\tPASS",
            identities.len() * Origin::ALL.len() * Profile::ALL.len()
        ),
        "INVARIANT\tobservation-vocabulary\tnine-values\tPASS".to_string(),
        "INVARIANT\tmask-source\tactions-or-stack-only\tPASS".to_string(),
        "INVARIANT\tfresh-witness-per-probe\ttrue\tPASS".to_string(),
    ];
    let harness_source = include_bytes!("resolution_face_census.rs");
    let cargo_source = include_bytes!("../../Cargo.toml");
    let wrapper_source = include_bytes!("../../../../scripts/audit-resolution-face-casting.sh");
    let harness_hash = sha256(
        [
            harness_source.as_slice(),
            cargo_source.as_slice(),
            wrapper_source.as_slice(),
        ]
        .concat(),
    );
    let mut output = vec![
        format!("SCHEMA\t{SCHEMA}"),
        format!("META\tcandidate_sha\t{candidate_sha}"),
        format!("META\twitness_schema\t{WITNESS_SCHEMA}"),
        format!("HASH\tharness_sha256\t{harness_hash}"),
        format!("HASH\tbinary_sha256\t{}", sha256(harness_source)),
        format!("HASH\tcargo_sha256\t{}", sha256(cargo_source)),
        format!("HASH\twrapper_sha256\t{}", sha256(wrapper_source)),
        format!(
            "HASH\troute_profiles_sha256\t{}",
            section_hash(
                &PROFILE_TABLE
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            )
        ),
        format!(
            "HASH\twitness_sha256\t{}",
            section_hash(&WITNESS.lines().map(str::to_string).collect::<Vec<_>>())
        ),
        format!("HASH\tinput_sha256\t{}", sha256(&raw)),
        format!("HASH\tprofiles_sha256\t{}", section_hash(&profile_lines)),
        format!("HASH\thand_fuse_sha256\t{}", section_hash(&hand_fuse_lines)),
        format!(
            "HASH\tinvariants_sha256\t{}",
            section_hash(&invariant_lines)
        ),
    ];
    output.extend(profile_lines);
    output.extend(diagnostics);
    output.extend(hand_fuse_lines);
    output.extend(invariant_lines);
    Ok(format!("{}\n", output.join("\n")))
}

fn parse_capture(input: &str) -> Result<CaptureParts, String> {
    let mut hashes = BTreeMap::new();
    let mut rows = BTreeMap::new();
    let mut encoded_rows = Vec::new();
    let mut schema_seen = false;
    let mut hand_fuse_lines = Vec::new();
    let mut hand_fuse = BTreeMap::new();
    let mut invariant_lines = Vec::new();
    let mut last_profile_order: Option<(String, u8, String)> = None;
    let known_observations = [
        "not-offered",
        "offered-no-face-action",
        "front-action",
        "back-action",
        "both-actions",
        "auto-front",
        "auto-back",
        "rejected",
        "route-error",
    ];
    for line in input.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        match fields.first().copied() {
            Some("SCHEMA") if fields.len() == 2 && fields[1] == SCHEMA => schema_seen = true,
            Some("HASH") if fields.len() == 3 => match hashes.entry(fields[1].to_string()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(fields[2].to_string());
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(format!("duplicate hash {}", fields[1]));
                }
            },
            Some("PROFILE") => {
                if fields.len() != 10 {
                    return Err(format!("malformed PROFILE row: {line}"));
                }
                if fields[1].is_empty()
                    || fields[5].is_empty()
                    || !matches!(
                        fields[3],
                        "modal-spell-spell" | "modal-spell-land" | "split-non-room" | "room"
                    )
                {
                    return Err(format!("malformed PROFILE key: {line}"));
                }
                let origin_rank = match fields[4] {
                    "Hand" => 0,
                    "Exile" => 1,
                    "Graveyard" => 2,
                    "Library" => 3,
                    _ => return Err(format!("unknown origin: {}", fields[4])),
                };
                let order = (fields[1].to_string(), origin_rank, fields[5].to_string());
                if last_profile_order
                    .as_ref()
                    .is_some_and(|last| last >= &order)
                {
                    return Err(format!("noncanonical PROFILE order: {line}"));
                }
                last_profile_order = Some(order);
                let included = match fields[6] {
                    "0" => false,
                    "1" => true,
                    _ => return Err(format!("invalid candidate bit: {}", fields[6])),
                };
                let mask = fields[7]
                    .parse::<u8>()
                    .map_err(|_| format!("invalid mask: {}", fields[7]))?;
                if mask > 3 {
                    return Err(format!("invalid mask: {mask}"));
                }
                if !known_observations.contains(&fields[8])
                    || !known_observations.contains(&fields[9])
                {
                    return Err("unknown observation".to_string());
                }
                let observed_front =
                    matches!(fields[8], "front-action" | "both-actions" | "auto-front");
                let observed_back =
                    matches!(fields[9], "back-action" | "both-actions" | "auto-back");
                let observed_mask = u8::from(observed_front) | (u8::from(observed_back) << 1);
                if observed_mask != mask {
                    return Err(format!("observation-to-mask mismatch: {line}"));
                }
                let target_free_auto_front = !included
                    && fields[5] == "target-free-any-origin"
                    && mask == 1
                    && fields[8] == "auto-front"
                    && fields[9] == "not-offered";
                if (!included
                    && !target_free_auto_front
                    && !(matches!(fields[8], "not-offered" | "route-error")
                        && matches!(fields[9], "not-offered" | "route-error")))
                    || (included
                        && (matches!(fields[8], "not-offered")
                            || matches!(fields[9], "not-offered")))
                {
                    return Err(format!("candidate-to-observation mismatch: {line}"));
                }
                let name: String = serde_json::from_str(fields[2])
                    .map_err(|_| format!("malformed canonical name: {}", fields[2]))?;
                let row = ProfileRow {
                    oracle_id: fields[1].to_string(),
                    canonical_name: name,
                    bucket: fields[3].to_string(),
                    origin: fields[4].to_string(),
                    route: fields[5].to_string(),
                    included,
                    mask,
                    front: fields[8].to_string(),
                    back: fields[9].to_string(),
                };
                let key = row.key();
                if rows.insert(key.clone(), row).is_some() {
                    return Err(format!("duplicate PROFILE key: {key:?}"));
                }
                encoded_rows.push(line.to_string());
            }
            Some("HAND_FUSE") => {
                if fields.len() != 7
                    || !matches!(fields[1], "structural" | "eligible" | "ineligible")
                {
                    return Err(format!("malformed HAND_FUSE row: {line}"));
                }
                let names: Vec<String> = serde_json::from_str(fields[3])
                    .map_err(|_| format!("malformed Fuse names: {line}"))?;
                let ids: Vec<String> = serde_json::from_str(fields[4])
                    .map_err(|_| format!("malformed Fuse oracle ids: {line}"))?;
                if names.len() != ids.len()
                    || fields[2].parse::<usize>().ok() != Some(ids.len())
                    || ids.iter().any(String::is_empty)
                    || ids.windows(2).any(|pair| pair[0] >= pair[1])
                {
                    return Err(format!("inconsistent HAND_FUSE row: {line}"));
                }
                if section_hash(&ids) != fields[5] || section_hash(&names) != fields[6] {
                    return Err(format!("HAND_FUSE row hash mismatch: {line}"));
                }
                let entries: BTreeMap<_, _> = ids.into_iter().zip(names).collect();
                if hand_fuse.insert(fields[1].to_string(), entries).is_some() {
                    return Err(format!("duplicate HAND_FUSE label: {}", fields[1]));
                }
                hand_fuse_lines.push(line.to_string());
            }
            Some("INVARIANT") => invariant_lines.push(line.to_string()),
            _ => {}
        }
    }
    if !schema_seen {
        return Err("missing or mismatched schema".to_string());
    }
    for required in [
        "harness_sha256",
        "route_profiles_sha256",
        "witness_sha256",
        "input_sha256",
        "profiles_sha256",
        "hand_fuse_sha256",
        "invariants_sha256",
        "routes_sha256",
    ] {
        if !hashes.contains_key(required) {
            return Err(format!("missing hash {required}"));
        }
    }
    if section_hash(&encoded_rows) != hashes["profiles_sha256"] {
        return Err("profile hash mismatch".to_string());
    }
    if section_hash(&hand_fuse_lines) != hashes["hand_fuse_sha256"] {
        return Err("hand Fuse hash mismatch".to_string());
    }
    if section_hash(&invariant_lines) != hashes["invariants_sha256"] {
        return Err("invariant hash mismatch".to_string());
    }
    let structural = hand_fuse
        .remove("structural")
        .ok_or_else(|| "missing structural HAND_FUSE row".to_string())?;
    let eligible = hand_fuse
        .remove("eligible")
        .ok_or_else(|| "missing eligible HAND_FUSE row".to_string())?;
    let ineligible = hand_fuse
        .remove("ineligible")
        .ok_or_else(|| "missing ineligible HAND_FUSE row".to_string())?;
    if !hand_fuse.is_empty() {
        return Err("unknown HAND_FUSE labels".to_string());
    }
    if eligible
        .keys()
        .any(|oracle| ineligible.contains_key(oracle))
    {
        return Err("eligible and ineligible HAND_FUSE sets overlap".to_string());
    }
    let partition: BTreeMap<_, _> = eligible
        .iter()
        .chain(ineligible.iter())
        .map(|(oracle, name)| (oracle.clone(), name.clone()))
        .collect();
    if partition != structural {
        return Err(
            "eligible and ineligible HAND_FUSE sets do not partition structural".to_string(),
        );
    }
    for (oracle, name) in &structural {
        let profile = rows
            .values()
            .find(|row| &row.oracle_id == oracle)
            .ok_or_else(|| format!("HAND_FUSE oracle missing from profiles: {oracle}"))?;
        if profile.bucket != "split-non-room" || profile.canonical_name != *name {
            return Err(format!(
                "HAND_FUSE identity does not match profiles: {oracle}"
            ));
        }
    }
    Ok((
        hashes,
        rows,
        HandFuse {
            structural,
            eligible,
            ineligible,
        },
    ))
}

#[derive(Clone)]
struct Delta {
    base: ProfileRow,
    candidate: ProfileRow,
    change: &'static str,
}

fn compare_text(base: &str, candidate: &str) -> Result<String, String> {
    let (base_hashes, base_rows, base_fuse) = parse_capture(base)?;
    let (candidate_hashes, candidate_rows, candidate_fuse) = parse_capture(candidate)?;
    for key in [
        "harness_sha256",
        "route_profiles_sha256",
        "witness_sha256",
        "input_sha256",
        "routes_sha256",
    ] {
        if base_hashes[key] != candidate_hashes[key] {
            return Err(format!("{key} mismatch"));
        }
    }
    if base_rows.keys().collect::<Vec<_>>() != candidate_rows.keys().collect::<Vec<_>>() {
        return Err("PROFILE key sets differ".to_string());
    }
    if base_fuse.structural != candidate_fuse.structural {
        return Err("structural HAND_FUSE sets differ".to_string());
    }
    let fuse_eligibility_added: BTreeMap<_, _> = candidate_fuse
        .eligible
        .iter()
        .filter(|(oracle, _)| !base_fuse.eligible.contains_key(*oracle))
        .map(|(oracle, name)| (oracle.clone(), name.clone()))
        .collect();
    let fuse_eligibility_removed: BTreeMap<_, _> = base_fuse
        .eligible
        .iter()
        .filter(|(oracle, _)| !candidate_fuse.eligible.contains_key(*oracle))
        .map(|(oracle, name)| (oracle.clone(), name.clone()))
        .collect();
    let fuse_eligibility_changed: BTreeSet<_> = fuse_eligibility_added
        .keys()
        .chain(fuse_eligibility_removed.keys())
        .cloned()
        .collect();
    let mut deltas = Vec::new();
    for (key, base_row) in &base_rows {
        let candidate_row = &candidate_rows[key];
        let change = match (base_row.included, candidate_row.included) {
            (false, true) => "candidate-added",
            (true, false) => "candidate-removed",
            _ if base_row.mask != candidate_row.mask
                || base_row.front != candidate_row.front
                || base_row.back != candidate_row.back =>
            {
                "faces-changed"
            }
            _ => "unchanged",
        };
        deltas.push(Delta {
            base: base_row.clone(),
            candidate: candidate_row.clone(),
            change,
        });
    }
    deltas.sort_by(|left, right| {
        let origin_rank = |origin: &str| match origin {
            "Hand" => 0,
            "Exile" => 1,
            "Graveyard" => 2,
            "Library" => 3,
            _ => 4,
        };
        left.base
            .oracle_id
            .cmp(&right.base.oracle_id)
            .then_with(|| origin_rank(&left.base.origin).cmp(&origin_rank(&right.base.origin)))
            .then_with(|| left.base.route.cmp(&right.base.route))
    });
    let mut output = vec![format!("SCHEMA\t{SCHEMA}-comparison")];
    let delta_lines: Vec<_> = deltas
        .iter()
        .map(|delta| {
            format!(
                "DELTA\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                delta.base.oracle_id,
                json_string(&delta.candidate.canonical_name),
                delta.candidate.bucket,
                delta.base.origin,
                delta.base.route,
                u8::from(delta.base.included),
                u8::from(delta.candidate.included),
                delta.base.mask,
                delta.candidate.mask,
                delta.change
            )
        })
        .collect();
    output.extend(delta_lines.clone());

    let mut by_identity: BTreeMap<String, Vec<&Delta>> = BTreeMap::new();
    for delta in &deltas {
        by_identity
            .entry(delta.base.oracle_id.clone())
            .or_default()
            .push(delta);
    }
    let mut identity_lines = Vec::new();
    let mut changed_by_bucket_scope: BTreeMap<(String, String), Vec<(String, String)>> =
        BTreeMap::new();
    let mut affected = 0usize;
    for (oracle, group) in by_identity {
        let sample = &group[0].candidate;
        let mut changed = [false; 4];
        let mut base_masks = [0u8; 4];
        let mut candidate_masks = [0u8; 4];
        for delta in group {
            let index = match delta.base.origin.as_str() {
                "Hand" => 0,
                "Exile" => 1,
                "Graveyard" => 2,
                "Library" => 3,
                _ => return Err(format!("unknown origin {}", delta.base.origin)),
            };
            changed[index] |= delta.change != "unchanged";
            base_masks[index] |= delta.base.mask;
            candidate_masks[index] |= delta.candidate.mask;
        }
        changed[0] |= fuse_eligibility_changed.contains(&oracle);
        let union = changed.iter().any(|value| *value);
        if union {
            affected += 1;
        }
        for (index, scope) in ["hand", "exile", "graveyard", "library"].iter().enumerate() {
            if changed[index] {
                changed_by_bucket_scope
                    .entry((sample.bucket.clone(), (*scope).to_string()))
                    .or_default()
                    .push((oracle.clone(), sample.canonical_name.clone()));
                changed_by_bucket_scope
                    .entry(("all".to_string(), (*scope).to_string()))
                    .or_default()
                    .push((oracle.clone(), sample.canonical_name.clone()));
            }
        }
        if union {
            for bucket in [sample.bucket.as_str(), "all"] {
                changed_by_bucket_scope
                    .entry((bucket.to_string(), "union".to_string()))
                    .or_default()
                    .push((oracle.clone(), sample.canonical_name.clone()));
            }
        }
        identity_lines.push(format!(
            "IDENTITY\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            oracle,
            json_string(&sample.canonical_name),
            sample.bucket,
            u8::from(changed[0]),
            u8::from(changed[1]),
            u8::from(changed[2]),
            u8::from(changed[3]),
            u8::from(union),
            base_masks[0],
            candidate_masks[0],
            base_masks[1],
            candidate_masks[1],
            base_masks[2],
            candidate_masks[2],
            base_masks[3],
            candidate_masks[3]
        ));
    }
    output.extend(identity_lines.clone());
    let mut bucket_lines = Vec::new();
    for bucket in [
        "modal-spell-spell",
        "modal-spell-land",
        "split-non-room",
        "room",
        "all",
    ] {
        for scope in ["hand", "exile", "graveyard", "library", "union"] {
            let entries = changed_by_bucket_scope
                .get(&(bucket.to_string(), scope.to_string()))
                .cloned()
                .unwrap_or_default();
            let ids: Vec<_> = entries.iter().map(|entry| entry.0.clone()).collect();
            let names: Vec<_> = entries.iter().map(|entry| entry.1.clone()).collect();
            let id_lines: Vec<_> = ids.clone();
            let name_lines: Vec<_> = names.clone();
            bucket_lines.push(format!(
                "BUCKET\t{bucket}\t{scope}\t{}\t{}\t{}\t{}",
                entries.len(),
                serde_json::to_string(&names).expect("names serialize"),
                section_hash(&id_lines),
                section_hash(&name_lines)
            ));
        }
    }
    output.extend(bucket_lines.clone());
    let changed_oracles: BTreeSet<_> = deltas
        .iter()
        .filter(|delta| delta.change != "unchanged")
        .map(|delta| delta.base.oracle_id.clone())
        .chain(fuse_eligibility_changed.iter().cloned())
        .collect();
    let fuse_overlap: Vec<_> = candidate_fuse
        .structural
        .iter()
        .filter(|(oracle, _)| changed_oracles.contains(*oracle))
        .map(|(oracle, name)| (oracle.clone(), name.clone()))
        .collect();
    let fuse_added: Vec<_> = fuse_eligibility_added.into_iter().collect();
    let fuse_removed: Vec<_> = fuse_eligibility_removed.into_iter().collect();
    let fuse_lines = vec![
        hand_fuse_line("affected-overlap", &fuse_overlap),
        hand_fuse_line("eligibility-added", &fuse_added),
        hand_fuse_line("eligibility-removed", &fuse_removed),
    ];
    output.extend(fuse_lines.clone());
    output.extend([
        format!("SUMMARY\taffected_total\t{affected}"),
        format!("SUMMARY\thand_fuse_eligibility_added\t{}", fuse_added.len()),
        format!(
            "SUMMARY\thand_fuse_eligibility_removed\t{}",
            fuse_removed.len()
        ),
        format!(
            "HASH\tbase_profiles_sha256\t{}",
            base_hashes["profiles_sha256"]
        ),
        format!(
            "HASH\tcandidate_profiles_sha256\t{}",
            candidate_hashes["profiles_sha256"]
        ),
        format!(
            "HASH\tbase_hand_fuse_sha256\t{}",
            base_hashes["hand_fuse_sha256"]
        ),
        format!(
            "HASH\tcandidate_hand_fuse_sha256\t{}",
            candidate_hashes["hand_fuse_sha256"]
        ),
        format!("HASH\tdeltas_sha256\t{}", section_hash(&delta_lines)),
        format!("HASH\tidentities_sha256\t{}", section_hash(&identity_lines)),
        format!("HASH\tsummary_sha256\t{}", section_hash(&bucket_lines)),
        format!("HASH\thand_fuse_sha256\t{}", section_hash(&fuse_lines)),
    ]);
    Ok(format!("{}\n", output.join("\n")))
}

fn synthetic_capture_with_fuse(
    included: bool,
    mask: u8,
    observation: &str,
    fuse_eligible: Option<bool>,
) -> String {
    let (front, back) = match mask {
        0 => (observation, observation),
        1 => (observation, "offered-no-face-action"),
        2 => ("offered-no-face-action", observation),
        3 => (observation, observation),
        _ => unreachable!("synthetic masks are canonical"),
    };
    synthetic_capture_row(included, mask, front, back, fuse_eligible, "route")
}

fn synthetic_capture_row(
    included: bool,
    mask: u8,
    front: &str,
    back: &str,
    fuse_eligible: Option<bool>,
    route: &str,
) -> String {
    let bucket = if fuse_eligible.is_some() {
        "split-non-room"
    } else {
        "room"
    };
    let row = format!(
        "PROFILE\toracle\t\"Name\"\t{bucket}\tHand\t{route}\t{}\t{}\t{}\t{}",
        u8::from(included),
        mask,
        front,
        back
    );
    let structural = fuse_eligible
        .map(|_| vec![("oracle".to_string(), "Name".to_string())])
        .unwrap_or_default();
    let eligible = fuse_eligible
        .filter(|eligible| *eligible)
        .map(|_| structural.clone())
        .unwrap_or_default();
    let ineligible = fuse_eligible
        .filter(|eligible| !eligible)
        .map(|_| structural.clone())
        .unwrap_or_default();
    let fuse_lines = vec![
        hand_fuse_line("structural", &structural),
        hand_fuse_line("eligible", &eligible),
        hand_fuse_line("ineligible", &ineligible),
    ];
    let invariant = "INVARIANT\tsynthetic\ttrue\tPASS";
    format!(
        "SCHEMA\t{SCHEMA}\nHASH\tharness_sha256\th\nHASH\troute_profiles_sha256\tp\nHASH\twitness_sha256\tw\nHASH\tinput_sha256\ti\nHASH\troutes_sha256\tr\nHASH\tprofiles_sha256\t{}\nHASH\thand_fuse_sha256\t{}\nHASH\tinvariants_sha256\t{}\n{row}\n{}\n{invariant}\n",
        section_hash(std::slice::from_ref(&row)),
        section_hash(&fuse_lines),
        section_hash(&[invariant.to_string()]),
        fuse_lines.join("\n")
    )
}

fn synthetic_capture(included: bool, mask: u8, observation: &str) -> String {
    synthetic_capture_with_fuse(included, mask, observation, None)
}

fn run_self_test() -> Result<(), String> {
    let absent = synthetic_capture(false, 0, "not-offered");
    let front = synthetic_capture(true, 1, "front-action");
    let both = synthetic_capture(true, 3, "both-actions");
    if !compare_text(&absent, &absent)?.contains("\tunchanged\n") {
        return Err("unchanged comparator case failed".to_string());
    }
    if !compare_text(&absent, &front)?.contains("\tcandidate-added\n") {
        return Err("candidate-added comparator case failed".to_string());
    }
    if !compare_text(&front, &absent)?.contains("\tcandidate-removed\n") {
        return Err("candidate-removed comparator case failed".to_string());
    }
    if !compare_text(&front, &both)?.contains("\tfaces-changed\n") {
        return Err("faces-changed comparator case failed".to_string());
    }
    let offered_without_action = synthetic_capture(true, 0, "offered-no-face-action");
    let rejected = synthetic_capture(true, 0, "rejected");
    if !compare_text(&offered_without_action, &rejected)?.contains("\tfaces-changed\n") {
        return Err("observation-only comparator case failed".to_string());
    }
    let route_error = synthetic_capture(false, 0, "route-error");
    if !compare_text(&absent, &route_error)?.contains("\tfaces-changed\n") {
        return Err("route-status comparator case failed".to_string());
    }
    let included_route_error = synthetic_capture(true, 0, "route-error");
    if !compare_text(&included_route_error, &included_route_error)?.contains("\tunchanged\n") {
        return Err("included route-error comparator case failed".to_string());
    }
    let target_free_absent = synthetic_capture_row(
        false,
        0,
        "not-offered",
        "not-offered",
        None,
        "target-free-any-origin",
    );
    let target_free_auto_front = synthetic_capture_row(
        false,
        1,
        "auto-front",
        "not-offered",
        None,
        "target-free-any-origin",
    );
    parse_capture(&target_free_auto_front)?;
    if !compare_text(&target_free_auto_front, &target_free_absent)?.contains("\tfaces-changed\n") {
        return Err("target-free auto-front comparator case failed".to_string());
    }
    let target_free_auto_front_offered = synthetic_capture_row(
        false,
        1,
        "auto-front",
        "offered-no-face-action",
        None,
        "target-free-any-origin",
    );
    if parse_capture(&target_free_auto_front_offered).is_ok() {
        return Err("target-free invalid auto-front tuple unexpectedly passed".to_string());
    }
    let fuse_ineligible = synthetic_capture_with_fuse(false, 0, "not-offered", Some(false));
    let fuse_eligible = synthetic_capture_with_fuse(false, 0, "not-offered", Some(true));
    let (ineligible_hashes, _, _) = parse_capture(&fuse_ineligible)?;
    let (eligible_hashes, _, _) = parse_capture(&fuse_eligible)?;
    let fuse_comparison = compare_text(&fuse_ineligible, &fuse_eligible)?;
    if !fuse_comparison.contains("HAND_FUSE\teligibility-added\t1\t[\"Name\"]\t[\"oracle\"]")
        || !fuse_comparison.contains("SUMMARY\taffected_total\t1")
        || !fuse_comparison.contains("SUMMARY\thand_fuse_eligibility_added\t1")
        || !fuse_comparison.contains("BUCKET\tsplit-non-room\thand\t1\t[\"Name\"]")
        || !fuse_comparison.contains("BUCKET\tsplit-non-room\tunion\t1\t[\"Name\"]")
        || !fuse_comparison.contains(&format!(
            "HASH\tbase_hand_fuse_sha256\t{}",
            ineligible_hashes["hand_fuse_sha256"]
        ))
        || !fuse_comparison.contains(&format!(
            "HASH\tcandidate_hand_fuse_sha256\t{}",
            eligible_hashes["hand_fuse_sha256"]
        ))
        || !fuse_comparison.lines().any(|line| {
            line.starts_with("IDENTITY\toracle\t") && line.contains("\t1\t0\t0\t0\t1\t")
        })
    {
        return Err("hand Fuse eligibility-added comparator case failed".to_string());
    }
    let fuse_reverse = compare_text(&fuse_eligible, &fuse_ineligible)?;
    if !fuse_reverse.contains("HAND_FUSE\teligibility-removed\t1\t[\"Name\"]\t[\"oracle\"]")
        || !fuse_reverse.contains("SUMMARY\thand_fuse_eligibility_removed\t1")
    {
        return Err("hand Fuse eligibility-removed comparator case failed".to_string());
    }
    let malformed = absent.replace("\toracle\t", "\t");
    if parse_capture(&malformed).is_ok() {
        return Err("malformed-key comparator case unexpectedly passed".to_string());
    }
    let row = absent
        .lines()
        .find(|line| line.starts_with("PROFILE\t"))
        .unwrap();
    let duplicate = format!("{absent}{row}\n");
    if parse_capture(&duplicate).is_ok() {
        return Err("duplicate-key comparator case unexpectedly passed".to_string());
    }
    if parse_capture(&absent.replace("not-offered", "invented-observation")).is_ok() {
        return Err("unknown-observation comparator case unexpectedly passed".to_string());
    }
    if parse_capture(&absent.replace("profiles_sha256\t", "profiles_sha256\tbad")).is_ok() {
        return Err("hash-mismatch comparator case unexpectedly passed".to_string());
    }
    Ok(())
}

fn required_arg(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing required argument {flag}"))
}

fn write_output(path: &Path, contents: &str) -> Result<(), String> {
    if path == Path::new("/dev/stdout") {
        print!("{contents}");
        Ok(())
    } else {
        fs::write(path, contents)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))
    }
}

fn real_main() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("capture") => {
            let card_data = PathBuf::from(required_arg(&args, "--card-data")?);
            let candidate_sha = required_arg(&args, "--candidate-sha")?;
            let output = PathBuf::from(required_arg(&args, "--output")?);
            write_output(&output, &capture(&card_data, &candidate_sha)?)
        }
        Some("self-test-fixture-export") => {
            let output = PathBuf::from(required_arg(&args, "--output")?);
            write_output(&output, &self_test_fixture_export())
        }
        Some("compare") => {
            let base = PathBuf::from(required_arg(&args, "--base")?);
            let candidate = PathBuf::from(required_arg(&args, "--candidate")?);
            let output = PathBuf::from(required_arg(&args, "--output")?);
            let base_text = fs::read_to_string(&base)
                .map_err(|error| format!("cannot read {}: {error}", base.display()))?;
            let candidate_text = fs::read_to_string(&candidate)
                .map_err(|error| format!("cannot read {}: {error}", candidate.display()))?;
            write_output(&output, &compare_text(&base_text, &candidate_text)?)
        }
        Some("self-test") => {
            run_self_test()?;
            println!("resolution-face-census self-test: PASS");
            Ok(())
        }
        _ => Err(
            "usage: resolution-face-census <capture|compare|self-test|self-test-fixture-export> [options]"
                .to_string(),
        ),
    }
}

fn main() {
    if let Err(error) = real_main() {
        eprintln!("resolution-face-census: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{ResolutionCastCleanup, ResolutionCastFacePolicy};
    use engine::types::identifiers::ResolutionCastOfferId;

    fn fixture_export() -> String {
        self_test_fixture_export()
    }

    fn fixture_identity_and_db() -> (Identity, CardDatabase) {
        let export = fixture_export();
        let (identities, diagnostics) = enumerate_identities(export.as_bytes()).unwrap();
        assert!(
            diagnostics.is_empty(),
            "fixture diagnostics: {diagnostics:?}"
        );
        let identity = identities
            .into_iter()
            .find(|identity| identity.oracle_id == "fixture-split-oracle")
            .expect("fixture split identity exists");
        let db = CardDatabase::from_json_str(&export).expect("fixture export hydrates");
        (identity, db)
    }

    #[test]
    fn observations_are_exhaustive_and_masked_only_by_actions_or_commit() {
        let values = [
            Observation::NotOffered,
            Observation::OfferedNoFaceAction,
            Observation::FrontAction,
            Observation::BackAction,
            Observation::BothActions,
            Observation::AutoFront,
            Observation::AutoBack,
            Observation::Rejected,
            Observation::RouteError,
        ];
        let encoded: BTreeSet<_> = values.into_iter().map(Observation::as_str).collect();
        assert_eq!(encoded.len(), 9);
    }

    #[test]
    fn comparator_covers_profile_and_hand_fuse_deltas_and_failures() {
        run_self_test().unwrap();
    }

    #[test]
    fn profile_matrix_is_typed_and_complete() {
        assert_eq!(Profile::ALL.len(), 11);
        assert_eq!(Origin::ALL.len(), 4);
        assert!(Profile::TargetFree.supports(Origin::Library));
        assert!(!Profile::Cascade.supports(Origin::Hand));
    }

    #[test]
    fn every_profile_uses_its_declared_public_route_shape() {
        assert_eq!(candidate_owner(Profile::WindowOpponent), PlayerId(1));
        assert!(Profile::ALL
            .into_iter()
            .filter(|profile| *profile != Profile::WindowOpponent)
            .all(|profile| candidate_owner(profile) == P0));

        assert_eq!(
            source_targets(Profile::AltHand),
            Some(vec![TargetRef::Object(CANDIDATE)]),
            "the alternative-cost hand pick must answer its public target prompt"
        );
        for profile in [Profile::PaidNormal, Profile::PaidAnyType] {
            let Effect::CastFromZone { target, .. } = source_effect(profile) else {
                panic!("{profile:?} must be a CastFromZone profile");
            };
            assert!(matches!(target, TargetFilter::And { filters } if filters.len() == 2));
        }
        assert_eq!(
            source_targets(Profile::WindowOpponent),
            Some(vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(CANDIDATE)
            ])
        );
    }

    #[test]
    fn public_route_guard_reports_a_repeated_state() {
        let state = canonical_witness();
        let mut guard = PublicRouteGuard::new("guard test", &state, ObjectId(900));
        let error = guard
            .record_after_action(&state, ObjectId(900), "synthetic public action")
            .unwrap_err();
        assert!(error.contains("repeated a public continuation state"));
        assert!(error.contains("step 1"));
    }

    #[test]
    fn selected_cast_confirms_manual_payment_without_enumerating_mana_actions() {
        let mut state = canonical_witness();
        state.waiting_for = WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: None,
        };
        assert!(matches!(
            selected_cast_continuation_action(&state),
            Ok(GameAction::PassPriority)
        ));
    }

    #[test]
    fn hand_fuse_eligibility_is_one_public_p0_cast_and_rejects_other_submitters() {
        let mut eligible = canonical_witness();
        insert_support(
            &mut eligible,
            CANDIDATE.0,
            P0,
            Zone::Hand,
            "Hand Fuse public-route candidate",
            &[CoreType::Instant],
            0,
        );
        eligible.waiting_for = WaitingFor::Priority { player: P0 };
        eligible.priority_player = P0;
        assert!(
            public_hand_cast_eligible(&mut eligible).unwrap(),
            "a cast accepted by the P0 public engine boundary is eligible"
        );

        let mut rejected = canonical_witness();
        insert_support(
            &mut rejected,
            CANDIDATE.0,
            P0,
            Zone::Hand,
            "Hand Fuse rejected public-route candidate",
            &[CoreType::Instant],
            0,
        );
        rejected.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        rejected.priority_player = PlayerId(1);
        assert!(
            !public_hand_cast_eligible(&mut rejected).unwrap(),
            "a public P0 cast rejected for actor authority is deterministically ineligible"
        );
    }

    #[test]
    fn candidate_offer_membership_covers_every_cast_offer_family() {
        let other = ObjectId(101);
        let zero_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 0,
        };
        let free_window = |candidates| CastOfferKind::FreeCastWindow {
            candidates,
            remaining_casts: Some(1),
            remaining_mv_budget: None,
            face_policy: ResolutionCastFacePolicy::new(
                TargetFilter::Any,
                ObjectId(900),
                PlayerId(0),
                None,
            ),
            zones: vec![Zone::Hand],
            graveyard_replacement: None,
            member_pool: Vec::new(),
        };
        let paid_graveyard = |hit_card| CastOfferKind::GraveyardPaidCast {
            hit_card,
            mana_spend_permission: None,
            graveyard_replacement: None,
            cast_transformed: false,
            additional_cost: None,
            cleanup: ResolutionCastCleanup {
                source_id: ObjectId(900),
                offer_id: Some(ResolutionCastOfferId(1)),
                face_policy: ResolutionCastFacePolicy::new(
                    TargetFilter::Any,
                    ObjectId(900),
                    PlayerId(0),
                    None,
                ),
                exiled_misses: Vec::new(),
                reject_action: engine::types::ability::ResolutionMvRejectAction::RemainExiled,
                success_action: engine::types::ability::ResolutionCastSuccessAction::BottomMisses,
                delayed_trigger_receipts: Vec::new(),
            },
        };
        let positive = vec![
            CastOfferKind::Adventure {
                object_id: CANDIDATE,
                card_id: CardId(100),
                payment_mode: Default::default(),
            },
            CastOfferKind::Miracle {
                object_id: CANDIDATE,
                cost: zero_cost.clone(),
            },
            CastOfferKind::Madness {
                object_id: CANDIDATE,
                cost: zero_cost.clone(),
            },
            CastOfferKind::Paradigm {
                offers: vec![other, CANDIDATE],
            },
            CastOfferKind::Cascade {
                hit_card: CANDIDATE,
                exiled_misses: Vec::new(),
                source_mv: 0,
                source_id: ObjectId(900),
            },
            CastOfferKind::Discover {
                hit_card: CANDIDATE,
                exiled_misses: Vec::new(),
                source_id: ObjectId(900),
                discover_value: 0,
            },
            CastOfferKind::Ripple {
                hit_card: other,
                remaining_hits: vec![CANDIDATE],
                revealed_misses: Vec::new(),
                source_id: ObjectId(900),
            },
            free_window(vec![other, CANDIDATE]),
            paid_graveyard(CANDIDATE),
        ];
        assert!(positive.iter().all(candidate_offer_membership));

        let negative = vec![
            CastOfferKind::Adventure {
                object_id: other,
                card_id: CardId(101),
                payment_mode: Default::default(),
            },
            CastOfferKind::Miracle {
                object_id: other,
                cost: zero_cost.clone(),
            },
            CastOfferKind::Madness {
                object_id: other,
                cost: zero_cost,
            },
            CastOfferKind::Paradigm {
                offers: vec![other],
            },
            CastOfferKind::Cascade {
                hit_card: other,
                exiled_misses: Vec::new(),
                source_mv: 0,
                source_id: ObjectId(900),
            },
            CastOfferKind::Discover {
                hit_card: other,
                exiled_misses: Vec::new(),
                source_id: ObjectId(900),
                discover_value: 0,
            },
            CastOfferKind::Ripple {
                hit_card: other,
                remaining_hits: vec![ObjectId(102)],
                revealed_misses: Vec::new(),
                source_id: ObjectId(900),
            },
            free_window(vec![other]),
            paid_graveyard(other),
        ];
        assert!(negative
            .iter()
            .all(|kind| !candidate_offer_membership(kind)));
    }

    #[test]
    fn non_cast_offer_windows_only_match_the_candidate_object() {
        assert!(candidate_in_effect_zone_choice(&[CANDIDATE]));
        assert!(!candidate_in_effect_zone_choice(&[ObjectId(101)]));
        assert!(candidate_modal_face_membership(CANDIDATE));
        assert!(!candidate_modal_face_membership(ObjectId(101)));
        assert!(!source_target_selection_is_candidate_offer());
    }

    #[test]
    fn target_free_public_route_elects_a_face_before_casting_to_the_stack() {
        let (identity, db) = fixture_identity_and_db();
        let mut state = canonical_witness();
        place_candidate(&mut state, &identity, Origin::Exile, &db, false, P0).unwrap();
        let source = prepare_source(&mut state, &identity, Profile::TargetFree).unwrap();
        let card_id = state.objects[&source].card_id;
        game_engine::apply(
            &mut state,
            P0,
            GameAction::CastSpell {
                object_id: source,
                card_id,
                targets: vec![CANDIDATE],
                payment_mode: Default::default(),
            },
        )
        .unwrap();
        let WaitingFor::TargetSelection { target_slots, .. } = &state.waiting_for else {
            panic!("source CastSpell did not open its public target prompt");
        };
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(CANDIDATE)));
        assert!(!candidate_offer_window_membership(&state));
        game_engine::apply(
            &mut state,
            P0,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(CANDIDATE)],
            },
        )
        .unwrap();
        assert!(matches!(&state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.objects[&source].zone, Zone::Stack);
        assert!(!candidate_offer_window_membership(&state));
        advance_source_to_candidate_window(
            &mut state,
            source,
            Profile::TargetFree,
            RouteProgress {
                source_target_prompt: true,
                source_target_action: true,
                ..RouteProgress::default()
            },
        )
        .unwrap();
        assert!(candidate_offer_window_membership(&state));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ModalFaceChoice { .. }
        ));
        game_engine::apply(
            &mut state,
            P0,
            GameAction::ChooseModalFace { back_face: false },
        )
        .unwrap();
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.objects[&CANDIDATE].zone, Zone::Stack);
        let candidate_entries: Vec<_> = state
            .stack
            .iter()
            .filter(|entry| entry.id == CANDIDATE && entry.source_id == CANDIDATE)
            .collect();
        assert_eq!(candidate_entries.len(), 1);
        assert!(matches!(
            &candidate_entries[0].kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Normal,
                ..
            }
        ));
        assert!(matches!(
            candidate_stack_endpoint(&state),
            Ok(CandidateStackEndpoint::Spell(CastingVariant::Normal))
        ));
        assert_ne!(state.objects[&source].zone, Zone::Stack);
    }

    #[test]
    fn source_advance_keeps_clean_no_offer_distinct_from_route_error() {
        let mut state = canonical_witness();
        assert_eq!(
            advance_source_to_candidate_window(
                &mut state,
                ObjectId(9_999),
                Profile::Cascade,
                RouteProgress::default(),
            ),
            Ok(SourceAdvance::NoOffer),
            "a clean priority endpoint without an offer is census data"
        );

        let error = advance_source_to_candidate_window(
            &mut state,
            ObjectId(9_999),
            Profile::Cascade,
            RouteProgress {
                source_target_action: true,
                source_target_prompt: false,
                ..RouteProgress::default()
            },
        )
        .expect_err("an impossible public route remains an error");
        assert!(error.contains("target action occurred without its public prompt"));
    }

    #[test]
    fn window_opponent_targets_are_publicly_ordered_once_and_stack_endpoint_is_explicit() {
        assert_eq!(
            source_targets(Profile::WindowOpponent),
            Some(vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(CANDIDATE)
            ])
        );
        let mut state = canonical_witness();
        let mut progress = RouteProgress {
            offer_accepted: true,
            ..RouteProgress::default()
        };
        assert!(accept_offer(&mut state, Profile::Cascade, &mut progress).is_err());

        insert_support(
            &mut state,
            CANDIDATE.0,
            P0,
            Zone::Stack,
            "Candidate stack endpoint",
            &[CoreType::Instant],
            0,
        );
        let mut entry = state.stack.back().unwrap().clone();
        entry.id = CANDIDATE;
        entry.source_id = CANDIDATE;
        state.stack.push_back(entry);
        assert!(matches!(
            candidate_stack_endpoint(&state),
            Ok(CandidateStackEndpoint::Spell(CastingVariant::Normal))
        ));
    }

    #[test]
    fn target_free_stack_endpoint_requires_one_normal_spell() {
        let mut state = canonical_witness();
        assert!(target_free_stack_endpoint(&state)
            .unwrap_err()
            .contains("missing"));

        let mut entry = state.stack.back().unwrap().clone();
        entry.id = CANDIDATE;
        entry.source_id = CANDIDATE;
        let StackEntryKind::Spell {
            casting_variant, ..
        } = &mut entry.kind
        else {
            panic!("canonical inert support must be a spell");
        };
        *casting_variant = CastingVariant::Fuse;
        state.stack.push_back(entry.clone());
        assert!(target_free_stack_endpoint(&state)
            .unwrap_err()
            .contains("non-Normal"));

        state.stack.pop_back();
        let StackEntryKind::Spell {
            casting_variant, ..
        } = &mut entry.kind
        else {
            unreachable!("entry was already established as a spell");
        };
        *casting_variant = CastingVariant::Normal;
        state.stack.push_back(entry.clone());
        state.stack.push_back(entry);
        assert!(target_free_stack_endpoint(&state)
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn exact_export_keys_preserve_back_face_and_back_face_fuse() {
        let (split, db) = fixture_identity_and_db();
        assert_eq!(split.canonical_name, "Fixture Front // Fixture Back");
        assert_eq!(split.front_key, "fixture-front-storage-key");
        assert_eq!(split.back_key, "fixture-back-storage-key");
        assert!(
            split.has_fuse,
            "a back-face Fuse must classify the identity"
        );
        let mut state = canonical_witness();
        place_candidate(&mut state, &split, Origin::Exile, &db, false, P0).unwrap();
        let candidate = state.objects.get(&CANDIDATE).unwrap();
        assert_eq!(candidate.name, "Fixture Front");
        assert_eq!(
            candidate
                .printed_ref
                .as_ref()
                .map(|printed_ref| printed_ref.oracle_id.as_str()),
            Some("fixture-split-oracle")
        );
        assert_eq!(
            candidate.back_face.as_ref().map(|face| face.name.as_str()),
            Some("Fixture Back")
        );
    }

    #[test]
    fn cascade_and_discover_seed_library_and_observe_exile_hit() {
        let (identity, db) = fixture_identity_and_db();
        let template = canonical_witness();

        for profile in [Profile::Cascade, Profile::Discover] {
            let observation = probe(&template, &identity, Origin::Exile, profile, &db).unwrap();
            assert!(observation.included, "{profile:?} did not offer its hit");
            assert_ne!(observation.front, Observation::RouteError);
            assert_ne!(observation.back, Observation::RouteError);
        }
    }

    #[test]
    fn future_modal_state_fields_do_not_affect_patterns() {
        let state = canonical_witness();
        let _ = matches!(&state.waiting_for, WaitingFor::ModalFaceChoice { .. });
    }

    #[test]
    fn export_identity_census_reports_non_candidates_without_widening_candidates() {
        let export = br#"{
            "front": {
                "scryfall_oracle_id": "candidate",
                "name": "Front",
                "layout": "split",
                "face_index": 0,
                "card_type": { "core_types": ["Instant"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }]
            },
            "back": {
                "scryfall_oracle_id": "candidate",
                "name": "Back",
                "layout": "split",
                "face_index": 1,
                "card_type": { "core_types": ["Sorcery"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }]
            },
            "ordinary": {
                "scryfall_oracle_id": "ordinary",
                "name": "Ordinary",
                "layout": "normal",
                "face_index": 0,
                "card_type": { "core_types": ["Creature"], "subtypes": [] },
                "abilities": []
            },
            "malformed": {
                "name": "Malformed",
                "layout": "split"
            }
        }"#;
        let (identities, diagnostics) = enumerate_identities(export).unwrap();

        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].oracle_id, "candidate");
        assert!(diagnostics.iter().any(|line| {
            line == "DIAGNOSTIC\tordinary\tExport\tidentity\taudited-face-count=0"
        }));
        assert!(diagnostics.iter().any(|line| {
            line == "DIAGNOSTIC\texport-key:malformed\tExport\tidentity\taudited-face-count=0"
        }));
        assert!(diagnostics.iter().any(|line| {
            line == "DIAGNOSTIC\texport-key:malformed\tExport\tentry\tmissing-oracle-id"
        }));
    }

    #[test]
    fn raw_keyword_shapes_only_classify_the_exact_fuse_string() {
        let export = br#"{
            "missing-front": {
                "scryfall_oracle_id": "missing",
                "name": "Missing Front",
                "layout": "split",
                "face_index": 0,
                "card_type": { "core_types": ["Instant"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }]
            },
            "irrelevant-back": {
                "scryfall_oracle_id": "missing",
                "name": "Irrelevant Back",
                "layout": "split",
                "face_index": 1,
                "card_type": { "core_types": ["Sorcery"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }],
                "keywords": ["Flying"]
            },
            "malformed-front": {
                "scryfall_oracle_id": "malformed",
                "name": "Malformed Front",
                "layout": "split",
                "face_index": 0,
                "card_type": { "core_types": ["Instant"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }],
                "keywords": { "type": "Fuse" }
            },
            "malformed-back": {
                "scryfall_oracle_id": "malformed",
                "name": "Malformed Back",
                "layout": "split",
                "face_index": 1,
                "card_type": { "core_types": ["Sorcery"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }],
                "keywords": ["Haste"]
            }
        }"#;

        let (identities, diagnostics) = enumerate_identities(export).unwrap();
        assert!(
            diagnostics.is_empty(),
            "keyword fixtures remain valid identities"
        );
        assert_eq!(identities.len(), 2);
        assert!(
            identities.iter().all(|identity| !identity.has_fuse),
            "missing, malformed, and unrelated keyword shapes are not Fuse"
        );
    }

    #[test]
    fn export_identity_census_excludes_mixed_audited_layout_pairs() {
        let export = br#"{
            "front": {
                "scryfall_oracle_id": "mixed-layout",
                "name": "Front",
                "layout": "modal_dfc",
                "face_index": 0,
                "card_type": { "core_types": ["Instant"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }]
            },
            "back": {
                "scryfall_oracle_id": "mixed-layout",
                "name": "Back",
                "layout": "split",
                "face_index": 1,
                "card_type": { "core_types": ["Sorcery"], "subtypes": [] },
                "abilities": [{ "kind": "Spell" }]
            }
        }"#;

        let (identities, diagnostics) = enumerate_identities(export).unwrap();

        assert!(identities.is_empty());
        assert_eq!(
            diagnostics,
            vec![
                "DIAGNOSTIC\tmixed-layout\tExport\tidentity\tmixed-audited-face-layouts=modal_dfc,split"
                    .to_string()
            ]
        );
    }
}
