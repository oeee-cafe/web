import { useState, useEffect, useRef, useCallback } from "react";
import { NEO_BUTTON, NEO_FIELD, NEO_WELL } from "neo-cucumber";
import { encodeChat } from "../binaryProtocol";
import { say } from "../../shared/siteDialog";
import { Trans, useLingui } from "@lingui/react/macro";

const getUserColors = (username: string) => {
  let hash = 5381;
  for (const character of username) hash = ((hash << 5) + hash) + character.charCodeAt(0);
  const hue = Math.abs(hash % 360);
  return {
    textColor: `hsl(${hue}, 75%, 95%)`,
    backgroundColor: `hsl(${hue}, 75%, 35%)`,
  };
};

interface ChatMessage {
  id: string;
  type: "user" | "system" | "join" | "leave";
  userId: string;
  username: string;
  message: string;
  timestamp: number;
}

interface Participant {
  userId: string;
  username: string;
  joinedAt: number;
}

interface ChatProps {
  wsRef: React.RefObject<WebSocket | null>;
  userId: string;
  participants: Map<string, Participant>;
  connectionState: "connecting" | "connected" | "disconnected";
  onChatMessage: (message: ChatMessage) => void;
  onAddMessage?: (
    addMessageFn: (message: {
      id: string;
      type: "join" | "leave" | "user";
      userId: string;
      username: string;
      message: string;
      timestamp: number;
    }) => void
  ) => void;
}

