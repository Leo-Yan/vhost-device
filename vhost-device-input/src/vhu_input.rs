// VIRTIO Input Emulation via vhost-user
//
// Copyright 2023 Linaro Ltd. All Rights Reserved.
// Leo Yan <leo.yan@linaro.org>
//
// SPDX-License-Identifier: Apache-2.0 or BSD-3-Clause

use evdev::Device;
use evdev::EventType;
use evdev::InputId;
use evdev::KeyCode;
use libc::_PC_NAME_MAX;
use log::warn;
use nix::{
    convert_ioctl_res, ioctl_none, ioctl_read, ioctl_read_buf, ioctl_readwrite, ioctl_write_int,
    ioctl_write_ptr, request_code_read,
};
use std::fs::File;
use std::io::Read;
use std::mem;
use std::ops::Deref;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};
use std::{convert, io, result};

use thiserror::Error as ThisError;
use vhost::vhost_user::message::{VhostUserProtocolFeatures, VhostUserVirtioFeatures};
use vhost_user_backend::{VhostUserBackendMut, VringRwLock, VringT};
use virtio_bindings::bindings::virtio_config::VIRTIO_F_VERSION_1;
use virtio_bindings::bindings::virtio_ring::{
    VIRTIO_RING_F_EVENT_IDX, VIRTIO_RING_F_INDIRECT_DESC,
};
use virtio_queue::{DescriptorChain, QueueOwnedT};
use vm_memory::{
    Bytes, GuestAddressSpace, GuestMemoryAtomic, GuestMemoryLoadGuard, GuestMemoryMmap,
};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::{EventFd, EFD_NONBLOCK};

const QUEUE_SIZE: usize = 1024;
const NUM_QUEUES: usize = 2;

const VIRTIO_INPUT_CFG_UNSET: u8 = 0x00;
const VIRTIO_INPUT_CFG_ID_NAME: u8 = 0x01;
const VIRTIO_INPUT_CFG_ID_SERIAL: u8 = 0x02;
const VIRTIO_INPUT_CFG_ID_DEVIDS: u8 = 0x03;
const VIRTIO_INPUT_CFG_PROP_BITS: u8 = 0x10;
const VIRTIO_INPUT_CFG_EV_BITS: u8 = 0x11;
const VIRTIO_INPUT_CFG_ABS_INFO: u8 = 0x12;

const EV_KEY: u8 = 0x01;
const EV_REL: u8 = 0x02;
const EV_ABS: u8 = 0x03;
const EV_MSC: u8 = 0x04;
const EV_SW: u8 = 0x05;

const KEY_CNT: u32 = 0x300;
const REL_CNT: u32 = 0x10;
const ABS_CNT: u32 = 0x40;
const MSC_CNT: u32 = 0x08;
const SW_CNT: u32 = 0x11;

type Result<T> = std::result::Result<T, VuInputError>;
type InputDescriptorChain = DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap<()>>>;

#[derive(Debug, Eq, PartialEq, ThisError)]
/// Errors related to vhost-device-rng daemon.
pub(crate) enum VuInputError {
    #[error("Descriptor not found")]
    DescriptorNotFound,
    #[error("Notification send failed")]
    SendNotificationFailed,
    #[error("Can't create eventFd")]
    EventFdError,
    #[error("Failed to handle event")]
    HandleEventNotEpollIn,
    #[error("Unknown device event")]
    HandleEventUnknownEvent,
    #[error("Too many descriptors: {0}")]
    UnexpectedDescriptorCount(usize),
    #[error("Unexpected Read Descriptor")]
    UnexpectedReadDescriptor,
    #[error("Failed to access input device")]
    UnexpectedInputDeviceAccessError,
    #[error("Failed to read from the input device")]
    UnexpectedInputDeviceError,
    #[error("Previous Time value is later than current time")]
    UnexpectedTimerValue,
}

ioctl_read_buf!(eviocgname, b'E', 0x06, u8);
ioctl_read_buf!(eviocgbit_type, b'E', 0x20, u8);
ioctl_read_buf!(eviocgbit_key, b'E', 0x21, u8);
ioctl_read_buf!(eviocgbit_relative, b'E', 0x22, u8);
ioctl_read_buf!(eviocgbit_absolute, b'E', 0x23, u8);
ioctl_read_buf!(eviocgbit_misc, b'E', 0x24, u8);
ioctl_read_buf!(eviocgbit_switch, b'E', 0x25, u8);

