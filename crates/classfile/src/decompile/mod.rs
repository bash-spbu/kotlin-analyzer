//! Method bodies from bytecode, as Java syntax trees: for reading a
//! library without sources, not for recompiling it.
//!
//! A body's bytecode is cut into basic blocks (`graph`); each replays on
//! the operand stack with expressions in place of values (`machine`):
//! `aload_0; getfield name; areturn` is `return this.name;`. Values left
//! on the stack where blocks join are `?:`; a condition jumping where the
//! next one does is `&&` or `||`. The blocks are then nested into `if`,
//! loops, `switch` and labeled blocks (`structure`), and tidied into what
//! a person would write. A body with what isn't read (`try`, lambdas, a
//! graph no loop nesting describes) is [`Unsupported`], and shown as
//! compiled code — a body is decompiled whole or not at all.

mod graph;
mod machine;
mod shapes;
mod structure;

use crate::class_file::{Bodies, ClassFile, Code, access};
use crate::signature::NestedNames;
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
    /// Its jumps don't nest as loops and blocks do (an irreducible graph),
    /// or use `jsr`.
    Branches,
    /// It has `try`/`catch`/`finally` or `synchronized`.
    ExceptionHandlers,
    /// A lambda, method reference or other `invokedynamic` but string
    /// concatenation.
    Lambda,
    /// The operand stack holds values across statements (`dup` of a call,
    /// an array initializer, `i++` inside an expression) or across a
    /// join that isn't `?:`.
    StackShape,
    /// A variable declared in two places needs one declaration, and
    /// nothing records its type (compiled without `-g`).
    UntypedVariable,
    /// An instruction not decompiled (its opcode).
    Instruction(u8),
    /// Bytecode that doesn't read: truncated, an unknown constant.
    Malformed,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Body {
    pub statements: Vec<Statement>,
}

/// A label of a loop or block, which `break` and `continue` name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Label(pub u32);

