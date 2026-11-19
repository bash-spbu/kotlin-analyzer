//! Reading real compiled code: the Kotlin standard library from the Gradle
//! cache (skipped when absent) and a Java class compiled by `javac`.

use std::path::{Path, PathBuf};

use crate::{
    ClassContents, ClassKind, DeprecationLevel, DeprecationOrigin, JarContents, LibraryClass,
    LibraryType, Member, MemberKind, OptInLevel, PackageMembers, TypeArgument, read_class,
    read_jar,
};

/// The newest cached compiled JAR of `group:artifact`.
fn cached_jar(group: &str, artifact: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let root = Path::new(&home)
        .join(".gradle/caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);
    let mut jars: Vec<PathBuf> = std::fs::read_dir(root)
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

fn render(type_ref: &LibraryType) -> String {
    match type_ref {
        LibraryType::Class {
            name,
            arguments,
            nullable,
        } => {
            let arguments: Vec<String> = arguments
                .iter()
                .map(|argument| match argument {
                    TypeArgument::Star => "*".to_owned(),
                    TypeArgument::Type { type_ref, .. } => render(type_ref),
                })
                .collect();
            let generics = if arguments.is_empty() {
                String::new()
            } else {
                format!("<{}>", arguments.join(", "))
            };
            format!(
                "{}{generics}{}",
                name.qualified(),
                if *nullable { "?" } else { "" }
            )
        }
        LibraryType::TypeParameter { name, nullable } => {
            format!("{name}{}", if *nullable { "?" } else { "" })
        }
        LibraryType::Function {
            receiver,
            parameters,
            result,
            suspend,
            ..
        } => {
            let parameters: Vec<String> = parameters.iter().map(render).collect();
            let receiver = receiver
                .as_ref()
                .map(|receiver| format!("{}.", render(receiver)))
                .unwrap_or_default();
            format!(
                "{}{receiver}({}) -> {}",
                if *suspend { "suspend " } else { "" },
                parameters.join(", "),
                render(result)
            )
        }
        LibraryType::Primitive(name) => name.to_string(),
        LibraryType::Array(element) => format!("{}[]", render(element)),
        LibraryType::Unreadable => "?".to_owned(),
    }
}

fn signature(member: &Member) -> String {
    let parameters: Vec<String> = member
        .parameters
        .iter()
        .map(|parameter| {
            format!(
                "{}{}: {}{}",
                if parameter.vararg { "vararg " } else { "" },
                parameter.name,
                render(&parameter.type_ref),
                if parameter.has_default { " = …" } else { "" }
            )
        })
        .collect();
    let receiver = member
        .receiver
        .as_ref()
        .map(|receiver| format!("{}.", render(receiver)))
        .unwrap_or_default();
    let result = member
        .return_type
        .as_ref()
        .map(|result| format!(": {}", render(result)))
        .unwrap_or_default();
    if let MemberKind::Property { .. } = member.kind {
        return format!("{receiver}{}{result}", member.name);
    }
    format!(
        "{receiver}{}({}){result}",
        member.name,
        parameters.join(", ")
    )
}

fn class<'contents>(contents: &'contents JarContents, qualified: &str) -> &'contents LibraryClass {
    contents
        .classes
        .iter()
        .find(|class| class.name.qualified() == qualified)
        .unwrap_or_else(|| panic!("no class {qualified}"))
}

fn top_level<'contents>(
    contents: &'contents JarContents,
    package: &str,
    name: &str,
) -> Vec<&'contents Member> {
    contents
        .package_members
        .iter()
        .filter(|members: &&PackageMembers| members.package.to_string() == package)
        .flat_map(|members| members.members.iter())
        .filter(|member| member.name.as_str() == name)
        .collect()
}

