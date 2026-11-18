//! Kotlin metadata → [`LibraryClass`] / [`PackageMembers`].
//!
//! Implemented from the format's published definition (JetBrains' Kotlin
//! repository, Apache-2.0): `metadata.proto`, `jvm_metadata.proto`,
//! `builtins.proto`, the flag layout of `Flags.java`, the byte packing of
//! `BitEncoding` and the string table of `JvmNameResolverBase`.

use intern::{Name, Path};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::Nesting;
use crate::class_file::{ClassFile, KotlinMetadata, latin1_bytes};
use crate::protobuf::Reader;
use crate::signature::NestedNames;
use crate::{
    ClassContents, ClassKind, ClassName, Deprecation, DeprecationOrigin, LibraryClass, LibraryType,
    Member, MemberKind, Modifiers, Origin, PackageMembers, TypeArgument, TypeParameter,
    ValueParameter, Variance, Visibility,
};

/// `kind` of `@kotlin.Metadata`.
/// What a class file's `@Metadata` (its `k`) describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MetadataKind {
    Class,
    /// `FooKt`: a file's top-level declarations.
    FileFacade,
    /// One file's part of a `@JvmMultifileClass` facade.
    MultiFileClassPart,
    /// A synthetic class, a lambda, the multi-file facade itself.
    Other,
}

impl MetadataKind {
    fn of(kind: i32) -> MetadataKind {
        match kind {
            1 => MetadataKind::Class,
            2 => MetadataKind::FileFacade,
            5 => MetadataKind::MultiFileClassPart,
            _ => MetadataKind::Other,
        }
    }
}

/// Strings Kotlin's JVM string table refers to by index, so they needn't be
/// stored in every class file (`JvmNameResolverBase.PREDEFINED_STRINGS`).
const PREDEFINED_STRINGS: &[&str] = &[
    "kotlin/Any",
    "kotlin/Nothing",
    "kotlin/Unit",
    "kotlin/Throwable",
    "kotlin/Number",
    "kotlin/Byte",
    "kotlin/Double",
    "kotlin/Float",
    "kotlin/Int",
    "kotlin/Long",
    "kotlin/Short",
    "kotlin/Boolean",
    "kotlin/Char",
    "kotlin/CharSequence",
    "kotlin/String",
    "kotlin/Comparable",
    "kotlin/Enum",
    "kotlin/Array",
    "kotlin/ByteArray",
    "kotlin/DoubleArray",
    "kotlin/FloatArray",
    "kotlin/IntArray",
    "kotlin/LongArray",
    "kotlin/ShortArray",
    "kotlin/BooleanArray",
    "kotlin/CharArray",
    "kotlin/Cloneable",
    "kotlin/Annotation",
    "kotlin/collections/Iterable",
    "kotlin/collections/MutableIterable",
    "kotlin/collections/Collection",
    "kotlin/collections/MutableCollection",
    "kotlin/collections/List",
    "kotlin/collections/MutableList",
    "kotlin/collections/Set",
    "kotlin/collections/MutableSet",
    "kotlin/collections/Map",
    "kotlin/collections/MutableMap",
    "kotlin/collections/Map.Entry",
    "kotlin/collections/MutableMap.MutableEntry",
    "kotlin/collections/Iterator",
    "kotlin/collections/MutableIterator",
    "kotlin/collections/ListIterator",
    "kotlin/collections/MutableListIterator",
];

pub(crate) fn read(class: &ClassFile, metadata: &KotlinMetadata) -> ClassContents {
    let bytes = decode_bytes(&metadata.data1);
    let mut reader = Reader::new(&bytes);
    let Some(string_table) = reader.delimited() else {
        return ClassContents::Nothing;
    };
    let names = NameTable::jvm(string_table, &metadata.data2);
    let message = reader.rest();
    let nested = NestedNames::new(&class.inner_classes);
    let methods = JvmMethods::of(&class.methods, &nested);
    match MetadataKind::of(metadata.kind) {
        MetadataKind::Class => {
            match read_class(message, &names, &methods, class.source_file.clone()) {
                Some(library_class) => ClassContents::Class(LibraryClass {
                    deprecation: kotlin_deprecation(class.deprecation.as_ref()).map(Box::new),
                    annotations: nested.class_names(&class.annotations),
                    opt_in_marker: class.opt_in_marker.clone(),
                    ..library_class
                }),
                None => ClassContents::Nothing,
            }
        }
        MetadataKind::FileFacade | MetadataKind::MultiFileClassPart => {
            let directory = || match class.name.rsplit_once('/') {
                Some((package, _)) => Path::parse(&package.replace('/', ".")),
                None => Path::default(),
            };
            let package = match &metadata.package_name {
                Some(declared) if !declared.is_empty() => Path::parse(declared),
                _ => directory(),
            };
            ClassContents::PackageMembers(read_package(
                message,
                &names,
                &methods,
                package,
                class.source_file.clone(),
            ))
        }
        MetadataKind::Other => ClassContents::Nothing,
    }
}

/// The classes of a `.kotlin_builtins` file: a version prefix, then a
/// `PackageFragment` message with its own string tables.
pub(crate) fn read_builtins(bytes: &[u8], package: Path) -> Vec<ClassContents> {
    // `BuiltInsBinaryVersion`: a big-endian count, then that many ints.
    let Some(count) = bytes
        .get(0..4)
        .map(|count| u32::from_be_bytes([count[0], count[1], count[2], count[3]]) as usize)
    else {
        return Vec::new();
    };
    let Some(fragment) = bytes.get(4 + count * 4..) else {
        return Vec::new();
    };
    let mut strings = Vec::new();
    let mut qualified_names = Vec::new();
    let mut package_message = None;
    let mut classes = Vec::new();
    let mut reader = Reader::new(fragment);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => strings = read_string_table(value.as_bytes().unwrap_or_default()),
            2 => qualified_names = read_qualified_names(value.as_bytes().unwrap_or_default()),
            3 => package_message = value.as_bytes(),
            4 => classes.extend(value.as_bytes()),
            _ => {}
        }
    }
    let names = NameTable::Builtins {
        strings,
        qualified_names,
    };
    let mut contents: Vec<ClassContents> = classes
        .into_iter()
        .filter_map(|class| read_class(class, &names, &JvmMethods::default(), None))
        .map(ClassContents::Class)
        .collect();
    // The package's own functions and properties (`arrayOf`, `emptyArray`).
    if let Some(message) = package_message {
        let members = read_package(message, &names, &JvmMethods::default(), package, None);
        if !members.members.is_empty() || !members.type_aliases.is_empty() {
            contents.push(ClassContents::PackageMembers(members));
        }
    }
    contents
}

