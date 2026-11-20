// What profiling decides, apart from VS Code: the events offered, the
// options checked, the environment telling gradle_task.py what was chosen,
// and the result it writes back (docs/spec/profiling.md). `profiler.ts`
// asks, runs and shows.

import { RUN_TESTS_COMMAND, type LensCommand } from "./debugging";

/** The command of the "Profile test" lenses. */
export const PROFILE_TESTS_COMMAND = "kotlin-analyzer.profileTests";

/** The command profiling a Gradle task of the active file's project. */
export const PROFILE_TASK_COMMAND = "kotlin-analyzer.profileTask";

/** The task type of profiled runs (`tasks.json` may hold them too). */
export const PROFILE_TASK_TYPE = "kotlin-profile";

/** What async-profiler can profile, as gradle_task.py names it. */
export interface ProfilerEvent {
  name: "cpu" | "alloc" | "lock" | "wall" | "all";
  label: string;
  detail: string;
}

export const PROFILER_EVENTS: readonly ProfilerEvent[] = [
  { name: "cpu", label: "CPU", detail: "where CPU time goes" },
  { name: "alloc", label: "Allocations", detail: "what allocates memory, by bytes" },
  { name: "lock", label: "Locks", detail: "where threads wait for monitors and locks" },
  { name: "wall", label: "Wall clock", detail: "every thread's time, sleeping and waiting included" },
  { name: "all", label: "CPU, allocations and locks", detail: "the three at once, a flame graph each" },
];

/** What to profile and with which more agent options (comma-separated). */
export interface ProfileSettings {
  event: ProfilerEvent["name"];
  options: string;
}

export const DEFAULT_SETTINGS: ProfileSettings = { event: "cpu", options: "" };

/**
 * Why `options` can't be used (the recording's file is gradle_task.py's), or
 * `undefined` when they can.
 */
export function refusedOptions(options: string): string | undefined {
  const names = options.split(",").map((option) => option.split("=")[0].trim());
  return names.includes("file") ? "The recording's file is kotlin-analyzer's: leave out file=" : undefined;
}

/** The events in the order to offer them: the last chosen first. */
export function eventsLastFirst(last: ProfileSettings["event"]): ProfilerEvent[] {
  const chosen = PROFILER_EVENTS.filter((event) => event.name === last);
  return [...chosen, ...PROFILER_EVENTS.filter((event) => event.name !== last)];
}

/**
 * The environment telling gradle_task.py what was chosen, so it asks
 * nothing, and where to write the flame graphs' paths.
 */
export function profileEnvironment(
  settings: ProfileSettings,
  resultFile: string,
  gradleTask?: string,
): Record<string, string> {
  return {
    KOTLIN_ANALYZER_PROFILER_EVENT: settings.event,
    KOTLIN_ANALYZER_PROFILER_OPTIONS: settings.options,
    KOTLIN_ANALYZER_PROFILE_RESULT: resultFile,
    ...(gradleTask === undefined ? {} : { KOTLIN_ANALYZER_PROFILED_TASK: gradleTask }),
  };
}

/** What gradle_task.py wrote after a profiled run. */
export interface ProfileResult {
  title: string;
  event: string;
  /** The flame graphs' HTML files. */
  pages: string[];
  /** The directory of the recordings and the pages. */
  recordings: string;
  gradleExitCode: number;
}

/** The result in `text`; `undefined` when it isn't one. */
export function parseProfileResult(text: string): ProfileResult | undefined {
  try {
    const parsed = JSON.parse(text) as Partial<ProfileResult>;
    const pagesAreFiles = Array.isArray(parsed.pages) && parsed.pages.every((page) => typeof page === "string");
    if (!pagesAreFiles || typeof parsed.recordings !== "string") {
      return undefined;
    }
    return {
      title: String(parsed.title ?? ""),
      event: String(parsed.event ?? ""),
      pages: parsed.pages as string[],
      recordings: parsed.recordings,
      gradleExitCode: Number(parsed.gradleExitCode ?? 0),
    };
  } catch {
    return undefined;
  }
}

/** A flame graph's tab title: `cpu.html` of `app.OrderTest` is "CPU: app.OrderTest". */
export function flameGraphTitle(page: string, title: string): string {
  const kind = page.split(/[\\/]/).pop()?.replace(/\.html$/, "") ?? "";
  const label = PROFILER_EVENTS.find((event) => event.name === kind)?.label ?? kind;
  return `${label}: ${title}`;
}

/**
 * The "Profile" twin of a "Run test(s)" lens: the same test, under
 * async-profiler; `undefined` for any other lens.
 */
export function profileTwin(command: LensCommand | undefined): LensCommand | undefined {
  if (command?.command !== RUN_TESTS_COMMAND) {
    return undefined;
  }
  const plural = command.title.trimEnd().endsWith("tests");
  return {
    title: plural ? "Profile tests" : "Profile test",
    command: PROFILE_TESTS_COMMAND,
    arguments: command.arguments,
  };
}
