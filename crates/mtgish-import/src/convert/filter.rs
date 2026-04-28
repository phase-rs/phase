//! Permanents → engine `TargetFilter` (Phase 3 narrow slice).
//!
//! mtgish's `Permanents` enum has 350+ variants; only a handful are
//! needed to unlock the Phase 4b filter-payload keywords (Enchant,
//! Landwalk, Equip, Champion, Affinity). This module covers the common
//! shapes — full Permanents conversion lands with Phase 3.

use engine::types::ability::{
    Comparator, ControllerRef, FilterProp, QuantityExpr, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::ManaColor;

use crate::convert::quantity::convert as convert_quantity;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{
    ArtifactType, CardType, CardtypeVariable, CheckHasable, Color, Comparison, CounterType,
    CreatureType, CreatureTypeVariable, EnchantmentType, GameNumber, LandType, NameFilter,
    Permanent, Permanents, PlaneswalkerType, Player, Players, SuperType,
};

/// Translate a `Permanents` filter to an engine `TargetFilter`. Returns
/// `Err(MalformedIdiom)` for shapes we don't yet support, so callers can
/// degrade gracefully (the keyword arm in turn fails the rule).
pub fn convert(p: &Permanents) -> ConvResult<TargetFilter> {
    Ok(match p {
        Permanents::AnyPermanent => TargetFilter::Typed(TypedFilter::permanent()),
        Permanents::SinglePermanent(p) => convert_permanent(p)?,
        Permanents::IsPermanent => TargetFilter::Typed(TypedFilter::permanent()),
        // CR 301.5 + CR 303.4: "attached to ~" subject phrase. Both the
        // plural-arg `AttachedToAPermanent(Permanents)` and singular-arg
        // `AttachedToPermanent(Permanent)` collapse to the engine's
        // attached-to-source axis (`TargetFilter::AttachedTo`); the inner
        // permanent reference is the same Equipment/Aura whose ability is
        // resolving.
        Permanents::AttachedToAPermanent(_) | Permanents::AttachedToPermanent(_) => {
            TargetFilter::AttachedTo
        }

        // CR 115.1 / CR 608.2b: Resolution-time references to outer-Targeted
        // target slots. The outer `Actions::Targeted` provides the typed
        // targets; the inner action's `Ref_TargetPermanents{,1,2}` selects
        // which slot to bind. Engine-side these collapse to the generic
        // target axis (`TargetFilter::Any`); the typed constraint is enforced
        // when the targets are chosen.
        Permanents::Ref_TargetPermanents
        | Permanents::Ref_TargetPermanents1
        | Permanents::Ref_TargetPermanents2 => TargetFilter::Any,

        Permanents::IsCardtype(ct) => TargetFilter::Typed(TypedFilter::new(card_type(ct))),
        Permanents::IsNonCardtype(ct) => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Non(Box::new(card_type(ct)))))
        }
        Permanents::IsCreatureType(s) => TargetFilter::Typed(
            TypedFilter::creature().with_type(TypeFilter::Subtype(creature_type_name(s))),
        ),
        Permanents::IsSupertype(s) => TargetFilter::Typed(
            TypedFilter::permanent().with_type(TypeFilter::Subtype(supertype_name(s))),
        ),

        // CR 205.3i: Land subtypes (Forest, Plains, …, Cave, Gate, Lair, Sphere, etc.).
        Permanents::IsLandType(lt) => TargetFilter::Typed(
            TypedFilter::permanent()
                .with_type(TypeFilter::Land)
                .with_type(TypeFilter::Subtype(land_type_name(lt))),
        ),
        // CR 205.3g: Artifact subtypes (Equipment, Vehicle, Treasure, Food, …).
        Permanents::IsArtifactType(at) => TargetFilter::Typed(
            TypedFilter::permanent()
                .with_type(TypeFilter::Artifact)
                .with_type(TypeFilter::Subtype(artifact_type_name(at))),
        ),
        // CR 205.3h: Enchantment subtypes (Aura, Saga, Curse, Rune, …).
        Permanents::IsEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::permanent()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Subtype(enchantment_type_name(et))),
        ),

        // CR 105.1: Color filter — "white creature", "red permanent", etc.
        // The schema's `Color` enum includes runtime-chosen colors which
        // strict-fail; only the five concrete colors map to ManaColor.
        Permanents::IsColor(c) => match concrete_color(c) {
            Some(color) => TargetFilter::Typed(
                TypedFilter::permanent().properties(vec![FilterProp::HasColor { color }]),
            ),
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Permanents/IsColor",
                    path: String::new(),
                    detail: format!("non-concrete color: {c:?}"),
                });
            }
        },
        Permanents::IsNonColor(c) => match concrete_color(c) {
            Some(color) => TargetFilter::Not {
                filter: Box::new(TargetFilter::Typed(
                    TypedFilter::permanent().properties(vec![FilterProp::HasColor { color }]),
                )),
            },
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Permanents/IsNonColor",
                    path: String::new(),
                    detail: format!("non-concrete color: {c:?}"),
                });
            }
        },

        // CR 111.6: Token filter — predicate "is a token".
        Permanents::IsToken => {
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Token]))
        }

        // CR 613.7: "Other [filter]" — exclude self via FilterProp::Another.
        // The inner filter is conjuncted with the Another property.
        Permanents::Other(inner) => match convert_permanent(inner)? {
            TargetFilter::Typed(tf) => {
                let mut props = tf.properties.clone();
                if !props.iter().any(|p| matches!(p, FilterProp::Another)) {
                    props.push(FilterProp::Another);
                }
                TargetFilter::Typed(tf.properties(props))
            }
            // Inner refers to a specific runtime object (SelfRef etc.).
            // Synthesize a permanent-typed filter with Another.
            _ => {
                TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Another]))
            }
        },

        // CR 109.4: Owner-scoped filter. We model owner as the controller
        // axis since for permanents owner == controller in the absence of
        // GainControl effects; engine `TargetFilter` does not yet have a
        // distinct owner axis.
        Permanents::OwnedByAPlayer(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::permanent().controller(ctrl))
        }

        Permanents::ControlledByAPlayer(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::permanent().controller(ctrl))
        }
        Permanents::ControlledByPlayer(player) => {
            let ctrl = player_to_controller(player)?;
            TargetFilter::Typed(TypedFilter::permanent().controller(ctrl))
        }

        Permanents::And(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(convert(part)?);
            }
            TargetFilter::And { filters }
        }
        Permanents::Or(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(convert(part)?);
            }
            TargetFilter::Or { filters }
        }
        Permanents::Not(inner) => TargetFilter::Not {
            filter: Box::new(convert(inner)?),
        },

        // -------------------------------------------------------------------
        // Status predicates — CR 110.5 status categories + CR 508/509 combat
        // -------------------------------------------------------------------

        // CR 110.5: tapped/untapped status.
        Permanents::IsTapped => prop_filter(FilterProp::Tapped),
        Permanents::IsUntapped => prop_filter(FilterProp::Untapped),

        // CR 508.1k: a creature is "attacking" once declared as an attacker.
        Permanents::IsAttacking => prop_filter(FilterProp::Attacking),
        // CR 509.1g: a creature is "blocking" once declared as a blocker.
        Permanents::IsBlocking => prop_filter(FilterProp::Blocking),
        // CR 509.1h: an attacking creature with no blockers is "unblocked".
        Permanents::IsUnblocked => prop_filter(FilterProp::Unblocked),

        // CR 110.5 (face up/face down) + CR 707.2: face-down permanents.
        Permanents::IsFaceDown => prop_filter(FilterProp::FaceDown),

        // CR 111.6: token / nontoken predicate.
        Permanents::IsNonToken => TargetFilter::Not {
            filter: Box::new(prop_filter(FilterProp::Token)),
        },

        // CR 105.2c: colorless filter.
        Permanents::IsColorless => prop_filter(FilterProp::Colorless),
        // CR 105.1: multicolored — has 2+ colors.
        Permanents::IsMulticolored => prop_filter(FilterProp::Multicolored),

        // CR 700.9: "modified" — has counters, is equipped, or enchanted by
        // an Aura its controller controls.
        Permanents::IsModified => prop_filter(FilterProp::Modified),
        // CR 700.6: "historic" — legendary, artifact, or Saga.
        Permanents::IsHistoric => prop_filter(FilterProp::Historic),
        // CR 701.60b: suspected creatures.
        Permanents::IsSuspected => prop_filter(FilterProp::Suspected),

        // CR 903.3 / CR 903.3d: commander predicate. "Your commander" requires
        // a You-controlled commander; "is a commander" matches any commander.
        Permanents::IsACommander => prop_filter(FilterProp::IsCommander),
        Permanents::IsYourCommander => TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::IsCommander]),
        ),

        // CR 400.7: object entered the battlefield this turn.
        Permanents::EnteredTheBattlefieldThisTurn => prop_filter(FilterProp::EnteredThisTurn),
        // CR 508.1a: creature attacked this turn.
        Permanents::AttackedThisTurn => prop_filter(FilterProp::AttackedThisTurn),
        // CR 509.1a: creature blocked this turn.
        Permanents::BlockedThisTurn => prop_filter(FilterProp::BlockedThisTurn),
        // CR 510.1: object was dealt damage during this turn.
        Permanents::WasDealtDamageThisTurn => prop_filter(FilterProp::WasDealtDamageThisTurn),

        // CR 110.1 + CR 403.1: the battlefield is the zone for permanents.
        // "Permanent on the battlefield" is the default scope; the variant is
        // a no-op disambiguator for natural-language Oracle phrasing.
        Permanents::OnTheBattlefield => TargetFilter::Typed(TypedFilter::permanent()),

        // -------------------------------------------------------------------
        // Attachment predicates — CR 301.5 (Equipment) / CR 303.4 (Aura)
        // -------------------------------------------------------------------

        // CR 303.4: enchanted permanent — has at least one Aura attached.
        Permanents::IsEnchanted => TargetFilter::Typed(TypedFilter::permanent().properties(vec![
            FilterProp::HasAttachment {
                kind: engine::types::ability::AttachmentKind::Aura,
                controller: None,
            },
        ])),
        // CR 301.5: equipped creature — has at least one Equipment attached.
        Permanents::IsEquipped => TargetFilter::Typed(TypedFilter::permanent().properties(vec![
            FilterProp::HasAttachment {
                kind: engine::types::ability::AttachmentKind::Equipment,
                controller: None,
            },
        ])),

        // -------------------------------------------------------------------
        // Counters — CR 122
        // -------------------------------------------------------------------

        // CR 122.1: at least one counter of the given kind on it.
        Permanents::HasACounterOfType(ct) => {
            let counter_type = counter_type_to_engine(ct)?;
            TargetFilter::Typed(
                TypedFilter::permanent().properties(vec![FilterProp::CountersGE {
                    counter_type,
                    count: QuantityExpr::Fixed { value: 1 },
                }]),
            )
        }
        // CR 122.1: any kind of counter on it.
        Permanents::HasACounter => prop_filter(FilterProp::HasAnyCounter),
        // CR 122.1: zero counters of the given kind on it.
        Permanents::HasNoCountersOfType(ct) => {
            let counter_type = counter_type_to_engine(ct)?;
            TargetFilter::Not {
                filter: Box::new(TargetFilter::Typed(TypedFilter::permanent().properties(
                    vec![FilterProp::CountersGE {
                        counter_type,
                        count: QuantityExpr::Fixed { value: 1 },
                    }],
                ))),
            }
        }
        // CR 122.1 + CR 700.4: comparison on counter count.
        Permanents::HasNumCountersOfType(cmp, ct) => {
            let counter_type = counter_type_to_engine(ct)?;
            counter_comparison_filter(counter_type, cmp)?
        }

        // -------------------------------------------------------------------
        // Power / toughness / mana value — CR 208.1 / CR 202.3
        // -------------------------------------------------------------------
        Permanents::PowerIs(cmp) => power_comparison_filter(cmp)?,
        Permanents::ToughnessIs(cmp) => toughness_comparison_filter(cmp)?,
        Permanents::ManaValueIs(cmp) => cmc_comparison_filter(cmp)?,

        // -------------------------------------------------------------------
        // Keyword presence — CR 702
        // -------------------------------------------------------------------
        Permanents::HasAbility(check) => has_ability_filter(check)?,
        Permanents::DoesntHaveAbility(check) => TargetFilter::Not {
            filter: Box::new(has_ability_filter(check)?),
        },

        // -------------------------------------------------------------------
        // Type-negation predicates — CR 205.4b
        // -------------------------------------------------------------------
        Permanents::IsNonCreatureType(s) => TargetFilter::Typed(TypedFilter::creature().with_type(
            TypeFilter::Non(Box::new(TypeFilter::Subtype(creature_type_name(s)))),
        )),
        Permanents::IsNonArtifactType(at) => TargetFilter::Typed(
            TypedFilter::permanent()
                .with_type(TypeFilter::Artifact)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    artifact_type_name(at),
                )))),
        ),
        Permanents::IsNonEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::permanent()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    enchantment_type_name(et),
                )))),
        ),
        Permanents::IsNonLandType(lt) => TargetFilter::Typed(
            TypedFilter::permanent()
                .with_type(TypeFilter::Land)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    land_type_name(lt),
                )))),
        ),
        // CR 205.4a: nonbasic, nonlegendary, etc.
        Permanents::IsNonSupertype(s) => {
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::NotSupertype {
                    value: supertype_to_engine(s),
                },
            ]))
        }

        // CR 205.3j: Planeswalker subtypes (Jace, Chandra, …).
        Permanents::IsPlaneswalkerType(pt) => TargetFilter::Typed(
            TypedFilter::permanent()
                .with_type(TypeFilter::Planeswalker)
                .with_type(TypeFilter::Subtype(planeswalker_type_name(pt))),
        ),

        // -------------------------------------------------------------------
        // Names — CR 201.2
        // -------------------------------------------------------------------
        Permanents::IsNamed(nf) => name_filter_to_filter(nf)?,
        // CR 201.2: negated name predicate.
        Permanents::IsNotNamed(nf) => TargetFilter::Not {
            filter: Box::new(name_filter_to_filter(nf)?),
        },

        // CR 903.3 + CR 903.3d: negated commander predicate ("noncommander").
        Permanents::IsNotACommander => TargetFilter::Not {
            filter: Box::new(prop_filter(FilterProp::IsCommander)),
        },

        // -------------------------------------------------------------------
        // Combat negations — CR 508.1a / CR 509.1a
        // -------------------------------------------------------------------
        // CR 508.1k: "isn't attacking" — negation of the attacking status.
        Permanents::IsntAttacking => TargetFilter::Not {
            filter: Box::new(prop_filter(FilterProp::Attacking)),
        },
        // CR 509.1g: "isn't blocking" — negation of the blocking status.
        Permanents::IsntBlocking => TargetFilter::Not {
            filter: Box::new(prop_filter(FilterProp::Blocking)),
        },
        // CR 508.1a: "didn't attack this turn" — negation of attacked-this-turn.
        Permanents::DidntAttackThisTurn => TargetFilter::Not {
            filter: Box::new(prop_filter(FilterProp::AttackedThisTurn)),
        },
        // CR 400.7: "didn't enter the battlefield this turn" — negation of EnteredThisTurn.
        Permanents::DidntEnterTheBattlefieldThisTurn => TargetFilter::Not {
            filter: Box::new(prop_filter(FilterProp::EnteredThisTurn)),
        },

        // CR 508.1b: "attacking you" — `FilterProp::AttackingController` matches
        // attackers whose defending player equals the filter's source controller.
        // Only safely expressible when the player axis is `You`; other player
        // axes (specific opponent / chosen player) need source-relative defender
        // resolution we don't have a primitive for, so they strict-fail.
        Permanents::IsAttackingPlayer(player) => match player_to_controller(player)? {
            ControllerRef::You => TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::AttackingController]),
            ),
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Permanents/convert",
                    path: String::new(),
                    detail: format!("IsAttackingPlayer with non-You axis: {other:?}"),
                });
            }
        },
        // CR 508.1b: "attacking a player" with a Players axis. Same source-
        // controller restriction as the singular form — we can express
        // "attacking you" via `AttackingController`, but a free-standing
        // attacker filter (any defending player) has no engine primitive.
        Permanents::IsAttackingAPlayer(players) => match players_to_controller(players)? {
            ControllerRef::You => TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::AttackingController]),
            ),
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Permanents/convert",
                    path: String::new(),
                    detail: format!("IsAttackingAPlayer with non-You axis: {other:?}"),
                });
            }
        },

        // "Except for ~" set-difference subject — semantically the
        // complement of the inner filter. mtgish surfaces this distinct from
        // `Not` for natural-language fidelity, but the engine has no separate
        // primitive: "except for X" is the same set as "any permanent that
        // is not X" (Not on the inner filter).
        Permanents::ExceptFor(inner) => TargetFilter::Not {
            filter: Box::new(convert(inner)?),
        },

        // CR 201.4 + CR 700.2: "of the chosen type" / "of the chosen creature
        // type" / "of the chosen card type" — read the source's
        // `ChosenAttribute` slot at runtime. The native parser maps these to
        // `FilterProp::IsChosenCreatureType` / `IsChosenCardType` (Adaptive
        // Automaton, An-Zerrin Ruins, Cavern of Souls, Archon of Valor's
        // Reach, Metallic Mimic class). Other CreatureTypeVariable shapes
        // (`TheNotedCreatureType`, exiled-card subtype binding, plural
        // chosen-creature-types) require multi-source ChosenAttribute
        // wiring the engine doesn't expose as a single FilterProp.
        Permanents::IsCreatureTypeVariable(CreatureTypeVariable::TheChosenCreatureType) => {
            TargetFilter::Typed(
                TypedFilter::permanent().properties(vec![FilterProp::IsChosenCreatureType]),
            )
        }
        Permanents::IsNonCreatureTypeVariable(CreatureTypeVariable::TheChosenCreatureType) => {
            TargetFilter::Not {
                filter: Box::new(TargetFilter::Typed(
                    TypedFilter::permanent().properties(vec![FilterProp::IsChosenCreatureType]),
                )),
            }
        }
        Permanents::IsCardtypeVariable(CardtypeVariable::TheChosenCardtype) => TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::IsChosenCardType]),
        ),

        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "FilterProp",
                needed_variant: format!("Permanents::{}", variant_tag(other)),
            });
        }
    })
}

