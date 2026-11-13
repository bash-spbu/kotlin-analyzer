//! What a workspace consists of: Gradle modules, their source sets (main,
//! test, test fixtures, generated sources), the dependencies between modules,
//! each module's classpath (compiled JARs and their sources JARs) and the JDK.
//! Read from the file the Gradle init script writes, or guessed from directory
//! conventions when there is none.
