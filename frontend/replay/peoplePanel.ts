/**
 * Everyone who was in a session: what they did, when they were there, and a
 * switch for looking at their marks alone.
 *
 * Hiding somebody's layers is how "my strokes vanished" gets answered -- with
 * everyone else out of the way it is plain whether the marks were never there
 * or are there under something.
 */

import { el, type InspectorData, type Panel } from "./dom";
import { elapsed } from "./logRows";
import { participantStats, type PersonStats } from "./people";

type Person = {
  /** Session id, when they drew or were seated; the stream's name for them. */
  sessionId: number | null;
  login: string;
  displayName: string | null;
  joinedAt: string | null;
  leftAt: string | null;
  inRoom: boolean;
  stats: PersonStats | null;
  said: number;
  reported: number;
};

/** Everyone the recording or the database knows of, matched by login name. */
function people(data: InspectorData): Person[] {
  const stats = participantStats(data.rows);
  const bySession = new Map<number, PersonStats>();
  for (const held of stats) bySession.set(held.sessionId, held);
  const count = (list: string[], login: string) => list.filter((name) => name === login).length;
  const speakers = data.chat.map((line) => line.login_name);
  const reporters = data.reports.map((filed) => filed.filed_by ?? "");

  const found: Person[] = [];
  const seen = new Set<string>();
  const ids = new Set<number>(bySession.keys());
  data.names.forEach((_, id) => ids.add(id));
  Array.from(ids)
    .sort((a, b) => a - b)
    .forEach((id) => {
      const login = data.names.get(id) ?? `#${id}`;
      const row = data.details?.participants.find((participant) => participant.login_name === login);
      seen.add(login);
      found.push({
        sessionId: id,
        login,
        displayName: row?.display_name ?? null,
        joinedAt: row?.joined_at ?? null,
        leftAt: row?.left_at ?? null,
        inRoom: row?.is_active ?? false,
        stats: bySession.get(id) ?? null,
        said: count(speakers, login),
        reported: count(reporters, login),
      });
    });
  // Joined and never drew, or drew before the room's ids were kept.
  for (const row of data.details?.participants ?? []) {
    if (seen.has(row.login_name)) continue;
    found.push({
      sessionId: null,
      login: row.login_name,
      displayName: row.display_name,
      joinedAt: row.joined_at,
      leftAt: row.left_at,
      inRoom: row.is_active,
      stats: null,
      said: count(speakers, row.login_name),
      reported: count(reporters, row.login_name),
    });
  }
  return found;
}

/** Their marks over the recording, as a row of bars. */
function sparkline(activity: number[]): HTMLElement {
  const strip = el("span", "inspect-spark");
  const most = Math.max(1, ...activity);
  for (const marks of activity) {
    const bar = el("span", "inspect-spark-bar");
    bar.style.height = `${marks === 0 ? 0 : Math.max(8, Math.round((marks / most) * 100))}%`;
    strip.appendChild(bar);
  }
  return strip;
}

/** A database timestamp as stored: local time, with no zone attached. */
function stamp(value: string): string {
  return value.replace("T", " ").slice(0, 16);
}

export function peoplePanel(onHidden: (actorIds: string[]) => void): Panel & { hidden(): string[] } {
  const root = el("div", "inspect-panel inspect-people");
  let hidden = new Set<string>();
  let list: Person[] = [];
  let data: InspectorData | null = null;

  const changed = () => {
    onHidden(Array.from(hidden));
    if (data) render(data);
  };

  const render = (next: InspectorData) => {
    data = next;
    list = people(next);
    root.textContent = "";
    if (list.length === 0) {
      root.appendChild(el("p", "ds-help inspect-empty", "Nobody is on record."));
      return;
    }
    const seated = list.filter((person) => person.sessionId !== null);
    if (seated.length > 1) {
      const bar = el("p", "inspect-people-actions");
      const all = el("button", "ds-button ds-button-small", "Show everyone");
      all.type = "button";
      all.disabled = hidden.size === 0;
      all.addEventListener("click", () => {
        hidden = new Set();
        changed();
      });
      bar.appendChild(all);
      root.appendChild(bar);
    }

    const start = next.startAt;
    for (const person of list) {
      const card = el("section", "inspect-person");
      const head = el("div", "inspect-person-head");
      const id = person.sessionId === null ? null : String(person.sessionId);

      if (id !== null && seated.length > 1) {
        const label = el("label", "inspect-person-show");
        const box = el("input", "ds-check");
        box.type = "checkbox";
        box.checked = !hidden.has(id);
        box.title = "Show their layers";
        box.addEventListener("change", () => {
          if (box.checked) hidden.delete(id);
          else hidden.add(id);
          changed();
        });
        label.appendChild(box);
        head.appendChild(label);
      }
      const name = el("a", "inspect-chat-who", person.login);
      name.href = `/@${encodeURIComponent(person.login)}`;
      name.target = "_blank";
      head.appendChild(name);
      if (person.displayName && person.displayName !== person.login) {
        head.appendChild(el("span", "inspect-muted", person.displayName));
      }
      if (id !== null) head.appendChild(el("span", "admin-tag", `#${id}`));
      if (person.inRoom) head.appendChild(el("span", "admin-tag inspect-tag-live", "in the room"));
      if (id !== null && seated.length > 1) {
        const only = el("button", "ds-button ds-button-small", "Only");
        only.type = "button";
        only.title = "Hide everyone else's layers";
        only.addEventListener("click", () => {
          hidden = new Set(seated.map((other) => String(other.sessionId)).filter((other) => other !== id));
          changed();
        });
        head.appendChild(only);
      }
      card.appendChild(head);

      const facts = el("p", "inspect-facts");
      const stats = person.stats;
      const add = (label: string, value: number | string, className?: string) => {
        const node = el("span", `inspect-fact${className ? ` ${className}` : ""}`);
        node.append(el("span", "inspect-fact-label", label), document.createTextNode(String(value)));
        facts.appendChild(node);
      };
      if (stats) {
        add("marks", stats.marks);
        add("strokes", stats.strokes);
        add("undo", stats.undos);
        add("redo", stats.redos);
        if (stats.onOthers) add("on others' layers", stats.onOthers, "inspect-warn");
        if (stats.byOthers) add("by others on theirs", stats.byOthers, "inspect-warn");
      } else {
        add("marks", 0);
      }
      add("chat", person.said);
      if (person.reported) add("reports", person.reported, "inspect-bad");
      card.appendChild(facts);

      const when = el("p", "inspect-person-when");
      const parts: string[] = [];
      if (stats && stats.firstAt !== null && stats.lastAt !== null && start !== null) {
        parts.push(`drew ${elapsed(stats.firstAt - start)} to ${elapsed(stats.lastAt - start)}`);
      }
      if (person.joinedAt) parts.push(`joined ${stamp(person.joinedAt)}`);
      if (person.leftAt) parts.push(`left ${stamp(person.leftAt)}`);
      when.textContent = parts.join("  ·  ");
      if (when.textContent) card.appendChild(when);
      if (stats && stats.marks > 0) card.appendChild(sparkline(stats.activity));

      root.appendChild(card);
    }
  };

  return {
    root,
    count: () => list.length,
    hidden: () => Array.from(hidden),
    setData: render,
  };
}

