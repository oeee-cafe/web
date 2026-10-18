// The page that was asked for is drawn at once, before it arrives: the
// content area swaps to a skeleton of it -- a grid of empty squares for the
// drawing feeds, a title and lines for anything else -- and the next page,
// or htmx's swap of the body, replaces it (toolbarControls.ts lays it down).
// Each skeleton is built from the page's own layout classes -- the same grid,
// card and column the page will arrive in -- with blocks where its words and
// drawings will be, so the page lands on the shape it was promised instead of
// rearranging itself. One builder a kind of page, chosen by path.

function bar(width: string, height: number, extra?: string): string {
  return (
    '<span class="ds-skeleton-bar' +
    (extra ? " " + extra : "") +
    '" style="width:' +
    width +
    ";height:" +
    height +
    'px"></span>'
  );
}

function perRow(): number {
  let cols = parseInt(getComputedStyle(document.documentElement).getPropertyValue("--home-cols"), 10);
  if (!(cols >= 1 && cols <= 12)) cols = 6;
  return window.innerWidth <= 600 ? 3 : cols;
}

// `month` for a feed that heads its drawings by month
// (post_feed_fragment.jinja): the label over the first row.
function postGrid(rows?: number, month?: boolean): string {
  let html =
    '<div class="posts-grid">' +
    (month ? '<div class="feed-period"><span class="feed-period-label">' + bar("32px", 15) + "</span></div>" : "");
  for (let i = 0, n = perRow() * (rows || 3); i < n; i++) {
    html +=
      '<div class="posts-grid-item"><div class="ds-skeleton-square"></div>' +
      '<div class="post-card-meta">' +
      bar("70%", 13) +
      bar("45%", 11, "ds-skeleton-gap") +
      "</div></div>";
  }
  return html + "</div>";
}

function commentRow(): string {
  return (
    '<div class="comment-row"><span class="notification-figure"><span class="notification-thumbnail">' +
    '<span class="ds-skeleton-fill"></span></span></span><div class="comment-row-body">' +
    bar("45%", 13) +
    bar("90%", 13, "ds-skeleton-gap") +
    bar("60%", 11, "ds-skeleton-gap") +
    "</div></div>"
  );
}

function gridHeader(label: string): string {
  return '<div class="grid-header">' + bar(label, 16) + (window.innerWidth > 600 ? bar("170px", 22) : "") + "</div>";
}

// The card over a person's, a community's or a tag's page: a name and a
// handle, then for a person a row of chips, for a community a line of
// description and its owner and counts as chips, and for a tag its count
// alone and one button.
function identityCard(kind: "person" | "community" | "tag"): string {
  const community = kind === "community";
  const tag = kind === "tag";
  const chip = community ? 30 : 24;
  return (
    '<section class="profile-card ds-card"><div class="profile-identity">' +
    bar("220px", 26) +
    bar("160px", community ? 16 : 13, "ds-skeleton-gap") +
    (community ? bar("60%", 18, "ds-skeleton-gap-l") : "") +
    (tag
      ? ""
      : '<div class="ds-skeleton-row">' +
        bar("72px", chip, "ds-skeleton-chip") +
        bar("96px", chip, "ds-skeleton-chip") +
        "</div>") +
    '</div><div class="profile-actions">' +
    bar("96px", 30, "ds-skeleton-button") +
    (tag ? "" : bar("80px", 30, "ds-skeleton-button")) +
    "</div></section>"
  );
}

// The pieces every page of drawings and comments is made of, in the layouts
// those pages share: Home's feeds, a community, a tag. The header of each --
// its pill, and on a page of drawings the Per row control after it -- then
// either the drawings with the comments beside them
// (comments_aside_macro.jinja, `.feed-layout`), or the comments alone as
// cards (comments_fragment.jinja). Home's pills sit in `.feed-choices`
// (feed_choices.jinja), which spaces them, and stand as tall as they are; a
// community's and a tag's are one pill in a `.grid-header`.
function pillHeader(kind: "feed-header" | "grid-header", pill: string, perRowControl: boolean): string {
  const pills = kind === "feed-header" ? '<div class="feed-choices">' + bar(pill, 32) + "</div>" : bar(pill, 22);
  return (
    '<div class="' + kind + '">' + pills + (perRowControl && window.innerWidth > 600 ? bar("170px", 22) : "") + "</div>"
  );
}

function drawingsBesideComments(section: string, header: string): string {
  let comments =
    '<aside class="feed-comments">' +
    bar("90px", 16, "feed-comments-title") +
    '<div class="feed-comments-panel"><div class="feed-comments-list">';
  for (let i = 0; i < 6; i++) comments += commentRow();
  return (
    '<section class="' +
    section +
    ' feed-layout has-comments">' +
    header +
    comments +
    "</div></div></aside>" +
    postGrid(3, true) +
    "</section>"
  );
}

function commentCards(section: string, header: string): string {
  let html = '<section class="' + section + '">' + header + '<div class="comment-grid">';
  for (let i = 0; i < 8; i++) html += commentRow();
  return html + "</div></section>";
}

// Home's own: the feed switch and the drawings | comments pill.
function home(inner: string): string {
  return '<div class="center-wide">' + inner + "</div>";
}

