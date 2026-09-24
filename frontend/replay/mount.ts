/**
 * The session page: a recording played back, with its log, its conversation,
 * its people and its clients' reports beside it, all tied to one position --
 * and, for a session still going, kept up to date as it goes.
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
import { el, type InspectorData, type Panel, type Seek, type SessionDetails } from "./dom";
import { header } from "./header";
import { logPanel } from "./logPanel";
import { logRows } from "./logRows";
import { markers } from "./markers";
import { peoplePanel } from "./peoplePanel";
import { createReplay, drawableEntries, type ReplayHandle } from "./player";
import { checkReplay } from "./replayCheck";
import { filedAt, reportsPanel, type FiledReport } from "./reportsPanel";

const SPEEDS = [1, 2, 4, 16];

/** How often a live session is asked what is new. Each ask is a listing and
 * a Redis read on the server, never a flush. */
const LIVE_POLL_MS = 5000;

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

/** A log's bytes as entries: nothing for an empty answer, which is what a
 * tail with nothing new is. */
function entriesOf(bytes: Uint8Array): ArchivedEntry[] | null {
  return bytes.length === 0 ? [] : decodeArchive(bytes);
}

/**
 * Where the address says to look: `#<tab>` and optionally `&seq=<n>`, so a
 * link can hand somebody the canvas, the tab and the row at one moment.
 */
function readHash(): { tab: string | null; seq: number | null } {
  const [tab, ...rest] = window.location.hash.slice(1).split("&");
  let seq: number | null = null;
  for (const part of rest) {
    const [key, value] = part.split("=");
    if (key === "seq" && /^\d+$/.test(value ?? "")) seq = Number(value);
  }
  return { tab: tab || null, seq };
}

function writeHash(tab: string, seq: number | null) {
  try {
    history.replaceState(null, "", `#${tab}${seq !== null ? `&seq=${seq}` : ""}`);
  } catch {
    // Only a convenience.
  }
}

