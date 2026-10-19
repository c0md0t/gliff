//! The network and decode worker: connect (ssh or TCP), run the protocol,
//! decode on the CPU with OpenH264, and hand finished frames to the window
//! thread. Input flows the other way as ready-made `ClientMsg`s.
//!
//! This is the CPU tier of the GTK client's `net.rs`, without the GPU
//! decoder. Clipboard sharing covers text and images, fetched as soon as
//! the server offers them.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use gliff_proto::clipboard::{resolve_mime, CHUNK, MAX_ITEM, TEXT_MIME};
use gliff_proto::{
    features, is_incompatible, version_mismatch, ChromaMode, ClientCaps, ClientMsg, ClipboardItem,
    ClipboardMsg, Codec, ServerMsg, VideoPipeline, PROTOCOL_VERSION,
};
use gliff_transport::clipboard::{
    outbound_channel, ChunkSink, Event as TransferEvent, Side, Transfers,
};
use gliff_transport::{spawn_ssh, Framed, SshTarget};

use crate::clip::{self, Content, PNG_MIME};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use winit::event_loop::EventLoopProxy;

/// Stream geometry one picture was configured under: stream size, view
/// size, and the effective scale x1000.
type Geometry = ((u32, u32), (u32, u32), u32);

const MAX_STREAM: (u32, u32) = (3840, 2160);
// Older protocol-v1 GPU servers ignore the advertised limits on their first
// stream. Accept their bounded encoder maximum so they can reach the resize.
const MAX_DECODE: (u32, u32) = (4096, 4096);

/// The CPU decoder holds the newest picture of a High-profile stream until
/// the next access unit; when the stream goes quiet this long, drain it.
const IDLE_DRAIN: Duration = Duration::from_millis(150);

/// A decoded frame with the geometry it was configured under.
pub struct Picture {
    pub frame: gliff_sw::BgraFrame,
    /// Stream size in physical pixels.
    pub stream: (u32, u32),
    /// The full-quality size the frame should be drawn at.
    pub view: (u32, u32),
    /// Effective stream scale x1000 for pointer mapping.
    pub scale_milli: u32,
}

/// The newest undisplayed picture. The worker overwrites it, so a slow
/// window only ever skips frames, never queues them.
pub type Slot = Arc<Mutex<Option<Picture>>>;

/// What the worker tells the window thread.
#[derive(Debug)]
pub enum Event {
    /// A new picture waits in the slot.
    Frame,
    Connected {
        /// "server tier→client tier", e.g. "GPU→CPU".
        video: String,
        /// The frames carry the compositor-drawn cursor.
        headless: bool,
    },
    Stats {
        fps: f32,
        mbit: f32,
        decode_ms: f32,
    },
    Cursor {
        width: u32,
        height: u32,
        hot_x: i32,
        hot_y: i32,
        /// B, G, R, A bytes per pixel.
        argb: Vec<u8>,
    },
    /// A key the Windows keyboard hook captured, as an evdev code. Not from
    /// the worker, but it shares the event loop's one user-event type.
    #[cfg_attr(not(windows), allow(dead_code))]
    Key {
        keycode: u32,
        pressed: bool,
    },
    Error(String),
    Closed,
}

/// How to reach the server.
#[derive(Clone, Debug)]
pub enum Endpoint {
    Tcp(String),
    Ssh(SshTarget),
}

/// Start the worker thread. Dropping the returned sender ends the session
/// and, with it, the ssh child.
pub fn spawn(
    endpoint: Endpoint,
    proxy: EventLoopProxy<Event>,
    slot: Slot,
    clipboard: bool,
) -> UnboundedSender<ClientMsg> {
    let (tx, rx) = unbounded_channel();
    std::thread::Builder::new()
        .name("gliff-net".into())
        .spawn(move || run(endpoint, proxy, slot, rx, clipboard))
        .expect("spawn network thread");
    tx
}

/// The session's end of the clipboard thread.
struct ClipIo {
    /// Content to put on the local clipboard.
    set: std_mpsc::Sender<Content>,
    /// Local clipboard changes.
    local: UnboundedReceiver<Content>,
}