// A person's, a community's or a tag's: its card over the rest.
function underCard(kind: "person" | "community" | "tag", inner: string): string {
  return '<div class="profile">' + identityCard(kind) + inner + "</div>";
}

/**
 * Pages a link from inside another page lays a skeleton down for, as well as
 * the toolbar's. Each is one pill's half of a page reached from cards and
 * bylines across the site, which lands a long way from where the reader was.
 */
export const FROM_ANYWHERE =
  /^\/(((following|joined)\/)?comments|communities\/@[^/]+(\/comments)?|tags\/[^/]+(\/comments)?)$/;

const SHAPES: [RegExp, () => string][] = [
  [/^\/(following|joined)?$/, () => home(drawingsBesideComments("", pillHeader("feed-header", "260px", true)))],
  [/^\/((following|joined)\/)?comments$/, () => home(commentCards("feed-layout", pillHeader("feed-header", "260px", false)))],
  [
    /^\/communities\/@[^/]+$/,
    () => underCard("community", drawingsBesideComments("profile-section", pillHeader("grid-header", "130px", true))),
  ],
  [
    /^\/communities\/@[^/]+\/comments$/,
    () => underCard("community", commentCards("profile-section", pillHeader("grid-header", "130px", false))),
  ],
  [
    /^\/tags\/[^/]+$/,
    () => underCard("tag", drawingsBesideComments("profile-section", pillHeader("grid-header", "130px", true))),
  ],
  [
    /^\/tags\/[^/]+\/comments$/,
    () => underCard("tag", commentCards("profile-section", pillHeader("grid-header", "130px", false))),
  ],
  // The tag directory, whose cards are the community directory's.
  [
    /^\/tags$/,
    () => {
      let html =
        '<div class="center-wide communities-page"><div class="communities-bar">' +
        bar("300px", 30, "ds-skeleton-button") +
        bar("240px", 30, "ds-skeleton-button") +
        '</div><div class="community-grid">';
      for (let i = 0; i < 8; i++) {
        html +=
          '<div class="community-card"><span class="community-card-cover"><span class="ds-skeleton-fill"></span></span>' +
          '<span class="community-card-body">' +
          bar("55%", 15) +
          bar("35%", 12, "ds-skeleton-gap") +
          "</span></div>";
      }
      return html + "</div></div>";
    },
  ],
  // The community directory: its bar, then cards with a cover strip.
  [
    /^\/communities$/,
    () => {
      let html =
        '<div class="center-wide communities-page"><div class="communities-bar">' +
        bar("220px", 30, "ds-skeleton-button") +
        bar("240px", 30, "ds-skeleton-button") +
        '</div><div class="community-grid">';
      for (let i = 0; i < 8; i++) {
        html +=
          '<div class="community-card"><span class="community-card-cover"><span class="ds-skeleton-fill"></span></span>' +
          '<span class="community-card-body">' +
          bar("65%", 15) +
          bar("35%", 12, "ds-skeleton-gap") +
          bar("80%", 11, "ds-skeleton-gap") +
          "</span></div>";
      }
      return html + "</div></div>";
    },
  ],
  // A search: its title and its own field, then the drawings it matched. It
  // used to fall through to the column of text below, which was a page of
  // the wrong shape entirely -- the grid landed on lines. A search reached
  // from the bar's field is the commonest way here now, so it is worth its
  // own.
  [
    /^\/search$/,
    () =>
      '<div class="center-wide communities-page search-page">' +
      bar("120px", 26) +
      '<div class="communities-bar ds-skeleton-gap-l">' +
      bar("300px", 30, "ds-skeleton-button") +
      bar("80px", 30, "ds-skeleton-button") +
      "</div>" +
      '<section class="profile-section">' +
      gridHeader("100px") +
      postGrid(2) +
      "</section></div>",
  ],
  // A person, drafts: the identity card, then the grid.
  [
    /^\/@[^/]+$/,
    () => underCard("person", '<section class="profile-section">' + gridHeader("160px") + postGrid(2, true) + "</section>"),
  ],
  [
    /^\/posts\/drafts$/,
    () =>
      '<div class="profile"><section class="profile-section"><div class="grid-header"><div>' +
      bar("120px", 26) +
      bar("80px", 13, "ds-skeleton-gap") +
      "</div></div>" +
      postGrid(2) +
      "</section></div>",
  ],
];

/** The skeleton of the page at `path`, not yet in the document. */
export function skeletonFor(path: string): HTMLDivElement {
  const skeleton = document.createElement("div");
  skeleton.className = "ds-skeleton";
  skeleton.setAttribute("aria-busy", "true");
  for (let i = 0; i < SHAPES.length; i++) {
    if (SHAPES[i][0].test(path)) {
      skeleton.innerHTML = SHAPES[i][1]();
      return skeleton;
    }
  }
  // Anything else is a column of text: a title and its lines.
  let html = '<div class="center">' + bar("200px", 24);
  for (let j = 0; j < 6; j++) html += bar(j % 3 === 2 ? "60%" : "100%", 14, "ds-skeleton-gap-l");
  skeleton.innerHTML = html + "</div>";
  return skeleton;
}
