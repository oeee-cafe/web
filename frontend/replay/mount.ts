/**
 * The session page: a recording played back, with its log, its conversation
 * and its clients' reports beside it, all tied to the same position.
 *
 * Staff-only by where it gets its data. Every endpoint is behind the admin
 * extractor, so a page served to anybody else fetches 403s and says so -- the
 * viewer holds no permission of its own and cannot be made to.
 */

import { mount, type PainterHandle } from "neo-cucumber";
import {
  decodeArchive,
  isRenderable,
  type ArchiveManifest,
  type ArchivedChat,
  type ArchivedEntry,
} from "./archiveLog";
import { chatPanel } from "./chatPanel";
import { el, type Panel, type Seek } from "./dom";
import { logPanel } from "./logPanel";
import { logRows, type LogRow } from "./logRows";
import { createReplay, drawableEntries, type ReplayHandle } from "./player";
import { reportsPanel, type FiledReport } from "./reportsPanel";

const SPEEDS = [1, 2, 4, 16];

/** The one participant a replay does not have. Named so it cannot collide
 * with a session id, which are the small integers the room assigns. */
const VIEWER_ACTOR = "replay-viewer";

type Loaded<T> = { ok: true; value: T } | { ok: false; status: number; message: string };

/**
 * One endpoint, read without throwing: each part of the page stands alone, so
 * a session with no recording still shows what was said about it.
 */
async function load<T>(url: string, read: (response: Response) => Promise<T>): Promise<Loaded<T>> {
  try {
    const response = await fetch(url, { credentials: "include" });
    if (!response.ok) {
      let message = `${response.status} ${response.statusText}`;
      try {
        const body = await response.json();
        if (body && body.error && typeof body.error.message === "string") message = body.error.message;
      } catch {
        // Not the server's JSON error; the status says enough.
      }
      return { ok: false, status: response.status, message };
    }
    return { ok: true, value: await read(response) };
  } catch (error) {
    return { ok: false, status: 0, message: error instanceof Error ? error.message : String(error) };
  }
}

/** Why a part could not be shown, in a sentence, or undefined if it could. */
function unavailable(loaded: Loaded<unknown>, what: string): string | undefined {
  if (loaded.ok) return undefined;
  return `Could not read the ${what}: ${loaded.message}.`;
}

