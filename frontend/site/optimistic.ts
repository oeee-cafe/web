// Controls that show their answer at the press, before the server's
// (optimistic.jinja): a reaction chip, and follow and unfollow
// (follow_macro.jinja), whose button takes the other one's words and fill.
//
// A reaction chip (post_reactions.jinja) fills or empties and moves its
// number, and the post's total with it, the moment it is pressed, the way a
// chat application's reactions do -- the round trip is a few hundred
// milliseconds on a phone, and a chip that sits there that long reads as a
// press that missed. The response is the whole block re-rendered, which
// stands in for this guess with the truth: someone else's reaction that
// landed meanwhile included.
//
// A request that fails is answered with a toast (web::htmx::error_banner) and
// no block, so the guess is taken back here, from what was noted before it
// was made; a request that never reached the server is taken back the same
// way. Only the numbers and the class are touched, never the markup, so the
// chip is still the element htmx knows.

/** htmx's request context, with the guess noted on it. */
interface Ctx {
  sourceElement?: Element;
  response?: { status: number };
  oeeeGuess?: { undo(): void } | null;
}

type OptimisticWindow = Window & typeof globalThis & { oeeeOptimisticInstalled?: boolean };

function nudge(span: Element | null, by: number): { span: Element; was: string | null } | null {
  if (!span) return null;
  const was = span.textContent;
  const n = parseInt(was!, 10);
  if (!isNaN(n)) span.textContent = String(Math.max(0, n + by));
  return { span, was };
}

// Follow and unfollow (follow_macro.jinja): the button takes the other one's
// words and fill.
function toggle(ctx: Ctx, form: Element): void {
  const button = form.querySelector("button[data-pressed-label]");
  if (!button) return;
  const was = { text: button.textContent, className: button.className };
  button.textContent = button.getAttribute("data-pressed-label");
  button.classList.toggle("ds-button-primary");
  ctx.oeeeGuess = {
    undo() {
      if (!button.isConnected) return;
      button.textContent = was.text;
      button.className = was.className;
    },
  };
}

function takeBack(ctx: Ctx | undefined): void {
  const guess = ctx && ctx.oeeeGuess;
  if (!guess) return;
  ctx!.oeeeGuess = null;
  guess.undo();
}

function ctxOf(event: Event): Ctx | undefined {
  const detail = (event as CustomEvent<{ ctx?: Ctx }>).detail;
  return detail && detail.ctx;
}

/** Listens for the presses to guess at, once for the document. */
export function installOptimistic(): void {
  const page = window as OptimisticWindow;
  if (page.oeeeOptimisticInstalled) return;
  page.oeeeOptimisticInstalled = true;
  document.addEventListener("htmx:before:request", (event) => {
    const ctx = ctxOf(event);
    const source = ctx && ctx.sourceElement;
    if (source && source.hasAttribute && source.hasAttribute("data-optimistic-toggle")) {
      toggle(ctx!, source);
      return;
    }
    const chip = source;
    if (!chip || !chip.classList || !chip.classList.contains("reaction-button")) return;
    const block = chip.closest(".post-reactions");
    if (!block) return;
    const reacted = !chip.classList.contains("reacted");
    const by = reacted ? 1 : -1;
    chip.classList.toggle("reacted", reacted);
    const counts = [nudge(chip.querySelector(".reaction-count"), by), nudge(block.querySelector(".reaction-total-count"), by)];
    ctx!.oeeeGuess = {
      undo() {
        if (!chip.isConnected) return;
        chip.classList.toggle("reacted", !reacted);
        for (let i = 0; i < counts.length; i++) {
          const count = counts[i];
          if (count && count.span.isConnected) count.span.textContent = count.was;
        }
      },
    };
  });
  document.addEventListener("htmx:after:request", (event) => {
    const ctx = ctxOf(event);
    if (ctx && ctx.response && ctx.response.status >= 400) takeBack(ctx);
  });
  document.addEventListener("htmx:error", (event) => {
    takeBack(ctxOf(event));
  });
}
