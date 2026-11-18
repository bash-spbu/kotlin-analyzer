//! A value whose class can't be the one the place it goes to declares —
//! Kotlin's `RETURN_TYPE_MISMATCH`, `INITIALIZER_TYPE_MISMATCH`,
//! `ASSIGNMENT_TYPE_MISMATCH`, `ARGUMENT_TYPE_MISMATCH`: a
//! `ConversionResult` returned from a function declared to give a
//! `BigDecimal`, `val count: Int = "x"`. Like rust-analyzer's
//! `emit_type_mismatch`, one place records them, where a type is required
//! rather than hinted. Only a class mismatch with every supertype known is
//! reported — not one of nullability alone (smart casts from contracts
//! such as `checkNotNull` aren't all modelled), not type arguments — and
//! only for a value whose type inference knows as the compiler does (see
//! `examinable`: a generic call's may come from what it's passed to).

use syntax::{SyntaxNode, TextRange, named_children};

use super::declarations::Inferrer;
use super::fit::Fit;
use super::{MismatchSite, TypeMismatch};
use crate::ty::{Nullability, Ty};

/// The classes the compiler converts an integer literal between
/// (`fun f(): Long = 1`).
const NUMBERS: [&str; 10] = [
    "Byte", "Short", "Int", "Long", "Float", "Double", "UByte", "UShort", "UInt", "ULong",
];

impl Inferrer<'_> {
    /// Records `value`, of type `actual`, where `site` requires `expected`,
    /// when its class can't be `expected`'s.
    pub(super) fn check_value(
        &mut self,
        site: MismatchSite,
        value: SyntaxNode<'_>,
        actual: &Ty,
        expected: &Ty,
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
        let mismatch = !from_java
            && !may_be_null_only
            && self.examinable(value)
            && self.class_mismatch(actual, expected);
        let finding = mismatch.then(|| TypeMismatch {
            site,
            range: salient_range(value),
            expected: expected.clone(),
            actual: actual.clone(),
        });
        self.record_type_mismatch(value.start_byte() as u32, finding);
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
