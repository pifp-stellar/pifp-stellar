# Merkle Mountain Range (MMR) Relayer Specification
## Cross-Chain Impact Proofs for PIFP

## Overview

External chains (e.g., Ethereum) need to cheaply verify PIFP impact funding events without trusting the oracle. This spec defines how off-chain relayers extract MMR roots from Soroban and submit them to EVM chains.

## MMR Contract API

The `MmrContract` in `contracts/pifp_protocol/src/mmr.rs` exposes:

- `append(leaves: Vec<BytesN<32>>) -> MmrSnapshot`
- `get_proof(leaf_index: u64) -> MmrProof`
- `verify_proof(proof: MmrProof) -> bool`
- `get_root() -> BytesN<32>`
- `get_leaf_count() -> u64`
- `get_snapshot(leaf_index: u64) -> Option<MmrSnapshot>`

## Relayer Responsibilities

### 1. Poll for New Snapshots

Relayers poll the Soroban RPC for `get_snapshot` with increasing `leaf_index` values. When a new snapshot is found, the relayer records the `(root, leaf_count, ledger_timestamp)` tuple.

### 2. Generate Inclusion Proofs

For each new funding event, the relayer calls `get_proof(leaf_index)` to obtain an `MmrProof`. The proof contains:

- `root`: Expected MMR root hash.
- `leaf_hash`: Hash of the leaf data.
- `leaf_index`: Zero-based position in the MMR.
- `path_hashes`: Sibling hashes from leaf to root.
- `peak_hashes`: Current peak bag for debugging.

### 3. Submit to Ethereum

The relayer submits the following calldata to the Ethereum `PIFPRelayer` contract:

```solidity
struct MmrRoot {
    bytes32 root;
    uint64 leafCount;
    uint256 sorobanLedger;
    bytes32 sorobanTxHash;
}
```

The relayer bundles multiple `MmrRoot` updates in a single transaction to amortise gas costs.

### 4. Light Client Verification

Ethereum light clients verify MMR proofs by:

1. Fetching the trusted `root` from the `PIFPRelayer` contract.
2. Recomputing the root from `leaf_hash` + `path_hashes`.
3. Checking `recomputed_root == trusted_root`.

## Gas Optimisation

- Root updates are submitted in batches (max 64 roots per tx).
- Peak hashes are not stored on-chain; only the root hash is needed.
- The Soroban contract pre-computes peaks during `append` to keep `get_proof` under 50,000 gas.

## Security Considerations

- The relayer must stake at least 100,000 `pifp-token` to prevent spam.
- A challenge period of 4,600 Ethereum blocks (~18 hours) allows anyone to dispute a fraudulent root update.
- If a challenge succeeds, the relayer's stake is slashed and the honest challenger is rewarded.
