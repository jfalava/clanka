import { Effect, Option, Result, Schema } from "effect";
import { AsyncData, Command, type Update } from "foldkit";
import { defineMessageUnion } from "foldkit/message";
import { modifyFields } from "foldkit/struct";

import { Detail, FactIssue, Found, Listed, Memory, Overview, WriteEntry, get, paths } from "./api";

// MODEL

export const Theme = Schema.Literals(["System", "Light", "Dark"]);
export type Theme = typeof Theme.Type;

/** What the page starts with: the saved theme and the token from the address `clanka web` printed. */
export const Flags = Schema.Struct({ theme: Theme, token: Schema.String });
export type Flags = typeof Flags.Type;

export const Tab = Schema.Literals(["Overview", "Memories", "Recall", "Log"]);
export type Tab = typeof Tab.Type;

/** Everything about one companion that the page reads at once. */
export const Loaded = Schema.Struct({
  overview: Overview,
  memories: Schema.Array(Memory),
  issues: Schema.Array(FactIssue),
});
export type Loaded = typeof Loaded.Type;

/** Which memories the table shows. An empty text is "any". */
export const Filters = Schema.Struct({
  kind: Schema.String,
  level: Schema.String,
  status: Schema.String,
  /** `unsaid` keeps the memories with a fact their turns never said. */
  facts: Schema.String,
  /** Words to find in a memory or its facts, or `#12` for the memory with that number. */
  text: Schema.String,
});
export type Filters = typeof Filters.Type;

export const LogRange = Schema.Literals(["1", "7", "30", "all"]);
export type LogRange = typeof LogRange.Type;

export const CompanionsData = AsyncData.Schema(Schema.Array(Listed), Schema.String);
export const LoadedData = AsyncData.Schema(Loaded, Schema.String);
export const DetailData = AsyncData.Schema(Detail, Schema.String);
export const FoundData = AsyncData.Schema(Found, Schema.String);
export const LogData = AsyncData.Schema(Schema.Array(WriteEntry), Schema.String);

/** How many memories the table shows before "Show more". */
export const PAGE = 100;

export const Model = Schema.Struct({
  token: Schema.String,
  theme: Theme,
  companions: CompanionsData.schema,
  maybeCompanion: Schema.Option(Schema.String),
  /** Raised each time another companion is chosen, so an answer about the last one is dropped. */
  generation: Schema.Int,
  loaded: LoadedData.schema,
  tab: Tab,
  filters: Filters,
  shown: Schema.Int,
  maybeOpenMemory: Schema.Option(Schema.Finite),
  detail: DetailData.schema,
  recallQuery: Schema.String,
  /** The query the results below the box are for. */
  recalledQuery: Schema.String,
  recall: FoundData.schema,
  logRange: LogRange,
  log: LogData.schema,
});
export type Model = typeof Model.Type;

// MESSAGE

export const FilterField = Schema.Literals(["kind", "level", "status", "facts", "text"]);

export const Message = defineMessageUnion({
  ClickedCompanion: { id: Schema.String },
  ClickedTab: { tab: Tab },
  ClickedRefresh: {},
  ClickedTheme: { theme: Theme },
  ChangedFilter: { field: FilterField, value: Schema.String },
  ClickedShowMore: {},
  ClickedMemory: { id: Schema.Finite },
  /** A memory's number somewhere else on the page: go to it in the table. */
  ClickedMemoryLink: { id: Schema.Finite },
  ChangedRecallQuery: { value: Schema.String },
  SubmittedRecall: {},
  ChangedLogRange: { value: Schema.String },
  SucceededFetchCompanions: { companions: Schema.Array(Listed) },
  FailedFetchCompanions: { error: Schema.String },
  SucceededFetchCompanion: { generation: Schema.Int, loaded: Loaded },
  FailedFetchCompanion: { generation: Schema.Int, error: Schema.String },
  SucceededFetchDetail: { generation: Schema.Int, memory: Schema.Finite, detail: Detail },
  FailedFetchDetail: { generation: Schema.Int, memory: Schema.Finite, error: Schema.String },
  SucceededFetchRecall: { generation: Schema.Int, query: Schema.String, found: Found },
  FailedFetchRecall: { generation: Schema.Int, query: Schema.String, error: Schema.String },
  SucceededFetchLog: { generation: Schema.Int, range: LogRange, log: Schema.Array(WriteEntry) },
  FailedFetchLog: { generation: Schema.Int, range: LogRange, error: Schema.String },
  CompletedSaveTheme: {},
});
export type Message = typeof Message.Type;

