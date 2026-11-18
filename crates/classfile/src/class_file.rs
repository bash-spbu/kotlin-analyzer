//! The class file format (JVM specification, chapter 4), as far as
//! declarations need: the constant pool, access flags, fields, methods and
//! the attributes that carry generic signatures, source file names, nested
//! classes, parameter names and the Kotlin metadata annotation.

use crate::{
    DeprecatedSinceKotlin, Deprecation, DeprecationLevel, DeprecationOrigin, Nesting, OptInLevel,
    OptInMarker,
};

/// Access flags, as the JVM specification numbers them.
pub(crate) mod access {
    pub(crate) const PUBLIC: u16 = 0x0001;
    pub(crate) const PROTECTED: u16 = 0x0004;
    pub(crate) const STATIC: u16 = 0x0008;
    pub(crate) const FINAL: u16 = 0x0010;
    /// On a method: a bridge the compiler generated.
    pub(crate) const BRIDGE: u16 = 0x0040;
    /// On a method: its last parameter is `...`.
    pub(crate) const VARARGS: u16 = 0x0080;
    pub(crate) const INTERFACE: u16 = 0x0200;
    pub(crate) const ABSTRACT: u16 = 0x0400;
    pub(crate) const SYNTHETIC: u16 = 0x1000;
    pub(crate) const ANNOTATION: u16 = 0x2000;
    pub(crate) const ENUM: u16 = 0x4000;
}

/// One constant-pool entry, kept only in the forms declarations read.
#[derive(Clone, Debug)]
enum Constant {
    Utf8(String),
    Class {
        name_index: u16,
    },
    Integer(i32),
    Other,
    /// The unusable slot after a long or double.
    Gap,
}

#[derive(Debug)]
pub(crate) struct Member {
    pub(crate) access: u16,
    pub(crate) name: String,
    pub(crate) descriptor: String,
    pub(crate) signature: Option<String>,
    /// From the `MethodParameters` attribute, when compiled with `-parameters`.
    pub(crate) parameter_names: Vec<String>,
    pub(crate) deprecation: Option<Deprecation>,
    /// The internal names of its other annotations
    /// (`kotlinx/coroutines/ExperimentalCoroutinesApi`).
    pub(crate) annotations: Vec<String>,
}

/// What `InnerClasses` says about one nested class.
#[derive(Clone, Debug)]
pub(crate) struct InnerClass {
    pub(crate) inner: String,
    /// `None` for a local or anonymous class.
    pub(crate) outer: Option<String>,
    /// `None` for an anonymous class.
    pub(crate) simple_name: Option<String>,
    pub(crate) access: u16,
}

/// `@kotlin.Metadata`'s arguments.
#[derive(Debug, Default)]
pub(crate) struct KotlinMetadata {
    /// 1 class, 2 file facade, 3 synthetic class, 4 multi-file facade,
    /// 5 multi-file class part.
    pub(crate) kind: i32,
    pub(crate) data1: Vec<String>,
    pub(crate) data2: Vec<String>,
    /// `pn`: the Kotlin package of a file facade, when it differs from the
    /// class file's directory (`kotlin/time/jdk8/…` declares `kotlin.time`).
    pub(crate) package_name: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ClassFile {
    pub(crate) access: u16,
    /// Binary name: `java/util/Map$Entry`.
    pub(crate) name: String,
    pub(crate) super_class: Option<String>,
    pub(crate) interfaces: Vec<String>,
    pub(crate) fields: Vec<Member>,
    pub(crate) methods: Vec<Member>,
    pub(crate) signature: Option<String>,
    pub(crate) source_file: Option<String>,
    pub(crate) inner_classes: Vec<InnerClass>,
    pub(crate) kotlin_metadata: Option<KotlinMetadata>,
    pub(crate) deprecation: Option<Deprecation>,
    /// The internal names of its other annotations.
    pub(crate) annotations: Vec<String>,
    /// `@RequiresOptIn(level, message)`: an annotation class that marks
    /// API needing opt-in.
    pub(crate) opt_in_marker: Option<OptInMarker>,
}

struct Bytes<'data> {
    data: &'data [u8],
    position: usize,
}

