use base_db::SourceDatabase;
use base_db::fixture::Fixture;
use expect_test::{Expect, expect};
use intern::Path;

use crate::file_item_tree;
use crate::resolver::{Resolution, resolve_import_path, resolve_type_path};

fn check_item_tree(fixture: &str, expected: Expect) {
    let fixture = Fixture::parse(fixture);
    let db = &fixture.database;
    let file = db.all_files()[0];
    expected.assert_eq(&file_item_tree(db, file).pretty());
}

/// Resolves the type name `path` written at the fixture's `$0` and
/// describes the result as `path:Name` (or the other outcomes).
fn check_type(fixture: &str, path: &str, expected: Expect) {
    let fixture = Fixture::parse(fixture);
    let (file_id, offset) = fixture.cursor();
    let db = &fixture.database;
    let file = db.source_file(file_id).unwrap();
    let resolution = resolve_type_path(db, file, &Path::parse(path), offset);
    expected.assert_eq(&describe(&fixture, &resolution));
}

fn describe(fixture: &Fixture, resolution: &Resolution) -> String {
    let db = &fixture.database;
    let location = |location: &crate::ItemLocation| {
        let tree = crate::item_tree(db, location.container);
        let place = match location.file() {
            Some(file) => fixture.vfs.path(file.file_id(db)).display().to_string(),
            None => format!("library {}", tree.package),
        };
        format!("{place}:{}", tree.item(location.item).name)
    };
    match resolution {
        Resolution::Declaration(found) => location(found),
        Resolution::TypeParameter { owner, index } => {
            let tree = crate::item_tree(db, owner.container);
            let item = tree.item(owner.item);
            format!(
                "type parameter {} of {}",
                item.type_parameters[*index].name, item.name
            )
        }
        Resolution::Ambiguous(found) => {
            let found: Vec<String> = found.iter().map(location).collect();
            format!("ambiguous: {}", found.join(", "))
        }
        Resolution::Package(path) => format!("package {path}"),
        Resolution::Unresolved => "unresolved".to_owned(),
    }
}

