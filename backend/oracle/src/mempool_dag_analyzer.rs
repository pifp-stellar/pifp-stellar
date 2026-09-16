use std::collections::{HashMap, HashSet, VecDeque};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Dependency classification between two pending transactions in the mempool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyType {
    /// Both transactions modify the same contract state entry.
    ContractStateConflict,
    /// Both transactions swap or add/remove liquidity on the same asset pair.
    AssetPairConflict,
    /// Transactions belong to the same sender account (ordered by sequence number).
    SenderNonceOrdering,
}

/// A node representing a parsed pending Stellar mempool transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MempoolTxNode {
    /// Transaction hash (hex-encoded or raw id).
    pub tx_hash: String,
    /// Account ID of the sender / source.
    pub sender: String,
    /// Sequence number of the transaction.
    pub sequence: u64,
    /// Target contract ID or asset pair (e.g. "XLM/USDC" or contract address).
    pub target_resource: String,
    /// Operation type (e.g., "Swap", "Deposit", "Withdraw", "ContractCall").
    pub op_type: String,
    /// Inferred trade direction (e.g. "Buy", "Sell", "Neutral").
    pub direction: String,
    /// Amount being traded or affected.
    pub amount: u128,
    /// Fee rate (stroops / op) offered by sender.
    pub fee_rate: u64,
    /// Raw XDR payload (bytes).
    pub xdr_payload: Vec<u8>,
    /// Unix timestamp when received by the mempool analyzer.
    pub timestamp: u64,
}

impl MempoolTxNode {
    pub fn new(
        tx_hash: impl Into<String>,
        sender: impl Into<String>,
        sequence: u64,
        target_resource: impl Into<String>,
        op_type: impl Into<String>,
        direction: impl Into<String>,
        amount: u128,
        fee_rate: u64,
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            tx_hash: tx_hash.into(),
            sender: sender.into(),
            sequence,
            target_resource: target_resource.into(),
            op_type: op_type.into(),
            direction: direction.into(),
            amount,
            fee_rate,
            xdr_payload: vec![],
            timestamp,
        }
    }
}

/// Directed edge indicating that `to_tx` depends on `from_tx` being executed first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from_tx: String,
    pub to_tx: String,
    pub dep_type: DependencyType,
}

/// Alert payload generated when a predatory sandwich MEV pattern is detected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandwichAlert {
    pub alert_id: String,
    pub target_resource: String,
    pub frontrunner_tx: String,
    pub victim_tx: String,
    pub backrunner_tx: String,
    pub attacker_address: String,
    pub victim_address: String,
    pub estimated_profit_stroops: u64,
    pub price_impact_bps: u32,
    pub timestamp: u64,
}

/// Real-time Mempool Directed Acyclic Graph (DAG) Analyzer.
#[derive(Debug, Default)]
pub struct MempoolDagAnalyzer {
    nodes: HashMap<String, MempoolTxNode>,
    adjacency: HashMap<String, Vec<String>>,
    in_degree: HashMap<String, usize>,
    edges: Vec<DependencyEdge>,
}

