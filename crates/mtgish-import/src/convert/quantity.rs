//! mtgish `GameNumber` → engine `QuantityExpr` (narrow Phase 6 slice).
//!
//! Covers the simplest forms — literal integers, X, and `Plus`/`Minus`/`Multiply`
//! arithmetic over the same. The vast long tail of game-state-derived quantities
//! (TheNumberOfCardsInYourHand, TheGreatestPowerAmongPermanents, etc.) requires
//! per-variant mapping into engine `QuantityRef` and lands in later phases.

use engine::types::ability::{
    AggregateFunction, CountScope, FilterProp, ObjectProperty, PlayerFilter, PlayerScope,
    QuantityExpr, QuantityRef, RoundingMode, TargetFilter, TypedFilter, ZoneRef,
};
use engine::types::player::PlayerCounterKind;
use engine::types::zones::Zone;

use crate::convert::filter::{
    cards_in_graveyard_to_filter, concrete_color, convert as convert_permanents, convert_permanent,
    spells_to_filter,
};
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{
    CardInExile, CardType, CardsInExile, CounterType, GameNumber, Permanent, Permanents, Player,
    Players, Spell,
};

pub fn convert(g: &GameNumber) -> ConvResult<QuantityExpr> {
    Ok(match g {
        GameNumber::Integer(n) => QuantityExpr::Fixed { value: *n },
        // CR 107.3b + CR 601.2f: X in spells/abilities resolves to its declared value
        // (or value paid at cast). Emitting Fixed { 0 } silently corrupts every X-cost
        // and X-quantity effect — the engine resolves Variable { "X" } from spell
        // payment context.
        GameNumber::ValueX | GameNumber::X_From_Casting => QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        },
        // CR 107.1: Arithmetic addition. Integer-anchored adds collapse into
        // QuantityExpr::Offset; two-dynamic adds use QuantityExpr::Sum which
        // is the engine's general "for each X and each Y" combinator
        // (ability.rs ~1700, see "Alrund's +1/+1 for each card in your hand
        // and each foretold card you own in exile").
        GameNumber::Plus(a, b) => match (&**a, &**b) {
            (GameNumber::Integer(x), inner) | (inner, GameNumber::Integer(x)) => {
                let inner_expr = convert(inner)?;
                QuantityExpr::Offset {
                    inner: Box::new(inner_expr),
                    offset: *x,
                }
            }
            (lhs, rhs) => QuantityExpr::Sum {
                exprs: vec![convert(lhs)?, convert(rhs)?],
            },
        },
        GameNumber::Minus(a, b) => match (&**a, &**b) {
            (inner, GameNumber::Integer(x)) => {
                let inner_expr = convert(inner)?;
                QuantityExpr::Offset {
                    inner: Box::new(inner_expr),
                    offset: -*x,
                }
            }
            // CR 107.1c: Dynamic-vs-dynamic subtraction. Engine has no
            // Subtract primitive but composes cleanly: `a - b` = `a +
            // (-1 * b)` via Sum of Multiply{factor:-1}. The Multiply
            // arm above accepts (Integer, dyn) ordering, so we
            // synthesize the negated rhs then sum.
            (lhs, rhs) => {
                let lhs_expr = convert(lhs)?;
                let rhs_expr = convert(rhs)?;
                let neg_rhs = QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(rhs_expr),
                };
                QuantityExpr::Sum {
                    exprs: vec![lhs_expr, neg_rhs],
                }
            }
        },
        GameNumber::Multiply(a, b) => match (&**a, &**b) {
            (GameNumber::Integer(factor), inner) | (inner, GameNumber::Integer(factor)) => {
                let inner_expr = convert(inner)?;
                QuantityExpr::Multiply {
                    factor: *factor,
                    inner: Box::new(inner_expr),
                }
            }
            _ => return Err(unsupported(g)),
        },

        // CR 107.3 + CR 401.1: "the number of permanents [filter] on the
        // battlefield" → QuantityRef::ObjectCount with the converted filter.
        GameNumber::TheNumberOfPermanentsOnTheBattlefield(filter) => QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: convert_permanents(filter)?,
            },
        },

        // CR 107.3 + CR 404.1: "the number of cards in [scope]'s graveyard"
        // → GraveyardSize when scope is the controller; else strict-fail
        // until the engine adds a target-player graveyard ref.
        GameNumber::TheNumberOfGraveyardCards(filter) => QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: cards_in_graveyard_to_filter(filter)?,
            },
        },

        // CR 601.2h + CR 202.2: Sunburst / Converge — "the number of colors
        // of mana spent to cast [spell]". Engine slot
        // `QuantityRef::ColorsSpentOnSelf` resolves against the source
        // object's `colors_spent_to_cast` tally; only the self-spell-ref
        // (`ThisSpell`) shape lowers cleanly. Other spell refs (target spell,
        // resolved-spell, etc.) need a parameterized
        // `QuantityRef::ColorsSpentToCast { spell: SpellRef }` engine
        // extension and strict-fail today.
        GameNumber::TheNumberOfColorsOfManaSpentToCastSpell(spell) => match &**spell {
            Spell::ThisSpell => QuantityExpr::Ref {
                qty: QuantityRef::ColorsSpentOnSelf,
            },
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "GameNumber/TheNumberOfColorsOfManaSpentToCastSpell",
                    path: String::new(),
                    detail: format!("non-self spell ref: {other:?}"),
                });
            }
        },

        // CR 702.151a-b: Party — "the number of creatures in your party" =
        // count of distinct subtypes among {Cleric, Rogue, Warrior, Wizard}
        // for creatures controlled by `player`, capped at 4. Engine-side
        // resolver lives in `game/quantity.rs` and walks post-layer subtypes
        // (CR 613.1d) so Changeling (CR 702.73a) participates correctly.
        GameNumber::NumCreaturesInPlayersParty(player) => match &**player {
            Player::You => QuantityExpr::Ref {
                qty: QuantityRef::PartySize {
                    player: PlayerScope::Controller,
                },
            },
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "GameNumber/NumCreaturesInPlayersParty",
                    path: String::new(),
                    detail: format!("non-You player: {other:?}"),
                });
            }
        },

        // CR 107.3 + CR 402.1: "the number of cards in [player]'s hand".
        GameNumber::TheNumberOfCardsInPlayersHand(player) => match &**player {
            Player::You => QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::Controller,
                },
            },
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "GameNumber/TheNumberOfCardsInPlayersHand",
                    path: String::new(),
                    detail: format!("non-You player: {other:?}"),
                });
            }
        },

        // CR 107.3 + CR 119.1: "[player]'s life total". You → controller-relative
        // LifeTotal; the controller's opponents (when Player::Trigger_ThatPlayer
        // / iteration variables resolve to "an opponent") share OpponentLifeTotal
        // (max across opponents); a target-player ref resolves to
        // TargetLifeTotal.
        GameNumber::LifeTotalOfPlayer(player) => match &**player {
            Player::You => QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
            },
            Player::Ref_TargetPlayer
            | Player::Ref_TargetPlayer1
            | Player::Ref_TargetPlayer2
            | Player::Ref_TargetPlayer3 => QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Target,
                },
            },
            other => return Err(player_gap("LifeTotalOfPlayer", other)),
        },

        // CR 107.1c: Twice the inner quantity — a Multiply by 2.
        GameNumber::Twice(inner) => QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(convert(inner)?),
        },

        // CR 107.1c: Thrice / "three times the number of N" — Multiply by 3.
        GameNumber::Thrice(inner) => QuantityExpr::Multiply {
            factor: 3,
            inner: Box::new(convert(inner)?),
        },

        // CR 107.1a: "Half N, rounded up/down" wraps an inner expression in a
        // rounding-aware divide-by-two. Mirrors the parser's HalfRounded path
        // (oracle_quantity.rs).
        GameNumber::HalfRoundedDown(inner) => QuantityExpr::HalfRounded {
            inner: Box::new(convert(inner)?),
            rounding: RoundingMode::Down,
        },
        GameNumber::HalfRoundedUp(inner) => QuantityExpr::HalfRounded {
            inner: Box::new(convert(inner)?),
            rounding: RoundingMode::Up,
        },

        // CR 107.3 + CR 208.1: "[permanent]'s power" — `~` / `it` / `this
        // [type]` resolve to the source object → SelfPower; `target ...`
        // / `Ref_TargetPermanent*` resolve to the targeted object → TargetPower;
        // `that creature` / `that permanent` / `that other...` (trigger
        // referents) resolve through the trigger-event source →
        // EventContextSourcePower (mirrors oracle_quantity.rs).
        GameNumber::PowerOfPermanent(perm) => QuantityExpr::Ref {
            qty: power_or_toughness_ref(perm, ObjectProperty::Power)?,
        },

        // CR 107.3 + CR 208.1: "[permanent]'s toughness" — symmetric to
        // PowerOfPermanent; SelfToughness / EventContextSourceToughness
        // depending on the referent. No TargetToughness primitive yet, so
        // target-anaphoric refs strict-fail.
        GameNumber::ToughnessOfPermanent(perm) => QuantityExpr::Ref {
            qty: power_or_toughness_ref(perm, ObjectProperty::Toughness)?,
        },

        // CR 107.3 + CR 119.3: "[player]'s starting life total" — controller-
        // scoped only (StartingLifeTotal is a controller-relative resolver in
        // ability.rs:1742). Other-player starting-life refs are not yet
        // expressible.
        GameNumber::StartingLifeTotalOfPlayer(player) => match &**player {
            Player::You => QuantityExpr::Ref {
                qty: QuantityRef::StartingLifeTotal,
            },
            other => return Err(player_gap("StartingLifeTotalOfPlayer", other)),
        },

        // CR 107.3 + CR 402.1: "the highest number of cards in hand among
        // [opponents]" → OpponentHandSize (engine semantic: max hand count
        // across the controller's opponents — ability.rs:1990).
        GameNumber::TheHighestNumberOfCardsInHandAmongPlayers(players) => match &**players {
            Players::Opponent => QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Max,
                    },
                },
            },
            other => {
                return Err(players_gap(
                    "TheHighestNumberOfCardsInHandAmongPlayers",
                    other,
                ))
            }
        },

        // CR 107.3 + CR 402.1: "the total number of cards in [scope]'s hands"
        // → ZoneCardCount{ Hand, [], scope } — sums hand sizes across the
        // matching player set. AnyPlayer→All, Opponent→Opponents,
        // SinglePlayer(You)→Controller. Mirrors Multani, Maro-Sorcerer
        // ("equal to the total number of cards in all players' hands").
        GameNumber::TheTotalNumberOfCardsInPlayersHands(players) => {
            let scope = players_to_count_scope(players)
                .ok_or_else(|| players_gap("TheTotalNumberOfCardsInPlayersHands", players))?;
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Hand,
                    card_types: Vec::new(),
                    scope,
                },
            }
        }

        // CR 107.3e + CR 119.1: "the highest life total among [players]" —
        // aggregate-Max over the player set. Players::AnyPlayer covers "all
        // players" semantically (the player iteration is unrestricted), and
        // Π-1's PlayerScope::AllPlayers { aggregate: Max } is the engine's
        // canonical lift for cross-player life-total reductions. Other
        // Players shapes strict-fail until a richer Players→PlayerScope
        // mapping is established.
        GameNumber::HighestLifeTotalAmongPlayers(players) => match &**players {
            Players::AnyPlayer => QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::AllPlayers {
                        aggregate: AggregateFunction::Max,
                    },
                },
            },
            Players::Opponent => QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Max,
                    },
                },
            },
            other => return Err(players_gap("HighestLifeTotalAmongPlayers", other)),
        },

        // CR 107.3e + CR 208.1: "the greatest/least power|toughness among
        // [permanents]" / "the highest mana value among [permanents]" /
        // "total power|toughness|mana value of [permanents]". All map to
        // QuantityRef::Aggregate with the appropriate function and property.
        // Mirrors oracle_quantity.rs aggregate-pattern parsing.
        GameNumber::TheGreatestPowerAmongPermanents(filter) => aggregate_ref(
            AggregateFunction::Max,
            ObjectProperty::Power,
            convert_permanents(filter)?,
        ),
        GameNumber::TheGreatestToughnessAmongPermanents(filter) => aggregate_ref(
            AggregateFunction::Max,
            ObjectProperty::Toughness,
            convert_permanents(filter)?,
        ),
        GameNumber::TheLeastPowerAmongPermanents(filter) => aggregate_ref(
            AggregateFunction::Min,
            ObjectProperty::Power,
            convert_permanents(filter)?,
        ),
        GameNumber::TheLeastToughnessAmongPermanents(filter) => aggregate_ref(
            AggregateFunction::Min,
            ObjectProperty::Toughness,
            convert_permanents(filter)?,
        ),
        GameNumber::TheHighestManaValueAmongPermanents(filter) => aggregate_ref(
            AggregateFunction::Max,
            ObjectProperty::ManaValue,
            convert_permanents(filter)?,
        ),
        GameNumber::TotalManaValueOfPermanents(filter) => aggregate_ref(
            AggregateFunction::Sum,
            ObjectProperty::ManaValue,
            convert_permanents(filter)?,
        ),
        GameNumber::TotalPowerOfPermanents(filter) => aggregate_ref(
            AggregateFunction::Sum,
            ObjectProperty::Power,
            convert_permanents(filter)?,
        ),
        GameNumber::TotalToughnessOfPermanents(filter) => aggregate_ref(
            AggregateFunction::Sum,
            ObjectProperty::Toughness,
            convert_permanents(filter)?,
        ),

        // CR 201.2 + CR 603.4: "the number of [permanents] with different
        // names" → ObjectCountDistinctNames. Other GroupFilter variants
        // (DifferentControllers, SameToughness, …) lack a dedicated primitive
        // and strict-fail.
        GameNumber::NumGroupPermanents(filter, group) => {
            use crate::schema::types::GroupFilter;
            match group {
                GroupFilter::DifferentNames => QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCountDistinctNames {
                        filter: convert_permanents(filter)?,
                    },
                },
                other => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "QuantityRef",
                        needed_variant: format!("NumGroupPermanents/{other:?}"),
                    });
                }
            }
        }

        // CR 107.3 + CR 119.3: "the number of [players]" — count of players
        // matching a player-level filter → PlayerCount.
        GameNumber::NumPlayers(players) => match players_to_player_filter(players) {
            Some(filter) => QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount { filter },
            },
            None => return Err(players_gap("NumPlayers", players)),
        },

        // CR 700.5: "your devotion to [color]" — only You-controlled
        // (Devotion in ability.rs:1834 is controller-relative). Concrete
        // single colors only; chosen-color refs are not yet expressible.
        GameNumber::PlayerDevotionTo(player, color) => match (&**player, concrete_color(color)) {
            (Player::You, Some(mana_color)) => QuantityExpr::Ref {
                qty: QuantityRef::Devotion {
                    colors: vec![mana_color],
                },
            },
            (Player::You, None) => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "QuantityRef",
                    needed_variant: format!("PlayerDevotionTo/non-concrete-color/{color:?}"),
                });
            }
            (other, _) => return Err(player_gap("PlayerDevotionTo", other)),
        },

        // CR 119.3 + CR 603.4: "the amount of life [you/an opponent] lost
        // this turn" — controller-scoped → LifeLostThisTurn; opponent-scoped
        // → OpponentLifeLostThisTurn.
        GameNumber::LifeLostByPlayerThisTurn(player) => match &**player {
            Player::You => QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            other => return Err(player_gap("LifeLostByPlayerThisTurn", other)),
        },

        // CR 119.3: "the life you've gained this turn" — controller-scoped
        // → LifeGainedThisTurn (ability.rs:1936).
        GameNumber::LifeGainedByPlayerThisTurn(player) => match &**player {
            Player::You => QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: engine::types::ability::PlayerScope::Controller,
                },
            },
            other => return Err(player_gap("LifeGainedByPlayerThisTurn", other)),
        },

        // CR 122.1: "the number of [counter type] counters on [permanent]".
        // Permanent variant decides between CountersOnSelf (source object)
        // and CountersOnTarget (anaphoric target).
        GameNumber::NumCountersOfTypeOnPermanent(counter_type, perm) => QuantityExpr::Ref {
            qty: counters_of_type_on_permanent_ref(counter_type, perm)?,
        },

        // CR 122.1: "the number of [counter type] counters on [permanents]"
        // (across all matching objects) → CountersOnObjects.
        GameNumber::NumCountersOfTypeOnPermanents(counter_type, filter) => QuantityExpr::Ref {
            qty: QuantityRef::CountersOnObjects {
                counter_type: Some(counter_type_string(counter_type)),
                filter: convert_permanents(filter)?,
            },
        },

        // CR 122.1: "the number of counters on [permanent]" — bare form,
        // no counter-type qualifier, sums all counter kinds. Self vs target
        // mirrors CountersOnSelf/Target dichotomy via AnyCountersOn{Self,Target}.
        GameNumber::NumCountersOnPermanent(perm) => QuantityExpr::Ref {
            qty: any_counters_on_permanent_ref(perm)?,
        },

        // CR 700.7 + CR 603.4: "the number of creatures that died this turn"
        // — game-wide event count → CreaturesDiedThisTurn (ability.rs:1949).
        // The filter shape is preserved by the engine resolver as "all
        // creatures, any controller"; the schema's filter argument is only
        // used to confirm we're counting creatures. Strict-fail otherwise.
        GameNumber::NumCreaturesOrPlaneswalkersThatDiedThisTurn(_filter) => QuantityExpr::Ref {
            qty: QuantityRef::CreaturesDiedThisTurn,
        },

        // CR 603.7c: "the value of X of that spell" — reads
        // GameObject::cost_x_paid via the trigger event (ability.rs:1916).
        GameNumber::Trigger_ValueXOfThatSpell => QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourceCostX,
        },

        // CR 603.7c: "the amount of counters [put on a permanent]" anaphor
        // for the triggering CounterAdded event → EventContextAmount.
        GameNumber::WhenCountersArePutOnAPermanent_AmountOfCounters => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 603.7c: "the amount of damage [that creature] dealt" — the
        // numeric payload of the triggering DamageDealt event. Mirrors
        // EventContextAmount usage for damage-trigger anaphora (Balefire
        // Dragon, Backfire, Amarant Coral).
        GameNumber::Trigger_AmountOfDamageDealt
        | GameNumber::Trigger_AmountOfDamagePrevented
        | GameNumber::Trigger_AmountOfExcessDamage
        | GameNumber::Trigger_AmountOfLifeGained
        | GameNumber::Trigger_AmountOfLifeLost
        | GameNumber::Trigger_AmountOfCards
        | GameNumber::Trigger_AmountOfCreatures
        | GameNumber::Trigger_NumberOfCreatures
        | GameNumber::Trigger_NumberOfPlayersBeingAttacked
        | GameNumber::Trigger_ThatMuch => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 603.7c: "the amount of counters" anaphor on the triggering
        // CounterAdded event — a separate name surfaced by the schema for
        // generic counter triggers, same engine semantic as
        // WhenCountersArePutOnAPermanent_AmountOfCounters.
        GameNumber::Trigger_TheAmountOfCounters => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 614.1 + CR 603.7c: Replacement-context "would" anaphors —
        // "would deal that much damage" / "would gain that much life" /
        // "would lose that much life" / "would draw that many cards" /
        // "would scry that much" / "would put N counters" /
        // "would create N tokens" / "would pay that much life". All resolve
        // through the engine's per-event "amount" channel
        // (`state.last_replacement_event_amount`); the converter emits
        // EventContextAmount for the schema's typed anaphor variants.
        GameNumber::WouldDealDamage_ThatMuchDamage
        | GameNumber::WouldGainLife_LifeAmount
        | GameNumber::WouldLoseLife_ThatMuch
        | GameNumber::WouldPayLife_ThatMuch
        | GameNumber::WouldDrawACard_ThatMany
        | GameNumber::WouldScry_ThatMuch
        | GameNumber::WouldGetCounters_NumberOfCounters
        | GameNumber::WouldPutCounters_NumberOfCounters
        | GameNumber::WouldCreateTokens_NumberTokens => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 122.1b + CR 122.1f + CR 122.1i + CR 728: Player-counter counts
        // (poison, experience, rad, ticket) → PlayerCounter{kind, scope}.
        // Every other CounterType targeting a Player has no engine variant;
        // those strict-fail.
        GameNumber::NumCountersOfTypePlayerHas(counter_type, player) => {
            let kind = player_counter_kind(counter_type).ok_or_else(|| {
                ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "QuantityRef",
                    needed_variant: format!(
                        "PlayerCounter/non-player-counter-type/{counter_type:?}"
                    ),
                }
            })?;
            let scope = match &**player {
                Player::You => CountScope::Controller,
                other => return Err(player_gap("NumCountersOfTypePlayerHas", other)),
            };
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCounter { kind, scope },
            }
        }

        // CR 122.1b: "the total number of [counter] counters [scope] has" →
        // sum across the player set. Same kind constraints as
        // NumCountersOfTypePlayerHas; scope drawn from the Players filter.
        GameNumber::NumCountersOfTypePlayersHave(counter_type, players) => {
            let kind = player_counter_kind(counter_type).ok_or_else(|| {
                ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "QuantityRef",
                    needed_variant: format!(
                        "PlayerCounter/non-player-counter-type/{counter_type:?}"
                    ),
                }
            })?;
            let scope = players_to_count_scope(players)
                .ok_or_else(|| players_gap("NumCountersOfTypePlayersHave", players))?;
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCounter { kind, scope },
            }
        }

        // CR 107.3 + CR 122.1f: "the number of poison counters [player] has"
        // — convenience variant routing through PlayerCounter.
        GameNumber::TheNumberOfPoisonCountersPlayerHas(player) => {
            let scope = match &**player {
                Player::You => CountScope::Controller,
                other => return Err(player_gap("TheNumberOfPoisonCountersPlayerHas", other)),
            };
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCounter {
                    kind: PlayerCounterKind::Poison,
                    scope,
                },
            }
        }

        // CR 202.3: "[permanent]'s mana value". `~` / `it` / `this [type]`
        // resolve to the source object → SelfManaValue (correct at any
        // resolver scope per ability.rs:1818). Trigger anaphors
        // (`that creature` / `that permanent`) resolve through the trigger
        // event source → EventContextSourceManaValue (ability.rs:1886).
        // `Ref_TargetPermanent*` lacks an engine `TargetManaValue` primitive
        // (see power_or_toughness_ref's same gap) and strict-fails.
        GameNumber::ManaValueOfPermanent(perm) => QuantityExpr::Ref {
            qty: mana_value_of_permanent_ref(perm)?,
        },

        // CR 202.3: "[spell]'s mana value". `ThisSpell` reads the source
        // object → SelfManaValue. `Trigger_ThatSpell` / `ThatSpell` (the
        // triggering spell-cast event subject) → EventContextSourceManaValue.
        // `Ref_TargetSpell` would need a TargetManaValue primitive; defer.
        GameNumber::ManaValueOfSpell(spell) => QuantityExpr::Ref {
            qty: mana_value_of_spell_ref(spell)?,
        },

        // CR 601.2h: "the amount of mana spent to cast [spell]". `ThisSpell`
        // → ManaSpentOnSelf (resolves against the ability's own source —
        // ability.rs ~1882). `Trigger_ThatSpell` → ManaSpentOnTriggeringSpell
        // (resolves against the spell referenced by the current trigger event
        // — ability.rs ~1875). Other spell refs strict-fail.
        GameNumber::AmountOfManaSpentToCastSpell(spell) => QuantityExpr::Ref {
            qty: mana_spent_to_cast_ref(spell)?,
        },

        // CR 117.1 + CR 700.5: "the number of [spells] [player] has cast this
        // turn" → SpellsCastThisTurn { filter } (ability.rs:1925, resolved
        // against the controller / scope_player). The schema's player slot
        // is currently mapped only for Player::You — non-You players need a
        // scope-bearing variant in the engine, so they strict-fail.
        GameNumber::NumSpellsCastByPlayerThisTurn(spells, player) => match &**player {
            Player::You => QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    filter: Some(spells_to_filter(spells)?),
                },
            },
            other => return Err(player_gap("NumSpellsCastByPlayerThisTurn", other)),
        },

        // CR 117.1: "the number of [spells] cast this turn" (no player qualifier)
        // — controller-scoped at runtime via SpellsCastThisTurn.
        GameNumber::NumSpellsCastThisTurn(spells) => QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                filter: Some(spells_to_filter(spells)?),
            },
        },

        // CR 603.7c: Power/toughness anaphors keyed off "this way" / "the
        // sacrificed/devoured/exiled/discarded/revealed/dead [creature|card]".
        // Each refers back to the source object captured by the triggering
        // event (sacrifice/devour/exile/discard/reveal/dies trigger), so they
        // resolve through `EventContextSource{Power,Toughness}`. Matches the
        // native parser's `parse_event_context_quantity` behavior for "the
        // sacrificed creature's power" (oracle_quantity.rs:1152).
        GameNumber::PowerOfTheSacrificedCreature
        | GameNumber::PowerOfTheDevouredCreature
        | GameNumber::PowerOfTheExiledCreature
        | GameNumber::PowerOfTheDiscardedCard
        | GameNumber::PowerOfTheRevealedCard
        | GameNumber::PowerOfDeadPermanent => QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourcePower,
        },
        GameNumber::ToughnessOfTheSacrificedCreature
        | GameNumber::Emerge_ToughnessOfTheSacrificedCreature
        | GameNumber::ToughnessOfTheExiledCreature
        | GameNumber::ToughnessOfTheRevealedCard
        | GameNumber::ToughnessOfDeadPermanent
        | GameNumber::ToughnessOfCreatureDestroyedThisWay
        | GameNumber::ToughnessOfCreatureSacrificedThisWay => QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourceToughness,
        },

        // CR 603.7c + CR 202.3: Mana-value anaphors on triggering-event
        // sources — "the mana value of the sacrificed/discarded/exiled/milled/
        // revealed/dead/found permanent" all read the captured trigger source's
        // mana value. Distinct from `CostPaidObjectManaValue` (which the
        // native parser emits for the literal "the sacrificed creature's mana
        // value" cost-payment idiom; see oracle_nom/quantity.rs:826).
        // The schema's `_ManaValueOf*ThisWay` and `Trigger_ManaValueOf*`
        // variants are uniformly trigger-event anaphors → EventContextSourceManaValue.
        GameNumber::ManaValueOfTheSacrificedPermanent
        | GameNumber::Trigger_ManaValueOfTheSacrificedPermanent
        | GameNumber::ManaValueOfThePermanentSacrificedThisWay
        | GameNumber::ManaValueOfTheCardDiscardedThisWay
        | GameNumber::ManaValueOfTheCardExiledThisWay
        | GameNumber::ManaValueOfTheCardMilledThisWay
        | GameNumber::ManaValueOfTheCardRevealedThisWay
        | GameNumber::ManaValueOfTheDiscardedCard
        | GameNumber::ManaValueOfDeadPermanent
        | GameNumber::ManaValueOfCardPutInGraveyard
        | GameNumber::ManaValueOfCardPutInHandThisWay
        | GameNumber::ManaValueOfTheCardFoundThisWay
        | GameNumber::ManaValueOfTheFoundCard
        | GameNumber::TheManaValueOfTheCardDiscoveredThisWay => QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourceManaValue,
        },

        // CR 609.3: "the number of permanents tapped this way" — the
        // sub_ability chain tracks the tapped set; the count is read via
        // TrackedSetSize. Mirrors the native parser's "tapped this way"
        // mapping (oracle_quantity.rs:577).
        GameNumber::TheNumberOfPermanentsTappedThisWay => QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        },

        // CR 609.3: "the amount of damage dealt this way" / "the amount of
        // damage prevented this way" — sub-ability chain anaphor for the
        // preceding damage/prevention effect. Routed through the per-event
        // `EventContextAmount` channel (the same channel the native parser
        // uses for "1 damage prevented this way"; oracle_quantity.rs:574).
        GameNumber::TheAmountOfDamageDealtThisWay
        | GameNumber::TheAmountOfDamagePreventedThisWay
        | GameNumber::AmountOfExcessDamageDealtThisWay
        | GameNumber::TheClampedAmountOfDamageDealtThisWay => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 609.3: "the number of cards drawn this way" / "the number of
        // counters removed this way" / "the number of cards discarded /
        // exiled / milled this way" — preceding-effect numeric anaphors,
        // surfaced as EventContextAmount via the chain's amount channel.
        GameNumber::NumberOfCardsDrawnThisWay
        | GameNumber::NumberOfCountersRemovedThisWay
        | GameNumber::NumCardsDiscardedThisWay
        | GameNumber::NumGraveyardCardsExiledThisWay
        | GameNumber::NumPermanentsExiledThisWay
        | GameNumber::NumPermanentsPhasedOutThisWay
        | GameNumber::NumHandCardsExiledThisWay
        | GameNumber::NumHandCardsExiledFaceDownThisWay
        | GameNumber::NumCardsReturnedToHandThisWay
        | GameNumber::NumCardsPutIntoLibraryThisWay
        | GameNumber::NumCardsShuffledIntoLibraryThisWay
        | GameNumber::TheNumberOfCardsInHandRevealedThisWay
        | GameNumber::TheNumberOfTokensCreatedThisWay
        | GameNumber::TheNumberOfPermanentsSacrificedThisWay
        | GameNumber::TheNumberOfPermanentsReturnedToHandThisWay
        | GameNumber::TheNumberOfCardsPutIntoHandThisWay
        | GameNumber::TheNumberOfCardsReturnedToTheBattlefieldThisWay
        | GameNumber::TheNumberOfCardsManifestedThisWay
        | GameNumber::TheNumberOfPermanentsGainedControlOfThisWay
        | GameNumber::TheNumberOfCreaturesGoadedThisWay => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 119.3 + CR 603.4: "the amount of life lost this way" / "the
        // amount of life paid this way" / "the life paid" — payment/event
        // anaphors on the preceding effect's life delta.
        GameNumber::LifeLostThisWay
        | GameNumber::AmountOfLifePaidThisWay
        | GameNumber::TheLifePaid => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 107.3 + CR 401.1: "Plus3(a, b, c)" three-way sum. Only handled
        // when at most one operand is a non-Integer expression; otherwise we
        // can't compose without a general add-of-expressions primitive.
        GameNumber::Plus3(a, b, c) => match (&**a, &**b, &**c) {
            (GameNumber::Integer(x), GameNumber::Integer(y), inner)
            | (GameNumber::Integer(x), inner, GameNumber::Integer(y))
            | (inner, GameNumber::Integer(x), GameNumber::Integer(y)) => {
                let inner_expr = convert(inner)?;
                QuantityExpr::Offset {
                    inner: Box::new(inner_expr),
                    offset: x + y,
                }
            }
            _ => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "QuantityExpr",
                    needed_variant: "Plus3/multi-non-integer".to_string(),
                });
            }
        },

        // CR 700.2: "the chosen number" — a number chosen as the source
        // entered the battlefield (e.g., Talion, the Kindly Lord). Mirrors
        // the engine's `ChosenAttribute::Number` channel.
        GameNumber::TheChosenNumber => QuantityExpr::Ref {
            qty: QuantityRef::ChosenNumber,
        },

        // CR 105 + CR 109.1: "the number of colors among [permanents]" —
        // distinct colors across the matching permanent set. Composes with
        // the permanents-filter converter; mirrors the parser's CDA mapping
        // (oracle_quantity.rs DistinctColorsAmongPermanents).
        GameNumber::NumColorsAmongPermanents(filter) => QuantityExpr::Ref {
            qty: QuantityRef::DistinctColorsAmongPermanents {
                filter: convert_permanents(filter)?,
            },
        },
        // CR 105 + CR 109.1: "the number of colors of [permanent]" — the
        // single-permanent specialization. Engine slot is the same
        // `DistinctColorsAmongPermanents { filter }` taking a one-permanent
        // TargetFilter; the resolver counts distinct W/U/B/R/G across the
        // resolved set (CR 105.1 — gold/multicolor/colorless are not colors).
        GameNumber::NumColorsOfPermanent(perm) => QuantityExpr::Ref {
            qty: QuantityRef::DistinctColorsAmongPermanents {
                filter: convert_permanent(perm)?,
            },
        },

        // CR 406.1 + CR 604.3: "the number of cards in exile" — owner-agnostic
        // count of all exiled cards (the `CardsInExile` filter shape is
        // currently restricted to the trivial "any exiled card" forms;
        // anything richer strict-fails to keep the converter honest).
        GameNumber::NumCardsInExile(cards) => match &**cards {
            CardsInExile::AnyCard | CardsInExile::AnyExiledCard | CardsInExile::InExile => {
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Exile,
                        card_types: Vec::new(),
                        scope: CountScope::All,
                    },
                }
            }
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "QuantityRef",
                    needed_variant: format!(
                        "NumCardsInExile/non-trivial-CardsInExile-filter/{other:?}"
                    ),
                });
            }
        },

        // CR 401.1 + CR 604.3: "the number of cards in [player]'s library".
        GameNumber::NumCardsInPlayersLibrary(player) => {
            let scope = player_to_count_scope(player)
                .ok_or_else(|| player_gap("NumCardsInPlayersLibrary", player))?;
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Library,
                    card_types: Vec::new(),
                    scope,
                },
            }
        }

        // CR 107.3e + CR 202.3 + CR 406.1: "the greatest mana value among
        // exiled cards" / "the total mana value of exiled cards" → Aggregate
        // over an InZone(Exile) filter. The engine's Aggregate resolver uses
        // `extract_in_zone()` to pick up the exile zone (game/quantity.rs:580),
        // so the filter must carry an `InZone { Exile }` predicate.
        GameNumber::TheGreatestManaValueAmongExiledCards(cards) => aggregate_ref(
            AggregateFunction::Max,
            ObjectProperty::ManaValue,
            cards_in_exile_to_filter(cards)?,
        ),
        GameNumber::TotalManaValueOfExiledCards(cards) => aggregate_ref(
            AggregateFunction::Sum,
            ObjectProperty::ManaValue,
            cards_in_exile_to_filter(cards)?,
        ),

        // CR 604.3: "the number of card types among cards in [player]'s
        // graveyard" — distinct CoreType count across a player's graveyard.
        GameNumber::TheNumberOfCardtypesAmongGraveyardCards(cards) => {
            use crate::schema::types::CardsInGraveyard as CG;
            // The schema wraps a CardsInGraveyard predicate; the engine
            // primitive scopes by player, not by an arbitrary card filter.
            // Honor the trivial "any graveyard card" / "in [player]'s
            // graveyard" cases; anything richer strict-fails.
            let scope = match &**cards {
                CG::AnyCardInAnyGraveyard => CountScope::All,
                CG::InAPlayersGraveyard(players) => {
                    players_to_count_scope(players).ok_or_else(|| {
                        players_gap("TheNumberOfCardtypesAmongGraveyardCards", players)
                    })?
                }
                other => {
                    return Err(ConversionGap::EnginePrerequisiteMissing {
                        engine_type: "QuantityRef",
                        needed_variant: format!(
                            "TheNumberOfCardtypesAmongGraveyardCards/non-trivial-CardsInGraveyard-filter/{other:?}"
                        ),
                    });
                }
            };
            QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypesInZone {
                    zone: ZoneRef::Graveyard,
                    scope,
                },
            }
        }

        // CR 601.2h + CR 202.2: "the number of colors of mana spent to cast
        // [this spell / this entering permanent]" → ColorsSpentOnSelf. Both
        // schema variants reference the source object's own
        // `colors_spent_to_cast` channel (ability.rs:2036 resolves against
        // the entering object inside ETB replacements; otherwise the static
        // source).
        GameNumber::NumColorsManaSpentToCastEnteringPermanent => QuantityExpr::Ref {
            qty: QuantityRef::ColorsSpentOnSelf,
        },
        GameNumber::NumColorsManaSpentToCastSpell(spell) => match &**spell {
            Spell::ThisSpell => QuantityExpr::Ref {
                qty: QuantityRef::ColorsSpentOnSelf,
            },
            other => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "QuantityRef",
                    needed_variant: format!(
                        "NumColorsManaSpentToCastSpell/non-self-spell-ref/{other:?}"
                    ),
                });
            }
        },

        // CR 609.3: Sub-ability chain anaphors — "the number of [counter
        // type] counters removed this way" / "the number of permanents
        // destroyed this way" route through the EventContextAmount channel,
        // mirroring the existing "...this way" handlers above (the engine
        // doesn't distinguish counter-type or filter shape on the chain
        // counter — the preceding effect's amount is what's read).
        GameNumber::NumberOfCountersOfTypeRemovedThisWay(_)
        | GameNumber::NumPermanentsDestroyedThisWay(_) => QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },

        // CR 122.1 + CR 603.7c: "the number of [counter type] counters on
        // the dead permanent" — the dead-permanent referent is the trigger-
        // event source captured by the dies trigger, so we route through the
        // CountersOnTarget anaphor channel. Mirrors the existing
        // counters_of_type_on_permanent_ref Trigger_ThatDeadPermanent arm.
        GameNumber::NumCountersOfTypeOnDeadPermanent(counter_type) => QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: engine::types::ability::ObjectScope::Target,
                counter_type: Some(counter_type_string(counter_type)),
            },
        },

        // CR 305.6: "the number of basic land types among [permanents]" —
        // when the permanent filter is exactly "lands you control", this is
        // Domain (BasicLandTypeCount, controller-relative). Other filter
        // shapes lack a generalized engine primitive.
        GameNumber::NumberOfBasicLandTypesAmongPermanents(filter) => {
            if is_lands_you_control(filter) {
                QuantityExpr::Ref {
                    qty: QuantityRef::BasicLandTypeCount,
                }
            } else {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "QuantityRef",
                    needed_variant: format!(
                        "BasicLandTypeCount/non-lands-you-control-filter/{filter:?}"
                    ),
                });
            }
        }

        // CR 603.7c + CR 202.3: "the mana value of the [exiled] card" anaphor
        // — `CardInExile` distinguishes triggering-exile referents
        // ("this way" / "the exiled card" / etc.) from targeted-exile refs.
        // Trigger anaphors → EventContextSourceManaValue (mirrors the
        // ManaValueOf*ThisWay block above for graveyard/sacrifice referents).
        // `ThisExiledCard` / `ThisExiledPermanentCard` resolve to the source
        // object itself → SelfManaValue. Targeted-exile refs lack a
        // TargetManaValue primitive and strict-fail.
        GameNumber::ManaValueOfExiled(card) => QuantityExpr::Ref {
            qty: mana_value_of_exiled_ref(card)?,
        },

        _ => return Err(unsupported(g)),
    })
}