/// Build a battlefield-scoped TypedFilter carrying a single property.
/// The most common shape — used by every status predicate.
fn prop_filter(prop: FilterProp) -> TargetFilter {
    TargetFilter::Typed(TypedFilter::permanent().properties(vec![prop]))
}

/// Special-purpose: extract a single subtype string from a Permanents
/// filter, used by `Keyword::Landwalk(String)` and `Keyword::Champion(String)`
/// where the engine encodes the subtype as a bare name. Recognises
/// `IsCreatureType(s)` and `And([IsCardtype(_), IsCreatureType(s)])`
/// (the canonical "Mountainwalk" shape: `Land + Mountain`).
pub fn extract_subtype(p: &Permanents) -> ConvResult<String> {
    match p {
        Permanents::IsCreatureType(s) => Ok(creature_type_name(s)),
        Permanents::IsSupertype(s) => Ok(supertype_name(s)),
        // CR 702.14a + CR 702.14c: Landwalk uses the land subtype name as the
        // walk-quality label ("Swamp" → swampwalk, "Forest" → forestwalk,
        // etc.). The native parser stores these as capitalized strings
        // (matched by `parse_keyword_from_oracle_landwalk_variants`), so
        // we render `LandType` via Debug to produce "Swamp", "Island",
        // "Forest", "Mountain", "Plains", "Desert", "Cave", "Gate", etc.
        Permanents::IsLandType(lt) => Ok(land_type_name(lt)),
        // CR 702.14c: "nonbasic landwalk" (defending player controls a land
        // without the specified supertype) — encoded by the native parser
        // (and by `combat.rs::keywords_for_object`) as the literal label
        // "Nonbasic". mtgish encodes this as `IsNonSupertype(Basic)`; any
        // other supertype here is not a recognized landwalk variant.
        Permanents::IsNonSupertype(SuperType::Basic) => Ok("Nonbasic".to_string()),
        Permanents::And(parts) => {
            for part in parts {
                if let Ok(s) = extract_subtype(part) {
                    return Ok(s);
                }
            }
            Err(no_subtype_gap(p))
        }
        _ => Err(no_subtype_gap(p)),
    }
}

