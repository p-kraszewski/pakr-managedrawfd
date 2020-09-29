//! A Trait and two Impls dealing with auto-closing `RawFd` file handles with a
//! sensible `Clone` trait implementations.
//!
//! - [`DuplicatingFD`](struct.DuplicatingFD.html) holds a raw handle directly and duplicates itself by calls to
//!   [dup(2)](https://man7.org/linux/man-pages/man2/dup.2.html) -
//!   that is each instance has its own handle that is individually closed on end-of-life of that
//!   instance. May panic on `clone()` call due to underlying OS error.
//!
//! - [`SharedFD`](struct.SharedFD.html) holds a raw handle in `std::sync::Arc` and duplicates itself by
//!   duplicating said `Arc` - that is each instance shares a single handle, which is closed after
//!   the last instance reaches end-of-life.
//!
//! # Common functionality
//!
//! Both implementations...
//!  - implement `AsRawFd` and `Clone` traits.
//!  - have a [`wrap(fd)`](trait.ManagedFD.html#tymethod.wrap) constructor that simply packs `fd` in
//!    managed shell and takes ownership (you shouldn't use `fd` afterwards and *definitely* not
//!    `close()` it).
//!  - have a [`dup_wrap(fd)`](trait.ManagedFD.html#tymethod.dup_wrap) constructor that packs
//!    [dup(2)](https://man7.org/linux/man-pages/man2/dup.2.html)
//!    copy of `fd` in managed shell. It doesn't take the ownership of the original `fd`, which you
//!    should dispose-of properly.
//!  - have a [`dup()`](trait.ManagedFD.html#tymethod.dup) method that clones handle accordingly,
//!    returning eventual errors.
//!
//! # Multi-access
//! Both are **not** multi-access safe, with `SharedFD` being even less safe.
//!
//! - Each of the related `DuplicatingFD` instances has _its own_ read/write pointer (still stepping on
//!   each other's toes during writes)
//! - All the related `SharedFD` instances have a _single, shared_ read/write pointer.
//!
use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::RawFd;
use std::sync::Arc;

/// Trait `ManagedFD` describes a managed `std::os::unix::io::RawFd`, with primary functionality of
/// auto-closing on drop and performing sensible `clone()`/`dup()` operations.
///
/// Warning: `Clone` trait has no way to convey errors, so implementations are forced to `panic!()`.
pub trait ManagedFD
where
    Self: AsRawFd + Clone,
{
    /// Wrap `fd` in `ManagedFD`. You should not use naked handle afterwards, in particular *don't
    /// close it*.
    fn wrap(fd: RawFd) -> Self;

    /// Wrap a [dup(2)](https://man7.org/linux/man-pages/man2/dup.2.html) copy of `fd` in `ManagedFD`.
    /// You should dispose the original `fd` properly at your discretion.
    fn dup_wrap(fd: RawFd) -> io::Result<Self>;

    /// Create a duplicate of handle in such a way, that dropping of one instance has no influence
    /// on the other ones.
    ///
    /// It lets you define (required) Clone trait as simply
    /// ```ignore
    /// impl Clone for MyImpl {
    ///     fn clone(&self) -> Self {
    ///         self.dup().unwrap()
    ///     }
    /// }
    /// ```
    fn dup(&self) -> io::Result<Self>;
}

/// Intermediate auto-closing handle. Does not implement `clone()`, but can create itself off a
/// `dup(2)` clone.
struct AutoClosingFD(RawFd);
impl AutoClosingFD{
    #[inline]
    fn wrap(fd: RawFd) -> Self {
        AutoClosingFD(fd)
    }

    #[inline]
    fn dup_wrap(fd: RawFd) -> io::Result<Self> {
        let new_handle = unsafe { libc::dup(fd) };
        if new_handle == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self::wrap(new_handle))
        }
    }
}

impl Drop for AutoClosingFD{
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
            self.0 = -1;
        }
    }
}

