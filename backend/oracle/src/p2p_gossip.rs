use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A signed price observation broadcast over the P2P Gossipsub network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedPriceObservation {
    /// ID of the observing Oracle node.
    pub oracle_id: String,
    /// Asset pair symbol (e.g., "XLM/USD", "BTC/USD").
    pub asset_pair: String,
    /// Price represented in 10^7 stroops / fixed point.
    pub price_stroops: u64,
    /// Unix timestamp of observation.
    pub timestamp: u64,
    /// Sequence/nonce of the oracle update.
    pub nonce: u64,
    /// Ed25519 signature over (oracle_id, asset_pair, price_stroops, timestamp, nonce).
    pub signature: Vec<u8>,
}

impl SignedPriceObservation {
    pub fn new(
        oracle_id: impl Into<String>,
        asset_pair: impl Into<String>,
        price_stroops: u64,
        nonce: u64,
        signature: Vec<u8>,
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            oracle_id: oracle_id.into(),
            asset_pair: asset_pair.into(),
            price_stroops,
            timestamp,
            nonce,
            signature,
        }
    }

    /// Payload buffer for signature verification.
    pub fn payload_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.oracle_id.as_bytes());
        bytes.extend_from_slice(self.asset_pair.as_bytes());
        bytes.extend_from_slice(&self.price_stroops.to_be_bytes());
        bytes.extend_from_slice(&self.timestamp.to_be_bytes());
        bytes.extend_from_slice(&self.nonce.to_be_bytes());
        bytes
    }
}

/// Configuration parameters for libp2p Gossipsub & Kademlia DHT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipConfig {
    pub topic_name: String,
    pub heartbeat_interval_ms: u64,
    pub max_transmit_size: usize,
    pub bootstrap_nodes: Vec<String>,
    pub enable_kademlia_dht: bool,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            topic_name: "pifp-oracle-prices-v1".to_string(),
            heartbeat_interval_ms: 1000,
            max_transmit_size: 65536,
            bootstrap_nodes: vec![
                "/dns4/bootstrap1.pifp-stellar.org/tcp/4001".to_string(),
                "/dns4/bootstrap2.pifp-stellar.org/tcp/4001".to_string(),
            ],
            enable_kademlia_dht: true,
        }
    }
}

/// P2P Gossip Network Node for Distributed Oracle Price Feeds.
pub struct P2PGossipNode {
    pub node_id: String,
    pub config: GossipConfig,
    active_peers: HashSet<String>,
    latest_observations: HashMap<String, Vec<SignedPriceObservation>>,
}

impl P2PGossipNode {
    pub fn new(node_id: impl Into<String>, config: GossipConfig) -> Self {
        Self {
            node_id: node_id.into(),
            config,
            active_peers: HashSet::new(),
            latest_observations: HashMap::new(),
        }
    }

    pub fn add_peer(&mut self, peer_id: impl Into<String>) {
        self.active_peers.insert(peer_id.into());
    }

    pub fn remove_peer(&mut self, peer_id: &str) {
        self.active_peers.remove(peer_id);
    }

    pub fn peer_count(&self) -> usize {
        self.active_peers.len()
    }

    /// Handle incoming gossipsub message payload.
    pub fn receive_message(&mut self, payload: &[u8]) -> Result<SignedPriceObservation, String> {
        let obs: SignedPriceObservation = serde_json::from_slice(payload)
            .map_err(|e| format!("Failed to deserialize gossip observation: {}", e))?;

        if obs.signature.is_empty() {
            return Err("Missing signature on price observation".to_string());
        }

        self.latest_observations
            .entry(obs.asset_pair.clone())
            .or_default()
            .push(obs.clone());

        Ok(obs)
    }

    /// Broadcast price observation to Gossipsub mesh.
    pub fn broadcast_observation(
        &mut self,
        asset_pair: &str,
        price_stroops: u64,
        nonce: u64,
        dummy_sig: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        let obs = SignedPriceObservation::new(&self.node_id, asset_pair, price_stroops, nonce, dummy_sig);
        
        self.latest_observations
            .entry(asset_pair.to_string())
            .or_default()
            .push(obs.clone());

        serde_json::to_vec(&obs).map_err(|e| format!("Failed to serialize observation: {}", e))
    }

    /// Compute aggregated median price from gathered P2P gossip observations.
    pub fn get_aggregated_median_price(&self, asset_pair: &str) -> Option<u64> {
        let obs_list = self.latest_observations.get(asset_pair)?;
        if obs_list.is_empty() {
            return None;
        }

        let mut prices: Vec<u64> = obs_list.iter().map(|o| o.price_stroops).collect();
        prices.sort_unstable();

        let mid = prices.len() / 2;
        if prices.len() % 2 == 0 {
            Some((prices[mid - 1] + prices[mid]) / 2)
        } else {
            Some(prices[mid])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p2p_gossip_broadcast_and_aggregation() {
        let mut node1 = P2PGossipNode::new("oracle-node-1", GossipConfig::default());
        let mut node2 = P2PGossipNode::new("oracle-node-2", GossipConfig::default());

        node1.add_peer("oracle-node-2");
        node2.add_peer("oracle-node-1");

        let payload1 = node1.broadcast_observation("XLM/USD", 1250000, 1, vec![1, 2, 3]).unwrap();
        let payload2 = node2.broadcast_observation("XLM/USD", 1270000, 1, vec![4, 5, 6]).unwrap();

        let obs_received = node1.receive_message(&payload2).unwrap();
        assert_eq!(obs_received.price_stroops, 1270000);

        let median = node1.get_aggregated_median_price("XLM/USD").unwrap();
        assert_eq!(median, 1260000);
    }
}
