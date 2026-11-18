plugins { kotlin("jvm") }
dependencies { implementation(project(":core")) }
// What the export reads as compiler options: warnings as errors, an
// opt-in, and one warning suppressed by name.
kotlin {
    compilerOptions {
        allWarningsAsErrors.set(true)
        optIn.add("kotlin.ExperimentalStdlibApi")
        freeCompilerArgs.add("-Xsuppress-warning=UNUSED_VARIABLE")
    }
}
