import { Option } from "effect";
import { AsyncData } from "foldkit";
import type { Document, Html, HtmlBuilder } from "foldkit/html";
import {
  BookOpenCheck,
  ChevronDown,
  ChevronRight,
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  RefreshCw,
  Search,
  Snowflake,
  Sun,
  Trash,
  Undo2,
  X,
} from "lucide";

import {
  KINDS,
  LEVELS,
  REREAD_BATCH,
  type Deletion,
  type Detail,
  type FactIssue,
  type Found,
  type Level,
  type Listed,
  type Memory,
  type Overview,
  type RollbackPlan,
  type WriteEntry,
} from "./api";
import * as AlertDialog from "./components/ui/alert-dialog";
import { badge, type BadgeVariant } from "./components/ui/badge";
import { button, buttonVariants } from "./components/ui/button";
import { Card } from "./components/ui/card";
import { Empty } from "./components/ui/empty";
import { nativeSelect, nativeSelectOption } from "./components/ui/native-select";
import { skeleton } from "./components/ui/skeleton";
import { Table, tableCellClass } from "./components/ui/table";
import { icon } from "./lib/icons";
import {
  Asking,
  Message,
  Repair,
  isConfirmable,
  isRepairing,
  matching,
  rereadable,
  rollbackSummary,
  type Loaded,
  type Model,
  type Tab,
  type Theme,
} from "./main";

type Builder = HtmlBuilder<Message>;

const count = (value: number) => value.toLocaleString("en-GB");
const when = (at: number) =>
  new Date(at).toLocaleString("en-GB", { dateStyle: "medium", timeStyle: "short" });
const day = (at: number) => new Date(at).toLocaleDateString("en-GB", { dateStyle: "medium" });
/** A time as `clanka doctor log` prints it and `clanka doctor rollback --to` reads it. */
const utc = (at: number) => new Date(at).toISOString().slice(0, 19);
const signed = (value: number) => `${value > 0 ? "+" : value < 0 ? "−" : ""}${Math.abs(value).toFixed(1)}`;
const words = (name: string) => name.replaceAll("_", " ");
const megabytes = (bytes: number) => `${(bytes / 1_048_576).toFixed(1)} MB`;
const factText = (value: unknown) => (typeof value === "string" ? value : JSON.stringify(value));

const mono = "font-mono text-xs";
const inputClass =
  "w-full rounded-md border border-input bg-transparent px-2.5 text-sm outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/30 dark:bg-input/30";

// PARTS

const section = (title: string, content: Html, h: Builder): Html =>
  h.section(
    [h.Class("flex min-w-0 flex-col gap-2")],
    [h.h2([h.Class("type-label text-muted-foreground")], [title]), content],
  );

const framed = (content: Html, h: Builder) =>
  h.div([h.Class("overflow-hidden rounded-lg border border-border")], [content]);

const nothing = (text: string, h: Builder) =>
  h.p([h.Class("rounded-lg border border-dashed border-border p-4 type-meta")], [text]);

const head = (labels: ReadonlyArray<string>, h: Builder) =>
  Table.header({}, [Table.row({}, labels.map((label) => Table.head({}, [label], h)), h)], h);

const failure = (title: string, error: string, h: Builder): Html =>
  Empty(
    { className: "rounded-lg border border-dashed border-destructive/40" },
    [
      Empty.header(
        {},
        [
          Empty.title({ className: "text-destructive" }, [title], h),
          Empty.description({}, [error], h),
        ],
        h,
      ),
      Empty.content(
        {},
        [button({ onClick: Message.ClickedRefresh(), variant: "outline" }, ["Retry"], h)],
        h,
      ),
    ],
    h,
  );

const LEVEL_BADGE: Record<Level, BadgeVariant> = {
  faint: "ghost",
  normal: "outline",
  vivid: "secondary",
  seared: "default",
};

const levelBadge = (level: Level, h: Builder) =>
  badge(
    { variant: LEVEL_BADGE[level], className: level === "faint" ? "text-muted-foreground" : "" },
    [level],
    h,
  );

/** A memory's number, as a way to it in the table. */
const memoryLink = (id: number, h: Builder): Html =>
  h.button(
    [
      h.Type("button"),
      h.OnClick(Message.ClickedMemoryLink({ id })),
      h.Class(
        `${mono} cursor-pointer text-primary underline decoration-dotted underline-offset-4 hover:decoration-solid`,
      ),
    ],
    [`#${id}`],
  );

// HEADER

const NEXT_THEME: Record<Theme, Theme> = { System: "Light", Light: "Dark", Dark: "System" };
const THEME_ICON = { System: Monitor, Light: Sun, Dark: Moon } as const;

const COMPANIONS_ID = "companions";

/**
 * Whether the list of companions is on the page. It closes only once a companion is being read:
 * with nobody chosen, the list is all there is to act on, and where a failure to list them shows.
 */
const isListShown = (model: Model) =>
  model.isCompanionsOpen || Option.isNone(model.maybeCompanion);

/** The header's button that closes the list of companions and opens it again. */
const listToggle = (model: Model, h: Builder): Html => {
  const isShown = isListShown(model);
  const label = isShown ? "Hide companions" : "Show companions";
  return button(
    {
      onClick: Message.ClickedToggleCompanions(),
      variant: "ghost",
      size: "icon-lg",
      isDisabled: model.token === "" || Option.isNone(model.maybeCompanion),
      className: "-ml-2 text-muted-foreground hover:text-foreground",
      attributes: [
        h.AriaLabel(label),
        h.Title(label),
        h.AriaExpanded(isShown),
        h.Attribute("aria-controls", COMPANIONS_ID),
      ],
    },
    [icon(h, isShown ? PanelLeftClose : PanelLeftOpen, "size-4")],
    h,
  );
};

/**
 * jfa.dev's SiteHeader: the brand on the left, the app's actions, a divider, the theme. Before the
 * brand, on the side the list is on, the button that closes it.
 */
const header = (model: Model, h: Builder): Html =>
  h.header(
    [h.Class("site-header sticky top-0 z-30 shrink-0 border-b border-border bg-background")],
    [
      h.div(
        [h.Class("header-gutters flex min-h-11 items-center justify-between gap-4 sm:gap-6")],
        [
          h.div(
            [h.Class("flex min-w-0 items-center gap-2")],
            [
              listToggle(model, h),
              h.div(
                [
                  h.Class("flex min-w-0 items-baseline gap-3 truncate"),
                  h.AriaLabel("CLANKA by JFA"),
                ],
                [
                  h.span(
                    [h.Class("shrink-0 text-sm font-bold tracking-tight text-primary")],
                    ["/CLANKA", h.span([h.Class("pl-0.5 text-xs tracking-tight")], ["by JFA"])],
                  ),
                  h.span([h.Class("hidden text-xs text-muted-foreground/75 sm:inline")], ["/"]),
                  h.span(
                    [h.Class("hidden truncate text-xs text-muted-foreground sm:inline")],
                    ["What your companions remember, on this machine"],
                  ),
                ],
              ),
            ],
          ),
          h.nav(
            [h.Class("flex shrink-0 items-center gap-1"), h.AriaLabel("Page")],
            [
              button(
                {
                  onClick: Message.ClickedRefresh(),
                  variant: "ghost",
                  size: "lg",
                  isDisabled:
                    model.token === "" ||
                    AsyncData.isPending(model.companions) ||
                    AsyncData.isPending(model.loaded),
                  className: "text-muted-foreground hover:text-foreground",
                },
                [icon(h, RefreshCw, "size-4"), h.span([h.Class("hidden lg:inline")], ["Refresh"])],
                h,
              ),
              h.span([h.AriaHidden(true), h.Class("mx-1 h-4 w-px shrink-0 bg-border sm:mx-1.5")]),
              button(
                {
                  onClick: Message.ClickedTheme({ theme: NEXT_THEME[model.theme] }),
                  variant: "ghost",
                  size: "lg",
                  className: "gap-1.5 px-2 text-muted-foreground hover:text-foreground",
                  attributes: [h.AriaLabel(`Theme: ${model.theme}`)],
                },
                [
                  icon(h, THEME_ICON[model.theme], "size-4"),
                  h.span([h.Class("hidden lg:inline")], [model.theme]),
                ],
                h,
              ),
            ],
          ),
        ],
      ),
    ],
  );

