//! The VM's events (`Event.Composite`) and what a debugger asks to hear
//! of (`EventRequest.Set`'s kinds and modifiers).

use crate::{Error, IdSizes, Location, ReferenceTypeId, ThreadId, TypeTag, Value};

/// The events of one composite: they happened together, and the VM
/// suspended (by `suspend_policy`) once for all of them — so a debugger
/// resumes once for the set, not once per event.
#[derive(Clone, PartialEq, Debug)]
pub struct EventSet {
    pub suspend_policy: SuspendPolicy,
    pub events: Vec<Event>,
}

/// What the VM tells.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    VmStart {
        thread: ThreadId,
    },
    Breakpoint {
        request: i32,
        thread: ThreadId,
        location: Location,
    },
    Step {
        request: i32,
        thread: ThreadId,
        location: Location,
    },
    Exception {
        request: i32,
        thread: ThreadId,
        location: Location,
        exception: Value,
        /// Where it is caught; `None` when nothing catches it.
        catch_location: Option<Location>,
    },
    ClassPrepare {
        request: i32,
        thread: ThreadId,
        type_tag: TypeTag,
        class: ReferenceTypeId,
        signature: String,
    },
    /// A method of the request's class returned `value` (the one it
    /// returned from is `location.method`).
    MethodExit {
        request: i32,
        thread: ThreadId,
        location: Location,
        value: Value,
    },
    ThreadStart {
        thread: ThreadId,
    },
    ThreadDeath {
        thread: ThreadId,
    },
    VmDeath,
    /// The connection closed: no more events.
    Disconnected,
}

impl Event {
    /// The thread the event happened in, if any.
    pub fn thread(&self) -> Option<ThreadId> {
        match self {
            Event::VmStart { thread }
            | Event::Breakpoint { thread, .. }
            | Event::Step { thread, .. }
            | Event::Exception { thread, .. }
            | Event::ClassPrepare { thread, .. }
            | Event::MethodExit { thread, .. }
            | Event::ThreadStart { thread }
            | Event::ThreadDeath { thread } => Some(*thread),
            Event::VmDeath | Event::Disconnected => None,
        }
    }
}

/// The kinds of events a debugger asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum EventKind {
    SingleStep = 1,
    Breakpoint = 2,
    Exception = 4,
    ThreadStart = 6,
    ThreadDeath = 7,
    ClassPrepare = 8,
    /// `MethodExitWithReturnValue`.
    MethodExit = 42,
}

/// Which threads an event suspends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum SuspendPolicy {
    None = 0,
    EventThread = 1,
    All = 2,
}

