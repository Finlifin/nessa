#include "tree_sitter/parser.h"
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <wctype.h>

enum TokenType {
  BLOCK_COMMENT,
  INDENT,
  OUTDENT,
  VIRTUAL_SEMI,
  VIEW_TICK,
  LABEL_TICK,
  CHAR_LITERAL,
  MACRO_CONTENT,
  STRING_CONTENT,
  ERROR_SENTINEL,
};

#define MAX_INDENTS 128

typedef struct {
  uint16_t indents[MAX_INDENTS];
  uint8_t count;
} Scanner;

static inline bool is_horiz_space(int32_t c) {
  return c == ' ' || c == '\t' || c == '\r';
}

static inline bool is_any_space(int32_t c) {
  return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' ||
         c == '\v';
}

static inline bool is_ident_start(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_' ||
         (c > 127 && iswalpha((wint_t)c));
}

static inline bool is_ident_continue(int32_t c) {
  return is_ident_start(c) || (c >= '0' && c <= '9') ||
         (c > 127 && iswalnum((wint_t)c));
}

static inline bool is_hex_digit(int32_t c) {
  return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') ||
         (c >= 'A' && c <= 'F');
}

void *tree_sitter_nessa_external_scanner_create(void) {
  Scanner *scanner = (Scanner *)calloc(1, sizeof(Scanner));
  if (scanner != NULL) {
    scanner->indents[0] = 0;
    scanner->count = 1;
  }
  return scanner;
}

void tree_sitter_nessa_external_scanner_destroy(void *payload) {
  free(payload);
}

unsigned tree_sitter_nessa_external_scanner_serialize(void *payload,
                                                      char *buffer) {
  Scanner *scanner = (Scanner *)payload;
  if (scanner == NULL || scanner->count <= 1) {
    return 0;
  }
  unsigned size = 0;
  for (uint8_t i = 1; i < scanner->count; i++) {
    if (size + 2 > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) {
      break;
    }
    buffer[size++] = (char)(scanner->indents[i] & 0xFF);
    buffer[size++] = (char)((scanner->indents[i] >> 8) & 0xFF);
  }
  return size;
}

void tree_sitter_nessa_external_scanner_deserialize(void *payload,
                                                    const char *buffer,
                                                    unsigned length) {
  Scanner *scanner = (Scanner *)payload;
  if (scanner == NULL) {
    return;
  }
  scanner->indents[0] = 0;
  scanner->count = 1;
  for (unsigned i = 0; i + 1 < length && scanner->count < MAX_INDENTS; i += 2) {
    uint16_t lo = (uint8_t)buffer[i];
    uint16_t hi = (uint8_t)buffer[i + 1];
    scanner->indents[scanner->count++] = (uint16_t)(lo | (hi << 8));
  }
}

static bool scan_block_comment_after_open_brace(TSLexer *lexer) {
  // Caller has already consumed '{' and verified lookahead == '-'.
  lexer->advance(lexer, false); // consume '-'
  int depth = 1;
  while (lexer->lookahead != 0 && depth > 0) {
    if (lexer->lookahead == '{') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '-') {
        lexer->advance(lexer, false);
        depth++;
      }
    } else if (lexer->lookahead == '-') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '}') {
        lexer->advance(lexer, false);
        depth--;
      }
    } else {
      lexer->advance(lexer, false);
    }
  }
  lexer->mark_end(lexer);
  lexer->result_symbol = BLOCK_COMMENT;
  return true;
}

static void skip_nested_block_comment_lookahead(TSLexer *lexer) {
  // Caller consumed '{' and verified lookahead == '-'.
  lexer->advance(lexer, false);
  int depth = 1;
  while (lexer->lookahead != 0 && depth > 0) {
    if (lexer->lookahead == '{') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '-') {
        lexer->advance(lexer, false);
        depth++;
      }
    } else if (lexer->lookahead == '-') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '}') {
        lexer->advance(lexer, false);
        depth--;
      }
    } else {
      lexer->advance(lexer, false);
    }
  }
}

