use super::{ModuleEntry, ModuleIterator, OwnedThread, ThreadIterator};
use crate::{
    external::{Handle, NtResult},
    terminated_array,
    types::win::{AllocationType, FreeType, MemoryProtection, ProcessRights},
    MfError, Matcher, DynPattern,
};
use core::{
    iter::from_fn,
    mem::{size_of, zeroed},
};

#[link(name = "kernel32")]
extern "C" {
    fn ReadProcessMemory(
        hnd: isize,
        addr: usize,
        buf: *mut u8,
        size: usize,
        read: Option<&mut usize>,
    ) -> NtResult;

    fn WriteProcessMemory(
        hnd: isize,
        addr: usize,
        buf: *const u8,
        size: usize,
        written: Option<&mut usize>,
    ) -> NtResult;

    fn VirtualProtectEx(
        hnd: isize,
        addr: usize,
        size: usize,
        new: MemoryProtection,
        old: &mut MemoryProtection,
    ) -> NtResult;

    fn VirtualAllocEx(
        hnd: isize,
        addr: usize,
        size: usize,
        alloc_type: AllocationType,
        protection: MemoryProtection,
    ) -> usize;

    fn VirtualFreeEx(hnd: isize, addr: usize, size: usize, free_ty: FreeType) -> NtResult;

    fn CreateRemoteThread(
        hnd: isize,
        sec_attrs: *mut (),
        stack_size: usize,
        start_addr: usize,
        param: usize,
        create_flags: u32,
        out_tid: Option<&mut u32>,
    ) -> Handle;

    fn GetProcessId(hnd: isize) -> u32;

    pub(crate) fn CreateToolhelp32Snapshot(flags: i32, pid: u32) -> Handle;
    fn Process32FirstW(hnd: isize, lppe: &mut FfiProcessEntry) -> bool;
    fn Process32NextW(hnd: isize, lppe: &mut FfiProcessEntry) -> bool;

    fn OpenProcess(access: ProcessRights, inherit: i32, id: u32) -> Handle;
    fn TerminateProcess(hnd: isize, code: u32) -> NtResult;
}

#[link(name = "ntdll")]
extern "C" {
    fn NtSuspendProcess(hnd: isize) -> NtResult;
    fn NtResumeProcess(hnd: isize) -> NtResult;
}

/// Owned handle to another process
pub struct OwnedProcess(Handle);

impl OwnedProcess {
    /// Takes ownership of handle.
    pub unsafe fn from_handle(h: Handle) -> Self {
        Self(h)
    }

    /// Gives away ownership of the handle.
    pub fn into_handle(self) -> Handle {
        self.0
    }

    /// Closes handle to the process.
    pub fn close(self) -> crate::Result<()> {
        self.into_handle().close()
    }

    /// Reads process memory, returns amount of bytes read.
    pub fn read_buf(&self, address: usize, mut buf: impl AsMut<[u8]>) -> crate::Result<usize> {
        let mut read = 0;
        let buf = buf.as_mut();

        unsafe {
            ReadProcessMemory(
                self.0 .0,
                address,
                buf.as_mut_ptr(),
                buf.len(),
                Some(&mut read),
            )
            .expect_nonzero(read)
        }
    }

    /// Reads process memory, returning the value read at the `address`.
    pub fn read<T>(&self, address: usize) -> crate::Result<T> {
        unsafe {
            let mut buf: T = zeroed();

            ReadProcessMemory(
                self.0 .0,
                address,
                &mut buf as *mut T as _,
                size_of::<T>(),
                None,
            )
            .expect_nonzero(buf)
        }
    }

    /// Reads zero terminated string at `address`.
    pub fn read_str(&self, address: usize) -> crate::Result<String> {
        const BUF_SIZE: usize = 4;

        let mut out = vec![];
        let mut offset = 0;

        loop {
            let buf = self.read::<[u8; BUF_SIZE]>(address + offset)?;

            if let Some(i) = buf.iter().position(|b| *b == 0) {
                out.extend_from_slice(&buf[..i]);
                break
            } else {
                out.extend_from_slice(&buf);
            }
            
            offset += BUF_SIZE
        }

        Ok(String::from_utf8(out).map_err(|_| MfError::InvalidString)?)
    }

    /// Writes buffer to the process memory, returning the amount of bytes written.
    pub fn write_buf(&self, address: usize, buf: impl AsRef<[u8]>) -> crate::Result<usize> {
        let mut written: usize = 0;
        let buf = buf.as_ref();

        unsafe {
            WriteProcessMemory(
                self.0 .0,
                address,
                buf.as_ptr(),
                buf.len(),
                Some(&mut written),
            )
            .expect_nonzero(written)
        }
    }

