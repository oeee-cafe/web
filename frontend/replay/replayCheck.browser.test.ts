import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, type PainterOperation } from "neo-cucumber";
import { encodePainterOperation } from "../collaborate/binaryProtocol";
import type { ArchiveManifest, ArchivedEntry } from "./archiveLog";
import type { SessionDetails } from "./dom";
import { checkReplay } from "./replayCheck";

/**
 * The replay check through real painters: the saved post here is made the
 * way the owner makes it -- a painter that applied the room's messages,
 * exported -- so "the same" is the claim under test and not an assumption.
 */

const WIDTH = 64;
const HEIGHT = 48;
const HISTORY = "00000000-0000-0000-0000-000000000007";

function stroke(at: { x: number; y: number }): PainterOperation {
  return {
    kind: "stroke",
    layer: "foreground",
    brushSize: 4,
    brush: "solid",
    color: { r: 200, g: 30, b: 60, a: 255 },
    points: [
      { x: at.x, y: at.y },
      { x: at.x + 6, y: at.y + 2 },
    ],
    mask: { type: 0, r: 0, g: 0, b: 0 },
  };
}

const MARKS: [number, { x: number; y: number }][] = [
  [1, { x: 8, y: 8 }],
  [2, { x: 30, y: 20 }],
  [1, { x: 44, y: 36 }],
];

function entries(marks = MARKS): ArchivedEntry[] {
  return marks.map(([user, point], index) => ({
    at: 1000 + index * 100,
    seq: index + 1,
    from: `conn-${user}`,
    historyId: HISTORY,
    payload: new Uint8Array(encodePainterOperation(user, stroke(point))),
  }));
}

const manifest = (firstSeq = 1): ArchiveManifest => ({
  format: "oeee-collab-archive",
  version: 2,
  session: "s",
  canvas: { width: WIDTH, height: HEIGHT, mode: "standard" },
  started_at: "",
  ended_at: "",
  duration_ms: 0,
  recording: { first_seq: firstSeq, last_seq: 3, first_at: 1000, last_at: 1200, messages: 3 },
  participants: [],
  sealed: true,
});

const details = (saved: string | null) =>
  ({ session: { saved_post_id: saved }, participants: [], seats: [] }) as unknown as SessionDetails;

/** What the owner's save would have uploaded, for these marks. */
async function savedPost(marks = MARKS): Promise<Blob> {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const painter = mount(host, {
    width: WIDTH,
    height: HEIGHT,
    mode: { kind: "standard" },
    controls: { kind: "none" },
    recordReplay: false,
    synchronization: { actorId: "owner", onOperation: () => {} },
  });
  await painter.ready;
  for (const [index, [user, point]] of marks.entries()) {
    await painter.applyCanonicalOperation({
      id: `saved:${index}`,
      actorId: String(user),
      sequence: index + 1,
      operation: stroke(point),
    });
  }
  const png = await painter.exportPng();
  painter.unmount();
  host.remove();
  return png;
}

/** Answers the two requests a check makes, and keeps what it posts. */
function server(reference: Blob | null) {
  const kept: unknown[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      if (url.endsWith("/reference")) {
        return reference ? new Response(reference, { status: 200 }) : new Response("", { status: 404 });
      }
      if (url.endsWith("/check")) {
        kept.push(JSON.parse(String(init?.body)));
        return new Response(null, { status: 204 });
      }
      throw new Error(`unexpected ${url}`);
    }),
  );
  return kept;
}

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.textContent = "";
});

async function run(options: { reference: Blob | null; firstSeq?: number; saved?: string | null }) {
  const kept = server(options.reference);
  const host = document.createElement("div");
  document.body.appendChild(host);
  await checkReplay({
    host,
    base: "/admin/collaborative-sessions/s",
    manifest: manifest(options.firstSeq),
    entries: entries(),
    details: details(options.saved === undefined ? "post" : options.saved),
    chatLines: 2,
    reports: 1,
  });
  return { host, kept: kept as Record<string, unknown>[] };
}

describe("checking a replay against what was saved", () => {
  it("finds a complete recording comes out as its saved post", async () => {
    const { host, kept } = await run({ reference: await savedPost() });
    expect(host.textContent).toContain("pixel for pixel");
    expect(kept).toHaveLength(1);
    expect(kept[0]).toMatchObject({
      outcome: "match",
      differing_pixels: 0,
      total_pixels: WIDTH * HEIGHT,
      seq: 3,
      chat_lines: 2,
      reports: 1,
    });
  });

  /** The failure this exists for: a replay missing, or adding, a mark. */
  it("says where a replay parts from what was saved", async () => {
    const { host, kept } = await run({ reference: await savedPost(MARKS.slice(0, 2)) });
    expect(host.textContent).toContain("does not come out as the saved post");
    expect(host.querySelectorAll("canvas")).toHaveLength(3);
    expect(kept[0].outcome).toBe("differs");
    expect(kept[0].differing_pixels as number).toBeGreaterThan(0);
  });

  it("does not claim to have checked a recording that starts part-way", async () => {
    const { kept } = await run({ reference: null, firstSeq: 3310 });
    expect(kept[0]).toMatchObject({ outcome: "incomplete", differing_pixels: null });
  });

  it("notes that a session never saved has nothing to check against", async () => {
    const { kept } = await run({ reference: null, saved: null });
    expect(kept[0].outcome).toBe("unavailable");
  });

  /** A check that could not run says nothing about the recording, so it is
   * not kept as if it did. */
  it("keeps nothing when the saved post cannot be read", async () => {
    const { host, kept } = await run({ reference: null });
    expect(host.textContent).toContain("Could not check the replay");
    expect(kept).toHaveLength(0);
  });
});
