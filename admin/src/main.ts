import { Effect, Option, Result, Schema } from "effect";
import { FetchHttpClient } from "effect/http";
import { HttpApiClient } from "effect/http-api";
import { AsyncData, Command, type Update } from "foldkit";
import { defineMessageUnion } from "foldkit/message";
import { modifyFields } from "foldkit/struct";

import { AdminApi, Cursor, EventRow, Filters, Overview, Range, errorText } from "./api";

// MODEL

export const Theme = Schema.Literals(["System", "Light", "Dark"]);
export type Theme = typeof Theme.Type;

export const OverviewData = AsyncData.Schema(Overview, Schema.String);

/** The events listed so far, newest first, and where the next page starts. */
export const Listed = Schema.Struct({
  events: Schema.Array(EventRow),
  next: Schema.NullOr(Cursor),
});
export type Listed = typeof Listed.Type;
export const ListedData = AsyncData.Schema(Listed, Schema.String);

export const Model = Schema.Struct({
  filters: Filters,
  /** Raised on every new request, so an answer to filters since changed is dropped. */
  generation: Schema.Int,
  overview: OverviewData.schema,
  listed: ListedData.schema,
  isLoadingMore: Schema.Boolean,
  maybeOpenEvent: Schema.Option(Schema.Finite),
  theme: Theme,
});
export type Model = typeof Model.Type;

// MESSAGE

export const FilterField = Schema.Literals(["days", "sender", "event", "errors"]);

export const Message = defineMessageUnion({
  ChangedFilter: { field: FilterField, value: Schema.String },
  ClickedRefresh: {},
  ClickedLoadMore: {},
  ClickedEvent: { id: Schema.Finite },
  ClickedTheme: { theme: Theme },
  SucceededFetchOverview: { generation: Schema.Int, overview: Overview },
  FailedFetchOverview: { generation: Schema.Int, error: Schema.String },
  SucceededFetchEvents: { generation: Schema.Int, page: Listed, isAppend: Schema.Boolean },
  FailedFetchEvents: { generation: Schema.Int, error: Schema.String },
  CompletedSaveTheme: {},
});
export type Message = typeof Message.Type;

// COMMAND

const client = HttpApiClient.make(AdminApi).pipe(Effect.provide(FetchHttpClient.layer));

const FetchOverview = Command.define("FetchOverview", {
  args: { generation: Schema.Int, filters: Filters },
  messages: [Message.SucceededFetchOverview, Message.FailedFetchOverview],
  execute: ({ generation, filters }) =>
    client.pipe(
      Effect.flatMap((api) => api.telemetry.overview({ query: filters })),
      Effect.map((overview) => Message.SucceededFetchOverview({ generation, overview })),
      Effect.catch((cause) =>
        Effect.succeed(Message.FailedFetchOverview({ generation, error: errorText(cause) })),
      ),
    ),
});

const FetchEvents = Command.define("FetchEvents", {
  args: { generation: Schema.Int, filters: Filters, before: Schema.NullOr(Cursor) },
  messages: [Message.SucceededFetchEvents, Message.FailedFetchEvents],
  execute: ({ generation, filters, before }) =>
    client.pipe(
      Effect.flatMap((api) =>
        api.telemetry.events({ query: { ...filters, before: before ?? undefined } }),
      ),
      Effect.map((page) =>
        Message.SucceededFetchEvents({ generation, page, isAppend: before !== null }),
      ),
      Effect.catch((cause) =>
        Effect.succeed(Message.FailedFetchEvents({ generation, error: errorText(cause) })),
      ),
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
      const secure = location.protocol === "https:" ? "; secure" : "";
      document.cookie = `${THEME_COOKIE}=${theme.toLowerCase()}; path=/; max-age=${YEAR_SECONDS}; samesite=lax${secure}`;
      applyTheme(theme);
      return Message.CompletedSaveTheme();
    }),
});

// UPDATE