/// CR 202.3: Map a `Permanent` reference to its mana-value resolver.
/// Mirrors `power_or_toughness_ref`: source → SelfManaValue; trigger anaphors
/// → EventContextSourceManaValue; targeted-permanent refs lack a
/// TargetManaValue primitive in the engine and strict-fail.
fn mana_value_of_permanent_ref(perm: &Permanent) -> ConvResult<QuantityRef> {
    match perm {
        Permanent::ThisPermanent | Permanent::Self_It => Ok(QuantityRef::SelfManaValue),
        Permanent::Trigger_ThatPermanent
        | Permanent::Trigger_ThatCreature
        | Permanent::Trigger_ThatOtherPermanent
        | Permanent::Trigger_ThatOtherCreature
        | Permanent::Trigger_ThatCreatureOrPlaneswalker
        | Permanent::Trigger_ThatDeadPermanent
        | Permanent::ThatEnteringPermanent => Ok(QuantityRef::EventContextSourceManaValue),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("ManaValueOfPermanent/{other:?}"),
        }),
    }
}

/// CR 202.3: Map a `Spell` reference to its mana-value resolver.
/// `ThisSpell` (the source object on the stack) → SelfManaValue.
/// `Trigger_ThatSpell` / `ThatSpell` (triggering spell-cast event subject)
/// → EventContextSourceManaValue. Other spell anaphors lack engine support.
fn mana_value_of_spell_ref(spell: &Spell) -> ConvResult<QuantityRef> {
    match spell {
        Spell::ThisSpell => Ok(QuantityRef::SelfManaValue),
        Spell::Trigger_ThatSpell | Spell::ThatSpell => Ok(QuantityRef::EventContextSourceManaValue),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("ManaValueOfSpell/{other:?}"),
        }),
    }
}

