import { Effect, Option, Result, Schema } from "effect";
import { AsyncData, Command, Update } from "foldkit";
import { defineMessageUnion } from "foldkit/message";
import { defineTaggedUnion } from "foldkit/schema";
import { modifyFields } from "foldkit/struct";

import {
  Deletion,
  Detail,
  Ended,
  FactIssue,
  Found,
  Frozen,
  Listed,
  Memory,
  Overview,
  REREAD_BATCH,
  Reread,
  Rollback,
  RollbackPlan,
  WriteEntry,
  get,
  paths,
  post,
} from "./api";
import * as AlertDialog from "./components/ui/alert-dialog";

// MODEL

export const Theme = Schema.Literals(["System", "Light", "Dark"]);
export type Theme = typeof Theme.Type;

/**
 * What the page starts with: the saved theme, the token from the address `clanka web` printed, and
 * whether the list of companions was left open.
 */
export const Flags = Schema.Struct({
  theme: Theme,
  token: Schema.String,
  isCompanionsOpen: Schema.Boolean,
});
export type Flags = typeof Flags.Type;

export const Tab = Schema.Literals(["Overview", "Memories", "Recall", "Log", "Doctor"]);
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

export const DeletionData = AsyncData.Schema(Deletion, Schema.String);
export const RollbackPlanData = AsyncData.Schema(RollbackPlan, Schema.String);

/**
 * What the dialog asks before a repair runs. A delete and a rollback come with the server's plan:
 * what they would remove, asked for when the dialog opens. The last question stays while the
 * dialog closes, so its words do not vanish under the animation.
 */
export const Asking = defineTaggedUnion({
  Nothing: {},
  EndSession: {},
  Delete: { memories: Schema.Array(Schema.Finite), plan: DeletionData.schema },
  Reread: { memories: Schema.Array(Schema.Finite) },
  /** Undo every write from this log entry on. The companion's name is typed to confirm. */
  Rollback: { entry: WriteEntry, plan: RollbackPlanData.schema, typedName: Schema.String },
});
export type Asking = typeof Asking.Type;

/** The repair under way, or how the last one went. */
export const Repair = defineTaggedUnion({
  Idle: {},
  Running: { label: Schema.String },
  /** Re-reading goes a batch at a time: what is still to send, and what came of the rest. */
  Rereading: {
    waiting: Schema.Array(Schema.Finite),
    total: Schema.Finite,
    rewritten: Schema.Finite,
    refused: Schema.Array(Schema.String),
  },
  Done: { summary: Schema.String, notes: Schema.Array(Schema.String) },
  Failed: { error: Schema.String },
});
export type Repair = typeof Repair.Type;

/** How many memories the table shows before "Show more". */
export const PAGE = 100;

export const Model = Schema.Struct({
  token: Schema.String,
  theme: Theme,
  /** Whether the list of companions is open beside the one being read. */
  isCompanionsOpen: Schema.Boolean,
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
  /** The memories ticked in the table, to delete or re-read together. */
  selected: Schema.Array(Schema.Finite),
  asking: Asking,
  dialog: AlertDialog.Model,
  repair: Repair,
});
export type Model = typeof Model.Type;

// MESSAGE

export const FilterField = Schema.Literals(["kind", "level", "status", "facts", "text"]);