/** Asks again for everything under the current filters; answers to older requests are dropped. */
const reload = (model: Model): Update.Return<Model, Message> => {
  const generation = model.generation + 1;
  return {
    model: modifyFields(model, {
      generation: () => generation,
      overview: (overview) =>
        Option.getOrElse(AsyncData.revalidateOrLoad(overview), () => overview),
      listed: (listed) => Option.getOrElse(AsyncData.revalidateOrLoad(listed), () => listed),
      isLoadingMore: () => false,
    }),
    commands: [
      FetchOverview({ generation, filters: model.filters }),
      FetchEvents({ generation, filters: model.filters, before: null }),
    ],
  };
};

const withFilter = (filters: Filters, field: typeof FilterField.Type, value: string): Filters => {
  if (field === "days") {
    return { ...filters, days: Schema.is(Range)(value) ? value : filters.days };
  } else if (field === "errors") {
    return { ...filters, errors: value === "only" ? "only" : undefined };
  } else {
    return { ...filters, [field]: value === "" ? undefined : value };
  }
};

export const update = (model: Model, message: Message) =>
  Message.match<Update.Return<Model, Message>>(message, {
    ChangedFilter: ({ field, value }) =>
      reload(
        modifyFields(model, {
          filters: (filters) => withFilter(filters, field, value),
          maybeOpenEvent: () => Option.none(),
        }),
      ),

    ClickedRefresh: () => reload(model),

    ClickedLoadMore: () => {
      const maybeNext = Option.flatMap(AsyncData.getData(model.listed), ({ next }) =>
        Option.fromNullishOr(next),
      );
      if (model.isLoadingMore || Option.isNone(maybeNext)) {
        return { model };
      } else {
        return {
          model: modifyFields(model, { isLoadingMore: () => true }),
          commands: [
            FetchEvents({
              generation: model.generation,
              filters: model.filters,
              before: maybeNext.value,
            }),
          ],
        };
      }
    },

    ClickedEvent: ({ id }) => ({
      model: modifyFields(model, {
        maybeOpenEvent: (maybeOpen) =>
          Option.contains(maybeOpen, id) ? Option.none() : Option.some(id),
      }),
    }),

    ClickedTheme: ({ theme }) => ({
      model: modifyFields(model, { theme: () => theme }),
      commands: [SaveTheme({ theme })],
    }),

    SucceededFetchOverview: ({ generation, overview }) =>
      generation === model.generation
        ? {
            model: modifyFields(model, {
              overview: AsyncData.settle<Overview, string>(Result.succeed(overview)),
            }),
          }
        : { model },

    FailedFetchOverview: ({ generation, error }) =>
      generation === model.generation
        ? { model: modifyFields(model, { overview: AsyncData.settle<Overview, string>(Result.fail(error)) }) }
        : { model },

    SucceededFetchEvents: ({ generation, page, isAppend }) => {
      if (generation !== model.generation) {
        return { model };
      }
      const shown = isAppend
        ? Option.match(AsyncData.getData(model.listed), {
            onNone: () => page,
            onSome: (listed) => ({ events: [...listed.events, ...page.events], next: page.next }),
          })
        : page;
      return {
        model: modifyFields(model, {
          listed: AsyncData.settle<Listed, string>(Result.succeed(shown)),
          isLoadingMore: () => false,
        }),
      };
    },

    FailedFetchEvents: ({ generation, error }) =>
      generation === model.generation
        ? {
            model: modifyFields(model, {
              listed: AsyncData.settle<Listed, string>(Result.fail(error)),
              isLoadingMore: () => false,
            }),
          }
        : { model },

    CompletedSaveTheme: () => ({ model }),
  });

// INIT

const initialFilters: Filters = { days: "7" };

// The flags are the saved theme. Not `Runtime.ApplicationInit<Model, Message, Theme>`: its
// `Flags extends void` test distributes over the union of themes and asks for no argument.
export const init = (theme: Theme): Update.Return<Model, Message> => ({
  model: {
    filters: initialFilters,
    generation: 0,
    overview: OverviewData.Loading(),
    listed: ListedData.Loading(),
    isLoadingMore: false,
    maybeOpenEvent: Option.none(),
    theme,
  },
  commands: [
    FetchOverview({ generation: 0, filters: initialFilters }),
    FetchEvents({ generation: 0, filters: initialFilters, before: null }),
  ],
});
