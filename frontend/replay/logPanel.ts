/**
 * Every message in the recording, one row each, following the replay.
 *
 * Windowed: a long afternoon is tens of thousands of messages, and a row per
 * message in the DOM would make scrolling the page the slowest thing on it.
 * Rows are one fixed height so where each one sits is arithmetic.
 */

import { el, type InspectorData, type Panel, type Seek } from "./dom";
import { elapsed, type LogRow } from "./logRows";

const ROW_HEIGHT = 20;
/** Rows kept above and below the visible ones, so a quick scroll does not
 * show a blank band before the next frame fills it. */
const OVERSCAN = 12;

const ALL = "";
const DRAWING = "drawing";

/** The last of `rows` that has happened once the canvas stands at
 * `position`, -1 for none. Rows are in recording order, so their positions
 * only rise. */
function rowAt(rows: LogRow[], position: number): number {
  let low = 0;
  let high = rows.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if (rows[middle].position <= position) low = middle + 1;
    else high = middle;
  }
  return low - 1;
}

function option(value: string, label: string): HTMLOptionElement {
  const node = el("option", undefined, label);
  node.value = value;
  return node;
}

/** Refills a select with counted options, keeping what was chosen. */
function refill(select: HTMLSelectElement, options: HTMLOptionElement[]) {
  const chosen = select.value;
  select.textContent = "";
  for (const node of options) select.appendChild(node);
  select.value = chosen;
  if (select.value !== chosen) select.value = ALL;
}

export type LogPanel = Panel & {
  /** Marks the row with this sequence as the one meant, for a link that
   * named it. */
  choose(seq: number): void;
};

