use std::{mem::MaybeUninit, sync::Arc, time::Duration};

use pnet::packet::{
    icmp::{
        echo_reply::EchoReplyPacket, echo_request::MutableEchoRequestPacket, IcmpCode, IcmpPacket,
        IcmpTypes,
    },
    ipv4::Ipv4Packet,
    Packet,
};
use socket2::{SockAddr, Socket};

#[tokio::main]
async fn main() {
    todo!()
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

async fn read_socket_to_buf(
    sock: &Arc<Socket>,
    buf: &mut [MaybeUninit<u8>;
             EchoReplyPacket::minimum_packet_size() + Ipv4Packet::minimum_packet_size()],
) {
    // let (res, addr) = read_socket(&sock, buf).await;
}
