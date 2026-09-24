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

const icon = (name: string, path: string) =>
  `<svg class="neo-cucumber-replay-icon neo-cucumber-replay-${name}-icon" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="${path}"/></svg>`;

/**
 * The controls as a page renders them before this script has arrived:
 * everything disabled and nothing to read, because the words are in this
 * script's catalog and not the server's. Rewind, Play and Skip to end are
 * one pill, as a player's transport is, and the speeds another. The three
 * are Material Symbols (skip-previous, play-arrow, pause, skip-next), inline
 * as the site's toolbar draws them: the viewer has no React for
 * ../components/Icon, and the page has to draw them before any script. So the
 * row is one width in every language, and one height; the speeds say the same
 * thing everywhere.
 *
 * templates/replay_controls.jinja is this, written out; embed.browser.test.ts
 * holds the two to each other. The seek bar starts at its end, the drawing
 * being finished, and is fine enough to let go of anywhere until the step
 * count is known.
 */
export function controlsMarkup(): string {
  const button = (control: string, icons: string) =>
    `<button class="neo-cucumber-replay-button" type="button" data-replay-control="${control}" disabled>${icons}</button>`;
  const speeds = SPEEDS.map(
    (speed, index) =>
      `<button class="neo-cucumber-replay-speed" type="button" aria-pressed="${index === DEFAULT_SPEED_INDEX}" disabled>${speed.label}</button>`
  ).join("");
  return (
    '<div class="neo-cucumber-replay-controls">' +
    '<input class="neo-cucumber-replay-seek" type="range" min="0" max="1000" value="1000" disabled>' +
    '<div class="neo-cucumber-replay-buttons">' +
    '<div class="neo-cucumber-replay-transport ds-segmented">' +
    button("rewind", icon("rewind", "M5.5 18V6h2v12zm13 0l-9-6l9-6z")) +
    button("play", icon("play", "M8 19V5l11 7z") + icon("pause", "M14 19V5h4v14zm-8 0V5h4v14z")) +
    button("skip", icon("skip", "M16.5 18V6h2v12zm-11 0V6l9 6z")) +
    "</div>" +
    `<div class="neo-cucumber-replay-speeds ds-segmented">${speeds}</div>` +
    "</div></div>"
  );
}

function buildControls(): HTMLElement {
  const holder = document.createElement("div");
  holder.innerHTML = controlsMarkup();
  return holder.firstChild as HTMLElement;
}

/**
 * Renders a replay viewer into `container`. Styling hangs off `neo-cucumber-replay-*`
 * class names so the host page keeps control of the look.
 *
 * A container that already holds the controls -- a page that rendered them
 * with the drawing, so nothing moves when this arrives -- keeps them and its
 * drawing, which becomes the poster; the controls are switched on and given
 * their words. Otherwise both are made here.
 */
export function mount(
  container: HTMLElement,
  options: MountOptions
): MountedViewer {
  const labels = labelsFor(options.lang);
  container.classList.add("neo-cucumber-replay");

  const existing = container.querySelector<HTMLElement>(".neo-cucumber-replay-controls");
  const rendered = existing !== null;
  const controls = existing ?? buildControls();
  if (!rendered) container.textContent = "";

  const status = el("p", "neo-cucumber-replay-status", labels.loading);

  const canvas = el("canvas", "neo-cucumber-replay-canvas");
  canvas.width = options.width ?? 300;
  canvas.height = options.height ?? 300;

  const control = <T extends Element>(selector: string) => {
    const found = controls.querySelector<T>(selector);
    if (!found) throw new Error(`replay controls have no ${selector}`);
    return found;
  };
  const seek = control<HTMLInputElement>(".neo-cucumber-replay-seek");
  const playButton = control<HTMLButtonElement>('[data-replay-control="play"]');
  const rewindButton = control<HTMLButtonElement>('[data-replay-control="rewind"]');
  const skipButton = control<HTMLButtonElement>('[data-replay-control="skip"]');
  const speedButtons = Array.from(
    controls.querySelectorAll<HTMLButtonElement>(".neo-cucumber-replay-speed")
  );

  const name = (node: Element, label: string) => {
    node.setAttribute("aria-label", label);
    node.setAttribute("title", label);
  };
  seek.setAttribute("aria-label", labels.seek);
  name(rewindButton, labels.rewind);
  name(skipButton, labels.skip);
  // Play and Pause are two icons of one width in one button, so a press
  // moves nothing after it.
  const showPlaying = (playing: boolean) => {
    if (playing) playButton.setAttribute("data-playing", "");
    else playButton.removeAttribute("data-playing");
    name(playButton, playing ? labels.pause : labels.play);
  };
  showPlaying(false);
  for (const node of [seek, playButton, rewindButton, skipButton, ...speedButtons]) {
    node.disabled = false;
  }

  let player: ReplayPlayer | null = null;
  let loading: Promise<ReplayPlayer | null> | null = null;
  let disposed = false;
  let speedIndex = DEFAULT_SPEED_INDEX;

  const onState = (state: PlayerState) => {
    seek.max = String(Math.max(1, state.total));
    seek.value = String(state.position);
    showPlaying(state.playing);
  };

  // With a poster, the drawing stands where the canvas will and the controls
  // are under it; the page's own drawing, when it rendered one.
  let poster: HTMLImageElement | null = null;
  if (options.poster) {
    poster = rendered ? container.querySelector("img") : null;
    if (!poster) {
      poster = el("img", "");
      poster.src = options.poster;
      poster.alt = "";
      poster.draggable = false;
      poster.width = options.width ?? 300;
      poster.height = options.height ?? 300;
      container.insertBefore(poster, rendered ? controls : null);
    }
    poster.classList.add("neo-cucumber-replay-poster");
    if (!rendered) container.appendChild(controls);
  } else {
    // Loading at once: the words say so until the canvas is ready.
    container.textContent = "";
    container.appendChild(status);
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