/// CR 601.2h: Map a `Spell` reference to its "mana spent to cast" resolver.
/// `ThisSpell` → ManaSpentOnSelf (reads `mana_spent_to_cast_amount` on the
/// ability's source object). `Trigger_ThatSpell` / `ThatSpell` →
/// ManaSpentOnTriggeringSpell (reads the same field on the spell referenced
/// by the current trigger event). Other spell anaphors strict-fail.
fn mana_spent_to_cast_ref(spell: &Spell) -> ConvResult<QuantityRef> {
    match spell {
        Spell::ThisSpell => Ok(QuantityRef::ManaSpentOnSelf),
        Spell::Trigger_ThatSpell | Spell::ThatSpell => Ok(QuantityRef::ManaSpentOnTriggeringSpell),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("AmountOfManaSpentToCastSpell/{other:?}"),
        }),
    }
}

/// CR 208.1 / CR 107.3: Map a `Permanent` reference to either the source
/// object's stat (`SelfPower` / `SelfToughness`), the targeted object's
/// stat (`TargetPower` — toughness has no `TargetToughness` primitive yet),
/// or the trigger-event source's stat (`EventContextSourcePower` /
/// `EventContextSourceToughness` — for "that creature" / "that permanent"
/// trigger anaphors that resolve via the current trigger event).
fn power_or_toughness_ref(perm: &Permanent, prop: ObjectProperty) -> ConvResult<QuantityRef> {
    match (perm, prop) {
        (Permanent::ThisPermanent | Permanent::Self_It, ObjectProperty::Power) => {
            Ok(QuantityRef::Power {
                scope: engine::types::ability::ObjectScope::Source,
            })
        }
        (Permanent::ThisPermanent | Permanent::Self_It, ObjectProperty::Toughness) => {
            Ok(QuantityRef::Toughness {
                scope: engine::types::ability::ObjectScope::Source,
            })
        }
        (
            Permanent::Ref_TargetPermanent
            | Permanent::Ref_TargetPermanent1
            | Permanent::Ref_TargetPermanent2
            | Permanent::Ref_TargetPermanent3
            | Permanent::Ref_TargetPermanent4
            | Permanent::Ref_TargetPermanent5,
            ObjectProperty::Power,
        ) => Ok(QuantityRef::Power {
            scope: engine::types::ability::ObjectScope::Target,
        }),
        (
            Permanent::Ref_TargetPermanent
            | Permanent::Ref_TargetPermanent1
            | Permanent::Ref_TargetPermanent2
            | Permanent::Ref_TargetPermanent3
            | Permanent::Ref_TargetPermanent4
            | Permanent::Ref_TargetPermanent5,
            ObjectProperty::Toughness,
        ) => Ok(QuantityRef::Toughness {
            scope: engine::types::ability::ObjectScope::Target,
        }),
        // CR 603.7c: Trigger anaphors (`that creature` / `that permanent`)
        // resolve through the trigger event source. Mirrors oracle_quantity.rs
        // mapping of "that creature's power" → EventContextSourcePower.
        (
            Permanent::Trigger_ThatPermanent
            | Permanent::Trigger_ThatCreature
            | Permanent::Trigger_ThatOtherPermanent
            | Permanent::Trigger_ThatOtherCreature
            | Permanent::Trigger_ThatCreatureOrPlaneswalker
            | Permanent::Trigger_ThatDeadPermanent
            | Permanent::ThatEnteringPermanent,
            ObjectProperty::Power,
        ) => Ok(QuantityRef::EventContextSourcePower),
        (
            Permanent::Trigger_ThatPermanent
            | Permanent::Trigger_ThatCreature
            | Permanent::Trigger_ThatOtherPermanent
            | Permanent::Trigger_ThatOtherCreature
            | Permanent::Trigger_ThatCreatureOrPlaneswalker
            | Permanent::Trigger_ThatDeadPermanent
            | Permanent::ThatEnteringPermanent,
            ObjectProperty::Toughness,
        ) => Ok(QuantityRef::EventContextSourceToughness),
        // No TargetToughness or TargetManaValue primitive in the engine yet.
        (other, prop) => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("Target<{prop:?}>/{other:?}"),
        }),
    }
}