// COMMAND

const FetchCompanions = Command.define("FetchCompanions", {
  args: { token: Schema.String },
  messages: [Message.SucceededFetchCompanions, Message.FailedFetchCompanions],
  execute: ({ token }) =>
    get(token, paths.companions, Schema.Array(Listed)).pipe(
      Effect.map((companions) => Message.SucceededFetchCompanions({ companions })),
      Effect.catch((error) => Effect.succeed(Message.FailedFetchCompanions({ error }))),
    ),
});

const FetchCompanion = Command.define("FetchCompanion", {
  args: { token: Schema.String, generation: Schema.Int, id: Schema.String },
  messages: [Message.SucceededFetchCompanion, Message.FailedFetchCompanion],
  execute: ({ token, generation, id }) =>
    Effect.all(
      {
        overview: get(token, paths.overview(id), Overview),
        memories: get(token, paths.memories(id), Schema.Array(Memory)),
        issues: get(token, paths.facts(id), Schema.Array(FactIssue)),
      },
      { concurrency: "unbounded" },
    ).pipe(
      Effect.map((loaded) => Message.SucceededFetchCompanion({ generation, loaded })),
      Effect.catch((error) => Effect.succeed(Message.FailedFetchCompanion({ generation, error }))),
    ),
});

const FetchDetail = Command.define("FetchDetail", {
  args: {
    token: Schema.String,
    generation: Schema.Int,
    id: Schema.String,
    memory: Schema.Finite,
  },
  messages: [Message.SucceededFetchDetail, Message.FailedFetchDetail],
  execute: ({ token, generation, id, memory }) =>
    get(token, paths.memory(id, memory), Detail).pipe(
      Effect.map((detail) => Message.SucceededFetchDetail({ generation, memory, detail })),
      Effect.catch((error) =>
        Effect.succeed(Message.FailedFetchDetail({ generation, memory, error })),
      ),
    ),
});

const FetchRecall = Command.define("FetchRecall", {
  args: { token: Schema.String, generation: Schema.Int, id: Schema.String, query: Schema.String },
  messages: [Message.SucceededFetchRecall, Message.FailedFetchRecall],
  execute: ({ token, generation, id, query }) =>
    get(token, paths.recall(id, query), Found).pipe(
      Effect.map((found) => Message.SucceededFetchRecall({ generation, query, found })),
      Effect.catch((error) =>
        Effect.succeed(Message.FailedFetchRecall({ generation, query, error })),
      ),
    ),
});

const DAY_MS = 24 * 60 * 60 * 1000;

const FetchLog = Command.define("FetchLog", {
  args: { token: Schema.String, generation: Schema.Int, id: Schema.String, range: LogRange },
  messages: [Message.SucceededFetchLog, Message.FailedFetchLog],
  execute: ({ token, generation, id, range }) =>
    // "The last 7 days" is counted from when the log is asked for.
    Effect.sync(() => (range === "all" ? null : Date.now() - Number(range) * DAY_MS)).pipe(
      Effect.flatMap((since) => get(token, paths.log(id, since), Schema.Array(WriteEntry))),
      Effect.map((log) => Message.SucceededFetchLog({ generation, range, log })),
      Effect.catch((error) => Effect.succeed(Message.FailedFetchLog({ generation, range, error }))),
    ),
});

