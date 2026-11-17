//! Host-owned VM resources: Xoshiro256** PRNG and buffered file handle table.
//!
//! Host OS handles (`File`), buffers, and PRNG words live in `VmState` rather
//! than GC-managed heap objects so `NessaScanning` only ever traces `TaggedValue`
//! words. Nessa code addresses open files through `i64` handle IDs.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::VmError;

/// Standardized I/O status codes returned in builtin status tuples.
pub const IO_OK: i64 = 0;
pub const IO_NOT_FOUND: i64 = 1;
pub const IO_PERMISSION_DENIED: i64 = 2;
pub const IO_ALREADY_EXISTS: i64 = 3;
pub const IO_INVALID_INPUT: i64 = 4;
pub const IO_UNEXPECTED_EOF: i64 = 5;
pub const IO_CLOSED: i64 = 6;
pub const IO_BROKEN_PIPE: i64 = 7;
pub const IO_OTHER: i64 = 8;

pub const STD_STDIN: i64 = 0;
pub const STD_STDOUT: i64 = 1;
pub const STD_STDERR: i64 = 2;

const READ_BUF_CAPACITY: usize = 8192;
/// Maximum bytes read by unbounded `FILE_READ_BYTES` (`List` max capacity).
const MAX_LIST_BYTES: usize = gc::MAX_LARGE_TAGGED_WORDS;
/// Maximum bytes read by `FILE_READ_ALL` (`String` max UTF-8 byte capacity).
const MAX_STRING_BYTES: usize = gc::MAX_EXTERNAL_RC_BYTES;

/// Map a `std::io::Error` to Nessa's standardized integer `IoError` code.
pub fn io_error_code(err: &io::Error) -> i64 {
    match err.kind() {
        io::ErrorKind::NotFound => IO_NOT_FOUND,
        io::ErrorKind::PermissionDenied => IO_PERMISSION_DENIED,
        io::ErrorKind::AlreadyExists => IO_ALREADY_EXISTS,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => IO_INVALID_INPUT,
        io::ErrorKind::UnexpectedEof => IO_UNEXPECTED_EOF,
        io::ErrorKind::BrokenPipe => IO_BROKEN_PIPE,
        _ => IO_OTHER,
    }
}

pub(crate) fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

pub(crate) fn fresh_entropy_pair() -> (i64, i64) {
    static COUNTER: AtomicU64 = AtomicU64::new(1);

    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x517c_c1b7_2722_0a95);
    let pid = u64::from(std::process::id());
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stack_addr = (&now_nanos as *const u64 as usize) as u64;

    let mut mix_a = now_nanos ^ pid.rotate_left(17) ^ seq.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut mix_b = stack_addr ^ (now_nanos >> 32) ^ seq.rotate_left(31);

    #[cfg(unix)]
    {
        if let Ok(mut urandom) = File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if urandom.read_exact(&mut buf).is_ok() {
                let r0 = u64::from_le_bytes([
                    buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                ]);
                let r1 = u64::from_le_bytes([
                    buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
                ]);
                mix_a ^= r0;
                mix_b ^= r1;
            }
        }
    }

    let s1 = splitmix64_next(&mut mix_a) as i64;
    let s2 = splitmix64_next(&mut mix_b) as i64;
    (s1, s2)
}

// ---------------------------------------------------------------------------
// VmRng — 256-bit Xoshiro256** PRNG
// ---------------------------------------------------------------------------

pub struct VmRng {
    state: [u64; 4],
}

impl VmRng {
    pub fn new() -> Self {
        let (s1, s2) = fresh_entropy_pair();
        let mut rng = Self {
            state: [1, 0, 0, 0],
        };
        rng.seed_pair(s1, s2);
        rng
    }

    fn seed_pair(&mut self, s1: i64, s2: i64) {
        let mut sm1 = s1 as u64;
        let mut sm2 = (s2 as u64) ^ 0xd1b5_4a32_d192_ed03;
        self.state = [
            splitmix64_next(&mut sm1),
            splitmix64_next(&mut sm2),
            splitmix64_next(&mut sm1),
            splitmix64_next(&mut sm2),
        ];
        if self.state == [0, 0, 0, 0] {
            self.state[0] = 0x9e37_79b9_7f4a_7c15;
        }
        for _ in 0..16 {
            let _ = self.next_u64();
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.state[1] << 17;

        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];

        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);

        result
    }

    pub fn seed(&mut self, mode: i64, s1: i64, s2: i64) -> Result<(i64, i64), VmError> {
        match mode {
            0 => {
                let (e1, e2) = fresh_entropy_pair();
                self.seed_pair(e1, e2);
                Ok((e1, e2))
            }
            1 => {
                self.seed_pair(s1, 0);
                Ok((s1, 0))
            }
            2 => {
                self.seed_pair(s1, s2);
                Ok((s1, s2))
            }
            _ => Err(VmError::InvalidArgument("invalid rng seed mode")),
        }
    }
}

