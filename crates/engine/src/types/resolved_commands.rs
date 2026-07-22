//! Append-only, identity-bearing records for resolved rules work.
//!
//! P1 records provenance only. Existing engine reducers remain the behavior
//! authority until the later resolved-command application phase.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::ability::TriggerDefinitionRef;
use super::identifiers::ObjectIncarnationRef;
use super::mana::{ManaPipId, ManaUnit};
use super::player::PlayerId;

/// Globally ordered identity of a resolved command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResolvedCommandOrdinal(pub u64);

/// Globally ordered identity of a rules-execution settlement node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SettlementNodeOrdinal(pub u64);

/// Typed identity of one resolved rules-execution node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RulesExecutionNodeRef {
    Proposal(ResolvedCommandOrdinal),
    ActivatedMana(SettlementNodeOrdinal),
    TriggeredMana(SettlementNodeOrdinal),
    Payment(SettlementNodeOrdinal),
    PlayerLeave(ResolvedCommandOrdinal),
}

/// Exact recipient of one mana payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaPaymentRecipient {
    Object(ObjectIncarnationRef),
    Player(PlayerId),
}

/// Semantic category of a resolved rules-execution node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RulesExecutionNodeKind {
    Proposal,
    ActivatedMana {
        source: ObjectIncarnationRef,
    },
    TriggeredMana {
        source: ObjectIncarnationRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<TriggerDefinitionRef>,
    },
    Payment {
        payer: PlayerId,
        recipient: ManaPaymentRecipient,
    },
    PlayerLeave,
}

/// Metadata shared by every resolved rules-execution node.
///
/// bundle_parent lets a triggered mana ability remain selectable with its
/// causing activation while retaining its own distinct causal node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementNode {
    pub ordinal: SettlementNodeOrdinal,
    pub identity: RulesExecutionNodeRef,
    pub kind: RulesExecutionNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<RulesExecutionNodeRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<RulesExecutionNodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_parent: Option<RulesExecutionNodeRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub produced_pips: Vec<ManaPipId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spent_pips: Vec<ManaPipId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub journal_ordinals: Vec<ResolvedCommandOrdinal>,
}

/// One command slot assigned to a journal node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCommandJournalEntry {
    pub ordinal: ResolvedCommandOrdinal,
    pub node: RulesExecutionNodeRef,
}

/// Exact stamped mana created by one node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProducedManaUnit {
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
}

impl PartialEq for ProducedManaUnit {
    fn eq(&self, other: &Self) -> bool {
        self.unit.pip_id == other.unit.pip_id
            && self.unit == other.unit
            && self.producer == other.producer
    }
}

impl Eq for ProducedManaUnit {}

/// Exact mana unit consumed for one cost component, in consumption order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpentManaUnit {
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
    pub payment: RulesExecutionNodeRef,
    pub recipient: ManaPaymentRecipient,
}

impl PartialEq for SpentManaUnit {
    fn eq(&self, other: &Self) -> bool {
        self.unit.pip_id == other.unit.pip_id
            && self.unit == other.unit
            && self.producer == other.producer
            && self.payment == other.payment
            && self.recipient == other.recipient
    }
}

impl Eq for SpentManaUnit {}

/// Checked allocation and authority-validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRulesJournalError {
    CommandOrdinalOverflow,
    SettlementNodeOrdinalOverflow,
    UnstampedManaPip,
    DuplicateProducedPip(ManaPipId),
    UnknownProducedPip(ManaPipId),
    DuplicateSpentPip(ManaPipId),
    UnknownNode(RulesExecutionNodeRef),
    InvalidSerializedAuthority(String),
}

impl std::fmt::Display for ResolvedRulesJournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommandOrdinalOverflow => write!(f, "resolved-command ordinal overflow"),
            Self::SettlementNodeOrdinalOverflow => write!(f, "settlement-node ordinal overflow"),
            Self::UnstampedManaPip => write!(f, "mana provenance requires a stamped pip id"),
            Self::DuplicateProducedPip(pip) => write!(f, "duplicate produced pip {}", pip.0),
            Self::UnknownProducedPip(pip) => write!(f, "spent pip {} has no producer", pip.0),
            Self::DuplicateSpentPip(pip) => write!(f, "pip {} was spent more than once", pip.0),
            Self::UnknownNode(node) => write!(f, "journal references unknown node {node:?}"),
            Self::InvalidSerializedAuthority(reason) => {
                write!(f, "invalid resolved-rules journal: {reason}")
            }
        }
    }
}

