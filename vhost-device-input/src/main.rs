//
// Copyright 2023 Linaro Ltd. All Rights Reserved.
// Leo Yan <leo.yan@linaro.org>
//
// SPDX-License-Identifier: Apache-2.0 or BSD-3-Clause

mod vhu_input;

use log::{error, info, warn};
use std::os::fd::AsRawFd;
use std::process::exit;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};

use clap::Parser;
use thiserror::Error as ThisError;
use vhost::{vhost_user, vhost_user::Listener};
use vhost_user_backend::VhostUserDaemon;
use vm_memory::{GuestMemoryAtomic, GuestMemoryMmap};

use vhu_input::VuInputBackend;
use vmm_sys_util::epoll::EventSet;

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, ThisError)]
/// Errors related to vhost-device-input daemon.
pub(crate) enum Error {
    #[error("Event device file doesn't exists or can't be accessed")]
    AccessEventDeviceFile,
    #[error("Threads can't be joined")]
    FailedJoiningThreads,
    #[error("Could not create backend: {0}")]
    CouldNotCreateBackend(std::io::Error),
    #[error("Could not create daemon: {0}")]
    CouldNotCreateDaemon(vhost_user_backend::Error),
    #[error("Failed while parsing to String")]
    ParseFailure,
}

#[derive(Clone, Parser, Debug, PartialEq)]
#[clap(author, version, about, long_about = None)]
struct InputArgs {
    // Location of vhost-user Unix domain socket.
    #[clap(short, long)]
    socket_path: String,

    // Path for reading input events.
    #[clap(short = 'e', long)]
    event_list: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VuInputConfig {
    pub socket_path: String,
    pub events: Vec<String>,
    pub count: u32,
}

impl TryFrom<InputArgs> for VuInputConfig {
    type Error = Error;

    fn try_from(args: InputArgs) -> Result<Self> {
        let socket_path = args.socket_path.trim().to_string();
        let event_list: Vec<&str> = args.event_list.trim().split(',').collect();
        let mut count = 0;
        let mut events: Vec<String> = Vec::new();

        for ev_str in event_list.iter() {
            let ev = ev_str.to_string();
            events.push(ev.clone());
            count += 1;
        }

        println!("events: {:?} count: {:?}", events, count);

        Ok(VuInputConfig {
            socket_path,
            events,
            count,
        })
    }
}

pub(crate) fn start_backend(config: VuInputConfig) -> Result<()> {
    let mut handles = Vec::new();

    for i in 0..config.count {
        let socket = format!("{}{}", config.socket_path.to_owned(), i);
        let event = config.events.get(i as usize).unwrap().clone();

        println!("socket: {} event: {}", socket, event);

        let handle: JoinHandle<Result<()>> = thread::spawn(move || loop {
            let event_owned = event.to_owned();
            println!("event_owned: {:?}", event_owned);

            let ev_dev =
                evdev::Device::open(event.to_owned()).map_err(|_| Error::AccessEventDeviceFile)?;
            let raw_fd = ev_dev.as_raw_fd();

            println!("raw_fd: {}", raw_fd);

            // If creating the VuRngBackend isn't successull there isn't much else to do than
            // killing the thread, which .unwrap() does.  When that happens an error code is
            // generated and displayed by the runtime mechanic.  Killing a thread doesn't affect
            // the other threads spun-off by the daemon.
            let vu_input_backend = Arc::new(RwLock::new(
                VuInputBackend::new(ev_dev).map_err(Error::CouldNotCreateBackend)?,
            ));

            println!("Create daemon");

            let mut daemon = VhostUserDaemon::new(
                String::from("vhost-device-input-backend"),
                Arc::clone(&vu_input_backend),
                GuestMemoryAtomic::new(GuestMemoryMmap::new()),
            )
            .map_err(Error::CouldNotCreateDaemon)?;

            println!("XXXXXXXXXXXXXXX");
            let handlers = daemon.get_epoll_handlers();
            let ret = handlers[0].register_listener(
                raw_fd,
                EventSet::IN,
                vhu_input::EPOLL_IN_VRING_EVENT_ID,
            );
            println!("register_listener result: {:?}", ret);

            let listener = Listener::new(socket.clone(), true).unwrap();
            daemon.start(listener).unwrap();

            match daemon.wait() {
                Ok(()) => {
                    info!("Stopping cleanly.");
                }
                Err(vhost_user_backend::Error::HandleRequest(
                    vhost_user::Error::PartialMessage | vhost_user::Error::Disconnected,
                )) => {
                    info!("vhost-user connection closed with partial message. If the VM is shutting down, this is expected behavior; otherwise, it might be a bug.");
                }
                Err(e) => {
                    warn!("Error running daemon: {:?}", e);
                }
            }

            // No matter the result, we need to shut down the worker thread.
            vu_input_backend
                .read()
                .unwrap()
                .exit_event
                .write(1)
                .expect("Shutting down worker thread");
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().map_err(|_| Error::FailedJoiningThreads)??;
    }

    Ok(())
}

fn main() {
    env_logger::init();

    if let Err(e) = VuInputConfig::try_from(InputArgs::parse()).and_then(start_backend) {
        error!("{e}");
        exit(1);
    }
}
