/**
 * NEO's toolbox as Tailwind utilities.
 *
 * These replace the three stylesheets this column used to carry: a verbatim
 * copy of neo.css, a generated transcription of the rules NEO builds at
 * runtime, and a hand-written chrome file that then had to fight both. Every
 * value here was read off a running NEO 1.7.0 rather than off its source, so
 * where the two disagreed the running instance won -- the column background is
 * `#ccccff`, not the `#ffffff` the generator had produced.
 *
 * They are strings rather than a stylesheet so Tailwind sees them: it scans
 * source text for candidates, so every utility below has to appear written out
 * in full at least once. Do not build these names by interpolation.
 */

/**
 * The column itself. `line-height: 18px` is NEO's, set on `.NEO`, and it is
 * what gives every 12px label an 18px box -- without it the labels sit a few
 * pixels high and nothing else lines up.
 *
 * The width is 50px where NEO's `#toolSet` is 52px, and that is deliberate.
 * NEO's widgets are 48px wide plus the 1px ring that box-shadow paints just
 * outside them, so they occupy 50px and leave 2px of slack in its column --
 * which NEO never shows, because it hangs the whole wrapper off the edge of
 * the painter (`#toolsWrapper { right: -3px }`). A floating panel has no edge
 * to hide slack behind, so keeping the raw 52px only produced a lopsided
 * panel: 3px of margin down one side and 7px down the other. The padding puts
 * the ring back inside, so every widget still sits exactly where NEO puts it.
 */
export const NEO_COLUMN =
  "w-[50px] px-[1px] shrink-0 bg-(--neo-tool-set) font-[Arial] text-(--neo-tool-text) " +
  "leading-[18px] cursor-default";

/*
  -------------------------------------------------------------------------
    Tool tips
  -------------------------------------------------------------------------

  46x18 with a 1px bevel and a 1px black ring outside it, 3px of margin above
  and below. Adjacent margins collapse, so the tips sit 23px apart rather than
  26px; that is why these are margins and not a flex gap.
*/

/*
 * These are outer sizes, and deliberately so.
 *
 * NEO sizes a tip content-box: 46x18 of artwork inside a 1px bevel, measuring
 * 48x20 overall. The app sets border-box globally, so the same widget is
 * written here as its 48x20 outer box, which leaves exactly NEO's 46x18 for
 * the artwork canvas. Writing 46x18 under border-box instead would give a
 * 44x16 content area -- a 21px rhythm rather than NEO's 23px, and two pixels
 * clipped off every sprite.
 */
const TIP_BASE =
  "relative mx-0 my-[3px] block h-[20px] w-[48px] border p-0 " +
  "shadow-[0_0_0_1px_var(--neo-tool-frame)]";

/** An unselected tool: raised, on `tool_color_button`. */
export const NEO_TIP_OFF =
  TIP_BASE +
  " bg-(--neo-tool-button) border-t-(--neo-bevel-light) border-l-(--neo-bevel-light) " +
  "border-r-(--neo-bevel-shadow) border-b-(--neo-bevel-shadow)";

/**
 * The selected tool: pressed, on `tool_color_button` darkened by NEO's own
 * 0.7 multiplier. Its top and left borders go transparent rather than dark,
 * which lets the black ring read as the shadow of a pressed button.
 */
export const NEO_TIP_ON =
  TIP_BASE +
  " bg-(--neo-tool-button-on) border-t-transparent border-l-transparent " +
  "border-r-(--neo-bevel-light) border-b-(--neo-bevel-light)";

/**
 * A tip that sets something rather than selecting a tool -- the draw mode and
 * the mask. NEO gives these `tool_color_button2` and never presses them.
 */
export const NEO_TIP_FIXED =
  TIP_BASE +
  " bg-(--neo-tool-button2) border-t-(--neo-bevel-light) border-l-(--neo-bevel-light) " +
  "border-r-(--neo-bevel-shadow) border-b-(--neo-bevel-shadow)";

/**
 * The text over a tip's artwork.
 *
 * It hangs 4px below the button and is deliberately not clipped: NEO's own
 * labels overflow their buttons, which is why its English toolbox reads
 * "Halftone" spilling past the edge. Clipping them would be tidier and wrong.
 */
export const NEO_TIP_LABEL =
  "pointer-events-none absolute bottom-[-4px] left-[1px] whitespace-pre " +
  "text-[12px] tracking-normal";

