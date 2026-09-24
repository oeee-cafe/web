/** A node, its class and its text in one call; the page is built of little
 * else. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

/** Moves the canvas to a position: the index of the last drawing message to
 * apply, -1 for blank. Undefined when there is no recording to move. */
export type Seek = ((position: number) => void) | undefined;

/** What a panel is told as the replay moves. */
export type Panel = {
  root: HTMLElement;
  /** How many things it holds, for its tab. */
  count: number;
  /** The canvas now stands at `position`. */
  update?: (position: number, playing: boolean) => void;
  /** Called when its tab is shown, since a hidden panel has no size. */
  shown?: () => void;
};