// COMPANIONS

const companionItem = (model: Model, listed: Listed, h: Builder): Html => {
  const { id, name } = listed;
  const file = listed.soul.split("/").at(-1) ?? listed.soul;
  if (id === null || name === null) {
    return h.keyed("li")(
      listed.soul,
      [h.Class("rounded-md border border-dashed border-destructive/40 px-3 py-2")],
      [
        h.p([h.Class(`${mono} truncate`), h.Title(listed.soul)], [file]),
        h.p([h.Class("type-meta text-destructive")], [listed.error ?? "This soul.md does not load."]),
      ],
    );
  }
  const isChosen = Option.contains(model.maybeCompanion, id);
  const neverOpened = listed.db_bytes === null;
  return h.keyed("li")(id, [], [
    h.button(
      [
        h.Type("button"),
        h.Disabled(neverOpened),
        h.OnClick(Message.ClickedCompanion({ id })),
        h.AriaCurrent(isChosen ? "true" : "false"),
        h.Title(listed.soul),
        h.Class(
          `flex w-full cursor-pointer flex-col items-start gap-0.5 rounded-md border px-3 py-2 text-left transition-colors disabled:cursor-default disabled:opacity-60 ${
            isChosen
              ? "border-primary bg-muted"
              : "border-transparent hover:bg-muted/50 disabled:hover:bg-transparent"
          }`,
        ),
      ],
      [
        h.span([h.Class("text-sm font-medium")], [name]),
        h.span(
          [h.Class(`${mono} text-muted-foreground`)],
          [
            `…${id.slice(-8)} · ${
              listed.db_bytes === null ? "never opened" : megabytes(listed.db_bytes)
            }`,
          ],
        ),
      ],
    ),
  ]);
};

const companions = (model: Model, h: Builder): Html =>
  h.aside(
    [h.Id(COMPANIONS_ID), h.Class("flex flex-col gap-2 lg:w-56 lg:shrink-0")],
    [
      h.h2([h.Class("type-label text-muted-foreground")], ["Companions"]),
      AsyncData.matchData(model.companions, {
        onEmpty: () => skeleton({ className: "h-24" }, [], h),
        onFailure: (error) => failure("The companions could not be listed", error, h),
        onData: (listed) =>
          listed.length === 0
            ? h.p(
                [h.Class("rounded-lg border border-dashed border-border p-4 type-meta")],
                ["No companions here yet. Make one with ", code("clanka init <name>", h), "."],
              )
            : h.ul(
                [h.Class("flex flex-col gap-1")],
                listed.map((entry) => companionItem(model, entry, h)),
              ),
      }),
    ],
  );

// OVERVIEW

const stat = (label: string, value: string, h: Builder, tone = "") =>
  Card(
    { size: "sm" },
    [
      Card.header({}, [Card.title({}, [label], h)], h),
      Card.content({}, [h.p([h.Class(`font-mono text-2xl font-medium ${tone}`)], [value])], h),
    ],
    h,
  );

const isOpenThread = (memory: Memory) =>
  memory.kind === "open_thread" && memory.status === "active" && memory.resolved_at === null;

const stats = ({ memories, issues }: Loaded, h: Builder): Html => {
  const active = memories.filter(({ status }) => status === "active");
  const unsaid = new Set(issues.map(({ memory }) => memory)).size;
  return h.div(
    [h.Class("grid grid-cols-2 gap-3 md:grid-cols-5")],
    [
      stat("Memories", count(active.length), h),
      stat("Replaced", count(memories.length - active.length), h),
      stat("Open threads", count(active.filter(isOpenThread).length), h),
      stat("Seared", count(active.filter(({ level }) => level === "seared").length), h),
      stat("Facts never said", count(unsaid), h, unsaid > 0 ? "text-destructive" : ""),
    ],
  );
};

const mood = (overview: Overview, h: Builder): Html =>
  section(
    "Mood",
    overview.mood === null
      ? nothing("Not set yet.", h)
      : h.div(
          [h.Class("flex flex-col gap-3 rounded-lg border border-border p-4")],
          [
            h.dl(
              [h.Class("grid grid-cols-3 gap-3")],
              (
                [
                  ["Valence", overview.mood.valence],
                  ["Energy", overview.mood.energy],
                  ["Warmth", overview.mood.warmth],
                ] as const
              ).map(([label, value]) =>
                h.keyed("div")(label, [], [
                  h.dt([h.Class("type-label text-muted-foreground")], [label]),
                  h.dd([h.Class("font-mono text-lg font-medium")], [signed(value)]),
                ]),
              ),
            ),
            h.p([h.Class("font-serif text-sm")], [overview.mood.cause]),
            overview.note === null
              ? h.empty
              : h.p(
                  [h.Class("border-t border-border pt-3 type-meta")],
                  ["Note for the next reply: ", h.span([h.Class("text-foreground")], [overview.note])],
                ),
          ],
        ),
    h,
  );

const SECTION_TITLE = { about_user: "About you", relationship: "The relationship" } as const;

const soulSections = (overview: Overview, h: Builder): ReadonlyArray<Html> =>
  overview.sections.map(({ name, versions }) => {
    const now = versions.at(-1);
    const earlier = versions.slice(0, -1).toReversed();
    return h.keyed("div")(name, [h.Class("min-w-0")], [
      section(
        SECTION_TITLE[name],
        now === undefined || now.content.trim() === ""
          ? nothing("Empty.", h)
          : h.div(
              [h.Class("flex flex-col gap-3 rounded-lg border border-border p-4")],
              [
                h.p([h.Class("font-serif text-sm whitespace-pre-wrap")], [now.content]),
                h.p([h.Class("type-meta")], [`Written ${when(now.created_at)}`]),
                earlier.length === 0
                  ? h.empty
                  : h.details(
                      [h.Class("border-t border-border pt-3")],
                      [
                        h.summary(
                          [h.Class("cursor-pointer type-meta")],
                          [`${count(earlier.length)} earlier ${earlier.length === 1 ? "version" : "versions"}`],
                        ),
                        h.ol(
                          [h.Class("flex flex-col gap-3 pt-3")],
                          earlier.map((version) =>
                            h.keyed("li")(String(version.id), [], [
                              h.p([h.Class("type-meta")], [when(version.created_at)]),
                              h.p(
                                [h.Class("font-serif text-sm whitespace-pre-wrap text-muted-foreground")],
                                [version.content],
                              ),
                            ]),
                          ),
                        ),
                      ],
                    ),
              ],
            ),
        h,
      ),
    ]);
  });

