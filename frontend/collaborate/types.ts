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
  /** The owner's guest switch, which they may flip while the session is open. */
  allowGuests?: boolean;
  /** The owner's switch for watching without drawing. */
  allowSpectators?: boolean;
  /** No community, or a public one: the only kind a guest may enter. */
  communityAdmitsGuests?: boolean;
  /** The owner gave it no title, and `title` is a stand-in. */
  untitled?: boolean;
  /** This viewer has drawn here, so is let back to draw into a full room. */
  viewerHasDrawn?: boolean;
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
  /** Watching, not drawing: no seat and no layers. */
  isSpectator?: boolean;
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
