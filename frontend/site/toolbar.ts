// The toolbar's script (toolbarControls.ts), as toolbar.jinja loads it: a
// classic script inside the nav, built to dist-site/toolbar.js, then an
// inline line that shows this toolbar.
//
// A boosted swap brings the nav back with both tags in it, and htmx re-runs
// them: the inline line at once, where it stands, and this one only once it
// has been fetched again, since a script htmx puts in is not waited for. So
// this file does its work the first time and nothing after, and the inline
// line is what each new toolbar is shown by -- in the same place in the
// page, and at the same moment, as the inline script this was.
import { installToolbar, showToolbar } from "./toolbarControls";

const host = window as unknown as { oeeeToolbar?: { show(): void } };

if (!host.oeeeToolbar) {
  installToolbar();
  host.oeeeToolbar = { show: showToolbar };
}
