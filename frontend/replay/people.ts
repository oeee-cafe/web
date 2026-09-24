/**
 * Who did what in a session, counted from its log.
 *
 * The questions this answers are the ones a report of trouble starts with --
 * whose marks vanished, who drew on whose layer, who was even there when it
 * happened -- and each is a count over the rows the log already has.
 */

import type { LogRow } from "./logRows";

export type PersonStats = {
  sessionId: number;
  /** Everything that put something on a canvas, undo boundaries aside. */
  marks: number;
  strokes: number;
  undos: number;
  redos: number;
  /** Marks made on somebody else's layers. */
  onOthers: number;
  /** Marks others made on this person's layers. */
  byOthers: number;
  firstAt: number | null;
  lastAt: number | null;
  /** Marks per slice of the recording, for a strip of how busy they were. */
  activity: number[];
};

function blank(sessionId: number, buckets: number): PersonStats {
  return {
    sessionId,
    marks: 0,
    strokes: 0,
    undos: 0,
    redos: 0,
    onOthers: 0,
    byOthers: 0,
    firstAt: null,
    lastAt: null,
    activity: new Array<number>(buckets).fill(0),
  };
}

/** Drawable, and not marks: the boundary every gesture opens with would
 * double every count, and undos and redos are counted on their own. */
const NOT_A_MARK = ["undoPoint", "undo", "redo"];

/**
 * Stats for everyone who sent anything, by session id, in the order they
 * first appear.
 */
export function participantStats(rows: LogRow[], buckets = 48): PersonStats[] {
  const people = new Map<number, PersonStats>();
  const person = (id: number) => {
    let held = people.get(id);
    if (!held) {
      held = blank(id, buckets);
      people.set(id, held);
    }
    return held;
  };
  const start = rows.length > 0 ? rows[0].at : 0;
  const span = rows.length > 0 ? Math.max(1, rows[rows.length - 1].at - start) : 1;

  for (const row of rows) {
    if (row.sessionId === null) continue;
    const stats = person(row.sessionId);
    if (stats.firstAt === null) stats.firstAt = row.at;
    stats.lastAt = row.at;
    if (row.kind === "undo") stats.undos++;
    else if (row.kind === "redo") stats.redos++;
    if (!row.drawable || NOT_A_MARK.indexOf(row.kind) >= 0) continue;
    stats.marks++;
    if (row.kind === "stroke") stats.strokes++;
    const bucket = Math.min(buckets - 1, Math.floor(((row.at - start) / span) * buckets));
    stats.activity[bucket]++;
    if (row.onOther) {
      stats.onOthers++;
      if (row.target !== null) person(row.target).byOthers++;
    }
  }
  return Array.from(people.values());
}
