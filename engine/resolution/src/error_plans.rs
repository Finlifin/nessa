//! Checked Error facts shared with lowering. Types are finalized before code generation.
use ast::NodeIndex;
use type_pool::TypeIndex;

#[derive(Clone, Debug)]
pub enum ErrorTagSource {
    Static(TypeIndex),
    DynamicPayload,
}
#[derive(Clone, Debug)]
pub struct ErrorConstructionPlan {
    pub operand: NodeIndex,
    pub result: TypeIndex,
    pub tag: ErrorTagSource,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorConversionKind {
    LiftOk,
    MapQualified,
    CheckQualified,
}
#[derive(Clone, Debug)]
pub struct ErrorConversionPlan {
    pub source: TypeIndex,
    pub target: TypeIndex,
    pub kind: ErrorConversionKind,
}
#[derive(Clone, Debug)]
pub struct ErrorPropagationPlan {
    pub operand: NodeIndex,
    pub input: TypeIndex,
    pub success: TypeIndex,
    pub callable: NodeIndex,
    pub return_target: TypeIndex,
    pub error_conversion: ErrorConversionPlan,
}
#[derive(Clone, Debug)]
pub struct ErrorEliminationPlan {
    pub operand: NodeIndex,
    pub input: TypeIndex,
    pub result: TypeIndex,
    pub residual: TypeIndex,
    pub arms: Vec<NodeIndex>,
    pub implicit_success_conversion: Option<ErrorConversionPlan>,
    pub residual_conversion: Option<ErrorConversionPlan>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorPatternBranch {
    Ok,
    Error,
}
#[derive(Clone, Debug)]
pub struct ErrorPatternPlan {
    pub branch: ErrorPatternBranch,
    pub payload_type: TypeIndex,
    pub type_test: Option<TypeIndex>,
}
