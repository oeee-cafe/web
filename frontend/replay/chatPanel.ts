/**
 * What was said, read against what was being drawn.
 */

import { positionAt, type ArchivedChat } from "./archiveLog";
import { el, type Panel, type Seek } from "./dom";
import { elapsed } from "./logRows";

export function chatPanel(options: {
  chat: ArchivedChat[];
  /** Why there is no transcript, when there could not be one. */
  unavailable?: string;
  /** When each drawing message was sequenced, in replay order. */
  drawTimes: number[];
  /** The recording's first moment, for the time column. */
  startAt: number | null;
  seek: Seek;
}): Panel {
  const { chat, drawTimes, startAt, seek } = options;
  const root = el("div", "inspect-panel inspect-chat");

  if (options.unavailable) {
    root.appendChild(el("p", "inspect-empty", options.unavailable));
    return { root, count: 0 };
  }
  if (chat.length === 0) {
    root.appendChild(el("p", "inspect-empty", "Nothing was said."));
    return { root, count: 0 };
  }

  // Where the canvas stood when each line was said, worked out once.
  const linePositions = chat.map((line) => positionAt(drawTimes, line.at));
  const rows = chat.map((line, index) => {
    const row = el(seek ? "button" : "div", "inspect-chat-line");
    const when = el(
      "span",
      "inspect-time",
      startAt !== null ? elapsed(line.at - startAt) : new Date(line.at).toISOString().slice(11, 19),
    );
    const who = el("span", "inspect-chat-who", line.login_name);
    const said = el("span", "inspect-chat-said", line.message);
    row.append(when, who, said);
    row.title = new Date(line.at).toISOString();
    if (seek) {
      (row as HTMLButtonElement).type = "button";
      row.addEventListener("click", () => seek(linePositions[index]));
    }
    root.appendChild(row);
    return row;
  });

  let lastShown: HTMLElement | null = null;
  return {
    root,
    count: chat.length,
    update(position) {
      // Everything said up to here, so the conversation arrives as the
      // drawing does rather than all at once at the top. Dimmed rather than
      // hidden, so its shape is visible while scrubbing.
      let last: HTMLElement | null = null;
      rows.forEach((row, index) => {
        const said = linePositions[index] <= position;
        row.classList.toggle("inspect-future", !said);
        if (said) last = row;
      });
      if (last && last !== lastShown && root.offsetParent) {
        (last as HTMLElement).scrollIntoView({ block: "nearest" });
      }
      lastShown = last;
    },
  };
}
