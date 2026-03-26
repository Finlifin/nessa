pub mod dump;

use rustc_span::Span;
use str_interner::StrId;

// ---------------------------------------------------------------------------
// NodeIndex – a compact handle into the AST node storage.
// 0 is the sentinel meaning "no node" (used for optional children).
// ---------------------------------------------------------------------------

/// Index of a node in the AST.  `NodeIndex(0)` is the "null" sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIndex(pub u32);

impl NodeIndex {
    pub const NULL: Self = Self(0);

    #[inline]
    pub fn is_null(self) -> bool {
        self.0 == 0
    }
}

// ---------------------------------------------------------------------------
// NodeKind – every syntactic construct in nessa
//
// Grammar reference: docs/grammar/*.md
// Each variant is annotated with:
//   - EBNF production from the grammar
//   - Children layout: [0]..[3] = fixed children, multi = variable-arity
// ---------------------------------------------------------------------------

/// Discriminant for every AST node kind in the nessa language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum NodeKind {
    /// Placeholder / null – never stored in the tree directly.
    Invalid = 0,

    // ── Leaf nodes ──────────────────────────────────────────────────
    /// `integer | int_bin | int_oct | int_hex`
    /// children: (none)
    Int,

    /// `real | real_sci`
    /// children: (none)
    Real,

    /// `string`
    /// children: (none)
    Str,

    /// `character`
    /// children: (none)
    Char,

    /// `true | false`
    /// children: (none)
    Bool,

    /// Identifier token
    /// children: (none)
    Id,

    /// `Self` (type-level self reference)
    /// children: (none)
    SelfUpper,

    /// `self` (instance-level self reference)
    /// children: (none)
    SelfLower,

    /// `null`
    /// children: (none)
    Null,

    /// `_` (wildcard / placeholder)
    /// children: (none)
    Underscore,

    /// `()`
    /// children: (none)
    Unit,

    /// `symbol -> . id`
    /// children: [0] id
    Symbol,

    // ── Collection literals ─────────────────────────────────────────
    /// `list_construction -> [expr*]`
    /// children: multi = elements
    ListOf,

    /// `tuple_construction -> (expr, expr*)`
    /// children: multi = elements
    Tuple,

    /// `object_construction -> { (property | expr)* }`
    /// children: multi = (Property | expr) items
    Object,

    // ── Lambda & case ───────────────────────────────────────────────
    /// `lambda -> |param*| (-> type)? (block | statement)`
    /// children: [0] body  [1] return_type  multi = params
    Lambda,

    /// `case_map -> case pattern => (block | statement)`
    /// children: [0] pattern  [1] body
    CaseMap,

    /// `case_alternative -> case_map | case_map`
    /// children: [0] left  [1] right
    CaseAlternative,

    // ── Prefix (unary) expressions ──────────────────────────────────
    /// `neg -> -expr`
    /// children: [0] inner
    Negative,

    /// `not -> not expr`
    /// children: [0] inner
    BoolNot,

    /// `error_construction -> error expr`
    /// children: [0] inner
    ErrorConstruction,

    /// `optional_type -> ?type_expr`
    /// children: [0] inner
    OptionalType,

    /// `effect_qualified_type -> #effect_set_expr type_expr`
    /// children: [0] effects  [1] inner_type
    EffectQualifiedType,

    /// `error_qualified_type -> !error_set_expr type_expr`
    /// children: [0] errors  [1] inner_type
    ErrorQualifiedType,

    // ── Range expressions ───────────────────────────────────────────
    /// `range_full -> ..`
    /// children: (none)
    RangeFull,

    /// `range_from -> expr ..`
    /// children: [0] start
    RangeFrom,

    /// `range_to -> .. expr`
    /// children: [0] end
    RangeTo,

    /// `range_from_to -> expr .. expr`
    /// children: [0] start  [1] end
    RangeFromTo,

    /// `range_to_inclusive -> ..= expr`
    /// children: [0] end
    RangeToInclusive,

    /// `range_from_to_inclusive -> expr ..= expr`
    /// children: [0] start  [1] end
    RangeFromToInclusive,

    // ── Binary arithmetic ───────────────────────────────────────────
    /// `add -> expr + expr`
    /// children: [0] lhs  [1] rhs
    Add,

    /// `sub -> expr - expr`
    /// children: [0] lhs  [1] rhs
    Sub,

    /// `mul -> expr * expr`
    /// children: [0] lhs  [1] rhs
    Mul,

    /// `div -> expr / expr`
    /// children: [0] lhs  [1] rhs
    Div,

    /// `mod -> expr % expr`
    /// children: [0] lhs  [1] rhs
    Mod,

    /// `concat -> expr ++ expr`
    /// children: [0] lhs  [1] rhs
    Concat,

    // ── Binary comparison ───────────────────────────────────────────
    /// `eq -> expr == expr`
    /// children: [0] lhs  [1] rhs
    BoolEq,

    /// `not_eq -> expr != expr`
    /// children: [0] lhs  [1] rhs
    BoolNotEq,

    /// `gt -> expr > expr`
    /// children: [0] lhs  [1] rhs
    BoolGt,

    /// `gte -> expr >= expr`
    /// children: [0] lhs  [1] rhs
    BoolGtEq,

    /// `lt -> expr < expr`
    /// children: [0] lhs  [1] rhs
    BoolLt,

    /// `lte -> expr <= expr`
    /// children: [0] lhs  [1] rhs
    BoolLtEq,

    // ── Binary logical ──────────────────────────────────────────────
    /// `and -> expr and expr`
    /// children: [0] lhs  [1] rhs
    BoolAnd,

    /// `or -> expr or expr`
    /// children: [0] lhs  [1] rhs
    BoolOr,

    /// `bool_matches -> expr matches pattern`
    /// children: [0] expr  [1] pattern
    BoolMatches,

    // ── Projections & pipes ─────────────────────────────────────────
    /// `projection -> expr . id`
    /// children: [0] object  [1] field_id
    Projection,

    /// `view -> expr ' id`
    /// children: [0] object  [1] field_id
    View,

    /// `arrow -> expr -> expr`
    /// children: [0] lhs  [1] rhs
    Arrow,

    /// `pipeline -> expr |> expr`
    /// children: [0] lhs  [1] rhs
    Pipeline,

    /// `infix_fn_call -> expr id expr`
    /// children: [0] lhs  [1] rhs
    InfixFnCall,

    // ── Effect / error / option handling ─────────────────────────────
    /// `effect_propagation -> expr #`
    /// children: [0] inner
    EffectPropagation,

    /// `effect_elimination -> expr # { case_arm* }`
    /// children: [0] inner  multi = arms
    EffectElimination,

    /// `error_propagation -> expr !`
    /// children: [0] inner
    ErrorPropagation,

    /// `error_elimination -> expr ! { (catch_arm | case_arm)* }`
    /// children: [0] inner  multi = arms
    ErrorElimination,

    /// `option_propagation -> expr ?`
    /// children: [0] inner
    OptionPropagation,

    /// `handler_application -> expr.use(handler)`
    /// children: [0] object  [1] handler
    HandlerApplication,

    // ── Call expressions ────────────────────────────────────────────
    /// `application -> expr (arg*)`
    /// children: [0] callee  multi = args
    Call,

    /// `extended_application -> expr { (property | expr)* }`
    /// children: [0] object  multi = args
    ExtendedCall,

    // ── Postfix expressions ─────────────────────────────────────────
    /// `post_match -> expr match { case_arm* }`
    /// children: [0] expr  multi = arms
    PostMatch,

    /// `post_do -> expr do (lambda | block)`
    /// children: [0] expr  [1] lambda_or_block
    PostDo,

    /// `type_cast -> expr.as(type)`
    /// children: [0] expr  [1] type
    TypeCast,

    // ── Delimited continuations ─────────────────────────────────────
    /// `reset expr block`
    /// children: [0] prompt  [1] body
    ResetExpr,

    /// `shift expr id block`
    /// children: [0] prompt  [1] cont_name  [2] body
    ShiftExpr,

    // ── Type expressions ────────────────────────────────────────────
    /// `fn_type -> fn (param_type*)`
    /// children: multi = param_types
    FnType,

    /// `effect_type -> async? effect (param_type*)`
    /// children: multi = param_types
    EffectType,

    // ── Statements ──────────────────────────────────────────────────
    /// Expression in statement position
    /// children: [0] inner
    ExprStatement,

    /// `let_decl -> let pattern (: expr)? = expr (else expr)?`
    /// children: [0] pattern  [1] type  [2] value  [3] else_branch
    LetDecl,

    /// `const_decl -> visibility_modifier? const pattern (: expr)? = expr (else expr)?`
    /// children: [0] pattern  [1] type  [2] value  [3] else_branch
    ConstDecl,

    /// `var pattern (: expr)? = expr`
    /// children: [0] pattern  [1] type  [2] value
    VarDecl,

    /// `global_decl -> visibility_modifier? global id : expr = expr`
    /// children: [0] inner
    GlobalDecl,

    /// `assoc_decl -> assoc id : expr (= expr)?`
    /// children: [0] inner
    AssocDecl,

    /// `handles(expr) let id (: expr)? = expr`
    /// children: [0] effect  [1] name  [2] type  [3] value
    LetHandler,

    /// `expr = expr`
    /// children: [0] lhs  [1] rhs
    Assign,

    /// `expr += expr`
    /// children: [0] lhs  [1] rhs
    AddAssign,

    /// `expr -= expr`
    /// children: [0] lhs  [1] rhs
    SubAssign,

    /// `expr *= expr`
    /// children: [0] lhs  [1] rhs
    MulAssign,

    /// `expr /= expr`
    /// children: [0] lhs  [1] rhs
    DivAssign,

    /// `expr %= expr`
    /// children: [0] lhs  [1] rhs
    ModAssign,

    /// `defer (block | statement)`
    /// children: [0] body
    DeferStatement,

    /// `return_statement -> return expr? (if expr)?`
    /// children: [0] value  [1] guard
    ReturnStatement,

    /// `resume_statement -> resume expr? (if expr)?`
    /// children: [0] value  [1] guard
    ResumeStatement,

    /// `break_statement -> break id? (if expr)?`
    /// children: [0] label  [1] guard
    BreakStatement,

    /// `continue_statement -> continue id? (if expr)?`
    /// children: [0] label  [1] guard
    ContinueStatement,

    /// `block -> { statement* }`
    /// children: multi = statements
    Block,

    // ── Control flow ────────────────────────────────────────────────
    /// `if_statement -> if expr block (else (if_statement | block))?`
    /// children: [0] condition  [1] then_block  [2] else_branch
    IfStatement,

    /// `for_loop -> for (: id)? pattern in expr block`
    /// children: [0] label  [1] pattern  [2] iter_expr  [3] body
    ForLoop,

    /// `while_loop -> while (: id)? expr block`
    /// children: [0] label  [1] condition  [2] body
    WhileLoop,

    /// `when_statement -> when { condition_arm* }`
    /// children: multi = arms
    WhenStatement,

    /// `condition_arm -> condition_expr => (block | statement)`
    /// children: [0] condition  [1] body
    ConditionArm,

    /// `else_condition_arm -> else => (block | statement)`
    /// children: [0] body
    ElseConditionArm,

    /// `case_arm -> pattern => (block | statement)` (arm in match / elimination)
    /// children: [0] pattern  [1] body
    CaseArm,

    /// `catch_arm -> catch id => (block | statement)`
    /// children: [0] error_id  [1] body
    CatchArm,

    // ── Pattern nodes ───────────────────────────────────────────────
    /// `pattern_or -> pattern or pattern`
    /// children: [0] lhs  [1] rhs
    PatternOr,

    /// `pattern_not -> not pattern`
    /// children: [0] inner
    PatternNot,

    /// `pattern_error -> error pattern`
    /// children: [0] inner
    PatternError,

    /// `pattern_call -> expr (pattern*)`
    /// children: [0] constructor  multi = args
    PatternCall,

    /// `pattern_extended_call -> expr { (property_pattern | id)* }`
    /// children: [0] constructor  multi = args
    PatternExtendedCall,

    /// `pattern_as_bind -> pattern as id`
    /// children: [0] pattern  [1] id
    PatternAsBind,

    /// `pattern_if_guard -> pattern if expr`
    /// children: [0] pattern  [1] guard_expr
    PatternIfGuard,

    /// `pattern_and_is -> pattern and expr is pattern`
    /// children: [0] lhs  [1] rhs
    PatternAndIs,

    /// `pattern_option_some -> pattern ?`
    /// children: [0] inner
    PatternOptionSome,

    /// `pattern_error_ok -> pattern !`
    /// children: [0] inner
    PatternErrorOk,

    /// `pattern_list -> [pattern*]`
    /// children: multi = elements
    PatternList,

    /// `pattern_tuple -> (pattern*)`
    /// children: multi = elements
    PatternTuple,

    /// `pattern_record -> { (property_pattern | id)* }`
    /// children: multi = properties
    PatternRecord,

    /// `pattern_rest_bind -> ...id`
    /// children: [0] id (or none)
    PatternRestBind,

    /// `pattern_async -> async pattern`
    /// children: [0] inner
    PatternAsync,

    /// Expression used as a pattern literal/value
    /// children: [0] expr
    PatternFromExpr,

    /// `property_pattern -> id : pattern`
    /// children: [0] id  [1] pattern
    PropertyPattern,

    // ── Definition nodes ────────────────────────────────────────────
    /// `function_def -> fn id ((param*)) (-> expr)? (handles expr)? ((= expr) | block)`
    /// children: [0] name  [1] return_type  [2] body  [3] capability  multi = params
    FunctionDef,

    /// `effect_def -> effect id (param*) (-> expr)?`
    /// children: [0] name  [1] return_type  multi = params
    EffectDef,

    /// `effect_def -> async effect id (param*) (-> expr)?`
    /// children: [0] name  [1] return_type  multi = params
    AsyncEffectDef,

    /// `handles_statement -> handles(expr)(param*) (-> expr)? ((= expr) | block)`
    /// children: [0] effect  [1] return_type  [2] body  multi = params
    HandlesStatement,

    /// `struct_def -> struct id { (struct_field | statement)* }`
    /// children: [0] name  multi = fields/statements
    StructDef,

    /// `struct_field -> id : expr (= expr)?`
    /// children: [0] name  [1] type  [2] default
    StructField,

    /// `enum_def -> enum id { (enum_variant | statement)* }`
    /// children: [0] name  multi = variants/statements
    EnumDef,

    /// `enum_variant -> id ((param*))?`
    /// children: [0] name  multi = params
    EnumVariant,

    /// `trait_def -> trait id ((expr+))? { (trait_def_fn | trait_derive_fn | assoc_decl | statement)* }`
    /// children: [0] name  [1] parents (ListOf or NULL)  multi = members
    TraitDef,

    /// `trait_def_fn -> fn id ((param*))? (-> expr)? (handles expr)?`
    /// children: [0] name  [1] return_type  [2] capability  multi = params
    TraitDefFn,

    /// `trait_derive_fn -> derive fn id ((param*))? (-> expr)? (handles expr)? ((= expr) | block)`
    /// children: [0] name  [1] return_type  [2] body  [3] capability  multi = params
    TraitDeriveFn,

    /// `impl_def -> impl expr { statement* }`
    /// children: [0] type  multi = members
    ImplDef,

    /// `impl_trait_def -> impl trait_expr for type_expr { (assoc_decl | statement)* }`
    /// children: [0] trait  [1] type  multi = members
    ImplTraitDef,

    /// `extend_def -> extend expr { statement* }`
    /// children: [0] type  multi = members
    ExtendDef,

    /// `extend_trait_def -> extend trait_expr for type_expr { (assoc_decl | statement)* }`
    /// children: [0] trait  [1] type  multi = members
    ExtendTraitDef,

    /// `derive_def -> derive expr+ for expr`
    /// children: [0] type_expr  multi = trait_exprs
    DeriveDef,

    /// `typealias -> typealias id = expr`
    /// children: [0] name  [1] type
    Typealias,

    /// `newtype -> newtype id = expr`
    /// children: [0] name  [1] type
    Newtype,

    /// `module_def -> mod id { statement* }`
    /// children: [0] name  multi = members
    ModuleDef,

    /// Visibility modifier wrapper: `pub definition`
    /// children: [0] inner
    PubDef,

    // ── Import / use ────────────────────────────────────────────────
    /// `use_statement -> pub? use path`
    /// children: [0] path
    UseStatement,

    /// `super_path -> . path`
    /// children: [0] inner
    SuperPath,

    /// `package_path -> @ path`
    /// children: [0] inner
    PackagePath,

    /// `path_projection -> path . path`
    /// children: [0] path  [1] id
    PathProjection,

    /// `path_projection_all -> path . *`
    /// children: [0] path
    PathProjectionAll,

    /// `path_projection_multi -> path . { path* }`
    /// children: [0] path  multi = items
    PathProjectionMulti,

    /// `path_as_bind -> id as id`
    /// children: [0] id  [1] alias
    PathAsBind,

    // ── Parameters ──────────────────────────────────────────────────
    /// `param_self -> self`
    /// children: (none)
    ParamSelf,

    /// `param_varargs -> ...id (: expr)?`
    /// children: [0] id  [1] type
    ParamVarargs,

    /// `param_optional -> . id : expr = expr`
    /// children: [0] id  [1] type  [2] default
    ParamOptional,

    /// `param_lambda -> lambda pattern (: expr)?`
    /// children: [0] pattern  [1] type
    ParamLambda,

    /// `param_typed -> pattern : expr`
    /// children: [0] pattern  [1] type
    ParamTyped,

    // ── Structural ──────────────────────────────────────────────────
    /// `property -> id : expr`
    /// children: [0] id  [1] value
    Property,

    /// `.id = expr` (property assignment)
    /// children: [0] id  [1] value
    PropertyAssignment,

    /// `id = expr` (named argument in function call)
    /// children: [0] id  [1] value
    NamedArg,

    /// Top-level file scope
    /// children: multi = items
    FileScope,
}

