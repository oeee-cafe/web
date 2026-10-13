import { useState } from "react";
import { Trans, useLingui } from "@lingui/react/macro";
import { NEO_BUTTON, NEO_FIELD } from "neo-cucumber";
import { ModalWrapper } from "./ModalWrapper";

/** `guest::NICKNAME_MAX_CHARS` on the server. */
export const NICKNAME_MAX_CHARS = 30;

export interface GuestNameModalProps {
  isOpen: boolean;
  /** What to start the field on: a Discord name the app knows, say. */
  suggestion?: string;
  /** Whether the room may be watched, which this dialog then offers. */
  canWatch: boolean;
  /** The visit already asked to watch (`?watch=1`): only that is offered. */
  watchOnly: boolean;
  /**
   * Answers with an error to show, or null once the name is taken. `watch`
   * is whether they chose to watch rather than draw.
   */
  onSubmit: (nickname: string, watch: boolean) => Promise<string | null>;
  /** Where signing in returns to: this room. */
  signInHref: string;
}

/**
 * Asked of somebody who is not signed in, before they come into a room that
 * takes guests: what to call them. The name goes on their cursor and their
 * chat, marked as a guest's. Signing in instead is one link away, and signing
 * in later keeps whatever they drew as a guest.
 */
export const GuestNameModal = ({
  isOpen,
  suggestion,
  canWatch,
  watchOnly,
  onSubmit,
  signInHref,
}: GuestNameModalProps) => {
  const { t } = useLingui();
  const [nickname, setNickname] = useState(suggestion ?? "");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const submit = async (event: React.FormEvent, watch = watchOnly) => {
    event.preventDefault();
    const name = nickname.trim();
    if (!name || submitting) return;
    setSubmitting(true);
    setError(await onSubmit(name, watch));
    setSubmitting(false);
  };

  return (
    <ModalWrapper isOpen={isOpen} title={<Trans>Join as a Guest</Trans>} className="max-w-md">
      <form onSubmit={submit} className="flex flex-col gap-[8px] text-left">
        <label htmlFor="guest-nickname" className="text-[12px]">
          <Trans>What should the others call you?</Trans>
        </label>
        <input
          id="guest-nickname"
          type="text"
          value={nickname}
          onChange={(event) => setNickname(event.target.value)}
          maxLength={NICKNAME_MAX_CHARS}
          autoComplete="nickname"
          autoFocus
          required
          placeholder={t`Your name`}
          className={`${NEO_FIELD} px-[4px] py-[3px] text-[12px]`}
        />
        <p className="text-[11px] opacity-80">
          <Trans>
            You'll be shown as a guest. If you sign in later, what you drew moves to your account.
          </Trans>
        </p>
        {error && (
          <p role="alert" className="text-[11px] text-(--neo-tool-text)">
            {error}
          </p>
        )}
        <div className="flex flex-wrap items-center justify-center gap-[6px]">
          <a href={signInHref} className={NEO_BUTTON}>
            <Trans>Sign In or Sign Up</Trans>
          </a>
          {canWatch && !watchOnly && (
            <button
              type="button"
              onClick={(event) => void submit(event, true)}
              disabled={submitting || !nickname.trim()}
              className={`${NEO_BUTTON} disabled:cursor-not-allowed`}
            >
              <Trans>Just Watch</Trans>
            </button>
          )}
          <button type="submit" disabled={submitting || !nickname.trim()} className={`${NEO_BUTTON} disabled:cursor-not-allowed`}>
            {watchOnly ? <Trans>Watch</Trans> : <Trans>Join</Trans>}
          </button>
        </div>
      </form>
    </ModalWrapper>
  );
};
