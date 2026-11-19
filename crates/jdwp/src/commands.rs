//! The commands a debugger sends, typed: each writes its data and reads
//! its reply as the specification lays them out (command set, command).

use crate::events::{EventKind, Modifier, SuspendPolicy};
use crate::{
    Connection, Error, FieldId, FrameId, IdSizes, Location, MethodId, ObjectId, ReferenceTypeId,
    Tag, ThreadId, TypeTag, Value,
};

mod set {
    pub const VIRTUAL_MACHINE: u8 = 1;
    pub const REFERENCE_TYPE: u8 = 2;
    pub const CLASS_TYPE: u8 = 3;
    pub const METHOD: u8 = 6;
    pub const OBJECT_REFERENCE: u8 = 9;
    pub const STRING_REFERENCE: u8 = 10;
    pub const THREAD_REFERENCE: u8 = 11;
    pub const ARRAY_REFERENCE: u8 = 13;
    pub const EVENT_REQUEST: u8 = 15;
    pub const STACK_FRAME: u8 = 16;
}

/// A loaded class, interface or array type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReferenceType {
    pub type_tag: TypeTag,
    pub id: ReferenceTypeId,
    /// Its JNI signature: `Lcom/example/Order;`.
    pub signature: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    pub id: FieldId,
    pub name: String,
    pub signature: String,
    pub modifiers: i32,
}

impl Field {
    pub fn is_static(&self) -> bool {
        self.modifiers & 0x0008 != 0
    }

    /// Made by the compiler (`this$0`, `$$delegatedProperties`).
    pub fn is_synthetic(&self) -> bool {
        self.modifiers & 0x1000 != 0
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Method {
    pub id: MethodId,
    pub name: String,
    pub signature: String,
    pub modifiers: i32,
}

/// Which bytecode indices start which source lines.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LineTable {
    pub start: u64,
    pub end: u64,
    /// `(code index, line)`, by code index.
    pub lines: Vec<(u64, i32)>,
}

/// A local variable or parameter, live over `[start, start + length)`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Variable {
    pub start: u64,
    pub name: String,
    pub signature: String,
    pub length: u32,
    pub slot: i32,
}

