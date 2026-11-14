//! Multi-file test fixtures, so tests read like the workspaces they model:
//!
//! ```text
//! //- /common/src/main/kotlin/a/Money.kt module=:common
//! package a
//! class Money
//! //- /api/src/main/kotlin/b/Use.kt module=:api deps=:common
//! package b
//! import a.Money
//! val price: Mon$0ey = TODO()
//! ```
//!
//! Each `//- /path` line starts a file. `module=:name` (default `:app`) and
//! `set=name` (default `main`) say which source set it belongs to;
//! `classpath=partial` marks the source set's classpath as only partly
//! resolved by the build;
//! `deps=:a,:b/test` lists the source sets it sees (`/main` by default). A
//! single `$0` marks the cursor; it is removed from the text.

use std::path::PathBuf;
use std::sync::Arc;

use project_model::{
    Classpath, ModelOrigin, Module, ModuleId, Platform, ProjectModel, SourceSet as ModelSourceSet,
    SourceSetId, SourceSetKind,
};
use syntax::Language;
use vfs::{FileId, Vfs};

use crate::{FileChange, RootDatabase, SourceDatabase};

/// The cursor marker inside fixture text.
pub const CURSOR: &str = "$0";

pub struct Fixture {
    pub database: RootDatabase,
    pub vfs: Vfs,
    /// The file and byte offset of `$0`, when the fixture has one.
    pub cursor: Option<(FileId, usize)>,
}

struct FixtureFile {
    path: PathBuf,
    module: String,
    set: String,
    dependencies: Vec<(String, String)>,
    classpath: Classpath,
    text: String,
}

impl Fixture {
    pub fn parse(fixture: &str) -> Fixture {
        let files = split(fixture);
        let model = model_of(&files);
        let mut vfs = Vfs::default();
        let mut cursor = None;
        let mut changes = Vec::new();
        for file in &files {
            let mut text = file.text.clone();
            let marker = text.find(CURSOR);
            if marker.is_some() {
                text = text.replacen(CURSOR, "", 1);
            }
            let file_id = vfs.set_contents(&file.path, Some(Arc::from(text.as_str())));
            if let Some(offset) = marker {
                cursor = Some((file_id, offset));
            }
            changes.push(FileChange {
                file: file_id,
                text: Some(Arc::from(text.as_str())),
                language: Language::of_path(&file.path).unwrap_or(Language::Kotlin),
                source_set: model.source_set_of(&file.path),
                directory_package: model.directory_package(&file.path),
            });
        }
        let mut database = RootDatabase::default();
        database.apply_change(changes);
        database.set_project_model(Arc::new(model));
        Fixture {
            database,
            vfs,
            cursor,
        }
    }

    /// Replaces the text of the fixture file at `path`, as an editor would.
    pub fn edit(&mut self, path: &str, text: &str) {
        let file_id = self.file(path);
        let source = self
            .database
            .source_file(file_id)
            .expect("a fixture file is in the database");
        let change = FileChange {
            file: file_id,
            text: Some(Arc::from(text)),
            language: source.language(&self.database),
            source_set: source.source_set(&self.database),
            directory_package: source.directory_package(&self.database).clone(),
        };
        self.vfs
            .set_contents(std::path::Path::new(path), Some(Arc::from(text)));
        self.database.apply_change(vec![change]);
    }

    pub fn cursor(&self) -> (FileId, usize) {
        self.cursor.expect("the fixture has a `$0` cursor")
    }

    pub fn file(&self, path: &str) -> FileId {
        self.vfs
            .file_id(std::path::Path::new(path))
            .unwrap_or_else(|| panic!("no fixture file {path}"))
    }
}

fn split(fixture: &str) -> Vec<FixtureFile> {
    let mut files: Vec<FixtureFile> = Vec::new();
    for line in fixture.lines() {
        let Some(header) = line.trim_start().strip_prefix("//- ") else {
            if let Some(file) = files.last_mut() {
                file.text.push_str(line);
                file.text.push('\n');
            }
            continue;
        };
        let mut words = header.split_whitespace();
        let path = PathBuf::from(words.next().expect("a fixture file header names a path"));
        let mut file = FixtureFile {
            path,
            module: ":app".to_owned(),
            set: "main".to_owned(),
            dependencies: Vec::new(),
            classpath: Classpath::Complete,
            text: String::new(),
        };
        for word in words {
            if let Some(module) = word.strip_prefix("module=") {
                file.module = module.to_owned();
            } else if let Some(set) = word.strip_prefix("set=") {
                file.set = set.to_owned();
            } else if word == "classpath=partial" {
                file.classpath = Classpath::Partial;
            } else if let Some(dependencies) = word.strip_prefix("deps=") {
                file.dependencies = dependencies
                    .split(',')
                    .map(|dependency| match dependency.split_once('/') {
                        Some((module, set)) => (module.to_owned(), set.to_owned()),
                        None => (dependency.to_owned(), "main".to_owned()),
                    })
                    .collect();
            }
        }
        files.push(file);
    }
    files
}