impl std::error::Error for ResolvedRulesJournalError {}

/// Persistent resolved rules journal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedRulesJournal {
    next_command_ordinal: u64,
    next_settlement_node_ordinal: u64,
    entries: Vec<ResolvedCommandJournalEntry>,
    nodes: Vec<SettlementNode>,
    produced_mana: Vec<ProducedManaUnit>,
    spent_mana: Vec<SpentManaUnit>,
}

#[derive(Serialize, Deserialize)]
struct ResolvedRulesJournalWire {
    next_command_ordinal: u64,
    next_settlement_node_ordinal: u64,
    #[serde(default)]
    entries: Vec<ResolvedCommandJournalEntry>,
    #[serde(default)]
    nodes: Vec<SettlementNode>,
    #[serde(default)]
    produced_mana: Vec<ProducedManaUnit>,
    #[serde(default)]
    spent_mana: Vec<SpentManaUnit>,
}

impl Serialize for ResolvedRulesJournal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ResolvedRulesJournalWire {
            next_command_ordinal: self.next_command_ordinal,
            next_settlement_node_ordinal: self.next_settlement_node_ordinal,
            entries: self.entries.clone(),
            nodes: self.nodes.clone(),
            produced_mana: self.produced_mana.clone(),
            spent_mana: self.spent_mana.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResolvedRulesJournal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ResolvedRulesJournalWire::deserialize(deserializer)?;
        let journal = Self {
            next_command_ordinal: wire.next_command_ordinal,
            next_settlement_node_ordinal: wire.next_settlement_node_ordinal,
            entries: wire.entries,
            nodes: wire.nodes,
            produced_mana: wire.produced_mana,
            spent_mana: wire.spent_mana,
        };
        journal
            .validate_serialized_authority()
            .map_err(serde::de::Error::custom)?;
        Ok(journal)
    }
}

impl ResolvedRulesJournal {
    pub fn entries(&self) -> &[ResolvedCommandJournalEntry] {
        &self.entries
    }

    pub fn nodes(&self) -> &[SettlementNode] {
        &self.nodes
    }

    pub fn produced_mana(&self) -> &[ProducedManaUnit] {
        &self.produced_mana
    }

    pub fn spent_mana(&self) -> &[SpentManaUnit] {
        &self.spent_mana
    }

    pub fn has_produced_pip(&self, pip: ManaPipId) -> bool {
        self.produced_mana
            .iter()
            .any(|record| record.unit.pip_id == pip)
    }

    pub fn latest_mana_producer_for_source(
        &self,
        source_id: super::identifiers::ObjectId,
    ) -> Option<RulesExecutionNodeRef> {
        self.produced_mana
            .iter()
            .rev()
            .find(|record| record.unit.source_id == source_id)
            .map(|record| record.producer)
    }

    pub fn next_command_ordinal(&self) -> ResolvedCommandOrdinal {
        ResolvedCommandOrdinal(self.next_command_ordinal)
    }

    pub fn next_settlement_node_ordinal(&self) -> SettlementNodeOrdinal {
        SettlementNodeOrdinal(self.next_settlement_node_ordinal)
    }

