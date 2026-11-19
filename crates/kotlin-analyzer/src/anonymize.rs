//! `kotlin-analyzer anonymize <file>`: a Kotlin or Java file with its own
//! names, strings and comments replaced — for a user to send when a log
//! isn't enough to reproduce a problem. The structure stays as it was
//! (the same syntax tree, node for node); names the language and the
//! common libraries give (`listOf`, `String`, what's imported from
//! `kotlin.*`, `java.*`, Spring…) stay too, as the analysis needs them to
//! resolve the same way. The same name always gets the same stand-in.
//! The user reads the result before sending it.

use std::path::Path;
use std::process::ExitCode;

use rustc_hash::{FxHashMap, FxHashSet};
use syntax::{Language, SyntaxNode, named_children};

pub(crate) fn command(arguments: &[String]) -> ExitCode {
    let Some(path) = arguments.first() else {
        eprintln!("usage: kotlin-analyzer anonymize <file.kt|file.java>");
        return ExitCode::FAILURE;
    };
    let path = Path::new(path);
    let Some(language) = Language::of_path(path) else {
        eprintln!("kotlin-analyzer: anonymize reads .kt, .kts and .java files");
        return ExitCode::FAILURE;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("kotlin-analyzer: cannot read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    print!("{}", anonymize(&text, language));
    eprintln!(
        "kotlin-analyzer: names, strings and comments replaced — read it through before sending"
    );
    ExitCode::SUCCESS
}

/// Packages whose names stay: the language's, the JDK's, common
/// frameworks'.
const PUBLIC_PACKAGES: &[&str] = &[
    "kotlin",
    "kotlinx",
    "java",
    "javax",
    "jakarta",
    "org.springframework",
    "org.junit",
    "org.assertj",
    "io.mockk",
    "org.mockito",
    "com.fasterxml.jackson",
    "org.slf4j",
    "reactor",
    "io.ktor",
    "androidx",
    "android",
];

/// Names that stay wherever they are: the standard library's most used,
/// and soft keywords the grammar reads as names.
const PUBLIC_NAMES: &[&str] = &[
    "it",
    "this",
    "super",
    "field",
    "value",
    "get",
    "set",
    "by",
    "init",
    "data",
    "Any",
    "Unit",
    "Nothing",
    "String",
    "CharSequence",
    "Char",
    "Boolean",
    "Byte",
    "Short",
    "Int",
    "Long",
    "Float",
    "Double",
    "Number",
    "Comparable",
    "Array",
    "IntArray",
    "LongArray",
    "ByteArray",
    "List",
    "MutableList",
    "Set",
    "MutableSet",
    "Map",
    "MutableMap",
    "Collection",
    "MutableCollection",
    "Iterable",
    "Iterator",
    "Sequence",
    "Pair",
    "Triple",
    "Result",
    "Lazy",
    "Throwable",
    "Exception",
    "RuntimeException",
    "Error",
    "IllegalArgumentException",
    "IllegalStateException",
    "NullPointerException",
    "UnsupportedOperationException",
    "IndexOutOfBoundsException",
    "NoSuchElementException",
    "Enum",
    "Annotation",
    "Deprecated",
    "Suppress",
    "JvmStatic",
    "JvmField",
    "JvmOverloads",
    "JvmName",
    "Volatile",
    "Synchronized",
    "Throws",
    "Override",
    "FunctionalInterface",
    "Object",
    "Integer",
    "Optional",
    "StringBuilder",
    "Math",
    "System",
    "Thread",
    "Runnable",
    "Callable",
    "UUID",
    "BigDecimal",
    "BigInteger",
    "Instant",
    "Duration",
    "LocalDate",
    "LocalDateTime",
    "ZonedDateTime",
    "OffsetDateTime",
    "ZoneId",
    "ZoneOffset",
    "Clock",
    "listOf",
    "mutableListOf",
    "arrayListOf",
    "emptyList",
    "listOfNotNull",
    "setOf",
    "mutableSetOf",
    "hashSetOf",
    "emptySet",
    "mapOf",
    "mutableMapOf",
    "hashMapOf",
    "emptyMap",
    "arrayOf",
    "emptyArray",
    "sequenceOf",
    "buildList",
    "buildMap",
    "buildSet",
    "buildString",
    "lazy",
    "require",
    "requireNotNull",
    "check",
    "checkNotNull",
    "error",
    "TODO",
    "println",
    "print",
    "repeat",
    "run",
    "let",
    "also",
    "apply",
    "with",
    "takeIf",
    "takeUnless",
    "use",
    "to",
    "map",
    "mapNotNull",
    "mapIndexed",
    "flatMap",
    "filter",
    "filterNot",
    "filterNotNull",
    "filterIsInstance",
    "forEach",
    "forEachIndexed",
    "first",
    "firstOrNull",
    "last",
    "lastOrNull",
    "single",
    "singleOrNull",
    "find",
    "any",
    "all",
    "none",
    "count",
    "sum",
    "sumOf",
    "maxOf",
    "minOf",
    "maxBy",
    "minBy",
    "maxByOrNull",
    "minByOrNull",
    "sorted",
    "sortedBy",
    "sortedByDescending",
    "sortedWith",
    "reversed",
    "distinct",
    "distinctBy",
    "groupBy",
    "associate",
    "associateBy",
    "associateWith",
    "partition",
    "zip",
    "chunked",
    "windowed",
    "fold",
    "reduce",
    "joinToString",
    "toList",
    "toMutableList",
    "toSet",
    "toMutableSet",
    "toMap",
    "toMutableMap",
    "toTypedArray",
    "toString",
    "toInt",
    "toLong",
    "toDouble",
    "toBigDecimal",
    "toInstant",
    "equals",
    "hashCode",
    "compareTo",
    "copy",
    "component1",
    "component2",
    "isEmpty",
    "isNotEmpty",
    "isNullOrEmpty",
    "isBlank",
    "isNotBlank",
    "isNullOrBlank",
    "orEmpty",
    "size",
    "length",
    "contains",
    "containsKey",
    "add",
    "addAll",
    "remove",
    "removeAll",
    "clear",
    "put",
    "putAll",
    "getOrDefault",
    "getOrElse",
    "getOrPut",
    "getValue",
    "keys",
    "values",
    "entries",
    "key",
    "indices",
    "plus",
    "minus",
    "times",
    "div",
    "rem",
    "invoke",
    "iterator",
    "next",
    "hasNext",
    "message",
    "cause",
    "name",
    "ordinal",
    "javaClass",
    "java",
    "kotlin",
    "class",
    "trim",
    "split",
    "substring",
    "startsWith",
    "endsWith",
    "replace",
    "lowercase",
    "uppercase",
    "format",
    "now",
    "of",
    "ofNullable",
    "isPresent",
    "orElse",
    "orElseThrow",
    "ifPresent",
    "stream",
    "collect",
    "Collectors",
    "Objects",
    "Arrays",
    "Collections",
    "Test",
    "BeforeEach",
    "AfterEach",
    "Nested",
    "DisplayName",
    "assertThat",
    "assertEquals",
    "assertTrue",
    "assertFalse",
    "assertThrows",
    "every",
    "verify",
    "mockk",
    "coEvery",
    "coVerify",
    "returns",
    "just",
    "Runs",
    "slot",
    "capture",
    "any",
    "eq",
];

pub(crate) fn anonymize(text: &str, language: Language) -> String {
    let parse = syntax::parse(text.into(), language);
    let root = parse.root();
    let mut kept: FxHashSet<String> = PUBLIC_NAMES.iter().map(|name| (*name).to_owned()).collect();
    let mut kept_imports: Vec<(usize, usize)> = Vec::new();
    for import in syntax::descendants(root)
        .filter(|node| matches!(node.kind(), "import_header" | "import_declaration"))
    {
        let path: String = parse
            .text_of(import)
            .trim_start_matches("import")
            .trim_start_matches(" static")
            .trim()
            .trim_end_matches(';')
            .to_owned();
        let public = PUBLIC_PACKAGES
            .iter()
            .any(|package| path == *package || path.starts_with(&format!("{package}.")));
        if !public {
            continue;
        }
        kept_imports.push((import.start_byte(), import.end_byte()));
        // What it imports keeps its name in the file (`as` alias too).
        if let Some(last) = path
            .split(" as ")
            .last()
            .and_then(|imported| imported.rsplit('.').next())
        {
            kept.insert(last.trim().to_owned());
        }
    }
    let mut stand_ins = StandIns::default();
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    let in_kept_import = |node: SyntaxNode<'_>| {
        kept_imports
            .iter()
            .any(|(start, end)| *start <= node.start_byte() && node.end_byte() <= *end)
    };
    for node in syntax::descendants(root) {
        let original = parse.text_of(node);
        let replacement = match (language, node.kind()) {
            (_, "line_comment") => Some(String::new()),
            (_, "multiline_comment" | "block_comment") => Some("/* */".to_owned()),
            (Language::Kotlin, "string_content") | (Language::Java, "string_fragment") => {
                Some(stand_ins.string(original))
            }
            (_, "character_literal") => Some("'c'".to_owned()),
            (
                Language::Kotlin,
                "simple_identifier" | "type_identifier" | "interpolated_identifier",
            )
            | (Language::Java, "identifier" | "type_identifier") => {
                let name = original.trim_matches('`');
                let keep = kept.contains(name) || in_kept_import(node);
                (!keep).then(|| stand_ins.name(name))
            }
            _ => None,
        };
        let is_leaf = named_children(node).next().is_none();
        if let (Some(replacement), true) = (replacement, is_leaf) {
            replacements.push((node.start_byte(), node.end_byte(), replacement));
        }
    }
    replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut anonymized = text.to_owned();
    for (start, end, replacement) in replacements {
        anonymized.replace_range(start..end, &replacement);
    }
    anonymized
}

/// The stand-in of each name and string met: the same one each time.
#[derive(Default)]
struct StandIns {
    names: FxHashMap<String, String>,
    strings: FxHashMap<String, String>,
}

impl StandIns {
    /// `Type7` for a capitalized name, `name7` else.
    fn name(&mut self, name: &str) -> String {
        let next = self.names.len() + 1;
        self.names
            .entry(name.to_owned())
            .or_insert_with(|| {
                let capitalized = name.chars().next().is_some_and(char::is_uppercase);
                if capitalized {
                    format!("Type{next}")
                } else {
                    format!("name{next}")
                }
            })
            .clone()
    }

    /// `<s7>` — never a letter first, so it doesn't run on into a `$name`
    /// before it. Text without a letter or digit (spaces, punctuation)
    /// tells nothing: it stays.
    fn string(&mut self, text: &str) -> String {
        if !text.chars().any(char::is_alphanumeric) {
            return text.to_owned();
        }
        let next = self.strings.len() + 1;
        self.strings
            .entry(text.to_owned())
            .or_insert_with(|| format!("<s{next}>"))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYROLL: &str = r#"package com.acme.secret
import com.acme.secret.billing.Invoice
import java.time.Instant
/** Doc about secrets */
class Payroll(private val ledger: Ledger) {
    // salary of the CEO
    fun total(names: List<String>, invoice: Invoice): Int =
        names.map { "Hello, $it ${ledger.owner}" }.size + ledger.owner.length + 42
    val stamp: Instant = Instant.now()
    val c = 'q'
}
"#;

    #[test]
    fn its_own_names_strings_and_comments_go_and_the_libraries_stay() {
        let anonymized = anonymize(PAYROLL, Language::Kotlin);
        for secret in [
            "acme", "secret", "billing", "Invoice", "Payroll", "ledger", "Ledger", "owner",
            "total", "names", "stamp", "Hello", "CEO", "salary", "Doc", "'q'",
        ] {
            assert!(
                !anonymized.contains(secret),
                "{secret} left in:\n{anonymized}"
            );
        }
        for public in [
            "java.time.Instant",
            "Instant.now()",
            "List<String>",
            "map",
            "size",
            "$it",
        ] {
            assert!(anonymized.contains(public), "{public} lost:\n{anonymized}");
        }
        // The same name gets the same stand-in each time.
        assert_eq!(anonymized.matches("name2.name3").count(), 2, "{anonymized}");
    }

    #[test]
    fn the_syntax_tree_stays_node_for_node() {
        let kinds = |text: &str| -> Vec<&'static str> {
            let parse = syntax::parse(text.into(), Language::Kotlin);
            syntax::descendants(parse.root())
                .map(|node| node.kind())
                .filter(|kind| !kind.ends_with("comment"))
                .collect()
        };
        let anonymized = anonymize(PAYROLL, Language::Kotlin);
        assert_eq!(kinds(PAYROLL), kinds(&anonymized));
        let parse = syntax::parse(anonymized.as_str().into(), Language::Kotlin);
        assert!(parse.errors().is_empty(), "{anonymized}");
    }
}
