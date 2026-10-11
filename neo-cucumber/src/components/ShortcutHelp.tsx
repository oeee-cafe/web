import { NEO_BUTTON, NEO_KBD, NEO_PANEL } from "./neo/neoClasses";
import type { ToolId } from "../neo/tools";
import { SHORTCUTS, describeAction, type Shortcut } from "../constants/shortcuts";

interface ShortcutHelpProps {
  open: boolean;
  onClose: () => void;
  /** The tools this painter offers; a key for any other is not listed. */
  tools: readonly ToolId[];
}

const GROUPS: Shortcut["group"][] = [
  "Tools",
  "Shape",
  "Drawing",
  "Canvas",
  "History",
];

/**
 * The shortcut list, rendered from the same table the bindings come from --
 * so it cannot teach a key that does nothing.
 */
export function ShortcutHelp({ open, onClose, tools }: ShortcutHelpProps) {
  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        className={`${NEO_PANEL} max-h-[80vh] overflow-y-auto p-4 shadow-lg`}
        style={{ minWidth: "320px" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-baseline justify-between gap-6">
          <h2 className="text-sm font-bold">Keyboard shortcuts</h2>
          <button
            type="button"
            onClick={onClose}
            className={`${NEO_BUTTON} text-xs`}
          >
            close
          </button>
        </div>

        <div className="grid gap-x-6 gap-y-3 sm:grid-cols-2">
          {GROUPS.map((group) => {
            const rows = SHORTCUTS.filter(
              (s) => s.group === group && (s.action.kind !== "tool" || tools.includes(s.action.tool)),
            );
            if (rows.length === 0) return null;
            return (
              <section key={group}>
                <h3 className="mb-1 text-xs font-bold uppercase tracking-wide opacity-70">
                  {group}
                </h3>
                <table className="w-full text-xs">
                  <tbody>
                    {rows.map((s) => (
                      <tr key={`${s.label}-${describeAction(s.action)}`}>
                        <td className="py-0.5 pr-3 align-top">
                          <kbd className={`${NEO_KBD} font-mono`}>
                            {s.label}
                          </kbd>
                        </td>
                        <td className="py-0.5 align-top">
                          {describeAction(s.action)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </section>
            );
          })}
        </div>

        <p className="mt-3 text-xs opacity-70">
          Hold Space to pan. Shift constrains a stroke while drawing.
        </p>
      </div>
    </div>
  );
}
