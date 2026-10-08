//! Throwaway local terminal channel inspector and raw input replay client.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use hypervisor_core::channel::{
    Capabilities, Control, Encoding, Frame, MAX_FRAME, MAX_SEQUENCED_BYTES, Mode, OpenRequest,
    OpenTarget, VERSION, WireSize,
};

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(socket), Some(session)) = (args.next(), args.next()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: terminal-debug SOCKET SESSION [RAW-INPUT-FILE]",
        ));
    };
    let recording = args.next();
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many arguments",
        ));
    }
    let mut stream = UnixStream::connect(socket)?;
    let request = Frame::OpenRequest(OpenRequest {
        versions: vec![VERSION],
        target: OpenTarget::Session(session),
        mode: if recording.is_some() {
            Mode::ReadWrite
        } else {
            Mode::ReadOnly
        },
        encoding: Encoding::Bytes,
        size: WireSize { cols: 80, rows: 24 },
        client: Capabilities {
            terminal: "terminal-debug".into(),
            flags: 0,
        },
        resume: None,
        max_frames_per_second: None,
    });
    send(&mut stream, &request)?;
    let response = read_frame(&mut stream)?;
    println!("{response:?}");
    if !matches!(response, Frame::OpenResponse(_)) {
        return Ok(());
    }
    if let Some(path) = recording {
        send(&mut stream, &Frame::Control(Control::Take))?;
        for chunk in fs::read(Path::new(&path))?.chunks(MAX_SEQUENCED_BYTES) {
            send(&mut stream, &Frame::Input(chunk.to_vec()))?;
        }
    }
    loop {
        match read_frame(&mut stream) {
            Ok(frame) => println!("{frame:?}"),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

fn send(stream: &mut UnixStream, frame: &Frame) -> io::Result<()> {
    let bytes = frame
        .encode()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
    stream.write_all(&bytes)
}

fn read_frame(stream: &mut UnixStream) -> io::Result<Frame> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if !(3..=MAX_FRAME).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut bytes = Vec::with_capacity(4 + length);
    bytes.extend_from_slice(&prefix);
    bytes.resize(4 + length, 0);
    stream.read_exact(&mut bytes[4..])?;
    match Frame::decode(&bytes) {
        Ok(Some((frame, consumed))) if consumed == bytes.len() => Ok(frame),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid frame")),
    }
}
