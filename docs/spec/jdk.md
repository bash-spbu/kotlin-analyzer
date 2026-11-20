# The JDK

Which JDK the server uses, and getting one when the machine has none.

## Which JDK

Two things need one: the programs the server starts (Gradle exporting the
model, the debugger's Gradle and `java`, a formatter's jar) need a `java` to
run, and the analysis reads the JDK's API (`lib/ct.sym`) and sources
(`lib/src.zip`, see libraries.md).

| where | runs programs | its API read |
|---|---|---|
| the model's `jdk.home` (the JDK Gradle ran on) | — | first |
| `JAVA_HOME`, when it holds `bin/java` | yes, first | next |
| `java` on `PATH` | yes, next | no (no home to read) |
| the newest JDK in `~/.jdks` | yes, last: `JAVA_HOME` set to it | last |

macOS's `/usr/bin/java` is a stub that runs an installed JDK (from
`/Library/Java/JavaVirtualMachines` or `~/Library/Java/JavaVirtualMachines`):
it counts as `java` on `PATH` only when there is one. A JDK in `~/.jdks` is a
directory whose `bin/java` — or, as a macOS bundle, `Contents/Home/bin/java` —
exists; its version is its `release` file's `JAVA_VERSION`. IntelliJ IDEA
downloads JDKs to the same directory, so its JDKs are found too.

## When there is none

No JDK is when none of the places above runs programs. The server then asks,
once per session, as the workspace starts loading (`window/showMessageRequest`):

1. *No JDK found … Download one into `~/.jdks`?* — a vendor: **Eclipse
   Temurin**, **Azul Zulu**, **GraalVM Community**, **Microsoft**, or **Not
   now**. These are the vendors whose packages come with a SHA-256 checksum.
2. *Which version of Temurin?* — the project's release first when the model
   names one (`jdk.release`, else the one every JVM source set compiles
   against), then the LTS releases **21**, **25**, **17**, or **Cancel**.

Dismissing a question is *Not now*. The same two questions come from the
code action **Download a JDK…** (offered anywhere in a Kotlin file while
there's no JDK) and from the command `kotlin-analyzer.downloadJdk`; the
command with arguments `[vendor, version]` (`"temurin"`, `21`) asks nothing.
`offerJdkDownload: false` in `initializationOptions` stops the question at
startup (the code action and the command stay).

## Downloading

The package is found with the Foojay Disco API (`api.foojay.io`, the index
Gradle's toolchains use): the latest GA JDK of that vendor and version for
this OS and architecture (glibc on Linux), a `.tar.gz`, without JavaFX. It is
downloaded from the vendor's own address with `curl` (HTTPS only), its SHA-256
checked against the checksum the API gives or links to — a package without
one isn't installed — and unpacked with `tar` beside the others, then moved
into place: `~/.jdks/<vendor>-<version>` (`temurin-21.0.13.1`), as IntelliJ
names it. A JDK already there is used as it is. Nothing is left behind when
any step fails: the download, the checksum, unpacking, or an archive without
a JDK in it.

The status bar shows the download (`kotlin-analyzer: Downloading a JDK`,
with its percentage), then a message says where it was installed, and the
server uses it: Gradle exports the model again (`JAVA_HOME` set to it) and
the workspace reloads, reading its API. A failure is shown with what failed.

The requests send nothing about the user or the project: the vendor, the
version, the OS and the architecture.
