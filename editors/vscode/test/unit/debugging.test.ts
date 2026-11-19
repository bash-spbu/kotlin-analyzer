import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import * as path from "node:path";
import { describe, it } from "node:test";

import {
  DEBUG_TESTS_COMMAND,
  DEBUG_TYPE,
  RUN_TESTS_COMMAND,
  debugTwin,
  isEmptyConfiguration,
  testConfiguration,
} from "../../src/debugging";

const extensionRoot = path.resolve(__dirname, "../../..");

function json(file: string): any {
  return JSON.parse(readFileSync(path.join(extensionRoot, file), "utf8"));
}

describe("debugging", () => {
  it("debugs the test a lens names by its Gradle filter", () => {
    assert.deepEqual(testConfiguration("/build/lib/src/test/kotlin/OrderTest.kt", "shop.OrderTest.creates an order"), {
      type: DEBUG_TYPE,
      request: "launch",
      name: "Debug shop.OrderTest.creates an order",
      test: { file: "/build/lib/src/test/kotlin/OrderTest.kt", filter: "shop.OrderTest.creates an order" },
    });
  });

  it("gives each Run lens a Debug twin with the same test, and no other lens one", () => {
    const run = { uri: "file:///OrderTest.kt", filter: "shop.OrderTest" };
    assert.deepEqual(debugTwin({ title: "▶ Run tests", command: RUN_TESTS_COMMAND, arguments: [run] }), {
      title: "Debug tests",
      command: DEBUG_TESTS_COMMAND,
      arguments: [run],
    });
    assert.equal(debugTwin({ title: "▶ Run test", command: RUN_TESTS_COMMAND, arguments: [run] })?.title, "Debug test");
    assert.equal(debugTwin({ title: "3 references", command: "kotlin-analyzer.showReferences" }), undefined);
    assert.equal(debugTwin(undefined), undefined, "an unresolved lens");
  });

  it("opens launch.json for F5 without one", () => {
    assert.equal(isEmptyConfiguration({}), true);
    assert.equal(isEmptyConfiguration({ type: DEBUG_TYPE, request: "attach", name: "a", port: 5005 }), false);
  });

  it("contributes the debug type, the command, and breakpoints in Kotlin", () => {
    const contributes = json("package.json").contributes;
    assert.deepEqual(
      contributes.debuggers.map((debuggerContribution: { type: string }) => debuggerContribution.type),
      [DEBUG_TYPE],
    );
    assert.ok(contributes.breakpoints.some((breakpoint: { language: string }) => breakpoint.language === "kotlin"));
    assert.ok(json("package.json").activationEvents.includes(`onDebugResolve:${DEBUG_TYPE}`));
  });

  it("takes every attribute Zed's configurations do: one adapter, one configuration", () => {
    const attributes = json("package.json").contributes.debuggers[0].configurationAttributes;
    const zed = json("../zed/debug_adapter_schemas/Kotlin.json");
    const zedKeys = (request: string): string[] =>
      zed.oneOf
        .filter((variant: any) => variant.properties.request.enum.includes(request))
        .flatMap((variant: any) => Object.keys(variant.properties))
        .filter((key: string) => key !== "request")
        .sort();
    assert.deepEqual(Object.keys(attributes.launch.properties).sort(), zedKeys("launch"));
    assert.deepEqual(Object.keys(attributes.attach.properties).sort(), zedKeys("attach"));
    const zedTest = zed.oneOf.find((variant: any) => variant.title === "Test").properties.test.properties;
    assert.deepEqual(Object.keys(attributes.launch.properties.test.properties).sort(), Object.keys(zedTest).sort());
  });
});
