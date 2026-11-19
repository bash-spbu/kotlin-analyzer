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
fn inlay_hints_show_inferred_types_but_not_written_or_constructed_ones() {
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
    names.map { name -> name.length }
    names.map { name: String -> name }
}
fun lengths() = listOf("a").map { it.length }
fun written(): Int = 1
interface Shape
class Circle : Shape
class Square : Shape
fun shapes(flag: Boolean) {
    val both = listOf(Circle(), Square())
    val mixed = if (flag) listOf("a") else listOf(1)
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
            format!("{}{}", &text[name_start..hint.offset as usize], hint.text())
        })
        .collect();
    expect![[r#"
        names: List<String>
        count: Int
        number: Int
        word: String
        it: String
        name: String
        : List<Int>
        it: String
        both: List<Shape>
        mixed: List<*>"#]]
    .assert_eq(&rendered.join("\n"));
}

#[test]
fn each_class_name_in_an_inlay_hint_links_to_its_declaration() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/a/Use.kt
package a
class Box<T>(val content: T)
class Shape
fun use(shapes: List<Shape>) {
    val boxes = shapes.map { Box(it) }
    val first = boxes.first()
}
"#,
    );
    let file = fixture.file("/app/a/Use.kt");
    let analysis = analysis(&fixture);
    let hints = analysis.inlay_hints(file, None).unwrap();
    let rendered: Vec<String> = hints
        .iter()
        .map(|hint| {
            let parts: String = hint
                .label
                .iter()
                .map(|part| match &part.target {
                    Some(crate::HintTarget::Declaration(target)) => {
                        format!("[{}@{}]", part.text, target.name)
                    }
                    Some(crate::HintTarget::LibraryDeclaration) => {
                        format!("[{}@library]", part.text)
                    }
                    None => part.text.clone(),
                })
                .collect();
            let resolved: Vec<String> = analysis
                .resolve_inlay_hint(file, hint.offset)
                .unwrap()
                .into_iter()
                .map(|target| target.map_or("-".to_owned(), |target| target.name))
                .collect();
            format!("{parts}  resolves {}", resolved.join(" "))
        })
        .collect();
    expect![[r#"
        : [List@List]<[Box@Box]<[Shape@Shape]>>  resolves - List - Box - Shape -
        it: [Shape@Shape]  resolves - Shape
        : [Box@Box]<[Shape@Shape]>  resolves - Box - Shape -"#]]
    .assert_eq(&rendered.join("\n"));
}

/// The workspace's files after `moves` (by path, with the new stem and
/// directory package) apply: each changed file, as `path` and its text.
fn files_after_rename(fixture: &str, moves: &[(&str, &str, &str)]) -> String {
    let fixture = Fixture::with_mini_stdlib(fixture);
    let moves: Vec<crate::FileMove> = moves
        .iter()
        .map(|(path, new_stem, new_package)| {
            let old_stem = std::path::Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            crate::FileMove {
                file: fixture.file(path),
                old_stem,
                new_stem: (*new_stem).to_owned(),
                new_directory_package: Some(intern::Path::parse(new_package)),
            }
        })
        .collect();
    let change = analysis(&fixture).will_rename_files(&moves).unwrap();
    let mut rendered = Vec::new();
    for (file, edits) in change.edits {
        let mut text = fixture.vfs.contents(file).unwrap().to_string();
        for edit in edits.iter().rev() {
            text.replace_range(
                edit.range.start as usize..edit.range.end as usize,
                &edit.new_text,
            );
        }
        rendered.push(format!("{}:\n{text}", fixture.vfs.path(file).display()));
    }
    rendered.sort();
    rendered.join("\n")
}

#[test]
fn renaming_a_file_renames_the_class_it_is_named_after() {
    expect![[r#"
        /app/a/Order.kt:
        package a
        class Invoice(val total: Int)
        fun helper() = Invoice(1)

        /app/a/Use.kt:
        package a
        fun use(order: Invoice) = Invoice(order.total)
    "#]]
    .assert_eq(&files_after_rename(
        r#"
//- /app/a/Order.kt
package a
class Order(val total: Int)
fun helper() = Order(1)
//- /app/a/Use.kt
package a
fun use(order: Order) = Order(order.total)
"#,
        &[("/app/a/Order.kt", "Invoice", "a")],
    ));
}

#[test]
fn moving_a_file_moves_its_package_and_its_uses_follow() {
    expect![[r#"
        /app/a/Order.kt:
        package b

        import a.Helper
        class Order(val helper: Helper)

        /app/a/Same.kt:
        package a

        import b.Order
        fun same() = Order(Helper())

        /app/c/Imported.kt:
        package c
        import b.Order
        fun imported(order: Order) = order

        /app/d/Qualified.kt:
        package d
        fun qualified(order: b.Order) = order

        /app/e/Star.kt:
        package e
        import a.*
        import b.Order
        fun star(order: Order) = order
    "#]]
    .assert_eq(&files_after_rename(
        r#"
//- /app/a/Order.kt root=/app
package a
class Order(val helper: Helper)
//- /app/a/Helper.kt root=/app
package a
class Helper
//- /app/a/Same.kt root=/app
package a
fun same() = Order(Helper())
//- /app/c/Imported.kt root=/app
package c
import a.Order
fun imported(order: Order) = order
//- /app/d/Qualified.kt root=/app
package d
fun qualified(order: a.Order) = order
//- /app/e/Star.kt root=/app
package e
import a.*
fun star(order: Order) = order
"#,
        &[("/app/a/Order.kt", "Order", "b")],
    ));
}

#[test]
fn files_moved_together_need_no_imports_of_each_other() {
    expect![[r#"
        /app/a/Helper.kt:
        package b
        class Helper

        /app/a/Order.kt:
        package b
        class Order(val helper: Helper)

        /app/a/Same.kt:
        package a

        import b.Helper
        import b.Order
        fun same() = Order(Helper())
    "#]]
    .assert_eq(&files_after_rename(
        r#"
//- /app/a/Order.kt root=/app
package a
class Order(val helper: Helper)
//- /app/a/Helper.kt root=/app
package a
class Helper
//- /app/a/Same.kt root=/app
package a
fun same() = Order(Helper())
"#,
        &[
            ("/app/a/Order.kt", "Order", "b"),
            ("/app/a/Helper.kt", "Helper", "b"),
        ],
    ));
}

#[test]
fn a_file_whose_package_isnt_its_directorys_keeps_it_and_a_rename_goes_with_a_move() {
    expect![[r#"
        /app/a/Order.kt:
        package b
        class Invoice

        /app/a/Same.kt:
        package a

        import b.Invoice
        fun same() = Invoice()

        /app/c/Use.kt:
        package c
        import b.Invoice
        import somewhere.else.Elsewhere
        fun use(order: Invoice, elsewhere: Elsewhere) = order
    "#]]
    .assert_eq(&files_after_rename(
        r#"
//- /app/a/Elsewhere.kt root=/app
package somewhere.else
class Elsewhere
//- /app/a/Order.kt root=/app
package a
class Order
//- /app/c/Use.kt root=/app
package c
import a.Order
import somewhere.else.Elsewhere
fun use(order: Order, elsewhere: Elsewhere) = order
//- /app/a/Same.kt root=/app
package a
fun same() = Order()
"#,
        &[
            ("/app/a/Elsewhere.kt", "Elsewhere", "b"),
            ("/app/a/Order.kt", "Invoice", "b"),
        ],
    ));
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
//- /app/b/Use.kt lang=2.3
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
            [unnecessary-safe-call] 12: Unnecessary safe call on a receiver of non-null type `String` | Replace `?.` with `.`: "."
            [senseless-comparison] 13: Condition `account.nickname != null` is always `true`: `String` isn't nullable
            [unnecessary-not-null-assertion] 21: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: ""
            [unnecessary-safe-call] 22: Unnecessary safe call on a receiver of non-null type `String` | Replace `?.` with `.`: "."
            [unnecessary-not-null-assertion] 24: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: ""
            [unnecessary-not-null-assertion] 33: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: ""
            [senseless-comparison] 35: Condition `right == null` is always `false`: `String` isn't nullable
            [unnecessary-not-null-assertion] 41: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: """#]],
    );
}

#[test]
fn before_kotlin_2_3_only_comparisons_are_reported() {
    // `!!`, `?.` and `?:` on a value that can't be null are the compiler's
    // data-flow warnings, which it reports from 2.3 on; a comparison with
    // `null` it reports always, after smart casts too.
    let fixture = |version: &str| {
        format!(
            r#"
//- /app/b/Use.kt lang={version}
package b
fun use(name: String, maybe: String?) {{
    name!!.length
    name?.length
    val other = name ?: ""
    if (name == null) return
    if (maybe != null) {{
        if (maybe == null) println(other)
    }}
}}
"#
        )
    };
    check_diagnostics(
        &fixture("2.2"),
        new_warnings(),
        expect![[r#"
            [senseless-comparison] 6: Condition `name == null` is always `false`: `String` isn't nullable
            [senseless-comparison] 8: Condition `maybe == null` is always `false`: `String` isn't nullable"#]],
    );
    check_diagnostics(
        &fixture("2.3"),
        new_warnings(),
        expect![[r#"
            [unnecessary-not-null-assertion] 3: Unnecessary non-null assertion (`!!`) on a value of non-null type `String` | Remove `!!`: ""
            [unnecessary-safe-call] 4: Unnecessary safe call on a receiver of non-null type `String` | Replace `?.` with `.`: "."
            [useless-elvis] 5: Elvis operator (`?:`) always returns the left operand of non-null type `String` | Remove the elvis operator: ""
            [senseless-comparison] 6: Condition `name == null` is always `false`: `String` isn't nullable
            [senseless-comparison] 8: Condition `maybe == null` is always `false`: `String` isn't nullable"#]],
    );
}

#[test]
fn uses_of_deprecated_declarations() {
    check_diagnostics(
        r#"
//- /app/a/Legacy.java
package a;
public class Legacy {
    @Deprecated public void stop() {}
    /** @deprecated use {@link #name()} */
    public String getTitle() { return ""; }
    public String name() { return ""; }
}
//- /app/a/Api.kt
package a
@Deprecated("Use Fresh", ReplaceWith("Fresh"))
class Old
class Fresh
@Deprecated("Gone", level = DeprecationLevel.ERROR)
fun removed() {}
@Deprecated("Use format(Int)")
fun format(text: String) = text
fun format(number: Int) = number.toString()
@Deprecated("Use Fresh")
typealias OldAlias = Fresh
open class Base {
    @Deprecated("Use run") open fun start() {}
    open fun run() {}
}
class Derived : Base() {
    override fun start() {}
}
open class JavaBase : Legacy() {
    override fun stop() {}
}
class Settings(@Deprecated("Use size") var count: Int = 0, val size: Int = 0)
class Span(val weight: Int) {
    @Deprecated("Old", level = DeprecationLevel.HIDDEN)
    constructor(weight: Int, style: String) : this(weight)
}
//- /app/b/Use.kt
package b
import a.Old
import a.format
import a.Derived
import a.JavaBase
import a.Legacy
import a.OldAlias
import a.Settings
import a.Span
import a.removed
fun use(legacy: Legacy, derived: Derived, old: Old): OldAlias? {
    legacy.stop()
    legacy.title
    legacy.name()
    derived.start()
    JavaBase().stop()
    Old()
    format("x")
    format(1)
    removed()
    val settings = Settings(count = 1)
    settings.count = 2
    println(settings.count)
    Span(1)
    return null
}
@Suppress("DEPRECATION")
fun quiet(legacy: Legacy) = legacy.stop()
class Quiet {
    @Suppress("DEPRECATION_ERROR")
    val gone: Unit
        get() = removed()
}
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::Deprecation, crate::Check::DeprecationError],
            ..crate::DiagnosticsConfig::default()
        },
        expect![[r#"
            [deprecation] 2: `Old` is deprecated. Use Fresh. Replace with `Fresh`.
            [deprecation] 7: `OldAlias` is deprecated. Use Fresh.
            [deprecation-error] 10: `removed` is deprecated. Gone.
            [deprecation] 11: `Old` is deprecated. Use Fresh. Replace with `Fresh`.
            [deprecation] 11: `OldAlias` is deprecated. Use Fresh.
            [deprecation] 12: `stop` is deprecated.
            [deprecation] 13: `getTitle` is deprecated.
            [deprecation] 15: `start` is deprecated. Use run.
            [deprecation] 17: `Old` is deprecated. Use Fresh. Replace with `Fresh`.
            [deprecation-error] 20: `removed` is deprecated. Gone.
            [deprecation] 23: `count` is deprecated. Use size."#]],
    );
}

#[test]
fn a_jdk_deprecation_counts_only_for_the_release_the_build_declares() {
    // A ct.sym of one class: `L/java.base/demo/Clock.sig`, compiled by javac.
    let directory = std::env::temp_dir().join(format!("ka-jdk-deprecation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let sources = directory.join("sources/demo");
    std::fs::create_dir_all(&sources).unwrap();
    std::fs::write(
        sources.join("Clock.java"),
        "package demo; public class Clock { @Deprecated public void tick() {} public void tock() {} }",
    )
    .unwrap();
    let classes = directory.join("classes");
    let compiled = std::process::Command::new("javac")
        .arg("-d")
        .arg(&classes)
        .arg(sources.join("Clock.java"))
        .status();
    if !compiled.is_ok_and(|status| status.success()) {
        eprintln!("skipped: javac unavailable");
        return;
    }
    let symbols = directory.join("symbols/L/java.base/demo");
    std::fs::create_dir_all(&symbols).unwrap();
    std::fs::copy(classes.join("demo/Clock.class"), symbols.join("Clock.sig")).unwrap();
    let archive = directory.join("ct.sym");
    let zipped = std::process::Command::new("jar")
        .arg("cfM")
        .arg(&archive)
        .arg("-C")
        .arg(directory.join("symbols"))
        .arg("L")
        .status();
    if !zipped.is_ok_and(|status| status.success()) {
        eprintln!("skipped: jar unavailable");
        return;
    }
    let fixture = r#"
//- /app/b/Use.kt
package b
import demo.Clock
fun use(clock: Clock) {
    clock.tick()
    clock.tock()
}
"#;
    let diagnostics = |known: base_db::JdkRelease| {
        let mut marked = Fixture::with_mini_stdlib(fixture);
        let jdk = base_db::LibrarySource {
            archive: archive.clone(),
            kind: base_db::ArchiveKind::JdkSymbols {
                release: 'L',
                known,
            },
            sources: None,
        };
        let indexed = hir::index_library(&jdk).unwrap();
        // The JDK only, no classpath for any source set.
        marked
            .database
            .set_libraries(vec![(jdk, indexed)], Vec::new(), Some(0));
        let file = marked.file("/app/b/Use.kt");
        let config = crate::DiagnosticsConfig {
            only: vec![crate::Check::Deprecation],
            ..crate::DiagnosticsConfig::default()
        };
        analysis(&marked)
            .diagnostics(file, &config)
            .unwrap()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        diagnostics(base_db::JdkRelease::Declared),
        vec!["`tick` is deprecated."]
    );
    assert!(diagnostics(base_db::JdkRelease::RunningJdk).is_empty());
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn deprecated_since_kotlin_follows_the_api_version_and_werror_makes_warnings_errors() {
    let Some(jar) = newest_cached_stdlib() else {
        eprintln!("skipped: no kotlin-stdlib in the Gradle cache");
        return;
    };
    // `Iterable.sumBy`: `@DeprecatedSinceKotlin(warningSince = "1.5")`.
    let diagnostics = |header: &str| {
        let text = format!(
            "\n//- /app/b/Use.kt lang=2.3 {header}\npackage b\nfun total(numbers: List<Int>, name: String) = numbers.sumBy {{ it }} + name?.length!!\n"
        );
        let mut fixture = Fixture::parse(&text);
        let source = base_db::LibrarySource {
            archive: jar.clone(),
            kind: base_db::ArchiveKind::Jar,
            sources: None,
        };
        let indexed = hir::index_library(&source).unwrap();
        fixture
            .database
            .set_libraries(vec![(source, indexed)], vec![vec![0]], None);
        let file = fixture.file("/app/b/Use.kt");
        let config = crate::DiagnosticsConfig {
            only: vec![crate::Check::Deprecation, crate::Check::UnnecessarySafeCall],
            ..crate::DiagnosticsConfig::default()
        };
        analysis(&fixture)
            .diagnostics(file, &config)
            .unwrap()
            .iter()
            .map(|diagnostic| format!("{:?} {:?}", diagnostic.check, diagnostic.severity))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // Before 1.5 it isn't deprecated; from it on, it is; unknown, nothing.
    assert_eq!(diagnostics("api=1.4"), "UnnecessarySafeCall Lint");
    assert_eq!(
        diagnostics("api=2.0"),
        "Deprecation Lint, UnnecessarySafeCall Lint"
    );
    assert_eq!(diagnostics(""), "UnnecessarySafeCall Lint");
    // With warnings as errors, both are errors.
    assert_eq!(
        diagnostics("api=2.0 werror"),
        "Deprecation Error, UnnecessarySafeCall Error"
    );
}

#[test]
fn a_library_class_in_an_inlay_hint_is_linked_when_resolved() {
    let Some(jar) = newest_cached_stdlib() else {
        eprintln!("skipped: no kotlin-stdlib in the Gradle cache");
        return;
    };
    let mut fixture = Fixture::parse(
        "\n//- /app/b/Use.kt\npackage b\nfun use() {\n    val names = listOf(\"a\")\n}\n",
    );
    let source = base_db::LibrarySource {
        archive: jar.clone(),
        kind: base_db::ArchiveKind::Jar,
        sources: None,
    };
    let indexed = hir::index_library(&source).unwrap();
    fixture
        .database
        .set_libraries(vec![(source, indexed)], vec![vec![0]], None);
    let file = fixture.file("/app/b/Use.kt");
    let hints = analysis(&fixture).inlay_hints(file, None).unwrap();
    let [hint] = &hints[..] else {
        panic!("one hint: {hints:?}");
    };
    let library_parts: Vec<&str> = hint
        .label
        .iter()
        .filter(|part| part.target == Some(crate::HintTarget::LibraryDeclaration))
        .map(|part| part.text.as_str())
        .collect();
    assert_eq!(library_parts, ["List", "String"]);
    // Without a sources archive there's nothing to open: resolving gives
    // no target, and nothing fails.
    let resolved = analysis(&fixture)
        .resolve_inlay_hint(file, hint.offset)
        .unwrap();
    assert_eq!(resolved.len(), hint.label.len());
}

/// The newest `kotlin-stdlib` JAR in the Gradle cache.
fn newest_cached_stdlib() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let root = std::path::Path::new(&home)
        .join(".gradle/caches/modules-2/files-2.1/org.jetbrains.kotlin/kotlin-stdlib");
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
fn casts_and_type_checks_whose_outcome_is_known() {
    let config = || crate::DiagnosticsConfig {
        only: vec![
            crate::Check::UselessCast,
            crate::Check::CastNeverSucceeds,
            crate::Check::UncheckedCast,
            crate::Check::UselessIsCheck,
        ],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        r#"
//- /app/b/Use.kt lang=2.3
package b
open class Animal
class Dog : Animal()
class Car
interface Pet
annotation class Component
@Component class Service
fun casts(animal: Animal, dog: Dog, any: Any, text: String?, list: List<String>, car: Car, pet: Pet, service: Service) {
    dog as Dog
    text as? String
    animal as Pet
    animal as Dog
    dog as Car
    car as Pet
    pet as Car
    service as Pet
    any as List<String>
    any as List<*>
    list as MutableList<String>
    1 as Any
}
fun <T> generic(value: Any?, own: T?, plain: T) {
    value as T
    own as T
    plain as T?
}
fun <T : Any> bounded(own: T?) = own as T
inline fun <reified R> reified(value: Any) = value as R
fun checks(dog: Dog, animal: Animal, text: String?) {
    if (dog is Animal) {}
    if (dog !is Animal) {}
    if (animal is Dog) {}
    if (text is String) {}
    if (text is String?) {}
    when (dog) {
        is Animal -> 1
        else -> 2
    }
    when (animal) {
        is Dog -> 1
        is Animal -> 2
    }
}
"#,
        config(),
        expect![[r#"
            [useless-cast] 9: No cast needed | Remove the cast: ""
            [useless-cast] 10: No cast needed | Remove the cast: ""
            [cast-never-succeeds] 13: This cast can never succeed
            [cast-never-succeeds] 14: This cast can never succeed
            [cast-never-succeeds] 15: This cast can never succeed
            [unchecked-cast] 17: Unchecked cast of `Any` to `List<String>`
            [unchecked-cast] 23: Unchecked cast of `Any?` to `T`
            [unchecked-cast] 24: Unchecked cast of `T?` to `T`
            [useless-is-check] 30: Check for instance is always `true`
            [useless-is-check] 31: Check for instance is always `false`
            [useless-is-check] 34: Check for instance is always `true`
            [useless-is-check] 36: Check for instance is always `true`"#]],
    );
    // Before Kotlin 2.3 the compiler doesn't report useless casts or casts
    // that never succeed.
    check_diagnostics(
        r#"
//- /app/b/Use.kt lang=2.2
package b
class Dog
class Car
fun casts(dog: Dog) {
    dog as Dog
    dog as Car
}
"#,
        config(),
        expect![[r#""#]],
    );
}

#[test]
fn when_branches_needless_or_twice() {
    let config = || crate::DiagnosticsConfig {
        only: vec![
            crate::Check::RedundantElseInWhen,
            crate::Check::DuplicateBranchCondition,
        ],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
enum class Side { BUY, SELL }
sealed interface Shape
class Circle : Shape
object Square : Shape
open class Animal
class Dog : Animal()
fun whens(side: Side, shape: Shape, code: Int, animal: Animal, nullable: Side?, other: Side?, flag: Boolean) {
    when (side) {
        Side.BUY -> 1
        Side.SELL -> 2
        else -> 3
    }
    when (shape) { is Circle -> 1; Square -> 2; else -> 3 }
    when (code) { 1 -> "a"; 2 -> "b"; 1 -> "c"; else -> "d" }
    when (side) { Side.BUY -> 1; Side.BUY -> 2; else -> 3 }
    when (animal) { is Dog -> 1; is  Dog -> 2; !is Dog -> 3; else -> 4 }
    when (nullable) { Side.BUY -> 1; Side.SELL -> 2; else -> 3 }
    when (other) { Side.BUY -> 1; Side.SELL -> 2; null -> 3; else -> 4 }
    when (side) { Side.BUY if flag -> 1; Side.SELL -> 2; else -> 3 }
    when (side) { Side.BUY if flag -> 1; Side.BUY -> 2; else -> 3 }
    when (code) { "$code".length -> 1; "$code".length -> 2; else -> 3 }
    when (flag) { true -> 1; false -> 2; else -> 3 }
}
"#,
        config(),
        expect![[r#"
            [redundant-else-in-when] 12: `when` is exhaustive so `else` is redundant here | Remove the `else` branch: ""
            [redundant-else-in-when] 14: `when` is exhaustive so `else` is redundant here | Remove the `else` branch: ""
            [duplicate-branch-condition] 15: Duplicate branch condition in `when`
            [duplicate-branch-condition] 16: Duplicate branch condition in `when`
            [duplicate-branch-condition] 17: Duplicate branch condition in `when`
            [redundant-else-in-when] 19: `when` is exhaustive so `else` is redundant here | Remove the `else` branch: ""
            [redundant-else-in-when] 23: `when` is exhaustive so `else` is redundant here | Remove the `else` branch: """#]],
    );
    // A multiplatform `jvmMain` may add subclasses to `commonMain`'s sealed
    // class: its `else` may be needed there.
    check_diagnostics(
        r#"
//- /app/b/Use.kt set=commonMain
package b
sealed interface Shape
class Circle : Shape
fun whens(shape: Shape) = when (shape) { is Circle -> 1; else -> 2 }
//- /app/jvm/b/Square.kt set=jvmMain deps=:app/commonMain
package b
class Square : Shape
"#,
        config(),
        expect![[r#""#]],
    );
}

#[test]
fn values_whose_class_cannot_be_the_one_required() {
    let config = || crate::DiagnosticsConfig {
        only: vec![crate::Check::ReturnTypeMismatch, crate::Check::TypeMismatch],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
open class Animal
class Dog : Animal()
class Car
class Result(val dog: Dog) { fun get(): Dog = dog }
fun wrong(result: Result): Dog {
    return result
}
fun wrongBody(result: Result): Dog = result
fun right(result: Result): Dog {
    return result.get()
}
fun subtype(dog: Dog): Animal = dog
fun nullable(dog: Dog?): Dog {
    return dog
}
fun literal(): Long = 1
fun branches(flag: Boolean, dog: Dog, car: Car): Animal = if (flag) dog else Animal()
fun inLambda(dogs: List<Dog>): Car {
    dogs.forEach { return Car() }
    run { return@run dogs }
    return Car()
}
fun anonymous(car: Car): Dog {
    val make = fun(): Car { return car }
    return Dog()
}
fun throws(): Dog = throw IllegalStateException()
fun unknown(): Dog = missing()
fun local(car: Car) {
    fun inner(): Dog = car
}
fun chain(result: Result): Car = result
    .get()
class Holder(var pet: Animal) {
    val count: Int = "many"
    var name: String = "x"
    fun rename(car: Car, any: Any) {
        name = car
        pet = Dog()
        var animal: Animal = Dog()
        if (any is Dog) { animal = any }
        var guessed = Dog()
        guessed = Dog()
        animal = car
    }
}
fun defaults(count: Int = "one", pet: Animal = Dog()) {}
fun feed(dog: Dog) {}
fun <T> keep(value: T): T = value
fun walk(dog: Dog) {}
fun walk(car: Car) {}
fun all(vararg dogs: Dog) {}
fun calls(car: Car, dog: Dog?) {
    feed(car)
    feed(Dog())
    keep(car)
    walk(car)
    all(Dog(), Dog())
    feed(dog)
}
fun ints(values: IntArray) {}
fun spread(vararg numbers: Int) {
    ints(numbers)
}
class Response
fun body(response: Response): String = ""
fun <T> body(response: Response, marker: Int = 0): T = TODO()
fun typed(response: Response): Dog = body<Dog>(response)
class Success(val amount: Int)
class Outcome { fun getOrThrow(): Success = TODO() }
class Exchange { fun convert(amount: Int, code: String): Outcome = TODO() }
class Settings<T>(val value: T) { fun get(): T = value }
class Converter(private val exchange: Exchange, private val settings: Settings<String>) {
    fun convert(amount: Int): Int {
        return exchange.convert(
            amount = amount,
            code = settings.get(),
        ).getOrThrow()
    }
}
"#,
        config(),
        expect![[r#"
            [return-type-mismatch] 7: Return type mismatch: expected `Dog`, actual `Result`
            [return-type-mismatch] 9: Return type mismatch: expected `Dog`, actual `Result`
            [return-type-mismatch] 15: Return type mismatch: expected `Dog`, actual `Dog?`
            [return-type-mismatch] 31: Return type mismatch: expected `Dog`, actual `Car`
            [return-type-mismatch] 34: Return type mismatch: expected `Car`, actual `Dog`
            [type-mismatch] 36: Initializer type mismatch: expected `Int`, actual `String`
            [type-mismatch] 39: Assignment type mismatch: expected `String`, actual `Car`
            [type-mismatch] 45: Assignment type mismatch: expected `Animal`, actual `Car`
            [type-mismatch] 48: Initializer type mismatch: expected `Int`, actual `String`
            [type-mismatch] 55: Argument type mismatch: expected `Dog`, actual `Car`
            [type-mismatch] 60: Argument type mismatch: expected `Dog`, actual `Dog?`
            [return-type-mismatch] 79: Return type mismatch: expected `Int`, actual `Success`"#]],
    );
}

#[test]
fn a_required_type_reaches_every_branch() {
    // As rust-analyzer's coercion of each branch: the mismatch is shown on
    // the value a branch ends with, not on the whole `if`.
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
open class Animal
class Dog : Animal()
class Car
fun branchy(flag: Boolean, dog: Dog, car: Car): Dog = if (flag) dog else car
fun whenny(number: Int, dog: Dog, car: Car): Dog = when (number) {
    1 -> dog
    else -> car
}
fun tried(dog: Dog, car: Car): Dog = try {
    dog
} catch (error: Exception) {
    car
}
fun elvised(maybe: Dog?, car: Car): Dog = maybe ?: car
fun tail(flag: Boolean, dog: Dog, car: Car) {
    val pet: Dog = if (flag) {
        val note = "x"
        car
    } else {
        dog
    }
}
fun nested(flag: Boolean, dog: Dog, car: Car): Dog {
    return if (flag) (if (!flag) car else dog) else dog
}
fun fine(flag: Boolean, dog: Dog, car: Car): Animal {
    if (flag) dog else car
    val pet: Dog = if (flag) dog else throw IllegalStateException()
    return if (flag) dog else Animal()
}
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::ReturnTypeMismatch, crate::Check::TypeMismatch],
            ..crate::DiagnosticsConfig::default()
        },
        expect![[r#"
            [return-type-mismatch] 5: Return type mismatch: expected `Dog`, actual `Car`
            [return-type-mismatch] 8: Return type mismatch: expected `Dog`, actual `Car`
            [return-type-mismatch] 13: Return type mismatch: expected `Dog`, actual `Car`
            [return-type-mismatch] 15: Return type mismatch: expected `Dog`, actual `Car`
            [type-mismatch] 19: Initializer type mismatch: expected `Dog`, actual `Car`
            [return-type-mismatch] 25: Return type mismatch: expected `Dog`, actual `Car`"#]],
    );
}

#[test]
fn a_nullable_local_is_judged_after_the_smart_casts_the_compiler_makes() {
    // Every way the compiler learns a local isn't null, beside a near
    // twin where some way to the use still lets it be null; a call's
    // value, which nothing smart-casts, is judged as it is.
    let config = || crate::DiagnosticsConfig {
        only: vec![crate::Check::TypeMismatch, crate::Check::ReturnTypeMismatch],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
open class Exception
class Dog { val name: String? = null; val owner: Owner? = null }
class Owner(val id: Int)
enum class Status { NEW, DONE }
class Order(val status: Status?)
fun feed(dog: Dog) {}
fun take(order: Order) {}
fun greet(name: String) {}
fun fail(): Nothing = TODO()
fun later(block: () -> Unit) {}
fun assigned(input: Dog?) {
    var dog = input
    if (dog == null) dog = Dog()
    feed(dog)
}
fun assignedOneWay(input: Dog?, flag: Boolean) {
    var dog = input
    if (flag) dog = Dog()
    feed(dog)
}
fun checkedButGoesOn(dog: Dog?) {
    if (dog == null) println("none")
    feed(dog)
}
fun subjectless(dog: Dog?) {
    when {
        dog == null -> return
        else -> feed(dog)
    }
    feed(dog)
}
fun subjectlessGoesOn(dog: Dog?, flag: Boolean) {
    when {
        dog == null -> println("none")
        flag -> return
    }
    feed(dog)
}
fun nullBranch(order: Order?) {
    when (order?.status) {
        null -> return
        Status.NEW -> take(order)
        Status.DONE -> fail()
    }
    take(order)
}
fun contracts(dog: Dog?, name: String?) {
    requireNotNull(dog)
    feed(dog)
    require(!name.isNullOrEmpty())
    greet(name)
}
fun nestedContract(dog: Dog?) {
    feed(requireNotNull(dog))
    feed(dog)
}
fun contractInLambda(dog: Dog?) {
    later { requireNotNull(dog) }
    feed(dog)
}
fun safeCall(dog: Dog?) {
    dog?.let { feed(dog) }
}
fun safeChainCompared(dog: Dog?) {
    if (dog?.owner != null) feed(dog)
}
fun comparedWithValue(dog: Dog?, other: Dog) {
    if (dog == other) feed(dog)
}
fun comparedWithNotValue(order: Order?) {
    if (order?.status != Status.NEW) return
    take(order)
}
fun tried(input: Dog?) {
    try {
        if (input == null) return
    } catch (exception: Exception) {
        return
    }
    feed(input)
}
fun triedCatchGoesOn(input: Dog?) {
    try {
        if (input == null) return
    } catch (exception: Exception) {
        println("failed")
    }
    feed(input)
}
fun aliased(info: Dog?) {
    val chain = info?.let { it.name }
    chain?.let { feed(info) }
}
class Repo { fun find(): Dog? = null }
fun find(): Dog? = null
fun called(repo: Repo) {
    feed(find())
    feed(repo.find())
    feed(repo.find() ?: Dog())
    val dog: Dog = find()
}
fun returned(): Dog = find()
fun inPlace(dog: Dog?) {
    run { requireNotNull(dog) }
    feed(dog)
}
fun inPlaceOnReceiver(dog: Dog?, other: Dog) {
    other.let { requireNotNull(dog) }
    feed(dog)
}
fun maybeRun(dog: Dog?, other: Dog?) {
    other?.let { requireNotNull(dog) }
    feed(dog)
}
fun leftEarly(dog: Dog?, flag: Boolean) {
    run {
        if (flag) return@run
        requireNotNull(dog)
    }
    feed(dog)
}
fun untilFound(next: () -> Dog?) {
    var dog: Dog? = next()
    while (dog == null) dog = next()
    feed(dog)
}
fun untilFoundOrBroken(next: () -> Dog?, flag: Boolean) {
    var dog: Dog? = next()
    while (dog == null) {
        if (flag) break
        dog = next()
    }
    feed(dog)
}
fun doUntilFound(next: () -> Dog?) {
    var dog: Dog?
    do {
        dog = next()
    } while (dog == null)
    feed(dog)
}
fun reassignedInLoop(start: Dog, next: () -> Dog?, flag: Boolean) {
    var dog: Dog? = start
    while (flag) {
        feed(dog)
        dog = next()
    }
}
fun reassignedInFor(start: Dog, all: List<Dog?>, next: () -> Dog?) {
    var dog: Dog? = start
    for (each in all) {
        val found: Dog? = next()
        dog = found
    }
    feed(dog)
}
"#,
        config(),
        expect![[r#"
            [type-mismatch] 20: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 24: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 38: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 60: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 89: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 98: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 99: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 101: Initializer type mismatch: expected `Dog`, actual `Dog?`
            [return-type-mismatch] 103: Return type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 114: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 121: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 134: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 146: Argument type mismatch: expected `Dog`, actual `Dog?`
            [type-mismatch] 156: Argument type mismatch: expected `Dog`, actual `Dog?`"#]],
    );
}

#[test]
fn type_arguments_are_judged_by_declared_variance() {
    // `List<out E>`, `Map<K, out V>`, `Sink<in T>`: an argument of an `out`
    // parameter must fit, one of an `in` parameter be fitted. An invariant
    // parameter's isn't judged: a use-site projection (`Box<out Animal>`)
    // isn't kept. Nor is a call's argument: a candidate whose parameter's
    // type arguments don't fit makes the compiler look further out.
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
open class Animal
class Dog : Animal()
class Car
class Box<T>(val value: T)
interface Source<out T> { fun next(): T }
interface Sink<in T> { fun put(value: T) }
fun walkAll(dogs: List<Dog>) {}
fun animals(all: List<Animal>) {}
fun use(
    dogList: List<Dog>,
    carList: List<Car>,
    maybeDogs: List<Dog?>,
    mutableDogs: MutableList<Dog>,
    dogBox: Box<Dog>,
    dogSource: Source<Dog>,
    animalSink: Sink<Animal>,
    dogSink: Sink<Dog>,
    carsByName: Map<String, Car>,
    carLists: List<List<Car>>,
) {
    walkAll(carList)
    animals(dogList)
    walkAll(maybeDogs)
    walkAll(mutableDogs)
    val box: Box<Animal> = dogBox
    val source: Source<Animal> = dogSource
    val sink: Sink<Dog> = animalSink
    val wrongSink: Sink<Animal> = dogSink
    val dogsByName: Map<String, Dog> = carsByName
    val dogLists: List<List<Dog>> = carLists
    val anyLists: List<List<Any>> = carLists
}
fun returned(cars: List<Car>): List<Dog> = cars
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::TypeMismatch, crate::Check::ReturnTypeMismatch],
            ..crate::DiagnosticsConfig::default()
        },
        expect![[r#"
            [type-mismatch] 29: Initializer type mismatch: expected `Sink<Animal>`, actual `Sink<Dog>`
            [type-mismatch] 30: Initializer type mismatch: expected `Map<String, Dog>`, actual `Map<String, Car>`
            [type-mismatch] 31: Initializer type mismatch: expected `List<List<Dog>>`, actual `List<List<Car>>`
            [return-type-mismatch] 34: Return type mismatch: expected `List<Dog>`, actual `List<Car>`"#]],
    );
}

#[test]
fn type_mismatches_stay_silent_where_inference_is_unsure() {
    // Each value here is right for the compiler; a resolution that misses
    // a rule of its would make it look wrong.
    let config = || crate::DiagnosticsConfig {
        only: vec![crate::Check::ReturnTypeMismatch, crate::Check::TypeMismatch],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        r#"
//- /app/a/Cache.kt
package a
class Dog
class Cache {
    private fun cached(key: Int): String = ""
}
interface Owned { val owner: Any }
interface Bound<T> { val owner: T }
interface DogSymbol : Owned, Bound<Dog>
//- /app/b/Use.kt
package b
import a.*
private fun Cache.cached(key: Int): Dog = Dog()
fun fromExtension(cache: Cache): Dog = cache.cached(1)
fun intersection(symbol: DogSymbol): Dog = symbol.owner
fun <T> build(block: T.() -> Unit): T = TODO()
interface Init { var method: Dog }
class Form {
    var method: String = ""
    fun fill(dog: Dog): Init = build { method = dog }
}
class Config {
    var transform: (Dog) -> String = { "" }
    fun transform(block: (Dog) -> String) {}
}
fun invoked(config: Config): String = config.transform(Dog())
fun takesText(text: String?) {}
fun onlyNull(dog: Dog?) {
    if (dog == null) takesText(dog)
}
class Request
fun cookiesOf(request: Request): String = ""
open class Cookies(protected val request: Any) {
    open fun fetch(): String = ""
}
class Call {
    inner class Incoming(request: Request) {
        val cookies: Cookies = object : Cookies(this@Incoming) {
            override fun fetch(): String = cookiesOf(request)
        }
    }
}
"#,
        config(),
        expect![[r#""#]],
    );
}

#[test]
fn uses_of_api_that_needs_opt_in() {
    let api = r#"
//- /app/a/Api.kt
package a
@RequiresOptIn(level = RequiresOptIn.Level.WARNING)
annotation class Beta
@RequiresOptIn(message = "Internal API, may change")
annotation class Internal
@Beta fun beta() {}
@Internal class Secret { fun reveal() {} }
fun stable() {}
@Beta fun over(number: Int) {}
fun over(text: String) {}
"#;
    let config = || crate::DiagnosticsConfig {
        only: vec![crate::Check::OptInUsage, crate::Check::OptInUsageError],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        &format!(
            "{api}{}",
            r#"
//- /app/b/Use.kt
package b
import a.*
fun use(secret: Secret) {
    beta()
    secret.reveal()
    stable()
    over(1)
}
@OptIn(Beta::class) fun optedIn() = beta()
@Beta fun propagates() = beta()
fun local() {
    @OptIn(Internal::class) val secret = Secret()
}
class Holder {
    @OptIn(Beta::class)
    val value = run { beta() }
    @OptIn(Beta::class)
    val detached: Unit
        get() = beta()
}
fun annotatedExpressions() {
    @OptIn(Beta::class)
    beta()
    take(
        @OptIn(Beta::class)
        value = beta(),
    )
}
fun take(value: Unit) = value
fun <R> run(block: () -> R): R = block()
"#
        ),
        config(),
        expect![[r#"
            [opt-in-usage-error] 3: This declaration needs opt-in. Internal API, may change
            [opt-in-usage] 4: This declaration needs opt-in. Its usage should be marked with `@a.Beta` or `@OptIn(a.Beta::class)`
            [opt-in-usage-error] 5: This declaration needs opt-in. Internal API, may change"#]],
    );
    // A file or a module that opts in accepts every use in it; without
    // either, the same use needs it.
    check_diagnostics(
        &format!(
            "{api}{}",
            "\n//- /app/b/Use.kt\npackage b\nimport a.*\nfun use() = beta()\n"
        ),
        config(),
        expect![
            "[opt-in-usage] 3: This declaration needs opt-in. Its usage should be marked with `@a.Beta` or `@OptIn(a.Beta::class)`"
        ],
    );
    check_diagnostics(
        &format!(
            "{api}{}",
            "\n//- /app/b/Use.kt\n@file:OptIn(a.Beta::class)\npackage b\nimport a.*\nfun use() = beta()\n"
        ),
        config(),
        expect![""],
    );
    check_diagnostics(
        &format!(
            "{api}{}",
            "\n//- /app/b/Use.kt optin=a.Beta\npackage b\nimport a.*\nfun use() = beta()\n"
        ),
        config(),
        expect![""],
    );
    // An annotation whose class doesn't resolve may be the opt-in.
    check_diagnostics(
        &format!(
            "{api}{}",
            "\n//- /app/b/Use.kt\npackage b\nimport a.*\n@Unknown(Beta::class) fun use() = beta()\n"
        ),
        config(),
        expect![""],
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

/// The Kotlin warnings about null checks and locals, alone.
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
fn java_overloads_that_differ_in_boxing_alone_are_both_seen() {
    // AssertJ's `isGreaterThan(long)` beside the inherited
    // `isGreaterThan(ACTUAL)` with `ACTUAL` = `Long`: two JVM methods, so
    // a `Long?` goes to the boxed one; where only `long` is, it can't.
    check_diagnostics(
        r#"
//- /app/a/ComparableAssert.java
package a;
public class ComparableAssert<ACTUAL> {
    public void isGreaterThan(ACTUAL other) {}
}
//- /app/a/LongAssert.java
package a;
public class LongAssert extends ComparableAssert<Long> {
    public void isGreaterThan(long other) {}
    public void isLessThan(long other) {}
}
//- /app/b/Use.kt
package b
import a.LongAssert
fun check(assertion: LongAssert, bound: Long?) {
    assertion.isGreaterThan(bound)
    assertion.isLessThan(bound)
}
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::TypeMismatch, crate::Check::ArgumentMismatch],
            ..crate::DiagnosticsConfig::default()
        },
        expect!["[type-mismatch] 5: Argument type mismatch: expected `Long`, actual `Long?`"],
    );
}

#[test]
fn unsafe_calls_follow_the_data_flow() {
    // A local's or a call's type after the smart casts the compiler makes
    // decides; a property's (whose smart casts aren't modelled) still goes
    // by whether it's checked anywhere around.
    let config = || crate::DiagnosticsConfig {
        only: vec![crate::Check::UnsafeCall],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
class Dog { val name: String? = null; fun bark() {} }
class Box<V>(val value: V) { fun get(): V? = value; fun clear() {} }
class Repo { fun find(): Dog? = null }
class Holder(val dog: Dog?)
fun checkedOnOneWay(dog: Dog?) {
    if (dog != null) dog.bark()
    dog.bark()
}
fun aliased(dog: Dog?) {
    val ready = dog != null
    if (ready) dog.bark()
}
fun aliasedNegated(dog: Dog?) {
    val missing = dog == null
    if (missing) return
    dog.bark()
}
fun aliasedVar(input: Dog?) {
    var dog = input
    val ready = dog != null
    if (ready) dog.bark()
}
fun aliasedVarAssigned(input: Dog?) {
    var dog = input
    val ready = dog != null
    dog = input
    if (ready) dog.bark()
}
fun elvisOnChain(dog: Dog?) {
    val name = dog?.name ?: return
    dog.bark()
}
fun <V : Any> comparedWithNonNull(box: Box<V>?, value: V) {
    if (box?.get() == value) box.clear()
}
fun <V> comparedWithMaybeNull(box: Box<V>?, value: V) {
    if (box?.get() == value) box.clear()
}
fun called(repo: Repo) {
    repo.find().bark()
}
fun calledTwice(repo: Repo) {
    if (repo.find() != null) repo.find().bark()
}
fun property(holder: Holder) {
    if (holder.dog != null) holder.dog.bark()
}
"#,
        config(),
        expect![[r#"
            [unsafe-call] 8: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`
            [unsafe-call] 28: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`
            [unsafe-call] 38: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Box<V>?`
            [unsafe-call] 41: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`
            [unsafe-call] 44: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`"#]],
    );
}

#[test]
fn stable_properties_smart_cast_as_locals_do() {
    // The compiler's rules: a final `val` with the default getter, of this
    // module (or a friend: tests), read from a value that can't change.
    // One it never smart-casts keeps its declared type; one it may
    // smart-cast where this inference can't tell is left alone.
    let config = || crate::DiagnosticsConfig {
        only: vec![
            crate::Check::UnsafeCall,
            crate::Check::TypeMismatch,
            crate::Check::UnresolvedReference,
        ],
        ..crate::DiagnosticsConfig::default()
    };
    check_diagnostics(
        r#"
//- /lib/a/Remote.kt module=:lib
package a
class Dog { fun bark() {} }
class Remote(val dog: Dog?)
//- /app/b/Use.kt module=:app deps=:lib
package b
import a.Dog
import a.Remote
class Holder(val dog: Dog?)
class Outer(val holder: Holder)
class Mutable(var dog: Dog?)
open class Base(open val dog: Dog?)
class Getter { val dog: Dog? get() = null }
class Self(val dog: Dog?) {
    fun bark() {
        if (dog != null) dog.bark()
        if (this.dog != null) dog.bark()
    }
}
fun feed(dog: Dog) {}
fun stable(holder: Holder) {
    if (holder.dog != null) holder.dog.bark()
}
fun chain(outer: Outer) {
    if (outer.holder.dog != null) feed(outer.holder.dog)
}
fun safeChain(holder: Holder?) {
    if (holder?.dog != null) holder.dog.bark()
}
fun returned(holder: Holder) {
    holder.dog ?: return
    holder.dog.bark()
}
fun unchecked(holder: Holder) {
    holder.dog.bark()
    feed(holder.dog)
}
fun mutable(holder: Mutable) {
    if (holder.dog != null) holder.dog.bark()
}
fun getter(holder: Getter) {
    if (holder.dog != null) holder.dog.bark()
}
fun remote(remote: Remote) {
    if (remote.dog != null) remote.dog.bark()
}
fun open(base: Base) {
    if (base.dog != null) base.dog.bark()
}
fun varRoot(input: Holder) {
    var holder = input
    if (holder.dog != null) holder.dog.bark()
}
//- /app/b/UseTest.kt module=:app set=test deps=:lib
package b
fun fromTest(holder: Holder) {
    if (holder.dog != null) holder.dog.bark()
}
"#,
        config(),
        expect![[r#"
            [unsafe-call] 30: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`
            [type-mismatch] 31: Argument type mismatch: expected `Dog`, actual `Dog?`
            [unsafe-call] 34: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`
            [unsafe-call] 37: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`
            [unsafe-call] 40: Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `Dog?`"#]],
    );
}

#[test]
fn references_to_a_java_getter_count_its_property_uses() {
    // `getTitle()` read as `store.title` or called as `store.getTitle()`:
    // both are references, found from either name.
    let fixture = r#"
//- /app/a/Store.java
package a;
public class Store {
    public String getTitle() { return null; }
}
//- /app/b/Use.kt
package b
import a.Store
fun use(store: Store) {
    println(store.title)
    println(store.getTitle())
}
"#;
    let references = |marker: &str, marked: &str| {
        let fixture = Fixture::parse(&fixture.replacen(marker, marked, 1));
        let (file, offset) = fixture.cursor();
        let mut lines: Vec<String> = analysis(&fixture)
            .references(file, offset, true)
            .unwrap()
            .iter()
            .map(|found| {
                let text = fixture.vfs.contents(found.file).unwrap();
                let line = text[..found.range.start as usize].matches('\n').count() + 1;
                let name = &text[found.range.start as usize..found.range.end as usize];
                format!("{}:{line} {name}", fixture.vfs.path(found.file).display())
            })
            .collect();
        lines.sort();
        lines.join("\n")
    };
    expect![[r#"
        /app/a/Store.java:3 getTitle
        /app/b/Use.kt:4 title
        /app/b/Use.kt:5 getTitle"#]]
    .assert_eq(&references("store.title", "store.ti$0tle"));
    expect![[r#"
        /app/a/Store.java:3 getTitle
        /app/b/Use.kt:4 title
        /app/b/Use.kt:5 getTitle"#]]
    .assert_eq(&references("String getTitle", "String getTi$0tle"));
}

#[test]
fn highlights_tell_writes_from_reads() {
    // A local and a property: the declaration, assignments (`=`, `+=`),
    // increments and a qualified target write; the rest reads — the
    // receiver of `box.count = …` included.
    let fixture = r#"
//- /app/b/Use.kt
package b
class Box(var count: Int)
fun use(box: Box) {
    var total = 0
    total = box.count
    total += 1
    total++
    println(total)
    box.count = total
}
"#;
    let highlights = |marker: &str, marked: &str| {
        let fixture = Fixture::parse(&fixture.replacen(marker, marked, 1));
        let (file, offset) = fixture.cursor();
        let text = fixture.vfs.contents(file).unwrap();
        analysis(&fixture)
            .highlights(file, offset)
            .unwrap()
            .iter()
            .map(|highlight| {
                let line = text[..highlight.range.start as usize].matches('\n').count() + 1;
                let name = &text[highlight.range.start as usize..highlight.range.end as usize];
                format!("{line} {name} {:?}", highlight.access)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    expect![[r#"
        4 total Write
        5 total Write
        6 total Write
        7 total Write
        8 total Read
        9 total Read"#]]
    .assert_eq(&highlights("println(total)", "println(to$0tal)"));
    expect![[r#"
        2 count Write
        5 count Read
        9 count Write"#]]
    .assert_eq(&highlights("box.count = total", "box.cou$0nt = total"));
}

#[test]
fn type_definitions_are_the_classes_of_the_type() {
    // A local's, a parameter's and a property's type: its class, then its
    // type arguments' (a library's, `List`, isn't shown here).
    let fixture = r#"
//- /app/b/Use.kt
package b
class Order(val id: Int)
class Holder(val orders: List<Order>)
fun use(holder: Holder) {
    val first = holder.orders
    println(first)
}
"#;
    let classes = |marker: &str, marked: &str| {
        let fixture = Fixture::with_mini_stdlib(&fixture.replacen(marker, marked, 1));
        let (file, offset) = fixture.cursor();
        analysis(&fixture)
            .goto_type_definition(file, offset)
            .unwrap()
            .iter()
            .map(|target| match target.file {
                crate::NavigationFile::Workspace(file) => {
                    let text = fixture.vfs.contents(file).unwrap();
                    text[target.focus_range.start as usize..target.focus_range.end as usize]
                        .to_owned()
                }
                crate::NavigationFile::Library { .. } => format!("library {}", target.name),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    expect!["List, Order"].assert_eq(&classes("println(first)", "println(fir$0st)"));
    expect!["Holder"].assert_eq(&classes("fun use(holder", "fun use(hol$0der"));
    expect!["List, Order"].assert_eq(&classes("val orders", "val ord$0ers"));
}

#[test]
fn implementations_are_subclasses_and_overrides() {
    // A class's or interface's subclasses at any depth, Java ones too; a
    // member's overrides in them — not a namesake in an unrelated class
    // nor an overload taking other parameters.
    let fixture = r#"
//- /app/a/Shapes.kt
package a
interface Shape { fun area(): Double; val name: String }
abstract class Polygon : Shape { override val name: String get() = "polygon" }
class Square(val side: Double) : Polygon() { override fun area(): Double = side * side }
open class Circle(val radius: Double) : Shape {
    override fun area(): Double = radius
    override val name: String = "circle"
    fun area(scale: Double): Double = radius * scale
}
class Ring(radius: Double) : Circle(radius) {
    override fun area(): Double = 0.0
}
class Unrelated { fun area(): Double = 0.0 }
//- /app/a/JavaShape.java
package a;
public class JavaShape implements Shape {
    public double area() { return 0; }
    public String getName() { return ""; }
}
//- /app/b/Use.kt
package b
import a.Shape
fun first(shape: Shape) = shape.area() + shape.name.length
"#;
    let implementations = |marker: &str, marked: &str| {
        let fixture = Fixture::parse(&fixture.replace(marker, marked));
        let (file, offset) = fixture.cursor();
        let mut lines: Vec<String> = analysis(&fixture)
            .goto_implementation(file, offset)
            .unwrap()
            .iter()
            .filter_map(|target| match target.file {
                crate::NavigationFile::Workspace(file) => {
                    let text = fixture.vfs.contents(file).unwrap();
                    let line = text[..target.focus_range.start as usize]
                        .matches('\n')
                        .count()
                        + 1;
                    Some(format!("{}:{line}", fixture.vfs.path(file).display()))
                }
                crate::NavigationFile::Library { .. } => None,
            })
            .collect();
        lines.sort();
        lines.join("\n")
    };
    expect![[r#"
        /app/a/JavaShape.java:2
        /app/a/Shapes.kt:10
        /app/a/Shapes.kt:3
        /app/a/Shapes.kt:4
        /app/a/Shapes.kt:5"#]]
    .assert_eq(&implementations("import a.Shape", "import a.Sha$0pe"));
    expect![[r#"
        /app/a/JavaShape.java:3
        /app/a/Shapes.kt:11
        /app/a/Shapes.kt:4
        /app/a/Shapes.kt:6"#]]
    .assert_eq(&implementations("shape.area()", "shape.ar$0ea()"));
    expect![[r#"
        /app/a/Shapes.kt:3
        /app/a/Shapes.kt:7"#]]
    .assert_eq(&implementations("shape.name", "shape.na$0me"));
}

#[test]
fn classes_override_and_implement_what_they_inherit() {
    // NOTHING_TO_OVERRIDE, VIRTUAL_MEMBER_HIDDEN and
    // ABSTRACT_MEMBER_NOT_IMPLEMENTED, beside what must stay silent: an
    // overload, `Any`'s members, a private member, a delegate, a getter on
    // the line after its property, a generic interface's member.
    check_diagnostics(
        r#"
//- /app/a/Listener.java
package a;
public interface Listener {
    void onEvent(String event);
}
//- /app/b/Use.kt
package b
import a.Listener
interface Shape {
    fun area(): Double
    val name: String
    fun describe(): String = name
}
abstract class Base : Shape {
    abstract fun scale(by: Double): Base
}
class Square(val side: Double) : Base() {
    override fun area(): Double = side * side
    override val name: String = "square"
    override fun scale(by: Double): Base = Square(side * by)
}
class Incomplete : Shape {
    override val name = "incomplete"
}
abstract class StillAbstract : Shape
class Wrong : Shape {
    override fun area(): Double = 0.0
    override val name = "wrong"
    override fun perimeter(): Double = 0.0
    fun describe(): String = "wrong"
    override fun toString(): String = "wrong"
    fun area(scale: Double): Double = scale
}
class Delegating(shape: Shape) : Shape by shape
interface Ordered {
    val order: Int
        get() = 0
}
class Plain : Ordered
object Single : Shape {
    override fun area() = 1.0
    override val name = "single"
}
interface Box<T> {
    fun put(value: T)
}
class IntBox : Box<Int> {
    override fun put(value: Int) {}
}
class StringBox : Box<String> {
    override fun put(value: Int) {}
}
open class Secretive {
    private fun secret() {}
}
class Peeker : Secretive() {
    fun secret() {}
}
class JavaImplemented : Listener {
    override fun onEvent(event: String) {}
}
class JavaMissing : Listener
"#,
        crate::DiagnosticsConfig {
            only: vec![
                crate::Check::NothingToOverride,
                crate::Check::MissingOverride,
                crate::Check::AbstractMemberNotImplemented,
            ],
            ..crate::DiagnosticsConfig::default()
        },
        expect![[r#"
            [abstract-member-not-implemented] 16: Class `Incomplete` isn't abstract and doesn't implement abstract member `area` of `Shape`
            [nothing-to-override] 23: `perimeter` overrides nothing
            [missing-override] 24: `describe` hides a member of supertype `Shape` and needs `override`
            [abstract-member-not-implemented] 44: Class `StringBox` isn't abstract and doesn't implement abstract member `put` of `Box`
            [nothing-to-override] 45: `put` overrides nothing
            [abstract-member-not-implemented] 56: Class `JavaMissing` isn't abstract and doesn't implement abstract member `onEvent` of `Listener`"#]],
    );
}

#[test]
fn values_are_assigned_once_and_functions_return() {
    // VAL_REASSIGNMENT and NO_RETURN_IN_FUNCTION_WITH_BLOCK_BODY, beside
    // what must stay silent: a `val`'s first value, a `var`, every way
    // jumping, an endless loop, a lambda run in place that returns, a call
    // not resolved (it may never return).
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
fun reassign(parameter: Int, items: List<Int>) {
    val fixed = 1
    fixed = 2
    val later: Int
    later = 3
    var free = 1
    free = 2
    parameter = 4
    for (item in items) {
        item = 0
    }
    items.forEach { it = 1 }
    fixed++
}
fun noReturn(flag: Boolean): Int {
    if (flag) return 1
}
fun everyWay(flag: Boolean): Int {
    if (flag) return 1 else return 2
}
fun throws(): Int {
    throw IllegalStateException()
}
fun endless(): Int {
    while (true) {
        println(1)
    }
}
fun endlessUntilBroken(): Int {
    while (true) {
        break
    }
}
fun unitResult() {
}
fun inPlace(): Int {
    run { return 1 }
}
fun unresolved(): Int {
    missing()
}
fun empty(): Int {
}
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::ValReassignment, crate::Check::MissingReturn],
            ..crate::DiagnosticsConfig::default()
        },
        expect![[r#"
            [val-reassignment] 4: `val` cannot be reassigned
            [val-reassignment] 9: `val` cannot be reassigned
            [val-reassignment] 11: `val` cannot be reassigned
            [val-reassignment] 13: `val` cannot be reassigned
            [val-reassignment] 14: `val` cannot be reassigned
            [missing-return] 18: Missing `return` statement
            [missing-return] 34: Missing `return` statement
            [missing-return] 44: Missing `return` statement"#]],
    );
}

#[test]
fn the_flow_merges_ways_as_the_compiler_does() {
    // Each way of a branch runs on its own copy of the flow; past it, what
    // every way that goes on knows — their common supertype — and ways
    // that jump don't count. Each case here compiles: nothing is reported.
    check_diagnostics(
        r#"
//- /app/a/Reader.java
package a;
public class Reader {
    public String readLine() { return null; }
}
//- /app/b/Use.kt lang=2.3
package b
import a.Reader
interface Shape { fun area(): Int }
class Circle : Shape { override fun area() = 1 }
class Square : Shape { override fun area() = 2 }
open class Failure
fun joined(c: Boolean) {
    var shape: Shape?
    if (c) shape = Circle() else shape = Square()
    shape.area()
}
fun either(x: Any) {
    if (x is Circle || x is Square) x.area()
}
fun eitherInWhen(x: Any) {
    when (x) {
        is Circle, is Square -> x.area()
        else -> {}
    }
}
fun tryFinally(p: String?) {
    try {
        if (p == null) return
    } finally {
        println("done")
    }
    p.length
}
fun emptyElse(p: String?) {
    if (p != null) {
        println(p)
    } else {
    }
    p?.length
}
fun parenthesized(x: String?) {
    val ready = (x != null)
    if (ready) x.length
}
fun broken(items: List<String?>): Int {
    var found: String? = null
    for (item in items) {
        if (item == null) continue
        found = item
        break
    }
    return found?.length ?: 0
}
fun caught(p: String?) {
    try {
        println(1)
    } catch (exception: Failure) {
        return
    }
    p?.length
}
fun javaValue(lines: Reader) {
    val line = lines.readLine()
    if (line != null) println(line)
    if (line == null) return
}
fun initialized() {
    var x: String? = "a"
    x.length
    x = null
}
fun assignedUnknown(shape: Shape?) {
    var current: Shape? = shape
    current = missing()
    current.area()
}
sealed class Side
class Left : Side()
class Right : Side()
fun exhaustive(side: Side, p: String?) {
    when (side) {
        is Left -> if (p == null) return
        is Right -> if (p == null) return
    }
    p.length
}
fun Shape.negated(): Int {
    if (this !is Circle) return 0
    return area()
}
fun Shape.both(): Boolean = this is Circle && area() == 1
fun Shape.cast(): Int {
    this as Circle
    return area()
}
class Node(val next: Node?)
fun receivers(a: Node, b: Node) {
    a.apply { next!! }
    b.apply { next?.next }
}
fun String?.orEmpty(): String = this ?: ""
class Holder(val p: String?)
fun plainDot(h: Holder?) {
    if (h?.p.orEmpty() == "x") h?.p
}
"#,
        crate::DiagnosticsConfig {
            only: vec![
                crate::Check::UnsafeCall,
                crate::Check::UnresolvedReference,
                crate::Check::UnnecessarySafeCall,
                crate::Check::SenselessComparison,
            ],
            ..crate::DiagnosticsConfig::default()
        },
        expect!["[unresolved-reference] 69: Unresolved reference `missing`"],
    );
}

#[test]
fn a_cast_to_a_type_parameter_after_a_smart_cast_is_not_judged() {
    // `this` is `Any & T` to the compiler after `require(this is T)`; this
    // model would see a `T` and call the cast useless.
    check_diagnostics(
        r#"
//- /app/b/Use.kt lang=2.3
package b
inline fun <reified T> Any.expect(): T {
    require(this is T)
    return this as T
}
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::UselessCast, crate::Check::UselessIsCheck],
            ..crate::DiagnosticsConfig::default()
        },
        expect![""],
    );
}

#[test]
fn values_passed_as_a_type_parameter_join_and_lambdas_dont_widen_it() {
    // `listOf(circle, square)`: each value is a lower bound, `T` is what
    // they share. A lambda's receiver (`apply` given a `Base.() -> Unit`)
    // is an upper bound: `T` stays the receiver's own class.
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
interface Shape { fun area(): Int }
class Circle : Shape { override fun area() = 1 }
class Square : Shape { override fun area() = 2 }
fun total() = listOf(Circle(), Square()).first().area()
open class Base
class Derived : Base() { val own = 1 }
fun build(block: Base.() -> Unit): Int = Derived().apply(block).own
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::UnresolvedReference],
            experimental: true,
            ..crate::DiagnosticsConfig::default()
        },
        expect![""],
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
fn completion_items_show_the_rest_of_their_signature_and_where_they_are_declared() {
    let details = |fixture: &str, labels: &[&str]| {
        let fixture = Fixture::with_mini_stdlib(fixture);
        let (file, offset) = fixture.cursor();
        let completions = analysis(&fixture).completions(file, offset).unwrap();
        labels
            .iter()
            .filter_map(|label| completions.items.iter().find(|item| item.label == *label))
            .map(|item| {
                format!(
                    "{}{}  | {}",
                    item.label,
                    item.label_detail.as_deref().unwrap_or(""),
                    item.label_description.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    expect![[r#"
        charge(amount: Long): Unit  | Account
        owner: String  | Account"#]]
    .assert_eq(&details(
        r#"
//- /app/b/Use.kt
package b
class Account(val owner: String) { fun charge(amount: Long): Unit {} }
fun use(account: Account) { account.$0 }
"#,
        &["charge", "owner"],
    ));
    expect![[r#"
        total: Int  | 
        helper(): Int  | b"#]]
    .assert_eq(&details(
        r#"
//- /app/b/Use.kt
package b
fun helper(): Int = 1
fun use() { val total = 1; total + $0 }
"#,
        &["total", "helper"],
    ));
}

#[test]
fn completion_in_an_argument_offers_the_parameters_by_name_and_resolves_documentation() {
    // `name = ` first for each parameter not passed by name yet; then what
    // the scope has. Resolving an item gives its declaration's KDoc.
    let fixture = r#"
//- /app/b/Use.kt
package b
/** Charges [amount] to an account. */
fun charge(amount: Long, currency: String, note: String = "") {}
fun use(account: Long) {
    charge(currency = "EUR", a$0)
}
"#;
    check_completions(
        fixture,
        4,
        expect![[r#"
            amount = NamedArgument
            account Variable
            Any Class
            Array Class"#]],
    );
    let fixture = Fixture::with_mini_stdlib(fixture);
    let (file, offset) = fixture.cursor();
    let completions = analysis(&fixture).completions(file, offset).unwrap();
    let named = completions
        .items
        .iter()
        .find(|item| item.label == "amount =")
        .unwrap();
    assert_eq!(named.insert_text(), "amount = ");
    // Before the `a`, nothing is typed: `charge` is offered too.
    let documentation = analysis(&fixture)
        .completion_documentation(file, offset - 1, "charge")
        .unwrap();
    assert_eq!(
        documentation.as_deref(),
        Some("Charges [amount] to an account.")
    );
    let documentation = analysis(&fixture)
        .completion_documentation(file, offset - 1, "account")
        .unwrap();
    assert_eq!(
        documentation, None,
        "a local has no declaration to document"
    );
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
fn member_completion_puts_own_properties_first_and_what_every_value_has_last() {
    // Declared functions first, an override of `toString` among them and
    // an inherited property: the order is the likelihood, not the text's.
    check_completions(
        r#"
//- /app/a/Rate.kt
package a
open class Quote(val source: String) {
    fun refresh() {}
}
class Rate(source: String) : Quote(source) {
    override fun toString(): String = "rate"
    fun convert(amount: Int): Int = amount
    val amount: Int = 1
    val currency: String = ""
}
//- /app/b/Use.kt
package b
import a.Rate
fun use(rate: Rate) {
    rate.$0
}
"#,
        8,
        expect![[r#"
            amount Field
            currency Field
            convert Method
            source Field
            refresh Method
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
fn the_declaration_of_an_actual_is_its_expect_and_of_anything_else_its_definition() {
    let fixture = r#"
//- /shared/common/a/Store.kt module=:shared set=commonMain
package a
expect fun storeUrl(): String?
fun plain(): Int = 1
//- /shared/jvm/a/Store.jvm.kt module=:shared set=jvmMain deps=:shared/commonMain
package a
actual fun storeUrl(): String? = null
fun use() = storeUrl() + plain()
"#;
    let declarations = |marker: &str, marked: &str| {
        let fixture = Fixture::parse(&fixture.replacen(marker, marked, 1));
        let (file, offset) = fixture.cursor();
        analysis(&fixture)
            .goto_declaration(file, offset)
            .unwrap()
            .iter()
            .map(|target| match target.file {
                crate::NavigationFile::Workspace(file) => {
                    let text = fixture.vfs.contents(file).unwrap();
                    let line = text[..target.focus_range.start as usize]
                        .matches('\n')
                        .count()
                        + 1;
                    format!("{}:{line}", fixture.vfs.path(file).display())
                }
                crate::NavigationFile::Library { .. } => "library".to_owned(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    // The `actual` declaration, and a call that resolves to it: its `expect`.
    expect!["/shared/common/a/Store.kt:2"].assert_eq(&declarations(
        "actual fun storeUrl",
        "actual fun store$0Url",
    ));
    expect!["/shared/common/a/Store.kt:2"]
        .assert_eq(&declarations("= storeUrl()", "= store$0Url()"));
    // An ordinary function: its definition.
    expect!["/shared/common/a/Store.kt:3"].assert_eq(&declarations("+ plain()", "+ pla$0in()"));
}

const HIERARCHY: &str = r#"
//- /app/b/Shapes.kt
package b
interface Shape
open class Polygon : Shape
class Square : Polygon()
class Circle : Shape
fun area(shape: Shape): Int = when (shape) { is Square -> side(); else -> 0 }
fun side(): Int = 1
fun report() = area(Square()) + area(Circle())
val total = area(Circle())
"#;

fn hierarchy_names(items: &[crate::HierarchyItem]) -> String {
    let mut names: Vec<&str> = items.iter().map(|item| item.target.name.as_str()).collect();
    names.sort();
    names.join(", ")
}

#[test]
fn the_type_hierarchy_has_direct_supertypes_and_workspace_subtypes() {
    let at = |marker: &str, marked: &str| {
        let fixture = Fixture::with_mini_stdlib(&HIERARCHY.replacen(marker, marked, 1));
        let (file, offset) = fixture.cursor();
        let analysis = analysis(&fixture);
        let prepared = analysis.prepare_type_hierarchy(file, offset).unwrap();
        let [item] = &prepared[..] else {
            panic!("one class: {prepared:?}");
        };
        let key = item.key.clone().unwrap();
        format!(
            "{}: supertypes [{}], subtypes [{}]",
            item.target.name,
            hierarchy_names(&analysis.supertypes(file, &key).unwrap()),
            hierarchy_names(&analysis.subtypes(file, &key).unwrap()),
        )
    };
    // A class's or an interface's implicit `Any` is one of its supertypes.
    expect!["Polygon: supertypes [Any, Shape], subtypes [Square]"]
        .assert_eq(&at("open class Polygon", "open class Poly$0gon"));
    expect!["Shape: supertypes [Any], subtypes [Circle, Polygon]"]
        .assert_eq(&at("interface Shape", "interface Sha$0pe"));
}

#[test]
fn the_call_hierarchy_groups_calls_by_caller_and_by_callee() {
    let fixture = Fixture::with_mini_stdlib(&HIERARCHY.replacen("fun area(", "fun ar$0ea(", 1));
    let (file, offset) = fixture.cursor();
    let analysis = analysis(&fixture);
    let prepared = analysis.prepare_call_hierarchy(file, offset).unwrap();
    let [area] = &prepared[..] else {
        panic!("one function: {prepared:?}");
    };
    let key = area.key.clone().unwrap();
    let render = |calls: Vec<crate::HierarchyCall>| {
        let mut rendered: Vec<String> = calls
            .iter()
            .map(|call| format!("{} x{}", call.item.target.name, call.ranges.len()))
            .collect();
        rendered.sort();
        rendered.join(", ")
    };
    expect!["report x2, total x1"].assert_eq(&render(analysis.incoming_calls(&key).unwrap()));
    expect!["side x1"].assert_eq(&render(analysis.outgoing_calls(&key).unwrap()));
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
        String Class defaultLibrary
        ZERO Property declaration,readonly,static
        balance Property declaration,mutable
        Long Class defaultLibrary
        value Parameter declaration,readonly
        field Variable mutable
        value Parameter readonly
        twice Property declaration,readonly
        Int Class defaultLibrary
        Int Class defaultLibrary
        it Parameter readonly
        describe Method declaration
        prefix Parameter declaration,readonly
        String Class defaultLibrary
        String Class defaultLibrary
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
        Int Class defaultLibrary
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

#[test]
fn a_type_check_the_grammar_splits_is_left_alone() {
    // At the end of a file the grammar reads `this is Result.Resolved` as
    // `(this is Result).Resolved`: a check the type `Result` would decide.
    check_diagnostics(
        r#"
//- /app/b/Use.kt
package b
sealed interface Result {
    data class Resolved(val data: String) : Result
    data object NotFound : Result
}
fun Result.dataOrNull(): String? = when (this) {
    is Result.Resolved -> data
    else -> null
}
fun Result.isResolved(): Boolean = this is Result.Resolved
"#,
        crate::DiagnosticsConfig {
            only: vec![crate::Check::UselessIsCheck],
            ..crate::DiagnosticsConfig::default()
        },
        expect![[r#""#]],
    );
}

#[test]
fn a_file_opened_before_the_workspace_loads_is_analysed_without_a_model() {
    let text = "package app\nclass Shop(val name: String) {\n    fun label() = name.length\n}\nfun use(shop: Shop) = shop.name.length\n";
    let mut vfs = vfs::Vfs::default();
    let file = vfs.set_contents(std::path::Path::new("/app/Shop.kt"), Some(text.into()));
    let mut host = crate::AnalysisHost::default();
    host.apply_change(vec![hir::FileChange {
        file,
        text: Some(text.into()),
        language: hir::Language::Kotlin,
        source_set: None,
        directory_package: None,
    }]);
    let analysis = host.analysis();
    // Every check, not only syntax: whatever asks before the model loads
    // sees a workspace without source sets.
    let diagnostics = analysis
        .diagnostics(file, &crate::DiagnosticsConfig::default())
        .unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    analysis.semantic_tokens(file).unwrap();
    analysis.document_symbols(file).unwrap();
}

/// The ranges "expand selection" grows through from `$0`, one per line.
fn check_selection(fixture: &str, expected: Expect) {
    let fixture = Fixture::parse(fixture);
    let (file, offset) = fixture.cursor();
    let text = fixture.vfs.contents(file).unwrap().clone();
    let ranges = analysis(&fixture)
        .selection_ranges(file, &[offset])
        .unwrap()
        .remove(0);
    for pair in ranges.windows(2) {
        assert!(pair[1].contains_range(pair[0]) && pair[1] != pair[0]);
    }
    let described: Vec<String> = ranges
        .iter()
        .map(|range| text[range.as_range()].replace('\n', "⏎"))
        .take_while(|selected| !selected.starts_with("package"))
        .collect();
    expected.assert_eq(&described.join("\n"));
}

#[test]
fn selection_grows_from_the_word_through_what_brackets_hold_to_the_declaration() {
    check_selection(
        r#"
//- /app/b/Use.kt
package b

fun total(prices: List<Int>): Int {
    return prices.sumOf { price -> price * 2 } + 1
}
"#
        .replace("price * 2", "pri$0ce * 2")
        .as_str(),
        expect![[r#"
            price
            price * 2
            price -> price * 2
            { price -> price * 2 }
            prices.sumOf { price -> price * 2 }
            prices.sumOf { price -> price * 2 } + 1
            return prices.sumOf { price -> price * 2 } + 1
            {⏎    return prices.sumOf { price -> price * 2 } + 1⏎}
            fun total(prices: List<Int>): Int {⏎    return prices.sumOf { price -> price * 2 } + 1⏎}"#]],
    );
}

#[test]
fn selection_at_a_words_end_starts_with_the_word_and_takes_a_strings_contents() {
    check_selection(
        r#"
//- /app/b/Use.kt
package b

val greeting = listOf("hello there", name$0)
"#,
        expect![[r#"
            name
            "hello there", name
            ("hello there", name)
            listOf("hello there", name)
            val greeting = listOf("hello there", name)"#]],
    );
    check_selection(
        r#"
//- /app/b/Use.kt
package b

val greeting = "hello th$0ere"
"#,
        expect![[r#"
            there
            hello there
            "hello there"
            val greeting = "hello there""#]],
    );
}

/// `line: kind` for each lens of `path`'s file.
fn lenses_of(fixture: &Fixture, path: &str) -> String {
    let file = fixture.file(path);
    let text = fixture.vfs.contents(file).unwrap().clone();
    let lenses = analysis(fixture).code_lenses(file).unwrap();
    lenses
        .iter()
        .map(|lens| {
            let name = &text[lens.range.as_range()];
            match &lens.kind {
                crate::CodeLensKind::References => format!("{name}: references"),
                crate::CodeLensKind::RunTests { target, filter } => {
                    format!("{name}: run {target:?} {filter}")
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_lenses_are_on_functions_annotated_as_tests_and_the_classes_holding_them() {
    let fixture = Fixture::parse(
        r#"
//- /lib/org/junit/jupiter/api/Test.kt
package org.junit.jupiter.api

annotation class Test
//- /app/fake/Test.kt
package app.fake

annotation class Test
//- /app/orders/OrderTest.kt
package app.orders

import org.junit.jupiter.api.Test

class OrderTest {
    @Test
    fun `creates an order`() {}

    fun helper() {}

    class Refunds {
        @org.junit.jupiter.api.Test
        fun refundsInFull() {}
    }
}

val shared = 1
//- /app/orders/Lookalike.kt
package app.orders

import app.fake.Test

class Lookalike {
    @Test
    fun notATest() {}
}
//- /app/orders/WithoutJUnit.kt
package app.orders

import kotlin.test.Test as KotlinTest

class WithoutJUnit {
    @KotlinTest
    fun runsWithoutTheLibrary() {}
}
"#,
    );
    expect![[r#"
        OrderTest: run Class app.orders.OrderTest
        OrderTest: references
        `creates an order`: run Function app.orders.OrderTest.creates an order
        `creates an order`: references
        helper: references
        Refunds: run Class app.orders.OrderTest$Refunds
        Refunds: references
        refundsInFull: run Function app.orders.OrderTest$Refunds.refundsInFull
        refundsInFull: references
        shared: references"#]]
    .assert_eq(&lenses_of(&fixture, "/app/orders/OrderTest.kt"));
    expect![[r#"
        Lookalike: references
        notATest: references"#]]
    .assert_eq(&lenses_of(&fixture, "/app/orders/Lookalike.kt"));
    expect![[r#"
        WithoutJUnit: run Class app.orders.WithoutJUnit
        WithoutJUnit: references
        runsWithoutTheLibrary: run Function app.orders.WithoutJUnit.runsWithoutTheLibrary
        runsWithoutTheLibrary: references"#]]
    .assert_eq(&lenses_of(&fixture, "/app/orders/WithoutJUnit.kt"));
}