    /// Opens a proposal node for legacy production outside a specific scope.
    pub fn begin_proposal(&mut self) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        self.ensure_node_capacity()?;
        let command = self.allocate_command();
        let ordinal = self.allocate_node();
        let identity = RulesExecutionNodeRef::Proposal(command);
        self.entries.push(ResolvedCommandJournalEntry {
            ordinal: command,
            node: identity,
        });
        self.nodes.push(SettlementNode {
            ordinal,
            identity,
            kind: RulesExecutionNodeKind::Proposal,
            caused_by: None,
            depends_on: Vec::new(),
            bundle_parent: None,
            produced_pips: Vec::new(),
            spent_pips: Vec::new(),
            journal_ordinals: vec![command],
        });
        Ok(identity)
    }

    pub fn begin_activated_mana(
        &mut self,
        source: ObjectIncarnationRef,
        caused_by: Option<RulesExecutionNodeRef>,
    ) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        self.begin_settlement(
            RulesExecutionNodeRef::ActivatedMana,
            RulesExecutionNodeKind::ActivatedMana { source },
            caused_by,
            None,
        )
    }

    pub fn begin_triggered_mana(
        &mut self,
        source: ObjectIncarnationRef,
        trigger: Option<TriggerDefinitionRef>,
        caused_by: Option<RulesExecutionNodeRef>,
    ) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        let bundle_parent = caused_by
            .map(|cause| self.bundle_owner(cause))
            .transpose()?
            .flatten();
        self.begin_settlement(
            RulesExecutionNodeRef::TriggeredMana,
            RulesExecutionNodeKind::TriggeredMana { source, trigger },
            caused_by,
            bundle_parent,
        )
    }

    pub fn record_produced_mana(
        &mut self,
        producer: RulesExecutionNodeRef,
        unit: ManaUnit,
    ) -> Result<(), ResolvedRulesJournalError> {
        Self::require_stamped(unit.pip_id)?;
        let node_index = self.node_index(producer)?;
        if self
            .produced_mana
            .iter()
            .any(|record| record.unit.pip_id == unit.pip_id)
        {
            return Err(ResolvedRulesJournalError::DuplicateProducedPip(unit.pip_id));
        }
        self.nodes[node_index].produced_pips.push(unit.pip_id);
        self.produced_mana.push(ProducedManaUnit { unit, producer });
        Ok(())
    }

    /// Records all exact units consumed by one cost component in solver order.
    pub fn record_spent_mana(
        &mut self,
        payer: PlayerId,
        recipient: ManaPaymentRecipient,
        spent: &[ManaUnit],
    ) -> Result<Option<RulesExecutionNodeRef>, ResolvedRulesJournalError> {
        if spent.is_empty() {
            return Ok(None);
        }
        let mut seen = HashSet::new();
        let mut dependencies = Vec::new();
        let mut producers = Vec::with_capacity(spent.len());
        for unit in spent {
            Self::require_stamped(unit.pip_id)?;
            if !seen.insert(unit.pip_id) || self.spent_pip_exists(unit.pip_id) {
                return Err(ResolvedRulesJournalError::DuplicateSpentPip(unit.pip_id));
            }
            let Some(produced) = self
                .produced_mana
                .iter()
                .find(|record| record.unit.pip_id == unit.pip_id)
            else {
                return Err(ResolvedRulesJournalError::UnknownProducedPip(unit.pip_id));
            };
            if !dependencies.contains(&produced.producer) {
                dependencies.push(produced.producer);
            }
            producers.push(produced.producer);
        }
        let payment = self.begin_settlement(
            RulesExecutionNodeRef::Payment,
            RulesExecutionNodeKind::Payment {
                payer,
                recipient: recipient.clone(),
            },
            None,
            None,
        )?;
        let payment_index = self.node_index(payment)?;
        self.nodes[payment_index].depends_on = dependencies;
        self.nodes[payment_index].spent_pips = spent.iter().map(|unit| unit.pip_id).collect();
        self.spent_mana.extend(
            spent
                .iter()
                .cloned()
                .zip(producers)
                .map(|(unit, producer)| SpentManaUnit {
                    unit,
                    producer,
                    payment,
                    recipient: recipient.clone(),
                }),
        );
        Ok(Some(payment))
    }

    fn begin_settlement(
        &mut self,
        identity_for: impl FnOnce(SettlementNodeOrdinal) -> RulesExecutionNodeRef,
        kind: RulesExecutionNodeKind,
        caused_by: Option<RulesExecutionNodeRef>,
        bundle_parent: Option<RulesExecutionNodeRef>,
    ) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        self.ensure_node_capacity()?;
        for dependency in caused_by.iter().chain(bundle_parent.iter()) {
            self.node_index(*dependency)?;
        }
        let command = self.allocate_command();
        let ordinal = self.allocate_node();
        let identity = identity_for(ordinal);
        self.entries.push(ResolvedCommandJournalEntry {
            ordinal: command,
            node: identity,
        });
        self.nodes.push(SettlementNode {
            ordinal,
            identity,
            kind,
            caused_by,
            depends_on: caused_by.into_iter().collect(),
            bundle_parent,
            produced_pips: Vec::new(),
            spent_pips: Vec::new(),
            journal_ordinals: vec![command],
        });
        Ok(identity)
    }

    fn ensure_command_capacity(&self) -> Result<(), ResolvedRulesJournalError> {
        (self.next_command_ordinal != u64::MAX)
            .then_some(())
            .ok_or(ResolvedRulesJournalError::CommandOrdinalOverflow)
    }

    fn ensure_node_capacity(&self) -> Result<(), ResolvedRulesJournalError> {
        (self.next_settlement_node_ordinal != u64::MAX)
            .then_some(())
            .ok_or(ResolvedRulesJournalError::SettlementNodeOrdinalOverflow)
    }

    fn allocate_command(&mut self) -> ResolvedCommandOrdinal {
        let ordinal = ResolvedCommandOrdinal(self.next_command_ordinal);
        self.next_command_ordinal += 1;
        ordinal
    }

    fn allocate_node(&mut self) -> SettlementNodeOrdinal {
        let ordinal = SettlementNodeOrdinal(self.next_settlement_node_ordinal);
        self.next_settlement_node_ordinal += 1;
        ordinal
    }

    fn node_index(
        &self,
        identity: RulesExecutionNodeRef,
    ) -> Result<usize, ResolvedRulesJournalError> {
        self.nodes
            .iter()
            .position(|node| node.identity == identity)
            .ok_or(ResolvedRulesJournalError::UnknownNode(identity))
    }

    fn bundle_owner(
        &self,
        identity: RulesExecutionNodeRef,
    ) -> Result<Option<RulesExecutionNodeRef>, ResolvedRulesJournalError> {
        let node = &self.nodes[self.node_index(identity)?];
        Ok(node.bundle_parent.or(Some(identity)))
    }

    fn spent_pip_exists(&self, pip: ManaPipId) -> bool {
        self.spent_mana
            .iter()
            .any(|record| record.unit.pip_id == pip)
    }

    fn require_stamped(pip: ManaPipId) -> Result<(), ResolvedRulesJournalError> {
        (pip.0 != 0)
            .then_some(())
            .ok_or(ResolvedRulesJournalError::UnstampedManaPip)
    }

    fn validate_serialized_authority(&self) -> Result<(), ResolvedRulesJournalError> {
        if self.next_command_ordinal != self.entries.len() as u64
            || self.next_settlement_node_ordinal != self.nodes.len() as u64
        {
            return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                "allocator is not contiguous with its records".to_string(),
            ));
        }
        for (expected, entry) in self.entries.iter().enumerate() {
            if entry.ordinal != ResolvedCommandOrdinal(expected as u64) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "command entries are duplicate or nonmonotonic".to_string(),
                ));
            }
        }
        for (expected, node) in self.nodes.iter().enumerate() {
            if node.ordinal != SettlementNodeOrdinal(expected as u64)
                || !identity_matches_kind(node.identity, &node.kind)
            {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "settlement node identity is duplicate, nonmonotonic, or mismatched"
                        .to_string(),
                ));
            }
            if has_duplicate_values(&node.journal_ordinals)
                || has_duplicate_values(&node.produced_pips)
                || has_duplicate_values(&node.spent_pips)
            {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "node metadata contains duplicate identities".to_string(),
                ));
            }
            for dependency in node
                .caused_by
                .iter()
                .chain(node.depends_on.iter())
                .chain(node.bundle_parent.iter())
            {
                let dependency_index = self.node_index(*dependency).map_err(|_| {
                    ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "node references an unknown dependency".to_string(),
                    )
                })?;
                if dependency_index >= expected {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "node depends on a non-prior node".to_string(),
                    ));
                }
            }
        }
        for entry in &self.entries {
            let node = self.node_index(entry.node).map_err(|_| {
                ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "command entry references an unknown node".to_string(),
                )
            })?;
            if !self.nodes[node].journal_ordinals.contains(&entry.ordinal) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "command entry is absent from node metadata".to_string(),
                ));
            }
        }
        for node in &self.nodes {
            for ordinal in &node.journal_ordinals {
                if !self
                    .entries
                    .iter()
                    .any(|entry| entry.ordinal == *ordinal && entry.node == node.identity)
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "node metadata references an unrelated journal entry".to_string(),
                    ));
                }
            }
        }

        let mut produced_pips = HashSet::new();
        for record in &self.produced_mana {
            Self::require_stamped(record.unit.pip_id)?;
            if !produced_pips.insert(record.unit.pip_id) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "duplicate produced mana pip".to_string(),
                ));
            }
            let node = self.node_index(record.producer).map_err(|_| {
                ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "produced mana references unknown node".to_string(),
                )
            })?;
            if !self.nodes[node].produced_pips.contains(&record.unit.pip_id) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "produced mana is absent from node metadata".to_string(),
                ));
            }
        }
        for node in &self.nodes {
            if node.produced_pips.iter().any(|pip| {
                self.produced_mana
                    .iter()
                    .filter(|record| record.producer == node.identity)
                    .all(|record| record.unit.pip_id != *pip)
            }) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "node metadata references unrecorded produced mana".to_string(),
                ));
            }
        }
        let mut spent_pips = HashSet::new();
        for record in &self.spent_mana {
            Self::require_stamped(record.unit.pip_id)?;
            if !spent_pips.insert(record.unit.pip_id) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "duplicate spent mana pip".to_string(),
                ));
            }
            let Some(produced) = self
                .produced_mana
                .iter()
                .find(|item| item.unit.pip_id == record.unit.pip_id)
            else {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana has no producer".to_string(),
                ));
            };
            let payment = self.node_index(record.payment).map_err(|_| {
                ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana references unknown payment".to_string(),
                )
            })?;
            let RulesExecutionNodeKind::Payment { recipient, .. } = &self.nodes[payment].kind
            else {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana references a non-payment node".to_string(),
                ));
            };
            if produced.producer != record.producer
                || produced.unit != record.unit
                || *recipient != record.recipient
                || !self.nodes[payment].spent_pips.contains(&record.unit.pip_id)
            {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana disagrees with recorded provenance".to_string(),
                ));
            }
        }
        for node in &self.nodes {
            if node.spent_pips.iter().any(|pip| {
                self.spent_mana
                    .iter()
                    .filter(|record| record.payment == node.identity)
                    .all(|record| record.unit.pip_id != *pip)
            }) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "node metadata references unrecorded spent mana".to_string(),
                ));
            }
        }
        Ok(())
    }
}