impl Default for VmRng {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ManagedFile & VmFileTable — synchronized buffered read/write/seek handles
// ---------------------------------------------------------------------------

struct ManagedFile {
    file: File,
    buf: Vec<u8>,
    pos: usize,
    cap: usize,
    can_read: bool,
    can_write: bool,
    append: bool,
    #[cfg(not(unix))]
    unlink_on_drop: Option<PathBuf>,
}

#[cfg(not(unix))]
impl Drop for ManagedFile {
    fn drop(&mut self) {
        if let Some(path) = self.unlink_on_drop.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl ManagedFile {
    fn new(
        file: File,
        can_read: bool,
        can_write: bool,
        append: bool,
        #[cfg_attr(unix, allow(unused_variables))] unlink_path: Option<PathBuf>,
    ) -> Self {
        Self {
            file,
            buf: Vec::new(),
            pos: 0,
            cap: 0,
            can_read,
            can_write,
            append,
            #[cfg(not(unix))]
            unlink_on_drop: unlink_path,
        }
    }

    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.pos >= self.cap {
            if self.buf.len() < READ_BUF_CAPACITY {
                self.buf.resize(READ_BUF_CAPACITY, 0);
            }
            let n = loop {
                match self.file.read(&mut self.buf) {
                    Ok(n) => break n,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(err) => return Err(err),
                }
            };
            self.pos = 0;
            self.cap = n;
        }
        Ok(&self.buf[self.pos..self.cap])
    }

    fn consume(&mut self, amt: usize) {
        self.pos = (self.pos + amt).min(self.cap);
    }

    /// Rewind the OS file descriptor by any unread buffered bytes before writing.
    fn sync_before_write(&mut self) -> io::Result<()> {
        if self.pos < self.cap {
            let unread = (self.cap - self.pos) as i64;
            if !self.append {
                self.file.seek(SeekFrom::Current(-unread))?;
            }
            self.pos = 0;
            self.cap = 0;
        }
        Ok(())
    }

    fn flush(&mut self) -> (i64, String) {
        if !self.can_write {
            return (IO_OK, String::new());
        }
        match self.file.flush() {
            Ok(()) => (IO_OK, String::new()),
            Err(err) => (io_error_code(&err), err.to_string()),
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> (i64, String) {
        if !self.can_write {
            return (IO_INVALID_INPUT, "file is not open for writing".to_string());
        }
        if let Err(err) = self.sync_before_write() {
            return (io_error_code(&err), err.to_string());
        }
        match self.file.write_all(bytes) {
            Ok(()) => (IO_OK, String::new()),
            Err(err) => (io_error_code(&err), err.to_string()),
        }
    }

    fn seek(&mut self, whence: i64, offset: i64) -> (i64, i64, String) {
        let target = match whence {
            0 => {
                let Ok(start) = u64::try_from(offset) else {
                    return (
                        IO_INVALID_INPUT,
                        -1,
                        "negative seek offset from start".to_string(),
                    );
                };
                SeekFrom::Start(start)
            }
            1 => {
                let unread = (self.cap - self.pos) as i64;
                let Some(adjusted) = offset.checked_sub(unread) else {
                    return (IO_INVALID_INPUT, -1, "seek offset overflow".to_string());
                };
                SeekFrom::Current(adjusted)
            }
            2 => SeekFrom::End(offset),
            _ => return (IO_INVALID_INPUT, -1, "invalid seek whence".to_string()),
        };

        match self.file.seek(target) {
            Ok(new_pos) => {
                self.pos = 0;
                self.cap = 0;
                match i64::try_from(new_pos) {
                    Ok(pos) => (IO_OK, pos, String::new()),
                    Err(_) => (IO_OTHER, -1, "file offset exceeds i64::MAX".to_string()),
                }
            }
            Err(err) => (io_error_code(&err), -1, err.to_string()),
        }
    }
}

struct VmFileTableInner {
    next_id: i64,
    files: HashMap<i64, Arc<Mutex<ManagedFile>>>,
}

pub struct VmFileTable {
    inner: Mutex<VmFileTableInner>,
}

impl Default for VmFileTable {
    fn default() -> Self {
        Self::new()
    }
}

impl VmFileTable {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VmFileTableInner {
                next_id: 3,
                files: HashMap::new(),
            }),
        }
    }

    fn alloc_id(&self, managed: ManagedFile) -> i64 {
        let arc = Arc::new(Mutex::new(managed));
        let mut inner = self.inner.lock();
        let id = inner.next_id;
        inner.next_id = inner
            .next_id
            .checked_add(1)
            .expect("file handle ID overflow");
        inner.files.insert(id, arc);
        id
    }

    fn lookup_file(&self, id: i64) -> Option<Arc<Mutex<ManagedFile>>> {
        self.inner.lock().files.get(&id).cloned()
    }

    pub fn is_open(&self, id: i64) -> bool {
        (STD_STDIN..=STD_STDERR).contains(&id) || self.inner.lock().files.contains_key(&id)
    }

    pub fn close(&self, id: i64) -> (i64, String) {
        if (STD_STDIN..=STD_STDERR).contains(&id) {
            return (IO_INVALID_INPUT, "cannot close standard stream".to_string());
        }
        let removed = self.inner.lock().files.remove(&id);
        match removed {
            Some(file_lock) => {
                let mut managed = file_lock.lock();
                if managed.can_write
                    && let Err(err) = managed.file.flush()
                {
                    return (io_error_code(&err), err.to_string());
                }
                (IO_OK, String::new())
            }
            None => (IO_CLOSED, "closed file".to_string()),
        }
    }

