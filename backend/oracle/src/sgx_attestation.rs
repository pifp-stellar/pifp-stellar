//! # SGX DCAP Attestation
//!
//! Implements Intel Data Center Attestation Primitives (DCAP) for remote
//! attestation.  The oracle uses this to prove to the Soroban contract
//! that the aggregation code ran inside a genuine Intel SGX enclave.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::sgx_enclave::{AttestationQuote, EnclaveHandle, SgxError, SgxResult};

// ─── Quote Verification Report ───────────────────────────────────────────────

/// Verification report returned by the PCCS after validating a quote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub id: String,
    pub timestamp: u64,
    pub epid_anonymous: Option<Vec<u8>>,
    pub advisory_ids: Vec<String>,
    pub adeus: Vec<u8>,
}

impl Default for VerificationReport {
    fn default() -> Self {
        Self {
            id: String::new(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            epid_anonymous: None,
            advisory_ids: Vec::new(),
            adeus: vec![0u8; 32],
        }
    }
}

// ─── Attestation Manager ─────────────────────────────────────────────────────

/// Manages the lifecycle of SGX DCAP attestation for the oracle node.
pub struct AttestationManager {
    handle: EnclaveHandle,
    cached_quote: Option<AttestationQuote>,
    cached_report: Option<VerificationReport>,
    quote_ttl_secs: u64,
}

impl AttestationManager {
    /// Create a new attestation manager for the given enclave.
    pub fn new(handle: EnclaveHandle, quote_ttl_secs: u64) -> Self {
        Self {
            handle,
            cached_quote: None,
            cached_report: None,
            quote_ttl_secs,
        }
    }

    /// Return a fresh DCAP quote, refreshing the cache if the TTL has
    /// elapsed.
    pub fn get_quote(&mut self) -> SgxResult<AttestationQuote> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let needs_refresh = match &self.cached_report {
            Some(report) => now.saturating_sub(report.timestamp) > self.quote_ttl_secs,
            None => true,
        };

        if needs_refresh {
            let quote = AttestationQuote::generate(&self.handle)?;
            self.cached_quote = Some(quote.clone());
            self.cached_report = Some(VerificationReport::default());
        }

        Ok(self.cached_quote.clone().unwrap())
    }

    /// Verify an attestation quote from a peer oracle node.
    pub fn verify_peer_quote(
        &self,
        quote: &AttestationQuote,
        expected_mr_enclave: &[u8; 32],
    ) -> SgxResult<VerificationReport> {
        quote.verify(expected_mr_enclave)?;
        Ok(VerificationReport::default())
    }

    /// Return the current MRENCLAVE of the managed enclave.
    pub fn mr_enclave(&self) -> &[u8; 32] {
        self.handle.mr_enclave()
    }

    /// Return whether the enclave is running in production mode.
    pub fn is_production(&self) -> bool {
        self.handle.is_production()
    }
}
