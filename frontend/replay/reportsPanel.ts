/**
 * What each client said it believed, at the moments one of them said
 * something was wrong.
 *
 * Read against the recording: a report's positions are only interesting next
 * to the stream they disagree with, so each one offers the canvas at the
 * moment it was filed and at the position its client believed it had reached.
 */

import { positionAt } from "./archiveLog";
import { el, type InspectorData, type Panel, type Seek } from "./dom";
import { elapsed } from "./logRows";

/** One entry of the painter's synchronisation trace; see `HistoryTraceEvent`. */
type TraceEvent = {
  at: number;
  source: string;
  op: string;
  actor: string;
  seq?: number;
  action?: string;
};

/** What `reportDiagnostics` posts, as far as this page reads it. Anything
 * else in a report is still there in the raw view. */
type DiagnosticReport = {
  at?: string;
  reason?: string;
  localId?: number | null;
  appliedSequence?: number;
  expectedSequence?: number;
  lastSeq?: number;
  catchingUp?: boolean;
  settled?: boolean;
  detail?: string;
  trace?: TraceEvent[];
};

/** A report as the admin endpoint returns it, with what only its storage key
 * knows. */
export type FiledReport = {
  filed_at: string | null;
  filed_by: string | null;
  key: string;
  report: DiagnosticReport;
};

/**
 * Trace actions that mean a client's own work did not end up where it
 * thought: the ones a desync is made of. `replay` is ordinary but costly and
 * worth seeing; the rest are routine.
 */
const TROUBLE = ["abandoned", "diverged", "fallbehind"];

function fact(label: string, value: string, className?: string): HTMLElement {
  const node = el("span", `inspect-fact${className ? ` ${className}` : ""}`);
  node.append(el("span", "inspect-fact-label", label), document.createTextNode(value));
  return node;
}

function traceTable(trace: TraceEvent[], names: Map<string, string>): HTMLElement {
  const table = el("table", "inspect-trace");
  const head = el("tr");
  for (const title of ["ms", "source", "op", "actor", "seq", "action"]) head.appendChild(el("th", undefined, title));
  table.appendChild(head);
  for (const event of trace) {
    const row = el("tr");
    if (event.action && TROUBLE.indexOf(event.action) >= 0) row.className = "inspect-trace-trouble";
    else if (event.action === "replay") row.className = "inspect-trace-replay";
    row.append(
      el("td", undefined, String(Math.round(event.at))),
      el("td", undefined, event.source),
      el("td", undefined, event.op),
      el("td", undefined, names.get(event.actor) ?? event.actor),
      el("td", undefined, event.seq === undefined ? "" : String(event.seq)),
      el("td", undefined, event.action ?? ""),
    );
    table.appendChild(row);
  }
  return table;
}

/** When a report was filed, by the server's clock where known; the report's
 * own `at` is the client's and is the fallback. NaN when neither says. */
export function filedAt(filed: FiledReport): number {
  const when = filed.filed_at ?? filed.report.at ?? null;
  return when ? Date.parse(when) : NaN;
}

