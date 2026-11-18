//! A value whose class can't be the one the place it goes to declares —
//! Kotlin's `RETURN_TYPE_MISMATCH`, `INITIALIZER_TYPE_MISMATCH`,
//! `ASSIGNMENT_TYPE_MISMATCH`, `ARGUMENT_TYPE_MISMATCH`: a
//! `ConversionResult` returned from a function declared to give a
//! `BigDecimal`, `val count: Int = "x"`. Like rust-analyzer's
//! `emit_type_mismatch`, one place records them, where a type is required
//! rather than hinted — a [`Requirement`], beside the type inference is
//! merely told to expect. Like rust-analyzer's `CoerceMany`, a requirement
//! on a branching expression (`if`, `when`, `try`, `?:`) passes to the
//! value each branch ends with, so a mismatch is shown on that innermost
//! value. A class mismatch with every supertype known is reported, and
//! one of nullability alone for a local (see `null_mismatch`), not type
//! arguments — and only for a value whose type inference knows as the
//! compiler does (see `examinable`: a generic call's may come from what
//! it's passed to).

use syntax::{SyntaxNode, TextRange, named_children};

use super::declarations::Inferrer;
use super::expressions::Typed;
use super::fit::Fit;
use super::{MismatchSite, TypeMismatch};
use crate::ty::{Nullability, Ty};

/// Whether a value that may be null is judged where one that can't goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum NullCheck {
    Judged,
    /// A Java method's: its overloads that differ in boxing alone
    /// (`isGreaterThan(long)`, `isGreaterThan(Long)`) aren't all weighed.
    Skipped,
}

/// The classes the compiler converts an integer literal between
/// (`fun f(): Long = 1`).
const NUMBERS: [&str; 10] = [
    "Byte", "Short", "Int", "Long", "Float", "Double", "UByte", "UShort", "UInt", "ULong",
];

/// The type a value must have where it goes (rust-analyzer's
/// `Expectation::HasType`), and what makes it so.
#[derive(Clone, Debug)]
pub(crate) struct Requirement {
    site: MismatchSite,
    ty: Ty,
}

impl Requirement {
    pub(super) fn new(site: MismatchSite, ty: &Ty) -> Requirement {
        Requirement {
            site,
            ty: ty.clone(),
        }
    }
}

/// How an expression meets a requirement.
enum Meets {
    /// Its value is checked.
    Itself,
    /// Its parenthesized expression does.
    Inner,
    /// The value each of its branches ends with does.
    Branches,
}

fn meets(node: SyntaxNode<'_>) -> Meets {
    match node.kind() {
        "parenthesized_expression" => Meets::Inner,
        "if_expression" | "when_expression" | "try_expression" | "elvis_expression" => {
            Meets::Branches
        }
        _ => Meets::Itself,
    }
}

/// Statements that aren't a value to check: declarations, labels, comments.
fn is_value_statement(node: SyntaxNode<'_>) -> bool {
    !matches!(
        node.kind(),
        "property_declaration"
            | "function_declaration"
            | "class_declaration"
            | "object_declaration"
            | "label"
            | "line_comment"
            | "multiline_comment"
            | "statements"
    )
}

