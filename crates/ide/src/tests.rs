use base_db::fixture::Fixture;
use expect_test::{Expect, expect};
use hir::FileId;

use crate::{Analysis, DocumentSymbol};

fn analysis(fixture: &Fixture) -> Analysis {
    Analysis {
        db: fixture.database.clone(),
    }
}

/// `path:line:Name` for each definition of what's under `$0`.
fn check_definition(fixture: &str, expected: Expect) {
    let fixture = Fixture::parse(fixture);
    let (file, offset) = fixture.cursor();
    let targets = analysis(&fixture).goto_definition(file, offset).unwrap();
    let described: Vec<String> = targets
        .iter()
        .map(|target| {
            let crate::NavigationFile::Workspace(file) = target.file else {
                return format!("library:{}", target.name);
            };
            let text = fixture.vfs.contents(file).unwrap();
            let line = text[..target.focus_range.start as usize]
                .matches('\n')
                .count()
                + 1;
            format!(
                "{}:{line}:{}",
                fixture.vfs.path(file).display(),
                target.name
            )
        })
        .collect();
    expected.assert_eq(&described.join("\n"));
}

#[test]
fn definition_of_a_type_in_a_signature_skips_the_unrelated_namesake() {
    check_definition(
        r#"
//- /common/a/Money.kt module=:common
package a
class Money
//- /other/a/Money.kt module=:other
package a
class Money
//- /api/b/Use.kt module=:api deps=:common
package b
import a.Money
fun price(): Mo$0ney? = null
"#,
        expect!["/common/a/Money.kt:2:Money"],
    );
}

