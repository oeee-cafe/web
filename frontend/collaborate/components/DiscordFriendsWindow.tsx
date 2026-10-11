import { Trans, useLingui } from "@lingui/react/macro";
import {
  attachWindowDrag,
  attachWindowResize,
  NEO_PANEL,
  NEO_RESIZE_GRIP,
  NEO_RESIZE_HANDLE,
  NEO_TITLEBAR_DOT,
  NEO_TITLEBAR_HANDLE,
} from "neo-cucumber";
import { useEffect, useRef, useState } from "react";

import { FRIENDS_WINDOW_SIZE, type Discord } from "../hooks/useDiscord";
import { DiscordInvite } from "./DiscordInvite";

/** Where the window opens, from the top left of the viewport. */
export interface WindowOrigin {
  x: number;
  y: number;
}

export interface DiscordFriendsWindowProps {
  discord: Discord;
  origin: WindowOrigin;
  /** The highest it may be dragged: under the site's toolbar, as the chat is. */
  minimumY: number;
  onClose: () => void;
}

/**
 * The Discord friends to invite, in a window of their own beside the chat
 * rather than inside it, where they squeezed the conversation into a strip:
 * a NEO window like the chat's, dragged by its title bar and sized by its
 * corner, put away by the × in its title bar or by Invite again.
 */
export const DiscordFriendsWindow = ({ discord, origin, minimumY, onClose }: DiscordFriendsWindowProps) => {
  const { t } = useLingui();
  const [position, setPosition] = useState(origin);
  const [size, setSize] = useState(FRIENDS_WINDOW_SIZE);
  const frameRef = useRef<HTMLDivElement>(null);
  const handleRef = useRef<HTMLDivElement>(null);
  const resizeRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const frame = frameRef.current;
    const handle = handleRef.current;
    if (!frame || !handle) return;
    return attachWindowDrag(frame, handle, { minimumY, onPosition: setPosition });
  }, [minimumY]);

  useEffect(() => {
    const frame = frameRef.current;
    const corner = resizeRef.current;
    if (!frame || !corner) return;
    return attachWindowResize(frame, corner, {
      minimum: { width: 180, height: 160 },
      onSize: setSize,
    });
  }, []);

  return (
    <div
      ref={frameRef}
      role="dialog"
      aria-label={t`Invite Discord friends`}
      className={`${NEO_PANEL} fixed z-40 flex flex-col overflow-hidden shadow-lg`}
      style={{
        left: `${position.x}px`,
        top: `${position.y}px`,
        width: `${size.width}px`,
        height: `${size.height}px`,
      }}
    >
      {/* The × sits over the title bar's end but is not inside it: the
          title bar takes the pointer for the drag as it goes down
          (attachWindowDrag), listening on itself, so a button within it was
          never clicked -- and stopping the press in React came too late,
          React hearing it only once it reached the page's root. */}
      <div className="relative shrink-0">
        <div ref={handleRef} className={`${NEO_TITLEBAR_HANDLE} pr-[22px]`}>
          <span className={NEO_TITLEBAR_DOT} />
          <span className={NEO_TITLEBAR_DOT} />
          <span className={NEO_TITLEBAR_DOT} />
          <span className="ml-[4px] min-w-0 flex-1 truncate text-[12px] leading-[18px]">
            <Trans>Discord friends</Trans>
          </span>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label={t`Close`}
          className="absolute top-0 right-[2px] bottom-0 cursor-pointer px-[4px] text-[14px] leading-[16px] text-(--neo-titlebar-text)"
        >
          ×
        </button>
      </div>
      <DiscordInvite discord={discord} onClose={onClose} />
      <div ref={resizeRef} aria-hidden="true" className={NEO_RESIZE_HANDLE}>
        <span className={NEO_RESIZE_GRIP} />
      </div>
    </div>
  );
};
