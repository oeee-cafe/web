import type { PainterCommand, PainterHandle } from "neo-cucumber";

/**
 * The painter, offered to the apps once it is ready (Pencil.swift and
 * Scripts.swift in oeee-cafe-apple; any app on the bridge can drive it). The app drives it from what the Apple Pencil says --
 * double-tap and squeeze, as the reader set them in Settings -- and tells it
 * when the system's "Only Draw with Apple Pencil" is on. Anywhere else nothing
 * is listening, and nothing happens.
 *
 * Both drawing pages make the offer, the painter's and the collaborative
 * session's, which is why it lives beside them rather than in either: a pen
 * does not stop being a pen because other people are drawing too.
 *
 * Returns the offer's withdrawal, for a host that unmounts its painter.
 */
export function offerPainterToApp(painter: PainterHandle): () => void {
  const host = window as unknown as {
    oeeeApp?: {
      post(type: string, data: Record<string, unknown>): boolean;
      painter?: { command(name: PainterCommand): void; preferPen(): void };
    };
  };
  const app = host.oeeeApp;
  if (!app) return () => {};
  const offered = {
    command: (name: PainterCommand) => painter.command(name),
    preferPen: () => painter.preferPen(),
  };
  app.painter = offered;
  // Said once the painter can be driven, so the app never has to guess when
  // that is (app_bridge.jinja).
  app.post("painter", { state: "ready" });
  return () => {
    if (app.painter === offered) delete app.painter;
  };
}
