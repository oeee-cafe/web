/**
 * What each client said it believed, at the moments one of them said
 * something was wrong.
 *
 * Read against the recording: a report's positions are only interesting next
 * to the stream they disagree with, so each one offers the canvas at the
 * moment it was filed and at the position its client believed it had reached.
 */

import { positionAt } from "./archiveLog";
import { el, type Panel, type Seek } from "./dom";
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

export function reportsPanel(options: {
  reports: FiledReport[];
  unavailable?: string;
  drawTimes: number[];
  /** The canvas position once the recording has reached a sequence. */
  positionOfSeq: (seq: number) => number;
  /** Session id, as the trace writes it, to login name. */
  names: Map<string, string>;
  startAt: number | null;
  seek: Seek;
}): Panel {
  const { reports, drawTimes, positionOfSeq, names, startAt, seek } = options;
  const root = el("div", "inspect-panel inspect-reports");

  if (options.unavailable) {
    root.appendChild(el("p", "inspect-empty", options.unavailable));
    return { root, count: 0 };
  }
  if (reports.length === 0) {
    root.appendChild(el("p", "inspect-empty", "No client filed a report."));
    return { root, count: 0 };
  }

  for (const filed of reports) {
    const report = filed.report;
    const card = el("section", "inspect-report");

    // Filed by the server's clock, which is the log's; the report's own `at`
    // is the client's and is the fallback.
    const when = filed.filed_at ?? report.at ?? null;
    const whenMs = when ? Date.parse(when) : NaN;
    const title = el("h3", "inspect-report-title");
    title.append(
      el("span", "inspect-chat-who", filed.filed_by ?? "someone"),
      el("span", "inspect-tag", report.reason ?? "no reason given"),
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
    card.appendChild(title);

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
    card.appendChild(facts);
    if (report.detail) card.appendChild(el("pre", "inspect-report-detail", report.detail));

    if (seek) {
      const actions = el("p", "inspect-report-actions");
      if (Number.isFinite(whenMs)) {
        const button = el("button", "replay-button", "Canvas when filed");
        button.type = "button";
        button.addEventListener("click", () => seek(positionAt(drawTimes, whenMs)));
        actions.appendChild(button);
      }
      if (typeof applied === "number") {
        const button = el("button", "replay-button", `Canvas at seq ${applied}`);
        button.type = "button";
        button.title = "Where this client believed it had got to";
        button.addEventListener("click", () => seek(positionOfSeq(applied)));
        actions.appendChild(button);
      }
      card.appendChild(actions);
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
      card.appendChild(details);
    }

    const raw = el("details");
    raw.appendChild(el("summary", undefined, "Raw report"));
    raw.appendChild(el("pre", "inspect-raw", JSON.stringify(filed, null, 2)));
    card.appendChild(raw);

    root.appendChild(card);
  }

  return { root, count: reports.length };
}
