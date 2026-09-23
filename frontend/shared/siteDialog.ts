/**
 * Something a page has to say, through the site's own alert
 * (confirm_dialog.jinja) wherever the page has one -- every page the site
 * serves, the painters' and the collaborative room's included. The
 * browser's own is titled with the site's address and worded in the
 * browser's language, and the apps would have to draw it themselves; only
 * a page without the site's (the offline painter) falls back to it.
 */
export function say(message: string): void {
  const site = (window as unknown as { dsAlert?: (message: string) => void }).dsAlert;
  if (site) site(message);
  else window.alert(message);
}
