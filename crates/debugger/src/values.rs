//! Values as a person reads them: `42`, `"text"`, `int[3] {1, 2, 3}`,
//! `Status.ACTIVE`, `ArrayList(size = 2)`, `Order@1f` — and the fields or
//! elements an object opens into.

use std::collections::HashMap;

use jdwp::{Connection, Error, Field, ObjectId, ReferenceTypeId, Tag, Value};

/// The most of a string shown.
const STRING_LIMIT: usize = 400;
/// The most elements an array or a list opens into, and shows in its
/// preview.
pub const ELEMENT_LIMIT: usize = 100;
const PREVIEW_LIMIT: usize = 8;

/// A named value: a variable, a field, an element.
#[derive(Clone, PartialEq, Debug)]
pub struct Shown {
    pub name: String,
    /// The value as text.
    pub value: String,
    /// Its type, simply: `String`, `int[]`, `Order`.
    pub type_name: String,
    /// What it opens into, if anything.
    pub children: Option<ObjectId>,
}

/// The JDK's boxes of primitives: each holds its value in `value`.
pub const BOXED: &[&str] = &[
    "Ljava/lang/Integer;",
    "Ljava/lang/Long;",
    "Ljava/lang/Short;",
    "Ljava/lang/Byte;",
    "Ljava/lang/Double;",
    "Ljava/lang/Float;",
    "Ljava/lang/Boolean;",
    "Ljava/lang/Character;",
];

/// Reads values, remembering each class's fields and names.
#[derive(Default)]
pub struct Inspector {
    fields: HashMap<ReferenceTypeId, Vec<(ReferenceTypeId, Field)>>,
    signatures: HashMap<ReferenceTypeId, String>,
    /// The thread an event stopped: `toString()` may be called in it.
    invoking_thread: Option<jdwp::ThreadId>,
}

impl Inspector {
    /// An inspector that may call `toString()` in `thread`, stopped by an
    /// event (the JVM runs a call only in such a thread).
    pub fn calling_in(thread: Option<jdwp::ThreadId>) -> Inspector {
        Inspector {
            invoking_thread: thread,
            ..Inspector::default()
        }
    }

    /// The text `toString()` gives, for the objects whose `toString()` is
    /// known to only show their state: the JDK's value types (`BigDecimal`,
    /// `UUID`, `Instant`…) and Kotlin's data classes. Not others': a call
    /// runs the program's code.
    fn text_by_to_string(
        &mut self,
        connection: &Connection,
        object: ObjectId,
        class: ReferenceTypeId,
        signature: &str,
    ) -> Option<String> {
        let thread = self.invoking_thread?;
        let is_value_type = [
            "Ljava/math/",
            "Ljava/time/",
            "Ljava/util/UUID;",
            "Ljava/net/URI;",
            "Ljava/util/Locale;",
            "Ljava/util/Currency;",
        ]
        .iter()
        .any(|prefix| signature.starts_with(prefix));
        let methods = connection.methods(class).ok()?;
        let is_data_class = methods.iter().any(|method| method.name == "copy")
            && methods.iter().any(|method| method.name == "component1");
        if !is_value_type && !is_data_class {
            return None;
        }
        let (declaring, method) = self.method_to_string(connection, class)?;
        let (result, exception) = connection
            .invoke_method(object, thread, declaring, method, &[])
            .ok()?;
        if exception.is_some() {
            return None;
        }
        match result {
            Value::Object {
                tag: Tag::String,
                id,
            } => connection
                .string_value(id)
                .ok()
                .map(|text| shortened(&text)),
            _ => None,
        }
    }

    /// The `toString()` the class has: its own or a superclass's.
    fn method_to_string(
        &mut self,
        connection: &Connection,
        class: ReferenceTypeId,
    ) -> Option<(ReferenceTypeId, jdwp::MethodId)> {
        let mut current = Some(class);
        while let Some(declaring) = current {
            let methods = connection.methods(declaring).ok()?;
            let found = methods.iter().find(|method| {
                method.name == "toString" && method.signature == "()Ljava/lang/String;"
            });
            if let Some(method) = found {
                return Some((declaring, method.id));
            }
            current = connection.superclass(declaring).ok().flatten();
        }
        None
    }
    pub fn signature(
        &mut self,
        connection: &Connection,
        class: ReferenceTypeId,
    ) -> Result<String, Error> {
        if let Some(signature) = self.signatures.get(&class) {
            return Ok(signature.clone());
        }
        let signature = connection.signature(class)?;
        self.signatures.insert(class, signature.clone());
        Ok(signature)
    }