/// `text` without its first and last characters (an internal name's `L`
/// and `;`), empty when it has fewer than two.
fn without_first_and_last(text: &str) -> String {
    let mut characters = text.chars();
    characters.next();
    characters.next_back();
    characters.collect()
}

/// `d1`'s strings back into bytes: UTF-8 mode (a leading `\0`, one byte per
/// character) or the older 7-bit packing.
fn decode_bytes(strings: &[String]) -> Vec<u8> {
    let marker = strings.first().and_then(|first| first.chars().next());
    if marker == Some('\u{0}') {
        let mut bytes = Vec::new();
        for (index, string) in strings.iter().enumerate() {
            let text = if index == 0 {
                &string[1..]
            } else {
                string.as_str()
            };
            bytes.extend(latin1_bytes(text));
        }
        return bytes;
    }
    let mut combined: Vec<u8> = Vec::new();
    for (index, string) in strings.iter().enumerate() {
        let mut characters = string.chars();
        if index == 0 && marker == Some('\u{FFFF}') {
            characters.next();
        }
        combined.extend(characters.map(|character| character as u32 as u8));
    }
    // The encoder added 1 to every 7-bit byte; adding 0x7f modulo 0x80
    // undoes it.
    for byte in &mut combined {
        *byte = byte.wrapping_add(0x7f) & 0x7f;
    }
    // Seven useful bits per byte, regrouped into eight.
    let length = 7 * combined.len() / 8;
    let mut decoded = Vec::with_capacity(length);
    let mut index = 0;
    let mut bit = 0u32;
    for _ in 0..length {
        let first = u32::from(combined[index]) >> bit;
        index += 1;
        let second = (u32::from(combined[index]) & ((1 << (bit + 1)) - 1)) << (7 - bit);
        decoded.push((first + second) as u8);
        if bit == 6 {
            index += 1;
            bit = 0;
        } else {
            bit += 1;
        }
    }
    decoded
}

/// One `StringTableTypes.Record`, applying to `range` consecutive strings.
#[derive(Clone, Default)]
struct Record {
    range: i32,
    predefined_index: Option<i32>,
    string: Option<String>,
    operation: StringOperation,
    substring: Vec<i32>,
    replace_char: Vec<i32>,
}

/// How a record turns its string into a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum StringOperation {
    #[default]
    None,
    /// `kotlin/collections/Map$Entry` → `kotlin/collections/Map.Entry`.
    InternalToClassId,
    /// `Lkotlin/collections/Map$Entry;` → the same, without `L` and `;`.
    DescriptorToClassId,
}

impl StringOperation {
    fn of(operation: i32) -> StringOperation {
        match operation {
            1 => StringOperation::InternalToClassId,
            2 => StringOperation::DescriptorToClassId,
            _ => StringOperation::None,
        }
    }
}

/// How metadata indexes resolve to names.
enum NameTable {
    /// A class file: `d2` plus the records describing how to read each.
    Jvm {
        strings: Vec<String>,
        records: Vec<Record>,
        local_names: Vec<i32>,
    },
    /// A `.kotlin_builtins` file: a string table and a table of qualified
    /// names built from it.
    Builtins {
        strings: Vec<String>,
        qualified_names: Vec<(i32, i32, i32)>,
    },
}

impl NameTable {
    fn jvm(string_table: &[u8], strings: &[String]) -> NameTable {
        let mut records = Vec::new();
        let mut local_names = Vec::new();
        let mut reader = Reader::new(string_table);
        while let Some((number, value)) = reader.field() {
            match number {
                1 => {
                    let record = read_record(value.as_bytes().unwrap_or_default());
                    // A record covers `range` strings; none past the last.
                    let room = strings.len().saturating_sub(records.len());
                    let covered = usize::try_from(record.range.max(1)).unwrap_or(1).min(room);
                    records.extend(std::iter::repeat_n(record, covered));
                }
                5 => value.push_integers(&mut local_names),
                _ => {}
            }
        }
        NameTable::Jvm {
            strings: strings.to_vec(),
            records,
            local_names,
        }
    }

    fn string(&self, index: i32) -> Option<String> {
        let index = usize::try_from(index).ok()?;
        match self {
            NameTable::Builtins { strings, .. } => strings.get(index).cloned(),
            NameTable::Jvm {
                strings, records, ..
            } => {
                let Some(record) = records.get(index) else {
                    return strings.get(index).cloned();
                };
                let mut text = match (&record.string, record.predefined_index) {
                    (Some(text), _) => text.clone(),
                    (None, Some(predefined)) => {
                        (*PREDEFINED_STRINGS.get(usize::try_from(predefined).ok()?)?).to_owned()
                    }
                    (None, None) => strings.get(index)?.clone(),
                };
                if let [begin, end, ..] = record.substring[..] {
                    let characters: Vec<char> = text.chars().collect();
                    let valid = 0 <= begin && begin <= end && (end as usize) <= characters.len();
                    if valid {
                        text = characters[begin as usize..end as usize].iter().collect();
                    }
                }
                if let [from, to, ..] = record.replace_char[..] {
                    let from = char::from_u32(from as u32)?;
                    let to = char::from_u32(to as u32)?;
                    text = text.replace(from, &to.to_string());
                }
                match record.operation {
                    StringOperation::InternalToClassId => text = text.replace('$', "."),
                    StringOperation::DescriptorToClassId => {
                        text = without_first_and_last(&text);
                        text = text.replace('$', ".");
                    }
                    StringOperation::None => {}
                }
                Some(text)
            }
        }
    }

    fn name(&self, index: i32) -> Option<Name> {
        self.string(index).map(|text| Name::new(&text))
    }