/// CR 122.1b: Map the schema's `CounterType` to engine
/// `PlayerCounterKind` for the four player-bearing counter kinds (poison,
/// experience, rad, ticket). Other counter types are object-borne and
/// strict-fail at the call site.
fn player_counter_kind(ct: &CounterType) -> Option<PlayerCounterKind> {
    match ct {
        CounterType::PoisonCounter => Some(PlayerCounterKind::Poison),
        CounterType::ExperienceCounter => Some(PlayerCounterKind::Experience),
        // Rad and Ticket counters lack dedicated schema variants in the
        // current vendored types; if the schema later adds them, extend
        // this mapping.
        _ => None,
    }
}

/// CR 119.3: Map the schema's `Players` filter to engine `CountScope` for
/// scope-aware quantity refs (PlayerCounter, ZoneCardCount). Mirrors the
/// `players_to_player_filter` shape but targets the narrower CountScope
/// enum.
fn players_to_count_scope(players: &Players) -> Option<CountScope> {
    match players {
        Players::SinglePlayer(p) if matches!(**p, Player::You) => Some(CountScope::Controller),
        Players::Opponent => Some(CountScope::Opponents),
        Players::AnyPlayer => Some(CountScope::All),
        _ => None,
    }
}

/// CR 122.1: Map (counter_type, permanent) → CountersOnSelf / CountersOnTarget.
fn counters_of_type_on_permanent_ref(
    counter_type: &CounterType,
    perm: &Permanent,
) -> ConvResult<QuantityRef> {
    let counter_type = Some(counter_type_string(counter_type));
    match perm {
        Permanent::ThisPermanent | Permanent::Self_It => Ok(QuantityRef::CountersOn {
            scope: engine::types::ability::ObjectScope::Source,
            counter_type,
        }),
        Permanent::Ref_TargetPermanent
        | Permanent::Ref_TargetPermanent1
        | Permanent::Ref_TargetPermanent2
        | Permanent::Ref_TargetPermanent3
        | Permanent::Ref_TargetPermanent4
        | Permanent::Ref_TargetPermanent5
        | Permanent::Trigger_ThatPermanent
        | Permanent::Trigger_ThatCreature
        | Permanent::Trigger_ThatOtherPermanent
        | Permanent::Trigger_ThatOtherCreature
        | Permanent::Trigger_ThatCreatureOrPlaneswalker
        | Permanent::Trigger_ThatDeadPermanent
        | Permanent::ThatEnteringPermanent
        | Permanent::TheChosenPermanent => Ok(QuantityRef::CountersOn {
            scope: engine::types::ability::ObjectScope::Target,
            counter_type,
        }),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("CountersOn<otherPermanentRef>/{other:?}"),
        }),
    }
}