fn no_subtype_gap(p: &Permanents) -> ConversionGap {
    ConversionGap::MalformedIdiom {
        idiom: "Permanents/extract_subtype",
        path: String::new(),
        detail: format!("no subtype in: {}", variant_tag(p)),
    }
}

pub(crate) fn card_type(ct: &CardType) -> TypeFilter {
    use CardType as C;
    match ct {
        C::Artifact => TypeFilter::Artifact,
        C::Battle => TypeFilter::Battle,
        C::Creature => TypeFilter::Creature,
        C::Enchantment => TypeFilter::Enchantment,
        C::Instant => TypeFilter::Instant,
        C::Land => TypeFilter::Land,
        C::Planeswalker => TypeFilter::Planeswalker,
        C::Sorcery => TypeFilter::Sorcery,
        // Less-common card types collapse to a generic Subtype string;
        // engine TypeFilter doesn't enumerate them but Subtype matching
        // works at the typeline level.
        C::Conspiracy => TypeFilter::Subtype("Conspiracy".into()),
        C::Dungeon => TypeFilter::Subtype("Dungeon".into()),
        C::Kindred => TypeFilter::Subtype("Kindred".into()),
        C::Phenomenon => TypeFilter::Subtype("Phenomenon".into()),
        C::Plane => TypeFilter::Subtype("Plane".into()),
        C::Scheme => TypeFilter::Subtype("Scheme".into()),
        C::Vanguard => TypeFilter::Subtype("Vanguard".into()),
    }
}

pub(crate) fn land_type_name(lt: &LandType) -> String {
    match lt {
        LandType::PowerPlant => "Power-Plant".to_string(),
        LandType::Urzas => "Urza's".to_string(),
        other => format!("{other:?}"),
    }
}

pub(crate) fn artifact_type_name(at: &ArtifactType) -> String {
    format!("{at:?}")
}

pub(crate) fn enchantment_type_name(et: &EnchantmentType) -> String {
    format!("{et:?}")
}

pub(crate) fn concrete_color(c: &Color) -> Option<ManaColor> {
    Some(match c {
        Color::White => ManaColor::White,
        Color::Blue => ManaColor::Blue,
        Color::Black => ManaColor::Black,
        Color::Red => ManaColor::Red,
        Color::Green => ManaColor::Green,
        // Colorless / chosen-color refs are not concrete; caller decides.
        _ => return None,
    })
}

pub(crate) fn supertype_name(s: &SuperType) -> String {
    match s {
        SuperType::Basic => "Basic".into(),
        SuperType::Legendary => "Legendary".into(),
        SuperType::Ongoing => "Ongoing".into(),
        SuperType::Snow => "Snow".into(),
        SuperType::World => "World".into(),
    }
}

/// Render a CreatureType variant name as its display string. Schema
/// generates ~250 PascalCase identifiers which match their display form
/// 1:1 (Mountain, Wizard, Goblin, etc.). Falls back to `Debug` for the
/// long tail.
pub(crate) fn creature_type_name(s: &CreatureType) -> String {
    format!("{s:?}")
}

pub(crate) fn players_to_controller(players: &Players) -> ConvResult<ControllerRef> {
    match players {
        Players::SinglePlayer(p) => player_to_controller(p),
        Players::Opponent => Ok(ControllerRef::Opponent),
        Players::AnyPlayer => Ok(ControllerRef::TargetPlayer),
        // CR 109.5: "every player other than X" — when X is You, the
        // remaining set is exactly the opponents. Other Player axes don't
        // collapse to a single ControllerRef and strict-fail below.
        Players::Other(p) | Players::OpponentOf(p) if matches!(p.as_ref(), Player::You) => {
            Ok(ControllerRef::Opponent)
        }
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ControllerRef",
            needed_variant: format!("Players::{}", players_variant_tag(other)),
        }),
    }
}

