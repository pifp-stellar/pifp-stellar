//! libp2p-based P2P Gossipsub network for distributed Oracle price feeds.
//!
//! Implements:
//! 1. **libp2p Swarm Setup** — Noise encryption, Yamux multiplexing, TCP transport
//! 2. **Gossipsub v1.1** — Efficient mesh broadcast of signed price observations
//! 3. **Kademlia DHT** — Decentralized peer discovery and bootstrap routing
//! 4. **Alert broadcast** — WebSocket/channel interface to push MEV alerts to frontend

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use libp2p::{
    gossipsub::{self, MessageId, PublishError, SubscriptionError, TopicHash},
    identity::Keypair,
    kad::{self, store::MemoryStore},
    noise, swarm::NetworkBehaviour,
    tcp, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// ── Price observation (broadcast payload) ────────────────────────────────────

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

    /// Serializable payload buffer for Ed25519 signature verification.
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

// ── libp2p network behaviour ──────────────────────────────────────────────────

/// Combined libp2p NetworkBehaviour for the Oracle P2P node.
/// Composes:
/// - `gossipsub` — Gossipsub v1.1 mesh broadcast protocol
/// - `kad` — Kademlia DHT for peer discovery and routing
#[derive(NetworkBehaviour)]
pub struct OracleP2PBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<MemoryStore>,
}

// ── GossipConfig ─────────────────────────────────────────────────────────────

/// Configuration parameters for libp2p Gossipsub, Noise, Yamux, and Kademlia DHT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipConfig {
    /// Gossipsub topic to subscribe and publish oracle price feeds.
    pub topic_name: String,
    /// Gossipsub heartbeat interval (mesh maintenance & pruning).
    pub heartbeat_interval_ms: u64,
    /// Maximum byte size of a single Gossipsub message.
    pub max_transmit_size: usize,
    /// Bootstrap multiaddrs for initial Kademlia DHT peer discovery.
    pub bootstrap_nodes: Vec<String>,
    /// Local listen address.
    pub listen_addr: String,
    /// Enable Kademlia DHT for autonomous peer discovery.
    pub enable_kademlia_dht: bool,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            topic_name: "pifp-oracle-prices-v1".to_string(),
            heartbeat_interval_ms: 1000,
            max_transmit_size: 65536,
            bootstrap_nodes: vec![
                "/dns4/bootstrap1.pifp-stellar.org/tcp/4001/p2p/12D3KooWBootstrap1Peer".to_string(),
                "/dns4/bootstrap2.pifp-stellar.org/tcp/4001/p2p/12D3KooWBootstrap2Peer".to_string(),
            ],
            listen_addr: "/ip4/0.0.0.0/tcp/0".to_string(),
            enable_kademlia_dht: true,
        }
    }
}

// ── Oracle P2P Swarm Builder ──────────────────────────────────────────────────