// ---------------------------------------------------------------------------
// NodeType – classifies how many children a node kind has.
// ---------------------------------------------------------------------------

/// Classification of node child structure, used for generic traversal/dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    /// Leaf node, no children.
    NoChild,
    /// Exactly 1 child.
    SingleChild,
    /// Exactly 2 children.
    DoubleChildren,
    /// Exactly 3 children.
    TripleChildren,
    /// Exactly 4 children.
    QuadrupleChildren,
    /// Variable number of children only.
    MultiChildren,
    /// 1 fixed child + variable children.
    SingleWithMulti,
    /// 2 fixed children + variable children.
    DoubleWithMulti,
    /// 3 fixed children + variable children.
    TripleWithMulti,
    /// 4 fixed children + variable children.
    QuadrupleWithMulti,
}

impl NodeKind {
    /// Return the structural classification of this node kind.
    pub fn node_type(self) -> NodeType {
        use NodeKind::*;
        use NodeType::*;
        match self {
            // Leaves
            Invalid | Int | Real | Str | Char | Bool | Id | SelfUpper | SelfLower | Null
            | Underscore | Unit | RangeFull | ParamSelf => NoChild,

            // 1 child
            Negative | BoolNot | ErrorConstruction | OptionalType | RangeFrom | RangeTo
            | RangeToInclusive | EffectPropagation | ErrorPropagation | OptionPropagation
            | ExprStatement | DeferStatement | PatternNot | PatternError | PatternAsync
            | PatternOptionSome | PatternErrorOk | PatternRestBind | PatternFromExpr
            | SuperPath | PackagePath | PathProjectionAll | ElseConditionArm | Symbol | PubDef
            | GlobalDecl | AssocDecl => SingleChild,

            // 2 children
            Add | Sub | Mul | Div | Mod | Concat | BoolEq | BoolNotEq | BoolGt | BoolGtEq
            | BoolLt | BoolLtEq | BoolAnd | BoolOr | BoolMatches | Projection | View | Arrow
            | Pipeline | InfixFnCall | RangeFromTo | RangeFromToInclusive | HandlerApplication
            | TypeCast | PostDo | CaseMap | CaseAlternative | PatternAsBind | PatternIfGuard
            | PatternAndIs | Assign | AddAssign | SubAssign | MulAssign | DivAssign | ModAssign
            | Typealias | Newtype | PropertyPattern | PathProjection | PathAsBind
            | ConditionArm | CaseArm | CatchArm | Property | PropertyAssignment | NamedArg
            | EffectQualifiedType | ErrorQualifiedType | ParamVarargs | ParamLambda
            | ParamTyped | PatternOr | ResetExpr | ReturnStatement | ResumeStatement
            | BreakStatement | ContinueStatement => DoubleChildren,

            // 3 children
            VarDecl | IfStatement | ShiftExpr | StructField | ParamOptional | WhileLoop => {
                TripleChildren
            }

            // 3 + multi
            HandlesStatement | TraitDefFn => TripleWithMulti,

            // 4 children
            LetDecl | ConstDecl | LetHandler | ForLoop => QuadrupleChildren,

            // Multi only
            ListOf | Tuple | Object | Block | FileScope | PatternList | PatternTuple
            | PatternRecord | WhenStatement | FnType | EffectType => MultiChildren,

            // 1 + multi
            UseStatement | PathProjectionMulti | StructDef | EnumDef | ImplDef | DeriveDef
            | ExtendDef | EffectElimination | ErrorElimination | PostMatch | ModuleDef
            | EnumVariant => SingleWithMulti,

            // 2 + multi
            Call | ExtendedCall | PatternCall | PatternExtendedCall | ImplTraitDef
            | ExtendTraitDef | Lambda | AsyncEffectDef | EffectDef | TraitDef => DoubleWithMulti,

            // 4 + multi
            FunctionDef | TraitDeriveFn => QuadrupleWithMulti,
        }
    }
}