const openThreads = ({ memories }: Loaded, h: Builder): Html => {
  const threads = memories.filter(isOpenThread).toSorted((a, b) => b.id - a.id);
  return section(
    "Open threads",
    threads.length === 0
      ? nothing("Nothing it is waiting to hear about.", h)
      : h.ul(
          [h.Class("flex flex-col divide-y divide-border rounded-lg border border-border")],
          threads.map((thread) =>
            h.keyed("li")(String(thread.id), [h.Class("flex items-baseline gap-3 px-4 py-2.5")], [
              memoryLink(thread.id, h),
              badge(
                { variant: thread.urgency === "hot" ? "destructive" : "outline" },
                [thread.urgency ?? "open"],
                h,
              ),
              h.span([h.Class("min-w-0 text-sm")], [thread.interpretation]),
            ]),
          ),
        ),
    h,
  );
};

/** How many active memories of each type sit at each level. */
const byKind = ({ memories }: Loaded, h: Builder): Html => {
  const active = memories.filter(({ status }) => status === "active");
  const rows = KINDS.map((kind) => {
    const of = active.filter((memory) => memory.kind === kind);
    return { kind, total: of.length, levels: LEVELS.map((level) => of.filter((m) => m.level === level).length) };
  }).filter(({ total }) => total > 0);
  return section(
    "By type and level",
    rows.length === 0
      ? nothing("No memories yet.", h)
      : framed(
          Table(
            {},
            [
              head(["Type", ...LEVELS, "All"], h),
              Table.body(
                {},
                rows.map(({ kind, total, levels }) =>
                  h.keyed("tr")(kind, [h.Class("border-b last:border-0")], [
                    Table.cell({}, [words(kind)], h),
                    ...levels.map((value) =>
                      Table.cell(
                        { className: `${mono} ${value === 0 ? "text-muted-foreground/50" : ""}` },
                        [count(value)],
                        h,
                      ),
                    ),
                    Table.cell({ className: `${mono} font-medium` }, [count(total)], h),
                  ]),
                ),
                h,
              ),
            ],
            h,
          ),
          h,
        ),
    h,
  );
};

const overviewTab = (loaded: Loaded, h: Builder): Html =>
  h.div(
    [h.Class("flex flex-col gap-8")],
    [
      stats(loaded, h),
      h.div(
        [h.Class("grid gap-8 xl:grid-cols-2")],
        [mood(loaded.overview, h), openThreads(loaded, h)],
      ),
      h.div([h.Class("grid gap-8 xl:grid-cols-2")], soulSections(loaded.overview, h)),
      byKind(loaded, h),
    ],
  );

// MEMORIES

const select = (
  id: string,
  label: string,
  value: string,
  onChange: (value: string) => Message,
  options: ReadonlyArray<readonly [string, string]>,
  h: Builder,
) =>
  nativeSelect(
    {
      id,
      label,
      value,
      onChange,
      options: options.map(([value, label]) => nativeSelectOption({ value, label }, h)),
      wrapperClass: "w-auto",
      className: "min-w-36",
    },
    h,
  );

const filters = (model: Model, h: Builder): Html => {
  const filter = (field: "kind" | "level" | "status" | "facts" | "text") => (value: string) =>
    Message.ChangedFilter({ field, value });
  return h.div(
    [h.Class("flex flex-wrap items-end gap-3")],
    [
      h.div(
        [h.Class("flex min-w-56 flex-1 flex-col gap-1.5")],
        [
          h.label([h.For("find"), h.Class("type-label text-muted-foreground")], ["Find"]),
          h.input([
            h.Id("find"),
            h.Type("search"),
            h.Value(model.filters.text),
            h.OnInput(filter("text")),
            h.Placeholder("Words in a memory or its facts, or #12"),
            h.Class(`h-7 ${inputClass}`),
          ]),
        ],
      ),
      select("kind", "Type", model.filters.kind, filter("kind"), [
        ["", "Every type"],
        ...KINDS.map((kind) => [kind, words(kind)] as const),
      ], h),
      select("level", "Level", model.filters.level, filter("level"), [
        ["", "Every level"],
        ...LEVELS.map((level) => [level, level] as const),
      ], h),
      select("status", "Status", model.filters.status, filter("status"), [
        ["active", "Current"],
        ["superseded", "Replaced"],
        ["", "Both"],
      ], h),
      select("facts", "Facts", model.filters.facts, filter("facts"), [
        ["", "Any"],
        ["unsaid", "Never said"],
      ], h),
    ],
  );
};

const COLUMNS = 7;

/** A memory's marks beside its text: what it is besides its type and level. */
const marks = (memory: Memory, issues: ReadonlyArray<FactIssue>, h: Builder): ReadonlyArray<Html> => [
  ...(memory.status === "superseded" ? [badge({ variant: "outline" }, ["replaced"], h)] : []),
  ...(isOpenThread(memory)
    ? [badge({ variant: memory.urgency === "hot" ? "destructive" : "outline" }, [memory.urgency ?? "open"], h)]
    : []),
  ...(memory.resolved_at !== null ? [badge({ variant: "outline" }, ["closed"], h)] : []),
  ...(memory.dreamt ? [badge({ variant: "secondary" }, ["dreamt"], h)] : []),
  ...(issues.some((issue) => issue.memory === memory.id)
    ? [badge({ variant: "destructive" }, ["fact never said"], h)]
    : []),
];

const facts = (memory: Memory, issues: ReadonlyArray<FactIssue>, h: Builder): Html => {
  const entries = Object.entries(memory.facts);
  const mine = issues.filter((issue) => issue.memory === memory.id);
  if (entries.length === 0) {
    return nothing("No facts: nothing here can be checked against what was said.", h);
  }
  return h.div(
    [h.Class("flex flex-col gap-2")],
    [
      h.dl(
        [h.Class("flex flex-col divide-y divide-border rounded-md border border-border bg-background")],
        entries.map(([name, value]) => {
          const isUnsaid = mine.some((issue) => issue.fact !== null && issue.fact[0] === name);
          return h.keyed("div")(name, [h.Class("flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-2")], [
            h.dt([h.Class(`${mono} text-muted-foreground`)], [name]),
            h.dd([h.Class("min-w-0 text-sm whitespace-normal")], [factText(value)]),
            isUnsaid ? badge({ variant: "destructive" }, ["never said"], h) : h.empty,
          ]);
        }),
      ),
      mine.some((issue) => issue.fact === null)
        ? h.p([h.Class("type-meta text-destructive")], ["It has facts and cites no turns to check them against."])
        : h.empty,
    ],
  );
};

/** Why the selection cannot be re-read right now, or null when it can. */
const rereadBlock = (overview: Overview, eligible: number): string | null =>
  overview.model === null
    ? "Re-reading needs a [model] in config.toml."
    : !overview.frozen
      ? "Freeze first: memories are re-read only while nothing else writes."
      : eligible === 0
        ? "None of these can be re-read: seared memories never change, and the rest cite no turns."
        : null;

