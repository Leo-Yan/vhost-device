// VIRTIO Input Emulation via vhost-user
//
// Copyright 2023 Linaro Ltd. All Rights Reserved.
// Leo Yan <leo.yan@linaro.org>
//
// SPDX-License-Identifier: Apache-2.0 or BSD-3-Clause

use nix::ioctl_read_buf;
use std::os::fd::AsRawFd;
use std::{convert, io, result};

use thiserror::Error as ThisError;

use evdev::Device;

use vhost::vhost_user::message::{VhostUserProtocolFeatures, VhostUserVirtioFeatures};
use vhost_user_backend::{VhostUserBackendMut, VringRwLock, VringT};
use virtio_bindings::bindings::virtio_config::VIRTIO_F_VERSION_1;
use virtio_bindings::bindings::virtio_ring::{
    VIRTIO_RING_F_EVENT_IDX, VIRTIO_RING_F_INDIRECT_DESC,
};
use virtio_queue::{DescriptorChain, QueueOwnedT, QueueT};

use vm_memory::{
    Bytes, GuestAddressSpace, GuestMemoryAtomic, GuestMemoryLoadGuard, GuestMemoryMmap,
};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::{EventFd, EFD_NONBLOCK};

type Result<T> = std::result::Result<T, VuInputError>;
type InputDescriptorChain = DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap<()>>>;

const QUEUE_SIZE: usize = 1024;
const NUM_QUEUES: usize = 2;

const VIRTIO_INPUT_CFG_ID_NAME: u8 = 0x01;
const VIRTIO_INPUT_CFG_ID_DEVIDS: u8 = 0x03;
const VIRTIO_INPUT_CFG_EV_BITS: u8 = 0x11;
const VIRTIO_INPUT_CFG_SIZE: usize = 128;

const EV_SYN: u8 = 0x00;
const EV_KEY: u8 = 0x01;
const EV_REL: u8 = 0x02;
const EV_ABS: u8 = 0x03;
const EV_MSC: u8 = 0x04;
const EV_SW: u8 = 0x05;

const SYN_REPORT: u8 = 0x00;

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputConfig {
    select: u8,
    subsel: u8,
    size: u8,
    reserved: [u8; 5],
    val: [u8; VIRTIO_INPUT_CFG_SIZE],
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct VuInputEvent {
    ev_type: u16,
    code: u16,
    value: u32,
}

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

unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
    ::core::slice::from_raw_parts((p as *const T) as *const u8, ::core::mem::size_of::<T>())
}

pub(crate) struct VuInputBackend {
    event_idx: bool,
    ev_dev: Device,
    pub exit_event: EventFd,
    select: u8,
    subsel: u8,
    mem: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
    ev_list: Vec<VuInputEvent>,
}

