export interface CollaborationMeta {
  title: string;
  width: number;
  height: number;
  ownerId: string;
  savedPostId?: string;
  ownerLoginName: string;
  maxUsers: number;
  currentUserCount: number;
  /** Over, saved or not: no socket to open, only the session-over dialog. */
  ended: boolean;
}

export interface Participant {
  userId: string;
  username: string;
  joinedAt: number;
  /**
   * The 1-byte id the canonical stream calls them, absent until the server
   * has assigned one. The painter keys layers by it, so it is what the layer
   * toolbox needs to attach a name to a pair.
   */
  sessionId?: number;
}