export function reportsPanel(seek: Seek): Panel {
  const root = el("div", "inspect-panel inspect-reports");
  let reports: FiledReport[] | null = null;
  let unavailable: string | undefined;
  /** Read when a button is pressed, so a live session's growth is counted. */
  let current: InspectorData | null = null;

  const render = (data: InspectorData) => {
    root.textContent = "";
    if (data.reportsUnavailable) {
      root.appendChild(el("p", "ds-help inspect-empty", data.reportsUnavailable));
      return;
    }
    if (data.reports.length === 0) {
      root.appendChild(el("p", "ds-help inspect-empty", "No client filed a report."));
      return;
    }
    for (const filed of data.reports) root.appendChild(card(filed));
  };

  const card = (filed: FiledReport): HTMLElement => {
    const report = filed.report;
    const names = new Map<string, string>();
    current?.names.forEach((name, id) => names.set(String(id), name));
    const startAt = current?.startAt ?? null;
    const section = el("section", "inspect-report");

    const whenMs = filedAt(filed);
    const title = el("h3", "inspect-report-title");
    title.append(
      el("span", "inspect-chat-who", filed.filed_by ?? "someone"),
      el("span", "admin-tag inspect-tag-danger", report.reason ?? "no reason given"),
      el(
        "span",
        "inspect-time",
        Number.isFinite(whenMs)
          ? startAt !== null
            ? `${elapsed(whenMs - startAt)}  ·  ${new Date(whenMs).toISOString()}`
            : new Date(whenMs).toISOString()
          : "",
      ),
    );
    section.appendChild(title);

    const facts = el("p", "inspect-facts");
    const applied = report.appliedSequence;
    const last = report.lastSeq;
    // The shape the checkpoint bug took: a client behind the room and not
    // catching up, which is what nothing outside the tab could see.
    const behind =
      typeof applied === "number" && typeof last === "number" && applied < last && !report.catchingUp;
    if (typeof report.localId === "number") {
      facts.appendChild(fact("session id", String(report.localId)));
    }
    if (typeof applied === "number") facts.appendChild(fact("applied", String(applied), behind ? "inspect-bad" : undefined));
    if (typeof report.expectedSequence === "number") facts.appendChild(fact("expected", String(report.expectedSequence)));
    if (typeof last === "number") facts.appendChild(fact("room at", String(last)));
    if (behind) facts.appendChild(fact("behind by", String((last as number) - (applied as number)), "inspect-bad"));
    if (typeof report.catchingUp === "boolean") facts.appendChild(fact("catching up", report.catchingUp ? "yes" : "no"));
    if (typeof report.settled === "boolean") {
      facts.appendChild(fact("settled", report.settled ? "yes" : "no", report.settled ? undefined : "inspect-warn"));
    }
    section.appendChild(facts);
    if (report.detail) section.appendChild(el("pre", "inspect-report-detail", report.detail));

    if (seek) {
      const actions = el("p", "inspect-report-actions");
      if (Number.isFinite(whenMs)) {
        const button = el("button", "ds-button ds-button-small", "Canvas when filed");
        button.type = "button";
        button.addEventListener("click", () => {
          if (current) seek(positionAt(current.drawTimes, whenMs));
        });
        actions.appendChild(button);
      }
      if (typeof applied === "number") {
        const button = el("button", "ds-button ds-button-small", `Canvas at seq ${applied}`);
        button.type = "button";
        button.title = "Where this client believed it had got to";
        button.addEventListener("click", () => {
          if (current) seek(current.positionOfSeq(applied), applied);
        });
        actions.appendChild(button);
      }
      section.appendChild(actions);
    }

    const trace = Array.isArray(report.trace) ? report.trace : [];
    if (trace.length > 0) {
      const troubled = trace.filter((event) => event.action && TROUBLE.indexOf(event.action) >= 0).length;
      const details = el("details");
      details.appendChild(
        el(
          "summary",
          undefined,
          `Trace, ${trace.length} events` + (troubled ? ` (${troubled} abandoned, diverged or fell behind)` : ""),
        ),
      );
      details.appendChild(traceTable(trace, names));
      section.appendChild(details);
    }

    const raw = el("details");
    raw.appendChild(el("summary", undefined, "Raw report"));
    raw.appendChild(el("pre", "inspect-raw", JSON.stringify(filed, null, 2)));
    section.appendChild(raw);
    return section;
  };

  return {
    root,
    count: () => (reports ? reports.length : 0),
    setData(data: InspectorData) {
      current = data;
      // Rebuilt only when a report arrives: a live session asks every few
      // seconds, and a rebuild would close the trace somebody was reading.
      if (reports && data.reports.length === reports.length && data.reportsUnavailable === unavailable) {
        return;
      }
      reports = data.reports;
      unavailable = data.reportsUnavailable;
      render(data);
    },
  };
}