    /// The instance fields of a class and its superclasses, the class's
    /// own first, each with the class declaring it.
    pub fn instance_fields(
        &mut self,
        connection: &Connection,
        class: ReferenceTypeId,
    ) -> Result<Vec<(ReferenceTypeId, Field)>, Error> {
        if let Some(fields) = self.fields.get(&class) {
            return Ok(fields.clone());
        }
        let mut fields = Vec::new();
        let mut current = Some(class);
        while let Some(declaring) = current {
            for field in connection.fields(declaring)? {
                if !field.is_static() {
                    fields.push((declaring, field));
                }
            }
            current = connection.superclass(declaring).unwrap_or(None);
        }
        self.fields.insert(class, fields.clone());
        Ok(fields)
    }

    /// The value of the field `name` of `object`, if it has one.
    pub fn field_value(
        &mut self,
        connection: &Connection,
        object: ObjectId,
        name: &str,
    ) -> Result<Option<Value>, Error> {
        let class = connection.reference_type(object)?;
        let fields = self.instance_fields(connection, class)?;
        let Some((_, field)) = fields.iter().find(|(_, field)| field.name == name) else {
            return Ok(None);
        };
        Ok(connection
            .object_values(object, &[field.id])?
            .into_iter()
            .next())
    }

    /// `value` named `name`, as shown.
    pub fn show(&mut self, connection: &Connection, name: &str, value: Value) -> Shown {
        let (text, type_name, children) = self
            .describe(connection, value)
            .unwrap_or_else(|error| (format!("<{error}>"), String::new(), None));
        Shown {
            name: name.to_owned(),
            value: text,
            type_name,
            children,
        }
    }

    fn describe(
        &mut self,
        connection: &Connection,
        value: Value,
    ) -> Result<(String, String, Option<ObjectId>), Error> {
        Ok(match value {
            Value::Byte(number) => (number.to_string(), "byte".to_owned(), None),
            Value::Short(number) => (number.to_string(), "short".to_owned(), None),
            Value::Int(number) => (number.to_string(), "int".to_owned(), None),
            Value::Long(number) => (number.to_string(), "long".to_owned(), None),
            Value::Float(number) => (number.to_string(), "float".to_owned(), None),
            Value::Double(number) => (number.to_string(), "double".to_owned(), None),
            Value::Boolean(flag) => (flag.to_string(), "boolean".to_owned(), None),
            Value::Char(unit) => (
                format!("'{}'", char::from_u32(u32::from(unit)).unwrap_or('?')),
                "char".to_owned(),
                None,
            ),
            Value::Void => ("void".to_owned(), "void".to_owned(), None),
            Value::Null => ("null".to_owned(), String::new(), None),
            Value::Object {
                tag: Tag::String,
                id,
            } => {
                let text = connection.string_value(id)?;
                (quoted(&text), "String".to_owned(), None)
            }
            Value::Object {
                tag: Tag::Array,
                id,
            } => {
                let class = connection.reference_type(id)?;
                let signature = self.signature(connection, class)?;
                let length = connection.array_length(id)?;
                let element_type = type_name(&signature[1..]);
                let shown = length.min(PREVIEW_LIMIT);
                let elements = connection.array_values(id, 0, shown)?;
                let preview: Vec<String> = elements
                    .into_iter()
                    .map(|element| self.brief(connection, element))
                    .collect();
                let more = if length > shown { ", …" } else { "" };
                (
                    format!("{element_type}[{length}] {{{}{more}}}", preview.join(", ")),
                    format!("{element_type}[]"),
                    (length > 0).then_some(id),
                )
            }
            Value::Object { id, .. } => {
                let class = connection.reference_type(id)?;
                let signature = self.signature(connection, class)?;
                let type_name = type_name(&signature);
                (
                    self.object_text(connection, id, &signature, &type_name)?,
                    type_name,
                    Some(id),
                )
            }
        })
    }

    /// An object's text: a boxed value's value, an enum's name, a
    /// collection's size, else its type and id.
    fn object_text(
        &mut self,
        connection: &Connection,
        object: ObjectId,
        signature: &str,
        type_name: &str,
    ) -> Result<String, Error> {
        let boxed = BOXED.contains(&signature);
        if boxed && let Some(value) = self.field_value(connection, object, "value")? {
            return Ok(self.brief(connection, value));
        }
        let class = connection.reference_type(object)?;
        if let Some(text) = self.text_by_to_string(connection, object, class, signature) {
            return Ok(text);
        }
        let fields = self.instance_fields(connection, class)?;
        // `java.lang.Enum` declares `name` for every enum entry.
        let mut is_enum = false;
        for (declaring, field) in &fields {
            if field.name == "name" && self.signature(connection, *declaring)? == "Ljava/lang/Enum;"
            {
                is_enum = true;
            }
        }
        if is_enum
            && let Some(Value::Object { id, .. }) = self.field_value(connection, object, "name")?
        {
            return Ok(format!("{type_name}.{}", connection.string_value(id)?));
        }
        let is_collection = signature.starts_with("Ljava/util/")
            && fields.iter().any(|(_, field)| field.name == "size");
        if is_collection
            && let Some(Value::Int(size)) = self.field_value(connection, object, "size")?
        {
            return Ok(format!("{type_name}(size = {size})"));
        }
        Ok(format!("{type_name}@{:x}", object.0))
    }

