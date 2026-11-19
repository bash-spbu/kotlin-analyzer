//! Breakpoints by file and line, and where the JVM has them. A file's
//! classes are those of its package compiled from a file of its name —
//! and, for a file declaring inline functions, any class its functions
//! were inlined into, which its source map names (`smap`).

use std::collections::HashMap;
use std::path::PathBuf;

use jdwp::{Connection, EventKind, Location, Modifier, ReferenceTypeId, SuspendPolicy};

use crate::conditions::{Condition, HitCondition, LogMessage};
use crate::smap::SourceMap;
use crate::sources::SourceKey;

/// Packages whose classes never hold a project's inlined code.
const LIBRARY_PREFIXES: &[&str] = &[
    "Ljava/",
    "Ljavax/",
    "Ljdk/",
    "Lsun/",
    "Lcom/sun/",
    "Lkotlin/",
];

/// A breakpoint as set: where it is asked for, and where the JVM has it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Breakpoint {
    pub id: u32,
    pub path: PathBuf,
    pub line: u32,
    /// Set in a loaded class: the JVM will stop there. Otherwise it waits
    /// for a class of the file to load.
    pub verified: bool,
    pub options: BreakpointOptions,
}

/// What a breakpoint asks before it stops, as given.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct BreakpointOptions {
    /// Stops only when this holds: `count > 3 && order.id == "A-1"`.
    pub condition: Option<String>,
    /// Stops only on these hits (of those the condition lets through):
    /// `5`, `>= 5`, `% 10`.
    pub hit_condition: Option<String>,
    /// Logs this instead of stopping: `sum is {sum}`.
    pub log_message: Option<String>,
}

/// The options parsed.
pub(crate) struct Checks {
    pub condition: Option<Condition>,
    pub hit_condition: Option<HitCondition>,
    pub log_message: Option<LogMessage>,
    pub hits: u64,
}

impl Checks {
    pub fn of(options: &BreakpointOptions) -> Result<Checks, String> {
        let present = |text: &Option<String>| {
            text.as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        Ok(Checks {
            condition: present(&options.condition)
                .map(|text| Condition::parse(&text))
                .transpose()?,
            hit_condition: present(&options.hit_condition)
                .map(|text| HitCondition::parse(&text))
                .transpose()?,
            log_message: present(&options.log_message)
                .map(|text| LogMessage::parse(&text))
                .transpose()?,
            hits: 0,
        })
    }
}

pub(crate) struct BreakpointEntry {
    pub breakpoint: Breakpoint,
    pub key: SourceKey,
    pub checks: Checks,
    /// The class-prepare requests watching for the file's classes.
    pub prepare_requests: Vec<i32>,
    /// The JVM's breakpoint requests, by location.
    pub requests: HashMap<Location, i32>,
}

#[derive(Default)]
pub(crate) struct BreakpointTable {
    pub next_id: u32,
    pub entries: Vec<BreakpointEntry>,
}

impl BreakpointTable {
    /// The breakpoints the JVM's breakpoint requests are of.
    pub fn of_requests(&self, requests: &[i32]) -> Vec<u32> {
        self.entries
            .iter()
            .filter(|entry| entry.requests.values().any(|id| requests.contains(id)))
            .map(|entry| entry.breakpoint.id)
            .collect()
    }

