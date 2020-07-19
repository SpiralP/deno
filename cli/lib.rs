#![allow(dead_code)]
#![allow(unused_imports)]

extern crate dissimilar;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate futures;
#[macro_use]
extern crate serde_json;
extern crate clap;
extern crate deno_core;
extern crate indexmap;
#[cfg(unix)]
extern crate nix;
extern crate rand;
extern crate regex;
extern crate reqwest;
extern crate serde;
extern crate serde_derive;
extern crate tokio;
extern crate url;

mod checksum;
pub mod colors;
pub mod deno_dir;
pub mod diagnostics;
mod diff;
mod disk_cache;
mod doc;
mod file_fetcher;
pub mod flags;
mod flags_allow_net;
mod fmt;
pub mod fmt_errors;
mod fs;
pub mod global_state;
mod global_timer;
pub mod http_cache;
mod http_util;
mod import_map;
mod inspector;
pub mod installer;
mod js;
mod lint;
mod lockfile;
mod metrics;
mod module_graph;
pub mod msg;
pub mod op_error;
pub mod ops;
pub mod permissions;
mod repl;
pub mod resolve_addr;
pub mod signal;
pub mod source_maps;
mod startup_data;
pub mod state;
mod swc_util;
mod test_runner;
mod tokio_util;
mod tsc;
mod upgrade;
pub mod version;
mod web_worker;
pub mod worker;

pub use deno_lint::dprint_plugin_typescript;
pub use deno_lint::swc_common;
pub use deno_lint::swc_ecma_ast;
pub use deno_lint::swc_ecma_parser;
pub use deno_lint::swc_ecma_visit;

use crate::doc::parser::DocFileLoader;
use crate::file_fetcher::SourceFile;
use crate::file_fetcher::SourceFileFetcher;
use crate::fs as deno_fs;
use crate::global_state::GlobalState;
use crate::msg::MediaType;
use crate::op_error::OpError;
use crate::permissions::Permissions;
use crate::tsc::TargetLib;
use crate::worker::MainWorker;
use deno_core::v8_set_flags;
use deno_core::Deps;
use deno_core::ErrBox;
use deno_core::EsIsolate;
use deno_core::ModuleSpecifier;
use flags::DenoSubcommand;
use flags::Flags;
use futures::channel::oneshot;
use futures::future::Either;
use futures::future::FutureExt;
use futures::Future;
use log::Level;
use log::Metadata;
use log::Record;
use state::exit_unstable;
use std::cell::Cell;
use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;
use std::thread;
use upgrade::upgrade_command;
use url::Url;

lazy_static! {
  static ref RUNNING_TASKS: Mutex<HashMap<u64, (thread::JoinHandle<()>, oneshot::Sender<()>)>> =
    Default::default();
}

fn get_id() -> u64 {
  lazy_static! {
    static ref ID: Mutex<u64> = Mutex::new(1);
  }

  let id = &mut *ID.lock().unwrap();

  let new_id = *id;
  *id = new_id + 1;
  new_id
}

#[no_mangle]
pub extern "C" fn cancel(id: u64) {
  let things = &mut *RUNNING_TASKS.lock().unwrap();
  if let Some((join_handle, killer)) = things.remove(&id) {
    let _ignore = killer.send(());
    join_handle.join().unwrap();
  }
}

thread_local!(
  static MY_ID: Cell<u64> = {
    let id = get_id();
    Cell::new(id)
  };
);

/// returns task id or 0 if failed
#[no_mangle]
pub extern "C" fn run(c_code: *const u8, len: u64) -> u64 {
  let code = unsafe { std::slice::from_raw_parts(c_code, len as usize) };
  let code = code.to_vec();

  let flags = flags::flags_from_vec(vec![
    "deno".to_string(),
    "run".to_string(),
    "-".to_string(),
  ]);

  if let Some(ref v8_flags) = flags.v8_flags {
    let mut v8_flags_ = v8_flags.clone();
    v8_flags_.insert(0, "UNUSED_BUT_NECESSARY_ARG0".to_string());
    v8_set_flags(v8_flags_);
  }

  let id = MY_ID.with(|c| c.get());
  let things = &mut *RUNNING_TASKS.lock().unwrap();
  let (tx, rx) = oneshot::channel();

  let join_handle = thread::spawn(move || {
    let fut = run_command(rx, flags, code).boxed_local();

    let result = tokio_util::run_basic(fut);
    if let Err(err) = result {
      let msg = format!("{}: {}", colors::red_bold("error"), err.to_string());
      eprintln!("{}", msg);
    }
  });
  things.insert(id, (join_handle, tx));

  id
}

async fn run_command<T: Into<Vec<u8>>>(
  killer: oneshot::Receiver<()>,
  flags: Flags,
  code: T,
) -> Result<(), ErrBox> {
  println!("{:#?}", flags);

  let global_state = GlobalState::new(flags.clone())?;
  let main_module =
    ModuleSpecifier::resolve_url_or_path("./__$deno$stdin.ts").unwrap();

  let mut worker =
    MainWorker::create(global_state.clone(), main_module.clone())?;
  {
    let main_module_url = main_module.as_url().to_owned();

    // Create a dummy source file.
    let source_file = SourceFile {
      filename: main_module_url.to_file_path().unwrap(),
      url: main_module_url,
      types_header: None,
      media_type: MediaType::TypeScript,
      source_code: code.into(),
    };

    // Save our fake file into file fetcher cache
    // to allow module access by TS compiler (e.g. op_fetch_source_files)
    worker
      .state
      .borrow()
      .global_state
      .file_fetcher
      .save_source_file_in_cache(&main_module, source_file);
  }

  match futures::future::select(
    async move {
      debug!("main_module {}", main_module);
      worker.execute_module(&main_module).await?;
      worker.execute("window.dispatchEvent(new Event('load'))")?;
      (&mut *worker).await?;
      worker.execute("window.dispatchEvent(new Event('unload'))")?;
      Ok::<_, ErrBox>(())
    }
    .boxed_local(),
    killer,
  )
  .await
  {
    Either::Left((result, _kill_future)) => {
      result?;
    }

    Either::Right((_result, running_future)) => {
      drop(running_future);
    }
  }

  Ok(())
}

#[test]
fn test_lib() {
  let code = "console.log('it works!')";
  let c_code = code.as_bytes();
  run(c_code.as_ptr(), c_code.len() as u64);
}
