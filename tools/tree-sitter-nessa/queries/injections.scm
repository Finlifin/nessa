; Inject comment parsers (TODO/FIXME/URLs) into Nessa comments
([
  (doc_comment)
  (line_comment)
  (block_comment)
] @injection.content
  (#set! injection.language "comment"))

; Inject regex highlighting into Regex.new("...") / regex.compile("...")
(call_expression
  function: (member_expression
    object: (identifier) @_regex_mod
    member: (identifier) @_regex_fn)
  (argument
    (string_literal
      (string_content) @injection.content))
  (#match? @_regex_mod "^(Regex|regex)$")
  (#match? @_regex_fn "^(new|compile|is_match|find|find_all|captures|replace|replace_all|split)$")
  (#set! injection.language "regex"))
