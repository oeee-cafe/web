// What Android's web view cannot do itself (app_polyfills.jinja): share, and
// save a file the page made. Each goes to the app as a message.

import type { OeeeApp } from "./bridge";

type PolyfillWindow = Window & typeof globalThis & { oeeeApp?: OeeeApp };

/** Stands in for Web Share and downloads, in the Android app only. */
export function installPolyfills(): void {
  const app = (window as PolyfillWindow).oeeeApp;
  if (!app || document.documentElement.getAttribute("data-app") !== "android" || !app.connected()) return;

  if (!navigator.share) {
    const shareable = (data?: ShareData) =>
      !!data && !(data.files && data.files.length) && !!(data.url || data.text || data.title);
    navigator.share = (data?: ShareData) => {
      if (!shareable(data)) return Promise.reject(new TypeError("Nothing this app can share"));
      const text = [data!.text, data!.url].filter(Boolean).join("\n");
      app.post("share", { title: data!.title || "", text: text || data!.title }, true);
      return Promise.resolve();
    };
    navigator.canShare = shareable;
  }

  const local = (link: HTMLAnchorElement) => link.hasAttribute("download") && /^(blob|data):/i.test(link.href);
  // Read now, inside the click: the painter revokes the blob's URL the moment
  // after it.
  const save = (link: HTMLAnchorElement) => {
    const name = link.getAttribute("download") || "";
    fetch(link.href)
      .then((response) => response.blob())
      .then((blob) => {
        const reader = new FileReader();
        reader.onload = () => {
          app.post("download", { name, mime: blob.type || "", data: reader.result }, true);
        };
        reader.readAsDataURL(blob);
      });
  };
  // A link made and clicked from script, never put in the page, as the
  // painter's is.
  const click = HTMLAnchorElement.prototype.click;
  HTMLAnchorElement.prototype.click = function (this: HTMLAnchorElement) {
    if (local(this)) {
      save(this);
      return;
    }
    return click.call(this);
  };
  // A link in the page, pressed by a finger.
  document.addEventListener(
    "click",
    (event) => {
      const target = event.target as Element | null;
      const link = target && target.closest ? (target.closest("a[download]") as HTMLAnchorElement | null) : null;
      if (!link || !local(link) || event.defaultPrevented) return;
      event.preventDefault();
      save(link);
    },
    true,
  );
}
