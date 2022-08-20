use core::fmt::{self, Display};

/// Global error type
#[derive(Debug)]
pub enum MfError {
    /// Nt error code
    #[cfg(windows)]
    NtStatus(u32),
    /// Specified process was not found
    ProcessNotFound,
    /// Specified module was not found
    ModuleNotFound,
    /// No threads running in the process
    NoThreads,
}

impl MfError {
    #[cfg(windows)]
    pub(crate) fn last<T>() -> Result<T> {
        #[link(name = "kernel32")]
        extern "C" {
            fn GetLastError() -> u32;
        }

        Err(MfError::NtStatus(unsafe { GetLastError() }))
    }
}

impl Display for MfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[allow(missing_docs)]
pub type Result<T> = std::result::Result<T, MfError>;