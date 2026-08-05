//! A small self-contained module to manage passing a `&mut dyn VMStore` across
//! function boundaries without it actually being a function parameter.
//!
//! Much of concurrent.rs and futures_and_streams.rs work with `Future` which
//! does not allow customizing state being passed to each poll of a future. In
//! Wasmtime, however, the mutable store is available during a calls to
//! `Future::poll`, but not across calls of `Future::poll`. That means that
//! effectively what we would ideally want is to thread `&mut dyn VMStore` as a
//! parameter to futures, but that's not possible with Rust's future trait.
//!
//! This module is the workaround to otherwise enable this which is to use
//! thread-local-storage instead to pass around this pointer. The goal of this
//! module is to enable the `set` API to pretend like it's passing a pointer as
//! a parameter to a closure and then `get` can be called to acquire this
//! parameter. This module is intentionally small and isolated to keep the
//! internal implementation details private and reduce the surface area that
//! must be audited for the `unsafe` blocks contained within.

use crate::runtime::vm::VMStore;
use crate::vm::{component_async_tls_get, component_async_tls_set};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;
use core::mem;
use core::ptr::NonNull;
use core::task::{Context, Poll, Waker};

fn tls_get() -> Option<NonNull<SetStorage>> {
    NonNull::new(component_async_tls_get().cast())
}

fn tls_set(val: Option<NonNull<SetStorage>>) {
    component_async_tls_set(match val {
        Some(v) => v.as_ptr().cast(),
        None => core::ptr::null_mut(),
    })
}

enum SetStorage {
    Present(NonNull<dyn VMStore>),
    Taken(Vec<Waker>),
}

/// Configures `store` to be available for the duration of `f` through calls to
/// the [`get`] function below.
///
/// This function will replace any prior state that was configured and overwrite
/// it. Upon `f` returning the previous state will be restored. This function
/// intentionally borrows `store` for the entire duration of `f` meaning that
/// `f` is not allowed to access `store` via Rust's borrow checker.
pub fn set<R>(store: &mut dyn VMStore, f: impl FnOnce() -> R) -> R {
    let mut storage = SetStorage::Present(NonNull::from(store));
    let _reset = ResetTls(component_async_tls_get());
    tls_set(Some(NonNull::from(&mut storage)));
    return f();

    struct ResetTls(*mut u8);

    impl Drop for ResetTls {
        fn drop(&mut self) {
            component_async_tls_set(self.0);
        }
    }
}

struct GetFuture<'a, R> {
    f: Option<Box<dyn FnOnce(&mut dyn VMStore) -> R + 'a + Send>>,
}

impl<'a, R> Future for GetFuture<'a, R> {
    type Output = R;

    fn poll(mut self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result = try_get(Some(cx.waker().clone()), |val| match val {
            TryGet::Some(store) => Some((self.f.take().unwrap())(store)),
            TryGet::None => get_failed(false),
            TryGet::Taken => None,
        });
        match result {
            Some(r) => Poll::Ready(r),
            None => Poll::Pending,
        }
    }
}

pub async fn get2<R>(f: impl FnOnce(&mut dyn VMStore) -> R + Send) -> R {
    let f = Box::new(f);
    GetFuture { f: Some(f) }.await
}

/// Acquires a reference to the previous store configured via [`set`] above,
/// yielding this reference to the closure `f provided here.
///
/// This function will "take" the store from thread-local-storage for the
/// duration of the `get` function here. This "take" operation means that
/// recursive calls to `get` here will fail as the second one won't be able to
/// re-acquire the same pointer the first one has (due to it having `&mut`
/// exclusive access.
///
/// # Panics
///
/// This function will panic if [`set`] has not been previously called or if the
/// current pointer is taken by a previous call to [`get`] on the stack.
pub fn get<R>(f: impl FnOnce(&mut dyn VMStore) -> R) -> R {
    try_get(None, |val| match val {
        TryGet::Some(store) => f(store),
        TryGet::None => get_failed(false),
        TryGet::Taken => get_failed(true),
    })
}