pub(crate) fn player_to_controller(player: &Player) -> ConvResult<ControllerRef> {
    match player {
        Player::You => Ok(ControllerRef::You),
        // CR 109.4 + CR 115.1: "controlled by target player" — at resolution
        // time, ControllerRef::TargetPlayer reads the first TargetRef::Player
        // from the enclosing ability's target slot, which the outer Targeted
        // action provides (Anathemancer, Brigid, Ajani Vengeant, etc.).
        Player::Ref_TargetPlayer
        | Player::Ref_TargetPlayer1
        | Player::Ref_TargetPlayer2
        | Player::Ref_TargetPlayer3 => Ok(ControllerRef::TargetPlayer),
        // CR 109.4: "controlled by you" anaphors. The host of the resolving
        // ability is, by CR 109.4, "you" from the source's frame. SelfPlayer
        // is the same identity in mtgish's emit model.
        Player::HostPlayer | Player::HostController | Player::SelfPlayer => Ok(ControllerRef::You),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ControllerRef",
            needed_variant: format!("Player::{}", player_variant_tag(other)),
        }),
    }
}

fn players_variant_tag(p: &Players) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.get("_Players").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn player_variant_tag(p: &Player) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.get("_Player").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// Singular `Permanent` (one specific object reference) → `TargetFilter`.
/// Most variants are runtime context refs (Trigger_That*, RefOuter_TargetPermanent,
/// the_permanent_*_this_way) that bind to a specific resolution-time object.
/// We map the most common stable refs; the rest fail strict.
pub fn convert_permanent(p: &Permanent) -> ConvResult<TargetFilter> {
    Ok(match p {
        Permanent::ThisPermanent | Permanent::Self_It => TargetFilter::SelfRef,
        Permanent::HostPermanent => TargetFilter::SelfRef,
        Permanent::ThatEnteringPermanent => TargetFilter::TriggeringSource,
        // CR 603.7c: Trigger anaphora — "that permanent / that creature /
        // that other permanent / that vehicle / that sacrificed permanent /
        // that creature or planeswalker / the attacking creature / the
        // blocking creature" all bind to the source of the triggering event
        // (or its combat-role analogue), which the engine exposes as
        // `TargetFilter::TriggeringSource`. Combat-role variants
        // (TheAttackingCreature / TheBlockingCreature) are also surfaced as
        // the trigger source for combat-event triggers (CR 506).
        Permanent::Trigger_ThatCreature
        | Permanent::Trigger_ThatArtifact
        | Permanent::Trigger_ThatLand
        | Permanent::Trigger_ThatDeadPermanent
        | Permanent::Trigger_ThatOtherCreature
        | Permanent::Trigger_ThatPermanent
        | Permanent::Trigger_ThatOtherPermanent
        | Permanent::Trigger_ThatSacrificedPermanent
        | Permanent::Trigger_ThatVehicle
        | Permanent::Trigger_ThatCreatureOrPlaneswalker
        | Permanent::Trigger_TheAttackingCreature
        | Permanent::Trigger_TheBlockingCreature => TargetFilter::TriggeringSource,
        // CR 115.1 / CR 608.2b: Outer-target slot references collapse to the
        // generic target axis — typed constraints are enforced when targets
        // are chosen by the outer Targeted action.
        Permanent::Ref_TargetPermanent
        | Permanent::Ref_TargetPermanent1
        | Permanent::Ref_TargetPermanent2
        | Permanent::Ref_TargetPermanent3
        | Permanent::Ref_TargetPermanent4
        | Permanent::Ref_TargetPermanent5
        | Permanent::RefOuter_TargetPermanent
        | Permanent::Ref_TargetPermanentOfPlayersChoice
        | Permanent::AnyTargetAsAPermanent => TargetFilter::Any,
        // CR 111.1 + CR 608.2c: "the token created this way" — engine's
        // `LastCreated` resolves to the most recently produced token from
        // an Effect::Token in the current chain.
        Permanent::TheCreatedToken => TargetFilter::LastCreated,
        // CR 608.2c: Anaphoric references to the parent ability's chosen
        // permanent — "the chosen permanent" / "the first chosen permanent"
        // / "the second chosen permanent". The engine's `ParentTarget`
        // resolves to the parent ability's selected target(s) (which is how
        // "Choose a permanent. <do thing to it>" surfaces the chosen object
        // to downstream sub_abilities).
        Permanent::TheChosenPermanent
        | Permanent::TheFirstChosenPermanent
        | Permanent::TheSecondChosenPermanent => TargetFilter::ParentTarget,
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TargetFilter",
                needed_variant: format!("Permanent::{}", permanent_tag(other)),
            });
        }
    })
}

fn permanent_tag(p: &Permanent) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| {
            v.get("_Permanent")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn variant_tag(p: &Permanents) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| {
            v.get("_Permanents")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 601.2f / CR 117.7: Convert a `Spells` predicate to an engine
/// `TargetFilter`. Used by spell-cost-modifier statics
/// (DecreaseSpellCost / IncreaseSpellCost) where the static's
/// `spell_filter` selects which spells the cost modifier applies to.
///
/// Reuses `card_type` / `creature_type_name` / `supertype_name` so the
/// `Spells` and `Permanents` filter dispatchers stay aligned (same type
/// names land in the engine's `TypedFilter`). `Spells::AnySpell`
/// becomes `TargetFilter::Typed(TypedFilter::default())` (matches all
/// objects without further constraint). Unsupported shapes strict-fail
/// so the report tracks the work queue.
pub(crate) fn spells_to_filter(s: &crate::schema::types::Spells) -> ConvResult<TargetFilter> {
    use crate::schema::types::Spells as S;
    Ok(match s {
        S::AnySpell => TargetFilter::Typed(TypedFilter::default()),
        S::IsCardtype(ct) => TargetFilter::Typed(TypedFilter::new(card_type(ct))),
        S::IsNonCardtype(ct) => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Non(Box::new(card_type(ct)))))
        }
        S::IsCreatureType(ct) => TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature)
                .with_type(TypeFilter::Subtype(creature_type_name(ct))),
        ),
        S::IsNonCreatureType(ct) => TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).with_type(TypeFilter::Non(Box::new(
                TypeFilter::Subtype(creature_type_name(ct)),
            ))),
        ),
        S::IsNonSupertype(st) => TargetFilter::Typed(TypedFilter::default().with_type(
            TypeFilter::Non(Box::new(TypeFilter::Subtype(supertype_name(st)))),
        )),
        S::IsNonEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    enchantment_type_name(et),
                )))),
        ),

        // CR 205.3: Subtype predicates on spells (cards on the stack still
        // carry their printed subtypes, so the typed-filter shape mirrors the
        // Permanents arms).
        S::IsArtifactType(at) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Artifact)
                .with_type(TypeFilter::Subtype(artifact_type_name(at))),
        ),
        S::IsEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Subtype(enchantment_type_name(et))),
        ),
        S::IsPlaneswalkerType(pt) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Planeswalker)
                .with_type(TypeFilter::Subtype(planeswalker_type_name(pt))),
        ),
        // CR 205.4a: nonbasic / nonlegendary / etc.
        S::IsSupertype(st) => TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Subtype(supertype_name(st))),
        ),
        // CR 110.1: spells that resolve to permanents (artifact/creature/
        // enchantment/land/planeswalker/battle). Engine TypedFilter has no
        // boolean "permanent" axis on a card-on-stack scope, so we inherit
        // the Permanents-side `permanent()` typed filter — the type set is
        // the same.
        S::IsPermanent => TargetFilter::Typed(TypedFilter::permanent()),

        // CR 105.1 / CR 105.2c: Color predicates over spells. Non-concrete
        // colors strict-fail (chosen-color requires runtime binding).
        S::IsColor(c) => match concrete_color(c) {
            Some(color) => TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
            ),
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Spells/IsColor",
                    path: String::new(),
                    detail: format!("non-concrete color: {c:?}"),
                });
            }
        },
        S::IsNonColor(c) => match concrete_color(c) {
            Some(color) => TargetFilter::Not {
                filter: Box::new(TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
                )),
            },
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Spells/IsNonColor",
                    path: String::new(),
                    detail: format!("non-concrete color: {c:?}"),
                });
            }
        },
        S::IsColorless => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Colorless]))
        }
        S::IsMulticolored => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Multicolored]))
        }
        // CR 700.6: "historic" — legendary, artifact, or Saga.
        S::IsHistoric => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Historic]))
        }

        // CR 202.3 / CR 208.1: Numeric comparisons on the cast spell. Reuse
        // the same comparison helpers Permanents uses; they emit
        // FilterProp::CmcGE/PowerGE/ToughnessGE which evaluate against the
        // card's printed/current values regardless of zone.
        S::ManaValueIs(cmp) => cmc_comparison_filter(cmp)?,
        S::PowerIs(cmp) => power_comparison_filter(cmp)?,
        S::ToughnessIs(cmp) => toughness_comparison_filter(cmp)?,
        // CR 107.3 + CR 202.1: spell with {X} in its mana cost.
        S::HasXInManaCost => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]))
        }

        // CR 201.2: name predicate. Reuses the shared NameFilter helper,
        // which strict-fails for non-static name shapes.
        S::IsNamed(nf) => name_filter_to_filter(nf)?,

        // CR 702: Keyword presence / absence on a spell. Reuses the same
        // CheckHasable helper as Permanents — composite shapes strict-fail.
        S::HasAbility(check) => has_ability_filter(check)?,
        S::DoesntHaveAbility(check) => TargetFilter::Not {
            filter: Box::new(has_ability_filter(check)?),
        },

        // CR 903.3: "your commander" — controller-scoped commander predicate.
        S::IsYourCommander => TargetFilter::Typed(
            TypedFilter::default()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::IsCommander]),
        ),
        // CR 903.3: "a commander" — any commander on the stack.
        S::IsACommander => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::IsCommander]))
        }

        // CR 601.2 / CR 109.4: caster / owner / controller axis. The engine
        // tracks the stack-object controller via ControllerRef; "cast by
        // player X" maps directly. Compound `Players` (multi-player sets)
        // strict-fail through `players_to_controller`.
        S::CastByAPlayer(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::default().controller(ctrl))
        }
        S::CastByPlayer(player) => {
            let ctrl = player_to_controller(player)?;
            TargetFilter::Typed(TypedFilter::default().controller(ctrl))
        }
        S::ControlledByAPlayer(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::default().controller(ctrl))
        }
        S::OwnedByAPlayer(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::default().controller(ctrl))
        }

        // CR 115.9c: "spell that targets only ~" — the inner Permanent /
        // Permanents filter constrains every target of the spell.
        S::TargetsOnlySinglePermanent(inner) => TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::TargetsOnly {
                filter: Box::new(convert_permanent(inner)?),
            }]),
        ),
        S::CanTargetOnly(inner) => {
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::TargetsOnly {
                    filter: Box::new(convert(inner)?),
                }]),
            )
        }
        // CR 115.9b: "spell that targets ~" (ANY target satisfies the inner
        // filter). `TargetsAPermanent` carries a `Permanents`; `TargetsPermanent`
        // carries a singular `Permanent`. Both reduce to FilterProp::Targets.
        S::TargetsAPermanent(inner) => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Targets {
                filter: Box::new(convert(inner)?),
            }]))
        }
        S::TargetsPermanent(inner) => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Targets {
                filter: Box::new(convert_permanent(inner)?),
            }]))
        }
        // CR 115.7: spell with exactly one target.
        S::HasASingleTarget => TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::HasSingleTarget]),
        ),

        S::And(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(spells_to_filter(part)?);
            }
            TargetFilter::And { filters }
        }
        S::Or(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(spells_to_filter(part)?);
            }
            TargetFilter::Or { filters }
        }
        S::Not(inner) => TargetFilter::Not {
            filter: Box::new(spells_to_filter(inner)?),
        },

        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TargetFilter",
                needed_variant: format!("Spells::{}", spells_variant_tag(other)),
            });
        }
    })
}

