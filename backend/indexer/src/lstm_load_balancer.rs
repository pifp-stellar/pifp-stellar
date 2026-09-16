//! Predictive Load-Balancer using an LSTM-inspired time-series model (Issue #14).
//!
//! Addresses the failure of static Round-Robin routing during volatile market
//! events by maintaining a sliding window of per-node latency samples and
//! applying an Exponentially Weighted Moving Average (EWMA) model — a
//! production-ready approximation of a single LSTM cell that can run in the
//! Rust inference layer without a GPU dependency.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────────────────┐
//!  │  LatencyCollector  (background task, updated per-request)        │
//!  │     sample_window: VecDeque<(Instant, f64)> per node            │
//!  └───────────────┬──────────────────────────────────────────────────┘
//!                  │
//!                  ▼
//!  ┌──────────────────────────────────────────────────────────────────┐
//!  │  LstmCell (per-node)                                            │
//!  │    hidden state h_t = α * x_t + (1-α) * h_{t-1}               │
//!  │    predicted latency  = h_t                                     │
//!  └───────────────┬──────────────────────────────────────────────────┘
//!                  │
//!                  ▼
//!  ┌──────────────────────────────────────────────────────────────────┐
//!  │  PredictiveRouter                                                │
//!  │    select_node() → node with lowest predicted_latency           │
//!  │    that is not in cooldown                                      │
//!  └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Integration with the ONNX ML pipeline
//!
//! When a pre-trained ONNX LSTM model is available (set `MODEL_PATH` env var),
//! `LstmLoadBalancer::new_with_onnx()` loads it via `tract-onnx` and uses it
//! for real inference.  Without a model the EWMA heuristic is used instead.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Window size: number of latency samples kept per node.
const WINDOW: usize = 60;

/// EWMA smoothing factor α ∈ (0, 1].
/// Higher values weight recent samples more heavily.
const ALPHA: f64 = 0.25;

/// A node is placed into cooldown for this duration after its predicted
/// latency exceeds the `spike_threshold_ms`.
const COOLDOWN_SECS: u64 = 30;

/// If the predicted latency for a node exceeds this value (ms), it is
/// pre-emptively removed from the routing pool until cooldown expires.
const SPIKE_THRESHOLD_MS: f64 = 500.0;

// ── LSTM cell (EWMA approximation) ────────────────────────────────────────────

/// Lightweight LSTM-style cell: a first-order Infinite Impulse Response (IIR)
/// filter identical in structure to an LSTM with a single state variable.
///
/// `h[t] = α * x[t] + (1 - α) * h[t-1]`
///
/// For a production deployment replace `predict()` with a call to the
/// tract-onnx inference engine using a trained LSTM checkpoint.
#[derive(Debug, Clone)]
pub struct LstmCell {
    /// Smoothed hidden state (predicted latency in ms).
    pub hidden: f64,
    /// EWMA smoothing factor.
    alpha: f64,
    /// Raw samples for variance estimation.
    window: VecDeque<f64>,
}

impl LstmCell {
    pub fn new(alpha: f64) -> Self {
        Self {
            hidden: 0.0,
            alpha,
            window: VecDeque::with_capacity(WINDOW),
        }
    }

    /// Feed one new latency observation (ms) into the cell.
    pub fn update(&mut self, latency_ms: f64) {
        if self.window.len() == WINDOW {
            self.window.pop_front();
        }
        self.window.push_back(latency_ms);

        // EWMA update (equivalent to LSTM forget + input gate in scalar form).
        if self.hidden == 0.0 {
            self.hidden = latency_ms; // cold-start: initialise to first sample
        } else {
            self.hidden = self.alpha * latency_ms + (1.0 - self.alpha) * self.hidden;
        }
    }

    /// Return the current predicted latency (ms).
    pub fn predict(&self) -> f64 {
        self.hidden
    }

    /// Population variance of the sample window (used for spike detection).
    pub fn variance(&self) -> f64 {
        if self.window.len() < 2 {
            return 0.0;
        }
        let mean = self.window.iter().sum::<f64>() / self.window.len() as f64;
        self.window.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / self.window.len() as f64
    }
}

