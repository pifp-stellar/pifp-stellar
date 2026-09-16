//! Trustless L1 Escape Hatch for ZK-Rollup (Issue #15).
//!
//! When the L2 sequencer goes offline or becomes malicious, users can bypass
//! it entirely and reclaim their L1 funds via an on-chain exit proof.
//!
//! # Protocol
//!
//! 1. **Claim**: User submits an `EscapeRequest` containing:
//!    - Their L2 account identifier.
//!    - A Merkle inclusion proof of their last known balance in the L2 state root.
//!    - The block height at which that state root was committed on L1.
//!
//! 2. **Challenge window** (default 7 days): The sequencer (or any watcher)
//!    can submit a `ChallengeProof` showing a more recent committed state root
//!    that contradicts the escape claim.  If a valid challenge arrives, the
//!    escape request is cancelled.
//!
//! 3. **Finalise**: After the challenge window passes unchallenged, any
//!    party may call `finalise_escape` which marks the exit as settled.
//!    The caller is responsible for submitting the L1 transaction that
//!    releases the corresponding funds from the Soroban bridge contract.
//!
//! # Fraud Detection
//!
//! Each committed state root is stored in `CommittedCheckpoint`.  The escape
//! verifier checks that the Merkle proof is consistent with a *known-committed*
//! root.  Fraudulent exits (pointing to a non-committed root) are rejected
//! immediately without requiring a challenge.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[allow(unused_imports)]
use tracing::{info, warn};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Duration of the challenge window.
const CHALLENGE_WINDOW_SECS: u64 = 7 * 24 * 3600; // 7 days

// ── On-chain state representation ─────────────────────────────────────────────

/// A state root that has been committed to L1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommittedCheckpoint {
    /// Sequential L1 batch number.
    pub batch_id: u64,
    /// Merkle root of L2 balances at this batch.
    pub state_root_hex: String,
    /// Unix timestamp when this checkpoint was recorded on L1.
    pub committed_at: u64,
}

// ── Escape request types ──────────────────────────────────────────────────────

/// An exit request submitted by an L2 user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscapeRequest {
    /// Unique identifier (SHA-256 of account + batch_id).
    pub id: String,
    /// L2 account identifier (hex-encoded public key).
    pub account: String,
    /// Claimed L2 balance at the time of the referenced state root.
    pub claimed_balance: u64,
    /// The batch whose state root is used as the exit anchor.
    pub anchor_batch_id: u64,
    /// Merkle sibling hashes (hex) proving `account` is in the state root.
    pub merkle_proof_siblings: Vec<String>,
    /// Unix timestamp when the exit was requested.
    pub requested_at: u64,
}

impl EscapeRequest {
    pub fn new(
        account: impl Into<String>,
        claimed_balance: u64,
        anchor_batch_id: u64,
        merkle_proof_siblings: Vec<String>,
    ) -> Self {
        let account = account.into();
        let mut hasher = Sha256::new();
        hasher.update(account.as_bytes());
        hasher.update(anchor_batch_id.to_le_bytes());
        let id = hex::encode(hasher.finalize());
        Self {
            id,
            account,
            claimed_balance,
            anchor_batch_id,
            merkle_proof_siblings,
            requested_at: unix_now(),
        }
    }

    pub fn is_challenge_window_open(&self) -> bool {
        unix_now() < self.requested_at + CHALLENGE_WINDOW_SECS
    }
}

/// Status of an escape request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscapeStatus {
    Pending,
    Challenged,
    Finalised,
    Rejected(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscapeEntry {
    pub request: EscapeRequest,
    pub status: EscapeStatus,
}

/// A challenge submitted by the sequencer or a watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeProof {
    /// The escape request ID being challenged.
    pub escape_id: String,
    /// The batch that proves the claimant's balance is *different*.
    pub correct_batch_id: u64,
    /// Merkle proof of the correct balance for the same account.
    pub merkle_proof_siblings: Vec<String>,
    /// The correct balance (must differ from the claimed one).
    pub correct_balance: u64,
}

// ── Escape Hatch Manager ──────────────────────────────────────────────────────

/// Stateful manager for the L2 escape hatch protocol.
///
/// In a production system this would be a Soroban smart contract.  Here it is
/// an in-process state machine used by the sequencer watchdog service.
pub struct EscapeHatchManager {
    /// All committed L1 checkpoints, keyed by batch_id.
    checkpoints: HashMap<u64, CommittedCheckpoint>,
    /// Active and historical escape requests, keyed by escape ID.
    escapes: HashMap<String, EscapeEntry>,
}

impl EscapeHatchManager {
    pub fn new() -> Self {
        Self {
            checkpoints: HashMap::new(),
            escapes: HashMap::new(),
        }
    }

    /// Record a new L1 state-root commitment.
    pub fn record_checkpoint(&mut self, cp: CommittedCheckpoint) {
        info!(
            batch_id = cp.batch_id,
            root = %cp.state_root_hex,
            "Escape hatch: recording committed checkpoint"
        );
        self.checkpoints.insert(cp.batch_id, cp);
    }