    /// Writes value to the process memory, returning the amount of bytes written.
    pub fn write<T>(&self, address: usize, value: T) -> crate::Result<usize> {
        let mut written: usize = 0;

        unsafe {
            WriteProcessMemory(
                self.0 .0,
                address,
                &value as *const T as _,
                size_of::<T>(),
                Some(&mut written),
            )
            .expect_nonzero(written)
        }
    }

    /// Writes string to the specified address, putting 0 at the end
    pub fn write_str(&self, address: usize, text: impl AsRef<str>) -> crate::Result<usize> {
        let text = text.as_ref();
        let mut wrote = self.write_buf(address, text.as_bytes())?;
        wrote += self.write(address + wrote, 0)?;
        Ok(wrote)
    }

    /// Changes the protection of memory pages, returning the old protection value.
    pub fn protect(
        &self,
        address: usize,
        size: usize,
        protection: MemoryProtection,
    ) -> crate::Result<MemoryProtection> {
        let mut old = MemoryProtection(0);
        unsafe {
            VirtualProtectEx(self.0 .0, address, size, protection, &mut old).expect_nonzero(old)
        }
    }

    /// Allocates new region of memory, returning pointer to it.
    pub fn allocate(
        &self,
        desired_address: Option<usize>,
        size: usize,
        alloc_type: AllocationType,
        protection: MemoryProtection,
    ) -> crate::Result<usize> {
        unsafe {
            let addr = VirtualAllocEx(
                self.0 .0,
                desired_address.unwrap_or_default(),
                size,
                alloc_type,
                protection,
            );

            if addr == 0 {
                MfError::last()
            } else {
                Ok(addr)
            }
        }
    }

    /// Frees region of memory.
    /// # Note
    /// If `free_type` is `MEM_RELEASE` then `size` must be 0.
    pub fn free(&self, address: usize, size: usize, free_type: FreeType) -> crate::Result<()> {
        unsafe { VirtualFreeEx(self.0 .0, address, size, free_type).expect_nonzero(()) }
    }

    /// Creates thread running in the process's context.
    pub fn create_thread(
        &self,
        stack_size: Option<usize>,
        start_address: usize,
        param: usize,
        suspended: bool,
    ) -> crate::Result<OwnedThread> {
        unsafe {
            let h = CreateRemoteThread(
                self.0 .0,
                0 as _,
                stack_size.unwrap_or_default(),
                start_address,
                param,
                if suspended { 0x4 } else { 0 },
                None,
            );

            if h.is_invalid() {
                MfError::last()
            } else {
                Ok(OwnedThread::from_handle(h))
            }
        }
    }

    /// Returns the id of the process.
    pub fn id(&self) -> u32 {
        unsafe { GetProcessId(self.0 .0) }
    }

    /// Returns an iterator over process's modules.
    pub fn modules(&self) -> crate::Result<ModuleIterator> {
        ModuleIterator::new(self.id())
    }

    /// Returns an iterator over process's threads.
    pub fn threads(&self) -> crate::Result<ThreadIterator> {
        ThreadIterator::new(self.id())
    }

    /// Searches for the module in the process.
    pub fn find_module(&self, module_name: &str) -> crate::Result<ModuleEntry> {
        self.modules()?
            .find(|me| me.name.eq_ignore_ascii_case(module_name))
            .ok_or(MfError::ModuleNotFound)
    }

    /// Finds all occurences of the pattern in a given range.
    // @TODO: Can be optimized
    pub fn find_pattern<'a>(
        &'a self,
        pat: impl Matcher + 'a,
        start: usize,
        len: usize,
    ) -> impl Iterator<Item = usize> + 'a {
        
        let mut offset = 0;
        let mut buf = vec![0; pat.size()];

        from_fn(move || {
            loop {
                if self.read_buf(start + offset, &mut buf[..]).is_err() {
                    return None;
                }

                if pat.matches(&buf[..]) {
                    break;
                }

                offset += 1;

                if offset >= len {
                    return None;
                }
            }
            
            offset += 1;
            Some(start + offset - 1)
        })
        .fuse()
    }

    /// Finds all occurences of the pattern in the specified module.
    pub fn find_pattern_in_module<'a>(
        &'a self,
        pat: impl Matcher + 'a,
        module_name: &str,
    ) -> crate::Result<impl Iterator<Item = usize> + 'a> {
        let module = self.find_module(module_name)?;
        Ok(self.find_pattern(pat, module.base, module.size))
    }

    /// Creates a pattern for `target` making sure there are no other matches in range from `start` to `start + len`.
