plugins { kotlin("multiplatform") }
kotlin {
    jvm()
    js { nodejs() }
    sourceSets { commonMain.dependencies { implementation(project(":core")) } }
}
