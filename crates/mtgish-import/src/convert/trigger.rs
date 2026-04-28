//! mtgish `Trigger` → engine `TriggerDefinition` (Phase 5 narrow slice).
//!
//! Maps the most common ETB / Dies / Attacks / Phase trigger shapes into
//! engine `TriggerMode` plus the appropriate `valid_card` / `origin` /
//! `destination` / `phase` filters. mtgish has 377 Trigger variants — only
//! the highest-frequency are mapped here; the long tail fails strict and
//! shows up in the report.

use engine::types::ability::{DamageKindFilter, TriggerConstraint};
use engine::types::triggers::TriggerMode;
use engine::types::{Phase, TargetFilter, TriggerDefinition, TypedFilter, Zone};

use crate::convert::filter::{
    cards_in_graveyard_to_filter, cards_to_filter, convert as convert_permanents,
    players_to_controller, spells_to_filter,
};
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{CardsInHand, Comparison, GameNumber, Players, Trigger};

/// CR 603: Convert a mtgish `Trigger` into one or more engine
/// `TriggerDefinition`s. Most triggers map 1:1; `Trigger::Or(Vec<Trigger>)`
/// fans out, mirroring CR 603's "fires when any of [list]" semantics —
/// each disjunct becomes its own TriggerDefinition sharing the same
/// downstream execute body via clones at the caller. The caller is
/// responsible for cloning the body across the returned definitions.
pub fn convert_many(t: &Trigger) -> ConvResult<Vec<TriggerDefinition>> {
    match t {
        // CR 603 (general): "When [A] or [B], do X." Each branch becomes
        // its own trigger; the engine fires whichever event happens (a
        // single resolved game event still produces one body resolution
        // because only one branch matches at a time).
        Trigger::Or(parts) => {
            let mut out = Vec::with_capacity(parts.len());
            for p in parts {
                out.extend(convert_many(p)?);
            }
            if out.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Trigger::Or/empty",
                    path: String::new(),
                    detail: "empty disjunction".into(),
                });
            }
            Ok(out)
        }
        _ => Ok(vec![convert(t)?]),
    }
}