/// CR 122.1: Map a bare "counters on [permanent]" reference to the
/// any-type variant of CountersOnSelf / CountersOnTarget.
fn any_counters_on_permanent_ref(perm: &Permanent) -> ConvResult<QuantityRef> {
    match perm {
        Permanent::ThisPermanent | Permanent::Self_It => Ok(QuantityRef::CountersOn {
            scope: engine::types::ability::ObjectScope::Source,
            counter_type: None,
        }),
        Permanent::Ref_TargetPermanent
        | Permanent::Ref_TargetPermanent1
        | Permanent::Ref_TargetPermanent2
        | Permanent::Ref_TargetPermanent3
        | Permanent::Ref_TargetPermanent4
        | Permanent::Ref_TargetPermanent5
        | Permanent::Trigger_ThatPermanent
        | Permanent::Trigger_ThatCreature
        | Permanent::Trigger_ThatOtherPermanent
        | Permanent::Trigger_ThatOtherCreature
        | Permanent::Trigger_ThatCreatureOrPlaneswalker
        | Permanent::Trigger_ThatDeadPermanent
        | Permanent::ThatEnteringPermanent
        | Permanent::TheChosenPermanent => Ok(QuantityRef::CountersOn {
            scope: engine::types::ability::ObjectScope::Target,
            counter_type: None,
        }),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("AnyCountersOn<otherPermanentRef>/{other:?}"),
        }),
    }
}