impl Inferrer<'_> {
    /// Infers `value` (with `hint` to guide inference), which must meet
    /// `requirement`.
    pub(super) fn required(
        &mut self,
        value: SyntaxNode<'_>,
        hint: Option<&Ty>,
        requirement: Requirement,
    ) -> Ty {
        let outer = self.requirement.replace(requirement);
        let ty = self.expression(value, hint);
        self.requirement = outer;
        ty
    }

    /// `node` inferred where `requirement` holds: checked itself, or
    /// through what it gives its value from.
    pub(super) fn typed_required(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        requirement: Requirement,
    ) -> Typed {
        match meets(node) {
            Meets::Inner => {
                self.requirement = Some(requirement);
                let typed = self.typed_inner(node, expected);
                self.requirement = None;
                typed
            }
            Meets::Branches => {
                self.branch_requirement = Some(requirement);
                let typed = self.typed_inner(node, expected);
                self.branch_requirement = None;
                typed
            }
            Meets::Itself => {
                let typed = self.typed_inner(node, expected);
                let ty = self.value_of(typed.clone());
                self.check_value(
                    requirement.site,
                    node,
                    &ty,
                    &requirement.ty,
                    NullCheck::Judged,
                );
                typed
            }
        }
    }

    /// A branch body whose value must meet `requirement`, when there is one.
    pub(super) fn body_required(
        &mut self,
        body: SyntaxNode<'_>,
        expected: Option<&Ty>,
        requirement: Option<Requirement>,
    ) -> Ty {
        let Some(requirement) = requirement else {
            return self.body(body, expected);
        };
        let content: Vec<SyntaxNode<'_>> = named_children(body).collect();
        match content[..] {
            [only] if only.kind() == "statements" => self.block_required(only, Some(requirement)),
            [only] if body.kind() == "control_structure_body" && is_value_statement(only) => {
                self.push_scope();
                let ty = self.required(only, expected, requirement);
                self.pop_scope();
                ty
            }
            _ => self.body(body, expected),
        }
    }

    /// A block whose last statement's value must meet `requirement`.
    pub(super) fn block_required(
        &mut self,
        node: SyntaxNode<'_>,
        requirement: Option<Requirement>,
    ) -> Ty {
        self.push_scope();
        let ty = self.statements_required(node, requirement);
        self.pop_scope();
        ty
    }

    /// Statements whose last one's value must meet `requirement`.
    pub(super) fn statements_required(
        &mut self,
        node: SyntaxNode<'_>,
        requirement: Option<Requirement>,
    ) -> Ty {
        let Some(requirement) = requirement else {
            return self.statements(node);
        };
        if node.kind() != "statements" {
            return self.statements(node);
        }
        let statements: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let Some((&last, leading)) = statements.split_last() else {
            return self.types.builtin("Unit");
        };
        for statement in leading {
            self.statement(*statement);
        }
        if !is_value_statement(last) {
            return self.statement(last);
        }
        self.requirement = Some(requirement);
        let ty = self.statement(last);
        self.requirement = None;
        ty
    }

    /// Records `value`, of type `actual`, where `site` requires `expected`,
    /// when its class can't be `expected`'s.
    pub(super) fn check_value(
        &mut self,
        site: MismatchSite,
        value: SyntaxNode<'_>,
        actual: &Ty,
        expected: &Ty,
        null_check: NullCheck,
    ) {
        // A Java value's type (`T!`) may come through generics this
        // inference doesn't follow (a builder's `build(): T`).
        let from_java = actual.nullability() == Nullability::Platform;
        // A nullable name where a nullable value goes may be smart-cast to
        // `Nothing?` (`if (token == null) copy(token = token)`).
        let both_nullable = actual.nullability() == Nullability::Nullable
            && expected.nullability() == Nullability::Nullable;
        let may_be_null_only =
            both_nullable && matches!(value.kind(), "simple_identifier" | "navigation_expression");
        // The class comparison first: whether the value is certain asks
        // receivers for members, and almost every value fits anyway.
        let class_mismatch = !from_java
            && !may_be_null_only
            && self.class_mismatch(actual, expected)
            && self.examinable(value);
        let mismatch = class_mismatch
            || null_check == NullCheck::Judged && self.null_mismatch(value, actual, expected);
        let finding = mismatch.then(|| TypeMismatch {
            site,
            range: salient_range(value),
            expected: expected.clone(),
            actual: actual.clone(),
        });
        self.record_type_mismatch(value.start_byte() as u32, finding);
    }

    /// A local that may be null where a value that can't be goes: its
    /// class fits, its `?` doesn't. Only a local, whose smart casts this
    /// inference follows as the compiler does (assignments, conditions,
    /// contracts, jumps), and only with the whole classpath — a contract
    /// on a library function that's missing would have narrowed it.
    fn null_mismatch(&self, value: SyntaxNode<'_>, actual: &Ty, expected: &Ty) -> bool {
        let is_local = value.kind() == "simple_identifier"
            && matches!(
                self.recorded_at(value).map(|resolved| resolved.reference),
                Some(super::Reference::Local(_))
            );
        let only_null_differs = actual.nullability() == Nullability::Nullable
            && expected.nullability() == Nullability::NotNull
            && matches!(expected, Ty::Class(_))
            && self.types.fits(&actual.clone().non_null(), expected) == Fit::Yes;
        is_local
            && only_null_differs
            && hir_def::def_map::classpath_complete(self.db, self.file)
            && self.examinable(value)
    }

    /// Whether no value of `actual`'s class is an `expected`, ignoring
    /// nullability and type arguments. A mutable collection takes its
    /// read-only one: Java's `java.util.List` is Kotlin's `(Mutable)List`,
    /// which a `List` may be passed as. A class of the same qualified name
    /// is the same class (one library on the classpath twice).
    fn class_mismatch(&self, actual: &Ty, expected: &Ty) -> bool {
        let (Ty::Class(actual_class), Ty::Class(expected_class)) = (actual, expected) else {
            return false;
        };
        let numbers = self.is_number(actual_class.class) && self.is_number(expected_class.class);
        // An `expect` class may be an `actual typealias` to another
        // (`ProxyConfig` = `java.net.Proxy`).
        let expected_class_is_expect =
            self.is_expect(actual_class.class) || self.is_expect(expected_class.class);
        if numbers || expected_class_is_expect {
            return false;
        }
        let required = match self.types.read_only_collection(expected_class) {
            Some(read_only) => crate::ty::ClassType {
                class: read_only,
                arguments: expected_class.arguments.clone(),
                nullability: Nullability::NotNull,
            },
            None => crate::ty::ClassType {
                nullability: Nullability::NotNull,
                ..expected_class.clone()
            },
        };
        let fit = self
            .types
            .fits(&actual.clone().non_null(), &Ty::Class(required.clone()));
        fit == Fit::No && !self.has_supertype_named_like(actual_class, required.class)
    }

    /// Whether `class_type` or a supertype has `class`'s qualified name.
    fn has_supertype_named_like(
        &self,
        class_type: &crate::ty::ClassType,
        class: hir_def::ItemLocation,
    ) -> bool {
        let wanted = qualified_name(self.db, class);
        self.types
            .all_supertypes(class_type)
            .iter()
            .any(|supertype| qualified_name(self.db, supertype.class) == wanted)
    }

    fn is_expect(&self, class: hir_def::ItemLocation) -> bool {
        hir_def::item_tree(self.db, class.container)
            .item(class.item)
            .modifiers
            .contains(hir_def::item_tree::Modifiers::EXPECT)
    }

    fn is_number(&self, class: hir_def::ItemLocation) -> bool {
        NUMBERS.iter().any(
            |name| matches!(self.types.builtin(name), Ty::Class(number) if number.class == class),
        )
    }
}

/// `package.Outer.Name`.
fn qualified_name(db: &dyn base_db::SourceDatabase, class: hir_def::ItemLocation) -> String {
    let tree = hir_def::item_tree(db, class.container);
    format!("{}.{}", tree.package, crate::display::class_name(db, class))
}

/// Where to show a mismatch, as rust-analyzer narrows it: the member name
/// of `a.b.f(…)` or `a.b`, not the whole (often multi-line) chain.
fn salient_range(value: SyntaxNode<'_>) -> TextRange {
    let salient = match value.kind() {
        "call_expression" => named_children(value)
            .next()
            .filter(|callee| callee.kind() == "navigation_expression")
            .and_then(member_name),
        "navigation_expression" => member_name(value),
        _ => None,
    };
    TextRange::of(salient.unwrap_or(value))
}

/// The member name of `receiver.name`.
fn member_name(navigation: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let suffix = named_children(navigation).last()?;
    named_children(suffix).find(|child| child.kind() == "simple_identifier")
}
