use std::{mem::MaybeUninit, sync::Arc, time::Duration, time::Instant};

use pnet::packet::icmp::echo_request::MutableEchoRequestPacket;
use socket2::Socket;
use tokio::{
    sync::{oneshot, oneshot::Receiver, oneshot::Sender},
    task::JoinHandle,
};

use utils::{
    read_socket, send_echo_request, socket, validate_icmp_response_packet, RESPONSE_BYTES,
};

pub mod utils;

type Time = std::time::Instant;

// TODO: get from command line
const ADDRESS: &str = "8.8.8.8";
const PING_COUNT: u16 = 10;
// Current implementation does not allow setting interval _significantly_ less
// than 1 ms as this interval must be much longer than tokio task spawning time.
const PING_INTERVAL_MS: u64 = 1;

const ECHO_TIMEOUT_SEC: u64 = 5;

enum Message {
    Ok(u16, Duration),
    Timeout(u16),
    UnexpectedEcho(u16),
}

#[cfg(not(test))]
impl Message {
    fn publish(self) {
        match self {
            Self::Ok(seq, dur) => println!("{},{},{}", ADDRESS, seq, dur.as_micros()),
            Self::Timeout(seq) => println!("Timeout ({})", seq),
            Self::UnexpectedEcho(seq) => println!("Unexpected echo reply ({})", seq),
        }
    }
}

fn create_oneshots(num: usize) -> (Vec<Sender<Sender<Time>>>, Vec<Receiver<Sender<Time>>>) {
    let mut receivers = Vec::with_capacity(num);
    let mut senders = Vec::with_capacity(num);
    for _ in 0..num {
        let (tx, rx) = oneshot::channel::<oneshot::Sender<Time>>();
        receivers.push(rx);
        senders.push(tx);
    }
    (senders, receivers)
}

// Send requests and spawn response awaiting tasks for each
async fn send_requsts(
    num: u16,
    socket: Arc<Socket>,
    mut senders: Vec<Sender<Sender<Time>>>,
) -> Vec<JoinHandle<()>> {
    let mut request_buf = [0; MutableEchoRequestPacket::minimum_packet_size()];
    let mut handles = Vec::with_capacity(num as usize);
    senders.reverse(); // Get ready to element `pop`ping

    let mut interval = tokio::time::interval(Duration::from_millis(PING_INTERVAL_MS));
    for i in 0..num {
        send_echo_request(&socket, &mut request_buf, i);
        let sent_at = Instant::now();
        let (tx, rx) = oneshot::channel::<Time>();
        senders.pop().unwrap().send(tx).unwrap();
        handles.push(tokio::spawn(async move {
            let res = tokio::time::timeout(Duration::from_secs(ECHO_TIMEOUT_SEC), rx).await;
            match res {
                Ok(Ok(received_at)) => {
                    let dur = received_at - sent_at;
                    Message::Ok(i, dur).publish();
                }
                Ok(Err(e)) => eprintln!("Sender dropped ({}): {}", i, e),
                Err(_) => Message::Timeout(i).publish(),
            }
        }));
        interval.tick().await;
    }
    handles
}

#[tokio::main]
async fn main() {
    let socket = socket(ADDRESS);
    let socket_for_listener = Arc::new(socket);
    let socket_for_sender = socket_for_listener.clone();

    // `senders[i]` and `receiver[i]` pertain to the `i`th sent request.
    let (senders, mut receivers) = create_oneshots(PING_COUNT as usize);

    // Spawn a listener
    tokio::spawn(async move {
        let mut buf: [MaybeUninit<u8>; RESPONSE_BYTES] = [MaybeUninit::uninit(); RESPONSE_BYTES];
        loop {
            let len = read_socket(&socket_for_listener, &mut buf).await;
            let received_at = Instant::now();
            let seq = validate_icmp_response_packet(&buf, len).unwrap();
            // Inform the dedicated task about the received response.
            match receivers[seq as usize].try_recv() {
                Ok(tx) => {
                    if let Err(_) = tx.send(received_at) {
                        eprintln!("Receiver dropped ({})", seq);
                    }
                }
                // It doesn't seem usefull to separate the cases of
                // too early and duplicated responses.
                Err(_) => Message::UnexpectedEcho(seq).publish(),
            };
        }
    });

    let handles = send_requsts(PING_COUNT, socket_for_sender, senders).await;
    for handle in handles {
        handle.await.expect("Task paniced");
    }
}

#[cfg(test)]
mod test {
    use crate::*;

    use std::mem::discriminant;

    static mut OUT: Vec<Message> = vec![];

    // Our test design makes concurrent `publish`ing impossible,
    // so I don't really feel like using locks here
    impl Message {
        pub fn publish(self) {
            unsafe { OUT.push(self) };
        }
    }

    async fn run_tasks(senders: Vec<Sender<Sender<Time>>>) {
        let fake_socket = Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM, // avoid permission checks on Linux
            Some(socket2::Protocol::ICMPV4),
        )
        .unwrap();
        let handles = send_requsts(PING_COUNT, Arc::new(fake_socket), senders).await;
        for handle in handles {
            handle.await.expect("Task paniced");
        }
    }

    // TODO: test-only parameters

    #[tokio::test]
    async fn test_no_reply() {
        let (senders, receivers) = create_oneshots(PING_COUNT as usize);
        run_tasks(senders).await;
        let expected_msg = discriminant(&Message::Timeout(0));
        unsafe {
            assert_eq!(
                OUT.len(),
                PING_COUNT as usize,
                "There're irresponsive tasks"
            );
            for m in &OUT {
                assert_eq!(discriminant(m), expected_msg, "Unexpected message");
            }
        }
    }
}
