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
        let tree = file_item_tree(db, location.file);
        let path = fixture.vfs.path(location.file.file_id(db));
        format!("{}:{}", path.display(), tree.item(location.item).name)
    };
    match resolution {
        Resolution::Declaration(found) => location(found),
        Resolution::TypeParameter { owner, index } => {
            let tree = file_item_tree(db, owner.file);
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
              fun findById(id: ID): Optional<T>
              fun map<R>(mapper: Function<in T, out R>, vararg tags: String): List<R>
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
