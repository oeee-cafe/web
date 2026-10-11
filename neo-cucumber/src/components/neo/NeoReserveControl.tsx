import { NEO_RESERVE, NEO_RESERVE_CONTROL } from "./neoClasses";
import type { NeoReserve } from "../../neo/toolboxSpec";

interface NeoReserveControlProps {
  reserves: readonly NeoReserve[];
  /** Left click: adopt everything the slot holds. */
  onLoad: (index: number) => void;
  /** Right press: overwrite the slot with the current settings. */
  onSave: (index: number) => void;
  /** Whether the toolbox's sticky right-click button is armed. */
  virtualRight?: boolean;
  /** Called once an armed press has been spent, so the button can release. */
  onVirtualRightUsed?: () => void;
}

/**
 * NEO's three tool memories.
 *
 * Each slot holds a whole setup -- tool, colour, size, alpha and draw type --
 * not just a colour, so the swatch you see is only the part of it that can be
 * shown. Left click loads a slot, right click saves the current settings into
 * it. It ships preloaded with a black 1px pen and two white erasers, which is
 * why two of the three start out looking blank.
 *
 * "Right" is NEO's `getModifier`, not the mouse button alone: Ctrl, Alt and
 * the toolbox's sticky right-click button say it too, and Shift says it is
 * not. The sticky button is the one that matters, because it is how a pen
 * with no second button -- an Apple Pencil -- saves a slot at all.
 */
export function NeoReserveControl({
  reserves,
  onLoad,
  onSave,
  virtualRight = false,
  onVirtualRightUsed,
}: NeoReserveControlProps) {
  return (
    <div className={NEO_RESERVE_CONTROL}>
      {reserves.map((reserve, index) => (
        <button
          key={index}
          type="button"
          className={NEO_RESERVE}
          // ReserveControl.init: 15px apart, starting 2px in
          style={{ left: index * 15 + 2, backgroundColor: reserve.color }}
          title={`Slot ${index + 1}: ${reserve.tool} ${reserve.size}px — click to load, right-click to save`}
          onPointerDown={(e) => {
            const right =
              !e.shiftKey &&
              (e.button === 2 || e.ctrlKey || e.altKey || virtualRight);
            if (!right) {
              onLoad(index);
              return;
            }
            onSave(index);
            if (virtualRight) onVirtualRightUsed?.();
          }}
          // The press has already decided; this only keeps the browser's menu
          // from opening over it, which a right click and a Mac's Ctrl-click
          // would both bring up.
          onContextMenu={(e) => e.preventDefault()}
        />
      ))}
    </div>
  );
}
