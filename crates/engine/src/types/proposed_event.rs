use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::counter::CounterType;

use super::ability::{Duration, StaticDefinition, TargetRef};
use super::card_type::{CoreType, Supertype};
use super::identifiers::ObjectId;
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaType};
use super::phase::Phase;
use super::player::PlayerId;
use super::zones::Zone;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplacementId {
    pub source: ObjectId,
    pub index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum EtbTapState {
    #[default]
    Unspecified,
    Tapped,
    Untapped,
}

impl EtbTapState {
    pub fn from_seeded_tapped(tapped: bool) -> Self {
        if tapped {
            Self::Tapped
        } else {
            Self::Unspecified
        }
    }

    /// Resolve to a concrete tapped state. `fallback` is used only when no
    /// replacement has set an explicit tap-state (`Unspecified`). For
    /// `ZoneChange` events pass `false`; for `CreateToken` pass
    /// `spec.tapped` (the token spec's authored default).
    pub fn resolve(self, fallback: bool) -> bool {
        match self {
            Self::Unspecified => fallback,
            Self::Tapped => true,
            Self::Untapped => false,
        }
    }
}

/// CR 111.1 + CR 111.4 + CR 111.10: Fully-resolved token creation specification.
///
/// `Effect::Token` carries authoring-time fields (`PtValue`, `QuantityExpr`,
/// `TargetFilter owner`) that must be resolved against game state before the
/// token hits the replacement pipeline. `TokenSpec` captures the resolved,
/// self-describing form used by `ProposedEvent::CreateToken` and the
/// post-accept apply path, so replacement matchers and modifiers see the full
/// characteristics of the token that's about to be created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSpec {
    /// CR 111.4: The token's display name (same as its subtype(s) + "Token"
    /// unless the creating effect specifies otherwise).
    pub display_name: String,
    /// Original Forge-style script name (or custom name) used by the token
    /// parser on the apply path to re-derive attributes. Preserved so the
    /// existing `parse_token_script` dispatch still fires after widening.
    pub script_name: String,
    /// CR 208.2: Fixed power, or `None` for non-creature tokens.
    pub power: Option<i32>,
    /// CR 208.2: Fixed toughness, or `None` for non-creature tokens.
    pub toughness: Option<i32>,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub colors: Vec<ManaColor>,
    pub keywords: Vec<Keyword>,
    /// CR 113.3d: Static abilities granted to the token (e.g., "This token
    /// can't block.").
    pub static_abilities: Vec<StaticDefinition>,
    /// CR 122.6a: Counters placed on the token as it enters the battlefield
    /// (resolved from `QuantityExpr` at propose time).
    pub enter_with_counters: Vec<(String, u32)>,
    /// CR 614.1: Token enters tapped.
    pub tapped: bool,
    /// CR 508.4: Token enters the battlefield attacking (not declared as
    /// attacker).
    pub enters_attacking: bool,
    /// CR 603.7: When set, the token is sacrificed at the end of the given
    /// duration (e.g., Mobilize tokens sacrificed at end of combat).
    pub sacrifice_at: Option<Duration>,
    /// CR 107.3a: Ability source — the object that created the token. Needed
    /// on the apply path for defending-player resolution (`enters_attacking`)
    /// and for the delayed-trigger source.
    pub source_id: ObjectId,
    /// CR 107.3a: Ability controller — the player who controls the effect
    /// creating the token (distinct from `owner`, the player to whom the
    /// token belongs).
    pub controller: PlayerId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProposedEvent {
    ZoneChange {
        object_id: ObjectId,
        from: Zone,
        to: Zone,
        cause: Option<ObjectId>,
        /// Explicit ETB tap-state override carried through the replacement pipeline.
        /// `Unspecified` preserves any non-replacement tapped seed from the originating effect.
        #[serde(default)]
        enter_tapped: EtbTapState,
        /// Counters to place on this permanent as it enters the battlefield.
        /// Each entry is (counter_type_string, count). Set by ETB-counter replacements.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(String, u32)>,
        /// Override the controller on ETB. Used by Earthbending return ("under your control")
        /// and other "enters the battlefield under [player]'s control" effects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller_override: Option<PlayerId>,
        /// CR 712.2: When true, the object enters the battlefield showing its back face.
        /// Set by "return ... transformed" effects.
        #[serde(default)]
        enter_transformed: bool,
        applied: HashSet<ReplacementId>,
    },
    Damage {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
        is_combat: bool,
        applied: HashSet<ReplacementId>,
    },
    Draw {
        player_id: PlayerId,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    LifeGain {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<ReplacementId>,
    },
    LifeLoss {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<ReplacementId>,
    },
    AddCounter {
        #[serde(default)]
        actor: PlayerId,
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    RemoveCounter {
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    /// CR 111.1 + CR 614.1a: Token creation event carrying the full
    /// self-describing token specification. Replacement effects can modify
    /// `count` (Doubling Season, Primal Vigor) or inspect `spec` for
    /// characteristic-based gating (e.g., "whenever a creature token you
    /// control would enter ...").
    ///
    /// `spec` is boxed so this variant doesn't dominate the enum size —
    /// `TokenSpec` is ~400 bytes of resolved characteristics, and most
    /// other variants are small IDs.
    CreateToken {
        owner: PlayerId,
        /// Resolved token characteristics, keyed by replacement pipeline
        /// matchers on the apply path to reproduce the token faithfully.
        spec: Box<TokenSpec>,
        /// Explicit ETB tap-state override carried through the replacement pipeline.
        /// `Unspecified` preserves the token spec's authored `tapped` bit.
        #[serde(default)]
        enter_tapped: EtbTapState,
        /// CR 614.1a: Number of tokens to create. May be modified by replacement effects.
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    Discard {
        player_id: PlayerId,
        object_id: ObjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
        applied: HashSet<ReplacementId>,
    },
    Tap {
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Untap {
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Destroy {
        object_id: ObjectId,
        source: Option<ObjectId>,
        /// CR 701.19c: When true, regeneration shields cannot prevent this destruction.
        cant_regenerate: bool,
        applied: HashSet<ReplacementId>,
    },
    Sacrifice {
        object_id: ObjectId,
        player_id: PlayerId,
        applied: HashSet<ReplacementId>,
    },
    /// CR 500.1 + CR 614.1b + CR 614.10: A turn is about to begin. Carried
    /// through the replacement pipeline so condition-gated skip effects
    /// (e.g., Stranglehold's "skip extra turns") can prevent the turn.
    ///
    /// `is_extra_turn` is true when this turn was granted by an effect
    /// (CR 500.7 — popped from `state.extra_turns`).
    BeginTurn {
        player_id: PlayerId,
        is_extra_turn: bool,
        applied: HashSet<ReplacementId>,
    },
    /// CR 500.1 + CR 614.1b: A phase/step is about to begin. Carried through
    /// the replacement pipeline so condition-gated skip effects can prevent
    /// the phase. Simple static-based skips (`StaticMode::SkipStep`) continue
    /// to short-circuit earlier in `turns.rs`; this pipeline path handles
    /// event-context-aware replacements.
    BeginPhase {
        player_id: PlayerId,
        phase: Phase,
        applied: HashSet<ReplacementId>,
    },
    /// CR 106.3 + CR 614.1a: Mana is about to be produced by a source and added
    /// to a player's mana pool. Carried through the replacement pipeline so
    /// static effects like Contamination ("produces {B} instead") can replace
    /// the produced mana type or amount before it enters the pool.
    ProduceMana {
        source_id: ObjectId,
        player_id: PlayerId,
        mana_type: ManaType,
        /// CR 106.3: Number of mana units of `mana_type` this event produces.
        #[serde(default = "default_produce_mana_count")]
        count: u32,
        /// CR 106.12: True when this production comes from activating a mana
        /// ability with the tap symbol in its cost.
        #[serde(default)]
        tapped_for_mana: bool,
        applied: HashSet<ReplacementId>,
    },
}

fn default_produce_mana_count() -> u32 {
    1
}

impl ProposedEvent {
    /// Construct a `ZoneChange` with default `enter_tapped: Unspecified` and empty `applied` set.
    pub fn zone_change(object_id: ObjectId, from: Zone, to: Zone, cause: Option<ObjectId>) -> Self {
        Self::ZoneChange {
            object_id,
            from,
            to,
            cause,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: HashSet::new(),
        }
    }

    /// CR 500.1 + CR 614.1b: Construct a `BeginTurn` proposed event.
    pub fn begin_turn(player_id: PlayerId, is_extra_turn: bool) -> Self {
        Self::BeginTurn {
            player_id,
            is_extra_turn,
            applied: HashSet::new(),
        }
    }

    /// CR 500.1 + CR 614.1b: Construct a `BeginPhase` proposed event.
    pub fn begin_phase(player_id: PlayerId, phase: Phase) -> Self {
        Self::BeginPhase {
            player_id,
            phase,
            applied: HashSet::new(),
        }
    }

    /// CR 106.3 + CR 614.1a: Construct a `ProduceMana` proposed event.
    pub fn produce_mana(source_id: ObjectId, player_id: PlayerId, mana_type: ManaType) -> Self {
        Self::produce_mana_with_context(source_id, player_id, mana_type, false)
    }

    /// CR 106.3 + CR 106.12 + CR 614.1a: Construct a `ProduceMana` proposed
    /// event while preserving whether the mana was produced by tapping the
    /// source for mana.
    pub fn produce_mana_with_context(
        source_id: ObjectId,
        player_id: PlayerId,
        mana_type: ManaType,
        tapped_for_mana: bool,
    ) -> Self {
        Self::ProduceMana {
            source_id,
            player_id,
            mana_type,
            count: 1,
            tapped_for_mana,
            applied: HashSet::new(),
        }
    }

    pub fn battlefield_entry_tap_state(&self) -> Option<EtbTapState> {
        match self {
            ProposedEvent::ZoneChange { enter_tapped, .. }
            | ProposedEvent::CreateToken { enter_tapped, .. } => Some(*enter_tapped),
            _ => None,
        }
    }

    pub fn battlefield_entry_tap_state_mut(&mut self) -> Option<&mut EtbTapState> {
        match self {
            ProposedEvent::ZoneChange { enter_tapped, .. }
            | ProposedEvent::CreateToken { enter_tapped, .. } => Some(enter_tapped),
            _ => None,
        }
    }

    pub fn applied_set(&self) -> &HashSet<ReplacementId> {
        match self {
            ProposedEvent::ZoneChange { applied, .. }
            | ProposedEvent::Damage { applied, .. }
            | ProposedEvent::Draw { applied, .. }
            | ProposedEvent::LifeGain { applied, .. }
            | ProposedEvent::LifeLoss { applied, .. }
            | ProposedEvent::AddCounter { applied, .. }
            | ProposedEvent::RemoveCounter { applied, .. }
            | ProposedEvent::CreateToken { applied, .. }
            | ProposedEvent::Discard { applied, .. }
            | ProposedEvent::Tap { applied, .. }
            | ProposedEvent::Untap { applied, .. }
            | ProposedEvent::Destroy { applied, .. }
            | ProposedEvent::Sacrifice { applied, .. }
            | ProposedEvent::BeginTurn { applied, .. }
            | ProposedEvent::BeginPhase { applied, .. }
            | ProposedEvent::ProduceMana { applied, .. } => applied,
        }
    }

    pub fn applied_set_mut(&mut self) -> &mut HashSet<ReplacementId> {
        match self {
            ProposedEvent::ZoneChange { applied, .. }
            | ProposedEvent::Damage { applied, .. }
            | ProposedEvent::Draw { applied, .. }
            | ProposedEvent::LifeGain { applied, .. }
            | ProposedEvent::LifeLoss { applied, .. }
            | ProposedEvent::AddCounter { applied, .. }
            | ProposedEvent::RemoveCounter { applied, .. }
            | ProposedEvent::CreateToken { applied, .. }
            | ProposedEvent::Discard { applied, .. }
            | ProposedEvent::Tap { applied, .. }
            | ProposedEvent::Untap { applied, .. }
            | ProposedEvent::Destroy { applied, .. }
            | ProposedEvent::Sacrifice { applied, .. }
            | ProposedEvent::BeginTurn { applied, .. }
            | ProposedEvent::BeginPhase { applied, .. }
            | ProposedEvent::ProduceMana { applied, .. } => applied,
        }
    }

    pub fn already_applied(&self, id: &ReplacementId) -> bool {
        self.applied_set().contains(id)
    }

    pub fn mark_applied(&mut self, id: ReplacementId) {
        self.applied_set_mut().insert(id);
    }

    pub fn affected_player(&self, state: &crate::types::game_state::GameState) -> PlayerId {
        match self {
            ProposedEvent::ZoneChange { object_id, .. }
            | ProposedEvent::Tap { object_id, .. }
            | ProposedEvent::Untap { object_id, .. }
            | ProposedEvent::Destroy { object_id, .. }
            | ProposedEvent::AddCounter { object_id, .. }
            | ProposedEvent::RemoveCounter { object_id, .. } => state
                .objects
                .get(object_id)
                .map(|o| o.controller)
                .unwrap_or(PlayerId(0)),
            ProposedEvent::Damage { target, .. } => match target {
                TargetRef::Player(pid) => *pid,
                TargetRef::Object(oid) => state
                    .objects
                    .get(oid)
                    .map(|o| o.controller)
                    .unwrap_or(PlayerId(0)),
            },
            ProposedEvent::Draw { player_id, .. }
            | ProposedEvent::LifeGain { player_id, .. }
            | ProposedEvent::LifeLoss { player_id, .. }
            | ProposedEvent::Discard { player_id, .. }
            | ProposedEvent::Sacrifice { player_id, .. }
            | ProposedEvent::BeginTurn { player_id, .. }
            | ProposedEvent::BeginPhase { player_id, .. }
            | ProposedEvent::ProduceMana { player_id, .. } => *player_id,
            ProposedEvent::CreateToken { owner, .. } => *owner,
        }
    }

    /// Returns the primary object affected by this event, if any.
    pub fn affected_object_id(&self) -> Option<ObjectId> {
        match self {
            ProposedEvent::ZoneChange { object_id, .. }
            | ProposedEvent::Tap { object_id, .. }
            | ProposedEvent::Untap { object_id, .. }
            | ProposedEvent::Destroy { object_id, .. }
            | ProposedEvent::AddCounter { object_id, .. }
            | ProposedEvent::RemoveCounter { object_id, .. }
            | ProposedEvent::Discard { object_id, .. }
            | ProposedEvent::Sacrifice { object_id, .. } => Some(*object_id),
            // CR 106.3: The mana source (land being tapped) is the affected object —
            // this is what `valid_card` filters are matched against.
            ProposedEvent::ProduceMana { source_id, .. } => Some(*source_id),
            ProposedEvent::Damage { target, .. } => match target {
                TargetRef::Object(oid) => Some(*oid),
                TargetRef::Player(_) => None,
            },
            ProposedEvent::Draw { .. }
            | ProposedEvent::LifeGain { .. }
            | ProposedEvent::LifeLoss { .. }
            | ProposedEvent::CreateToken { .. }
            | ProposedEvent::BeginTurn { .. }
            | ProposedEvent::BeginPhase { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposed_event_has_16_variants() {
        // Verify all 16 variants compile
        let events: Vec<ProposedEvent> = vec![
            ProposedEvent::zone_change(ObjectId(1), Zone::Battlefield, Zone::Graveyard, None),
            ProposedEvent::Damage {
                source_id: ObjectId(1),
                target: TargetRef::Player(PlayerId(0)),
                amount: 3,
                is_combat: false,
                applied: HashSet::new(),
            },
            ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::LifeGain {
                player_id: PlayerId(0),
                amount: 3,
                applied: HashSet::new(),
            },
            ProposedEvent::LifeLoss {
                player_id: PlayerId(0),
                amount: 3,
                applied: HashSet::new(),
            },
            ProposedEvent::AddCounter {
                actor: PlayerId(0),
                object_id: ObjectId(1),
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::RemoveCounter {
                object_id: ObjectId(1),
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::CreateToken {
                owner: PlayerId(0),
                spec: Box::new(TokenSpec {
                    display_name: "Soldier".to_string(),
                    script_name: "w_1_1_soldier".to_string(),
                    power: Some(1),
                    toughness: Some(1),
                    core_types: Vec::new(),
                    subtypes: Vec::new(),
                    supertypes: Vec::new(),
                    colors: Vec::new(),
                    keywords: Vec::new(),
                    static_abilities: Vec::new(),
                    enter_with_counters: Vec::new(),
                    tapped: false,
                    enters_attacking: false,
                    sacrifice_at: None,
                    source_id: ObjectId(1),
                    controller: PlayerId(0),
                }),
                enter_tapped: EtbTapState::Unspecified,
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::Discard {
                player_id: PlayerId(0),
                object_id: ObjectId(2),
                source_id: None,
                applied: HashSet::new(),
            },
            ProposedEvent::Tap {
                object_id: ObjectId(1),
                applied: HashSet::new(),
            },
            ProposedEvent::Untap {
                object_id: ObjectId(1),
                applied: HashSet::new(),
            },
            ProposedEvent::Destroy {
                object_id: ObjectId(1),
                source: None,
                cant_regenerate: false,
                applied: HashSet::new(),
            },
            ProposedEvent::Sacrifice {
                object_id: ObjectId(1),
                player_id: PlayerId(0),
                applied: HashSet::new(),
            },
            ProposedEvent::begin_turn(PlayerId(0), false),
            ProposedEvent::begin_phase(PlayerId(0), Phase::Untap),
            ProposedEvent::produce_mana(ObjectId(1), PlayerId(0), ManaType::Green),
        ];
        assert_eq!(events.len(), 16);
    }

    #[test]
    fn replacement_id_equality_and_hash() {
        let id1 = ReplacementId {
            source: ObjectId(1),
            index: 0,
        };
        let id2 = ReplacementId {
            source: ObjectId(1),
            index: 0,
        };
        let id3 = ReplacementId {
            source: ObjectId(1),
            index: 1,
        };
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);

        let mut set = HashSet::new();
        set.insert(id1);
        assert!(set.contains(&id2));
        assert!(!set.contains(&id3));
    }

    #[test]
    fn mark_applied_and_already_applied() {
        let mut event = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        let rid = ReplacementId {
            source: ObjectId(5),
            index: 0,
        };
        assert!(!event.already_applied(&rid));
        event.mark_applied(rid);
        assert!(event.already_applied(&rid));
    }
}
