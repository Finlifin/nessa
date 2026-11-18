# Nessa Developer & Editor Tooling

This directory contains official language tooling and editor integrations for the Nessa programming language.

## Contents

| Path | Description |
| --- | --- |
| [`tree-sitter-nessa/`](./tree-sitter-nessa/README.md) | Tree-sitter grammar, C external scanner, and editor queries (`highlights`, `locals`, `indents`, `folds`, `tags`, `injections`) for `.ns` source files. |

## Quick Verification

From `tools/tree-sitter-nessa`:

```sh
tree-sitter generate
tree-sitter test
cc -shared -fPIC -O2 -Wall -Wextra -Werror -Isrc src/parser.c src/scanner.c -o /tmp/libtree_sitter_nessa_check.so && rm -f /tmp/libtree_sitter_nessa_check.so
```