    pub fn open(&self, path: &str, mode: &str) -> (i64, i64, String) {
        let mut opts = OpenOptions::new();
        let (can_read, can_write, append) = match mode {
            "r" | "rb" => {
                opts.read(true);
                (true, false, false)
            }
            "w" | "wb" => {
                opts.write(true).create(true).truncate(true);
                (false, true, false)
            }
            "a" | "ab" => {
                opts.append(true).create(true);
                (false, true, true)
            }
            "r+" | "rb+" | "r+b" => {
                opts.read(true).write(true);
                (true, true, false)
            }
            "w+" | "wb+" | "w+b" => {
                opts.read(true).write(true).create(true).truncate(true);
                (true, true, false)
            }
            "a+" | "ab+" | "a+b" => {
                opts.read(true).append(true).create(true);
                (true, true, true)
            }
            "wx" | "wbx" | "wxb" => {
                opts.write(true).create_new(true);
                (false, true, false)
            }
            "w+x" | "wb+x" | "wx+" | "wbx+" => {
                opts.read(true).write(true).create_new(true);
                (true, true, false)
            }
            _ => {
                return (
                    IO_INVALID_INPUT,
                    -1,
                    format!("invalid file open mode: {mode:?}"),
                );
            }
        };

        match opts.open(path) {
            Ok(file) => {
                let id = self.alloc_id(ManagedFile::new(file, can_read, can_write, append, None));
                (IO_OK, id, String::new())
            }
            Err(err) => (io_error_code(&err), -1, err.to_string()),
        }
    }

