//! The bytes between den and den-server: frames on one pipe (ssh's stdin and
//! stdout), each
//!
//! ```text
//! u32 length of the rest | u8 kind | u32 id | u32 length of the JSON | JSON | blob
//! ```
//!
//! little-endian. The JSON says what is asked or answered; the blob carries
//! bytes as they are (a file's contents, git's output, a terminal's). A
//! message with a large blob goes out as `CHUNK` frames first, one at a
//! time between other messages, so a file on its way never holds up a
//! keystroke.

use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufWriter, Read, Write},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::JoinHandle,
};

/// den asks; `id` names the call (0: no answer wanted).
pub const REQUEST: u8 = 1;
/// The server's answer to the call `id`, which ends it.
pub const RESPONSE: u8 = 2;
/// One of the things a call sends before its answer (a batch of search
/// results, changed paths).
pub const ITEM: u8 = 3;
/// den no longer wants the call `id`.
pub const CANCEL: u8 = 4;
/// A piece of the blob of the message with this `id` that follows.
pub const CHUNK: u8 = 5;

/// A blob larger than this goes in pieces of this size.
pub const CHUNK_BYTES: usize = 48 * 1024;
/// No frame is longer: a length beyond this is not this protocol.
const MAX_FRAME: usize = 256 * 1024 * 1024;
const HEADER: usize = 9;

/// One message, whole: what a frame carries, or several `CHUNK`s and the
/// frame after them.
#[derive(Debug, PartialEq, Eq)]
pub struct Message {
    pub kind: u8,
    pub id: u32,
    pub json: Vec<u8>,
    pub blob: Vec<u8>,
}

pub(super) fn write_frame(
    out: &mut impl Write,
    kind: u8,
    id: u32,
    json: &[u8],
    blob: &[u8],
) -> io::Result<()> {
    let length = HEADER + json.len() + blob.len();
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the message is too long to send",
        ));
    }
    out.write_all(&(length as u32).to_le_bytes())?;
    out.write_all(&[kind])?;
    out.write_all(&id.to_le_bytes())?;
    out.write_all(&(json.len() as u32).to_le_bytes())?;
    out.write_all(json)?;
    out.write_all(blob)
}

/// The next frame; `None` when the pipe closed between two frames.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Message>> {
    let mut length = [0u8; 4];
    match input.read_exact(&mut length) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let length = u32::from_le_bytes(length) as usize;
    if !(HEADER..=MAX_FRAME).contains(&length) {
        return Err(invalid("a frame of an impossible length"));
    }
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    let kind = body[0];
    let id = u32::from_le_bytes(body[1..5].try_into().expect("4 bytes"));
    let json_len = u32::from_le_bytes(body[5..9].try_into().expect("4 bytes")) as usize;
    if HEADER + json_len > length {
        return Err(invalid("a frame whose JSON is longer than it is"));
    }
    let blob = body.split_off(HEADER + json_len);
    body.drain(..HEADER);
    Ok(Some(Message {
        kind,
        id,
        json: body,
        blob,
    }))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// Puts the pieces of a chunked message back together.
#[derive(Default)]
pub struct Assembler {
    partial: HashMap<u32, Vec<u8>>,
}

impl Assembler {
    /// The message `frame` completes, if it does.
    pub fn push(&mut self, mut frame: Message) -> Option<Message> {
        match frame.kind {
            CHUNK => {
                self.partial
                    .entry(frame.id)
                    .or_default()
                    .append(&mut frame.blob);
                None
            }
            // Items go out whole, and one may pass between the chunks of
            // its call's answer.
            ITEM => Some(frame),
            _ => {
                if let Some(mut blob) = self.partial.remove(&frame.id) {
                    blob.append(&mut frame.blob);
                    frame.blob = blob;
                }
                Some(frame)
            }
        }
    }
}

/// Queues messages for the thread that writes them. Sending never waits on
/// the pipe, so it is safe from the UI thread.
#[derive(Clone)]
pub struct Sender(mpsc::Sender<Message>);

impl Sender {
    /// False once the writer is gone.
    pub fn send(&self, kind: u8, id: u32, json: Vec<u8>, blob: Vec<u8>) -> bool {
        self.0
            .send(Message {
                kind,
                id,
                json,
                blob,
            })
            .is_ok()
    }
}

/// Start the thread that writes to `out`. It ends when every `Sender` is
/// gone and all is written, or when the pipe breaks.
pub fn writer(out: impl Write + Send + 'static) -> (Sender, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("den-remote-writer".into())
        .spawn(move || {
            let _ = write_all(rx, BufWriter::new(out));
        })
        .expect("a thread");
    (Sender(tx), thread)
}