impl<'data> Bytes<'data> {
    fn u8(&mut self) -> Option<u8> {
        let value = *self.data.get(self.position)?;
        self.position += 1;
        Some(value)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes([
            self.u8()?,
            self.u8()?,
            self.u8()?,
            self.u8()?,
        ]))
    }

    fn take(&mut self, length: usize) -> Option<&'data [u8]> {
        let end = self.position.checked_add(length)?;
        let slice = self.data.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }
}

const MAGIC: u32 = 0xCAFE_BABE;

impl ClassFile {
    pub(crate) fn parse(data: &[u8]) -> Option<ClassFile> {
        let mut bytes = Bytes { data, position: 0 };
        if bytes.u32()? != MAGIC {
            return None;
        }
        bytes.u16()?;
        bytes.u16()?;
        let pool = read_constant_pool(&mut bytes)?;
        let access = bytes.u16()?;
        let name = class_name(&pool, bytes.u16()?)?;
        let super_index = bytes.u16()?;
        let super_class = (super_index != 0)
            .then(|| class_name(&pool, super_index))
            .flatten();
        let interface_count = bytes.u16()?;
        let mut interfaces = Vec::with_capacity(interface_count as usize);
        for _ in 0..interface_count {
            interfaces.push(class_name(&pool, bytes.u16()?)?);
        }
        let fields = read_members(&mut bytes, &pool)?;
        let methods = read_members(&mut bytes, &pool)?;
        let mut class = ClassFile {
            access,
            name,
            super_class,
            interfaces,
            fields,
            methods,
            signature: None,
            source_file: None,
            inner_classes: Vec::new(),
            kotlin_metadata: None,
            deprecation: None,
            annotations: Vec::new(),
            opt_in_marker: None,
        };
        let mut found = Deprecations::default();
        let attribute_count = bytes.u16()?;
        for _ in 0..attribute_count {
            let attribute_name = utf8(&pool, bytes.u16()?)?;
            let length = bytes.u32()? as usize;
            let body = bytes.take(length)?;
            let mut body = Bytes {
                data: body,
                position: 0,
            };
            match attribute_name {
                "Signature" => class.signature = utf8(&pool, body.u16()?).map(str::to_owned),
                "SourceFile" => class.source_file = utf8(&pool, body.u16()?).map(str::to_owned),
                "Deprecated" => found.in_javadoc = true,
                "InnerClasses" => class.inner_classes = read_inner_classes(&mut body, &pool)?,
                "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                    let annotations = read_annotations(&mut body, &pool)?;
                    found.add(&annotations);
                    class.annotations.extend(annotations.others.iter().cloned());
                    if annotations.opt_in_marker.is_some() {
                        class.opt_in_marker = annotations.opt_in_marker.clone();
                    }
                    if annotations.kotlin_metadata.is_some() {
                        class.kotlin_metadata = annotations.kotlin_metadata;
                    }
                }
                _ => {}
            }
        }
        class.deprecation = found.deprecation();
        Some(class)
    }
}

fn read_constant_pool(bytes: &mut Bytes<'_>) -> Option<Vec<Constant>> {
    let count = bytes.u16()? as usize;
    let mut pool = vec![Constant::Other; count.max(1)];
    let mut index = 1;
    while index < count {
        let tag = bytes.u8()?;
        let constant = match tag {
            1 => {
                let length = bytes.u16()? as usize;
                Constant::Utf8(decode_modified_utf8(bytes.take(length)?))
            }
            3 => Constant::Integer(bytes.u32()? as i32),
            4 => {
                bytes.u32()?;
                Constant::Other
            }
            5 | 6 => {
                bytes.u32()?;
                bytes.u32()?;
                pool[index] = Constant::Other;
                index += 1;
                if index < count {
                    pool[index] = Constant::Gap;
                }
                index += 1;
                continue;
            }
            7 => Constant::Class {
                name_index: bytes.u16()?,
            },
            8 | 16 | 19 | 20 => {
                bytes.u16()?;
                Constant::Other
            }
            9..=12 | 17 | 18 => {
                bytes.u16()?;
                bytes.u16()?;
                Constant::Other
            }
            15 => {
                bytes.u8()?;
                bytes.u16()?;
                Constant::Other
            }
            _ => return None,
        };
        pool[index] = constant;
        index += 1;
    }
    Some(pool)
}

