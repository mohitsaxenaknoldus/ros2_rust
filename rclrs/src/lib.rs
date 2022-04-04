extern crate alloc;
extern crate core_error;
extern crate downcast;
extern crate parking_lot;
extern crate rosidl_runtime_rs;
extern crate std;
use parking_lot::Mutex;

use alloc::sync::Arc;
use std::future::Future;
use std::task::Poll;
use std::task::RawWaker;
use std::task::RawWakerVTable;
use std::task::Waker;

pub mod context;
pub mod error;
pub mod future;
pub mod node;
pub mod qos;
pub mod wait;

mod rcl_bindings;

pub use self::context::*;
pub use self::error::*;
pub use self::future::*;
pub use self::node::*;
pub use self::qos::*;
pub use self::wait::*;

use rcl_bindings::rcl_context_is_valid;
use std::time::Duration;

use std::pin::Pin;

pub use rcl_bindings::rmw_request_id_t;

/// Polls the node for new messages and executes the corresponding callbacks.
///
/// See [`WaitSet::wait`] for the meaning of the `timeout` parameter.
///
/// This may under some circumstances return
/// [`SubscriptionTakeFailed`][1], [`ClientTakeFailed`][2], [`ServiceTakeFailed`][3] when the wait
/// set spuriously wakes up.
/// This can usually be ignored.
///
/// [1]: crate::SubscriberErrorCode
/// [2]: crate::ClientErrorCode
/// [3]: crate::ServiceErrorCode
pub fn spin_once(node: &Node, timeout: Option<Duration>) -> Result<(), RclReturnCode> {
    let live_subscriptions = node.live_subscriptions();
    let live_clients = node.live_clients();
    let live_services = node.live_services();
    let ctx = Context {
        handle: node.context.clone(),
    };
    let mut wait_set = WaitSet::new(
        live_subscriptions.len(),
        0,
        0,
        live_clients.len(),
        live_services.len(),
        0,
        &ctx,
    )?;

    for live_subscription in &live_subscriptions {
        wait_set.add_subscription(live_subscription.clone())?;
    }

    for live_client in &live_clients {
        wait_set.add_client(live_client.clone())?;
    }

    for live_service in &live_services {
        wait_set.add_service(live_service.clone())?;
    }

    let ready_entities = wait_set.wait(timeout)?;

    for ready_subscription in ready_entities.subscriptions {
        ready_subscription.execute()?;
    }

    for ready_client in ready_entities.clients {
        ready_client.execute()?;
    }

    for ready_service in ready_entities.services {
        ready_service.execute()?;
    }

    Ok(())
}

/// Convenience function for calling [`rclrs::spin_once`] in a loop.
///
/// This function additionally checks that the context is still valid.
pub fn spin(node: &Node) -> Result<(), RclReturnCode> {
    // SAFETY: No preconditions for this function.
    while unsafe { rcl_context_is_valid(&mut *node.context.lock() as *mut _) } {
        if let Some(error) = spin_once(node, None).err() {
            match error {
                RclReturnCode::Timeout => continue,
                error => return Err(error),
            };
        }
    }
    Ok(())
}

#[derive(Clone)]
struct RclWaker {}

fn rclwaker_wake(s: &RclWaker) {
    let waker_ptr: *const RclWaker = s;
    let _waker_arc = unsafe { Arc::from_raw(waker_ptr) };
}

fn rclwaker_wake_by_ref(s: &RclWaker) {
    let waker_ptr: *const RclWaker = s;
    let _waker_arc = unsafe { Arc::from_raw(waker_ptr) };
}

fn rclwaker_clone(s: &RclWaker) -> RawWaker {
    let arc = unsafe { Arc::from_raw(s) };
    std::mem::forget(arc.clone());
    RawWaker::new(Arc::into_raw(arc) as *const (), &VTABLE)
}

const VTABLE: RawWakerVTable = unsafe {
    RawWakerVTable::new(
        |s| rclwaker_clone(&*(s as *const RclWaker)),
        |s| rclwaker_wake(&*(s as *const RclWaker)),
        |s| rclwaker_wake_by_ref(&*(s as *const RclWaker)),
        |s| drop(Arc::from_raw(s as *const RclWaker)),
    )
};

fn rclwaker_into_waker(s: *const RclWaker) -> Waker {
    let raw_waker = RawWaker::new(s as *const (), &VTABLE);
    unsafe { Waker::from_raw(raw_waker) }
}

pub fn spin_until_future_complete<T: Unpin + Clone>(
    node: &node::Node,
    mut future: Arc<Mutex<Box<RclFuture<T>>>>,
) -> Result<<future::RclFuture<T> as Future>::Output, RclReturnCode> {
    let rclwaker = Arc::new(RclWaker {});
    let waker = rclwaker_into_waker(Arc::into_raw(rclwaker));
    let mut cx = std::task::Context::from_waker(&waker);

    loop {
        let context_valid = unsafe { rcl_context_is_valid(&mut *node.context.lock() as *mut _) };
        if context_valid {
            if let Some(error) = spin_once(node, None).err() {
                match error {
                    RclReturnCode::Timeout => continue,
                    error => return Err(error),
                };
            };
            match Future::poll(Pin::new(&mut *future.lock()), &mut cx) {
                Poll::Ready(val) => break Ok(val),
                Poll::Pending => continue,
            };
        }
    }
}