    /// The class an `fq_name` index names; `None` for a local class.
    fn class_name(&self, index: i32) -> Option<ClassName> {
        match self {
            NameTable::Jvm { local_names, .. } => {
                if local_names.contains(&index) {
                    return None;
                }
                ClassName::from_kotlin_internal(&self.string(index)?)
            }
            NameTable::Builtins {
                qualified_names, ..
            } => {
                let mut package = Vec::new();
                let mut classes = Vec::new();
                let mut current = index;
                let mut steps = 0;
                while current >= 0 && steps < 64 {
                    let (parent, short_name, kind) = *qualified_names.get(current as usize)?;
                    let name = self.name(short_name)?;
                    match kind {
                        0 => classes.push(name),
                        1 => package.push(name),
                        _ => return None,
                    }
                    current = parent;
                    steps += 1;
                }
                package.reverse();
                classes.reverse();
                (!classes.is_empty()).then(|| ClassName {
                    package: Path::new(package),
                    classes: classes.into_boxed_slice(),
                })
            }
        }
    }
}

fn read_record(bytes: &[u8]) -> Record {
    let mut record = Record {
        range: 1,
        ..Record::default()
    };
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => record.range = value.as_i32().unwrap_or(1),
            2 => record.predefined_index = value.as_i32(),
            3 => record.operation = StringOperation::of(value.as_i32().unwrap_or(0)),
            4 => value.push_integers(&mut record.substring),
            5 => value.push_integers(&mut record.replace_char),
            6 => {
                record.string = value
                    .as_bytes()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            }
            _ => {}
        }
    }
    record
}

fn read_string_table(bytes: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        if number == 1 {
            strings
                .push(String::from_utf8_lossy(value.as_bytes().unwrap_or_default()).into_owned());
        }
    }
    strings
}

/// `(parent, short name, kind)` per `QualifiedName`; kind 0 class, 1
/// package, 2 local.
fn read_qualified_names(bytes: &[u8]) -> Vec<(i32, i32, i32)> {
    let mut names = Vec::new();
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        if number != 1 {
            continue;
        }
        let mut parent = -1;
        let mut short_name = 0;
        let mut kind = 1;
        let mut inner = Reader::new(value.as_bytes().unwrap_or_default());
        while let Some((field, value)) = inner.field() {
            match field {
                1 => parent = value.as_i32().unwrap_or(-1),
                2 => short_name = value.as_i32().unwrap_or(0),
                3 => kind = value.as_i32().unwrap_or(1),
                _ => {}
            }
        }
        names.push((parent, short_name, kind));
    }
    names
}

// ─── Flags (`Flags.java`): has-annotations at bit 0, visibility in bits 1–3,
// modality in bits 4–5, then per-declaration fields. ───────────────────────

fn visibility_of(flags: i32) -> Option<Visibility> {
    match (flags >> 1) & 0b111 {
        3 => Some(Visibility::Public),
        2 => Some(Visibility::Protected),
        // internal, private, private-to-this, local: invisible to users.
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Modality {
    Final,
    Open,
    Abstract,
    Sealed,
}

fn modality(flags: i32) -> Modality {
    match (flags >> 4) & 0b11 {
        1 => Modality::Open,
        2 => Modality::Abstract,
        3 => Modality::Sealed,
        _ => Modality::Final,
    }
}

fn bit(flags: i32, index: u32) -> bool {
    flags & (1 << index) != 0
}

/// A member's kind (bits 6–7): declaration, fake override, delegation,
/// synthesized. Fake overrides repeat a supertype's member.
fn is_fake_override(flags: i32) -> bool {
    (flags >> 6) & 0b11 == 1
}

/// A member the compiler generated (a data class's `componentN`, `copy`,
/// …): kind 3.
fn is_synthesized(flags: i32) -> bool {
    (flags >> 6) & 0b11 == 3
}

fn read_class(
    bytes: &[u8],
    names: &NameTable,
    methods: &JvmMethods,
    source_file: Option<String>,
) -> Option<LibraryClass> {
    let mut flags = 6;
    let mut name = None;
    let mut type_parameters = Vec::new();
    let mut supertypes = Vec::new();
    let mut supertype_ids = Vec::new();
    let mut constructors = Vec::new();
    let mut functions = Vec::new();
    let mut properties = Vec::new();
    let mut enum_entries = Vec::new();
    let mut sealed_subclasses = Vec::new();
    let mut type_table = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => flags = value.as_i32().unwrap_or(6),
            3 => name = value.as_i32(),
            5 => type_parameters.extend(value.as_bytes()),
            6 => supertypes.extend(value.as_bytes()),
            2 => value.push_integers(&mut supertype_ids),
            8 => constructors.extend(value.as_bytes()),
            9 => functions.extend(value.as_bytes()),
            10 => properties.extend(value.as_bytes()),
            13 => enum_entries.extend(value.as_bytes()),
            16 => value.push_integers(&mut sealed_subclasses),
            30 => type_table = value.as_bytes(),
            _ => {}
        }
    }
    let class_name = names.class_name(name?)?;
    let visibility = visibility_of(flags)?;
    let kind = match (flags >> 6) & 0b111 {
        0 => ClassKind::Class,
        1 => ClassKind::Interface,
        2 => ClassKind::Enum,
        4 => ClassKind::Annotation,
        5 => ClassKind::Object,
        6 => ClassKind::CompanionObject,
        // An enum entry with a body compiles to its own class.
        _ => return None,
    };
    let table = TypeTable::read(type_table);
    let outer = TypeContext {
        names,
        table: &table,
        scopes: Vec::new(),
        methods,
        overloaded: &overloaded_names(&functions, &properties, names),
    };
    let (class_type_parameters, scope) = read_type_parameters(&type_parameters, &outer);
    let context = outer.with_scope(scope);
    let mut class_supertypes: Vec<LibraryType> = supertypes
        .iter()
        .map(|bytes| context.library_type(&RawType::read(bytes)))
        .collect();
    class_supertypes.extend(supertype_ids.iter().map(|id| context.table_type(*id)));
    let mut members = Vec::new();
    for constructor in constructors {
        members.extend(read_constructor(
            constructor,
            &context,
            class_name.simple_name(),
        ));
    }
    for function in functions {
        members.extend(read_function(function, &context));
    }
    for property in properties {
        members.extend(read_property(property, &context));
    }
    let class_modality = modality(flags);
    Some(LibraryClass {
        kind,
        origin: Origin::Kotlin,
        visibility,
        modifiers: Modifiers {
            is_abstract: class_modality == Modality::Abstract,
            is_open: class_modality == Modality::Open,
            is_sealed: class_modality == Modality::Sealed,
            is_inner: bit(flags, 9),
            is_data: bit(flags, 10),
            is_expect: bit(flags, 12),
            is_value: bit(flags, 13),
            is_fun_interface: bit(flags, 14),
            ..Modifiers::default()
        },
        // The class file's own annotations, which `read` knows.
        deprecation: None,
        annotations: Box::new([]),
        opt_in_marker: None,
        type_parameters: class_type_parameters,
        supertypes: class_supertypes,
        members,
        enum_entries: enum_entries
            .iter()
            .filter_map(|entry| {
                let mut reader = Reader::new(entry);
                let mut found = None;
                while let Some((number, value)) = reader.field() {
                    if number == 1 {
                        found = value.as_i32().and_then(|index| names.name(index));
                    }
                }
                found
            })
            .collect(),
        sealed_subclasses: sealed_subclasses
            .iter()
            .filter_map(|index| names.class_name(*index))
            .collect(),
        name: class_name,
        source_file,
    })
}

