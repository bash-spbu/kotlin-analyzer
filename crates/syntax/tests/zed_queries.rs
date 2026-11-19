//! The Zed extension's Kotlin queries (`editors/zed/languages/kotlin`)
//! compile against the Kotlin grammar and find what they're for. The
//! extension pins its own revision of the grammar (the one Zed's Java
//! extension uses too); this checks them against the server's, a close
//! fork, so a pattern the grammar can't match is caught here.

use std::collections::BTreeSet;
use std::path::PathBuf;

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

const SAMPLE: &str = r#"
@file:JvmName("Sample")
package app.payments

import kotlin.collections.List

/** Money, in minor units. */
@JvmInline
value class Money(val amount: Long)

enum class Status { ACTIVE, CLOSED }

interface Repository<T> {
    fun find(id: Long): T?
}

class Service(private val repository: Repository<Money>) : Base() {
    companion object {
        const val LIMIT = 10
    }

    var count: Int = 0

    @Deprecated("old")
    fun total(values: List<Money>): Long {
        val sum = values.sumOf { it.amount }
        when (status()) {
            Status.ACTIVE -> println("active $sum ${values.size}")
            else -> return 0
        }
        return this.count + sum + LIMIT
    }
}

fun main() = Service(repository = TODO()).total(listOf())
"#;

fn queries_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editors/zed/languages/kotlin")
}

fn captures(language: &Language, file: &str) -> BTreeSet<String> {
    let path = queries_directory().join(format!("{file}.scm"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let query = Query::new(language, &text).unwrap_or_else(|error| panic!("{file}.scm: {error}"));
    let mut parser = Parser::new();
    parser.set_language(language).unwrap();
    let tree = parser.parse(SAMPLE, None).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), SAMPLE.as_bytes());
    let mut found = BTreeSet::new();
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            let name = query.capture_names()[capture.index as usize];
            let text = &SAMPLE[capture.node.byte_range()];
            found.insert(format!("{name}={}", text.lines().next().unwrap_or("")));
        }
    }
    found
}

#[test]
fn the_extension_queries_compile_and_capture_what_they_are_for() {
    let language: Language = tree_sitter_kotlin::LANGUAGE.into();
    let highlights = captures(&language, "highlights");
    for expected in [
        "keyword=class",
        "keyword=fun",
        // `enum` is a token of its own, not a class modifier.
        "keyword=enum",
        "type=Money",
        "function=total",
        "function=sumOf",
        "property=count",
        "constant=LIMIT",
        "constant=ACTIVE",
        "attribute=Deprecated",
        "string=\"old\"",
        "comment.doc=/** Money, in minor units. */",
        "variable.special=this",
        "constructor=Service",
        "number=10",
    ] {
        assert!(
            highlights.contains(expected),
            "highlights miss {expected}: {highlights:?}"
        );
    }
    let outline = captures(&language, "outline");
    for expected in [
        "name=Service",
        "name=total",
        "name=count",
        "name=LIMIT",
        "name=ACTIVE",
        "name=main",
        "name=amount",
    ] {
        assert!(
            outline.contains(expected),
            "outline misses {expected}: {outline:?}"
        );
    }
    assert!(
        !outline.contains("name=sum"),
        "a local is not in the outline: {outline:?}"
    );
    let brackets = captures(&language, "brackets");
    assert!(
        brackets.contains("open={") && brackets.contains("open=<"),
        "{brackets:?}"
    );
    assert!(!captures(&language, "indents").is_empty());
    // A function's node starts with its annotations.
    let textobjects = captures(&language, "textobjects");
    assert!(
        textobjects.contains("function.around=@Deprecated(\"old\")"),
        "{textobjects:?}"
    );
    assert!(textobjects.contains("class.inside={"), "{textobjects:?}");
}

const TEST_SAMPLE: &str = r#"
package app.orders

import org.junit.jupiter.api.Test

class OrderTest {
    @Test
    fun `creates an order`() {}

    @ParameterizedTest(name = "{0}")
    fun parameterized(value: Int) {}

    @Deprecated("not a test")
    fun helper() {}

    @Nested
    inner class Refunds {
        @org.junit.jupiter.api.Test
        fun refundsInFull() {}
    }
}

class Plain {
    fun notATest() {}
}
"#;

/// Each runnable in `TEST_SAMPLE`: its tag and the captures its task reads.
fn runnables(language: &Language) -> BTreeSet<String> {
    let path = queries_directory().join("runnables.scm");
    let text = std::fs::read_to_string(&path).unwrap();
    let query =
        Query::new(language, &text).unwrap_or_else(|error| panic!("runnables.scm: {error}"));
    let mut parser = Parser::new();
    parser.set_language(language).unwrap();
    let tree = parser.parse(TEST_SAMPLE, None).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), TEST_SAMPLE.as_bytes());
    let mut found = BTreeSet::new();
    while let Some(matched) = matches.next() {
        let tag = query
            .property_settings(matched.pattern_index)
            .iter()
            .find(|setting| &*setting.key == "tag")
            .and_then(|setting| setting.value.as_deref())
            .unwrap_or("untagged")
            .to_owned();
        let mut variables: Vec<String> = matched
            .captures
            .iter()
            .map(|capture| (query.capture_names()[capture.index as usize], capture.node))
            .filter(|(name, _)| !name.starts_with('_'))
            .map(|(name, node)| format!("{name}={}", &TEST_SAMPLE[node.byte_range()]))
            .collect();
        variables.sort();
        found.insert(format!("{tag}: {}", variables.join(" ")));
    }
    found
}