#[test]
fn kotlin_item_tree() {
    check_item_tree(
        r#"
//- /src/Sample.kt
@file:JvmName("Sample")
package com.example.app

import java.util.UUID
import com.example.other.Helper as Aid
import com.example.util.*

typealias Mapper<T> = (T) -> String

enum class Color(val code: Int) { RED(1), GREEN(2); fun hex(): String = "" }
fun interface Check { fun run(): Boolean }
sealed interface Shape
@Deprecated("old") annotation class Marker
data class Point<T : Comparable<T>>(val x: T, private var y: Int = 0, label: String) : Shape, Base(1) {
    constructor(x: T) : this(x, 0, "")
    val size: Int get() = 1
    internal fun <R> map(vararg values: R, block: T.(R) -> Unit = {}): List<R>? = null
    companion object { const val ZERO = 0 }
    inner class Nested
    object Single
}
private val top: Map<String, List<out Number>>? = null
fun String.shout(): String = uppercase()
suspend fun load(id: UUID, retry: suspend () -> Unit): Result<com.example.Model>? = null
"#,
        expect![[r#"
            package com.example.app
            import java.util.UUID
            import com.example.other.Helper as Aid
            import com.example.util.*
            typealias Mapper<T> = (T) -> String
            enum class Color(code: Int)
              val code: Int
              entry RED
              entry GREEN
              fun hex(): String
            fun interface Check
              abstract fun run(): Boolean
            sealed interface Shape
            @Deprecated annotation class Marker
            data class Point<T : Comparable<T>>(x: T, y: Int = …, label: String) : Shape, Base
              val x: T
              private var y: Int
              constructor Point(x: T)
              val size: Int
              internal fun map<R>(vararg values: R, block: T.(R) -> Unit = …): List<R>?
              companion object Companion
                const val ZERO
              inner class Nested
              object Single
            private val top: Map<String, List<out Number>>?
            fun String.shout(): String
            suspend fun load(id: UUID, retry: suspend () -> Unit): Result<com.example.Model>?
        "#]],
    );
}

#[test]
fn java_item_tree() {
    check_item_tree(
        r#"
//- /src/Repo.java
package com.example.data;

import java.util.*;
import java.util.function.Function;

public interface Repo<T, ID> extends Base<T>, java.io.Serializable {
    int LIMIT = 10;
    Optional<T> findById(ID id);
    <R> List<R> map(Function<? super T, ? extends R> mapper, String... tags);
    enum Mode { FAST, SAFE; public static Mode parse(String text) { return FAST; } }
    class Impl implements Repo<String, Long> {
        private final Map<String, int[]> cache = null;
        Impl(int size) {}
        @Deprecated protected void evict() {}
    }
}
"#,
        expect![[r#"
            package com.example.data
            import java.util.*
            import java.util.function.Function
            interface Repo<T, ID> : Base<T>, java.io.Serializable
              static final val LIMIT: int
              abstract fun findById(id: ID): Optional<T>
              abstract fun map<R>(mapper: Function<in T, out R>, vararg tags: String): List<R>
              enum class Mode
                static entry FAST
                static entry SAFE
                static fun parse(text: String): Mode
              class Impl : Repo<String, Long>
                private final val cache: Map<String, int[]>
                package-private constructor Impl(size: int)
                protected @Deprecated fun evict(): void
        "#]],
    );
}

#[test]
fn a_namesake_in_a_module_the_file_doesnt_depend_on_is_invisible() {
    check_type(
        r#"
//- /common/a/Money.kt module=:common
package a
class Money
//- /other/a/Money.kt module=:other
package a
class Money(val cents: Long)
//- /api/b/Use.kt module=:api deps=:common
package b
import a.Money
val price: Money$0? = null
"#,
        "Money",
        expect!["/common/a/Money.kt:Money"],
    );
}

#[test]
fn the_same_class_in_two_visible_modules_is_ambiguous() {
    check_type(
        r#"
//- /one/a/Money.kt module=:one
package a
class Money
//- /two/a/Money.kt module=:two
package a
class Money
//- /api/b/Use.kt module=:api deps=:one,:two
package b
import a.Money
val price: Money$0? = null
"#,
        "Money",
        expect!["ambiguous: /one/a/Money.kt:Money, /two/a/Money.kt:Money"],
    );
}

#[test]
fn explicit_import_beats_same_package_which_beats_star_import() {
    let fixture = r#"
//- /app/x/Id.kt
package x
class Id
//- /app/y/Id.kt
package y
class Id
//- /app/z/Id.kt
package z
class Id
//- /app/x/Use.kt
package x
import y.Id
import z.*
val id: Id$0? = null
"#;
    check_type(fixture, "Id", expect!["/app/y/Id.kt:Id"]);
    check_type(
        &fixture.replace("import y.Id\n", ""),
        "Id",
        expect!["/app/x/Id.kt:Id"],
    );
}

#[test]
fn two_star_imports_with_the_name_are_ambiguous() {
    check_type(
        r#"
//- /app/y/Id.kt
package y
class Id
//- /app/z/Id.kt
package z
class Id
//- /app/x/Use.kt
package x
import y.*
import z.*
val id: Id$0? = null
"#,
        "Id",
        expect!["ambiguous: /app/y/Id.kt:Id, /app/z/Id.kt:Id"],
    );
}

#[test]
fn a_type_parameter_shadows_a_class_of_the_same_name() {
    check_type(
        r#"
//- /app/x/T.kt
package x
class T
class Box<T>(val item: T$0)
"#,
        "T",
        expect!["type parameter T of Box"],
    );
}

#[test]
fn nested_classes_of_enclosing_classes_and_their_supertypes() {
    let fixture = r#"
//- /app/x/Base.kt
package x
open class Base { class State }
class State
//- /app/x/Screen.kt
package x
class Screen : Base() {
    class Event
    fun handle(event: Event, state: State$0) {}
}
"#;
    check_type(fixture, "State", expect!["/app/x/Base.kt:State"]);
    check_type(fixture, "Event", expect!["/app/x/Screen.kt:Event"]);
}

#[test]
fn a_supertype_cycle_ends_and_still_finds_what_is_outside_it() {
    let fixture = r#"
//- /app/x/Cycle.kt
package x
interface Holder { class Found }
interface A : B, D, Holder
interface B : A, D, Holder
interface D : A, B, Holder
class Use : A {
    val found: Found? = null
    val missing: Missing$0? = null
}
"#;
    let started = std::time::Instant::now();
    check_type(fixture, "Missing", expect!["unresolved"]);
    check_type(fixture, "Found", expect!["/app/x/Cycle.kt:Found"]);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn a_nested_class_reached_through_a_diamond_is_one_class() {
    check_type(
        r#"
//- /app/x/Diamond.kt
package x
interface Root { class Key }
interface Left : Root
interface Right : Root
class Leaf : Left, Right {
    val key: Key$0? = null
}
"#,
        "Key",
        expect!["/app/x/Diamond.kt:Key"],
    );
}

#[test]
fn a_class_at_the_start_of_its_file_resolves_its_supertypes_outside_itself() {
    let fixture = r#"
//- /app/Screen.kt
class Screen : First, Second, Third {
    val state: State$0? = null
}
interface First
interface Second
interface Third { class State }
"#;
    let started = std::time::Instant::now();
    check_type(fixture, "State", expect!["/app/Screen.kt:State"]);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn qualified_names_resolve_by_package_or_by_outer_class() {
    let fixture = r#"
//- /app/a/b/Outer.kt
package a.b
class Outer { class Inner }
//- /app/c/Use.kt
package c
import a.b.Outer
val first: a.b.Outer.Inner? = null
val second: Outer.Inner$0? = null
"#;
    check_type(
        fixture,
        "a.b.Outer.Inner",
        expect!["/app/a/b/Outer.kt:Inner"],
    );
    check_type(fixture, "Outer.Inner", expect!["/app/a/b/Outer.kt:Inner"]);
    check_type(fixture, "a.b", expect!["package a.b"]);
}

#[test]
fn main_code_does_not_see_test_code_and_tests_see_main() {
    let fixture = r#"
//- /app/main/x/Clock.kt
package x
class Clock
//- /app/test/x/FakeClock.kt set=test
package x
class FakeClock
val clock: Clock$0? = null
//- /app/main/x/Use.kt
package x
val fake: FakeClock? = null
"#;
    check_type(fixture, "Clock", expect!["/app/main/x/Clock.kt:Clock"]);
    let main_fixture = Fixture::parse(fixture);
    let db = &main_fixture.database;
    let use_file = db
        .source_file(main_fixture.file("/app/main/x/Use.kt"))
        .unwrap();
    let resolution = resolve_type_path(db, use_file, &Path::parse("FakeClock"), 30);
    assert_eq!(resolution, Resolution::Unresolved);
}

#[test]
fn private_and_internal_declarations_stay_where_they_belong() {
    let fixture = r#"
//- /lib/x/Hidden.kt module=:lib
package x
private class Secret
internal class Local
//- /lib/x/Peer.kt module=:lib
package x
val local: Local$0? = null
//- /app/x/Use.kt module=:app deps=:lib
package x
val secret: Secret? = null
val local: Local? = null
"#;
    check_type(fixture, "Local", expect!["/lib/x/Hidden.kt:Local"]);
    let parsed = Fixture::parse(fixture);
    let db = &parsed.database;
    let peer = db.source_file(parsed.file("/lib/x/Peer.kt")).unwrap();
    let user = db.source_file(parsed.file("/app/x/Use.kt")).unwrap();
    assert_eq!(
        resolve_type_path(db, peer, &Path::parse("Secret"), 20),
        Resolution::Unresolved
    );
    assert_eq!(
        resolve_type_path(db, user, &Path::parse("Local"), 40),
        Resolution::Unresolved
    );
}

#[test]
fn kotlin_sees_java_classes_and_imports_name_functions_too() {
    let fixture = r#"
//- /app/x/Gateway.java
package x;
public class Gateway { public static class Request {} }
//- /app/y/Util.kt
package y
fun helper() {}
//- /app/z/Use.kt
package z
import x.Gateway
import x.Gateway.Request
import y.helper
val request: Request$0? = null
"#;
    check_type(fixture, "Request", expect!["/app/x/Gateway.java:Request"]);
    check_type(fixture, "Gateway", expect!["/app/x/Gateway.java:Gateway"]);
    let parsed = Fixture::parse(fixture);
    let db = &parsed.database;
    let user = db.source_file(parsed.file("/app/z/Use.kt")).unwrap();
    let helper = resolve_import_path(db, user, &Path::parse("y.helper"));
    expect!["/app/y/Util.kt:helper"].assert_eq(&describe(&parsed, &helper));
}

#[test]
fn kotlin_default_imports_outrank_the_platform_ones() {
    let fixture = r#"
//- /lib/kotlin/String.kt
package kotlin
class String
class Exception
//- /lib/java/lang/String.java
package java.lang;
public final class String {}
//- /lib/java/lang/Thread.java
package java.lang;
public class Thread {}
//- /app/x/Use.kt
package x
val name: String$0? = null
val thread: Thread? = null
"#;
    check_type(fixture, "String", expect!["/lib/kotlin/String.kt:String"]);
    check_type(
        fixture,
        "Thread",
        expect!["/lib/java/lang/Thread.java:Thread"],
    );
}

/// The newest cached compiled JAR of `group:artifact` in the Gradle cache.
fn cached_jar(group: &str, artifact: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let root = std::path::Path::new(&home)
        .join(".gradle/caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);
    let mut jars: Vec<std::path::PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .flat_map(|version| {
            std::fs::read_dir(version.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .flat_map(|hash| {
            std::fs::read_dir(hash.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            name.ends_with(".jar")
                && !name.ends_with("-sources.jar")
                && !name.ends_with("-javadoc.jar")
        })
        .collect();
    jars.sort();
    jars.pop()
}

#[test]
fn library_types_resolve_through_the_classpath_and_a_workspace_class_wins() {
    let Some(jar) = cached_jar("org.jetbrains.kotlin", "kotlin-stdlib") else {
        eprintln!("skipped: no kotlin-stdlib in the Gradle cache");
        return;
    };
    let mut parsed = Fixture::parse(
        r#"
//- /app/x/Pair.kt
package x
class Pair
val text: String? = null
val list: MutableList<Int>? = null
val array: ArrayList<Int>? = null
val pair: Pair? = null
val entry: Map.Entry<String, Int>? = null
"#,
    );
    let source = base_db::LibrarySource {
        archive: jar,
        kind: base_db::ArchiveKind::Jar,
        sources: None,
    };
    let indexed = crate::library::index_library(&source).expect("kotlin-stdlib indexes");
    parsed
        .database
        .set_libraries(vec![(source, indexed)], vec![vec![0]], None);
    let db = &parsed.database;
    let file = db.all_files()[0];
    let resolve =
        |path: &str| describe(&parsed, &resolve_type_path(db, file, &Path::parse(path), 0));
    assert_eq!(resolve("String"), "library kotlin:String");
    assert_eq!(
        resolve("MutableList"),
        "library kotlin.collections:MutableList"
    );
    assert_eq!(resolve("ArrayList"), "library kotlin.collections:ArrayList");
    assert_eq!(resolve("Map.Entry"), "library kotlin.collections:Entry");
    // `kotlin.Pair` is a default import, but the file's own package wins.
    assert_eq!(resolve("Pair"), "/app/x/Pair.kt:Pair");
}

#[test]
fn library_sources_are_found_in_archives_laid_out_by_source_set() {
    let Some(jar) = cached_jar("org.jetbrains.kotlin", "kotlin-stdlib") else {
        eprintln!("skipped: no kotlin-stdlib in the Gradle cache");
        return;
    };
    let sources = jar.with_file_name(
        jar.file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".jar", "-sources.jar"),
    );
    let sources = std::fs::read_dir(jar.parent().unwrap().parent().unwrap())
        .into_iter()
        .flatten()
        .flatten()
        .map(|hash| hash.path().join(sources.file_name().unwrap()))
        .find(|candidate| candidate.is_file());
    let Some(sources) = sources else {
        eprintln!("skipped: no kotlin-stdlib sources JAR in the Gradle cache");
        return;
    };
    let mut parsed = Fixture::parse("//- /app/x/Use.kt\npackage x\n");
    let source = base_db::LibrarySource {
        archive: jar,
        kind: base_db::ArchiveKind::Jar,
        sources: Some(sources),
    };
    let indexed = crate::library::index_library(&source).expect("kotlin-stdlib indexes");
    parsed
        .database
        .set_libraries(vec![(source, indexed)], vec![vec![0]], None);
    let db = &parsed.database;
    let file = db.all_files()[0];
    let declaration = |package: &str, name: &str| {
        crate::def_map::visible_top_level(db, file, &Path::parse(package), intern::Name::new(name))
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{package}.{name} is declared"))
    };
    // The generated `_Arrays.kt`, `_Collections.kt`, … live under
    // `commonMain/generated/`, not `kotlin/collections/`.
    let map_not_null = crate::library::library_declaration_source(
        db,
        declaration("kotlin.collections", "mapNotNull"),
    )
    .expect("mapNotNull's source is found");
    assert!(
        map_not_null.entry.contains("generated/_"),
        "{}",
        map_not_null.entry
    );
    let text = &map_not_null.text
        [map_not_null.name_range.start as usize..map_not_null.name_range.end as usize];
    assert_eq!(text, "mapNotNull");
}

#[test]
fn edits_that_leave_declarations_alone_recompute_nothing_built_on_them() {
    let mut fixture = Fixture::parse(
        r#"
//- /app/x/A.kt
package x
fun first() = 1
class Later { fun inside() {} }
//- /app/x/B.kt
package x
class Other
"#,
    );
    let later_id = |fixture: &Fixture| {
        let db = &fixture.database;
        let file = db.source_file(fixture.file("/app/x/A.kt")).unwrap();
        let tree = file_item_tree(db, file);
        tree.indexed_items()
            .find(|(_, item)| item.name.as_str() == "Later")
            .map(|(id, _)| id)
            .unwrap()
    };
    let def_map = |fixture: &Fixture| {
        let db = &fixture.database;
        let file = db.source_file(fixture.file("/app/x/A.kt")).unwrap();
        let set = file.source_set(db).unwrap();
        let source_set = db.workspace().source_sets(db)[set.0 as usize];
        crate::source_set_def_map(db, source_set)
    };
    let before = def_map(&fixture);
    let later_before = later_id(&fixture);

    // A reload with the same model keeps every source set's input.
    let model = fixture
        .database
        .workspace()
        .model(&fixture.database)
        .clone();
    fixture.database.set_project_model(model);
    assert!(std::sync::Arc::ptr_eq(&before, &def_map(&fixture)));

    // A body edit that also moves every declaration below it.
    fixture.edit(
        "/app/x/A.kt",
        "package x\n\n\nfun first() = 2 + 40\nclass Later { fun inside() { val moved = 1 } }\n",
    );
    assert!(
        std::sync::Arc::ptr_eq(&before, &def_map(&fixture)),
        "the definition map is reused, not recomputed"
    );
    assert_eq!(later_id(&fixture), later_before);

    // A new declaration above: the map changes, `Later` keeps its id.
    fixture.edit(
        "/app/x/A.kt",
        "package x\nfun added() {}\nfun first() = 1\nclass Later { fun inside() {} }\n",
    );
    assert!(!std::sync::Arc::ptr_eq(&before, &def_map(&fixture)));
    assert_eq!(later_id(&fixture), later_before);
}

#[test]
fn overloads_and_same_named_members_get_distinct_ids() {
    let fixture = Fixture::parse(
        r#"
//- /app/x/A.kt
package x
fun pay(amount: Int) {}
fun pay(amount: Long) {}
class Pay { fun pay() {} }
"#,
    );
    let db = &fixture.database;
    let file = db.source_file(fixture.file("/app/x/A.kt")).unwrap();
    let tree = file_item_tree(db, file);
    let mut ids: Vec<crate::ItemId> = tree.indexed_items().map(|(id, _)| id).collect();
    let count = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), count);
}

#[test]
fn which_files_can_name_a_declaration() {
    let fixture = Fixture::parse(
        r#"
//- /lib/x/Hidden.kt module=:lib
package x
private class Secret
internal class Local
class Open
//- /lib/x/Peer.kt module=:lib
package x
//- /app/x/Use.kt module=:app deps=:lib
package x
//- /other/x/Far.kt module=:other
package x
"#,
    );
    let db = &fixture.database;
    let file = |path: &str| db.source_file(fixture.file(path)).unwrap();
    let hidden = file("/lib/x/Hidden.kt");
    let declared = |name: &str| {
        let tree = file_item_tree(db, hidden);
        let (id, _) = tree
            .indexed_items()
            .find(|(_, item)| item.name.as_str() == name)
            .unwrap();
        crate::ItemLocation::in_file(hidden, id)
    };
    let sees = |path: &str, name: &str| crate::def_map::sees(db, file(path), declared(name));
    assert!(sees("/lib/x/Hidden.kt", "Secret"));
    assert!(!sees("/lib/x/Peer.kt", "Secret"));
    assert!(sees("/lib/x/Peer.kt", "Local"));
    assert!(!sees("/app/x/Use.kt", "Local"));
    assert!(sees("/app/x/Use.kt", "Open"));
    assert!(
        !sees("/other/x/Far.kt", "Open"),
        "a module that doesn't depend on it"
    );
}

#[test]
fn a_release_missing_from_ct_sym_falls_back_to_the_newest_one_before_it() {
    use crate::library::newest_release_at_most;
    // JDK 21's ct.sym: releases 8 to 20 (`K`) in full, 21 (`L`) in two
    // entries — the current API is in the runtime image.
    let mut jdk_21 = vec!["META-INF", "META-INF"];
    // Each class covers every release, in one entry or several.
    jdk_21.extend(["8", "9A", "BCDEFGHIJK"].repeat(10));
    jdk_21.extend(["89ABCDEFGHIJK"].repeat(10));
    jdk_21.extend(["KL", "L"]);
    let newest = |requested| newest_release_at_most(jdk_21.iter().copied(), requested);
    assert_eq!(newest('L'), Some('K'));
    assert_eq!(newest('B'), Some('B'));
    assert_eq!(newest('9'), Some('9'));
    // META-INF's letters aren't releases: nothing for Java 7.
    assert_eq!(newest('7'), None);
    // A ct.sym that has its own release in full keeps it.
    let jdk_25 = ["BCDEFGHIJKLMNOP", "P"];
    assert_eq!(newest_release_at_most(jdk_25.into_iter(), 'P'), Some('P'));
}

#[test]
fn only_jvm_source_sets_see_the_jdk() {
    let mut parsed = Fixture::parse(
        r#"
//- /shared/jvm/x/Jvm.kt module=:shared set=jvmMain
package x
//- /shared/js/x/Js.kt module=:shared set=jsMain
package x
"#,
    );
    let jdk = base_db::LibrarySource {
        archive: std::path::PathBuf::from("/jdk/lib/ct.sym"),
        kind: base_db::ArchiveKind::JdkSymbols { release: 'L' },
        sources: None,
    };
    let indexed = base_db::IndexedLibrary {
        packages: vec![base_db::IndexedPackage {
            package: Path::parse("java.lang"),
            names: vec![intern::Name::new("Object")],
            entries: Vec::new(),
            module: None,
        }],
        stamp: base_db::ArchiveStamp::default(),
    };
    parsed
        .database
        .set_libraries(vec![(jdk, indexed)], vec![Vec::new(), Vec::new()], Some(0));
    let db = &parsed.database;
    let jvm = db.source_file(parsed.file("/shared/jvm/x/Jvm.kt")).unwrap();
    let js = db.source_file(parsed.file("/shared/js/x/Js.kt")).unwrap();
    let java_lang = Path::parse("java.lang");
    assert!(crate::def_map::package_exists(db, jvm, &java_lang));
    assert!(!crate::def_map::package_exists(db, js, &java_lang));
}
