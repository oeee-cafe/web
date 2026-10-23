import { expect, it } from "vitest";
import { GUEST_OWNER, newDraftId, putLocalDraft, type LocalDraft } from "../shared/localDrafts";

function kept(overrides: Partial<LocalDraft>): LocalDraft {
  return {
    id: newDraftId(),
    owner: GUEST_OWNER,
    savedAt: Date.now(),
    png: new Uint8Array([137, 80, 78, 71]).buffer,
    replay: new Uint8Array([1]).buffer,
    width: 400,
    height: 300,
    tool: "neo-cucumber-offline",
    paintDurationMs: 1000,
    strokeCount: 1,
    communityId: null,
    communityName: null,
    parentPostId: null,
    ...overrides,
  };
}

/**
 * A drawing kept in this browser goes back into the painter the way the New
 * drawing dialog opens one -- a POST to /draw for its canvas -- naming itself
 * for the painter to open. A collaborative session's has no replay to carry
 * on from, and is offered no such thing.
 */
it("sends a kept drawing back to the painter to carry on with", async () => {
  document.body.innerHTML = `
    <section id="local-drafts" hidden data-user-id="">
      <div class="posts-grid" id="local-drafts-grid"></div>
    </section>
    <script id="local-drafts-words" type="application/json">{"continue": "Continue drawing"}</script>`;

  const drawing = kept({
    communityId: "11111111-1111-1111-1111-111111111111",
    communityName: "Cucumbers",
    parentPostId: "22222222-2222-2222-2222-222222222222",
  });
  await putLocalDraft(drawing);
  await putLocalDraft(
    kept({ tool: "collaborative", collaborativeSessionId: "33333333-3333-3333-3333-333333333333" }),
  );

  const submitted: HTMLFormElement[] = [];
  HTMLFormElement.prototype.submit = function (this: HTMLFormElement) {
    submitted.push(this);
  };

  await import("./entry");
  await expect.poll(() => document.querySelectorAll(".local-draft-item").length).toBe(2);

  const continues = Array.from(document.querySelectorAll("button")).filter(
    (button) => button.textContent === "Continue drawing",
  );
  expect(continues).toHaveLength(1);
  continues[0].click();

  expect(submitted).toHaveLength(1);
  const form = submitted[0];
  expect(form.method).toBe("post");
  expect(new URL(form.action).pathname).toBe("/draw");
  expect(form.isConnected).toBe(true);
  expect(Object.fromEntries(new FormData(form))).toEqual({
    width: "400",
    height: "300",
    community_id: "11111111-1111-1111-1111-111111111111",
    local_draft_id: drawing.id,
    parent_post_id: "22222222-2222-2222-2222-222222222222",
  });
});
