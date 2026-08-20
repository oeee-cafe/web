/**
 * The replay page: fetch a recording, mount a painter, drive it.
 *
 * Staff-only by where it gets its data. Both endpoints are behind the admin
 * extractor, so a page served to anybody else fetches two 403s and says so --
 * the viewer holds no permission of its own and cannot be made to.
 */

import { mount, type PainterHandle } from "neo-cucumber";
import { decodeArchive, isRenderable, type ArchiveManifest } from "./archiveLog";
import { createReplay, type ReplayHandle } from "./player";

const SPEEDS = [1, 2, 4, 16];

/** The one participant a replay does not have. Named so it cannot collide
 * with a session id, which are the small integers the room assigns. */
const VIEWER_ACTOR = "replay-viewer";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

export async function mountReplay(host: HTMLElement, session: string): Promise<void> {
  const status = el("p", "replay-status", "Loading the recording…");
  host.appendChild(status);

  const [manifestResponse, logResponse] = await Promise.all([
    fetch(`/admin/collaborative-sessions/${session}/manifest`, { credentials: "include" }),
    fetch(`/admin/collaborative-sessions/${session}/archive`, { credentials: "include" }),
  ]);

  if (!manifestResponse.ok || !logResponse.ok) {
    status.textContent =
      manifestResponse.status === 404 || logResponse.status === 404
        ? "No recording for this session."
        : `Could not read the recording (${manifestResponse.status}/${logResponse.status}).`;
    return;
  }

  const manifest: ArchiveManifest = await manifestResponse.json();
  const entries = decodeArchive(new Uint8Array(await logResponse.arrayBuffer()));
  if (!entries) {
    status.textContent = "That file is not a recording.";
    return;
  }

  // A recording that does not start at the room's first message is missing
  // whatever a checkpoint squashed, and drawing it would present a fragment as
  // the finished picture.
  const partial = !isRenderable(manifest);

  const canvasHost = el("div", "replay-canvas");
  const controls = el("div", "replay-controls");
  host.appendChild(canvasHost);
  host.appendChild(controls);

  let mounted: PainterHandle | null = null;
  const newPainter = async (): Promise<PainterHandle> => {
    // Unmounted before the host is cleared, not after: `unmount` releases
    // listeners and framework roots by taking its own nodes out, and emptying
    // the host first leaves it removing children that are no longer there.
    mounted?.unmount();
    mounted = null;
    canvasHost.textContent = "";
    const painter = mount(canvasHost, {
      width: manifest.width,
      height: manifest.height,
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

  const playButton = el("button", "replay-button", "Play");
  const restartButton = el("button", "replay-button", "Restart");
  const endButton = el("button", "replay-button", "Jump to end");
  const speedButton = el("button", "replay-button", "1×");
  const scrubber = el("input", "replay-scrubber");
  scrubber.type = "range";
  scrubber.min = "-1";
  scrubber.step = "1";
  const readout = el("span", "replay-readout");

  controls.append(playButton, restartButton, endButton, speedButton, scrubber, readout);

  let speedIndex = 0;

  const replay: ReplayHandle = createReplay({
    painter,
    entries,
    remount: newPainter,
    onProgress: (index, playing) => {
      playButton.textContent = playing ? "Pause" : "Play";
      scrubber.value = String(index);
      const shown = index + 1;
      const entry = entries[Math.max(0, index)];
      readout.textContent =
        `${shown} / ${replay.length}` +
        (entry ? `  ·  seq ${entry.seq}  ·  ${new Date(entry.at).toISOString()}` : "");
    },
  });

  scrubber.max = String(replay.length - 1);
  scrubber.value = "-1";

  playButton.addEventListener("click", () => {
    if (playButton.textContent === "Play") replay.play();
    else replay.pause();
  });
  restartButton.addEventListener("click", () => {
    replay.pause();
    void replay.seek(-1);
  });
  endButton.addEventListener("click", () => {
    replay.pause();
    void replay.seek(replay.length - 1);
  });
  speedButton.addEventListener("click", () => {
    speedIndex = (speedIndex + 1) % SPEEDS.length;
    replay.setSpeed(SPEEDS[speedIndex]);
    speedButton.textContent = `${SPEEDS[speedIndex]}×`;
  });
  // On release rather than on drag: a seek backwards rebuilds the canvas from
  // the first message, and doing that for every pixel of a drag would be a
  // hundred rebuilds nobody asked for.
  scrubber.addEventListener("change", () => {
    replay.pause();
    void replay.seek(Number(scrubber.value));
  });

  const participants = manifest.participants
    .map((participant) => `${participant.login_name} (${participant.session_id})`)
    .join(", ");
  status.textContent =
    `${replay.length} drawing messages of ${entries.length} recorded` +
    `  ·  ${manifest.width}×${manifest.height}` +
    (participants ? `  ·  ${participants}` : "") +
    (manifest.sealed ? "" : "  ·  not sealed") +
    (partial
      ? `  ·  INCOMPLETE: this recording starts at sequence ${manifest.first_seq}, so everything before it is missing`
      : "");
  if (partial) status.classList.add("replay-incomplete");
}
