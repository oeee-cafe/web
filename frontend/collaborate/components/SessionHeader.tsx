import { Trans, useLingui } from "@lingui/react/macro";
import { Icon, NEO_BUTTON, NEO_BUTTON_ON } from "neo-cucumber";
import { useState } from "react";

import { roomIsJoinable, useDiscord } from "../hooks/useDiscord";
import { DiscordInvite, DiscordMark } from "./DiscordInvite";

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
 * In the Windows app with Discord, Invite sits beside Share and opens the
 * Discord friends to ask in (DiscordInvite), when the room is one a friend
 * could enter. Share stays for everyone else.
 */
export const SessionHeader = ({
  canvasMeta,
  connectionState,
  isCatchingUp,
}: SessionHeaderProps) => {
  const { t } = useLingui();
  const discord = useDiscord();
  const [inviting, setInviting] = useState(false);
  const canInvite = discord.state !== null && roomIsJoinable();

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
    <>
    <div className="flex w-full items-center gap-[6px] px-[3px] pt-[3px] text-[11px] leading-[14px]">
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
      {canInvite && (
        <button
          type="button"
          onClick={() => setInviting((open) => !open)}
          aria-expanded={inviting}
          className={`${NEO_BUTTON} ml-auto flex shrink-0 items-center gap-[3px] ${inviting ? NEO_BUTTON_ON : ""}`}
          title={t`Invite Discord friends`}
        >
          <DiscordMark />
          <Trans>Invite</Trans>
        </button>
      )}
      <button
        type="button"
        onClick={handleShare}
        className={`${NEO_BUTTON} ${canInvite ? "" : "ml-auto"} flex shrink-0 items-center gap-[3px]`}
        title={t`Share this session`}
      >
        <Icon icon="material-symbols:upload" width={14} height={14} />
        <Trans>Share</Trans>
      </button>
    </div>
    {canInvite && inviting && <DiscordInvite discord={discord} onClose={() => setInviting(false)} />}
    </>
  );
};
