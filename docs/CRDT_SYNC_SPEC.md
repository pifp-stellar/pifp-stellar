# CRDT Offline-First Synchronisation Specification

## Overview

Creators in low-connectivity areas lose project draft data when the network drops. This spec defines a CRDT-based offline-first editing experience with eventual consistency.

## Technology Choices

- **CRDT Library:** Yjs (MIT-licensed, battle-tested, ~10KB gzipped).
- **Persistence:** `y-indexeddb` — persists Y.js updates to IndexedDB automatically.
- **Sync Transport:** WebRTC (`y-webrtc`) for peer-to-peer sync and WebSocket fallback.

## Integration Architecture

```text
┌──────────────────────────────────────────┐
│  Next.js App Router                      │
│  ┌────────────────────────────────────┐  │
│  │  CrdtDraftEditor (React)           │  │
│  │  ├── Y.Text (project-draft)        │  │
│  │  ├── IndexeddbPersistence          │  │
│  │  └── CrdtSyncProtocol              │  │
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
```

## IndexedDB Persistence

The `y-indexeddb` provider stores every Y.js update as it arrives. On page reload:

1. The `CrdtDraftEditor` creates a new `Y.Doc`.
2. `IndexeddbPersistence` loads the persisted state.
3. The `synced` event fires once the local state is fully restored.
4. The `sync` event fires when the server state is merged.

## Conflict Resolution

Yjs CRDTs resolve conflicts automatically by design:

- **Insertions:** Merged in causal order.
- **Deletions:** Tombstones ensure deleted text stays deleted.
- **Concurrent edits at the same position:** Yjs uses a deterministic tie-breaker based on client IDs.

No manual merge UI is required.

## Sync Protocol

### Online Sync

When the browser detects `navigator.onLine === true`, the `CrdtSyncProtocol` initiates a sync:

1. Compute the Y.js state vector (set of missing updates).
2. POST `{ projectId, stateVector, updates }` to `/api/crdt/sync`.
3. Server returns `{ missingStateVector, missingUpdates }`.
4. Client applies server updates to its local `Y.Doc`.
5. Client sends any local updates the server is missing.

### Offline Queue

While offline, edits are stored in the local `Y.Doc` and persisted to IndexedDB. The sync protocol queues updates and flushes them once connectivity is restored.

## Usage

```tsx
import { CrdtDraftEditor } from '@/components/CrdtDraftEditor';

export default function DraftPage({ params }) {
  return (
    <CrdtDraftEditor
      projectId={params.id}
      roomName={`pifp-draft-${params.id}`}
      onSave={(content) => console.log('saved', content)}
    />
  );
}
```

## Performance

- Initial load: < 100ms (IndexedDB read is fast).
- Edit latency: < 16ms (Y.js operations are O(1)).
- Memory footprint: ~1KB per open document.
