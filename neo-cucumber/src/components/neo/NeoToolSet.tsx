import { useRef } from "react";
import { NeoToolTip } from "./NeoToolTip";
import { NEO_TOOL_ICONS } from "../../neo/toolIcons";
import { NEO_TIPS, neoTipArt } from "../../neo/toolboxSpec";
import { usePainterLabels } from "../../hooks/usePainterLabels";
import type { ToolId, DrawType } from "../../neo/tools";

const DRAW_TYPES: readonly DrawType[] = ["freehand", "line", "bezier"];

interface NeoToolSetProps {
  brushType: ToolId;
  drawType: DrawType;
  /** 0-4, NEO's MASKTYPE. */
  maskType: number;
  color: string;
  alpha: number;
  maskColor: string;
  /**
   * Which tools to offer. Any tip left with no tools is dropped, so a mode
   * that restricts the toolset simply loses those buttons.
   */
  tools: readonly ToolId[];
  onSelectTool: (tool: ToolId) => void;
  onSelectDrawType: (drawType: DrawType) => void;
  onSelectMaskType: (maskType: number) => void;
  /** NEO's right-click on the mask tip: adopt the drawing colour as the mask. */
  onAdoptMaskColor: () => void;
}

/**
 * NEO's `#toolSet`: seven buttons that between them reach every tool.
 *
 * A button selects its group; clicking the selected group again advances
 * within it and right-clicking steps back. Each group remembers where it was
 * left, so returning to it returns to the tool you last used rather than
 * resetting to the first -- that is `ToolTip.prototype.mode`.
 */
export function NeoToolSet({
  brushType,
  drawType,
  maskType,
  color,
  alpha,
  maskColor,
  tools,
  onSelectTool,
  onSelectDrawType,
  onSelectMaskType,
  onAdoptMaskColor,
}: NeoToolSetProps) {
  /** Where each group was left, keyed by NEO's element id. */
  const modes = useRef<Record<string, number>>({});

  // The painter's own words, in its own locale, unless the host replaced them.
  const labels = usePainterLabels();
  const drawTypeNames = DRAW_TYPES.map((type) => labels.tools[type]).join(", ");

  const tips = NEO_TIPS.map((tip) => ({
    ...tip,
    tools: tip.tools.filter((tool) => tools.includes(tool)),
  }))
    // A tip whose tools this mode does not offer has nothing left to do,
    // but the fixed tips carry settings rather than tools and always stay.
    .filter((tip) => tip.fixed || tip.tools.length > 0);

  /*
   * Paste is on no tip, and NEO does not pretend otherwise: its
   * getToolButton returns nothing for PasteTool, so updateUI leaves alone
   * whichever tip was lit -- the copy tip, since only a finished copy gets
   * there. The column goes on showing copy, selected, while the copy is
   * being placed.
   */
  const heldTool: ToolId = brushType === "paste" ? "copy" : brushType;

  const cycle = (
    name: string,
    groupTools: readonly ToolId[],
    backwards: boolean
  ) => {
    const held = groupTools.indexOf(heldTool);
    const length = groupTools.length;
    let next: number;
    if (held === -1) {
      // Not this group's turn yet: come back to where it was left
      next = modes.current[name] ?? 0;
      if (next >= length) next = 0;
    } else {
      next = backwards ? (held - 1 + length) % length : (held + 1) % length;
    }
    modes.current[name] = next;
    onSelectTool(groupTools[next]);
  };

  return (
    <div>
      {tips.map((tip) => {
        if (tip.name === "mask") {
          return (
            <NeoToolTip
              key="mask"
              fixed
              art={{ kind: "maskBar" }}
              label={labels.masks[maskType] ?? labels.masks[0]}
              title="Mask mode — click to cycle, right-click to take the current colour"
              color={color}
              alpha={alpha}
              maskColor={maskColor}
              onActivate={() =>
                onSelectMaskType((maskType + 1) % labels.masks.length)
              }
              onAlternate={onAdoptMaskColor}
            />
          );
        }

        if (tip.name === "draw") {
          const at = DRAW_TYPES.indexOf(drawType);
          return (
            <NeoToolTip
              key="draw"
              fixed
              art={neoTipArt(drawType)}
              label={labels.tools[drawType] ?? drawType}
              title={`How strokes are laid down: ${drawTypeNames}`}
              color={color}
              alpha={alpha}
              maskColor={maskColor}
              onActivate={() =>
                onSelectDrawType(DRAW_TYPES[(at + 1) % DRAW_TYPES.length])
              }
              onAlternate={() =>
                onSelectDrawType(
                  DRAW_TYPES[(at - 1 + DRAW_TYPES.length) % DRAW_TYPES.length]
                )
              }
            />
          );
        }

        const held = tip.tools.indexOf(heldTool);
        const selected = held !== -1;
        // Show the tool this group holds, or the one it would return to
        const shown =
          tip.tools[held === -1 ? (modes.current[tip.name] ?? 0) : held] ??
          tip.tools[0];

        return (
          <NeoToolTip
            key={tip.name}
            art={neoTipArt(shown)}
            label={labels.tools[shown] ?? shown}
            selected={selected}
            underlay={tip.name === "effect2" ? NEO_TOOL_ICONS.copy2 : undefined}
            title={
              tip.tools.length > 1
                ? `${labels.tools[shown] ?? shown} — click again to cycle (${tip.tools
                    .map((tool) => labels.tools[tool] ?? tool)
                    .join(", ")})`
                : (labels.tools[shown] ?? shown)
            }
            color={color}
            alpha={alpha}
            maskColor={maskColor}
            onActivate={() => cycle(tip.name, tip.tools, false)}
            onAlternate={() => cycle(tip.name, tip.tools, true)}
          />
        );
      })}
    </div>
  );
}