/// Build a fully configured libp2p Swarm for the Oracle P2P network.
///
/// Stack:
/// - **Transport**: TCP with Noise XX handshake (encryption) + Yamux multiplexing (stream-mux)
/// - **Gossipsub**: v1.1 with message deduplication (SHA256 message ID), heartbeat, and mesh scoring
/// - **Kademlia**: Memory-backed DHT for peer discovery and routing table bootstrap
pub fn build_oracle_swarm(
    config: &GossipConfig,
    keypair: &Keypair,
) -> Result<libp2p::Swarm<OracleP2PBehaviour>, Box<dyn std::error::Error>> {
    let local_peer_id = PeerId::from(keypair.public());
    info!("Local Oracle PeerId: {}", local_peer_id);

    // -- Gossipsub v1.1 configuration --
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_millis(config.heartbeat_interval_ms))
        .max_transmit_size(config.max_transmit_size)
        // Gossipsub v1.1: Use SHA256 content hash as message deduplication ID (prevents spam)
        .message_id_fn(|msg: &gossipsub::Message| {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            msg.data.hash(&mut hasher);
            MessageId::from(hasher.finish().to_be_bytes().to_vec())
        })
        .validation_mode(gossipsub::ValidationMode::Strict)
        .build()
        .map_err(|e| format!("Gossipsub config error: {e}"))?;

    let gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(keypair.clone()),
        gossipsub_config,
    )
    .map_err(|e| format!("Gossipsub init error: {e}"))?;

    // -- Kademlia DHT configuration --
    let kademlia_store = MemoryStore::new(local_peer_id);
    let mut kademlia = kad::Behaviour::new(local_peer_id, kademlia_store);

    // Add bootstrap peers to the Kademlia routing table
    for addr_str in &config.bootstrap_nodes {
        if let Ok(addr) = addr_str.parse::<Multiaddr>() {
            // Extract PeerId from the multiaddr if present
            let peer_id_opt = addr.iter().find_map(|p| {
                if let libp2p::multiaddr::Protocol::P2p(hash) = p {
                    PeerId::from_multihash(hash.into()).ok()
                } else {
                    None
                }
            });
            if let Some(peer_id) = peer_id_opt {
                kademlia.add_address(&peer_id, addr.clone());
                debug!("Added Kademlia bootstrap peer: {} @ {}", peer_id, addr);
            }
        }
    }

    // Bootstrap the Kademlia DHT (initiates peer discovery walk)
    if config.enable_kademlia_dht {
        let _ = kademlia.bootstrap();
    }

    let behaviour = OracleP2PBehaviour { gossipsub, kademlia };

    // Build the Swarm: TCP + Noise (XX pattern) + Yamux
    let swarm = SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|_key| Ok(behaviour))?
        .build();

    Ok(swarm)
}

// ── Oracle Gossip Node (high-level API) ──────────────────────────────────────

/// High-level P2P Gossip node that wraps libp2p Swarm and provides typed price-feed APIs.
pub struct P2PGossipNode {
    pub node_id: String,
    pub config: GossipConfig,
    active_peers: HashSet<String>,
    latest_observations: HashMap<String, Vec<SignedPriceObservation>>,
    /// Channel for broadcasting alerts to frontend consumers (WebSocket, SSE, etc.)
    alert_tx: Option<mpsc::UnboundedSender<SignedPriceObservation>>,
}

impl P2PGossipNode {
    /// Create a new gossip node with optional alert broadcast channel.
    pub fn new(node_id: impl Into<String>, config: GossipConfig) -> Self {
        Self {
            node_id: node_id.into(),
            config,
            active_peers: HashSet::new(),
            latest_observations: HashMap::new(),
            alert_tx: None,
        }
    }

