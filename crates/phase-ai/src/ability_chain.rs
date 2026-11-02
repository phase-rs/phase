//! Shared helpers for walking an `AbilityDefinition`'s effect tree.
//!
//! `AbilityDefinition` composes a primary `effect` with an optional
//! `sub_ability` that itself has an `effect` plus another optional
//! `sub_ability`, forming a single-linked list. Feature detectors and
//! policies both need to classify the *set* of effects produced by an
//! ability (e.g., "does this ability both search the library and put a
//! land onto the battlefield?"), so they collect the chain into a flat
//! slice and iterate with `matches!`.
//!
//! Two branches of that definition are *conditional* rather than part of the
//! unconditional chain: `else_ability` (the CR 608.2c "Otherwise, ..." leg) and
//! `mode_abilities` (the CR 700.2 modal alternatives). Whether they belong in
//! the walk depends on the question being asked, which is what [`AbilityScope`]
//! names — see its docs. [`collect_scoped_effects`] is the single authority for
//! both walks; `collect_chain_effects` is the unconditional shorthand.
//!
//! A created-token placement is the one cross-effect reading this module owns:
//! a `PutCounter { target: LastCreated }` whose nearest preceding publisher of
//! the created referent (`Effect::publishes_created_referent`, the engine's
//! classification) in the same unconditional branch chain is an
//! `Effect::Token` puts its counters on the token(s) that `Token` just created,
//! so a consumer reads it as part of that token's creation, the way it reads
//! `Token { enter_with_counters }`.
//! [`created_token_placement_source`] answers the pairing question for one
//! chain, and [`AbilityWalk::created_token_pairs`] collects the pairs of every
//! branch chain of an ability: the root chain, and each `else_ability` /
//! mode branch head's own chain. The filtering view
//! ([`AbilityWalk::without_created_token_placements`]) and X attribution share
//! those pairs. Pairing never crosses abilities or chains: a `Token` in one
//! branch never pairs with a placement in another, or in the root.
//!
//! Keep this module small — it is a single building block shared across
//! `features/*` and `policies/*`.

use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};

/// Which part of an ability tree a classification question is asking about.
///
/// The distinction is a decision-boundary one, not a convenience one:
///
/// * [`AbilityScope::Unconditional`] answers *"does resolving this ability as
///   already announced produce effect X?"* — the walk a LIVE per-action policy
///   needs. CR 601.2b makes mode selection a distinct step of announcing a
///   spell, so at `CastSpell` time no mode has been chosen yet and a modal
///   branch must not be credited to the cast.
/// * [`AbilityScope::Potential`] answers *"can this card ever produce effect
///   X?"* — the walk DECK-TIME detection needs, where every branch the card
///   could take is in scope.
///
/// A typed scope rather than a `bool` so each call site states which question
/// it is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbilityScope {
    /// `effect` plus the `sub_ability` chain — everything that happens no
    /// matter which branch or mode is taken.
    Unconditional,
    /// The full tree: `Unconditional` plus the `else_ability` branch (CR
    /// 608.2c) and every entry of `mode_abilities` (CR 700.2), recursively.
    Potential,
}

/// Walk an ability tree at `scope`, returning borrowed effect references in
/// traversal order.
///
/// This is the mode-aware authority both deck-time detection and live policy
/// classification share; they differ only in the `scope` they pass, so the two
/// can never drift apart on which branches exist.
pub(crate) fn collect_scoped_effects(
    ability: &AbilityDefinition,
    scope: AbilityScope,
) -> Vec<&Effect> {
    let mut effects: Vec<&Effect> = Vec::new();
    push_scoped_effects(ability, scope, &mut effects);
    effects
}

fn push_scoped_effects<'a>(
    ability: &'a AbilityDefinition,
    scope: AbilityScope,
    out: &mut Vec<&'a Effect>,
) {
    out.push(&ability.effect);
    if let Some(sub) = &ability.sub_ability {
        push_scoped_effects(sub, scope, out);
    }
    if scope == AbilityScope::Unconditional {
        return;
    }
    // CR 608.2c: the "Otherwise, ..." leg is one of two mutually exclusive
    // outcomes, so it is potential rather than unconditional.
    if let Some(other) = &ability.else_ability {
        push_scoped_effects(other, scope, out);
    }
    // CR 700.2: each mode is an alternative the controller may choose.
    for mode in &ability.mode_abilities {
        push_scoped_effects(mode, scope, out);
    }
}

