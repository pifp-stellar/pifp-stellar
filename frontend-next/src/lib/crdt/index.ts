//! # CRDT Layer
//!
//! Provides Conflict-free Replicated Data Type (CRDT) support using Yjs
//! for offline-first project draft editing with eventual consistency.

import * as Y from 'yjs';
import { WebrtcProvider } from 'y-webrtc';

export interface CrdtConfig {
  roomName: string;
  signalingUrls?: string[];
  awareness?: {
    localClientId?: number;
    name?: string;
    color?: string;
  };
}

export function createCrdtDoc(config: CrdtConfig): {
  doc: Y.Doc;
  provider: WebrtcProvider;
  getText: () => Y.Text;
} {
  const doc = new Y.Doc();
  const provider = new WebrtcProvider(config.roomName, doc, {
    signaling: config.signalingUrls,
  });

  if (config.awareness) {
    provider.awareness.setLocalStateField('user', config.awareness);
  }

  return {
    doc,
    provider,
    getText: () => doc.getText('project-draft'),
  };
}

export function createLocalCrdtDoc(): Y.Doc {
  return new Y.Doc();
}
