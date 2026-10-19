//! The network and decode worker: connect (ssh or TCP), run the protocol,
//! decode on the CPU with OpenH264, and hand finished frames to the window
//! thread. Input flows the other way as ready-made `ClientMsg`s.
//!
//! This is the CPU tier of the GTK client's `net.rs`, without the GPU
//! decoder or clipboard sharing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use gliff_proto::clipboard::CHUNK;
use gliff_proto::{
    features, is_incompatible, version_mismatch, ChromaMode, ClientCaps, ClientMsg, ClipboardMsg,
    Codec, ServerMsg, VideoPipeline, PROTOCOL_VERSION,
};
use gliff_transport::{spawn_ssh, Framed, SshTarget};
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
) -> UnboundedSender<ClientMsg> {
    let (tx, rx) = unbounded_channel();
    std::thread::Builder::new()
        .name("gliff-net".into())
        .spawn(move || run(endpoint, proxy, slot, rx))
        .expect("spawn network thread");
    tx
}

fn run(
    endpoint: Endpoint,
    proxy: EventLoopProxy<Event>,
    slot: Slot,
    input: UnboundedReceiver<ClientMsg>,
) {
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
                session(rd, wr, events, slot, input).await
            }
            Endpoint::Ssh(target) => {
                let ssh = spawn_ssh(&target).context("start ssh")?;
                // Keep the child alive for the session; dropping it kills ssh.
                let _child = ssh.child;
                session(ssh.stdout, ssh.stdin, events, slot, input).await
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
    mut input: UnboundedReceiver<ClientMsg>,
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
    let (out_tx, mut out_rx) = unbounded_channel::<ClientMsg>();
    let mut writes = tokio::task::spawn_local(async move {
        while let Some(m) = out_rx.recv().await {
            writer.write_msg(&m).await?;
        }
        Ok::<(), gliff_transport::Error>(())
    });

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
                    let _ = out_tx.send(m);
                    continue;
                }
                // The window closed.
                None => return Ok(()),
            },
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
                let _ = out_tx.send(ClientMsg::FrameAck {
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
                        let _ = out_tx.send(ClientMsg::RequestKeyframe);
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
                let _ = out_tx.send(ClientMsg::Pong { t });
            }
            ServerMsg::CursorPos { .. } | ServerMsg::Pong { .. } | ServerMsg::HelloAck { .. } => {}
            ServerMsg::Error { code, message, .. } if is_incompatible(code) => {
                anyhow::bail!("incompatible server: {message}")
            }
            ServerMsg::Error { code, message, .. } => {
                anyhow::bail!("server error {code}: {message}")
            }
            other => {
                // No clipboard sharing yet: offers are ignored, so the
                // server never sends data unasked, but any payload must
                // still be read to keep the framing in step.
                if let Ok(clip) = other.into_clipboard() {
                    match clip {
                        ClipboardMsg::Data { data_len, .. } => {
                            anyhow::ensure!(
                                data_len as usize <= CHUNK,
                                "clipboard chunk of {data_len} bytes exceeds the limit"
                            );
                            reader.read_payload(data_len).await?;
                        }
                        ClipboardMsg::Request { id, .. } => {
                            let _ = out_tx.send(ClientMsg::ClipboardAbort { id });
                        }
                        _ => {}
                    }
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
