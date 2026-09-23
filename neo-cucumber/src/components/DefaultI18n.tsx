import type { ReactNode } from "react";

/**
 * What `<Trans>` renders its text into, as `I18nProvider`'s
 * `defaultComponent`. The painter mounts with it and the collaborative host
 * renders with it, so a message is the same element whichever of the two
 * wrote it.
 */
export const DefaultI18n = ({ children }: { children: ReactNode }) => (
  <span>{children}</span>
);
