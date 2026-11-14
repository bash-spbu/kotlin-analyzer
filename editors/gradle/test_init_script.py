#!/usr/bin/env python3
"""Runs kotlin-analyzer.init.gradle on fixtures/multiplatform and checks the
model it exports. Needs Gradle 9 (GRADLE, else `gradle` on the PATH) and the
network for the Kotlin plugin, so it isn't part of `cargo xtask check`:

    python3 -I editors/gradle/test_init_script.py

The fixture turns the configuration cache on with problems=fail, as many
builds do: the export must run with it off.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "fixtures" / "multiplatform"
INIT_SCRIPT = HERE / "kotlin-analyzer.init.gradle"
EXPORT_ARGUMENTS = [
    "--quiet",
    "--no-configuration-cache",
    "-Dorg.gradle.isolated-projects=false",
    "-Dorg.gradle.unsafe.isolated-projects=false",
]


def exported_model() -> dict:
    gradle = os.environ.get("GRADLE") or shutil.which("gradle")
    if gradle is None:
        raise unittest.SkipTest("no Gradle: set GRADLE or put gradle on the PATH")
    with tempfile.TemporaryDirectory() as scratch:
        build = Path(scratch) / "multiplatform"
        shutil.copytree(FIXTURE, build)
        output = Path(scratch) / "project.json"
        subprocess.run(
            [gradle, *EXPORT_ARGUMENTS, "--init-script", str(INIT_SCRIPT), "kotlinAnalyzerModel",
             f"-PkotlinAnalyzerOutput={output}"],
            cwd=build, check=True, timeout=900,
        )
        return json.loads(output.read_text())


class MultiplatformExportTest(unittest.TestCase):
    model: dict

    @classmethod
    def setUpClass(cls) -> None:
        cls.model = exported_model()

    def source_set(self, module: str, name: str) -> dict:
        found = next(module_entry for module_entry in self.model["modules"] if module_entry["path"] == module)
        return next(entry for entry in found["sourceSets"] if entry["name"] == name)

    def dependencies(self, module: str, name: str) -> set[str]:
        return {
            f"{dependency['module']}/{dependency['sourceSet']}"
            for dependency in self.source_set(module, name)["moduleDependencies"]
        }

    def libraries(self, module: str, name: str) -> set[str]:
        return {library["coordinates"].split(":")[1] for library in self.source_set(module, name)["libraries"]}

    def test_source_sets_have_their_kind_and_directories(self) -> None:
        self.assertEqual(self.source_set(":core", "commonMain")["kind"], "main")
        self.assertEqual(self.source_set(":core", "jvmTest")["kind"], "test")
        directories = self.source_set(":core", "jvmMain")["sourceDirectories"]
        self.assertTrue(any(directory.endswith("core/src/jvmMain/kotlin") for directory in directories), directories)

    def test_platform_code_sees_what_it_is_built_on_and_common_code_sees_no_platform(self) -> None:
        self.assertEqual(self.dependencies(":core", "commonMain"), set())
        self.assertIn(":core/commonMain", self.dependencies(":core", "jvmMain"))
        self.assertIn(":core/webMain", self.dependencies(":core", "jsMain"))

    def test_a_test_sees_the_main_code_every_one_of_its_compilations_tests(self) -> None:
        self.assertEqual(self.dependencies(":core", "commonTest"), {":core/commonMain"})
        self.assertTrue({":core/jvmMain", ":core/commonMain", ":core/commonTest"} <= self.dependencies(":core", "jvmTest"))

    def test_project_dependencies_reach_the_other_modules_source_sets(self) -> None:
        self.assertIn(":core/commonMain", self.dependencies(":app", "commonMain"))
        self.assertIn(":core/jvmMain", self.dependencies(":app", "jvmMain"))
        # A test sees the other project's main code of its own platform.
        self.assertIn(":core/jvmMain", self.dependencies(":app", "jvmTest"))
        self.assertNotIn(":core/jvmMain", self.dependencies(":app", "commonTest"))
        self.assertTrue({":core/commonMain", ":core/jvmMain"} <= self.dependencies(":server", "main"))

    def test_only_code_built_for_the_jvm_is_on_the_jvm(self) -> None:
        self.assertEqual(self.source_set(":core", "jvmMain")["platform"], "jvm")
        self.assertEqual(self.source_set(":core", "commonMain")["platform"], "jvm")
        self.assertEqual(self.source_set(":core", "jsMain")["platform"], "other")
        self.assertEqual(self.source_set(":core", "webMain")["platform"], "other")

    def test_jvm_built_code_gets_jvm_libraries_and_the_rest_the_standard_library(self) -> None:
        self.assertIn("kotlin-stdlib", self.libraries(":core", "commonMain"))
        self.assertIn("kotlin-test", self.libraries(":core", "commonTest"))
        self.assertEqual(self.libraries(":core", "jsMain"), {"kotlin-stdlib"})
        self.assertIn("kotlin-stdlib", self.libraries(":server", "main"))

    def test_a_classpath_that_cannot_resolve_does_not_stop_the_export(self) -> None:
        # :legacy asks for a JDK 9 toolchain, which isn't installed: its
        # classpath is partial, which the model says.
        self.assertEqual(self.source_set(":legacy", "main")["libraries"], [])
        self.assertEqual(self.source_set(":legacy", "main")["classpath"], "partial")
        self.assertEqual(self.source_set(":server", "main")["classpath"], "complete")
        self.assertIn("kotlin-stdlib", self.libraries(":server", "main"))


if __name__ == "__main__":
    unittest.main()
