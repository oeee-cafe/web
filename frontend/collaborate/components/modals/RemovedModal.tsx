import { Trans } from "@lingui/react/macro";
import { NEO_BUTTON } from "neo-cucumber";
import { ModalWrapper } from "./ModalWrapper";

export interface RemovedModalProps {
  isOpen: boolean;
  onDownloadPNG: () => void;
  onGoToLobby: () => void;
}

/**
 * The owner sent this person out of the session. The server will not take
 * them back into it, so there is no reconnecting -- only the canvas as it
 * stood, theirs to keep a picture of.
 */
export const RemovedModal = ({ isOpen, onDownloadPNG, onGoToLobby }: RemovedModalProps) => (
  <ModalWrapper isOpen={isOpen} title={<Trans>Removed from Session</Trans>} className="max-w-md" zIndex="z-[999999]">
    <p className="mb-[12px]">
      <Trans>The owner removed you from this session, and you can't rejoin it.</Trans>
    </p>
    <div className="flex flex-wrap justify-center gap-[6px]">
      <button type="button" onClick={onDownloadPNG} className={NEO_BUTTON}>
        <Trans>Save as PNG</Trans>
      </button>
      <button type="button" onClick={onGoToLobby} className={NEO_BUTTON}>
        <Trans>Return to Lobby</Trans>
      </button>
    </div>
  </ModalWrapper>
);