static bool scan_string_content(TSLexer *lexer) {
  bool has_content = false;
  while (lexer->lookahead != 0 && lexer->lookahead != '"' &&
         lexer->lookahead != '\\' && lexer->lookahead != '\n' &&
         lexer->lookahead != '{' && lexer->lookahead != '}') {
    lexer->advance(lexer, false);
    has_content = true;
  }
  if (has_content) {
    lexer->mark_end(lexer);
    lexer->result_symbol = STRING_CONTENT;
    return true;
  }
  return false;
}

static bool scan_quote_or_char(TSLexer *lexer, const bool *valid_symbols) {
  // Consume opening '\''
  lexer->advance(lexer, false);
  lexer->mark_end(lexer);

  int32_t first = lexer->lookahead;
  if (first == 0 || first == '\n') {
    return false;
  }

  // Macro content: '{ ... } or '   { ... }
  if (first == '{' && valid_symbols[MACRO_CONTENT]) {
    lexer->advance(lexer, false);
    int depth = 1;
    while (lexer->lookahead != 0 && depth > 0) {
      if (lexer->lookahead == '{') {
        depth++;
      } else if (lexer->lookahead == '}') {
        depth--;
      }
      lexer->advance(lexer, false);
    }
    if (depth == 0) {
      lexer->mark_end(lexer);
      lexer->result_symbol = MACRO_CONTENT;
      return true;
    }
    return false;
  }

  // Escaped character literal: '\n', '\x7f', '\u0041', '\u{1F600}', etc.
  if (first == '\\') {
    if (!valid_symbols[CHAR_LITERAL]) {
      return false;
    }
    lexer->advance(lexer, false);
    int32_t esc = lexer->lookahead;
    if (esc == 0 || esc == '\n') {
      return false;
    }
    if (esc == 'x') {
      lexer->advance(lexer, false);
      for (int i = 0; i < 2 && is_hex_digit(lexer->lookahead); i++) {
        lexer->advance(lexer, false);
      }
    } else if (esc == 'u') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '{') {
        lexer->advance(lexer, false);
        while (lexer->lookahead != 0 && lexer->lookahead != '}' &&
               lexer->lookahead != '\n') {
          lexer->advance(lexer, false);
        }
        if (lexer->lookahead == '}') {
          lexer->advance(lexer, false);
        }
      } else {
        for (int i = 0; i < 4 && is_hex_digit(lexer->lookahead); i++) {
          lexer->advance(lexer, false);
        }
      }
    } else {
      lexer->advance(lexer, false);
    }
    if (lexer->lookahead == '\'') {
      lexer->advance(lexer, false);
      lexer->mark_end(lexer);
      lexer->result_symbol = CHAR_LITERAL;
      return true;
    }
    return false;
  }

  // Empty '' is invalid as char literal
  if (first == '\'') {
    return false;
  }

  // Advance one Unicode code point to check for closing '\'' (including '`')
  lexer->advance(lexer, false);
  if (lexer->lookahead == '\'') {
    if (valid_symbols[CHAR_LITERAL]) {
      lexer->advance(lexer, false);
      lexer->mark_end(lexer);
      lexer->result_symbol = CHAR_LITERAL;
      return true;
    }
    return false;
  }

  // Backtick identifier after tick: obj'`raw-view` or '`outer`:
  if (first == '`') {
    if (valid_symbols[VIEW_TICK]) {
      lexer->result_symbol = VIEW_TICK;
      return true;
    }
    if (valid_symbols[LABEL_TICK]) {
      lexer->result_symbol = LABEL_TICK;
      return true;
    }
    return false;
  }

  // Whitespace after opening '\'' followed by '{' -> macro content
  if (is_horiz_space(first) && valid_symbols[MACRO_CONTENT]) {
    while (is_horiz_space(lexer->lookahead)) {
      lexer->advance(lexer, false);
    }
    if (lexer->lookahead == '{') {
      lexer->advance(lexer, false);
      int depth = 1;
      while (lexer->lookahead != 0 && depth > 0) {
        if (lexer->lookahead == '{') {
          depth++;
        } else if (lexer->lookahead == '}') {
          depth--;
        }
        lexer->advance(lexer, false);
      }
      if (depth == 0) {
        lexer->mark_end(lexer);
        lexer->result_symbol = MACRO_CONTENT;
        return true;
      }
    }
    return false;
  }

  // Not a closed char literal: check postfix view tick or loop label tick!
  if (is_ident_start(first)) {
    if (valid_symbols[VIEW_TICK]) {
      lexer->result_symbol = VIEW_TICK;
      return true;
    }
    if (valid_symbols[LABEL_TICK]) {
      lexer->result_symbol = LABEL_TICK;
      return true;
    }
  }

  return false;
}