const THEME_COOKIE = "jfa-theme";
const YEAR_SECONDS = 365 * 24 * 60 * 60;

/** jfa.dev's theme cookie and `.dark` class (common/src/preferences.ts, hooks/use-theme.tsx). */
export const applyTheme = (theme: Theme) => {
  const isDark =
    theme === "Dark" ||
    (theme === "System" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", isDark);
  document.documentElement.style.colorScheme = isDark ? "dark" : "light";
};

const SaveTheme = Command.define("SaveTheme", {
  args: { theme: Theme },
  messages: [Message.CompletedSaveTheme],
  execute: ({ theme }) =>
    Effect.sync(() => {
      document.cookie = `${THEME_COOKIE}=${theme.toLowerCase()}; path=/; max-age=${YEAR_SECONDS}; samesite=lax`;
      applyTheme(theme);
      return Message.CompletedSaveTheme();
    }),
});

// FILTERING

export const everything: Filters = { kind: "", level: "", status: "", facts: "", text: "" };
/** What the table opens on: what the companion holds now, without what it replaced. */
export const current: Filters = { ...everything, status: "active" };

const factText = (value: unknown) => (typeof value === "string" ? value : JSON.stringify(value));

/** The memories the filters keep, newest first. */
export const matching = (
  memories: ReadonlyArray<Memory>,
  issues: ReadonlyArray<FactIssue>,
  filters: Filters,
): ReadonlyArray<Memory> => {
  const unsaid = new Set(issues.map(({ memory }) => memory));
  const text = filters.text.trim().toLowerCase();
  const number = /^#(\d+)$/.exec(text);
  const hasText = (memory: Memory) =>
    memory.interpretation.toLowerCase().includes(text) ||
    Object.entries(memory.facts).some(
      ([name, value]) =>
        name.toLowerCase().includes(text) || factText(value).toLowerCase().includes(text),
    );
  return memories
    .filter(
      (memory) =>
        (filters.kind === "" || memory.kind === filters.kind) &&
        (filters.level === "" || memory.level === filters.level) &&
        (filters.status === "" || memory.status === filters.status) &&
        (filters.facts !== "unsaid" || unsaid.has(memory.id)) &&
        (number !== null ? memory.id === Number(number[1]) : text === "" || hasText(memory)),
    )
    .toSorted((a, b) => b.id - a.id);
};

// UPDATE

type Return = Update.Return<Model, Message>;

const reloading = <A>(data: AsyncData.AsyncData<A, string>) =>
  Option.getOrElse(AsyncData.revalidateOrLoad(data), () => data);

const fetchLog = (model: Model, id: string, generation: number): Return => ({
  model: modifyFields(model, { log: reloading }),
  commands: [FetchLog({ token: model.token, generation, id, range: model.logRange })],
});

/** Chooses a companion and asks for everything about it. */
const choose = (model: Model, id: string): Return => {
  const generation = model.generation + 1;
  const chosen = modifyFields(model, {
    generation: () => generation,
    maybeCompanion: () => Option.some(id),
    loaded: () => LoadedData.Loading(),
    filters: () => current,
    shown: () => PAGE,
    maybeOpenMemory: () => Option.none(),
    detail: () => DetailData.Idle(),
    recallQuery: () => "",
    recalledQuery: () => "",
    recall: () => FoundData.Idle(),
    log: () => LogData.Idle(),
  });
  const companion = FetchCompanion({ token: model.token, generation, id });
  if (model.tab === "Log") {
    const withLog = fetchLog(chosen, id, generation);
    return { model: withLog.model, commands: [companion, ...(withLog.commands ?? [])] };
  } else {
    return { model: chosen, commands: [companion] };
  }
};

/** Asks again for what is on the page, keeping what is shown until the answers come. */
const refresh = (model: Model): Return => {
  const companions = FetchCompanions({ token: model.token });
  return Option.match(model.maybeCompanion, {
    onNone: () => ({
      model: modifyFields(model, { companions: reloading }),
      commands: [companions],
    }),
    onSome: (id) => {
      const generation = model.generation + 1;
      const token = model.token;
      return {
        model: modifyFields(model, {
          generation: () => generation,
          companions: reloading,
          loaded: reloading,
          detail: Option.isSome(model.maybeOpenMemory) ? reloading : () => DetailData.Idle(),
          log: AsyncData.isIdle(model.log) ? () => LogData.Idle() : reloading,
        }),
        commands: [
          companions,
          FetchCompanion({ token, generation, id }),
          ...Option.match(model.maybeOpenMemory, {
            onNone: () => [],
            onSome: (memory) => [FetchDetail({ token, generation, id, memory })],
          }),
          ...(AsyncData.isIdle(model.log)
            ? []
            : [FetchLog({ token, generation, id, range: model.logRange })]),
        ],
      };
    },
  });
};

const openMemory = (model: Model, memory: number): Return =>
  Option.match(model.maybeCompanion, {
    onNone: () => ({ model }),
    onSome: (id) => ({
      model: modifyFields(model, {
        maybeOpenMemory: () => Option.some(memory),
        detail: () => DetailData.Loading(),
      }),
      commands: [FetchDetail({ token: model.token, generation: model.generation, id, memory })],
    }),
  });

export const update = (model: Model, message: Message) =>
  Message.match<Return>(message, {
    ClickedCompanion: ({ id }) =>
      Option.contains(model.maybeCompanion, id) ? { model } : choose(model, id),

    ClickedTab: ({ tab }) => {
      const shown = modifyFields(model, { tab: () => tab });
      return tab === "Log" && AsyncData.isIdle(model.log)
        ? Option.match(model.maybeCompanion, {
            onNone: () => ({ model: shown }),
            onSome: (id) => fetchLog(shown, id, model.generation),
          })
        : { model: shown };
    },

    ClickedRefresh: () => refresh(model),

    ClickedTheme: ({ theme }) => ({
      model: modifyFields(model, { theme: () => theme }),
      commands: [SaveTheme({ theme })],
    }),

    ChangedFilter: ({ field, value }) => ({
      model: modifyFields(model, {
        filters: (filters) => ({ ...filters, [field]: value }),
        shown: () => PAGE,
      }),
    }),

    ClickedShowMore: () => ({ model: modifyFields(model, { shown: (shown) => shown + PAGE }) }),

    ClickedMemory: ({ id }) =>
      Option.contains(model.maybeOpenMemory, id)
        ? {
            model: modifyFields(model, {
              maybeOpenMemory: () => Option.none(),
              detail: () => DetailData.Idle(),
            }),
          }
        : openMemory(model, id),

    ClickedMemoryLink: ({ id }) =>
      openMemory(
        modifyFields(model, {
          tab: () => "Memories" as const,
          filters: () => ({ ...everything, text: `#${id}` }),
          shown: () => PAGE,
        }),
        id,
      ),

    ChangedRecallQuery: ({ value }) => ({
      model: modifyFields(model, { recallQuery: () => value }),
    }),

    SubmittedRecall: () => {
      const query = model.recallQuery.trim();
      return Option.match(model.maybeCompanion, {
        onNone: () => ({ model }),
        onSome: (id) =>
          query === ""
            ? { model }
            : {
                model: modifyFields(model, {
                  recalledQuery: () => query,
                  recall: () => FoundData.Loading(),
                }),
                commands: [
                  FetchRecall({ token: model.token, generation: model.generation, id, query }),
                ],
              },
      });
    },

    ChangedLogRange: ({ value }) => {
      const ranged = modifyFields(model, {
        logRange: (range) => (Schema.is(LogRange)(value) ? value : range),
      });
      return Option.match(model.maybeCompanion, {
        onNone: () => ({ model: ranged }),
        onSome: (id) => fetchLog(ranged, id, model.generation),
      });
    },

    SucceededFetchCompanions: ({ companions }) => {
      const listed = modifyFields(model, {
        companions: AsyncData.settle<ReadonlyArray<Listed>, string>(Result.succeed(companions)),
      });
      // With nobody chosen yet, the first companion that has a memory to show.
      const first = companions.find(({ id, db_bytes }) => id !== null && db_bytes !== null);
      return Option.isNone(model.maybeCompanion) && first?.id != null
        ? choose(listed, first.id)
        : { model: listed };
    },

    FailedFetchCompanions: ({ error }) => ({
      model: modifyFields(model, {
        companions: AsyncData.settle<ReadonlyArray<Listed>, string>(Result.fail(error)),
      }),
    }),

    SucceededFetchCompanion: ({ generation, loaded }) =>
      generation === model.generation
        ? {
            model: modifyFields(model, {
              loaded: AsyncData.settle<Loaded, string>(Result.succeed(loaded)),
            }),
          }
        : { model },

    FailedFetchCompanion: ({ generation, error }) =>
      generation === model.generation
        ? {
            model: modifyFields(model, {
              loaded: AsyncData.settle<Loaded, string>(Result.fail(error)),
            }),
          }
        : { model },

    SucceededFetchDetail: ({ generation, memory, detail }) =>
      generation === model.generation && Option.contains(model.maybeOpenMemory, memory)
        ? {
            model: modifyFields(model, {
              detail: AsyncData.settle<Detail, string>(Result.succeed(detail)),
            }),
          }
        : { model },

    FailedFetchDetail: ({ generation, memory, error }) =>
      generation === model.generation && Option.contains(model.maybeOpenMemory, memory)
        ? {
            model: modifyFields(model, {
              detail: AsyncData.settle<Detail, string>(Result.fail(error)),
            }),
          }
        : { model },

    SucceededFetchRecall: ({ generation, query, found }) =>
      generation === model.generation && query === model.recalledQuery
        ? {
            model: modifyFields(model, {
              recall: AsyncData.settle<Found, string>(Result.succeed(found)),
            }),
          }
        : { model },

    FailedFetchRecall: ({ generation, query, error }) =>
      generation === model.generation && query === model.recalledQuery
        ? {
            model: modifyFields(model, {
              recall: AsyncData.settle<Found, string>(Result.fail(error)),
            }),
          }
        : { model },

    SucceededFetchLog: ({ generation, range, log }) =>
      generation === model.generation && range === model.logRange
        ? {
            model: modifyFields(model, {
              log: AsyncData.settle<ReadonlyArray<WriteEntry>, string>(Result.succeed(log)),
            }),
          }
        : { model },

    FailedFetchLog: ({ generation, range, error }) =>
      generation === model.generation && range === model.logRange
        ? {
            model: modifyFields(model, {
              log: AsyncData.settle<ReadonlyArray<WriteEntry>, string>(Result.fail(error)),
            }),
          }
        : { model },

    CompletedSaveTheme: () => ({ model }),
  });

// INIT

export const init = ({ theme, token }: Flags): Return => ({
  model: {
    token,
    theme,
    // Without the token there is nothing to ask for; the view says how to get one.
    companions: token === "" ? CompanionsData.Idle() : CompanionsData.Loading(),
    maybeCompanion: Option.none(),
    generation: 0,
    loaded: LoadedData.Idle(),
    tab: "Overview",
    filters: current,
    shown: PAGE,
    maybeOpenMemory: Option.none(),
    detail: DetailData.Idle(),
    recallQuery: "",
    recalledQuery: "",
    recall: FoundData.Idle(),
    logRange: "7",
    log: LogData.Idle(),
  },
  commands: token === "" ? [] : [FetchCompanions({ token })],
});