export const Message = defineMessageUnion({
  ClickedCompanion: { id: Schema.String },
  ClickedTab: { tab: Tab },
  ClickedRefresh: {},
  ClickedTheme: { theme: Theme },
  ClickedToggleCompanions: {},
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
  CompletedSaveCompanionsOpen: {},
  ClickedFreeze: {},
  ClickedUnfreeze: {},
  ClickedEndSession: {},
  ToggledMemory: { id: Schema.Finite },
  /** The table's own checkbox: tick every memory it shows, or none of them. */
  ToggledShownMemories: { memories: Schema.Array(Schema.Finite), isSelected: Schema.Boolean },
  ClickedClearSelection: {},
  ClickedDelete: { memories: Schema.Array(Schema.Finite) },
  ClickedReread: { memories: Schema.Array(Schema.Finite) },
  ClickedRollback: { entry: WriteEntry },
  ChangedTypedName: { value: Schema.String },
  ClickedConfirm: {},
  ClickedDismissRepair: {},
  GotDialogMessage: { message: AlertDialog.Message },
  SucceededFetchDeletePlan: {
    generation: Schema.Int,
    memories: Schema.Array(Schema.Finite),
    plan: Deletion,
  },
  FailedFetchDeletePlan: {
    generation: Schema.Int,
    memories: Schema.Array(Schema.Finite),
    error: Schema.String,
  },
  SucceededFetchRollbackPlan: { generation: Schema.Int, entry: Schema.Finite, plan: RollbackPlan },
  FailedFetchRollbackPlan: { generation: Schema.Int, entry: Schema.Finite, error: Schema.String },
  /** Froze or unfroze: the banner says which, so nothing more is said. */
  SucceededFreeze: {},
  SucceededRepair: { summary: Schema.String, notes: Schema.Array(Schema.String) },
  FailedRepair: { error: Schema.String },
  SucceededRereadBatch: { companion: Schema.String, reread: Reread },
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

// REPAIRS
//
// Each is one POST, the core call its `clanka doctor` command makes. What they say when they are
// done is what the command prints.

const repaired = (summary: string, notes: ReadonlyArray<string> = []) =>
  Message.SucceededRepair({ summary, notes });
const failedRepair = (error: string) => Effect.succeed(Message.FailedRepair({ error }));
const numbers = (ids: ReadonlyArray<number>) => ids.map((id) => `#${id}`).join(", ");
const plural = (count: number, one: string, many: string) =>
  `${count.toLocaleString("en-GB")} ${count === 1 ? one : many}`;

const Freeze = Command.define("Freeze", {
  args: { token: Schema.String, id: Schema.String, isFrozen: Schema.Boolean },
  messages: [Message.SucceededFreeze, Message.FailedRepair],
  execute: ({ token, id, isFrozen }) =>
    post(token, isFrozen ? paths.freeze(id) : paths.unfreeze(id), Frozen).pipe(
      Effect.map(() => Message.SucceededFreeze()),
      Effect.catch(failedRepair),
    ),
});

const ENDED = {
  dream: "Dropped the open dream; nothing of it is kept.",
  conversation: "Ended the open conversation. The next message starts a new one.",
  none: "No conversation was open.",
} as const;

const EndSession = Command.define("EndSession", {
  args: { token: Schema.String, id: Schema.String },
  messages: [Message.SucceededRepair, Message.FailedRepair],
  execute: ({ token, id }) =>
    post(token, paths.endSession(id), Ended).pipe(
      Effect.map(({ ended }) => repaired(ENDED[ended])),
      Effect.catch(failedRepair),
    ),
});

const FetchDeletePlan = Command.define("FetchDeletePlan", {
  args: {
    token: Schema.String,
    generation: Schema.Int,
    id: Schema.String,
    memories: Schema.Array(Schema.Finite),
  },
  messages: [Message.SucceededFetchDeletePlan, Message.FailedFetchDeletePlan],
  execute: ({ token, generation, id, memories }) =>
    get(token, paths.deletePlan(id, memories), Deletion).pipe(
      Effect.map((plan) => Message.SucceededFetchDeletePlan({ generation, memories, plan })),
      Effect.catch((error) =>
        Effect.succeed(Message.FailedFetchDeletePlan({ generation, memories, error })),
      ),
    ),
});

const DeleteMemories = Command.define("DeleteMemories", {
  args: { token: Schema.String, id: Schema.String, memories: Schema.Array(Schema.Finite) },
  messages: [Message.SucceededRepair, Message.FailedRepair],
  execute: ({ token, id, memories }) =>
    post(token, paths.delete(id), Deletion, { ids: memories }).pipe(
      Effect.map(({ turns, restored }) =>
        repaired(
          turns.length === 0
            ? `Deleted ${plural(memories.length, "memory", "memories")}. No turn left the transcript with ${memories.length === 1 ? "it" : "them"}.`
            : `Deleted ${plural(memories.length, "memory", "memories")} and ${plural(turns.length, "turn", "turns")} only ${memories.length === 1 ? "it" : "they"} cited.`,
          restored.length === 0
            ? []
            : [`Current again, now that what replaced ${restored.length === 1 ? "it" : "them"} is gone: ${numbers(restored)}.`],
        ),
      ),
      Effect.catch(failedRepair),
    ),
});

const FetchRollbackPlan = Command.define("FetchRollbackPlan", {
  args: {
    token: Schema.String,
    generation: Schema.Int,
    id: Schema.String,
    entry: Schema.Finite,
    to: Schema.Finite,
  },
  messages: [Message.SucceededFetchRollbackPlan, Message.FailedFetchRollbackPlan],
  execute: ({ token, generation, id, entry, to }) =>
    get(token, paths.rollbackPlan(id, to), RollbackPlan).pipe(
      Effect.map((plan) => Message.SucceededFetchRollbackPlan({ generation, entry, plan })),
      Effect.catch((error) =>
        Effect.succeed(Message.FailedFetchRollbackPlan({ generation, entry, error })),
      ),
    ),
});

/** What a rollback does, in the words `clanka doctor rollback` prints. */
export const rollbackSummary = (rollback: Rollback) =>
  `${plural(rollback.removed.length, "memory", "memories")} removed, ${rollback.restored.length.toLocaleString("en-GB")} restored to their earlier text, ${plural(rollback.reopened.length, "thread", "threads")} reopened, ${plural(rollback.state_rows, "mood, note and section entry", "mood, note and section entries")} dropped`;

const RollBack = Command.define("RollBack", {
  args: { token: Schema.String, id: Schema.String, to: Schema.Finite, seen: Schema.Finite },
  messages: [Message.SucceededRepair, Message.FailedRepair],
  execute: ({ token, id, to, seen }) =>
    post(token, paths.rollback(id), Rollback, { to, seen }).pipe(
      Effect.map((rollback) =>
        repaired(
          `Rolled back to ${new Date(to).toISOString().slice(0, 19)} UTC: ${rollbackSummary(rollback)}.`,
          rollback.kept_seared.length === 0
            ? []
            : [`Seared memories are never rolled back; left as they are: ${numbers(rollback.kept_seared)}.`],
        ),
      ),
      Effect.catch(failedRepair),
    ),
});

const RereadBatch = Command.define("RereadBatch", {
  args: { token: Schema.String, id: Schema.String, memories: Schema.Array(Schema.Finite) },
  messages: [Message.SucceededRereadBatch, Message.FailedRepair],
  execute: ({ token, id, memories }) =>
    post(token, paths.reinterpret(id), Reread, { ids: memories }).pipe(
      Effect.map((reread) => Message.SucceededRereadBatch({ companion: id, reread })),
      Effect.catch(failedRepair),
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

/** Where the browser keeps whether the list of companions was left open. */
export const COMPANIONS_KEY = "clanka-web-companions";

const SaveCompanionsOpen = Command.define("SaveCompanionsOpen", {
  args: { isOpen: Schema.Boolean },
  messages: [Message.CompletedSaveCompanionsOpen],
  execute: ({ isOpen }) =>
    Effect.sync(() => {
      localStorage.setItem(COMPANIONS_KEY, isOpen ? "open" : "closed");
      return Message.CompletedSaveCompanionsOpen();
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

/**
 * Which of these memories the companion can re-read: `clanka doctor reinterpret` refuses a seared
 * memory, and one that cites no turns has nothing to re-read. The dialog lists the rest as skipped.
 */
export const rereadable = (all: ReadonlyArray<Memory>, chosen: ReadonlyArray<number>) => {
  const picked = all.filter(({ id }) => chosen.includes(id));
  const seared = picked.filter(({ level }) => level === "seared");
  const uncited = picked.filter(
    ({ level, source_turns }) => level !== "seared" && source_turns.length === 0,
  );
  const skipped = new Set([...seared, ...uncited].map(({ id }) => id));
  return {
    memories: picked.filter(({ id }) => !skipped.has(id)).map(({ id }) => id),
    seared: seared.map(({ id }) => id),
    uncited: uncited.map(({ id }) => id),
  };
};

/**
 * Whether the dialog's question can be answered yes. A delete and a rollback wait for their plan,
 * since that is what the owner agrees to, and a rollback for the companion's name typed out.
 */
export const isConfirmable = (model: Model): boolean =>
  Asking.match(model.asking, {
    Nothing: () => false,
    EndSession: () => true,
    Delete: ({ plan }) => Option.isSome(AsyncData.getData(plan)),
    Reread: ({ memories }) => memories.length > 0,
    Rollback: ({ plan, typedName }) =>
      Option.isSome(AsyncData.getData(plan)) &&
      Option.exists(
        AsyncData.getData(model.loaded),
        ({ overview }) => overview.name === typedName.trim(),
      ),
  });

/** Whether a repair is at work: nothing else is started until it is done. */
export const isRepairing = ({ repair }: Model) =>
  repair._tag === "Running" || repair._tag === "Rereading";

// UPDATE

type Return = Update.Return<Model, Message>;

const foldDialogOutMessage = AlertDialog.OutMessage.match<Update.Step<Model, Message>>({
  Opened: () => (model) => ({ model }),
  Closed: () => (model) => ({ model }),
});

const foldDialog = Update.foldChild({
  update: AlertDialog.update,
  read: (model: Model) => Option.some(model.dialog),
  write: (model, dialog) => modifyFields(model, { dialog: () => dialog }),
  toParentMessage: (message) => Message.GotDialogMessage({ message }),
  foldOutMessage: foldDialogOutMessage,
});

const openDialog = Update.foldChildStep({
  update: AlertDialog.open,
  read: (model: Model) => Option.some(model.dialog),
  write: (model, dialog) => modifyFields(model, { dialog: () => dialog }),
  toParentMessage: (message) => Message.GotDialogMessage({ message }),
  foldOutMessage: foldDialogOutMessage,
});

const closeDialog = Update.foldChildStep({
  update: AlertDialog.close,
  read: (model: Model) => Option.some(model.dialog),
  write: (model, dialog) => modifyFields(model, { dialog: () => dialog }),
  toParentMessage: (message) => Message.GotDialogMessage({ message }),
  foldOutMessage: foldDialogOutMessage,
});

/** Puts a question in the dialog and opens it, with the commands that fetch its plan. */
const ask = (
  model: Model,
  asking: Asking,
  commands: Update.Commands<Message> = [],
): Return =>
  isRepairing(model)
    ? { model }
    : Update.combine(model, [
        (current) => ({ model: modifyFields(current, { asking: () => asking }), commands }),
        openDialog,
      ]);

/** Starts a repair: its command runs and nothing else starts until it answers. */
const run = (model: Model, label: string, command: Update.Commands<Message>[number]): Return =>
  isRepairing(model)
    ? { model }
    : {
        model: modifyFields(model, { repair: () => Repair.Running({ label }) }),
        commands: [command],
      };

/** Sends the next batch of memories to re-read, or says how all of them went. */
const reread = (
  model: Model,
  id: string,
  progress: Omit<typeof Repair.Rereading.Type, "_tag">,
): Return => {
  if (progress.waiting.length === 0) {
    return afterRepair(
      model,
      Repair.Done({
        summary: `Re-read ${progress.rewritten.toLocaleString("en-GB")} of ${plural(progress.total, "memory", "memories")}.`,
        notes: progress.refused,
      }),
    );
  } else {
    return {
      model: modifyFields(model, {
        repair: () =>
          Repair.Rereading({ ...progress, waiting: progress.waiting.slice(REREAD_BATCH) }),
      }),
      commands: [
        RereadBatch({ token: model.token, id, memories: progress.waiting.slice(0, REREAD_BATCH) }),
      ],
    };
  }
};

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
    selected: () => [],
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

/**
 * A repair is over: says how it went and reads the companion again, as Refresh does. The ticks
 * are kept through a freeze, which is the step before re-reading them, and dropped otherwise.
 */
const afterRepair = (model: Model, repair: Repair): Return =>
  refresh(
    modifyFields(model, {
      repair: () => repair,
      selected: (selected) => (repair._tag === "Idle" ? selected : []),
    }),
  );

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

    ClickedToggleCompanions: () => ({
      model: modifyFields(model, { isCompanionsOpen: (isOpen) => !isOpen }),
      commands: [SaveCompanionsOpen({ isOpen: !model.isCompanionsOpen })],
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

    CompletedSaveCompanionsOpen: () => ({ model }),

    ClickedFreeze: () =>
      Option.match(model.maybeCompanion, {
        onNone: () => ({ model }),
        onSome: (id) => run(model, "Freezing", Freeze({ token: model.token, id, isFrozen: true })),
      }),

    ClickedUnfreeze: () =>
      Option.match(model.maybeCompanion, {
        onNone: () => ({ model }),
        onSome: (id) =>
          run(model, "Unfreezing", Freeze({ token: model.token, id, isFrozen: false })),
      }),

    ClickedEndSession: () => ask(model, Asking.EndSession()),

    ToggledMemory: ({ id }) => ({
      model: modifyFields(model, {
        selected: (selected) =>
          selected.includes(id) ? selected.filter((other) => other !== id) : [...selected, id],
      }),
    }),

    ToggledShownMemories: ({ memories, isSelected }) => ({
      model: modifyFields(model, {
        selected: (selected) =>
          isSelected
            ? [...new Set([...selected, ...memories])]
            : selected.filter((id) => !memories.includes(id)),
      }),
    }),

    ClickedClearSelection: () => ({ model: modifyFields(model, { selected: () => [] }) }),

    ClickedDelete: ({ memories }) =>
      Option.match(model.maybeCompanion, {
        onNone: () => ({ model }),
        onSome: (id) =>
          memories.length === 0
            ? { model }
            : ask(model, Asking.Delete({ memories, plan: DeletionData.Loading() }), [
                FetchDeletePlan({
                  token: model.token,
                  generation: model.generation,
                  id,
                  memories,
                }),
              ]),
      }),

    ClickedReread: ({ memories }) =>
      memories.length === 0 ? { model } : ask(model, Asking.Reread({ memories })),

    ClickedRollback: ({ entry }) =>
      Option.match(model.maybeCompanion, {
        onNone: () => ({ model }),
        onSome: (id) =>
          ask(
            model,
            Asking.Rollback({ entry, plan: RollbackPlanData.Loading(), typedName: "" }),
            [
              FetchRollbackPlan({
                token: model.token,
                generation: model.generation,
                id,
                entry: entry.id,
                to: entry.created_at,
              }),
            ],
          ),
      }),

    ChangedTypedName: ({ value }) => ({
      model: modifyFields(model, {
        asking: (asking) =>
          asking._tag === "Rollback" ? Asking.Rollback({ ...asking, typedName: value }) : asking,
      }),
    }),

    ClickedConfirm: () =>
      Option.match(model.maybeCompanion, {
        onNone: () => ({ model }),
        onSome: (id) => {
          if (!isConfirmable(model)) {
            return { model };
          }
          const token = model.token;
          const start = Asking.match<Update.Step<Model, Message>>(model.asking, {
            Nothing: () => (current) => ({ model: current }),
            EndSession: () => (current) =>
              run(current, "Ending the conversation", EndSession({ token, id })),
            Delete: ({ memories }) => (current) =>
              run(current, "Deleting", DeleteMemories({ token, id, memories })),
            Reread: ({ memories }) => (current) =>
              isRepairing(current)
                ? { model: current }
                : reread(current, id, {
                    waiting: memories,
                    total: memories.length,
                    rewritten: 0,
                    refused: [],
                  }),
            // Without the plan there is nothing the owner has agreed to.
            Rollback: ({ entry, plan }) => (current) =>
              Option.match(AsyncData.getData(plan), {
                onNone: () => ({ model: current }),
                onSome: ({ seen }) =>
                  run(
                    current,
                    "Rolling back",
                    RollBack({ token, id, to: entry.created_at, seen }),
                  ),
              }),
          });
          return Update.combine(model, [start, closeDialog]);
        },
      }),

    ClickedDismissRepair: () => ({
      model: modifyFields(model, {
        repair: (repair) => (isRepairing(model) ? repair : Repair.Idle()),
      }),
    }),

    GotDialogMessage: ({ message }) => foldDialog(model, message),

    SucceededFetchDeletePlan: ({ generation, memories, plan }) => ({
      model: modifyFields(model, {
        asking: (asking) =>
          generation === model.generation &&
          asking._tag === "Delete" &&
          asking.memories.join() === memories.join()
            ? Asking.Delete({
                memories,
                plan: AsyncData.settle<Deletion, string>(Result.succeed(plan))(asking.plan),
              })
            : asking,
      }),
    }),

    FailedFetchDeletePlan: ({ generation, memories, error }) => ({
      model: modifyFields(model, {
        asking: (asking) =>
          generation === model.generation &&
          asking._tag === "Delete" &&
          asking.memories.join() === memories.join()
            ? Asking.Delete({
                memories,
                plan: AsyncData.settle<Deletion, string>(Result.fail(error))(asking.plan),
              })
            : asking,
      }),
    }),

    SucceededFetchRollbackPlan: ({ generation, entry, plan }) => ({
      model: modifyFields(model, {
        asking: (asking) =>
          generation === model.generation &&
          asking._tag === "Rollback" &&
          asking.entry.id === entry
            ? Asking.Rollback({
                ...asking,
                plan: AsyncData.settle<RollbackPlan, string>(Result.succeed(plan))(asking.plan),
              })
            : asking,
      }),
    }),

    FailedFetchRollbackPlan: ({ generation, entry, error }) => ({
      model: modifyFields(model, {
        asking: (asking) =>
          generation === model.generation &&
          asking._tag === "Rollback" &&
          asking.entry.id === entry
            ? Asking.Rollback({
                ...asking,
                plan: AsyncData.settle<RollbackPlan, string>(Result.fail(error))(asking.plan),
              })
            : asking,
      }),
    }),

    SucceededFreeze: () => afterRepair(model, Repair.Idle()),

    SucceededRepair: ({ summary, notes }) => afterRepair(model, Repair.Done({ summary, notes })),

    // What the repair did before it failed is on the disk: read it again too.
    FailedRepair: ({ error }) =>
      afterRepair(
        model,
        Repair.Failed({
          error:
            model.repair._tag === "Rereading"
              ? `${error} (${model.repair.rewritten.toLocaleString("en-GB")} of ${model.repair.total.toLocaleString("en-GB")} were re-read before that.)`
              : error,
        }),
      ),

    SucceededRereadBatch: ({ companion, reread: { rewritten, refused } }) =>
      model.repair._tag === "Rereading"
        ? reread(model, companion, {
            waiting: model.repair.waiting,
            total: model.repair.total,
            rewritten: model.repair.rewritten + rewritten,
            refused: [...model.repair.refused, ...refused],
          })
        : { model },
  });

// INIT

export const init = ({ theme, token, isCompanionsOpen }: Flags): Return => ({
  model: {
    token,
    theme,
    isCompanionsOpen,
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
    selected: [],
    asking: Asking.Nothing(),
    dialog: AlertDialog.init({ id: "confirm", isAnimated: false }),
    repair: Repair.Idle(),
  },
  commands: token === "" ? [] : [FetchCompanions({ token })],
});