export async function mountReplay(host: HTMLElement, session: string): Promise<void> {
  const base = `/admin/collaborative-sessions/${session}`;

  const head = header(session);
  const logLink = el("a", "ds-button ds-button-small", "log ↓");
  logLink.href = `${base}/archive`;
  logLink.setAttribute("download", "");
  const manifestLink = el("a", "ds-button ds-button-small", "manifest");
  manifestLink.href = `${base}/manifest`;
  const copyLink = el("button", "ds-button ds-button-small", "copy link");
  copyLink.type = "button";
  copyLink.title = "A link to this tab at this moment";
  copyLink.addEventListener("click", () => {
    const href = window.location.href;
    const done = () => {
      copyLink.textContent = "copied";
      setTimeout(() => (copyLink.textContent = "copy link"), 1500);
    };
    if (navigator.clipboard) {
      navigator.clipboard.writeText(href).then(done, () => window.prompt("Link", href));
    } else {
      window.prompt("Link", href);
    }
  });
  head.links.append(copyLink, logLink, manifestLink);
  const status = el("p", "ds-help replay-status", "Loading…");
  host.append(head.root, status);

  const [manifestLoaded, logLoaded, chatLoaded, reportsLoaded, detailsLoaded] = await Promise.all([
    load<ArchiveManifest>(`${base}/manifest`, (response) => response.json()),
    load(`${base}/archive`, async (response) => new Uint8Array(await response.arrayBuffer())),
    load<ArchivedChat[]>(`${base}/chat`, (response) => response.json()),
    load<FiledReport[]>(`${base}/diagnostics`, (response) => response.json()),
    load<SessionDetails>(`${base}/details`, (response) => response.json()),
  ]);

  let details = detailsLoaded.ok ? detailsLoaded.value : null;
  head.setDetails(details, unavailable(detailsLoaded, "session"));
  const manifest = manifestLoaded.ok ? manifestLoaded.value : null;
  // A 404 for the log is a session with nothing stored yet, which a live one
  // can still grow out of; anything else is a failure to say.
  const decoded = logLoaded.ok ? entriesOf(logLoaded.value) : logLoaded.status === 404 ? [] : null;
  let entries: ArchivedEntry[] = decoded ?? [];
  const logProblem = !logLoaded.ok
    ? logLoaded.status === 404
      ? "No recording for this session."
      : unavailable(logLoaded, "log")
    : decoded === null
      ? "That file is not a recording."
      : undefined;
  let live = details !== null && !details.session.ended_at;
  let chat: ArchivedChat[] = chatLoaded.ok ? chatLoaded.value : [];
  let chatUnavailable = unavailable(chatLoaded, "transcript");
  let reports: FiledReport[] = reportsLoaded.ok ? reportsLoaded.value : [];
  let reportsUnavailable = unavailable(reportsLoaded, "reports");

  /** Session id to login name: the manifest's record, and over it whoever the
   * room says holds each id now -- somebody who joined after the manifest was
   * last written is named only there. */
  const names = new Map<number, string>();
  const learnNames = () => {
    for (const participant of manifest?.participants ?? []) {
      names.set(participant.session_id, participant.login_name);
    }
    for (const seat of details?.seats ?? []) names.set(seat.session_id, seat.login_name);
  };
  learnNames();

  const build = (): InspectorData => {
    const rows = logRows(entries, names);
    const drawTimes = drawableEntries(entries).map((held) => held.entry.at);
    return {
      rows,
      drawTimes,
      startAt: entries.length > 0 ? entries[0].at : null,
      names,
      logUnavailable: logProblem,
      chat,
      chatUnavailable,
      reports,
      reportsUnavailable,
      details,
      positionOfSeq: (seq: number) => {
        let low = 0;
        let high = rows.length;
        while (low < high) {
          const middle = (low + high) >> 1;
          if (rows[middle].seq <= seq) low = middle + 1;
          else high = middle;
        }
        return low > 0 ? rows[low - 1].position : -1;
      },
    };
  };
  let data = build();

  // What the canvas is: the manifest's record, or for a session that has not
  // written one yet, the database's.
  const canvas = manifest?.canvas ?? (details ? { width: details.session.width, height: details.session.height } : null);

  let replay: ReplayHandle | null = null;
  /** The message somebody chose, for the address; see `Seek`. */
  let chosenSeq: number | null = null;
  /** Where the canvas stands and whether it is playing, as last reported. */
  let position = -1;
  let playing = false;
  const seek: Seek = canvas
    ? (position: number, seq?: number) => {
        if (!replay) return;
        // Paused before the choice is recorded, not after: pausing reports
        // where the canvas still is, and a choice already made would be
        // dropped as not matching it.
        if (playing) replay.pause();
        chosenSeq = seq ?? null;
        void replay.seek(position);
      }
    : undefined;

  let mounted: PainterHandle | null = null;
  const people = peoplePanel((hidden) => mounted?.setHiddenParticipants(hidden));
  const log = logPanel(seek);
  const panels: { key: string; label: string; panel: Panel }[] = [
    { key: "log", label: "Log", panel: log },
    { key: "chat", label: "Chat", panel: chatPanel(seek) },
    { key: "people", label: "People", panel: people },
    { key: "reports", label: "Reports", panel: reportsPanel(seek) },
  ];

  const stage = el("div", "replay-stage");
  const main = el("div", "replay-main");
  const canvasHost = el("div", "replay-canvas");
  const controls = el("div", "replay-controls");
  main.append(canvasHost, controls);
  const side = el("aside", "ds-card inspect-side");
  const tabs = el("nav", "ds-segmented inspect-tabs");
  const tabBar = el("div", "inspect-tab-bar");
  tabBar.appendChild(tabs);
  side.appendChild(tabBar);
  stage.append(main, side);
  host.appendChild(stage);

  let tab = "log";
  let hashSeq: number | null = null;
  const buttons = new Map<string, HTMLButtonElement>();
  const select = (key: string) => {
    tab = key;
    for (const { key: other, panel } of panels) {
      const on = other === key;
      panel.root.style.display = on ? "" : "none";
      buttons.get(other)?.setAttribute("aria-pressed", on ? "true" : "false");
      if (on) panel.shown?.();
    }
    writeHash(tab, hashSeq);
  };
  const label = () => {
    for (const { key, label: name, panel } of panels) {
      const count = panel.count();
      const button = buttons.get(key);
      if (button) button.textContent = count > 0 ? `${name} ${count}` : name;
    }
  };
  for (const { key, panel } of panels) {
    const button = el("button", "inspect-tab");
    button.type = "button";
    button.addEventListener("click", () => select(key));
    buttons.set(key, button);
    tabs.appendChild(button);
    side.appendChild(panel.root);
    panel.setData(data);
  }
  label();
  const asked = readHash();
  const has = (key: string) => (panels.find((entry) => entry.key === key)?.panel.count() ?? 0) > 0;
  // Reports first when there are any: somebody who filed one is why this page
  // is usually open.
  select(
    asked.tab && panels.some((entry) => entry.key === asked.tab)
      ? asked.tab
      : has("reports")
        ? "reports"
        : has("chat")
          ? "chat"
          : "log",
  );

  const update = (position: number, playing: boolean) => {
    for (const { panel } of panels) panel.update?.(position, playing);
  };

  if (!canvas) {
    canvasHost.classList.add("replay-canvas-missing");
    canvasHost.textContent = logProblem ?? "No recording for this session.";
    controls.style.display = "none";
    status.textContent = "";
    // Everything said and reported is at its end state: there is no drawing
    // to have reached any point of.
    update(Number.MAX_SAFE_INTEGER, false);
    return;
  }

  const newPainter = async (): Promise<PainterHandle> => {
    // Unmounted before the host is cleared, not after: `unmount` releases
    // listeners and framework roots by taking its own nodes out, and emptying
    // the host first leaves it removing children that are no longer there.
    mounted?.unmount();
    mounted = null;
    canvasHost.textContent = "";
    const painter = mount(canvasHost, {
      width: canvas.width,
      height: canvas.height,
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
      Array.from(names.entries()).map(([id, name]) => ({ actorId: String(id), name })),
    );
    // A seek backwards starts from a fresh painter, and whoever was hidden
    // should stay hidden through it.
    painter.setHiddenParticipants(people.hidden());
    mounted = painter;
    return painter;
  };

  const painter = await newPainter();
  // As wide as the canvas and no wider, with room for the controls: left to
  // size itself, the scrubber's flex basis grows the column until the side
  // panel has nowhere to go but underneath.
  main.style.width = `${Math.max(canvasHost.offsetWidth, 420)}px`;

  const playButton = el("button", "ds-button ds-button-primary ds-button-small replay-play", "Play");
  const restartButton = el("button", "ds-button ds-button-small", "Restart");
  const stepBackButton = el("button", "ds-button ds-button-small", "◀");
  stepBackButton.title = "One mark back (←)";
  const stepButton = el("button", "ds-button ds-button-small", "▶");
  stepButton.title = "One mark forward (→)";
  const endButton = el("button", "ds-button ds-button-small", "Jump to end");
  const speedButton = el("button", "ds-button ds-button-small", "1×");
  const scrub = el("div", "replay-scrub");
  const scrubber = el("input", "ds-range replay-scrubber");
  scrubber.type = "range";
  scrubber.min = "-1";
  scrubber.step = "1";
  const markerStrip = el("div", "replay-markers");
  scrub.append(scrubber, markerStrip);
  const readout = el("span", "replay-readout");
  const liveLabel = el("label", "replay-live");
  const followLive = el("input", "ds-check");
  followLive.type = "checkbox";
  followLive.checked = true;
  liveLabel.append(followLive, document.createTextNode(" Follow live"));
  liveLabel.title = "Fetch what is drawn and said as it happens, and stay at the end";
  liveLabel.style.display = live ? "" : "none";

  controls.append(
    playButton,
    restartButton,
    stepBackButton,
    stepButton,
    endButton,
    speedButton,
    liveLabel,
    scrub,
    readout,
  );

  let speedIndex = 0;
  let drawn = drawableEntries(entries).map((held) => held.entry);

  replay = createReplay({
    painter,
    entries,
    remount: newPainter,
    onProgress: (index, nowPlaying) => {
      position = index;
      playing = nowPlaying;
      playButton.textContent = nowPlaying ? "Pause" : "Play";
      scrubber.value = String(index);
      const entry = index >= 0 ? drawn[index] : undefined;
      readout.textContent =
        `${index + 1} / ${replay?.length ?? 0}` +
        (entry ? `  ·  seq ${entry.seq}  ·  ${new Date(entry.at).toISOString()}` : "");
      update(index, nowPlaying);
      if (!nowPlaying) {
        // The chosen message when it still stands for this canvas, and the
        // mark that made it otherwise.
        if (chosenSeq !== null && data.positionOfSeq(chosenSeq) !== index) chosenSeq = null;
        hashSeq = chosenSeq ?? (entry ? entry.seq : null);
        writeHash(tab, hashSeq);
      }
    },
  });
  const player = replay;

  const drawMarkers = () => {
    markerStrip.textContent = "";
    const span = Math.max(1, player.length);
    const found = markers({
      rows: data.rows,
      drawTimes: data.drawTimes,
      chat: data.chat,
      reports: data.reports
        .map((filed) => ({ at: filedAt(filed), label: `${filed.filed_by ?? "someone"}: ${filed.report.reason ?? "report"}` }))
        .filter((report) => Number.isFinite(report.at)),
    });
    for (const marker of found) {
      const tick = el("button", `replay-marker replay-marker-${marker.kind}`);
      tick.type = "button";
      tick.title = marker.label;
      tick.style.left = `${((marker.position + 1) / span) * 100}%`;
      tick.addEventListener("click", () => seek?.(marker.position));
      markerStrip.appendChild(tick);
    }
  };
  const sized = () => {
    scrubber.max = String(player.length - 1);
    drawMarkers();
  };
  sized();
  scrubber.value = "-1";

  const togglePlay = () => {
    if (playButton.textContent === "Play") player.play();
    else player.pause();
  };
  const step = (by: number) => seek?.(position + by);
  playButton.addEventListener("click", togglePlay);
  restartButton.addEventListener("click", () => seek?.(-1));
  stepBackButton.addEventListener("click", () => step(-1));
  stepButton.addEventListener("click", () => step(1));
  endButton.addEventListener("click", () => seek?.(player.length - 1));
  speedButton.addEventListener("click", () => {
    speedIndex = (speedIndex + 1) % SPEEDS.length;
    player.setSpeed(SPEEDS[speedIndex]);
    speedButton.textContent = `${SPEEDS[speedIndex]}×`;
  });
  // On release rather than on drag: a seek backwards rebuilds the canvas from
  // the first message, and doing that for every pixel of a drag would be a
  // hundred rebuilds nobody asked for.
  scrubber.addEventListener("change", () => seek?.(Number(scrubber.value)));
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

  const describe = () => {
    const partial = manifest !== null && !isRenderable(manifest);
    const who = Array.from(names.entries())
      .map(([id, name]) => `${name} (${id})`)
      .join(", ");
    status.textContent =
      `${player.length} drawing messages of ${entries.length} recorded` +
      `  ·  ${canvas.width}×${canvas.height}` +
      (who ? `  ·  ${who}` : "") +
      (manifest && !manifest.sealed && !live ? "  ·  not sealed" : "") +
      (entries.length === 0 && logProblem ? `  ·  ${logProblem}` : "") +
      (partial
        ? `  ·  INCOMPLETE: this recording starts at sequence ${manifest.recording.first_seq}, so everything before it is missing`
        : "");
    // Said loudly, because a fragment looks exactly like a finished picture.
    status.className = partial ? "ds-notice ds-notice-warning replay-status" : "ds-help replay-status";
  };
  describe();
  readout.textContent = `0 / ${player.length}`;
  update(-1, false);

  // Whether the recording plays back to what was saved: checked by itself on
  // every visit to a finished session, and kept for the session list.
  if (!live && manifest && entries.length > 0) {
    const checkHost = el("div", "inspect-check");
    main.appendChild(checkHost);
    void checkReplay({
      host: checkHost,
      base,
      manifest,
      entries,
      details,
      chatLines: chatLoaded.ok ? chatLoaded.value.length : 0,
      reports: reportsLoaded.ok ? reportsLoaded.value.length : 0,
    });
  }

  // A link that named a moment opens on it.
  if (asked.seq !== null) {
    const seq = asked.seq;
    seek?.(data.positionOfSeq(seq), seq);
    log.choose(seq);
  }

  if (!live) return;

  // Following a live session: ask what is new, add it to everything, and if
  // the canvas was at the end, keep it there.
  let lastSeq = entries.length > 0 ? entries[entries.length - 1].seq : 0;
  const poll = async () => {
    const [tailLoaded, chatNow, reportsNow, detailsNow] = await Promise.all([
      load(`${base}/archive/tail?after=${lastSeq}`, async (response) => new Uint8Array(await response.arrayBuffer())),
      load<ArchivedChat[]>(`${base}/chat`, (response) => response.json()),
      load<FiledReport[]>(`${base}/diagnostics`, (response) => response.json()),
      load<SessionDetails>(`${base}/details`, (response) => response.json()),
    ]);
    const namesBefore = Array.from(names.entries()).join();
    if (detailsNow.ok) {
      details = detailsNow.value;
      head.setDetails(details);
      learnNames();
      if (details.session.ended_at) live = false;
    }
    // The tail can repeat what is already here -- the chunk the last sequence
    // fell inside, or a message caught between the buffer and a flush -- so
    // only what is past the end is new, and each sequence once.
    const seen = new Set<number>();
    const fresh = (tailLoaded.ok ? entriesOf(tailLoaded.value) ?? [] : [])
      .filter((entry) => entry.seq > lastSeq && !seen.has(entry.seq) && (seen.add(entry.seq), true))
      .sort((a, b) => a.seq - b.seq);
    const moreChat = chatNow.ok && chatNow.value.length !== chat.length;
    const moreReports = reportsNow.ok && reportsNow.value.length !== reports.length;
    const renamed = Array.from(names.entries()).join() !== namesBefore;
    if (fresh.length === 0 && !moreChat && !moreReports && !renamed && detailsNow.ok) {
      // Details alone can change what the people tab says about who is in
      // the room.
      data = { ...data, details };
      people.setData(data);
      return;
    }

    const wasAtEnd = !playing && position === player.length - 1;
    if (fresh.length > 0) {
      entries = entries.concat(fresh);
      lastSeq = fresh[fresh.length - 1].seq;
      player.append(fresh);
      drawn = drawableEntries(entries).map((held) => held.entry);
    }
    if (chatNow.ok) {
      chat = chatNow.value;
      chatUnavailable = undefined;
    }
    if (reportsNow.ok) {
      reports = reportsNow.value;
      reportsUnavailable = undefined;
    }
    if (renamed) {
      mounted?.setParticipants(Array.from(names.entries()).map(([id, name]) => ({ actorId: String(id), name })));
    }
    data = build();
    for (const { panel } of panels) panel.setData(data);
    label();
    sized();
    describe();
    if (fresh.length > 0 && wasAtEnd && followLive.checked) {
      chosenSeq = null;
      await player.seek(player.length - 1);
    } else {
      // Nothing moved, but the readout's count and the dimming did.
      update(position, playing);
      readout.textContent = `${position + 1} / ${player.length}`;
    }
  };

  const schedule = () => {
    window.setTimeout(async () => {
      // Not while nobody is looking, and not once the session is over: its
      // last answer is its final one.
      if (followLive.checked && !document.hidden) {
        try {
          await poll();
        } catch (error) {
          console.error("Could not follow the session", error);
        }
      }
      if (live) schedule();
      else liveLabel.style.display = "none";
    }, LIVE_POLL_MS);
  };
  schedule();
}