fn spells_variant_tag(s: &crate::schema::types::Spells) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.get("_Spells").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 113.6 + CR 401.1: Convert a `CardsInGraveyard` predicate to an
/// engine `TargetFilter`. Used by `Rule::EachCardInGraveyardEffect`
/// where the static's `affected` filter selects which graveyard cards
/// the effect grants its modification to. The caller is responsible
/// for setting `affected_zone = Some(Zone::Graveyard)` on the
/// StaticDefinition; this helper produces the type/subtype/and/or
/// shape only.
///
/// Reuses `card_type` / `creature_type_name` / `supertype_name` so the
/// `CardsInGraveyard` and `Spells`/`Permanents` filter dispatchers
/// stay aligned.
pub(crate) fn cards_in_graveyard_to_filter(
    c: &crate::schema::types::CardsInGraveyard,
) -> ConvResult<TargetFilter> {
    use crate::schema::types::CardsInGraveyard as C;
    Ok(match c {
        C::AnyCardInAnyGraveyard => TargetFilter::Typed(TypedFilter::default()),
        // CR 115.1 / CR 608.2b: Resolution-time references to outer
        // graveyard-card target slots. The outer targeted wrapper provides
        // the graveyard-card legality; inside the action body these refs are
        // anaphoric selectors, so the engine target axis can stay unconstrained.
        C::Ref_TargetGraveyardCards
        | C::Ref_TargetGraveyardCards1
        | C::Ref_TargetGraveyardCards2 => TargetFilter::Any,
        C::IsCardtype(ct) => TargetFilter::Typed(TypedFilter::new(card_type(ct))),
        C::IsNonCardtype(ct) => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Non(Box::new(card_type(ct)))))
        }
        // Subtype/supertype/permanence/color/keyword arms mirror the Cards
        // and Permanents dispatchers — graveyard cards retain their printed
        // properties (CR 401.1 + CR 205) so the typed-filter shape composes
        // identically.
        C::IsCreatureType(s) => TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature)
                .with_type(TypeFilter::Subtype(creature_type_name(s))),
        ),
        C::IsSupertype(s) => TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Subtype(supertype_name(s))),
        ),
        C::IsLandType(lt) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Land)
                .with_type(TypeFilter::Subtype(land_type_name(lt))),
        ),
        C::IsArtifactType(at) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Artifact)
                .with_type(TypeFilter::Subtype(artifact_type_name(at))),
        ),
        C::IsEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Subtype(enchantment_type_name(et))),
        ),
        C::IsPlaneswalkerType(pt) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Planeswalker)
                .with_type(TypeFilter::Subtype(planeswalker_type_name(pt))),
        ),
        C::IsPermanent => TargetFilter::Typed(TypedFilter::permanent()),
        C::IsNonCreatureType(s) => TargetFilter::Typed(TypedFilter::creature().with_type(
            TypeFilter::Non(Box::new(TypeFilter::Subtype(creature_type_name(s)))),
        )),
        C::IsNonEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    enchantment_type_name(et),
                )))),
        ),
        C::IsNonSupertype(s) => {
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::NotSupertype {
                    value: supertype_to_engine(s),
                }]),
            )
        }
        // CR 105: color predicates on graveyard cards.
        C::IsColor(c) => match concrete_color(c) {
            Some(color) => TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
            ),
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "CardsInGraveyard/IsColor",
                    path: String::new(),
                    detail: format!("non-concrete color: {c:?}"),
                });
            }
        },
        C::IsColorless => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Colorless]))
        }
        C::IsMulticolored => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Multicolored]))
        }
        C::IsHistoric => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Historic]))
        }
        // CR 700.4 / CR 202.3 / CR 208.1: numeric comparisons.
        C::ManaValueIs(cmp) => cmc_comparison_filter(cmp)?,
        C::PowerIs(cmp) => power_comparison_filter(cmp)?,
        C::ToughnessIs(cmp) => toughness_comparison_filter(cmp)?,
        // CR 201.2.
        C::IsNamed(nf) => name_filter_to_filter(nf)?,
        C::IsNotNamed(nf) => TargetFilter::Not {
            filter: Box::new(name_filter_to_filter(nf)?),
        },
        // CR 702 keyword presence.
        C::HasAbility(check) => has_ability_filter(check)?,
        C::DoesntHaveAbility(check) => TargetFilter::Not {
            filter: Box::new(has_ability_filter(check)?),
        },
        // CR 700.2 / CR 201.4: chosen creature type binding.
        C::IsCreatureTypeVariable(CreatureTypeVariable::TheChosenCreatureType) => {
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::IsChosenCreatureType]),
            )
        }
        // CR 109.4: graveyard scope for owner — `affected_zone` is set by the
        // caller; `controller` axis on TypedFilter still drives the per-player
        // filter.
        C::InAPlayersGraveyard(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::default().controller(ctrl))
        }
        C::And(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(cards_in_graveyard_to_filter(part)?);
            }
            TargetFilter::And { filters }
        }
        C::Or(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(cards_in_graveyard_to_filter(part)?);
            }
            TargetFilter::Or { filters }
        }
        C::Not(inner) => TargetFilter::Not {
            filter: Box::new(cards_in_graveyard_to_filter(inner)?),
        },

        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TargetFilter",
                needed_variant: format!(
                    "CardsInGraveyard::{}",
                    cards_in_graveyard_variant_tag(other)
                ),
            });
        }
    })
}