#[derive(Clone, PartialEq, Debug)]
pub enum Statement {
    Expression(Expression),
    /// `Type name = value;`, or `Type name;` — `var` without a recorded
    /// type.
    Declare {
        type_ref: Option<LibraryType>,
        name: String,
        value: Option<Expression>,
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
    If {
        condition: Expression,
        then: Vec<Statement>,
        otherwise: Vec<Statement>,
    },
    Loop {
        label: Option<Label>,
        kind: LoopKind,
        body: Vec<Statement>,
    },
    /// `label: { … }`: what `break label` leaves.
    Block {
        label: Label,
        body: Vec<Statement>,
    },
    Break(Option<Label>),
    Continue(Option<Label>),
    Switch {
        value: Expression,
        cases: Vec<SwitchCase>,
    },
    Try {
        body: Vec<Statement>,
        catches: Vec<CatchClause>,
    },
}

/// `catch (A | B name) { … }`.
#[derive(Clone, PartialEq, Debug)]
pub struct CatchClause {
    pub types: Vec<ClassName>,
    pub name: String,
    pub body: Vec<Statement>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum LoopKind {
    /// `while (true)`.
    Forever,
    While(Expression),
    DoWhile(Expression),
    /// `for (initializer; condition; update)`.
    For {
        initializer: Option<Box<Statement>>,
        condition: Expression,
        update: Box<Statement>,
    },
    /// `for (Type name : iterable)`: over an array or an `Iterable`.
    ForEach {
        type_ref: Option<LibraryType>,
        name: String,
        iterable: Expression,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub struct SwitchCase {
    /// The `case` values: numbers, or a `switch` on strings' strings.
    pub values: Vec<Literal>,
    /// Whether it is (also) `default`.
    pub default: bool,
    pub body: Vec<Statement>,
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
    /// `a < b`. `floating` when the operands are `float` or `double`:
    /// with NaN, `!(a < b)` isn't `a >= b`.
    Compare {
        relation: Relation,
        left: Box<Expression>,
        right: Box<Expression>,
        floating: bool,
    },
    Not(Box<Expression>),
    Logical {
        operator: LogicalOperator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    /// `condition ? then : otherwise`.
    Conditional {
        condition: Box<Expression>,
        then: Box<Expression>,
        otherwise: Box<Expression>,
    },
    /// `new T[] { a, b }`: `length` elements, `values` the first ones (the
    /// rest the default).
    ArrayInitializer {
        element: LibraryType,
        length: usize,
        values: Vec<Expression>,
    },
    /// `name++` (`by` 1) or `name--` (`by` -1) as a value: the variable's
    /// value before.
    PostIncrement {
        name: String,
        by: i32,
    },
    /// `Integer.valueOf(x)` as javac boxes a primitive: where Java boxes
    /// by itself (a return, an assignment), just `x`.
    Boxed {
        class: ClassName,
        value: Box<Expression>,
    },
    /// `x.intValue()` as javac unboxes: where Java unboxes by itself, `x`.
    Unboxed {
        method: String,
        value: Box<Expression>,
    },
    /// `(a, b) -> a + b`, or with a block body.
    Lambda {
        parameters: Vec<String>,
        body: LambdaBody,
    },
    /// `String::length`, `this::helper`, `Order::new`.
    MethodReference {
        target: ReferenceTarget,
        /// `new` for a constructor.
        name: String,
    },
    /// What `lcmp`, `fcmpl`… leave for the branch after them: never in a
    /// finished body.
    CompareResult {
        left: Box<Expression>,
        right: Box<Expression>,
        nan: NanOrder,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub enum LambdaBody {
    Expression(Box<Expression>),
    Block(Vec<Statement>),
}

/// What a method reference is on.
#[derive(Clone, PartialEq, Debug)]
pub enum ReferenceTarget {
    /// `Type::method`: static, or the first argument is the receiver.
    Class(ClassName),
    /// `expression::method`: bound to that receiver.
    Bound(Box<Expression>),
    /// `super::method`.
    Super,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Relation {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl Relation {
    /// The relation holding exactly when this one doesn't — for numbers
    /// without NaN.
    fn inverse(self) -> Relation {
        match self {
            Relation::Equal => Relation::NotEqual,
            Relation::NotEqual => Relation::Equal,
            Relation::Less => Relation::GreaterOrEqual,
            Relation::LessOrEqual => Relation::Greater,
            Relation::Greater => Relation::LessOrEqual,
            Relation::GreaterOrEqual => Relation::Less,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogicalOperator {
    And,
    Or,
}

/// What a comparison instruction gives for NaN: `fcmpg`/`dcmpg` 1,
/// `fcmpl`/`dcmpl` -1; `lcmp` has no NaN.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NanOrder {
    Integral,
    Greater,
    Less,
}

impl From<Literal> for Expression {
    fn from(literal: Literal) -> Expression {
        Expression::Literal(literal)
    }
}

/// Decompiles the methods of a class file; `None` if it doesn't parse.
pub fn decompile_class(bytes: &[u8]) -> Option<DecompiledClass> {
    let class = ClassFile::parse_keeping(bytes, Bodies::Kept)?;
    let names = NestedNames::new(&class.inner_classes);
    let methods = class
        .methods
        .iter()
        .map(|method| {
            let (parameter_names, body) = decompile_method(&class, &names, method, 0);
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

/// How deep lambdas nest in lambdas before a body isn't read.
const LAMBDA_DEPTH_LIMIT: usize = 8;

/// A method's parameters' names and its body; `depth` lambdas deep.
fn decompile_method(
    class: &ClassFile,
    names: &NestedNames,
    method: &crate::class_file::Member,
    depth: usize,
) -> (Vec<String>, Result<Body, Unsupported>) {
    let is_static = method.access & access::STATIC != 0;
    let parameters = parameter_descriptors(&method.descriptor).unwrap_or_default();
    let code = method.code.as_ref();
    let parameter_names = parameter_names(code, &method.parameter_names, &parameters, is_static);
    let body = match code {
        None => Err(Unsupported::NoCode),
        Some(_) if depth > LAMBDA_DEPTH_LIMIT => Err(Unsupported::Lambda),
        // `finally`, `synchronized` and try-with-resources catch anything.
        Some(code)
            if code
                .exception_handlers
                .iter()
                .any(|handler| handler.catch_type.is_none()) =>
        {
            Err(Unsupported::ExceptionHandlers)
        }
        Some(code) => {
            let machine =
                machine::Machine::new(class, names, method, code, &parameter_names, depth);
            graph::build(machine).and_then(structure::structure)
        }
    };
    (parameter_names, body)
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

/// The condition holding exactly when `condition` doesn't, written as
/// directly as it can be: `a >= b` for `!(a < b)` on integers, De Morgan
/// through `&&` and `||`.
pub(crate) fn negated(condition: Expression) -> Expression {
    match condition {
        Expression::Not(inner) => *inner,
        Expression::Literal(Literal::Boolean(value)) => Literal::Boolean(!value).into(),
        Expression::Compare {
            relation,
            left,
            right,
            floating,
        } if !floating || matches!(relation, Relation::Equal | Relation::NotEqual) => {
            Expression::Compare {
                relation: relation.inverse(),
                left,
                right,
                floating,
            }
        }
        Expression::Logical {
            operator,
            left,
            right,
        } => Expression::Logical {
            operator: match operator {
                LogicalOperator::And => LogicalOperator::Or,
                LogicalOperator::Or => LogicalOperator::And,
            },
            left: Box::new(negated(*left)),
            right: Box::new(negated(*right)),
        },
        other => Expression::Not(Box::new(other)),
    }
}

/// A value as the type it goes to writes it: `true` for a `boolean`'s
/// `1`, `'a'` for a `char`'s `97`; a `boolean`'s `c ? 1 : 0` is `c`.
pub(crate) fn coerced(expression: Expression, descriptor: &str) -> Expression {
    match (expression, descriptor) {
        (Expression::Literal(Literal::Int(value)), "Z") if value == 0 || value == 1 => {
            Expression::Literal(Literal::Boolean(value == 1))
        }
        (Expression::Literal(Literal::Int(value)), "C") if (0..=0xffff).contains(&value) => {
            Expression::Literal(Literal::Char(value as u16))
        }
        (
            Expression::Conditional {
                condition,
                then,
                otherwise,
            },
            descriptor,
        ) => {
            let then = coerced(*then, descriptor);
            let otherwise = coerced(*otherwise, descriptor);
            match (&then, &otherwise) {
                (
                    Expression::Literal(Literal::Boolean(true)),
                    Expression::Literal(Literal::Boolean(false)),
                ) => *condition,
                (
                    Expression::Literal(Literal::Boolean(false)),
                    Expression::Literal(Literal::Boolean(true)),
                ) => negated(*condition),
                _ => Expression::Conditional {
                    condition,
                    then: Box::new(then),
                    otherwise: Box::new(otherwise),
                },
            }
        }
        (expression, _) => expression,
    }
}

/// `name` replaced by `value` wherever `statements` read it.
fn substitute_local(statements: &mut [Statement], name: &str, value: &Expression) {
    for statement in statements.iter_mut() {
        statement_expressions_mut(statement, &mut |expression| {
            if *expression == Expression::Local(name.to_owned()) {
                *expression = value.clone();
            }
        });
    }
}

/// `visit` on every expression in `statement`, inner ones first.
fn statement_expressions_mut(statement: &mut Statement, visit: &mut dyn FnMut(&mut Expression)) {
    let mut each = |expression: &mut Expression| expression_mut(expression, visit);
    match statement {
        Statement::Expression(expression)
        | Statement::Return(Some(expression))
        | Statement::Throw(expression) => each(expression),
        Statement::Declare { value, .. } => {
            if let Some(value) = value {
                each(value);
            }
        }
        Statement::Assign { target, value } => {
            each(target);
            each(value);
        }
        Statement::SuperConstructor(arguments) | Statement::ThisConstructor(arguments) => {
            arguments.iter_mut().for_each(each);
        }
        Statement::If {
            condition,
            then,
            otherwise,
        } => {
            each(condition);
            for inner in then.iter_mut().chain(otherwise.iter_mut()) {
                statement_expressions_mut(inner, visit);
            }
        }
        Statement::Loop { kind, body, .. } => {
            match kind {
                LoopKind::Forever => {}
                LoopKind::While(condition) | LoopKind::DoWhile(condition) => each(condition),
                LoopKind::For {
                    initializer,
                    condition,
                    update,
                } => {
                    each(condition);
                    if let Some(initializer) = initializer {
                        statement_expressions_mut(initializer, visit);
                    }
                    statement_expressions_mut(update, visit);
                }
                LoopKind::ForEach { iterable, .. } => each(iterable),
            }
            for inner in body.iter_mut() {
                statement_expressions_mut(inner, visit);
            }
        }
        Statement::Block { body, .. } => {
            for inner in body.iter_mut() {
                statement_expressions_mut(inner, visit);
            }
        }
        Statement::Switch { value, cases } => {
            each(value);
            for inner in cases.iter_mut().flat_map(|case| case.body.iter_mut()) {
                statement_expressions_mut(inner, visit);
            }
        }
        Statement::Try { body, catches } => {
            let all = body
                .iter_mut()
                .chain(catches.iter_mut().flat_map(|clause| clause.body.iter_mut()));
            for inner in all {
                statement_expressions_mut(inner, visit);
            }
        }
        Statement::Increment { .. }
        | Statement::Return(None)
        | Statement::Break(_)
        | Statement::Continue(_) => {}
    }
}

/// `visit` on `expression` and every expression inside it, inner first.
fn expression_mut(expression: &mut Expression, visit: &mut dyn FnMut(&mut Expression)) {
    match expression {
        Expression::This
        | Expression::Local(_)
        | Expression::Literal(_)
        | Expression::ClassLiteral(_)
        | Expression::PostIncrement { .. } => {}
        Expression::ArrayInitializer { values, .. } => values
            .iter_mut()
            .for_each(|value| expression_mut(value, visit)),
        Expression::Field { owner, .. } => {
            if let Owner::Instance(object) = owner {
                expression_mut(object, visit);
            }
        }
        Expression::Call {
            receiver,
            arguments,
            ..
        } => {
            if let Receiver::Instance(object) = receiver {
                expression_mut(object, visit);
            }
            arguments
                .iter_mut()
                .for_each(|argument| expression_mut(argument, visit));
        }
        Expression::New { arguments, .. } => arguments
            .iter_mut()
            .for_each(|argument| expression_mut(argument, visit)),
        Expression::Concatenation(pieces) => pieces
            .iter_mut()
            .for_each(|piece| expression_mut(piece, visit)),
        Expression::NewArray { length, .. } => expression_mut(length, visit),
        Expression::ArrayElement { array, index } => {
            expression_mut(array, visit);
            expression_mut(index, visit);
        }
        Expression::ArrayLength(operand)
        | Expression::Negate(operand)
        | Expression::Not(operand)
        | Expression::Cast { operand, .. }
        | Expression::InstanceOf { operand, .. }
        | Expression::Boxed { value: operand, .. }
        | Expression::Unboxed { value: operand, .. } => expression_mut(operand, visit),
        Expression::Binary { left, right, .. }
        | Expression::Logical { left, right, .. }
        | Expression::Compare { left, right, .. }
        | Expression::CompareResult { left, right, .. } => {
            expression_mut(left, visit);
            expression_mut(right, visit);
        }
        Expression::Conditional {
            condition,
            then,
            otherwise,
        } => {
            expression_mut(condition, visit);
            expression_mut(then, visit);
            expression_mut(otherwise, visit);
        }
        Expression::Lambda { body, .. } => match body {
            LambdaBody::Expression(value) => expression_mut(value, visit),
            LambdaBody::Block(statements) => {
                for statement in statements.iter_mut() {
                    statement_expressions_mut(statement, visit);
                }
            }
        },
        Expression::MethodReference { target, .. } => {
            if let ReferenceTarget::Bound(receiver) = target {
                expression_mut(receiver, visit);
            }
        }
    }
    visit(expression);
}

/// A value where Java boxes and unboxes by itself: without javac's
/// `valueOf`/`xxxValue()` around it.
pub(crate) fn plain(expression: Expression) -> Expression {
    match expression {
        Expression::Boxed { value, .. } | Expression::Unboxed { value, .. } => *value,
        other => other,
    }
}
