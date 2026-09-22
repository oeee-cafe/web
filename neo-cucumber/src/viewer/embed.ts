/**
 * Standalone replay viewer, mounted straight into a server-rendered page the
 * way neo.js was.
 *
 * Deliberately free of React and the rest of the collaborative app: NeoPainter,
 * NeoReplay and ReplayPlayer are all plain TypeScript, so a page that only
 * wants to watch a drawing does not have to download a painter and a WebSocket
 * client to do it.
 *
 * It does carry Lingui's runtime, because its handful of labels belong in the
 * catalogs with every other string here rather than in a table of their own.
 * That costs about 2kB gzipped, against a second place to translate -- which is
 * how a string comes to be forgotten. Its catalog is its own, though; see
 * ./i18n.
 */
import { decodePCH } from "../neo/NeoReplay";
import { labelsFor } from "./i18n";
import {
  DEFAULT_SPEED_INDEX,
  ReplayPlayer,
  SPEEDS,
  type PlayerState,
} from "./ReplayPlayer";

export interface MountOptions {
  /** URL of the .pch file. */
  replay: string;
  /** Fallback canvas size, used until the file's own header is read. */
  width?: number;
  height?: number;
  /** BCP-47 language tag; defaults to the document's. */
  lang?: string;
  /**
   * The finished drawing, shown until the replay is wanted. With a poster
   * the controls are there from the start and nothing is fetched until one
   * of them is used: a page that shows a drawing with its replay under it
   * does not download every recording for everyone who only looks. Without
   * one, the replay loads and plays at once, as it always has.
   */
  poster?: string;
}

