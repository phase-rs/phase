//! Tests relocated from `src/types/ability.rs`'s test module.

use engine::types::ability::{
    default_mana_production, CombineSource, ControllerRef, EachDamageRecipient, Effect,
    EffectScope, ExtraPhaseAnchor, ExtraPhaseRecipient, ManaTargetRole, PerPlayerScope,
    PerpetualModification, PtValue, QuantityExpr, TapStateChange, TargetFilter, TypedFilter,
    ZoneOwner,
};
use engine::types::phase::{PhaseGroup, TurnSegment};
use engine::types::zones::Zone;
use syn::__private::ToTokens;

/// Flatten `Pat::Or` recursively and unwrap `Pat::Paren`, yielding the
/// top-level alternatives of one arm pattern.
fn arm_alternatives(pat: &syn::Pat, out: &mut Vec<syn::Pat>) {
    match pat {
        syn::Pat::Or(or) => or.cases.iter().for_each(|c| arm_alternatives(c, out)),
        syn::Pat::Paren(p) => arm_alternatives(&p.pat, out),
        other => out.push(other.clone()),
    }
}

/// Render a `syn::Pat` for comparison and messages without pulling in `quote`.
fn quote_pat(pat: &syn::Pat) -> String {
    pat.to_token_stream().to_string()
}

/// The single top-level `match` expression of a method body.
fn sole_top_level_match(f: &syn::ImplItemFn) -> &syn::ExprMatch {
    let matches: Vec<&syn::ExprMatch> = f
        .block
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            syn::Stmt::Expr(syn::Expr::Match(m), _) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "`Effect::{}`'s body must contain exactly one top-level `match`, or this \
             row reads the wrong one (or none); found {}",
        f.sig.ident,
        matches.len()
    );
    matches[0]
}

/// Every arm's pattern, rendered, in order.
fn arm_patterns(m: &syn::ExprMatch) -> Vec<String> {
    m.arms.iter().map(|arm| quote_pat(&arm.pat)).collect()
}

/// Whether an arm's body is the literal `None`.
fn arm_body_is_none(arm: &syn::Arm) -> bool {
    matches!(arm.body.as_ref(), syn::Expr::Path(p) if p.qself.is_none() && p.path.is_ident("None"))
}

/// Every top-level alternative of every arm.
fn match_alternatives(m: &syn::ExprMatch) -> Vec<syn::Pat> {
    let mut alternatives = Vec::new();
    m.arms
        .iter()
        .for_each(|arm| arm_alternatives(&arm.pat, &mut alternatives));
    alternatives
}

/// Whether some top-level alternative of `m` names `Effect::<variant>`.
fn names_effect_variant(m: &syn::ExprMatch, variant: &str) -> bool {
    match_alternatives(m).iter().any(|alternative| {
        let path = match alternative {
            syn::Pat::Path(p) => &p.path,
            syn::Pat::TupleStruct(p) => &p.path,
            syn::Pat::Struct(p) => &p.path,
            _ => return false,
        };
        path.segments.len() == 2
            && path.segments[0].ident == "Effect"
            && path.segments[1].ident == variant
    })
}

/// The patterns of the arms whose bodies are `None` in exactly one of the two
/// matches, with the side that returns `None` (`true` = the twin).
fn none_class_mismatches(shared: &syn::ExprMatch, twin: &syn::ExprMatch) -> Vec<(String, bool)> {
    shared
        .arms
        .iter()
        .zip(&twin.arms)
        .filter(|(s, t)| arm_body_is_none(s) != arm_body_is_none(t))
        .map(|(s, t)| (quote_pat(&s.pat), arm_body_is_none(t)))
        .collect()
}

fn assert_twin_addresses_the_same_field(mut effect: Effect) {
    let shared = effect
        .target_filter()
        .map(|filter| filter as *const TargetFilter);
    let twin = effect
        .target_filter_mut()
        .map(|filter| &*filter as *const TargetFilter);
    assert!(
        shared.is_some(),
        "sample must surface an operand: {effect:?}"
    );
    assert_eq!(
        shared, twin,
        "the twin must address the same field: {effect:?}"
    );
}

fn assert_neither_surfaces_an_operand(mut effect: Effect) {
    assert!(effect.target_filter().is_none(), "{effect:?}");
    assert!(effect.target_filter_mut().is_none(), "{effect:?}");
}

/// `target_filter` returns a promoted constant, not a field: the twin has no
/// field to address.
fn assert_constant_arm(mut effect: Effect, constant: &TargetFilter) {
    assert_eq!(effect.target_filter(), Some(constant), "{effect:?}");
    assert!(effect.target_filter_mut().is_none(), "{effect:?}");
}

