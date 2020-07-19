use super::dispatch_json::{JsonOp, Value};
use crate::op_error::OpError;
use crate::state::State;
use deno_core::CoreIsolate;
use deno_core::CoreIsolateState;
use deno_core::ZeroCopyBuf;

#[cfg(unix)]
use super::dispatch_json::Deserialize;
#[cfg(unix)]
use futures::future::{poll_fn, FutureExt};
#[cfg(unix)]
use std::task::Waker;
#[cfg(unix)]
use tokio::signal::unix::{signal, Signal, SignalKind};

pub fn init(i: &mut CoreIsolate, s: &State) {
  i.register_op("op_mcgalaxy_test", s.stateful_json_op2(op_mcgalaxy_test));
}

fn op_mcgalaxy_test(
  _isolate_state: &mut CoreIsolateState,
  _state: &State,
  _args: Value,
  _zero_copy: &mut [ZeroCopyBuf],
) -> Result<JsonOp, OpError> {
  Ok(JsonOp::Sync(json!({
    "test": "yes",
  })))
}