/** What to do with the ticked memories. The page shows it only on a run that repairs. */
const selection = (model: Model, loaded: Loaded, h: Builder): Html => {
  const chosen = loaded.memories.filter(({ id }) => model.selected.includes(id)).map(({ id }) => id);
  if (chosen.length === 0) {
    return h.p([h.Class("type-meta")], ["Tick memories to delete them or have them re-read."]);
  }
  const eligible = rereadable(loaded.memories, chosen).memories;
  const block = rereadBlock(loaded.overview, eligible.length);
  const isBusy = isRepairing(model);
  return h.div(
    [h.Class("flex flex-wrap items-center gap-2 rounded-lg border border-border bg-muted/50 px-3 py-2")],
    [
      h.p([h.Class("mr-1 text-sm font-medium")], [`${count(chosen.length)} ticked`]),
      button(
        { onClick: Message.ClickedDelete({ memories: chosen }), variant: "destructive", isDisabled: isBusy },
        [icon(h, Trash, "size-3.5"), "Delete…"],
        h,
      ),
      button(
        {
          onClick: Message.ClickedReread({ memories: eligible }),
          variant: "outline",
          isDisabled: isBusy || block !== null,
        },
        [icon(h, BookOpenCheck, "size-3.5"), "Re-read…"],
        h,
      ),
      button({ onClick: Message.ClickedClearSelection(), variant: "ghost" }, ["Clear"], h),
      block === null ? h.empty : h.p([h.Class("type-meta")], [block]),
    ],
  );
};

const about = (memory: Memory, all: ReadonlyArray<Memory>, h: Builder): Html => {
  const replaced = all.filter(({ superseded_by }) => superseded_by === memory.id);
  const lines: ReadonlyArray<ReadonlyArray<Html | string>> = [
    [`Written ${when(memory.created_at)}`, ...(memory.updated_at !== memory.created_at ? [`, changed ${when(memory.updated_at)}`] : [])],
    ...(memory.occurred_at === null ? [] : [[`About ${day(memory.occurred_at)}`]]),
    [`Recalled ${count(memory.recall_count)} ${memory.recall_count === 1 ? "time" : "times"}`],
    ...(memory.strength === null ? [] : [[`Held with strength ${memory.strength.toFixed(2)}`]]),
    ...(memory.resolved_at === null ? [] : [[`Closed ${when(memory.resolved_at)}`]]),
    ...(memory.superseded_by === null ? [] : [["Replaced by ", memoryLink(memory.superseded_by, h)]]),
    ...replaced.map(({ id }) => ["Replaced ", memoryLink(id, h)]),
    [`Persona ${memory.persona_hash.slice(0, 8)}`],
  ];
  return h.ul(
    [h.Class("flex flex-col gap-1 type-meta")],
    lines.map((line, index) => h.keyed("li")(String(index), [], line)),
  );
};

const history = (detail: Detail, h: Builder): Html =>
  h.ol(
    [h.Class("flex flex-col gap-3")],
    detail.versions.toReversed().map((version) =>
      h.keyed("li")(String(version.id), [h.Class("flex flex-col gap-1 border-l-2 border-border pl-3")], [
        h.p(
          [h.Class("flex flex-wrap items-center gap-2 type-meta")],
          [
            when(version.created_at),
            badge({ variant: "outline" }, [version.reason], h),
            levelBadge(version.level, h),
            version.drift_magnitude === null
              ? h.empty
              : h.span([h.Class(mono)], [`drift ${version.drift_magnitude.toFixed(2)}`]),
          ],
        ),
        h.p([h.Class("text-sm whitespace-normal")], [version.interpretation]),
      ]),
    ),
  );

const said = (detail: Detail, h: Builder): Html =>
  detail.turns.length === 0
    ? nothing("It cites no turns: it was not written from an exchange.", h)
    : h.ol(
        [h.Class("flex flex-col gap-2")],
        detail.turns.map((turn) =>
          h.keyed("li")(
            String(turn.id),
            [h.Class(`flex flex-col gap-1 ${turn.speaker === "user" ? "items-end" : "items-start"}`)],
            [
              h.p(
                [h.Class("type-meta")],
                [
                  `${turn.speaker === "user" ? "You" : "Companion"} · ${when(turn.created_at)}${
                    turn.echo ? " · echo" : ""
                  }`,
                ],
              ),
              h.p(
                [
                  h.Class(
                    `max-w-prose rounded-lg px-3 py-2 font-serif text-sm whitespace-pre-wrap ${
                      turn.speaker === "user" ? "bg-primary/10" : "border border-border bg-background"
                    }`,
                  ),
                ],
                [turn.content],
              ),
            ],
          ),
        ),
      );

const opened = (model: Model, memory: Memory, loaded: Loaded, h: Builder): Html =>
  h.keyed("tr")(
    `open-${memory.id}`,
    [h.Class("border-b bg-muted/30")],
    [
      h.td(
        [
          h.Attribute("colspan", String(COLUMNS + (loaded.overview.can_repair ? 1 : 0))),
          h.Class("px-4 py-4 whitespace-normal"),
        ],
        [
          h.div(
            [h.Class("grid gap-6 xl:grid-cols-2")],
            [
              h.div(
                [h.Class("flex min-w-0 flex-col gap-6")],
                [
                  section("Facts", facts(memory, loaded.issues, h), h),
                  section("About it", about(memory, loaded.memories, h), h),
                  loaded.overview.can_repair
                    ? h.div(
                        [],
                        [
                          button(
                            {
                              onClick: Message.ClickedDelete({ memories: [memory.id] }),
                              variant: "destructive",
                              isDisabled: isRepairing(model),
                            },
                            [icon(h, Trash, "size-3.5"), "Delete this memory…"],
                            h,
                          ),
                        ],
                      )
                    : h.empty,
                ],
              ),
              AsyncData.matchData(model.detail, {
                onEmpty: () => skeleton({ className: "h-32" }, [], h),
                onFailure: (error) => failure("This memory's history could not be read", error, h),
                onData: (detail) =>
                  h.div(
                    [h.Class("flex min-w-0 flex-col gap-6")],
                    [
                      section("What was said", said(detail, h), h),
                      section("History", history(detail, h), h),
                    ],
                  ),
              }),
            ],
          ),
        ],
      ),
    ],
  );

const checkboxClass = "size-4 cursor-pointer align-middle accent-primary";

const memoryRows = (model: Model, memory: Memory, loaded: Loaded, h: Builder): ReadonlyArray<Html> => {
  const isOpen = Option.contains(model.maybeOpenMemory, memory.id);
  // The cells open the memory, not the row: the checkbox's cell is not one of them, so ticking
  // a memory does not open it.
  const cell = (className: string, children: ReadonlyArray<Html | string>) =>
    h.td(
      [h.Class(`${tableCellClass} ${className}`), h.OnClick(Message.ClickedMemory({ id: memory.id }))],
      children,
    );
  const row = h.keyed("tr")(
    `memory-${memory.id}`,
    [
      h.Class(
        `cursor-pointer border-b transition-colors hover:bg-muted/50 ${
          memory.status === "superseded" ? "text-muted-foreground" : ""
        }`,
      ),
      h.AriaExpanded(isOpen),
    ],
    [
      ...(loaded.overview.can_repair
        ? [
            h.td(
              [h.Class(`${tableCellClass} w-6 cursor-default align-top`)],
              [
                h.input([
                  h.Type("checkbox"),
                  h.Checked(model.selected.includes(memory.id)),
                  h.OnChange(() => Message.ToggledMemory({ id: memory.id })),
                  h.AriaLabel(`Tick memory #${memory.id}`),
                  h.Class(checkboxClass),
                ]),
              ],
            ),
          ]
        : []),
      cell("w-6 align-top text-muted-foreground", [icon(h, isOpen ? ChevronDown : ChevronRight, "size-3.5")]),
      cell(`${mono} align-top text-muted-foreground`, [`#${memory.id}`]),
      cell("align-top", [words(memory.kind)]),
      cell("align-top", [levelBadge(memory.level, h)]),
      cell("align-top type-meta", [day(memory.created_at)]),
      cell(`${mono} align-top text-muted-foreground`, [memory.valence === null ? "—" : signed(memory.valence)]),
      cell("min-w-80 whitespace-normal", [
        h.div(
          [h.Class("flex flex-col gap-1.5")],
          [
            h.p([h.Class(isOpen ? "" : "line-clamp-2")], [memory.interpretation]),
            h.div([h.Class("flex flex-wrap gap-1.5 empty:hidden")], marks(memory, loaded.issues, h)),
          ],
        ),
      ]),
    ],
  );
  return isOpen ? [row, opened(model, memory, loaded, h)] : [row];
};