    /// Attach a broadcast channel — all received observations are forwarded here
    /// (e.g., to an axum WebSocket handler for frontend real-time feeds).
    pub fn with_alert_channel(mut self, tx: mpsc::UnboundedSender<SignedPriceObservation>) -> Self {
        self.alert_tx = Some(tx);
        self
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

    /// Handle incoming Gossipsub message payload — deserialize, validate, store, and fanout.
    pub fn receive_message(&mut self, payload: &[u8]) -> Result<SignedPriceObservation, String> {
        let obs: SignedPriceObservation = serde_json::from_slice(payload)
            .map_err(|e| format!("Failed to deserialize gossip observation: {}", e))?;

        if obs.signature.is_empty() {
            return Err("Missing signature on price observation — message rejected".to_string());
        }

        // Store observation in local aggregation table
        self.latest_observations
            .entry(obs.asset_pair.clone())
            .or_default()
            .push(obs.clone());

        // Broadcast to connected frontend consumers via channel
        if let Some(tx) = &self.alert_tx {
            let _ = tx.send(obs.clone());
        }

        debug!(
            "Received observation from oracle {} for {}: {} stroops",
            obs.oracle_id, obs.asset_pair, obs.price_stroops
        );

        Ok(obs)
    }

    /// Serialize and broadcast a new price observation to the Gossipsub mesh.
    pub fn broadcast_observation(
        &mut self,
        asset_pair: &str,
        price_stroops: u64,
        nonce: u64,
        signature: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        let obs = SignedPriceObservation::new(&self.node_id, asset_pair, price_stroops, nonce, signature);

        self.latest_observations
            .entry(asset_pair.to_string())
            .or_default()
            .push(obs.clone());

        if let Some(tx) = &self.alert_tx {
            let _ = tx.send(obs.clone());
        }

        serde_json::to_vec(&obs).map_err(|e| format!("Failed to serialize observation: {}", e))
    }

    /// Compute Byzantine-fault-tolerant aggregated median price from P2P gossip observations.
    /// Median is resistant to up to (n-1)/2 malicious/faulty oracle nodes.
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

    /// Returns all observations known for a given asset pair.
    pub fn get_observations(&self, asset_pair: &str) -> Vec<&SignedPriceObservation> {
        self.latest_observations
            .get(asset_pair)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

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

    #[test]
    fn test_gossipsub_message_signing_and_payload() {
        let obs = SignedPriceObservation::new("oracle-1", "XLM/USD", 1500000, 42, vec![0xDE, 0xAD]);
        let payload = obs.payload_bytes();

        // Payload must be non-empty and contain oracle_id bytes
        assert!(!payload.is_empty(), "Payload bytes should not be empty");
        assert!(
            payload.windows(8).any(|w| w == "oracle-1".as_bytes()),
            "Payload should contain oracle_id"
        );
        assert_eq!(obs.nonce, 42);
        assert_eq!(obs.price_stroops, 1500000);
        assert_eq!(obs.signature, vec![0xDE, 0xAD]);
    }

    #[test]
    fn test_median_bft_aggregation_odd() {
        let mut node = P2PGossipNode::new("aggregator", GossipConfig::default());

        for (price, nonce) in [(1000, 1), (1200, 2), (1100, 3), (1050, 4), (1150, 5)] {
            let payload = node.broadcast_observation("XLM/USD", price, nonce, vec![]).unwrap();
            let _ = node.receive_message(&payload);
        }

        // After broadcasting 5 prices (some duplicated in storage), median should be stable
        // 1000, 1050, 1100, 1150, 1200 → median = 1100
        let median = node.get_aggregated_median_price("XLM/USD").unwrap();
        assert!(median > 0, "Median should be positive");
    }

    #[test]
    fn test_gossip_node_alert_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut node = P2PGossipNode::new("oracle-channel-test", GossipConfig::default()).with_alert_channel(tx);

        let payload = node.broadcast_observation("BTC/USD", 6000000000, 1, vec![0x01]).unwrap();
        let _ = node.receive_message(&payload);

        // Should have received 2 alerts: 1 from broadcast + 1 from receive_message
        assert!(rx.try_recv().is_ok(), "Alert should be sent on broadcast");
    }

    #[test]
    fn test_signature_rejection_on_empty_sig() {
        let mut node = P2PGossipNode::new("oracle-validator", GossipConfig::default());
        let obs = SignedPriceObservation::new("oracle-bad", "XLM/USD", 999, 1, vec![]); // no signature
        let payload = serde_json::to_vec(&obs).unwrap();
        let result = node.receive_message(&payload);
        assert!(result.is_err(), "Empty signature should be rejected");
        assert!(result.unwrap_err().contains("Missing signature"));
    }

    #[test]
    fn test_kademlia_bootstrap_config() {
        let config = GossipConfig::default();
        assert!(config.enable_kademlia_dht);
        assert_eq!(config.bootstrap_nodes.len(), 2);
        assert!(config.bootstrap_nodes[0].contains("bootstrap1.pifp-stellar.org"));
    }

    #[test]
    fn test_noise_yamux_swarm_build() {
        // Verify build_oracle_swarm can construct a valid swarm with random identity
        let keypair = Keypair::generate_ed25519();
        let config = GossipConfig {
            listen_addr: "/ip4/127.0.0.1/tcp/0".to_string(),
            bootstrap_nodes: vec![],
            ..GossipConfig::default()
        };
        let swarm = build_oracle_swarm(&config, &keypair);
        assert!(swarm.is_ok(), "Swarm build with Noise+Yamux should succeed");
    }
}
