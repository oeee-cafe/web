// The comment box's @ autocomplete (mentionBox.ts), as the post page loads
// it: a classic script at the end of the body, built to dist-site/mention.js.
import { attachAllMentions } from "./mentionBox";

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", attachAllMentions);
} else {
  attachAllMentions();
}
