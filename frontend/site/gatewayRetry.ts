// A page htmx could not fetch is fetched again as an ordinary navigation.
//
// htmx fetches the site's pages itself (hx-boost, toolbar.jinja). When
// nothing answers it does nothing at all, and when a gateway answers in the
// site's place -- a proxy's 502, 503 or 504 during a deploy, Cloudflare's 52x
// and 530 when the Mac mini is out of reach -- it swaps the gateway's page
// into the body. Either way the reader is left on a page that is not the one
// they asked for and cannot tell why. Asked again as a navigation, the
// failure is the browser's to show -- and in the apps, each of which puts up
// its own "can't be reached" screen when a page fails to load
// (Unreachable.swift, Unreachable.kt, offline.rs in oeee-cafe-desktop), the
// app's. Each app used to watch htmx for this itself, or not at all.
//
// Only a page: the whole body, by GET. A part of a page that fails is the
// page's to show, and an error page of the site's own -- a 404, a 500 -- is
// the site speaking, and is swapped in as usual.

interface Ctx {
  target?: Element;
  request?: { method: string; action: string };
  response?: { status: number };
}

const GATEWAY = [502, 503, 504, 520, 521, 522, 523, 524, 525, 526, 527, 530];

function isPage(ctx: Ctx | undefined): ctx is Ctx & { request: { method: string; action: string } } {
  return (
    !!ctx && ctx.target === document.body && !!ctx.request && String(ctx.request.method).toUpperCase() === "GET"
  );
}

function ctxOf(event: Event): Ctx | undefined {
  const detail = (event as CustomEvent<{ ctx?: Ctx }>).detail;
  return detail && detail.ctx;
}

/** Listens for a page htmx could not have, once for the document. */
export function installGatewayRetry(): void {
  document.addEventListener("htmx:before:swap", (event) => {
    const ctx = ctxOf(event);
    if (!isPage(ctx) || !ctx.response || GATEWAY.indexOf(ctx.response.status) < 0) return;
    event.preventDefault();
    location.assign(ctx.request.action);
  });
  document.addEventListener("htmx:error", (event) => {
    const ctx = ctxOf(event);
    if (!isPage(ctx) || ctx.response) return;
    location.assign(ctx.request.action);
  });
}
