import { Trans, useLingui } from "@lingui/react/macro";
import { Icon, NEO_BUTTON, NEO_BUTTON_ON, NEO_CHECKBOX, PANEL_MARGIN } from "neo-cucumber";
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
  ownerIsGuest?: boolean;
  maxUsers: number;
  currentUserCount: number;
}

/** The owner's guest switch, where guests could come in at all. */
export interface GuestsSwitch {
  allowed: boolean;
  busy: boolean;
  onToggle: () => void;
}

/** Watching or drawing, for somebody who is not the owner. */
export interface ModeSwitch {
  watching: boolean;
  /** A drawing seat is free, for somebody watching to take. */
  seatFree: boolean;
  /** The owner lets people watch, for somebody drawing to sit back. */
  canWatch: boolean;
  onSwitch: () => void;
}

export interface SessionHeaderProps {
  canvasMeta: CollaborationMeta;
  /** Present for the owner, of a session whose community admits guests. */
  guests?: GuestsSwitch;
  /** Present for the owner: whether people may watch. */
  spectators?: GuestsSwitch;
  /** Present for everybody else. */
  mode?: ModeSwitch;
  /**
   * Present for a guest: where signing up from this room goes, and what to
   * do as they go -- it is a way out the room meant, not an accident.
   */
  signUp?: { href: string; onFollow: () => void };
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
 * Steam's logo, before the word on the Steam button: the path the site's
 * Sign in with Steam draws (sign_in_providers_macro.jinja), in the words'
 * own colour so it turns with the theme.
 */
const SteamMark = () => (
  <svg width={14} height={14} viewBox="0 0 24 24" aria-hidden="true" focusable="false" className="shrink-0">
    <path
      fill="currentColor"
      d="M11.979 0C5.678 0 .511 4.86.022 11.037l6.432 2.658c.545-.371 1.203-.59 1.912-.59.063 0 .125.004.188.006l2.861-4.142V8.91c0-2.495 2.028-4.524 4.524-4.524 2.494 0 4.524 2.031 4.524 4.527s-2.03 4.525-4.524 4.525h-.105l-4.076 2.911c0 .052.004.105.004.159 0 1.875-1.515 3.396-3.39 3.396-1.635 0-3.016-1.173-3.331-2.727L.436 15.27C1.862 20.307 6.486 24 11.979 24c6.627 0 11.999-5.373 11.999-12S18.605 0 11.979 0zM7.54 18.21l-1.473-.61c.262.543.714.999 1.314 1.25 1.297.539 2.793-.076 3.332-1.375.263-.63.264-1.319.005-1.949s-.75-1.121-1.377-1.383c-.624-.26-1.29-.249-1.878-.03l1.523.63c.956.4 1.409 1.5 1.009 2.455-.397.957-1.497 1.41-2.454 1.012H7.54zm11.415-9.303c0-1.662-1.353-3.015-3.015-3.015-1.665 0-3.015 1.353-3.015 3.015 0 1.665 1.35 3.015 3.015 3.015 1.663 0 3.015-1.35 3.015-3.015zm-5.273-.005c0-1.252 1.013-2.266 2.265-2.266 1.249 0 2.266 1.014 2.266 2.266 0 1.251-1.017 2.265-2.266 2.265-1.253 0-2.265-1.014-2.265-2.265z"
    />
  </svg>
);

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
 * beside the status and the owner's name squeezed both into ellipses. They
 * share the row's width, edge to edge, rather than gather at its end and
 * leave a gap where the row starts.
 *
 * In the Windows app with Discord, Invite sits beside Share and opens the
 * Discord friends to ask in, in a window of their own beside the chat
 * (DiscordFriendsWindow), when the room is one a friend could enter. Share
 * stays for everyone else. The window is put on the page rather than in the
 * chat, whose frame clips what it holds and is moved by a transform while it
 * is dragged.
 *
 * A Discord friend who pressed Ask to Join on the reader's activity is
 * listed under the buttons until they are let in or put off: Let in answers
 * their request with an invitation, as Discord's own notification would, and
 * Not now only takes them off the list.
 *
 * In the Steam build, Steam's own Invite sits there too: it opens Steam's
 * invitation over the window, where Steam lists the friends, and a friend
 * who accepts has Steam open their app on the room. Only friends with
 * Oeee Cafe on Steam can come in that way.
 */
export const SessionHeader = ({
  canvasMeta,
  guests,
  spectators,
  mode,
  signUp,
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
      {mode?.watching && (
        <div className="flex shrink-0 items-center gap-[3px] opacity-80">
          <Icon icon="material-symbols:visibility" width={12} height={12} />
          <Trans>Watching</Trans>
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
        {/* A guest's name is not a handle; there is no account behind it. */}
        {canvasMeta.ownerIsGuest ? (
          <>
            <Trans>by</Trans> {canvasMeta.ownerLoginName} <Trans>(guest)</Trans>
          </>
        ) : (
          <>
            <Trans>by</Trans> @{canvasMeta.ownerLoginName}
          </>
        )}
      </div>
    </div>
    <div className="flex items-center gap-[4px]">
      {canInvite && (
        <button
          type="button"
          onClick={() => setInviting((open) => (open ? null : besideChat(headerRef.current, ceiling)))}
          aria-expanded={inviting !== null}
          className={`${NEO_BUTTON} flex min-w-0 flex-1 items-center justify-center gap-[4px] ${inviting ? NEO_BUTTON_ON : ""}`}
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
          className={`${NEO_BUTTON} flex min-w-0 flex-1 items-center justify-center gap-[4px]`}
          title={t`Invite Steam friends`}
        >
          <SteamMark />
          Steam
        </button>
      )}
      <button
        type="button"
        onClick={handleShare}
        className={`${NEO_BUTTON} flex min-w-0 flex-1 items-center justify-center gap-[4px]`}
        title={t`Share this session`}
      >
        <Icon icon="material-symbols:upload" width={14} height={14} />
        <Trans>Share</Trans>
      </button>
    </div>
    {/* Watching or drawing: a row of its own, as the owner's permissions
        are. Beside Invite, Steam and Share it was the fourth button in a
        row the window opens too narrow for, and broke onto two lines. */}
    {signUp && (
      // A guest's way to an account without leaving the room for good: the
      // sign-up comes back here, and what they drew comes with them.
      <div
        className="min-w-0 truncate"
        title={t`Sign up and come back here: what you drew moves to your new account.`}
      >
        <Trans>You're a guest.</Trans>{" "}
        <a
          href={signUp.href}
          className="text-(--neo-text) underline"
          onClick={signUp.onFollow}
        >
          <Trans>Sign up</Trans>
        </a>
      </div>
    )}
    {mode && (mode.watching || mode.canWatch) && (
      <button
        type="button"
        onClick={mode.onSwitch}
        disabled={mode.watching && !mode.seatFree}
        className={`${NEO_BUTTON} flex w-full items-center justify-center gap-[4px] disabled:cursor-not-allowed`}
        title={
          mode.watching
            ? mode.seatFree
              ? t`Take a seat and draw`
              : t`Every seat is taken. You can draw once one frees.`
            : t`Give up your seat and watch`
        }
      >
        {mode.watching ? <Trans>Join Drawing</Trans> : <Trans>Just Watch</Trans>}
      </button>
    )}
    {(guests || spectators) && (
      // The owner's: who may come in, from here on. Unticking stops new
      // arrivals only; somebody already in the room stays.
      <div className="flex flex-wrap items-center gap-x-[8px] gap-y-[2px]">
        {guests && (
          <label
            className="flex items-center gap-[3px]"
            title={t`People without an account can join. Unticking stops new ones; guests already here stay.`}
          >
            <input
              type="checkbox"
              checked={guests.allowed}
              disabled={guests.busy}
              onChange={guests.onToggle}
              className={NEO_CHECKBOX}
            />
            <Trans>Allow guests</Trans>
          </label>
        )}
        {spectators && (
          <label
            className="flex items-center gap-[3px]"
            title={t`People can watch without drawing. Unticking stops new ones; anybody already watching stays.`}
          >
            <input
              type="checkbox"
              checked={spectators.allowed}
              disabled={spectators.busy}
              onChange={spectators.onToggle}
              className={NEO_CHECKBOX}
            />
            <Trans>Allow spectators</Trans>
          </label>
        )}
      </div>
    )}
    {canInvite &&
      discord.requests.map((request) => {
        const fate = discord.invitations[request.id];
        const name = request.name;
        return (
          <div key={request.id} role="status" className="flex min-w-0 items-center gap-[4px]">
            <DiscordMark />
            <span className="min-w-0 flex-1 truncate">
              {fate === "failed" ? (
                <Trans>Couldn’t let {name} in</Trans>
              ) : (
                <Trans>{name} asks to join</Trans>
              )}
            </span>
            <button
              type="button"
              disabled={fate === "sending"}
              onClick={() => discord.letIn(request.id)}
              className={`${NEO_BUTTON} shrink-0 !px-[5px] !py-0`}
            >
              {fate === "sending" ? (
                <Trans>Sending…</Trans>
              ) : fate === "failed" ? (
                <Trans>Try again</Trans>
              ) : (
                <Trans>Let in</Trans>
              )}
            </button>
            <button
              type="button"
              onClick={() => discord.dismiss(request.id)}
              className={`${NEO_BUTTON} shrink-0 !px-[5px] !py-0`}
            >
              <Trans>Not now</Trans>
            </button>
          </div>
        );
      })}
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
