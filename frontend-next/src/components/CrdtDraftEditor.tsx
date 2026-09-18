//! # CRDT Draft Editor
//!
//! A React component that provides offline-first project draft editing
//! using Yjs CRDT with IndexedDB persistence and background sync.

'use client';

import { useEffect, useRef, useState } from 'react';
import * as Y from 'yjs';
import { IndexeddbPersistence } from 'y-indexeddb';
import { createCrdtDoc, createLocalCrdtDoc } from '@/lib/crdt';
import { createIdbPersistence, waitForIdbReady } from '@/lib/crdt/idb-persistence';
import { CrdtSyncProtocol } from '@/lib/crdt/sync';

export interface CrdtDraftEditorProps {
  projectId: string;
  initialContent?: string;
  roomName?: string;
  readOnly?: boolean;
  onSave?: (content: string) => void;
}

export function CrdtDraftEditor({
  projectId,
  initialContent = '',
  roomName,
  readOnly = false,
  onSave,
}: CrdtDraftEditorProps) {
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const [content, setContent] = useState(initialContent);
  const [connectionStatus, setConnectionStatus] = useState<'local' | 'connected' | 'syncing'>('local');
  const [pendingUpdates, setPendingUpdates] = useState(0);

  const ydocRef = useRef<Y.Doc | null>(null);
  const ytextRef = useRef<Y.Text | null>(null);
  const persistenceRef = useRef<IndexeddbPersistence | null>(null);
  const syncProtocolRef = useRef<CrdtSyncProtocol | null>(null);

  useEffect(() => {
    const room = roomName || `pifp-draft-${projectId}`;
    const doc = room ? createCrdtDoc({ roomName: room }).doc : createLocalCrdtDoc();
    const ytext = doc.getText('content');

    ydocRef.current = doc;
    ytextRef.current = ytext;

    // Set initial content if empty.
    if (ytext.length === 0 && initialContent) {
      ytext.insert(0, initialContent);
    }

    // Persist to IndexedDB.
    const persistence = createIdbPersistence({
      dbName: 'pifp-crdt',
      storeName: `draft-${projectId}`,
      doc,
    });
    persistenceRef.current = persistence;

    waitForIdbReady(persistence).then(() => {
      console.debug('[crdt] IndexedDB sync complete');
    });

    // Observe remote changes.
    ytext.observe((event) => {
      if (event.transaction.local) return;
      const newContent = ytext.toString();
      setContent(newContent);
      setConnectionStatus('connected');
    });

    // Set up sync protocol.
    const sync = new CrdtSyncProtocol('/api/crdt/sync');
    sync.subscribe((evt) => {
      switch (evt.type) {
        case 'online':
          setConnectionStatus('connected');
          break;
        case 'offline':
          setConnectionStatus('local');
          break;
        case 'sync-start':
          setConnectionStatus('syncing');
          break;
        case 'sync-complete':
          setConnectionStatus('connected');
          setPendingUpdates(0);
          break;
        case 'sync-error':
          console.error('[crdt] sync error:', evt.payload);
          break;
      }
    });
    sync.start();
    syncProtocolRef.current = sync;

    // Cleanup.
    return () => {
      sync.stop();
      persistence.destroy();
      doc.destroy();
    };
  }, [projectId, roomName, initialContent]);

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const newContent = e.target.value;
    setContent(newContent);

    if (ytextRef.current) {
      // Replace entire content (simple approach; for production use Y.js transactions).
      ytextRef.current.delete(0, ytextRef.current.length);
      ytextRef.current.insert(0, newContent);
    }

    onSave?.(newContent);
  };

  const statusColor =
    connectionStatus === 'connected'
      ? 'var(--color-ok)'
      : connectionStatus === 'syncing'
        ? 'var(--color-warn)'
        : 'var(--color-muted)';

  return (
    <div className="crdt-editor" data-crdt-status={connectionStatus}>
      <div className="crdt-header">
        <span className="crdt-status" style={{ color: statusColor }}>
          {connectionStatus === 'connected' && '● Synced'}
          {connectionStatus === 'syncing' && '⟳ Syncing...'}
          {connectionStatus === 'local' && '○ Offline'}
        </span>
        {pendingUpdates > 0 && (
          <span className="crdt-pending">{pendingUpdates} pending updates</span>
        )}
      </div>
      <textarea
        ref={editorRef}
        value={content}
        onChange={handleChange}
        readOnly={readOnly}
        className="crdt-textarea"
        placeholder="Start typing your project draft..."
      />
    </div>
  );
}
