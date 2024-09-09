use std::{sync::mpsc::{self, RecvError, SendError}, sync::{Arc, Mutex}, thread};

use log::{debug, error};

use chd_rs::Chd;
use lru::LruCache;


const NUM_HUNKS_READAHEAD: u32 = 8;
const NUM_READAHEAD_HUNKS_LOW_WATER: u32 = 2;
const NUM_CMD_SLOTS: usize = 2;

const CACHE_CAPACITY: usize = 100;

struct ChdThread {
    chd: Chd<std::fs::File>,
    cmd_receiver: mpsc::Receiver<Command>,
    cmd_while_prefetching: Option<Command>,
    hunk_sender: mpsc::SyncSender<Result<u32, chd_rs::Error>>,

    num_hunks: u32,

    // Used to "lock" the hunk in the cache
    last_requested_hunk: u32,

    hunk_cache: Arc<Mutex<LruCache<u32, Vec<u8>>>>,
    // Intermediate buffer for the compressed data, needed for chd crate
    comp_buf: Vec<u8>,
}

impl ChdThread {
    fn start(chd: Chd<std::fs::File>,
        cmd_receiver: mpsc::Receiver<Command>,
        hunk_sender: mpsc::SyncSender<Result<u32, chd_rs::Error>>)
        -> (thread::JoinHandle<()>, Arc<Mutex<LruCache<u32, Vec<u8>>>>)
    {
        let num_hunks = chd.header().hunk_count();
        let hunk_cache = Arc::new(Mutex::new(LruCache::new(std::num::NonZero::new(CACHE_CAPACITY).unwrap())));
        let chd_thread = ChdThread {
            chd,
            cmd_receiver,
            cmd_while_prefetching: None,
            hunk_sender,

            num_hunks,

            last_requested_hunk: 0,

            hunk_cache: hunk_cache.clone(),
            comp_buf: Vec::new(),
        };

        (thread::spawn(move || {
            chd_thread.run();
        }),
        hunk_cache.clone())
    }

    fn run(mut self) {
        loop {
            let result = if let Some(cmd) = self.cmd_while_prefetching {
                self.cmd_while_prefetching = None;
                self.handle_command(cmd)
            } else if let Ok(cmd) = self.cmd_receiver.recv() {
                self.handle_command(cmd)
            } else {
                break
            };
            if result.is_err() {
                break;
            }
        }
    }

    fn handle_command(&mut self, cmd: Command) -> Result<(), SendError<Result<u32, chd_rs::Error>>> {
        debug!("Received command {:?}", cmd);
        match cmd {
            Command::ReadHunk(hunk_no) => {
                let t = std::time::Instant::now();
                self.last_requested_hunk = hunk_no;

                let result = self.read_hunk_to_cache(hunk_no);
                self.hunk_sender.send(result)?;
                debug!("Sent hunk {} after {:?}", hunk_no, t.elapsed());

                let mut low_water_range = (hunk_no + 1)..=(hunk_no + NUM_READAHEAD_HUNKS_LOW_WATER);
                let do_readahead = {
                    if self.num_hunks <= hunk_no + NUM_READAHEAD_HUNKS_LOW_WATER {
                        // We're at the end of the image
                        false
                    } else {
                        let hunk_cache = self.hunk_cache.lock().unwrap();
                        !low_water_range.all(|x| hunk_cache.contains(&x))
                    }
                };
                if do_readahead {
                    for i in 1..NUM_HUNKS_READAHEAD {
                        if let Ok(new_cmd) = self.cmd_receiver.try_recv() {
                            self.cmd_while_prefetching = Some(new_cmd);
                            break;
                        }
                        // Ignore errors for readahead
                        let _ = self.read_hunk_to_cache(hunk_no + i);
                    }
                }
            },
            Command::PrefetchHunk(hunk_no) => {
                for i in 0..NUM_HUNKS_READAHEAD {
                    // Stop prefetching
                    if let Ok(new_cmd) = self.cmd_receiver.try_recv() {
                        self.cmd_while_prefetching = Some(new_cmd);
                        break;
                    }
                    // Ignore errors when prefetching
                    let _ = self.read_hunk_to_cache(hunk_no + i);
                }
            }
        }
        Ok(())
    }

