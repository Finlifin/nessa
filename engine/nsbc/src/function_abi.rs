//! Explicit physical entry layout, independent of the source function type.

use type_pool::TypeIndex;

/// A slot in a closure environment, before the function's user parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureAbi {
    Value,
    TraitProof { view: TypeIndex },
}

/// Expansion of one source parameter into physical entry slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterAbi {
    Value,
    /// The proof precedes its receiver data, as specified by trait dispatch.
    Trait {
        view: TypeIndex,
    },
    /// A specialized default method has concrete Self in its source signature,
    /// but receives the caller's frozen interface proof before that data.
    TraitSelf {
        view: TypeIndex,
    },
}

/// Captures and logical parameters are explicitly separated. A legacy function
/// has no descriptor; loading it must not invent proof slots from count gaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionAbi {
    pub captures: Vec<CaptureAbi>,
    pub parameters: Vec<ParameterAbi>,
}

impl FunctionAbi {
    pub fn capture_count(&self) -> usize {
        self.captures.len()
    }

    pub fn logical_parameter_count(&self) -> usize {
        self.parameters.len()
    }

    pub fn physical_parameter_count(&self) -> usize {
        self.captures.len()
            + self
                .parameters
                .iter()
                .map(|parameter| match parameter {
                    ParameterAbi::Value => 1,
                    ParameterAbi::Trait { .. } | ParameterAbi::TraitSelf { .. } => 2,
                })
                .sum::<usize>()
    }

    pub fn has_trait_proofs(&self) -> bool {
        self.captures
            .iter()
            .any(|capture| matches!(capture, CaptureAbi::TraitProof { .. }))
            || self.parameters.iter().any(|parameter| {
                matches!(
                    parameter,
                    ParameterAbi::Trait { .. } | ParameterAbi::TraitSelf { .. }
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_slots_do_not_become_closure_captures() {
        let view = TypeIndex::from_raw(42);
        let abi = FunctionAbi {
            captures: vec![CaptureAbi::Value, CaptureAbi::TraitProof { view }],
            parameters: vec![
                ParameterAbi::Value,
                ParameterAbi::Trait { view },
                ParameterAbi::Trait { view },
            ],
        };
        assert_eq!(abi.capture_count(), 2);
        assert_eq!(abi.logical_parameter_count(), 3);
        assert_eq!(abi.physical_parameter_count(), 7);
        assert!(abi.has_trait_proofs());
    }
}
