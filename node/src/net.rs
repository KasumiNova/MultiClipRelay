use anyhow::Context;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

pub async fn connect(relay: &str) -> anyhow::Result<TcpStream> {
    let s = TcpStream::connect(relay).await.context("connect")?;
    Ok(s)
}

pub async fn send_frame(mut stream: TcpStream, buf: Vec<u8>) -> anyhow::Result<()> {
    stream
        .write_u32(buf.len() as u32)
        .await
        .context("write len")?;
    stream.write_all(&buf).await.context("write payload")?;
    Ok(())
}

pub async fn send_join(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    device_id: &str,
    room: &str,
) -> anyhow::Result<()> {
    let join = utils::Message::new_join(device_id, room).to_bytes();
    writer
        .write_u32(join.len() as u32)
        .await
        .context("write join len")?;
    writer.write_all(&join).await.context("write join")?;
    Ok(())
}
