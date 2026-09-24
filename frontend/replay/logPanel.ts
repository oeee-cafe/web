/**
 * Every message in the recording, one row each, following the replay.
 *
 * Windowed: a long afternoon is tens of thousands of messages, and a row per
 * message in the DOM would make scrolling the page the slowest thing on it.
 * Rows are one fixed height so where each one sits is arithmetic.
 */

import { el, type Panel, type Seek } from "./dom";
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

export function logPanel(options: {
  rows: LogRow[];
  unavailable?: string;
  startAt: number | null;
  seek: Seek;
}): Panel {
  const { rows, seek } = options;
  const startAt = options.startAt ?? 0;
  const root = el("div", "inspect-panel");

  if (options.unavailable) {
    root.appendChild(el("p", "inspect-empty", options.unavailable));
    return { root, count: 0 };
  }
  // Unpadded, so the rows run edge to edge; only once there are rows.
  root.classList.add("inspect-log-panel");

  // Filters, by what a message was and by who sent it.
  const toolbar = el("div", "inspect-log-toolbar");
  const kindSelect = el("select");
  const kinds = new Map<string, number>();
  const actors = new Map<string, number>();
  for (const row of rows) {
    kinds.set(row.kind, (kinds.get(row.kind) ?? 0) + 1);
    actors.set(row.actor, (actors.get(row.actor) ?? 0) + 1);
  }
  kindSelect.append(
    option(ALL, `All messages (${rows.length})`),
    option(DRAWING, `Drawing only (${rows.filter((row) => row.drawable).length})`),
  );
  Array.from(kinds.keys())
    .sort()
    .forEach((kind) => kindSelect.appendChild(option(`kind:${kind}`, `${kind} (${kinds.get(kind)})`)));
  const actorSelect = el("select");
  actorSelect.appendChild(option(ALL, "Everyone"));
  Array.from(actors.keys())
    .sort()
    .forEach((actor) => actorSelect.appendChild(option(actor, `${actor} (${actors.get(actor)})`)));
  const followLabel = el("label", "inspect-follow");
  const follow = el("input");
  follow.type = "checkbox";
  follow.checked = true;
  followLabel.append(follow, document.createTextNode(" Follow playback"));
  toolbar.append(kindSelect, actorSelect, followLabel);

  const header = el("div", "inspect-log-row inspect-log-head");
  for (const title of ["seq", "time", "who", "what", ""]) header.appendChild(el("span", undefined, title));

  const viewport = el("div", "inspect-log");
  const spacer = el("div", "inspect-log-spacer");
  viewport.appendChild(spacer);
  const empty = el("p", "inspect-empty", "No messages match.");
  root.append(toolbar, header, viewport, empty);

  let shown = rows;
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
        chosen = row;
        seek(row.position);
      });
    }
    return node;
  };

  const render = () => {
    frame = null;
    spacer.style.height = `${shown.length * ROW_HEIGHT}px`;
    empty.style.display = shown.length === 0 ? "" : "none";
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

  /** Brings the current row into view, if following and it is not. */
  const reveal = () => {
    if (!follow.checked || current < 0) return;
    const rowTop = current * ROW_HEIGHT;
    const height = viewport.clientHeight;
    if (rowTop < viewport.scrollTop || rowTop + ROW_HEIGHT > viewport.scrollTop + height) {
      viewport.scrollTop = Math.max(0, rowTop - height / 2);
    }
  };

  const applyFilter = () => {
    const kind = kindSelect.value;
    const actor = actorSelect.value;
    shown = rows.filter(
      (row) =>
        (kind === ALL || (kind === DRAWING ? row.drawable : `kind:${row.kind}` === kind)) &&
        (actor === ALL || row.actor === actor),
    );
    current = locate();
    viewport.scrollTop = 0;
    reveal();
    schedule();
  };
  kindSelect.addEventListener("change", applyFilter);
  actorSelect.addEventListener("change", applyFilter);
  follow.addEventListener("change", () => {
    reveal();
    schedule();
  });
  viewport.addEventListener("scroll", schedule);

  return {
    root,
    count: rows.length,
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
  };
}
