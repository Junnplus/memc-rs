use crate::protocol::binary_codec::{
    BinaryRequest, BinaryResponse, MemcacheBinaryCodec, ResponseMessage,
};
use bytes::{BufMut, BytesMut};
use std::cmp;
use std::io;
use std::io::{Error, ErrorKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Decoder;

pub struct MemcacheBinaryConnection {
    stream: TcpStream,
    codec: MemcacheBinaryCodec,
}

impl MemcacheBinaryConnection {
    pub fn new(socket: TcpStream, item_size_limit: u32) -> Self {
        MemcacheBinaryConnection {
            stream: socket,
            codec: MemcacheBinaryCodec::new(item_size_limit),
        }
    }

    pub async fn read_frame(&mut self) -> Result<Option<BinaryRequest>, io::Error> {
        let mut buffer = BytesMut::with_capacity(24);
        let extras_length: u32 = 8;
        loop {
            // Attempt to parse a frame from the buffered data. If enough data
            // has been buffered, the frame is returned.
            if let Some(frame) = self.codec.decode(&mut buffer)? {
                match frame {
                    BinaryRequest::ItemTooLarge(request) => {
                        debug!(
                            "Body len {:?} buffer len {:?}",
                            request.header.body_length,
                            buffer.len()
                        );
                        let skip = (request.header.body_length) - (buffer.len() as u32);
                        if skip >= buffer.len() as u32 {
                            buffer.clear();
                        } else {
                            buffer = buffer.split_off(skip as usize);
                        }
                        self.skip_bytes(skip).await?;
                        return Ok(Some(BinaryRequest::ItemTooLarge(request)));
                    }
                    _ => {
                        buffer.clear();
                        return Ok(Some(frame));
                    }
                }
            }

            // There is not enough buffered data to read a frame. Attempt to
            // read more data from the socket.
            //
            // On success, the number of bytes is returned. `0` indicates "end
            // of stream".
            if 0 == self.stream.read_buf(&mut buffer).await? {
                // The remote closed the connection. For this to be a clean
                // shutdown, there should be no data in the read buffer. If
                // there is, this means that the peer closed the socket while
                // sending a frame.
                if buffer.is_empty() {
                    return Ok(None);
                } else {
                    return Err(Error::new(
                        ErrorKind::ConnectionReset,
                        "Connection reset by peer",
                    ));
                }
            }
        }
    }

    pub async fn skip_bytes(&mut self, bytes: u32) -> io::Result<()> {
        let buffer_size = 64 * 1024;
        let mut buffer = BytesMut::with_capacity(cmp::min(bytes as usize, buffer_size));
        let mut bytes_read: usize;
        let mut bytes_counter: usize = 0;
        debug!("Skip bytes {:?}", bytes);
        if bytes == 0 {
            return Ok(());
        }

        loop {
            bytes_read = self.stream.read_buf(&mut buffer).await?;

            // The remote closed the connection. For this to be a clean
            // shutdown, there should be no data in the read buffer. If
            // there is, this means that the peer closed the socket while
            // sending a frame.
            if bytes_read == 0 {
                debug!("Bytes read {:?}", bytes_read);
                if buffer.is_empty() {
                    return Ok(());
                } else {
                    return Err(Error::new(
                        ErrorKind::ConnectionReset,
                        "Connection reset by peer",
                    ));
                }
            }

            bytes_counter += bytes_read;
            let difference = bytes as usize - bytes_counter;
            debug!(
                "Bytes read: {:?} {:?} {:?}",
                bytes_read, bytes_counter, difference
            );

            if bytes_counter == bytes as usize {
                return Ok(());
            }

            if difference < buffer_size {
                buffer = BytesMut::with_capacity(difference);
            } else {
                buffer.clear();
            }

            if bytes_counter > bytes as usize {
                panic!("Read too much bytes socket corrupted");
            }
        }
    }

    pub async fn write(&mut self, msg: &BinaryResponse) -> io::Result<()> {
        let message = self.codec.encode_message(msg);
        self.write_data_to_stream(message).await?;
        Ok(())
    }

    async fn write_data_to_stream(&mut self, msg: ResponseMessage) -> io::Result<()> {
        self.stream.write_all(&msg.data[..]).await?;
        match msg.value {
            Some(value) => {
                self.stream.write_all(&value).await?;
            }
            None => {}
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.stream.shutdown().await?;
        Ok(())
    }
}