static bool ident_is_continuation_keyword(const char *buf, size_t len,
                                          TSLexer *lexer) {
  if (len == 2) {
    if (memcmp(buf, "or", 2) == 0 || memcmp(buf, "as", 2) == 0 ||
        memcmp(buf, "in", 2) == 0 || memcmp(buf, "is", 2) == 0 ||
        memcmp(buf, "do", 2) == 0) {
      return true;
    }
  } else if (len == 3) {
    if (memcmp(buf, "and", 3) == 0) {
      return true;
    }
  } else if (len == 4) {
    if (memcmp(buf, "else", 4) == 0) {
      // `else =>` inside `when { ... }` starts an arm!
      // All other `else` (`if ... else`, `let ... else`) continue the statement.
      while (is_horiz_space(lexer->lookahead)) {
        lexer->advance(lexer, false);
      }
      if (lexer->lookahead == '=') {
        lexer->advance(lexer, false);
        if (lexer->lookahead == '>') {
          return false;
        }
      }
      return true;
    }
  } else if (len == 5) {
    if (memcmp(buf, "match", 5) == 0 || memcmp(buf, "where", 5) == 0) {
      return true;
    }
  } else if (len == 7) {
    if (memcmp(buf, "matches", 7) == 0) {
      return true;
    }
  }
  return false;
}

// Inspects the token starting at `first` (where `lexer->lookahead` is the
// character immediately AFTER `first` because `first` was already consumed, OR
// if `already_consumed == false`, `first == lexer->lookahead` has not been
// consumed yet).
static bool token_can_start_statement(TSLexer *lexer, int32_t first,
                                      bool already_consumed) {
  if (first == 0) {
    return false;
  }
  switch (first) {
  case '}':
  case ')':
  case ']':
  case ',':
  case ';':
  case '.':
  case ':':
  case '=':
  case '*':
  case '/':
  case '%':
  case '<':
  case '>':
  case '|':
  case '^':
  case '~':
  case '@':
  case '$':
  case '&':
    return false;

  case '+':
    if (!already_consumed) {
      lexer->advance(lexer, false);
    }
    return lexer->lookahead != '=' && lexer->lookahead != '+';

  case '-':
    if (!already_consumed) {
      lexer->advance(lexer, false);
    }
    return lexer->lookahead != '=' && lexer->lookahead != '>' &&
           lexer->lookahead != '-';

  case '?':
    return true;

  case '#':
    if (!already_consumed) {
      lexer->advance(lexer, false);
    }
    while (is_horiz_space(lexer->lookahead)) {
      lexer->advance(lexer, false);
    }
    // `expr \n # { ... }` is postfix effect elimination, not a new statement.
    return lexer->lookahead != '{';

  case '!':
    if (!already_consumed) {
      lexer->advance(lexer, false);
    }
    if (lexer->lookahead == '=') {
      return false;
    }
    while (is_horiz_space(lexer->lookahead)) {
      lexer->advance(lexer, false);
    }
    // `expr \n ! { ... }` is postfix error elimination, not a new statement.
    return lexer->lookahead != '{';

  case '{':
  case '[':
  case '(':
  case '"':
  case '`':
    return true;

  case '\'':
    return true;

  default:
    if (first >= '0' && first <= '9') {
      return true;
    }
    if (is_ident_start(first)) {
      char buf[16];
      size_t len = 0;
      if (first < 128) {
        buf[len++] = (char)first;
      } else {
        return true;
      }
      if (!already_consumed) {
        lexer->advance(lexer, false);
      }
      while (is_ident_continue(lexer->lookahead)) {
        if (lexer->lookahead < 128 && len < sizeof(buf)) {
          buf[len++] = (char)lexer->lookahead;
        } else {
          len = sizeof(buf);
        }
        lexer->advance(lexer, false);
      }
      if (len < sizeof(buf) && ident_is_continuation_keyword(buf, len, lexer)) {
        return false;
      }
      return true;
    }
    return false;
  }
}

