import { Trans, useLingui } from "@lingui/react/macro";
import { NEO_BUTTON, NEO_BUTTON_ON, NEO_FIELD, NEO_WELL } from "neo-cucumber";
import { useEffect, useMemo, useState } from "react";

import { askDiscord, type DiscordFriend } from "../../shared/appBridge";
import type { Discord } from "../hooks/useDiscord";

/** Discord's own symbol, in its colour, as its branding page gives it. */
export const DiscordMark = () => (
  <img src="/static/signin/discord-mark-blurple.svg" width={13} height={10} alt="" className="shrink-0" />
);

const STATUS_DOT: Record<DiscordFriend["status"], string> = {
  online: "bg-green-500",
  idle: "bg-yellow-500",
  dnd: "bg-red-500",
  offline: "bg-gray-400",
};

export interface DiscordInviteProps {
  discord: Discord;
  onClose: () => void;
}

/**
 * Inviting Discord friends into the room, below the session header in the
 * chat window: why to connect the first time, then the friends, those in
 * Oeee Cafe first. An invitation is a message in Discord with the room's
 * Join button, sent as Discord's own + would send it, and once sent a
 * friend's button says so, so a second press sends nothing twice.
 */
export const DiscordInvite = ({ discord, onClose }: DiscordInviteProps) => {
  const { t } = useLingui();
  const [query, setQuery] = useState("");
  const { state, friends, invitations, invite } = discord;
  const connected = state?.connected ?? false;

  // The friends, asked for each time the picker opens and connected: the
  // app sends them again as they change, for as long as the picker is open.
  useEffect(() => {
    if (connected) askDiscord("friends");
  }, [connected]);

  const shown = useMemo(() => {
    const wanted = query.trim().toLowerCase();
    return (friends ?? []).filter((friend) => !wanted || friend.name.toLowerCase().includes(wanted));
  }, [friends, query]);

  const panel = `${NEO_WELL} mx-[3px] mt-[3px] flex max-h-[160px] min-h-0 shrink-0 flex-col p-[4px] text-[11px] leading-[15px]`;

  if (!state) return null;

  if (!connected) {
    return (
      <div className={panel}>
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
    <div className={panel}>
      <input
        type="search"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder={t`Search friends`}
        aria-label={t`Search friends`}
        className={`${NEO_FIELD} mb-[4px] w-full shrink-0 px-[3px] text-[11px] leading-[15px]`}
      />
      <ul className="min-h-0 flex-1 overflow-y-auto">
        {friends === null ? (
          <li className="opacity-70">
            <Trans>Loading friends…</Trans>
          </li>
        ) : shown.length === 0 ? (
          <li className="opacity-70">
            {query ? <Trans>No friend by that name.</Trans> : <Trans>No Discord friends to invite yet.</Trans>}
          </li>
        ) : (
          shown.map((friend) => {
            const fate = invitations[friend.id];
            return (
              <li
                key={friend.id}
                className={`flex items-center gap-[4px] py-[1px] ${friend.status === "offline" ? "opacity-60" : ""}`}
              >
                <span className={`h-[6px] w-[6px] shrink-0 rounded-full ${STATUS_DOT[friend.status]}`} />
                <span className="min-w-0 flex-1 truncate">
                  {friend.name}
                  {friend.playing && (
                    <span className="opacity-60">
                      {" · "}
                      <Trans>in Oeee Cafe</Trans>
                    </span>
                  )}
                </span>
                <button
                  type="button"
                  disabled={fate === "sending" || fate === "sent"}
                  onClick={() => invite(friend.id)}
                  className={`${NEO_BUTTON} shrink-0 !px-[5px] !py-0 ${fate === "sent" ? NEO_BUTTON_ON : ""}`}
                >
                  {fate === "sending" ? (
                    <Trans>Sending…</Trans>
                  ) : fate === "sent" ? (
                    <Trans>Sent</Trans>
                  ) : fate === "failed" ? (
                    <Trans>Try again</Trans>
                  ) : (
                    <Trans>Invite</Trans>
                  )}
                </button>
              </li>
            );
          })
        )}
      </ul>
    </div>
  );
};
