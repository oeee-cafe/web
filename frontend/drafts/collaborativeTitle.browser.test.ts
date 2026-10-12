import { expect, it } from "vitest";
import { GUEST_OWNER, newDraftId, putLocalDraft } from "../shared/localDrafts";

/** A small PNG, drawn here. */
async function png(): Promise<ArrayBuffer> {
  const canvas = document.createElement("canvas");
  canvas.width = 8;
  canvas.height = 8;
  const blob = await new Promise<Blob>((resolve) => canvas.toBlob((b) => resolve(b!), "image/png"));
  return blob.arrayBuffer();
}

/**
 * A collaborative session a guest owner kept here is shown under the title
 * its owner gave it, where every other kept drawing is "Untitled" -- the
 * other drafts have nowhere a title could have come from.
 */
it("shows a kept collaborative session under its own title", async () => {
  document.body.innerHTML = `
    <section id="local-drafts" hidden data-user-id="">
      <div class="posts-grid" id="local-drafts-grid"></div>
    </section>
    <div id="local-drafts-empty"></div>
    <script id="local-drafts-words" type="application/json">{"untitled": "Untitled"}</script>`;

  const sessionId = newDraftId();
  await putLocalDraft({
    id: newDraftId(),
    owner: GUEST_OWNER,
    savedAt: Date.now(),
    png: await png(),
    replay: new ArrayBuffer(0),
    width: 8,
    height: 8,
    tool: "collaborative",
    paintDurationMs: 0,
    strokeCount: 0,
    communityId: null,
    communityName: null,
    parentPostId: null,
    collaborativeSessionId: sessionId,
    title: "Saturday jam",
  });

  await import("./entry");
  await expect
    .poll(() =>
      Array.from(document.querySelectorAll(".post-card-title")).map((title) => [
        title.textContent,
        title.classList.contains("is-untitled"),
      ]),
    )
    .toContainEqual(["Saturday jam", false]);
});