/// Walk `ability.effect` plus each `sub_ability.effect` in turn, returning
/// borrowed references in chain order. Shorthand for
/// [`collect_scoped_effects`] at [`AbilityScope::Unconditional`].
pub(crate) fn collect_chain_effects(ability: &AbilityDefinition) -> Vec<&Effect> {
    collect_scoped_effects(ability, AbilityScope::Unconditional)
}

/// CR 608.2c + CR 111.1: an ability's instructions resolve in order, and each
/// publisher of the created referent assigns it, so a
/// `PutCounter { target: LastCreated }` puts its counters on what the NEAREST
/// preceding publisher in the same unconditional chain produced (CR 122.6:
/// counters put on an object after it entered). The publisher set is the
/// engine's (`Effect::publishes_created_referent`). Returns that publisher's
/// chain index only when it is an `Effect::Token`. Counter-type agnostic.
///
/// The pairing gives the imperative "create … then put counters on it" the
/// reading of the declarative `Token { enter_with_counters }`. `CopyTokenOf`
/// and `Populate` publish tokens too but have no `enter_with_counters` twin, so
/// pairing them would invent a reading no consumer defines: they stay
/// unpaired. Any other nearest publisher (a Conjure, Manifest, keyword action,
/// or a wrapper whose inline payload publishes) also returns `None`, so every
/// consumer keeps its existing reading of the placement.
///
/// `chain` must be one unconditional branch chain ([`collect_chain_effects`]
/// of the root or of one `else_ability` / mode head), never a concatenation of
/// several abilities' or branches' walks.
pub(crate) fn created_token_placement_source(chain: &[&Effect], index: usize) -> Option<usize> {
    match chain.get(index)? {
        Effect::PutCounter {
            target: TargetFilter::LastCreated,
            ..
        } => {
            let publisher = chain[..index]
                .iter()
                .rposition(|effect| effect.publishes_created_referent())?;
            matches!(chain[publisher], Effect::Token { .. }).then_some(publisher)
        }
        _ => None,
    }
}

/// A created-token placement and the `Token` it pairs with, both nodes of one
/// branch chain (compared by `std::ptr::eq`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CreatedTokenPair<'a> {
    pub(crate) token: &'a Effect,
    pub(crate) placement: &'a Effect,
}

/// Every unconditional chain of `ability`'s tree, each as its own sequence:
/// the root chain ([`collect_chain_effects`]), then, for every node of a
/// collected chain, its `else_ability` (CR 608.2c: the "Otherwise" leg replaces
/// the chain it guards) and each of its `mode_abilities` (CR 700.2: each mode
/// is an alternative), recursively. Chains are never concatenated.
fn branch_chains(ability: &AbilityDefinition) -> Vec<Vec<&Effect>> {
    let mut chains = Vec::new();
    push_branch_chains(ability, &mut chains);
    chains
}

fn push_branch_chains<'a>(head: &'a AbilityDefinition, chains: &mut Vec<Vec<&'a Effect>>) {
    chains.push(collect_chain_effects(head));
    for node in std::iter::successors(Some(head), |node| node.sub_ability.as_deref()) {
        if let Some(other) = node.else_ability.as_deref() {
            push_branch_chains(other, chains);
        }
        for mode in &node.mode_abilities {
            push_branch_chains(mode, chains);
        }
    }
}

/// One ability's effects as a consumer walks them, plus the ability they came
/// from. The ability's branch chains, the only sequences the created-token
/// predicate reads, are built on demand, never at construction.
pub(crate) struct AbilityWalk<'a> {
    ability: Option<&'a AbilityDefinition>,
    effects: Vec<&'a Effect>,
}

impl<'a> AbilityWalk<'a> {
    /// `effects` must be a walk of `ability` (any of the existing walkers), so
    /// that each entry is a node of `ability`'s tree and identity comparison
    /// against the chain is meaningful.
    pub(crate) fn of(ability: &'a AbilityDefinition, effects: Vec<&'a Effect>) -> Self {
        Self {
            ability: Some(ability),
            effects,
        }
    }

    /// The unconditional chain as the walk ([`collect_chain_effects`]).
    pub(crate) fn unconditional(ability: &'a AbilityDefinition) -> Self {
        Self::of(ability, collect_chain_effects(ability))
    }

    /// A walk with no `AbilityDefinition` behind it (a `ResolvedAbility` walk):
    /// `ability` is `None`, so the conservative default holds for every entry.
    pub(crate) fn unpaired(effects: Vec<&'a Effect>) -> Self {
        Self {
            ability: None,
            effects,
        }
    }

