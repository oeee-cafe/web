import "neo-cucumber/style.css";
import "./replay.css";
import { mountReplay } from "./mount";

/**
 * The session to play is in the path the admin route serves this under:
 * /admin/collaborative-sessions/<uuid>/replay.
 */
const session = window.location.pathname.split("/")[3] ?? "";
const host = document.getElementById("replay");
if (host) {
  void mountReplay(host, session).catch((error) => {
    host.textContent = `Could not play this recording: ${
      error instanceof Error ? error.message : String(error)
    }`;
  });
}