// ---------------------------------------------------------------------------
// Node – a single entry in the AST arena
// ---------------------------------------------------------------------------

/// A single AST node stored in the flat arena.
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    pub span: Span,
    /// Interned string id for identifier / literal content (0 = unused).
    pub str_id: StrId,
    /// Fixed-arity children (up to 4).
    pub children: [NodeIndex; 4],
    /// Start index into `Ast::extra_children` for variable-arity children.
    pub multi_start: u32,
    /// Number of variable-arity children.
    pub multi_len: u32,
}

// ---------------------------------------------------------------------------
// Ast – arena-based abstract syntax tree
// ---------------------------------------------------------------------------

/// Arena-based AST.  Nodes are stored in `nodes[1..]` (index 0 is reserved as
/// the null sentinel).  Variable-arity children are stored in `extra_children`.
pub struct Ast {
    /// Node storage.  `nodes[0]` is the null sentinel.
    pub nodes: Vec<Node>,
    /// Flat storage for variable-arity children.
    pub extra_children: Vec<NodeIndex>,
    /// Root node index (set after a successful parse).
    pub root: NodeIndex,
    /// Source text for this AST (borrowed during dump).
    pub source: String,
}

impl Ast {
    pub fn new() -> Self {
        let null_node = Node {
            kind: NodeKind::Invalid,
            span: rustc_span::DUMMY_SP,
            str_id: StrId::from_raw(0),
            children: [NodeIndex::NULL; 4],
            multi_start: 0,
            multi_len: 0,
        };
        Self {
            nodes: vec![null_node],
            extra_children: Vec::new(),
            root: NodeIndex::NULL,
            source: String::new(),
        }
    }

