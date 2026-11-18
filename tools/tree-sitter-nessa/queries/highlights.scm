; Comments & Directives
(format_directive) @keyword.directive
(doc_comment) @comment.documentation
(line_comment) @comment
(block_comment) @comment

; Literals
(integer_literal) @number
(float_literal) @number.float
(boolean_literal) @boolean
(char_literal) @character
(string_literal) @string
(escape_sequence) @string.escape
(null_literal) @constant.builtin
(unit_literal) @constant.builtin
(self_literal) @variable.builtin
(self_type) @type.builtin
(wildcard) @variable.builtin
(macro_content) @embedded

(string_interpolation
  "{" @punctuation.special
  "}" @punctuation.special) @embedded

(symbol_literal
  "." @punctuation.special
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @string.special.symbol)

; Backtick identifiers (fallback before structural captures)
(arbitrary_identifier) @variable

; Visibility & Keywords
(visibility) @keyword.modifier

[
  "fn"
  "def"
  "struct"
  "enum"
  "trait"
  "assoc"
  "impl"
  "extend"
  "derive"
  "typealias"
  "newtype"
  "mod"
  "global"
  "const"
  "effect"
  "handles"
  "catch"
  "error"
  "reset"
  "shift"
  "case"
  "lambda"
] @keyword

[
  "async"
  "await"
] @keyword.coroutine

[
  "use"
] @keyword.import

[
  "let"
  "var"
] @keyword.storage

[
  "if"
  "else"
  "when"
  "match"
  "do"
] @keyword.conditional

[
  "for"
  "while"
  "in"
  "break"
  "continue"
] @keyword.repeat

[
  "return"
  "resume"
  "defer"
] @keyword.return

[
  "and"
  "or"
  "not"
  "matches"
  "is"
  "as"
] @keyword.operator

; Operators
[
  "="
  "+="
  "-="
  "*="
  "/="
  "%="
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "+"
  "-"
  "*"
  "/"
  "%"
  "++"
  "->"
  "=>"
  "|>"
  "|"
  ".."
  "..="
  "..."
  "?"
  "!"
  "#"
  "@"
  "'"
] @operator

; Punctuation
[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

[
  ","
  ";"
  ":"
  "."
] @punctuation.delimiter

; Module & Import Paths
(mod_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @module)

(package_use_path
  "@" @punctuation.special)

(use_as_clause
  alias: [
    (identifier)
    (arbitrary_identifier)
  ] @module)

; Type Declarations (PascalCase per docs/code-style.md)
(struct_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @type.definition)

(enum_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @type.definition)

(trait_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @type.definition)

(newtype_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @type.definition)

(typealias_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @type.definition)

(assoc_declaration
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @type.definition)

; Functions, Methods & Effects (snake_case per docs/code-style.md)
(function_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @function)

(impl_definition
  body: (declaration_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @function.method)))

(extend_definition
  body: (declaration_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @function.method)))

(struct_definition
  body: (struct_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @function.method)))

(enum_definition
  body: (enum_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @function.method)))

(trait_method_signature
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @function.method)

(trait_default_method
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @function.method)

(effect_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @function)

(handles_statement
  effect: (handles_target
    [
      (identifier)
      (arbitrary_identifier)
    ] @function))

; Constants & Globals
(const_declaration
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @constant)

(global_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable)

; Enum Variants: distinguish PascalCase Error variants vs snake_case value constructors
(enum_variant
  name: (identifier) @constructor
  (#match? @constructor "^[a-z_]"))

(enum_variant
  name: (arbitrary_identifier) @constructor)

(enum_variant
  name: (identifier) @constant
  (#match? @constant "^[A-Z]"))

; Fields, Properties & Parameters
(struct_field
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.member)

(property
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.member)

(property_pattern
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.member)

(named_argument
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.parameter)

(parameter
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.parameter)

(optional_parameter
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.parameter)

(varargs_parameter
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.parameter)

(catch_parameter
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.parameter)

(catch_arm
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.parameter)

(shift_expression
  continuation: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.parameter)

; Labels ('outer / :outer / break outer)
(loop_label
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @label)

(break_statement
  label: [
    (identifier)
    (arbitrary_identifier)
  ] @label)

(continue_statement
  label: [
    (identifier)
    (arbitrary_identifier)
  ] @label)

; Member Access, Views, Casts & Calls
(member_expression
  member: (tuple_index) @number)

(member_expression
  member: [
    (identifier)
    (arbitrary_identifier)
  ] @variable.member)

; Enum variant access on PascalCase type: Color.red (constructor) vs IoError.NotFound (error constant)
(member_expression
  object: (identifier) @_enum_type
  member: (identifier) @constructor
  (#match? @_enum_type "^[A-Z]")
  (#match? @constructor "^[a-z_]"))

(member_expression
  object: (identifier) @_error_type
  member: (identifier) @constant
  (#match? @_error_type "^[A-Z]")
  (#match? @constant "^[A-Z]"))

(member_pattern
  object: (identifier) @_enum_type
  member: (identifier) @constructor
  (#match? @_enum_type "^[A-Z]")
  (#match? @constructor "^[a-z_]"))

(member_pattern
  object: (identifier) @_error_type
  member: (identifier) @constant
  (#match? @_error_type "^[A-Z]")
  (#match? @constant "^[A-Z]"))

(call_expression
  function: [
    (identifier)
    (arbitrary_identifier)
  ] @function.call)

(call_expression
  function: (member_expression
    member: [
      (identifier)
      (arbitrary_identifier)
    ] @function.method.call))

; Constructor call on PascalCase type or Error variant
(call_expression
  function: (identifier) @type
  (#match? @type "^[A-Z]"))

(call_expression
  function: (member_expression
    object: (identifier) @_enum_type
    member: (identifier) @constructor)
  (#match? @_enum_type "^[A-Z]")
  (#match? @constructor "^[a-z_]"))

(call_expression
  function: (member_expression
    object: (identifier) @_error_type
    member: (identifier) @constant)
  (#match? @_error_type "^[A-Z]")
  (#match? @constant "^[A-Z]"))

(infix_call_expression
  function: [
    (identifier)
    (arbitrary_identifier)
  ] @function.call)

(cast_expression
  kind: [
    "as"
    "use"
  ] @keyword.operator)

(view_expression
  view: [
    (identifier)
    (arbitrary_identifier)
  ] @attribute)

(view_expression
  "'" @keyword.directive
  view: (identifier) @keyword.directive
  (#eq? @keyword.directive "builtin"))

; Builtin & PascalCase Types
((identifier) @type.builtin
  (#match? @type.builtin "^(bool|char|i8|i16|i32|i64|i128|isize|u8|u16|u32|u64|u128|usize|f32|f64|Any|Unit|NoReturn|Type|String|Bytes|List|Map|Set|Continuation)$"))

((identifier) @type
  (#match? @type "^[A-Z][a-zA-Z0-9_]*$"))
