use std::{
    mem::size_of,
    sync::atomic::{AtomicI32, Ordering},
};

struct Pipes {
    read_fd: AtomicI32,
    write_fd: AtomicI32,
}

static MEM_VALIDATE_PIPE: Pipes = Pipes {
    read_fd: AtomicI32::new(-1),
    write_fd: AtomicI32::new(-1),
};

#[inline]
#[cfg(any(target_os = "android", target_os = "linux"))]
fn create_pipe() -> std::io::Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    let ret = unsafe { libc::pipe2(fds.as_mut_ptr().cast(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }

    Ok((fds[0], fds[1]))
}

#[inline]
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn create_pipe() -> std::io::Result<(i32, i32)> {
    fn set_flags(fd: i32) -> std::io::Result<()> {
        let mut flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags == -1 {
            return Err(std::io::Error::last_os_error());
        }

        flags |= libc::FD_CLOEXEC;

        let ret = unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
        if ret == -1 {
            return Err(std::io::Error::last_os_error());
        }

        let mut flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 {
            return Err(std::io::Error::last_os_error());
        }
        flags |= libc::O_NONBLOCK;
        let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
        if ret == -1 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(())
    }

    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr().cast()) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let [ read_fd, write_fd ] = fds;

    set_flags(read_fd)?;
    set_flags(write_fd)?;
    Ok((read_fd, write_fd))
}

fn open_pipe() -> std::io::Result<()> {
    // ignore the result
    unsafe {
        let _ = libc::close(MEM_VALIDATE_PIPE.read_fd.load(Ordering::SeqCst));
        let _ = libc::close(MEM_VALIDATE_PIPE.write_fd.load(Ordering::SeqCst));
    }

    let (read_fd, write_fd) = create_pipe()?;

    MEM_VALIDATE_PIPE.read_fd.store(read_fd, Ordering::SeqCst);
    MEM_VALIDATE_PIPE.write_fd.store(write_fd, Ordering::SeqCst);

    Ok(())
}

// validate whether the address `addr` is readable through `write()` to a pipe
//
// if the second argument of `write(ptr, buf)` is not a valid address, the
// `write()` will return an error the error number should be `EFAULT` in most
// cases, but we regard all errors (except EINTR) as a failure of validation
pub fn validate(addr: *const libc::c_void) -> bool {
    // it's a short circuit for null pointer, as it'll give an error in
    // `std::slice::from_raw_parts` if the pointer is null.
    if addr.is_null() {
        return false;
    }

    const CHECK_LENGTH: usize = 2 * size_of::<*const libc::c_void>() / size_of::<u8>();

    // read data in the pipe
    let read_fd = MEM_VALIDATE_PIPE.read_fd.load(Ordering::SeqCst);
    let valid_read = loop {
        let mut buf = [0u8; CHECK_LENGTH];

        let ret = unsafe {
            libc::read(
                read_fd,
                buf.as_mut_ptr() as *mut _,
                CHECK_LENGTH,
            )
        };
        if ret == -1 {
            let err = std::io::Error::last_os_error();
            match err.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => break true,
                _ => break false,
            }
        }

        break ret > 0
    };

    if !valid_read && open_pipe().is_err() {
        return false;
    }

    let write_fd = MEM_VALIDATE_PIPE.write_fd.load(Ordering::SeqCst);
    loop {
        let buf = unsafe { std::slice::from_raw_parts(addr as *const u8, CHECK_LENGTH) };

        let ret = unsafe { libc::write(write_fd, buf.as_ptr() as *const _, CHECK_LENGTH) };
        if ret == -1 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }

            break false
        }

        break ret >= 0
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn validate_stack() {
        let i = 0;

        assert!(validate(&i as *const _ as *const libc::c_void));
    }

    #[test]
    fn validate_heap() {
        let vec = vec![0; 1000];

        for i in vec.iter() {
            assert!(validate(i as *const _ as *const libc::c_void));
        }
    }

    #[test]
    fn failed_validate() {
        assert!(!validate(std::ptr::null::<libc::c_void>()));
        assert!(!validate(-1_i32 as usize as *const libc::c_void))
    }
}