const MEMORY_LABELS = ["", "#", "Type", "Level", "Written", "Feel", "Memory"];

/** The table's head: on a run that repairs, with a checkbox that ticks every memory shown. */
const memoriesHead = (model: Model, shown: ReadonlyArray<Memory>, canRepair: boolean, h: Builder): Html => {
  if (!canRepair) {
    return head(MEMORY_LABELS, h);
  }
  const ids = shown.map(({ id }) => id);
  const areAllTicked = ids.every((id) => model.selected.includes(id));
  return Table.header(
    {},
    [
      Table.row(
        {},
        [
          Table.head(
            { className: "w-6" },
            [
              h.input([
                h.Type("checkbox"),
                h.Checked(areAllTicked),
                h.OnChange(() => Message.ToggledShownMemories({ memories: ids, isSelected: !areAllTicked })),
                h.AriaLabel(areAllTicked ? "Untick every memory shown" : "Tick every memory shown"),
                h.Class(checkboxClass),
              ]),
            ],
            h,
          ),
          ...MEMORY_LABELS.map((label) => Table.head({}, [label], h)),
        ],
        h,
      ),
    ],
    h,
  );
};

const memoriesTab = (model: Model, loaded: Loaded, h: Builder): Html => {
  const found = matching(loaded.memories, loaded.issues, model.filters);
  const shown = found.slice(0, model.shown);
  return h.div(
    [h.Class("flex flex-col gap-4")],
    [
      filters(model, h),
      loaded.overview.can_repair ? selection(model, loaded, h) : h.empty,
      found.length === 0
        ? nothing(
            loaded.memories.length === 0
              ? "This companion remembers nothing yet."
              : "No memory matches these filters.",
            h,
          )
        : framed(
            Table(
              {},
              [
                memoriesHead(model, shown, loaded.overview.can_repair, h),
                Table.body({}, shown.flatMap((memory) => memoryRows(model, memory, loaded, h)), h),
              ],
              h,
            ),
            h,
          ),
      h.div(
        [h.Class("flex flex-wrap items-center gap-3")],
        [
          h.p(
            [h.Class("type-meta")],
            [
              `${count(shown.length)} of ${count(found.length)} shown, of ${count(loaded.memories.length)} in all.`,
            ],
          ),
          shown.length < found.length
            ? button({ onClick: Message.ClickedShowMore(), variant: "outline" }, ["Show more"], h)
            : h.empty,
        ],
      ),
    ],
  );
};

// RECALL

const foundTable = (found: Found, h: Builder): Html =>
  h.div(
    [h.Class("flex flex-col gap-3")],
    [
      found.found.length === 0
        ? nothing("Nothing comes up for this.", h)
        : framed(
            Table(
              {},
              [
                head(["Relevance", "Score", "#", "Type", "Level", "Memory"], h),
                Table.body(
                  {},
                  found.found.map(({ memory, relevance, score }) =>
                    h.keyed("tr")(String(memory.id), [h.Class("border-b last:border-0")], [
                      Table.cell({ className: `${mono} align-top` }, [relevance.toFixed(2)], h),
                      Table.cell({ className: `${mono} align-top font-medium` }, [score.toFixed(2)], h),
                      Table.cell({ className: "align-top" }, [memoryLink(memory.id, h)], h),
                      Table.cell({ className: "align-top" }, [words(memory.kind)], h),
                      Table.cell({ className: "align-top" }, [levelBadge(memory.level, h)], h),
                      Table.cell({ className: "min-w-80 whitespace-normal" }, [memory.interpretation], h),
                    ]),
                  ),
                  h,
                ),
              ],
              h,
            ),
            h,
          ),
      h.p(
        [h.Class("type-meta")],
        [
          `${count(found.found.length)} of at most ${count(found.limit)} memories (min_relevance ${found.min_relevance}, min_score ${found.min_score}). Nothing was marked as recalled.`,
        ],
      ),
    ],
  );

const recallTab = (model: Model, h: Builder): Html =>
  h.div(
    [h.Class("flex flex-col gap-4")],
    [
      h.form(
        [h.OnSubmit(Message.SubmittedRecall()), h.Class("flex flex-wrap items-end gap-3")],
        [
          h.div(
            [h.Class("flex min-w-56 flex-1 flex-col gap-1.5")],
            [
              h.label([h.For("recall"), h.Class("type-label text-muted-foreground")], ["What you would say"]),
              h.input([
                h.Id("recall"),
                h.Type("search"),
                h.Value(model.recallQuery),
                h.OnInput((value) => Message.ChangedRecallQuery({ value })),
                h.Placeholder("how was the concert"),
                h.Class(`h-8 ${inputClass}`),
              ]),
            ],
          ),
          button(
            {
              type: "submit",
              size: "lg",
              isDisabled: model.recallQuery.trim() === "" || AsyncData.isPending(model.recall),
            },
            [icon(h, Search, "size-4"), "Recall"],
            h,
          ),
        ],
      ),
      AsyncData.matchDataSplitEmpty(model.recall, {
        onIdle: () =>
          nothing(
            "The search a turn runs, on the query alone: how close each memory is to it, and its score after level, age, mood and open threads.",
            h,
          ),
        onLoading: () => skeleton({ className: "h-32" }, [], h),
        onFailure: (error) => failure("Memory could not be searched", error, h),
        onData: (found) => section(`For “${model.recalledQuery}”`, foundTable(found, h), h),
      }),
    ],
  );

// LOG

const LOG_RANGES: ReadonlyArray<readonly [string, string]> = [
  ["1", "Last 24 hours"],
  ["7", "Last 7 days"],
  ["30", "Last 30 days"],
  ["all", "Everything"],
];

const logTable = (model: Model, overview: Overview, log: ReadonlyArray<WriteEntry>, h: Builder): Html =>
  log.length === 0
    ? nothing("Nothing was written in this period.", h)
    : framed(
        Table(
          {},
          [
            head(["When (UTC)", "Who", "What", "To", "Details", ...(overview.can_repair ? [""] : [])], h),
            Table.body(
              {},
              // Newest first: the log is read from now backwards.
              log.toReversed().map((entry) =>
                h.keyed("tr")(String(entry.id), [h.Class("border-b last:border-0")], [
                  Table.cell({ className: `${mono} align-top` }, [utc(entry.created_at)], h),
                  Table.cell({ className: "align-top" }, [badge({ variant: "outline" }, [entry.actor], h)], h),
                  Table.cell({ className: `${mono} align-top` }, [entry.action], h),
                  Table.cell(
                    { className: "align-top" },
                    entry.target === "memory" && entry.target_id !== null
                      ? [memoryLink(entry.target_id, h)]
                      : [h.span([h.Class(mono)], [entry.target_id === null ? entry.target : `${entry.target} #${entry.target_id}`])],
                    h,
                  ),
                  Table.cell(
                    { className: `${mono} min-w-64 break-all whitespace-normal text-muted-foreground` },
                    [JSON.stringify(entry.payload)],
                    h,
                  ),
                  ...(overview.can_repair
                    ? [
                        Table.cell(
                          { className: "align-top" },
                          [
                            button(
                              {
                                onClick: Message.ClickedRollback({ entry }),
                                variant: "outline",
                                size: "sm",
                                isDisabled: !overview.frozen || isRepairing(model),
                                attributes: [
                                  h.Title(
                                    overview.frozen
                                      ? "Undo this write and every later one"
                                      : "Freeze first: a rollback runs only while nothing else writes",
                                  ),
                                ],
                              },
                              [icon(h, Undo2, "size-3"), "Roll back…"],
                              h,
                            ),
                          ],
                          h,
                        ),
                      ]
                    : []),
                ]),
              ),
              h,
            ),
          ],
          h,
        ),
        h,
      );

