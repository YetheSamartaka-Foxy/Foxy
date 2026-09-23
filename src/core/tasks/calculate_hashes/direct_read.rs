//! Non-cached hash reads. The file is opened without the Windows cache
//! manager and read in aligned blocks, `depth` of them in flight at once, so
//! hashing one block overlaps reading the next and a pass over the payload
//! neither copies it through the cache nor evicts the rest of the machine's
//! cache. On rotational storage a [`DiskTurn`] lets one file stream at a time.

use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};

/// Offsets and lengths of non-cached reads must be sector multiples; this
/// covers every sector size in use and matches the allocation granularity of
/// the buffers.
const ALIGN: u64 = 64 * 1024;

/// How one run reads existing files without the cache.
#[derive(Clone)]
pub(super) struct DirectReadPlan {
    pub(super) block: usize,
    pub(super) depth: usize,
    /// Files shorter than this keep the cached reader.
    pub(super) min_len: u64,
    pub(super) pool: Arc<SlotPool>,
    pub(super) turn: Option<Arc<DiskTurn>>,
}

impl DirectReadPlan {
    pub(super) fn new(block: usize, depth: usize, min_len: u64, one_stream: bool) -> Self {
        let block = (block as u64).div_ceil(ALIGN).max(1) * ALIGN;
        Self {
            block: block as usize,
            depth: depth.max(1),
            min_len,
            pool: Arc::new(SlotPool::default()),
            turn: one_stream.then(|| Arc::new(DiskTurn::default())),
        }
    }
}

/// Lets one reader at a time issue reads, from its first read until its last
/// one is issued, so a rotational disk streams one file instead of
/// alternating between two, and the next file starts as the current ends.
#[derive(Default)]
pub(super) struct DiskTurn {
    busy: Mutex<bool>,
    freed: Condvar,
}

impl DiskTurn {
    fn acquire(&self) {
        let mut busy = self
            .busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *busy {
            busy = self
                .freed
                .wait(busy)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *busy = true;
    }

    fn release(&self) {
        *self
            .busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
        self.freed.notify_one();
    }
}

/// Read buffers shared by one run's readers, so a run allocates them once per
/// concurrent read rather than once per file.
#[derive(Default)]
pub(super) struct SlotPool {
    free: Mutex<Vec<imp::Slot>>,
}

impl SlotPool {
    fn take(&self, block: usize) -> io::Result<imp::Slot> {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match free.iter().position(|slot| slot.len() == block) {
            Some(index) => Ok(free.swap_remove(index)),
            None => {
                drop(free);
                imp::Slot::new(block)
            }
        }
    }

    fn give(&self, slot: imp::Slot) {
        self.free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(slot);
    }
}

struct Filled {
    slot: imp::Slot,
    offset: u64,
    len: usize,
}

struct InFlight {
    slot: imp::Slot,
    offset: u64,
}

/// A non-cached, read-ahead reader over one file. `Read`, `BufRead` and
/// `Seek` behave as on a `BufReader`, so the layout parser, the part loop and
/// the fingerprint tap run on it unchanged.
pub(super) struct DirectReader {
    file: imp::File,
    file_len: u64,
    block: usize,
    depth: usize,
    pool: Arc<SlotPool>,
    turn: Option<Arc<DiskTurn>>,
    holding_turn: bool,
    current: Option<Filled>,
    in_flight: VecDeque<InFlight>,
    /// Logical read position.
    pos: u64,
    /// Offset of the next read to issue; always aligned.
    next_issue: u64,
}

impl DirectReader {
    /// `None` when the file cannot be opened without the cache (network
    /// shares, unusual file systems); the caller falls back to a cached read.
    pub(super) fn open(path: &str, file_len: u64, plan: &DirectReadPlan) -> Option<Self> {
        let file = imp::File::open(path).ok()?;
        Some(Self {
            file,
            file_len,
            block: plan.block,
            depth: plan.depth,
            pool: plan.pool.clone(),
            turn: plan.turn.clone(),
            holding_turn: false,
            current: None,
            in_flight: VecDeque::new(),
            pos: 0,
            next_issue: 0,
        })
    }

    fn release_turn(&mut self) {
        if self.holding_turn {
            self.holding_turn = false;
            if let Some(turn) = &self.turn {
                turn.release();
            }
        }
    }

