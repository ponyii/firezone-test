use std::{
    io::ErrorKind, mem::MaybeUninit, net::AddrParseError, net::SocketAddrV4, num::ParseIntError,
    sync::Arc, time::Duration,
};

use pnet::packet::{
    icmp::{
        checksum, echo_reply::EchoReplyPacket, echo_request::MutableEchoRequestPacket, IcmpCode,
        IcmpPacket, IcmpTypes,
    },
    ipv4::Ipv4Packet,
    Packet,
};
use socket2::{SockAddr, Socket};

use crate::Cfg;

// Identifier randomization would be necessary to use multiple ICMP servers at once.
const IDENTIFIER: u16 = 100;
pub const RESPONSE_BYTES: usize =
    EchoReplyPacket::minimum_packet_size() + Ipv4Packet::minimum_packet_size();

#[derive(Debug)]
pub enum AppError {
    InvalidArgs(String),
    SocketError(std::io::Error),
}

impl From<ParseIntError> for AppError {
    fn from(err: ParseIntError) -> Self {
        Self::InvalidArgs(err.to_string())
    }
}

impl From<AddrParseError> for AppError {
    fn from(err: AddrParseError) -> Self {
        Self::InvalidArgs(err.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::SocketError(err)
    }
}

pub fn to_sock_addr(a: &String) -> Result<SockAddr, AppError> {
    Ok(SocketAddrV4::new(a.parse()?, 0).into())
}

pub fn socket(cfg: &Cfg) -> Result<Socket, AppError> {
    let socket = Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::RAW,
        Some(socket2::Protocol::ICMPV4),
    )?;
    socket.connect(&to_sock_addr(&cfg.address)?)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

#[cfg(not(test))]
pub fn send_echo_request(
    socket: &Arc<Socket>,
    buf: &mut [u8; MutableEchoRequestPacket::minimum_packet_size()],
    seq: u16,
) -> Result<(), AppError> {
    let p = create_icmp_request_packet(buf, seq, IDENTIFIER);
    socket.send(p.packet())?;
    Ok(())
}

#[cfg(test)]
pub fn send_echo_request(
    _: &Arc<Socket>,
    _: &mut [u8; MutableEchoRequestPacket::minimum_packet_size()],
    _: u16,
) -> Result<(), AppError> {
    Ok(())
}

// From the template.
pub fn create_icmp_request_packet(
    buf: &mut [u8; MutableEchoRequestPacket::minimum_packet_size()],
    seq: u16,
    identifier: u16,
) -> MutableEchoRequestPacket {
    let mut packet = MutableEchoRequestPacket::new(buf).unwrap();

    packet.set_icmp_type(IcmpTypes::EchoRequest);
    packet.set_icmp_code(IcmpCode(0));
    packet.set_sequence_number(seq);
    packet.set_identifier(identifier);

    let checksum = checksum(&IcmpPacket::new(packet.packet()).unwrap());
    packet.set_checksum(checksum);

    packet
}

// Validates the response and returns its sequence number.
// For the sake of simplicity only ICMP header is checked,
// any unexpected packets are silently ignored.
pub fn validate_icmp_response_packet(buf: &[MaybeUninit<u8>], len: usize) -> Option<u16> {
    if len != RESPONSE_BYTES {
        return None;
    }
    let safe_buf: [u8; RESPONSE_BYTES] = std::array::from_fn(|i| unsafe { buf[i].assume_init() });
    let packet = Ipv4Packet::new(&safe_buf[0..len])?;
    let echo_reply = EchoReplyPacket::new(packet.payload())?;
    if echo_reply.get_icmp_type() != IcmpTypes::EchoReply
        || echo_reply.get_icmp_code() != IcmpCode(0)
        || echo_reply.get_identifier() != IDENTIFIER
        || echo_reply.get_checksum() != checksum(&IcmpPacket::new(echo_reply.packet())?)
    {
        return None;
    }
    Some(echo_reply.get_sequence_number())
}

// From the template but slightly modified.
pub async fn read_socket(socket: &Arc<Socket>, buf: &mut [MaybeUninit<u8>]) -> usize {
    loop {
        match socket.recv(buf) {
            Err(e) => {
                if e.kind() == ErrorKind::WouldBlock {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                } else {
                    panic!("Something went wrong while reading the socket");
                }
            }
            Ok(res) => return res,
        }
    }
}
