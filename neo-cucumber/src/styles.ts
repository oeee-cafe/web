/**
 * NEO's chrome, as class names a host can put on its own elements.
 *
 * A host always renders controls the painter does not own -- a chat panel, a
 * Save button, a session header -- and those sit inches from the toolbox, so
 * anything that is merely close to NEO's chrome reads as broken rather than as
 * different. The two ways to get them matching without this module were both
 * worse than they look: copy the utility strings into the host, where they go
 * stale the first time a bevel colour changes here and nobody notices until
 * the two panels are photographed together; or read `className` off a rendered
 * toolbox button and reuse it, which is what this repository did, and which
 * makes the host's chrome depend on the toolbox's private DOM staying the
 * shape it is today.
 *
 * These are names, not styles. The rules they name are compiled into
 * `neo-cucumber/style.css`, which a host imports once; a host that does not
 * run Tailwind at all still gets the same chrome, because the utilities are
 * already in that file. The palette underneath them is CSS custom properties
 * (`--neo-*`) defined by the same stylesheet, so a host that needs a colour
 * these names do not cover can reach for the variable rather than for a hex
 * value that will not follow the theme.
 *
 * Everything here is part of the package's public contract. The components
 * that use these names are not.
 */
export {
  /** The floating window face: NEO's panel, raised off the page. */
  NEO_PANEL,
  /** The strip across a panel's top, which reads as a title bar. */
  NEO_TITLEBAR,
  /** The same strip when it is the window's drag handle. */
  NEO_TITLEBAR_HANDLE,
  /** One of the three dots that mark a handle as grabbable. */
  NEO_TITLEBAR_DOT,
  /** A pushable control carrying a label. */
  NEO_BUTTON,
  /** The same control carrying nothing but an icon. */
  NEO_ICON_BUTTON,
  /** A control spanning a panel's width, as in the toolbox's own stack. */
  NEO_PANEL_BUTTON,
  /** Added to any of the above while its setting is on: it stays pressed. */
  NEO_BUTTON_ON,
  /** Somewhere to type, sunken into the panel. */
  NEO_FIELD,
  /** A sunken surface for content that scrolls. */
  NEO_WELL,
  /** A checkbox, sunken like a field; put it on a native `<input type="checkbox">`. */
  NEO_CHECKBOX,
  /** A key cap, as in the toolbox's shortcut list. */
  NEO_KBD,
  /** The corner a window is resized by; pair it with `attachWindowResize`. */
  NEO_RESIZE_HANDLE,
  /** The ticks drawn inside that corner. */
  NEO_RESIZE_GRIP,
} from "./components/neo/neoClasses";