    fn issue_ahead(&mut self) -> io::Result<()> {
        let slots_in_use = self.in_flight.len() + usize::from(self.current.is_some());
        let mut free_slots = self.depth.saturating_sub(slots_in_use);
        while free_slots > 0 && self.next_issue < self.file_len {
            if !self.holding_turn
                && let Some(turn) = &self.turn
            {
                turn.acquire();
                self.holding_turn = true;
            }
            let mut slot = self.pool.take(self.block)?;
            let offset = self.next_issue;
            if let Err(err) = self.file.start_read(&mut slot, offset) {
                self.pool.give(slot);
                return Err(err);
            }
            self.in_flight.push_back(InFlight { slot, offset });
            self.next_issue = offset + self.block as u64;
            free_slots -= 1;
        }
        if self.next_issue >= self.file_len {
            self.release_turn();
        }
        Ok(())
    }

    /// Stops every outstanding read and waits for it, since the kernel owns
    /// a buffer until its read completes.
    fn drain(&mut self) {
        for read in &mut self.in_flight {
            self.file.cancel(&mut read.slot);
        }
        while let Some(mut read) = self.in_flight.pop_front() {
            let _ = self.file.finish_read(&mut read.slot);
            self.pool.give(read.slot);
        }
        if let Some(filled) = self.current.take() {
            self.pool.give(filled.slot);
        }
    }

    fn restart_at(&mut self, target: u64) {
        self.drain();
        self.pos = target;
        self.next_issue = target / ALIGN * ALIGN;
    }
}

impl Read for DirectReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let count = available.len().min(out.len());
        out[..count].copy_from_slice(&available[..count]);
        self.consume(count);
        Ok(count)
    }
}

impl BufRead for DirectReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        loop {
            if let Some(filled) = &self.current {
                if self.pos < filled.offset + filled.len as u64 {
                    break;
                }
                if filled.len < self.block {
                    return Ok(&[]);
                }
                let filled = self.current.take().expect("checked above");
                self.pool.give(filled.slot);
            }
            if self.pos >= self.file_len {
                return Ok(&[]);
            }
            self.issue_ahead()?;
            let Some(mut read) = self.in_flight.pop_front() else {
                return Ok(&[]);
            };
            let len = match self.file.finish_read(&mut read.slot) {
                Ok(len) => len,
                Err(err) => {
                    self.pool.give(read.slot);
                    self.restart_at(self.pos);
                    return Err(err);
                }
            };
            self.current = Some(Filled {
                slot: read.slot,
                offset: read.offset,
                len,
            });
            self.issue_ahead()?;
            if len == 0 {
                return Ok(&[]);
            }
        }
        let filled = self.current.as_ref().expect("loop exits with a block");
        let start = (self.pos - filled.offset) as usize;
        Ok(&filled.slot.bytes()[start..filled.len])
    }

    fn consume(&mut self, amount: usize) {
        self.pos += amount as u64;
    }
}

impl Seek for DirectReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            SeekFrom::End(delta) => self.file_len.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        let window_start = self
            .current
            .as_ref()
            .map(|filled| filled.offset)
            .or_else(|| self.in_flight.front().map(|read| read.offset))
            .unwrap_or(self.next_issue);
        if target >= window_start && target < self.next_issue {
            self.pos = target;
        } else {
            self.restart_at(target);
        }
        Ok(self.pos)
    }
}

