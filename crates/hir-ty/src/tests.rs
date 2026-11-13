use base_db::SourceDatabase;
use base_db::fixture::Fixture;
use expect_test::{Expect, expect};
use hir_def::item_tree::{ItemKind, Signature};
use hir_def::{ItemLocation, file_item_tree};
use intern::Name;

use crate::display::display;
use crate::infer::{Reference, infer_file};
use crate::lower::{TypeScope, lower_type};
use crate::members::Types;
use crate::ty::Ty;

fn with_stdlib(fixture: &str) -> Fixture {
    Fixture::with_mini_stdlib(fixture)
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
    let types = Types::new(db, file);
    let mut members = types.members_named(&receiver, Name::new(name));
    members.extend(types.java_getters(&receiver, Name::new(name)));
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

/// Every name inference resolved in `/app/Use.kt`, in order: the name,
/// its line, what it means, and its type.
fn check_inference(fixture: &str, expected: Expect) {
    let fixture = with_stdlib(fixture);
    let db = &fixture.database;
    let file = db.source_file(fixture.file("/app/Use.kt")).unwrap();
    let text = file.text(db).clone();
    let inference = infer_file(db, file);
    let line_of = |offset: u32| text[..offset as usize].matches('\n').count() + 1;
    let word_at = |offset: u32| {
        text[offset as usize..]
            .chars()
            .take_while(|char| char.is_alphanumeric() || *char == '_')
            .collect::<String>()
    };
    let describe = |location: &ItemLocation| {
        let tree = hir_def::item_tree(db, location.container);
        let item = tree.item(location.item);
        let mut names = vec![item.name.to_string()];
        let mut parent = item.parent;
        while let Some(index) = parent {
            names.push(tree.item(index).name.to_string());
            parent = tree.item(index).parent;
        }
        names.reverse();
        format!("{}.{}", tree.package, names.join("."))
    };
    let mut lines: Vec<(u32, String)> = inference
        .names()
        .map(|(start, resolved)| {
            let target = match &resolved.reference {
                Reference::Local(local) => {
                    let local = inference.local(*local);
                    format!("local {}@{}", local.name, line_of(local.name_range.start))
                }
                Reference::Declaration(location) => describe(location),
                Reference::Candidates(candidates) => {
                    let candidates: Vec<String> = candidates.iter().map(describe).collect();
                    format!("one of [{}]", candidates.join(", "))
                }
            };
            (
                start,
                format!(
                    "{}@{} -> {}: {}",
                    word_at(start),
                    line_of(start),
                    target,
                    display(db, &resolved.ty)
                ),
            )
        })
        .collect();
    for (start, name) in inference.unresolved() {
        lines.push((*start, format!("{name}@{} -> ?", line_of(*start))));
    }
    lines.sort();
    let lines: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
    expected.assert_eq(&lines.join("\n"));
}

#[test]
fn locals_members_and_top_level_names_resolve_with_their_types() {
    check_inference(
        r#"
//- /app/Use.kt
package app
val prefix = "id-"
class Account(val id: Int, private val owner: String?) {
    val label = prefix + id
    fun describe(): String {
        val name = owner
        if (name != null) {
            return name
        }
        return label
    }
    fun total(extra: Int) = id + extra
}
fun use(account: Account) {
    val sum = account.total(1)
    account.describe().length
}
"#,
        expect![[r#"
            prefix@4 -> app.prefix: String
            id@4 -> local id@3: Int
            owner@6 -> local owner@3: String?
            name@7 -> local name@6: String?
            name@8 -> local name@6: String
            label@10 -> app.Account.label: String
            id@12 -> local id@3: Int
            extra@12 -> local extra@12: Int
            account@15 -> local account@14: Account
            total@15 -> app.Account.total: Int
            account@16 -> local account@14: Account
            describe@16 -> app.Account.describe: String
            length@16 -> kotlin.String.length: Int"#]],
    );
}

#[test]
fn overloads_are_chosen_by_argument_types_and_levels() {
    check_inference(
        r#"
//- /app/Helpers.kt
package app
fun format(value: Int): String = TODO()
fun format(value: String): Int = TODO()
fun String.shout(): String = TODO()
fun Int.shout(): Int = TODO()
fun wide(value: Long): Long = TODO()
//- /app/Use.kt
package app
class Printer {
    fun format(value: Boolean): Boolean = TODO()
    fun run() {
        val a = format("x")
        val b = format(1)
        val c = format(true)
        val d = "x".shout()
        val e = 1.shout()
        val f = wide(1)
        println(1)
        println("x")
    }
}
"#,
        expect![[r#"
            TODO@3 -> kotlin.TODO: Nothing
            format@5 -> app.format: Int
            format@6 -> app.format: String
            format@7 -> app.Printer.format: Boolean
            shout@8 -> app.shout: String
            shout@9 -> app.shout: Int
            wide@10 -> app.wide: Long
            println@11 -> kotlin.println: Unit
            println@12 -> kotlin.println: Unit"#]],
    );
}

#[test]
fn generic_calls_infer_type_arguments_and_lambda_parameters() {
    check_inference(
        r#"
//- /app/Use.kt
package app
class Box<T>(val value: T)
fun use() {
    val names = listOf("a", "b")
    val first = names.first()
    val lengths = names.map { it.length }
    names.forEach { name -> name.length }
    val boxed = Box(names)
    boxed.value.first()
    val pair = 1 to "one"
    val (number, word) = pair
    val applied = "x".apply { length }
    val chosen = names.first().let { text -> text.length }
    val empty: List<String> = emptyList()
}
"#,
        expect![[r#"
            listOf@4 -> kotlin.collections.listOf: List<String>
            names@5 -> local names@4: List<String>
            first@5 -> kotlin.collections.List.first: String
            names@6 -> local names@4: List<String>
            map@6 -> kotlin.collections.map: List<Int>
            it@6 -> local it@6: String
            length@6 -> kotlin.String.length: Int
            names@7 -> local names@4: List<String>
            forEach@7 -> kotlin.collections.forEach: Unit
            name@7 -> local name@7: String
            length@7 -> kotlin.String.length: Int
            Box@8 -> app.Box: Box<List<String>>
            names@8 -> local names@4: List<String>
            boxed@9 -> local boxed@8: Box<List<String>>
            value@9 -> app.Box.value: List<String>
            first@9 -> kotlin.collections.List.first: String
            to@10 -> kotlin.to: Pair<Int, String>
            pair@11 -> local pair@10: Pair<Int, String>
            apply@12 -> kotlin.apply: String
            length@12 -> kotlin.String.length: Int
            names@13 -> local names@4: List<String>
            first@13 -> kotlin.collections.List.first: String
            let@13 -> kotlin.let: Int
            text@13 -> local text@13: String
            length@13 -> kotlin.String.length: Int
            emptyList@14 -> kotlin.collections.emptyList: List<String>"#]],
    );
}

#[test]
fn java_members_constructors_companions_and_enums() {
    check_inference(
        r#"
//- /app/Store.java
package app;
public class Store {
    public Store(String name) {}
    public String getTitle() { return null; }
    public void setTitle(String title) {}
    public static Store open() { return null; }
}
//- /app/Use.kt
package app
enum class Status { ACTIVE, CLOSED }
class Money(val amount: Long, val currency: String = "USD") {
    companion object {
        val ZERO = Money(0)
        fun of(amount: Long) = Money(amount, currency = "EUR")
    }
}
fun use() {
    val store = Store("main")
    store.title.length
    store.title = "x"
    val opened = Store.open()
    val zero = Money.ZERO.amount
    val euro = Money.of(5).currency
    val status = Status.ACTIVE
    status.name
}
"#,
        expect![[r#"
            Money@5 -> app.Money: Money
            Money@6 -> app.Money: Money
            amount@6 -> local amount@6: Long
            Store@10 -> app.Store.Store: Store
            store@11 -> local store@10: Store
            title@11 -> app.Store.getTitle: String!
            length@11 -> kotlin.String.length: Int
            store@12 -> local store@10: Store
            title@12 -> app.Store.getTitle: String!
            Store@13 -> app.Store: {unknown}
            open@13 -> app.Store.open: Store!
            Money@14 -> app.Money: Money.Companion
            ZERO@14 -> app.Money.Companion.ZERO: Money
            amount@14 -> app.Money.amount: Long
            Money@15 -> app.Money: Money.Companion
            of@15 -> app.Money.Companion.of: Money
            currency@15 -> app.Money.currency: String
            Status@16 -> app.Status: {unknown}
            ACTIVE@16 -> app.Status.ACTIVE: Status
            status@17 -> local status@16: Status
            name@17 -> kotlin.Enum.name: String"#]],
    );
}

#[test]
fn self_referencing_declarations_are_unknown_not_a_hang() {
    check_inference(
        r#"
//- /app/Use.kt
package app
fun ping() = pong()
fun pong() = ping()
fun use() {
    val value = ping()
}
"#,
        expect![[r#"
            pong@2 -> app.pong: {unknown}
            ping@3 -> app.ping: {unknown}
            ping@5 -> app.ping: {unknown}"#]],
    );
}

#[test]
fn overloads_declared_in_one_class_all_stay_members() {
    let fixture = with_stdlib(
        r#"
//- /app/Use.kt
package app
open class Base {
    open fun find(id: Int): String = TODO()
}
class Repo : Base() {
    override fun find(id: Int): String = TODO()
    fun find(name: String): Int = TODO()
}
val repo: Repo = TODO()
"#,
    );
    // `Base.find` is overridden; Repo's two overloads both remain.
    assert_eq!(
        describe_members(&fixture, "repo", "find"),
        vec!["fun Repo.find: String", "fun Repo.find: Int"]
    );
}

#[test]
fn generated_functions_and_inherited_companions() {
    check_inference(
        r#"
//- /app/Base.kt
package app
abstract class TestBase {
    companion object {
        const val API_KEY = "key"
        fun freshId(): Long = TODO()
    }
}
//- /app/Use.kt
package app
data class Point(val x: Int, val y: Int)
enum class Color { RED }
class UseTest : TestBase() {
    fun use(point: Point) {
        val moved = point.copy(y = 2)
        val color = Color.valueOf("RED")
        val all = Color.values()
        val key = API_KEY
        val id = freshId()
    }
}
"#,
        expect![[r#"
            point@6 -> local point@5: Point
            copy@6 -> app.Point: Point
            Color@7 -> app.Color: {unknown}
            valueOf@7 -> app.Color: Color
            Color@8 -> app.Color: {unknown}
            values@8 -> app.Color: Array<Color>
            API_KEY@9 -> app.TestBase.Companion.API_KEY: String
            freshId@10 -> app.TestBase.Companion.freshId: Long"#]],
    );
}

#[test]
fn extension_receivers_and_smart_casts_of_this() {
    check_inference(
        r#"
//- /app/Use.kt
package app
sealed class Response
class Found(val data: String) : Response()
class Missing : Response()
data class Filter(val user: String?)
fun Filter.describe(): String = "$this ${user?.length}"
fun Response.text(): String = when (this) {
    is Found -> data
    is Missing -> "none"
}
fun Response.size(): Int {
    if (this is Found) {
        return data.length
    }
    return 0
}
"#,
        expect![[r#"
            user@6 -> app.Filter.user: String?
            length@6 -> kotlin.String.length: Int?
            data@8 -> app.Found.data: String
            data@13 -> app.Found.data: String
            length@13 -> kotlin.String.length: Int"#]],
    );
}

#[test]
fn lambdas_in_varargs_and_extensions_of_companions() {
    check_inference(
        r#"
//- /app/Use.kt
package app
class Rate(val date: String, val code: String) {
    companion object
}
fun Rate.Companion.of(code: String): Rate = TODO()
fun use(rates: List<Rate>) {
    val sorted = rates.sortedWith(compareBy({ it.date }, { it.code }))
    val parsed = Rate.of("USD")
}
"#,
        expect![[r#"
            TODO@5 -> kotlin.TODO: Nothing
            rates@7 -> local rates@6: List<Rate>
            sortedWith@7 -> kotlin.collections.sortedWith: List<Rate>
            compareBy@7 -> kotlin.collections.compareBy: Comparator<Rate>
            it@7 -> local it@7: Rate
            date@7 -> app.Rate.date: String
            it@7 -> local it@7: Rate
            code@7 -> app.Rate.code: String
            Rate@8 -> app.Rate: Rate.Companion
            of@8 -> app.of: Rate"#]],
    );
}

#[test]
fn the_most_specific_receiver_and_the_functional_interface_with_the_lambdas_arity_win() {
    check_inference(
        r#"
//- /app/Rows.java
package app;
public interface Rows<T> { T map(String row, int index); }
//- /app/Each.java
package app;
public interface Each { void accept(String row); }
//- /lib/java/lang/Class.java
package java.lang;
public final class Class<T> {}
//- /app/Query.java
package app;
public class Query {
    public <T> java.util.List<T> query(Rows<T> rows) { return null; }
    public void query(Each each) {}
    public <T> java.util.List<T> query(Class<T> type) { return null; }
}
//- /app/Use.kt
package app
fun <T> Iterable<T>.firstItem(): T = TODO()
fun <T> List<T>.firstItem(): T = TODO()
fun use(query: Query, names: List<String>) {
    val found = query.query { row, _ -> row.length }
    val first = names.firstItem()
}
"#,
        expect![[r#"
            TODO@2 -> kotlin.TODO: Nothing
            TODO@3 -> kotlin.TODO: Nothing
            query@5 -> local query@4: Query
            query@5 -> app.Query.query: MutableList<Int>!
            row@5 -> local row@5: String!
            length@5 -> kotlin.String.length: Int
            names@6 -> local names@4: List<String>
            firstItem@6 -> app.firstItem: String"#]],
    );
}

#[test]
fn a_generic_overload_is_out_when_the_argument_breaks_its_bounds() {
    check_inference(
        r#"
//- /app/Assertions.java
package app;
public class Assertions {
    public static <T extends Comparable<T>> ComparableAssert assertThat(T actual) { return null; }
    public static <T> ObjectAssert assertThat(T actual) { return null; }
}
//- /app/ComparableAssert.java
package app;
public class ComparableAssert {}
//- /app/ObjectAssert.java
package app;
public class ObjectAssert {}
//- /app/Use.kt
package app
class Category(val name: String)
fun use(category: Category?) {
    val checked = Assertions.assertThat(category)
}
"#,
        expect![[r#"
            Assertions@4 -> app.Assertions: {unknown}
            assertThat@4 -> app.Assertions.assertThat: ObjectAssert!
            category@4 -> local category@3: Category?"#]],
    );
}

#[test]
fn a_java_collection_parameter_takes_read_only_collections_and_nothing_else() {
    check_inference(
        r#"
//- /lib/java/util/Collection.java
package java.util;
public interface Collection<E> {}
//- /app/Checks.java
package app;
public class Checks {
    public static ListCheck check(java.util.List<String> values) { return null; }
    public static ObjectCheck check(Object value) { return null; }
}
//- /app/ListCheck.java
package app;
public class ListCheck {}
//- /app/ObjectCheck.java
package app;
public class ObjectCheck {}
//- /app/Use.kt
package app
class Category
fun use(names: List<String>, category: Category) {
    val listed = Checks.check(names)
    val other = Checks.check(category)
}
"#,
        expect![[r#"
            Checks@4 -> app.Checks: {unknown}
            check@4 -> app.Checks.check: ListCheck!
            names@4 -> local names@3: List<String>
            Checks@5 -> app.Checks: {unknown}
            check@5 -> app.Checks.check: ObjectCheck!
            category@5 -> local category@3: Category"#]],
    );
}