#[test]
fn the_kotlin_standard_library_reads_with_exact_signatures() {
    let Some(jar) = cached_jar("org.jetbrains.kotlin", "kotlin-stdlib") else {
        eprintln!("skipped: no kotlin-stdlib in the Gradle cache");
        return;
    };
    let contents = read_jar(&jar).expect("kotlin-stdlib reads");

    // Built-ins have no class files: they come from `.kotlin_builtins`.
    let string = class(&contents, "kotlin.String");
    assert!(
        string
            .members
            .iter()
            .any(|member| member.name.as_str() == "length")
    );
    let list = class(&contents, "kotlin.collections.List");
    assert_eq!(list.kind, ClassKind::Interface);
    assert_eq!(
        render(&list.supertypes[0]),
        "kotlin.collections.Collection<E>"
    );

    let list_of: Vec<String> = top_level(&contents, "kotlin.collections", "listOf")
        .into_iter()
        .map(signature)
        .collect();
    assert!(
        list_of.contains(&"listOf(vararg elements: T): kotlin.collections.List<T>".to_owned()),
        "{list_of:?}"
    );

    let join = top_level(&contents, "kotlin.collections", "joinToString");
    let iterable_join =
        join.iter()
            .find(|member| {
                member.receiver.as_ref().is_some_and(|receiver| {
                    render(receiver).starts_with("kotlin.collections.Iterable")
                })
            })
            .expect("Iterable.joinToString");
    assert!(
        iterable_join
            .parameters
            .iter()
            .all(|parameter| parameter.has_default)
    );

    let apply = top_level(&contents, "kotlin", "apply");
    assert_eq!(
        apply
            .iter()
            .map(|member| signature(member))
            .collect::<Vec<_>>(),
        vec!["T.apply(block: T.() -> kotlin.Unit): T"]
    );

    // A function deprecated with level `HIDDEN` is left out: `Iterable.maxBy` once returned `T?`, now `T`.
    let max_by: Vec<String> = top_level(&contents, "kotlin.collections", "maxBy")
        .into_iter()
        .filter(|member| {
            member
                .receiver
                .as_ref()
                .is_some_and(|receiver| render(receiver).starts_with("kotlin.collections.Iterable"))
        })
        .map(signature)
        .collect();
    assert!(
        !max_by.is_empty() && max_by.iter().all(|signature| signature.ends_with(": T")),
        "{max_by:?}"
    );
    // A file facade in `kotlin/time/jdk8/` declares package `kotlin.time`.
    assert!(!top_level(&contents, "kotlin.time", "toKotlinDuration").is_empty());
    assert!(top_level(&contents, "kotlin.time.jdk8", "toKotlinDuration").is_empty());

    // Built-in functions have no class file either: `arrayOf` is in the
    // `kotlin` package's built-ins.
    let array_of: Vec<String> = top_level(&contents, "kotlin", "arrayOf")
        .into_iter()
        .map(signature)
        .collect();
    assert_eq!(
        array_of,
        vec!["arrayOf(vararg elements: T): kotlin.Array<T>"]
    );

    let aliases: Vec<String> = contents
        .package_members
        .iter()
        .filter(|members| members.package.to_string() == "kotlin.collections")
        .flat_map(|members| members.type_aliases.iter())
        .filter(|alias| alias.name.as_str() == "ArrayList")
        .map(|alias| render(&alias.expanded))
        .collect();
    assert_eq!(aliases, vec!["java.util.ArrayList<E>"]);

    // Deprecations: a function's message, replacement and since-versions
    // from its JVM method; a property's from its synthetic `$annotations`
    // method; a class's from its class file.
    let sum_by = top_level(&contents, "kotlin.collections", "sumBy")
        .into_iter()
        .find(|member| {
            member
                .receiver
                .as_ref()
                .is_some_and(|receiver| render(receiver).starts_with("kotlin.collections.Iterable"))
        })
        .expect("Iterable.sumBy");
    let deprecation = sum_by.deprecation.as_ref().expect("sumBy is deprecated");
    assert_eq!(deprecation.level, DeprecationLevel::Warning);
    assert_eq!(deprecation.message.as_deref(), Some("Use sumOf instead."));
    assert_eq!(
        deprecation.replace_with.as_deref(),
        Some("this.sumOf(selector)")
    );
    assert_eq!(deprecation.origin, DeprecationOrigin::Kotlin);
    assert_eq!(
        deprecation
            .since_kotlin
            .as_ref()
            .and_then(|since| since.warning_since.as_deref()),
        Some("1.5")
    );
    // Overloads are told apart by the descriptor their types compile to.
    let int_array_sum_by = top_level(&contents, "kotlin.collections", "sumBy")
        .into_iter()
        .find(|member| {
            member
                .receiver
                .as_ref()
                .is_some_and(|receiver| render(receiver) == "kotlin.IntArray")
        })
        .expect("IntArray.sumBy");
    assert!(int_array_sum_by.deprecation.is_some());
    let to_upper_case: Vec<(String, Option<String>)> =
        top_level(&contents, "kotlin.text", "toUpperCase")
            .into_iter()
            .filter(|member| {
                member
                    .receiver
                    .as_ref()
                    .is_some_and(|receiver| render(receiver) == "kotlin.String")
            })
            .map(|member| {
                (
                    signature(member),
                    member
                        .deprecation
                        .as_ref()
                        .and_then(|deprecation| deprecation.message.as_deref().map(str::to_owned)),
                )
            })
            .collect();
    assert!(
        to_upper_case.contains(&(
            "kotlin.String.toUpperCase(): kotlin.String".to_owned(),
            Some("Use uppercase() instead.".to_owned())
        )),
        "{to_upper_case:?}"
    );
    let sum_of = top_level(&contents, "kotlin.collections", "sumOf");
    assert!(sum_of.iter().all(|member| member.deprecation.is_none()));
    let end_exclusive = class(&contents, "kotlin.ranges.IntRange")
        .members
        .iter()
        .find(|member| member.name.as_str() == "endExclusive")
        .expect("IntRange.endExclusive");
    assert!(end_exclusive.deprecation.is_some());
    // Opt-in: a marker is an annotation class with `@RequiresOptIn`, its
    // level `ERROR` unless it says; a declaration it marks carries it.
    let marker_level = |name: &str| {
        class(&contents, name)
            .opt_in_marker
            .as_ref()
            .map(|marker| marker.level)
    };
    assert_eq!(
        marker_level("kotlin.ExperimentalStdlibApi"),
        Some(OptInLevel::Error)
    );
    assert_eq!(
        marker_level("kotlin.ExperimentalUnsignedTypes"),
        Some(OptInLevel::Warning)
    );
    assert_eq!(marker_level("kotlin.collections.List"), None);
    // Contracts: what holds of the arguments when a function returns.
    let contract = |package: &str, name: &str| {
        top_level(&contents, package, name)
            .iter()
            .find_map(|member| member.contract.as_deref().cloned())
    };
    let render = |contract: Option<crate::Contract<LibraryType>>| {
        contract.map(|contract| {
            contract
                .effects
                .iter()
                .map(|effect| format!("{:?} => {:?}", effect.returns, effect.implies.map(&|_| ())))
                .collect::<Vec<_>>()
                .join("; ")
        })
    };
    assert_eq!(
        render(contract("kotlin", "requireNotNull")).as_deref(),
        Some("Normally => IsNull { subject: Parameter(0), negated: true }")
    );
    assert_eq!(
        render(contract("kotlin", "require")).as_deref(),
        Some("Normally => Holds { subject: Parameter(0), negated: false }")
    );
    assert_eq!(
        render(contract("kotlin.text", "isNullOrEmpty")).as_deref(),
        Some("Value(false) => IsNull { subject: Receiver, negated: true }")
    );
    assert_eq!(render(contract("kotlin", "assert")), None);
    // And which lambdas a function surely calls before it returns.
    let calls_in_place = |package: &str, name: &str| {
        contract(package, name).map(|contract| {
            contract
                .calls_in_place
                .iter()
                .map(|calls| format!("{:?} {:?}", calls.lambda, calls.invocation))
                .collect::<Vec<_>>()
                .join("; ")
        })
    };
    assert_eq!(
        calls_in_place("kotlin", "run").as_deref(),
        Some("Parameter(0) ExactlyOnce")
    );
    assert_eq!(
        calls_in_place("kotlin", "let").as_deref(),
        Some("Parameter(0) ExactlyOnce")
    );
    // A data class's generated members are marked; what it declares isn't.
    // Declaration-site variance: `List<out E>`, `Comparable<in T>`,
    // `MutableList<E>`.
    let variances = |name: &str| {
        class(&contents, name)
            .type_parameters
            .iter()
            .map(|parameter| parameter.variance)
            .collect::<Vec<_>>()
    };
    assert_eq!(variances("kotlin.collections.List"), [crate::Variance::Out]);
    assert_eq!(variances("kotlin.Comparable"), [crate::Variance::In]);
    assert_eq!(
        variances("kotlin.collections.MutableList"),
        [crate::Variance::Invariant]
    );
    let pair = class(&contents, "kotlin.Pair");
    let synthesized = |name: &str| {
        pair.members
            .iter()
            .find(|member| member.name.as_str() == name)
            .map(|member| member.modifiers.is_synthesized)
    };
    assert_eq!(synthesized("component1"), Some(true));
    assert_eq!(synthesized("copy"), Some(true));
    assert_eq!(synthesized("first"), Some(false));
    assert_eq!(synthesized("toList"), None);
    let if_or_null = top_level(&contents, "kotlin", "ifOrNull");
    assert!(
        if_or_null.iter().all(|member| member
            .annotations
            .iter()
            .any(|annotation| annotation.qualified() == "kotlin.ExperimentalStdlibApi")),
        "{if_or_null:?}"
    );
    let jvm_default = class(&contents, "kotlin.jvm.JvmDefault");
    assert_eq!(
        jvm_default
            .deprecation
            .as_ref()
            .map(|deprecation| deprecation.level),
        Some(DeprecationLevel::Hidden)
    );
}