/// A model with one source set per distinct `(module, set)` of the files;
/// each file's directory is its source set's directory.
fn model_of(files: &[FixtureFile]) -> ProjectModel {
    let mut modules: Vec<Module> = Vec::new();
    let mut source_sets: Vec<ModelSourceSet> = Vec::new();
    let mut declared_dependencies: Vec<(SourceSetId, Vec<(String, String)>)> = Vec::new();
    for file in files {
        let module_index = match modules
            .iter()
            .position(|module| module.gradle_path == file.module)
        {
            Some(index) => index,
            None => {
                modules.push(Module {
                    gradle_path: file.module.clone(),
                    directory: PathBuf::from("/"),
                    source_sets: Vec::new(),
                });
                modules.len() - 1
            }
        };
        let existing = modules[module_index]
            .source_sets
            .iter()
            .copied()
            .find(|id| source_sets[id.0 as usize].name == file.set);
        let id = match existing {
            Some(id) => id,
            None => {
                let id = SourceSetId(source_sets.len() as u32);
                source_sets.push(ModelSourceSet {
                    id,
                    module: ModuleId(module_index as u32),
                    name: file.set.clone(),
                    kind: SourceSetKind::of_name(&file.set),
                    platform: Platform::of_name(&file.set),
                    directories: Vec::new(),
                    dependencies: Vec::new(),
                    libraries: Vec::new(),
                    classpath: file.classpath,
                });
                modules[module_index].source_sets.push(id);
                id
            }
        };
        // Any of a source set's files may say its classpath is partial.
        if file.classpath == Classpath::Partial {
            source_sets[id.0 as usize].classpath = Classpath::Partial;
        }
        source_sets[id.0 as usize]
            .directories
            .push(file.path.parent().map(PathBuf::from).unwrap_or_default());
        declared_dependencies.push((id, file.dependencies.clone()));
    }
    for (id, dependencies) in declared_dependencies {
        for (module, set) in dependencies {
            let target = modules
                .iter()
                .find(|candidate| candidate.gradle_path == module)
                .and_then(|candidate| {
                    candidate
                        .source_sets
                        .iter()
                        .copied()
                        .find(|candidate_set| source_sets[candidate_set.0 as usize].name == set)
                })
                .unwrap_or_else(|| panic!("fixture dependency {module}/{set} has no files"));
            let own = &mut source_sets[id.0 as usize].dependencies;
            if !own.contains(&target) {
                own.push(target);
            }
        }
    }
    // A test source set sees its module's main one, as Gradle sets it up.
    let snapshot: Vec<(SourceSetId, ModuleId, SourceSetKind)> = source_sets
        .iter()
        .map(|set| (set.id, set.module, set.kind))
        .collect();
    for source_set in &mut source_sets {
        if source_set.kind == SourceSetKind::Main {
            continue;
        }
        for (id, module, kind) in &snapshot {
            let same_module_main = *module == source_set.module && *kind == SourceSetKind::Main;
            if same_module_main && !source_set.dependencies.contains(id) {
                source_set.dependencies.push(*id);
            }
        }
    }
    ProjectModel::new(
        PathBuf::from("/"),
        ModelOrigin::GradleExport,
        modules,
        source_sets,
        None,
    )
}

/// The few `kotlin` and `java` declarations tests lean on, written as
/// sources: enough of the standard library for calls, generics and lambdas.
pub const MINI_STDLIB: &str = r#"
//- /lib/kotlin/Any.kt
package kotlin
open class Any {
    open fun toString(): String = TODO()
    open fun hashCode(): Int = TODO()
}
class String : CharSequence, Comparable<String> {
    val length: Int = 0
    fun plus(other: Any?): String = TODO()
}
interface CharSequence
interface Comparable<in T>
fun interface Comparator<T> {
    fun compare(a: T, b: T): Int
}
class Int : Number(), Comparable<Int> {
    fun plus(other: Int): Int = TODO()
}
class Long : Number()
abstract class Number
class Boolean
class Unit
class Nothing
class Array<T>(val size: Int) {
    fun get(index: Int): T = TODO()
}
class IntArray(val size: Int)
abstract class Enum<E : Enum<E>>(val name: String, val ordinal: Int)
fun <T> T.also(block: (T) -> Unit): T = TODO()
fun <T, R> T.let(block: (T) -> R): R = TODO()
fun <T> T.apply(block: T.() -> Unit): T = TODO()
fun TODO(): Nothing = TODO()
fun <T, R> with(receiver: T, block: T.() -> R): R = TODO()
fun <T : Any> requireNotNull(value: T?): T = TODO()
fun require(value: Boolean) {}
fun println(message: Any?) {}
fun println(message: Int) {}
data class Pair<out A, out B>(val first: A, val second: B)
infix fun <A, B> A.to(that: B): Pair<A, B> = TODO()
//- /lib/kotlin/collections/Collections.kt
package kotlin.collections
interface Iterable<out T>
interface Collection<out E> : Iterable<E> {
    val size: Int
}
interface List<out E> : Collection<E> {
    fun get(index: Int): E
    fun first(): E
}
interface MutableList<E> : List<E> {
    fun add(element: E): Boolean
}
interface Map<K, out V> {
    fun get(key: K): V?
    interface Entry<out K, out V> {
        val key: K
        val value: V
    }
}
typealias ArrayList<E> = java.util.ArrayList<E>
fun <T> listOf(vararg elements: T): List<T> = TODO()
fun <T> emptyList(): List<T> = TODO()
fun <T, R> Iterable<T>.map(transform: (T) -> R): List<R> = TODO()
fun <T> Iterable<T>.forEach(action: (T) -> Unit) {}
fun <T> Iterable<T>.sortedWith(comparator: Comparator<in T>): List<T> = TODO()
fun <T> compareBy(vararg selectors: (T) -> Comparable<*>?): Comparator<T> = TODO()
//- /lib/java/lang/Object.java
package java.lang;
public class Object {}
//- /lib/java/lang/String.java
package java.lang;
public final class String {}
//- /lib/java/util/List.java
package java.util;
public interface List<E> {}
//- /lib/java/util/ArrayList.java
package java.util;
public class ArrayList<E> implements List<E> {
    public E remove(int index) { return null; }
}
"#;

impl Fixture {
    /// `fixture` with [`MINI_STDLIB`] alongside.
    pub fn with_mini_stdlib(fixture: &str) -> Fixture {
        Fixture::parse(&format!("{MINI_STDLIB}{fixture}"))
    }
}
