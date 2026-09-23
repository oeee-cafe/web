import { draftPng, draftReplay, type LocalDraft } from "./localDrafts";

/** What /draw/finish answers a post with. */
export interface DrawFinishResult {
  post_id: string;
}

/**
 * Why an upload failed, as far as the page can tell. `code` is the server's
 * error code where it gave one; `UNAUTHORIZED` also covers the login redirect
 * a signed-out request to /draw/finish is answered with.
 */
export class UploadError extends Error {
  code: string | null;

  constructor(message: string, code: string | null) {
    super(message);
    this.code = code;
  }
}

export function blobToDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read PNG"));
    reader.readAsDataURL(blob);
  });
}

/**
 * Send `form` to `url` and read the JSON reply, turning every way it can fail
 * into an `UploadError`.
 */
export async function postDrawing<T>(url: string, form: FormData): Promise<T> {
  const response = await fetch(url, { method: "POST", body: form, credentials: "same-origin" });
  // /draw/finish is behind the sign-in gate, which answers a signed-out
  // request by redirecting to /login; fetch follows it to a 200 HTML page.
  if (response.redirected && new URL(response.url).pathname === "/login") {
    throw new UploadError("Not signed in", "UNAUTHORIZED");
  }
  let result: { error?: { code?: string; message?: string } | string } & Record<string, unknown>;
  try {
    result = await response.json();
  } catch {
    throw new UploadError(`Upload failed: ${response.status}`, null);
  }
  if (!response.ok || result?.error) {
    const error = result?.error;
    if (typeof error === "object" && error) {
      throw new UploadError(error.message ?? `Upload failed: ${response.status}`, error.code ?? null);
    }
    throw new UploadError(error ?? `Upload failed: ${response.status}`, null);
  }
  return result as unknown as T;
}

/**
 * Upload a kept drawing as a draft post. `withoutCommunity` sends it as a
 * personal drawing, for one whose community this account cannot post in.
 */
export async function uploadLocalDraft(
  draft: LocalDraft,
  { withoutCommunity = false }: { withoutCommunity?: boolean } = {},
): Promise<DrawFinishResult> {
  const form = new FormData();
  form.append("image", await blobToDataUrl(draftPng(draft)));
  form.append("animation", draftReplay(draft));
  form.append("width", String(draft.width));
  form.append("height", String(draft.height));
  form.append("paint_duration_ms", String(Math.max(0, Math.round(draft.paintDurationMs))));
  form.append("security_count", String(draft.strokeCount));
  form.append("tool", draft.tool);
  form.append("client_draft_id", draft.id);
  if (draft.communityId && !withoutCommunity) form.append("community_id", draft.communityId);
  if (draft.parentPostId) form.append("parent_post_id", draft.parentPostId);
  return postDrawing<DrawFinishResult>("/draw/finish", form);
}

/** Hand the reader a PNG to save. Firefox only follows a link that is in the document. */
export function downloadPng(png: Blob, savedAt: number): void {
  const url = URL.createObjectURL(png);
  const link = document.createElement("a");
  const date = new Date(savedAt);
  const pad = (n: number) => (n < 10 ? `0${n}` : String(n));
  link.href = url;
  link.download = `oeee-cafe-${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-${pad(date.getHours())}${pad(date.getMinutes())}.png`;
  link.style.display = "none";
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}