export function logPanel(seek: Seek): LogPanel {
  const root = el("div", "inspect-panel inspect-log-panel");
  const message = el("p", "ds-help inspect-empty inspect-log-message");

  // Filters, by what a message was and by who sent it.
  const toolbar = el("div", "inspect-log-toolbar");
  const kindSelect = el("select", "ds-select");
  const actorSelect = el("select", "ds-select");
  const followLabel = el("label", "inspect-follow");
  const follow = el("input", "ds-check");
  follow.type = "checkbox";
  follow.checked = true;
  followLabel.append(follow, document.createTextNode(" Follow playback"));
  toolbar.append(kindSelect, actorSelect, followLabel);

  const header = el("div", "inspect-log-row inspect-log-head");
  for (const title of ["seq", "time", "who", "what", ""]) header.appendChild(el("span", undefined, title));

  const viewport = el("div", "inspect-log");
  const spacer = el("div", "inspect-log-spacer");
  viewport.appendChild(spacer);
  const empty = el("p", "ds-help inspect-empty", "No messages match.");
  root.append(message, toolbar, header, viewport, empty);

  let rows: LogRow[] = [];
  let startAt = 0;
  let shown: LogRow[] = [];
  /** Index into `shown` of the row the canvas stands at, -1 for none. */
  let current = -1;
  let position = -1;
  /** The row somebody chose, which stands for the canvas as long as the
   * canvas is where that row left it. Several rows share a position -- a mark
   * and the pointer lifted after it -- and the one clicked is the one meant. */
  let chosen: LogRow | null = null;
  let frame: number | null = null;

  const locate = () => {
    const chosenAt = chosen ? shown.indexOf(chosen) : -1;
    return chosenAt >= 0 ? chosenAt : rowAt(shown, position);
  };

  const renderRow = (row: LogRow, at: number): HTMLElement => {
    const node = el(seek ? "button" : "div", "inspect-log-row");
    if (!row.drawable) node.classList.add("inspect-log-quiet");
    if (at === current) node.classList.add("inspect-log-current");
    if (at > current) node.classList.add("inspect-future");
    node.style.top = `${at * ROW_HEIGHT}px`;
    node.append(
      el("span", "inspect-log-seq", String(row.seq)),
      el("span", "inspect-time", elapsed(row.at - startAt)),
      el("span", "inspect-log-who", row.actor),
      el("span", "inspect-log-kind", row.kind),
      el("span", "inspect-log-summary", row.summary),
    );
    node.title = `seq ${row.seq} · ${new Date(row.at).toISOString()}${row.summary ? ` · ${row.summary}` : ""}`;
    if (seek) {
      (node as HTMLButtonElement).type = "button";
      node.addEventListener("click", () => {
        // After the seek, which may report the canvas where it was before
        // it moves; chosen first, the choice would not survive that.
        seek(row.position, row.seq);
        chosen = row;
        current = locate();
        schedule();
      });
    }
    return node;
  };

  /** Whether the current row is to be brought into view on the next frame. */
  let revealing = false;

  const render = () => {
    frame = null;
    spacer.style.height = `${shown.length * ROW_HEIGHT}px`;
    empty.style.display = shown.length === 0 && rows.length > 0 ? "" : "none";
    // Only once the spacer has its height: scrolled before, the viewport is
    // as short as its last contents and the scroll is clamped to nothing --
    // which is what a link to a row did on arrival.
    if (revealing) {
      revealing = false;
      if (follow.checked && current >= 0) {
        const rowTop = current * ROW_HEIGHT;
        const height = viewport.clientHeight;
        if (rowTop < viewport.scrollTop || rowTop + ROW_HEIGHT > viewport.scrollTop + height) {
          viewport.scrollTop = Math.max(0, rowTop - height / 2);
        }
      }
    }
    const top = viewport.scrollTop;
    const height = viewport.clientHeight || 400;
    const first = Math.max(0, Math.floor(top / ROW_HEIGHT) - OVERSCAN);
    const last = Math.min(shown.length, Math.ceil((top + height) / ROW_HEIGHT) + OVERSCAN);
    spacer.textContent = "";
    for (let at = first; at < last; at++) spacer.appendChild(renderRow(shown[at], at));
  };
  const schedule = () => {
    if (frame === null) frame = requestAnimationFrame(render);
  };

  /** Brings the current row into view, if following and it is not, on the
   * next frame. */
  const reveal = () => {
    revealing = true;
    schedule();
  };

  const filter = () => {
    const kind = kindSelect.value;
    const actor = actorSelect.value;
    shown = rows.filter(
      (row) =>
        (kind === ALL || (kind === DRAWING ? row.drawable : `kind:${row.kind}` === kind)) &&
        (actor === ALL || row.actor === actor),
    );
    current = locate();
  };
  const refilter = () => {
    filter();
    viewport.scrollTop = 0;
    reveal();
    schedule();
  };
  kindSelect.addEventListener("change", refilter);
  actorSelect.addEventListener("change", refilter);
  follow.addEventListener("change", () => {
    reveal();
    schedule();
  });
  viewport.addEventListener("scroll", schedule);

  return {
    root,
    count: () => rows.length,
    setData(data: InspectorData) {
      const unavailable = data.rows.length === 0 ? data.logUnavailable : undefined;
      message.textContent = unavailable ?? (data.rows.length === 0 ? "Nothing recorded yet." : "");
      message.style.display = message.textContent ? "" : "none";
      for (const part of [toolbar, header, viewport]) {
        part.style.display = data.rows.length === 0 ? "none" : "";
      }
      if (data.rows === rows) return;
      rows = data.rows;
      startAt = data.startAt ?? 0;
      // A chosen row survives a live update only if it is still the same
      // message; rows are rebuilt, so it is found again by its sequence.
      if (chosen) {
        const seq = chosen.seq;
        chosen = rows.find((row) => row.seq === seq) ?? null;
      }

      const kinds = new Map<string, number>();
      const actors = new Map<string, number>();
      for (const row of rows) {
        kinds.set(row.kind, (kinds.get(row.kind) ?? 0) + 1);
        actors.set(row.actor, (actors.get(row.actor) ?? 0) + 1);
      }
      refill(kindSelect, [
        option(ALL, `All messages (${rows.length})`),
        option(DRAWING, `Drawing only (${rows.filter((row) => row.drawable).length})`),
        ...Array.from(kinds.keys())
          .sort()
          .map((kind) => option(`kind:${kind}`, `${kind} (${kinds.get(kind)})`)),
      ]);
      refill(actorSelect, [
        option(ALL, "Everyone"),
        ...Array.from(actors.keys())
          .sort()
          .map((actor) => option(actor, `${actor} (${actors.get(actor)})`)),
      ]);
      // Filtered again in place: somebody reading row four hundred of a live
      // log should not be thrown back to the top every few seconds.
      filter();
      reveal();
      schedule();
    },
    update(next) {
      position = next;
      if (chosen && chosen.position !== position) chosen = null;
      current = locate();
      reveal();
      schedule();
    },
    shown() {
      reveal();
      schedule();
    },
    choose(seq) {
      chosen = rows.find((row) => row.seq === seq) ?? null;
      current = locate();
      reveal();
      schedule();
    },
  };
}
