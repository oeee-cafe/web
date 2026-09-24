import { Trans } from "@lingui/react/macro";
import { NEO_BUTTON, NEO_PANEL, NEO_TITLEBAR, NEO_WELL } from "neo-cucumber";
import type { SyncProgress } from "../../hooks/useWebSocket";

export interface ConnectionStatusModalProps {
  isCatchingUp: boolean;
  connectionState: "connecting" | "connected" | "disconnected";
  onReconnect: () => void;
  onDownloadPNG: () => void;
  syncProgress: SyncProgress;
  synchronizationError: string | null;
}

/** Centred over the canvas, in the same chrome as the panels beside it. */
const PANEL =
  `${NEO_PANEL} absolute top-1/2 left-1/2 z-[1000] -translate-x-1/2 ` +
  "-translate-y-1/2 shadow-lg";

const TITLE = `${NEO_TITLEBAR} px-[4px] text-[11px] leading-[14px]`;

export const ConnectionStatusModal = ({
  isCatchingUp,
  connectionState,
  onReconnect,
  onDownloadPNG,
  syncProgress,
  synchronizationError,
}: ConnectionStatusModalProps) => {
  if (connectionState === "connected" && !isCatchingUp) {
    return null;
  }

  if (isCatchingUp && !synchronizationError) {
    const hasTarget = syncProgress.targetSequence !== null && syncProgress.targetSequence > 0;
    const percent = hasTarget
      ? Math.min(100, Math.round(syncProgress.appliedSequence / syncProgress.targetSequence! * 100))
      : null;
    return (
      <div className={PANEL}>
        <div className={TITLE}>
          {syncProgress.phase === "joining" ? <Trans>Connecting...</Trans> : <Trans>Loading...</Trans>}
        </div>
        <div className="p-[10px] text-center">
          <div className="mb-[6px] animate-spin-slow text-[32px]">🥒</div>
          {percent !== null && (
            <div className="min-w-[200px]" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
              <div className="mb-[3px] text-[11px]">{syncProgress.appliedSequence} / {syncProgress.targetSequence} ({percent}%)</div>
              {/* A sunken track with NEO's own bar colour running along it. */}
              <div className={`${NEO_WELL} h-[10px]`}>
                <div className="h-full bg-(--neo-bar) transition-[width]" style={{ width: `${percent}%` }} />
              </div>
            </div>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className={`${PANEL} min-w-[280px] touch-auto`}>
      <div className={TITLE}>
        {connectionState === "connecting"
          ? <Trans>Connecting...</Trans>
          : <Trans>Disconnected</Trans>}
      </div>
      <div className="p-[12px] text-center">
        {(connectionState === "disconnected" || synchronizationError) && (
          <>
            <div className="mb-[12px]">
              {synchronizationError
                ? <Trans>The shared drawing could not be synchronized safely. Your current canvas is still available to download.</Trans>
                : <Trans>Connection lost. Your work is saved locally.</Trans>}
            </div>
            <div className="flex justify-center gap-[6px]">
              <button className={NEO_BUTTON} onClick={onReconnect}>
                <Trans>Reconnect</Trans>
              </button>
              <button className={NEO_BUTTON} onClick={onDownloadPNG}>
                <Trans>Download PNG</Trans>
              </button>
            </div>
          </>
        )}
        {connectionState === "connecting" && (
          <div className="animate-spin text-[24px]">🥒</div>
        )}
      </div>
    </div>
  );
};
