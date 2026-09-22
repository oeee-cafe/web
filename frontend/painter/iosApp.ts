import type { PainterCommand, PainterHandle } from "neo-cucumber";

/**
 * The painter, offered to the iOS app once it is ready (WebTab.swift in
 * oeee-cafe/apple). The app drives it from what the Apple Pencil says --
 * double-tap and squeeze, as the reader set them in Settings -- and tells it
 * when the system's "Only Draw with Apple Pencil" is on. Anywhere else nothing
 * is listening, and nothing happens.
 *
 * Returns the offer's withdrawal, for a host that unmounts its painter.
 */
export function offerPainterToApp(painter: PainterHandle): () => void {
  const host = window as unknown as {
    oeeePainter?: { command(name: PainterCommand): void; preferPen(): void };
    webkit?: { messageHandlers?: { oeeePainter?: { postMessage(message: string): void } } };
  };
  const offered = {
    command: (name: PainterCommand) => painter.command(name),
    preferPen: () => painter.preferPen(),
  };
  host.oeeePainter = offered;
  // Said once the painter can be driven, so the app never has to guess when
  // that is.
  host.webkit?.messageHandlers?.oeeePainter?.postMessage("ready");
  return () => {
    if (host.oeeePainter === offered) delete host.oeeePainter;
  };
}
