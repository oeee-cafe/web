import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import { ToolboxPanel } from "./ToolboxPanel";
import { ALL_TOOLS } from "../constants/drawing";
import type { DrawingState } from "../types/drawing";
// Without the stylesheet the buttons are unstyled inline boxes, which sit
// side by side by accident and would pass the first test for no reason.
import "../App.css";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/*
 * Fill and Right Click sit under undo and redo: the four buttons NEO keeps
 * together in the bar above its canvas (container.js: redo, undo, fill,
 * right). Fill used to share its row with a paste button NEO does not have;
 * removing that left a hole beside fill, and this is what closes it.
 */

const state: DrawingState = {
  brushSize: 1,
  opacity: 255,
  color: "#000000",
  brushType: "solid",
  layerType: "background",
  zoomLevel: 100,
  fgVisible: true,
  bgVisible: true,
  isFlippedHorizontal: false,
};

let root: Root | null = null;
let host: HTMLElement | null = null;

afterEach(() => {
  if (root) act(() => root?.unmount());
  root = null;
  host?.remove();
  host = null;
});

function renderExtras(withRightClick: boolean) {
  i18n.load("en", {});
  i18n.activate("en");
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  const noop = () => {};
  act(() => {
    root!.render(
      <I18nProvider i18n={i18n}>
        <ToolboxPanel
          section="extras"
          drawingState={state}
          historyState={{ canUndo: false, canRedo: false }}
          paletteColors={[]}
          selectedPaletteIndex={0}
          currentZoom={1}
          isOwner={false}
          tools={ALL_TOOLS}
          isSaving={false}
          sessionEnded={false}
          onUndo={noop}
          onRedo={noop}
          onUpdateBrushType={noop}
          onUpdateDrawingState={noop}
          onUpdateColor={noop}
          onSetSelectedPaletteIndex={noop}
          onSetPaletteColor={noop}
          onZoomIn={noop}
          onZoomOut={noop}
          onZoomReset={noop}
          onZoomFit={noop}
          onSaveCollaborativeDrawing={noop}
          onToggleVirtualRight={withRightClick ? noop : undefined}
          initialPosition={{ x: 10, y: 10 }}
        />
      </I18nProvider>,
    );
  });
  const find = (name: string) =>
    host!.querySelector<HTMLElement>(`button[aria-label="${name}"], button[title="${name}"]`);
  const box = (name: string) => find(name)!.getBoundingClientRect();
  return { find, box };
}

describe("the extras column's fill row", () => {
  it("puts fill and Right Click under undo and redo, two by two", () => {
    const { box } = renderExtras(true);
    const undo = box("Undo");
    const redo = box("Redo");
    const fill = box("Fill");
    const right = box("Right click");

    expect(redo.top).toBe(undo.top);
    expect(right.top).toBe(fill.top);
    expect(fill.top).toBeGreaterThan(undo.top);
    expect(fill.left).toBe(undo.left);
    expect(right.left).toBe(redo.left);
    expect(fill.width).toBe(undo.width);
  });

  it("gives fill the whole row when the host offers no Right Click", () => {
    const { find, box } = renderExtras(false);
    expect(find("Right click")).toBeNull();

    const undo = box("Undo");
    const redo = box("Redo");
    const fill = box("Fill");
    // No hole beside it: it runs from undo's left edge to redo's right.
    expect(fill.left).toBe(undo.left);
    expect(fill.right).toBe(redo.right);
  });
});
