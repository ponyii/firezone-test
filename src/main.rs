use std::net::SocketAddrV4;
use std::{io::ErrorKind, mem::MaybeUninit, sync::Arc, time::Duration};

use pnet::packet::{
    icmp::{
        echo_reply::EchoReplyPacket, echo_request::MutableEchoRequestPacket, IcmpCode, IcmpPacket,
        IcmpTypes,
    },
    ipv4::Ipv4Packet,
    Packet,
};
use socket2::{SockAddr, Socket};

const ADDRESS: &str = "8.8.8.8";
// Identifier randomization would be necessary to use multiple ICMP servers at once.
const IDENTIFIER: u16 = 100;
const ECHO_TIMEOUT_SEC: u64 = 5;
const RESPONSE_BYTES: usize =
    EchoReplyPacket::minimum_packet_size() + Ipv4Packet::minimum_packet_size();

fn to_sock_addr(a: &str) -> SockAddr {
    SocketAddrV4::new(a.parse().unwrap(), 0).into()
}

#[tokio::main]
async fn main() {
    let socket = Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::RAW,
        Some(socket2::Protocol::ICMPV4),
    )
    .unwrap();
    socket.connect(&to_sock_addr(ADDRESS)).unwrap();
    socket.set_nonblocking(true).unwrap();

    let mut request_buf = [0; MutableEchoRequestPacket::minimum_packet_size()];
    let mut response_buf: [u8; RESPONSE_BYTES] = [0; RESPONSE_BYTES];
    let buf_ref =
        unsafe { &mut *(response_buf.as_mut_slice() as *mut [u8] as *mut [MaybeUninit<u8>]) };

    let p = create_icmp_request_packet(&mut request_buf, 0, IDENTIFIER);
    socket.send(p.packet()).unwrap();
    tokio::time::timeout(Duration::from_secs(ECHO_TIMEOUT_SEC), async {
        loop {
            let len = read_socket(&socket, buf_ref).await;
            if validate_icmp_response_packet(&response_buf, len).unwrap() == 0 {
                println!("response received");
                break;
            }
        }
    })
    .await
    .unwrap();
}

fn create_icmp_request_packet(
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
fn validate_icmp_response_packet(buf: &[u8], len: usize) -> Option<u16> {
    // TODO: proper validation & error handling
    let packet = Ipv4Packet::new(&buf[0..len])?;
    let echo_reply = EchoReplyPacket::new(packet.payload())?;
    Some(echo_reply.get_sequence_number())
}

async fn read_socket(socket: &Socket, buf: &mut [MaybeUninit<u8>]) -> usize {
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
