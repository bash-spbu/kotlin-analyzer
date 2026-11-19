//! A client of the Java Debug Wire Protocol (JDWP): what a debugger says
//! to a JVM started with `-agentlib:jdwp`, over a socket — as the protocol
//! specification of OpenJDK describes it.
//!
//! [`Connection`] does the handshake, sends commands and waits for their
//! replies (any thread may ask), and hands the VM's events to whoever reads
//! [`Connection::events`]. The commands a debugger needs are its typed
//! methods (`commands.rs`); the events are [`Event`] (`events.rs`).

mod commands;
mod events;
mod wire;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use commands::{Field, LineTable, Method, ReferenceType, StackFrame, ThreadStatus, Variable};
pub use events::{Event, EventKind, EventSet, Modifier, StepDepth, SuspendPolicy};
pub use wire::{Reader, Writer};

/// How long a command waits for its reply.
const REPLY_PATIENCE: Duration = Duration::from_secs(30);

const HANDSHAKE: &[u8] = b"JDWP-Handshake";

macro_rules! id_type {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
            pub struct $name(pub u64);
        )*
    };
}

id_type!(
    /// An object, thread, array, string or class object in the VM.
    ObjectId,
    /// A class, interface or array type.
    ReferenceTypeId,
    MethodId,
    FieldId,
    /// A frame of a suspended thread's stack: valid until it resumes.
    FrameId,
);

/// A thread is an object.
pub type ThreadId = ObjectId;

/// How wide the VM's ids are, in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdSizes {
    pub field: usize,
    pub method: usize,
    pub object: usize,
    pub reference_type: usize,
    pub frame: usize,
}

/// What a reference type is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum TypeTag {
    Class = 1,
    Interface = 2,
    Array = 3,
}

impl TypeTag {
    fn of(byte: u8) -> Result<TypeTag, Error> {
        match byte {
            1 => Ok(TypeTag::Class),
            2 => Ok(TypeTag::Interface),
            3 => Ok(TypeTag::Array),
            _ => Err(Error::Malformed),
        }
    }
}

/// A place in code: a method of a class and a bytecode index in it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Location {
    pub type_tag: TypeTag,
    pub class: ReferenceTypeId,
    pub method: MethodId,
    pub index: u64,
}

/// A value's type, as JDWP tags it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Tag {
    Array = b'[',
    Byte = b'B',
    Char = b'C',
    Object = b'L',
    Float = b'F',
    Double = b'D',
    Int = b'I',
    Long = b'J',
    Short = b'S',
    Void = b'V',
    Boolean = b'Z',
    String = b's',
    Thread = b't',
    ThreadGroup = b'g',
    ClassLoader = b'l',
    ClassObject = b'c',
}

impl Tag {
    fn of(byte: u8) -> Result<Tag, Error> {
        Ok(match byte {
            b'[' => Tag::Array,
            b'B' => Tag::Byte,
            b'C' => Tag::Char,
            b'L' => Tag::Object,
            b'F' => Tag::Float,
            b'D' => Tag::Double,
            b'I' => Tag::Int,
            b'J' => Tag::Long,
            b'S' => Tag::Short,
            b'V' => Tag::Void,
            b'Z' => Tag::Boolean,
            b's' => Tag::String,
            b't' => Tag::Thread,
            b'g' => Tag::ThreadGroup,
            b'l' => Tag::ClassLoader,
            b'c' => Tag::ClassObject,
            _ => return Err(Error::Malformed),
        })
    }

    /// The tag of a value of a type with this JNI signature (`I`,
    /// `Ljava/lang/String;`, `[I`): its first character.
    pub fn of_signature(signature: &str) -> Tag {
        match signature.as_bytes().first() {
            Some(b'[') => Tag::Array,
            Some(byte) => Tag::of(*byte).unwrap_or(Tag::Object),
            None => Tag::Object,
        }
    }

    pub fn is_primitive(self) -> bool {
        matches!(
            self,
            Tag::Byte
                | Tag::Char
                | Tag::Float
                | Tag::Double
                | Tag::Int
                | Tag::Long
                | Tag::Short
                | Tag::Boolean
        )
    }
}

/// A value in the VM.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Value {
    Byte(i8),
    Boolean(bool),
    Char(u16),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Void,
    Null,
    /// An object of the kind `tag` says (`String`, `Array`, `Object`…).
    Object {
        tag: Tag,
        id: ObjectId,
    },
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The VM answered with a JDWP error code (`ABSENT_INFORMATION` is
    /// 101, `INVALID_OBJECT` 20, `THREAD_NOT_SUSPENDED` 13…).
    Vm(u16),
    /// The other side isn't a JDWP VM, or sent what can't be read.
    Malformed,
    /// No reply in time.
    TimedOut,
    /// The connection is closed (the VM exited).
    Closed,
}

impl Error {
    pub const ABSENT_INFORMATION: u16 = 101;
    pub const INVALID_OBJECT: u16 = 20;

