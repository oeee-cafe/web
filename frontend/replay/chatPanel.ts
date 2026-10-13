/**
 * What was said, read against what was being drawn.
 */

import { positionAt, type ArchivedChat } from "./archiveLog";
import { el, personName, type InspectorData, type Panel, type Seek } from "./dom";
import { elapsed } from "./logRows";

export function chatPanel(seek: Seek): Panel {
  const root = el("div", "inspect-panel inspect-chat");
  let chat: ArchivedChat[] = [];
  let rows: HTMLElement[] = [];
  /** Where the canvas stood when each line was said. Recomputed as a live
   * recording grows, since a line said a moment ago may now have marks
   * before it that were not there. */
  let linePositions: number[] = [];
  let position = -1;
  let lastShown: HTMLElement | null = null;

  const show = (reached: number) => {
    position = reached;
    // Everything said up to here, so the conversation arrives as the drawing
    // does rather than all at once at the top. Dimmed rather than hidden, so
    // its shape is visible while scrubbing.
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
  };

  return {
    root,
    count: () => chat.length,
    setData(data: InspectorData) {
      linePositions = data.chat.map((line) => positionAt(data.drawTimes, line.at));
      if (data.chatUnavailable) {
        chat = [];
        rows = [];
        root.textContent = "";
        root.appendChild(el("p", "ds-help inspect-empty", data.chatUnavailable));
        return;
      }
      // Rebuilt only when there is something new to say: a live session asks
      // every few seconds, and a rebuild would lose the reader's scroll.
      if (data.chat.length !== chat.length || rows.length === 0) {
        chat = data.chat;
        root.textContent = "";
        lastShown = null;
        if (chat.length === 0) root.appendChild(el("p", "ds-help inspect-empty", "Nothing was said."));
        rows = chat.map((line, index) => {
          // A row, not a <button>: the speaker's name inside it is a link,
          // and a link inside a button is neither valid nor clickable in
          // every engine.
          const row = el("div", "inspect-chat-line");
          const when = el(
            "span",
            "inspect-time",
            data.startAt !== null
              ? elapsed(line.at - data.startAt)
              : new Date(line.at).toISOString().slice(11, 19),
          );
          row.append(
            when,
            personName(line.login_name, line.is_guest, "inspect-chat-who"),
            ...(line.is_spectator ? [el("span", "admin-tag", "watching")] : []),
            el("span", "inspect-chat-said", line.message),
          );
          row.title = new Date(line.at).toISOString();
          if (seek) {
            row.setAttribute("role", "button");
            row.tabIndex = 0;
            // Following the name is going to their profile, not moving the canvas.
            row.addEventListener("click", (event) => {
              if ((event.target as Element).closest("a")) return;
              seek(linePositions[index]);
            });
            row.addEventListener("keydown", (event) => {
              if (event.target !== row || (event.key !== "Enter" && event.key !== " ")) return;
              event.preventDefault();
              seek(linePositions[index]);
            });
          }
          root.appendChild(row);
          return row;
        });
      }
      show(position);
    },
    update: (next) => show(next),
  };
}