/// The JVM's "modified UTF-8": `0` as two bytes, characters outside the
/// basic plane as surrogate pairs of three bytes each.
fn decode_modified_utf8(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index];
        if first & 0x80 == 0 {
            units.push(u16::from(first));
            index += 1;
        } else if first & 0xE0 == 0xC0 && index + 1 < bytes.len() {
            units.push((u16::from(first & 0x1F) << 6) | u16::from(bytes[index + 1] & 0x3F));
            index += 2;
        } else if first & 0xF0 == 0xE0 && index + 2 < bytes.len() {
            units.push(
                (u16::from(first & 0x0F) << 12)
                    | (u16::from(bytes[index + 1] & 0x3F) << 6)
                    | u16::from(bytes[index + 2] & 0x3F),
            );
            index += 3;
        } else {
            index += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

/// The text of a modified-UTF-8 constant whose characters are all below 256
/// — the bytes of Kotlin metadata in its UTF-8 mode.
pub(crate) fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .map(|character| character as u32 as u8)
        .collect()
}

fn utf8(pool: &[Constant], index: u16) -> Option<&str> {
    match pool.get(index as usize)? {
        Constant::Utf8(text) => Some(text),
        _ => None,
    }
}

fn class_name(pool: &[Constant], index: u16) -> Option<String> {
    match pool.get(index as usize)? {
        Constant::Class { name_index } => utf8(pool, *name_index).map(str::to_owned),
        _ => None,
    }
}

fn read_members(bytes: &mut Bytes<'_>, pool: &[Constant]) -> Option<Vec<Member>> {
    let count = bytes.u16()?;
    let mut members = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let access = bytes.u16()?;
        let name = utf8(pool, bytes.u16()?)?.to_owned();
        let descriptor = utf8(pool, bytes.u16()?)?.to_owned();
        let mut member = Member {
            access,
            name,
            descriptor,
            signature: None,
            parameter_names: Vec::new(),
            deprecation: None,
            annotations: Vec::new(),
        };
        let mut found = Deprecations::default();
        let attribute_count = bytes.u16()?;
        for _ in 0..attribute_count {
            let attribute_name = utf8(pool, bytes.u16()?)?;
            let length = bytes.u32()? as usize;
            let body = bytes.take(length)?;
            let mut body = Bytes {
                data: body,
                position: 0,
            };
            match attribute_name {
                "Signature" => member.signature = utf8(pool, body.u16()?).map(str::to_owned),
                "Deprecated" => found.in_javadoc = true,
                "MethodParameters" => {
                    let count = body.u8()?;
                    for _ in 0..count {
                        let name_index = body.u16()?;
                        body.u16()?;
                        let name = if name_index == 0 {
                            String::new()
                        } else {
                            utf8(pool, name_index).unwrap_or_default().to_owned()
                        };
                        member.parameter_names.push(name);
                    }
                }
                "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                    let annotations = read_annotations(&mut body, pool)?;
                    found.add(&annotations);
                    member
                        .annotations
                        .extend(annotations.others.iter().cloned());
                }
                _ => {}
            }
        }
        member.deprecation = found.deprecation();
        members.push(member);
    }
    Some(members)
}

fn read_inner_classes(bytes: &mut Bytes<'_>, pool: &[Constant]) -> Option<Vec<InnerClass>> {
    let count = bytes.u16()?;
    let mut inner_classes = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let inner = class_name(pool, bytes.u16()?)?;
        let outer_index = bytes.u16()?;
        let name_index = bytes.u16()?;
        let access = bytes.u16()?;
        inner_classes.push(InnerClass {
            inner,
            outer: (outer_index != 0)
                .then(|| class_name(pool, outer_index))
                .flatten(),
            simple_name: (name_index != 0)
                .then(|| utf8(pool, name_index).map(str::to_owned))
                .flatten(),
            access,
        });
    }
    Some(inner_classes)
}

