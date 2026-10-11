import { Trans, useLingui } from "@lingui/react/macro";
import { Icon, NEO_BUTTON, NEO_BUTTON_ON, PANEL_MARGIN } from "neo-cucumber";
import { useRef, useState } from "react";
import { createPortal } from "react-dom";

import { inviteSteamFriends, steamCanInvite } from "../../shared/appBridge";
import { FRIENDS_WINDOW_SIZE, roomIsJoinable, roomJoin, useDiscord } from "../hooks/useDiscord";
import { DiscordMark } from "./DiscordInvite";
import { DiscordFriendsWindow, type WindowOrigin } from "./DiscordFriendsWindow";

interface CollaborationMeta {
  title: string;
  width: number;
  height: number;
  ownerId: string;
  savedPostId?: string;
  ownerLoginName: string;
  maxUsers: number;
  currentUserCount: number;
}

export interface SessionHeaderProps {
  canvasMeta: CollaborationMeta;
  connectionState: "connecting" | "connected" | "disconnected";
  isCatchingUp: boolean;
  /** The highest a window may be dragged, as the chat's own is held. */
  ceiling?: number;
}

/**
 * Where the friends window opens: beside the chat window it was asked from,
 * on whichever side has the room, level with its top.
 */
function besideChat(header: HTMLElement | null, ceiling: number): WindowOrigin {
  const chat = header?.parentElement?.getBoundingClientRect();
  if (!chat) return { x: PANEL_MARGIN, y: Math.max(PANEL_MARGIN, ceiling) };
  const { width, height } = FRIENDS_WINDOW_SIZE;
  const right = chat.right + PANEL_MARGIN;
  const x = right + width <= window.innerWidth ? right : Math.max(0, chat.left - PANEL_MARGIN - width);
  const y = Math.max(ceiling, Math.min(chat.top, window.innerHeight - height));
  return { x, y };
}

/**
 * The session, at the top of the chat window.
 *
 * This was a bar across the page above the canvas, which the site's toolbar
 * now makes a second title bar: the way home is in the toolbar, and what is
 * left -- whose session it is, whether you are connected, and the link to
 * share -- belongs to the window where the session's people are. The title
 * itself is in that window's title bar; see App.
 *
 * It sits below the title bar rather than in it because the title bar is the
 * handle the window is dragged by, and Share has to stay a button.
 *
 * The buttons have a row of their own under who and whether: three of them
 * beside the status and the owner's name squeezed both into ellipses.
 *
 * In the Windows app with Discord, Invite sits beside Share and opens the
 * Discord friends to ask in, in a window of their own beside the chat
 * (DiscordFriendsWindow), when the room is one a friend could enter. Share
 * stays for everyone else. The window is put on the page rather than in the
 * chat, whose frame clips what it holds and is moved by a transform while it
 * is dragged.
 *
 * In the Steam build, Steam's own Invite sits there too: it opens Steam's
 * invitation over the window, where Steam lists the friends, and a friend
 * who accepts has Steam open their app on the room. Only friends with
 * Oeee Cafe on Steam can come in that way.
 */
export const SessionHeader = ({
  canvasMeta,
  connectionState,
  isCatchingUp,
  ceiling = 0,
}: SessionHeaderProps) => {
  const { t } = useLingui();
  const discord = useDiscord();
  // Where the friends window opened, while it is open.
  const [inviting, setInviting] = useState<WindowOrigin | null>(null);
  const headerRef = useRef<HTMLDivElement>(null);
  const canInvite = discord.state !== null && roomIsJoinable();
  const steamJoin = steamCanInvite() ? roomJoin() : null;

  const handleShare = () => {
    if (navigator.share) {
      navigator
        .share({
          title: canvasMeta.title,
          url: window.location.href,
        })
        .catch(console.error);
    } else {
      navigator.clipboard
        .writeText(window.location.href)
        .then(() => {
          // Could show a toast notification here
          console.log("URL copied to clipboard");
        })
        .catch(console.error);
    }
  };

  return (
    <div ref={headerRef} className="flex w-full flex-col gap-[4px] px-[3px] pt-[3px] text-[11px] leading-[14px]">
    <div className="flex min-w-0 items-center gap-[6px]">
      {connectionState === "connected" && !isCatchingUp && (
        <div className="flex shrink-0 items-center gap-[3px] opacity-80">
          <div className="h-[6px] w-[6px] bg-green-500 rounded-full"></div>
          <Trans>Connected</Trans>
        </div>
      )}
      {connectionState === "connecting" && (
        <div className="flex shrink-0 items-center gap-[3px] opacity-80">
          <div className="h-[6px] w-[6px] bg-yellow-500 rounded-full animate-pulse"></div>
          <Trans>Connecting</Trans>
        </div>
      )}
      {connectionState === "disconnected" && !isCatchingUp && (
        <div className="flex shrink-0 items-center gap-[3px] opacity-80">
          <div className="h-[6px] w-[6px] bg-red-500 rounded-full"></div>
          <Trans>Disconnected</Trans>
        </div>
      )}
      {isCatchingUp && (
        <div className="flex shrink-0 items-center gap-[3px] opacity-80">
          <div className="h-[6px] w-[6px] bg-blue-500 rounded-full animate-pulse"></div>
          <Trans>Loading</Trans>
        </div>
      )}
      <div className="min-w-0 truncate opacity-70">
        <Trans>by</Trans> @{canvasMeta.ownerLoginName}
      </div>
    </div>
    <div className="flex flex-wrap items-center justify-end gap-[4px]">
      {canInvite && (
        <button
          type="button"
          onClick={() => setInviting((open) => (open ? null : besideChat(headerRef.current, ceiling)))}
          aria-expanded={inviting !== null}
          className={`${NEO_BUTTON} flex shrink-0 items-center gap-[4px] ${inviting ? NEO_BUTTON_ON : ""}`}
          title={t`Invite Discord friends`}
        >
          <DiscordMark />
          <Trans>Invite</Trans>
        </button>
      )}
      {steamJoin && (
        <button
          type="button"
          onClick={() => inviteSteamFriends(steamJoin)}
          className={`${NEO_BUTTON} flex shrink-0 items-center gap-[4px]`}
          title={t`Invite Steam friends`}
        >
          <Icon icon="material-symbols:group-add" width={14} height={14} />
          Steam
        </button>
      )}
      <button
        type="button"
        onClick={handleShare}
        className={`${NEO_BUTTON} flex shrink-0 items-center gap-[4px]`}
        title={t`Share this session`}
      >
        <Icon icon="material-symbols:upload" width={14} height={14} />
        <Trans>Share</Trans>
      </button>
    </div>
    {canInvite &&
      inviting &&
      createPortal(
        <DiscordFriendsWindow
          discord={discord}
          origin={inviting}
          minimumY={ceiling}
          onClose={() => setInviting(null)}
        />,
        document.body,
      )}
    </div>
  );
};