fn read_package(
    bytes: &[u8],
    names: &NameTable,
    methods: &JvmMethods,
    package: Path,
    source_file: Option<String>,
) -> PackageMembers {
    let mut functions = Vec::new();
    let mut properties = Vec::new();
    let mut type_aliases = Vec::new();
    let mut type_table = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            3 => functions.extend(value.as_bytes()),
            4 => properties.extend(value.as_bytes()),
            5 => type_aliases.extend(value.as_bytes()),
            30 => type_table = value.as_bytes(),
            _ => {}
        }
    }
    let table = TypeTable::read(type_table);
    let context = TypeContext {
        names,
        table: &table,
        scopes: Vec::new(),
        methods,
        overloaded: &overloaded_names(&functions, &properties, names),
    };
    let mut members = Vec::new();
    for function in functions {
        members.extend(read_function(function, &context));
    }
    for property in properties {
        members.extend(read_property(property, &context));
    }
    let aliases = type_aliases
        .iter()
        .filter_map(|alias| read_type_alias(alias, &context))
        .collect();
    PackageMembers {
        package,
        members,
        type_aliases: aliases,
        source_file,
    }
}

fn read_type_alias(bytes: &[u8], context: &TypeContext<'_>) -> Option<crate::TypeAlias> {
    let mut flags = 6;
    let mut name = None;
    let mut type_parameters = Vec::new();
    let mut expanded = None;
    let mut underlying = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => flags = value.as_i32().unwrap_or(6),
            2 => name = value.as_i32(),
            3 => type_parameters.extend(value.as_bytes()),
            4 => underlying = value.as_bytes().map(RawType::read),
            5 => underlying = value.as_i32().map(|id| context.raw_table_type(id)),
            6 => expanded = value.as_bytes().map(RawType::read),
            7 => expanded = value.as_i32().map(|id| context.raw_table_type(id)),
            _ => {}
        }
    }
    visibility_of(flags)?;
    let (parameters, scope) = read_type_parameters(&type_parameters, context);
    let inner = context.with_scope(scope);
    let target = expanded.or(underlying)?;
    Some(crate::TypeAlias {
        name: context.names.name(name?)?,
        type_parameters: parameters,
        expanded: inner.library_type(&target),
    })
}

fn read_constructor(bytes: &[u8], context: &TypeContext<'_>, class_name: Name) -> Option<Member> {
    let mut flags = 6;
    let mut parameters = Vec::new();
    let mut jvm_signature = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => flags = value.as_i32().unwrap_or(6),
            2 => parameters.extend(value.as_bytes()),
            100 => jvm_signature = value.as_bytes(),
            _ => {}
        }
    }
    let mut member = Member {
        name: class_name,
        kind: MemberKind::Constructor,
        visibility: visibility_of(flags)?,
        modifiers: Modifiers::default(),
        deprecation: None,
        annotations: Box::new([]),
        type_parameters: Vec::new(),
        receiver: None,
        parameters: parameters
            .iter()
            .filter_map(|parameter| read_value_parameter(parameter, context))
            .collect(),
        return_type: None,
    };
    let facts = context
        .method_facts(jvm_signature, "<init>", || {
            crate::jvm_descriptor::of_constructor(&member)
        })
        .unwrap_or_default();
    member.deprecation = facts.deprecation.map(Box::new);
    member.annotations = facts.annotations;
    Some(member)
}

fn read_function(bytes: &[u8], context: &TypeContext<'_>) -> Option<Member> {
    let mut flags = 6;
    let mut name = None;
    let mut return_type = None;
    let mut receiver = None;
    let mut type_parameters = Vec::new();
    let mut parameters = Vec::new();
    let mut own_table = None;
    let mut jvm_signature = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            100 => jvm_signature = value.as_bytes(),
            9 => flags = value.as_i32().unwrap_or(6),
            2 => name = value.as_i32(),
            3 => return_type = value.as_bytes().map(RawType::read),
            7 => return_type = value.as_i32().map(|id| context.raw_table_type(id)),
            4 => type_parameters.extend(value.as_bytes()),
            5 => receiver = value.as_bytes().map(RawType::read),
            8 => receiver = value.as_i32().map(|id| context.raw_table_type(id)),
            6 => parameters.extend(value.as_bytes()),
            30 => own_table = value.as_bytes(),
            _ => {}
        }
    }
    if is_fake_override(flags) {
        return None;
    }
    let kotlin_name = context.names.string(name?)?;
    let visibility = visibility_of(flags)?;
    let table;
    let context = match own_table {
        Some(bytes) => {
            table = TypeTable::read(Some(bytes));
            TypeContext {
                names: context.names,
                table: &table,
                scopes: context.scopes.clone(),
                methods: context.methods,
                overloaded: context.overloaded,
            }
        }
        None => context.with_scope(Vec::new()),
    };
    let (parameters_declared, scope) = read_type_parameters(&type_parameters, &context);
    let context = context.with_scope(scope);
    let mut member = Member {
        name: context.names.name(name?)?,
        kind: MemberKind::Function,
        visibility,
        modifiers: Modifiers {
            is_abstract: modality(flags) == Modality::Abstract,
            is_open: modality(flags) == Modality::Open,
            is_operator: bit(flags, 8),
            is_infix: bit(flags, 9),
            is_inline: bit(flags, 10),
            is_suspend: bit(flags, 13),
            is_expect: bit(flags, 14),
            is_synthesized: is_synthesized(flags),
            ..Modifiers::default()
        },
        deprecation: None,
        annotations: Box::new([]),
        type_parameters: parameters_declared,
        receiver: receiver.map(|receiver| context.library_type(&receiver)),
        parameters: parameters
            .iter()
            .filter_map(|parameter| read_value_parameter(parameter, &context))
            .collect(),
        return_type: Some(
            return_type
                .map(|return_type| context.library_type(&return_type))
                .unwrap_or(LibraryType::Unreadable),
        ),
    };
    let facts = context
        .method_facts(jvm_signature, &kotlin_name, || {
            crate::jvm_descriptor::of_function(&member)
        })
        .unwrap_or_default();
    if facts
        .deprecation
        .as_ref()
        .is_some_and(Deprecation::is_hidden)
    {
        return None;
    }
    member.deprecation = facts.deprecation.map(Box::new);
    member.annotations = facts.annotations;
    Some(member)
}

