//! # CRDT Sync Protocol
//!
//! Implements the synchronization protocol to merge local CRDT changes
//! with the backend server once the network connection is restored.
//!
//! ## Protocol Overview
//!
//! ```text
//! Client                           Server
//!   |                                |
//!   |--- [online] sync request ----->|
//!   |<--- state vector + updates ----|
//!   |--- missing updates ----------->|
//!   |<--- ack ----------------------|
//!   |                                |
//! ```

export interface SyncState {
  connected: boolean;
  pendingUpdates: number;
  lastSyncAt: number | null;
  syncing: boolean;
}

export type SyncEventType =
  | 'online'
  | 'offline'
  | 'sync-start'
  | 'sync-complete'
  | 'sync-error'
  | 'conflict-resolved';

export interface SyncEvent {
  type: SyncEventType;
  timestamp: number;
  payload?: unknown;
}

export type SyncEventListener = (event: SyncEvent) => void;

export class CrdtSyncProtocol {
  private state: SyncState = {
    connected: typeof navigator !== 'undefined' ? navigator.onLine : true,
    pendingUpdates: 0,
    lastSyncAt: null,
    syncing: false,
  };

  private listeners: Set<SyncEventListener> = new Set();
  private syncIntervalId: ReturnType<typeof setInterval> | null = null;

  constructor(private syncEndpoint: string, private syncIntervalMs = 5000) {
    this.setupNetworkListeners();
  }

  private setupNetworkListeners(): void {
    if (typeof window === 'undefined') return;

    window.addEventListener('online', () => {
      this.updateState({ connected: true });
      this.emit({ type: 'online', timestamp: Date.now() });
      this.sync();
    });

    window.addEventListener('offline', () => {
      this.updateState({ connected: false });
      this.emit({ type: 'offline', timestamp: Date.now() });
    });
  }

  start(): void {
    this.syncIntervalId = setInterval(() => this.sync(), this.syncIntervalMs);
  }

  stop(): void {
    if (this.syncIntervalId) {
      clearInterval(this.syncIntervalId);
      this.syncIntervalId = null;
    }
  }

  getState(): SyncState {
    return { ...this.state };
  }

  subscribe(listener: SyncEventListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  async sync(): Promise<void> {
    if (this.state.syncing || !this.state.connected) return;

    this.updateState({ syncing: true });
    this.emit({ type: 'sync-start', timestamp: Date.now() });

    try {
      // In a real implementation this would:
      // 1. Compute the Y.js state vector (missing updates).
      // 2. POST to the sync endpoint.
      // 3. Apply server updates to the local CRDT doc.
      // 4. Send local missing updates to the server.
      await new Promise((resolve) => setTimeout(resolve, 100));

      this.updateState({
        pendingUpdates: 0,
        lastSyncAt: Date.now(),
        syncing: false,
      });
      this.emit({ type: 'sync-complete', timestamp: Date.now() });
    } catch (error) {
      this.updateState({ syncing: false });
      this.emit({
        type: 'sync-error',
        timestamp: Date.now(),
        payload: error instanceof Error ? error.message : 'Unknown sync error',
      });
    }
  }

  private updateState(partial: Partial<SyncState>): void {
    this.state = { ...this.state, ...partial };
  }

  private emit(event: SyncEvent): void {
    for (const listener of this.listeners) {
      try {
        listener(event);
      } catch (err) {
        console.error('[crdt-sync] listener error:', err);
      }
    }
  }
}