#[cold]
fn get_failed(taken: bool) -> ! {
    if taken {
        panic!(
            "attempted to recursively call `Accessor::with` when the pointer \
            was already taken by a previous call to `Accessor::with`; try \
            using `RUST_BACKTRACE=1` to find two stack frames to \
            `Accessor::with` on the stack"
        );
    } else {
        panic!(
            "`Accessor::with` was called when the TLS pointer was not \
             previously set; this is likely a bug in Wasmtime and we would \
             appreciate an issue being filed to help fix this."
        );
    }
}

/// Values yielded to the [`try_get`] closure as an argument.
pub enum TryGet<'a> {
    /// The [`set`] API was not previously called, so there is no store
    /// available at all.
    None,
    /// The [`set`] API was previously called but it was then subsequently taken
    /// via a call to [`get`] meaning it's not available.
    Taken,
    /// The [`set`] API was previously called and this is the store that it was
    /// called with.
    Some(&'a mut dyn VMStore),
}

/// Same as [`get`] except that this does not panic if `set` has not been
/// called.
pub fn try_get<R>(waker: Option<Waker>, f: impl FnOnce(TryGet<'_>) -> R) -> R {
    // SAFETY: This is The Unsafe Block of this module on which everything
    // hinges. The overall idea is that the pointer previously provided to
    // `set` is passed to the closure here but only at most once because it's
    // passed mutably. Thus there's a number of things that this takes care of:
    //
    // * The lifetime in `TryGet` that's handed out is anonymous via the
    //   type signature of `f`, meaning that it cannot be safely persisted
    //   outside that closure. That means that once `f` is returned this
    //   function has exclusive access to the store again.
    //
    // * If TLS is not set then that means `set` has not been configured,
    //   thus `TryGet::None` is yielded.
    //
    // * If TLS is set then we're guaranteed it's set for the entire
    //   lifetime of this function call, and we're also guaranteed that the
    //   pointer stored in there is the same pointer we'll be modifying for
    //   this whole function call.
    //
    // * The TLS pointer is read/written only in a scoped manner here and
    //   borrows of this value are not persisted for very long.
    //
    // With all of that put together it should make it such that this is a safe
    // reborrow of the store provided to `set` to pass to the closure `f` here.
    unsafe {
        let storage = tls_get();
        let _reset;
        let val = match storage {
            Some(mut storage) => {
                match mem::replace(storage.as_mut(), SetStorage::Taken(Vec::new())) {
                    SetStorage::Taken(mut v) => {
                        if let Some(waker) = waker {
                            v.push(waker);
                        }
                        _reset = ResetStorage::Taken(storage, v);
                        TryGet::Taken
                    }
                    SetStorage::Present(mut ptr) => {
                        _reset = ResetStorage::Some(storage, ptr);
                        TryGet::Some(ptr.as_mut())
                    }
                }
            }
            None => TryGet::None,
        };
        return f(val);
    }

    enum ResetStorage {
        Some(NonNull<SetStorage>, NonNull<dyn VMStore>),
        Taken(NonNull<SetStorage>, Vec<Waker>),
    }

    impl Drop for ResetStorage {
        fn drop(&mut self) {
            match self {
                Self::Some(storage, store) => unsafe {
                    if let SetStorage::Taken(wakers) =
                        mem::replace(storage.as_mut(), SetStorage::Present(*store))
                    {
                        for waker in wakers {
                            waker.wake();
                        }
                    } else {
                        panic!("TLS corrupted");
                    }
                },
                Self::Taken(storage, v) => unsafe {
                    *storage.as_mut() = SetStorage::Taken(v.clone());
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TryGet, get, set, try_get};
    use crate::{AsContextMut, Engine, Store};
    use alloc::sync::Arc;
    use alloc::task::Wake;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::Waker;

    #[test]
    fn test_simple() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());

        set(store.as_context_mut().0, || {
            get(|_| {});
            try_get(None, |t| {
                assert!(matches!(t, TryGet::Some(_)));
            });
        });
    }

    #[test]
    fn test_try_get() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());

        try_get(None, |t| {
            assert!(matches!(t, TryGet::None));
            try_get(None, |t| {
                assert!(matches!(t, TryGet::None));
            });
        });
        set(store.as_context_mut().0, || {
            get(|_| {
                try_get(None, |t| {
                    assert!(matches!(t, TryGet::Taken));
                    try_get(None, |t| {
                        assert!(matches!(t, TryGet::Taken));
                    });
                });
            });
            try_get(None, |t| {
                assert!(matches!(t, TryGet::Some(_)));
                try_get(None, |t| {
                    assert!(matches!(t, TryGet::Taken));
                    try_get(None, |t| {
                        assert!(matches!(t, TryGet::Taken));
                    });
                });
            });
            try_get(None, |t| {
                assert!(matches!(t, TryGet::Some(_)));
                try_get(None, |t| {
                    assert!(matches!(t, TryGet::Taken));
                });
            });
        });
        try_get(None, |t| {
            assert!(matches!(t, TryGet::None));
        });
    }

    #[test]
    #[should_panic(expected = "attempted to recursively call")]
    fn test_get_panic() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());

        set(store.as_context_mut().0, || {
            get(|_| {
                get(|_| {
                    panic!("should not get here");
                });
            });
        });
    }

    /// A [`Waker`] that counts how many times it has been woken.
    ///
    /// Used to observe when `try_get` wakes wakers that were registered while
    /// the store was [`TryGet::Taken`].
    struct CountingWaker(AtomicUsize);

    impl CountingWaker {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicUsize::new(0)))
        }

        /// The number of times this waker has been woken so far.
        fn count(&self) -> usize {
            self.0.load(Ordering::Relaxed)
        }
    }

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A waker registered while the store is taken is woken exactly once when
    /// the holder of the store releases it, and not a moment before.
    #[test]
    fn test_waker_woken_on_release() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());

        let waker = CountingWaker::new();

        set(store.as_context_mut().0, || {
            get(|_| {
                // The store is currently held by this `get`, so a `try_get`
                // that passes a waker observes `Taken` and registers the waker
                // to be woken on release.
                try_get(Some(Waker::from(waker.clone())), |t| {
                    assert!(matches!(t, TryGet::Taken));
                });

                // The store has not been released yet, so the waker must not
                // have been woken.
                assert_eq!(waker.count(), 0);
            });

            // Returning from `get` released the store, which must have woken the
            // waker registered above exactly once...
            assert_eq!(waker.count(), 1);

            // ...and the store is available again.
            try_get(None, |t| assert!(matches!(t, TryGet::Some(_))));
        });
    }

    /// Every waker registered while the store is taken is woken when the store
    /// is released, not just the first or the last one.
    #[test]
    fn test_all_wakers_woken_on_release() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());

        let waker1 = CountingWaker::new();
        let waker2 = CountingWaker::new();

        set(store.as_context_mut().0, || {
            get(|_| {
                try_get(Some(Waker::from(waker1.clone())), |t| {
                    assert!(matches!(t, TryGet::Taken));
                });
                try_get(Some(Waker::from(waker2.clone())), |t| {
                    assert!(matches!(t, TryGet::Taken));
                });

                assert_eq!(waker1.count(), 0);
                assert_eq!(waker2.count(), 0);
            });

            // Both wakers registered while the store was taken are woken.
            assert_eq!(waker1.count(), 1);
            assert_eq!(waker2.count(), 1);
        });
    }

    /// When the store is available, `try_get` hands it out directly and a waker
    /// passed alongside is simply dropped, never registered nor woken.
    #[test]
    fn test_waker_ignored_when_store_available() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());

        let waker = CountingWaker::new();

        set(store.as_context_mut().0, || {
            try_get(Some(Waker::from(waker.clone())), |t| {
                assert!(matches!(t, TryGet::Some(_)));
            });
        });

        // The waker was dropped unused; it must never have been woken.
        assert_eq!(waker.count(), 0);
    }
}
