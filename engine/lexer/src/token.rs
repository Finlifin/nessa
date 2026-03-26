use rustc_span::{BytePos, Span};

pub type Index = u32;

/// All token kinds in nessa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // -- Punctuation & operators --
    Bang,      // !
    BangEq,    // !=
    Hash,      // #
    Dot,       // .
    Colon,     // :
    Eq,        // =
    EqEq,      // ==
    FatArrow,  // =>
    Plus,      // +
    PlusPlus,  // ++
    PlusEq,    // +=
    Minus,     // -
    MinusEq,   // -=
    Arrow,     // ->
    Star,      // *
    StarEq,    // *=
    Slash,     // /
    SlashEq,   // /=
    Percent,   // %
    PercentEq, // %=
    Caret,     // ^
    Ampersand, // &
    Pipe,      // |
    PipeGt,    // |>
    Tilde,     // ~
    At,        // @
    Dollar,    // $
    Lt,        // <
    LtEq,      // <=
    Gt,        // >
    GtEq,      // >=
    Question,  // ?
    Backslash, // \\
    LBracket,  // [
    RBracket,  // ]
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    Comma,     // ,
    Quote,     // '
    Semi,      // ;

    // -- Layout (virtual or real) tokens --
    Newline, // '\n'
    Indent,  // indentation increase
    Outdent, // indentation decrease

    // -- Primitive literals --
    String,  // "..."
    Integer, // 123
    IntBin,  // 0b1010
    IntOct,  // 0o777
    IntHex,  // 0xFF
    Real,    // 123.45
    RealSci, // 1.23e-4
    Char,    // 'a' or '\n'

    // -- Keywords --
    Underscore,  // _
    KwAnd,       // and
    KwAs,        // as
    KwAssoc,     // assoc   (new in nessa2)
    KwAsync,     // async
    KwAtomic,    // atomic
    KwAwait,     // await
    KwBreak,     // break
    KwCase,      // case
    KwCatch,     // catch
    KwConst,     // const
    KwContinue,  // continue
    KwDef,       // def
    KwDefer,     // defer
    KwDerive,    // derive
    KwDo,        // do
    KwEffect,    // effect
    KwElse,      // else
    KwEnum,      // enum
    KwError,     // error
    KwExtend,    // extend
    KwExtern,    // extern
    KwFalse,     // false
    KwFn,        // fn
    KwFor,       // for
    KwGlobal,    // global  (new in nessa2)
    KwHandles,   // handles
    KwIf,        // if
    KwImpl,      // impl
    KwIn,        // in
    KwIs,        // is
    KwItself,    // itself
    KwLambda,    // lambda
    KwLet,       // let
    KwMatch,     // match
    KwMatches,   // matches
    KwMod,       // mod
    KwNewtype,   // newtype
    KwNot,       // not
    KwNull,      // null
    KwOr,        // or
    KwPrivate,   // private
    KwPub,       // pub     (new in nessa2)
    KwReset,     // reset
    KwResume,    // resume
    KwReturn,    // return
    KwSelfLower, // self
    KwSelfCap,   // Self
    KwShift,     // shift
    KwStatic,    // static
    KwStruct,    // struct
    KwTest,      // test
    KwTrait,     // trait
    KwTrue,      // true
    KwTypealias, // typealias
    KwUse,       // use
    KwVar,       // var     (new in nessa2)
    KwWhen,      // when
    KwWhile,     // while
    KwWhere,     // where

    // -- Others --
    Id,           // identifier
    ArbitraryId,  // `arbitrary text`
    MacroContent, // '{ ... }' macro content
    Comment,      // -- comment or {- comment -}
    Invalid,      // invalid token
    Sof,          // start of file
    Eof,          // end of file
}