    pub fn with_source(mut self, source: String) -> Self {
        self.source = source;
        self
    }

    /// Get a reference to the node at `index`.
    #[inline]
    pub fn node(&self, index: NodeIndex) -> &Node {
        &self.nodes[index.0 as usize]
    }

    /// Get the fixed children of a node (returns only the non-null ones
    /// according to the node's type).
    pub fn fixed_children(&self, index: NodeIndex) -> &[NodeIndex] {
        let node = self.node(index);
        let count = match node.kind.node_type() {
            NodeType::NoChild | NodeType::MultiChildren => 0,
            NodeType::SingleChild | NodeType::SingleWithMulti => 1,
            NodeType::DoubleChildren | NodeType::DoubleWithMulti => 2,
            NodeType::TripleChildren | NodeType::TripleWithMulti => 3,
            NodeType::QuadrupleChildren | NodeType::QuadrupleWithMulti => 4,
        };
        &node.children[..count]
    }

    /// Get the variable-arity children of a node.
    pub fn multi_children(&self, index: NodeIndex) -> &[NodeIndex] {
        let node = self.node(index);
        if node.multi_len == 0 {
            return &[];
        }
        let start = node.multi_start as usize;
        let end = start + node.multi_len as usize;
        &self.extra_children[start..end]
    }

