// A drawing's stage on its post page (post_view.jinja) and on the page that
// publishes it (post_form.jinja): as large as fits, and on the post page, the
// replay played in place by the viewer bundle (neo-cucumber/src/viewer).

interface ReplayViewer {
  mount(
    box: Element,
    options: { replay: string | null; poster: string | null; width: number; height: number; lang: string | null },
  ): { play(): void };
}

type ViewerWindow = Window & typeof globalThis & { NeoCucumberReplay?: ReplayViewer };

/**
 * As large as fits, on a phone the width of the window. The size is a custom
 * property on the stage, so the image, the replay's poster and its canvas all
 * take it. `crisp` marks the stage drawn hard when it is not shown smaller
 * than its own size, as the publish page asks.
 */
function fitStageWith(stageBox: HTMLElement, crisp: boolean): () => void {
  return () => {
    const width = parseInt(stageBox.getAttribute("data-width")!, 10);
    const height = parseInt(stageBox.getAttribute("data-height")!, 10);
    const room = (stageBox.parentNode!.parentNode as HTMLElement).clientWidth;
    // The stage's height is the window's, less the toolbar, on a wide screen;
    // a phone scrolls, so only the width counts there.
    const tall = window.innerWidth > 600 ? Math.max(window.innerHeight - 190, height) : Infinity;
    // As large as fits, up to four times its own size, and smoothly so: whole
    // steps only, it fell from twice its size to its own the moment twice
    // stopped fitting beside the inspector.
    const shown = Math.min(room, width * 4, (width * tall) / height);
    // Always a length, never auto: an auto-sized replay box takes the width
    // of its controls, which stretched a 300px drawing to ~400 and resized it
    // again whenever Play became Pause.
    stageBox.style.setProperty("--post-stage-shown", shown + "px");
    // In one column the details and comments under it are as wide.
    document.querySelector<HTMLElement>(".post-page")!.style.setProperty("--post-shown", shown + "px");
    // Drawn hard unless it is shown smaller than its own size.
    if (crisp) stageBox.classList.toggle("is-crisp", width <= room);
  };
}

/** Loads the replay viewer, then hands it to `then`. */
export function withReplayViewer(viewerUrl: string, then: (viewer: ReplayViewer) => void): void {
  const script = document.createElement("script");
  script.src = viewerUrl;
  script.onload = () => then((window as ViewerWindow).NeoCucumberReplay!);
  document.head.appendChild(script);
}

/** The post page's stage, and its replay played in place where it has one. */
export function postStage(viewerUrl: string): void {
  const stageBox = document.querySelector<HTMLElement>("[data-post-stage]");
  const fitStage = () => {
    if (!stageBox) return;
    fitStageWith(stageBox, false)();
  };
  fitStage();
  window.addEventListener("resize", fitStage);

  const box = document.getElementById("post-stage-replay");
  if (!box) return;
  withReplayViewer(viewerUrl, (replay) => {
    const drawing = box.querySelector<HTMLImageElement>("img[data-oeee-drawing]");
    const viewer = replay.mount(box, {
      replay: box.getAttribute("data-replay"),
      poster: box.getAttribute("data-poster"),
      width: parseInt(box.getAttribute("data-width")!, 10),
      height: parseInt(box.getAttribute("data-height")!, 10),
      lang: box.getAttribute("data-lang"),
    });
    // The viewer empties the box and draws the drawing again as its poster,
    // so the mark that lets a long press in the iOS app open its menu for the
    // drawing (ds.css, bridge.ts) goes over to the poster, with the drawing's
    // name.
    const poster = box.querySelector<HTMLImageElement>(".neo-cucumber-replay-poster");
    if (drawing && poster) {
      poster.setAttribute("data-oeee-drawing", "");
      poster.alt = drawing.alt;
    }
    // The replay routes send NEO replays here, to be played at once.
    if (location.hash === "#replay") viewer.play();
  });
}

/** The publish page's stage. */
export function publishStage(): void {
  const stageBox = document.querySelector<HTMLElement>("[data-post-stage]")!;
  const fitStage = fitStageWith(stageBox, true);
  fitStage();
  window.addEventListener("resize", fitStage);
}

/** The staff post page's replay (admin/post_detail.jinja). */
export function adminReplay(viewerUrl: string): void {
  const box = document.getElementById("admin-post-replay")!;
  withReplayViewer(viewerUrl, (replay) => {
    replay.mount(box, {
      replay: box.getAttribute("data-replay"),
      poster: box.querySelector("img")!.getAttribute("src"),
      width: parseInt(box.getAttribute("data-width")!, 10),
      height: parseInt(box.getAttribute("data-height")!, 10),
      lang: box.getAttribute("data-lang"),
    });
  });
}
