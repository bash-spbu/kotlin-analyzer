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
    if members.is_empty() {
        members = types.java_getters(&receiver, Name::new(name));
    }
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
    public String getURL() { return null; }
    public String getaway() { return null; }
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
    // A boolean `is` getter keeps its name; an acronym is lowered whole.
    assert_eq!(
        describe_members(&fixture, "store", "isOpen"),
        vec!["fun Store.isOpen: Boolean"]
    );
    assert!(describe_members(&fixture, "store", "open").is_empty());
    assert_eq!(
        describe_members(&fixture, "store", "url"),
        vec!["fun Store.getURL: String!"]
    );
    assert!(describe_members(&fixture, "store", "away").is_empty());
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
    for unresolved in inference.unresolved() {
        let start = unresolved.range.start;
        lines.push((
            start,
            format!("{}@{} -> ?", unresolved.name, line_of(start)),
        ));
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
            owner@6 -> app.Account.owner: String?
            name@7 -> local name@6: String?
            name@8 -> local name@6: String
            label@10 -> app.Account.label: String
            id@12 -> app.Account.id: Int
            extra@12 -> local extra@12: Int
            account@15 -> local account@14: Account
            total@15 -> app.Account.total: Int
            account@16 -> local account@14: Account
            describe@16 -> app.Account.describe: String
            length@16 -> kotlin.String.length: Int"#]],
    );
}

