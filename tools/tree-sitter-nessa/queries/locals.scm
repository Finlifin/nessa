; Lexical Scopes
[
  (source_file)
  (mod_definition)
  (function_definition)
  (trait_definition)
  (trait_default_method)
  (impl_definition)
  (extend_definition)
  (struct_definition)
  (enum_definition)
  (handles_statement)
  (block)
  (for_statement)
  (while_statement)
  (if_expression)
  (when_expression)
  (match_arm)
  (catch_arm)
  (case_arm)
  (lambda_expression)
  (reset_expression)
  (shift_expression)
] @local.scope

; Definitions
(function_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(effect_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(parameter
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(optional_parameter
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(varargs_parameter
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(catch_parameter
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(catch_arm
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(shift_expression
  continuation: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(let_statement
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(var_statement
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(let_handler
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(const_declaration
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(global_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(typed_pattern
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(as_pattern
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

(use_as_clause
  alias: [
    (identifier)
    (arbitrary_identifier)
  ] @local.definition)

; References
[
  (identifier)
  (arbitrary_identifier)
] @local.reference
