import { Trans, useLingui } from "@lingui/react/macro";
import { Icon, NEO_BUTTON } from "neo-cucumber";
import { ModalWrapper } from "./modals/ModalWrapper";
import { SaveDestination, type SaveDestinationProps } from "./SaveDestination";
import { say } from "../../shared/siteDialog";

export interface SessionExpiredModalProps {
  isOpen: boolean;
  isOwner: boolean;
  canvasMeta: {
    savedPostId?: string;
  } | null;
  isSaving: boolean;
  /**
   * Whether the drawing is still here to save or download. It is when the
   * session expired under this tab; it is not when the page opened on a
   * session that was already over, whose canvas the server no longer holds
   * and this tab never had.
   */
  hasCanvas: boolean;
  /**
   * The owner ended it on purpose rather than letting it run out: a guest
   * owner who kept the drawing on their device.
   */
  endedByOwner?: boolean;
  /**
   * The owner is a guest: what they can do with it is keep it in this
   * device's drafts, not post it.
   */
  ownerKeepsOnDevice?: boolean;
  onClose: () => void;
  onSaveToGallery: () => Promise<void>;
  /** Where the gallery save posts, and the owner's choice of it. */
  destination?: SaveDestinationProps;
  onDownloadPNG: () => void;
  onReturnToLobby: () => void;
}

export const SessionExpiredModal = ({
  isOpen,
  isOwner,
  canvasMeta,
  isSaving,
  hasCanvas,
  endedByOwner = false,
  ownerKeepsOnDevice = false,
  onClose,
  onSaveToGallery,
  destination,
  onDownloadPNG,
  onReturnToLobby,
}: SessionExpiredModalProps) => {
  const { t } = useLingui();

  const handleSaveToGallery = async (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    try {
      onClose(); // Close the modal
      await onSaveToGallery();
    } catch (error) {
      // Reopen modal if save fails
      // Show appropriate error message
      if (
        error instanceof Error &&
        error.message.includes("already been saved")
      ) {
        say(
          t`This session has already been saved. You can only download it as a PNG.`
        );
      } else {
        say(
          t`Failed to save session. Please try downloading as PNG instead.`
        );
      }
    }
  };

  const handleDownloadPNG = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    onDownloadPNG();
  };

  const handleReturnToLobby = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    onReturnToLobby();
  };

  return (
    <ModalWrapper
      isOpen={isOpen}
      title={<Trans>Session Expired</Trans>}
      className="max-w-md"
      zIndex="z-[999999]"
    >
      {/*
        The warning colour is NEO's own tool text rather than a stock yellow:
        it is the accent that follows the light and dark palettes.
      */}
      <div className="mb-[8px] text-(--neo-tool-text)">
        <Icon icon="material-symbols:warning" width={28} height={28} />
      </div>
      <div className="mb-[12px]">
        {!hasCanvas
          ? t`This collaborative session has ended. Its drawing was not saved to the gallery and is no longer available.`
          : endedByOwner
          ? t`The owner ended this session. You can save your work locally as a PNG before leaving.`
          : isOwner && ownerKeepsOnDevice
          ? t`This collaborative session has ended due to inactivity. You can keep it on this device and post it after signing in, or download it as a PNG.`
          : isOwner
          ? canvasMeta?.savedPostId
            ? t`This collaborative session has ended due to inactivity. The session has already been saved to the gallery, but you can download it as a PNG.`
            : t`This collaborative session has ended due to inactivity. As the owner, you can save it to the gallery or download it as a PNG.`
          : t`This collaborative session has ended due to inactivity. You can save your work locally as a PNG before leaving.`}
      </div>
      {/* Only where the owner is offered the gallery save. */}
      {hasCanvas && isOwner && !endedByOwner && !ownerKeepsOnDevice && !canvasMeta?.savedPostId && destination && (
        <SaveDestination {...destination} />
      )}
      <div className="flex flex-wrap justify-center gap-[6px]">
        {!hasCanvas ? null : isOwner && !endedByOwner ? (
          <>
            {!canvasMeta?.savedPostId && (
              <button
                onClick={handleSaveToGallery}
                disabled={isSaving}
                type="button"
                data-haptic="medium"
                className={`${NEO_BUTTON} flex items-center gap-[4px] disabled:cursor-not-allowed`}
              >
                {isSaving ? (
                  <>
                    <Icon
                      icon="material-symbols:refresh"
                      width={14}
                      height={14}
                      className="animate-spin"
                    />
                    <Trans>Saving...</Trans>
                  </>
                ) : (
                  <>
                    <Icon icon="material-symbols:save" width={14} height={14} />
                    {ownerKeepsOnDevice ? <Trans>Keep on This Device</Trans> : <Trans>Save to Gallery</Trans>}
                  </>
                )}
              </button>
            )}
            <button
              onClick={handleDownloadPNG}
              type="button"
              className={`${NEO_BUTTON} flex items-center gap-[4px]`}
            >
              <Icon icon="material-symbols:download" width={14} height={14} />
              <Trans>Download PNG</Trans>
            </button>
          </>
        ) : (
          <button
            onClick={handleDownloadPNG}
            type="button"
            className={`${NEO_BUTTON} flex items-center gap-[4px]`}
          >
            <Icon icon="material-symbols:save" width={14} height={14} />
            <Trans>Save as PNG</Trans>
          </button>
        )}
        <button
          onClick={handleReturnToLobby}
          type="button"
          className={NEO_BUTTON}
        >
          <Trans>Return to Lobby</Trans>
        </button>
      </div>
    </ModalWrapper>
  );
};