/** The 46x18 artwork canvas, pinned under the label. */
export const NEO_TIP_CANVAS = "pointer-events-none absolute top-0 left-0";

/*
  -------------------------------------------------------------------------
    Colour swatches
  -------------------------------------------------------------------------
*/

/** The 48x144 block the fourteen swatches are positioned inside. */
export const NEO_COLORTIPS = "relative mt-[4px] h-[144px] w-[48px]";

/**
 * One swatch. Positioned absolutely by NEO rather than laid out, with a 1px
 * black ring and a 1px negative top margin that lifts the first row over the
 * block's edge.
 */
export const NEO_COLORTIP =
  "absolute m-0 mt-[-1px] mr-[4px] h-[18px] w-[22px] overflow-hidden p-0 " +
  "shadow-[0_0_0_1px_var(--neo-tool-frame)]";

/** The bevel sprite laid over a swatch at half strength. */
export const NEO_COLORTIP_SPRITE_IMG =
  "pointer-events-none absolute top-0 h-[18px] w-[44px] max-w-[44px] opacity-50";

/*
  -------------------------------------------------------------------------
    Sliders, reserves, layers
  -------------------------------------------------------------------------

  All four share `tool_color_bar` and the same 1px black ring.
*/

const BAR = "relative bg-(--neo-tool-bar) shadow-[0_0_0_1px_var(--neo-tool-frame)]";

/** One RGBA channel: 48x13, 3px above. */
export const NEO_COLOR_SLIDER = BAR + " mt-[3px] h-[13px] w-[48px]";

/** The brush size bar: 48x33, 4px above. */
export const NEO_SIZE_SLIDER = BAR + " mt-[4px] h-[33px] w-[48px]";

/** The three tool memories: 48x13, 4px above. */
export const NEO_RESERVE_CONTROL = BAR + " mt-[4px] h-[13px] w-[48px]";

/** The layer button: 48x20, 6px above. */
export const NEO_LAYER_CONTROL = BAR + " mt-[6px] h-[20px] w-[48px]";

/**
 * A channel's fill. The inset shadow darkens its leading edge, which is what
 * stops a full bar from looking like a flat block of colour.
 */
export const NEO_COLOR_SLIDER_FILL =
  "pointer-events-none absolute top-0 left-0 h-full " +
  "shadow-[inset_-1px_0_0_0_rgba(0,0,0,0.3)]";

/** The size fill grows downward, so its inset edge is on top. */
export const NEO_SIZE_SLIDER_FILL =
  "pointer-events-none absolute top-0 left-0 w-full " +
  "shadow-[inset_0_-1px_0_0_rgba(0,0,0,0.3)]";

/** Every bar labels itself in the same place. */
export const NEO_BAR_LABEL =
  "pointer-events-none absolute bottom-[-3px] left-[2px] text-[12px] " +
  "tracking-normal";

/**
 * NEO's slider hit areas are larger than the bars they drive, and overhang
 * them: 6px each side on a channel, 4px above the size bar. Dragging outside
 * the bar is how it switches to fine adjustment, so the overhang is load
 * bearing rather than a courtesy.
 */
export const NEO_COLOR_SLIDER_HIT =
  "absolute top-0 left-[-6px] h-[13px] w-[60px] touch-none bg-white opacity-[0.01]";
export const NEO_SIZE_SLIDER_HIT =
  "absolute top-[-4px] left-0 h-[41px] w-[48px] touch-none bg-white opacity-[0.01]";

/** One saved tool: NEO's 11x8 inside a 1px black border, so 13x10 outer. */
export const NEO_RESERVE =
  "absolute top-[1px] h-[10px] w-[13px] border border-(--neo-tool-frame) p-0";

/** The strip behind the layer name, underlined in the label colour. */
export const NEO_LAYER_BG =
  "pointer-events-none absolute top-0 left-[2px] h-[10px] w-[44px] " +
  "border-b border-b-(--neo-tool-text)";

/** The layer name. Two of them exist; NEO shows whichever layer is current. */
export const NEO_LAYER_LABEL =
  "pointer-events-none absolute left-[2px] text-[11px] tracking-normal";

/**
 * The diagonal NEO strikes through a hidden layer's half of the button. It is
 * a gradient rather than a border so it can cross the box corner to corner.
 */