    pub fn create_temp(&self, rng: &Mutex<VmRng>) -> (i64, i64, String) {
        let dir = std::env::temp_dir();
        for _ in 0..64 {
            let suffix = rng.lock().next_u64();
            let path = dir.join(format!("nessa_tmp_{}_{:016x}", std::process::id(), suffix));
            let mut opts = OpenOptions::new();
            opts.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&path) {
                Ok(file) => {
                    #[cfg(unix)]
                    {
                        let _ = std::fs::remove_file(&path);
                        let id = self.alloc_id(ManagedFile::new(file, true, true, false, None));
                        return (IO_OK, id, String::new());
                    }
                    #[cfg(not(unix))]
                    {
                        let id =
                            self.alloc_id(ManagedFile::new(file, true, true, false, Some(path)));
                        return (IO_OK, id, String::new());
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return (io_error_code(&err), -1, err.to_string()),
            }
        }
        (
            IO_ALREADY_EXISTS,
            -1,
            "failed to allocate unique temporary file".to_string(),
        )
    }

    pub fn flush(&self, id: i64) -> (i64, String) {
        match id {
            STD_STDIN => (IO_OK, String::new()),
            STD_STDOUT => match io::stdout().flush() {
                Ok(()) => (IO_OK, String::new()),
                Err(err) => (io_error_code(&err), err.to_string()),
            },
            STD_STDERR => match io::stderr().flush() {
                Ok(()) => (IO_OK, String::new()),
                Err(err) => (io_error_code(&err), err.to_string()),
            },
            _ => match self.lookup_file(id) {
                Some(file_lock) => file_lock.lock().flush(),
                None => (IO_CLOSED, "closed file".to_string()),
            },
        }
    }

    pub fn write_bytes(&self, id: i64, bytes: &[u8]) -> (i64, String) {
        match id {
            STD_STDIN => (
                IO_INVALID_INPUT,
                "standard input is not open for writing".to_string(),
            ),
            STD_STDOUT => {
                let mut out = io::stdout().lock();
                match out.write_all(bytes) {
                    Ok(()) => (IO_OK, String::new()),
                    Err(err) => (io_error_code(&err), err.to_string()),
                }
            }
            STD_STDERR => {
                let mut err_stream = io::stderr().lock();
                match err_stream.write_all(bytes) {
                    Ok(()) => (IO_OK, String::new()),
                    Err(err) => (io_error_code(&err), err.to_string()),
                }
            }
            _ => match self.lookup_file(id) {
                Some(file_lock) => file_lock.lock().write_bytes(bytes),
                None => (IO_CLOSED, "closed file".to_string()),
            },
        }
    }

    pub fn seek(&self, id: i64, whence: i64, offset: i64) -> (i64, i64, String) {
        if (STD_STDIN..=STD_STDERR).contains(&id) {
            return (
                IO_INVALID_INPUT,
                -1,
                "cannot seek standard stream".to_string(),
            );
        }
        match self.lookup_file(id) {
            Some(file_lock) => file_lock.lock().seek(whence, offset),
            None => (IO_CLOSED, -1, "closed file".to_string()),
        }
    }

    pub fn read_all(&self, id: i64) -> Result<(i64, String), VmError> {
        let mut raw = Vec::new();
        match id {
            STD_STDIN => {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                if let Err(err) = read_bounded(&mut lock, MAX_STRING_BYTES, &mut raw)? {
                    return Ok((io_error_code(&err), err.to_string()));
                }
            }
            STD_STDOUT | STD_STDERR => {
                return Ok((
                    IO_INVALID_INPUT,
                    "standard output stream is not open for reading".to_string(),
                ));
            }
            _ => {
                let Some(file_lock) = self.lookup_file(id) else {
                    return Ok((IO_CLOSED, "closed file".to_string()));
                };
                let mut managed = file_lock.lock();
                if !managed.can_read {
                    return Ok((IO_INVALID_INPUT, "file is not open for reading".to_string()));
                }
                loop {
                    let chunk = match managed.fill_buf() {
                        Ok(chunk) => chunk,
                        Err(err) => return Ok((io_error_code(&err), err.to_string())),
                    };
                    if chunk.is_empty() {
                        break;
                    }
                    if raw.len().saturating_add(chunk.len()) > MAX_STRING_BYTES {
                        return Err(VmError::ObjectTooLarge);
                    }
                    raw.extend_from_slice(chunk);
                    let len = chunk.len();
                    managed.consume(len);
                }
            }
        }
        match String::from_utf8(raw) {
            Ok(text) => Ok((IO_OK, text)),
            Err(err) => Ok((IO_INVALID_INPUT, err.to_string())),
        }
    }

    pub fn read_line(
        &self,
        id: i64,
        keep_newline: bool,
    ) -> Result<(i64, Option<String>, String), VmError> {
        let mut raw = Vec::new();
        match id {
            STD_STDIN => {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                loop {
                    let chunk = match lock.fill_buf() {
                        Ok(chunk) => chunk,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => return Ok((io_error_code(&err), None, err.to_string())),
                    };
                    if chunk.is_empty() {
                        break;
                    }
                    if let Some(nl_pos) = chunk.iter().position(|&b| b == b'\n') {
                        let take = nl_pos + 1;
                        if raw.len().saturating_add(take) > MAX_STRING_BYTES {
                            return Err(VmError::ObjectTooLarge);
                        }
                        raw.extend_from_slice(&chunk[..take]);
                        lock.consume(take);
                        break;
                    }
                    let take = chunk.len();
                    if raw.len().saturating_add(take) > MAX_STRING_BYTES {
                        return Err(VmError::ObjectTooLarge);
                    }
                    raw.extend_from_slice(chunk);
                    lock.consume(take);
                }
            }
            STD_STDOUT | STD_STDERR => {
                return Ok((
                    IO_INVALID_INPUT,
                    None,
                    "standard output stream is not open for reading".to_string(),
                ));
            }
            _ => {
                let Some(file_lock) = self.lookup_file(id) else {
                    return Ok((IO_CLOSED, None, "closed file".to_string()));
                };
                let mut managed = file_lock.lock();
                if !managed.can_read {
                    return Ok((
                        IO_INVALID_INPUT,
                        None,
                        "file is not open for reading".to_string(),
                    ));
                }
                loop {
                    let chunk = match managed.fill_buf() {
                        Ok(chunk) => chunk,
                        Err(err) => return Ok((io_error_code(&err), None, err.to_string())),
                    };
                    if chunk.is_empty() {
                        break;
                    }
                    if let Some(nl_pos) = chunk.iter().position(|&b| b == b'\n') {
                        let take = nl_pos + 1;
                        if raw.len().saturating_add(take) > MAX_STRING_BYTES {
                            return Err(VmError::ObjectTooLarge);
                        }
                        raw.extend_from_slice(&chunk[..take]);
                        managed.consume(take);
                        break;
                    }
                    let take = chunk.len();
                    if raw.len().saturating_add(take) > MAX_STRING_BYTES {
                        return Err(VmError::ObjectTooLarge);
                    }
                    raw.extend_from_slice(chunk);
                    managed.consume(take);
                }
            }
        }

        if raw.is_empty() {
            return Ok((IO_OK, None, String::new()));
        }
        if !keep_newline && raw.last() == Some(&b'\n') {
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
        }
        match String::from_utf8(raw) {
            Ok(line) => Ok((IO_OK, Some(line), String::new())),
            Err(err) => Ok((IO_INVALID_INPUT, None, err.to_string())),
        }
    }

    pub fn read_chars(
        &self,
        id: i64,
        count: i64,
    ) -> Result<(i64, Option<String>, String), VmError> {
        if count < 0 {
            return Ok((
                IO_INVALID_INPUT,
                None,
                "character count must not be negative".to_string(),
            ));
        }
        let count = usize::try_from(count).unwrap_or(usize::MAX);

        match id {
            STD_STDIN => {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                read_utf8_scalars(&mut StdinByteSource(&mut lock), count)
            }
            STD_STDOUT | STD_STDERR => Ok((
                IO_INVALID_INPUT,
                None,
                "standard output stream is not open for reading".to_string(),
            )),
            _ => {
                let Some(file_lock) = self.lookup_file(id) else {
                    return Ok((IO_CLOSED, None, "closed file".to_string()));
                };
                let mut managed = file_lock.lock();
                if !managed.can_read {
                    return Ok((
                        IO_INVALID_INPUT,
                        None,
                        "file is not open for reading".to_string(),
                    ));
                }
                read_utf8_scalars(&mut ManagedByteSource(&mut managed), count)
            }
        }
    }

    pub fn read_bytes(&self, id: i64, max_bytes: i64) -> Result<(i64, Vec<u8>, String), VmError> {
        let limit = if max_bytes < 0 {
            None
        } else {
            let requested = usize::try_from(max_bytes).map_err(|_| VmError::ObjectTooLarge)?;
            if requested > MAX_LIST_BYTES {
                return Err(VmError::ObjectTooLarge);
            }
            Some(requested)
        };

        let mut raw = Vec::new();
        match id {
            STD_STDIN => {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                match limit {
                    None => {
                        if let Err(err) = read_bounded(&mut lock, MAX_LIST_BYTES, &mut raw)? {
                            return Ok((io_error_code(&err), Vec::new(), err.to_string()));
                        }
                    }
                    Some(max_n) => {
                        raw.reserve(max_n.min(READ_BUF_CAPACITY));
                        while raw.len() < max_n {
                            let chunk = match lock.fill_buf() {
                                Ok(c) => c,
                                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                                Err(err) => {
                                    return Ok((io_error_code(&err), Vec::new(), err.to_string()));
                                }
                            };
                            if chunk.is_empty() {
                                break;
                            }
                            let take = chunk.len().min(max_n - raw.len());
                            raw.extend_from_slice(&chunk[..take]);
                            lock.consume(take);
                        }
                    }
                }
            }
            STD_STDOUT | STD_STDERR => {
                return Ok((
                    IO_INVALID_INPUT,
                    Vec::new(),
                    "standard output stream is not open for reading".to_string(),
                ));
            }
            _ => {
                let Some(file_lock) = self.lookup_file(id) else {
                    return Ok((IO_CLOSED, Vec::new(), "closed file".to_string()));
                };
                let mut managed = file_lock.lock();
                if !managed.can_read {
                    return Ok((
                        IO_INVALID_INPUT,
                        Vec::new(),
                        "file is not open for reading".to_string(),
                    ));
                }
                match limit {
                    None => loop {
                        let chunk = match managed.fill_buf() {
                            Ok(c) => c,
                            Err(err) => {
                                return Ok((io_error_code(&err), Vec::new(), err.to_string()));
                            }
                        };
                        if chunk.is_empty() {
                            break;
                        }
                        if raw.len().saturating_add(chunk.len()) > MAX_LIST_BYTES {
                            return Err(VmError::ObjectTooLarge);
                        }
                        let take = chunk.len();
                        raw.extend_from_slice(chunk);
                        managed.consume(take);
                    },
                    Some(max_n) => {
                        raw.reserve(max_n.min(READ_BUF_CAPACITY));
                        while raw.len() < max_n {
                            let chunk = match managed.fill_buf() {
                                Ok(c) => c,
                                Err(err) => {
                                    return Ok((io_error_code(&err), Vec::new(), err.to_string()));
                                }
                            };
                            if chunk.is_empty() {
                                break;
                            }
                            let take = chunk.len().min(max_n - raw.len());
                            raw.extend_from_slice(&chunk[..take]);
                            managed.consume(take);
                        }
                    }
                }
            }
        }
        Ok((IO_OK, raw, String::new()))
    }

    pub fn read_number(&self, id: i64) -> (i64, Option<f64>, String) {
        match id {
            STD_STDIN => {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                scan_number_token(&mut StdinByteSource(&mut lock))
            }
            STD_STDOUT | STD_STDERR => (
                IO_INVALID_INPUT,
                None,
                "standard output stream is not open for reading".to_string(),
            ),
            _ => {
                let Some(file_lock) = self.lookup_file(id) else {
                    return (IO_CLOSED, None, "closed file".to_string());
                };
                let mut managed = file_lock.lock();
                if !managed.can_read {
                    return (
                        IO_INVALID_INPUT,
                        None,
                        "file is not open for reading".to_string(),
                    );
                }
                scan_number_token(&mut ManagedByteSource(&mut managed))
            }
        }
    }
}

fn read_bounded<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
    out: &mut Vec<u8>,
) -> Result<io::Result<()>, VmError> {
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Ok(Err(err)),
        };
        if chunk.is_empty() {
            return Ok(Ok(()));
        }
        if out.len().saturating_add(chunk.len()) > max_bytes {
            return Err(VmError::ObjectTooLarge);
        }
        let take = chunk.len();
        out.extend_from_slice(chunk);
        reader.consume(take);
    }
}

