import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App.tsx";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import * as Sentry from "@sentry/react";
import { setupI18n, fetchPreferredLocale } from "./i18n";
import { DefaultI18n } from "neo-cucumber";
import { refuseSelection } from "../shared/selection";

Sentry.init({
  dsn: "https://930f2aecbd98603e4dd1651924c1004a@o4504757655764992.ingest.us.sentry.io/4510046135582720",
  // Setting this option to true will send default PII data to Sentry.
  // For example, automatic IP address collection on events
  sendDefaultPii: true,
});

// Beside app.css, which refuses selection for the page: see selection.ts.
refuseSelection();

// Initialize i18n with default locale (English)
setupI18n("en");

// Asynchronously fetch and apply user's preferred locale
(async () => {
  const preferredLocale = await fetchPreferredLocale();
  if (preferredLocale) {
    setupI18n(preferredLocale);
  }
})();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
      <App />
    </I18nProvider>
  </StrictMode>
);