static bool scan_newline_and_layout(Scanner *scanner, TSLexer *lexer,
                                    const bool *valid_symbols) {
  // `lexer->lookahead` is currently '\n' and `lexer->mark_end(lexer)` has
  // already been called at this exact position (zero-width).
  lexer->advance(lexer, false); // consume '\n'
  uint16_t col = 0;

  for (;;) {
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
           lexer->lookahead == '\r') {
      if (lexer->lookahead == ' ') {
        col++;
      } else if (lexer->lookahead == '\t') {
        col += 4;
      }
      lexer->advance(lexer, false);
    }

    if (lexer->lookahead == '\n') {
      col = 0;
      lexer->advance(lexer, false);
      continue;
    }

    if (lexer->lookahead == 0) {
      // EOF reached after newline(s)
      if (valid_symbols[OUTDENT] && scanner != NULL && scanner->count > 1) {
        scanner->count--;
        lexer->result_symbol = OUTDENT;
        return true;
      }
      return false;
    }

    if (lexer->lookahead == '-') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '-') {
        // Line comment `-- ...`: skip to end of line
        while (lexer->lookahead != 0 && lexer->lookahead != '\n') {
          lexer->advance(lexer, false);
        }
        continue;
      }
      // Real token starting with '-' at `col`!
      uint16_t current =
          (scanner != NULL && scanner->count > 0)
              ? scanner->indents[scanner->count - 1]
              : 0;
      if (valid_symbols[OUTDENT] && scanner != NULL && scanner->count > 1 &&
          col < current) {
        scanner->count--;
        lexer->result_symbol = OUTDENT;
        return true;
      }
      if (valid_symbols[INDENT] && scanner != NULL &&
          scanner->count < MAX_INDENTS && col > current) {
        scanner->indents[scanner->count++] = col;
        lexer->result_symbol = INDENT;
        return true;
      }
      if (valid_symbols[VIRTUAL_SEMI] &&
          token_can_start_statement(lexer, '-', true)) {
        lexer->result_symbol = VIRTUAL_SEMI;
        return true;
      }
      return false;
    }

    if (lexer->lookahead == '{') {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '-') {
        // Block comment `{- ... -}`: skip and continue scanning
        skip_nested_block_comment_lookahead(lexer);
        continue;
      }
      // Real '{' token at `col`!
      uint16_t current =
          (scanner != NULL && scanner->count > 0)
              ? scanner->indents[scanner->count - 1]
              : 0;
      if (valid_symbols[OUTDENT] && scanner != NULL && scanner->count > 1 &&
          col < current) {
        scanner->count--;
        lexer->result_symbol = OUTDENT;
        return true;
      }
      if (valid_symbols[INDENT] && scanner != NULL &&
          scanner->count < MAX_INDENTS && col > current) {
        scanner->indents[scanner->count++] = col;
        lexer->result_symbol = INDENT;
        return true;
      }
      if (valid_symbols[VIRTUAL_SEMI]) {
        lexer->result_symbol = VIRTUAL_SEMI;
        return true;
      }
      return false;
    }

    // Ordinary non-comment token at `col`!
    uint16_t current =
        (scanner != NULL && scanner->count > 0)
            ? scanner->indents[scanner->count - 1]
            : 0;

    if (valid_symbols[OUTDENT] && scanner != NULL && scanner->count > 1 &&
        col < current) {
      scanner->count--;
      lexer->result_symbol = OUTDENT;
      return true;
    }

    if (valid_symbols[INDENT] && scanner != NULL &&
        scanner->count < MAX_INDENTS && col > current) {
      scanner->indents[scanner->count++] = col;
      lexer->result_symbol = INDENT;
      return true;
    }

    if (valid_symbols[VIRTUAL_SEMI] &&
        token_can_start_statement(lexer, lexer->lookahead, false)) {
      lexer->result_symbol = VIRTUAL_SEMI;
      return true;
    }

    return false;
  }
}

