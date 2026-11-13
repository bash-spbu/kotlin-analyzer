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
    check_signature_help(
        &fixture.replace("CALL", "pay($0)"),
        expect![[r#"
        > pay([amount: Long])
          pay(amount: Long, note: String, vararg tags: String)"#]],
    );
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
}

#[test]
fn semantic_tokens_tell_properties_locals_parameters_and_classes_apart() {
    let fixture = Fixture::with_mini_stdlib(
        r#"
//- /app/b/Use.kt
package b
class Account(val owner: String) {
    var balance: Long = 0
    fun describe(prefix: String): String {
        val label = prefix + owner
        return label
    }
}
"#,
    );
    let file = fixture.file("/app/b/Use.kt");
    let text = fixture.vfs.contents(file).unwrap().clone();
    let tokens = analysis(&fixture).semantic_tokens(file).unwrap();
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
    expect![[r#"
        Account Class declaration
        owner Property declaration,readonly
        String Class 
        balance Property declaration
        Long Class 
        describe Method declaration
        prefix Parameter declaration,readonly
        String Class 
        String Class 
        label Variable declaration,readonly
        prefix Parameter readonly
        owner Property readonly
        label Variable readonly"#]]
    .assert_eq(&rendered.join("\n"));
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
