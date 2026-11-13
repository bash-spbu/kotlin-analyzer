//! Kotlin metadata → [`LibraryClass`] / [`PackageMembers`].
//!
//! Implemented from the format's published definition (JetBrains' Kotlin
//! repository, Apache-2.0): `metadata.proto`, `jvm_metadata.proto`,
//! `builtins.proto`, the flag layout of `Flags.java`, the byte packing of
//! `BitEncoding` and the string table of `JvmNameResolverBase`.

use intern::{Name, Path};

use crate::class_file::{ClassFile, KotlinMetadata, latin1_bytes};
use crate::protobuf::Reader;
use crate::{
    ClassContents, ClassKind, ClassName, LibraryClass, LibraryType, Member, MemberKind, Modifiers,
    Origin, PackageMembers, TypeArgument, TypeParameter, ValueParameter, Variance, Visibility,
};

/// `kind` of `@kotlin.Metadata`.
const CLASS: i32 = 1;
const FILE_FACADE: i32 = 2;
const MULTI_FILE_CLASS_PART: i32 = 5;

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
    match metadata.kind {
        CLASS => match read_class(message, &names, class.source_file.clone()) {
            Some(library_class) => ClassContents::Class(library_class),
            None => ClassContents::Nothing,
        },
        FILE_FACADE | MULTI_FILE_CLASS_PART => {
            let package = match class.name.rsplit_once('/') {
                Some((package, _)) => Path::parse(&package.replace('/', ".")),
                None => Path::default(),
            };
            ClassContents::PackageMembers(read_package(
                message,
                &names,
                package,
                class.source_file.clone(),
            ))
        }
        _ => ClassContents::Nothing,
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
        .filter_map(|class| read_class(class, &names, None))
        .map(ClassContents::Class)
        .collect();
    // The package's own functions and properties (`arrayOf`, `emptyArray`).
    if let Some(message) = package_message {
        let members = read_package(message, &names, package, None);
        if !members.members.is_empty() || !members.type_aliases.is_empty() {
            contents.push(ClassContents::PackageMembers(members));
        }
    }
    contents
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
    /// 0 none, 1 internal name to class id, 2 descriptor to class id.
    operation: i32,
    substring: Vec<i32>,
    replace_char: Vec<i32>,
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
                    for _ in 0..record.range.max(1) {
                        records.push(record.clone());
                    }
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
                    1 => text = text.replace('$', "."),
                    2 => {
                        if text.len() >= 2 {
                            text = text[1..text.len() - 1].to_owned();
                        }
                        text = text.replace('$', ".");
                    }
                    _ => {}
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
            3 => record.operation = value.as_i32().unwrap_or(0),
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

fn modality(flags: i32) -> i32 {
    (flags >> 4) & 0b11
}

const MODALITY_OPEN: i32 = 1;
const MODALITY_ABSTRACT: i32 = 2;
const MODALITY_SEALED: i32 = 3;

fn bit(flags: i32, index: u32) -> bool {
    flags & (1 << index) != 0
}

/// A member's kind (bits 6–7): declaration, fake override, delegation,
/// synthesized. Fake overrides repeat a supertype's member.
fn is_fake_override(flags: i32) -> bool {
    (flags >> 6) & 0b11 == 1
}

fn read_class(
    bytes: &[u8],
    names: &NameTable,
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
            is_abstract: class_modality == MODALITY_ABSTRACT,
            is_open: class_modality == MODALITY_OPEN,
            is_sealed: class_modality == MODALITY_SEALED,
            is_inner: bit(flags, 9),
            is_data: bit(flags, 10),
            is_expect: bit(flags, 12),
            is_value: bit(flags, 13),
            is_fun_interface: bit(flags, 14),
            ..Modifiers::default()
        },
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
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => flags = value.as_i32().unwrap_or(6),
            2 => parameters.extend(value.as_bytes()),
            _ => {}
        }
    }
    Some(Member {
        name: class_name,
        kind: MemberKind::Constructor,
        visibility: visibility_of(flags)?,
        modifiers: Modifiers::default(),
        type_parameters: Vec::new(),
        receiver: None,
        parameters: parameters
            .iter()
            .filter_map(|parameter| read_value_parameter(parameter, context))
            .collect(),
        return_type: None,
    })
}

