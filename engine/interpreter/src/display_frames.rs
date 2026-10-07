//! Frame-owned Display traversal survives capture without shared mutable paths.

use nsbc::{FuncId, ParameterAbi};
use runtime::{DisplayState, TaggedValue, TaskId};
use type_pool::TypeKind;

use crate::{Vm, VmError};

pub(super) const MAX_DISPLAY_BYTES: usize = 1024 * 1024;
const MAX_DISPLAY_DEPTH: usize = 128;

impl Vm {
    pub(super) fn display_call_state(
        &self,
        task: TaskId,
        target: FuncId,
        arguments: &[TaggedValue],
    ) -> Result<(Option<DisplayState>, bool), VmError> {
        let code = self.checked_function(target)?;
        let mut state = self
            .roots
            .scheduler
            .get_task(task)
            .ok_or(VmError::InvalidTask)?
            .display_state
            .clone();
        if code.display_owner.is_some() && state.is_none() {
            state = Some(DisplayState::default());
        }
        let Some(mut state) = state else {
            return Ok((None, false));
        };
        state.current_target = false;
        state.inline_count = 0;
        let display = self.state.type_pool.well_known.display;
        let name = str_interner::intern("to_string");
        let owner = code.display_owner.or_else(|| {
            self.state
                .type_pool
                .vtables_snapshot()
                .iter()
                .find(|table| {
                    self.state
                        .type_pool
                        .trait_schema(table.trait_type)
                        .is_some_and(|schema| {
                            schema
                                .slots
                                .iter()
                                .zip(&table.entries)
                                .any(|(key, &function)| {
                                    key.trait_owner == display
                                        && key.name == name
                                        && function == target.0
                                })
                        })
                })
                .map(|table| table.implementor)
        });
        let Some(owner) = owner else {
            return Ok((Some(state), false));
        };
        let owner = self
            .state
            .type_pool
            .canonical_type(owner)
            .ok_or(VmError::InvalidType(owner))?;
        let data_index = match code.abi.as_ref().and_then(|abi| abi.parameters.first()) {
            Some(ParameterAbi::Value) => code.abi.as_ref().unwrap().capture_count(),
            Some(ParameterAbi::TraitSelf { .. }) => code.abi.as_ref().unwrap().capture_count() + 1,
            _ => return Err(VmError::UnsupportedFunctionAbi),
        };
        let receiver = *arguments.get(data_index).ok_or(VmError::TypeError)?;
        if self.reflected_type(receiver)? != owner
            || !self.value_matches_type(receiver, owner, 0)?
        {
            return Err(VmError::TypeError);
        }
        let aggregate = matches!(
            self.state.type_pool.get(owner).kind,
            TypeKind::Struct { .. } | TypeKind::Tuple { .. } | TypeKind::Enum { .. }
        );
        if aggregate && receiver.is_heap() && state.ancestors.contains(&receiver) {
            state.current_target = true;
            return Ok((Some(state), true));
        }
        if state.depth >= MAX_DISPLAY_DEPTH {
            return Err(VmError::DisplayDepthExceeded);
        }
        state.depth += 1;
        state.current_target = true;
        if aggregate && receiver.is_heap() {
            state.ancestors.push(receiver);
        }
        Ok((Some(state), false))
    }

    pub(super) fn check_display_return(
        &self,
        task: TaskId,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        if self.roots.scheduler.get_task(task).is_some_and(|task| {
            task.display_state
                .as_ref()
                .is_some_and(|state| state.current_target)
        }) {
            let text = crate::builtin_ctx::heap_string_to_owned(value).ok_or(VmError::TypeError)?;
            self.check_display_bytes(task, text.len())?;
        }
        Ok(())
    }

    pub(super) fn check_display_bytes(&self, task: TaskId, bytes: usize) -> Result<(), VmError> {
        if bytes > MAX_DISPLAY_BYTES
            && self
                .roots
                .scheduler
                .get_task(task)
                .is_some_and(|task| task.display_state.is_some())
        {
            return Err(VmError::DisplaySizeExceeded);
        }
        Ok(())
    }

    pub(super) fn display_inline_tuple(
        &mut self,
        task: TaskId,
        value: TaggedValue,
        enter: bool,
    ) -> Result<bool, VmError> {
        self.tuple_values(value)?;
        let context = self
            .roots
            .scheduler
            .get_task(task)
            .ok_or(VmError::InvalidTask)?;
        if self
            .checked_function(context.current_func)?
            .display_owner
            .is_none()
        {
            return Err(VmError::TypeError);
        }
        let context = self
            .roots
            .scheduler
            .get_task_mut(task)
            .ok_or(VmError::InvalidTask)?;
        let state = context.display_state.as_mut().ok_or(VmError::TypeError)?;
        if !state.current_target {
            return Err(VmError::TypeError);
        }
        if enter {
            if state.ancestors.contains(&value) {
                return Ok(false);
            }
            if state.depth >= MAX_DISPLAY_DEPTH {
                return Err(VmError::DisplayDepthExceeded);
            }
            state.depth += 1;
            state.inline_count += 1;
            state.ancestors.push(value);
        } else {
            if state.inline_count == 0 || state.ancestors.last() != Some(&value) {
                return Err(VmError::TypeError);
            }
            state.inline_count -= 1;
            state.depth -= 1;
            state.ancestors.pop();
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_size_limit_is_local_to_an_active_traversal() {
        let mut vm = crate::tests::make_vm();
        let task = vm.spawn_root(FuncId(0));
        assert!(vm.check_display_bytes(task, MAX_DISPLAY_BYTES + 1).is_ok());
        vm.roots.scheduler.get_task_mut(task).unwrap().display_state =
            Some(DisplayState::default());
        assert!(vm.check_display_bytes(task, MAX_DISPLAY_BYTES).is_ok());
        assert!(matches!(
            vm.check_display_bytes(task, MAX_DISPLAY_BYTES + 1),
            Err(VmError::DisplaySizeExceeded)
        ));
        vm.roots.scheduler.get_task_mut(task).unwrap().display_state = None;
        assert!(vm.check_display_bytes(task, MAX_DISPLAY_BYTES + 1).is_ok());
    }

    #[test]
    fn ordinary_helpers_inherit_independent_paths_without_becoming_display_targets() {
        let mut vm = crate::tests::make_vm();
        vm.add_function(runtime::FunctionCode {
            display_owner: None,
            abi: Some(nsbc::FunctionAbi {
                captures: Vec::new(),
                parameters: Vec::new(),
            }),
            func_id: FuncId(0),
            instructions: Vec::new(),
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task = vm.spawn_root(FuncId(0));
        let original = DisplayState {
            depth: 127,
            ancestors: vec![TaggedValue::UNIT],
            current_target: true,
            inline_count: 1,
        };
        vm.roots.scheduler.get_task_mut(task).unwrap().display_state = Some(original);
        let (state, cycle) = vm.display_call_state(task, FuncId(0), &[]).unwrap();
        let mut state = state.unwrap();
        assert!(!cycle);
        assert_eq!(state.depth, 127);
        assert!(!state.current_target);
        assert_eq!(state.inline_count, 0);
        state.ancestors.clear();
        let parent = vm
            .roots
            .scheduler
            .get_task(task)
            .unwrap()
            .display_state
            .as_ref()
            .unwrap();
        assert_eq!(parent.ancestors, [TaggedValue::UNIT]);
        assert_eq!(parent.inline_count, 1);
    }
}
