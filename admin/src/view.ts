import { Option } from "effect";
import { AsyncData } from "foldkit";
import type { Document, Html, HtmlBuilder } from "foldkit/html";
import { ChevronDown, ChevronRight, Monitor, Moon, RefreshCw, Sun } from "lucide";

import type { EventRow, Overview, Range } from "./api";
import { badge } from "./components/ui/badge";
import { button } from "./components/ui/button";
import { Card } from "./components/ui/card";
import { Empty } from "./components/ui/empty";
import { nativeSelect, nativeSelectOption } from "./components/ui/native-select";
import { skeleton } from "./components/ui/skeleton";
import { Table } from "./components/ui/table";
import { icon } from "./lib/icons";
import { Message, type Model, type Theme } from "./main";

type Builder = HtmlBuilder<Message>;

const RANGES: ReadonlyArray<readonly [Range, string]> = [
  ["1", "Last 24 hours"],
  ["7", "Last 7 days"],
  ["30", "Last 30 days"],
  ["90", "Last 90 days"],
];

const count = (value: number) => value.toLocaleString("en-GB");
const when = (at: number) =>
  new Date(at).toLocaleString("en-GB", { dateStyle: "medium", timeStyle: "short" });
const ms = (value: number | null) => (value === null ? "—" : `${count(value)} ms`);
const word = (value: string | null) => value ?? "—";

// HEADER

const NEXT_THEME: Record<Theme, Theme> = { System: "Light", Light: "Dark", Dark: "System" };
const THEME_ICON = { System: Monitor, Light: Sun, Dark: Moon } as const;

