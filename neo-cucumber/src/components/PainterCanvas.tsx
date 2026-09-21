import type React from "react";
import { CANVAS_Z_INDEX } from "../neo/canvasStack";
import { fontSizeForBrush, TEXT_FONT_FAMILY } from "../neo/tools";

export interface PainterCanvasProps {
  width: number;
  height: number;
  zoom: number;
  brushType: string;
  brushSize: number;
  color: string;
  containerRef: React.RefObject<HTMLDivElement | null>;
  interactionRef: React.RefObject<HTMLCanvasElement | null>;
  cursorRef: React.RefObject<HTMLCanvasElement | null>;
  previewRef: React.RefObject<HTMLCanvasElement | null>;
  textBoxRef: React.RefObject<HTMLDivElement | null>;
  textAt: { x: number; y: number } | null;
  onTextKeyDown: (event: React.KeyboardEvent<HTMLDivElement>) => void;
  onDismissText: () => void;
}

/**
 * The shared painter surface used by both local and collaborative editors.
 * Drawing layers are inserted dynamically into `.canvas-content`; keeping the
 * frame and every overlay here prevents the two modes from drifting in zoom,
 * stacking, flip, or pointer behaviour.
 */
export function PainterCanvas({
  width,
  height,
  zoom,
  brushType,
  brushSize,
  color,
  containerRef,
  interactionRef,
  cursorRef,
  previewRef,
  textBoxRef,
  textAt,
  onTextKeyDown,
  onDismissText,
}: PainterCanvasProps) {
  const overlayWidth = Math.max(1, Math.round(width * zoom));
  const overlayHeight = Math.max(1, Math.round(height * zoom));
  const overlayStyle: React.CSSProperties = {
    width: `${width * zoom}px`,
    height: `${height * zoom}px`,
    transform: `scale(${1 / zoom})`,
    transformOrigin: "top left",
  };
  const fontSize = fontSizeForBrush(brushSize);

  return (
    <div
      ref={containerRef}
      className={`relative mx-auto border border-main bg-white touch-none select-none canvas-container ${
        // Hard pixels only hold at 1x and above; below it nearest-neighbour
        // drops whole rows. See getZoomLevels.
        zoom < 1 ? "canvas-downscaled" : ""
      } ${
        brushType === "pan"
          ? "cursor-grab active:cursor-grabbing"
          : "cursor-crosshair"
      }`}
      style={{
        width: `${width}px`,
        height: `${height}px`,
        minWidth: `${width}px`,
        minHeight: `${height}px`,
        maxWidth: `${width}px`,
        maxHeight: `${height}px`,
        flexShrink: 0,
      }}
    >
      <div className="canvas-content absolute inset-0">
        <canvas
          id="canvas"
          ref={interactionRef}
          width={width}
          height={height}
          className="absolute top-0 left-0 pointer-events-auto canvas-bg"
          style={{ width: `${width}px`, height: `${height}px` }}
        />

        {textAt && (
          <div
            ref={textBoxRef}
            contentEditable
            suppressContentEditableWarning
            onKeyDown={onTextKeyDown}
            onBlur={onDismissText}
            className="absolute whitespace-pre outline outline-1 outline-dashed outline-(--neo-tool-frame) outline-offset-[1px]"
            style={{
              left: `${textAt.x}px`,
              top: `${textAt.y - fontSize}px`,
              fontFamily: TEXT_FONT_FAMILY,
              fontSize: `${fontSize}px`,
              lineHeight: `${fontSize}px`,
              color,
              zIndex: CANVAS_Z_INDEX.textEditor,
              minWidth: "1em",
            }}
          />
        )}

        <canvas
          ref={cursorRef}
          width={overlayWidth}
          height={overlayHeight}
          className="absolute top-0 left-0 pointer-events-none"
          style={{ ...overlayStyle, zIndex: CANVAS_Z_INDEX.cursor }}
        />
        <canvas
          ref={previewRef}
          width={overlayWidth}
          height={overlayHeight}
          className="absolute top-0 left-0 pointer-events-none"
          style={{ ...overlayStyle, zIndex: CANVAS_Z_INDEX.preview }}
        />
      </div>
    </div>
  );
}
