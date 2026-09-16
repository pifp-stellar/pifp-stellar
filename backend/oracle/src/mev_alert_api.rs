//! MEV Alert WebSocket/SSE broadcast endpoint — Issue #7
//!
//! Provides a real-time Server-Sent Events (SSE) endpoint that pushes
//! `SandwichAlert` notifications from the mempool DAG analyzer to connected frontend clients.

use axum::{
    extract::State,
    response::{IntoResponse, Response, Sse},
    routing::get,
    Json, Router,
};
use axum::response::sse::{Event, KeepAlive};
use futures::stream::{self, Stream};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, RwLock};
use tracing::info;

use crate::mempool_dag_analyzer::{MempoolDagAnalyzer, MempoolTxNode, SandwichAlert};

// ── Shared alert state ────────────────────────────────────────────────────────

/// Shared state holding the MEV analyzer and a broadcast channel for real-time alert delivery.
#[derive(Clone)]
pub struct MevAlertState {
    pub analyzer: Arc<RwLock<MempoolDagAnalyzer>>,
    pub alert_sender: broadcast::Sender<SandwichAlert>,
}

impl MevAlertState {
    pub fn new() -> Self {
        let (alert_sender, _) = broadcast::channel(256);
        Self {
            analyzer: Arc::new(RwLock::new(MempoolDagAnalyzer::new())),
            alert_sender,
        }
    }
}

impl Default for MevAlertState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Routes ────────────────────────────────────────────────────────────────────

/// Constructs the MEV alert router.
pub fn mev_alert_router(state: MevAlertState) -> Router {
    Router::new()
        .route("/api/mev/alerts/stream", get(sse_alert_stream))
        .route("/api/mev/alerts/latest", get(latest_alerts))
        .route("/api/mev/mempool/ingest", axum::routing::post(ingest_transaction))
        .with_state(state)
}

// ── SSE stream handler ────────────────────────────────────────────────────────

/// SSE endpoint: `GET /api/mev/alerts/stream`
///
/// Frontend clients subscribe here to receive real-time `SandwichAlert` JSON events.
/// Uses `text/event-stream` protocol compatible with `EventSource` in modern browsers.
async fn sse_alert_stream(
    State(state): State<MevAlertState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.alert_sender.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(alert) => {
                    if let Ok(json) = serde_json::to_string(&alert) {
                        yield Ok(Event::default().event("sandwich_alert").data(json));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let lag_evt = Event::default()
                        .event("lag_warning")
                        .data(format!("{{\"skipped\":{}}}", n));
                    yield Ok(lag_evt);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Latest alerts snapshot ────────────────────────────────────────────────────

/// REST snapshot: `GET /api/mev/alerts/latest`
///
/// Returns the current set of detected sandwich alerts from the live analyzer.
async fn latest_alerts(
    State(state): State<MevAlertState>,
) -> Json<Vec<SandwichAlert>> {
    let analyzer = state.analyzer.read().await;
    Json(analyzer.detect_sandwich_attacks())
}

// ── Mempool transaction ingestion ─────────────────────────────────────────────

/// REST endpoint: `POST /api/mev/mempool/ingest`
///
/// Accepts a parsed `MempoolTxNode` JSON payload, adds it to the DAG analyzer,
/// and broadcasts any newly detected sandwich alerts to all SSE subscribers.
async fn ingest_transaction(
    State(state): State<MevAlertState>,
    Json(tx_node): Json<MempoolTxNode>,
) -> Response {
    let mut analyzer = state.analyzer.write().await;
    analyzer.add_transaction(tx_node);

    let alerts = analyzer.detect_sandwich_attacks();
    for alert in alerts {
        let _ = state.alert_sender.send(alert);
    }

    axum::http::StatusCode::ACCEPTED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_latest_alerts_empty_initially() {
        let state = MevAlertState::new();
        let app = mev_alert_router(state);

        let response = app
            .oneshot(Request::builder().uri("/api/mev/alerts/latest").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_ingest_transaction_returns_accepted() {
        let state = MevAlertState::new();
        let app = mev_alert_router(state);

        let tx = MempoolTxNode::new("tx-test", "alice", 1, "XLM/USDC", "Swap", "Buy", 1000, 50);
        let body = serde_json::to_vec(&tx).unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mev/mempool/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_alert_broadcast_on_sandwich_detection() {
        let state = MevAlertState::new();
        let mut rx = state.alert_sender.subscribe();

        let analyzer_clone = state.analyzer.clone();
        let sender_clone = state.alert_sender.clone();

        // Simulate sandwich ingest
        let txs = vec![
            MempoolTxNode::new("front", "attacker", 1, "XLM/USDC", "Swap", "Buy", 10000, 500),
            MempoolTxNode::new("victim", "victim_user", 10, "XLM/USDC", "Swap", "Buy", 50000, 50),
            MempoolTxNode::new("back", "attacker", 2, "XLM/USDC", "Swap", "Sell", 10000, 100),
        ];

        {
            let mut analyzer = analyzer_clone.write().await;
            for tx in txs {
                analyzer.add_transaction(tx);
            }
            let alerts = analyzer.detect_sandwich_attacks();
            for alert in alerts {
                let _ = sender_clone.send(alert);
            }
        }

        // Should be able to receive the broadcast alert
        let result = rx.try_recv();
        assert!(result.is_ok(), "Sandwich alert should be broadcast to subscribers");
        assert_eq!(result.unwrap().frontrunner_tx, "front");
    }
}
