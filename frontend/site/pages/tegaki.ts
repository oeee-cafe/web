// Tegaki's pages: drawing with it (draw_post_tegaki.jinja) and playing back
// what was drawn (post_replay_view_tgkr.jinja). Tegaki itself is the scripts
// under tegaki/js, loaded by the page before this runs, and it is a global.

interface Recorder {
  stop(): void;
  toBlob(): Blob;
}

interface TegakiGlobal {
  onTabClose: (event: BeforeUnloadEvent) => void;
  onCloseViewerClick: () => void;
  updatePosOffset(): void;
  flatten(): HTMLCanvasElement;
  replayRecorder: Recorder;
  startTimeStamp: number;
  resume(): void;
  open(options: Record<string, unknown>): void;
}

type TegakiWindow = Window &
  typeof globalThis & {
    Tegaki: TegakiGlobal;
    TegakiHistory: { undoStack: unknown[] };
    TegakiCursor: { updateCanvasSize(): void };
  };

const page = window as unknown as TegakiWindow;

// Tegaki fills the viewport from the top. It starts under the toolbar
// instead, as the painter does, so the page has the same bar as every other;
// measured, since the bar is taller under a notch. Tegaki keeps the canvas's
// place on screen to turn a pointer into a pixel, and measured it before this
// moved it -- on open, and on a resize, whose own handler runs before this
// one. Unmeasured, every stroke lands a toolbar's height away from the pen.
function fitUnderToolbar(cursorToo: boolean): () => void {
  return () => {
    const bar = document.querySelector(".nav-bar");
    const tegaki = document.getElementById("tegaki");
    if (!bar || !tegaki) return;
    const top = Math.round(bar.getBoundingClientRect().bottom);
    tegaki.style.top = top + "px";
    tegaki.style.height = "calc(100% - " + top + "px)";
    page.Tegaki.updatePosOffset();
    if (cursorToo) page.TegakiCursor.updateCanvasSize();
  };
}

/** What the drawing page knows of the drawing it is for. */
export interface TegakiDrawing {
  communityId: string;
  parentPostId: string | null;
  cancelTo: string;
  postError: string;
  width: number;
  height: number;
}

/** Drawing with Tegaki, and posting what was drawn. */
export function tegakiDraw(drawing: TegakiDrawing): void {
  const Tegaki = page.Tegaki;
  let securityCount = 0;

  // Tegaki keeps nothing on the device, so leaving -- by the toolbar, say --
  // loses the drawing, and Tegaki asks first. It asks even of an empty
  // canvas; this asks only once there is something on the undo stack, as the
  // painter answers the apps' wouldLoseWork() (bridge.ts). Set before open(),
  // which hooks it up; Tegaki unhooks it itself when Finish or Cancel takes
  // the page away.
  Tegaki.onTabClose = (event) => {
    if (page.TegakiHistory.undoStack.length === 0) return;
    event.preventDefault();
    event.returnValue = "";
  };

  const fit = fitUnderToolbar(true);

  function onDone(): void {
    Tegaki.replayRecorder.stop();
    const image = Tegaki.flatten();
    const animation = Tegaki.replayRecorder.toBlob();

    const form = new FormData();
    form.append("image", image.toDataURL("image/png"));
    form.append("animation", animation, "blob");
    form.append("community_id", drawing.communityId);
    form.append("paint_duration_ms", String(Math.max(0, Date.now() - Tegaki.startTimeStamp)));
    form.append("security_count", String(securityCount));
    form.append("width", String(image.width));
    form.append("height", String(image.height));
    form.append("tool", "tegaki");
    if (drawing.parentPostId !== null) form.append("parent_post_id", drawing.parentPostId);

    fetch("/draw/finish", { method: "POST", body: form, credentials: "same-origin" })
      .then((response) => {
        if (!response.ok) throw new Error("HTTP " + response.status);
        return response.json();
      })
      .then((data: { post_id: string }) => {
        location.href = "/posts/" + data.post_id + "/publish";
      })
      .catch(() => {
        alert(drawing.postError);
        // Done hid the painter; give the drawing back to keep at.
        Tegaki.resume();
        fit();
      });
  }

  function onCancel(): void {
    location.href = drawing.cancelTo;
  }

  Tegaki.open({
    saveReplay: true,
    onDone,
    onCancel,
    width: drawing.width,
    height: drawing.height,
  });
  fit();
  window.addEventListener("resize", fit);
  // The bar can grow once its font arrives.
  window.addEventListener("load", fit);

  document.getElementById("tegaki")!.addEventListener("pointerdown", () => {
    securityCount++;
  });
}

/** Playing back a Tegaki drawing; closing it goes to the post. */
export function tegakiReplay(postUrl: string, replayUrl: string): void {
  const Tegaki = page.Tegaki;
  const fit = fitUnderToolbar(false);
  Tegaki.onCloseViewerClick = () => {
    window.location.href = postUrl;
  };
  Tegaki.open({
    replayMode: true,
    replayURL: replayUrl,
  });
  fit();
  window.addEventListener("resize", fit);
  // The bar can grow once its font arrives.
  window.addEventListener("load", fit);
}