#[derive(Default)]
struct Annotations {
    /// `@kotlin.Deprecated(…)`.
    kotlin_deprecation: Option<Deprecation>,
    /// `@java.lang.Deprecated`.
    java_deprecated: bool,
    since_kotlin: Option<DeprecatedSinceKotlin>,
    kotlin_metadata: Option<KotlinMetadata>,
    /// `@kotlin.RequiresOptIn(…)`.
    opt_in_marker: Option<OptInMarker>,
    /// The internal names of the other annotations that may matter to
    /// Kotlin (an opt-in marker); the JDK's, nullability's and the
    /// compiler's own are left out.
    others: Vec<String>,
}

/// What a declaration's attributes say about its deprecation, read in
/// whatever order they come.
#[derive(Default)]
struct Deprecations {
    kotlin: Option<Deprecation>,
    java_annotation: bool,
    /// The `Deprecated` attribute: a Javadoc `@deprecated`, or what the
    /// compiler adds beside an annotation.
    in_javadoc: bool,
    since_kotlin: Option<DeprecatedSinceKotlin>,
}

impl Deprecations {
    fn add(&mut self, annotations: &Annotations) {
        if annotations.kotlin_deprecation.is_some() {
            self.kotlin = annotations.kotlin_deprecation.clone();
        }
        self.java_annotation |= annotations.java_deprecated;
        if annotations.since_kotlin.is_some() {
            self.since_kotlin = annotations.since_kotlin.clone();
        }
    }

    /// Kotlin's annotation over Java's, Java's over the Javadoc tag.
    fn deprecation(self) -> Option<Deprecation> {
        if let Some(mut kotlin) = self.kotlin {
            kotlin.since_kotlin = self.since_kotlin;
            return Some(kotlin);
        }
        (self.java_annotation || self.in_javadoc).then(Deprecation::java)
    }
}

fn read_annotations(bytes: &mut Bytes<'_>, pool: &[Constant]) -> Option<Annotations> {
    let mut found = Annotations::default();
    let count = bytes.u16()?;
    for _ in 0..count {
        let type_name = utf8(pool, bytes.u16()?)?.to_owned();
        match type_name.as_str() {
            "Lkotlin/Metadata;" => found.kotlin_metadata = Some(read_metadata(bytes, pool)?),
            "Ljava/lang/Deprecated;" => {
                found.java_deprecated = true;
                skip_element_pairs(bytes, Nesting::TOP)?;
            }
            "Lkotlin/Deprecated;" => {
                found.kotlin_deprecation = Some(read_kotlin_deprecation(bytes, pool)?);
            }
            "Lkotlin/DeprecatedSinceKotlin;" => {
                found.since_kotlin = Some(read_deprecated_since_kotlin(bytes, pool)?);
            }
            "Lkotlin/RequiresOptIn;" => {
                found.opt_in_marker = Some(read_opt_in_marker(bytes, pool)?)
            }
            other => {
                if let Some(internal) = other
                    .strip_prefix('L')
                    .and_then(|rest| rest.strip_suffix(';'))
                    && may_mark_opt_in(internal)
                {
                    found.others.push(internal.to_owned());
                }
                skip_element_pairs(bytes, Nesting::TOP)?
            }
        }
    }
    Some(found)
}

