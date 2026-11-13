; Kotlin highlighting for kotlin-analyzer's Zed extension. Later patterns
; take precedence over earlier ones: general captures come first.

(simple_identifier) @variable
(type_identifier) @type

; Members reached with `.`: properties unless called (below).
(navigation_suffix (simple_identifier) @property)

; `this`, `super`, labels.
(this_expression) @variable.special
(super_expression) @variable.special
(label) @label

; Declarations.
(class_declaration (type_identifier) @type)
(object_declaration (type_identifier) @type)
(companion_object (type_identifier) @type)
(type_alias (type_identifier) @type)
(type_parameter (type_identifier) @type)
(function_declaration (simple_identifier) @function)
(property_declaration (variable_declaration (simple_identifier) @property))
(class_parameter (simple_identifier) @property)
(enum_entry (simple_identifier) @constant)

; Calls: `f(…)`, `x.f(…)`, and a capitalised callee as a constructor.
(call_expression (simple_identifier) @function)
(call_expression (navigation_expression (navigation_suffix (simple_identifier) @function)))
((call_expression (simple_identifier) @constructor)
  (#match? @constructor "^[A-Z]"))
(constructor_invocation (user_type (type_identifier) @constructor))
(delegation_specifier (user_type (type_identifier) @type))

; Constants by convention: `MAX_SIZE`, `Status.ACTIVE`.
((simple_identifier) @constant
  (#match? @constant "^[A-Z][A-Z0-9_]+$"))

; Annotations.
(annotation "@" @attribute)
(annotation (user_type (type_identifier) @attribute))
(annotation (constructor_invocation (user_type (type_identifier) @attribute)))
(file_annotation "@" @attribute)
(file_annotation (user_type (type_identifier) @attribute))
(file_annotation (constructor_invocation (user_type (type_identifier) @attribute)))
(use_site_target) @attribute

; Literals.
(string_literal) @string
(string_content) @string
(character_literal) @string
(character_escape_seq) @string.escape
(interpolation_identifier_start) @punctuation.special
(interpolation_expression_start) @punctuation.special
(interpolation_expression_end) @punctuation.special
(interpolated_identifier) @variable
[
  (integer_literal)
  (long_literal)
  (hex_literal)
  (bin_literal)
  (unsigned_literal)
  (real_literal)
] @number
(boolean_literal) @boolean
(null_literal) @constant

; Comments.
(line_comment) @comment
(multiline_comment) @comment
((multiline_comment) @comment.doc
  (#match? @comment.doc "^/\\*\\*"))
(shebang_line) @preproc

; Keywords.
[
  "package"
  "import"
  "class"
  "interface"
  "object"
  "fun"
  "typealias"
  "constructor"
  "init"
  "companion"
  "by"
  "where"
  "get"
  "set"
  "field"
  "dynamic"
  "val"
  "var"
] @keyword

[
  "if"
  "else"
  "when"
  "for"
  "while"
  "do"
  "try"
  "catch"
  "finally"
  "return"
  "return@"
  "throw"
  "break"
  "break@"
  "continue"
  "continue@"
  "in"
  "is"
  "as"
  "as?"
] @keyword

[
  (visibility_modifier)
  (class_modifier)
  (member_modifier)
  (function_modifier)
  (property_modifier)
  (inheritance_modifier)
  (parameter_modifier)
  (platform_modifier)
  (variance_modifier)
  (reification_modifier)
] @keyword

(binding_pattern_kind) @keyword
(this_expression "this" @variable.special)
(super_expression "super" @variable.special)

; Operators and punctuation.
[
  "+" "-" "*" "/" "%"
  "=" "+=" "-=" "*=" "/=" "%="
  "==" "!=" "===" "!=="
  "<" ">" "<=" ">="
  "&&" "||" "!" "!!"
  "++" "--"
  "?:" "?." ".." "..<" "->" "::"
] @operator

["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," "." ":" ";"] @punctuation.delimiter
