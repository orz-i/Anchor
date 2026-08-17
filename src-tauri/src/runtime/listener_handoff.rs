use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener};

/// A process-local duplicate of a live business listener.
///
/// The serving task owns a separate descriptor/handle for the same kernel
/// socket. Keeping this duplicate alive lets a future daemon generation inherit
/// the listener without releasing and rebinding the stable business port.
pub(crate) struct HandoffListener {
    listener: TcpListener,
}

impl HandoffListener {
    pub(crate) fn close(self) {
        drop(self.listener);
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
    let handoff = HandoffListener {
        listener: listener.try_clone()?,
    };
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
}