    /// Create a `NodeBuilder` to construct a new node.
    pub fn builder(&mut self, kind: NodeKind, span: Span) -> NodeBuilder<'_> {
        NodeBuilder {
            ast: self,
            kind,
            span,
            str_id: StrId::from_raw(0),
            children: [NodeIndex::NULL; 4],
            child_cursor: 0,
            multi: Vec::new(),
        }
    }

    /// Push a pre-built node directly and return its index.
    fn push_node(&mut self, node: Node) -> NodeIndex {
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        NodeIndex(idx)
    }
}

impl Default for Ast {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// NodeBuilder – ergonomic construction of AST nodes
// ---------------------------------------------------------------------------

/// Builder for constructing a single AST node, mirroring the Zig NodeBuilder
/// pattern.
pub struct NodeBuilder<'a> {
    ast: &'a mut Ast,
    kind: NodeKind,
    span: Span,
    str_id: StrId,
    children: [NodeIndex; 4],
    child_cursor: usize,
    multi: Vec<NodeIndex>,
}

impl<'a> NodeBuilder<'a> {
    /// Set the interned string id for this node.
    pub fn set_str_id(&mut self, id: StrId) -> &mut Self {
        self.str_id = id;
        self
    }

    /// Add a fixed-arity child.
    pub fn add_child(&mut self, child: NodeIndex) -> &mut Self {
        debug_assert!(
            self.child_cursor < 4,
            "too many fixed children for {:?}",
            self.kind
        );
        self.children[self.child_cursor] = child;
        self.child_cursor += 1;
        self
    }

    /// Set variable-arity children.
    pub fn add_multi_children(&mut self, children: &[NodeIndex]) -> &mut Self {
        self.multi.extend_from_slice(children);
        self
    }

    /// Finalize the builder and return the new node's index.
    pub fn build(&mut self) -> NodeIndex {
        let multi_start = self.ast.extra_children.len() as u32;
        let multi_len = self.multi.len() as u32;
        self.ast.extra_children.append(&mut self.multi);

        let node = Node {
            kind: self.kind,
            span: self.span,
            str_id: self.str_id,
            children: self.children,
            multi_start,
            multi_len,
        };
        self.ast.push_node(node)
    }
}

// Re-export dump functionality
pub use dump::dump_ast;
pub use dump::dump_ast_to_string;
