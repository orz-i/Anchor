use std::future::Future;
use std::sync::OnceLock;

pub type JoinHandle<T> = tokio::task::JoinHandle<T>;

fn fallback_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("anchor-runtime")
            .build()
            .expect("failed to build async runtime")
    })
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.spawn(future),
        Err(_) => fallback_runtime().spawn(future),
    }
}

pub fn spawn_blocking<F, R>(function: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.spawn_blocking(function),
        Err(_) => fallback_runtime().spawn_blocking(function),
    }
}

pub fn block_on<F>(future: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| handle.block_on(future))
            }
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| fallback_runtime().block_on(future))
                        .join()
                        .expect("fallback async runtime worker panicked")
                })
            }
            _ => panic!("unsupported Tokio runtime flavor"),
        },
        Err(_) => fallback_runtime().block_on(future),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn block_on_from_current_thread_spawn_blocking_uses_fallback_runtime() {
        let value = tokio::task::spawn_blocking(|| {
            super::block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                7
            })
        })
        .await
        .expect("blocking worker");
        assert_eq!(value, 7);
    }
}
