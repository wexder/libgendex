//! Small deterministic MyISAM/RAR fixtures built in Rust; no database or archiver executable.
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub struct TempDir(pub PathBuf);
impl TempDir {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bookjev-test-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub enum Cell<'a> {
    Int(u64),
    Text(&'a str),
}

pub fn table(
    name: &str,
    columns: &[(&str, Cell<'_>)],
    fixed: bool,
    declared_rows: u64,
) -> Vec<(String, Vec<u8>)> {
    let count = columns.len();
    // Column separators in FRM are single 0xff bytes, not UTF-8.
    let mut names = vec![255];
    for (i, (name, _)) in columns.iter().enumerate() {
        if i > 0 {
            names.push(255);
        }
        names.extend(name.as_bytes());
    }
    names.extend([255, 0]);
    let mut frm = vec![0; 68 + 288 + count * 17];
    frm[..2].copy_from_slice(&[254, 1]);
    frm[30..32].copy_from_slice(&u16::from(!fixed).to_le_bytes());
    frm[64..68].copy_from_slice(&68u32.to_le_bytes());
    frm[68 + 258..68 + 260].copy_from_slice(&(count as u16).to_le_bytes());
    frm[68 + 268..68 + 270].copy_from_slice(&(names.len() as u16).to_le_bytes());
    let mut offset = 1usize;
    let mut defs = vec![(0i16, 1usize)];
    let mut row = vec![1];
    for (i, (_, cell)) in columns.iter().enumerate() {
        let at = 68 + 288 + 17 * i;
        let (kind, width, typ) = match cell {
            Cell::Int(_) => (0, 8, 8),
            Cell::Text(_) => {
                assert!(!fixed);
                (8, 1026, 253)
            }
        };
        frm[at + 3..at + 5].copy_from_slice(&(width as u16).to_le_bytes());
        let pos = (offset as u32 + 1).to_le_bytes();
        frm[at + 5..at + 8].copy_from_slice(&pos[..3]);
        frm[at + 13] = typ;
        offset += width;
        defs.push((kind, width));
        match cell {
            Cell::Int(n) => row.extend(n.to_le_bytes()),
            Cell::Text(s) => {
                assert!(s.len() <= 1024);
                if s.len() < 255 {
                    row.push(s.len() as u8);
                } else {
                    row.push(255);
                    row.extend((s.len() as u16).to_be_bytes());
                }
                row.extend(s.as_bytes());
            }
        }
    }
    frm.extend(names);
    let mut myi = vec![0; 140 + defs.len() * 7];
    let header_len = myi.len() as u16;
    myi[..4].copy_from_slice(&[254, 254, 7, 1]);
    myi[4..6].copy_from_slice(&u16::from(!fixed).to_be_bytes());
    myi[6..8].copy_from_slice(&header_len.to_be_bytes());
    myi[10..12].copy_from_slice(&100u16.to_be_bytes());
    myi[12..14].copy_from_slice(&40u16.to_be_bytes());
    myi[28..36].copy_from_slice(&declared_rows.to_be_bytes());
    myi[84..88].copy_from_slice(&(offset as u32).to_be_bytes());
    myi[104..108].copy_from_slice(&(defs.len() as u32).to_be_bytes());
    for (i, (kind, width)) in defs.iter().enumerate() {
        let at = 140 + i * 7;
        myi[at..at + 2].copy_from_slice(&kind.to_be_bytes());
        myi[at + 2..at + 4].copy_from_slice(&(*width as u16).to_be_bytes());
    }
    let myd = if fixed {
        row
    } else {
        let mut data = vec![1];
        data.extend((row.len() as u16).to_be_bytes());
        data.extend(row);
        data
    };
    vec![
        (format!("{name}.frm"), frm),
        (format!("{name}.MYI"), myi),
        (format!("{name}.MYD"), myd),
    ]
}

pub fn books(mismatch: bool) -> Vec<(String, Vec<u8>)> {
    use Cell::*;
    let mut files = table(
        "editions",
        &[
            ("e_id", Int(42)),
            ("libgen_topic", Text("f")),
            ("visible", Text("")),
            ("title", Text("The Native Rust Archive")),
            ("author", Text("Test Author")),
            ("series_name", Text("Test Series")),
            ("publisher", Text("Publisher")),
            ("year", Text("2026")),
            ("ignored_blob", Text(&"z".repeat(800))),
        ],
        false,
        if mismatch { 2 } else { 1 },
    );
    files.extend(table(
        "editions_add_descr",
        &[
            ("e_id", Int(42)),
            ("key", Int(101)),
            ("value", Text("English")),
        ],
        false,
        1,
    ));
    files.extend(table(
        "editions_to_files",
        &[("e_id", Int(42)), ("f_id", Int(9))],
        true,
        1,
    ));
    files.extend(table(
        "files",
        &[
            ("f_id", Int(9)),
            ("libgen_id", Int(0)),
            ("fiction_id", Int(77)),
            ("libgen_topic", Text("f")),
            ("extension", Text("epub")),
            ("visible", Text("")),
            ("broken", Text("N")),
            ("md5", Text("0123456789abcdef0123456789abcdef")),
            ("filesize", Int(12345)),
            ("pages", Int(321)),
        ],
        false,
        1,
    ));
    files.extend(table(
        "elem_descr",
        &[("key", Int(101)), ("name_en", Text("Language"))],
        false,
        1,
    ));
    files
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
fn header(kind: u8, flags: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0, 0, kind];
    bytes.extend(flags.to_le_bytes());
    bytes.extend((7u16 + body.len() as u16).to_le_bytes());
    bytes.extend(body);
    let crc = crc32(&bytes[2..]) as u16;
    bytes[..2].copy_from_slice(&crc.to_le_bytes());
    bytes
}
pub fn rar(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut rar = b"Rar!\x1a\x07\x00".to_vec();
    rar.extend(header(0x73, 0, &[0; 6]));
    for (name, data) in members {
        let mut body = Vec::new();
        body.extend((data.len() as u32).to_le_bytes());
        body.extend((data.len() as u32).to_le_bytes());
        body.push(3); // Unix
        body.extend(crc32(data).to_le_bytes());
        body.extend([0; 4]); // DOS modification date
        body.extend([20, 0x30]); // RAR 2.0, stored method
        body.extend((name.len() as u16).to_le_bytes());
        body.extend(0o100644u32.to_le_bytes());
        body.extend(name.as_bytes());
        rar.extend(header(0x74, 0x8000, &body));
        rar.extend(data);
    }
    rar.extend(header(0x7b, 0, &[]));
    rar
}

pub struct FtpServer {
    pub url: String,
    pub transfers: std::sync::Arc<AtomicU64>,
    pub aborts: std::sync::Arc<AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl FtpServer {
    pub fn new(data: Vec<u8>, fail_first_transfer: bool) -> Self {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            sync::{Arc, atomic::AtomicBool},
            thread,
            time::Duration,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let transfers = Arc::new(AtomicU64::new(0));
        let aborts = Arc::new(AtomicU64::new(0));
        let run_stop = stop.clone();
        let run_transfers = transfers.clone();
        let run_aborts = aborts.clone();
        let data = Arc::new(data);
        let worker = thread::spawn(move || {
            while !run_stop.load(Ordering::Relaxed) {
                let (mut control, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => panic!("mock FTP accept: {e}"),
                };
                let data = data.clone();
                let transfers = run_transfers.clone();
                let aborts = run_aborts.clone();
                thread::spawn(move || {
                    control
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut input = BufReader::new(control.try_clone().unwrap());
                    if control.write_all(b"220 Rust fixture FTP\r\n").is_err() {
                        return;
                    }
                    let mut passive = None;
                    let mut offset = 0;
                    loop {
                        let mut line = String::new();
                        if !matches!(input.read_line(&mut line), Ok(n) if n > 0) {
                            break;
                        }
                        let (cmd, arg) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
                        let reply = match cmd {
                            "USER" => "331 Password required\r\n".to_string(),
                            "PASS" => "230 Logged in\r\n".to_string(),
                            "TYPE" => "200 Binary mode\r\n".to_string(),
                            "SIZE" => format!("213 {}\r\n", data.len()),
                            "REST" => {
                                offset = arg.parse::<usize>().unwrap();
                                "350 Restart accepted\r\n".to_string()
                            }
                            "PASV" | "EPSV" => {
                                let socket = TcpListener::bind("127.0.0.1:0").unwrap();
                                let port = socket.local_addr().unwrap().port();
                                passive = Some(socket);
                                if cmd == "EPSV" {
                                    format!("229 Entering Extended Passive Mode (|||{port}|)\r\n")
                                } else {
                                    format!(
                                        "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                                        port >> 8,
                                        port & 255
                                    )
                                }
                            }
                            "NLST" => {
                                if control.write_all(b"150 Opening listing\r\n").is_err() {
                                    break;
                                }
                                let (mut stream, _) = passive.take().unwrap().accept().unwrap();
                                let _ = stream.write_all(b"libgen_new-2026-09-06.part001.rar\r\n");
                                drop(stream);
                                "226 Listing complete\r\n".to_string()
                            }
                            "RETR" => {
                                if control.write_all(b"150 Opening data\r\n").is_err() {
                                    break;
                                }
                                let (mut stream, _) = passive.take().unwrap().accept().unwrap();
                                let attempt = transfers.fetch_add(1, Ordering::Relaxed);
                                let end = if fail_first_transfer && attempt == 0 {
                                    (offset + 100).min(data.len())
                                } else {
                                    data.len()
                                };
                                let _ = stream.write_all(&data[offset..end]);
                                drop(stream);
                                "226 Transfer complete\r\n".to_string()
                            }
                            "ABOR" => {
                                aborts.fetch_add(1, Ordering::Relaxed);
                                "226 Abort complete\r\n".to_string()
                            }
                            "QUIT" => {
                                let _ = control.write_all(b"221 Goodbye\r\n");
                                break;
                            }
                            _ => format!("500 Unknown command {cmd}\r\n"),
                        };
                        if control.write_all(reply.as_bytes()).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Self {
            url: format!("ftp://{addr}/fixture.rar"),
            transfers,
            aborts,
            stop,
            thread: Some(worker),
        }
    }
}
impl Drop for FtpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
