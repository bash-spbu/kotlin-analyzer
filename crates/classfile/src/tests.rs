//! Reading real compiled code: the Kotlin standard library from the Gradle
//! cache (skipped when absent) and a Java class compiled by `javac`.

use std::path::{Path, PathBuf};

use crate::{
    ClassContents, ClassKind, JarContents, LibraryClass, LibraryType, Member, MemberKind,
    PackageMembers, TypeArgument, read_class, read_jar,
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

    let aliases: Vec<String> = contents
        .package_members
        .iter()
        .filter(|members| members.package.to_string() == "kotlin.collections")
        .flat_map(|members| members.type_aliases.iter())
        .filter(|alias| alias.name.as_str() == "ArrayList")
        .map(|alias| render(&alias.expanded))
        .collect();
    assert_eq!(aliases, vec!["java.util.ArrayList<E>"]);
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
        ]
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