fn read_property(bytes: &[u8], context: &TypeContext<'_>) -> Option<Member> {
    let mut flags = 518;
    let mut name = None;
    let mut return_type = None;
    let mut receiver = None;
    let mut type_parameters = Vec::new();
    let mut jvm_signature = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            100 => jvm_signature = value.as_bytes(),
            11 => flags = value.as_i32().unwrap_or(518),
            2 => name = value.as_i32(),
            3 => return_type = value.as_bytes().map(RawType::read),
            9 => return_type = value.as_i32().map(|id| context.raw_table_type(id)),
            4 => type_parameters.extend(value.as_bytes()),
            5 => receiver = value.as_bytes().map(RawType::read),
            10 => receiver = value.as_i32().map(|id| context.raw_table_type(id)),
            _ => {}
        }
    }
    if is_fake_override(flags) {
        return None;
    }
    let visibility = visibility_of(flags)?;
    let kotlin_name = context.names.string(name?)?;
    let facts = context
        .property_facts(jvm_signature, &kotlin_name)
        .unwrap_or_default();
    let (parameters_declared, scope) = read_type_parameters(&type_parameters, context);
    let context = context.with_scope(scope);
    Some(Member {
        name: context.names.name(name?)?,
        kind: MemberKind::Property {
            mutable: bit(flags, 8),
        },
        visibility,
        modifiers: Modifiers {
            is_abstract: modality(flags) == Modality::Abstract,
            is_open: modality(flags) == Modality::Open,
            is_const: bit(flags, 11),
            ..Modifiers::default()
        },
        deprecation: facts.deprecation.map(Box::new),
        annotations: facts.annotations,
        type_parameters: parameters_declared,
        receiver: receiver.map(|receiver| context.library_type(&receiver)),
        parameters: Vec::new(),
        return_type: Some(
            return_type
                .map(|return_type| context.library_type(&return_type))
                .unwrap_or(LibraryType::Unreadable),
        ),
    })
}

fn read_value_parameter(bytes: &[u8], context: &TypeContext<'_>) -> Option<ValueParameter> {
    let mut flags = 0;
    let mut name = None;
    let mut type_ref = None;
    let mut vararg_element = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => flags = value.as_i32().unwrap_or(0),
            2 => name = value.as_i32(),
            3 => type_ref = value.as_bytes().map(RawType::read),
            5 => type_ref = value.as_i32().map(|id| context.raw_table_type(id)),
            4 => vararg_element = value.as_bytes().map(RawType::read),
            6 => vararg_element = value.as_i32().map(|id| context.raw_table_type(id)),
            _ => {}
        }
    }
    let vararg = vararg_element.is_some();
    let written = vararg_element.or(type_ref)?;
    Some(ValueParameter {
        name: context.names.name(name?)?,
        type_ref: context.library_type(&written),
        // Bit 1, after has-annotations: `DECLARES_DEFAULT_VALUE`.
        has_default: bit(flags, 1),
        vararg,
    })
}

/// `TypeParameter` messages: the parameters, and the scope (ids to names)
/// that declarations under them read types in.
fn read_type_parameters(
    messages: &[&[u8]],
    context: &TypeContext<'_>,
) -> (Vec<TypeParameter>, Vec<(i32, Name)>) {
    let mut raw: Vec<(i32, Name, Vec<RawType>)> = Vec::new();
    for message in messages {
        let mut id = 0;
        let mut name = None;
        let mut bounds = Vec::new();
        let mut reader = Reader::new(message);
        while let Some((number, value)) = reader.field() {
            match number {
                1 => id = value.as_i32().unwrap_or(0),
                2 => name = value.as_i32().and_then(|index| context.names.name(index)),
                5 => bounds.extend(value.as_bytes().map(RawType::read)),
                6 => {
                    let mut ids = Vec::new();
                    value.push_integers(&mut ids);
                    bounds.extend(ids.into_iter().filter_map(|id| context.table.raw(id)));
                }
                _ => {}
            }
        }
        if let Some(name) = name {
            raw.push((id, name, bounds));
        }
    }
    let scope: Vec<(i32, Name)> = raw.iter().map(|(id, name, _)| (*id, *name)).collect();
    // Bounds may mention the parameters themselves (`T : Comparable<T>`).
    let inner = context.with_scope(scope.clone());
    let parameters = raw
        .into_iter()
        .map(|(_, name, bounds)| TypeParameter {
            name,
            bounds: bounds
                .iter()
                .map(|bound| inner.library_type(bound))
                .collect(),
        })
        .collect();
    (parameters, scope)
}

/// The fields of a `Type` message this reader uses.
#[derive(Clone, Default)]
struct RawType {
    class_name: Option<i32>,
    type_alias_name: Option<i32>,
    type_parameter: Option<i32>,
    type_parameter_name: Option<i32>,
    arguments: Vec<RawArgument>,
    nullable: bool,
    flags: i32,
    /// The class ids of the type's annotations (`@ExtensionFunctionType`
    /// marks `R.(A) -> B`).
    annotation_ids: Vec<i32>,
}

#[derive(Clone)]
struct RawArgument {
    projection: Projection,
    type_ref: Option<RawType>,
    type_id: Option<i32>,
}

impl RawType {
    fn read(bytes: &[u8]) -> RawType {
        RawType::read_nested(bytes, Nesting::TOP)
    }

