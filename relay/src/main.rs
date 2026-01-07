use anyhow::{bail, Context};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

use utils::Kind;
use utils::Message;

type Tx = mpsc::Sender<Vec<u8>>;
type ConnId = u64;
type SharedRooms = Arc<Mutex<HashMap<String, Vec<(ConnId, Tx)>>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut addr = std::env::var("RELAY_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    // Minimal CLI parsing (avoid extra deps):
    //   relay --bind 127.0.0.1:8080
    // Env RELAY_ADDR still works and is the default when no args are given.
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bind" | "--addr" => {
                addr = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing value for {a}"))?;
            }
            "-h" | "--help" => {
                println!("Usage: relay [--bind <ip:port>]\n\nEnv: RELAY_ADDR=<ip:port>");
                return Ok(());
            }
            other => bail!("unknown arg: {other}"),
        }
    }

    println!("Relay listening on {}", addr);
    let listener = TcpListener::bind(&addr).await.context("bind")?;
    let rooms: SharedRooms = Arc::new(Mutex::new(HashMap::new()));

    loop {
        let (socket, peer) = listener.accept().await.context("accept")?;
        let rooms = rooms.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(socket, rooms).await {
                eprintln!("connection error {}: {:?}", peer, e);
            }
        });
    }
}

async fn handle_conn(socket: TcpStream, rooms: SharedRooms) -> anyhow::Result<()> {
    let conn_id: ConnId = rand_conn_id();
    let (mut reader, mut writer_half) = socket.into_split();
    // create outbound channel
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(32);
    // writer task
    let writer = tokio::spawn(async move {
        while let Some(buf) = rx.recv().await {
            // write length (u32 BE) then payload
            if writer_half.write_u32(buf.len() as u32).await.is_err() {
                break;
            }
            if writer_half.write_all(&buf).await.is_err() {
                break;
            }
        }
    });

    // read loop
    let mut registered_room: Option<String> = None;
    loop {
        // read len
        let len = match reader.read_u32().await {
            Ok(l) => l as usize,
            Err(_) => break,
        };
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await.context("read payload")?;
        let msg = Message::from_bytes(&buf);

        // register sender into room when first message arrives
        if registered_room.is_none() {
            let r = msg.room.clone();
            let mut map = rooms.lock().await;
            map.entry(r.clone())
                .or_default()
                .push((conn_id, tx.clone()));
            registered_room = Some(r);
        }

        // broadcast to room
        if matches!(msg.kind, Kind::Join) {
            continue;
        }
        let room = msg.room.clone();
        let out = msg.to_bytes();
        let mut map = rooms.lock().await;
        if let Some(list) = map.get_mut(&room) {
            // Drop only closed channels (a full channel should not kick the client).
            list.retain(|(_, s)| !s.is_closed());
            for (id, s) in list.iter() {
                if *id == conn_id {
                    continue;
                }
                let _ = s.try_send(out.clone());
            }
        }
    }

    // cleanup writer
    drop(tx);
    let _ = writer.await;
    // remove from rooms
    if let Some(room) = registered_room {
        let mut map = rooms.lock().await;
        if let Some(list) = map.get_mut(&room) {
            // remove closed channels
            list.retain(|(_, s)| !s.is_closed());
        }
    }

    Ok(())
}

fn rand_conn_id() -> ConnId {
    // Good enough for a prototype: a random-ish u64 from current time.
    // (We avoid adding an extra dependency; collisions are extremely unlikely here.)
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