const logTab = (model: Model, { overview }: Loaded, h: Builder): Html =>
  h.div(
    [h.Class("flex flex-col gap-4")],
    [
      h.div(
        [h.Class("flex flex-wrap items-end gap-3")],
        [
          select("range", "Period", model.logRange, (value) => Message.ChangedLogRange({ value }), LOG_RANGES, h),
          h.p(
            [h.Class("type-meta reading-measure pb-1.5")],
            [
              "Every write to the memory. A time here is what ",
              code("clanka doctor rollback --to", h),
              " takes.",
              ...(overview.can_repair
                ? [
                    overview.frozen
                      ? " Rolling back to a write undoes it and every write after it."
                      : " To roll back to a write, freeze first.",
                  ]
                : []),
            ],
          ),
        ],
      ),
      AsyncData.matchData(model.log, {
        onEmpty: () => skeleton({ className: "h-40" }, [], h),
        onFailure: (error) => failure("The log could not be read", error, h),
        onData: (log) => logTable(model, overview, log, h),
      }),
    ],
  );

// DOCTOR

/** A path as a shell takes it in one piece: on macOS a soul.md is under "Application Support". */
const quoted = (path: string) =>
  /^[\w@%+=:,./-]+$/.test(path) ? path : `'${path.replaceAll("'", "'\\''")}'`;

const repairCard = (
  title: string,
  description: ReadonlyArray<Html | string>,
  action: Html,
  h: Builder,
): Html =>
  h.div(
    [h.Class("flex flex-wrap items-start justify-between gap-4 rounded-lg border border-border p-4")],
    [
      h.div(
        [h.Class("flex min-w-0 flex-1 basis-80 flex-col gap-1")],
        [
          h.h3([h.Class("text-sm font-medium")], [title]),
          h.p([h.Class("type-meta reading-measure")], description),
        ],
      ),
      action,
    ],
  );

const SESSION_TITLE = {
  dream: "A dream is open",
  conversation: "A conversation is open",
  none: "No conversation is open",
} as const;

const doctorTab = (model: Model, { overview }: Loaded, h: Builder): Html => {
  if (!overview.can_repair) {
    return h.div(
      [h.Class("flex flex-col gap-4")],
      [
        notice(
          "plain",
          [
            "This run of clanka web only reads. To repair from this page, stop it and run ",
            code("clanka web --doctor", h),
            ". The same repairs are in ",
            code("clanka doctor", h),
            ".",
          ],
          h,
        ),
        h.p(
          [h.Class("type-meta")],
          [
            overview.frozen
              ? "Frozen: nothing writes to this memory."
              : "Not frozen: conversations write to this memory.",
          ],
        ),
      ],
    );
  }
  const isBusy = isRepairing(model);
  return h.div(
    [h.Class("flex flex-col gap-3")],
    [
      repairCard(
        overview.frozen ? "Frozen" : "Not frozen",
        [
          overview.frozen
            ? "Nothing writes to this memory, in any process that has the companion open, and nothing said meanwhile is recorded. Rolling back and re-reading work only now. Unfreeze when you are done."
            : "Freezing stops every write to this memory, in every process that has the companion open, until you unfreeze. Nothing said meanwhile is recorded. Rolling back and re-reading need it.",
        ],
        overview.frozen
          ? button(
              { onClick: Message.ClickedUnfreeze(), variant: "outline", size: "lg", isDisabled: isBusy },
              ["Unfreeze"],
              h,
            )
          : button(
              { onClick: Message.ClickedFreeze(), size: "lg", isDisabled: isBusy },
              [icon(h, Snowflake, "size-4"), "Freeze"],
              h,
            ),
        h,
      ),
      repairCard(
        SESSION_TITLE[overview.session],
        [
          "Ending a conversation that was left open keeps it from running into the next one. It ends without a night, and the memories already written from it stay. An open dream is dropped with nothing kept. Works frozen or not.",
        ],
        button(
          {
            onClick: Message.ClickedEndSession(),
            variant: "outline",
            size: "lg",
            isDisabled: isBusy || overview.session === "none",
          },
          [overview.session === "dream" ? "Drop the dream…" : "End the conversation…"],
          h,
        ),
        h,
      ),
      repairCard(
        "Delete or re-read memories",
        [
          "Tick them in Memories. A delete first shows the turns that leave the transcript with them. ",
          overview.model === null
            ? "Re-reading is off: config.toml has no [model]."
            : `Re-reading asks ${overview.model}, and needs the freeze.`,
        ],
        button({ onClick: Message.ClickedTab({ tab: "Memories" }), variant: "outline", size: "lg" }, ["Go to Memories"], h),
        h,
      ),
      repairCard(
        "Roll back",
        [
          "Pick a write in the Log to put the memory back as it was before it. You see what would be removed, restored and reopened before anything changes. Needs the freeze.",
        ],
        button({ onClick: Message.ClickedTab({ tab: "Log" }), variant: "outline", size: "lg" }, ["Go to Log"], h),
        h,
      ),
      repairCard(
        "Delete everything",
        [
          "Not on this page. It deletes every memory, the whole transcript and every session, and nothing brings them back. While frozen, in a terminal:",
          h.code(
            [h.Class(`${mono} mt-2 block rounded-md bg-muted px-2.5 py-2 break-all text-foreground select-all`)],
            [`clanka doctor delete-all ${quoted(overview.soul)}`],
          ),
        ],
        h.empty,
        h,
      ),
    ],
  );
};

/** The repair at work, or how the last one went, above whichever tab is open. */
const repairStatus = (model: Model, h: Builder): Html => {
  const box = (tone: "warn" | "plain", children: ReadonlyArray<Html>, canDismiss: boolean) =>
    h.div(
      [
        h.Role("status"),
        h.Class(
          `flex items-start justify-between gap-3 rounded-lg border px-4 py-2.5 text-sm ${
            tone === "warn" ? "border-destructive/40 bg-destructive/10" : "border-border bg-muted/50"
          }`,
        ),
      ],
      [
        h.div([h.Class("flex min-w-0 flex-col gap-1")], children),
        canDismiss
          ? button(
              {
                onClick: Message.ClickedDismissRepair(),
                variant: "ghost",
                size: "icon-sm",
                className: "shrink-0 text-muted-foreground hover:text-foreground",
                attributes: [h.AriaLabel("Dismiss")],
              },
              [icon(h, X, "size-3.5")],
              h,
            )
          : h.empty,
      ],
    );
  return Repair.match<Html>(model.repair, {
    Idle: () => h.empty,
    Running: ({ label }) => box("plain", [h.p([], [`${label}…`])], false),
    Rereading: ({ total, rewritten }) =>
      box(
        "plain",
        [
          h.p(
            [],
            [
              `Re-reading ${count(total)} ${total === 1 ? "memory" : "memories"}: ${count(rewritten)} rewritten so far. Each request waits on the model.`,
            ],
          ),
        ],
        false,
      ),
    Done: ({ summary, notes }) =>
      box(
        "plain",
        [
          h.p([], [summary]),
          notes.length === 0
            ? h.empty
            : h.ul(
                [h.Class("flex flex-col gap-0.5 type-meta")],
                notes.map((note, index) => h.keyed("li")(String(index), [], [note])),
              ),
        ],
        true,
      ),
    Failed: ({ error }) =>
      box(
        "warn",
        [h.p([h.Class("font-medium text-destructive")], ["The repair did not go through"]), h.p([], [error])],
        true,
      ),
  });
};