trait ByteSource {
    fn peek_byte(&mut self) -> io::Result<Option<u8>>;
    fn consume_one(&mut self);
}

struct ManagedByteSource<'a>(&'a mut ManagedFile);

impl ByteSource for ManagedByteSource<'_> {
    fn peek_byte(&mut self) -> io::Result<Option<u8>> {
        Ok(self.0.fill_buf()?.first().copied())
    }

    fn consume_one(&mut self) {
        self.0.consume(1);
    }
}

struct StdinByteSource<'a, R>(&'a mut R);

impl<R: BufRead> ByteSource for StdinByteSource<'_, R> {
    fn peek_byte(&mut self) -> io::Result<Option<u8>> {
        loop {
            match self.0.fill_buf() {
                Ok(buf) => return Ok(buf.first().copied()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }

    fn consume_one(&mut self) {
        self.0.consume(1);
    }
}

fn utf8_char_width(first: u8) -> Option<usize> {
    match first {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

fn read_utf8_scalars<S: ByteSource>(
    src: &mut S,
    count: usize,
) -> Result<(i64, Option<String>, String), VmError> {
    let first_peek = match src.peek_byte() {
        Ok(b) => b,
        Err(err) => return Ok((io_error_code(&err), None, err.to_string())),
    };
    let Some(_) = first_peek else {
        return Ok((IO_OK, None, String::new()));
    };
    if count == 0 {
        return Ok((IO_OK, Some(String::new()), String::new()));
    }

    let mut raw = Vec::new();
    for _ in 0..count {
        let next = match src.peek_byte() {
            Ok(b) => b,
            Err(err) => return Ok((io_error_code(&err), None, err.to_string())),
        };
        let Some(lead) = next else {
            break;
        };
        let Some(width) = utf8_char_width(lead) else {
            return Ok((
                IO_INVALID_INPUT,
                None,
                "stream did not contain valid UTF-8".to_string(),
            ));
        };
        if raw.len().saturating_add(width) > MAX_STRING_BYTES {
            return Err(VmError::ObjectTooLarge);
        }
        let mut seq = [0u8; 4];
        seq[0] = lead;
        src.consume_one();
        for slot in &mut seq[1..width] {
            match src.peek_byte() {
                Ok(Some(b)) if (b & 0xC0) == 0x80 => {
                    *slot = b;
                    src.consume_one();
                }
                Ok(_) => {
                    return Ok((
                        IO_INVALID_INPUT,
                        None,
                        "stream did not contain valid UTF-8".to_string(),
                    ));
                }
                Err(err) => return Ok((io_error_code(&err), None, err.to_string())),
            }
        }
        if std::str::from_utf8(&seq[..width]).is_err() {
            return Ok((
                IO_INVALID_INPUT,
                None,
                "stream did not contain valid UTF-8".to_string(),
            ));
        }
        raw.extend_from_slice(&seq[..width]);
    }
    // Validated scalar-by-scalar above.
    let text = String::from_utf8(raw).expect("validated UTF-8 scalars");
    Ok((IO_OK, Some(text), String::new()))
}

const MAX_NUM_TOKEN_LEN: usize = 256;

fn scan_number_token<S: ByteSource>(src: &mut S) -> (i64, Option<f64>, String) {
    // Skip leading ASCII whitespace.
    loop {
        match src.peek_byte() {
            Ok(Some(b)) if b.is_ascii_whitespace() => src.consume_one(),
            Ok(Some(_)) => break,
            Ok(None) => return (IO_OK, None, String::new()),
            Err(err) => return (io_error_code(&err), None, err.to_string()),
        }
    }

    let mut buf = Vec::with_capacity(32);
    let push_next = |src: &mut S, buf: &mut Vec<u8>| -> Result<bool, (i64, String)> {
        if buf.len() >= MAX_NUM_TOKEN_LEN {
            return Ok(false);
        }
        match src.peek_byte() {
            Ok(Some(b)) => {
                buf.push(b);
                src.consume_one();
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(err) => Err((io_error_code(&err), err.to_string())),
        }
    };

    // Optional sign.
    if let Ok(Some(b'+' | b'-')) = src.peek_byte()
        && let Err((c, m)) = push_next(src, &mut buf)
    {
        return (c, None, m);
    }

    // Check first significand byte before consuming non-numeric text.
    let first = match src.peek_byte() {
        Ok(Some(b)) => b,
        Ok(None) => return (IO_OK, None, String::new()),
        Err(err) => return (io_error_code(&err), None, err.to_string()),
    };
    if !first.is_ascii_digit() && first != b'.' {
        return (IO_OK, None, String::new());
    }

    let mut is_hex = false;
    if first == b'0' {
        if let Err((c, m)) = push_next(src, &mut buf) {
            return (c, None, m);
        }
        if let Ok(Some(b'x' | b'X')) = src.peek_byte() {
            is_hex = true;
            if let Err((c, m)) = push_next(src, &mut buf) {
                return (c, None, m);
            }
        }
    }

    let is_sig_digit = |b: u8, hex: bool| {
        if hex {
            b.is_ascii_hexdigit()
        } else {
            b.is_ascii_digit()
        }
    };

    let mut has_digits = !is_hex && first == b'0';
    loop {
        match src.peek_byte() {
            Ok(Some(b)) if is_sig_digit(b, is_hex) => {
                has_digits = true;
                if let Err((c, m)) = push_next(src, &mut buf) {
                    return (c, None, m);
                }
                if buf.len() >= MAX_NUM_TOKEN_LEN {
                    break;
                }
            }
            Ok(_) => break,
            Err(err) => return (io_error_code(&err), None, err.to_string()),
        }
    }

    if let Ok(Some(b'.')) = src.peek_byte() {
        if let Err((c, m)) = push_next(src, &mut buf) {
            return (c, None, m);
        }
        loop {
            match src.peek_byte() {
                Ok(Some(b)) if is_sig_digit(b, is_hex) => {
                    has_digits = true;
                    if let Err((c, m)) = push_next(src, &mut buf) {
                        return (c, None, m);
                    }
                    if buf.len() >= MAX_NUM_TOKEN_LEN {
                        break;
                    }
                }
                Ok(_) => break,
                Err(err) => return (io_error_code(&err), None, err.to_string()),
            }
        }
    }

    if !has_digits {
        return (IO_OK, None, String::new());
    }

    let exp_match = match src.peek_byte() {
        Ok(Some(b'p' | b'P')) if is_hex => true,
        Ok(Some(b'e' | b'E')) if !is_hex => true,
        Ok(_) => false,
        Err(err) => return (io_error_code(&err), None, err.to_string()),
    };
    if exp_match {
        if let Err((c, m)) = push_next(src, &mut buf) {
            return (c, None, m);
        }
        if let Ok(Some(b'+' | b'-')) = src.peek_byte()
            && let Err((c, m)) = push_next(src, &mut buf)
        {
            return (c, None, m);
        }
        let mut exp_digits = false;
        loop {
            match src.peek_byte() {
                Ok(Some(b)) if b.is_ascii_digit() => {
                    exp_digits = true;
                    if let Err((c, m)) = push_next(src, &mut buf) {
                        return (c, None, m);
                    }
                    if buf.len() >= MAX_NUM_TOKEN_LEN {
                        break;
                    }
                }
                Ok(_) => break,
                Err(err) => return (io_error_code(&err), None, err.to_string()),
            }
        }
        if !exp_digits {
            return (IO_OK, None, String::new());
        }
    }

    let Ok(token) = std::str::from_utf8(&buf) else {
        return (IO_OK, None, String::new());
    };
    let parsed = if is_hex {
        parse_hex_float(token)
    } else {
        token.parse::<f64>().ok()
    };
    (IO_OK, parsed, String::new())
}

/// Parse a C99 / Lua hexadecimal float/integer literal (`[+-]?0[xX]h*(\.h*)?([pP][+-]?d+)?`).
pub fn parse_hex_float(input: &str) -> Option<f64> {
    let s = input.trim();
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (false, s)
    };
    let rest = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))?;

    let (mant_str, exp_str) = if let Some(idx) = rest.find(['p', 'P']) {
        (&rest[..idx], Some(&rest[idx + 1..]))
    } else {
        (rest, None)
    };

    let (int_part, frac_part) = if let Some(dot) = mant_str.find('.') {
        (&mant_str[..dot], &mant_str[dot + 1..])
    } else {
        (mant_str, "")
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_hexdigit())
        || !frac_part.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }

    let mut exp2: i32 = if let Some(es) = exp_str {
        if es.is_empty() {
            return None;
        }
        es.parse::<i32>().ok()?
    } else {
        0
    };

    // Accumulate up to 16 significant hex digits (64 bits) with sticky bit for exact IEEE rounding.
    let mut sig: u64 = 0;
    let mut sig_digits: usize = 0;
    let mut saw_nonzero = false;
    let mut sticky = false;

    for ch in int_part.chars() {
        let d = ch.to_digit(16)? as u64;
        if !saw_nonzero && d == 0 {
            continue;
        }
        saw_nonzero = true;
        if sig_digits < 15 {
            sig = (sig << 4) | d;
            sig_digits += 1;
        } else {
            if d != 0 {
                sticky = true;
            }
            exp2 = exp2.saturating_add(4);
        }
    }

    for ch in frac_part.chars() {
        let d = ch.to_digit(16)? as u64;
        if !saw_nonzero && d == 0 {
            exp2 = exp2.saturating_sub(4);
            continue;
        }
        saw_nonzero = true;
        if sig_digits < 15 {
            sig = (sig << 4) | d;
            sig_digits += 1;
            exp2 = exp2.saturating_sub(4);
        } else if d != 0 {
            sticky = true;
        }
    }

    if !saw_nonzero {
        return Some(if neg { -0.0 } else { 0.0 });
    }
    if sticky {
        sig = (sig << 1) | 1;
        exp2 = exp2.saturating_sub(1);
    }

    let val = ldexp_f64(sig as f64, i64::from(exp2));
    Some(if neg { -val } else { val })
}

// ---------------------------------------------------------------------------
// VmResourceTable — Monotonic type-tagged host resource registry
// ---------------------------------------------------------------------------

type ResourceCleanup = Box<dyn FnOnce(Box<dyn std::any::Any + Send>) + Send>;

struct ResourceEntry {
    type_tag: u64,
    payload: Option<Box<dyn std::any::Any + Send>>,
    cleanup: Option<ResourceCleanup>,
}

/// Monotonic, type-tagged host resource registry stored in `VmState`.
///
/// Handle IDs start at `1` and strictly increase (`i64`), never reusing closed IDs.
/// Closing an already-closed handle (`1 <= id < next_id`) returns `Ok(false)` cleanly;
/// forged/never-allocated IDs (`id <= 0` or `id >= next_id`) and mismatched `type_tag`s
/// return `Err(VmError::InvalidArgument(...))`. Dropping the VM cleans up all live entries.
pub struct VmResourceTable {
    next_id: i64,
    entries: HashMap<i64, ResourceEntry>,
}

impl Default for VmResourceTable {
    fn default() -> Self {
        Self::new()
    }
}

impl VmResourceTable {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            entries: HashMap::new(),
        }
    }

    pub fn insert<T: Send + 'static>(&mut self, type_tag: u64, value: T) -> Result<i64, VmError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(VmError::InvalidArgument("resource handle table exhausted"))?;
        self.entries.insert(
            id,
            ResourceEntry {
                type_tag,
                payload: Some(Box::new(value)),
                cleanup: None,
            },
        );
        Ok(id)
    }

    pub fn insert_with_cleanup<T: Send + 'static>(
        &mut self,
        type_tag: u64,
        value: T,
        cleanup: impl FnOnce(T) + Send + 'static,
    ) -> Result<i64, VmError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(VmError::InvalidArgument("resource handle table exhausted"))?;
        self.entries.insert(
            id,
            ResourceEntry {
                type_tag,
                payload: Some(Box::new(value)),
                cleanup: Some(Box::new(move |boxed| {
                    if let Ok(typed) = boxed.downcast::<T>() {
                        cleanup(*typed);
                    }
                })),
            },
        );
        Ok(id)
    }

    pub fn get<T: 'static>(&self, id: i64, expected_tag: u64) -> Result<&T, VmError> {
        if id <= 0 || id >= self.next_id {
            return Err(VmError::InvalidArgument("invalid resource handle"));
        }
        let entry = self
            .entries
            .get(&id)
            .ok_or(VmError::InvalidArgument("resource handle is closed"))?;
        if entry.type_tag != expected_tag {
            return Err(VmError::InvalidArgument("resource type tag mismatch"));
        }
        entry
            .payload
            .as_deref()
            .and_then(|any| (any as &dyn std::any::Any).downcast_ref::<T>())
            .ok_or(VmError::InvalidArgument("resource payload type mismatch"))
    }

    pub fn get_mut<T: 'static>(&mut self, id: i64, expected_tag: u64) -> Result<&mut T, VmError> {
        if id <= 0 || id >= self.next_id {
            return Err(VmError::InvalidArgument("invalid resource handle"));
        }
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or(VmError::InvalidArgument("resource handle is closed"))?;
        if entry.type_tag != expected_tag {
            return Err(VmError::InvalidArgument("resource type tag mismatch"));
        }
        entry
            .payload
            .as_deref_mut()
            .and_then(|any| (any as &mut dyn std::any::Any).downcast_mut::<T>())
            .ok_or(VmError::InvalidArgument("resource payload type mismatch"))
    }

    pub fn close(&mut self, id: i64, expected_tag: Option<u64>) -> Result<bool, VmError> {
        if id <= 0 || id >= self.next_id {
            return Err(VmError::InvalidArgument("invalid resource handle"));
        }
        let Some(entry) = self.entries.get(&id) else {
            return Ok(false);
        };
        if let Some(tag) = expected_tag
            && entry.type_tag != tag
        {
            return Err(VmError::InvalidArgument("resource type tag mismatch"));
        }
        let mut removed = self.entries.remove(&id).expect("checked entry exists");
        if let (Some(cleanup), Some(payload)) = (removed.cleanup.take(), removed.payload.take()) {
            cleanup(payload);
        }
        Ok(true)
    }

    pub fn close_tags(&mut self, id: i64, allowed_tags: &[u64]) -> Result<bool, VmError> {
        if id <= 0 || id >= self.next_id {
            return Err(VmError::InvalidArgument("invalid resource handle"));
        }
        let Some(entry) = self.entries.get(&id) else {
            return Ok(false);
        };
        if !allowed_tags.contains(&entry.type_tag) {
            return Err(VmError::InvalidArgument("resource type tag mismatch"));
        }
        let mut removed = self.entries.remove(&id).expect("checked entry exists");
        if let (Some(cleanup), Some(payload)) = (removed.cleanup.take(), removed.payload.take()) {
            cleanup(payload);
        }
        Ok(true)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Drop for VmResourceTable {
    fn drop(&mut self) {
        let mut ids: Vec<i64> = self.entries.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            if let Some(mut entry) = self.entries.remove(&id)
                && let (Some(cleanup), Some(payload)) = (entry.cleanup.take(), entry.payload.take())
            {
                cleanup(payload);
            }
        }
    }
}

/// Exact `m * 2^exp` scaling without intermediate overflow or underflow panic.
pub fn ldexp_f64(mut m: f64, exp: i64) -> f64 {
    if m == 0.0 || !m.is_finite() || exp == 0 {
        return m;
    }
    let mut rem = exp.clamp(-4096, 4096);
    while rem > 0 {
        let step = rem.min(1023) as i32;
        let factor = f64::from_bits(((1023 + step) as u64) << 52);
        m *= factor;
        rem -= i64::from(step);
        if !m.is_finite() {
            break;
        }
    }
    while rem < 0 {
        let step = rem.max(-1022) as i32;
        let factor = f64::from_bits(((1023 + step) as u64) << 52);
        m *= factor;
        rem -= i64::from(step);
        if m == 0.0 {
            break;
        }
    }
    m
}