#[test]
fn the_runnables_find_test_functions_and_the_classes_holding_them() {
    let language: Language = tree_sitter_kotlin::LANGUAGE.into();
    let found: Vec<String> = runnables(&language).into_iter().collect();
    assert_eq!(
        found,
        [
            "kotlin-test-class: kotlin_class_name=OrderTest kotlin_package_name=app.orders run=OrderTest",
            "kotlin-test-method: kotlin_class_name=OrderTest kotlin_method_name=`creates an order` kotlin_package_name=app.orders run=`creates an order`",
            "kotlin-test-method: kotlin_class_name=OrderTest kotlin_method_name=parameterized kotlin_package_name=app.orders run=parameterized",
            "kotlin-test-nested-class: kotlin_class_name=Refunds kotlin_outer_class_name=OrderTest kotlin_package_name=app.orders run=Refunds",
            "kotlin-test-nested-method: kotlin_class_name=Refunds kotlin_method_name=refundsInFull kotlin_outer_class_name=OrderTest kotlin_package_name=app.orders run=refundsInFull",
        ]
    );
}

const INJECTION_SAMPLE: &str = r#"
interface MarkupRepository {
    @Query("select m from Markup m where m.id = :id")
    fun one(id: Long): Markup

    @Query(value = """select * from markup""", nativeQuery = true)
    fun all(): List<Markup>

    @Named("not sql")
    fun other(): Markup
}

val digits = Regex("[0-9]+")
val words = "\\w+".toRegex()
val compiled = Pattern.compile("a|b")
val plain = listOf("not a regex")
// TODO: tidy
"#;

