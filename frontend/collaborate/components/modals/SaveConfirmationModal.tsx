import { Trans } from "@lingui/react/macro";
import { NEO_BUTTON } from "neo-cucumber";
import { ModalWrapper } from "./ModalWrapper";

export interface SaveConfirmationModalProps {
  isOpen: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Asked before the owner saves.
 *
 * Saving is not "keep a copy": it posts the drawing and ends the session for
 * everybody in it, and the button that does it sits in the toolbox beside the
 * drawing tools, a stray tap away from whatever the owner meant to press. The
 * wording says what happens to the other participants, because that is the
 * part nobody can undo.
 */
export const SaveConfirmationModal = ({
  isOpen,
  onConfirm,
  onCancel,
}: SaveConfirmationModalProps) => {
  return (
    <ModalWrapper
      isOpen={isOpen}
      title={<Trans>Save to Gallery</Trans>}
      className="max-w-md"
      zIndex="z-[99999]"
      onBackdropClick={onCancel}
    >
      <p className="mb-[12px]">
        <Trans>
          Save this drawing to the gallery? The session ends for everyone and
          nobody can keep drawing on it afterwards.
        </Trans>
      </p>
      <div className="flex justify-center gap-[6px]">
        <button type="button" onClick={onCancel} className={NEO_BUTTON}>
          <Trans>Cancel</Trans>
        </button>
        <button type="button" onClick={onConfirm} className={NEO_BUTTON}>
          <Trans>Save to Gallery</Trans>
        </button>
      </div>
    </ModalWrapper>
  );
};