/// If `max` is set, function will abort if failed to find pattern in less than `max` bytes.
    pub fn create_pattern(
        &self,
        target: usize,
        start: usize,
        len: usize,
        max: Option<usize>,
    ) -> crate::Result<Option<DynPattern>> {

        let mut size = 3;
        let mut offset = 0;

        loop {
            let mut pat = vec![0; size];
            self.read_buf(target, &mut pat[..])?;

            let mut done = true;
            for oc in self.find_pattern(&pat[..], start + offset, len - offset) {
                if oc != target {
                    size += 1;
                    offset = oc - start;
                    done = false;

                    if let Some(max) = max && size > max {
                        return Ok(None);
                    }

                    break;
                }
            }

            if done {
                return Ok(Some(pat.as_slice().into()))
            }
        }
    }

    /// Creates a pattern for `target` making sure there are no other matches in the specified module.
    /// If `max` is set, function will abort if failed to find pattern in less than `max` bytes.
    pub fn create_pattern_in_module(
        &self,
        target: usize,
        module_name: &str,
        max: Option<usize>
    ) -> crate::Result<Option<DynPattern>> {
        let module = self.find_module(module_name)?;
        Ok(self.create_pattern(target, module.base, module.size, max)?)
    }

    /// Resolves multilevel pointer
    pub fn resolve_multilevel(
        &self,
        mut base: usize,
        offsets: &[usize]
    ) -> crate::Result<usize> {
        for &o in offsets {
            base = self.read(base + o)?;
        }

        Ok(base)
    }

    /// Terminates the process with the specified code.
    pub fn terminate(&self, exit_code: u32) -> crate::Result<()> {
        unsafe { TerminateProcess(self.0 .0, exit_code).expect_nonzero(()) }
    }

    /// Suspends the process with `NtSuspendProcess`
    pub fn suspend(&self) -> crate::Result<()> {
        unsafe { NtSuspendProcess(self.0 .0).expect_zero(()) }
    }

    /// Resumes the process with `NtResumeProcess`
    pub fn resume(&self) -> crate::Result<()> {
        unsafe { NtResumeProcess(self.0 .0).expect_zero(()) }
    }
}

#[repr(C)]
struct FfiProcessEntry {
    size: u32,
    usage: u32,
    pid: u32,
    heap_id: usize,
    mod_id: u32,
    cnt_threads: u32,
    parent: u32,
    pri_class: i32,
    flags: u32,
    file_path: [u16; 260],
}

/// Iterator over all processes in the system.
pub struct ProcessIterator {
    h: Handle,
    entry: FfiProcessEntry,
    stop: bool,
}

impl ProcessIterator {
    /// Creates new iterator over processes.
    pub fn new() -> crate::Result<Self> {
        unsafe {
            let h = CreateToolhelp32Snapshot(0x00000002, 0);
            if h.is_invalid() {
                return MfError::last();
            }

            let mut this = Self {
                h,
                entry: zeroed(),
                stop: false,
            };
            this.entry.size = size_of::<FfiProcessEntry>() as u32;
            if Process32FirstW(this.h.0, &mut this.entry) {
                Ok(this)
            } else {
                MfError::last()
            }
        }
    }
}

impl Iterator for ProcessIterator {
    type Item = ProcessEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stop {
            return None;
        }

        let current = ProcessEntry::from(&self.entry);
        unsafe {
            self.stop = !Process32NextW(self.h.0, &mut self.entry);
        }
        Some(current)
    }
}

/// ProcessEntry contains information about process running in system
#[derive(Debug)]
#[allow(missing_docs)]
pub struct ProcessEntry {
    pub id: u32,
    pub parent_id: u32,
    pub default_heap: usize,
    pub thread_count: u32,
    pub path: String,
}

impl ProcessEntry {
    /// Opens process by the entry's process id.
    pub fn open(
        &self,
        inherit_handle: bool,
        access_rights: ProcessRights,
    ) -> crate::Result<OwnedProcess> {
        open_process_by_id(self.id, inherit_handle, access_rights)
    }
}

impl From<&FfiProcessEntry> for ProcessEntry {
    fn from(pe: &FfiProcessEntry) -> Self {
        Self {
            id: pe.pid,
            parent_id: pe.parent,
            default_heap: pe.heap_id,
            thread_count: pe.cnt_threads,
            path: String::from_utf16_lossy(unsafe { terminated_array(pe.file_path.as_ptr(), 0) }),
        }
    }
}

/// Tried to open process by name
pub fn open_process_by_name(
    name: &str,
    inherit_handle: bool,
    access_rights: ProcessRights,
) -> crate::Result<OwnedProcess> {
    ProcessIterator::new()?
        .find_map(|pe| {
            if pe.path.eq_ignore_ascii_case(name) {
                Some(pe.open(inherit_handle, access_rights))
            } else {
                None
            }
        })
        .ok_or(MfError::ProcessNotFound)?
}

/// Tried to open process by id
pub fn open_process_by_id(
    id: u32,
    inherit_handle: bool,
    access_rights: ProcessRights,
) -> crate::Result<OwnedProcess> {
    unsafe {
        let h = OpenProcess(access_rights, inherit_handle as i32, id);
        if h.is_invalid() {
            MfError::last()
        } else {
            Ok(OwnedProcess(h))
        }
    }
}
