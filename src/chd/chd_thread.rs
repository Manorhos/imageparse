use std::{sync::mpsc::{self, RecvError, SendError}, thread};

use log::{debug, error};

use chdr::{ChdFile, ChdError};
use lru::LruCache;


const NUM_HUNKS_READAHEAD: u32 = 8;
const NUM_READAHEAD_HUNKS_LOW_WATER: u32 = 2;

// Should never be less than 2 * NUM_HUNKS_READAHEAD to keep normal readahead
// hunks from being kicked out of the cache by a prefetch
const CACHE_CAPACITY: usize = 100;

struct ChdThread {
    chd: ChdFile,
    cmd_receiver: mpsc::Receiver<Command>,
    cmd_while_prefetching: Option<Command>,
    hunk_sender: mpsc::SyncSender<Result<Vec<u8>, ChdError>>,

    num_hunks: u32,
    hunk_len: usize,

    hunk_cache: LruCache<u32, Vec<u8>>,
}

impl ChdThread {
    fn start(chd: ChdFile,
        cmd_receiver: mpsc::Receiver<Command>,
        hunk_sender: mpsc::SyncSender<Result<Vec<u8>, ChdError>>) -> thread::JoinHandle<()>
    {
        let num_hunks = chd.num_hunks();
        let hunk_len = chd.hunk_len() as usize;
        let chd_thread = ChdThread {
            chd,
            cmd_receiver,
            cmd_while_prefetching: None,
            hunk_sender,

            num_hunks,
            hunk_len,

            hunk_cache: LruCache::new(CACHE_CAPACITY),
        };

        thread::spawn(move || {
            chd_thread.run();
        })
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

    fn handle_command(&mut self, cmd: Command) -> Result<(), SendError<Result<Vec<u8>, ChdError>>> {
        match cmd {
            Command::ReadHunk(hunk_no) => {
                let t = std::time::Instant::now();

                let to_send = self.read_hunk(hunk_no);
                self.hunk_sender.send(to_send)?;
                debug!("Sent hunk {} after {} µs", hunk_no, t.elapsed().as_micros());

                let mut low_water_range = (hunk_no + 1)..=(hunk_no + NUM_READAHEAD_HUNKS_LOW_WATER);
                if self.num_hunks > hunk_no + NUM_READAHEAD_HUNKS_LOW_WATER &&
                    !low_water_range.all(|x| self.hunk_cache.contains(&x))
                {
                    for i in 1..NUM_HUNKS_READAHEAD {
                        if let Ok(new_cmd) = self.cmd_receiver.try_recv() {
                            self.cmd_while_prefetching = Some(new_cmd);
                            break;
                        }
                        self.read_hunk_to_cache(hunk_no + i);
                    }
                }
            },
            Command::PrefetchHunk(hunk_no) => {
                for i in 0..NUM_HUNKS_READAHEAD {
                    if let Ok(new_cmd) = self.cmd_receiver.try_recv() {
                        self.cmd_while_prefetching = Some(new_cmd);
                        break;
                    }
                    self.read_hunk_to_cache(hunk_no + i);
                }
            }
        }
        Ok(())
    }

    fn read_hunk(&mut self, hunk_no: u32) -> Result<Vec<u8>, ChdError> {
        if hunk_no >= self.num_hunks {
            return Err(ChdError::OutOfRange);
        }

        if let Some(buf) = self.hunk_cache.get(&hunk_no) {
            debug!("Hunk {} is in cache", hunk_no);
            Ok(buf.clone())
        } else {
            let t = std::time::Instant::now();
            let mut buf = vec![0; self.hunk_len];
            self.chd.read_hunk(hunk_no, &mut buf[..])?;
            self.hunk_cache.put(hunk_no, buf.clone());
            debug!("Hunk {} not in cache, fetching from CHD took {:?}", hunk_no, t.elapsed());
            Ok(buf)
        }
    }

    fn read_hunk_to_cache(&mut self, hunk_no: u32) {
        let t = std::time::Instant::now();
        if hunk_no >= self.num_hunks {
            return;
        }

        if !self.hunk_cache.contains(&hunk_no) {
            let mut buf = vec![0; self.hunk_len];
            if self.chd.read_hunk(hunk_no, &mut buf[..]).is_ok() {
                self.hunk_cache.put(hunk_no, buf);
                debug!("Prefetching hunk {} took {:?}", hunk_no, t.elapsed());
            }
        }
    }
}

pub struct ChdHunkReader {
    _handle: thread::JoinHandle<()>,
    hunk_read_pending: bool,

    cmd_sender: mpsc::SyncSender<Command>,
    hunk_receiver: mpsc::Receiver<Result<Vec<u8>, ChdError>>,
}

impl ChdHunkReader {
    pub fn new(chd: ChdFile) -> ChdHunkReader {
        let (cmd_sender, cmd_receiver) = mpsc::sync_channel(2);
        let (hunk_sender, hunk_receiver) = mpsc::sync_channel(1);

        ChdHunkReader {
            _handle: ChdThread::start(chd, cmd_receiver, hunk_sender),
            hunk_read_pending: false,

            cmd_sender,
            hunk_receiver,
        }
    }

    pub fn read_hunk(&mut self, hunk_no: u32) {
        if let Err(e) = self.cmd_sender.send(Command::ReadHunk(hunk_no)) {
            error!("Error sending hunk read command: {:?}", e);
        }
        self.hunk_read_pending = true;
    }

    pub fn recv_hunk(&mut self) -> Result<Result<Vec<u8>, ChdError>, RecvError>  {
        assert!(self.hunk_read_pending);
        let hunk_result = self.hunk_receiver.recv();
        self.hunk_read_pending = false;
        hunk_result
    }

    pub fn prefetch_hunk(&mut self, hunk_no: u32) {
        if let Err(e) = self.cmd_sender.try_send(Command::PrefetchHunk(hunk_no)) {
            debug!("Prefetch failed: {:?}", e);
        }
    }

    pub fn hunk_read_pending(&self) -> bool {
        self.hunk_read_pending
    }
}

enum Command {
    // 1st param: hunk number, 2nd param: last hunk buffer for recycling
    ReadHunk(u32),
    PrefetchHunk(u32)
}