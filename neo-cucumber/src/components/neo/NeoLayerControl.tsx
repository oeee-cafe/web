import { useRef } from "react";
import {
  NEO_LAYER_BG,
  NEO_LAYER_CONTROL,
  NEO_LAYER_LABEL,
  NEO_LAYER_LINE,
} from "./neoClasses";
import { usePainterLabels } from "../../hooks/usePainterLabels";

interface NeoLayerControlProps {
  /** Which layer is being drawn on. */
  current: "foreground" | "background";
  fgVisible: boolean;
  bgVisible: boolean;
  onSwitch: () => void;
  /** NEO's right-click: hide or show the layer you are on. */
  onToggleVisible: () => void;
}

/**
 * NEO's layer button.
 *
 * One button, not two: it names the layer you are on, clicking swaps, and
 * right-clicking hides that layer. A hidden layer is struck through with a red
 * diagonal across its half of the button -- the top half is the foreground,
 * the bottom the background -- so both layers' states are visible even though
 * only one is named.
 *
 * The swap waits for the release rather than taking the press. A touch screen
 * has no second button, so it asks for the context menu with a long press --
 * which arrives as an ordinary press first and `contextmenu` after it. Swapping
 * on the press meant a long press swapped the layer and then hid the one it had
 * just swapped to, and nothing about that is visible: the button reads the new
 * layer, the canvas goes on taking every stroke and recording it, and the only
 * sign is a drawing that has stopped appearing. A context menu now cancels the
 * swap its own press was going to make.
 */
export function NeoLayerControl({
  current,
  fgVisible,
  bgVisible,
  onSwitch,
  onToggleVisible,
}: NeoLayerControlProps) {
  const labels = usePainterLabels();
  /** The press that will swap layers, if nothing claims it first. */
  const pendingSwap = useRef<number | null>(null);

  return (
    <button
      type="button"
      className={NEO_LAYER_CONTROL}
      title="Switch layer — right-click to hide it"
      onPointerDown={(e) => {
        if (e.button === 2) return;
        pendingSwap.current = e.pointerId;
      }}
      onPointerUp={(e) => {
        if (pendingSwap.current !== e.pointerId) return;
        pendingSwap.current = null;
        onSwitch();
      }}
      onPointerCancel={() => {
        pendingSwap.current = null;
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        pendingSwap.current = null;
        onToggleVisible();
      }}
    >
      <div className={NEO_LAYER_BG} />
      <span className={NEO_LAYER_LABEL} style={{ top: -4 }}>
        {current === "foreground" ? labels.layers[1] : ""}
      </span>
      <span className={NEO_LAYER_LABEL} style={{ top: 6 }}>
        {current === "background" ? labels.layers[0] : ""}
      </span>
      {!fgVisible && <div className={NEO_LAYER_LINE} style={{ top: 0 }} />}
      {!bgVisible && <div className={NEO_LAYER_LINE} style={{ top: 10 }} />}
    </button>
  );
}