#[test]
fn definition_in_annotation_arguments_on_every_declaration() {
    let fixture = r#"
//- /app/a/Props.kt
package a
annotation class Enable(val value: Any)
class Props
const val PREFIX = "p"
//- /app/c/Props.kt
package c
class Props
const val PREFIX = "q"
//- /app/b/Config.kt
package b
import a.Enable
import a.Props
import a.PREFIX
@Enable(Props::class)
class Config
@Enable(PREFIX)
class Prefixed
class Holder(@param:Enable(Props::class) val x: Int) {
    @Enable(Props::class) fun method() {}
    @Enable(Props::class) val property = 1
}
@Enable(Props::class) fun top() {}
"#;
    let mut found = Vec::new();
    let cursors = [
        ("Props", 2),
        ("PREFIX", 2),
        ("Props", 3),
        ("Props", 4),
        ("Props", 5),
        ("Props", 6),
    ];
    for (word, nth) in cursors {
        let marked = Fixture::with_mini_stdlib(&mark_nth(fixture, "/app/b/Config.kt", word, nth));
        let (file, offset) = marked.cursor();
        let targets = analysis(&marked).goto_definition(file, offset).unwrap();
        let described: Vec<String> = targets
            .iter()
            .map(|target| {
                let crate::NavigationFile::Workspace(file) = target.file else {
                    return format!("library:{}", target.name);
                };
                format!("{}:{}", marked.vfs.path(file).display(), target.name)
            })
            .collect();
        found.push(format!("{word} #{nth}: {}", described.join(", ")));
    }
    expect![[r#"
        Props #2: /app/a/Props.kt:Props
        PREFIX #2: /app/a/Props.kt:PREFIX
        Props #3: /app/a/Props.kt:Props
        Props #4: /app/a/Props.kt:Props
        Props #5: /app/a/Props.kt:Props
        Props #6: /app/a/Props.kt:Props"#]]
    .assert_eq(&found.join("\n"));
}

#[test]
fn definition_of_each_segment_of_a_qualified_type_and_of_an_import() {
    let fixture = r#"
//- /app/a/b/Outer.kt
package a.b
class Outer { class Inner }
fun helper() {}
//- /app/c/Use.kt
package c
import a.b.helper
val inner: a.b.Outer.Inn$0er? = null
"#;
    check_definition(fixture, expect!["/app/a/b/Outer.kt:2:Inner"]);
    check_definition(
        &fixture
            .replace("Inn$0er", "Inner")
            .replace("a.b.Outer.Inner", "a.b.Out$0er.Inner"),
        expect!["/app/a/b/Outer.kt:2:Outer"],
    );
    check_definition(
        &fixture
            .replace("Inn$0er", "Inner")
            .replace("import a.b.helper", "import a.b.hel$0per"),
        expect!["/app/a/b/Outer.kt:3:helper"],
    );
}

#[test]
fn definition_of_a_supertype_and_of_a_declaration_name() {
    let fixture = r#"
//- /app/x/Base.kt
package x
open class Base
//- /app/x/Child.kt
package x
class Child : Ba$0se()
"#;
    check_definition(fixture, expect!["/app/x/Base.kt:2:Base"]);
    check_definition(
        &fixture
            .replace("Ba$0se()", "Base()")
            .replace("class Child", "class Chi$0ld"),
        expect!["/app/x/Child.kt:2:Child"],
    );
}

#[test]
fn document_symbols_form_a_tree() {
    let fixture = Fixture::parse(
        r#"
//- /app/x/Shapes.kt
package x
sealed interface Shape { fun area(): Double }
data class Circle(val radius: Double) : Shape {
    override fun area(): Double = radius * radius
    companion object { val UNIT = Circle(1.0) }
}
"#,
    );
    let file = fixture.file("/app/x/Shapes.kt");
    let symbols = analysis(&fixture).document_symbols(file).unwrap();
    fn render(symbols: &[DocumentSymbol], depth: usize, output: &mut String) {
        for symbol in symbols {
            let detail = symbol.detail.as_deref().unwrap_or("");
            output.push_str(&format!(
                "{}{:?} {}{detail}\n",
                "  ".repeat(depth),
                symbol.kind,
                symbol.name
            ));
            render(&symbol.children, depth + 1, output);
        }
    }
    let mut output = String::new();
    render(&symbols, 0, &mut output);
    expect![[r#"
        Interface Shape
          Method area(): Double
        Class Circle
          Field radiusDouble
          Method area(): Double
          Object Companion
            Field UNIT
    "#]]
    .assert_eq(&output);
}

#[test]
fn workspace_symbols_rank_exact_before_prefix_before_fuzzy() {
    let fixture = Fixture::parse(
        r#"
//- /app/x/A.kt
package x
class RateService
class Rate
class CurrencyRateProvider
fun rateOf() {}
"#,
    );
    let names: Vec<String> = analysis(&fixture)
        .workspace_symbols("Rate", 10)
        .unwrap()
        .into_iter()
        .map(|target| target.name)
        .collect();
    assert_eq!(
        names,
        vec!["Rate", "rateOf", "RateService", "CurrencyRateProvider"]
    );
}

#[test]
fn hover_shows_signature_documentation_and_place() {
    let fixture = Fixture::parse(
        r#"
//- /app/x/Money.kt
package x
/**
 * An amount in minor units.
 * Never negative.
 */
@JvmInline
value class Money(val cents: Long) {
    fun plus(other: Money): Money = Money(cents + other.cents)
}
//- /app/x/Use.kt
package x
fun total(price: Mon$0ey): Money = price
"#,
    );
    let (file, offset) = fixture.cursor();
    let hover = analysis(&fixture).hover(file, offset).unwrap().unwrap();
    expect![[r#"
        ```kotlin
        value class Money(cents: Long)
        ```

        ---

        An amount in minor units.
        Never negative.

        ---

        in `x`"#]]
    .assert_eq(&hover.markdown);
}

#[test]
fn definition_of_names_in_bodies_follows_inference() {
    let fixture = r#"
//- /app/a/Repo.kt
package a
class Repo {
    fun find(id: Int): String = TODO()
    fun find(name: String): Int = TODO()
}
fun find(id: Int): Long = TODO()
//- /app/a/Use.kt
package a
fun use(repo: Repo) {
    val found = repo.find("x")
    found.plus(1)
}
"#;
    // The member overload taking a `String`, not the top-level `find` nor
    // the `Int` overload.
    check_definition(
        &format!(
            "{}{}",
            base_db::fixture::MINI_STDLIB,
            fixture.replace("repo.find(\"x\")", "repo.fi$0nd(\"x\")")
        ),
        expect!["/app/a/Repo.kt:4:find"],
    );
    check_definition(
        &fixture.replace("found.plus", "fou$0nd.plus"),
        expect!["/app/a/Use.kt:3:found"],
    );
    check_definition(
        &fixture.replace("repo.find", "re$0po.find"),
        expect!["/app/a/Use.kt:2:repo"],
    );
}

#[test]
fn hover_shows_a_local_with_its_type_and_a_generic_member_as_used() {
    let fixture = r#"
//- /app/a/Box.kt
package a
class Box<T>(val content: T)
//- /app/a/Use.kt
package a
fun use() {
    val box = Box("text")
    box.content
}
"#;
    let hover_at = |marked: &str| {
        let fixture = Fixture::with_mini_stdlib(marked);
        let (file, offset) = fixture.cursor();
        analysis(&fixture)
            .hover(file, offset)
            .unwrap()
            .unwrap()
            .markdown
    };
    expect![[r#"
        ```kotlin
        val box: Box<String>
        ```"#]]
    .assert_eq(&hover_at(&fixture.replace("val box", "val b$0ox")));
    expect![[r#"
        ```kotlin
        val content: T
        ```

        Type here: `String`

        ---

        in `a.Box`"#]]
    .assert_eq(&hover_at(&fixture.replace("box.content", "box.con$0tent")));
}

#[test]
fn inlay_hints_show_inferred_local_types_but_not_written_or_constructed_ones() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/a/Use.kt
package a
class Box<T>(val content: T)
fun use() {
    val names = listOf("a")
    val written: List<String> = names
    val box = Box(names)
    var count = names.first().length
    val (number, word) = 1 to "one"
    names.map { it }
}
"#,
    );
    let file = fixture.file("/app/a/Use.kt");
    let text = fixture.vfs.contents(file).unwrap().clone();
    let hints = analysis(&fixture).inlay_hints(file, None).unwrap();
    let rendered: Vec<String> = hints
        .iter()
        .map(|hint| {
            let name_start = text[..hint.offset as usize]
                .rfind(|char: char| !char.is_alphanumeric())
                .map_or(0, |index| index + 1);
            format!("{}{}", &text[name_start..hint.offset as usize], hint.label)
        })
        .collect();
    expect![[r#"
        names: List<String>
        count: Int
        number: Int
        word: String"#]]
    .assert_eq(&rendered.join("\n"));
}

/// Each diagnostic of `/app/b/Use.kt` as `[check] line: message`, with the
/// first fix's edit when it has one.
fn check_diagnostics(fixture: &str, config: crate::DiagnosticsConfig, expected: Expect) {
    check_fixture_diagnostics(Fixture::with_mini_stdlib(fixture), config, expected);
}

fn check_fixture_diagnostics(fixture: Fixture, config: crate::DiagnosticsConfig, expected: Expect) {
    let file = fixture.file("/app/b/Use.kt");
    let text = fixture.vfs.contents(file).unwrap().clone();
    let diagnostics = analysis(&fixture).diagnostics(file, &config).unwrap();
    let rendered: Vec<String> = diagnostics
        .iter()
        .map(|diagnostic| {
            let line = text[..diagnostic.range.start as usize]
                .matches('\n')
                .count()
                + 1;
            let fix = diagnostic
                .fixes
                .first()
                .map(|fix| format!(" | {}: {:?}", fix.label, fix.edits[0].new_text))
                .unwrap_or_default();
            format!(
                "[{}] {line}: {}{fix}",
                diagnostic.check.name(),
                diagnostic.message
            )
        })
        .collect();
    expected.assert_eq(&rendered.join("\n"));
}

#[test]
fn diagnostics_for_imports_and_packages() {
    check_diagnostics(
        r#"
//- /app/a/Money.kt
package a
class Money(val amount: Long)
fun format(money: Money): String = TODO()
operator fun Money.plus(other: Money): Money = TODO()
//- /app/c/Price.kt
package c
class Price
//- /app/b/Use.kt
package b

import a.plus
import a.Money
import a.format
import x.y.Gone
import kotlin.collections.List

/** Formats with [format]. */
fun use(money: Money, gone: Gone): Price {
    println(money + money)
    return TODO()
}
"#,
        crate::DiagnosticsConfig::default(),
        expect![[r#"
            [unused-import] 7: Unused import `List` | Remove the unused import: ""
            [missing-import] 10: `Price` isn't imported: `import c.Price` | Import `c.Price`: "import c.Price\n""#]],
    );
}

#[test]
fn null_checks_on_values_that_cannot_be_null() {
    check_diagnostics(
        r#"
//- /app/a/Clock.java
package a;
public class Clock {
    public String zone() { return null; }
    public <T> T same(T value) { return value; }
}
//- /app/b/Use.kt
package b
import a.Clock
class Account(val owner: String, val nickname: String?)
fun <T> pick(value: T): T = value
fun use(account: Account, clock: Clock, maybe: String?, items: List<String?>) {
    account.owner!!.length
    account.owner?.length
    val name = account.owner ?: "anonymous"
    if (account.owner == null) return
    if (null != name) println(name)
    account.nickname!!.length
    account.nickname?.length ?: 0
    if (account.nickname != null) println(account.nickname)
    clock.zone()!!.length
    clock.zone()?.length ?: 0
    clock.same(account.owner)!!.length
    clock.zone().let { zone -> zone!!.length }
    pick(maybe)!!.length
    items.first()!!.length
    if (maybe != null) {
        maybe!!.length
        maybe?.length
    }
    (account.owner)!!
    val fallback = account.owner
    val picked = safeCall(fallback) { account.nickname } ?: fallback
    val early = run {
        if (maybe == null) return@run null
        account.owner
    } ?: fallback
    println(picked + early)
    maybe.let { value -> if (value == null) println("none") }
    maybe?.let { value -> value!!.length }
    val (left, right) = maybe to account.owner
    if (left == null || right == null) println("none")
    val polled = try { clock.zone() } catch (failure: Exception) { return }
    if (polled == null) println("none")
    val recovered = attempt { account.nickname }.orElse { account.owner }
    if (recovered == null) println("none")
    val required = maybe ?: fail()
    required!!.length
    var cookie: String? = null
    fun String.cookie() {
        cookie?.length
    }
    cookie = "set"
    val holder: Tags? = null
    holder?.tags[0]?.length
}
class Tags(val tags: List<String>)
fun fail(): Nothing = TODO()
class Outcome<out T>
fun <T> attempt(block: () -> T): Outcome<T> = TODO()
fun <R, T : R> Outcome<T>.orElse(onFailure: () -> R): R = TODO()
fun <T> safeCall(default: T, call: () -> T): T = call()
fun <R> run(block: () -> R): R = block()
"#,
        new_warnings(),
        expect![[r#"
            [unnecessary-not-null-assertion] 6: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: ""
            [unnecessary-safe-call] 7: Unnecessary safe call on a receiver of non-null type `String` | Replace `?.` with `.`: "."
            [useless-elvis] 8: Elvis operator (`?:`) always returns the left operand of non-null type `String` | Remove the elvis operator: ""
            [senseless-comparison] 9: Condition `account.owner == null` is always `false`: `String` isn't nullable
            [senseless-comparison] 10: Condition `null != name` is always `true`: `String` isn't nullable
            [unnecessary-not-null-assertion] 24: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: ""
            [unnecessary-not-null-assertion] 33: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: ""
            [senseless-comparison] 35: Condition `right == null` is always `false`: `String` isn't nullable
            [unnecessary-not-null-assertion] 41: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: """#]],
    );
}

#[test]
fn unused_locals_and_vars_never_assigned_again() {
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
fun use(items: List<String>) {
    val unused = items.first()
    val inTemplate = 1
    println("$inTemplate")
    var counter = 0
    counter += 1
    var stepped = 0
    stepped++
    var once = items.first()
    println(once)
    var inLambda = 0
    items.forEach { inLambda = 1 }
    println(inLambda)
    var declaredLater: Int
    declaredLater = 1
    println(declaredLater)
    val (first, second) = items.first() to items.first()
    val _ = items.first()
    @Suppress("UNUSED_VARIABLE") val quiet = 1
    @Suppress("CAN_BE_VAL") var quietVar = 1
    println(quietVar)
}
@Suppress("unused")
fun suppressed() {
    val ignored = 1
}
"#,
        new_warnings(),
        expect![[r#"
            [unused-variable] 3: Variable `unused` is never used
            [can-be-val] 10: Variable is never assigned again: it can be a `val` | Change to `val`: "val""#]],
    );
}

/// The compiler warnings about null checks and locals, alone.
fn new_warnings() -> crate::DiagnosticsConfig {
    use crate::Check;
    crate::DiagnosticsConfig {
        only: vec![
            Check::UnnecessaryNotNullAssertion,
            Check::UnnecessarySafeCall,
            Check::UselessElvis,
            Check::SenselessComparison,
            Check::UnusedVariable,
            Check::CanBeVal,
        ],
        ..crate::DiagnosticsConfig::default()
    }
}

#[test]
fn without_the_standard_library_a_default_imports_name_is_not_called_missing() {
    // `Result` is kotlin.Result here; a model guessed from the directories
    // knows no libraries, so the workspace's own `Result` isn't the fix.
    let fixture = r#"
//- /app/a/result/Result.kt
package a.result
sealed interface Result<out T>
//- /app/b/Use.kt
package b

fun <T> attempt(block: () -> T): Result<T> = Result.success(block())
"#;
    check_fixture_diagnostics(
        Fixture::parse(fixture),
        crate::DiagnosticsConfig::default(),
        expect![[""]],
    );
    // With the standard library visible, a missing class is still found.
    check_diagnostics(
        r#"
//- /app/a/result/Outcome.kt
package a.result
sealed interface Outcome<out T>
//- /app/b/Use.kt
package b

fun <T> attempt(block: () -> T): Outcome<T> = TODO()
"#,
        crate::DiagnosticsConfig::default(),
        expect![[
            r#"[missing-import] 3: `Outcome` isn't imported: `import a.result.Outcome` | Import `a.result.Outcome`: "\nimport a.result.Outcome\n""#
        ]],
    );
}

#[test]
fn an_assign_import_is_used_by_assignments_and_file_suppressions_are_honoured() {
    check_diagnostics(
        r#"
//- /app/gradle/Dsl.kt
package gradle
class Property<T>
operator fun <T> Property<T>.assign(value: T) {}
fun configure() {}
//- /app/b/Use.kt
@file:Suppress("PackageDirectoryMismatch")

import gradle.assign
import gradle.configure

class Extension
"#,
        crate::DiagnosticsConfig::default(),
        expect![[r#"[unused-import] 4: Unused import `configure` | Remove the unused import: """#]],
    );
    check_diagnostics(
        r#"
//- /app/b/Use.kt
@file:Suppress("UnusedImport", "PackageDirectoryMismatch")

import kotlin.collections.List

class Extension
"#,
        crate::DiagnosticsConfig::default(),
        expect![[""]],
    );
    // Another annotation's strings don't suppress anything.
    check_diagnostics(
        r#"
//- /app/b/Use.kt
@file:JvmName("UnusedImport")

import kotlin.collections.List

class Extension
"#,
        crate::DiagnosticsConfig::default(),
        expect![[r#"[unused-import] 3: Unused import `List` | Remove the unused import: """#]],
    );
}

#[test]
fn calls_through_an_imported_inherited_static_a_companion_invoke_extension_and_a_sam_constructor() {
    check_diagnostics(
        r#"
//- /app/a/ContextCompat.java
package a;
public class ContextCompat {
    public static int checkSelfPermission(a.Context context, String permission) { return 0; }
}
//- /app/a/ActivityCompat.java
package a;
public class ActivityCompat extends ContextCompat {}
//- /app/a/Context.kt
package a
abstract class Context {
    abstract fun checkSelfPermission(permission: String): Int
}
//- /app/a/Handle.kt
package a
class Handle(val initial: Map<String, Any?>) {
    constructor() : this(TODO())
    companion object
}
fun interface Override {
    fun wrap(content: () -> Unit)
}
// A function named like the interface, as compose-ui-test has one.
fun Override(override: Override, content: () -> Unit) {}
//- /app/a/Task.java
package a;
public interface Task { String run(int attempt); }
//- /app/t/Testing.kt
package t
import a.Handle
operator fun Handle.Companion.invoke(route: Any): Handle = Handle()
//- /app/b/Use.kt
package b

import a.ActivityCompat.checkSelfPermission
import a.Context
import a.Handle
import a.Override
import a.Task
import t.invoke

fun granted(context: Context): Boolean = with(context) {
    checkSelfPermission(this, "camera") == 0
}

fun handle(): Handle = Handle(route = 1)

fun override(): Override = Override { content -> content() }

fun task(): Int = Task { attempt -> "try $attempt" }.run(1).length
"#,
        crate::DiagnosticsConfig {
            experimental: true,
            ..crate::DiagnosticsConfig::default()
        },
        expect![[""]],
    );
}

#[test]
fn an_android_modules_generated_r_is_not_called_missing() {
    // `R` is generated in the module's namespace by the build; a library's
    // `R` elsewhere isn't the fix. Without the Android SDK it would be.
    let module = r#"
//- /app/androidx/core/R.kt
package androidx.core
object R
//- /app/b/Use.kt
package b

fun icon(): Any = R.drawable.icon
"#;
    check_diagnostics(
        &format!(
            "//- /app/android/app/Activity.kt\npackage android.app\nopen class Activity\n{module}"
        ),
        crate::DiagnosticsConfig::default(),
        expect![[""]],
    );
    check_diagnostics(
        module,
        crate::DiagnosticsConfig::default(),
        expect![[
            r#"[missing-import] 3: `R` isn't imported: `import androidx.core.R` | Import `androidx.core.R`: "\nimport androidx.core.R\n""#
        ]],
    );
}

#[test]
fn platform_code_calls_the_actual_typealiases_chains_receiver_values_and_member_extensions() {
    let fixture = r#"
//- /lib/java/net/SocketTimeout.java module=:shared set=jvmMain deps=:app
package java.net;
public class SocketTimeout { public SocketTimeout() {} public SocketTimeout(String message) {} }
//- /shared/common/a/Sockets.kt module=:shared set=commonMain deps=:app
package a
expect class SocketTimeout(message: String)
expect fun nonce(size: Int = 16): String
expect class Clock(zone: String = "UTC")
expect class Server {
    fun stop(grace: Long, timeout: Long)
}
// Declared in common code: its type is the `expect`, the JVM sees the `actual`.
fun startServer(): Server = TODO()
typealias Cancelled = Inner
typealias Inner = Failure
open class Failure(message: String?)
class Builder {
    var test: suspend Scope.(client: Int) -> Unit = {}
}
// An extension of the property's name: the member property is called.
fun Builder.test(block: suspend Scope.(client: Int) -> Unit) {}
class Response
class Delegated(val origin: Response, private val content: Response.() -> String) {
    val raw: String get() = origin.content()
}
class Scope
interface Template<in Outer> {
    fun Outer.apply()
}
class Page
fun <T> T.apply(block: T.() -> Unit): T = this
fun Page.render(template: Template<Page>) {
    with(template) { apply() }
}
//- /shared/jvm/a/Sockets.jvm.kt module=:shared set=jvmMain deps=:shared/commonMain,:app
package a
actual typealias SocketTimeout = java.net.SocketTimeout
actual fun nonce(size: Int): String = TODO()
actual class Clock actual constructor(zone: String)
actual class Server {
    actual fun stop(grace: Long, timeout: Long) {}
    fun stop(grace: Long, timeout: Long, unit: String) {}
}
//- /shared/jvm/b/Use.kt module=:shared set=jvmMain deps=:shared/commonMain,:app
package b

import a.Builder
import a.Cancelled
import a.Clock
import a.nonce
import a.Scope
import a.Server
import a.SocketTimeout

suspend fun use(server: Server, builder: Builder, scope: Scope) {
    val timeout = SocketTimeout()
    server.stop(1, 5, "seconds")
    val cancelled = Cancelled("gone")
    builder.test(scope, 1)
    val token = nonce()
    val clock = Clock()
    a.startServer().stop(1, 5, "seconds")
}
"#;
    let fixture = Fixture::with_mini_stdlib(fixture);
    let file = fixture.file("/shared/jvm/b/Use.kt");
    let common = fixture.file("/shared/common/a/Sockets.kt");
    let config = crate::DiagnosticsConfig {
        experimental: true,
        disabled: vec![crate::Check::UnusedVariable],
        ..crate::DiagnosticsConfig::default()
    };
    let render = |file| {
        analysis(&fixture)
            .diagnostics(file, &config)
            .unwrap()
            .iter()
            .map(|diagnostic| format!("[{}] {}", diagnostic.check.name(), diagnostic.message))
            .collect::<Vec<_>>()
            .join("\n")
    };
    expect![[""]].assert_eq(&render(file));
    expect![[""]].assert_eq(&render(common));
}

#[test]
fn members_through_where_bounds_and_a_smart_cast_to_a_generic_class() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/a/Schemes.kt
package a
interface Scheme {
    val name: String
}
class Roles<B>(val base: B) where B : Scheme {
    val key: String = "roles:" + base.name
}
sealed interface ReferenceOr<out A>
data class Value<out A>(val value: A) : ReferenceOr<A>
data class Schema(val title: String, val required: List<String>? = null)
fun withRequired(schema: ReferenceOr<Schema>): ReferenceOr<Schema> {
    if (schema !is Value) return schema
    return Value(schema.value.copy(required = listOf("id")))
}
"#,
    );
    let file = fixture.file("/app/a/Schemes.kt");
    let config = crate::DiagnosticsConfig {
        experimental: true,
        ..crate::DiagnosticsConfig::default()
    };
    let found: Vec<String> = analysis(&fixture)
        .diagnostics(file, &config)
        .unwrap()
        .iter()
        .map(|diagnostic| format!("[{}] {}", diagnostic.check.name(), diagnostic.message))
        .collect();
    expect![[""]].assert_eq(&found.join("\n"));
}

#[test]
fn a_lambda_receivers_member_hides_a_constructor_parameter_not_the_functions_locals() {
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b

class Pipe {
    fun send(first: Int, second: Int) {}
}
class Sink {
    fun send(value: Int) {}
}
class Scope {
    val channel: Sink = Sink()
}
fun produce(block: Scope.() -> Unit) {}
fun consume(block: Scope.(Pipe) -> Unit) {}

class Socket(private val channel: Pipe) {
    // `channel` is the Scope's here, not the constructor parameter.
    val receiver = produce { channel.send(1) }
    // The lambda's own parameter is as near as its receiver: it wins.
    val sender = consume { channel -> channel.send(1, 2) }
}

// A local of the function itself comes before any implicit receiver.
fun send(channel: Pipe) = produce { channel.send(1, 2) }
"#,
        crate::DiagnosticsConfig::default(),
        expect![[""]],
    );
}

#[test]
fn context_parameters_are_named_in_the_body() {
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b

class State(val ready: Boolean)
class Logger
// A namesake the body must not mean: nullable, so `state.ready` would be unsafe.
val state: State? = null

context(state: State, _: Logger)
fun supports(): Boolean = state.ready
"#,
        crate::DiagnosticsConfig::default(),
        expect![[""]],
    );
}

#[test]
fn a_star_import_of_a_class_and_a_local_class_are_in_scope() {
    check_diagnostics(
        r#"
//- /app/a/BaseAssertions.java
package a;
public class BaseAssertions {
    public static void assertFalse(boolean condition) {}
}
//- /app/a/Assertions.java
package a;
public class Assertions extends BaseAssertions {
    public static void assertTrue(boolean condition) {}
}
//- /app/c/Case.kt
package c
// A namesake outside whose constructor takes nothing.
class Case
class Config
//- /app/b/Use.kt
package b

import a.Assertions.*

fun check() {
    assertTrue(true)
    assertFalse(false)
}

fun cases(): Int {
    data class Case(val input: String, val expected: Int)
    data class Config(var enabled: Boolean = false)
    val make = ::Config
    return listOf(Case("a", 1), Case("b", 2)).size
}
"#,
        crate::DiagnosticsConfig {
            experimental: true,
            disabled: vec![crate::Check::UnusedVariable],
            ..crate::DiagnosticsConfig::default()
        },
        expect![[""]],
    );
}

#[test]
fn an_extension_on_a_star_projected_receiver_takes_any_arguments() {
    check_diagnostics(
        r#"
//- /app/kotlin/collections/Filters.kt
package kotlin.collections
fun <R> Iterable<*>.filterIsInstance(): List<R> = TODO()
//- /app/b/Use.kt
package b

class Note
fun notes(annotations: List<Any>): Int = annotations.filterIsInstance<Note>().size
"#,
        crate::DiagnosticsConfig {
            experimental: true,
            ..crate::DiagnosticsConfig::default()
        },
        expect![[""]],
    );
}

#[test]
fn a_partly_resolved_classpath_calls_nothing_missing() {
    // `Price` may be in the library the build couldn't fetch.
    let fixture = |header: &str| {
        format!(
            "//- /app/c/Price.kt\npackage c\nclass Price\n{header}\npackage b\n\nfun price(): Price = TODO()\n"
        )
    };
    check_diagnostics(
        &fixture("//- /app/b/Use.kt"),
        crate::DiagnosticsConfig::default(),
        expect![[
            r#"[missing-import] 3: `Price` isn't imported: `import c.Price` | Import `c.Price`: "\nimport c.Price\n""#
        ]],
    );
    check_diagnostics(
        &fixture("//- /app/b/Use.kt classpath=partial"),
        crate::DiagnosticsConfig::default(),
        expect![[""]],
    );
}

#[test]
fn diagnostics_for_calls_nullability_and_when() {
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b

enum class Side { BUY, SELL }
class Order(val note: String?)
fun place(amount: Long, side: Side = Side.BUY) {}
fun use(order: Order, side: Side) {
    place()
    order.note.length
    when (side) { Side.BUY -> 1 }
    missing()
}
"#,
        crate::DiagnosticsConfig {
            experimental: true,
            ..crate::DiagnosticsConfig::default()
        },
        expect![[r#"
            [argument-mismatch] 7: No `place` takes these arguments
            [unsafe-call] 8: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `String?`
            [non-exhaustive-when] 9: `when` must be exhaustive: add the missing `Side.SELL` or an `else` branch | Add the missing branches: "; Side.SELL -> TODO()"
            [unresolved-reference] 10: Unresolved reference `missing`"#]],
    );
}

/// The first completion items at `$0`: `label kind [import]`.
fn check_completions(fixture: &str, take: usize, expected: Expect) {
    let fixture = Fixture::with_mini_stdlib(fixture);
    let (file, offset) = fixture.cursor();
    let completions = analysis(&fixture).completions(file, offset).unwrap();
    let rendered: Vec<String> = completions
        .items
        .iter()
        .take(take)
        .map(|item| {
            let import = item
                .import
                .as_deref()
                .map(|import| format!(" +import {import}"))
                .unwrap_or_default();
            format!("{} {:?}{import}", item.label, item.kind)
        })
        .collect();
    expected.assert_eq(&rendered.join("\n"));
}

#[test]
fn completion_after_a_dot_offers_members_getters_and_extensions() {
    check_completions(
        r#"
//- /app/a/Store.java
package a;
public class Store {
    public String getTitle() { return null; }
}
//- /app/a/Account.kt
package a
open class Base { fun baseCall(): Int = 1 }
class Account(val owner: String) : Base() {
    fun close() {}
}
fun Account.describe(): String = owner
fun String.shout(): String = this
//- /app/b/Use.kt
package b
import a.Account
import a.Store
import a.describe
fun use(account: Account, store: Store) {
    val local = 1
    account.$0
}
"#,
        6,
        expect![[r#"
            owner Field
            close Method
            baseCall Method
            toString Method
            hashCode Method
            also Function"#]],
    );
}

#[test]
fn completion_of_a_name_offers_locals_then_scope_then_imports() {
    check_completions(
        r#"
//- /app/a/Money.kt
package a
class Money
class MoneyFormat
//- /app/b/Use.kt
package b
class Monitor
fun use(money: Int) {
    val moneyTotal = money + 1
    mon$0
}
"#,
        6,
        expect![[r#"
            moneyTotal Variable
            money Variable
            Monitor Class
            Money Class +import a.Money
            MoneyFormat Class +import a.MoneyFormat"#]],
    );
}

#[test]
fn completion_sees_the_scope_of_the_declaration_around_it_only() {
    let fixture = r#"
//- /app/b/Use.kt
package b
class Outer(val seed: Long, factor: Int) {
    val tripled = factor * 3
    var stored: Long = 0
        get() = fi$0
    inner class Inner(val depth: Int) {
        fun walk(step: Int) = de
    }
}
"#;
    check_completions(fixture, 1, expect!["field Variable"]);
    check_completions(
        &fixture
            .replace("get() = fi$0", "get() = field")
            .replace("= de\n", "= de$0\n"),
        1,
        expect!["depth Field"],
    );
}

#[test]
fn names_kotlin_reserves_are_written_in_backticks() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/b/Use.kt
package b
enum class Direction { `in`, out }
fun use(direction: Direction): Int {
    val `object` = 1
    obj$0
    return when (direction) { Direction.out -> 1 }
}
"#,
    );
    let (file, offset) = fixture.cursor();
    let completions = analysis(&fixture).completions(file, offset).unwrap();
    let object = completions
        .items
        .iter()
        .find(|item| item.label == "object")
        .expect("the local is offered");
    assert_eq!(object.insert_text(), "`object`");
    let diagnostics = analysis(&fixture)
        .diagnostics(file, &crate::DiagnosticsConfig::default())
        .unwrap();
    let fill = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.check == crate::Check::NonExhaustiveWhen)
        .and_then(|diagnostic| diagnostic.fixes.first())
        .expect("a fix for the when");
    assert_eq!(fill.edits[0].new_text, "; Direction.`in` -> TODO()");
}

#[test]
fn completion_in_a_type_offers_classes_only_and_sees_smart_casts_elsewhere() {
    check_completions(
        r#"
//- /app/a/Money.kt
package a
class Money
fun money(): Int = 1
//- /app/b/Use.kt
package b
fun use() {
    val price: Mo$0
}
"#,
        3,
        expect!["Money Class +import a.Money"],
    );
    check_completions(
        r#"
//- /app/b/Use.kt
package b
sealed class Shape
class Circle(val radius: Int) : Shape()
fun use(shape: Shape) {
    if (shape is Circle) {
        shape.ra$0
    }
}
"#,
        2,
        expect!["radius Field"],
    );
}

/// The signatures at `$0`, the active one marked `>` and its active
/// parameter in brackets.
fn check_signature_help(fixture: &str, expected: Expect) {
    let fixture = Fixture::with_mini_stdlib(fixture);
    let (file, offset) = fixture.cursor();
    let Some(help) = analysis(&fixture).signature_help(file, offset).unwrap() else {
        expected.assert_eq("none");
        return;
    };
    let rendered: Vec<String> = help
        .signatures
        .iter()
        .enumerate()
        .map(|(index, signature)| {
            let mut label = signature.label.clone();
            if index == help.active_signature
                && let Some((start, end)) = help
                    .active_parameter
                    .and_then(|active| signature.parameters.get(active))
            {
                label.insert(*end, ']');
                label.insert(*start, '[');
            }
            let marker = if index == help.active_signature {
                ">"
            } else {
                " "
            };
            format!("{marker} {label}")
        })
        .collect();
    expected.assert_eq(&rendered.join("\n"));
}

#[test]
fn signature_help_follows_the_argument_and_the_overloads() {
    let fixture = r#"
//- /app/b/Use.kt
package b
class Money(val amount: Long, val currency: String = "USD")
fun pay(amount: Long) {}
fun pay(amount: Long, note: String, vararg tags: String) {}
fun use() {
    CALL
}
"#;
    check_signature_help(
        &fixture.replace("CALL", "pay(1, \"x\", $0"),
        expect!["> pay(amount: Long, note: String, [vararg tags: String])"],
    );
    check_signature_help(
        &fixture.replace("CALL", "Money(currency = $0)"),
        expect!["> Money(amount: Long, [currency: String = …])"],
    );
    // Commas in characters, strings and comparisons don't separate
    // arguments — with the `)` typed and without.
    check_signature_help(
        &fixture.replace("CALL", "pay(1 < 2, ',', $0)"),
        expect![[r#"
              pay(amount: Long)
            > pay(amount: Long, note: String, [vararg tags: String])"#]],
    );
    check_signature_help(
        &fixture.replace("CALL", "pay(1 < 2, \"a,b\", $0"),
        expect![[r#"
              pay(amount: Long)
            > pay(amount: Long, note: String, [vararg tags: String])"#]],
    );
    check_signature_help(
        &fixture.replace("CALL", "pay($0)"),
        expect![[r#"
        > pay([amount: Long])
          pay(amount: Long, note: String, vararg tags: String)"#]],
    );
}

#[test]
fn an_expect_and_its_actuals_are_one_declaration_for_references_rename_and_implementation() {
    // The overload taking a region, an ordinary namesake and another
    // module's `actual` must stay out.
    let fixture = r#"
//- /shared/common/a/Store.kt module=:shared set=commonMain
package a
expect fun storeUrl(): String?
expect fun storeUrl(region: Int): String?
//- /shared/ios/a/Store.ios.kt module=:shared set=iosMain deps=:shared/commonMain
package a
actual fun storeUrl(): String? = null
actual fun storeUrl(region: Int): String? = null
//- /shared/jvm/a/Store.jvm.kt module=:shared set=jvmMain deps=:shared/commonMain
package a
actual fun storeUrl(): String? = null
actual fun storeUrl(region: Int): String? = null
//- /shared/jvm/c/Store.kt module=:shared set=jvmMain deps=:shared/commonMain
package c
fun storeUrl(): String? = null
//- /other/a/Store.kt module=:other
package a
actual fun storeUrl(): String? = null
//- /shared/common/b/Use.kt module=:shared set=commonMain
package b
import a.storeUrl
fun use() = storeUrl()
"#;
    let marked =
        Fixture::parse(&fixture.replace("fun use() = storeUrl()", "fun use() = store$0Url()"));
    let (file, offset) = marked.cursor();
    let lines = |ranges: Vec<(FileId, u32)>| {
        let mut rendered: Vec<String> = ranges
            .iter()
            .map(|(file, start)| {
                let text = marked.vfs.contents(*file).unwrap();
                let line = text[..*start as usize].matches('\n').count() + 1;
                format!("{}:{line}", marked.vfs.path(*file).display())
            })
            .collect();
        rendered.sort();
        rendered.join("\n")
    };

    let implementations = analysis(&marked).goto_implementation(file, offset).unwrap();
    expect![[r#"
        /shared/ios/a/Store.ios.kt:2
        /shared/jvm/a/Store.jvm.kt:2"#]]
    .assert_eq(&lines(
        implementations
            .iter()
            .filter_map(|target| match target.file {
                crate::NavigationFile::Workspace(file) => Some((file, target.focus_range.start)),
                crate::NavigationFile::Library { .. } => None,
            })
            .collect(),
    ));

    let references = analysis(&marked).references(file, offset, true).unwrap();
    expect![[r#"
        /shared/common/a/Store.kt:2
        /shared/common/b/Use.kt:2
        /shared/common/b/Use.kt:3
        /shared/ios/a/Store.ios.kt:2
        /shared/jvm/a/Store.jvm.kt:2"#]]
    .assert_eq(&lines(
        references
            .iter()
            .map(|found| (found.file, found.range.start))
            .collect(),
    ));

    let change = analysis(&marked)
        .rename(file, offset, "storeLink")
        .unwrap()
        .unwrap();
    let renamed: Vec<(FileId, u32)> = change
        .edits
        .iter()
        .flat_map(|(file, edits)| edits.iter().map(|edit| (*file, edit.range.start)))
        .collect();
    expect![[r#"
        /shared/common/a/Store.kt:2
        /shared/common/b/Use.kt:2
        /shared/common/b/Use.kt:3
        /shared/ios/a/Store.ios.kt:2
        /shared/jvm/a/Store.jvm.kt:2"#]]
    .assert_eq(&lines(renamed));
}

#[test]
fn a_constructor_property_is_one_symbol_from_its_declaration_and_every_use() {
    let fixture = r#"
//- /app/a/Switches.kt
package a
class Switches(
    val enabled: Boolean,
    val fallback: Boolean = enabled,
) {
    val shown = enabled
    fun check(): Boolean = enabled
}
class Other(val enabled: Boolean)
fun plain(enabled: Boolean) = enabled
//- /app/b/Use.kt
package b
import a.Switches
fun use(switches: Switches, other: a.Other): Boolean {
    val made = Switches(enabled = true)
    return switches.enabled && other.enabled && made.enabled
}
"#;
    // Each cursor is the `nth` `enabled` of the file; all mean one symbol.
    let cursors = [
        ("/app/a/Switches.kt", 1),
        ("/app/a/Switches.kt", 3),
        ("/app/a/Switches.kt", 4),
        ("/app/b/Use.kt", 2),
    ];
    for (path, nth) in cursors {
        let marked = Fixture::with_mini_stdlib(&mark_nth(fixture, path, "enabled", nth));
        let (file, offset) = marked.cursor();
        let found = analysis(&marked).references(file, offset, true).unwrap();
        let mut lines: Vec<String> = found
            .iter()
            .map(|range| {
                let text = marked.vfs.contents(range.file).unwrap();
                let line = text[..range.range.start as usize].matches('\n').count() + 1;
                format!("{}:{line}", marked.vfs.path(range.file).display())
            })
            .collect();
        lines.sort();
        expect![[r#"
            /app/a/Switches.kt:3
            /app/a/Switches.kt:4
            /app/a/Switches.kt:6
            /app/a/Switches.kt:7
            /app/b/Use.kt:4
            /app/b/Use.kt:5
            /app/b/Use.kt:5"#]]
        .assert_eq(&lines.join("\n"));
    }
}

/// `fixture` with `$0` before the `nth` (1-based) occurrence of `word` in
/// the file at `path`.
fn mark_nth(fixture: &str, path: &str, word: &str, nth: usize) -> String {
    let file_start = fixture.find(&format!("//- {path}\n")).unwrap();
    let mut position = file_start;
    for _ in 0..nth {
        position = fixture[position..].find(word).unwrap() + position + 1;
    }
    let at = position - 1;
    format!("{}$0{}", &fixture[..at], &fixture[at..])
}

#[test]
fn references_skip_a_namesake_and_rename_edits_every_file() {
    let fixture = r#"
//- /app/a/Money.kt
package a
class Money(val amount: Long) {
    fun add(other: Money): Money = Money(amount + other.amount)
}
//- /app/c/Money.kt
package c
class Money
//- /app/b/Use.kt
package b
import a.Money
fun use(money: Money, other: c.Money): Long {
    val total = money.add(money).amount
    return total + total
}
"#;
    let render = |fixture: &Fixture, ranges: Vec<crate::FileRange>| {
        ranges
            .iter()
            .map(|range| {
                let text = fixture.vfs.contents(range.file).unwrap();
                let line = text[..range.range.start as usize].matches('\n').count() + 1;
                format!("{}:{line}", fixture.vfs.path(range.file).display())
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let marked =
        Fixture::with_mini_stdlib(&fixture.replace("class Money(val", "class Mo$0ney(val"));
    let (file, offset) = marked.cursor();
    let found = analysis(&marked).references(file, offset, true).unwrap();
    expect![[r#"
        /app/a/Money.kt:2
        /app/a/Money.kt:3
        /app/a/Money.kt:3
        /app/a/Money.kt:3
        /app/b/Use.kt:2
        /app/b/Use.kt:3"#]]
    .assert_eq(&render(&marked, found));

    let local = Fixture::with_mini_stdlib(&fixture.replace("val total", "val to$0tal"));
    let (file, offset) = local.cursor();
    let change = analysis(&local)
        .rename(file, offset, "sum")
        .unwrap()
        .unwrap();
    let edits: usize = change.edits.iter().map(|(_, edits)| edits.len()).sum();
    assert_eq!(edits, 3, "{change:?}");
    assert_eq!(
        analysis(&local).rename(file, offset, "fun").unwrap(),
        Err(crate::RenameError::InvalidName("fun".to_owned()))
    );

    let java = Fixture::with_mini_stdlib(
        r#"
//- /app/a/Clock.java
package a;
public class Clock { public static long now() { return 0; } }
//- /app/b/Use.kt
package b
fun use() = a.Clock.no$0w()
"#,
    );
    let (file, offset) = java.cursor();
    assert_eq!(
        analysis(&java).rename(file, offset, "later").unwrap(),
        Err(crate::RenameError::ReadOnly)
    );
    assert!(matches!(
        analysis(&java).rename(file, offset, "fun").unwrap(),
        Err(crate::RenameError::ReadOnly)
    ));

    let backing_field = Fixture::with_mini_stdlib(
        r#"
//- /app/b/Use.kt
package b
class Account {
    var balance: Long = 0
        set(value) { fie$0ld = value }
}
"#,
    );
    let (file, offset) = backing_field.cursor();
    assert_eq!(
        analysis(&backing_field)
            .rename(file, offset, "stored")
            .unwrap(),
        Err(crate::RenameError::NothingToRename)
    );
}

#[test]
fn renaming_a_member_renames_its_overrides_and_what_it_overrides() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/a/Payments.kt
package a
interface Payment { fun settle() }
open class Card : Payment {
    override fun settle() {}
    fun settle(times: Int) {}
}
class Gold : Card() { override fun settle() {} }
class Cash : Payment { override fun sett$0le() {} }
//- /app/b/Use.kt
package b
import a.Card
import a.Payment
fun use(payment: Payment, card: Card) {
    payment.settle()
    card.settle()
    card.settle(2)
}
"#,
    );
    let (file, offset) = fixture.cursor();
    let change = analysis(&fixture)
        .rename(file, offset, "complete")
        .unwrap()
        .unwrap();
    let mut renamed: Vec<String> = Vec::new();
    for (file, edits) in &change.edits {
        let text = fixture.vfs.contents(*file).unwrap();
        for edit in edits {
            let line = text[..edit.range.start as usize].matches('\n').count() + 1;
            renamed.push(format!("{}:{line}", fixture.vfs.path(*file).display()));
        }
    }
    renamed.sort();
    // Every declaration and call of the parameterless `settle`; not the
    // overload taking `times`.
    expect![[r#"
        /app/a/Payments.kt:2
        /app/a/Payments.kt:4
        /app/a/Payments.kt:7
        /app/a/Payments.kt:8
        /app/b/Use.kt:5
        /app/b/Use.kt:6"#]]
    .assert_eq(&renamed.join("\n"));
}

#[test]
fn semantic_tokens_tell_properties_locals_parameters_and_classes_apart() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/b/Use.kt
package b
const val LIMIT = 10
class Account(val owner: String) {
    companion object { val ZERO = 0 }
    var balance: Long = 0
        set(value) { field = value }
    val twice: (Int) -> Int = { it }
    fun describe(prefix: String): String {
        val label = prefix + owner
        return label
    }
}
"#,
    );
    expect![[r#"
        LIMIT Property declaration,readonly,static
        Account Class declaration
        owner Property declaration,readonly
        String Class 
        ZERO Property declaration,readonly,static
        balance Property declaration,mutable
        Long Class 
        value Parameter declaration,readonly
        field Variable mutable
        value Parameter readonly
        twice Property declaration,readonly
        Int Class 
        Int Class 
        it Parameter readonly
        describe Method declaration
        prefix Parameter declaration,readonly
        String Class 
        String Class 
        label Variable declaration,readonly
        prefix Parameter readonly
        owner Property readonly
        label Variable readonly"#]]
    .assert_eq(&rendered_tokens(&fixture, "/app/b/Use.kt"));
}

#[test]
fn semantic_tokens_mark_annotation_names_as_decorators() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/a/Marks.kt
package a
annotation class Enable(val value: Any)
annotation class Plain
//- /app/b/Use.kt
@file:a.Plain
package b
import a.Enable
import a.Plain
@Plain
@Enable(Enable::class)
class Config(@param:Enable(Plain::class) val limit: Int) {
    @a.Plain fun run(): Enable? = null
}
"#,
    );
    expect![[r#"
        Plain Decorator 
        Plain Decorator 
        Enable Decorator 
        Enable Interface 
        Config Class declaration
        Enable Decorator 
        Plain Interface 
        limit Property declaration,readonly
        Int Class 
        Plain Decorator 
        run Method declaration
        Enable Interface "#]]
    .assert_eq(&rendered_tokens(&fixture, "/app/b/Use.kt"));
}

/// `text kind modifiers` per semantic token of the file at `path`.
fn rendered_tokens(fixture: &Fixture, path: &str) -> String {
    let file = fixture.file(path);
    let text = fixture.vfs.contents(file).unwrap().clone();
    let tokens = analysis(fixture).semantic_tokens(file).unwrap();
    let rendered: Vec<String> = tokens
        .iter()
        .map(|token| {
            let names: Vec<&str> = crate::TokenModifiers::NAMES
                .iter()
                .enumerate()
                .filter(|(bit, _)| token.modifiers.0 & (1 << bit) != 0)
                .map(|(_, name)| *name)
                .collect();
            format!(
                "{} {:?} {}",
                &text[token.range.start as usize..token.range.end as usize],
                token.kind,
                names.join(",")
            )
        })
        .collect();
    rendered.join("\n")
}

#[test]
fn folding_covers_multi_line_bodies_imports_and_comments_once() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/b/Use.kt
package b

import kotlin.collections.List
import kotlin.collections.Map

/*
 * About this file.
 */
class Account {
    fun one() = 1
    fun describe(): String {
        return "x"
    }
}
"#,
    );
    let file = fixture.file("/app/b/Use.kt");
    let text = fixture.vfs.contents(file).unwrap().clone();
    let line = |offset: u32| text[..offset as usize].matches('\n').count() + 1;
    let folds: Vec<String> = analysis(&fixture)
        .folding_ranges(file)
        .unwrap()
        .iter()
        .map(|fold| {
            format!(
                "{:?} {}-{}",
                fold.kind,
                line(fold.range.start),
                line(fold.range.end)
            )
        })
        .collect();
    expect![[r#"
        Imports 3-4
        Comment 6-8
        Region 9-14
        Region 11-13"#]]
    .assert_eq(&folds.join("\n"));
}

#[test]
fn a_non_exhaustive_when_gets_its_missing_branches_added() {
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b

enum class Side { BUY, SELL, HOLD }
fun use(side: Side): Int {
    return when (side) {
        Side.BUY -> 1
    }
}
fun inline(side: Side) = when (side) { Side.BUY -> 1; Side.SELL -> 2 }
"#,
        crate::DiagnosticsConfig::default(),
        expect![[r#"
            [non-exhaustive-when] 5: `when` must be exhaustive: add the missing `Side.SELL`, `Side.HOLD` or an `else` branch | Add the missing branches: "        Side.SELL -> TODO()\n        Side.HOLD -> TODO()\n"
            [non-exhaustive-when] 9: `when` must be exhaustive: add the missing `Side.HOLD` or an `else` branch | Add the missing branches: "; Side.HOLD -> TODO()""#]],
    );
}