/// `@kotlin.Deprecated(message, replaceWith = ReplaceWith(expression),
/// level)`.
fn read_kotlin_deprecation(bytes: &mut Bytes<'_>, pool: &[Constant]) -> Option<Deprecation> {
    let mut deprecation = Deprecation {
        level: DeprecationLevel::Warning,
        message: None,
        replace_with: None,
        origin: DeprecationOrigin::Kotlin,
        since_kotlin: None,
    };
    let pairs = bytes.u16()?;
    for _ in 0..pairs {
        let element = utf8(pool, bytes.u16()?)?.to_owned();
        let tag = bytes.data.get(bytes.position).copied();
        match (element.as_str(), tag) {
            ("message", Some(b's')) => {
                bytes.u8()?;
                deprecation.message = Some(utf8(pool, bytes.u16()?)?.into());
            }
            ("level", Some(b'e')) => {
                bytes.u8()?;
                bytes.u16()?;
                deprecation.level = match utf8(pool, bytes.u16()?)? {
                    "ERROR" => DeprecationLevel::Error,
                    "HIDDEN" => DeprecationLevel::Hidden,
                    _ => DeprecationLevel::Warning,
                };
            }
            ("replaceWith", Some(b'@')) => {
                bytes.u8()?;
                bytes.u16()?;
                deprecation.replace_with = read_string_element(bytes, pool, "expression")?;
            }
            _ => skip_element_value(bytes, Nesting::TOP)?,
        }
    }
    Some(deprecation)
}

/// Whether an annotation's class may be an opt-in marker: not the JDK's,
/// nullability's or the compiler's own, which are on nearly every
/// declaration.
fn may_mark_opt_in(internal: &str) -> bool {
    const NEVER: [&str; 8] = [
        "java/",
        "javax/",
        "jakarta/",
        "org/jetbrains/annotations/",
        "kotlin/jvm/",
        "kotlin/internal/",
        "kotlin/SinceKotlin",
        "kotlin/ReplaceWith",
    ];
    !NEVER.iter().any(|prefix| internal.starts_with(prefix))
}

/// `@RequiresOptIn(message, level)`: `ERROR` unless it says.
fn read_opt_in_marker(bytes: &mut Bytes<'_>, pool: &[Constant]) -> Option<OptInMarker> {
    let mut marker = OptInMarker {
        level: OptInLevel::Error,
        message: None,
    };
    let pairs = bytes.u16()?;
    for _ in 0..pairs {
        let element = utf8(pool, bytes.u16()?)?.to_owned();
        let tag = bytes.data.get(bytes.position).copied();
        match (element.as_str(), tag) {
            ("message", Some(b's')) => {
                bytes.u8()?;
                marker.message = Some(utf8(pool, bytes.u16()?)?.into());
            }
            ("level", Some(b'e')) => {
                bytes.u8()?;
                bytes.u16()?;
                if utf8(pool, bytes.u16()?)? == "WARNING" {
                    marker.level = OptInLevel::Warning;
                }
            }
            _ => skip_element_value(bytes, Nesting::TOP)?,
        }
    }
    Some(marker)
}

/// `@DeprecatedSinceKotlin(warningSince, errorSince, hiddenSince)`.
fn read_deprecated_since_kotlin(
    bytes: &mut Bytes<'_>,
    pool: &[Constant],
) -> Option<DeprecatedSinceKotlin> {
    let mut since = DeprecatedSinceKotlin::default();
    let pairs = bytes.u16()?;
    for _ in 0..pairs {
        let element = utf8(pool, bytes.u16()?)?.to_owned();
        let is_string = bytes.data.get(bytes.position) == Some(&b's');
        let slot = match element.as_str() {
            "warningSince" => Some(&mut since.warning_since),
            "errorSince" => Some(&mut since.error_since),
            "hiddenSince" => Some(&mut since.hidden_since),
            _ => None,
        };
        match slot {
            Some(slot) if is_string => {
                bytes.u8()?;
                *slot = Some(utf8(pool, bytes.u16()?)?.into());
            }
            _ => skip_element_value(bytes, Nesting::TOP)?,
        }
    }
    Some(since)
}

/// The string element `name` of a nested annotation's pairs; the rest
/// skipped.
fn read_string_element(
    bytes: &mut Bytes<'_>,
    pool: &[Constant],
    name: &str,
) -> Option<Option<Box<str>>> {
    let mut found = None;
    let pairs = bytes.u16()?;
    for _ in 0..pairs {
        let element = utf8(pool, bytes.u16()?)?.to_owned();
        let is_string = bytes.data.get(bytes.position) == Some(&b's');
        if element == name && is_string {
            bytes.u8()?;
            found = Some(utf8(pool, bytes.u16()?)?.into());
        } else {
            skip_element_value(bytes, Nesting::TOP.deeper()?)?;
        }
    }
    Some(found)
}

