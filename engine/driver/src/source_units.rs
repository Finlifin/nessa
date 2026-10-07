//! Diagnostics for checked assembly of independently parsed source units.
use ast::{Ast, AstStorageLimits, NodeIndex};
use diagnostic::{Diagnostic, DiagnosticContext};

use crate::Driver;

impl Driver {
    pub(crate) fn append_source_units(
        &self,
        destination: &mut Ast,
        sources: Vec<Ast>,
    ) -> Result<Vec<NodeIndex>, Vec<Diagnostic>> {
        destination
            .try_append_asts_with_limits(sources, AstStorageLimits::default())
            .map_err(|error| {
                let context = DiagnosticContext::new(&self.source_map);
                context
                    .error(format!("cannot assemble source units: {error}"))
                    .emit(&context);
                context.diagnostics().to_vec()
            })
    }
}
