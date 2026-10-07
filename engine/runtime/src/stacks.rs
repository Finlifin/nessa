//! Delimiter-owned stacks. Capture and resume change links, never copy frames.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nsbc::{FuncId, Reg};
use stack_pool::{StackHandle, StackPool};

use crate::{CallFrame, EffectHandler, RegisterFile, TaggedValue};

/// A prompt identifies a delimiter, with nearest matching prompt semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptId(pub u32);

/// An internal execution handle, scoped to one task's stack collection.
/// This is a linear ABI handle, not the language's multi-shot Continuation value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContinuationId(u64);

static NEXT_CONTINUATION: AtomicU64 = AtomicU64::new(0);

impl ContinuationId {
    /// ABI representation. Foreign or consumed handles are rejected on lookup.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn from_u64(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackError {
    MissingPrompt(PromptId),
    InvalidContinuation,
    InvalidRegister(Reg),
    TooManyArguments,
    HandleSpaceExhausted,
}

/// Registers and frames are local to a stack segment and stay at stable addresses.
#[derive(Clone)]
pub struct StackContext {
    pub display_state: Option<crate::DisplayState>,
    /// Environment of the segment's entry function, retained across capture.
    /// Nested calls use their immediate call frame's environment instead.
    pub entry_closure_env: Option<TaggedValue>,
    pub registers: RegisterFile,
    /// Local values for the current function, including spilled temporaries.
    pub local_slots: Vec<TaggedValue>,
    pub call_stack: Vec<CallFrame>,
    pub pc: u32,
    pub current_func: FuncId,
    pub handler_stack: Vec<EffectHandler>,
}

impl StackContext {
    fn new(func_id: FuncId) -> Self {
        Self {
            display_state: None,
            entry_closure_env: None,
            registers: RegisterFile::new(),
            local_slots: Vec::new(),
            call_stack: Vec::new(),
            pc: 0,
            current_func: func_id,
            handler_stack: Vec::new(),
        }
    }
}

/// Owns a pool slot and returns it exactly once, including during task drop.
struct OwnedStack {
    handle: StackHandle,
    pool: Arc<StackPool>,
}

impl Drop for OwnedStack {
    fn drop(&mut self) {
        self.pool.dealloc(&self.handle);
    }
}

struct Segment {
    context: StackContext,
    parent: Option<usize>,
    prompts: Vec<PromptId>,
    allocation: Option<OwnedStack>,
}

struct CapturedStack {
    head: usize,
    boundary: usize,
    resume_register: Reg,
    language_owned: bool,
}

/// All attached and detached stacks owned by a task.
///
/// A segment is boxed so growing the arena cannot relocate GC root slots.
/// Capturing detaches the link below the matching delimiter; resuming reconnects
/// that link to the caller. Nested delimiters travel with the captured chain.
pub struct TaskStacks {
    segments: Vec<Option<Box<Segment>>>,
    vacant: Vec<usize>,
    active: usize,
    captured: HashMap<ContinuationId, CapturedStack>,
    pool: Option<Arc<StackPool>>,
}