impl TokenKind {
    /// Return the fixed lexeme for keyword/operator tokens, or a descriptive
    /// placeholder for variable-content tokens.
    pub fn lexeme(self) -> &'static str {
        match self {
            Self::Bang => "!",
            Self::BangEq => "!=",
            Self::Hash => "#",
            Self::Dot => ".",
            Self::Colon => ":",
            Self::Eq => "=",
            Self::EqEq => "==",
            Self::FatArrow => "=>",
            Self::Plus => "+",
            Self::PlusPlus => "++",
            Self::PlusEq => "+=",
            Self::Minus => "-",
            Self::MinusEq => "-=",
            Self::Arrow => "->",
            Self::Star => "*",
            Self::StarEq => "*=",
            Self::Slash => "/",
            Self::SlashEq => "/=",
            Self::Percent => "%",
            Self::PercentEq => "%=",
            Self::Caret => "^",
            Self::Ampersand => "&",
            Self::Pipe => "|",
            Self::PipeGt => "|>",
            Self::Tilde => "~",
            Self::At => "@",
            Self::Dollar => "$",
            Self::Lt => "<",
            Self::LtEq => "<=",
            Self::Gt => ">",
            Self::GtEq => ">=",
            Self::Question => "?",
            Self::Backslash => "\\",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::Comma => ",",
            Self::Quote => "'",
            Self::Semi => ";",
            Self::Newline => "<NEWLINE>",
            Self::Indent => "<INDENT>",
            Self::Outdent => "<OUTDENT>",
            Self::Underscore => "_",
            Self::KwAnd => "and",
            Self::KwAs => "as",
            Self::KwAssoc => "assoc",
            Self::KwAsync => "async",
            Self::KwAtomic => "atomic",
            Self::KwAwait => "await",
            Self::KwBreak => "break",
            Self::KwCase => "case",
            Self::KwCatch => "catch",
            Self::KwConst => "const",
            Self::KwContinue => "continue",
            Self::KwDef => "def",
            Self::KwDefer => "defer",
            Self::KwDerive => "derive",
            Self::KwDo => "do",
            Self::KwEffect => "effect",
            Self::KwElse => "else",
            Self::KwEnum => "enum",
            Self::KwError => "error",
            Self::KwExtend => "extend",
            Self::KwExtern => "extern",
            Self::KwFalse => "false",
            Self::KwFn => "fn",
            Self::KwFor => "for",
            Self::KwGlobal => "global",
            Self::KwHandles => "handles",
            Self::KwIf => "if",
            Self::KwImpl => "impl",
            Self::KwIn => "in",
            Self::KwIs => "is",
            Self::KwItself => "itself",
            Self::KwLambda => "lambda",
            Self::KwLet => "let",
            Self::KwMatch => "match",
            Self::KwMatches => "matches",
            Self::KwMod => "mod",
            Self::KwNewtype => "newtype",
            Self::KwNot => "not",
            Self::KwNull => "null",
            Self::KwOr => "or",
            Self::KwPrivate => "private",
            Self::KwPub => "pub",
            Self::KwReset => "reset",
            Self::KwResume => "resume",
            Self::KwReturn => "return",
            Self::KwSelfLower => "self",
            Self::KwSelfCap => "Self",
            Self::KwShift => "shift",
            Self::KwStatic => "static",
            Self::KwStruct => "struct",
            Self::KwTest => "test",
            Self::KwTrait => "trait",
            Self::KwTrue => "true",
            Self::KwTypealias => "typealias",
            Self::KwUse => "use",
            Self::KwVar => "var",
            Self::KwWhen => "when",
            Self::KwWhile => "while",
            Self::KwWhere => "where",
            Self::Id => "<identifier>",
            Self::ArbitraryId => "<arbitrary_id>",
            Self::MacroContent => "<macro>",
            Self::Comment => "<comment>",
            Self::Invalid => "<invalid>",
            Self::Sof => "<SOF>",
            Self::Eof => "<EOF>",
            Self::String => "<string_literal>",
            Self::Integer => "<integer_literal>",
            Self::IntBin => "<integer_bin>",
            Self::IntOct => "<integer_oct>",
            Self::IntHex => "<integer_hex>",
            Self::Real => "<real_literal>",
            Self::RealSci => "<real_sci>",
            Self::Char => "<char_literal>",
        }
    }

    /// Whether this token kind can legally end a statement or definition.
    pub fn can_end_statement(self) -> bool {
        matches!(
            self,
            Self::Hash
                | Self::Bang
                | Self::RBrace
                | Self::RBracket
                | Self::RParen
                | Self::Outdent
                | Self::Eof
                | Self::String
                | Self::Integer
                | Self::IntBin
                | Self::IntOct
                | Self::IntHex
                | Self::Real
                | Self::RealSci
                | Self::Char
                | Self::KwBreak
                | Self::KwContinue
                | Self::KwReturn
                | Self::KwTrue
                | Self::KwFalse
                | Self::KwNull
                | Self::KwAwait
                | Self::KwResume
                | Self::KwSelfLower
                | Self::KwSelfCap
                | Self::Underscore
                | Self::Id
                | Self::ArbitraryId
        )
    }

    /// Whether this token kind can legally start a statement or definition.
    pub fn can_start_statement(self) -> bool {
        matches!(
            self,
            Self::Plus
                | Self::Minus
                | Self::Hash
                | Self::Bang
                | Self::Question
                | Self::LBrace
                | Self::LBracket
                | Self::LParen
                | Self::Newline
                | Self::Indent
                | Self::String
                | Self::Integer
                | Self::IntBin
                | Self::IntOct
                | Self::IntHex
                | Self::Real
                | Self::RealSci
                | Self::Char
                | Self::KwTrue
                | Self::KwFalse
                | Self::KwResume
                | Self::KwDefer
                | Self::KwReturn
                | Self::KwContinue
                | Self::KwBreak
                | Self::KwConst
                | Self::KwDef
                | Self::KwDerive
                | Self::KwLet
                | Self::KwAsync
                | Self::KwDo
                | Self::KwAtomic
                | Self::KwEffect
                | Self::KwEnum
                | Self::KwError
                | Self::KwExtend
                | Self::KwExtern
                | Self::KwFn
                | Self::KwFor
                | Self::KwGlobal
                | Self::KwHandles
                | Self::KwImpl
                | Self::KwMod
                | Self::KwNewtype
                | Self::KwNull
                | Self::KwPrivate
                | Self::KwPub
                | Self::KwStruct
                | Self::KwTrait
                | Self::KwUse
                | Self::KwVar
                | Self::KwIf
                | Self::KwWhile
                | Self::KwWhen
                | Self::KwReset
                | Self::KwShift
                | Self::KwStatic
                | Self::KwSelfLower
                | Self::KwSelfCap
                | Self::KwTypealias
                | Self::KwAssoc
                | Self::Underscore
                | Self::Id
                | Self::ArbitraryId
        )
    }
}

