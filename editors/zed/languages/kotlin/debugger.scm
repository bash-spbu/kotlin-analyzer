; Inline values while debugging: the names whose values Zed shows at the
; end of their lines, up to the line the program stopped at. A name shows
; only in a scope holding that line, and only while the stopped frame has
; a variable of that name. Capitalized names are classes and objects.

(parameter
  (simple_identifier) @debug-variable)

(variable_declaration
  (simple_identifier) @debug-variable)

(directly_assignable_expression
  (simple_identifier) @debug-variable)

(this_expression) @debug-variable

(navigation_expression
  .
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(for_statement
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(property_declaration
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(assignment
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(additive_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(multiplicative_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(comparison_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(equality_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(conjunction_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(disjunction_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(elvis_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(range_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(infix_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(check_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(as_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(prefix_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(postfix_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(parenthesized_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(indexing_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(jump_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(if_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(when_subject
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(value_argument
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(interpolated_expression
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(collection_literal
  (simple_identifier) @debug-variable
  (#not-match? @debug-variable "^[A-Z]"))

(interpolated_identifier) @debug-variable

(function_body) @debug-scope

(lambda_literal) @debug-scope

(control_structure_body) @debug-scope

(for_statement) @debug-scope

(catch_block) @debug-scope