    /// The VM has no such information (`SourceFile` of a class compiled
    /// without it, `VariableTable` without `-g`).
    pub fn is_absent_information(&self) -> bool {
        matches!(self, Error::Vm(Error::ABSENT_INFORMATION))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(error) => write!(formatter, "{error}"),
            Error::Vm(code) => write!(formatter, "the JVM answered error {code}"),
            Error::Malformed => formatter.write_str("unreadable JDWP data"),
            Error::TimedOut => formatter.write_str("the JVM didn't answer in time"),
            Error::Closed => formatter.write_str("the JVM is gone"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Io(error)
    }
}

type Reply = Result<Vec<u8>, Error>;

/// A connection to a JVM.
pub struct Connection {
    writer: Mutex<TcpStream>,
    next_id: AtomicU32,
    waiting: Arc<Mutex<HashMap<u32, std::sync::mpsc::SyncSender<Reply>>>>,
    events: Mutex<Option<std::sync::mpsc::Receiver<EventSet>>>,
    sizes: IdSizes,
}

impl Connection {
    /// Does the handshake on `stream` — connected either way: the VM
    /// listening (`server=y`) or the VM having connected (`server=n`) —
    /// and asks the VM's id sizes.
    pub fn open(stream: TcpStream) -> Result<Connection, Error> {
        let mut stream = stream;
        stream.set_nodelay(true)?;
        stream.write_all(HANDSHAKE)?;
        let mut answer = [0u8; 14];
        stream.read_exact(&mut answer)?;
        if answer != HANDSHAKE {
            return Err(Error::Malformed);
        }
        let reader = stream.try_clone()?;
        let waiting: Arc<Mutex<HashMap<u32, std::sync::mpsc::SyncSender<Reply>>>> = Arc::default();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let provisional = IdSizes {
            field: 8,
            method: 8,
            object: 8,
            reference_type: 8,
            frame: 8,
        };
        let sizes_cell: Arc<Mutex<IdSizes>> = Arc::new(Mutex::new(provisional));
        {
            let waiting = waiting.clone();
            let sizes_cell = sizes_cell.clone();
            std::thread::Builder::new()
                .name("jdwp-reader".to_owned())
                .spawn(move || read_packets(reader, &waiting, &event_sender, &sizes_cell))?;
        }
        let mut connection = Connection {
            writer: Mutex::new(stream),
            next_id: AtomicU32::new(1),
            waiting,
            events: Mutex::new(Some(event_receiver)),
            sizes: provisional,
        };
        let sizes = connection.id_sizes()?;
        connection.sizes = sizes;
        if let Ok(mut cell) = sizes_cell.lock() {
            *cell = sizes;
        }
        Ok(connection)
    }

    /// The VM's event sets, in order: taken once, by whoever handles them.
    pub fn events(&self) -> Option<std::sync::mpsc::Receiver<EventSet>> {
        self.events.lock().ok()?.take()
    }

    pub fn sizes(&self) -> IdSizes {
        self.sizes
    }

    /// Sends command `command` of `set` with the data `write` writes, and
    /// waits for its reply's data.
    pub fn command(
        &self,
        set: u8,
        command: u8,
        write: impl FnOnce(&mut Writer),
    ) -> Result<Vec<u8>, Error> {
        let mut writer = Writer::new(&self.sizes);
        write(&mut writer);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let length = u32::try_from(11 + writer.bytes.len()).map_err(|_| Error::Malformed)?;
        let mut packet = Vec::with_capacity(length as usize);
        packet.extend_from_slice(&length.to_be_bytes());
        packet.extend_from_slice(&id.to_be_bytes());
        packet.push(0);
        packet.push(set);
        packet.push(command);
        packet.extend_from_slice(&writer.bytes);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.waiting
            .lock()
            .map_err(|_| Error::Closed)?
            .insert(id, sender);
        let written = self
            .writer
            .lock()
            .map_err(|_| Error::Closed)
            .and_then(|mut stream| stream.write_all(&packet).map_err(Error::from));
        if let Err(error) = written {
            if let Ok(mut waiting) = self.waiting.lock() {
                waiting.remove(&id);
            }
            return Err(error);
        }
        match receiver.recv_timeout(REPLY_PATIENCE) {
            Ok(reply) => reply,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(mut waiting) = self.waiting.lock() {
                    waiting.remove(&id);
                }
                Err(Error::TimedOut)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(Error::Closed),
        }
    }

    /// A reader of a reply's data.
    pub fn reader<'data>(&self, data: &'data [u8]) -> Reader<'data> {
        Reader::new(data, self.sizes)
    }
}

/// Reads packets until the connection closes: replies go to whoever
/// waits for them, the VM's commands (events) to the events channel.
fn read_packets(
    mut stream: TcpStream,
    waiting: &Mutex<HashMap<u32, std::sync::mpsc::SyncSender<Reply>>>,
    events: &std::sync::mpsc::Sender<EventSet>,
    sizes: &Mutex<IdSizes>,
) {
    loop {
        let mut header = [0u8; 11];
        if stream.read_exact(&mut header).is_err() {
            break;
        }
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let id = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        let is_reply = header[8] & 0x80 != 0;
        let mut data = vec![0u8; length.saturating_sub(11)];
        if stream.read_exact(&mut data).is_err() {
            break;
        }
        if is_reply {
            let error = u16::from_be_bytes([header[9], header[10]]);
            let reply = if error == 0 {
                Ok(data)
            } else {
                Err(Error::Vm(error))
            };
            let sender = waiting
                .lock()
                .ok()
                .and_then(|mut waiting| waiting.remove(&id));
            if let Some(sender) = sender {
                let _ = sender.send(reply);
            }
            continue;
        }
        let (set, command) = (header[9], header[10]);
        let is_composite_event = set == 64 && command == 100;
        if !is_composite_event {
            continue;
        }
        let Ok(sizes) = sizes.lock().map(|sizes| *sizes) else {
            break;
        };
        if let Ok(set) = events::parse_composite(&data, sizes)
            && events.send(set).is_err()
        {
            return;
        }
    }
    // The VM is gone: whoever waits hears so, and the events end.
    if let Ok(mut waiting) = waiting.lock() {
        for (_, sender) in waiting.drain() {
            let _ = sender.send(Err(Error::Closed));
        }
    }
    let _ = events.send(EventSet {
        suspend_policy: SuspendPolicy::None,
        events: vec![Event::Disconnected],
    });
}