#[test]
fn comments_before_the_first_statement_or_expression_are_skipped() {
    check_inference(
        r#"
//- /app/Use.kt
package app
class Switches(val enabled: Boolean)
fun <T> run(block: () -> T): T = block()
fun blockBody(switches: Switches) {
    // given
    val value = switches
    value.enabled
}
fun expressionBody(switches: Switches) = /* the flag */ switches.enabled
fun lambda(switches: Switches) = run {
    // first
    switches.enabled
}
fun parenthesized(switches: Switches) = (/* inner */ switches).enabled
fun branch(switches: Switches) = when {
    switches.enabled -> {
        /* chosen */
        switches
    }
    else -> null
}
"#,
        expect![[r#"
            block@3 -> local block@3: T
            switches@6 -> local switches@4: Switches
            value@7 -> local value@6: Switches
            enabled@7 -> app.Switches.enabled: Boolean
            switches@9 -> local switches@9: Switches
            enabled@9 -> app.Switches.enabled: Boolean
            run@10 -> app.run: Boolean
            switches@12 -> local switches@10: Switches
            enabled@12 -> app.Switches.enabled: Boolean
            switches@14 -> local switches@14: Switches
            enabled@14 -> app.Switches.enabled: Boolean
            switches@16 -> local switches@15: Switches
            enabled@16 -> app.Switches.enabled: Boolean
            switches@18 -> local switches@15: Switches"#]],
    );
}

#[test]
fn a_smart_cast_to_a_type_parameter_is_unknown_not_the_parameter() {
    check_inference(
        r#"
//- /app/Use.kt
package app
open class Node { fun remove(): Boolean = true }
fun <T> List<T>.remove(index: Int): T = TODO()
inline fun <reified T> firstOf(node: Node): T? {
    if (node !is T) return null
    node.remove()
    return node
}
fun <T> same(value: T, other: Any?) {
    if (other is T) other
    if (value is T) value
}
"#,
        expect![[r#"
            TODO@3 -> kotlin.TODO: Nothing
            node@5 -> local node@4: Node
            node@6 -> local node@4: {unknown}
            node@7 -> local node@4: {unknown}
            other@10 -> local other@9: Any?
            other@10 -> local other@9: T
            value@11 -> local value@9: T
            value@11 -> local value@9: T"#]],
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
            currency@6 -> app.Money.currency: String
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
fn only_a_member_with_the_same_parameter_types_overrides() {
    let fixture = with_stdlib(
        r#"
//- /app/Use.kt
package app
open class Base {
    open fun find(id: Int): String = TODO()
    open fun <T> wrap(value: T): String = TODO()
}
class Repo : Base() {
    fun find(name: String): Int = TODO()
    override fun <U> wrap(value: U): String = TODO()
}
interface Store<T> { fun save(item: T): Unit }
class Users : Store<String> { override fun save(item: String) {} }
interface Bag<E> { fun put(element: E): Boolean }
interface ListBag<E> : Bag<E> { override fun put(element: E): Boolean }
val repo: Repo = TODO()
val users: Users = TODO()
val unknownElements: ListBag<Missing> = TODO()
"#,
    );
    // An argument that isn't known doesn't make an override an overload.
    assert_eq!(
        describe_members(&fixture, "unknownElements", "put"),
        vec!["fun ListBag.put: Boolean"]
    );
    // Same arity, other parameter type: an overload, not an override.
    assert_eq!(
        describe_members(&fixture, "repo", "find"),
        vec!["fun Repo.find: Int", "fun Base.find: String"]
    );
    assert_eq!(
        describe_members(&fixture, "repo", "wrap"),
        vec!["fun Repo.wrap: String"]
    );
    assert_eq!(
        describe_members(&fixture, "users", "save"),
        vec!["fun Users.save: -"]
    );
}

#[test]
fn inherited_overloads_and_defaults_follow_the_parameter_types() {
    check_inference(
        r#"
//- /app/Use.kt
package app
open class Finder {
    fun find(id: Int): String = TODO()
}
class Named : Finder() {
    fun find(name: String): Int = TODO()
}
interface Api {
    fun get(id: Int, verbose: Boolean = false): Int
    fun get(name: String, verbose: Int): String
}
class Impl : Api {
    override fun get(id: Int, verbose: Boolean): Int = TODO()
    override fun get(name: String, verbose: Int): String = TODO()
}
fun use(named: Named, impl: Impl) {
    named.find(1)
    impl.get(1)
    impl.get("x")
}
"#,
        expect![[r#"
            TODO@3 -> kotlin.TODO: Nothing
            TODO@6 -> kotlin.TODO: Nothing
            TODO@13 -> kotlin.TODO: Nothing
            TODO@14 -> kotlin.TODO: Nothing
            named@17 -> local named@16: Named
            find@17 -> app.Finder.find: String
            impl@18 -> local impl@16: Impl
            get@18 -> app.Impl.get: Int
            impl@19 -> local impl@16: Impl
            get@19 -> one of [app.Impl.get, app.Impl.get]: {unknown}"#]],
    );
}

#[test]
fn a_java_subclass_inherits_its_superclass_static_overloads() {
    check_facts(
        r#"
//- /app/Base.java
package app;
public class Base {
    public static Base of(int first, int second) { return null; }
    public static Base copy(Base base) { return null; }
}
//- /app/Child.java
package app;
public class Child extends Base {
    public static Child of(String name) { return null; }
    public static Child copy(Base base) { return null; }
}
//- /app/Use.kt
package app
fun use() {
    Child.of(1, 2)
    Child.of("x")
    Child.of()
    Child.copy(Child.of("x"), 1)
}
"#,
        expect![[r#"
            arguments@5 2 candidates
            arguments@6 1 candidates"#]],
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
            y@6 -> app.Point.y: Int
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

/// The facts checks report from, one per line: `kind@line detail`.
fn check_facts(fixture: &str, expected: Expect) {
    let fixture = with_stdlib(fixture);
    let db = &fixture.database;
    let file = db.source_file(fixture.file("/app/Use.kt")).unwrap();
    let text = file.text(db).clone();
    let inference = infer_file(db, file);
    let line_of = |offset: u32| text[..offset as usize].matches('\n').count() + 1;
    let mut lines: Vec<(u32, String)> = Vec::new();
    for unresolved in inference.unresolved() {
        lines.push((
            unresolved.range.start,
            format!(
                "unresolved@{} {} {:?}",
                line_of(unresolved.range.start),
                unresolved.name,
                unresolved.certainty
            ),
        ));
    }
    for mismatch in inference.argument_mismatches() {
        lines.push((
            mismatch.range.start,
            format!(
                "arguments@{} {} candidates",
                line_of(mismatch.range.start),
                mismatch.candidates.len()
            ),
        ));
    }
    for call in inference.unsafe_calls() {
        lines.push((
            call.range.start,
            format!(
                "unsafe@{} {}",
                line_of(call.range.start),
                display(db, &call.receiver)
            ),
        ));
    }
    for when in inference.non_exhaustive_whens() {
        lines.push((
            when.range.start,
            format!(
                "when@{} missing {}",
                line_of(when.range.start),
                when.missing.join(", ")
            ),
        ));
    }
    lines.sort();
    let lines: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
    expected.assert_eq(&lines.join("\n"));
}

#[test]
fn facts_for_unresolved_names_and_argument_mismatches() {
    check_facts(
        r#"
//- /app/Use.kt
package app
fun pay(amount: Long, currency: String = "USD") {}
fun <T> unknownShape(block: T) {}
fun use() {
    pay(1, "EUR")
    pay()
    pay(1, "EUR", true)
    pay(1, note = "x")
    missing()
    with(1) { alsoMissing() }
}
"#,
        expect![[r#"
            arguments@6 1 candidates
            arguments@7 1 candidates
            arguments@8 1 candidates
            unresolved@9 missing Certain
            unresolved@10 alsoMissing Certain"#]],
    );
}

#[test]
fn facts_for_unsafe_calls_skip_anything_that_could_be_smart_cast() {
    check_facts(
        r#"
//- /app/Use.kt
package app
class Account(val owner: String?, val backup: String?, val note: String?)
fun String?.orBlank(): String = this ?: ""
fun use(account: Account) {
    account.owner.length
    if (account.backup != null) {
        account.backup.length
    }
    account.note.orBlank()
    account.note.toString()
}
fun other(text: String?) {
    requireNotNull(text)
    text.length
}
"#,
        expect!["unsafe@5 String?"],
    );
}

#[test]
fn facts_for_when_over_enums_sealed_types_and_booleans() {
    check_facts(
        r#"
//- /app/Use.kt
package app
enum class Color { RED, GREEN, BLUE }
sealed interface Shape
class Circle : Shape
object Dot : Shape
sealed class Polygon : Shape
class Square : Polygon()
fun use(color: Color, shape: Shape, flag: Boolean, maybe: Color?) {
    when (color) { Color.RED -> 1; Color.GREEN -> 2 }
    when (color) { Color.RED -> 1; else -> 2 }
    when (shape) { is Circle -> 1; Dot -> 2 }
    when (shape) { is Circle -> 1; Dot -> 2; is Polygon -> 3 }
    when (flag) { true -> 1 }
    when (maybe) { Color.RED, Color.GREEN, Color.BLUE -> 1 }
}
fun narrowed(color: Color) {
    if (color == Color.BLUE) return
    when (color) { Color.RED -> 1; Color.GREEN -> 2 }
}
"#,
        expect![[r#"
            when@9 missing Color.BLUE
            when@11 missing is Square
            when@13 missing false
            when@14 missing null"#]],
    );
}

#[test]
fn named_arguments_in_their_own_positions_let_positional_ones_follow() {
    check_facts(
        r#"
//- /app/Use.kt
package app
fun record(name: String, duration: Long, vararg tags: String) {}
fun use() {
    record(name = "call", duration = 1, "a", "b")
    record(duration = 1, name = "call", "a")
}
"#,
        expect!["arguments@5 1 candidates"],
    );
}

#[test]
fn constructor_parameters_stay_out_of_functions_and_member_extensions_resolve() {
    check_inference(
        r#"
//- /app/Use.kt
package app
open class Client { fun base(): Int = 1 }
class Impl : Client() { fun extra(): Int = 2 }
class Service(client: Client, private val values: Map<String, String>) {
    private val client = client as Impl
    val first = client.base()
    fun use() {
        client.extra()
        values.pick("key")
    }
    private fun Map<String, String>.pick(key: String): String = TODO()
}
"#,
        expect![[r#"
            client@5 -> local client@4: Client
            client@6 -> local client@4: Client
            base@6 -> app.Client.base: Int
            client@8 -> app.Service.client: Impl
            extra@8 -> app.Impl.extra: Int
            values@9 -> app.Service.values: Map<String, String>
            pick@9 -> app.Service.pick: String
            TODO@11 -> kotlin.TODO: Nothing"#]],
    );
}

#[test]
fn a_type_parameter_bounded_by_any_binds_the_non_null_type() {
    check_inference(
        r#"
//- /app/Use.kt
package app
class Gas(val price: Long)
fun <T : Any> match(matcher: (T) -> Boolean): T = TODO()
fun prepare(gas: Gas?) {}
fun use() {
    prepare(match { it.price > 0 })
}
"#,
        expect![[r#"
            TODO@3 -> kotlin.TODO: Nothing
            prepare@6 -> app.prepare: Unit
            match@6 -> app.match: Gas
            it@6 -> local it@6: Gas
            price@6 -> app.Gas.price: Long"#]],
    );
}

#[test]
fn named_argument_labels_and_annotation_arguments_resolve() {
    check_inference(
        r#"
//- /app/Listener.java
package app;
public @interface Listener { String topic(); int batch() default 1; }
//- /app/Use.kt
package app
const val TOPIC = "events"
data class Deposit(val userId: Long, val amount: Long = 0)
class Consumer {
    @Listener(topic = TOPIC, batch = 2)
    fun consume() {
        val deposit = Deposit(userId = 1)
        deposit.copy(amount = 2)
    }
}
"#,
        expect![[r#"
            topic@5 -> app.Listener.topic: {unknown}
            TOPIC@5 -> app.TOPIC: String
            batch@5 -> app.Listener.batch: {unknown}
            Deposit@7 -> app.Deposit: Deposit
            userId@7 -> app.Deposit.userId: Long
            deposit@8 -> local deposit@7: Deposit
            copy@8 -> app.Deposit: Deposit
            amount@8 -> app.Deposit.amount: Long"#]],
    );
}

#[test]
fn smart_casts_keep_the_more_specific_type_and_follow_casts_and_contracts() {
    check_inference(
        r#"
//- /app/Use.kt
package app
interface Named { val name: String }
interface Aged { val age: Int }
class Cat(override val name: String, override val age: Int) : Named, Aged
fun use(any: Any, named: Named, list: List<String>, maybe: String?, other: String?, last: Any) {
    if (any is String) any.length
    if (named is Aged) named.age
    if (list is Collection<*>) list.first()
    when (named) { is Cat, is Aged -> named.name }
    any as Cat
    any.age
    requireNotNull(maybe)
    maybe.length
    require(other != null && last is Cat)
    other
    last.name
}
"#,
        expect![[r#"
            any@6 -> local any@5: Any
            any@6 -> local any@5: String
            length@6 -> kotlin.String.length: Int
            named@7 -> local named@5: Named
            named@7 -> local named@5: {unknown}
            list@8 -> local list@5: List<String>
            list@8 -> local list@5: List<String>
            first@8 -> kotlin.collections.List.first: String
            named@9 -> local named@5: Named
            named@9 -> local named@5: Named
            name@9 -> app.Named.name: String
            any@10 -> local any@5: Any
            any@11 -> local any@5: Cat
            age@11 -> app.Cat.age: Int
            requireNotNull@12 -> kotlin.requireNotNull: String
            maybe@12 -> local maybe@5: String?
            maybe@13 -> local maybe@5: String
            length@13 -> kotlin.String.length: Int
            require@14 -> kotlin.require: Unit
            other@14 -> local other@5: String?
            last@14 -> local last@5: Any
            other@15 -> local other@5: String
            last@16 -> local last@5: Cat
            name@16 -> app.Cat.name: String"#]],
    );
}

#[test]
fn facts_follow_this_paths_and_contracts_before_a_when() {
    check_facts(
        r#"
//- /app/Use.kt
package app
enum class Side { BUY, SELL }
class Order(var note: String?, var side: Side?) {
    fun noteLength(): Int {
        if (this.note == null) return 0
        return note.length
    }
    fun label(): Int {
        requireNotNull(side)
        return when (side) { Side.BUY -> 1; Side.SELL -> 2 }
    }
}
fun asserted(side: Side?): Int {
    side!!
    return when (side) { Side.BUY -> 1; Side.SELL -> 2 }
}
fun unchecked(side: Side?): Int = when (side) { Side.BUY -> 1; Side.SELL -> 2 }
class Info(val legacyId: String?, val note: String?)
class Holder(val info: Info?) {
    val key: Int?
        get() {
            val id = info?.legacyId ?: return null
            return info.note?.length
        }
}
"#,
        expect!["when@17 missing null"],
    );
}

#[test]
fn common_types_super_inner_classes_local_extensions_and_object_literals() {
    check_inference(
        r#"
//- /app/Use.kt
package app
interface Named { val name: String }
open class Pet { fun feed() {} }
class Cat : Pet(), Named { override val name = "cat" }
class Dog : Pet(), Named { override val name = "dog" }
class Fish : Pet()
interface Walker { fun walk(): Int = 1 }
open class Base { open fun walk(): String = "" ; open fun rest(): Long = 0 }
class Outer { inner class Inner(val depth: Int) }
class Robot : Base(), Walker {
    override fun walk(): String {
        val step = super<Walker>.walk()
        val rest = super.rest()
        with(Outer()) { super.rest() }
        return super<Base>.walk()
    }
}
fun use(flag: Boolean, outer: Outer) {
    val both = if (flag) Cat() else Dog()
    val pet = if (flag) Cat() else Fish()
    val inner = outer.Inner(1)
    fun String.shout(): String = this
    val loud = "x".shout()
    val counter = object : Pet() { fun count(): Int = 1 }
    counter.count()
    val plain = object : Pet() {}
    plain.feed()
    pet.feed()
    both.name
}
"#,
        expect![[r#"
            walk@12 -> app.Walker.walk: Int
            rest@13 -> app.Base.rest: Long
            with@14 -> kotlin.with: Long
            Outer@14 -> app.Outer: Outer
            rest@14 -> app.Base.rest: Long
            walk@15 -> app.Base.walk: String
            flag@19 -> local flag@18: Boolean
            Cat@19 -> app.Cat: Cat
            Dog@19 -> app.Dog: Dog
            flag@20 -> local flag@18: Boolean
            Cat@20 -> app.Cat: Cat
            Fish@20 -> app.Fish: Fish
            outer@21 -> local outer@18: Outer
            Inner@21 -> app.Outer.Inner: Outer.Inner
            shout@23 -> local shout@22: String
            counter@25 -> local counter@24: {unknown}
            plain@27 -> local plain@26: Pet
            feed@27 -> app.Pet.feed: Unit
            pet@28 -> local pet@20: Pet
            feed@28 -> app.Pet.feed: Unit
            both@29 -> local both@19: {unknown}"#]],
    );
}

#[test]
fn facts_for_enums_in_sealed_hierarchies_and_unresolved_bounds() {
    check_facts(
        r#"
//- /app/Use.kt
package app
sealed interface Event
enum class Simple : Event { START, STOP }
data class Custom(val name: String) : Event
fun all(event: Event): Int = when (event) {
    Simple.START -> 1
    Simple.STOP -> 2
    is Custom -> 3
}
fun some(event: Event): Int = when (event) {
    Simple.START -> 1
    is Custom -> 3
}
fun <T : Missing> bounded(value: T) {
    value.anything()
}
"#,
        expect![[r#"
            when@10 missing Simple.STOP
            unresolved@15 anything Uncertain"#]],
    );
}

#[test]
fn accessor_bodies_on_their_own_lines_see_field_and_the_value() {
    check_inference(
        r#"
//- /app/Use.kt
package app
class Account(private val prefix: String) {
    var balance: Long = 0
        get() = field + 1
        set(value) {
            field = value
        }
    val label
        get() = prefix.length
}
val total: Int = 0
    get() = field
"#,
        expect![[r#"
            field@4 -> local field@4: Long
            field@6 -> local field@5: Long
            value@6 -> local value@5: Long
            prefix@9 -> app.Account.prefix: String
            length@9 -> kotlin.String.length: Int
            field@12 -> local field@12: Int"#]],
    );
}

#[test]
fn accessors_after_a_comment_still_belong_to_their_property() {
    check_inference(
        r#"
//- /app/Use.kt
package app
class Account {
    var balance: Long = 0
        // read
        get() = field + 1
        /* written */
        set(value) {
            field = value
        }
}
"#,
        expect![[r#"
            field@5 -> local field@5: Long
            field@8 -> local field@7: Long
            value@8 -> local value@7: Long"#]],
    );
}

#[test]
fn a_body_edit_in_one_file_leaves_other_files_inference_alone() {
    let mut fixture = with_stdlib(
        r#"
//- /app/Account.kt
package app
class Account { fun balance(): Long = 0 }
//- /app/Use.kt
package app
fun use(account: Account) = account.balance()
"#,
    );
    let inference = |fixture: &Fixture| {
        let db = &fixture.database;
        infer_file(db, db.source_file(fixture.file("/app/Use.kt")).unwrap())
    };
    let before = inference(&fixture);
    fixture.edit(
        "/app/Account.kt",
        "package app\n\n// moved\nclass Account { fun balance(): Long = 1 + 41 }\n",
    );
    assert!(
        std::sync::Arc::ptr_eq(&before, &inference(&fixture)),
        "Use.kt's inference is reused"
    );
    fixture.edit(
        "/app/Account.kt",
        "package app\nclass Account { fun balance(): Int = 0 }\n",
    );
    let after = inference(&fixture);
    assert!(!std::sync::Arc::ptr_eq(&before, &after));
}

#[test]
fn a_block_bodied_function_is_unit_without_inferring_its_body() {
    check_inference(
        r#"
//- /app/Use.kt
package app
fun first() { second() }
fun second() = first()
val result = second()
"#,
        expect![[r#"
            second@2 -> app.second: Unit
            first@3 -> app.first: Unit
            second@4 -> app.second: Unit"#]],
    );
}

#[test]
fn defaults_see_every_parameter_and_a_function_property_is_called_when_no_function_fits() {
    check_facts(
        r#"
//- /app/Use.kt
package app
class Detail(val text: String)
open class Failure { val cause: Detail? = null }
class Wrapped(
    message: String = cause.text,
    cause: Detail,
) : Failure()
class Mapper<R>(private val notFound: (String) -> R) {
    fun notFound(): R = notFound("missing")
}
fun later(first: String = second.text, second: Detail) {}
"#,
        expect![""],
    );
}

#[test]
fn negated_type_tests_nullable_receiver_extensions_and_safe_call_tests() {
    check_facts(
        r#"
//- /app/Use.kt
package app
class Recorder { fun snapshot(): Int = 1 }
class Currency(val value: String)
class Deposit(val fiat: Currency?)
class Stats {
    fun direct(recorder: Recorder?): Int = recorder.snapshot()
    private fun Recorder?.snapshot(): Int = this?.snapshot() ?: 0
    fun resolve(scenario: Any?): Any? {
        if (scenario !is Map<*, *>) return scenario
        return scenario.label("name")
    }
    private fun Map<*, *>.label(field: String): String = field
    fun fiat(deposit: Deposit?): String {
        if (deposit?.fiat != null) {
            return deposit.fiat.value
        }
        return ""
    }
    fun branch(value: Any): Int = when (value) {
        !is Recorder -> 0
        else -> value.snapshot()
    }
}
"#,
        expect![""],
    );
}
