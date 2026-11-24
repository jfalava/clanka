import "./styles.css";

import { Effect } from "effect";
import { Runtime } from "foldkit";

import {
  COMPANIONS_KEY,
  Flags,
  Message,
  Model,
  Theme,
  applyTheme,
  init,
  update,
} from "./main";
import { view } from "./view";

/** The theme jfa.dev apps keep in the `jfa-theme` cookie; index.html painted it already. */
const saved = /(?:^|;\s*)jfa-theme=(light|dark|system)/.exec(document.cookie)?.[1] ?? "system";
const theme: Theme = saved === "light" ? "Light" : saved === "dark" ? "Dark" : "System";
applyTheme(theme);

const TOKEN_KEY = "clanka-web-token";

/**
 * This run's token. `clanka web` prints its address with the token after the `#`, which a browser
 * never sends anywhere. It is moved to the tab's session storage, so a reload keeps it, and taken
 * out of the address bar, so copying the address does not hand it over.
 */
const token = (() => {
  const given = /^#t=([0-9a-f]+)$/.exec(location.hash)?.[1];
  if (given !== undefined) {
    sessionStorage.setItem(TOKEN_KEY, given);
    history.replaceState(null, "", location.pathname + location.search);
  }
  return given ?? sessionStorage.getItem(TOKEN_KEY) ?? "";
})();

// Another run's address pasted into this tab changes only what follows the `#`, which loads
// nothing. Loading the page again takes up its token.
window.addEventListener("hashchange", () => {
  if (location.hash.startsWith("#t=")) {
    location.reload();
  }
});

Runtime.run(
  Runtime.makeApplication({
    Model,
    Flags,
    init,
    update,
    view,
    container: document.getElementById("root"),
    devTools: { Message },
  }),
  {
    flags: Effect.succeed({
      theme,
      token,
      isCompanionsOpen: localStorage.getItem(COMPANIONS_KEY) !== "closed",
    }),
  },
);
