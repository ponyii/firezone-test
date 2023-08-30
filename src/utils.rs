use std::{io::ErrorKind, mem::MaybeUninit, net::SocketAddrV4, sync::Arc, time::Duration};

use pnet::packet::{
    icmp::{
        echo_reply::EchoReplyPacket, echo_request::MutableEchoRequestPacket, IcmpCode, IcmpPacket,
        IcmpTypes,
    },
    ipv4::Ipv4Packet,
    Packet,
};
use socket2::{SockAddr, Socket};

// Identifier randomization would be necessary to use multiple ICMP servers at once.
const IDENTIFIER: u16 = 100;
pub const RESPONSE_BYTES: usize =
    EchoReplyPacket::minimum_packet_size() + Ipv4Packet::minimum_packet_size();

fn to_sock_addr(a: &str) -> SockAddr {
    SocketAddrV4::new(a.parse().unwrap(), 0).into()
}

pub fn socket(address: &str) -> Socket {
    let socket = Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::RAW,
        Some(socket2::Protocol::ICMPV4),
    )
    .unwrap();
    socket.connect(&to_sock_addr(address)).unwrap();
    socket.set_nonblocking(true).unwrap();
    socket
}

#[cfg(not(test))]
pub fn send_echo_request(
    socket: &Arc<Socket>,
    buf: &mut [u8; MutableEchoRequestPacket::minimum_packet_size()],
    seq: u16,
) {
    let p = create_icmp_request_packet(buf, seq, IDENTIFIER);
    socket.send(p.packet()).unwrap();
}

#[cfg(test)]
pub fn send_echo_request(
    _: &Arc<Socket>,
    _: &mut [u8; MutableEchoRequestPacket::minimum_packet_size()],
    _: u16,
) {
}

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

    let checksum = pnet::packet::icmp::checksum(&IcmpPacket::new(packet.packet()).unwrap());
    packet.set_checksum(checksum);

    packet
}

// Validates the response and returns its sequence number.
pub fn validate_icmp_response_packet(buf: &[MaybeUninit<u8>], len: usize) -> Option<u16> {
    // TODO: proper validation & error handling
    if len != RESPONSE_BYTES {
        return None;
    }
    let safe_buf: [u8; RESPONSE_BYTES] = std::array::from_fn(|i| unsafe { buf[i].assume_init() });
    let packet = Ipv4Packet::new(&safe_buf[0..len])?;
    let echo_reply = EchoReplyPacket::new(packet.payload())?;
    Some(echo_reply.get_sequence_number())
}

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
