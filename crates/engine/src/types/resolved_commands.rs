//! Append-only, identity-bearing records for resolved rules work.
//!
//! P1 established provenance and ordering. P2 makes mana insert and exact
//! spend commands executable through their owning authority appliers.

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

/// One exact mana-pool insertion after mana production has been resolved.
///
/// CR 106.4: resolved mana enters this player’s pool with this already-stamped
/// pip identity; replay must neither choose a new recipient nor mint a new pip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedManaInsertCommand {
    pub player: PlayerId,
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
}

/// One exact mana unit selected by the payment solver, with its producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedManaSpentUnit {
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
}

/// One exact mana-pool removal after the payment solver has selected its units.
///
/// CR 118.3a: this command removes precisely these units, in their recorded
/// consumption order. It never asks a solver to choose replacement mana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedManaSpendCommand {
    pub payer: PlayerId,
    pub recipient: ManaPaymentRecipient,
    pub payment: RulesExecutionNodeRef,
    pub units: Vec<ResolvedManaSpentUnit>,
}

/// Semantic command payload currently carried by a resolved-rules journal entry.
///
/// Additional command families are intentionally added by their owning P2
/// authority rather than by a central replay dispatcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedRulesCommand {
    ManaInsert(ResolvedManaInsertCommand),
    ManaSpend(ResolvedManaSpendCommand),
}

/// Typed failure while applying an already-resolved mana command to a replay state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedManaReplayInvariantError {
    UnknownPlayer(PlayerId),
    UnstampedManaPip,
    DuplicateManaPip(ManaPipId),
    ManaPipIdOverflow(ManaPipId),
    DuplicateSpentManaPip(ManaPipId),
    MissingExactManaUnit(ManaPipId),
    MismatchedExactManaUnit(ManaPipId),
}

impl std::fmt::Display for ResolvedManaReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPlayer(player) => write!(f, "unknown mana-command player {}", player.0),
            Self::UnstampedManaPip => write!(f, "resolved mana command has an unstamped pip"),
            Self::DuplicateManaPip(pip) => {
                write!(f, "resolved mana command would duplicate pip {}", pip.0)
            }
            Self::ManaPipIdOverflow(pip) => {
                write!(f, "resolved mana command cannot advance past pip {}", pip.0)
            }
            Self::DuplicateSpentManaPip(pip) => {
                write!(f, "resolved mana spend repeats pip {}", pip.0)
            }
            Self::MissingExactManaUnit(pip) => {
                write!(f, "resolved mana spend cannot find pip {}", pip.0)
            }
            Self::MismatchedExactManaUnit(pip) => {
                write!(f, "resolved mana spend found mismatched pip {}", pip.0)
            }
        }
    }
}