fn write_all(rx: Receiver<Message>, mut out: impl Write) -> io::Result<()> {
    // Messages with a large blob, and how much of each has gone.
    let mut bulk: VecDeque<(Message, usize)> = VecDeque::new();
    let mut open = true;
    loop {
        // Small messages first, in the order they came.
        let next = match rx.try_recv() {
            Ok(message) => Some(message),
            Err(TryRecvError::Disconnected) => {
                open = false;
                None
            }
            Err(TryRecvError::Empty) if bulk.is_empty() => {
                out.flush()?;
                match rx.recv() {
                    Ok(message) => Some(message),
                    Err(_) => {
                        open = false;
                        None
                    }
                }
            }
            Err(TryRecvError::Empty) => None,
        };
        if let Some(message) = next {
            if message.blob.len() <= CHUNK_BYTES || message.kind == ITEM {
                write_frame(
                    &mut out,
                    message.kind,
                    message.id,
                    &message.json,
                    &message.blob,
                )?;
            } else {
                bulk.push_back((message, 0));
            }
            continue;
        }
        // Nothing small waiting: one piece of the next large one.
        let Some((message, sent)) = bulk.front_mut() else {
            if open {
                continue;
            }
            return out.flush();
        };
        let rest = &message.blob[*sent..];
        if rest.len() > CHUNK_BYTES {
            write_frame(&mut out, CHUNK, message.id, &[], &rest[..CHUNK_BYTES])?;
            *sent += CHUNK_BYTES;
            bulk.rotate_left(1);
        } else {
            write_frame(&mut out, message.kind, message.id, &message.json, rest)?;
            bulk.pop_front();
        }
        out.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the writer puts out for `messages`, read back.
    fn round_trip(messages: Vec<Message>) -> (Vec<Message>, Vec<u8>) {
        let (tx, rx) = mpsc::channel();
        for message in messages {
            tx.send(message).unwrap();
        }
        drop(tx);
        let mut bytes = Vec::new();
        write_all(rx, &mut bytes).unwrap();
        let mut input = bytes.as_slice();
        let mut assembler = Assembler::default();
        let mut out = Vec::new();
        while let Some(frame) = read_frame(&mut input).unwrap() {
            out.extend(assembler.push(frame));
        }
        (out, bytes)
    }

    fn message(kind: u8, id: u32, json: &str, blob: Vec<u8>) -> Message {
        Message {
            kind,
            id,
            json: json.as_bytes().to_vec(),
            blob,
        }
    }

    #[test]
    fn a_frame_comes_back_as_it_went() {
        let (out, _) = round_trip(vec![message(REQUEST, 7, r#"{"op":"ping"}"#, vec![1, 2, 3])]);
        assert_eq!(
            out,
            vec![message(REQUEST, 7, r#"{"op":"ping"}"#, vec![1, 2, 3])]
        );
    }

    #[test]
    fn a_large_blob_goes_in_pieces_and_arrives_whole() {
        let blob: Vec<u8> = (0..CHUNK_BYTES * 3 + 17).map(|i| (i % 251) as u8).collect();
        let (out, bytes) = round_trip(vec![message(RESPONSE, 3, "{}", blob.clone())]);
        assert_eq!(out, vec![message(RESPONSE, 3, "{}", blob)]);
        // Three chunks and the frame that ends it.
        let mut input = bytes.as_slice();
        let mut kinds = Vec::new();
        while let Some(frame) = read_frame(&mut input).unwrap() {
            assert!(frame.blob.len() <= CHUNK_BYTES);
            kinds.push(frame.kind);
        }
        assert_eq!(kinds, vec![CHUNK, CHUNK, CHUNK, RESPONSE]);
    }

    #[test]
    fn small_messages_pass_a_large_one_and_large_ones_take_turns() {
        let big = |fill: u8| vec![fill; CHUNK_BYTES * 2 + 1];
        let (out, bytes) = round_trip(vec![
            message(RESPONSE, 1, "{}", big(1)),
            message(RESPONSE, 2, "{}", big(2)),
            message(REQUEST, 3, "{}", vec![9]),
            message(ITEM, 1, "{}", vec![8]),
        ]);
        // The small ones were queued behind the large ones and still go first.
        let ids: Vec<(u8, u32)> = out.iter().map(|m| (m.kind, m.id)).collect();
        assert_eq!(
            ids,
            vec![(REQUEST, 3), (ITEM, 1), (RESPONSE, 1), (RESPONSE, 2)]
        );
        assert_eq!(out[2].blob, big(1));
        assert_eq!(out[3].blob, big(2));
        // An item of call 1 passed between its chunks and took none of them.
        assert_eq!(out[1].blob, vec![8]);
        let mut input = bytes.as_slice();
        let mut order = Vec::new();
        while let Some(frame) = read_frame(&mut input).unwrap() {
            order.push((frame.kind, frame.id));
        }
        assert_eq!(
            order[2..],
            [
                (CHUNK, 1),
                (CHUNK, 2),
                (CHUNK, 1),
                (CHUNK, 2),
                (RESPONSE, 1),
                (RESPONSE, 2)
            ]
        );
    }

    #[test]
    fn a_pipe_that_closes_between_frames_ends_cleanly_and_inside_one_does_not() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, REQUEST, 1, b"{}", b"abc").unwrap();
        let mut whole = bytes.as_slice();
        assert!(read_frame(&mut whole).unwrap().is_some());
        assert!(read_frame(&mut whole).unwrap().is_none());
        let mut cut = &bytes[..bytes.len() - 1];
        assert!(read_frame(&mut cut).is_err());
        let mut absurd: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0, 0];
        assert!(read_frame(&mut absurd).is_err());
    }
}