fn read_function(bytes: &[u8], context: &TypeContext<'_>) -> Option<Member> {
    let mut flags = 6;
    let mut name = None;
    let mut return_type = None;
    let mut receiver = None;
    let mut type_parameters = Vec::new();
    let mut parameters = Vec::new();
    let mut own_table = None;
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
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
    let visibility = visibility_of(flags)?;
    let table;
    let context = match own_table {
        Some(bytes) => {
            table = TypeTable::read(Some(bytes));
            TypeContext {
                names: context.names,
                table: &table,
                scopes: context.scopes.clone(),
            }
        }
        None => context.with_scope(Vec::new()),
    };
    let (parameters_declared, scope) = read_type_parameters(&type_parameters, &context);
    let context = context.with_scope(scope);
    Some(Member {
        name: context.names.name(name?)?,
        kind: MemberKind::Function,
        visibility,
        modifiers: Modifiers {
            is_abstract: modality(flags) == MODALITY_ABSTRACT,
            is_open: modality(flags) == MODALITY_OPEN,
            is_operator: bit(flags, 8),
            is_infix: bit(flags, 9),
            is_inline: bit(flags, 10),
            is_suspend: bit(flags, 13),
            is_expect: bit(flags, 14),
            ..Modifiers::default()
        },
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
    })
}

fn read_property(bytes: &[u8], context: &TypeContext<'_>) -> Option<Member> {
    let mut flags = 518;
    let mut name = None;
    let mut return_type = None;
    let mut receiver = None;
    let mut type_parameters = Vec::new();
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
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
    let (parameters_declared, scope) = read_type_parameters(&type_parameters, context);
    let context = context.with_scope(scope);
    Some(Member {
        name: context.names.name(name?)?,
        kind: MemberKind::Property {
            mutable: bit(flags, 8),
        },
        visibility,
        modifiers: Modifiers {
            is_abstract: modality(flags) == MODALITY_ABSTRACT,
            is_open: modality(flags) == MODALITY_OPEN,
            is_const: bit(flags, 11),
            ..Modifiers::default()
        },
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
    /// 0 in, 1 out, 2 invariant, 3 star.
    projection: i32,
    type_ref: Option<RawType>,
    type_id: Option<i32>,
}

impl RawType {
    fn read(bytes: &[u8]) -> RawType {
        let mut raw = RawType::default();
        let mut reader = Reader::new(bytes);
        while let Some((number, value)) = reader.field() {
            match number {
                1 => raw.flags = value.as_i32().unwrap_or(0),
                2 => raw.arguments.extend(value.as_bytes().map(read_argument)),
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

fn read_argument(bytes: &[u8]) -> RawArgument {
    let mut argument = RawArgument {
        projection: 2,
        type_ref: None,
        type_id: None,
    };
    let mut reader = Reader::new(bytes);
    while let Some((number, value)) = reader.field() {
        match number {
            1 => argument.projection = value.as_i32().unwrap_or(2),
            2 => argument.type_ref = value.as_bytes().map(RawType::read),
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

/// What reading a type needs: names, the type table, and the type
/// parameters in scope (outermost first).
struct TypeContext<'table> {
    names: &'table NameTable,
    table: &'table TypeTable,
    /// Type parameters in scope by id, outermost scope first.
    scopes: Vec<Vec<(i32, Name)>>,
}

impl<'table> TypeContext<'table> {
    fn with_scope(&self, scope: Vec<(i32, Name)>) -> TypeContext<'table> {
        let mut scopes = self.scopes.clone();
        scopes.push(scope);
        TypeContext {
            names: self.names,
            table: self.table,
            scopes,
        }
    }

    fn raw_table_type(&self, id: i32) -> RawType {
        self.table.raw(id).unwrap_or_default()
    }

    fn table_type(&self, id: i32) -> LibraryType {
        self.library_type(&self.raw_table_type(id))
    }

    fn library_type(&self, raw: &RawType) -> LibraryType {
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
                    (3, _) | (_, None) => TypeArgument::Star,
                    (projection, Some(type_ref)) => TypeArgument::Type {
                        variance: match projection {
                            0 => Variance::In,
                            1 => Variance::Out,
                            _ => Variance::Invariant,
                        },
                        type_ref: self.library_type(&type_ref),
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
}
