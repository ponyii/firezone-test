use std::{env, mem::MaybeUninit, ops::Range, sync::Arc, time::Duration, time::Instant};

use pnet::packet::icmp::echo_request::MutableEchoRequestPacket;
use socket2::Socket;
use tokio::{
    sync::{oneshot, oneshot::Receiver, oneshot::Sender},
    task::JoinHandle,
};

use utils::{
    read_socket, send_echo_request, socket, validate_icmp_response_packet, AppError, RESPONSE_BYTES,
};

pub mod utils;

type Time = std::time::Instant;

// All the output messages for users (i.e. not developers) are registered here.
enum Message {
    Ok(String, u16, Duration),
    Timeout(u16),
    // Probably this one should be turned into developers-only error message.
    UnexpectedEcho(u16),
}

#[cfg(not(test))]
impl Message {
    fn publish(self) {
        match self {
            Self::Ok(address, seq, dur) => println!("{},{},{}", address, seq, dur.as_micros()),
            Self::Timeout(seq) => println!("Timeout ({})", seq),
            Self::UnexpectedEcho(seq) => println!("Unexpected echo reply ({})", seq),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cfg {
    address: String,
    ping_count: u16,
    // Current implementation does not allow setting interval _significantly_ less
    // than 1 ms as this interval must be much longer than tokio task spawning time.
    ping_interval_ms: u64,
    // This value is hardcoded now.
    echo_timeout_sec: u64,
}

impl Cfg {
    const PING_COUNT_RANGE: Range<u16> = 1..11;
    const PING_INTERVAL_MS_RANGE: Range<u64> = 1..1001;

    fn parse_args() -> Result<Self, AppError> {
        let args: Vec<String> = env::args().collect();
        if args.len() != 2 {
            return Err(AppError::InvalidArgs(format!(
                "Expected one arg, found {}",
                args.len() - 1
            )));
        }
        let split_args: Vec<&str> = args[1].split(',').collect();
        if split_args.len() != 3 {
            return Err(AppError::InvalidArgs(format!(
                "Can't parse the argument, 3 comma-separated pieces expected, found {}",
                split_args.len()
            )));
        }
        Ok(Self {
            address: split_args[0].to_owned(),
            ping_count: split_args[1].parse()?,
            ping_interval_ms: split_args[2].parse()?,
            echo_timeout_sec: 5,
        })
    }

    fn init() -> Result<Self, AppError> {
        let cfg = Self::parse_args()?;
        if !Self::PING_COUNT_RANGE.contains(&cfg.ping_count) {
            return Err(AppError::InvalidArgs(format!(
                "Ping count must lie in {:?}",
                Self::PING_COUNT_RANGE
            )));
        }
        if !Self::PING_INTERVAL_MS_RANGE.contains(&cfg.ping_interval_ms) {
            return Err(AppError::InvalidArgs(format!(
                "Ping interval (ms) must lie in {:?}",
                Self::PING_INTERVAL_MS_RANGE
            )));
        }
        Ok(cfg)
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

// Send requests and spawn response-awaiting tasks for each
async fn send_requsts(
    cfg: &Cfg,
    socket: Arc<Socket>,
    mut senders: Vec<Sender<Sender<Time>>>,
) -> Result<Vec<JoinHandle<()>>, AppError> {
    let mut request_buf = [0; MutableEchoRequestPacket::minimum_packet_size()];
    let mut handles = Vec::with_capacity(cfg.ping_count as usize);
    senders.reverse(); // Get ready to index-reversing `pop`ping

    let mut interval = tokio::time::interval(Duration::from_millis(cfg.ping_interval_ms));
    for i in 0..cfg.ping_count {
        send_echo_request(&socket, &mut request_buf, i)?;
        let sent_at = Instant::now();
        let (tx, rx) = oneshot::channel::<Time>();
        senders.pop().unwrap().send(tx).unwrap();
        let cfg_copy = cfg.clone();
        handles.push(tokio::spawn(async move {
            let res =
                tokio::time::timeout(Duration::from_secs(cfg_copy.echo_timeout_sec), rx).await;
            match res {
                Ok(Ok(received_at)) => {
                    let dur = received_at - sent_at;
                    Message::Ok(cfg_copy.address, i, dur).publish();
                }
                Ok(Err(e)) => eprintln!("Sender dropped ({}): {}", i, e),
                Err(_) => Message::Timeout(i).publish(),
            }
        }));
        interval.tick().await;
    }
    Ok(handles)
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    let cfg = Cfg::init()?;

    let socket = socket(&cfg)?;
    let socket_for_listener = Arc::new(socket);
    let socket_for_sender = socket_for_listener.clone();

    // `senders[i]` and `receiver[i]` pertain to the `i`th sent request.
    let (senders, mut receivers) = create_oneshots(cfg.ping_count as usize);

    // Spawn a listener
    tokio::spawn(async move {
        let mut buf: [MaybeUninit<u8>; RESPONSE_BYTES] = [MaybeUninit::uninit(); RESPONSE_BYTES];
        loop {
            let len = read_socket(&socket_for_listener, &mut buf).await;
            let received_at = Instant::now();
            // Inform the dedicated task about the received response.
            // Silently drop incorrect ones.
            if let Some(seq) = validate_icmp_response_packet(&buf, len) {
                match receivers.get_mut(seq as usize) {
                    Some(rx) => match rx.try_recv() {
                        Ok(tx) => {
                            if let Err(_) = tx.send(received_at) {
                                eprintln!("Receiver dropped ({})", seq);
                            }
                        }
                        // It doesn't seem usefull to separate the cases of
                        // too early and duplicated responses.
                        Err(_) => Message::UnexpectedEcho(seq).publish(),
                    },
                    None => eprintln!("Unexpected seq {}", seq),
                }
            }
        }
    });

    let handles = send_requsts(&cfg, socket_for_sender, senders).await?;
    for handle in handles {
        handle.await.expect("Task panicked");
    }
    Ok(())
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

    async fn run_tasks(cfg: &Cfg, senders: Vec<Sender<Sender<Time>>>) {
        let fake_socket = Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM, // avoid permission checks on Linux
            Some(socket2::Protocol::ICMPV4),
        )
        .unwrap();
        let handles = send_requsts(cfg, Arc::new(fake_socket), senders)
            .await
            .unwrap();
        for handle in handles {
            handle.await.expect("Task panicked");
        }
    }

    fn test_config() -> Cfg {
        Cfg {
            address: "".to_owned(),
            ping_count: 50,
            ping_interval_ms: 1,
            echo_timeout_sec: 1,
        }
    }

    unsafe fn check(cfg: &Cfg, expected_msg: std::mem::Discriminant<Message>) {
        assert_eq!(
            OUT.len(),
            cfg.ping_count as usize,
            "There're irresponsive tasks"
        );
        for m in &OUT {
            assert_eq!(discriminant(m), expected_msg, "Unexpected message");
        }
        OUT.clear();
    }

    #[tokio::test]
    async fn test_no_reply() {
        let cfg = test_config();
        let (senders, _receivers) = create_oneshots(cfg.ping_count as usize);
        run_tasks(&cfg, senders).await;
        let expected_msg = discriminant(&Message::Timeout(0));
        unsafe {
            check(&cfg, expected_msg);
        }
    }

    #[tokio::test]
    async fn test_quick_replies() {
        let cfg = test_config();
        let (senders, receivers) = create_oneshots(cfg.ping_count as usize);
        let cfg_copy = cfg.clone();
        // This thread sends `Instant::now()` to the `i`th tasks
        // roughly when `i + 1`th is being spawned.
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(cfg_copy.ping_interval_ms));
            for mut rx in receivers {
                interval.tick().await;
                rx.try_recv().unwrap().send(Instant::now()).unwrap();
            }
        });
        run_tasks(&cfg, senders).await;
        let expected_msg = discriminant(&Message::Ok("".to_string(), 0, Duration::default()));
        unsafe {
            check(&cfg, expected_msg);
        }
    }
}
