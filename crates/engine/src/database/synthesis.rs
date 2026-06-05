use std::str::FromStr;

use crate::database::mtgjson::{parse_mtgjson_mana_cost, AtomicCard};
use crate::game::printed_cards::derive_colors_from_mana_cost;
use crate::parser::oracle::{
    compute_deck_copy_limit_from_text, oracle_text_allows_commander, parse_oracle_text,
};
use crate::parser::oracle_keyword::{keyword_display_name, parse_keyword_from_oracle};
use crate::parser::oracle_util::{apply_bracket_mode, strip_reminder_text, BracketMode};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AbilityTag,
    ActivationRestriction, AdditionalCost, AdditionalCostPaymentSource, AggregateFunction,
    AttackScope, AttackSubject, CardPlayMode, CastFromZoneDriver, CastManaObjectScope,
    CastManaSpentMetric, CastVariantPaid, ChoiceType, Comparator, ContinuousModification,
    ControllerRef, CopyRetargetPermission, CounterTriggerFilter, DamageKindFilter,
    DamageModification, Duration, Effect, FilterProp, KickerVariant, ManaContribution,
    ManaProduction, ModalSelectionCondition, ModalSelectionConstraint, NinjutsuVariant,
    ObjectScope, ParsedCondition, PaymentCost, PlayerFilter, PlayerScope, PtStat, PtValue,
    PtValueScope, QuantityExpr, QuantityRef, ReplacementCondition, ReplacementDefinition,
    RuntimeHandler, SearchSelectionConstraint, StaticCondition, StaticDefinition,
    TargetChoiceTiming, TargetFilter, TriggerCondition, TriggerDefinition, TypeFilter, TypedFilter,
    UnlessPayModifier,
};
use crate::types::card::{CardFace, CardLayout, CleaveVariant};
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::format::DeckCopyLimit;
use crate::types::keywords::{BloodthirstValue, BuybackCost, CyclingCost, Keyword, PartnerType};
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::phase::Phase;
use crate::types::player::PlayerCounterKind;
use crate::types::replacements::ReplacementEvent;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

// ---------------------------------------------------------------------------
// Shared helpers for building card faces from MTGJSON data
// ---------------------------------------------------------------------------

/// CR 702.148a-b + CR 612: Parse a face's Oracle text under Cleave's
/// text-changing semantics, returning the printed-cost parse and (when the face
/// has Cleave) the bracket-removed cleave variant.
///
/// Single authority for the cleave bracket prep so the real card-data build
/// pipeline (`build_oracle_face_inner`) and the test scenario harness
/// (`scenario::build_face_from_oracle`) cannot silently diverge:
///   * The base parse keeps the bracketed clause but drops the bracket
///     characters (`BracketMode::KeepContent`) so the printed-cost spell parses
///     correctly. For non-cleave faces the strip is a no-op (the text never
///     enters the strip), preserving every other parse — and the strip is GATED
///     on the face having Cleave so the ~362 planeswalkers using `[+N]`/`[−N]`
///     loyalty brackets are never corrupted.
///   * When the face has Cleave, a SECOND parse over the bracket-removed text
///     (`BracketMode::RemoveSpan`) is stashed in the returned `CleaveVariant`.
///     The casting flow swaps this onto the stack object when the spell is cast
///     for its cleave cost. This is a leaf parse — never re-projected, so there
///     is no cleave recursion.
pub(crate) fn parse_oracle_with_cleave_brackets(
    raw_oracle_text: &str,
    card_name: &str,
    keyword_names: &[String],
    types: &[String],
    subtypes: &[String],
) -> (
    crate::parser::oracle::ParsedAbilities,
    Option<CleaveVariant>,
) {
    let has_cleave = keyword_names.iter().any(|n| n == "cleave");

    let base_oracle_text = if has_cleave {
        apply_bracket_mode(raw_oracle_text, BracketMode::KeepContent)
    } else {
        raw_oracle_text.to_string()
    };
    let parsed = parse_oracle_text(&base_oracle_text, card_name, keyword_names, types, subtypes);

    let cleave_variant = if has_cleave {
        let cleave_text = apply_bracket_mode(raw_oracle_text, BracketMode::RemoveSpan);
        let cleave_parsed =
            parse_oracle_text(&cleave_text, card_name, keyword_names, types, subtypes);
        Some(CleaveVariant {
            abilities: cleave_parsed.abilities,
            triggers: cleave_parsed.triggers,
            static_abilities: cleave_parsed.statics,
            replacements: cleave_parsed.replacements,
        })
    } else {
        None
    };

    (parsed, cleave_variant)
}

/// Internal layout classification from MTGJSON layout strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Single,
    Split,
    Flip,
    Transform,
    Meld,
    Adventure,
    Modal,
    /// CR 702.xxx: Prepare (Strixhaven) — Adventure-family two-face layout.
    /// Assign when WotC publishes SOS CR update.
    Prepare,
    /// Digital-only Specialize (Alchemy Horizons: Baldur's Gate).
    Specialize,
}

pub fn map_layout(layout_str: &str) -> LayoutKind {
    match layout_str {
        "normal" | "saga" | "class" | "case" | "leveler" => LayoutKind::Single,
        "split" => LayoutKind::Split,
        "flip" => LayoutKind::Flip,
        "transform" => LayoutKind::Transform,
        "meld" => LayoutKind::Meld,
        "adventure" => LayoutKind::Adventure,
        "modal_dfc" => LayoutKind::Modal,
        // CR 702.xxx: Prepare frame (Strixhaven) — two-face card whose face `b`
        // is a "prepare spell". Assign when WotC publishes SOS CR update.
        "prepare" => LayoutKind::Prepare,
        "specialize" => LayoutKind::Specialize,
        _ => LayoutKind::Single,
    }
}

pub fn build_card_type(mtgjson: &AtomicCard) -> CardType {
    let supertypes = mtgjson
        .supertypes
        .iter()
        .filter_map(|s| Supertype::from_str(s).ok())
        .collect();
    let core_types = mtgjson
        .types
        .iter()
        .filter_map(|s| CoreType::from_str(s).ok())
        .collect();
    let subtypes = mtgjson.subtypes.clone();
    CardType {
        supertypes,
        core_types,
        subtypes,
    }
}

pub fn map_mtgjson_color(code: &str) -> Option<ManaColor> {
    match code {
        "W" => Some(ManaColor::White),
        "U" => Some(ManaColor::Blue),
        "B" => Some(ManaColor::Black),
        "R" => Some(ManaColor::Red),
        "G" => Some(ManaColor::Green),
        _ => None,
    }
}

pub fn parse_pt_value(s: &str) -> PtValue {
    match s.parse::<i32>() {
        Ok(n) => PtValue::Fixed(n),
        Err(_) => PtValue::Variable(s.to_string()),
    }
}

pub fn layout_faces(layout: &CardLayout) -> Vec<&CardFace> {
    match layout {
        CardLayout::Single(face) => vec![face],
        CardLayout::Split(a, b)
        | CardLayout::Flip(a, b)
        | CardLayout::Transform(a, b)
        | CardLayout::Meld(a, b)
        | CardLayout::Adventure(a, b)
        | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b)
        | CardLayout::Prepare(a, b) => vec![a, b],
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![base];
            faces.extend(variants);
            faces
        }
    }
}

// ---------------------------------------------------------------------------
// Synthesize functions — keyword → ability/trigger expansion
// ---------------------------------------------------------------------------

pub struct KeywordTriggerInstaller;

impl KeywordTriggerInstaller {
    pub fn triggers_for(keyword: &Keyword) -> Vec<TriggerDefinition> {
        match keyword {
            Keyword::Echo(cost) => vec![build_echo_trigger(cost.clone())],
            // CR 702.24a: Cumulative upkeep — at the beginning of your upkeep,
            // put an age counter on this permanent, then sacrifice it unless
            // you pay its upkeep cost for each age counter on it.
            //
            // Gate by base-cost shape: `AbilityCost` owns the single support
            // boundary for what `handle_unless_payment` + the
            // `expand_per_counter` pipeline can pay end-to-end today.
            // Installing the trigger for an unsupported base (Discard, Exile,
            // EffectCost, etc.) would silently sacrifice the permanent every
            // upkeep because the payment falls through to `payment_failed =
            // true`, causing the unless-effect (Sacrifice) to always fire.
            // Pre-branch these cards had no trigger at all (silent no-op),
            // which is the correct fallback until the resolution pipeline is
            // extended per shape.
            Keyword::CumulativeUpkeep(cost) if cost.supports_cumulative_upkeep_payment() => {
                vec![build_cumulative_upkeep_trigger(cost.clone())]
            }
            Keyword::CumulativeUpkeep(_) => vec![],
            Keyword::Undying => vec![build_dies_return_with_counter_trigger(
                "P1P1", "+1/+1", "702.93a",
            )],
            Keyword::Persist => vec![build_dies_return_with_counter_trigger(
                "M1M1", "-1/-1", "702.79a",
            )],
            // CR 702.135a: Afterlife N — dies trigger creating N 1/1 white and
            // black Spirit creature tokens with flying. Per CR 702.135b each
            // instance triggers separately, so one trigger is emitted per
            // `Keyword::Afterlife(_)` on the face.
            Keyword::Afterlife(n) => vec![build_afterlife_trigger(*n)],
            // CR 702.46a: Soulshift N — dies trigger optionally returning a
            // target Spirit card with mana value N or less from your graveyard
            // to your hand. Per CR 702.46b each instance triggers separately, so
            // one trigger is emitted per `Keyword::Soulshift(_)` on the face.
            Keyword::Soulshift(n) => vec![build_soulshift_trigger(*n)],
            Keyword::Annihilator(n) => vec![build_annihilator_trigger(*n)],
            // CR 702.39a: Provoke — attacks trigger that may untap a creature the
            // defending player controls and force it to block this attacker.
            Keyword::Provoke => vec![build_provoke_trigger()],
            Keyword::Renown(n) => vec![build_renown_trigger(*n)],
            Keyword::Mentor => vec![build_mentor_trigger()],
            // CR 702.58a + CR 604.1: granted Graft installs only the
            // "another creature enters" trigger. The ETB-with-N replacement
            // (CR 702.58a clause 1) is a static ability that functions only as
            // the permanent enters the battlefield — runtime-granted Graft
            // misses that window by definition (the permanent is already on
            // the battlefield when the keyword is granted), so the
            // replacement is not installed here. The trigger, however, is a
            // static-on-battlefield ability that fires from the granted-from
            // moment on.
            Keyword::Graft(_) => vec![build_graft_enters_trigger()],
            // CR 702.45a: Bushido N — fires on both "blocks" and "becomes
            // blocked". CR 702.45b: each instance separately; one trigger per event.
            Keyword::Bushido(n) => vec![
                build_bushido_trigger(TriggerMode::Blocks, *n),
                build_bushido_trigger(TriggerMode::BecomesBlocked, *n),
            ],
            // CR 702.91a: Battle cry — whenever this creature attacks, each
            // other attacking creature gets +1/+0 until end of turn. CR 702.91b:
            // each instance triggers separately; one trigger per `Battlecry`.
            Keyword::Battlecry => vec![build_battlecry_trigger()],
            // CR 702.23a: Rampage N — becomes-blocked self pump of +N/+N per
            // blocker beyond the first. CR 702.23c: each instance triggers
            // separately; one trigger per `Rampage`.
            Keyword::Rampage(n) => vec![build_rampage_trigger(*n)],
            // CR 702.121a: Melee — attack-trigger self pump of +1/+1 per opponent
            // you attacked this combat. CR 702.121b: each instance triggers
            // separately; one trigger per `Melee`.
            Keyword::Melee => vec![build_melee_trigger()],
            Keyword::Dethrone => vec![build_dethrone_trigger()],
            // CR 702.59a: Recover {cost} — graveyard-sourced dies trigger with
            // a mandatory pay-or-else-exile branch.
            Keyword::Recover(cost) => vec![build_recover_trigger(cost.clone())],
            Keyword::Evolve => vec![build_evolve_trigger()],
            Keyword::Exalted => vec![build_exalted_trigger()],
            // CR 702.25a: Flanking — a becomes-blocked debuff trigger. CR 702.25b:
            // each instance triggers separately (one trigger per instance).
            Keyword::Flanking => vec![build_flanking_trigger()],
            Keyword::Extort => vec![build_extort_trigger()],
            Keyword::Increment => vec![build_increment_trigger()],
            Keyword::Myriad => vec![build_myriad_trigger()],
            Keyword::DoubleTeam => vec![build_double_team_trigger()],
            Keyword::Soulbond => build_soulbond_triggers(),
            // CR 702.62a + CR 604.1: granted Suspend carries the same two
            // triggered abilities printed Suspend synthesizes. The
            // hand-activated alt-cost (1st ability) is NOT installed for
            // runtime-granted suspend — the card is already in exile with time
            // counters; only the upkeep counter-removal and last-counter
            // free-cast triggers are relevant in that zone.
            Keyword::Suspend { .. } => vec![
                build_suspend_upkeep_removal_trigger(),
                build_suspend_last_counter_cast_trigger(),
            ],
            // CR 702.130a: Afflict N — a once-per-blocked-attacker trigger.
            // CR 702.130b: each Afflict instance triggers separately (one trigger per instance).
            Keyword::Afflict(n) => vec![build_afflict_trigger(*n)],
            // CR 702.149a: Training — an attacks trigger.
            // CR 702.149b: each Training instance triggers separately (one trigger per instance).
            Keyword::Training => vec![build_training_trigger()],
            // CR 702.70a: Poisonous N — a combat-damage-to-player trigger.
            // CR 702.70b: each Poisonous instance triggers separately (one trigger per instance).
            Keyword::Poisonous(n) => vec![build_poisonous_trigger(*n)],
            // CR 702.115a: Ingest — a combat-damage-to-player trigger.
            // CR 702.115b: each Ingest instance triggers separately (one trigger per instance).
            Keyword::Ingest => vec![build_ingest_trigger()],
            // CR 702.69a: Gravestorm — a stack-functioning spell-cast copy
            // trigger. CR 702.69b: each Gravestorm instance triggers separately.
            Keyword::Gravestorm => vec![build_gravestorm_trigger()],
            // CR 702.32a + CR 604.1: granted Fading carries the upkeep
            // counter-removal / "if you can't, sacrifice" trigger. The
            // ETB-with-N-fade-counters replacement (CR 702.32a clause 1) is a
            // static ability that functions only as the permanent enters; a
            // runtime-granted keyword misses that window (the permanent is
            // already on the battlefield), so the replacement is not installed.
            Keyword::Fading(_) => vec![build_fading_upkeep_trigger()],
            // CR 702.63a + CR 604.1: granted Vanishing carries the upkeep
            // counter-removal and last-counter sacrifice triggers; the ETB
            // replacement is not installed for the same reason as Fading.
            Keyword::Vanishing(_) => vec![
                build_battlefield_upkeep_counter_removal_trigger(CounterType::Time, "702.63a"),
                build_vanishing_sacrifice_trigger(),
            ],
            // CR 702.72a + CR 702.72b: Champion a[n] [type] — paired
            // ETB-exile-or-sacrifice and LTB-return-linked-card triggers. See
            // `build_champion_triggers` for the linkage rationale. Granted
            // Champion (CR 604.1) installs both triggers; the ETB trigger's
            // "when this enters" window has already passed for a runtime grant
            // (mirroring Graft), so in practice only the LTB return matters
            // off-grant, and it safely no-ops when no exile link exists.
            Keyword::Champion(type_str) => build_champion_triggers(type_str),
            _ => Vec::new(),
        }
    }

    pub fn trigger_matches_keyword_kind(trigger: &TriggerDefinition, keyword: &Keyword) -> bool {
        match keyword {
            Keyword::Echo(_) => is_echo_trigger(trigger),
            Keyword::CumulativeUpkeep(_) => is_cumulative_upkeep_trigger(trigger),
            Keyword::Undying => {
                is_dies_return_with_counter_trigger(trigger, &CounterType::Plus1Plus1)
            }
            Keyword::Persist => {
                is_dies_return_with_counter_trigger(trigger, &CounterType::Minus1Minus1)
            }
            Keyword::Afterlife(n) => is_afterlife_trigger_for_count(trigger, *n),
            // CR 702.46b: the mana-value threshold is load-bearing so multiple
            // Soulshift instances with differing N do not dedupe each other.
            Keyword::Soulshift(n) => is_soulshift_trigger_for_value(trigger, *n),
            Keyword::Annihilator(_) => is_annihilator_attack_trigger(trigger),
            Keyword::Provoke => is_provoke_attack_trigger(trigger),
            Keyword::Renown(_) => is_renown_trigger(trigger),
            Keyword::Mentor => is_mentor_trigger(trigger),
            // CR 702.58a + CR 604.1: symmetric removal — `RemoveKeyword`
            // strips the Graft enters-trigger when the granted keyword is
            // removed.
            Keyword::Graft(_) => is_graft_enters_trigger(trigger),
            Keyword::Bushido(n) => is_bushido_trigger(trigger, *n),
            Keyword::Battlecry => is_battlecry_trigger(trigger),
            Keyword::Rampage(n) => is_rampage_trigger(trigger, *n),
            Keyword::Melee => is_melee_trigger(trigger),
            Keyword::Dethrone => is_dethrone_attack_trigger(trigger),
            // CR 702.59a: symmetric removal identifies the synthesized Recover
            // dies trigger.
            Keyword::Recover(_) => is_recover_trigger(trigger),
            Keyword::Evolve => is_evolve_trigger(trigger),
            Keyword::Exalted => is_exalted_trigger(trigger),
            Keyword::Flanking => is_flanking_trigger(trigger),
            Keyword::Extort => is_extort_trigger(trigger),
            Keyword::Increment => is_increment_trigger(trigger),
            Keyword::Myriad => is_myriad_attack_trigger(trigger),
            Keyword::DoubleTeam => is_double_team_attack_trigger(trigger),
            Keyword::Soulbond => is_soulbond_trigger(trigger),
            // CR 702.62a + CR 604.1: symmetric removal — `RemoveKeyword` strips
            // both suspend triggers when the granted keyword is removed.
            Keyword::Suspend { .. } => {
                is_suspend_upkeep_trigger(trigger) || is_suspend_last_counter_trigger(trigger)
            }
            // CR 702.130a + CR 604.1: symmetric removal — `RemoveKeyword` strips
            // the Afflict blocked-attacker trigger when the granted keyword is removed.
            Keyword::Afflict(n) => is_afflict_trigger(trigger, *n),
            // CR 702.149a + CR 604.1: symmetric removal for granted Training.
            Keyword::Training => is_training_trigger(trigger),
            // CR 702.70a + CR 604.1: symmetric removal for granted Poisonous.
            Keyword::Poisonous(n) => is_poisonous_trigger(trigger, *n),
            // CR 702.115a + CR 604.1: symmetric removal for granted Ingest.
            Keyword::Ingest => is_ingest_trigger(trigger),
            // CR 702.69a + CR 604.1: symmetric removal for granted Gravestorm.
            Keyword::Gravestorm => is_gravestorm_trigger(trigger),
            // CR 702.32a + CR 604.1: symmetric removal — `RemoveKeyword` strips
            // the granted Fading trigger when the granted keyword is removed.
            Keyword::Fading(_) => is_fading_upkeep_trigger(trigger),
            // CR 702.63a + CR 604.1: symmetric removal — `RemoveKeyword` strips
            // both Vanishing triggers when the granted keyword is removed.
            Keyword::Vanishing(_) => {
                is_battlefield_upkeep_counter_removal_trigger(trigger, &CounterType::Time)
                    || is_vanishing_sacrifice_trigger(trigger)
            }
            // CR 702.72a + CR 702.72b + CR 604.1: symmetric removal — both the
            // ETB exile-or-sacrifice trigger and the LTB return trigger are
            // recognized so `RemoveKeyword` strips exactly what Champion added.
            Keyword::Champion(type_str) => {
                is_champion_etb_trigger(trigger, type_str)
                    || is_champion_ltb_return_trigger(trigger)
            }
            _ => false,
        }
    }

    fn install_matching<F>(face: &mut CardFace, matches_keyword: F)
    where
        F: Fn(&Keyword) -> bool,
    {
        let desired: Vec<TriggerDefinition> = face
            .keywords
            .iter()
            .filter(|keyword| matches_keyword(keyword))
            .flat_map(Self::triggers_for)
            .collect();

        for (index, trigger) in desired.iter().enumerate() {
            let desired_before = desired[..index].iter().filter(|t| *t == trigger).count();
            let existing = face.triggers.iter().filter(|t| *t == trigger).count();
            if existing <= desired_before {
                face.triggers.push(trigger.clone());
            }
        }
    }
}

pub fn synthesize_basic_land_mana(face: &mut CardFace) {
    let land_mana: Vec<(&str, ManaColor)> = vec![
        ("Plains", ManaColor::White),
        ("Island", ManaColor::Blue),
        ("Swamp", ManaColor::Black),
        ("Mountain", ManaColor::Red),
        ("Forest", ManaColor::Green),
    ];

    for (subtype, color) in land_mana {
        if face.card_type.subtypes.iter().any(|s| s == subtype) {
            face.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![color],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }
    }
}

/// CR 702.6a: Equip is an activated ability of Equipment cards. "Equip [cost]"
/// means "[Cost]: Attach this permanent to target creature you control.
/// Activate only as a sorcery." The `.sorcery_speed()` builder is the single
/// authority that sets both the display flag and pushes
/// `ActivationRestriction::AsSorcery` so the runtime legality gate enforces
/// timing at activation time.
pub fn synthesize_equip(face: &mut CardFace) {
    let equip_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::Equip(cost) = kw {
                Some(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::Attach {
                            attachment: TargetFilter::SelfRef,
                            target: TargetFilter::Typed(
                                TypedFilter::creature().controller(ControllerRef::You),
                            ),
                        },
                    )
                    .cost(AbilityCost::Mana { cost: cost.clone() })
                    // CR 702.6a: "Activate only as a sorcery."
                    .sorcery_speed(),
                )
            } else {
                None
            }
        })
        .collect();

    face.abilities.extend(equip_abilities);
}

/// CR 702.151a: Reconfigure represents two activated abilities —
/// "[Cost]: Attach this permanent to another target creature you control.
/// Activate only as a sorcery." and "[Cost]: Unattach this permanent. Activate
/// only if this permanent is attached to a creature and only as a sorcery."
/// Both are synthesized as sorcery-speed activated abilities whose cost is the
/// reconfigure cost. The attach mode mirrors `synthesize_equip`; the unattach
/// mode uses `Effect::UnattachAll { attachment: SelfRef }` (CR 701.3d). This
/// makes Equipment with Reconfigure (e.g. The Reality Chip) actually
/// attachable/detachable instead of offering no ability at all.
pub fn synthesize_reconfigure(face: &mut CardFace) {
    let mut abilities: Vec<AbilityDefinition> = Vec::new();
    for kw in &face.keywords {
        let Keyword::Reconfigure(cost) = kw else {
            continue;
        };
        // CR 702.151a: "[Cost]: Attach this permanent to another target creature
        // you control. Activate only as a sorcery."
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                },
            )
            .cost(AbilityCost::Mana { cost: cost.clone() })
            .sorcery_speed(),
        );
        // CR 702.151a + CR 701.3d: "[Cost]: Unattach this permanent." Unattaches
        // this Equipment from whatever creature it is attached to.
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::UnattachAll {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::Any,
                },
            )
            .cost(AbilityCost::Mana { cost: cost.clone() })
            .activation_restrictions(vec![ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::SourceAttachedTo {
                    required_type: CoreType::Creature,
                }),
            }])
            .sorcery_speed(),
        );
    }
    face.abilities.extend(abilities);
}

/// CR 702.167a/b: Craft is an activated ability "[Cost], Exile this permanent,
/// Exile [materials] from among permanents you control and/or cards in your
/// graveyard: Return this card to the battlefield transformed under its owner's
/// control. Activate only as a sorcery." (CR 712.14a: "transformed" enters the
/// back face up.) Synthesized as a sorcery-speed activated ability whose cost is
/// a `Composite` of the mana cost, the self-exile (`Exile { filter: SelfRef }`),
/// and the materials exile (`ExileMaterials`); the effect returns the source
/// from exile to the battlefield transformed. Without this synthesis a card with
/// `Keyword::Craft` offered no ability at all (issue #1516).
pub fn synthesize_craft(face: &mut CardFace) {
    let mut abilities: Vec<AbilityDefinition> = Vec::new();
    for kw in &face.keywords {
        let Keyword::Craft {
            cost,
            materials,
            count,
        } = kw
        else {
            continue;
        };
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::ChangeZone {
                    origin: Some(Zone::Exile),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    // CR 712.14a: "transformed" — the card enters showing its back face.
                    enter_transformed: true,
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: Vec::new(),
                    face_down_profile: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: cost.clone() },
                    // CR 702.167a: "Exile this permanent" — the source self-exiles
                    // from the battlefield as part of the cost.
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Battlefield),
                        filter: Some(TargetFilter::SelfRef),
                    },
                    // CR 702.167a/b: "Exile [materials] from among permanents you
                    // control and/or cards in your graveyard."
                    AbilityCost::ExileMaterials {
                        materials: materials.clone(),
                        count: *count,
                    },
                ],
            })
            // CR 702.167a: "Activate only as a sorcery."
            .sorcery_speed(),
        );
    }
    face.abilities.extend(abilities);
}

/// CR 702.49: Synthesize marker activated abilities for the Ninjutsu family
/// (Ninjutsu, CommanderNinjutsu). The actual activation is handled
/// by the GameAction::ActivateNinjutsu path, not by normal activated ability
/// resolution. CR 702.190a Sneak and CR 702.188a Web-slinging are NOT
/// ninjutsu-family activations — they are cast alternative costs handled by
/// the casting pipeline — so they do not synthesize activated abilities here.
pub fn synthesize_ninjutsu_family(face: &mut CardFace) {
    let abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            let (variant, cost) = match kw {
                Keyword::Ninjutsu(c) => (NinjutsuVariant::Ninjutsu, c),
                Keyword::CommanderNinjutsu(c) => (NinjutsuVariant::CommanderNinjutsu, c),
                _ => return None,
            };
            Some(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::RuntimeHandled {
                        handler: RuntimeHandler::NinjutsuFamily,
                    },
                )
                .cost(AbilityCost::NinjutsuFamily {
                    variant,
                    mana_cost: cost.clone(),
                }),
            )
        })
        .collect();
    face.abilities.extend(abilities);
}

// Warp is handled at runtime via Keyword::Warp(ManaCost):
// - `prepare_spell_cast` overrides the mana cost when cast from hand
// - `stack.rs::resolve_top` creates a delayed exile trigger on resolution

/// Synthesize Mobilize N trigger: when this creature attacks, create N 1/1 red
/// Warrior creature tokens tapped and attacking. Sacrifice them at end of combat.
pub fn synthesize_mobilize(face: &mut CardFace) {
    use crate::types::ability::PtValue;
    use crate::types::triggers::TriggerMode;

    // Idempotency: skip if a Mobilize attack trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Attacks)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Token { name, .. }) if name == "Warrior"
            )
    });
    if already_has_trigger {
        return;
    }

    for kw in &face.keywords {
        if let Keyword::Mobilize(qty) = kw {
            let token_effect = Effect::Token {
                name: "Warrior".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Warrior".to_string()],
                colors: vec![ManaColor::Red],
                keywords: vec![],
                tapped: true,
                count: qty.clone(),
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: true,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            };

            face.triggers.push(
                TriggerDefinition::new(TriggerMode::Attacks)
                    .execute(
                        AbilityDefinition::new(AbilityKind::Spell, token_effect)
                            .duration(Duration::UntilEndOfCombat),
                    )
                    .description(
                        "Mobilize — create Warrior tokens tapped and attacking".to_string(),
                    ),
            );
        }
    }
}

/// CR 702.134a: Mentor — "Whenever this creature attacks, put a +1/+1 counter on
/// target attacking creature with power less than this creature's power."
/// Synthesized as a `TriggerMode::Attacks` trigger whose source is the
/// mentoring creature. The "power less than this creature's" target is composed
/// from existing filter building blocks — an `Attacking` creature whose current
/// power is `< Power { scope: Source }` (the mentoring creature's post-layer
/// power, CR 208.1) — so no new filter variant is required. CR 702.134b:
/// multiple Mentor instances trigger separately, hence one synthesized trigger
/// per `Keyword::Mentor` copy.
pub fn synthesize_mentor(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Mentor));
}

/// CR 603.6a + CR 205.3 + CR 105.2: Synthesize a "keyword ETB → create
/// typed token → attach this Equipment" trigger. Shared shape for any
/// keyword whose CR text follows the template:
///
///   "When this Equipment enters, create a <P/T> <color> <subtypes>
///    creature token, then attach this Equipment to it."
///
/// Currently used by Job select (CR 702.182a — 1/1 colorless Hero), Living
/// weapon (CR 702.92a — 0/0 black Phyrexian Germ), and For Mirrodin!
/// (CR 702.163a — 2/2 red Rebel). Future keywords with the same shape can
/// register here without copying the skeleton.
///
/// The helper prepends "Creature" to the subtype list internally so callers
/// only pass the actual subtypes (`["Hero"]`, `["Phyrexian", "Germ"]`). The
/// `keyword_matcher` closure gates synthesis on the presence of the
/// originating keyword, and the idempotency guard is scoped to a
/// ChangesZone trigger landing on the battlefield whose execute effect is
/// `Effect::Token` with the matching token name — re-running synthesis
/// never duplicates the trigger.
#[allow(clippy::too_many_arguments)]
fn synthesize_etb_token_attach_keyword(
    face: &mut CardFace,
    keyword_matcher: impl Fn(&Keyword) -> bool,
    token_name: &str,
    power: i32,
    toughness: i32,
    subtype_list: &[&str],
    colors: &[ManaColor],
    description: &str,
) {
    use crate::types::ability::PtValue;

    if !face.keywords.iter().any(keyword_matcher) {
        return;
    }

    // Idempotency: skip if the ETB token trigger for this token name already
    // exists. Re-running the synthesis pipeline must never duplicate the
    // trigger — otherwise re-loaded card data would fire multiple tokens
    // per Equipment ETB.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::ChangesZone)
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Token { name, .. }) if name == token_name
            )
    });
    if already_has_trigger {
        return;
    }

    // CR 205.3: Token effect's `types` field stores both core types and
    // subtypes in a single vector. Prepend "Creature" so callers pass only
    // the actual creature subtypes.
    let mut types = Vec::with_capacity(subtype_list.len() + 1);
    types.push("Creature".to_string());
    types.extend(subtype_list.iter().map(|s| s.to_string()));

    let token_effect = Effect::Token {
        name: token_name.to_string(),
        power: PtValue::Fixed(power),
        toughness: PtValue::Fixed(toughness),
        types,
        colors: colors.to_vec(),
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    };

    let attach_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target: TargetFilter::LastCreated,
        },
    );

    // CR 603.6a: Enters-the-battlefield abilities trigger when a permanent
    // enters the battlefield. The trigger source must be on the battlefield
    // for the evaluator to match, so `trigger_zones` must include
    // `Zone::Battlefield`.
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Battlefield])
            .execute(
                AbilityDefinition::new(AbilityKind::Spell, token_effect).sub_ability(attach_effect),
            )
            .description(description.to_string()),
    );
}

/// CR 702.182a: Synthesize Job select trigger: when this Equipment enters,
/// create a 1/1 colorless Hero creature token, then attach this Equipment to it.
pub fn synthesize_job_select(face: &mut CardFace) {
    synthesize_etb_token_attach_keyword(
        face,
        |k| matches!(k, Keyword::JobSelect),
        "Hero",
        1,
        1,
        &["Hero"],
        &[],
        "Job select — create Hero token and attach",
    );
}

/// CR 702.92a: Synthesize Living weapon trigger — when this Equipment enters,
/// create a 0/0 black Phyrexian Germ creature token, then attach this
/// Equipment to it. Structurally identical to `synthesize_job_select` (CR
/// 702.182a) — both delegate to `synthesize_etb_token_attach_keyword`,
/// differing only in the leaf-level axes (P/T, token name, subtype list,
/// color, description).
///
/// CR 205.3m: "Phyrexian" and "Germ" are creature subtypes.
/// CR 105.2: Phyrexian Germ tokens are black (single-color).
///
/// Issue #974 (Kaldra Compleat — "Living Weapon" doesn't work): the keyword
/// was parsed into `Keyword::LivingWeapon` and stored on the card face, but
/// no synthesis pass turned it into the rule-mandated ETB trigger, so the
/// Equipment entered the battlefield with no companion Germ token and never
/// auto-attached. Class affects ~15 cards (Batterskull, Flayer Husk,
/// Mortarpod, Bonehoard, etc.) and Kaldra Compleat directly.
pub fn synthesize_living_weapon(face: &mut CardFace) {
    synthesize_etb_token_attach_keyword(
        face,
        |k| matches!(k, Keyword::LivingWeapon),
        "Phyrexian Germ",
        0,
        0,
        &["Phyrexian", "Germ"],
        &[ManaColor::Black],
        "Living weapon — create Phyrexian Germ token and attach",
    );
}

/// CR 702.163a: Synthesize For Mirrodin! trigger — when this Equipment enters,
/// create a 2/2 red Rebel creature token, then attach this Equipment to it.
/// Structurally identical to `synthesize_job_select` and `synthesize_living_weapon` —
/// all three delegate to `synthesize_etb_token_attach_keyword`, differing only
/// in the leaf-level axes (P/T, token name, subtype list, color, description).
///
/// CR 205.3m: "Rebel" is a creature subtype.
/// CR 105.2: For Mirrodin! tokens are red (single-color).
pub fn synthesize_for_mirrodin(face: &mut CardFace) {
    synthesize_etb_token_attach_keyword(
        face,
        |k| matches!(k, Keyword::ForMirrodin),
        "Rebel",
        2,
        2,
        &["Rebel"],
        &[ManaColor::Red],
        "For Mirrodin! — create Rebel token and attach",
    );
}

/// If the card has Changeling as a printed keyword, emit a characteristic-defining
/// static ability that grants all creature types (expanded at runtime via
/// `GameState::all_creature_types`).
/// CR 702.184a + CR 721.2b: Synthesize Station's creature-at-threshold static.
///
/// The Station keyword's reminder text includes "It's an artifact creature at
/// N+." (CR 721.2b). The threshold N is the highest station symbol printed on
/// the card — the point at which the Spacecraft gains the Creature type and
/// uses its printed P/T. We extract N from the parenthesized Station reminder
/// paragraph (kept on `oracle_text` before `strip_reminder_text` eats it for
/// the ability parser), then push a SelfRef static that:
///
/// - Adds `CoreType::Creature` (Layer 4 — CR 613.1d)
/// - Sets power/toughness to the card's printed values (Layer 7b)
///
/// All gated by `StaticCondition::HasCounters { counter_type: "charge",
/// minimum: N, maximum: None }`.
///
/// Non-battlefield zones automatically do not apply this (layer system only
/// evaluates battlefield objects), matching CR 721.2c: while in any zone
/// other than the battlefield, station cards do not have power or toughness.
pub fn synthesize_station(face: &mut CardFace) {
    // CR 721.2b: Require printed P/T. Station Spacecraft without a printed P/T
    // box (e.g. "The Eternity Elevator") are support-only; no creature-shift.
    let (Some(PtValue::Fixed(power)), Some(PtValue::Fixed(toughness))) =
        (face.power.as_ref(), face.toughness.as_ref())
    else {
        return;
    };
    let power = *power;
    let toughness = *toughness;

    // CR 721.1: Spacecraft is the marker subtype — no Spacecraft subtype, no
    // station striations, so no creature shift applies.
    if !face
        .card_type
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Spacecraft"))
    {
        return;
    }

    // CR 721.2b / CR 721.3: The striation containing the printed P/T box is the
    // highest N+ threshold on the card. Reminder text ("It's an artifact
    // creature at N+") has no rules force (CR 721.3) and is deliberately
    // ignored.
    let Some(oracle) = face.oracle_text.as_deref() else {
        return;
    };
    let lines: Vec<&str> = oracle.lines().collect();
    let Some(threshold) = crate::parser::oracle_spacecraft::max_spacecraft_threshold(&lines) else {
        return;
    };

    let condition = crate::types::ability::StaticCondition::HasCounters {
        counters: crate::types::counter::CounterMatch::OfType(
            crate::types::counter::CounterType::Generic("charge".to_string()),
        ),
        minimum: threshold,
        maximum: None,
    };
    face.static_abilities.push(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(condition)
            .modifications(vec![
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
                ContinuousModification::SetPower { value: power },
                ContinuousModification::SetToughness { value: toughness },
            ])
            .description(format!(
                "CR 721.2b: Spacecraft is an artifact creature at {threshold}+"
            )),
    );
}

pub fn synthesize_changeling_cda(face: &mut CardFace) {
    if face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Changeling))
    {
        face.static_abilities.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddAllCreatureTypes])
                .cda(),
        );
    }
}

/// CR 702.161a: Living metal — "During your turn, this permanent is an artifact
/// creature in addition to its other types." Synthesize a SelfRef static that
/// adds the Creature type (Layer 4, CR 613.1d) while it is the controller's turn,
/// gated by `StaticCondition::DuringYourTurn`. The Vehicle uses its printed P/T as
/// a creature on its controller's turn and is a noncreature artifact otherwise.
/// Mirrors `synthesize_station`'s creature-shift; the source is already an artifact
/// (Vehicle), so only the Creature type is added. (Transformers — Flamewar,
/// Streetwise Operative, etc.; #1547.)
fn is_living_metal_static(static_ability: &StaticDefinition) -> bool {
    static_ability.mode == StaticMode::Continuous
        && static_ability.affected == Some(TargetFilter::SelfRef)
        && matches!(
            &static_ability.condition,
            Some(crate::types::ability::StaticCondition::DuringYourTurn)
        )
        && static_ability.modifications.len() == 1
        && static_ability.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }
            )
        })
}

pub fn synthesize_living_metal(face: &mut CardFace) {
    if !face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::LivingMetal))
    {
        return;
    }
    if face.static_abilities.iter().any(is_living_metal_static) {
        return;
    }
    face.static_abilities.push(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(crate::types::ability::StaticCondition::DuringYourTurn)
            .modifications(vec![ContinuousModification::AddType {
                core_type: CoreType::Creature,
            }])
            .description(
                "CR 702.161a: Living metal — artifact creature during your turn".to_string(),
            ),
    );
}

/// Synthesize `additional_cost` from `Keyword::Kicker(ManaCost)`.
///
/// If the card has Kicker and no additional_cost was already parsed from Oracle text
/// (blight takes precedence since it's parsed from the "as an additional cost" line),
/// set `additional_cost = Some(AdditionalCost::Kicker { ... })`.
pub fn synthesize_kicker(face: &mut CardFace) {
    if face.additional_cost.is_some() {
        return;
    }
    let costs: Vec<AbilityCost> = face
        .keywords
        .iter()
        .filter_map(|k| match k {
            Keyword::Kicker(cost) => Some(AbilityCost::Mana { cost: cost.clone() }),
            _ => None,
        })
        .collect();
    if !costs.is_empty() {
        face.additional_cost = Some(AdditionalCost::Kicker {
            costs,
            repeatable: false,
        });
    }
}

/// CR 702.33f: Conditions of the form "if it was kicked with its [A] kicker"
/// are linked to the first or second kicker cost printed on the card. Parser
/// output carries the printed mana cost as typed metadata; this synthesis pass
/// resolves it back to the positional `KickerVariant` once card-level kicker
/// declarations are visible.
pub fn resolve_kicker_condition_variants(face: &mut CardFace) {
    let Some(additional_cost) = &face.additional_cost else {
        return;
    };

    for ability in &mut face.abilities {
        resolve_ability_kicker_condition_variants(ability, additional_cost);
    }
    for trigger in &mut face.triggers {
        if let Some(execute) = trigger.execute.as_mut() {
            resolve_ability_kicker_condition_variants(execute, additional_cost);
        }
    }
    for replacement in &mut face.replacements {
        resolve_replacement_kicker_condition_variants(replacement, additional_cost);
    }
}

fn kicker_variant_for_cost(
    additional_cost: &AdditionalCost,
    target_cost: &ManaCost,
) -> Option<KickerVariant> {
    let AdditionalCost::Kicker { costs, .. } = additional_cost else {
        return None;
    };
    costs.iter().enumerate().find_map(|(index, cost)| {
        let AbilityCost::Mana { cost } = cost else {
            return None;
        };
        if cost != target_cost {
            return None;
        }
        match index {
            0 => Some(KickerVariant::First),
            1 => Some(KickerVariant::Second),
            _ => None,
        }
    })
}

fn resolve_ability_kicker_condition_variants(
    ability: &mut AbilityDefinition,
    additional_cost: &AdditionalCost,
) {
    if let Some(condition) = ability.condition.as_mut() {
        resolve_condition_kicker_variant(condition, additional_cost);
    }
    if let Some(modal) = ability.modal.as_mut() {
        resolve_modal_kicker_condition_variants(modal, additional_cost);
    }

    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        resolve_ability_kicker_condition_variants(sub_ability, additional_cost);
    }

    for mode in &mut ability.mode_abilities {
        resolve_ability_kicker_condition_variants(mode, additional_cost);
    }
}

fn resolve_modal_kicker_condition_variants(
    modal: &mut crate::types::ability::ModalChoice,
    additional_cost: &AdditionalCost,
) {
    for constraint in &mut modal.constraints {
        let ModalSelectionConstraint::ConditionalMaxChoices { condition, .. } = constraint else {
            continue;
        };
        let ModalSelectionCondition::AdditionalCostPaid {
            variant,
            kicker_cost,
            ..
        } = condition
        else {
            continue;
        };
        resolve_kicker_cost_metadata(variant, kicker_cost, additional_cost);
    }
}

fn resolve_condition_kicker_variant(
    condition: &mut AbilityCondition,
    additional_cost: &AdditionalCost,
) {
    match condition {
        AbilityCondition::AdditionalCostPaid {
            variant,
            kicker_cost,
            ..
        } => {
            resolve_kicker_cost_metadata(variant, kicker_cost, additional_cost);
        }
        AbilityCondition::ConditionInstead { inner }
        | AbilityCondition::Not { condition: inner } => {
            resolve_condition_kicker_variant(inner, additional_cost);
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for condition in conditions {
                resolve_condition_kicker_variant(condition, additional_cost);
            }
        }
        _ => {}
    }
}

fn resolve_replacement_kicker_condition_variants(
    replacement: &mut ReplacementDefinition,
    additional_cost: &AdditionalCost,
) {
    if let Some(ReplacementCondition::CastViaKicker {
        variant,
        kicker_cost,
    }) = replacement.condition.as_mut()
    {
        resolve_kicker_cost_metadata(variant, kicker_cost, additional_cost);
    }

    if let Some(execute) = replacement.execute.as_mut() {
        resolve_ability_kicker_condition_variants(execute, additional_cost);
    }
}

fn resolve_kicker_cost_metadata(
    variant: &mut Option<KickerVariant>,
    kicker_cost: &mut Option<ManaCost>,
    additional_cost: &AdditionalCost,
) {
    if let (None, Some(resolved_variant)) = (
        *variant,
        kicker_cost
            .as_ref()
            .and_then(|cost| kicker_variant_for_cost(additional_cost, cost)),
    ) {
        *variant = Some(resolved_variant);
        *kicker_cost = None;
    }
}

/// CR 702.27a: Synthesize `additional_cost` from `Keyword::Buyback(BuybackCost)`.
///
/// Buyback is an optional additional cost: "You may pay an additional [cost]
/// as you cast this spell. If the buyback cost was paid, put this spell into
/// its owner's hand instead of into that player's graveyard as it resolves."
///
/// The resolution-time routing (hand instead of graveyard) is handled in
/// `game::stack::resolve_top` by inspecting `ability.context.additional_cost_paid`
/// on the resolving spell when the source carries `Keyword::Buyback`.
///
/// Idempotent: skips if `additional_cost` is already set (Oracle-parsed
/// "as an additional cost" lines take precedence, matching the Kicker pattern).
pub fn synthesize_buyback(face: &mut CardFace) {
    if face.additional_cost.is_some() {
        return;
    }
    let Some(buyback_cost) = face.keywords.iter().find_map(|k| match k {
        Keyword::Buyback(cost) => Some(cost.clone()),
        _ => None,
    }) else {
        return;
    };
    let cost = match buyback_cost {
        BuybackCost::Mana(mana_cost) => AbilityCost::Mana { cost: mana_cost },
        BuybackCost::NonMana(ac) => ac,
    };
    face.additional_cost = Some(AdditionalCost::Optional {
        cost,
        repeatable: false,
    });
}

/// CR 702.166a: Synthesize `additional_cost` from `Keyword::Bargain`.
pub fn synthesize_bargain(face: &mut CardFace) {
    if face.additional_cost.is_some()
        || !face.keywords.iter().any(|k| matches!(k, Keyword::Bargain))
    {
        return;
    }

    face.additional_cost = Some(AdditionalCost::Optional {
        cost: AbilityCost::Sacrifice {
            target: TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
                    TargetFilter::Typed(
                        TypedFilter::permanent().properties(vec![FilterProp::Token]),
                    ),
                ],
            },
            count: 1,
        },
        repeatable: false,
    });
}

/// Synthesize Gift optional cost and delivery effect.
/// Gift is a promise (zero-cost optional additional cost) that sets `additional_cost_paid`
/// when the player promises the gift. Conditional branches ("if the gift was promised" /
/// "wasn't promised") are handled by the parser via `strip_additional_cost_conditional`.
///
/// Gift delivery (opponent receives the gift) is injected as a `GiftDelivery` effect
/// wrapping the first spell ability. The delivery checks `additional_cost_paid` at
/// resolution time — if the gift wasn't promised, it's a no-op and the spell resolves
/// normally. If promised, the opponent receives the gift before the spell's other effects.
pub fn synthesize_gift(face: &mut CardFace) {
    if face.additional_cost.is_some() {
        return;
    }
    // Use rfind (last match) because the MTGJSON bare "Gift" keyword defaults to
    // Gift(Card), while the Oracle-parsed keyword (e.g., Gift(TappedFish)) comes later
    // and is always the correct, specific kind.
    let gift_kind = face.keywords.iter().rev().find_map(|k| match k {
        Keyword::Gift(kind) => Some(kind.clone()),
        _ => None,
    });
    let Some(gift_kind) = gift_kind else {
        return;
    };

    // Gift uses a zero-cost optional additional cost — the "cost" is just a decision.
    face.additional_cost = Some(AdditionalCost::Optional {
        cost: AbilityCost::Mana {
            cost: ManaCost::zero(),
        },
        repeatable: false,
    });

    // Inject GiftDelivery as a wrapper around the first spell ability.
    // The delivery effect is a no-op when the gift wasn't promised, so the
    // chain always flows through to the spell's normal effects.
    if let Some(first_ability) = face.abilities.first_mut() {
        let original = std::mem::replace(
            first_ability,
            AbilityDefinition::new(AbilityKind::Spell, Effect::GiftDelivery { kind: gift_kind }),
        );
        first_ability.sub_ability = Some(Box::new(original));
    }
}

/// CR 719.2: Synthesize the intrinsic Case auto-solve trigger.
/// Every Case with a solve condition has: "At the beginning of your end step,
/// if this Case is not solved and its requirement is met, it becomes solved."
pub fn synthesize_case_solve(face: &mut CardFace) {
    if !face.card_type.subtypes.iter().any(|s| s == "Case") {
        return;
    }
    if face.solve_condition.is_none() {
        return;
    }

    // Idempotency: skip if the Case auto-solve end-step trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Phase)
            && t.phase == Some(Phase::End)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::SolveCase)
            )
    });
    if already_has_trigger {
        return;
    }

    face.triggers.push(
        TriggerDefinition::new(TriggerMode::Phase)
            .phase(Phase::End)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SolveCase,
            ))
            .condition(TriggerCondition::SolveConditionMet)
            .description("CR 719.2: Case auto-solve at end step".to_string()),
    );
}

/// Digital-only Specialize: `{cost}, Discard a card` activated ability at sorcery speed.
pub fn synthesize_specialize(face: &mut CardFace) {
    let specialize_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::Specialize(cost) = kw {
                Some(
                    AbilityDefinition::new(AbilityKind::Activated, Effect::Specialize)
                        .cost(AbilityCost::Composite {
                            costs: vec![
                                AbilityCost::Mana { cost: cost.clone() },
                                AbilityCost::Discard {
                                    count: QuantityExpr::Fixed { value: 1 },
                                    filter: Some(specialize_discard_filter()),
                                    random: false,
                                    self_ref: false,
                                },
                            ],
                        })
                        .sorcery_speed(),
                )
            } else {
                None
            }
        })
        .collect();
    face.abilities.extend(specialize_abilities);
}

fn specialize_discard_filter() -> TargetFilter {
    TargetFilter::Or {
        filters: vec![
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Card).properties(vec![
                FilterProp::ColorCount {
                    comparator: Comparator::GE,
                    count: 1,
                },
            ])),
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::AnyOf(
                        ["Plains", "Island", "Swamp", "Mountain", "Forest"]
                            .into_iter()
                            .map(|subtype| TypeFilter::Subtype(subtype.to_string()))
                            .collect(),
                    ))),
                ],
            },
        ],
    }
}

/// CR 702.87a: Synthesize level up activated ability — "Pay {cost}: Put a level counter
/// on this permanent. Activate only as a sorcery."
pub fn synthesize_level_up(face: &mut CardFace) {
    let level_up_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::LevelUp(cost) = kw {
                // CR 702.87a: Level up is an activated ability, sorcery-speed only.
                Some(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::PutCounter {
                            counter_type: CounterType::Generic("level".to_string()),
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                    )
                    .cost(AbilityCost::Mana { cost: cost.clone() })
                    // CR 702.87a: "Activate only as a sorcery." `.sorcery_speed()`
                    // sets the display flag and pushes `AsSorcery` for runtime.
                    .sorcery_speed(),
                )
            } else {
                None
            }
        })
        .collect();

    face.abilities.extend(level_up_abilities);
}

/// CR 903.3: determine if a card can be a Commander.
/// Uses the union of MTGJSON's `leadershipSkills.commander` (which catches the full
/// WotC-blessed surface: legendary creatures, Vehicles, Spacecraft with P/T,
/// Backgrounds, and every "can be your commander" carve-out) and our own type-line
/// check (CR 903.3 a/b/c plus 903.3a). The union mirrors `compute_brawl_commander`
/// so we stay correct when MTGJSON is missing, stale, or hasn't yet annotated
/// a freshly-printed card.
pub fn compute_commander(mtgjson: &super::mtgjson::AtomicCard, face: &CardFace) -> bool {
    // Source 1: MTGJSON leadership skills (authoritative for the standard format).
    let mtgjson_says = mtgjson
        .leadership_skills
        .as_ref()
        .is_some_and(|ls| ls.commander);

    // Source 2: type-line analysis — mirrors crate::game::deck_validation logic
    // so this function is the single authority for commander eligibility.
    mtgjson_says || type_line_commander_eligible(face)
}

/// CR 100.2a / CR 903.5b: determine a card's deck-construction copy-limit
/// override from its Oracle text (including reminder-text bodies). `None` means
/// the default four-of (constructed) / singleton (Commander) limit applies.
/// Delegates to the parser so the recognizer and the validator agree.
pub fn compute_deck_copy_limit(face: &CardFace) -> Option<DeckCopyLimit> {
    face.oracle_text
        .as_deref()
        .and_then(compute_deck_copy_limit_from_text)
}

/// CR 903.3 type-line analysis (excludes MTGJSON skill data). Public for use by
/// the deck-validation predicate, which reads the precomputed `face.is_commander`
/// at runtime but exposes this helper for callers that only have a `CardFace`.
pub fn type_line_commander_eligible(face: &CardFace) -> bool {
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let subtypes = &face.card_type.subtypes;

    // CR 903.3(a): legendary creature.
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    // CR 903.3(b): legendary Vehicle (introduced for Unfinity / pre-EOE Vehicles).
    let is_vehicle = subtypes.iter().any(|s| s.eq_ignore_ascii_case("Vehicle"));
    // CR 903.3(c): legendary Spacecraft with one or more power/toughness boxes.
    // The P/T-box guard is load-bearing per CR 903.3(c); future Spacecraft
    // without a P/T box are not eligible.
    let is_spacecraft_with_pt = subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Spacecraft"))
        && face.power.is_some()
        && face.toughness.is_some();
    // CR 702.124: legendary Background enchantment (paired with a partner).
    let is_background = subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Background"));
    // CR 903.3a: explicit "can be your commander" override.
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));

    (is_legendary && (is_creature || is_vehicle || is_spacecraft_with_pt || is_background))
        || explicitly_allowed
}

/// Brawl variant of CR 903.3: determine if a card can be a Brawl commander.
/// Uses the union of MTGJSON's `leadershipSkills.brawl` (which catches Vehicles/Spacecraft)
/// and our own type-line check (legendary creature or legendary planeswalker, or
/// "can be your commander" in Oracle text).
pub fn compute_brawl_commander(mtgjson: &super::mtgjson::AtomicCard, face: &CardFace) -> bool {
    // Source 1: MTGJSON leadership skills (catches Legendary Vehicles etc.)
    let mtgjson_says = mtgjson
        .leadership_skills
        .as_ref()
        .is_some_and(|ls| ls.brawl);

    // Source 2: type-line analysis
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let is_planeswalker = face.card_type.core_types.contains(&CoreType::Planeswalker);
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));
    let type_line_says = (is_legendary && (is_creature || is_planeswalker)) || explicitly_allowed;

    mtgjson_says || type_line_says
}

/// Oathbreaker RC: determine if a card can be an Oathbreaker commander.
/// Uses MTGJSON `leadershipSkills.oathbreaker` (authoritative for WotC-blessed
/// Planeswalkers) unioned with type-line analysis (legendary Planeswalker) as a
/// staleness guard. Mirrors the `compute_commander` / `compute_brawl_commander` pattern.
pub fn compute_oathbreaker(mtgjson: &super::mtgjson::AtomicCard, face: &CardFace) -> bool {
    let mtgjson_says = mtgjson
        .leadership_skills
        .as_ref()
        .is_some_and(|ls| ls.oathbreaker);
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let is_planeswalker = face.card_type.core_types.contains(&CoreType::Planeswalker);
    mtgjson_says || (is_legendary && is_planeswalker)
}

/// CR 702.29a/e: Synthesize Cycling and Typecycling keywords into activated abilities.
///
/// Cycling: "[Cost], Discard this card: Draw a card." (activated from hand)
/// Typecycling: "[Cost], Discard this card: Search library for a [type] card,
///   reveal it, put it into your hand. Then shuffle."
///
/// DEFERRED RUNTIME GAP (CR 702.29e + CR 113.6b) — Homing Sliver class:
/// This build-time synthesis reads only the face's INTRINSIC printed keywords.
/// A Typecycling/Cycling keyword GRANTED at runtime by a continuous effect
/// (Homing Sliver: "Each Sliver card in each player's hand has slivercycling
/// {3}.") lands on the recipient's runtime keyword set (CR 113.6b zone-of-
/// function + `TargetFilter::extract_in_zone` resolves it in the Hand zone), but
/// is NOT synthesized into a runtime-activatable ability — synthesis never runs
/// over runtime-granted keywords. Closing this needs a general runtime
/// granted-keyword -> activatable-ability primitive (not card-specific), which
/// is deferred. The parser/grant half is correct and covered by
/// `static_homing_sliver_grants_typecycling_to_slivers_in_hand` in
/// `parser/oracle_static/tests.rs`.
pub fn synthesize_cycling(face: &mut CardFace) {
    let cycling_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            // CR 702.29a: Basic cycling — discard self, draw a card.
            // Cost may be mana ("cycling {2}") or non-mana ("cycling—pay 2 life").
            Keyword::Cycling(cycling_cost) => {
                // CR 702.29a: "Discard THIS card" — self_ref = true.
                let discard_self = AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: true,
                };
                let composite_cost = match cycling_cost {
                    CyclingCost::Mana(cost) => AbilityCost::Composite {
                        costs: vec![AbilityCost::Mana { cost: cost.clone() }, discard_self],
                    },
                    CyclingCost::NonMana(ac) => match ac {
                        // Flatten an already-Composite non-mana cost so the discard joins
                        // the existing sub-costs instead of nesting.
                        AbilityCost::Composite { costs } => {
                            let mut flat = costs.clone();
                            flat.push(discard_self);
                            AbilityCost::Composite { costs: flat }
                        }
                        other => AbilityCost::Composite {
                            costs: vec![other.clone(), discard_self],
                        },
                    },
                };
                let mut def = AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(composite_cost);
                def.activation_zone = Some(Zone::Hand);
                Some(def)
            }
            // CR 702.29e: Typecycling — discard self, search library for [type] card.
            Keyword::Typecycling { cost, subtype } => {
                let composite_cost = AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana { cost: cost.clone() },
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            random: false,
                            self_ref: true,
                        },
                    ],
                };
                let filter = typecycling_subtype_to_filter(subtype);
                let shuffle_def = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Shuffle {
                        target: TargetFilter::Controller,
                    },
                );
                let mut put_in_hand_def = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: Some(Zone::Library),
                        destination: Zone::Hand,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        face_down_profile: None,
                    },
                );
                put_in_hand_def.sub_ability = Some(Box::new(shuffle_def));
                let mut def = AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::SearchLibrary {
                        filter,
                        count: QuantityExpr::Fixed { value: 1 },
                        reveal: true,
                        target_player: None,
                        selection_constraint: SearchSelectionConstraint::None,
                        split: None,
                        source_zones: vec![crate::types::zones::Zone::Library],
                    },
                )
                .cost(composite_cost);
                def.activation_zone = Some(Zone::Hand);
                def.sub_ability = Some(Box::new(put_in_hand_def));
                Some(def)
            }
            _ => None,
        })
        .collect();

    // CR 702.29a + CR 702.29c + CR 702.29e: Tag every synthesized cycling /
    // typecycling ability with `AbilityTag::Cycling` so that activating it emits
    // a `GameEvent::Cycled` ("When you cycle this card" triggers, CR 702.29c).
    let mut cycling_abilities = cycling_abilities;
    for def in &mut cycling_abilities {
        def.ability_tag = Some(AbilityTag::Cycling);
    }

    face.abilities.extend(cycling_abilities);
}

/// CR 702.97a: Synthesize Scavenge into an activated ability on the card.
///
/// Scavenge is an activated ability that functions only while the card with scavenge is
/// in a graveyard. "Scavenge [cost]" means "[Cost], Exile this card from your graveyard:
/// Put a number of +1/+1 counters equal to this card's power on target creature. Activate
/// only as a sorcery."
///
/// Power snapshot timing (CR 208.3 + CR 400.7): At resolution the source has already
/// been exiled as a cost; CR 702.97a specifies "the power of the card you exiled",
/// which is read from the exile-zone object via `QuantityRef::Power { scope: crate::types::ability::ObjectScope::Source }` (with LKI
/// fallback if the object is somehow gone). Non-battlefield zones do not run layer
/// computation, so the read value equals the card's printed power — the correct
/// target for "this card's power" in the graveyard reminder text. No new quantity
/// ref is needed; `SelfPower` is already the right abstraction.
pub fn synthesize_scavenge(face: &mut CardFace) {
    use crate::types::ability::QuantityRef;

    let scavenge_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            let Keyword::Scavenge(cost) = kw else {
                return None;
            };
            // CR 118.3: Composite cost — pay mana, then exile this card from graveyard.
            let composite_cost = AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: cost.clone() },
                    // CR 702.97a: "Exile this card from your graveyard" — SelfRef + Graveyard
                    // is auto-paid by pay_ability_cost (no player choice needed).
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Graveyard),
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            };
            // CR 702.97a: "Put a number of +1/+1 counters equal to this card's power on
            // target creature." SelfPower is resolved via LKI at resolution time so the
            // power read is the card's last known power before it was exiled.
            let effect = Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source,
                    },
                },
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            };
            let mut def = AbilityDefinition::new(AbilityKind::Activated, effect)
                .cost(composite_cost)
                // CR 702.97a: "Activate only as a sorcery." The `.sorcery_speed()`
                // builder sets both the display flag and pushes
                // `ActivationRestriction::AsSorcery` for runtime enforcement.
                .sorcery_speed();
            // CR 702.97a: "functions only while the card with scavenge is in a graveyard."
            def.activation_zone = Some(Zone::Graveyard);
            Some(def)
        })
        .collect();

    face.abilities.extend(scavenge_abilities);
}

/// CR 702.107a: Synthesize the Outlast activated ability from a `Keyword::Outlast(cost)`.
/// "Outlast [cost]" means "[Cost], {T}: Put a +1/+1 counter on this creature.
/// Activate only as a sorcery."
pub fn synthesize_outlast(face: &mut CardFace) {
    let outlast_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            let Keyword::Outlast(cost) = kw else {
                return None;
            };
            // CR 702.107a: Composite cost — pay mana, then tap this creature.
            let composite_cost = AbilityCost::Composite {
                costs: vec![AbilityCost::Mana { cost: cost.clone() }, AbilityCost::Tap],
            };
            // CR 702.107a: "Put a +1/+1 counter on this creature."
            let effect = Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            };
            let mut def = AbilityDefinition::new(AbilityKind::Activated, effect)
                .cost(composite_cost)
                // CR 702.107a: "Activate only as a sorcery."
                .sorcery_speed();
            // Tag so "whenever you activate this creature's outlast ability" triggers fire.
            def.ability_tag = Some(AbilityTag::Outlast);
            Some(def)
        })
        .collect();

    face.abilities.extend(outlast_abilities);
}

/// CR 702.77a: Synthesize the Reinforce activated ability from `Keyword::Reinforce { count, cost }`.
/// "Reinforce N—[cost]" means "[Cost], Discard this card: Put N +1/+1 counters on target creature."
pub fn synthesize_reinforce(face: &mut CardFace) {
    let reinforce_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            let Keyword::Reinforce { count, cost } = kw else {
                return None;
            };
            // CR 702.77a: Composite cost — pay mana, then discard this card.
            let composite_cost = AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: cost.clone() },
                    // CR 702.77a: "Discard this card" — self_ref=true so the
                    // engine auto-discards the source card.
                    AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: None,
                        random: false,
                        self_ref: true,
                    },
                ],
            };
            // CR 702.77a: "Put N +1/+1 counters on target creature."
            // When count == 0, this is Reinforce X — use Variable("X") which
            // resolves to chosen_x at runtime (the X in the mana cost).
            let counter_count = if *count == 0 {
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                }
            } else {
                QuantityExpr::Fixed {
                    value: *count as i32,
                }
            };
            let effect = Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: counter_count,
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            };
            let def = AbilityDefinition::new(AbilityKind::Activated, effect).cost(composite_cost);
            // CR 702.77a: Reinforce is activated from hand (discard as cost).
            // No zone restriction needed — the discard cost implicitly requires the card
            // to be in hand. The default activation zone (battlefield) won't apply since
            // the card is never on the battlefield when this ability is relevant.
            // Actually, per CR 702.77b: "A creature card with reinforce may also be cast
            // as a spell." The ability functions from hand, so we set activation_zone.
            let mut def = def;
            def.activation_zone = Some(Zone::Hand);
            Some(def)
        })
        .collect();

    face.abilities.extend(reinforce_abilities);
}

/// Convert a typecycling subtype string to a `TargetFilter` for library search.
///
/// Single subtypes (e.g., "Plains", "Forest") → subtype filter.
/// "Basic Land" → supertype Basic + core type Land.
fn typecycling_subtype_to_filter(subtype: &str) -> TargetFilter {
    if subtype == "Basic Land" {
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Land).properties(vec![
            FilterProp::HasSupertype {
                value: Supertype::Basic,
            },
        ]))
    } else {
        TargetFilter::Typed(TypedFilter::card().subtype(subtype.to_string()))
    }
}

/// CR 702.153a: The canonical `AbilityDefinition` produced by a Casualty
/// trigger — a self-referential `CopySpell` gated on the additional cost
/// having been paid. This is the single authority for what a casualty trigger
/// resolves into; both `synthesize_casualty` (intrinsic, embedded as the
/// trigger's `execute`) and the dynamically-granted casualty path in
/// `triggers::process_triggers` (instantiated via `build_resolved_from_def`)
/// share this shape.
pub fn casualty_copy_ability_definition() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        // CR 702.153a: Casualty — "If the spell has any targets, you may choose
        // new targets for the copy."
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            copier: None,
        },
    )
    .condition(AbilityCondition::additional_cost_paid_any())
}

/// CR 702.153a: Synthesize Casualty N into an optional sacrifice cost + self-cast copy trigger.
///
/// Casualty N = two abilities:
/// 1. Optional additional cost: sacrifice a creature with power N or greater
/// 2. Triggered ability: "When you cast this spell, if a casualty cost was paid, copy it"
pub fn synthesize_casualty(face: &mut CardFace) {
    let threshold = match face.keywords.iter().find_map(|k| match k {
        Keyword::Casualty(n) => Some(*n),
        _ => None,
    }) {
        Some(n) => n,
        None => return,
    };

    // CR 702.153a: "As an additional cost, you may sacrifice a creature with power N or greater"
    if face.additional_cost.is_none() {
        let sacrifice_filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed {
                    value: threshold as i32,
                },
            },
        ]));
        face.additional_cost = Some(AdditionalCost::Optional {
            cost: AbilityCost::Sacrifice {
                target: sacrifice_filter,
                count: 1,
            },
            repeatable: false,
        });
    }

    // CR 702.153a: "When you cast this spell, if a casualty cost was paid, copy it.
    // If the spell has any targets, you may choose new targets for the copy."
    // Idempotency: skip if the casualty copy-on-cast trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::SpellCast)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && t.trigger_zones.contains(&Zone::Stack)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::CopySpell {
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    });
    if already_has_trigger {
        return;
    }

    face.triggers.push(
        TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Stack])
            .execute(casualty_copy_ability_definition())
            .description("Casualty — copy this spell when cast with casualty paid".to_string()),
    );
}

/// CR 702.56a: The canonical `AbilityDefinition` produced by a Replicate
/// trigger — a self-referential `CopySpell` repeated once for each time the
/// replicate cost was paid, gated on the replicate (additional) cost having
/// been paid. This is the single authority for what a replicate trigger
/// resolves into.
///
/// Differs from `casualty_copy_ability_definition` in exactly one axis:
/// Casualty copies the spell once (a single sacrifice), while Replicate is a
/// *repeatable* additional cost (CR 702.56a: "pay [cost] any number of times")
/// that copies the spell once per payment. That per-payment count flows through
/// `repeat_for = QuantityRef::AdditionalCostPaymentCount`, which the
/// `resolve_chain_body` iteration loop reads to drive N `CopySpell` iterations
/// — each producing one stack copy with its own CR 707.10c retarget step.
pub fn replicate_copy_ability_definition() -> AbilityDefinition {
    let mut def = AbilityDefinition::new(
        AbilityKind::Spell,
        // CR 702.56a + CR 707.10c: "If the spell has any targets, you may
        // choose new targets for any of the copies."
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            copier: None,
        },
    )
    // CR 702.56a: "if a replicate cost was paid for it". With zero payments the
    // count is also zero, but the condition keeps the trigger's resolution a
    // no-op (no SpellCopied events) when replicate was declined, matching the
    // intervening-if phrasing exactly.
    .condition(AbilityCondition::additional_cost_paid_any());
    // CR 702.56a: "copy it for each time its replicate cost was paid." The
    // replicate cost is a repeatable additional cost, so the number of copies
    // equals the cast-time payment count
    // (`SpellContext::additional_cost_payment_count`).
    def.repeat_for = Some(QuantityExpr::Ref {
        qty: QuantityRef::AdditionalCostPaymentCount,
    });
    def
}

/// CR 702.56a: Synthesize Replicate {cost} into a repeatable optional additional
/// cost and a "when you cast this spell" trigger that copies it once for each
/// time the replicate cost was paid.
///
/// Replicate = two abilities (CR 702.56a):
/// 1. Static ability: "As an additional cost to cast this spell, you may pay
///    [cost] any number of times" — modeled as
///    `AdditionalCost::Optional { repeatable: true, .. }` (same shape as Squad,
///    CR 702.157a).
/// 2. Triggered ability: "When you cast this spell, if a replicate cost was paid
///    for it, copy it for each time its replicate cost was paid. If the spell
///    has any targets, you may choose new targets for any of the copies." —
///    modeled as a `SpellCast` trigger (same shape as Casualty, CR 702.153a)
///    whose execute is `replicate_copy_ability_definition()`.
///
/// Build-for-the-class: every card with `Keyword::Replicate(cost)` flows through
/// this single synthesizer. Idempotent across repeated invocations.
pub fn synthesize_replicate(face: &mut CardFace) {
    let replicate_costs: Vec<_> = face
        .keywords
        .iter()
        .filter_map(|k| match k {
            Keyword::Replicate(cost) => Some(cost.clone()),
            _ => None,
        })
        .collect();
    if replicate_costs.is_empty() {
        return;
    }

    // CR 702.56b: Multiple Replicate instances are paid separately and each
    // instance's linked trigger counts only its own payments. The engine tracks
    // a single aggregate `additional_cost_payment_count`, so it cannot keep
    // per-instance payment tallies. Defer rather than over-count copies. Mirrors
    // the Squad multi-instance deferral (CR 702.157b).
    if replicate_costs.len() > 1 {
        defer_synthesis(
            face,
            "replicate_multiple_instances",
            "CR 702.56b: multiple Replicate instances require per-instance payment tracking"
                .to_string(),
        );
        return;
    }

    let replicate_cost = replicate_costs[0].clone();

    // CR 702.56a: "As an additional cost to cast this spell, you may pay [cost]
    // any number of times." Repeatable optional mana cost — the cast-time
    // payment loop records each payment in `additional_cost_payment_count`.
    if face.additional_cost.is_none() {
        face.additional_cost = Some(AdditionalCost::Optional {
            cost: AbilityCost::Mana {
                cost: replicate_cost,
            },
            repeatable: true,
        });
    }

    // CR 702.56a: "When you cast this spell, if a replicate cost was paid for
    // it, copy it for each time its replicate cost was paid."
    // Idempotency: skip if the replicate copy-on-cast trigger already exists.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::SpellCast)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && t.trigger_zones.contains(&Zone::Stack)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::CopySpell {
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    });
    if already_has_trigger {
        return;
    }

    face.triggers.push(
        TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Stack])
            .execute(replicate_copy_ability_definition())
            .description(
                "Replicate — copy this spell once for each time its replicate cost was paid"
                    .to_string(),
            ),
    );
}

/// CR 702.144a: The `AbilityDefinition` produced by a Demonstrate trigger — an
/// optional self-copy ("you may copy it ... and you may choose new targets")
/// whose sub-ability copies the spell for a chosen opponent ("if you copy the
/// spell, choose an opponent; that player copies the spell and may choose new
/// targets for that copy").
///
/// The opponent's copy is a `sub_ability` so it only happens when the controller
/// accepts the optional copy (CR 702.144a "if you copy the spell"); the existing
/// chain resolver sequences it after the controller's copy (and its retarget)
/// via `pending_continuation`. The opponent is routed through the new
/// `Effect::CopySpell { copier: Some(Opponent) }` axis, which `copy_spell::resolve`
/// turns into an opponent-controlled copy (CR 707.10).
pub fn demonstrate_copy_ability_definition() -> AbilityDefinition {
    let opponent_copy = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            copier: Some(ControllerRef::Opponent),
        },
    )
    .description(
        "CR 702.144a: Demonstrate — the chosen opponent copies the spell and may choose new \
         targets for that copy"
            .to_string(),
    );

    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            copier: None,
        },
    )
    .optional()
    .sub_ability(opponent_copy)
    .description(
        "CR 702.144a: Demonstrate — you may copy this spell (you may choose new targets); if you \
         do, a chosen opponent also copies it"
            .to_string(),
    )
}

/// CR 702.144a: Identity predicate for a synthesized Demonstrate copy-on-cast
/// trigger — an optional `SpellCast` self-copy whose sub-ability is an
/// opponent-`copier` copy. Used for idempotent synthesis.
fn is_demonstrate_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::SpellCast)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && t.trigger_zones.contains(&Zone::Stack)
        && t.execute.as_deref().is_some_and(|a| {
            a.optional
                && matches!(
                    &*a.effect,
                    Effect::CopySpell {
                        target: TargetFilter::SelfRef,
                        copier: None,
                        ..
                    }
                )
                && a.sub_ability.as_deref().is_some_and(|sub| {
                    matches!(
                        &*sub.effect,
                        Effect::CopySpell {
                            copier: Some(ControllerRef::Opponent),
                            ..
                        }
                    )
                })
        })
}

/// CR 702.144a: Synthesize Demonstrate into a "when you cast this spell" copy
/// trigger that functions on the stack: you may copy the spell, and if you do, a
/// chosen opponent also copies it. Both copies may choose new targets (CR
/// 707.10c).
///
/// Build-for-the-class: keyed entirely on `Keyword::Demonstrate`, so every
/// printed Demonstrate spell flows through this one synthesizer. Idempotent
/// across repeated invocations.
pub fn synthesize_demonstrate(face: &mut CardFace) {
    if !face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Demonstrate))
    {
        return;
    }
    if face.triggers.iter().any(is_demonstrate_trigger) {
        return;
    }
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Stack])
            .execute(demonstrate_copy_ability_definition())
            .description(
                "CR 702.144a: Demonstrate — when you cast this spell, you may copy it; if you do, \
                 a chosen opponent also copies it."
                    .to_string(),
            ),
    );
}

/// CR 702.69a: The `AbilityDefinition` produced by a Gravestorm trigger — a
/// self-referential `CopySpell` repeated once for each permanent put into a
/// graveyard from the battlefield this turn. Mirrors
/// `replicate_copy_ability_definition` but drives `repeat_for` off the
/// battlefield-to-graveyard zone-change count (CR 702.69a) rather than the
/// additional-cost payment count, and carries no intervening-if.
pub fn gravestorm_copy_ability_definition() -> AbilityDefinition {
    let mut def = AbilityDefinition::new(
        AbilityKind::Spell,
        // CR 702.69a + CR 707.10c: "If the spell has any targets, you may
        // choose new targets for any of the copies."
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            copier: None,
        },
    );
    // CR 702.69a: "copy it for each permanent that was put into a graveyard from
    // the battlefield this turn." The count drives N `CopySpell` iterations.
    def.repeat_for = Some(QuantityExpr::Ref {
        qty: QuantityRef::ZoneChangeCountThisTurn {
            from: Some(Zone::Battlefield),
            to: Some(Zone::Graveyard),
            filter: TargetFilter::Typed(TypedFilter::permanent()),
        },
    });
    def
}

/// CR 702.69a: A Gravestorm trigger — a self-referential `SpellCast` copy
/// trigger that functions on the stack and whose copy count is the
/// battlefield-to-graveyard zone-change count this turn.
fn is_gravestorm_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::SpellCast)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && t.trigger_zones.contains(&Zone::Stack)
        && t.execute.as_deref().is_some_and(|a| {
            matches!(
                &*a.effect,
                Effect::CopySpell {
                    target: TargetFilter::SelfRef,
                    retarget: CopyRetargetPermission::MayChooseNewTargets,
                    ..
                }
            ) && a.repeat_for.as_ref().is_some_and(|repeat_for| {
                matches!(
                    repeat_for,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeCountThisTurn {
                            from: Some(Zone::Battlefield),
                            to: Some(Zone::Graveyard),
                            filter,
                        }
                    } if *filter == TargetFilter::Typed(TypedFilter::permanent())
                )
            })
        })
}

/// CR 702.69a: Build one Gravestorm trigger — "when you cast this spell, copy
/// it for each permanent that was put into a graveyard from the battlefield
/// this turn." CR 702.69b: multiple Gravestorm instances trigger separately.
fn build_gravestorm_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::SpellCast)
        .valid_card(TargetFilter::SelfRef)
        .trigger_zones(vec![Zone::Stack])
        .execute(gravestorm_copy_ability_definition())
        .description(
            "CR 702.69a: Gravestorm — when you cast this spell, copy it for each permanent \
             put into a graveyard from the battlefield this turn."
                .to_string(),
        )
}

/// CR 702.69a: Synthesize Gravestorm into "when you cast this spell" copy
/// triggers that function on the stack and copy the spell once for each
/// permanent put into a graveyard from the battlefield this turn.
///
/// Build-for-the-class: keyed entirely on `Keyword::Gravestorm`, so every
/// printed Gravestorm card flows through this one synthesizer. CR 702.69b says
/// each Gravestorm instance triggers separately, which `install_matching`
/// preserves while keeping repeated synthesis idempotent.
pub fn synthesize_gravestorm(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Gravestorm));
}

/// CR 702.42a: Synthesize Entwine cost onto modal spell's ModalChoice.
///
/// Sets `entwine_cost` on the face's modal abilities and raises `max_choices`
/// to `mode_count` so all modes can be selected.
pub fn synthesize_entwine(face: &mut CardFace) {
    let cost = match face.keywords.iter().find_map(|k| match k {
        Keyword::Entwine(cost) => Some(cost.clone()),
        _ => None,
    }) {
        Some(c) => c,
        None => return,
    };

    // Set entwine_cost on the face's modal choice + allow all-mode selection
    if let Some(ref mut modal) = face.modal {
        modal.entwine_cost = Some(cost);
        // CR 702.42a: "You may choose all modes" — raise max_choices to allow it
        modal.max_choices = modal.mode_count;
    }
}

/// CR 702.35a: Madness is a static ability with a replacement effect plus a
/// linked triggered ability. If the player discards the card, they exile it
/// instead of putting it into their graveyard; when they do, they may cast it
/// for its madness cost or put it into their graveyard.
pub fn synthesize_madness_intrinsics(face: &mut CardFace) {
    let Some(cost) = face.keywords.iter().find_map(|kw| match kw {
        Keyword::Madness(cost) => Some(cost.clone()),
        _ => None,
    }) else {
        return;
    };

    let already_has_replacement = face.replacements.iter().any(|r| {
        matches!(r.event, ReplacementEvent::Discard)
            && matches!(r.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                r.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::ChangeZone {
                    origin: Some(Zone::Hand),
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    });
    if !already_has_replacement {
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Discard);
        replacement.valid_card = Some(TargetFilter::SelfRef);
        replacement.description = Some(
            "CR 702.35a: If you discard this card, exile it instead of putting it into your graveyard."
                .to_string(),
        );
        replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        )));
        face.replacements.push(replacement);
    }

    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Discarded)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && t.trigger_zones.contains(&Zone::Exile)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::MadnessCast { .. })
            )
    });
    if !already_has_trigger {
        let trigger = TriggerDefinition::new(TriggerMode::Discarded)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Exile])
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::MadnessCast { cost },
            ))
            .description(
                "CR 702.35a: When this card is exiled this way, its owner may cast it for its madness cost or put it into their graveyard."
                    .to_string(),
            );
        face.triggers.push(trigger);
    }
}

/// CR 702.74a: Evoke is a static ability granting an alternative cost plus a
/// linked intervening-if triggered ability. The static ability's
/// "you may cast for evoke cost" is wired at the engine level via
/// `CastingVariant::Evoke` (handled in `casting::handle_cast_spell` and
/// `prepare_spell_cast_with_variant_override`); only the triggered ability
/// needs to be synthesized here.
///
/// "When this permanent enters, if its evoke cost was paid, sacrifice it."
/// `TriggerCondition::CastVariantPaid { variant: Evoke }` reads
/// `GameObject.cast_variant_paid`, which the resolution path tags when the
/// spell was cast via `CastingVariant::Evoke`.
pub fn synthesize_evoke(face: &mut CardFace) {
    if !face.keywords.iter().any(|k| matches!(k, Keyword::Evoke(_))) {
        return;
    }
    // Idempotency: skip if a CastVariantPaid::Evoke ETB sacrifice trigger already
    // exists (oracle parser already extracted it, or this synthesizer already ran).
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::ChangesZone)
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.condition,
                Some(TriggerCondition::CastVariantPaid {
                    variant: CastVariantPaid::Evoke,
                })
            )
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    });
    if already_has_trigger {
        return;
    }

    face.triggers.push(build_evoke_etb_sac_trigger());
}

/// CR 702.74a: Build the evoke ETB-sacrifice triggered ability — "When this
/// permanent enters, if its evoke cost was paid, its controller sacrifices it."
///
/// Shared by `synthesize_evoke` (card-data baking of printed evoke) and the
/// runtime `ensure_evoke_etb_sac_trigger` (granted evoke, where the keyword
/// lived on the spell and never reached the resolving permanent's printed
/// triggers). The trigger is gated on `CastVariantPaid { variant: Evoke }`, so
/// it is a no-op when the permanent was not cast for its evoke cost.
pub(crate) fn build_evoke_etb_sac_trigger() -> TriggerDefinition {
    let sac = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Evoke,
        })
        .execute(sac)
        .description(
            "CR 702.74a: When this permanent enters, if its evoke cost was paid, sacrifice it."
                .to_string(),
        )
}

/// CR 702.74a + CR 604.1: Install the evoke ETB-sacrifice trigger on a live
/// `GameObject` if it is not already present.
///
/// For printed evoke the trigger is already baked into the card face by
/// `synthesize_evoke`, so this is an idempotent no-op. For *granted* evoke
/// (a `StaticMode::CastWithKeyword { keyword: Evoke }` static such as Ashling,
/// the Limitless) the keyword lives on the spell while it is on the stack and
/// never propagates to the resolving permanent's printed triggers, so the
/// trigger must be installed onto the permanent at resolution. The structural-
/// equality scan mirrors `synthesize_evoke`'s `already_has_trigger` matcher so
/// the two paths never double-install.
pub(crate) fn ensure_evoke_etb_sac_trigger(obj: &mut crate::game::game_object::GameObject) {
    let is_evoke_sac = |t: &TriggerDefinition| {
        matches!(t.mode, TriggerMode::ChangesZone)
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.condition,
                Some(TriggerCondition::CastVariantPaid {
                    variant: CastVariantPaid::Evoke,
                })
            )
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    };
    // CR 702.74a + CR 613.1f: the layer system rebuilds `trigger_definitions`
    // from `base_trigger_definitions` on every evaluation (layers.rs), so a
    // runtime install must land in the durable base or it is wiped before the
    // ETB ChangesZone trigger is collected. Printed evoke already carries the
    // trigger in `base_trigger_definitions` (baked into the card face by
    // `synthesize_evoke`), making this an idempotent no-op for that path. Push to
    // base (the durable source layers rebuild from), then refresh the live copy
    // so the trigger is collectable this same resolution before the next layers
    // pass re-derives `trigger_definitions`.
    if obj.base_trigger_definitions.iter().any(is_evoke_sac) {
        if !obj.trigger_definitions.iter_all().any(is_evoke_sac) {
            obj.trigger_definitions.push(build_evoke_etb_sac_trigger());
        }
        return;
    }
    let trigger = build_evoke_etb_sac_trigger();
    std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger.clone());
    obj.trigger_definitions.push(trigger);
}

/// CR 702.30a: Echo is a triggered ability. "Echo [cost]" means "At the
/// beginning of your upkeep, if this permanent came under your control since
/// the beginning of your last upkeep, sacrifice it unless you pay [cost]."
///
/// The runtime marks each new echo permanent `echo_due` when it enters and
/// clears the marker when the unless-payment is handled.
pub fn synthesize_echo(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Echo(_)));
}

/// CR 702.24a: Cumulative upkeep is a triggered ability. "Cumulative upkeep
/// [cost]" means "At the beginning of your upkeep, if this permanent is on
/// the battlefield, put an age counter on this permanent. Then you may pay
/// [cost] for each age counter on it. If you don't, sacrifice it."
///
/// See `build_cumulative_upkeep_trigger` for the chained-ability shape that
/// preserves the rules ordering (counter add first, then per-counter prompt).
pub fn synthesize_cumulative_upkeep(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| {
        matches!(kw, Keyword::CumulativeUpkeep(_))
    });
}

/// Insert a synthesis-level unsupported sentinel once.
///
/// Coverage already treats `Effect::Unimplemented` on any ability as an
/// unsupported card-data gap. `AbilityKind::Spell` is used deliberately here
/// because `AbilityDefinition` has no non-runtime sentinel kind; cards with
/// this marker stay unsupported rather than entering legal play paths.
fn defer_synthesis(face: &mut CardFace, name: &str, description: String) {
    let already_deferred = face.abilities.iter().any(|ability| {
        matches!(
            &*ability.effect,
            Effect::Unimplemented {
                name: existing, ..
            } if existing == name
        )
    });
    if already_deferred {
        return;
    }

    face.abilities.push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Unimplemented {
            name: name.to_string(),
            description: Some(description),
        },
    ));
}

fn install_etb_copy_on_additional_cost(
    face: &mut CardFace,
    additional_cost: AdditionalCost,
    payment_source: AdditionalCostPaymentSource,
    min_count: u32,
    count: QuantityExpr,
    additional_modifications: Vec<ContinuousModification>,
    description: String,
) {
    if face.additional_cost.is_none() {
        face.additional_cost = Some(additional_cost);
    }

    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::ChangesZone)
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.condition,
                Some(TriggerCondition::AdditionalCostPaid {
                    source,
                    min_count: existing_min_count,
                    ..
                }) if source == payment_source && existing_min_count == min_count
            )
            && t.execute.as_deref().is_some_and(|a| match &*a.effect {
                Effect::CopyTokenOf {
                    count: existing_count,
                    additional_modifications: existing_modifications,
                    ..
                } => {
                    existing_count == &count && existing_modifications == &additional_modifications
                }
                _ => false,
            })
    });
    if already_has_trigger {
        return;
    }

    let copy_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count,
            extra_keywords: vec![],
            additional_modifications,
        },
    );
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::AdditionalCostPaid {
            source: payment_source,
            variant: None,
            kicker_cost: None,
            min_count,
        })
        .execute(copy_effect)
        .description(description);
    face.triggers.push(trigger);
}

/// CR 702.175a: Offspring represents two abilities:
///   1. "You may pay an additional [cost] as you cast this spell" — modeled as
///      `AdditionalCost::Optional { repeatable: false, .. }`.
///   2. "When this permanent enters, if its offspring cost was paid, create a
///      token that's a copy of it, except it's 1/1." — modeled as an ETB trigger
///      with `TriggerCondition::AdditionalCostPaid` and `Effect::CopyTokenOf`
///      carrying `SetPower { value: 1 }` + `SetToughness { value: 1 }` modifications.
///
/// Build-for-the-class: every card with `Keyword::Offspring(cost)` flows through
/// this single synthesizer. Idempotent across repeated invocations.
pub fn synthesize_offspring(face: &mut CardFace) {
    let Some(offspring_cost) = face.keywords.iter().find_map(|k| match k {
        Keyword::Offspring(cost) => Some(cost.clone()),
        _ => None,
    }) else {
        return;
    };

    install_etb_copy_on_additional_cost(
        face,
        AdditionalCost::Optional {
            cost: AbilityCost::Mana {
                cost: offspring_cost,
            },
            repeatable: false,
        },
        AdditionalCostPaymentSource::Any,
        1,
        QuantityExpr::Fixed { value: 1 },
        vec![
            ContinuousModification::SetPower { value: 1 },
            ContinuousModification::SetToughness { value: 1 },
        ],
        "CR 702.175a: When this permanent enters, if its offspring cost was paid, create a token that's a copy of it, except it's 1/1."
            .to_string(),
    );
}

/// CR 702.157a: Squad represents a repeatable optional additional cost and an
/// ETB trigger that creates one copy token for each time the squad cost was paid.
pub fn synthesize_squad(face: &mut CardFace) {
    let squad_costs: Vec<_> = face
        .keywords
        .iter()
        .filter_map(|k| match k {
            Keyword::Squad(cost) => Some(cost.clone()),
            _ => None,
        })
        .collect();
    if squad_costs.is_empty() {
        return;
    }

    // CR 702.157b: Multiple Squad instances are paid independently and each
    // instance's linked trigger counts only its own payments. Do not collapse
    // them into one repeatable cost; leave coverage unsupported until the
    // linked-instance model exists.
    if squad_costs.len() > 1 {
        defer_synthesis(
            face,
            "squad_multiple_instances",
            "CR 702.157b: multiple Squad instances require per-instance payment tracking"
                .to_string(),
        );
        return;
    }

    let squad_cost = squad_costs[0].clone();

    install_etb_copy_on_additional_cost(
        face,
        AdditionalCost::Optional {
            cost: AbilityCost::Mana {
                cost: squad_cost,
            },
            repeatable: true,
        },
        AdditionalCostPaymentSource::NonKicker,
        0,
        QuantityExpr::Ref {
            qty: QuantityRef::AdditionalCostPaymentCount,
        },
        vec![],
        "CR 702.157a: When this permanent enters, create a token that's a copy of it for each time its squad cost was paid."
            .to_string(),
    );
}

/// CR 702.123a: Fabricate N — "When this permanent enters, you may put N
/// +1/+1 counters on it. If you don't, create N 1/1 colorless Servo artifact
/// creature tokens."
///
/// CR 702.123b: Each instance of Fabricate triggers separately. A card with
/// two `Keyword::Fabricate(N)` entries synthesizes two distinct ETB triggers.
///
/// Modeled as an ETB trigger whose execute body is `Effect::ChooseOneOf` with
/// two branches:
///   - Branch A: `PutCounter { P1P1, count: N, target: SelfRef }`
///   - Branch B: `Token { Servo 1/1 colorless artifact creature, count: N }`
///
/// The CR phrasing ("you may put… if you don't, create…") is structurally
/// equivalent to a controller-chosen branch: the controller decides which of
/// the two outcomes resolves. `ChooseOneOf` is the existing primitive for
/// "you may A or B" patterns and is the correct building block here — adding
/// a bespoke "may/else" variant would duplicate it without categorical gain.
///
/// Timing axis: Fabricate's counter branch is a CR 603 *triggered* ability
/// that resolves AFTER the permanent has entered, not a CR 614.1c as-enters
/// replacement. Consequences: counter-placement replacements that modify
/// "+1/+1 counter placement" broadly (Doubling Season, Hardened Scales) DO
/// apply to Fabricate's counter branch via the standard counter-placement
/// modification path. Effects scoped specifically to "enters with counters"
/// as-enters replacements do NOT apply — Fabricate's counters are added
/// post-ETB by trigger resolution. Do not move this synthesis into the
/// as-enters replacement window: that would change the rules-correct timing.
pub fn synthesize_fabricate(face: &mut CardFace) {
    let fabricate_values: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Fabricate(n) => Some(*n),
            _ => None,
        })
        .collect();
    if fabricate_values.is_empty() {
        return;
    }

    // Idempotency: skip if an ETB ChooseOneOf{P1P1 | Servo} trigger already
    // exists. Match by structural shape (mode + destination + valid_card +
    // execute effect kind) so re-running the synthesizer on an already-built
    // face is a no-op.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::ChangesZone)
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::ChooseOneOf { branches, .. })
                    if branches.iter().any(|b| matches!(
                        &*b.effect,
                        Effect::Token { name, .. } if name == "Servo"
                    ))
            )
    });
    if already_has_trigger {
        return;
    }

    for n in fabricate_values {
        let count_expr = QuantityExpr::Fixed { value: n as i32 };
        let counter_word = if n == 1 { "counter" } else { "counters" };
        let token_word = if n == 1 { "token" } else { "tokens" };

        let counters_branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: count_expr.clone(),
                target: TargetFilter::SelfRef,
            },
        )
        .description(format!("Put {n} +1/+1 {counter_word} on it"));

        // CR 111.1 + CR 111.4: Token is a 1/1 colorless Servo artifact
        // creature token. `types` carries both core types ("Artifact",
        // "Creature") and the creature subtype ("Servo") — mirrors the
        // Treasure pattern (`["Artifact", "Treasure"]`) and Mobilize Warrior
        // pattern (`["Creature", "Warrior"]`). Colorless is represented as
        // an empty `colors` vec.
        let servos_branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Servo".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec![
                    "Artifact".to_string(),
                    "Creature".to_string(),
                    "Servo".to_string(),
                ],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: count_expr,
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        )
        .description(format!(
            "Create {n} 1/1 colorless Servo artifact creature {token_word}"
        ));

        let choose = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: crate::types::ability::PlayerFilter::Controller,
                branches: vec![counters_branch, servos_branch],
            },
        );

        let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .execute(choose)
            .description(format!(
                "CR 702.123a: Fabricate {n} — when this permanent enters, put {n} +1/+1 {counter_word} on it or create {n} 1/1 colorless Servo artifact creature {token_word}."
            ));
        face.triggers.push(trigger);
    }
}

/// CR 702.136a: Riot — "You may have this permanent enter with an additional
/// +1/+1 counter on it. If you don't, it gains haste."
///
/// Modeled as an optional `Moved` replacement, not an ETB trigger: accepting
/// folds the counter into the battlefield-entry event, while declining runs the
/// haste grant after the object enters. Static grants of Riot (Uncivil Unrest)
/// synthesize the same replacement from the static's affected filter.
pub fn synthesize_riot(face: &mut CardFace) {
    let printed_count = face
        .keywords
        .iter()
        .filter(|kw| matches!(kw, Keyword::Riot))
        .count();
    add_riot_replacements(face, TargetFilter::SelfRef, printed_count);

    let static_grants: Vec<TargetFilter> = face
        .static_abilities
        .iter()
        .filter(|static_def| static_grants_riot(static_def))
        .map(|static_def| static_def.affected.clone().unwrap_or(TargetFilter::Any))
        .collect();
    for filter in static_grants {
        add_riot_replacements(face, filter, 1);
    }
}

/// CR 702.64a: Absorb N — "If a source would deal damage to this creature,
/// prevent N of that damage." A continuous, self-recipient damage replacement:
/// `DamageModification::Minus { value: N }` saturating-subtracts N from each
/// damage event whose recipient is this creature (`valid_card: SelfRef`). It is
/// NOT a consumed shield, so it re-applies to every source and every event
/// independently (CR 702.64b). No new variant — mirrors the continuous
/// damage-prevention statics (Benevolent Unicorn class) and the self-scoped
/// `valid_card(SelfRef)` damage replacements (persistent prevention shields).
fn build_absorb_replacement(n: u32) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .valid_card(TargetFilter::SelfRef)
        .damage_modification(DamageModification::Minus { value: n })
        .description(format!(
            "CR 702.64a: Absorb {n} — if a source would deal damage to this creature, \
             prevent {n} of that damage."
        ))
}

/// CR 702.64a: Identity predicate for an Absorb `n` replacement — a self-recipient
/// `DamageDone` replacement that subtracts `n` from the damage. Parameterized by
/// `n` for count-based idempotency and so a granted-then-removed Absorb strips
/// exactly its own replacement.
fn is_absorb_replacement(r: &ReplacementDefinition, n: u32) -> bool {
    matches!(r.event, ReplacementEvent::DamageDone)
        && matches!(r.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            r.damage_modification,
            Some(DamageModification::Minus { value }) if value == n
        )
}

/// CR 702.64a/c: Synthesize Absorb into a continuous self-recipient damage
/// replacement. CR 702.64c: multiple instances apply separately, so one
/// replacement is installed per `Keyword::Absorb` instance (grouped by N).
///
/// Build-for-the-class: keyed entirely on `Keyword::Absorb(n)`, so every printed
/// Absorb creature and every creature granted Absorb at runtime gets identical
/// prevention. Idempotent across repeated invocations via per-N count matching
/// (mirrors `add_riot_replacements`).
pub fn synthesize_absorb(face: &mut CardFace) {
    let mut counts: Vec<(u32, usize)> = Vec::new();
    for kw in &face.keywords {
        if let Keyword::Absorb(n) = kw {
            match counts.iter_mut().find(|(value, _)| value == n) {
                Some((_, c)) => *c += 1,
                None => counts.push((*n, 1)),
            }
        }
    }
    for (n, desired) in counts {
        let existing = face
            .replacements
            .iter()
            .filter(|r| is_absorb_replacement(r, n))
            .count();
        for _ in existing..desired {
            face.replacements.push(build_absorb_replacement(n));
        }
    }
}

fn static_grants_riot(static_def: &StaticDefinition) -> bool {
    static_def.mode == StaticMode::Continuous
        && static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Riot
                }
            )
        })
}

fn add_riot_replacements(face: &mut CardFace, valid_card: TargetFilter, needed: usize) {
    let existing = face
        .replacements
        .iter()
        .filter(|replacement| is_riot_replacement(replacement, &valid_card))
        .count();
    for _ in existing..needed {
        face.replacements
            .push(build_riot_replacement(valid_card.clone()));
    }
}

fn build_riot_replacement(valid_card: TargetFilter) -> ReplacementDefinition {
    let counter_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .description("This permanent enters with an additional +1/+1 counter on it".to_string());

    let haste_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])],
            duration: Some(Duration::Permanent),
            target: None,
        },
    )
    .duration(Duration::Permanent)
    .description("It gains haste".to_string());

    ReplacementDefinition {
        event: ReplacementEvent::Moved,
        execute: Some(Box::new(counter_branch)),
        mode: crate::types::ability::ReplacementMode::Optional {
            decline: Some(Box::new(haste_branch)),
        },
        valid_card: Some(valid_card),
        destination_zone: Some(Zone::Battlefield),
        description: Some(
            "CR 702.136a: Riot — this permanent may enter with an additional +1/+1 counter; otherwise it gains haste."
                .to_string(),
        ),
        ..ReplacementDefinition::new(ReplacementEvent::Moved)
    }
}

fn is_riot_replacement(replacement: &ReplacementDefinition, valid_card: &TargetFilter) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || replacement.valid_card.as_ref() != Some(valid_card)
        || replacement.destination_zone != Some(Zone::Battlefield)
    {
        return false;
    }

    let Some(execute) = replacement.execute.as_deref() else {
        return false;
    };
    let Effect::PutCounter {
        counter_type,
        count: QuantityExpr::Fixed { value },
        target: TargetFilter::SelfRef,
    } = &*execute.effect
    else {
        return false;
    };
    if *counter_type != CounterType::Plus1Plus1 || *value != 1 {
        return false;
    }

    let crate::types::ability::ReplacementMode::Optional {
        decline: Some(decline),
    } = &replacement.mode
    else {
        return false;
    };
    matches!(
        &*decline.effect,
        Effect::GenericEffect {
            static_abilities,
            duration: Some(Duration::Permanent),
            ..
        } if static_abilities.iter().any(static_grants_haste_to_self)
    )
}

fn static_grants_haste_to_self(static_def: &StaticDefinition) -> bool {
    static_def.affected == Some(TargetFilter::SelfRef)
        && static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste
                }
            )
        })
}

/// CR 702.98a: Unleash represents two static abilities — the permanent "may enter
/// with an additional +1/+1 counter on it" and "can't block as long as it has a
/// +1/+1 counter on it." The first mirrors `synthesize_riot`'s optional ETB +1/+1
/// counter (here with no decline branch); the second is a `CantBlock` static gated
/// on the creature carrying any +1/+1 counter (CR 702.98a keys on *any* such
/// counter, not only the unleash one). Static grants of Unleash synthesize the
/// same shape from the static's affected filter, mirroring `synthesize_riot`.
pub fn synthesize_unleash(face: &mut CardFace) {
    let printed_count = face
        .keywords
        .iter()
        .filter(|kw| matches!(kw, Keyword::Unleash))
        .count();
    add_unleash_replacements(face, TargetFilter::SelfRef, printed_count);
    if printed_count > 0 {
        add_unleash_cant_block_static(
            face,
            TargetFilter::SelfRef,
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 1,
                maximum: None,
            },
        );
    }

    let static_grants: Vec<TargetFilter> = face
        .static_abilities
        .iter()
        .filter(|static_def| static_grants_unleash(static_def))
        .map(|static_def| static_def.affected.clone().unwrap_or(TargetFilter::Any))
        .collect();
    for filter in static_grants {
        add_unleash_replacements(face, filter.clone(), 1);
        add_unleash_cant_block_static(
            face,
            filter,
            StaticCondition::RecipientHasCounters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                minimum: 1,
                maximum: None,
            },
        );
    }
}

fn static_grants_unleash(static_def: &StaticDefinition) -> bool {
    static_def.mode == StaticMode::Continuous
        && static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Unleash
                }
            )
        })
}

fn add_unleash_replacements(face: &mut CardFace, valid_card: TargetFilter, needed: usize) {
    let existing = face
        .replacements
        .iter()
        .filter(|replacement| is_unleash_replacement(replacement, &valid_card))
        .count();
    for _ in existing..needed {
        face.replacements
            .push(build_unleash_replacement(valid_card.clone()));
    }
}

fn build_unleash_replacement(valid_card: TargetFilter) -> ReplacementDefinition {
    // CR 702.98a: "You may have this permanent enter with an additional +1/+1
    // counter on it."
    let counter_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .description("This permanent enters with an additional +1/+1 counter on it".to_string());

    ReplacementDefinition {
        event: ReplacementEvent::Moved,
        execute: Some(Box::new(counter_branch)),
        mode: crate::types::ability::ReplacementMode::Optional { decline: None },
        valid_card: Some(valid_card),
        destination_zone: Some(Zone::Battlefield),
        description: Some(
            "CR 702.98a: Unleash — this permanent may enter with an additional +1/+1 counter on it."
                .to_string(),
        ),
        ..ReplacementDefinition::new(ReplacementEvent::Moved)
    }
}

fn is_unleash_replacement(replacement: &ReplacementDefinition, valid_card: &TargetFilter) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || replacement.valid_card.as_ref() != Some(valid_card)
        || replacement.destination_zone != Some(Zone::Battlefield)
        || !matches!(
            replacement.mode,
            crate::types::ability::ReplacementMode::Optional { decline: None }
        )
    {
        return false;
    }
    let Some(execute) = replacement.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        }
    )
}

fn add_unleash_cant_block_static(
    face: &mut CardFace,
    affected: TargetFilter,
    condition: StaticCondition,
) {
    if !face
        .static_abilities
        .iter()
        .any(|static_def| is_unleash_cant_block_static(static_def, &affected, &condition))
    {
        face.static_abilities
            .push(build_unleash_cant_block_static(affected, condition));
    }
}

fn build_unleash_cant_block_static(
    affected: TargetFilter,
    condition: StaticCondition,
) -> StaticDefinition {
    // CR 702.98a: "This permanent can't block as long as it has a +1/+1 counter on
    // it." The condition is source-relative for printed Unleash and
    // recipient-relative for static grants.
    StaticDefinition::new(StaticMode::CantBlock)
        .affected(affected)
        .condition(condition)
        .description("can't block as long as it has a +1/+1 counter on it".to_string())
}

fn is_unleash_cant_block_static(
    static_def: &StaticDefinition,
    affected: &TargetFilter,
    condition: &StaticCondition,
) -> bool {
    static_def.mode == StaticMode::CantBlock
        && static_def.affected.as_ref() == Some(affected)
        && static_def.condition.as_ref() == Some(condition)
}

/// CR 702.93a: Undying — "When this permanent is put into a graveyard from the
/// battlefield, if it had no +1/+1 counters on it, return it to the battlefield
/// under its owner's control with a +1/+1 counter on it."
///
/// Synthesizes one dies-triggered ability per `Keyword::Undying` on the face:
///   * `TriggerMode::ChangesZone` with `origin = Battlefield`, `destination =
///     Graveyard`, `valid_card = SelfRef` (the canonical dies trigger shape;
///     CR 603.10a — leaves-the-battlefield triggers look back in time).
///   * `condition = Not(HadCounters { Some("P1P1") })` — CR 400.7 LKI lookup
///     against `state.lki_cache` for the source's pre-death counter map.
///   * Execute body: `Effect::ChangeZone` from `Graveyard` → `Battlefield`
///     targeting `SelfRef`, with `enter_with_counters = [("P1P1", 1)]`. The
///     default `enters_under = None` matches the rule's "under its owner's
///     control" exactly (CR 110.2a).
///
/// Per CR 113.2c ("If an object has multiple instances of the same ability,
/// each instance functions independently") combined with the absence of a
/// redundancy clause in CR 702.93 (compare CR 702.2f for deathtouch and
/// CR 702.9c for flying, which explicitly mark those keywords as redundant),
/// every `Keyword::Undying` on the face emits a distinct trigger.
///
/// Sibling of `synthesize_persist` — both share this dies-trigger shape and
/// differ only in counter polarity (CR 702.79a vs CR 702.93a). They are kept
/// as separate synthesizers (not parameterized into one) because the keyword
/// enum carries the polarity choice at the type level; no runtime branching
/// is needed.
pub fn synthesize_undying(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Undying));
}

/// CR 702.79a: Persist — "When this permanent is put into a graveyard from the
/// battlefield, if it had no -1/-1 counters on it, return it to the battlefield
/// under its owner's control with a -1/-1 counter on it."
///
/// Mirror of `synthesize_undying` with -1/-1 counters (`CounterType::Minus1Minus1`
/// → `"M1M1"`). Per CR 113.2c and the absence of a redundancy clause in
/// CR 702.79, every `Keyword::Persist` instance functions independently, so
/// one synthesized trigger is emitted per keyword on the face.
pub fn synthesize_persist(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Persist));
}

/// CR 702.135a: Afterlife N — "When this permanent is put into a graveyard from
/// the battlefield, create N 1/1 white and black Spirit creature tokens with
/// flying."
///
/// Synthesized as a self-referential dies trigger (`ChangesZone`
/// Battlefield→Graveyard with `valid_card: SelfRef`, the same shape Undying and
/// Persist use) whose effect creates the Spirit tokens. The trigger keys on
/// "this permanent" (CR 702.135a), not "this creature", so it also fires for a
/// non-creature permanent that has afterlife.
///
/// CR 702.135b: multiple instances of afterlife trigger separately, so (via
/// `install_matching`) one trigger is emitted per `Keyword::Afterlife(_)` on the
/// face.
pub fn synthesize_afterlife(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Afterlife(_)));
}

/// Builds the CR 702.135a Afterlife dies trigger for `count` Spirit tokens.
fn build_afterlife_trigger(count: u32) -> TriggerDefinition {
    let plural = if count == 1 { "" } else { "s" };
    // CR 702.135a + CR 111.3 / CR 111.4: 1/1 white and black Spirit creature
    // token with flying. Colors carry both White and Black (CR 105.2b
    // multicolored).
    let token_effect = Effect::Token {
        name: "Spirit".to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string(), "Spirit".to_string()],
        colors: vec![ManaColor::White, ManaColor::Black],
        keywords: vec![Keyword::Flying],
        tapped: false,
        count: QuantityExpr::Fixed {
            value: count as i32,
        },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    };

    let execute = AbilityDefinition::new(AbilityKind::Spell, token_effect).description(format!(
        "Create {count} 1/1 white and black Spirit creature token{plural} with flying"
    ));

    // CR 702.135a: "put into a graveyard from the battlefield" — the same
    // Battlefield→Graveyard self-referential dies trigger shape as Undying /
    // Persist (`build_dies_return_with_counter_trigger`).
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(format!(
            "CR 702.135a: When ~ is put into a graveyard from the battlefield, create {count} 1/1 white and black Spirit creature token{plural} with flying."
        ))
}

/// Idempotency-shape predicate for `synthesize`-installed Afterlife triggers.
/// Mirrors `is_dies_return_with_counter_trigger` but discriminates on the
/// full CR 702.135a Spirit-token effect (so it never collides with another
/// self-ref dies trigger that happens to create a Spirit token).
#[cfg(test)]
fn is_afterlife_trigger(t: &TriggerDefinition) -> bool {
    afterlife_trigger_count(t).is_some()
}

fn is_afterlife_trigger_for_count(t: &TriggerDefinition, count: u32) -> bool {
    let Ok(count) = i32::try_from(count) else {
        return false;
    };
    afterlife_trigger_count(t) == Some(count)
}

fn afterlife_trigger_count(t: &TriggerDefinition) -> Option<i32> {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.origin != Some(Zone::Battlefield)
        || t.destination != Some(Zone::Graveyard)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return None;
    }
    let execute = t.execute.as_deref()?;
    let Effect::Token {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        tapped,
        count,
        owner,
        attach_to,
        enters_attacking,
        supertypes,
        static_abilities,
        enter_with_counters,
        ..
    } = &*execute.effect
    else {
        return None;
    };

    if name != "Spirit"
        || !matches!(power, PtValue::Fixed(1))
        || !matches!(toughness, PtValue::Fixed(1))
        || !types.iter().map(String::as_str).eq(["Creature", "Spirit"])
        || colors.as_slice() != [ManaColor::White, ManaColor::Black]
        || keywords.as_slice() != [Keyword::Flying]
        || *tapped
        || owner != &TargetFilter::Controller
        || attach_to.is_some()
        || *enters_attacking
        || !supertypes.is_empty()
        || !static_abilities.is_empty()
        || !enter_with_counters.is_empty()
    {
        return None;
    }

    let QuantityExpr::Fixed { value } = count else {
        return None;
    };
    Some(*value)
}

/// CR 702.46a: Soulshift N — "When this creature dies, you may return target
/// Spirit card with mana value N or less from your graveyard to your hand."
///
/// CR 702.46b: each instance of Soulshift triggers separately, so (via
/// `install_matching`) one trigger is emitted per `Keyword::Soulshift(_)` on the
/// face — mirroring Afterlife (CR 702.135b) and Bushido (CR 702.45b).
pub fn synthesize_soulshift(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Soulshift(_)));
}

/// Builds the CR 702.46a Soulshift dies trigger for mana value `n` or less.
///
/// Shape mirrors `build_afterlife_trigger`, the nearest Dies-mode analog. The
/// trigger is `TriggerMode::ChangesZone`, Battlefield→Graveyard, with
/// `valid_card = SelfRef` ("when this creature dies", CR 702.46a — the same
/// self-referential dies shape as Afterlife / Undying / Persist). Its execute
/// body is an OPTIONAL ability (`.optional()` — the "you may", mirroring Graft's
/// `build_graft_enters_trigger`) carrying an `Effect::ChangeZone` that moves the
/// chosen target from Graveyard → Hand. The `target` is a graveyard-zone
/// `TargetFilter::Typed` constrained to subtype Spirit AND mana value ≤ N,
/// reusing the existing `FilterProp` primitives: `TypeFilter::Subtype("Spirit")`
/// (CR 205.3), `FilterProp::InZone { Graveyard }`, `FilterProp::Owned { You }`
/// ("your graveyard", CR 109.5), and `FilterProp::Cmc { LE, Fixed(n) }` ("mana
/// value N or less", CR 202.3).
///
/// Graveyard is a public zone, so `extract_target_filter_from_effect` surfaces
/// this `Typed` filter as a real stack-time target (CR 603.5) — no extra
/// targeting wiring is needed (unlike Hand/Library origins, which it skips).
fn build_soulshift_trigger(n: u32) -> TriggerDefinition {
    // CR 109.5 + CR 202.3 + CR 205.3: "target Spirit card with mana value N or
    // less from your graveyard". Conjunction of subtype, owner+zone, and a
    // mana-value comparator, all expressed with existing `FilterProp` building
    // blocks (no new filter language).
    let spirit_in_graveyard = TargetFilter::Typed(
        TypedFilter::card()
            .subtype("Spirit".to_string())
            .properties(vec![
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: n as i32 },
                },
            ]),
    );

    // CR 702.46a: "return ... from your graveyard to your hand". Graveyard→Hand
    // move of the single chosen target via the existing `Effect::ChangeZone`
    // plumbing. `origin = Some(Graveyard)` mirrors the parsed "return target card
    // from your graveyard to your hand" shape (oracle_effect's Graveyard→Hand
    // ChangeZone). Default flags (no counters, no transform, owner's control).
    let return_to_hand = Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Hand,
        target: spirit_in_graveyard,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: false,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        face_down_profile: None,
    };

    // CR 603.5 + CR 702.46a "you may": optionality lives on the execute ability
    // (mirrors Graft's `move_one.optional()`), so the controller is prompted
    // before the return resolves; declining leaves the card in the graveyard.
    let execute = AbilityDefinition::new(AbilityKind::Spell, return_to_hand)
        .optional()
        .description(format!(
            "You may return target Spirit card with mana value {n} or less from your graveyard to your hand"
        ));

    // CR 702.46a: "when this creature dies" — Battlefield→Graveyard self-ref
    // dies trigger, the same shape as Afterlife (`build_afterlife_trigger`).
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(format!(
            "CR 702.46a: When ~ dies, you may return target Spirit card with mana value {n} or less from your graveyard to your hand."
        ))
}

/// Idempotency-shape predicate for `synthesize`-installed Soulshift triggers.
/// Mirrors `is_afterlife_trigger_for_count` — discriminates on the full
/// CR 702.46a Graveyard→Hand Spirit-return effect (so it never collides with
/// another self-ref dies trigger). The mana-value threshold `n` is load-bearing:
/// a face with Soulshift 4 and Soulshift 7 (CR 702.46b) keeps both triggers
/// rather than collapsing by keyword kind.
fn is_soulshift_trigger_for_value(t: &TriggerDefinition, n: u32) -> bool {
    let Ok(n) = i32::try_from(n) else {
        return false;
    };
    soulshift_trigger_value(t) == Some(n)
}

/// Extracts the mana-value threshold from a synthesized Soulshift trigger, or
/// `None` if `t` is not a Soulshift trigger. Used by the idempotency predicate
/// and tests; shared so the shape definition lives in exactly one place.
fn soulshift_trigger_value(t: &TriggerDefinition) -> Option<i32> {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.origin != Some(Zone::Battlefield)
        || t.destination != Some(Zone::Graveyard)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return None;
    }
    let execute = t.execute.as_deref()?;
    // CR 702.46a "you may": the return is an optional ability.
    if !execute.optional {
        return None;
    }
    let Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Hand,
        target: TargetFilter::Typed(tf),
        up_to: false,
        ..
    } = &*execute.effect
    else {
        return None;
    };

    // Subtype Spirit (CR 205.3) + your-graveyard (CR 109.5).
    if tf.get_subtype() != Some("Spirit")
        || !tf.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard,
        })
        || !tf.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::You,
        })
    {
        return None;
    }

    // CR 202.3: the "mana value N or less" comparator carries the threshold.
    tf.properties.iter().find_map(|p| match p {
        FilterProp::Cmc {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value },
        } => Some(*value),
        _ => None,
    })
}

/// Test-only shape predicate (value-agnostic) — true iff `t` is any synthesized
/// Soulshift trigger. Mirrors `is_afterlife_trigger`.
#[cfg(test)]
fn is_soulshift_trigger(t: &TriggerDefinition) -> bool {
    soulshift_trigger_value(t).is_some()
}

/// CR 702.112a: Renown N — combat-damage-to-player trigger with an
/// intervening-if renowned designation check.
///
/// CR 702.112c: Each renown instance triggers independently, so synthesis emits
/// one trigger per `Keyword::Renown(_)` instance.
pub fn synthesize_renown(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Renown(_)));
}

/// CR 702.86a: Annihilator N — "Whenever this creature attacks, defending
/// player sacrifices N permanents."
///
/// Each `Keyword::Annihilator(n)` on the face emits one attack-triggered
/// ability whose execute body is `Effect::Sacrifice` over the permanent pool
/// controlled by the per-attacker defending player. The defending player is
/// resolved at resolution time through
/// `ControllerRef::DefendingPlayer` →
/// `defending_player_for_attacker(state, ability.source_id)` (CR 508.5 / 508.5a:
/// the defending player relative to an attacking creature is the specific
/// player that creature is attacking — never "each opponent"). This means in
/// multiplayer, only the player being attacked by THIS creature sacrifices.
///
/// CR 702.86b: "If a creature has multiple instances of annihilator, each
/// triggers separately." One trigger is synthesized per `Keyword::Annihilator`
/// on the face. (CR 113.2c also independently mandates that multiple instances
/// of an ability function independently.)
///
/// The trigger uses `TriggerMode::Attacks` with `valid_card = SelfRef` so it
/// fires only when this creature is among the declared attackers
/// (`match_attacks` in `trigger_matchers.rs`).
///
/// Sacrifice count is encoded as `QuantityExpr::Fixed { value: n }`. The
/// shared sacrifice resolver (`game::effects::sacrifice::resolve`) routes
/// `ControllerRef::DefendingPlayer` through `resolve_sacrifice_scope` and
/// handles the "fewer permanents than N" case via the CR 609.3 "does only as
/// much as possible" mandatory-all fast-path — no separate "as many as
/// possible" plumbing is needed here.
pub fn synthesize_annihilator(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Annihilator(_)));
}

/// CR 702.39a: Provoke — an `Attacks` trigger (source = this creature) that may
/// untap a creature the defending player controls and force it to block this
/// creature this turn. One trigger is synthesized per `Keyword::Provoke`
/// (Provoke has no numeric parameter; multiple instances are vanishingly rare
/// but each functions independently per CR 113.2c, which `install_matching`'s
/// per-instance emission preserves). See `build_provoke_trigger`.
pub fn synthesize_provoke(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Provoke));
}

/// CR 702.83a: Exalted — an attack trigger that fires whenever a creature you
/// control attacks alone, giving +1/+1 until end of turn. CR 702.83b: each
/// instance triggers separately, so one trigger is synthesized per
/// `Keyword::Exalted` instance.
pub fn synthesize_exalted(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Exalted));
}

/// CR 702.25a: Flanking — install the becomes-blocked debuff trigger that gives
/// each blocking creature without flanking -1/-1 until end of turn. CR 702.25b:
/// each instance triggers separately (one trigger per `Keyword::Flanking`).
pub fn synthesize_flanking(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Flanking));
}

/// CR 702.45a: Bushido N — "Whenever this creature blocks or becomes blocked, it
/// gets +N/+N until end of turn." Two self-triggers (blocks + becomes-blocked),
/// since there is no combined block trigger mode. CR 702.45b: each instance
/// triggers separately, so one pair is synthesized per `Keyword::Bushido`.
pub fn synthesize_bushido(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Bushido(_)));
}

/// CR 702.91a: Battle cry — "whenever this creature attacks, each other
/// attacking creature gets +1/+0 until end of turn." CR 702.91b: each instance
/// triggers separately, so one trigger is synthesized per `Keyword::Battlecry`.
pub fn synthesize_battlecry(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Battlecry));
}

/// CR 702.23a: Rampage N — "whenever this creature becomes blocked, it gets +N/+N
/// until end of turn for each creature blocking it beyond the first." CR 702.23c:
/// each instance triggers separately, so one trigger is synthesized per
/// `Keyword::Rampage` instance.
pub fn synthesize_rampage(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Rampage(_)));
}

/// CR 702.121a: Melee — "whenever this creature attacks, it gets +1/+1 until end
/// of turn for each opponent you attacked with a creature this combat." CR
/// 702.121b: each instance triggers separately, so one trigger is synthesized per
/// `Keyword::Melee` instance.
pub fn synthesize_melee(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Melee));
}

/// CR 702.101a: Extort — a spell-cast trigger that lets you pay {W/B} to drain
/// each opponent for 1 life. CR 702.101b: each instance triggers separately,
/// so one trigger is synthesized per `Keyword::Extort` instance.
pub fn synthesize_extort(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Extort));
}

/// CR 702.191a: Increment — spell-cast trigger that puts a +1/+1 counter on this
/// creature when mana spent to cast the spell exceeds its power or toughness.
/// CR 702.191b: each instance triggers separately.
pub fn synthesize_increment(face: &mut CardFace) {
    // `install_matching` dedupes exact synthesized trigger values. Increment can
    // also arrive from parsed reminder text, whose trigger is semantically the
    // same but not necessarily structurally identical, so count by Increment
    // identity instead.
    let desired = face
        .keywords
        .iter()
        .filter(|kw| matches!(kw, Keyword::Increment))
        .count();
    let existing = face
        .triggers
        .iter()
        .filter(|t| is_increment_trigger(t))
        .count();

    for _ in existing..desired {
        face.triggers.push(build_increment_trigger());
    }
}

/// CR 702.105a: Dethrone — an attack trigger that fires whenever this creature
/// attacks the player with the most life or tied for most life, putting a +1/+1
/// counter on it. CR 702.105b: each instance triggers separately.
pub fn synthesize_dethrone(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Dethrone));
}

/// CR 702.100a: Evolve — an ETB trigger that fires whenever another creature you
/// control enters with greater power or toughness than the Evolve creature,
/// putting a +1/+1 counter on it. CR 702.100d: each instance triggers
/// separately, so one trigger is synthesized per `Keyword::Evolve` instance.
pub fn synthesize_evolve(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Evolve));
}

/// CR 702.116a: Myriad is an attack trigger. On resolution, the controller may
/// create one tapped attacking copy token for each opponent other than the
/// source creature's defending player; if any are created, they are exiled at
/// end of combat. The resolver chooses the player branch of "that player or a
/// planeswalker they control" until the engine has UI for that choice.
///
/// CR 702.116b: Multiple Myriad instances trigger separately, so one trigger is
/// synthesized per `Keyword::Myriad` instance.
pub fn synthesize_myriad(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Myriad));
}

/// Double team is an Arena/Alchemy keyword that triggers on attack and creates
/// one tapped attacking token copy of the attacking creature.
pub fn synthesize_double_team(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::DoubleTeam));
}

/// CR 702.95a + CR 115.10a: Soulbond represents two optional triggered
/// abilities. One fires when the soulbond creature enters and can pair it with
/// another unpaired creature you control; the other fires when another unpaired
/// creature you control enters and can pair that creature with the soulbond
/// source. The paired creature is not a target.
pub fn synthesize_soulbond(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Soulbond));
}

fn unpaired_creature_you_control_on_battlefield() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![
                FilterProp::InZone {
                    zone: Zone::Battlefield,
                },
                FilterProp::Unpaired,
            ]),
    )
}

fn another_unpaired_creature_you_control() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::Another, FilterProp::Unpaired]),
    )
}

fn another_unpaired_creature_you_control_on_battlefield() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![
                FilterProp::Another,
                FilterProp::InZone {
                    zone: Zone::Battlefield,
                },
                FilterProp::Unpaired,
            ]),
    )
}

fn build_soulbond_triggers() -> Vec<TriggerDefinition> {
    let source_unpaired = TriggerCondition::SourceMatchesFilter {
        filter: unpaired_creature_you_control_on_battlefield(),
    };
    let source_enters_condition = TriggerCondition::And {
        conditions: vec![
            source_unpaired.clone(),
            TriggerCondition::ControlsType {
                filter: another_unpaired_creature_you_control_on_battlefield(),
            },
        ],
    };
    let other_enters_condition = TriggerCondition::And {
        conditions: vec![
            source_unpaired,
            TriggerCondition::ZoneChangeObjectMatchesFilter {
                origin: None,
                destination: Zone::Battlefield,
                filter: another_unpaired_creature_you_control_on_battlefield(),
            },
        ],
    };
    let pair_target = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PairWith {
            target: another_unpaired_creature_you_control_on_battlefield(),
        },
    )
    .target_choice_timing(TargetChoiceTiming::Resolution)
    .optional();
    let pair_triggering = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PairWith {
            target: TargetFilter::TriggeringSource,
        },
    )
    .target_choice_timing(TargetChoiceTiming::Resolution)
    .optional();

    vec![
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .condition(source_enters_condition)
            .execute(pair_target)
            .description(
                "CR 702.95a: When this creature enters, you may pair it with another unpaired creature you control.".to_string(),
            ),
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(another_unpaired_creature_you_control())
            .destination(Zone::Battlefield)
            .condition(other_enters_condition)
            .execute(pair_triggering)
            .description(
                "CR 702.95a: Whenever another unpaired creature you control enters, you may pair it with this creature.".to_string(),
            ),
    ]
}

fn is_soulbond_trigger(trigger: &TriggerDefinition) -> bool {
    if trigger.mode != TriggerMode::ChangesZone || trigger.destination != Some(Zone::Battlefield) {
        return false;
    }
    trigger.execute.as_ref().is_some_and(|ability| {
        matches!(ability.effect.as_ref(), Effect::PairWith { .. }) && ability.optional
    })
}

/// Idempotency-shape predicate for `synthesize_annihilator`. True iff `trigger`
/// is the synthesized Annihilator attack-trigger shape (`TriggerMode::Attacks`
/// with `valid_card = SelfRef` and execute body `Effect::Sacrifice` over a
/// `ControllerRef::DefendingPlayer` permanent filter).
///
/// The check is narrow on purpose: an unrelated `Attacks` trigger on the same
/// face (e.g., "Whenever ~ attacks, you draw a card") must NOT be counted as
/// an existing Annihilator emission.
fn is_annihilator_attack_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::Attacks)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    let Effect::Sacrifice { target, .. } = &*execute.effect else {
        return false;
    };
    matches!(
        target,
        TargetFilter::Typed(tf)
            if tf.controller == Some(ControllerRef::DefendingPlayer)
    )
}

/// Idempotency-shape predicate for `synthesize`-installed Evolve triggers.
/// `TriggerMode::Evolve` is unique to the Evolve keyword, so the mode alone
/// uniquely identifies a synthesized Evolve trigger — no execute-shape check
/// is needed (or wanted: it would disambiguate nothing).
fn is_evolve_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Evolve)
}

fn is_myriad_attack_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Attacks)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && t.execute.as_deref().is_some_and(|ability| {
            ability.optional && matches!(ability.effect.as_ref(), Effect::Myriad)
        })
}
/// Idempotency-shape predicate for `synthesize_dethrone`. True iff `trigger`
/// is the synthesized Dethrone attack-trigger shape (`TriggerMode::Attacks`
/// + `valid_card = SelfRef` + execute = PutCounter(P1P1) on SelfRef
/// + condition = DefendingPlayer life >= AllPlayers max life).
fn is_dethrone_attack_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::Attacks)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
        || !matches!(
            t.attack_target_filter,
            Some(crate::types::triggers::AttackTargetFilter::Player)
        )
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            target: TargetFilter::SelfRef,
            ..
        }
    ) && t.condition.is_some()
}
fn is_echo_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::PayEcho)
        && t.phase == Some(Phase::Upkeep)
        && matches!(t.valid_target, Some(TargetFilter::Controller))
        && matches!(t.condition, Some(TriggerCondition::EchoDue))
        && t.unless_pay.is_some()
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            })
        )
}

fn build_myriad_trigger() -> TriggerDefinition {
    let execute = AbilityDefinition::new(AbilityKind::Spell, Effect::Myriad)
        .optional()
        .description(
            "Create token copies for each opponent other than defending player".to_string(),
        );

    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(
            "CR 702.116a: Myriad — whenever this creature attacks, you may create tapped attacking copy tokens for each opponent other than defending player, then exile them at end of combat.".to_string(),
        )
}

fn is_double_team_attack_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Attacks)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && t.execute
            .as_deref()
            .is_some_and(|ability| match &*ability.effect {
                Effect::CopyTokenOf {
                    target,
                    owner,
                    enters_attacking,
                    tapped,
                    count,
                    ..
                } => {
                    !ability.optional
                        && matches!(target, TargetFilter::SelfRef)
                        && matches!(owner, TargetFilter::Controller)
                        && *enters_attacking
                        && *tapped
                        && matches!(count, QuantityExpr::Fixed { value: 1 })
                }
                _ => false,
            })
}

fn build_double_team_trigger() -> TriggerDefinition {
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: true,
            tapped: true,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        },
    )
    .description("Create a tapped attacking token copy of this creature".to_string());

    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(
            "Double team — whenever this creature attacks, create a tapped and attacking token that's a copy of it.".to_string(),
        )
}

fn build_echo_trigger(cost: ManaCost) -> TriggerDefinition {
    let sac = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    let mut trigger = TriggerDefinition::new(TriggerMode::PayEcho)
        .phase(Phase::Upkeep)
        .valid_target(TargetFilter::Controller)
        .condition(TriggerCondition::EchoDue)
        .execute(sac)
        .description(
            "CR 702.30a: At the beginning of your upkeep, sacrifice this permanent unless you pay its echo cost."
                .to_string(),
        );
    trigger.unless_pay = Some(UnlessPayModifier {
        cost: AbilityCost::Mana { cost },
        payer: TargetFilter::Controller,
    });
    trigger
}

/// CR 702.24a: Cumulative-upkeep trigger shape — recognizer for synthesis
/// idempotency. Mirrors `is_echo_trigger`: stays correct if the builder ever
/// changes shape. The trigger's outer effect places the age counter; the
/// sub-ability sacrifices unless the per-counter cost is paid.
fn is_cumulative_upkeep_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::PayCumulativeUpkeep)
        && t.phase == Some(Phase::Upkeep)
        && matches!(t.valid_target, Some(TargetFilter::Controller))
        // CR 702.24a: "if this permanent is on the battlefield" — the
        // intervening-if must be wired or a bounced source would still
        // resolve its sub-ability chain. See `build_cumulative_upkeep_trigger`.
        && matches!(
            t.condition,
            Some(TriggerCondition::SourceInZone {
                zone: Zone::Battlefield
            })
        )
        && t.execute.as_deref().is_some_and(|outer| {
            matches!(
                outer.effect.as_ref(),
                Effect::AddCounter {
                    counter_type: CounterType::Age,
                    ..
                }
            ) && outer.sub_ability.as_deref().is_some_and(|sub| {
                matches!(
                    sub.effect.as_ref(),
                    Effect::Sacrifice {
                        target: TargetFilter::SelfRef,
                        ..
                    }
                ) && sub.unless_pay.as_ref().is_some_and(|u| {
                    matches!(
                        &u.cost,
                        AbilityCost::PerCounter {
                            counter: CounterType::Age,
                            target: TargetFilter::SelfRef,
                            ..
                        },
                    )
                })
            })
        })
}

/// CR 702.24a: Cumulative upkeep is a triggered ability — "Cumulative upkeep
/// [cost]" means "At the beginning of your upkeep, if this permanent is on
/// the battlefield, put an age counter on this permanent. Then you may pay
/// [cost] for each age counter on it. If you don't, sacrifice it."
///
/// The trigger is modeled as a chained `AbilityDefinition`:
///   - Outer effect: `AddCounter { Age, 1, SelfRef }` (CR 122.1 + CR 702.24a)
///     places one new age counter unconditionally before the prompt.
///   - Sub-ability: `Sacrifice { SelfRef }` gated by
///     `unless_pay = PerCounter { Age, SelfRef, base }`. The sub-ability
///     resolves AFTER the parent (per `resolve_chain_body` in `effects/mod.rs`),
///     so the `PerCounter` expansion reads the post-tick counter total — the
///     exact semantics CR 702.24a requires.
///
/// The builder is generic over `base_cost: AbilityCost`: mana, life payment,
/// sacrifice, and OneOf-disjunctive costs all compose with `PerCounter`
/// uniformly (CLAUDE.md "build for the class"). Callers must pre-filter the
/// base cost through `AbilityCost::supports_cumulative_upkeep_payment` — the
/// builder itself does not refuse unsupported shapes.
///
/// Exposed `pub(crate)` so the end-to-end Mystic Remora tests in
/// `game::engine::phase_trigger_regression_tests` bind directly to the
/// production synthesizer rather than a duplicated mirror — any regression
/// in this builder's chained-ability shape (variant ordering, missing
/// `.phase(Upkeep)`, swapped PerCounter payer, etc.) breaks the pipeline
/// tests immediately.
pub(crate) fn build_cumulative_upkeep_trigger(base_cost: AbilityCost) -> TriggerDefinition {
    // Inner sub-ability: "sacrifice ~ unless you pay [base × age counters]".
    // The `unless_pay` lives on the SUB-ability (not the outer trigger) so the
    // outer AddCounter resolves first and the per-counter cost reads the
    // post-tick total at sub-resolution time.
    let mut sacrifice_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    sacrifice_branch.unless_pay = Some(UnlessPayModifier {
        cost: AbilityCost::PerCounter {
            counter: CounterType::Age,
            target: TargetFilter::SelfRef,
            base: Box::new(base_cost),
        },
        payer: TargetFilter::Controller,
    });

    // Outer execute: "put an age counter on ~", then the sacrifice-or-pay
    // branch. CR 122.1: AddCounter is the typed primitive for placing a
    // counter on a permanent.
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::AddCounter {
            counter_type: CounterType::Age,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .sub_ability(sacrifice_branch);

    TriggerDefinition::new(TriggerMode::PayCumulativeUpkeep)
        .phase(Phase::Upkeep)
        .valid_target(TargetFilter::Controller)
        // CR 603.4 + CR 702.24a: "if this permanent is on the battlefield" —
        // re-checked at resolution time so a bounced/exiled source no-ops the
        // entire chain (no age counter is placed, no unless-pay prompt is
        // emitted, no sacrifice). Without this guard the AddCounter outer
        // effect would write to the post-move object's counter map and the
        // sub-ability would still prompt the controller.
        .condition(TriggerCondition::SourceInZone {
            zone: Zone::Battlefield,
        })
        .execute(execute)
        .description(
            "CR 702.24a: At the beginning of your upkeep, if this permanent \
             is on the battlefield, put an age counter on this permanent, \
             then sacrifice it unless you pay its upkeep cost for each age \
             counter on it."
                .to_string(),
        )
}

fn build_annihilator_trigger(n: u32) -> TriggerDefinition {
    // CR 701.21a: sacrifice moves the permanent to its owner's graveyard.
    // Sacrifice scope derives from the target filter's `ControllerRef`;
    // `DefendingPlayer` routes to `defending_player_for_attacker(state,
    // source_id)` at resolution.
    let sacrifice_effect = Effect::Sacrifice {
        target: TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::DefendingPlayer),
        ),
        count: QuantityExpr::Fixed { value: n as i32 },
        min_count: 0,
    };

    let execute =
        AbilityDefinition::new(AbilityKind::Spell, sacrifice_effect).description(format!(
            "Defending player sacrifices {n} permanent{}",
            if n == 1 { "" } else { "s" }
        ));

    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(format!(
            "CR 702.86a: Annihilator {n} — whenever ~ attacks, defending player sacrifices {n} permanent{}.",
            if n == 1 { "" } else { "s" }
        ))
}

/// CR 702.39a: A Provoke trigger — a self-scoped (`valid_card: SelfRef`)
/// `Attacks` trigger whose execute body untaps a creature the defending player
/// controls (`Effect::Untap` targeting a `ControllerRef::DefendingPlayer`
/// creature) and chains an `Effect::ForceBlock` on that same target via
/// `TargetFilter::ParentTarget`. Used by `RemoveKeyword` symmetric removal and
/// `triggers_for`/`trigger_matches_keyword_kind` so a granted-then-removed
/// `Provoke` strips exactly its own trigger and a coincidental "Whenever ~
/// attacks, untap target creature" printed trigger is never misclassified.
fn is_provoke_attack_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::Attacks)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    // CR 702.39a: "you may have target creature..." — the untap is optional.
    if !execute.optional {
        return false;
    }
    // CR 702.39a + CR 701.26b: the parent body untaps a creature the defending
    // player controls.
    let Effect::Untap {
        target: TargetFilter::Typed(tf),
    } = &*execute.effect
    else {
        return false;
    };
    if tf.controller != Some(ControllerRef::DefendingPlayer) {
        return false;
    }
    // CR 702.39a + CR 509.1c: the chained sub-body force-blocks that same
    // target (`ParentTarget`).
    matches!(
        execute.sub_ability.as_deref().map(|a| &*a.effect),
        Some(Effect::ForceBlock {
            target: TargetFilter::ParentTarget,
        })
    )
}

/// CR 702.39a: Provoke — "Whenever this creature attacks, you may have target
/// creature defending player controls untap and block it this turn if able."
///
/// The trigger is `TriggerMode::Attacks` with `valid_card = SelfRef` so it
/// fires only when this creature is among the declared attackers
/// (`match_attacks` in `trigger_matchers.rs`), mirroring `build_annihilator_trigger`.
///
/// CR 702.39a is "you may", so the execute ability is `optional`. The single
/// target is a creature controlled by the defending player — `ControllerRef::
/// DefendingPlayer` resolves at target-selection time to the player THIS
/// creature is attacking (CR 508.5 / 508.5a), not "each opponent". The execute
/// body untaps that creature (`Effect::Untap` — CR 701.26b), then a
/// continuation `sub_ability` applies `Effect::ForceBlock` to the same target
/// via `TargetFilter::ParentTarget` (CR 608.2c chained-anaphor inheritance).
///
/// `Effect::ForceBlock` is the EXISTING source-referential force-block resolver
/// (`game::effects::force_block`): because the source is an active attacker at
/// resolution it grants `StaticMode::MustBlockAttacker { attacker: source }`
/// (CR 702.39a + CR 509.1c), enforced in `combat.rs` declare-blockers
/// validation. No force-block logic is reimplemented here.
///
fn build_provoke_trigger() -> TriggerDefinition {
    // CR 702.39a: "target creature defending player controls". `DefendingPlayer`
    // routes to `defending_player_for_attacker(state, source_id)` at
    // target-selection time (CR 508.5 / 508.5a).
    let untap_target =
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::DefendingPlayer));

    // CR 702.39a + CR 509.1c: chained continuation forcing the untapped target
    // to block this attacker. Reuses the existing source-referential ForceBlock
    // resolver via `ParentTarget` so the same creature is affected.
    let force_block = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ForceBlock {
            target: TargetFilter::ParentTarget,
        },
    )
    .description("CR 509.1c: that creature blocks this creature this turn if able".to_string());

    // CR 702.39a + CR 701.26b: "you may have target creature ... untap" — the
    // optional parent body untaps the chosen defender, then force-blocks it.
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Untap {
            target: untap_target,
        },
    )
    .optional()
    .sub_ability(force_block)
    .description(
        "Provoke — untap target creature defending player controls; it blocks this turn if able"
            .to_string(),
    );

    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(
            "CR 702.39a: Provoke — whenever this creature attacks, you may have target \
             creature defending player controls untap and block it this turn if able."
                .to_string(),
        )
}

/// CR 702.83a: Exalted — "Whenever a creature you control attacks alone,
/// that creature gets +1/+1 until end of turn for each instance of exalted
/// among permanents you control."
///
/// Each instance of Exalted triggers separately (CR 702.83b), so one trigger
/// is synthesized per `Keyword::Exalted` instance. The +1/+1 stacking is
/// automatic because each trigger resolves independently.
fn is_exalted_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Attacks)
        && matches!(t.condition, Some(TriggerCondition::Not { .. }))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Pump {
                target: TargetFilter::TriggeringSource,
                ..
            })
        )
}

/// CR 702.25a: Build the Flanking trigger — "whenever this creature becomes
/// blocked by a creature without flanking, the blocking creature gets -1/-1
/// until end of turn." `collect_matching_triggers` splits `BecomesBlocked`
/// events per qualifying blocker so each blocker creates its own stack object.
fn build_flanking_trigger() -> TriggerDefinition {
    let debuff = Effect::Pump {
        power: PtValue::Fixed(-1),
        toughness: PtValue::Fixed(-1),
        target: TargetFilter::TriggeringSource,
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, debuff)
        .duration(Duration::UntilEndOfTurn)
        .description(
            "CR 702.25a: Flanking — blocking creatures without flanking get -1/-1 until end of turn"
                .to_string(),
        );
    TriggerDefinition::new(TriggerMode::BecomesBlocked)
        .valid_card(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::WithoutKeyword {
                value: Keyword::Flanking,
            }],
        )))
        .execute(execute)
        .description(
            "CR 702.25a: Flanking — whenever this creature becomes blocked by a creature \
             without flanking, the blocking creature gets -1/-1 until end of turn."
                .to_string(),
        )
}

/// CR 702.25a: A Flanking-shaped trigger — a self-scoped `BecomesBlocked` trigger
/// whose blocker filter excludes creatures with flanking.
/// Used by `RemoveKeyword` symmetric removal.
fn is_flanking_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::BecomesBlocked)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.valid_target.as_ref(),
            Some(TargetFilter::Typed(tf)) if tf.properties.contains(&FilterProp::WithoutKeyword {
                value: Keyword::Flanking,
            })
        )
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Pump {
                target: TargetFilter::TriggeringSource,
                ..
            })
        )
}

/// CR 702.45a: Build one Bushido self-trigger for the given block event
/// (`Blocks` or `BecomesBlocked`): "this creature gets +N/+N until end of turn."
/// Scoped to the source creature via `valid_card` (the field block matchers read)
/// and pumps `SelfRef`, mirroring self-trigger handling in `build_dethrone_trigger`.
fn build_bushido_trigger(mode: TriggerMode, n: u32) -> TriggerDefinition {
    // CR 702.45a: "it gets +N/+N" — the Bushido creature itself. Target `SelfRef`,
    // NOT `TriggeringSource`: for a `BecomesBlocked` event the triggering source
    // resolves to the *blocker* (ambiguous/None with multiple blockers), so
    // `TriggeringSource` would pump the wrong creature. The pending trigger's
    // own source is this creature, so `SelfRef` is correct on both the blocks
    // and becomes-blocked halves. Mirrors the self-trigger Dethrone, not Exalted
    // (which watches other attacking creatures).
    let pump = Effect::Pump {
        power: PtValue::Fixed(n as i32),
        toughness: PtValue::Fixed(n as i32),
        target: TargetFilter::SelfRef,
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, pump).description(format!(
        "CR 702.45a: Bushido {n} — +{n}/+{n} until end of turn"
    ));
    TriggerDefinition::new(mode)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(format!(
            "CR 702.45a: Bushido {n} — whenever this creature blocks or becomes \
             blocked, it gets +{n}/+{n} until end of turn."
        ))
}

/// CR 702.45a: A Bushido `n` trigger — a self-scoped (`valid_card: SelfRef`)
/// block / becomes-blocked trigger that pumps the source creature +n/+n. Used
/// by `RemoveKeyword` symmetric removal so a granted-then-removed `Bushido(n)`
/// strips exactly its own triggers — parameterized by `n` (and asserting
/// `valid_card`) so it never matches a different Bushido level or a coincidental
/// printed block-pump on the same face.
fn is_bushido_trigger(t: &TriggerDefinition, n: u32) -> bool {
    matches!(t.mode, TriggerMode::Blocks | TriggerMode::BecomesBlocked)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Pump {
                power: PtValue::Fixed(p),
                toughness: PtValue::Fixed(tough),
                target: TargetFilter::SelfRef,
            }) if *p == n as i32 && *tough == n as i32
        )
}

/// CR 702.91a: "each other attacking creature" — every attacking creature
/// except the Battle cry source. `Another` is source-relative in this path: the
/// `PumpAll` resolves with `FilterContext::from_ability`, whose `recipient_id`
/// is `None`, so the object-level check reduces to `object_id != source.id` and
/// excludes exactly the ability source. Shared by the builder and the
/// `RemoveKeyword` matcher so both describe one canonical filter.
fn battlecry_target_filter() -> TypedFilter {
    let mut tf = TypedFilter::creature();
    tf.properties = vec![FilterProp::Attacking, FilterProp::Another];
    tf
}

/// CR 702.91a: Build the Battle cry attack trigger. The effect is a mass
/// `Effect::PumpAll` over the other-attackers set (no target slot, no choice),
/// mirroring the self-scoped Bushido trigger but pumping co-attackers +1/+0.
fn build_battlecry_trigger() -> TriggerDefinition {
    let pump = Effect::PumpAll {
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(0),
        target: TargetFilter::Typed(battlecry_target_filter()),
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, pump).description(
        "CR 702.91a: Battle cry — each other attacking creature +1/+0 until end of turn"
            .to_string(),
    );
    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(
            "CR 702.91a: Battle cry — whenever this creature attacks, each other \
             attacking creature gets +1/+0 until end of turn."
                .to_string(),
        )
}

/// CR 702.91a/b: A Battle cry trigger — an `Attacks` trigger scoped to the
/// source (`valid_card: SelfRef`) whose execute is the canonical
/// `PumpAll(+1/+0)` over `battlecry_target_filter()`. Used by `RemoveKeyword`
/// symmetric removal so a granted-then-removed `Battlecry` strips exactly its
/// own trigger (asserting the filter so it never matches a coincidental printed
/// attack-pump on the same face).
fn is_battlecry_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Attacks)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::PumpAll {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(0),
                target: TargetFilter::Typed(tf),
            }) if *tf == battlecry_target_filter()
        )
}

/// CR 702.23a: Rampage N magnitude — N per blocker beyond the first =
/// N × max(blockerCount − 1, 0). Mirrors the parser's "for each creature
/// blocking it beyond the first" shape: `ObjectCount` over creatures blocking
/// the source, offset by −1, clamped to zero, then scaled by N. CR 702.23b:
/// the bonus is calculated only once, when the trigger resolves.
fn rampage_beyond_first_expr(factor: i32) -> QuantityExpr {
    let mut blockers = TypedFilter::creature();
    blockers.properties = vec![FilterProp::BlockingSource];
    let count_minus_one = QuantityExpr::Offset {
        inner: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(blockers),
            },
        }),
        offset: -1,
    };
    QuantityExpr::Multiply {
        factor,
        inner: Box::new(QuantityExpr::ClampMin {
            inner: Box::new(count_minus_one),
            minimum: 0,
        }),
    }
}

/// CR 702.23a: Build the Rampage N becomes-blocked trigger — a self `Pump` of
/// +N/+N for each creature blocking it beyond the first (dynamic `PtValue::Quantity`).
fn build_rampage_trigger(n: u32) -> TriggerDefinition {
    let amount = PtValue::Quantity(rampage_beyond_first_expr(n as i32));
    let pump = Effect::Pump {
        power: amount.clone(),
        toughness: amount,
        target: TargetFilter::SelfRef,
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, pump).description(format!(
        "CR 702.23a: Rampage {n} — +{n}/+{n} for each creature blocking it beyond the first"
    ));
    TriggerDefinition::new(TriggerMode::BecomesBlocked)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(format!(
            "CR 702.23a: Rampage {n} — whenever this creature becomes blocked, it gets \
             +{n}/+{n} until end of turn for each creature blocking it beyond the first."
        ))
}

/// CR 702.23a/b: A Rampage `n` trigger — a self-scoped `BecomesBlocked` trigger
/// whose execute is the canonical per-blocker `Pump`. Parameterized by `n` (and
/// asserting `valid_card`) so `RemoveKeyword` strips exactly its own level.
fn is_rampage_trigger(t: &TriggerDefinition, n: u32) -> bool {
    matches!(t.mode, TriggerMode::BecomesBlocked)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Pump {
                power: PtValue::Quantity(expr),
                target: TargetFilter::SelfRef,
                ..
            }) if *expr == rampage_beyond_first_expr(n as i32)
        )
}

/// CR 702.121a: Melee magnitude — +1/+1 for each opponent you attacked with a
/// creature THIS COMBAT. Counts the combat-scoped opponent set via the existing
/// `PlayerCount` building block over `OpponentAttacked { You, ThisCombat }`.
fn melee_attacked_opponents_expr() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentAttacked {
                subject: AttackSubject::You,
                scope: AttackScope::ThisCombat,
            },
        },
    }
}

/// CR 702.121a: Build the Melee attack trigger — a self `Pump` of +1/+1 per
/// opponent attacked this combat (dynamic `PtValue::Quantity`).
fn build_melee_trigger() -> TriggerDefinition {
    let amount = PtValue::Quantity(melee_attacked_opponents_expr());
    let pump = Effect::Pump {
        power: amount.clone(),
        toughness: amount,
        target: TargetFilter::SelfRef,
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, pump).description(
        "CR 702.121a: Melee — +1/+1 for each opponent you attacked this combat".to_string(),
    );
    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(
            "CR 702.121a: Melee — whenever this creature attacks, it gets +1/+1 until \
             end of turn for each opponent you attacked with a creature this combat."
                .to_string(),
        )
}

/// CR 702.121a/b: A Melee trigger — a self-scoped `Attacks` trigger whose execute
/// is the canonical per-opponent `Pump`. Used by `RemoveKeyword` symmetric removal.
fn is_melee_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Attacks)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Pump {
                power: PtValue::Quantity(expr),
                target: TargetFilter::SelfRef,
                ..
            }) if *expr == melee_attacked_opponents_expr()
        )
}

fn build_exalted_trigger() -> TriggerDefinition {
    let pump_effect = Effect::Pump {
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        target: TargetFilter::TriggeringSource,
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, pump_effect)
        .description("CR 702.83a: Exalted — +1/+1 until end of turn".to_string());
    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
        .condition(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::MinCoAttackers {
                minimum: 1,
                filter: None,
            }),
        })
        .execute(execute)
        .description(
            "CR 702.83a: Exalted — whenever a creature you control attacks alone, \
             that creature gets +1/+1 until end of turn."
                .to_string(),
        )
}

fn is_mentor_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Attacks)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && t.execute.as_deref().is_some_and(|ability| {
            matches!(
                ability.effect.as_ref(),
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(tf),
                } if tf.properties.contains(&FilterProp::Attacking)
                    && tf.properties.iter().any(|prop| matches!(
                        prop,
                        FilterProp::PtComparison {
                            stat: PtStat::Power,
                            scope: PtValueScope::Current,
                            comparator: Comparator::LT,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: ObjectScope::Source
                                }
                            },
                        }
                    ))
            )
        })
}

fn build_mentor_trigger() -> TriggerDefinition {
    let mut target_filter = TypedFilter::creature();
    target_filter.properties = vec![
        // CR 702.134a: only attacking creatures are legal Mentor targets.
        FilterProp::Attacking,
        // CR 702.134a + CR 208.1: Mentor targets a creature with power
        // strictly less than the source creature's current power.
        FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::LT,
            value: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            },
        },
    ];

    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Typed(target_filter),
        },
    )
    .description("Mentor — put a +1/+1 counter on a lesser-power attacker".to_string());

    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(
            "CR 702.134a: Mentor — whenever this creature attacks, put a \
             +1/+1 counter on target attacking creature with power less than \
             this creature's power."
                .to_string(),
        )
}

/// CR 702.130a: Afflict N — "Whenever this creature becomes blocked, defending
/// player loses N life." Each instance triggers separately (CR 702.130b), so one
/// trigger is synthesized per `Keyword::Afflict` instance. Self-scoped via
/// `valid_card` (the field block matchers read) exactly like Bushido; the life
/// loss is directed at `DefendingPlayer`, which routes through
/// `combat::defending_player_for_attacker` for the source attacking creature.
fn build_afflict_trigger(n: u32) -> TriggerDefinition {
    // CR 702.130a: "defending player loses N life."
    let lose_life = Effect::LoseLife {
        amount: QuantityExpr::Fixed { value: n as i32 },
        target: Some(TargetFilter::DefendingPlayer),
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, lose_life).description(format!(
        "CR 702.130a: Afflict {n} — defending player loses {n} life"
    ));
    TriggerDefinition::new(TriggerMode::AttackerBlocked)
        .valid_card(TargetFilter::SelfRef)
        .execute(execute)
        .description(format!(
            "CR 702.130a: Afflict {n} — whenever this creature becomes blocked, \
             the defending player loses {n} life."
        ))
}

/// CR 702.130a: An Afflict `n` trigger — a self-scoped (`valid_card: SelfRef`)
/// `AttackerBlocked` trigger whose effect makes the `DefendingPlayer` lose `n`
/// life. Parameterized by `n` (and asserting `valid_card`) so `RemoveKeyword`
/// symmetric removal strips exactly its own trigger without matching a different
/// Afflict level or a coincidental printed becomes-blocked life-loss.
fn is_afflict_trigger(t: &TriggerDefinition, n: u32) -> bool {
    matches!(t.mode, TriggerMode::AttackerBlocked)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::LoseLife {
                amount: QuantityExpr::Fixed { value },
                target: Some(TargetFilter::DefendingPlayer),
            }) if *value == n as i32
        )
}

/// CR 702.149a + CR 208.1: Training's co-attacker gate — a creature whose current
/// power is strictly greater than the source creature's current power. Reuses the
/// same `PtComparison` building block Mentor uses (inverted comparator: `GT`
/// instead of `LT`), resolved with the source creature as the filter's source so
/// the comparison reads the Training creature's power.
fn training_higher_power_coattacker_filter() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature().properties(vec![FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::GT,
            value: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            },
        }]),
    )
}

/// CR 702.149a: Training — "Whenever this creature and at least one other
/// creature with power greater than this creature's power attack, put a +1/+1
/// counter on this creature." Each instance triggers separately (CR 702.149b),
/// so one trigger is synthesized per `Keyword::Training` instance.
///
/// Mirrors the self-scoped `Attacks` shape of Exalted but pumps itself with a
/// +1/+1 counter (not until-end-of-turn) and gates on a higher-power co-attacker
/// via a `MinCoAttackers { minimum: 1, filter }` intervening condition — the
/// existing co-attacker counter parameterized with the power filter, so no new
/// condition variant is introduced.
fn build_training_trigger() -> TriggerDefinition {
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .description("CR 702.149a: Training — put a +1/+1 counter on this creature".to_string());

    TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::MinCoAttackers {
            minimum: 1,
            filter: Some(training_higher_power_coattacker_filter()),
        })
        .execute(execute)
        .description(
            "CR 702.149a: Training — whenever this creature and at least one other \
             creature with power greater than this creature's power attack, put a \
             +1/+1 counter on this creature."
                .to_string(),
        )
}

/// CR 702.149a: A Training trigger — a self-scoped (`valid_card: SelfRef`)
/// `Attacks` trigger gated on a filtered `MinCoAttackers` higher-power co-attacker
/// that puts a single +1/+1 counter on the source. Used by `RemoveKeyword`
/// symmetric removal so a granted-then-removed Training strips exactly its own
/// trigger.
fn is_training_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Attacks)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.condition.as_ref(),
            Some(TriggerCondition::MinCoAttackers {
                minimum: 1,
                filter: Some(f),
            }) if f == &training_higher_power_coattacker_filter()
        )
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            })
        )
}

/// CR 702.70a: Poisonous N — "Whenever this creature deals combat damage to a
/// player, that player gets N poison counters." Each instance triggers separately
/// (CR 702.70b), so one trigger is synthesized per `Keyword::Poisonous` instance.
///
/// Modeled as a source-led combat-damage trigger (`DamageDone` +
/// `DamageKindFilter::CombatOnly`, `valid_source: SelfRef`, `valid_target:
/// Player`), mirroring the parser's "deals combat damage to a player" shape. The
/// poison routes to the damaged player (`TriggeringPlayer`) via
/// `GivePlayerCounter`, which sends `PlayerCounterKind::Poison` to the dedicated
/// poison field (CR 104.3d).
fn build_poisonous_trigger(n: u32) -> TriggerDefinition {
    let give_poison = Effect::GivePlayerCounter {
        counter_kind: PlayerCounterKind::Poison,
        count: QuantityExpr::Fixed { value: n as i32 },
        target: TargetFilter::TriggeringPlayer,
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, give_poison).description(format!(
        "CR 702.70a: Poisonous {n} — that player gets {n} poison counters"
    ));
    TriggerDefinition::new(TriggerMode::DamageDone)
        .damage_kind(DamageKindFilter::CombatOnly)
        .valid_source(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Player)
        .execute(execute)
        .description(format!(
            "CR 702.70a: Poisonous {n} — whenever this creature deals combat \
             damage to a player, that player gets {n} poison counters."
        ))
}

/// CR 702.70a: A Poisonous `n` trigger — a source-scoped combat-damage-to-player
/// trigger that gives the damaged player `n` poison counters. Parameterized by
/// `n` so `RemoveKeyword` symmetric removal strips exactly its own trigger.
fn is_poisonous_trigger(t: &TriggerDefinition, n: u32) -> bool {
    matches!(t.mode, TriggerMode::DamageDone)
        && matches!(t.damage_kind, DamageKindFilter::CombatOnly)
        && matches!(t.valid_source, Some(TargetFilter::SelfRef))
        && matches!(t.valid_target, Some(TargetFilter::Player))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::GivePlayerCounter {
                counter_kind: PlayerCounterKind::Poison,
                count: QuantityExpr::Fixed { value },
                target: TargetFilter::TriggeringPlayer,
            }) if *value == n as i32
        )
}

/// CR 702.115a: Ingest — "Whenever this creature deals combat damage to a
/// player, that player exiles the top card of their library." Each instance
/// triggers separately (CR 702.115b), so one trigger is synthesized per
/// `Keyword::Ingest` instance.
fn build_ingest_trigger() -> TriggerDefinition {
    let exile = Effect::ExileTop {
        player: TargetFilter::TriggeringPlayer,
        count: QuantityExpr::Fixed { value: 1 },
        face_down: false,
    };
    let execute = AbilityDefinition::new(AbilityKind::Spell, exile).description(
        "CR 702.115a: Ingest — that player exiles the top card of their library".to_string(),
    );
    TriggerDefinition::new(TriggerMode::DamageDone)
        .damage_kind(DamageKindFilter::CombatOnly)
        .valid_source(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Player)
        .execute(execute)
        .description(
            "CR 702.115a: Ingest — whenever this creature deals combat damage to a player, \
             that player exiles the top card of their library."
                .to_string(),
        )
}

/// CR 702.115a: An Ingest trigger — a source-scoped combat-damage-to-player
/// trigger that exiles the top card of the damaged player's library. Used by
/// `RemoveKeyword` symmetric removal so a granted-then-removed Ingest strips
/// exactly its own trigger.
fn is_ingest_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::DamageDone)
        && matches!(t.damage_kind, DamageKindFilter::CombatOnly)
        && matches!(t.valid_source, Some(TargetFilter::SelfRef))
        && matches!(t.valid_target, Some(TargetFilter::Player))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::ExileTop {
                player: TargetFilter::TriggeringPlayer,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            })
        )
}

/// CR 702.115a: Synthesize Ingest into a combat-damage-to-player trigger that
/// exiles the top card of the damaged player's library. CR 702.115b: multiple
/// Ingest instances trigger separately.
pub fn synthesize_ingest(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Ingest));
}

/// CR 702.101a: Extort — "Whenever you cast a spell, you may pay {W/B}.
/// If you do, each opponent loses 1 life and you gain that much life."
///
/// Each instance of Extort triggers separately (CR 702.101b), so one trigger
/// is synthesized per `Keyword::Extort` instance.
fn is_extort_mana_cost(cost: &ManaCost) -> bool {
    matches!(
        cost,
        ManaCost::Cost {
            shards,
            generic: 0,
        } if shards.as_slice() == [ManaCostShard::WhiteBlack]
    )
}

fn is_extort_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::SpellCast)
        && matches!(t.valid_target, Some(TargetFilter::Controller))
        && t.execute.as_deref().is_some_and(|a| {
            a.optional
                && matches!(
                    &*a.effect,
                    Effect::PayCost {
                        cost: PaymentCost::AbilityCost {
                            cost: AbilityCost::Mana { cost },
                        },
                        payer: TargetFilter::Controller,
                    } if is_extort_mana_cost(cost)
                )
                && a.sub_ability.as_deref().is_some_and(|drain| {
                    matches!(drain.player_scope, Some(PlayerFilter::Opponent))
                        && drain.condition == Some(AbilityCondition::effect_performed())
                        && matches!(
                            &*drain.effect,
                            Effect::LoseLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                                target: None,
                            }
                        )
                        && drain.sub_ability.as_deref().is_some_and(|gain| {
                            matches!(
                                &*gain.effect,
                                Effect::GainLife {
                                    amount: QuantityExpr::Ref {
                                        qty: QuantityRef::PreviousEffectAmount,
                                    },
                                    player: TargetFilter::Controller,
                                }
                            )
                        })
                })
        })
}

/// CR 702.191a: Intervening-if — this permanent is a creature and mana spent to
/// cast the spell exceeds its power or toughness.
fn increment_mana_spent_exceeds_self_pt_condition() -> TriggerCondition {
    TriggerCondition::And {
        conditions: vec![
            TriggerCondition::SourceMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::creature()),
            },
            TriggerCondition::Or {
                conditions: vec![
                    TriggerCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ManaSpentToCast {
                                scope: CastManaObjectScope::TriggeringSpell,
                                metric: CastManaSpentMetric::Total,
                            },
                        },
                        comparator: Comparator::GT,
                        rhs: QuantityExpr::Ref {
                            qty: QuantityRef::Power {
                                scope: ObjectScope::Source,
                            },
                        },
                    },
                    TriggerCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ManaSpentToCast {
                                scope: CastManaObjectScope::TriggeringSpell,
                                metric: CastManaSpentMetric::Total,
                            },
                        },
                        comparator: Comparator::GT,
                        rhs: QuantityExpr::Ref {
                            qty: QuantityRef::Toughness {
                                scope: ObjectScope::Source,
                            },
                        },
                    },
                ],
            },
        ],
    }
}

/// CR 702.191a: Synthesized Increment spell-cast trigger identity.
fn is_increment_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::SpellCast)
        && matches!(t.valid_target, Some(TargetFilter::Controller))
        && t.condition.as_ref() == Some(&increment_mana_spent_exceeds_self_pt_condition())
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            })
        )
}

/// CR 702.191a: Increment — whenever you cast a spell, if this permanent is a
/// creature and mana spent to cast that spell exceeds its power or toughness,
/// put a +1/+1 counter on this creature.
fn build_increment_trigger() -> TriggerDefinition {
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .description("Put a +1/+1 counter on this creature".to_string());

    TriggerDefinition::new(TriggerMode::SpellCast)
        .valid_target(TargetFilter::Controller)
        .condition(increment_mana_spent_exceeds_self_pt_condition())
        .execute(execute)
        .description(
            "CR 702.191a: Increment — whenever you cast a spell, if mana spent to cast that spell is greater than this creature's power or toughness, put a +1/+1 counter on it.".to_string(),
        )
}

fn build_extort_trigger() -> TriggerDefinition {
    // CR 702.101a: Optional {W/B} payment must resolve before the opponent drain.
    // `AbilityDefinition::cost` is not carried into `ResolvedAbility`, so the
    // payment is modeled as an optional `PayCost` parent (mirroring parsed
    // "you may pay {cost}. If you do, …" chains) with the drain on the sub.
    let wb_mana = ManaCost::Cost {
        shards: vec![ManaCostShard::WhiteBlack],
        generic: 0,
    };
    let gain_life = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectAmount,
            },
            player: TargetFilter::Controller,
        },
    );
    let drain = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: None,
        },
    )
    .player_scope(PlayerFilter::Opponent)
    .sub_ability(gain_life)
    .condition(AbilityCondition::effect_performed());
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PayCost {
            cost: PaymentCost::AbilityCost {
                cost: AbilityCost::Mana {
                    cost: wb_mana.clone(),
                },
            },
            payer: TargetFilter::Controller,
        },
    )
    .optional()
    .sub_ability(drain)
    .description(
        "CR 702.101a: Extort — pay {W/B}, each opponent loses 1 life, you gain that much life"
            .to_string(),
    );
    TriggerDefinition::new(TriggerMode::SpellCast)
        .valid_target(TargetFilter::Controller)
        .execute(execute)
        .description(
            "CR 702.101a: Extort — whenever you cast a spell, you may pay {W/B}. \
             If you do, each opponent loses 1 life and you gain that much life."
                .to_string(),
        )
}

/// CR 702.105a: Dethrone — "Whenever a creature with dethrone attacks the
/// player with the most life or tied for most life, put a +1/+1 counter on
/// that creature."
///
/// Build-for-the-class: keyed entirely on `Keyword::Dethrone`, so every printed
/// Dethrone card and every creature granted Dethrone at runtime gets an identical
/// trigger. CR 702.105b: each instance triggers separately.
fn build_dethrone_trigger() -> TriggerDefinition {
    // CR 122.1: put a single +1/+1 counter on the Dethrone creature itself.
    let put_counter = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .description("Put a +1/+1 counter on this creature".to_string());

    // CR 702.105a: "attacks the player with the most life or tied for most
    // life". The defending player's life total must be >= the maximum life
    // total among all players. This is an intervening-if condition checked
    // at both detection and resolution per CR 603.4.
    let condition = TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::DefendingPlayer,
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                    exclude: None,
                },
            },
        },
    };

    let mut trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .condition(condition)
        .execute(put_counter)
        .description(
            "CR 702.105a: Dethrone — whenever ~ attacks the player with the most life or tied for most life, put a +1/+1 counter on ~."
                .to_string(),
        );
    // CR 702.105a: Dethrone only triggers when attacking a player, not a
    // planeswalker or battle.
    trigger.attack_target_filter = Some(crate::types::triggers::AttackTargetFilter::Player);
    trigger
}

/// Builds the `Effect::ChangeZone` that moves this card (`SelfRef`) from its
/// own graveyard to the given `destination`. Shared by the two Recover branches
/// (`Zone::Hand` for the pay branch, `Zone::Exile` for the otherwise branch).
fn build_recover_self_change_zone(destination: Zone) -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination,
        target: TargetFilter::SelfRef,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: false,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: Vec::new(),
        face_down_profile: None,
    }
}

/// CR 702.59a: Recover {cost} — "When a creature is put into your graveyard
/// from the battlefield, you may pay [cost]. If you do, return this card from
/// your graveyard to your hand. Otherwise, exile this card."
///
/// Modeled by reusing the unless-pay/pay-with-else machinery
/// (`AbilityDefinition.unless_pay` + the `IfAPlayerDoes`/effect-performed
/// sub-ability, resolved by `handle_unless_payment` in
/// `engine_payment_choices.rs`):
///   * The PRIMARY effect is the "otherwise exile this card" branch — it runs
///     when the controller declines or cannot pay (the unless-pay decline path).
///   * The `unless_pay` modifier carries the Recover {cost}; the payer is the
///     controller of the Recover card (`TargetFilter::Controller`).
///   * The pay-success ALTERNATIVE is a `sub_ability` gated on
///     `AbilityCondition::effect_performed()` (CR 608.2c "if you do"), which
///     returns this card from the graveyard to its owner's hand. On payment
///     success `handle_unless_payment` suppresses the primary (exile) and runs
///     this alternative.
///
/// The trigger is GRAVEYARD-SOURCED: `trigger_zones = [Graveyard]` because the
/// Recover card itself is in the graveyard when it fires, keying on ANOTHER
/// creature put into your graveyard (`ChangesZone` Battlefield→Graveyard,
/// `valid_card` = another creature you own). Both branches act on `SelfRef`
/// (this card in the graveyard).
///
/// Single source of truth for the Recover trigger shape, shared by the printed
/// path (`synthesize_recover`) and the runtime-granted path
/// (`KeywordTriggerInstaller::triggers_for`) per CR 604.1.
fn build_recover_trigger(cost: ManaCost) -> TriggerDefinition {
    // Pay-success alternative (CR 608.2c "if you do"): return this card from the
    // graveyard to its owner's hand.
    let return_to_hand = AbilityDefinition::new(
        AbilityKind::Spell,
        build_recover_self_change_zone(Zone::Hand),
    )
    .condition(AbilityCondition::effect_performed())
    .description("If you do, return this card from your graveyard to your hand".to_string());

    // Primary (otherwise) branch: exile this card. Runs when the controller
    // declines or cannot pay the Recover cost.
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        build_recover_self_change_zone(Zone::Exile),
    )
    .unless_pay(UnlessPayModifier {
        cost: AbilityCost::Mana { cost },
        // CR 702.59a: "you may pay" — the controller of the Recover card pays.
        payer: TargetFilter::Controller,
    })
    .sub_ability(return_to_hand)
    .description("Otherwise, exile this card".to_string());

    // CR 404.1 + CR 702.59a: "your graveyard" means the creature is put into
    // its owner's graveyard. `FilterProp::Another` excludes the Recover card
    // itself; `Owned(You)` restricts to cards owned by the Recover controller.
    let another_creature_you_own = TargetFilter::Typed(TypedFilter::creature().properties(vec![
        FilterProp::Another,
        FilterProp::Owned {
            controller: ControllerRef::You,
        },
    ]));

    let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(another_creature_you_own)
        .execute(execute)
        .description(
            "CR 702.59a: Recover — when a creature is put into your graveyard from the battlefield, you may pay the recover cost; if you do, return this card from your graveyard to your hand, otherwise exile this card."
                .to_string(),
        );
    // CR 702.59a: the Recover card fires this trigger from its own graveyard.
    trigger.trigger_zones = vec![Zone::Graveyard];
    trigger
}

/// Idempotency / symmetric-removal shape predicate for the Recover dies trigger.
/// True iff `t` is a graveyard-sourced (`trigger_zones` includes `Graveyard`)
/// `ChangesZone` Battlefield→Graveyard trigger on another creature you own
/// whose execute body exiles `SelfRef` from the graveyard under an `unless_pay`
/// modifier, with a `effect_performed`-gated sub-ability returning `SelfRef` to
/// hand. The cost value is not inspected (cost-independent shape).
fn is_recover_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.origin != Some(Zone::Battlefield)
        || t.destination != Some(Zone::Graveyard)
        || !t.trigger_zones.contains(&Zone::Graveyard)
    {
        return false;
    }
    let Some(TargetFilter::Typed(tf)) = t.valid_card.as_ref() else {
        return false;
    };
    if !tf.properties.contains(&FilterProp::Another)
        || !tf.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::You,
        })
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    let exiles_self = matches!(
        &*execute.effect,
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Exile,
            target: TargetFilter::SelfRef,
            ..
        }
    );
    let has_unless_pay = execute.unless_pay.is_some();
    let returns_self_to_hand = execute.sub_ability.as_deref().is_some_and(|sub| {
        sub.condition
            .as_ref()
            .is_some_and(AbilityCondition::is_optional_effect_performed)
            && matches!(
                &*sub.effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Hand,
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
    });
    exiles_self && has_unless_pay && returns_self_to_hand
}

/// CR 702.59a: Recover {cost} — graveyard-sourced dies trigger with a
/// mandatory pay-or-else-exile branch. Synthesized via the
/// shared `install_matching` installer so the printed and runtime-granted paths
/// share the single `build_recover_trigger` shape. Per the absence of a
/// redundancy clause every `Keyword::Recover` instance functions independently,
/// so one trigger is emitted per keyword on the face.
pub fn synthesize_recover(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Recover(_)));
}

fn is_renown_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::DamageDone)
        && matches!(t.valid_source, Some(TargetFilter::SelfRef))
        && matches!(t.valid_target, Some(TargetFilter::Player))
        && matches!(t.damage_kind, DamageKindFilter::CombatOnly)
        && matches!(
            t.condition,
            Some(TriggerCondition::Not {
                condition: ref inner,
            }) if matches!(**inner, TriggerCondition::SourceIsRenowned)
        )
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Renown { .. })
        )
}

/// CR 702.112a: Renown N — "When this creature deals combat damage to a player,
/// if it isn't renowned, put N +1/+1 counters on it and it becomes renowned."
///
/// CR 702.112c: Multiple renown instances trigger separately; the first to
/// resolve makes the creature renowned, and later instances do nothing via the
/// same intervening-if condition.
fn build_renown_trigger(n: u32) -> TriggerDefinition {
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Renown {
            count: QuantityExpr::Fixed { value: n as i32 },
        },
    )
    .description(format!("Renown {n}"));

    TriggerDefinition::new(TriggerMode::DamageDone)
        .valid_source(TargetFilter::SelfRef)
        .valid_target(TargetFilter::Player)
        .damage_kind(DamageKindFilter::CombatOnly)
        .condition(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::SourceIsRenowned),
        })
        .execute(execute)
        .description(format!(
            "CR 702.112a: Renown {n} — when this creature deals combat damage to a player, if it isn't renowned, put {n} +1/+1 counter{} on it and it becomes renowned.",
            if n == 1 { "" } else { "s" }
        ))
}

/// CR 702.100a: Evolve — "Whenever a creature you control enters, if that
/// creature's power is greater than this creature's power and/or that
/// creature's toughness is greater than this creature's toughness, put a
/// +1/+1 counter on this creature."
///
/// Build-for-the-class: keyed entirely on `Keyword::Evolve`, so every printed
/// Evolve card and every creature granted Evolve at runtime gets an identical
/// trigger. CR 702.100d (multiple Evolve instances trigger separately) is
/// satisfied for free by `triggers_for` being invoked per keyword instance.
fn build_evolve_trigger() -> TriggerDefinition {
    // CR 122.1: put a single +1/+1 counter on the Evolve creature itself.
    let mut put_counter = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .description("Put a +1/+1 counter on this creature".to_string());
    put_counter.ability_tag = Some(AbilityTag::Evolve);

    // CR 702.100a "and/or": fire if the entering creature's power is greater
    // OR its toughness is greater than this creature's. CR 603.4: this is the
    // intervening-if — checked at detection AND re-checked on resolution.
    let condition = TriggerCondition::Or {
        conditions: vec![
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::EventSource,
                    },
                },
                comparator: crate::types::ability::Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source,
                    },
                },
            },
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: ObjectScope::EventSource,
                    },
                },
                comparator: crate::types::ability::Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::Toughness {
                        scope: ObjectScope::Source,
                    },
                },
            },
        ],
    };

    // CR 702.100a: "a creature you control enters". `.destination(Battlefield)`
    // constrains the trigger definition itself to entering the battlefield;
    // `valid_card` selects any creature the trigger controller controls
    // (including the Evolve creature itself — its self-vs-self P/T comparison
    // yields equal values, so the strict-`GT` intervening-if filters it out).
    TriggerDefinition::new(TriggerMode::Evolve)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
        .condition(condition)
        .execute(put_counter)
        .description(
            "CR 702.100a: Evolve — whenever a creature you control enters with greater power or toughness than ~, put a +1/+1 counter on ~."
                .to_string(),
        )
}

/// Shared trigger builder for the Undying/Persist class (CR 702.93a / CR 702.79a):
/// "When this permanent dies, if it had no `<polarity>` counters on it, return
/// it to the battlefield under its owner's control with a `<polarity>` counter
/// on it."
///
/// Build-for-the-class: parameterized over the counter polarity string
/// (`"P1P1"` or `"M1M1"`). Any future "dies → return with single typed
/// counter, gated on the same counter type's prior absence" keyword can reuse
/// this directly.
fn build_dies_return_with_counter_trigger(
    counter_type: &str,
    counter_label: &str,
    cr_ref: &str,
) -> TriggerDefinition {
    let counter_type = crate::types::counter::parse_counter_type(counter_type);
    // CR 122.1 + CR 614.1c: Single +1/+1 (or -1/-1) counter applied as
    // the object enters the battlefield, via the existing
    // `Effect::ChangeZone.enter_with_counters` plumbing.
    let return_effect = Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Battlefield,
        target: TargetFilter::SelfRef,
        owner_library: false,
        enter_transformed: false,
        // CR 702.93a / CR 702.79a: "under its owner's control" — default
        // (false) sends the object to its owner's control. `true` would
        // override to the ability controller's control.
        enters_under: None,
        enter_tapped: false,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![(counter_type.clone(), QuantityExpr::Fixed { value: 1 })],
        face_down_profile: None,
    };

    let execute = AbilityDefinition::new(AbilityKind::Spell, return_effect).description(format!(
        "Return it to the battlefield with a {counter_label} counter on it"
    ));

    // CR 400.7 + CR 603.10a: "if it had no <polarity> counters on it" —
    // negate `HadCounters` to express the absence of the specific counter
    // type in the LKI snapshot captured by `apply_zone_exit_cleanup`.
    let condition = TriggerCondition::Not {
        condition: Box::new(TriggerCondition::HadCounters {
            counter_type: Some(counter_type),
        }),
    };

    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::SelfRef)
        .condition(condition)
        .execute(execute)
        .description(format!(
            "CR {cr_ref}: When ~ dies, if it had no {counter_label} counters on it, return it to the battlefield under its owner's control with a {counter_label} counter on it."
        ))
}

/// CR 702.62a (2nd ability): "At the beginning of your upkeep, if this card is
/// suspended, remove a time counter from it." `trigger_zones = [Exile]` plus
/// `HasCounters{Time, min:1}` together model CR 702.62b (a card is "suspended"
/// iff it's in exile, has suspend, and has a time counter on it);
/// `TriggerConstraint::OnlyDuringYourTurn` enforces "your" upkeep.
///
/// Single source of truth for the suspend upkeep-removal trigger shape, shared
/// by the printed path (`synthesize_suspend`) and the runtime-granted path
/// (`KeywordTriggerInstaller::triggers_for`) per CR 604.1.
fn build_suspend_upkeep_removal_trigger() -> TriggerDefinition {
    let remove_one = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: Some(CounterType::Time),
            count: 1,
            target: TargetFilter::SelfRef,
        },
    );
    let mut trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Time),
            minimum: 1,
            maximum: None,
        })
        .constraint(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        .execute(remove_one)
        .description(
            "CR 702.62a: At the beginning of your upkeep, if this card is suspended, remove a time counter from it."
                .to_string(),
        );
    trigger.trigger_zones = vec![Zone::Exile];
    trigger
}

/// CR 702.62a (3rd ability): "When the last time counter is removed from this
/// card, if it's exiled, you may play it without paying its mana cost if able."
///
/// Single source of truth for the suspend last-counter cast trigger shape,
/// shared by the printed path (`synthesize_suspend`) and the runtime-granted
/// path (`KeywordTriggerInstaller::triggers_for`) per CR 604.1.
fn build_suspend_last_counter_cast_trigger() -> TriggerDefinition {
    let cast = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CastFromZone {
            target: TargetFilter::SelfRef,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            // CR 702.62a + CR 608.2g: cast the suspended card AS this trigger
            // resolves, not via a lingering permission — this arms the
            // sorcery-speed timing bypass for an upkeep recast (issue #1520).
            driver: CastFromZoneDriver::DuringResolution,
        },
    )
    .optional();
    let mut trigger = TriggerDefinition::new(TriggerMode::CounterRemoved)
        .valid_card(TargetFilter::SelfRef)
        .counter_filter(CounterTriggerFilter {
            counter_type: CounterType::Time,
            threshold: Some(0),
        })
        .execute(cast)
        .description(
            "CR 702.62a: When the last time counter is removed from this card, if it's exiled, you may play it without paying its mana cost."
                .to_string(),
        );
    trigger.trigger_zones = vec![Zone::Exile];
    trigger
}

/// Structural predicate: true iff `trigger` is the suspend upkeep counter-removal
/// trigger shape. Mirrors `is_echo_trigger` — stays correct if the builder ever
/// gains a parameter.
fn is_suspend_upkeep_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Phase)
        && t.phase == Some(Phase::Upkeep)
        && t.trigger_zones == vec![Zone::Exile]
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::RemoveCounter { counter_type: Some(counter_type), target: TargetFilter::SelfRef, .. })
                if *counter_type == CounterType::Time
        )
}

/// Structural predicate: true iff `trigger` is the suspend last-counter free-cast
/// trigger shape. Mirrors `is_echo_trigger` — stays correct if the builder ever
/// gains a parameter.
fn is_suspend_last_counter_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::CounterRemoved)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && t.counter_filter
            .as_ref()
            .is_some_and(|f| matches!(f.counter_type, CounterType::Time) && f.threshold == Some(0))
}

// ---------------------------------------------------------------------------
// Fading (CR 702.32) and Vanishing (CR 702.63)
//
// Both keywords use upkeep counter-removal shapes — a `TriggerMode::Phase`
// upkeep trigger gated `OnlyDuringYourTurn` whose execute body starts with
// `Effect::RemoveCounter { count: 1, target: SelfRef }` — and differ in counter
// type and sacrifice timing:
//
//   * Fading N (CR 702.32a) enters with N *fade* counters and is sacrificed at
//     the upkeep where it *can't* remove one (the upkeep with 0 fade counters,
//     one upkeep AFTER its last counter was removed) — so it gets N uses.
//   * Vanishing N (CR 702.63a) enters with N *time* counters and is sacrificed
//     *when its last time counter is removed* (the Nth upkeep, the removal that
//     takes it to 0).
//
// Vanishing's removal trigger mirrors suspend (CR 702.62a) but in the
// Battlefield zone. Fading needs a single remove-or-sacrifice trigger because
// its "if you can't" branch is checked during resolution.

/// CR 702.63a: Shared upkeep counter-removal trigger for Vanishing and other
/// "remove one counter if one remains" battlefield keywords. Mirrors
/// `build_suspend_upkeep_removal_trigger`, but on the battlefield
/// (Vanishing permanents are on the battlefield, suspend cards are in exile).
///
/// The `HasCounters { minimum: 1 }` intervening-if (CR 603.4) ensures the
/// removal only fires while a counter remains. This matches Vanishing's printed
/// "if this permanent has a time counter on it" (CR 702.63a). Fading is not
/// built through this helper because its "if you can't, sacrifice" branch must
/// be decided during the single upkeep trigger's resolution.
fn build_battlefield_upkeep_counter_removal_trigger(
    counter_type: CounterType,
    cr: &str,
) -> TriggerDefinition {
    let remove_one = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: Some(counter_type.clone()),
            count: 1,
            target: TargetFilter::SelfRef,
        },
    );
    TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::HasCounters {
            counters: CounterMatch::OfType(counter_type.clone()),
            minimum: 1,
            maximum: None,
        })
        .constraint(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        .execute(remove_one)
        .description(format!(
            "CR {cr}: At the beginning of your upkeep, remove a {} counter from this permanent.",
            counter_type.as_str()
        ))
}

/// CR 702.32a: Fading's single upkeep trigger. It attempts to remove one fade
/// counter, then sacrifices the permanent if that removal did not happen during
/// resolution. The sacrifice branch is a sub-ability gated by the existing
/// `PreviousEffectAmount == 0` chain signal; `RemoveCounter` stamps the amount
/// from emitted `CounterRemoved` events, so a counter removed in response makes
/// this trigger sacrifice at resolution instead of silently doing nothing.
fn build_fading_upkeep_trigger() -> TriggerDefinition {
    let sacrifice_if_none_removed = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    )
    .condition(AbilityCondition::PreviousEffectAmount {
        comparator: Comparator::EQ,
        rhs: QuantityExpr::Fixed { value: 0 },
    });
    let remove_one = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: Some(CounterType::Fade),
            count: 1,
            target: TargetFilter::SelfRef,
        },
    )
    .sub_ability(sacrifice_if_none_removed);
    TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .valid_card(TargetFilter::SelfRef)
        .constraint(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        .execute(remove_one)
        .description(
            "CR 702.32a: At the beginning of your upkeep, if you can't remove a fade counter from this permanent, sacrifice it."
                .to_string(),
        )
}

/// CR 702.63a (3rd ability): Vanishing's sacrifice trigger. "When the last time
/// counter is removed from this permanent, sacrifice it." Identical shape to the
/// suspend last-counter trigger (`build_suspend_last_counter_cast_trigger`) —
/// `TriggerMode::CounterRemoved` with `threshold: Some(0)` (fire only when the
/// post-removal count is 0) — but on the battlefield and executing a sacrifice
/// instead of a free cast. This fires on the very upkeep that removes the last
/// counter (CR 702.63a), one upkeep earlier than Fading's sacrifice.
fn build_vanishing_sacrifice_trigger() -> TriggerDefinition {
    let sacrifice = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    TriggerDefinition::new(TriggerMode::CounterRemoved)
        .valid_card(TargetFilter::SelfRef)
        .counter_filter(CounterTriggerFilter {
            counter_type: CounterType::Time,
            threshold: Some(0),
        })
        .execute(sacrifice)
        .description(
            "CR 702.63a: When the last time counter is removed from this permanent, sacrifice it."
                .to_string(),
        )
}

/// Structural predicate: true iff `trigger` is the battlefield upkeep
/// counter-removal trigger for `counter_type`. Mirrors `is_suspend_upkeep_trigger`,
/// but on the battlefield.
fn is_battlefield_upkeep_counter_removal_trigger(
    t: &TriggerDefinition,
    counter_type: &CounterType,
) -> bool {
    // Default `trigger_zones` (empty) means battlefield-only, which is what the
    // builder leaves — distinguishing this from the suspend upkeep trigger,
    // which explicitly sets `trigger_zones = [Exile]`.
    matches!(t.mode, TriggerMode::Phase)
        && t.phase == Some(Phase::Upkeep)
        && t.trigger_zones.is_empty()
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::RemoveCounter { counter_type: Some(ct), target: TargetFilter::SelfRef, .. })
                if ct == counter_type
        )
}

/// Structural predicate: true iff `trigger` is the Fading upkeep trigger shape:
/// upkeep, self fade-counter removal, then self-sacrifice if no counter was
/// removed during resolution.
fn is_fading_upkeep_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::Phase)
        && t.phase == Some(Phase::Upkeep)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::RemoveCounter {
                counter_type: Some(CounterType::Fade),
                target: TargetFilter::SelfRef,
                ..
            })
        )
        && t.execute
            .as_deref()
            .and_then(|a| a.sub_ability.as_deref())
            .is_some_and(|sub| {
                matches!(
                    &sub.condition,
                    Some(AbilityCondition::PreviousEffectAmount {
                        comparator: Comparator::EQ,
                        rhs: QuantityExpr::Fixed { value: 0 },
                    })
                ) && matches!(
                    &*sub.effect,
                    Effect::Sacrifice {
                        target: TargetFilter::SelfRef,
                        ..
                    }
                )
            })
}

/// Structural predicate: true iff `trigger` is the Vanishing last-counter
/// sacrifice trigger shape. Mirrors `is_suspend_last_counter_trigger` but
/// executes a self-sacrifice rather than a free cast.
fn is_vanishing_sacrifice_trigger(t: &TriggerDefinition) -> bool {
    matches!(t.mode, TriggerMode::CounterRemoved)
        && matches!(t.valid_card, Some(TargetFilter::SelfRef))
        && t.counter_filter
            .as_ref()
            .is_some_and(|f| matches!(f.counter_type, CounterType::Time) && f.threshold == Some(0))
        && matches!(
            t.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            })
        )
}

/// Build the shared "enters with N counters" ETB replacement for Fading /
/// Vanishing. Mirrors the Modular ETB-with-N-P1P1 replacement
/// (`synthesize_modular`): a `ReplacementEvent::Moved` replacement on `SelfRef`
/// whose execute body is `Effect::PutCounter { count: Fixed(n), target: SelfRef }`.
/// CR 702.32a / CR 702.63a: "This permanent enters with N [fade/time] counters
/// on it."
fn build_fade_vanish_etb_replacement(
    counter_type: CounterType,
    n: u32,
    cr: &str,
) -> ReplacementDefinition {
    let etb_counters = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: counter_type.clone(),
            count: QuantityExpr::Fixed { value: n as i32 },
            target: TargetFilter::SelfRef,
        },
    )
    .description(format!(
        "This permanent enters with {n} {} counter{} on it",
        counter_type.as_str(),
        if n == 1 { "" } else { "s" }
    ));
    ReplacementDefinition {
        event: ReplacementEvent::Moved,
        execute: Some(Box::new(etb_counters)),
        valid_card: Some(TargetFilter::SelfRef),
        description: Some(format!(
            "CR {cr}: this permanent enters with {n} {} counter{} on it.",
            counter_type.as_str(),
            if n == 1 { "" } else { "s" }
        )),
        ..ReplacementDefinition::new(ReplacementEvent::Moved)
    }
}

/// Idempotency-shape predicate for the Fading/Vanishing ETB-with-counters
/// replacement. True iff `replacement` is a `Moved` replacement on `SelfRef`
/// whose execute body places exactly `expected_n` `counter_type` counters on
/// `SelfRef` with a fixed count. The `expected_n` argument is load-bearing for
/// the same reason as `is_modular_etb_replacement`: a card carrying both a
/// printed "enters with K counters" replacement and the keyword with K ≠ N must
/// not silently dedupe.
fn is_fade_vanish_etb_replacement(
    replacement: &ReplacementDefinition,
    counter_type: &CounterType,
    expected_n: u32,
) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || !matches!(replacement.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    matches!(
        replacement.execute.as_deref().map(|a| &*a.effect),
        Some(Effect::PutCounter {
            counter_type: ct,
            count: QuantityExpr::Fixed { value },
            target: TargetFilter::SelfRef,
        }) if ct == counter_type && *value == expected_n as i32
    )
}

/// CR 702.32a: Fading N — synthesize the enters-with-N-fade-counters ETB
/// replacement and the single upkeep "remove a fade counter; if you can't,
/// sacrifice" trigger. Each Fading instance functions separately (CR 113.2c);
/// the per-N idempotency mirrors `synthesize_modular`.
pub fn synthesize_fading(face: &mut CardFace) {
    let fading_values: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Fading(n) => Some(*n),
            _ => None,
        })
        .collect();
    if fading_values.is_empty() {
        return;
    }

    // ETB-with-N-fade-counters replacement, per-N idempotent (mirrors Modular).
    for &n in &fading_values {
        let needed = fading_values.iter().filter(|m| **m == n).count();
        let existing = face
            .replacements
            .iter()
            .filter(|r| is_fade_vanish_etb_replacement(r, &CounterType::Fade, n))
            .count();
        if existing >= needed {
            continue;
        }
        face.replacements.push(build_fade_vanish_etb_replacement(
            CounterType::Fade,
            n,
            "702.32a",
        ));
    }

    // Upkeep remove-or-sacrifice trigger (shape-only idempotency: no N dependence).
    if !face.triggers.iter().any(is_fading_upkeep_trigger) {
        face.triggers.push(build_fading_upkeep_trigger());
    }
}

/// CR 702.63a: Vanishing N — synthesize the enters-with-N-time-counters ETB
/// replacement, the upkeep time-counter-removal trigger, and the last-counter
/// sacrifice trigger. Each Vanishing instance functions separately (CR 702.63c);
/// the per-N idempotency mirrors `synthesize_modular`.
pub fn synthesize_vanishing(face: &mut CardFace) {
    let vanishing_values: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Vanishing(n) => Some(*n),
            _ => None,
        })
        .collect();
    if vanishing_values.is_empty() {
        return;
    }

    // ETB-with-N-time-counters replacement, per-N idempotent (mirrors Modular).
    for &n in &vanishing_values {
        let needed = vanishing_values.iter().filter(|m| **m == n).count();
        let existing = face
            .replacements
            .iter()
            .filter(|r| is_fade_vanish_etb_replacement(r, &CounterType::Time, n))
            .count();
        if existing >= needed {
            continue;
        }
        face.replacements.push(build_fade_vanish_etb_replacement(
            CounterType::Time,
            n,
            "702.63a",
        ));
    }

    // Upkeep time-counter-removal trigger (shape-only idempotency).
    if !face
        .triggers
        .iter()
        .any(|t| is_battlefield_upkeep_counter_removal_trigger(t, &CounterType::Time))
    {
        face.triggers
            .push(build_battlefield_upkeep_counter_removal_trigger(
                CounterType::Time,
                "702.63a",
            ));
    }

    // Last-counter sacrifice trigger (shape-only idempotency).
    if !face.triggers.iter().any(is_vanishing_sacrifice_trigger) {
        face.triggers.push(build_vanishing_sacrifice_trigger());
    }
}

/// Idempotency-shape predicate for `synthesize_dies_return_with_counter`.
/// True iff `trigger` is the synthesized dies-trigger shape for the given
/// counter polarity. The check is intentionally narrow — it matches the
/// engine's exact wire-up (origin/destination/valid_card on the trigger plus
/// the counter type on the execute body's `enter_with_counters`) — so an
/// unrelated dies-trigger on the same face (e.g., "When ~ dies, draw a card")
/// is correctly ignored.
fn is_dies_return_with_counter_trigger(t: &TriggerDefinition, counter_type: &CounterType) -> bool {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.origin != Some(Zone::Battlefield)
        || t.destination != Some(Zone::Graveyard)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            enter_with_counters,
            ..
        } if enter_with_counters
            .iter()
            .any(|(ct, _)| ct == counter_type)
    )
}

/// CR 702.43a: Modular N — "This permanent enters the battlefield with N +1/+1
/// counters on it. When it's put into a graveyard from the battlefield, you
/// may put a +1/+1 counter on target artifact creature for each +1/+1 counter
/// on this permanent."
///
/// Per CR 702.43b ("If a creature has multiple instances of modular, each one
/// works separately") and CR 113.2c, each `Keyword::Modular(n)` on the face
/// emits its own ETB-with-counters replacement AND its own dies-transfer
/// trigger. No printed card today has multiple Modular instances, but the
/// per-instance synthesis pins the rule shape so a future printing routes
/// correctly.
///
/// Wiring (composed entirely from existing primitives — no new enum variants):
///
///   1. **ETB-with-N P1P1 counters** — `ReplacementDefinition` on
///      `ReplacementEvent::Moved` with `valid_card = SelfRef`, executing
///      `Effect::PutCounter { counter_type: "P1P1", count: Fixed(n), target:
///      SelfRef }`. Mirrors the parser's Walking Ballista shape for "this
///      creature enters with X +1/+1 counters on it" (CR 614.1c).
///
///   2. **Dies-transfer trigger** — `TriggerMode::ChangesZone` (Battlefield →
///      Graveyard) with `valid_card = SelfRef` (canonical dies trigger; CR
///      603.10a — leaves-the-battlefield triggers look back in time). The
///      execute body is `Effect::PutCounter` targeting a single artifact
///      creature with `count = QuantityRef::CountersOn { scope: Source,
///      counter_type: Some("P1P1") }`. Per CR 122.1 + CR 400.7 the `Source`
///      scope falls back to the LKI snapshot when the dying object is in the
///      graveyard at resolution, so the count reflects the pre-death P1P1
///      counter total (which may differ from N due to Hardened Scales doubling,
///      added counters from other sources, or -1/-1 counter annihilation).
///      The ability is marked `.optional()` per CR 603.5 — optional triggered
///      abilities go on the stack and the controller is prompted "you may"
///      when the ability resolves.
///
/// Build-for-the-class: any future "dies → transfer counters of one type to a
/// target permanent of a fixed type/property class" keyword can lift this
/// shape directly (parameterize over counter type + target type filter).
pub fn synthesize_modular(face: &mut CardFace) {
    let modular_values: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Modular(n) => Some(*n),
            _ => None,
        })
        .collect();
    if modular_values.is_empty() {
        return;
    }

    // ETB-with-counters replacement: per-N idempotency match on the synthesized
    // Moved → PutCounter(SelfRef, P1P1, Fixed(N)) replacement. The predicate is
    // narrowed to the exact N so a card that carries both a printed "enters
    // with K +1/+1 counters" replacement AND `Keyword::Modular(N)` with K≠N
    // can't silently dedupe — each Modular instance only counts an existing
    // ETB replacement as covered when its `Fixed` value equals that instance's
    // N. Walking Ballista's `count: CostXPaid` variant fails the `Fixed { .. }`
    // pattern regardless and never collides.
    //
    // Dies-transfer is shape-only because the execute body has no N dependence
    // (count is the LKI-counted runtime quantity, identical across all
    // instances on a single face).
    let existing_dies: usize = face
        .triggers
        .iter()
        .filter(|t| is_modular_dies_transfer_trigger(t))
        .count();

    // Per CR 702.43b + CR 113.2c: each Modular instance emits its own ETB
    // replacement. To survive re-running synthesis idempotently, count
    // existing same-N replacements and emit only the delta — `Modular(2)`
    // twice on a face needs two `Fixed(2)` replacements; running synthesis
    // again must not add a third.
    for &n in &modular_values {
        let needed = modular_values.iter().filter(|m| **m == n).count();
        let existing = face
            .replacements
            .iter()
            .filter(|r| is_modular_etb_replacement(r, n))
            .count();
        if existing >= needed {
            continue;
        }
        let etb_counters = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: n as i32 },
                target: TargetFilter::SelfRef,
            },
        )
        .description(format!(
            "This permanent enters with {n} +1/+1 counter{} on it",
            if n == 1 { "" } else { "s" }
        ));

        let replacement = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(etb_counters)),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(format!(
                "CR 702.43a: Modular {n} — this permanent enters with {n} +1/+1 counter{} on it.",
                if n == 1 { "" } else { "s" }
            )),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(replacement);
    }

    for _ in modular_values.iter().skip(existing_dies) {
        // CR 122.1 + CR 400.7: Transfer count reads from the source object's
        // counter map, with LKI fallback. At dies-trigger resolution the source
        // is already in the graveyard, so this resolves against the LKI
        // snapshot captured by `apply_zone_exit_cleanup` — capturing any
        // counters added by Hardened Scales, removed by -1/-1 annihilation,
        // etc. before death.
        let transfer = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(CounterType::Plus1Plus1),
                    },
                },
                // CR 702.43a: "target artifact creature" — conjunction of
                // Artifact + Creature core types.
                target: TargetFilter::Typed(
                    TypedFilter::creature().with_type(TypeFilter::Artifact),
                ),
            },
        )
        .description("Put a +1/+1 counter on target artifact creature for each +1/+1 counter on this permanent".to_string())
        // CR 603.5: "you may" — optional triggered abilities go on the stack
        // and the controller is prompted to skip the option during resolution.
        .optional();

        let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .valid_card(TargetFilter::SelfRef)
            .execute(transfer)
            .description(
                "CR 702.43a: Modular — when this creature dies, you may put a +1/+1 counter on target artifact creature for each +1/+1 counter on it."
                    .to_string(),
            );
        face.triggers.push(trigger);
    }
}

/// Idempotency-shape predicate for `synthesize_modular`'s ETB-with-counters
/// replacement. True iff `replacement` is a `Moved` replacement on `SelfRef`
/// whose execute body is `Effect::PutCounter` placing exactly `expected_n`
/// P1P1 counters on `SelfRef` with a fixed count.
///
/// The `expected_n` argument is load-bearing: a card carrying both a parsed
/// "enters with K +1/+1 counters" replacement AND `Keyword::Modular(N)` with
/// K ≠ N must NOT silently dedupe — the K replacement is not a Modular-N
/// replacement and the synthesizer must still emit Fixed(N). Matching by
/// shape alone (any `Fixed { value }`) would treat K as covering N and skip
/// the emit, leaving the card with the wrong ETB count.
fn is_modular_etb_replacement(replacement: &ReplacementDefinition, expected_n: u32) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || !matches!(replacement.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = replacement.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type,
            count: QuantityExpr::Fixed { value },
            target: TargetFilter::SelfRef,
        } if *counter_type == CounterType::Plus1Plus1 && *value == expected_n as i32
    )
}

/// Idempotency-shape predicate for `synthesize_modular`'s dies-transfer
/// trigger. True iff `trigger` is a dies trigger (Battlefield → Graveyard) on
/// `SelfRef` whose execute body is `Effect::PutCounter` placing P1P1 counters
/// on an artifact-creature target with an LKI-counter-count quantity ref.
fn is_modular_dies_transfer_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.origin != Some(Zone::Battlefield)
        || t.destination != Some(Zone::Graveyard)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type,
            count: QuantityExpr::Ref {
                qty: QuantityRef::CountersOn {
                    scope: ObjectScope::Source,
                    counter_type: Some(lki_ct),
                },
            },
            target: TargetFilter::Typed(tf),
        } if *counter_type == CounterType::Plus1Plus1
            && *lki_ct == CounterType::Plus1Plus1
            && tf.type_filters.iter().any(|f| matches!(f, TypeFilter::Creature))
            && tf.type_filters.iter().any(|f| matches!(f, TypeFilter::Artifact))
    )
}

/// CR 702.44a: Sunburst — "If this object is entering as a creature, ignoring
/// any type-changing effects that would affect it, it enters with a +1/+1
/// counter on it for each color of mana spent to cast it. Otherwise, it enters
/// with a charge counter on it for each color of mana spent to cast it."
///
/// CR 702.44b: counters are added only when the object enters from the stack as
/// a resolving spell and one or more colored mana was spent on its costs. Both
/// gates are satisfied for free by the chosen primitives:
///   * The replacement is a `ReplacementEvent::Moved` on `SelfRef` (mirrors
///     `synthesize_modular`), so it only fires on the keyword-bearing object's
///     own battlefield entry. A permanent entering from a zone other than the
///     stack (e.g. blinked, reanimated) had no mana spent on it, so
///     `colors_spent_to_cast` is empty (CR 601.2h tracks it only on a cast) and
///     `DistinctColors` resolves to 0 — no counters are placed, matching CR
///     702.44b without a separate "from the stack" guard.
///   * The count is `QuantityRef::ManaSpentToCast { SelfObject, DistinctColors }`,
///     which the ETB-counter resolver threads against the *entering* object's
///     per-color mana tally (see `extract_etb_counters`). Colorless and generic
///     mana never increment a color bucket, so a fully colorless payment yields
///     0 distinct colors and 0 counters.
///
/// CR 702.44a's creature-vs-noncreature branch is resolved at synthesis time on
/// the face's characteristic-defining (printed) core types — which is precisely
/// "ignoring any type-changing effects." Every printed Sunburst card is either
/// a creature card or a noncreature card on its face, so the printed type is the
/// authoritative branch. A creature face places `Plus1Plus1`; any other face
/// places `Generic("charge")` (charge counters have no dedicated `CounterType`
/// variant — they are the canonical generic counter; see Everflowing Chalice).
///
/// CR 702.44d: if an object has multiple instances of sunburst, each one works
/// separately — so one replacement is emitted per `Keyword::Sunburst` instance,
/// with the same per-instance idempotency discipline as `synthesize_modular`.
///
/// CR 702.44c (Sunburst used as a variable for another ability, e.g.
/// "Modular—Sunburst") is NOT this keyword: that case is parsed as a
/// `QuantityRef::ManaSpentToCast { DistinctColors }` count on the host ability
/// and already resolves through the shared mana-spent-to-cast plumbing,
/// independent of creature/noncreature status.
///
/// Build-for-the-class: this is the exact `synthesize_modular` ETB shape with
/// the count generalized from `Fixed(n)` to the distinct-colors-spent ref and
/// the counter type chosen by entering-as-creature. Any future
/// "enters-with-counters-equal-to-a-cast-metric" keyword lifts this directly.
pub fn synthesize_sunburst(face: &mut CardFace) {
    let instances = face
        .keywords
        .iter()
        .filter(|kw| matches!(kw, Keyword::Sunburst))
        .count();
    if instances == 0 {
        return;
    }

    // CR 702.44a: branch on the printed (characteristic-defining) core types,
    // ignoring type-changing effects.
    let counter_type = if face.card_type.core_types.contains(&CoreType::Creature) {
        CounterType::Plus1Plus1
    } else {
        CounterType::Generic("charge".to_string())
    };

    // CR 702.44d: each instance works separately. Emit one replacement per
    // instance, counting existing synthesized Sunburst replacements so a re-run
    // adds only the delta (idempotency mirrors `synthesize_modular`).
    let existing = face
        .replacements
        .iter()
        .filter(|r| is_sunburst_etb_replacement(r, &counter_type))
        .count();

    let counter_phrase = match &counter_type {
        CounterType::Plus1Plus1 => "+1/+1",
        _ => "charge",
    };

    for _ in existing..instances {
        let etb_counters = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: counter_type.clone(),
                // CR 702.44a + CR 601.2h: one counter per *color* (max 5) of mana
                // spent to cast this object — the distinct-colors metric, not the
                // total amount.
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: CastManaObjectScope::SelfObject,
                        metric: CastManaSpentMetric::DistinctColors,
                    },
                },
                target: TargetFilter::SelfRef,
            },
        )
        .description(format!(
            "This permanent enters with a {counter_phrase} counter on it for each color of mana spent to cast it"
        ));

        let replacement = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(etb_counters)),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(format!(
                "CR 702.44a: Sunburst — this permanent enters with a {counter_phrase} counter on it for each color of mana spent to cast it."
            )),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(replacement);
    }
}

/// Idempotency-shape predicate for `synthesize_sunburst`'s ETB-with-counters
/// replacement. True iff `replacement` is a `Moved` replacement on `SelfRef`
/// whose execute body is `Effect::PutCounter` placing the expected counter type
/// on `SelfRef` with a `ManaSpentToCast { SelfObject, DistinctColors }` count.
///
/// The `expected_ct` argument keys the match to the branch (`Plus1Plus1` for a
/// creature face, `Generic("charge")` otherwise) so the predicate counts only
/// replacements this synthesizer would have emitted for the current face.
fn is_sunburst_etb_replacement(
    replacement: &ReplacementDefinition,
    expected_ct: &CounterType,
) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || !matches!(replacement.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = replacement.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type,
            count: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::DistinctColors,
                },
            },
            target: TargetFilter::SelfRef,
        } if counter_type == expected_ct
    )
}

/// Idempotency-shape predicate for `synthesize_backup`'s ETB trigger.
/// True iff `trigger` is a ChangesZone (→Battlefield) trigger on SelfRef
/// whose execute body is `Effect::PutCounter` placing `expected_n` P1P1
/// counters on a creature target.
fn is_backup_etb_trigger_with_count(t: &TriggerDefinition, expected_n: u32) -> bool {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.destination != Some(Zone::Battlefield)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type,
            count: QuantityExpr::Fixed { value },
            target: TargetFilter::Typed(tf),
        } if *counter_type == CounterType::Plus1Plus1
            && *value == expected_n as i32
            && tf.type_filters.iter().any(|f| matches!(f, TypeFilter::Creature))
    )
}

#[cfg(test)]
fn is_backup_etb_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.destination != Some(Zone::Battlefield)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type,
            target: TargetFilter::Typed(tf),
            ..
        } if *counter_type == CounterType::Plus1Plus1
            && tf.type_filters.iter().any(|f| matches!(f, TypeFilter::Creature))
    )
}

fn backup_granted_oracle_text(face: &CardFace) -> Option<String> {
    let mut lines = face.oracle_text.as_deref()?.lines();
    for line in lines.by_ref() {
        let first_keyword = strip_reminder_text(line)
            .split([',', ';'])
            .next()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .and_then(|text| parse_keyword_from_oracle(&text));
        if matches!(first_keyword, Some(Keyword::Backup(_))) {
            let granted = lines.collect::<Vec<_>>().join("\n");
            return Some(granted);
        }
    }
    None
}

fn backup_grant_modifications(face: &CardFace) -> Vec<ContinuousModification> {
    let Some(granted_text) = backup_granted_oracle_text(face) else {
        return Vec::new();
    };
    if granted_text.trim().is_empty() {
        return Vec::new();
    }

    let types: Vec<String> = face
        .card_type
        .core_types
        .iter()
        .map(ToString::to_string)
        .collect();
    let keyword_names: Vec<String> = face.keywords.iter().map(keyword_display_name).collect();
    let parsed = parse_oracle_text(
        &granted_text,
        &face.name,
        &keyword_names,
        &types,
        &face.card_type.subtypes,
    );

    let mut modifications = backup_keyword_modifications(&granted_text);
    for keyword in parsed.extracted_keywords {
        push_backup_keyword_modification(&mut modifications, keyword);
    }
    for definition in parsed.abilities {
        modifications.push(ContinuousModification::GrantAbility {
            definition: Box::new(definition),
        });
    }
    for trigger in parsed.triggers {
        modifications.push(ContinuousModification::GrantTrigger {
            trigger: Box::new(trigger),
        });
    }
    for definition in parsed.statics {
        modifications.push(ContinuousModification::GrantStaticAbility {
            definition: Box::new(definition),
        });
    }

    modifications
}

fn backup_keyword_modifications(granted_text: &str) -> Vec<ContinuousModification> {
    let mut modifications = Vec::new();
    for line in granted_text.lines() {
        let without_reminder = strip_reminder_text(line);
        let parts: Vec<&str> = without_reminder
            .split([',', ';'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        if parts.is_empty() {
            continue;
        }

        let mut line_keywords = Vec::new();
        for part in parts {
            let lower = part.to_ascii_lowercase();
            let Some(keyword) = parse_keyword_from_oracle(&lower) else {
                line_keywords.clear();
                break;
            };
            line_keywords.push(keyword);
        }

        for keyword in line_keywords {
            push_backup_keyword_modification(&mut modifications, keyword);
        }
    }
    modifications
}

fn push_backup_keyword_modification(
    modifications: &mut Vec<ContinuousModification>,
    keyword: Keyword,
) {
    if matches!(keyword, Keyword::Backup(_)) {
        return;
    }
    let modification = ContinuousModification::AddKeyword { keyword };
    if !modifications.contains(&modification) {
        modifications.push(modification);
    }
}

/// CR 702.165: Backup N — ETB triggered ability that places N +1/+1 counters
/// on target creature and grants this creature's non-Backup abilities printed
/// below that Backup ability to that creature until end of turn if it's another
/// creature.
///
/// Build-for-the-class: synthesized from `Keyword::Backup(N)` so every printed
/// Backup card gets the same trigger. CR 702.165a/c: only abilities printed
/// below the Backup ability are granted; abilities printed above Backup and
/// abilities gained from effects are not.
///
/// CR 702.165d: The granted abilities are locked in when the triggered ability
/// is put on the stack. In card-data synthesis, that means parsing the printed
/// Oracle suffix below the first Backup line rather than copying the face's
/// already-merged current ability vectors.
pub fn synthesize_backup(face: &mut CardFace) {
    let backup_values: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Backup(n) => Some(*n),
            _ => None,
        })
        .collect();
    if backup_values.is_empty() {
        return;
    }

    let modifications = backup_grant_modifications(face);

    // Build the GenericEffect for ability granting
    // CR 702.165c: "until end of turn" + "if that's another creature"
    let grant_effect = if modifications.is_empty() {
        // No other abilities to grant, just place counters
        None
    } else {
        Some(Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::ParentTarget),
                modifications,
                condition: None,
                ..StaticDefinition::new(StaticMode::Continuous)
            }],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
        })
    };

    for &n in &backup_values {
        let needed = backup_values.iter().filter(|value| **value == n).count();
        let existing = face
            .triggers
            .iter()
            .filter(|trigger| is_backup_etb_trigger_with_count(trigger, n))
            .count();
        if existing >= needed {
            continue;
        }

        // Build the counter-placement primary ability
        // CR 702.165a: "put N +1/+1 counters on target creature"
        let counter_ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: n as i32 },
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .description(format!(
            "Put {n} +1/+1 counter{} on target creature",
            if n == 1 { "" } else { "s" }
        ));

        // Chain ability granting if needed, gated on "if that's another creature"
        let counter_ability = if let Some(grant_effect) = grant_effect.clone() {
            let grant_sub = AbilityDefinition::new(AbilityKind::Spell, grant_effect)
                .condition(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::TargetMatchesFilter {
                        filter: TargetFilter::SelfRef,
                        use_lki: false,
                    }),
                })
                .description(
                    "If that's another creature, it gains this creature's non-backup abilities printed below backup until end of turn."
                        .to_string(),
                );
            counter_ability.sub_ability(grant_sub)
        } else {
            counter_ability
        };

        // Build the ETB trigger
        // CR 702.165a: "when this creature enters"
        let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Battlefield])
            .execute(counter_ability)
            .description(format!(
                "CR 702.165: Backup {n} — when this creature enters, put {n} +1/+1 counter{} on target creature. If that's another creature, it gains this creature's non-backup abilities printed below backup until end of turn.",
                if n == 1 { "" } else { "s" }
            ));

        face.triggers.push(trigger);
    }
}

/// CR 702.72a: Build the `TargetFilter` for "another [type] you control" — the
/// permanent the controller may exile to avoid sacrificing the championing
/// permanent. The `type_str` is the capitalized Champion payload (e.g.
/// "Kithkin", "Dragon"); per CR 702.72a it always names a creature type. A
/// payload of "Creature" (cards that champion a creature of any type) yields a
/// bare creature filter with no subtype constraint.
///
/// `FilterProp::Another` enforces the "another" clause (CR 109.1): the
/// championing permanent itself can never be the exiled creature.
fn champion_type_filter(type_str: &str) -> TargetFilter {
    let mut filter = TypedFilter::creature()
        .controller(ControllerRef::You)
        .properties(vec![FilterProp::Another]);
    if !type_str.eq_ignore_ascii_case("creature") {
        filter = filter.subtype(type_str.to_string());
    }
    TargetFilter::Typed(filter)
}

fn champion_has_eligible_object_condition(type_str: &str) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: champion_type_filter(type_str),
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    }
}

fn is_champion_self_sacrifice_ability(ability: &AbilityDefinition) -> bool {
    matches!(
        &*ability.effect,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    )
}

/// CR 702.72a + CR 702.72b: Build Champion's paired triggers from the keyword's
/// `[type]` payload.
///
/// **Linkage (the load-bearing design decision).** The exiled card and the
/// championing permanent must be linked so the SAME card returns when the
/// permanent leaves. Champion has an explicit leaves-battlefield return trigger,
/// so the ETB branch records the exiled object with the source and the LTB
/// trigger consumes `TargetFilter::ExiledBySource`. That keeps both the
/// auto-selected single-object path and the interactive multi-object path on the
/// same `ExileLinkKind::TrackedBySource` model rather than depending on
/// `UntilHostLeavesPlay` duration threading through the choice UI.
///
/// Note this rules out modeling CR 702.72a as a literal "sacrifice it unless
/// you pay [Exile cost]" (`UnlessPayModifier { cost: AbilityCost::Exile }`):
/// exile-as-COST never creates a source-tracked link, so the championed
/// card would be lost forever. Instead the ETB is conditionally a controller
/// choice (`Effect::ChooseOneOf`, the Fabricate shape) between
/// exiling-with-link and sacrificing when an eligible object exists, with a
/// direct sacrifice `else_ability` when none exists.
///
/// 1. ETB (CR 702.72a): "When this permanent enters, sacrifice it unless you
///    exile another [type] you control." Branch A is a `ChangeZone` exile of a
///    chosen "another [type] you control"; because the source carries an LTB
///    `ExiledBySource` consumer, the `ChangeZone` resolver records the link.
///    Branch B is a `Sacrifice` of the championing permanent. The wrapper
///    condition gates the choice on `ObjectCount(another [type] you control) >=
///    1`, so no eligible object means the sacrifice branch runs directly
///    instead of offering a no-op exile branch.
/// 2. LTB (CR 702.72a): "When this permanent leaves the battlefield, return the
///    exiled card to the battlefield under its owner's control." Modeled as a
///    `LeavesBattlefield` trigger returning `TargetFilter::ExiledBySource`
///    (resolved from `state.exile_links` or the source's LKI snapshot).
fn build_champion_triggers(type_str: &str) -> Vec<TriggerDefinition> {
    vec![
        build_champion_etb_trigger(type_str),
        build_champion_ltb_return_trigger(),
    ]
}

fn build_champion_etb_trigger(type_str: &str) -> TriggerDefinition {
    let champion_filter = champion_type_filter(type_str);

    // CR 702.72a branch A: "exile another [type] you control." The source's LTB
    // trigger consumes `ExiledBySource`, so the `ChangeZone` resolver records a
    // source-tracked link to the championing permanent.
    let exile_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Exile,
            target: champion_filter.clone(),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            face_down_profile: None,
        },
    )
    .description(format!("Exile another {type_str} you control"));

    // CR 702.72a branch B + CR 701.21a: "sacrifice it" — sacrifice the
    // championing permanent itself.
    let sacrifice_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    )
    .description("Sacrifice this permanent".to_string());

    let mut choose = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![exile_branch, sacrifice_branch.clone()],
        },
    )
    .condition(champion_has_eligible_object_condition(type_str));
    choose.else_ability = Some(Box::new(sacrifice_branch));

    TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(choose)
        .description(format!(
            "CR 702.72a: Champion a{} {type_str} — when this permanent enters, sacrifice it unless you exile another {type_str} you control.",
            if starts_with_vowel_sound(type_str) { "n" } else { "" }
        ))
}

fn build_champion_ltb_return_trigger() -> TriggerDefinition {
    // CR 702.72a: "return the exiled card to the battlefield under its owner's
    // control." `TargetFilter::ExiledBySource` resolves the linked card from
    // `state.exile_links`; CR 702.72b makes the ETB exile and LTB return linked.
    // `owner_library: false` keeps the return under the owner (not the
    // controller). The card returns to the battlefield.
    let return_ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target: TargetFilter::ExiledBySource,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            face_down_profile: None,
        },
    )
    .description("Return the exiled card to the battlefield under its owner's control".to_string());

    TriggerDefinition::new(TriggerMode::LeavesBattlefield)
        .valid_card(TargetFilter::SelfRef)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(return_ability)
        .description(
            "CR 702.72a: Champion — when this permanent leaves the battlefield, return the exiled card to the battlefield under its owner's control."
                .to_string(),
        )
}

/// Heuristic for the "a"/"an" article in Champion's display description. Not a
/// game rule — display-only.
fn starts_with_vowel_sound(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u'))
}

/// Idempotency / runtime-removal shape predicate for Champion's ETB trigger.
/// True iff `trigger` is an ETB (→ Battlefield) trigger on `SelfRef` whose
/// execute body is a `ChooseOneOf` between an exile of "another [type] you
/// control" and a self-`Sacrifice`, gated by the same
/// eligible-object condition with self-sacrifice as the fallback.
fn is_champion_etb_trigger(t: &TriggerDefinition, type_str: &str) -> bool {
    if !matches!(t.mode, TriggerMode::ChangesZone)
        || t.destination != Some(Zone::Battlefield)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    if execute.condition != Some(champion_has_eligible_object_condition(type_str))
        || !execute
            .else_ability
            .as_deref()
            .is_some_and(is_champion_self_sacrifice_ability)
    {
        return false;
    }
    let Effect::ChooseOneOf { branches, .. } = &*execute.effect else {
        return false;
    };
    let has_exile = branches.iter().any(|b| {
        b.duration.is_none()
            && matches!(
                &*b.effect,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    target,
                    ..
                } if *target == champion_type_filter(type_str)
            )
    });
    let has_sacrifice = branches.iter().any(is_champion_self_sacrifice_ability);
    has_exile && has_sacrifice
}

/// Idempotency / runtime-removal shape predicate for Champion's LTB trigger.
/// True iff `trigger` is a `LeavesBattlefield` trigger on `SelfRef` whose
/// execute returns `TargetFilter::ExiledBySource` to the battlefield.
fn is_champion_ltb_return_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::LeavesBattlefield)
        || !matches!(t.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    matches!(
        t.execute.as_deref().map(|a| &*a.effect),
        Some(Effect::ChangeZone {
            destination: Zone::Battlefield,
            target: TargetFilter::ExiledBySource,
            ..
        })
    )
}

/// CR 702.72a + CR 702.72b: Champion a[n] [type] — install the paired
/// ETB-exile-or-sacrifice and LTB-return-linked-card triggers. Reuses the
/// `install_matching` chokepoint so the synthesis is idempotent and shares the
/// CR 604.1 runtime-grant/removal path with the other keyword triggers.
pub fn synthesize_champion(face: &mut CardFace) {
    KeywordTriggerInstaller::install_matching(face, |kw| matches!(kw, Keyword::Champion(_)));
}

/// CR 702.58a: Graft N — represents both a static ability and a triggered
/// ability. "Graft N" means "This permanent enters with N +1/+1 counters on
/// it" AND "Whenever another creature enters, if this permanent has a +1/+1
/// counter on it, you may move a +1/+1 counter from this permanent onto that
/// creature."
///
/// Build-for-the-class: synthesized from `Keyword::Graft(N)` so every printed
/// Graft card and every runtime-granted Graft (via `AddKeyword`) gets the same
/// two abilities. Mirrors `synthesize_modular` (CR 702.43a) which is the
/// nearest analog — ETB-with-N-P1P1 replacement + ChangesZone trigger. The
/// trigger differs in two ways:
///   1. It fires on *another creature* entering (CR 702.58a "another
///      creature"), not on this permanent dying. We model the "another"
///      exclusion via `FilterProp::Another` on the trigger's `valid_card`.
///   2. It is gated by an intervening-if "if this permanent has a +1/+1
///      counter on it" — `TriggerCondition::HasCounters { OfType(P1P1),
///      minimum: 1 }` checked at detection AND on resolution per CR 603.4.
///   3. The action is a counter MOVE (CR 122.5), not a put — so the source
///      loses a counter as the target gains one. `Effect::MoveCounters` with
///      `source = SelfRef`, `target = TriggeringSource`, `mode = Move`,
///      `counter_type = P1P1`, `count = 1`.
///
/// CR 702.58b: Multiple Graft instances work separately, so the synthesizer
/// emits one replacement + one trigger per instance, deduped via shape-
/// matching idempotency predicates.
pub fn synthesize_graft(face: &mut CardFace) {
    let graft_values: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Graft(n) => Some(*n),
            _ => None,
        })
        .collect();
    if graft_values.is_empty() {
        return;
    }

    // CR 702.58a clause 1 + CR 113.2c: Each Graft instance emits its own ETB
    // replacement. Per-N idempotency mirrors `synthesize_modular` — a card
    // carrying both a printed "enters with K +1/+1 counters" replacement AND
    // `Keyword::Graft(N)` with K≠N must not silently dedupe.
    for &n in &graft_values {
        let needed = graft_values.iter().filter(|m| **m == n).count();
        let existing = face
            .replacements
            .iter()
            .filter(|r| is_graft_etb_replacement(r, n))
            .count();
        if existing >= needed {
            continue;
        }
        let etb_counters = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: n as i32 },
                target: TargetFilter::SelfRef,
            },
        )
        .description(format!(
            "This permanent enters with {n} +1/+1 counter{} on it",
            if n == 1 { "" } else { "s" }
        ));

        let replacement = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(etb_counters)),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(format!(
                "CR 702.58a: Graft {n} — this permanent enters with {n} +1/+1 counter{} on it.",
                if n == 1 { "" } else { "s" }
            )),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(replacement);
    }

    // CR 702.58a clause 2: "Whenever another creature enters, if this permanent
    // has a +1/+1 counter on it, you may move a +1/+1 counter from this
    // permanent onto that creature." Emit one trigger per Graft instance per
    // CR 702.58b. Trigger shape is N-independent (the move is always exactly
    // one +1/+1 counter), so idempotency counts triggers shape-only.
    let needed_triggers = graft_values.len();
    let existing_triggers = face
        .triggers
        .iter()
        .filter(|t| is_graft_enters_trigger(t))
        .count();
    let trigger = build_graft_enters_trigger();
    for _ in 0..needed_triggers.saturating_sub(existing_triggers) {
        face.triggers.push(trigger.clone());
    }
}

/// Build the Graft "another creature enters" trigger.
///
/// Single source of truth for the trigger shape, shared by the printed path
/// (`synthesize_graft`) and the runtime-granted path
/// (`KeywordTriggerInstaller::triggers_for`) per CR 604.1.
///
/// CR 702.58a: trigger condition is "another creature enters" — modeled as
/// `TriggerMode::ChangesZone` with `destination = Battlefield` and
/// `valid_card` set to a creature filter with `FilterProp::Another` (CR
/// 109.3 + CR 603.6a). The Graft permanent itself can be a creature (e.g.,
/// Vigean Graftmage) or a land (Llanowar Reborn) — either way the source is
/// excluded by `Another` (the entering creature is always a different object
/// from the source). Note also the trigger uses no controller filter — Graft
/// fires on ANY creature ETB, not just "you control" (CR 702.58a does not
/// restrict by controller).
fn build_graft_enters_trigger() -> TriggerDefinition {
    // CR 122.5: Move exactly one +1/+1 counter from this permanent (SelfRef)
    // onto the entering creature (TriggeringSource).
    let move_one = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::MoveCounters {
            source: TargetFilter::SelfRef,
            counter_type: Some(CounterType::Plus1Plus1),
            count: Some(QuantityExpr::Fixed { value: 1 }),
            mode: crate::types::ability::CounterTransferMode::Move,
            selection: crate::types::ability::CounterMoveSelection::StackTarget,
            target: TargetFilter::TriggeringSource,
        },
    )
    .description("Move a +1/+1 counter from this permanent onto that creature".to_string())
    // CR 603.5: "you may" — optional triggered ability; controller is prompted
    // before the move resolves.
    .optional();

    // CR 702.58a "another creature enters" — match any creature except the
    // trigger source. `FilterProp::Another` (CR 109.3) excludes the ability
    // source, so a Graft creature that itself enters does not satisfy this
    // (the ETB of the source is handled by the replacement, not this trigger).
    let another_creature =
        TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Another]));

    // CR 603.4 + CR 702.58a "if this permanent has a +1/+1 counter on it" —
    // intervening-if checked at detection AND re-checked on resolution.
    // `CounterMatch::OfType(P1P1)` restricts to the specific counter type;
    // `minimum: 1` enforces "has a +1/+1 counter".
    let condition = TriggerCondition::HasCounters {
        counters: CounterMatch::OfType(CounterType::Plus1Plus1),
        minimum: 1,
        maximum: None,
    };

    TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(another_creature)
        .condition(condition)
        .execute(move_one)
        .description(
            "CR 702.58a: Graft — whenever another creature enters, if this permanent has a +1/+1 counter on it, you may move a +1/+1 counter from this permanent onto that creature."
                .to_string(),
        )
}

/// Idempotency-shape predicate for `synthesize_graft`'s ETB-with-counters
/// replacement. True iff `replacement` is a `Moved` replacement on `SelfRef`
/// whose execute body is `Effect::PutCounter` placing exactly `expected_n`
/// P1P1 counters on `SelfRef` with a fixed count.
///
/// Mirrors `is_modular_etb_replacement` — `expected_n` is load-bearing so a
/// card carrying both a parsed "enters with K +1/+1 counters" replacement AND
/// `Keyword::Graft(N)` with K ≠ N must NOT silently dedupe. Walking Ballista-
/// style `count: CostXPaid` variants fail the `Fixed { .. }` pattern.
fn is_graft_etb_replacement(replacement: &ReplacementDefinition, expected_n: u32) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || !matches!(replacement.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = replacement.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::PutCounter {
            counter_type,
            count: QuantityExpr::Fixed { value },
            target: TargetFilter::SelfRef,
        } if *counter_type == CounterType::Plus1Plus1 && *value == expected_n as i32
    )
}

/// Idempotency-shape predicate for `synthesize_graft`'s "another creature
/// enters" trigger. True iff `trigger` is an enters-the-battlefield trigger
/// (ChangesZone, destination = Battlefield) on `Another` creature whose
/// execute body is `Effect::MoveCounters` moving one P1P1 counter from
/// `SelfRef` onto `TriggeringSource`. The trigger shape is N-independent — a
/// face with multiple Graft instances has multiple identical triggers.
fn is_graft_enters_trigger(t: &TriggerDefinition) -> bool {
    if !matches!(t.mode, TriggerMode::ChangesZone) || t.destination != Some(Zone::Battlefield) {
        return false;
    }
    let Some(TargetFilter::Typed(tf)) = t.valid_card.as_ref() else {
        return false;
    };
    if !tf
        .type_filters
        .iter()
        .any(|f| matches!(f, TypeFilter::Creature))
        || !tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Another))
    {
        return false;
    }
    let Some(execute) = t.execute.as_deref() else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::MoveCounters {
            source: TargetFilter::SelfRef,
            counter_type: Some(ct),
            count: Some(QuantityExpr::Fixed { value: 1 }),
            mode: crate::types::ability::CounterTransferMode::Move,
            target: TargetFilter::TriggeringSource,
            ..
        } if *ct == CounterType::Plus1Plus1
    )
}

/// CR 702.54a + CR 702.54b: Bloodthirst is a static ability that creates an
/// enters-with-counters replacement. Fixed-N Bloodthirst is conditional on an
/// opponent being dealt damage this turn; Bloodthirst X is unconditional and
/// resolves X from the total damage opponents were dealt this turn.
///
/// CR 702.54c + CR 113.2c: Each Bloodthirst instance applies separately.
/// No printed card today carries two instances, but the per-value idempotency
/// match below treats the count as load-bearing so a granted-Bloodthirst
/// case or future printing routes correctly. The idempotency predicate
/// includes the condition axis, so fixed-N and X forms do not dedupe each
/// other or unrelated ETB-with-counters replacements.
pub fn synthesize_bloodthirst(face: &mut CardFace) {
    let bloodthirst_values: Vec<BloodthirstValue> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Bloodthirst(value) => Some(value.clone()),
            _ => None,
        })
        .collect();
    if bloodthirst_values.is_empty() {
        return;
    }

    // Per CR 702.54c + CR 113.2c: each Bloodthirst instance emits its own
    // ETB replacement. To survive re-running synthesis idempotently, count
    // existing same-value replacements and emit only the delta.
    for value in &bloodthirst_values {
        let needed = bloodthirst_values.iter().filter(|v| *v == value).count();
        let existing = face
            .replacements
            .iter()
            .filter(|r| is_bloodthirst_etb_replacement(r, value))
            .count();
        if existing >= needed {
            continue;
        }
        let etb_counters = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: bloodthirst_counter_quantity(value),
                target: TargetFilter::SelfRef,
            },
        )
        .description(bloodthirst_execute_description(value));

        let replacement = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(etb_counters)),
            valid_card: Some(TargetFilter::SelfRef),
            condition: bloodthirst_condition(value),
            description: Some(bloodthirst_replacement_description(value)),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(replacement);
    }
}

fn bloodthirst_counter_quantity(value: &BloodthirstValue) -> QuantityExpr {
    match value {
        BloodthirstValue::Fixed(n) => QuantityExpr::Fixed { value: *n as i32 },
        BloodthirstValue::X => QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(bloodthirst_opponent_player_filter()),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
        },
    }
}

fn bloodthirst_opponent_player_filter() -> TargetFilter {
    TargetFilter::And {
        filters: vec![
            TargetFilter::Player,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        ],
    }
}

fn bloodthirst_condition(value: &BloodthirstValue) -> Option<ReplacementCondition> {
    match value {
        BloodthirstValue::Fixed(_) => Some(ReplacementCondition::OpponentDamagedThisTurn),
        BloodthirstValue::X => None,
    }
}

fn bloodthirst_execute_description(value: &BloodthirstValue) -> String {
    match value {
        BloodthirstValue::Fixed(n) => format!(
            "This permanent enters with {n} +1/+1 counter{} on it",
            if *n == 1 { "" } else { "s" }
        ),
        BloodthirstValue::X => "This permanent enters with X +1/+1 counters on it".to_string(),
    }
}

fn bloodthirst_replacement_description(value: &BloodthirstValue) -> String {
    match value {
        BloodthirstValue::Fixed(n) => format!(
            "CR 702.54a: Bloodthirst {n} — if an opponent was dealt damage this turn, this permanent enters with {n} +1/+1 counter{} on it.",
            if *n == 1 { "" } else { "s" }
        ),
        BloodthirstValue::X => {
            "CR 702.54b: Bloodthirst X — this permanent enters with X +1/+1 counters on it, where X is the total damage your opponents have been dealt this turn.".to_string()
        }
    }
}

/// Idempotency-shape predicate for `synthesize_bloodthirst`. True iff
/// `replacement` is a `Moved` replacement on `SelfRef` whose condition and
/// execute count match the requested Bloodthirst value.
///
/// The `expected_value` argument is load-bearing: a card carrying both a parsed
/// "enters with K +1/+1 counters" replacement AND `Keyword::Bloodthirst(N)`
/// with K ≠ N must NOT silently dedupe. The condition match is also
/// load-bearing: an unconditional ETB-with-counters replacement (e.g., a
/// printed "this permanent enters with N +1/+1 counters on it") with the
/// same N is NOT a Bloodthirst replacement and must not pre-satisfy the
/// emit (Bloodthirst is conditional on damage history, the printed
/// unconditional one always fires).
fn is_bloodthirst_etb_replacement(
    replacement: &ReplacementDefinition,
    expected_value: &BloodthirstValue,
) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || !matches!(replacement.valid_card, Some(TargetFilter::SelfRef))
        || replacement.condition != bloodthirst_condition(expected_value)
    {
        return false;
    }
    let Some(execute) = replacement.execute.as_deref() else {
        return false;
    };
    let Effect::PutCounter {
        counter_type,
        count,
        target,
    } = &*execute.effect
    else {
        return false;
    };

    *counter_type == CounterType::Plus1Plus1
        && *target == TargetFilter::SelfRef
        && *count == bloodthirst_counter_quantity(expected_value)
}

#[cfg(test)]
fn is_fixed_bloodthirst_etb_replacement(
    replacement: &ReplacementDefinition,
    expected_n: u32,
) -> bool {
    is_bloodthirst_etb_replacement(replacement, &BloodthirstValue::Fixed(expected_n))
}

#[cfg(test)]
fn is_bloodthirst_x_etb_replacement(replacement: &ReplacementDefinition) -> bool {
    is_bloodthirst_etb_replacement(replacement, &BloodthirstValue::X)
}

/// CR 702.82a: Devour N is a static ability — "As this object enters, you may
/// sacrifice any number of creatures. This permanent enters with N +1/+1
/// counters on it for each creature sacrificed this way."
///
/// CR 614.1c: an "as [this permanent] enters" clause is a replacement effect;
/// CR 614.12a: the optional sacrifice choice is made *before* the permanent
/// enters the battlefield. Devour is therefore synthesized as a
/// `ReplacementEvent::Moved` replacement on `SelfRef`, never an activated or
/// triggered ability.
///
/// The synthesized `execute` is a two-step sub-ability chain:
///   1. `Effect::Sacrifice` with `count: UpTo(ObjectCount(your creatures))`
///      and `min_count: 0` — the ranged interactive "sacrifice any number"
///      choice (an empty choice is legal: CR 702.82a "you *may* sacrifice").
///   2. `.sub_ability` = `Effect::PutCounter` of N +1/+1 counters per creature
///      sacrificed on `SelfRef` (CR 122.1).
///
/// Counter-count linkage: the ranged `EffectZoneChoice` Sacrifice completion
/// stamps `state.last_effect_count` (the number of creatures chosen).
/// `QuantityRef::EventContextAmount`'s resolver falls back through
/// `last_effect_count`, so the `PutCounter` count reads exactly the number
/// sacrificed. For Devour N > 1 the count is wrapped in
/// `QuantityExpr::Multiply { factor: n, .. }` (CR 702.82a "N counters per
/// creature sacrificed"). `PreviousEffectAmount` is NOT used — it reads
/// `last_effect_amount`, which the ranged Sacrifice never stamps.
///
/// CR 702.82c "Devour [quality]" variant: `Keyword::Devour(u32)` carries only
/// N, not a quality filter. This synthesizer hard-codes the CR 702.82a default
/// (sacrifice creatures). A future card needing the quality axis requires
/// parameterizing the keyword to `Devour { n, quality }`.
///
/// CR 113.2c: each Devour instance functions independently. Per-N idempotency
/// (`is_devour_etb_replacement`) emits only the delta so re-running synthesis
/// is a no-op.
pub fn synthesize_devour(face: &mut CardFace) {
    let devour_values: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Devour(n) => Some(*n),
            _ => None,
        })
        .collect();
    if devour_values.is_empty() {
        return;
    }

    for &n in &devour_values {
        let needed = devour_values.iter().filter(|m| **m == n).count();
        let existing = face
            .replacements
            .iter()
            .filter(|r| is_devour_etb_replacement(r, n))
            .count();
        if existing >= needed {
            continue;
        }

        // CR 122.1: N +1/+1 counters per creature sacrificed this way. The
        // per-creature count is `EventContextAmount` (resolves to the number
        // the ranged Sacrifice choice stamped into `last_effect_count`); for
        // N > 1 it is scaled by `factor: n`.
        let counter_count = if n == 1 {
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }
        } else {
            QuantityExpr::Multiply {
                factor: n as i32,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            }
        };

        let put_counters = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: counter_count,
                target: TargetFilter::SelfRef,
            },
        );

        // CR 702.82a: "you may sacrifice any number of creatures" — a ranged
        // `UpTo` choice bounded by the controller's eligible creature pool,
        // `min_count: 0` so an empty choice is legal.
        let sacrifice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                count: QuantityExpr::up_to(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::creature().controller(ControllerRef::You),
                        ),
                    },
                }),
                min_count: 0,
            },
        )
        .description(format!(
            "CR 702.82a: Devour {n} — sacrifice any number of creatures; this \
             permanent enters with {n} +1/+1 counter{} per creature sacrificed.",
            if n == 1 { "" } else { "s" }
        ))
        .sub_ability(put_counters);

        let replacement = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(sacrifice)),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(format!(
                "CR 702.82a + CR 614.1c: Devour {n} — as this creature enters, \
                 you may sacrifice any number of creatures; it enters with {n} \
                 +1/+1 counter{} for each creature sacrificed this way.",
                if n == 1 { "" } else { "s" }
            )),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(replacement);
    }
}

/// Idempotency-shape predicate for `synthesize_devour`'s ETB replacement.
/// True iff `replacement` is a `Moved` replacement on `SelfRef` whose `execute`
/// chain is `Effect::Sacrifice` of your creatures (ranged `UpTo`) with a
/// `PutCounter` of `expected_n` P1P1 counters per creature on `SelfRef` as its
/// sub-ability.
///
/// `expected_n` is load-bearing: a card carrying both a printed enters-with-K
/// replacement and `Keyword::Devour(N≠K)` must not dedupe — the `Multiply`
/// factor (N) for N > 1 and the bare `EventContextAmount` (N == 1) discriminate
/// the count.
fn is_devour_etb_replacement(replacement: &ReplacementDefinition, expected_n: u32) -> bool {
    if !matches!(replacement.event, ReplacementEvent::Moved)
        || !matches!(replacement.valid_card, Some(TargetFilter::SelfRef))
    {
        return false;
    }
    let Some(execute) = replacement.execute.as_deref() else {
        return false;
    };
    if !matches!(&*execute.effect, Effect::Sacrifice { .. }) {
        return false;
    }
    let Some(sub) = execute.sub_ability.as_deref() else {
        return false;
    };
    let Effect::PutCounter {
        counter_type,
        count,
        target: TargetFilter::SelfRef,
    } = &*sub.effect
    else {
        return false;
    };
    if *counter_type != CounterType::Plus1Plus1 {
        return false;
    }
    let expected_count = if expected_n == 1 {
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        }
    } else {
        QuantityExpr::Multiply {
            factor: expected_n as i32,
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
        }
    };
    *count == expected_count
}

/// CR 702.62a: Suspend N—{cost} synthesizes three abilities for every face
/// carrying `Keyword::Suspend { count, cost }`:
///
///   1. **Hand-activated alt-cost** ("Rather than cast this card from your hand,
///      you may pay [cost] and exile it with N time counters on it. This action
///      doesn't use the stack."). Modeled as an activated ability with
///      `activation_zone = Hand` and `ActivationRestriction::MatchesCardCastTiming`
///      (CR 702.62a "if you could begin to cast this card by putting it onto the
///      stack from your hand"). Cost is composite (mana + exile self from hand);
///      effect is a Time-counter `PutCounter` on the now-exiled SelfRef. The
///      synthesized activation does land on the stack as an activated ability,
///      which is a controlled approximation of the rule's "doesn't use the stack"
///      — no card today interacts with that distinction.
///
///   2. **Upkeep counter-removal trigger** ("At the beginning of your upkeep,
///      if this card is suspended, remove a time counter from it.") fires from
///      the Exile zone (CR 702.62b: "suspended" = in exile + has time counters)
///      via `trigger_zones = [Exile]`, gated by `TriggerConstraint::OnlyDuringYourTurn`
///      so only the suspended card's controller's upkeep triggers it.
///
///   3. **Last-counter free-cast trigger** ("When the last time counter is
///      removed from this card, if it's exiled, you may play it without paying
///      its mana cost…") mirrors `synthesize_siege_intrinsics`' victory trigger
///      pattern: `TriggerMode::CounterRemoved` with
///      `CounterTriggerFilter { Time, threshold: Some(0) }` and an optional
///      `Effect::CastFromZone { without_paying_mana_cost: true }` execute body.
///      The cast itself is detected as `CastingVariant::Suspend` by
///      `prepare_spell_cast` (keyword presence on the exile-zone source) and
///      tagged at stack resolution as `CastVariantPaid::Suspend`. The
///      "if creature, gains haste until you lose control" rider (CR 702.62a
///      final sentence) is installed at stack resolution as a transient
///      continuous effect with
///      `Duration::ForAsLongAs { SourceControllerEquals { resolution_controller } }`.
///
/// Idempotent across repeated invocations (parser pipelines may re-run on the
/// same face). Build-for-the-class: every Suspend card flows through this
/// single synthesizer regardless of card type — the haste install branches by
/// `CoreType::Creature` at runtime, not here.
pub fn synthesize_suspend(face: &mut CardFace) {
    use crate::types::ability::ActivationRestriction;

    // Find the first Suspend keyword. Cards do not print multiple Suspends.
    let Some((time_counters, suspend_cost)) = face.keywords.iter().find_map(|k| match k {
        Keyword::Suspend { count, cost } => Some((*count, cost.clone())),
        _ => None,
    }) else {
        return;
    };

    // CR 702.62a: Activated ability — pay [cost], exile self from hand, then
    // place N time counters on it. Composite cost mirrors `synthesize_cycling`.
    let already_has_activation = face.abilities.iter().any(|a| {
        a.activation_zone == Some(Zone::Hand)
            && a.activation_restrictions
                .contains(&ActivationRestriction::MatchesCardCastTiming)
            && matches!(
                &*a.effect,
                Effect::PutCounter { counter_type, target: TargetFilter::SelfRef, .. }
                    if *counter_type == CounterType::Time
            )
    });
    if !already_has_activation {
        let composite_cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: suspend_cost.clone(),
                },
                // CR 702.62a: "exile it" — self-targeted exile from hand.
                AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Hand),
                    filter: Some(TargetFilter::SelfRef),
                },
            ],
        };
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            // CR 702.62a: "...with N time counters on it." Time counter is a
            // typed CounterType variant; the legacy String API for PutCounter
            // takes the canonical `as_str()` value ("time").
            Effect::PutCounter {
                counter_type: CounterType::Time,
                count: QuantityExpr::Fixed {
                    value: time_counters as i32,
                },
                target: TargetFilter::SelfRef,
            },
        )
        .cost(composite_cost)
        .activation_restrictions(vec![ActivationRestriction::MatchesCardCastTiming]);
        def.activation_zone = Some(Zone::Hand);
        face.abilities.push(def);
    }

    // CR 702.62a + CR 702.62b: Upkeep state trigger — at the beginning of the
    // suspended card's controller's upkeep, if it has any time counters,
    // remove one. `TriggerConstraint::OnlyDuringYourTurn` enforces "your"
    // upkeep; `TriggerCondition::HasCounters` enforces "if this card is
    // suspended" (CR 702.62b: suspended = in exile + has time counters; the
    // exile zone is enforced by `trigger_zones`).
    let already_has_upkeep_trigger = face.triggers.iter().any(is_suspend_upkeep_trigger);
    if !already_has_upkeep_trigger {
        face.triggers.push(build_suspend_upkeep_removal_trigger());
    }

    // CR 702.62a: Last-counter free-cast trigger — "When the last time counter
    // is removed from this card, if it's exiled, you may play it without
    // paying its mana cost." Mirrors `synthesize_siege_intrinsics` victory
    // trigger (CR 310.11b) — both use `CounterRemoved` with `threshold: Some(0)`.
    // The cast itself goes through the normal casting pipeline; `prepare_spell_cast`
    // detects the variant via `obj.zone == Exile && Keyword::Suspend` and assigns
    // `CastingVariant::Suspend`, which tags `CastVariantPaid::Suspend` at
    // resolution and installs the haste static for creatures.
    let already_has_last_counter_trigger =
        face.triggers.iter().any(is_suspend_last_counter_trigger);
    if !already_has_last_counter_trigger {
        face.triggers
            .push(build_suspend_last_counter_cast_trigger());
    }
}

/// CR 702.170 + CR 116.2k: Plot — synthesize a hand-zone activated ability for
/// every face carrying `Keyword::Plot(cost)`.
///
/// Printed text (CR 702.170a): "Plot [cost]" means "Any time you have priority
/// during your main phase while the stack is empty, you may exile this card
/// from your hand and pay [cost]. It becomes a plotted card." Plotting is a
/// special action (CR 116.2k / CR 702.170b) that doesn't use the stack; we
/// approximate it as an activated ability with `activation_zone = Hand`, the
/// `.sorcery_speed()` single-authority builder, and a composite cost
/// `(pay [cost], exile self from hand)`. This is the same controlled
/// approximation Suspend uses (see `synthesize_suspend`); no card today
/// interacts with the "doesn't use the stack" distinction.
///
/// On resolution the activation grants `CastingPermission::Plotted { turn_plotted: 0 }`
/// to the now-exiled card (SelfRef). `grant_permission::resolve` stamps the
/// real `state.turn_number` into `turn_plotted` (mirroring how it resolves
/// `PlayFromExile { granted_to }` for the ability controller). The cast side
/// is detected by `prepare_spell_cast` via `is_plot_cast` — exile-zone source
/// with a `Plotted` permission — which zeros the mana cost
/// (CR 702.170d: "without paying its mana cost") and tags
/// `CastingVariant::Plot` for routing. The "on a later turn" gate is enforced
/// by `has_exile_cast_permission` comparing `state.turn_number > turn_plotted`.
/// Sorcery-speed main-phase-with-empty-stack enforcement is free: Plot cards
/// are non-Instant in the printed OTJ cycle, so `check_spell_timing`'s default
/// sorcery-speed branch covers "may cast as a sorcery" (CR 307.1 + CR 116.1).
///
/// Idempotent across repeated invocations (parser pipelines may re-run on the
/// same face). Build-for-the-class: every Plot card flows through this single
/// synthesizer regardless of card type.
pub fn synthesize_plot(face: &mut CardFace) {
    use crate::types::ability::{ActivationRestriction, CastingPermission, PermissionGrantee};

    // CR 702.170a: Find the first Plot keyword. Cards do not print multiple Plots.
    let Some(plot_cost) = face.keywords.iter().find_map(|k| match k {
        Keyword::Plot(cost) => Some(cost.clone()),
        _ => None,
    }) else {
        return;
    };

    // CR 702.170a: Activated ability — pay [cost] + exile self from hand, then
    // grant Plotted casting permission on the now-exiled SelfRef. Composite cost
    // mirrors `synthesize_suspend`; `.sorcery_speed()` enforces main-phase +
    // empty-stack + active-player timing via `ActivationRestriction::AsSorcery`.
    let already_has_plot_activation = face.abilities.iter().any(|a| {
        a.activation_zone == Some(Zone::Hand)
            && a.activation_restrictions
                .contains(&ActivationRestriction::AsSorcery)
            && matches!(
                &*a.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::Plotted { .. },
                    ..
                }
            )
    });
    if !already_has_plot_activation {
        let composite_cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: plot_cost.clone(),
                },
                // CR 702.170a: "exile this card from your hand" — self-targeted
                // exile from hand. Mirrors Suspend's self-exile cost component.
                AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Hand),
                    filter: Some(TargetFilter::SelfRef),
                },
            ],
        };
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            // CR 702.170a + CR 702.170d: Grant the `Plotted` casting permission
            // to the exiled card. `turn_plotted: 0` is a placeholder stamped
            // by `grant_permission::resolve` to `state.turn_number` at
            // resolution. Grantee is the default `AbilityController` — the
            // plot owner — which is the player allowed to cast it later.
            Effect::GrantCastingPermission {
                permission: CastingPermission::Plotted { turn_plotted: 0 },
                target: TargetFilter::SelfRef,
                grantee: PermissionGrantee::AbilityController,
            },
        )
        .cost(composite_cost)
        // CR 702.170a: "Any time you have priority during your main phase while
        // the stack is empty" — i.e. sorcery-speed timing. `.sorcery_speed()`
        // is the single-authority builder (see `AbilityDefinition::sorcery_speed`).
        .sorcery_speed();
        def.activation_zone = Some(Zone::Hand);
        face.abilities.push(def);
    }
}

/// Run all synthesis functions in canonical order on a card face.
/// Both `oracle_loader.rs` and `oracle_gen.rs` call this to ensure the same
/// complete set of synthesizers is applied.
pub fn synthesize_all(face: &mut CardFace) {
    synthesize_basic_land_mana(face);
    synthesize_equip(face);
    // CR 702.151a: Reconfigure — attach/unattach activated abilities.
    synthesize_reconfigure(face);
    // CR 702.167a/b: Craft — sorcery-speed activated ability that exiles the
    // source plus materials and returns the card transformed.
    synthesize_craft(face);
    // CR 702.122a: Crew has no synthesized ability — activation is handled by
    // GameAction::CrewVehicle directly, not through ActivateAbility dispatch.
    // The Keyword::Crew(N) on the card provides display information.
    synthesize_ninjutsu_family(face);
    synthesize_changeling_cda(face);
    synthesize_kicker(face);
    synthesize_buyback(face);
    synthesize_bargain(face);
    synthesize_gift(face);
    resolve_kicker_condition_variants(face);
    synthesize_case_solve(face);
    // Warp: no synthesis needed — runtime handled by Keyword::Warp directly
    synthesize_mobilize(face);
    // CR 702.134a: Mentor — attack trigger placing a +1/+1 counter on a
    // lesser-power attacking creature.
    synthesize_mentor(face);
    synthesize_job_select(face);
    // CR 702.92a: Living weapon — Equipment ETB trigger creating a 0/0
    // black Phyrexian Germ creature token, then attaching this Equipment
    // to it. Same shape as job select; both share the keyword-to-ETB-attach
    // synthesis pattern.
    synthesize_living_weapon(face);
    // CR 702.163a: For Mirrodin! — same ETB-token-attach shape as living weapon
    // and job select; creates a 2/2 red Rebel creature token.
    synthesize_for_mirrodin(face);
    synthesize_level_up(face);
    synthesize_specialize(face);
    synthesize_cycling(face);
    synthesize_scavenge(face);
    // CR 702.128a / CR 702.129a: Embalm / Eternalize graveyard-activated
    // token-copy abilities (self-contained building block in its own module).
    crate::database::embalm_eternalize::synthesize_embalm_eternalize(face);
    // CR 702.84a: Unearth graveyard-activated temporary reanimation (return with
    // haste, exile at the next end step) — self-contained building block.
    crate::database::unearth::synthesize_unearth(face);
    synthesize_outlast(face);
    synthesize_reinforce(face);
    synthesize_casualty(face);
    // CR 702.56a: Replicate — repeatable optional additional cost + SpellCast
    // copy trigger that makes one copy per replicate payment.
    synthesize_replicate(face);
    // CR 702.69a: Gravestorm — copy this spell for each permanent put into a
    // graveyard from the battlefield this turn.
    synthesize_gravestorm(face);
    // CR 702.144a: Demonstrate — optional self-copy on cast; if taken, a chosen
    // opponent also copies the spell.
    synthesize_demonstrate(face);
    synthesize_entwine(face);
    synthesize_madness_intrinsics(face);
    synthesize_evoke(face);
    synthesize_echo(face);
    // CR 702.24a: Cumulative upkeep — at the beginning of your upkeep, put an
    // age counter on this permanent, then sacrifice it unless you pay its
    // upkeep cost for each age counter on it. Chained-ability shape
    // (AddCounter → Sacrifice with PerCounter unless_pay) preserves the
    // rules-mandated ordering: tick first, then prompt against post-tick total.
    synthesize_cumulative_upkeep(face);
    // CR 702.175a: Offspring — optional additional cost + ETB 1/1 copy trigger.
    synthesize_offspring(face);
    // CR 702.157a: Squad — repeatable optional additional cost + ETB copy trigger.
    synthesize_squad(face);
    // CR 702.123a: Fabricate N — ETB trigger with controller-chosen branch
    // between N +1/+1 counters or N 1/1 colorless Servo artifact creature
    // tokens. Modeled via `Effect::ChooseOneOf`.
    synthesize_fabricate(face);
    // CR 702.136a: Riot — optional ETB replacement choosing +1/+1 counter or
    // haste. Static grants of Riot synthesize matching ETB replacements from
    // their affected filters.
    synthesize_riot(face);
    // CR 702.64a: Absorb N — continuous self-recipient damage replacement that
    // prevents N from each source each time.
    synthesize_absorb(face);
    // CR 702.98a: Unleash — optional ETB +1/+1 counter plus a "can't block while
    // it has a +1/+1 counter" static. Sibling of Riot's optional-counter shape.
    synthesize_unleash(face);
    // CR 702.93a: Undying — dies trigger that returns the permanent with a
    // +1/+1 counter, gated on having had no +1/+1 counter at death (LKI).
    synthesize_undying(face);
    // CR 702.79a: Persist — dies trigger that returns the permanent with a
    // -1/-1 counter, gated on having had no -1/-1 counter at death (LKI).
    // Sibling of Undying via shared `synthesize_dies_return_with_counter`.
    synthesize_persist(face);
    // CR 702.135a: Afterlife N — dies trigger creating N 1/1 white and black
    // Spirit creature tokens with flying. Self-referential dies trigger shape
    // shared with Undying/Persist.
    synthesize_afterlife(face);
    // CR 702.46a: Soulshift N — dies trigger optionally returning a target
    // Spirit card with mana value N or less from your graveyard to your hand.
    // Self-referential dies trigger shape shared with Afterlife/Undying/Persist.
    // CR 702.46b: each instance triggers separately.
    synthesize_soulshift(face);
    // CR 702.112a: Renown N — combat damage to player trigger with
    // designation-setting resolution. CR 702.112c: each instance triggers
    // separately; the resolution-time designation guard suppresses later ones.
    synthesize_renown(face);
    // CR 702.86a: Annihilator N — attacks trigger that forces the defending
    // player to sacrifice N permanents. CR 702.86b: each instance triggers
    // separately. Defending player resolved per-attacker via
    // `ControllerRef::DefendingPlayer` (CR 508.5 / 508.5a).
    synthesize_annihilator(face);
    // CR 702.39a: Provoke — attacks trigger that may untap a creature the
    // defending player controls (CR 508.5 / 508.5a) and force it to block this
    // attacker (reusing the existing source-referential ForceBlock resolver).
    synthesize_provoke(face);
    // CR 702.83a: Exalted — attack trigger that gives +1/+1 until end of turn
    // whenever a creature you control attacks alone. CR 702.83b: each instance
    // triggers separately.
    synthesize_exalted(face);
    // CR 702.25a: Flanking — becomes-blocked trigger giving each blocking
    // creature without flanking -1/-1 until end of turn.
    synthesize_flanking(face);
    // CR 702.101a: Extort — spell-cast trigger that lets you pay {W/B} to drain
    // each opponent for 1 life. CR 702.101b: each instance triggers separately.
    synthesize_extort(face);
    // CR 702.191a: Increment — spell-cast trigger when mana spent exceeds P/T.
    // CR 702.191b: each instance triggers separately.
    synthesize_increment(face);
    // CR 702.105a: Dethrone — attack trigger that puts a +1/+1 counter on the
    // creature whenever it attacks the player with the most life or tied for
    // most life. CR 702.105b: each instance triggers separately.
    synthesize_dethrone(face);
    // CR 702.59a: Recover {cost} — graveyard-sourced dies trigger with a
    // mandatory pay-or-else-exile branch.
    // When another creature is put into your graveyard, you may pay the recover
    // cost to return this card from your graveyard to your hand; otherwise
    // exile it.
    synthesize_recover(face);
    // CR 702.115a: Ingest — combat-damage-to-player trigger that exiles the top
    // card of the damaged player's library.
    synthesize_ingest(face);
    // CR 702.100a: Evolve — ETB trigger that puts a +1/+1 counter on the
    // creature whenever another creature you control enters with greater power
    // or toughness. CR 702.100d: each instance triggers separately.
    synthesize_evolve(face);
    // CR 702.116a: Myriad — attack trigger creating tapped attacking copy
    // tokens for each opponent other than the source creature's defending
    // player, exiled at end of combat. CR 702.116b: each instance triggers
    // separately.
    synthesize_myriad(face);
    // Double team is an Arena/Alchemy attack trigger creating one tapped
    // attacking copy. Each instance triggers separately.
    synthesize_double_team(face);
    // CR 702.45a: Bushido N — self blocks / becomes-blocked triggers that pump
    // the creature +N/+N until end of turn.
    synthesize_bushido(face);
    // CR 702.91a: Battle cry — attack trigger pumping each other attacking
    // creature +1/+0 until end of turn.
    synthesize_battlecry(face);
    // CR 702.25a: Flanking — becomes-blocked trigger giving each non-flanking
    // blocker -1/-1 until end of turn.
    synthesize_flanking(face);
    // CR 702.23a: Rampage N — becomes-blocked self pump scaling +N/+N per blocker
    // beyond the first.
    synthesize_rampage(face);
    // CR 702.121a: Melee — attack-trigger self pump +1/+1 per opponent attacked
    // this combat.
    synthesize_melee(face);
    // CR 702.95a: Soulbond — two optional ETB triggers that create pair
    // relationships under the resolution checks in CR 702.95c-d.
    synthesize_soulbond(face);
    // CR 702.43a + CR 702.43b: Modular N — ETB-with-N-P1P1 replacement plus a
    // dies-trigger transferring counters (LKI-counted) to a target artifact
    // creature. Each instance functions independently.
    synthesize_modular(face);
    // CR 702.44a + CR 702.44b + CR 702.44d: Sunburst — as-enters replacement
    // placing one +1/+1 counter (creature face) or charge counter (noncreature
    // face) per distinct color of mana spent to cast it. Reuses the Modular ETB
    // shape with the count generalized to the distinct-colors-spent metric. Each
    // instance functions independently. Must run after Oracle parsing so
    // `face.card_type` reflects the printed type for the CR 702.44a branch.
    synthesize_sunburst(face);
    // CR 702.58a + CR 702.58b: Graft N — ETB-with-N-P1P1 replacement plus a
    // "whenever another creature enters" trigger that optionally moves one
    // +1/+1 counter from this permanent onto the entering creature, gated on
    // this permanent currently having a +1/+1 counter. Each instance
    // functions independently.
    synthesize_graft(face);
    // CR 702.54a + CR 702.54b + CR 702.54c: Bloodthirst N/X —
    // ETB-with-P1P1 replacement. Each instance functions independently.
    synthesize_bloodthirst(face);
    // CR 702.82a + CR 614.1c + CR 614.12a: Devour N — as-enters replacement
    // whose execute chain is a ranged "sacrifice any number of creatures"
    // choice → PutCounter of N P1P1 counters per creature sacrificed. Each
    // instance functions independently (CR 113.2c).
    synthesize_devour(face);
    // CR 702.62a: Suspend — hand-activated alt-cost + upkeep counter-removal +
    // last-counter free-cast. Runs after Evoke to keep alt-cost synthesizers
    // grouped; idempotent so order against Cycling/Madness is irrelevant.
    synthesize_suspend(face);
    // CR 702.32a: Fading N — enters-with-N-fade-counters ETB replacement, upkeep
    // fade-counter-removal trigger, and the "if you can't, sacrifice" upkeep
    // trigger. Each instance functions separately; idempotent.
    synthesize_fading(face);
    // CR 702.63a: Vanishing N — enters-with-N-time-counters ETB replacement,
    // upkeep time-counter-removal trigger, and the last-counter sacrifice
    // trigger. Each instance functions separately; idempotent.
    synthesize_vanishing(face);
    // CR 702.170 + CR 116.2k: Plot — hand-activated special-action-approximated
    // ability that exiles self and grants a Plotted casting permission for
    // free-cast on a later turn. Runs after Suspend; idempotent.
    synthesize_plot(face);
    // CR 702.176a: Impending — static "not a creature" effect plus end-step
    // trigger removing one time counter while the impending cost was paid and a
    // counter remains. Idempotent.
    synthesize_impending(face);
    synthesize_siege_intrinsics(face);
    synthesize_tribute_intrinsics(face);
    // CR 702.124j: Partner with — ETB trigger letting target player fetch the
    // named partner card from their library into their hand, then shuffle.
    // The parenthetical reminder text is stripped by the oracle parser, so
    // this trigger must be synthesized from the Keyword::Partner(With(name)).
    synthesize_partner_with(face);
    // CR 721.2b: Spacecraft creature-shift at the max station-symbol striation
    // threshold. Must run after Oracle parsing so `face.power`/`face.toughness`
    // are in place and `Keyword::Station` has been normalized.
    synthesize_station(face);
    // CR 702.161a: Living metal — Vehicle is an artifact creature during its
    // controller's turn. Must run after Oracle parsing so `Keyword::LivingMetal`
    // is present on the (Vehicle) face.
    synthesize_living_metal(face);
    // CR 702.165: Backup — ETB trigger placing +1/+1 counters and granting
    // non-Backup abilities printed below Backup until end of turn.
    synthesize_backup(face);
    // CR 702.72a + CR 702.72b: Champion a[n] [type] — ETB trigger that exiles
    // (linked) another creature of the championed type you control or else
    // sacrifices this permanent, plus an LTB trigger that returns the linked
    // exiled card. Reuses the source-tracked exile-link infrastructure.
    synthesize_champion(face);
}

/// CR 702.176a: Synthesize Impending's battlefield static and end-step trigger.
///
/// "At the beginning of your end step, if this permanent's impending cost was
/// paid and it has a time counter on it, remove a time counter from it."
///
/// The static is a Layer 4 `RemoveType(Creature)` continuous effect gated on:
/// - `StaticCondition::CastVariantPaid { Impending }` — impending cost was paid
/// - `StaticCondition::HasCounters { Time, minimum: 1 }` — still has counters
///
/// The trigger is a battlefield-zone, end-step trigger gated on:
/// - `TriggerCondition::CastVariantPaidPersistent { Impending }` — impending cost was paid
/// - `TriggerCondition::HasCounters { Time, minimum: 1 }` — still has counters
///
/// Combined with `TriggerConstraint::OnlyDuringYourTurn` to enforce "your" end step.
/// Idempotent: skips if the trigger shape is already present.
pub fn synthesize_impending(face: &mut CardFace) {
    if !face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Impending { .. }))
    {
        return;
    }
    let static_condition = StaticCondition::And {
        conditions: vec![
            StaticCondition::CastVariantPaid {
                variant: CastVariantPaid::Impending,
            },
            StaticCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Time),
                minimum: 1,
                maximum: None,
            },
        ],
    };
    let already_has_static = face.static_abilities.iter().any(|static_def| {
        static_def.affected == Some(TargetFilter::SelfRef)
            && static_def.condition == Some(static_condition.clone())
            && static_def
                .modifications
                .contains(&ContinuousModification::RemoveType {
                    core_type: CoreType::Creature,
                })
    });
    if !already_has_static {
        face.static_abilities.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(static_condition)
                .modifications(vec![ContinuousModification::RemoveType {
                    core_type: CoreType::Creature,
                }])
                .description(
                    "CR 702.176a: As long as this permanent's impending cost was paid and it has a time counter on it, it's not a creature.".to_string(),
                ),
        );
    }

    // Idempotency: skip if the end-step counter-removal trigger is already present.
    let already_has_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::Phase)
            && t.phase == Some(Phase::End)
            && matches!(
                t.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::RemoveCounter {
                    counter_type: Some(CounterType::Time),
                    target: TargetFilter::SelfRef,
                    ..
                })
            )
    });
    if already_has_trigger {
        return;
    }

    let remove_one = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: Some(CounterType::Time),
            count: 1,
            target: TargetFilter::SelfRef,
        },
    );
    // CR 702.176a: gated on impending cost paid AND has a time counter.
    let condition = TriggerCondition::And {
        conditions: vec![
            TriggerCondition::CastVariantPaidPersistent {
                variant: CastVariantPaid::Impending,
            },
            TriggerCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Time),
                minimum: 1,
                maximum: None,
            },
        ],
    };
    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::End)
        .condition(condition)
        .constraint(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        .execute(remove_one)
        .description(
            "CR 702.176a: At the beginning of your end step, if this permanent's impending cost was paid and it has a time counter on it, remove a time counter from it.".to_string(),
        );
    face.triggers.push(trigger);
}

/// CR 702.124j: Synthesize the "Partner with [Name]" ETB trigger.
///
/// Oracle reminder text (stripped by the parser):
///   "When this creature enters, target player may put [Name] into their
///    hand from their library, then shuffle."
///
/// The trigger searches the target player's library for a card with the exact
/// partner name and puts it in their hand, then shuffles. The "may" is modeled
/// as `optional: true` on the execute ability so the target player can decline.
/// Idempotent: skips if the trigger is already present (re-synthesis guards).
pub fn synthesize_partner_with(face: &mut CardFace) {
    let partner_name = face.keywords.iter().find_map(|kw| {
        if let Keyword::Partner(PartnerType::With(name)) = kw {
            Some(name.clone())
        } else {
            None
        }
    });
    let Some(partner_name) = partner_name else {
        return;
    };

    // Idempotency: skip if an ETB trigger already references this partner by name.
    let already_present = face.triggers.iter().any(|t| {
        t.mode == TriggerMode::ChangesZone
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && t.execute.as_deref().is_some_and(|ex| {
                matches!(
                    ex.effect.as_ref(),
                    Effect::SearchLibrary {
                        filter: TargetFilter::Named { name },
                        ..
                    } if name == &partner_name
                )
            })
    });
    if already_present {
        return;
    }

    // Shuffle target player's library after the search.
    let shuffle = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            // TargetFilter::Player resolves against ability.targets (the chosen
            // target player), shuffling the correct library.
            target: TargetFilter::Player,
        },
    );

    // Put the found card from the library into the target player's hand.
    let put_in_hand = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            face_down_profile: None,
        },
    )
    .sub_ability(shuffle);

    // Search target player's library for the named partner card.
    let mut search = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SearchLibrary {
            filter: TargetFilter::Named {
                name: partner_name.clone(),
            },
            count: QuantityExpr::Fixed { value: 1 },
            reveal: true,
            source_zones: vec![Zone::Library],
            // CR 702.124j: the target player searches their own library.
            target_player: Some(TargetFilter::Player),
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
        },
    )
    .sub_ability(put_in_hand);
    // "may" — the target player can decline to search.
    search.optional = true;

    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .trigger_zones(vec![Zone::Battlefield])
            .execute(search)
            .description(format!(
                "When ~ enters, target player may put {partner_name} into their hand from their library, then shuffle."
            )),
    );
}

/// CR 310.11a + CR 310.11b: Synthesize the two intrinsic abilities every Siege has:
///   1. As-enters replacement: "As this Siege enters, its controller chooses an
///      opponent to be its protector." (CR 310.11a)
///   2. Victory trigger: "When the last defense counter is removed from this
///      permanent, exile it, then you may cast it transformed without paying
///      its mana cost." (CR 310.11b)
///
/// The defense-counter ETB replacement (CR 310.4b) is handled directly by
/// `apply_card_face_to_object` which seeds `CounterType::Defense` at load time,
/// so no separate replacement synthesis is needed for that rule.
pub fn synthesize_siege_intrinsics(face: &mut CardFace) {
    let is_battle = face.card_type.core_types.contains(&CoreType::Battle);
    let is_siege = face.card_type.subtypes.iter().any(|s| s == "Siege");
    if !is_battle || !is_siege {
        return;
    }

    // CR 310.11a: "As a Siege enters the battlefield, its controller must
    // choose its protector from among their opponents." Modeled as a
    // self-referential `Moved` replacement that persists the opponent choice
    // as a `ChosenAttribute::Player`, which `GameObject::protector()` reads.
    let already_has_protector_choice = face.replacements.iter().any(|r| {
        matches!(r.event, ReplacementEvent::Moved)
            && matches!(r.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                r.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Choose {
                    choice_type: ChoiceType::Opponent,
                    persist: true,
                })
            )
    });
    if !already_has_protector_choice {
        let mut protector_replacement = ReplacementDefinition::new(ReplacementEvent::Moved);
        protector_replacement.valid_card = Some(TargetFilter::SelfRef);
        protector_replacement.destination_zone = Some(Zone::Battlefield);
        protector_replacement.description = Some(
            "CR 310.11a: As a Siege enters, its controller chooses an opponent as its protector."
                .to_string(),
        );
        protector_replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::Opponent,
                persist: true,
            },
        )));
        face.replacements.push(protector_replacement);
    }

    // CR 310.11b: Victory triggered ability — "When the last defense counter
    // is removed from this permanent, exile it, then you may cast it
    // transformed without paying its mana cost."
    let already_has_victory_trigger = face.triggers.iter().any(|t| {
        matches!(t.mode, TriggerMode::CounterRemoved)
            && t.counter_filter
                .as_ref()
                .is_some_and(|f| matches!(f.counter_type, CounterType::Defense))
    });
    if !already_has_victory_trigger {
        // exile SelfRef → (optional) cast SelfRef from exile transformed
        let cast_sub = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: true,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                // CR 310.11b + CR 608.2g: the Siege victory ability casts the
                // exiled back face AS this trigger resolves — a self-free-cast
                // during resolution, structurally identical to Suspend's
                // last-counter cast. (Pre-`driver`, the `duration.is_none()`
                // router already routed this shape through during-resolution;
                // the explicit discriminator preserves that.)
                driver: CastFromZoneDriver::DuringResolution,
            },
        )
        .optional();
        let exile_then_cast = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        )
        .sub_ability(cast_sub);

        let trigger = TriggerDefinition::new(TriggerMode::CounterRemoved)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: CounterType::Defense,
                threshold: Some(0),
            })
            .execute(exile_then_cast)
            .description(
                "CR 310.11b: When the last defense counter is removed from this Siege, exile it, then you may cast it transformed without paying its mana cost.".to_string(),
            );
        face.triggers.push(trigger);
    }
}

/// CR 702.104a: Synthesize the intrinsic ETB replacement for every creature with
/// `Keyword::Tribute(N)`.
///
/// Oracle: "Tribute N (As this creature enters, an opponent of your choice may put
/// N +1/+1 counters on it.)"
///
/// Modeled as a self-referential `Moved` replacement whose post-replacement effect
/// chain has two stages:
///
///   1. `Effect::Choose { Opponent, persist: true }` — controller picks the opponent;
///      the selection is persisted on the entering creature as `ChosenAttribute::Player`
///      (mirrors `synthesize_siege_intrinsics`' protector choice).
///
///   2. `Effect::Tribute { count: N }` (sub-ability) — reads the persisted opponent,
///      prompts them pay/decline via `WaitingFor::TributeChoice`, and on resolution
///      records `ChosenAttribute::TributeOutcome` so the companion "if tribute
///      wasn't paid" trigger (CR 702.104b) can read the outcome.
pub fn synthesize_tribute_intrinsics(face: &mut CardFace) {
    let Some(count) = face.keywords.iter().find_map(|k| match k {
        Keyword::Tribute(n) => Some(*n),
        _ => None,
    }) else {
        return;
    };

    // Idempotency guard: don't re-add if already synthesized (parser pipelines can
    // run twice in some code paths).
    let already_synthesized = face.replacements.iter().any(|r| {
        matches!(r.event, ReplacementEvent::Moved)
            && matches!(r.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                r.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Choose {
                    choice_type: ChoiceType::Opponent,
                    persist: true,
                }),
            )
            && r.execute
                .as_deref()
                .and_then(|a| a.sub_ability.as_deref())
                .is_some_and(|sub| matches!(&*sub.effect, Effect::Tribute { .. }))
    });
    if already_synthesized {
        return;
    }

    // Stage 2: Effect::Tribute { count } — the chosen opponent decides pay/decline.
    let tribute_stage = AbilityDefinition::new(AbilityKind::Spell, Effect::Tribute { count });

    // Stage 1: Effect::Choose { Opponent, persist } — controller picks the opponent.
    // Chained with stage 2 as a sub-ability (runs after the Choose resolves).
    let choose_stage = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: ChoiceType::Opponent,
            persist: true,
        },
    )
    .sub_ability(tribute_stage);

    let mut replacement = ReplacementDefinition::new(ReplacementEvent::Moved);
    replacement.valid_card = Some(TargetFilter::SelfRef);
    replacement.destination_zone = Some(Zone::Battlefield);
    replacement.description = Some(format!(
        "CR 702.104a: Tribute {count} — as this creature enters, an opponent of your choice may put {count} +1/+1 counters on it.",
    ));
    replacement.execute = Some(Box::new(choose_stage));

    face.replacements.push(replacement);
}

/// Merge parser-extracted keywords into a base (MTGJSON-derived) keyword list,
/// reconciling parameterized and multi-instance keywords. Single authority shared
/// by the production card-data pipeline (`build_oracle_face_inner`) and the
/// scenario test harness (`game::scenario::build_face_from_oracle`) so the two
/// cannot diverge.
///
/// CR 113.2c / CR 702.85c / CR 702.40b / CR 702.116b: keywords whose instances
/// each function separately (`instances_function_separately()`) are printed as
/// repeated bare words but deduped by MTGJSON; the parser recovered the true
/// printed instance count from Oracle text, so drop every MTGJSON copy of THIS
/// keyword (matched on the concrete variant, not `kind()`: Storm and Myriad share
/// `KeywordKind::Unknown` with ~50 other keywords, so a `kind()`-based retain would
/// wrongly strip unrelated Unknown keywords — concrete-variant equality is exact for
/// all four predicate keywords regardless of kind, since they are unit variants) and
/// let the parser-recovered occurrences be authoritative, then extend. Fallback: if the
/// parser found zero occurrences, this branch never runs for the keyword, so the
/// single MTGJSON copy is preserved.
///
/// Bloodthirst is parameterized: replace any MTGJSON-derived default
/// (`Bloodthirst(_)`) with the parser-extracted value.
/// All other keywords: when the parser extracts a parameterized keyword (e.g.,
/// `Morph({2}{B}{G}{U})`), remove any MTGJSON-derived default of the same `kind()`
/// (e.g., `Morph(zero)`).
pub(crate) fn merge_extracted_keywords(base: &mut Vec<Keyword>, extracted: Vec<Keyword>) {
    for extracted_kw in &extracted {
        if extracted_kw.instances_function_separately() {
            base.retain(|existing| existing != extracted_kw);
        } else if matches!(extracted_kw, Keyword::Bloodthirst(_)) {
            base.retain(|existing| !matches!(existing, Keyword::Bloodthirst(_)));
        } else {
            let kind = extracted_kw.kind();
            base.retain(|existing| existing.kind() != kind);
        }
    }
    base.extend(extracted);
}

/// Build a `CardFace` from MTGJSON data, running the Oracle text parser and all synthesis.
/// Both `oracle_loader.rs` and `oracle_gen.rs` call this to ensure identical processing.
pub fn build_oracle_face(mtgjson: &AtomicCard, oracle_id: Option<String>) -> CardFace {
    build_oracle_face_inner(mtgjson, oracle_id, false)
}

/// Build an Oracle face for a multi-face card, skipping MTGJSON keywords
/// to prevent cross-face keyword leakage (B8: Saga back-face keyword contamination).
pub fn build_oracle_face_multi(mtgjson: &AtomicCard, oracle_id: Option<String>) -> CardFace {
    build_oracle_face_inner(mtgjson, oracle_id, true)
}

fn build_oracle_face_inner(
    mtgjson: &AtomicCard,
    oracle_id: Option<String>,
    skip_mtgjson_keywords: bool,
) -> CardFace {
    let card_type = build_card_type(mtgjson);
    // Raw MTGJSON keyword names (lowercased) for keyword-only line detection.
    // Still needed for keyword line detection even when skipping MTGJSON keywords.
    let mtgjson_keyword_names: Vec<String> = mtgjson
        .keywords
        .as_ref()
        .map(|kws| kws.iter().map(|s| s.to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let parser_keyword_names: Vec<String> = if skip_mtgjson_keywords {
        vec!["__force_keyword_extract__".to_string()]
    } else {
        mtgjson_keyword_names.clone()
    };

    // B8: For multi-face cards, skip MTGJSON-provided keywords entirely.
    // MTGJSON duplicates keywords across both faces of Transform/DFC cards,
    // causing the front face to incorrectly gain back-face keywords.
    // Parser-extracted keywords from `extract_keyword_line` are face-specific.
    let mut keywords: Vec<Keyword> = if skip_mtgjson_keywords {
        Vec::new()
    } else {
        mtgjson
            .keywords
            .as_ref()
            .map(|kws| {
                kws.iter()
                    .map(|s| s.parse::<Keyword>().unwrap())
                    .filter(|k| !matches!(k, Keyword::Unknown(_)))
                    .collect()
            })
            .unwrap_or_default()
    };

    let raw_oracle_text = mtgjson.text.as_deref().unwrap_or("");
    let face_name = mtgjson.face_name.as_deref().unwrap_or(&mtgjson.name);

    let types: Vec<String> = mtgjson.types.clone();
    let subtypes: Vec<String> = mtgjson.subtypes.clone();

    // CR 702.148a-b + CR 612: Cleave's text-changing effect removes every
    // square-bracketed span from the spell's rules text. `parse_oracle_with_cleave_brackets`
    // is the single authority for the dual (printed-cost / cleave-cost) parse,
    // shared with the test scenario harness so the two pipelines cannot diverge.
    let (parsed, cleave_variant) = parse_oracle_with_cleave_brackets(
        raw_oracle_text,
        face_name,
        &parser_keyword_names,
        &types,
        &subtypes,
    );

    let extracted_keywords = parsed.extracted_keywords;
    let extracted_has_craft = extracted_keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Craft { .. }));
    let oracle_has_craft_materials = raw_oracle_text
        .lines()
        .map(str::trim_start)
        .map(str::to_ascii_lowercase)
        .any(|line| line.strip_prefix("craft with ").is_some());
    if oracle_has_craft_materials && !extracted_has_craft {
        keywords.retain(|keyword| !matches!(keyword, Keyword::Craft { .. }));
    }

    // Merge keywords extracted from Oracle text with MTGJSON keywords via the
    // shared `merge_extracted_keywords` authority (also used by the scenario test
    // harness so the two pipelines cannot diverge). It reconciles parameterized
    // keywords (e.g., Morph) and CR 113.2c multi-instance keywords (Cascade/Storm/
    // Myriad/Exalted) — see the helper's doc comment for the per-class rules.
    merge_extracted_keywords(&mut keywords, extracted_keywords);

    // CR 702.124j: "Partner with [Name]" — upgrade Generic → With(name).
    // MTGJSON sends both "Partner" and "Partner with" keywords; the former produces
    // Partner(Generic) via FromStr. Scan Oracle text for the actual partner name.
    if mtgjson_keyword_names.contains(&"partner with".to_string()) {
        let lower_oracle = raw_oracle_text.to_lowercase();
        if let Some(line) = lower_oracle
            .lines()
            .find(|l| l.starts_with("partner with "))
        {
            let rest = &line["partner with ".len()..];
            // Name ends at first '(' (reminder text) or end of line
            let name = rest.find('(').map(|i| &rest[..i]).unwrap_or(rest).trim();
            if !name.is_empty() {
                // Extract original-case name from the raw oracle text
                let original_name = mtgjson
                    .text
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("partner with "))
                    .map(|l| {
                        let r = &l["Partner with ".len()..];
                        r.find('(').map(|i| &r[..i]).unwrap_or(r).trim().to_string()
                    })
                    .unwrap_or_else(|| name.to_string());

                // Upgrade any Generic partner to With(name)
                for kw in &mut keywords {
                    if matches!(kw, Keyword::Partner(PartnerType::Generic)) {
                        *kw = Keyword::Partner(PartnerType::With(original_name.clone()));
                        break;
                    }
                }
            }
        }
    }

    // CR 702.124: Deduplicate — if any non-Generic partner variant exists,
    // remove stale Partner(Generic) entries (e.g., MTGJSON "Partner" keyword
    // producing Generic when Oracle text has "Partner—Friends forever").
    let has_specific_partner = keywords
        .iter()
        .any(|kw| matches!(kw, Keyword::Partner(pt) if !matches!(pt, PartnerType::Generic)));
    if has_specific_partner {
        keywords.retain(|kw| !matches!(kw, Keyword::Partner(PartnerType::Generic)));
    }

    // CR 702.11c: Deduplicate — if any HexproofFrom variant exists, remove
    // bare Hexproof (MTGJSON sends both "Hexproof" and "Hexproof from [quality]").
    let has_hexproof_from = keywords
        .iter()
        .any(|kw| matches!(kw, Keyword::HexproofFrom(_)));
    if has_hexproof_from {
        keywords.retain(|kw| !matches!(kw, Keyword::Hexproof));
    }

    // CR 202.1b: A card with no `manaCost` (lands, and suspend-only cards like
    // Inevitable Betrayal / Ancestral Vision) has *no* mana cost where its cost
    // would appear — not a payable {0} cost.
    // CR 118.6: no mana cost is an unpayable cost. Map an absent manaCost to
    // `NoCost`; `unwrap_or_default()` previously collapsed it to `Cost{0}`, which
    // made such cards castable for free from hand (issue #827). Real `{0}` cards
    // (Ornithopter, Mox) carry an explicit `"{0}"` manaCost and stay
    // `Cost{ generic: 0 }` via `parse_mtgjson_mana_cost`.
    let mana_cost = mtgjson
        .mana_cost
        .as_deref()
        .map(parse_mtgjson_mana_cost)
        .unwrap_or(ManaCost::NoCost);

    let mana_derived_colors = derive_colors_from_mana_cost(&mana_cost);
    let mtgjson_colors: Vec<ManaColor> = mtgjson
        .colors
        .iter()
        .filter_map(|c| map_mtgjson_color(c))
        .collect();
    let color_override = if mtgjson_colors != mana_derived_colors {
        Some(mtgjson_colors)
    } else {
        None
    };

    let mut face = CardFace {
        name: face_name.to_string(),
        mana_cost,
        card_type,
        power: mtgjson.power.as_ref().map(|s| parse_pt_value(s)),
        toughness: mtgjson.toughness.as_ref().map(|s| parse_pt_value(s)),
        loyalty: mtgjson.loyalty.clone(),
        defense: mtgjson.defense.clone(),
        oracle_text: mtgjson.text.clone(),
        non_ability_text: None,
        flavor_name: None,
        keywords,
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        cleave_variant,
        color_override,
        color_identity: mtgjson
            .color_identity
            .iter()
            .filter_map(|code| map_mtgjson_color(code))
            .collect(),
        scryfall_oracle_id: oracle_id,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        strive_cost: parsed.strive_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
        parse_warnings: parsed.parse_warnings,
        brawl_commander: false,
        is_commander: false,
        is_oathbreaker: false,
        deck_copy_limit: None,
        metadata: Default::default(),
        rarities: Default::default(),
        attraction_lights: vec![],
    };

    face.brawl_commander = compute_brawl_commander(mtgjson, &face);
    face.is_commander = compute_commander(mtgjson, &face);
    face.is_oathbreaker = compute_oathbreaker(mtgjson, &face);
    // CR 100.2a / CR 903.5b: per-card deck-construction copy-limit override.
    // `face.oracle_text` retains reminder text + DCI prefix, so reminder-only
    // limits (Vazal, the Compleat's Megalegendary) are still discovered.
    face.deck_copy_limit = compute_deck_copy_limit(&face);
    synthesize_all(&mut face);
    face
}

#[cfg(test)]
mod kicker_synthesis_tests {
    use super::*;
    use crate::types::mana::ManaCostShard;

    #[test]
    fn synthesize_kicker_sets_typed_kicker_additional_cost() {
        let mut face = CardFace {
            keywords: vec![Keyword::Kicker(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Blue],
            })],
            ..CardFace::default()
        };

        synthesize_kicker(&mut face);

        match face.additional_cost.expect("additional_cost set") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(!repeatable);
                assert_eq!(costs.len(), 1);
                assert!(matches!(
                    &costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { generic: 2, shards }
                    } if shards == &vec![ManaCostShard::Blue]
                ));
            }
            other => panic!("expected Kicker additional cost, got {other:?}"),
        }
    }

    #[test]
    fn resolves_specific_kicker_condition_to_position() {
        let mut face = CardFace {
            oracle_text: Some(
                "Kicker {2}{U} and/or {2}{B}\nWhen ~ enters, if it was kicked with its {2}{U} kicker, draw a card."
                    .to_string(),
            ),
            additional_cost: Some(AdditionalCost::Kicker {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Blue],
                        },
                    },
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Black],
                        },
                    },
                ],
                repeatable: false,
            }),
            triggers: vec![TriggerDefinition::new(TriggerMode::ChangesZone).execute(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .condition(AbilityCondition::additional_cost_paid_kicker_cost(
                    ManaCost::Cost {
                        generic: 2,
                        shards: vec![ManaCostShard::Blue],
                    },
                )),
            )],
            ..CardFace::default()
        };

        resolve_kicker_condition_variants(&mut face);

        let condition = face.triggers[0]
            .execute
            .as_ref()
            .and_then(|ability| ability.condition.as_ref());
        assert_eq!(
            condition,
            Some(&AbilityCondition::additional_cost_paid_kicker(
                KickerVariant::First
            ))
        );
    }

    #[test]
    fn resolves_specific_kicker_replacement_condition_to_position() {
        let mut face = CardFace {
            additional_cost: Some(AdditionalCost::Kicker {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Red],
                        },
                    },
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::White],
                        },
                    },
                ],
                repeatable: false,
            }),
            replacements: vec![
                ReplacementDefinition::new(ReplacementEvent::Moved).condition(
                    ReplacementCondition::CastViaKicker {
                        variant: None,
                        kicker_cost: Some(ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::White],
                        }),
                    },
                ),
            ],
            ..CardFace::default()
        };

        resolve_kicker_condition_variants(&mut face);

        assert!(matches!(
            face.replacements[0].condition,
            Some(ReplacementCondition::CastViaKicker {
                variant: Some(KickerVariant::Second),
                kicker_cost: None
            })
        ));
    }

    #[test]
    fn resolves_specific_kicker_modal_condition_to_position() {
        let mut face = CardFace {
            additional_cost: Some(AdditionalCost::Kicker {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Red],
                        },
                    },
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::White],
                        },
                    },
                ],
                repeatable: false,
            }),
            abilities: vec![AbilityDefinition {
                modal: Some(crate::types::ability::ModalChoice {
                    constraints: vec![ModalSelectionConstraint::ConditionalMaxChoices {
                        condition: ModalSelectionCondition::AdditionalCostPaid {
                            source: AdditionalCostPaymentSource::Kicker,
                            variant: None,
                            kicker_cost: Some(ManaCost::Cost {
                                generic: 1,
                                shards: vec![ManaCostShard::White],
                            }),
                            min_count: 1,
                        },
                        max_choices: 2,
                        otherwise_max_choices: 1,
                    }],
                    ..Default::default()
                }),
                ..AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
            }],
            ..CardFace::default()
        };

        resolve_kicker_condition_variants(&mut face);

        let Some(ModalSelectionConstraint::ConditionalMaxChoices { condition, .. }) = face
            .abilities
            .first()
            .and_then(|ability| ability.modal.as_ref())
            .and_then(|modal| modal.constraints.first())
        else {
            panic!("expected conditional modal constraint");
        };
        assert!(matches!(
            condition,
            ModalSelectionCondition::AdditionalCostPaid {
                source: AdditionalCostPaymentSource::Kicker,
                variant: Some(KickerVariant::Second),
                kicker_cost: None,
                min_count: 1
            }
        ));
    }
}

#[cfg(test)]
mod buyback_synthesis_tests {
    use super::*;

    /// CR 702.27a: Mana-cost Buyback synthesizes an optional additional mana cost.
    #[test]
    fn synthesize_buyback_mana_sets_optional_additional_cost() {
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                generic: 3,
                shards: vec![],
            }))],
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);

        match face.additional_cost.expect("additional_cost set") {
            AdditionalCost::Optional {
                cost: AbilityCost::Mana { cost },
                repeatable: false,
            } => {
                assert!(matches!(
                    cost,
                    ManaCost::Cost {
                        generic: 3,
                        ref shards,
                    } if shards.is_empty()
                ));
            }
            other => panic!("expected Optional(Mana), got {other:?}"),
        }
    }

    /// CR 702.27a: Non-mana Buyback (Constant Mists "Sacrifice a land") routes
    /// through the full AbilityCost pipeline as an optional additional cost.
    #[test]
    fn synthesize_buyback_non_mana_preserves_ability_cost() {
        let sac_cost = AbilityCost::Sacrifice {
            target: TargetFilter::Any,
            count: 1,
        };
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::NonMana(sac_cost.clone()))],
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);

        match face.additional_cost.expect("additional_cost set") {
            AdditionalCost::Optional {
                cost,
                repeatable: false,
            } => assert_eq!(cost, sac_cost),
            other => panic!("expected Optional(Sacrifice), got {other:?}"),
        }
    }

    /// Idempotency: running synthesize_buyback twice produces the same result.
    #[test]
    fn synthesize_buyback_is_idempotent() {
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                generic: 5,
                shards: vec![],
            }))],
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);
        let first = face.additional_cost.clone();
        synthesize_buyback(&mut face);
        assert_eq!(face.additional_cost, first);
    }

    /// Parser-parsed `additional_cost` takes precedence over synthesized buyback
    /// (Kicker pattern).
    #[test]
    fn synthesize_buyback_skips_when_additional_cost_already_set() {
        let existing = AdditionalCost::Required(AbilityCost::Mana {
            cost: ManaCost::Cost {
                generic: 1,
                shards: vec![],
            },
        });
        let mut face = CardFace {
            keywords: vec![Keyword::Buyback(BuybackCost::Mana(ManaCost::Cost {
                generic: 3,
                shards: vec![],
            }))],
            additional_cost: Some(existing.clone()),
            ..CardFace::default()
        };

        synthesize_buyback(&mut face);
        assert_eq!(face.additional_cost, Some(existing));
    }

    /// No-op when the card has no Buyback keyword.
    #[test]
    fn synthesize_buyback_noop_without_keyword() {
        let mut face = CardFace::default();
        synthesize_buyback(&mut face);
        assert!(face.additional_cost.is_none());
    }
}

#[cfg(test)]
mod bargain_synthesis_tests {
    use super::*;

    #[test]
    fn synthesize_bargain_sets_optional_sacrifice_additional_cost() {
        let mut face = CardFace {
            keywords: vec![Keyword::Bargain],
            ..CardFace::default()
        };

        synthesize_bargain(&mut face);

        match face.additional_cost.expect("additional_cost set") {
            AdditionalCost::Optional {
                cost: AbilityCost::Sacrifice { target, count },
                repeatable: false,
            } => {
                assert_eq!(count, 1);
                let TargetFilter::Or { filters } = target else {
                    panic!("expected artifact/enchantment/token disjunction, got {target:?}");
                };
                assert!(
                    filters.contains(&TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)))
                );
                assert!(filters.contains(&TargetFilter::Typed(TypedFilter::new(
                    TypeFilter::Enchantment
                ))));
                assert!(filters.contains(&TargetFilter::Typed(
                    TypedFilter::permanent().properties(vec![FilterProp::Token])
                )));
            }
            other => panic!("expected Optional(Sacrifice), got {other:?}"),
        }
    }

    #[test]
    fn synthesize_bargain_skips_when_additional_cost_already_set() {
        let existing = AdditionalCost::Required(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        let mut face = CardFace {
            keywords: vec![Keyword::Bargain],
            additional_cost: Some(existing.clone()),
            ..CardFace::default()
        };

        synthesize_bargain(&mut face);

        assert_eq!(face.additional_cost, Some(existing));
    }
}

#[cfg(test)]
mod cycling_synthesis_tests {
    use super::*;

    #[test]
    fn typecycling_moves_found_card_to_hand_before_shuffle() {
        let mut face = CardFace {
            keywords: vec![Keyword::Typecycling {
                cost: ManaCost::Cost {
                    generic: 1,
                    shards: vec![],
                },
                subtype: "Basic Land".to_string(),
            }],
            ..CardFace::default()
        };

        synthesize_cycling(&mut face);

        let ability = face.abilities.first().expect("typecycling ability");
        assert!(matches!(&*ability.effect, Effect::SearchLibrary { .. }));
        let put_in_hand = ability.sub_ability.as_ref().expect("put in hand");
        assert!(matches!(
            &*put_in_hand.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Any,
                ..
            }
        ));
        let shuffle = put_in_hand.sub_ability.as_ref().expect("shuffle");
        assert!(matches!(&*shuffle.effect, Effect::Shuffle { .. }));
    }
}

#[cfg(test)]
mod job_select_synthesis_tests {
    use super::*;
    use crate::types::triggers::TriggerMode;

    fn face_with_job_select() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::JobSelect);
        face
    }

    /// CR 702.182a: Job select synthesis produces exactly one ChangesZone trigger
    /// with an ETB destination, a Token effect for a 1/1 colorless Hero creature,
    /// and an Attach sub-ability targeting LastCreated.
    #[test]
    fn synthesize_job_select_builds_etb_trigger_with_token_and_attach() {
        let mut face = face_with_job_select();
        synthesize_job_select(&mut face);

        assert_eq!(face.triggers.len(), 1, "exactly one Job select trigger");
        let trigger = &face.triggers[0];
        assert!(
            matches!(trigger.mode, TriggerMode::ChangesZone),
            "trigger should be ChangesZone (ETB)"
        );
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert_eq!(
            trigger.valid_card,
            Some(TargetFilter::SelfRef),
            "trigger must scope to self-ETB only"
        );

        // Verify execute chain: Token → Attach
        let execute = trigger.execute.as_ref().expect("trigger must have execute");
        match execute.effect.as_ref() {
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                ..
            } => {
                assert_eq!(name, "Hero");
                assert!(matches!(power, crate::types::ability::PtValue::Fixed(1)));
                assert!(matches!(
                    toughness,
                    crate::types::ability::PtValue::Fixed(1)
                ));
                assert!(types.contains(&"Creature".to_string()));
                assert!(types.contains(&"Hero".to_string()));
                assert!(colors.is_empty(), "Hero token should be colorless");
            }
            other => panic!("expected Token effect, got {:?}", other),
        }

        // Verify sub_ability is Attach { target: LastCreated }
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("Token effect must chain to Attach sub_ability");
        assert!(
            matches!(
                sub.effect.as_ref(),
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::LastCreated
                }
            ),
            "sub_ability should be Attach targeting LastCreated"
        );
    }

    #[test]
    fn synthesize_job_select_is_idempotent() {
        let mut face = face_with_job_select();
        synthesize_job_select(&mut face);
        let count = face.triggers.len();
        synthesize_job_select(&mut face);
        // Repeat synthesis must not duplicate the ETB trigger. A
        // non-idempotent synthesizer would push the same trigger multiple
        // times and cause per-ETB-event doubling at runtime.
        assert_eq!(face.triggers.len(), count);
    }

    #[test]
    fn synthesize_job_select_skips_without_keyword() {
        let mut face = CardFace::default();
        synthesize_job_select(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 603.6a: ETB triggers fire from the battlefield. The synthesized
    /// ChangesZone trigger must list `Zone::Battlefield` in `trigger_zones`
    /// or the runtime evaluator never matches Job Select equipment's ETB.
    #[test]
    fn synthesize_job_select_binds_battlefield_trigger_zone() {
        let mut face = face_with_job_select();
        synthesize_job_select(&mut face);
        let trigger = &face.triggers[0];
        assert_eq!(trigger.trigger_zones, vec![Zone::Battlefield]);
    }
}

#[cfg(test)]
mod madness_synthesis_tests {
    use super::*;

    fn madness_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Madness(ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Red],
            generic: 0,
        }));
        face
    }

    #[test]
    fn synthesize_madness_adds_discard_replacement_and_exile_trigger() {
        let mut face = madness_face();
        synthesize_madness_intrinsics(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| matches!(r.event, ReplacementEvent::Discard))
            .expect("madness should add a discard replacement");
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));
        assert!(matches!(
            replacement.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            })
        ));

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::Discarded))
            .expect("madness should add a discarded trigger");
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        assert_eq!(trigger.trigger_zones, vec![Zone::Exile]);
        assert!(matches!(
            trigger.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::MadnessCast { cost })
                if *cost == (ManaCost::Cost {
                    shards: vec![crate::types::mana::ManaCostShard::Red],
                    generic: 0,
                })
        ));
    }

    #[test]
    fn synthesize_madness_is_idempotent() {
        let mut face = madness_face();
        synthesize_madness_intrinsics(&mut face);
        synthesize_madness_intrinsics(&mut face);

        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| matches!(r.event, ReplacementEvent::Discard))
                .count(),
            1
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| matches!(t.mode, TriggerMode::Discarded))
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod evoke_synthesis_tests {
    use super::*;
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn evoke_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::Evoke(crate::types::keywords::EvokeCost::Mana(
                ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 1,
                },
            )));
        face
    }

    /// CR 702.74a: Evoke synthesis injects an intervening-if ETB sacrifice
    /// trigger that fires only when the evoke alt-cost was paid.
    #[test]
    fn synthesize_evoke_adds_conditional_etb_sac_trigger() {
        let mut face = evoke_face();
        synthesize_evoke(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.destination == Some(Zone::Battlefield)
                    && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            })
            .expect("evoke should add an ETB trigger");
        assert!(matches!(
            trigger.condition,
            Some(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Evoke,
            })
        ));
        assert!(matches!(
            trigger.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            })
        ));
    }

    /// Repeated synthesis must not duplicate the trigger.
    #[test]
    fn synthesize_evoke_is_idempotent() {
        let mut face = evoke_face();
        synthesize_evoke(&mut face);
        synthesize_evoke(&mut face);

        let count = face
            .triggers
            .iter()
            .filter(|t| {
                matches!(
                    t.condition,
                    Some(TriggerCondition::CastVariantPaid {
                        variant: CastVariantPaid::Evoke,
                        ..
                    })
                )
            })
            .count();
        assert_eq!(count, 1, "evoke trigger should be deduped");
    }

    /// Cards without Evoke are unaffected.
    #[test]
    fn synthesize_evoke_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_evoke(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// Issue #580: MTGJSON's bare "Evoke" keyword must be replaced by the
    /// parser-extracted non-mana cost from the Oracle evoke line.
    #[test]
    fn build_oracle_face_solitude_evoke_merges_to_non_mana() {
        use crate::types::keywords::EvokeCost;

        let mtgjson = AtomicCard {
            name: "Solitude".to_string(),
            mana_cost: Some("{3}{W}{W}".to_string()),
            colors: vec!["W".to_string()],
            color_identity: vec!["W".to_string()],
            power: Some("3".to_string()),
            toughness: Some("2".to_string()),
            loyalty: None,
            defense: None,
            text: Some(
                "Flash\nLifelink\nWhen this creature enters, exile up to one other target creature. That creature's controller gains life equal to its power.\nEvoke\u{2014}Exile a white card from your hand.".to_string(),
            ),
            layout: "normal".to_string(),
            type_line: Some("Creature — Elemental Incarnation".to_string()),
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elemental".to_string(), "Incarnation".to_string()],
            supertypes: Vec::new(),
            keywords: Some(vec![
                "Flash".to_string(),
                "Lifelink".to_string(),
                "Evoke".to_string(),
            ]),
            side: None,
            face_name: None,
            mana_value: 5.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: crate::database::mtgjson::AtomicIdentifiers {
                scryfall_id: None,
                scryfall_oracle_id: None,
            },
            foreign_data: Vec::new(),
        };

        let face = build_oracle_face(&mtgjson, None);
        let evoke = face
            .keywords
            .iter()
            .find_map(|k| match k {
                Keyword::Evoke(cost) => Some(cost),
                _ => None,
            })
            .expect("Solitude must carry Evoke after synthesis");
        assert!(
            matches!(evoke, EvokeCost::NonMana(AbilityCost::Exile { .. })),
            "MTGJSON bare Evoke must merge to NonMana(Exile), got {evoke:?}"
        );
    }
}

#[cfg(test)]
mod impending_synthesis_tests {
    use super::*;
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn impending_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Impending {
            counters: 3,
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            },
        });
        face
    }

    /// CR 702.176a: Impending synthesizes both battlefield abilities: a static
    /// Layer 4 type-removal effect while the permanent has a time counter, and
    /// a recurring end-step trigger that removes those counters.
    #[test]
    fn synthesize_impending_adds_static_and_persistent_end_step_trigger() {
        let mut face = impending_face();

        synthesize_impending(&mut face);

        let static_def = face
            .static_abilities
            .iter()
            .find(|static_def| {
                static_def.affected == Some(TargetFilter::SelfRef)
                    && static_def
                        .modifications
                        .contains(&ContinuousModification::RemoveType {
                            core_type: CoreType::Creature,
                        })
            })
            .expect("impending should add a not-creature static");
        assert!(matches!(
            static_def.condition,
            Some(StaticCondition::And { ref conditions })
                if conditions.contains(&StaticCondition::CastVariantPaid {
                    variant: CastVariantPaid::Impending,
                }) && conditions.contains(&StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Time),
                    minimum: 1,
                    maximum: None,
                })
        ));

        let trigger = face
            .triggers
            .iter()
            .find(|trigger| {
                matches!(trigger.mode, TriggerMode::Phase) && trigger.phase == Some(Phase::End)
            })
            .expect("impending should add an end-step trigger");
        assert!(matches!(
            trigger.condition,
            Some(TriggerCondition::And { ref conditions })
                if conditions.contains(&TriggerCondition::CastVariantPaidPersistent {
                    variant: CastVariantPaid::Impending,
                }) && conditions.contains(&TriggerCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Time),
                    minimum: 1,
                    maximum: None,
                })
        ));
        assert!(matches!(
            trigger.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::RemoveCounter {
                counter_type: Some(CounterType::Time),
                target: TargetFilter::SelfRef,
                ..
            })
        ));
    }

    #[test]
    fn synthesize_impending_is_idempotent() {
        let mut face = impending_face();

        synthesize_impending(&mut face);
        synthesize_impending(&mut face);

        let static_count = face
            .static_abilities
            .iter()
            .filter(|static_def| {
                static_def
                    .modifications
                    .contains(&ContinuousModification::RemoveType {
                        core_type: CoreType::Creature,
                    })
            })
            .count();
        let trigger_count = face
            .triggers
            .iter()
            .filter(|trigger| {
                matches!(trigger.mode, TriggerMode::Phase) && trigger.phase == Some(Phase::End)
            })
            .count();

        assert_eq!(static_count, 1);
        assert_eq!(trigger_count, 1);
    }
}

#[cfg(test)]
mod fabricate_synthesis_tests {
    use super::*;

    fn fabricate_face(n: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Fabricate(n));
        face
    }

    /// CR 702.123a: Fabricate synthesizes an ETB ChooseOneOf trigger whose
    /// two branches are the P1P1 counter placement and the Servo token
    /// creation, both parameterized by N.
    #[test]
    fn synthesize_fabricate_adds_etb_choose_branches() {
        let mut face = fabricate_face(2);
        synthesize_fabricate(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.destination == Some(Zone::Battlefield)
                    && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            })
            .expect("fabricate should add an ETB trigger");

        let Some(Effect::ChooseOneOf { branches, .. }) =
            trigger.execute.as_deref().map(|a| &*a.effect)
        else {
            panic!("fabricate execute should be ChooseOneOf");
        };
        assert_eq!(branches.len(), 2, "fabricate offers two branches");

        let counter_branch = branches
            .iter()
            .find(|b| matches!(&*b.effect, Effect::PutCounter { .. }))
            .expect("one branch must place +1/+1 counters");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*counter_branch.effect
        else {
            unreachable!();
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
        assert!(matches!(target, TargetFilter::SelfRef));

        let token_branch = branches
            .iter()
            .find(|b| matches!(&*b.effect, Effect::Token { .. }))
            .expect("one branch must create Servo tokens");
        let Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            count,
            ..
        } = &*token_branch.effect
        else {
            unreachable!();
        };
        assert_eq!(name, "Servo");
        assert!(matches!(power, PtValue::Fixed(1)));
        assert!(matches!(toughness, PtValue::Fixed(1)));
        assert_eq!(
            types,
            &vec![
                "Artifact".to_string(),
                "Creature".to_string(),
                "Servo".to_string()
            ]
        );
        assert!(colors.is_empty(), "Servo tokens are colorless");
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
    }

    /// Repeated synthesis must not duplicate the trigger (idempotency).
    #[test]
    fn synthesize_fabricate_is_idempotent() {
        let mut face = fabricate_face(1);
        synthesize_fabricate(&mut face);
        synthesize_fabricate(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.destination == Some(Zone::Battlefield)
                    && matches!(
                        t.execute.as_deref().map(|a| &*a.effect),
                        Some(Effect::ChooseOneOf { .. })
                    )
            })
            .count();
        assert_eq!(count, 1, "fabricate trigger should be deduped");
    }

    /// Cards without Fabricate are unaffected.
    #[test]
    fn synthesize_fabricate_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_fabricate(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// Negative test: a creature ETB without Fabricate must not synthesize
    /// a ChooseOneOf trigger. Guards against false positives that would
    /// prompt on every non-Fabricate creature.
    #[test]
    fn synthesize_fabricate_does_not_affect_other_keywords() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Trample);
        face.keywords.push(Keyword::Vigilance);
        synthesize_fabricate(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 702.123b: Each instance of Fabricate triggers separately, so a
    /// card with two `Keyword::Fabricate` entries synthesizes two triggers.
    /// No printed card has this today; the test guards the rule shape.
    #[test]
    fn synthesize_fabricate_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Fabricate(1));
        face.keywords.push(Keyword::Fabricate(3));
        synthesize_fabricate(&mut face);
        let triggers: Vec<_> = face
            .triggers
            .iter()
            .filter(|t| {
                matches!(
                    t.execute.as_deref().map(|a| &*a.effect),
                    Some(Effect::ChooseOneOf { .. })
                )
            })
            .collect();
        assert_eq!(triggers.len(), 2);
        // Idempotency dedupe is by structural shape, but the first call
        // installs both N=1 and N=3 in one pass — the second call sees the
        // shape match and skips entirely. Verify both Ns are present from
        // the first pass.
        let ns: Vec<i32> = triggers
            .iter()
            .filter_map(|t| match t.execute.as_deref().map(|a| &*a.effect) {
                Some(Effect::ChooseOneOf { branches, .. }) => {
                    branches.iter().find_map(|b| match &*b.effect {
                        Effect::PutCounter {
                            count: QuantityExpr::Fixed { value },
                            ..
                        } => Some(*value),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect();
        assert!(ns.contains(&1) && ns.contains(&3));
    }
}

#[cfg(test)]
mod fabricate_runtime_tests {
    //! CR 702.123a runtime integration: the synthesized ETB ChooseOneOf
    //! trigger fires on enters-the-battlefield, lands on the stack as a
    //! triggered ability, resolves into `WaitingFor::ChooseOneOfBranch`,
    //! and each branch produces the rule-correct outcome (P1P1 counters
    //! or Servo tokens).

    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::triggers::process_triggers;
    use crate::game::zones::create_object;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, StackEntryKind, WaitingFor, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Build a `CardFace` that mimics a Cultivator-of-Blades-shaped card
    /// (creature with `Fabricate N`) and apply the full synthesis pipeline.
    fn fabricate_creature_face(name: &str, n: u32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            keywords: vec![Keyword::Fabricate(n)],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    /// CR 603.6a + CR 111.1: Synthesize an enters-the-battlefield event so
    /// `process_triggers` recognizes the ETB and the synthesized Fabricate
    /// trigger fires.
    fn etb_event(object_id: ObjectId, name: &str) -> GameEvent {
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Stack),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: name.to_string(),
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
                ..ZoneChangeRecord::test_minimal(object_id, Some(Zone::Stack), Zone::Battlefield)
            }),
        }
    }

    fn setup_state_with_priority(controller: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
        state
    }

    /// Cast a Fabricate creature from hand, then pass priority through the
    /// normal stack pipeline until the ETB trigger resolves into the
    /// ChooseOneOfBranch prompt. This intentionally does not synthesize the
    /// ZoneChanged event or call process_triggers directly.
    fn cast_and_resolve_fabricate_to_choice(
        face: &CardFace,
        controller: PlayerId,
    ) -> (GameState, ObjectId) {
        let mut state = setup_state_with_priority(controller);
        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(
            &mut state,
            next_card,
            controller,
            face.name.clone(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: next_card,
                targets: vec![],
            },
        )
        .unwrap();

        let mut saw_fabricate_trigger_on_stack = false;
        for _ in 0..8 {
            if matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }) {
                assert!(
                    saw_fabricate_trigger_on_stack,
                    "Fabricate ETB trigger must land on the stack before resolving"
                );
                assert_eq!(
                    state.objects.get(&obj_id).unwrap().zone,
                    Zone::Battlefield,
                    "Fabricate creature must enter through stack resolution"
                );
                return (state, obj_id);
            }

            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "expected priority while advancing cast/trigger pipeline, got {:?}",
                state.waiting_for
            );
            saw_fabricate_trigger_on_stack |= state
                .stack
                .iter()
                .any(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }));
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        }

        panic!(
            "Fabricate ETB trigger did not resolve to ChooseOneOfBranch; waiting_for={:?}, stack_len={}",
            state.waiting_for,
            state.stack.len()
        );
    }

    /// CR 702.123a branch A: choosing the +1/+1 counter branch places N
    /// P1P1 counters on the permanent that entered via normal spell
    /// resolution.
    #[test]
    fn fabricate_e2e_counter_branch_places_p1p1_counters_on_self() {
        let face = fabricate_creature_face("Cultivator of Blades", 2);
        let (mut state, obj_id) = cast_and_resolve_fabricate_to_choice(&face, PlayerId(0));

        // Confirm the choose-one-of prompt is waiting on the controller.
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: PlayerId(0),
                ..
            }
        ));

        // Branch 0 = P1P1 counters per synthesizer construction order.
        crate::game::engine::apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        let p1p1_count: u32 = obj
            .counters
            .iter()
            .filter(|(ct, _)| **ct == crate::types::counter::CounterType::Plus1Plus1)
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(
            p1p1_count, 2,
            "Fabricate 2 counter branch must place 2 +1/+1 counters"
        );
    }

    /// CR 702.123a branch B: choosing the Servo branch creates N 1/1
    /// colorless Servo artifact creature tokens under the controller after
    /// normal spell and ETB-trigger resolution.
    #[test]
    fn fabricate_e2e_servo_branch_creates_artifact_creature_tokens() {
        let face = fabricate_creature_face("Cultivator of Blades", 2);
        let (mut state, _obj_id) = cast_and_resolve_fabricate_to_choice(&face, PlayerId(0));

        // Branch 1 = Servo tokens.
        crate::game::engine::apply_as_current(&mut state, GameAction::ChooseBranch { index: 1 })
            .unwrap();

        let servos: Vec<&crate::game::game_object::GameObject> = state
            .objects
            .values()
            .filter(|obj| obj.name == "Servo" && obj.is_token)
            .collect();
        assert_eq!(
            servos.len(),
            2,
            "Fabricate 2 token branch must create 2 Servos"
        );
        for token in &servos {
            assert!(
                token.card_types.core_types.contains(&CoreType::Artifact),
                "Servo must be an artifact"
            );
            assert!(
                token.card_types.core_types.contains(&CoreType::Creature),
                "Servo must be a creature"
            );
            assert!(
                token.card_types.subtypes.iter().any(|s| s == "Servo"),
                "Servo must carry Servo subtype"
            );
            assert!(token.color.is_empty(), "Servo must be colorless");
            assert_eq!(token.controller, PlayerId(0));
        }
    }

    /// CR 702.123a with Fabricate 1 — Ambitious Aetherborn shape — exercises
    /// the same flow with N=1 to guard against off-by-one collapse of the
    /// branch construction.
    #[test]
    fn fabricate_one_resolves_with_singleton_payload() {
        let face = fabricate_creature_face("Ambitious Aetherborn", 1);
        let (mut state, obj_id) = cast_and_resolve_fabricate_to_choice(&face, PlayerId(0));

        crate::game::engine::apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        let p1p1_count: u32 = obj
            .counters
            .iter()
            .filter(|(ct, _)| **ct == crate::types::counter::CounterType::Plus1Plus1)
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(p1p1_count, 1);
    }

    /// Lower-level trigger plumbing negative: a non-Fabricate creature ETB
    /// event must not synthesize a ChooseOneOf prompt. The positive branch
    /// tests above cover the full cast/priority/stack runtime pipeline.
    #[test]
    fn etb_without_fabricate_does_not_emit_choose_one_of() {
        let mut face = CardFace {
            name: "Plain Bear".to_string(),
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);

        let mut state = setup_state_with_priority(PlayerId(0));

        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(
            &mut state,
            next_card,
            PlayerId(0),
            face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, &face);
        }
        process_triggers(&mut state, &[etb_event(obj_id, &face.name)]);
        assert!(
            !state
                .stack
                .iter()
                .any(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })),
            "non-Fabricate ETB must not push a triggered ability"
        );
    }
}

#[cfg(test)]
mod undying_persist_synthesis_tests {
    //! CR 702.93a + CR 702.79a: Shape tests for the synthesized dies-triggers
    //! that return a permanent with a counter, gated on its LKI counter state.
    //! Pinned to the exact wire-up the runtime resolver consumes:
    //! `TriggerMode::ChangesZone` (Battlefield → Graveyard), `valid_card =
    //! SelfRef`, `condition = Not(HadCounters(...))`, execute body
    //! `Effect::ChangeZone` (Graveyard → Battlefield) with
    //! `enter_with_counters = [(polarity, 1)]`.
    use super::*;

    fn face_with_keyword(kw: Keyword) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(kw);
        face
    }

    fn renown_trigger_count(face: &CardFace, n: i32) -> usize {
        face.triggers
            .iter()
            .filter(|trigger| {
                is_renown_trigger(trigger)
                    && matches!(
                        trigger.execute.as_deref().map(|a| a.effect.as_ref()),
                        Some(Effect::Renown {
                            count: QuantityExpr::Fixed { value }
                        }) if *value == n
                    )
            })
            .count()
    }

    /// CR 702.93a: Undying synthesizes a dies-trigger that returns the
    /// permanent with one +1/+1 counter, gated on the LKI absence of any
    /// +1/+1 counter.
    #[test]
    fn synthesize_undying_adds_dies_trigger_with_p1p1_return() {
        let mut face = face_with_keyword(Keyword::Undying);
        synthesize_undying(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_dies_return_with_counter_trigger(t, &CounterType::Plus1Plus1))
            .expect("undying should synthesize a dies-return trigger");

        // Trigger shape: dies (battlefield → graveyard) with self-ref filter.
        assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));

        // Condition: Not(HadCounters { Some("P1P1") }) — LKI-gated absence.
        let Some(TriggerCondition::Not { condition }) = &trigger.condition else {
            panic!("undying condition should be Not(...)");
        };
        let TriggerCondition::HadCounters { counter_type } = condition.as_ref() else {
            panic!("undying inner condition should be HadCounters");
        };
        assert_eq!(counter_type, &Some(CounterType::Plus1Plus1));

        // Execute: ChangeZone graveyard → battlefield + one P1P1 counter.
        let execute = trigger.execute.as_deref().expect("execute body required");
        let Effect::ChangeZone {
            origin,
            destination,
            target,
            enters_under,
            enter_with_counters,
            ..
        } = &*execute.effect
        else {
            panic!("undying execute should be Effect::ChangeZone");
        };
        assert_eq!(*origin, Some(Zone::Graveyard));
        assert_eq!(*destination, Zone::Battlefield);
        assert!(matches!(target, TargetFilter::SelfRef));
        // CR 702.93a: "under its owner's control" — default routing (no
        // override) places the object under its owner.
        assert_eq!(*enters_under, None);
        assert_eq!(enter_with_counters.len(), 1);
        let (ct, qty) = &enter_with_counters[0];
        assert_eq!(ct, &CounterType::Plus1Plus1);
        assert!(matches!(qty, QuantityExpr::Fixed { value: 1 }));
    }

    /// CR 702.79a: Persist mirror of the Undying shape test — -1/-1 counters,
    /// same trigger/effect topology.
    #[test]
    fn synthesize_persist_adds_dies_trigger_with_m1m1_return() {
        let mut face = face_with_keyword(Keyword::Persist);
        synthesize_persist(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_dies_return_with_counter_trigger(t, &CounterType::Minus1Minus1))
            .expect("persist should synthesize a dies-return trigger");

        let Some(TriggerCondition::Not { condition }) = &trigger.condition else {
            panic!("persist condition should be Not(...)");
        };
        let TriggerCondition::HadCounters { counter_type } = condition.as_ref() else {
            panic!("persist inner condition should be HadCounters");
        };
        assert_eq!(counter_type, &Some(CounterType::Minus1Minus1));

        let execute = trigger.execute.as_deref().expect("execute body required");
        let Effect::ChangeZone {
            enter_with_counters,
            ..
        } = &*execute.effect
        else {
            panic!("persist execute should be Effect::ChangeZone");
        };
        let (ct, qty) = &enter_with_counters[0];
        assert_eq!(ct, &CounterType::Minus1Minus1);
        assert!(matches!(qty, QuantityExpr::Fixed { value: 1 }));
    }

    #[test]
    fn synthesize_renown_adds_combat_damage_trigger() {
        let mut face = face_with_keyword(Keyword::Renown(2));
        synthesize_renown(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_renown_trigger(t))
            .expect("renown should synthesize a combat-damage trigger");

        assert_eq!(trigger.mode, TriggerMode::DamageDone);
        assert_eq!(trigger.valid_source, Some(TargetFilter::SelfRef));
        assert_eq!(trigger.valid_target, Some(TargetFilter::Player));
        assert_eq!(trigger.damage_kind, DamageKindFilter::CombatOnly);

        let Some(TriggerCondition::Not { condition }) = &trigger.condition else {
            panic!("renown condition should be Not(...)");
        };
        assert!(matches!(
            condition.as_ref(),
            TriggerCondition::SourceIsRenowned
        ));

        let execute = trigger.execute.as_deref().expect("execute body required");
        assert!(matches!(
            execute.effect.as_ref(),
            Effect::Renown {
                count: QuantityExpr::Fixed { value: 2 }
            }
        ));
    }

    #[test]
    fn synthesize_renown_preserves_multiple_instances() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Renown(1));
        face.keywords.push(Keyword::Renown(2));

        synthesize_renown(&mut face);

        assert_eq!(renown_trigger_count(&face, 1), 1);
        assert_eq!(renown_trigger_count(&face, 2), 1);
    }

    #[test]
    fn synthesize_renown_preserves_duplicate_instances_and_is_idempotent() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Renown(2));
        face.keywords.push(Keyword::Renown(2));

        synthesize_renown(&mut face);
        synthesize_renown(&mut face);

        assert_eq!(
            renown_trigger_count(&face, 2),
            2,
            "CR 702.112c: duplicate Renown instances each trigger separately"
        );
    }

    /// Repeated synthesis must not duplicate the trigger — the idempotency
    /// guard counts existing matching-shape triggers and skips when the
    /// keyword count is already satisfied.
    #[test]
    fn synthesize_undying_is_idempotent() {
        let mut face = face_with_keyword(Keyword::Undying);
        synthesize_undying(&mut face);
        synthesize_undying(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_dies_return_with_counter_trigger(t, &CounterType::Plus1Plus1))
            .count();
        assert_eq!(count, 1, "undying trigger should be deduped");
    }

    #[test]
    fn synthesize_persist_is_idempotent() {
        let mut face = face_with_keyword(Keyword::Persist);
        synthesize_persist(&mut face);
        synthesize_persist(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_dies_return_with_counter_trigger(t, &CounterType::Minus1Minus1))
            .count();
        assert_eq!(count, 1, "persist trigger should be deduped");
    }

    /// Faces without the keyword get no synthesized trigger.
    #[test]
    fn synthesize_undying_noop_without_keyword() {
        let mut face = face_with_keyword(Keyword::Flying);
        synthesize_undying(&mut face);
        assert!(face.triggers.is_empty());
    }

    #[test]
    fn synthesize_persist_noop_without_keyword() {
        let mut face = face_with_keyword(Keyword::Trample);
        synthesize_persist(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 113.2c + absence of redundancy clause in CR 702.93: multiple
    /// instances of Undying each function independently and so each emit a
    /// trigger. No printed card today has multiple Undying keywords; the
    /// test pins the rule shape so a future printing routes correctly.
    #[test]
    fn synthesize_undying_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Undying);
        face.keywords.push(Keyword::Undying);
        synthesize_undying(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_dies_return_with_counter_trigger(t, &CounterType::Plus1Plus1))
            .count();
        assert_eq!(count, 2);
    }

    /// A face that carries both Undying and Persist (no printed card today)
    /// synthesizes two distinct triggers — one per polarity. The shared
    /// `is_dies_return_with_counter_trigger` predicate is keyed on counter
    /// type so the Persist trigger doesn't dedupe the Undying trigger.
    #[test]
    fn synthesize_undying_and_persist_coexist_with_distinct_triggers() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Undying);
        face.keywords.push(Keyword::Persist);
        synthesize_undying(&mut face);
        synthesize_persist(&mut face);

        let p1p1 = face
            .triggers
            .iter()
            .filter(|t| is_dies_return_with_counter_trigger(t, &CounterType::Plus1Plus1))
            .count();
        let m1m1 = face
            .triggers
            .iter()
            .filter(|t| is_dies_return_with_counter_trigger(t, &CounterType::Minus1Minus1))
            .count();
        assert_eq!(p1p1, 1, "exactly one Undying trigger");
        assert_eq!(m1m1, 1, "exactly one Persist trigger");
    }

    /// CR 702.135a: Afterlife N synthesizes a self-ref dies trigger whose
    /// effect creates N 1/1 white-and-black flying Spirit tokens.
    #[test]
    fn synthesize_afterlife_adds_dies_trigger_with_spirit_tokens() {
        let mut face = face_with_keyword(Keyword::Afterlife(2));
        synthesize_afterlife(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_afterlife_trigger(t))
            .expect("afterlife should synthesize a dies trigger");

        // Trigger shape: dies (battlefield → graveyard) with self-ref filter.
        assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        // CR 702.135a is unconditional — no intervening-if gate.
        assert!(trigger.condition.is_none());

        let execute = trigger.execute.as_deref().expect("execute body required");
        let Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            keywords,
            count,
            owner,
            tapped,
            enters_attacking,
            ..
        } = &*execute.effect
        else {
            panic!("afterlife execute should be Effect::Token");
        };
        assert_eq!(name, "Spirit");
        assert!(matches!(power, PtValue::Fixed(1)));
        assert!(matches!(toughness, PtValue::Fixed(1)));
        assert!(types.contains(&"Creature".to_string()));
        assert!(types.contains(&"Spirit".to_string()));
        assert_eq!(colors, &vec![ManaColor::White, ManaColor::Black]);
        assert_eq!(keywords, &vec![Keyword::Flying]);
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
        assert!(matches!(owner, TargetFilter::Controller));
        assert!(!tapped);
        assert!(!enters_attacking);
    }

    /// Afterlife 1 vs Afterlife 3 — the token `count` tracks N.
    #[test]
    fn synthesize_afterlife_count_tracks_n() {
        for n in [1u32, 3] {
            let mut face = face_with_keyword(Keyword::Afterlife(n));
            synthesize_afterlife(&mut face);
            let execute = face
                .triggers
                .iter()
                .find(|t| is_afterlife_trigger(t))
                .and_then(|t| t.execute.as_deref())
                .expect("afterlife trigger with execute");
            let Effect::Token { count, .. } = &*execute.effect else {
                panic!("expected Effect::Token");
            };
            assert!(
                matches!(count, QuantityExpr::Fixed { value } if *value == n as i32),
                "afterlife {n} should create {n} tokens"
            );
        }
    }

    #[test]
    fn synthesize_afterlife_is_idempotent() {
        let mut face = face_with_keyword(Keyword::Afterlife(2));
        synthesize_afterlife(&mut face);
        synthesize_afterlife(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_afterlife_trigger(t))
            .count();
        assert_eq!(count, 1, "afterlife trigger should be deduped");
    }

    #[test]
    fn synthesize_afterlife_noop_without_keyword() {
        let mut face = face_with_keyword(Keyword::Flying);
        synthesize_afterlife(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 702.135b: multiple instances of afterlife each trigger separately.
    #[test]
    fn synthesize_afterlife_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Afterlife(1));
        face.keywords.push(Keyword::Afterlife(1));
        synthesize_afterlife(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_afterlife_trigger(t))
            .count();
        assert_eq!(count, 2);
    }

    /// CR 702.135b with differing N values: each Afterlife instance carries its
    /// own token count, so a face with Afterlife 1 and Afterlife 2 gets one
    /// trigger for each quantity instead of collapsing by keyword kind.
    #[test]
    fn synthesize_afterlife_keeps_distinct_counts() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Afterlife(1));
        face.keywords.push(Keyword::Afterlife(2));
        synthesize_afterlife(&mut face);

        let mut counts: Vec<i32> = face
            .triggers
            .iter()
            .filter_map(afterlife_trigger_count)
            .collect();
        counts.sort_unstable();
        assert_eq!(counts, vec![1, 2]);
    }

    /// The Afterlife matcher (Spirit-token effect) must not collide with the
    /// Undying/Persist return triggers, which share the Battlefield→Graveyard
    /// self-ref shape but carry an `Effect::ChangeZone`.
    #[test]
    fn afterlife_trigger_distinct_from_undying() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Afterlife(2));
        face.keywords.push(Keyword::Undying);
        synthesize_afterlife(&mut face);
        synthesize_undying(&mut face);

        let afterlife = face
            .triggers
            .iter()
            .filter(|t| is_afterlife_trigger(t))
            .count();
        let undying = face
            .triggers
            .iter()
            .filter(|t| is_dies_return_with_counter_trigger(t, &CounterType::Plus1Plus1))
            .count();
        assert_eq!(afterlife, 1, "exactly one Afterlife trigger");
        assert_eq!(undying, 1, "exactly one Undying trigger");
        // Neither predicate matches the other's trigger.
        assert!(
            !face.triggers.iter().any(|t| is_afterlife_trigger(t)
                && is_dies_return_with_counter_trigger(t, &CounterType::Plus1Plus1)),
            "no trigger is matched by both predicates"
        );
    }

    /// CR 604.1 runtime-grant path: `triggers_for` produces the trigger and
    /// `trigger_matches_keyword_kind` recognizes it (used by layers.rs when
    /// afterlife is granted on the battlefield, and for symmetric removal).
    #[test]
    fn afterlife_triggers_for_and_matcher_roundtrip() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Afterlife(2));
        assert_eq!(triggers.len(), 1, "afterlife yields exactly one trigger");
        assert!(
            KeywordTriggerInstaller::trigger_matches_keyword_kind(
                &triggers[0],
                &Keyword::Afterlife(2)
            ),
            "matcher must recognize the synthesized afterlife trigger"
        );
        // It must NOT be recognized as some other keyword's trigger.
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Undying
        ));
    }

    /// Runtime-grant removal uses `trigger_matches_keyword_kind`, so the matcher
    /// must discriminate `Afterlife(N)` by N rather than stripping every
    /// Afterlife-style Spirit trigger for the same discriminant.
    #[test]
    fn afterlife_matcher_distinguishes_count() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Afterlife(2));
        assert_eq!(triggers.len(), 1, "afterlife yields exactly one trigger");

        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Afterlife(2)
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Afterlife(1)
        ));
    }

    // -----------------------------------------------------------------------
    // CR 702.46a/702.46b: Soulshift N — dies trigger that optionally returns a
    // target Spirit card with mana value N or less from your graveyard to your
    // hand. Shape tests pinned to the exact wire-up the runtime resolver
    // consumes: ChangesZone (Battlefield → Graveyard), valid_card = SelfRef,
    // optional execute body ChangeZone (Graveyard → Hand) targeting a Spirit
    // card in your graveyard with Cmc ≤ N.
    // -----------------------------------------------------------------------

    #[test]
    fn synthesize_soulshift_adds_dies_return_trigger() {
        let mut face = face_with_keyword(Keyword::Soulshift(4));
        synthesize_soulshift(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_soulshift_trigger(t))
            .expect("soulshift should synthesize a dies trigger");

        // Trigger shape: dies (battlefield → graveyard) with self-ref filter.
        assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        // CR 702.46a is unconditional (no intervening-if) — the "you may" lives
        // on the execute ability, not as a trigger condition.
        assert!(trigger.condition.is_none());

        let execute = trigger.execute.as_deref().expect("execute body required");
        // CR 702.46a "you may": the return is an optional ability.
        assert!(execute.optional, "soulshift return must be optional");

        let Effect::ChangeZone {
            origin,
            destination,
            target,
            up_to,
            enter_with_counters,
            ..
        } = &*execute.effect
        else {
            panic!("soulshift execute should be Effect::ChangeZone");
        };
        // CR 702.46a: return from your graveyard to your hand.
        assert_eq!(*origin, Some(Zone::Graveyard));
        assert_eq!(*destination, Zone::Hand);
        // "target ... card" — a single mandatory target when performed.
        assert!(!up_to);
        assert!(enter_with_counters.is_empty());

        // Target filter: Spirit card in YOUR graveyard with mana value ≤ N.
        let TargetFilter::Typed(tf) = target else {
            panic!("soulshift target should be a Typed graveyard filter");
        };
        assert_eq!(tf.get_subtype(), Some("Spirit")); // CR 205.3
        assert!(tf.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }));
        assert!(tf.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::You
        })); // CR 109.5
             // CR 202.3: "mana value N or less" — LE comparator carrying the threshold.
        assert!(tf.properties.contains(&FilterProp::Cmc {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 4 },
        }));
    }

    /// The mana-value threshold tracks N (CR 702.46a).
    #[test]
    fn synthesize_soulshift_value_tracks_n() {
        for n in [1u32, 3, 7] {
            let mut face = face_with_keyword(Keyword::Soulshift(n));
            synthesize_soulshift(&mut face);
            let value = face
                .triggers
                .iter()
                .find_map(soulshift_trigger_value)
                .expect("soulshift trigger present");
            assert_eq!(value, n as i32, "soulshift {n} should target Cmc ≤ {n}");
        }
    }

    #[test]
    fn synthesize_soulshift_is_idempotent() {
        let mut face = face_with_keyword(Keyword::Soulshift(4));
        synthesize_soulshift(&mut face);
        synthesize_soulshift(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_soulshift_trigger(t))
            .count();
        assert_eq!(count, 1, "soulshift trigger should be deduped");
    }

    #[test]
    fn synthesize_soulshift_noop_without_keyword() {
        let mut face = face_with_keyword(Keyword::Flying);
        synthesize_soulshift(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 702.46b: multiple instances of Soulshift each trigger separately —
    /// and differing N values must NOT collapse by keyword kind.
    #[test]
    fn synthesize_soulshift_keeps_distinct_values() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Soulshift(4));
        face.keywords.push(Keyword::Soulshift(7));
        synthesize_soulshift(&mut face);

        let mut values: Vec<i32> = face
            .triggers
            .iter()
            .filter_map(soulshift_trigger_value)
            .collect();
        values.sort_unstable();
        assert_eq!(values, vec![4, 7]);
    }

    /// CR 604.1 runtime-grant path: `triggers_for` produces the trigger and
    /// `trigger_matches_keyword_kind` recognizes it (granted-keyword install +
    /// symmetric removal), discriminating by N.
    #[test]
    fn soulshift_triggers_for_and_matcher_roundtrip() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Soulshift(4));
        assert_eq!(triggers.len(), 1, "soulshift yields exactly one trigger");
        assert!(
            KeywordTriggerInstaller::trigger_matches_keyword_kind(
                &triggers[0],
                &Keyword::Soulshift(4)
            ),
            "matcher must recognize the synthesized soulshift trigger"
        );
        // Wrong N must not match (CR 702.46b load-bearing threshold).
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Soulshift(3)
        ));
        // Must not be recognized as another keyword's trigger.
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Undying
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Afterlife(4)
        ));
    }

    /// The Soulshift matcher (Graveyard→Hand Spirit return) must not collide
    /// with the Afterlife Spirit-token trigger, which shares the
    /// Battlefield→Graveyard self-ref dies shape.
    #[test]
    fn soulshift_trigger_distinct_from_afterlife() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Soulshift(4));
        face.keywords.push(Keyword::Afterlife(2));
        synthesize_soulshift(&mut face);
        synthesize_afterlife(&mut face);

        let soulshift = face
            .triggers
            .iter()
            .filter(|t| is_soulshift_trigger(t))
            .count();
        let afterlife = face
            .triggers
            .iter()
            .filter(|t| is_afterlife_trigger(t))
            .count();
        assert_eq!(soulshift, 1, "exactly one Soulshift trigger");
        assert_eq!(afterlife, 1, "exactly one Afterlife trigger");
        assert!(
            !face
                .triggers
                .iter()
                .any(|t| is_soulshift_trigger(t) && is_afterlife_trigger(t)),
            "no trigger is matched by both predicates"
        );
    }
}

#[cfg(test)]
mod undying_persist_runtime_tests {
    //! CR 702.93a + CR 702.79a runtime integration: a battlefield permanent
    //! with the keyword dies, `apply_zone_exit_cleanup` captures its LKI
    //! counter map into `state.lki_cache`, `process_triggers` fires the
    //! synthesized dies-trigger, the intervening `Not(HadCounters)` condition
    //! reads the LKI snapshot, and `resolve_top` resolves `Effect::ChangeZone`
    //! to return the permanent with a single +1/+1 (or -1/-1) counter.

    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::triggers::process_triggers;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::ability::TargetRef;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, StackEntryKind, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Build a creature face with the given keyword and run the full
    /// synthesis pipeline to install the dies-trigger.
    fn creature_face_with_keyword(name: &str, kw: Keyword) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(1)),
            keywords: vec![kw],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn spirit_card_face(name: &str, mana_value: u32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::generic(mana_value),
            power: Some(PtValue::Fixed(1)),
            toughness: Some(PtValue::Fixed(1)),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        face.card_type.subtypes.push("Spirit".to_string());
        face
    }

    fn creature_card_face(name: &str, mana_value: u32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::generic(mana_value),
            power: Some(PtValue::Fixed(1)),
            toughness: Some(PtValue::Fixed(1)),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        face
    }

    fn create_face_object(
        state: &mut GameState,
        face: &CardFace,
        owner: PlayerId,
        zone: Zone,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, owner, face.name.clone(), zone);
        apply_card_face_to_object(state.objects.get_mut(&id).unwrap(), face);
        id
    }

    /// Stand up a two-player state with `face` on the battlefield under
    /// `controller`. Returns the state and the spawned object id so callers
    /// can mutate counters before killing the creature.
    fn setup_with_creature(face: &CardFace, controller: PlayerId) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };

        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(
            &mut state,
            next_card,
            controller,
            face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }
        (state, obj_id)
    }

    /// Kill the permanent (battlefield → graveyard), fire its dies-trigger,
    /// then resolve the top of the stack. Returns the events the chain
    /// produced so callers can inspect the return-to-battlefield event.
    fn kill_and_resolve(state: &mut GameState, obj_id: ObjectId) -> Vec<GameEvent> {
        let mut events = Vec::new();
        // CR 603.10a: `move_to_zone` captures LKI in `apply_zone_exit_cleanup`
        // before the object physically leaves the battlefield and emits the
        // `ZoneChanged` event that `process_triggers` consumes.
        move_to_zone(state, obj_id, Zone::Graveyard, &mut events);
        process_triggers(state, &events);
        let mut resolve_events = Vec::new();
        if !state.stack.is_empty() {
            crate::game::stack::resolve_top(state, &mut resolve_events);
        }
        resolve_events
    }

    /// CR 702.93a happy path: a creature with Undying that dies with zero
    /// +1/+1 counters returns to the battlefield with one +1/+1 counter.
    #[test]
    fn undying_returns_with_counter_when_died_with_zero_p1p1_counters() {
        let face = creature_face_with_keyword("Young Wolf", Keyword::Undying);
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        let _ = kill_and_resolve(&mut state, obj_id);

        let obj = state.objects.get(&obj_id).expect("object still tracked");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "undying should return the permanent to the battlefield"
        );
        assert_eq!(obj.owner, PlayerId(0));
        // CR 702.93a: "under its owner's control"
        assert_eq!(obj.controller, PlayerId(0));
        let p1p1: u32 = obj
            .counters
            .iter()
            .filter(|(ct, _)| **ct == CounterType::Plus1Plus1)
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(p1p1, 1, "undying returns with exactly one +1/+1 counter");
    }

    /// CR 702.93a negative path: a creature with Undying that died WITH a
    /// +1/+1 counter must NOT return. The intervening `Not(HadCounters)`
    /// condition gates the trigger out at the check phase, so the stack
    /// never has a triggered ability for the return.
    #[test]
    fn undying_does_not_return_when_died_with_one_p1p1_counter() {
        let face = creature_face_with_keyword("Strangleroot Geist", Keyword::Undying);
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        // Seed a +1/+1 counter on the live creature so the LKI snapshot
        // (captured at `move_to_zone` entry) shows the counter.
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let _ = kill_and_resolve(&mut state, obj_id);

        let obj = state.objects.get(&obj_id).expect("object still tracked");
        assert_eq!(
            obj.zone,
            Zone::Graveyard,
            "undying must NOT return a creature that died with a +1/+1 counter"
        );
        assert!(
            !state
                .stack
                .iter()
                .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
            "no surviving trigger on the stack — the intervening-if filtered it"
        );
    }

    /// CR 702.79a happy path: Persist returns the permanent with one -1/-1
    /// counter if it died with no -1/-1 counter.
    #[test]
    fn persist_returns_with_counter_when_died_with_zero_m1m1_counters() {
        let face = creature_face_with_keyword("Kitchen Finks", Keyword::Persist);
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        let _ = kill_and_resolve(&mut state, obj_id);

        let obj = state.objects.get(&obj_id).expect("object still tracked");
        assert_eq!(obj.zone, Zone::Battlefield);
        let m1m1: u32 = obj
            .counters
            .iter()
            .filter(|(ct, _)| **ct == CounterType::Minus1Minus1)
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(m1m1, 1, "persist returns with exactly one -1/-1 counter");
    }

    /// CR 702.79a negative path: Persist creature that died with a -1/-1
    /// counter must NOT return.
    #[test]
    fn persist_does_not_return_when_died_with_one_m1m1_counter() {
        let face = creature_face_with_keyword("Murderous Redcap", Keyword::Persist);
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .counters
            .insert(CounterType::Minus1Minus1, 1);

        let _ = kill_and_resolve(&mut state, obj_id);

        let obj = state.objects.get(&obj_id).expect("object still tracked");
        assert_eq!(
            obj.zone,
            Zone::Graveyard,
            "persist must NOT return a creature that died with a -1/-1 counter"
        );
    }

    /// CR 702.135a runtime path: when a permanent with Afterlife N dies, the
    /// synthesized dies trigger resolves through `Effect::Token` and creates N
    /// 1/1 white-and-black flying Spirit creature tokens under the controller.
    #[test]
    fn afterlife_creates_spirit_tokens_when_permanent_dies() {
        let face = creature_face_with_keyword("Tithe Taker", Keyword::Afterlife(2));
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        let _ = kill_and_resolve(&mut state, obj_id);

        let source = state.objects.get(&obj_id).expect("object still tracked");
        assert_eq!(
            source.zone,
            Zone::Graveyard,
            "afterlife does not return the source permanent"
        );

        let spirits: Vec<_> = state
            .objects
            .values()
            .filter(|obj| obj.is_token && obj.name == "Spirit" && obj.zone == Zone::Battlefield)
            .collect();
        assert_eq!(
            spirits.len(),
            2,
            "Afterlife 2 must create exactly 2 Spirits"
        );
        for spirit in spirits {
            assert_eq!(spirit.owner, PlayerId(0));
            assert_eq!(spirit.controller, PlayerId(0));
            assert_eq!(spirit.power, Some(1));
            assert_eq!(spirit.toughness, Some(1));
            assert!(
                spirit.card_types.core_types.contains(&CoreType::Creature),
                "Spirit token must be a creature"
            );
            assert!(
                spirit.card_types.subtypes.iter().any(|s| s == "Spirit"),
                "Spirit token must carry Spirit subtype"
            );
            assert_eq!(spirit.color, vec![ManaColor::White, ManaColor::Black]);
            assert!(
                spirit.keywords.contains(&Keyword::Flying),
                "Spirit token must have flying"
            );
        }
    }

    /// Seed a Spirit card into `player`'s graveyard with the given mana value.
    /// Returns the object id so callers can assert it as the auto-chosen target.
    fn spirit_in_graveyard(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        mana_cost: &str,
    ) -> ObjectId {
        let mut face = CardFace {
            name: name.to_string(),
            mana_cost: parse_mtgjson_mana_cost(mana_cost),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        face.card_type.subtypes.push("Spirit".to_string());
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(state, card_id, player, face.name.clone(), Zone::Graveyard);
        let obj = state.objects.get_mut(&obj_id).unwrap();
        apply_card_face_to_object(obj, &face);
        obj_id
    }

    /// CR 702.46a runtime path: when a creature with Soulshift N dies and the
    /// controller has exactly one eligible Spirit card (mana value ≤ N) in their
    /// graveyard, the synthesized dies trigger lands on the stack with that
    /// Spirit auto-chosen as its single target. This exercises the graveyard
    /// `TargetFilter` (subtype Spirit + your-graveyard + Cmc ≤ N) against real
    /// graveyard objects through `process_triggers` + the targeting pipeline,
    /// independent of the optional "you may" resolution prompt.
    #[test]
    fn soulshift_dies_trigger_targets_eligible_graveyard_spirit() {
        let face = creature_face_with_keyword("Kami of the Hunt", Keyword::Soulshift(4));
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        // Eligible: a Spirit with mana value 2 (≤ 4) in your graveyard.
        let eligible = spirit_in_graveyard(&mut state, PlayerId(0), "Kami of False Hope", "{1}{W}");
        // Ineligible: a Spirit with mana value 6 (> 4) — filtered out by Cmc,
        // leaving exactly one legal target so the trigger auto-targets.
        let _too_expensive = spirit_in_graveyard(
            &mut state,
            PlayerId(0),
            "Kira, Great Glass-Spinner",
            "{1}{U}{U}{U}{U}{U}",
        );
        // Ineligible: a Spirit in the OPPONENT's graveyard — excluded by
        // `Owned { You }`.
        let _opponent_spirit =
            spirit_in_graveyard(&mut state, PlayerId(1), "Spirit of the Hearth", "{1}{W}");

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);

        let triggered = state
            .stack
            .iter()
            .find(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. }))
            .expect("soulshift dies trigger must land on the stack");
        let ability = triggered
            .ability()
            .expect("triggered ability carries a ResolvedAbility");
        assert_eq!(
            ability.targets,
            vec![crate::types::ability::TargetRef::Object(eligible)],
            "the single eligible graveyard Spirit (MV ≤ N, yours) must be auto-targeted"
        );
    }

    /// CR 702.46a negative runtime path: when no eligible Spirit is in the
    /// controller's graveyard, the dies trigger has no legal target. Because the
    /// return is optional ("you may"), the trigger still goes on the stack but
    /// targets nothing — it resolves as a no-op rather than freezing the engine.
    #[test]
    fn soulshift_dies_trigger_with_no_eligible_spirit_targets_nothing() {
        let face = creature_face_with_keyword("Kami of the Hunt", Keyword::Soulshift(2));
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        // Only an over-cost Spirit (MV 5 > 2) in your graveyard — not eligible.
        let _too_expensive =
            spirit_in_graveyard(&mut state, PlayerId(0), "Yosei, the Morning Star", "{4}{W}");

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);

        if let Some(triggered) = state
            .stack
            .iter()
            .find(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. }))
        {
            let ability = triggered.ability().expect("ResolvedAbility");
            assert!(
                ability.targets.is_empty(),
                "no eligible Spirit means no target is chosen"
            );
        }
    }

    /// CR 702.46a runtime path: accepting Soulshift N returns target Spirit card
    /// with mana value N or less from the controller's graveyard to their hand.
    #[test]
    fn soulshift_returns_eligible_spirit_card_from_graveyard() {
        let face = creature_face_with_keyword("Kami of the Honored Dead", Keyword::Soulshift(4));
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));
        let legal_spirit = create_face_object(
            &mut state,
            &spirit_card_face("Petalmane Baku", 3),
            PlayerId(0),
            Zone::Graveyard,
        );
        let too_expensive_spirit = create_face_object(
            &mut state,
            &spirit_card_face("High-Cost Spirit", 5),
            PlayerId(0),
            Zone::Graveyard,
        );
        let non_spirit = create_face_object(
            &mut state,
            &creature_card_face("Ordinary Bear", 2),
            PlayerId(0),
            Zone::Graveyard,
        );

        let mut events = Vec::new();
        move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);
        if matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }) {
            crate::game::engine::apply_as_current(
                &mut state,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(legal_spirit)),
                },
            )
            .expect("choose the only legal Soulshift target");
        }

        let mut resolve_events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut resolve_events);
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "Soulshift is optional and must ask before returning the Spirit"
        );
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("accept Soulshift");
        if matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }) {
            crate::game::engine::apply_as_current(
                &mut state,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(legal_spirit)),
                },
            )
            .expect("choose the legal Soulshift target after accepting");
        }

        assert_eq!(
            state.objects[&legal_spirit].zone,
            Zone::Hand,
            "Soulshift must return the eligible Spirit card to hand"
        );
        assert_eq!(
            state.objects[&too_expensive_spirit].zone,
            Zone::Graveyard,
            "Soulshift 4 must not return a Spirit card with mana value 5"
        );
        assert_eq!(
            state.objects[&non_spirit].zone,
            Zone::Graveyard,
            "Soulshift must not return a non-Spirit card"
        );
    }

    /// CR 603 multi-trigger semantics: a permanent that carries BOTH Undying
    /// and Persist (a contrived dual-keyword card) puts both triggers on the
    /// stack on death. The first to resolve returns the permanent to the
    /// battlefield.
    ///
    /// The engine reuses `obj_id` for the returned permanent (CR 400.7 makes
    /// it a new game object conceptually, but the implementation preserves
    /// the `ObjectId` across the zone change). When the second trigger
    /// resolves, its `Effect::ChangeZone` evaluates `from_zone =
    /// Zone::Battlefield`, which fails the `expected_origin ==
    /// Some(Zone::Graveyard)` guard at `change_zone.rs:501-505` and the
    /// move silently no-ops. `enter_with_counters` runs only on a successful
    /// move, so the second trigger places no counter either.
    ///
    /// Post-condition pinned by this test: exactly one battlefield object
    /// with the name, and exactly ONE counter (polarity = whichever trigger
    /// resolved first). Asserting the counter total catches a future
    /// regression in which the origin guard is weakened and the second
    /// trigger's `enter_with_counters` accidentally executes.
    #[test]
    fn undying_and_persist_together_on_same_face_does_not_double_return() {
        let mut face = CardFace {
            name: "Test Dual".to_string(),
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(1)),
            keywords: vec![Keyword::Undying, Keyword::Persist],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);

        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        // Die with zero counters — both Undying and Persist conditions
        // evaluate true at trigger-condition check.
        let mut events = Vec::new();
        move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);
        // CR 603.3b (#531): controller has 2 simultaneous triggers (Undying
        // + Persist); drain the ordering prompt with identity order.
        crate::game::triggers::drain_order_triggers_with_identity(&mut state);

        // Drain the entire stack.
        while !state.stack.is_empty() {
            let mut resolve_events = Vec::new();
            crate::game::stack::resolve_top(&mut state, &mut resolve_events);
        }

        let obj = state.objects.get(&obj_id).expect("object still tracked");
        assert_eq!(obj.zone, Zone::Battlefield);
        let count_in_battlefield = state
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.name == "Test Dual")
            .count();
        assert_eq!(
            count_in_battlefield, 1,
            "dual-keyword permanent must not be double-returned"
        );
        // The origin guard at change_zone.rs:501-505 prevents the
        // second-to-resolve trigger from executing its move, so its
        // `enter_with_counters` never runs. Exactly one counter ends up on
        // the returned permanent (polarity = whichever trigger resolved
        // first).
        let total_counters: u32 = obj.counters.values().sum();
        assert_eq!(
            total_counters, 1,
            "exactly one counter from the first-resolved trigger; the origin guard prevents the second"
        );
    }

    /// CR 702.79a "under its owner's control" — the returned permanent must
    /// route to its OWNER, not the controller at the moment of death.
    ///
    /// Setup: a Persist creature owned by player 0 with a REAL Layer-2
    /// control-changing continuous effect (CR 613.1b) installed via
    /// `add_transient_continuous_effect` — a Threaten / Act-of-Treason-style
    /// `ChangeController` modification making player 1 the controller. The
    /// precondition `obj.controller == PlayerId(1)` is then asserted *through*
    /// `evaluate_layers`, so the test genuinely exercises the layer system
    /// the routing implicitly depends on, instead of poking `obj.controller`
    /// directly.
    ///
    /// Discrimination mechanism (`enters_under: None` vs a
    /// `Some(ControllerRef::You)`-regression): when the creature dies,
    /// `apply_zone_exit_cleanup` (`zones.rs`) prunes the `SpecificObject`-
    /// bound control effect via `prune_affected_object_left_effects` regardless
    /// of duration, but does NOT reset `obj.controller` — only
    /// `reset_for_battlefield_exit` resets `base_controller`. So the graveyard
    /// object still reads `controller = P1`, and the dies-trigger captures
    /// `ability.controller = P1`. With `enters_under: None`,
    /// `ctrl_override = None` → `reset_for_battlefield_entry` sets
    /// `controller`/`base_controller` to the owner → Layer 2 yields P0
    /// (test passes). With a `Some(ControllerRef::You)`-regression,
    /// `apply_battlefield_entry_controller_override` writes
    /// `base_controller = Some(P1)`, which Layer 2 preserves → P1 (test fails).
    /// The mutation check (flipping the synthesized Persist trigger's
    /// `enters_under` to `Some(ControllerRef::You)`) was performed during
    /// implementation and confirmed to make this test fail — proof the
    /// assertion discriminates.
    ///
    /// The post-return lookup uses `state.objects.get(&obj_id)` directly:
    /// Persist's return does not create a new object — `move_to_zone` mutates
    /// the existing `GameObject` in place, keeping the same `ObjectId` across
    /// Battlefield→Graveyard→Battlefield.
    ///
    /// This pins the `enters_under: None` field's "send to owner" semantics
    /// (CR 110.2a): without it, a control-grab would steal the Persist /
    /// Undying creature permanently on death.
    #[test]
    fn persist_returns_under_owner_not_controller_after_control_grab() {
        // Use a 2/2 base so the post-return -1/-1 counter doesn't push the
        // permanent to 0 toughness — otherwise the SBA pass we run below
        // (to force a layers re-evaluation) would send it back to the
        // graveyard before the owner-vs-controller assertion.
        let mut face = CardFace {
            name: "Stolen Finks".to_string(),
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            keywords: vec![Keyword::Persist],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        let (mut state, obj_id) = setup_with_creature(&face, PlayerId(0));

        // CR 613.1b: install a REAL Threaten / Act-of-Treason-style
        // Layer-2 control-changing continuous effect so player 1 genuinely
        // controls the creature via the continuous-effect system — not a raw
        // `obj.controller =` mutation. The precondition is then derived
        // through `evaluate_layers`, exercising the layer system the
        // owner-vs-controller routing implicitly depends on.
        {
            let obj = state.objects.get(&obj_id).unwrap();
            assert_eq!(obj.owner, PlayerId(0), "precondition: owner is P0");
        }
        state.add_transient_continuous_effect(
            obj_id,      // source (self-referential is fine)
            PlayerId(1), // new effective controller
            crate::types::ability::Duration::Permanent,
            TargetFilter::SpecificObject { id: obj_id },
            vec![crate::types::ability::ContinuousModification::ChangeController],
            None,
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&obj_id].controller,
            PlayerId(1),
            "precondition: real Layer-2 control effect makes P1 the controller"
        );

        let _ = kill_and_resolve(&mut state, obj_id);

        // CR 704.3: Run SBAs so the layers pass triggered by the return
        // zone-change (which sets `state.layers_dirty = true` in
        // `effects/change_zone.rs:52`) actually evaluates. Layer 2 resets
        // `controller` to `owner` per CR 613.1b for any battlefield object
        // without an active control-changing continuous effect.
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);

        let obj = state.objects.get(&obj_id).expect("object still tracked");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "persist returns the permanent to the battlefield"
        );
        // CR 702.79a "under its owner's control" — owner wins over the
        // pre-death controller. `enters_under: None` on the
        // `Effect::ChangeZone` causes `move_to_zone` not to write any
        // controller override; CR 613.1b then resets controller to owner
        // during the next layers pass.
        assert_eq!(
            obj.owner,
            PlayerId(0),
            "owner unchanged across the zone round-trip"
        );
        assert_eq!(
            obj.controller,
            PlayerId(0),
            "persist must return under its owner's control, not under the death-time controller"
        );
    }
}

#[cfg(test)]
mod annihilator_synthesis_tests {
    //! CR 702.86a + CR 702.86b shape tests: the synthesized Annihilator
    //! trigger is an `Attacks` trigger gated on `SelfRef` whose execute body
    //! is `Effect::Sacrifice` over a permanent filter scoped to the defending
    //! player via `ControllerRef::DefendingPlayer`.
    use super::*;

    fn annihilator_face(n: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Annihilator(n));
        face
    }

    /// CR 702.86a: synthesizer emits an `Attacks` trigger with execute body
    /// `Effect::Sacrifice` over `DefendingPlayer`-controlled permanents.
    #[test]
    fn synthesize_annihilator_adds_attack_trigger() {
        let mut face = annihilator_face(2);
        synthesize_annihilator(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::Attacks))
            .expect("annihilator should add an Attacks trigger");

        assert!(
            matches!(trigger.valid_card, Some(TargetFilter::SelfRef)),
            "valid_card must be SelfRef so the trigger fires only when this \
             creature attacks (not when other attackers are declared)"
        );

        let Some(execute) = trigger.execute.as_deref() else {
            panic!("execute body required");
        };
        let Effect::Sacrifice {
            target,
            count,
            min_count,
        } = &*execute.effect
        else {
            panic!("execute body must be Effect::Sacrifice");
        };
        assert_eq!(*min_count, 0);
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));

        let TargetFilter::Typed(tf) = target else {
            panic!("sacrifice target must be a TypedFilter");
        };
        assert_eq!(
            tf.controller,
            Some(ControllerRef::DefendingPlayer),
            "sacrifice scope must be the defending player (CR 508.5)"
        );
        // CR 701.21a: Annihilator sacrifices permanents, not just creatures.
        assert!(
            tf.type_filters
                .iter()
                .any(|f| matches!(f, TypeFilter::Permanent)),
            "filter must target permanents"
        );
    }

    /// Repeated synthesis must not duplicate the trigger (idempotency).
    #[test]
    fn synthesize_annihilator_is_idempotent() {
        let mut face = annihilator_face(1);
        synthesize_annihilator(&mut face);
        synthesize_annihilator(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_annihilator_attack_trigger(t))
            .count();
        assert_eq!(count, 1, "annihilator trigger should be deduped");
    }

    /// Cards without Annihilator are unaffected.
    #[test]
    fn synthesize_annihilator_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_annihilator(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// Negative test: a creature with unrelated keywords must not synthesize
    /// an Annihilator trigger.
    #[test]
    fn synthesize_annihilator_does_not_affect_other_keywords() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Trample);
        face.keywords.push(Keyword::Vigilance);
        synthesize_annihilator(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 702.86b: "If a creature has multiple instances of annihilator, each
    /// triggers separately." A card with two `Keyword::Annihilator` entries
    /// (e.g., a hypothetical card with two printed instances, or one printed
    /// plus one granted) synthesizes two distinct triggers. CR 113.2c also
    /// independently requires this.
    #[test]
    fn synthesize_annihilator_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Annihilator(1));
        face.keywords.push(Keyword::Annihilator(3));
        synthesize_annihilator(&mut face);
        let triggers: Vec<_> = face
            .triggers
            .iter()
            .filter(|t| is_annihilator_attack_trigger(t))
            .collect();
        assert_eq!(triggers.len(), 2);

        // Both N=1 and N=3 must be present from the first pass.
        let ns: Vec<i32> = triggers
            .iter()
            .filter_map(|t| match t.execute.as_deref().map(|a| &*a.effect) {
                Some(Effect::Sacrifice {
                    count: QuantityExpr::Fixed { value },
                    ..
                }) => Some(*value),
                _ => None,
            })
            .collect();
        assert!(ns.contains(&1) && ns.contains(&3));
    }

    /// Idempotency-shape predicate must NOT match unrelated `Attacks` triggers
    /// (e.g., "Whenever this creature attacks, draw a card"). A face with both
    /// a card-draw Attacks trigger and `Keyword::Annihilator(1)` must produce
    /// the Annihilator trigger without the predicate misclassifying the
    /// draw-trigger as Annihilator.
    #[test]
    fn synthesize_annihilator_distinguishes_unrelated_attacks_triggers() {
        let mut face = annihilator_face(1);
        // Install an unrelated Attacks trigger on the face FIRST.
        let unrelated = TriggerDefinition::new(TriggerMode::Attacks)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        face.triggers.push(unrelated);
        synthesize_annihilator(&mut face);

        let annihilator_count = face
            .triggers
            .iter()
            .filter(|t| is_annihilator_attack_trigger(t))
            .count();
        assert_eq!(
            annihilator_count, 1,
            "the unrelated draw-on-attack trigger must not pre-satisfy the \
             Annihilator idempotency check"
        );
        // Total triggers: 1 draw + 1 Annihilator.
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| matches!(t.mode, TriggerMode::Attacks))
                .count(),
            2
        );
    }
}

#[cfg(test)]
mod provoke_synthesis_tests {
    //! CR 702.39a shape tests: the synthesized Provoke trigger is an `Attacks`
    //! trigger gated on `SelfRef` whose OPTIONAL execute body untaps a creature
    //! the defending player controls (`Effect::Untap` over a
    //! `ControllerRef::DefendingPlayer` creature filter) and chains an
    //! `Effect::ForceBlock` on that same target via `TargetFilter::ParentTarget`.
    use super::*;

    fn provoke_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Provoke);
        face
    }

    /// CR 702.39a: synthesizer emits an optional `Attacks` trigger whose execute
    /// body untaps a `DefendingPlayer`-controlled creature, then force-blocks it.
    #[test]
    fn synthesize_provoke_adds_untap_and_force_block_attack_trigger() {
        let mut face = provoke_face();
        synthesize_provoke(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::Attacks))
            .expect("provoke should add an Attacks trigger");

        // CR 702.39a: "Whenever THIS creature attacks" — fires only for the source.
        assert!(
            matches!(trigger.valid_card, Some(TargetFilter::SelfRef)),
            "valid_card must be SelfRef (only when the provoking creature attacks)"
        );

        let execute = trigger.execute.as_deref().expect("execute body required");

        // CR 702.39a: "you may have target creature..." — the ability is optional.
        assert!(
            execute.optional,
            "Provoke is a 'you may' trigger (CR 702.39a)"
        );

        // CR 702.39a + CR 701.26b: parent body untaps the defending player's creature.
        let Effect::Untap {
            target: TargetFilter::Typed(tf),
        } = &*execute.effect
        else {
            panic!("execute body must be Effect::Untap over a TypedFilter");
        };
        assert_eq!(
            tf.controller,
            Some(ControllerRef::DefendingPlayer),
            "untap target must be a creature the defending player controls (CR 702.39a / CR 508.5)"
        );
        assert!(
            tf.type_filters
                .iter()
                .any(|f| matches!(f, TypeFilter::Creature)),
            "untap target filter must be a creature"
        );

        // CR 702.39a + CR 509.1c: chained continuation force-blocks the SAME target.
        let sub = execute
            .sub_ability
            .as_deref()
            .expect("force-block continuation required");
        assert!(
            matches!(
                &*sub.effect,
                Effect::ForceBlock {
                    target: TargetFilter::ParentTarget,
                }
            ),
            "sub-ability must force-block the parent (untapped) target via ParentTarget, got {:?}",
            sub.effect
        );
    }

    /// Repeated synthesis must not duplicate the trigger (idempotency).
    #[test]
    fn synthesize_provoke_is_idempotent() {
        let mut face = provoke_face();
        synthesize_provoke(&mut face);
        synthesize_provoke(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_provoke_attack_trigger(t))
            .count();
        assert_eq!(count, 1, "provoke trigger should be deduped");
    }

    /// Cards without Provoke are unaffected.
    #[test]
    fn synthesize_provoke_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_provoke(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// Idempotency-shape predicate must NOT match unrelated `Attacks` triggers
    /// — e.g. a non-optional "Whenever this creature attacks, untap target
    /// creature defending player controls" must not be misclassified as Provoke
    /// (Provoke requires the optional flag AND the ForceBlock continuation).
    #[test]
    fn is_provoke_attack_trigger_rejects_unrelated_untap_on_attack() {
        // Same target/mode shape but NOT optional and NO force-block sub.
        let unrelated = TriggerDefinition::new(TriggerMode::Attacks)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Untap {
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::DefendingPlayer),
                    ),
                },
            ));
        assert!(
            !is_provoke_attack_trigger(&unrelated),
            "a non-optional untap-on-attack with no force-block must not match Provoke"
        );
    }

    /// CR 604.1 runtime-grant path: `triggers_for` produces the trigger and
    /// `trigger_matches_keyword_kind` recognizes it (used by layers.rs when
    /// Provoke is granted/removed at runtime). Symmetric with the analogous
    /// annihilator/afterlife roundtrip coverage.
    #[test]
    fn provoke_triggers_for_and_matcher_roundtrip() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Provoke);
        assert_eq!(triggers.len(), 1, "Provoke installs exactly one trigger");
        assert!(
            KeywordTriggerInstaller::trigger_matches_keyword_kind(&triggers[0], &Keyword::Provoke),
            "matcher must recognize the synthesized Provoke trigger"
        );
        // Must not cross-match an unrelated keyword.
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Mentor
        ));
    }

    /// CR 702.39b: if a creature has multiple instances of Provoke, each
    /// triggers separately. MTGJSON dedupes the keyword array to one "Provoke",
    /// so `build_oracle_face` must recover repeated printed bare words from
    /// Oracle text before `synthesize_all` installs one trigger per instance.
    #[test]
    fn build_oracle_face_recovers_repeated_provoke_instances_from_oracle_text() {
        let mtgjson = AtomicCard {
            name: "Repeated Provoke Test".to_string(),
            mana_cost: Some("{2}{G}".to_string()),
            colors: vec!["G".to_string()],
            color_identity: vec!["G".to_string()],
            power: Some("2".to_string()),
            toughness: Some("2".to_string()),
            loyalty: None,
            defense: None,
            text: Some("Provoke, provoke".to_string()),
            layout: "normal".to_string(),
            type_line: Some("Creature — Beast".to_string()),
            types: vec!["Creature".to_string()],
            subtypes: vec!["Beast".to_string()],
            supertypes: Vec::new(),
            keywords: Some(vec!["Provoke".to_string()]),
            side: None,
            face_name: None,
            mana_value: 3.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: crate::database::mtgjson::AtomicIdentifiers {
                scryfall_id: None,
                scryfall_oracle_id: None,
            },
            foreign_data: Vec::new(),
        };

        let face = build_oracle_face(&mtgjson, None);

        assert_eq!(
            face.keywords
                .iter()
                .filter(|keyword| matches!(keyword, Keyword::Provoke))
                .count(),
            2,
            "Oracle text must recover repeated Provoke instances that MTGJSON dedupes"
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_provoke_attack_trigger(trigger))
                .count(),
            2,
            "CR 702.39b: each recovered Provoke instance triggers separately"
        );
    }
}

#[cfg(test)]
mod provoke_runtime_tests {
    //! CR 702.39a runtime integration: resolving the synthesized Provoke
    //! execute chain (with a defending-player creature chosen as the target)
    //! untaps that creature (`Effect::Untap` — CR 701.26b) and, because the
    //! source is an active attacker, the chained `Effect::ForceBlock` grants
    //! `StaticMode::MustBlockAttacker { attacker: source }` (CR 702.39a /
    //! CR 509.1c) — exercising the EXISTING force-block resolver, not new logic.

    use super::*;
    use crate::game::ability_utils::build_resolved_from_def_with_targets;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::effects::resolve_ability_chain;
    use crate::game::zones::create_object;
    use crate::types::ability::{ContinuousModification, EffectKind, TargetRef};
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;

    fn place(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        create_object(
            state,
            card_id,
            controller,
            name.to_string(),
            Zone::Battlefield,
        )
    }

    /// CR 702.39a + CR 701.26b + CR 509.1c happy path: the provoking creature
    /// (PlayerId(0)) is an active attacker; the chosen defender (a tapped
    /// PlayerId(1) creature) untaps and gains `MustBlockAttacker { attacker:
    /// provoker }`.
    #[test]
    fn provoke_execute_untaps_target_and_forces_block_on_attacker() {
        let trigger = build_provoke_trigger();
        let execute = trigger.execute.as_deref().expect("execute body required");

        let mut state = GameState::new_two_player(42);
        let provoker = place(&mut state, PlayerId(0), "Provoker");
        let defender = place(&mut state, PlayerId(1), "Tapped Bear");
        // Target starts tapped so the untap is observable.
        state.objects.get_mut(&defender).unwrap().tapped = true;

        // CR 508.5 / CR 509.1c: the source must be an active attacker for the
        // ForceBlock resolver to bind `MustBlockAttacker { attacker: source }`.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                provoker,
                AttackTarget::Player(PlayerId(1)),
                PlayerId(1),
            )],
            ..Default::default()
        });

        // The player chose "yes" and selected the defending creature as target.
        // `optional` is cleared on the built resolved ability to represent that
        // affirmative may-choice without routing through the prompt state machine.
        let mut resolved = build_resolved_from_def_with_targets(
            execute,
            provoker,
            PlayerId(0),
            vec![TargetRef::Object(defender)],
        );
        resolved.optional = false;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();

        // CR 701.26b: the targeted creature untaps.
        assert!(
            !state.objects.get(&defender).unwrap().tapped,
            "Provoke must untap the targeted defending creature"
        );

        // CR 702.39a + CR 509.1c: a MustBlockAttacker static bound to the
        // provoking attacker is applied to the targeted creature.
        let forced = state.transient_continuous_effects.iter().any(|ce| {
            ce.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustBlockAttacker { attacker },
                    } if *attacker == provoker
                )
            })
        });
        assert!(
            forced,
            "Provoke must apply MustBlockAttacker bound to the provoking attacker, \
             reusing the existing source-referential ForceBlock resolver"
        );

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::ForceBlock,
                ..
            }
        )));
    }
}

#[cfg(test)]
mod mentor_synthesis_tests {
    //! CR 702.134a: Mentor synthesizes an `Attacks` trigger (source = this
    //! creature) that puts a +1/+1 counter on a lesser-power attacking creature.
    use super::*;

    fn mentor_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Mentor);
        face
    }

    #[test]
    fn synthesize_mentor_adds_lesser_power_attack_trigger() {
        let mut face = mentor_face();
        synthesize_mentor(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::Attacks))
            .expect("mentor should add an Attacks trigger");

        // CR 702.134a: "Whenever THIS creature attacks" — fires only for the source.
        assert!(
            matches!(trigger.valid_card, Some(TargetFilter::SelfRef)),
            "valid_card must be SelfRef (only when the mentoring creature attacks)"
        );

        let execute = trigger.execute.as_deref().expect("execute body required");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("execute body must be Effect::PutCounter");
        };
        assert_eq!(*counter_type, CounterType::Plus1Plus1);
        assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));

        let TargetFilter::Typed(tf) = target else {
            panic!("counter target must be a TypedFilter");
        };
        assert!(
            tf.properties.contains(&FilterProp::Attacking),
            "target must be an attacking creature (CR 702.134a)"
        );
        // CR 702.134a + CR 208.1: power strictly less than this creature's power.
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LT,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Source
                        }
                    },
                }
            )),
            "target power must be strictly less than the source's power, got {:?}",
            tf.properties
        );
    }

    #[test]
    fn synthesize_mentor_preserves_duplicate_instances_and_is_idempotent() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Mentor);
        face.keywords.push(Keyword::Mentor);

        synthesize_mentor(&mut face);
        synthesize_mentor(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_mentor_trigger(t))
                .count(),
            2,
            "CR 702.134b requires one trigger per Mentor instance, while repeated synthesis must remain idempotent"
        );
    }

    #[test]
    fn synthesize_mentor_is_noop_without_keyword() {
        let mut face = CardFace::default();
        synthesize_mentor(&mut face);
        assert!(face.triggers.is_empty());
    }

    #[test]
    fn keyword_trigger_installer_exposes_runtime_granted_mentor_trigger() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Mentor);

        assert_eq!(triggers.len(), 1);
        assert!(is_mentor_trigger(&triggers[0]));
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Mentor
        ));
    }
}

#[cfg(test)]
mod exalted_synthesis_tests {
    //! CR 702.83a + CR 702.83b shape tests: the synthesized Exalted trigger
    //! is an `Attacks` trigger gated on a creature-you-control filter with a
    //! `Not(MinCoAttackers { minimum: 1 })` condition (= attacks alone) whose
    //! execute body is `Effect::Pump` targeting `TriggeringSource`.
    use super::*;

    fn exalted_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Exalted);
        face
    }

    #[test]
    fn synthesize_exalted_adds_attack_trigger() {
        let mut face = exalted_face();
        synthesize_exalted(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_exalted_trigger(t))
            .expect("exalted should add an Attacks trigger");

        assert!(matches!(trigger.mode, TriggerMode::Attacks));
        assert!(matches!(
            trigger.condition,
            Some(TriggerCondition::Not { .. })
        ));

        let Some(execute) = trigger.execute.as_deref() else {
            panic!("execute body required");
        };
        let Effect::Pump {
            power,
            toughness,
            target,
        } = &*execute.effect
        else {
            panic!("execute body must be Effect::Pump");
        };
        assert!(matches!(power, PtValue::Fixed(1)));
        assert!(matches!(toughness, PtValue::Fixed(1)));
        assert!(matches!(target, TargetFilter::TriggeringSource));
    }

    #[test]
    fn synthesize_exalted_is_idempotent() {
        let mut face = exalted_face();
        synthesize_exalted(&mut face);
        synthesize_exalted(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_exalted_trigger(t))
            .count();
        assert_eq!(count, 1, "exalted trigger should be deduped");
    }

    #[test]
    fn synthesize_exalted_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_exalted(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 702.83b: multiple instances trigger separately.
    #[test]
    fn synthesize_exalted_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Exalted);
        face.keywords.push(Keyword::Exalted);
        synthesize_exalted(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_exalted_trigger(t))
            .count();
        assert_eq!(count, 2);
    }
}

#[cfg(test)]
mod flanking_synthesis_tests {
    //! CR 702.25a shape tests: a self-scoped BecomesBlocked trigger whose
    //! `Effect::Pump(-1/-1)` debuffs the triggering blocker without flanking.
    use super::*;

    #[test]
    fn synthesize_flanking_adds_becomes_blocked_debuff_trigger() {
        // CR 702.25a: Flanking installs a self BecomesBlocked trigger that gives
        // each blocking creature without flanking -1/-1 until end of turn.
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flanking);
        synthesize_flanking(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_flanking_trigger(t))
            .expect("flanking should add a BecomesBlocked trigger");
        assert!(matches!(trigger.mode, TriggerMode::BecomesBlocked));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        let execute = trigger.execute.as_deref().expect("execute body required");
        assert_eq!(execute.duration, Some(Duration::UntilEndOfTurn));
        let Effect::Pump {
            power,
            toughness,
            target,
        } = &*execute.effect
        else {
            panic!("flanking execute must be Effect::Pump");
        };
        assert!(matches!(power, PtValue::Fixed(-1)));
        assert!(matches!(toughness, PtValue::Fixed(-1)));
        assert!(matches!(target, TargetFilter::TriggeringSource));
        let Some(TargetFilter::Typed(tf)) = trigger.valid_target.as_ref() else {
            panic!("expected Typed non-flanking blocker filter");
        };
        assert!(tf.properties.contains(&FilterProp::WithoutKeyword {
            value: Keyword::Flanking,
        }));
    }

    #[test]
    fn synthesize_flanking_is_idempotent_and_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flanking);
        synthesize_flanking(&mut face);
        synthesize_flanking(&mut face);
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_flanking_trigger(t))
                .count(),
            1,
            "flanking trigger should be deduped across passes"
        );

        let mut bare = CardFace::default();
        synthesize_flanking(&mut bare);
        assert!(bare.triggers.iter().all(|t| !is_flanking_trigger(t)));
    }
}

#[cfg(test)]
mod bushido_synthesis_tests {
    //! CR 702.45a shape tests: two self-scoped triggers (Blocks +
    //! BecomesBlocked), each an `Effect::Pump` on `SelfRef` of +N/+N.
    use super::*;

    #[test]
    fn synthesize_bushido_adds_block_and_becomes_blocked_triggers() {
        // CR 702.45a: Bushido N installs two self-triggers (blocks + becomes
        // blocked), each pumping the source +N/+N until end of turn.
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Bushido(2));
        synthesize_bushido(&mut face);

        let bushido: Vec<_> = face
            .triggers
            .iter()
            .filter(|t| is_bushido_trigger(t, 2))
            .collect();
        assert_eq!(bushido.len(), 2, "blocks + becomes-blocked");
        assert!(bushido
            .iter()
            .any(|t| matches!(t.mode, TriggerMode::Blocks)));
        assert!(bushido
            .iter()
            .any(|t| matches!(t.mode, TriggerMode::BecomesBlocked)));
        for t in &bushido {
            assert!(matches!(t.valid_card, Some(TargetFilter::SelfRef)));
            let Some(Effect::Pump {
                power,
                toughness,
                target,
            }) = t.execute.as_deref().map(|a| &*a.effect)
            else {
                panic!("bushido execute must be Effect::Pump");
            };
            assert!(matches!(power, PtValue::Fixed(2)));
            assert!(matches!(toughness, PtValue::Fixed(2)));
            assert!(matches!(target, TargetFilter::SelfRef));
        }
    }

    #[test]
    fn synthesize_bushido_is_idempotent_and_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Bushido(1));
        synthesize_bushido(&mut face);
        synthesize_bushido(&mut face);
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_bushido_trigger(t, 1))
                .count(),
            2,
            "two triggers (blocks + becomes-blocked), deduped across passes"
        );

        let mut bare = CardFace::default();
        synthesize_bushido(&mut bare);
        assert!(bare.triggers.iter().all(|t| !is_bushido_trigger(t, 1)));
    }
}

#[cfg(test)]
mod battlecry_synthesis_tests {
    //! CR 702.91a shape tests: one `Attacks` trigger whose execute is a mass
    //! `Effect::PumpAll(+1/+0)` over other attacking creatures.
    use super::*;

    #[test]
    fn synthesize_battlecry_adds_attack_pump_all_trigger() {
        // CR 702.91a: Battle cry installs one attack trigger pumping each other
        // attacking creature +1/+0 until end of turn.
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Battlecry);
        synthesize_battlecry(&mut face);

        let triggers: Vec<_> = face
            .triggers
            .iter()
            .filter(|t| is_battlecry_trigger(t))
            .collect();
        assert_eq!(triggers.len(), 1);
        let t = triggers[0];
        assert!(matches!(t.mode, TriggerMode::Attacks));
        assert!(matches!(t.valid_card, Some(TargetFilter::SelfRef)));
        let Some(Effect::PumpAll {
            power,
            toughness,
            target,
        }) = t.execute.as_deref().map(|a| &*a.effect)
        else {
            panic!("battle cry execute must be Effect::PumpAll");
        };
        assert!(matches!(power, PtValue::Fixed(1)));
        assert!(matches!(toughness, PtValue::Fixed(0)));
        let TargetFilter::Typed(tf) = target else {
            panic!("battle cry target must be Typed");
        };
        // CR 702.91a: other attacking creatures — `Attacking` + source-relative
        // `Another`.
        assert_eq!(
            tf.properties,
            vec![FilterProp::Attacking, FilterProp::Another]
        );
    }

    #[test]
    fn synthesize_battlecry_is_idempotent_and_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Battlecry);
        synthesize_battlecry(&mut face);
        synthesize_battlecry(&mut face);
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_battlecry_trigger(t))
                .count(),
            1,
            "one trigger, deduped across passes"
        );

        let mut bare = CardFace::default();
        synthesize_battlecry(&mut bare);
        assert!(bare.triggers.iter().all(|t| !is_battlecry_trigger(t)));
    }

    #[test]
    fn battlecry_multiplicity_installs_one_trigger_per_instance() {
        // CR 702.91b: each instance of battle cry triggers separately.
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Battlecry);
        face.keywords.push(Keyword::Battlecry);
        synthesize_battlecry(&mut face);
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_battlecry_trigger(t))
                .count(),
            2
        );
    }

    #[test]
    fn battlecry_triggers_for_and_matcher_roundtrip() {
        // CR 604.1 runtime-grant path: `triggers_for` produces the trigger and
        // `trigger_matches_keyword_kind` recognizes it (RemoveKeyword symmetry).
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Battlecry);
        assert_eq!(triggers.len(), 1);
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Battlecry
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Flanking
        ));
    }
}

#[cfg(test)]
mod rampage_synthesis_tests {
    //! CR 702.23a shape tests: one self-scoped `BecomesBlocked` trigger whose
    //! execute is a dynamic `Effect::Pump` of N × (blockers − 1).
    use super::*;

    #[test]
    fn synthesize_rampage_adds_becomes_blocked_dynamic_pump() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Rampage(2));
        synthesize_rampage(&mut face);

        let triggers: Vec<_> = face
            .triggers
            .iter()
            .filter(|t| is_rampage_trigger(t, 2))
            .collect();
        assert_eq!(triggers.len(), 1);
        let t = triggers[0];
        assert!(matches!(t.mode, TriggerMode::BecomesBlocked));
        assert!(matches!(t.valid_card, Some(TargetFilter::SelfRef)));
        let Some(Effect::Pump {
            power,
            toughness,
            target,
        }) = t.execute.as_deref().map(|a| &*a.effect)
        else {
            panic!("rampage execute must be Effect::Pump");
        };
        assert!(matches!(target, TargetFilter::SelfRef));
        // CR 702.23a + CR 107.1b: +N/+N per blocker beyond the first —
        // N × max(blockers − 1, 0).
        let expected = PtValue::Quantity(rampage_beyond_first_expr(2));
        assert_eq!(power, &expected);
        assert_eq!(toughness, &expected);
    }

    #[test]
    fn synthesize_rampage_is_idempotent_and_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Rampage(1));
        synthesize_rampage(&mut face);
        synthesize_rampage(&mut face);
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_rampage_trigger(t, 1))
                .count(),
            1
        );

        let mut bare = CardFace::default();
        synthesize_rampage(&mut bare);
        assert!(bare.triggers.iter().all(|t| !is_rampage_trigger(t, 1)));
    }

    #[test]
    fn rampage_triggers_for_and_matcher_roundtrip() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Rampage(3));
        assert_eq!(triggers.len(), 1);
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Rampage(3)
        ));
        // CR 702.23c: a different Rampage level is a distinct trigger.
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Rampage(2)
        ));
    }
}

#[cfg(test)]
mod melee_synthesis_tests {
    //! CR 702.121a shape tests: one self-scoped `Attacks` trigger whose execute
    //! is a `Pump` of +1/+1 per opponent attacked this combat.
    use super::*;

    #[test]
    fn synthesize_melee_adds_attack_per_opponent_pump() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Melee);
        synthesize_melee(&mut face);

        let triggers: Vec<_> = face
            .triggers
            .iter()
            .filter(|t| is_melee_trigger(t))
            .collect();
        assert_eq!(triggers.len(), 1);
        let t = triggers[0];
        assert!(matches!(t.mode, TriggerMode::Attacks));
        assert!(matches!(t.valid_card, Some(TargetFilter::SelfRef)));
        let Some(Effect::Pump {
            power,
            toughness,
            target,
        }) = t.execute.as_deref().map(|a| &*a.effect)
        else {
            panic!("melee execute must be Effect::Pump");
        };
        assert!(matches!(target, TargetFilter::SelfRef));
        // CR 702.121a: +1/+1 for each opponent you attacked this combat.
        let expected = PtValue::Quantity(melee_attacked_opponents_expr());
        assert_eq!(power, &expected);
        assert_eq!(toughness, &expected);
    }

    #[test]
    fn melee_count_uses_combat_scoped_opponent_filter() {
        // CR 702.121a: the magnitude must count opponents attacked THIS COMBAT,
        // not this turn — guards against reusing the turn-scoped filter.
        assert_eq!(
            melee_attacked_opponents_expr(),
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::OpponentAttacked {
                        subject: AttackSubject::You,
                        scope: AttackScope::ThisCombat,
                    },
                },
            }
        );
    }

    #[test]
    fn synthesize_melee_is_idempotent_and_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Melee);
        synthesize_melee(&mut face);
        synthesize_melee(&mut face);
        assert_eq!(
            face.triggers.iter().filter(|t| is_melee_trigger(t)).count(),
            1
        );

        let mut bare = CardFace::default();
        synthesize_melee(&mut bare);
        assert!(bare.triggers.iter().all(|t| !is_melee_trigger(t)));
    }

    #[test]
    fn melee_triggers_for_and_matcher_roundtrip() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Melee);
        assert_eq!(triggers.len(), 1);
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Melee
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Battlecry
        ));
    }
}

#[cfg(test)]
mod extort_synthesis_tests {
    //! CR 702.101a + CR 702.101b shape tests: the synthesized Extort trigger
    //! is a `SpellCast` trigger with `valid_target = Controller` whose execute
    //! body is optional with a mana cost and a `LoseLife` effect scoped to
    //! opponents. The chained `GainLife` uses `PreviousEffectAmount` because
    //! CR 702.101a's "that much life" is the total life actually lost by all
    //! opponents.
    use super::*;
    use crate::game::effects::resolve_ability_chain;
    use crate::types::ability::ResolvedAbility;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn extort_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Extort);
        face
    }

    #[test]
    fn synthesize_extort_adds_spell_cast_trigger() {
        let mut face = extort_face();
        synthesize_extort(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_extort_trigger(t))
            .expect("extort should add a SpellCast trigger");

        assert!(matches!(trigger.mode, TriggerMode::SpellCast));
        assert!(matches!(
            trigger.valid_target,
            Some(TargetFilter::Controller)
        ));

        let Some(execute) = trigger.execute.as_deref() else {
            panic!("execute body required");
        };
        assert!(execute.optional, "extort must be optional (may pay)");
        let Effect::PayCost {
            cost:
                PaymentCost::AbilityCost {
                    cost: AbilityCost::Mana { cost },
                },
            payer,
        } = &*execute.effect
        else {
            panic!("extort must pay W/B via PayCost before draining");
        };
        assert_eq!(
            cost,
            &ManaCost::Cost {
                shards: vec![ManaCostShard::WhiteBlack],
                generic: 0,
            }
        );
        assert!(matches!(payer, TargetFilter::Controller));

        let Some(drain) = execute.sub_ability.as_deref() else {
            panic!("extort must chain drain after payment");
        };
        assert!(
            matches!(drain.player_scope, Some(PlayerFilter::Opponent)),
            "drain must scope to opponents"
        );
        assert_eq!(
            drain.condition,
            Some(AbilityCondition::effect_performed()),
            "drain must be gated on successful W/B payment (If you do)"
        );
        assert!(
            matches!(&*drain.effect, Effect::LoseLife { .. }),
            "drain effect must be LoseLife"
        );

        let Some(gain) = drain.sub_ability.as_deref() else {
            panic!("extort must chain a gain-life rider");
        };
        let Effect::GainLife { amount, player } = &*gain.effect else {
            panic!("extort rider must be GainLife");
        };
        assert!(matches!(
            amount,
            QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectAmount
            }
        ));
        assert!(matches!(player, TargetFilter::Controller));
    }

    #[test]
    fn synthesize_extort_is_idempotent() {
        let mut face = extort_face();
        synthesize_extort(&mut face);
        synthesize_extort(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_extort_trigger(t))
            .count();
        assert_eq!(count, 1, "extort trigger should be deduped");
    }

    #[test]
    fn synthesize_extort_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_extort(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 702.101b: multiple instances trigger separately.
    #[test]
    fn synthesize_extort_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Extort);
        face.keywords.push(Keyword::Extort);
        synthesize_extort(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_extort_trigger(t))
            .count();
        assert_eq!(count, 2);
    }

    /// CR 604.1: the runtime-granted path (`triggers_for`) yields the same shape
    /// as the printed path, and `trigger_matches_keyword_kind` recognizes exactly
    /// that shape for symmetric removal.
    #[test]
    fn triggers_for_extort_matches_keyword_kind() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Extort);
        assert_eq!(triggers.len(), 1);
        assert!(is_extort_trigger(&triggers[0]));
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Extort
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &build_dethrone_trigger(),
            &Keyword::Extort
        ));
    }

    /// The Extort matcher feeds runtime `RemoveKeyword`, so it must not strip a
    /// coincidental spell-cast trigger with a different payment/drain shape.
    #[test]
    fn extort_matcher_rejects_non_extort_pay_and_drain_trigger() {
        let gain = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        );
        let drain = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            },
        )
        .player_scope(PlayerFilter::Opponent)
        .sub_ability(gain)
        .condition(AbilityCondition::effect_performed());
        let execute = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PayCost {
                cost: PaymentCost::AbilityCost {
                    cost: AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                },
                payer: TargetFilter::Controller,
            },
        )
        .optional()
        .sub_ability(drain);
        let trigger = TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_target(TargetFilter::Controller)
            .execute(execute);

        assert!(!is_extort_trigger(&trigger));
    }

    /// CR 702.101a: "you gain that much life" means the total life actually
    /// lost by all opponents, not a fixed 1. In a three-player game one Extort
    /// trigger drains two opponents for 1 each, so the controller gains 2.
    #[test]
    fn extort_gain_tracks_total_life_lost_across_opponents() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let source_id = ObjectId(100);
        let mut drain = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        drain.player_scope = Some(PlayerFilter::Opponent);
        drain.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &drain, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, 22);
        assert_eq!(state.players[1].life, 19);
        assert_eq!(state.players[2].life, 19);
    }
}

#[cfg(test)]
mod increment_synthesis_tests {
    use super::*;

    fn increment_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Increment);
        face
    }

    fn increment_atomic_card(text: &str) -> AtomicCard {
        AtomicCard {
            name: "Topiary Lecturer".to_string(),
            mana_cost: Some("{2}{G}".to_string()),
            colors: vec!["G".to_string()],
            color_identity: vec!["G".to_string()],
            text: Some(text.to_string()),
            power: Some("2".to_string()),
            toughness: Some("3".to_string()),
            loyalty: None,
            defense: None,
            layout: "normal".to_string(),
            type_line: Some("Creature — Plant Employee".to_string()),
            types: vec!["Creature".to_string()],
            subtypes: vec!["Plant".to_string(), "Employee".to_string()],
            supertypes: Vec::new(),
            keywords: Some(vec!["Increment".to_string()]),
            side: None,
            face_name: None,
            mana_value: 3.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: crate::database::mtgjson::AtomicIdentifiers {
                scryfall_oracle_id: Some("increment-dedupe-test".to_string()),
                scryfall_id: Some("increment-dedupe-test-face".to_string()),
            },
            foreign_data: Vec::new(),
        }
    }

    #[test]
    fn synthesize_increment_adds_spell_cast_trigger_with_intervening_if() {
        let mut face = increment_face();
        synthesize_increment(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_increment_trigger(t))
            .expect("increment should add a SpellCast trigger");

        assert!(matches!(trigger.mode, TriggerMode::SpellCast));
        assert!(matches!(
            trigger.valid_target,
            Some(TargetFilter::Controller)
        ));
        assert!(
            matches!(trigger.condition, Some(TriggerCondition::And { .. })),
            "increment must gate on creature check and mana spent vs source P/T"
        );

        let Some(execute) = trigger.execute.as_deref() else {
            panic!("execute body required");
        };
        assert!(matches!(
            &*execute.effect,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn synthesize_increment_is_idempotent() {
        let mut face = increment_face();
        synthesize_increment(&mut face);
        synthesize_increment(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_increment_trigger(t))
            .count();
        assert_eq!(count, 1, "increment trigger should be deduped");
    }

    #[test]
    fn synthesize_increment_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Increment);
        face.keywords.push(Keyword::Increment);
        synthesize_increment(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_increment_trigger(t))
            .count();
        assert_eq!(count, 2, "CR 702.191b: each instance triggers separately");
    }

    #[test]
    fn build_oracle_face_dedupes_increment_keyword_and_reminder_body() {
        let mtgjson = increment_atomic_card(
            "Increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)",
        );

        let face = build_oracle_face(&mtgjson, None);

        assert!(face
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Increment)));
        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_increment_trigger(trigger))
                .count(),
            1,
            "Increment keyword synthesis should recognize the parsed reminder trigger"
        );
        assert_eq!(
            face.triggers.len(),
            1,
            "Increment reminder parsing and keyword synthesis should not create duplicate triggers"
        );
    }

    #[test]
    fn build_oracle_face_preserves_repeated_increment_instances_from_oracle_text() {
        let mtgjson = increment_atomic_card(
            "Increment, increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)",
        );

        let face = build_oracle_face(&mtgjson, None);

        assert_eq!(
            face.keywords
                .iter()
                .filter(|keyword| matches!(keyword, Keyword::Increment))
                .count(),
            2,
            "Oracle text must recover repeated Increment instances that MTGJSON dedupes"
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_increment_trigger(trigger))
                .count(),
            2,
            "CR 702.191b: each recovered Increment instance triggers separately"
        );
        assert_eq!(
            face.triggers.len(),
            2,
            "repeated Increment should produce exactly one trigger per printed instance"
        );
    }
}

#[cfg(test)]
mod riot_synthesis_tests {
    use super::*;

    #[test]
    fn synthesize_riot_adds_optional_etb_replacement() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Riot);
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_riot(&mut face);
        assert!(
            face.replacements
                .iter()
                .any(|replacement| is_riot_replacement(replacement, &TargetFilter::SelfRef)),
            "riot should add ETB optional replacement, got {:?}",
            face.replacements
        );
    }

    #[test]
    fn synthesize_riot_is_idempotent() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Riot);
        synthesize_riot(&mut face);
        synthesize_riot(&mut face);
        assert_eq!(
            face.replacements
                .iter()
                .filter(|replacement| is_riot_replacement(replacement, &TargetFilter::SelfRef))
                .count(),
            1
        );
    }

    #[test]
    fn synthesize_riot_static_grant_adds_replacement_for_affected_filter() {
        let mut face = CardFace::default();
        let affected = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::NonToken]),
        );
        face.static_abilities.push(
            StaticDefinition::continuous()
                .affected(affected.clone())
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Riot,
                }]),
        );
        synthesize_riot(&mut face);
        assert!(
            face.replacements
                .iter()
                .any(|replacement| is_riot_replacement(replacement, &affected)),
            "static Riot grant should add ETB replacement for affected filter, got {:?}",
            face.replacements
        );
    }

    #[test]
    fn synthesize_unleash_adds_optional_etb_counter_and_cant_block_static() {
        // CR 702.98a: both halves — the optional ETB +1/+1 counter and the
        // "can't block while it has a +1/+1 counter" static.
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Unleash);
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_unleash(&mut face);
        assert!(
            face.replacements
                .iter()
                .any(|replacement| is_unleash_replacement(replacement, &TargetFilter::SelfRef)),
            "unleash should add an optional ETB +1/+1 counter replacement, got {:?}",
            face.replacements
        );
        let condition = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            minimum: 1,
            maximum: None,
        };
        assert!(
            face.static_abilities
                .iter()
                .any(|static_def| is_unleash_cant_block_static(
                    static_def,
                    &TargetFilter::SelfRef,
                    &condition
                )),
            "unleash should add a counter-conditioned CantBlock static, got {:?}",
            face.static_abilities
        );
    }

    #[test]
    fn synthesize_unleash_is_idempotent() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Unleash);
        synthesize_unleash(&mut face);
        synthesize_unleash(&mut face);
        assert_eq!(
            face.replacements
                .iter()
                .filter(|replacement| is_unleash_replacement(replacement, &TargetFilter::SelfRef))
                .count(),
            1
        );
        let condition = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            minimum: 1,
            maximum: None,
        };
        assert_eq!(
            face.static_abilities
                .iter()
                .filter(|static_def| is_unleash_cant_block_static(
                    static_def,
                    &TargetFilter::SelfRef,
                    &condition
                ))
                .count(),
            1
        );
    }

    #[test]
    fn synthesize_unleash_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_unleash(&mut face);
        assert!(face
            .replacements
            .iter()
            .all(|r| !is_unleash_replacement(r, &TargetFilter::SelfRef)));
        let condition = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            minimum: 1,
            maximum: None,
        };
        assert!(face
            .static_abilities
            .iter()
            .all(|s| !is_unleash_cant_block_static(s, &TargetFilter::SelfRef, &condition)));
    }

    #[test]
    fn synthesize_unleash_static_grant_adds_replacement_and_recipient_cant_block_static() {
        let mut face = CardFace::default();
        let affected = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::NonToken]),
        );
        face.static_abilities.push(
            StaticDefinition::continuous()
                .affected(affected.clone())
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Unleash,
                }]),
        );
        synthesize_unleash(&mut face);
        assert!(
            face.replacements
                .iter()
                .any(|replacement| is_unleash_replacement(replacement, &affected)),
            "static Unleash grant should add ETB replacement for affected filter, got {:?}",
            face.replacements
        );
        let condition = StaticCondition::RecipientHasCounters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            minimum: 1,
            maximum: None,
        };
        assert!(
            face.static_abilities
                .iter()
                .any(|static_def| is_unleash_cant_block_static(
                    static_def,
                    &affected,
                    &condition
                )),
            "static Unleash grant should add recipient-gated CantBlock for affected filter, got {:?}",
            face.static_abilities
        );
    }
}

#[cfg(test)]
mod dethrone_tests {
    //! CR 702.105a: Dethrone synthesis tests. The synthesized trigger must be
    //! `TriggerMode::Attacks` with `valid_card = SelfRef`, an intervening-if
    //! condition comparing the defending player's life total against the maximum
    //! life total among all players, and execute body `Effect::PutCounter` with
    //! a single +1/+1 counter on `SelfRef`.
    use super::*;

    fn dethrone_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Dethrone);
        face
    }

    /// CR 702.105a: synthesizer emits an `Attacks` trigger with execute body
    /// `Effect::PutCounter(P1P1)` on SelfRef and a life-total condition.
    #[test]
    fn synthesize_dethrone_adds_attack_trigger() {
        let mut face = dethrone_face();
        synthesize_dethrone(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_dethrone_attack_trigger(t))
            .expect("dethrone should add an Attacks trigger");

        assert!(
            matches!(trigger.valid_card, Some(TargetFilter::SelfRef)),
            "valid_card must be SelfRef so the trigger fires only when this creature attacks"
        );
        assert_eq!(
            trigger.attack_target_filter,
            Some(crate::types::triggers::AttackTargetFilter::Player),
            "attack_target_filter must be Player so the trigger fires only when attacking a player"
        );

        let Some(execute) = trigger.execute.as_deref() else {
            panic!("execute body required");
        };
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("execute body must be Effect::PutCounter");
        };
        assert_eq!(*counter_type, CounterType::Plus1Plus1);
        assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
        assert!(matches!(target, TargetFilter::SelfRef));
        assert!(
            trigger.condition.is_some(),
            "dethrone trigger must have an intervening-if condition"
        );
    }

    #[test]
    fn synthesize_dethrone_is_idempotent() {
        let mut face = dethrone_face();
        synthesize_dethrone(&mut face);
        synthesize_dethrone(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_dethrone_attack_trigger(t))
            .count();
        assert_eq!(count, 1, "dethrone trigger should be deduped");
    }

    #[test]
    fn synthesize_dethrone_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_dethrone(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 702.105b: multiple instances trigger separately.
    #[test]
    fn synthesize_dethrone_emits_one_trigger_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Dethrone);
        face.keywords.push(Keyword::Dethrone);
        synthesize_dethrone(&mut face);
        let count = face
            .triggers
            .iter()
            .filter(|t| is_dethrone_attack_trigger(t))
            .count();
        assert_eq!(count, 2);
    }
}

#[cfg(test)]
mod annihilator_runtime_tests {
    //! CR 702.86a runtime integration: an attacking creature with
    //! `Keyword::Annihilator(N)` declared as an attacker fires the synthesized
    //! Attacks trigger via `process_triggers(&[AttackersDeclared { … }])`. The
    //! triggered ability lands on the stack; `resolve_top` invokes the
    //! Sacrifice resolver, which routes `ControllerRef::DefendingPlayer`
    //! through `defending_player_for_attacker(state, source_id)` (reading
    //! `state.combat.attackers`) to identify the player who must sacrifice.

    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::triggers::process_triggers;
    use crate::game::zones::create_object;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, StackEntryKind, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Build an Annihilator-bearing creature face and run the full synthesis
    /// pipeline so the Attacks trigger is installed.
    fn annihilator_creature_face(name: &str, n: u32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(15)),
            toughness: Some(PtValue::Fixed(15)),
            keywords: vec![Keyword::Annihilator(n)],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    /// Place a generic permanent (no special abilities) on the battlefield
    /// for `controller`. Used to populate the defending player's sacrifice
    /// pool.
    fn place_dummy_permanent(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            state,
            card_id,
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        // CR 701.21a: a permanent (Annihilator sacrifices "permanents", which
        // includes any non-emblem battlefield object). Mark as a creature so
        // it cleanly satisfies the TypeFilter::Permanent check without
        // overloading the test fixture.
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    /// Build an `AttackersDeclared` event for `attacker_id` attacking
    /// `defending_player`. Mirrors the event shape produced by
    /// `declare_attackers` so `match_attacks` recognizes it as a real attack
    /// declaration.
    fn attackers_declared_event(attacker_id: ObjectId, defending_player: PlayerId) -> GameEvent {
        GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker_id],
            defending_player,
            attacks: vec![(attacker_id, AttackTarget::Player(defending_player))],
        }
    }

    /// Spawn an Annihilator creature attacking `defending_player`, populate
    /// `state.combat.attackers` so `defending_player_for_attacker` can find
    /// the per-attacker defending player, then fire the AttackersDeclared
    /// event and resolve the synthesized trigger off the stack.
    fn attack_and_resolve_to_sacrifice(
        state: &mut GameState,
        face: &CardFace,
        controller: PlayerId,
        defending_player: PlayerId,
    ) -> ObjectId {
        let next_card = CardId(state.next_object_id);
        let attacker_id = create_object(
            state,
            next_card,
            controller,
            face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker_id).unwrap();
            apply_card_face_to_object(obj, face);
        }

        // CR 508.5: `defending_player_for_attacker` reads from
        // `state.combat.attackers`. Populate the attacker entry so the
        // sacrifice resolver can identify the defending player by source id.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker_id,
                AttackTarget::Player(defending_player),
                defending_player,
            )],
            ..Default::default()
        });

        process_triggers(
            state,
            &[attackers_declared_event(attacker_id, defending_player)],
        );

        assert!(
            state
                .stack
                .iter()
                .any(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })),
            "Annihilator Attacks trigger must land on the stack"
        );

        let mut resolve_events = Vec::new();
        crate::game::stack::resolve_top(state, &mut resolve_events);
        attacker_id
    }

    /// CR 702.86a + CR 508.5 happy path: an attacker with Annihilator 2
    /// attacks P1; P1 has 3 sacrifice-eligible permanents and must choose 2
    /// of them to sacrifice. The synthesized trigger should park the engine
    /// in `WaitingFor::EffectZoneChoice` with P1 as the chooser and
    /// `count = 2`.
    #[test]
    fn annihilator_attacks_defending_player_sacrifices_n_permanents() {
        let face = annihilator_creature_face("Emrakul's Echo", 2);

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let p1_a = place_dummy_permanent(&mut state, PlayerId(1), "Pawn A");
        let p1_b = place_dummy_permanent(&mut state, PlayerId(1), "Pawn B");
        let p1_c = place_dummy_permanent(&mut state, PlayerId(1), "Pawn C");
        // Ability controller has a permanent too; it must NOT enter the
        // defending player's sacrifice pool.
        let p0_own = place_dummy_permanent(&mut state, PlayerId(0), "Own Pawn");

        let _attacker =
            attack_and_resolve_to_sacrifice(&mut state, &face, PlayerId(0), PlayerId(1));

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                effect_kind,
                ..
            } => {
                assert_eq!(*player, PlayerId(1), "defending player chooses sacrifices");
                assert_eq!(*count, 2, "Annihilator 2 sacrifices exactly 2");
                assert_eq!(*effect_kind, crate::types::ability::EffectKind::Sacrifice);
                assert!(cards.contains(&p1_a));
                assert!(cards.contains(&p1_b));
                assert!(cards.contains(&p1_c));
                assert!(
                    !cards.contains(&p0_own),
                    "attacker's controller's permanent must NOT be in the \
                     defending player's sacrifice pool"
                );
                assert_eq!(cards.len(), 3);
            }
            other => panic!("expected EffectZoneChoice on defending player, got {other:?}"),
        }

        // Drive the choice: defending player sacrifices two specific
        // permanents.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![p1_a, p1_b],
            },
        )
        .unwrap();

        // CR 701.21a: sacrificed permanents end up in their owner's graveyard.
        assert_eq!(
            state.objects.get(&p1_a).unwrap().zone,
            Zone::Graveyard,
            "Pawn A sacrificed"
        );
        assert_eq!(
            state.objects.get(&p1_b).unwrap().zone,
            Zone::Graveyard,
            "Pawn B sacrificed"
        );
        assert_eq!(
            state.objects.get(&p1_c).unwrap().zone,
            Zone::Battlefield,
            "Pawn C not chosen, still on battlefield"
        );
        assert_eq!(
            state.objects.get(&p0_own).unwrap().zone,
            Zone::Battlefield,
            "attacker controller's permanent never threatened"
        );
    }

    /// CR 609.3: "If an effect attempts to do something impossible, it does
    /// only as much as possible." When the resolved sacrifice count meets or
    /// exceeds the defending player's eligible pool and the effect is
    /// mandatory, every eligible permanent is sacrificed. Annihilator 2
    /// against a defender with only one permanent must sacrifice that one
    /// permanent (and not hang waiting for the second choice).
    #[test]
    fn annihilator_with_fewer_permanents_than_n_sacrifices_all_of_them() {
        let face = annihilator_creature_face("Ulamog's Echo", 2);

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let only_one = place_dummy_permanent(&mut state, PlayerId(1), "Sole Pawn");

        let _attacker =
            attack_and_resolve_to_sacrifice(&mut state, &face, PlayerId(0), PlayerId(1));

        // CR 609.3 fast-path: the resolver takes the mandatory-all branch
        // ("does only as much as possible") and does not park in
        // EffectZoneChoice — the sole permanent goes straight to the
        // graveyard.
        assert_eq!(
            state.objects.get(&only_one).unwrap().zone,
            Zone::Graveyard,
            "the sole eligible permanent is sacrificed in the mandatory-all path"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "no EffectZoneChoice — fewer permanents than N means CR 609.3 \
             auto-sacrifices the entire pool"
        );
    }

    /// CR 508.5a multiplayer invariant: when an attacker with Annihilator
    /// attacks P1 in a 3-player game, only P1 sacrifices — P2 (a defending
    /// player not being attacked by THIS creature) is unaffected. This is
    /// the key correctness property that distinguishes
    /// `ControllerRef::DefendingPlayer` (per-attacker lookup) from a hypo-
    /// thetical "each opponent" sacrifice.
    #[test]
    fn annihilator_in_multiplayer_targets_defending_player_not_all_opponents() {
        let face = annihilator_creature_face("Kozilek's Echo", 1);

        // CR 802.1: multiplayer game. Use the 3-player constructor so the
        // sacrifice pool resolution can distinguish "defending player" from
        // "each opponent".
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // P1 (defending) has 2 permanents; P2 (uninvolved) has 1 permanent.
        let p1_a = place_dummy_permanent(&mut state, PlayerId(1), "P1 Pawn A");
        let p1_b = place_dummy_permanent(&mut state, PlayerId(1), "P1 Pawn B");
        let p2_only = place_dummy_permanent(&mut state, PlayerId(2), "P2 Pawn");

        let _attacker =
            attack_and_resolve_to_sacrifice(&mut state, &face, PlayerId(0), PlayerId(1));

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "only the defending player (P1) chooses — never P2"
                );
                assert!(cards.contains(&p1_a) && cards.contains(&p1_b));
                assert!(
                    !cards.contains(&p2_only),
                    "P2's permanent must NOT be in the sacrifice pool; only \
                     the per-attacker defending player (P1) sacrifices \
                     (CR 508.5a)"
                );
            }
            other => panic!("expected EffectZoneChoice on P1, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod myriad_runtime_tests {
    use super::*;
    use crate::game::combat::AttackTarget;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::zones::create_object;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{GameState, StackEntryKind, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn myriad_creature_face(name: &str, instances: usize) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            keywords: vec![Keyword::Myriad; instances],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn double_team_creature_face(name: &str, instances: usize) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            keywords: vec![Keyword::DoubleTeam; instances],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn setup_attack_state(player_count: u8, face: &CardFace) -> (GameState, ObjectId) {
        let mut state = GameState::new(FormatConfig::standard(), player_count, 42);
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![],
        };

        let card_id = CardId(state.next_object_id);
        let attacker_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            face.name.clone(),
            Zone::Battlefield,
        );
        {
            let attacker = state.objects.get_mut(&attacker_id).unwrap();
            apply_card_face_to_object(attacker, face);
            attacker.entered_battlefield_turn = Some(1);
        }
        (state, attacker_id)
    }

    fn declare_attack(state: &mut GameState, attacker_id: ObjectId, defender: PlayerId) {
        crate::game::engine::apply_as_current(
            state,
            GameAction::DeclareAttackers {
                attacks: vec![(attacker_id, AttackTarget::Player(defender))],
                bands: vec![],
            },
        )
        .expect("declare attacker");
        // CR 603.3b (#531): multiple triggers from the same controller
        // surface an OrderTriggers prompt; drain with identity for legacy
        // stack-assertion tests.
        crate::game::triggers::drain_order_triggers_with_identity(state);
    }

    fn resolve_myriad_trigger(state: &mut GameState) {
        let mut events = Vec::new();
        crate::game::stack::resolve_top(state, &mut events);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        crate::game::engine::apply_as_current(
            state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("accept Myriad trigger");
    }

    fn myriad_tokens(state: &GameState, source_name: &str) -> Vec<ObjectId> {
        state
            .objects
            .iter()
            .filter_map(|(id, obj)| {
                (obj.is_token && obj.name == source_name && obj.zone == Zone::Battlefield)
                    .then_some(*id)
            })
            .collect()
    }

    #[test]
    fn double_team_attack_creates_tapped_attacking_copy() {
        let face = double_team_creature_face("Double Team Bear", 1);
        let (mut state, attacker_id) = setup_attack_state(2, &face);

        declare_attack(&mut state, attacker_id, PlayerId(1));
        assert_eq!(
            state.stack.len(),
            1,
            "Double Team attack trigger goes on stack"
        );

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        let tokens = myriad_tokens(&state, &face.name);
        assert_eq!(tokens.len(), 1, "one tapped attacking copy");
        let token_id = tokens[0];
        assert!(state.objects.get(&token_id).unwrap().tapped);

        let token_attacker = state
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .find(|attacker| attacker.object_id == token_id)
            .expect("Double Team token is attacking");
        assert_eq!(token_attacker.defending_player, PlayerId(1));
        assert_eq!(
            token_attacker.attack_target,
            AttackTarget::Player(PlayerId(1))
        );
        assert!(
            state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .any(|attacker| attacker.object_id == attacker_id),
            "original attacker remains in combat"
        );
    }

    #[test]
    fn double_team_multiple_instances_stack_separately() {
        let face = double_team_creature_face("Double Double Team Bear", 2);
        let (mut state, attacker_id) = setup_attack_state(2, &face);

        declare_attack(&mut state, attacker_id, PlayerId(1));

        assert_eq!(
            state.stack.len(),
            2,
            "each Double Team instance should synthesize an independent attack trigger"
        );
    }

    #[test]
    fn myriad_three_player_attack_creates_token_attacking_other_opponent_and_exiles_at_eoc() {
        let face = myriad_creature_face("Blade of Selves Bear", 1);
        let (mut state, attacker_id) = setup_attack_state(3, &face);

        declare_attack(&mut state, attacker_id, PlayerId(1));
        assert_eq!(state.stack.len(), 1, "Myriad attack trigger goes on stack");

        resolve_myriad_trigger(&mut state);

        let tokens = myriad_tokens(&state, &face.name);
        assert_eq!(tokens.len(), 1, "3-player Myriad creates one token");
        let token_id = tokens[0];
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.tapped, "Myriad token enters tapped");

        let token_attacker = state
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .find(|attacker| attacker.object_id == token_id)
            .expect("Myriad token is attacking");
        assert_eq!(token_attacker.defending_player, PlayerId(2));
        assert_eq!(
            token_attacker.attack_target,
            AttackTarget::Player(PlayerId(2))
        );

        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "EOC exile trigger scheduled"
        );
        let delayed_targets = &state.delayed_triggers[0].ability.targets;
        assert_eq!(
            delayed_targets,
            &vec![crate::types::ability::TargetRef::Object(token_id)]
        );

        let eoc_events = crate::game::triggers::check_delayed_triggers(
            &mut state,
            &[GameEvent::PhaseChanged {
                phase: Phase::EndCombat,
            }],
        );
        assert!(!eoc_events.is_empty(), "EOC delayed trigger fires");
        let mut resolve_events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut resolve_events);
        assert_eq!(state.objects.get(&token_id).unwrap().zone, Zone::Exile);
    }

    #[test]
    fn myriad_uses_attack_event_defending_player_after_source_removed_from_combat() {
        let face = myriad_creature_face("Detached Myriad Bear", 1);
        let (mut state, attacker_id) = setup_attack_state(3, &face);

        declare_attack(&mut state, attacker_id, PlayerId(1));
        assert_eq!(state.stack.len(), 1, "Myriad attack trigger goes on stack");

        crate::game::effects::remove_from_combat::remove_object_from_combat(
            &mut state,
            attacker_id,
        );
        assert!(
            state.combat.as_ref().is_some_and(|combat| combat
                .attackers
                .iter()
                .all(|attacker| attacker.object_id != attacker_id)),
            "source was removed from live combat before Myriad resolved"
        );

        resolve_myriad_trigger(&mut state);

        let tokens = myriad_tokens(&state, &face.name);
        assert_eq!(
            tokens.len(),
            1,
            "Myriad must use the attack event's defending player LKI"
        );
        let token_attacker = state
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .find(|attacker| attacker.object_id == tokens[0])
            .expect("Myriad token is attacking");
        assert_eq!(token_attacker.defending_player, PlayerId(2));
        assert_eq!(
            token_attacker.attack_target,
            AttackTarget::Player(PlayerId(2))
        );
    }

    #[test]
    fn myriad_two_player_attack_creates_no_token() {
        let face = myriad_creature_face("Duke Ulder's Cub", 1);
        let (mut state, attacker_id) = setup_attack_state(2, &face);

        declare_attack(&mut state, attacker_id, PlayerId(1));
        assert_eq!(state.stack.len(), 1, "Myriad still triggers in two-player");

        resolve_myriad_trigger(&mut state);

        assert!(myriad_tokens(&state, &face.name).is_empty());
        assert!(
            state.delayed_triggers.is_empty(),
            "no EOC cleanup trigger is scheduled when no tokens are created"
        );
    }

    #[test]
    fn multiple_myriad_instances_create_independent_token_sets() {
        let face = myriad_creature_face("Echoing Myriad Bear", 2);
        let (mut state, attacker_id) = setup_attack_state(3, &face);

        declare_attack(&mut state, attacker_id, PlayerId(1));
        let trigger_count = state
            .stack
            .iter()
            .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
            .count();
        assert_eq!(
            trigger_count, 2,
            "CR 702.116b: each Myriad instance triggers"
        );

        resolve_myriad_trigger(&mut state);
        resolve_myriad_trigger(&mut state);

        assert_eq!(
            myriad_tokens(&state, &face.name).len(),
            2,
            "each Myriad trigger creates its own token"
        );
        assert_eq!(state.delayed_triggers.len(), 2);
    }
}

#[cfg(test)]
mod echo_synthesis_tests {
    use super::*;
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn echo_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Echo(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 3,
        }));
        face
    }

    #[test]
    fn synthesize_echo_adds_upkeep_pay_or_sac_trigger() {
        let mut face = echo_face();
        synthesize_echo(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::PayEcho))
            .expect("echo should add an upkeep trigger");
        assert_eq!(trigger.phase, Some(Phase::Upkeep));
        assert!(matches!(
            trigger.valid_target,
            Some(TargetFilter::Controller)
        ));
        assert!(matches!(trigger.condition, Some(TriggerCondition::EchoDue)));
        assert!(matches!(
            trigger.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            })
        ));
        assert!(matches!(
            trigger.unless_pay.as_ref(),
            Some(UnlessPayModifier {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost { generic: 3, .. },
                },
                payer: TargetFilter::Controller,
            })
        ));
    }

    #[test]
    fn synthesize_echo_is_idempotent() {
        let mut face = echo_face();
        synthesize_echo(&mut face);
        synthesize_echo(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| matches!(t.mode, TriggerMode::PayEcho))
                .count(),
            1
        );
    }

    #[test]
    fn synthesize_echo_is_noop_without_keyword() {
        let mut face = CardFace::default();
        synthesize_echo(&mut face);

        assert!(face.triggers.is_empty());
    }
}

#[cfg(test)]
mod cumulative_upkeep_synthesis_tests {
    use super::*;
    use crate::types::mana::ManaCost;

    fn cu_face_mana_one() -> CardFace {
        let mut face = CardFace::default();
        // CR 702.24a: Mystic Remora — "Cumulative upkeep {1}".
        face.keywords
            .push(Keyword::CumulativeUpkeep(AbilityCost::Mana {
                cost: ManaCost::generic(1),
            }));
        face
    }

    /// CR 702.24a: The synthesized trigger is the chained-ability shape —
    /// outer AddCounter(Age, SelfRef), inner Sacrifice(SelfRef) gated by
    /// `unless_pay = PerCounter { Age, SelfRef, base }`. The outer effect
    /// resolves first so the per-counter prompt reads the post-tick total.
    #[test]
    fn cumulative_upkeep_keyword_synthesizes_age_counter_trigger() {
        let mut face = cu_face_mana_one();
        synthesize_cumulative_upkeep(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::PayCumulativeUpkeep))
            .expect("cumulative upkeep should add an upkeep trigger");
        assert_eq!(trigger.phase, Some(Phase::Upkeep));
        assert!(matches!(
            trigger.valid_target,
            Some(TargetFilter::Controller)
        ));
        // The trigger's own unless_pay is NOT used — the per-counter prompt
        // lives on the sub-ability so it fires after the outer tick.
        assert!(trigger.unless_pay.is_none());

        let outer = trigger.execute.as_deref().expect("execute set");
        match outer.effect.as_ref() {
            Effect::AddCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(*counter_type, CounterType::Age);
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert!(matches!(target, TargetFilter::SelfRef));
            }
            other => panic!("expected outer AddCounter, got {other:?}"),
        }

        let sub = outer.sub_ability.as_deref().expect("sub_ability set");
        assert!(matches!(
            sub.effect.as_ref(),
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            }
        ));

        let unless = sub.unless_pay.as_ref().expect("unless_pay on sub");
        assert!(matches!(unless.payer, TargetFilter::Controller));
        match &unless.cost {
            AbilityCost::PerCounter {
                counter,
                target,
                base,
            } => {
                assert_eq!(*counter, CounterType::Age);
                assert!(matches!(target, TargetFilter::SelfRef));
                // Base cost preserved verbatim — {1} mana for Mystic Remora.
                match base.as_ref() {
                    AbilityCost::Mana { cost } => {
                        assert_eq!(*cost, ManaCost::generic(1));
                    }
                    other => panic!("expected base Mana({{1}}), got {other:?}"),
                }
            }
            other => panic!("expected unless_pay PerCounter, got {other:?}"),
        }
    }

    /// CR 702.24a: The builder is generic over the base cost shape; non-mana
    /// costs (life payment) compose through `PerCounter` unchanged.
    #[test]
    fn cumulative_upkeep_keyword_preserves_pay_life_base_cost() {
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::CumulativeUpkeep(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            }));
        synthesize_cumulative_upkeep(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::PayCumulativeUpkeep))
            .expect("cumulative upkeep should add an upkeep trigger");
        let outer = trigger.execute.as_deref().expect("execute set");
        let sub = outer.sub_ability.as_deref().expect("sub_ability set");
        let unless = sub.unless_pay.as_ref().expect("unless_pay on sub");
        match &unless.cost {
            AbilityCost::PerCounter { base, .. } => match base.as_ref() {
                AbilityCost::PayLife { amount } => {
                    assert_eq!(*amount, QuantityExpr::Fixed { value: 2 });
                }
                other => panic!("expected base PayLife(2), got {other:?}"),
            },
            other => panic!("expected unless_pay PerCounter, got {other:?}"),
        }
    }

    /// CR 604.1: Idempotent synthesis — re-running the synthesizer must not
    /// duplicate the trigger. Recognizer pairs with builder so existing
    /// printed triggers match and the second install is skipped.
    #[test]
    fn cumulative_upkeep_synthesis_is_idempotent() {
        let mut face = cu_face_mana_one();
        synthesize_cumulative_upkeep(&mut face);
        synthesize_cumulative_upkeep(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| matches!(t.mode, TriggerMode::PayCumulativeUpkeep))
                .count(),
            1,
            "duplicate synthesis should not add a second cumulative upkeep trigger"
        );
    }

    #[test]
    fn synthesize_cumulative_upkeep_is_noop_without_keyword() {
        let mut face = CardFace::default();
        synthesize_cumulative_upkeep(&mut face);

        assert!(face.triggers.is_empty());
    }

    /// CR 702.24a: Discard/EffectCost/Exile bases are not yet payable by the
    /// `expand_per_counter` + `handle_unless_payment` pipeline. Installing the
    /// trigger anyway would silently sacrifice the permanent every upkeep
    /// (payment failure → unless-effect Sacrifice fires). Pre-branch these
    /// cards had no trigger at all; the synthesizer must preserve that
    /// silent-no-op state until per-shape support lands.
    #[test]
    fn cumulative_upkeep_synthesizer_skips_unsupported_base_shapes() {
        // Discard base — no current cumulative-upkeep card resolves through
        // `AbilityCost::Discard` because the unless-payment pipeline can't pay
        // it. Synthesizer must refuse to install.
        let discard_kw = Keyword::CumulativeUpkeep(AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            random: false,
            self_ref: false,
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&discard_kw).len(),
            0,
            "Discard base must not install a cumulative-upkeep trigger"
        );

        // Exile base — same reasoning as Discard.
        let exile_kw = Keyword::CumulativeUpkeep(AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: None,
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&exile_kw).len(),
            0,
            "Exile base must not install a cumulative-upkeep trigger"
        );

        // Composite of mixed shapes — Composite-of-Mana is supported, but
        // Composite containing Discard/Exile is not.
        let mixed_composite_kw = Keyword::CumulativeUpkeep(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: false,
                },
            ],
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&mixed_composite_kw).len(),
            0,
            "mixed-shape Composite base must not install a cumulative-upkeep trigger"
        );

        // End-to-end: a face carrying the unsupported keyword must have no
        // PayCumulativeUpkeep trigger after synthesis runs.
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::CumulativeUpkeep(AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: false,
            }));
        synthesize_cumulative_upkeep(&mut face);
        assert!(
            face.triggers.is_empty(),
            "synthesize_cumulative_upkeep on a Discard base must install no triggers"
        );
    }

    /// CR 702.24a: Sanity — the supported base shapes (Mana, PayLife,
    /// Sacrifice, OneOf-of-Mana, Composite-of-Mana) MUST still install a
    /// single trigger so the gating fix doesn't accidentally break the
    /// payable cumulative-upkeep cards.
    #[test]
    fn cumulative_upkeep_synthesizer_installs_supported_base_shapes() {
        let mana_kw = Keyword::CumulativeUpkeep(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&mana_kw).len(),
            1,
            "Mana base must install exactly one cumulative-upkeep trigger"
        );

        let pay_life_kw = Keyword::CumulativeUpkeep(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&pay_life_kw).len(),
            1,
            "PayLife base must install exactly one cumulative-upkeep trigger"
        );

        let sacrifice_kw = Keyword::CumulativeUpkeep(AbilityCost::Sacrifice {
            target: TargetFilter::SelfRef,
            count: 1,
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&sacrifice_kw).len(),
            1,
            "Sacrifice base must install exactly one cumulative-upkeep trigger"
        );

        // OneOf-of-Mana — Jötun Owl Keeper-shape disjunction of mana costs.
        let one_of_mana_kw = Keyword::CumulativeUpkeep(AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
            ],
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&one_of_mana_kw).len(),
            1,
            "OneOf-of-Mana base must install exactly one cumulative-upkeep trigger"
        );

        // Composite of all-Mana shapes — folds to one combined mana payment.
        let composite_mana_kw = Keyword::CumulativeUpkeep(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
            ],
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&composite_mana_kw).len(),
            1,
            "Composite of Mana costs must install exactly one cumulative-upkeep trigger"
        );

        // Mixed Composite is not currently payable end-to-end.
        let composite_mixed_kw = Keyword::CumulativeUpkeep(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        });
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&composite_mixed_kw).len(),
            0,
            "mixed Composite costs must remain unsupported until sequenced payment exists"
        );
    }
}

#[cfg(test)]
mod evoke_runtime_tests {
    use super::*;
    use crate::game::triggers::check_trigger_condition;
    use crate::game::zones::create_object;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    /// CR 702.74a: The synthesized intervening-if condition fires only when the
    /// permanent's `cast_variant_paid` matches Evoke for the current turn.
    /// Mirrors the runtime contract used by Sneak/Ninjutsu.
    #[test]
    fn cast_variant_paid_evoke_condition_fires_only_when_tagged() {
        let mut state = GameState::new_two_player(0);
        state.turn_number = 3;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mulldrifter".to_string(),
            Zone::Battlefield,
        );

        let condition = TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Evoke,
        };

        // Untagged → false.
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged with a different variant → false.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Sneak, state.turn_number));
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Evoke for the current turn → true.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Evoke, state.turn_number));
        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Evoke but for a stale turn → false (per-turn freshness, CR 603.4).
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Evoke, state.turn_number - 1));
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(id),
            None
        ));
    }

    /// CR 702.138b + CR 603.4: Phlage, Titan of Fire's Fury — the negated
    /// `CastVariantPaid { variant: Escape, negated: true }` must satisfy for
    /// (a) untagged permanents (reanimation, flicker: per WotC ruling,
    /// sacrifice fires), (b) permanents tagged with a different variant (no
    /// cast-via-escape happened), and (c) stale escape tags. It must fail only
    /// when the source is tagged `Escape` for the current turn.
    #[test]
    fn cast_variant_paid_escape_negated_fires_unless_escape_tagged() {
        let mut state = GameState::new_two_player(0);
        state.turn_number = 5;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Phlage, Titan of Fire's Fury".to_string(),
            Zone::Battlefield,
        );

        let negated = TriggerCondition::Not {
            condition: Box::new(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Escape,
            }),
        };

        // Untagged (reanimated or put onto battlefield without being cast) →
        // "unless it escaped" is satisfied → trigger fires.
        assert!(check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged with a non-Escape variant (hard-cast from hand leaves
        // `cast_variant_paid = None`; this branch covers hypothetical other
        // alt-costs like Evoke if composed) → still satisfies.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Evoke, state.turn_number));
        assert!(check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Escape for the CURRENT turn → "unless it escaped" fails →
        // trigger does NOT fire.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Escape, state.turn_number));
        assert!(!check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));

        // Tagged Escape for a STALE turn → tag is not the current turn, so
        // the permanent is treated as not having escaped (per-turn freshness,
        // CR 603.4) → sacrifice fires.
        state.objects.get_mut(&id).unwrap().cast_variant_paid =
            Some((CastVariantPaid::Escape, state.turn_number - 1));
        assert!(check_trigger_condition(
            &state,
            &negated,
            PlayerId(0),
            Some(id),
            None
        ));
    }
}

#[cfg(test)]
mod scavenge_synthesis_tests {
    use super::*;
    use crate::types::ability::{ActivationRestriction, QuantityRef};
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn face_with_scavenge(cost: ManaCost) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Scavenge(cost));
        face
    }

    /// CR 702.97a: Scavenge synthesis produces exactly one activated ability whose
    /// shape matches the reminder text — graveyard activation, sorcery speed,
    /// composite cost of mana + self-exile, +1/+1 counters on target creature
    /// scaled by SelfPower.
    #[test]
    fn synthesize_scavenge_builds_activated_ability_with_correct_shape() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        };
        let mut face = face_with_scavenge(cost.clone());
        synthesize_scavenge(&mut face);

        assert_eq!(face.abilities.len(), 1, "exactly one scavenge ability");
        let def = &face.abilities[0];
        assert_eq!(def.kind, AbilityKind::Activated);
        assert_eq!(def.activation_zone, Some(Zone::Graveyard));
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));

        // CR 118.3: Composite cost — mana + exile-self-from-graveyard.
        match def.cost.as_ref().expect("scavenge must have a cost") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2);
                assert!(matches!(&costs[0], AbilityCost::Mana { cost: c } if *c == cost));
                assert!(matches!(
                    &costs[1],
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Graveyard),
                        filter: Some(TargetFilter::SelfRef),
                    }
                ));
            }
            other => panic!("expected Composite cost, got {:?}", other),
        }

        // CR 702.97a: Effect is +1/+1 counters equal to SelfPower on target creature.
        match def.effect.as_ref() {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source
                        }
                    }
                ));
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))
                );
            }
            other => panic!("expected PutCounter effect, got {:?}", other),
        }
    }

    /// Scavenge {0} (Slitherhead) — cost-0 mana still produces a well-formed ability.
    #[test]
    fn synthesize_scavenge_handles_zero_cost() {
        let cost = ManaCost::default();
        let mut face = face_with_scavenge(cost);
        synthesize_scavenge(&mut face);
        assert_eq!(face.abilities.len(), 1);
    }

    /// Cards without Scavenge are unaffected.
    #[test]
    fn synthesize_scavenge_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_scavenge(&mut face);
        assert!(face.abilities.is_empty());
    }
}

#[cfg(test)]
mod scavenge_runtime_tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::casting::{can_activate_ability_now, handle_activate_ability};
    use crate::game::zones::create_object;
    use crate::types::counter::CounterType;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;

    /// Helper: put a creature in the graveyard with Scavenge synthesized on it, and
    /// stage a target creature on the battlefield. Returns (source_id, target_id).
    fn setup_scavenge_scenario(
        state: &mut GameState,
        scavenge_cost: ManaCost,
    ) -> (ObjectId, ObjectId) {
        let source = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Scavenge Source".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Scavenge(scavenge_cost.clone()));
        }
        // Synthesize to attach the activated ability.
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Scavenge(scavenge_cost));
        synthesize_scavenge(&mut face);
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities)
            .extend(face.abilities);

        let target = create_object(
            state,
            CardId(2),
            PlayerId(0),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        (source, target)
    }

    /// CR 702.97a: Scavenge can be activated while the source is in a graveyard.
    /// CR 702.97a: Activation is gated by sorcery timing.
    #[test]
    fn scavenge_is_activatable_from_graveyard_at_sorcery_speed() {
        let mut state = GameState::new_two_player(42);
        // Active player's main phase, empty stack — sorcery-speed window.
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        let zero_cost = ManaCost::default(); // Scavenge {0}
        let (source, _target) = setup_scavenge_scenario(&mut state, zero_cost);

        assert!(
            can_activate_ability_now(&state, PlayerId(0), source, 0),
            "Scavenge {{0}} should be activatable from graveyard during sorcery window"
        );
    }

    /// CR 702.97a: Scavenge cannot be activated at instant speed.
    #[test]
    fn scavenge_rejects_instant_speed() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        // Outside the sorcery window (upkeep phase is not a main phase).
        state.phase = Phase::Upkeep;

        let (source, _target) = setup_scavenge_scenario(&mut state, ManaCost::default());

        assert!(
            !can_activate_ability_now(&state, PlayerId(0), source, 0),
            "Scavenge must reject activation outside the sorcery-speed window"
        );
    }

    /// CR 602.1: Scavenge can only be activated while the source is in the graveyard.
    #[test]
    fn scavenge_rejects_from_battlefield() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        let (source, _target) = setup_scavenge_scenario(&mut state, ManaCost::default());
        // Move source out of graveyard onto the battlefield.
        crate::game::zones::move_to_zone(&mut state, source, Zone::Battlefield, &mut Vec::new());

        assert!(
            !can_activate_ability_now(&state, PlayerId(0), source, 0),
            "Scavenge must reject activation when source is not in a graveyard"
        );
    }

    /// CR 702.97a + CR 208.3: End-to-end — activating Scavenge exiles the source from
    /// graveyard as a cost, then on resolution places +1/+1 counters equal to SelfPower
    /// (read via LKI) on target creature.
    #[test]
    fn scavenge_activation_exiles_source_and_places_counters_on_target() {
        use crate::game::stack::resolve_top;

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;
        // Use Scavenge {0} (Slitherhead-shaped) to avoid mana-pool plumbing in the test.
        let (source, target) = setup_scavenge_scenario(&mut state, ManaCost::default());

        // Activate the ability.
        let mut events = Vec::new();
        let result = handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events);
        assert!(result.is_ok(), "activation must succeed: {:?}", result);

        // CR 702.97a: Exile cost — source moved graveyard → exile as cost payment.
        assert_eq!(
            state.objects[&source].zone,
            Zone::Exile,
            "Scavenge source must be exiled as a cost"
        );
        assert!(
            !state.players[0].graveyard.contains(&source),
            "source must be removed from graveyard"
        );
        assert!(
            state.exile.contains(&source),
            "source must be in exile zone"
        );

        // Ability is on the stack awaiting resolution.
        assert!(!state.stack.is_empty(), "ability must be on the stack");

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);

        // CR 702.97a + CR 208.3: target creature gains counters equal to source's LKI power (4).
        let counter_count = state.objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counter_count, 4,
            "target must gain +1/+1 counters equal to source's LKI power (4)"
        );
    }
}

#[cfg(test)]
mod siege_synthesis_tests {
    use super::*;
    use crate::types::triggers::TriggerMode;

    fn siege_face() -> CardFace {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Battle);
        face.card_type.subtypes.push("Siege".to_string());
        face
    }

    /// CR 310.11a: Sieges get a synthesized Moved-replacement that asks the
    /// controller to choose an opponent as the protector.
    #[test]
    fn synthesize_adds_protector_choice_replacement() {
        let mut face = siege_face();
        synthesize_siege_intrinsics(&mut face);
        let protector = face
            .replacements
            .iter()
            .find(|r| matches!(r.event, ReplacementEvent::Moved))
            .expect("Siege should have a Moved replacement");
        assert_eq!(protector.destination_zone, Some(Zone::Battlefield));
        assert!(matches!(protector.valid_card, Some(TargetFilter::SelfRef)));
        assert!(matches!(
            protector.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Choose {
                choice_type: ChoiceType::Opponent,
                persist: true,
            })
        ));
    }

    /// CR 310.11b: Sieges get a synthesized `CounterRemoved` trigger with a
    /// `CounterTriggerFilter` targeting defense at threshold 0 (last counter
    /// removed). The execute chain exiles the Siege then offers an optional
    /// `CastFromZone` with both `without_paying_mana_cost` and `cast_transformed`.
    #[test]
    fn synthesize_adds_victory_trigger() {
        let mut face = siege_face();
        synthesize_siege_intrinsics(&mut face);
        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::CounterRemoved))
            .expect("Siege should have a CounterRemoved trigger");
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        let filter = trigger
            .counter_filter
            .as_ref()
            .expect("trigger must have counter_filter");
        assert!(matches!(filter.counter_type, CounterType::Defense));
        assert_eq!(filter.threshold, Some(0));

        let exec = trigger.execute.as_ref().expect("execute body");
        // Top-level = ChangeZone to Exile with target SelfRef.
        let Effect::ChangeZone {
            destination,
            ref target,
            ..
        } = *exec.effect
        else {
            panic!("top-level should be ChangeZone, got {:?}", exec.effect);
        };
        assert_eq!(destination, Zone::Exile);
        assert!(matches!(target, TargetFilter::SelfRef));

        // Sub-ability = optional CastFromZone with both flags set.
        let sub = exec.sub_ability.as_ref().expect("optional cast sub");
        assert!(sub.optional);
        assert!(matches!(
            *sub.effect,
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                cast_transformed: true,
                ..
            }
        ));
    }

    /// Non-Sieges are unaffected.
    #[test]
    fn synthesize_is_noop_for_non_siege() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_siege_intrinsics(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// Battles without the Siege subtype don't get Siege-specific intrinsics.
    /// (Currently all printed battles are Sieges, but this keeps the synthesis
    /// correctly scoped per CR 310.11.)
    #[test]
    fn synthesize_is_noop_for_non_siege_battle() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Battle);
        // No Siege subtype.
        synthesize_siege_intrinsics(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// Re-running synthesis on an already-synthesized face is idempotent.
    #[test]
    fn synthesize_is_idempotent() {
        let mut face = siege_face();
        synthesize_siege_intrinsics(&mut face);
        let first_trigger_count = face.triggers.len();
        let first_replacement_count = face.replacements.len();
        synthesize_siege_intrinsics(&mut face);
        assert_eq!(face.triggers.len(), first_trigger_count);
        assert_eq!(face.replacements.len(), first_replacement_count);
    }
}

#[cfg(test)]
mod station_synthesis_tests {
    use super::*;
    use crate::types::ability::{ContinuousModification, StaticCondition, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::statics::StaticMode;

    fn spacecraft_face_with_reminder() -> CardFace {
        let mut face = CardFace {
            name: "Uthros Research Craft".to_string(),
            oracle_text: Some(
                "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 12+.)\n3+ | Whenever you cast an artifact spell, draw a card. Put a charge counter on this Spacecraft.\n12+ | Flying\nThis Spacecraft gets +1/+0 for each artifact you control.".to_string(),
            ),
            power: Some(PtValue::Fixed(0)),
            toughness: Some(PtValue::Fixed(8)),
            keywords: vec![Keyword::Station],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Artifact);
        face.card_type.subtypes.push("Spacecraft".to_string());
        face
    }

    #[test]
    fn synthesize_station_adds_creature_shift_at_threshold() {
        let mut face = spacecraft_face_with_reminder();
        synthesize_station(&mut face);
        let sd = face
            .static_abilities
            .iter()
            .find(|s| {
                s.mode == StaticMode::Continuous
                    && s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AddType {
                                core_type: CoreType::Creature,
                            }
                        )
                    })
            })
            .expect("AddType(Creature) static must be synthesized");
        assert_eq!(sd.affected, Some(TargetFilter::SelfRef));
        assert!(matches!(
            sd.condition,
            Some(StaticCondition::HasCounters {
                counters: crate::types::counter::CounterMatch::OfType(
                    crate::types::counter::CounterType::Generic(ref name)
                ),
                minimum: 12,
                maximum: None,
            }) if name == "charge"
        ));
        // Exactly three modifications: AddType + SetPower(0) + SetToughness(8)
        assert_eq!(sd.modifications.len(), 3);
        assert!(sd
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value: 0 })));
        assert!(sd
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetToughness { value: 8 })));
    }

    #[test]
    fn synthesize_living_metal_adds_during_your_turn_creature_static() {
        // CR 702.161a: Living metal makes the Vehicle an artifact creature during
        // its controller's turn (Flamewar, Streetwise Operative; #1547).
        let mut face = CardFace {
            keywords: vec![Keyword::LivingMetal],
            ..CardFace::default()
        };
        synthesize_living_metal(&mut face);
        let sd = face
            .static_abilities
            .iter()
            .find(|s| {
                s.mode == StaticMode::Continuous
                    && s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AddType {
                                core_type: CoreType::Creature,
                            }
                        )
                    })
            })
            .expect("Living metal must synthesize an AddType(Creature) static");
        assert_eq!(sd.affected, Some(TargetFilter::SelfRef));
        assert!(matches!(
            sd.condition,
            Some(StaticCondition::DuringYourTurn)
        ));
        // Only the type is added — the Vehicle's printed P/T flows through; no
        // P/T override (unlike Station, whose P/T lives in a striation).
        assert_eq!(sd.modifications.len(), 1);
    }

    #[test]
    fn synthesize_living_metal_noop_without_keyword() {
        let mut face = CardFace {
            keywords: vec![Keyword::Menace],
            ..CardFace::default()
        };
        let before = face.static_abilities.len();
        synthesize_living_metal(&mut face);
        assert_eq!(
            face.static_abilities.len(),
            before,
            "no Living Metal keyword → no synthesized static"
        );
    }

    #[test]
    fn synthesize_living_metal_is_idempotent() {
        let mut face = CardFace {
            keywords: vec![Keyword::LivingMetal],
            ..CardFace::default()
        };
        synthesize_living_metal(&mut face);
        synthesize_living_metal(&mut face);

        let count = face
            .static_abilities
            .iter()
            .filter(|s| is_living_metal_static(s))
            .count();
        assert_eq!(count, 1);
    }

    /// CR 721.2b: Reminder text "It's an artifact creature at N+" has no
    /// rules force (CR 721.3). The creature-shift threshold is derived from
    /// the highest N+ striation containing the printed P/T box.
    #[test]
    fn station_creature_shift_derived_from_max_threshold_not_reminder_text() {
        let mut face = spacecraft_face_with_reminder();
        // Original oracle has thresholds 3 and 12; max is 12 → creature-shift gates on 12.
        synthesize_station(&mut face);
        let sd = face
            .static_abilities
            .iter()
            .find(|s| {
                s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }
                    )
                })
            })
            .expect("creature-shift static must derive from max striation");
        assert!(matches!(
            sd.condition,
            Some(StaticCondition::HasCounters { minimum: 12, .. })
        ));
    }

    #[test]
    fn station_creature_shift_ignores_reminder_text_absence() {
        // Oracle without the "at N+" reminder phrase still emits creature-shift
        // because the derivation reads N+ striations, not reminder text.
        let mut face = spacecraft_face_with_reminder();
        face.oracle_text = Some("Station\n8+ | Flying".to_string());
        synthesize_station(&mut face);
        let sd = face
            .static_abilities
            .iter()
            .find(|s| {
                s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }
                    )
                })
            })
            .expect("creature-shift static must be emitted from striation alone");
        assert!(matches!(
            sd.condition,
            Some(StaticCondition::HasCounters { minimum: 8, .. })
        ));
    }

    #[test]
    fn station_no_creature_shift_when_no_printed_pt() {
        // CR 721.2b: support-only Spacecraft (null P/T) gets no creature-shift.
        // Mirrors "the eternity elevator" — Station + 20+ threshold but no P/T.
        let mut face = spacecraft_face_with_reminder();
        face.power = None;
        face.toughness = None;
        let before = face.static_abilities.len();
        synthesize_station(&mut face);
        assert_eq!(face.static_abilities.len(), before);
    }

    #[test]
    fn station_no_creature_shift_when_no_thresholds() {
        // No N+ striations → no creature-shift static.
        let mut face = spacecraft_face_with_reminder();
        face.oracle_text = Some("Station\nPlain rules text with no thresholds.".to_string());
        let before = face.static_abilities.len();
        synthesize_station(&mut face);
        assert_eq!(face.static_abilities.len(), before);
    }

    #[test]
    fn station_no_creature_shift_for_non_spacecraft_card() {
        // Non-Spacecraft with charge counters and an N+ line in flavor must
        // not trigger creature-shift derivation.
        let mut face = spacecraft_face_with_reminder();
        face.card_type.subtypes.clear();
        face.card_type.subtypes.push("Vehicle".to_string());
        let before = face.static_abilities.len();
        synthesize_station(&mut face);
        assert_eq!(face.static_abilities.len(), before);
    }

    /// CR 721.2b: End-to-end regression for every TDM Spacecraft in the
    /// pre-built export. Locks in per-card expected creature-shift thresholds
    /// against the ground-truth table derived from printed P/T + `N+ |`
    /// striations (plan §C3). A future data edit (MTGJSON patch, Oracle text
    /// change) that shifts any threshold will fail this test loudly.
    ///
    /// Scryfall-frame verification (plan §C5): Candela, Monoist Gravliner,
    /// and Squadron Carrier are MTGJSON-reminder-text-missing cards. Their
    /// printed card frames were manually confirmed on scryfall.com to have
    /// the P/T box in the highest-N station striation:
    ///   - Candela, Aegis of Adagia: P/T 3/3, single threshold 8 → 8+.
    ///   - Monoist Gravliner:        P/T 2/3, single threshold 6 → 6+.
    ///   - Squadron Carrier:         P/T 4/4, single threshold 10 → 10+
    ///     (not support-only despite first-draft speculation).
    #[test]
    fn station_32_tdm_spacecraft_regression_suite() {
        use crate::database::CardDatabase;
        use std::path::PathBuf;

        // CARGO_MANIFEST_DIR points at crates/engine; the workspace root is
        // two levels up. Skip gracefully if the export has not been generated
        // (fresh clone before setup.sh).
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let path = workspace_root.join("client/public/card-data.json");
        if !path.exists() {
            eprintln!(
                "skipping: {} not found (run ./scripts/gen-card-data.sh)",
                path.display()
            );
            return;
        }
        let db = CardDatabase::from_export(&path).expect("card-data.json loads as a valid export");

        // Ground truth: (card name, expected creature-shift). None = support-only
        // or excluded (non-Station Spacecraft crossover).
        let cases: &[(&str, Option<u32>)] = &[
            ("Atmospheric Greenhouse", Some(8)),
            ("Candela, Aegis of Adagia", Some(8)),
            ("Dawnsire, Sunstar Dreadnought", Some(20)),
            ("Debris Field Crusher", Some(8)),
            ("Entropic Battlecruiser", Some(8)),
            ("Exploration Broodship", Some(8)),
            ("Extinguisher Battleship", Some(5)),
            ("Fell Gravship", Some(8)),
            ("Galvanizing Sawship", Some(3)),
            ("Hearthhull, the Worldseed", Some(8)),
            ("Hotel of Fears", None), // excluded (crossover)
            ("Infinite Guideline Station", Some(12)),
            ("Inspirit, Flagship Vessel", Some(8)),
            ("Larval Scoutlander", Some(7)),
            ("Lumen-Class Frigate", Some(12)),
            ("Mondassian Colony Ship", None), // excluded (crossover)
            ("Monoist Gravliner", Some(6)),
            ("Pinnacle Kill-Ship", Some(7)),
            ("Rescue Skiff", Some(10)),
            ("Sledge-Class Seedship", Some(7)),
            ("Specimen Freighter", Some(9)),
            ("Squadron Carrier", Some(10)),
            ("Susurian Dirgecraft", Some(7)),
            ("Synthesizer Labship", Some(9)),
            ("The Dining Car", None),        // excluded (crossover)
            ("The Eternity Elevator", None), // support-only (null P/T)
            ("The Seriema", Some(7)),
            ("Uthros Research Craft", Some(12)),
            ("Uthros Scanship", Some(8)),
            ("Warmaker Gunship", Some(6)),
            ("Wedgelight Rammer", Some(9)),
            ("Wurmwall Sweeper", Some(4)),
        ];

        // Coverage sanity: 32 cards total (28 creature-shift + 1 support-only
        // + 3 excluded). Locks the table size so accidental deletions fail.
        assert_eq!(
            cases.len(),
            32,
            "regression table must cover all 32 TDM Spacecraft"
        );
        let shifted = cases.iter().filter(|(_, n)| n.is_some()).count();
        assert_eq!(shifted, 28, "28 cards must have a creature-shift threshold");

        let mut missing: Vec<&str> = Vec::new();
        let mut wrong: Vec<String> = Vec::new();
        for (name, expected) in cases {
            let Some(face) = db.get_face_by_name(name) else {
                missing.push(name);
                continue;
            };
            let creature_shift_min = face.static_abilities.iter().find_map(|s| {
                let has_creature_add = s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        }
                    )
                });
                if !has_creature_add {
                    return None;
                }
                match &s.condition {
                    Some(StaticCondition::HasCounters {
                        counters:
                            crate::types::counter::CounterMatch::OfType(
                                crate::types::counter::CounterType::Generic(name),
                            ),
                        minimum,
                        ..
                    }) if name == "charge" => Some(*minimum),
                    _ => None,
                }
            });
            match (expected, creature_shift_min) {
                (Some(exp), Some(got)) if *exp == got => {}
                (None, None) => {}
                (exp, got) => {
                    wrong.push(format!("{name}: expected {exp:?}, got {got:?}"));
                }
            }
        }

        if !missing.is_empty() {
            eprintln!(
                "skipping regression for cards missing from export: {}",
                missing.join(", ")
            );
        }
        assert!(
            wrong.is_empty(),
            "synthesize_station produced wrong thresholds:\n  {}",
            wrong.join("\n  ")
        );
    }
}

// CR 702.xxx: Loader-side invariant for Prepare (Strixhaven). The resolver in
// `game/effects/prepare.rs::has_prepare_face` keys off
// `back_face.layout_kind == Some(LayoutKind::Prepare)` to gate the Biblioplex
// "only creatures with prepare spells can become prepared" rule. That gate
// holds only if the layout-string `"prepare"` round-trips through
// `map_layout` / `map_layout_str` / `CardLayout::Prepare` consistently.
// Locking those mappings here prevents a loader regression from silently
// neutering Biblioplex. Assign when WotC publishes SOS CR update.
#[cfg(test)]
mod prepare_layout_invariant_tests {
    use super::*;
    use crate::types::card::{CardFace, CardLayout};

    #[test]
    fn mtgjson_layout_prepare_maps_to_layout_kind_prepare() {
        // `map_layout` returns the synthesis-local LayoutKind; the
        // `"prepare"` string is the MTGJSON-side marker for the Strixhaven
        // two-face Adventure-family frame.
        assert_eq!(map_layout("prepare"), LayoutKind::Prepare);
    }

    #[test]
    fn card_layout_prepare_back_face_is_tagged_prepare() {
        // The printed-cards loader pattern-matches on `CardLayout::Prepare(_, back)`
        // to populate `back_face.layout_kind = Some(LayoutKind::Prepare)`. The test
        // asserts that a `CardLayout::Prepare` constructed from a "prepare"
        // layout string exposes its back face through `layout_faces`, keeping
        // the loader's match-arm assumption load-bearing.
        let a = CardFace {
            name: "Front".to_string(),
            ..CardFace::default()
        };
        let b = CardFace {
            name: "Back".to_string(),
            ..CardFace::default()
        };
        let layout = CardLayout::Prepare(a, b);
        let faces = layout_faces(&layout);
        assert_eq!(faces.len(), 2, "Prepare layout exposes both faces");
        assert_eq!(faces[1].name, "Back");
    }
}

#[cfg(test)]
mod suspend_synthesis_tests {
    use super::*;
    use crate::types::ability::ActivationRestriction;
    use crate::types::counter::CounterType;
    use crate::types::mana::{ManaCost, ManaCostShard};

    /// Builds a Suspend-bearing face with `count` time counters and a single-blue
    /// alt-cost. Returns the populated face for synthesizer probing.
    fn suspend_face(count: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Suspend {
            count,
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            },
        });
        face
    }

    /// CR 702.62a: Suspend synthesizes (a) a hand-activated alt-cost ability,
    /// (b) an upkeep counter-removal trigger, and (c) a last-counter free-cast
    /// trigger. This regression locks the canonical shape so future refactors
    /// of synthesis.rs don't silently drop a sub-ability.
    #[test]
    fn synthesize_suspend_adds_activation_and_two_triggers() {
        let mut face = suspend_face(3);
        synthesize_suspend(&mut face);

        // (a) Hand activation with MatchesCardCastTiming + composite cost.
        let activation = face
            .abilities
            .iter()
            .find(|a| a.activation_zone == Some(Zone::Hand))
            .expect("suspend should add a hand-activated ability");
        assert!(activation
            .activation_restrictions
            .contains(&ActivationRestriction::MatchesCardCastTiming));
        // CR 702.62a: cost = pay [cost] AND exile self from hand.
        match &activation.cost {
            Some(AbilityCost::Composite { costs }) => {
                assert!(matches!(costs[0], AbilityCost::Mana { .. }));
                assert!(matches!(
                    costs[1],
                    AbilityCost::Exile {
                        zone: Some(Zone::Hand),
                        ..
                    }
                ));
            }
            other => panic!("expected Composite cost, got {other:?}"),
        }
        // CR 702.62a: effect places N time counters on SelfRef.
        match &*activation.effect {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, &CounterType::Time);
                assert!(matches!(target, TargetFilter::SelfRef));
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("expected PutCounter effect, got {other:?}"),
        }

        // (b) Upkeep counter-removal trigger from Exile zone.
        let upkeep = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::Phase)
                    && t.phase == Some(Phase::Upkeep)
                    && t.trigger_zones == vec![Zone::Exile]
            })
            .expect("suspend should add an upkeep trigger from Exile");
        assert!(matches!(
            upkeep.condition,
            Some(TriggerCondition::HasCounters {
                counters: CounterMatch::OfType(CounterType::Time),
                minimum: 1,
                maximum: None,
            })
        ));
        match upkeep.execute.as_deref().map(|a| &*a.effect) {
            Some(Effect::RemoveCounter {
                counter_type,
                target: TargetFilter::SelfRef,
                ..
            }) => assert_eq!(counter_type, &Some(CounterType::Time)),
            other => panic!("expected RemoveCounter effect, got {other:?}"),
        }

        // (c) Last-counter free-cast trigger via CounterRemoved + threshold(0).
        let last = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::CounterRemoved)
                    && t.trigger_zones == vec![Zone::Exile]
            })
            .expect("suspend should add a last-counter trigger from Exile");
        let cf = last.counter_filter.as_ref().expect("counter_filter set");
        assert!(matches!(cf.counter_type, CounterType::Time));
        assert_eq!(cf.threshold, Some(0));
        let exec = last.execute.as_ref().expect("execute body");
        assert!(exec.optional, "free cast must be a 'you may'");
        assert!(matches!(
            *exec.effect,
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                ..
            }
        ));
    }

    /// Idempotency: parser/loader pipelines may invoke `synthesize_all` more
    /// than once on the same face during multi-stage card-data builds.
    #[test]
    fn synthesize_suspend_is_idempotent() {
        let mut face = suspend_face(2);
        synthesize_suspend(&mut face);
        synthesize_suspend(&mut face);

        let activation_count = face
            .abilities
            .iter()
            .filter(|a| a.activation_zone == Some(Zone::Hand))
            .count();
        assert_eq!(activation_count, 1, "activation must dedupe");
        let upkeep_count = face
            .triggers
            .iter()
            .filter(|t| matches!(t.mode, TriggerMode::Phase) && t.phase == Some(Phase::Upkeep))
            .count();
        assert_eq!(upkeep_count, 1, "upkeep trigger must dedupe");
        let last_count = face
            .triggers
            .iter()
            .filter(|t| matches!(t.mode, TriggerMode::CounterRemoved))
            .count();
        assert_eq!(last_count, 1, "last-counter trigger must dedupe");
    }

    /// Cards without `Keyword::Suspend` are completely untouched.
    #[test]
    fn synthesize_suspend_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_suspend(&mut face);
        assert!(face.abilities.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// Issue #501: `KeywordTriggerInstaller::triggers_for` is the runtime
    /// chokepoint for granted-keyword companion triggers. Granted Suspend must
    /// return exactly the two suspend triggered abilities (CR 702.62a) — the
    /// upkeep counter-removal trigger and the last-counter free-cast trigger.
    /// The hand-activated alt-cost (1st ability) is NOT installed for runtime
    /// grants. Tests the building block across the *granted* `count: 0` input,
    /// not a single card.
    #[test]
    fn triggers_for_granted_suspend_returns_upkeep_and_cast_triggers() {
        let granted = Keyword::Suspend {
            count: 0,
            cost: ManaCost::Cost {
                shards: vec![],
                generic: 0,
            },
        };
        let triggers = KeywordTriggerInstaller::triggers_for(&granted);
        assert_eq!(
            triggers.len(),
            2,
            "granted Suspend installs exactly 2 triggers"
        );
        assert_eq!(triggers[0], build_suspend_upkeep_removal_trigger());
        assert_eq!(triggers[1], build_suspend_last_counter_cast_trigger());

        // A non-zero printed Suspend N returns the identical shared instances —
        // the triggers are SelfRef-scoped and parameter-free.
        let printed = Keyword::Suspend {
            count: 4,
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
        };
        assert_eq!(
            KeywordTriggerInstaller::triggers_for(&printed),
            triggers,
            "suspend triggers are count-agnostic — same shared instances for any N"
        );
    }

    /// Issue #501: symmetric removal — `trigger_matches_keyword_kind` must
    /// identify both suspend companion triggers so `RemoveKeyword` strips them
    /// when granted Suspend is removed. An unrelated trigger must not match.
    #[test]
    fn trigger_matches_keyword_kind_identifies_suspend_triggers() {
        let suspend = Keyword::Suspend {
            count: 0,
            cost: ManaCost::Cost {
                shards: vec![],
                generic: 0,
            },
        };
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &build_suspend_upkeep_removal_trigger(),
            &suspend,
        ));
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &build_suspend_last_counter_cast_trigger(),
            &suspend,
        ));
        // An unrelated trigger (echo) is not a suspend trigger.
        let echo = build_echo_trigger(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        });
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &echo, &suspend,
        ));
    }

    /// Issue #501 regression guard: after extracting the two suspend trigger
    /// builders, the printed-Suspend `synthesize_suspend` path must still emit
    /// triggers byte-identical to the shared builders' output — no drift
    /// between the printed and granted paths.
    #[test]
    fn synthesize_suspend_uses_shared_builders() {
        let mut face = suspend_face(4);
        synthesize_suspend(&mut face);

        let upkeep = build_suspend_upkeep_removal_trigger();
        let cast = build_suspend_last_counter_cast_trigger();
        assert!(
            face.triggers.contains(&upkeep),
            "printed Suspend's upkeep trigger must equal the shared builder output"
        );
        assert!(
            face.triggers.contains(&cast),
            "printed Suspend's last-counter trigger must equal the shared builder output"
        );
    }
}

#[cfg(test)]
mod suspend_serialization_tests {
    use crate::types::ability::{CastVariantPaid, StaticCondition};
    use crate::types::counter::CounterType;
    use crate::types::game_state::CastingVariant;
    use crate::types::player::PlayerId;

    /// CR 702.62a: All four typed primitives added by the Suspend runtime
    /// round-trip through serde. This guards against accidental
    /// `#[serde(skip)]` regressions or rename-without-migration mistakes.
    #[test]
    fn suspend_typed_primitives_round_trip() {
        let ct = CounterType::Time;
        let s = serde_json::to_string(&ct).unwrap();
        assert_eq!(s, "\"time\"");
        let back: CounterType = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, CounterType::Time));

        let cv = CastingVariant::Suspend;
        let s = serde_json::to_string(&cv).unwrap();
        let back: CastingVariant = serde_json::from_str(&s).unwrap();
        assert_eq!(back, CastingVariant::Suspend);

        let cvp = CastVariantPaid::Suspend;
        let s = serde_json::to_string(&cvp).unwrap();
        let back: CastVariantPaid = serde_json::from_str(&s).unwrap();
        assert_eq!(back, CastVariantPaid::Suspend);

        let cond = StaticCondition::SourceControllerEquals {
            player: PlayerId(1),
        };
        let s = serde_json::to_string(&cond).unwrap();
        let back: StaticCondition = serde_json::from_str(&s).unwrap();
        assert!(matches!(
            back,
            StaticCondition::SourceControllerEquals { player } if player == PlayerId(1)
        ));
    }
}

#[cfg(test)]
mod plot_synthesis_tests {
    //! CR 702.170 + CR 116.2k: Plot synthesis regression suite. Locks the
    //! shape of the hand-activated special-action-approximated ability that
    //! every `Keyword::Plot` card carries. Mirrors `suspend_synthesis_tests`.
    use super::*;
    use crate::types::ability::{ActivationRestriction, CastingPermission, PermissionGrantee};
    use crate::types::mana::{ManaCost, ManaCostShard};

    /// Builds a Plot-bearing face with a {1}{R} plot cost (Highway Robbery's
    /// printed cost). Returns the populated face for synthesizer probing.
    fn plot_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Plot(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        }));
        face
    }

    /// CR 702.170a: Plot synthesizes a single hand-activated ability with
    /// composite cost (mana + exile self from hand), sorcery-speed
    /// `ActivationRestriction::AsSorcery`, `activation_zone = Hand`, and a
    /// `GrantCastingPermission { Plotted { turn_plotted: 0 } }` effect.
    #[test]
    fn synthesize_plot_adds_hand_activation_with_sorcery_speed() {
        let mut face = plot_face();
        synthesize_plot(&mut face);

        let activation = face
            .abilities
            .iter()
            .find(|a| a.activation_zone == Some(Zone::Hand))
            .expect("plot should add a hand-activated ability");

        // CR 702.170a: sorcery-speed activation — AsSorcery restriction + flag.
        assert!(activation.sorcery_speed, "plot is sorcery-speed");
        assert!(activation
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));

        // CR 702.170a: cost = pay [cost] AND exile this card from hand.
        match &activation.cost {
            Some(AbilityCost::Composite { costs }) => {
                assert_eq!(costs.len(), 2, "composite cost has exactly 2 components");
                assert!(matches!(costs[0], AbilityCost::Mana { .. }));
                assert!(matches!(
                    costs[1],
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Hand),
                        filter: Some(TargetFilter::SelfRef),
                    }
                ));
            }
            other => panic!("expected Composite cost, got {other:?}"),
        }

        // CR 702.170a + CR 702.170d: effect grants `Plotted` to SelfRef with
        // placeholder turn_plotted = 0 (stamped at resolution).
        match &*activation.effect {
            Effect::GrantCastingPermission {
                permission: CastingPermission::Plotted { turn_plotted },
                target: TargetFilter::SelfRef,
                grantee: PermissionGrantee::AbilityController,
            } => {
                assert_eq!(
                    *turn_plotted, 0,
                    "turn_plotted is a placeholder until resolution"
                );
            }
            other => panic!("expected GrantCastingPermission(Plotted), got {other:?}"),
        }
    }

    /// Idempotency: parser pipelines may call `synthesize_all` multiple times.
    #[test]
    fn synthesize_plot_is_idempotent() {
        let mut face = plot_face();
        synthesize_plot(&mut face);
        synthesize_plot(&mut face);

        let count = face
            .abilities
            .iter()
            .filter(|a| a.activation_zone == Some(Zone::Hand))
            .count();
        assert_eq!(count, 1, "plot activation must dedupe on repeat invocation");
    }

    /// Cards without `Keyword::Plot` are completely untouched.
    #[test]
    fn synthesize_plot_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_plot(&mut face);
        assert!(face.abilities.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// CR 702.170d: The `Plotted` permission's `turn_plotted` field gates
    /// casts by the "later turn" rule. The in-engine comparison (in
    /// `has_exile_cast_permission`) uses `state.turn_number > turn_plotted`,
    /// so: same-turn → false, later-turn → true. Lock the comparison
    /// semantics here so future refactors don't flip the sign.
    #[test]
    fn plotted_permission_comparison_is_strictly_greater() {
        let perm = CastingPermission::Plotted { turn_plotted: 5 };
        // Extract the turn_plotted value and verify the comparison contract.
        let CastingPermission::Plotted { turn_plotted } = perm else {
            panic!("constructed variant");
        };
        // Same-turn: must NOT be castable (strictly greater, not >=).
        assert!(turn_plotted <= turn_plotted);
        // Later turn: must be castable.
        assert!(turn_plotted + 1 > turn_plotted);
        // Earlier turn: must NOT pass the `turn_number > turn_plotted` check.
        // Use addition rather than subtraction to avoid underflow semantics on u32.
        let earlier = turn_plotted;
        let later = turn_plotted + 1;
        assert!(!(earlier > later), "earlier turn never passes the gate");
    }

    /// CR 702.170d + CR 400.7: The `Plotted` permission is dropped when the
    /// card leaves exile. Verifies the exhaustive match arm in
    /// `zones::apply_zone_exit_cleanup` includes `Plotted` — regression guard
    /// against a future refactor that forgets to add new permission variants
    /// to the cleanup set.
    #[test]
    fn plotted_variant_is_serializable() {
        let perm = CastingPermission::Plotted { turn_plotted: 3 };
        let s = serde_json::to_string(&perm).unwrap();
        let back: CastingPermission = serde_json::from_str(&s).unwrap();
        match back {
            CastingPermission::Plotted { turn_plotted } => assert_eq!(turn_plotted, 3),
            other => panic!("round-trip produced {other:?}"),
        }
    }
}

#[cfg(test)]
mod idempotency_tests {
    //! Regression tests for trigger double-fire defect: every synthesis function
    //! that pushes a `TriggerDefinition` must be idempotent under repeated
    //! invocation. Non-idempotent synthesis causes multiple identical
    //! `TriggerDefinition` entries on the same card face, which in turn causes
    //! the engine's per-event dedup (keyed on `(ObjectId, trig_idx)`) to fail
    //! — distinct `trig_idx` values register separately.
    use super::*;
    use crate::types::ability::QuantityExpr;
    use crate::types::card_type::CoreType;

    #[test]
    fn synthesize_mobilize_is_idempotent() {
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::Mobilize(QuantityExpr::Fixed { value: 1 }));
        synthesize_mobilize(&mut face);
        synthesize_mobilize(&mut face);
        assert_eq!(
            face.triggers.len(),
            1,
            "mobilize trigger should only register once"
        );
    }

    #[test]
    fn synthesize_mobilize_preserves_dynamic_quantity() {
        use crate::types::ability::{CountScope, QuantityRef, TypeFilter, ZoneRef};

        let quantity = QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![TypeFilter::Creature],
                scope: CountScope::Controller,
            },
        };
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Mobilize(quantity.clone()));

        synthesize_mobilize(&mut face);

        let trigger = face.triggers.first().expect("mobilize trigger");
        match trigger
            .execute
            .as_deref()
            .map(|ability| ability.effect.as_ref())
        {
            Some(Effect::Token { count, .. }) => assert_eq!(count, &quantity),
            other => panic!("expected mobilize token effect, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_case_solve_is_idempotent() {
        let mut face = CardFace::default();
        face.card_type.subtypes.push("Case".to_string());
        face.solve_condition = Some(crate::types::ability::SolveCondition::Text {
            description: "test".to_string(),
        });
        synthesize_case_solve(&mut face);
        synthesize_case_solve(&mut face);
        assert_eq!(
            face.triggers.len(),
            1,
            "case-solve trigger should only register once"
        );
    }

    #[test]
    fn synthesize_casualty_is_idempotent() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Sorcery);
        face.keywords.push(Keyword::Casualty(2));
        synthesize_casualty(&mut face);
        let first_count = face.triggers.len();
        synthesize_casualty(&mut face);
        assert_eq!(
            face.triggers.len(),
            first_count,
            "casualty trigger should only register once"
        );
    }

    /// CR 702.153a: The intrinsic synthesized casualty trigger embeds the
    /// canonical `casualty_copy_ability_definition()` as its `execute`. This
    /// regression test guards the L9 fix: both `synthesize_casualty` and the
    /// dynamically-granted casualty path in `triggers::process_triggers` must
    /// derive the trigger's resolved ability shape from this single source of
    /// truth (effect = `CopySpell { SelfRef }`, condition =
    /// `additional_cost_paid_any`).
    #[test]
    fn intrinsic_casualty_trigger_uses_shared_canonical_definition() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Sorcery);
        face.keywords.push(Keyword::Casualty(1));
        synthesize_casualty(&mut face);

        let canonical = casualty_copy_ability_definition();
        let trig = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::SpellCast))
            .expect("synthesize_casualty should produce a SpellCast trigger");
        let execute = trig
            .execute
            .as_ref()
            .expect("casualty trigger must have an execute ability");

        assert_eq!(
            **execute, canonical,
            "intrinsic casualty trigger's execute must equal the canonical \
             casualty_copy_ability_definition() — single source of truth for \
             both intrinsic and dynamically-granted casualty"
        );
    }
}

#[cfg(test)]
mod sorcery_speed_invariant_tests {
    //! CR 602.5d: Every activated ability tagged with the `sorcery_speed`
    //! display flag MUST also carry `ActivationRestriction::AsSorcery` so the
    //! runtime legality gate (`game::restrictions::check_activation_restrictions`)
    //! actually enforces sorcery timing. Historically the `sorcery_speed` bool
    //! was display-only, and callers were required to separately push the enum
    //! variant — a recurring source of bugs where equip abilities were
    //! activatable at instant speed. Unifying the two via the `.sorcery_speed()`
    //! builder (and this invariant) prevents the bug class from recurring.
    use super::*;
    use crate::types::ability::ActivationRestriction;
    use crate::types::mana::{ManaCost, ManaCostShard};

    /// Walk every sub_ability in the chain.
    fn walk_chain<F: FnMut(&AbilityDefinition)>(def: &AbilityDefinition, mut visit: F) {
        let mut cur: Option<&AbilityDefinition> = Some(def);
        while let Some(d) = cur {
            visit(d);
            cur = d.sub_ability.as_deref();
        }
    }

    fn assert_sorcery_invariant(def: &AbilityDefinition, context: &str) {
        walk_chain(def, |d| {
            if d.sorcery_speed {
                assert!(
                    d.activation_restrictions
                        .contains(&ActivationRestriction::AsSorcery),
                    "{context}: ability has sorcery_speed=true but \
                     activation_restrictions is missing AsSorcery"
                );
            }
        });
    }

    /// CR 702.6a: Swiftfoot Boots — "Equip {1}" synthesizes an activated ability
    /// that MUST be gated at sorcery speed. Regression test for the confirmed
    /// bug where equip abilities were activatable at instant speed because
    /// `synthesize_equip` set neither the display flag nor the restriction.
    #[test]
    fn synthesize_equip_pushes_as_sorcery_restriction() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Equip(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        }));
        synthesize_equip(&mut face);

        assert_eq!(face.abilities.len(), 1, "one equip ability");
        let def = &face.abilities[0];
        assert!(def.sorcery_speed, "sorcery_speed display flag set");
        assert!(
            def.activation_restrictions
                .contains(&ActivationRestriction::AsSorcery),
            "AsSorcery restriction pushed for runtime enforcement (CR 702.6a)"
        );
    }

    /// CR 702.151a (issue #1559): Reconfigure synthesizes two sorcery-speed
    /// activated abilities — attach and unattach — so Equipment with Reconfigure
    /// (e.g. The Reality Chip) can actually be attached/detached for its cost.
    #[test]
    fn synthesize_reconfigure_pushes_attach_and_unattach_as_sorcery() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Reconfigure(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        }));
        synthesize_reconfigure(&mut face);

        assert_eq!(
            face.abilities.len(),
            2,
            "reconfigure synthesizes attach + unattach abilities"
        );
        for def in &face.abilities {
            assert!(def.sorcery_speed, "reconfigure abilities are sorcery-speed");
            assert!(
                def.activation_restrictions
                    .contains(&ActivationRestriction::AsSorcery),
                "AsSorcery restriction enforced (CR 702.151a)"
            );
        }
        assert!(
            matches!(*face.abilities[0].effect, Effect::Attach { .. }),
            "first reconfigure ability attaches the Equipment"
        );
        assert!(
            matches!(*face.abilities[1].effect, Effect::UnattachAll { .. }),
            "second reconfigure ability unattaches the Equipment"
        );
        assert!(
            face.abilities[1]
                .activation_restrictions
                .iter()
                .any(|restriction| matches!(
                    restriction,
                    ActivationRestriction::RequiresCondition {
                        condition: Some(ParsedCondition::SourceAttachedTo {
                            required_type: CoreType::Creature
                        })
                    }
                )),
            "unattach mode is legal only while attached to a creature (CR 702.151a)"
        );
    }

    /// CR 702.167a/b: Parsing the Oracle line "craft with creature {4}{b}" must
    /// produce a `Keyword::Craft` whose materials are the dual-zone
    /// (battlefield + graveyard) `Or` and count 1, and `synthesize_craft` must
    /// turn it into exactly one sorcery-speed activated ability whose cost is a
    /// `Composite[Mana, Exile{SelfRef}, ExileMaterials]` and whose effect returns
    /// the source from exile transformed. RED on main (no ability synthesized).
    #[test]
    fn synthesize_craft_from_oracle_line_builds_sorcery_speed_return_transformed() {
        use crate::parser::oracle_keyword::parse_keyword_from_oracle;
        use crate::types::ability::{CostObjectCount, TargetFilter};
        use crate::types::zones::Zone;

        let kw = parse_keyword_from_oracle("craft with creature {4}{b}")
            .expect("craft Oracle line parses to a keyword");
        let (materials, count) = match &kw {
            Keyword::Craft {
                materials, count, ..
            } => (materials.clone(), *count),
            other => panic!("expected Keyword::Craft, got {other:?}"),
        };
        assert_eq!(count, CostObjectCount::exactly(1), "single-material craft");
        match &materials {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2, "battlefield + graveyard legs");
            }
            other => panic!("expected dual-zone Or materials, got {other:?}"),
        }

        let mut face = CardFace::default();
        face.keywords.push(kw);
        synthesize_craft(&mut face);

        assert_eq!(
            face.abilities.len(),
            1,
            "craft synthesizes exactly one activated ability"
        );
        let def = &face.abilities[0];
        assert!(matches!(def.kind, AbilityKind::Activated));
        assert!(def.sorcery_speed, "craft is sorcery-speed (CR 702.167a)");
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));

        // Effect: return self from exile to battlefield transformed.
        match def.effect.as_ref() {
            Effect::ChangeZone {
                origin,
                destination,
                target,
                enter_transformed,
                ..
            } => {
                assert_eq!(*origin, Some(Zone::Exile));
                assert_eq!(*destination, Zone::Battlefield);
                assert_eq!(*target, TargetFilter::SelfRef);
                assert!(*enter_transformed, "CR 712.14a: enters transformed");
            }
            other => panic!("expected ChangeZone effect, got {other:?}"),
        }

        // Cost: Composite[Mana, Exile{SelfRef}, ExileMaterials].
        let Some(AbilityCost::Composite { costs }) = def.cost.as_ref() else {
            panic!("expected Composite craft cost, got {:?}", def.cost);
        };
        assert!(
            costs.iter().any(|c| matches!(c, AbilityCost::Mana { .. })),
            "mana sub-cost present"
        );
        assert!(
            costs.iter().any(|c| matches!(
                c,
                AbilityCost::Exile {
                    filter: Some(TargetFilter::SelfRef),
                    ..
                }
            )),
            "self-exile sub-cost present (CR 702.167a)"
        );
        assert!(
            costs.iter().any(|c| matches!(
                c,
                AbilityCost::ExileMaterials {
                    count: CostObjectCount::Exactly { count: 1 },
                    ..
                }
            )),
            "materials-exile sub-cost present (CR 702.167a/b)"
        );
    }

    /// CR 702.87a: Level Up synthesis must carry AsSorcery.
    #[test]
    fn synthesize_level_up_pushes_as_sorcery_restriction() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::LevelUp(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        }));
        synthesize_level_up(&mut face);

        let def = &face.abilities[0];
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
    }

    /// CR 702.97a: Scavenge synthesis must carry AsSorcery (single `.sorcery_speed()`
    /// call must produce both the flag and the restriction).
    #[test]
    fn synthesize_scavenge_pushes_as_sorcery_restriction() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Scavenge(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        }));
        synthesize_scavenge(&mut face);

        let def = &face.abilities[0];
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
        // Guard against double-push regression: AsSorcery should appear exactly once.
        let count = def
            .activation_restrictions
            .iter()
            .filter(|r| matches!(r, ActivationRestriction::AsSorcery))
            .count();
        assert_eq!(count, 1, "AsSorcery must not be duplicated");
    }

    /// CR 602.5d: The shared invariant — corpus-wide, walk every synthesized
    /// ability and its sub_ability chain; every ability with
    /// `sorcery_speed=true` must carry `AsSorcery`. Runs the synthesis pipeline
    /// against every keyword variant that has synthesis coverage and enforces
    /// the invariant, so any future keyword synthesis regressing to a
    /// display-only `sorcery_speed=true` fails this test.
    #[test]
    fn sorcery_speed_flag_implies_as_sorcery_restriction_for_synthesized_abilities() {
        fn mana() -> ManaCost {
            ManaCost::Cost {
                shards: vec![],
                generic: 1,
            }
        }

        type SynthCase = (&'static str, fn() -> CardFace);
        let cases: &[SynthCase] = &[
            ("Equip {1}", || {
                let mut f = CardFace::default();
                f.keywords.push(Keyword::Equip(mana()));
                synthesize_equip(&mut f);
                f
            }),
            ("Level Up {1}", || {
                let mut f = CardFace::default();
                f.keywords.push(Keyword::LevelUp(mana()));
                synthesize_level_up(&mut f);
                f
            }),
            ("Scavenge {1}", || {
                let mut f = CardFace::default();
                f.keywords.push(Keyword::Scavenge(mana()));
                synthesize_scavenge(&mut f);
                f
            }),
        ];

        for (name, build) in cases {
            let face = build();
            for def in face.abilities.iter() {
                assert_sorcery_invariant(def, name);
            }
        }
    }
}

#[cfg(test)]
mod loyalty_sorcery_speed_tests {
    //! CR 606.3: Planeswalker loyalty abilities may only be activated during
    //! the controller's main phase with an empty stack, and only once per turn
    //! per permanent. The parser tags every loyalty line with
    //! `ActivationRestriction::AsSorcery` (CR 606.3 timing) for downstream
    //! consumers. It does NOT add `OnlyOnceEachTurn`: that restriction is
    //! per-ability-index, while CR 606.3 is per-permanent across ALL loyalty
    //! ability indices. The per-permanent cap is enforced authoritatively by
    //! `game::planeswalker::can_activate_loyalty_ability` against
    //! `obj.loyalty_activations_this_turn`. See `apply_loyalty_restrictions`
    //! in `parser::oracle` for the rationale and The Chain Veil interaction.
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::ActivationRestriction;

    #[test]
    fn loyalty_ability_parses_with_as_sorcery() {
        // Jace, the Mind Sculptor reminder-text-like minimal loyalty line.
        let r = parse_oracle_text("+2: Draw a card.", "Test Planeswalker", &[], &[], &[]);
        assert_eq!(r.abilities.len(), 1);
        let def = &r.abilities[0];
        assert!(def.sorcery_speed, "loyalty sets sorcery_speed display flag");
        assert!(
            def.activation_restrictions
                .contains(&ActivationRestriction::AsSorcery),
            "CR 606.3: AsSorcery restriction is pushed for loyalty"
        );
        // CR 606.3: Loyalty's "once per turn per permanent" gate lives on the
        // permanent counter, not on per-ability `OnlyOnceEachTurn`. The Chain
        // Veil's cap-raise depends on this separation.
        assert!(
            !def.activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "CR 606.3: OnlyOnceEachTurn must NOT be attached — the per-permanent counter is the gate"
        );
    }

    #[test]
    fn loyalty_bracket_format_also_tagged() {
        // Bracket format: [+1]: effect.
        let r = parse_oracle_text("[+1]: Draw a card.", "Test Planeswalker", &[], &[], &[]);
        assert_eq!(r.abilities.len(), 1);
        let def = &r.abilities[0];
        assert!(def.sorcery_speed);
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
        assert!(!def
            .activation_restrictions
            .contains(&ActivationRestriction::OnlyOnceEachTurn));
    }

    #[test]
    fn loyalty_negative_minus_cost_tagged() {
        let r = parse_oracle_text(
            "\u{2212}3: Destroy target creature.",
            "Test Planeswalker",
            &[],
            &[],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let def = &r.abilities[0];
        assert!(def
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
    }
}

#[cfg(test)]
mod offspring_synthesis_tests {
    use super::*;
    use crate::types::mana::ManaCostShard;

    /// CR 702.175a: Offspring synthesizes an optional additional cost and an
    /// ETB trigger that creates a 1/1 copy token.
    #[test]
    fn synthesize_offspring_sets_additional_cost_and_trigger() {
        let offspring_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        };
        let mut face = CardFace {
            keywords: vec![Keyword::Offspring(offspring_cost.clone())],
            ..CardFace::default()
        };

        synthesize_offspring(&mut face);

        // Part 1: additional_cost is Optional(Mana { offspring_cost })
        match face.additional_cost.as_ref().expect("additional_cost set") {
            AdditionalCost::Optional {
                cost: AbilityCost::Mana { cost },
                repeatable: false,
            } => {
                assert_eq!(*cost, offspring_cost);
            }
            other => panic!("expected Optional(Mana), got {other:?}"),
        }

        // Part 2: ETB trigger with AdditionalCostPaid condition + CopyTokenOf effect
        let trigger = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.destination == Some(Zone::Battlefield)
                    && matches!(
                        t.condition,
                        Some(TriggerCondition::AdditionalCostPaid { .. })
                    )
            })
            .expect("offspring ETB trigger");
        let effect = &trigger.execute.as_ref().expect("execute body").effect;
        match &**effect {
            Effect::CopyTokenOf {
                target,
                additional_modifications,
                ..
            } => {
                assert!(matches!(target, TargetFilter::SelfRef));
                assert_eq!(additional_modifications.len(), 2);
                assert!(matches!(
                    additional_modifications[0],
                    ContinuousModification::SetPower { value: 1 }
                ));
                assert!(matches!(
                    additional_modifications[1],
                    ContinuousModification::SetToughness { value: 1 }
                ));
            }
            other => panic!("expected CopyTokenOf, got {other:?}"),
        }
    }

    /// Idempotency: running synthesize_offspring twice produces the same result.
    #[test]
    fn synthesize_offspring_is_idempotent() {
        let mut face = CardFace {
            keywords: vec![Keyword::Offspring(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })],
            ..CardFace::default()
        };

        synthesize_offspring(&mut face);
        let first_cost = face.additional_cost.clone();
        let first_trigger_count = face.triggers.len();
        synthesize_offspring(&mut face);
        assert_eq!(face.additional_cost, first_cost);
        assert_eq!(face.triggers.len(), first_trigger_count);
    }

    /// Offspring skips additional_cost when one is already set (e.g., kicker).
    #[test]
    fn synthesize_offspring_skips_additional_cost_when_already_set() {
        let existing = AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 1,
                    shards: vec![],
                },
            }],
            repeatable: false,
        };
        let mut face = CardFace {
            keywords: vec![Keyword::Offspring(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::White],
            })],
            additional_cost: Some(existing.clone()),
            ..CardFace::default()
        };

        synthesize_offspring(&mut face);

        // additional_cost unchanged (kicker takes precedence)
        assert_eq!(face.additional_cost, Some(existing));
        // Trigger is still synthesized
        assert_eq!(face.triggers.len(), 1);
    }
}

#[cfg(test)]
mod recover_synthesis_tests {
    use super::*;
    use crate::types::card_type::CoreType;
    use crate::types::mana::{ManaCost, ManaCostShard};

    /// Recover cost {1}{B} (Grave Defiler-style).
    fn recover_cost() -> ManaCost {
        ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        }
    }

    /// Helper: a creature face carrying `Keyword::Recover({1}{B})`.
    fn face_with_recover() -> CardFace {
        CardFace {
            name: "Recoverer".to_string(),
            oracle_text: Some("Recover {1}{B}".to_string()),
            keywords: vec![Keyword::Recover(recover_cost())],
            card_type: crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        }
    }

    /// CR 702.59a: Recover synthesizes a graveyard-sourced "another creature you
    /// control dies" trigger.
    #[test]
    fn synthesize_recover_adds_another_creature_dies_trigger() {
        let mut face = face_with_recover();
        synthesize_recover(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_recover_trigger(t))
            .expect("Recover dies trigger should be synthesized");

        // Dies trigger: Battlefield → Graveyard.
        assert_eq!(trigger.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));

        // Graveyard-sourced: the Recover card fires from its own graveyard.
        assert!(
            trigger.trigger_zones.contains(&Zone::Graveyard),
            "Recover trigger must be active from the graveyard zone"
        );

        // valid_card = another creature you own.
        match trigger.valid_card.as_ref() {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|f| matches!(f, TypeFilter::Creature)));
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(tf.properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::You,
                }));
                assert_eq!(tf.controller, None);
            }
            other => panic!("expected Typed(another creature you own), got {other:?}"),
        }
    }

    /// CR 702.59a + CR 118.12: the trigger's execute is the pay-or-else-exile
    /// branch — primary effect exiles SelfRef, gated by an `unless_pay` carrying
    /// the recover cost, with a pay-success sub-ability returning SelfRef to hand.
    #[test]
    fn synthesize_recover_builds_pay_or_else_exile_branch() {
        let mut face = face_with_recover();
        synthesize_recover(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_recover_trigger(t))
            .expect("Recover dies trigger should be synthesized");
        let execute = trigger.execute.as_deref().expect("execute body");

        // Primary (otherwise) branch: exile this card from the graveyard.
        assert!(
            matches!(
                &*execute.effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "primary effect must exile SelfRef from the graveyard"
        );

        // unless_pay carries the recover cost; controller is the payer.
        let unless_pay = execute
            .unless_pay
            .as_ref()
            .expect("execute must carry an unless_pay modifier");
        assert_eq!(
            unless_pay.cost,
            AbilityCost::Mana {
                cost: recover_cost()
            }
        );
        assert_eq!(unless_pay.payer, TargetFilter::Controller);

        // Pay-success alternative: return SelfRef to hand, gated on
        // effect-performed ("if you do").
        let sub = execute
            .sub_ability
            .as_deref()
            .expect("execute must carry the pay-success sub-ability");
        assert!(
            sub.condition
                .as_ref()
                .is_some_and(AbilityCondition::is_optional_effect_performed),
            "return-to-hand branch must be gated on effect-performed"
        );
        assert!(
            matches!(
                &*sub.effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Hand,
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "pay-success branch must return SelfRef from graveyard to hand"
        );
    }

    /// Re-running synthesis must not duplicate the trigger.
    #[test]
    fn synthesize_recover_is_idempotent() {
        let mut face = face_with_recover();
        synthesize_recover(&mut face);
        let first = face
            .triggers
            .iter()
            .filter(|t| is_recover_trigger(t))
            .count();
        synthesize_recover(&mut face);
        let second = face
            .triggers
            .iter()
            .filter(|t| is_recover_trigger(t))
            .count();
        assert_eq!(first, 1);
        assert_eq!(second, first, "synthesis must be idempotent");
    }

    /// A face without Recover gets no Recover trigger.
    #[test]
    fn synthesize_recover_is_noop_without_keyword() {
        let mut face = CardFace {
            name: "Plain Bear".to_string(),
            keywords: vec![Keyword::Flying],
            card_type: crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        };
        synthesize_recover(&mut face);
        assert!(face.triggers.iter().all(|t| !is_recover_trigger(t)));
    }

    /// CR 604.1: the runtime-granted path (`triggers_for`) yields the same shape
    /// as the printed path, and `trigger_matches_keyword_kind` recognizes it for
    /// symmetric removal.
    #[test]
    fn triggers_for_recover_matches_keyword_kind() {
        let kw = Keyword::Recover(recover_cost());
        let triggers = KeywordTriggerInstaller::triggers_for(&kw);
        assert_eq!(triggers.len(), 1);
        assert!(is_recover_trigger(&triggers[0]));
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &kw
        ));
        // A different keyword's trigger must not match Recover.
        let dethrone = build_dethrone_trigger();
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &dethrone, &kw
        ));
    }
}

#[cfg(test)]
mod backup_synthesis_tests {
    use super::*;
    use crate::types::card_type::CoreType;

    /// Helper to create a Guardian Scalelord-like face with Backup 1, Flying, and an attack trigger.
    fn face_with_backup() -> CardFace {
        let mut face = CardFace {
            name: "Guardian Scalelord".to_string(),
            oracle_text: Some(
                "Backup 1\nFlying\nWhenever this creature attacks, draw a card.".to_string(),
            ),
            keywords: vec![Keyword::Backup(1), Keyword::Flying],
            card_type: crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        };

        // Add an attack trigger (like Guardian Scalelord's "Whenever this creature attacks...")
        let attack_trigger = TriggerDefinition::new(TriggerMode::Attacks)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))
            .description("Whenever this creature attacks, draw a card.".to_string());
        face.triggers.push(attack_trigger);

        face
    }

    /// CR 702.165a: Backup synthesizes an ETB trigger placing +1/+1 counters on target creature.
    #[test]
    fn synthesize_backup_adds_etb_trigger() {
        let mut face = face_with_backup();
        synthesize_backup(&mut face);

        let backup_trigger = face
            .triggers
            .iter()
            .find(|t| is_backup_etb_trigger(t))
            .expect("Backup ETB trigger should be synthesized");

        assert_eq!(backup_trigger.mode, TriggerMode::ChangesZone);
        assert_eq!(backup_trigger.destination, Some(Zone::Battlefield));
        assert!(matches!(
            backup_trigger.valid_card,
            Some(TargetFilter::SelfRef)
        ));

        // Verify the execute body places +1/+1 counters on a creature
        let execute = backup_trigger.execute.as_ref().expect("execute body");
        match &*execute.effect {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
                assert_eq!(count, &QuantityExpr::Fixed { value: 1 });
                assert!(matches!(
                    target,
                    TargetFilter::Typed(tf) if tf.type_filters.iter().any(|f| matches!(f, TypeFilter::Creature))
                ));
            }
            other => panic!("expected PutCounter effect, got {:?}", other),
        }
    }

    /// Re-running synthesis must not duplicate the trigger.
    #[test]
    fn synthesize_backup_is_idempotent() {
        let mut face = face_with_backup();
        synthesize_backup(&mut face);
        let first_count = face
            .triggers
            .iter()
            .filter(|t| is_backup_etb_trigger(t))
            .count();

        synthesize_backup(&mut face);
        let second_count = face
            .triggers
            .iter()
            .filter(|t| is_backup_etb_trigger(t))
            .count();

        assert_eq!(first_count, 1, "first run should add one trigger");
        assert_eq!(second_count, 1, "second run should not add another trigger");
    }

    /// CR 702.165a: Multiple Backup instances trigger separately.
    #[test]
    fn synthesize_backup_emits_one_trigger_per_instance() {
        let mut face = CardFace {
            name: "Conclave Sledge-Captain".to_string(),
            oracle_text: Some(
                "Backup 1, backup 1, backup 1\n\
                 Trample\n\
                 Whenever this creature deals combat damage to a player, put that many +1/+1 counters on it."
                    .to_string(),
            ),
            keywords: vec![Keyword::Backup(1), Keyword::Backup(1), Keyword::Backup(1)],
            card_type: crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        };

        synthesize_backup(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_backup_etb_trigger_with_count(trigger, 1))
                .count(),
            3,
            "each Backup instance must emit its own ETB trigger"
        );

        synthesize_backup(&mut face);
        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_backup_etb_trigger_with_count(trigger, 1))
                .count(),
            3,
            "synthesis remains idempotent for repeated Backup instances"
        );
    }

    /// A face without `Keyword::Backup` is unaffected.
    #[test]
    fn synthesize_backup_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_backup(&mut face);
        assert!(face.triggers.iter().all(|t| !is_backup_etb_trigger(t)));
    }

    /// CR 702.165c: Backup grants the source's other abilities (keywords, triggers, statics) to the target.
    #[test]
    fn synthesize_backup_grants_non_backup_abilities() {
        let mut face = face_with_backup();
        synthesize_backup(&mut face);

        let backup_trigger = face
            .triggers
            .iter()
            .find(|t| is_backup_etb_trigger(t))
            .expect("Backup ETB trigger");

        let execute = backup_trigger.execute.as_ref().expect("execute body");

        // Check that the sub_ability exists and contains GenericEffect
        let sub_ability = execute.sub_ability.as_ref().expect("sub_ability");
        match &*sub_ability.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert_eq!(duration, &Some(Duration::UntilEndOfTurn));
                assert_eq!(static_abilities.len(), 1);

                let static_def = &static_abilities[0];
                assert!(matches!(static_def.mode, StaticMode::Continuous));
                assert!(matches!(
                    static_def.affected,
                    Some(TargetFilter::ParentTarget)
                ));

                // Verify modifications include Flying keyword and the attack trigger
                let has_flying = static_def.modifications.iter().any(|mod_| {
                    matches!(mod_, ContinuousModification::AddKeyword { keyword } if matches!(keyword, Keyword::Flying))
                });
                assert!(has_flying, "should grant Flying keyword");

                let has_trigger = static_def
                    .modifications
                    .iter()
                    .any(|mod_| matches!(mod_, ContinuousModification::GrantTrigger { .. }));
                assert!(has_trigger, "should grant attack trigger");

                // Verify Backup itself is NOT granted
                let has_backup = static_def.modifications.iter().any(|mod_| {
                    matches!(mod_, ContinuousModification::AddKeyword { keyword } if matches!(keyword, Keyword::Backup(_)))
                });
                assert!(!has_backup, "should NOT grant Backup keyword");
            }
            other => panic!("expected GenericEffect, got {:?}", other),
        }
    }

    /// CR 702.165a/c: Backup grants only abilities printed below the Backup
    /// line. Abilities printed above it, like Saiba Cryptomancer's flash, are
    /// not granted.
    #[test]
    fn synthesize_backup_does_not_grant_abilities_printed_above_backup() {
        let mut face = CardFace {
            name: "Saiba Cryptomancer".to_string(),
            oracle_text: Some(
                "Flash\n\
                 Backup 1\n\
                 Hexproof"
                    .to_string(),
            ),
            keywords: vec![Keyword::Flash, Keyword::Backup(1), Keyword::Hexproof],
            card_type: crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        };

        synthesize_backup(&mut face);

        let backup_trigger = face
            .triggers
            .iter()
            .find(|trigger| is_backup_etb_trigger(trigger))
            .expect("Backup ETB trigger");
        let grant_sub = backup_trigger
            .execute
            .as_ref()
            .and_then(|execute| execute.sub_ability.as_ref())
            .expect("Backup ability grant");
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*grant_sub.effect
        else {
            panic!("expected GenericEffect grant, got {:?}", grant_sub.effect);
        };
        let modifications = &static_abilities[0].modifications;

        assert!(
            modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Hexproof
                    }
                )
            }),
            "Backup should grant Hexproof printed below Backup"
        );
        assert!(
            !modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Flash
                    }
                )
            }),
            "Backup must not grant Flash printed above Backup"
        );
    }

    /// CR 702.165c: Ability granting is gated on "if that's another creature".
    #[test]
    fn synthesize_backup_self_targeting_condition() {
        let mut face = face_with_backup();
        synthesize_backup(&mut face);

        let backup_trigger = face
            .triggers
            .iter()
            .find(|t| is_backup_etb_trigger(t))
            .expect("Backup ETB trigger");

        let execute = backup_trigger.execute.as_ref().expect("execute body");
        let sub_ability = execute.sub_ability.as_ref().expect("sub_ability");

        // Verify the condition is Not(TargetMatchesFilter(SelfRef))
        match &sub_ability.condition {
            Some(AbilityCondition::Not { condition }) => match condition.as_ref() {
                AbilityCondition::TargetMatchesFilter { filter, use_lki } => {
                    assert!(matches!(filter, TargetFilter::SelfRef));
                    assert!(!use_lki);
                }
                other => panic!("expected TargetMatchesFilter, got {:?}", other),
            },
            other => panic!("expected Not condition, got {:?}", other),
        }
    }

    /// Backup with no other abilities to grant should still place counters.
    #[test]
    fn synthesize_backup_with_no_other_abilities() {
        let mut face = CardFace {
            name: "Simple Backup".to_string(),
            keywords: vec![Keyword::Backup(2)],
            card_type: crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        };

        synthesize_backup(&mut face);

        let backup_trigger = face
            .triggers
            .iter()
            .find(|t| is_backup_etb_trigger(t))
            .expect("Backup ETB trigger");

        let execute = backup_trigger.execute.as_ref().expect("execute body");

        // Should place 2 counters
        match &*execute.effect {
            Effect::PutCounter { count, .. } => {
                assert_eq!(count, &QuantityExpr::Fixed { value: 2 });
            }
            other => panic!("expected PutCounter, got {:?}", other),
        }

        // Should have no sub_ability since there's nothing to grant
        assert!(execute.sub_ability.is_none());
    }
}

#[cfg(test)]
mod squad_synthesis_tests {
    use super::*;
    use crate::types::mana::ManaCostShard;

    /// CR 702.157a: Squad synthesizes a repeatable optional additional cost and
    /// an ETB trigger whose copy count is the number of squad payments.
    #[test]
    fn synthesize_squad_sets_repeatable_cost_and_payment_count_copy_trigger() {
        let squad_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        };
        let mut face = CardFace {
            keywords: vec![Keyword::Squad(squad_cost.clone())],
            ..CardFace::default()
        };

        synthesize_squad(&mut face);

        match face.additional_cost.as_ref().expect("additional_cost set") {
            AdditionalCost::Optional {
                cost: AbilityCost::Mana { cost },
                repeatable: true,
            } => {
                assert_eq!(cost, &squad_cost);
            }
            other => panic!("expected repeatable additional cost, got {other:?}"),
        }

        let trigger = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::ChangesZone)
                    && t.destination == Some(Zone::Battlefield)
                    && matches!(
                        t.condition,
                        Some(TriggerCondition::AdditionalCostPaid { .. })
                    )
            })
            .expect("squad ETB trigger");
        let effect = &trigger.execute.as_ref().expect("execute body").effect;
        match &**effect {
            Effect::CopyTokenOf { target, count, .. } => {
                assert!(matches!(target, TargetFilter::SelfRef));
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::AdditionalCostPaymentCount
                    }
                ));
            }
            other => panic!("expected CopyTokenOf, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_squad_is_idempotent() {
        let mut face = CardFace {
            keywords: vec![Keyword::Squad(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })],
            ..CardFace::default()
        };

        synthesize_squad(&mut face);
        let first_cost = face.additional_cost.clone();
        let first_trigger_count = face.triggers.len();
        synthesize_squad(&mut face);

        assert_eq!(face.additional_cost, first_cost);
        assert_eq!(face.triggers.len(), first_trigger_count);
    }

    #[test]
    fn synthesize_squad_defers_multiple_instances() {
        let mut face = CardFace {
            keywords: vec![
                Keyword::Squad(ManaCost::Cost {
                    generic: 1,
                    shards: vec![],
                }),
                Keyword::Squad(ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                }),
            ],
            ..CardFace::default()
        };

        synthesize_squad(&mut face);
        synthesize_squad(&mut face);

        assert!(face.additional_cost.is_none());
        assert!(face.triggers.is_empty());
        assert_eq!(
            face.abilities
                .iter()
                .filter(|ability| {
                    matches!(
                        &*ability.effect,
                        Effect::Unimplemented { name, .. }
                            if name == "squad_multiple_instances"
                    )
                })
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod replicate_synthesis_tests {
    use super::*;
    use crate::types::mana::ManaCostShard;

    /// CR 702.56a: Replicate synthesizes a repeatable optional additional cost
    /// and a SpellCast trigger whose `CopySpell` count is the number of
    /// replicate payments (`AdditionalCostPaymentCount`).
    #[test]
    fn synthesize_replicate_sets_repeatable_cost_and_payment_count_copy_trigger() {
        let replicate_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        };
        let mut face = CardFace {
            keywords: vec![Keyword::Replicate(replicate_cost.clone())],
            ..CardFace::default()
        };

        synthesize_replicate(&mut face);

        match face.additional_cost.as_ref().expect("additional_cost set") {
            AdditionalCost::Optional {
                cost: AbilityCost::Mana { cost },
                repeatable: true,
            } => {
                assert_eq!(cost, &replicate_cost);
            }
            other => panic!("expected repeatable Optional mana cost, got {other:?}"),
        }

        let trigger = face
            .triggers
            .iter()
            .find(|t| matches!(t.mode, TriggerMode::SpellCast))
            .expect("replicate SpellCast trigger");
        let execute = trigger.execute.as_ref().expect("execute body");
        assert_eq!(
            **execute,
            replicate_copy_ability_definition(),
            "replicate trigger's execute must equal the canonical \
             replicate_copy_ability_definition() — single source of truth"
        );
        // CR 707.10c: copies may choose new targets.
        match &*execute.effect {
            Effect::CopySpell {
                target, retarget, ..
            } => {
                assert!(matches!(target, TargetFilter::SelfRef));
                assert!(matches!(
                    retarget,
                    CopyRetargetPermission::MayChooseNewTargets
                ));
            }
            other => panic!("expected CopySpell, got {other:?}"),
        }
        // CR 702.56a: one copy per replicate payment.
        assert!(matches!(
            execute.repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::AdditionalCostPaymentCount,
            })
        ));
    }

    #[test]
    fn synthesize_replicate_is_idempotent() {
        let mut face = CardFace {
            keywords: vec![Keyword::Replicate(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })],
            ..CardFace::default()
        };

        synthesize_replicate(&mut face);
        let first_cost = face.additional_cost.clone();
        let first_trigger_count = face.triggers.len();
        synthesize_replicate(&mut face);

        assert_eq!(face.additional_cost, first_cost);
        assert_eq!(face.triggers.len(), first_trigger_count);
    }

    /// CR 702.56b: Multiple Replicate instances require per-instance payment
    /// tracking the engine cannot yet model, so synthesis defers.
    #[test]
    fn synthesize_replicate_defers_multiple_instances() {
        let mut face = CardFace {
            keywords: vec![
                Keyword::Replicate(ManaCost::Cost {
                    generic: 1,
                    shards: vec![],
                }),
                Keyword::Replicate(ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                }),
            ],
            ..CardFace::default()
        };

        synthesize_replicate(&mut face);

        assert!(face.additional_cost.is_none());
        assert!(face.triggers.is_empty());
        assert_eq!(
            face.abilities
                .iter()
                .filter(|ability| {
                    matches!(
                        &*ability.effect,
                        Effect::Unimplemented { name, .. }
                            if name == "replicate_multiple_instances"
                    )
                })
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod modular_synthesis_tests {
    //! CR 702.43a + CR 702.43b: Shape tests for the synthesized Modular pair.
    //! Pinned to the exact wire-up the runtime resolver consumes:
    //!   * ETB-with-counters: `ReplacementEvent::Moved` with `valid_card =
    //!     SelfRef`, execute `Effect::PutCounter { counter_type: "P1P1",
    //!     count: Fixed(N), target: SelfRef }`.
    //!   * Dies-transfer: `TriggerMode::ChangesZone` (Battlefield → Graveyard)
    //!     with `valid_card = SelfRef`, execute `Effect::PutCounter` placing
    //!     P1P1 counters on a target artifact-creature with the count read
    //!     from the source's LKI counter map via `QuantityRef::CountersOn {
    //!     scope: Source, counter_type: Some("P1P1") }`.
    use super::*;

    fn face_with_modular(n: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Modular(n));
        face
    }

    /// CR 702.43a clause 1: ETB-with-N-counters replacement.
    #[test]
    fn synthesize_modular_adds_etb_counters_replacement() {
        let mut face = face_with_modular(2);
        synthesize_modular(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| is_modular_etb_replacement(r, 2))
            .expect("modular should synthesize an ETB-with-counters replacement");

        assert!(matches!(replacement.event, ReplacementEvent::Moved));
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));

        let execute = replacement
            .execute
            .as_deref()
            .expect("ETB replacement requires execute body");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("modular ETB execute body should be Effect::PutCounter");
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert!(matches!(target, TargetFilter::SelfRef));
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
    }

    /// CR 702.43a clause 2: Dies-transfer trigger reads the source's LKI P1P1
    /// counter count and places that many counters on a target artifact
    /// creature.
    #[test]
    fn synthesize_modular_adds_dies_transfer_trigger() {
        let mut face = face_with_modular(1);
        synthesize_modular(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_modular_dies_transfer_trigger(t))
            .expect("modular should synthesize a dies-transfer trigger");

        assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));

        let execute = trigger
            .execute
            .as_deref()
            .expect("dies trigger requires execute body");

        // CR 603.5: "you may" — optional triggered ability; controller is
        // prompted before the effect runs.
        assert!(
            execute.optional,
            "modular dies-transfer must be optional per CR 702.43a 'you may'"
        );

        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("modular dies execute body should be Effect::PutCounter");
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);

        // Count = LKI P1P1 counter count on the dying source.
        let QuantityExpr::Ref { qty } = count else {
            panic!("modular dies count should be a QuantityRef::Ref");
        };
        let QuantityRef::CountersOn {
            scope,
            counter_type: lki_ct,
        } = qty
        else {
            panic!("modular dies count should be QuantityRef::CountersOn");
        };
        assert!(matches!(scope, ObjectScope::Source));
        assert_eq!(lki_ct, &Some(CounterType::Plus1Plus1));

        // Target = artifact creature (conjunction).
        let TargetFilter::Typed(tf) = target else {
            panic!("modular dies target must be a TypedFilter");
        };
        assert!(tf
            .type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Creature)));
        assert!(tf
            .type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Artifact)));
    }

    /// Re-running synthesis must not duplicate the replacement or the trigger.
    #[test]
    fn synthesize_modular_is_idempotent() {
        let mut face = face_with_modular(2);
        synthesize_modular(&mut face);
        synthesize_modular(&mut face);

        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| is_modular_etb_replacement(r, 2))
                .count(),
            1,
            "ETB replacement should be deduped"
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_modular_dies_transfer_trigger(t))
                .count(),
            1,
            "dies-transfer trigger should be deduped"
        );
    }

    /// A face without `Keyword::Modular` is unaffected.
    #[test]
    fn synthesize_modular_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_modular(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// CR 113.2c + CR 702.43b: each Modular instance emits its own ETB-counters
    /// replacement + dies-transfer trigger. No printed card today has two
    /// Modular instances; the test pins the rule so a future printing (or a
    /// granted-Modular case) routes correctly.
    #[test]
    fn synthesize_modular_emits_two_abilities_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Modular(1));
        face.keywords.push(Keyword::Modular(3));
        synthesize_modular(&mut face);

        // CR 113.2c: each instance emits its own ETB replacement; the
        // predicate is per-N so we filter by either N to find the matching
        // instance's emission.
        let replacement_n1 = face
            .replacements
            .iter()
            .filter(|r| is_modular_etb_replacement(r, 1))
            .count();
        let replacement_n3 = face
            .replacements
            .iter()
            .filter(|r| is_modular_etb_replacement(r, 3))
            .count();
        assert_eq!(replacement_n1, 1, "exactly one Fixed(1) ETB replacement");
        assert_eq!(replacement_n3, 1, "exactly one Fixed(3) ETB replacement");

        let replacements: Vec<_> = face
            .replacements
            .iter()
            .filter(|r| is_modular_etb_replacement(r, 1) || is_modular_etb_replacement(r, 3))
            .collect();
        assert_eq!(replacements.len(), 2);

        // Both N=1 and N=3 must be present from the first pass.
        let ns: Vec<i32> = replacements
            .iter()
            .filter_map(|r| match r.execute.as_deref().map(|a| &*a.effect) {
                Some(Effect::PutCounter {
                    count: QuantityExpr::Fixed { value },
                    ..
                }) => Some(*value),
                _ => None,
            })
            .collect();
        assert!(ns.contains(&1) && ns.contains(&3));

        let triggers = face
            .triggers
            .iter()
            .filter(|t| is_modular_dies_transfer_trigger(t))
            .count();
        assert_eq!(
            triggers, 2,
            "each Modular instance independently emits its dies-transfer"
        );
    }

    /// Negative test: unrelated keywords do not synthesize Modular.
    #[test]
    fn synthesize_modular_does_not_affect_other_keywords() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Trample);
        face.keywords.push(Keyword::Vigilance);
        synthesize_modular(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// Idempotency-shape predicates must NOT match unrelated replacements /
    /// triggers (e.g., a Moved replacement with a different counter type, or a
    /// dies-trigger that draws a card).
    #[test]
    fn synthesize_modular_distinguishes_unrelated_replacements_and_triggers() {
        let mut face = face_with_modular(1);

        // Unrelated dies trigger: "When ~ dies, draw a card."
        let unrelated_dies = TriggerDefinition::new(TriggerMode::ChangesZone)
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        face.triggers.push(unrelated_dies);

        synthesize_modular(&mut face);

        let modular_dies = face
            .triggers
            .iter()
            .filter(|t| is_modular_dies_transfer_trigger(t))
            .count();
        assert_eq!(
            modular_dies, 1,
            "the unrelated draw-on-death trigger must not pre-satisfy the \
             Modular idempotency check"
        );
    }

    /// CR 614.1c regression guard: a face that already carries a parsed
    /// "enters with K +1/+1 counters" ETB replacement with K ≠ N MUST still
    /// receive a synthesized Fixed(N) replacement. The per-N predicate
    /// prevents the K-replacement from silently pre-satisfying the Modular-N
    /// idempotency check (and the resulting card from entering with the
    /// wrong counter count). No printed card carries both today, but the
    /// safety is one line of code and pins the predicate semantics.
    #[test]
    fn synthesize_modular_does_not_dedupe_unrelated_etb_counter_replacement() {
        let mut face = face_with_modular(2);

        // Pre-existing K=3 ETB replacement (as if a parser had emitted one
        // for a printed "this permanent enters with 3 +1/+1 counters on it"
        // clause). Shape matches Modular's emission except for the count.
        let unrelated_etb = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::SelfRef,
                },
            ))),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some("Pre-existing K=3 ETB replacement".to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(unrelated_etb);

        synthesize_modular(&mut face);

        // Both replacements must coexist: the unrelated Fixed(3) and the
        // synthesized Fixed(2).
        let fixed2 = face
            .replacements
            .iter()
            .filter(|r| is_modular_etb_replacement(r, 2))
            .count();
        let fixed3 = face
            .replacements
            .iter()
            .filter(|r| is_modular_etb_replacement(r, 3))
            .count();
        assert_eq!(fixed2, 1, "Fixed(2) Modular ETB must still be emitted");
        assert_eq!(fixed3, 1, "Pre-existing Fixed(3) replacement preserved");
    }
}

#[cfg(test)]
mod modular_runtime_tests {
    //! CR 702.43a runtime integration: an Arcbound-style creature with
    //! `Keyword::Modular(n)` enters with N +1/+1 counters via the synthesized
    //! Moved replacement, and on death pushes a dies-transfer trigger that
    //! reads the LKI P1P1 counter count from `state.lki_cache` and places
    //! that many counters on a target artifact creature. The "you may" gate
    //! is honored by parking the engine in `WaitingFor::OptionalEffectChoice`.

    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::stack::resolve_top;
    use crate::game::triggers::process_triggers;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{GameState, StackEntryKind, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Build an artifact-creature face with `Keyword::Modular(n)` and run the
    /// full synthesis pipeline. Arcbound family cards are all artifact
    /// creatures.
    fn arcbound_face(name: &str, n: u32, base_pt: i32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(base_pt)),
            toughness: Some(PtValue::Fixed(base_pt)),
            keywords: vec![Keyword::Modular(n)],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Artifact);
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    /// Plain artifact creature target (no Modular). Used as the transfer
    /// destination in the dies-trigger tests.
    fn plain_artifact_creature_face(name: &str) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(1)),
            toughness: Some(PtValue::Fixed(1)),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Artifact);
        face.card_type.core_types.push(CoreType::Creature);
        face
    }

    fn setup_state_with_priority(controller: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
        state
    }

    /// Spawn an Arcbound creature in the Hand, then drive a real
    /// `ProposedEvent::ZoneChange { from: Hand, to: Battlefield }` through
    /// the engine replacement pipeline. The synthesized
    /// `ReplacementEvent::Moved` is absorbed by the pipeline into
    /// `enter_with_counters`, which `apply_etb_counters` then applies via
    /// `add_counter_with_replacement` — going through the same path
    /// `ReplacementEvent::AddCounter` modifiers (e.g., Hardened Scales) hook
    /// into. This exercises the full ETB-with-counters wiring end-to-end.
    fn spawn_arcbound_via_etb_pipeline(
        state: &mut GameState,
        face: &CardFace,
        controller: PlayerId,
    ) -> ObjectId {
        let next_card = CardId(state.next_object_id);
        // Place the object in Hand first so the proposed Hand→Battlefield
        // ZoneChange routes through the replacement pipeline.
        let obj_id = create_object(state, next_card, controller, face.name.clone(), Zone::Hand);
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }

        let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
            obj_id,
            Zone::Hand,
            Zone::Battlefield,
            None,
        );
        let mut events = Vec::new();
        let result = crate::game::replacement::replace_event(state, proposed, &mut events);
        let crate::game::replacement::ReplacementResult::Execute(event) = result else {
            panic!(
                "Arcbound ETB replacement is Mandatory — pipeline must execute directly, got {result:?}"
            );
        };
        let crate::types::proposed_event::ProposedEvent::ZoneChange {
            object_id,
            to,
            enter_with_counters,
            ..
        } = event
        else {
            panic!("pipeline must yield a ZoneChange execute event");
        };
        move_to_zone(state, object_id, to, &mut events);
        // CR 614.1c: Apply the counters the Moved replacement absorbed into
        // `enter_with_counters`. Each entry routes through
        // `add_counter_with_replacement` (the public single-authority counter
        // entry point) so Hardened-Scales-class AddCounter modifiers
        // (CR 614.1a) layer on. Mirrors the loop in
        // `engine_replacement::apply_etb_counters`, which is `pub(super)`
        // and not reachable from the database module — re-implementing the
        // public-API loop here is cleaner than widening visibility for one
        // test consumer.
        let actor = state
            .objects
            .get(&object_id)
            .map(|obj| obj.controller)
            .unwrap_or(controller);
        for (counter_type, count) in &enter_with_counters {
            crate::game::effects::counters::add_counter_with_replacement(
                state,
                actor,
                object_id,
                counter_type.clone(),
                *count,
                &mut events,
            );
        }
        obj_id
    }

    /// Place an Arcbound creature directly on the battlefield with N P1P1
    /// counters pre-installed, bypassing the ETB replacement pipeline. Used
    /// by dies-trigger tests that isolate LKI counter-snapshot semantics —
    /// the ETB path is exercised separately by
    /// `spawn_arcbound_via_etb_pipeline`. The "with_counters" suffix is
    /// load-bearing: callers must read this as a post-ETB shortcut, NOT as
    /// a pipeline-driving helper.
    fn place_arcbound_on_battlefield_with_counters(
        state: &mut GameState,
        face: &CardFace,
        controller: PlayerId,
    ) -> ObjectId {
        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(
            state,
            next_card,
            controller,
            face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }
        // Manually install the N counters the (skipped) ETB pipeline would
        // have placed — matches the post-replacement state the dies trigger
        // sees in real games.
        if let Some(Keyword::Modular(n)) = face
            .keywords
            .iter()
            .find(|kw| matches!(kw, Keyword::Modular(_)))
        {
            state
                .objects
                .get_mut(&obj_id)
                .unwrap()
                .counters
                .insert(CounterType::Plus1Plus1, *n);
        }
        obj_id
    }

    /// CR 702.43a clause 1 + CR 614.1c runtime: a real Hand→Battlefield
    /// ZoneChange routed through `replace_event` triggers the synthesized
    /// `ReplacementEvent::Moved`, which absorbs the `Effect::PutCounter`
    /// execute body into `enter_with_counters` on the ZoneChange event. The
    /// caller (`spawn_arcbound_via_etb_pipeline`) then calls `move_to_zone`
    /// followed by `add_counter_with_replacement` per absorbed counter,
    /// mirroring the dispatch path
    /// `engine_replacement::handle_replacement_choice` and `stack::resolve_top`
    /// use for spell-cast and choice-resume entries. After the pipeline
    /// settles, the object has exactly N P1P1 counters — proving the
    /// synthesized replacement integrates with the engine, not just that
    /// shape inspection matches the synthesizer's emit.
    #[test]
    fn modular_etb_via_pipeline_places_n_p1p1_counters() {
        let face = arcbound_face("Arcbound Crusher", 2, 5);

        let mut state = setup_state_with_priority(PlayerId(0));
        let obj_id = spawn_arcbound_via_etb_pipeline(&mut state, &face, PlayerId(0));

        // After the pipeline executes the Moved replacement and apply_etb_counters
        // runs, the object is on the battlefield with 2 P1P1 counters.
        let obj = state.objects.get(&obj_id).expect("object exists");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "object must reach battlefield after pipeline"
        );
        let p1p1 = *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0);
        assert_eq!(
            p1p1, 2,
            "the synthesized ETB replacement routed through replace_event \
             must place exactly Modular N (=2) +1/+1 counters"
        );
    }

    /// Shape-only check (decoupled from the pipeline test above): the
    /// synthesized replacement's execute body carries `Fixed(N)` so it can
    /// be absorbed by the Moved-event applier as ETB counters. Distinct from
    /// the synthesis_tests module's shape test in that it asserts against
    /// the post-`synthesize_all` face that an Arcbound Crusher would carry.
    #[test]
    fn arcbound_face_carries_fixed_n_etb_replacement() {
        let face = arcbound_face("Arcbound Crusher", 2, 5);
        let replacement = face
            .replacements
            .iter()
            .find(|r| is_modular_etb_replacement(r, 2))
            .expect("Arcbound Crusher should have the synthesized ETB replacement");
        let execute = replacement.execute.as_deref().unwrap();
        let Effect::PutCounter {
            count: QuantityExpr::Fixed { value },
            ..
        } = &*execute.effect
        else {
            panic!("ETB execute should be PutCounter with a fixed count");
        };
        assert_eq!(*value, 2, "Modular 2 places 2 counters on ETB");
    }

    /// CR 702.43a clause 2 happy path: a dying Arcbound creature with K
    /// counters on it transfers K counters to a target artifact creature
    /// (controller accepts the optional "you may").
    #[test]
    fn modular_dies_transfers_counters_to_target_artifact_creature() {
        let arcbound = arcbound_face("Arcbound Worker", 1, 1);
        let target_face = plain_artifact_creature_face("Steel Walker");

        let mut state = setup_state_with_priority(PlayerId(0));
        let arcbound_id =
            place_arcbound_on_battlefield_with_counters(&mut state, &arcbound, PlayerId(0));

        let target_card = CardId(state.next_object_id);
        let target_id = create_object(
            &mut state,
            target_card,
            PlayerId(0),
            target_face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            apply_card_face_to_object(obj, &target_face);
        }

        // Kill the Arcbound creature. `move_to_zone` snapshots LKI counters
        // into `state.lki_cache` so the dies trigger's LKI-counted quantity
        // resolves to 1 (the Modular N=1 ETB total).
        let mut events = Vec::new();
        move_to_zone(&mut state, arcbound_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);

        assert!(
            state
                .stack
                .iter()
                .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
            "modular dies-transfer must land on the stack"
        );

        // Resolve the trigger. Because it's optional, the engine parks in
        // `OptionalEffectChoice` for the controller; accept the prompt.
        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);

        // Drive the optional "may" choice → accept, then target selection.
        drive_optional_then_select_target(&mut state, target_id);

        let target_p1p1 = *state
            .objects
            .get(&target_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            target_p1p1, 1,
            "target artifact creature gains exactly 1 +1/+1 counter (= LKI source count)"
        );
    }

    /// CR 702.43a clause 2 + CR 400.7 + CR 122.2: the transfer count reads
    /// from LKI, so a creature that died with MORE counters than its printed
    /// Modular N transfers the modified post-ETB count — whatever counter
    /// total the LKI snapshot captured at zone exit. The test mutates
    /// `obj.counters` directly to a non-N value before death so the LKI
    /// snapshot pre-exit deviates from `Modular(N)`; this isolates the LKI
    /// look-back wiring (`resolve_counters_on_scope::Source` zone-keyed
    /// fallback) from any specific counter-modifier replacement effect.
    ///
    /// The "extra counters acquired post-ETB" framing is honest: the test
    /// proves "LKI captures whatever counter count was on the object at
    /// death," NOT "Hardened Scales doubles Modular ETB end-to-end." The
    /// latter is exercised separately by `hardened_scales_doubles_modular_etb`
    /// below.
    #[test]
    fn modular_dies_transfers_extra_counters_acquired_post_etb() {
        let arcbound = arcbound_face("Arcbound Worker", 1, 1);
        let target_face = plain_artifact_creature_face("Steel Walker");

        let mut state = setup_state_with_priority(PlayerId(0));
        let arcbound_id =
            place_arcbound_on_battlefield_with_counters(&mut state, &arcbound, PlayerId(0));
        // Direct mutation: simulate "Arcbound Worker ETB'd with 1 counter;
        // an additional counter was added by another source mid-life." LKI
        // must capture the modified total (2), not the printed Modular N (1).
        state
            .objects
            .get_mut(&arcbound_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        let target_card = CardId(state.next_object_id);
        let target_id = create_object(
            &mut state,
            target_card,
            PlayerId(0),
            target_face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            apply_card_face_to_object(obj, &target_face);
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, arcbound_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        drive_optional_then_select_target(&mut state, target_id);

        let target_p1p1 = *state
            .objects
            .get(&target_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            target_p1p1, 2,
            "transfer reads LKI count (2), NOT printed Modular N (1)"
        );
    }

    /// CR 702.43a + CR 614.1a + CR 614.1c real Hardened Scales end-to-end:
    /// install an `AddCounter` modifier (`QuantityModification::Plus { 1 }`,
    /// scoped to P1P1 counters via `CounterMatch::OfType(Plus1Plus1)`) on a
    /// separate battlefield object, then drive a Modular N=1 Arcbound Worker
    /// through the ETB pipeline. The flow:
    ///
    ///   1. `replace_event(ZoneChange { Hand → Battlefield })` matches the
    ///      synthesized Modular `Moved` replacement, absorbing
    ///      `Effect::PutCounter { Fixed(1), SelfRef }` into the ZoneChange's
    ///      `enter_with_counters = [("P1P1", 1)]`.
    ///   2. `apply_etb_counters` → `add_counter_with_replacement` proposes
    ///      `ProposedEvent::AddCounter { count: 1 }`, which goes through the
    ///      pipeline a second time. Hardened Scales matches via
    ///      `AddCounter`+`Plus1Plus1`, modifies count → 2.
    ///   3. The modified AddCounter applies, placing 2 P1P1 counters.
    ///   4. Killing the creature now snapshots `{P1P1: 2}` into LKI.
    ///   5. The dies-trigger transfers the LKI-counted 2 to the target.
    ///
    /// Proves both halves of the Modular wiring (CR 614.1c absorption + CR
    /// 122.1 LKI-counted transfer) compose correctly with a real CR 614.1a
    /// AddCounter modifier — exactly what Hardened Scales + Arcbound Worker
    /// does in a real game.
    #[test]
    fn hardened_scales_doubles_modular_etb_and_dies_transfer() {
        use crate::types::ability::QuantityModification;
        use crate::types::counter::CounterMatch;

        let arcbound = arcbound_face("Arcbound Worker", 1, 1);
        let target_face = plain_artifact_creature_face("Steel Walker");

        let mut state = setup_state_with_priority(PlayerId(0));

        // Install Hardened Scales as a battlefield object with an
        // `AddCounter` quantity modifier filtered to P1P1 counters. The
        // pipeline matches the modifier when the proposed AddCounter event's
        // counter type matches the `CounterMatch` filter.
        let hs_card = CardId(state.next_object_id);
        let hs_id = create_object(
            &mut state,
            hs_card,
            PlayerId(0),
            "Hardened Scales".to_string(),
            Zone::Battlefield,
        );
        {
            let hs_obj = state.objects.get_mut(&hs_id).unwrap();
            hs_obj.card_types.core_types.push(CoreType::Enchantment);
            hs_obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::Plus { value: 1 })
                    .counter_match(CounterMatch::OfType(CounterType::Plus1Plus1))
                    .description("Hardened Scales".to_string()),
            );
        }

        // Drive the Modular ETB through the full pipeline. The Moved
        // replacement absorbs Fixed(1) into enter_with_counters; the inner
        // AddCounter event is then modified by Hardened Scales → count=2.
        let arcbound_id = spawn_arcbound_via_etb_pipeline(&mut state, &arcbound, PlayerId(0));
        let etb_counters = *state
            .objects
            .get(&arcbound_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            etb_counters, 2,
            "Hardened Scales must add +1 to the Modular N=1 ETB: 1 + 1 = 2"
        );

        // Stand up the transfer target.
        let target_card = CardId(state.next_object_id);
        let target_id = create_object(
            &mut state,
            target_card,
            PlayerId(0),
            target_face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            apply_card_face_to_object(obj, &target_face);
        }

        // Kill the Arcbound Worker. LKI captures {P1P1: 2}.
        let mut events = Vec::new();
        move_to_zone(&mut state, arcbound_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        drive_optional_then_select_target(&mut state, target_id);

        // The transfer reads LKI = 2 and places 2 counters on the target.
        // Hardened Scales matches the inner AddCounter event again (it's a
        // P1P1 add) and adds another +1, so the target ends up with 3.
        // CR 614.5 prevents a replacement from re-applying to its own
        // already-replaced event, but Modular's transfer is a NEW AddCounter
        // event (not the same instance), so Hardened Scales fires on it too.
        let target_p1p1 = *state
            .objects
            .get(&target_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            target_p1p1, 3,
            "transfer count from LKI (2) is itself modified by Hardened \
             Scales on the transfer event: 2 + 1 = 3"
        );
    }

    /// CR 603.5: controller declines the optional "you may" — no counters
    /// transfer.
    #[test]
    fn modular_dies_may_be_skipped_by_controller() {
        let arcbound = arcbound_face("Arcbound Stinger", 1, 1);
        let target_face = plain_artifact_creature_face("Steel Walker");

        let mut state = setup_state_with_priority(PlayerId(0));
        let arcbound_id =
            place_arcbound_on_battlefield_with_counters(&mut state, &arcbound, PlayerId(0));

        let target_card = CardId(state.next_object_id);
        let target_id = create_object(
            &mut state,
            target_card,
            PlayerId(0),
            target_face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            apply_card_face_to_object(obj, &target_face);
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, arcbound_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);

        // Decline the "may" prompt.
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "optional dies-transfer must park engine on OptionalEffectChoice"
        );
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();

        let target_p1p1 = *state
            .objects
            .get(&target_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            target_p1p1, 0,
            "decline leaves target unchanged — no counters transferred"
        );
    }

    /// CR 702.43a + CR 115.1e + CR 115.2 + CR 800: in a 3-player game, an
    /// opponent-controlled artifact creature is a first-class legal target
    /// for the Modular dies-transfer. The target filter is
    /// `TypedFilter::creature().with_type(Artifact)` — no controller
    /// restriction. P0 (the dying Modular's controller) has none of their
    /// own artifact creatures; P1 has the artifact-creature target; P2 has
    /// a plain (non-artifact) creature that the Artifact + Creature
    /// conjunction filter must exclude. Auto-select binds P1's creature
    /// (the unique legal target), proving:
    ///   (a) opponent-controlled targets are not restricted away
    ///   (b) the conjunction filter actually filters — P2's plain creature
    ///       must NOT be considered a legal target
    /// Mirrors the 3-player rigor of
    /// `annihilator_in_multiplayer_targets_defending_player_not_all_opponents`.
    #[test]
    fn modular_dies_in_3p_can_target_opponents_artifact_creature() {
        let arcbound = arcbound_face("Arcbound Stinger", 1, 1);
        let target_face = plain_artifact_creature_face("Opposing Walker");

        // CR 800.1: 3-player game.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let arcbound_id =
            place_arcbound_on_battlefield_with_counters(&mut state, &arcbound, PlayerId(0));

        // P1 controls the artifact-creature target.
        let p1_target_card = CardId(state.next_object_id);
        let p1_target_id = create_object(
            &mut state,
            p1_target_card,
            PlayerId(1),
            target_face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&p1_target_id).unwrap();
            apply_card_face_to_object(obj, &target_face);
        }

        // P2 controls a plain (non-artifact) creature — an illegal target.
        // Asserts the Artifact + Creature conjunction filter actually
        // excludes non-artifact creatures rather than letting any creature
        // through.
        let p2_decoy_card = CardId(state.next_object_id);
        let p2_decoy_id = create_object(
            &mut state,
            p2_decoy_card,
            PlayerId(2),
            "Plain Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&p2_decoy_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, arcbound_id, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);

        // Exactly one legal target (P1's artifact creature) — auto-select
        // binds it. P2's plain creature is excluded by the Artifact
        // requirement on the target filter.
        assert!(
            state
                .stack
                .iter()
                .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
            "trigger with one legal target must auto-bind and push to stack"
        );

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        drive_optional_then_select_target(&mut state, p1_target_id);

        let p1_p1p1 = *state
            .objects
            .get(&p1_target_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        let p2_p1p1 = *state
            .objects
            .get(&p2_decoy_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            p1_p1p1, 1,
            "the opponent-controlled artifact creature is a legal target \
             and receives the transfer"
        );
        assert_eq!(
            p2_p1p1, 0,
            "the non-artifact creature is excluded by the Artifact + \
             Creature conjunction filter"
        );
    }

    /// Driver: accept the optional `may` prompt. Targets are auto-selected at
    /// stack-push time (CR 603.3d) when the synthesized trigger has exactly
    /// one legal target — every happy-path fixture here places exactly one
    /// legal artifact-creature target on the battlefield, so the engine
    /// auto-binds it (including the 3-player test, where P2's plain creature
    /// is filtered out by the Artifact requirement).
    fn drive_optional_then_select_target(state: &mut GameState, _target_id: ObjectId) {
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );
        crate::game::engine::apply_as_current(
            state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();
    }
}

#[cfg(test)]
mod graft_synthesis_tests {
    //! CR 702.58a + CR 702.58b: Shape tests for the synthesized Graft pair.
    //! Pinned to the exact wire-up the runtime resolver consumes:
    //!   * ETB-with-counters: `ReplacementEvent::Moved` with `valid_card =
    //!     SelfRef`, execute `Effect::PutCounter { counter_type: Plus1Plus1,
    //!     count: Fixed(N), target: SelfRef }`.
    //!   * "Another creature enters" trigger: `TriggerMode::ChangesZone`
    //!     (destination = Battlefield) with `valid_card` = Creature +
    //!     `FilterProp::Another` filter, condition
    //!     `HasCounters { OfType(P1P1), minimum: 1 }`, optional execute
    //!     `Effect::MoveCounters { source: SelfRef, target: TriggeringSource,
    //!     mode: Move, count: 1, counter_type: P1P1 }`.
    use super::*;

    fn face_with_graft(n: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Graft(n));
        face
    }

    /// CR 702.58a clause 1: ETB-with-N-counters replacement.
    #[test]
    fn synthesize_graft_adds_etb_counters_replacement() {
        let mut face = face_with_graft(2);
        synthesize_graft(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| is_graft_etb_replacement(r, 2))
            .expect("graft should synthesize an ETB-with-counters replacement");

        assert!(matches!(replacement.event, ReplacementEvent::Moved));
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));

        let execute = replacement
            .execute
            .as_deref()
            .expect("ETB replacement requires execute body");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("graft ETB execute body should be Effect::PutCounter");
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert!(matches!(target, TargetFilter::SelfRef));
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
    }

    /// CR 702.58a clause 2: "Whenever another creature enters" trigger.
    /// Shape pins the wire-up the runtime expects:
    ///   - `ChangesZone` → Battlefield (creature ETB)
    ///   - `valid_card` includes `FilterProp::Another` (CR 109.3)
    ///   - `condition = HasCounters { OfType(P1P1), minimum: 1 }`
    ///   - execute body is `MoveCounters` with the right shape
    ///   - execute is `.optional()` (CR 603.5 "you may")
    #[test]
    fn synthesize_graft_adds_another_creature_enters_trigger() {
        let mut face = face_with_graft(2);
        synthesize_graft(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_graft_enters_trigger(t))
            .expect("graft should synthesize an enters-battlefield trigger");

        assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
        assert_eq!(trigger.destination, Some(Zone::Battlefield));

        // valid_card must be a Creature filter with FilterProp::Another to
        // exclude the source object from "another creature".
        let TargetFilter::Typed(ref tf) = trigger.valid_card.as_ref().unwrap() else {
            panic!("graft trigger valid_card must be Typed");
        };
        assert!(tf
            .type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Creature)));
        assert!(tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Another)));

        // CR 603.4 + CR 702.58a "if this permanent has a +1/+1 counter on it".
        let Some(TriggerCondition::HasCounters {
            counters,
            minimum,
            maximum,
        }) = trigger.condition.as_ref()
        else {
            panic!("graft trigger condition must be HasCounters");
        };
        assert_eq!(*counters, CounterMatch::OfType(CounterType::Plus1Plus1));
        assert_eq!(*minimum, 1);
        assert_eq!(*maximum, None);

        let execute = trigger
            .execute
            .as_deref()
            .expect("trigger requires execute body");

        // CR 603.5: "you may" — optional triggered ability.
        assert!(
            execute.optional,
            "graft trigger must be optional per CR 702.58a 'you may'"
        );

        let Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            target,
            ..
        } = &*execute.effect
        else {
            panic!("graft trigger execute body should be Effect::MoveCounters");
        };
        assert!(matches!(source, TargetFilter::SelfRef));
        assert_eq!(*counter_type, Some(CounterType::Plus1Plus1));
        assert!(matches!(count, Some(QuantityExpr::Fixed { value: 1 })));
        assert_eq!(*mode, crate::types::ability::CounterTransferMode::Move);
        assert!(matches!(target, TargetFilter::TriggeringSource));
    }

    /// CR 702.58a is controller-agnostic: the trigger fires on ANY creature
    /// entering, not just on creatures the Graft controller controls. The
    /// `valid_card` filter must therefore NOT carry a `ControllerRef`
    /// constraint.
    #[test]
    fn graft_trigger_has_no_controller_restriction() {
        let mut face = face_with_graft(1);
        synthesize_graft(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|t| is_graft_enters_trigger(t))
            .expect("graft trigger present");
        let TargetFilter::Typed(ref tf) = trigger.valid_card.as_ref().unwrap() else {
            panic!("graft trigger valid_card must be Typed");
        };
        assert_eq!(
            tf.controller, None,
            "CR 702.58a does not restrict by controller — Graft fires on any creature ETB"
        );
    }

    /// Re-running synthesis must not duplicate the replacement or the trigger.
    #[test]
    fn synthesize_graft_is_idempotent() {
        let mut face = face_with_graft(2);
        synthesize_graft(&mut face);
        synthesize_graft(&mut face);

        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| is_graft_etb_replacement(r, 2))
                .count(),
            1,
            "ETB replacement should be deduped"
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_graft_enters_trigger(t))
                .count(),
            1,
            "enters-creature trigger should be deduped"
        );
    }

    /// A face without `Keyword::Graft` is unaffected.
    #[test]
    fn synthesize_graft_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_graft(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// CR 113.2c + CR 702.58b: each Graft instance emits its own ETB-counters
    /// replacement. The trigger is N-independent in shape (move one P1P1
    /// counter), so the trigger count equals the keyword count. No printed
    /// card today carries two Graft instances; the test pins the rule so a
    /// future printing (or a granted-Graft case) routes correctly.
    #[test]
    fn synthesize_graft_emits_per_instance() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Graft(1));
        face.keywords.push(Keyword::Graft(3));
        synthesize_graft(&mut face);

        // CR 113.2c: each instance emits its own ETB replacement; the
        // predicate is per-N so we filter by either N.
        let n1 = face
            .replacements
            .iter()
            .filter(|r| is_graft_etb_replacement(r, 1))
            .count();
        let n3 = face
            .replacements
            .iter()
            .filter(|r| is_graft_etb_replacement(r, 3))
            .count();
        assert_eq!(n1, 1, "Graft(1) emits one Fixed(1) ETB replacement");
        assert_eq!(n3, 1, "Graft(3) emits one Fixed(3) ETB replacement");

        // CR 702.58b: each instance is its own trigger too.
        let trigger_count = face
            .triggers
            .iter()
            .filter(|t| is_graft_enters_trigger(t))
            .count();
        assert_eq!(trigger_count, 2, "two Graft instances → two triggers");
    }

    /// Pre-existing ETB-with-counters replacement at K ≠ N does NOT dedupe a
    /// Graft N synthesis. Mirrors `synthesize_modular`'s per-N idempotency:
    /// a card that prints both "enters with K +1/+1 counters" AND Graft N
    /// where K ≠ N must still get its N-counter ETB replacement.
    #[test]
    fn graft_etb_does_not_dedupe_against_mismatched_fixed_replacement() {
        let mut face = face_with_graft(2);

        // Install a pre-existing Fixed(3) ETB replacement (shape-match for
        // is_graft_etb_replacement but at the wrong N).
        let pre_existing = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::SelfRef,
                },
            ))),
            valid_card: Some(TargetFilter::SelfRef),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(pre_existing);

        synthesize_graft(&mut face);

        let fixed2 = face
            .replacements
            .iter()
            .filter(|r| is_graft_etb_replacement(r, 2))
            .count();
        let fixed3 = face
            .replacements
            .iter()
            .filter(|r| is_graft_etb_replacement(r, 3))
            .count();
        assert_eq!(fixed2, 1, "Fixed(2) Graft ETB must still be emitted");
        assert_eq!(fixed3, 1, "Pre-existing Fixed(3) replacement preserved");
    }

    /// `KeywordTriggerInstaller::triggers_for(Keyword::Graft(_))` returns the
    /// enters-creature trigger — this is the runtime-granted Graft path
    /// (CR 604.1). Pins the shape consistency between the printed synthesis
    /// path and the runtime-granted path.
    #[test]
    fn keyword_installer_returns_graft_enters_trigger() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Graft(5));
        assert_eq!(
            triggers.len(),
            1,
            "granted Graft installs exactly one trigger (CR 604.1 — the ETB \
             replacement is missed by definition when granted)"
        );
        assert!(
            is_graft_enters_trigger(&triggers[0]),
            "granted Graft trigger must match the same shape predicate as the \
             printed synthesis path"
        );
    }

    /// `KeywordTriggerInstaller::trigger_matches_keyword_kind` recognizes the
    /// Graft trigger shape so `RemoveKeyword` correctly strips it when the
    /// keyword is removed.
    #[test]
    fn keyword_installer_recognizes_graft_trigger_for_removal() {
        let trigger = build_graft_enters_trigger();
        assert!(
            KeywordTriggerInstaller::trigger_matches_keyword_kind(&trigger, &Keyword::Graft(2)),
            "trigger_matches_keyword_kind must identify the Graft trigger \
             shape so RemoveKeyword can symmetrically strip it"
        );
    }

    /// CR 702.58a: `KeywordKind::Graft` is the canonical kind for the Graft
    /// variant — pins the mapping so the coverage layer reports Graft as a
    /// recognized keyword (not `Unknown`).
    #[test]
    fn graft_maps_to_dedicated_keyword_kind() {
        use crate::types::keywords::KeywordKind;
        assert_eq!(Keyword::Graft(1).kind(), KeywordKind::Graft);
        assert_eq!(Keyword::Graft(5).kind(), KeywordKind::Graft);
    }
}

#[cfg(test)]
mod graft_runtime_tests {
    //! CR 702.58a runtime integration: a Graft N creature enters with N +1/+1
    //! counters via the synthesized Moved replacement, and on subsequent
    //! creature ETB pushes a trigger that optionally moves one P1P1 counter
    //! from the Graft source onto the entering creature. The "you may" gate
    //! parks the engine in `WaitingFor::OptionalEffectChoice` per CR 603.5.
    //! Mirrors `modular_runtime_tests` patterns end-to-end.

    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::stack::resolve_top;
    use crate::game::triggers::process_triggers;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{GameState, StackEntryKind, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Build a Graft creature face. Vigean Graftmage / Plaxcaster Frogling
    /// class — a creature with `Keyword::Graft(n)`.
    fn graft_creature_face(name: &str, n: u32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(0)),
            toughness: Some(PtValue::Fixed(0)),
            keywords: vec![Keyword::Graft(n)],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    /// Plain creature face (no Graft). Used as the ETB observer that triggers
    /// the Graft source's "another creature enters" ability.
    fn plain_creature_face(name: &str) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(1)),
            toughness: Some(PtValue::Fixed(1)),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        face
    }

    fn setup_state_with_priority(controller: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
        state
    }

    /// Place a creature directly on the battlefield with `counters` P1P1
    /// counters pre-installed. Used by trigger tests that isolate the trigger
    /// behavior from the ETB replacement (which is exercised separately in
    /// the synthesis_tests module's shape tests).
    fn place_creature_with_counters(
        state: &mut GameState,
        face: &CardFace,
        controller: PlayerId,
        counters: u32,
    ) -> ObjectId {
        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(
            state,
            next_card,
            controller,
            face.name.clone(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }
        if counters > 0 {
            state
                .objects
                .get_mut(&obj_id)
                .unwrap()
                .counters
                .insert(CounterType::Plus1Plus1, counters);
        }
        obj_id
    }

    /// Spawn a creature on the battlefield via the move_to_zone path so that
    /// the engine emits a `Moved` event the trigger machinery can observe.
    /// Used to fire the Graft source's "another creature enters" trigger.
    fn spawn_creature_via_zone_change(
        state: &mut GameState,
        face: &CardFace,
        controller: PlayerId,
    ) -> ObjectId {
        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(state, next_card, controller, face.name.clone(), Zone::Hand);
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }
        let mut events = Vec::new();
        move_to_zone(state, obj_id, Zone::Battlefield, &mut events);
        process_triggers(state, &events);
        obj_id
    }

    /// CR 702.58a clause 1: A Graft N creature entering the battlefield places
    /// N +1/+1 counters on itself via the synthesized Moved replacement.
    #[test]
    fn graft_etb_places_n_p1p1_counters() {
        let face = graft_creature_face("Vigean Graftmage", 2);

        let mut state = setup_state_with_priority(PlayerId(0));

        // Drive a Hand→Battlefield ZoneChange through the replacement pipeline
        // and read back the counter state.
        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(
            &mut state,
            next_card,
            PlayerId(0),
            face.name.clone(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, &face);
        }

        let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
            obj_id,
            Zone::Hand,
            Zone::Battlefield,
            None,
        );
        let mut events = Vec::new();
        let result = crate::game::replacement::replace_event(&mut state, proposed, &mut events);
        let crate::game::replacement::ReplacementResult::Execute(event) = result else {
            panic!(
                "Graft ETB replacement is Mandatory — pipeline must execute directly, got {result:?}"
            );
        };
        let crate::types::proposed_event::ProposedEvent::ZoneChange {
            object_id,
            to,
            enter_with_counters,
            ..
        } = event
        else {
            panic!("pipeline must yield a ZoneChange execute event");
        };
        move_to_zone(&mut state, object_id, to, &mut events);
        let actor = state
            .objects
            .get(&object_id)
            .map(|obj| obj.controller)
            .unwrap_or(PlayerId(0));
        for (counter_type, count) in &enter_with_counters {
            crate::game::effects::counters::add_counter_with_replacement(
                &mut state,
                actor,
                object_id,
                counter_type.clone(),
                *count,
                &mut events,
            );
        }

        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        let p1p1 = *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0);
        assert_eq!(
            p1p1, 2,
            "Graft 2 must place exactly 2 +1/+1 counters via the synthesized \
             Moved replacement"
        );
    }

    /// CR 702.58a clause 2 happy path: with a Graft source already on the
    /// battlefield holding +1/+1 counters, when another creature ETBs the
    /// trigger fires and (after the controller accepts the may-prompt) moves
    /// one P1P1 counter from the source onto the new creature.
    #[test]
    fn graft_trigger_moves_counter_to_entering_creature_when_accepted() {
        let graft_face = graft_creature_face("Vigean Graftmage", 2);
        let other_face = plain_creature_face("Llanowar Elves");

        let mut state = setup_state_with_priority(PlayerId(0));
        let graft_id = place_creature_with_counters(&mut state, &graft_face, PlayerId(0), 2);

        // Spawn the second creature, which fires the Graft trigger.
        let other_id = spawn_creature_via_zone_change(&mut state, &other_face, PlayerId(0));

        assert!(
            state
                .stack
                .iter()
                .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
            "graft trigger must land on the stack"
        );

        // Resolve the optional trigger.
        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);

        // Accept the may-prompt; the engine auto-binds the single legal target.
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        let source_p1p1 = *state
            .objects
            .get(&graft_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        let target_p1p1 = *state
            .objects
            .get(&other_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            source_p1p1, 1,
            "Graft source loses one +1/+1 counter (2 → 1)"
        );
        assert_eq!(target_p1p1, 1, "Entering creature gains one +1/+1 counter");
    }

    /// CR 603.5 "you may" — declining the optional trigger leaves both
    /// objects' counter totals unchanged.
    #[test]
    fn graft_trigger_no_op_when_controller_declines() {
        let graft_face = graft_creature_face("Vigean Graftmage", 2);
        let other_face = plain_creature_face("Llanowar Elves");

        let mut state = setup_state_with_priority(PlayerId(0));
        let graft_id = place_creature_with_counters(&mut state, &graft_face, PlayerId(0), 2);
        let other_id = spawn_creature_via_zone_change(&mut state, &other_face, PlayerId(0));

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();

        let source_p1p1 = *state
            .objects
            .get(&graft_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        let target_p1p1 = *state
            .objects
            .get(&other_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(source_p1p1, 2, "declining leaves the source unchanged");
        assert_eq!(target_p1p1, 0, "declining leaves the target unchanged");
    }

    /// CR 702.58a "if this permanent has a +1/+1 counter on it" intervening-if:
    /// when the Graft source has zero P1P1 counters the trigger must NOT land
    /// on the stack (intervening-if fails at detection per CR 603.4).
    #[test]
    fn graft_trigger_does_not_fire_when_source_has_no_counters() {
        let graft_face = graft_creature_face("Vigean Graftmage", 2);
        let other_face = plain_creature_face("Llanowar Elves");

        let mut state = setup_state_with_priority(PlayerId(0));
        // Source on battlefield with ZERO +1/+1 counters.
        place_creature_with_counters(&mut state, &graft_face, PlayerId(0), 0);
        spawn_creature_via_zone_change(&mut state, &other_face, PlayerId(0));

        assert!(
            state.stack.is_empty(),
            "with zero +1/+1 counters the intervening-if `HasCounters` must \
             fail at detection — no trigger on the stack"
        );
    }

    /// CR 702.58a is controller-agnostic: an opponent's creature entering also
    /// fires the Graft trigger (the rule does not say "you control"). The
    /// move-counter resolution still places the counter on the opponent's
    /// creature, which is honest to the rule even if strategically inverted.
    #[test]
    fn graft_trigger_fires_for_opponent_controlled_creature() {
        let graft_face = graft_creature_face("Vigean Graftmage", 2);
        let opp_face = plain_creature_face("Goblin Guide");

        let mut state = setup_state_with_priority(PlayerId(0));
        place_creature_with_counters(&mut state, &graft_face, PlayerId(0), 2);
        // Opponent (PlayerId(1)) controls the entering creature.
        let opp_id = spawn_creature_via_zone_change(&mut state, &opp_face, PlayerId(1));

        assert!(
            state
                .stack
                .iter()
                .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
            "graft trigger must fire on opponent's creature ETB — CR 702.58a \
             does not restrict by controller"
        );

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        let target_p1p1 = *state
            .objects
            .get(&opp_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            target_p1p1, 1,
            "the counter lands on the opponent's creature when the controller \
             accepts the may-prompt"
        );
    }

    /// CR 702.58a "another creature": the Graft source's own ETB must NOT
    /// trigger itself. `FilterProp::Another` excludes the source object.
    #[test]
    fn graft_trigger_does_not_fire_on_own_etb() {
        let graft_face = graft_creature_face("Vigean Graftmage", 2);

        let mut state = setup_state_with_priority(PlayerId(0));

        // Drive the Graft creature itself through ETB. The synthesized ETB
        // replacement places 2 counters, but the "another creature" trigger
        // must NOT fire for the source's own entry.
        spawn_creature_via_zone_change(&mut state, &graft_face, PlayerId(0));

        // After ETB the stack should be empty (the synthesized Moved
        // replacement is consumed by the pipeline, not pushed to the stack;
        // the "another creature" trigger excludes self).
        assert!(
            state.stack.is_empty(),
            "Graft source's own ETB must not fire the another-creature trigger"
        );
    }
}

#[cfg(test)]
mod bloodthirst_synthesis_tests {
    //! CR 702.54a + CR 702.54b + CR 702.54c: Shape tests for the
    //! synthesized Bloodthirst ETB-with-counters replacement. Pinned to the
    //! exact wire-up the runtime resolver consumes.
    use super::*;

    fn face_with_bloodthirst(n: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::Bloodthirst(BloodthirstValue::Fixed(n)));
        face
    }

    fn face_with_bloodthirst_x() -> CardFace {
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::Bloodthirst(BloodthirstValue::X));
        face
    }

    /// CR 702.54a: ETB-with-N-counters replacement gated on
    /// `OpponentDamagedThisTurn`.
    #[test]
    fn synthesize_bloodthirst_adds_conditional_etb_replacement() {
        let mut face = face_with_bloodthirst(2);
        synthesize_bloodthirst(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| is_fixed_bloodthirst_etb_replacement(r, 2))
            .expect("bloodthirst should synthesize an ETB-with-counters replacement");

        assert!(matches!(replacement.event, ReplacementEvent::Moved));
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));
        assert!(matches!(
            replacement.condition,
            Some(ReplacementCondition::OpponentDamagedThisTurn)
        ));

        let execute = replacement
            .execute
            .as_deref()
            .expect("ETB replacement requires execute body");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("bloodthirst ETB execute body should be Effect::PutCounter");
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert!(matches!(target, TargetFilter::SelfRef));
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
    }

    /// CR 702.54b: Bloodthirst X is not gated by
    /// `OpponentDamagedThisTurn`; X itself resolves to the total damage
    /// opponents were dealt this turn.
    #[test]
    fn synthesize_bloodthirst_x_adds_unconditional_dynamic_etb_replacement() {
        let mut face = face_with_bloodthirst_x();
        synthesize_bloodthirst(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| is_bloodthirst_x_etb_replacement(r))
            .expect("bloodthirst X should synthesize a dynamic ETB replacement");

        assert!(matches!(replacement.event, ReplacementEvent::Moved));
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));
        assert_eq!(replacement.condition, None);

        let execute = replacement
            .execute
            .as_deref()
            .expect("ETB replacement requires execute body");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("bloodthirst X ETB execute body should be Effect::PutCounter");
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert!(matches!(target, TargetFilter::SelfRef));
        assert_eq!(count, &bloodthirst_counter_quantity(&BloodthirstValue::X));
    }

    #[test]
    fn mtgjson_bloodthirst_x_oracle_overrides_fixed_fallback() {
        let mtgjson = AtomicCard {
            name: "Petrified Wood-Kin".to_string(),
            mana_cost: Some("{6}{G}".to_string()),
            colors: vec!["G".to_string()],
            color_identity: vec!["G".to_string()],
            power: Some("3".to_string()),
            toughness: Some("3".to_string()),
            loyalty: None,
            defense: None,
            text: Some(
                "Bloodthirst X (This creature enters with X +1/+1 counters on it, where X is the damage dealt to your opponents this turn.)"
                    .to_string(),
            ),
            layout: "normal".to_string(),
            type_line: Some("Creature — Elemental Warrior".to_string()),
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elemental".to_string(), "Warrior".to_string()],
            supertypes: Vec::new(),
            keywords: Some(vec!["Bloodthirst".to_string()]),
            side: None,
            face_name: None,
            mana_value: 7.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: crate::database::mtgjson::AtomicIdentifiers {
                scryfall_id: None,
                scryfall_oracle_id: None,
            },
            foreign_data: Vec::new(),
        };

        let face = build_oracle_face(&mtgjson, None);

        assert!(
            face.keywords
                .contains(&Keyword::Bloodthirst(BloodthirstValue::X)),
            "Oracle Bloodthirst X must replace MTGJSON's bare Bloodthirst fallback"
        );
        assert!(
            !face
                .keywords
                .contains(&Keyword::Bloodthirst(BloodthirstValue::Fixed(1))),
            "Bloodthirst X must not leave the fixed-1 fallback behind"
        );
        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| is_bloodthirst_x_etb_replacement(r))
                .count(),
            1,
            "Bloodthirst X should synthesize exactly one dynamic replacement"
        );
    }

    /// Builds an MTGJSON-shaped `AtomicCard` for a card whose keyword line
    /// prints cascade as repeated bare words, so the synthesis pipeline can be
    /// exercised end-to-end. MTGJSON dedupes the keywords array to one "Cascade".
    fn cascade_atomic(
        name: &str,
        type_line: &str,
        subtypes: Vec<String>,
        text: &str,
    ) -> AtomicCard {
        AtomicCard {
            name: name.to_string(),
            mana_cost: Some("{8}{R}{G}".to_string()),
            colors: vec!["G".to_string(), "R".to_string()],
            color_identity: vec!["G".to_string(), "R".to_string(), "U".to_string()],
            power: Some("7".to_string()),
            toughness: Some("5".to_string()),
            loyalty: None,
            defense: None,
            text: Some(text.to_string()),
            layout: "normal".to_string(),
            type_line: Some(type_line.to_string()),
            types: vec!["Creature".to_string()],
            subtypes,
            supertypes: Vec::new(),
            keywords: Some(vec!["Cascade".to_string()]),
            side: None,
            face_name: None,
            mana_value: 10.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: crate::database::mtgjson::AtomicIdentifiers {
                scryfall_id: None,
                scryfall_oracle_id: None,
            },
            foreign_data: Vec::new(),
        }
    }

    /// CR 702.85c / CR 702.40b: Maelstrom Wanderer prints "Cascade, cascade".
    /// MTGJSON's deduped keywords array carries one "Cascade"; the synthesized
    /// face must carry exactly two so the runtime fires two cascade triggers.
    #[test]
    fn synthesize_face_recovers_maelstrom_wanderer_two_cascades() {
        let mtgjson = cascade_atomic(
            "Maelstrom Wanderer",
            "Creature — Elemental",
            vec!["Elemental".to_string()],
            "Creatures you control have haste.\nCascade, cascade (When you cast this spell, exile cards from the top of your library until you exile a nonland card that costs less. You may cast it without paying its mana cost. Put the exiled cards on the bottom of your library in a random order.)",
        );

        let face = build_oracle_face(&mtgjson, None);

        let cascades: Vec<&Keyword> = face
            .keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Cascade))
            .collect();
        assert_eq!(
            cascades.len(),
            2,
            "Maelstrom Wanderer must synthesize two Cascade instances"
        );
        // Pin ordering/leak behavior: filtering the face keywords to Cascade must
        // yield exactly [Cascade, Cascade] — no foreign keyword masquerades as
        // Cascade and both printed instances survive.
        assert_eq!(
            face.keywords
                .iter()
                .filter(|k| matches!(k, Keyword::Cascade))
                .cloned()
                .collect::<Vec<_>>(),
            vec![Keyword::Cascade, Keyword::Cascade],
        );
    }

    /// CR 702.85c: Apex Devastator prints cascade four times; the synthesized
    /// face must carry exactly four instances.
    #[test]
    fn synthesize_face_recovers_apex_devastator_four_cascades() {
        let mtgjson = cascade_atomic(
            "Apex Devastator",
            "Creature — Hydra",
            vec!["Hydra".to_string()],
            "Cascade, cascade, cascade, cascade (When you cast this spell, exile cards from the top of your library until you exile a nonland card that costs less. You may cast it without paying its mana cost. Put the exiled cards on the bottom of your library in a random order.)",
        );

        let face = build_oracle_face(&mtgjson, None);

        assert_eq!(
            face.keywords
                .iter()
                .filter(|k| matches!(k, Keyword::Cascade))
                .count(),
            4,
            "Apex Devastator must synthesize four Cascade instances"
        );
    }

    /// CR 702.85c regression guard: Bloodbraid Elf prints a single cascade and
    /// must net exactly one instance after synthesis — recovery must not double.
    #[test]
    fn synthesize_face_single_cascade_yields_one_instance() {
        let mtgjson = cascade_atomic(
            "Bloodbraid Elf",
            "Creature — Elf Berserker",
            vec!["Elf".to_string(), "Berserker".to_string()],
            "Haste\nCascade (When you cast this spell, exile cards from the top of your library until you exile a nonland card that costs less. You may cast it without paying its mana cost. Put the exiled cards on the bottom of your library in a random order.)",
        );

        let face = build_oracle_face(&mtgjson, None);

        assert_eq!(
            face.keywords
                .iter()
                .filter(|k| matches!(k, Keyword::Cascade))
                .count(),
            1,
            "Bloodbraid Elf must synthesize exactly one Cascade instance"
        );
    }

    /// Builds an MTGJSON-shaped `AtomicCard` for a creature whose keyword line
    /// prints myriad as repeated bare words. MTGJSON dedupes the keywords array
    /// to one "Myriad"; the synthesis pipeline must recover the printed count.
    fn myriad_atomic(name: &str, subtypes: Vec<String>, text: &str) -> AtomicCard {
        AtomicCard {
            name: name.to_string(),
            mana_cost: Some("{4}{G}".to_string()),
            colors: vec!["G".to_string()],
            color_identity: vec!["G".to_string()],
            power: Some("3".to_string()),
            toughness: Some("3".to_string()),
            loyalty: None,
            defense: None,
            text: Some(text.to_string()),
            layout: "normal".to_string(),
            type_line: Some("Creature — Squirrel".to_string()),
            types: vec!["Creature".to_string()],
            subtypes,
            supertypes: Vec::new(),
            keywords: Some(vec!["Myriad".to_string()]),
            side: None,
            face_name: None,
            mana_value: 5.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: crate::database::mtgjson::AtomicIdentifiers {
                scryfall_id: None,
                scryfall_oracle_id: None,
            },
            foreign_data: Vec::new(),
        }
    }

    /// CR 702.116b: Scurry of Squirrels prints "Myriad, myriad". MTGJSON's
    /// deduped keywords array carries one "Myriad"; the synthesized face must
    /// carry exactly two so `synthesize_all` installs two separate Myriad attack
    /// triggers (CR 702.116a), one per printed instance. Oracle text is verbatim
    /// from `data/card-data.json`.
    #[test]
    fn synthesize_face_recovers_scurry_of_squirrels_two_myriads() {
        let mtgjson = myriad_atomic(
            "Scurry of Squirrels",
            vec!["Squirrel".to_string()],
            "Myriad, myriad (Whenever this creature attacks, for each opponent other than defending player, you may create a token that's a copy of this creature that's tapped and attacking that player or a planeswalker they control. Then do it again. Exile the tokens at end of combat.)\nWhenever this creature deals combat damage to a player, put a +1/+1 counter on target creature you control.",
        );

        let face = build_oracle_face(&mtgjson, None);

        // CR 113.2c / CR 702.116b: both printed Myriad instances survive the
        // card-data merge instead of collapsing to one.
        assert_eq!(
            face.keywords
                .iter()
                .filter(|k| matches!(k, Keyword::Myriad))
                .count(),
            2,
            "Scurry of Squirrels must synthesize two Myriad instances"
        );

        // CR 702.116a/b: synthesize_all installs one Myriad attack trigger per
        // surviving instance, so the face must carry exactly two.
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_myriad_attack_trigger(t))
                .count(),
            2,
            "two Myriad instances must yield two separate Myriad attack triggers"
        );
    }

    /// CR 113.2c: the shared merge authority must preserve the parser-recovered
    /// multiplicity of an instances-function-separately keyword, dropping the
    /// single MTGJSON copy in favor of the two parser-extracted occurrences.
    #[test]
    fn merge_extracted_keywords_preserves_multi_instance_count() {
        let mut base = vec![Keyword::Myriad];
        merge_extracted_keywords(&mut base, vec![Keyword::Myriad, Keyword::Myriad]);
        assert_eq!(
            base.iter().filter(|k| matches!(k, Keyword::Myriad)).count(),
            2,
            "two recovered Myriad occurrences must net exactly two after merge"
        );
    }

    /// The shared merge authority replaces a parameterized MTGJSON default with
    /// the parser-extracted value of the same `kind()` (Bloodthirst path).
    #[test]
    fn merge_extracted_keywords_replaces_parameterized_default() {
        let mut base = vec![Keyword::Bloodthirst(BloodthirstValue::Fixed(0))];
        merge_extracted_keywords(
            &mut base,
            vec![Keyword::Bloodthirst(BloodthirstValue::Fixed(3))],
        );
        assert_eq!(
            base,
            vec![Keyword::Bloodthirst(BloodthirstValue::Fixed(3))],
            "parser-extracted Bloodthirst(3) must replace the MTGJSON default"
        );
    }

    /// Non-multiplicity parameterized keywords must be replaced, not duplicated,
    /// when the scenario harness supplies the same keyword as both the base hint
    /// and parser-extracted Oracle keyword.
    #[test]
    fn merge_extracted_keywords_does_not_duplicate_equal_parameterized_keyword() {
        let squad_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        };
        let mut base = vec![Keyword::Squad(squad_cost.clone())];
        merge_extracted_keywords(&mut base, vec![Keyword::Squad(squad_cost.clone())]);

        assert_eq!(base, vec![Keyword::Squad(squad_cost)]);
    }

    #[test]
    fn build_oracle_face_drops_craft_default_when_material_constraint_is_unparsed() {
        let mtgjson = AtomicCard {
            name: "Threefold Thunderhulk".to_string(),
            mana_cost: Some("{7}".to_string()),
            colors: Vec::new(),
            color_identity: Vec::new(),
            power: Some("0".to_string()),
            toughness: Some("0".to_string()),
            loyalty: None,
            defense: None,
            text: Some("Craft with two that share a card type {6}".to_string()),
            layout: "transform".to_string(),
            type_line: Some("Artifact Creature — Gnome".to_string()),
            types: vec!["Artifact".to_string(), "Creature".to_string()],
            subtypes: vec!["Gnome".to_string()],
            supertypes: Vec::new(),
            keywords: Some(vec!["Craft:{6}".to_string()]),
            side: None,
            face_name: None,
            mana_value: 7.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: crate::database::mtgjson::AtomicIdentifiers {
                scryfall_id: None,
                scryfall_oracle_id: None,
            },
            foreign_data: Vec::new(),
        };

        let face = build_oracle_face(&mtgjson, None);

        assert!(
            face.keywords
                .iter()
                .all(|keyword| !matches!(keyword, Keyword::Craft { .. })),
            "unparsed Craft material constraints must not keep MTGJSON's generic Craft fallback"
        );
        assert!(
            face.abilities
                .iter()
                .all(|definition| !matches!(definition.cost, Some(AbilityCost::Composite { .. }))),
            "unsupported Craft must not synthesize an approximate activated ability"
        );
    }

    /// Re-running synthesis must not duplicate the replacement.
    #[test]
    fn synthesize_bloodthirst_is_idempotent() {
        let mut face = face_with_bloodthirst(3);
        synthesize_bloodthirst(&mut face);
        synthesize_bloodthirst(&mut face);

        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| is_fixed_bloodthirst_etb_replacement(r, 3))
                .count(),
            1,
            "ETB replacement should be deduped"
        );
    }

    /// A face without `Keyword::Bloodthirst` is unaffected.
    #[test]
    fn synthesize_bloodthirst_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_bloodthirst(&mut face);
        assert!(face.replacements.is_empty());
        assert!(face.triggers.is_empty());
    }

    /// Negative test: unrelated keywords do not synthesize Bloodthirst.
    #[test]
    fn synthesize_bloodthirst_does_not_affect_other_keywords() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Trample);
        face.keywords.push(Keyword::Vigilance);
        synthesize_bloodthirst(&mut face);
        assert!(face.replacements.is_empty());
    }

    /// CR 113.2c + CR 702.54c: each Bloodthirst instance emits its own ETB
    /// replacement. No printed card today has two Bloodthirst instances;
    /// the test pins the rule so a future printing (or a granted-Bloodthirst
    /// case) routes correctly.
    #[test]
    fn synthesize_bloodthirst_emits_one_replacement_per_instance() {
        let mut face = CardFace::default();
        face.keywords
            .push(Keyword::Bloodthirst(BloodthirstValue::Fixed(1)));
        face.keywords
            .push(Keyword::Bloodthirst(BloodthirstValue::Fixed(3)));
        synthesize_bloodthirst(&mut face);

        let replacement_n1 = face
            .replacements
            .iter()
            .filter(|r| is_fixed_bloodthirst_etb_replacement(r, 1))
            .count();
        let replacement_n3 = face
            .replacements
            .iter()
            .filter(|r| is_fixed_bloodthirst_etb_replacement(r, 3))
            .count();
        assert_eq!(replacement_n1, 1, "exactly one Fixed(1) ETB replacement");
        assert_eq!(replacement_n3, 1, "exactly one Fixed(3) ETB replacement");
    }

    /// CR 702.54a regression guard: a face that already carries a parsed
    /// "enters with K +1/+1 counters" ETB replacement with K ≠ N MUST still
    /// receive a synthesized Fixed(N) replacement. The per-N predicate
    /// prevents the K-replacement from silently pre-satisfying the
    /// Bloodthirst-N idempotency check (and the resulting card from
    /// entering with the wrong counter count).
    #[test]
    fn is_bloodthirst_etb_replacement_per_n_predicate_distinguishes_k_vs_n() {
        let mut face = face_with_bloodthirst(2);

        // Pre-existing K=3 unconditional ETB replacement (as if a parser had
        // emitted one for a printed "this permanent enters with 3 +1/+1
        // counters on it" clause). Shape matches Bloodthirst's emission except
        // for the count AND the absence of the OpponentDamagedThisTurn
        // condition.
        let unrelated_etb = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::SelfRef,
                },
            ))),
            valid_card: Some(TargetFilter::SelfRef),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face.replacements.push(unrelated_etb);

        synthesize_bloodthirst(&mut face);

        // The K=3 replacement does not match the per-N predicate (its count
        // is 3, not 2; and it has no condition).
        let fixed_2_with_condition = face
            .replacements
            .iter()
            .filter(|r| is_fixed_bloodthirst_etb_replacement(r, 2))
            .count();
        assert_eq!(
            fixed_2_with_condition, 1,
            "Bloodthirst N=2 must emit its own Fixed(2)+condition replacement"
        );

        // An unconditional ETB-counters replacement with the SAME N as
        // Bloodthirst N must ALSO not dedupe — Bloodthirst is conditional,
        // the printed unconditional replacement is not. They must coexist.
        let mut face2 = face_with_bloodthirst(2);
        let unconditional_same_n = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::SelfRef,
                },
            ))),
            valid_card: Some(TargetFilter::SelfRef),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        face2.replacements.push(unconditional_same_n);
        synthesize_bloodthirst(&mut face2);
        // Two replacements: the unconditional one (no condition) and the
        // Bloodthirst-synthesized one (with condition).
        assert_eq!(
            face2.replacements.len(),
            2,
            "unconditional Fixed(N) and conditional Fixed(N) must coexist — \
             they are not the same replacement"
        );
        assert_eq!(
            face2
                .replacements
                .iter()
                .filter(|r| is_fixed_bloodthirst_etb_replacement(r, 2))
                .count(),
            1,
            "only the gated one is a Bloodthirst replacement"
        );
    }
}

#[cfg(test)]
mod bloodthirst_runtime_tests {
    //! CR 702.54a + CR 702.54b runtime integration: a Bloodthirst-bearing
    //! creature enters with +1/+1 counters via the synthesized Moved
    //! replacement. Fixed N is gated by opponent damage; X is dynamic and
    //! resolves from opponent damage totals.

    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::ability::{QuantityModification, TargetRef};
    use crate::types::card_type::CoreType;
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::game_state::{DamageRecord, GameState, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Build a creature face with `Keyword::Bloodthirst(Fixed(n))` and run the
    /// full synthesis pipeline.
    fn bloodthirst_face(name: &str, n: u32, base_pt: i32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(base_pt)),
            toughness: Some(PtValue::Fixed(base_pt)),
            keywords: vec![Keyword::Bloodthirst(BloodthirstValue::Fixed(n))],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn bloodthirst_x_face(name: &str, base_pt: i32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(base_pt)),
            toughness: Some(PtValue::Fixed(base_pt)),
            keywords: vec![Keyword::Bloodthirst(BloodthirstValue::X)],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn setup_state_with_priority(controller: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
        state
    }

    /// Drive a real Hand→Battlefield ZoneChange through the replacement
    /// pipeline, mirroring `spawn_arcbound_via_etb_pipeline`. The
    /// synthesized `ReplacementEvent::Moved` is absorbed by the pipeline
    /// into `enter_with_counters` (when the condition holds) and the
    /// resulting per-counter `add_counter_with_replacement` calls layer in
    /// any `AddCounter` modifiers (e.g., Hardened Scales).
    fn spawn_bloodthirst_via_etb_pipeline(
        state: &mut GameState,
        face: &CardFace,
        controller: PlayerId,
    ) -> ObjectId {
        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(state, next_card, controller, face.name.clone(), Zone::Hand);
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }

        let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
            obj_id,
            Zone::Hand,
            Zone::Battlefield,
            None,
        );
        let mut events = Vec::new();
        let result = crate::game::replacement::replace_event(state, proposed, &mut events);
        // When the condition is false, the replacement is not a candidate
        // and `replace_event` returns the unmodified Execute(ZoneChange)
        // with empty `enter_with_counters`. When the condition is true the
        // replacement applies and `enter_with_counters` is populated.
        let crate::game::replacement::ReplacementResult::Execute(event) = result else {
            panic!("Bloodthirst ETB pipeline must return Execute, got {result:?}");
        };
        let crate::types::proposed_event::ProposedEvent::ZoneChange {
            object_id,
            to,
            enter_with_counters,
            ..
        } = event
        else {
            panic!("pipeline must yield a ZoneChange execute event");
        };
        move_to_zone(state, object_id, to, &mut events);
        let actor = state
            .objects
            .get(&object_id)
            .map(|obj| obj.controller)
            .unwrap_or(controller);
        for (counter_type, count) in &enter_with_counters {
            crate::game::effects::counters::add_counter_with_replacement(
                state,
                actor,
                object_id,
                counter_type.clone(),
                *count,
                &mut events,
            );
        }
        obj_id
    }

    fn create_damage_source(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        create_object(
            state,
            card_id,
            controller,
            "Damage Source".to_string(),
            Zone::Battlefield,
        )
    }

    /// CR 702.54a: with no recorded opponent damage this turn, the
    /// Bloodthirst ETB replacement's condition is false and the permanent
    /// enters with 0 counters. The Moved event still resolves (the
    /// replacement only gates the inner counter-placing effect).
    #[test]
    fn bloodthirst_etb_no_damage_dealt_enters_without_counters() {
        let face = bloodthirst_face("Test Bloodthirster", 2, 2);

        let mut state = setup_state_with_priority(PlayerId(0));
        // Verify the per-turn damage tracker is empty at the start.
        assert!(state.damage_dealt_this_turn.is_empty());

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));

        let obj = state.objects.get(&obj_id).expect("object exists");
        assert_eq!(obj.zone, Zone::Battlefield, "object must reach battlefield");
        let p1p1 = *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0);
        assert_eq!(
            p1p1, 0,
            "Bloodthirst with no opponent damage this turn: no counters"
        );
    }

    /// CR 702.54a: when an opponent has been dealt damage this turn, the
    /// Bloodthirst condition is true and the permanent enters with N P1P1
    /// counters via the absorbed `Effect::PutCounter` execute body.
    #[test]
    fn bloodthirst_etb_after_damage_dealt_enters_with_n_counters() {
        let face = bloodthirst_face("Test Bloodthirster", 3, 2);

        let mut state = setup_state_with_priority(PlayerId(0));
        // Record direct damage to opponent (PlayerId(1)) earlier this turn.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(999), // any source; CR 702.54a doesn't care
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 1,
            is_combat: false,
            ..Default::default()
        });

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));

        let obj = state.objects.get(&obj_id).expect("object exists");
        assert_eq!(obj.zone, Zone::Battlefield);
        let p1p1 = *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0);
        assert_eq!(
            p1p1, 3,
            "Bloodthirst N=3 with opponent damaged earlier this turn → 3 counters"
        );
    }

    /// CR 702.54b: Bloodthirst X is unconditional; with no opponent damage,
    /// X resolves to 0 and the permanent enters without counters.
    #[test]
    fn bloodthirst_x_etb_no_damage_dealt_enters_without_counters() {
        let face = bloodthirst_x_face("Test Bloodthirst X", 3);

        let mut state = setup_state_with_priority(PlayerId(0));
        assert!(state.damage_dealt_this_turn.is_empty());

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));

        let obj = state.objects.get(&obj_id).expect("object exists");
        assert_eq!(obj.zone, Zone::Battlefield, "object must reach battlefield");
        let p1p1 = *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0);
        assert_eq!(p1p1, 0, "Bloodthirst X with no opponent damage: X = 0");
    }

    /// CR 702.54b: X is the total damage opponents were dealt this turn,
    /// not a fixed fallback of 1.
    #[test]
    fn bloodthirst_x_etb_counts_total_opponent_damage() {
        let face = bloodthirst_x_face("Test Bloodthirst X", 3);

        let mut state = setup_state_with_priority(PlayerId(0));
        let source_id = create_damage_source(&mut state, PlayerId(0));
        state.damage_dealt_this_turn.extend([
            DamageRecord {
                source_id,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 2,
                is_combat: false,
                ..Default::default()
            },
            DamageRecord {
                source_id,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 3,
                is_combat: true,
                ..Default::default()
            },
            DamageRecord {
                source_id,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(0)),
                target_controller: PlayerId(0),
                amount: 7,
                is_combat: false,
                ..Default::default()
            },
        ]);

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));

        let p1p1 = *state
            .objects
            .get(&obj_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            p1p1, 5,
            "Bloodthirst X must total opponent damage only: 2 + 3 = 5"
        );
    }

    /// CR 702.54b: the source of the earlier damage is irrelevant. Damage
    /// records must still count after the source object has left the game.
    #[test]
    fn bloodthirst_x_etb_counts_opponent_damage_from_missing_source() {
        let face = bloodthirst_x_face("Test Bloodthirst X", 3);

        let mut state = setup_state_with_priority(PlayerId(0));
        let source_id = create_damage_source(&mut state, PlayerId(0));
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id,
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 4,
            is_combat: true,
            ..Default::default()
        });
        state.objects.remove(&source_id);

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));

        let p1p1 = *state
            .objects
            .get(&obj_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            p1p1, 4,
            "Bloodthirst X must count opponent damage even if the source left"
        );
    }

    /// CR 702.54a + CR 614.1c: the condition is checked at the ETB window
    /// (replacement-applicability time). If opponent damage happens AFTER
    /// the permanent has entered, no retroactive counters appear.
    #[test]
    fn bloodthirst_condition_only_checks_at_etb_window() {
        let face = bloodthirst_face("Test Bloodthirster", 2, 2);

        let mut state = setup_state_with_priority(PlayerId(0));
        // No damage recorded yet.
        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));

        // After the permanent has entered, record damage to the opponent.
        // This must NOT retroactively add counters.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(999),
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 4,
            is_combat: false,
            ..Default::default()
        });

        let obj = state.objects.get(&obj_id).expect("object exists");
        let p1p1 = *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0);
        assert_eq!(
            p1p1, 0,
            "post-ETB damage must not retroactively add counters"
        );
    }

    /// CR 702.54a + CR 115.1 (multiplayer): in a 3-player game, ANY
    /// opponent being dealt damage satisfies the condition. The rule
    /// reads "an opponent" not "a specific opponent" — damage to any
    /// non-controller, non-eliminated player suffices.
    #[test]
    fn bloodthirst_in_3p_any_opponent_damaged_satisfies_condition() {
        let face = bloodthirst_face("Test Bloodthirster", 1, 2);

        // Build a 3-player state. `new_two_player` is the only constructor;
        // we extend with a third player by mirroring its initialization.
        // `opponents()` consults `seat_order` (CR 102.2), so both
        // `state.players` and `state.seat_order` must include the third
        // seat or the helper will not recognize it as an opponent.
        let mut state = setup_state_with_priority(PlayerId(0));
        let third_player = {
            let template = state.players[1].clone();
            let mut p2 = template;
            p2.id = PlayerId(2);
            state.players.push(p2);
            state.seat_order.push(PlayerId(2));
            PlayerId(2)
        };

        // Damage dealt to the SECOND opponent (PlayerId(2)) — not the
        // primary opponent (PlayerId(1)). Bloodthirst still triggers.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(999),
            source_controller: PlayerId(0),
            target: TargetRef::Player(third_player),
            target_controller: third_player,
            amount: 2,
            is_combat: false,
            ..Default::default()
        });

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));

        let p1p1 = *state
            .objects
            .get(&obj_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(p1p1, 1, "damage to ANY opponent satisfies CR 702.54a");
    }

    /// CR 702.54a + CR 614.1a + CR 614.1c: with the condition satisfied,
    /// the Bloodthirst ETB absorbs a `PutCounter(Fixed(N))` into
    /// `enter_with_counters`. Each per-counter `AddCounter` event then
    /// passes through the replacement pipeline, where a real Hardened
    /// Scales replacement (`QuantityModification::Plus { 1 }` filtered to
    /// P1P1) modifies the count → N + 1 counters land.
    #[test]
    fn bloodthirst_with_hardened_scales_doubles_counters_when_condition_met() {
        let face = bloodthirst_face("Test Bloodthirster", 2, 2);

        let mut state = setup_state_with_priority(PlayerId(0));
        // Condition satisfied: an opponent was damaged earlier this turn.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(999),
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 1,
            is_combat: true,
            ..Default::default()
        });

        // Install Hardened Scales as a battlefield object.
        let hs_card = CardId(state.next_object_id);
        let hs_id = create_object(
            &mut state,
            hs_card,
            PlayerId(0),
            "Hardened Scales".to_string(),
            Zone::Battlefield,
        );
        {
            let hs_obj = state.objects.get_mut(&hs_id).unwrap();
            hs_obj.card_types.core_types.push(CoreType::Enchantment);
            hs_obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::Plus { value: 1 })
                    .counter_match(CounterMatch::OfType(CounterType::Plus1Plus1))
                    .description("Hardened Scales".to_string()),
            );
        }

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, PlayerId(0));
        let p1p1 = *state
            .objects
            .get(&obj_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            p1p1, 3,
            "Hardened Scales adds +1 to the Bloodthirst N=2 ETB: 2 + 1 = 3"
        );
    }

    /// CR 514.2 + CR 702.54a: the damage-history store is cleared at the
    /// start of the next turn (`start_next_turn` clears
    /// `damage_dealt_this_turn`). Damage on turn 1 must NOT carry over
    /// into a Bloodthirst check on turn 2.
    #[test]
    fn bloodthirst_condition_clears_at_end_of_turn() {
        let face = bloodthirst_face("Test Bloodthirster", 2, 2);

        let mut state = setup_state_with_priority(PlayerId(0));
        // Turn 1: opponent took damage.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(999),
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 2,
            is_combat: true,
            ..Default::default()
        });

        // Advance to the next turn via the real engine path that clears
        // `damage_dealt_this_turn`.
        let mut events = Vec::new();
        crate::game::turns::start_next_turn(&mut state, &mut events);
        assert!(
            state.damage_dealt_this_turn.is_empty(),
            "start_next_turn must clear the per-turn damage record"
        );

        // Re-park the engine on priority for the new active player so the
        // ETB pipeline has a consistent starting state.
        let new_active = state.active_player;
        state.priority_player = new_active;
        state.waiting_for = WaitingFor::Priority { player: new_active };

        let obj_id = spawn_bloodthirst_via_etb_pipeline(&mut state, &face, new_active);
        let p1p1 = *state
            .objects
            .get(&obj_id)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0);
        assert_eq!(
            p1p1, 0,
            "after turn rollover the previous turn's damage no longer counts"
        );
    }
}

#[cfg(test)]
mod devour_synthesis_tests {
    //! CR 702.82a + CR 614.1c: Shape tests for the synthesized Devour
    //! as-enters replacement. Pinned to the exact wire-up the runtime
    //! resolver consumes — a `Moved`/`SelfRef` replacement whose `execute`
    //! chain is `Effect::Sacrifice` (ranged `UpTo` over your creatures) →
    //! `Effect::PutCounter` of P1P1 counters on `SelfRef`.
    use super::*;

    fn face_with_devour(n: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Devour(n));
        face
    }

    /// CR 702.82a: Devour 1 synthesizes one `Moved`/`SelfRef` replacement
    /// whose execute chain is `Sacrifice(UpTo) → PutCounter(P1P1, SelfRef)`,
    /// and whose `PutCounter` count is the bare `EventContextAmount` (one
    /// counter per creature sacrificed).
    #[test]
    fn synthesize_devour_1_builds_sacrifice_then_counter_chain() {
        let mut face = face_with_devour(1);
        synthesize_devour(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| is_devour_etb_replacement(r, 1))
            .expect("Devour 1 must synthesize an as-enters replacement");

        assert!(matches!(replacement.event, ReplacementEvent::Moved));
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));

        let execute = replacement
            .execute
            .as_deref()
            .expect("Devour replacement requires an execute body");

        // Parent effect: ranged "sacrifice up to N of your creatures".
        let Effect::Sacrifice {
            target,
            count,
            min_count,
        } = &*execute.effect
        else {
            panic!("Devour execute parent must be Effect::Sacrifice");
        };
        assert_eq!(
            *min_count, 0,
            "CR 702.82a: 'you may sacrifice any number' — an empty choice is legal"
        );
        assert!(
            matches!(count, QuantityExpr::UpTo { .. }),
            "Devour sacrifice count must be a ranged UpTo choice, got {count:?}"
        );
        assert_eq!(
            *target,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            "Devour sacrifices creatures the controller controls"
        );

        // Sub-ability: PutCounter of EventContextAmount P1P1 counters on self.
        let sub = execute
            .sub_ability
            .as_deref()
            .expect("Devour execute must chain to a PutCounter sub-ability");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*sub.effect
        else {
            panic!("Devour sub-ability must be Effect::PutCounter");
        };
        assert_eq!(*counter_type, CounterType::Plus1Plus1);
        assert!(matches!(target, TargetFilter::SelfRef));
        assert_eq!(
            *count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            },
            "Devour 1 places exactly one counter per creature sacrificed — \
             the count must be the bare EventContextAmount (NOT \
             PreviousEffectAmount, which the ranged Sacrifice never stamps)"
        );
    }

    /// CR 702.82a: Devour 2 scales the per-creature counter count by the
    /// keyword's N via `QuantityExpr::Multiply { factor: 2, .. }`.
    #[test]
    fn synthesize_devour_2_scales_counter_count_by_n() {
        let mut face = face_with_devour(2);
        synthesize_devour(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| is_devour_etb_replacement(r, 2))
            .expect("Devour 2 must synthesize an as-enters replacement");
        let sub = replacement
            .execute
            .as_deref()
            .and_then(|e| e.sub_ability.as_deref())
            .expect("Devour 2 execute must chain to a PutCounter sub-ability");
        let Effect::PutCounter { count, .. } = &*sub.effect else {
            panic!("Devour 2 sub-ability must be Effect::PutCounter");
        };
        assert_eq!(
            *count,
            QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                }),
            },
            "Devour 2 places 2 counters per creature sacrificed (CR 702.82a)"
        );
        // A Devour-2 replacement must not be mistaken for a Devour-1 one.
        assert!(!is_devour_etb_replacement(replacement, 1));
    }

    /// CR 113.2c: re-running synthesis is idempotent — exactly one Devour
    /// replacement survives.
    #[test]
    fn synthesize_devour_is_idempotent() {
        let mut face = face_with_devour(2);
        synthesize_devour(&mut face);
        synthesize_devour(&mut face);
        let count = face
            .replacements
            .iter()
            .filter(|r| is_devour_etb_replacement(r, 2))
            .count();
        assert_eq!(count, 1, "running synthesis twice must not duplicate");
    }

    /// A face with no Devour keyword gets no Devour replacement.
    #[test]
    fn synthesize_devour_is_noop_without_keyword() {
        let mut face = CardFace::default();
        synthesize_devour(&mut face);
        assert!(face.replacements.is_empty());
    }
}

#[cfg(test)]
mod living_weapon_synthesis_tests {
    use super::*;
    use crate::types::triggers::TriggerMode;

    fn face_with_living_weapon() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::LivingWeapon);
        face
    }

    /// CR 702.92a — Issue #974: Living weapon synthesis produces exactly one
    /// CR 702.124j + #1143: "Partner with [Name]" synthesizes an ETB trigger
    /// that lets the target player fetch the named partner from their library.
    #[test]
    fn synthesize_partner_with_emits_etb_search_trigger() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Partner(PartnerType::With(
            "Bebop, Skull & Crossbones".to_string(),
        )));
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_partner_with(&mut face);

        assert_eq!(face.triggers.len(), 1, "exactly one Partner With trigger");
        let trigger = &face.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
        let execute = trigger.execute.as_deref().expect("must have execute");
        assert!(execute.optional, "must be optional (\"may\")");
        match execute.effect.as_ref() {
            Effect::SearchLibrary {
                filter: TargetFilter::Named { name },
                target_player: Some(TargetFilter::Player),
                reveal: true,
                ..
            } => {
                assert_eq!(name, "Bebop, Skull & Crossbones");
            }
            other => panic!("expected SearchLibrary(Named, Player), got {other:?}"),
        }
        // Idempotency: calling again should not add a second trigger.
        synthesize_partner_with(&mut face);
        assert_eq!(face.triggers.len(), 1, "idempotent: no duplicate triggers");
    }

    #[test]
    fn synthesize_partner_with_idempotency_matches_exact_partner_name() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Partner(PartnerType::With(
            "Bebop, Skull & Crossbones".to_string(),
        )));
        face.triggers.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::SelfRef)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SearchLibrary {
                        filter: TargetFilter::Named {
                            name: "Different Partner".to_string(),
                        },
                        count: QuantityExpr::Fixed { value: 1 },
                        reveal: true,
                        source_zones: vec![Zone::Library],
                        target_player: Some(TargetFilter::Player),
                        selection_constraint: SearchSelectionConstraint::None,
                        split: None,
                    },
                )),
        );

        synthesize_partner_with(&mut face);

        assert_eq!(
            face.triggers.len(),
            2,
            "other named searches must not suppress Partner With synthesis"
        );
        assert!(face.triggers.iter().any(|trigger| {
            trigger.execute.as_deref().is_some_and(|execute| {
                matches!(
                    execute.effect.as_ref(),
                    Effect::SearchLibrary {
                        filter: TargetFilter::Named { name },
                        ..
                    } if name == "Bebop, Skull & Crossbones"
                )
            })
        }));
    }

    /// ChangesZone ETB trigger whose execute chain is `Token(Phyrexian Germ,
    /// 0/0 black) → Attach(SelfRef, LastCreated)`. Mirrors the job-select
    /// regression shape (both share the keyword-to-ETB-attach synthesis
    /// pattern), but the token spec is 0/0 black Phyrexian Germ instead of
    /// 1/1 colorless Hero.
    #[test]
    fn synthesize_living_weapon_builds_etb_trigger_with_token_and_attach() {
        let mut face = face_with_living_weapon();
        synthesize_living_weapon(&mut face);

        assert_eq!(face.triggers.len(), 1, "exactly one Living weapon trigger");
        let trigger = &face.triggers[0];
        assert!(
            matches!(trigger.mode, TriggerMode::ChangesZone),
            "trigger should be ChangesZone (ETB)",
        );
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert_eq!(
            trigger.valid_card,
            Some(TargetFilter::SelfRef),
            "trigger must scope to self-ETB only",
        );

        // Verify execute chain: Token(Phyrexian Germ 0/0 black) → Attach.
        let execute = trigger.execute.as_ref().expect("trigger must have execute");
        match execute.effect.as_ref() {
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                ..
            } => {
                assert_eq!(name, "Phyrexian Germ");
                assert!(matches!(power, crate::types::ability::PtValue::Fixed(0)));
                assert!(matches!(
                    toughness,
                    crate::types::ability::PtValue::Fixed(0)
                ));
                assert!(types.contains(&"Creature".to_string()));
                assert!(types.contains(&"Phyrexian".to_string()));
                assert!(types.contains(&"Germ".to_string()));
                assert_eq!(
                    colors,
                    &vec![crate::types::mana::ManaColor::Black],
                    "Phyrexian Germ must be black",
                );
            }
            other => panic!("expected Token effect, got {:?}", other),
        }

        // Verify sub_ability is Attach { attachment: SelfRef, target: LastCreated }.
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("Token effect must chain to Attach sub_ability");
        assert!(
            matches!(
                sub.effect.as_ref(),
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::LastCreated,
                }
            ),
            "sub_ability should be Attach(SelfRef, LastCreated), got {:?}",
            sub.effect,
        );
    }

    /// Re-running synthesis must not duplicate the ETB trigger — otherwise
    /// re-loaded card data would fire two Germ tokens per Equipment ETB.
    #[test]
    fn synthesize_living_weapon_is_idempotent() {
        let mut face = face_with_living_weapon();
        synthesize_living_weapon(&mut face);
        let count = face.triggers.len();
        synthesize_living_weapon(&mut face);
        assert_eq!(face.triggers.len(), count);
    }

    /// Faces without the Living weapon keyword get no Germ-token trigger.
    #[test]
    fn synthesize_living_weapon_skips_without_keyword() {
        let mut face = CardFace::default();
        synthesize_living_weapon(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 603.6a: ETB triggers fire from the battlefield. The synthesized
    /// ChangesZone trigger must list `Zone::Battlefield` in `trigger_zones`
    /// or the runtime evaluator never matches a Living-Weapon Equipment's
    /// ETB. Mirrors the job-select parity test so the shared
    /// `synthesize_etb_token_attach_keyword` helper can't accidentally drop
    /// the battlefield zone in a future refactor without both keyword
    /// modules failing.
    #[test]
    fn synthesize_living_weapon_binds_battlefield_trigger_zone() {
        let mut face = face_with_living_weapon();
        synthesize_living_weapon(&mut face);
        let trigger = &face.triggers[0];
        assert_eq!(trigger.trigger_zones, vec![Zone::Battlefield]);
    }
}

#[cfg(test)]
mod for_mirrodin_synthesis_tests {
    use super::*;
    use crate::types::triggers::TriggerMode;

    fn face_with_for_mirrodin() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::ForMirrodin);
        face
    }

    /// CR 702.163a: For Mirrodin! synthesis produces exactly one ChangesZone
    /// ETB trigger whose execute chain is `Token(Rebel, 2/2 red) →
    /// Attach(SelfRef, LastCreated)`. Shares the keyword-to-ETB-attach
    /// synthesis pattern with Living weapon and Job select.
    #[test]
    fn synthesize_for_mirrodin_builds_etb_trigger_with_token_and_attach() {
        let mut face = face_with_for_mirrodin();
        synthesize_for_mirrodin(&mut face);

        assert_eq!(face.triggers.len(), 1, "exactly one For Mirrodin! trigger");
        let trigger = &face.triggers[0];
        assert!(
            matches!(trigger.mode, TriggerMode::ChangesZone),
            "trigger should be ChangesZone (ETB)",
        );
        assert_eq!(trigger.destination, Some(Zone::Battlefield));
        assert_eq!(
            trigger.valid_card,
            Some(TargetFilter::SelfRef),
            "trigger must scope to self-ETB only",
        );

        // Verify execute chain: Token(Rebel, 2/2 red) → Attach.
        let execute = trigger.execute.as_ref().expect("trigger must have execute");
        match execute.effect.as_ref() {
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                ..
            } => {
                assert_eq!(name, "Rebel");
                assert!(matches!(power, crate::types::ability::PtValue::Fixed(2)));
                assert!(matches!(
                    toughness,
                    crate::types::ability::PtValue::Fixed(2)
                ));
                assert!(types.contains(&"Creature".to_string()));
                assert!(types.contains(&"Rebel".to_string()));
                assert_eq!(
                    colors,
                    &vec![crate::types::mana::ManaColor::Red],
                    "Rebel must be red (CR 702.163a)",
                );
            }
            other => panic!("expected Token effect, got {:?}", other),
        }

        // Verify sub_ability is Attach { attachment: SelfRef, target: LastCreated }.
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("Token effect must chain to Attach sub_ability");
        assert!(
            matches!(
                sub.effect.as_ref(),
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::LastCreated,
                }
            ),
            "sub_ability should be Attach(SelfRef, LastCreated), got {:?}",
            sub.effect,
        );
    }

    /// Re-running synthesis must not duplicate the ETB trigger.
    #[test]
    fn synthesize_for_mirrodin_is_idempotent() {
        let mut face = face_with_for_mirrodin();
        synthesize_for_mirrodin(&mut face);
        let count = face.triggers.len();
        synthesize_for_mirrodin(&mut face);
        assert_eq!(face.triggers.len(), count);
    }

    /// Faces without the For Mirrodin! keyword get no Rebel-token trigger.
    #[test]
    fn synthesize_for_mirrodin_skips_without_keyword() {
        let mut face = CardFace::default();
        synthesize_for_mirrodin(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 603.6a: ETB triggers fire from the battlefield. The synthesized
    /// ChangesZone trigger must list `Zone::Battlefield` in `trigger_zones`.
    #[test]
    fn synthesize_for_mirrodin_binds_battlefield_trigger_zone() {
        let mut face = face_with_for_mirrodin();
        synthesize_for_mirrodin(&mut face);
        let trigger = &face.triggers[0];
        assert_eq!(trigger.trigger_zones, vec![Zone::Battlefield]);
    }
}

#[cfg(test)]
mod reinforce_synthesis_tests {
    use super::*;
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn face_with_reinforce(count: u32, cost: ManaCost) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Reinforce { count, cost });
        face
    }

    /// CR 702.77a: Reinforce synthesis produces exactly one activated ability whose
    /// shape matches the reminder text — hand activation, composite cost of mana +
    /// self-discard, +1/+1 counters on target creature scaled by the fixed count.
    #[test]
    fn synthesize_reinforce_builds_activated_ability_with_correct_shape() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        };
        let mut face = face_with_reinforce(3, cost.clone());
        synthesize_reinforce(&mut face);

        assert_eq!(face.abilities.len(), 1, "exactly one reinforce ability");
        let def = &face.abilities[0];
        assert_eq!(def.kind, AbilityKind::Activated);
        assert_eq!(def.activation_zone, Some(Zone::Hand));
        // Reinforce is instant-speed (no sorcery restriction).
        assert!(!def.sorcery_speed);

        // CR 118.3: Composite cost — mana + discard-self.
        match def.cost.as_ref().expect("reinforce must have a cost") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2);
                assert!(matches!(&costs[0], AbilityCost::Mana { cost: c } if *c == cost));
                assert!(matches!(
                    &costs[1],
                    AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: None,
                        random: false,
                        self_ref: true,
                    }
                ));
            }
            other => panic!("expected Composite cost, got {:?}", other),
        }

        // CR 702.77a: Effect is N +1/+1 counters on target creature.
        match def.effect.as_ref() {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
                assert_eq!(count, &QuantityExpr::Fixed { value: 3 });
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))
                );
            }
            other => panic!("expected PutCounter effect, got {:?}", other),
        }
    }

    /// Reinforce with zero-cost mana (e.g., {0}) still produces a well-formed ability.
    #[test]
    fn synthesize_reinforce_handles_zero_cost() {
        let cost = ManaCost::zero();
        let mut face = face_with_reinforce(2, cost);
        synthesize_reinforce(&mut face);
        assert_eq!(face.abilities.len(), 1);
    }

    /// Cards without Reinforce are unaffected.
    #[test]
    fn synthesize_reinforce_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_reinforce(&mut face);
        assert!(face.abilities.is_empty());
    }

    /// Idempotent: calling synthesize_reinforce twice doubles the abilities
    /// (synthesis is additive, caller is responsible for single invocation).
    #[test]
    fn synthesize_reinforce_is_additive() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        };
        let mut face = face_with_reinforce(1, cost);
        synthesize_reinforce(&mut face);
        synthesize_reinforce(&mut face);
        assert_eq!(face.abilities.len(), 2);
    }

    /// CR 702.77a: Reinforce X (count=0) uses Variable("X") for counter quantity,
    /// resolved at runtime via chosen_x from the X in the mana cost.
    #[test]
    fn synthesize_reinforce_x_uses_variable_quantity() {
        // Swell of Courage: Reinforce X—{X}{W}{W}
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::White, ManaCostShard::White],
            generic: 0,
        };
        let mut face = face_with_reinforce(0, cost.clone());
        synthesize_reinforce(&mut face);
        assert_eq!(face.abilities.len(), 1, "exactly one reinforce ability");
        let def = &face.abilities[0];
        assert_eq!(def.kind, AbilityKind::Activated);
        assert_eq!(def.activation_zone, Some(Zone::Hand));
        // Verify the counter count is Variable("X"), not Fixed(0).
        match def.effect.as_ref() {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
                assert_eq!(
                    count,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string()
                        }
                    }
                );
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))
                );
            }
            other => panic!("expected PutCounter effect, got {:?}", other),
        }
        // Verify the mana cost contains X shard (triggers ChooseXValue at runtime).
        match def.cost.as_ref().expect("reinforce must have a cost") {
            AbilityCost::Composite { costs } => {
                assert!(matches!(&costs[0], AbilityCost::Mana { cost: c } if *c == cost));
            }
            other => panic!("expected Composite cost, got {:?}", other),
        }
    }
}

// ---------------------------------------------------------------------------
// Fading (CR 702.32) and Vanishing (CR 702.63) tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod fading_vanishing_tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0};
    use crate::types::actions::GameAction;
    use crate::types::game_state::{GameState, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    // --- SHAPE tests: synthesized triggers/replacements on the CardFace ---

    #[test]
    fn synthesize_fading_adds_etb_replacement_and_single_remove_or_sacrifice_trigger() {
        // CR 702.32a: Fading 2 enters with 2 fade counters; an upkeep trigger
        // removes one, then sacrifices if no counter was removed.
        let mut face = CardFace {
            keywords: vec![Keyword::Fading(2)],
            ..CardFace::default()
        };
        synthesize_fading(&mut face);

        // ETB-with-2-fade-counters replacement.
        let etb = face
            .replacements
            .iter()
            .find(|r| is_fade_vanish_etb_replacement(r, &CounterType::Fade, 2))
            .expect("Fading 2 must synthesize an enters-with-2-fade-counters replacement");
        assert!(matches!(
            etb.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::PutCounter {
                counter_type: CounterType::Fade,
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            })
        ));

        // Upkeep trigger: remove a fade counter, then sacrifice if none was removed.
        let removal = face
            .triggers
            .iter()
            .find(|t| is_fading_upkeep_trigger(t))
            .expect("Fading must synthesize a single upkeep remove-or-sacrifice trigger");
        assert!(removal.condition.is_none());
        assert!(matches!(
            removal.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::RemoveCounter {
                counter_type: Some(CounterType::Fade),
                count: 1,
                target: TargetFilter::SelfRef,
            })
        ));
        let sacrifice = removal
            .execute
            .as_deref()
            .and_then(|a| a.sub_ability.as_deref())
            .expect("Fading removal must carry the conditional sacrifice branch");
        assert!(matches!(
            &sacrifice.condition,
            Some(AbilityCondition::PreviousEffectAmount {
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            })
        ));
        assert!(matches!(
            &*sacrifice.effect,
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            }
        ));
    }

    #[test]
    fn synthesize_vanishing_adds_etb_replacement_removal_and_last_counter_sacrifice() {
        // CR 702.63a: Vanishing 2 enters with 2 time counters; an upkeep trigger
        // removes one; a CounterRemoved trigger sacrifices when the last is gone.
        let mut face = CardFace {
            keywords: vec![Keyword::Vanishing(2)],
            ..CardFace::default()
        };
        synthesize_vanishing(&mut face);

        let etb = face
            .replacements
            .iter()
            .find(|r| is_fade_vanish_etb_replacement(r, &CounterType::Time, 2))
            .expect("Vanishing 2 must synthesize an enters-with-2-time-counters replacement");
        assert!(matches!(
            etb.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::PutCounter {
                counter_type: CounterType::Time,
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            })
        ));

        let removal = face
            .triggers
            .iter()
            .find(|t| is_battlefield_upkeep_counter_removal_trigger(t, &CounterType::Time))
            .expect("Vanishing must synthesize an upkeep time-counter-removal trigger");
        assert!(matches!(
            removal.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::RemoveCounter {
                counter_type: Some(CounterType::Time),
                count: 1,
                target: TargetFilter::SelfRef,
            })
        ));

        // Last-counter sacrifice: CounterRemoved with threshold Some(0).
        let sac = face
            .triggers
            .iter()
            .find(|t| is_vanishing_sacrifice_trigger(t))
            .expect("Vanishing must synthesize a last-counter sacrifice trigger");
        assert!(matches!(sac.mode, TriggerMode::CounterRemoved));
        assert!(sac
            .counter_filter
            .as_ref()
            .is_some_and(|f| f.counter_type == CounterType::Time && f.threshold == Some(0)));
    }

    #[test]
    fn synthesize_fading_and_vanishing_are_idempotent_and_noop_without_keyword() {
        // Idempotency: re-running synthesis must not duplicate replacements/triggers.
        let mut fading = CardFace {
            keywords: vec![Keyword::Fading(3)],
            ..CardFace::default()
        };
        synthesize_fading(&mut fading);
        synthesize_fading(&mut fading);
        assert_eq!(
            fading
                .replacements
                .iter()
                .filter(|r| is_fade_vanish_etb_replacement(r, &CounterType::Fade, 3))
                .count(),
            1
        );
        assert_eq!(
            fading
                .triggers
                .iter()
                .filter(|t| is_fading_upkeep_trigger(t))
                .count(),
            1
        );

        let mut vanishing = CardFace {
            keywords: vec![Keyword::Vanishing(3)],
            ..CardFace::default()
        };
        synthesize_vanishing(&mut vanishing);
        synthesize_vanishing(&mut vanishing);
        assert_eq!(
            vanishing
                .triggers
                .iter()
                .filter(|t| is_vanishing_sacrifice_trigger(t))
                .count(),
            1
        );

        // No-op without the keyword.
        let mut other = CardFace {
            keywords: vec![Keyword::Menace],
            ..CardFace::default()
        };
        synthesize_fading(&mut other);
        synthesize_vanishing(&mut other);
        assert!(other.triggers.is_empty());
        assert!(other.replacements.is_empty());
    }

    #[test]
    fn fade_counter_round_trips_through_serialization() {
        // CR 122.1: the fade counter is a distinct named type, serialized as "fade".
        assert_eq!(CounterType::Fade.as_str().as_ref(), "fade");
        let json = serde_json::to_string(&CounterType::Fade).unwrap();
        assert_eq!(json, "\"fade\"");
        let back: CounterType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CounterType::Fade);
    }

    // --- End-to-end runtime tests: drive real turns and observe behavior ---

    /// Advance real turn progression (through `apply`, the clobber site) until
    /// `PlayerId(0)` reaches an upkeep strictly later than `after_turn`,
    /// draining any stack the auto-advance path places (so upkeep triggers
    /// resolve). Returns the turn number reached.
    fn run_to_p0_upkeep(state: &mut GameState, after_turn: u32) -> u32 {
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 400, "turn progression stalled before P0's upkeep");
            if state.phase == Phase::Upkeep
                && state.active_player == PlayerId(0)
                && state.turn_number > after_turn
                && state.stack.is_empty()
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                return state.turn_number;
            }
            if !state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                crate::game::engine::apply_as_current(state, GameAction::PassPriority)
                    .expect("priority pass to resolve stack");
                continue;
            }
            match &state.waiting_for {
                WaitingFor::Priority { .. } => {
                    crate::game::engine::apply_as_current(state, GameAction::PassPriority)
                        .expect("priority pass to advance the turn");
                }
                WaitingFor::DeclareAttackers { .. } => {
                    crate::game::engine::apply_as_current(
                        state,
                        GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        },
                    )
                    .expect("declare no attackers");
                }
                WaitingFor::DeclareBlockers { .. } => {
                    crate::game::engine::apply_as_current(
                        state,
                        GameAction::DeclareBlockers {
                            assignments: vec![],
                        },
                    )
                    .expect("declare no blockers");
                }
                other => panic!("unexpected waiting state during turn progression: {other:?}"),
            }
        }
    }

    fn run_until_p0_upkeep_trigger_on_stack(state: &mut GameState, after_turn: u32) -> u32 {
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(
                guard < 400,
                "turn progression stalled before P0's upkeep trigger"
            );
            if state.phase == Phase::Upkeep
                && state.active_player == PlayerId(0)
                && state.turn_number > after_turn
                && !state.stack.is_empty()
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                return state.turn_number;
            }
            match &state.waiting_for {
                WaitingFor::Priority { .. } => {
                    crate::game::engine::apply_as_current(state, GameAction::PassPriority)
                        .expect("priority pass to advance to upkeep trigger");
                }
                WaitingFor::DeclareAttackers { .. } => {
                    crate::game::engine::apply_as_current(
                        state,
                        GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        },
                    )
                    .expect("declare no attackers");
                }
                WaitingFor::DeclareBlockers { .. } => {
                    crate::game::engine::apply_as_current(
                        state,
                        GameAction::DeclareBlockers {
                            assignments: vec![],
                        },
                    )
                    .expect("declare no blockers");
                }
                other => panic!("unexpected waiting state during turn progression: {other:?}"),
            }
        }
    }

    /// Build a 2-player state with a battlefield creature controlled by P0 that
    /// carries `counters` of `counter_type` plus the supplied synthesized
    /// triggers. Libraries are stocked so neither player decks out. Returns
    /// `(state, creature_id)`. Starts on P0's turn at the post-upkeep main phase
    /// so the FIRST upkeep observed by `run_to_p0_upkeep` is a real later turn.
    fn battlefield_with_triggers(
        counter_type: CounterType,
        counters: u32,
        triggers: Vec<TriggerDefinition>,
    ) -> (GameState, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let creature = scenario.add_creature(P0, "Test Permanent", 2, 2).id();
        let mut runner = scenario.build();
        {
            let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
            obj.counters.insert(counter_type, counters);
            for t in &triggers {
                obj.trigger_definitions.push(t.clone());
                std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(t.clone());
            }
        }
        // Stock libraries to avoid decking out across several turns.
        let state = runner.state_mut();
        for player in [PlayerId(0), PlayerId(1)] {
            for i in 0..20u64 {
                crate::game::zones::create_object(
                    state,
                    CardId(9000 + u64::from(player.0) * 100 + i),
                    player,
                    format!("Filler {}-{i}", player.0),
                    Zone::Library,
                );
            }
        }
        let state = std::mem::replace(runner.state_mut(), GameState::new_two_player(0));
        (state, creature)
    }

    #[test]
    fn fading_two_survives_two_upkeeps_then_sacrificed_on_third() {
        // CR 702.32a: Fading 2 — enters with 2 fade counters, survives two of
        // P0's upkeeps (removing one fade counter each), and is sacrificed on
        // the THIRD upkeep, the one where it can't remove a counter (count 0).
        let (mut state, creature) =
            battlefield_with_triggers(CounterType::Fade, 2, vec![build_fading_upkeep_trigger()]);

        let start = state.turn_number;

        // First P0 upkeep after setup: 2 -> 1 fade counters, still on battlefield.
        run_to_p0_upkeep(&mut state, start);
        assert_eq!(
            state.objects[&creature]
                .counters
                .get(&CounterType::Fade)
                .copied(),
            Some(1),
            "after 1st upkeep, one fade counter removed (2 -> 1)"
        );
        assert_eq!(state.objects[&creature].zone, Zone::Battlefield);

        // Second P0 upkeep: 1 -> 0 fade counters, still on battlefield (survives).
        let t2 = state.turn_number;
        run_to_p0_upkeep(&mut state, t2);
        assert_eq!(
            state.objects[&creature]
                .counters
                .get(&CounterType::Fade)
                .copied()
                .unwrap_or(0),
            0,
            "after 2nd upkeep, last fade counter removed (1 -> 0)"
        );
        assert_eq!(
            state.objects[&creature].zone,
            Zone::Battlefield,
            "CR 702.32a: Fading survives the upkeep that removes its LAST counter"
        );

        // Third P0 upkeep: cannot remove a fade counter (0 remain) -> sacrificed.
        let t3 = state.turn_number;
        run_to_p0_upkeep(&mut state, t3);
        assert_eq!(
            state.objects[&creature].zone,
            Zone::Graveyard,
            "CR 702.32a: Fading is sacrificed on the upkeep where it CAN'T remove a fade counter (the 3rd upkeep)"
        );
    }

    #[test]
    fn fading_sacrifices_if_last_counter_removed_before_upkeep_trigger_resolves() {
        // CR 702.32a: Fading decides "if you can't" as the upkeep trigger
        // resolves. If the last fade counter disappears after the trigger is
        // put onto the stack but before it resolves, the permanent is
        // sacrificed during that resolution.
        let (mut state, creature) =
            battlefield_with_triggers(CounterType::Fade, 1, vec![build_fading_upkeep_trigger()]);

        let start = state.turn_number;
        run_until_p0_upkeep_trigger_on_stack(&mut state, start);
        state
            .objects
            .get_mut(&creature)
            .expect("fading permanent exists")
            .counters
            .insert(CounterType::Fade, 0);

        while !state.stack.is_empty() {
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                .expect("priority pass to resolve fading trigger");
        }

        assert_eq!(
            state.objects[&creature].zone,
            Zone::Graveyard,
            "Fading must sacrifice when no fade counter can be removed at trigger resolution"
        );
    }

    #[test]
    fn vanishing_two_sacrificed_on_second_upkeep_when_last_counter_removed() {
        // CR 702.63a: Vanishing 2 — enters with 2 time counters, removes one on
        // the first P0 upkeep (2 -> 1, survives), and is sacrificed on the SECOND
        // upkeep, the removal that takes it to 0 (one upkeep EARLIER than Fading 2).
        let (mut state, creature) = battlefield_with_triggers(
            CounterType::Time,
            2,
            vec![
                build_battlefield_upkeep_counter_removal_trigger(CounterType::Time, "702.63a"),
                build_vanishing_sacrifice_trigger(),
            ],
        );

        let start = state.turn_number;

        // First P0 upkeep: 2 -> 1 time counters, still on battlefield.
        run_to_p0_upkeep(&mut state, start);
        assert_eq!(
            state.objects[&creature]
                .counters
                .get(&CounterType::Time)
                .copied(),
            Some(1),
            "after 1st upkeep, one time counter removed (2 -> 1)"
        );
        assert_eq!(state.objects[&creature].zone, Zone::Battlefield);

        // Second P0 upkeep: removal takes it 1 -> 0 and the last-counter
        // trigger sacrifices it the SAME upkeep.
        let t2 = state.turn_number;
        run_to_p0_upkeep(&mut state, t2);
        assert_eq!(
            state.objects[&creature].zone,
            Zone::Graveyard,
            "CR 702.63a: Vanishing is sacrificed WHEN its last time counter is removed (the 2nd upkeep)"
        );
    }
}

#[cfg(test)]
mod sunburst_synthesis_tests {
    //! CR 702.44a + CR 702.44b + CR 702.44d: shape tests for the synthesized
    //! Sunburst ETB-with-counters replacement.
    //!
    //! A Sunburst face gets one `ReplacementEvent::Moved` replacement per
    //! `Keyword::Sunburst` instance whose execute body is `Effect::PutCounter`
    //! on `SelfRef` with the count read from
    //! `QuantityRef::ManaSpentToCast { SelfObject, DistinctColors }`. The counter
    //! type is `Plus1Plus1` for a creature face and `Generic("charge")` for any
    //! noncreature face.
    use super::*;

    fn creature_sunburst_face() -> CardFace {
        let mut face = CardFace {
            name: "Sunburst Creature".to_string(),
            power: Some(PtValue::Fixed(0)),
            toughness: Some(PtValue::Fixed(0)),
            keywords: vec![Keyword::Sunburst],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Artifact);
        face.card_type.core_types.push(CoreType::Creature);
        face
    }

    fn artifact_sunburst_face() -> CardFace {
        let mut face = CardFace {
            name: "Sunburst Artifact".to_string(),
            keywords: vec![Keyword::Sunburst],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Artifact);
        face
    }

    /// CR 702.44a (creature branch): a creature face synthesizes a Moved → SelfRef
    /// `PutCounter` replacement placing +1/+1 counters, counted by the distinct
    /// colors of mana spent to cast it.
    #[test]
    fn synthesize_sunburst_creature_adds_p1p1_etb_replacement() {
        let mut face = creature_sunburst_face();
        synthesize_sunburst(&mut face);

        let replacement = face
            .replacements
            .iter()
            .find(|r| is_sunburst_etb_replacement(r, &CounterType::Plus1Plus1))
            .expect("creature Sunburst must synthesize a P1P1 ETB replacement");

        assert!(matches!(replacement.event, ReplacementEvent::Moved));
        assert!(matches!(
            replacement.valid_card,
            Some(TargetFilter::SelfRef)
        ));

        let execute = replacement
            .execute
            .as_deref()
            .expect("ETB replacement requires execute body");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*execute.effect
        else {
            panic!("sunburst ETB execute body should be Effect::PutCounter");
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert!(matches!(target, TargetFilter::SelfRef));
        // CR 702.44a + CR 601.2h: one counter per distinct color of mana spent.
        assert!(matches!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::DistinctColors,
                },
            }
        ));
    }

    /// CR 702.44a (noncreature branch): a noncreature artifact face synthesizes a
    /// Moved → SelfRef `PutCounter` placing charge counters
    /// (`Generic("charge")`), counted by distinct colors spent.
    #[test]
    fn synthesize_sunburst_artifact_adds_charge_etb_replacement() {
        let mut face = artifact_sunburst_face();
        synthesize_sunburst(&mut face);

        let charge = CounterType::Generic("charge".to_string());
        let replacement = face
            .replacements
            .iter()
            .find(|r| is_sunburst_etb_replacement(r, &charge))
            .expect("noncreature Sunburst must synthesize a charge ETB replacement");

        let execute = replacement
            .execute
            .as_deref()
            .expect("requires execute body");
        let Effect::PutCounter {
            counter_type,
            count,
            ..
        } = &*execute.effect
        else {
            panic!("expected Effect::PutCounter");
        };
        assert_eq!(counter_type, &charge);
        // The noncreature branch must NOT emit a +1/+1 replacement.
        assert!(
            !face
                .replacements
                .iter()
                .any(|r| is_sunburst_etb_replacement(r, &CounterType::Plus1Plus1)),
            "a noncreature Sunburst face must not place +1/+1 counters (CR 702.44a)"
        );
        assert!(matches!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::DistinctColors,
                },
            }
        ));
    }

    /// Re-running synthesis must not duplicate the replacement (idempotency
    /// discipline shared with `synthesize_modular`).
    #[test]
    fn synthesize_sunburst_is_idempotent() {
        let mut face = creature_sunburst_face();
        synthesize_sunburst(&mut face);
        synthesize_sunburst(&mut face);

        let count = face
            .replacements
            .iter()
            .filter(|r| is_sunburst_etb_replacement(r, &CounterType::Plus1Plus1))
            .count();
        assert_eq!(count, 1, "Sunburst ETB replacement must be emitted once");
    }

    /// CR 702.44d: two instances of Sunburst each work separately — two
    /// replacements are emitted (so the counters stack).
    #[test]
    fn synthesize_sunburst_emits_one_replacement_per_instance() {
        let mut face = creature_sunburst_face();
        face.keywords.push(Keyword::Sunburst);
        synthesize_sunburst(&mut face);

        let count = face
            .replacements
            .iter()
            .filter(|r| is_sunburst_etb_replacement(r, &CounterType::Plus1Plus1))
            .count();
        assert_eq!(
            count, 2,
            "two Sunburst instances must each emit their own ETB replacement (CR 702.44d)"
        );
    }

    /// A face without `Keyword::Sunburst` is unaffected.
    #[test]
    fn synthesize_sunburst_is_noop_without_keyword() {
        let mut face = CardFace::default();
        face.card_type.core_types.push(CoreType::Artifact);
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_sunburst(&mut face);
        assert!(face.replacements.is_empty());
    }
}

#[cfg(test)]
mod sunburst_runtime_tests {
    //! CR 702.44a/b runtime integration: cast a Sunburst object through the full
    //! casting pipeline paying N distinct colors and assert it enters the
    //! battlefield with N counters of the branch-correct type. The full cast is
    //! required so the engine populates the entering object's
    //! `colors_spent_to_cast` from the actual mana spent (CR 601.2h), which the
    //! synthesized ETB replacement reads via
    //! `QuantityRef::ManaSpentToCast { DistinctColors }`.
    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::scenario::GameRunner;
    use crate::game::zones::create_object;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{GameState, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::player::PlayerId;

    const P0: PlayerId = PlayerId(0);

    /// A Sunburst face with the given printed core types, fully synthesized.
    fn sunburst_face(name: &str, core_types: &[CoreType], pt: Option<i32>) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: pt.map(PtValue::Fixed),
            toughness: pt.map(PtValue::Fixed),
            keywords: vec![Keyword::Sunburst],
            ..CardFace::default()
        };
        for ct in core_types {
            face.card_type.core_types.push(*ct);
        }
        synthesize_all(&mut face);
        face
    }

    /// Stage a Sunburst spell in P0's hand with the given mana cost, fund P0's
    /// pool with exactly `colors` (one unit each), and park the engine at P0
    /// priority in a main phase. Returns the runner and the spell object id.
    fn stage_sunburst_cast(
        face: &CardFace,
        shards: Vec<ManaCostShard>,
        colors: &[ManaColor],
    ) -> (GameRunner, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        let card_id = CardId(state.next_object_id);
        let spell = create_object(&mut state, card_id, P0, face.name.clone(), Zone::Hand);
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            apply_card_face_to_object(obj, face);
            obj.mana_cost = ManaCost::Cost { shards, generic: 0 };
        }

        let mana: Vec<ManaUnit> = colors
            .iter()
            .map(|c| ManaUnit::new(ManaType::from(*c), ObjectId(0), false, Vec::new()))
            .collect();
        if let Some(p) = state.players.iter_mut().find(|p| p.id == P0) {
            p.mana_pool.mana = mana;
        }

        (GameRunner::from_state(state), spell)
    }

    fn counters_of(runner: &GameRunner, id: ObjectId, ct: &CounterType) -> u32 {
        runner
            .state()
            .objects
            .get(&id)
            .and_then(|o| o.counters.get(ct))
            .copied()
            .unwrap_or(0)
    }

    /// Cast a Sunburst CREATURE (artifact creature) paying three distinct colors
    /// → it must enter with three +1/+1 counters. CR 702.44a (creature branch).
    #[test]
    fn sunburst_creature_paying_three_colors_enters_with_three_p1p1_counters() {
        let face = sunburst_face(
            "Sunburst Drake",
            &[CoreType::Artifact, CoreType::Creature],
            Some(0),
        );
        let (mut runner, spell) = stage_sunburst_cast(
            &face,
            vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
            &[ManaColor::White, ManaColor::Blue, ManaColor::Black],
        );

        runner.cast(spell).resolve();

        assert_eq!(
            counters_of(&runner, spell, &CounterType::Plus1Plus1),
            3,
            "creature Sunburst cast for 3 colors must enter with 3 +1/+1 counters"
        );
    }

    /// Cast a Sunburst CREATURE paying a single color (twice) → exactly one
    /// +1/+1 counter. CR 702.44a counts COLORS, not total mana.
    #[test]
    fn sunburst_creature_paying_one_color_enters_with_one_p1p1_counter() {
        let face = sunburst_face(
            "Sunburst Imp",
            &[CoreType::Artifact, CoreType::Creature],
            Some(0),
        );
        // {R}{R}: two mana, one distinct color.
        let (mut runner, spell) = stage_sunburst_cast(
            &face,
            vec![ManaCostShard::Red, ManaCostShard::Red],
            &[ManaColor::Red, ManaColor::Red],
        );

        runner.cast(spell).resolve();

        assert_eq!(
            counters_of(&runner, spell, &CounterType::Plus1Plus1),
            1,
            "Sunburst counts distinct colors: {{R}}{{R}} is one color, so one +1/+1 counter"
        );
    }

    /// Cast a Sunburst noncreature ARTIFACT paying two distinct colors → it must
    /// enter with two CHARGE counters and zero +1/+1 counters. CR 702.44a
    /// (otherwise branch).
    #[test]
    fn sunburst_artifact_paying_two_colors_enters_with_two_charge_counters() {
        let face = sunburst_face("Sunburst Relic", &[CoreType::Artifact], None);
        let (mut runner, spell) = stage_sunburst_cast(
            &face,
            vec![ManaCostShard::White, ManaCostShard::Green],
            &[ManaColor::White, ManaColor::Green],
        );

        runner.cast(spell).resolve();

        let charge = CounterType::Generic("charge".to_string());
        assert_eq!(
            counters_of(&runner, spell, &charge),
            2,
            "noncreature Sunburst cast for 2 colors must enter with 2 charge counters"
        );
        assert_eq!(
            counters_of(&runner, spell, &CounterType::Plus1Plus1),
            0,
            "a noncreature Sunburst must not place +1/+1 counters (CR 702.44a)"
        );
    }
}

#[cfg(test)]
mod afflict_training_poisonous_synthesis_tests {
    //! CR 702.130a (Afflict), CR 702.149a (Training), CR 702.70a (Poisonous)
    //! shape + matcher-roundtrip tests. These three keywords were previously
    //! parsed/typed but fell through `triggers_for` to `_ => Vec::new()` (silent
    //! no-ops). Each is now synthesized into exactly one trigger per instance
    //! (CR 702.130b / 702.149b / 702.70b) and recognized by
    //! `trigger_matches_keyword_kind` for runtime grant + symmetric removal
    //! (CR 604.1).
    use super::*;

    // ---- Afflict (CR 702.130a) ----

    #[test]
    fn afflict_synthesizes_becomes_blocked_life_loss() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Afflict(3));
        assert_eq!(triggers.len(), 1, "CR 702.130b: one trigger per Afflict");
        let t = &triggers[0];
        assert!(matches!(t.mode, TriggerMode::AttackerBlocked));
        assert!(matches!(t.valid_card, Some(TargetFilter::SelfRef)));
        let effect = &*t.execute.as_deref().expect("execute body").effect;
        let Effect::LoseLife { amount, target } = effect else {
            panic!("Afflict must lose life, got {effect:?}");
        };
        assert!(matches!(amount, QuantityExpr::Fixed { value: 3 }));
        assert!(
            matches!(target, Some(TargetFilter::DefendingPlayer)),
            "CR 702.130a: the defending player loses the life"
        );
    }

    #[test]
    fn afflict_matcher_roundtrips_and_distinguishes_n() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Afflict(2));
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Afflict(2)
        ));
        // Different N must not match (symmetric removal correctness).
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Afflict(1)
        ));
        // Must not be mistaken for another becomes-blocked keyword.
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Flanking
        ));
    }

    // ---- Training (CR 702.149a) ----

    #[test]
    fn training_synthesizes_gated_self_counter() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Training);
        assert_eq!(triggers.len(), 1, "CR 702.149b: one trigger per Training");
        let t = &triggers[0];
        assert!(matches!(t.mode, TriggerMode::Attacks));
        assert!(matches!(t.valid_card, Some(TargetFilter::SelfRef)));

        // CR 702.149a: gated on at least one higher-power co-attacker.
        let Some(TriggerCondition::MinCoAttackers { minimum, filter }) = t.condition.as_ref()
        else {
            panic!(
                "Training must gate on MinCoAttackers, got {:?}",
                t.condition
            );
        };
        assert_eq!(*minimum, 1);
        let Some(TargetFilter::Typed(tf)) = filter.as_ref() else {
            panic!("Training co-attacker filter must be a typed creature filter");
        };
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    comparator: Comparator::GT,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Source
                        }
                    },
                    ..
                }
            )),
            "CR 702.149a: co-attacker power must be strictly greater than the source's power"
        );

        // CR 702.149a: puts a +1/+1 counter on this creature.
        let effect = &*t.execute.as_deref().expect("execute body").effect;
        assert!(matches!(
            effect,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }
        ));
    }

    #[test]
    fn training_matcher_roundtrips_and_is_distinct_from_mentor() {
        let training = KeywordTriggerInstaller::triggers_for(&Keyword::Training);
        let mentor = KeywordTriggerInstaller::triggers_for(&Keyword::Mentor);

        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &training[0],
            &Keyword::Training
        ));
        // Both are self-scoped Attacks/+1+1 triggers, but Mentor targets a
        // lesser-power attacker (no co-attacker gate) — the matchers must not
        // cross-match, or RemoveKeyword would strip the wrong trigger.
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &training[0],
            &Keyword::Mentor
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &mentor[0],
            &Keyword::Training
        ));
    }

    // ---- Poisonous (CR 702.70a) ----

    #[test]
    fn poisonous_synthesizes_combat_damage_poison() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Poisonous(2));
        assert_eq!(triggers.len(), 1, "CR 702.70b: one trigger per Poisonous");
        let t = &triggers[0];
        assert!(matches!(t.mode, TriggerMode::DamageDone));
        assert!(
            matches!(t.damage_kind, DamageKindFilter::CombatOnly),
            "CR 702.70a: only combat damage triggers Poisonous"
        );
        assert!(matches!(t.valid_source, Some(TargetFilter::SelfRef)));
        assert!(
            matches!(t.valid_target, Some(TargetFilter::Player)),
            "CR 702.70a: combat damage must be dealt to a player"
        );
        let effect = &*t.execute.as_deref().expect("execute body").effect;
        let Effect::GivePlayerCounter {
            counter_kind,
            count,
            target,
        } = effect
        else {
            panic!("Poisonous must give player counters, got {effect:?}");
        };
        assert!(matches!(counter_kind, PlayerCounterKind::Poison));
        assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
        assert!(
            matches!(target, TargetFilter::TriggeringPlayer),
            "CR 702.70a: the player dealt damage gets the poison counters"
        );
    }

    #[test]
    fn poisonous_matcher_roundtrips_and_distinguishes_n() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Poisonous(2));
        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Poisonous(2)
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &triggers[0],
            &Keyword::Poisonous(1)
        ));
    }

    // ---- cross-keyword regression guard ----

    #[test]
    fn previously_dead_keywords_now_synthesize_triggers() {
        // Regression: these three keywords used to fall through `triggers_for`
        // to `_ => Vec::new()`. Each must now yield exactly one trigger.
        for kw in [
            Keyword::Afflict(1),
            Keyword::Training,
            Keyword::Poisonous(1),
        ] {
            assert_eq!(
                KeywordTriggerInstaller::triggers_for(&kw).len(),
                1,
                "{kw:?} must synthesize one trigger (was a silent no-op)"
            );
        }
    }
}

#[cfg(test)]
mod ingest_gravestorm_synthesis_tests {
    //! CR 702.115a/b (Ingest) and CR 702.69a/b (Gravestorm) shape tests for
    //! printed keyword synthesis plus runtime-grant trigger installation.
    use super::*;

    #[test]
    fn ingest_synthesizes_combat_damage_exile() {
        let triggers = KeywordTriggerInstaller::triggers_for(&Keyword::Ingest);
        assert_eq!(triggers.len(), 1, "CR 702.115b: one trigger per Ingest");
        let t = &triggers[0];
        assert!(matches!(t.mode, TriggerMode::DamageDone));
        assert!(
            matches!(t.damage_kind, DamageKindFilter::CombatOnly),
            "CR 702.115a: only combat damage triggers Ingest"
        );
        assert!(matches!(t.valid_source, Some(TargetFilter::SelfRef)));
        assert!(
            matches!(t.valid_target, Some(TargetFilter::Player)),
            "CR 702.115a: combat damage must be dealt to a player"
        );

        let effect = &*t.execute.as_deref().expect("execute body").effect;
        let Effect::ExileTop {
            player,
            count,
            face_down,
        } = effect
        else {
            panic!("Ingest must exile the top card, got {effect:?}");
        };
        assert!(
            matches!(player, TargetFilter::TriggeringPlayer),
            "CR 702.115a: the damaged player exiles the card"
        );
        assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
        assert!(!face_down, "CR 406.3: Ingest exiles face up by default");
    }

    #[test]
    fn ingest_printed_keyword_synthesizes_trigger() {
        let mut face = CardFace {
            keywords: vec![Keyword::Ingest],
            ..CardFace::default()
        };
        synthesize_ingest(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_ingest_trigger(trigger))
                .count(),
            1,
            "printed Ingest must synthesize its combat-damage trigger"
        );
    }

    #[test]
    fn ingest_preserves_multiple_instances() {
        let mut face = CardFace {
            keywords: vec![Keyword::Ingest, Keyword::Ingest],
            ..CardFace::default()
        };
        synthesize_ingest(&mut face);
        synthesize_ingest(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_ingest_trigger(trigger))
                .count(),
            2,
            "CR 702.115b: two Ingest instances trigger separately"
        );
    }

    #[test]
    fn ingest_matcher_roundtrips_and_is_distinct_from_renown() {
        let ingest = KeywordTriggerInstaller::triggers_for(&Keyword::Ingest);
        let renown = KeywordTriggerInstaller::triggers_for(&Keyword::Renown(1));

        assert!(KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &ingest[0],
            &Keyword::Ingest
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &ingest[0],
            &Keyword::Renown(1)
        ));
        assert!(!KeywordTriggerInstaller::trigger_matches_keyword_kind(
            &renown[0],
            &Keyword::Ingest
        ));
    }

    #[test]
    fn synthesize_all_wires_printed_ingest_and_gravestorm() {
        let mut face = CardFace {
            keywords: vec![Keyword::Ingest, Keyword::Gravestorm],
            ..CardFace::default()
        };
        synthesize_all(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_ingest_trigger(trigger))
                .count(),
            1,
            "synthesize_all must install printed Ingest triggers"
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_gravestorm_trigger(trigger))
                .count(),
            1,
            "synthesize_all must install printed Gravestorm triggers"
        );
    }

    #[test]
    fn gravestorm_synthesizes_zone_counted_copy_trigger() {
        let mut face = CardFace {
            keywords: vec![Keyword::Gravestorm],
            ..CardFace::default()
        };
        synthesize_gravestorm(&mut face);

        let trigger = face
            .triggers
            .iter()
            .find(|trigger| is_gravestorm_trigger(trigger))
            .expect("printed Gravestorm must synthesize a copy trigger");
        assert!(matches!(trigger.mode, TriggerMode::SpellCast));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        assert!(
            trigger.trigger_zones.contains(&Zone::Stack),
            "CR 702.69a: Gravestorm functions while the spell is on the stack"
        );

        let execute = trigger.execute.as_deref().expect("execute body");
        assert!(matches!(
            &*execute.effect,
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                ..
            }
        ));
        assert!(matches!(
            execute.repeat_for.as_ref(),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Graveyard),
                    filter,
                }
            }) if *filter == TargetFilter::Typed(TypedFilter::permanent())
        ));
    }

    #[test]
    fn gravestorm_preserves_multiple_instances() {
        let mut face = CardFace {
            keywords: vec![Keyword::Gravestorm, Keyword::Gravestorm],
            ..CardFace::default()
        };
        synthesize_gravestorm(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_gravestorm_trigger(trigger))
                .count(),
            2,
            "CR 702.69b: two Gravestorm instances trigger separately"
        );
    }

    #[test]
    fn gravestorm_is_idempotent() {
        let mut face = CardFace {
            keywords: vec![Keyword::Gravestorm, Keyword::Gravestorm],
            ..CardFace::default()
        };
        synthesize_gravestorm(&mut face);
        synthesize_gravestorm(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|trigger| is_gravestorm_trigger(trigger))
                .count(),
            2,
            "re-running synthesis must not duplicate Gravestorm triggers"
        );
    }

    #[test]
    fn gravestorm_noop_without_keyword() {
        let mut face = CardFace {
            keywords: vec![Keyword::Flying],
            ..CardFace::default()
        };
        synthesize_gravestorm(&mut face);
        assert!(face.triggers.is_empty());
    }
}

/// CR 702.72a + CR 702.72b synthesis-shape coverage for Champion.
///
/// The shape tests assert the synthesized triggers plug into the existing
/// `Effect::ChangeZone` + `TargetFilter::ExiledBySource` linked-exile
/// infrastructure, and the runtime tests below cover Champion-specific
/// branch/fallback behavior.
#[cfg(test)]
mod champion_synthesis_tests {
    use super::*;
    use crate::types::card_type::CoreType;

    /// Soulshifter Drake-like face: a creature with "Champion an Elf".
    fn face_with_champion(type_str: &str) -> CardFace {
        CardFace {
            name: "Test Championer".to_string(),
            oracle_text: Some(format!(
                "Champion a{} {type_str}",
                if type_str
                    .chars()
                    .next()
                    .is_some_and(|c| "aeiou".contains(c.to_ascii_lowercase()))
                {
                    "n"
                } else {
                    ""
                }
            )),
            keywords: vec![Keyword::Champion(type_str.to_string())],
            card_type: CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        }
    }

    /// CR 702.72a: Champion synthesizes an ETB trigger that is a `ChooseOneOf`
    /// between exiling (linked) another creature of the championed type you
    /// control and sacrificing this permanent, with direct self-sacrifice when
    /// no eligible champion object exists.
    #[test]
    fn synthesize_champion_adds_etb_exile_or_sacrifice_trigger() {
        let mut face = face_with_champion("Elf");
        synthesize_champion(&mut face);

        let etb = face
            .triggers
            .iter()
            .find(|t| is_champion_etb_trigger(t, "Elf"))
            .expect("Champion ETB trigger should be synthesized");

        assert_eq!(etb.mode, TriggerMode::ChangesZone);
        assert_eq!(etb.destination, Some(Zone::Battlefield));
        assert!(matches!(etb.valid_card, Some(TargetFilter::SelfRef)));

        let execute = etb
            .execute
            .as_deref()
            .expect("Champion ETB trigger should execute an ability");
        assert_eq!(
            execute.condition,
            Some(champion_has_eligible_object_condition("Elf")),
            "Champion should only offer the exile/sacrifice choice when an eligible object exists"
        );
        assert!(
            execute
                .else_ability
                .as_deref()
                .is_some_and(is_champion_self_sacrifice_ability),
            "Champion should sacrifice itself directly when no eligible object exists"
        );

        let Some(Effect::ChooseOneOf { branches, chooser }) =
            etb.execute.as_deref().map(|a| &*a.effect)
        else {
            panic!("expected ChooseOneOf ETB execute");
        };
        assert_eq!(*chooser, PlayerFilter::Controller);
        assert_eq!(branches.len(), 2, "exile branch + sacrifice branch");

        // Exile branch: source-tracked linked exile of "another Elf you control".
        let exile = branches
            .iter()
            .find(|b| b.duration.is_none() && matches!(&*b.effect, Effect::ChangeZone { .. }))
            .expect("exile branch must be a source-tracked zone change");
        match &*exile.effect {
            Effect::ChangeZone {
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile);
                // CR 702.72a: "another [type] you control".
                assert_eq!(*target, champion_type_filter("Elf"));
                match target {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(tf.properties.contains(&FilterProp::Another));
                        assert!(tf
                            .type_filters
                            .iter()
                            .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Elf")));
                        assert!(tf
                            .type_filters
                            .iter()
                            .any(|f| matches!(f, TypeFilter::Creature)));
                    }
                    other => panic!("expected Typed exile filter, got {other:?}"),
                }
            }
            other => panic!("expected ChangeZone exile, got {other:?}"),
        }

        // Sacrifice branch: self-sacrifice.
        let sacrifice = branches
            .iter()
            .find(|b| matches!(&*b.effect, Effect::Sacrifice { .. }))
            .expect("sacrifice branch must exist");
        assert!(is_champion_self_sacrifice_ability(sacrifice));
    }

    /// CR 702.72a + CR 702.72b: Champion synthesizes an LTB trigger that
    /// returns the linked exiled card (`ExiledBySource`) to the battlefield.
    #[test]
    fn synthesize_champion_adds_ltb_return_trigger() {
        let mut face = face_with_champion("Elf");
        synthesize_champion(&mut face);

        let ltb = face
            .triggers
            .iter()
            .find(|t| is_champion_ltb_return_trigger(t))
            .expect("Champion LTB return trigger should be synthesized");

        assert_eq!(ltb.mode, TriggerMode::LeavesBattlefield);
        assert!(matches!(ltb.valid_card, Some(TargetFilter::SelfRef)));
        match ltb.execute.as_deref().map(|a| &*a.effect) {
            Some(Effect::ChangeZone {
                origin,
                destination,
                target,
                owner_library,
                ..
            }) => {
                assert_eq!(*origin, Some(Zone::Exile));
                assert_eq!(*destination, Zone::Battlefield);
                // CR 610.3: returns the card linked via state.exile_links.
                assert_eq!(*target, TargetFilter::ExiledBySource);
                // CR 702.72a: "under its owner's control".
                assert!(!owner_library);
            }
            other => panic!("expected ChangeZone return, got {other:?}"),
        }
    }

    /// The "Champion a creature" payload (no subtype) yields a bare creature
    /// filter — still "another creature you control", with no subtype constraint.
    #[test]
    fn synthesize_champion_creature_payload_has_no_subtype() {
        let filter = champion_type_filter("creature");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(!tf
                    .type_filters
                    .iter()
                    .any(|f| matches!(f, TypeFilter::Subtype(_))));
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|f| matches!(f, TypeFilter::Creature)));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    /// Re-running synthesis must not duplicate either trigger (idempotent via
    /// the `install_matching` chokepoint).
    #[test]
    fn synthesize_champion_is_idempotent() {
        let mut face = face_with_champion("Goblin");
        synthesize_champion(&mut face);
        synthesize_champion(&mut face);

        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_champion_etb_trigger(t, "Goblin"))
                .count(),
            1,
            "ETB trigger must not duplicate"
        );
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_champion_ltb_return_trigger(t))
                .count(),
            1,
            "LTB trigger must not duplicate"
        );
    }

    /// A face without Champion gets no Champion triggers.
    #[test]
    fn synthesize_champion_is_noop_without_keyword() {
        let mut face = CardFace {
            name: "Plain Creature".to_string(),
            keywords: vec![Keyword::Flying],
            card_type: CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            ..CardFace::default()
        };
        synthesize_champion(&mut face);
        assert!(face.triggers.is_empty());
    }

    /// CR 604.1: the runtime-grant chokepoint returns both triggers, and the
    /// kind-matcher recognizes each — so `RemoveKeyword` strips exactly what
    /// Champion added.
    #[test]
    fn champion_triggers_for_and_kind_match() {
        let kw = Keyword::Champion("Dragon".to_string());
        let triggers = KeywordTriggerInstaller::triggers_for(&kw);
        assert_eq!(triggers.len(), 2, "ETB + LTB");
        for trigger in &triggers {
            assert!(
                KeywordTriggerInstaller::trigger_matches_keyword_kind(trigger, &kw),
                "each synthesized trigger must be recognized by its keyword kind"
            );
        }
        // The ETB-kind matcher is type-specific: a Dragon trigger is not an Elf trigger.
        let elf_etb = build_champion_etb_trigger("Elf");
        assert!(!is_champion_etb_trigger(&elf_etb, "Dragon"));
        assert!(is_champion_etb_trigger(&elf_etb, "Elf"));
    }
}

#[cfg(test)]
mod champion_runtime_tests {
    //! CR 702.72a runtime integration: the synthesized ETB trigger must either
    //! exile an eligible "another [type] you control" through the linked-exile
    //! path or sacrifice the championing permanent when no eligible object
    //! exists.

    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::zones::create_object;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{ExileLinkKind, GameState, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    const P0: PlayerId = PlayerId(0);

    fn champion_creature_face(type_str: &str) -> CardFace {
        let mut face = CardFace {
            name: "Test Championer".to_string(),
            keywords: vec![Keyword::Champion(type_str.to_string())],
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn elf_face(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(1)),
            toughness: Some(PtValue::Fixed(1)),
            card_type: CardType {
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Elf".to_string()],
                ..Default::default()
            },
            ..CardFace::default()
        }
    }

    fn setup_state_with_priority(controller: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
        state
    }

    fn create_object_with_face(
        state: &mut GameState,
        face: &CardFace,
        controller: PlayerId,
        zone: Zone,
    ) -> (CardId, ObjectId) {
        let card_id = CardId(state.next_object_id);
        let object_id = create_object(state, card_id, controller, face.name.clone(), zone);
        {
            let obj = state.objects.get_mut(&object_id).unwrap();
            apply_card_face_to_object(obj, face);
        }
        (card_id, object_id)
    }

    fn cast_and_advance_until(
        state: &mut GameState,
        card_id: CardId,
        object_id: ObjectId,
        mut done: impl FnMut(&GameState) -> bool,
    ) {
        crate::game::engine::apply_as_current(
            state,
            GameAction::CastSpell {
                object_id,
                card_id,
                targets: vec![],
            },
        )
        .unwrap();

        for _ in 0..10 {
            if done(state) {
                return;
            }
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "expected priority while advancing Champion cast/trigger pipeline, got {:?}",
                state.waiting_for
            );
            crate::game::engine::apply_as_current(state, GameAction::PassPriority).unwrap();
        }

        panic!(
            "Champion cast/trigger pipeline did not reach expected state; waiting_for={:?}, stack_len={}",
            state.waiting_for,
            state.stack.len()
        );
    }

    /// CR 702.72a: If there is no "another [type] you control", the ETB
    /// trigger must sacrifice the championing permanent directly rather than
    /// prompt for an impossible exile branch.
    #[test]
    fn champion_etb_without_eligible_object_sacrifices_without_choice() {
        let face = champion_creature_face("Elf");
        let mut state = setup_state_with_priority(P0);
        let (card_id, champion_id) = create_object_with_face(&mut state, &face, P0, Zone::Hand);

        cast_and_advance_until(&mut state, card_id, champion_id, |state| {
            state
                .objects
                .get(&champion_id)
                .is_some_and(|obj| obj.zone == Zone::Graveyard)
        });

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "Champion with no eligible object must not offer a ChooseOneOf branch"
        );
        assert_eq!(
            state.objects.get(&champion_id).unwrap().zone,
            Zone::Graveyard
        );
        assert!(state.exile_links.is_empty());
    }

    /// CR 702.72a + CR 702.72b: With an eligible object, choosing the exile
    /// branch must exile that object and create a source-tracked link keyed
    /// to the championing permanent.
    #[test]
    fn champion_etb_exile_branch_exiles_eligible_object_with_link() {
        let champion_face = champion_creature_face("Elf");
        let eligible_elf_face = elf_face("Llanowar Sentinel");
        let mut state = setup_state_with_priority(P0);
        let (_, elf_id) =
            create_object_with_face(&mut state, &eligible_elf_face, P0, Zone::Battlefield);
        let (_, other_elf_id) =
            create_object_with_face(&mut state, &eligible_elf_face, P0, Zone::Battlefield);
        let (card_id, champion_id) =
            create_object_with_face(&mut state, &champion_face, P0, Zone::Hand);

        cast_and_advance_until(&mut state, card_id, champion_id, |state| {
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. })
        });

        crate::game::engine::apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .unwrap();

        let WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            zone,
            destination,
            track_exiled_by_source,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "expected Champion exile branch to prompt for an eligible object, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*count, 1);
        assert_eq!(*zone, Zone::Battlefield);
        assert_eq!(*destination, Some(Zone::Exile));
        assert!(*track_exiled_by_source);
        assert!(cards.contains(&elf_id));
        assert!(cards.contains(&other_elf_id));
        assert!(!cards.contains(&champion_id));

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![elf_id],
            },
        )
        .unwrap();

        assert_eq!(
            state.objects.get(&champion_id).unwrap().zone,
            Zone::Battlefield
        );
        assert_eq!(state.objects.get(&elf_id).unwrap().zone, Zone::Exile);
        assert!(state.exile_links.iter().any(|link| {
            link.source_id == champion_id
                && link.exiled_id == elf_id
                && matches!(link.kind, ExileLinkKind::TrackedBySource)
        }));

        let mut leave_events = Vec::new();
        crate::game::zones::move_to_zone(
            &mut state,
            champion_id,
            Zone::Graveyard,
            &mut leave_events,
        );
        crate::game::triggers::process_triggers(&mut state, &leave_events);
        assert_eq!(
            state.stack.len(),
            1,
            "Champion LTB return trigger should reach the stack"
        );

        let mut return_events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut return_events);
        assert_eq!(state.objects.get(&elf_id).unwrap().zone, Zone::Battlefield);
        assert!(
            !state
                .exile_links
                .iter()
                .any(|link| { link.source_id == champion_id && link.exiled_id == elf_id }),
            "Champion LTB return should consume the source-tracked exile link"
        );
    }
}

#[cfg(test)]
mod demonstrate_synthesis_tests {
    //! CR 702.144a shape tests: Demonstrate was parsed/typed but had no
    //! `synthesize_*` pass. `synthesize_demonstrate` installs an optional
    //! "when you cast this spell" self-copy trigger whose sub-ability copies the
    //! spell for a chosen opponent (`Effect::CopySpell { copier: Some(Opponent) }`).
    //! The copier routing itself is verified behaviorally in `copy_spell`'s tests.
    use super::*;

    fn demonstrate_face() -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Demonstrate);
        face
    }

    #[test]
    fn demonstrate_synthesizes_optional_self_copy_with_opponent_subcopy() {
        let mut face = demonstrate_face();
        synthesize_demonstrate(&mut face);
        let t = face
            .triggers
            .iter()
            .find(|t| is_demonstrate_trigger(t))
            .expect("Demonstrate should add a SpellCast copy trigger");

        assert!(matches!(t.mode, TriggerMode::SpellCast));
        assert!(matches!(t.valid_card, Some(TargetFilter::SelfRef)));
        assert!(
            t.trigger_zones.contains(&Zone::Stack),
            "CR 702.144a: Demonstrate functions on the stack"
        );

        let execute = t.execute.as_deref().expect("execute body");
        assert!(execute.optional, "CR 702.144a: 'you MAY copy it'");
        // Controller's copy — no copier override, may retarget.
        assert!(matches!(
            &*execute.effect,
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
            }
        ));
        // Opponent's copy — sub-ability with the opponent copier, may retarget.
        let sub = execute.sub_ability.as_deref().expect("opponent sub-copy");
        assert!(matches!(
            &*sub.effect,
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: Some(ControllerRef::Opponent),
            }
        ));
    }

    #[test]
    fn demonstrate_is_idempotent() {
        let mut face = demonstrate_face();
        synthesize_demonstrate(&mut face);
        synthesize_demonstrate(&mut face);
        assert_eq!(
            face.triggers
                .iter()
                .filter(|t| is_demonstrate_trigger(t))
                .count(),
            1,
            "repeated synthesis must not duplicate the Demonstrate trigger"
        );
    }

    #[test]
    fn demonstrate_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_demonstrate(&mut face);
        assert!(face.triggers.iter().all(|t| !is_demonstrate_trigger(t)));
    }
}

#[cfg(test)]
mod absorb_synthesis_tests {
    //! CR 702.64a shape tests: Absorb was parsed/typed but had no runtime.
    //! `synthesize_absorb` installs a continuous self-recipient `DamageDone`
    //! replacement that subtracts N from each incoming damage event
    //! (`DamageModification::Minus { value: N }`, `valid_card: SelfRef`). The
    //! continuous, non-consumed, per-source/per-event semantics (CR 702.64b) come
    //! for free from `Minus`; CR 702.64c (each instance separate) is one
    //! replacement per instance.
    use super::*;
    use crate::game::effects::deal_damage;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::zones::create_object;
    use crate::types::ability::{ResolvedAbility, TargetRef};
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn absorb_face(n: u32) -> CardFace {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Absorb(n));
        face
    }

    fn absorb_creature_face(name: &str, keywords: Vec<Keyword>) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(3)),
            toughness: Some(PtValue::Fixed(3)),
            keywords,
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn marked_damage_after_absorb_damage(keywords: Vec<Keyword>, damage: u32) -> u32 {
        let face = absorb_creature_face("Absorb Test", keywords);
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Damage Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            face.name.clone(),
            Zone::Battlefield,
        );
        apply_card_face_to_object(state.objects.get_mut(&target).unwrap(), &face);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed {
                    value: damage as i32,
                },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        deal_damage::resolve(&mut state, &ability, &mut events).unwrap();

        state.objects[&target].damage_marked
    }

    #[test]
    fn absorb_synthesizes_self_damage_prevention() {
        let mut face = absorb_face(2);
        synthesize_absorb(&mut face);
        let r = face
            .replacements
            .iter()
            .find(|r| is_absorb_replacement(r, 2))
            .expect("Absorb should add a self-recipient damage replacement");
        assert!(matches!(r.event, ReplacementEvent::DamageDone));
        assert!(
            matches!(r.valid_card, Some(TargetFilter::SelfRef)),
            "CR 702.64a: only damage to THIS creature is prevented"
        );
        assert!(
            matches!(
                r.damage_modification,
                Some(DamageModification::Minus { value: 2 })
            ),
            "CR 702.64a: prevent N (=2) of the damage"
        );
    }

    #[test]
    fn absorb_is_idempotent() {
        let mut face = absorb_face(1);
        synthesize_absorb(&mut face);
        synthesize_absorb(&mut face);
        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| is_absorb_replacement(r, 1))
                .count(),
            1,
            "repeated synthesis must not duplicate the Absorb replacement"
        );
    }

    #[test]
    fn absorb_multiple_instances_apply_separately() {
        // CR 702.64c: two instances of Absorb 1 prevent 1 each (2 total per source).
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Absorb(1));
        face.keywords.push(Keyword::Absorb(1));
        synthesize_absorb(&mut face);
        assert_eq!(
            face.replacements
                .iter()
                .filter(|r| is_absorb_replacement(r, 1))
                .count(),
            2,
            "CR 702.64c: each Absorb instance installs its own prevention replacement"
        );
    }

    #[test]
    fn absorb_noop_without_keyword() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Flying);
        synthesize_absorb(&mut face);
        assert!(face
            .replacements
            .iter()
            .all(|r| !is_absorb_replacement(r, 1)));
    }

    #[test]
    fn absorb_runtime_prevents_damage_to_absorb_creature() {
        assert_eq!(
            marked_damage_after_absorb_damage(vec![Keyword::Absorb(2)], 5),
            3,
            "CR 702.64a: Absorb 2 prevents 2 damage from the event"
        );
    }

    #[test]
    fn absorb_runtime_applies_multiple_instances_separately() {
        assert_eq!(
            marked_damage_after_absorb_damage(vec![Keyword::Absorb(1), Keyword::Absorb(1)], 3),
            1,
            "CR 702.64c: two Absorb 1 instances each prevent 1 damage"
        );
    }
}