fn cards_in_graveyard_variant_tag(c: &crate::schema::types::CardsInGraveyard) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| {
            v.get("_CardsInGraveyard")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// CR 113.6 + CR 401.1: Convert a `Cards` predicate to an engine
/// `TargetFilter`. Used by `Rule::EachCardInPlayersHandEffect` and any
/// other zone-scoped static where the affected filter is a generic
/// `Cards` (not the graveyard-specific `CardsInGraveyard`).
///
/// Mirrors `cards_in_graveyard_to_filter` shape for the variants that
/// `Cards` and `CardsInGraveyard` share.
pub(crate) fn cards_to_filter(c: &crate::schema::types::Cards) -> ConvResult<TargetFilter> {
    use crate::schema::types::Cards as C;
    Ok(match c {
        C::AnyCard => TargetFilter::Typed(TypedFilter::default()),
        C::IsCardtype(ct) => TargetFilter::Typed(TypedFilter::new(card_type(ct))),
        C::IsNonCardtype(ct) => {
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Non(Box::new(card_type(ct)))))
        }
        C::IsCreatureType(ct) => TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature)
                .with_type(TypeFilter::Subtype(creature_type_name(ct))),
        ),
        C::IsSupertype(s) => TargetFilter::Typed(
            TypedFilter::permanent().with_type(TypeFilter::Subtype(supertype_name(s))),
        ),
        C::IsPermanent => TargetFilter::Typed(TypedFilter::permanent()),

        // CR 205.3i / CR 205.3g / CR 205.3h / CR 205.3j: Subtype filters that
        // mirror the Permanents arms — Cards in non-battlefield zones still
        // carry their printed subtypes, so the type+subtype shape is the
        // same TypedFilter composition.
        C::IsLandType(lt) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Land)
                .with_type(TypeFilter::Subtype(land_type_name(lt))),
        ),
        C::IsArtifactType(at) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Artifact)
                .with_type(TypeFilter::Subtype(artifact_type_name(at))),
        ),
        C::IsEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Subtype(enchantment_type_name(et))),
        ),
        C::IsPlaneswalkerType(pt) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Planeswalker)
                .with_type(TypeFilter::Subtype(planeswalker_type_name(pt))),
        ),

        // CR 205.4b: Type-negation predicates within the same type axis.
        C::IsNonCreatureType(s) => TargetFilter::Typed(TypedFilter::creature().with_type(
            TypeFilter::Non(Box::new(TypeFilter::Subtype(creature_type_name(s)))),
        )),
        C::IsNonArtifactType(at) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Artifact)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    artifact_type_name(at),
                )))),
        ),
        C::IsNonEnchantmentType(et) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Enchantment)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    enchantment_type_name(et),
                )))),
        ),
        C::IsNonLandType(lt) => TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Land)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                    land_type_name(lt),
                )))),
        ),
        // CR 205.4a: nonbasic / nonlegendary / etc.
        C::IsNonSupertype(s) => {
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::NotSupertype {
                    value: supertype_to_engine(s),
                }]),
            )
        }

        // CR 105.1 / CR 105.2c: Color filters. Non-concrete colors strict-fail
        // because they require a runtime "chosen color" binding the converter
        // cannot resolve at static-conversion time.
        C::IsColor(c) => match concrete_color(c) {
            Some(color) => TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
            ),
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Cards/IsColor",
                    path: String::new(),
                    detail: format!("non-concrete color: {c:?}"),
                });
            }
        },
        C::IsNonColor(c) => match concrete_color(c) {
            Some(color) => TargetFilter::Not {
                filter: Box::new(TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
                )),
            },
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Cards/IsNonColor",
                    path: String::new(),
                    detail: format!("non-concrete color: {c:?}"),
                });
            }
        },
        C::IsColorless => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Colorless]))
        }
        C::IsMulticolored => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Multicolored]))
        }

        // CR 700.6: "historic" — legendary, artifact, or Saga.
        C::IsHistoric => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Historic]))
        }

        // CR 202.3 / CR 208.1: Numeric comparisons. Reuse the same comparison
        // helpers Permanents uses; the resulting TargetFilter shape is
        // zone-agnostic (FilterProp::CmcGE / PowerGE / ToughnessGE evaluate
        // against the card's printed/current values).
        C::ManaValueIs(cmp) => cmc_comparison_filter(cmp)?,
        C::PowerIs(cmp) => power_comparison_filter(cmp)?,
        C::ToughnessIs(cmp) => toughness_comparison_filter(cmp)?,

        // CR 201.2: Static name match / mismatch.
        C::IsNamed(nf) => name_filter_to_filter(nf)?,
        C::IsNotNamed(nf) => TargetFilter::Not {
            filter: Box::new(name_filter_to_filter(nf)?),
        },

        // CR 702: Keyword presence / absence (vanilla and parameterized
        // keyword kinds). Composite shapes strict-fail via has_ability_filter.
        C::HasAbility(check) => has_ability_filter(check)?,
        C::DoesntHaveAbility(check) => TargetFilter::Not {
            filter: Box::new(has_ability_filter(check)?),
        },

        // CR 903.3: "your commander" — controller-scoped commander predicate.
        C::IsYourCommander => TargetFilter::Typed(
            TypedFilter::default()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::IsCommander]),
        ),

        // CR 109.4: Owner / controller axis. Reuse `players_to_controller` so
        // mtgish's Players enum maps consistently across Permanents and Cards.
        C::OwnedByAPlayer(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::default().controller(ctrl))
        }
        C::ControlledByAPlayer(players) => {
            let ctrl = players_to_controller(players)?;
            TargetFilter::Typed(TypedFilter::default().controller(ctrl))
        }

        C::And(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(cards_to_filter(part)?);
            }
            TargetFilter::And { filters }
        }
        C::Or(parts) => {
            let mut filters = Vec::with_capacity(parts.len());
            for part in parts {
                filters.push(cards_to_filter(part)?);
            }
            TargetFilter::Or { filters }
        }
        C::Not(inner) => TargetFilter::Not {
            filter: Box::new(cards_to_filter(inner)?),
        },

        // CR 700.2 / CR 201.4: chosen-type bindings. Mirrors the Permanents
        // dispatcher — the engine's `IsChosenCreatureType` / `IsChosenCardType`
        // resolve against the source's `ChosenAttribute` slot regardless of
        // the card's current zone.
        C::IsCreatureTypeVariable(CreatureTypeVariable::TheChosenCreatureType) => {
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::IsChosenCreatureType]),
            )
        }
        C::IsCardtypeVariable(CardtypeVariable::TheChosenCardtype) => TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::IsChosenCardType]),
        ),
        // CR 107.3 + CR 202.1: spell/card with {X} in its mana cost.
        C::HasXInManaCost => {
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]))
        }

        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TargetFilter",
                needed_variant: format!("Cards::{}", cards_variant_tag(other)),
            });
        }
    })
}

pub(crate) fn cards_variant_tag(c: &crate::schema::types::Cards) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.get("_Cards").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

// ---------------------------------------------------------------------------
// Helpers for status / counter / P-T / keyword / name predicates
// ---------------------------------------------------------------------------

fn supertype_to_engine(s: &SuperType) -> engine::types::card_type::Supertype {
    use engine::types::card_type::Supertype as E;
    match s {
        SuperType::Basic => E::Basic,
        SuperType::Legendary => E::Legendary,
        SuperType::Ongoing => E::Ongoing,
        SuperType::Snow => E::Snow,
        SuperType::World => E::World,
    }
}

pub(crate) fn planeswalker_type_name(pt: &PlaneswalkerType) -> String {
    format!("{pt:?}")
}