    pub fn entry_mut(&mut self, id: u32) -> Option<&mut BreakpointEntry> {
        self.entries
            .iter_mut()
            .find(|entry| entry.breakpoint.id == id)
    }
}

/// Asks to hear of the file's classes loading: those of its package, and
/// for a file with inline functions any class naming it.
pub(crate) fn watch_classes(connection: &Connection, key: &SourceKey) -> Vec<i32> {
    let mut patterns = vec![vec![
        Modifier::ClassMatch(key.class_pattern()),
        Modifier::SourceNameMatch(key.file_name.clone()),
    ]];
    if key.declares_inline_functions {
        // The source names a class prepare matches include its source
        // map's.
        patterns.push(vec![Modifier::SourceNameMatch(key.file_name.clone())]);
    }
    patterns
        .iter()
        .filter_map(|modifiers| {
            connection
                .set_event_request(
                    EventKind::ClassPrepare,
                    SuspendPolicy::EventThread,
                    modifiers,
                )
                .ok()
        })
        .collect()
}

/// The loaded classes that may hold the file's code.
pub(crate) fn loaded_classes_of(
    connection: &Connection,
    key: &SourceKey,
) -> Vec<(ReferenceTypeId, String)> {
    let Ok(classes) = connection.all_classes() else {
        return Vec::new();
    };
    classes
        .into_iter()
        .filter(|class| {
            let is_library = LIBRARY_PREFIXES
                .iter()
                .any(|prefix| class.signature.starts_with(prefix));
            let in_package = key.holds_class(&class.signature);
            in_package || (key.declares_inline_functions && !is_library)
        })
        .map(|class| (class.id, class.signature))
        .collect()
}

/// Where the code of `key`'s `line` starts in `class`: in each method
/// with one of its lines, the first code index of each.
pub(crate) fn locations_at(
    connection: &Connection,
    class: ReferenceTypeId,
    signature: &str,
    key: &SourceKey,
    line: u32,
) -> Vec<Location> {
    let lines = class_lines_of(connection, class, signature, key, line);
    if lines.is_empty() {
        return Vec::new();
    }
    let Ok(methods) = connection.methods(class) else {
        return Vec::new();
    };
    let mut locations = Vec::new();
    for method in methods {
        let Ok(table) = connection.line_table(class, method.id) else {
            continue;
        };
        for wanted in &lines {
            let first = table
                .lines
                .iter()
                .filter(|(_, entry_line)| i64::from(*entry_line) == i64::from(*wanted))
                .map(|(index, _)| *index)
                .min();
            if let Some(index) = first {
                locations.push(Location {
                    type_tag: jdwp::TypeTag::Class,
                    class,
                    method: method.id,
                    index,
                });
            }
        }
    }
    locations
}

/// The class's own line numbers holding `key`'s `line`: through its
/// source map when it has one, else the line itself if the class is the
/// file's.
fn class_lines_of(
    connection: &Connection,
    class: ReferenceTypeId,
    signature: &str,
    key: &SourceKey,
    line: u32,
) -> Vec<u32> {
    let source_map = connection
        .source_debug_extension(class)
        .ok()
        .and_then(|text| SourceMap::parse(&text));
    if let Some(source_map) = source_map {
        return source_map.output_lines_of(&key.file_name, &key.package, line);
    }
    let is_the_files = key.holds_class(signature)
        && connection
            .source_file(class)
            .is_ok_and(|name| name == key.file_name);
    if is_the_files { vec![line] } else { Vec::new() }
}

pub(crate) fn set_breakpoint_request(
    connection: &Connection,
    location: &Location,
) -> Result<i32, jdwp::Error> {
    connection.set_event_request(
        EventKind::Breakpoint,
        SuspendPolicy::All,
        &[Modifier::LocationOnly(*location)],
    )
}

/// A class loaded for the class-prepare request `request`: the
/// breakpoints watching for it go into it.
pub(crate) fn prepared(
    connection: &Connection,
    table: &mut BreakpointTable,
    request: i32,
    class: ReferenceTypeId,
    signature: &str,
) {
    for entry in table.entries.iter_mut() {
        if !entry.prepare_requests.contains(&request) {
            continue;
        }
        for location in locations_at(
            connection,
            class,
            signature,
            &entry.key,
            entry.breakpoint.line,
        ) {
            if entry.requests.contains_key(&location) {
                continue;
            }
            if let Ok(id) = set_breakpoint_request(connection, &location) {
                entry.requests.insert(location, id);
                entry.breakpoint.verified = true;
            }
        }
    }
}
