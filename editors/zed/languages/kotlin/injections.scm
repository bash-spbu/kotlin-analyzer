; Languages inside Kotlin: TODO/FIXME and the like in comments; SQL in a
; string that reads as SQL, one a `// language=SQL` comment marks, and
; Spring Data's `@Query`; regular expressions in `Regex(…)`, `toRegex()`
; and `Pattern.compile(…)`.
;
; A SQL string's pieces between templates (`$table`, `${id}`) are one
; match, so Zed parses them as one SQL text, the templates left out. Its
; patterns repeat the same string shape: `.` then the pieces, templates
; between them.

([
  (line_comment)
  (multiline_comment)
] @injection.content
  (#set! injection.language "comment"))

;; SQL

; A string that starts as SQL does — `SELECT … FROM`, `WITH name AS (`,
; `INSERT INTO`, `UPDATE name SET`, `DELETE FROM`, `MERGE INTO`, in any
; case — wherever it is: `jdbc.sql("""…""".trimIndent())`,
; `jdbcTemplate.query("…")`, `val query = "…"`.
((string_literal
    .
    [
      (string_content) @injection.content
      (interpolation_identifier_start)
      (interpolated_identifier)
      (interpolation_expression_start)
      (interpolated_expression)
      (interpolation_expression_end)
    ]+) @_literal
  (#match? @_literal "^\"+\\s*(?is:select\\s.+?\\sfrom\\b|with\\s+(recursive\\s+)?\\w+\\s+as\\s*\\(|insert\\s+into\\b|update\\s+\\S+\\s+set\\b|delete\\s+from\\b|merge\\s+into\\b)")
  (#set! injection.language "sql"))

; Any other a `// language=SQL` (or `/* language=SQL */`) comment marks,
; as IntelliJ reads it: on the line before a property or an argument.
([
  (line_comment)
  (multiline_comment)
] @_marker
  .
  [
    (property_declaration
      [
        (string_literal
          .
          [
            (string_content) @injection.content
            (interpolation_identifier_start)
            (interpolated_identifier)
            (interpolation_expression_start)
            (interpolated_expression)
            (interpolation_expression_end)
          ]+) @_literal
        (call_expression
          (navigation_expression
            (string_literal
              .
              [
                (string_content) @injection.content
                (interpolation_identifier_start)
                (interpolated_identifier)
                (interpolation_expression_start)
                (interpolated_expression)
                (interpolation_expression_end)
              ]+) @_literal))
      ])
    (value_argument
      [
        (string_literal
          .
          [
            (string_content) @injection.content
            (interpolation_identifier_start)
            (interpolated_identifier)
            (interpolation_expression_start)
            (interpolated_expression)
            (interpolation_expression_end)
          ]+) @_literal
        (call_expression
          (navigation_expression
            (string_literal
              .
              [
                (string_content) @injection.content
                (interpolation_identifier_start)
                (interpolated_identifier)
                (interpolation_expression_start)
                (interpolated_expression)
                (interpolation_expression_end)
              ]+) @_literal))
      ])
  ]
  (#match? @_marker "^(//|/\\*)\\s*language\\s*=\\s*(?i:sql)\\b")
  (#not-match? @_literal "^\"+\\s*(?is:select\\s.+?\\sfrom\\b|with\\s+(recursive\\s+)?\\w+\\s+as\\s*\\(|insert\\s+into\\b|update\\s+\\S+\\s+set\\b|delete\\s+from\\b|merge\\s+into\\b)")
  (#set! injection.language "sql"))

; Spring Data's @Query("…") and @Query(value = "…", nativeQuery = true),
; when the string doesn't read as SQL by itself (`from Markup m`).
(annotation
  (constructor_invocation
    (user_type (type_identifier) @_annotation .)
    (value_arguments
      (value_argument
        . (string_literal (string_content) @injection.content) @_literal)))
  (#eq? @_annotation "Query")
  (#not-match? @_literal "^\"+\\s*(?is:select\\s.+?\\sfrom\\b|with\\s+(recursive\\s+)?\\w+\\s+as\\s*\\(|insert\\s+into\\b|update\\s+\\S+\\s+set\\b|delete\\s+from\\b|merge\\s+into\\b)")
  (#set! injection.language "sql"))

(annotation
  (constructor_invocation
    (user_type (type_identifier) @_annotation .)
    (value_arguments
      (value_argument
        (simple_identifier) @_argument
        (string_literal (string_content) @injection.content) @_literal)))
  (#eq? @_annotation "Query")
  (#eq? @_argument "value")
  (#not-match? @_literal "^\"+\\s*(?is:select\\s.+?\\sfrom\\b|with\\s+(recursive\\s+)?\\w+\\s+as\\s*\\(|insert\\s+into\\b|update\\s+\\S+\\s+set\\b|delete\\s+from\\b|merge\\s+into\\b)")
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