#[test]
fn a_java_class_compiled_by_javac() {
    let directory = std::env::temp_dir().join(format!("ka-classfile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(directory.join("demo")).unwrap();
    std::fs::write(
        directory.join("demo/Store.java"),
        r#"package demo;
import java.util.*;
public class Store<K extends Comparable<K>, V> implements Iterable<V> {
    public static final int LIMIT = 10;
    private int hidden;
    public Store(int capacity) {}
    public Optional<V> find(K key) { return Optional.empty(); }
    public static <T> List<T> of(T... items) { return null; }
    protected void evict(Map.Entry<K, V> entry) {}
    public Iterator<V> iterator() { return null; }
    @Deprecated public void clear() {}
    /** @deprecated use {@link #clear()} */
    public void reset() {}
    public class Cursor { public Cursor(String label) {} }
    public static class Builder {}
    public enum Mode { FAST, SAFE }
}
"#,
    )
    .unwrap();
    let compiled = std::process::Command::new("javac")
        .arg("-parameters")
        .arg("-d")
        .arg(&directory)
        .arg(directory.join("demo/Store.java"))
        .status();
    if !compiled.is_ok_and(|status| status.success()) {
        eprintln!("skipped: javac unavailable");
        return;
    }
    let read = |name: &str| match read_class(&std::fs::read(directory.join(name)).unwrap()) {
        ClassContents::Class(class) => class,
        _ => panic!("{name} is a class"),
    };
    let store = read("demo/Store.class");
    assert_eq!(store.name.qualified(), "demo.Store");
    assert_eq!(render(&store.supertypes[1]), "java.lang.Iterable<V>");
    let members: Vec<String> = store.members.iter().map(signature).collect();
    assert_eq!(
        members,
        vec![
            "LIMIT: int",
            "Store(capacity: int)",
            "find(key: K): java.util.Optional<V>",
            "of(vararg items: T): java.util.List<T>",
            "evict(entry: java.util.Map.Entry<K, V>): void",
            "iterator(): java.util.Iterator<V>",
            "clear(): void",
            "reset(): void",
        ]
    );
    // Java's `@Deprecated` and a Javadoc `@deprecated` alike: a warning.
    let deprecations: Vec<Option<DeprecationLevel>> = store
        .members
        .iter()
        .map(|member| {
            member
                .deprecation
                .as_ref()
                .map(|deprecation| deprecation.level)
        })
        .collect();
    assert_eq!(
        deprecations[4..],
        [
            None,
            None,
            Some(DeprecationLevel::Warning),
            Some(DeprecationLevel::Warning)
        ]
    );
    assert_eq!(
        store.members[6]
            .deprecation
            .as_ref()
            .map(|deprecation| deprecation.origin),
        Some(DeprecationOrigin::Java)
    );
    assert!(store.members[0].modifiers.is_static);
    assert!(matches!(
        store.members[0].kind,
        MemberKind::Property { mutable: false }
    ));

    // An inner class's constructor doesn't show its outer instance.
    let cursor = read("demo/Store$Cursor.class");
    assert_eq!(cursor.name.qualified(), "demo.Store.Cursor");
    assert!(cursor.modifiers.is_inner);
    assert_eq!(
        signature(&cursor.members[0]),
        "Cursor(label: java.lang.String)"
    );
    let mode = read("demo/Store$Mode.class");
    assert_eq!(mode.kind, ClassKind::Enum);
    assert_eq!(
        mode.enum_entries
            .iter()
            .map(|entry| entry.as_str())
            .collect::<Vec<_>>(),
        vec!["FAST", "SAFE"]
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn an_entry_larger_than_the_limit_is_refused_whatever_its_header_claims() {
    let data = [7u8; 100];
    assert_eq!(
        crate::read_at_most(&data[..], 100, 100).map(|bytes| bytes.len()),
        Some(100)
    );
    assert!(crate::read_at_most(&data[..], 10, 50).is_none());
    assert!(crate::read_at_most(&data[..], 1 << 40, 1000).is_none());
}
