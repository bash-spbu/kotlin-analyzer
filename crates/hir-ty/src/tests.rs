use base_db::SourceDatabase;
use base_db::fixture::Fixture;
use expect_test::{Expect, expect};
use hir_def::item_tree::{ItemKind, Signature};
use hir_def::{ItemLocation, file_item_tree};
use intern::Name;

use crate::display::display;
use crate::lower::{TypeScope, lower_type};
use crate::members::{java_getters, members_named};
use crate::ty::Ty;

/// The few `kotlin` and `java` classes tests lean on, written as sources.
pub(crate) const MINI_STDLIB: &str = r#"
//- /lib/kotlin/Any.kt
package kotlin
open class Any {
    open fun toString(): String = TODO()
    open fun hashCode(): Int = TODO()
}
class String : CharSequence, Comparable<String> {
    val length: Int = 0
    fun plus(other: Any?): String = TODO()
}
interface CharSequence
interface Comparable<in T>
class Int : Number(), Comparable<Int> {
    fun plus(other: Int): Int = TODO()
}
class Long : Number()
abstract class Number
class Boolean
class Unit
class Nothing
class Array<T>(val size: Int) {
    fun get(index: Int): T = TODO()
}
class IntArray(val size: Int)
abstract class Enum<E : Enum<E>>(val name: String, val ordinal: Int)
fun <T> T.also(block: (T) -> Unit): T = TODO()
fun TODO(): Nothing = TODO()
//- /lib/kotlin/collections/Collections.kt
package kotlin.collections
interface Iterable<out T>
interface Collection<out E> : Iterable<E> {
    val size: Int
}
interface List<out E> : Collection<E> {
    fun get(index: Int): E
    fun first(): E
}
interface MutableList<E> : List<E> {
    fun add(element: E): Boolean
}
interface Map<K, out V> {
    fun get(key: K): V?
    interface Entry<out K, out V> {
        val key: K
        val value: V
    }
}
typealias ArrayList<E> = java.util.ArrayList<E>
fun <T> listOf(vararg elements: T): List<T> = TODO()
//- /lib/java/lang/Object.java
package java.lang;
public class Object {}
//- /lib/java/lang/String.java
package java.lang;
public final class String {}
//- /lib/java/util/List.java
package java.util;
public interface List<E> {}
//- /lib/java/util/ArrayList.java
package java.util;
public class ArrayList<E> implements List<E> {
    public E remove(int index) { return null; }
}
"#;

fn with_stdlib(fixture: &str) -> Fixture {
    Fixture::parse(&format!("{MINI_STDLIB}{fixture}"))
}

/// The declared type of each top-level property in `/app/Use.kt`.
fn check_declared_types(fixture: &str, expected: Expect) {
    let fixture = with_stdlib(fixture);
    let db = &fixture.database;
    let file = db.source_file(fixture.file("/app/Use.kt")).unwrap();
    let tree = file_item_tree(db, file);
    let mut lines = Vec::new();
    for index in tree.top_level.iter() {
        let item = tree.item(*index);
        let Signature::Property {
            type_ref: Some(type_ref),
            ..
        } = &item.signature
        else {
            continue;
        };
        let location = ItemLocation::in_file(file, *index);
        let scope = TypeScope::of_declaration(db, file, location);
        let ty = lower_type(db, scope, type_ref);
        lines.push(format!("{}: {}", item.name, display(db, &ty)));
    }
    expected.assert_eq(&lines.join("\n"));
}

/// The type of the top-level property `name` in `/app/Use.kt`.
fn declared_type(fixture: &Fixture, name: &str) -> Ty {
    let db = &fixture.database;
    let file = db.source_file(fixture.file("/app/Use.kt")).unwrap();
    let tree = file_item_tree(db, file);
    let index = *tree
        .top_level
        .iter()
        .find(|index| tree.item(**index).name.as_str() == name)
        .expect("the property is declared");
    let Signature::Property {
        type_ref: Some(type_ref),
        ..
    } = &tree.item(index).signature
    else {
        panic!("{name} has no written type");
    };
    let location = ItemLocation::in_file(file, index);
    lower_type(db, TypeScope::of_declaration(db, file, location), type_ref)
}