export const NEO_LAYER_LINE =
  "pointer-events-none absolute left-[-1px] m-0 h-[10px] w-[50px] p-0 " +
  "bg-[linear-gradient(to_top_right,transparent,transparent_47%,red_47%,red_53%,transparent_53%,transparent)]";

/*
  -------------------------------------------------------------------------
    Panel chrome
  -------------------------------------------------------------------------

  The window the column sits in, and the controls beside it that NEO keeps
  outside its toolbox -- undo, zoom, saving. NEO's own values again: the face
  is `color_icon`, the bevels are it multiplied by 1.3 and 0.7, and the
  selected state is `color_iconselect`.
*/

/** The floating panel: NEO's face, raised off the page. */
export const NEO_PANEL =
  "bg-(--neo-icon) text-(--neo-text) font-[Arial] text-[12px] leading-[18px] " +
  "border-t border-l border-t-(--neo-bevel-light) border-l-(--neo-bevel-light) " +
  "border-r border-b border-r-(--neo-panel-shadow) border-b-(--neo-panel-shadow) " +
  "cursor-default";

/**
 * The bevel and behaviour every control in the chrome shares.
 *
 * `neo-button` styles nothing. It is how a host page tells these apart from
 * the rest of its markup -- the site feels them as a light press in its apps
 * (theme_head.jinja), and a utility class is no name to find a button by.
 */
const BUTTON_FACE =
  "neo-button bg-(--neo-icon) text-(--neo-text) rounded-none leading-[1.2] " +
  "border-t border-l border-t-(--neo-bevel-light) border-l-(--neo-bevel-light) " +
  "border-r border-b border-r-(--neo-panel-shadow) border-b-(--neo-panel-shadow) " +
  "hover:not-disabled:bg-(--neo-bk2) " +
  "active:not-disabled:border-t-(--neo-panel-shadow) active:not-disabled:border-l-(--neo-panel-shadow) " +
  "active:not-disabled:border-r-(--neo-bevel-light) active:not-disabled:border-b-(--neo-bevel-light) " +
  "disabled:text-(--neo-panel-shadow) disabled:opacity-100";

/** A pushable control carrying a label. */
export const NEO_BUTTON = BUTTON_FACE + " px-[6px] py-[2px]";

/**
 * The bevel run backwards, so a surface reads as cut into the panel rather
 * than raised off it.
 *
 * NEO has no sunken widget to copy -- the only thing it lets you type into is
 * nothing at all, and its bars carry their own ring instead. This is the same
 * four colours the panel uses, swapped corner for corner, which is what makes
 * a field belong to the panel it sits on rather than merely sit near it.
 */
const SUNKEN =
  "border-t border-l border-t-(--neo-panel-shadow) border-l-(--neo-panel-shadow) " +
  "border-r border-b border-r-(--neo-bevel-light) border-b-(--neo-bevel-light)";

/**
 * The colour input, sized to the column. NEO has no colour picker at all --
 * its swatches and RGBA sliders are the picker -- so this only ever appears
 * in our own panel.
 */
export const NEO_COLOR_INPUT =
  "block h-[18px] w-full cursor-pointer rounded-none p-0 " +
  SUNKEN +
  " [&::-webkit-color-swatch-wrapper]:p-[1px] " +
  "[&::-webkit-color-swatch]:border-none [&::-moz-color-swatch]:border-none";

/**
 * Somewhere to type: sunken, on the page ground rather than the panel face.
 *
 * The caret cursor is set here rather than left to the browser because
 * `cursor` inherits and NEO_PANEL sets it: a field on a NEO panel that did not
 * say otherwise would show the panel's arrow over its own text.
 */
export const NEO_FIELD =
  SUNKEN + " bg-(--neo-bk) text-(--neo-text) rounded-none cursor-text";

/** A sunken surface holding content that scrolls -- a log, a list, a message. */
export const NEO_WELL = SUNKEN + " bg-(--neo-bk) text-(--neo-text)";

/**
 * A control carrying nothing but an icon.
 *
 * It has no horizontal padding on purpose. A flex item will not shrink below
 * its content, so a 14px icon plus NEO_BUTTON's padding refuses to go under
 * 28px -- two of those overflow the 52px NEO is wide and drag the whole panel
 * wider than the column it sits beside.
 */
