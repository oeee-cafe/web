import { useCallback, useEffect, useRef } from "react";
import { CANVAS_Z_INDEX } from "neo-cucumber";
import type { SessionLink } from "./useWebSocket";

const CURSOR_IDLE_MS = 1500;
const CURSOR_FADE_MS = 300;
/**
 * How often idleness is checked. Cursors used to be aged by a timer per
 * cursor, re-armed on every message about them; a room where three people are
 * drawing is a few hundred messages a second, and that was a `clearTimeout`
 * and a `setTimeout` for each one. One sweep does the same job, and only runs
 * while somebody else's cursor is on screen.
 */
const CURSOR_SWEEP_MS = 250;

const cursorColor = (username: string): string => {
  let hash = 5381;
  for (const character of username) {
    hash = ((hash << 5) + hash) + character.charCodeAt(0);
  }
  return `hsl(${Math.abs(hash % 360)}, 75%, 35%)`;
};

interface RemoteCursor {
  element: HTMLDivElement;
  /** Latest reported position, in drawing coordinates. */
  x: number;
  y: number;
  /** Whether `x`/`y` have moved since the last frame wrote them out. */
  moved: boolean;
  /** When we last heard from them, for the idle sweep. */
  seenAt: number;
  /** Set while fading out, so the sweep does not keep re-hiding it. */
  hiddenAt: number | null;
}

/**
 * Remote cursors live inside neo-cucumber's transformed canvas container.
 * Their logical drawing coordinates therefore inherit zoom, pan and flip
 * without duplicating the painter's view-transform calculations here.
 *
 * Positions are written once per frame rather than once per message. The
 * messages carrying them arrive at whatever rate the room is drawing at --
 * every pointer move from every participant, plus every stroke -- and the
 * screen cannot show more than one position per frame anyway. Doing the work
 * per message put style writes, and the layout they invalidate, in between the
 * pointer events of whoever is drawing here.
 */
export const useRemoteCursors = (
  painterRootRef: React.RefObject<HTMLDivElement | null>,
  /** Whose cursor is not drawn: the local id, once the server assigns it. */
  link: Pick<SessionLink, "localId">,
) => {
  const cursorsRef = useRef(new Map<string, RemoteCursor>());
  const frameRef = useRef<number | null>(null);
  const sweepRef = useRef<number | null>(null);
  /**
   * The painter's transformed container, looked up once instead of per
   * message. Re-resolved if the painter remounts and takes it away.
   */
  const containerRef = useRef<HTMLElement | null>(null);

  const container = useCallback((): HTMLElement | null => {
    const cached = containerRef.current;
    if (cached?.isConnected) return cached;
    const found = painterRootRef.current?.querySelector<HTMLElement>(
      ".canvas-container",
    ) ?? null;
    containerRef.current = found;
    return found;
  }, [painterRootRef]);

  const removeCursor = useCallback((userId: string) => {
    cursorsRef.current.get(userId)?.element.remove();
    cursorsRef.current.delete(userId);
  }, []);

  const hideCursor = useCallback((userId: string) => {
    const cursor = cursorsRef.current.get(userId);
    if (!cursor || cursor.hiddenAt !== null) return;
    cursor.element.style.opacity = "0";
    cursor.hiddenAt = performance.now();
  }, []);

  /** Writes out every position that moved since the last frame. */
  const flush = useCallback(() => {
    frameRef.current = null;
    for (const cursor of cursorsRef.current.values()) {
      if (!cursor.moved) continue;
      cursor.moved = false;
      // `translate` rather than `left`/`top`: this runs while somebody is
      // drawing, and a transform does not invalidate the layout of a container
      // holding a canvas per participant.
      cursor.element.style.transform = `translate(${cursor.x}px, ${cursor.y}px)`;
    }
  }, []);

  const scheduleFlush = useCallback(() => {
    if (frameRef.current !== null) return;
    frameRef.current = requestAnimationFrame(flush);
  }, [flush]);

  /** Fades out whoever has gone quiet, and drops whoever has finished fading. */
  const sweep = useCallback(() => {
    const now = performance.now();
    for (const [userId, cursor] of [...cursorsRef.current]) {
      if (cursor.hiddenAt !== null) {
        if (now - cursor.hiddenAt >= CURSOR_FADE_MS) removeCursor(userId);
      } else if (now - cursor.seenAt >= CURSOR_IDLE_MS) {
        hideCursor(userId);
      }
    }
    if (cursorsRef.current.size === 0 && sweepRef.current !== null) {
      window.clearInterval(sweepRef.current);
      sweepRef.current = null;
    }
  }, [hideCursor, removeCursor]);

  const startSweep = useCallback(() => {
    if (sweepRef.current !== null) return;
    sweepRef.current = window.setInterval(sweep, CURSOR_SWEEP_MS);
  }, [sweep]);

  const createOrUpdateCursor = useCallback((
    userId: string, x: number, y: number, username: string,
  ) => {
    if (userId === String(link.localId)) return;

    let cursor = cursorsRef.current.get(userId);
    if (!cursor) {
      const parent = container();
      if (!parent) return;
      const color = cursorColor(username);
      const element = document.createElement("div");
      element.className = "pointer-events-none absolute transition-opacity duration-300";
      // A number from the painter's own stack, not a utility: its layers sit
      // in the thousands, and a z-30 tag was drawn under all of them.
      element.style.zIndex = String(CANVAS_Z_INDEX.collaborators);
      element.setAttribute("aria-hidden", "true");
      // Positioned by transform from the container's origin, so the numbers
      // written per frame are the drawing coordinates as they arrive.
      element.style.left = "0px";
      element.style.top = "0px";
      element.innerHTML =
        `<span data-cursor-label></span>` +
        `<span data-cursor-crosshair style="position:absolute;left:-8px;top:-8px;width:16px;height:16px;color:${color}">` +
        `<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">` +
        `<path d="M8 1v5M8 10v5M1 8h5M10 8h5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>` +
        `<circle cx="8" cy="8" r="1.5" fill="currentColor"/></svg></span>`;
      const label = element.querySelector<HTMLElement>("[data-cursor-label]")!;
      label.textContent = username;
      label.style.cssText = `position:absolute;left:50%;top:-30px;transform:translateX(-50%);` +
        `white-space:nowrap;padding:1px 4px;border:1px solid ${color};border-radius:3px;` +
        `background:rgba(255,255,255,.92);color:${color};font:700 10px sans-serif;`;
      parent.appendChild(element);
      cursor = { element, x, y, moved: true, seenAt: 0, hiddenAt: null };
      cursorsRef.current.set(userId, cursor);
      startSweep();
    }

    cursor.x = x;
    cursor.y = y;
    cursor.moved = true;
    cursor.seenAt = performance.now();
    if (cursor.hiddenAt !== null) {
      cursor.hiddenAt = null;
      cursor.element.style.opacity = "1";
    }
    scheduleFlush();
  }, [container, link, scheduleFlush, startSweep]);

  const clearCursors = useCallback(() => {
    for (const cursor of cursorsRef.current.values()) cursor.element.remove();
    cursorsRef.current.clear();
    if (frameRef.current !== null) {
      cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }
    if (sweepRef.current !== null) {
      window.clearInterval(sweepRef.current);
      sweepRef.current = null;
    }
  }, []);

  useEffect(() => clearCursors, [clearCursors]);

  return { createOrUpdateCursor, hideCursor, removeCursor, clearCursors };
};
