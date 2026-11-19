// What the debugger integration decides, apart from VS Code: the debug
// configuration of a test a lens names, and which lenses get a "Debug"
// twin. The adapter is the server's `kotlin-analyzer dap`
// (docs/spec/agents.md, Debugging); `debugger.ts` registers it.

/** The `type` of kotlin-analyzer's debug configurations. */
export const DEBUG_TYPE = "kotlin-analyzer";

/** The command of the "Debug test" lenses: the test, under the debugger. */
export const DEBUG_TESTS_COMMAND = "kotlin-analyzer.debugTests";

/** The command of the server's "Run test" lenses (docs/spec/lsp.md). */
export const RUN_TESTS_COMMAND = "kotlin-analyzer.runTests";

/** A test lens's argument: the test file, and Gradle's `--tests` filter. */
export interface TestRun {
  uri: string;
  filter: string;
}

/** A debug configuration of kotlin-analyzer's, as `launch.json` holds one. */
export interface DebugConfiguration {
  type: string;
  request: "launch" | "attach";
  name: string;
  [attribute: string]: unknown;
}

/** Debugs the tests `filter` names in the project holding `file`. */
export function testConfiguration(file: string, filter: string): DebugConfiguration {
  return {
    type: DEBUG_TYPE,
    request: "launch",
    name: `Debug ${filter}`,
    test: { file, filter },
  };
}

/** A lens as the server sends it: its title and command. */
export interface LensCommand {
  title: string;
  command: string;
  arguments?: unknown[];
}

/**
 * The "Debug" twin of a "Run test(s)" lens: the same test, under the
 * debugger; `undefined` for any other lens.
 */
export function debugTwin(command: LensCommand | undefined): LensCommand | undefined {
  if (command?.command !== RUN_TESTS_COMMAND) {
    return undefined;
  }
  const plural = command.title.trimEnd().endsWith("tests");
  return {
    title: plural ? "Debug tests" : "Debug test",
    command: DEBUG_TESTS_COMMAND,
    arguments: command.arguments,
  };
}

/**
 * Whether a configuration is VS Code's empty one (F5 with no `launch.json`):
 * there's nothing to debug yet, so VS Code is to open `launch.json`.
 */
export function isEmptyConfiguration(configuration: Record<string, unknown>): boolean {
  const given = ["type", "request", "name"].filter((attribute) => configuration[attribute] !== undefined);
  return given.length === 0;
}
