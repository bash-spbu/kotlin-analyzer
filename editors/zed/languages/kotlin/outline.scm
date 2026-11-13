; The outline panel and breadcrumbs: classes, objects, type aliases, and
; the functions and properties of files and classes (not local ones).

(class_declaration
  ["class" "interface"] @context
  (type_identifier) @name) @item

(object_declaration
  "object" @context
  (type_identifier) @name) @item

(companion_object
  "companion" @context
  "object" @name) @item

(type_alias
  "typealias" @context
  (type_identifier) @name) @item

(enum_class_body
  (enum_entry (simple_identifier) @name) @item)

(source_file
  (function_declaration
    "fun" @context
    (simple_identifier) @name) @item)

(class_body
  (function_declaration
    "fun" @context
    (simple_identifier) @name) @item)

(source_file
  (property_declaration
    (binding_pattern_kind) @context
    (variable_declaration (simple_identifier) @name)) @item)

(class_body
  (property_declaration
    (binding_pattern_kind) @context
    (variable_declaration (simple_identifier) @name)) @item)

(primary_constructor
  (class_parameter
    (binding_pattern_kind) @context
    (simple_identifier) @name) @item)