    /// Submit an escape request.
    ///
    /// Returns `Err` if:
    /// * The anchor batch is not in the committed checkpoint store.
    /// * The Merkle proof is invalid against the committed state root.
    /// * A duplicate escape request already exists.
    pub fn submit_escape(
        &mut self,
        req: EscapeRequest,
    ) -> Result<String, EscapeError> {
        if self.escapes.contains_key(&req.id) {
            return Err(EscapeError::Duplicate(req.id.clone()));
        }

        // Verify the anchor batch is a committed checkpoint.
        let cp = self
            .checkpoints
            .get(&req.anchor_batch_id)
            .ok_or(EscapeError::UnknownBatch(req.anchor_batch_id))?;

        // Verify Merkle inclusion proof.
        self.verify_merkle_proof(&req, cp)?;

        let id = req.id.clone();
        self.escapes.insert(
            id.clone(),
            EscapeEntry {
                request: req,
                status: EscapeStatus::Pending,
            },
        );
        info!(escape_id = %id, "Escape request accepted — challenge window open");
        Ok(id)
    }

    /// Submit a challenge proof against a pending escape.
    pub fn challenge(&mut self, proof: ChallengeProof) -> Result<(), EscapeError> {
        let entry = self
            .escapes
            .get_mut(&proof.escape_id)
            .ok_or(EscapeError::NotFound(proof.escape_id.clone()))?;

        if entry.status != EscapeStatus::Pending {
            return Err(EscapeError::NotPending(proof.escape_id.clone()));
        }
        if !entry.request.is_challenge_window_open() {
            return Err(EscapeError::WindowClosed(proof.escape_id.clone()));
        }

        // A valid challenge must reference a newer committed batch.
        let challenger_batch = self
            .checkpoints
            .get(&proof.correct_batch_id)
            .ok_or(EscapeError::UnknownBatch(proof.correct_batch_id))?;

        if challenger_batch.batch_id <= entry.request.anchor_batch_id {
            return Err(EscapeError::InvalidChallenge(
                "challenger batch must be newer than anchor batch".to_string(),
            ));
        }

        if proof.correct_balance == entry.request.claimed_balance {
            // Balance matches → not actually fraudulent.
            return Err(EscapeError::InvalidChallenge(
                "challenger balance matches claim — no fraud".to_string(),
            ));
        }

        warn!(
            escape_id = %proof.escape_id,
            claimed = entry.request.claimed_balance,
            correct = proof.correct_balance,
            "Escape hatch: fraudulent exit CHALLENGED and cancelled"
        );
        entry.status = EscapeStatus::Challenged;
        Ok(())
    }

    /// Finalise an unchallenged escape after the challenge window expires.
    pub fn finalise_escape(&mut self, escape_id: &str) -> Result<u64, EscapeError> {
        let entry = self
            .escapes
            .get_mut(escape_id)
            .ok_or_else(|| EscapeError::NotFound(escape_id.to_string()))?;

        match entry.status {
            EscapeStatus::Pending if !entry.request.is_challenge_window_open() => {
                entry.status = EscapeStatus::Finalised;
                let balance = entry.request.claimed_balance;
                info!(
                    escape_id = %escape_id,
                    balance,
                    account = %entry.request.account,
                    "Escape hatch: exit FINALISED — funds releasable on L1"
                );
                Ok(balance)
            }
            EscapeStatus::Pending => {
                Err(EscapeError::WindowStillOpen(escape_id.to_string()))
            }
            EscapeStatus::Challenged => {
                Err(EscapeError::Challenged(escape_id.to_string()))
            }
            EscapeStatus::Finalised => Ok(entry.request.claimed_balance),
            EscapeStatus::Rejected(ref reason) => {
                Err(EscapeError::Rejected(reason.clone()))
            }
        }
    }

