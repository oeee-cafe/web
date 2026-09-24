import type { ArchivedChat } from "./archiveLog";
import type { LogRow } from "./logRows";
import type { FiledReport } from "./reportsPanel";

/** A node, its class and its text in one call; the page is built of little
 * else. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

/**
 * Moves the canvas to a position: the index of the last drawing message to
 * apply, -1 for blank. `seq` is the message that was chosen, when that is
 * more exact than the position -- a pointer lifted shares its position with
 * the stroke before it. Undefined when there is nothing to move.
 */
export type Seek = ((position: number, seq?: number) => void) | undefined;

/** What the database knows about a session; see `collaborative_session_details`. */
export type SessionDetails = {
  session: {
    id: string;
    title: string | null;
    owner_login_name: string;
    width: number;
    height: number;
    max_participants: number;
    active_participant_count: number;
    total_participant_count: number;
    is_public: boolean;
    community_slug: string | null;
    community_name: string | null;
    community_visibility: string | null;
    created_at: string;
    last_activity: string;
    ended_at: string | null;
    saved_post_id: string | null;
  };
  participants: {
    user_id: string;
    login_name: string;
    display_name: string;
    joined_at: string;
    left_at: string | null;
    is_active: boolean;
  }[];
  /** Session id to login name while the room still remembers it. */
  seats: { session_id: number; login_name: string }[];
};

/** Everything the panels read, rebuilt whenever a live session grows. */
export type InspectorData = {
  rows: LogRow[];
  /** When each drawing message was sequenced, in replay order. */
  drawTimes: number[];
  /** The recording's first moment, for the time columns. */
  startAt: number | null;
  /** Session id to login name. */
  names: Map<number, string>;
  /** Why there is no log to show, when there is not. */
  logUnavailable?: string;
  chat: ArchivedChat[];
  chatUnavailable?: string;
  reports: FiledReport[];
  reportsUnavailable?: string;
  details: SessionDetails | null;
  /** The canvas position once the recording has reached `seq`. */
  positionOfSeq: (seq: number) => number;
};

/** One tab's worth of the side column. */
export type Panel = {
  root: HTMLElement;
  /** How many things it holds, for its tab. */
  count(): number;
  setData(data: InspectorData): void;
  /** The canvas now stands at `position`. */
  update?: (position: number, playing: boolean) => void;
  /** Called when its tab is shown, since a hidden panel has no size. */
  shown?: () => void;
};
