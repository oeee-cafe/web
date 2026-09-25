/**
 * Where the drawing pages report the errors they cannot handle.
 *
 * One project for both pages: an event carries the page's URL, which is how
 * a /draw error is told from a /collaborate one. The collaborative page
 * initialises the React client from this in its own entry (main.tsx); the
 * painter page loads a bundle of its own for it (frontend/painter/sentry.ts),
 * which is where the reasoning about old engines lives.
 */
export const SENTRY_OPTIONS = {
  dsn: "https://930f2aecbd98603e4dd1651924c1004a@o4504757655764992.ingest.us.sentry.io/4510046135582720",
  // The IP address, which is what stands in for a user on these pages: the
  // painter has no account of its own to name one by.
  sendDefaultPii: true,
};