impl SuspendPolicy {
    fn of(byte: u8) -> SuspendPolicy {
        match byte {
            1 => SuspendPolicy::EventThread,
            2 => SuspendPolicy::All,
            _ => SuspendPolicy::None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum StepDepth {
    Into = 0,
    Over = 1,
    Out = 2,
}

/// What narrows a request.
#[derive(Clone, PartialEq, Debug)]
pub enum Modifier {
    /// Only the first `n` events.
    Count(i32),
    ThreadOnly(ThreadId),
    /// Classes whose name matches (`com.example.*`).
    ClassMatch(String),
    ClassExclude(String),
    LocationOnly(Location),
    ExceptionOnly {
        /// `None` for every exception.
        exception: Option<ReferenceTypeId>,
        caught: bool,
        uncaught: bool,
    },
    /// A step of a line in `thread`.
    Step {
        thread: ThreadId,
        depth: StepDepth,
    },
    /// Classes whose source file's name matches (`Order.kt`).
    SourceNameMatch(String),
    /// Only where `this` is this object.
    InstanceOnly(crate::ObjectId),
}

impl Modifier {
    pub(crate) fn write(&self, writer: &mut crate::Writer) {
        match self {
            Modifier::Count(count) => {
                writer.byte(1).int(*count);
            }
            Modifier::ThreadOnly(thread) => {
                writer.byte(3).object(*thread);
            }
            Modifier::ClassMatch(pattern) => {
                writer.byte(5).string(pattern);
            }
            Modifier::ClassExclude(pattern) => {
                writer.byte(6).string(pattern);
            }
            Modifier::LocationOnly(location) => {
                writer.byte(7).location(location);
            }
            Modifier::ExceptionOnly {
                exception,
                caught,
                uncaught,
            } => {
                writer
                    .byte(8)
                    .reference_type(exception.unwrap_or(ReferenceTypeId(0)))
                    .boolean(*caught)
                    .boolean(*uncaught);
            }
            Modifier::Step { thread, depth } => {
                // Size LINE: a step ends on another line.
                writer.byte(10).object(*thread).int(1).int(*depth as i32);
            }
            Modifier::SourceNameMatch(pattern) => {
                writer.byte(12).string(pattern);
            }
            Modifier::InstanceOnly(object) => {
                writer.byte(11).object(*object);
            }
        }
    }
}

/// The events of one `Event.Composite` command.
pub(crate) fn parse_composite(data: &[u8], sizes: IdSizes) -> Result<EventSet, Error> {
    let mut reader = crate::Reader::new(data, sizes);
    let suspend_policy = SuspendPolicy::of(reader.byte()?);
    let count = reader.count()?;
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = reader.byte()?;
        let event = match kind {
            90 => {
                let _request = reader.int()?;
                Event::VmStart {
                    thread: reader.object()?,
                }
            }
            1 | 2 => {
                let request = reader.int()?;
                let thread = reader.object()?;
                let location = reader.location()?;
                if kind == 2 {
                    Event::Breakpoint {
                        request,
                        thread,
                        location,
                    }
                } else {
                    Event::Step {
                        request,
                        thread,
                        location,
                    }
                }
            }
            4 => {
                let request = reader.int()?;
                let thread = reader.object()?;
                let location = reader.location()?;
                let exception = reader.value()?;
                let catch_location = reader.location()?;
                Event::Exception {
                    request,
                    thread,
                    location,
                    exception,
                    catch_location: (catch_location.class.0 != 0).then_some(catch_location),
                }
            }
            6 | 7 => {
                let _request = reader.int()?;
                let thread = reader.object()?;
                if kind == 6 {
                    Event::ThreadStart { thread }
                } else {
                    Event::ThreadDeath { thread }
                }
            }
            42 => {
                let request = reader.int()?;
                let thread = reader.object()?;
                let location = reader.location()?;
                let value = reader.value()?;
                Event::MethodExit {
                    request,
                    thread,
                    location,
                    value,
                }
            }
            8 => {
                let request = reader.int()?;
                let thread = reader.object()?;
                let type_tag = reader.type_tag()?;
                let class = reader.reference_type()?;
                let signature = reader.string()?;
                let _status = reader.int()?;
                Event::ClassPrepare {
                    request,
                    thread,
                    type_tag,
                    class,
                    signature,
                }
            }
            99 => {
                let _request = reader.int()?;
                Event::VmDeath
            }
            // A kind this client never asks for: the rest can't be read.
            _ => break,
        };
        events.push(event);
    }
    Ok(EventSet {
        suspend_policy,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MethodId, ObjectId, Tag, Writer};

    const SIZES: IdSizes = IdSizes {
        field: 8,
        method: 8,
        object: 8,
        reference_type: 8,
        frame: 8,
    };

    #[test]
    fn a_method_exit_carries_the_returned_value() {
        let location = Location {
            type_tag: TypeTag::Class,
            class: ReferenceTypeId(3),
            method: MethodId(4),
            index: 17,
        };
        let mut writer = Writer::new(&SIZES);
        writer
            .byte(SuspendPolicy::All as u8)
            .int(1)
            .byte(EventKind::MethodExit as u8)
            .int(9)
            .object(ObjectId(5))
            .location(&location)
            .byte(b'L')
            .object(ObjectId(6));
        let set = parse_composite(&writer.bytes, SIZES).unwrap();
        assert_eq!(set.suspend_policy, SuspendPolicy::All);
        assert_eq!(
            set.events,
            [Event::MethodExit {
                request: 9,
                thread: ObjectId(5),
                location,
                value: Value::Object {
                    tag: Tag::Object,
                    id: ObjectId(6)
                },
            }]
        );
    }

    #[test]
    fn instance_only_is_modifier_eleven_with_the_object() {
        let mut writer = Writer::new(&SIZES);
        Modifier::InstanceOnly(ObjectId(0x10)).write(&mut writer);
        assert_eq!(writer.bytes, [11, 0, 0, 0, 0, 0, 0, 0, 0x10]);
    }
}
