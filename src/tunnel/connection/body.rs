use std::{
    io::{Error, ErrorKind, Result, Write},
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, BufMut, Bytes};
use futures::{channel::mpsc, Stream};
use http_body::{Body, Frame};
use tokio::io::ReadBuf;

pub fn reader<T>(body: T) -> BodyReader<T>
where
    T: Body + Unpin,
    T::Data: Buf + Unpin + Default,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    BodyReader {
        data: T::Data::default(),
        body,
    }
}

pub struct BodyReader<T: Body> {
    data: T::Data,
    body: T,
}

impl<T> futures::io::AsyncRead for BodyReader<T>
where
    T: Body + Unpin,
    T::Data: Buf + Unpin + Default,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize>> {
        if self.data.remaining() == 0 {
            match Pin::new(&mut self.body).poll_frame(ctx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.data = data;
                    } else {
                        return Poll::Pending;
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Err(Error::new(ErrorKind::Other, error)));
                }
                Poll::Ready(None) => {
                    return Poll::Ready(Ok(0));
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
        let amount = buf.as_mut().write(self.data.chunk())?;
        self.data.advance(amount);
        Poll::Ready(Ok(amount))
    }
}

impl<T> tokio::io::AsyncRead for BodyReader<T>
where
    T: Body + Unpin,
    T::Data: Buf + Unpin + Default,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        loop {
            match Pin::new(&mut self.body).poll_frame(ctx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        buf.put(data);
                        return Poll::Ready(Ok(()));
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Err(Error::new(ErrorKind::Other, error)))
                }
                Poll::Ready(None) => {
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

pub fn writer() -> (BodyWriter, WriterBody) {
    let (sender, receiver) = mpsc::channel(8);
    (BodyWriter { sender }, WriterBody { receiver })
}

pub struct BodyWriter {
    sender: mpsc::Sender<Bytes>,
}

impl futures::io::AsyncWrite for BodyWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        match self.sender.poll_ready(ctx) {
            Poll::Ready(Ok(_)) => {
                if let Err(error) = self.sender.try_send(Bytes::copy_from_slice(buf)) {
                    Poll::Ready(Err(Error::new(ErrorKind::Other, error.into_send_error())))
                } else {
                    Poll::Ready(Ok(buf.len()))
                }
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(Error::new(ErrorKind::Other, error))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<()>> {
        self.sender.close_channel();
        Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncWrite for BodyWriter {
    fn poll_write(self: Pin<&mut Self>, ctx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        futures::io::AsyncWrite::poll_write(self, ctx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<()>> {
        futures::io::AsyncWrite::poll_flush(self, ctx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<()>> {
        futures::io::AsyncWrite::poll_close(self, ctx)
    }
}

pub struct WriterBody {
    receiver: mpsc::Receiver<Bytes>,
}

impl Body for WriterBody {
    type Data = Bytes;

    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>>>> {
        match Pin::new(&mut self.receiver).poll_next(ctx) {
            Poll::Ready(Some(data)) => Poll::Ready(Some(Ok(Frame::data(data)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