// THE QUESTION BEFORE A REPAIR

type Question = Readonly<{
  title: string;
  description: string;
  body: ReadonlyArray<Html>;
  action: string;
  isDestructive: boolean;
}>;

const numbers = (ids: ReadonlyArray<number>) => ids.map((id) => `#${id}`).join(", ");

const planBox = (children: ReadonlyArray<Html>, h: Builder): Html =>
  h.div([h.Class("flex flex-col gap-1.5 rounded-md border border-border bg-background p-3 text-sm")], children);

const idList = (label: string, ids: ReadonlyArray<number>, h: Builder): Html =>
  ids.length === 0 ? h.empty : h.p([], [`${label}: `, h.span([h.Class(mono)], [numbers(ids)])]);

const planFailure = (error: string, h: Builder): Html =>
  h.p(
    [h.Class("rounded-md border border-destructive/40 bg-destructive/10 p-3 text-sm")],
    [`What this would do could not be worked out, so it cannot run: ${error}`],
  );

/** How many of the memories a dialog names it also quotes. */
const QUOTED = 4;

const quotedMemories = (chosen: ReadonlyArray<Memory>, h: Builder): Html =>
  h.ul(
    [h.Class("flex flex-col gap-1 text-sm")],
    [
      ...chosen.slice(0, QUOTED).map((memory) =>
        h.keyed("li")(String(memory.id), [h.Class("flex items-baseline gap-2")], [
          h.span([h.Class(`${mono} shrink-0 text-muted-foreground`)], [`#${memory.id}`]),
          h.span([h.Class("line-clamp-1 min-w-0")], [memory.interpretation]),
        ]),
      ),
      chosen.length > QUOTED
        ? h.keyed("li")("more", [h.Class("type-meta")], [`and ${count(chosen.length - QUOTED)} more`])
        : h.empty,
    ],
  );