/** jfa.dev's SiteHeader: the brand on the left, the app's actions, a divider, the theme. */
const header = (model: Model, h: Builder): Html =>
  h.header(
    [h.Class("site-header sticky top-0 z-30 shrink-0 border-b border-border bg-background")],
    [
      h.div(
        [h.Class("header-gutters flex min-h-11 items-center justify-between gap-4 sm:gap-6")],
        [
          h.div(
            [h.Class("flex min-w-0 items-baseline gap-3 truncate"), h.AriaLabel("CLANKA by JFA")],
            [
              h.span(
                [h.Class("shrink-0 text-sm font-bold tracking-tight text-primary")],
                ["CLANKA", h.span([h.Class("pl-0.5 text-xs tracking-tight")], ["by JFA"])],
              ),
              h.span([h.Class("hidden text-xs text-muted-foreground/75 sm:inline")], ["/"]),
              h.span(
                [h.Class("hidden truncate text-xs text-muted-foreground sm:inline")],
                ["Diagnostics sent by installs that opted in"],
              ),
            ],
          ),
          h.nav(
            [h.Class("flex shrink-0 items-center gap-1"), h.AriaLabel("Console")],
            [
              button(
                {
                  onClick: Message.ClickedRefresh(),
                  variant: "ghost",
                  size: "lg",
                  isDisabled: AsyncData.isPending(model.overview),
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

// FILTERS

const filters = (model: Model, h: Builder): Html => {
  const overview = AsyncData.getData(model.overview);
  const senders = Option.match(overview, { onNone: () => [], onSome: ({ senders }) => senders });
  const events = Option.match(overview, { onNone: () => [], onSome: ({ events }) => events });
  const select = (
    id: string,
    label: string,
    field: "days" | "sender" | "event" | "errors",
    value: string,
    options: ReadonlyArray<readonly [string, string]>,
  ) =>
    nativeSelect(
      {
        id,
        label,
        value,
        onChange: (value) => Message.ChangedFilter({ field, value }),
        options: options.map(([value, label]) => nativeSelectOption({ value, label }, h)),
        wrapperClass: "w-auto",
        className: "min-w-36",
      },
      h,
    );
  return h.div(
    [h.Class("flex flex-wrap items-end gap-3")],
    [
      select("range", "Period", "days", model.filters.days, RANGES),
      select("sender", "Sender", "sender", model.filters.sender ?? "", [
        ["", "Every sender"],
        ...senders.map((name) => [name, name] as const),
      ]),
      select("event", "Event", "event", model.filters.event ?? "", [
        ["", "Every event"],
        ...events.map((name) => [name, name] as const),
      ]),
      select("errors", "Outcome", "errors", model.filters.errors ?? "", [
        ["", "Everything"],
        ["only", "Failures only"],
      ]),
    ],
  );
};

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

const stats = (overview: Overview, h: Builder): Html => {
  const { totals } = overview;
  return h.div(
    [h.Class("grid grid-cols-2 gap-3 md:grid-cols-5")],
    [
      stat("Events", count(totals.events), h),
      stat("Failures", count(totals.errors), h, totals.errors > 0 ? "text-destructive" : ""),
      stat("Installs", count(totals.installs), h),
      stat("Senders", count(totals.senders), h),
      stat("Last event", totals.lastAt === null ? "—" : when(totals.lastAt), h, "text-sm! pt-2"),
    ],
  );
};

/** Events per day as bars, failures stacked in red on top of the rest. */
const days = (overview: Overview, h: Builder): Html => {
  const highest = Math.max(1, ...overview.byDay.map(({ count }) => count));
  return section(
    "Per day",
    h.div(
      [
        h.Class("flex h-32 items-end gap-1 rounded-lg border border-border p-3"),
        h.Role("img"),
        h.AriaLabel("Events per day"),
      ],
      overview.byDay.map(({ day, count: total, errors }) =>
        h.keyed("div")(
          day,
          [
            h.Class("flex h-full min-w-1 flex-1 flex-col justify-end"),
            h.Title(`${day}: ${count(total)} events, ${count(errors)} failures`),
          ],
          [
            h.div([h.Class("bg-destructive"), h.Style({ height: `${(errors / highest) * 100}%` })]),
            h.div([
              h.Class("rounded-t-sm bg-primary"),
              h.Style({ height: `${((total - errors) / highest) * 100}%` }),
            ]),
          ],
        ),
      ),
    ),
    h,
  );
};

const section = (title: string, content: Html, h: Builder): Html =>
  h.section(
    [h.Class("flex flex-col gap-2")],
    [h.h2([h.Class("type-label text-muted-foreground")], [title]), content],
  );

const framed = (content: Html, h: Builder) =>
  h.div([h.Class("overflow-hidden rounded-lg border border-border")], [content]);

const nothing = (text: string, h: Builder) =>
  h.p([h.Class("rounded-lg border border-dashed border-border p-4 type-meta")], [text]);

const head = (labels: ReadonlyArray<string>, h: Builder) =>
  Table.header(
    {},
    [
      Table.row(
        {},
        labels.map((label) => Table.head({}, [label], h)),
        h,
      ),
    ],
    h,
  );

const mono = "font-mono text-xs";

const byEvent = (overview: Overview, h: Builder): Html =>
  section(
    "By event",
    overview.byEvent.length === 0
      ? nothing("No events in this period.", h)
      : framed(
          Table(
            {},
            [
              head(["Event", "About", "Count", "Failures", "Average"], h),
              Table.body(
                {},
                overview.byEvent.map((row) =>
                  Table.row(
                    {},
                    [
                      Table.cell({ className: mono }, [row.event], h),
                      Table.cell(
                        { className: `${mono} text-muted-foreground` },
                        [word(row.detail)],
                        h,
                      ),
                      Table.cell({ className: mono }, [count(row.count)], h),
                      Table.cell(
                        {
                          className: `${mono} ${row.errors > 0 ? "text-destructive" : "text-muted-foreground"}`,
                        },
                        [count(row.errors)],
                        h,
                      ),
                      Table.cell(
                        { className: `${mono} text-muted-foreground` },
                        [ms(row.avgMs)],
                        h,
                      ),
                    ],
                    h,
                  ),
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

const byError = (overview: Overview, h: Builder): Html =>
  section(
    "Failures",
    overview.byError.length === 0
      ? nothing("Nothing failed in this period.", h)
      : framed(
          Table(
            {},
            [
              head(["Error", "Event", "Count", "Last"], h),
              Table.body(
                {},
                overview.byError.map((row) =>
                  Table.row(
                    {},
                    [
                      Table.cell({}, [badge({ variant: "destructive" }, [row.error], h)], h),
                      Table.cell({ className: mono }, [row.event], h),
                      Table.cell({ className: mono }, [count(row.count)], h),
                      Table.cell({ className: "type-meta" }, [when(row.lastAt)], h),
                    ],
                    h,
                  ),
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

const installs = (overview: Overview, h: Builder): Html =>
  section(
    "Installs",
    overview.installs.length === 0
      ? nothing("No install sent anything in this period.", h)
      : framed(
          Table(
            {},
            [
              head(
                ["Sender", "Install", "Host", "Version", "Platform", "Events", "Failures", "Last"],
                h,
              ),
              Table.body(
                {},
                overview.installs.map((row) =>
                  Table.row(
                    {},
                    [
                      Table.cell({}, [badge({ variant: "outline" }, [row.sender], h)], h),
                      Table.cell(
                        { className: `${mono} text-muted-foreground` },
                        [row.install.slice(0, 12)],
                        h,
                      ),
                      Table.cell({ className: mono }, [word(row.host)], h),
                      Table.cell({ className: mono }, [word(row.version)], h),
                      Table.cell(
                        { className: `${mono} text-muted-foreground` },
                        [row.os === null ? "—" : `${row.os}/${word(row.arch)}`],
                        h,
                      ),
                      Table.cell({ className: mono }, [count(row.events)], h),
                      Table.cell(
                        {
                          className: `${mono} ${row.errors > 0 ? "text-destructive" : "text-muted-foreground"}`,
                        },
                        [count(row.errors)],
                        h,
                      ),
                      Table.cell({ className: "type-meta" }, [when(row.lastAt)], h),
                    ],
                    h,
                  ),
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

// EVENTS

const COLUMNS = 7;

const eventRows = (model: Model, row: EventRow, h: Builder): ReadonlyArray<Html> => {
  const isOpen = Option.contains(model.maybeOpenEvent, row.id);
  const summary = h.keyed("tr")(
    `event-${row.id}`,
    [
      h.Class("cursor-pointer border-b transition-colors hover:bg-muted/50"),
      h.OnClick(Message.ClickedEvent({ id: row.id })),
      h.AriaExpanded(isOpen),
    ],
    [
      Table.cell(
        { className: "w-6 text-muted-foreground" },
        [icon(h, isOpen ? ChevronDown : ChevronRight, "size-3.5")],
        h,
      ),
      Table.cell({ className: "type-meta" }, [when(row.at)], h),
      Table.cell({ className: mono }, [row.event], h),
      Table.cell(
        { className: `${mono} text-muted-foreground` },
        [word(row.pass ?? row.companion)],
        h,
      ),
      Table.cell(
        {},
        [
          row.error === null
            ? h.span([h.Class("type-meta")], ["—"])
            : badge({ variant: "destructive" }, [row.error], h),
        ],
        h,
      ),
      Table.cell({ className: `${mono} text-muted-foreground` }, [ms(row.ms)], h),
      Table.cell({}, [badge({ variant: "outline" }, [row.sender], h)], h),
    ],
  );
  if (!isOpen) {
    return [summary];
  }
  return [
    summary,
    h.keyed("tr")(
      `body-${row.id}`,
      [h.Class("border-b bg-muted/30")],
      [
        h.td(
          [h.Attribute("colspan", String(COLUMNS)), h.Class("px-3 py-3")],
          [
            h.p(
              [h.Class("type-meta pb-2")],
              [
                `#${row.id} · install ${row.install} · ${word(row.host)} ${word(row.version)} · received ${when(row.receivedAt)}`,
              ],
            ),
            h.pre(
              [
                h.Class(
                  "overflow-x-auto rounded-md border border-border bg-background p-3 font-mono text-xs",
                ),
              ],
              [JSON.stringify(JSON.parse(row.body), null, 2)],
            ),
          ],
        ),
      ],
    ),
  ];
};

const recent = (model: Model, h: Builder): Html =>
  section(
    "Events",
    AsyncData.matchData(model.listed, {
      onEmpty: () => skeleton({ className: "h-40" }, [], h),
      onFailure: (error) => failure(error, h),
      onData: (listed) =>
        listed.events.length === 0
          ? nothing("No events match these filters.", h)
          : h.div(
              [h.Class("flex flex-col gap-3")],
              [
                framed(
                  Table(
                    {},
                    [
                      head(["", "When", "Event", "About", "Error", "Took", "Sender"], h),
                      Table.body(
                        {},
                        listed.events.flatMap((row) => eventRows(model, row, h)),
                        h,
                      ),
                    ],
                    h,
                  ),
                  h,
                ),
                listed.next === null
                  ? h.p(
                      [h.Class("type-meta")],
                      [`${count(listed.events.length)} events, all shown.`],
                    )
                  : button(
                      {
                        onClick: Message.ClickedLoadMore(),
                        variant: "outline",
                        isDisabled: model.isLoadingMore,
                        className: "self-start",
                      },
                      [model.isLoadingMore ? "Loading…" : "Show older events"],
                      h,
                    ),
              ],
            ),
    }),
    h,
  );

// VIEW

const failure = (error: string, h: Builder): Html =>
  Empty(
    { className: "rounded-lg border border-dashed border-destructive/40" },
    [
      Empty.header(
        {},
        [
          Empty.title({ className: "text-destructive" }, ["The console could not load this"], h),
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

const loading = (h: Builder): Html =>
  h.div(
    [h.Class("flex flex-col gap-6"), h.AriaBusy(true)],
    [
      h.div(
        [h.Class("grid grid-cols-2 gap-3 md:grid-cols-5")],
        [1, 2, 3, 4, 5].map((index) =>
          h.keyed("div")(String(index), [], [skeleton({ className: "h-20" }, [], h)]),
        ),
      ),
      skeleton({ className: "h-32" }, [], h),
    ],
  );

const overview = (model: Model, h: Builder): Html =>
  AsyncData.matchData(model.overview, {
    onEmpty: () => loading(h),
    onFailure: (error) => failure(error, h),
    onData: (data) =>
      h.div(
        [h.Class("flex flex-col gap-8"), h.AriaBusy(AsyncData.isPending(model.overview))],
        [
          stats(data, h),
          days(data, h),
          h.div([h.Class("grid gap-8 xl:grid-cols-2")], [byEvent(data, h), byError(data, h)]),
          installs(data, h),
        ],
      ),
  });

export const view = (model: Model, h: Builder): Document => ({
  title: "Telemetry | clanka",
  body: h.div(
    [h.Class("min-h-screen")],
    [
      header(model, h),
      h.main(
        [h.Class("app-frame page-gutters flex min-h-[calc(100vh-2.75rem)] flex-col gap-8 py-8")],
        [
          h.div(
            [h.Class("flex flex-col gap-2")],
            [
              h.h1([h.Class("type-section-title")], ["Telemetry"]),
              h.p(
                [h.Class("type-meta reading-measure")],
                [
                  "Numbers and fixed words clanka's installs sent, never what was said. Read-only; events are kept for 90 days.",
                ],
              ),
            ],
          ),
          filters(model, h),
          overview(model, h),
          recent(model, h),
        ],
      ),
    ],
  ),
});
