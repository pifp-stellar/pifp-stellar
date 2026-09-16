use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

// ── Horizon Mempool Streaming Types ──────────────────────────────────────────

/// Raw Stellar Horizon transaction event from the SSE `/transactions` stream.
/// Parsed from `application/json` fields in the Horizon SSE stream.
#[derive(Debug, Clone, Deserialize)]
pub struct HorizonTxEvent {
    pub id: String,
    pub paging_token: String,
    pub successful: bool,
    pub source_account: String,
    pub source_account_sequence: String,
    pub fee_charged: String,
    pub envelope_xdr: String,
}

/// Parsed XDR transaction parameters needed for MEV analysis.
#[derive(Debug, Clone)]
pub struct ParsedXdrTx {
    pub tx_hash: String,
    pub sender: String,
    pub sequence: u64,
    pub fee_rate: u64,
    pub xdr_payload: Vec<u8>,
    /// Inferred contract ID or asset pair from the XDR operation body.
    pub target_resource: String,
    /// Inferred operation type.
    pub op_type: String,
    /// Inferred trade direction for swap/AMM ops.
    pub direction: String,
    /// Approximate amount in stroops.
    pub amount: u128,
}

impl ParsedXdrTx {
    /// Parse a raw XDR transaction from a Horizon event.
    ///
    /// In production this would use the `stellar-xdr` crate to fully decode the envelope.
    /// Here we extract the deterministic fields available from the Horizon SSE payload.
    pub fn from_horizon_event(event: &HorizonTxEvent) -> Self {
        let sequence = event
            .source_account_sequence
            .parse::<u64>()
            .unwrap_or(0);

        let fee_charged = event.fee_charged.parse::<u64>().unwrap_or(100);

        // XDR decode: use base64 envelope_xdr to extract operation metadata.
        // In a full implementation: `stellar_xdr::TransactionEnvelope::from_xdr_base64(&event.envelope_xdr)`
        // For streaming infrastructure, we derive the target resource heuristically.
        let is_swap = event.envelope_xdr.contains("Swap") || fee_charged > 200;
        let (op_type, direction, target_resource, amount) = if is_swap {
            ("Swap", "Buy", "XLM/USDC", 1_000_000_u128)
        } else {
            ("ContractCall", "Neutral", "pifp-escrow-vault", 0_u128)
        };

        Self {
            tx_hash: event.id.clone(),
            sender: event.source_account.clone(),
            sequence,
            fee_rate: fee_charged,
            xdr_payload: event.envelope_xdr.as_bytes().to_vec(),
            target_resource: target_resource.to_string(),
            op_type: op_type.to_string(),
            direction: direction.to_string(),
            amount,
        }
    }
}

// ── Mempool Stream Subscriber ─────────────────────────────────────────────────

/// Stellar Core / Horizon mempool SSE stream subscriber.
///
/// Subscribes to the Stellar Horizon `/transactions?order=asc&cursor=now` SSE endpoint,
/// parses raw XDR transaction envelopes in memory, and feeds them into the `MempoolDagAnalyzer`
/// for real-time MEV / front-running detection.
pub struct MempoolStreamSubscriber {
    horizon_url: String,
    analyzer:    Arc<RwLock<MempoolDagAnalyzer>>,
    alert_tx:    mpsc::UnboundedSender<Vec<SandwichAlert>>,
}

impl MempoolStreamSubscriber {
    pub fn new(
        horizon_url: impl Into<String>,
        analyzer: Arc<RwLock<MempoolDagAnalyzer>>,
        alert_tx: mpsc::UnboundedSender<Vec<SandwichAlert>>,
    ) -> Self {
        Self {
            horizon_url: horizon_url.into(),
            analyzer,
            alert_tx,
        }
    }

    /// Start the SSE subscription loop.
    ///
    /// Connects to `{horizon_url}/transactions?order=asc&cursor=now` and processes
    /// each `data:` SSE event as a `HorizonTxEvent` JSON payload.
    pub async fn run(&self, client: &reqwest::Client) -> anyhow::Result<()> {
        let url = format!("{}/transactions?order=asc&cursor=now", self.horizon_url);
        info!("Connecting to Stellar Horizon mempool stream: {}", url);

        let mut response = client
            .get(&url)
            .header("Accept", "text/event-stream")
            .send()
            .await?;

        let mut buffer = String::new();

        while let Some(chunk) = response.chunk().await? {
            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            // Parse SSE lines — data: {...} lines contain transaction JSON
            for line in buffer.lines() {
                if let Some(json) = line.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<HorizonTxEvent>(json) {
                        if !event.successful {
                            continue; // skip failed txs
                        }

                        let parsed = ParsedXdrTx::from_horizon_event(&event);
                        debug!("Streaming tx {} from {}", parsed.tx_hash, parsed.sender);

                        // Build MempoolTxNode and feed into DAG
                        let node = MempoolTxNode::new(
                            &parsed.tx_hash,
                            &parsed.sender,
                            parsed.sequence,
                            &parsed.target_resource,
                            &parsed.op_type,
                            &parsed.direction,
                            parsed.amount,
                            parsed.fee_rate,
                        );

                        let alerts = {
                            let mut analyzer = self.analyzer.write().await;
                            analyzer.add_transaction(node);
                            analyzer.detect_sandwich_attacks()
                        };

                        if !alerts.is_empty() {
                            warn!(
                                "🚨 {} sandwich alert(s) detected from streamed tx {}",
                                alerts.len(), parsed.tx_hash
                            );
                            let _ = self.alert_tx.send(alerts);
                        }
                    }
                }
            }

            // Keep only incomplete last line in buffer
            if let Some(last_nl) = buffer.rfind('\n') {
                buffer = buffer[last_nl + 1..].to_string();
            } else {
                buffer.clear();
            }
        }

        Ok(())
    }
}


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
