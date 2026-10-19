import { Trans } from "@lingui/react/macro";
import { NEO_BUTTON } from "neo-cucumber";
import { useEffect } from "react";

import { askDiscord } from "../../shared/appBridge";
import type { Discord } from "../hooks/useDiscord";
import { FRIENDS_PANEL, FriendList } from "./FriendList";

/** Discord's own symbol, in its colour, as its branding page gives it. */
export const DiscordMark = () => (
  <img src="/static/signin/discord-mark-blurple.svg" width={13} height={10} alt="" className="shrink-0" />
);

export interface DiscordInviteProps {
  discord: Discord;
  onClose: () => void;
}

/**
 * Inviting Discord friends into the room, filling the friends window
 * (FriendsWindow): why to connect the first time, then the friends, those in
 * Oeee Cafe first. An invitation is a message in Discord with the room's
 * Join button, sent as Discord's own + would send it.
 */
export const DiscordInvite = ({ discord, onClose }: DiscordInviteProps) => {
  const { state, friends, invitations, invite } = discord;
  const connected = state?.connected ?? false;

  // The friends, asked for each time the picker opens and connected: the
  // app sends them again as they change, for as long as the picker is open.
  useEffect(() => {
    if (connected) askDiscord("friends");
  }, [connected]);

  if (!state) return null;

  if (!connected) {
    return (
      <div className={FRIENDS_PANEL}>
        <div className="mb-[4px] font-bold">
          <Trans>Invite Discord friends</Trans>
        </div>
        {state.connecting ? (
          <div className="mb-[8px]">
            <Trans>Connecting to Discord…</Trans>
          </div>
        ) : (
          <div className="mb-[8px]">
            <Trans>Connect Discord once to see your friends here and invite them to this session.</Trans>
          </div>
        )}
        <div className="flex gap-[3px]">
          <button
            type="button"
            disabled={state.connecting}
            onClick={() => askDiscord("connect")}
            className={`${NEO_BUTTON} flex items-center gap-[3px]`}
          >
            <DiscordMark />
            <Trans>Connect Discord</Trans>
          </button>
          <button type="button" onClick={onClose} className={NEO_BUTTON}>
            <Trans>Not now</Trans>
          </button>
        </div>
        <div className="mt-[6px] opacity-70">
          <Trans>Or use + in any Discord chat.</Trans>
        </div>
      </div>
    );
  }

  return (
    <FriendList
      friends={friends}
      invitations={invitations}
      invite={invite}
      empty={<Trans>No Discord friends to invite yet.</Trans>}
    />
  );
};