fn identity_matches_kind(identity: RulesExecutionNodeRef, kind: &RulesExecutionNodeKind) -> bool {
    matches!(
        (identity, kind),
        (
            RulesExecutionNodeRef::Proposal(_),
            RulesExecutionNodeKind::Proposal
        ) | (
            RulesExecutionNodeRef::ActivatedMana(_),
            RulesExecutionNodeKind::ActivatedMana { .. }
        ) | (
            RulesExecutionNodeRef::TriggeredMana(_),
            RulesExecutionNodeKind::TriggeredMana { .. }
        ) | (
            RulesExecutionNodeRef::Payment(_),
            RulesExecutionNodeKind::Payment { .. }
        ) | (
            RulesExecutionNodeRef::PlayerLeave(_),
            RulesExecutionNodeKind::PlayerLeave
        )
    )
}

fn has_duplicate_values<T: Eq + std::hash::Hash>(values: &[T]) -> bool {
    let mut seen = HashSet::new();
    values.iter().any(|value| !seen.insert(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::{ManaRestriction, ManaType};

    fn unit(pip: u64) -> ManaUnit {
        ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(9),
            pip_id: ManaPipId(pip),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForSpell],
            grants: Vec::new(),
            expiry: None,
        }
    }

    #[test]
    fn ordinals_are_monotonic_unique_and_checked() {
        let mut journal = ResolvedRulesJournal::default();
        assert_eq!(
            journal.begin_proposal().unwrap(),
            RulesExecutionNodeRef::Proposal(ResolvedCommandOrdinal(0))
        );
        assert_eq!(
            journal.begin_proposal().unwrap(),
            RulesExecutionNodeRef::Proposal(ResolvedCommandOrdinal(1))
        );
        assert_eq!(journal.next_command_ordinal(), ResolvedCommandOrdinal(2));
        assert_eq!(
            journal.next_settlement_node_ordinal(),
            SettlementNodeOrdinal(2)
        );
        journal.next_command_ordinal = u64::MAX;
        assert_eq!(
            journal.begin_proposal(),
            Err(ResolvedRulesJournalError::CommandOrdinalOverflow)
        );
        let mut nodes = ResolvedRulesJournal {
            next_settlement_node_ordinal: u64::MAX,
            ..ResolvedRulesJournal::default()
        };
        assert_eq!(
            nodes.begin_activated_mana(ObjectIncarnationRef::of(ObjectId(1), 1), None),
            Err(ResolvedRulesJournalError::SettlementNodeOrdinalOverflow)
        );
    }

    #[test]
    fn records_exact_producer_spender_and_trigger_bundle() {
        let mut journal = ResolvedRulesJournal::default();
        let activation = journal
            .begin_activated_mana(ObjectIncarnationRef::of(ObjectId(1), 2), None)
            .unwrap();
        let trigger = journal
            .begin_triggered_mana(
                ObjectIncarnationRef::of(ObjectId(2), 3),
                None,
                Some(activation),
            )
            .unwrap();
        let produced = unit(1);
        journal
            .record_produced_mana(trigger, produced.clone())
            .unwrap();
        let payment = journal
            .record_spent_mana(
                PlayerId(0),
                ManaPaymentRecipient::Object(ObjectIncarnationRef::of(ObjectId(4), 5)),
                std::slice::from_ref(&produced),
            )
            .unwrap()
            .unwrap();
        assert_eq!(journal.spent_mana()[0].unit, produced);
        assert_eq!(journal.spent_mana()[0].producer, trigger);
        assert_eq!(
            journal.spent_mana()[0].unit.restrictions,
            vec![ManaRestriction::OnlyForSpell],
            "spent provenance preserves the produced unit's restrictions"
        );
        let node = journal
            .nodes()
            .iter()
            .find(|node| node.identity == trigger)
            .unwrap();
        assert_eq!(node.caused_by, Some(activation));
        assert_eq!(node.bundle_parent, Some(activation));
        assert_eq!(
            journal
                .nodes()
                .iter()
                .find(|node| node.identity == payment)
                .unwrap()
                .depends_on,
            vec![trigger]
        );
        assert_eq!(
            journal
                .nodes()
                .iter()
                .map(|node| node.journal_ordinals.clone())
                .collect::<Vec<_>>(),
            vec![
                vec![ResolvedCommandOrdinal(0)],
                vec![ResolvedCommandOrdinal(1)],
                vec![ResolvedCommandOrdinal(2)],
            ],
            "each distinct execution node receives a globally ordered journal slot"
        );
        let roundtrip =
            serde_json::from_value::<ResolvedRulesJournal>(serde_json::to_value(&journal).unwrap())
                .unwrap();
        assert_eq!(roundtrip, journal);
    }

    #[test]
    fn serde_roundtrip_rejects_duplicate_and_nonmonotonic_ordinals() {
        let mut journal = ResolvedRulesJournal::default();
        journal.begin_proposal().unwrap();
        journal.begin_proposal().unwrap();
        let serialized = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serialized.clone()).unwrap(),
            journal
        );
        let mut duplicate = serialized.clone();
        duplicate["entries"][1]["ordinal"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(duplicate).is_err());
        let mut nonmonotonic = serialized;
        nonmonotonic["nodes"][1]["ordinal"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(nonmonotonic).is_err());
    }
}