const question = (model: Model, loaded: Loaded, h: Builder): Option.Option<Question> => {
  const { overview } = loaded;
  const among = (ids: ReadonlyArray<number>) => loaded.memories.filter(({ id }) => ids.includes(id));
  const memories = (amount: number) => `${count(amount)} ${amount === 1 ? "memory" : "memories"}`;
  return Asking.match<Option.Option<Question>>(model.asking, {
    Nothing: () => Option.none(),

    EndSession: () =>
      Option.some(
        overview.session === "dream"
          ? {
              title: "Drop the open dream?",
              description: "The dream ends here and nothing of it is kept.",
              body: [],
              action: "Drop it",
              isDestructive: true,
            }
          : {
              title: "End the open conversation?",
              description:
                "It ends without a night: no episode is written and nothing drifts. Memories already written from it stay, and the next message starts a new conversation.",
              body: [],
              action: "End it",
              isDestructive: false,
            },
      ),

    Delete: ({ memories: ids, plan }) => {
      const chosen = among(ids);
      const seared = chosen.filter(({ level }) => level === "seared").length;
      return Option.some({
        title: `Delete ${memories(ids.length)} for good?`,
        description:
          "There is no undo. Each goes with its history and with the turns only it cites, so what was said there leaves the transcript too.",
        body: [
          quotedMemories(chosen, h),
          seared === 0
            ? h.empty
            : h.p(
                [h.Class("text-sm text-destructive")],
                [`${count(seared)} of them ${seared === 1 ? "is" : "are"} seared, the level that never fades or changes.`],
              ),
          AsyncData.matchData(plan, {
            onEmpty: () => skeleton({ className: "h-12" }, [], h),
            onFailure: (error) => planFailure(error, h),
            onData: ({ turns, restored }: Deletion) =>
              planBox(
                [
                  h.p(
                    [],
                    [
                      turns.length === 0
                        ? "No turn leaves the transcript with them."
                        : `${count(turns.length)} ${turns.length === 1 ? "turn leaves" : "turns leave"} the transcript with them.`,
                    ],
                  ),
                  idList("Current again, once what replaced them is gone", restored, h),
                ],
                h,
              ),
          }),
        ],
        action: `Delete ${memories(ids.length)}`,
        isDestructive: true,
      });
    },

    Reread: ({ memories: ids }) => {
      const { seared, uncited } = rereadable(loaded.memories, model.selected);
      const requests = Math.ceil(ids.length / REREAD_BATCH);
      return Option.some({
        title: `Have ${memories(ids.length)} re-read?`,
        description: `${overview.model ?? "The model"} reads the turns each was written from and writes how ${overview.name} sees it now. The old text stays in each memory's history.`,
        body: [
          quotedMemories(among(ids), h),
          seared.length + uncited.length === 0 && requests === 1
            ? h.empty
            : planBox(
                [
                  idList("Skipped, seared and never changed", seared, h),
                  idList("Skipped, they cite no turns to re-read", uncited, h),
                  requests === 1
                    ? h.empty
                    : h.p([], [`Sent ${REREAD_BATCH} at a time, in ${count(requests)} requests. Each waits on the model.`]),
                ],
                h,
              ),
        ],
        action: "Re-read",
        isDestructive: false,
      });
    },

    Rollback: ({ entry, plan, typedName }) =>
      Option.some({
        title: "Roll back to before this write?",
        description: `Every write to the memory from ${utc(entry.created_at)} UTC on is undone. There is no undo. What was said and the write log are kept.`,
        body: [
          h.p(
            [h.Class(`${mono} text-muted-foreground`)],
            [
              `${utc(entry.created_at)}  ${entry.actor}  ${entry.action}  ${
                entry.target_id === null ? entry.target : `${entry.target} #${entry.target_id}`
              }`,
            ],
          ),
          AsyncData.matchData(plan, {
            onEmpty: () => skeleton({ className: "h-16" }, [], h),
            onFailure: (error) => planFailure(error, h),
            onData: ({ plan: rollback }: RollbackPlan) =>
              planBox(
                [
                  h.p([], [`${rollbackSummary(rollback)}.`]),
                  idList("Removed", rollback.removed, h),
                  idList("Restored", rollback.restored, h),
                  idList("Reopened", rollback.reopened, h),
                  idList("Seared, so left as they are", rollback.kept_seared, h),
                ],
                h,
              ),
          }),
          h.div(
            [h.Class("flex flex-col gap-1.5")],
            [
              h.label(
                [h.For("typed-name"), h.Class("text-sm")],
                ["Type ", h.span([h.Class("font-medium")], [overview.name]), " to roll back"],
              ),
              h.input([
                h.Id("typed-name"),
                h.Type("text"),
                h.Value(typedName),
                h.OnInput((value) => Message.ChangedTypedName({ value })),
                h.Attribute("autocomplete", "off"),
                h.Class(`h-8 ${inputClass}`),
              ]),
            ],
          ),
        ],
        action: "Roll back",
        isDestructive: true,
      }),
  });
};

/** The one dialog: it asks whatever `model.asking` holds, and a yes is `ClickedConfirm`. */
const confirmDialog = (model: Model, h: Builder): Html => {
  const maybeQuestion = Option.flatMap(AsyncData.getData(model.loaded), (loaded) => question(model, loaded, h));
  return h.submodel({
    slotId: model.dialog.id,
    model: model.dialog,
    view: AlertDialog.view,
    viewInputs: AlertDialog.styledViewInputs<Message>(
      {
        hasDescription: true,
        panelClass:
          "max-h-[85vh] w-[calc(100vw-2rem)] overflow-y-auto data-[size=default]:max-w-md data-[size=default]:sm:max-w-lg",
        content: ({ closeButton, title, description }) =>
          Option.match(maybeQuestion, {
            onNone: () => [],
            onSome: (asked) => [
              AlertDialog.header(
                {},
                [
                  AlertDialog.title({ attributes: title }, [asked.title], h),
                  AlertDialog.description({ attributes: description }, [asked.description], h),
                ],
                h,
              ),
              ...asked.body,
              AlertDialog.footer(
                {},
                [
                  AlertDialog.cancelButton({ attributes: closeButton }, ["Cancel"], h),
                  AlertDialog.actionButton(
                    {
                      className: asked.isDestructive ? buttonVariants.destructive : undefined,
                      attributes: [
                        h.Type("button"),
                        h.OnClick(Message.ClickedConfirm()),
                        h.Disabled(!isConfirmable(model)),
                      ],
                    },
                    [asked.action],
                    h,
                  ),
                ],
                h,
              ),
            ],
          }),
      },
      h,
    ),
    toParentMessage: (message) => Message.GotDialogMessage({ message }),
  });
};

// COMPANION

const TABS: ReadonlyArray<Tab> = ["Overview", "Memories", "Recall", "Log", "Doctor"];

const tabs = (model: Model, h: Builder): Html =>
  h.nav(
    [h.Class("flex gap-1 border-b border-border"), h.AriaLabel("Views")],
    TABS.map((tab) =>
      h.keyed("button")(
        tab,
        [
          h.Type("button"),
          h.OnClick(Message.ClickedTab({ tab })),
          h.AriaCurrent(model.tab === tab ? "page" : "false"),
          h.Class(
            `-mb-px cursor-pointer border-b-2 px-3 py-2 text-sm transition-colors ${
              model.tab === tab
                ? "border-primary font-medium text-foreground"
                : "border-transparent text-muted-foreground hover:text-foreground"
            }`,
          ),
        ],
        [tab],
      ),
    ),
  );

const notice = (tone: "warn" | "plain", children: ReadonlyArray<Html | string>, h: Builder): Html =>
  h.p(
    [
      h.Role("status"),
      h.Class(
        `rounded-lg border px-4 py-2.5 text-sm ${
          tone === "warn" ? "border-destructive/40 bg-destructive/10" : "border-border bg-muted/50"
        }`,
      ),
    ],
    children,
  );

const code = (text: string, h: Builder) => h.code([h.Class(mono)], [text]);

/** What state the companion is in, when it is one a reader should know before reading on. */
const notices = (model: Model, overview: Overview, h: Builder): ReadonlyArray<Html> => [
  ...(overview.frozen
    ? [
        overview.can_repair
          ? h.div(
              [
                h.Role("status"),
                h.Class(
                  "flex flex-wrap items-center justify-between gap-3 rounded-lg border border-destructive/40 bg-destructive/10 px-4 py-2 text-sm",
                ),
              ],
              [
                h.p([], ["Frozen by the doctor: nothing writes to this memory until you unfreeze."]),
                button(
                  { onClick: Message.ClickedUnfreeze(), variant: "outline", isDisabled: isRepairing(model) },
                  ["Unfreeze"],
                  h,
                ),
              ],
            )
          : notice(
              "warn",
              ["Frozen by the doctor: nothing writes to this memory until ", code("clanka doctor unfreeze", h), "."],
              h,
            ),
      ]
    : []),
  ...(overview.persona_changed
    ? [
        notice(
          "warn",
          [
            "The persona in its soul.md changed since these memories were written. Nothing talks to it until you settle that with ",
            code("clanka soul continue", h),
            ", ",
            code("update", h),
            " or ",
            code("fork", h),
            ".",
          ],
          h,
        ),
      ]
    : []),
  ...(overview.archived
    ? [notice("plain", ["Archived by a fork: this memory is kept as it was and nothing writes to it."], h)]
    : []),
];

const SESSION = {
  dream: "in a dream",
  conversation: "in a conversation",
  none: "no conversation open",
} as const;

const companion = (model: Model, h: Builder): Html =>
  AsyncData.matchDataSplitEmpty(model.loaded, {
    onIdle: () => nothing("Choose a companion.", h),
    onLoading: () =>
      h.div(
        [h.Class("flex flex-col gap-6"), h.AriaBusy(true)],
        [skeleton({ className: "h-16" }, [], h), skeleton({ className: "h-64" }, [], h)],
      ),
    onFailure: (error) => failure("This companion could not be opened", error, h),
    onData: (loaded) => {
      const { overview } = loaded;
      return h.div(
        [h.Class("flex flex-col gap-6"), h.AriaBusy(AsyncData.isPending(model.loaded))],
        [
          h.div(
            [h.Class("flex flex-col gap-2")],
            [
              h.h1([h.Class("type-section-title")], [overview.name]),
              h.p(
                [h.Class("type-meta")],
                [
                  `${SESSION[overview.session]}${
                    overview.nights_waiting === 0
                      ? ""
                      : ` · ${count(overview.nights_waiting)} ${
                          overview.nights_waiting === 1 ? "night waits" : "nights wait"
                        } to be slept on`
                  } · `,
                  h.span([h.Class(mono)], [overview.soul]),
                ],
              ),
            ],
          ),
          ...notices(model, overview, h),
          repairStatus(model, h),
          tabs(model, h),
          model.tab === "Overview"
            ? overviewTab(loaded, h)
            : model.tab === "Memories"
              ? memoriesTab(model, loaded, h)
              : model.tab === "Recall"
                ? recallTab(model, h)
                : model.tab === "Log"
                  ? logTab(model, loaded, h)
                  : doctorTab(model, loaded, h),
        ],
      );
    },
  });

// VIEW

const noToken = (h: Builder): Html =>
  Empty(
    { className: "rounded-lg border border-dashed border-border" },
    [
      Empty.header(
        {},
        [
          Empty.title({}, ["This page needs the address clanka web printed"], h),
          Empty.description(
            {},
            [
              "It ends in a token made for that run, and the memories are only read out to a page that has it. Run clanka web again, or copy the whole address from its terminal.",
            ],
            h,
          ),
        ],
        h,
      ),
    ],
    h,
  );

export const view = (model: Model, h: Builder): Document => ({
  title: Option.match(AsyncData.getData(model.loaded), {
    onNone: () => "Memories | clanka",
    onSome: ({ overview }) => `${overview.name} | clanka`,
  }),
  body: h.div(
    [h.Class("min-h-screen")],
    [
      header(model, h),
      h.main(
        [h.Class("app-frame page-gutters flex min-h-[calc(100vh-2.75rem)] flex-col gap-8 py-8 lg:flex-row")],
        model.token === ""
          ? [h.div([h.Class("flex-1")], [noToken(h)])]
          : [
              isListShown(model) ? companions(model, h) : h.empty,
              h.div([h.Class("min-w-0 flex-1")], [companion(model, h)]),
            ],
      ),
      confirmDialog(model, h),
    ],
  ),
});
