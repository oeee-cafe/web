/**
 * Does the recording play back to what was saved?
 *
 * A replay that renders differently from the canvas it was recorded on is the
 * worst failure this codebase can produce, and until now the only evidence of
 * one was somebody noticing. The owner saves a session with the painter's
 * `exportPng`, after waiting until every message up to the room's last has
 * been applied; so a complete recording, applied to a fresh painter and
 * exported the same way, has to come out as the same picture exactly.
 *
 * Run on a painter of its own, off the page, so the canvas somebody is
 * looking at stays where they put it. What it finds is kept for the session
 * list, which is where a mismatch nobody was looking for gets seen.
 */

import { mount } from "neo-cucumber";
import { isRenderable, type ArchiveManifest, type ArchivedEntry } from "./archiveLog";
import { compare, type Pixels } from "./compare";
import { el, type SessionDetails } from "./dom";
import { drawableEntries } from "./player";

type Outcome = "match" | "differs" | "incomplete" | "unavailable";

type Result = {
  outcome: Outcome;
  differing_pixels: number | null;
  total_pixels: number | null;
  seq: number | null;
  note: string | null;
};

async function pixelsOf(blob: Blob): Promise<{ pixels: Pixels; canvas: HTMLCanvasElement }> {
  const bitmap = await createImageBitmap(blob);
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("No 2D context to read the picture with");
  context.drawImage(bitmap, 0, 0);
  const data = context.getImageData(0, 0, bitmap.width, bitmap.height);
  return { pixels: { width: data.width, height: data.height, data: data.data }, canvas };
}

/** The recording applied to a painter nobody sees, and exported as saved. */
async function replayToEnd(manifest: ArchiveManifest, entries: ArchivedEntry[]): Promise<Blob> {
  const host = el("div", "inspect-check-stage");
  host.setAttribute("aria-hidden", "true");
  document.body.appendChild(host);
  const painter = mount(host, {
    width: manifest.canvas.width,
    height: manifest.canvas.height,
    mode: { kind: "standard" },
    controls: { kind: "none" },
    recordReplay: false,
    synchronization: { actorId: "replay-check", onOperation: () => {} },
  });
  try {
    await painter.ready;
    painter.setInteractionEnabled(false);
    for (const held of drawableEntries(entries)) {
      await painter.applyCanonicalOperation(held.operation);
    }
    return await painter.exportPng();
  } finally {
    painter.unmount();
    host.remove();
  }
}

function figure(caption: string, canvas: HTMLCanvasElement): HTMLElement {
  const node = el("figure", "inspect-check-figure");
  node.append(canvas, el("figcaption", "ds-help", caption));
  return node;
}

/**
 * Runs the check, says what it found in `host`, and keeps it. Resolves once
 * it has; never throws -- a check that could not run says so instead.
 */
export async function checkReplay(options: {
  host: HTMLElement;
  base: string;
  manifest: ArchiveManifest;
  entries: ArchivedEntry[];
  details: SessionDetails | null;
  chatLines: number;
  reports: number;
}): Promise<void> {
  const { host, base, manifest, entries, details } = options;
  const last = entries.length > 0 ? entries[entries.length - 1].seq : null;
  /** What was found, in a line of its own: a notice is a row, and the
   * pictures go below it rather than beside. */
  const say = (className: string, text: string) => {
    host.textContent = "";
    const line = el("p", `inspect-check-says ${className}`);
    line.appendChild(el("span", "ds-notice-body", text));
    host.appendChild(line);
  };

  const keep = (result: Result) =>
    fetch(`${base}/check`, {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        ...result,
        chat_lines: options.chatLines,
        reports: options.reports,
        span: {
          first_seq: manifest.recording.first_seq,
          last_seq: manifest.recording.last_seq,
          messages: manifest.recording.messages,
          sealed: manifest.sealed,
        },
      }),
    }).catch(() => {
      // Noting it is for the list. The page has already said what it found.
    });

  if (!isRenderable(manifest)) {
    const note = `The recording starts at sequence ${manifest.recording.first_seq}, so it cannot be played from nothing.`;
    say("ds-help", `Not checked against the saved post: ${note}`);
    await keep({ outcome: "incomplete", differing_pixels: null, total_pixels: null, seq: last, note });
    return;
  }
  if (!details?.session.saved_post_id) {
    const note = "Never saved as a post, so there is nothing to check the replay against.";
    say("ds-help", note);
    await keep({ outcome: "unavailable", differing_pixels: null, total_pixels: null, seq: last, note });
    return;
  }

  say("ds-help", "Checking the replay against the saved post…");
  try {
    const [replayed, reference] = await Promise.all([
      replayToEnd(manifest, entries),
      fetch(`${base}/reference`, { credentials: "include" }).then((response) => {
        if (!response.ok) throw new Error(`the saved post could not be read (${response.status})`);
        return response.blob();
      }),
    ]);
    const [replay, saved] = await Promise.all([pixelsOf(replayed), pixelsOf(reference)]);
    const result = compare(replay.pixels, saved.pixels);

    if (result.differing === 0) {
      say(
        "ds-notice ds-notice-success",
        `The replay comes out as the saved post, pixel for pixel (${saved.pixels.width}×${saved.pixels.height}, through seq ${last}).`,
      );
      await keep({ outcome: "match", differing_pixels: 0, total_pixels: result.total, seq: last, note: null });
      return;
    }

    const share = ((result.differing / result.total) * 100).toFixed(result.differing * 1000 < result.total ? 2 : 1);
    const note = result.sameSize
      ? `${result.differing} of ${result.total} pixels (${share}%) differ.`
      : `The replay is ${replay.pixels.width}×${replay.pixels.height} and the saved post ${saved.pixels.width}×${saved.pixels.height}.`;
    say("ds-notice ds-notice-error", `The replay does not come out as the saved post. ${note}`);
    // The three side by side: what was saved, what the recording makes, and
    // where they part.
    const details = el("details", "inspect-check-details");
    details.open = true;
    details.appendChild(el("summary", undefined, "Saved post, replay, difference"));
    const row = el("div", "inspect-check-figures");
    row.append(figure("Saved post", saved.canvas), figure("Replay", replay.canvas));
    if (result.sameSize) {
      const diff = document.createElement("canvas");
      diff.width = saved.pixels.width;
      diff.height = saved.pixels.height;
      const context = diff.getContext("2d");
      if (context) {
        // The saved post faintly beneath, so the red reads as a place.
        context.globalAlpha = 0.25;
        context.drawImage(saved.canvas, 0, 0);
        context.globalAlpha = 1;
        const overlay = document.createElement("canvas");
        overlay.width = diff.width;
        overlay.height = diff.height;
        overlay.getContext("2d")?.putImageData(new ImageData(result.mask, diff.width, diff.height), 0, 0);
        context.drawImage(overlay, 0, 0);
      }
      row.appendChild(figure(`Difference: ${result.differing} px`, diff));
    }
    details.appendChild(row);
    host.appendChild(details);
    await keep({
      outcome: "differs",
      differing_pixels: result.differing,
      total_pixels: result.total,
      seq: last,
      note,
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    say("ds-help", `Could not check the replay: ${message}.`);
    // Not kept: a check that failed to run says nothing about the recording.
  }
}
