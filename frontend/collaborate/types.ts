export interface CollaborationMeta {
  title: string;
  width: number;
  height: number;
  ownerId: string;
  savedPostId?: string;
  /** The saved post's page; under its community's slug when it has one. */
  savedPostUrl?: string;
  ownerLoginName: string;
  /** A guest opened it: it is kept on their device rather than posted. */
  ownerIsGuest?: boolean;
  /** Whether somebody without an account may come in as a guest. */
  guestsWelcome?: boolean;
  maxUsers: number;
  currentUserCount: number;
  /** Over, saved or not: no socket to open, only the session-over dialog. */
  ended: boolean;
}

export interface Participant {
  userId: string;
  username: string;
  /** No account: a name they chose, shown with a guest badge. */
  isGuest?: boolean;
  /**
   * Gone from the room since the last roster: left, or sent out. Still named
   * on their layers, which stay; no longer listed as here.
   */
  left?: boolean;
  joinedAt: number;
  /**
   * The 1-byte id the canonical stream calls them, absent until the server
   * has assigned one. The painter keys layers by it, so it is what the layer
   * toolbox needs to attach a name to a pair.
   */
  sessionId?: number;
}