/// `Effect::target_filter_mut` is `Effect::target_filter`'s mutable twin:
/// the same arms in the same order with the same patterns (structural half,
/// over `phase-ast`'s `types/ability.rs` source), each addressing the same field (behavioural
/// half, by pointer equality). A variant moved between one function's
/// `Some` and `None` sides fails the structural half; an arm whose twin
/// addresses another field fails the behavioural half on its sample.
#[test]
fn target_filter_mut_is_target_filters_twin() {
    // Routed through the repo's single comment-policy authority
    // (`crate::source_census`), per `no_source_reading_file_carries_a_private_comment_policy`.
    let stripped =
        crate::source_census::code_lines(include_str!("../../../phase-ast/src/types/ability.rs"));
    let file = syn::parse_file(&stripped)
        .expect("this file must parse as Rust for the structural half to mean anything");
    let effect_methods: Vec<&syn::ImplItemFn> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(imp)
                if imp.trait_.is_none()
                    && matches!(imp.self_ty.as_ref(), syn::Type::Path(p) if p.path.is_ident("Effect")) =>
            {
                Some(imp)
            }
            _ => None,
        })
        .flat_map(|imp| imp.items.iter())
        .filter_map(|item| match item {
            syn::ImplItem::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    let method = |name: &str| {
        let found: Vec<&&syn::ImplItemFn> = effect_methods
            .iter()
            .filter(|f| f.sig.ident == name)
            .collect();
        assert_eq!(found.len(), 1, "exactly one `Effect::{name}` must exist");
        sole_top_level_match(found[0])
    };
    let shared = method("target_filter");
    let twin = method("target_filter_mut");

    // Reach: the row reads the real accessors.
    for (name, m) in [("target_filter", shared), ("target_filter_mut", twin)] {
        assert!(
            m.arms.len() >= 30,
            "`{name}` must have at least 30 arms; got {}",
            m.arms.len()
        );
        let alternatives = match_alternatives(m).len();
        assert!(
            alternatives >= 200,
            "`{name}`'s arms must have at least 200 top-level alternatives; got {alternatives}"
        );
        for variant in ["Goad", "SetTapState"] {
            assert!(
                names_effect_variant(m, variant),
                "`{name}` must name `Effect::{variant}` as an alternative"
            );
        }
    }

    // Same arms, pattern for pattern.
    assert_eq!(arm_patterns(shared), arm_patterns(twin));

    // Same `None` class on every arm except the constant arm with no
    // field-shaped twin: `ChooseFromZone`'s targeted-players iteration.
    let mismatches = none_class_mismatches(shared, twin);
    assert_eq!(
        mismatches.len(),
        1,
        "exactly one arm may be `None` in the twin only; got {mismatches:?}"
    );
    let (pattern, twin_is_none) = &mismatches[0];
    assert!(
        *twin_is_none && pattern.contains("ChooseFromZone") && pattern.contains("TargetedPlayers"),
        "the one exception must be `ChooseFromZone`'s targeted-players arm, `None` in the \
             twin; got {mismatches:?}"
    );

    // The instrument fires: moving one variant from the `Some` alternation to
    // the `None` alternation changes the compared pattern lists.
    let before: syn::ExprMatch = syn::parse_str(
        "match self { Effect::Goad { target, .. } | Effect::Draw { target, .. } => Some(target), \
             Effect::Myriad => None }",
    )
    .expect("the control match must parse");
    let after: syn::ExprMatch = syn::parse_str(
        "match self { Effect::Draw { target, .. } => Some(target), \
             Effect::Goad { .. } | Effect::Myriad => None }",
    )
    .expect("the control match must parse");
    assert_ne!(arm_patterns(&before), arm_patterns(&after));

    // Behavioural half: one sample per non-plain arm shape.
    let creature = || TargetFilter::Typed(TypedFilter::creature());
    let context_ref = || TargetFilter::ParentTarget;
    let fixed_one = || QuantityExpr::Fixed { value: 1 };
    let copy_token_of = |source_filter: Option<TargetFilter>| Effect::CopyTokenOf {
        target: creature(),
        owner: TargetFilter::Controller,
        source_filter,
        enters_attacking: false,
        attack_destination: engine::types::ability::TokenAttackDestination::ControllerChooses,
        tapped: false,
        count: fixed_one(),
        extra_keywords: Vec::new(),
        additional_modifications: Vec::new(),
    };
    let token = |attach_to: Option<TargetFilter>| Effect::Token {
        name: "Spirit".to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string()],
        colors: Vec::new(),
        keywords: Vec::new(),
        tapped: false,
        count: fixed_one(),
        owner: TargetFilter::Controller,
        attach_to,
        enters_attacking: false,
        attack_destination: engine::types::ability::TokenAttackDestination::ControllerChooses,
        supertypes: Vec::new(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
    };
    let apply_perpetual = |target: TargetFilter| Effect::ApplyPerpetual {
        target,
        modification: PerpetualModification::ModifyPowerToughness {
            power: QuantityExpr::Fixed { value: 1 },
            toughness: QuantityExpr::Fixed { value: 1 },
            keywords: Vec::new(),
        },
    };
    let reveal_top = |player: TargetFilter| Effect::RevealTop { player, count: 1 };
    let reveal_until = |player: TargetFilter| Effect::RevealUntil {
        player,
        filter: creature(),
        count: fixed_one(),
        matched_disposition: Default::default(),
        kept_destination: Zone::Hand,
        rest_destination: Zone::Graveyard,
        rest_order: Default::default(),
        enter_tapped: Default::default(),
        enters_attacking: false,
        kept_optional_to: None,
        enters_under: None,
        kept_destination_if: None,
    };
    let search_library = |target_player: Option<TargetFilter>| Effect::SearchLibrary {
        source_zones: vec![Zone::Library],
        filter: creature(),
        count: fixed_one(),
        reveal: false,
        target_player,
        selection_constraint: Default::default(),
        split: None,
    };
    let mana = |target: ManaTargetRole| Effect::Mana {
        produced: default_mana_production(),
        restrictions: Vec::new(),
        grants: Vec::new(),
        expiry: None,
        target: Some(target),
    };
    let additional_phase = |recipient: ExtraPhaseRecipient| Effect::AdditionalPhase {
        recipient,
        segment: TurnSegment::Phase(PhaseGroup::Combat),
        after: ExtraPhaseAnchor::ThisPhase { named: None },
        followed_by: Vec::new(),
        count: fixed_one(),
        attacker_restriction: None,
    };
    let set_tap_state = |scope: EffectScope| Effect::SetTapState {
        target: creature(),
        scope,
        state: TapStateChange::Tap,
    };
    let generic_effect = |target: Option<TargetFilter>| Effect::GenericEffect {
        static_abilities: Vec::new(),
        duration: None,
        target,
        end_cost: None,
    };

    // Conditional arms, both branches each.
    assert_twin_addresses_the_same_field(copy_token_of(None));
    assert_twin_addresses_the_same_field(copy_token_of(Some(creature())));
    assert_twin_addresses_the_same_field(token(Some(creature())));
    assert_twin_addresses_the_same_field(token(None));
    assert_twin_addresses_the_same_field(apply_perpetual(creature()));
    assert_neither_surfaces_an_operand(apply_perpetual(TargetFilter::Any));
    assert_twin_addresses_the_same_field(reveal_top(TargetFilter::Player));
    assert_neither_surfaces_an_operand(reveal_top(TargetFilter::Controller));
    assert_twin_addresses_the_same_field(reveal_until(TargetFilter::Player));
    assert_neither_surfaces_an_operand(reveal_until(context_ref()));
    assert_neither_surfaces_an_operand(search_library(Some(TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::ParentTargetOwner),
        ..TypedFilter::default()
    }))));
    assert_twin_addresses_the_same_field(search_library(Some(TargetFilter::Player)));
    assert_neither_surfaces_an_operand(search_library(None));

    // `Mana`: the first declared role, by role shape.
    assert_twin_addresses_the_same_field(mana(ManaTargetRole::Recipient {
        recipient: TargetFilter::Player,
    }));
    assert_twin_addresses_the_same_field(mana(ManaTargetRole::CountSource {
        count_source: creature(),
    }));
    assert_twin_addresses_the_same_field(mana(ManaTargetRole::Both {
        recipient: TargetFilter::Player,
        count_source: creature(),
    }));

    // `AdditionalPhase`: a field for a targeted player, a constant otherwise.
    assert_twin_addresses_the_same_field(additional_phase(ExtraPhaseRecipient::TargetedPlayer(
        TargetFilter::Player,
    )));
    assert_constant_arm(
        additional_phase(ExtraPhaseRecipient::Controller),
        &TargetFilter::Controller,
    );

    // Boxed, nested and scope-split fields.
    assert_twin_addresses_the_same_field(Effect::CombineHost {
        source: CombineSource::Source,
        host: Box::new(creature()),
    });
    assert_twin_addresses_the_same_field(Effect::EachSourceDealsDamage {
        sources: creature(),
        amount: fixed_one(),
        recipient: EachDamageRecipient::Shared(TargetFilter::Player),
    });
    assert_twin_addresses_the_same_field(set_tap_state(EffectScope::Single));
    assert_neither_surfaces_an_operand(set_tap_state(EffectScope::All));
    assert_twin_addresses_the_same_field(generic_effect(Some(creature())));
    assert_neither_surfaces_an_operand(generic_effect(None));
    assert_twin_addresses_the_same_field(Effect::Goad { target: creature() });

    // `ChooseFromZone`'s targeted-players iteration: a constant.
    assert_constant_arm(
        Effect::ChooseFromZone {
            count: 1,
            zone: Zone::Hand,
            additional_zones: Vec::new(),
            zone_owner: ZoneOwner::Each(PerPlayerScope::TargetedPlayers),
            filter: None,
            chooser: Default::default(),
            candidate_source: Default::default(),
            reciprocal_role: None,
            gather_role: None,
            up_to: false,
            selection: Default::default(),
            constraint: None,
        },
        &TargetFilter::Player,
    );
}