export interface MountedViewer {
  /** Loads the replay if it has not been, and plays it from the start. */
  play(): void;
  dispose(): void;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/**
 * Renders a replay viewer into `container`. Styling hangs off `neo-cucumber-replay-*`
 * class names so the host page keeps control of the look.
 */
export function mount(
  container: HTMLElement,
  options: MountOptions
): MountedViewer {
  const labels = labelsFor(options.lang);
  container.classList.add("neo-cucumber-replay");
  container.textContent = "";

  const status = el("p", "neo-cucumber-replay-status", labels.loading);
  container.appendChild(status);

  const canvas = el("canvas", "neo-cucumber-replay-canvas");
  canvas.width = options.width ?? 300;
  canvas.height = options.height ?? 300;

  const controls = el("div", "neo-cucumber-replay-controls");
  const seek = el("input", "neo-cucumber-replay-seek") as HTMLInputElement;
  seek.type = "range";
  seek.min = "0";
  seek.value = "0";
  seek.setAttribute("aria-label", labels.seek);

  const buttons = el("div", "neo-cucumber-replay-buttons");
  // Play and Pause are both in the button, the one not in use hidden but
  // still taking room, so the button is as wide as the wider of them. With
  // only the current one in it, every press moved the buttons after it --
  // and on the post page, where the controls can set the stage's width,
  // resized the drawing too.
  const playButton = el("button", "neo-cucumber-replay-button");
  playButton.type = "button";
  const playLabel = el("span", "neo-cucumber-replay-label", labels.play);
  const pauseLabel = el("span", "neo-cucumber-replay-label", labels.pause);
  playButton.append(playLabel, pauseLabel);
  const showPlaying = (playing: boolean) => {
    playLabel.setAttribute("aria-hidden", String(playing));
    pauseLabel.setAttribute("aria-hidden", String(!playing));
  };
  showPlaying(false);
  const rewindButton = el("button", "neo-cucumber-replay-button", labels.rewind);
  rewindButton.type = "button";
  const skipButton = el("button", "neo-cucumber-replay-button", labels.skip);
  skipButton.type = "button";

  const speeds = el("div", "neo-cucumber-replay-speeds");
  const speedButtons = SPEEDS.map((speed, index) => {
    const button = el("button", "neo-cucumber-replay-speed", speed.label);
    button.type = "button";
    button.setAttribute("aria-pressed", String(index === DEFAULT_SPEED_INDEX));
    speeds.appendChild(button);
    return button;
  });

  buttons.append(playButton, rewindButton, skipButton, speeds);
  controls.append(seek, buttons);

  let player: ReplayPlayer | null = null;
  let loading: Promise<ReplayPlayer | null> | null = null;
  let disposed = false;
  let speedIndex = DEFAULT_SPEED_INDEX;

  const onState = (state: PlayerState) => {
    seek.max = String(Math.max(1, state.total));
    seek.value = String(state.position);
    showPlaying(state.playing);
  };

  // With a poster, the drawing stands where the canvas will, the controls
  // under it, and the seek bar at its end -- the drawing is finished.
  let poster: HTMLImageElement | null = null;
  if (options.poster) {
    poster = el("img", "neo-cucumber-replay-poster");
    poster.src = options.poster;
    poster.alt = "";
    poster.draggable = false;
    poster.width = options.width ?? 300;
    poster.height = options.height ?? 300;
    // Fine enough to let go of anywhere, until the step count is known.
    seek.max = "1000";
    seek.value = "1000";
    status.remove();
    container.append(poster, controls);
  }

  const load = (): Promise<ReplayPlayer | null> => {
    if (loading) return loading;
    if (poster) container.classList.add("is-loading");
    loading = (async () => {
      try {
        const response = await fetch(options.replay);
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const decoded = decodePCH(await response.arrayBuffer());
        if (!decoded) throw new Error("not a PCH file");
        if (disposed) return null;

        canvas.width = decoded.width;
        canvas.height = decoded.height;
        canvas.style.width = `${decoded.width}px`;
        canvas.style.height = `${decoded.height}px`;

        const ctx = canvas.getContext("2d");
        if (!ctx) throw new Error("no 2d context");

        status.remove();
        container.classList.remove("is-loading");
        if (poster) {
          poster.replaceWith(canvas);
          poster = null;
        } else {
          container.append(canvas, controls);
        }

        player = new ReplayPlayer(
          decoded.items,
          decoded.width,
          decoded.height,
          ctx,
          onState
        );
        player.setSpeed(SPEEDS[speedIndex].rate);
        return player;
      } catch (error) {
        if (disposed) return null;
        container.classList.remove("is-loading");
        status.textContent = `${labels.failed} (${
          error instanceof Error ? error.message : String(error)
        })`;
        container.appendChild(status);
        return null;
      }
    })();
    return loading;
  };

  const play = () => {
    void load().then((loaded) => {
      if (!loaded) return;
      // Loaded paused, or finished: from the start, as NEO plays.
      if (!loaded.getState().playing) loaded.play();
    });
  };

  if (!options.poster) play();

  playButton.addEventListener("click", () => {
    if (!player) return play();
    if (player.getState().playing) player.pause();
    else player.play();
  });
  rewindButton.addEventListener("click", () => {
    void load().then((loaded) => loaded?.rewind());
  });
  skipButton.addEventListener("click", () => {
    void load().then((loaded) => loaded?.skipToEnd());
  });
  seek.addEventListener("input", () => {
    if (player) return void player.seekTo(Number(seek.value));
    // Before the file is here the bar measures the drawing, not its steps:
    // where it was let go is a fraction of the way through.
    const fraction = Number(seek.value) / Math.max(1, Number(seek.max));
    void load().then((loaded) => {
      if (loaded) void loaded.seekTo(fraction * loaded.getState().total);
    });
  });

  speedButtons.forEach((button, index) => {
    button.addEventListener("click", () => {
      speedIndex = index;
      player?.setSpeed(SPEEDS[index].rate);
      speedButtons.forEach((other, i) =>
        other.setAttribute("aria-pressed", String(i === index))
      );
    });
  });

  return {
    play,
    dispose() {
      disposed = true;
      player?.dispose();
      player = null;
      container.textContent = "";
    },
  };
}
