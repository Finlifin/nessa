[
  (block)
  (declaration_body)
  (struct_body)
  (enum_body)
  (trait_body)
  (object_expression)
  (extended_application)
  (list_expression)
  (tuple_expression)
  (parameter_list)
  (when_expression)
  (match_expression)
  (effect_handling_expression)
  (error_handling_expression)
] @indent.begin

[
  "}"
  "]"
  ")"
] @indent.end @indent.branch

[
  "else"
] @indent.branch

[
  (format_directive)
  (doc_comment)
  (line_comment)
  (block_comment)
  (string_literal)
] @indent.auto
