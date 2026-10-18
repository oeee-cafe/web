// The pages' smallest scripts.

/**
 * Safari on iOS gives nothing :active unless the page listens for touches,
 * and the pressed states in ds.css are how a control answers a tap there now
 * that the tap highlight is off (base.jinja).
 */
export function touchActive(): void {
  document.addEventListener("touchstart", () => {}, { passive: true });
}

/**
 * In the macOS app the header floats over the content (ds.css), which needs
 * its height to start below it -- a height that changes when the toolbar
 * stacks or a message arrives (base.jinja).
 */
export function macHeaderHeight(): void {
  const root = document.documentElement;
  if (root.getAttribute("data-app") !== "macos" || !window.ResizeObserver) return;
  const header = document.querySelector<HTMLElement>(".ds-page > header")!;
  function measure(): void {
    root.style.setProperty("--ds-header-height", header.offsetHeight + "px");
  }
  measure();
  new ResizeObserver(measure).observe(header);
}

/**
 * Back to the app that opened the browser for a sign-in
 * (identity_handoff_done.jinja): once, and with replace, so going back does
 * not land here again.
 */
export function handoffDone(back: string): void {
  try {
    location.replace(back);
  } catch {
    // No app to take it: the page says what to do instead.
  }
}

/** The design reference's indeterminate checkboxes (design.jinja). */
export function indeterminateBoxes(): void {
  document.querySelectorAll<HTMLInputElement>(".ds-indeterminate").forEach((box) => {
    box.indeterminate = true;
  });
}