impl std::error::Error for ResolvedManaReplayInvariantError {}

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
    /// P1 node slots intentionally have no semantic payload. P2 commands append
    /// their own globally ordered entry while preserving those original slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<ResolvedRulesCommand>,
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
            command: None,
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

    /// Records and owns the exact command that inserted one mana unit.
    pub fn record_mana_insert(
        &mut self,
        command: ResolvedManaInsertCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        self.record_produced_mana(command.producer, command.unit.clone())?;
        self.append_command(command.producer, ResolvedRulesCommand::ManaInsert(command))
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

    /// Records and owns one exact solver-selected mana payment command.
    pub fn record_mana_spend(
        &mut self,
        payer: PlayerId,
        recipient: ManaPaymentRecipient,
        spent: &[ManaUnit],
    ) -> Result<Option<ResolvedManaSpendCommand>, ResolvedRulesJournalError> {
        if spent.is_empty() {
            return Ok(None);
        }
        self.ensure_command_capacity_for(2)?;
        let Some(payment) = self.record_spent_mana(payer, recipient.clone(), spent)? else {
            return Ok(None);
        };
        self.ensure_command_capacity()?;
        let units = spent
            .iter()
            .map(|unit| {
                let producer = self
                    .spent_mana
                    .iter()
                    .find(|record| record.payment == payment && record.unit.pip_id == unit.pip_id)
                    .expect("recorded spent mana must retain its producer")
                    .producer;
                ResolvedManaSpentUnit {
                    unit: unit.clone(),
                    producer,
                }
            })
            .collect();
        let command = ResolvedManaSpendCommand {
            payer,
            recipient,
            payment,
            units,
        };
        self.append_command(payment, ResolvedRulesCommand::ManaSpend(command.clone()))?;
        Ok(Some(command))
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
            command: None,
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

    fn append_command(
        &mut self,
        node: RulesExecutionNodeRef,
        command: ResolvedRulesCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        let node_index = self.node_index(node)?;
        let ordinal = self.allocate_command();
        self.entries.push(ResolvedCommandJournalEntry {
            ordinal,
            node,
            command: Some(command),
        });
        self.nodes[node_index].journal_ordinals.push(ordinal);
        Ok(ordinal)
    }

    fn ensure_command_capacity(&self) -> Result<(), ResolvedRulesJournalError> {
        self.ensure_command_capacity_for(1)
    }

    fn ensure_command_capacity_for(&self, count: u64) -> Result<(), ResolvedRulesJournalError> {
        (self.next_command_ordinal <= u64::MAX.saturating_sub(count))
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
        let mut inserted_command_pips = HashSet::new();
        let mut spent_command_pips = HashSet::new();
        for entry in &self.entries {
            let Some(command) = &entry.command else {
                continue;
            };
            self.validate_resolved_command(entry, command)?;
            match command {
                ResolvedRulesCommand::ManaInsert(command) => {
                    if !inserted_command_pips.insert(command.unit.pip_id) {
                        return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                            "duplicate mana-insert command pip".to_string(),
                        ));
                    }
                }
                ResolvedRulesCommand::ManaSpend(command) => {
                    for spent in &command.units {
                        if !spent_command_pips.insert(spent.unit.pip_id) {
                            return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                                "duplicate mana-spend command pip".to_string(),
                            ));
                        }
                    }
                }
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

    fn validate_resolved_command(
        &self,
        entry: &ResolvedCommandJournalEntry,
        command: &ResolvedRulesCommand,
    ) -> Result<(), ResolvedRulesJournalError> {
        match command {
            ResolvedRulesCommand::ManaInsert(command) => {
                Self::require_stamped(command.unit.pip_id)?;
                if entry.node != command.producer
                    || !self.produced_mana.iter().any(|record| {
                        record.producer == command.producer
                            && exact_mana_unit_eq(&record.unit, &command.unit)
                    })
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-insert command disagrees with produced mana".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::ManaSpend(command) => {
                if command.units.is_empty() || entry.node != command.payment {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command has an empty or unrelated payment".to_string(),
                    ));
                }
                let payment = self.node_index(command.payment).map_err(|_| {
                    ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command references an unknown payment".to_string(),
                    )
                })?;
                let RulesExecutionNodeKind::Payment { payer, recipient } =
                    &self.nodes[payment].kind
                else {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command references a non-payment node".to_string(),
                    ));
                };
                if *payer != command.payer || *recipient != command.recipient {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command disagrees with payment metadata".to_string(),
                    ));
                }
                let records: Vec<&SpentManaUnit> = self
                    .spent_mana
                    .iter()
                    .filter(|record| record.payment == command.payment)
                    .collect();
                if records.len() != command.units.len()
                    || records.iter().zip(&command.units).any(|(record, spent)| {
                        record.producer != spent.producer
                            || !exact_mana_unit_eq(&record.unit, &spent.unit)
                            || record.recipient != command.recipient
                    })
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command disagrees with spent mana".to_string(),
                    ));
                }
                let mut pips = HashSet::new();
                if command
                    .units
                    .iter()
                    .any(|spent| spent.unit.pip_id.0 == 0 || !pips.insert(spent.unit.pip_id))
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command has duplicate or unstamped pips".to_string(),
                    ));
                }
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

fn exact_mana_unit_eq(left: &ManaUnit, right: &ManaUnit) -> bool {
    left.pip_id == right.pip_id && left == right
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

    #[test]
    fn semantic_mana_commands_roundtrip_and_reject_malformed_payloads() {
        let mut journal = ResolvedRulesJournal::default();
        let producer = journal.begin_proposal().unwrap();
        let produced = unit(1);
        journal
            .record_mana_insert(ResolvedManaInsertCommand {
                player: PlayerId(0),
                unit: produced.clone(),
                producer,
            })
            .unwrap();
        let spend = journal
            .record_mana_spend(
                PlayerId(0),
                ManaPaymentRecipient::Player(PlayerId(0)),
                std::slice::from_ref(&produced),
            )
            .unwrap()
            .unwrap();
        assert_eq!(spend.units[0].unit, produced);
        assert!(matches!(
            journal.entries()[1].command.as_ref(),
            Some(ResolvedRulesCommand::ManaInsert(_))
        ));
        assert!(matches!(
            journal.entries()[3].command.as_ref(),
            Some(ResolvedRulesCommand::ManaSpend(_))
        ));
        let serialized = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serialized).unwrap(),
            journal
        );

        let mut mismatched_insert = journal.clone();
        let Some(ResolvedRulesCommand::ManaInsert(command)) =
            mismatched_insert.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the insert command");
        };
        command.unit.pip_id = ManaPipId(99);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(mismatched_insert).unwrap()
        )
        .is_err());

        let mut duplicate_spend = journal.clone();
        let mut duplicate_entry = duplicate_spend.entries[3].clone();
        duplicate_entry.ordinal = ResolvedCommandOrdinal(4);
        duplicate_spend.entries.push(duplicate_entry);
        duplicate_spend.nodes[1]
            .journal_ordinals
            .push(ResolvedCommandOrdinal(4));
        duplicate_spend.next_command_ordinal = 5;
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(duplicate_spend).unwrap()
        )
        .is_err());
    }
}
