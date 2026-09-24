// The emoji picker behind a post's "+" reaction chip (post_reactions.jinja).
//
// A module, so a browser too old for one never runs it and keeps the form
// that sits in the same panel: a box to type an emoji into. The picker itself
// (emoji-picker-element) is fetched the first time a "+" is hovered or opened,
// not with the page. Everything here is delegated from the document, because
// the reactions block is swapped out whole after every reaction and a boosted
// navigation swaps the body around it; a module in the head runs once.

const PICKER = "https://cdn.jsdelivr.net/npm/emoji-picker-element@1.29.1/";
const DATA = "https://cdn.jsdelivr.net/npm/emoji-picker-element-data@1.8.0/";

// Emojibase has no Korean names; CLDR's are the same shape.
const SOURCES = {
  en: "en/emojibase/data.json",
  ja: "ja/emojibase/data.json",
  zh: "zh/emojibase/data.json",
  ko: "ko/cldr/data.json",
};

// The picker ships Japanese and Chinese chrome but no Korean.
const KO = {
  categoriesLabel: "카테고리",
  emojiUnsupportedMessage: "이 브라우저에서는 컬러 이모지를 표시할 수 없습니다.",
  favoritesLabel: "자주 쓰는 이모지",
  loadingMessage: "불러오는 중…",
  networkErrorMessage: "이모지를 불러오지 못했습니다.",
  regionLabel: "이모지 선택",
  searchDescription: "검색 결과가 있으면 위아래 방향키로 고르고 Enter로 선택하세요.",
  searchLabel: "검색",
  searchResultsLabel: "검색 결과",
  skinToneDescription: "펼친 뒤 위아래 방향키로 고르고 Enter로 선택하세요.",
  skinToneLabel: "피부색 선택 (현재 {skinTone})",
  skinTonesLabel: "피부색",
  skinTones: ["기본", "밝은 피부색", "약간 밝은 피부색", "중간 피부색", "약간 어두운 피부색", "어두운 피부색"],
  categories: {
    custom: "사용자 지정",
    "smileys-emotion": "표정과 감정",
    "people-body": "사람과 신체",
    "animals-nature": "동물과 자연",
    "food-drink": "음식과 음료",
    "travel-places": "여행과 장소",
    activities: "활동",
    objects: "사물",
    symbols: "기호",
    flags: "깃발",
  },
};

// The category tabs are emoji too, and at the grid's size and colour they
// read as more emoji to pick. Set them apart the way Discord does: a strip of
// their own, small and grey until current or pointed at. The picker's shadow
// root is open and its README documents styling it this way; the site's
// tokens inherit into it, so the strip follows the theme switch.
const NAV_STYLE = `
  .nav, .indicator-wrapper { background: var(--ds-ground); }
  .nav { padding: 2px 4px 0; }
  .nav-emoji { filter: grayscale(1); opacity: 0.55; transition: filter 0.15s, opacity 0.15s; }
  .nav-button:hover .nav-emoji,
  .nav-button[aria-selected="true"] .nav-emoji { filter: none; opacity: 1; }
`;

let loading = null;

function load() {
  if (!loading) {
    loading = import(PICKER + "index.js").catch((error) => {
      loading = null;
      throw error;
    });
  }
  return loading;
}

function language() {
  const lang = (document.documentElement.lang || "en").slice(0, 2);
  return SOURCES[lang] ? lang : "en";
}

async function chrome(lang) {
  if (lang === "ko") return KO;
  if (lang === "ja") return (await import(PICKER + "i18n/ja.js")).default;
  if (lang === "zh") return (await import(PICKER + "i18n/zh_CN.js")).default;
  return null;
}

// The site's own switch wins over the system's, as it does for every token.
function dark() {
  const theme = document.documentElement.getAttribute("data-theme");
  if (theme) return theme === "dark";
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function submit(form) {
  if (form.requestSubmit) form.requestSubmit();
  else form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
}

// Keep the panel on screen when the chip is near the right edge. On a phone
// it is a sheet along the bottom instead (style.css) and needs nothing.
function place(details) {
  const panel = details.querySelector(".reaction-custom-panel");
  if (!panel) return;
  panel.style.left = "";
  if (window.matchMedia("(max-width: 480px)").matches) return;
  const overflow = panel.getBoundingClientRect().right - (document.documentElement.clientWidth - 16);
  if (overflow > 0) panel.style.left = -overflow + "px";
}

async function mount(details) {
  const slot = details.querySelector(".reaction-picker");
  const form = details.querySelector(".reaction-custom-form");
  if (!slot || !form || slot.firstChild) return;

  const lang = language();
  const [, i18n] = await Promise.all([load(), chrome(lang)]);
  if (slot.firstChild || !details.isConnected) return;

  const picker = document.createElement("emoji-picker");
  picker.className = dark() ? "dark" : "light";
  picker.locale = lang;
  picker.dataSource = DATA + SOURCES[lang];
  if (i18n) picker.i18n = i18n;
  picker.addEventListener("emoji-click", (event) => {
    const input = form.querySelector("input[name=emoji]");
    input.value = event.detail.unicode;
    details.open = false;
    submit(form);
  });
  slot.appendChild(picker);
  if (picker.shadowRoot) {
    const style = document.createElement("style");
    style.textContent = NAV_STYLE;
    picker.shadowRoot.appendChild(style);
    // The category tabs tick as a segmented control's do (theme_head.jinja).
    // That listener sees only the picker itself, from outside its shadow.
    picker.shadowRoot.addEventListener("click", (event) => {
      const tab = event.target.closest && event.target.closest(".nav-button");
      if (!tab || tab.getAttribute("aria-selected") === "true") return;
      if (window.oeeeApp && window.oeeeApp.connected()) window.oeeeApp.feel("selection");
    });
  }
  // The typed form is only for when there is no picker. Its pattern is for a
  // person's typing; the picker can send a variation selector the server
  // accepts and the pattern would not.
  form.noValidate = true;
  details.classList.add("has-picker");
  place(details);

  if (window.matchMedia("(pointer: fine)").matches) {
    requestAnimationFrame(() => {
      const search = picker.shadowRoot && picker.shadowRoot.querySelector("input");
      if (search) search.focus();
    });
  }
}

document.addEventListener(
  "toggle",
  (event) => {
    const details = event.target;
    if (!(details instanceof HTMLDetailsElement) || !details.classList.contains("reaction-custom")) return;
    if (!details.open) return;
    place(details);
    mount(details).catch(() => {
      // Offline, or the CDN is unreachable: the typed form is still there.
      const input = details.querySelector(".reaction-custom-form input[name=emoji]");
      if (input) input.focus();
    });
  },
  true,
);

// Start fetching the picker as soon as a pointer heads for the chip.
document.addEventListener("pointerover", (event) => {
  if (event.target instanceof Element && event.target.closest(".reaction-custom > summary")) {
    load().catch(() => {});
  }
});

document.addEventListener("click", (event) => {
  for (const details of document.querySelectorAll("details.reaction-custom[open]")) {
    if (!details.contains(event.target)) details.open = false;
  }
});

document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  for (const details of document.querySelectorAll("details.reaction-custom[open]")) {
    details.open = false;
    const summary = details.querySelector("summary");
    if (summary) summary.focus();
  }
});