impl MempoolDagAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a parsed pending transaction and reconstruct dependency edges.
    pub fn add_transaction(&mut self, node: MempoolTxNode) {
        let hash = node.tx_hash.clone();
        
        // Find existing nodes that conflict or establish causal ordering
        for existing in self.nodes.values() {
            let mut dep_type = None;

            if existing.sender == node.sender && existing.sequence < node.sequence {
                dep_type = Some(DependencyType::SenderNonceOrdering);
            } else if existing.target_resource == node.target_resource {
                if existing.op_type == "ContractCall" && node.op_type == "ContractCall" {
                    dep_type = Some(DependencyType::ContractStateConflict);
                } else if existing.op_type == "Swap" && node.op_type == "Swap" {
                    dep_type = Some(DependencyType::AssetPairConflict);
                }
            }

            if let Some(t) = dep_type {
                self.edges.push(DependencyEdge {
                    from_tx: existing.tx_hash.clone(),
                    to_tx: hash.clone(),
                    dep_type: t,
                });
                self.adjacency.entry(existing.tx_hash.clone()).or_default().push(hash.clone());
                *self.in_degree.entry(hash.clone()).or_insert(0) += 1;
            }
        }

        self.adjacency.entry(hash.clone()).or_default();
        self.in_degree.entry(hash.clone()).or_insert(0);
        self.nodes.insert(hash, node);
    }

    /// Topological sort to verify DAG validity and determine processing sequence.
    pub fn topological_sort(&self) -> Result<Vec<String>, String> {
        let mut in_degree = self.in_degree.clone();
        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(k, _)| k.clone())
            .collect();

        let mut sorted = Vec::new();

        while let Some(node) = queue.pop_front() {
            sorted.push(node.clone());
            if let Some(neighbors) = self.adjacency.get(&node) {
                for neighbor in neighbors {
                    if let Some(deg) = in_degree.get_mut(neighbor) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            queue.push_back(neighbor.clone());
                        }
                    }
                }
            }
        }

        if sorted.len() != self.nodes.len() {
            Err("Cycle detected in mempool dependency graph".to_string())
        } else {
            Ok(sorted)
        }
    }

    /// Algorithmic Sandwich Attack Detection Heuristic.
    ///
    /// Identifies sandwich bundles: (Tx_front, Tx_target, Tx_back) where:
    /// - Tx_front and Tx_back share the same sender or operate as a pair around Tx_target
    /// - Tx_front has a higher fee rate to ensure inclusion ahead of Tx_target
    /// - Tx_front buys the asset, Tx_target buys at inflated price, Tx_back sells at profit
    pub fn detect_sandwich_attacks(&self) -> Vec<SandwichAlert> {
        let mut alerts = Vec::new();

        // Group transactions by target resource (asset pair / pool)
        let mut resource_groups: HashMap<String, Vec<&MempoolTxNode>> = HashMap::new();
        for node in self.nodes.values() {
            resource_groups
                .entry(node.target_resource.clone())
                .or_default()
                .push(node);
        }

        for (resource, txs) in resource_groups {
            if txs.len() < 3 {
                continue;
            }

            for front in &txs {
                if front.direction != "Buy" {
                    continue;
                }

                for victim in &txs {
                    if victim.tx_hash == front.tx_hash
                        || victim.sender == front.sender
                        || victim.direction != "Buy"
                        || victim.fee_rate >= front.fee_rate
                    {
                        continue;
                    }

                    for back in &txs {
                        if back.tx_hash == front.tx_hash
                            || back.tx_hash == victim.tx_hash
                            || back.direction != "Sell"
                        {
                            continue;
                        }

                        // Sandwich match: back-runner is either same attacker account OR has a high-fee sell immediately after victim
                        let is_sandwich = back.sender == front.sender
                            || (back.fee_rate >= victim.fee_rate && back.direction == "Sell");
                        if is_sandwich {
                            let estimated_profit =
                                front.amount.min(back.amount) / 100 * ((front.fee_rate / 10).max(1) as u128);
                            let price_impact =
                                ((front.amount as f64 / (victim.amount as f64 + 1.0)) * 10000.0) as u32;

                            let alert = SandwichAlert {
                                alert_id: format!(
                                    "alert-{}-{}-{}",
                                    front.tx_hash, victim.tx_hash, back.tx_hash
                                ),
                                target_resource: resource.clone(),
                                frontrunner_tx: front.tx_hash.clone(),
                                victim_tx: victim.tx_hash.clone(),
                                backrunner_tx: back.tx_hash.clone(),
                                attacker_address: front.sender.clone(),
                                victim_address: victim.sender.clone(),
                                estimated_profit_stroops: estimated_profit as u64,
                                price_impact_bps: price_impact,
                                timestamp: SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            };

                            if !alerts.contains(&alert) {
                                alerts.push(alert);
                            }
                        }
                    }
                }
            }
        }

        alerts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mempool_dag_construction_and_topo_sort() {
        let mut analyzer = MempoolDagAnalyzer::new();

        let tx1 = MempoolTxNode::new("tx-1", "alice", 1, "XLM/USDC", "Swap", "Buy", 1000, 50);
        let tx2 = MempoolTxNode::new("tx-2", "bob", 1, "XLM/USDC", "Swap", "Buy", 5000, 10);
        let tx3 = MempoolTxNode::new("tx-3", "alice", 2, "XLM/USDC", "Swap", "Sell", 1000, 40);

        analyzer.add_transaction(tx1);
        analyzer.add_transaction(tx2);
        analyzer.add_transaction(tx3);

        let order = analyzer.topological_sort().expect("Should form a valid DAG");
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn test_sandwich_detection_heuristic() {
        let mut analyzer = MempoolDagAnalyzer::new();

        // Front-runner: attacker buys with high fee (gets executed first)
        let front = MempoolTxNode::new("tx-front", "attacker", 1, "XLM/USDC", "Swap", "Buy", 10000, 500);
        // Victim: normal user buys at lower fee (gets sandwiched)
        let victim = MempoolTxNode::new("tx-victim", "victim_user", 10, "XLM/USDC", "Swap", "Buy", 50000, 50);
        // Back-runner: same attacker sells after victim (completes the sandwich)
        let back = MempoolTxNode::new("tx-back", "attacker", 2, "XLM/USDC", "Swap", "Sell", 10000, 100);

        analyzer.add_transaction(front);
        analyzer.add_transaction(victim);
        analyzer.add_transaction(back);

        let alerts = analyzer.detect_sandwich_attacks();
        assert!(!alerts.is_empty(), "Should detect sandwich attack pattern");

        // Verify the alert identifies the correct participants (order-independent due to HashMap)
        let alert = &alerts[0];
        assert_eq!(alert.frontrunner_tx, "tx-front", "Front-runner should be tx-front");
        assert_eq!(alert.victim_tx, "tx-victim", "Victim should be tx-victim");
        assert_eq!(alert.backrunner_tx, "tx-back", "Back-runner should be tx-back");
        assert_eq!(alert.attacker_address, "attacker");
        assert_eq!(alert.victim_address, "victim_user");
        assert_eq!(alert.target_resource, "XLM/USDC");
    }
}
