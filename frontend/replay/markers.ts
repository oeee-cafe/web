/**
 * The moments worth jumping to, placed along the scrubber.
 *
 * A session is thousands of messages and the interesting ones are a handful:
 * somebody said something, somebody reported trouble, the room took a
 * checkpoint, or nobody did anything for a while. Each is a position the
 * canvas can be sent to.
 */

import { positionAt, type ArchivedChat } from "./archiveLog";
import type { LogRow } from "./logRows";

export type MarkerKind = "chat" | "report" | "checkpoint" | "quiet";

export type Marker = {
  kind: MarkerKind;
  /** Canvas position, as the scrubber counts: -1 is blank. */
  position: number;
  label: string;
};

/** A pause at least this long is marked. Shorter ones are somebody thinking. */
export const QUIET_MS = 60_000;

function duration(ms: number): string {
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.floor((ms % 60_000) / 1000);
  return minutes > 0 ? `${minutes}m ${seconds}s` : `${seconds}s`;
}

export function markers(options: {
  rows: LogRow[];
  drawTimes: number[];
  chat: ArchivedChat[];
  /** When each report was filed, by the server's clock where known. */
  reports: { at: number; label: string }[];
}): Marker[] {
  const { rows, drawTimes } = options;
  const found: Marker[] = [];

  for (const line of options.chat) {
    found.push({
      kind: "chat",
      position: positionAt(drawTimes, line.at),
      label: `${line.login_name}: ${line.message}`,
    });
  }
  for (const report of options.reports) {
    found.push({ kind: "report", position: positionAt(drawTimes, report.at), label: report.label });
  }
  for (const row of rows) {
    if (row.kind === "resetPoint") {
      found.push({ kind: "checkpoint", position: row.position, label: `checkpoint, seq ${row.seq}` });
    }
  }
  // Between marks rather than between messages: a pointer lifted a minute
  // after the stroke it ended is not a quiet minute on the canvas.
  for (let index = 1; index < drawTimes.length; index++) {
    const gap = drawTimes[index] - drawTimes[index - 1];
    if (gap >= QUIET_MS) {
      found.push({ kind: "quiet", position: index - 1, label: `quiet for ${duration(gap)}` });
    }
  }
  return found;
}