    /// A type too deeply nested reads as no type (unreadable).
    fn read_nested(bytes: &[u8], nesting: Nesting) -> RawType {
        let mut raw = RawType::default();
        let Some(nested) = nesting.deeper() else {
            return raw;
        };
        let mut reader = Reader::new(bytes);
        while let Some((number, value)) = reader.field() {
            match number {
                1 => raw.flags = value.as_i32().unwrap_or(0),
                2 => raw
                    .arguments
                    .extend(value.as_bytes().map(|bytes| read_argument(bytes, nested))),
                3 => raw.nullable = value.as_u64() == Some(1),
                6 => raw.class_name = value.as_i32(),
                7 => raw.type_parameter = value.as_i32(),
                9 => raw.type_parameter_name = value.as_i32(),
                12 => raw.type_alias_name = value.as_i32(),
                // Type annotations (100; 150 in built-ins).
                100 | 150 => {
                    if let Some(id) = value.as_bytes().and_then(annotation_class_id) {
                        raw.annotation_ids.push(id);
                    }
                }
                _ => {}
            }
        }
        raw
    }
}

fn annotation_class_id(bytes: &[u8]) -> Option<i32> {
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        if number == 1 {
            return value.as_i32();
        }
    }
    None
}

/// A type argument's projection: `in T`, `out T`, `T`, `*`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Projection {
    In,
    Out,
    Invariant,
    Star,
}

impl Projection {
    fn of(projection: i32) -> Projection {
        match projection {
            0 => Projection::In,
            1 => Projection::Out,
            3 => Projection::Star,
            _ => Projection::Invariant,
        }
    }
}

fn read_argument(bytes: &[u8], nesting: Nesting) -> RawArgument {
    let mut argument = RawArgument {
        projection: Projection::Invariant,
        type_ref: None,
        type_id: None,
    };
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => argument.projection = Projection::of(value.as_i32().unwrap_or(2)),
            2 => {
                argument.type_ref = value
                    .as_bytes()
                    .map(|bytes| RawType::read_nested(bytes, nesting))
            }
            3 => argument.type_id = value.as_i32(),
            _ => {}
        }
    }
    argument
}

/// A message's `TypeTable`: types referred to by index.
#[derive(Default)]
struct TypeTable {
    types: Vec<RawType>,
    /// Types from this index on are nullable.
    first_nullable: Option<usize>,
}

impl TypeTable {
    fn read(bytes: Option<&[u8]>) -> TypeTable {
        let mut table = TypeTable::default();
        let Some(bytes) = bytes else {
            return table;
        };
        let mut reader = Reader::new(bytes);
        while let Some((number, value)) = reader.field() {
            match number {
                1 => table.types.extend(value.as_bytes().map(RawType::read)),
                2 => {
                    table.first_nullable = value
                        .as_i32()
                        .filter(|index| *index >= 0)
                        .map(|index| index as usize)
                }
                _ => {}
            }
        }
        table
    }

    fn raw(&self, id: i32) -> Option<RawType> {
        let index = usize::try_from(id).ok()?;
        let mut raw = self.types.get(index)?.clone();
        if self.first_nullable.is_some_and(|first| index >= first) {
            raw.nullable = true;
        }
        Some(raw)
    }
}

/// What a class file's methods say about the Kotlin members its metadata
/// describes: their deprecation and annotations. Metadata names a member's
/// JVM method only when its signature isn't the obvious one, so a member is
/// also found by the descriptor its types map to, or by name — when every
/// method of the name says the same.
#[derive(Default)]
struct JvmMethods {
    /// `name` + descriptor of every method.
    by_signature: FxHashMap<String, MethodFacts>,
    by_name: FxHashMap<String, NameFacts>,
}

/// What one JVM method says.
#[derive(Clone, PartialEq, Default)]
struct MethodFacts {
    deprecation: Option<Deprecation>,
    /// Its annotations that may mark it as needing opt-in.
    annotations: Box<[ClassName]>,
}

/// What the methods of one name say.
enum NameFacts {
    Agree(MethodFacts),
    Differ,
}

impl JvmMethods {
    /// The methods' facts; none at all when no method says anything —
    /// most classes, which then skip every lookup.
    fn of(methods: &[crate::class_file::Member], nested: &NestedNames) -> JvmMethods {
        let says_something = methods.iter().any(|method| {
            !method.annotations.is_empty()
                || kotlin_deprecation(method.deprecation.as_ref()).is_some()
        });
        if !says_something {
            return JvmMethods::default();
        }
        let mut found = JvmMethods::default();
        for method in methods {
            let facts = MethodFacts {
                deprecation: kotlin_deprecation(method.deprecation.as_ref()),
                annotations: nested.class_names(&method.annotations),
            };
            found.by_signature.insert(
                format!("{}{}", method.name, method.descriptor),
                facts.clone(),
            );
            let agreed = found
                .by_name
                .entry(method.name.clone())
                .or_insert_with(|| NameFacts::Agree(facts.clone()));
            if let NameFacts::Agree(existing) = agreed
                && *existing != facts
            {
                *agreed = NameFacts::Differ;
            }
        }
        found
    }

    fn is_empty(&self) -> bool {
        self.by_signature.is_empty()
    }
}

/// A `JvmMethodSignature` message: its name (1) and descriptor (2), as
/// string table indices.
fn jvm_method_signature(bytes: &[u8], names: &NameTable) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut descriptor = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => name = value.as_i32().and_then(|index| names.string(index)),
            2 => descriptor = value.as_i32().and_then(|index| names.string(index)),
            _ => {}
        }
    }
    (name, descriptor)
}

/// What a Kotlin class file's `@kotlin.Deprecated` says. The `Deprecated`
/// attribute there means nothing to Kotlin: the compiler puts it on every
/// synthetic `$annotations` method and on a deprecated property's
/// accessors, to keep Java away; the annotation is what was written.
fn kotlin_deprecation(found: Option<&Deprecation>) -> Option<Deprecation> {
    found
        .filter(|deprecation| deprecation.origin == DeprecationOrigin::Kotlin)
        .cloned()
}

/// The names that more than one of a container's functions and
/// properties have.
fn overloaded_names(
    functions: &[&[u8]],
    properties: &[&[u8]],
    names: &NameTable,
) -> FxHashSet<String> {
    let mut seen = FxHashSet::default();
    let mut overloaded = FxHashSet::default();
    for member in functions.iter().chain(properties) {
        let mut reader = Reader::new(member);
        while let Some((number, value)) = reader.field() {
            if number == 2
                && let Some(name) = value.as_i32().and_then(|index| names.string(index))
                && !seen.insert(name.clone())
            {
                overloaded.insert(name);
            }
        }
    }
    overloaded
}

