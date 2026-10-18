// How many drawings a row (per_row_macro.jinja): a slider from big tiles to
// many small ones, the count beside it. The reader's choice is remembered in
// this browser under the control's data-key, and set on the root as its
// data-var, which the grid reads -- so the page's own head script
// (post_feed_head.jinja for the feeds) can apply it before the grid paints.
//
// The count is also written to the root as `data-{key}-density`: "dense" past
// six a row, "bare" past nine, which the grid's captions use to say less as
// the squares get narrower. Several controls for the same key on one page --
// the profile's two tabs -- stay in step.

function density(cols: number): string {
  return cols > 9 ? "bare" : cols > 6 ? "dense" : "";
}

function show(control: Element, cols: number): void {
  const slider = control.querySelector("input")!;
  slider.value = String(cols);
  control.querySelector("output")!.textContent = String(cols);
  // The filled part of the track, for engines that have no
  // ::-moz-range-progress of their own.
  slider.style.setProperty("--ds-range-fill", ((cols - 1) / 11) * 100 + "%");
}

function init(): void {
  const controls = document.querySelectorAll("[data-per-row]:not([data-ready])");
  for (let i = 0; i < controls.length; i++) {
    const control = controls[i];
    control.setAttribute("data-ready", "");
    const key = control.getAttribute("data-key")!;
    const cssVar = control.getAttribute("data-var")!;
    let cols = parseInt(control.getAttribute("data-default")!, 10);
    try {
      const saved = parseInt(localStorage.getItem(key)!, 10);
      if (saved >= 1 && saved <= 12) cols = saved;
    } catch {
      // No storage: the default.
    }
    show(control, cols);
    const slider = control.querySelector("input")!;
    slider.addEventListener("input", () => {
      const value = parseInt(slider.value, 10);
      const root = document.documentElement;
      root.style.setProperty(cssVar, String(value));
      root.setAttribute("data-" + key + "-density", density(value));
      const same = document.querySelectorAll('[data-per-row][data-key="' + key + '"]');
      for (let j = 0; j < same.length; j++) show(same[j], value);
      try {
        localStorage.setItem(key, String(value));
      } catch {
        // Not remembering it is not a reason to refuse the change.
      }
    });
  }
}

/** Sets up every control on the page not set up yet, once the page is read. */
export function attachPerRow(): void {
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
}
