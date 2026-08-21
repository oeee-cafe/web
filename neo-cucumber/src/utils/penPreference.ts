/**
 * Whether a pen is being used here, and so whether fingers should draw.
 *
 * Someone drawing with a stylus rests a hand on the glass, and that hand lands
 * before the nib does. The palm takes the press, the hold in `useBaseDrawing`
 * runs out, and what gets drawn -- and broadcast, and written into the replay
 * -- is the heel of a hand. Refusing touches only while a pen is in contact
 * cannot help: at the moment the palm lands there is no pen contact yet.
 *
 * So the question is put to the session rather than to the moment. Once a pen
 * has been seen on this load, fingers stop drawing and are left to the pinch,
 * which is how every pen-first painter behaves.
 *
 * Deliberately not remembered across loads. A latch in storage would outlive
 * the pen that set it -- a flat stylus battery, a tablet handed to someone
 * else, a Wacom unplugged -- and leave a painter whose canvas silently refuses
 * the only pointer its user has. Forgetting costs at most the first stroke of
 * a session, and only on a device whose pen cannot hover; being wrong the
 * other way costs someone the whole app with nothing on screen to explain it.
 */

/** Set by `notePointerType` and cleared only by a reload. */
let penSeen = false;

/** True once a pen has been used on this load. */
export function penPreferred(): boolean {
  return penSeen;
}

/**
 * Notes what kind of pointer the painter just saw.
 *
 * Worth calling for hover as well as contact: a pen that reports hover
 * announces itself before it touches down, which is one stroke earlier than
 * the latch would otherwise be set.
 */
export function notePointerType(type: string): void {
  if (type === "pen") penSeen = true;
}

/**
 * Forgets the pen, for tests that need a session which has not seen one.
 *
 * Nothing in the painter calls this: within a load the latch only ever closes.
 */
export function resetPenPreference(): void {
  penSeen = false;
}
