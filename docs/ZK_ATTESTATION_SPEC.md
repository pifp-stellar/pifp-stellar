# Zero-Knowledge Donor Attestation Specification

## Overview

Donors must prove their contribution tier without revealing their wallet address. This spec defines the zk-SNARK circuit, WASM prover pipeline, and Soroban on-chain verifier.

## zk-SNARK Circuit

The Circom circuit (`donor_tier.circom`) takes the following private inputs:

- `donorSecret`: 256-bit secret known only to the donor.
- `contributionAmount`: Total amount donated across all projects.
- `projectId`: Target project identifier.

Public inputs:

- `signalHash`: SHA-256 commitment to all public signals.
- `tierIndex`: Donor tier (0 = Bronze, 1 = Silver, 2 = Gold, 3 = Platinum).
- `nullifier`: Unique spend identifier to prevent double-spending.

### Tier Thresholds

| Tier | Minimum Contribution (XLM) |
|------|----------------------------|
| Bronze   | 10                        |
| Silver   | 100                       |
| Gold     | 1,000                     |
| Platinum | 10,000                    |

## WASM Compilation

The prover is compiled from Circom/snarkjs to WASM using:

```bash
snarkjs wtns calculate donor_tier.wasm input.json witness.wtns
snarkjs zkey export soliditycalldata proof.json public.json
```

The resulting `prover.wasm` is optimised for browser execution:
- Single-threaded to avoid Worker overhead.
- SIMD instructions enabled for field arithmetic.
- Pre-allocated WASM memory pool to prevent GC pauses.

## Web Worker Integration

Proof generation takes ~2–3 seconds on a modern CPU. To prevent UI blocking, the computation runs in a dedicated Web Worker (`src/workers/zkProof.worker.ts`).

### Message Protocol

**Request:**
```json
{
  "id": 1,
  "type": "generate",
  "donorSecret": "<base64>",
  "contributionAmount": "<bigint>",
  "projectId": "<bigint>",
  "tierThreshold": "<bigint>"
}
```

**Response:**
```json
{
  "id": 1,
  "type": "result",
  "proof": { "pi_a": [...], "pi_b": [...], "pi_c": [...] },
  "publicSignals": ["...", "..."],
  "durationMs": 2100
}
```

## Soroban On-Chain Verifier

The `ZkProofContract` in `contracts/pifp_protocol/src/zk_proof.rs` verifies Groth16 proofs. It exposes:

- `verify_donor_tier(proof: ZkProof, min_tier: u32) -> bool`

Gas budget: < 50,000 units.

### Verification Flow

1. Donor generates proof off-chain using `useZkProof` hook.
2. Frontend calls `verify_donor_tier` on the Soroban contract.
3. If valid, the contract updates the donor's private tier state.
4. Tier state is readable via `get_donor_tier(donor: Address) -> Option<u32>`.

## Security Considerations

- The verification key must be registered by the SuperAdmin before any proofs can be verified.
- The nullifier is stored on-chain to prevent replay attacks.
- The donor secret is never transmitted; only the nullifier is stored.
