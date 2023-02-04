use crate::protocol::{
    Address, Authenticate as AuthenticateHeader, Connect as ConnectHeader,
    Dissociate as DissociateHeader, Heartbeat as HeartbeatHeader, Packet as PacketHeader,
};
use parking_lot::Mutex;
use register_count::{Counter, Register};
use std::{
    collections::HashMap,
    mem,
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use thiserror::Error;

mod authenticate;
mod connect;
mod dissociate;
mod heartbeat;
mod packet;

pub use self::{
    authenticate::Authenticate,
    connect::Connect,
    dissociate::Dissociate,
    heartbeat::Heartbeat,
    packet::{Fragments, Packet},
};

#[derive(Clone)]
pub struct Connection<B> {
    udp_sessions: Arc<Mutex<UdpSessions<B>>>,
    task_connect_count: Counter,
    task_associate_count: Counter,
}

impl<B> Connection<B>
where
    B: AsRef<[u8]>,
{
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let task_associate_count = Counter::new();

        Self {
            udp_sessions: Arc::new(Mutex::new(UdpSessions::new(task_associate_count.clone()))),
            task_connect_count: Counter::new(),
            task_associate_count,
        }
    }

    pub fn send_authenticate(&self, token: [u8; 32]) -> Authenticate<side::Tx> {
        Authenticate::<side::Tx>::new(token)
    }

    pub fn recv_authenticate(&self, header: AuthenticateHeader) -> Authenticate<side::Rx> {
        let (token,) = header.into();
        Authenticate::<side::Rx>::new(token)
    }

    pub fn send_connect(&self, addr: Address) -> Connect<side::Tx> {
        Connect::<side::Tx>::new(self.task_connect_count.reg(), addr)
    }

    pub fn recv_connect(&self, header: ConnectHeader) -> Connect<side::Rx> {
        let (addr,) = header.into();
        Connect::<side::Rx>::new(self.task_connect_count.reg(), addr)
    }

    pub fn send_packet(
        &self,
        assoc_id: u16,
        addr: Address,
        max_pkt_size: usize,
    ) -> Packet<side::Tx, B> {
        self.udp_sessions
            .lock()
            .send_packet(assoc_id, addr, max_pkt_size)
    }

    pub fn recv_packet(&self, header: PacketHeader) -> Option<Packet<side::Rx, B>> {
        let (assoc_id, pkt_id, frag_total, frag_id, size, addr) = header.into();
        self.udp_sessions.lock().recv_packet(
            self.udp_sessions.clone(),
            assoc_id,
            pkt_id,
            frag_total,
            frag_id,
            size,
            addr,
        )
    }

    pub fn recv_packet_unrestricted(&self, header: PacketHeader) -> Packet<side::Rx, B> {
        let (assoc_id, pkt_id, frag_total, frag_id, size, addr) = header.into();
        self.udp_sessions.lock().recv_packet_unrestricted(
            self.udp_sessions.clone(),
            assoc_id,
            pkt_id,
            frag_total,
            frag_id,
            size,
            addr,
        )
    }

    pub fn send_dissociate(&self, assoc_id: u16) -> Dissociate<side::Tx> {
        self.udp_sessions.lock().send_dissociate(assoc_id)
    }

    pub fn recv_dissociate(&self, header: DissociateHeader) -> Dissociate<side::Rx> {
        let (assoc_id,) = header.into();
        self.udp_sessions.lock().recv_dissociate(assoc_id)
    }

    pub fn send_heartbeat(&self) -> Heartbeat<side::Tx> {
        Heartbeat::<side::Tx>::new()
    }

    pub fn recv_heartbeat(&self, header: HeartbeatHeader) -> Heartbeat<side::Rx> {
        let () = header.into();
        Heartbeat::<side::Rx>::new()
    }

    pub fn task_connect_count(&self) -> usize {
        self.task_connect_count.count()
    }

    pub fn task_associate_count(&self) -> usize {
        self.task_associate_count.count()
    }

    pub fn collect_garbage(&self, timeout: Duration) {
        self.udp_sessions.lock().collect_garbage(timeout);
    }
}