    /// A short text for a preview: primitives and strings as themselves,
    /// objects by type.
    fn brief(&mut self, connection: &Connection, value: Value) -> String {
        match value {
            Value::Object {
                tag: Tag::String,
                id,
            } => connection
                .string_value(id)
                .map(|text| quoted(&text))
                .unwrap_or_else(|_| "\"…\"".to_owned()),
            Value::Object { id, .. } => connection
                .reference_type(id)
                .and_then(|class| self.signature(connection, class))
                .map(|signature| type_name(&signature))
                .unwrap_or_else(|_| "?".to_owned()),
            primitive => self.show(connection, "", primitive).value,
        }
    }

    /// What an object or array opens into: its fields, or its first
    /// elements (an `ArrayList` its elements too).
    pub fn children(
        &mut self,
        connection: &Connection,
        object: ObjectId,
    ) -> Result<Vec<Shown>, Error> {
        let class = connection.reference_type(object)?;
        let signature = self.signature(connection, class)?;
        if signature.starts_with('[') {
            let length = connection.array_length(object)?.min(ELEMENT_LIMIT);
            let elements = connection.array_values(object, 0, length)?;
            return Ok(elements
                .into_iter()
                .enumerate()
                .map(|(index, element)| self.show(connection, &format!("[{index}]"), element))
                .collect());
        }
        let mut shown = Vec::new();
        let is_array_list = matches!(
            signature.as_str(),
            "Ljava/util/ArrayList;" | "Ljava/util/Arrays$ArrayList;"
        );
        if is_array_list {
            let size = match self.field_value(connection, object, "size")? {
                Some(Value::Int(size)) => usize::try_from(size).unwrap_or(0),
                _ => 0,
            };
            let data = self
                .field_value(connection, object, "elementData")?
                .or(self.field_value(connection, object, "a")?);
            if let Some(Value::Object { id, .. }) = data {
                let count = size.min(ELEMENT_LIMIT).min(connection.array_length(id)?);
                let elements = connection.array_values(id, 0, count)?;
                for (index, element) in elements.into_iter().enumerate() {
                    shown.push(self.show(connection, &format!("[{index}]"), element));
                }
                return Ok(shown);
            }
        }
        let fields = self.instance_fields(connection, class)?;
        let ids: Vec<jdwp::FieldId> = fields.iter().map(|(_, field)| field.id).collect();
        let values = connection.object_values(object, &ids)?;
        for ((_, field), value) in fields.iter().zip(values) {
            shown.push(self.show(connection, &field.name, value));
        }
        Ok(shown)
    }
}

/// `text` cut to `STRING_LIMIT` characters.
fn shortened(text: &str) -> String {
    let mut shown: String = text.chars().take(STRING_LIMIT).collect();
    if text.chars().count() > STRING_LIMIT {
        shown.push('…');
    }
    shown
}

fn quoted(text: &str) -> String {
    let shown = shortened(text);
    format!(
        "\"{}\"",
        shown
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

/// A JNI type signature's simple name: `String` for
/// `Ljava/lang/String;`, `Order.Line` for `Lshop/Order$Line;`, `int[]`.
pub fn type_name(signature: &str) -> String {
    match signature.as_bytes().first() {
        Some(b'[') => format!("{}[]", type_name(&signature[1..])),
        Some(b'L') => {
            let name = signature[1..].trim_end_matches(';');
            let simple = name.rsplit('/').next().unwrap_or(name);
            simple.replace('$', ".")
        }
        Some(b'I') => "int".to_owned(),
        Some(b'J') => "long".to_owned(),
        Some(b'Z') => "boolean".to_owned(),
        Some(b'B') => "byte".to_owned(),
        Some(b'C') => "char".to_owned(),
        Some(b'S') => "short".to_owned(),
        Some(b'F') => "float".to_owned(),
        Some(b'D') => "double".to_owned(),
        Some(b'V') => "void".to_owned(),
        _ => signature.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_read_as_simple_type_names() {
        assert_eq!(type_name("Ljava/lang/String;"), "String");
        assert_eq!(type_name("Lshop/Order$Line;"), "Order.Line");
        assert_eq!(type_name("[[I"), "int[][]");
        assert_eq!(type_name("[Ljava/util/List;"), "List[]");
    }

    #[test]
    fn strings_are_quoted_escaped_and_cut() {
        assert_eq!(quoted("a \"b\"\nc"), "\"a \\\"b\\\"\\nc\"");
        let long = "x".repeat(STRING_LIMIT + 5);
        assert!(quoted(&long).ends_with("x…\""));
    }
}