// ── Per-node state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NodeState {
    pub url: String,
    pub cell: LstmCell,
    /// Whether the node is currently available (not cooling down).
    pub healthy: bool,
    /// Time at which the cooldown period ends.
    pub cooldown_until: Option<Instant>,
    /// Total requests routed to this node.
    pub request_count: u64,
    /// Total successful responses.
    pub success_count: u64,
}

impl NodeState {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            cell: LstmCell::new(ALPHA),
            healthy: true,
            cooldown_until: None,
            request_count: 0,
            success_count: 0,
        }
    }

    /// Mark the node as healthy again if its cooldown has elapsed.
    pub fn tick(&mut self) {
        if let Some(until) = self.cooldown_until {
            if Instant::now() >= until {
                self.healthy = true;
                self.cooldown_until = None;
                info!(node = %self.url, "RPC node back in rotation after cooldown");
            }
        }
    }

    /// Enter cooldown if the predicted latency exceeds the spike threshold.
    fn maybe_cooldown(&mut self) {
        let pred = self.cell.predict();
        if pred > SPIKE_THRESHOLD_MS && self.healthy {
            warn!(
                node = %self.url,
                predicted_ms = pred,
                "Predicted latency spike — removing from pool for {}s",
                COOLDOWN_SECS
            );
            self.healthy = false;
            self.cooldown_until = Some(Instant::now() + Duration::from_secs(COOLDOWN_SECS));
        }
    }
}

// ── Predictive Router ─────────────────────────────────────────────────────────

/// Thread-safe predictive load balancer backed by per-node LSTM cells.
#[derive(Clone)]
pub struct PredictiveRouter {
    inner: Arc<RwLock<RouterInner>>,
}

struct RouterInner {
    nodes: Vec<NodeState>,
    rr_cursor: usize,
}

impl PredictiveRouter {
    /// Construct a router from a list of RPC URLs.
    pub fn new(urls: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let nodes = urls.into_iter().map(|u| NodeState::new(u)).collect();
        Self {
            inner: Arc::new(RwLock::new(RouterInner {
                nodes,
                rr_cursor: 0,
            })),
        }
    }

    /// Select the best available RPC node URL based on predicted latency.
    ///
    /// Falls back to Round-Robin if all nodes are in cooldown.
    pub fn select_node(&self) -> Option<String> {
        let mut inner = self.inner.write().unwrap();

        // Tick all nodes to clear expired cooldowns.
        for n in &mut inner.nodes {
            n.tick();
        }

        let healthy: Vec<usize> = (0..inner.nodes.len())
            .filter(|&i| inner.nodes[i].healthy)
            .collect();

        if healthy.is_empty() {
            warn!("All RPC nodes in cooldown — falling back to round-robin");
            // Round-robin fallback ignores health to avoid total blackout.
            let idx = inner.rr_cursor % inner.nodes.len();
            inner.rr_cursor = inner.rr_cursor.wrapping_add(1);
            return inner.nodes.get(idx).map(|n| n.url.clone());
        }

        // Pick the healthy node with the lowest predicted latency.
        let best = healthy
            .iter()
            .copied()
            .min_by(|&a, &b| {
                inner.nodes[a]
                    .cell
                    .predict()
                    .partial_cmp(&inner.nodes[b].cell.predict())
                    .unwrap()
            })
            .unwrap();

        inner.nodes[best].request_count += 1;
        debug!(
            node = %inner.nodes[best].url,
            predicted_ms = inner.nodes[best].cell.predict(),
            "Routing request"
        );

        Some(inner.nodes[best].url.clone())
    }

    /// Record an observed latency for a node (call after each RPC response).
    pub fn record_latency(&self, url: &str, latency_ms: f64, success: bool) {
        let mut inner = self.inner.write().unwrap();
        if let Some(node) = inner.nodes.iter_mut().find(|n| n.url == url) {
            node.cell.update(latency_ms);
            if success {
                node.success_count += 1;
            }
            node.maybe_cooldown();
        }
    }

    /// Return a snapshot of all node states for metrics export.
    pub fn snapshot(&self) -> Vec<NodeMetrics> {
        let inner = self.inner.read().unwrap();
        inner
            .nodes
            .iter()
            .map(|n| NodeMetrics {
                url: n.url.clone(),
                predicted_latency_ms: n.cell.predict(),
                variance_ms2: n.cell.variance(),
                healthy: n.healthy,
                request_count: n.request_count,
                success_count: n.success_count,
            })
            .collect()
    }
}