/// CR 603.6: convert a mtgish `Trigger` into an engine `TriggerDefinition`.
/// The returned definition has its event filters set; the caller wires the
/// `execute` ability and any intervening-if `condition`.
pub fn convert(t: &Trigger) -> ConvResult<TriggerDefinition> {
    Ok(match t {
        // CR 603.6a: ETB triggers — "When [filter] enters the battlefield".
        Trigger::WhenAPermanentEntersTheBattlefield(filter) => {
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(convert_permanents(filter)?)
        }

        // CR 603.6c + CR 700.4: Dies triggers — "When [creature/PW] dies".
        // CR 700.4: "Dies" is shorthand for "is put into a graveyard from the
        // battlefield." Singular "dies" / "is put into" both fire per-event.
        Trigger::WhenACreatureOrPlaneswalkerDies(filter) => {
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .destination(Zone::Graveyard)
                .valid_card(convert_permanents(filter)?)
        }
        // CR 700.4 + CR 603.10a: "Whenever one or more creatures or planeswalkers
        // die" — batched dies trigger. `ChangesZoneAll` + `batched = true` mirrors
        // the native parser at oracle_trigger.rs:3194-3196 (one-or-more zone-change
        // triggers fire once per simultaneous batch).
        Trigger::WhenAnyNumberOfCreaturesOrPlaneswalkersDie(filter) => {
            let mut def = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
                .origin(Zone::Battlefield)
                .destination(Zone::Graveyard)
                .valid_card(convert_permanents(filter)?);
            def.batched = true;
            def
        }

        // CR 700.4: "[permanent] is put into a player's graveyard" — the permanent
        // form is functionally equivalent to the dies trigger (origin=battlefield,
        // destination=graveyard) but covers non-creature/non-planeswalker permanents
        // (artifacts, enchantments, lands) for which "dies" doesn't apply per CR 700.4.
        // Mirrors `parse_dies_verb` in native parser at oracle_trigger.rs:2402-2409.
        // The `_players` axis (graveyard owner) is dropped — TriggerDefinition has no
        // valid_player axis, matching the convention from existing arms (e.g.
        // WhenAPlayerSacrificesAPermanent line 126).
        Trigger::WhenAPermanentIsPutIntoAPlayersGraveyard(filter, _players) => {
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .destination(Zone::Graveyard)
                .valid_card(convert_permanents(filter)?)
        }
        // CR 700.4 + CR 603.10a: Batched form — "any number of permanents are put
        // into players' graveyards." `ChangesZoneAll` + batched.
        Trigger::WhenAnyNumberOfPermanentsArePutIntoAPlayersGraveyards(filter, _players) => {
            let mut def = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
                .origin(Zone::Battlefield)
                .destination(Zone::Graveyard)
                .valid_card(convert_permanents(filter)?);
            def.batched = true;
            def
        }

        // CR 700.4 + CR 603.6e: "[card] is put into a player's graveyard from
        // anywhere" — fires on any zone change ending in graveyard. Origin is
        // unrestricted (None). Mirrors `try_parse_put_into_graveyard` with
        // "from anywhere" branch at oracle_trigger.rs:4890-4891.
        Trigger::WhenACardIsPutIntoAPlayersGraveyardFromAnywhere(cards, _players) => {
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Graveyard)
                .valid_card(cards_to_filter(cards)?)
        }
        // CR 700.4 + CR 603.10a: Batched form of put-into-graveyard-from-anywhere.
        Trigger::WhenAnyNumberOfCardsArePutIntoAPlayersGraveyardFromAnywhere(cards, _players) => {
            let mut def = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
                .destination(Zone::Graveyard)
                .valid_card(cards_to_filter(cards)?);
            def.batched = true;
            def
        }

        // CR 700.4: Library → Graveyard (mill-style). Mirrors native parser's
        // origin=Library branch at oracle_trigger.rs:4892.
        Trigger::WhenACardIsPutIntoAPlayersGraveyardFromTheirLibrary(cards, _players) => {
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Library)
                .destination(Zone::Graveyard)
                .valid_card(cards_to_filter(cards)?)
        }
        Trigger::WhenAnyNumberOfCardsArePutIntoAPlayersGraveyardFromTheirLibrary(
            cards,
            _players,
        ) => {
            let mut def = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
                .origin(Zone::Library)
                .destination(Zone::Graveyard)
                .valid_card(cards_to_filter(cards)?);
            def.batched = true;
            def
        }
        // CR 700.4: Hand → Graveyard (discard-style zone change distinct from
        // CR 701.9 discard, which fires on the discard action regardless of where
        // the card ends up). Mirrors native parser's origin=Hand branch at
        // oracle_trigger.rs:4893.
        Trigger::WhenACardIsPutIntoAPlayersGraveyardFromTheirHand(cards, _players) => {
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Hand)
                .destination(Zone::Graveyard)
                .valid_card(cards_to_filter(cards)?)
        }

        // CR 603.6a: ETB triggers from a specific origin zone — "[permanent]
        // enters the battlefield from a graveyard." Reanimation-detect triggers.
        // Origin = Graveyard, destination = Battlefield.
        Trigger::WhenAPermanentEntersTheBattlefieldFromAPlayersGraveyard(filter, _players) => {
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Graveyard)
                .destination(Zone::Battlefield)
                .valid_card(convert_permanents(filter)?)
        }

        // CR 400.3 + CR 603.10a: "Whenever [card] leaves a graveyard" — the
        // graveyard is the origin zone; destination is unrestricted. Source object
        // is in graveyard at trigger-fire time, so `trigger_zones` must include
        // Graveyard (and Exile / Battlefield) to allow the ability to fire from
        // there. Mirrors `try_parse_one_or_more_leave_graveyard` at
        // oracle_trigger.rs:3432-3438.
        Trigger::WhenAGraveyardCardLeaves(filter) => TriggerDefinition::new(TriggerMode::ChangesZone)
            .origin(Zone::Graveyard)
            .valid_card(cards_in_graveyard_to_filter(filter)?)
            .trigger_zones(vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile]),
        Trigger::WhenAnyNumberOfGraveyardCardsLeave(filter) => {
            let mut def = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
                .origin(Zone::Graveyard)
                .valid_card(cards_in_graveyard_to_filter(filter)?)
                .trigger_zones(vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile]);
            def.batched = true;
            def
        }

        // CR 700.4: "from anywhere other than the battlefield" — the origin axis
        // can't be expressed as a single excluded zone with the current
        // `origin: Option<Zone>` field (it's "any zone, but not battlefield"; the
        // engine's None means "any zone including battlefield"). Strict-fail until
        // engine extends `origin` to a typed inclusion/exclusion set.
        Trigger::WhenACardIsPutIntoAGraveyardFromAnywhereOtherThanTheBattlefield(_cards, _players) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerDefinition",
                needed_variant: "ChangesZone with origin-exclusion (anywhere other than battlefield)"
                    .into(),
            });
        }
        // CR 603.10a: "for the first time each turn" — once-per-turn dampener on a
        // batched zone-change trigger. Engine has no per-turn-once trigger
        // constraint that gates on this batched event. Strict-fail.
        Trigger::WhenAnyNumberOfCardsArePutIntoAPlayersGraveyardFromAnywhereForTheFirstTimeEachTurn(
            _cards,
            _players,
        ) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerDefinition",
                needed_variant: "ChangesZoneAll with first-time-each-turn constraint".into(),
            });
        }
        // CR 603.10c: "leaves the battlefield without dying" — destination must be
        // exile/library/hand/command (i.e. NOT graveyard). The
        // `LeavesBattlefield` mode + a destination-exclusion predicate is not
        // expressible today. Strict-fail.
        Trigger::WhenAPermanentLeavesTheBattlefieldWithoutDying(_filter) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerDefinition",
                needed_variant: "LeavesBattlefield with destination-exclusion (not Graveyard)"
                    .into(),
            });
        }

        Trigger::WhenAPermanentLeavesTheBattlefield(filter) => {
            TriggerDefinition::new(TriggerMode::LeavesBattlefield)
                .valid_card(convert_permanents(filter)?)
        }

        // CR 508.3a: Attack triggers.
        Trigger::WhenACreatureAttacks(filter) => {
            TriggerDefinition::new(TriggerMode::Attacks).valid_card(convert_permanents(filter)?)
        }

        // CR 509.1h: Block triggers.
        Trigger::WhenACreatureBlocks(filter) => {
            TriggerDefinition::new(TriggerMode::Blocks).valid_card(convert_permanents(filter)?)
        }
        Trigger::WhenACreatureBecomesBlocked(filter) => {
            TriggerDefinition::new(TriggerMode::BecomesBlocked)
                .valid_card(convert_permanents(filter)?)
        }

        // CR 120.2: Damage triggers.
        Trigger::WhenAPermanentDealsDamage(filter) => {
            TriggerDefinition::new(TriggerMode::DamageDone)
                .valid_source(convert_permanents(filter)?)
        }
        Trigger::WhenAPermanentDealsDamageToAPlayer(filter, _players) => {
            TriggerDefinition::new(TriggerMode::DamageDone)
                .valid_source(convert_permanents(filter)?)
                .valid_target(TargetFilter::Player)
        }
        Trigger::WhenAPermanentDealsDamageToAPermanent(filter, targets) => {
            TriggerDefinition::new(TriggerMode::DamageDone)
                .valid_source(convert_permanents(filter)?)
                .valid_target(convert_permanents(targets)?)
        }

        // CR 120.2a + CR 510.2: Combat-damage-only damage triggers. Combat
        // damage is dealt simultaneously during the combat damage step (510.2)
        // and is distinguished from noncombat damage at CR 120.2a. Engine uses
        // the same `DamageDone` mode as non-combat damage but distinguishes via
        // the existing `damage_kind: CombatOnly` filter. Source / target axes
        // mirror the non-combat `WhenAPermanentDealsDamage*` arms above.
        Trigger::WhenACreatureDealsCombatDamage(filter) => {
            TriggerDefinition::new(TriggerMode::DamageDone)
                .valid_source(convert_permanents(filter)?)
                .damage_kind(DamageKindFilter::CombatOnly)
        }
        Trigger::WhenACreatureDealsCombatDamageToAPlayer(filter, _players) => {
            TriggerDefinition::new(TriggerMode::DamageDone)
                .valid_source(convert_permanents(filter)?)
                .valid_target(TargetFilter::Player)
                .damage_kind(DamageKindFilter::CombatOnly)
        }
        Trigger::WhenACreatureDealsCombatDamageToAPermanent(filter, targets) => {
            TriggerDefinition::new(TriggerMode::DamageDone)
                .valid_source(convert_permanents(filter)?)
                .valid_target(convert_permanents(targets)?)
                .damage_kind(DamageKindFilter::CombatOnly)
        }

        // CR 701.21: Sacrifice triggers.
        Trigger::WhenAPlayerSacrificesAPermanent(_players, filter) => {
            TriggerDefinition::new(TriggerMode::Sacrificed).valid_card(convert_permanents(filter)?)
        }
        // CR 701.8: Destroy triggers.
        Trigger::WhenAPermanentIsDestroyed(filter) => {
            TriggerDefinition::new(TriggerMode::Destroyed).valid_card(convert_permanents(filter)?)
        }
        // CR 701.9: Discard triggers — "when [players] discards [cards]". Engine
        // TriggerDefinition::Discarded has no valid_player or discarded-card
        // filter axis today, so dropping the player/card constraints fires the
        // trigger on every discard regardless of who/what. Strict-fail until
        // engine extends.
        Trigger::WhenAPlayerDiscardsACard(_players, _cards) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerDefinition",
                needed_variant: "Discarded with valid_player + discarded-card filter".into(),
            });
        }
        // CR 702.29 + CR 603: Cycling triggers — "whenever [a player] cycles a
        // card". Engine `TriggerMode::Cycled` fires per cycle event; the
        // player axis lives on `valid_target` (mirroring native parser at
        // oracle_trigger.rs:4162-4189). Specific card-type predicates on the
        // cycled card (`CardsInHand` other than `AnyCard`) have no engine
        // axis today (TriggerDefinition has no cycled-card filter slot) and
        // strict-fail until that lands.
        Trigger::WhenAPlayerCyclesACard(players, cards) => {
            cycled_trigger(
                players,
                cards,
                TriggerMode::Cycled,
                "Trigger::WhenAPlayerCyclesACard",
            )?
        }
        // CR 702.29d: "whenever [a player] cycles or discards a card" —
        // unifies the two events into one trigger that fires once per
        // qualifying event. Same player-axis handling as `Cycled`.
        Trigger::WhenAPlayerCyclesOrDiscardsACard(players, cards) => cycled_trigger(
            players,
            cards,
            TriggerMode::CycledOrDiscarded,
            "Trigger::WhenAPlayerCyclesOrDiscardsACard",
        )?,
        // CR 702.29 + CR 603.4: "whenever [a player] cycles a card for the
        // first time each turn" — adds a per-turn frequency gate on top of
        // the Cycled mode. `TriggerConstraint::OncePerTurn` encodes the
        // "first only" cap.
        Trigger::WhenAPlayerCyclesACardForTheFirstTimeEachTurn(players, cards) => {
            let mut def = cycled_trigger(
                players,
                cards,
                TriggerMode::Cycled,
                "Trigger::WhenAPlayerCyclesACardForTheFirstTimeEachTurn",
            )?;
            def.constraint = Some(TriggerConstraint::OncePerTurn);
            def
        }
        // CR 702.37c (Morph) + CR 701.40b (Turn Face Up): "Whenever [permanent]
        // is turned face up" — fires when a face-down permanent flips face up
        // via the morph activation (or any other Turn-Face-Up effect, e.g.
        // Manifest CR 701.58b). Engine `TriggerMode::TurnFaceUp` is the
        // unit mode consumed by the `match_turn_face_up` matcher (see
        // `game/trigger_matchers.rs`), with `valid_card` constraining which
        // permanent's flip fires the trigger. Mirrors the native parser's
        // mapping at `oracle_trigger.rs:2675` (`SimpleEvent::TurnFaceUp`).
        Trigger::WhenAPermanentIsTurnedFaceUp(filter) => {
            TriggerDefinition::new(TriggerMode::TurnFaceUp).valid_card(convert_permanents(filter)?)
        }

        // CR 701.26: Tap/untap triggers.
        Trigger::WhenAPermanentBecomesTapped(filter) => {
            TriggerDefinition::new(TriggerMode::Taps).valid_card(convert_permanents(filter)?)
        }
        Trigger::WhenAPermanentBecomesUntapped(filter) => {
            TriggerDefinition::new(TriggerMode::Untaps).valid_card(convert_permanents(filter)?)
        }
        // CR 605.1 + CR 106.1: "Whenever [a player] taps [permanent] for mana"
        // — fires on the mana-ability resolution, distinct from the bare tap
        // event. The player axis goes onto `valid_target`; the permanent
        // filter onto `valid_card`. Mirrors WhenAPermanentBecomesTapped's
        // filter-shape but with the mana-specific TriggerMode.
        Trigger::WhenAPlayerTapsAPermanentForMana(players, filter) => {
            let mut def = TriggerDefinition::new(TriggerMode::TapsForMana)
                .valid_card(convert_permanents(filter)?);
            if !matches!(**players, Players::AnyPlayer) {
                let controller = players_to_controller(players)?;
                def.valid_target = Some(TargetFilter::Typed(
                    TypedFilter::default().controller(controller),
                ));
            }
            def
        }
        // CR 509.1h + CR 506.4: "Whenever [creature] attacks and isn't
        // blocked" — fires once per unblocked attacker after blockers are
        // declared. Engine `TriggerMode::AttackerUnblocked` mirrors the
        // declared-blockers-step decision (vs `AttackerUnblockedOnce` which
        // collapses N attackers into one trigger).
        Trigger::WhenACreatureAttacksAndIsntBlocked(filter) => {
            TriggerDefinition::new(TriggerMode::AttackerUnblocked)
                .valid_card(convert_permanents(filter)?)
        }
        // CR 603.6a: ETB sub-shapes — "enters tapped/untapped/transformed" must
        // gate the trigger on the entering object's state. Engine TriggerDefinition
        // has no entry_state / tapped_predicate / transformed_predicate field today,
        // so collapsing these to a plain ChangesZone trigger fires on every ETB
        // (rules-correctness violation). Strict-fail until engine extends the
        // shape.
        Trigger::WhenAPermanentEntersTheBattlefieldTapped(_filter)
        | Trigger::WhenAPermanentEntersTheBattlefieldUntapped(_filter)
        | Trigger::WhenAPermanentEntersTheBattlefieldTransformed(_filter) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerDefinition",
                needed_variant:
                    "ChangesZone with entry-state predicate (tapped/untapped/transformed)".into(),
            });
        }
        // CR 603.6a: "enters under [player]'s control" must gate the trigger on
        // the entering object's controller. Engine TriggerDefinition has no
        // valid_player axis on ChangesZone today, so dropping the player
        // constraint fires on every controller. Strict-fail until engine extends.
        Trigger::WhenAPermanentEntersTheBattlefieldUnderAPlayersControl(_filter, _players) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerDefinition",
                needed_variant: "ChangesZone with controller predicate (under a player's control)"
                    .into(),
            });
        }

        // CR 121.1: Card draw.
        Trigger::WhenAPlayerDrawsACard(_players) => TriggerDefinition::new(TriggerMode::Drawn),

        // CR 601.2i: Spell cast.
        Trigger::WhenASpellIsCast(_spells) => TriggerDefinition::new(TriggerMode::SpellCast),
        Trigger::WhenAPlayerCastsASpell(_players, _spells) => {
            TriggerDefinition::new(TriggerMode::SpellCast)
        }

        // CR 119.4 / CR 119.3: Life triggers.
        Trigger::WhenAPlayerGainsLife(_players) => TriggerDefinition::new(TriggerMode::LifeGained),
        Trigger::WhenAPlayerLosesLife(_players) => TriggerDefinition::new(TriggerMode::LifeLost),

        // CR 603.2b: Phase triggers — "At the beginning of [scope] [phase/step]".
        Trigger::AtTheBeginningOfAPlayersUpkeep(_players) => {
            TriggerDefinition::new(TriggerMode::Phase).phase(Phase::Upkeep)
        }
        Trigger::AtTheBeginningOfAPlayersDrawStep(_players) => {
            TriggerDefinition::new(TriggerMode::Phase).phase(Phase::Draw)
        }
        Trigger::AtTheBeginningOfAPlayersEndStep(_players) => {
            TriggerDefinition::new(TriggerMode::Phase).phase(Phase::End)
        }
        Trigger::AtTheBeginningOfCombat => {
            TriggerDefinition::new(TriggerMode::Phase).phase(Phase::BeginCombat)
        }
        Trigger::AtTheBeginningOfCombatDuringAPlayersTurn(_players) => {
            TriggerDefinition::new(TriggerMode::Phase).phase(Phase::BeginCombat)
        }

        // CR 311.7: Chaos abilities trigger "Whenever chaos ensues" — fired by
        // the planar die's chaos symbol, by resolving spells/abilities that say
        // chaos ensues, or for a particular object. Engine `TriggerMode::ChaosEnsues`
        // is a unit mode (no plane/source filter) — mirrors what the native
        // oracle_trigger parser produces (see oracle_trigger.rs:2854).
        Trigger::WhenChaosEnsues => TriggerDefinition::new(TriggerMode::ChaosEnsues),

        // CR 701.32 + CR 904: "When you set this scheme in motion" — fires when
        // the archenemy moves the top scheme off the scheme deck and turns it
        // face up (CR 505.3 turn-based action, or via instruction). The mtgish
        // shape carries `Players` and `Schemes` filters, but engine
        // `TriggerMode::SetInMotion` is a unit mode — the scheme self-reference
        // is implicit ("this scheme") and the player axis is constrained by the
        // archenemy rule (CR 904.7). Mirrors native parser at oracle_trigger.rs:2861.
        Trigger::WhenAPlayerSetsASchemeInMotion(_players, _schemes) => {
            TriggerDefinition::new(TriggerMode::SetInMotion)
        }

        // CR 701.31 + CR 901.11: "When you planeswalk to/away from a plane" —
        // fires when a face-up plane card changes (CR 701.31d). Engine
        // `TriggerMode::PlaneswalkedTo` / `PlaneswalkedFrom` are unit modes;
        // the `Plane` filter and `Players` axis are dropped (planar controller
        // is implicit per CR 901.6).
        Trigger::WhenAPlayerPlaneswalksToAPlane(_players, _planes) => {
            TriggerDefinition::new(TriggerMode::PlaneswalkedTo)
        }
        Trigger::WhenAPlayerPlaneswalksAwayFromAPlane(_players, _planes) => {
            TriggerDefinition::new(TriggerMode::PlaneswalkedFrom)
        }

        // CR 508.3d: "Whenever [a player] attacks" — fires when one or more
        // creatures that player controls are declared as attackers. Engine
        // `TriggerMode::YouAttack` is the (batched) declared-attackers mode
        // for the player axis. Player constraint is dropped — engine has no
        // attacker-controller axis on YouAttack today; the trigger source's
        // controller is implicit ("you" on the printed card). Mirrors native
        // parser at oracle_trigger.rs:3181 / :4257.
        Trigger::WhenAPlayerAttacks(_players) => {
            TriggerDefinition::new(TriggerMode::YouAttack)
        }

        // CR 508.3a: "Whenever [a creature] attacks alone" — same firing axis
        // as a regular attack trigger; the "alone" qualifier (single-attacker
        // batch) has no engine field today. Mirrors native parser at
        // oracle_trigger.rs:8273 (collapses to `TriggerMode::Attacks`).
        Trigger::WhenACreatureAttacksAlone(filter) => {
            TriggerDefinition::new(TriggerMode::Attacks).valid_card(convert_permanents(filter)?)
        }

        // CR 509.1h: "Whenever [a creature] blocks [a creature]" — fires for
        // the blocking creature. Engine `TriggerMode::Blocks` carries
        // `valid_card` for the blocker; the blocked-attacker filter (second
        // arg) is dropped because the engine has no blocked-target axis on
        // Blocks. Mirrors native parser at oracle_trigger.rs:2502-2509.
        Trigger::WhenACreatureBlocksACreature(blocker, _blocked) => {
            TriggerDefinition::new(TriggerMode::Blocks).valid_card(convert_permanents(blocker)?)
        }

        // CR 509.1h + CR 603.2e: "Whenever [a creature] becomes blocked by
        // [a creature]" — fires for the attacker that becomes blocked. Engine
        // `TriggerMode::BecomesBlocked` carries `valid_card` for the attacker;
        // the blocker filter (second arg) is dropped (no blocker axis today).
        Trigger::WhenACreatureBecomesBlockedByACreature(attacker, _blocker) => {
            TriggerDefinition::new(TriggerMode::BecomesBlocked)
                .valid_card(convert_permanents(attacker)?)
        }

        // CR 603.2e: "Whenever [a permanent] becomes the target of a spell or
        // ability" — fires when the targeted permanent becomes a target. The
        // SpellsAndAbilities filter (second arg) is dropped because engine
        // `TriggerMode::BecomesTarget` has no source-spell axis (the native
        // parser only constrains `valid_source` for the "spell only" sub-form
        // at oracle_trigger.rs:2709-2712). Both forms collapse here.
        Trigger::WhenAPermanentBecomesTheTargetOfASpellOrAbility(filter, _sa) => {
            TriggerDefinition::new(TriggerMode::BecomesTarget)
                .valid_card(convert_permanents(filter)?)
        }

        // CR 120.2: "Whenever [a permanent] is dealt damage" — fires when the
        // permanent receives damage. Engine `TriggerMode::DamageReceived`
        // carries `valid_card` for the recipient. Mirrors native parser at
        // oracle_trigger.rs:2719-2722.
        Trigger::WhenAPermanentIsDealtDamage(filter) => {
            TriggerDefinition::new(TriggerMode::DamageReceived)
                .valid_card(convert_permanents(filter)?)
        }

        // CR 120.2a + CR 510.2 + CR 603.10a: "Whenever one or more creatures
        // [filter] deal combat damage to a player" — once-per-batch combat
        // damage trigger. Engine `TriggerMode::DamageDoneOnceByController`
        // captures the once-per-batch semantics directly (no `batched` flag
        // needed). Mirrors native parser at oracle_trigger.rs:3517-3546.
        // The `_players` axis (recipient subset) is dropped — `valid_target`
        // is set to the generic Player filter, matching the native arm.
        Trigger::WhenAnyNumberOfCreaturesDealCombatDamageToAPlayer(creatures, _players) => {
            TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                .valid_source(convert_permanents(creatures)?)
                .valid_target(TargetFilter::Player)
                .damage_kind(DamageKindFilter::CombatOnly)
        }

        // CR 601.2i + CR 603.4: "Whenever [a player] casts their Nth
        // [qualifier] spell each turn" — SpellCast trigger gated by
        // per-caster spell count. The mtgish `Comparison` arg encodes the N;
        // only the literal-EqualTo shape maps to `NthSpellThisTurn { n }`.
        // Other comparators (>=, <=) have no engine counterpart — strict-fail.
        // Mirrors native parser at oracle_trigger.rs:4408-4509.
        Trigger::WhenAPlayerCastsTheirNthSpellInATurn(players, comparison, spells) => {
            let n = nth_count_from_comparison(comparison, "WhenAPlayerCastsTheirNthSpellInATurn")?;
            let controller = players_to_controller(players)?;
            let filter = match &**spells {
                crate::schema::types::Spells::AnySpell => None,
                other => Some(spells_to_filter(other)?),
            };
            let mut def = TriggerDefinition::new(TriggerMode::SpellCast);
            def.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(controller),
            ));
            def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
            def
        }

        // CR 121.2 + CR 603.4: "Whenever [a player] draws their Nth card each
        // turn" — Drawn trigger gated by per-caster draw count. Only the
        // literal-EqualTo `Comparison` shape maps to `NthDrawThisTurn`.
        // Mirrors native parser at oracle_trigger.rs:4655-4715.
        Trigger::WhenAPlayerDrawsTheirNthCardEachTurn(_players, comparison) => {
            let n = nth_count_from_comparison(comparison, "WhenAPlayerDrawsTheirNthCardEachTurn")?;
            let mut def = TriggerDefinition::new(TriggerMode::Drawn);
            def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
            def
        }

        // CR 707.10: "Whenever [a player] copies a spell" — fires when a copy
        // of a spell is put on the stack. Engine `TriggerMode::SpellCopy` is
        // a unit mode; `_players` and `_spells` axes are dropped (no
        // valid_player/valid_target on SpellCopy in the engine today).
        // Mirrors native parser at oracle_trigger.rs:4264.
        Trigger::WhenAPlayerCopiesASpell(_players, _spells) => {
            TriggerDefinition::new(TriggerMode::SpellCopy)
        }

        // CR 505.1a: "At the beginning of [a player's] precombat (first) main
        // phase" — engine `Phase::PreCombatMain` is the first main phase.
        // The `_players` axis is dropped, mirroring the existing phase arms
        // above (e.g. AtTheBeginningOfAPlayersUpkeep).
        Trigger::AtTheBeginningOfAPlayersFirstMainPhase(_players) => {
            TriggerDefinition::new(TriggerMode::Phase).phase(Phase::PreCombatMain)
        }

        // CR ???: Specialize is a Strixhaven Mystical Archive / Lost Caverns
        // mechanic not in CR text — needs manual verification (not in CR
        // text). Engine `TriggerMode::Specializes` is the unit mode used by
        // the native parser for "When ~ specializes" patterns.
        Trigger::WhenACreatureSpecializes(filter) => {
            TriggerDefinition::new(TriggerMode::Specializes)
                .valid_card(convert_permanents(filter)?)
        }

        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: variant_tag(t),
            });
        }
    })
}

