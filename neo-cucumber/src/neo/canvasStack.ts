/**
 * NEO paints both artwork layers first, then draws tool previews and the
 * brush cursor over the composite. Keep the DOM canvas stack in that order.
 *
 * A collaborative session has one such pair per participant, stacked as
 * contiguous units: a participant is entirely above or entirely below another,
 * so their background covers the person below's lines the same way their own
 * background covers their own. Earlier joiners sit on top, which is how the
 * pre-shared-canvas painter ordered them and puts the session owner above
 * everyone.
 */
export const CANVAS_Z_INDEX = {
  background: 1,
  foreground: 2,
  /**
   * The other participants' pointer tags, which a host places in the same
   * container. Above every layer -- a tag under somebody's drawing is a tag
   * nobody can find -- and below this painter's own previews and cursor, so
   * nobody else's pointer covers what the person here is aiming.
   */
  collaborators: 9900,
  preview: 10000,
  cursor: 10001,
  textEditor: 10002,
} as const;

/** How far apart two participants' bands sit; two layers plus headroom. */
const PARTICIPANT_STRIDE = 10;

/** The top of the participant band, below every overlay. */
const PARTICIPANT_BASE = 9000;

/**
 * Where a participant's two canvases belong in the stack.
 *
 * `rank` is their position in join order, earliest first. Earlier joiners get
 * higher z-indices so they composite on top. The band is bounded below by 0
 * and above by `preview`, so no number of participants can reach the overlays.
 */
export function participantZIndex(
  rank: number,
  layer: "background" | "foreground"
): number {
  const base = Math.max(
    CANVAS_Z_INDEX.background,
    PARTICIPANT_BASE - rank * PARTICIPANT_STRIDE
  );
  return base + (layer === "foreground" ? 1 : 0);
}

/**
 * Participants as their pairs stack on screen, bottom first: the latest
 * joiner, whose pair everybody else's covers. For compositing, where the
 * order of drawing is the order of the stack.
 */
export function bottomFirst(owners: string[]): string[] {
  return inJoinOrder(owners).reverse();
}

/**
 * Participants in join order, earliest first.
 *
 * For a collaborative session that is ascending session id: the ids are handed
 * out by a counter as people connect and are never reused, so this is the same
 * order on every client, including for anyone who has since left. The offline
 * painter has a single non-numeric owner and sorts trivially.
 */
export function inJoinOrder(owners: string[]): string[] {
  return [...owners].sort((a, b) => {
    const left = Number(a);
    const right = Number(b);
    if (Number.isNaN(left) || Number.isNaN(right)) {
      return a < b ? -1 : a > b ? 1 : 0;
    }
    return left - right;
  });
}