export async function mountReplay(host: HTMLElement, session: string): Promise<void> {
  const base = `/admin/collaborative-sessions/${session}`;

  const header = el("header", "inspect-header");
  const back = el("a", "inspect-back", "← Sessions");
  back.href = "/admin/collaborative-sessions";
  const heading = el("h1", "inspect-title", `Session ${session.slice(0, 8)}`);
  heading.title = session;
  const links = el("span", "inspect-links");
  const logLink = el("a", undefined, "log ↓");
  logLink.href = `${base}/archive`;
  logLink.setAttribute("download", "");
  const manifestLink = el("a", undefined, "manifest");
  manifestLink.href = `${base}/manifest`;
  links.append(logLink, manifestLink);
  header.append(back, heading, links);
  const status = el("p", "replay-status", "Loading…");
  host.append(header, status);

  const [manifestLoaded, logLoaded, chatLoaded, reportsLoaded] = await Promise.all([
    load<ArchiveManifest>(`${base}/manifest`, (response) => response.json()),
    load(`${base}/archive`, async (response) => new Uint8Array(await response.arrayBuffer())),
    load<ArchivedChat[]>(`${base}/chat`, (response) => response.json()),
    load<FiledReport[]>(`${base}/diagnostics`, (response) => response.json()),
  ]);

  const manifest = manifestLoaded.ok ? manifestLoaded.value : null;
  const decoded = logLoaded.ok ? decodeArchive(logLoaded.value) : null;
  const entries: ArchivedEntry[] = decoded ?? [];
  // Only a manifest and a log together are a recording: the manifest says how
  // big the canvas is and who each session id was.
  const recorded = manifest !== null && decoded !== null;
  const noRecording = !manifestLoaded.ok
    ? manifestLoaded.status === 404
      ? "No recording for this session."
      : unavailable(manifestLoaded, "manifest")
    : !logLoaded.ok
      ? logLoaded.status === 404
        ? "No recording for this session."
        : unavailable(logLoaded, "log")
      : decoded === null
        ? "That file is not a recording."
        : undefined;

  const sessionNames = new Map<number, string>();
  const traceNames = new Map<string, string>();
  for (const participant of manifest?.participants ?? []) {
    sessionNames.set(participant.session_id, participant.login_name);
    traceNames.set(String(participant.session_id), participant.login_name);
  }

  const rows: LogRow[] = logRows(entries, sessionNames);
  const drawn = drawableEntries(entries).map((held) => held.entry);
  const drawTimes = drawn.map((entry) => entry.at);
  const startAt = entries.length > 0 ? entries[0].at : null;
  /** The canvas once the recording has reached `seq`. */
  const positionOfSeq = (seq: number) => {
    let low = 0;
    let high = rows.length;
    while (low < high) {
      const middle = (low + high) >> 1;
      if (rows[middle].seq <= seq) low = middle + 1;
      else high = middle;
    }
    return low > 0 ? rows[low - 1].position : -1;
  };

  let replay: ReplayHandle | null = null;
  const seek: Seek = recorded
    ? (position: number) => {
        if (!replay) return;
        replay.pause();
        void replay.seek(position);
      }
    : undefined;

  const panels: { key: string; label: string; panel: Panel }[] = [
    {
      key: "chat",
      label: "Chat",
      panel: chatPanel({
        chat: chatLoaded.ok ? chatLoaded.value : [],
        unavailable: unavailable(chatLoaded, "transcript"),
        drawTimes,
        startAt,
        seek,
      }),
    },
    {
      key: "log",
      label: "Log",
      panel: logPanel({ rows, unavailable: recorded ? undefined : noRecording, startAt, seek }),
    },
    {
      key: "reports",
      label: "Reports",
      panel: reportsPanel({
        reports: reportsLoaded.ok ? reportsLoaded.value : [],
        unavailable: unavailable(reportsLoaded, "reports"),
        drawTimes,
        positionOfSeq,
        names: traceNames,
        startAt,
        seek,
      }),
    },
  ];

  const stage = el("div", "replay-stage");
  const main = el("div", "replay-main");
  const canvasHost = el("div", "replay-canvas");
  const controls = el("div", "replay-controls");
  main.append(canvasHost, controls);
  const side = el("aside", "inspect-side");
  const tabs = el("nav", "inspect-tabs");
  side.appendChild(tabs);
  stage.append(main, side);
  host.appendChild(stage);

  // The tab is kept in the address, so a link to a session's reports opens on
  // its reports.
  const buttons = new Map<string, HTMLButtonElement>();
  const select = (key: string) => {
    for (const { key: other, panel } of panels) {
      const on = other === key;
      panel.root.style.display = on ? "" : "none";
      buttons.get(other)?.setAttribute("aria-selected", on ? "true" : "false");
      if (on) panel.shown?.();
    }
    try {
      history.replaceState(null, "", `#${key}`);
    } catch {
      // Only a convenience.
    }
  };
  for (const { key, label, panel } of panels) {
    const button = el("button", "inspect-tab", panel.count > 0 ? `${label} ${panel.count}` : label);
    button.type = "button";
    button.setAttribute("role", "tab");
    button.addEventListener("click", () => select(key));
    buttons.set(key, button);
    tabs.appendChild(button);
    side.appendChild(panel.root);
  }
  const hashed = window.location.hash.slice(1);
  const byCount = (key: string) => (panels.find((entry) => entry.key === key)?.panel.count ?? 0) > 0;
  // Reports first when there are any: somebody who filed one is why this page
  // is usually open.
  select(
    panels.some((entry) => entry.key === hashed)
      ? hashed
      : byCount("reports")
        ? "reports"
        : byCount("chat")
          ? "chat"
          : "log",
  );

  const update = (position: number, playing: boolean) => {
    for (const { panel } of panels) panel.update?.(position, playing);
  };

  if (!recorded || !manifest) {
    canvasHost.classList.add("replay-canvas-missing");
    canvasHost.textContent = noRecording ?? "No recording for this session.";
    controls.style.display = "none";
    status.textContent = "";
    // Everything said and reported is at its end state: there is no drawing
    // to have reached any point of.
    update(Number.MAX_SAFE_INTEGER, false);
    return;
  }

  // A recording that does not start at the room's first message is missing
  // whatever a checkpoint squashed, and drawing it would present a fragment as
  // the finished picture.
  const partial = !isRenderable(manifest);

  let mounted: PainterHandle | null = null;
  const newPainter = async (): Promise<PainterHandle> => {
    // Unmounted before the host is cleared, not after: `unmount` releases
    // listeners and framework roots by taking its own nodes out, and emptying
    // the host first leaves it removing children that are no longer there.
    mounted?.unmount();
    mounted = null;
    canvasHost.textContent = "";
    const painter = mount(canvasHost, {
      width: manifest.canvas.width,
      height: manifest.canvas.height,
      mode: { kind: "standard" },
      // No toolbox: nothing here is editable, and a replay that offered a
      // brush would be inviting somebody to draw on the record.
      controls: { kind: "none" },
      recordReplay: false,
      // Present so the painter runs in controlled mode, which is what makes
      // `applyCanonicalOperation` the way pixels arrive. It never emits: the
      // canvas takes no input.
      synchronization: { actorId: VIEWER_ACTOR, onOperation: () => {} },
    });
    await painter.ready;
    painter.setInteractionEnabled(false);
    painter.setParticipants(
      manifest.participants.map((participant) => ({
        actorId: String(participant.session_id),
        name: participant.login_name,
      })),
    );
    mounted = painter;
    return painter;
  };

  const painter = await newPainter();
  // As wide as the canvas and no wider, with room for the controls: left to
  // size itself, the scrubber's flex basis grows the column until the side
  // panel has nowhere to go but underneath.
  main.style.width = `${Math.max(canvasHost.offsetWidth, 420)}px`;

  const playButton = el("button", "replay-button", "Play");
  const restartButton = el("button", "replay-button", "Restart");
  const stepBackButton = el("button", "replay-button", "◀");
  stepBackButton.title = "One mark back (←)";
  const stepButton = el("button", "replay-button", "▶");
  stepButton.title = "One mark forward (→)";
  const endButton = el("button", "replay-button", "Jump to end");
  const speedButton = el("button", "replay-button", "1×");
  const scrubber = el("input", "replay-scrubber");
  scrubber.type = "range";
  scrubber.min = "-1";
  scrubber.step = "1";
  const readout = el("span", "replay-readout");

  controls.append(
    playButton,
    restartButton,
    stepBackButton,
    stepButton,
    endButton,
    speedButton,
    scrubber,
    readout,
  );

  let speedIndex = 0;
  let position = -1;

  replay = createReplay({
    painter,
    entries,
    remount: newPainter,
    onProgress: (index, playing) => {
      position = index;
      playButton.textContent = playing ? "Pause" : "Play";
      scrubber.value = String(index);
      const entry = index >= 0 ? drawn[index] : undefined;
      readout.textContent =
        `${index + 1} / ${replay?.length ?? 0}` +
        (entry ? `  ·  seq ${entry.seq}  ·  ${new Date(entry.at).toISOString()}` : "");
      update(index, playing);
    },
  });
  const player = replay;

  scrubber.max = String(player.length - 1);
  scrubber.value = "-1";

  const togglePlay = () => {
    if (playButton.textContent === "Play") player.play();
    else player.pause();
  };
  const step = (by: number) => {
    player.pause();
    void player.seek(position + by);
  };
  playButton.addEventListener("click", togglePlay);
  restartButton.addEventListener("click", () => {
    player.pause();
    void player.seek(-1);
  });
  stepBackButton.addEventListener("click", () => step(-1));
  stepButton.addEventListener("click", () => step(1));
  endButton.addEventListener("click", () => {
    player.pause();
    void player.seek(player.length - 1);
  });
  speedButton.addEventListener("click", () => {
    speedIndex = (speedIndex + 1) % SPEEDS.length;
    player.setSpeed(SPEEDS[speedIndex]);
    speedButton.textContent = `${SPEEDS[speedIndex]}×`;
  });
  // On release rather than on drag: a seek backwards rebuilds the canvas from
  // the first message, and doing that for every pixel of a drag would be a
  // hundred rebuilds nobody asked for.
  scrubber.addEventListener("change", () => {
    player.pause();
    void player.seek(Number(scrubber.value));
  });
  // Space and the arrows, unless somebody is typing or choosing in a control
  // that wants them.
  document.addEventListener("keydown", (event) => {
    const target = event.target as HTMLElement | null;
    if (target && /^(INPUT|SELECT|TEXTAREA)$/.test(target.tagName)) return;
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === " ") {
      event.preventDefault();
      togglePlay();
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      step(-1);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      step(1);
    }
  });

  const participants = manifest.participants
    .map((participant) => `${participant.login_name} (${participant.session_id})`)
    .join(", ");
  status.textContent =
    `${player.length} drawing messages of ${entries.length} recorded` +
    `  ·  ${manifest.canvas.width}×${manifest.canvas.height}` +
    `  ·  started ${manifest.started_at}` +
    (participants ? `  ·  ${participants}` : "") +
    (manifest.sealed ? "" : "  ·  not sealed") +
    (partial
      ? `  ·  INCOMPLETE: this recording starts at sequence ${manifest.recording.first_seq}, so everything before it is missing`
      : "");
  if (partial) status.classList.add("replay-incomplete");
  readout.textContent = `0 / ${player.length}`;
  update(-1, false);
}
