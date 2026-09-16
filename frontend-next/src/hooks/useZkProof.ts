//! # useZkProof Hook
//!
//! React hook that manages the lifecycle of a zero-knowledge proof generation
//! task.  Offloads the heavy computation to a Web Worker and returns a
//! typed result state.

import { useCallback, useEffect, useRef, useState } from 'react';

export interface ZkProofState<T> {
  data: T | null;
  isLoading: boolean;
  error: string | null;
  durationMs: number | null;
}

export interface UseZkProofOptions {
  onSuccess?: (proof: ZkProofState<unknown>['data']) => void;
  onError?: (error: string) => void;
}

export function useZkProof<T = unknown>(
  workerPath: string,
  options?: UseZkProofOptions,
): ZkProofState<T> & { generate: (inputs: unknown) => void } {
  const [state, setState] = useState<ZkProofState<T>>({
    data: null,
    isLoading: false,
    error: null,
    durationMs: null,
  });

  const workerRef = useRef<Worker | null>(null);
  const requestIdRef = useRef(0);
  const pendingRef = useRef(new Map<number, {
    resolve: (value: unknown) => void;
    reject: (reason: string) => void;
  }>());

  useEffect(() => {
    let worker: Worker;
    try {
      worker = new Worker(workerPath, { type: 'module' });
    } catch (e) {
      console.error('[zk] failed to create worker:', e);
      setState(s => ({ ...s, error: 'Failed to create ZK proof worker' }));
      return;
    }

    worker.onmessage = (event: MessageEvent) => {
      const msg = event.data;
      const pending = pendingRef.current.get(msg.id);
      if (!pending) return;

      if (msg.type === 'result') {
        pending.resolve(msg);
        setState(s => ({
          data: msg as T,
          isLoading: false,
          error: null,
          durationMs: msg.durationMs,
        }));
        options?.onSuccess?.(msg as T);
      } else if (msg.type === 'error') {
        pending.reject(msg.message);
        setState(s => ({
          ...s,
          isLoading: false,
          error: msg.message,
        }));
        options?.onError?.(msg.message);
      }

      pendingRef.current.delete(msg.id);
    };

    worker.onerror = (err) => {
      console.error('[zk] worker error:', err);
      setState(s => ({ ...s, isLoading: false, error: 'ZK worker crashed' }));
    };

    workerRef.current = worker;

    return () => {
      worker.terminate();
      workerRef.current = null;
    };
  }, [workerPath, options]);

  const generate = useCallback((inputs: unknown) => {
    const worker = workerRef.current;
    if (!worker) {
      setState(s => ({ ...s, error: 'ZK worker not initialised' }));
      return;
    }

    const id = ++requestIdRef.current;
    setState(s => ({ ...s, isLoading: true, error: null, durationMs: null }));

    const promise = new Promise<unknown>((resolve, reject) => {
      pendingRef.current.set(id, { resolve, reject });
    });

    // Timeout after 30 seconds.
    const timeout = setTimeout(() => {
      pendingRef.current.delete(id);
      setState(s => ({ ...s, isLoading: false, error: 'ZK proof generation timed out' }));
    }, 30_000);

    promise.then(
      (result) => {
        clearTimeout(timeout);
        return result;
      },
      (err) => {
        clearTimeout(timeout);
        throw err;
      },
    );
  }, []);

  return { ...state, generate };
}