impl Drop for DirectReader {
    fn drop(&mut self) {
        self.drain();
        self.release_turn();
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use winapi::shared::minwindef::{DWORD, FALSE, TRUE};
    use winapi::shared::winerror::{ERROR_HANDLE_EOF, ERROR_IO_PENDING};
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::fileapi::ReadFile;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::ioapiset::{CancelIoEx, GetOverlappedResult};
    use winapi::um::memoryapi::{VirtualAlloc, VirtualFree};
    use winapi::um::minwinbase::OVERLAPPED;
    use winapi::um::synchapi::CreateEventW;
    use winapi::um::winnt::{HANDLE, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE};

    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

    pub(super) struct File(std::fs::File);

    impl File {
        pub(super) fn open(path: &str) -> io::Result<Self> {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED)
                .open(path)
                .map(Self)
        }

        fn handle(&self) -> HANDLE {
            self.0.as_raw_handle().cast()
        }

        pub(super) fn start_read(&self, slot: &mut Slot, offset: u64) -> io::Result<()> {
            // SAFETY: all-zero is a valid OVERLAPPED; the offset halves and the
            // slot's event are set before the read starts.
            let overlapped = slot.overlapped.as_mut();
            *overlapped = unsafe { std::mem::zeroed() };
            // SAFETY: writing the plain offset fields of the union.
            unsafe {
                let parts = overlapped.u.s_mut();
                parts.Offset = offset as u32;
                parts.OffsetHigh = (offset >> 32) as u32;
            }
            overlapped.hEvent = slot.event;
            // SAFETY: the buffer and the boxed OVERLAPPED stay alive and in
            // place until `finish_read` has waited for this read.
            let ok = unsafe {
                ReadFile(
                    self.handle(),
                    slot.buffer.cast(),
                    slot.len as DWORD,
                    std::ptr::null_mut(),
                    overlapped,
                )
            };
            slot.pending = true;
            // SAFETY: GetLastError has no preconditions.
            if ok == 0 {
                match unsafe { GetLastError() } {
                    ERROR_IO_PENDING => {}
                    ERROR_HANDLE_EOF => slot.eof = true,
                    code => {
                        slot.pending = false;
                        return Err(io::Error::from_raw_os_error(code as i32));
                    }
                }
            }
            Ok(())
        }

        pub(super) fn finish_read(&self, slot: &mut Slot) -> io::Result<usize> {
            if !slot.pending {
                return Ok(0);
            }
            slot.pending = false;
            if std::mem::take(&mut slot.eof) {
                return Ok(0);
            }
            let mut transferred: DWORD = 0;
            // SAFETY: the OVERLAPPED belongs to a read started on this handle.
            let ok = unsafe {
                GetOverlappedResult(
                    self.handle(),
                    slot.overlapped.as_mut(),
                    &mut transferred,
                    TRUE,
                )
            };
            if ok == 0 {
                // SAFETY: GetLastError has no preconditions.
                return match unsafe { GetLastError() } {
                    ERROR_HANDLE_EOF => Ok(0),
                    code => Err(io::Error::from_raw_os_error(code as i32)),
                };
            }
            Ok(transferred as usize)
        }

        pub(super) fn cancel(&self, slot: &mut Slot) {
            if slot.pending && !slot.eof {
                // SAFETY: cancelling a read this handle started; completion is
                // still awaited by `finish_read` before the buffer is reused.
                unsafe { CancelIoEx(self.handle(), slot.overlapped.as_mut()) };
            }
        }
    }

    /// An aligned read buffer with its own OVERLAPPED and completion event.
    pub(super) struct Slot {
        buffer: *mut u8,
        len: usize,
        event: HANDLE,
        overlapped: Box<OVERLAPPED>,
        pending: bool,
        eof: bool,
    }

    // SAFETY: the buffer and event are owned by the slot and used by one
    // thread at a time; the pool hands a slot to one reader.
    unsafe impl Send for Slot {}

    impl Slot {
        pub(super) fn new(len: usize) -> io::Result<Self> {
            // SAFETY: a fresh committed allocation, aligned to the allocation
            // granularity, released in Drop.
            let buffer = unsafe {
                VirtualAlloc(
                    std::ptr::null_mut(),
                    len,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            if buffer.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: an unnamed manual-reset event, closed in Drop.
            let event =
                unsafe { CreateEventW(std::ptr::null_mut(), TRUE, FALSE, std::ptr::null()) };
            if event.is_null() {
                let err = io::Error::last_os_error();
                // SAFETY: releasing the allocation made above.
                unsafe { VirtualFree(buffer, 0, MEM_RELEASE) };
                return Err(err);
            }
            Ok(Self {
                buffer: buffer.cast(),
                len,
                event,
                // SAFETY: all-zero is a valid OVERLAPPED.
                overlapped: Box::new(unsafe { std::mem::zeroed() }),
                pending: false,
                eof: false,
            })
        }

        pub(super) fn len(&self) -> usize {
            self.len
        }

        pub(super) fn bytes(&self) -> &[u8] {
            // SAFETY: `len` committed bytes, not written while no read is
            // pending on this slot.
            unsafe { std::slice::from_raw_parts(self.buffer, self.len) }
        }
    }

    impl Drop for Slot {
        fn drop(&mut self) {
            // SAFETY: both were created in `new` and are released once; no
            // read is pending on a slot that is dropped.
            unsafe {
                CloseHandle(self.event);
                VirtualFree(self.buffer.cast(), 0, MEM_RELEASE);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    pub(super) struct File;

    impl File {
        pub(super) fn open(_path: &str) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "non-cached reads are Windows only",
            ))
        }

        pub(super) fn start_read(&self, _slot: &mut Slot, _offset: u64) -> io::Result<()> {
            unreachable!("never opened")
        }

        pub(super) fn finish_read(&self, _slot: &mut Slot) -> io::Result<usize> {
            unreachable!("never opened")
        }

        pub(super) fn cancel(&self, _slot: &mut Slot) {}
    }

    pub(super) struct Slot(Vec<u8>);

    impl Slot {
        pub(super) fn new(len: usize) -> io::Result<Self> {
            Ok(Self(vec![0; len]))
        }

        pub(super) fn len(&self) -> usize {
            self.0.len()
        }

        pub(super) fn bytes(&self) -> &[u8] {
            &self.0
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::io::Write;

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 + i / 7) as u8).collect()
    }