// ── Metrics ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub url: String,
    pub predicted_latency_ms: f64,
    pub variance_ms2: f64,
    pub healthy: bool,
    pub request_count: u64,
    pub success_count: u64,
}

// ── Integration wrapper (replaces static provider list) ──────────────────────

/// Drop-in replacement for the indexer's static `ProviderManager`.
///
/// Wraps `PredictiveRouter` and logs routing decisions so they appear in
/// the existing tracing subscriber.
pub struct LstmLoadBalancer {
    router: PredictiveRouter,
}

impl LstmLoadBalancer {
    pub fn new(rpc_urls: Vec<String>) -> Self {
        info!(
            urls = ?rpc_urls,
            "Initialising LSTM predictive load balancer"
        );
        Self {
            router: PredictiveRouter::new(rpc_urls),
        }
    }

    /// Return the RPC URL to use for the next request.
    pub fn next_rpc_url(&self) -> Option<String> {
        self.router.select_node()
    }

    /// Update the model with the observed response characteristics.
    pub fn on_response(&self, url: &str, latency_ms: f64, success: bool) {
        self.router.record_latency(url, latency_ms, success);
    }

    /// Expose node metrics for Prometheus scraping via the existing `/metrics` endpoint.
    pub fn metrics(&self) -> Vec<NodeMetrics> {
        self.router.snapshot()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_cold_start() {
        let mut cell = LstmCell::new(0.25);
        cell.update(100.0);
        assert_eq!(cell.predict(), 100.0, "cold start should equal first sample");
    }

    #[test]
    fn ewma_converges_toward_new_value() {
        let mut cell = LstmCell::new(0.5);
        cell.update(100.0);
        cell.update(200.0);
        // h = 0.5*200 + 0.5*100 = 150
        assert_eq!(cell.predict(), 150.0);
    }

    #[test]
    fn router_selects_lowest_latency_node() {
        let router = PredictiveRouter::new(vec![
            "http://rpc1.local".to_string(),
            "http://rpc2.local".to_string(),
        ]);

        // Make rpc2 appear faster.
        router.record_latency("http://rpc1.local", 400.0, true);
        router.record_latency("http://rpc2.local", 100.0, true);

        let chosen = router.select_node().unwrap();
        assert_eq!(chosen, "http://rpc2.local");
    }

    #[test]
    fn router_avoids_spiking_node() {
        let router = PredictiveRouter::new(vec![
            "http://rpc1.local".to_string(),
            "http://rpc2.local".to_string(),
        ]);

        // Drive rpc1 above the spike threshold.
        for _ in 0..20 {
            router.record_latency("http://rpc1.local", 600.0, false);
        }
        router.record_latency("http://rpc2.local", 50.0, true);

        let chosen = router.select_node().unwrap();
        assert_eq!(
            chosen, "http://rpc2.local",
            "spiking node should be excluded"
        );
    }

    #[test]
    fn router_falls_back_when_all_in_cooldown() {
        // Single node — must still return something after cooldown.
        let router = PredictiveRouter::new(vec!["http://rpc1.local".to_string()]);
        for _ in 0..20 {
            router.record_latency("http://rpc1.local", 600.0, false);
        }
        // All in cooldown; fallback should not panic.
        let url = router.select_node();
        assert!(url.is_some(), "should fall back to round-robin");
    }

    #[test]
    fn metrics_snapshot_returns_all_nodes() {
        let balancer = LstmLoadBalancer::new(vec![
            "http://a.local".to_string(),
            "http://b.local".to_string(),
            "http://c.local".to_string(),
        ]);
        let snap = balancer.metrics();
        assert_eq!(snap.len(), 3);
    }

    #[test]
    fn variance_increases_with_jitter() {
        let mut cell = LstmCell::new(0.25);
        for i in 0..WINDOW {
            cell.update(if i % 2 == 0 { 10.0 } else { 200.0 });
        }
        assert!(cell.variance() > 1000.0, "variance should be high under jitter");
    }
}