fn variant_tag(t: &Trigger) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.get("_Trigger").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// Extract a literal-N count from an `EqualTo(Integer(N))` comparison, used
/// by Nth-spell / Nth-draw trigger constraints (`NthSpellThisTurn`,
/// `NthDrawThisTurn`). Other comparator shapes (>=, <=, etc.) and non-
/// literal `GameNumber` shapes have no engine counterpart — strict-fail
/// with `EnginePrerequisiteMissing` so the report tracks the work queue.
fn nth_count_from_comparison(comparison: &Comparison, idiom: &'static str) -> ConvResult<u32> {
    match comparison {
        Comparison::EqualTo(g) => match &**g {
            GameNumber::Integer(n) => {
                u32::try_from(*n).map_err(|_| ConversionGap::MalformedIdiom {
                    idiom,
                    path: String::new(),
                    detail: format!("non-positive Nth literal: {n}"),
                })
            }
            other => Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerConstraint",
                needed_variant: format!("Nth/non-literal GameNumber: {other:?}"),
            }),
        },
        other => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "TriggerConstraint",
            needed_variant: format!("Nth/non-EQ comparator: {other:?}"),
        }),
    }
}

/// CR 702.29 + CR 603: Build a `Cycled`/`CycledOrDiscarded` trigger with the
/// player axis lowered to `valid_target` (mirrors native parser at
/// `oracle_trigger.rs:4162-4189`).
///
/// - `Players::AnyPlayer` leaves the player slot open (any player triggers);
///   other axes install a `ControllerRef`-scoped filter via `valid_target`.
/// - `cards == CardsInHand::AnyCard` leaves `valid_card` open (any card
///   triggers).
/// - `cards == CardsInHand::SingleCardInHand(ThisCardInHand)` is the
///   self-cycling pattern ("when ~ is cycled, …" — Astral Slide / Lightning
///   Rift family) and lowers to `valid_card: TargetFilter::SelfRef`.
/// - Other card predicates (specific card-type filters) have no engine
///   slot today and strict-fail.
fn cycled_trigger(
    players: &Players,
    cards: &CardsInHand,
    mode: TriggerMode,
    idiom: &'static str,
) -> ConvResult<TriggerDefinition> {
    let mut def = TriggerDefinition::new(mode);
    if !matches!(players, Players::AnyPlayer) {
        let controller = players_to_controller(players)?;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(controller),
        ));
    }
    match cards {
        CardsInHand::AnyCard => {}
        CardsInHand::SingleCardInHand(crate::schema::types::CardInHand::ThisCardInHand) => {
            def.valid_card = Some(TargetFilter::SelfRef);
        }
        other => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerDefinition",
                needed_variant: format!("{idiom} with cycled-card filter: CardsInHand::{other:?}"),
            });
        }
    }
    Ok(def)
}