/// CR 122: Map a schema `CounterType` to the engine's typed `CounterType`.
/// PT counters and the standard parameterless kinds map to dedicated variants;
/// every other counter falls into `Generic(String)` keyed by the schema name
/// minus the `Counter` suffix (matching `action::counter_type_name`).
pub(crate) fn counter_type_to_engine(
    ct: &CounterType,
) -> ConvResult<engine::types::counter::CounterType> {
    use engine::types::counter::CounterType as E;
    use CounterType as S;
    Ok(match ct {
        S::PTCounter(1, 1) => E::Plus1Plus1,
        S::PTCounter(-1, -1) => E::Minus1Minus1,
        // Other PT shapes (e.g. +0/+1) are rare; fall through to generic.
        S::PTCounter(p, t) => E::Generic(format!("{p:+}/{t:+}")),

        S::LoyaltyCounter => E::Loyalty,
        S::DefenseCounter => E::Defense,
        S::LoreCounter => E::Lore,
        // Schema's keyword counters (DeathtouchCounter, FlyingCounter, …) →
        // engine's typed `Keyword(KeywordKind)`.
        S::DeathtouchCounter => E::Keyword(KeywordKind::Deathtouch),
        S::DoubleStrikeCounter => E::Keyword(KeywordKind::DoubleStrike),
        S::ExaltedCounter => E::Keyword(KeywordKind::Exalted),
        S::FirstStrikeCounter => E::Keyword(KeywordKind::FirstStrike),
        S::FlyingCounter => E::Keyword(KeywordKind::Flying),
        S::HasteCounter => E::Keyword(KeywordKind::Haste),
        S::HexproofCounter => E::Keyword(KeywordKind::Hexproof),
        S::IndestructibleCounter => E::Keyword(KeywordKind::Indestructible),
        S::LifelinkCounter => E::Keyword(KeywordKind::Lifelink),
        S::MenaceCounter => E::Keyword(KeywordKind::Menace),
        S::ReachCounter => E::Keyword(KeywordKind::Reach),
        S::ShadowCounter => E::Keyword(KeywordKind::Shadow),
        S::TrampleCounter => E::Keyword(KeywordKind::Trample),
        S::VigilanceCounter => E::Keyword(KeywordKind::Vigilance),
        S::DecayedCounter => E::Keyword(KeywordKind::Decayed),

        // Generic counters: strip `Counter` suffix to match engine string form.
        other => {
            let name = format!("{other:?}");
            let stripped = name.strip_suffix("Counter").unwrap_or(&name).to_string();
            E::Generic(stripped)
        }
    })
}

/// Schema `Comparison` over a counter count → a `TargetFilter` battlefield-
/// scoped on a typed permanent with the corresponding `CountersGE`/`Not`
/// composition. CR 700.4: comparison semantics; CR 122.1: counter count.
fn counter_comparison_filter(
    counter_type: engine::types::counter::CounterType,
    cmp: &Comparison,
) -> ConvResult<TargetFilter> {
    let ge = |value: QuantityExpr| {
        TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::CountersGE {
                counter_type: counter_type.clone(),
                count: value,
            }]),
        )
    };
    Ok(match cmp {
        Comparison::GreaterThanOrEqualTo(g) => ge(convert_quantity(g)?),
        Comparison::GreaterThan(g) => ge(offset(convert_quantity(g)?, 1)),
        Comparison::EqualTo(g) => {
            let n = convert_quantity(g)?;
            // count == N ⇔ count >= N AND NOT count >= N+1
            TargetFilter::And {
                filters: vec![
                    ge(n.clone()),
                    TargetFilter::Not {
                        filter: Box::new(ge(offset(n, 1))),
                    },
                ],
            }
        }
        Comparison::LessThanOrEqualTo(g) => TargetFilter::Not {
            filter: Box::new(ge(offset(convert_quantity(g)?, 1))),
        },
        Comparison::LessThan(g) => TargetFilter::Not {
            filter: Box::new(ge(convert_quantity(g)?)),
        },
        Comparison::NotEqualTo(g) => {
            let n = convert_quantity(g)?;
            TargetFilter::Not {
                filter: Box::new(TargetFilter::And {
                    filters: vec![
                        ge(n.clone()),
                        TargetFilter::Not {
                            filter: Box::new(ge(offset(n, 1))),
                        },
                    ],
                }),
            }
        }
        other => return Err(unsupported_comparison("counter", other)),
    })
}

/// CR 208.1: Power comparison over a typed creature filter.
fn power_comparison_filter(cmp: &Comparison) -> ConvResult<TargetFilter> {
    Ok(match cmp {
        Comparison::GreaterThanOrEqualTo(g) => prop_filter_creature(FilterProp::PowerGE {
            value: convert_quantity(g)?,
        }),
        Comparison::GreaterThan(g) => prop_filter_creature(FilterProp::PowerGE {
            value: offset(convert_quantity(g)?, 1),
        }),
        Comparison::LessThanOrEqualTo(g) => prop_filter_creature(FilterProp::PowerLE {
            value: convert_quantity(g)?,
        }),
        Comparison::LessThan(g) => prop_filter_creature(FilterProp::PowerLE {
            value: offset(convert_quantity(g)?, -1),
        }),
        Comparison::EqualTo(g) => {
            let n = convert_quantity(g)?;
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PowerGE { value: n.clone() },
                FilterProp::PowerLE { value: n },
            ]))
        }
        other => return Err(unsupported_comparison("power", other)),
    })
}

/// CR 208.1: Toughness comparison over a typed creature filter.
fn toughness_comparison_filter(cmp: &Comparison) -> ConvResult<TargetFilter> {
    Ok(match cmp {
        Comparison::GreaterThanOrEqualTo(g) => prop_filter_creature(FilterProp::ToughnessGE {
            value: convert_quantity(g)?,
        }),
        Comparison::GreaterThan(g) => prop_filter_creature(FilterProp::ToughnessGE {
            value: offset(convert_quantity(g)?, 1),
        }),
        Comparison::LessThanOrEqualTo(g) => prop_filter_creature(FilterProp::ToughnessLE {
            value: convert_quantity(g)?,
        }),
        Comparison::LessThan(g) => prop_filter_creature(FilterProp::ToughnessLE {
            value: offset(convert_quantity(g)?, -1),
        }),
        Comparison::EqualTo(g) => {
            let n = convert_quantity(g)?;
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::ToughnessGE { value: n.clone() },
                FilterProp::ToughnessLE { value: n },
            ]))
        }
        other => return Err(unsupported_comparison("toughness", other)),
    })
}

/// CR 202.3: Mana value comparison over a typed permanent filter.
fn cmc_comparison_filter(cmp: &Comparison) -> ConvResult<TargetFilter> {
    // Π-7: FilterProp::Cmc { comparator, value } unifies the per-comparator
    // siblings; emit the engine's Comparator directly so GT/LT no longer
    // need ±1-offset workarounds against GE/LE.
    Ok(match cmp {
        Comparison::GreaterThanOrEqualTo(g) => prop_filter(FilterProp::Cmc {
            comparator: Comparator::GE,
            value: convert_quantity(g)?,
        }),
        Comparison::GreaterThan(g) => prop_filter(FilterProp::Cmc {
            comparator: Comparator::GT,
            value: convert_quantity(g)?,
        }),
        Comparison::LessThanOrEqualTo(g) => prop_filter(FilterProp::Cmc {
            comparator: Comparator::LE,
            value: convert_quantity(g)?,
        }),
        Comparison::LessThan(g) => prop_filter(FilterProp::Cmc {
            comparator: Comparator::LT,
            value: convert_quantity(g)?,
        }),
        Comparison::EqualTo(g) => prop_filter(FilterProp::Cmc {
            comparator: Comparator::EQ,
            value: convert_quantity(g)?,
        }),
        other => return Err(unsupported_comparison("mana_value", other)),
    })
}

fn prop_filter_creature(prop: FilterProp) -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().properties(vec![prop]))
}

/// Apply a constant integer offset to a `QuantityExpr`. Integer literals fold
/// in place; everything else wraps in `QuantityExpr::Offset`.
fn offset(q: QuantityExpr, n: i32) -> QuantityExpr {
    match q {
        QuantityExpr::Fixed { value } => QuantityExpr::Fixed {
            value: value.saturating_add(n),
        },
        QuantityExpr::Offset {
            inner,
            offset: existing,
        } => QuantityExpr::Offset {
            inner,
            offset: existing.saturating_add(n),
        },
        other => QuantityExpr::Offset {
            inner: Box::new(other),
            offset: n,
        },
    }
}