    fn read_hunk_to_cache(&mut self, hunk_no: u32) -> Result<u32, chd_rs::Error> {
        if hunk_no >= self.num_hunks {
            return Err(chd_rs::Error::HunkOutOfRange);
        }

        // Try to hold the lock for as little at a time as possible as I/O follows.
        // Lock contention shouldn't be a problem here as the main thread only acquires it
        // for the initial cache lookup and, if the hunk isn't contained then, after receiving
        // the completion.

        let hunk_contained = {
            let mut cache = self.hunk_cache.lock().unwrap();
            // Make the last explicitly read hunk the most recent one to avoid a situation
            // where it's kicked out of the cache due to too many prefetch requests
            let _ = cache.get(&self.last_requested_hunk);
            cache.contains(&hunk_no)
        };

        if !hunk_contained {
            let mut buf = self.chd.get_hunksized_buffer();
            let t = std::time::Instant::now();
            let result = self.chd.hunk(hunk_no)?.read_hunk_in(&mut self.comp_buf, &mut buf);
            if result.is_ok() {
                self.hunk_cache.lock().unwrap().put(hunk_no, buf);
                debug!("Reading hunk {} took {:?}", hunk_no, t.elapsed());
                Ok(hunk_no)
            } else {
                Err(result.unwrap_err())
            }
        } else {
            // Hunk already in cache
            Ok(hunk_no)
        }
    }
}

pub struct ChdHunkReader {
    _handle: thread::JoinHandle<()>,
    cache: Arc<Mutex<LruCache<u32, Vec<u8>>>>,
    hunk_read_pending: bool,

    cmd_sender: mpsc::SyncSender<Command>,
    completion_receiver: mpsc::Receiver<Result<u32, chd_rs::Error>>,
}

impl ChdHunkReader {
    pub fn new(chd: Chd<std::fs::File>) -> ChdHunkReader {
        let (cmd_sender, cmd_receiver) = mpsc::sync_channel(NUM_CMD_SLOTS);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(1);

        let (handle, cache) = ChdThread::start(chd, cmd_receiver, completion_sender);

        ChdHunkReader {
            _handle: handle,
            cache,
            hunk_read_pending: false,

            cmd_sender,
            completion_receiver,
        }
    }

    pub fn send_read_hunk_command(&mut self, hunk_no: u32) {
        if let Err(e) = self.cmd_sender.send(Command::ReadHunk(hunk_no)) {
            error!("Error sending hunk read command: {:?}", e);
        }
        self.hunk_read_pending = true;
    }

    // Completion contains Ok(read_hunk_no) or the error that occured when
    // trying to read the requested hunk
    pub fn recv_completion(&mut self) -> Result<Result<u32, chd_rs::Error>, RecvError>  {
        assert!(self.hunk_read_pending);
        let completion = self.completion_receiver.recv();
        self.hunk_read_pending = false;
        completion
    }

    pub fn send_prefetch_hunk_command(&mut self, hunk_no: u32) {
        if let Err(e) = self.cmd_sender.try_send(Command::PrefetchHunk(hunk_no)) {
            debug!("Prefetch failed: {:?}", e);
        }
    }

    pub fn hunk_read_pending(&self) -> bool {
        self.hunk_read_pending
    }

    pub fn get_hunk_from_cache(&mut self, hunk_no: u32) -> Option<Vec<u8>> {
        let mut cache = self.cache.lock().unwrap();
        cache.get(&hunk_no).map(|x| x.clone())
    }
}

#[derive(Debug)]
enum Command {
    // Requests to read a hunk, responds with a completion
    ReadHunk(u32),
    // Hints to prefetch a hunk, no completion response
    PrefetchHunk(u32)
}