import { Trans } from "@lingui/react/macro";
import { NEO_BUTTON } from "neo-cucumber";
import { ModalWrapper } from "./ModalWrapper";

export interface RemovedModalProps {
  isOpen: boolean;
  onGoToLobby: () => void;
}

/**
 * The owner sent this person out of the session. The server will not take
 * them back into it, so there is no reconnecting, and nothing of the room to
 * take with them: the canvas is the room's, and somebody sent out of it is
 * not offered a copy.
 */
export const RemovedModal = ({ isOpen, onGoToLobby }: RemovedModalProps) => (
  <ModalWrapper isOpen={isOpen} title={<Trans>Removed from Session</Trans>} className="max-w-md" zIndex="z-[999999]">
    <p className="mb-[12px]">
      <Trans>The owner removed you from this session, and you can't rejoin it.</Trans>
    </p>
    <div className="flex flex-wrap justify-center gap-[6px]">
      <button type="button" onClick={onGoToLobby} className={NEO_BUTTON}>
        <Trans>Return to Lobby</Trans>
      </button>
    </div>
  </ModalWrapper>
);
