(function_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.function

(impl_definition
  body: (declaration_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @name) @definition.method))

(extend_definition
  body: (declaration_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @name) @definition.method))

(struct_definition
  body: (struct_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @name) @definition.method))

(enum_definition
  body: (enum_body
    (function_definition
      name: [
        (identifier)
        (arbitrary_identifier)
      ] @name) @definition.method))

(trait_method_signature
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.method

(trait_default_method
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.method

(struct_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.class

(enum_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.class

(trait_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.interface

(effect_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.interface

(newtype_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.type

(typealias_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.type

(mod_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.module

(const_declaration
  pattern: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.constant

(global_definition
  name: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @definition.constant

(call_expression
  function: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @reference.call

(call_expression
  function: (member_expression
    member: [
      (identifier)
      (arbitrary_identifier)
    ] @name)) @reference.call

(infix_call_expression
  function: [
    (identifier)
    (arbitrary_identifier)
  ] @name) @reference.call