    /// Every created-token pair of the ability's branch chains, each read
    /// within its own chain ([`created_token_placement_source`]); empty when
    /// there is no ability behind the walk. Built on each call.
    pub(crate) fn created_token_pairs(&self) -> Vec<CreatedTokenPair<'a>> {
        self.ability
            .map(branch_chains)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|chain| {
                (0..chain.len())
                    .filter_map(|index| {
                        created_token_placement_source(&chain, index).map(|token| {
                            CreatedTokenPair {
                                token: chain[token],
                                placement: chain[index],
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(crate) fn effects(&self) -> &[&'a Effect] {
        &self.effects
    }

    pub(crate) fn into_effects(self) -> Vec<&'a Effect> {
        self.effects
    }

    /// The filtering view: this walk without its created-token placements. An
    /// entry is dropped iff it is (by `std::ptr::eq`) the placement of one of
    /// [`Self::created_token_pairs`]. A placement in an `else_ability` or mode
    /// branch is dropped only when its own branch chain pairs it; a placement
    /// whose `Token` sits in another chain is kept. Builds the pairs once per
    /// call.
    pub(crate) fn without_created_token_placements(&self) -> Vec<&'a Effect> {
        let pairs = self.created_token_pairs();
        self.effects
            .iter()
            .copied()
            .filter(|effect| {
                !pairs
                    .iter()
                    .any(|pair| std::ptr::eq(pair.placement, *effect))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cast_facts::collect_definition_effects;
    use crate::features::plus_one_counters::ability_creates_token_with_plus_one_counters;
    use crate::policies::tests::created_token_placement_parity::{
        effect_chain, last_created_placement, token_effect,
    };
    use engine::types::ability::{
        AbilityCondition, AbilityKind, ChooseOneOfKind, ConjureCard, ConjureSource,
        DieResultBranch, PlayerFilter, QuantityExpr, SubAbilityLink, TypedFilter,
    };
    use engine::types::counter::CounterType;
    use engine::types::zones::Zone;

    fn one() -> QuantityExpr {
        QuantityExpr::Fixed { value: 1 }
    }

    fn token() -> Effect {
        token_effect(0, 0, vec![])
    }

    fn placement() -> Effect {
        last_created_placement(CounterType::Plus1Plus1, one())
    }

    fn put_counter_on(target: TargetFilter) -> Effect {
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: one(),
            target,
        }
    }

    fn conjure() -> Effect {
        Effect::Conjure {
            cards: vec![ConjureCard {
                source: ConjureSource::Named {
                    name: "Hangarback Walker".to_string(),
                },
                count: one(),
            }],
            destination: Zone::Battlefield,
            tapped: false,
            library_position: None,
            library_players: None,
        }
    }

    fn source_at(effects: &[Effect], index: usize) -> Option<usize> {
        let chain: Vec<&Effect> = effects.iter().collect();
        created_token_placement_source(&chain, index)
    }

    /// CR 608.2c + CR 111.1: the placement pairs with the NEAREST preceding
    /// publisher of its own chain when that publisher is a `Token`, whatever
    /// non-publisher sits between them and whatever the counter type.
    #[test]
    fn placement_pairs_with_the_nearest_publisher_when_it_is_a_token() {
        assert_eq!(source_at(&[token(), placement()], 1), Some(0));
        assert_eq!(source_at(&[token(), token(), placement()], 2), Some(1));
        assert_eq!(
            source_at(&[Effect::Investigate, token(), placement()], 2),
            Some(1)
        );
        let deathtouch = last_created_placement(CounterType::Generic("deathtouch".into()), one());
        let mill = Effect::Mill {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        };
        assert_eq!(source_at(&[token(), mill, deathtouch], 2), Some(0));
    }

    /// Conservative default: anything other than a `LastCreated` placement whose
    /// nearest preceding publisher is a `Token` keeps today's reading.
    #[test]
    fn non_token_publishers_and_other_targets_do_not_pair() {
        // A Conjure is a `LastCreated` publisher but not a token creation.
        assert_eq!(source_at(&[conjure(), placement()], 1), None);
        assert_eq!(
            source_at(&[token(), put_counter_on(TargetFilter::SelfRef)], 1),
            None
        );
        assert_eq!(
            source_at(
                &[
                    token(),
                    put_counter_on(TargetFilter::Typed(TypedFilter::creature()))
                ],
                1
            ),
            None
        );
        // The token must come first in resolution order.
        assert_eq!(source_at(&[placement(), token()], 0), None);
        // Out of range, and a non-placement index.
        assert_eq!(source_at(&[token(), placement()], 2), None);
        assert_eq!(source_at(&[token(), placement()], 0), None);
    }

    /// The filtering view drops exactly the chain's created-token placements.
    #[test]
    fn filtering_view_drops_only_created_token_placements() {
        let ability = effect_chain(
            AbilityKind::Spell,
            SubAbilityLink::ContinuationStep,
            vec![token(), placement(), put_counter_on(TargetFilter::SelfRef)],
        );
        let walk = AbilityWalk::unconditional(&ability);
        assert_eq!(walk.effects().len(), 3);
        let kept = walk.without_created_token_placements();
        assert_eq!(kept.len(), 2);
        assert!(matches!(kept[0], Effect::Token { .. }));
        assert!(matches!(
            kept[1],
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
        // A walk with no ability behind it keeps every entry.
        let unpaired = AbilityWalk::unpaired(collect_chain_effects(&ability));
        assert!(unpaired.created_token_pairs().is_empty());
        assert_eq!(unpaired.without_created_token_placements().len(), 3);
    }

    /// CR 608.2c: an "otherwise" leg replaces the chain it guards, so a
    /// placement there never resolves after the root `Token`. It is not in the
    /// unconditional chain, and the branch-inclusive walk keeps it.
    #[test]
    fn else_branch_placement_is_not_paired_with_the_root_token() {
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, token());
        ability.condition = Some(AbilityCondition::IsYourTurn);
        ability.else_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            placement(),
        )));
        assert_eq!(collect_chain_effects(&ability).len(), 1);
        let walk = AbilityWalk::of(&ability, collect_definition_effects(&ability));
        assert_eq!(walk.effects().len(), 2);
        assert_eq!(walk.without_created_token_placements().len(), 2);

        // Control: the same placement in the root sub chain is paired.
        let mut chained = AbilityDefinition::new(AbilityKind::Spell, token());
        chained.condition = Some(AbilityCondition::IsYourTurn);
        chained.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            placement(),
        )));
        let walk = AbilityWalk::of(&chained, collect_definition_effects(&chained));
        assert_eq!(walk.without_created_token_placements().len(), 1);
    }

    /// CR 700.2: modes are alternatives, so a `Token` in one mode never pairs
    /// with a placement in another.
    #[test]
    fn placements_in_another_mode_are_not_paired() {
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        ability.mode_abilities = vec![
            AbilityDefinition::new(AbilityKind::Spell, token()),
            AbilityDefinition::new(AbilityKind::Spell, placement()),
        ];
        let walk = AbilityWalk::of(&ability, collect_definition_effects(&ability));
        assert_eq!(walk.effects().len(), 3);
        assert_eq!(walk.without_created_token_placements().len(), 3);
    }

    /// N3 (CR 608.2c): an "otherwise" leg is its own chain, so a complete
    /// `[Token, P]` pair inside it pairs within that chain. The root `Token`
    /// still never pairs with a placement in the else leg (the test above).
    #[test]
    fn else_branch_complete_pair_pairs_within_its_chain() {
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, gain_life());
        ability.condition = Some(AbilityCondition::IsYourTurn);
        ability.else_ability = Some(Box::new(spell(vec![token(), placement()])));
        let walk = AbilityWalk::of(&ability, collect_definition_effects(&ability));
        assert_eq!(walk.effects().len(), 3);
        let kept = walk.without_created_token_placements();
        assert_eq!(kept.len(), 2, "the else leg's own placement is dropped");
        assert!(kept
            .iter()
            .all(|effect| !matches!(effect, Effect::PutCounter { .. })));
        assert_eq!(walk.created_token_pairs().len(), 1);
    }

