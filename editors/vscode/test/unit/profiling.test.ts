import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import * as path from "node:path";
import { describe, it } from "node:test";

import { RUN_TESTS_COMMAND } from "../../src/debugging";
import {
  PROFILER_EVENTS,
  PROFILE_TASK_TYPE,
  PROFILE_TESTS_COMMAND,
  eventsLastFirst,
  flameGraphTitle,
  parseProfileResult,
  profileEnvironment,
  profileTwin,
  refusedOptions,
} from "../../src/profiling";

const extensionRoot = path.resolve(__dirname, "../../..");

describe("profiling", () => {
  it("gives each Run lens a Profile twin with the same test, and no other lens one", () => {
    const run = { uri: "file:///OrderTest.kt", filter: "shop.OrderTest" };
    assert.deepEqual(profileTwin({ title: "▶ Run tests", command: RUN_TESTS_COMMAND, arguments: [run] }), {
      title: "Profile tests",
      command: PROFILE_TESTS_COMMAND,
      arguments: [run],
    });
    assert.equal(profileTwin({ title: "▶ Run test", command: RUN_TESTS_COMMAND, arguments: [run] })?.title, "Profile test");
    assert.equal(profileTwin({ title: "↑ Base.name", command: "kotlin-analyzer.showReferences" }), undefined);
    assert.equal(profileTwin(undefined), undefined);
  });

  it("offers the last event chosen first, then the others in order", () => {
    assert.deepEqual(
      eventsLastFirst("lock").map((event) => event.name),
      ["lock", "cpu", "alloc", "wall", "all"],
    );
    assert.deepEqual(
      eventsLastFirst("cpu").map((event) => event.name),
      PROFILER_EVENTS.map((event) => event.name),
    );
  });

  it("refuses an option naming the recording's file, and nothing else", () => {
    assert.match(refusedOptions("interval=1ms, file=/tmp/x.jfr") ?? "", /file=/);
    assert.equal(refusedOptions("interval=1ms,threads"), undefined);
    assert.equal(refusedOptions(""), undefined);
    assert.equal(refusedOptions("filter=app"), undefined, "a name starting with file isn't file");
  });

  it("tells gradle_task.py the choices, and the task only when there is one", () => {
    assert.deepEqual(profileEnvironment({ event: "alloc", options: "alloc=512k" }, "/tmp/r.json"), {
      KOTLIN_ANALYZER_PROFILER_EVENT: "alloc",
      KOTLIN_ANALYZER_PROFILER_OPTIONS: "alloc=512k",
      KOTLIN_ANALYZER_PROFILE_RESULT: "/tmp/r.json",
    });
    assert.equal(
      profileEnvironment({ event: "cpu", options: "" }, "/tmp/r.json", "bootRun").KOTLIN_ANALYZER_PROFILED_TASK,
      "bootRun",
    );
  });

  it("reads the result gradle_task.py writes, and nothing else as one", () => {
    const written = JSON.stringify({
      title: "shop.OrderTest",
      event: "all",
      pages: ["/b/build/kotlin-analyzer/profiles/1-all/cpu.html", "/b/build/kotlin-analyzer/profiles/1-all/alloc.html"],
      recordings: "/b/build/kotlin-analyzer/profiles/1-all",
      gradleExitCode: 1,
    });
    const result = parseProfileResult(written);
    assert.equal(result?.pages.length, 2);
    assert.equal(result?.gradleExitCode, 1);
    assert.equal(parseProfileResult(""), undefined);
    assert.equal(parseProfileResult(JSON.stringify({ pages: [1], recordings: "/r" })), undefined);
    assert.equal(flameGraphTitle(result!.pages[1], result!.title), "Allocations: shop.OrderTest");
    assert.equal(flameGraphTitle("/r/cpu.html", "bootRun"), "CPU: bootRun");
  });

  it("declares the task type with the events gradle_task.py knows", () => {
    const manifest = JSON.parse(readFileSync(path.join(extensionRoot, "package.json"), "utf8"));
    const definition = manifest.contributes.taskDefinitions.find(
      (candidate: { type: string }) => candidate.type === PROFILE_TASK_TYPE,
    );
    assert.deepEqual(
      definition.properties.event.enum,
      PROFILER_EVENTS.map((event) => event.name),
    );
    const script = readFileSync(path.join(extensionRoot, "../gradle/gradle_task.py"), "utf8");
    for (const event of PROFILER_EVENTS) {
      assert.match(script, new RegExp(`"${event.name}": \\("event=`), event.name);
    }
  });
});
