/**
 * The site's own alert and question (confirm_dialog.jinja), which every page
 * the site serves has -- the painters' and the collaborative room's
 * included. Never the browser's alert() or confirm(): those are titled with
 * the site's address and worded in the browser's language, and the apps'
 * web views show them as a box of their own or not at all (the Apple apps
 * implement neither). A page without the site's dialog, which only a
 * development page is, gets the message in its console instead.
 */
interface SiteDialogs {
  dsAlert?: (message: string) => void;
  dsConfirm?: (text: string, action?: string, tone?: "plain") => Promise<boolean>;
}

const site = () => window as unknown as SiteDialogs;

/** Something a page has to say, and OK. */
export function say(message: string): void {
  const alert = site().dsAlert;
  if (alert) alert(message);
  else console.warn(message);
}

/**
 * A yes or no: the question, what saying yes does (`action`, or Delete), and
 * `tone` "plain" for one that destroys nothing. Resolves to whether it was
 * yes; without the site's dialog, no.
 */
export function ask(text: string, action?: string, tone?: "plain"): Promise<boolean> {
  const confirm = site().dsConfirm;
  if (confirm) return confirm(text, action, tone);
  console.warn(text);
  return Promise.resolve(false);
}