/// A property's getter name: `isOpen` stays, `name` becomes `getName`.
fn getter_name(property: &str) -> String {
    let after_is = property
        .strip_prefix("is")
        .and_then(|rest| rest.chars().next());
    if after_is.is_some_and(|char| !char.is_lowercase()) {
        return property.to_owned();
    }
    let mut chars = property.chars();
    match chars.next() {
        Some(first) => format!("get{}{}", first.to_uppercase(), chars.as_str()),
        None => "get".to_owned(),
    }
}

struct TypeContext<'table> {
    names: &'table NameTable,
    table: &'table TypeTable,
    /// Type parameters in scope by id, outermost scope first.
    scopes: Vec<Vec<(i32, Name)>>,
    /// What the class file's methods say.
    methods: &'table JvmMethods,
    /// Kotlin member names the container declares more than once
    /// (`Int.dp`, `Float.dp`): their JVM methods can't be told apart by
    /// name.
    overloaded: &'table FxHashSet<String>,
}

impl<'table> TypeContext<'table> {
    /// What a function's or constructor's JVM method says — the method
    /// named by the metadata's signature (the name defaults to the Kotlin
    /// one), else by the descriptor its Kotlin types map to when a method
    /// has it, else found by name.
    fn method_facts(
        &self,
        jvm_signature: Option<&[u8]>,
        kotlin_name: &str,
        computed_descriptor: impl FnOnce() -> Option<String>,
    ) -> Option<MethodFacts> {
        if self.methods.is_empty() {
            return None;
        }
        let (name, descriptor) = jvm_signature
            .map(|bytes| jvm_method_signature(bytes, self.names))
            .unwrap_or_default();
        let name = name.unwrap_or_else(|| kotlin_name.to_owned());
        let signature = match descriptor {
            Some(descriptor) => Some(format!("{name}{descriptor}")),
            None => computed_descriptor()
                .map(|descriptor| format!("{name}{descriptor}"))
                .filter(|signature| self.methods.by_signature.contains_key(signature)),
        };
        if let Some(signature) = signature {
            return self.methods.by_signature.get(&signature).cloned();
        }
        if self.overloaded.contains(kotlin_name) {
            return None;
        }
        self.agreed_facts(&name)
    }

    /// What a property's synthetic `get…$annotations` method says — where
    /// `@Deprecated` and the other annotations on a property are compiled
    /// to. A JVM method named like the getter says nothing: it may be a
    /// function of its own (`HttpHeaders.getAuthorization()` beside `const
    /// val Authorization`).
    fn property_facts(
        &self,
        jvm_signature: Option<&[u8]>,
        kotlin_name: &str,
    ) -> Option<MethodFacts> {
        // `JvmPropertySignature`: the synthetic method (2).
        let mut synthetic = None;
        if let Some(bytes) = jvm_signature {
            let mut reader = Reader::new(bytes);
            while let Some((number, value)) = reader.field() {
                if number == 2 {
                    synthetic = value
                        .as_bytes()
                        .map(|bytes| jvm_method_signature(bytes, self.names));
                }
            }
        }
        if self.overloaded.contains(kotlin_name) || self.methods.is_empty() {
            return None;
        }
        // The synthetic method is named after the property, whatever
        // `@JvmName` its getter has.
        let synthetic = synthetic
            .and_then(|(name, _)| name)
            .unwrap_or_else(|| format!("{}$annotations", getter_name(kotlin_name)));
        self.agreed_facts(&synthetic)
    }

    /// What every method named `name` says; nothing when they differ.
    fn agreed_facts(&self, name: &str) -> Option<MethodFacts> {
        match self.methods.by_name.get(name) {
            Some(NameFacts::Agree(facts)) => Some(facts.clone()),
            Some(NameFacts::Differ) | None => None,
        }
    }

