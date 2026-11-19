; Languages inside Kotlin: TODO/FIXME and the like in comments, SQL in
; Spring Data's `@Query`, regular expressions in `Regex(…)`, `toRegex()`
; and `Pattern.compile(…)`. A string with templates (`$name`) is injected
; piece by piece.

([
  (line_comment)
  (multiline_comment)
] @injection.content
  (#set! injection.language "comment"))

; @Query("select …") and @Query(value = "select …", nativeQuery = true)
(annotation
  (constructor_invocation
    (user_type (type_identifier) @_annotation .)
    (value_arguments
      (value_argument
        . (string_literal (string_content) @injection.content))))
  (#eq? @_annotation "Query")
  (#set! injection.language "sql"))

(annotation
  (constructor_invocation
    (user_type (type_identifier) @_annotation .)
    (value_arguments
      (value_argument
        (simple_identifier) @_argument
        (string_literal (string_content) @injection.content))))
  (#eq? @_annotation "Query")
  (#eq? @_argument "value")
  (#set! injection.language "sql"))

; Regex("…"), Pattern.compile("…")
(call_expression
  [
    (simple_identifier) @_function
    (navigation_expression (navigation_suffix (simple_identifier) @_function))
  ]
  (call_suffix
    (value_arguments
      . (value_argument
        . (string_literal (string_content) @injection.content))))
  (#any-of? @_function "Regex" "compile")
  (#set! injection.language "regex"))

; "…".toRegex()
(call_expression
  (navigation_expression
    (string_literal (string_content) @injection.content)
    (navigation_suffix (simple_identifier) @_function))
  (#eq? @_function "toRegex")
  (#set! injection.language "regex"))