pub mod side {
    pub struct Tx;
    pub struct Rx;

    pub(super) enum Side<T, R> {
        Tx(T),
        Rx(R),
    }
}

struct UdpSessions<B> {
    sessions: HashMap<u16, UdpSession<B>>,
    task_associate_count: Counter,
}

impl<B> UdpSessions<B>
where
    B: AsRef<[u8]>,
{
    fn new(task_associate_count: Counter) -> Self {
        Self {
            sessions: HashMap::new(),
            task_associate_count,
        }
    }

    fn send_packet(
        &mut self,
        assoc_id: u16,
        addr: Address,
        max_pkt_size: usize,
    ) -> Packet<side::Tx, B> {
        self.sessions
            .entry(assoc_id)
            .or_insert_with(|| UdpSession::new(self.task_associate_count.reg()))
            .send_packet(assoc_id, addr, max_pkt_size)
    }

    #[allow(clippy::too_many_arguments)]
    fn recv_packet(
        &mut self,
        sessions: Arc<Mutex<Self>>,
        assoc_id: u16,
        pkt_id: u16,
        frag_total: u8,
        frag_id: u8,
        size: u16,
        addr: Address,
    ) -> Option<Packet<side::Rx, B>> {
        self.sessions.get_mut(&assoc_id).map(|session| {
            session.recv_packet(sessions, assoc_id, pkt_id, frag_total, frag_id, size, addr)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn recv_packet_unrestricted(
        &mut self,
        sessions: Arc<Mutex<Self>>,
        assoc_id: u16,
        pkt_id: u16,
        frag_total: u8,
        frag_id: u8,
        size: u16,
        addr: Address,
    ) -> Packet<side::Rx, B> {
        self.sessions
            .entry(assoc_id)
            .or_insert_with(|| UdpSession::new(self.task_associate_count.reg()))
            .recv_packet(sessions, assoc_id, pkt_id, frag_total, frag_id, size, addr)
    }

    fn send_dissociate(&mut self, assoc_id: u16) -> Dissociate<side::Tx> {
        self.sessions.remove(&assoc_id);
        Dissociate::<side::Tx>::new(assoc_id)
    }

    fn recv_dissociate(&mut self, assoc_id: u16) -> Dissociate<side::Rx> {
        self.sessions.remove(&assoc_id);
        Dissociate::<side::Rx>::new(assoc_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &mut self,
        assoc_id: u16,
        pkt_id: u16,
        frag_total: u8,
        frag_id: u8,
        size: u16,
        addr: Address,
        data: B,
    ) -> Result<Option<Assemblable<B>>, AssembleError> {
        self.sessions
            .entry(assoc_id)
            .or_insert_with(|| UdpSession::new(self.task_associate_count.reg()))
            .insert(assoc_id, pkt_id, frag_total, frag_id, size, addr, data)
    }

    fn collect_garbage(&mut self, timeout: Duration) {
        for (_, session) in self.sessions.iter_mut() {
            session.collect_garbage(timeout);
        }
    }
}

struct UdpSession<B> {
    pkt_buf: HashMap<u16, PacketBuffer<B>>,
    next_pkt_id: AtomicU16,
    _task_reg: Register,
}

impl<B> UdpSession<B>
where
    B: AsRef<[u8]>,
{
    fn new(task_reg: Register) -> Self {
        Self {
            pkt_buf: HashMap::new(),
            next_pkt_id: AtomicU16::new(0),
            _task_reg: task_reg,
        }
    }

    fn send_packet(
        &self,
        assoc_id: u16,
        addr: Address,
        max_pkt_size: usize,
    ) -> Packet<side::Tx, B> {
        Packet::<side::Tx, B>::new(
            assoc_id,
            self.next_pkt_id.fetch_add(1, Ordering::AcqRel),
            addr,
            max_pkt_size,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn recv_packet(
        &self,
        sessions: Arc<Mutex<UdpSessions<B>>>,
        assoc_id: u16,
        pkt_id: u16,
        frag_total: u8,
        frag_id: u8,
        size: u16,
        addr: Address,
    ) -> Packet<side::Rx, B> {
        Packet::<side::Rx, B>::new(sessions, assoc_id, pkt_id, frag_total, frag_id, size, addr)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &mut self,
        assoc_id: u16,
        pkt_id: u16,
        frag_total: u8,
        frag_id: u8,
        size: u16,
        addr: Address,
        data: B,
    ) -> Result<Option<Assemblable<B>>, AssembleError> {
        let res = self
            .pkt_buf
            .entry(pkt_id)
            .or_insert_with(|| PacketBuffer::new(frag_total))
            .insert(assoc_id, frag_total, frag_id, size, addr, data)?;

        if res.is_some() {
            self.pkt_buf.remove(&pkt_id);
        }

        Ok(res)
    }

    fn collect_garbage(&mut self, timeout: Duration) {
        self.pkt_buf.retain(|_, buf| buf.c_time.elapsed() < timeout);
    }
}

struct PacketBuffer<B> {
    buf: Vec<Option<B>>,
    frag_total: u8,
    frag_received: u8,
    addr: Address,
    c_time: Instant,
}

impl<B> PacketBuffer<B>
where
    B: AsRef<[u8]>,
{
    fn new(frag_total: u8) -> Self {
        let mut buf = Vec::with_capacity(frag_total as usize);
        buf.resize_with(frag_total as usize, || None);

        Self {
            buf,
            frag_total,
            frag_received: 0,
            addr: Address::None,
            c_time: Instant::now(),
        }
    }

    fn insert(
        &mut self,
        assoc_id: u16,
        frag_total: u8,
        frag_id: u8,
        size: u16,
        addr: Address,
        data: B,
    ) -> Result<Option<Assemblable<B>>, AssembleError> {
        assert_eq!(data.as_ref().len(), size as usize);

        if frag_id >= frag_total {
            return Err(AssembleError::InvalidFragmentId(frag_total, frag_id));
        }

        if frag_id == 0 && addr.is_none() {
            return Err(AssembleError::InvalidAddress(
                "no address in first fragment",
            ));
        }

        if frag_id != 0 && !addr.is_none() {
            return Err(AssembleError::InvalidAddress(
                "address in non-first fragment",
            ));
        }

        if self.buf[frag_id as usize].is_some() {
            return Err(AssembleError::DuplicatedFragment(frag_id));
        }

        self.buf[frag_id as usize] = Some(data);
        self.frag_received += 1;

        if frag_id == 0 {
            self.addr = addr;
        }

        if self.frag_received == self.frag_total {
            Ok(Some(Assemblable::new(
                mem::take(&mut self.buf),
                self.addr.take(),
                assoc_id,
            )))
        } else {
            Ok(None)
        }
    }
}

pub struct Assemblable<B> {
    buf: Vec<Option<B>>,
    addr: Address,
    assoc_id: u16,
}

impl<B> Assemblable<B>
where
    B: AsRef<[u8]>,
{
    fn new(buf: Vec<Option<B>>, addr: Address, assoc_id: u16) -> Self {
        Self {
            buf,
            addr,
            assoc_id,
        }
    }

    pub fn assemble<A>(self, buf: &mut A) -> (Address, u16)
    where
        A: Assembler<B>,
    {
        let data = self.buf.into_iter().map(|b| b.unwrap());
        buf.assemble(data);
        (self.addr, self.assoc_id)
    }
}

pub trait Assembler<B>
where
    Self: Sized,
    B: AsRef<[u8]>,
{
    fn assemble(&mut self, data: impl IntoIterator<Item = B>);
}

impl<B> Assembler<B> for Vec<u8>
where
    B: AsRef<[u8]>,
{
    fn assemble(&mut self, data: impl IntoIterator<Item = B>) {
        for d in data {
            self.extend_from_slice(d.as_ref());
        }
    }
}

#[derive(Debug, Error)]
pub enum AssembleError {
    #[error("invalid fragment id {1} in total {0} fragments")]
    InvalidFragmentId(u8, u8),
    #[error("{0}")]
    InvalidAddress(&'static str),
    #[error("duplicated fragment: {0}")]
    DuplicatedFragment(u8),
}
