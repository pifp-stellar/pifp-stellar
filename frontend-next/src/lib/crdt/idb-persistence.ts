//! # IndexedDB Persistence for CRDT
//!
//! Persists Yjs CRDT state to IndexedDB so that offline edits survive
//! browser restarts and tab closures.

import * as Y from 'yjs';
import { IndexeddbPersistence } from 'y-indexeddb';

export interface IdbPersistenceOptions {
  dbName?: string;
  storeName?: string;
  doc: Y.Doc;
}

export function createIdbPersistence(options: IdbPersistenceOptions): IndexeddbPersistence {
  const { dbName = 'pifp-crdt', storeName = 'pifp-drafts', doc } = options;

  const persistence = new IndexeddbPersistence(`${dbName}-${storeName}`, doc, {
    name: storeName,
  });

  return persistence;
}

export async function waitForIdbReady(persistence: IndexeddbPersistence): Promise<void> {
  if (persistence.synced) {
    return;
  }

  return new Promise((resolve) => {
    persistence.on('synced', () => resolve());
  });
}
