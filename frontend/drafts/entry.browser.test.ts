import { expect, it } from "vitest";
// The site's stylesheet, as the drafts page is served with it: the grid and
// its frames are what decide how large a drawing is drawn.
import "../../static/style.css";
import { GUEST_OWNER, newDraftId, putLocalDraft } from "../shared/localDrafts";

/** A 1024×800 PNG, drawn here, which unframed would fill the page. */
async function largePng(): Promise<ArrayBuffer> {
  const canvas = document.createElement("canvas");
  canvas.width = 1024;
  canvas.height = 800;
  canvas.getContext("2d")!.fillRect(0, 0, 1024, 800);
  const blob = await new Promise<Blob>((resolve) => canvas.toBlob((b) => resolve(b!), "image/png"));
  return blob.arrayBuffer();
}

it("draws a kept drawing at the size of the grid's cell", async () => {
  document.body.innerHTML = `
    <div class="profile drafts-page" style="width: 900px; --home-cols: 6">
      <section id="local-drafts" hidden data-user-id="">
        <div class="posts-grid" id="local-drafts-grid"></div>
      </section>
      <div id="local-drafts-empty"></div>
    </div>
    <script id="local-drafts-words" type="application/json">{}</script>`;

  await putLocalDraft({
    id: newDraftId(),
    owner: GUEST_OWNER,
    savedAt: Date.now(),
    png: await largePng(),
    replay: new Uint8Array([1]).buffer,
    width: 1024,
    height: 800,
    tool: "neo-cucumber-offline",
    paintDurationMs: 1000,
    strokeCount: 1,
    communityId: null,
    communityName: null,
    parentPostId: null,
  });

  await import("./entry");
  await expect.poll(() => document.querySelector("#local-drafts-grid img")).toBeTruthy();

  const cell = document.querySelector<HTMLElement>(".local-draft-item")!.getBoundingClientRect();
  const image = document.querySelector<HTMLImageElement>("#local-drafts-grid img")!.getBoundingClientRect();
  expect(image.width).toBeLessThanOrEqual(cell.width + 1);
  expect(image.height).toBeLessThanOrEqual(cell.width + 1);
  // The frame is a link to the drawing at full size.
  expect(document.querySelector("#local-drafts-grid .local-draft-item > a:first-child img")).toBeTruthy();
});
