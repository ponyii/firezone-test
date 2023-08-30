use std::{mem::MaybeUninit, sync::Arc, time::Duration, time::Instant};

use pnet::packet::{Packet, icmp::echo_request::MutableEchoRequestPacket};
use tokio::sync::oneshot;

use utils::{
    create_icmp_request_packet, read_socket, socket, validate_icmp_response_packet, RESPONSE_BYTES,
};

pub mod utils;

type Time = std::time::Instant;

// TODO: get from command line
const ADDRESS: &str = "8.8.8.8";
const PING_COUNT: u16 = 10;
// Current implementation does not allow setting interval _significantly_ less
// than 1 ms as this interval must be much longer than tokio task spawning time.
const PING_INTERVAL_MS: u64 = 1;

// Identifier randomization would be necessary to use multiple ICMP servers at once.
const IDENTIFIER: u16 = 100;
const ECHO_TIMEOUT_SEC: u64 = 5;

#[tokio::main]
async fn main() {
    let socket = socket(ADDRESS);
    let socket_for_listener = Arc::new(socket);
    let socket_for_sender = socket_for_listener.clone();

    let mut request_buf = [0; MutableEchoRequestPacket::minimum_packet_size()];

    // `senders[i]` and `receiver[i]` pertain to the `i`th sent request.
    let mut receivers = Vec::with_capacity(PING_COUNT as usize);
    let mut senders = Vec::with_capacity(PING_COUNT as usize);
    for _ in 0..PING_COUNT {
        let (tx, rx) = oneshot::channel::<oneshot::Sender<Time>>();
        receivers.push(rx);
        senders.push(tx);
    }

    let mut handles = Vec::with_capacity(PING_COUNT as usize);

    // Spawn a listener
    tokio::spawn(async move {
        let mut buf: [MaybeUninit<u8>; RESPONSE_BYTES] = [MaybeUninit::uninit(); RESPONSE_BYTES];
        loop {
            let len = read_socket(&socket_for_listener, &mut buf).await;
            let received_at = Instant::now();
            let seq = validate_icmp_response_packet(&buf, len).unwrap();
            match receivers[seq as usize].try_recv() {
                Ok(tx) => {
                    if let Err(_) = tx.send(received_at) {
                        eprintln!("Receiver dropped ({})", seq);
                    }
                }
                // It doesn't seem usefull to separate the cases of
                // too early and duplicated responses.
                Err(_) => eprintln!("Unexpected echo reply ({})", seq),
            };
        }
    });

    // Send requests and spawn response awaiting tasks for each
    senders.reverse(); // Get ready to element `pop`ping
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
                Ok(Err(e)) => eprintln!("Sender dropped ({}): {}", i, e),
                Err(_) => println!("Timeout ({})", i),
            }
        }));

        interval.tick().await;
    }

    for handle in handles {
        handle.await.expect("Task paniced");
    }
}