fn read_metadata(bytes: &mut Bytes<'_>, pool: &[Constant]) -> Option<KotlinMetadata> {
    let mut metadata = KotlinMetadata::default();
    let pairs = bytes.u16()?;
    for _ in 0..pairs {
        let element = utf8(pool, bytes.u16()?)?.to_owned();
        match element.as_str() {
            "k" => {
                let tag = bytes.u8()?;
                let index = bytes.u16()?;
                if tag == b'I'
                    && let Some(Constant::Integer(kind)) = pool.get(index as usize)
                {
                    metadata.kind = *kind;
                }
            }
            "pn" => {
                let tag = bytes.u8()?;
                let index = bytes.u16()?;
                if tag == b's' {
                    metadata.package_name = utf8(pool, index).map(str::to_owned);
                }
            }
            "d1" | "d2" => {
                let strings = read_string_array(bytes, pool)?;
                if element == "d1" {
                    metadata.data1 = strings;
                } else {
                    metadata.data2 = strings;
                }
            }
            _ => skip_element_value(bytes, Nesting::TOP)?,
        }
    }
    Some(metadata)
}

fn read_string_array(bytes: &mut Bytes<'_>, pool: &[Constant]) -> Option<Vec<String>> {
    if bytes.u8()? != b'[' {
        return None;
    }
    let count = bytes.u16()?;
    let mut strings = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let tag = bytes.u8()?;
        let index = bytes.u16()?;
        if tag != b's' {
            return None;
        }
        strings.push(utf8(pool, index)?.to_owned());
    }
    Some(strings)
}

/// Reads `@kotlin.Deprecated`'s arguments: whether `level` is `HIDDEN`.
fn skip_element_pairs(bytes: &mut Bytes<'_>, nesting: Nesting) -> Option<()> {
    let pairs = bytes.u16()?;
    for _ in 0..pairs {
        bytes.u16()?;
        skip_element_value(bytes, nesting)?;
    }
    Some(())
}

/// Skips one annotation element value; a nested annotation or array goes
/// one level deeper.
fn skip_element_value(bytes: &mut Bytes<'_>, nesting: Nesting) -> Option<()> {
    match bytes.u8()? {
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' | b'c' => {
            bytes.u16()?;
        }
        b'e' => {
            bytes.u16()?;
            bytes.u16()?;
        }
        b'@' => {
            bytes.u16()?;
            skip_element_pairs(bytes, nesting.deeper()?)?;
        }
        b'[' => {
            let count = bytes.u16()?;
            let nested = nesting.deeper()?;
            for _ in 0..count {
                skip_element_value(bytes, nested)?;
            }
        }
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_values_nested_past_the_limit_are_refused() {
        // `[` with one element, a hundred thousand times over.
        let mut data = Vec::new();
        for _ in 0..100_000 {
            data.extend_from_slice(&[b'[', 0, 1]);
        }
        data.extend_from_slice(&[b'I', 0, 1]);
        let mut bytes = Bytes {
            data: &data,
            position: 0,
        };
        assert!(skip_element_value(&mut bytes, Nesting::TOP).is_none());
        let shallow = [b'[', 0, 1, b'[', 0, 1, b'I', 0, 1];
        let mut shallow = Bytes {
            data: &shallow,
            position: 0,
        };
        assert!(skip_element_value(&mut shallow, Nesting::TOP).is_some());
    }

    #[test]
    fn modified_utf8_decodes_zero_and_two_byte_characters() {
        assert_eq!(
            decode_modified_utf8(&[0xC0, 0x80, b'a', 0xC3, 0xBF]),
            "\u{0}aÿ"
        );
        assert_eq!(latin1_bytes("\u{0}aÿ"), vec![0, b'a', 0xFF]);
    }
}
