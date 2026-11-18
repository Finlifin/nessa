# tree-sitter-nessa

Tree-sitter grammar, C external scanner, and editor queries for the Nessa programming language (`.ns`).

## Overview

`tree-sitter-nessa` implements Nessa's lexical and surface syntax as defined in [`docs/grammar/`](../../docs/grammar/intro.md) and [`docs/code-style.md`](../../docs/code-style.md), including:

- Line comments (`-- ...`), documentation comments (`--- ...`), formatter directives (`--% ...`), and arbitrarily nested block comments (`{- ... -}`).
- Newline statement separation and format-sensitive indented blocks (`:` / `=>` / `do` followed by indented suites) alongside K&R `{ ... }` blocks.
- Single-quote disambiguation across character literals (`'a'`, `'\n'`, `'\x7f'`, `'\u0041'`, `'\u{1F600}'`), postfix view ticks (`expr'view`, `.panic'builtin`, `` obj'`raw-view` ``), and loop labels (`'outer: while cond { break 'outer }` as well as `:outer`).
- Interpolated double-quoted strings (`"hello {name}, {{literal}}"`) with arbitrary nested Nessa expressions and nested string literals.
- Arbitrary backtick identifiers (`` `extend` ``, `` `plus-weighted` ``) in declaration, member, parameter, pattern, and infix call (`` a `plus-weighted` b ``) positions.
- Full Nessa declarations (`pub` / `private`, `mod`, `use`, `fn`, `effect`, `async effect`, `handles`, `struct`, `enum`, `trait`, `assoc`, `impl`, `extend`, `derive`, `newtype`, `typealias`, `global`, `const`, `let`, `var`, `defer`), qualified types (`?T`, `!Err T`, `![E1, E2] T`, `#eff T`, `#[e1, e2] T`), and postfix elimination (`expr?`, `expr!`, `expr#`, `expr ! { ... }`, `expr # { ... }`, `expr match { ... }`, `expr.as(T)`, `expr.use(cap)`, `expr.await`, `expr do |x| { ... }`).

## Code Style Example

```nessa
use std.collections.{List, Map}

enum IoError {
    NotFound,
    Timeout(ms: u64),
}

enum Shape {
    circle(radius: f64),
    rect(width: f64, height: f64),
    point,
}

effect read_line() -> String
async effect fetch_bytes(url: String) -> !IoError Bytes

pub fn area(shape: Shape) -> f64 {
    shape match {
        Shape.circle(r) => 3.141592653589793 * r * r,
        Shape.rect(w, h) => w * h,
        Shape.point => 0.0,
    }
}
```

## Directory Layout

- `grammar.js` — Tree-sitter grammar specification.
- `src/scanner.c` — C external scanner for nested `{- -}` block comments, layout tokens (`INDENT` / `OUTDENT` / virtual statement separators), `'view` / `'label` / character literal disambiguation, and interpolated string segments.
- `src/parser.c`, `src/grammar.json`, `src/node-types.json` — Generated parser artifacts (`tree-sitter generate`).
- `queries/highlights.scm` — Syntax highlighting captures.
- `queries/locals.scm` — Lexical scope, local definitions, and reference captures.
- `queries/indents.scm` — Editor indentation rules.
- `queries/folds.scm` — Code folding regions.
- `queries/tags.scm` — Symbol navigation tags.
- `queries/injections.scm` — Comment and regex language injections.
- `test/corpus/` — Tree-sitter corpus test suite (`declarations.txt`, `types_and_effects.txt`, `expressions.txt`, `patterns_and_comments.txt`).

## Development Commands

```sh
cd tools/tree-sitter-nessa
tree-sitter generate
tree-sitter test
tree-sitter parse -q ../../library/std/*.ns ../../library/regex/src/lib.ns
```