    /// N3 (CR 700.2): a mode is its own chain, so a complete `[Token, P]` pair
    /// inside one mode pairs; the other mode is untouched.
    #[test]
    fn mode_branch_complete_pair_pairs_within_its_mode() {
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        ability.mode_abilities = vec![
            spell(vec![token(), placement()]),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: one(),
                    target: TargetFilter::Controller,
                },
            ),
        ];
        let walk = AbilityWalk::of(&ability, collect_definition_effects(&ability));
        assert_eq!(walk.effects().len(), 4);
        let kept = walk.without_created_token_placements();
        assert_eq!(kept.len(), 3);
        assert!(kept
            .iter()
            .all(|effect| !matches!(effect, Effect::PutCounter { .. })));
    }

    /// N3: the nearest-publisher rule holds inside a branch chain too: a
    /// Conjure between the mode's `Token` and its placement blocks the pairing.
    #[test]
    fn branch_intervening_conjure_blocks_the_pairing() {
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        ability.mode_abilities = vec![spell(vec![token(), conjure(), placement()])];
        let walk = AbilityWalk::of(&ability, collect_definition_effects(&ability));
        assert!(walk.created_token_pairs().is_empty());
        assert_eq!(
            walk.without_created_token_placements().len(),
            walk.effects().len()
        );
    }

    // ─── F4: the nearest publisher decides (engine classification) ──────────

    fn effect(value: serde_json::Value) -> Effect {
        serde_json::from_value(value).expect("fixture effect deserializes")
    }

    fn fixed(value: i32) -> serde_json::Value {
        serde_json::json!({ "type": "Fixed", "value": value })
    }

    fn copy_token_of() -> Effect {
        effect(serde_json::json!({
            "type": "CopyTokenOf",
            "target": { "type": "Any" },
            "owner": { "type": "Controller" },
            "count": fixed(1)
        }))
    }

    fn draft_from_spellbook() -> Effect {
        effect(serde_json::json!({
            "type": "DraftFromSpellbook",
            "destination": "Battlefield",
            "random": true
        }))
    }

    fn gain_life() -> Effect {
        Effect::GainLife {
            amount: one(),
            player: TargetFilter::Controller,
        }
    }

    /// One fixture per publisher the engine classifies as an `OtherProducer`.
    fn other_producers() -> Vec<Effect> {
        use serde_json::json;
        vec![
            effect(
                json!({ "type": "Manifest", "target": { "type": "Controller" }, "count": fixed(1) }),
            ),
            Effect::ManifestDread,
            effect(
                json!({ "type": "Cloak", "target": { "type": "Controller" }, "count": fixed(1) }),
            ),
            conjure(),
            draft_from_spellbook(),
            effect(json!({
                "type": "CopyTokenBlockingAttacker",
                "source_filter": { "type": "Any" },
                "owner": { "type": "Controller" }
            })),
            effect(json!({
                "type": "EachPlayerCopyChosen",
                "choose_filter": { "type": "Any" },
                "min": 1,
                "max": 1
            })),
            effect(json!({
                "type": "CreateTokenCopyFromPool",
                "mv": "LE",
                "mv_bound": fixed(3),
                "selection": "Random"
            })),
            Effect::Investigate,
            effect(json!({ "type": "Amass", "subtype": "Orc", "count": fixed(1) })),
            effect(
                json!({ "type": "Endure", "amount": fixed(2), "subject": { "type": "SelfRef" } }),
            ),
            Effect::Myriad,
            Effect::Encore,
            effect(json!({ "type": "EmpowerJace", "count": fixed(2) })),
        ]
    }

    fn choose_one_of(branches: Vec<Effect>) -> Effect {
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: branches
                .into_iter()
                .map(|branch| AbilityDefinition::new(AbilityKind::Spell, branch))
                .collect(),
            kind: ChooseOneOfKind::Ordinary,
        }
    }

    fn roll_die_creating_a_token() -> Effect {
        Effect::RollDie {
            count: one(),
            sides: 6,
            results: vec![DieResultBranch {
                min: 1,
                max: 6,
                effect: Box::new(AbilityDefinition::new(AbilityKind::Spell, token())),
            }],
            modifier: None,
        }
    }

    fn spell(effects: Vec<Effect>) -> AbilityDefinition {
        effect_chain(
            AbilityKind::Spell,
            SubAbilityLink::ContinuationStep,
            effects,
        )
    }

    /// F4 (CR 608.2c): `[Token, Conjure, P]` — the Conjure assigns the referent
    /// after the Token, so the placement's counters go on the conjured card. It
    /// is not paired, and every consumer keeps it as a plain counter placement.
    /// Control: `[Token, P]` pairs through the same consumers.
    #[test]
    fn an_intervening_conjure_blocks_the_pairing() {
        let ordinary = spell(vec![token(), placement()]);
        assert_eq!(source_at(&[token(), placement()], 1), Some(0));
        assert!(ability_creates_token_with_plus_one_counters(&ordinary));
        assert_eq!(
            AbilityWalk::unconditional(&ordinary)
                .without_created_token_placements()
                .len(),
            1
        );

        let interleaved = spell(vec![token(), conjure(), placement()]);
        assert_eq!(source_at(&[token(), conjure(), placement()], 2), None);
        assert!(!ability_creates_token_with_plus_one_counters(&interleaved));
        assert_eq!(
            AbilityWalk::unconditional(&interleaved)
                .without_created_token_placements()
                .len(),
            3
        );
        // The walk-level view the `x_reference` / `plus_one_counters` consumers
        // build over the definition walk keeps the placement too.
        let walk = AbilityWalk::of(&interleaved, collect_definition_effects(&interleaved));
        assert_eq!(walk.without_created_token_placements().len(), 3);
    }

    /// F4 class: every other producer between the `Token` and the placement is
    /// the nearest publisher, so none pairs.
    #[test]
    fn every_intervening_other_producer_blocks_the_pairing() {
        let producers = other_producers();
        assert_eq!(producers.len(), 14);
        for producer in producers {
            assert!(producer.publishes_created_referent(), "{producer:?}");
            assert_eq!(
                source_at(&[token(), producer.clone(), placement()], 2),
                None,
                "{producer:?}"
            );
        }
    }

    /// D1.2: `CopyTokenOf` and `Populate` publish tokens but have no
    /// `enter_with_counters` twin, so they stay unpaired — also when they are
    /// nearer than an earlier `Token`. An earlier publisher never shadows a
    /// nearer `Token`.
    #[test]
    fn copy_publishers_stay_unpaired_and_order_decides() {
        assert_eq!(source_at(&[copy_token_of(), placement()], 1), None);
        assert_eq!(source_at(&[Effect::Populate, placement()], 1), None);
        assert_eq!(source_at(&[token(), copy_token_of(), placement()], 2), None);

        assert_eq!(source_at(&[conjure(), token(), placement()], 2), Some(1));
        assert_eq!(
            source_at(&[copy_token_of(), token(), placement()], 2),
            Some(1)
        );
        assert_eq!(
            source_at(&[draft_from_spellbook(), token(), placement()], 2),
            Some(1)
        );
    }

    /// The rule is "publisher", not "creates a token": Incubate creates an
    /// Incubator token without assigning the referent, so it is transparent.
    #[test]
    fn a_token_creator_that_does_not_publish_is_transparent() {
        let incubate = effect(serde_json::json!({ "type": "Incubate", "count": fixed(2) }));
        assert!(!incubate.publishes_created_referent());
        assert_eq!(source_at(&[token(), incubate, placement()], 2), Some(0));
    }

    /// CR 608.2c + CR 608.2d: a wrapper whose inline payload publishes is the
    /// nearest publisher (its chosen payload assigns the referent before the
    /// placement), nested wrappers included; a wrapper whose payload creates
    /// nothing is transparent.
    #[test]
    fn a_wrapper_blocks_the_pairing_only_when_its_payload_publishes() {
        let choose = choose_one_of(vec![token(), gain_life()]);
        assert!(choose.publishes_created_referent());
        assert_eq!(source_at(&[token(), choose, placement()], 2), None);

        let roll = roll_die_creating_a_token();
        assert!(roll.publishes_created_referent());
        assert_eq!(source_at(&[token(), roll, placement()], 2), None);

        let nested = choose_one_of(vec![roll_die_creating_a_token()]);
        assert_eq!(source_at(&[token(), nested, placement()], 2), None);

        let quiet = choose_one_of(vec![
            gain_life(),
            Effect::Draw {
                count: one(),
                target: TargetFilter::Controller,
            },
        ]);
        assert!(!quiet.publishes_created_referent());
        assert_eq!(source_at(&[token(), quiet, placement()], 2), Some(0));
    }

    /// CR 603.7a: a delayed trigger's `Token` resolves later as its own ability,
    /// so the carrier never blocks the pairing.
    #[test]
    fn a_later_resolution_payload_never_blocks_the_pairing() {
        let carrier = effect(serde_json::json!({
            "type": "CreateDelayedTrigger",
            "condition": { "type": "WhenDies", "filter": { "type": "SelfRef" } },
            "effect": serde_json::to_value(AbilityDefinition::new(AbilityKind::Spell, token()))
                .expect("payload serializes")
        }));
        assert!(!carrier.publishes_created_referent());
        assert_eq!(source_at(&[token(), carrier, placement()], 2), Some(0));
    }
}
