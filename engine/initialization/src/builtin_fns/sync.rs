//! `std.sync` native builtins (`805..=846`).

use interpreter::{BuiltinCtx, VmError};

// -- Channel (`805..=812`) ----------------------------------------------------

/// `805` `__sync_chan_new(capacity: i64) -> i64`
pub fn chan_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let cap = ctx.arg_i64(0)?;
    ctx.sync_chan_new(cap)
}

/// `806` `__sync_chan_send(id: i64, val: Any) -> i64`
pub fn chan_send(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let val = ctx.arg_rooted(1)?;
    ctx.sync_chan_send(id, &val)
}

/// `807` `__sync_chan_try_send(id: i64, val: Any) -> i64`
pub fn chan_try_send(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let val = ctx.arg_rooted(1)?;
    ctx.sync_chan_try_send(id, &val)
}

/// `808` `__sync_chan_recv(id: i64) -> (i64, Any)`
pub fn chan_recv(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_chan_recv(id)
}

/// `809` `__sync_chan_try_recv(id: i64) -> (i64, Any)`
pub fn chan_try_recv(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_chan_try_recv(id)
}

/// `810` `__sync_chan_close(id: i64) -> Unit`
pub fn chan_close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_chan_close(id)
}

/// `811` `__sync_chan_is_closed(id: i64) -> bool`
pub fn chan_is_closed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_chan_is_closed(id)
}

/// `812` `__sync_chan_len(id: i64) -> i64`
pub fn chan_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_chan_len(id)
}

// -- Mutex (`813..=819`) ------------------------------------------------------

/// `813` `__sync_mutex_new(initial: Any) -> i64`
pub fn mutex_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg_rooted(0)?;
    ctx.sync_mutex_new(&val)
}

/// `814` `__sync_mutex_lock(id: i64) -> Any`
pub fn mutex_lock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_mutex_lock(id)
}

/// `815` `__sync_mutex_try_lock(id: i64) -> bool`
pub fn mutex_try_lock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_mutex_try_lock(id)
}

/// `816` `__sync_mutex_unlock(id: i64) -> Unit`
pub fn mutex_unlock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_mutex_unlock(id)
}

/// `817` `__sync_mutex_get(id: i64) -> Any`
pub fn mutex_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_mutex_get(id)
}

/// `818` `__sync_mutex_set(id: i64, val: Any) -> Unit`
pub fn mutex_set(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let val = ctx.arg_rooted(1)?;
    ctx.sync_mutex_set(id, &val)
}

/// `819` `__sync_mutex_is_locked(id: i64) -> bool`
pub fn mutex_is_locked(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_mutex_is_locked(id)
}

// -- RwLock (`820..=828`) -----------------------------------------------------

/// `820` `__sync_rwlock_new(initial: Any) -> i64`
pub fn rwlock_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg_rooted(0)?;
    ctx.sync_rwlock_new(&val)
}

/// `821` `__sync_rwlock_read_lock(id: i64) -> Any`
pub fn rwlock_read_lock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_rwlock_read_lock(id)
}

/// `822` `__sync_rwlock_try_read_lock(id: i64) -> bool`
pub fn rwlock_try_read_lock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_rwlock_try_read_lock(id)
}

/// `823` `__sync_rwlock_read_unlock(id: i64) -> Unit`
pub fn rwlock_read_unlock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_rwlock_read_unlock(id)
}

/// `824` `__sync_rwlock_write_lock(id: i64) -> Any`
pub fn rwlock_write_lock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_rwlock_write_lock(id)
}

/// `825` `__sync_rwlock_try_write_lock(id: i64) -> bool`
pub fn rwlock_try_write_lock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_rwlock_try_write_lock(id)
}

/// `826` `__sync_rwlock_write_unlock(id: i64) -> Unit`
pub fn rwlock_write_unlock(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_rwlock_write_unlock(id)
}

/// `827` `__sync_rwlock_get(id: i64) -> Any`
pub fn rwlock_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_rwlock_get(id)
}

/// `828` `__sync_rwlock_set(id: i64, val: Any) -> Unit`
pub fn rwlock_set(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let val = ctx.arg_rooted(1)?;
    ctx.sync_rwlock_set(id, &val)
}

// -- Semaphore (`829..=833`) --------------------------------------------------

/// `829` `__sync_sem_new(permits: i64) -> i64`
pub fn sem_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let permits = ctx.arg_i64(0)?;
    ctx.sync_sem_new(permits)
}

/// `830` `__sync_sem_acquire(id: i64) -> Unit`
pub fn sem_acquire(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_sem_acquire(id)
}

/// `831` `__sync_sem_try_acquire(id: i64) -> bool`
pub fn sem_try_acquire(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_sem_try_acquire(id)
}

/// `832` `__sync_sem_release(id: i64, count: i64) -> Unit`
pub fn sem_release(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let count = ctx.arg_i64(1)?;
    ctx.sync_sem_release(id, count)
}

/// `833` `__sync_sem_available(id: i64) -> i64`
pub fn sem_available(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_sem_available(id)
}

// -- Once / Lazy (`834..=838`) ------------------------------------------------

/// `834` `__sync_once_new() -> i64`
pub fn once_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.sync_once_new()
}

/// `835` `__sync_once_begin(id: i64) -> (i64, Any)`
pub fn once_begin(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_once_begin(id)
}

/// `836` `__sync_once_complete(id: i64, val: Any) -> Unit`
pub fn once_complete(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let val = ctx.arg_rooted(1)?;
    ctx.sync_once_complete(id, &val)
}

/// `837` `__sync_once_abort(id: i64) -> Unit`
pub fn once_abort(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_once_abort(id)
}

/// `838` `__sync_once_get(id: i64) -> (bool, Any)`
pub fn once_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_once_get(id)
}

// -- Barrier (`839..=840`) ----------------------------------------------------

/// `839` `__sync_barrier_new(parties: i64) -> i64`
pub fn barrier_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let parties = ctx.arg_i64(0)?;
    ctx.sync_barrier_new(parties)
}

/// `840` `__sync_barrier_wait(id: i64) -> bool`
pub fn barrier_wait(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_barrier_wait(id)
}

// -- Lock-Free Atomics (`841..=846`) ------------------------------------------

/// `841` `__sync_atomic_new(initial: i64) -> i64`
pub fn atomic_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let initial = ctx.arg_i64(0)?;
    ctx.sync_atomic_new(initial)
}

/// `842` `__sync_atomic_load(id: i64) -> i64`
pub fn atomic_load(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.sync_atomic_load(id)
}

/// `843` `__sync_atomic_store(id: i64, val: i64) -> Unit`
pub fn atomic_store(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let val = ctx.arg_i64(1)?;
    ctx.sync_atomic_store(id, val)
}

/// `844` `__sync_atomic_swap(id: i64, val: i64) -> i64`
pub fn atomic_swap(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let val = ctx.arg_i64(1)?;
    ctx.sync_atomic_swap(id, val)
}

/// `845` `__sync_atomic_fetch_add(id: i64, delta: i64) -> i64`
pub fn atomic_fetch_add(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let id = ctx.arg_i64(0)?;
    let delta = ctx.arg_i64(1)?;
    ctx.sync_atomic_fetch_add(id, delta)
}

/// `846` `__sync_atomic_cas(id: i64, expected: i64, desired: i64) -> (bool, i64)`
pub fn atomic_cas(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let id = ctx.arg_i64(0)?;
    let expected = ctx.arg_i64(1)?;
    let desired = ctx.arg_i64(2)?;
    ctx.sync_atomic_cas(id, expected, desired)
}