fn unsupported_comparison(axis: &'static str, cmp: &Comparison) -> ConversionGap {
    ConversionGap::MalformedIdiom {
        idiom: "Permanents/comparison",
        path: String::new(),
        detail: format!("unsupported {axis} comparison: {cmp:?}"),
    }
}

/// CR 702: Convert a `CheckHasable` describing a vanilla or parameterized
/// keyword presence query into a `WithKeyword` / `HasKeywordKind` filter.
/// Composite shapes (And, ReplaceWouldDealDamage, ThisAbility, …) are not
/// keyword-presence queries and strict-fail.
fn has_ability_filter(check: &CheckHasable) -> ConvResult<TargetFilter> {
    use CheckHasable as C;
    let prop = match check {
        // Parameterless keywords map to a typed `WithKeyword(Keyword::*)`.
        C::Banding => FilterProp::WithKeyword {
            value: Keyword::Banding,
        },
        C::Cascade => FilterProp::WithKeyword {
            value: Keyword::Cascade,
        },
        C::Convoke => FilterProp::WithKeyword {
            value: Keyword::Convoke,
        },
        C::Deathtouch => FilterProp::WithKeyword {
            value: Keyword::Deathtouch,
        },
        C::Decayed => FilterProp::WithKeyword {
            value: Keyword::Decayed,
        },
        C::Defender => FilterProp::WithKeyword {
            value: Keyword::Defender,
        },
        C::Devoid => FilterProp::WithKeyword {
            value: Keyword::Devoid,
        },
        C::DoubleStrike => FilterProp::WithKeyword {
            value: Keyword::DoubleStrike,
        },
        C::Fear => FilterProp::WithKeyword {
            value: Keyword::Fear,
        },
        C::FirstStrike => FilterProp::WithKeyword {
            value: Keyword::FirstStrike,
        },
        C::Flanking => FilterProp::WithKeyword {
            value: Keyword::Flanking,
        },
        C::Flash => FilterProp::WithKeyword {
            value: Keyword::Flash,
        },
        C::Flying => FilterProp::WithKeyword {
            value: Keyword::Flying,
        },
        C::Haste => FilterProp::WithKeyword {
            value: Keyword::Haste,
        },
        C::Horsemanship => FilterProp::WithKeyword {
            value: Keyword::Horsemanship,
        },
        C::Indestructible => FilterProp::WithKeyword {
            value: Keyword::Indestructible,
        },
        C::Infect => FilterProp::WithKeyword {
            value: Keyword::Infect,
        },
        C::Lifelink => FilterProp::WithKeyword {
            value: Keyword::Lifelink,
        },
        C::Menace => FilterProp::WithKeyword {
            value: Keyword::Menace,
        },
        C::Phasing => FilterProp::WithKeyword {
            value: Keyword::Phasing,
        },
        C::Reach => FilterProp::WithKeyword {
            value: Keyword::Reach,
        },
        C::Shadow => FilterProp::WithKeyword {
            value: Keyword::Shadow,
        },
        C::Shroud => FilterProp::WithKeyword {
            value: Keyword::Shroud,
        },
        C::Skulk => FilterProp::WithKeyword {
            value: Keyword::Skulk,
        },
        C::Soulbond => FilterProp::WithKeyword {
            value: Keyword::Soulbond,
        },
        C::Trample => FilterProp::WithKeyword {
            value: Keyword::Trample,
        },
        C::Vigilance => FilterProp::WithKeyword {
            value: Keyword::Vigilance,
        },

        // Parameterized keywords without specific args: match by kind so the
        // value of N / cost / color is irrelevant ("creature with hexproof").
        C::AnyHexproof => FilterProp::HasKeywordKind {
            value: KeywordKind::Hexproof,
        },
        C::AnyKicker => FilterProp::HasKeywordKind {
            value: KeywordKind::Kicker,
        },
        C::AnyLandwalk => FilterProp::HasKeywordKind {
            value: KeywordKind::Landwalk,
        },
        C::AnyProtection | C::AnyProtectionFromColor => FilterProp::HasKeywordKind {
            value: KeywordKind::Protection,
        },
        C::AnyWard => FilterProp::HasKeywordKind {
            value: KeywordKind::Ward,
        },
        C::AnyCycling => FilterProp::HasKeywordKind {
            value: KeywordKind::Cycling,
        },
        C::AnyFlashback => FilterProp::HasKeywordKind {
            value: KeywordKind::Flashback,
        },
        C::AnySuspend => FilterProp::HasKeywordKind {
            value: KeywordKind::Suspend,
        },
        C::AnyMorph => FilterProp::HasKeywordKind {
            value: KeywordKind::Morph,
        },
        C::AnyForetell => FilterProp::HasKeywordKind {
            value: KeywordKind::Foretell,
        },
        C::AnyEmbalm => FilterProp::HasKeywordKind {
            value: KeywordKind::Embalm,
        },
        C::AnyEternalize => FilterProp::HasKeywordKind {
            value: KeywordKind::Eternalize,
        },
        C::AnyDisturb => FilterProp::HasKeywordKind {
            value: KeywordKind::Disturb,
        },
        C::AnyMutate => FilterProp::HasKeywordKind {
            value: KeywordKind::Mutate,
        },
        C::AnyAwaken => FilterProp::HasKeywordKind {
            value: KeywordKind::Awaken,
        },
        C::AnyBlitz => FilterProp::HasKeywordKind {
            value: KeywordKind::Blitz,
        },
        C::AnyFading => FilterProp::HasKeywordKind {
            value: KeywordKind::Fading,
        },
        C::AnyVanishing => FilterProp::HasKeywordKind {
            value: KeywordKind::Vanishing,
        },
        C::AnyUnearth => FilterProp::HasKeywordKind {
            value: KeywordKind::Unearth,
        },
        C::AnyFreerunning => FilterProp::HasKeywordKind {
            value: KeywordKind::Freerunning,
        },
        C::AnyRampage => FilterProp::HasKeywordKind {
            value: KeywordKind::Rampage,
        },
        C::AnyPartner => FilterProp::HasKeywordKind {
            value: KeywordKind::Partner,
        },
        C::AnyModular => FilterProp::HasKeywordKind {
            value: KeywordKind::Modular,
        },
        C::AnyWarp => FilterProp::HasKeywordKind {
            value: KeywordKind::Warp,
        },

        // Color-parameterized: "protection from <color>" — model as a typed
        // Protection prop. Engine uses Keyword::Protection(...) / HexproofFrom,
        // but exact-color match in a filter requires the parsed color slot.
        C::ProtectionFromColor(c) => match concrete_color(c) {
            Some(_color) => FilterProp::HasKeywordKind {
                value: KeywordKind::Protection,
            },
            None => return Err(unsupported_check_hasable(check)),
        },

        // Composite / structural shapes — not vanilla keyword presence queries.
        _ => return Err(unsupported_check_hasable(check)),
    };
    Ok(prop_filter(prop))
}

fn unsupported_check_hasable(check: &CheckHasable) -> ConversionGap {
    ConversionGap::MalformedIdiom {
        idiom: "Permanents/HasAbility",
        path: String::new(),
        detail: format!("unsupported CheckHasable: {check:?}"),
    }
}

/// CR 201.2: `IsNamed` predicate. Only static name matches translate; chosen-
/// name / draft-noted / sacrificed-creature-name shapes need runtime context.
fn name_filter_to_filter(nf: &NameFilter) -> ConvResult<TargetFilter> {
    Ok(match nf {
        NameFilter::NamedCard(name) => prop_filter(FilterProp::Named { name: name.clone() }),
        // CR 201.4: cards/spells matching the name chosen by an earlier effect
        // (Anointed Peacekeeper, Pithing Needle, Meddling Mage class). Engine
        // models this directly on TargetFilter (not as a FilterProp); the
        // resolver reads `ChosenAttribute::CardName` from the source.
        NameFilter::TheChosenName => TargetFilter::HasChosenName,
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Permanents/IsNamed",
                path: String::new(),
                detail: format!("unsupported NameFilter: {other:?}"),
            });
        }
    })
}

// `GameNumber` import retained in case we extend the comparison helpers.
#[allow(dead_code)]
fn _gamenumber_marker(_g: &GameNumber) {}
