import { Trans } from "@lingui/react/macro";
import { NEO_BUTTON } from "neo-cucumber";
import { ModalWrapper } from "./ModalWrapper";

export interface PausedModalProps {
  isOpen: boolean;
  /** Comes back into the room, which puts it back where it stood. */
  onResume: () => void;
  onGoToLobby: () => void;
}

/**
 * The room was paused after half an hour without drawing (`collaborate::pause`
 * on the server). Nothing is lost: it is kept for a week, and coming back --
 * here, or from the lobby, by anybody who could come in -- picks it up exactly
 * where it stood.
 */
export const PausedModal = ({ isOpen, onResume, onGoToLobby }: PausedModalProps) => (
  <ModalWrapper isOpen={isOpen} title={<Trans>Session Paused</Trans>} className="max-w-md" zIndex="z-[999999]">
    <p className="mb-[12px]">
      <Trans>
        Nobody drew for half an hour, so this session was paused. The drawing is kept for 7 days, and
        anyone in the session can pick it back up.
      </Trans>
    </p>
    <div className="flex flex-wrap justify-center gap-[6px]">
      <button type="button" onClick={onResume} className={NEO_BUTTON}>
        <Trans>Resume</Trans>
      </button>
      <button type="button" onClick={onGoToLobby} className={NEO_BUTTON}>
        <Trans>Return to Lobby</Trans>
      </button>
    </div>
  </ModalWrapper>
);
