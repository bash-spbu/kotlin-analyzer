; Languages inside Kotlin: TODO/FIXME and the like in comments; SQL in a
; string that reads as SQL, one a `// language=SQL` comment marks, and
; Spring Data's `@Query`; JSON and YAML in a string a `// language=JSON`
; or `// language=YAML` comment marks; regular expressions in `Regex(…)`,
; `toRegex()` and `Pattern.compile(…)`.
;
; An injected string's pieces between templates (`$table`, `${id}`) are one
; match, so Zed parses them as one text, the templates left out. Its
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
; as IntelliJ reads it: on the line before a property, an argument or an
; assignment, the string or `"…".trimIndent()`. The comment and the
; string's holder are a pattern's top level, not one alternation: a
; quantifier under an alternation stops at the first template.
([
  (line_comment)
  (multiline_comment)
] @_marker
  .
  (_
    (string_literal
      .
      [
        (string_content) @injection.content
        (interpolation_identifier_start)
        (interpolated_identifier)
        (interpolation_expression_start)
        (interpolated_expression)
        (interpolation_expression_end)
      ]+) @_literal)
  (#match? @_marker "^(//|/\\*)\\s*language\\s*=\\s*(?i:sql)\\b")
  (#not-match? @_literal "^\"+\\s*(?is:select\\s.+?\\sfrom\\b|with\\s+(recursive\\s+)?\\w+\\s+as\\s*\\(|insert\\s+into\\b|update\\s+\\S+\\s+set\\b|delete\\s+from\\b|merge\\s+into\\b)")
  (#set! injection.language "sql"))

([
  (line_comment)
  (multiline_comment)
] @_marker
  .
  (_
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
          ]+) @_literal)))
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

;; JSON and YAML

; A string a `// language=JSON` or `// language=YAML` comment (or its
; `/* … */` form) marks, as for SQL above.
([
  (line_comment)
  (multiline_comment)
] @_marker
  .
  (_
    (string_literal
      .
      [
        (string_content) @injection.content
        (interpolation_identifier_start)
        (interpolated_identifier)
        (interpolation_expression_start)
        (interpolated_expression)
        (interpolation_expression_end)
      ]+) @_literal)
  (#match? @_marker "^(//|/\\*)\\s*language\\s*=\\s*(?i:json)\\b")
  (#set! injection.language "json"))

([
  (line_comment)
  (multiline_comment)
] @_marker
  .
  (_
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
          ]+) @_literal)))
  (#match? @_marker "^(//|/\\*)\\s*language\\s*=\\s*(?i:json)\\b")
  (#set! injection.language "json"))

([
  (line_comment)
  (multiline_comment)
] @_marker
  .
  (_
    (string_literal
      .
      [
        (string_content) @injection.content
        (interpolation_identifier_start)
        (interpolated_identifier)
        (interpolation_expression_start)
        (interpolated_expression)
        (interpolation_expression_end)
      ]+) @_literal)
  (#match? @_marker "^(//|/\\*)\\s*language\\s*=\\s*(?i:yaml)\\b")
  (#set! injection.language "yaml"))

([
  (line_comment)
  (multiline_comment)
] @_marker
  .
  (_
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
          ]+) @_literal)))
  (#match? @_marker "^(//|/\\*)\\s*language\\s*=\\s*(?i:yaml)\\b")
  (#set! injection.language "yaml"))

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
