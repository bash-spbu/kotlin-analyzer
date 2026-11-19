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
