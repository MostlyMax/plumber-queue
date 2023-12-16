use std::{sync::{Arc, atomic::{AtomicBool, Ordering, AtomicUsize}}, net::{TcpStream, SocketAddr}, time::Duration, io::{BufReader, self, BufWriter, BufRead, Write}, thread};

use crossbeam::{queue::ArrayQueue, channel::Sender};

pub trait Client: Sized {
    fn run(self) {}
}

pub struct GeneralClient {
    ringbuf: Arc<ArrayQueue<String>>,
    sync_sender: Sender<String>,
    sync_consumer: Arc<AtomicUsize>,
    stream: TcpStream,
    heartbeat: Duration,
    addr: SocketAddr,
    running: Arc<AtomicBool>,
}

impl GeneralClient {
    pub fn new(ringbuf: Arc<ArrayQueue<String>>,
               sync_sender: Sender<String>,
               sync_consumer: Arc<AtomicUsize>,
               stream: TcpStream,
               heartbeat: Duration,
               addr: SocketAddr,
               running: Arc<AtomicBool>,) -> Self {
        Self {
            ringbuf,
            sync_sender,
            sync_consumer,
            stream,
            heartbeat,
            addr,
            running,
        }
    }
}

impl Client for GeneralClient {}

pub struct ProducerClient {
    ringbuf: Arc<ArrayQueue<String>>,
    sync_sender: Sender<String>,
    _sync_consumer: Arc<AtomicUsize>,
    stream: TcpStream,
    _heartbeat: Duration,
    addr: SocketAddr,
    running: Arc<AtomicBool>,
}

impl From<GeneralClient> for ProducerClient {
    fn from(c: GeneralClient) -> Self {
        Self {
            ringbuf: c.ringbuf,
            sync_sender: c.sync_sender,
            _sync_consumer: c.sync_consumer,
            stream: c.stream,
            _heartbeat: c.heartbeat,
            addr: c.addr,
            running: c.running
        }
    }
}

impl Client for ProducerClient {
    fn run(self) {
        log::info!("({}) accepted new producer client", self.addr);

        self.stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let stream = BufReader::new(&self.stream);
        let mut stream = stream.lines();

        while self.running.load(Ordering::Relaxed) {
            let Some(line) = stream.next() else { break };

            let line = match line {
                Ok(l) => l,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    log::trace!("({}) waiting for data...", self.addr);
                    continue;
                },
                Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {
                    log::trace!("({}) waiting for data...", self.addr);
                    continue;
                },
                Err(e) => {
                    log::error!("({}) connection failed: {:#?}", self.addr, e);
                    break;
                }
            };

            log::trace!("({}) {:#?}", self.addr, line);
            self.ringbuf.force_push(line.clone());
            self.sync_sender.send(line).unwrap();

        }

        log::info!("({}) closing producer client", self.addr)
    }
}

pub struct ConsumerClient {
    ringbuf: Arc<ArrayQueue<String>>,
    stream: TcpStream,
    sync_consumer: Arc<AtomicUsize>,
    heartbeat: Duration,
    addr: SocketAddr,
    running: Arc<AtomicBool>,
}

impl Clone for ConsumerClient {
    fn clone(&self) -> Self {
        Self {
            ringbuf: self.ringbuf.clone(),
            stream: self.stream.try_clone().unwrap(),
            sync_consumer: self.sync_consumer.clone(),
            heartbeat: self.heartbeat,
            addr: self.addr,
            running: self.running.clone() }
    }
}

impl From<GeneralClient> for ConsumerClient {
    fn from(c: GeneralClient) -> Self {
        Self {
            ringbuf: c.ringbuf,
            stream: c.stream,
            sync_consumer: c.sync_consumer,
            heartbeat: c.heartbeat,
            addr: c.addr,
            running: c.running
        }
    }
}

trait KeepAlive {
    fn keepalive(&self, heartbeat: Duration, alive: Arc<AtomicBool>);
}

impl KeepAlive for TcpStream {
    fn keepalive(&self, heartbeat: Duration, alive: Arc<AtomicBool>) {
        self.set_read_timeout(Some(heartbeat)).unwrap();
        let stream = BufReader::new(self);
        let mut stream = stream.lines();

        while alive.load(Ordering::Relaxed) {
            let Some(line) = stream.next() else {
                alive.store(false, Ordering::Relaxed);
                continue;
            };

            match line {
                Ok(line) => { log::trace!("{line}") },
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    alive.store(false, Ordering::Relaxed);
                },
                Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {
                    alive.store(false, Ordering::Relaxed);
                },
                Err(_) => {
                    alive.store(false, Ordering::Relaxed);
                }
            };
        }
    }
}

impl Client for ConsumerClient {
    fn run(self) {
        log::info!("({}) accepted new consumer client", self.addr);
        let alive = Arc::new(AtomicBool::new(true));
        let a = alive.clone();

        let stream_clone = self.stream.try_clone().unwrap();
        let heartbeat = self.heartbeat;
        thread::spawn(move || stream_clone.keepalive(heartbeat, a));

        let mut stream = BufWriter::new(&self.stream);


        while self.running.load(Ordering::Relaxed) {
            if !alive.load(Ordering::Relaxed) {
                log::warn!("({}) dead heartbeat", self.addr);
                break;
            }

            let line = match self.ringbuf.pop() {
                Some(l) => l,
                None => {
                    stream.flush().unwrap();
                    log::trace!("({}) waiting for data...", self.addr);
                    thread::sleep(Duration::from_millis(1000));
                    continue;
                }
            };

            log::trace!("{:#?}", line);
            stream.write_all(line.as_bytes()).unwrap();
            stream.write_all(&[b'\n']).unwrap();
            self.sync_consumer.fetch_add(1, Ordering::Relaxed);
        }

        log::info!("({}) closing consumer client", self.addr)
    }
}
