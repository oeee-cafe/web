import { beforeAll, describe, expect, it } from "vitest";
import {
  DB_NAME,
  deleteLocalDraft,
  getLocalDraft,
  GUEST_OWNER,
  listLocalDrafts,
  newDraftId,
  putLocalDraft,
  type LocalDraft,
} from "./localDrafts";

function draft(owner: string, savedAt: number, bytes: number[] = [1, 2, 3]): LocalDraft {
  return {
    id: newDraftId(),
    owner,
    savedAt,
    png: new Uint8Array(bytes).buffer,
    replay: new Uint8Array([9, 8, 7]).buffer,
    width: 300,
    height: 300,
    tool: "neo-cucumber-offline",
    paintDurationMs: 1234,
    strokeCount: 5,
    communityId: null,
    communityName: null,
    parentPostId: null,
  };
}

function deleteDatabase(): Promise<void> {
  return new Promise((resolve, reject) => {
    const deleting = indexedDB.deleteDatabase(DB_NAME);
    deleting.onsuccess = () => resolve();
    deleting.onerror = () => reject(deleting.error);
  });
}

function databaseNames(): Promise<string[]> {
  return indexedDB.databases().then((list) => list.map((db) => db.name ?? ""));
}

/**
 * The toolbar's count (templates/toolbar.jinja), as it is written there:
 * open, and abort the upgrade a first visit asks for.
 */
function toolbarProbe(): Promise<"absent" | number> {
  return new Promise((resolve) => {
    const opening = indexedDB.open(DB_NAME, 1);
    opening.onupgradeneeded = () => opening.transaction?.abort();
    opening.onerror = (event) => {
      event.preventDefault();
      resolve("absent");
    };
    opening.onsuccess = () => {
      const db = opening.result;
      const counting = db.transaction("drafts", "readonly").objectStore("drafts").count();
      counting.onsuccess = () => {
        db.close();
        resolve(counting.result);
      };
    };
  });
}

describe("drawings kept in this browser", () => {
  // Before anything in this file opens the database through the module,
  // which keeps its connection for the page.
  beforeAll(deleteDatabase);

  it("are counted by the toolbar without the toolbar creating the database", async () => {
    expect(await toolbarProbe()).toBe("absent");
    expect(await databaseNames()).not.toContain(DB_NAME);

    // Had the probe left an empty version 1 behind, this would find no store
    // and throw: the upgrade that creates it would never run again.
    await putLocalDraft(draft(GUEST_OWNER, 1));
    expect(await toolbarProbe()).toBe(1);
  });

  it("show a guest only guests' drawings, and an account its own besides", async () => {
    const guest = draft(GUEST_OWNER, 100);
    const mine = draft("00000000-0000-4000-8000-00000000000a", 300);
    const theirs = draft("00000000-0000-4000-8000-00000000000b", 200);
    await Promise.all([guest, mine, theirs].map(putLocalDraft));

    const signedOut = (await listLocalDrafts(null)).map((d) => d.id);
    expect(signedOut).toContain(guest.id);
    expect(signedOut).not.toContain(mine.id);
    expect(signedOut).not.toContain(theirs.id);

    const signedIn = (await listLocalDrafts(mine.owner)).map((d) => d.id);
    expect(signedIn.indexOf(mine.id)).toBeLessThan(signedIn.indexOf(guest.id));
    expect(signedIn).not.toContain(theirs.id);
  });

  it("keep the drawing's bytes, and one copy per id", async () => {
    const first = draft(GUEST_OWNER, 10, [137, 80, 78, 71]);
    await putLocalDraft(first);
    await putLocalDraft({ ...first, savedAt: 20, png: new Uint8Array([1]).buffer });

    const kept = await getLocalDraft(first.id);
    expect(kept?.savedAt).toBe(20);
    expect(Array.from(new Uint8Array(kept!.png))).toEqual([1]);
    expect((await listLocalDrafts(null)).filter((d) => d.id === first.id)).toHaveLength(1);

    await deleteLocalDraft(first.id);
    expect(await getLocalDraft(first.id)).toBeUndefined();
  });

  it("are named by version 4 UUIDs the server will parse", () => {
    const ids = new Set(Array.from({ length: 50 }, newDraftId));
    expect(ids.size).toBe(50);
    for (const id of ids) {
      expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    }
  });
});