impl std::fmt::Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.lexeme())
    }
}

/// A single token produced by the nessa lexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub from: Index,
    pub to: Index,
}

impl Token {
    #[inline]
    pub fn new(kind: TokenKind, from: Index, to: Index) -> Self {
        Self { kind, from, to }
    }

    pub const INVALID: Self = Self {
        kind: TokenKind::Invalid,
        from: 0,
        to: 0,
    };

    pub const EOF: Self = Self {
        kind: TokenKind::Eof,
        from: 0,
        to: 0,
    };

    pub fn span(self) -> Span {
        Span::new(BytePos(self.from), BytePos(self.to))
    }

    /// Return the source text slice that this token covers.
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.from as usize..self.to as usize]
    }
}

/// Look up an identifier string and return its keyword `TokenKind`, if any.
pub fn lookup_keyword(ident: &str) -> Option<TokenKind> {
    match ident {
        "_" => Some(TokenKind::Underscore),
        "and" => Some(TokenKind::KwAnd),
        "as" => Some(TokenKind::KwAs),
        "assoc" => Some(TokenKind::KwAssoc),
        "async" => Some(TokenKind::KwAsync),
        "atomic" => Some(TokenKind::KwAtomic),
        "await" => Some(TokenKind::KwAwait),
        "break" => Some(TokenKind::KwBreak),
        "case" => Some(TokenKind::KwCase),
        "catch" => Some(TokenKind::KwCatch),
        "const" => Some(TokenKind::KwConst),
        "continue" => Some(TokenKind::KwContinue),
        "def" => Some(TokenKind::KwDef),
        "defer" => Some(TokenKind::KwDefer),
        "derive" => Some(TokenKind::KwDerive),
        "do" => Some(TokenKind::KwDo),
        "effect" => Some(TokenKind::KwEffect),
        "else" => Some(TokenKind::KwElse),
        "enum" => Some(TokenKind::KwEnum),
        "error" => Some(TokenKind::KwError),
        "extend" => Some(TokenKind::KwExtend),
        "extern" => Some(TokenKind::KwExtern),
        "false" => Some(TokenKind::KwFalse),
        "fn" => Some(TokenKind::KwFn),
        "for" => Some(TokenKind::KwFor),
        "global" => Some(TokenKind::KwGlobal),
        "handles" => Some(TokenKind::KwHandles),
        "if" => Some(TokenKind::KwIf),
        "impl" => Some(TokenKind::KwImpl),
        "in" => Some(TokenKind::KwIn),
        "is" => Some(TokenKind::KwIs),
        "itself" => Some(TokenKind::KwItself),
        "lambda" => Some(TokenKind::KwLambda),
        "let" => Some(TokenKind::KwLet),
        "match" => Some(TokenKind::KwMatch),
        "matches" => Some(TokenKind::KwMatches),
        "mod" => Some(TokenKind::KwMod),
        "newtype" => Some(TokenKind::KwNewtype),
        "not" => Some(TokenKind::KwNot),
        "null" => Some(TokenKind::KwNull),
        "or" => Some(TokenKind::KwOr),
        "private" => Some(TokenKind::KwPrivate),
        "pub" => Some(TokenKind::KwPub),
        "reset" => Some(TokenKind::KwReset),
        "resume" => Some(TokenKind::KwResume),
        "return" => Some(TokenKind::KwReturn),
        "self" => Some(TokenKind::KwSelfLower),
        "Self" => Some(TokenKind::KwSelfCap),
        "shift" => Some(TokenKind::KwShift),
        "static" => Some(TokenKind::KwStatic),
        "struct" => Some(TokenKind::KwStruct),
        "test" => Some(TokenKind::KwTest),
        "trait" => Some(TokenKind::KwTrait),
        "true" => Some(TokenKind::KwTrue),
        "typealias" => Some(TokenKind::KwTypealias),
        "use" => Some(TokenKind::KwUse),
        "var" => Some(TokenKind::KwVar),
        "when" => Some(TokenKind::KwWhen),
        "while" => Some(TokenKind::KwWhile),
        "where" => Some(TokenKind::KwWhere),
        _ => None,
    }
}