export const Chat = ({
  wsRef,
  userId,
  participants,
  connectionState,
  onChatMessage,
  onAddMessage,
}: ChatProps) => {
  const { t } = useLingui();

  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [inputValue, setInputValue] = useState("");
  const [isComposing, setIsComposing] = useState(false);
  const [unreadCount, setUnreadCount] = useState(0);
  const messagesRef = useRef<HTMLDivElement>(null);
  const isNearBottomRef = useRef(true);
  const inputRef = useRef<HTMLInputElement>(null);

  /*
   * Auto-scroll to bottom when new messages arrive.
   *
   * Scroll the log itself rather than asking the browser to bring the last
   * message into view: scrollIntoView walks every scrollable ancestor, so a
   * new message -- or the focus that follows sending one -- dragged the page
   * down with it and took the canvas along.
   */
  const scrollToBottom = useCallback(() => {
    const element = messagesRef.current;
    if (!element) return;
    element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
  }, []);

  // Handle incoming chat messages
  const addMessage = useCallback(
    (message: ChatMessage) => {
      setMessages((previous) => {
        // Recent chat replay overlaps the live PubSub stream by design. The
        // protocol message id makes that overlap harmless on reconnect.
        if (previous.some((entry) => entry.id === message.id)) return previous;
        return [...previous, message].slice(-200);
      });
      if (isNearBottomRef.current) {
        window.requestAnimationFrame(scrollToBottom);
      } else {
        setUnreadCount((count) => count + 1);
      }
      onChatMessage(message);
    },
    [onChatMessage, scrollToBottom]
  );

  // Expose addMessage to parent component via callback
  useEffect(() => {
    if (onAddMessage) {
      onAddMessage(addMessage);
    }
  }, [onAddMessage, addMessage]);

  // Send chat message
  const sendMessage = useCallback(() => {
    const ws = wsRef.current;
    if (connectionState !== "connected" || !ws || ws.readyState !== WebSocket.OPEN || !inputValue.trim()) {
      return;
    }

    try {
      const message = inputValue.trim();
      if (message.length > 500) {
        say(t`Message too long (max 500 characters)`);
        return;
      }

      const binaryMessage = encodeChat(userId, message, Date.now());
      ws.send(binaryMessage);

      setInputValue("");
      // Keep the caret in the box without letting the browser scroll the page
      // to put it there: the box has not moved, and the painter behind it must
      // not either.
      inputRef.current?.focus({ preventScroll: true });
    } catch (error) {
      console.error("Failed to send chat message:", error);
    }
  }, [connectionState, wsRef, userId, inputValue, t]);

  const handleMessagesScroll = useCallback(() => {
    const element = messagesRef.current;
    if (!element) return;
    isNearBottomRef.current = element.scrollHeight - element.scrollTop - element.clientHeight < 24;
    if (isNearBottomRef.current) setUnreadCount(0);
  }, []);

  const showNewestMessages = useCallback(() => {
    isNearBottomRef.current = true;
    setUnreadCount(0);
    scrollToBottom();
  }, [scrollToBottom]);

  // Handle IME composition events
  const handleCompositionStart = useCallback(() => {
    setIsComposing(true);
  }, []);

  const handleCompositionEnd = useCallback(() => {
    setIsComposing(false);
  }, []);

  // Handle input focus for mobile
  const handleInputFocus = useCallback(() => {
    // Give the soft keyboard time to appear, then move the input only if it
    // ended up out of view. `center` moved something on every focus, and
    // sending a message focuses the input again -- which is how typing in the
    // chat scrolled the whole app down.
    setTimeout(() => {
      inputRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
    }, 300);
  }, []);

  // Handle Enter key
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter" && !e.shiftKey && !isComposing) {
        e.preventDefault();
        sendMessage();
      }
    },
    [sendMessage, isComposing]
  );

  // Format timestamp
  const formatTime = (timestamp: number) => {
    return new Date(timestamp).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    });
  };

  // Generate unique color and background for participant based on username
  const getUserStyle = (username: string) => {
    const colors = getUserColors(username);
    return {
      color: colors.textColor,
      backgroundColor: colors.backgroundColor,
      padding: "1px 4px",
      borderRadius: "3px",
      fontSize: "inherit",
      fontWeight: "bold",
      // Override any inherited styles
      borderColor: colors.backgroundColor,
    };
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-[3px] p-[3px] touch-auto">
      <>
          <div className="flex w-full flex-wrap items-center gap-[3px]">
            <span
              title={t`Participants (${participants.size})`}
              aria-label={t`Participants (${participants.size})`}
              className={`${NEO_WELL} px-[3px] text-[11px] leading-[14px]`}
            >
              {participants.size}
            </span>
            {Array.from(participants.values())
              .sort((a, b) => a.joinedAt - b.joinedAt)
              .map((participant) => (
                <div
                  key={participant.userId}
                  className="items-center text-[11px] leading-[14px]"
                  style={getUserStyle(participant.username)}
                >
                  {participant.username}
                </div>
              ))}
          </div>
          <div ref={messagesRef} onScroll={handleMessagesScroll} className={`${NEO_WELL} chat-transcript relative min-h-0 flex-1 overflow-y-auto p-[3px] text-[11px] leading-[15px]`}>
            <div>
              {messages.map((msg) => (
                <div
                  key={msg.id}
                  className={`mb-[2px] px-[3px] py-[1px] ${
                    msg.type === "system" ||
                    msg.type === "join" ||
                    msg.type === "leave"
                      ? "mb-[1px]"
                      : "border-b border-(--neo-bk2)"
                  } last:border-b-0`}
                >
                  {msg.type === "system" ||
                  msg.type === "join" ||
                  msg.type === "leave" ? (
                    <div className="text-[11px] leading-[14px] italic opacity-80">
                      {msg.type === "join"
                        ? `${msg.username} joined`
                        : msg.type === "leave"
                        ? `${msg.username} left the session`
                        : msg.message}
                    </div>
                  ) : (
                    <div className="flex items-baseline gap-1 text-[11px] leading-[14px]">
                      <span className="shrink-0" style={getUserStyle(msg.username)}>
                        {msg.username}
                      </span>
                      <span className="shrink-0 text-[10px] leading-[12px] opacity-70">
                        {formatTime(msg.timestamp)}
                      </span>
                      <span className="min-w-0 break-words">
                        {msg.message}
                      </span>
                    </div>
                  )}
                </div>
              ))}
            </div>
            {unreadCount > 0 && (
              <button type="button" onClick={showNewestMessages} className={`${NEO_BUTTON} sticky bottom-1 left-1/2 -translate-x-1/2 text-[11px] shadow`}>
                {unreadCount} new {unreadCount === 1 ? "message" : "messages"}
              </button>
            )}
          </div>
          <div className="flex gap-[3px] pr-[20px]">
            <input
              ref={inputRef}
              type="text"
              value={inputValue}
              onChange={(e) => setInputValue(e.target.value)}
              onKeyDown={handleKeyDown}
              onCompositionStart={handleCompositionStart}
              onCompositionEnd={handleCompositionEnd}
              onFocus={handleInputFocus}
              placeholder={t`Type a message...`}
              maxLength={500}
              className={`${NEO_FIELD} min-w-0 flex-1 px-[3px] py-[2px] text-[11px] leading-[14px] font-sans focus:outline-2 focus:outline-(--neo-icon-select) focus:-outline-offset-2`}
            />
            <button
              onClick={sendMessage}
              disabled={connectionState !== "connected" || !inputValue.trim()}
              className={`${NEO_BUTTON} shrink-0 text-[11px] font-sans disabled:cursor-not-allowed`}
            >
              <Trans>Send</Trans>
            </button>
          </div>
      </>
    </div>
  );
};
