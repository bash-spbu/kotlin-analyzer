; Scopes where config.toml's settings differ: a bracket pair with
; `not_in = ["string"]` or `["comment"]` doesn't close itself there.
(string_literal) @string
(character_literal) @string

[
  (line_comment)
  (multiline_comment)
] @comment.inclusive
