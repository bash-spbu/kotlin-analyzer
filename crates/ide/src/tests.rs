use base_db::fixture::Fixture;
use expect_test::{Expect, expect};

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
    let fixture = Fixture::with_mini_stdlib(fixture);
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
    val sum = money + money
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
            [non-exhaustive-when] 9: `when` must be exhaustive: add the missing `SELL` or an `else` branch
            [unresolved-reference] 10: Unresolved reference `missing`"#]],
    );
}