/// What each pattern of `file.scm` captures in `sample`, as
/// `<property>: <capture>=<text>`, `property` being the pattern's `key`.
fn captures_with_property(language: &Language, file: &str, sample: &str, key: &str) -> Vec<String> {
    let path = queries_directory().join(format!("{file}.scm"));
    let text = std::fs::read_to_string(&path).unwrap();
    let query = Query::new(language, &text).unwrap_or_else(|error| panic!("{file}.scm: {error}"));
    let mut parser = Parser::new();
    parser.set_language(language).unwrap();
    let tree = parser.parse(sample, None).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), sample.as_bytes());
    let mut found = Vec::new();
    while let Some(matched) = matches.next() {
        let property = query
            .property_settings(matched.pattern_index)
            .iter()
            .find(|setting| &*setting.key == key)
            .and_then(|setting| setting.value.as_deref())
            .unwrap_or("-")
            .to_owned();
        for capture in matched.captures {
            let name = query.capture_names()[capture.index as usize];
            if !name.starts_with('_') {
                let captured = &sample[capture.node.byte_range()];
                found.push(format!(
                    "{property}: {name}={}",
                    captured.lines().next().unwrap_or("")
                ));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn injections_put_sql_in_query_annotations_regex_in_regex_calls_and_comments_in_comments() {
    let language: Language = tree_sitter_kotlin::LANGUAGE.into();
    assert_eq!(
        captures_with_property(
            &language,
            "injections",
            INJECTION_SAMPLE,
            "injection.language"
        ),
        [
            "comment: injection.content=// TODO: tidy",
            "regex: injection.content=[0-9]+",
            "regex: injection.content=\\\\w+",
            "regex: injection.content=a|b",
            "sql: injection.content=select * from markup",
            "sql: injection.content=select m from Markup m where m.id = :id",
        ]
    );
}

/// Each match of `injections.scm` in `sample` that sets a language: the
/// language and its content's pieces, as Zed parses them together.
fn injected_texts(sample: &str) -> Vec<String> {
    let language: Language = tree_sitter_kotlin::LANGUAGE.into();
    let path = queries_directory().join("injections.scm");
    let text = std::fs::read_to_string(&path).unwrap();
    let query =
        Query::new(&language, &text).unwrap_or_else(|error| panic!("injections.scm: {error}"));
    let content = query
        .capture_index_for_name("injection.content")
        .expect("an injection.content capture");
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(sample, None).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), sample.as_bytes());
    let mut found = Vec::new();
    while let Some(matched) = matches.next() {
        let Some(injected) = query
            .property_settings(matched.pattern_index)
            .iter()
            .find(|setting| &*setting.key == "injection.language")
            .and_then(|setting| setting.value.as_deref())
        else {
            continue;
        };
        let pieces: Vec<&str> = matched
            .nodes_for_capture_index(content)
            .map(|node| &sample[node.byte_range()])
            .collect();
        found.push(format!("{injected}: {}", pieces.join("|")));
    }
    found.retain(|injection| !injection.starts_with("comment: "));
    found
}

#[test]
fn sql_is_injected_where_a_string_reads_as_sql_or_is_marked_as_sql() {
    let sample = r#"
class Repository(private val jdbc: JdbcClient) {
    fun sums(id: Long) = jdbc.sql(
        """
        WITH filtered AS (SELECT code FROM $table WHERE id = ${id})
        SELECT * FROM filtered
        """.trimIndent()
    )

    fun rename() = jdbcTemplate.update("update markup set name = ?", name)

    // language=SQL
    val ddl = "CREATE TABLE t (id int)"

    /* language=sql */
    val marked = "select 1 from t"

    fun alter() = jdbc.sql(
        // language=SQL
        "ALTER TABLE t ADD c int"
    )

    @Query("from Markup m")
    fun all(): List<Markup>

    val failure = "Update failed for $id"
    val prompt = "select one of: a, b"
    // language=JSON
    val json = "{}"
}
"#;
    assert_eq!(
        injected_texts(sample),
        [
            // The pieces between the templates, together.
            "sql: \n        WITH filtered AS (SELECT code FROM | WHERE id = |)\n        SELECT * FROM filtered\n        ",
            "sql: update markup set name = ?",
            // Marked: what doesn't read as SQL by itself.
            "sql: CREATE TABLE t (id int)",
            // Marked and reading as SQL: injected once.
            "sql: select 1 from t",
            "sql: ALTER TABLE t ADD c int",
            "sql: from Markup m",
        ]
    );
}

#[test]
fn overrides_scope_strings_and_comments() {
    let language: Language = tree_sitter_kotlin::LANGUAGE.into();
    let found = captures_with_property(&language, "overrides", INJECTION_SAMPLE, "-");
    assert!(
        found.contains(&"-: string=\"[0-9]+\"".to_owned()),
        "{found:?}"
    );
    assert!(
        found.contains(&"-: comment.inclusive=// TODO: tidy".to_owned()),
        "{found:?}"
    );
}

#[test]
fn the_zed_extensions_label_code_parses_and_highlights_its_names() {
    // What editors/zed/src/labels.rs writes for a function, a value, a type
    // and an enum entry.
    let language: Language = tree_sitter_kotlin::LANGUAGE.into();
    let path = queries_directory().join("highlights.scm");
    let query = Query::new(&language, &std::fs::read_to_string(path).unwrap()).unwrap();
    for (code, name, expected) in [
        (
            "fun total(values: List<Money>): Long {}",
            "total",
            "function",
        ),
        ("val count: Int", "count", "property"),
        ("class Money", "Money", "type"),
        ("enum class Entries { ACTIVE }", "ACTIVE", "constant"),
    ] {
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(code, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{code}: {}",
            tree.root_node().to_sexp()
        );
        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(&query, tree.root_node(), code.as_bytes());
        let mut found = Vec::new();
        while let Some((matched, index)) = captures.next() {
            let capture = matched.captures[*index];
            if &code[capture.node.byte_range()] == name {
                found.push(query.capture_names()[capture.index as usize]);
            }
        }
        assert!(found.contains(&expected), "{code}: {name} is {found:?}");
    }
}

const DEBUGGER_SAMPLE: &str = r#"
fun total(values: List<Int>, limit: Int): Int {
    var sum = 0
    for (value in values) {
        sum += value * 2
    }
    val (first, second) = pair
    values.forEach { item -> println("item $item ${item + limit}") }
    sum = sum + limit
    sum = limit
    if (sum > limit && first != null) return sum
    return this.count + values.size + Totals.LIMIT
}
"#;

#[test]
fn the_debugger_query_names_variables_not_calls_members_or_classes() {
    let language: Language = tree_sitter_kotlin::LANGUAGE.into();
    let found = captures_with_property(&language, "debugger", DEBUGGER_SAMPLE, "-");
    let mut variables = std::collections::BTreeMap::<&str, usize>::new();
    for capture in &found {
        if let Some(name) = capture.strip_prefix("-: debug-variable=") {
            *variables.entry(name).or_default() += 1;
        }
    }
    // Each place a name is read or written, members, calls and classes
    // left out.
    assert_eq!(
        variables,
        std::collections::BTreeMap::from([
            ("values", 4),
            ("limit", 5),
            ("sum", 7),
            ("value", 2),
            ("first", 2),
            ("second", 1),
            ("pair", 1),
            ("item", 3),
            ("this", 1),
        ])
    );
    let scopes = found
        .iter()
        .filter(|capture| capture.starts_with("-: debug-scope="))
        .count();
    // The function's body, the loop and its body, the lambda, the `if`'s.
    assert_eq!(scopes, 5, "{found:?}");
}
