/**
 * What a session is, above what it recorded: its title, whose it is, where it
 * was posted from, who could see it, and what became of it.
 *
 * None of this is in the recording. It comes from the database, so the page
 * can say it even for a session that recorded nothing.
 */

import { el, type SessionDetails } from "./dom";

/** A database timestamp as stored: local time, with no zone attached. */
function stamp(value: string): string {
  return value.replace("T", " ").slice(0, 16);
}

function link(text: string, href: string, className?: string): HTMLAnchorElement {
  const node = el("a", className, text);
  node.href = href;
  return node;
}

export type Header = {
  root: HTMLElement;
  /** Where the page's own links and switches go. */
  links: HTMLElement;
  setDetails(details: SessionDetails | null, unavailable?: string): void;
};

export function header(sessionId: string): Header {
  const root = el("header", "inspect-header");
  const top = el("div", "inspect-header-top");
  const back = link("← Sessions", "/admin/collaborative-sessions", "ds-button ds-button-quiet ds-button-small");
  const title = el("h1", "inspect-title", `Session ${sessionId.slice(0, 8)}`);
  const links = el("span", "inspect-links");
  top.append(back, title, links);
  const meta = el("p", "ds-help inspect-meta");
  root.append(top, meta);

  return {
    root,
    links,
    setDetails(details, unavailable) {
      meta.textContent = "";
      if (!details) {
        if (unavailable) meta.appendChild(el("span", "inspect-muted", unavailable));
        return;
      }
      const session = details.session;
      title.textContent = "";
      title.append(
        el("span", session.title ? undefined : "inspect-muted", session.title || "untitled"),
        el("span", "inspect-title-id", sessionId),
      );
      document.title = `${session.title || "untitled"} · session`;

      const parts: HTMLElement[] = [];
      const owner = el("span");
      owner.append(document.createTextNode("by "), link(`@${session.owner_login_name}`, `/@${session.owner_login_name}`));
      parts.push(owner);
      if (session.community_slug) {
        const community = el("span");
        community.append(
          document.createTextNode("in "),
          link(session.community_name ?? session.community_slug, `/admin/communities/${session.community_slug}/posts`),
        );
        if (session.community_visibility && session.community_visibility !== "public") {
          community.append(" ", el("span", "admin-tag", session.community_visibility));
        }
        parts.push(community);
      } else {
        parts.push(el("span", "inspect-muted", "personal"));
      }
      parts.push(
        session.is_public ? el("span", undefined, "public") : el("span", "admin-tag", "link only"),
      );
      parts.push(el("span", undefined, `${session.width}×${session.height}`));
      parts.push(
        el(
          "span",
          undefined,
          `${session.active_participant_count} / ${session.max_participants} seats` +
            (session.total_participant_count > session.active_participant_count
              ? ` (${session.total_participant_count} ever)`
              : ""),
        ),
      );
      parts.push(el("span", undefined, `created ${stamp(session.created_at)}`));
      if (session.ended_at) {
        parts.push(el("span", undefined, `ended ${new Date(session.ended_at).toISOString().slice(0, 16).replace("T", " ")} UTC`));
      } else {
        parts.push(el("span", "admin-tag inspect-tag-live", "live"));
        // The ordinary room link and nothing more convenient: joining takes a
        // seat and puts a layer in the drawing.
        parts.push(link("open room →", `/collaborate/${session.id}`));
      }
      if (session.saved_post_id) {
        parts.push(link("saved post →", `/@${session.owner_login_name}/${session.saved_post_id}`));
      }
      parts.forEach((part, index) => {
        if (index > 0) meta.appendChild(document.createTextNode("  ·  "));
        meta.appendChild(part);
      });
    },
  };
}
