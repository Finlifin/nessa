/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  ASSIGN: 1,
  LOW_PREFIX: 5,
  RANGE: 10,
  OR: 20,
  AND: 30,
  COMPARE: 40,
  ARROW: 50,
  ADD: 60,
  MUL: 70,
  PIPE: 80,
  PREFIX: 90,
  POSTFIX: 100,
  MEMBER: 110,
};

function commaSep(rule) {
  return optional(commaSep1(rule));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

function semiSep($, rule) {
  return seq(
    optional($._real_semis),
    optional(seq(
      rule,
      repeat(seq(
        choice($._real_semis, seq($._virtual_semi, optional($._real_semis))),
        rule,
      )),
      optional($._real_semis),
    )),
  );
}

function commaOrSemiSep($, rule) {
  return seq(
    optional($._real_comma_semis),
    optional(seq(
      rule,
      repeat(seq(
        choice($._real_comma_semis, seq($._virtual_semi, optional($._real_comma_semis))),
        rule,
      )),
      optional($._real_comma_semis),
    )),
  );
}

module.exports = grammar({
  name: 'nessa',

  word: $ => $.identifier,

  extras: $ => [
    /\s/,
    $.format_directive,
    $.doc_comment,
    $.line_comment,
    $.block_comment,
  ],

  externals: $ => [
    $.block_comment,
    $._indent,
    $._outdent,
    $._virtual_semi,
    $._view_tick,
    $._label_tick,
    $.char_literal,
    $.macro_content,
    $.string_content,
    $._error_sentinel,
  ],

  supertypes: $ => [
    $._expression,
    $._pattern,
    $._definition,
    $._statement,
  ],

  conflicts: $ => [
    [$.block, $.object_expression],
    [$.parameter_type, $._primary_expression],
    [$.break_statement, $._primary_expression],
    [$.continue_statement, $._primary_expression],
  ],

  rules: {
    source_file: $ => semiSep($, $._file_item),

    _real_semis: _ => repeat1(';'),
    _real_comma_semis: _ => repeat1(choice(',', ';')),

    _file_item: $ => choice(
      $._definition,
      $._statement,
    ),

    // -------------------------------------------------------------------------
    // Comments & Directives
    // -------------------------------------------------------------------------
    format_directive: _ => token(prec(2, seq('--%', /[^\n]*/))),
    doc_comment: _ => token(prec(2, /---([^-\n][^\n]*)?/)),
    line_comment: _ => token(seq('--', /[^\n]*/)),

    // -------------------------------------------------------------------------
    // Identifiers & Literals
    // -------------------------------------------------------------------------
    identifier: _ => /[\p{Alphabetic}_][\p{Alphabetic}\p{Number}_]*/u,
    arbitrary_identifier: _ => token(seq('`', /[^`\n]+/, '`')),

    _ident: $ => choice(
      $.identifier,
      $.arbitrary_identifier,
    ),

    wildcard: _ => '_',
    self_literal: _ => 'self',
    self_type: _ => 'Self',
    boolean_literal: _ => choice('true', 'false'),
    null_literal: _ => 'null',
    unit_literal: _ => seq('(', ')'),

    integer_literal: _ => token(choice(
      /0[bB][01]+/,
      /0[oO][0-7]+/,
      /0[xX][0-9a-fA-F]+/,
      /[0-9]+/,
    )),

    float_literal: _ => token(seq(
      /[0-9]+/,
      '.',
      /[0-9]+/,
      optional(seq(/[eE]/, optional(/[+-]/), /[0-9]+/)),
    )),

    escape_sequence: _ => token.immediate(choice(
      '{{',
      '}}',
      seq('\\', choice(
        /[ntr0\\'"`]/,
        /x[0-9a-fA-F]{2}/,
        /u[0-9a-fA-F]{4}/,
        /u\{[0-9a-fA-F]{1,6}\}/,
      )),
    )),

    string_interpolation: $ => seq(
      alias(token.immediate('{'), '{'),
      $._expression,
      '}',
    ),

    string_literal: $ => seq(
      '"',
      repeat(choice(
        $.string_content,
        $.escape_sequence,
        $.string_interpolation,
      )),
      alias(token.immediate('"'), '"'),
    ),

    symbol_literal: $ => prec.right(PREC.MEMBER, seq(
      '.',
      field('name', $._ident),
    )),

    visibility: _ => choice('pub', 'private'),

    // -------------------------------------------------------------------------
    // Use Declarations & Import Paths
    // -------------------------------------------------------------------------
    use_declaration: $ => prec.right(seq(
      optional($.visibility),
      'use',
      field('path', $._use_path),
    )),

    _use_path: $ => choice(
      $._use_target,
      $.use_as_clause,
    ),

    _use_target: $ => choice(
      $._use_segment,
      $.super_use_path,
      $.package_use_path,
      $.scoped_use_path,
      $.use_list_path,
      $.use_wildcard_path,
    ),

    _use_segment: $ => choice(
      $._ident,
      $.self_literal,
    ),

    super_use_path: $ => prec.right(1, seq('.', $._use_target)),
    package_use_path: $ => prec.right(1, seq('@', $._use_target)),

    scoped_use_path: $ => prec.left(2, seq(
      field('path', $._use_target),
      '.',
      field('name', $._ident),
    )),

    use_list_path: $ => prec.left(2, seq(
      field('path', $._use_target),
      '.',
      '{',
      commaSep($._use_path),
      '}',
    )),

    use_wildcard_path: $ => prec.left(2, seq(
      field('path', $._use_target),
      '.',
      '*',
    )),

    use_as_clause: $ => prec(2, seq(
      field('path', $._use_target),
      'as',
      field('alias', $._ident),
    )),

    // -------------------------------------------------------------------------
    // Definitions
    // -------------------------------------------------------------------------
    _definition: $ => choice(
      $.function_definition,
      $.effect_definition,
      $.struct_definition,
      $.enum_definition,
      $.trait_definition,
      $.assoc_declaration,
      $.impl_definition,
      $.extend_definition,
      $.derive_definition,
      $.typealias_definition,
      $.newtype_definition,
      $.mod_definition,
      $.global_definition,
      $.const_declaration,
      $.use_declaration,
    ),

    function_definition: $ => prec.right(seq(
      optional($.visibility),
      'fn',
      field('name', $._ident),
      field('parameters', $.parameter_list),
      optional(seq('->', field('return_type', $._expr_no_ext))),
      optional(field('capability', $.handles_clause)),
      choice(
        seq('=', field('value', $._expression)),
        field('body', $._block),
      ),
    )),

    handles_clause: $ => seq(
      'handles',
      field('effect', $._expr_no_ext),
    ),

    parameter_list: $ => seq(
      '(',
      commaSep($._parameter),
      ')',
    ),

    _parameter: $ => choice(
      $.self_parameter,
      $.catch_parameter,
      $.varargs_parameter,
      $.optional_parameter,
      $.lambda_parameter_decl,
      $.parameter,
    ),

    self_parameter: $ => $.self_literal,

    catch_parameter: $ => seq(
      'catch',
      field('name', $._ident),
      optional(seq(':', field('type', $._expr_no_ext))),
    ),

    varargs_parameter: $ => prec(2, seq(
      '...',
      field('name', $._ident),
      optional(seq(':', field('type', $._expr_no_ext))),
    )),

    optional_parameter: $ => prec(PREC.MEMBER + 10, seq(
      '.',
      field('name', $._ident),
      optional(seq(':', field('type', $._expr_no_ext))),
      '=',
      field('default', $._expression),
    )),

    lambda_parameter_decl: $ => seq(
      'lambda',
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._expr_no_ext))),
    ),

    parameter: $ => seq(
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._expr_no_ext))),
    ),

    effect_definition: $ => prec.right(seq(
      optional($.visibility),
      optional('async'),
      'effect',
      field('name', $._ident),
      field('parameters', $.parameter_list),
      optional(seq('->', field('return_type', $._expr_no_ext))),
    )),

    struct_definition: $ => seq(
      optional($.visibility),
      'struct',
      field('name', $._ident),
      field('body', $.struct_body),
    ),

    struct_body: $ => choice(
      seq('{', commaOrSemiSep($, choice($.struct_field, $._definition, $._statement)), '}'),
      seq(optional(':'), $._indent, commaOrSemiSep($, choice($.struct_field, $._definition, $._statement)), $._outdent),
    ),

    struct_field: $ => prec(2, seq(
      optional($.visibility),
      field('name', $._ident),
      ':',
      field('type', $._expr_no_ext),
      optional(seq('=', field('default', $._expression))),
    )),

    enum_definition: $ => seq(
      optional($.visibility),
      'enum',
      field('name', $._ident),
      field('body', $.enum_body),
    ),

    enum_body: $ => choice(
      seq('{', commaOrSemiSep($, choice($.enum_variant, $._definition, $._statement)), '}'),
      seq(optional(':'), $._indent, commaOrSemiSep($, choice($.enum_variant, $._definition, $._statement)), $._outdent),
    ),

    enum_variant: $ => prec(2, seq(
      optional($.visibility),
      field('name', $._ident),
      optional(field('parameters', $.parameter_list)),
    )),

    trait_definition: $ => seq(
      optional($.visibility),
      'trait',
      field('name', $._ident),
      optional(field('bounds', $.trait_bounds)),
      field('body', $.trait_body),
    ),

    trait_bounds: $ => seq(
      '(',
      commaSep1($._expression),
      ')',
    ),

    trait_body: $ => choice(
      seq('{', semiSep($, $._trait_member), '}'),
      seq(optional(':'), $._indent, semiSep($, $._trait_member), $._outdent),
    ),

    _trait_member: $ => choice(
      $.trait_default_method,
      $.trait_method_signature,
      $._definition,
      $._statement,
    ),

    trait_method_signature: $ => prec(1, seq(
      optional($.visibility),
      optional('def'),
      'fn',
      field('name', $._ident),
      field('parameters', $.parameter_list),
      optional(seq('->', field('return_type', $._expr_no_ext))),
      optional(field('capability', $.handles_clause)),
    )),

    trait_default_method: $ => prec.right(2, seq(
      optional($.visibility),
      'derive',
      'fn',
      field('name', $._ident),
      field('parameters', $.parameter_list),
      optional(seq('->', field('return_type', $._expr_no_ext))),
      optional(field('capability', $.handles_clause)),
      choice(
        seq('=', field('value', $._expression)),
        field('body', $._block),
      ),
    )),

    assoc_declaration: $ => prec.right(choice(
      seq(
        optional($.visibility),
        'assoc',
        field('name', $._ident),
        choice(
          seq(':', field('type', $._expr_no_ext), optional(seq('=', field('value', $._expression)))),
          seq('=', field('value', $._expression)),
        ),
      ),
      seq(
        optional($.visibility),
        'assoc',
        field('definition', $._definition),
      ),
    )),

    impl_definition: $ => seq(
      optional($.visibility),
      'impl',
      field('subject', $._expr_no_ext),
      optional(seq('for', field('target', $._expr_no_ext))),
      field('body', $.declaration_body),
    ),

    extend_definition: $ => seq(
      optional($.visibility),
      'extend',
      field('subject', $._expr_no_ext),
      optional(seq('for', field('target', $._expr_no_ext))),
      field('body', $.declaration_body),
    ),

    derive_definition: $ => prec.right(seq(
      optional($.visibility),
      'derive',
      commaSep1(field('trait', $._expr_no_ext)),
      'for',
      field('target', $._expr_no_ext),
    )),

    typealias_definition: $ => prec.right(seq(
      optional($.visibility),
      'typealias',
      field('name', $._ident),
      '=',
      field('type', $._expression),
    )),

    newtype_definition: $ => prec.right(seq(
      optional($.visibility),
      'newtype',
      field('name', $._ident),
      '=',
      field('type', $._expression),
    )),

    mod_definition: $ => prec.right(seq(
      optional($.visibility),
      'mod',
      field('name', $._ident),
      optional(field('body', $.declaration_body)),
    )),

    global_definition: $ => prec.right(seq(
      optional($.visibility),
      'global',
      field('name', $._ident),
      ':',
      field('type', $._expr_no_ext),
      '=',
      field('value', $._expression),
    )),

    const_declaration: $ => prec.right(seq(
      optional($.visibility),
      'const',
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._expr_no_ext))),
      '=',
      field('value', $._expression),
      optional(seq('else', field('else_branch', $._block_or_statement))),
    )),

    declaration_body: $ => choice(
      seq('{', semiSep($, $._file_item), '}'),
      seq(optional(':'), $._indent, semiSep($, $._file_item), $._outdent),
    ),

    // -------------------------------------------------------------------------
    // Statements & Blocks
    // -------------------------------------------------------------------------
    block: $ => prec.dynamic(5, seq(
      '{',
      semiSep($, $._file_item),
      '}',
    )),

    colon_block: $ => seq(
      ':',
      $._indent,
      semiSep($, $._file_item),
      $._outdent,
    ),

    indented_block: $ => seq(
      $._indent,
      semiSep($, $._file_item),
      $._outdent,
    ),

    _block: $ => choice(
      $.block,
      alias($.colon_block, $.block),
    ),

    _block_or_statement: $ => choice(
      alias($.colon_block, $.block),
      $._definition,
      $._statement,
    ),

    _arm_body: $ => choice(
      alias($.indented_block, $.block),
      alias($.colon_block, $.block),
      $._definition,
      $._statement,
    ),

    _statement: $ => choice(
      $.let_statement,
      $.var_statement,
      $.let_handler,
      $.handles_statement,
      $.assignment_statement,
      $.defer_statement,
      $.return_statement,
      $.resume_statement,
      $.break_statement,
      $.continue_statement,
      $.for_statement,
      $.while_statement,
      prec.dynamic(5, $.block),
      $.expression_statement,
    ),

    let_statement: $ => prec.right(seq(
      optional($.visibility),
      'let',
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._expr_no_ext))),
      '=',
      field('value', $._expression),
      optional(seq('else', field('else_branch', $._block_or_statement))),
    )),

    var_statement: $ => prec.right(seq(
      optional($.visibility),
      'var',
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._expr_no_ext))),
      '=',
      field('value', $._expression),
      optional(seq('else', field('else_branch', $._block_or_statement))),
    )),

    let_handler: $ => prec.right(seq(
      optional($.visibility),
      repeat1(seq('handles', '(', field('effect', $._expression), ')')),
      'let',
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._expr_no_ext))),
      '=',
      field('value', $._expression),
    )),

    handles_statement: $ => prec.right(choice(
      seq(
        optional($.visibility),
        'handles',
        '(',
        field('effect', $._expression),
        ')',
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $._expr_no_ext))),
        choice(
          seq('=', field('value', $._expression)),
          field('body', $._block),
        ),
      ),
      seq(
        optional($.visibility),
        'handles',
        optional('async'),
        field('effect', $.handles_target),
        field('parameters', $.parameter_list),
        optional(seq('->', field('return_type', $._expr_no_ext))),
        '=>',
        field('body', $._arm_body),
      ),
    )),

    handles_target: $ => prec.left(seq(
      $._ident,
      repeat(seq('.', $._ident)),
    )),

    assignment_statement: $ => prec.right(PREC.ASSIGN, seq(
      field('left', $._expression),
      field('operator', choice('=', '+=', '-=', '*=', '/=', '%=')),
      field('right', $._expression),
    )),

    defer_statement: $ => prec.right(seq(
      'defer',
      field('body', $._block_or_statement),
    )),

    return_statement: $ => prec.right(seq(
      'return',
      optional(field('value', $._expression)),
      optional(seq('if', field('condition', $._expr_no_ext))),
    )),

    resume_statement: $ => prec.right(seq(
      'resume',
      optional(field('value', $._expression)),
      optional(seq('if', field('condition', $._expr_no_ext))),
    )),

    loop_label: $ => choice(
      seq(':', field('name', $._ident)),
      seq(alias($._label_tick, '\''), field('name', $._ident)),
    ),

    tick_label_prefix: $ => seq(
      alias($._label_tick, '\''),
      field('name', $._ident),
    ),

    break_statement: $ => prec.right(seq(
      'break',
      optional(choice(
        seq(field('label', $.loop_label), optional(field('value', $._expr_no_ext))),
        prec(1, field('label', $._ident)),
        field('value', $._expr_no_ext),
      )),
      optional(seq('if', field('condition', $._expr_no_ext))),
    )),

    continue_statement: $ => prec.right(seq(
      'continue',
      optional(choice(
        field('label', $.loop_label),
        prec(1, field('label', $._ident)),
        field('value', $._expr_no_ext),
      )),
      optional(seq('if', field('condition', $._expr_no_ext))),
    )),

    for_statement: $ => prec.right(seq(
      optional(seq(alias($.tick_label_prefix, $.loop_label), ':')),
      'for',
      optional(field('label', $.loop_label)),
      field('pattern', $._refutable_pattern_no_ext),
      'in',
      field('iterator', $._expr_no_ext),
      field('body', $._block),
    )),

    while_statement: $ => prec.right(seq(
      optional(seq(alias($.tick_label_prefix, $.loop_label), ':')),
      'while',
      optional(field('label', $.loop_label)),
      field('condition', $._expr_no_ext),
      field('body', $._block),
    )),

    expression_statement: $ => $._expression,

    // -------------------------------------------------------------------------
    // Arms (match, when, effect/error elimination)
    // -------------------------------------------------------------------------
    match_arm: $ => prec.right(seq(
      field('pattern', $._refutable_pattern),
      '=>',
      field('body', $._arm_body),
    )),

    catch_arm: $ => prec.right(seq(
      'catch',
      field('name', $._ident),
      '=>',
      field('body', $._arm_body),
    )),

    condition_arm: $ => prec.right(seq(
      field('condition', $._expr_no_ext),
      '=>',
      field('body', $._arm_body),
    )),

    else_arm: $ => prec.right(seq(
      'else',
      '=>',
      field('body', $._arm_body),
    )),

    // -------------------------------------------------------------------------
    // Expressions (full and no-extended-call variants)
    // -------------------------------------------------------------------------
    _expression: $ => choice(
      $._primary_expression,
      $.unary_expression,
      $.binary_expression,
      $.infix_call_expression,
      $.range_expression,
      $.postfix_expression,
      $.member_expression,
      $.await_expression,
      $.view_expression,
      $.cast_expression,
      $.call_expression,
      $.extended_application,
      $.matches_expression,
      $.match_expression,
      $.post_do_expression,
      $.effect_handling_expression,
      $.error_handling_expression,
      $.qualified_type,
      $.function_type,
      $.effect_type,
      $.lambda_expression,
      $.case_map_expression,
      $.reset_expression,
      $.shift_expression,
    ),

    _expr_no_ext: $ => choice(
      $._primary_expression,
      alias($.unary_expression_no_ext, $.unary_expression),
      alias($.binary_expression_no_ext, $.binary_expression),
      alias($.infix_call_expression_no_ext, $.infix_call_expression),
      alias($.range_expression_no_ext, $.range_expression),
      alias($.postfix_expression_no_ext, $.postfix_expression),
      alias($.member_expression_no_ext, $.member_expression),
      alias($.await_expression_no_ext, $.await_expression),
      alias($.view_expression_no_ext, $.view_expression),
      alias($.cast_expression_no_ext, $.cast_expression),
      alias($.call_expression_no_ext, $.call_expression),
      alias($.matches_expression_no_ext, $.matches_expression),
      alias($.qualified_type_no_ext, $.qualified_type),
      $.function_type,
      $.effect_type,
    ),

    _primary_expression: $ => choice(
      $._ident,
      $.self_literal,
      $.self_type,
      $.wildcard,
      $.integer_literal,
      $.float_literal,
      $.char_literal,
      $.string_literal,
      $.boolean_literal,
      $.null_literal,
      $.unit_literal,
      $.range_full,
      $.symbol_literal,
      $.macro_content,
      $.parenthesized_expression,
      $.tuple_expression,
      $.list_expression,
      $.object_expression,
      $.if_expression,
      $.when_expression,
    ),

    parenthesized_expression: $ => seq('(', $._expression, ')'),

    tuple_expression: $ => seq(
      '(',
      $._expression,
      ',',
      commaSep($._expression),
      ')',
    ),

    list_expression: $ => seq(
      '[',
      commaSep(choice($.spread_argument, $._expression)),
      ']',
    ),

    object_expression: $ => prec.dynamic(-5, seq(
      '{',
      commaSep(choice($.property, $.spread_argument, $._object_element)),
      '}',
    )),

    _object_element: $ => prec(-1, $._expression),

    property: $ => prec(1, seq(
      field('name', $._ident),
      ':',
      field('value', $._expression),
    )),

    if_expression: $ => prec.right(seq(
      'if',
      field('condition', $._expr_no_ext),
      field('consequence', $._block),
      optional(seq('else', field('alternative', choice($.if_expression, $._block)))),
    )),

    when_expression: $ => seq(
      'when',
      choice(
        seq('{', commaOrSemiSep($, choice($.else_arm, $.condition_arm)), '}'),
        seq(optional(':'), $._indent, commaOrSemiSep($, choice($.else_arm, $.condition_arm)), $._outdent),
      ),
    ),

    lambda_expression: $ => prec.right(PREC.ASSIGN, seq(
      '|',
      commaSep($._parameter),
      '|',
      choice(
        seq('->', field('return_type', $._expr_no_ext), field('body', $._block)),
        field('body', $._arm_body),
      ),
    )),

    case_map_expression: $ => prec.right(PREC.ASSIGN, seq(
      $.case_arm,
      repeat(seq('|', $.case_arm)),
    )),

    case_arm: $ => prec.right(seq(
      'case',
      field('pattern', $._refutable_pattern),
      '=>',
      field('body', $._arm_body),
    )),

    reset_expression: $ => prec.right(PREC.ASSIGN, seq(
      'reset',
      field('prompt', $._expr_no_ext),
      field('body', $._block),
    )),

    shift_expression: $ => prec.right(PREC.ASSIGN, seq(
      'shift',
      field('prompt', $._expr_no_ext),
      ',',
      field('continuation', $._ident),
      field('body', $._block),
    )),

    // Prefix & Type Expressions
    unary_expression: $ => choice(
      prec.right(PREC.PREFIX, seq(
        field('operator', choice('-', '?')),
        field('operand', $._expression),
      )),
      prec.right(PREC.LOW_PREFIX, seq(
        field('operator', choice('not', 'error')),
        field('operand', $._expression),
      )),
    ),

    unary_expression_no_ext: $ => choice(
      prec.right(PREC.PREFIX, seq(
        field('operator', choice('-', '?')),
        field('operand', $._expr_no_ext),
      )),
      prec.right(PREC.LOW_PREFIX, seq(
        field('operator', choice('not', 'error')),
        field('operand', $._expr_no_ext),
      )),
    ),

    _qualifier_atom: $ => choice(
      $._ident,
      $.self_literal,
      $.self_type,
      $.symbol_literal,
      $.parenthesized_expression,
      $.list_expression,
      alias($.qualifier_member_expression, $.member_expression),
      alias($.qualifier_view_expression, $.view_expression),
      alias($.qualifier_call_expression, $.call_expression),
    ),

    qualifier_member_expression: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._qualifier_atom),
      '.',
      field('member', $._ident),
    )),

    qualifier_view_expression: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._qualifier_atom),
      alias($._view_tick, '\''),
      field('view', $._ident),
    )),

    qualifier_call_expression: $ => prec.left(PREC.POSTFIX, seq(
      field('function', $._qualifier_atom),
      token.immediate('('),
      commaSep($.argument),
      ')',
    )),

    _qualifier_set: $ => choice(
      $._qualifier_atom,
      alias($.qualifier_concat_expression, $.binary_expression),
    ),

    qualifier_concat_expression: $ => prec.left(PREC.ADD, seq(
      field('left', $._qualifier_set),
      field('operator', '++'),
      field('right', $._qualifier_atom),
    )),

    qualified_type: $ => prec.right(PREC.PREFIX, seq(
      field('qualifier_kind', choice('!', '#')),
      field('qualifier', $._qualifier_set),
      field('type', $._expression),
    )),

    qualified_type_no_ext: $ => prec.right(PREC.PREFIX, seq(
      field('qualifier_kind', choice('!', '#')),
      field('qualifier', $._qualifier_set),
      field('type', $._expr_no_ext),
    )),

    function_type: $ => prec.right(PREC.PREFIX, seq(
      'fn',
      '(',
      commaSep($.parameter_type),
      ')',
      optional(seq('->', field('return_type', $._expr_no_ext))),
    )),

    effect_type: $ => prec.right(PREC.PREFIX, seq(
      optional('async'),
      'effect',
      '(',
      commaSep($.parameter_type),
      ')',
      optional(seq('->', field('return_type', $._expr_no_ext))),
    )),

    parameter_type: $ => choice(
      $.self_literal,
      seq('catch', field('name', $._ident), optional(seq(':', field('type', $._expr_no_ext)))),
      seq('...', field('name', $._ident), optional(seq(':', field('type', $._expr_no_ext)))),
      prec.dynamic(2, seq('.', field('name', $._ident), ':', field('type', $._expr_no_ext), optional(seq('=', field('default', $._expr_no_ext))))),
      prec.dynamic(2, seq(field('name', $._ident), ':', field('type', $._expr_no_ext))),
      $._expr_no_ext,
    ),

    // Range Expressions
    range_full: _ => prec(-1, '..'),

    _range_bound_no_ext: $ => prec(PREC.RANGE + 1, $._expr_no_ext),

    range_expression: $ => choice(
      prec.right(PREC.RANGE, seq(
        field('start', $._expression),
        field('operator', choice('..', '..=')),
        optional(field('end', $._expression)),
      )),
      prec.right(PREC.RANGE, seq(
        field('operator', choice('..', '..=')),
        field('end', $._expression),
      )),
    ),

    range_expression_no_ext: $ => choice(
      prec.right(PREC.RANGE, seq(
        field('start', $._expr_no_ext),
        field('operator', choice('..', '..=')),
        optional(field('end', $._range_bound_no_ext)),
      )),
      prec.right(PREC.RANGE, seq(
        field('operator', choice('..', '..=')),
        field('end', $._range_bound_no_ext),
      )),
    ),

    // Binary & Infix Expressions
    binary_expression: $ => choice(
      prec.left(PREC.OR, seq(field('left', $._expression), field('operator', 'or'), field('right', $._expression))),
      prec.left(PREC.AND, seq(field('left', $._expression), field('operator', 'and'), field('right', $._expression))),
      prec.left(PREC.COMPARE, seq(
        field('left', $._expression),
        field('operator', choice('==', '!=', '<', '<=', '>', '>=')),
        field('right', $._expression),
      )),
      prec.right(PREC.ARROW, seq(field('left', $._expression), field('operator', '->'), field('right', $._expression))),
      prec.left(PREC.ADD, seq(
        field('left', $._expression),
        field('operator', choice('+', '-', '++')),
        field('right', $._expression),
      )),
      prec.left(PREC.MUL, seq(
        field('left', $._expression),
        field('operator', choice('*', '/', '%')),
        field('right', $._expression),
      )),
      prec.left(PREC.PIPE, seq(
        field('left', $._expression),
        field('operator', '|>'),
        field('right', $._expression),
      )),
    ),

    binary_expression_no_ext: $ => choice(
      prec.left(PREC.OR, seq(field('left', $._expr_no_ext), field('operator', 'or'), field('right', $._expr_no_ext))),
      prec.left(PREC.AND, seq(field('left', $._expr_no_ext), field('operator', 'and'), field('right', $._expr_no_ext))),
      prec.left(PREC.COMPARE, seq(
        field('left', $._expr_no_ext),
        field('operator', choice('==', '!=', '<', '<=', '>', '>=')),
        field('right', $._expr_no_ext),
      )),
      prec.right(PREC.ARROW, seq(field('left', $._expr_no_ext), field('operator', '->'), field('right', $._expr_no_ext))),
      prec.left(PREC.ADD, seq(
        field('left', $._expr_no_ext),
        field('operator', choice('+', '-', '++')),
        field('right', $._expr_no_ext),
      )),
      prec.left(PREC.MUL, seq(
        field('left', $._expr_no_ext),
        field('operator', choice('*', '/', '%')),
        field('right', $._expr_no_ext),
      )),
      prec.left(PREC.PIPE, seq(
        field('left', $._expr_no_ext),
        field('operator', '|>'),
        field('right', $._expr_no_ext),
      )),
    ),

    infix_call_expression: $ => prec.left(PREC.MUL, seq(
      field('left', $._expression),
      field('function', $._ident),
      field('right', $._expression),
    )),

    infix_call_expression_no_ext: $ => prec.left(PREC.MUL, seq(
      field('left', $._expr_no_ext),
      field('function', $._ident),
      field('right', $._expr_no_ext),
    )),

    // Postfix & Member Expressions
    member_expression: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._expression),
      '.',
      field('member', choice($._ident, alias($.integer_literal, $.tuple_index))),
    )),

    member_expression_no_ext: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._expr_no_ext),
      '.',
      field('member', choice($._ident, alias($.integer_literal, $.tuple_index))),
    )),

    await_expression: $ => prec.left(PREC.MEMBER, seq(
      field('value', $._expression),
      '.',
      'await',
    )),

    await_expression_no_ext: $ => prec.left(PREC.MEMBER, seq(
      field('value', $._expr_no_ext),
      '.',
      'await',
    )),

    view_expression: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._expression),
      alias($._view_tick, '\''),
      field('view', $._ident),
    )),

    view_expression_no_ext: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._expr_no_ext),
      alias($._view_tick, '\''),
      field('view', $._ident),
    )),

    cast_expression: $ => prec.left(PREC.MEMBER, seq(
      field('value', $._expression),
      '.',
      field('kind', choice('as', 'use')),
      '(',
      field('target', $._expression),
      ')',
    )),

    cast_expression_no_ext: $ => prec.left(PREC.MEMBER, seq(
      field('value', $._expr_no_ext),
      '.',
      field('kind', choice('as', 'use')),
      '(',
      field('target', $._expression),
      ')',
    )),

    call_expression: $ => prec.left(PREC.POSTFIX, seq(
      field('function', $._expression),
      '(',
      commaSep($.argument),
      ')',
    )),

    call_expression_no_ext: $ => prec.left(PREC.POSTFIX, seq(
      field('function', $._expr_no_ext),
      '(',
      commaSep($.argument),
      ')',
    )),

    extended_application: $ => prec.left(PREC.POSTFIX, seq(
      field('function', $._expression),
      '{',
      commaSep(choice($.property, $.spread_argument, $._expression)),
      '}',
    )),

    postfix_expression: $ => prec.left(PREC.POSTFIX, seq(
      field('operand', $._expression),
      field('operator', choice('?', '!', '#')),
    )),

    postfix_expression_no_ext: $ => prec.left(PREC.POSTFIX, seq(
      field('operand', $._expr_no_ext),
      field('operator', choice('?', '!', '#')),
    )),

    matches_expression: $ => prec.left(PREC.COMPARE, seq(
      field('value', $._expression),
      'matches',
      field('pattern', $._refutable_pattern_no_ext),
    )),

    matches_expression_no_ext: $ => prec.left(PREC.COMPARE, seq(
      field('value', $._expr_no_ext),
      'matches',
      field('pattern', $._refutable_pattern_no_ext),
    )),

    match_expression: $ => prec.left(PREC.POSTFIX, seq(
      field('value', $._expression),
      'match',
      choice(
        seq('{', commaOrSemiSep($, $.match_arm), '}'),
        seq(optional(':'), $._indent, commaOrSemiSep($, $.match_arm), $._outdent),
      ),
    )),

    post_do_expression: $ => prec.left(PREC.POSTFIX, choice(
      seq(
        field('receiver', $._expression),
        'do',
        field('handler', choice($.lambda_expression, $.block, alias($.indented_block, $.block))),
      ),
      seq(
        field('receiver', $.call_expression),
        field('handler', $.lambda_expression),
      ),
    )),

    effect_handling_expression: $ => prec.left(PREC.POSTFIX + 1, seq(
      field('value', $._expression),
      '#',
      '{',
      commaOrSemiSep($, $.match_arm),
      '}',
    )),

    error_handling_expression: $ => prec.left(PREC.POSTFIX + 1, seq(
      field('value', $._expression),
      '!',
      '{',
      commaOrSemiSep($, choice($.catch_arm, $.match_arm)),
      '}',
    )),

    argument: $ => choice(
      $.named_argument,
      $.spread_argument,
      $._expression,
    ),

    named_argument: $ => prec(2, seq(
      optional('.'),
      field('name', $._ident),
      '=',
      field('value', $._expression),
    )),

    spread_argument: $ => prec.right(1, seq(
      '...',
      field('value', $._expression),
    )),

    // -------------------------------------------------------------------------
    // Patterns (irrefutable top-level and refutable variants)
    // -------------------------------------------------------------------------
    _pattern: $ => choice(
      $._primary_pattern,
      $.prefix_pattern,
      $.or_pattern,
      $.and_is_pattern,
      $.range_pattern,
      $.postfix_pattern,
      $.application_pattern,
      $.extended_pattern,
      $.member_pattern,
      $.as_pattern,
    ),

    _refutable_pattern: $ => choice(
      $._primary_pattern,
      $.typed_pattern,
      alias($.refutable_prefix_pattern, $.prefix_pattern),
      alias($.refutable_or_pattern, $.or_pattern),
      alias($.refutable_and_is_pattern, $.and_is_pattern),
      alias($.refutable_range_pattern, $.range_pattern),
      alias($.refutable_postfix_pattern, $.postfix_pattern),
      alias($.refutable_application_pattern, $.application_pattern),
      alias($.refutable_extended_pattern, $.extended_pattern),
      alias($.refutable_member_pattern, $.member_pattern),
      alias($.refutable_as_pattern, $.as_pattern),
      $.guard_pattern,
    ),

    _refutable_pattern_no_ext: $ => choice(
      $._primary_pattern,
      $.typed_pattern,
      alias($.refutable_prefix_pattern_no_ext, $.prefix_pattern),
      alias($.refutable_or_pattern_no_ext, $.or_pattern),
      alias($.refutable_and_is_pattern_no_ext, $.and_is_pattern),
      alias($.refutable_range_pattern_no_ext, $.range_pattern),
      alias($.refutable_postfix_pattern_no_ext, $.postfix_pattern),
      alias($.refutable_application_pattern_no_ext, $.application_pattern),
      alias($.refutable_member_pattern_no_ext, $.member_pattern),
      alias($.refutable_as_pattern_no_ext, $.as_pattern),
      alias($.guard_pattern_no_ext, $.guard_pattern),
    ),

    _primary_pattern: $ => choice(
      $._ident,
      $.wildcard,
      $.self_type,
      $.unit_literal,
      $.range_full,
      $.null_literal,
      $.boolean_literal,
      $.char_literal,
      $.string_literal,
      $.integer_literal,
      $.float_literal,
      $.negative_literal_pattern,
      $.symbol_literal,
      $.parenthesized_pattern,
      $.tuple_pattern,
      $.list_pattern,
      $.rest_pattern,
      $.record_pattern,
    ),

    negative_literal_pattern: $ => seq(
      '-',
      choice($.integer_literal, $.float_literal),
    ),

    parenthesized_pattern: $ => seq('(', $._refutable_pattern, ')'),

    tuple_pattern: $ => seq(
      '(',
      $._refutable_pattern,
      ',',
      commaSep($._refutable_pattern),
      ')',
    ),

    list_pattern: $ => seq(
      '[',
      commaSep($._refutable_pattern),
      ']',
    ),

    rest_pattern: $ => prec.right(seq(
      '...',
      optional(choice($._ident, $.wildcard)),
    )),

    record_pattern: $ => seq(
      '{',
      commaOrSemiSep($, choice($.property_pattern, $.rest_pattern, $._ident)),
      '}',
    ),

    property_pattern: $ => prec(1, seq(
      field('name', $._ident),
      ':',
      field('pattern', $._refutable_pattern),
    )),

    // Pattern type expression (prec >= 50: excludes `or`, `and`, comparisons, `matches`, and `{...}`)
    _pattern_type: $ => choice(
      $._primary_expression,
      alias($.pattern_type_unary, $.unary_expression),
      alias($.pattern_type_arrow, $.binary_expression),
      alias($.pattern_type_member, $.member_expression),
      alias($.pattern_type_view, $.view_expression),
      alias($.pattern_type_call, $.call_expression),
      alias($.qualified_type_pattern, $.qualified_type),
      $.function_type,
      $.effect_type,
    ),

    pattern_type_member: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._pattern_type),
      '.',
      field('member', choice($._ident, alias($.integer_literal, $.tuple_index))),
    )),

    pattern_type_view: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._pattern_type),
      alias($._view_tick, '\''),
      field('view', $._ident),
    )),

    pattern_type_call: $ => prec.left(PREC.POSTFIX, seq(
      field('function', $._pattern_type),
      '(',
      commaSep($.argument),
      ')',
    )),

    pattern_type_unary: $ => prec.right(PREC.PREFIX, seq(
      field('operator', '?'),
      field('operand', $._pattern_type),
    )),

    pattern_type_arrow: $ => prec.right(PREC.ARROW, seq(
      field('left', $._pattern_type),
      field('operator', '->'),
      field('right', $._pattern_type),
    )),

    qualified_type_pattern: $ => prec.right(PREC.PREFIX, seq(
      field('qualifier_kind', choice('!', '#')),
      field('qualifier', $._qualifier_set),
      field('type', $._pattern_type),
    )),

    typed_pattern: $ => prec(92, seq(
      field('pattern', choice($._ident, $.wildcard)),
      ':',
      field('type', $._pattern_type),
    )),

    // Irrefutable pattern combinators
    prefix_pattern: $ => choice(
      prec.right(95, seq(field('operator', 'not'), field('pattern', $._pattern))),
      prec.right(PREC.PREFIX, seq(field('operator', choice('error', '!', '#', 'async')), field('pattern', $._pattern))),
    ),

    or_pattern: $ => prec.left(PREC.OR, seq(
      field('left', $._pattern),
      'or',
      field('right', $._pattern),
    )),

    and_is_pattern: $ => prec.left(PREC.AND, seq(
      field('pattern', $._pattern),
      'and',
      field('value', $._expr_no_ext),
      'is',
      field('subpattern', $._pattern),
    )),

    range_pattern: $ => choice(
      prec.left(PREC.RANGE, seq($._pattern, choice('..', '..='), $._pattern)),
      prec.right(PREC.PREFIX, seq(choice('..', '..='), $._pattern)),
    ),

    postfix_pattern: $ => prec.left(PREC.POSTFIX, seq(
      field('pattern', $._pattern),
      field('operator', choice('?', '!')),
    )),

    member_pattern: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._pattern),
      '.',
      field('member', choice($._ident, '*')),
    )),

    application_pattern: $ => prec.left(PREC.POSTFIX, seq(
      field('constructor', $._pattern),
      '(',
      commaSep($._refutable_pattern),
      ')',
    )),

    extended_pattern: $ => prec.left(PREC.POSTFIX, seq(
      field('constructor', $._pattern),
      '{',
      commaOrSemiSep($, choice($.property_pattern, $.rest_pattern, $._ident)),
      '}',
    )),

    as_pattern: $ => prec.left(PREC.PREFIX - 5, seq(
      field('pattern', $._pattern),
      'as',
      field('name', $._ident),
    )),

    // Refutable pattern combinators
    refutable_prefix_pattern: $ => choice(
      prec.right(95, seq(field('operator', 'not'), field('pattern', $._refutable_pattern))),
      prec.right(PREC.PREFIX, seq(field('operator', choice('error', '!', '#', 'async')), field('pattern', $._refutable_pattern))),
    ),

    refutable_prefix_pattern_no_ext: $ => choice(
      prec.right(95, seq(field('operator', 'not'), field('pattern', $._refutable_pattern_no_ext))),
      prec.right(PREC.PREFIX, seq(field('operator', choice('error', '!', '#', 'async')), field('pattern', $._refutable_pattern_no_ext))),
    ),

    refutable_or_pattern: $ => prec.left(PREC.OR, seq(
      field('left', $._refutable_pattern),
      'or',
      field('right', $._refutable_pattern),
    )),

    refutable_or_pattern_no_ext: $ => prec.left(PREC.OR, seq(
      field('left', $._refutable_pattern_no_ext),
      'or',
      field('right', $._refutable_pattern_no_ext),
    )),

    refutable_and_is_pattern: $ => prec.left(PREC.AND, seq(
      field('pattern', $._refutable_pattern),
      'and',
      field('value', $._expr_no_ext),
      'is',
      field('subpattern', $._refutable_pattern),
    )),

    refutable_and_is_pattern_no_ext: $ => prec.left(PREC.AND, seq(
      field('pattern', $._refutable_pattern_no_ext),
      'and',
      field('value', $._expr_no_ext),
      'is',
      field('subpattern', $._refutable_pattern_no_ext),
    )),

    refutable_range_pattern: $ => choice(
      prec.left(PREC.RANGE, seq($._refutable_pattern, choice('..', '..='), $._refutable_pattern)),
      prec.right(PREC.PREFIX, seq(choice('..', '..='), $._refutable_pattern)),
    ),

    refutable_range_pattern_no_ext: $ => choice(
      prec.left(PREC.RANGE, seq($._refutable_pattern_no_ext, choice('..', '..='), $._refutable_pattern_no_ext)),
      prec.right(PREC.PREFIX, seq(choice('..', '..='), $._refutable_pattern_no_ext)),
    ),

    refutable_postfix_pattern: $ => prec.left(PREC.POSTFIX, seq(
      field('pattern', $._refutable_pattern),
      field('operator', choice('?', '!')),
    )),

    refutable_postfix_pattern_no_ext: $ => prec.left(PREC.POSTFIX, seq(
      field('pattern', $._refutable_pattern_no_ext),
      field('operator', choice('?', '!')),
    )),

    refutable_member_pattern: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._refutable_pattern),
      '.',
      field('member', choice($._ident, '*')),
    )),

    refutable_member_pattern_no_ext: $ => prec.left(PREC.MEMBER, seq(
      field('object', $._refutable_pattern_no_ext),
      '.',
      field('member', choice($._ident, '*')),
    )),

    refutable_application_pattern: $ => prec.left(PREC.POSTFIX, seq(
      field('constructor', $._refutable_pattern),
      '(',
      commaSep($._refutable_pattern),
      ')',
    )),

    refutable_application_pattern_no_ext: $ => prec.left(PREC.POSTFIX, seq(
      field('constructor', $._refutable_pattern_no_ext),
      '(',
      commaSep($._refutable_pattern),
      ')',
    )),

    refutable_extended_pattern: $ => prec.left(PREC.POSTFIX, seq(
      field('constructor', $._refutable_pattern),
      '{',
      commaOrSemiSep($, choice($.property_pattern, $.rest_pattern, $._ident)),
      '}',
    )),

    refutable_as_pattern: $ => prec.left(PREC.PREFIX - 5, seq(
      field('pattern', $._refutable_pattern),
      'as',
      field('name', $._ident),
    )),

    refutable_as_pattern_no_ext: $ => prec.left(PREC.PREFIX - 5, seq(
      field('pattern', $._refutable_pattern_no_ext),
      'as',
      field('name', $._ident),
    )),

    guard_pattern: $ => prec.left(PREC.RANGE, seq(
      field('pattern', $._refutable_pattern),
      'if',
      field('condition', $._expression),
    )),

    guard_pattern_no_ext: $ => prec.left(PREC.RANGE, seq(
      field('pattern', $._refutable_pattern_no_ext),
      'if',
      field('condition', $._expr_no_ext),
    )),
  },
});
