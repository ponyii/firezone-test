use std::net::SocketAddrV4;
use std::{io::ErrorKind, mem::MaybeUninit, sync::Arc, time::Duration, time::Instant};

use pnet::packet::{
    icmp::{
        echo_reply::EchoReplyPacket, echo_request::MutableEchoRequestPacket, IcmpCode, IcmpPacket,
        IcmpTypes,
    },
    ipv4::Ipv4Packet,
    Packet,
};
use socket2::{SockAddr, Socket};
use tokio::sync::oneshot;

type Time = std::time::Instant;

const ADDRESS: &str = "8.8.8.8";
// Identifier randomization would be necessary to use multiple ICMP servers at once.
const IDENTIFIER: u16 = 100;
const PING_COUNT: u16 = 10;
const PING_INTERVAL_MS: u64 = 1;
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
    let socket_for_listener = Arc::new(socket);
    let socket_for_sender = socket_for_listener.clone();

    let mut request_buf = [0; MutableEchoRequestPacket::minimum_packet_size()];

    let mut receivers = Vec::with_capacity(PING_COUNT as usize);
    let mut senders = Vec::with_capacity(PING_COUNT as usize);
    for _ in 0..PING_COUNT {
        let (tx, rx) = oneshot::channel::<oneshot::Sender<Time>>();
        receivers.push(rx);
        senders.push(tx);
    }
    senders.reverse();

    let mut handles = Vec::with_capacity(PING_COUNT as usize);

    tokio::spawn(async move {
        let mut buf: [MaybeUninit<u8>; RESPONSE_BYTES] = [MaybeUninit::uninit(); RESPONSE_BYTES];
        loop {
            let len = read_socket(&socket_for_listener, &mut buf).await;
            let received_at = Instant::now();
            let seq = validate_icmp_response_packet(&buf, len).unwrap();
            match receivers[seq as usize].try_recv() {
                Ok(tx) => tx.send(received_at).unwrap(),
                Err(_) => (), // TODO: handle errors
            };
        }
    });

    let mut interval = tokio::time::interval(Duration::from_millis(PING_INTERVAL_MS));
    for i in 0..PING_COUNT {
        let p = create_icmp_request_packet(&mut request_buf, i, IDENTIFIER);
        socket_for_sender.send(p.packet()).unwrap();

        let sent_at = Instant::now();
        let (tx, rx) = oneshot::channel::<Time>();
        senders.pop().unwrap().send(tx).unwrap();
        handles.push(tokio::spawn(async move {
            let res = tokio::time::timeout(Duration::from_secs(ECHO_TIMEOUT_SEC), rx).await;
            match res {
                Ok(Ok(received_at)) => {
                    println!("{},{},{}", ADDRESS, i, (received_at - sent_at).as_micros())
                }
                Ok(Err(e)) => eprintln!("Receiver dropped ({}): {}", i, e),
                Err(_) => println!("Timeout ({})", i),
            }
        }));

        interval.tick().await;
    }

    for handle in handles {
        handle.await.expect("Task paniced");
    }
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
fn validate_icmp_response_packet(buf: &[MaybeUninit<u8>], len: usize) -> Option<u16> {
    // TODO: proper validation & error handling
    if len != RESPONSE_BYTES {
        return None;
    }
    let safe_buf: [u8; RESPONSE_BYTES] = std::array::from_fn(|i| unsafe { buf[i].assume_init() });
    let packet = Ipv4Packet::new(&safe_buf[0..len])?;
    let echo_reply = EchoReplyPacket::new(packet.payload())?;
    Some(echo_reply.get_sequence_number())
}

async fn read_socket(socket: &Arc<Socket>, buf: &mut [MaybeUninit<u8>]) -> usize {
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
