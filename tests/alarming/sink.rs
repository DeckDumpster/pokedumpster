// A stand-in for healthchecks.io and Pushover: records every request it gets.
//
// tests/alarming/run.sh points PKDUMP_BACKUP_PING_URL and PUSHOVER_API_URL here
// so the gate can assert what the alarming layers ACTUALLY SENT, rather than
// reading the scripts and reasoning about what they would send. Every layer had
// been reviewed and none of them had ever fired.
//
// One JSON object per request, appended to the log:
//     {"method": "GET", "path": "/hc/<token>/fail", "body": ""}
//
// Std only, so run.sh builds it with a bare `rustc` and the workspace carries
// nothing for it. Usage: sink <port> <logfile>

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::{env, thread};

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn serve(stream: TcpStream, logfile: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let (method, path) = match (parts.next(), parts.next()) {
            (Some(m), Some(p)) => (m.to_string(), p.to_string()),
            _ => return Ok(()),
        };
        let mut length = 0usize;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 {
                return Ok(());
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
        let rec = format!(
            "{{\"method\": {}, \"path\": {}, \"body\": {}}}\n",
            json_str(&method),
            json_str(&path),
            json_str(&String::from_utf8_lossy(&body))
        );
        // One write() on an O_APPEND file: concurrent connections cannot interleave.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(logfile)?
            .write_all(rec.as_bytes())?;
        let payload = "{\"status\":1}";
        write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        )?;
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let (port, logfile) = (&args[1], args[2].clone());
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    for stream in listener.incoming().flatten() {
        let logfile = logfile.clone();
        thread::spawn(move || {
            let _ = serve(stream, &logfile);
        });
    }
}