/// Each member `name` of the type of property `property`: where it's
/// declared and its return type as the receiver sees it.
fn describe_members(fixture: &Fixture, property: &str, name: &str) -> Vec<String> {
    let db = &fixture.database;
    let file = db.source_file(fixture.file("/app/Use.kt")).unwrap();
    let receiver = declared_type(fixture, property);
    let mut members = members_named(db, file, &receiver, Name::new(name));
    members.extend(java_getters(db, file, &receiver, Name::new(name)));
    members
        .iter()
        .map(|member| {
            let tree = hir_def::item_tree(db, member.location.container);
            let item = tree.item(member.location.item);
            let parent = item
                .parent
                .map(|parent| tree.item(parent).name.to_string())
                .unwrap_or_default();
            let scope = TypeScope::of_declaration(db, file, member.location);
            let written = match &item.signature {
                Signature::Function { return_type, .. } => return_type.as_ref(),
                Signature::Property { type_ref, .. } => type_ref.as_ref(),
                _ => None,
            };
            let returns = written
                .map(|type_ref| lower_type(db, scope, type_ref).substitute(&member.substitution))
                .map(|ty| display(db, &ty))
                .unwrap_or_else(|| "-".to_owned());
            let kind = match item.kind {
                ItemKind::Function => "fun",
                _ => "val",
            };
            format!("{kind} {parent}.{}: {returns}", item.name)
        })
        .collect()
}

#[test]
fn written_types_lower_with_aliases_type_parameters_and_nullability() {
    check_declared_types(
        r#"
//- /app/Use.kt
package app
import kotlin.collections.Map
class Holder<T> {
    val inner: T? = null
}
typealias Names = List<String>
typealias Handler<A> = (A) -> Unit
val text: String? = null
val names: Names = TODO()
val handler: Handler<Int> = TODO()
val receiverLambda: suspend String.(Int) -> Boolean = TODO()
val entry: Map.Entry<String, List<Int>>? = null
val star: List<*> = TODO()
val list: ArrayList<String> = TODO()
val missing: Missing = TODO()
"#,
        expect![[r#"
            text: String?
            names: List<String>
            handler: (Int) -> Unit
            receiverLambda: suspend String.(Int) -> Boolean
            entry: Map.Entry<String, List<Int>>?
            star: List<*>
            list: ArrayList<String>
            missing: {unknown}"#]],
    );
}

#[test]
fn java_declarations_have_platform_types_and_kotlin_names() {
    let fixture = with_stdlib(
        r#"
//- /app/Store.java
package app;
import java.util.List;
public class Store {
    public String name;
    public List<String> names() { return null; }
    public int count() { return 0; }
    public int[] counts() { return null; }
    public String getTitle() { return null; }
    public boolean isOpen() { return false; }
}
//- /app/Use.kt
package app
val store: Store = TODO()
"#,
    );
    assert_eq!(
        describe_members(&fixture, "store", "names"),
        vec!["fun Store.names: MutableList<String!>!"]
    );
    assert_eq!(
        describe_members(&fixture, "store", "count"),
        vec!["fun Store.count: Int"]
    );
    assert_eq!(
        describe_members(&fixture, "store", "counts"),
        vec!["fun Store.counts: IntArray!"]
    );
    assert_eq!(
        describe_members(&fixture, "store", "title"),
        vec!["fun Store.getTitle: String!"]
    );
    assert_eq!(
        describe_members(&fixture, "store", "open"),
        vec!["fun Store.isOpen: Boolean"]
    );
}

#[test]
fn inherited_members_are_seen_through_the_supertype_arguments() {
    let fixture = with_stdlib(
        r#"
//- /app/Use.kt
package app
open class Box<T> {
    open fun get(): T = TODO()
    fun peek(): T? = null
}
open class Labelled<L> : Box<List<L>>()
class Names : Labelled<String>() {
    override fun get(): List<String> = TODO()
}
val names: Names = TODO()
val entries: List<Map.Entry<String, Int>> = TODO()
val bounded: Bounded<String> = TODO()
class Bounded<B : CharSequence> {
    val value: B = TODO()
}
"#,
    );
    // The override hides `Box.get`; `peek` comes from two levels up with
    // `T` = `List<L>` = `List<String>`.
    assert_eq!(
        describe_members(&fixture, "names", "get"),
        vec!["fun Names.get: List<String>"]
    );
    assert_eq!(
        describe_members(&fixture, "names", "peek"),
        vec!["fun Box.peek: List<String>?"]
    );
    assert_eq!(
        describe_members(&fixture, "names", "hashCode"),
        vec!["fun Any.hashCode: Int"]
    );
    assert_eq!(
        describe_members(&fixture, "entries", "first"),
        vec!["fun List.first: Map.Entry<String, Int>"]
    );
    assert_eq!(
        describe_members(&fixture, "entries", "size"),
        vec!["val Collection.size: Int"]
    );
}