impl TaskStacks {
    fn next_handle() -> Result<ContinuationId, StackError> {
        // Never reuse a handle, including across tasks.
        NEXT_CONTINUATION
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                (id < (1 << 57) - 1).then_some(id + 1)
            })
            .map(ContinuationId)
            .map_err(|_| StackError::HandleSpaceExhausted)
    }

    pub fn new(func_id: FuncId) -> Self {
        Self {
            segments: vec![Some(Box::new(Segment {
                context: StackContext::new(func_id),
                parent: None,
                prompts: Vec::new(),
                allocation: None,
            }))],
            active: 0,
            vacant: Vec::new(),
            captured: HashMap::new(),
            pool: None,
        }
    }

    /// Install the pool before running or entering any delimiters.
    pub fn with_pool(func_id: FuncId, pool: Arc<StackPool>) -> Self {
        let mut stacks = Self::new(func_id);
        stacks.segment_mut(0).allocation = Some(OwnedStack {
            handle: pool.alloc(),
            pool: Arc::clone(&pool),
        });
        stacks.pool = Some(pool);
        stacks
    }

    fn segment(&self, index: usize) -> &Segment {
        // Only internal links access arena indices; removed segments are unlinked.
        self.segments[index].as_deref().expect("live stack link")
    }

    fn segment_mut(&mut self, index: usize) -> &mut Segment {
        self.segments[index]
            .as_deref_mut()
            .expect("live stack link")
    }

    fn insert_segment(&mut self, segment: Segment) -> usize {
        let segment = Some(Box::new(segment));
        if let Some(index) = self.vacant.pop() {
            self.segments[index] = segment;
            index
        } else {
            let index = self.segments.len();
            self.segments.push(segment);
            index
        }
    }

    fn remove_segment(&mut self, index: usize) -> Box<Segment> {
        let segment = self.segments[index].take().expect("live stack segment");
        self.vacant.push(index);
        segment
    }

    pub fn active(&self) -> &StackContext {
        &self.segment(self.active).context
    }

    pub fn active_mut(&mut self) -> &mut StackContext {
        &mut self.segment_mut(self.active).context
    }

    /// Nearest handler across active delimiter segments and their parents.
    pub fn find_handler(&self, effect: type_pool::TypeIndex) -> Option<&EffectHandler> {
        let mut index = self.active;
        loop {
            let segment = self.segment(index);
            if let Some(handler) = segment
                .context
                .handler_stack
                .iter()
                .rev()
                .find(|handler| handler.effect_type == effect)
            {
                return Some(handler);
            }
            index = segment.parent?;
        }
    }

    pub fn effective_handlers(&self) -> Vec<EffectHandler> {
        let mut handlers = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(self.active);
        while let Some(index) = current {
            let segment = self.segment(index);
            for handler in segment.context.handler_stack.iter().rev() {
                if seen.insert(handler.effect_type) {
                    handlers.push(handler.clone());
                }
            }
            current = segment.parent;
        }
        // Retain only the visible binding for each effect. Repeated ancestor
        // snapshots would otherwise grow the environment at every nesting.
        handlers.reverse();
        handlers
    }

    /// Enumerate all owned contexts, including conditionally rooted language templates.
    /// Use `root_contexts` for unconditional GC roots.
    pub fn contexts(&self) -> impl Iterator<Item = &StackContext> {
        self.segments
            .iter()
            .filter_map(|segment| segment.as_deref().map(|segment| &segment.context))
    }

    fn conditional_segments(&self) -> HashSet<usize> {
        let mut conditional = HashSet::new();
        for captured in self
            .captured
            .values()
            .filter(|capture| capture.language_owned)
        {
            let mut current = captured.head;
            loop {
                conditional.insert(current);
                if current == captured.boundary {
                    break;
                }
                current = self
                    .segment(current)
                    .parent
                    .expect("captured chain reaches boundary");
            }
        }
        conditional
    }

    /// Enumerate active contexts and raw or not-yet-published captures.
    /// Language-owned detached chains are traced only when their owners are reachable.
    pub fn root_contexts(&self) -> impl Iterator<Item = &StackContext> {
        let conditional = self.conditional_segments();
        self.segments
            .iter()
            .enumerate()
            .filter_map(move |(index, segment)| {
                if conditional.contains(&index) {
                    None
                } else {
                    segment.as_deref().map(|segment| &segment.context)
                }
            })
    }

    /// Mutable slots for the same unconditional roots as `root_contexts`.
    /// Segment boxes and their links stay fixed throughout this iteration.
    pub fn root_contexts_mut(&mut self) -> impl Iterator<Item = &mut StackContext> {
        let conditional = self.conditional_segments();
        self.segments
            .iter_mut()
            .enumerate()
            .filter_map(move |(index, segment)| {
                if conditional.contains(&index) {
                    None
                } else {
                    segment.as_deref_mut().map(|segment| &mut segment.context)
                }
            })
    }

    /// Transfer a pending capture to a published language continuation owner.
    /// The caller must arrange conditional tracing before removing its unconditional roots.
    pub fn promote_language_capture(&mut self, id: ContinuationId) -> Result<(), StackError> {
        self.captured
            .get_mut(&id)
            .ok_or(StackError::InvalidContinuation)?
            .language_owned = true;
        Ok(())
    }

    pub fn has_capture(&self, id: ContinuationId) -> bool {
        self.captured.contains_key(&id)
    }

    pub fn captured_count(&self) -> usize {
        self.captured.len()
    }

    /// Visit a captured chain from its innermost context through its delimiter boundary.
    /// Contexts stay in their original boxes; the visitor cannot change stack links.
    pub fn visit_captured_contexts_mut(
        &mut self,
        id: ContinuationId,
        mut visitor: impl FnMut(&mut StackContext),
    ) -> Result<(), StackError> {
        let captured = self
            .captured
            .get(&id)
            .ok_or(StackError::InvalidContinuation)?;
        let mut current = captured.head;
        let boundary = captured.boundary;
        loop {
            let segment = self.segment_mut(current);
            visitor(&mut segment.context);
            if current == boundary {
                break;
            }
            current = segment.parent.expect("captured chain reaches boundary");
        }
        Ok(())
    }

    /// Start a delimited computation on its own stack with explicit arguments.
    pub fn enter_delimiter(
        &mut self,
        prompt: PromptId,
        body: FuncId,
        args: &[TaggedValue],
    ) -> Result<(), StackError> {
        self.enter_delimiters(&[prompt], body, args)
    }

    /// One elimination scope can delimit several captured effects.
    pub fn enter_delimiters(
        &mut self,
        prompts: &[PromptId],
        body: FuncId,
        args: &[TaggedValue],
    ) -> Result<(), StackError> {
        if args.len() > crate::GP_REGISTER_COUNT {
            return Err(StackError::TooManyArguments);
        }
        let mut context = StackContext::new(body);
        context.display_state = self.active().display_state.clone().map(|mut state| {
            state.current_target = false;
            state.inline_count = 0;
            state
        });
        context.registers.regs[..args.len()].copy_from_slice(args);
        let allocation = self.pool.as_ref().map(|pool| OwnedStack {
            handle: pool.alloc(),
            pool: Arc::clone(pool),
        });
        let index = self.insert_segment(Segment {
            context,
            parent: Some(self.active),
            prompts: prompts.to_vec(),
            allocation,
        });
        self.active = index;
        Ok(())
    }

    /// Detach the chain through the nearest matching delimiter.
    /// The parent becomes active; the suspended computation is not relocated.
    pub fn capture(
        &mut self,
        prompt: PromptId,
        resume_register: Reg,
    ) -> Result<ContinuationId, StackError> {
        if resume_register.0 as usize >= crate::GP_REGISTER_COUNT {
            return Err(StackError::InvalidRegister(resume_register));
        }
        let mut boundary = self.active;
        while !self.segment(boundary).prompts.contains(&prompt) {
            boundary = self
                .segment(boundary)
                .parent
                .ok_or(StackError::MissingPrompt(prompt))?;
        }
        // The root has no prompt, so every matching delimiter has a parent.
        let parent = self
            .segment(boundary)
            .parent
            .expect("attached delimiter parent");
        // TaggedValue's unsigned immediate payload is 57 bits. Never reuse IDs,
        // even between tasks, so a foreign handle cannot alias a local capture.
        let id = Self::next_handle()?;
        self.captured.insert(
            id,
            CapturedStack {
                head: self.active,
                boundary,
                resume_register,
                language_owned: false,
            },
        );
        self.segment_mut(boundary).parent = None;
        self.active = parent;
        Ok(id)
    }

    /// Consume an ABI handle and switch to its suspended chain without copying.
    /// The completed delimiter returns its result to this caller's r0.
    pub fn resume(&mut self, id: ContinuationId, value: TaggedValue) -> Result<(), StackError> {
        let captured = self
            .captured
            .remove(&id)
            .ok_or(StackError::InvalidContinuation)?;
        self.segment_mut(captured.boundary).parent = Some(self.active);
        self.active = captured.head;
        self.active_mut()
            .registers
            .set(captured.resume_register, value);
        Ok(())
    }

    /// Explicitly branch a suspended computation for multi-shot language calls.
    /// Only this operation duplicates VM frames and allocates branch stack slots;
    /// capture and resume themselves always transfer the existing chain.
    /// A new branch is unconditionally rooted until independently promoted.
    pub fn fork(&mut self, id: ContinuationId) -> Result<ContinuationId, StackError> {
        let captured = self
            .captured
            .get(&id)
            .ok_or(StackError::InvalidContinuation)?;
        let resume_register = captured.resume_register;
        let mut chain = vec![captured.head];
        while *chain.last().expect("nonempty captured chain") != captured.boundary {
            chain.push(
                self.segment(*chain.last().expect("nonempty captured chain"))
                    .parent
                    .expect("captured chain reaches boundary"),
            );
        }
        let new_id = Self::next_handle()?;
        let mut parent = None;
        let mut boundary = None;
        for source in chain.into_iter().rev() {
            let context = self.segment(source).context.clone();
            let prompts = self.segment(source).prompts.clone();
            let allocation = self.pool.as_ref().map(|pool| OwnedStack {
                handle: pool.alloc(),
                pool: Arc::clone(pool),
            });
            let index = self.insert_segment(Segment {
                context,
                parent,
                prompts,
                allocation,
            });
            boundary.get_or_insert(index);
            parent = Some(index);
        }
        self.captured.insert(
            new_id,
            CapturedStack {
                head: parent.expect("forked captured head"),
                boundary: boundary.expect("forked captured boundary"),
                resume_register,
                language_owned: false,
            },
        );
        Ok(new_id)
    }

    /// Return from a delimiter after its local call frames have been unwound.
    /// Returns false at the task root or while an ordinary call is still active.
    pub fn return_from_delimiter(&mut self, value: TaggedValue) -> bool {
        let segment = self.segment(self.active);
        if !segment.context.call_stack.is_empty() {
            return false;
        }
        let Some(parent) = segment.parent else {
            return false;
        };
        self.remove_segment(self.active);
        self.active = parent;
        self.active_mut().registers.set(Reg(0), value);
        true
    }

    /// Abandon a captured chain and return all its stack slots to the pool.
    pub fn discard(&mut self, id: ContinuationId) -> Result<(), StackError> {
        let captured = self
            .captured
            .remove(&id)
            .ok_or(StackError::InvalidContinuation)?;
        let mut current = captured.head;
        loop {
            let segment = self.remove_segment(current);
            if current == captured.boundary {
                break;
            }
            current = segment.parent.expect("nested captured delimiter parent");
        }
        Ok(())
    }

    /// Release every pool slot on finish/cancel, retaining the root result for inspection.
    pub fn finish(&mut self) {
        self.captured.clear();
        for segment in self.segments.iter_mut().skip(1) {
            segment.take();
        }
        self.segments.truncate(1);
        self.vacant.clear();
        let root = self.segment_mut(0);
        root.allocation.take();
        root.context.call_stack.clear();
        root.context.local_slots = Vec::new();
        root.context.handler_stack.clear();
        root.context.entry_closure_env = None;
        root.context.display_state = None;
        self.active = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stacks() -> (TaskStacks, Arc<StackPool>) {
        let pool = Arc::new(StackPool::new());
        (TaskStacks::with_pool(FuncId(0), Arc::clone(&pool)), pool)
    }

    fn root_functions(stacks: &TaskStacks) -> Vec<FuncId> {
        stacks
            .root_contexts()
            .map(|context| context.current_func)
            .collect()
    }

    #[test]
    fn language_ownership_excludes_only_its_detached_chain() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        stacks.enter_delimiter(PromptId(2), FuncId(2), &[]).unwrap();
        let managed = stacks.capture(PromptId(1), Reg(0)).unwrap();
        assert_eq!(
            root_functions(&stacks),
            vec![FuncId(0), FuncId(1), FuncId(2)]
        );
        stacks.promote_language_capture(managed).unwrap();
        stacks.promote_language_capture(managed).unwrap();
        assert_eq!(root_functions(&stacks), vec![FuncId(0)]);
        assert_eq!(stacks.contexts().count(), 3);
        assert_eq!(pool.active_count(), 3);

        stacks.enter_delimiter(PromptId(3), FuncId(3), &[]).unwrap();
        let raw = stacks.capture(PromptId(3), Reg(0)).unwrap();
        stacks.enter_delimiter(PromptId(4), FuncId(4), &[]).unwrap();
        assert_eq!(
            root_functions(&stacks),
            vec![FuncId(0), FuncId(3), FuncId(4)]
        );
        assert!(stacks.has_capture(managed));
        assert!(stacks.has_capture(raw));
        assert_eq!(stacks.captured_count(), 2);
        stacks.discard(managed).unwrap();
        assert_eq!(pool.active_count(), 3);
        assert_eq!(
            root_functions(&stacks),
            vec![FuncId(0), FuncId(3), FuncId(4)]
        );
        stacks.discard(raw).unwrap();
        assert_eq!(pool.active_count(), 2);
        stacks.finish();
        assert_eq!(pool.active_count(), 0);
        assert_eq!(stacks.captured_count(), 0);
    }

    #[test]
    fn conditional_context_visit_preserves_addresses_and_nested_order() {
        let (mut stacks, _) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        let outer = stacks.active() as *const StackContext;
        stacks.enter_delimiter(PromptId(2), FuncId(2), &[]).unwrap();
        let inner = stacks.active() as *const StackContext;
        let managed = stacks.capture(PromptId(1), Reg(5)).unwrap();
        stacks.promote_language_capture(managed).unwrap();
        let mut visited = Vec::new();
        stacks
            .visit_captured_contexts_mut(managed, |context| {
                visited.push((context.current_func, context as *const StackContext));
                context.local_slots = vec![TaggedValue::from_i64(42)];
            })
            .unwrap();
        assert_eq!(visited, vec![(FuncId(2), inner), (FuncId(1), outer)]);
        assert!(stacks.active().local_slots.is_empty());
        stacks.resume(managed, TaggedValue::from_i64(7)).unwrap();
        assert!(!stacks.has_capture(managed));
        assert_eq!(stacks.captured_count(), 0);
        assert_eq!(
            stacks.promote_language_capture(managed),
            Err(StackError::InvalidContinuation)
        );
        assert_eq!(
            stacks.visit_captured_contexts_mut(managed, |_| panic!("consumed capture visited")),
            Err(StackError::InvalidContinuation)
        );
        assert_eq!(
            root_functions(&stacks),
            vec![FuncId(0), FuncId(1), FuncId(2)]
        );
        assert_eq!(stacks.active() as *const StackContext, inner);
        assert_eq!(stacks.active().local_slots[0].as_i64(), Some(42));
        assert_eq!(stacks.active().registers.get(Reg(5)).as_i64(), Some(7));
        assert!(stacks.return_from_delimiter(TaggedValue::UNIT));
        assert_eq!(stacks.active() as *const StackContext, outer);
        assert_eq!(stacks.active().local_slots[0].as_i64(), Some(42));
    }

    #[test]
    fn forks_are_unconditional_until_their_own_publication() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        let managed = stacks.capture(PromptId(1), Reg(0)).unwrap();
        stacks.promote_language_capture(managed).unwrap();
        let pending = stacks.fork(managed).unwrap();
        let raw_fork = stacks.fork(pending).unwrap();
        assert_eq!(pool.active_count(), 4);
        assert_eq!(
            root_functions(&stacks),
            vec![FuncId(0), FuncId(1), FuncId(1)]
        );
        stacks.promote_language_capture(pending).unwrap();
        assert_eq!(root_functions(&stacks), vec![FuncId(0), FuncId(1)]);
        stacks.resume(raw_fork, TaggedValue::TRUE).unwrap();
        assert_eq!(root_functions(&stacks), vec![FuncId(0), FuncId(1)]);
        assert_eq!(stacks.captured_count(), 2);
        assert!(stacks.return_from_delimiter(TaggedValue::UNIT));
        assert_eq!(pool.active_count(), 3);
        stacks.discard(pending).unwrap();
        stacks.discard(managed).unwrap();
        assert_eq!(pool.active_count(), 1);
        assert_eq!(root_functions(&stacks), vec![FuncId(0)]);
    }

    #[test]
    fn invalid_or_finished_captures_do_not_visit_contexts() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        let managed = stacks.capture(PromptId(1), Reg(0)).unwrap();
        stacks.promote_language_capture(managed).unwrap();
        let (mut foreign, _) = self::stacks();
        assert_eq!(
            foreign.promote_language_capture(managed),
            Err(StackError::InvalidContinuation)
        );
        assert_eq!(
            foreign.visit_captured_contexts_mut(managed, |_| panic!("foreign capture visited")),
            Err(StackError::InvalidContinuation)
        );
        assert!(!foreign.has_capture(managed));
        stacks.finish();
        stacks.finish();
        assert_eq!(pool.active_count(), 0);
        assert_eq!(stacks.captured_count(), 0);
        assert!(!stacks.has_capture(managed));
        assert_eq!(
            stacks.promote_language_capture(managed),
            Err(StackError::InvalidContinuation)
        );
        assert_eq!(
            stacks.visit_captured_contexts_mut(managed, |_| panic!("finished capture visited")),
            Err(StackError::InvalidContinuation)
        );
        assert_eq!(root_functions(&stacks), vec![FuncId(0)]);
        drop(stacks);
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn capture_and_resume_preserve_context_and_frame_storage() {
        let (mut stacks, pool) = stacks();
        stacks.active_mut().pc = 12;
        stacks
            .active_mut()
            .registers
            .set(Reg(20), TaggedValue::from_i64(99));
        stacks.enter_delimiter(PromptId(7), FuncId(1), &[]).unwrap();
        stacks.active_mut().pc = 42;
        stacks.active_mut().local_slots = vec![TaggedValue::from_i64(77)];
        stacks.active_mut().call_stack.push(CallFrame {
            display_state: None,
            return_pc: 9,
            func_id: FuncId(2),
            saved_regs: vec![(Reg(20), TaggedValue::from_i64(123))],
            local_slots: vec![TaggedValue::from_i64(88)],
            evidence: vec![],
            closure_env: None,
        });
        let context_address = stacks.active() as *const StackContext;
        let frame_address = stacks.active().call_stack.as_ptr();
        let locals_address = stacks.active().local_slots.as_ptr();
        let saved_locals_address = stacks.active().call_stack[0].local_slots.as_ptr();
        let slot = stacks
            .segment(stacks.active)
            .allocation
            .as_ref()
            .unwrap()
            .handle
            .id;
        let continuation = stacks.capture(PromptId(7), Reg(3)).unwrap();
        assert_eq!(stacks.active().pc, 12);
        assert_eq!(stacks.active().registers.get(Reg(20)).as_i64(), Some(99));
        assert_eq!(pool.active_count(), 2);

        stacks
            .resume(continuation, TaggedValue::from_i64(5))
            .unwrap();
        assert_eq!(stacks.active() as *const StackContext, context_address);
        assert_eq!(stacks.active().call_stack.as_ptr(), frame_address);
        assert_eq!(stacks.active().local_slots.as_ptr(), locals_address);
        assert_eq!(
            stacks.active().call_stack[0].local_slots.as_ptr(),
            saved_locals_address
        );
        assert_eq!(stacks.active().local_slots[0].as_i64(), Some(77));
        assert_eq!(
            stacks.active().call_stack[0].local_slots[0].as_i64(),
            Some(88)
        );
        assert_eq!(
            stacks
                .segment(stacks.active)
                .allocation
                .as_ref()
                .unwrap()
                .handle
                .id,
            slot
        );
        assert_eq!(stacks.active().pc, 42);
        assert_eq!(stacks.active().registers.get(Reg(3)).as_i64(), Some(5));
        assert!(!stacks.return_from_delimiter(TaggedValue::UNIT));
        stacks.active_mut().call_stack.pop();
        assert!(stacks.return_from_delimiter(TaggedValue::from_i64(6)));
        assert_eq!(stacks.active().current_func, FuncId(0));
        assert_eq!(stacks.active().registers.get(Reg(0)).as_i64(), Some(6));
        assert_eq!(pool.active_count(), 1);
    }

    #[test]
    fn outer_prompt_capture_transfers_nested_segments() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        let outer = stacks.active() as *const StackContext;
        stacks.enter_delimiter(PromptId(2), FuncId(2), &[]).unwrap();
        let inner = stacks.active() as *const StackContext;
        let k = stacks.capture(PromptId(1), Reg(0)).unwrap();
        assert_eq!(stacks.active().current_func, FuncId(0));
        assert_eq!(stacks.contexts().count(), 3);
        stacks.resume(k, TaggedValue::TRUE).unwrap();
        assert_eq!(stacks.active() as *const StackContext, inner);
        assert!(stacks.return_from_delimiter(TaggedValue::from_i64(2)));
        assert_eq!(stacks.active() as *const StackContext, outer);
        assert!(stacks.return_from_delimiter(TaggedValue::from_i64(3)));
        assert_eq!(stacks.active().registers.get(Reg(0)).as_i64(), Some(3));
        assert_eq!(pool.active_count(), 1);
    }

    #[test]
    fn repeated_prompt_matches_nearest_delimiter() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        stacks.enter_delimiter(PromptId(1), FuncId(2), &[]).unwrap();
        let k = stacks.capture(PromptId(1), Reg(0)).unwrap();
        assert_eq!(stacks.active().current_func, FuncId(1));
        stacks.discard(k).unwrap();
        assert_eq!(pool.active_count(), 2);
        assert!(stacks.return_from_delimiter(TaggedValue::UNIT));
        assert_eq!(pool.active_count(), 1);
    }

    #[test]
    fn invalid_operations_preserve_active_state() {
        let (mut stacks, pool) = stacks();
        assert_eq!(
            stacks.capture(PromptId(9), Reg(0)),
            Err(StackError::MissingPrompt(PromptId(9)))
        );
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        assert_eq!(
            stacks.capture(PromptId(1), Reg(32)),
            Err(StackError::InvalidRegister(Reg(32)))
        );
        assert_eq!(stacks.active().current_func, FuncId(1));
        assert_eq!(pool.active_count(), 2);
        let k = stacks.capture(PromptId(1), Reg(0)).unwrap();
        let (mut other, _) = self::stacks();
        assert_eq!(
            other.resume(k, TaggedValue::UNIT),
            Err(StackError::InvalidContinuation)
        );
        stacks.resume(k, TaggedValue::UNIT).unwrap();
        assert_eq!(
            stacks.resume(k, TaggedValue::UNIT),
            Err(StackError::InvalidContinuation)
        );
        assert_eq!(stacks.active().current_func, FuncId(1));
    }

    #[test]
    fn discard_releases_entire_chain_and_invalidates_handle() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        stacks.enter_delimiter(PromptId(2), FuncId(2), &[]).unwrap();
        let k = stacks.capture(PromptId(1), Reg(0)).unwrap();
        stacks.discard(k).unwrap();
        assert_eq!(pool.active_count(), 1);
        assert_eq!(stacks.contexts().count(), 1);
        assert_eq!(stacks.discard(k), Err(StackError::InvalidContinuation));
    }

    #[test]
    fn fork_has_independent_registers_frames_and_stack_slots() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        stacks.active_mut().call_stack.push(CallFrame {
            display_state: None,
            return_pc: 4,
            func_id: FuncId(1),
            saved_regs: vec![(Reg(20), TaggedValue::from_i64(10))],
            local_slots: vec![TaggedValue::from_i64(30)],
            evidence: vec![],
            closure_env: None,
        });
        stacks.active_mut().local_slots = vec![TaggedValue::from_i64(40)];
        let original_locals = stacks.active().local_slots.as_ptr();
        let original_saved_locals = stacks.active().call_stack[0].local_slots.as_ptr();
        let original = stacks.active() as *const StackContext;
        let original_slot = stacks
            .segment(stacks.active)
            .allocation
            .as_ref()
            .unwrap()
            .handle
            .id;
        let k = stacks.capture(PromptId(1), Reg(0)).unwrap();
        let fork = stacks.fork(k).unwrap();
        assert_eq!(pool.active_count(), 3);
        stacks.resume(fork, TaggedValue::from_i64(2)).unwrap();
        assert_ne!(stacks.active() as *const StackContext, original);
        assert_ne!(stacks.active().local_slots.as_ptr(), original_locals);
        assert_ne!(
            stacks.active().call_stack[0].local_slots.as_ptr(),
            original_saved_locals
        );
        stacks.active_mut().local_slots[0] = TaggedValue::from_i64(400);
        stacks.active_mut().call_stack[0].local_slots[0] = TaggedValue::from_i64(300);
        assert_ne!(
            stacks
                .segment(stacks.active)
                .allocation
                .as_ref()
                .unwrap()
                .handle
                .id,
            original_slot
        );
        stacks.active_mut().call_stack[0].saved_regs[0].1 = TaggedValue::from_i64(20);
        stacks.active_mut().call_stack.clear();
        assert!(stacks.return_from_delimiter(TaggedValue::from_i64(20)));
        stacks.resume(k, TaggedValue::from_i64(1)).unwrap();
        assert_eq!(stacks.active() as *const StackContext, original);
        assert_eq!(stacks.active().local_slots[0].as_i64(), Some(40));
        assert_eq!(
            stacks.active().call_stack[0].local_slots[0].as_i64(),
            Some(30)
        );
        assert_eq!(
            stacks.active().call_stack[0].saved_regs[0].1.as_i64(),
            Some(10)
        );
        assert_eq!(stacks.active().registers.get(Reg(0)).as_i64(), Some(1));
        stacks.active_mut().call_stack.clear();
        assert!(stacks.return_from_delimiter(TaggedValue::from_i64(10)));
        assert_eq!(pool.active_count(), 1);
    }

    #[test]
    fn finish_and_drop_release_attached_and_captured_slots_once() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        stacks.capture(PromptId(1), Reg(0)).unwrap();
        stacks.enter_delimiter(PromptId(2), FuncId(2), &[]).unwrap();
        assert_eq!(pool.active_count(), 3);
        stacks.finish();
        stacks.finish();
        assert_eq!(pool.active_count(), 0);
        drop(stacks);
        assert_eq!(pool.active_count(), 0);

        let mut stacks = TaskStacks::with_pool(FuncId(0), Arc::clone(&pool));
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        stacks.capture(PromptId(1), Reg(0)).unwrap();
        drop(stacks);
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn suspended_registers_and_frames_remain_gc_roots() {
        let (mut stacks, _) = stacks();
        stacks
            .enter_delimiter(PromptId(1), FuncId(1), &[TaggedValue::from_i64(8)])
            .unwrap();
        let register_slot = &stacks.active().registers.regs[0] as *const TaggedValue;
        let k = stacks.capture(PromptId(1), Reg(0)).unwrap();
        assert!(
            stacks
                .contexts()
                .any(|context| { std::ptr::eq(&context.registers.regs[0], register_slot) })
        );
        stacks.discard(k).unwrap();
        assert!(
            !stacks
                .contexts()
                .any(|context| { std::ptr::eq(&context.registers.regs[0], register_slot) })
        );
    }

    #[test]
    fn completed_delimiters_reuse_metadata_without_aliasing_old_handles() {
        let (mut stacks, pool) = stacks();
        stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
        let stale = stacks.capture(PromptId(1), Reg(0)).unwrap();
        stacks.discard(stale).unwrap();
        for _ in 0..100 {
            stacks.enter_delimiter(PromptId(1), FuncId(1), &[]).unwrap();
            assert_eq!(
                stacks.resume(stale, TaggedValue::UNIT),
                Err(StackError::InvalidContinuation)
            );
            assert!(stacks.return_from_delimiter(TaggedValue::UNIT));
        }
        assert_eq!(stacks.segments.len(), 2);
        assert_eq!(pool.active_count(), 1);
    }

    #[test]
    fn retained_handlers_do_not_accumulate_shadowed_ancestor_bindings() {
        let (mut stacks, _) = stacks();
        let effect = type_pool::TypeIndex::from_raw(100);
        stacks.active_mut().handler_stack.push(EffectHandler {
            effect_type: effect,
            handler_func: FuncId(1),
            is_async: false,
            closure_env: None,
            continuation_param: None,
        });
        for depth in 2..20 {
            let handlers = stacks.effective_handlers();
            assert_eq!(handlers.len(), 1);
            stacks
                .enter_delimiter(PromptId(depth), FuncId(depth), &[])
                .unwrap();
            stacks.active_mut().handler_stack = handlers;
            stacks.active_mut().handler_stack.push(EffectHandler {
                effect_type: effect,
                handler_func: FuncId(depth),
                is_async: false,
                closure_env: None,
                continuation_param: None,
            });
            let retained = stacks.effective_handlers();
            assert_eq!(retained.len(), 1);
            assert_eq!(retained[0].handler_func, FuncId(depth));
        }
    }
}
