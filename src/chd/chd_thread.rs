use std::{sync::mpsc, thread};

use log::{debug, error};

use chdr::{ChdFile, ChdError};

struct ChdThread {
    chd: ChdFile,
    cmd_receiver: mpsc::Receiver<Command>,
    hunk_sender: mpsc::SyncSender<Vec<u8>>,

    hunk: Vec<u8>,
    num_hunks: u32,
    hunk_len: usize,

    readahead: Option<u32>,
}

impl ChdThread {
    fn start(chd: ChdFile,
        cmd_receiver: mpsc::Receiver<Command>,
        hunk_sender: mpsc::SyncSender<Vec<u8>>) -> thread::JoinHandle<()>
    {
        let num_hunks = chd.num_hunks();
        let hunk_len = chd.hunk_len() as usize;
        let chd_thread = ChdThread {
            chd,
            cmd_receiver,
            hunk_sender,

            hunk: vec![0u8; hunk_len],
            num_hunks,
            hunk_len,

            readahead: None,
        };

        thread::spawn(move || {
            chd_thread.run();
        })
    }

    fn run(mut self) {
        while let Ok(cmd) = self.cmd_receiver.recv() {
            match cmd {
                Command::ReadHunk(hunk_no, recycled_hunk_buf) => {
                    let t = std::time::Instant::now();
                    if self.readahead == Some(hunk_no) {
                        self.readahead = None;
                    } else {
                        if let Err(e) = self.read_hunk(hunk_no) {
                            error!("Error reading hunk: {:?}", e);
                        }
                    }

                    let hunk_to_send = std::mem::replace(&mut self.hunk, recycled_hunk_buf);
                    if let Err(_) = self.hunk_sender.send(hunk_to_send) {
                        break;
                    }
                    debug!("Sent hunk {} after {} µs", hunk_no, t.elapsed().as_micros());

                    if hunk_no + 1 < self.num_hunks {
                        let t = std::time::Instant::now();
                        if self.read_hunk(hunk_no + 1).is_ok() {
                            self.readahead = Some(hunk_no + 1);
                        }
                        debug!("Readahead took {} µs", t.elapsed().as_micros());
                    }
                },
                Command::PrefetchHunk(hunk_no) => {
                    if self.readahead != Some(hunk_no) {
                        let t = std::time::Instant::now();
                        self.readahead = None;
                        if self.read_hunk(hunk_no).is_ok() {
                            self.readahead = Some(hunk_no);
                        }
                        debug!("Prefetching hunk {} took {} µs", hunk_no, t.elapsed().as_micros());
                    }
                }
            }
        }
    }

    fn read_hunk(&mut self, hunk_no: u32) -> Result<(), ChdError> {
        if hunk_no >= self.num_hunks {
            error!("Hunk {} out of range", hunk_no);
        }

        assert_eq!(self.hunk.len(), self.hunk_len);

        self.chd.read_hunk(hunk_no, &mut self.hunk[..])
    }
}

pub struct ChdHunkReader {
    _handle: thread::JoinHandle<()>,
    hunk_read_pending: bool,

    cmd_sender: mpsc::SyncSender<Command>,
    hunk_receiver: mpsc::Receiver<Vec<u8>>,
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

    pub fn read_hunk(&mut self, hunk_no: u32, recycled_buf: Vec<u8>) {
        if let Err(e) = self.cmd_sender.send(Command::ReadHunk(hunk_no, recycled_buf)) {
            error!("Error sending hunk read command: {:?}", e);
        }
        self.hunk_read_pending = true;
    }

    pub fn recv_hunk(&mut self) -> Option<Vec<u8>> {
        assert!(self.hunk_read_pending);
        let hunk = self.hunk_receiver.recv().ok();
        self.hunk_read_pending = false;
        hunk
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
    ReadHunk(u32, Vec<u8>),
    PrefetchHunk(u32)
}