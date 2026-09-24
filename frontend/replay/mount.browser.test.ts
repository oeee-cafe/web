import { afterEach, describe, expect, it, vi } from "vitest";
import type { PainterOperation } from "neo-cucumber";
import { encodePainterOperation } from "../collaborate/binaryProtocol";
import { mountReplay } from "./mount";

/**
 * The inspector has to stay pressable while it plays.
 *
 * Safari drops a click whose press and release land on different nodes, and
 * the page redraws its controls every frame while playing -- so a control
 * rebuilt, or even re-labelled, under the pointer cannot be pressed there.
 * Pause was exactly that. Chromium forgives it -- it updates a lone text
 * node in place where WebKit replaces it -- so these pin the property that
 * matters instead: while playing, nothing somebody might press is rewritten.
 */

const MAGIC = [0x4f, 0x45, 0x45, 0x45, 0x4c, 0x4f, 0x47, 0x02];
const HISTORY = "0000000000000000000000000000000a";
const MARKS = 120;

function stroke(index: number): PainterOperation {
  return {
    kind: "stroke",
    layer: "foreground",
    brushSize: 2,
    brush: "solid",
    color: { r: 0, g: 0, b: 0, a: 255 },
    points: [
      { x: (index * 7) % 60, y: (index * 5) % 40 },
      { x: ((index * 7) % 60) + 3, y: (index * 5) % 40 },
    ],
    mask: { type: 0, r: 0, g: 0, b: 0 },
  };
}

/** A recording framed as the server writes one, a mark every 50ms. */
function archive(): Uint8Array {
  const bytes: number[] = [...MAGIC, ...HISTORY.match(/../g)!.map((b) => parseInt(b, 16)), 1, 1, 0x61];
  for (let index = 0; index < MARKS; index++) {
    const payload = new Uint8Array(encodePainterOperation(1, stroke(index)));
    const fixed = new Uint8Array(22);
    const view = new DataView(fixed.buffer);
    view.setBigUint64(2, BigInt(index + 1), true);
    view.setBigUint64(10, BigInt(1000 + index * 50), true);
    view.setUint32(18, payload.length, true);
    bytes.push(...fixed, ...payload);
  }
  return Uint8Array.from(bytes);
}

const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200 });

function serve() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      if (url.endsWith("/manifest")) {
        return json({
          format: "oeee-collab-archive",
          version: 2,
          session: "s",
          canvas: { width: 64, height: 48, mode: "standard" },
          started_at: "",
          ended_at: "",
          duration_ms: 0,
          recording: { first_seq: 1, last_seq: MARKS, first_at: 1000, last_at: 1000 + MARKS * 50, messages: MARKS },
          participants: [{ session_id: 1, user_id: "u", login_name: "miro" }],
          sealed: true,
        });
      }
      if (url.endsWith("/archive")) return new Response(archive().buffer as ArrayBuffer, { status: 200 });
      if (url.endsWith("/chat") || url.endsWith("/diagnostics")) return json([]);
      if (url.endsWith("/details")) {
        return json({
          session: {
            id: "s", title: "t", owner_login_name: "miro", width: 64, height: 48,
            max_participants: 8, active_participant_count: 0, total_participant_count: 1,
            is_public: true, community_slug: null, community_name: null, community_visibility: null,
            created_at: "2026-09-24T17:40:00", last_activity: "2026-09-24T17:41:00",
            ended_at: "2026-09-24T08:41:00Z", saved_post_id: null,
          },
          participants: [],
          seats: [],
        });
      }
      if (url.endsWith("/check")) return new Response(null, { status: 204 });
      throw new Error(`unexpected ${url}`);
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.textContent = "";
  history.replaceState(null, "", window.location.pathname);
});

const frames = (count: number) =>
  new Promise<void>((resolve) => {
    let left = count;
    const tick = () => (--left <= 0 ? resolve() : requestAnimationFrame(tick));
    requestAnimationFrame(tick);
  });

async function inspector() {
  serve();
  history.replaceState(null, "", "#log");
  const host = document.createElement("div");
  document.body.appendChild(host);
  await mountReplay(host, "s");
  const play = host.querySelector<HTMLButtonElement>(".replay-play")!;
  return { host, play };
}

describe("pressing things while the inspector plays", () => {
  it("leaves the play button alone while playing", async () => {
    const { host, play } = await inspector();
    play.click();
    await frames(3);
    expect(play.textContent).toBe("Pause");
    const writes: MutationRecord[] = [];
    const watching = new MutationObserver((records) => writes.push(...records));
    watching.observe(play, { childList: true, characterData: true, subtree: true });
    const before = host.querySelector(".replay-readout")!.textContent;
    await frames(10);
    watching.disconnect();
    // Still playing, and the button untouched throughout: a press now
    // releases on what it pressed.
    expect(host.querySelector(".replay-readout")!.textContent).not.toBe(before);
    expect(writes).toHaveLength(0);

    play.click();
    await frames(2);
    expect(play.textContent).toBe("Play");
    const stoppedAt = host.querySelector(".replay-readout")!.textContent;
    await frames(10);
    expect(host.querySelector(".replay-readout")!.textContent).toBe(stoppedAt);
  });

  it("keeps a log row's element while the log follows playback", async () => {
    const { host, play } = await inspector();
    const first = () =>
      Array.from(host.querySelectorAll<HTMLElement>(".inspect-log .inspect-log-row")).find(
        (row) => row.querySelector(".inspect-log-seq")?.textContent === "3",
      );
    await frames(2);
    const row = first();
    expect(row).toBeDefined();
    play.click();
    await frames(8);
    expect(first()).toBe(row);
    play.click();
  });
});
