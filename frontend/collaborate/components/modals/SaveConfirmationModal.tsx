import { Trans } from "@lingui/react/macro";
import { NEO_BUTTON } from "neo-cucumber";
import { ModalWrapper } from "./ModalWrapper";
import { SaveDestination, type SaveDestinationProps } from "../SaveDestination";

export interface SaveConfirmationModalProps {
  isOpen: boolean;
  /**
   * A guest owner's save: the drawing goes into this device's drafts rather
   * than the gallery, to be posted once they sign in.
   */
  toDevice?: boolean;
  /** Where the gallery save posts, and the owner's choice of it. */
  destination?: SaveDestinationProps;
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
  toDevice = false,
  destination,
  onConfirm,
  onCancel,
}: SaveConfirmationModalProps) => {
  return (
    <ModalWrapper
      isOpen={isOpen}
      title={toDevice ? <Trans>Keep on This Device</Trans> : <Trans>Save</Trans>}
      className="max-w-md"
      zIndex="z-[99999]"
      onBackdropClick={onCancel}
    >
      <p className="mb-[12px]">
        {toDevice ? (
          <Trans>
            Keep this drawing on this device? The session ends for everyone and
            nobody can keep drawing on it afterwards. Sign in to post it from
            your drafts.
          </Trans>
        ) : (
          <Trans>
            Save this drawing to the gallery? The session ends for everyone and
            nobody can keep drawing on it afterwards.
          </Trans>
        )}
      </p>
      {!toDevice && destination && <SaveDestination {...destination} />}
      <div className="flex justify-center gap-[6px]">
        <button type="button" onClick={onCancel} className={NEO_BUTTON}>
          <Trans>Cancel</Trans>
        </button>
        <button type="button" onClick={onConfirm} className={NEO_BUTTON} data-haptic="medium">
          {toDevice ? <Trans>Keep on This Device</Trans> : <Trans>Save</Trans>}
        </button>
      </div>
    </ModalWrapper>
  );
};
