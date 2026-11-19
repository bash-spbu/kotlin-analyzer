//! Method bodies from bytecode, as Java syntax trees: for reading a
//! library without sources, not for recompiling it.
//!
//! The bytecode of a body runs on an operand stack; replaying it with
//! expressions in place of values gives the statements it was compiled
//! from — `aload_0; getfield name; areturn` is `return this.name;`. This
//! covers bodies that run straight through: no branches, loops, `switch`
//! or `try`. A body with anything else is [`Unsupported`], and shown as
//! compiled code — a body is decompiled whole or not at all.

use crate::class_file::{Bodies, ClassFile, Code, Constant, access};
use crate::signature::{NestedNames, Parser};
use crate::{ClassName, LibraryType};

/// A class's methods, each with its body when it could be decompiled.
#[derive(Clone, PartialEq, Debug)]
pub struct DecompiledClass {
    pub methods: Vec<DecompiledMethod>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DecompiledMethod {
    /// `<init>` for a constructor, `<clinit>` for the static initializer.
    pub name: String,
    pub descriptor: String,
    /// Each parameter's name, as the descriptor lists them (an inner
    /// class's constructor's outer instance first): from the local
    /// variables, else `MethodParameters`, else `p0`, `p1`…
    pub parameter_names: Vec<String>,
    pub body: Result<Body, Unsupported>,
}

/// Why a body isn't decompiled.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Unsupported {
    /// Abstract or native: there is no body.
    NoCode,
    /// It branches: `if`, a loop, `switch`, `&&`, `?:`.
    Branches,
    /// It has `try`/`catch`/`finally` or `synchronized`.
    ExceptionHandlers,
    /// A lambda, method reference or other `invokedynamic` but string
    /// concatenation.
    Lambda,
    /// The operand stack holds values across statements (`dup` of a call,
    /// an array initializer, `i++` inside an expression).
    StackShape,
    /// An instruction not decompiled (its opcode).
    Instruction(u8),
    /// Bytecode that doesn't read: truncated, an unknown constant.
    Malformed,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Body {
    pub statements: Vec<Statement>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Statement {
    Expression(Expression),
    /// `Type name = value;` (`var` without a recorded type).
    Declare {
        type_ref: Option<LibraryType>,
        name: String,
        value: Expression,
    },
    Assign {
        target: Expression,
        value: Expression,
    },
    /// `name += by;`
    Increment {
        name: String,
        by: i32,
    },
    Return(Option<Expression>),
    Throw(Expression),
    /// `super(arguments);` in a constructor.
    SuperConstructor(Vec<Expression>),
    /// `this(arguments);` in a constructor.
    ThisConstructor(Vec<Expression>),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Expression {
    This,
    Local(String),
    Literal(Literal),
    Field {
        owner: Owner,
        name: String,
    },
    Call {
        receiver: Receiver,
        name: String,
        arguments: Vec<Expression>,
    },
    New {
        class: ClassName,
        arguments: Vec<Expression>,
    },
    NewArray {
        element: LibraryType,
        length: Box<Expression>,
    },
    ArrayElement {
        array: Box<Expression>,
        index: Box<Expression>,
    },
    ArrayLength(Box<Expression>),
    Binary {
        operator: BinaryOperator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Negate(Box<Expression>),
    Cast {
        to: LibraryType,
        operand: Box<Expression>,
    },
    InstanceOf {
        operand: Box<Expression>,
        type_ref: LibraryType,
    },
    /// `"total: " + total`: the pieces, in order.
    Concatenation(Vec<Expression>),
    /// `String.class`.
    ClassLiteral(LibraryType),
}

/// Whose field it is.
#[derive(Clone, PartialEq, Debug)]
pub enum Owner {
    Instance(Box<Expression>),
    Static(ClassName),
}

/// What a method is called on.
#[derive(Clone, PartialEq, Debug)]
pub enum Receiver {
    Instance(Box<Expression>),
    Static(ClassName),
    /// `super.name(…)`.
    Super,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Literal {
    Null,
    Boolean(bool),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// A UTF-16 unit.
    Char(u16),
    String(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    ShiftLeft,
    ShiftRight,
    UnsignedShiftRight,
    And,
    Or,
    Xor,
}

/// Decompiles the methods of a class file; `None` if it doesn't parse.
pub fn decompile_class(bytes: &[u8]) -> Option<DecompiledClass> {
    let class = ClassFile::parse_keeping(bytes, Bodies::Kept)?;
    let names = NestedNames::new(&class.inner_classes);
    let methods = class
        .methods
        .iter()
        .map(|method| {
            let is_static = method.access & access::STATIC != 0;
            let parameters = parameter_descriptors(&method.descriptor).unwrap_or_default();
            let code = method.code.as_ref();
            let parameter_names =
                parameter_names(code, &method.parameter_names, &parameters, is_static);
            let body = match code {
                None => Err(Unsupported::NoCode),
                Some(code) => Machine::new(&class, &names, method, code, &parameter_names).run(),
            };
            DecompiledMethod {
                name: method.name.clone(),
                descriptor: method.descriptor.clone(),
                parameter_names,
                body,
            }
        })
        .collect();
    Some(DecompiledClass { methods })
}

/// The parameters' descriptors: `(ILjava/lang/String;[J)V` →
/// `I`, `Ljava/lang/String;`, `[J`.
pub fn parameter_descriptors(descriptor: &str) -> Option<Vec<&str>> {
    let inside = descriptor.strip_prefix('(')?.split_once(')')?.0;
    let mut parameters = Vec::new();
    let mut rest = inside;
    while !rest.is_empty() {
        let length = descriptor_length(rest)?;
        parameters.push(&rest[..length]);
        rest = &rest[length..];
    }
    Some(parameters)
}

/// The result's descriptor: `V`, `I`, `Ljava/lang/String;`.
fn result_descriptor(descriptor: &str) -> Option<&str> {
    descriptor.split_once(')').map(|(_, result)| result)
}

/// The length of the first type descriptor of `text`.
fn descriptor_length(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut position = 0;
    while bytes.get(position) == Some(&b'[') {
        position += 1;
    }
    match bytes.get(position)? {
        b'L' => Some(position + text[position..].find(';')? + 1),
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => Some(position + 1),
        _ => None,
    }
}

/// The parameters' names: from the local variables at the start, else
/// `MethodParameters`, else `p0`, `p1`….
fn parameter_names(
    code: Option<&Code>,
    from_attribute: &[String],
    parameters: &[&str],
    is_static: bool,
) -> Vec<String> {
    let mut slot: u16 = if is_static { 0 } else { 1 };
    let mut names = Vec::with_capacity(parameters.len());
    for (index, parameter) in parameters.iter().enumerate() {
        let recorded = code.and_then(|code| {
            code.local_variables
                .iter()
                .find(|variable| variable.slot == slot && variable.start == 0)
                .map(|variable| variable.name.clone())
        });
        let named = from_attribute
            .get(index)
            .filter(|name| !name.is_empty())
            .cloned();
        names.push(recorded.or(named).unwrap_or_else(|| format!("p{index}")));
        slot += if is_wide_descriptor(parameter) { 2 } else { 1 };
    }
    names
}

fn is_wide_descriptor(descriptor: &str) -> bool {
    descriptor == "J" || descriptor == "D"
}

/// What a value on the operand stack is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Int,
    Long,
    Float,
    Double,
    Reference,
    /// A `new` object before its constructor runs: the number tells which.
    Uninitialized(usize),
}

impl Kind {
    fn of_descriptor(descriptor: &str) -> Kind {
        match descriptor.as_bytes().first() {
            Some(b'J') => Kind::Long,
            Some(b'F') => Kind::Float,
            Some(b'D') => Kind::Double,
            Some(b'L' | b'[') => Kind::Reference,
            _ => Kind::Int,
        }
    }

    fn is_wide(self) -> bool {
        matches!(self, Kind::Long | Kind::Double)
    }
}

#[derive(Clone, Debug)]
struct Value {
    expression: Expression,
    kind: Kind,
}

/// One body's replay.
struct Machine<'class> {
    class: &'class ClassFile,
    names: &'class NestedNames,
    code: &'class Code,
    is_static: bool,
    is_constructor: bool,
    result: String,
    /// The parameters' slots and names.
    parameters: Vec<(u16, String)>,
    stack: Vec<Value>,
    statements: Vec<Statement>,
    /// The local variables declared so far: slot and where their scope
    /// starts.
    declared: Vec<(u16, u32)>,
    uninitialized: Vec<ClassName>,
}

/// An instruction's step: on to the next, or the body is done.
enum Step {
    Next,
    Done,
}

impl<'class> Machine<'class> {
    fn new(
        class: &'class ClassFile,
        names: &'class NestedNames,
        method: &'class crate::class_file::Member,
        code: &'class Code,
        parameter_names: &[String],
    ) -> Machine<'class> {
        let is_static = method.access & access::STATIC != 0;
        let parameters_descriptors = parameter_descriptors(&method.descriptor).unwrap_or_default();
        let mut slot: u16 = if is_static { 0 } else { 1 };
        let mut parameters = Vec::new();
        for (descriptor, name) in parameters_descriptors.iter().zip(parameter_names) {
            parameters.push((slot, name.clone()));
            slot += if is_wide_descriptor(descriptor) { 2 } else { 1 };
        }
        Machine {
            class,
            names,
            code,
            is_static,
            is_constructor: method.name == "<init>",
            result: result_descriptor(&method.descriptor)
                .unwrap_or("V")
                .to_owned(),
            parameters,
            stack: Vec::new(),
            statements: Vec::new(),
            declared: Vec::new(),
            uninitialized: Vec::new(),
        }
    }

    fn run(mut self) -> Result<Body, Unsupported> {
        if self.code.exception_handlers > 0 {
            return Err(Unsupported::ExceptionHandlers);
        }
        let bytes = &self.code.bytes;
        let mut pc = 0;
        while pc < bytes.len() {
            let (length, step) = self.step(pc)?;
            pc += length;
            if let Step::Done = step {
                // Nothing may follow a return or throw in a body that
                // doesn't branch: if anything does, something jumps there.
                if pc != bytes.len() {
                    return Err(Unsupported::Branches);
                }
                return Ok(Body {
                    statements: self.statements,
                });
            }
        }
        Err(Unsupported::Malformed)
    }

    fn byte(&self, at: usize) -> Result<u8, Unsupported> {
        self.code
            .bytes
            .get(at)
            .copied()
            .ok_or(Unsupported::Malformed)
    }

    fn u16_at(&self, at: usize) -> Result<u16, Unsupported> {
        Ok(u16::from_be_bytes([self.byte(at)?, self.byte(at + 1)?]))
    }

    fn pop(&mut self) -> Result<Value, Unsupported> {
        self.stack.pop().ok_or(Unsupported::Malformed)
    }

    fn push(&mut self, expression: Expression, kind: Kind) {
        self.stack.push(Value { expression, kind });
    }

    /// Adds a statement; the stack must hold nothing still to be used, or
    /// the statement would run out of its order.
    fn emit(&mut self, statement: Statement) -> Result<(), Unsupported> {
        if !self.stack.is_empty() {
            return Err(Unsupported::StackShape);
        }
        self.statements.push(statement);
        Ok(())
    }

    fn class_name(&self, binary: &str) -> ClassName {
        self.names.class_name(binary)
    }

    /// The type a descriptor or a class constant's name (`[I`,
    /// `java/lang/String`) names.
    fn type_of(&self, text: &str) -> LibraryType {
        let descriptor = if text.starts_with('[') || text.len() == 1 {
            text.to_owned()
        } else {
            format!("L{text};")
        };
        Parser::new(&descriptor, self.names)
            .field_signature()
            .unwrap_or(LibraryType::Unreadable)
    }

    /// One instruction at `pc`: its length and whether the body ends.
    fn step(&mut self, pc: usize) -> Result<(usize, Step), Unsupported> {
        let opcode = self.byte(pc)?;
        let length = match opcode {
            0x00 => 1,
            0x01 => {
                self.push(Expression::Literal(Literal::Null), Kind::Reference);
                1
            }
            0x02..=0x08 => {
                let value = i32::from(opcode) - 3;
                self.push(Expression::Literal(Literal::Int(value)), Kind::Int);
                1
            }
            0x09 | 0x0a => {
                let value = i64::from(opcode - 0x09);
                self.push(Expression::Literal(Literal::Long(value)), Kind::Long);
                1
            }
            0x0b..=0x0d => {
                let value = f32::from(opcode - 0x0b);
                self.push(Expression::Literal(Literal::Float(value)), Kind::Float);
                1
            }
            0x0e | 0x0f => {
                let value = f64::from(opcode - 0x0e);
                self.push(Expression::Literal(Literal::Double(value)), Kind::Double);
                1
            }
            0x10 => {
                let value = i32::from(self.byte(pc + 1)? as i8);
                self.push(Expression::Literal(Literal::Int(value)), Kind::Int);
                2
            }
            0x11 => {
                let value = i32::from(self.u16_at(pc + 1)? as i16);
                self.push(Expression::Literal(Literal::Int(value)), Kind::Int);
                3
            }
            0x12 => {
                self.load_constant(u16::from(self.byte(pc + 1)?))?;
                2
            }
            0x13 | 0x14 => {
                self.load_constant(self.u16_at(pc + 1)?)?;
                3
            }
            0x15..=0x19 => {
                let slot = u16::from(self.byte(pc + 1)?);
                self.load(slot, opcode - 0x15, pc)?;
                2
            }
            0x1a..=0x2d => {
                let offset = opcode - 0x1a;
                self.load(u16::from(offset % 4), offset / 4, pc)?;
                1
            }
            0x2e..=0x35 => {
                let index = self.pop()?;
                let array = self.pop()?;
                let kind = match opcode {
                    0x2f => Kind::Long,
                    0x30 => Kind::Float,
                    0x31 => Kind::Double,
                    0x32 => Kind::Reference,
                    _ => Kind::Int,
                };
                self.push(
                    Expression::ArrayElement {
                        array: Box::new(array.expression),
                        index: Box::new(index.expression),
                    },
                    kind,
                );
                1
            }
            0x36..=0x3a => {
                let slot = u16::from(self.byte(pc + 1)?);
                self.store(slot, pc + 2)?;
                2
            }
            0x3b..=0x4e => {
                let slot = u16::from((opcode - 0x3b) % 4);
                self.store(slot, pc + 1)?;
                1
            }
            0x4f..=0x56 => {
                let value = self.pop()?;
                let index = self.pop()?;
                let array = self.pop()?;
                let value = match opcode {
                    0x55 => coerced(value.expression, "C"),
                    _ => value.expression,
                };
                let target = Expression::ArrayElement {
                    array: Box::new(array.expression),
                    index: Box::new(index.expression),
                };
                self.emit(Statement::Assign { target, value })?;
                1
            }
            0x57 => {
                let value = self.pop()?;
                self.discard(value)?;
                1
            }
            0x58 => {
                let value = self.pop()?;
                if !value.kind.is_wide() {
                    return Err(Unsupported::StackShape);
                }
                self.discard(value)?;
                1
            }
            0x59 => {
                let top = self.stack.last().ok_or(Unsupported::Malformed)?.clone();
                let can_repeat =
                    matches!(top.kind, Kind::Uninitialized(_)) || is_repeatable(&top.expression);
                if !can_repeat {
                    return Err(Unsupported::StackShape);
                }
                self.stack.push(top);
                1
            }
            0x5a..=0x5f => return Err(Unsupported::StackShape),
            0x60..=0x83 => {
                self.arithmetic(opcode)?;
                1
            }
            0x84 => {
                let slot = u16::from(self.byte(pc + 1)?);
                let by = i32::from(self.byte(pc + 2)? as i8);
                let name = self.local_name(slot, pc as u32);
                self.emit(Statement::Increment { name, by })?;
                3
            }
            0x85..=0x93 => {
                self.convert(opcode)?;
                1
            }
            0x94..=0x98 => return Err(Unsupported::Branches),
            0x99..=0xab | 0xc6..=0xc9 => return Err(Unsupported::Branches),
            0xac..=0xb0 => {
                let value = self.pop()?;
                let value = coerced(value.expression, &self.result.clone());
                self.emit(Statement::Return(Some(value)))?;
                return Ok((1, Step::Done));
            }
            0xb1 => {
                self.emit(Statement::Return(None))?;
                return Ok((1, Step::Done));
            }
            0xb2..=0xb5 => {
                self.field(opcode, self.u16_at(pc + 1)?)?;
                3
            }
            0xb6..=0xb9 => {
                self.invoke(opcode, self.u16_at(pc + 1)?)?;
                if opcode == 0xb9 { 5 } else { 3 }
            }
            0xba => {
                self.invoke_dynamic(self.u16_at(pc + 1)?)?;
                5
            }
            0xbb => {
                let class = self
                    .class
                    .class_name_at(self.u16_at(pc + 1)?)
                    .ok_or(Unsupported::Malformed)?;
                let class = self.class_name(&class);
                let id = self.uninitialized.len();
                self.uninitialized.push(class.clone());
                self.push(
                    Expression::New {
                        class,
                        arguments: Vec::new(),
                    },
                    Kind::Uninitialized(id),
                );
                3
            }
            0xbc => {
                let element = match self.byte(pc + 1)? {
                    4 => "Z",
                    5 => "C",
                    6 => "F",
                    7 => "D",
                    8 => "B",
                    9 => "S",
                    10 => "I",
                    11 => "J",
                    _ => return Err(Unsupported::Malformed),
                };
                let length = self.pop()?;
                let element = self.type_of(element);
                self.push(
                    Expression::NewArray {
                        element,
                        length: Box::new(length.expression),
                    },
                    Kind::Reference,
                );
                2
            }
            0xbd => {
                let class = self
                    .class
                    .class_name_at(self.u16_at(pc + 1)?)
                    .ok_or(Unsupported::Malformed)?;
                let length = self.pop()?;
                let element = self.type_of(&class);
                self.push(
                    Expression::NewArray {
                        element,
                        length: Box::new(length.expression),
                    },
                    Kind::Reference,
                );
                3
            }
            0xbe => {
                let array = self.pop()?;
                self.push(
                    Expression::ArrayLength(Box::new(array.expression)),
                    Kind::Int,
                );
                1
            }
            0xbf => {
                let thrown = self.pop()?;
                self.emit(Statement::Throw(thrown.expression))?;
                return Ok((1, Step::Done));
            }
            0xc0 | 0xc1 => {
                let class = self
                    .class
                    .class_name_at(self.u16_at(pc + 1)?)
                    .ok_or(Unsupported::Malformed)?;
                let type_ref = self.type_of(&class);
                let operand = self.pop()?;
                if opcode == 0xc0 {
                    self.push(
                        Expression::Cast {
                            to: type_ref,
                            operand: Box::new(operand.expression),
                        },
                        Kind::Reference,
                    );
                } else {
                    self.push(
                        Expression::InstanceOf {
                            operand: Box::new(operand.expression),
                            type_ref,
                        },
                        Kind::Int,
                    );
                }
                3
            }
            0xc4 => self.wide(pc)?,
            other => return Err(Unsupported::Instruction(other)),
        };
        Ok((length, Step::Next))
    }

    /// `wide` before a local variable instruction: its index in two bytes.
    fn wide(&mut self, pc: usize) -> Result<usize, Unsupported> {
        let opcode = self.byte(pc + 1)?;
        let slot = self.u16_at(pc + 2)?;
        match opcode {
            0x15..=0x19 => {
                self.load(slot, opcode - 0x15, pc)?;
                Ok(4)
            }
            0x36..=0x3a => {
                self.store(slot, pc + 4)?;
                Ok(4)
            }
            0x84 => {
                let by = i32::from(self.u16_at(pc + 4)? as i16);
                let name = self.local_name(slot, pc as u32);
                self.emit(Statement::Increment { name, by })?;
                Ok(6)
            }
            other => Err(Unsupported::Instruction(other)),
        }
    }

    fn load_constant(&mut self, index: u16) -> Result<(), Unsupported> {
        let (expression, kind) = match self.class.constant(index).ok_or(Unsupported::Malformed)? {
            Constant::Integer(value) => (Literal::Int(*value).into(), Kind::Int),
            Constant::Float(value) => (Literal::Float(*value).into(), Kind::Float),
            Constant::Long(value) => (Literal::Long(*value).into(), Kind::Long),
            Constant::Double(value) => (Literal::Double(*value).into(), Kind::Double),
            Constant::String { utf8_index } => {
                let text = self.class.utf8(*utf8_index).ok_or(Unsupported::Malformed)?;
                (Literal::String(text.to_owned()).into(), Kind::Reference)
            }
            Constant::Class { name_index } => {
                let name = self.class.utf8(*name_index).ok_or(Unsupported::Malformed)?;
                (
                    Expression::ClassLiteral(self.type_of(name)),
                    Kind::Reference,
                )
            }
            _ => return Err(Unsupported::Instruction(0x12)),
        };
        self.push(expression, kind);
        Ok(())
    }

    /// `xload slot`: `kind_index` 0 int, 1 long, 2 float, 3 double, 4
    /// reference.
    fn load(&mut self, slot: u16, kind_index: u8, pc: usize) -> Result<(), Unsupported> {
        let kind = [
            Kind::Int,
            Kind::Long,
            Kind::Float,
            Kind::Double,
            Kind::Reference,
        ][usize::from(kind_index.min(4))];
        let is_this = slot == 0 && !self.is_static;
        let expression = if is_this {
            Expression::This
        } else {
            Expression::Local(self.local_name(slot, pc as u32))
        };
        self.push(expression, kind);
        Ok(())
    }

    /// The name of the variable in `slot` at `pc`.
    fn local_name(&self, slot: u16, pc: u32) -> String {
        if let Some(variable) = self
            .code
            .local_variables
            .iter()
            .find(|variable| variable.slot == slot && variable.covers(pc))
        {
            return variable.name.clone();
        }
        match self
            .parameters
            .iter()
            .find(|(parameter, _)| *parameter == slot)
        {
            Some((_, name)) => name.clone(),
            None => format!("var{slot}"),
        }
    }

    /// `xstore slot`: to a parameter, an assignment; to a local variable,
    /// its declaration the first time its scope sees it.
    fn store(&mut self, slot: u16, next_pc: usize) -> Result<(), Unsupported> {
        let value = self.pop()?;
        let next_pc = next_pc as u32;
        let is_parameter = self
            .parameters
            .iter()
            .any(|(parameter, _)| *parameter == slot);
        let starting_here = self
            .code
            .local_variables
            .iter()
            .find(|variable| variable.slot == slot && variable.start == next_pc)
            .or_else(|| {
                self.code
                    .local_variables
                    .iter()
                    .find(|variable| variable.slot == slot && variable.covers(next_pc))
            })
            .cloned();
        let statement = match starting_here {
            Some(variable) => {
                let descriptor = variable.descriptor.clone();
                let value = coerced(value.expression, &descriptor);
                let scope = (slot, variable.start);
                if self.declared.contains(&scope) {
                    Statement::Assign {
                        target: Expression::Local(variable.name),
                        value,
                    }
                } else {
                    self.declared.push(scope);
                    let text = variable.signature.unwrap_or(descriptor);
                    let type_ref = Parser::new(&text, self.names).field_signature();
                    Statement::Declare {
                        type_ref,
                        name: variable.name,
                        value,
                    }
                }
            }
            None if is_parameter => Statement::Assign {
                target: Expression::Local(self.local_name(slot, next_pc)),
                value: value.expression,
            },
            None => {
                let scope = (slot, 0);
                let name = format!("var{slot}");
                if self.declared.contains(&scope) {
                    Statement::Assign {
                        target: Expression::Local(name),
                        value: value.expression,
                    }
                } else {
                    self.declared.push(scope);
                    Statement::Declare {
                        type_ref: None,
                        name,
                        value: value.expression,
                    }
                }
            }
        };
        self.emit(statement)
    }

    /// A value nothing uses: a call or `new` stays as a statement, anything
    /// else is gone.
    fn discard(&mut self, value: Value) -> Result<(), Unsupported> {
        match value.expression {
            expression @ (Expression::Call { .. } | Expression::New { .. }) => {
                self.emit(Statement::Expression(expression))
            }
            expression if is_repeatable(&expression) => Ok(()),
            _ => Err(Unsupported::StackShape),
        }
    }

    fn arithmetic(&mut self, opcode: u8) -> Result<(), Unsupported> {
        let kind = |offset: u8| match offset % 2 {
            0 => Kind::Int,
            _ => Kind::Long,
        };
        if (0x74..=0x77).contains(&opcode) {
            let operand = self.pop()?;
            self.push(
                Expression::Negate(Box::new(operand.expression)),
                operand.kind,
            );
            return Ok(());
        }
        let operator = match opcode {
            0x60..=0x63 => BinaryOperator::Add,
            0x64..=0x67 => BinaryOperator::Subtract,
            0x68..=0x6b => BinaryOperator::Multiply,
            0x6c..=0x6f => BinaryOperator::Divide,
            0x70..=0x73 => BinaryOperator::Remainder,
            0x78 | 0x79 => BinaryOperator::ShiftLeft,
            0x7a | 0x7b => BinaryOperator::ShiftRight,
            0x7c | 0x7d => BinaryOperator::UnsignedShiftRight,
            0x7e | 0x7f => BinaryOperator::And,
            0x80 | 0x81 => BinaryOperator::Or,
            _ => BinaryOperator::Xor,
        };
        let right = self.pop()?;
        let left = self.pop()?;
        let result_kind = match opcode {
            0x60..=0x73 => {
                [Kind::Int, Kind::Long, Kind::Float, Kind::Double][usize::from((opcode - 0x60) % 4)]
            }
            _ => kind(opcode - 0x78),
        };
        self.push(
            Expression::Binary {
                operator,
                left: Box::new(left.expression),
                right: Box::new(right.expression),
            },
            result_kind,
        );
        Ok(())
    }

    /// `i2l`…`i2s`: Java widens implicitly, a narrowing is a cast.
    fn convert(&mut self, opcode: u8) -> Result<(), Unsupported> {
        let operand = self.pop()?;
        let (target, kind, implicit) = match opcode {
            0x85 => ("J", Kind::Long, true),
            0x86 => ("F", Kind::Float, true),
            0x87 => ("D", Kind::Double, true),
            0x88 => ("I", Kind::Int, false),
            0x89 => ("F", Kind::Float, true),
            0x8a => ("D", Kind::Double, true),
            0x8b => ("I", Kind::Int, false),
            0x8c => ("J", Kind::Long, false),
            0x8d => ("D", Kind::Double, true),
            0x8e => ("I", Kind::Int, false),
            0x8f => ("J", Kind::Long, false),
            0x90 => ("F", Kind::Float, false),
            0x91 => ("B", Kind::Int, false),
            0x92 => ("C", Kind::Int, false),
            _ => ("S", Kind::Int, false),
        };
        let expression = if implicit {
            operand.expression
        } else {
            Expression::Cast {
                to: self.type_of(target),
                operand: Box::new(operand.expression),
            }
        };
        self.push(expression, kind);
        Ok(())
    }

    fn field(&mut self, opcode: u8, index: u16) -> Result<(), Unsupported> {
        let (owner, name, descriptor) =
            self.class.reference(index).ok_or(Unsupported::Malformed)?;
        let (name, descriptor) = (name.to_owned(), descriptor.to_owned());
        let owner_class = self.class_name(&owner);
        match opcode {
            0xb2 => self.push(
                Expression::Field {
                    owner: Owner::Static(owner_class),
                    name,
                },
                Kind::of_descriptor(&descriptor),
            ),
            0xb3 => {
                let value = self.pop()?;
                let target = Expression::Field {
                    owner: Owner::Static(owner_class),
                    name,
                };
                let value = coerced(value.expression, &descriptor);
                self.emit(Statement::Assign { target, value })?;
            }
            0xb4 => {
                let object = self.pop()?;
                self.push(
                    Expression::Field {
                        owner: Owner::Instance(Box::new(object.expression)),
                        name,
                    },
                    Kind::of_descriptor(&descriptor),
                );
            }
            _ => {
                let value = self.pop()?;
                let object = self.pop()?;
                let target = Expression::Field {
                    owner: Owner::Instance(Box::new(object.expression)),
                    name,
                };
                let value = coerced(value.expression, &descriptor);
                self.emit(Statement::Assign { target, value })?;
            }
        }
        Ok(())
    }

    /// The arguments of a call to a method of `descriptor`, in order, each
    /// literal as the parameter's type writes it.
    fn arguments(&mut self, descriptor: &str) -> Result<Vec<Expression>, Unsupported> {
        let parameters = parameter_descriptors(descriptor).ok_or(Unsupported::Malformed)?;
        let mut arguments = Vec::with_capacity(parameters.len());
        for parameter in parameters.iter().rev() {
            let value = self.pop()?;
            arguments.push(coerced(value.expression, parameter));
        }
        arguments.reverse();
        Ok(arguments)
    }

    fn invoke(&mut self, opcode: u8, index: u16) -> Result<(), Unsupported> {
        let (owner, name, descriptor) =
            self.class.reference(index).ok_or(Unsupported::Malformed)?;
        let (name, descriptor) = (name.to_owned(), descriptor.to_owned());
        let arguments = self.arguments(&descriptor)?;
        let result = result_descriptor(&descriptor)
            .ok_or(Unsupported::Malformed)?
            .to_owned();
        let owner_class = self.class_name(&owner);
        if opcode == 0xb8 {
            let call = Expression::Call {
                receiver: Receiver::Static(owner_class),
                name,
                arguments,
            };
            return self.call_result(call, &result);
        }
        let receiver = self.pop()?;
        let is_special = opcode == 0xb7;
        if is_special && name == "<init>" {
            return self.constructed(receiver, owner, owner_class, arguments);
        }
        let is_super_call =
            is_special && receiver.expression == Expression::This && owner != self.class.name;
        let receiver = if is_super_call {
            Receiver::Super
        } else {
            Receiver::Instance(Box::new(receiver.expression))
        };
        let call = Expression::Call {
            receiver,
            name,
            arguments,
        };
        self.call_result(call, &result)
    }

    /// A call's value, or — `void` — its statement. `new StringBuilder()
    /// .append(…)…toString()` reads back as the concatenation it was.
    fn call_result(&mut self, call: Expression, result: &str) -> Result<(), Unsupported> {
        if result == "V" {
            return self.emit(Statement::Expression(call));
        }
        let call = concatenation_of(&call).unwrap_or(call);
        self.push(call, Kind::of_descriptor(result));
        Ok(())
    }

    /// A constructor call: of a `new` object (then that object is built),
    /// or — in a constructor — `super(…)` or `this(…)`.
    fn constructed(
        &mut self,
        receiver: Value,
        owner: String,
        owner_class: ClassName,
        arguments: Vec<Expression>,
    ) -> Result<(), Unsupported> {
        if let Kind::Uninitialized(id) = receiver.kind {
            let class = self
                .uninitialized
                .get(id)
                .cloned()
                .ok_or(Unsupported::Malformed)?;
            let built = Expression::New { class, arguments };
            for value in self.stack.iter_mut() {
                if value.kind == Kind::Uninitialized(id) {
                    *value = Value {
                        expression: built.clone(),
                        kind: Kind::Reference,
                    };
                }
            }
            // `new Foo(…)` whose object nothing kept: a statement.
            if !self.stack.iter().any(|value| value.expression == built) {
                return self.emit(Statement::Expression(built));
            }
            return Ok(());
        }
        let is_own_constructor_call =
            self.is_constructor && receiver.expression == Expression::This;
        if !is_own_constructor_call {
            return Err(Unsupported::StackShape);
        }
        let _ = owner_class;
        let statement = if owner == self.class.name {
            Statement::ThisConstructor(arguments)
        } else {
            Statement::SuperConstructor(arguments)
        };
        self.emit(statement)
    }

    /// `invokedynamic`: string concatenation (`makeConcatWithConstants`,
    /// `makeConcat`); anything else is a lambda or the like.
    fn invoke_dynamic(&mut self, index: u16) -> Result<(), Unsupported> {
        let Some(Constant::InvokeDynamic {
            bootstrap_index,
            name_and_type_index,
        }) = self.class.constant(index)
        else {
            return Err(Unsupported::Malformed);
        };
        let (name, descriptor) = self
            .class
            .name_and_type(*name_and_type_index)
            .ok_or(Unsupported::Malformed)?;
        let bootstrap = self
            .class
            .bootstrap_methods
            .get(usize::from(*bootstrap_index))
            .ok_or(Unsupported::Malformed)?;
        let is_concatenation = self.bootstrap_owner(bootstrap.method_handle).as_deref()
            == Some("java/lang/invoke/StringConcatFactory");
        if !is_concatenation {
            return Err(Unsupported::Lambda);
        }
        let descriptor = descriptor.to_owned();
        let arguments = self.arguments(&descriptor)?;
        let pieces = match name {
            "makeConcat" => arguments,
            _ => {
                let recipe = bootstrap
                    .arguments
                    .first()
                    .and_then(|argument| match self.class.constant(*argument) {
                        Some(Constant::String { utf8_index }) => self.class.utf8(*utf8_index),
                        _ => None,
                    })
                    .ok_or(Unsupported::Malformed)?
                    .to_owned();
                let constants: Vec<Expression> = bootstrap.arguments[1..]
                    .iter()
                    .map(|argument| match self.class.constant(*argument) {
                        Some(Constant::String { utf8_index }) => self
                            .class
                            .utf8(*utf8_index)
                            .map(|text| Literal::String(text.to_owned()).into()),
                        _ => None,
                    })
                    .collect::<Option<_>>()
                    .ok_or(Unsupported::Malformed)?;
                pieces_of_recipe(&recipe, arguments, constants)?
            }
        };
        self.push(Expression::Concatenation(pieces), Kind::Reference);
        Ok(())
    }

    /// The class of a bootstrap method handle's method.
    fn bootstrap_owner(&self, method_handle: u16) -> Option<String> {
        let Constant::MethodHandle { reference_index } = self.class.constant(method_handle)? else {
            return None;
        };
        self.class
            .reference(*reference_index)
            .map(|(owner, _, _)| owner)
    }
}

impl From<Literal> for Expression {
    fn from(literal: Literal) -> Expression {
        Expression::Literal(literal)
    }
}

/// `makeConcatWithConstants`'s recipe: `\u{1}` an argument, `\u{2}` the
/// next constant, anything else text.
fn pieces_of_recipe(
    recipe: &str,
    arguments: Vec<Expression>,
    constants: Vec<Expression>,
) -> Result<Vec<Expression>, Unsupported> {
    let mut arguments = arguments.into_iter();
    let mut constants = constants.into_iter();
    let mut pieces = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, pieces: &mut Vec<Expression>| {
        if !text.is_empty() {
            pieces.push(Literal::String(std::mem::take(text)).into());
        }
    };
    for char in recipe.chars() {
        match char {
            '\u{1}' => {
                flush(&mut text, &mut pieces);
                pieces.push(arguments.next().ok_or(Unsupported::Malformed)?);
            }
            '\u{2}' => {
                flush(&mut text, &mut pieces);
                pieces.push(constants.next().ok_or(Unsupported::Malformed)?);
            }
            char => text.push(char),
        }
    }
    flush(&mut text, &mut pieces);
    Ok(pieces)
}

/// The pieces of `new StringBuilder(…).append(a)….toString()`.
fn concatenation_of(call: &Expression) -> Option<Expression> {
    let Expression::Call {
        receiver: Receiver::Instance(builder),
        name,
        arguments,
    } = call
    else {
        return None;
    };
    if name != "toString" || !arguments.is_empty() {
        return None;
    }
    let mut pieces = Vec::new();
    let mut current = builder.as_ref();
    loop {
        match current {
            Expression::Call {
                receiver: Receiver::Instance(inner),
                name,
                arguments,
            } if name == "append" && arguments.len() == 1 => {
                pieces.push(arguments[0].clone());
                current = inner;
            }
            Expression::New { class, arguments } if is_string_builder(class) => {
                match arguments.as_slice() {
                    [] => {}
                    [first] => pieces.push(first.clone()),
                    _ => return None,
                }
                break;
            }
            _ => return None,
        }
    }
    pieces.reverse();
    Some(Expression::Concatenation(pieces))
}

fn is_string_builder(class: &ClassName) -> bool {
    matches!(
        class.qualified().as_str(),
        "java.lang.StringBuilder" | "java.lang.StringBuffer"
    )
}

/// An expression that reads the same however often it's written: a
/// variable, `this`, a literal, a field of one of them.
fn is_repeatable(expression: &Expression) -> bool {
    match expression {
        Expression::This | Expression::Local(_) | Expression::Literal(_) => true,
        Expression::Field { owner, .. } => match owner {
            Owner::Static(_) => true,
            Owner::Instance(object) => is_repeatable(object),
        },
        _ => false,
    }
}

/// A literal as the type it goes to writes it: `true` for a `boolean`'s
/// `1`, `'a'` for a `char`'s `97`.
fn coerced(expression: Expression, descriptor: &str) -> Expression {
    match (expression, descriptor) {
        (Expression::Literal(Literal::Int(value)), "Z") if value == 0 || value == 1 => {
            Expression::Literal(Literal::Boolean(value == 1))
        }
        (Expression::Literal(Literal::Int(value)), "C") if (0..=0xffff).contains(&value) => {
            Expression::Literal(Literal::Char(value as u16))
        }
        (expression, _) => expression,
    }
}