impl Variable {
    pub fn is_live_at(&self, index: u64) -> bool {
        self.start <= index && index < self.start + u64::from(self.length)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StackFrame {
    pub id: FrameId,
    pub location: Location,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThreadStatus {
    /// `ThreadStatus`: 0 zombie, 1 running, 2 sleeping, 3 monitor, 4 wait.
    pub status: i32,
    pub suspended: bool,
}

impl Connection {
    pub(crate) fn id_sizes(&self) -> Result<IdSizes, Error> {
        let reply = self.command(set::VIRTUAL_MACHINE, 7, |_| {})?;
        let mut reader = self.reader(&reply);
        let mut size = || -> Result<usize, Error> { reader.count() };
        Ok(IdSizes {
            field: size()?,
            method: size()?,
            object: size()?,
            reference_type: size()?,
            frame: size()?,
        })
    }

    /// The VM's description and version: `(description, jdwp major,
    /// jdwp minor, vm version, vm name)`.
    pub fn version(&self) -> Result<(String, i32, i32, String, String), Error> {
        let reply = self.command(set::VIRTUAL_MACHINE, 1, |_| {})?;
        let mut reader = self.reader(&reply);
        Ok((
            reader.string()?,
            reader.int()?,
            reader.int()?,
            reader.string()?,
            reader.string()?,
        ))
    }

    /// The loaded types with this JNI signature (several class loaders may
    /// each load one).
    pub fn classes_by_signature(&self, signature: &str) -> Result<Vec<ReferenceType>, Error> {
        let reply = self.command(set::VIRTUAL_MACHINE, 2, |writer| {
            writer.string(signature);
        })?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        let mut types = Vec::with_capacity(count);
        for _ in 0..count {
            let type_tag = reader.type_tag()?;
            let id = reader.reference_type()?;
            let _status = reader.int()?;
            types.push(ReferenceType {
                type_tag,
                id,
                signature: signature.to_owned(),
            });
        }
        Ok(types)
    }

    /// Every loaded type.
    pub fn all_classes(&self) -> Result<Vec<ReferenceType>, Error> {
        let reply = self.command(set::VIRTUAL_MACHINE, 3, |_| {})?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        let mut types = Vec::with_capacity(count);
        for _ in 0..count {
            let type_tag = reader.type_tag()?;
            let id = reader.reference_type()?;
            let signature = reader.string()?;
            let _status = reader.int()?;
            types.push(ReferenceType {
                type_tag,
                id,
                signature,
            });
        }
        Ok(types)
    }

    pub fn all_threads(&self) -> Result<Vec<ThreadId>, Error> {
        let reply = self.command(set::VIRTUAL_MACHINE, 4, |_| {})?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        (0..count).map(|_| reader.object()).collect()
    }

    pub fn suspend_all(&self) -> Result<(), Error> {
        self.command(set::VIRTUAL_MACHINE, 8, |_| {}).map(|_| ())
    }

    pub fn resume_all(&self) -> Result<(), Error> {
        self.command(set::VIRTUAL_MACHINE, 9, |_| {}).map(|_| ())
    }

    /// Ends the VM with `code`.
    pub fn exit(&self, code: i32) -> Result<(), Error> {
        self.command(set::VIRTUAL_MACHINE, 10, |writer| {
            writer.int(code);
        })
        .map(|_| ())
    }

    /// Lets the VM go on without a debugger.
    pub fn dispose(&self) -> Result<(), Error> {
        self.command(set::VIRTUAL_MACHINE, 6, |_| {}).map(|_| ())
    }

    pub fn signature(&self, class: ReferenceTypeId) -> Result<String, Error> {
        let reply = self.command(set::REFERENCE_TYPE, 1, |writer| {
            writer.reference_type(class);
        })?;
        self.reader(&reply).string()
    }

    pub fn fields(&self, class: ReferenceTypeId) -> Result<Vec<Field>, Error> {
        let reply = self.command(set::REFERENCE_TYPE, 4, |writer| {
            writer.reference_type(class);
        })?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        let mut fields = Vec::with_capacity(count);
        for _ in 0..count {
            fields.push(Field {
                id: reader.field()?,
                name: reader.string()?,
                signature: reader.string()?,
                modifiers: reader.int()?,
            });
        }
        Ok(fields)
    }

    pub fn methods(&self, class: ReferenceTypeId) -> Result<Vec<Method>, Error> {
        let reply = self.command(set::REFERENCE_TYPE, 5, |writer| {
            writer.reference_type(class);
        })?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        let mut methods = Vec::with_capacity(count);
        for _ in 0..count {
            methods.push(Method {
                id: reader.method()?,
                name: reader.string()?,
                signature: reader.string()?,
                modifiers: reader.int()?,
            });
        }
        Ok(methods)
    }

    /// Static fields' values.
    pub fn static_values(
        &self,
        class: ReferenceTypeId,
        fields: &[FieldId],
    ) -> Result<Vec<Value>, Error> {
        let reply = self.command(set::REFERENCE_TYPE, 6, |writer| {
            writer.reference_type(class);
            writer.int(fields.len() as i32);
            for field in fields {
                writer.field(*field);
            }
        })?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        (0..count).map(|_| reader.value()).collect()
    }

    /// The name of the source file the class was compiled from
    /// (`Order.kt`), without its directory.
    pub fn source_file(&self, class: ReferenceTypeId) -> Result<String, Error> {
        let reply = self.command(set::REFERENCE_TYPE, 7, |writer| {
            writer.reference_type(class);
        })?;
        self.reader(&reply).string()
    }

    /// The class's `SourceDebugExtension` (Kotlin's SMAP for inlined code).
    pub fn source_debug_extension(&self, class: ReferenceTypeId) -> Result<String, Error> {
        let reply = self.command(set::REFERENCE_TYPE, 12, |writer| {
            writer.reference_type(class);
        })?;
        self.reader(&reply).string()
    }

    pub fn superclass(&self, class: ReferenceTypeId) -> Result<Option<ReferenceTypeId>, Error> {
        let reply = self.command(set::CLASS_TYPE, 1, |writer| {
            writer.reference_type(class);
        })?;
        let superclass = self.reader(&reply).reference_type()?;
        Ok((superclass.0 != 0).then_some(superclass))
    }

    pub fn line_table(&self, class: ReferenceTypeId, method: MethodId) -> Result<LineTable, Error> {
        let reply = self.command(set::METHOD, 1, |writer| {
            writer.reference_type(class).method(method);
        })?;
        let mut reader = self.reader(&reply);
        let start = reader.long()? as u64;
        let end = reader.long()? as u64;
        let count = reader.count()?;
        let mut lines = Vec::with_capacity(count);
        for _ in 0..count {
            lines.push((reader.long()? as u64, reader.int()?));
        }
        lines.sort();
        Ok(LineTable { start, end, lines })
    }

    pub fn variable_table(
        &self,
        class: ReferenceTypeId,
        method: MethodId,
    ) -> Result<Vec<Variable>, Error> {
        let reply = self.command(set::METHOD, 2, |writer| {
            writer.reference_type(class).method(method);
        })?;
        let mut reader = self.reader(&reply);
        let _arguments = reader.int()?;
        let count = reader.count()?;
        let mut variables = Vec::with_capacity(count);
        for _ in 0..count {
            variables.push(Variable {
                start: reader.long()? as u64,
                name: reader.string()?,
                signature: reader.string()?,
                length: reader.int()? as u32,
                slot: reader.int()?,
            });
        }
        Ok(variables)
    }

    /// The object's runtime type.
    pub fn reference_type(&self, object: ObjectId) -> Result<ReferenceTypeId, Error> {
        let reply = self.command(set::OBJECT_REFERENCE, 1, |writer| {
            writer.object(object);
        })?;
        let mut reader = self.reader(&reply);
        let _tag = reader.type_tag()?;
        reader.reference_type()
    }

    pub fn object_values(&self, object: ObjectId, fields: &[FieldId]) -> Result<Vec<Value>, Error> {
        let reply = self.command(set::OBJECT_REFERENCE, 2, |writer| {
            writer.object(object);
            writer.int(fields.len() as i32);
            for field in fields {
                writer.field(*field);
            }
        })?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        (0..count).map(|_| reader.value()).collect()
    }

    /// Calls `method` on `object` in `thread` (suspended by an event), its
    /// result and the exception it threw, if any.
    pub fn invoke_method(
        &self,
        object: ObjectId,
        thread: ThreadId,
        class: ReferenceTypeId,
        method: MethodId,
        arguments: &[Value],
    ) -> Result<(Value, Option<ObjectId>), Error> {
        let reply = self.command(set::OBJECT_REFERENCE, 6, |writer| {
            writer
                .object(object)
                .object(thread)
                .reference_type(class)
                .method(method)
                .int(arguments.len() as i32);
            for argument in arguments {
                write_value(writer, argument);
            }
            // INVOKE_SINGLE_THREADED: the others stay suspended.
            writer.int(1);
        })?;
        let mut reader = self.reader(&reply);
        let result = reader.value()?;
        let _tag = reader.byte()?;
        let exception = reader.object()?;
        Ok((result, (exception.0 != 0).then_some(exception)))
    }

    pub fn string_value(&self, string: ObjectId) -> Result<String, Error> {
        let reply = self.command(set::STRING_REFERENCE, 1, |writer| {
            writer.object(string);
        })?;
        self.reader(&reply).string()
    }

    pub fn thread_name(&self, thread: ThreadId) -> Result<String, Error> {
        let reply = self.command(set::THREAD_REFERENCE, 1, |writer| {
            writer.object(thread);
        })?;
        self.reader(&reply).string()
    }

    pub fn suspend_thread(&self, thread: ThreadId) -> Result<(), Error> {
        self.command(set::THREAD_REFERENCE, 2, |writer| {
            writer.object(thread);
        })
        .map(|_| ())
    }

    pub fn resume_thread(&self, thread: ThreadId) -> Result<(), Error> {
        self.command(set::THREAD_REFERENCE, 3, |writer| {
            writer.object(thread);
        })
        .map(|_| ())
    }

    pub fn thread_status(&self, thread: ThreadId) -> Result<ThreadStatus, Error> {
        let reply = self.command(set::THREAD_REFERENCE, 4, |writer| {
            writer.object(thread);
        })?;
        let mut reader = self.reader(&reply);
        Ok(ThreadStatus {
            status: reader.int()?,
            suspended: reader.int()? & 1 != 0,
        })
    }

    /// The frames of a suspended thread's stack, the innermost first.
    pub fn frames(&self, thread: ThreadId) -> Result<Vec<StackFrame>, Error> {
        let reply = self.command(set::THREAD_REFERENCE, 6, |writer| {
            writer.object(thread).int(0).int(-1);
        })?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        let mut frames = Vec::with_capacity(count);
        for _ in 0..count {
            frames.push(StackFrame {
                id: reader.frame()?,
                location: reader.location()?,
            });
        }
        Ok(frames)
    }

    pub fn array_length(&self, array: ObjectId) -> Result<usize, Error> {
        let reply = self.command(set::ARRAY_REFERENCE, 1, |writer| {
            writer.object(array);
        })?;
        self.reader(&reply).count()
    }

    pub fn array_values(
        &self,
        array: ObjectId,
        first: usize,
        length: usize,
    ) -> Result<Vec<Value>, Error> {
        let reply = self.command(set::ARRAY_REFERENCE, 2, |writer| {
            writer.object(array).int(first as i32).int(length as i32);
        })?;
        let mut reader = self.reader(&reply);
        let tag = Tag::of(reader.byte()?)?;
        let count = reader.count()?;
        (0..count)
            .map(|_| {
                if tag.is_primitive() {
                    reader.untagged_value(tag)
                } else {
                    reader.value()
                }
            })
            .collect()
    }

    /// Asks for events of `kind` narrowed by `modifiers`: the request's id.
    pub fn set_event_request(
        &self,
        kind: EventKind,
        suspend: SuspendPolicy,
        modifiers: &[Modifier],
    ) -> Result<i32, Error> {
        let reply = self.command(set::EVENT_REQUEST, 1, |writer| {
            writer
                .byte(kind as u8)
                .byte(suspend as u8)
                .int(modifiers.len() as i32);
            for modifier in modifiers {
                modifier.write(writer);
            }
        })?;
        self.reader(&reply).int()
    }

    pub fn clear_event_request(&self, kind: EventKind, request: i32) -> Result<(), Error> {
        self.command(set::EVENT_REQUEST, 2, |writer| {
            writer.byte(kind as u8).int(request);
        })
        .map(|_| ())
    }

    /// Local variables' values in a frame, by slot and the signature of
    /// their type.
    pub fn frame_values(
        &self,
        thread: ThreadId,
        frame: FrameId,
        slots: &[(i32, Tag)],
    ) -> Result<Vec<Value>, Error> {
        let reply = self.command(set::STACK_FRAME, 1, |writer| {
            writer.object(thread).frame(frame).int(slots.len() as i32);
            for (slot, tag) in slots {
                writer.int(*slot).byte(*tag as u8);
            }
        })?;
        let mut reader = self.reader(&reply);
        let count = reader.count()?;
        (0..count).map(|_| reader.value()).collect()
    }

    /// The frame's `this`; `None` in a static method.
    pub fn this_object(&self, thread: ThreadId, frame: FrameId) -> Result<Option<ObjectId>, Error> {
        let reply = self.command(set::STACK_FRAME, 3, |writer| {
            writer.object(thread).frame(frame);
        })?;
        let mut reader = self.reader(&reply);
        let _tag = reader.byte()?;
        let object = reader.object()?;
        Ok((object.0 != 0).then_some(object))
    }
}

fn write_value(writer: &mut crate::Writer, value: &Value) {
    match value {
        Value::Byte(value) => {
            writer.byte(b'B').byte(*value as u8);
        }
        Value::Boolean(value) => {
            writer.byte(b'Z').boolean(*value);
        }
        Value::Char(value) => {
            writer.byte(b'C');
            writer.bytes.extend_from_slice(&value.to_be_bytes());
        }
        Value::Short(value) => {
            writer.byte(b'S');
            writer.bytes.extend_from_slice(&value.to_be_bytes());
        }
        Value::Int(value) => {
            writer.byte(b'I').int(*value);
        }
        Value::Long(value) => {
            writer.byte(b'J').long(*value);
        }
        Value::Float(value) => {
            writer.byte(b'F').int(value.to_bits() as i32);
        }
        Value::Double(value) => {
            writer.byte(b'D').long(value.to_bits() as i64);
        }
        Value::Void => {
            writer.byte(b'V');
        }
        Value::Null => {
            writer.byte(b'L').object(ObjectId(0));
        }
        Value::Object { tag, id } => {
            writer.byte(*tag as u8).object(*id);
        }
    }
}
