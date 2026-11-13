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
