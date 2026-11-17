import "./styles.css";

import { Effect } from "effect";
import { Runtime } from "foldkit";

import { Message, Model, Theme, applyTheme, init, update } from "./main";
import { view } from "./view";

/** The theme jfa.dev apps keep in the `jfa-theme` cookie; index.html painted it already. */
const saved = /(?:^|;\s*)jfa-theme=(light|dark|system)/.exec(document.cookie)?.[1] ?? "system";
const theme: Theme = saved === "light" ? "Light" : saved === "dark" ? "Dark" : "System";
applyTheme(theme);

Runtime.run(
  Runtime.makeApplication({
    Model,
    Flags: Theme,
    init,
    update,
    view,
    container: document.getElementById("root"),
    devTools: { Message },
  }),
  { flags: Effect.succeed(theme) },
);