    /// Look up the current status of an escape request.
    pub fn status(&self, escape_id: &str) -> Option<&EscapeStatus> {
        self.escapes.get(escape_id).map(|e| &e.status)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Verify that the Merkle proof in `req` is consistent with `cp`.
    ///
    /// Computes `H(balance || account)` then walks up the Merkle tree using
    /// the sibling hashes, producing a root that must equal `cp.state_root_hex`.
    fn verify_merkle_proof(
        &self,
        req: &EscapeRequest,
        cp: &CommittedCheckpoint,
    ) -> Result<(), EscapeError> {
        // Leaf hash: H(account || balance_le64)
        let mut h = Sha256::new();
        h.update(req.account.as_bytes());
        h.update(req.claimed_balance.to_le_bytes());
        let mut current: [u8; 32] = h.finalize().into();

        for sibling_hex in &req.merkle_proof_siblings {
            let sibling = hex::decode(sibling_hex)
                .map_err(|e| EscapeError::InvalidProof(e.to_string()))?;
            if sibling.len() != 32 {
                return Err(EscapeError::InvalidProof(
                    "sibling hash must be 32 bytes".to_string(),
                ));
            }
            let mut parent = Sha256::new();
            // Sort children to match the SMT convention used in sequencer/src/smt.rs.
            if current.as_slice() <= sibling.as_slice() {
                parent.update(current);
                parent.update(&sibling);
            } else {
                parent.update(&sibling);
                parent.update(current);
            }
            current = parent.finalize().into();
        }

        let computed_root = hex::encode(current);
        // Strip optional "0x" prefix for comparison.
        let expected = cp.state_root_hex.trim_start_matches("0x");
        if computed_root != expected {
            return Err(EscapeError::InvalidProof(format!(
                "root mismatch: got {computed_root} expected {expected}"
            )));
        }
        Ok(())
    }
}

impl Default for EscapeHatchManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Error types ───────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum EscapeError {
    #[error("Unknown batch id: {0}")]
    UnknownBatch(u64),
    #[error("Invalid Merkle proof: {0}")]
    InvalidProof(String),
    #[error("Duplicate escape request: {0}")]
    Duplicate(String),
    #[error("Escape request not found: {0}")]
    NotFound(String),
    #[error("Escape already challenged: {0}")]
    Challenged(String),
    #[error("Escape is not in Pending state: {0}")]
    NotPending(String),
    #[error("Challenge window still open: {0}")]
    WindowStillOpen(String),
    #[error("Challenge window has closed: {0}")]
    WindowClosed(String),
    #[error("Invalid challenge: {0}")]
    InvalidChallenge(String),
    #[error("Escape rejected: {0}")]
    Rejected(String),
}

// ── Utility ───────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn make_leaf_hash(account: &str, balance: u64) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(account.as_bytes());
        h.update(balance.to_le_bytes());
        h.finalize().into()
    }

    fn make_root(leaf: [u8; 32], sibling: [u8; 32]) -> String {
        let mut h = Sha256::new();
        if leaf <= sibling {
            h.update(leaf);
            h.update(sibling);
        } else {
            h.update(sibling);
            h.update(leaf);
        }
        hex::encode(h.finalize())
    }

    #[test]
    fn valid_escape_round_trip() {
        let account = "alice_pubkey_hex";
        let balance = 500u64;
        let sibling = [0u8; 32];
        let leaf = make_leaf_hash(account, balance);
        let root = make_root(leaf, sibling);

        let mut mgr = EscapeHatchManager::new();
        mgr.record_checkpoint(CommittedCheckpoint {
            batch_id: 1,
            state_root_hex: root.clone(),
            committed_at: unix_now() - 8 * 24 * 3600, // committed 8 days ago
        });

        let req = EscapeRequest {
            id: "test-escape-001".to_string(),
            account: account.to_string(),
            claimed_balance: balance,
            anchor_batch_id: 1,
            merkle_proof_siblings: vec![hex::encode(sibling)],
            requested_at: unix_now() - 8 * 24 * 3600, // requested 8 days ago (window expired)
        };

        let id = mgr.submit_escape(req).unwrap();
        let released = mgr.finalise_escape(&id).unwrap();
        assert_eq!(released, balance);
    }

    #[test]
    fn unknown_batch_rejected() {
        let mut mgr = EscapeHatchManager::new();
        let req = EscapeRequest::new("alice", 100, 999, vec![]);
        let err = mgr.submit_escape(req).unwrap_err();
        assert!(matches!(err, EscapeError::UnknownBatch(999)));
    }

    #[test]
    fn invalid_merkle_proof_rejected() {
        let mut mgr = EscapeHatchManager::new();
        mgr.record_checkpoint(CommittedCheckpoint {
            batch_id: 1,
            state_root_hex: "deadbeef".repeat(8),
            committed_at: unix_now(),
        });
        // Wrong siblings → computed root ≠ committed root.
        let req = EscapeRequest::new("alice", 100, 1, vec!["aa".repeat(32)]);
        let err = mgr.submit_escape(req).unwrap_err();
        assert!(matches!(err, EscapeError::InvalidProof(_)));
    }

    #[test]
    fn window_still_open_blocks_finalise() {
        let account = "bob";
        let balance = 200u64;
        let sibling = [0u8; 32];
        let leaf = make_leaf_hash(account, balance);
        let root = make_root(leaf, sibling);

        let mut mgr = EscapeHatchManager::new();
        mgr.record_checkpoint(CommittedCheckpoint {
            batch_id: 2,
            state_root_hex: root,
            committed_at: unix_now(),
        });

        let req = EscapeRequest {
            id: "test-escape-002".to_string(),
            account: account.to_string(),
            claimed_balance: balance,
            anchor_batch_id: 2,
            merkle_proof_siblings: vec![hex::encode(sibling)],
            requested_at: unix_now(), // just now → window still open
        };

        let id = mgr.submit_escape(req).unwrap();
        let err = mgr.finalise_escape(&id).unwrap_err();
        assert!(matches!(err, EscapeError::WindowStillOpen(_)));
    }
}
