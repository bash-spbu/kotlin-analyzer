//! Kotlin coroutines as the debugger sees them. A suspend function's
//! state is a continuation object (a `BaseContinuationImpl`); resumed, it
//! runs from the dispatcher, so the functions waiting for it aren't on the
//! thread's stack — they are its `completion` chain. Each continuation
//! says where it waits (`getStackTraceElement()`, from the compiler's
//! `@DebugMetadata`): the coroutine's async stack.

use std::collections::HashMap;

use jdwp::{Connection, MethodId, ObjectId, ReferenceTypeId, Tag, ThreadId, Value};

use crate::values::Inspector;

const BASE_CONTINUATION: &str = "Lkotlin/coroutines/jvm/internal/BaseContinuationImpl;";

/// `COROUTINE_SUSPENDED` and its siblings: what a suspend function
/// returns when it suspends.
const COROUTINE_SINGLETONS: &str = "Lkotlin/coroutines/intrinsics/CoroutineSingletons;";

/// The longest async stack followed.
const DEPTH_LIMIT: usize = 64;

/// A function waiting for a coroutine: where it is suspended.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Waiting {
    pub continuation: ObjectId,
    /// `demo.MainKt`.
    pub class_name: String,
    pub method: String,
    pub file_name: Option<String>,
    pub line: Option<i32>,
}

/// What's known of classes, kept for a session.
#[derive(Default)]
pub struct Coroutines {
    is_continuation: HashMap<ReferenceTypeId, bool>,
    /// `BaseContinuationImpl.getStackTraceElement()`.
    stack_trace_element: Option<(ReferenceTypeId, MethodId)>,
}

impl Coroutines {
    /// Whether `class` is a compiled suspend function's or lambda's
    /// continuation.
    pub fn is_continuation(
        &mut self,
        connection: &Connection,
        inspector: &mut Inspector,
        class: ReferenceTypeId,
    ) -> bool {
        if let Some(known) = self.is_continuation.get(&class) {
            return *known;
        }
        let mut current = Some(class);
        let mut found = false;
        while let Some(candidate) = current {
            if inspector.signature(connection, candidate).ok().as_deref() == Some(BASE_CONTINUATION)
            {
                found = true;
                if self.stack_trace_element.is_none() {
                    self.stack_trace_element = connection
                        .methods(candidate)
                        .ok()
                        .and_then(|methods| {
                            methods
                                .into_iter()
                                .find(|method| method.name == "getStackTraceElement")
                        })
                        .map(|method| (candidate, method.id));
                }
                break;
            }
            current = connection.superclass(candidate).ok().flatten();
        }
        self.is_continuation.insert(class, found);
        found
    }

    /// The functions waiting for `continuation`'s coroutine, nearest
    /// first: its `completion` chain while it is made of suspend
    /// functions. Calls `getStackTraceElement()` in `thread`, which an
    /// event must have stopped.
    pub fn waiting_for(
        &mut self,
        connection: &Connection,
        inspector: &mut Inspector,
        thread: ThreadId,
        continuation: ObjectId,
    ) -> Vec<Waiting> {
        let mut waiting = Vec::new();
        let mut current = continuation;
        for _ in 0..DEPTH_LIMIT {
            let completion = inspector
                .field_value(connection, current, "completion")
                .ok()
                .flatten();
            let Some(Value::Object { id, .. }) = completion else {
                break;
            };
            let is_continuation = connection
                .reference_type(id)
                .is_ok_and(|class| self.is_continuation(connection, inspector, class));
            if !is_continuation {
                break;
            }
            match self.where_waiting(connection, inspector, thread, id) {
                Some(found) => waiting.push(found),
                None => break,
            }
            current = id;
        }
        waiting
    }

    fn where_waiting(
        &mut self,
        connection: &Connection,
        inspector: &mut Inspector,
        thread: ThreadId,
        continuation: ObjectId,
    ) -> Option<Waiting> {
        let (class, method) = self.stack_trace_element?;
        let (element, exception) = connection
            .invoke_method(continuation, thread, class, method, &[])
            .ok()?;
        if exception.is_some() {
            return None;
        }
        let Value::Object { id: element, .. } = element else {
            return None;
        };
        let mut text = |name: &str| match inspector.field_value(connection, element, name) {
            Ok(Some(Value::Object {
                tag: Tag::String,
                id,
            })) => connection.string_value(id).ok(),
            _ => None,
        };
        let class_name = text("declaringClass")?;
        let method = text("methodName").unwrap_or_default();
        let file_name = text("fileName");
        let line = match inspector.field_value(connection, element, "lineNumber") {
            Ok(Some(Value::Int(line))) if line > 0 => Some(line),
            _ => None,
        };
        Some(Waiting {
            continuation,
            class_name,
            method,
            file_name,
            line,
        })
    }
}

/// Whether a value a method returned says it suspended.
pub fn is_suspension_marker(connection: &Connection, value: &Value) -> bool {
    let Value::Object { id, .. } = value else {
        return false;
    };
    connection
        .reference_type(*id)
        .and_then(|class| connection.signature(class))
        .is_ok_and(|signature| signature == COROUTINE_SINGLETONS)
}
