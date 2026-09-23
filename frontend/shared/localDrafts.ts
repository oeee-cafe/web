/**
 * Drawings kept in this browser until they are posted.
 *
 * Every save of a post lands here first. Someone signed in uploads it at once
 * and the copy here is forgotten when the upload succeeds; a guest's stays
 * until they have an account and post it from the drafts page. So a failed
 * upload, or a session that ran out while drawing, leaves the drawing here
 * instead of nowhere.
 *
 * IndexedDB rather than localStorage: a PNG and a replay are binary, and
 * localStorage holds only strings, in a quota of about five megabytes that
 * one large drawing base64-encoded can fill. The images are stored as
 * ArrayBuffers rather than Blobs because Safari before 14 could fail to store
 * a Blob in IndexedDB at all.
 *
 * The toolbar counts these with an inline script of its own
 * (templates/toolbar.jinja), so the database, store and index names below
 * are written there too.
 */

export const DB_NAME = "oeee-local-drafts";
const STORE = "drafts";
const OWNER_INDEX = "owner";

/** The owner of a drawing saved while signed out. IndexedDB cannot index null. */
export const GUEST_OWNER = "";

export type LocalDraftTool = "neo-cucumber-offline" | "cucumber";

export interface LocalDraft {
  /** Sent as `client_draft_id`, so an upload that arrives twice makes one post. */
  id: string;
  /**
   * Whose drawing this is: the id of the account that was signed in when it
   * was saved, or `GUEST_OWNER`. The drafts page shows someone their own and
   * every guest's, and never another account's.
   */
  owner: string;
  savedAt: number;
  png: ArrayBuffer;
  replay: ArrayBuffer;
  width: number;
  height: number;
  tool: LocalDraftTool;
  paintDurationMs: number;
  strokeCount: number;
  communityId: string | null;
  communityName: string | null;
  parentPostId: string | null;
}

function request<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("IndexedDB request failed"));
  });
}

function done(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () =>
      reject(transaction.error ?? new Error("IndexedDB transaction failed"));
    transaction.onabort = () =>
      reject(transaction.error ?? new Error("IndexedDB transaction aborted"));
  });
}

let database: Promise<IDBDatabase> | null = null;

/**
 * The database, opened once per page.
 *
 * Rejects where there is none to open: Firefox before 115 refuses IndexedDB
 * in a private window, some by throwing and some by failing the request, and
 * a browser with site data blocked does the same.
 */
function openDatabase(): Promise<IDBDatabase> {
  if (database) return database;
  database = new Promise<IDBDatabase>((resolve, reject) => {
    let opening: IDBOpenDBRequest;
    try {
      opening = indexedDB.open(DB_NAME, 1);
    } catch (error) {
      reject(error);
      return;
    }
    opening.onupgradeneeded = () => {
      const store = opening.result.createObjectStore(STORE, { keyPath: "id" });
      store.createIndex(OWNER_INDEX, "owner");
    };
    opening.onsuccess = () => resolve(opening.result);
    opening.onerror = () => reject(opening.error ?? new Error("IndexedDB unavailable"));
    opening.onblocked = () => reject(new Error("IndexedDB upgrade blocked"));
  });
  // Let a later call try again rather than repeat this failure forever.
  database.catch(() => {
    database = null;
  });
  return database;
}

/** Whether this browser will keep drawings at all. */
export async function localDraftsAvailable(): Promise<boolean> {
  try {
    await openDatabase();
    return true;
  } catch {
    return false;
  }
}

/**
 * A version 4 UUID. `crypto.randomUUID` is Firefox 95 and Safari 15.4, and
 * the painter is built for Firefox 56.
 */
export function newDraftId(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.prototype.map
    .call(bytes, (byte: number) => (byte + 0x100).toString(16).slice(1))
    .join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** `Blob.arrayBuffer` is Firefox 69. */
export function blobToArrayBuffer(blob: Blob): Promise<ArrayBuffer> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as ArrayBuffer);
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read blob"));
    reader.readAsArrayBuffer(blob);
  });
}

export function draftPng(draft: LocalDraft): Blob {
  return new Blob([draft.png], { type: "image/png" });
}

export function draftReplay(draft: LocalDraft): Blob {
  return new Blob([draft.replay], { type: "application/octet-stream" });
}

/** Keep `draft`, replacing whatever was kept under its id. */
export async function putLocalDraft(draft: LocalDraft): Promise<void> {
  const db = await openDatabase();
  const transaction = db.transaction(STORE, "readwrite");
  transaction.objectStore(STORE).put(draft);
  await done(transaction);
}

export async function deleteLocalDraft(id: string): Promise<void> {
  const db = await openDatabase();
  const transaction = db.transaction(STORE, "readwrite");
  transaction.objectStore(STORE).delete(id);
  await done(transaction);
}

export async function getLocalDraft(id: string): Promise<LocalDraft | undefined> {
  const db = await openDatabase();
  const store = db.transaction(STORE, "readonly").objectStore(STORE);
  return request(store.get(id) as IDBRequest<LocalDraft | undefined>);
}

/**
 * What the drafts page shows the reader: every guest's drawing, and when
 * signed in, their own. Newest first, as the server's drafts are.
 */
export async function listLocalDrafts(userId: string | null): Promise<LocalDraft[]> {
  const db = await openDatabase();
  const index = db.transaction(STORE, "readonly").objectStore(STORE).index(OWNER_INDEX);
  const owners = userId ? [GUEST_OWNER, userId] : [GUEST_OWNER];
  const lists = await Promise.all(
    owners.map((owner) => request(index.getAll(owner) as IDBRequest<LocalDraft[]>)),
  );
  return ([] as LocalDraft[])
    .concat(...lists)
    .sort((a, b) => b.savedAt - a.savedAt);
}