/// CR 107.3e: Wrap an aggregate query over a filter into a QuantityExpr.
fn aggregate_ref(
    function: AggregateFunction,
    property: ObjectProperty,
    filter: engine::types::ability::TargetFilter,
) -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::Aggregate {
            function,
            property,
            filter,
        },
    }
}

/// CR 119.3 + CR 603.4: Narrow `Players` → `PlayerFilter` mapping for
/// quantity contexts. Mirrors the engine parser's PlayerFilter usage —
/// only the static, controller-relative cases are expressible here.
fn players_to_player_filter(players: &Players) -> Option<PlayerFilter> {
    match players {
        Players::SinglePlayer(p) if matches!(**p, Player::You) => Some(PlayerFilter::Controller),
        Players::Opponent => Some(PlayerFilter::Opponent),
        Players::AnyPlayer => Some(PlayerFilter::All),
        _ => None,
    }
}

/// Render a CounterType as the engine's stringly-typed counter identifier.
/// Engine convention (parser/oracle_effect/counter.rs::normalize_counter_type)
/// uses "P1P1" / "M1M1" for +1/+1 and -1/-1 counters; everything else is the
/// PascalCase variant name with the trailing "Counter" stripped.
fn counter_type_string(ct: &CounterType) -> String {
    match ct {
        CounterType::PTCounter(1, 1) => "P1P1".to_string(),
        CounterType::PTCounter(-1, -1) => "M1M1".to_string(),
        CounterType::PTCounter(p, t) => format!("{p:+}/{t:+}"),
        other => format!("{other:?}")
            .strip_suffix("Counter")
            .map(str::to_string)
            .unwrap_or_else(|| format!("{other:?}")),
    }
}