bool tree_sitter_nessa_external_scanner_scan(void *payload, TSLexer *lexer,
                                             const bool *valid_symbols) {
  Scanner *scanner = (Scanner *)payload;

  // Error recovery mode: only scan nested `{- ... -}` block comments, never
  // emit zero-width layout tokens.
  if (valid_symbols[ERROR_SENTINEL]) {
    while (is_any_space(lexer->lookahead)) {
      lexer->advance(lexer, true);
    }
    if (lexer->lookahead == '{' && valid_symbols[BLOCK_COMMENT]) {
      lexer->advance(lexer, false);
      if (lexer->lookahead == '-') {
        return scan_block_comment_after_open_brace(lexer);
      }
    }
    return false;
  }

  // String content must be scanned BEFORE skipping any whitespace!
  if (valid_symbols[STRING_CONTENT] && scan_string_content(lexer)) {
    return true;
  }

  // Skip horizontal whitespace on the current line first.
  while (is_horiz_space(lexer->lookahead)) {
    lexer->advance(lexer, true);
  }

  // Mark zero-width end before inspecting EOF, '\n', or closing '}' for layout.
  lexer->mark_end(lexer);

  if (lexer->lookahead == 0) {
    if (valid_symbols[OUTDENT] && scanner != NULL && scanner->count > 1) {
      scanner->count--;
      lexer->result_symbol = OUTDENT;
      return true;
    }
    return false;
  }

  if (lexer->lookahead == '\n') {
    if (valid_symbols[OUTDENT] || valid_symbols[INDENT] ||
        valid_symbols[VIRTUAL_SEMI]) {
      if (scan_newline_and_layout(scanner, lexer, valid_symbols)) {
        return true;
      }
      // None of the layout tokens applied; we already consumed past '\n' with
      // advance(false), so return false and Tree-sitter will reset to mark_end
      // and skip whitespace via internal `extras`!
      return false;
    }
    // Layout tokens are not valid here; skip all whitespace for block comments
    // or quote/char tokens on later lines.
    while (is_any_space(lexer->lookahead)) {
      lexer->advance(lexer, true);
    }
    lexer->mark_end(lexer);
    if (lexer->lookahead == 0) {
      return false;
    }
  }

  // Same-line closing '}' closes an enclosing indented block if directly valid.
  if (lexer->lookahead == '}' && valid_symbols[OUTDENT] && scanner != NULL &&
      scanner->count > 1) {
    scanner->count--;
    lexer->result_symbol = OUTDENT;
    return true;
  }

  if (lexer->lookahead == '{' && valid_symbols[BLOCK_COMMENT]) {
    lexer->advance(lexer, false);
    if (lexer->lookahead == '-') {
      return scan_block_comment_after_open_brace(lexer);
    }
    return false;
  }

  if (lexer->lookahead == '\'' &&
      (valid_symbols[VIEW_TICK] || valid_symbols[LABEL_TICK] ||
       valid_symbols[CHAR_LITERAL] || valid_symbols[MACRO_CONTENT])) {
    return scan_quote_or_char(lexer, valid_symbols);
  }

  return false;
}