impl VuInputBackend {
    pub fn new(ev_dev: Device) -> std::result::Result<Self, std::io::Error> {
        Ok(VuInputBackend {
            event_idx: false,
            ev_dev,
            exit_event: EventFd::new(EFD_NONBLOCK).map_err(|_| VuInputError::EventFdError)?,
            select: 0,
            subsel: 0,
            mem: None,
            ev_list: Vec::new(),
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
        let evs = self.ev_dev.fetch_events().unwrap();

        for ev in evs {
            let ev_raw_data = VuInputEvent {
                ev_type: ev.event_type().0,
                code: ev.code(),
                value: ev.value() as u32,
            };

        //for ev_raw_data in &self.ev_list {

            let next_avail = vring.queue_next_avail();
            println!("next_avail:{:?}", next_avail);

            let queue_used_idx = vring.queue_used_idx().unwrap();
            println!("queue used idx: {}", queue_used_idx);

            //for ev in &self.ev_list {
            //let ev_raw_data = VuInputEvent {
            //    ev_type: ev.event_type().0,
            //    code: ev.code(),
            //    value: ev.value() as u32,
            //};
            println!("send event: {ev_raw_data:?}");

            // for ev in &self.ev_list {

            // println!("ev: {:?}", ev);

            let mem = self.mem.as_ref().unwrap().memory();

            println!("0000");

            let mut desc = vring
                .get_mut()
                .get_queue_mut()
                .pop_descriptor_chain(mem.clone());

            println!("xxxx");

            match desc {
                None => {
                    vring
                        .signal_used_queue()
                        .map_err(|_| VuInputError::SendNotificationFailed)?;

                    //vring.get_mut().get_queue_mut().go_to_previous_position();
                    //desc = vring
                    //    .get_mut()
                    //    .get_queue_mut()
                    //    .pop_descriptor_chain(mem);
                    return Ok(false);
                }
                _ => (),
            }

            println!("yyyy");

            //println!("desc2: {:?}", desc2);
            println!("1111");

            let desc_chain = desc.unwrap();

            let descriptors: Vec<_> = desc_chain.clone().collect();

            if descriptors.len() != 1 {
                println!("desc length is not 1");
            }

            let descriptor = descriptors[0];

            let ev_data = unsafe { any_as_u8_slice(&ev_raw_data) };
            //let ev_data = unsafe { any_as_u8_slice(&ev) };

            println!("2222");

            let len = desc_chain.memory().write(ev_data, descriptor.addr()).unwrap();

            println!("Sent out {0} byte", len);

            if vring.add_used(desc_chain.head_index(), len as u32).is_err() {
                println!("Couldn't return used descriptors to the ring");
            }
        }

        vring
            .signal_used_queue()
            .map_err(|_| VuInputError::SendNotificationFailed)?;

        Ok(true)
    }

    pub fn read_event_config(&self) -> Result<VuInputConfig> {
        let mut counter: u8 = 0;
        let mut index: u8 = 0;
        let mut cfg: [u8; VIRTIO_INPUT_CFG_SIZE] = [0; VIRTIO_INPUT_CFG_SIZE];
        let func: unsafe fn(nix::libc::c_int, &mut [u8]) -> nix::Result<libc::c_int>;

        match self.subsel {
            EV_KEY => {
                func = eviocgbit_key;
            }
            EV_ABS => {
                func = eviocgbit_absolute;
            }
            EV_REL => {
                func = eviocgbit_relative;
            }
            EV_MSC => {
                func = eviocgbit_misc;
            }
            EV_SW => {
                func = eviocgbit_switch;
            }
            _ => {
                return Err(VuInputError::HandleEventUnknownEvent);
            }
        }

        match unsafe { func(self.ev_dev.as_raw_fd(), &mut cfg) } {
            Err(_) => {
                return Err(VuInputError::UnexpectedInputDeviceError);
            }
            _ => (),
        }

        for val in cfg.iter() {
            index += 1;
            if *val != 0 {
                counter = index;
            }
        }

        Ok(VuInputConfig {
            select: self.select,
            subsel: self.subsel,
            size: counter,
            reserved: [0; 5],
            val: cfg,
        })
    }

    pub fn read_name_config(&self) -> Result<VuInputConfig> {
        let mut name: [u8; VIRTIO_INPUT_CFG_SIZE] = [0; VIRTIO_INPUT_CFG_SIZE];

        match unsafe { eviocgname(self.ev_dev.as_raw_fd(), name.as_mut_slice()) } {
            Ok(len) if len as usize > name.len() => {
                return Err(VuInputError::UnexpectedInputDeviceError);
            }
            Ok(len) if len <= 1 => {
                return Err(VuInputError::UnexpectedInputDeviceError);
            }
            Err(_) => {
                return Err(VuInputError::UnexpectedInputDeviceError);
            }
            _ => (),
        }

        let size = String::from_utf8(name.to_vec()).unwrap().len();

        Ok(VuInputConfig {
            select: self.select,
            subsel: 0,
            size: size as u8,
            reserved: [0; 5],
            val: name,
        })
    }

    pub fn read_id_config(&self) -> Result<VuInputConfig> {
        let input_id = self.ev_dev.input_id();
        let mut dev_id = unsafe { any_as_u8_slice(&input_id).to_vec() };

        dev_id.resize(VIRTIO_INPUT_CFG_SIZE, 0);

        Ok(VuInputConfig {
            select: VIRTIO_INPUT_CFG_ID_DEVIDS,
            subsel: 0,
            size: VIRTIO_INPUT_CFG_SIZE as u8,
            reserved: [0; 5],
            val: dev_id.try_into().unwrap(),
        })
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
        1 << VIRTIO_F_VERSION_1
            | 1 << VIRTIO_RING_F_INDIRECT_DESC
            | 1 << VIRTIO_RING_F_EVENT_IDX
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }

    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        VhostUserProtocolFeatures::MQ | VhostUserProtocolFeatures::CONFIG
    }

    fn get_config(&self, offset: u32, size: u32) -> Vec<u8> {
        let cfg;

        match self.select {
            VIRTIO_INPUT_CFG_ID_NAME => {
                cfg = self.read_name_config();
            }
            VIRTIO_INPUT_CFG_ID_DEVIDS => {
                cfg = self.read_id_config();
            }
            VIRTIO_INPUT_CFG_EV_BITS => {
                cfg = self.read_event_config();
            }
            _ => {
                // Return zeros for unsupported config types
                return vec![0; size as usize];
            }
        }

        match cfg {
            Ok(val) => unsafe { any_as_u8_slice(&val).to_vec() },
            _ => {
                vec![0; size as usize]
            }
        }
    }

    fn set_config(&mut self, offset: u32, buf: &[u8]) -> io::Result<()> {
        self.select = buf[0];
        self.subsel = buf[1];

        Ok(())
    }

    fn set_event_idx(&mut self, enabled: bool) {
        println!("set_event_idx: enabled={}", enabled);
        dbg!(self.event_idx = enabled);
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

        if self.event_idx == false {
            self.ev_dev.fetch_events().unwrap();
            return Ok(false);
        }

        if evset != EventSet::IN {
            return Err(VuInputError::HandleEventNotEpollIn.into());
        }

        //let evs = self.ev_dev.fetch_events().unwrap();

        //for ev in evs {
        //    let ev_raw_data = VuInputEvent {
        //        ev_type: ev.event_type().0,
        //        code: ev.code(),
        //        value: ev.value() as u32,
        //    };
        //    println!("push event: {ev_raw_data:?}");
        //    self.ev_list.push(ev_raw_data);
        //}

        let vring = &vrings[0];

        self.process_queue(vring)?;

        //let mut requests = vring
        //    .get_mut()
        //    .get_queue_mut()
        //    .iter(mem)
        //    .map_err(|_| VuInputError::DescriptorNotFound)
        //    .unwrap();

        //let mut item = requests.next();
        //println!("item:{:?}", item);

        println!("handle_event: exit!");

        Ok(false)
    }

    fn exit_event(&self, _thread_index: usize) -> Option<EventFd> {
        self.exit_event.try_clone().ok()
    }
}
