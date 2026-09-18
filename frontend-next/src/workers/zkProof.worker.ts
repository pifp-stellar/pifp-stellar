//! # ZK Proof Web Worker
//!
//! Offloads heavy Circom/snarkjs proof generation to a dedicated Web Worker
//! to prevent UI blocking.  The worker communicates with the main thread
//! via a simple message protocol.

export interface WorkerRequest {
  id: number;
  type: 'generate';
  donorSecret: Uint8Array;
  contributionAmount: bigint;
  projectId: bigint;
  tierThreshold: bigint;
}

export interface WorkerResponse {
  id: number;
  type: 'result';
  proof: {
    pi_a: [string, string];
    pi_b: [string, string];
    pi_c: [string, string];
  };
  publicSignals: string[];
  durationMs: number;
}

export interface WorkerErrorResponse {
  id: number;
  type: 'error';
  message: string;
}

// Mock proof generator (in production: import snarkjs WASM).
function mockGenerateProof(req: WorkerRequest): WorkerResponse {
  const start = performance.now();

  const proof = {
    pi_a: ['0x1234', '0x5678'],
    pi_b: ['0x9abc', '0xdef0'],
    pi_c: ['0x1111', '2222'],
  };

  const publicSignals = [
    `0x${Buffer.from(req.donorSecret).toString('hex').slice(0, 64).padEnd(64, '0')}`,
    `0x${req.contributionAmount.toString(16).padStart(64, '0')}`,
    `0x${req.projectId.toString(16).padStart(64, '0')}`,
    `0x${req.tierThreshold.toString(16).padStart(64, '0')}`,
  ];

  const durationMs = performance.now() - start;

  return {
    id: req.id,
    type: 'result',
    proof,
    publicSignals,
    durationMs,
  };
}

// Listen for messages from the main thread.
self.onmessage = (event: MessageEvent<WorkerRequest>) => {
  const req = event.data;

  try {
    const result = mockGenerateProof(req);
    self.postMessage(result);
  } catch (err) {
    const errorResponse: WorkerErrorResponse = {
      id: req.id,
      type: 'error',
      message: err instanceof Error ? err.message : 'Unknown error',
    };
    self.postMessage(errorResponse);
  }
};
