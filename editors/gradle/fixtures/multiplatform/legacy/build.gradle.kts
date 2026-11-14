plugins { kotlin("jvm") }
// A toolchain no machine here has, and nothing may download: resolving
// this module's classpaths fails, and the export must go on without it.
kotlin { jvmToolchain(9) }