impl convert::From<VuInputError> for io::Error {
    fn from(e: VuInputError) -> Self {
        io::Error::new(io::ErrorKind::Other, e)
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputAbsinfo {
    min: u32,
    max: u32,
    fuzz: u32,
    flat: u32,
    res: u32,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputDevids {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputConfig {
    select: u8,
    subsel: u8,
    size: u8,
    reserved: [u8; 5],
    val: [u8; 128],
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputConfigName {
    select: u8,
    subsel: u8,
    size: u8,
    reserved: [u8; 5],
    name: [u8; 128],
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputConfigBitMap {
    select: u8,
    subsel: u8,
    size: u8,
    reserved: [u8; 5],
    bitmap: [u8; 128],
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputConfigDevId {
    select: u8,
    subsel: u8,
    size: u8,
    reserved: [u8; 5],
    devids: [u8; 128],
}

unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
    ::core::slice::from_raw_parts((p as *const T) as *const u8, ::core::mem::size_of::<T>())
}

pub(crate) struct VuInputBackend {
    ev_dev: String,
    pub exit_event: EventFd,
    mem: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
    select: u8,
    subsel: u8,
}

impl VuInputBackend {
    /// Create a new virtio rng device that gets random data from /dev/urandom.
    pub fn new(ev_dev: String) -> std::result::Result<Self, std::io::Error> {
        Ok(VuInputBackend {
            ev_dev,
            exit_event: EventFd::new(EFD_NONBLOCK).map_err(|_| VuInputError::EventFdError)?,
            mem: None,
            select: 0,
            subsel: 0,
        })
    }

    pub fn process_requests(
        &mut self,
        requests: Vec<InputDescriptorChain>,
        vring: &VringRwLock,
    ) -> Result<bool> {
        if requests.is_empty() {
            return Ok(true);
        }

        Ok(true)
    }

    /// Process the requests in the vring and dispatch replies
    fn process_queue(&mut self, vring: &VringRwLock) -> Result<bool> {
        let requests: Vec<_> = vring
            .get_mut()
            .get_queue_mut()
            .iter(self.mem.as_ref().unwrap().memory())
            .map_err(|_| VuInputError::DescriptorNotFound)?
            .collect();

        if self.process_requests(requests, vring)? {
            // Send notification once all the requests are processed
            vring
                .signal_used_queue()
                .map_err(|_| VuInputError::SendNotificationFailed)?;
        }

        Ok(true)
    }

    pub fn event_config(&self) -> VuInputConfig {
        let ev_type = self.subsel;
        let f = File::open(self.ev_dev.clone()).unwrap();
        let fd = OwnedFd::from(f);
        let mut prop: Vec<u8>;
        let mut counter: u8 = 0;
        let mut index: u8 = 0;

        match ev_type {
            EV_KEY => {
                let mut keys: [u8; KEY_CNT as usize] = [0; KEY_CNT as usize];
                unsafe { eviocgbit_key(fd.as_raw_fd(), &mut keys).unwrap() };
                prop = keys.to_vec();
            }
            EV_ABS => {
                let mut abs: [u8; ABS_CNT as usize] = [0; ABS_CNT as usize];
                unsafe { eviocgbit_absolute(fd.as_raw_fd(), &mut abs).unwrap() };
                prop = abs.to_vec();
            }
            EV_REL => {
                let mut rel: [u8; REL_CNT as usize] = [0; REL_CNT as usize];
                unsafe { eviocgbit_relative(fd.as_raw_fd(), &mut rel).unwrap() };
                prop = rel.to_vec();
            }
            EV_MSC => {
                let mut msc: [u8; MSC_CNT as usize] = [0; MSC_CNT as usize];
                unsafe { eviocgbit_misc(fd.as_raw_fd(), &mut msc).unwrap() };
                prop = msc.to_vec();
            }
            EV_SW => {
                let mut sw: [u8; SW_CNT as usize] = [0; SW_CNT as usize];
                unsafe { eviocgbit_switch(fd.as_raw_fd(), &mut sw).unwrap() };
                prop = sw.to_vec();
            }
            _ => {
                prop = vec![0; 128];
            }
        }

        prop.resize(128, 0);

        for val in prop.iter() {
            index += 1;
            if *val != 0 {
                counter = index;
            }
        }

        let config = VuInputConfig {
            select: VIRTIO_INPUT_CFG_ID_DEVIDS,
            subsel: ev_type,
            size: counter,
            reserved: [0; 5],
            val: prop.try_into().unwrap(),
        };
        println!("VuInputConfig: {:?}", config);
        config
    }
}

/// VhostUserBackend trait methods
impl VhostUserBackendMut<VringRwLock, ()> for VuInputBackend {
    fn num_queues(&self) -> usize {
        println!("num_queues");
        NUM_QUEUES
    }

    fn max_queue_size(&self) -> usize {
        println!("max_queue_size");
        QUEUE_SIZE
    }

    fn features(&self) -> u64 {
        // this matches the current libvhost defaults except VHOST_F_LOG_ALL
        1 << VIRTIO_F_VERSION_1
            | 1 << VIRTIO_INPUT_CFG_ID_NAME
            | 1 << VIRTIO_INPUT_CFG_ID_DEVIDS
            | 1 << VIRTIO_INPUT_CFG_PROP_BITS
            | 1 << VIRTIO_INPUT_CFG_EV_BITS
            | 1 << VIRTIO_INPUT_CFG_ABS_INFO
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }

    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        println!("protocol_features");

        VhostUserProtocolFeatures::CONFIG
    }

    fn get_config(&self, offset: u32, size: u32) -> Vec<u8> {
        println!(
            "get_config: offset:{} size:{} select:{} subsel:{}",
            offset, size, self.select, self.subsel
        );

        match self.select {
            VIRTIO_INPUT_CFG_ID_NAME => {
                let f = File::open(self.ev_dev.clone()).unwrap();
                let fd = OwnedFd::from(f);

                let mut name: Vec<u8> = vec![0; 128];
                unsafe { eviocgname(fd.as_raw_fd(), &mut name).unwrap() };

                let str_len = String::from_utf8(name.clone()).unwrap().len();

                let v = VuInputConfig {
                    select: VIRTIO_INPUT_CFG_ID_NAME,
                    subsel: 0,
                    size: str_len as u8,
                    reserved: [0; 5],
                    val: name.try_into().unwrap(),
                };

                unsafe { any_as_u8_slice(&v).to_vec() }
            }
            VIRTIO_INPUT_CFG_ID_DEVIDS => {
                let f = File::open(self.ev_dev.clone()).unwrap();
                let fd = OwnedFd::from(f);
                let ev_dev_file = Device::from_fd(fd).unwrap();

                let input_id = ev_dev_file.input_id();
                let mut dev_id = unsafe { any_as_u8_slice(&input_id).to_vec() };
                dev_id.resize(128, 0);

                let v = VuInputConfig {
                    select: VIRTIO_INPUT_CFG_ID_DEVIDS,
                    subsel: 0,
                    size: 128,
                    reserved: [0; 5],
                    val: dev_id.try_into().unwrap(),
                };

                let val = unsafe { any_as_u8_slice(&v).to_vec() };
                val
            }
            VIRTIO_INPUT_CFG_EV_BITS => {
                let v = self.event_config();

                let val = unsafe { any_as_u8_slice(&v).to_vec() };
                println!("select:{} subsel:{} val:{:?}", v.select, v.subsel, val);
                val
            }
            _ => {
                let v: Vec<u8> = vec![0; size as usize];
                v
            }
        }
    }

    fn set_config(&mut self, _offset: u32, _buf: &[u8]) -> io::Result<()> {
        self.select = _buf[0];
        self.subsel = _buf[1];

        println!(
            "set_config: offset:{} select:{} subsel:{}",
            _offset, self.select, self.subsel
        );
        Ok(())
    }

    fn set_event_idx(&mut self, enabled: bool) {
        println!("set_event_idx: enabled={}", enabled);
    }

    fn update_memory(
        &mut self,
        mem: GuestMemoryAtomic<GuestMemoryMmap>,
    ) -> result::Result<(), io::Error> {
        println!("update_memory");
        self.mem = Some(mem);
        Ok(())
    }

    fn handle_event(
        &mut self,
        device_event: u16,
        evset: EventSet,
        vrings: &[VringRwLock],
        _thread_id: usize,
    ) -> result::Result<bool, io::Error> {
        println!("handle_event");

        if evset != EventSet::IN {
            return Err(VuInputError::HandleEventNotEpollIn.into());
        }

        Ok(false)
    }

    fn exit_event(&self, _thread_index: usize) -> Option<EventFd> {
        self.exit_event.try_clone().ok()
    }
}
