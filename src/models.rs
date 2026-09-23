//! Library models: wrappers whose outputs carry default obligations.
//!
//! A value handed out by these wrappers must be [`discharge`]d or explicitly
//! [`abandon`]ed; dropping it (e.g. through cancellation) is reported.
//!
//! [`discharge`]: crate::Obligation::discharge
//! [`abandon`]: crate::Obligation::abandon

use std::future::Future;
use std::panic::Location;

use crate::Obligation;

/// Wrap the output of `fut` in an obligation, attributed to the caller.
#[track_caller]
pub fn oblige<F: Future>(fut: F, label: &'static str) -> impl Future<Output = Obligation<F::Output>> {
    let site = Location::caller();
    async move { Obligation::at(fut.await, label, site) }
}

pub mod mpsc {
    use std::future::Future;
    use std::panic::Location;

    use tokio::sync::mpsc;

    use crate::Obligation;

    macro_rules! receiver {
        ($name:ident, $inner:ty) => {
            /// Receiver whose messages must be delivered, requeued, or explicitly abandoned.
            pub struct $name<T> {
                inner: $inner,
                label: &'static str,
            }

            impl<T> $name<T> {
                pub fn new(inner: $inner, label: &'static str) -> Self {
                    Self { inner, label }
                }

                /// Cancel-safe like the underlying `recv`: a message is only taken on `Ready`.
                #[track_caller]
                pub fn recv(&mut self) -> impl Future<Output = Option<Obligation<T>>> + '_ {
                    let site = Location::caller();
                    async move {
                        let label = self.label;
                        self.inner.recv().await.map(|v| Obligation::at(v, label, site))
                    }
                }

                #[track_caller]
                pub fn try_recv(&mut self) -> Result<Obligation<T>, mpsc::error::TryRecvError> {
                    let site = Location::caller();
                    self.inner.try_recv().map(|v| Obligation::at(v, self.label, site))
                }

                pub fn into_inner(self) -> $inner {
                    self.inner
                }
            }

            impl<T> From<$inner> for $name<T> {
                fn from(inner: $inner) -> Self {
                    Self::new(inner, "received message")
                }
            }
        };
    }

    receiver!(Receiver, mpsc::Receiver<T>);
    receiver!(UnboundedReceiver, mpsc::UnboundedReceiver<T>);
}