    fn file_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        file
    }

    fn reader(file: &tempfile::NamedTempFile, len: usize, plan: &DirectReadPlan) -> DirectReader {
        DirectReader::open(file.path().to_str().unwrap(), len as u64, plan).unwrap()
    }

    #[test]
    fn reads_every_byte_of_files_around_block_boundaries() {
        for depth in [1, 2, 4] {
            let plan = DirectReadPlan::new(64 * 1024, depth, 0, false);
            for len in [0, 1, 4095, 65_535, 65_536, 65_537, 200_000, 262_144] {
                let bytes = payload(len);
                let file = file_with(&bytes);
                let mut out = Vec::new();
                reader(&file, len, &plan).read_to_end(&mut out).unwrap();
                assert_eq!(out, bytes, "len={len} depth={depth}");
            }
        }
    }

    #[test]
    fn seeks_forward_backward_and_past_the_window_read_the_right_bytes() {
        let bytes = payload(700_000);
        let file = file_with(&bytes);
        let plan = DirectReadPlan::new(64 * 1024, 2, 0, false);
        let mut reader = reader(&file, bytes.len(), &plan);
        let mut buf = [0u8; 1000];
        for offset in [0u64, 10, 70_000, 69_000, 400_123, 5, 699_000, 131_072] {
            reader.seek(SeekFrom::Start(offset)).unwrap();
            assert_eq!(reader.stream_position().unwrap(), offset);
            reader.read_exact(&mut buf).unwrap();
            let at = offset as usize;
            assert_eq!(&buf[..], &bytes[at..at + 1000], "offset={offset}");
        }
        reader.seek(SeekFrom::Current(-500)).unwrap();
        reader.read_exact(&mut buf[..500]).unwrap();
        assert_eq!(&buf[..500], &bytes[131_572..132_072]);
        reader.seek(SeekFrom::End(-3)).unwrap();
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, &bytes[bytes.len() - 3..]);
    }

    #[test]
    fn buffers_are_reused_across_files_of_one_run() {
        let plan = DirectReadPlan::new(64 * 1024, 2, 0, true);
        for _ in 0..3 {
            let bytes = payload(300_000);
            let file = file_with(&bytes);
            let mut out = Vec::new();
            reader(&file, bytes.len(), &plan)
                .read_to_end(&mut out)
                .unwrap();
            assert_eq!(out, bytes);
        }
        assert!(plan.pool.free.lock().unwrap().len() <= 2);
    }

    #[test]
    fn one_stream_turn_passes_to_the_next_file_once_the_last_read_is_issued() {
        let plan = DirectReadPlan::new(64 * 1024, 2, 0, true);
        let first_bytes = payload(100_000);
        let first = file_with(&first_bytes);
        let second_bytes = payload(90_000);
        let second = file_with(&second_bytes);
        let mut a = reader(&first, first_bytes.len(), &plan);
        let mut b = reader(&second, second_bytes.len(), &plan);
        let mut out = vec![0u8; 10];
        a.read_exact(&mut out).unwrap();
        // Both of the first file's reads are issued, so the second file may
        // start while the first is still being consumed.
        let mut rest = Vec::new();
        b.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, second_bytes);
        let mut remaining = Vec::new();
        a.read_to_end(&mut remaining).unwrap();
        assert_eq!(remaining, &first_bytes[10..]);
    }
}