impl ClipIo {
    fn start() -> Option<Self> {
        let (local_tx, local) = unbounded_channel();
        let set = clip::spawn(local_tx)?;
        Some(Self { set, local })
    }
}

/// The next local clipboard change, or never without a clipboard.
async fn next_local(clip: &mut Option<ClipIo>) -> Content {
    match clip {
        Some(io) => match io.local.recv().await {
            Some(content) => content,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Collects a fetched item in memory.
struct Collect(Rc<RefCell<Vec<u8>>>);

impl ChunkSink for Collect {
    async fn write(&mut self, chunk: Bytes) -> std::io::Result<()> {
        self.0.borrow_mut().extend_from_slice(&chunk);
        Ok(())
    }

    async fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Fetch the server's offer `serial` (text, else a PNG image) and put it
/// on the local clipboard, unless a newer offer came in meanwhile.
async fn take_offer(
    transfers: Transfers,
    serial: u32,
    mime_types: Vec<String>,
    set: std_mpsc::Sender<Content>,
    latest: Rc<Cell<u32>>,
) {
    let mime = match resolve_mime(TEXT_MIME, &mime_types) {
        Some(m) => m.to_string(),
        None if mime_types.iter().any(|m| m == PNG_MIME) => PNG_MIME.to_string(),
        None => return,
    };
    let data = Rc::new(RefCell::new(Vec::new()));
    let fetched = transfers
        .fetch(
            ClipboardItem::Mime(mime.clone()),
            serial,
            Collect(data.clone()),
            Some(MAX_ITEM),
        )
        .await;
    if let Err(e) = fetched {
        tracing::warn!(error = %e, mime, "cannot fetch the remote clipboard");
        return;
    }
    if latest.get() != serial {
        return;
    }
    let data = data.take();
    let content = if mime == PNG_MIME {
        Content::Png(data.into())
    } else {
        Content::Text(String::from_utf8_lossy(&data).into_owned())
    };
    let _ = set.send(content);
}

fn run(
    endpoint: Endpoint,
    proxy: EventLoopProxy<Event>,
    slot: Slot,
    mut input: UnboundedReceiver<ClientMsg>,
    clipboard: bool,
) {
    let mut clip = if clipboard { ClipIo::start() } else { None };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = proxy.send_event(Event::Error(e.to_string()));
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    let events = proxy.clone();
    let result: anyhow::Result<()> = local.block_on(&rt, async move {
        match endpoint {
            Endpoint::Tcp(addr) => {
                let stream = tokio::net::TcpStream::connect(&addr)
                    .await
                    .with_context(|| format!("connect to {addr}"))?;
                stream.set_nodelay(true)?;
                let (rd, wr) = tokio::io::split(stream);
                session(rd, wr, events, slot, &mut input, &mut clip).await
            }
            Endpoint::Ssh(target) => {
                let ssh = spawn_ssh(&target).context("start ssh")?;
                // Keep the child alive for the session; dropping it kills ssh.
                let _child = ssh.child;
                session(ssh.stdout, ssh.stdin, events, slot, &mut input, &mut clip).await
            }
        }
    });
    let _ = proxy.send_event(match result {
        Ok(()) => Event::Closed,
        Err(e) => Event::Error(format!("{e:#}")),
    });
}

fn tier_label(pipeline: Option<VideoPipeline>) -> String {
    let server = pipeline.map_or("?", |p| match p {
        VideoPipeline::Gpu => "GPU",
        VideoPipeline::Cpu => "CPU",
    });
    format!("{server}→CPU")
}

fn new_decoder(chroma: ChromaMode, width: u32, height: u32) -> anyhow::Result<gliff_sw::Decoder> {
    validate_stream_size(width, height)?;
    Ok(gliff_sw::Decoder::new(chroma != ChromaMode::Single420)?)
}

async fn session<R, W>(
    rd: R,
    wr: W,
    events: EventLoopProxy<Event>,
    slot: Slot,
    input: &mut UnboundedReceiver<ClientMsg>,
    clip: &mut Option<ClipIo>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let mut reader = Framed::new(rd);
    let mut writer = Framed::new(wr);

    // One 4:2:0 stream: the CPU tier decodes one stream, not two. No keymap:
    // the server uses its own, which suits a client with no XKB text.
    writer
        .write_msg(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            keymap: String::new(),
            caps: ClientCaps {
                codecs: vec![Codec::H264],
                max_width: MAX_STREAM.0,
                max_height: MAX_STREAM.1,
                chroma: vec![ChromaMode::Single420],
                features: features(),
            },
        })
        .await?;

    // The server may Ping between HelloAck and StreamConfig; answer at once
    // so its round-trip estimate is seeded before the first frame.
    let mut got_ack = false;
    let mut headless = false;
    let (mut width, mut height, mut chroma, mut scale_milli, pipeline, mut view) = loop {
        match reader.read_msg::<ServerMsg>().await? {
            ServerMsg::HelloAck {
                version, session, ..
            } => {
                if version != PROTOCOL_VERSION {
                    anyhow::bail!("{}", version_mismatch(PROTOCOL_VERSION, version));
                }
                headless = session.headless;
                got_ack = true;
            }
            ServerMsg::Ping { t } => writer.write_msg(&ClientMsg::Pong { t }).await?,
            ServerMsg::StreamConfig {
                width,
                height,
                chroma,
                scale_milli,
                pipeline,
                view_width,
                view_height,
                ..
            } if got_ack => {
                break (
                    width,
                    height,
                    chroma,
                    scale_milli,
                    pipeline,
                    (view_width, view_height),
                )
            }
            ServerMsg::Error { code, message, .. } if is_incompatible(code) => {
                anyhow::bail!("incompatible server: {message}")
            }
            ServerMsg::Error { code, message, .. } => {
                anyhow::bail!("server error {code}: {message}")
            }
            other => anyhow::bail!("unexpected message before StreamConfig: {other:?}"),
        }
    };
    let mut decoder = new_decoder(chroma, width, height)?;
    let _ = events.send_event(Event::Connected {
        video: tier_label(pipeline),
        headless,
    });
    tracing::info!(width, height, scale_milli, "connected");

    // Writes run on their own task so reads (draining video) never block on
    // a write and the two peers cannot deadlock.
    // A message's payload (a clipboard chunk) follows its header.
    let (out_tx, mut out_rx) = unbounded_channel::<(ClientMsg, Bytes)>();
    let mut writes = tokio::task::spawn_local(async move {
        while let Some((m, payload)) = out_rx.recv().await {
            if payload.is_empty() {
                writer.write_msg(&m).await?;
            } else {
                writer.write_msg_with_payloads(&m, &[&payload]).await?;
            }
        }
        Ok::<(), gliff_transport::Error>(())
    });
    let send = |m: ClientMsg| {
        let _ = out_tx.send((m, Bytes::new()));
    };

    // Clipboard transfers write through the same channel, in chunks.
    let (clip_out_tx, mut clip_out_rx) = outbound_channel();
    let transfers = Transfers::new(clip_out_tx, Side::Client);
    {
        let out_tx = out_tx.clone();
        tokio::task::spawn_local(async move {
            while let Some((m, payload)) = clip_out_rx.recv().await {
                if out_tx.send((ClientMsg::from(m), payload)).is_err() {
                    break;
                }
            }
        });
    }
    // Our latest offer, which the server's requests must name.
    let mut offered: Option<(u32, Content)> = None;
    let mut offer_serial = 0u32;
    // The server's latest offer; an older fetch finishing late is dropped.
    let remote_serial = Rc::new(Cell::new(0u32));

    let mut frames_since = 0u32;
    let mut bytes_since = 0u64;
    let mut decode_ms_acc = 0f32;
    let mut last_report = std::time::Instant::now();
    let mut drain_at = tokio::time::Instant::now() + IDLE_DRAIN;
    // Geometry of the access units inside the decoder, oldest first: the
    // decoder may return the PREVIOUS unit's picture, which must carry the
    // configuration it was encoded under.
    let mut in_decoder: VecDeque<Geometry> = VecDeque::new();
    let deliver = |frame: gliff_sw::BgraFrame, (stream, view, scale_milli): Geometry| {
        let picture = Picture {
            frame,
            stream,
            view,
            scale_milli,
        };
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(picture);
        let _ = events.send_event(Event::Frame);
    };

    loop {
        let read = tokio::select! {
            read = reader.read_msg::<ServerMsg>() => read,
            msg = input.recv() => match msg {
                Some(m) => {
                    send(m);
                    continue;
                }
                // The window closed.
                None => return Ok(()),
            },
            content = next_local(clip) => {
                tracing::debug!(mime = content.mime(), "offering our clipboard");
                offer_serial = offer_serial.wrapping_add(1);
                let offer = ClipboardMsg::Offer {
                    serial: offer_serial,
                    mime_types: vec![content.mime().to_string()],
                    files: Vec::new(),
                };
                offered = Some((offer_serial, content));
                let transfers = transfers.clone();
                tokio::task::spawn_local(async move {
                    let _ = transfers.send(offer).await;
                });
                continue;
            }
            written = &mut writes => {
                written.context("writer task")?.context("write to server")?;
                return Ok(());
            }
            _ = tokio::time::sleep_until(drain_at), if decoder.has_pending() => {
                drain_at = tokio::time::Instant::now() + IDLE_DRAIN;
                match decoder.flush() {
                    Ok(Some(frame)) => {
                        let geometry = in_decoder
                            .pop_front()
                            .unwrap_or(((width, height), view, scale_milli));
                        deliver(frame, geometry);
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "idle drain failed"),
                }
                continue;
            }
        };
        let msg = match read {
            Ok(m) => m,
            Err(gliff_transport::Error::Closed) => break,
            Err(e) => return Err(e.into()),
        };
        match msg {
            ServerMsg::VideoFrame {
                frame_id,
                data_len,
                aux_len,
                ..
            } => {
                let main = reader.read_payload(data_len).await?;
                let aux = if aux_len > 0 {
                    reader.read_payload(aux_len).await?
                } else {
                    Bytes::new()
                };
                bytes_since += (data_len + aux_len) as u64;
                drain_at = tokio::time::Instant::now() + IDLE_DRAIN;
                in_decoder.push_back(((width, height), view, scale_milli));
                let t0 = std::time::Instant::now();
                let decoded = decoder.decode(&main, &aux);
                let dec_ms = t0.elapsed().as_secs_f32() * 1000.0;
                // Ack every frame at once so the server keeps pacing.
                send(ClientMsg::FrameAck {
                    frame_id,
                    decoded_at_ms: now_ms(),
                });
                match decoded {
                    Ok(Some(frame)) => {
                        let geometry =
                            in_decoder
                                .pop_front()
                                .unwrap_or(((width, height), view, scale_milli));
                        deliver(frame, geometry);
                        frames_since += 1;
                        decode_ms_acc += dec_ms;
                    }
                    // The decoder holds this unit; drained later or by the
                    // next decode.
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "decode error; requesting keyframe");
                        in_decoder.clear();
                        send(ClientMsg::RequestKeyframe);
                    }
                }
            }
            ServerMsg::StreamConfig {
                width: w,
                height: h,
                chroma: c,
                scale_milli: s,
                pipeline,
                view_width,
                view_height,
                ..
            } => {
                validate_stream_size(w, h)?;
                // A pace-only change must not reset the decoder; a change
                // of the coded stream does.
                if (w, h, c) != (width, height, chroma) {
                    decoder = new_decoder(c, w, h)?;
                    in_decoder.clear();
                }
                (width, height, chroma, scale_milli) = (w, h, c, s);
                view = (view_width, view_height);
                let _ = events.send_event(Event::Connected {
                    video: tier_label(pipeline),
                    headless,
                });
            }
            ServerMsg::CursorShape {
                width,
                height,
                hot_x,
                hot_y,
                argb_len,
                ..
            } => {
                let argb = reader.read_payload(argb_len).await?.to_vec();
                let _ = events.send_event(Event::Cursor {
                    width,
                    height,
                    hot_x,
                    hot_y,
                    argb,
                });
            }
            ServerMsg::Ping { t } => {
                send(ClientMsg::Pong { t });
            }
            ServerMsg::CursorPos { .. } | ServerMsg::Pong { .. } | ServerMsg::HelloAck { .. } => {}
            ServerMsg::Error { code, message, .. } if is_incompatible(code) => {
                anyhow::bail!("incompatible server: {message}")
            }
            ServerMsg::Error { code, message, .. } => {
                anyhow::bail!("server error {code}: {message}")
            }
            other => {
                let Ok(msg) = other.into_clipboard() else {
                    continue;
                };
                // A chunk's payload is read even when clipboard sharing is
                // off, to keep the framing in step.
                let payload = match &msg {
                    ClipboardMsg::Data { data_len, .. } => {
                        anyhow::ensure!(
                            *data_len as usize <= CHUNK,
                            "clipboard chunk of {data_len} bytes exceeds the limit"
                        );
                        reader.read_payload(*data_len).await?
                    }
                    _ => Bytes::new(),
                };
                match transfers.on_msg(msg, payload) {
                    Some(TransferEvent::Offer {
                        serial, mime_types, ..
                    }) => {
                        tracing::debug!(serial, ?mime_types, "server offers its clipboard");
                        if let Some(io) = clip {
                            remote_serial.set(serial);
                            tokio::task::spawn_local(take_offer(
                                transfers.clone(),
                                serial,
                                mime_types,
                                io.set.clone(),
                                remote_serial.clone(),
                            ));
                        }
                    }
                    Some(TransferEvent::Request { id, serial, item }) => match (&offered, item) {
                        (Some((s, content)), ClipboardItem::Mime(mime))
                            if *s == serial && content.serves(&mime) =>
                        {
                            let (tx, rx) = tokio::sync::mpsc::channel(4);
                            let data = content.bytes();
                            tokio::task::spawn_local(async move {
                                for start in (0..data.len()).step_by(CHUNK) {
                                    let end = (start + CHUNK).min(data.len());
                                    if tx.send(Ok(data.slice(start..end))).await.is_err() {
                                        break;
                                    }
                                }
                            });
                            tracing::debug!(id, serial, mime, "serving our clipboard");
                            transfers.serve(id, rx);
                        }
                        _ => {
                            tracing::debug!(id, serial, "refusing a clipboard request");
                            transfers.refuse(id)
                        }
                    },
                    None => {}
                }
            }
        }
        if last_report.elapsed().as_secs_f32() >= 1.0 {
            let secs = last_report.elapsed().as_secs_f32();
            let _ = events.send_event(Event::Stats {
                fps: frames_since as f32 / secs,
                mbit: bytes_since as f32 * 8.0 / 1_000_000.0 / secs,
                decode_ms: if frames_since > 0 {
                    decode_ms_acc / frames_since as f32
                } else {
                    0.0
                },
            });
            frames_since = 0;
            bytes_since = 0;
            decode_ms_acc = 0.0;
            last_report = std::time::Instant::now();
        }
    }
    Ok(())
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn validate_stream_size(width: u32, height: u32) -> anyhow::Result<()> {
    anyhow::ensure!(width > 0 && height > 0 && width <= MAX_DECODE.0 && height <= MAX_DECODE.1
        && width.is_multiple_of(2) && height.is_multiple_of(2),
        "invalid stream dimensions {width}x{height}; expected even dimensions within 4096x4096 decoder limit");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_stream_geometry() {
        for size in [(0, 1080), (1920, 0), (4098, 2160), (1919, 1080)] {
            assert!(validate_stream_size(size.0, size.1).is_err());
        }
        assert!(validate_stream_size(3840, 2160).is_ok());
        assert!(validate_stream_size(4096, 4096).is_ok());
    }
}