fn player_gap(idiom: &'static str, p: &Player) -> ConversionGap {
    ConversionGap::MalformedIdiom {
        idiom: "GameNumber/convert",
        path: String::new(),
        detail: format!("{idiom}: non-You player: {p:?}"),
    }
}

fn players_gap(idiom: &'static str, p: &Players) -> ConversionGap {
    ConversionGap::MalformedIdiom {
        idiom: "GameNumber/convert",
        path: String::new(),
        detail: format!("{idiom}: unsupported Players: {p:?}"),
    }
}

/// CR 119.3: Single-player → CountScope. Mirrors `players_to_count_scope` for
/// the singular `Player` type; only the controller-relative case is currently
/// expressible (the engine's CountScope is controller/opponents/all, with no
/// per-target slot).
fn player_to_count_scope(player: &Player) -> Option<CountScope> {
    match player {
        Player::You => Some(CountScope::Controller),
        _ => None,
    }
}

/// CR 406.1 + CR 604.3: Map a `CardsInExile` predicate to a TargetFilter
/// that carries `InZone { Exile }` so the engine's Aggregate resolver
/// (game/quantity.rs:580 — `extract_in_zone()`) walks the exile zone instead
/// of the battlefield. Only the trivial "any exiled card" / mana-value
/// comparison shapes are supported; richer filters strict-fail to keep the
/// converter honest about what makes it into engine resolution.
fn cards_in_exile_to_filter(cards: &CardsInExile) -> ConvResult<TargetFilter> {
    match cards {
        CardsInExile::AnyCard | CardsInExile::AnyExiledCard | CardsInExile::InExile => {
            Ok(TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::InZone { zone: Zone::Exile },
            ])))
        }
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "TargetFilter",
            needed_variant: format!("CardsInExile-aggregate-filter/{other:?}"),
        }),
    }
}

