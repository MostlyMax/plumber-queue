use std::fs;
use std::io::{self, BufWriter, Write};
use std::net::{TcpListener, SocketAddr, IpAddr, Ipv4Addr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{thread, sync::Arc, time::Duration};

use crate::handlers::{GeneralClient, Client, ProducerClient, ConsumerClient};
use crossbeam::queue::ArrayQueue;

pub struct QueueServer {
    addr_producer: SocketAddr,
    addr_consumer: SocketAddr,
    ringbuf: Arc<ArrayQueue<String>>,
    running: Arc<AtomicBool>,
    heartbeat: u64
}

impl Drop for QueueServer {
    fn drop(&mut self) {
        if !self.ringbuf.is_empty() {
            log::info!("writing queue to disk...");
            let f = fs::File::create("test_backup").unwrap();
            let mut f = BufWriter::new(f);
            while let Some(s) = self.ringbuf.pop() {
                f.write_all(s.as_bytes()).unwrap();
                f.write_all(&[b'\n']).unwrap();
            }
            log::info!("done!");
        }
    }
}

// maybe send copy of data down channel that gets written to disk????
// idk
impl QueueServer {
    pub const DEFAULT_QUEUE_SIZE: usize = 1_000_000;
    pub const DEFAULT_PRODUCER_PORT: u16 = 8084;
    pub const DEFAULT_CONSUMER_PORT: u16 = 8085;
    pub const DEFAULT_IPV4: (u8, u8, u8, u8) = (127, 0, 0, 1);
    pub const DEFAULT_HEARTBEAT_MS: u64 = 10_000;

    #[allow(dead_code)]
    pub fn new() -> Self {
        let ringbuf: ArrayQueue<String> = ArrayQueue::new(Self::DEFAULT_QUEUE_SIZE);
        let ringbuf = Arc::new(ringbuf);
        let running = Arc::new(AtomicBool::new(true));

        let (a, b, c, d) = Self::DEFAULT_IPV4;
        let addr_producer = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            Self::DEFAULT_PRODUCER_PORT);
        let addr_consumer = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            Self::DEFAULT_CONSUMER_PORT);

        let heartbeat = Self::DEFAULT_HEARTBEAT_MS;

        Self {
            addr_producer,
            addr_consumer,
            ringbuf,
            running,
            heartbeat
        }
    }

    #[allow(dead_code)]
    pub fn new_with_size(n: usize) -> Self {
        let ringbuf: ArrayQueue<String> = ArrayQueue::new(n);
        let ringbuf = Arc::new(ringbuf);
        let running = Arc::new(AtomicBool::new(true));

        let (a, b, c, d) = Self::DEFAULT_IPV4;
        let addr_producer = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            Self::DEFAULT_PRODUCER_PORT);
        let addr_consumer = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            Self::DEFAULT_CONSUMER_PORT);

        let heartbeat = Self::DEFAULT_HEARTBEAT_MS;

        Self {
            addr_producer,
            addr_consumer,
            ringbuf,
            running,
            heartbeat
        }
    }

    #[allow(dead_code)]
    pub fn with_size(mut self, n: usize) -> Self {
        let ringbuf: ArrayQueue<String> = ArrayQueue::new(n);
        let ringbuf = Arc::new(ringbuf);

        self.ringbuf = ringbuf;
        self
    }

    #[allow(dead_code)]
    pub fn with_producer_port(mut self, port: u16) -> Self {
        let (a, b, c, d) = Self::DEFAULT_IPV4;
        let addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            port);

        self.addr_producer = addr;
        self
    }

    #[allow(dead_code)]
    pub fn with_consumer_port(mut self, port: u16) -> Self {
        let (a, b, c, d) = Self::DEFAULT_IPV4;
        let addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            port);

        self.addr_consumer = addr;
        self
    }

    #[allow(dead_code)]
    pub fn with_producer_address(mut self, addr: &SocketAddr) -> Self {
        self.addr_producer = *addr;
        self
    }

    #[allow(dead_code)]
    pub fn with_consumer_address(mut self, addr: &SocketAddr) -> Self {
        self.addr_consumer = *addr;
        self
    }

    fn client_handler<C>(listener: TcpListener, heartbeat: Duration, ringbuf: Arc<ArrayQueue<String>>, running: Arc<AtomicBool>)
            where C: From<GeneralClient> + Client + Send + 'static {

        // this is necessary for timeouts and things
        listener.set_nonblocking(false).unwrap();

        while running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, addr)) => {
                    if !running.load(Ordering::Relaxed) { break; }

                    let ringbuf_clone = ringbuf.clone();
                    let r = running.clone();

                    let client = GeneralClient::new(
                        ringbuf_clone,
                        stream,
                        heartbeat,
                        addr,
                        r
                    );

                    let client = C::from(client);
                    let _ = thread::spawn(move || client.run());
                },
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    log::trace!("waiting...");
                    thread::sleep(Duration::from_secs(1));
                    continue;
                },
                Err(e) => { log::error!("connection failed: {:#?}", e) }
            }
        }
    }

    pub fn run(self) {
        log::debug!("starting queue server...");

        let running_clone = self.running.clone();
        ctrlc::set_handler(move || {
            running_clone.store(false, Ordering::Relaxed);

            let _ = TcpStream::connect_timeout(&self.addr_consumer, Duration::from_secs(2));
            let _ = TcpStream::connect_timeout(&self.addr_producer, Duration::from_secs(2));
        }).unwrap();

        let p_listener = TcpListener::bind(self.addr_producer).unwrap();
        let c_listener = TcpListener::bind(self.addr_consumer).unwrap();
        let heartbeat = Duration::from_millis(self.heartbeat);

        log::debug!("spawning producer handler");
        let ringbuf_clone = self.ringbuf.clone();
        let running_clone = self.running.clone();
        let p_thread = thread::spawn(move || QueueServer::client_handler::<ProducerClient>(
            p_listener,
            heartbeat,
            ringbuf_clone,
            running_clone
        ));

        log::debug!("spawning consumer handler");
        let ringbuf_clone = self.ringbuf.clone();
        let running_clone = self.running.clone();
        let c_thread = thread::spawn(move || QueueServer::client_handler::<ConsumerClient>(
            c_listener,
            heartbeat,
            ringbuf_clone,
            running_clone
        ));

        log::info!("ready to accept connections!");
        c_thread.join().unwrap();
        p_thread.join().unwrap();

    }

}