impl  AsRawFd for AutoClosingFD {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

/// Implements `Clone` trait that calls [dup(2)](https://man7.org/linux/man-pages/man2/dup.2.html) on
/// underlying handle and returns new instance wrapping dup-ped handle.
///
/// **Warning**: `clone()` will panic if `dup(2)` returns error.
///
/// Each `clone()`/`dup()` of `DuplicatingFD` contains a different handle.
///
/// # Example
/// ```
/// use pakr_managedrawfd::*;
/// use std::os::unix::io::AsRawFd;
///
/// let stdout_handle = std::io::stdout().lock().as_raw_fd();
///
/// // We don't want myH to close the real stdout, therefore dup_wrap;
/// let myDupH = DuplicatingFD::dup_wrap(stdout_handle).unwrap();
///
/// // ... myDupH shall have handle other than stdout.
/// assert_ne!(myDupH.as_raw_fd(),stdout_handle);
///
/// // Clone it
/// let myOtherDupH = myDupH.clone();
///
/// // ... myOtherDupH shall have yet another handle, other than both myDupH and stdout.
/// assert_ne!(myOtherDupH.as_raw_fd(),stdout_handle);
/// assert_ne!(myOtherDupH.as_raw_fd(),myDupH.as_raw_fd());
///
/// ```
pub struct DuplicatingFD(AutoClosingFD);

impl ManagedFD for DuplicatingFD {
    fn wrap(fd: RawFd) -> Self {
        DuplicatingFD(AutoClosingFD::wrap(fd))
    }

    fn dup_wrap(fd: RawFd) -> io::Result<Self> {
        Ok(DuplicatingFD(AutoClosingFD::dup_wrap(fd)?))
    }

    fn dup(&self) -> io::Result<Self> {
        let new_handle = unsafe { libc::dup(self.as_raw_fd()) };
        if new_handle == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self::wrap(new_handle))
        }
    }
}

impl Clone for DuplicatingFD {
    fn clone(&self) -> Self {
        self.dup().unwrap()
    }

    fn clone_from(&mut self, source: &Self) {
        assert!(source.as_raw_fd()>=0);
        assert!(self.as_raw_fd()>=0);

        if source.as_raw_fd() != self.as_raw_fd() {
            unsafe { libc::close(self.as_raw_fd()) };
            let rc = unsafe { libc::dup2(source.as_raw_fd(), self.as_raw_fd()) };
            if rc == -1 {
                panic!(io::Error::last_os_error());
            }
        }
    }
}

impl AsRawFd for DuplicatingFD {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// Implements `Clone` trait that creates new `SharedFD` with `Arc::clone` of the
/// embedded handle.
///
/// Each `clone()`/`dup()` of  `SharedFD` contains the same handle.
///
/// # Example
/// ```
/// use pakr_managedrawfd::*;
/// use std::os::unix::io::AsRawFd;
///
/// let stdout_handle = std::io::stdout().lock().as_raw_fd();
///
/// // We don't want myShH to close the real stdout, therefore dup_wrap;
/// let myShH = SharedFD::dup_wrap(stdout_handle).unwrap();
///
/// // ... myShH shall have handle other than stdout.
/// assert_ne!(myShH.as_raw_fd(),stdout_handle);
///
/// // Clone it
/// let myOtherShH = myShH.clone();
///
/// // ... myOtherShH shall have the same handle as myShH.
/// assert_eq!(myOtherShH.as_raw_fd(),myShH.as_raw_fd());
///
/// ```
pub struct SharedFD(Arc<AutoClosingFD>);

impl ManagedFD for SharedFD {
    fn wrap(fd: RawFd) -> Self {
        SharedFD(Arc::new(AutoClosingFD::wrap(fd)))
    }

    fn dup_wrap(fd: RawFd) -> io::Result<Self> {
        Ok(SharedFD(Arc::new(AutoClosingFD::dup_wrap(fd)?)))
    }

    fn dup(&self) -> io::Result<Self> {
        Ok(SharedFD(self.0.clone()))
    }
}

impl AsRawFd for SharedFD {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl Clone for SharedFD {
    fn clone(&self) -> Self {
        self.dup().unwrap()
    }
}