/// CR 603.7c + CR 202.3: Map a `CardInExile` referent to its mana-value
/// resolver. Triggering-exile anaphors ("this way" / "the exiled card") read
/// the trigger event source → EventContextSourceManaValue. `ThisExiledCard`
/// / `ThisExiledPermanentCard` (the source object itself) → SelfManaValue.
/// Targeted-exile refs lack a TargetManaValue primitive and strict-fail.
fn mana_value_of_exiled_ref(card: &CardInExile) -> ConvResult<QuantityRef> {
    match card {
        CardInExile::ThisExiledCard | CardInExile::ThisExiledPermanentCard => {
            Ok(QuantityRef::SelfManaValue)
        }
        CardInExile::TheLastExiledCard
        | CardInExile::TheCardConjuredIntoExileThisWay
        | CardInExile::TheExiledCardChosenThisWay
        | CardInExile::EachableExiled
        | CardInExile::TopCardOfExiledPile
        | CardInExile::WhenAPermanentIsExiled_ThatExiledPermanent
        | CardInExile::TheExiledDeadPermanent
        | CardInExile::TheExiledTopOfLibrary
        | CardInExile::TheCardExiledThisWay
        | CardInExile::TheChosenExiledCard
        | CardInExile::TheExiledCard
        | CardInExile::TheExiledCardFoundThisWay
        | CardInExile::TheFirstCardExiledThisWay
        | CardInExile::TheSecondCardExiledThisWay
        | CardInExile::TheSingleCardExiledThisWay
        | CardInExile::TheSinglePermanentExiledThisWay
        | CardInExile::TheSpecificCardExiledThisWay
        | CardInExile::Trigger_ThatExiledCard => Ok(QuantityRef::EventContextSourceManaValue),
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "QuantityRef",
            needed_variant: format!("ManaValueOfExiled/{other:?}"),
        }),
    }
}

/// CR 305.6: Match the schema's "lands you control" idiom — an `And`
/// combination of `IsCardtype(Land)` and `ControlledByAPlayer(SinglePlayer(You))`
/// in either order. Used to gate `BasicLandTypeCount`, which is intrinsically
/// controller-and-lands-only (Domain, CR 305.6).
fn is_lands_you_control(p: &Permanents) -> bool {
    let parts = match p {
        Permanents::And(parts) => parts,
        _ => return false,
    };
    if parts.len() != 2 {
        return false;
    }
    let mut has_land = false;
    let mut has_you = false;
    for part in parts {
        match part {
            Permanents::IsCardtype(CardType::Land) => has_land = true,
            Permanents::ControlledByAPlayer(players) => {
                if let Players::SinglePlayer(p) = &**players {
                    if matches!(**p, Player::You) {
                        has_you = true;
                    }
                }
            }
            _ => return false,
        }
    }
    has_land && has_you
}

fn unsupported(g: &GameNumber) -> ConversionGap {
    let tag = serde_json::to_value(g)
        .ok()
        .and_then(|v| {
            v.get("_GameNumber")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".into());
    ConversionGap::MalformedIdiom {
        idiom: "GameNumber/convert",
        path: String::new(),
        // Tag-leading format so the report's sub-bin discriminator
        // (everything before the first `:`) is the GameNumber variant
        // tag itself. `MalformedIdiom[...]` sub-binning relies on this
        // convention.
        detail: format!("{tag}: unsupported variant"),
    }
}