    fn with_scope(&self, scope: Vec<(i32, Name)>) -> TypeContext<'table> {
        let mut scopes = self.scopes.clone();
        scopes.push(scope);
        TypeContext {
            names: self.names,
            table: self.table,
            scopes,
            methods: self.methods,
            overloaded: self.overloaded,
        }
    }

    fn raw_table_type(&self, id: i32) -> RawType {
        self.table.raw(id).unwrap_or_default()
    }

    fn table_type(&self, id: i32) -> LibraryType {
        self.library_type(&self.raw_table_type(id))
    }

    fn library_type(&self, raw: &RawType) -> LibraryType {
        self.library_type_nested(raw, Nesting::TOP)
    }

    /// Type arguments go one level deeper — a bound on more than the stack:
    /// a type table entry's argument may name the entry itself.
    fn library_type_nested(&self, raw: &RawType, nesting: Nesting) -> LibraryType {
        let Some(nested) = nesting.deeper() else {
            return LibraryType::Unreadable;
        };
        if let Some(name_index) = raw.type_parameter_name {
            return match self.names.name(name_index) {
                Some(name) => LibraryType::TypeParameter {
                    name,
                    nullable: raw.nullable,
                },
                None => LibraryType::Unreadable,
            };
        }
        if let Some(id) = raw.type_parameter {
            return match self.type_parameter_by_id(id) {
                Some(name) => LibraryType::TypeParameter {
                    name,
                    nullable: raw.nullable,
                },
                None => LibraryType::Unreadable,
            };
        }
        let Some(class_index) = raw.class_name.or(raw.type_alias_name) else {
            return LibraryType::Unreadable;
        };
        let Some(name) = self.names.class_name(class_index) else {
            return LibraryType::Unreadable;
        };
        let arguments: Vec<TypeArgument> = raw
            .arguments
            .iter()
            .map(|argument| {
                let type_ref = argument
                    .type_ref
                    .clone()
                    .or_else(|| argument.type_id.and_then(|id| self.table.raw(id)));
                match (argument.projection, type_ref) {
                    (Projection::Star, _) | (_, None) => TypeArgument::Star,
                    (projection, Some(type_ref)) => TypeArgument::Type {
                        variance: match projection {
                            Projection::In => Variance::In,
                            Projection::Out => Variance::Out,
                            Projection::Invariant | Projection::Star => Variance::Invariant,
                        },
                        type_ref: self.library_type_nested(&type_ref, nested),
                    },
                }
            })
            .collect();
        if let Some(function) = self.function_type(&name, &arguments, raw) {
            return function;
        }
        LibraryType::Class {
            name,
            arguments,
            nullable: raw.nullable,
        }
    }

    /// Ids are unique across a class and its members, so the innermost
    /// scope that has the id declares it.
    fn type_parameter_by_id(&self, id: i32) -> Option<Name> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter())
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, name)| *name)
    }

    /// `kotlin.FunctionN<A, B, R>` (with `@ExtensionFunctionType`: the first
    /// argument is the receiver) as a function type.
    fn function_type(
        &self,
        name: &ClassName,
        arguments: &[TypeArgument],
        raw: &RawType,
    ) -> Option<LibraryType> {
        let simple = name.simple_name();
        let simple = simple.as_str();
        let is_function = name.package.to_string() == "kotlin"
            && simple
                .strip_prefix("Function")
                .is_some_and(|arity| arity.parse::<u32>().is_ok());
        if !is_function {
            return None;
        }
        let mut types: Vec<LibraryType> = arguments
            .iter()
            .map(|argument| match argument {
                TypeArgument::Type { type_ref, .. } => type_ref.clone(),
                TypeArgument::Star => LibraryType::Unreadable,
            })
            .collect();
        let mut result = types.pop()?;
        let has_receiver = raw
            .annotation_ids
            .iter()
            .filter_map(|id| self.names.class_name(*id))
            .any(|annotation| annotation.qualified() == "kotlin.ExtensionFunctionType");
        let receiver = (has_receiver && !types.is_empty()).then(|| Box::new(types.remove(0)));
        // `SUSPEND_TYPE` (bit 0): the last parameter is the continuation,
        // whose argument is the real result.
        let suspend = bit(raw.flags, 0);
        if suspend
            && let Some(LibraryType::Class {
                name, arguments, ..
            }) = types.last()
            && name.qualified() == "kotlin.coroutines.Continuation"
        {
            if let Some(TypeArgument::Type { type_ref, .. }) = arguments.first() {
                result = type_ref.clone();
            }
            types.pop();
        }
        Some(LibraryType::Function {
            receiver,
            parameters: types,
            result: Box::new(result),
            suspend,
            nullable: raw.nullable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_bit_packing_round_trips_through_the_documented_encoder() {
        // Encode [0x12, 0xff, 0x00] the way `BitEncoding.encode8to7` does:
        // regroup 8-bit bytes into 7-bit ones, then add 1 to each modulo 0x80.
        let input: [u8; 3] = [0x12, 0xff, 0x00];
        let mut bits: Vec<u8> = Vec::new();
        for byte in input {
            for index in 0..8 {
                bits.push((byte >> index) & 1);
            }
        }
        let mut packed: Vec<u8> = bits
            .chunks(7)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .map(|(index, bit)| bit << index)
                    .sum::<u8>()
            })
            .collect();
        for byte in &mut packed {
            *byte = (*byte + 1) & 0x7f;
        }
        let text: String = packed.iter().map(|byte| *byte as char).collect();
        assert_eq!(decode_bytes(&[text]), input.to_vec());
    }

    #[test]
    fn utf8_mode_drops_the_marker() {
        assert_eq!(decode_bytes(&["\u{0}\u{1}ÿ".to_owned()]), vec![1, 0xff]);
    }

    fn varint(mut value: u64, into: &mut Vec<u8>) {
        while value >= 0x80 {
            into.push((value as u8) | 0x80);
            value >>= 7;
        }
        into.push(value as u8);
    }

    /// A length-delimited protobuf field.
    fn message_field(number: u64, message: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        varint(number << 3 | 2, &mut bytes);
        varint(message.len() as u64, &mut bytes);
        bytes.extend_from_slice(message);
        bytes
    }

    fn argument_depth(raw: &RawType) -> usize {
        raw.arguments
            .iter()
            .filter_map(|argument| argument.type_ref.as_ref())
            .map(|type_ref| 1 + argument_depth(type_ref))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn a_type_nested_past_the_limit_stops_reading_instead_of_overflowing() {
        // `List<List<List<…>>>`, ten thousand deep: field 2 of a Type is an
        // Argument, field 2 of an Argument a Type.
        let mut bytes = vec![6 << 3, 0];
        for _ in 0..10_000 {
            bytes = message_field(2, &message_field(2, &bytes));
        }
        let raw = RawType::read(&bytes);
        assert!(argument_depth(&raw) <= usize::from(Nesting::LIMIT));
    }

    #[test]
    fn a_type_table_entry_naming_itself_as_its_argument_ends() {
        let names = NameTable::Jvm {
            strings: vec!["kotlin/collections/List".to_owned()],
            records: Vec::new(),
            local_names: Vec::new(),
        };
        let looping = RawType {
            class_name: Some(0),
            arguments: vec![RawArgument {
                projection: Projection::Invariant,
                type_ref: None,
                type_id: Some(0),
            }],
            ..RawType::default()
        };
        let table = TypeTable {
            types: vec![looping],
            first_nullable: None,
        };
        let methods = JvmMethods::default();
        let context = TypeContext {
            names: &names,
            table: &table,
            scopes: Vec::new(),
            methods: &methods,
            overloaded: &FxHashSet::default(),
        };
        assert!(matches!(context.table_type(0), LibraryType::Class { .. }));
    }

    #[test]
    fn a_record_range_never_reaches_past_the_strings() {
        let mut record = vec![1 << 3];
        varint(i32::MAX as u64, &mut record);
        let string_table = message_field(1, &record);
        let strings = vec!["a".to_owned(), "b".to_owned()];
        let NameTable::Jvm { records, .. } = NameTable::jvm(&string_table, &strings) else {
            unreachable!("a JVM table");
        };
        assert_eq!(records.len(), strings.len());
    }

    #[test]
    fn a_descriptor_operation_on_non_ascii_text_strips_characters_not_bytes() {
        let names = NameTable::Jvm {
            strings: vec!["Lпакет/Кот;".to_owned(), "ж".to_owned()],
            records: vec![
                Record {
                    range: 2,
                    operation: StringOperation::DescriptorToClassId,
                    ..Record::default()
                };
                2
            ],
            local_names: Vec::new(),
        };
        assert_eq!(names.string(0).as_deref(), Some("пакет/Кот"));
        assert_eq!(names.string(1).as_deref(), Some(""));
    }
}
