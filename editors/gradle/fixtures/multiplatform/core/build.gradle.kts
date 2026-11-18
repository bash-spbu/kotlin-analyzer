plugins { kotlin("multiplatform") }
kotlin {
    jvm()
    js { nodejs() }
    sourceSets { commonTest.dependencies { implementation(kotlin("test")) } }
}
// An opt-in through languageSettings, which compile tasks' options don't show.
kotlin.sourceSets.all { languageSettings.optIn("kotlin.ExperimentalStdlibApi") }