export const NEO_ICON_BUTTON = BUTTON_FACE + " px-0 py-[2px]";

/**
 * A control spanning the panel's width, which is the shape the stack under
 * the colour input is made of.
 *
 * It exists as one name rather than as `NEO_ICON_BUTTON` plus four layout
 * utilities because hosts append to that stack -- the site's Save and Help
 * buttons are the host's, not the toolbox's -- and a host writing those
 * utilities out itself is a host whose buttons are only styled for as long as
 * something inside this package happens to use the same ones.
 */
export const NEO_PANEL_BUTTON =
  NEO_ICON_BUTTON + " flex w-full items-center justify-center gap-[3px]";

/** A control that stays pressed while its setting is on. */
export const NEO_BUTTON_ON =
  "bg-(--neo-icon-select) border-t-(--neo-panel-shadow) border-l-(--neo-panel-shadow) " +
  "border-r-(--neo-bevel-light) border-b-(--neo-bevel-light)";

/** The strip across a window's top, which reads as a title bar. */
export const NEO_TITLEBAR =
  "bg-[linear-gradient(to_right,var(--neo-titlebar-from),var(--neo-titlebar-to))] " +
  "text-(--neo-titlebar-text) p-[1px]";

/**
 * The same strip when it is what you grab to move the window.
 *
 * `touch-none` is load bearing: without it a drag on a touch screen scrolls
 * the page instead of moving the window.
 *
 * Three 3px dots inside 1px of padding make a strip five pixels tall, which is
 * a fine target for a cursor and no target at all for a fingertip -- and the
 * next thing down is always a tool button, so missing it does not fail
 * harmlessly, it draws. On a coarse pointer the strip grows to 18px, about the
 * height of one of the widgets below it.
 *
 * That is well under the 44px a touch target is supposed to be, and it is the
 * whole of the target: dragging the panel by its background was tried and
 * removed, being worth little on a column that is almost entirely widgets.
 * These panels are 52px wide, so a bar sized to the guideline would be a square
 * of chrome sitting on the canvas -- 18px is the compromise, and if it proves
 * too mean under a thumb this is the number to raise.
 */
export const NEO_TITLEBAR_HANDLE =
  NEO_TITLEBAR +
  " flex touch-none items-center gap-[3px] cursor-grab active:cursor-grabbing " +
  "pointer-coarse:min-h-[18px] pointer-coarse:gap-[5px] pointer-coarse:px-[4px]";

/**
 * One of the three dots that mark a handle as grabbable. NEO has no such
 * thing to copy -- nothing of its own moves -- so this is the whole of the
 * affordance, and it is the reason a window needs no title to be legible.
 */
export const NEO_TITLEBAR_DOT =
  "h-[3px] w-[3px] shrink-0 rounded-full bg-white/70 first:ml-[3px] " +
  "pointer-coarse:h-[4px] pointer-coarse:w-[4px] pointer-coarse:first:ml-0";

/**
 * The corner you drag to resize a window, and the ticks drawn in it.
 *
 * This is a real element rather than CSS `resize` because CSS `resize` draws
 * nothing on a touch browser -- Chrome on Android renders no grabber at all,
 * so a window using it looks like a window that cannot be resized. Drawing the
 * corner ourselves is also the only way it can wear NEO's shadow colour rather
 * than the browser's idea of a resizer.
 */
export const NEO_RESIZE_HANDLE =
  "absolute right-0 bottom-0 z-30 h-[20px] w-[20px] touch-none cursor-se-resize";

/** The diagonal ticks inside the corner. Purely a mark: it takes no pointers. */
export const NEO_RESIZE_GRIP =
  "pointer-events-none absolute right-[2px] bottom-[2px] h-[12px] w-[12px] " +
  "bg-[linear-gradient(135deg,transparent_0%,transparent_42%,var(--neo-panel-shadow)_42%,var(--neo-panel-shadow)_50%,transparent_50%,transparent_62%,var(--neo-panel-shadow)_62%,var(--neo-panel-shadow)_70%,transparent_70%)]";

/** A key cap in the shortcut list. */
export const NEO_KBD =
  "bg-(--neo-icon) rounded-none px-[3px] " +
  "border-t border-l border-t-(--neo-bevel-light) border-l-(--neo-bevel-light) " +
  "border-r border-b border-r-(--neo-panel-shadow) border-b-(--neo-panel-shadow)";
