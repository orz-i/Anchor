use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener};

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};

/// A process-local handoff token for a live business listener.
///
/// Unix keeps a duplicate descriptor for zero-downtime daemon handoff. Other
/// platforms do not support listener inheritance, so retaining an extra socket
/// handle would only delay deterministic port release during stop/restart.
pub(crate) struct HandoffListener {
    #[cfg(unix)]
    listener: TcpListener,
}

#[cfg(unix)]
fn set_cloexec(fd: RawFd, enabled: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let next = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl HandoffListener {
    pub(crate) fn close(self) {
        #[cfg(unix)]
        drop(self.listener);
    }

    pub(crate) fn activate(self) -> io::Result<(tokio::net::TcpListener, HandoffListener)> {
        #[cfg(unix)]
        {
            prepare_listener(self.listener)
        }

        #[cfg(not(unix))]
        {
            let _ = self;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "listener handoff is only supported on Unix",
            ))
        }
    }

    #[cfg(unix)]
    pub(crate) fn duplicate_for_child(&self) -> io::Result<InheritableListener> {
        let listener = self.listener.try_clone()?;
        set_cloexec(listener.as_raw_fd(), false)?;
        Ok(InheritableListener { listener })
    }

    #[cfg(unix)]
    pub(crate) unsafe fn from_inherited_fd(fd: RawFd) -> io::Result<Self> {
        // SAFETY: the daemon handoff child receives an fd duplicated by
        // `duplicate_for_child`; ownership transfers to this constructor once.
        let listener = unsafe { TcpListener::from_raw_fd(fd) };
        ensure_loopback_listener(&listener)?;
        listener.set_nonblocking(true)?;
        set_cloexec(listener.as_raw_fd(), true)?;
        Ok(Self { listener })
    }
}

#[cfg(unix)]
pub(crate) struct InheritableListener {
    listener: TcpListener,
}

#[cfg(unix)]
impl InheritableListener {
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }
}

pub(crate) fn bind_loopback_listener(
    port: u16,
) -> io::Result<(tokio::net::TcpListener, HandoffListener)> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))?;
    prepare_listener(listener)
}

fn prepare_listener(
    listener: TcpListener,
) -> io::Result<(tokio::net::TcpListener, HandoffListener)> {
    ensure_loopback_listener(&listener)?;
    listener.set_nonblocking(true)?;
    #[cfg(unix)]
    let handoff = HandoffListener {
        listener: listener.try_clone()?,
    };
    #[cfg(not(unix))]
    let handoff = HandoffListener {};
    let serving = tokio::net::TcpListener::from_std(listener)?;
    Ok((serving, handoff))
}

fn ensure_loopback_listener(listener: &TcpListener) -> io::Result<()> {
    let address = listener.local_addr()?;
    let loopback = match address.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    };
    if loopback {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("handoff listener must be loopback-bound, got {address}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn duplicate_keeps_the_business_port_bound_after_serving_handle_drops() {
        let (serving, handoff) = bind_loopback_listener(0).expect("bind listener");
        let address = handoff.listener.local_addr().expect("handoff address");
        assert_eq!(serving.local_addr().expect("serving address"), address);

        drop(serving);
        assert!(
            TcpListener::bind(address).is_err(),
            "handoff duplicate must keep the kernel listener alive"
        );

        handoff.close();
        let rebound = TcpListener::bind(address).expect("port released after final listener drop");
        drop(rebound);
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn non_unix_handoff_token_does_not_retain_business_listener() {
        let (serving, handoff) = bind_loopback_listener(0).expect("bind listener");
        let address = serving.local_addr().expect("serving address");

        drop(serving);
        let rebound = TcpListener::bind(address)
            .expect("non-Unix handoff token must not retain the business listener");
        drop(rebound);
        handoff.close();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inherited_descriptor_can_be_reactivated_without_rebinding() {
        let (serving, handoff) = bind_loopback_listener(0).expect("bind listener");
        let address = serving.local_addr().expect("serving address");
        let inheritable = handoff
            .duplicate_for_child()
            .expect("duplicate inheritable listener");
        let fd = inheritable.raw_fd();
        let imported_fd = unsafe { libc::dup(fd) };
        assert!(imported_fd >= 0, "duplicate imported fd");
        let imported = unsafe { HandoffListener::from_inherited_fd(imported_fd) }
            .expect("import inherited listener");

        drop(serving);
        handoff.close();
        drop(inheritable);
        assert!(TcpListener::bind(address).is_err());

        let (reactivated, retained) = imported.activate().expect("reactivate listener");
        assert_eq!(
            reactivated.local_addr().expect("reactivated address"),
            address
        );
        drop(reactivated);
        retained.close();
        let rebound = TcpListener::bind(address).expect("port released after imported close");
        drop(rebound);
    }
}
